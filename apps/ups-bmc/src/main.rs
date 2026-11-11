//! `ups-bmc`: the UPS-BMC desktop UI (Slint).
//!
//! It shows live data from the local collector (`ups-collector`), keeps an
//! icon in the notification area and raises desktop notifications for power
//! events. Closing the window only hides it; use "Quit" in the tray menu to exit.
//! If the collector isn't running, the UI starts it once.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod autostart;
mod client;
mod health_view;
mod launch;
mod view;

use std::cell::RefCell;
use std::rc::Rc;

use anyhow::Result;
use clap::Parser;
use slint::{ComponentHandle, Model, ModelRc, SharedString, StandardListViewItem, VecModel};
use ups_api::{DEFAULT_PORT, Event, EventKind, HealthReport, RuntimeTestRequest};

use crate::client::{ClientConfig, Update};
use crate::view::{EventLine, OutagesView, View};

/// Code generated from `ui/app.slint`.
#[allow(
    missing_debug_implementations,
    clippy::all,
    clippy::pedantic,
    clippy::todo
)]
mod ui {
    slint::include_modules!();
}
use ui::{AppWindow, EventRow, StatCardData, Tray};

/// UPS-BMC desktop UI.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Collector address.
    #[arg(long, default_value_t = format!("127.0.0.1:{DEFAULT_PORT}"))]
    collector: String,
    /// Don't start the collector automatically.
    #[arg(long)]
    no_auto_start: bool,
    /// If the UI starts the collector, use the simulated UPS (demo/testing).
    #[arg(long)]
    sim: bool,
    /// Start hidden in the notification area (for autostart).
    #[arg(long)]
    minimized: bool,
}

fn apply_view(w: &AppWindow, tray: &Tray, v: &View) {
    let s = |x: &str| SharedString::from(x);
    w.set_state(v.state);
    w.set_device_name(s(&v.device_name));
    w.set_connection_text(s(&v.connection_text));
    w.set_updated_text(s(&v.updated_text));
    w.set_status_title(s(&v.status_title));
    w.set_status_detail(s(&v.status_detail));
    w.set_show_start(v.show_start);
    w.set_charge_text(s(&v.charge_text));
    w.set_charge_arc(s(&v.charge_arc));
    w.set_runtime_text(s(&v.runtime_text));
    w.set_load_text(s(&v.load_text));
    w.set_load_detail(s(&v.load_detail));
    w.set_input_text(s(&v.input_text));
    w.set_input_detail(s(&v.input_detail));
    w.set_output_text(s(&v.output_text));
    w.set_output_detail(s(&v.output_detail));
    w.set_battery_v_text(s(&v.battery_v_text));
    w.set_battery_v_detail(s(&v.battery_v_detail));
    // Re-rendered at the current plot size on every update (1/s), so window
    // resizes are picked up within a second.
    let charts = v.charts.render(
        f64::from(w.get_plot_width()),
        f64::from(w.get_plot_height()),
    );
    w.set_chart_shade(s(&charts.shade));
    w.set_chart_charge(s(&charts.charge));
    w.set_chart_load(s(&charts.load));
    w.set_chart_input(s(&charts.input));
    w.set_chart_output(s(&charts.output));
    w.set_volt_top(s(&v.volt_top));
    w.set_volt_bottom(s(&v.volt_bottom));
    w.set_chart_span(s(&v.chart_span));
    w.set_footer_text(s(&v.footer_text));
    w.set_beeper_available(v.beeper_available);
    w.set_beeper_text(s(&v.beeper_text));
    w.set_assumed_load_text(s(&v.assumed_load_text));
    // Don't fight the switch while a change is being applied.
    if !w.get_beeper_busy() {
        w.set_beeper_on(v.beeper_on);
    }
    w.set_test_phase(v.test_phase);
    w.set_test_text(s(&v.test_text));
    if v.test_phase != 0 {
        w.set_test_message(SharedString::new());
    }
    tray.set_state(v.state);
    tray.set_tip(s(&v.tooltip));
    render_trend(w);
}

