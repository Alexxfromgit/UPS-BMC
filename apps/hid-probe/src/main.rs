//! `hid-probe`: read-only discovery for HID UPS devices.
//!
//! It lists HID collections. Then, for each UPS (usage page 0x84/0x85, or the
//! `--vid/--pid` you choose), it:
//!   1. reads the HID report descriptor and parses it,
//!   2. reads every *feature* report the descriptor declares (`GET_FEATURE`),
//!   3. optionally listens to *input* reports for a few seconds,
//!   4. prints a decoded table and writes everything to a JSON file.
//!
//! It never writes to the device. All access goes through
//! [`readonly::ReadOnlyHid`], and `clippy.toml` denies every other path.

mod dump;
mod readonly;

use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Utc;
use clap::Parser;
use hidapi::{DeviceInfo, HidApi};
use ups_hid::{ReportDescriptor, ReportKind, usages};

use crate::dump::{DESCRIPTOR_SOURCE, DeviceDump, InfoDump, ReportDump, decode, hex};
use crate::readonly::ReadOnlyHid;

/// Size of hidapi's report descriptor buffer (`HID_API_MAX_REPORT_DESCRIPTOR_SIZE`).
const DESCRIPTOR_BUF: usize = 4096;
/// Minimum report buffer. Windows requires at least the collection's longest
/// report length, and a larger buffer is harmless.
const MIN_REPORT_BUF: usize = 64;

/// Read-only HID UPS discovery tool (part of UPS-BMC).
#[derive(Debug, Parser)]
#[command(version, about)]
#[allow(clippy::struct_excessive_bools)] // plain CLI switches
struct Args {
    /// Only probe devices with this USB vendor ID (hex, e.g. 0463).
    #[arg(long, value_parser = parse_hex_u16)]
    vid: Option<u16>,
    /// Only probe devices with this USB product ID (hex, e.g. ffff).
    #[arg(long, value_parser = parse_hex_u16)]
    pid: Option<u16>,
    /// List every HID collection on the system, not only power devices.
    #[arg(long)]
    list_all: bool,
    /// Listen for input (interrupt) reports for this many seconds.
    #[arg(long, default_value_t = 5)]
    listen: u64,
    /// Directory for JSON dumps.
    #[arg(long, default_value = "probe-output")]
    out: PathBuf,
    /// Withhold serial numbers (USB serial, iSerialNumber, `SerialNumber`) from
    /// the console output and the JSON. Use this for dumps you commit.
    #[arg(long)]
    redact_serial: bool,
}

fn parse_hex_u16(s: &str) -> Result<u16, String> {
    let digits = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(format!("`{s}` is not a hex number (e.g. 0463)"));
    }
    u16::from_str_radix(digits, 16).map_err(|e| format!("`{s}`: {e}"))
}

/// A device is probed when it matches `--vid/--pid`, or, without those, when
/// it is a power device collection.
fn is_target(
    vid: u16,
    pid: u16,
    usage_page: u16,
    vendor_filter: Option<u16>,
    product_filter: Option<u16>,
) -> bool {
    if vendor_filter.is_none() && product_filter.is_none() {
        return usages::is_power_page(usage_page);
    }
    vendor_filter.is_none_or(|v| v == vid) && product_filter.is_none_or(|p| p == pid)
}

