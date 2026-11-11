use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// One (possibly aggregated) point of telemetry history. Numeric values are
/// averages over the bucket. `on_battery` is true if any sample in the bucket
/// was on battery.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryPoint {
    pub ts: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_battery: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_charge_pct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_runtime_s: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_voltage_v: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_pct: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_voltage_v: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_voltage_v: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub realpower_w: Option<f32>,
}
