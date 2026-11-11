//! Human-readable formatting shared by the collector, reports and the UI.

/// `"35 s"`, `"2 min 10 s"`, `"1 h 05 min"`, `"2 d 03 h"`.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // rounded, clamped seconds
pub fn duration(seconds: f64) -> String {
    let secs = seconds.max(0.0).round() as u64;
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min {:02} s", secs / 60, secs % 60),
        3600..86_400 => format!("{} h {:02} min", secs / 3600, (secs % 3600) / 60),
        _ => format!("{} d {:02} h", secs / 86_400, (secs % 86_400) / 3600),
    }
}

/// Percentage with one decimal, or `"—"`.
#[must_use]
pub fn percent(v: Option<f64>) -> String {
    v.map_or_else(|| "—".to_owned(), |v| format!("{v:.1} %"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_durations() {
        assert_eq!(duration(0.4), "0 s");
        assert_eq!(duration(35.0), "35 s");
        assert_eq!(duration(130.0), "2 min 10 s");
        assert_eq!(duration(3900.0), "1 h 05 min");
        assert_eq!(duration(183_600.0), "2 d 03 h");
        assert_eq!(duration(-5.0), "0 s");
        assert_eq!(percent(Some(99.95)), "100.0 %");
        assert_eq!(percent(None), "—");
    }
}
