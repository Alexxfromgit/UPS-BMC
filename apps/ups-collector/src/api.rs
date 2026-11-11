//! The local HTTP + WebSocket API (see `ups-api` for the wire types).

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Local, Utc};
use serde::Deserialize;
use tokio::sync::broadcast;
use tracing::debug;
use ups_api::{
    AssumedLoadRequest, BatteryRequest, BeeperRequest, EventsResponse, HealthReport,
    HistoryResponse, LiveMessage, OutagesResponse, RuntimeTestRequest, SettingResponse,
    StatsResponse, StatusResponse, UpsStatus,
};
use ups_source::Setting;

use crate::poller::Control;
use ups_store::Store;

/// Error responses: status code plus a plain-text message.
type ApiError = (StatusCode, String);

#[derive(Clone)]
pub struct AppState {
    pub status: Arc<RwLock<StatusResponse>>,
    pub live: broadcast::Sender<LiveMessage>,
    pub db_path: PathBuf,
    /// Requests to the poller thread (setting changes).
    pub control: std::sync::mpsc::Sender<Control>,
    /// Requests to the ingest thread (load estimate).
    pub ingest: std::sync::mpsc::Sender<crate::poller::PollerMsg>,
    /// The port the API listens on (for the `Host` check).
    pub port: u16,
    /// Asks `main` to shut the collector down gracefully.
    pub shutdown: tokio::sync::mpsc::Sender<()>,
}

/// How often the live WebSocket repeats the status even when nothing changes
/// (e.g. while the UPS is unplugged), so clients can tell a quiet collector
/// from a dead one (review M1-1).
const HEARTBEAT: std::time::Duration = std::time::Duration::from_secs(3);

pub fn router(state: AppState) -> Router {
    let guard = axum::middleware::from_fn_with_state(state.clone(), local_clients_only);
    Router::new()
        .route(
            "/",
            get(|| async { "UPS-BMC collector. API: /api/v1/status" }),
        )
        .route("/api/v1/status", get(status))
        .route("/api/v1/history", get(history))
        .route("/api/v1/events", get(events))
        .route("/api/v1/live", get(live))
        .route("/api/v1/settings/beeper", post(set_beeper))
        .route("/api/v1/settings/assumed-load", post(set_assumed_load))
        .route("/api/v1/outages", get(outages))
        .route("/api/v1/stats", get(stats))
        .route("/api/v1/report", get(report))
        .route("/api/v1/health", get(health))
        .route("/api/v1/settings/battery", post(set_battery))
        .route("/api/v1/battery/test", post(runtime_test))
        .route("/api/v1/shutdown", post(shutdown))
        .layer(guard)
        .with_state(state)
}

