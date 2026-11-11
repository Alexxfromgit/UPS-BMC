//! Battery health estimation (M3).
//!
//! Lead-acid UPS batteries lose capacity with age. Every outage that drains a
//! measurable amount of charge is a small capacity test: from its duration and
//! the charge used, the runtime of a full battery at that load can be
//! extrapolated. Comparing recent discharges with the first ones recorded for
//! the battery shows how much capacity it has lost.
//!
//! * When the load is known (measured, or the user's estimate), discharges are
//!   compared by *capacity*: `power × full runtime`. That evens out different
//!   loads (to first order; lead-acid delivers somewhat more at light loads).
//! * Otherwise they are compared by full runtime, which assumes a similar load.
//!
//! The result is relative (vs. the baseline), labelled with a confidence, and
//! is an estimate: the UPS's charge percentage is itself derived from voltage.

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use ups_model::{Confidence, Discharge, HealthReport, HealthVerdict, Outage};

/// Thresholds of the estimator.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HealthConfig {
    /// A discharge must use at least this much charge (percentage points).
    pub min_used_pct: f32,
    /// ... last at least this long (seconds) ...
    pub min_duration_s: f64,
    /// ... and start from at least this charge (partial charges distort it).
    pub min_start_pct: f32,
    /// Discharges using this much charge (or runtime tests) are "strong".
    pub strong_used_pct: f32,
    /// Discharges averaged for the baseline and for the recent value.
    pub group: usize,
    /// Below this health the battery is "aging".
    pub aging_below_pct: f64,
    /// Below this health the battery should be replaced.
    pub replace_below_pct: f64,
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            min_used_pct: 5.0,
            min_duration_s: 60.0,
            min_start_pct: 80.0,
            strong_used_pct: 20.0,
            group: 3,
            aging_below_pct: 80.0,
            replace_below_pct: 60.0,
        }
    }
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        f64::midpoint(v[n / 2 - 1], v[n / 2])
    })
}

/// The measurements of one outage, if it is usable for health estimation.
#[must_use]
pub fn discharge(o: &Outage, cfg: &HealthConfig) -> Option<Discharge> {
    let end = o.end?;
    if !o.start_observed() || !o.end_observed() {
        return None;
    }
    let start_pct = o.charge_start_pct?;
    let used = o.charge_used_pct()?;
    #[allow(clippy::cast_precision_loss)] // durations far below 2^52 ms
    let duration_s = (end - o.start).num_milliseconds() as f64 / 1000.0;
    if start_pct < cfg.min_start_pct || used < cfg.min_used_pct || duration_s < cfg.min_duration_s {
        return None;
    }
    let full_runtime_s = duration_s * 100.0 / f64::from(used);
    let power = o.avg_power_w.filter(|w| *w > 0.0);
    let vs_ups = o
        .runtime_estimate_start_s
        .filter(|e| *e > 0)
        .map(|e| full_runtime_s * f64::from(start_pct) / f64::from(e));
    let sag = match (o.battery_v_start, o.battery_v_min) {
        (Some(a), Some(m)) if a > m => Some(f64::from(a - m) * 1000.0 / f64::from(used)),
        _ => None,
    };
    Some(Discharge {
        outage_id: o.id,
        start: o.start,
        duration_s,
        charge_start_pct: start_pct,
        charge_used_pct: used,
        avg_power_w: power,
        power_estimated: power.is_some() && o.power_estimated,
        runtime_test: o.runtime_test,
        full_runtime_s,
        capacity_wh: power.map(|w| f64::from(w) * full_runtime_s / 3600.0),
        vs_ups_estimate_pct: vs_ups,
        sag_mv_per_pct: sag,
        relative_pct: None,
        baseline: false,
    })
}

fn downgrade(c: Confidence) -> Confidence {
    match c {
        Confidence::High => Confidence::Medium,
        _ => Confidence::Low,
    }
}

