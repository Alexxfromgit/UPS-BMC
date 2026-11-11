//! Presentation of the Battery health tab (pure, unit-tested).

use std::fmt::Write as _;

use chrono::{DateTime, Local, Utc};
use ups_analysis::format;
use ups_api::{HealthReport, HealthVerdict, RuntimeTestPhase, RuntimeTestStatus};

const DASH: &str = "—";
/// Vertical range of the trend chart, in percent of the baseline.
const TREND_MAX: f64 = 110.0;
const TREND_MIN: f64 = 40.0;

/// Stop levels offered for the runtime test (must match `ui/app.slint`).
pub const STOP_LEVELS: [f32; 6] = [80.0, 70.0, 60.0, 50.0, 40.0, 30.0];
/// Default: 50 %, deep enough for a good measurement, gentle on the battery.
pub const DEFAULT_STOP_INDEX: i32 = 3;

/// Everything on the Battery health tab except the live runtime test state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HealthView {
    /// 0 unknown (grey), 1 good (green), 2 aging (amber), 3 replace (red).
    pub level: i32,
    pub verdict: String,
    pub health_text: String,
    pub health_detail: String,
    pub runtime_text: String,
    pub runtime_detail: String,
    pub age_text: String,
    pub age_detail: String,
    pub ups_text: String,
    pub ups_detail: String,
    /// Bulleted explanation, one reason per line.
    pub reasons: String,
    pub trend: TrendData,
    pub trend_span: String,
    /// Discharge table rows, newest first.
    pub rows: Vec<Vec<String>>,
    pub summary: String,
    /// Installation date as `YYYY-MM-DD` (empty if unknown), for the input field.
    pub installed_value: String,
}

/// Trend points normalised to 0..1 (x: time, y: 0 = top).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrendData {
    points: Vec<(f64, f64)>,
}

/// Path commands for the trend chart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderedTrend {
    pub line: String,
    pub dots: String,
    /// Dashed-looking guides at the "aging" (80 %) and "replace" (60 %) levels.
    pub guides: String,
}

fn y_of(pct: f64) -> f64 {
    1.0 - (pct.clamp(TREND_MIN, TREND_MAX) - TREND_MIN) / (TREND_MAX - TREND_MIN)
}

impl TrendData {
    /// Path commands for a plot of `width` × `height` logical pixels.
    #[must_use]
    pub fn render(&self, width: f64, height: f64) -> RenderedTrend {
        let (w, h) = if width > 1.0 && height > 1.0 {
            (width, height)
        } else {
            (1000.0, 100.0)
        };
        let mut line = String::new();
        let mut dots = String::new();
        for (i, &(x, y)) in self.points.iter().enumerate() {
            let (px, py) = (x * w, y * h);
            let _ = write!(line, "{} {px:.1} {py:.1} ", if i == 0 { 'M' } else { 'L' });
            // A small diamond per discharge.
            let _ = write!(
                dots,
                "M {:.1} {py:.1} L {px:.1} {:.1} L {:.1} {py:.1} L {px:.1} {:.1} Z ",
                px - 4.0,
                py - 4.0,
                px + 4.0,
                py + 4.0
            );
        }
        let mut guides = String::new();
        // Guides only with data (they would cross the empty-state text).
        let levels: &[f64] = if self.points.is_empty() {
            &[]
        } else {
            &[80.0, 60.0]
        };
        for &pct in levels {
            let y = y_of(pct) * h;
            let mut x = 0.0;
            while x < w {
                let _ = write!(guides, "M {x:.1} {y:.1} L {:.1} {y:.1} ", (x + 6.0).min(w));
                x += 12.0;
            }
        }
        RenderedTrend {
            line: line.trim_end().to_owned(),
            dots: dots.trim_end().to_owned(),
            guides: guides.trim_end().to_owned(),
        }
    }
}

fn local_date(ts: DateTime<Utc>) -> String {
    ts.with_timezone(&Local).format("%d.%m.%Y").to_string()
}

