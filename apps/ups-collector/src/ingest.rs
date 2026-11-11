//! The ingest thread: the single writer. It stores samples and events, runs
//! the event detector, updates the shared status and broadcasts live messages.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use chrono::Utc;
use tokio::sync::broadcast;
use tracing::{error, info, warn};
use ups_analysis::{RuntimeTest, TestNotice, format};
use ups_api::{Connection, LiveMessage, RuntimeTestRequest, StatusResponse};
use ups_model::{Event, EventKind, Outage, Snapshot};
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
    pub poll: Duration,
}

/// `meta` key holding the runtime test in progress (JSON), so a collector
/// restart doesn't lose it (review M3-3).
const META_RUNTIME_TEST: &str = "runtime_test";

impl Ingest {
    pub fn spawn(self, rx: Receiver<PollerMsg>) -> std::io::Result<JoinHandle<()>> {
        std::thread::Builder::new()
            .name("ups-ingest".into())
            .spawn(move || self.run(&rx))
    }

    fn run(mut self, rx: &Receiver<PollerMsg>) {
        let mut detector = EventDetector::default();
        let mut analysis = OutageAnalysis::start(&mut self.store, self.poll);
        let assumed = self.store.assumed_load_w().ok().flatten();
        analysis.set_assumed_power(assumed);
        self.update(|s| s.assumed_load_w = assumed);
        let mut last_prune: Option<Instant> = None;
        let mut test = self.restore_test();
        let mut latest: Option<Snapshot> = None;
        self.record_start();

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
                PollerMsg::SetAssumedLoad { watts, reply } => {
                    let result = self
                        .store
                        .set_assumed_load_w(watts)
                        .map_err(|e| e.to_string());
                    if result.is_ok() {
                        analysis.set_assumed_power(watts);
                        self.update(|s| s.assumed_load_w = watts);
                    }
                    let _ = reply.send(result);
                    Vec::new()
                }
                PollerMsg::SetBatteryInstalled { date, at, reply } => {
                    let result = self
                        .store
                        .set_battery_installed(date, at)
                        .map_err(|e| e.to_string());
                    let ok = result.is_ok();
                    let _ = reply.send(result);
                    let message = date.map_or_else(
                        || "Battery installation date cleared".to_owned(),
                        |d| format!("Battery installation date set to {}", d.format("%d.%m.%Y")),
                    );
                    if ok {
                        vec![Event::new(now, EventKind::SettingChanged, message)]
                    } else {
                        Vec::new()
                    }
                }
                PollerMsg::RuntimeTest { request, reply } => {
                    let was_discharging = test.discharging();
                    let (result, events) =
                        runtime_test_request(&mut test, request, latest.as_ref());
                    let _ = reply.send(result);
                    if was_discharging && test.status().is_none() {
                        // Cancelled mid-discharge: not a test (review M3-6).
                        analysis.set_testing(false);
                        for o in analysis.clear_test_tag(&mut self.store) {
                            let _ = self.live.send(LiveMessage::Outage(o));
                        }
                    }
                    self.save_test(&test);
                    events
                }
                PollerMsg::Snapshot(snapshot) => {
                    latest = Some(snapshot.clone());
                    self.on_snapshot(snapshot, &mut detector, &mut analysis, &mut test)
                }
            };
            for event in events {
                self.record(event);
            }
            if last_prune.is_none_or(|t| t.elapsed() >= PRUNE_EVERY) {
                last_prune = Some(Instant::now());
                self.prune();
            }
        }

        analysis.flush(&mut self.store);
        self.record(Event::new(
            Utc::now(),
            EventKind::CollectorStopped,
            "Collector stopped",
        ));
    }

    /// Stores a reading and runs event detection, outage analysis and the
    /// runtime test on it. Returns the events to record.
    fn on_snapshot(
        &mut self,
        snapshot: Snapshot,
        detector: &mut EventDetector,
        analysis: &mut OutageAnalysis,
        test: &mut RuntimeTest,
    ) -> Vec<Event> {
        if let Err(e) = self.store.insert_sample(&snapshot) {
            error!(error = %e, "could not store sample");
        }
        let mut events = detector.on_snapshot(&snapshot);
        let before = test.status().cloned();
        let notices = test.on_snapshot(&snapshot);
        analysis.set_testing(test.discharging());
        let mut finished: Option<Outage> = None;
        let mut changed = analysis.on_snapshot(&mut self.store, &snapshot);
        if notices.contains(&TestNotice::Interrupted) {
            // A mains blip: that short outage was not the test (review M3-6).
            changed.extend(analysis.clear_test_tag(&mut self.store));
        }
        for outage in changed {
            if outage.runtime_test && outage.end.is_some() {
                finished = Some(outage.clone());
            }
            let _ = self.live.send(LiveMessage::Outage(outage));
        }
        if test.status() != before.as_ref() {
            self.save_test(test);
        }
        events.extend(
            notices
                .into_iter()
                .map(|n| test_event(snapshot.ts, &n, finished.as_ref())),
        );
        self.update(|s| s.snapshot = Some(snapshot));
        events
    }

    /// The runtime test saved before a restart, if any (review M3-3).
    fn restore_test(&mut self) -> RuntimeTest {
        let mut test = RuntimeTest::default();
        if let Some(saved) = self
            .store
            .meta(META_RUNTIME_TEST)
            .ok()
            .flatten()
            .and_then(|j| serde_json::from_str(&j).ok())
        {
            test.restore(saved);
        }
        let status = test.status().cloned();
        self.update(|s| s.runtime_test = status);
        test
    }

    /// Records the start, noting when the previous run didn't stop cleanly
    /// (PC shutdown, crash or forced update; review M1-5).
    fn record_start(&mut self) {
        let clean_stop = self
            .store
            .recent_events(1)
            .ok()
            .and_then(|e| e.first().map(|e| e.kind))
            .is_none_or(|k| k == EventKind::CollectorStopped);
        let version = env!("CARGO_PKG_VERSION");
        self.record(Event::new(
            Utc::now(),
            EventKind::CollectorStarted,
            if clean_stop {
                format!("Collector {version} started")
            } else {
                format!(
                    "Collector {version} started (the previous run ended without a clean stop, \
                     e.g. a PC shutdown or an update)"
                )
            },
        ));
    }

    /// Saves the runtime test state and publishes it in the live status.
    fn save_test(&mut self, test: &RuntimeTest) {
        let status = test.status().cloned();
        let json = status.as_ref().and_then(|s| serde_json::to_string(s).ok());
        if let Err(e) = self.store.set_meta(META_RUNTIME_TEST, json) {
            warn!(error = %e, "could not save the runtime test state");
        }
        self.update(|s| s.runtime_test = status);
    }

    /// Deletes samples older than the retention period.
    fn prune(&mut self) {
        match self.store.prune_samples(Utc::now() - self.retention) {
            Ok(0) => {}
            Ok(n) => info!(removed = n, "pruned old samples"),
            Err(e) => warn!(error = %e, "pruning failed"),
        }
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

/// Handles a start/cancel request. Returns the reply and events to record.
fn runtime_test_request(
    test: &mut RuntimeTest,
    request: RuntimeTestRequest,
    latest: Option<&Snapshot>,
) -> (Result<String, String>, Vec<Event>) {
    let now = Utc::now();
    match request {
        RuntimeTestRequest::Start { stop_at_pct } => match test.arm(stop_at_pct, latest, now) {
            Ok(()) => {
                let msg = format!(
                    "Runtime test armed: unplug the UPS from the wall socket now. You will be asked \
                     to restore power at {stop_at_pct:.0} %."
                );
                (
                    Ok(msg.clone()),
                    vec![Event::new(now, EventKind::RuntimeTest, msg)],
                )
            }
            Err(e) => (Err(e), Vec::new()),
        },
        RuntimeTestRequest::Cancel => {
            if test.cancel() {
                let msg = "Runtime test cancelled".to_owned();
                (
                    Ok(msg.clone()),
                    vec![Event::new(now, EventKind::RuntimeTest, msg)],
                )
            } else {
                (Err("No runtime test is in progress.".into()), Vec::new())
            }
        }
    }
}

/// The event recorded for a runtime test notice.
fn test_event(ts: chrono::DateTime<Utc>, notice: &TestNotice, finished: Option<&Outage>) -> Event {
    let pct = |c: Option<f32>| c.map_or_else(|| "?".to_owned(), |c| format!("{c:.0} %"));
    match notice {
        TestNotice::Started { charge_pct } => Event::new(
            ts,
            EventKind::RuntimeTest,
            format!("Runtime test started at {}", pct(*charge_pct)),
        ),
        TestNotice::RestorePower {
            charge_pct,
            reminder,
        } => Event::new(
            ts,
            EventKind::RuntimeTestRestorePower,
            format!(
                "{}Runtime test reached {}: plug the UPS back into the wall socket now.",
                if *reminder { "Reminder: " } else { "" },
                pct(*charge_pct)
            ),
        ),
        TestNotice::Finished => {
            let summary = finished.and_then(|o| {
                let end = o.end?;
                let secs = (end - o.start).num_seconds();
                #[allow(clippy::cast_precision_loss)] // seconds
                let duration = format::duration(secs as f64);
                Some(match (o.charge_start_pct, o.charge_end_pct) {
                    (Some(a), Some(b)) => {
                        format!("{duration} on battery, {a:.0} → {b:.0} %")
                    }
                    _ => format!("{duration} on battery"),
                })
            });
            Event::new(
                ts,
                EventKind::RuntimeTest,
                match summary {
                    Some(s) => format!(
                        "Runtime test finished: {s}. See the Battery health tab for the result."
                    ),
                    None => "Runtime test finished".to_owned(),
                },
            )
        }
        TestNotice::Interrupted => Event::new(
            ts,
            EventKind::RuntimeTest,
            "Mains power came back within a minute: the runtime test is armed again. Unplug the \
             UPS to start it.",
        ),
        TestNotice::Expired => Event::new(
            ts,
            EventKind::RuntimeTest,
            "Runtime test cancelled: the UPS was not unplugged within 30 minutes",
        ),
    }
}
