//! Outage statistics over calendar periods (local time).

use chrono::{DateTime, Datelike, Duration, NaiveDate, TimeZone, Utc};
use ups_model::{DailyCoverage, Outage, PeriodStats};

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// `"November 2026"`.
#[must_use]
pub fn month_label(year: i32, month: u32) -> String {
    let name = MONTHS
        .get(usize::try_from(month.saturating_sub(1)).unwrap_or(0))
        .copied()
        .unwrap_or("?");
    format!("{name} {year}")
}

/// First and last day of a month.
#[must_use]
pub fn month_range(year: i32, month: u32) -> Option<(NaiveDate, NaiveDate)> {
    let first = NaiveDate::from_ymd_opt(year, month, 1)?;
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)?
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)?
    };
    Some((first, next.pred_opt()?))
}

/// UTC instant at which local `day` begins. Where a DST switch skips
/// midnight itself (e.g. Chile, Cuba), the day begins at the first local time
/// that exists (review M2-6).
pub(crate) fn local_midnight<Tz: TimeZone>(tz: &Tz, day: NaiveDate) -> DateTime<Utc> {
    (0..=3)
        .filter_map(|h| day.and_hms_opt(h, 0, 0))
        .find_map(|t| tz.from_local_datetime(&t).earliest())
        .map_or_else(
            || day.and_hms_opt(0, 0, 0).unwrap_or_default().and_utc(),
            |m| m.with_timezone(&Utc),
        )
}

/// Statistics for `first_day..=last_day` (local days in `tz`).
#[allow(clippy::cast_precision_loss)] // second counts far below 2^52
pub fn period_stats<Tz: TimeZone>(
    label: impl Into<String>,
    first_day: NaiveDate,
    last_day: NaiveDate,
    outages: &[Outage],
    daily: &[DailyCoverage],
    tz: &Tz,
    now: DateTime<Utc>,
) -> PeriodStats {
    let in_period = |d: NaiveDate| d >= first_day && d <= last_day;
    let started_in: Vec<&Outage> = outages
        .iter()
        .filter(|o| in_period(o.start.with_timezone(tz).date_naive()))
        .collect();
    let (monitored_s, on_battery_s) = daily
        .iter()
        .filter(|d| in_period(d.day))
        .fold((0.0, 0.0), |(m, b), d| {
            (m + d.monitored_s, b + d.on_battery_s)
        });

    let period_start = local_midnight(tz, first_day);
    let period_end = last_day
        .succ_opt()
        .map_or(now, |d| local_midnight(tz, d))
        .min(now);
    let period_s = ((period_end - period_start).num_milliseconds() as f64 / 1000.0).max(0.0);

    let secs = |d: Duration| d.num_milliseconds() as f64 / 1000.0;
    // Outages that never drained the battery below "full" need no recharge
    // and would pull the average towards zero.
    let recharges: Vec<f64> = started_in
        .iter()
        .filter(|o| o.recharge_needed())
        .filter_map(|o| o.recharge_duration().map(secs))
        .collect();

    PeriodStats {
        label: label.into(),
        first_day,
        last_day,
        outages: u32::try_from(started_in.len()).unwrap_or(u32::MAX),
        on_battery_s,
        longest_s: started_in
            .iter()
            .map(|o| secs(o.duration(now)))
            .fold(0.0, f64::max),
        monitored_s,
        coverage_pct: if period_s > 0.0 {
            (monitored_s / period_s * 100.0).min(100.0)
        } else {
            0.0
        },
        availability_pct: (monitored_s > 0.0)
            .then(|| ((monitored_s - on_battery_s) / monitored_s * 100.0).clamp(0.0, 100.0)),
        avg_recharge_s: (!recharges.is_empty())
            .then(|| recharges.iter().sum::<f64>() / recharges.len() as f64),
    }
}

