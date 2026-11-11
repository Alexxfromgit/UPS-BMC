//! Monthly reports: a self-contained, printable HTML page and a CSV export.

use std::fmt::Write as _;

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use ups_model::{DailyCoverage, DeviceInfo, Outage};

use crate::format;
use crate::stats::{month_label, month_range, period_stats};

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn local<Tz: TimeZone>(t: DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    t.with_timezone(tz).format("%Y-%m-%d %H:%M:%S").to_string()
}

fn opt<T>(v: Option<T>, f: impl Fn(T) -> String) -> String {
    v.map_or_else(|| "—".to_owned(), f)
}

#[allow(clippy::cast_precision_loss)]
fn secs(d: Duration) -> f64 {
    d.num_milliseconds() as f64 / 1000.0
}

/// Outages that started in the month (local time), oldest first.
fn month_outages<'a, Tz: TimeZone>(
    outages: &'a [Outage],
    first: NaiveDate,
    last: NaiveDate,
    tz: &Tz,
) -> Vec<&'a Outage> {
    let mut v: Vec<&Outage> = outages
        .iter()
        .filter(|o| {
            let d = o.start.with_timezone(tz).date_naive();
            d >= first && d <= last
        })
        .collect();
    v.sort_by_key(|o| o.start);
    v
}

fn notes(o: &Outage) -> String {
    let mut n = Vec::new();
    if o.is_ongoing() {
        n.push("ongoing".to_owned());
    }
    if o.runtime_test {
        n.push("guided runtime test (unplugged on purpose)".to_owned());
    }
    if !o.start_observed() {
        n.push("start not observed (monitoring gap)".to_owned());
    }
    if !o.end_observed() {
        n.push("end not observed (monitoring gap)".to_owned());
    }
    if o.end.is_some() && o.recharged_at.is_none() {
        n.push("recharge not completed/observed".to_owned());
    }
    n.join("; ")
}

