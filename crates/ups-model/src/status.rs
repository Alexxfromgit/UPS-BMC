use std::fmt;

use bitflags::bitflags;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

bitflags! {
    /// UPS status flags, matching the NUT `ups.status` tokens
    /// (NUT developer guide, "Status data").
    ///
    /// Serialises as the NUT status string (e.g. `"OL CHRG"`), not as a bit
    /// mask, so the wire format stays readable and independent of bit order.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct UpsStatus: u32 {
        /// On line (mains power present).
        const OL      = 1 << 0;
        /// On battery.
        const OB      = 1 << 1;
        /// Low battery.
        const LB      = 1 << 2;
        /// High battery.
        const HB      = 1 << 3;
        /// The battery needs to be replaced.
        const RB      = 1 << 4;
        /// The battery is charging.
        const CHRG    = 1 << 5;
        /// The battery is discharging (inverter is providing load power).
        const DISCHRG = 1 << 6;
        /// UPS bypass circuit is active (no battery protection).
        const BYPASS  = 1 << 7;
        /// UPS is performing runtime calibration (on battery).
        const CAL     = 1 << 8;
        /// UPS is offline: not supplying power to the load.
        const OFF     = 1 << 9;
        /// UPS is overloaded.
        const OVER    = 1 << 10;
        /// UPS is trimming incoming voltage (AVR buck).
        const TRIM    = 1 << 11;
        /// UPS is boosting incoming voltage (AVR boost).
        const BOOST   = 1 << 12;
        /// Forced shutdown. Set by `upsd` on command from the primary
        /// `upsmon`; drivers normally do not report it.
        const FSD     = 1 << 13;
        /// An alarm is active. The NUT framework adds this whenever
        /// `ups.alarm` is non-empty.
        const ALARM   = 1 << 14;
    }
}

const TOKENS: &[(&str, UpsStatus)] = &[
    ("OL", UpsStatus::OL),
    ("OB", UpsStatus::OB),
    ("LB", UpsStatus::LB),
    ("HB", UpsStatus::HB),
    ("RB", UpsStatus::RB),
    ("CHRG", UpsStatus::CHRG),
    ("DISCHRG", UpsStatus::DISCHRG),
    ("BYPASS", UpsStatus::BYPASS),
    ("CAL", UpsStatus::CAL),
    ("OFF", UpsStatus::OFF),
    ("OVER", UpsStatus::OVER),
    ("TRIM", UpsStatus::TRIM),
    ("BOOST", UpsStatus::BOOST),
    ("FSD", UpsStatus::FSD),
    ("ALARM", UpsStatus::ALARM),
];

impl UpsStatus {
    /// Parses a NUT `ups.status` string such as `"OL CHRG"`.
    ///
    /// Unknown tokens are returned separately so callers can log them instead
    /// of silently losing information.
    #[must_use]
    pub fn from_nut(s: &str) -> (Self, Vec<String>) {
        let mut flags = Self::empty();
        let mut unknown = Vec::new();
        for token in s.split_whitespace() {
            match TOKENS.iter().find(|(name, _)| *name == token) {
                Some((_, flag)) => flags |= *flag,
                None => unknown.push(token.to_owned()),
            }
        }
        (flags, unknown)
    }

    /// Formats the flags as a NUT `ups.status` string (canonical token order).
    #[must_use]
    pub fn to_nut(self) -> String {
        self.to_string()
    }

    /// True when the load is being powered from the battery.
    #[must_use]
    pub fn on_battery(self) -> bool {
        self.contains(Self::OB)
    }
}

impl fmt::Display for UpsStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for (name, flag) in TOKENS {
            if self.contains(*flag) {
                if !first {
                    f.write_str(" ")?;
                }
                f.write_str(name)?;
                first = false;
            }
        }
        Ok(())
    }
}

impl Serialize for UpsStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for UpsStatus {
    /// Lenient: unknown tokens are ignored, matching NUT client behaviour.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Self::from_nut(&s).0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_statuses() {
        assert_eq!(
            UpsStatus::from_nut("OL CHRG"),
            (UpsStatus::OL | UpsStatus::CHRG, vec![])
        );
        assert_eq!(
            UpsStatus::from_nut("  OB  DISCHRG LB "),
            (UpsStatus::OB | UpsStatus::DISCHRG | UpsStatus::LB, vec![])
        );
        assert_eq!(UpsStatus::from_nut(""), (UpsStatus::empty(), vec![]));
    }

    #[test]
    fn understands_alarm_and_keeps_unknown_tokens() {
        assert_eq!(
            UpsStatus::from_nut("OL ALARM"),
            (UpsStatus::OL | UpsStatus::ALARM, vec![])
        );
        let (flags, unknown) = UpsStatus::from_nut("OL COMMBAD");
        assert_eq!(flags, UpsStatus::OL);
        assert_eq!(unknown, vec!["COMMBAD".to_owned()]);
    }

    #[test]
    fn round_trips_in_canonical_order() {
        let flags = UpsStatus::DISCHRG | UpsStatus::OB | UpsStatus::LB;
        assert_eq!(flags.to_nut(), "OB LB DISCHRG");
        assert_eq!(UpsStatus::from_nut(&flags.to_nut()).0, flags);
        assert_eq!(UpsStatus::empty().to_nut(), "");
    }

    #[test]
    fn every_flag_has_a_token() {
        let all = TOKENS
            .iter()
            .fold(UpsStatus::empty(), |acc, (_, f)| acc | *f);
        assert_eq!(all, UpsStatus::all());
    }

    #[test]
    fn serialises_as_nut_string() {
        let flags = UpsStatus::OL | UpsStatus::CHRG;
        assert_eq!(serde_json::to_string(&flags).unwrap(), r#""OL CHRG""#);
        assert_eq!(
            serde_json::from_str::<UpsStatus>(r#""OB FOO LB""#).unwrap(),
            UpsStatus::OB | UpsStatus::LB
        );
    }

    #[test]
    fn on_battery_follows_ob() {
        assert!(UpsStatus::OB.on_battery());
        assert!(!(UpsStatus::OL | UpsStatus::CHRG).on_battery());
    }
}
