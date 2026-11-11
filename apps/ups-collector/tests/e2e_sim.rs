//! End-to-end: run the real collector binary with the simulated UPS and talk
//! to its HTTP and WebSocket API, exactly as the desktop UI does.

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use ups_api::{
    Connection, EventKind, EventsResponse, HistoryResponse, LiveMessage, StatusResponse,
};

struct Collector {
    child: Child,
    base: String,
    dir: std::path::PathBuf,
}

impl Drop for Collector {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn start() -> Collector {
    start_with(&[])
}

fn start_with(extra: &[&str]) -> Collector {
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("ups-collector-e2e-{}-{port}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_ups-collector"))
        .args(["--source", "sim", "--poll-ms", "250", "--bind"])
        .arg(format!("127.0.0.1:{port}"))
        .arg("--db")
        .arg(dir.join("test.sqlite"))
        .arg("--log-dir")
        .arg(dir.join("logs"))
        .args(extra)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Collector {
        child,
        base: format!("127.0.0.1:{port}"),
        dir,
    }
}

fn get<T: serde::de::DeserializeOwned>(c: &Collector, path: &str) -> Option<T> {
    ureq::get(format!("http://{}{path}", c.base))
        .call()
        .ok()?
        .body_mut()
        .read_json()
        .ok()
}

fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(start.elapsed() < timeout, "timed out");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn serves_status_history_events_and_live_updates() {
    let c = start();

    // Status: connected to the simulator with a fresh snapshot.
    let status: StatusResponse = wait_for(Duration::from_secs(20), || {
        get::<StatusResponse>(&c, "/api/v1/status").filter(|s| s.snapshot.is_some())
    });
    assert_eq!(status.source, "sim");
    assert_eq!(status.connection, Connection::Connected);
    assert_eq!(status.poll_interval_ms, 250);
    let device = status.device.unwrap();
    assert_eq!(device.source, "sim");
    assert!(status.snapshot.unwrap().battery_charge_pct.is_some());

    // Events: the start is recorded.
    let events: EventsResponse = get(&c, "/api/v1/events?limit=10").unwrap();
    assert!(
        events
            .events
            .iter()
            .any(|e| e.kind == EventKind::CollectorStarted)
    );

    // History: samples are being stored.
    let history: HistoryResponse = wait_for(Duration::from_secs(10), || {
        get::<HistoryResponse>(&c, "/api/v1/history?minutes=5&points=100")
            .filter(|h| h.points.len() >= 2)
    });
    assert!(
        history
            .points
            .iter()
            .all(|p| p.battery_charge_pct.is_some())
    );

    // Live: first a full status, then a stream of updates.
    let (mut ws, _) = tungstenite::connect(format!("ws://{}/api/v1/live", c.base)).unwrap();
    let mut statuses = 0;
    let start = Instant::now();
    while statuses < 3 {
        assert!(start.elapsed() < Duration::from_secs(10), "no live updates");
        if let tungstenite::Message::Text(text) = ws.read().unwrap()
            && let LiveMessage::Status(s) = serde_json::from_str(text.as_str()).unwrap()
        {
            assert_eq!(s.connection, Connection::Connected);
            statuses += 1;
        }
    }
}

fn post(c: &Collector, path: &str, body: &str, origin: Option<&str>) -> (u16, String) {
    let mut req = ureq::post(format!("http://{}{path}", c.base))
        .config()
        .http_status_as_error(false)
        .build()
        .header("Content-Type", "application/json");
    if let Some(o) = origin {
        req = req.header("Origin", o);
    }
    let mut resp = req.send(body).unwrap();
    let status = resp.status().as_u16();
    (status, resp.body_mut().read_to_string().unwrap_or_default())
}

fn beeper(c: &Collector) -> Option<String> {
    get::<StatusResponse>(c, "/api/v1/status")?
        .snapshot?
        .extra
        .get("ups.beeper.status")
        .cloned()
}