/// Today, last 7 days, last 30 days and this month.
pub fn standard_periods<Tz: TimeZone>(
    outages: &[Outage],
    daily: &[DailyCoverage],
    tz: &Tz,
    now: DateTime<Utc>,
) -> Vec<PeriodStats> {
    let today = now.with_timezone(tz).date_naive();
    let back = |days: i64| today - Duration::days(days - 1);
    let (month_first, month_last) =
        month_range(today.year(), today.month()).unwrap_or((today, today));
    vec![
        period_stats("Today", today, today, outages, daily, tz, now),
        period_stats("Last 7 days", back(7), today, outages, daily, tz, now),
        period_stats("Last 30 days", back(30), today, outages, daily, tz, now),
        period_stats(
            month_label(today.year(), today.month()),
            month_first,
            month_last,
            outages,
            daily,
            tz,
            now,
        ),
    ]
}

/// `YYYY-MM` keys of the months that have monitoring data, newest first.
#[must_use]
pub fn months_with_data(daily: &[DailyCoverage]) -> Vec<String> {
    let mut months: Vec<String> = daily
        .iter()
        .filter(|d| d.monitored_s > 0.0)
        .map(|d| format!("{:04}-{:02}", d.day.year(), d.day.month()))
        .collect();
    months.sort_unstable();
    months.dedup();
    months.reverse();
    months
}

/// Parses `YYYY-MM`.
#[must_use]
pub fn parse_month(s: &str) -> Option<(i32, u32)> {
    let (y, m) = s.split_once('-')?;
    let (y, m) = (y.parse().ok()?, m.parse().ok()?);
    month_range(y, m).map(|_| (y, m))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn day(d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, d).unwrap()
    }

    fn outage(start_h: u32, minutes: i64, recharge_min: Option<i64>) -> Outage {
        let start = Utc.with_ymd_and_hms(2026, 11, 10, start_h, 0, 0).unwrap();
        let mut o = Outage::started(start);
        o.end = Some(start + Duration::minutes(minutes));
        o.recharged_at = recharge_min.map(|r| start + Duration::minutes(minutes + r));
        o
    }

    #[test]
    fn computes_period_stats() {
        let outages = [outage(8, 10, Some(60)), outage(20, 30, Some(120))];
        let daily = [
            DailyCoverage {
                day: day(10),
                monitored_s: 86_400.0,
                on_battery_s: 2400.0,
            },
            DailyCoverage {
                day: day(11),
                monitored_s: 43_200.0,
                on_battery_s: 0.0,
            },
        ];
        let now = Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap();
        let s = period_stats("x", day(10), day(11), &outages, &daily, &Utc, now);
        assert_eq!(s.outages, 2);
        assert_eq!(s.on_battery_s, 2400.0);
        assert_eq!(s.longest_s, 1800.0);
        assert_eq!(s.monitored_s, 129_600.0);
        // The period runs until `now` (36 h), fully monitored.
        assert!((s.coverage_pct - 100.0).abs() < 1e-9);
        let availability = s.availability_pct.unwrap();
        assert!((availability - (1.0 - 2400.0 / 129_600.0) * 100.0).abs() < 1e-9);
        assert_eq!(s.avg_recharge_s, Some(5400.0));

        let empty = period_stats("y", day(1), day(5), &outages, &[], &Utc, now);
        assert_eq!(empty.outages, 0);
        assert_eq!(empty.availability_pct, None);
        assert_eq!(empty.coverage_pct, 0.0);
    }

    #[test]
    fn builds_standard_periods_and_months() {
        let now = Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap();
        let p = standard_periods(&[], &[], &Utc, now);
        let labels: Vec<_> = p.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(
            labels,
            ["Today", "Last 7 days", "Last 30 days", "November 2026"]
        );
        assert_eq!(p[1].first_day, day(5));
        assert_eq!(p[3].last_day, day(30));

        let daily = [
            DailyCoverage {
                day: day(1),
                monitored_s: 1.0,
                on_battery_s: 0.0,
            },
            DailyCoverage {
                day: NaiveDate::from_ymd_opt(2026, 10, 31).unwrap(),
                monitored_s: 1.0,
                on_battery_s: 0.0,
            },
        ];
        assert_eq!(months_with_data(&daily), ["2026-11", "2026-10"]);
        assert_eq!(parse_month("2026-02"), Some((2026, 2)));
        assert_eq!(parse_month("2026-13"), None);
        assert_eq!(month_range(2028, 2).unwrap().1.day(), 29);
    }
}
