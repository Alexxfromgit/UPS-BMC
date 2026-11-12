//! The NUT source against a fake `upsd` speaking the network protocol.

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use ups_model::UpsStatus;
use ups_source::nut::{NutConfig, NutSource};
use ups_source::{SourceError, TelemetrySource};

/// Serves canned replies; records every command it receives.
fn fake_upsd(
    upses: &'static [&'static str],
    status: Arc<Mutex<&'static str>>,
) -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let (log, status) = (log.clone(), status.clone());
            std::thread::spawn(move || serve(stream, upses, &status, &log));
        }
    });
    (port, seen)
}

fn serve(
    stream: TcpStream,
    upses: &[&str],
    status: &Mutex<&'static str>,
    log: &Mutex<Vec<String>>,
) {
    let mut out = stream.try_clone().unwrap();
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { return };
        log.lock().unwrap().push(line.clone());
        let reply = match line.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["LIST", "UPS"] => {
                let mut r = String::from("BEGIN LIST UPS\n");
                for u in upses {
                    let _ = writeln!(r, "UPS {u} \"Eaton \\\"5E\\\"\"");
                }
                r + "END LIST UPS\n"
            }
            ["LIST", "VAR", u] if upses.contains(u) => {
                let st = *status.lock().unwrap();
                if st == "STALE" {
                    "ERR DATA-STALE\n".to_owned()
                } else {
                    format!(
                        "BEGIN LIST VAR {u}\n\
                         VAR {u} device.mfr \"EATON\"\n\
                         VAR {u} device.model \"Eaton 5E 900\"\n\
                         VAR {u} ups.status \"{st}\"\n\
                         VAR {u} battery.charge \"93\"\n\
                         VAR {u} battery.runtime \"1500\"\n\
                         VAR {u} ups.load \"14\"\n\
                         VAR {u} input.voltage \"231.0\"\n\
                         VAR {u} ups.beeper.status \"enabled\"\n\
                         END LIST VAR {u}\n"
                    )
                }
            }
            ["LIST", "VAR", _] => "ERR UNKNOWN-UPS\n".to_owned(),
            ["LOGOUT"] => {
                let _ = out.write_all(b"OK Goodbye\n");
                return;
            }
            _ => "ERR UNKNOWN-COMMAND\n".to_owned(),
        };
        if out.write_all(reply.as_bytes()).is_err() {
            return;
        }
    }
}

fn source(port: u16, ups: Option<&str>) -> NutSource {
    NutSource::new(NutConfig {
        host: "127.0.0.1".into(),
        port,
        ups: ups.map(str::to_owned),
    })
}

#[test]
fn reads_the_only_ups_and_its_variables() {
    let status = Arc::new(Mutex::new("OL CHRG"));
    let (port, seen) = fake_upsd(&["eaton"], status.clone());
    let mut src = source(port, None);

    let device = src.connect().unwrap();
    assert_eq!(device.source, "nut");
    assert_eq!(device.display_name(), "Eaton 5E 900");

    let s = src.poll().unwrap();
    assert_eq!(s.status, Some(UpsStatus::OL | UpsStatus::CHRG));
    assert_eq!(s.battery_charge_pct, Some(93.0));
    assert_eq!(s.battery_runtime_s, Some(1500));
    assert_eq!(s.input_voltage_v, Some(231.0));
    assert_eq!(s.extra["ups.beeper.status"], "enabled");

    *status.lock().unwrap() = "OB DISCHRG";
    assert!(src.poll().unwrap().on_battery().unwrap());

    // A stale driver disconnects; reconnecting works once data is back.
    *status.lock().unwrap() = "STALE";
    assert!(matches!(src.poll(), Err(SourceError::Io(m)) if m.contains("stale")));
    assert_eq!(src.poll(), Err(SourceError::NotConnected));
    *status.lock().unwrap() = "OL";
    src.connect().unwrap();
    assert_eq!(src.poll().unwrap().status, Some(UpsStatus::OL));

    drop(src);
    // Read-only: nothing but LIST and LOGOUT is ever sent.
    std::thread::sleep(std::time::Duration::from_millis(100));
    for cmd in seen.lock().unwrap().iter() {
        assert!(
            cmd.starts_with("LIST ") || cmd == "LOGOUT",
            "unexpected command {cmd}"
        );
    }
}

#[test]
fn asks_for_a_name_when_upsd_serves_several_upses() {
    let (port, _) = fake_upsd(&["a", "b"], Arc::new(Mutex::new("OL")));
    let err = source(port, None).connect().unwrap_err();
    assert!(matches!(err, SourceError::Unsupported(m) if m.contains("a, b")));
    assert!(source(port, Some("b")).connect().is_ok());
    assert_eq!(
        source(port, Some("c")).connect().unwrap_err(),
        SourceError::NotFound
    );
}

#[test]
fn reports_an_unreachable_upsd() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    assert!(matches!(
        source(port, None).connect(),
        Err(SourceError::Io(_))
    ));
}
