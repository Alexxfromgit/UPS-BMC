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
    keep_dir: bool,
}

impl Drop for Collector {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if !self.keep_dir {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
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
    start_in(dir, port, extra)
}

fn start_in(dir: std::path::PathBuf, port: u16, extra: &[&str]) -> Collector {
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
        keep_dir: false,
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

    // Coverage is written every 10 s.
    let stats = wait_for(Duration::from_secs(20), || {
        get::<ups_api::StatsResponse>(&c, "/api/v1/stats")
            .filter(|s| s.periods[0].on_battery_s > 0.0 && !s.months.is_empty())
    });
    assert_eq!(stats.periods[0].label, "Today");
    assert!(stats.periods[0].outages >= 1);
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

#[test]
fn runs_a_guided_runtime_test_and_reports_health() {
    // 30× simulator: a 10 s cycle, on mains and full for the first ~2 s.
    let c = start_with(&["--sim-speed", "30"]);
    wait_for(Duration::from_secs(20), || {
        get::<StatusResponse>(&c, "/api/v1/status").filter(|s| s.snapshot.is_some())
    });
    // Browsers can't start a test.
    let start = r#"{"action":"start","stop_at_pct":50}"#;
    assert_eq!(
        post(
            &c,
            "/api/v1/battery/test",
            start,
            Some("https://evil.example")
        )
        .0,
        403
    );
    assert_eq!(
        post(
            &c,
            "/api/v1/battery/test",
            r#"{"action":"start","stop_at_pct":5}"#,
            None
        )
        .0,
        409
    );
    // Arming only works on mains with a full battery: retry until then.
    wait_for(Duration::from_secs(30), || {
        (post(&c, "/api/v1/battery/test", start, None).0 == 200).then_some(())
    });
    let status: StatusResponse = get(&c, "/api/v1/status").unwrap();
    assert!(status.runtime_test.is_some());

    // The simulated outage is the test: it is tagged, and the user is asked
    // to restore power at 50 %.
    let outage = wait_for(Duration::from_secs(30), || {
        get::<ups_api::OutagesResponse>(&c, "/api/v1/outages?days=1")?
            .outages
            .into_iter()
            .find(|o| o.runtime_test && o.end.is_some())
    });
    assert!(outage.charge_min_pct.unwrap() <= 50.0);
    let events: EventsResponse = get(&c, "/api/v1/events?limit=50").unwrap();
    let kinds: Vec<EventKind> = events.events.iter().map(|e| e.kind).collect();
    assert!(
        kinds.contains(&EventKind::RuntimeTestRestorePower),
        "{kinds:?}"
    );
    assert!(
        events
            .events
            .iter()
            .any(|e| e.message.starts_with("Runtime test finished")),
        "{events:?}"
    );
    let status: StatusResponse = get(&c, "/api/v1/status").unwrap();
    assert!(status.runtime_test.is_none());
    assert_eq!(
        post(&c, "/api/v1/battery/test", r#"{"action":"cancel"}"#, None).0,
        409
    );

    // Health: 3-second outages are too short to use, but the report works.
    let health: ups_api::HealthReport = get(&c, "/api/v1/health").unwrap();
    assert_eq!(health.verdict, ups_api::HealthVerdict::Unknown);
    assert!(health.skipped >= 1);

    // Battery date: future dates are refused; a past date is stored.
    let (code, _) = post(
        &c,
        "/api/v1/settings/battery",
        r#"{"installed":"2999-01-01"}"#,
        None,
    );
    assert_eq!(code, 422);
    let (code, body) = post(
        &c,
        "/api/v1/settings/battery",
        r#"{"installed":"2024-03-15"}"#,
        None,
    );
    assert_eq!(code, 200, "{body}");
    let health: ups_api::HealthReport = get(&c, "/api/v1/health").unwrap();
    assert_eq!(
        health.battery_installed,
        chrono::NaiveDate::from_ymd_opt(2024, 3, 15)
    );
}

#[test]
fn refuses_foreign_host_names() {
    // DNS rebinding: a web page reaching 127.0.0.1 under its own host name.
    let c = start();
    wait_for(Duration::from_secs(20), || {
        get::<StatusResponse>(&c, "/api/v1/status").filter(|s| s.snapshot.is_some())
    });
    let resp = ureq::get(format!("http://{}/api/v1/status", c.base))
        .header("Host", "evil.example:47800")
        .config()
        .http_status_as_error(false)
        .build()
        .call()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 403);
    let port = c.base.rsplit(':').next().unwrap();
    let resp = ureq::get(format!("http://{}/api/v1/status", c.base))
        .header("Host", format!("localhost:{port}"))
        .call()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 200);
}

#[test]
fn survives_a_restart_mid_outage() {
    // 10× simulator: outage from ~6 s to ~15 s after start.
    let port = free_port();
    let dir = std::env::temp_dir().join(format!("ups-collector-e2e-{}-{port}", std::process::id()));
    let mut first = start_in(dir.clone(), port, &["--sim-speed", "10"]);
    first.keep_dir = true;
    wait_for(Duration::from_secs(30), || {
        get::<ups_api::OutagesResponse>(&first, "/api/v1/outages?days=1")?
            .outages
            .into_iter()
            .find(|o| o.is_ongoing() && o.samples >= 4)
    });
    // Graceful stop through the API (what the installer uses).
    let (code, _) = post(&first, "/api/v1/shutdown", "{}", None);
    assert_eq!(code, 200);
    wait_for(Duration::from_secs(15), || {
        first.child.try_wait().ok().flatten()
    });
    drop(first);

    let second = start_in(dir, port, &["--sim-speed", "10"]);
    let outages = wait_for(Duration::from_secs(40), || {
        get::<ups_api::OutagesResponse>(&second, "/api/v1/outages?days=1")
            .map(|r| r.outages)
            .filter(|o| o.iter().any(|o| o.end.is_some()))
    });
    // The restarted simulator begins its cycle on mains, so the outage ends
    // at the restart. The short downtime is not a monitoring gap: the outage
    // was resumed and closed with both ends observed, and its sample count
    // (which weights resumed averages; unit-tested in the tracker) survived.
    let o = outages.iter().find(|o| o.end.is_some()).unwrap();
    assert_eq!(outages.len(), 1, "{outages:?}");
    assert!(o.start_observed() && o.end_observed(), "{o:?}");
    assert!(o.samples >= 4, "{o:?}");
    let events: EventsResponse = get(&second, "/api/v1/events?limit=50").unwrap();
    assert!(
        events
            .events
            .iter()
            .any(|e| e.kind == EventKind::CollectorStopped),
        "graceful stop recorded"
    );
}