fn main() -> Result<()> {
    let args = Args::parse();
    let api = HidApi::new().context("initialising hidapi")?;

    let devices: Vec<&DeviceInfo> = api.device_list().collect();
    print_device_list(&devices, args.list_all);

    let targets: Vec<&DeviceInfo> = devices
        .iter()
        .copied()
        .filter(|d| {
            is_target(
                d.vendor_id(),
                d.product_id(),
                d.usage_page(),
                args.vid,
                args.pid,
            )
        })
        .collect();

    if targets.is_empty() {
        println!("\nNo matching UPS found.");
        println!("- Is the UPS's USB cable connected to this computer?");
        println!("- Windows: is the UPS listed under Batteries in Device Manager?");
        println!("- Linux: do you have read access to /dev/hidraw* (udev rule)?");
        println!("- Try `hid-probe --list-all`, then pass --vid/--pid explicitly.");
        return Ok(());
    }

    fs::create_dir_all(&args.out).with_context(|| format!("creating {}", args.out.display()))?;

    let mut failures = 0;
    for (index, info) in targets.iter().enumerate() {
        if let Err(e) = probe_and_save(&api, info, index, &args) {
            failures += 1;
            eprintln!("\nERROR: {e:#}");
        }
    }
    if failures > 0 {
        anyhow::bail!(
            "{failures} of {} device(s) could not be saved",
            targets.len()
        );
    }
    Ok(())
}

fn print_device_list(devices: &[&DeviceInfo], all: bool) {
    println!(
        "HID collections{}:",
        if all {
            ""
        } else {
            " (power devices only; --list-all for every one)"
        }
    );
    let mut shown = 0;
    for d in devices {
        let power = usages::is_power_page(d.usage_page());
        if !all && !power {
            continue;
        }
        shown += 1;
        println!(
            "  {} {:04x}:{:04x}  page 0x{:04X} ({}) usage 0x{:04X}  if {}  {} / {}",
            if power { "*" } else { " " },
            d.vendor_id(),
            d.product_id(),
            d.usage_page(),
            usages::page_name(d.usage_page()),
            d.usage(),
            d.interface_number(),
            d.manufacturer_string().unwrap_or("?"),
            d.product_string().unwrap_or("?"),
        );
    }
    if shown == 0 {
        println!("  (none)");
    }
}

/// Probes one collection. The dump is saved as soon as the descriptor and the
/// feature reports have been read, then saved again after listening, so an
/// interrupted `--listen` keeps everything read so far.
fn probe_and_save(api: &HidApi, info: &DeviceInfo, index: usize, args: &Args) -> Result<()> {
    let mut dump = new_dump(info, args.redact_serial);
    let path = args.out.join(dump_file_name(&dump, index));

    let device = match ReadOnlyHid::open(api, info.path()) {
        Ok(device) => Some(device),
        Err(e) => {
            dump.open_error = Some(e.to_string());
            None
        }
    };

    if let Some(device) = &device {
        read_descriptor_and_features(device, &mut dump, args.redact_serial);
    }
    let listen = device.is_some() && dump.descriptor.is_some() && args.listen > 0;
    dump.incomplete = listen;
    print_dump(&dump);
    save(&dump, &path, true)?;

    if let (true, Some(device)) = (listen, &device) {
        listen_inputs(
            device,
            &mut dump,
            Duration::from_secs(args.listen),
            args.redact_serial,
        );
        dump.incomplete = false;
        print_reports(&dump.input_reports);
        save(&dump, &path, false)?;
    }
    println!("\nSaved: {}", path.display());
    Ok(())
}

fn new_dump(info: &DeviceInfo, redact_serial: bool) -> DeviceDump {
    DeviceDump {
        tool: env!("CARGO_PKG_NAME"),
        tool_version: env!("CARGO_PKG_VERSION"),
        captured_at: Utc::now(),
        os: std::env::consts::OS,
        descriptor_source: DESCRIPTOR_SOURCE,
        serial_redacted: redact_serial,
        info: InfoDump {
            vendor_id: info.vendor_id(),
            product_id: info.product_id(),
            release_number: info.release_number(),
            manufacturer: info.manufacturer_string().map(str::to_owned),
            product: info.product_string().map(str::to_owned),
            serial: if redact_serial {
                None
            } else {
                info.serial_number().map(str::to_owned)
            },
            usage_page: info.usage_page(),
            usage: info.usage(),
            interface_number: info.interface_number(),
            path: info.path().to_string_lossy().into_owned(),
        },
        open_error: None,
        report_descriptor_hex: None,
        descriptor_error: None,
        descriptor: None,
        feature_reports: Vec::new(),
        input_reports: Vec::new(),
        incomplete: false,
        feature_cycle_ms: None,
    }
}

