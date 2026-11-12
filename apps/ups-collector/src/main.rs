//! `ups-collector`: the headless UPS-BMC service (ADR-0002).
//!
//! It polls the UPS (1 s by default), stores samples and events in SQLite and
//! serves the API on `127.0.0.1:47800` for the desktop UI. With a token it can
//! also serve the LAN, e.g. on a Raspberry Pi reading NUT (ADR-0005), and it
//! sends remote notifications (ntfy, Telegram, email) when configured.

mod api;
mod config;
mod detector;
mod ingest;
mod notify;
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
use ups_source::nut::NutConfig;
use ups_store::Store;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SourceKind {
    /// USB HID UPS attached to this computer.
    #[cfg(feature = "hid")]
    Hid,
    /// A UPS served by NUT's upsd (see --nut).
    Nut,
    /// Simulated UPS with an outage every 5 minutes (for testing the UI).
    Sim,
}

#[cfg(feature = "hid")]
const DEFAULT_SOURCE: SourceKind = SourceKind::Hid;
#[cfg(not(feature = "hid"))]
const DEFAULT_SOURCE: SourceKind = SourceKind::Nut;

/// UPS-BMC collector: polls the UPS, records telemetry and serves the local API.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Telemetry source.
    #[arg(long, value_enum, default_value_t = DEFAULT_SOURCE)]
    source: SourceKind,
    /// NUT server and UPS for `--source nut`: `[upsname@]host[:port]`. Without
    /// a name, the only UPS upsd serves is used.
    #[arg(long, default_value = "localhost", value_parser = NutConfig::parse)]
    nut: NutConfig,
    /// Only use a HID UPS with this USB vendor ID (hex).
    #[cfg(feature = "hid")]
    #[arg(long, value_parser = parse_hex_u16)]
    vid: Option<u16>,
    /// Only use a HID UPS with this USB product ID (hex).
    #[cfg(feature = "hid")]
    #[arg(long, value_parser = parse_hex_u16)]
    pid: Option<u16>,
    /// Address for the API. Anything but a loopback address (e.g.
    /// `0.0.0.0:47800` to serve the LAN) requires an API token.
    #[arg(long, default_value_t = SocketAddr::from(([127, 0, 0, 1], DEFAULT_PORT)))]
    bind: SocketAddr,
    /// File holding the API token that remote clients must send (default:
    /// the `UPS_BMC_TOKEN` environment variable).
    #[arg(long)]
    token_file: Option<PathBuf>,
    /// Configuration file (TOML) for remote notifications.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Print a new random API token and exit.
    #[arg(long)]
    generate_token: bool,
    /// Send a test message to every notification channel in --config and exit.
    #[arg(long, requires = "config")]
    test_notify: bool,
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

#[cfg(feature = "hid")]
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

/// What `main` runs with, once the configuration has been checked.
struct Setup {
    notifier: notify::Notifier,
    token: Option<config::Secret>,
}

/// Handles the one-shot options and checks the configuration. Returns `None`
/// when there is nothing more to do.
fn setup(args: &Args) -> Result<Option<Setup>> {
    if args.generate_token {
        println!("{}", config::generate_token()?);
        return Ok(None);
    }
    let file_config = match &args.config {
        Some(path) => config::FileConfig::load(path)?,
        None => config::FileConfig::default(),
    };
    let notifier = notify::Notifier::from_config(&file_config.notify)?;
    if args.test_notify {
        notify::send_test(&notifier)?;
        return Ok(None);
    }
    let token = config::load_token(args.token_file.as_deref())?;
    if !args.bind.ip().is_loopback() && token.is_none() {
        bail!(
            "refusing to listen on {} without an API token: create one with \
             `ups-collector --generate-token`, save it to a file and pass --token-file \
             (or set {})",
            args.bind,
            ups_api::TOKEN_ENV
        );
    }
    Ok(Some(Setup { notifier, token }))
}

fn source_config(args: &Args) -> SourceConfig {
    match args.source {
        #[cfg(feature = "hid")]
        SourceKind::Hid => SourceConfig::Hid(ups_source::hid::HidConfig {
            vendor_id: args.vid,
            product_id: args.pid,
            ..ups_source::hid::HidConfig::default()
        }),
        SourceKind::Nut => SourceConfig::Nut(args.nut.clone()),
        SourceKind::Sim => SourceConfig::Sim {
            speed: args.sim_speed,
        },
    }
}

fn main() -> Result<()> {
    let args = Args::parse();
    let Some(Setup { notifier, token }) = setup(&args)? else {
        return Ok(());
    };

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

    let source = source_config(&args);
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
        poll: interval,
    }
    .spawn(poll_rx)?;
    let poller = poller::spawn(source, interval, poll_tx, control_rx)?;
    notifier.spawn(&runtime, &live, status.clone());

    info!(
        bind = %args.bind,
        db = %db_path.display(),
        source = ?args.source,
        poll_ms = args.poll_ms,
        token = token.is_some(),
        "UPS-BMC collector started"
    );

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
    let app = api::router(api::AppState {
        status,
        live,
        db_path,
        control: control_tx.clone(),
        ingest: ingest_tx,
        port: args.bind.port(),
        token,
        shutdown: shutdown_tx,
    });
    let served = runtime.block_on(async {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            tokio::select! {
                () = stop_signal() => {}
                _ = shutdown_rx.recv() => {}
            }
            info!("shutting down");
        })
        .await
    });
    if let Err(e) = served {
        warn!(error = %e, "API server stopped with an error");
    }

    stop(&control_tx, poller, runtime, ingest);
    Ok(())
}

/// Stops the threads in order: poller, API tasks, then ingest.
fn stop(
    control_tx: &mpsc::Sender<poller::Control>,
    poller: std::thread::JoinHandle<()>,
    runtime: tokio::runtime::Runtime,
    ingest: std::thread::JoinHandle<()>,
) {
    // Stop the poller; the ingest thread then drains, records "stopped" and exits.
    let _ = control_tx.send(poller::Control::Stop);
    // A UPS that stopped answering can block the poller in a USB read; don't
    // hang forever on it (review M1-6). Exiting the process ends the thread.
    join_within(poller, Duration::from_secs(5), "poller");
    // Stop API tasks first: open WebSocket sessions hold senders to the
    // ingest thread, which only finishes once every sender is gone.
    runtime.shutdown_timeout(Duration::from_secs(2));
    join_within(ingest, Duration::from_secs(10), "ingest");
}

/// Waits for a thread to finish, at most `limit`.
fn join_within(handle: std::thread::JoinHandle<()>, limit: Duration, name: &str) {
    let start = std::time::Instant::now();
    while !handle.is_finished() {
        if start.elapsed() > limit {
            warn!(thread = name, "thread did not stop in time; exiting anyway");
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = handle.join();
}

/// Ctrl+C; on Windows also closing the console, sign-out and shutdown
/// (review M1-5); on Unix also SIGTERM. A collector started without a console (by the UI) is
/// stopped through `POST /api/v1/shutdown` instead.
async fn stop_signal() {
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        let (Ok(mut close), Ok(mut logoff), Ok(mut shutdown)) = (
            windows::ctrl_close(),
            windows::ctrl_logoff(),
            windows::ctrl_shutdown(),
        ) else {
            let _ = tokio::signal::ctrl_c().await;
            return;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = close.recv() => {}
            _ = logoff.recv() => {}
            _ = shutdown.recv() => {}
        }
    }
    // systemd stops services with SIGTERM.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut term) = signal(SignalKind::terminate()) else {
            let _ = tokio::signal::ctrl_c().await;
            return;
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
}