thread_local! {
    /// The latest health trend (UI thread), re-rendered at the plot's size.
    static TREND: RefCell<health_view::TrendData> = RefCell::default();
    /// The installation date last shown, so typing isn't overwritten.
    static SHOWN_INSTALLED: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn render_trend(w: &AppWindow) {
    let t = TREND.with_borrow(|t| {
        t.render(
            f64::from(w.get_trend_width()),
            f64::from(w.get_trend_height()),
        )
    });
    w.set_trend_line(t.line.into());
    w.set_trend_dots(t.dots.into());
    w.set_trend_guides(t.guides.into());
}

fn apply_health(w: &AppWindow, report: &HealthReport) {
    let v = health_view::build(report, chrono::Utc::now());
    let s = |x: &str| SharedString::from(x);
    w.set_health_level(v.level);
    w.set_health_verdict(s(&v.verdict));
    w.set_health_text(s(&v.health_text));
    w.set_health_detail(s(&v.health_detail));
    w.set_health_runtime_text(s(&v.runtime_text));
    w.set_health_runtime_detail(s(&v.runtime_detail));
    w.set_health_age_text(s(&v.age_text));
    w.set_health_age_detail(s(&v.age_detail));
    w.set_health_ups_text(s(&v.ups_text));
    w.set_health_ups_detail(s(&v.ups_detail));
    w.set_health_reasons(s(&v.reasons));
    w.set_trend_span(s(&v.trend_span));
    w.set_health_summary(s(&v.summary));
    w.set_discharge_rows(table(&v.rows));
    let changed = SHOWN_INSTALLED.with_borrow_mut(|shown| {
        let changed = shown.as_deref() != Some(v.installed_value.as_str());
        *shown = Some(v.installed_value.clone());
        changed
    });
    if changed {
        w.set_installed_input(s(&v.installed_value));
    }
    TREND.set(v.trend);
    render_trend(w);
}

/// A table model from rows of cells.
fn table(rows: &[Vec<String>]) -> ModelRc<ModelRc<StandardListViewItem>> {
    let rows: Vec<ModelRc<StandardListViewItem>> = rows
        .iter()
        .map(|r| {
            let cells: Vec<StandardListViewItem> = r
                .iter()
                .map(|c| StandardListViewItem::from(c.as_str()))
                .collect();
            ModelRc::from(Rc::new(VecModel::from(cells)))
        })
        .collect();
    ModelRc::from(Rc::new(VecModel::from(rows)))
}

/// Runs a collector request off the UI thread, shows its result with `show`,
/// then refreshes the Battery health tab.
fn request_then_refresh(
    w: slint::Weak<AppWindow>,
    address: String,
    request: impl FnOnce(&str) -> Result<String, String> + Send + 'static,
    show: fn(&AppWindow, SharedString),
) {
    std::thread::spawn(move || {
        let result = request(&address);
        let health = client::fetch_health(&address).ok();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(w) = w.upgrade() else { return };
            show(&w, result.unwrap_or_else(|e| e).into());
            if let Some(h) = health {
                apply_health(&w, &h);
            }
        });
    });
}

/// Battery health tab: installation date and the guided runtime test.
fn wire_health(window: &AppWindow, collector: &str) {
    window.set_test_stop_index(health_view::DEFAULT_STOP_INDEX);

    let (w, address) = (window.as_weak(), collector.to_owned());
    window.on_set_battery_installed(move |text| {
        let text = text.trim();
        let date = if text.is_empty() {
            None
        } else if let Some(d) = ["%Y-%m-%d", "%d.%m.%Y"]
            .iter()
            .find_map(|f| chrono::NaiveDate::parse_from_str(text, f).ok())
        {
            Some(d)
        } else {
            if let Some(w) = w.upgrade() {
                w.set_battery_message("Enter the date as YYYY-MM-DD, e.g. 2024-03-15.".into());
            }
            return;
        };
        request_then_refresh(
            w.clone(),
            address.clone(),
            move |a| client::set_battery_installed(a, date),
            AppWindow::set_battery_message,
        );
    });

    let (w, address) = (window.as_weak(), collector.to_owned());
    window.on_battery_replaced_today(move || {
        let today = chrono::Local::now().date_naive();
        request_then_refresh(
            w.clone(),
            address.clone(),
            move |a| client::set_battery_installed(a, Some(today)),
            AppWindow::set_battery_message,
        );
    });

    let (w, address) = (window.as_weak(), collector.to_owned());
    window.on_start_runtime_test(move |index| {
        let Some(&stop_at_pct) = usize::try_from(index)
            .ok()
            .and_then(|i| health_view::STOP_LEVELS.get(i))
        else {
            return;
        };
        request_then_refresh(
            w.clone(),
            address.clone(),
            move |a| client::runtime_test(a, RuntimeTestRequest::Start { stop_at_pct }),
            AppWindow::set_test_message,
        );
    });

    let (w, address) = (window.as_weak(), collector.to_owned());
    window.on_cancel_runtime_test(move || {
        request_then_refresh(
            w.clone(),
            address.clone(),
            |a| client::runtime_test(a, RuntimeTestRequest::Cancel),
            AppWindow::set_test_message,
        );
    });
}

fn apply_events(w: &AppWindow, lines: Vec<EventLine>) {
    let rows: Vec<EventRow> = lines
        .into_iter()
        .map(|l| EventRow {
            time: l.time.into(),
            text: l.text.into(),
            level: l.level,
        })
        .collect();
    w.set_events(ModelRc::from(Rc::new(VecModel::from(rows))));
}

