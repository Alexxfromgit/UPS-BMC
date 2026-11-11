//! Pure presentation logic: collector data → strings and chart paths for the
//! UI. No Slint types here, so everything is unit-testable.

use std::fmt::Write as _;

use chrono::{DateTime, Local, Utc};
use ups_api::{Connection, Event, HistoryPoint, Severity, Snapshot, StatusResponse, UpsStatus};

pub const STATE_OFFLINE: i32 = 0;
pub const STATE_UTILITY: i32 = 1;
pub const STATE_BATTERY: i32 = 2;
pub const STATE_CRITICAL: i32 = 3;

/// History shown in the charts.
pub const CHART_WINDOW: chrono::Duration = chrono::Duration::minutes(15);
/// A snapshot older than this is shown as stale.
const STALE_AFTER: chrono::Duration = chrono::Duration::seconds(10);
/// Charge at or below which "on battery" is shown as critical, even before
/// the UPS raises its low-battery flag.
const CRITICAL_CHARGE: f32 = 20.0;
/// Voltage chart range.
const VOLT_MIN: f32 = 180.0;
const VOLT_MAX: f32 = 260.0;

/// Everything the main window and the tray display.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct View {
    pub state: i32,
    pub device_name: String,
    pub connection_text: String,
    pub updated_text: String,
    pub status_title: String,
    pub status_detail: String,
    pub show_start: bool,
    pub charge_text: String,
    pub charge_arc: String,
    pub runtime_text: String,
    pub load_text: String,
    pub load_detail: String,
    pub input_text: String,
    pub input_detail: String,
    pub output_text: String,
    pub output_detail: String,
    pub battery_v_text: String,
    pub battery_v_detail: String,
    pub charts: ChartData,
    pub volt_top: String,
    pub volt_bottom: String,
    pub chart_span: String,
    pub footer_text: String,
    pub tooltip: String,
    /// The beeper switch can be used (connected UPS that supports it).
    pub beeper_available: bool,
    pub beeper_on: bool,
    pub beeper_text: String,
}

/// One row of the events list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventLine {
    pub time: String,
    pub text: String,
    pub level: i32,
}

const DASH: &str = "—";

fn fmt_opt(v: Option<f32>, unit: &str, decimals: usize) -> String {
    v.map_or_else(|| DASH.to_owned(), |v| format!("{v:.decimals$} {unit}"))
}

/// `"4 h 37 min"`, `"12 min"`, `"45 s"`.
pub fn runtime(seconds: u32) -> String {
    match seconds {
        0..60 => format!("{seconds} s"),
        60..3600 => format!("{} min", seconds / 60),
        _ => format!("{} h {:02} min", seconds / 3600, (seconds % 3600) / 60),
    }
}

/// View shown while the collector itself can't be reached.
pub fn collector_offline(detail: &str, can_start: bool) -> View {
    View {
        state: STATE_OFFLINE,
        device_name: "UPS-BMC".into(),
        connection_text: "Collector offline".into(),
        status_title: "Collector not running".into(),
        status_detail: detail.into(),
        show_start: can_start,
        charge_text: DASH.into(),
        runtime_text: DASH.into(),
        load_text: DASH.into(),
        input_text: DASH.into(),
        output_text: DASH.into(),
        battery_v_text: DASH.into(),
        volt_top: format!("{VOLT_MAX:.0}"),
        volt_bottom: format!("{VOLT_MIN:.0}"),
        tooltip: "UPS-BMC: collector not running".into(),
        beeper_text: "Available while the collector is running".into(),
        ..View::default()
    }
}

