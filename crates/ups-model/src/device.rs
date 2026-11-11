use serde::{Deserialize, Serialize};

/// Identity of the monitored UPS, as reported by the source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// Telemetry source kind, e.g. `"hid"`, `"sim"` or `"nut"`.
    pub source: String,
    /// `ups.mfr`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    /// `ups.model`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `ups.serial`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    /// `battery.type`, e.g. `PbAc`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub battery_type: Option<String>,
    /// USB vendor ID (HID sources).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor_id: Option<u16>,
    /// USB product ID (HID sources).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_id: Option<u16>,
}

impl DeviceInfo {
    /// `"Eaton 5E"`-style display name, falling back to the source kind.
    #[must_use]
    pub fn display_name(&self) -> String {
        match (&self.manufacturer, &self.model) {
            (Some(m), Some(p)) if p.to_lowercase().starts_with(&m.to_lowercase()) => p.clone(),
            (Some(m), Some(p)) => format!("{m} {p}"),
            (None, Some(p)) => p.clone(),
            (Some(m), None) => m.clone(),
            (None, None) => format!("UPS ({})", self.source),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_avoids_repeating_the_manufacturer() {
        let mut d = DeviceInfo {
            source: "hid".into(),
            manufacturer: Some("EATON".into()),
            model: Some("Eaton 5E".into()),
            ..DeviceInfo::default()
        };
        assert_eq!(d.display_name(), "Eaton 5E");
        d.model = Some("5E 900".into());
        assert_eq!(d.display_name(), "EATON 5E 900");
        d.model = None;
        d.manufacturer = None;
        assert_eq!(d.display_name(), "UPS (hid)");
    }
}
