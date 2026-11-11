//! The guided runtime test: a controlled discharge to measure the battery.
//!
//! The user arms the test, then unplugs the UPS from the mains. UPS-BMC
//! follows the discharge and asks the user to restore power once the charge
//! reaches the stop level. UPS-BMC never commands the UPS: the test is just a
//! normal outage started by hand, tagged so health estimation can trust it.

use chrono::{DateTime, Duration, Utc};
use ups_model::{RuntimeTestPhase, RuntimeTestStatus, Snapshot, UpsStatus};

/// An armed test is cancelled if the UPS isn't unplugged within this time.
const ARM_TIMEOUT: Duration = Duration::minutes(30);
/// Repeat the "restore power" reminder this often.
const REMIND_EVERY: Duration = Duration::minutes(2);
/// The battery must be at least this full to start a test.
pub const MIN_START_PCT: f32 = 95.0;
/// Allowed stop levels.
pub const STOP_RANGE: std::ops::RangeInclusive<f32> = 20.0..=90.0;

/// What happened to the test on a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub enum TestNotice {
    /// The UPS went on battery: the measured discharge has begun.
    Started { charge_pct: Option<f32> },
    /// The stop level was reached (or the UPS reports low battery).
    RestorePower {
        charge_pct: Option<f32>,
        reminder: bool,
    },
    /// Mains power is back: the test is complete.
    Finished,
    /// Nobody unplugged the UPS in time.
    Expired,
}

/// State of the guided runtime test.
#[derive(Debug, Clone, Default)]
pub struct RuntimeTest {
    status: Option<RuntimeTestStatus>,
    last_reminder: Option<DateTime<Utc>>,
}

impl RuntimeTest {
    /// The test in progress, if any.
    #[must_use]
    pub fn status(&self) -> Option<&RuntimeTestStatus> {
        self.status.as_ref()
    }

    /// True while the battery is being discharged for the test.
    #[must_use]
    pub fn discharging(&self) -> bool {
        self.status
            .as_ref()
            .is_some_and(|s| s.phase != RuntimeTestPhase::Armed)
    }

    /// Arms a test. `latest` is the newest reading of the UPS.
    ///
    /// # Errors
    /// A human-readable reason when a test can't start now.
    pub fn arm(
        &mut self,
        stop_at_pct: f32,
        latest: Option<&Snapshot>,
        now: DateTime<Utc>,
    ) -> Result<(), String> {
        if self.status.is_some() {
            return Err("A runtime test is already in progress.".into());
        }
        if !STOP_RANGE.contains(&stop_at_pct) {
            return Err(format!(
                "The stop level must be between {:.0} % and {:.0} %.",
                STOP_RANGE.start(),
                STOP_RANGE.end()
            ));
        }
        let Some(s) = latest.filter(|s| now - s.ts < Duration::seconds(30)) else {
            return Err("The UPS is not connected.".into());
        };
        if s.on_battery() != Some(false) {
            return Err("The UPS must be on mains power to start a test.".into());
        }
        match s.battery_charge_pct {
            Some(c) if c >= MIN_START_PCT => {}
            Some(c) => {
                return Err(format!(
                    "Wait until the battery is fully charged (now {c:.0} %): a test from a partial \
                     charge can't be compared with others."
                ));
            }
            None => return Err("The UPS doesn't report its charge.".into()),
        }
        self.status = Some(RuntimeTestStatus {
            phase: RuntimeTestPhase::Armed,
            armed_at: now,
            stop_at_pct,
            started_at: None,
            charge_start_pct: None,
        });
        self.last_reminder = None;
        Ok(())
    }

    /// Cancels the test. Returns false when none was in progress.
    pub fn cancel(&mut self) -> bool {
        self.status.take().is_some()
    }

