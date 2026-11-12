//! The optional collector configuration file (`--config`, TOML) and the API
//! token (`--token-file` or `UPS_BMC_TOKEN`). See ADR-0005 and
//! `deploy/raspberry-pi/collector.toml.example`.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use ups_api::EventKind;

/// A secret from the configuration: never printed in logs or `Debug` output.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(transparent)]
pub struct Secret(String);

impl Secret {
    #[cfg(test)]
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("\"***\"")
    }
}

/// The whole file.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileConfig {
    #[serde(default)]
    pub notify: NotifyConfig,
}

/// `[notify]`: remote notification channels. Any number may be configured.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotifyConfig {
    /// Event kinds to send (`power_lost`, `low_battery`, ...). Default: the
    /// same events the desktop UI shows notifications for.
    pub events: Option<Vec<String>>,
    pub ntfy: Option<NtfyConfig>,
    pub telegram: Option<TelegramConfig>,
    pub email: Option<EmailConfig>,
}

/// `[notify.ntfy]`
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NtfyConfig {
    /// Topic URL, e.g. `https://ntfy.sh/my-secret-ups-topic`.
    pub url: String,
    /// Access token for protected topics.
    pub token: Option<Secret>,
}

/// `[notify.telegram]`
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramConfig {
    /// Bot token from @`BotFather`.
    pub bot_token: Secret,
    /// Chat to post to (a number, or `@channelname`).
    pub chat_id: ChatId,
    /// Bot API base URL (tests, or a self-hosted Bot API server).
    #[serde(default = "telegram_api")]
    pub api_url: String,
}

fn telegram_api() -> String {
    "https://api.telegram.org".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum ChatId {
    Number(i64),
    Name(String),
}

impl std::fmt::Display for ChatId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{n}"),
            Self::Name(s) => f.write_str(s),
        }
    }
}

/// `[notify.email]`
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailConfig {
    pub smtp_host: String,
    /// Default: 587 for `starttls`, 465 for `tls`, 25 for `none`.
    pub smtp_port: Option<u16>,
    #[serde(default)]
    pub security: SmtpSecurity,
    pub username: Option<String>,
    pub password: Option<Secret>,
    /// Sender, e.g. `UPS-BMC <ups@example.com>`.
    pub from: String,
    /// Recipients.
    pub to: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SmtpSecurity {
    /// Plain connection upgraded with STARTTLS (required).
    #[default]
    Starttls,
    /// TLS from the start (SMTPS).
    Tls,
    /// No encryption: only for a relay on this machine or a trusted LAN.
    None,
}

impl FileConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let config: Self =
            toml::from_str(&text).with_context(|| format!("in {}", path.display()))?;
        config.notify.event_kinds()?;
        Ok(config)
    }
}

impl NotifyConfig {
    /// The event kinds to send.
    pub fn event_kinds(&self) -> Result<Option<Vec<EventKind>>> {
        let Some(names) = &self.events else {
            return Ok(None);
        };
        names
            .iter()
            .map(|n| {
                EventKind::parse(n)
                    .with_context(|| format!("unknown event kind `{n}` in notify.events"))
            })
            .collect::<Result<Vec<_>>>()
            .map(Some)
    }
}

/// Shortest token accepted (characters).
const MIN_TOKEN: usize = 16;

/// Reads the API token from `file`, else from `UPS_BMC_TOKEN`.
pub fn load_token(file: Option<&Path>) -> Result<Option<Secret>> {
    let token = match file {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("reading the token file {}", path.display()))?,
        ),
        None => std::env::var(ups_api::TOKEN_ENV).ok(),
    };
    let Some(token) = token.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()) else {
        return Ok(None);
    };
    if token.len() < MIN_TOKEN || !token.bytes().all(|b| b.is_ascii_graphic()) {
        bail!(
            "the API token must be at least {MIN_TOKEN} printable ASCII characters without \
             spaces; create one with `ups-collector --generate-token`"
        );
    }
    Ok(Some(Secret(token)))
}

/// A new random token: 32 bytes as 64 hex digits.
pub fn generate_token() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("no random numbers: {e}"))?;
    Ok(bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
        s
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_full_file() {
        let c: FileConfig = toml::from_str(
            r#"
            [notify]
            events = ["power_lost", "power_restored"]
            [notify.ntfy]
            url = "https://ntfy.sh/topic"
            [notify.telegram]
            bot_token = "123:abc"
            chat_id = -100123
            [notify.email]
            smtp_host = "smtp.example.com"
            security = "tls"
            username = "me"
            password = "pw"
            from = "UPS <ups@example.com>"
            to = ["me@example.com"]
            "#,
        )
        .unwrap();
        let n = &c.notify;
        assert_eq!(
            n.event_kinds().unwrap(),
            Some(vec![EventKind::PowerLost, EventKind::PowerRestored])
        );
        let t = n.telegram.as_ref().unwrap();
        assert_eq!(t.chat_id.to_string(), "-100123");
        assert_eq!(t.api_url, "https://api.telegram.org");
        let e = n.email.as_ref().unwrap();
        assert_eq!(e.security, SmtpSecurity::Tls);
        // Secrets stay out of debug output (and so out of logs).
        let debug = format!("{c:?}");
        assert!(
            !debug.contains("123:abc") && !debug.contains("\"pw\""),
            "{debug}"
        );
    }

    /// The shipped example parses as is, and with every setting uncommented.
    #[test]
    fn example_file_is_valid() {
        let example = include_str!("../../../deploy/raspberry-pi/collector.toml.example");
        let c: FileConfig = toml::from_str(example).unwrap();
        assert!(c.notify.ntfy.is_none());
        let all: String = example
            .lines()
            .map(|l| match l.strip_prefix("# ") {
                Some(rest)
                    if rest.starts_with('[')
                        || rest.split_once(" = ").is_some_and(|(k, _)| {
                            !k.contains(' ')
                                && k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                        }) =>
                {
                    rest
                }
                _ => l,
            })
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        let c: FileConfig = toml::from_str(&all).unwrap_or_else(|e| {
            panic!(
                "{e}
{all}"
            )
        });
        let n = &c.notify;
        assert!(n.ntfy.is_some() && n.telegram.is_some() && n.email.is_some());
        assert_eq!(n.event_kinds().unwrap().map(|k| k.len()), Some(3));
        assert!(crate::notify::Notifier::from_config(n).is_ok());
    }

    #[test]
    fn rejects_mistakes() {
        assert!(toml::from_str::<FileConfig>("[notify.ntfy]\nurl = \"x\"\ntopic = \"y\"").is_err());
        let c: FileConfig = toml::from_str("[notify]\nevents = [\"power_out\"]").unwrap();
        assert!(c.notify.event_kinds().is_err());
        assert!(
            toml::from_str::<FileConfig>("")
                .unwrap()
                .notify
                .ntfy
                .is_none()
        );
    }

    #[test]
    fn generates_distinct_hex_tokens() {
        let a = generate_token().unwrap();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_ne!(a, generate_token().unwrap());
    }

    #[test]
    fn checks_token_files() {
        let dir = std::env::temp_dir().join(format!("ups-token-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("token");
        std::fs::write(&file, "short\n").unwrap();
        assert!(load_token(Some(&file)).is_err());
        std::fs::write(&file, "0123456789abcdef0123\n").unwrap();
        assert_eq!(
            load_token(Some(&file)).unwrap().unwrap().expose(),
            "0123456789abcdef0123"
        );
        std::fs::write(&file, "  \n").unwrap();
        assert!(load_token(Some(&file)).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
