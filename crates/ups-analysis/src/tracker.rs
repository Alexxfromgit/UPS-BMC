//! The outage engine: snapshots in, [`Outage`] records out.

use chrono::{DateTime, Duration, Utc};
use ups_model::{Outage, Snapshot};

/// Tuning of the outage tracker.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackerConfig {
    /// A pause between samples longer than this is a monitoring gap.
    pub gap: Duration,
    /// Mains returning for less than this before failing again counts as
    /// one outage (flapping power).
    pub merge_window: Duration,
    /// Charge at which the battery counts as recharged.
    pub recharged_pct: f32,
    /// Longest interval used for energy integration.
    pub max_step: Duration,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            gap: Duration::seconds(30),
            merge_window: Duration::seconds(5),
            recharged_pct: ups_model::RECHARGED_PCT,
            max_step: Duration::seconds(10),
        }
    }
}

impl TrackerConfig {
    /// Defaults adapted to the polling interval: a slow poll must not make
    /// every interval a "gap" or cut energy steps short (review M2-7).
    #[must_use]
    pub fn for_poll_interval(poll: Duration) -> Self {
        let d = Self::default();
        Self {
            gap: d.gap.max(poll * 5),
            max_step: d.max_step.max(poll * 2),
            ..d
        }
    }
}

/// Running sums for the open outage's averages.
#[derive(Debug, Clone, Default, PartialEq)]
struct Sums {
    load: f64,
    load_n: u32,
    power: f64,
    power_n: u32,
    energy_wh: f64,
}

impl Sums {
    /// Continue from a stored outage, weighting its averages by the number of
    /// samples behind them (older rows without a count weigh as one sample).
    fn from_outage(o: &Outage) -> Self {
        let n = o.samples.max(1);
        let weight = |avg: Option<f32>| {
            (
                avg.map_or(0.0, |v| f64::from(v) * f64::from(n)),
                if avg.is_some() { n } else { 0 },
            )
        };
        let (load, load_n) = weight(o.avg_load_pct);
        let (power, power_n) = weight(o.avg_power_w);
        Self {
            load,
            load_n,
            power,
            power_n,
            energy_wh: o.energy_wh.map_or(0.0, f64::from),
        }
    }

    #[allow(clippy::cast_possible_truncation)] // averages of f32 inputs
    fn write_to(&self, o: &mut Outage) {
        o.avg_load_pct = (self.load_n > 0).then(|| (self.load / f64::from(self.load_n)) as f32);
        o.avg_power_w = (self.power_n > 0).then(|| (self.power / f64::from(self.power_n)) as f32);
        o.energy_wh = (self.power_n > 0).then_some(self.energy_wh as f32);
    }
}

/// Tracks outages from a stream of snapshots (in time order).
#[derive(Debug, Clone)]
pub struct OutageTracker {
    cfg: TrackerConfig,
    open: Option<(Outage, Sums)>,
    /// The last ended outage, until its battery is full again.
    recharging: Option<Outage>,
    /// Time and on-battery state of the previous snapshot.
    last: Option<(DateTime<Utc>, bool)>,
    /// The user's estimate of the protected load, used when the UPS reports
    /// no measurable power (0 W / 0 %).
    assumed_power_w: Option<f32>,
    /// A guided runtime test is discharging the battery.
    testing: bool,
}

impl OutageTracker {
    #[must_use]
    pub fn new(cfg: TrackerConfig) -> Self {
        Self {
            cfg,
            open: None,
            recharging: None,
            last: None,
            assumed_power_w: None,
            testing: false,
        }
    }

    /// Restores state after a collector restart: the outage left open in
    /// storage, the most recent ended outage still waiting for its recharge,
    /// and the time and state of the last stored sample. The first new
    /// snapshot then closes or continues them, treating the downtime as a gap.
    pub fn resume(
        &mut self,
        open: Option<Outage>,
        recharging: Option<Outage>,
        last_sample: Option<(DateTime<Utc>, bool)>,
    ) {
        self.open = open.map(|o| {
            let sums = Sums::from_outage(&o);
            (o, sums)
        });
        self.recharging = recharging;
        self.last = last_sample;
    }

    /// Gives the storage ID to the outage that was just stored for the first time.
    pub fn assign_id(&mut self, id: i64) {
        if let Some((o, _)) = self.open.as_mut().filter(|(o, _)| o.id == 0) {
            o.id = id;
        } else if let Some(o) = self.recharging.as_mut().filter(|o| o.id == 0) {
            o.id = id;
        }
    }

