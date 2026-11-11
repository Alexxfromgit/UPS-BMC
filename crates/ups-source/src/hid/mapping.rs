//! Maps HID Power Device fields to the NUT vocabulary.
//!
//! The mapping is built from the report descriptor alone, by NUT-style usage
//! path (`UPS.PowerSummary.RemainingCapacity`). It works for any HID UPS that
//! uses the standard usages, with candidate paths taken from NUT's `mge-hid`
//! and generic tables. Building the [`Snapshot`] is a pure function of the
//! descriptor and the cached report payloads, so it is tested against real
//! captures without hardware.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use ups_hid::{Field, ReportDescriptor, ReportKind, units, usages};
use ups_model::{Snapshot, UpsStatus, vars};

/// How often a value needs refreshing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rate {
    /// Every poll (status and measurements).
    Live,
    /// Every minute or so (ratings and configuration).
    Slow,
    /// Once per connection (string descriptor indices).
    Once,
}

/// A NUT-level quantity that a HID field can provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    AcPresent,
    Charging,
    Discharging,
    LowBattery,
    ReplaceBattery,
    Overload,
    ShutdownImminent,
    OutputGood,
    Boost,
    Buck,
    Charge,
    Runtime,
    BatteryVoltage,
    Load,
    InputVoltage,
    InputFrequency,
    OutputVoltage,
    OutputFrequency,
    RealPower,
    ApparentPower,
    NominalRealPower,
    NominalApparentPower,
    NominalOutputVoltage,
    LowBatteryLimit,
    Beeper,
    Manufacturer,
    Product,
    Chemistry,
    Serial,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Flag,
    Number,
    StringIndex,
}

const TARGETS: [Target; 29] = [
    Target::AcPresent,
    Target::Charging,
    Target::Discharging,
    Target::LowBattery,
    Target::ReplaceBattery,
    Target::Overload,
    Target::ShutdownImminent,
    Target::OutputGood,
    Target::Boost,
    Target::Buck,
    Target::Charge,
    Target::Runtime,
    Target::BatteryVoltage,
    Target::Load,
    Target::InputVoltage,
    Target::InputFrequency,
    Target::OutputVoltage,
    Target::OutputFrequency,
    Target::RealPower,
    Target::ApparentPower,
    Target::NominalRealPower,
    Target::NominalApparentPower,
    Target::NominalOutputVoltage,
    Target::LowBatteryLimit,
    Target::Beeper,
    Target::Manufacturer,
    Target::Product,
    Target::Chemistry,
    Target::Serial,
];

impl Target {
    /// Candidate NUT-style paths, most specific first.
    fn paths(self) -> &'static [&'static str] {
        match self {
            Self::AcPresent => &["UPS.PowerSummary.PresentStatus.ACPresent"],
            Self::Charging => &["UPS.PowerSummary.PresentStatus.Charging"],
            Self::Discharging => &["UPS.PowerSummary.PresentStatus.Discharging"],
            Self::LowBattery => &["UPS.PowerSummary.PresentStatus.BelowRemainingCapacityLimit"],
            Self::ReplaceBattery => &["UPS.PowerSummary.PresentStatus.NeedReplacement"],
            Self::Overload => &["UPS.PowerSummary.PresentStatus.Overload"],
            Self::ShutdownImminent => &["UPS.PowerSummary.PresentStatus.ShutdownImminent"],
            Self::OutputGood => &["UPS.PowerSummary.PresentStatus.Good"],
            Self::Boost => &["UPS.PowerConverter.Input.PresentStatus.Boost"],
            Self::Buck => &["UPS.PowerConverter.Input.PresentStatus.Buck"],
            Self::Charge => &["UPS.PowerSummary.RemainingCapacity"],
            Self::Runtime => &["UPS.PowerSummary.RunTimeToEmpty"],
            Self::BatteryVoltage => &[
                "UPS.PowerSummary.Voltage",
                "UPS.BatterySystem.Battery.Voltage",
            ],
            Self::Load => &[
                "UPS.PowerSummary.PercentLoad",
                "UPS.PowerConverter.Output.PercentLoad",
            ],
            Self::InputVoltage => &["UPS.PowerConverter.Input.Voltage", "UPS.Input.Voltage"],
            Self::InputFrequency => &["UPS.PowerConverter.Input.Frequency", "UPS.Input.Frequency"],
            Self::OutputVoltage => &["UPS.PowerConverter.Output.Voltage", "UPS.Output.Voltage"],
            Self::OutputFrequency => &[
                "UPS.PowerConverter.Output.Frequency",
                "UPS.Output.Frequency",
            ],
            Self::RealPower => &["UPS.PowerConverter.Output.ActivePower"],
            Self::ApparentPower => &["UPS.PowerConverter.Output.ApparentPower"],
            Self::NominalRealPower => &["UPS.Flow.ConfigActivePower"],
            Self::NominalApparentPower => &["UPS.Flow.ConfigApparentPower"],
            Self::NominalOutputVoltage => &["UPS.Flow.ConfigVoltage"],
            Self::LowBatteryLimit => &["UPS.PowerSummary.RemainingCapacityLimit"],
            Self::Beeper => &["UPS.PowerSummary.AudibleAlarmControl"],
            Self::Manufacturer => &["UPS.PowerSummary.iManufacturer"],
            Self::Product => &["UPS.PowerSummary.iProduct"],
            Self::Chemistry => &["UPS.PowerSummary.iDeviceChemistry"],
            Self::Serial => &["UPS.PowerSummary.iSerialNumber"],
        }
    }

    fn shape(self) -> Shape {
        match self {
            Self::AcPresent
            | Self::Charging
            | Self::Discharging
            | Self::LowBattery
            | Self::ReplaceBattery
            | Self::Overload
            | Self::ShutdownImminent
            | Self::OutputGood
            | Self::Boost
            | Self::Buck => Shape::Flag,
            Self::Manufacturer | Self::Product | Self::Chemistry | Self::Serial => {
                Shape::StringIndex
            }
            _ => Shape::Number,
        }
    }

    /// How often the value must be refreshed.
    #[must_use]
    pub fn rate(self) -> Rate {
        match self {
            Self::NominalRealPower
            | Self::NominalApparentPower
            | Self::NominalOutputVoltage
            | Self::LowBatteryLimit
            | Self::Beeper => Rate::Slow,
            _ if self.shape() == Shape::StringIndex => Rate::Once,
            _ => Rate::Live,
        }
    }
}

