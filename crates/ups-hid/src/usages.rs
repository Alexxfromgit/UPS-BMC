//! Usage names from the USB "HID Usage Tables for Power Devices" (Release 1.0).

use crate::Usage;

pub const PAGE_POWER_DEVICE: u16 = 0x84;
pub const PAGE_BATTERY_SYSTEM: u16 = 0x85;

/// Human-readable usage page name.
#[must_use]
pub fn page_name(page: u16) -> &'static str {
    match page {
        0x01 => "GenericDesktop",
        0x06 => "GenericDevice",
        0x08 => "LED",
        0x09 => "Button",
        0x0F => "PID",
        PAGE_POWER_DEVICE => "PowerDevice",
        PAGE_BATTERY_SYSTEM => "BatterySystem",
        0xFF00..=0xFFFF => "VendorDefined",
        _ => "Unknown",
    }
}

/// True for UPS-related top-level collections (what `hid-probe` looks for).
#[must_use]
pub fn is_power_page(page: u16) -> bool {
    matches!(page, PAGE_POWER_DEVICE | PAGE_BATTERY_SYSTEM)
}

/// Usages whose value is a USB string descriptor index rather than a quantity.
#[must_use]
pub fn is_string_index(usage: Usage) -> bool {
    matches!(
        (usage.page, usage.id),
        (PAGE_POWER_DEVICE, 0x01 | 0xFD | 0xFE | 0xFF)
            | (PAGE_BATTERY_SYSTEM, 0x87 | 0x88 | 0x89 | 0x8F)
    )
}

/// Usages that carry a device serial number (directly or as a string index).
#[must_use]
pub fn is_serial_number(usage: Usage) -> bool {
    matches!(
        (usage.page, usage.id),
        (PAGE_POWER_DEVICE, 0xFF) | (PAGE_BATTERY_SYSTEM, 0x86)
    )
}

/// Name of a usage, if it is a known Power Device / Battery System usage.
#[must_use]
pub fn usage_name(usage: Usage) -> Option<&'static str> {
    match usage.page {
        PAGE_POWER_DEVICE => power_device(usage.id),
        PAGE_BATTERY_SYSTEM => battery_system(usage.id),
        _ => None,
    }
}

/// The bare usage name (`RemainingCapacity`), or `0xPPPP:0xIIII` for unknown usages.
#[must_use]
pub fn short_name(usage: Usage) -> String {
    usage_name(usage).map_or_else(
        || format!("0x{:04X}:0x{:04X}", usage.page, usage.id),
        str::to_owned,
    )
}

/// NUT-style dotted path (`UPS.PowerSummary.RemainingCapacity`), the same
/// form NUT's `usbhid-ups -DD` logs and its mge-hid mapping tables use.
#[must_use]
pub fn nut_path(collection_path: &[Usage], usage: Usage) -> String {
    collection_path
        .iter()
        .chain(std::iter::once(&usage))
        .map(|u| short_name(*u))
        .collect::<Vec<_>>()
        .join(".")
}

/// `Page.Name`, falling back to hex for unknown usages.
#[must_use]
pub fn display(usage: Usage) -> String {
    match usage_name(usage) {
        Some(name) => format!("{}.{name}", page_name(usage.page)),
        None => format!("0x{:04X}:0x{:04X}", usage.page, usage.id),
    }
}

#[allow(clippy::too_many_lines)]
fn power_device(id: u16) -> Option<&'static str> {
    Some(match id {
        0x01 => "iName",
        0x02 => "PresentStatus",
        0x03 => "ChangedStatus",
        0x04 => "UPS",
        0x05 => "PowerSupply",
        0x10 => "BatterySystem",
        0x11 => "BatterySystemID",
        0x12 => "Battery",
        0x13 => "BatteryID",
        0x14 => "Charger",
        0x15 => "ChargerID",
        0x16 => "PowerConverter",
        0x17 => "PowerConverterID",
        0x18 => "OutletSystem",
        0x19 => "OutletSystemID",
        0x1A => "Input",
        0x1B => "InputID",
        0x1C => "Output",
        0x1D => "OutputID",
        0x1E => "Flow",
        0x1F => "FlowID",
        0x20 => "Outlet",
        0x21 => "OutletID",
        0x22 => "Gang",
        0x23 => "GangID",
        0x24 => "PowerSummary",
        0x25 => "PowerSummaryID",
        0x30 => "Voltage",
        0x31 => "Current",
        0x32 => "Frequency",
        0x33 => "ApparentPower",
        0x34 => "ActivePower",
        0x35 => "PercentLoad",
        0x36 => "Temperature",
        0x37 => "Humidity",
        0x38 => "BadCount",
        0x40 => "ConfigVoltage",
        0x41 => "ConfigCurrent",
        0x42 => "ConfigFrequency",
        0x43 => "ConfigApparentPower",
        0x44 => "ConfigActivePower",
        0x45 => "ConfigPercentLoad",
        0x46 => "ConfigTemperature",
        0x47 => "ConfigHumidity",
        0x50 => "SwitchOnControl",
        0x51 => "SwitchOffControl",
        0x52 => "ToggleControl",
        0x53 => "LowVoltageTransfer",
        0x54 => "HighVoltageTransfer",
        0x55 => "DelayBeforeReboot",
        0x56 => "DelayBeforeStartup",
        0x57 => "DelayBeforeShutdown",
        0x58 => "Test",
        0x59 => "ModuleReset",
        0x5A => "AudibleAlarmControl",
        0x60 => "Present",
        0x61 => "Good",
        0x62 => "InternalFailure",
        0x63 => "VoltageOutOfRange",
        0x64 => "FrequencyOutOfRange",
        0x65 => "Overload",
        0x66 => "OverCharged",
        0x67 => "OverTemperature",
        0x68 => "ShutdownRequested",
        0x69 => "ShutdownImminent",
        0x6B => "SwitchOnOff",
        0x6C => "Switchable",
        0x6D => "Used",
        0x6E => "Boost",
        0x6F => "Buck",
        0x70 => "Initialized",
        0x71 => "Tested",
        0x72 => "AwaitingPower",
        0x73 => "CommunicationLost",
        0xFD => "iManufacturer",
        0xFE => "iProduct",
        0xFF => "iSerialNumber",
        _ => return None,
    })
}

