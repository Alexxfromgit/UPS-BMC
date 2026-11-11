//! `ups-collector`: the headless UPS-BMC service (ADR-0002).
//!
//! It polls the UPS (1 s by default), stores samples and events in SQLite and
//! serves the local API on `127.0.0.1:47800` for the desktop UI.

mod api;
mod detector;
mod ingest;
mod outages;
mod poller;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock, mpsc};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::{Parser, ValueEnum};
use tokio::sync::broadcast;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use ups_api::{Connection, DEFAULT_PORT, StatusResponse};
use ups_source::SourceConfig;
use ups_source::hid::HidConfig;
use ups_store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SourceKind {
    /// USB HID UPS attached to this computer.
    Hid,
    /// Simulated UPS with an outage every 5 minutes (for testing the UI).
    Sim,
}

/// UPS-BMC collector: polls the UPS, records telemetry and serves the local API.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Telemetry source.
    #[arg(long, value_enum, default_value_t = SourceKind::Hid)]
    source: SourceKind,
    /// Only use a HID UPS with this USB vendor ID (hex).
    #[arg(long, value_parser = parse_hex_u16)]
    vid: Option<u16>,
    /// Only use a HID UPS with this USB product ID (hex).
    #[arg(long, value_parser = parse_hex_u16)]
    pid: Option<u16>,
    /// Address for the API (loopback only until remote access lands in M4).
    #[arg(long, default_value_t = SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)))]
    bind: SocketAddr,
    /// Polling interval in milliseconds (minimum 250).
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u64).range(250..))]
    poll_ms: u64,
    /// Database file (default: the per-user data directory).
    #[arg(long)]
    db: Option<PathBuf>,
    /// Keep raw samples for this many days.
    #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u32).range(1..=3650))]
    retention_days: u32,
    /// Simulator clock speed-up (testing only).
    #[arg(long, default_value_t = 1, hide = true)]
    sim_speed: u32,
    /// Directory for log files (default: the per-user data directory).
    #[arg(long)]
    log_dir: Option<PathBuf>,
}

fn parse_hex_u16(s: &str) -> Result<u16, String> {
    let digits = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u16::from_str_radix(digits, 16).map_err(|e| format!("`{s}`: {e}"))
}

fn data_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "UPS-BMC")
        .context("could not determine the user data directory")?;
    Ok(dirs.data_local_dir().to_path_buf())
}

fn init_logging(dir: &std::path::Path) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("collector")
        .filename_suffix("log")
        .max_log_files(14)
        .build(dir)
        .context("creating log file")?;
    let (file, guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file),
        )
        .init();
    Ok(guard)
}

fn main() -> Result<()> {
    let args = Args::parse();
    if !args.bind.ip().is_loopback() {
        bail!(
            "refusing to listen on {}: the API has no authentication yet, so only loopback \
             addresses are allowed (remote access is planned for M4)",
            args.bind
        );
    }

    let data = data_dir()?;
    let _log_guard = init_logging(&args.log_dir.clone().unwrap_or_else(|| data.join("logs")))?;
    let db_path = args
        .db
        .clone()
        .unwrap_or_else(|| data.join("ups-bmc.sqlite"));
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let store = Store::open(&db_path).with_context(|| format!("opening {}", db_path.display()))?;

    let source = match args.source {
        SourceKind::Hid => SourceConfig::Hid(HidConfig {
            vendor_id: args.vid,
            product_id: args.pid,
            ..HidConfig::default()
        }),
        SourceKind::Sim => SourceConfig::Sim {
            speed: args.sim_speed,
        },
    };
    let interval = Duration::from_millis(args.poll_ms);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("ups-api")
        .build()?;
    // Bind first, so a second instance fails fast without touching the device.
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind(args.bind))
        .with_context(|| {
            format!(
                "cannot listen on {}: is another collector already running?",
                args.bind
            )
        })?;

    let status = Arc::new(RwLock::new(StatusResponse {
        collector_version: env!("CARGO_PKG_VERSION").into(),
        source: format!("{:?}", args.source).to_lowercase(),
        poll_interval_ms: args.poll_ms,
        started_at: Utc::now(),
        connection: Connection::Connecting,
        device: store.device().ok().flatten(),
        capabilities: Vec::new(),
        assumed_load_w: None,
        runtime_test: None,
        snapshot: None,
    }));
    let (live, _) = broadcast::channel(256);

    let (poll_tx, poll_rx) = mpsc::channel();
    let ingest_tx = poll_tx.clone();
    let (control_tx, control_rx) = mpsc::channel::<poller::Control>();
    let ingest = ingest::Ingest {
        store,
        status: status.clone(),
        live: live.clone(),
        retention: chrono::Duration::days(i64::from(args.retention_days)),
    }
    .spawn(poll_rx)?;
    let poller = poller::spawn(source, interval, poll_tx, control_rx)?;

    info!(
        bind = %args.bind,
        db = %db_path.display(),
        source = ?args.source,
        poll_ms = args.poll_ms,
        "UPS-BMC collector started"
    );

    let app = api::router(api::AppState {
        status,
        live,
        db_path,
        control: control_tx.clone(),
        ingest: ingest_tx,
    });
    let served = runtime.block_on(async {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
                info!("shutting down");
            })
            .await
    });
    if let Err(e) = served {
        warn!(error = %e, "API server stopped with an error");
    }

    // Stop the poller; the ingest thread then drains, records "stopped" and exits.
    let _ = control_tx.send(poller::Control::Stop);
    let _ = poller.join();
    // Stop API tasks first: open WebSocket sessions hold senders to the
    // ingest thread, which only finishes once every sender is gone.
    runtime.shutdown_timeout(Duration::from_secs(2));
    let _ = ingest.join();
    Ok(())
}