/// Builds the view from the latest status and the recent history.
#[allow(clippy::too_many_lines)]
pub fn build(status: &StatusResponse, history: &[HistoryPoint], now: DateTime<Utc>) -> View {
    let device_name = status
        .device
        .as_ref()
        .map_or_else(|| "UPS".to_owned(), ups_api::DeviceInfo::display_name);
    let connected = status.connection == Connection::Connected;
    let snapshot = status.snapshot.as_ref();
    let stale = snapshot.is_none_or(|s| now - s.ts > STALE_AFTER);
    let flags = snapshot.and_then(|s| s.status);
    let charge = snapshot.and_then(|s| s.battery_charge_pct);
    let runtime_s = snapshot.and_then(|s| s.battery_runtime_s);

    let level = match (connected && !stale, flags) {
        (false, _) | (true, None) => STATE_OFFLINE,
        (true, Some(f)) if f.contains(UpsStatus::LB) => STATE_CRITICAL,
        (true, Some(f)) if f.on_battery() => {
            if charge.is_some_and(|c| c <= CRITICAL_CHARGE) {
                STATE_CRITICAL
            } else {
                STATE_BATTERY
            }
        }
        (true, Some(_)) => STATE_UTILITY,
    };

    let left = runtime_s.map(|r| format!("about {} left", runtime(r)));
    let (status_title, status_detail) = match &status.connection {
        Connection::Connecting => ("Looking for the UPS…".to_owned(), String::new()),
        Connection::Disconnected { reason } => (
            "UPS not connected".to_owned(),
            format!("{reason}. Check the USB cable; the collector retries automatically."),
        ),
        Connection::Connected if stale => (
            "Waiting for data…".to_owned(),
            "The UPS has not reported for a while.".to_owned(),
        ),
        Connection::Connected => match level {
            STATE_CRITICAL => (
                "Low battery!".to_owned(),
                format!(
                    "Mains power is off. {}",
                    left.clone()
                        .unwrap_or_else(|| "Shutdown may happen soon".into())
                ),
            ),
            STATE_BATTERY => (
                "Running on battery".to_owned(),
                format!(
                    "Mains power lost · {}",
                    left.clone().unwrap_or_else(|| "runtime unknown".into())
                ),
            ),
            _ => ("On utility power".to_owned(), utility_detail(flags, charge)),
        },
    };

    let connection_text = match (&status.connection, stale) {
        (Connection::Connected, false) => format!("Connected · {}", status.source.to_uppercase()),
        (Connection::Connected, true) => "Stale data".into(),
        (Connection::Connecting, _) => "Connecting…".into(),
        (Connection::Disconnected { .. }, _) => "UPS disconnected".into(),
    };
    let updated_text = snapshot
        .map(|s| s.ts.with_timezone(&Local).format("%H:%M:%S").to_string())
        .unwrap_or_default();

    let s = snapshot;
    let load = s.and_then(|s| s.load_pct);
    let watts = s.and_then(|s| s.realpower_w.or_else(|| s.estimated_load_w()));
    let nominal_w = s.and_then(|s| s.realpower_nominal_w);
    let nominal_va = s.and_then(|s| s.power_nominal_va);
    let load_detail = match (watts, nominal_w, nominal_va) {
        (Some(w), Some(n), Some(va)) => format!("{w:.0} W of {n:.0} W ({va:.0} VA rating)"),
        (Some(w), Some(n), None) => format!("{w:.0} W of {n:.0} W"),
        (Some(w), None, _) => format!("{w:.0} W"),
        _ => String::new(),
    };

    let on_battery = flags.is_some_and(UpsStatus::on_battery);
    let input = s.and_then(|s| s.input_voltage_v);
    let input_detail = if on_battery {
        "mains power is off".to_owned()
    } else {
        match s.and_then(|s| s.input_frequency_hz) {
            Some(hz) => format!("{hz:.1} Hz"),
            None => avr_text(flags).unwrap_or("mains voltage").to_owned(),
        }
    };
    let output_detail = [
        s.and_then(|s| s.output_frequency_hz)
            .map(|hz| format!("{hz:.1} Hz")),
        s.and_then(|s| s.extra.get("output.voltage.nominal"))
            .map(|v| format!("nominal {v} V")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    let battery_v_detail = [
        status
            .device
            .as_ref()
            .and_then(|d| d.battery_type.clone())
            .map(|t| {
                if t == "PbAc" {
                    "lead-acid".to_owned()
                } else {
                    t
                }
            }),
        s.and_then(|s| s.extra.get(ups_api::vars::BATTERY_CHARGE_LOW))
            .map(|v| format!("low-battery alarm at {v} %")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");

    let beeper_status = s
        .and_then(|s| s.extra.get(ups_api::vars::UPS_BEEPER_STATUS))
        .map(String::as_str);
    let can_set_beeper = connected && !stale && status.capabilities.iter().any(|c| c == "beeper");
    let beeper_text = match (can_set_beeper, beeper_status) {
        (false, _) => "Available when a UPS that supports it is connected".to_owned(),
        (true, Some("muted")) => "Muted until the current alarm ends".to_owned(),
        (true, Some(reported)) => format!("The UPS reports: {reported}"),
        (true, None) => "The UPS does not report the beeper state".to_owned(),
    };

    let charts = ChartData::build(history, now);
    let charge_text = charge.map_or_else(|| DASH.to_owned(), |c| format!("{c:.0}%"));
    let tooltip = match charge {
        Some(c) => format!("UPS-BMC: {status_title} · {c:.0} %"),
        None => format!("UPS-BMC: {status_title}"),
    };

    View {
        state: level,
        device_name,
        connection_text,
        updated_text,
        status_title,
        status_detail,
        show_start: false,
        charge_arc: charge.map(arc_path).unwrap_or_default(),
        charge_text,
        runtime_text: runtime_s.map_or_else(|| DASH.to_owned(), runtime),
        load_text: fmt_opt(load, "%", 0),
        load_detail,
        input_text: fmt_opt(input, "V", 0),
        input_detail,
        output_text: fmt_opt(s.and_then(|s| s.output_voltage_v), "V", 0),
        output_detail,
        battery_v_text: fmt_opt(s.and_then(|s| s.battery_voltage_v), "V", 1),
        battery_v_detail,
        charts,
        volt_top: format!("{VOLT_MAX:.0}"),
        volt_bottom: format!("{VOLT_MIN:.0}"),
        chart_span: format!("last {} min", CHART_WINDOW.num_minutes()),
        footer_text: format!(
            "Collector {} · source {} · polling every {} ms · running since {}",
            status.collector_version,
            status.source,
            status.poll_interval_ms,
            status
                .started_at
                .with_timezone(&Local)
                .format("%Y-%m-%d %H:%M"),
        ),
        tooltip,
        beeper_available: can_set_beeper && beeper_status.is_some(),
        beeper_on: beeper_status == Some("enabled"),
        beeper_text,
    }
}

fn avr_text(flags: Option<UpsStatus>) -> Option<&'static str> {
    let f = flags?;
    if f.contains(UpsStatus::BOOST) {
        Some("AVR boosting low mains voltage")
    } else if f.contains(UpsStatus::TRIM) {
        Some("AVR trimming high mains voltage")
    } else {
        None
    }
}

fn utility_detail(flags: Option<UpsStatus>, charge: Option<f32>) -> String {
    let mut parts = Vec::new();
    let full = charge.is_some_and(|c| c >= 99.5);
    match flags {
        Some(f) if f.contains(UpsStatus::CHRG) && !full => parts.push("Charging".to_owned()),
        _ if full => parts.push("Battery fully charged".to_owned()),
        _ => {}
    }
    if let Some(avr) = avr_text(flags) {
        parts.push(avr.to_owned());
    }
    if let Some(f) = flags {
        if f.contains(UpsStatus::RB) {
            parts.push("Battery needs replacement".to_owned());
        }
        if f.contains(UpsStatus::OVER) {
            parts.push("Overloaded!".to_owned());
        }
    }
    if parts.is_empty() {
        "Mains power OK".into()
    } else {
        parts.join(" · ")
    }
}

/// Ring gauge arc (100 × 100 view box, radius 44, clockwise from the top).
pub fn arc_path(pct: f32) -> String {
    let pct = f64::from(pct.clamp(0.0, 100.0));
    if pct <= 0.0 {
        return String::new();
    }
    if pct >= 99.95 {
        return "M 50 6 A 44 44 0 1 1 49.99 6".into();
    }
    let angle = pct / 100.0 * std::f64::consts::TAU;
    let x = 50.0 + 44.0 * angle.sin();
    let y = 50.0 - 44.0 * angle.cos();
    let large = i32::from(pct > 50.0);
    format!("M 50 6 A 44 44 0 {large} 1 {x:.2} {y:.2}")
}

/// Chart data over [`CHART_WINDOW`], normalised to 0..1 (x: time, y: 0 = top).
/// [`ChartData::render`] turns it into path commands in the plot's real pixel
/// size. Slint's `Path` keeps the view box aspect ratio, so a fixed view box
/// would squash the plot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChartData {
    charge: Vec<(f64, Option<f64>)>,
    load: Vec<(f64, Option<f64>)>,
    input: Vec<(f64, Option<f64>)>,
    output: Vec<(f64, Option<f64>)>,
    on_battery: Vec<(f64, bool)>,
}

/// Path commands for the two charts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderedCharts {
    pub shade: String,
    pub charge: String,
    pub load: String,
    pub input: String,
    pub output: String,
}

impl ChartData {
    fn build(history: &[HistoryPoint], now: DateTime<Utc>) -> Self {
        let start = now - CHART_WINDOW;
        #[allow(clippy::cast_precision_loss)] // milliseconds within 15 minutes
        let x = |ts: DateTime<Utc>| {
            ((ts - start).num_milliseconds() as f64 / CHART_WINDOW.num_milliseconds() as f64)
                .clamp(0.0, 1.0)
        };
        let pct = |v: f32| 1.0 - f64::from(v.clamp(0.0, 100.0)) / 100.0;
        let volt = |v: f32| {
            1.0 - f64::from((v.clamp(VOLT_MIN, VOLT_MAX) - VOLT_MIN) / (VOLT_MAX - VOLT_MIN))
        };
        let visible: Vec<&HistoryPoint> = history.iter().filter(|p| p.ts >= start).collect();
        let line = |value: &dyn Fn(&HistoryPoint) -> Option<f64>| {
            visible.iter().map(|p| (x(p.ts), value(p))).collect()
        };
        Self {
            charge: line(&|p| p.battery_charge_pct.map(pct)),
            load: line(&|p| p.load_pct.map(pct)),
            // 0 V on battery would pin the line to the bottom; show a gap instead.
            input: line(&|p| p.input_voltage_v.filter(|v| *v > 1.0).map(volt)),
            output: line(&|p| p.output_voltage_v.filter(|v| *v > 1.0).map(volt)),
            on_battery: visible
                .iter()
                .map(|p| (x(p.ts), p.on_battery == Some(true)))
                .collect(),
        }
    }

    /// Path commands for a plot area of `width` × `height` logical pixels.
    #[must_use]
    pub fn render(&self, width: f64, height: f64) -> RenderedCharts {
        let (w, h) = if width > 1.0 && height > 1.0 {
            (width, height)
        } else {
            (1000.0, 100.0)
        };
        let scale = |pts: &[(f64, Option<f64>)]| {
            series(pts.iter().map(|&(x, y)| (x * w, y.map(|y| y * h))))
        };
        RenderedCharts {
            shade: shade(self.on_battery.iter().map(|&(x, b)| (x * w, b)), h),
            charge: scale(&self.charge),
            load: scale(&self.load),
            input: scale(&self.input),
            output: scale(&self.output),
        }
    }
}

/// SVG path through the points, with gaps where the value is `None`.
fn series(points: impl Iterator<Item = (f64, Option<f64>)>) -> String {
    let mut out = String::new();
    let mut pen_down = false;
    for (x, y) in points {
        match y {
            Some(y) => {
                let cmd = if pen_down { 'L' } else { 'M' };
                let _ = write!(out, "{cmd} {x:.1} {y:.1} ");
                pen_down = true;
            }
            None => pen_down = false,
        }
    }
    out.trim_end().to_owned()
}

/// Filled rectangles of height `h` over the spans where `on_battery` is true.
fn shade(points: impl Iterator<Item = (f64, bool)>, h: f64) -> String {
    let mut out = String::new();
    let mut span_start: Option<f64> = None;
    let mut last_x = 0.0;
    let close = |out: &mut String, from: f64, to: f64| {
        let to = to.max(from + 2.0);
        let _ = write!(
            out,
            "M {from:.1} 0 L {to:.1} 0 L {to:.1} {h:.1} L {from:.1} {h:.1} Z "
        );
    };
    for (x, on_battery) in points {
        match (on_battery, span_start) {
            (true, None) => span_start = Some(x),
            (false, Some(from)) => {
                close(&mut out, from, x);
                span_start = None;
            }
            _ => {}
        }
        last_x = x;
    }
    if let Some(from) = span_start {
        close(&mut out, from, last_x);
    }
    out.trim_end().to_owned()
}
/// Converts a live snapshot into a history point for the local chart buffer.
pub fn history_point(s: &Snapshot) -> HistoryPoint {
    HistoryPoint {
        ts: s.ts,
        on_battery: s.on_battery(),
        battery_charge_pct: s.battery_charge_pct,
        #[allow(clippy::cast_precision_loss)] // runtimes are far below 2^24 s
        battery_runtime_s: s.battery_runtime_s.map(|r| r as f32),
        battery_voltage_v: s.battery_voltage_v,
        load_pct: s.load_pct,
        input_voltage_v: s.input_voltage_v,
        output_voltage_v: s.output_voltage_v,
        realpower_w: s.realpower_w,
    }
}

/// Event list rows: time as `HH:MM:SS` today, `DD.MM HH:MM` otherwise.
pub fn event_lines(events: &[Event], now: DateTime<Utc>) -> Vec<EventLine> {
    let today = now.with_timezone(&Local).date_naive();
    events
        .iter()
        .map(|e| {
            let local = e.ts.with_timezone(&Local);
            let time = if local.date_naive() == today {
                local.format("%H:%M:%S").to_string()
            } else {
                local.format("%d.%m %H:%M").to_string()
            };
            EventLine {
                time,
                text: e.message.clone(),
                level: match e.severity {
                    Severity::Info => 0,
                    Severity::Warning => 1,
                    Severity::Critical => 2,
                },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};
    use ups_api::{DeviceInfo, EventKind};

    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap()
    }

    fn status(flags: UpsStatus, charge: f32) -> StatusResponse {
        let mut s = Snapshot::empty(now() - Duration::seconds(1));
        s.status = Some(flags);
        s.battery_charge_pct = Some(charge);
        s.battery_runtime_s = Some(16_621);
        s.load_pct = Some(12.0);
        s.realpower_w = Some(58.0);
        s.realpower_nominal_w = Some(480.0);
        s.power_nominal_va = Some(900.0);
        s.input_voltage_v = Some(218.0);
        s.output_voltage_v = Some(216.0);
        s.output_frequency_hz = Some(50.0);
        s.battery_voltage_v = Some(13.6);
        StatusResponse {
            collector_version: "0.1.0".into(),
            source: "hid".into(),
            poll_interval_ms: 1000,
            started_at: now() - Duration::hours(1),
            connection: Connection::Connected,
            capabilities: vec!["beeper".into()],
            device: Some(DeviceInfo {
                source: "hid".into(),
                manufacturer: Some("EATON".into()),
                model: Some("Eaton 5E".into()),
                battery_type: Some("PbAc".into()),
                ..DeviceInfo::default()
            }),
            snapshot: Some(s),
        }
    }

    #[test]
    fn shows_utility_power() {
        let v = build(&status(UpsStatus::OL | UpsStatus::CHRG, 100.0), &[], now());
        assert_eq!(v.state, STATE_UTILITY);
        assert_eq!(v.device_name, "Eaton 5E");
        assert_eq!(v.status_title, "On utility power");
        assert_eq!(v.status_detail, "Battery fully charged");
        assert_eq!(v.charge_text, "100%");
        assert_eq!(v.runtime_text, "4 h 37 min");
        assert_eq!(v.load_text, "12 %");
        assert_eq!(v.load_detail, "58 W of 480 W (900 VA rating)");
        assert_eq!(v.input_text, "218 V");
        assert_eq!(v.output_detail, "50.0 Hz");
        assert_eq!(v.battery_v_text, "13.6 V");
        assert_eq!(v.battery_v_detail, "lead-acid");
        assert_eq!(v.connection_text, "Connected · HID");
        assert!(v.tooltip.contains("100 %"));
    }

    #[test]
    fn exposes_the_beeper_setting() {
        let mut st = status(UpsStatus::OL, 100.0);
        let v = build(&st, &[], now());
        assert!(!v.beeper_available, "beeper state unknown");
        st.snapshot
            .as_mut()
            .unwrap()
            .extra
            .insert("ups.beeper.status".into(), "enabled".into());
        let v = build(&st, &[], now());
        assert!(v.beeper_available);
        assert!(v.beeper_on);
        assert_eq!(v.beeper_text, "The UPS reports: enabled");
        st.capabilities.clear();
        assert!(!build(&st, &[], now()).beeper_available);
    }

    #[test]
    fn shows_battery_and_critical_states() {
        let v = build(
            &status(UpsStatus::OB | UpsStatus::DISCHRG, 70.0),
            &[],
            now(),
        );
        assert_eq!(v.state, STATE_BATTERY);
        assert_eq!(v.status_title, "Running on battery");
        assert!(v.status_detail.contains("about 4 h 37 min left"));
        assert_eq!(v.input_detail, "mains power is off");

        let v = build(&status(UpsStatus::OB | UpsStatus::LB, 30.0), &[], now());
        assert_eq!(v.state, STATE_CRITICAL);
        let v = build(&status(UpsStatus::OB, 15.0), &[], now());
        assert_eq!(v.state, STATE_CRITICAL, "low charge on battery is critical");
    }

    #[test]
    fn shows_disconnected_and_stale() {
        let mut st = status(UpsStatus::OL, 100.0);
        st.connection = Connection::Disconnected {
            reason: "no matching UPS found".into(),
        };
        let v = build(&st, &[], now());
        assert_eq!(v.state, STATE_OFFLINE);
        assert_eq!(v.status_title, "UPS not connected");

        let st = status(UpsStatus::OL, 100.0);
        let v = build(&st, &[], now() + Duration::seconds(30));
        assert_eq!(v.state, STATE_OFFLINE);
        assert_eq!(v.connection_text, "Stale data");
    }

    #[test]
    fn draws_arcs() {
        assert_eq!(arc_path(0.0), "");
        assert_eq!(arc_path(25.0), "M 50 6 A 44 44 0 0 1 94.00 50.00");
        assert_eq!(arc_path(75.0), "M 50 6 A 44 44 0 1 1 6.00 50.00");
        assert_eq!(arc_path(100.0), "M 50 6 A 44 44 0 1 1 49.99 6");
    }

    #[test]
    fn draws_series_with_gaps_and_battery_shading() {
        assert_eq!(
            series(
                [
                    (0.0, Some(10.0)),
                    (5.0, None),
                    (10.0, Some(20.0)),
                    (20.0, Some(30.0))
                ]
                .into_iter()
            ),
            "M 0.0 10.0 M 10.0 20.0 L 20.0 30.0"
        );
        assert_eq!(
            shade(
                [
                    (0.0, false),
                    (100.0, true),
                    (200.0, true),
                    (300.0, false),
                    (400.0, true)
                ]
                .into_iter(),
                100.0
            ),
            "M 100.0 0 L 300.0 0 L 300.0 100.0 L 100.0 100.0 Z M 400.0 0 L 402.0 0 L 402.0 100.0 L 400.0 100.0 Z"
        );
    }

    #[test]
    fn maps_history_into_the_window() {
        let mut p = history_point(&status(UpsStatus::OL, 50.0).snapshot.unwrap());
        p.ts = now() - Duration::minutes(15);
        let mut q = p.clone();
        q.ts = now();
        let old = HistoryPoint {
            ts: now() - Duration::minutes(30),
            ..p.clone()
        };
        let data = ChartData::build(&[old, p, q], now());
        // Rendered at the plot's real size: 50 % charge is half the height.
        let charts = data.render(800.0, 240.0);
        assert_eq!(charts.charge, "M 0.0 120.0 L 800.0 120.0");
        assert_eq!(charts.shade, "");
        // 218 V on the 180..260 V scale.
        assert_eq!(charts.input, "M 0.0 126.0 L 800.0 126.0");
        // Before layout (zero size) a fallback size is used.
        assert_eq!(data.render(0.0, 0.0).charge, "M 0.0 50.0 L 1000.0 50.0");
    }

    #[test]
    fn formats_events() {
        let e = Event::new(now() - Duration::minutes(1), EventKind::PowerLost, "lost");
        let lines = event_lines(&[e], now());
        assert_eq!(lines[0].text, "lost");
        assert_eq!(lines[0].level, 1);
        assert_eq!(lines[0].time.len(), 8);
    }
}

/// One statistics card on the Outages tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatCard {
    pub title: String,
    pub outages: String,
    pub on_battery: String,
    pub availability: String,
    pub coverage: String,
}

/// Everything on the Outages and Reports tabs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutagesView {
    pub cards: Vec<StatCard>,
    /// Table rows, one cell per column of the outage table in `ui/app.slint`.
    pub rows: Vec<Vec<String>>,
    pub summary: String,
    /// `YYYY-MM` keys and their labels (`November 2026`), newest first.
    pub months: Vec<String>,
    pub month_labels: Vec<String>,
}

