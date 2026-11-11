//! Turns state changes into [`Event`]s (pure and unit-tested).
//!
//! M1 reports every transition as soon as it is seen. Outage records with
//! debouncing, durations and statistics are M2's outage engine. It will build
//! on these same events and on the stored samples.

use chrono::{DateTime, Utc};
use ups_model::{DeviceInfo, Event, EventKind, Snapshot, UpsStatus};

#[derive(Debug, Default)]
pub struct EventDetector {
    prev: Option<UpsStatus>,
    on_battery_since: Option<DateTime<Utc>>,
    /// `Some(true)` once connected, `Some(false)` after a loss was reported.
    connected: Option<bool>,
}

impl EventDetector {
    pub fn on_connected(&mut self, ts: DateTime<Utc>, device: &DeviceInfo) -> Vec<Event> {
        let was = self.connected.replace(true);
        match was {
            Some(false) => vec![Event::new(
                ts,
                EventKind::CommunicationRestored,
                format!("Communication with {} restored", device.display_name()),
            )],
            _ => Vec::new(),
        }
    }

    pub fn on_disconnected(&mut self, ts: DateTime<Utc>, reason: &str) -> Vec<Event> {
        // Report a loss only after a successful connection, and only once.
        if self.connected == Some(true) {
            self.connected = Some(false);
            vec![Event::new(
                ts,
                EventKind::CommunicationLost,
                format!("Communication with the UPS lost: {reason}"),
            )]
        } else {
            Vec::new()
        }
    }

    pub fn on_snapshot(&mut self, s: &Snapshot) -> Vec<Event> {
        let Some(now) = s.status else {
            return Vec::new();
        };
        let prev = self.prev.replace(now);
        let before = prev.unwrap_or_default();
        let rose = |f: UpsStatus| now.contains(f) && !before.contains(f);
        let fell = |f: UpsStatus| !now.contains(f) && before.contains(f);
        let mut events = Vec::new();

        if rose(UpsStatus::OB) {
            self.on_battery_since = Some(s.ts);
            let at_start = if prev.is_none() {
                " (already on battery when monitoring started)"
            } else {
                ""
            };
            events.push(Event::new(
                s.ts,
                EventKind::PowerLost,
                format!(
                    "Mains power lost, running on battery{at_start}: {}",
                    battery_summary(s)
                ),
            ));
        }
        if fell(UpsStatus::OB) && now.contains(UpsStatus::OL) {
            let duration = self
                .on_battery_since
                .take()
                .map(|since| format!(" after {}", human_duration(s.ts - since)))
                .unwrap_or_default();
            events.push(Event::new(
                s.ts,
                EventKind::PowerRestored,
                format!("Mains power restored{duration}: {}", battery_summary(s)),
            ));
        }
        if rose(UpsStatus::LB) {
            events.push(Event::new(
                s.ts,
                EventKind::LowBattery,
                format!("Low battery: {}", battery_summary(s)),
            ));
        }
        if fell(UpsStatus::LB) {
            events.push(Event::new(
                s.ts,
                EventKind::LowBatteryCleared,
                "Low battery condition cleared",
            ));
        }
        if rose(UpsStatus::RB) {
            events.push(Event::new(
                s.ts,
                EventKind::ReplaceBattery,
                "The UPS reports that the battery needs replacement",
            ));
        }
        if rose(UpsStatus::OVER) {
            let load = s
                .load_pct
                .map(|l| format!(" (load {l:.0} %)"))
                .unwrap_or_default();
            events.push(Event::new(
                s.ts,
                EventKind::Overload,
                format!("UPS overloaded{load}"),
            ));
        }
        if fell(UpsStatus::OVER) {
            events.push(Event::new(
                s.ts,
                EventKind::OverloadCleared,
                "Overload cleared",
            ));
        }
        events
    }
}

/// `"battery 87 %, about 25 min left"`, with whatever is known.
fn battery_summary(s: &Snapshot) -> String {
    let mut parts = Vec::new();
    if let Some(c) = s.battery_charge_pct {
        parts.push(format!("battery {c:.0} %"));
    }
    if let Some(r) = s.battery_runtime_s {
        parts.push(format!(
            "about {} left",
            human_duration(chrono::Duration::seconds(i64::from(r)))
        ));
    }
    if parts.is_empty() {
        "battery state unknown".into()
    } else {
        parts.join(", ")
    }
}

