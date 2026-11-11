//! The only place in UPS-BMC that opens a HID device.
//!
//! [`ReadOnlyHid`] exposes the read paths only: report descriptor,
//! `GET_FEATURE`, input reports and string descriptors. Opening devices any
//! other way, and every write method, is denied by `clippy.toml`
//! (disallowed-methods), so all device access has to go through this wrapper.
//! See ADR-0001.

use std::ffi::CStr;

use hidapi::{HidApi, HidDevice, HidResult};

/// A HID device handle that can only read.
#[derive(Debug)]
pub struct ReadOnlyHid(HidDevice);

impl ReadOnlyHid {
    /// Opens the HID collection at `path`.
    ///
    /// Note: on Windows hidapi asks for a read/write handle (falling back to
    /// read-only). The read-only guarantee is enforced in code, by this type,
    /// not by the OS.
    #[allow(clippy::disallowed_methods)] // the single sanctioned open
    pub fn open(api: &HidApi, path: &CStr) -> HidResult<Self> {
        api.open_path(path).map(Self)
    }

    /// Reads the report descriptor into `buf` and returns its length.
    pub fn report_descriptor(&self, buf: &mut [u8]) -> HidResult<usize> {
        self.0.get_report_descriptor(buf)
    }

    /// `GET_FEATURE`: `buf[0]` selects the report ID. The returned length
    /// includes that report ID byte.
    pub fn feature_report(&self, buf: &mut [u8]) -> HidResult<usize> {
        self.0.get_feature_report(buf)
    }

    /// Waits up to `timeout_ms` for the next input report.
    pub fn input_report(&self, buf: &mut [u8], timeout_ms: i32) -> HidResult<usize> {
        self.0.read_timeout(buf, timeout_ms)
    }

    /// Reads a USB string descriptor. This is a `GET_DESCRIPTOR` request, not a
    /// write. It isn't supported by hidapi's Linux hidraw backend.
    pub fn indexed_string(&self, index: i32) -> HidResult<Option<String>> {
        self.0.get_indexed_string(index)
    }
}