/// Builds the Battery health tab from the collector's report.
#[must_use]
#[allow(clippy::too_many_lines)] // one flat mapping per card and column
pub fn build(r: &HealthReport, now: DateTime<Utc>) -> HealthView {
    let (level, verdict) = match r.verdict {
        HealthVerdict::Unknown => (0, "Not enough data yet"),
        HealthVerdict::Good => (1, "Battery is in good condition"),
        HealthVerdict::Aging => (2, "Battery is aging: plan a replacement"),
        HealthVerdict::Replace => (3, "Replace the battery"),
    };
    let what = if r.metric == "capacity" {
        "capacity"
    } else {
        "runtime"
    };
    let health_detail = match r.health_pct {
        Some(_) => format!("{} · {what} vs. the first discharges", r.confidence.label()),
        None => format!(
            "{} usable discharge{} so far",
            r.discharges.len(),
            if r.discharges.len() == 1 { "" } else { "s" }
        ),
    };
    let runtime_detail = match (r.full_runtime_s, r.reference_power_w) {
        (None, _) => "appears after the first usable outage".to_owned(),
        (Some(_), Some(w)) if r.reference_power_estimated => {
            format!("from a full battery at ≈ {w:.0} W (your load estimate)")
        }
        (Some(_), Some(w)) => format!("from a full battery at {w:.0} W"),
        (Some(_), None) => "from a full battery at the recent load".to_owned(),
    };
    let today = now.with_timezone(&Local).date_naive();
    let (age_text, age_detail) = match r.battery_installed {
        Some(d) => {
            let months = ups_analysis::health::age_months(d, today);
            let text = match (months / 12, months % 12) {
                (0, m) => format!("{m} mo"),
                (y, 0) => format!("{y} y"),
                (y, m) => format!("{y} y {m} mo"),
            };
            (text, format!("installed {}", d.format("%d.%m.%Y")))
        }
        None => (
            "Unknown".to_owned(),
            "enter the installation date below".to_owned(),
        ),
    };
    let (ups_text, ups_detail) = if r.ups_replace_flag {
        (
            "Replace battery".to_owned(),
            "the UPS's own battery check failed".to_owned(),
        )
    } else {
        (
            "No warning".to_owned(),
            "the UPS's own battery check (RB flag)".to_owned(),
        )
    };

    let first = r.discharges.first().map(|d| d.start);
    let span_start = first.unwrap_or(now);
    #[allow(clippy::cast_precision_loss)] // milliseconds
    let span_ms = (now - span_start).num_milliseconds().max(1) as f64;
    #[allow(clippy::cast_precision_loss)]
    let points = r
        .discharges
        .iter()
        .filter_map(|d| {
            let rel = d.relative_pct?;
            let x = ((d.start - span_start).num_milliseconds() as f64 / span_ms).clamp(0.0, 1.0);
            // Keep the first and last points off the plot border.
            Some((0.02 + x * 0.96, y_of(rel)))
        })
        .collect();
    let trend_span = first.map_or_else(String::new, |f| {
        format!("{} – {}", local_date(f), local_date(now))
    });

    let rows = r
        .discharges
        .iter()
        .rev()
        .map(|d| {
            let mut notes = Vec::new();
            if d.runtime_test {
                notes.push("runtime test");
            }
            if d.baseline {
                notes.push("baseline");
            }
            vec![
                d.start
                    .with_timezone(&Local)
                    .format("%d.%m.%Y %H:%M")
                    .to_string(),
                format::duration(d.duration_s),
                format!(
                    "{:.0} → {:.0} %",
                    d.charge_start_pct,
                    d.charge_start_pct - d.charge_used_pct
                ),
                match d.avg_power_w {
                    Some(w) if d.power_estimated => format!("≈ {w:.0} W"),
                    Some(w) => format!("{w:.0} W"),
                    None => DASH.to_owned(),
                },
                format::duration(d.full_runtime_s),
                d.capacity_wh.map_or_else(
                    || DASH.to_owned(),
                    |c| {
                        if d.power_estimated {
                            format!("≈ {c:.0} Wh")
                        } else {
                            format!("{c:.0} Wh")
                        }
                    },
                ),
                d.vs_ups_estimate_pct
                    .map_or_else(|| DASH.to_owned(), |v| format!("{v:.0} %")),
                d.relative_pct
                    .map_or_else(|| DASH.to_owned(), |v| format!("{v:.0} %")),
                notes.join(", "),
            ]
        })
        .collect();

    let summary = match (r.discharges.len(), r.skipped) {
        (0, 0) => "No outages recorded yet.".to_owned(),
        (n, s) => format!(
            "{n} usable discharge{} · {s} other outage{} too short or partial. \
             \"Full runtime\" extrapolates each discharge to 100 → 0 %.",
            if n == 1 { "" } else { "s" },
            if s == 1 { "" } else { "s" }
        ),
    };

    HealthView {
        level,
        verdict: verdict.to_owned(),
        health_text: r
            .health_pct
            .map_or_else(|| DASH.to_owned(), |h| format!("{h:.0} %")),
        health_detail,
        runtime_text: r
            .full_runtime_s
            .map_or_else(|| DASH.to_owned(), |s| format!("≈ {}", format::duration(s))),
        runtime_detail,
        age_text,
        age_detail,
        ups_text,
        ups_detail,
        reasons: r
            .reasons
            .iter()
            .map(|x| format!("• {x}"))
            .collect::<Vec<_>>()
            .join("\n"),
        trend: TrendData { points },
        trend_span,
        rows,
        summary,
        installed_value: r
            .battery_installed
            .map(|d| d.to_string())
            .unwrap_or_default(),
    }
}

