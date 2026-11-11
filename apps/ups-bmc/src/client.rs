//! Background thread that talks to the collector: initial HTTP fetch, then
//! the live WebSocket. Results are handed to the UI thread as plain data.

use std::net::TcpStream;
use std::time::{Duration, Instant};

use chrono::Utc;
use tungstenite::stream::MaybeTlsStream;
use ups_api::{
    Event, EventsResponse, HistoryPoint, HistoryResponse, LiveMessage, Outage, OutagesResponse,
    StatsResponse, StatusResponse,
};

use crate::view::{self, CHART_WINDOW, EventLine, OutagesView, View};

/// What the client thread reports to the UI.
#[derive(Debug)]
pub enum Update {
    View(Box<View>),
    Events(Vec<EventLine>),
    /// A live event worth a desktop notification.
    Notify(Event),
    Outages(Box<OutagesView>),
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
/// Outage history shown in the UI.
const OUTAGE_DAYS: u32 = 90;
/// Statistics are refreshed at least this often (and after each outage change).
const STATS_EVERY: Duration = Duration::from_secs(60);

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
        let mut outages = Outages::fetch(config);
        outages.send(send);

        // 3. Live updates until the connection drops.
        if let Err(e) = live(config, send, &mut history, &mut events, &mut outages) {
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
    outages: &mut Outages,
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
                // Ongoing outages tick; stats refresh periodically.
                if outages.stats_due() {
                    outages.refresh_stats(config);
                    outages.send(send);
                } else if outages.any_ongoing() {
                    outages.send(send);
                }
            }
            Ok(LiveMessage::Outage(outage)) => {
                let new_or_ended = outages.upsert(outage);
                if new_or_ended {
                    outages.refresh_stats(config);
                }
                outages.send(send);
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

/// Asks the collector to switch the UPS beeper (ADR-0003).
pub fn set_beeper(address: &str, enabled: bool) -> Result<String, String> {
    post_setting(address, "beeper", &ups_api::BeeperRequest { enabled })
}

/// Sets or clears the load estimate used when the UPS reads 0 %.
pub fn set_assumed_load(address: &str, watts: Option<f32>) -> Result<String, String> {
    post_setting(
        address,
        "assumed-load",
        &ups_api::AssumedLoadRequest { watts },
    )
}

/// POSTs a setting; returns the collector's message or its error text.
fn post_setting(address: &str, name: &str, body: &impl serde::Serialize) -> Result<String, String> {
    let mut response = ureq::post(format!("http://{address}/api/v1/settings/{name}"))
        .config()
        .timeout_global(Some(Duration::from_secs(15)))
        .http_status_as_error(false)
        .build()
        .send_json(body)
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

/// Outage list and statistics as last fetched from the collector.
#[derive(Debug, Default)]
struct Outages {
    list: Vec<Outage>,
    stats: Option<StatsResponse>,
    stats_at: Option<Instant>,
}

impl Outages {
    fn fetch(config: &ClientConfig) -> Self {
        let list = http::<OutagesResponse>(config, &format!("/api/v1/outages?days={OUTAGE_DAYS}"))
            .map(|r| r.outages)
            .unwrap_or_default();
        let mut me = Self {
            list,
            ..Self::default()
        };
        me.refresh_stats(config);
        me
    }

    fn refresh_stats(&mut self, config: &ClientConfig) {
        self.stats_at = Some(Instant::now());
        if let Ok(s) = http::<StatsResponse>(config, "/api/v1/stats") {
            self.stats = Some(s);
        }
    }

    fn stats_due(&self) -> bool {
        self.stats_at.is_none_or(|t| t.elapsed() >= STATS_EVERY)
    }

    fn any_ongoing(&self) -> bool {
        self.list.iter().any(Outage::is_ongoing)
    }

    /// Inserts or replaces an outage. Returns true if it is new or has just ended.
    fn upsert(&mut self, outage: Outage) -> bool {
        if let Some(existing) = self.list.iter_mut().find(|o| o.id == outage.id) {
            let ended = existing.end.is_none() && outage.end.is_some();
            *existing = outage;
            ended
        } else {
            self.list.insert(0, outage);
            true
        }
    }

    fn send(&self, send: &dyn Fn(Update)) {
        send(Update::Outages(Box::new(view::outages_view(
            &self.list,
            self.stats.as_ref(),
            Utc::now(),
        ))));
    }
}

/// Downloads a monthly report (`month` = `YYYY-MM`) as text.
pub fn fetch_report(address: &str, month: &str, csv: bool) -> Result<String, String> {
    let format = if csv { "csv" } else { "html" };
    let mut response = ureq::get(format!(
        "http://{address}/api/v1/report?month={month}&format={format}"
    ))
    .config()
    .timeout_global(Some(Duration::from_secs(30)))
    .http_status_as_error(false)
    .build()
    .call()
    .map_err(|e| e.to_string())?;
    let ok = response.status().is_success();
    let body = response
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    if ok { Ok(body) } else { Err(body) }
}