/// Monthly HTML report. `None` for an invalid month.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub fn monthly_html<Tz: TimeZone>(
    year: i32,
    month: u32,
    outages: &[Outage],
    daily: &[DailyCoverage],
    device: Option<&DeviceInfo>,
    tz: &Tz,
    now: DateTime<Utc>,
    generator: &str,
) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    let (first, last) = month_range(year, month)?;
    let title = month_label(year, month);
    let stats = period_stats(&title, first, last, outages, daily, tz, now);
    let month_list = month_outages(outages, first, last, tz);
    let device_name = device.map_or_else(|| "UPS".to_owned(), DeviceInfo::display_name);

    let mut h = String::with_capacity(16 * 1024);
    let _ = write!(
        h,
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Power report — {title}</title>
<style>
  :root {{ --fg:#1f2328; --muted:#59636e; --line:#d1d9e0; --ok:#1a7f37; --warn:#bf8700; --bad:#cf222e; }}
  body {{ font: 14px/1.45 "Segoe UI", system-ui, sans-serif; color: var(--fg); margin: 32px auto; max-width: 1100px; padding: 0 16px; }}
  h1 {{ margin: 0 0 4px; font-size: 26px; }}
  h2 {{ margin: 32px 0 10px; font-size: 18px; }}
  .sub {{ color: var(--muted); margin: 0 0 20px; }}
  .kpis {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr)); gap: 12px; }}
  .kpi {{ border: 1px solid var(--line); border-radius: 10px; padding: 12px 14px; }}
  .kpi b {{ display: block; font-size: 22px; margin-top: 2px; }}
  .kpi span {{ color: var(--muted); font-size: 12px; }}
  table {{ border-collapse: collapse; width: 100%; font-size: 13px; }}
  th, td {{ border-bottom: 1px solid var(--line); padding: 6px 8px; text-align: left; vertical-align: top; }}
  th {{ color: var(--muted); font-weight: 600; }}
  td.num {{ text-align: right; font-variant-numeric: tabular-nums; }}
  .bar {{ background: #eef1f4; border-radius: 4px; height: 10px; min-width: 120px; position: relative; }}
  .bar i {{ position: absolute; left: 0; top: 0; bottom: 0; border-radius: 4px; background: var(--ok); }}
  .bar i.warn {{ background: var(--warn); }} .bar i.bad {{ background: var(--bad); }}
  .note {{ color: var(--muted); font-size: 12px; margin-top: 24px; }}
  @media print {{ body {{ margin: 0; }} .kpi {{ break-inside: avoid; }} }}
</style>
</head>
<body>
<h1>Power report — {title}</h1>
<p class="sub">{device} · generated {generated} by {generator}</p>
<section class="kpis">
"#,
        title = esc(&title),
        device = esc(&device_name),
        generated = now.with_timezone(tz).format("%Y-%m-%d %H:%M"),
        generator = esc(generator),
    );
    let kpi = |h: &mut String, label: &str, value: String, hint: &str| {
        let _ = writeln!(
            h,
            "  <div class=\"kpi\"><span>{}</span><b>{}</b><span>{}</span></div>",
            esc(label),
            esc(&value),
            esc(hint)
        );
    };
    kpi(
        &mut h,
        "Mains availability",
        format::percent(stats.availability_pct),
        "of monitored time",
    );
    kpi(
        &mut h,
        "Outages",
        stats.outages.to_string(),
        "started this month",
    );
    kpi(
        &mut h,
        "Time on battery",
        format::duration(stats.on_battery_s),
        "total",
    );
    kpi(
        &mut h,
        "Longest outage",
        format::duration(stats.longest_s),
        "",
    );
    kpi(
        &mut h,
        "Average recharge",
        stats
            .avg_recharge_s
            .map_or_else(|| "—".to_owned(), format::duration),
        "to 99 % after an outage",
    );
    kpi(
        &mut h,
        "Monitoring coverage",
        format::percent(Some(stats.coverage_pct)),
        "of the month so far",
    );
    h.push_str("</section>\n\n<h2>Daily overview</h2>\n<table>\n<tr><th>Day</th><th>Mains availability</th><th></th><th class=\"num\">Outages</th><th class=\"num\">On battery</th><th class=\"num\">Monitored</th></tr>\n");

    let today = now.with_timezone(tz).date_naive();
    let mut day = first;
    while day <= last && day <= today {
        let cov = daily.iter().find(|d| d.day == day);
        let count = month_list
            .iter()
            .filter(|o| o.start.with_timezone(tz).date_naive() == day)
            .count();
        let avail = cov
            .filter(|c| c.monitored_s > 0.0)
            .map(|c| (c.monitored_s - c.on_battery_s) / c.monitored_s * 100.0);
        let class = match avail {
            Some(a) if a >= 99.9 => "",
            Some(a) if a >= 99.0 => "warn",
            Some(_) => "bad",
            None => "",
        };
        let _ = writeln!(
            h,
            "<tr><td>{}</td><td class=\"num\">{}</td><td><div class=\"bar\"><i class=\"{class}\" style=\"width:{:.1}%\"></i></div></td><td class=\"num\">{count}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
            day.format("%a %d.%m"),
            format::percent(avail),
            avail.unwrap_or(0.0),
            opt(cov.map(|c| c.on_battery_s), format::duration),
            opt(cov.map(|c| c.monitored_s), format::duration),
        );
        let Some(next) = day.succ_opt() else { break };
        day = next;
    }
    h.push_str("</table>\n\n<h2>Outages</h2>\n");

    if month_list.is_empty() {
        h.push_str("<p>No outages recorded this month.</p>\n");
    } else {
        h.push_str("<table>\n<tr><th>#</th><th>Start</th><th>End</th><th class=\"num\">Duration</th><th class=\"num\">Battery</th><th class=\"num\">Lowest</th><th class=\"num\">Battery V</th><th class=\"num\">UPS estimate at start</th><th class=\"num\">Avg load</th><th class=\"num\">Energy</th><th class=\"num\">Recharge</th><th>Notes</th></tr>\n");
        for (i, o) in month_list.iter().enumerate() {
            let approx = if o.power_estimated { "≈ " } else { "" };
            let _ = writeln!(
                h,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td>{}</td></tr>",
                i + 1,
                local(o.start, tz),
                opt(o.end, |e| local(e, tz)),
                format::duration(secs(o.duration(now))),
                match (o.charge_start_pct, o.charge_end_pct) {
                    (Some(a), Some(b)) => format!("{a:.0} → {b:.0} %"),
                    _ => "—".into(),
                },
                opt(o.charge_min_pct, |c| format!("{c:.0} %")),
                match (o.battery_v_start, o.battery_v_min) {
                    (Some(a), Some(m)) => format!("{a:.1} → {m:.1} V"),
                    _ => "—".into(),
                },
                opt(o.runtime_estimate_start_s, |r| format::duration(f64::from(
                    r
                ))),
                opt(o.avg_power_w, |w| if o.power_estimated {
                    format!("≈ {w:.0} W (estimate)")
                } else {
                    format!(
                        "{w:.0} W{}",
                        o.avg_load_pct
                            .map(|l| format!(" ({l:.0} %)"))
                            .unwrap_or_default()
                    )
                }),
                opt(o.energy_wh, |e| format!("{approx}{e:.1} Wh")),
                opt(o.recharge_duration(), |d| format::duration(secs(d))),
                esc(&notes(o)),
            );
        }
        h.push_str("</table>\n");
    }
    h.push_str(
        "<p class=\"note\">Availability counts only time when UPS-BMC was monitoring. \
         Outages are times the UPS ran on battery. Runtime figures are the UPS's own \
         estimates, not laboratory measurements. Times are local.</p>\n</body>\n</html>\n",
    );
    Some(h)
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// CSV of the outages that started in the month (local times, plain numbers).
pub fn monthly_csv<Tz: TimeZone>(
    year: i32,
    month: u32,
    outages: &[Outage],
    tz: &Tz,
    now: DateTime<Utc>,
) -> Option<String>
where
    Tz::Offset: std::fmt::Display,
{
    let (first, last) = month_range(year, month)?;
    let mut out = String::from(
        "start,end,duration_s,start_observed,end_observed,charge_start_pct,charge_end_pct,\
         charge_min_pct,ups_runtime_estimate_s,avg_load_pct,avg_power_w,energy_wh,recharge_s,\
         battery_v_start,battery_v_end,battery_v_min,power_estimated,notes\r\n",
    );
    let num = |v: Option<f64>| v.map(|v| format!("{v:.1}")).unwrap_or_default();
    for o in month_outages(outages, first, last, tz) {
        let row = [
            local(o.start, tz),
            o.end.map(|e| local(e, tz)).unwrap_or_default(),
            format!("{:.0}", secs(o.duration(now))),
            o.start_observed().to_string(),
            o.end_observed().to_string(),
            num(o.charge_start_pct.map(f64::from)),
            num(o.charge_end_pct.map(f64::from)),
            num(o.charge_min_pct.map(f64::from)),
            o.runtime_estimate_start_s
                .map(|r| r.to_string())
                .unwrap_or_default(),
            num(o.avg_load_pct.map(f64::from)),
            num(o.avg_power_w.map(f64::from)),
            num(o.energy_wh.map(f64::from)),
            o.recharge_duration()
                .map(|d| format!("{:.0}", secs(d)))
                .unwrap_or_default(),
            num(o.battery_v_start.map(f64::from)),
            num(o.battery_v_end.map(f64::from)),
            num(o.battery_v_min.map(f64::from)),
            o.power_estimated.to_string(),
            notes(o),
        ];
        let line: Vec<String> = row.iter().map(|f| csv_field(f)).collect();
        out.push_str(&line.join(","));
        out.push_str("\r\n");
    }
    Some(out)
}

/// Suggested file name, e.g. `UPS-BMC report 2026-11.html`.
#[must_use]
pub fn file_name(year: i32, month: u32, extension: &str) -> String {
    format!("UPS-BMC report {year:04}-{month:02}.{extension}")
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn sample() -> (Vec<Outage>, Vec<DailyCoverage>, DateTime<Utc>) {
        let start = Utc.with_ymd_and_hms(2026, 11, 10, 8, 0, 0).unwrap();
        let mut a = Outage::started(start);
        a.end = Some(start + Duration::minutes(12));
        a.charge_start_pct = Some(100.0);
        a.charge_end_pct = Some(81.0);
        a.charge_min_pct = Some(81.0);
        a.runtime_estimate_start_s = Some(1800);
        a.avg_load_pct = Some(20.0);
        a.avg_power_w = Some(96.0);
        a.energy_wh = Some(19.2);
        a.recharged_at = Some(start + Duration::minutes(80));
        a.battery_v_start = Some(13.4);
        a.battery_v_min = Some(12.3);
        let mut b = Outage::started(start + Duration::hours(30));
        b.end = Some(start + Duration::hours(31));
        b.end_latest = Some(start + Duration::hours(33));
        let mut october = Outage::started(Utc.with_ymd_and_hms(2026, 10, 30, 8, 0, 0).unwrap());
        october.end = Some(october.start + Duration::minutes(1));
        let daily = vec![DailyCoverage {
            day: NaiveDate::from_ymd_opt(2026, 11, 10).unwrap(),
            monitored_s: 86_400.0,
            on_battery_s: 720.0,
        }];
        (
            vec![a, b, october],
            daily,
            Utc.with_ymd_and_hms(2026, 11, 12, 9, 0, 0).unwrap(),
        )
    }

    #[test]
    fn renders_the_monthly_html_report() {
        let (outages, daily, now) = sample();
        let device = DeviceInfo {
            source: "hid".into(),
            model: Some("Eaton 5E <900>".into()),
            ..DeviceInfo::default()
        };
        let html = monthly_html(
            2026,
            11,
            &outages,
            &daily,
            Some(&device),
            &Utc,
            now,
            "UPS-BMC 0.1.0",
        )
        .unwrap();
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("Power report — November 2026"));
        assert!(
            html.contains("Eaton 5E &lt;900&gt;"),
            "device name is escaped"
        );
        assert!(html.contains("<b>2</b>"), "two outages started in November");
        assert!(html.contains("100 → 81 %"));
        assert!(html.contains("13.4 → 12.3 V"));
        assert!(html.contains("end not observed (monitoring gap)"));
        assert!(!html.contains("2026-10-30"), "October outage excluded");
        assert!(html.contains("99.2 %"), "daily availability of the 10th");
        assert_eq!(monthly_html(2026, 13, &[], &[], None, &Utc, now, "x"), None);
    }

    #[test]
    fn renders_csv() {
        let (outages, _, now) = sample();
        let csv = monthly_csv(2026, 11, &outages, &Utc, now).unwrap();
        let lines: Vec<&str> = csv.lines().collect();
        assert_eq!(lines.len(), 3, "{csv}");
        assert!(lines[0].starts_with("start,end,duration_s"));
        assert!(
            lines[1]
                .starts_with("2026-11-10 08:00:00,2026-11-10 08:12:00,720,true,true,100.0,81.0")
        );
        assert!(lines[2].contains(",true,false,"));
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(file_name(2026, 11, "csv"), "UPS-BMC report 2026-11.csv");
    }
}