/// `"35 s"`, `"2 min 10 s"`, `"1 h 05 min"`.
pub fn human_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min {:02} s", secs / 60, secs % 60),
        _ => format!("{} h {:02} min", secs / 3600, (secs % 3600) / 60),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use super::*;

    fn snap(secs: i64, status: UpsStatus, charge: f32) -> Snapshot {
        let mut s = Snapshot::empty(
            Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap() + Duration::seconds(secs),
        );
        s.status = Some(status);
        s.battery_charge_pct = Some(charge);
        s.battery_runtime_s = Some(1500);
        s
    }

    fn kinds(events: &[Event]) -> Vec<EventKind> {
        events.iter().map(|e| e.kind).collect()
    }

    #[test]
    fn reports_an_outage_with_duration() {
        let mut d = EventDetector::default();
        assert_eq!(kinds(&d.on_snapshot(&snap(0, UpsStatus::OL, 100.0))), []);
        let lost = d.on_snapshot(&snap(10, UpsStatus::OB | UpsStatus::DISCHRG, 99.0));
        assert_eq!(kinds(&lost), [EventKind::PowerLost]);
        assert_eq!(
            lost[0].message,
            "Mains power lost, running on battery: battery 99 %, about 25 min 00 s left"
        );
        assert_eq!(
            kinds(&d.on_snapshot(&snap(11, UpsStatus::OB | UpsStatus::DISCHRG, 98.0))),
            []
        );
        let low = d.on_snapshot(&snap(70, UpsStatus::OB | UpsStatus::LB, 15.0));
        assert_eq!(kinds(&low), [EventKind::LowBattery]);
        let back = d.on_snapshot(&snap(135, UpsStatus::OL | UpsStatus::CHRG, 14.0));
        assert_eq!(
            kinds(&back),
            [EventKind::PowerRestored, EventKind::LowBatteryCleared]
        );
        assert!(
            back[0]
                .message
                .starts_with("Mains power restored after 2 min 05 s")
        );
    }

    #[test]
    fn notices_on_battery_at_startup_and_ignores_unknown_status() {
        let mut d = EventDetector::default();
        assert_eq!(kinds(&d.on_snapshot(&Snapshot::empty(Utc::now()))), []);
        let e = d.on_snapshot(&snap(0, UpsStatus::OB, 80.0));
        assert_eq!(kinds(&e), [EventKind::PowerLost]);
        assert!(e[0].message.contains("already on battery"));
    }

    #[test]
    fn reports_overload_and_replace_battery_edges() {
        let mut d = EventDetector::default();
        d.on_snapshot(&snap(0, UpsStatus::OL, 100.0));
        let e = d.on_snapshot(&snap(
            1,
            UpsStatus::OL | UpsStatus::OVER | UpsStatus::RB,
            100.0,
        ));
        assert_eq!(kinds(&e), [EventKind::ReplaceBattery, EventKind::Overload]);
        assert_eq!(
            kinds(&d.on_snapshot(&snap(
                2,
                UpsStatus::OL | UpsStatus::OVER | UpsStatus::RB,
                100.0
            ))),
            []
        );
        let e = d.on_snapshot(&snap(3, UpsStatus::OL | UpsStatus::RB, 100.0));
        assert_eq!(kinds(&e), [EventKind::OverloadCleared]);
    }

    #[test]
    fn reports_communication_loss_once_and_recovery() {
        let mut d = EventDetector::default();
        let dev = DeviceInfo::default();
        // Failing before the first connection is not an event.
        assert_eq!(kinds(&d.on_disconnected(Utc::now(), "not found")), []);
        assert_eq!(kinds(&d.on_connected(Utc::now(), &dev)), []);
        assert_eq!(
            kinds(&d.on_disconnected(Utc::now(), "unplugged")),
            [EventKind::CommunicationLost]
        );
        assert_eq!(kinds(&d.on_disconnected(Utc::now(), "still unplugged")), []);
        assert_eq!(
            kinds(&d.on_connected(Utc::now(), &dev)),
            [EventKind::CommunicationRestored]
        );
    }

    #[test]
    fn formats_durations() {
        assert_eq!(human_duration(Duration::seconds(35)), "35 s");
        assert_eq!(human_duration(Duration::seconds(130)), "2 min 10 s");
        assert_eq!(human_duration(Duration::seconds(3900)), "1 h 05 min");
        assert_eq!(human_duration(Duration::seconds(-5)), "0 s");
    }
}