    /// Sets (or clears) the assumed load used when the UPS measures 0.
    pub fn set_assumed_power(&mut self, watts: Option<f32>) {
        self.assumed_power_w = watts.filter(|w| *w > 0.0);
    }

    /// While set, the ongoing outage is tagged as a runtime test.
    pub fn set_testing(&mut self, testing: bool) {
        self.testing = testing;
    }

    /// The ongoing outage, if any.
    #[must_use]
    pub fn current(&self) -> Option<&Outage> {
        self.open.as_ref().map(|(o, _)| o)
    }

    /// Removes the runtime-test tag from the ongoing outage and from the one
    /// still recharging (a test was cancelled or interrupted). Returns the
    /// outages that changed, to be stored.
    pub fn clear_test_tag(&mut self) -> Vec<Outage> {
        let mut changed = Vec::new();
        if let Some((o, _)) = self.open.as_mut().filter(|(o, _)| o.runtime_test) {
            o.runtime_test = false;
            changed.push(o.clone());
        }
        if let Some(o) = self.recharging.as_mut().filter(|o| o.runtime_test) {
            o.runtime_test = false;
            changed.push(o.clone());
        }
        changed
    }

    /// Feeds one snapshot. Returns every outage that was created or changed;
    /// the caller stores them (insert when `id == 0`, then [`Self::assign_id`]).
    pub fn feed(&mut self, s: &Snapshot) -> Vec<Outage> {
        let Some(status) = s.status else {
            return Vec::new(); // unknown status: neither start nor end anything
        };
        let ts = s.ts;
        let on_battery = status.on_battery();
        let last = self.last;
        // Slightly out of order or duplicate: ignore. A large backward jump
        // (the clock was set back) is treated as a gap, so tracking goes on
        // instead of ignoring everything until the old time is reached (M2-9).
        if last.is_some_and(|(t, _)| ts <= t && t - ts <= self.cfg.gap) {
            return Vec::new();
        }
        let gap = last.is_some_and(|(t, _)| ts < t || ts - t > self.cfg.gap);
        let step = last
            .map(|(t, _)| ts - t)
            .filter(|_| !gap)
            .map_or(Duration::zero(), |d| d.min(self.cfg.max_step));
        self.last = Some((ts, on_battery));

        let mut changed = Vec::new();
        if on_battery {
            // The interval before the first sample on battery was on utility,
            // so it contributes no battery energy.
            let step = if let Some((o, _)) = self.open.as_mut() {
                // Battery → gap → battery: mains may have returned in between.
                o.gap_inside |= gap;
                step
            } else {
                if let Some(r) = self.recharging.as_mut().filter(|_| gap) {
                    r.recharge_unobserved = true;
                    changed.push(r.clone());
                }
                self.begin(s, last.filter(|_| gap).map(|(t, _)| t), gap);
                Duration::zero()
            };
            if let Some((o, sums)) = self.open.as_mut() {
                o.runtime_test |= self.testing;
                accumulate(o, sums, s, step, self.assumed_power_w);
                changed.push(o.clone());
            }
        } else {
            // A gap while recharging: when the battery became full is unknown.
            if gap && let Some(mut r) = self.recharging.take() {
                r.recharge_unobserved = true;
                changed.push(r);
            }
            if let Some((mut o, sums)) = self.open.take() {
                sums.write_to(&mut o);
                if gap {
                    // Not observed: the outage ended somewhere in the gap,
                    // so the recharge time can't be measured either.
                    o.end = last.map(|(t, _)| t);
                    o.end_latest = Some(ts);
                    o.recharge_unobserved = true;
                    changed.push(o);
                } else {
                    o.end = Some(ts);
                    changed.push(o.clone());
                    self.recharging = Some(o);
                }
            }
            self.check_recharged(s, &mut changed);
        }
        changed
    }

