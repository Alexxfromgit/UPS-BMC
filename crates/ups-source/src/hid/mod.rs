//! Direct USB HID Power Device source (read-only, ADR-0001; the only write is
//! the allowlisted settings in `control.rs`, ADR-0003).
//!
//! On connect, the source reads the report descriptor, builds a [`Mapping`]
//! and reads every mapped report once. Each poll then re-reads only the
//! *live* reports (status, charge/runtime, load, voltages). On the Eaton 5E
//! that is 5 reports at about 44 ms each. Ratings and configuration are
//! re-read every `slow_every`.

mod control;
pub mod mapping;
mod readonly;

use std::collections::HashMap;
use std::ffi::CString;
use std::time::{Duration, Instant};

use chrono::Utc;
use hidapi::{HidApi, HidError};
use tracing::{debug, info};
use ups_hid::{ReportDescriptor, ReportKind, usages};
use ups_model::{DeviceInfo, Snapshot};

pub use mapping::{Mapping, Rate, Target};
pub use readonly::ReadOnlyHid;

use crate::{Setting, SourceError, TelemetrySource};

/// Size of hidapi's report descriptor buffer.
const DESCRIPTOR_BUF: usize = 4096;
/// Minimum report buffer (Windows wants at least the longest report).
const MIN_REPORT_BUF: usize = 64;
/// Usage ID of the "UPS" application collection on the Power Device page.
const USAGE_UPS: u16 = 0x04;

/// Which device to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HidConfig {
    /// Restrict to this USB vendor ID.
    pub vendor_id: Option<u16>,
    /// Restrict to this USB product ID.
    pub product_id: Option<u16>,
    /// How often ratings and configuration are refreshed.
    pub slow_every: Duration,
}

impl Default for HidConfig {
    fn default() -> Self {
        Self {
            vendor_id: None,
            product_id: None,
            slow_every: Duration::from_secs(60),
        }
    }
}

fn io(e: &HidError) -> SourceError {
    SourceError::Io(e.to_string())
}

/// A connected device.
struct Connection {
    device: ReadOnlyHid,
    descriptor: ReportDescriptor,
    mapping: Mapping,
    live: Vec<u8>,
    slow: Vec<u8>,
    /// Last payload of every report read so far.
    reports: HashMap<u8, Vec<u8>>,
    buf: Vec<u8>,
    last_slow: Instant,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("live", &self.live)
            .field("slow", &self.slow)
            .finish_non_exhaustive()
    }
}

impl Connection {
    fn read(&mut self, report_id: u8) -> Result<(), SourceError> {
        self.buf.fill(0);
        self.buf[0] = report_id;
        let n = self
            .device
            .feature_report(&mut self.buf)
            .map_err(|e| io(&e))?;
        let expected = self.descriptor.report_len(ReportKind::Feature, report_id);
        let payload = self.buf.get(1..n).unwrap_or_default();
        let payload = &payload[..payload.len().min(expected)];
        self.reports.insert(report_id, payload.to_vec());
        Ok(())
    }

    fn read_all(&mut self, ids: &[u8]) -> Result<(), SourceError> {
        ids.iter().try_for_each(|&id| self.read(id))
    }
}

/// Reads a HID UPS through the OS HID stack.
pub struct HidSource {
    config: HidConfig,
    api: Option<HidApi>,
    conn: Option<Connection>,
}

impl std::fmt::Debug for HidSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HidSource")
            .field("config", &self.config)
            .field("connected", &self.conn.is_some())
            .finish_non_exhaustive()
    }
}

impl HidSource {
    #[must_use]
    pub fn new(config: HidConfig) -> Self {
        Self {
            config,
            api: None,
            conn: None,
        }
    }

    fn api(&mut self) -> Result<&HidApi, SourceError> {
        match &mut self.api {
            Some(api) => api.refresh_devices().map_err(|e| io(&e))?,
            None => self.api = Some(HidApi::new().map_err(|e| io(&e))?),
        }
        Ok(self.api.as_ref().expect("initialised above"))
    }

