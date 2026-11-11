//! Telemetry sources.
//!
//! A [`TelemetrySource`] produces [`Snapshot`]s from one UPS. Sources are
//! synchronous and are driven by a dedicated polling thread in the collector:
//! [`TelemetrySource::connect`], then [`TelemetrySource::poll`] in a loop, and
//! `connect` again after an error.
//!
//! * [`hid::HidSource`]: reads a USB HID Power Device directly (read-only, see
//!   ADR-0001).
//! * [`sim::SimSource`]: a deterministic simulated UPS with periodic outages,
//!   for UI development and tests.

pub mod hid;
pub mod sim;

use std::time::Duration;

use ups_model::{DeviceInfo, Snapshot};

/// Errors a source can report. All of them mean "try to reconnect later".
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("no matching UPS found")]
    NotFound,
    #[error("not connected")]
    NotConnected,
    #[error("device not supported: {0}")]
    Unsupported(String),
    #[error("I/O error: {0}")]
    Io(String),
}

/// A UPS setting UPS-BMC may change (ADR-0003 allowlist).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setting {
    /// Audible alarm on/off.
    Beeper { enabled: bool },
}

/// Capability name for [`Setting::Beeper`] (used in the API).
pub const CAP_BEEPER: &str = "beeper";

/// A UPS telemetry source.
pub trait TelemetrySource {
    /// Short source kind (`"hid"`, `"sim"`, ...).
    fn kind(&self) -> &'static str;

    /// (Re)connects to the device and returns its identity.
    fn connect(&mut self) -> Result<DeviceInfo, SourceError>;

    /// Reads one snapshot. On error the source is disconnected; call
    /// [`TelemetrySource::connect`] again.
    fn poll(&mut self) -> Result<Snapshot, SourceError>;

    /// Settings the connected device lets UPS-BMC change (e.g. [`CAP_BEEPER`]).
    fn capabilities(&self) -> Vec<&'static str> {
        Vec::new()
    }

    /// Changes a setting and verifies it. The next snapshot reflects it.
    fn apply(&mut self, setting: Setting) -> Result<(), SourceError> {
        Err(SourceError::Unsupported(format!(
            "{setting:?} is not supported by the {} source",
            self.kind()
        )))
    }
}

/// Which source to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceConfig {
    Hid(hid::HidConfig),
    Sim,
}

impl SourceConfig {
    /// Builds the configured source. HID sources must be created on the
    /// thread that polls them.
    #[must_use]
    pub fn create(&self) -> Box<dyn TelemetrySource> {
        match self {
            Self::Hid(cfg) => Box::new(hid::HidSource::new(cfg.clone())),
            Self::Sim => Box::new(sim::SimSource::new()),
        }
    }

    /// Default polling interval for this source.
    #[must_use]
    pub fn default_poll_interval(&self) -> Duration {
        Duration::from_secs(1)
    }
}