#[test]
fn switches_the_beeper_and_refuses_browsers() {
    let c = start();
    let status: StatusResponse = wait_for(Duration::from_secs(20), || {
        get::<StatusResponse>(&c, "/api/v1/status").filter(|s| s.snapshot.is_some())
    });
    assert_eq!(status.capabilities, ["beeper"]);
    assert_eq!(beeper(&c).as_deref(), Some("enabled"));

    // A web page (Origin header) must not be able to change UPS settings.
    let (code, _) = post(
        &c,
        "/api/v1/settings/beeper",
        r#"{"enabled":false}"#,
        Some("https://evil.example"),
    );
    assert_eq!(code, 403);
    assert_eq!(beeper(&c).as_deref(), Some("enabled"));

    // The UI's request works, is reflected in the next snapshot and logged.
    let (code, body) = post(&c, "/api/v1/settings/beeper", r#"{"enabled":false}"#, None);
    assert_eq!(code, 200, "{body}");
    assert!(body.contains("Beeper disabled"), "{body}");
    wait_for(Duration::from_secs(5), || {
        (beeper(&c).as_deref() == Some("disabled")).then_some(())
    });
    let events: EventsResponse = get(&c, "/api/v1/events?limit=10").unwrap();
    assert!(
        events
            .events
            .iter()
            .any(|e| e.kind == EventKind::SettingChanged && e.message.contains("disabled"))
    );

    // Malformed body: rejected before reaching the device.
    let (code, _) = post(&c, "/api/v1/settings/beeper", r#"{"on":1}"#, None);
    assert_eq!(code, 422);
}

#[test]
fn refuses_non_loopback_bind() {
    let out = Command::new(env!("CARGO_BIN_EXE_ups-collector"))
        .args(["--source", "sim", "--bind", "0.0.0.0:47999"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("only loopback"), "{stderr}");
}

fn text(c: &Collector, path: &str) -> (u16, String) {
    let mut resp = ureq::get(format!("http://{}{path}", c.base))
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .unwrap();
    let status = resp.status().as_u16();
    (status, resp.body_mut().read_to_string().unwrap_or_default())
}

#[test]
fn records_outages_stats_and_reports() {
    // 30× simulator: outage from ~2 s to ~5 s, recharged by ~10 s.
    let c = start_with(&["--sim-speed", "30"]);
    let outage = wait_for(Duration::from_secs(30), || {
        get::<ups_api::OutagesResponse>(&c, "/api/v1/outages?days=1")?
            .outages
            .into_iter()
            .find(|o| o.recharged_at.is_some())
    });
    assert!(outage.end.is_some());
    assert!(outage.start_observed() && outage.end_observed());
    assert_eq!(outage.charge_start_pct, Some(100.0));
    assert!(outage.charge_min_pct.unwrap() < 30.0, "{outage:?}");
    assert!(outage.energy_wh.unwrap() > 0.0);
    assert!(outage.recharge_duration().unwrap() > chrono::Duration::zero());

    let stats: ups_api::StatsResponse = get(&c, "/api/v1/stats").unwrap();
    assert_eq!(stats.periods[0].label, "Today");
    assert!(stats.periods[0].outages >= 1);
    assert!(stats.periods[0].on_battery_s > 0.0, "{stats:?}");
    let month = stats.months.first().cloned().expect("current month listed");

    let (code, html) = text(&c, &format!("/api/v1/report?month={month}&format=html"));
    assert_eq!(code, 200);
    assert!(html.contains("Power report"), "{html}");
    let (code, csv) = text(&c, &format!("/api/v1/report?month={month}&format=csv"));
    assert_eq!(code, 200);
    assert!(csv.lines().count() >= 2, "{csv}");
    assert_eq!(text(&c, "/api/v1/report?month=2026-13").0, 400);
    assert_eq!(
        text(&c, &format!("/api/v1/report?month={month}&format=pdf")).0,
        400
    );
}
