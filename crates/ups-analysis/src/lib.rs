//! Pure analysis on top of the telemetry stream (no I/O, fully unit-tested):
//!
//! * [`OutageTracker`]: turns snapshots into [`Outage`] records. It merges
//!   flapping, handles monitoring gaps (collector down, PC shut down) and
//!   measures recharge time and delivered energy.
//! * [`CoverageTracker`]: per local day, how long the UPS was monitored and how
//!   much of that was on battery. This is the basis for availability figures.
//! * [`stats`]: period statistics (today, last 7/30 days, months).
//! * [`report`]: monthly HTML and CSV reports.
//!
//! [`Outage`]: ups_model::Outage

mod coverage;
pub mod format;
pub mod report;
pub mod stats;
mod tracker;

pub use coverage::CoverageTracker;
pub use tracker::{OutageTracker, TrackerConfig};