/// Every request must come from a local native client (review M1-2):
/// * the `Host` must be a loopback name with our port. A web page using DNS
///   rebinding reaches 127.0.0.1 under its own host name and is refused, so
///   it can't read the status, history or reports;
/// * no `Origin` header (browsers send it on cross-site requests; see
///   [`reject_browsers`]).
async fn local_clients_only(
    State(state): State<AppState>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<Response, ApiError> {
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let allowed = ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .any(|name| host == format!("{name}:{}", state.port));
    if !allowed {
        return Err((StatusCode::FORBIDDEN, "unexpected Host header".into()));
    }
    reject_browsers(request.headers())?;
    Ok(next.run(request).await)
}

/// `POST /api/v1/shutdown` with `{}`: stops the collector gracefully (used by
/// the installer before an upgrade; review M1-5).
async fn shutdown(
    State(state): State<AppState>,
    Json(_): Json<serde_json::Value>,
) -> Json<SettingResponse> {
    let _ = state.shutdown.try_send(());
    Json(SettingResponse {
        ok: true,
        message: "Collector stopping".into(),
    })
}

fn current(state: &AppState) -> StatusResponse {
    state
        .status
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

async fn status(State(state): State<AppState>) -> Json<StatusResponse> {
    Json(current(&state))
}

/// Runs a read-only query on a fresh read connection (SQLite WAL allows
/// readers alongside the ingest writer).
async fn query<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(&Store) -> ups_store::Result<T> + Send + 'static,
) -> Result<T, ApiError> {
    let path = state.db_path.clone();
    tokio::task::spawn_blocking(move || Store::open_read_only(&path).and_then(|s| f(&s)))
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

#[derive(Debug, Deserialize)]
struct HistoryParams {
    /// Window length in minutes (default 15, max 30 days).
    minutes: Option<u32>,
    /// Maximum number of points (default 300, max 5000).
    points: Option<usize>,
}

async fn history(
    State(state): State<AppState>,
    Query(p): Query<HistoryParams>,
) -> Result<Json<HistoryResponse>, ApiError> {
    let minutes = p.minutes.unwrap_or(15).clamp(1, 30 * 24 * 60);
    let points = p.points.unwrap_or(300).clamp(1, 5000);
    let to = Utc::now();
    let from = to - chrono::Duration::minutes(i64::from(minutes));
    let points = query(&state, move |s| s.history(from, to, points)).await?;
    Ok(Json(HistoryResponse { from, to, points }))
}

#[derive(Debug, Deserialize)]
struct EventsParams {
    /// Maximum number of events (default 50, max 1000).
    limit: Option<usize>,
}

async fn events(
    State(state): State<AppState>,
    Query(p): Query<EventsParams>,
) -> Result<Json<EventsResponse>, ApiError> {
    let limit = p.limit.unwrap_or(50).clamp(1, 1000);
    let events = query(&state, move |s| s.recent_events(limit)).await?;
    Ok(Json(EventsResponse { events }))
}

async fn live(State(state): State<AppState>, ws: WebSocketUpgrade) -> Result<Response, ApiError> {
    Ok(ws.on_upgrade(move |socket| live_session(socket, state)))
}

async fn send(socket: &mut WebSocket, msg: &LiveMessage) -> bool {
    match serde_json::to_string(msg) {
        Ok(json) => socket.send(Message::Text(json.into())).await.is_ok(),
        Err(_) => true,
    }
}

async fn live_session(mut socket: WebSocket, state: AppState) {
    // Subscribe before sending the first status so nothing is missed.
    let mut rx = state.live.subscribe();
    if !send(&mut socket, &LiveMessage::Status(Box::new(current(&state)))).await {
        return;
    }
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if !send(&mut socket, &LiveMessage::Status(Box::new(current(&state)))).await { break }
            }
            msg = rx.recv() => match msg {
                Ok(msg) => if !send(&mut socket, &msg).await { break },
                // Slow client: skip ahead and resend the full status.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if !send(&mut socket, &LiveMessage::Status(Box::new(current(&state)))).await { break }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => {} // ignore client messages (pings are answered by axum)
            },
        }
    }
    debug!("live client disconnected");
}

/// The API is for local native clients (the UPS-BMC UI). Browsers attach an
/// `Origin` header to cross-site requests, so refusing any request that has
/// one stops a web page from changing UPS settings (CSRF) or reading the
/// live stream through `127.0.0.1`.
fn reject_browsers(headers: &HeaderMap) -> Result<(), ApiError> {
    if headers.contains_key(header::ORIGIN) {
        return Err((
            StatusCode::FORBIDDEN,
            "requests from web pages are not allowed".into(),
        ));
    }
    Ok(())
}

/// `POST /api/v1/settings/beeper` with `{"enabled": true|false}` (a JSON body
/// is required). The change is applied by the poller thread, verified by
/// reading it back, and recorded as an event.
async fn set_beeper(
    State(state): State<AppState>,
    Json(req): Json<BeeperRequest>,
) -> Result<Json<SettingResponse>, ApiError> {
    let unavailable = |e: &str| (StatusCode::SERVICE_UNAVAILABLE, e.to_owned());
    let (reply, rx) = tokio::sync::oneshot::channel();
    state
        .control
        .send(Control::Apply {
            setting: Setting::Beeper {
                enabled: req.enabled,
            },
            reply,
        })
        .map_err(|_| unavailable("the collector is shutting down"))?;
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), rx)
        .await
        .map_err(|_| unavailable("the UPS did not respond in time"))?
        .map_err(|_| unavailable("the collector is shutting down"))?;
    match result {
        Ok(()) => Ok(Json(SettingResponse {
            ok: true,
            message: if req.enabled {
                "Beeper enabled".into()
            } else {
                "Beeper disabled".into()
            },
        })),
        Err(e) => Err((StatusCode::CONFLICT, e)),
    }
}

