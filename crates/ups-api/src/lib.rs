//! Wire types of the collector API, version 1.
//!
//! | Method | Path | Response |
//! |---|---|---|
//! | GET | `/api/v1/status` | [`StatusResponse`] |
//! | GET | `/api/v1/history?minutes=15&points=300` | [`HistoryResponse`] |
//! | GET | `/api/v1/events?limit=50` | [`EventsResponse`] |
//! | GET (WebSocket) | `/api/v1/live` | stream of [`LiveMessage`] (JSON text frames) |
//! | POST | `/api/v1/settings/beeper` with [`BeeperRequest`] | [`SettingResponse`] |
//! | GET | `/api/v1/outages?days=90&limit=500` | [`OutagesResponse`] |
//! | GET | `/api/v1/stats` | [`StatsResponse`] |
//! | GET | `/api/v1/report?month=2026-11&format=html` (or `csv`) | report document |
//!
//! The collector binds to `127.0.0.1:47800` by default (see ADR-0002).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
pub use ups_model::{
    DailyCoverage, DeviceInfo, Event, EventKind, HistoryPoint, Outage, PeriodStats, Severity,
    Snapshot, UpsStatus, vars,
};

/// Default collector port.
pub const DEFAULT_PORT: u16 = 47_800;
/// URL prefix of this API version.
pub const PREFIX: &str = "/api/v1";

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
}
