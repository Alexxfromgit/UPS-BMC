//! Wire types of the collector API, version 1.
//!
//! | Method | Path | Response |
//! |---|---|---|
//! | GET | `/api/v1/status` | [`StatusResponse`] |
//! | GET | `/api/v1/history?minutes=15&points=300` | [`HistoryResponse`] |
//! | GET | `/api/v1/events?limit=50` | [`EventsResponse`] |
//! | GET (WebSocket) | `/api/v1/live` | stream of [`LiveMessage`] (JSON text frames) |
//! | POST | `/api/v1/settings/beeper` with [`BeeperRequest`] | [`SettingResponse`] |
//! | POST | `/api/v1/settings/assumed-load` with [`AssumedLoadRequest`] | [`SettingResponse`] |
//! | GET | `/api/v1/outages?days=90&limit=500` | [`OutagesResponse`] |
//! | GET | `/api/v1/stats` | [`StatsResponse`] |
//! | GET | `/api/v1/report?month=2026-11&format=html` (or `csv`) | report document |
//! | GET | `/api/v1/health` | [`HealthReport`] |
//! | POST | `/api/v1/settings/battery` with [`BatteryRequest`] | [`SettingResponse`] |
//! | POST | `/api/v1/battery/test` with [`RuntimeTestRequest`] | [`SettingResponse`] |
//! | POST | `/api/v1/shutdown` with `{}` | [`SettingResponse`]; the collector stops |
//!
//! **Access** (ADR-0005). Requests carrying an `Origin` header are always
//! refused, so web pages can't use the API. Otherwise a request is allowed
//! when either
//! * it comes from this computer (loopback peer) with a loopback `Host`
//!   (`127.0.0.1:<port>`, `localhost:<port>`, `[::1]:<port>`), or
//! * it carries `Authorization: Bearer <token>` with the collector's token.
//!
//! The collector binds to `127.0.0.1:47800` by default (ADR-0002). Listening
//! on other addresses (e.g. a Raspberry Pi serving the LAN) requires a token.
//! `POST /api/v1/shutdown` is only accepted from this computer.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
pub use ups_model::{
    Confidence, DailyCoverage, DeviceInfo, Discharge, Event, EventKind, HealthReport,
    HealthVerdict, HistoryPoint, Outage, PeriodStats, RuntimeTestPhase, RuntimeTestStatus,
    Severity, Snapshot, UpsStatus, vars,
};

/// Default collector port.
pub const DEFAULT_PORT: u16 = 47_800;
/// URL prefix of this API version.
pub const PREFIX: &str = "/api/v1";

/// Environment variable holding the API token, for the UI and the collector.
pub const TOKEN_ENV: &str = "UPS_BMC_TOKEN";

/// The title of a notification about `event` (desktop toast, ntfy, Telegram,
/// email), or `None` for events that don't deserve one.
#[must_use]
pub fn notification_title(event: &Event) -> Option<&'static str> {
    Some(match event.kind {
        EventKind::PowerLost => "Power outage",
        EventKind::PowerRestored => "Power restored",
        EventKind::LowBattery => "UPS battery low!",
        EventKind::CommunicationLost => "UPS connection lost",
        EventKind::CommunicationRestored => "UPS connection restored",
        EventKind::Overload => "UPS overloaded",
        EventKind::ReplaceBattery => "Replace the UPS battery",
        EventKind::RuntimeTestRestorePower => "Runtime test: plug the UPS back in",
        EventKind::RuntimeTest if event.message.starts_with("Runtime test finished") => {
            "Runtime test finished"
        }
        _ => return None,
    })
}

/// Connection between the collector and the UPS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Connection {
    /// The collector is starting or looking for the device.
    Connecting,
    Connected,
    Disconnected {
        reason: String,
    },
}

