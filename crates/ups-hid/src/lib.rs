//! HID support for UPS devices.
//!
//! * [`ReportDescriptor`]: a dependency-free, defensive HID report descriptor
//!   parser. It turns the raw descriptor into a flat list of [`Field`]s with
//!   exact bit positions, so feature and input reports can be decoded without
//!   OS help.
//! * [`usages`]: names for the HID Power Device (0x84) and Battery System
//!   (0x85) usage pages from the USB "HID Usage Tables for Power Devices".
//! * [`units`]: readable names and exponent normalisation for the HID unit
//!   codes that UPSes commonly use.
//!
//! This crate does no I/O. It never builds output or feature reports to
//! send: UPS-BMC is read-only towards the UPS (see ADR-0001).

mod descriptor;
pub mod units;
pub mod usages;

pub use descriptor::{
    Collection, Field, MAX_ARRAY_USAGES_TOTAL, MAX_COLLECTION_DEPTH, MAX_COLLECTIONS, MAX_FIELDS,
    MAX_LOCAL_USAGES, MAX_REPORT_BITS, MAX_REPORT_SIZE_BITS, ParseError, ReportDescriptor,
    ReportKind, ReportLayout, Usage,
};
