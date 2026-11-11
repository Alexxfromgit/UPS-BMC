use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What happened. Stable `snake_case` names are used in storage and the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    CollectorStarted,
    CollectorStopped,
    CommunicationLost,
    CommunicationRestored,
    /// Mains failed: the UPS is running on battery.
    PowerLost,
    /// Mains is back.
    PowerRestored,
    LowBattery,
    LowBatteryCleared,
    ReplaceBattery,
    Overload,
    OverloadCleared,
    /// A UPS setting was changed from UPS-BMC (ADR-0003).
    SettingChanged,
}

impl EventKind {
    #[must_use]
    pub fn severity(self) -> Severity {
        match self {
            Self::PowerLost | Self::Overload | Self::ReplaceBattery | Self::CommunicationLost => {
                Severity::Warning
            }
            Self::LowBattery => Severity::Critical,
            _ => Severity::Info,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CollectorStarted => "collector_started",
            Self::CollectorStopped => "collector_stopped",
            Self::CommunicationLost => "communication_lost",
            Self::CommunicationRestored => "communication_restored",
            Self::PowerLost => "power_lost",
            Self::PowerRestored => "power_restored",
            Self::LowBattery => "low_battery",
            Self::LowBatteryCleared => "low_battery_cleared",
            Self::ReplaceBattery => "replace_battery",
            Self::Overload => "overload",
            Self::OverloadCleared => "overload_cleared",
            Self::SettingChanged => "setting_changed",
        }
    }

    /// Inverse of [`EventKind::as_str`].
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

const ALL: [EventKind; 12] = [
    EventKind::CollectorStarted,
    EventKind::CollectorStopped,
    EventKind::CommunicationLost,
    EventKind::CommunicationRestored,
    EventKind::PowerLost,
    EventKind::PowerRestored,
    EventKind::LowBattery,
    EventKind::LowBatteryCleared,
    EventKind::ReplaceBattery,
    EventKind::Overload,
    EventKind::OverloadCleared,
    EventKind::SettingChanged,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Critical,
}

/// A recorded event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Storage ID (0 before the event is stored).
    pub id: i64,
    pub ts: DateTime<Utc>,
    pub kind: EventKind,
    pub severity: Severity,
    /// Human-readable description.
    pub message: String,
}

impl Event {
    #[must_use]
    pub fn new(ts: DateTime<Utc>, kind: EventKind, message: impl Into<String>) -> Self {
        Self {
            id: 0,
            ts,
            kind,
            severity: kind.severity(),
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip_through_their_names() {
        for kind in ALL {
            assert_eq!(EventKind::parse(kind.as_str()), Some(kind));
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
        }
        assert_eq!(EventKind::parse("nope"), None);
    }
}