fn read_descriptor_and_features(device: &ReadOnlyHid, dump: &mut DeviceDump, redact: bool) {
    let mut buf = vec![0u8; DESCRIPTOR_BUF];
    let descriptor = match device.report_descriptor(&mut buf) {
        Ok(n) => {
            let bytes = &buf[..n];
            dump.report_descriptor_hex = Some(hex(bytes));
            match ReportDescriptor::parse(bytes) {
                Ok(d) => d,
                Err(e) => {
                    dump.descriptor_error = Some(e.to_string());
                    return;
                }
            }
        }
        Err(e) => {
            dump.descriptor_error = Some(format!("reading descriptor: {e}"));
            return;
        }
    };

    let buf_len = (descriptor.max_report_len(ReportKind::Feature) + 1).max(MIN_REPORT_BUF);
    dump.feature_cycle_ms = time_feature_cycle(device, &descriptor, buf_len);
    for report_id in descriptor.report_ids(ReportKind::Feature) {
        let mut buf = vec![0u8; buf_len];
        buf[0] = report_id;
        let expected_len = descriptor.report_len(ReportKind::Feature, report_id);
        let report = match device.feature_report(&mut buf) {
            Ok(n) => {
                // The reply starts with the report ID byte (also when IDs are unused).
                let received = buf.get(1..n).unwrap_or_default();
                let payload = &received[..received.len().min(expected_len)];
                let mut lookup = |i: i32| device.indexed_string(i).map_err(|e| e.to_string());
                ReportDump {
                    kind: ReportKind::Feature,
                    report_id,
                    at_ms: None,
                    error: None,
                    payload_hex: Some(hex(payload)),
                    expected_len,
                    received_len: Some(received.len()),
                    values: decode(
                        &descriptor,
                        descriptor.fields_in(ReportKind::Feature, report_id),
                        payload,
                        &mut lookup,
                        redact,
                    ),
                }
            }
            Err(e) => ReportDump {
                kind: ReportKind::Feature,
                report_id,
                at_ms: None,
                error: Some(e.to_string()),
                payload_hex: None,
                expected_len,
                received_len: None,
                values: Vec::new(),
            },
        };
        dump.feature_reports.push(report);
    }
    dump.descriptor = Some(descriptor);
}

/// Average time of one full `GET_FEATURE` sweep over every feature report,
/// excluding decoding and string lookups. It tells us how fast the collector
/// can poll.
fn time_feature_cycle(device: &ReadOnlyHid, d: &ReportDescriptor, buf_len: usize) -> Option<f64> {
    const CYCLES: u32 = 5;
    let ids = d.report_ids(ReportKind::Feature);
    let mut buf = vec![0u8; buf_len];
    let start = Instant::now();
    for _ in 0..CYCLES {
        for &id in &ids {
            buf[0] = id;
            device.feature_report(&mut buf).ok()?;
        }
    }
    Some(start.elapsed().as_secs_f64() * 1000.0 / f64::from(CYCLES))
}