    fn find(&mut self) -> Result<(CString, u16, u16), SourceError> {
        let config = self.config.clone();
        let api = self.api()?;
        api.device_list()
            .find(|d| {
                let ids_match = config.vendor_id.is_none_or(|v| v == d.vendor_id())
                    && config.product_id.is_none_or(|p| p == d.product_id());
                let is_ups = d.usage_page() == usages::PAGE_POWER_DEVICE && d.usage() == USAGE_UPS;
                ids_match && is_ups
            })
            .map(|d| (d.path().to_owned(), d.vendor_id(), d.product_id()))
            .ok_or(SourceError::NotFound)
    }
}

impl TelemetrySource for HidSource {
    fn kind(&self) -> &'static str {
        "hid"
    }

    fn connect(&mut self) -> Result<DeviceInfo, SourceError> {
        self.conn = None;
        let (path, vendor_id, product_id) = self.find()?;
        let api = self.api.as_ref().ok_or(SourceError::NotConnected)?;
        let device = ReadOnlyHid::open(api, &path).map_err(|e| io(&e))?;

        let mut buf = vec![0u8; DESCRIPTOR_BUF];
        let n = device.report_descriptor(&mut buf).map_err(|e| io(&e))?;
        let descriptor = ReportDescriptor::parse(&buf[..n])
            .map_err(|e| SourceError::Unsupported(format!("report descriptor: {e}")))?;
        let mapping = Mapping::build(&descriptor);
        if !mapping.is_usable() {
            return Err(SourceError::Unsupported(
                "no power source or charge usages in the descriptor".into(),
            ));
        }
        debug!(bindings = mapping.bindings().len(), "HID mapping built");

        let buf_len = (descriptor.max_report_len(ReportKind::Feature) + 1).max(MIN_REPORT_BUF);
        let mut conn = Connection {
            device,
            live: mapping.reports(Rate::Live),
            slow: mapping.reports(Rate::Slow),
            mapping,
            descriptor,
            reports: HashMap::new(),
            buf: vec![0u8; buf_len],
            last_slow: Instant::now(),
        };
        let once = conn.mapping.reports(Rate::Once);
        let all: Vec<u8> = [conn.live.clone(), conn.slow.clone(), once].concat();
        conn.read_all(&all)?;

        let mut strings = HashMap::new();
        for (target, index) in conn.mapping.string_indices(&conn.descriptor, &conn.reports) {
            if let Ok(Some(s)) = conn.device.indexed_string(index) {
                strings.insert(target, s.trim().to_owned());
            }
        }
        let info = DeviceInfo {
            source: "hid".into(),
            manufacturer: strings.remove(&Target::Manufacturer),
            model: strings.remove(&Target::Product),
            serial: strings.remove(&Target::Serial),
            battery_type: strings.remove(&Target::Chemistry),
            vendor_id: Some(vendor_id),
            product_id: Some(product_id),
        };
        info!(
            device = %info.display_name(),
            live_reports = ?conn.live,
            slow_reports = ?conn.slow,
            "HID UPS connected"
        );
        self.conn = Some(conn);
        Ok(info)
    }

    fn poll(&mut self) -> Result<Snapshot, SourceError> {
        let slow_every = self.config.slow_every;
        let conn = self.conn.as_mut().ok_or(SourceError::NotConnected)?;
        let ts = Utc::now();
        let live = conn.live.clone();
        let mut result = conn.read_all(&live);
        if result.is_ok() && conn.last_slow.elapsed() >= slow_every {
            let slow = conn.slow.clone();
            result = conn.read_all(&slow);
            conn.last_slow = Instant::now();
        }
        match result {
            Ok(()) => Ok(conn.mapping.snapshot(&conn.descriptor, &conn.reports, ts)),
            Err(e) => {
                self.conn = None;
                Err(e)
            }
        }
    }

    fn capabilities(&self) -> Vec<&'static str> {
        self.conn
            .as_ref()
            .filter(|c| {
                control::supports(&c.descriptor, &c.mapping, Setting::Beeper { enabled: true })
            })
            .map(|_| vec![crate::CAP_BEEPER])
            .unwrap_or_default()
    }

    fn apply(&mut self, setting: Setting) -> Result<(), SourceError> {
        let conn = self.conn.as_mut().ok_or(SourceError::NotConnected)?;
        control::apply(
            &conn.device,
            &conn.descriptor,
            &conn.mapping,
            &mut conn.reports,
            setting,
        )
    }
}