#[derive(Debug, Deserialize)]
struct OutagesParams {
    /// How far back, in days (default 90, max 3650).
    days: Option<u32>,
    /// Maximum number of outages (default 500, max 5000).
    limit: Option<usize>,
}

async fn outages(
    State(state): State<AppState>,
    Query(p): Query<OutagesParams>,
) -> Result<Json<OutagesResponse>, ApiError> {
    let days = p.days.unwrap_or(90).clamp(1, 3650);
    let limit = p.limit.unwrap_or(500).clamp(1, 5000);
    let from = Utc::now() - chrono::Duration::days(i64::from(days));
    let outages = query(&state, move |s| s.outages_since(from, limit)).await?;
    Ok(Json(OutagesResponse { outages }))
}

async fn stats(State(state): State<AppState>) -> Result<Json<StatsResponse>, ApiError> {
    let now = Utc::now();
    let response = query(&state, move |s| {
        // Enough history for "last 30 days" and "this month".
        let outages = s.outages_since(now - chrono::Duration::days(62), 100_000)?;
        let daily = s.all_daily()?;
        Ok(StatsResponse {
            periods: ups_analysis::stats::standard_periods(&outages, &daily, &Local, now),
            months: ups_analysis::stats::months_with_data(&daily),
        })
    })
    .await?;
    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
struct ReportParams {
    /// `YYYY-MM`
    month: String,
    /// `html` (default) or `csv`
    format: Option<String>,
}

async fn report(
    State(state): State<AppState>,
    Query(p): Query<ReportParams>,
) -> Result<Response, ApiError> {
    use ups_analysis::{report, stats};

    let bad = |m: &str| (StatusCode::BAD_REQUEST, m.to_owned());
    let (year, month) =
        stats::parse_month(&p.month).ok_or_else(|| bad("month must look like 2026-11"))?;
    let csv = match p.format.as_deref().unwrap_or("html") {
        "html" => false,
        "csv" => true,
        _ => return Err(bad("format must be html or csv")),
    };
    let (first, _) = stats::month_range(year, month).ok_or_else(|| bad("invalid month"))?;
    let now = Utc::now();
    // Start a day early: the month starts at local midnight, not UTC midnight.
    let from = first
        .pred_opt()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map_or(now, |d| d.and_utc());
    let body = query(&state, move |s| {
        let outages = s.outages_since(from, 100_000)?;
        Ok(if csv {
            report::monthly_csv(year, month, &outages, &Local, now)
        } else {
            let daily = s.all_daily()?;
            let device = s.device()?;
            let generator = format!("UPS-BMC {}", env!("CARGO_PKG_VERSION"));
            report::monthly_html(
                year,
                month,
                &outages,
                &daily,
                device.as_ref(),
                &Local,
                now,
                &generator,
            )
        })
    })
    .await?
    .ok_or_else(|| bad("invalid month"))?;

    let (mime, ext) = if csv {
        ("text/csv; charset=utf-8", "csv")
    } else {
        ("text/html; charset=utf-8", "html")
    };
    let disposition = format!(
        "inline; filename=\"{}\"",
        report::file_name(year, month, ext)
    );
    Ok((
        [
            (header::CONTENT_TYPE, mime.to_owned()),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response())
}

/// `POST /api/v1/settings/assumed-load` with `{"watts": 20}` or `{"watts": null}`.
async fn set_assumed_load(
    State(state): State<AppState>,
    Json(req): Json<AssumedLoadRequest>,
) -> Result<Json<SettingResponse>, ApiError> {
    if let Some(w) = req.watts
        && !(1.0..=2000.0).contains(&w)
    {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "watts must be between 1 and 2000".into(),
        ));
    }
    ask_ingest(&state, |reply| crate::poller::PollerMsg::SetAssumedLoad {
        watts: req.watts,
        reply,
    })
    .await?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(SettingResponse {
        ok: true,
        message: match req.watts {
            Some(w) => format!("Load estimate set to {w:.0} W"),
            None => "Load estimate cleared".into(),
        },
    }))
}

