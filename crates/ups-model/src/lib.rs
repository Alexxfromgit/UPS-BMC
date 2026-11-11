//! Source-independent UPS domain model.
//!
//! Every telemetry source (direct HID on Windows, NUT on a Raspberry Pi or
//! router, the simulator, replay) maps its data into these types. The variable
//! vocabulary follows Network UPS Tools (NUT) naming, so the same names are used
//! everywhere: in storage, the API and the UI.

mod device;
mod event;
mod history;
mod snapshot;
mod status;
pub mod vars;

pub use device::DeviceInfo;
pub use event::{Event, EventKind, Severity};
pub use history::HistoryPoint;
pub use snapshot::Snapshot;
pub use status::UpsStatus;
