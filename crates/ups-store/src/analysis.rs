//! Storage for M2 analysis data: outages (kept forever) and per-day
//! monitoring coverage (kept forever), plus sample replay for backfilling.

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{OptionalExtension, Row, params};
use ups_model::{DailyCoverage, Outage, Snapshot, UpsStatus};

use crate::{Result, Store, StoreError, from_ms, ms};

const OUTAGE_COLUMNS: &str = "id, start_ms, start_earliest_ms, end_ms, end_latest_ms, \
     charge_start, charge_end, charge_min, runtime_est_start_s, avg_load_pct, avg_power_w, \
     energy_wh, recharged_ms, battery_v_start, battery_v_end, battery_v_min, power_estimated, \
     runtime_test";

fn outage_from_row(r: &Row<'_>) -> rusqlite::Result<Outage> {
    let t = |v: Option<i64>| v.map(from_ms);
    Ok(Outage {
        id: r.get(0)?,
        start: from_ms(r.get(1)?),
        start_earliest: t(r.get(2)?),
        end: t(r.get(3)?),
        end_latest: t(r.get(4)?),
        charge_start_pct: r.get(5)?,
        charge_end_pct: r.get(6)?,
        charge_min_pct: r.get(7)?,
        runtime_estimate_start_s: r.get(8)?,
        avg_load_pct: r.get(9)?,
        avg_power_w: r.get(10)?,
        energy_wh: r.get(11)?,
        recharged_at: t(r.get(12)?),
        battery_v_start: r.get(13)?,
        battery_v_end: r.get(14)?,
        battery_v_min: r.get(15)?,
        power_estimated: r.get(16)?,
        runtime_test: r.get(17)?,
    })
}

