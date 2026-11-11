//! NUT-style variable names used as the canonical vocabulary.
//!
//! See <https://networkupstools.org/docs/user-manual.chunked/nut-names.html>
//! and RFC 9271, which standardises the variable names.

pub const UPS_STATUS: &str = "ups.status";
pub const UPS_ALARM: &str = "ups.alarm";
pub const UPS_LOAD: &str = "ups.load";
pub const UPS_REALPOWER_NOMINAL: &str = "ups.realpower.nominal";
pub const UPS_POWER_NOMINAL: &str = "ups.power.nominal";
pub const UPS_BEEPER_STATUS: &str = "ups.beeper.status";
pub const UPS_TEST_RESULT: &str = "ups.test.result";
pub const UPS_MFR: &str = "ups.mfr";
pub const UPS_MODEL: &str = "ups.model";
pub const UPS_SERIAL: &str = "ups.serial";
pub const UPS_FIRMWARE: &str = "ups.firmware";

pub const BATTERY_CHARGE: &str = "battery.charge";
pub const BATTERY_CHARGE_LOW: &str = "battery.charge.low";
pub const BATTERY_RUNTIME: &str = "battery.runtime";
pub const BATTERY_RUNTIME_LOW: &str = "battery.runtime.low";
pub const BATTERY_VOLTAGE: &str = "battery.voltage";
pub const BATTERY_TYPE: &str = "battery.type";

pub const INPUT_VOLTAGE: &str = "input.voltage";
pub const INPUT_FREQUENCY: &str = "input.frequency";
pub const INPUT_TRANSFER_LOW: &str = "input.transfer.low";
pub const INPUT_TRANSFER_HIGH: &str = "input.transfer.high";

pub const OUTPUT_VOLTAGE: &str = "output.voltage";
pub const OUTPUT_FREQUENCY: &str = "output.frequency";
