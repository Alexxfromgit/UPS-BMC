//! Source-independent UPS domain model.
//!
//! Every telemetry source (direct HID on Windows, Windows battery API, NUT on a
//! Raspberry Pi, simulator, replay) maps its data into these types. The variable
//! vocabulary deliberately follows Network UPS Tools (NUT) naming so the same
//! names are used everywhere — in storage, the API and the UI.

mod snapshot;
mod status;
pub mod vars;

pub use snapshot::Snapshot;
pub use status::UpsStatus;
