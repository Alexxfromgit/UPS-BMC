//! A deterministic simulated UPS, for UI development, demos and tests.
//!
//! It repeats a 5-minute cycle:
//! * **0–60 s** on utility, fully charged;
//! * **60–150 s** power outage: discharging, and low battery near the end;
//! * **150–300 s** power restored, recharging.
//!
//! The values look like the Eaton 5E 900 (900 VA / 480 W, 12 V lead-acid).

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use ups_model::{DeviceInfo, Snapshot, UpsStatus, vars};

use crate::{Setting, SourceError, TelemetrySource};

/// Length of one simulated cycle.
pub const CYCLE: Duration = Duration::from_secs(300);
const OUTAGE_START: f64 = 60.0;
const OUTAGE_END: f64 = 150.0;
const LOW_BATTERY_PCT: f64 = 20.0;
const NOMINAL_W: f64 = 480.0;

/// Simulated UPS.
#[derive(Debug)]
pub struct SimSource {
    started: Instant,
    beeper: bool,
}

impl SimSource {
    #[must_use]
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            beeper: true,
        }
    }
}

impl Default for SimSource {
    fn default() -> Self {
        Self::new()
    }
}

impl TelemetrySource for SimSource {
    fn kind(&self) -> &'static str {
        "sim"
    }

    fn connect(&mut self) -> Result<DeviceInfo, SourceError> {
        Ok(DeviceInfo {
            source: "sim".into(),
            manufacturer: Some("UPS-BMC".into()),
            model: Some("Simulated UPS 900".into()),
            serial: None,
            battery_type: Some("PbAc".into()),
            vendor_id: None,
            product_id: None,
        })
    }

    fn poll(&mut self) -> Result<Snapshot, SourceError> {
        let mut s = snapshot_at(self.started.elapsed(), Utc::now());
        let beeper = if self.beeper { "enabled" } else { "disabled" };
        s.extra
            .insert(vars::UPS_BEEPER_STATUS.into(), beeper.into());
        Ok(s)
    }

    fn capabilities(&self) -> Vec<&'static str> {
        vec![crate::CAP_BEEPER]
    }

    fn apply(&mut self, setting: Setting) -> Result<(), SourceError> {
        match setting {
            Setting::Beeper { enabled } => self.beeper = enabled,
        }
        Ok(())
    }
}

/// The simulated reading `elapsed` after start (pure, for tests).
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // bounded values
pub fn snapshot_at(elapsed: Duration, ts: DateTime<Utc>) -> Snapshot {
    let t = elapsed.as_secs_f64() % CYCLE.as_secs_f64();
    let wobble = (elapsed.as_secs_f64() / 7.0).sin();
    let load = 20.0 + 3.0 * wobble;

    let (on_battery, charge) = if t < OUTAGE_START {
        (false, 100.0)
    } else if t < OUTAGE_END {
        (true, 100.0 - (t - OUTAGE_START) * 0.9)
    } else {
        let low = 100.0 - (OUTAGE_END - OUTAGE_START) * 0.9;
        (false, (low + (t - OUTAGE_END) * 0.54).min(100.0))
    };
    let charging = !on_battery && charge < 100.0;

    let mut status = if on_battery {
        UpsStatus::OB | UpsStatus::DISCHRG
    } else {
        UpsStatus::OL
    };
    if charging {
        status |= UpsStatus::CHRG;
    }
    if on_battery && charge < LOW_BATTERY_PCT {
        status |= UpsStatus::LB;
    }

    let mut s = Snapshot::empty(ts);
    s.status = Some(status);
    s.battery_charge_pct = Some(charge.round() as f32);
    // Runtime estimate: about 30 min at 20 % load from full charge.
    s.battery_runtime_s = Some((charge * 18.0 * (20.0 / load)).round() as u32);
    s.battery_voltage_v = Some(if on_battery {
        12.9 - (100.0 - charge) * 0.012
    } else if charging {
        14.1
    } else {
        13.6
    } as f32);
    s.load_pct = Some(load.round() as f32);
    s.input_voltage_v = Some(if on_battery {
        0.0
    } else {
        229.0 + 2.0 * wobble
    } as f32);
    s.output_voltage_v = Some(if on_battery {
        230.0
    } else {
        229.0 + 2.0 * wobble
    } as f32);
    s.output_frequency_hz = Some(50.0);
    s.realpower_w = Some((load / 100.0 * NOMINAL_W).round() as f32);
    s.power_va = Some((load / 100.0 * 900.0).round() as f32);
    s.realpower_nominal_w = Some(NOMINAL_W as f32);
    s.power_nominal_va = Some(900.0);
    s.extra.insert(vars::BATTERY_CHARGE_LOW.into(), "20".into());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(secs: u64) -> Snapshot {
        snapshot_at(Duration::from_secs(secs), Utc::now())
    }

    #[test]
    fn follows_the_outage_cycle() {
        let online = at(10);
        assert_eq!(online.status, Some(UpsStatus::OL));
        assert_eq!(online.battery_charge_pct, Some(100.0));

        let outage = at(100);
        assert!(
            outage
                .status
                .unwrap()
                .contains(UpsStatus::OB | UpsStatus::DISCHRG)
        );
        assert!(outage.battery_charge_pct.unwrap() < 100.0);
        assert_eq!(outage.input_voltage_v, Some(0.0));

        let low = at(149);
        assert!(low.status.unwrap().contains(UpsStatus::LB));

        let recharge = at(200);
        assert_eq!(recharge.status, Some(UpsStatus::OL | UpsStatus::CHRG));

        // The cycle repeats.
        assert_eq!(at(310).status, online.status);
    }
}
