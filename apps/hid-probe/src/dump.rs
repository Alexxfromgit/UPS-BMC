//! The JSON dump format and the (pure, testable) decoding of reports into it.

use chrono::{DateTime, Utc};
use serde::Serialize;
use ups_hid::{Field, ReportDescriptor, ReportKind, units, usages};

/// Where `report_descriptor_hex` came from. On Windows, hidapi *rebuilds* a
/// descriptor from the HIDP preparsed data of one top-level collection. It is
/// logically equivalent to, but not byte-identical with, the device's
/// descriptor (see ADR-0001).
pub const DESCRIPTOR_SOURCE: &str = if cfg!(windows) {
    "windows-reconstructed-from-preparsed-data"
} else if cfg!(target_os = "linux") {
    "linux-hidraw"
} else if cfg!(target_os = "macos") {
    "macos-iokit"
} else {
    "unknown"
};

#[derive(Debug, Serialize)]
pub struct DeviceDump {
    pub tool: &'static str,
    pub tool_version: &'static str,
    pub captured_at: DateTime<Utc>,
    pub os: &'static str,
    pub descriptor_source: &'static str,
    pub serial_redacted: bool,
    pub info: InfoDump,
    pub open_error: Option<String>,
    pub report_descriptor_hex: Option<String>,
    pub descriptor_error: Option<String>,
    /// Parsed descriptor: fields, collections, array usage lists and report lengths.
    pub descriptor: Option<ReportDescriptor>,
    pub feature_reports: Vec<ReportDump>,
    pub input_reports: Vec<ReportDump>,
    /// Set while listening for input reports; cleared when done.
    pub incomplete: bool,
    /// Average milliseconds for one sweep over all feature reports.
    pub feature_cycle_ms: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct InfoDump {
    pub vendor_id: u16,
    pub product_id: u16,
    pub release_number: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    pub usage_page: u16,
    pub usage: u16,
    pub interface_number: i32,
    pub path: String,
}

#[derive(Debug, Serialize)]
pub struct ReportDump {
    pub kind: ReportKind,
    pub report_id: u8,
    /// Milliseconds since listening started (input reports only).
    pub at_ms: Option<u64>,
    pub error: Option<String>,
    /// Payload (report ID byte excluded), trimmed to `expected_len`.
    pub payload_hex: Option<String>,
    /// Payload length according to the descriptor.
    pub expected_len: usize,
    /// Payload length the OS actually returned (can differ, see R15).
    pub received_len: Option<usize>,
    pub values: Vec<FieldValue>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct FieldValue {
    /// NUT-style path, e.g. `UPS.PowerSummary.RemainingCapacity`.
    pub name: String,
    pub page: u16,
    pub id: u16,
    pub bit_offset: u32,
    pub bit_size: u32,
    pub array: bool,
    pub raw: Option<i64>,
    pub scaled: Option<f64>,
    /// For array fields: the usage the raw selector picks.
    pub selected: Option<String>,
    pub unit: Option<&'static str>,
    pub unit_exponent: i8,
    /// Exponent relative to the everyday unit (V, W, s, ...).
    pub normalized_exponent: Option<i8>,
    pub string: Option<String>,
    pub string_error: Option<String>,
    /// The value was withheld because it identifies the device (`--redact-serial`).
    pub redacted: bool,
}

/// Resolves a USB string descriptor index (iProduct, iSerialNumber, ...).
pub type StringLookup<'a> = dyn FnMut(i32) -> Result<Option<String>, String> + 'a;

/// Decodes `fields` from a report payload (report ID byte excluded).
pub fn decode<'a>(
    descriptor: &ReportDescriptor,
    fields: impl Iterator<Item = &'a Field>,
    payload: &[u8],
    lookup: &mut StringLookup<'_>,
    redact_serial: bool,
) -> Vec<FieldValue> {
    fields
        .map(|f| {
            let name = usages::nut_path(descriptor.collection_path(f), f.usage);
            let redacted = redact_serial && usages::is_serial_number(f.usage);
            let raw = if redacted { None } else { f.extract(payload) };

            let (mut string, mut string_error) = (None, None);
            if let Some(index) = raw.filter(|i| *i > 0 && usages::is_string_index(f.usage)) {
                match i32::try_from(index)
                    .map_err(|e| e.to_string())
                    .and_then(&mut *lookup)
                {
                    Ok(s) => string = s,
                    Err(e) => string_error = Some(e),
                }
            }

            let selected = if f.is_array() {
                raw.and_then(|r| descriptor.array_selection(f, r))
                    .map(usages::display)
            } else {
                None
            };

            FieldValue {
                name,
                page: f.usage.page,
                id: f.usage.id,
                bit_offset: f.bit_offset,
                bit_size: f.bit_size,
                array: f.is_array(),
                raw,
                scaled: raw.map(|r| f.scale(r)),
                selected,
                unit: units::symbol(f.unit),
                unit_exponent: f.unit_exponent,
                normalized_exponent: units::normalized_exponent(f.unit, f.unit_exponent),
                string,
                string_error,
                redacted,
            }
        })
        .collect()
}

/// Space-separated lowercase hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UPS.PowerSummary with iProduct (0x84:0xFE), iSerialNumber (0x84:0xFF)
    /// and Battery System `SerialNumber` (0x85:0x86), all in feature report 1.
    #[rustfmt::skip]
    const DESC: &[u8] = &[
        0x05, 0x84, 0x09, 0x04, 0xA1, 0x01,
        0x09, 0x24, 0xA1, 0x02,
        0x85, 0x01, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x01,
        0x09, 0xFE, 0xB1, 0x02,
        0x09, 0xFF, 0xB1, 0x02,
        0x05, 0x85, 0x09, 0x86, 0xB1, 0x02,
        0xC0, 0xC0,
    ];