/// One mapped field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub target: Target,
    /// Index into [`ReportDescriptor::fields`].
    pub field: usize,
    pub report_id: u8,
}

/// The fields of one device that UPS-BMC understands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Mapping {
    bindings: Vec<Binding>,
}

impl Mapping {
    /// Finds every known target among the descriptor's feature fields.
    #[must_use]
    pub fn build(descriptor: &ReportDescriptor) -> Self {
        let mut by_path: HashMap<String, usize> = HashMap::new();
        for (index, field) in descriptor.fields().iter().enumerate() {
            if field.kind == ReportKind::Feature {
                let path = usages::nut_path(descriptor.collection_path(field), field.usage);
                by_path.entry(path).or_insert(index);
            }
        }
        let bindings = TARGETS
            .iter()
            .filter_map(|&target| {
                let field = target.paths().iter().find_map(|p| by_path.get(*p))?;
                Some(Binding {
                    target,
                    field: *field,
                    report_id: descriptor.fields()[*field].report_id,
                })
            })
            .collect();
        Self { bindings }
    }

    #[must_use]
    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    #[must_use]
    pub fn has(&self, target: Target) -> bool {
        self.bindings.iter().any(|b| b.target == target)
    }

    /// A device is usable if it reports at least the power source or the charge.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.has(Target::AcPresent) || self.has(Target::Charge)
    }

    /// Feature report IDs needed for values of the given rate, ascending.
    #[must_use]
    pub fn reports(&self, rate: Rate) -> Vec<u8> {
        let mut ids: Vec<u8> = self
            .bindings
            .iter()
            .filter(|b| b.target.rate() == rate)
            .map(|b| b.report_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// String descriptor indices for manufacturer, product, chemistry and serial.
    #[must_use]
    pub fn string_indices(
        &self,
        descriptor: &ReportDescriptor,
        reports: &HashMap<u8, Vec<u8>>,
    ) -> Vec<(Target, i32)> {
        self.bindings
            .iter()
            .filter(|b| b.target.shape() == Shape::StringIndex)
            .filter_map(|b| {
                let raw = extract(descriptor, b, reports)?;
                let index = i32::try_from(raw).ok().filter(|i| *i > 0)?;
                Some((b.target, index))
            })
            .collect()
    }

    /// Builds a snapshot from the cached report payloads.
    #[must_use]
    #[allow(clippy::cast_possible_truncation)] // values come from ≤ 32-bit fields
    pub fn snapshot(
        &self,
        descriptor: &ReportDescriptor,
        reports: &HashMap<u8, Vec<u8>>,
        ts: DateTime<Utc>,
    ) -> Snapshot {
        let mut flags: HashMap<Target, bool> = HashMap::new();
        let mut numbers: HashMap<Target, f64> = HashMap::new();
        for b in &self.bindings {
            let Some(raw) = extract(descriptor, b, reports) else {
                continue;
            };
            match b.target.shape() {
                Shape::Flag => {
                    flags.insert(b.target, raw != 0);
                }
                Shape::Number => {
                    let field = &descriptor.fields()[b.field];
                    numbers.insert(b.target, physical(field, raw));
                }
                Shape::StringIndex => {}
            }
        }

        let mut s = Snapshot::empty(ts);
        s.status = status(&flags);
        let num = |t| numbers.get(&t).copied();
        s.battery_charge_pct = num(Target::Charge).map(to_f32);
        s.battery_runtime_s = num(Target::Runtime)
            .filter(|v| *v >= 0.0)
            .map(|v| u32::try_from(v.round() as i64).unwrap_or(u32::MAX));
        s.battery_voltage_v = num(Target::BatteryVoltage).map(to_f32);
        s.load_pct = num(Target::Load).map(to_f32);
        s.input_voltage_v = num(Target::InputVoltage).map(to_f32);
        s.input_frequency_hz = num(Target::InputFrequency).map(to_f32);
        s.output_voltage_v = num(Target::OutputVoltage).map(to_f32);
        s.output_frequency_hz = num(Target::OutputFrequency).map(to_f32);
        s.realpower_w = num(Target::RealPower).map(to_f32);
        s.power_va = num(Target::ApparentPower).map(to_f32);
        s.realpower_nominal_w = num(Target::NominalRealPower).map(to_f32);
        s.power_nominal_va = num(Target::NominalApparentPower).map(to_f32);

        if let Some(v) = num(Target::LowBatteryLimit) {
            s.extra
                .insert(vars::BATTERY_CHARGE_LOW.into(), format!("{v}"));
        }
        if let Some(v) = num(Target::NominalOutputVoltage) {
            s.extra
                .insert("output.voltage.nominal".into(), format!("{v}"));
        }
        if let Some(v) = num(Target::Beeper) {
            // HID Power Device AudibleAlarmControl, as mapped by NUT.
            let text = match v.round() as i64 {
                1 => "disabled",
                2 => "enabled",
                3 => "muted",
                _ => "unknown",
            };
            s.extra.insert(vars::UPS_BEEPER_STATUS.into(), text.into());
        }
        s
    }
}

fn extract(
    descriptor: &ReportDescriptor,
    binding: &Binding,
    reports: &HashMap<u8, Vec<u8>>,
) -> Option<i64> {
    let payload = reports.get(&binding.report_id)?;
    descriptor.fields().get(binding.field)?.extract(payload)
}

/// Logical value → physical value in the everyday unit (V, W, Hz, s, %).
fn physical(field: &Field, raw: i64) -> f64 {
    let value = field.scale(raw);
    match units::normalized_exponent(field.unit, field.unit_exponent) {
        Some(exp) if exp != 0 => value * 10f64.powi(i32::from(exp)),
        _ => value,
    }
}

#[allow(clippy::cast_possible_truncation)] // UPS measurements fit easily in f32
fn to_f32(v: f64) -> f32 {
    v as f32
}

/// Combines the HID status flags into NUT `ups.status`, like `usbhid-ups`.
fn status(flags: &HashMap<Target, bool>) -> Option<UpsStatus> {
    if flags.is_empty() {
        return None;
    }
    let on = |t| flags.get(&t).copied();
    let mut s = UpsStatus::empty();
    match (on(Target::AcPresent), on(Target::Discharging)) {
        // Without an ACPresent usage, not discharging means mains power.
        (Some(true), _) | (None, Some(false)) => s |= UpsStatus::OL,
        (Some(false), _) | (None, Some(true)) => s |= UpsStatus::OB,
        (None, None) => {}
    }
    let mut set = |t, flag| {
        if on(t) == Some(true) {
            s |= flag;
        }
    };
    set(Target::Discharging, UpsStatus::DISCHRG);
    set(Target::Charging, UpsStatus::CHRG);
    set(Target::LowBattery, UpsStatus::LB);
    set(Target::ShutdownImminent, UpsStatus::LB);
    set(Target::ReplaceBattery, UpsStatus::RB);
    set(Target::Overload, UpsStatus::OVER);
    set(Target::Boost, UpsStatus::BOOST);
    set(Target::Buck, UpsStatus::TRIM);
    if on(Target::OutputGood) == Some(false) {
        s |= UpsStatus::OFF;
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags(pairs: &[(Target, bool)]) -> HashMap<Target, bool> {
        pairs.iter().copied().collect()
    }

    #[test]
    fn combines_status_flags_like_nut() {
        use Target::*;
        assert_eq!(status(&HashMap::new()), None);
        assert_eq!(
            status(&flags(&[
                (AcPresent, true),
                (Charging, true),
                (OutputGood, true)
            ])),
            Some(UpsStatus::OL | UpsStatus::CHRG)
        );
        assert_eq!(
            status(&flags(&[
                (AcPresent, false),
                (Discharging, true),
                (LowBattery, true)
            ])),
            Some(UpsStatus::OB | UpsStatus::DISCHRG | UpsStatus::LB)
        );
        // No ACPresent usage: discharging implies on battery.
        assert_eq!(
            status(&flags(&[(Discharging, true)])),
            Some(UpsStatus::OB | UpsStatus::DISCHRG)
        );
        // ... and not discharging implies mains power (review M1-8).
        assert_eq!(
            status(&flags(&[(Discharging, false), (Charging, true)])),
            Some(UpsStatus::OL | UpsStatus::CHRG)
        );
        assert_eq!(
            status(&flags(&[
                (AcPresent, true),
                (Buck, true),
                (OutputGood, false)
            ])),
            Some(UpsStatus::OL | UpsStatus::TRIM | UpsStatus::OFF)
        );
    }

    #[test]
    fn every_target_has_paths() {
        for t in TARGETS {
            assert!(!t.paths().is_empty(), "{t:?}");
        }
    }
}