/// Column titles of the outage table (must match `ui/app.slint`).
#[cfg(test)]
const OUTAGE_COLUMNS: [&str; 9] = [
    "Started",
    "Duration",
    "Battery",
    "Lowest",
    "UPS estimate",
    "Avg load",
    "Energy",
    "Recharge",
    "Notes",
];

/// Rows shown in the table (the API returns up to 500).
const MAX_ROWS: usize = 200;

#[allow(clippy::cast_precision_loss)] // durations far below 2^52 ms
fn secs(d: chrono::Duration) -> f64 {
    d.num_milliseconds() as f64 / 1000.0
}

/// Builds the Outages/Reports view (pure).
pub fn outages_view(
    outages: &[ups_api::Outage],
    stats: Option<&ups_api::StatsResponse>,
    now: DateTime<Utc>,
) -> OutagesView {
    use ups_analysis::format;

    let cards = stats
        .map(|s| {
            s.periods
                .iter()
                .map(|p| StatCard {
                    title: p.label.clone(),
                    outages: match p.outages {
                        0 => "No outages".to_owned(),
                        1 => "1 outage".to_owned(),
                        n => format!("{n} outages"),
                    },
                    on_battery: format!("On battery: {}", format::duration(p.on_battery_s)),
                    availability: format!(
                        "Mains availability: {}",
                        format::percent(p.availability_pct)
                    ),
                    coverage: format!("Monitored: {}", format::percent(Some(p.coverage_pct))),
                })
                .collect()
        })
        .unwrap_or_default();

    let dash = || DASH.to_owned();
    let rows = outages
        .iter()
        .take(MAX_ROWS)
        .enumerate()
        .map(|(i, o)| {
            let duration = format::duration(secs(o.duration(now)));
            let mut notes = Vec::new();
            if !o.start_observed() {
                notes.push("start not observed");
            }
            if !o.end_observed() {
                notes.push("end not observed");
            }
            vec![
                o.start
                    .with_timezone(&Local)
                    .format("%d.%m.%Y %H:%M:%S")
                    .to_string(),
                if o.is_ongoing() {
                    format!("ongoing · {duration}")
                } else {
                    duration
                },
                match (o.charge_start_pct, o.charge_end_pct) {
                    (Some(a), Some(b)) => format!("{a:.0} → {b:.0} %"),
                    _ => dash(),
                },
                o.charge_min_pct.map_or_else(dash, |c| format!("{c:.0} %")),
                o.runtime_estimate_start_s
                    .map_or_else(dash, |r| format::duration(f64::from(r))),
                match (o.avg_power_w, o.avg_load_pct) {
                    (Some(w), Some(l)) => format!("{w:.0} W ({l:.0} %)"),
                    (Some(w), None) => format!("{w:.0} W"),
                    (None, Some(l)) => format!("{l:.0} %"),
                    (None, None) => dash(),
                },
                o.energy_wh.map_or_else(dash, |e| format!("{e:.1} Wh")),
                match (o.recharge_duration(), o.end) {
                    (Some(d), _) => format::duration(secs(d)),
                    // Newest first: only the newest outage can still be recharging.
                    (None, Some(_)) if i == 0 => "in progress".to_owned(),
                    (None, Some(_)) => "interrupted".to_owned(),
                    (None, None) => dash(),
                },
                notes.join(", "),
            ]
        })
        .collect();

    let months = stats.map(|s| s.months.clone()).unwrap_or_default();
    let month_labels = months
        .iter()
        .map(|m| {
            ups_analysis::stats::parse_month(m).map_or_else(
                || m.clone(),
                |(y, mo)| ups_analysis::stats::month_label(y, mo),
            )
        })
        .collect();
    let summary = match outages.len() {
        0 => "No outages recorded in the last 90 days.".to_owned(),
        1 => "1 outage in the last 90 days.".to_owned(),
        n => format!("{n} outages in the last 90 days, newest first."),
    };
    OutagesView {
        cards,
        rows,
        summary,
        months,
        month_labels,
    }
}

