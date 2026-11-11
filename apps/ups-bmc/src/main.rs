//! `ups-bmc`: the UPS-BMC desktop UI (Slint).
//!
//! It shows live data from the local collector (`ups-collector`), keeps an
//! icon in the notification area and raises desktop notifications for power
//! events. Closing the window only hides it; use "Quit" in the tray menu to exit.
//! If the collector isn't running, the UI starts it once.

#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod autostart;
mod client;
mod launch;
mod view;

use std::rc::Rc;

use anyhow::Result;
use clap::Parser;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};
use ups_api::{DEFAULT_PORT, Event, EventKind};

use crate::client::{ClientConfig, Update};
use crate::view::{EventLine, View};

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
use ui::{AppWindow, EventRow, Tray};

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
    // Don't fight the switch while a change is being applied.
    if !w.get_beeper_busy() {
        w.set_beeper_on(v.beeper_on);
    }
    tray.set_state(v.state);
    tray.set_tip(s(&v.tooltip));
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
