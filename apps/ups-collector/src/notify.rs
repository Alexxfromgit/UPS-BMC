//! Remote notification channels: ntfy, Telegram and email (ADR-0005).
//!
//! The collector sends them itself, so they work without a desktop UI running,
//! for example from a Raspberry Pi. Events come from the live broadcast; each
//! message is sent on the blocking pool and retried with back-off, because
//! the network may be down during the very outage being reported.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Local;
use lettre::message::{Mailbox, header::ContentType};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use tokio::sync::broadcast;
use tracing::{info, warn};
use ups_api::{Event, EventKind, LiveMessage, Severity, StatusResponse};

use crate::config::{EmailConfig, NotifyConfig, NtfyConfig, Secret, SmtpSecurity, TelegramConfig};

/// Waits before the retries of a failed send.
const RETRIES: [Duration; 3] = [
    Duration::from_secs(10),
    Duration::from_secs(60),
    Duration::from_secs(300),
];
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// One message, as every channel sends it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub title: String,
    pub body: String,
    pub severity: Severity,
    pub kind: Option<EventKind>,
}

/// A configured channel.
pub enum Channel {
    Ntfy(NtfyConfig),
    Telegram(TelegramConfig),
    Email(Box<Email>),
}

impl std::fmt::Debug for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A validated email channel.
pub struct Email {
    transport: SmtpTransport,
    from: Mailbox,
    to: Vec<Mailbox>,
}

impl Email {
    fn new(c: &EmailConfig) -> Result<Self> {
        let host = c.smtp_host.as_str();
        let mut builder = match c.security {
            SmtpSecurity::Starttls => SmtpTransport::starttls_relay(host)?,
            SmtpSecurity::Tls => SmtpTransport::relay(host)?,
            SmtpSecurity::None => SmtpTransport::builder_dangerous(host),
        };
        if let Some(port) = c.smtp_port {
            builder = builder.port(port);
        }
        match (&c.username, &c.password) {
            (Some(u), Some(p)) => {
                builder = builder.credentials(Credentials::new(u.clone(), p.expose().to_owned()));
            }
            (None, None) => {}
            _ => bail!("notify.email needs both username and password, or neither"),
        }
        let from = c
            .from
            .parse()
            .with_context(|| format!("notify.email.from `{}`", c.from))?;
        if c.to.is_empty() {
            bail!("notify.email.to needs at least one recipient");
        }
        let to =
            c.to.iter()
                .map(|t| t.parse().with_context(|| format!("notify.email.to `{t}`")))
                .collect::<Result<_>>()?;
        Ok(Self {
            transport: builder.timeout(Some(HTTP_TIMEOUT)).build(),
            from,
            to,
        })
    }
}

impl Channel {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Ntfy(_) => "ntfy",
            Self::Telegram(_) => "telegram",
            Self::Email(_) => "email",
        }
    }

    /// Sends one message (blocking).
    pub fn send(&self, note: &Note) -> Result<(), String> {
        match self {
            Self::Ntfy(c) => send_ntfy(c, note),
            Self::Telegram(c) => send_telegram(c, note),
            Self::Email(e) => send_email(e, note),
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(HTTP_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .into()
}

fn check(mut response: ureq::http::Response<ureq::Body>) -> Result<(), String> {
    if response.status().is_success() {
        return Ok(());
    }
    let body = response.body_mut().read_to_string().unwrap_or_default();
    Err(format!("HTTP {}: {body:.200}", response.status()))
}

fn send_ntfy(c: &NtfyConfig, note: &Note) -> Result<(), String> {
    let (priority, tags) = match note.severity {
        Severity::Critical => ("urgent", "rotating_light"),
        Severity::Warning => ("high", "warning"),
        Severity::Info => ("default", "electric_plug"),
    };
    let mut request = agent()
        .post(&c.url)
        .header("Title", ascii(&note.title))
        .header("Priority", priority)
        .header("Tags", tags);
    if let Some(token) = &c.token {
        request = request.header("Authorization", format!("Bearer {}", token.expose()));
    }
    check(
        request
            .send(note.body.as_str())
            .map_err(|e| e.to_string())?,
    )
}

/// HTTP header values must be ASCII; titles are, but be safe.
fn ascii(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c
            } else {
                '?'
            }
        })
        .collect()
}

fn send_telegram(c: &TelegramConfig, note: &Note) -> Result<(), String> {
    let url = format!(
        "{}/bot{}/sendMessage",
        c.api_url.trim_end_matches('/'),
        c.bot_token.expose()
    );
    let body = serde_json::json!({
        "chat_id": c.chat_id.to_string(),
        "text": format!("{}\n{}", note.title, note.body),
        "disable_web_page_preview": true,
    });
    // ureq's error text contains the URL, and so the bot token: keep it out.
    let response = agent()
        .post(&url)
        .send_json(&body)
        .map_err(|e| redact(&e.to_string(), &c.bot_token))?;
    check(response).map_err(|e| redact(&e, &c.bot_token))
}

