//! The polling thread: owns the telemetry source (HID handles stay on one
//! thread), connects, polls at a fixed rate and reconnects after errors.
//! Setting changes from the API are executed here too, between polls, so the
//! device is never accessed from two threads.

use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tokio::sync::oneshot;
use tracing::{info, warn};
use ups_model::{DeviceInfo, Snapshot};
use ups_source::{Setting, SourceConfig, TelemetrySource};

/// Delay between reconnection attempts.
const RECONNECT_DELAY: Duration = Duration::from_secs(3);

/// Messages from the poller to the ingest thread.
#[derive(Debug)]
pub enum PollerMsg {
    Connected {
        device: DeviceInfo,
        capabilities: Vec<String>,
    },
    Snapshot(Snapshot),
    Disconnected(String),
    /// A setting was changed and verified (human-readable description).
    SettingChanged(String),
    /// From the API: set or clear the load estimate (handled by ingest).
    SetAssumedLoad {
        watts: Option<f32>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// From the API: set or clear the battery installation date.
    SetBatteryInstalled {
        date: Option<chrono::NaiveDate>,
        /// The exact time, for "replaced now".
        at: Option<chrono::DateTime<chrono::Utc>>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// From the API: start or cancel a guided runtime test.
    RuntimeTest {
        request: ups_api::RuntimeTestRequest,
        reply: oneshot::Sender<Result<String, String>>,
    },
}

/// Requests to the poller.
#[derive(Debug)]
pub enum Control {
    Apply {
        setting: Setting,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Stop,
}

fn describe(setting: Setting) -> String {
    match setting {
        Setting::Beeper { enabled: true } => "UPS beeper enabled from UPS-BMC".into(),
        Setting::Beeper { enabled: false } => "UPS beeper disabled from UPS-BMC".into(),
    }
}

pub fn spawn(
    config: SourceConfig,
    interval: Duration,
    tx: Sender<PollerMsg>,
    control: Receiver<Control>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("ups-poller".into())
        .spawn(move || {
            let mut poller = Poller {
                source: config.create(),
                tx,
                control,
            };
            poller.run(interval);
        })
}

struct Poller {
    source: Box<dyn TelemetrySource>,
    tx: Sender<PollerMsg>,
    control: Receiver<Control>,
}

impl Poller {
    /// Waits up to `d`, handling setting requests meanwhile. Returns `true`
    /// when the poller must stop.
    fn wait(&mut self, d: Duration) -> bool {
        let deadline = Instant::now() + d;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.control.recv_timeout(remaining) {
                // The caller gave up waiting (timeout): don't change the UPS
                // behind its back later (review M1-6).
                Ok(Control::Apply { setting, reply }) if reply.is_closed() => {
                    warn!(?setting, "setting request dropped: the caller timed out");
                }
                Ok(Control::Apply { setting, reply }) => {
                    let result = self.source.apply(setting).map_err(|e| e.to_string());
                    match &result {
                        Ok(()) => {
                            info!(?setting, "setting applied and verified");
                            let _ = self.tx.send(PollerMsg::SettingChanged(describe(setting)));
                        }
                        Err(e) => warn!(?setting, error = %e, "setting change failed"),
                    }
                    let _ = reply.send(result);
                }
                Ok(Control::Stop) | Err(RecvTimeoutError::Disconnected) => return true,
                Err(RecvTimeoutError::Timeout) => return false,
            }
        }
    }

    fn send(&self, msg: PollerMsg) -> bool {
        self.tx.send(msg).is_ok()
    }

    fn run(&mut self, interval: Duration) {
        let mut last_error = String::new();
        loop {
            // Connect.
            match self.source.connect() {
                Ok(device) => {
                    last_error.clear();
                    let capabilities = self
                        .source
                        .capabilities()
                        .into_iter()
                        .map(str::to_owned)
                        .collect();
                    if !self.send(PollerMsg::Connected {
                        device,
                        capabilities,
                    }) {
                        return;
                    }
                }
                Err(e) => {
                    let reason = e.to_string();
                    if reason != last_error {
                        warn!(%reason, "UPS not available, retrying every {}s", RECONNECT_DELAY.as_secs());
                        if !self.send(PollerMsg::Disconnected(reason.clone())) {
                            return;
                        }
                        last_error = reason;
                    }
                    if self.wait(RECONNECT_DELAY) {
                        return;
                    }
                    continue;
                }
            }

            // Poll at a fixed rate until an error.
            let mut next = Instant::now();
            loop {
                match self.source.poll() {
                    Ok(snapshot) => {
                        if !self.send(PollerMsg::Snapshot(snapshot)) {
                            return;
                        }
                    }
                    Err(e) => {
                        let reason = e.to_string();
                        info!(%reason, "UPS connection lost");
                        if !self.send(PollerMsg::Disconnected(reason.clone())) {
                            return;
                        }
                        last_error = reason;
                        break;
                    }
                }
                next += interval;
                let now = Instant::now();
                if next < now {
                    next = now; // fell behind (slow device): don't try to catch up
                }
                if self.wait(next - now) {
                    return;
                }
            }
            if self.wait(RECONNECT_DELAY) {
                return;
            }
        }
    }
}
