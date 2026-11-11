//! "Start with Windows": a per-user `Run` registry value that launches the UI
//! minimised at sign-in. The UI then starts the collector. Changed only when
//! the user flips the switch in Settings, or ticks the option in the installer,
//! which writes the same value.

/// Registry value name (shared with the installer, `installer/ups-bmc.iss`).
pub const VALUE_NAME: &str = "UPS-BMC";

/// The command line registered for autostart.
#[must_use]
pub fn command_line(exe: &std::path::Path) -> String {
    format!("\"{}\" --minimized", exe.display())
}

#[cfg(windows)]
mod imp {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};

    pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    pub fn supported() -> bool {
        true
    }

    /// True when the UPS-BMC value exists under `key` (any path).
    pub fn is_enabled_at(key: &str) -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(key, KEY_READ)
            .and_then(|k| k.get_value::<String, _>(super::VALUE_NAME))
            .is_ok()
    }

    pub fn set_enabled_at(key: &str, enabled: bool, command: &str) -> std::io::Result<()> {
        let (key, _) = RegKey::predef(HKEY_CURRENT_USER).create_subkey(key)?;
        if enabled {
            key.set_value(super::VALUE_NAME, &command)
        } else {
            match key.delete_value(super::VALUE_NAME) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        }
    }

    pub fn is_enabled() -> bool {
        is_enabled_at(RUN_KEY)
    }

    pub fn set_enabled(enabled: bool) -> std::io::Result<()> {
        let exe = std::env::current_exe()?;
        set_enabled_at(RUN_KEY, enabled, &super::command_line(&exe))
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn supported() -> bool {
        false
    }
    pub fn is_enabled() -> bool {
        false
    }
    pub fn set_enabled(_enabled: bool) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "autostart is only implemented for Windows",
        ))
    }
}

pub use imp::{is_enabled, set_enabled, supported};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_the_executable_path() {
        let p = std::path::Path::new(r"C:\Users\me\AppData\Local\Programs\UPS-BMC\ups-bmc.exe");
        assert_eq!(
            command_line(p),
            r#""C:\Users\me\AppData\Local\Programs\UPS-BMC\ups-bmc.exe" --minimized"#
        );
    }

    /// Round trip under a private test key; the real `Run` key is never touched.
    #[cfg(windows)]
    #[test]
    fn registry_round_trip_on_a_test_key() {
        use winreg::RegKey;
        use winreg::enums::HKEY_CURRENT_USER;

        let key = format!(r"Software\UPS-BMC-tests\run-{}", std::process::id());
        assert!(!imp::is_enabled_at(&key));
        imp::set_enabled_at(&key, true, "\"x.exe\" --minimized").unwrap();
        assert!(imp::is_enabled_at(&key));
        imp::set_enabled_at(&key, false, "").unwrap();
        assert!(!imp::is_enabled_at(&key));
        // Disabling twice is fine.
        imp::set_enabled_at(&key, false, "").unwrap();
        RegKey::predef(HKEY_CURRENT_USER)
            .delete_subkey_all(r"Software\UPS-BMC-tests")
            .unwrap();
    }
}