/// Live runtime test state: phase code (0 none, 1 armed, 2 running,
/// 3 restore power) and a sentence for the user.
#[must_use]
pub fn test_state(t: Option<&RuntimeTestStatus>) -> (i32, String) {
    let Some(t) = t else {
        return (0, String::new());
    };
    match t.phase {
        RuntimeTestPhase::Armed => (
            1,
            format!(
                "Armed: unplug the UPS from the wall socket now (leave your equipment plugged into \
                 the UPS). You'll be asked to plug it back in at {:.0} %.",
                t.stop_at_pct
            ),
        ),
        RuntimeTestPhase::Running => (
            2,
            format!(
                "Running on battery since {}. Keep the UPS unplugged until the charge reaches {:.0} %.",
                t.started_at.map_or_else(
                    || DASH.to_owned(),
                    |s| s.with_timezone(&Local).format("%H:%M:%S").to_string()
                ),
                t.stop_at_pct
            ),
        ),
        RuntimeTestPhase::RestorePower => (
            3,
            format!(
                "The charge reached {:.0} %: plug the UPS back into the wall socket now. \
                 The result appears when mains power is back.",
                t.stop_at_pct
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, NaiveDate, TimeZone};
    use ups_api::{Confidence, Discharge};

    use super::*;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap()
    }

    fn discharge(days_ago: i64, rel: f64, baseline: bool) -> Discharge {
        Discharge {
            outage_id: 1,
            start: now() - Duration::days(days_ago),
            duration_s: 1800.0,
            charge_start_pct: 100.0,
            charge_used_pct: 25.0,
            avg_power_w: Some(20.0),
            power_estimated: true,
            runtime_test: !baseline,
            full_runtime_s: 7200.0,
            capacity_wh: Some(40.0),
            vs_ups_estimate_pct: Some(82.0),
            sag_mv_per_pct: None,
            relative_pct: Some(rel),
            baseline,
        }
    }

    fn report() -> HealthReport {
        HealthReport {
            generated_at: now(),
            verdict: HealthVerdict::Aging,
            confidence: Confidence::Medium,
            health_pct: Some(75.4),
            metric: "capacity".into(),
            reference_power_w: Some(20.0),
            reference_power_estimated: true,
            full_runtime_s: Some(5400.0),
            reasons: vec!["First reason.".into(), "Second.".into()],
            discharges: vec![discharge(100, 100.0, true), discharge(10, 75.0, false)],
            skipped: 1,
            battery_installed: NaiveDate::from_ymd_opt(2024, 3, 15),
            ups_replace_flag: false,
        }
    }

    #[test]
    fn builds_cards_rows_and_trend() {
        let v = build(&report(), now());
        assert_eq!(v.level, 2);
        assert_eq!(v.health_text, "75 %");
        assert_eq!(
            v.health_detail,
            "medium confidence · capacity vs. the first discharges"
        );
        assert_eq!(v.runtime_text, "≈ 1 h 30 min");
        assert!(v.runtime_detail.contains("≈ 20 W (your load estimate)"));
        assert_eq!(v.age_text, "2 y 7 mo");
        assert_eq!(v.installed_value, "2024-03-15");
        assert_eq!(v.reasons, "• First reason.\n• Second.");
        // Newest first.
        assert_eq!(v.rows[0][7], "75 %");
        assert_eq!(v.rows[0][8], "runtime test");
        assert_eq!(v.rows[1][8], "baseline");
        assert_eq!(v.rows[0][3], "≈ 20 W");
        assert_eq!(v.rows[0][5], "≈ 40 Wh");
        assert_eq!(v.rows[0].len(), 9);
        assert!(
            v.summary
                .starts_with("2 usable discharges · 1 other outage too short")
        );

        let t = v.trend.render(700.0, 140.0);
        // 100 % → y = 10/70 of the height; first point near the left edge.
        assert!(t.line.starts_with("M 14.0 20.0 L"), "{}", t.line);
        assert_eq!(t.dots.matches('Z').count(), 2);
        assert_ne!(t.guides, "");
    }

    #[test]
    fn handles_an_empty_report() {
        let mut r = report();
        r.verdict = HealthVerdict::Unknown;
        r.health_pct = None;
        r.full_runtime_s = None;
        r.discharges.clear();
        r.skipped = 0;
        r.battery_installed = None;
        r.ups_replace_flag = true;
        let v = build(&r, now());
        assert_eq!(v.level, 0);
        assert_eq!(v.health_text, "—");
        assert_eq!(v.health_detail, "0 usable discharges so far");
        assert_eq!(v.age_text, "Unknown");
        assert_eq!(v.ups_text, "Replace battery");
        assert_eq!(v.summary, "No outages recorded yet.");
        assert_eq!(v.trend.render(100.0, 100.0).line, "");
    }

    #[test]
    fn describes_the_test_phases() {
        assert_eq!(test_state(None), (0, String::new()));
        let mut t = RuntimeTestStatus {
            phase: RuntimeTestPhase::Armed,
            armed_at: now(),
            stop_at_pct: 50.0,
            started_at: None,
            charge_start_pct: None,
        };
        assert!(test_state(Some(&t)).1.contains("unplug the UPS"));
        t.phase = RuntimeTestPhase::RestorePower;
        let (phase, text) = test_state(Some(&t));
        assert_eq!(phase, 3);
        assert!(text.contains("plug the UPS back"));
        assert_eq!(
            STOP_LEVELS[usize::try_from(DEFAULT_STOP_INDEX).unwrap()],
            50.0
        );
    }
}