#[allow(clippy::too_many_lines)]
fn battery_system(id: u16) -> Option<&'static str> {
    Some(match id {
        0x01 => "SMBBatteryMode",
        0x02 => "SMBBatteryStatus",
        0x03 => "SMBAlarmWarning",
        0x04 => "SMBChargerMode",
        0x05 => "SMBChargerStatus",
        0x06 => "SMBChargerSpecInfo",
        0x07 => "SMBSelectorState",
        0x08 => "SMBSelectorPresets",
        0x09 => "SMBSelectorInfo",
        0x10 => "OptionalMfgFunction1",
        0x11 => "OptionalMfgFunction2",
        0x12 => "OptionalMfgFunction3",
        0x13 => "OptionalMfgFunction4",
        0x14 => "OptionalMfgFunction5",
        0x15 => "ConnectionToSMBus",
        0x16 => "OutputConnection",
        0x17 => "ChargerConnection",
        0x18 => "BatteryInsertion",
        0x19 => "Usenext",
        0x1A => "OKToUse",
        0x1B => "BatterySupported",
        0x1C => "SelectorRevision",
        0x1D => "ChargingIndicator",
        0x28 => "ManufacturerAccess",
        0x29 => "RemainingCapacityLimit",
        0x2A => "RemainingTimeLimit",
        0x2B => "AtRate",
        0x2C => "CapacityMode",
        0x2D => "BroadcastToCharger",
        0x2E => "PrimaryBattery",
        0x2F => "ChargeController",
        0x40 => "TerminateCharge",
        0x41 => "TerminateDischarge",
        0x42 => "BelowRemainingCapacityLimit",
        0x43 => "RemainingTimeLimitExpired",
        0x44 => "Charging",
        0x45 => "Discharging",
        0x46 => "FullyCharged",
        0x47 => "FullyDischarged",
        0x48 => "ConditioningFlag",
        0x49 => "AtRateOK",
        0x4A => "SMBErrorCode",
        0x4B => "NeedReplacement",
        0x60 => "AtRateTimeToFull",
        0x61 => "AtRateTimeToEmpty",
        0x62 => "AverageCurrent",
        0x63 => "MaxError",
        0x64 => "RelativeStateOfCharge",
        0x65 => "AbsoluteStateOfCharge",
        0x66 => "RemainingCapacity",
        0x67 => "FullChargeCapacity",
        0x68 => "RunTimeToEmpty",
        0x69 => "AverageTimeToEmpty",
        0x6A => "AverageTimeToFull",
        0x6B => "CycleCount",
        0x80 => "BattPackModelLevel",
        0x81 => "InternalChargeController",
        0x82 => "PrimaryBatterySupport",
        0x83 => "DesignCapacity",
        0x84 => "SpecificationInfo",
        0x85 => "ManufacturerDate",
        0x86 => "SerialNumber",
        0x87 => "iManufacturerName",
        0x88 => "iDeviceName",
        0x89 => "iDeviceChemistry",
        0x8A => "ManufacturerData",
        0x8B => "Rechargeable",
        0x8C => "WarningCapacityLimit",
        0x8D => "CapacityGranularity1",
        0x8E => "CapacityGranularity2",
        0x8F => "iOEMInformation",
        0xC0 => "InhibitCharge",
        0xC1 => "EnablePolling",
        0xC2 => "ResetToZero",
        0xD0 => "ACPresent",
        0xD1 => "BatteryPresent",
        0xD2 => "PowerFail",
        0xD3 => "AlarmInhibited",
        0xD4 => "ThermistorUnderRange",
        0xD5 => "ThermistorHot",
        0xD6 => "ThermistorCold",
        0xD7 => "ThermistorOverRange",
        0xD8 => "VoltageOutOfRange",
        0xD9 => "CurrentOutOfRange",
        0xDA => "CurrentNotRegulated",
        0xDB => "VoltageNotRegulated",
        0xDC => "MasterMode",
        0xF0 => "ChargerSelectorSupport",
        0xF1 => "ChargerSpec",
        0xF2 => "Level2",
        0xF3 => "Level3",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_common_ups_usages() {
        assert_eq!(
            usage_name(Usage::new(0x85, 0x66)),
            Some("RemainingCapacity")
        );
        assert_eq!(usage_name(Usage::new(0x84, 0x35)), Some("PercentLoad"));
        assert_eq!(display(Usage::new(0x85, 0xD0)), "BatterySystem.ACPresent");
        assert_eq!(display(Usage::new(0xFFFF, 0x12)), "0xFFFF:0x0012");
    }

    #[test]
    fn builds_nut_style_paths() {
        let path = [Usage::new(0x84, 0x04), Usage::new(0x84, 0x24)];
        assert_eq!(
            nut_path(&path, Usage::new(0x85, 0x66)),
            "UPS.PowerSummary.RemainingCapacity"
        );
        assert_eq!(nut_path(&[], Usage::new(0xFF01, 0x02)), "0xFF01:0x0002");
    }

    #[test]
    fn detects_string_index_usages() {
        assert!(is_string_index(Usage::new(0x84, 0xFE)));
        assert!(is_string_index(Usage::new(0x85, 0x89)));
        assert!(!is_string_index(Usage::new(0x85, 0x66)));
    }
}
