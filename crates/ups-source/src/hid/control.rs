//! Allowlisted UPS settings (ADR-0003). This is the only code that writes to
//! the device.
//!
//! Every write: checks the setting is on the allowlist and the field is
//! writable (not Constant), validates the value, reads the report fresh,
//! changes only the target field's bits, sends it, then reads the report back
//! and verifies the new value. Anything else is refused.

use std::collections::HashMap;

use ups_hid::{ReportDescriptor, ReportKind};

use super::mapping::{Mapping, Target};
use super::readonly::ReadOnlyHid;
use crate::{Setting, SourceError};

/// HID `AudibleAlarmControl` values (HID Power Device spec; NUT `beeper.*`).
pub const BEEPER_DISABLED: i64 = 1;
pub const BEEPER_ENABLED: i64 = 2;

/// The allowlist: which HID field a setting may write, and with which value.
fn target_and_value(setting: Setting) -> (Target, i64) {
    match setting {
        Setting::Beeper { enabled } => (
            Target::Beeper,
            if enabled {
                BEEPER_ENABLED
            } else {
                BEEPER_DISABLED
            },
        ),
    }
}

/// Whether the device supports `setting` (field present and writable).
pub fn supports(descriptor: &ReportDescriptor, mapping: &Mapping, setting: Setting) -> bool {
    let (target, _) = target_and_value(setting);
    mapping
        .bindings()
        .iter()
        .find(|b| b.target == target)
        .and_then(|b| descriptor.fields().get(b.field))
        .is_some_and(|f| f.kind == ReportKind::Feature && !f.is_constant())
}

/// Applies `setting` and verifies it by reading the report back. On success the
/// verified payload is stored in `reports`.
pub fn apply(
    device: &ReadOnlyHid,
    descriptor: &ReportDescriptor,
    mapping: &Mapping,
    reports: &mut HashMap<u8, Vec<u8>>,
    setting: Setting,
) -> Result<(), SourceError> {
    let (target, value) = target_and_value(setting);
    if !supports(descriptor, mapping, setting) {
        return Err(SourceError::Unsupported(format!(
            "this UPS does not offer a writable {target:?} setting"
        )));
    }
    let binding = mapping
        .bindings()
        .iter()
        .find(|b| b.target == target)
        .ok_or_else(|| SourceError::Unsupported(format!("{target:?} not mapped")))?;
    let field = &descriptor.fields()[binding.field];
    let id = binding.report_id;
    let len = descriptor.report_len(ReportKind::Feature, id);
    let buf_len = (descriptor.max_report_len(ReportKind::Feature) + 1).max(64);

    let read = |device: &ReadOnlyHid| -> Result<Vec<u8>, SourceError> {
        let mut buf = vec![0u8; buf_len];
        buf[0] = id;
        let n = device
            .feature_report(&mut buf)
            .map_err(|e| SourceError::Io(e.to_string()))?;
        let payload = buf.get(1..n).unwrap_or_default();
        Ok(payload[..payload.len().min(len)].to_vec())
    };

    // Fresh read so other fields in the same report keep their current values.
    let mut payload = read(device)?;
    if payload.len() < len {
        return Err(SourceError::Io(format!(
            "feature report {id:#04x} too short ({} of {len} bytes)",
            payload.len()
        )));
    }
    if !field.insert(&mut payload, value) {
        return Err(SourceError::Unsupported(format!(
            "value {value} is out of range for {target:?}"
        )));
    }
    let mut out = Vec::with_capacity(len + 1);
    out.push(id);
    out.extend_from_slice(&payload);
    device
        .send_feature_report(&out)
        .map_err(|e| SourceError::Io(format!("writing setting failed: {e}")))?;

    // Verify.
    let check = read(device)?;
    match field.extract(&check) {
        Some(v) if v == value => {
            reports.insert(id, check);
            Ok(())
        }
        other => Err(SourceError::Io(format!(
            "the UPS did not accept the setting (reads back {other:?}, expected {value})"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str =
        include_str!("../../../../fixtures/hid/eaton-5e900ud-g2-fw01.02.0009-windows.json");

    #[test]
    fn beeper_is_writable_on_the_5e() {
        let json: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let bytes: Vec<u8> = json["report_descriptor_hex"]
            .as_str()
            .unwrap()
            .split_whitespace()
            .map(|b| u8::from_str_radix(b, 16).unwrap())
            .collect();
        let d = ReportDescriptor::parse(&bytes).unwrap();
        let m = Mapping::build(&d);
        assert!(supports(&d, &m, Setting::Beeper { enabled: true }));
        assert_eq!(
            target_and_value(Setting::Beeper { enabled: false }),
            (Target::Beeper, BEEPER_DISABLED)
        );
    }
}
