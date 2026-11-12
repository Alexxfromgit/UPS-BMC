//! Which collector the UI talks to: `--collector` / `--token-file`, the
//! `UPS_BMC_TOKEN` environment variable, or `ui.toml` in the per-user
//! configuration directory (`%APPDATA%\UPS-BMC\config\ui.toml` on Windows).
//! The file lets autostart (which passes only `--minimized`) reach a remote
//! collector, e.g. on a Raspberry Pi (ADR-0005):
//!
//! ```toml
//! collector = "ups-pi.lan:47800"
//! token = "the token printed by `ups-collector --generate-token`"
//! ```

use std::path::{Path, PathBuf};

use serde::Deserialize;
use ups_api::{DEFAULT_PORT, TOKEN_ENV};

use crate::client::Endpoint;

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct UiConfig {
    collector: Option<String>,
    token: Option<String>,
}

fn path() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "UPS-BMC").map(|d| d.config_dir().join("ui.toml"))
}

fn load(path: &Path) -> Result<UiConfig, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UiConfig::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The collector to use, and a problem with the configuration to show the
/// user, if any (the UI still starts, with what could be read).
pub fn endpoint(
    collector: Option<String>,
    token_file: Option<&Path>,
) -> (Endpoint, Option<String>) {
    let (file, mut error) = match path().map(|p| load(&p)) {
        Some(Ok(c)) => (c, None),
        Some(Err(e)) => (UiConfig::default(), Some(e)),
        None => (UiConfig::default(), None),
    };
    let token = match token_file {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(t) => Some(t),
            Err(e) => {
                error = Some(format!("{}: {e}", p.display()));
                None
            }
        },
        None => std::env::var(TOKEN_ENV).ok().or(file.token),
    };
    let endpoint = Endpoint {
        address: collector
            .or(file.collector)
            .map(|a| a.trim().to_owned())
            .filter(|a| !a.is_empty())
            .unwrap_or_else(|| format!("127.0.0.1:{DEFAULT_PORT}")),
        token: token.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()),
    };
    (endpoint, error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_file() {
        let c: UiConfig = toml::from_str("collector = \"pi:47800\"\ntoken = \"t\"").unwrap();
        assert_eq!(c.collector.as_deref(), Some("pi:47800"));
        assert!(toml::from_str::<UiConfig>("colector = \"typo\"").is_err());
    }

    #[test]
    fn tells_local_from_remote_collectors() {
        let ep = |a: &str| Endpoint {
            address: a.into(),
            token: None,
        };
        for local in [
            "127.0.0.1:47800",
            "localhost:47800",
            "[::1]:47800",
            "127.0.0.2:1",
        ] {
            assert!(ep(local).is_local(), "{local}");
        }
        for remote in ["ups-pi.lan:47800", "192.168.1.20:47800", "[fe80::1]:47800"] {
            assert!(!ep(remote).is_local(), "{remote}");
        }
    }
}
