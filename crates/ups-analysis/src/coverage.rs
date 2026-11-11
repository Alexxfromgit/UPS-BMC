//! Monitoring coverage per local calendar day.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use ups_model::{DailyCoverage, Snapshot};

/// Accumulates, per local day, the time covered by samples and the part of
/// it spent on battery. Intervals longer than `gap` are monitoring gaps and
/// count as unmonitored. Intervals crossing local midnight are split.
#[derive(Debug, Clone)]
pub struct CoverageTracker<Tz: TimeZone> {
    tz: Tz,
    gap: Duration,
    last: Option<(DateTime<Utc>, bool)>,
    pending: BTreeMap<NaiveDate, (f64, f64)>,
}

impl<Tz: TimeZone> CoverageTracker<Tz> {
    #[must_use]
    pub fn new(tz: Tz, gap: Duration) -> Self {
        Self {
            tz,
            gap,
            last: None,
            pending: BTreeMap::new(),
        }
    }

    /// Continue from the last stored sample (time, on battery).
    pub fn resume(&mut self, last_sample: Option<(DateTime<Utc>, bool)>) {
        self.last = last_sample;
    }

    pub fn feed(&mut self, s: &Snapshot) {
        let on_battery = s.on_battery().unwrap_or(false);
        if let Some((prev, prev_on_battery)) = self.last {
            if s.ts <= prev {
                // Duplicates and jitter are ignored; a clock set back further
                // than a gap restarts counting from here (review M2-9).
                if prev - s.ts > self.gap {
                    self.last = Some((s.ts, on_battery));
                }
                return;
            }
            if s.ts - prev <= self.gap {
                // The interval takes the state of the sample that began it.
                self.add(prev, s.ts, prev_on_battery);
            }
        }
        self.last = Some((s.ts, on_battery));
    }

    #[allow(clippy::cast_precision_loss)] // milliseconds within one gap
    fn add(&mut self, mut from: DateTime<Utc>, to: DateTime<Utc>, on_battery: bool) {
        while from < to {
            let day = from.with_timezone(&self.tz).date_naive();
            let next_midnight = day
                .succ_opt()
                .map_or(to, |d| crate::stats::local_midnight(&self.tz, d));
            let until = to.min(next_midnight);
            if until <= from {
                break; // defensive: time zone oddities must not loop forever
            }
            let secs = (until - from).num_milliseconds() as f64 / 1000.0;
            let entry = self.pending.entry(day).or_insert((0.0, 0.0));
            entry.0 += secs;
            if on_battery {
                entry.1 += secs;
            }
            from = until;
        }
    }

    /// Takes the accumulated increments (to be added to storage).
    pub fn take(&mut self) -> Vec<DailyCoverage> {
        std::mem::take(&mut self.pending)
            .into_iter()
            .map(|(day, (monitored_s, on_battery_s))| DailyCoverage {
                day,
                monitored_s,
                on_battery_s,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use chrono::FixedOffset;
    use ups_model::UpsStatus;

    use super::*;

    fn snap(ts: DateTime<Utc>, on_battery: bool) -> Snapshot {
        let mut s = Snapshot::empty(ts);
        s.status = Some(if on_battery {
            UpsStatus::OB
        } else {
            UpsStatus::OL
        });
        s
    }

    #[test]
    fn splits_at_local_midnight_and_skips_gaps() {
        // UTC+2: local midnight is 22:00 UTC.
        let tz = FixedOffset::east_opt(2 * 3600).unwrap();
        let mut c = CoverageTracker::new(tz, Duration::seconds(30));
        let base = Utc.with_ymd_and_hms(2026, 11, 11, 21, 59, 50).unwrap();
        c.feed(&snap(base, false));
        c.feed(&snap(base + Duration::seconds(20), true)); // 10 s day 1, 10 s day 2 (utility)
        c.feed(&snap(base + Duration::seconds(30), false)); // 10 s on battery, day 2
        c.feed(&snap(base + Duration::seconds(130), false)); // 100 s gap: ignored
        let days = c.take();
        assert_eq!(days.len(), 2);
        let d1 = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        assert_eq!(
            (days[0].day, days[0].monitored_s, days[0].on_battery_s),
            (d1, 10.0, 0.0)
        );
        assert_eq!((days[1].monitored_s, days[1].on_battery_s), (20.0, 10.0));
        assert!(c.take().is_empty(), "take drains");
    }

    #[test]
    fn restarts_after_the_clock_is_set_back() {
        let tz = FixedOffset::east_opt(0).unwrap();
        let mut c = CoverageTracker::new(tz, Duration::seconds(30));
        let base = Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap();
        c.feed(&snap(base, false));
        c.feed(&snap(base - Duration::hours(1), false));
        c.feed(&snap(
            base - Duration::hours(1) + Duration::seconds(10),
            false,
        ));
        assert_eq!(c.take()[0].monitored_s, 10.0);
    }
}
