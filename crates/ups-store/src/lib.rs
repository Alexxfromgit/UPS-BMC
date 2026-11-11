//! SQLite storage for telemetry samples, events and device metadata.
//!
//! * One row per sample (1 s polling ≈ 86,400 rows/day). Raw samples are
//!   pruned after the configured retention period. Downsampled long-term
//!   history arrives with M2.
//! * Events are kept forever. They are the basis of outage analysis (V2).
//! * WAL mode: the collector writes while API handlers read through a second
//!   connection.

use std::path::Path;

use chrono::{DateTime, TimeZone, Utc};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use ups_model::{DeviceInfo, Event, EventKind, HistoryPoint, Snapshot, UpsStatus};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored data is invalid: {0}")]
    Corrupt(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Schema migrations. Index + 1 is the `user_version` after applying.
const MIGRATIONS: &[&str] = &[
    r"
    CREATE TABLE samples (
        ts_ms       INTEGER PRIMARY KEY,
        status      TEXT,
        on_battery  INTEGER,
        charge      REAL,
        runtime_s   INTEGER,
        battery_v   REAL,
        load_pct    REAL,
        input_v     REAL,
        input_hz    REAL,
        output_v    REAL,
        output_hz   REAL,
        realpower_w REAL,
        power_va    REAL
    ) WITHOUT ROWID;
    CREATE TABLE events (
        id       INTEGER PRIMARY KEY AUTOINCREMENT,
        ts_ms    INTEGER NOT NULL,
        kind     TEXT NOT NULL,
        severity TEXT NOT NULL,
        message  TEXT NOT NULL
    );
    CREATE INDEX events_ts ON events (ts_ms);
    CREATE TABLE meta (
        key   TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );
",
    r"
    CREATE TABLE outages (
        id                  INTEGER PRIMARY KEY AUTOINCREMENT,
        start_ms            INTEGER NOT NULL,
        start_earliest_ms   INTEGER,
        end_ms              INTEGER,
        end_latest_ms       INTEGER,
        charge_start        REAL,
        charge_end          REAL,
        charge_min          REAL,
        runtime_est_start_s INTEGER,
        avg_load_pct        REAL,
        avg_power_w         REAL,
        energy_wh           REAL,
        recharged_ms        INTEGER
    );
    CREATE INDEX outages_start ON outages (start_ms);
    CREATE TABLE daily (
        day          TEXT PRIMARY KEY,
        monitored_s  REAL NOT NULL DEFAULT 0,
        on_battery_s REAL NOT NULL DEFAULT 0
    );
",
    r"
    ALTER TABLE outages ADD COLUMN battery_v_start REAL;
    ALTER TABLE outages ADD COLUMN battery_v_end REAL;
    ALTER TABLE outages ADD COLUMN battery_v_min REAL;
    ALTER TABLE outages ADD COLUMN power_estimated INTEGER NOT NULL DEFAULT 0;
",
    r"
    ALTER TABLE outages ADD COLUMN runtime_test INTEGER NOT NULL DEFAULT 0;
",
];

mod analysis;

const META_DEVICE: &str = "device";

/// A connection to the UPS-BMC database.
#[derive(Debug)]
pub struct Store {
    conn: Connection,
}

fn ms(ts: DateTime<Utc>) -> i64 {
    ts.timestamp_millis()
}

fn from_ms(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap_or_default()
}

#[allow(clippy::cast_possible_truncation)] // averages of f32 values
fn f32_opt(v: Option<f64>) -> Option<f32> {
    v.map(|v| v as f32)
}

impl Store {
    /// Opens (or creates) the database at `path` for reading and writing.
    pub fn open(path: &Path) -> Result<Self> {
        Self::init(Connection::open(path)?)
    }

    /// Opens an existing database read-only (used by API readers).
    pub fn open_read_only(path: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self { conn })
    }

    /// A private in-memory database (tests).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        let mut store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?;
        let current = usize::try_from(version).unwrap_or(0);
        if current > MIGRATIONS.len() {
            return Err(StoreError::Corrupt(format!(
                "database schema v{current} is newer than this program (v{})",
                MIGRATIONS.len()
            )));
        }
        for (i, sql) in MIGRATIONS.iter().enumerate().skip(current) {
            let tx = self.conn.transaction()?;
            tx.execute_batch(sql)?;
            tx.pragma_update(
                None,
                "user_version",
                i64::try_from(i + 1).unwrap_or(i64::MAX),
            )?;
            tx.commit()?;
        }
        Ok(())
    }

    /// Stores one sample (a later sample with the same millisecond replaces it).
    pub fn insert_sample(&mut self, s: &Snapshot) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO samples
             (ts_ms, status, on_battery, charge, runtime_s, battery_v, load_pct,
              input_v, input_hz, output_v, output_hz, realpower_w, power_va)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                ms(s.ts),
                s.status.map(UpsStatus::to_nut),
                s.on_battery(),
                s.battery_charge_pct,
                s.battery_runtime_s,
                s.battery_voltage_v,
                s.load_pct,
                s.input_voltage_v,
                s.input_frequency_hz,
                s.output_voltage_v,
                s.output_frequency_hz,
                s.realpower_w,
                s.power_va,
            ],
        )?;
        Ok(())
    }

    /// Stores an event and returns it with its ID.
    pub fn insert_event(&mut self, event: &Event) -> Result<Event> {
        self.conn.execute(
            "INSERT INTO events (ts_ms, kind, severity, message) VALUES (?1, ?2, ?3, ?4)",
            params![
                ms(event.ts),
                event.kind.as_str(),
                serde_json::to_value(event.severity)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                event.message,
            ],
        )?;
        Ok(Event {
            id: self.conn.last_insert_rowid(),
            ..event.clone()
        })
    }

    /// The newest `limit` events, newest first.
    pub fn recent_events(&self, limit: usize) -> Result<Vec<Event>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, ts_ms, kind, message FROM events ORDER BY ts_ms DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut events = Vec::new();
        for row in rows {
            let (id, ts, kind, message) = row?;
            let kind = EventKind::parse(&kind)
                .ok_or_else(|| StoreError::Corrupt(format!("unknown event kind {kind}")))?;
            events.push(Event {
                id,
                ..Event::new(from_ms(ts), kind, message)
            });
        }
        Ok(events)
    }

    /// History between `from` and `to`, averaged into at most about
    /// `max_points` buckets (at least 1 s each), oldest first.
    pub fn history(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        max_points: usize,
    ) -> Result<Vec<HistoryPoint>> {
        let (from, to) = (ms(from), ms(to));
        let span = (to - from).max(1);
        let points = i64::try_from(max_points.max(1)).unwrap_or(i64::MAX);
        let bucket = (span / points).max(1000);
        let mut stmt = self.conn.prepare(
            "SELECT MIN(ts_ms), MAX(on_battery), AVG(charge), AVG(runtime_s), AVG(battery_v),
                    AVG(load_pct), AVG(input_v), AVG(output_v), AVG(realpower_w)
             FROM samples
             WHERE ts_ms >= ?1 AND ts_ms <= ?2
             GROUP BY (ts_ms - ?1) / ?3
             ORDER BY 1",
        )?;
        let rows = stmt.query_map(params![from, to, bucket], |r| {
            Ok(HistoryPoint {
                ts: from_ms(r.get(0)?),
                on_battery: r.get::<_, Option<bool>>(1)?,
                battery_charge_pct: f32_opt(r.get(2)?),
                battery_runtime_s: f32_opt(r.get(3)?),
                battery_voltage_v: f32_opt(r.get(4)?),
                load_pct: f32_opt(r.get(5)?),
                input_voltage_v: f32_opt(r.get(6)?),
                output_voltage_v: f32_opt(r.get(7)?),
                realpower_w: f32_opt(r.get(8)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Deletes samples older than `cutoff`. Returns the number removed.
    pub fn prune_samples(&mut self, cutoff: DateTime<Utc>) -> Result<usize> {
        Ok(self
            .conn
            .execute("DELETE FROM samples WHERE ts_ms < ?1", [ms(cutoff)])?)
    }

    /// Number of stored samples.
    pub fn sample_count(&self) -> Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM samples", [], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Remembers the last connected device.
    pub fn set_device(&mut self, device: &DeviceInfo) -> Result<()> {
        let json = serde_json::to_string(device).map_err(|e| StoreError::Corrupt(e.to_string()))?;
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            params![META_DEVICE, json],
        )?;
        Ok(())
    }

    /// The last connected device, if any.
    pub fn device(&self) -> Result<Option<DeviceInfo>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = ?1",
                [META_DEVICE],
                |r| r.get(0),
            )
            .optional()?;
        json.map(|j| serde_json::from_str(&j).map_err(|e| StoreError::Corrupt(e.to_string())))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    fn sample(ts: DateTime<Utc>, charge: f32, on_battery: bool) -> Snapshot {
        let mut s = Snapshot::empty(ts);
        s.status = Some(if on_battery {
            UpsStatus::OB | UpsStatus::DISCHRG
        } else {
            UpsStatus::OL
        });
        s.battery_charge_pct = Some(charge);
        s.load_pct = Some(10.0);
        s
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap()
    }

    #[test]
    fn migrates_and_reopens() {
        let dir = std::env::temp_dir().join(format!("ups-store-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("db.sqlite");
        {
            let mut s = Store::open(&path).unwrap();
            s.insert_sample(&sample(t0(), 90.0, false)).unwrap();
        }
        let s = Store::open(&path).unwrap();
        assert_eq!(s.sample_count().unwrap(), 1);
        let r = Store::open_read_only(&path).unwrap();
        assert_eq!(r.sample_count().unwrap(), 1);
        drop((s, r));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn buckets_history() {
        let mut s = Store::open_in_memory().unwrap();
        // 10 samples, 1 s apart; on battery for the last 3.
        for i in 0..10_u8 {
            let on_battery = i >= 7;
            s.insert_sample(&sample(
                t0() + Duration::seconds(i64::from(i)),
                100.0 - f32::from(i),
                on_battery,
            ))
            .unwrap();
        }
        let all = s.history(t0(), t0() + Duration::seconds(9), 100).unwrap();
        assert_eq!(all.len(), 10);
        assert_eq!(all[0].battery_charge_pct, Some(100.0));
        assert_eq!(all[9].on_battery, Some(true));

        // Two 5-second buckets.
        let two = s.history(t0(), t0() + Duration::seconds(10), 2).unwrap();
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].battery_charge_pct, Some(98.0)); // avg of 100..96
        assert_eq!(two[0].on_battery, Some(false));
        assert_eq!(two[1].on_battery, Some(true));
    }

    #[test]
    fn stores_events_newest_first() {
        let mut s = Store::open_in_memory().unwrap();
        let a = s
            .insert_event(&Event::new(t0(), EventKind::PowerLost, "lost"))
            .unwrap();
        let b = s
            .insert_event(&Event::new(
                t0() + Duration::seconds(5),
                EventKind::PowerRestored,
                "back",
            ))
            .unwrap();
        assert!(b.id > a.id);
        let events = s.recent_events(10).unwrap();
        assert_eq!(events, [b, a]);
        assert_eq!(s.recent_events(1).unwrap().len(), 1);
    }

    #[test]
    fn prunes_old_samples_and_keeps_device() {
        let mut s = Store::open_in_memory().unwrap();
        s.insert_sample(&sample(t0(), 50.0, false)).unwrap();
        s.insert_sample(&sample(t0() + Duration::days(40), 50.0, false))
            .unwrap();
        assert_eq!(s.prune_samples(t0() + Duration::days(10)).unwrap(), 1);
        assert_eq!(s.sample_count().unwrap(), 1);

        assert_eq!(s.device().unwrap(), None);
        let d = DeviceInfo {
            source: "sim".into(),
            model: Some("Sim".into()),
            ..DeviceInfo::default()
        };
        s.set_device(&d).unwrap();
        assert_eq!(s.device().unwrap(), Some(d));
    }
}