fn apply_outages(w: &AppWindow, v: &OutagesView) {
    let cards: Vec<StatCardData> = v
        .cards
        .iter()
        .map(|c| StatCardData {
            title: c.title.as_str().into(),
            outages: c.outages.as_str().into(),
            on_battery: c.on_battery.as_str().into(),
            availability: c.availability.as_str().into(),
            coverage: c.coverage.as_str().into(),
        })
        .collect();
    w.set_stat_cards(ModelRc::from(Rc::new(VecModel::from(cards))));
    w.set_outage_rows(table(&v.rows));
    w.set_outages_summary(v.summary.as_str().into());
    let strings = |items: &[String]| {
        let v: Vec<SharedString> = items.iter().map(|s| s.as_str().into()).collect();
        ModelRc::from(Rc::new(VecModel::from(v)))
    };
    // Only replace the month list when it changed, so the selection stays.
    let keys: Vec<String> = w.get_month_keys().iter().map(|s| s.to_string()).collect();
    if keys != v.months {
        w.set_month_keys(strings(&v.months));
        w.set_month_labels(strings(&v.month_labels));
        w.set_month_index(0);
    }
}

/// Opens a saved report with its default application (HTML: the browser),
/// or shows a CSV in the file manager.
fn open_path(path: &std::path::Path, reveal: bool) -> std::io::Result<()> {
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("explorer");
        if reveal {
            c.arg(format!("/select,{}", path.display()));
        } else {
            c.arg(path);
        }
        c
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(if reveal {
            path.parent().unwrap_or(path)
        } else {
            path
        });
        c
    };
    cmd.spawn().map(|_| ())
}

/// Reports tab: download the selected month's report and open it.
fn wire_reports(window: &AppWindow, collector: &str) {
    let w = window.as_weak();
    let address = collector.to_owned();
    window.on_open_report(move |index, csv| {
        let Some(win) = w.upgrade() else { return };
        let Some(month) = usize::try_from(index)
            .ok()
            .and_then(|i| win.get_month_keys().row_data(i))
        else {
            return;
        };
        win.set_report_message("Generating report…".into());
        let (w, address, month) = (w.clone(), address.clone(), month.to_string());
        std::thread::spawn(move || {
            let result = save_report(&address, &month, csv);
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = w.upgrade() {
                    w.set_report_message(
                        match result {
                            Ok(path) => format!("Saved {}", path.display()),
                            Err(e) => format!("Report failed: {e}"),
                        }
                        .into(),
                    );
                }
            });
        });
    });
}

fn save_report(address: &str, month: &str, csv: bool) -> Result<std::path::PathBuf, String> {
    let body = client::fetch_report(address, month, csv)?;
    let (year, mon) =
        ups_analysis::stats::parse_month(month).ok_or_else(|| format!("bad month {month}"))?;
    let dir = directories::UserDirs::new()
        .and_then(|d| d.document_dir().map(|p| p.join("UPS-BMC Reports")))
        .ok_or("could not find the Documents folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(ups_analysis::report::file_name(
        year,
        mon,
        if csv { "csv" } else { "html" },
    ));
    std::fs::write(&path, body).map_err(|e| e.to_string())?;
    open_path(&path, csv).map_err(|e| e.to_string())?;
    Ok(path)
}

/// `AppUserModelID` of the installed Start Menu shortcut (`installer/ups-bmc.iss`).
#[cfg(windows)]
const APP_USER_MODEL_ID: &str = "UPS-BMC.Desktop";

#[cfg(windows)]
fn installed_shortcut_exists() -> bool {
    std::env::var_os("APPDATA").is_some_and(|appdata| {
        std::path::Path::new(&appdata)
            .join(r"Microsoft\Windows\Start Menu\Programs\UPS-BMC.lnk")
            .exists()
    })
}

/// Desktop notification for the events that matter to a person.
fn notify(event: &Event) {
    let title = match event.kind {
        EventKind::PowerLost => "Power outage",
        EventKind::PowerRestored => "Power restored",
        EventKind::LowBattery => "UPS battery low!",
        EventKind::CommunicationLost => "UPS connection lost",
        EventKind::CommunicationRestored => "UPS connection restored",
        EventKind::Overload => "UPS overloaded",
        EventKind::ReplaceBattery => "Replace the UPS battery",
        EventKind::RuntimeTestRestorePower => "Runtime test: plug the UPS back in",
        EventKind::RuntimeTest if event.message.starts_with("Runtime test finished") => {
            "Runtime test finished"
        }
        _ => return,
    };
    let mut n = notify_rust::Notification::new();
    n.appname("UPS-BMC").summary(title).body(&event.message);
    // Installed builds have a Start Menu shortcut carrying our AppUserModelID,
    // so Windows shows "UPS-BMC" as the sender. Dev builds keep the default.
    #[cfg(windows)]
    if installed_shortcut_exists() {
        n.app_id(APP_USER_MODEL_ID);
    }
    // Notifications are best-effort; failures (e.g. disabled by policy) are ignored.
    let _ = n.show();
}