    /// Marks the recharging outage as recharged once the charge is full again.
    /// Not within the merge window after its end: mains may still fail again,
    /// and the outage must stay available for merging (review M2-1).
    fn check_recharged(&mut self, s: &Snapshot, changed: &mut Vec<Outage>) {
        let full = s
            .battery_charge_pct
            .is_some_and(|c| c >= self.cfg.recharged_pct);
        let Some(r) = self.recharging.as_mut() else {
            return;
        };
        let settled = r.end.is_none_or(|e| s.ts - e > self.cfg.merge_window);
        if !(full && settled) {
            return;
        }
        // Never below the threshold: nothing to recharge.
        r.recharged_at = if r.recharge_needed() {
            Some(s.ts)
        } else {
            r.end
        };
        // The same outage may already be in `changed` (closed on this very
        // sample); replace it with the newest version.
        changed.retain(|c| c.id != r.id || c.start != r.start);
        changed.push(r.clone());
        self.recharging = None;
    }

    /// Opens an outage, or reopens the one that ended moments ago (flapping).
    fn begin(&mut self, s: &Snapshot, gap_start: Option<DateTime<Utc>>, gap: bool) {
        let reopen = self.recharging.take().filter(|r| {
            !gap && r.end_observed() && r.end.is_some_and(|e| s.ts - e <= self.cfg.merge_window)
        });
        if let Some(mut o) = reopen {
            o.end = None;
            o.recharged_at = None;
            let sums = Sums::from_outage(&o);
            self.open = Some((o, sums));
            return;
        }
        // Any outage still recharging stays without `recharged_at` (interrupted).
        let mut o = Outage::started(s.ts);
        o.start_earliest = gap_start;
        o.charge_start_pct = s.battery_charge_pct;
        o.runtime_estimate_start_s = s.battery_runtime_s;
        self.open = Some((o, Sums::default()));
    }
}

#[allow(clippy::cast_precision_loss)] // step is at most a few seconds
fn accumulate(o: &mut Outage, sums: &mut Sums, s: &Snapshot, step: Duration, assumed: Option<f32>) {
    if let Some(v) = s.battery_voltage_v {
        o.battery_v_start.get_or_insert(v);
        o.battery_v_end = Some(v);
        o.battery_v_min = Some(o.battery_v_min.map_or(v, |m| m.min(v)));
    }
    if let Some(c) = s.battery_charge_pct {
        o.charge_start_pct.get_or_insert(c);
        o.charge_end_pct = Some(c);
        o.charge_min_pct = Some(o.charge_min_pct.map_or(c, |m| m.min(c)));
    }
    if o.runtime_estimate_start_s.is_none() {
        o.runtime_estimate_start_s = s.battery_runtime_s;
    }
    o.samples = o.samples.saturating_add(1);
    if let Some(l) = s.load_pct {
        sums.load += f64::from(l);
        sums.load_n += 1;
    }
    // Measured power, unless the UPS reads 0 (below its measuring range) and
    // the user gave an estimate.
    let measured = s.realpower_w.or_else(|| s.estimated_load_w());
    let power = match (measured, assumed) {
        (Some(w), _) if w > 0.0 => Some(w),
        (_, Some(a)) => {
            o.power_estimated = true;
            Some(a)
        }
        (m, None) => m,
    };
    if let Some(w) = power {
        sums.power += f64::from(w);
        sums.power_n += 1;
        sums.energy_wh += f64::from(w) * step.num_milliseconds() as f64 / 3_600_000.0;
    }
    sums.write_to(o);
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss)] // small test indices
mod tests {
    use chrono::TimeZone;
    use ups_model::UpsStatus;