/// Current state of the collector and the UPS.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatusResponse {
    pub collector_version: String,
    /// Source kind (`hid`, `sim`, ...).
    pub source: String,
    pub poll_interval_ms: u64,
    pub started_at: DateTime<Utc>,
    pub connection: Connection,
    pub device: Option<DeviceInfo>,
    /// Settings the connected UPS lets UPS-BMC change (e.g. `beeper`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// The user's estimate of the protected load, used when the UPS reports
    /// 0 (below its measuring range).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assumed_load_w: Option<f32>,
    /// The guided runtime test in progress, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_test: Option<RuntimeTestStatus>,
    /// Latest reading. Stale when `connection` is not `Connected`.
    pub snapshot: Option<Snapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryResponse {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    pub points: Vec<HistoryPoint>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventsResponse {
    /// Newest first.
    pub events: Vec<Event>,
}

/// Recorded outages, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutagesResponse {
    pub outages: Vec<Outage>,
}

/// Outage statistics for standard periods, and the months with data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatsResponse {
    /// Today, last 7 days, last 30 days, this month.
    pub periods: Vec<PeriodStats>,
    /// `YYYY-MM` months with monitoring data, newest first.
    pub months: Vec<String>,
}

/// Body of `POST /api/v1/settings/beeper`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BeeperRequest {
    pub enabled: bool,
}

/// Body of `POST /api/v1/settings/assumed-load`: watts (1–2000), or null to clear.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AssumedLoadRequest {
    pub watts: Option<f32>,
}

/// Body of `POST /api/v1/settings/battery`: when the current battery was
/// installed (`YYYY-MM-DD`), or null if unknown. Health estimation only uses
/// outages since then. With `replaced_now: true` the collector records today
/// and the exact current time instead (so earlier outages that day, on the
/// old battery, don't count).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatteryRequest {
    pub installed: Option<NaiveDate>,
    #[serde(default)]
    pub replaced_now: bool,
}

/// Body of `POST /api/v1/battery/test`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RuntimeTestRequest {
    /// Arm a test; the user then unplugs the UPS. UPS-BMC asks for mains
    /// power to be restored when the charge reaches `stop_at_pct` (20–90).
    Start {
        stop_at_pct: f32,
    },
    Cancel,
}

/// Result of a setting change. Errors use HTTP status codes with a text body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingResponse {
    pub ok: bool,
    pub message: String,
}

/// A message on the live WebSocket. The first message after connecting is
/// always a `status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LiveMessage {
    Status(Box<StatusResponse>),
    Event(Event),
    /// An outage started, changed (every poll while ongoing) or ended.
    Outage(Outage),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_messages_are_tagged() {
        let status = StatusResponse {
            collector_version: "0.1.0".into(),
            source: "sim".into(),
            poll_interval_ms: 1000,
            started_at: DateTime::default(),
            connection: Connection::Disconnected {
                reason: "no UPS".into(),
            },
            device: None,
            capabilities: Vec::new(),
            assumed_load_w: None,
            runtime_test: None,
            snapshot: None,
        };
        let json = serde_json::to_string(&LiveMessage::Status(Box::new(status.clone()))).unwrap();
        assert!(json.starts_with(r#"{"type":"status","#), "{json}");
        assert!(json.contains(r#""connection":{"state":"disconnected","reason":"no UPS"}"#));
        let back: LiveMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, LiveMessage::Status(Box::new(status)));

        let event = Event::new(DateTime::default(), EventKind::PowerLost, "x");
        let json = serde_json::to_string(&LiveMessage::Event(event)).unwrap();
        assert!(json.starts_with(r#"{"type":"event","#), "{json}");
    }

    #[test]
    fn runtime_test_requests_are_tagged() {
        let json = serde_json::to_string(&RuntimeTestRequest::Start { stop_at_pct: 50.0 }).unwrap();
        assert_eq!(json, r#"{"action":"start","stop_at_pct":50.0}"#);
        let back: RuntimeTestRequest = serde_json::from_str(r#"{"action":"cancel"}"#).unwrap();
        assert_eq!(back, RuntimeTestRequest::Cancel);
        let b: BatteryRequest = serde_json::from_str(r#"{"installed":"2024-03-15"}"#).unwrap();
        assert_eq!(b.installed, NaiveDate::from_ymd_opt(2024, 3, 15));
    }
}