/// `GET /api/v1/health`: battery health from the outages since the battery
/// was installed (or all outages).
async fn health(State(state): State<AppState>) -> Result<Json<HealthReport>, ApiError> {
    let now = Utc::now();
    // Only a current reading counts: a stale one would keep (or miss) the
    // "replace battery" verdict while the UPS is disconnected (review M3-9).
    let st = current(&state);
    let replace_flag = st.connection == ups_api::Connection::Connected
        && st
            .snapshot
            .and_then(|s| s.status)
            .is_some_and(|f| f.contains(UpsStatus::RB));
    let report = query(&state, move |s| {
        let installed = s.battery_installed()?;
        // The exact replacement time if known, else local midnight of the
        // date (review M3-7/M3-8).
        let since = s.battery_installed_at()?.or_else(|| {
            installed
                .and_then(|d| d.and_hms_opt(0, 0, 0))
                .and_then(|m| m.and_local_timezone(Local).earliest())
                .map(|m| m.with_timezone(&Utc))
        });
        let from = since.unwrap_or(chrono::DateTime::<Utc>::UNIX_EPOCH);
        let outages = s.outages_since(from, 100_000)?;
        Ok(ups_analysis::health::assess(
            &outages,
            ups_analysis::health::Battery { installed, since },
            replace_flag,
            now,
            &ups_analysis::health::HealthConfig::default(),
        ))
    })
    .await?;
    Ok(Json(report))
}

/// Sends a request to the ingest thread and waits for its reply.
async fn ask_ingest<T>(
    state: &AppState,
    msg: impl FnOnce(tokio::sync::oneshot::Sender<Result<T, String>>) -> crate::poller::PollerMsg,
) -> Result<Result<T, String>, ApiError> {
    let unavailable = || {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "the collector is shutting down".to_owned(),
        )
    };
    let (reply, rx) = tokio::sync::oneshot::channel();
    state.ingest.send(msg(reply)).map_err(|_| unavailable())?;
    rx.await.map_err(|_| unavailable())
}

/// `POST /api/v1/settings/battery` with `{"installed": "2024-03-15"}` or `{"installed": null}`.
async fn set_battery(
    State(state): State<AppState>,
    Json(req): Json<BatteryRequest>,
) -> Result<Json<SettingResponse>, ApiError> {
    if req.installed.is_some_and(|d| d > Local::now().date_naive()) {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            "the installation date can't be in the future".into(),
        ));
    }
    // "Replaced now": today's date and the exact time (review M3-8).
    let (date, at) = if req.replaced_now {
        (Some(Local::now().date_naive()), Some(Utc::now()))
    } else {
        (req.installed, None)
    };
    ask_ingest(&state, |reply| {
        crate::poller::PollerMsg::SetBatteryInstalled { date, at, reply }
    })
    .await?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    Ok(Json(SettingResponse {
        ok: true,
        message: match (req.replaced_now, date) {
            (true, _) => "New battery recorded: from now on, health is measured for it".into(),
            (false, Some(d)) => format!("Battery installed on {}", d.format("%d.%m.%Y")),
            (false, None) => "Battery installation date cleared".into(),
        },
    }))
}

/// `POST /api/v1/battery/test` with `{"action":"start","stop_at_pct":50}` or
/// `{"action":"cancel"}`. Nothing is sent to the UPS: the user unplugs it.
async fn runtime_test(
    State(state): State<AppState>,
    Json(request): Json<RuntimeTestRequest>,
) -> Result<Json<SettingResponse>, ApiError> {
    let message = ask_ingest(&state, |reply| crate::poller::PollerMsg::RuntimeTest {
        request,
        reply,
    })
    .await?
    .map_err(|e| (StatusCode::CONFLICT, e))?;
    Ok(Json(SettingResponse { ok: true, message }))
}
