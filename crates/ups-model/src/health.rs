//! Battery health (M3): per-discharge measurements and the overall estimate.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// How much the health estimate can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    /// Not enough discharges to compare.
    None,
    Low,
    Medium,
    High,
}

impl Confidence {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::None => "not enough data",
            Self::Low => "low confidence",
            Self::Medium => "medium confidence",
            Self::High => "high confidence",
        }
    }
}

/// Overall assessment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthVerdict {
    /// Fewer than two usable discharges so far.
    Unknown,
    Good,
    /// Noticeably weaker than at the start: plan a replacement.
    Aging,
    /// Much weaker, or the UPS itself asks for a new battery.
    Replace,
}

/// One discharge (outage or runtime test) usable for health estimation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // independent flags of a wire record
pub struct Discharge {
    pub outage_id: i64,
    pub start: DateTime<Utc>,
    pub duration_s: f64,
    pub charge_start_pct: f32,
    pub charge_used_pct: f32,
    /// Average output power (measured, or the user's estimate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avg_power_w: Option<f32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub power_estimated: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub runtime_test: bool,
    /// Runtime from 100 % to 0 % at this discharge's load, extrapolated linearly.
    pub full_runtime_s: f64,
    /// Energy the full battery would deliver at this load (`power × runtime`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_wh: Option<f64>,
    /// Observed runtime as a share of the UPS's own estimate at the start, in percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vs_ups_estimate_pct: Option<f64>,
    /// Battery voltage drop per percentage point of charge used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sag_mv_per_pct: Option<f64>,
    /// This discharge compared with the baseline, in percent (`None` without a baseline).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative_pct: Option<f64>,
    /// Part of the baseline (the first discharges since the battery was installed).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub baseline: bool,
    /// Used in the health comparison (false: no load value while discharges
    /// are compared by capacity).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compared: bool,
}

/// Battery health estimate. Built from discharges since the battery was
/// installed (or since monitoring started): it compares recent discharges
/// with the first ones, so it shows the *decline*, not an absolute capacity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthReport {
    pub generated_at: DateTime<Utc>,
    pub verdict: HealthVerdict,
    pub confidence: Confidence,
    /// Recent capacity relative to the baseline, in percent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_pct: Option<f64>,
    /// `capacity` (power-normalised, Wh) or `runtime` (load unknown).
    pub metric: String,
    /// Typical load of the recent discharges, used for `full_runtime_s`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_power_w: Option<f32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reference_power_estimated: bool,
    /// Expected runtime from a full battery at the reference load, from recent discharges.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub full_runtime_s: Option<f64>,
    /// Short explanations of the verdict, most important first.
    pub reasons: Vec<String>,
    /// Usable discharges, oldest first.
    pub discharges: Vec<Discharge>,
    /// Outages considered but too short, partial or not fully observed.
    pub skipped: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_installed: Option<NaiveDate>,
    /// The UPS reports "replace battery" (RB flag).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub ups_replace_flag: bool,
}

/// Phase of a guided runtime test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeTestPhase {
    /// Waiting for the user to unplug the UPS from the mains.
    Armed,
    /// On battery, discharging towards the stop level.
    Running,
    /// The stop level was reached: the user should restore mains power.
    RestorePower,
}

/// A guided runtime test in progress (part of the live status).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeTestStatus {
    pub phase: RuntimeTestPhase,
    /// When the test was armed.
    pub armed_at: DateTime<Utc>,
    /// Charge at which the user is asked to restore mains power.
    pub stop_at_pct: f32,
    /// When the discharge started (`Running` and later).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub charge_start_pct: Option<f32>,
}