    /// Follows the test on a new reading.
    pub fn on_snapshot(&mut self, s: &Snapshot) -> Vec<TestNotice> {
        let Some(st) = self.status.as_mut() else {
            return Vec::new();
        };
        let Some(on_battery) = s.on_battery() else {
            return Vec::new();
        };
        let mut notices = Vec::new();
        match st.phase {
            RuntimeTestPhase::Armed => {
                if on_battery {
                    st.phase = RuntimeTestPhase::Running;
                    st.started_at = Some(s.ts);
                    st.charge_start_pct = s.battery_charge_pct;
                    notices.push(TestNotice::Started {
                        charge_pct: s.battery_charge_pct,
                    });
                } else if s.ts - st.armed_at > ARM_TIMEOUT {
                    self.status = None;
                    return vec![TestNotice::Expired];
                }
            }
            RuntimeTestPhase::Running | RuntimeTestPhase::RestorePower if !on_battery => {
                self.status = None;
                return vec![TestNotice::Finished];
            }
            RuntimeTestPhase::Running => {}
            RuntimeTestPhase::RestorePower => {
                if self.last_reminder.is_none_or(|t| s.ts - t >= REMIND_EVERY) {
                    self.last_reminder = Some(s.ts);
                    notices.push(TestNotice::RestorePower {
                        charge_pct: s.battery_charge_pct,
                        reminder: true,
                    });
                }
            }
        }
        if st.phase == RuntimeTestPhase::Running {
            let low = s.status.is_some_and(|f| f.contains(UpsStatus::LB));
            if low || s.battery_charge_pct.is_some_and(|c| c <= st.stop_at_pct) {
                st.phase = RuntimeTestPhase::RestorePower;
                self.last_reminder = Some(s.ts);
                notices.push(TestNotice::RestorePower {
                    charge_pct: s.battery_charge_pct,
                    reminder: false,
                });
            }
        }
        notices
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    fn t(secs: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap() + Duration::seconds(secs)
    }

    fn snap(secs: i64, on_battery: bool, charge: f32) -> Snapshot {
        let mut s = Snapshot::empty(t(secs));
        s.status = Some(if on_battery {
            UpsStatus::OB | UpsStatus::DISCHRG
        } else {
            UpsStatus::OL
        });
        s.battery_charge_pct = Some(charge);
        s
    }

    #[test]
    fn refuses_to_start_from_a_bad_state() {
        let mut rt = RuntimeTest::default();
        assert!(
            rt.arm(50.0, None, t(0))
                .unwrap_err()
                .contains("not connected")
        );
        let stale = snap(0, false, 100.0);
        assert!(rt.arm(50.0, Some(&stale), t(60)).is_err());
        let ob = snap(0, true, 100.0);
        assert!(rt.arm(50.0, Some(&ob), t(1)).unwrap_err().contains("mains"));
        let partial = snap(0, false, 80.0);
        assert!(
            rt.arm(50.0, Some(&partial), t(1))
                .unwrap_err()
                .contains("80 %")
        );
        let full = snap(0, false, 100.0);
        assert!(
            rt.arm(10.0, Some(&full), t(1))
                .unwrap_err()
                .contains("between")
        );
        assert!(rt.status().is_none());
        rt.arm(50.0, Some(&full), t(1)).unwrap();
        assert!(
            rt.arm(50.0, Some(&full), t(1))
                .unwrap_err()
                .contains("already")
        );
    }

    #[test]
    fn follows_a_test_to_the_end() {
        let mut rt = RuntimeTest::default();
        rt.arm(50.0, Some(&snap(0, false, 100.0)), t(0)).unwrap();
        assert_eq!(rt.status().unwrap().phase, RuntimeTestPhase::Armed);
        assert_eq!(
            rt.on_snapshot(&snap(5, false, 100.0)),
            Vec::<TestNotice>::new()
        );
        assert!(!rt.discharging());

        let n = rt.on_snapshot(&snap(10, true, 100.0));
        assert_eq!(
            n,
            [TestNotice::Started {
                charge_pct: Some(100.0)
            }]
        );
        assert!(rt.discharging());
        assert_eq!(rt.status().unwrap().started_at, Some(t(10)));
        assert_eq!(
            rt.on_snapshot(&snap(20, true, 70.0)),
            Vec::<TestNotice>::new()
        );

        let n = rt.on_snapshot(&snap(30, true, 50.0));
        assert_eq!(
            n,
            [TestNotice::RestorePower {
                charge_pct: Some(50.0),
                reminder: false
            }]
        );
        assert_eq!(rt.status().unwrap().phase, RuntimeTestPhase::RestorePower);
        assert_eq!(
            rt.on_snapshot(&snap(60, true, 49.0)),
            Vec::<TestNotice>::new()
        );
        let n = rt.on_snapshot(&snap(150, true, 47.0));
        assert_eq!(
            n,
            [TestNotice::RestorePower {
                charge_pct: Some(47.0),
                reminder: true
            }]
        );

        assert_eq!(
            rt.on_snapshot(&snap(160, false, 47.0)),
            [TestNotice::Finished]
        );
        assert!(rt.status().is_none());
    }

    #[test]
    fn low_battery_stops_early_and_unused_tests_expire() {
        let mut rt = RuntimeTest::default();
        rt.arm(30.0, Some(&snap(0, false, 100.0)), t(0)).unwrap();
        rt.on_snapshot(&snap(1, true, 100.0));
        let mut low = snap(2, true, 60.0);
        low.status = Some(UpsStatus::OB | UpsStatus::LB);
        assert!(matches!(
            rt.on_snapshot(&low)[..],
            [TestNotice::RestorePower { .. }]
        ));

        let mut rt = RuntimeTest::default();
        rt.arm(50.0, Some(&snap(0, false, 100.0)), t(0)).unwrap();
        assert_eq!(
            rt.on_snapshot(&snap(1700, false, 100.0)),
            Vec::<TestNotice>::new()
        );
        assert_eq!(
            rt.on_snapshot(&snap(1900, false, 100.0)),
            [TestNotice::Expired]
        );
        assert!(rt.status().is_none());
        assert!(!rt.cancel());
    }
}
