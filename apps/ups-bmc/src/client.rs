//! Background thread that talks to the collector: initial HTTP fetch, then
//! the live WebSocket. Results are handed to the UI thread as plain data.

use std::net::TcpStream;
use std::time::Duration;

use chrono::Utc;
use tungstenite::stream::MaybeTlsStream;
use ups_api::{Event, EventsResponse, HistoryPoint, HistoryResponse, LiveMessage, StatusResponse};

use crate::view::{self, CHART_WINDOW, EventLine, View};

/// What the client thread reports to the UI.
#[derive(Debug)]
pub enum Update {
    View(Box<View>),
    Events(Vec<EventLine>),
    /// A live event worth a desktop notification.
    Notify(Event),
}

/// Configuration of the client thread.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// `127.0.0.1:47800`
    pub address: String,
    /// Start the collector once if it isn't running.
    pub auto_start: bool,
    /// Arguments for an auto-started collector.
    pub collector_args: Vec<String>,
}

const RETRY: Duration = Duration::from_secs(2);
const HISTORY_POINTS: usize = 900; // one point per second over 15 minutes
const MAX_EVENTS: usize = 50;

pub fn spawn(
    config: ClientConfig,
    send: impl Fn(Update) + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("collector-client".into())
        .spawn(move || run(&config, &send))
}

fn http<T: serde::de::DeserializeOwned>(config: &ClientConfig, path: &str) -> Result<T, String> {
    ureq::get(format!("http://{}{path}", config.address))
        .config()
        .timeout_global(Some(Duration::from_secs(5)))
        .build()
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_json()
        .map_err(|e| e.to_string())
}

fn run(config: &ClientConfig, send: &dyn Fn(Update)) {
    let mut tried_start = false;
    loop {
        // 1. Is the collector there?
        let Ok(status) = http::<StatusResponse>(config, "/api/v1/status") else {
            {
                if config.auto_start && !tried_start {
                    tried_start = true;
                    let detail = match crate::launch::start_collector(&config.collector_args) {
                        Ok(()) => "Starting the collector…".to_owned(),
                        Err(e) => format!("Could not start the collector: {e}"),
                    };
                    send(Update::View(Box::new(view::collector_offline(
                        &detail, false,
                    ))));
                } else {
                    send(Update::View(Box::new(view::collector_offline(
                        "The background collector records UPS data. Start it to begin monitoring.",
                        true,
                    ))));
                }
                std::thread::sleep(RETRY);
                continue;
            }
        };
        tried_start = true;

        // 2. Initial history and events.
        let minutes = CHART_WINDOW.num_minutes();
        let mut history: Vec<HistoryPoint> = http::<HistoryResponse>(
            config,
            &format!("/api/v1/history?minutes={minutes}&points={HISTORY_POINTS}"),
        )
        .map(|h| h.points)
        .unwrap_or_default();
        let mut events: Vec<Event> =
            http::<EventsResponse>(config, &format!("/api/v1/events?limit={MAX_EVENTS}"))
                .map(|e| e.events)
                .unwrap_or_default();
        send(Update::View(Box::new(view::build(
            &status,
            &history,
            Utc::now(),
        ))));
        send(Update::Events(view::event_lines(&events, Utc::now())));

        // 3. Live updates until the connection drops.
        if let Err(e) = live(config, send, &mut history, &mut events) {
            send(Update::View(Box::new(view::collector_offline(
                &format!("Lost the connection to the collector ({e}). Reconnecting…"),
                false,
            ))));
        }
        std::thread::sleep(RETRY);
    }
}

fn live(
    config: &ClientConfig,
    send: &dyn Fn(Update),
    history: &mut Vec<HistoryPoint>,
    events: &mut Vec<Event>,
) -> Result<(), String> {
    let (mut ws, _) = tungstenite::connect(format!("ws://{}/api/v1/live", config.address))
        .map_err(|e| e.to_string())?;
    // The collector sends a status every poll; silence means it is gone.
    if let MaybeTlsStream::Plain(stream) = ws.get_mut() {
        let _ = TcpStream::set_read_timeout(stream, Some(Duration::from_secs(10)));
    }
    loop {
        let msg = ws.read().map_err(|e| e.to_string())?;
        let tungstenite::Message::Text(text) = msg else {
            continue;
        };
        match serde_json::from_str::<LiveMessage>(text.as_str()) {
            Ok(LiveMessage::Status(status)) => {
                if let Some(s) = &status.snapshot
                    && history.last().is_none_or(|p| p.ts < s.ts)
                {
                    history.push(view::history_point(s));
                }
                let now = Utc::now();
                history.retain(|p| now - p.ts <= CHART_WINDOW);
                send(Update::View(Box::new(view::build(&status, history, now))));
            }
            Ok(LiveMessage::Event(event)) => {
                events.insert(0, event.clone());
                events.truncate(MAX_EVENTS);
                send(Update::Events(view::event_lines(events, Utc::now())));
                send(Update::Notify(event));
            }
            Err(_) => {} // newer collector with unknown message types
        }
    }
}

/// Asks the collector to switch the UPS beeper (ADR-0003). Returns the
/// collector's message, or the error text it sent back.
pub fn set_beeper(address: &str, enabled: bool) -> Result<String, String> {
    let mut response = ureq::post(format!("http://{address}/api/v1/settings/beeper"))
        .config()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .send_json(ups_api::BeeperRequest { enabled })
        .map_err(|e| e.to_string())?;
    let ok = response.status().is_success();
    let body = response.body_mut().read_to_string().unwrap_or_default();
    if ok {
        Ok(serde_json::from_str::<ups_api::SettingResponse>(&body)
            .map(|r| r.message)
            .unwrap_or(body))
    } else {
        Err(if body.is_empty() {
            format!("HTTP {}", response.status())
        } else {
            body
        })
    }
}
