use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// One mains power outage: the UPS ran on battery from `start` to `end`.
///
/// Monitoring can have gaps (collector stopped, PC shut down during a long
/// outage). Then the exact start or end isn't known; it lies between
/// `start_earliest`..`start` or `end`..`end_latest`, and the `*_observed`
/// flags are false.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outage {
    /// Storage ID (0 before the outage is stored).
    pub id: i64,
    /// First sample on battery.
    pub start: DateTime<Utc>,
    /// If the start wasn't observed: last sample before it (on utility).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_earliest: Option<DateTime<Utc>>,
    /// First sample back on utility (or last sample on battery if the end
    /// wasn't observed). `None` while the outage is ongoing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end: Option<DateTime<Utc>>,
    /// If the end wasn't observed: first sample after the gap.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_latest: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charge_start_pct: Option<f32>,
    /// Charge at the last sample on battery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charge_end_pct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charge_min_pct: Option<f32>,
    /// The UPS's runtime estimate when the outage began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_estimate_start_s: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_load_pct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_power_w: Option<f32>,
    /// Battery voltage at the first sample on battery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_v_start: Option<f32>,
    /// Battery voltage at the last sample on battery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_v_end: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_v_min: Option<f32>,
    /// True when power and energy figures use the user's load estimate
    /// because the UPS reported 0 (below its measuring range).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub power_estimated: bool,
    /// Energy delivered from the battery (integrated output power).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_wh: Option<f32>,
    /// When the battery was full again after the outage. `None` while
    /// recharging, or if another outage interrupted the recharge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recharged_at: Option<DateTime<Utc>>,
}

impl Outage {
    #[must_use]
    pub fn started(start: DateTime<Utc>) -> Self {
        Self {
            id: 0,
            start,
            start_earliest: None,
            end: None,
            end_latest: None,
            charge_start_pct: None,
            charge_end_pct: None,
            charge_min_pct: None,
            runtime_estimate_start_s: None,
            avg_load_pct: None,
            avg_power_w: None,
            battery_v_start: None,
            battery_v_end: None,
            battery_v_min: None,
            power_estimated: false,
            energy_wh: None,
            recharged_at: None,
        }
    }

    #[must_use]
    pub fn is_ongoing(&self) -> bool {
        self.end.is_none()
    }

    #[must_use]
    pub fn start_observed(&self) -> bool {
        self.start_earliest.is_none()
    }

    #[must_use]
    pub fn end_observed(&self) -> bool {
        self.end_latest.is_none()
    }

    /// Observed duration on battery (until `now` while ongoing).
    #[must_use]
    pub fn duration(&self, now: DateTime<Utc>) -> chrono::Duration {
        (self.end.unwrap_or(now) - self.start).max(chrono::Duration::zero())
    }

    /// Time from the end of the outage until the battery was full again.
    #[must_use]
    pub fn recharge_duration(&self) -> Option<chrono::Duration> {
        Some(self.recharged_at? - self.end?)
    }

    /// Charge used during the outage, in percentage points.
    #[must_use]
    pub fn charge_used_pct(&self) -> Option<f32> {
        Some((self.charge_start_pct? - self.charge_end_pct?).max(0.0))
    }

    /// Full-battery runtime at this outage's average load, extrapolated
    /// linearly from the observed discharge. Only meaningful for discharges of
    /// at least 5 percentage points; `None` otherwise. This is an estimate
    /// (lead-acid discharge is not linear), used as input for M3's
    /// degradation analysis.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // durations far below 2^52 ms
    pub fn projected_full_runtime_s(&self) -> Option<f64> {
        let used = f64::from(self.charge_used_pct()?);
        if used < 5.0 || self.end.is_none() {
            return None;
        }
        let secs = self.duration(self.end?).num_milliseconds() as f64 / 1000.0;
        Some(secs * 100.0 / used)
    }
}

/// Monitoring coverage of one local calendar day.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DailyCoverage {
    pub day: NaiveDate,
    /// Seconds covered by samples (gaps longer than the gap limit excluded).
    pub monitored_s: f64,
    /// Seconds of the monitored time spent on battery.
    pub on_battery_s: f64,
}

/// Outage statistics over one period (a day, week, month...).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeriodStats {
    /// Human label, e.g. `Today`, `Last 7 days`, `November 2026`.
    pub label: String,
    /// First local day of the period (inclusive).
    pub first_day: NaiveDate,
    /// Last local day of the period (inclusive).
    pub last_day: NaiveDate,
    /// Outages that started in the period.
    pub outages: u32,
    /// Time on battery within the period.
    pub on_battery_s: f64,
    /// Longest outage that started in the period.
    pub longest_s: f64,
    /// Time the collector was monitoring.
    pub monitored_s: f64,
    /// Share of the period that was monitored, in percent.
    pub coverage_pct: f64,
    /// Share of the *monitored* time with mains power, in percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_pct: Option<f64>,
    /// Average time to recharge after outages that started in the period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_recharge_s: Option<f64>,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use super::*;

    #[test]
    fn derives_durations_and_projection() {
        let t0 = Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap();
        let mut o = Outage::started(t0);
        o.charge_start_pct = Some(100.0);
        assert!(o.is_ongoing());
        assert_eq!(
            o.duration(t0 + Duration::seconds(30)),
            Duration::seconds(30)
        );
        assert_eq!(o.projected_full_runtime_s(), None);

        o.end = Some(t0 + Duration::minutes(10));
        o.charge_end_pct = Some(80.0);
        o.recharged_at = Some(t0 + Duration::minutes(70));
        assert_eq!(o.charge_used_pct(), Some(20.0));
        // 10 minutes for 20 % → 50 minutes for 100 %.
        assert_eq!(o.projected_full_runtime_s(), Some(3000.0));
        assert_eq!(o.recharge_duration(), Some(Duration::minutes(60)));
        assert!(o.start_observed() && o.end_observed());

        o.charge_end_pct = Some(97.0); // too small a discharge to extrapolate
        assert_eq!(o.projected_full_runtime_s(), None);
    }
}