fn listen_inputs(device: &ReadOnlyHid, dump: &mut DeviceDump, duration: Duration, redact: bool) {
    let Some(descriptor) = &dump.descriptor else {
        return;
    };
    println!(
        "\nListening for input reports for {}s ...",
        duration.as_secs()
    );
    let buf_len = (descriptor.max_report_len(ReportKind::Input) + 1).max(MIN_REPORT_BUF);
    let mut buf = vec![0u8; buf_len];
    let start = Instant::now();
    let elapsed_ms = || Some(u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX));

    while start.elapsed() < duration {
        match device.input_report(&mut buf, 250) {
            Ok(0) => {}
            Ok(n) => {
                // Input reports carry a report ID byte only when the device uses IDs.
                let (report_id, received) = if descriptor.uses_report_ids() {
                    (buf[0], &buf[1..n])
                } else {
                    (0, &buf[..n])
                };
                let expected_len = descriptor.report_len(ReportKind::Input, report_id);
                let payload = &received[..received.len().min(expected_len)];
                let mut lookup = |i: i32| device.indexed_string(i).map_err(|e| e.to_string());
                dump.input_reports.push(ReportDump {
                    kind: ReportKind::Input,
                    report_id,
                    at_ms: elapsed_ms(),
                    error: None,
                    payload_hex: Some(hex(payload)),
                    expected_len,
                    received_len: Some(received.len()),
                    values: decode(
                        descriptor,
                        descriptor.fields_in(ReportKind::Input, report_id),
                        payload,
                        &mut lookup,
                        redact,
                    ),
                });
            }
            Err(e) => {
                dump.input_reports.push(ReportDump {
                    kind: ReportKind::Input,
                    report_id: 0,
                    at_ms: elapsed_ms(),
                    error: Some(e.to_string()),
                    payload_hex: None,
                    expected_len: 0,
                    received_len: None,
                    values: Vec::new(),
                });
                break;
            }
        }
    }
}

fn print_dump(dump: &DeviceDump) {
    let i = &dump.info;
    println!(
        "\n=== {:04x}:{:04x}  {} / {}  (page 0x{:04X} usage 0x{:04X}, interface {})",
        i.vendor_id,
        i.product_id,
        i.manufacturer.as_deref().unwrap_or("?"),
        i.product.as_deref().unwrap_or("?"),
        i.usage_page,
        i.usage,
        i.interface_number,
    );
    if let Some(serial) = &i.serial {
        println!("    serial {serial}");
    }
    println!("    {}", i.path);

    if let Some(e) = &dump.open_error {
        println!("    OPEN FAILED: {e}");
        return;
    }
    if let Some(e) = &dump.descriptor_error {
        println!("    DESCRIPTOR FAILED: {e}");
        return;
    }
    if let Some(d) = &dump.descriptor {
        println!(
            "    descriptor ({DESCRIPTOR_SOURCE}): {} data fields, {} collections",
            d.fields().len(),
            d.collections().len()
        );
    }
    print_reports(&dump.feature_reports);
}

fn print_reports(reports: &[ReportDump]) {
    for r in reports {
        let when = r.at_ms.map(|ms| format!(" @{ms}ms")).unwrap_or_default();
        match (&r.error, &r.payload_hex) {
            (Some(e), _) => println!("\n  {:?} #{:02X}{when}: ERROR {e}", r.kind, r.report_id),
            (None, Some(p)) => {
                let length_note = match r.received_len {
                    Some(n) if n != r.expected_len => {
                        format!("  (received {n} bytes, descriptor says {})", r.expected_len)
                    }
                    _ => String::new(),
                };
                println!(
                    "\n  {:?} #{:02X}{when}: [{p}]{length_note}",
                    r.kind, r.report_id
                );
            }
            (None, None) => {}
        }
        for v in &r.values {
            println!("    {:<56} = {}", v.name, format_value(v));
        }
    }
}

fn format_value(v: &dump::FieldValue) -> String {
    if v.redacted {
        return "<redacted>".to_owned();
    }
    let Some(raw) = v.raw else {
        return "<missing>".to_owned();
    };
    // Writing to a String can't fail, so the fmt::Results below are ignored.
    let mut out = raw.to_string();
    #[allow(clippy::cast_precision_loss)] // raw is at most 33 bits wide
    let raw_f = raw as f64;
    if let Some(scaled) = v.scaled.filter(|s| (s - raw_f).abs() > f64::EPSILON) {
        let _ = write!(out, " (scaled {scaled})");
    }
    if v.array {
        let selected = v.selected.as_deref().unwrap_or("none");
        let _ = write!(out, " [array → {selected}]");
    }
    if let Some(unit) = v.unit {
        let _ = match v.normalized_exponent {
            Some(0) => write!(out, " {unit}"),
            Some(e) => write!(out, " ×10^{e} {unit}"),
            None => write!(out, " {unit} (exp {})", v.unit_exponent),
        };
    }
    if let Some(s) = &v.string {
        let _ = write!(out, "  \"{s}\"");
    }
    if let Some(e) = &v.string_error {
        let _ = write!(out, "  (string unavailable: {e})");
    }
    out
}