impl Store {
    /// Inserts a new outage (`id == 0`) or updates an existing one. Returns its ID.
    pub fn save_outage(&mut self, o: &Outage) -> Result<i64> {
        let values = params![
            ms(o.start),
            o.start_earliest.map(ms),
            o.end.map(ms),
            o.end_latest.map(ms),
            o.charge_start_pct,
            o.charge_end_pct,
            o.charge_min_pct,
            o.runtime_estimate_start_s,
            o.avg_load_pct,
            o.avg_power_w,
            o.energy_wh,
            o.recharged_at.map(ms),
            o.battery_v_start,
            o.battery_v_end,
            o.battery_v_min,
            o.power_estimated,
            o.runtime_test,
        ];
        if o.id == 0 {
            self.conn.execute(
                "INSERT INTO outages (start_ms, start_earliest_ms, end_ms, end_latest_ms,
                 charge_start, charge_end, charge_min, runtime_est_start_s, avg_load_pct,
                 avg_power_w, energy_wh, recharged_ms, battery_v_start, battery_v_end,
                 battery_v_min, power_estimated, runtime_test)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                values,
            )?;
            Ok(self.conn.last_insert_rowid())
        } else {
            let mut stmt = self.conn.prepare_cached(
                "UPDATE outages SET start_ms = ?1, start_earliest_ms = ?2, end_ms = ?3,
                 end_latest_ms = ?4, charge_start = ?5, charge_end = ?6, charge_min = ?7,
                 runtime_est_start_s = ?8, avg_load_pct = ?9, avg_power_w = ?10,
                 energy_wh = ?11, recharged_ms = ?12, battery_v_start = ?13, battery_v_end = ?14,
                 battery_v_min = ?15, power_estimated = ?16, runtime_test = ?17 WHERE id = ?18",
            )?;
            let mut p: Vec<&dyn rusqlite::ToSql> = values.to_vec();
            p.push(&o.id);
            let n = stmt.execute(p.as_slice())?;
            if n == 0 {
                return Err(StoreError::Corrupt(format!("outage {} not found", o.id)));
            }
            Ok(o.id)
        }
    }

    /// The ongoing outage (no end), if any.
    pub fn open_outage(&self) -> Result<Option<Outage>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {OUTAGE_COLUMNS} FROM outages WHERE end_ms IS NULL \
                     ORDER BY start_ms DESC LIMIT 1"
                ),
                [],
                outage_from_row,
            )
            .optional()?)
    }

    /// The most recent ended outage whose recharge hasn't been seen yet, if it
    /// ended after `since`.
    pub fn recharging_outage(&self, since: DateTime<Utc>) -> Result<Option<Outage>> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "SELECT {OUTAGE_COLUMNS} FROM outages \
                     WHERE end_ms IS NOT NULL AND recharged_ms IS NULL AND end_ms >= ?1 \
                     ORDER BY start_ms DESC LIMIT 1"
                ),
                [ms(since)],
                outage_from_row,
            )
            .optional()?)
    }

    /// Outages that started at or after `from`, newest first (at most `limit`).
    pub fn outages_since(&self, from: DateTime<Utc>, limit: usize) -> Result<Vec<Outage>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {OUTAGE_COLUMNS} FROM outages WHERE start_ms >= ?1 \
             ORDER BY start_ms DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(
            params![ms(from), i64::try_from(limit).unwrap_or(i64::MAX)],
            outage_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn outage_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM outages", [], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Adds coverage increments (monitored / on-battery seconds) per day.
    pub fn add_daily(&mut self, increments: &[DailyCoverage]) -> Result<()> {
        if increments.is_empty() {
            return Ok(());
        }
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO daily (day, monitored_s, on_battery_s) VALUES (?1, ?2, ?3)
                 ON CONFLICT(day) DO UPDATE SET
                   monitored_s = monitored_s + excluded.monitored_s,
                   on_battery_s = on_battery_s + excluded.on_battery_s",
            )?;
            for d in increments {
                stmt.execute(params![d.day.to_string(), d.monitored_s, d.on_battery_s])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Daily coverage from `first` to `last` (inclusive), oldest first.
    pub fn daily(&self, first: NaiveDate, last: NaiveDate) -> Result<Vec<DailyCoverage>> {
        let mut stmt = self.conn.prepare(
            "SELECT day, monitored_s, on_battery_s FROM daily
             WHERE day >= ?1 AND day <= ?2 ORDER BY day",
        )?;
        let rows = stmt.query_map(params![first.to_string(), last.to_string()], |r| {
            Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (day, monitored_s, on_battery_s) = row?;
            let day = day
                .parse()
                .map_err(|_| StoreError::Corrupt(format!("bad day {day}")))?;
            out.push(DailyCoverage {
                day,
                monitored_s,
                on_battery_s,
            });
        }
        Ok(out)
    }

    fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    fn set_meta(&mut self, key: &str, value: Option<String>) -> Result<()> {
        match value {
            Some(v) => self.conn.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                [key, &v],
            )?,
            None => self
                .conn
                .execute("DELETE FROM meta WHERE key = ?1", [key])?,
        };
        Ok(())
    }

    /// The user's estimate of the protected load (watts), if set.
    pub fn assumed_load_w(&self) -> Result<Option<f32>> {
        Ok(self.meta("assumed_load_w")?.and_then(|v| v.parse().ok()))
    }

    /// Sets or clears the load estimate.
    pub fn set_assumed_load_w(&mut self, watts: Option<f32>) -> Result<()> {
        self.set_meta("assumed_load_w", watts.map(|w| w.to_string()))
    }

    /// When the current battery was installed (user-provided), if known.
    pub fn battery_installed(&self) -> Result<Option<NaiveDate>> {
        Ok(self.meta("battery_installed")?.and_then(|v| v.parse().ok()))
    }

    /// Sets or clears the battery installation date.
    pub fn set_battery_installed(&mut self, date: Option<NaiveDate>) -> Result<()> {
        self.set_meta("battery_installed", date.map(|d| d.to_string()))
    }

    /// All stored daily coverage, oldest first.
    pub fn all_daily(&self) -> Result<Vec<DailyCoverage>> {
        let (Some(first), Some(last)) = (
            NaiveDate::from_ymd_opt(1970, 1, 1),
            NaiveDate::from_ymd_opt(9999, 12, 31),
        ) else {
            return Ok(Vec::new());
        };
        self.daily(first, last)
    }

    /// Time and on-battery state of the newest sample.
    pub fn last_sample(&self) -> Result<Option<(DateTime<Utc>, bool)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT ts_ms, on_battery FROM samples ORDER BY ts_ms DESC LIMIT 1",
                [],
                |r| {
                    Ok((
                        from_ms(r.get(0)?),
                        r.get::<_, Option<bool>>(1)?.unwrap_or(false),
                    ))
                },
            )
            .optional()?)
    }

    /// Replays all stored samples, oldest first (for backfilling analysis).
    pub fn for_each_sample(&self, mut f: impl FnMut(&Snapshot)) -> Result<()> {
        let mut stmt = self.conn.prepare(
            "SELECT ts_ms, status, charge, runtime_s, load_pct, realpower_w, battery_v FROM samples
             ORDER BY ts_ms",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let mut s = Snapshot::empty(from_ms(r.get(0)?));
            s.status = r
                .get::<_, Option<String>>(1)?
                .map(|st| UpsStatus::from_nut(&st).0);
            s.battery_charge_pct = r.get(2)?;
            s.battery_runtime_s = r.get(3)?;
            s.load_pct = r.get(4)?;
            s.realpower_w = r.get(5)?;
            s.battery_voltage_v = r.get(6)?;
            f(&s);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use super::*;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap()
    }

    #[test]
    fn saves_and_updates_outages() {
        let mut s = Store::open_in_memory().unwrap();
        let mut o = Outage::started(t0());
        o.charge_start_pct = Some(100.0);
        o.battery_v_min = Some(12.4);
        o.power_estimated = true;
        o.runtime_test = true;
        o.id = s.save_outage(&o).unwrap();
        assert_eq!(s.open_outage().unwrap(), Some(o.clone()));

        o.end = Some(t0() + Duration::minutes(5));
        o.charge_end_pct = Some(90.0);
        assert_eq!(s.save_outage(&o).unwrap(), o.id);
        assert_eq!(s.open_outage().unwrap(), None);
        assert_eq!(s.recharging_outage(t0()).unwrap(), Some(o.clone()));
        assert_eq!(
            s.recharging_outage(t0() + Duration::hours(1)).unwrap(),
            None
        );
        assert_eq!(s.outages_since(t0(), 10).unwrap(), [o.clone()]);
        assert_eq!(s.outage_count().unwrap(), 1);

        let mut missing = o.clone();
        missing.id = 999;
        assert!(s.save_outage(&missing).is_err());
    }

    #[test]
    fn accumulates_daily_coverage() {
        let mut s = Store::open_in_memory().unwrap();
        let d = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
        let inc = DailyCoverage {
            day: d,
            monitored_s: 10.0,
            on_battery_s: 2.0,
        };
        s.add_daily(std::slice::from_ref(&inc)).unwrap();
        s.add_daily(&[inc]).unwrap();
        let days = s.daily(d, d).unwrap();
        assert_eq!(days[0].monitored_s, 20.0);
        assert_eq!(days[0].on_battery_s, 4.0);

        assert_eq!(s.assumed_load_w().unwrap(), None);
        s.set_assumed_load_w(Some(20.0)).unwrap();
        assert_eq!(s.assumed_load_w().unwrap(), Some(20.0));
        s.set_assumed_load_w(None).unwrap();
        assert_eq!(s.assumed_load_w().unwrap(), None);

        assert_eq!(s.battery_installed().unwrap(), None);
        s.set_battery_installed(Some(d)).unwrap();
        assert_eq!(s.battery_installed().unwrap(), Some(d));
        s.set_battery_installed(None).unwrap();
        assert_eq!(s.battery_installed().unwrap(), None);
    }

    #[test]
    fn replays_samples_in_order() {
        let mut s = Store::open_in_memory().unwrap();
        for (i, ob) in [(2, true), (1, false)] {
            let mut snap = Snapshot::empty(t0() + Duration::seconds(i));
            snap.status = Some(if ob { UpsStatus::OB } else { UpsStatus::OL });
            snap.battery_charge_pct = Some(50.0);
            s.insert_sample(&snap).unwrap();
        }
        let mut seen = Vec::new();
        s.for_each_sample(|snap| seen.push((snap.ts, snap.on_battery())))
            .unwrap();
        assert_eq!(
            seen,
            [
                (t0() + Duration::seconds(1), Some(false)),
                (t0() + Duration::seconds(2), Some(true))
            ]
        );
        assert_eq!(
            s.last_sample().unwrap(),
            Some((t0() + Duration::seconds(2), true))
        );
    }
}
