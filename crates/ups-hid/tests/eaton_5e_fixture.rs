//! Regression test against a real capture of the reference UPS
//! (Eaton 5E 900 G2, firmware 01.02.0009, read by `hid-probe` on Windows 11).

use std::collections::HashMap;

use serde_json::Value;
use ups_hid::{ReportDescriptor, ReportKind, usages};

const FIXTURE: &str =
    include_str!("../../../fixtures/hid/eaton-5e900ud-g2-fw01.02.0009-windows.json");

fn unhex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect()
}

struct Capture {
    descriptor: ReportDescriptor,
    /// Feature report ID -> recorded payload.
    features: HashMap<u8, Vec<u8>>,
}

fn load() -> Capture {
    let json: Value = serde_json::from_str(FIXTURE).unwrap();
    assert_eq!(json["serial_redacted"], true, "fixtures must be redacted");
    let descriptor =
        ReportDescriptor::parse(&unhex(json["report_descriptor_hex"].as_str().unwrap())).unwrap();
    let features = json["feature_reports"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| {
            let id = u8::try_from(r["report_id"].as_u64()?).ok()?;
            Some((id, unhex(r["payload_hex"].as_str()?)))
        })
        .collect();
    Capture {
        descriptor,
        features,
    }
}

/// Decodes the field with NUT-style `path` from its recorded feature report.
fn value(c: &Capture, path: &str) -> (i64, u8) {
    let field = c
        .descriptor
        .fields()
        .iter()
        .find(|f| {
            f.kind == ReportKind::Feature
                && usages::nut_path(c.descriptor.collection_path(f), f.usage) == path
        })
        .unwrap_or_else(|| panic!("no feature field {path}"));
    let payload = &c.features[&field.report_id];
    (field.extract(payload).unwrap(), field.report_id)
}

#[test]
fn parses_the_real_descriptor() {
    let c = load();
    assert_eq!(c.descriptor.fields().len(), 337);
    assert_eq!(c.descriptor.collections().len(), 16);
    // 65 values (58 of them on the standard Power Device / Battery System
    // pages, including charge, runtime, load and status) are declared
    // Constant (read-only). Treating them as padding drops exactly these.
    let constant = c.descriptor.fields().iter().filter(|f| f.is_constant());
    assert_eq!(constant.count(), 65);
}

#[test]
fn decodes_battery_and_status() {
    let c = load();
    assert_eq!(value(&c, "UPS.PowerSummary.RemainingCapacity"), (100, 0x06));
    assert_eq!(value(&c, "UPS.PowerSummary.RunTimeToEmpty"), (16_621, 0x06));
    assert_eq!(value(&c, "UPS.PowerSummary.PercentLoad"), (0, 0x07));
    assert_eq!(
        value(&c, "UPS.PowerSummary.RemainingCapacityLimit"),
        (5, 0x08)
    );
    for (flag, expected) in [
        ("ACPresent", 1),
        ("Charging", 1),
        ("Discharging", 0),
        ("BelowRemainingCapacityLimit", 0),
        ("NeedReplacement", 0),
        ("Overload", 0),
        ("ShutdownImminent", 0),
    ] {
        let path = format!("UPS.PowerSummary.PresentStatus.{flag}");
        assert_eq!(value(&c, &path), (expected, 0x01), "{flag}");
    }
}

#[test]
fn decodes_electrical_measurements_with_units() {
    let c = load();
    // Battery voltage: 136 × 10^-1 V (unit exponent 6 with CGS volts).
    let battery = c
        .descriptor
        .fields()
        .iter()
        .find(|f| {
            usages::nut_path(c.descriptor.collection_path(f), f.usage) == "UPS.PowerSummary.Voltage"
        })
        .unwrap();
    assert_eq!(
        ups_hid::units::normalized_exponent(battery.unit, battery.unit_exponent),
        Some(-1)
    );
    assert_eq!(value(&c, "UPS.PowerSummary.Voltage").0, 136);

    assert_eq!(value(&c, "UPS.PowerConverter.Input.Voltage"), (218, 0x0E));
    assert_eq!(value(&c, "UPS.PowerConverter.Output.Voltage"), (216, 0x0E));
    assert_eq!(
        value(&c, "UPS.PowerConverter.Output.Frequency"),
        (500, 0x0E)
    ); // 50.0 Hz
    assert_eq!(value(&c, "UPS.Flow.ConfigApparentPower"), (900, 0x0D));
    assert_eq!(value(&c, "UPS.Flow.ConfigActivePower"), (480, 0x0D));
    assert_eq!(value(&c, "UPS.Flow.ConfigVoltage"), (230, 0x13));
}