fn redact(text: &str, secret: &Secret) -> String {
    text.replace(secret.expose(), "***")
}

fn send_email(e: &Email, note: &Note) -> Result<(), String> {
    let mut builder = Message::builder()
        .from(e.from.clone())
        .subject(format!("UPS-BMC: {}", note.title))
        .header(ContentType::TEXT_PLAIN);
    for to in &e.to {
        builder = builder.to(to.clone());
    }
    let message = builder
        .body(note.body.clone())
        .map_err(|err| err.to_string())?;
    e.transport
        .send(&message)
        .map(|_| ())
        .map_err(|err| err.to_string())
}

/// The configured channels and which events they get.
#[derive(Debug)]
pub struct Notifier {
    pub channels: Vec<Channel>,
    events: Option<Vec<EventKind>>,
}

impl Notifier {
    pub fn from_config(c: &NotifyConfig) -> Result<Self> {
        let mut channels = Vec::new();
        if let Some(n) = &c.ntfy {
            if !(n.url.starts_with("https://") || n.url.starts_with("http://")) {
                bail!("notify.ntfy.url must start with https:// (or http://)");
            }
            channels.push(Channel::Ntfy(n.clone()));
        }
        if let Some(t) = &c.telegram {
            channels.push(Channel::Telegram(t.clone()));
        }
        if let Some(e) = &c.email {
            channels.push(Channel::Email(Box::new(Email::new(e)?)));
        }
        Ok(Self {
            channels,
            events: c.event_kinds()?,
        })
    }

    /// The message for `event`, if it is one to send.
    pub fn note_for(&self, event: &Event, device: Option<&str>) -> Option<Note> {
        let title = match &self.events {
            Some(kinds) if kinds.contains(&event.kind) => {
                ups_api::notification_title(event).unwrap_or(event.kind.as_str())
            }
            Some(_) => return None,
            None => ups_api::notification_title(event)?,
        };
        // Retries can deliver messages late or out of order: say when it happened.
        let when = event.ts.with_timezone(&Local).format("%Y-%m-%d %H:%M:%S");
        let body = match device {
            Some(d) => format!("{}\n{d}, {when}", event.message),
            None => format!("{}\n{when}", event.message),
        };
        Some(Note {
            title: title.to_owned(),
            body,
            severity: event.severity,
            kind: Some(event.kind),
        })
    }

    /// Sends live events until the broadcast closes.
    pub fn spawn(
        self,
        runtime: &tokio::runtime::Runtime,
        live: &broadcast::Sender<LiveMessage>,
        status: Arc<RwLock<StatusResponse>>,
    ) {
        if self.channels.is_empty() {
            return;
        }
        info!(
            channels = ?self.channels.iter().map(Channel::name).collect::<Vec<_>>(),
            "remote notifications enabled"
        );
        let mut rx = live.subscribe();
        let channels: Vec<Arc<Channel>> = self.channels.into_iter().map(Arc::new).collect();
        let me = Self {
            channels: Vec::new(),
            events: self.events,
        };
        runtime.spawn(async move {
            loop {
                let event = match rx.recv().await {
                    Ok(LiveMessage::Event(e)) => e,
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            skipped = n,
                            "notifier fell behind; some messages were skipped"
                        );
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                let device = status
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .device
                    .as_ref()
                    .map(ups_api::DeviceInfo::display_name);
                let Some(note) = me.note_for(&event, device.as_deref()) else {
                    continue;
                };
                for channel in &channels {
                    let (channel, note) = (channel.clone(), note.clone());
                    tokio::task::spawn_blocking(move || deliver(&channel, &note));
                }
            }
        });
    }
}

/// Sends with retries; logs the outcome.
fn deliver(channel: &Channel, note: &Note) {
    let mut waits = RETRIES.iter();
    loop {
        match channel.send(note) {
            Ok(()) => {
                info!(channel = channel.name(), title = %note.title, "notification sent");
                return;
            }
            Err(e) => {
                let Some(wait) = waits.next() else {
                    warn!(channel = channel.name(), error = %e, title = %note.title, "notification dropped");
                    return;
                };
                warn!(channel = channel.name(), error = %e, retry_in_s = wait.as_secs(), "notification failed");
                std::thread::sleep(*wait);
            }
        }
    }
}

