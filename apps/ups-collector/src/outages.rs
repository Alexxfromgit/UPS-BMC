//! M2 analysis inside the collector: feeds the outage and coverage trackers,
//! persists their results, and restores state after restarts.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use chrono::{Local, Utc};
use tracing::{error, info, warn};
use ups_analysis::{CoverageTracker, OutageTracker, TrackerConfig};
use ups_model::{Outage, Snapshot};
use ups_store::Store;

/// Coverage increments are written in batches.
const FLUSH_EVERY: Duration = Duration::from_secs(10);
/// An ended outage older than this is not resumed for recharge tracking.
const RECHARGE_RESUME_WINDOW: chrono::Duration = chrono::Duration::hours(12);
/// `meta` keys marking the one-time rebuild from samples.
const BACKFILL_STARTED: &str = "outage_backfill_started";
const BACKFILL_DONE: &str = "outage_backfill_done";

pub struct OutageAnalysis {
    tracker: OutageTracker,
    coverage: CoverageTracker<Local>,
    last_flush: Instant,
}

impl OutageAnalysis {
    /// Restores state from storage. On the first run with M2 (no outages or
    /// coverage stored yet), rebuilds both from the samples already recorded.
    pub fn start(store: &mut Store, poll: Duration) -> Self {
        let cfg = TrackerConfig::for_poll_interval(
            chrono::Duration::from_std(poll).unwrap_or(chrono::Duration::seconds(1)),
        );
        let mut me = Self {
            tracker: OutageTracker::new(cfg),
            coverage: CoverageTracker::new(Local, cfg.gap),
            last_flush: Instant::now(),
        };
        if let Err(e) = me.backfill_if_needed(store) {
            warn!(error = %e, "rebuilding outage history from samples failed");
        }
        let open = store.open_outage().ok().flatten();
        let recharging = store
            .recharging_outage(Utc::now() - RECHARGE_RESUME_WINDOW)
            .ok()
            .flatten();
        let last = store.last_sample().ok().flatten();
        me.tracker = OutageTracker::new(cfg);
        me.tracker.resume(open, recharging, last);
        me.coverage = CoverageTracker::new(Local, cfg.gap);
        me.coverage.resume(last);
        me
    }

    fn backfill_if_needed(&mut self, store: &mut Store) -> ups_store::Result<()> {
        // An explicit marker, written last: a backfill interrupted halfway is
        // redone (review M2-8). Databases from before the marker count as done
        // once they have outages or coverage.
        if store.meta(BACKFILL_DONE)?.is_some() {
            return Ok(());
        }
        let has_data = store.outage_count()? > 0 || !store.all_daily()?.is_empty();
        if has_data && store.meta(BACKFILL_STARTED)?.is_none() {
            store.set_meta(BACKFILL_DONE, Some("1".into()))?;
            return Ok(());
        }
        if store.last_sample()?.is_none() {
            store.set_meta(BACKFILL_DONE, Some("1".into()))?;
            return Ok(());
        }
        // Half-done earlier: start over.
        store.clear_analysis()?;
        store.set_meta(BACKFILL_STARTED, Some("1".into()))?;
        // Replay without touching storage; outages get temporary IDs.
        let mut finals: BTreeMap<i64, Outage> = BTreeMap::new();
        let mut samples = 0u64;
        let tracker = &mut self.tracker;
        let coverage = &mut self.coverage;
        store.for_each_sample(|s| {
            samples += 1;
            coverage.feed(s);
            for mut o in tracker.feed(s) {
                if o.id == 0 {
                    o.id = i64::try_from(finals.len()).unwrap_or(i64::MAX) + 1;
                    tracker.assign_id(o.id);
                }
                finals.insert(o.id, o);
            }
        })?;
        for mut o in finals.into_values() {
            o.id = 0;
            store.save_outage(&o)?;
        }
        store.add_daily(&self.coverage.take())?;
        store.set_meta(BACKFILL_DONE, Some("1".into()))?;
        store.set_meta(BACKFILL_STARTED, None)?;
        info!(
            samples,
            outages = store.outage_count()?,
            "rebuilt outage history from stored samples"
        );
        Ok(())
    }

    /// Feeds one snapshot; returns the outages that changed (as stored).
    pub fn on_snapshot(&mut self, store: &mut Store, s: &Snapshot) -> Vec<Outage> {
        self.coverage.feed(s);
        let mut stored = Vec::new();
        for mut o in self.tracker.feed(s) {
            match store.save_outage(&o) {
                Ok(id) => {
                    if o.id == 0 {
                        o.id = id;
                        self.tracker.assign_id(id);
                    }
                    stored.push(o);
                }
                Err(e) => error!(error = %e, "could not store outage"),
            }
        }
        if self.last_flush.elapsed() >= FLUSH_EVERY {
            self.flush(store);
        }
        stored
    }

    /// Load estimate used when the UPS reports 0 W.
    pub fn set_assumed_power(&mut self, watts: Option<f32>) {
        self.tracker.set_assumed_power(watts);
    }

    /// While set, the ongoing outage is tagged as a guided runtime test.
    pub fn set_testing(&mut self, testing: bool) {
        self.tracker.set_testing(testing);
    }

    /// Removes the runtime-test tag from the current or just-ended outage
    /// (test cancelled or interrupted). Returns the stored outages.
    pub fn clear_test_tag(&mut self, store: &mut Store) -> Vec<Outage> {
        self.tracker
            .clear_test_tag()
            .into_iter()
            .filter(|o| o.id != 0)
            .filter_map(|o| match store.save_outage(&o) {
                Ok(_) => Some(o),
                Err(e) => {
                    error!(error = %e, "could not store outage");
                    None
                }
            })
            .collect()
    }

    /// Writes pending coverage increments.
    pub fn flush(&mut self, store: &mut Store) {
        self.last_flush = Instant::now();
        if let Err(e) = store.add_daily(&self.coverage.take()) {
            error!(error = %e, "could not store daily coverage");
        }
    }
}