#[cfg(test)]
mod outage_tests {
    use chrono::{Duration, TimeZone};
    use ups_api::{Outage, PeriodStats, StatsResponse};

    use super::*;

    #[test]
    fn builds_outage_rows_and_cards() {
        let now = Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap();
        let mut done = Outage::started(now - Duration::hours(3));
        done.id = 1;
        done.end = Some(done.start + Duration::minutes(12));
        done.charge_start_pct = Some(100.0);
        done.charge_end_pct = Some(81.0);
        done.charge_min_pct = Some(81.0);
        done.avg_power_w = Some(96.0);
        done.avg_load_pct = Some(20.0);
        done.energy_wh = Some(19.2);
        done.recharged_at = Some(done.start + Duration::minutes(80));
        let mut ongoing = Outage::started(now - Duration::seconds(90));
        ongoing.id = 2;
        ongoing.start_earliest = Some(now - Duration::minutes(5));

        let day = now.date_naive();
        let stats = StatsResponse {
            periods: vec![PeriodStats {
                label: "Today".into(),
                first_day: day,
                last_day: day,
                outages: 2,
                on_battery_s: 810.0,
                longest_s: 720.0,
                monitored_s: 43_200.0,
                coverage_pct: 100.0,
                availability_pct: Some(98.125),
                avg_recharge_s: None,
            }],
            months: vec!["2026-11".into()],
        };
        let v = outages_view(&[ongoing, done], Some(&stats), now);
        assert_eq!(v.cards[0].outages, "2 outages");
        assert_eq!(v.cards[0].availability, "Mains availability: 98.1 %");
        assert_eq!(v.rows[0][1], "ongoing · 1 min 30 s");
        assert_eq!(v.rows[0][8], "start not observed");
        assert_eq!(v.rows[1][2], "100 → 81 %");
        assert_eq!(v.rows[1][5], "96 W (20 %)");
        assert_eq!(v.rows[1][7], "1 h 08 min");
        assert_eq!(v.rows[1].len(), OUTAGE_COLUMNS.len());
        assert_eq!(v.month_labels, ["November 2026"]);
        assert!(v.summary.starts_with("2 outages"));
    }
}