/// `{vid}-{pid}-p{page}-u{usage}-if{iface}-{index}-{timestamp}.json`: unique
/// per collection even when one device exposes several.
fn dump_file_name(dump: &DeviceDump, index: usize) -> String {
    let i = &dump.info;
    format!(
        "{:04x}-{:04x}-p{:04x}-u{:04x}-if{}-{index}-{}.json",
        i.vendor_id,
        i.product_id,
        i.usage_page,
        i.usage,
        i.interface_number,
        dump.captured_at.format("%Y%m%dT%H%M%SZ")
    )
}

/// Writes the dump. The first write refuses to replace an existing file.
fn save(dump: &DeviceDump, path: &Path, first: bool) -> Result<()> {
    let json = serde_json::to_string_pretty(dump)?;
    let mut options = OpenOptions::new();
    options.write(true);
    if first {
        options.create_new(true);
    } else {
        options.truncate(true);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("writing {}", path.display()))?;
    file.write_all(json.as_bytes())
        .with_context(|| format!("writing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hex_ids() {
        assert_eq!(parse_hex_u16("0463"), Ok(0x0463));
        assert_eq!(parse_hex_u16("0xFFFF"), Ok(0xFFFF));
        assert_eq!(parse_hex_u16("0Xffff"), Ok(0xFFFF));
        assert!(parse_hex_u16("0x0x12").is_err());
        assert!(parse_hex_u16("+12").is_err());
        assert!(parse_hex_u16("").is_err());
        assert!(parse_hex_u16("0x").is_err());
        assert!(parse_hex_u16("10000").is_err());
    }

    #[test]
    fn selects_targets() {
        // No filter: power devices only.
        assert!(is_target(0x0463, 0xFFFF, 0x84, None, None));
        assert!(is_target(0x0463, 0xFFFF, 0x85, None, None));
        assert!(!is_target(0x046D, 0xB037, 0x01, None, None));
        // Filters: any page, matching IDs.
        assert!(is_target(0x0463, 0xFFFF, 0xFF00, Some(0x0463), None));
        assert!(is_target(0x0463, 0xFFFF, 0x01, Some(0x0463), Some(0xFFFF)));
        assert!(!is_target(0x0463, 0xFFFF, 0x84, Some(0x0463), Some(0x0001)));
        assert!(!is_target(0x0463, 0xFFFF, 0x84, None, Some(0x0001)));
    }

    #[test]
    fn file_names_are_unique_per_collection() {
        let info = InfoDump {
            vendor_id: 0x0463,
            product_id: 0xFFFF,
            release_number: 1,
            manufacturer: None,
            product: None,
            serial: None,
            usage_page: 0x84,
            usage: 0x04,
            interface_number: 0,
            path: String::new(),
        };
        let dump = DeviceDump {
            tool: "t",
            tool_version: "0",
            captured_at: "2026-11-11T12:00:00Z".parse().unwrap(),
            os: "test",
            descriptor_source: DESCRIPTOR_SOURCE,
            serial_redacted: false,
            info,
            open_error: None,
            report_descriptor_hex: None,
            descriptor_error: None,
            descriptor: None,
            feature_reports: Vec::new(),
            input_reports: Vec::new(),
            incomplete: false,
            feature_cycle_ms: None,
        };
        assert_eq!(
            dump_file_name(&dump, 2),
            "0463-ffff-p0084-u0004-if0-2-20261111T120000Z.json"
        );
    }
}
