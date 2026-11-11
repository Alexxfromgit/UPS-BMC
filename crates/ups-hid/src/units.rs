//! HID unit codes commonly used by UPSes.
//!
//! A HID unit is a packed set of nibbles: system (1 = SI Linear), length,
//! mass, time, temperature, current and luminous intensity. Only the handful
//! of combinations seen on power devices are named here.
//!
//! HID "SI Linear" is really CGS: length in centimetres and mass in grams. A
//! voltage declared in correct SI-Linear form therefore carries a unit
//! exponent of 7 (1 V = 10^7 g·cm²·s⁻³·A⁻¹). Use [`normalized_exponent`] to
//! get the power of ten relative to the everyday unit (V, W, ...).

pub const NONE: u32 = 0x0000_0000;
pub const VOLT: u32 = 0x00F0_D121;
pub const AMPERE: u32 = 0x0010_0001;
/// Watt and volt-ampere share the same dimension.
pub const WATT: u32 = 0x0000_D121;
pub const SECOND: u32 = 0x0000_1001;
pub const KELVIN: u32 = 0x0001_0001;
pub const HERTZ: u32 = 0x0000_F001;
pub const AMPERE_SECOND: u32 = 0x0010_1001;

/// Short symbol for a known unit code.
#[must_use]
pub fn symbol(unit: u32) -> Option<&'static str> {
    Some(match unit {
        VOLT => "V",
        AMPERE => "A",
        WATT => "W/VA",
        SECOND => "s",
        KELVIN => "K",
        HERTZ => "Hz",
        AMPERE_SECOND => "As",
        _ => return None,
    })
}

/// Power of ten that turns a value into the everyday unit returned by
/// [`symbol`]. For V and W/VA this compensates for the CGS base units
/// (exponent − 7); for the other known units it equals the declared exponent.
/// Returns `None` for unknown units.
#[must_use]
pub fn normalized_exponent(unit: u32, declared: i8) -> Option<i8> {
    match unit {
        VOLT | WATT => Some(declared.saturating_sub(7)),
        AMPERE | SECOND | KELVIN | HERTZ | AMPERE_SECOND => Some(declared),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_cgs_based_units() {
        assert_eq!(normalized_exponent(VOLT, 7), Some(0));
        assert_eq!(normalized_exponent(VOLT, 5), Some(-2));
        assert_eq!(normalized_exponent(WATT, 7), Some(0));
        assert_eq!(normalized_exponent(SECOND, 0), Some(0));
        assert_eq!(normalized_exponent(0x1234, 0), None);
    }
}