/// Assesses battery health from recorded outages (any order).
///
/// Only outages starting on or after `installed` count (a new battery starts
/// a new baseline). `ups_replace_flag` is the UPS's own "replace battery" flag.
#[must_use]
#[allow(clippy::too_many_lines)] // one linear assessment, easier to follow in one piece
pub fn assess(
    outages: &[Outage],
    installed: Option<NaiveDate>,
    ups_replace_flag: bool,
    now: DateTime<Utc>,
    cfg: &HealthConfig,
) -> HealthReport {
    let mut relevant: Vec<&Outage> = outages
        .iter()
        .filter(|o| o.end.is_some())
        .filter(|o| installed.is_none_or(|d| o.start.date_naive() >= d))
        .collect();
    relevant.sort_by_key(|o| o.start);
    let mut all: Vec<Discharge> = relevant.iter().filter_map(|o| discharge(o, cfg)).collect();
    let skipped = u32::try_from(relevant.len() - all.len()).unwrap_or(u32::MAX);

    // Compare by capacity when the load is known for (nearly) all discharges.
    let with_power = all.iter().filter(|d| d.capacity_wh.is_some()).count();
    let by_capacity = with_power >= 2 && with_power * 2 >= all.len();
    let metric = |d: &Discharge| {
        if by_capacity {
            d.capacity_wh
        } else {
            Some(d.full_runtime_s)
        }
    };
    let used: Vec<usize> = (0..all.len())
        .filter(|&i| metric(&all[i]).is_some())
        .collect();
    let n = used.len();

    let mut reasons = Vec::new();
    if ups_replace_flag {
        reasons.push("The UPS itself reports that the battery needs replacing.".to_owned());
    }

    let mut health_pct = None;
    let mut confidence = Confidence::None;
    let recent: Vec<usize>;
    if n >= 2 {
        let b = cfg.group.min(n / 2).max(1);
        let r = cfg.group.min(n - b).max(1);
        let baseline_ids = &used[..b];
        recent = used[n - r..].to_vec();
        let base = median(
            baseline_ids
                .iter()
                .filter_map(|&i| metric(&all[i]))
                .collect(),
        );
        let now_v = median(recent.iter().filter_map(|&i| metric(&all[i])).collect());
        if let (Some(base), Some(now_v)) = (base, now_v)
            && base > 0.0
        {
            for &i in &used {
                all[i].relative_pct = metric(&all[i]).map(|m| m / base * 100.0);
            }
            for &i in baseline_ids {
                all[i].baseline = true;
            }
            let h = (now_v / base * 100.0).clamp(0.0, 100.0);
            health_pct = Some(h);

            confidence = if n >= 2 * cfg.group {
                Confidence::High
            } else if n >= 4 {
                Confidence::Medium
            } else {
                Confidence::Low
            };
            let strong =
                |i: &usize| all[*i].runtime_test || all[*i].charge_used_pct >= cfg.strong_used_pct;
            if !recent.iter().any(strong) || !baseline_ids.iter().any(strong) {
                confidence = downgrade(confidence);
            }
            let powers: Vec<f32> = used.iter().filter_map(|&i| all[i].avg_power_w).collect();
            let spread = powers.iter().copied().fold(f32::MIN, f32::max)
                / powers.iter().copied().fold(f32::MAX, f32::min);
            if !by_capacity || (powers.len() >= 2 && spread > 2.0) {
                confidence = downgrade(confidence);
            }

            let since = all[used[0]]
                .start
                .with_timezone(&chrono::Local)
                .format("%d.%m.%Y");
            let what = if by_capacity { "capacity" } else { "runtime" };
            reasons.push(format!(
                "Recent discharges show {h:.0} % of the {what} measured in the first ones (since {since})."
            ));
            if !by_capacity {
                reasons.push(
                    "Load is unknown, so runtimes are compared directly; this assumes a similar load. \
                     Set your estimated load in Settings for a better comparison."
                        .to_owned(),
                );
            }
        }
    } else {
        recent = used.clone();
        let missing = 2 - n;
        reasons.push(format!(
            "Needs {missing} more outage{} (or runtime test) using at least {:.0} % of charge from a \
             nearly full battery to compare. Shorter outages are recorded but not used.",
            if missing == 1 { "" } else { "s" },
            cfg.min_used_pct
        ));
    }

    // The UPS's own runtime prediction vs. what the battery delivered.
    if let Some(vs) = median(
        recent
            .iter()
            .filter_map(|&i| all[i].vs_ups_estimate_pct)
            .collect(),
    ) && vs < 70.0
    {
        reasons.push(format!(
            "Recent outages lasted only about {vs:.0} % of the runtime the UPS predicted."
        ));
    }

    if let Some(d) = installed {
        #[allow(clippy::cast_precision_loss)] // days
        let years = (now.date_naive() - d).num_days().max(0) as f64 / 365.25;
        if years >= 4.0 {
            reasons.push(format!(
                "The battery is {years:.1} years old; lead-acid UPS batteries usually last 3–5 years."
            ));
        } else if years >= 3.0 {
            reasons.push(format!(
                "The battery is {years:.1} years old: lead-acid UPS batteries usually last 3–5 years, \
                 so expect a replacement in the next year or two."
            ));
        }
    }
    if used.iter().any(|&i| all[i].power_estimated) {
        reasons.push(
            "Load figures use your estimate (the UPS cannot measure loads this small).".to_owned(),
        );
    }
    if skipped > 0 {
        reasons.push(format!(
            "{skipped} other outage{} didn't qualify (too short, partial or not fully observed).",
            if skipped == 1 { "" } else { "s" }
        ));
    }

    let verdict = if ups_replace_flag {
        HealthVerdict::Replace
    } else {
        match health_pct {
            None => HealthVerdict::Unknown,
            Some(h) if h < cfg.replace_below_pct => HealthVerdict::Replace,
            Some(h) if h < cfg.aging_below_pct => HealthVerdict::Aging,
            Some(_) => HealthVerdict::Good,
        }
    };

    // Expected full runtime at the typical recent load.
    let reference_power = median(
        recent
            .iter()
            .filter_map(|&i| all[i].avg_power_w)
            .map(f64::from)
            .collect(),
    );
    #[allow(clippy::cast_possible_truncation)] // watts
    let reference_power_w = reference_power.map(|w| w as f32);
    let full_runtime_s = match (by_capacity, reference_power) {
        (true, Some(w)) if w > 0.0 => {
            median(recent.iter().filter_map(|&i| all[i].capacity_wh).collect())
                .map(|wh| wh * 3600.0 / w)
        }
        _ => median(recent.iter().map(|&i| all[i].full_runtime_s).collect()),
    };

    HealthReport {
        generated_at: now,
        verdict,
        confidence,
        health_pct,
        metric: if by_capacity { "capacity" } else { "runtime" }.to_owned(),
        reference_power_w,
        reference_power_estimated: recent.iter().any(|&i| all[i].power_estimated),
        full_runtime_s,
        reasons,
        discharges: all,
        skipped,
        battery_installed: installed,
        ups_replace_flag,
    }
}