    use super::*;

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap() + Duration::seconds(secs)
    }

    fn snap(secs: i64, on_battery: bool, charge: f32) -> Snapshot {
        let mut s = Snapshot::empty(t(secs));
        s.status = Some(if on_battery {
            UpsStatus::OB | UpsStatus::DISCHRG
        } else {
            UpsStatus::OL | UpsStatus::CHRG
        });
        s.battery_charge_pct = Some(charge);
        s.battery_runtime_s = Some(1800);
        s.load_pct = Some(20.0);
        s.realpower_w = Some(96.0);
        s
    }

    /// Feeds snapshots, storing outages like the collector does. Returns the
    /// final stored versions by ID.
    fn run(tracker: &mut OutageTracker, snaps: &[Snapshot]) -> Vec<Outage> {
        let mut stored: Vec<Outage> = Vec::new();
        for s in snaps {
            for mut o in tracker.feed(s) {
                if o.id == 0 {
                    o.id = i64::try_from(stored.len()).unwrap() + 1;
                    tracker.assign_id(o.id);
                }
                match stored.iter_mut().find(|x| x.id == o.id) {
                    Some(x) => *x = o,
                    None => stored.push(o),
                }
            }
        }
        stored
    }

    fn seconds(from: i64, to: i64, on_battery: bool, charge: impl Fn(i64) -> f32) -> Vec<Snapshot> {
        (from..to).map(|i| snap(i, on_battery, charge(i))).collect()
    }

    #[test]
    fn records_a_complete_outage_with_recharge() {
        let mut tr = OutageTracker::new(TrackerConfig::default());
        let mut snaps = seconds(0, 10, false, |_| 100.0);
        snaps.extend(seconds(10, 70, true, |i| 100.0 - (i - 10) as f32 * 0.5)); // 60 s on battery
        snaps.extend(seconds(70, 200, false, |i| {
            (70.0 + (i - 70) as f32 * 0.25).min(100.0)
        }));
        let outages = run(&mut tr, &snaps);
        assert_eq!(outages.len(), 1);
        let o = &outages[0];
        assert_eq!(o.start, t(10));
        assert_eq!(o.end, Some(t(70)));
        assert!(o.start_observed() && o.end_observed());
        assert_eq!(o.charge_start_pct, Some(100.0));
        assert_eq!(o.charge_end_pct, Some(70.5));
        assert_eq!(o.charge_min_pct, Some(70.5));
        assert_eq!(o.runtime_estimate_start_s, Some(1800));
        assert_eq!(o.avg_load_pct, Some(20.0));
        assert_eq!(o.avg_power_w, Some(96.0));
        // 59 one-second steps at 96 W.
        assert!((o.energy_wh.unwrap() - 96.0 * 59.0 / 3600.0).abs() < 1e-3);
        // 70 % → 99 % at 0.25 %/s takes 116 s.
        assert_eq!(o.recharged_at, Some(t(70 + 116)));
        assert_eq!(o.recharge_duration(), Some(Duration::seconds(116)));
        assert!(tr.current().is_none());
    }

    #[test]
    fn merges_flapping_power_into_one_outage() {
        let mut tr = OutageTracker::new(TrackerConfig::default());
        let mut snaps = seconds(0, 5, false, |_| 100.0);
        snaps.extend(seconds(5, 20, true, |_| 95.0));
        snaps.extend(seconds(20, 23, false, |_| 95.0)); // back for 3 s
        snaps.extend(seconds(23, 40, true, |_| 90.0));
        snaps.extend(seconds(40, 60, false, |_| 90.0));
        let outages = run(&mut tr, &snaps);
        assert_eq!(outages.len(), 1, "{outages:#?}");
        assert_eq!(outages[0].start, t(5));
        assert_eq!(outages[0].end, Some(t(40)));
    }

    #[test]
    fn separates_outages_beyond_the_merge_window() {
        let mut tr = OutageTracker::new(TrackerConfig::default());
        let mut snaps = seconds(0, 10, true, |_| 95.0);
        snaps.extend(seconds(10, 30, false, |_| 95.0));
        snaps.extend(seconds(30, 40, true, |_| 90.0));
        snaps.push(snap(40, false, 90.0));
        let outages = run(&mut tr, &snaps);
        assert_eq!(outages.len(), 2);
        // The first recharge was interrupted by the second outage.
        assert_eq!(outages[0].recharged_at, None);
        assert!(outages[0].end_observed());
    }

    #[test]
    fn marks_an_end_hidden_in_a_monitoring_gap() {
        // On battery, collector stops (PC shut down), back later on utility.
        let mut tr = OutageTracker::new(TrackerConfig::default());
        let mut snaps = seconds(0, 10, true, |_| 50.0);
        snaps.push(snap(3600, false, 60.0));
        let outages = run(&mut tr, &snaps);
        assert_eq!(outages.len(), 1);
        let o = &outages[0];
        assert_eq!(o.end, Some(t(9)));
        assert_eq!(o.end_latest, Some(t(3600)));
        assert!(!o.end_observed());
    }

    #[test]
    fn marks_a_start_hidden_in_a_monitoring_gap() {
        let mut tr = OutageTracker::new(TrackerConfig::default());
        let mut snaps = seconds(0, 3, false, |_| 100.0);
        snaps.extend(seconds(600, 605, true, |_| 80.0));
        let outages = run(&mut tr, &snaps);
        assert_eq!(outages[0].start, t(600));
        assert_eq!(outages[0].start_earliest, Some(t(2)));
        assert!(!outages[0].start_observed());
        assert!(outages[0].is_ongoing());
    }

    #[test]
    fn resumes_an_open_outage_after_restart() {
        let mut first = OutageTracker::new(TrackerConfig::default());
        let snaps = seconds(0, 10, true, |_| 90.0);
        let stored = run(&mut first, &snaps);
        let open = stored[0].clone();

        // Collector restarts 5 s later while still on battery: same outage.
        let mut second = OutageTracker::new(TrackerConfig::default());
        second.resume(Some(open.clone()), None, Some((t(9), true)));
        let changed = second.feed(&snap(14, true, 89.0));
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].id, open.id);
        assert_eq!(changed[0].charge_end_pct, Some(89.0));
        let changed = second.feed(&snap(15, false, 89.0));
        assert_eq!(changed[0].end, Some(t(15)));
    }

    #[test]
    fn merges_flapping_power_at_full_charge() {
        // Review M2-1: the 5E reads 100 % through short outages, so the
        // closing sample is already "full". The flap must still merge.
        let mut tr = OutageTracker::new(TrackerConfig::default());
        let mut snaps = seconds(0, 5, false, |_| 100.0);
        snaps.extend(seconds(5, 15, true, |_| 100.0));
        snaps.extend(seconds(15, 17, false, |_| 100.0)); // back for 2 s
        snaps.extend(seconds(17, 30, true, |_| 100.0));
        snaps.extend(seconds(30, 60, false, |_| 100.0));
        let outages = run(&mut tr, &snaps);
        assert_eq!(outages.len(), 1, "{outages:#?}");
        let o = &outages[0];
        assert_eq!(o.end, Some(t(30)));
        // Nothing to recharge: recharged at the end, and not counted as a recharge.
        assert!(!o.recharge_needed());
        assert_eq!(o.recharged_at, Some(t(30)));
    }

    #[test]
    fn weights_resumed_averages_by_sample_count() {
        // Review M2-2: 600 s at 200 W, a 3 s flap, then 10 s at 50 W.
        let mut tr = OutageTracker::new(TrackerConfig::default());
        let mut snaps: Vec<Snapshot> = (0..600)
            .map(|i| {
                let mut s = snap(i, true, 90.0);
                s.realpower_w = Some(200.0);
                s
            })
            .collect();
        snaps.extend(seconds(600, 603, false, |_| 90.0));
        snaps.extend((603..613).map(|i| {
            let mut s = snap(i, true, 89.0);
            s.realpower_w = Some(50.0);
            s
        }));
        let outages = run(&mut tr, &snaps);
        assert_eq!(outages.len(), 1);
        let w = outages[0].avg_power_w.unwrap();
        assert!((w - 197.5).abs() < 0.5, "{w}");
        assert_eq!(outages[0].samples, 610);
    }

    #[test]
    fn recharge_after_a_gap_is_not_observed() {
        // Review M2-4a: outage, PC off overnight, back at 100 %.
        let mut tr = OutageTracker::new(TrackerConfig::default());
        let mut snaps = seconds(0, 10, true, |i| 100.0 - i as f32);
        snaps.extend(seconds(10, 20, false, |_| 91.0));
        snaps.push(snap(8 * 3600, false, 100.0));
        let o = &run(&mut tr, &snaps)[0];
        assert!(o.end_observed());
        assert_eq!(o.recharged_at, None);
        assert!(o.recharge_unobserved);
    }

    #[test]
    fn flags_a_gap_inside_an_outage() {
        // Review M2-4b: battery → collector gone for 1 h → battery.
        let mut tr = OutageTracker::new(TrackerConfig::default());
        let mut snaps = seconds(0, 10, true, |_| 90.0);
        snaps.extend(seconds(3600, 3610, true, |_| 60.0));
        snaps.push(snap(3610, false, 60.0));
        let o = &run(&mut tr, &snaps)[0];
        assert!(o.gap_inside);
        assert!(o.start_observed() && o.end_observed());
    }

    #[test]
    fn keeps_tracking_after_the_clock_is_set_back() {
        // Review M2-9: a 1 h backward jump is a gap, not a reason to ignore samples.
        let mut tr = OutageTracker::new(TrackerConfig::default());
        tr.feed(&snap(3600, false, 100.0));
        assert_eq!(
            tr.feed(&snap(3599, true, 100.0)),
            Vec::<Outage>::new(),
            "small jitter ignored"
        );
        let changed = tr.feed(&snap(10, true, 100.0));
        assert_eq!(changed.len(), 1);
        assert!(!changed[0].start_observed());
    }

    #[test]
    fn clears_the_test_tag() {
        let mut tr = OutageTracker::new(TrackerConfig::default());
        tr.set_testing(true);
        tr.feed(&snap(0, true, 100.0));
        tr.set_testing(false);
        let cleared = tr.clear_test_tag();
        assert_eq!(cleared.len(), 1);
        assert!(!cleared[0].runtime_test);
        assert!(!tr.feed(&snap(1, true, 99.0))[0].runtime_test);
    }

    #[test]
    fn adapts_to_slow_polling() {
        let cfg = TrackerConfig::for_poll_interval(Duration::seconds(15));
        assert_eq!(cfg.gap, Duration::seconds(75));
        assert_eq!(cfg.max_step, Duration::seconds(30));
        assert_eq!(
            TrackerConfig::for_poll_interval(Duration::seconds(1)),
            TrackerConfig::default()
        );
    }

    #[test]
    fn ignores_unknown_status_and_out_of_order_samples() {
        let mut tr = OutageTracker::new(TrackerConfig::default());
        assert_eq!(tr.feed(&Snapshot::empty(t(0))), Vec::<Outage>::new());
        assert_eq!(tr.feed(&snap(5, true, 90.0)).len(), 1);
        assert_eq!(tr.feed(&snap(4, false, 90.0)), Vec::<Outage>::new());
        assert!(tr.current().is_some());
    }
}