/// Settings dialog: beeper (through the collector, ADR-0003) and autostart.
fn wire_settings(window: &AppWindow, collector: &str) {
    window.set_autostart_available(autostart::supported());
    window.set_autostart_on(autostart::is_enabled());

    let w = window.as_weak();
    window.on_set_autostart(move |enabled| {
        let Some(w) = w.upgrade() else { return };
        match autostart::set_enabled(enabled) {
            Ok(()) => w.set_settings_message(SharedString::new()),
            Err(e) => {
                w.set_autostart_on(!enabled);
                w.set_settings_message(format!("Could not change autostart: {e}").into());
            }
        }
    });

    let w = window.as_weak();
    let address = collector.to_owned();
    window.on_set_assumed_load(move |text| {
        let text = text.trim().replace(',', ".");
        let watts = if text.is_empty() {
            None
        } else {
            match text.parse::<f32>() {
                Ok(v) if (1.0..=2000.0).contains(&v) => Some(v),
                _ => {
                    if let Some(win) = w.upgrade() {
                        win.set_settings_message(
                            "Enter a number of watts between 1 and 2000.".into(),
                        );
                    }
                    return;
                }
            }
        };
        let (w, address) = (w.clone(), address.clone());
        std::thread::spawn(move || {
            let result = client::set_assumed_load(&address, watts);
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(w) = w.upgrade() {
                    w.set_settings_message(
                        match result {
                            Ok(m) => m,
                            Err(e) => format!("Estimate not saved: {e}"),
                        }
                        .into(),
                    );
                }
            });
        });
    });

    let w = window.as_weak();
    let address = collector.to_owned();
    window.on_set_beeper(move |enabled| {
        if let Some(win) = w.upgrade() {
            win.set_beeper_busy(true);
            win.set_settings_message(SharedString::new());
        }
        let (w, address) = (w.clone(), address.clone());
        // Network call off the UI thread; the result comes back via the event loop.
        std::thread::spawn(move || {
            let result = client::set_beeper(&address, enabled);
            let _ = slint::invoke_from_event_loop(move || {
                let Some(w) = w.upgrade() else { return };
                w.set_beeper_busy(false);
                if let Err(e) = result {
                    w.set_beeper_on(!enabled);
                    w.set_settings_message(format!("Beeper not changed: {e}").into());
                }
            });
        });
    });
}

fn wire_tray(window: &AppWindow, tray: &Tray) {
    let w = window.as_weak();
    tray.on_open(move || {
        if let Some(w) = w.upgrade() {
            let _ = w.show();
        }
    });
    tray.on_quit(|| {
        let _ = slint::quit_event_loop();
    });
}

fn main() -> Result<()> {
    let args = Args::parse();

    let window = AppWindow::new()?;
    let tray = Tray::new()?;
    apply_view(
        &window,
        &tray,
        &view::collector_offline("Connecting…", false),
    );
    wire_tray(&window, &tray);
    wire_settings(&window, &args.collector);
    wire_reports(&window, &args.collector);
    wire_health(&window, &args.collector);

    let collector_args: Vec<String> = if args.sim {
        vec!["--source".into(), "sim".into()]
    } else {
        Vec::new()
    };
    {
        let w = window.as_weak();
        let collector_args = collector_args.clone();
        window.on_start_collector(move || {
            let detail = match launch::start_collector(&collector_args) {
                Ok(()) => "Starting the collector…".to_owned(),
                Err(e) => format!("Could not start the collector: {e}"),
            };
            if let Some(w) = w.upgrade() {
                w.set_status_detail(detail.into());
                w.set_show_start(false);
            }
        });
    }

    let w = window.as_weak();
    let t = tray.as_weak();
    client::spawn(
        ClientConfig {
            address: args.collector.clone(),
            auto_start: !args.no_auto_start,
            collector_args,
        },
        move |update| {
            let (w, t) = (w.clone(), t.clone());
            let _ = slint::invoke_from_event_loop(move || {
                let (Some(w), Some(t)) = (w.upgrade(), t.upgrade()) else {
                    return;
                };
                match update {
                    Update::View(v) => apply_view(&w, &t, &v),
                    Update::Events(lines) => apply_events(&w, lines),
                    Update::Notify(e) => notify(&e),
                    Update::Outages(v) => apply_outages(&w, &v),
                    Update::Health(h) => apply_health(&w, &h),
                }
            });
        },
    )?;

    tray.show()?;
    if !args.minimized {
        window.show()?;
    }
    // The tray keeps the event loop alive while the window is hidden.
    slint::run_event_loop_until_quit()?;
    Ok(())
}
