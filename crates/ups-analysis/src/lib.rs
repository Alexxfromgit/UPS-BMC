//! Pure analysis on top of the telemetry stream (no I/O, fully unit-tested):
//!
//! * [`OutageTracker`]: turns snapshots into [`Outage`] records. It merges
//!   flapping, handles monitoring gaps (collector down, PC shut down) and
//!   measures recharge time and delivered energy.
//! * [`CoverageTracker`]: per local day, how long the UPS was monitored and how
//!   much of that was on battery. This is the basis for availability figures.
//! * [`stats`]: period statistics (today, last 7/30 days, months).
//! * [`report`]: monthly HTML and CSV reports.
//! * [`health`]: battery health from recorded discharges (M3).
//! * [`RuntimeTest`]: the guided runtime test (M3).
//!
//! [`Outage`]: ups_model::Outage

mod coverage;
pub mod format;
pub mod health;
pub mod report;
mod runtime_test;
pub mod stats;
mod tracker;

pub use coverage::CoverageTracker;
pub use runtime_test::{RuntimeTest, TestNotice};
pub use tracker::{OutageTracker, TrackerConfig};