#[cfg(test)]
mod estimate_tests {
    use chrono::TimeZone;
    use ups_model::UpsStatus;

    use super::*;

    fn snap(secs: i64, on_battery: bool, volts: f32) -> Snapshot {
        let mut s = Snapshot::empty(
            Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap() + Duration::seconds(secs),
        );
        s.status = Some(if on_battery {
            UpsStatus::OB
        } else {
            UpsStatus::OL
        });
        s.battery_voltage_v = Some(volts);
        s.load_pct = Some(0.0);
        s.realpower_w = Some(0.0); // below the UPS's measuring range
        s
    }

    #[test]
    fn records_battery_voltage_and_uses_the_assumed_load() {
        let mut tr = OutageTracker::new(TrackerConfig::default());
        tr.set_assumed_power(Some(20.0));
        tr.feed(&snap(0, false, 13.6));
        let mut last = Vec::new();
        for (i, v) in [(1, 12.9), (2, 12.7), (3, 12.6), (4, 12.5)] {
            last = tr.feed(&snap(i, true, v));
        }
        let o = &last[0];
        assert_eq!(o.battery_v_start, Some(12.9));
        assert_eq!(o.battery_v_end, Some(12.5));
        assert_eq!(o.battery_v_min, Some(12.5));
        assert!(o.power_estimated);
        assert_eq!(o.avg_power_w, Some(20.0));
        // 3 one-second steps at 20 W.
        assert!((o.energy_wh.unwrap() - 60.0 / 3600.0).abs() < 1e-6);
    }

    #[test]
    fn measured_power_wins_over_the_estimate() {
        let mut tr = OutageTracker::new(TrackerConfig::default());
        tr.set_assumed_power(Some(20.0));
        let mut s = snap(0, true, 12.9);
        s.realpower_w = Some(55.0);
        let o = &tr.feed(&s)[0];
        assert!(!o.power_estimated);
        assert_eq!(o.avg_power_w, Some(55.0));

        // Without an estimate, a 0 W reading stays 0 W (not estimated).
        let mut plain = OutageTracker::new(TrackerConfig::default());
        let o = &plain.feed(&snap(0, true, 12.9))[0];
        assert!(!o.power_estimated);
        assert_eq!(o.avg_power_w, Some(0.0));
    }

    #[test]
    fn tags_runtime_tests() {
        let mut tr = OutageTracker::new(TrackerConfig::default());
        assert!(!tr.feed(&snap(0, true, 12.9))[0].runtime_test);
        tr.set_testing(true);
        assert!(tr.feed(&snap(1, true, 12.8))[0].runtime_test);
        tr.set_testing(false);
        assert!(
            tr.feed(&snap(2, true, 12.7))[0].runtime_test,
            "stays tagged"
        );
    }
}