    fn run(redact: bool, lookup: &mut StringLookup<'_>) -> Vec<FieldValue> {
        let d = ReportDescriptor::parse(DESC).unwrap();
        decode(
            &d,
            d.fields_in(ReportKind::Feature, 1),
            &[2, 3, 0x42],
            lookup,
            redact,
        )
    }

    #[test]
    fn resolves_strings_and_nut_names() {
        let mut lookup = |i: i32| Ok(Some(format!("string{i}")));
        let values = run(false, &mut lookup);
        assert_eq!(values[0].name, "UPS.PowerSummary.iProduct");
        assert_eq!(values[0].string.as_deref(), Some("string2"));
        assert_eq!(values[1].string.as_deref(), Some("string3"));
        assert_eq!(values[2].raw, Some(0x42));
    }

    #[test]
    fn redaction_withholds_every_serial_number() {
        let mut asked = Vec::new();
        let mut lookup = |i: i32| {
            asked.push(i);
            Ok(Some(
                if i == 3 { "G123A45678" } else { "5E 900" }.to_owned(),
            ))
        };
        let values = run(true, &mut lookup);
        assert_eq!(asked, [2], "iSerialNumber must not even be looked up");

        let serial = &values[1];
        assert!(serial.redacted);
        assert_eq!((serial.raw, &serial.string), (None, &None));
        let battery_serial = &values[2];
        assert!(battery_serial.redacted);
        assert_eq!(battery_serial.raw, None);

        let json = serde_json::to_string(&values).unwrap();
        assert!(!json.contains("G123A45678"));
    }

    #[test]
    fn records_string_lookup_errors() {
        let mut lookup = |_: i32| Err("not supported by hidraw".to_owned());
        let values = run(false, &mut lookup);
        assert_eq!(values[0].string, None);
        assert_eq!(
            values[0].string_error.as_deref(),
            Some("not supported by hidraw")
        );
    }

    #[test]
    fn hex_is_space_separated() {
        assert_eq!(hex(&[0x00, 0xAB, 0x10]), "00 ab 10");
        assert_eq!(hex(&[]), "");
    }
}
