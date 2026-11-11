//! The HID → NUT mapping, checked against the real Eaton 5E 900 G2 capture.

use std::collections::HashMap;

use chrono::Utc;
use serde_json::Value;
use ups_hid::ReportDescriptor;
use ups_model::{UpsStatus, vars};
use ups_source::hid::{Mapping, Rate, Target};

const FIXTURE: &str =
    include_str!("../../../fixtures/hid/eaton-5e900ud-g2-fw01.02.0009-windows.json");

fn unhex(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|b| u8::from_str_radix(b, 16).unwrap())
        .collect()
}

fn load() -> (ReportDescriptor, HashMap<u8, Vec<u8>>) {
    let json: Value = serde_json::from_str(FIXTURE).unwrap();
    let descriptor =
        ReportDescriptor::parse(&unhex(json["report_descriptor_hex"].as_str().unwrap())).unwrap();
    let reports = json["feature_reports"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| {
            let id = u8::try_from(r["report_id"].as_u64()?).ok()?;
            Some((id, unhex(r["payload_hex"].as_str()?)))
        })
        .collect();
    (descriptor, reports)
}

#[test]
fn polls_only_five_live_reports() {
    let (d, _) = load();
    let m = Mapping::build(&d);
    assert!(m.is_usable());
    assert_eq!(m.reports(Rate::Live), [0x01, 0x02, 0x06, 0x07, 0x0E]);
    assert_eq!(m.reports(Rate::Slow), [0x08, 0x0D, 0x13, 0x14]);
    assert_eq!(m.reports(Rate::Once), [0x10]);
    // Not present on this model.
    assert!(!m.has(Target::InputFrequency));
}

#[test]
fn builds_the_expected_snapshot() {
    let (d, reports) = load();
    let m = Mapping::build(&d);
    let s = m.snapshot(&d, &reports, Utc::now());

    assert_eq!(s.status, Some(UpsStatus::OL | UpsStatus::CHRG));
    assert_eq!(s.battery_charge_pct, Some(100.0));
    assert_eq!(s.battery_runtime_s, Some(16_621));
    assert_eq!(s.load_pct, Some(0.0));
    assert!((s.battery_voltage_v.unwrap() - 13.6).abs() < 1e-4);
    assert_eq!(s.input_voltage_v, Some(218.0));
    assert_eq!(s.output_voltage_v, Some(216.0));
    assert!((s.output_frequency_hz.unwrap() - 50.0).abs() < 1e-4);
    assert_eq!(s.realpower_w, Some(0.0));
    assert_eq!(s.power_va, Some(0.0));
    assert_eq!(s.realpower_nominal_w, Some(480.0));
    assert_eq!(s.power_nominal_va, Some(900.0));
    assert_eq!(
        s.extra.get(vars::BATTERY_CHARGE_LOW).map(String::as_str),
        Some("5")
    );
    assert_eq!(
        s.extra.get("output.voltage.nominal").map(String::as_str),
        Some("230")
    );
    assert_eq!(
        s.extra.get(vars::UPS_BEEPER_STATUS).map(String::as_str),
        Some("disabled")
    );
}

#[test]
fn finds_string_indices() {
    let (d, reports) = load();
    let m = Mapping::build(&d);
    let mut idx = m.string_indices(&d, &reports);
    idx.sort_by_key(|(_, i)| *i);
    // iManufacturer=1, iProduct=2, iSerialNumber=4, iDeviceChemistry=5. The
    // fixture keeps the *index*; the serial string itself was never captured.
    assert_eq!(
        idx,
        [
            (Target::Manufacturer, 1),
            (Target::Product, 2),
            (Target::Serial, 4),
            (Target::Chemistry, 5)
        ]
    );
}
