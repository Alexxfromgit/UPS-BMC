//! The ingest thread: the single writer. It stores samples and events, runs
//! the event detector, updates the shared status and broadcasts live messages.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use ups_api::{Connection, LiveMessage, StatusResponse};
use ups_model::{Event, EventKind};
use ups_store::Store;

use crate::detector::EventDetector;
use crate::outages::OutageAnalysis;
use crate::poller::PollerMsg;

const PRUNE_EVERY: Duration = Duration::from_secs(3600);

pub struct Ingest {
    pub store: Store,
    pub status: Arc<RwLock<StatusResponse>>,
    pub live: broadcast::Sender<LiveMessage>,
    pub retention: chrono::Duration,
}

impl Ingest {
    pub fn spawn(self, rx: Receiver<PollerMsg>) -> std::io::Result<JoinHandle<()>> {
        std::thread::Builder::new()
            .name("ups-ingest".into())
            .spawn(move || self.run(&rx))
    }

    fn run(mut self, rx: &Receiver<PollerMsg>) {
        let mut detector = EventDetector::default();
        let mut analysis = OutageAnalysis::start(&mut self.store);
        let mut last_prune: Option<Instant> = None;
        self.record(Event::new(
            Utc::now(),
            EventKind::CollectorStarted,
            format!("Collector {} started", env!("CARGO_PKG_VERSION")),
        ));

        // Ends when the poller stops and drops its sender.
        for msg in rx {
            let now = Utc::now();
            let events = match msg {
                PollerMsg::Connected {
                    device,
                    capabilities,
                } => {
                    if let Err(e) = self.store.set_device(&device) {
                        warn!(error = %e, "could not store device info");
                    }
                    let events = detector.on_connected(now, &device);
                    self.update(|s| {
                        s.connection = Connection::Connected;
                        s.device = Some(device);
                        s.capabilities = capabilities;
                    });
                    events
                }
                PollerMsg::Disconnected(reason) => {
                    let events = detector.on_disconnected(now, &reason);
                    self.update(|s| {
                        s.connection = Connection::Disconnected { reason };
                        s.capabilities.clear();
                    });
                    events
                }
                PollerMsg::SettingChanged(message) => {
                    vec![Event::new(now, EventKind::SettingChanged, message)]
                }
                PollerMsg::Snapshot(snapshot) => {
                    if let Err(e) = self.store.insert_sample(&snapshot) {
                        error!(error = %e, "could not store sample");
                    }
                    let events = detector.on_snapshot(&snapshot);
                    for outage in analysis.on_snapshot(&mut self.store, &snapshot) {
                        let _ = self.live.send(LiveMessage::Outage(outage));
                    }
                    self.update(|s| s.snapshot = Some(snapshot));
                    events
                }
            };
            for event in events {
                self.record(event);
            }
            if last_prune.is_none_or(|t| t.elapsed() >= PRUNE_EVERY) {
                last_prune = Some(Instant::now());
                match self.store.prune_samples(Utc::now() - self.retention) {
                    Ok(0) => {}
                    Ok(n) => info!(removed = n, "pruned old samples"),
                    Err(e) => warn!(error = %e, "pruning failed"),
                }
            }
        }

        analysis.flush(&mut self.store);
        self.record(Event::new(
            Utc::now(),
            EventKind::CollectorStopped,
            "Collector stopped",
        ));
    }

    /// Applies a change to the shared status and broadcasts the new status.
    fn update(&self, change: impl FnOnce(&mut StatusResponse)) {
        let snapshot = {
            let mut status = self
                .status
                .write()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            change(&mut status);
            status.clone()
        };
        // No receivers is fine (no UI connected).
        let _ = self.live.send(LiveMessage::Status(Box::new(snapshot)));
    }

    fn record(&mut self, event: Event) {
        info!(kind = event.kind.as_str(), "{}", event.message);
        let event = match self.store.insert_event(&event) {
            Ok(stored) => stored,
            Err(e) => {
                error!(error = %e, "could not store event");
                event
            }
        };
        let _ = self.live.send(LiveMessage::Event(event));
    }
}