/// Age of the battery in whole months, for display.
#[must_use]
pub fn age_months(installed: NaiveDate, today: NaiveDate) -> u32 {
    let month = |d: NaiveDate| i64::from(d.year()) * 12 + i64::from(d.month());
    let months = month(today) - month(installed) - i64::from(today.day() < installed.day());
    u32::try_from(months).unwrap_or(0)
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, clippy::float_cmp)]
mod tests {
    use chrono::{Duration, TimeZone};

    use super::*;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap()
    }

    /// An outage `day` days after t0 lasting `minutes`, using `used` % at `watts`.
    fn outage(id: i64, day: i64, minutes: i64, used: f32, watts: Option<f32>) -> Outage {
        let mut o = Outage::started(t0() + Duration::days(day));
        o.id = id;
        o.end = Some(o.start + Duration::minutes(minutes));
        o.charge_start_pct = Some(100.0);
        o.charge_end_pct = Some(100.0 - used);
        o.avg_power_w = watts;
        o.runtime_estimate_start_s = Some(3600);
        o.battery_v_start = Some(13.0);
        o.battery_v_min = Some(13.0 - used * 0.01);
        o
    }

    #[test]
    fn needs_two_usable_discharges() {
        let cfg = HealthConfig::default();
        let short = outage(1, 0, 10, 2.0, Some(20.0)); // only 2 %
        let r = assess(&[short], None, false, t0(), &cfg);
        assert_eq!(r.verdict, HealthVerdict::Unknown);
        assert_eq!(r.confidence, Confidence::None);
        assert_eq!(r.skipped, 1);
        assert_eq!(r.discharges, Vec::<ups_model::Discharge>::new());
        assert!(r.reasons[0].starts_with("Needs 2 more outages"));

        let one = outage(2, 1, 30, 25.0, Some(20.0));
        let r = assess(&[one], None, false, t0(), &cfg);
        assert_eq!(r.verdict, HealthVerdict::Unknown);
        // 30 min for 25 % → 2 h from full.
        assert_eq!(r.full_runtime_s, Some(7200.0));
        assert!(r.reasons[0].starts_with("Needs 1 more outage "));
    }

    #[test]
    fn compares_recent_capacity_with_the_baseline() {
        let cfg = HealthConfig::default();
        // Baseline: 2 h full runtime at 20 W = 40 Wh. Later: 1.5 h = 30 Wh.
        let mut v = Vec::new();
        for (i, day) in [0, 10, 20].into_iter().enumerate() {
            v.push(outage(
                i64::try_from(i).unwrap() + 1,
                day,
                30,
                25.0,
                Some(20.0),
            ));
        }
        for (i, day) in [300, 310, 320].into_iter().enumerate() {
            v.push(outage(
                i64::try_from(i).unwrap() + 10,
                day,
                45,
                50.0,
                Some(20.0),
            ));
        }
        let r = assess(&v, None, false, t0() + Duration::days(330), &cfg);
        assert_eq!(r.metric, "capacity");
        assert_eq!(r.health_pct, Some(75.0));
        assert_eq!(r.verdict, HealthVerdict::Aging);
        assert_eq!(r.confidence, Confidence::High);
        assert_eq!(r.full_runtime_s, Some(5400.0));
        assert_eq!(r.reference_power_w, Some(20.0));
        assert!(r.discharges[0].baseline && !r.discharges[3].baseline);
        assert_eq!(r.discharges[0].capacity_wh, Some(40.0));
        assert_eq!(r.discharges[5].relative_pct, Some(75.0));
        assert!(
            r.reasons[0].contains("75 % of the capacity"),
            "{:?}",
            r.reasons
        );
    }

    #[test]
    fn normalises_different_loads_by_capacity() {
        let cfg = HealthConfig::default();
        // Same 40 Wh battery: 2 h at 20 W, 1 h at 40 W.
        let a = outage(1, 0, 30, 25.0, Some(20.0));
        let b = outage(2, 5, 15, 25.0, Some(40.0));
        let r = assess(&[b, a], None, false, t0(), &cfg);
        assert_eq!(r.health_pct, Some(100.0));
        assert_eq!(r.verdict, HealthVerdict::Good);
        // Two discharges only, and the loads differ by 2×: low confidence.
        assert_eq!(r.confidence, Confidence::Low);
    }

    #[test]
    fn falls_back_to_runtime_without_load_and_honours_the_install_date() {
        let cfg = HealthConfig::default();
        let old = outage(1, 0, 10, 25.0, None); // before the new battery
        let a = outage(2, 100, 30, 25.0, None);
        let b = outage(3, 110, 12, 25.0, None);
        let installed = (t0() + Duration::days(50)).date_naive();
        let r = assess(&[old, a, b], Some(installed), false, t0(), &cfg);
        assert_eq!(r.metric, "runtime");
        assert_eq!(r.discharges.len(), 2);
        assert_eq!(r.health_pct, Some(40.0));
        assert_eq!(r.verdict, HealthVerdict::Replace);
        assert_eq!(r.confidence, Confidence::Low);
        assert!(r.reasons.iter().any(|x| x.contains("Load is unknown")));
    }

    #[test]
    fn the_ups_flag_and_its_estimate_are_reported() {
        let cfg = HealthConfig::default();
        let mut a = outage(1, 0, 30, 25.0, Some(20.0));
        // Full runtime 2 h from 100 % vs. a 4 h UPS estimate: 50 %.
        a.runtime_estimate_start_s = Some(14_400);
        let r = assess(&[a], None, true, t0(), &cfg);
        assert_eq!(r.verdict, HealthVerdict::Replace);
        assert!(r.reasons[0].contains("UPS itself"));
        assert!(
            r.reasons.iter().any(|x| x.contains("about 50 %")),
            "{:?}",
            r.reasons
        );
        assert_eq!(r.discharges[0].vs_ups_estimate_pct, Some(50.0));
        assert_eq!(r.discharges[0].sag_mv_per_pct, Some(10.0));
    }

    #[test]
    fn skips_unobserved_and_partial_discharges() {
        let cfg = HealthConfig::default();
        let mut gap = outage(1, 0, 30, 25.0, Some(20.0));
        gap.end_latest = gap.end.map(|e| e + Duration::hours(1));
        let mut partial = outage(2, 1, 30, 25.0, Some(20.0));
        partial.charge_start_pct = Some(60.0);
        let mut ongoing = outage(3, 2, 30, 25.0, Some(20.0));
        ongoing.end = None;
        let r = assess(&[gap, partial, ongoing], None, false, t0(), &cfg);
        assert_eq!(r.discharges, Vec::<ups_model::Discharge>::new());
        assert_eq!(r.skipped, 2, "ongoing outages are not counted");
    }

    #[test]
    fn counts_battery_age() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        assert_eq!(age_months(d(2024, 3, 15), d(2026, 3, 14)), 23);
        assert_eq!(age_months(d(2024, 3, 15), d(2026, 3, 15)), 24);
        assert_eq!(age_months(d(2026, 3, 15), d(2026, 1, 1)), 0);
        let cfg = HealthConfig::default();
        let r = assess(&[], Some(d(2021, 1, 1)), false, t0(), &cfg);
        assert!(
            r.reasons.iter().any(|x| x.contains("5.0 years old")),
            "{:?}",
            r.reasons
        );
    }
}