/// `--test-notify`: sends a test message to every channel, once, and reports
/// each result.
pub fn send_test(notifier: &Notifier) -> Result<()> {
    if notifier.channels.is_empty() {
        bail!("no notification channels are configured (see [notify] in the config file)");
    }
    let note = Note {
        title: "UPS-BMC test notification".into(),
        body: format!(
            "Notifications from the UPS-BMC collector work.\n{}",
            Local::now().format("%Y-%m-%d %H:%M:%S")
        ),
        severity: Severity::Info,
        kind: None,
    };
    let mut failed = 0;
    for channel in &notifier.channels {
        match channel.send(&note) {
            Ok(()) => println!("{}: sent", channel.name()),
            Err(e) => {
                failed += 1;
                println!("{}: FAILED: {e}", channel.name());
            }
        }
    }
    if failed > 0 {
        bail!("{failed} channel(s) failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    use chrono::Utc;

    use super::*;
    use crate::config::ChatId;

    /// Accepts one HTTP request, answers `status`, and returns the request
    /// head and body.
    fn one_request(status: u16) -> (String, std::thread::JoinHandle<(String, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut head = String::new();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = v.trim().parse().unwrap();
                }
                if line == "\r\n" {
                    break;
                }
                head += &line;
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let mut out = stream;
            write!(
                out,
                "HTTP/1.1 {status} X\r\ncontent-length: 2\r\nconnection: close\r\n\r\nok"
            )
            .unwrap();
            (head, String::from_utf8(body).unwrap())
        });
        (base, handle)
    }

    fn note() -> Note {
        Note {
            title: "Power outage".into(),
            body: "The UPS is on battery".into(),
            severity: Severity::Warning,
            kind: Some(EventKind::PowerLost),
        }
    }

    #[test]
    fn posts_to_ntfy() {
        let (base, server) = one_request(200);
        let channel = Channel::Ntfy(NtfyConfig {
            url: format!("{base}/ups"),
            token: Some(Secret::new("tk_123")),
        });
        channel.send(&note()).unwrap();
        let (head, body) = server.join().unwrap();
        let head = head.to_ascii_lowercase();
        assert!(head.starts_with("post /ups "), "{head}");
        assert!(head.contains("title: power outage"));
        assert!(head.contains("priority: high"));
        assert!(head.contains("authorization: bearer tk_123"));
        assert_eq!(body, "The UPS is on battery");
    }

    #[test]
    fn posts_to_telegram_without_leaking_the_token() {
        let (base, server) = one_request(401);
        let channel = Channel::Telegram(TelegramConfig {
            bot_token: Secret::new("123:SECRET"),
            chat_id: ChatId::Number(42),
            api_url: base,
        });
        let err = channel.send(&note()).unwrap_err();
        assert!(err.contains("401") && !err.contains("SECRET"), "{err}");
        let (head, body) = server.join().unwrap();
        assert!(
            head.starts_with("POST /bot123:SECRET/sendMessage "),
            "{head}"
        );
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["chat_id"], "42");
        assert_eq!(json["text"], "Power outage\nThe UPS is on battery");
    }

    #[test]
    fn chooses_events() {
        let lost = Event::new(Utc::now(), EventKind::PowerLost, "On battery");
        let started = Event::new(Utc::now(), EventKind::CollectorStarted, "Started");
        let default = Notifier::from_config(&NotifyConfig::default()).unwrap();
        let n = default.note_for(&lost, Some("Eaton 5E")).unwrap();
        assert_eq!(n.title, "Power outage");
        assert!(n.body.starts_with("On battery\nEaton 5E, "), "{}", n.body);
        assert!(default.note_for(&started, None).is_none());

        let custom = Notifier::from_config(&NotifyConfig {
            events: Some(vec!["collector_started".into()]),
            ..NotifyConfig::default()
        })
        .unwrap();
        assert!(custom.note_for(&lost, None).is_none());
        assert_eq!(
            custom.note_for(&started, None).unwrap().title,
            "collector_started"
        );
    }

    #[test]
    fn validates_email_settings() {
        let mut c = EmailConfig {
            smtp_host: "smtp.example.com".into(),
            smtp_port: None,
            security: SmtpSecurity::Starttls,
            username: Some("me".into()),
            password: Some(Secret::new("pw")),
            from: "UPS-BMC <ups@example.com>".into(),
            to: vec!["me@example.com".into()],
        };
        assert!(Email::new(&c).is_ok());
        c.to = vec!["not an address".into()];
        assert!(Email::new(&c).is_err());
        c.to = vec![];
        assert!(Email::new(&c).is_err());
        c.to = vec!["me@example.com".into()];
        c.password = None;
        assert!(Email::new(&c).is_err());
    }
}
