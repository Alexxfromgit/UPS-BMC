use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::UpsStatus;

/// One point-in-time reading of a UPS, independent of where it came from.
///
/// Every measurement, including the status, is optional. Entry-level units such
/// as the Eaton 5E don't expose every value, and a failed or stale read must
/// stay distinguishable from a real reading: the app must never invent data.
///
/// The serialised form (JSON, storage, API) uses NUT variable names
/// (`battery.charge`, `ups.status`, ...). Values that weren't read are left out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// When the reading was taken (UTC).
    pub ts: DateTime<Utc>,
    /// `ups.status`. `None` means the status wasn't read (or is stale).
    #[serde(
        rename = "ups.status",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub status: Option<UpsStatus>,
    /// `battery.charge`, percent.
    #[serde(
        rename = "battery.charge",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub battery_charge_pct: Option<f32>,
    /// `battery.runtime`, seconds (the UPS's own estimate).
    #[serde(
        rename = "battery.runtime",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub battery_runtime_s: Option<u32>,
    /// `battery.voltage`, volts.
    #[serde(
        rename = "battery.voltage",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub battery_voltage_v: Option<f32>,
    /// `ups.load`, percent of the UPS's nominal capacity.
    #[serde(rename = "ups.load", default, skip_serializing_if = "Option::is_none")]
    pub load_pct: Option<f32>,
    /// `input.voltage`, volts.
    #[serde(
        rename = "input.voltage",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub input_voltage_v: Option<f32>,
    /// `input.frequency`, hertz.
    #[serde(
        rename = "input.frequency",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub input_frequency_hz: Option<f32>,
    /// `output.voltage`, volts.
    #[serde(
        rename = "output.voltage",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub output_voltage_v: Option<f32>,
    /// `ups.realpower.nominal`, watts.
    #[serde(
        rename = "ups.realpower.nominal",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub realpower_nominal_w: Option<f32>,
    /// `ups.power.nominal`, volt-amperes.
    #[serde(
        rename = "ups.power.nominal",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub power_nominal_va: Option<f32>,
    /// Any other variables the source exposes, keyed by NUT-style name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, String>,
}

impl Snapshot {
    /// An empty snapshot at `ts` with no measurements.
    #[must_use]
    pub fn empty(ts: DateTime<Utc>) -> Self {
        Self {
            ts,
            status: None,
            battery_charge_pct: None,
            battery_runtime_s: None,
            battery_voltage_v: None,
            load_pct: None,
            input_voltage_v: None,
            input_frequency_hz: None,
            output_voltage_v: None,
            realpower_nominal_w: None,
            power_nominal_va: None,
            extra: BTreeMap::new(),
        }
    }

    /// `Some(true)` on battery, `Some(false)` otherwise, and `None` if the
    /// status is unknown.
    #[must_use]
    pub fn on_battery(&self) -> Option<bool> {
        self.status.map(UpsStatus::on_battery)
    }

    /// Rough real power drawn by the load, in watts:
    /// `ups.load` × `ups.realpower.nominal`.
    ///
    /// This assumes `ups.load` is relative to *real* power. Some devices report
    /// it relative to VA, which overestimates the result by the load's power
    /// factor. Confirm which applies against a real capture before relying on it.
    #[must_use]
    pub fn estimated_load_w(&self) -> Option<f32> {
        Some(self.load_pct? / 100.0 * self.realpower_nominal_w?)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn at_noon() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap()
    }

    #[test]
    fn estimated_load_needs_both_values() {
        let mut s = Snapshot::empty(at_noon());
        assert_eq!(s.estimated_load_w(), None);
        s.load_pct = Some(25.0);
        assert_eq!(s.estimated_load_w(), None);
        s.realpower_nominal_w = Some(540.0);
        assert_eq!(s.estimated_load_w(), Some(135.0));
    }

    #[test]
    fn unknown_status_is_not_online() {
        let mut s = Snapshot::empty(at_noon());
        assert_eq!(s.on_battery(), None);
        s.status = Some(UpsStatus::OB | UpsStatus::DISCHRG);
        assert_eq!(s.on_battery(), Some(true));
    }

    /// Golden wire format: storage and the API will depend on this.
    #[test]
    fn serialises_with_nut_names() {
        let mut s = Snapshot::empty(at_noon());
        s.status = Some(UpsStatus::OL | UpsStatus::CHRG);
        s.battery_charge_pct = Some(87.0);
        s.battery_runtime_s = Some(1830);
        s.load_pct = Some(12.0);
        s.extra.insert("ups.beeper.status".into(), "enabled".into());

        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(
            json,
            r#"{"ts":"2026-11-11T12:00:00Z","ups.status":"OL CHRG","battery.charge":87.0,"battery.runtime":1830,"ups.load":12.0,"extra":{"ups.beeper.status":"enabled"}}"#
        );
        assert_eq!(serde_json::from_str::<Snapshot>(&json).unwrap(), s);
    }

    #[test]
    fn empty_snapshot_round_trips() {
        let s = Snapshot::empty(at_noon());
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"ts":"2026-11-11T12:00:00Z"}"#);
        assert_eq!(serde_json::from_str::<Snapshot>(&json).unwrap(), s);
    }
}
