//! NUT (Network UPS Tools) source: reads a UPS through `upsd` over the NUT
//! network protocol (RFC 9271), e.g. on a Raspberry Pi where NUT's own driver
//! owns the USB device (ADR-0005).
//!
//! Read-only: the source only sends `LIST UPS`, `LIST VAR` and `LOGOUT`. It
//! never logs in, so `upsd` doesn't count it as a `upsmon` client, and it has
//! no way to send `SET VAR` or `INSTCMD`.
//!
//! NUT already uses the variable names UPS-BMC's model is built on, so the
//! mapping is direct; unknown variables go to [`Snapshot::extra`].

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use chrono::Utc;
use tracing::{debug, info};
use ups_model::{DeviceInfo, Snapshot, UpsStatus, vars};

use crate::{SourceError, TelemetrySource};

/// `upsd`'s default port.
pub const DEFAULT_PORT: u16 = 3493;
/// Timeout for connecting and for each reply.
const TIMEOUT: Duration = Duration::from_secs(5);
/// Longest reply line accepted (NUT limits values to 256 bytes).
const MAX_LINE: usize = 4096;
/// Most variables accepted in one `LIST VAR` reply.
const MAX_VARS: usize = 1000;

/// Which `upsd` and UPS to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NutConfig {
    /// Host name or address of `upsd`.
    pub host: String,
    pub port: u16,
    /// UPS name as configured in `ups.conf`. `None` uses the only UPS `upsd`
    /// serves (and fails if there are several).
    pub ups: Option<String>,
}

impl Default for NutConfig {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: DEFAULT_PORT,
            ups: None,
        }
    }
}

impl NutConfig {
    /// Parses NUT's `[upsname@]host[:port]` notation, e.g. `eaton@localhost`,
    /// `nas.lan:3493` or `ups@[::1]:3493`.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let (ups, rest) = match s.split_once('@') {
            Some((u, r)) => (Some(u), r),
            None => (None, s),
        };
        if let Some(u) = ups
            && !is_name(u)
        {
            return Err(format!("`{u}` is not a valid NUT UPS name"));
        }
        let (host, port) = if let Some(v6) = rest.strip_prefix('[') {
            let (h, after) = v6
                .split_once(']')
                .ok_or_else(|| format!("missing `]` in `{rest}`"))?;
            match after.strip_prefix(':') {
                Some(p) => (h, Some(p)),
                None if after.is_empty() => (h, None),
                None => return Err(format!("unexpected `{after}` after `]`")),
            }
        } else {
            match rest.rsplit_once(':') {
                Some((h, p)) if !h.contains(':') => (h, Some(p)),
                Some(_) => return Err("put IPv6 addresses in brackets: `[::1]:3493`".into()),
                None => (rest, None),
            }
        };
        if host.is_empty() {
            return Err("the NUT host is missing".into());
        }
        let port = match port {
            Some(p) => p.parse().map_err(|_| format!("`{p}` is not a port"))?,
            None => DEFAULT_PORT,
        };
        Ok(Self {
            host: host.to_owned(),
            port,
            ups: ups.map(str::to_owned),
        })
    }
}

impl std::fmt::Display for NutConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(u) = &self.ups {
            write!(f, "{u}@")?;
        }
        if self.host.contains(':') {
            write!(f, "[{}]:{}", self.host, self.port)
        } else {
            write!(f, "{}:{}", self.host, self.port)
        }
    }
}

/// UPS names are sent unquoted, so only plain tokens are allowed.
fn is_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// A UPS read through `upsd`.
#[derive(Debug)]
pub struct NutSource {
    config: NutConfig,
    conn: Option<Conn>,
}

#[derive(Debug)]
struct Conn {
    reader: BufReader<TcpStream>,
    ups: String,
}

impl NutSource {
    #[must_use]
    pub fn new(config: NutConfig) -> Self {
        Self { config, conn: None }
    }
}

fn io(e: &std::io::Error) -> SourceError {
    SourceError::Io(e.to_string())
}

impl Conn {
    fn open(config: &NutConfig) -> Result<Self, SourceError> {
        let addrs: Vec<SocketAddr> = (config.host.as_str(), config.port)
            .to_socket_addrs()
            .map_err(|e| SourceError::Io(format!("resolving {}: {e}", config.host)))?
            .collect();
        let mut last = SourceError::NotFound;
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, TIMEOUT) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(TIMEOUT)).map_err(|e| io(&e))?;
                    stream
                        .set_write_timeout(Some(TIMEOUT))
                        .map_err(|e| io(&e))?;
                    let _ = stream.set_nodelay(true);
                    return Ok(Self {
                        reader: BufReader::new(stream),
                        ups: String::new(),
                    });
                }
                Err(e) => last = SourceError::Io(format!("connecting to upsd at {addr}: {e}")),
            }
        }
        Err(last)
    }

    fn send(&mut self, command: &str) -> Result<(), SourceError> {
        let stream = self.reader.get_mut();
        stream
            .write_all(format!("{command}\n").as_bytes())
            .and_then(|()| stream.flush())
            .map_err(|e| io(&e))
    }

    fn line(&mut self) -> Result<String, SourceError> {
        let mut buf = Vec::new();
        let n = (&mut self.reader)
            .take(MAX_LINE as u64)
            .read_until(b'\n', &mut buf)
            .map_err(|e| io(&e))?;
        if n == 0 {
            return Err(SourceError::Io("upsd closed the connection".into()));
        }
        if buf.last() != Some(&b'\n') {
            return Err(SourceError::Io("reply line from upsd too long".into()));
        }
        let line = String::from_utf8_lossy(&buf);
        Ok(line.trim_end_matches(['\r', '\n']).to_owned())
    }

    /// Sends `LIST <what>` and returns the lines between `BEGIN` and `END`,
    /// split into words.
    fn list(&mut self, what: &str) -> Result<Vec<Vec<String>>, SourceError> {
        self.send(&format!("LIST {what}"))?;
        let first = self.line()?;
        if first != format!("BEGIN LIST {what}") {
            return Err(reply_error(&first));
        }
        let end = format!("END LIST {what}");
        let mut items = Vec::new();
        loop {
            let line = self.line()?;
            if line == end {
                return Ok(items);
            }
            if items.len() >= MAX_VARS {
                return Err(SourceError::Io(
                    "too many lines in a reply from upsd".into(),
                ));
            }
            items.push(split(&line).map_err(SourceError::Io)?);
        }
    }

    fn vars(&mut self) -> Result<Vec<(String, String)>, SourceError> {
        let what = format!("VAR {}", self.ups);
        let lines = self.list(&what)?;
        Ok(lines
            .into_iter()
            .filter_map(|mut w| {
                // VAR <ups> <name> "<value>"
                (w.len() == 4 && w[0] == "VAR").then(|| {
                    let value = w.pop().unwrap_or_default();
                    let name = w.pop().unwrap_or_default();
                    (name, value)
                })
            })
            .collect())
    }
}

/// Turns an unexpected reply (normally `ERR <code>`) into an error.
fn reply_error(line: &str) -> SourceError {
    match line.strip_prefix("ERR ") {
        Some(code) => {
            let code = code.split_whitespace().next().unwrap_or(code);
            match code {
                "UNKNOWN-UPS" => SourceError::NotFound,
                "DATA-STALE" => SourceError::Io("upsd: the UPS data is stale".into()),
                "DRIVER-NOT-CONNECTED" => {
                    SourceError::Io("upsd: the NUT driver is not running".into())
                }
                "ACCESS-DENIED" => SourceError::Io("upsd: access denied".into()),
                other => SourceError::Io(format!("upsd: {other}")),
            }
        }
        None => SourceError::Io(format!("unexpected reply from upsd: {line:.80}")),
    }
}

/// Splits a reply line into words; `"quoted strings"` may contain spaces and
/// `\"` / `\\` escapes.
fn split(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut chars = line.chars().peekable();
    loop {
        while chars.next_if(|c| *c == ' ').is_some() {}
        let Some(&c) = chars.peek() else {
            return Ok(words);
        };
        let mut word = String::new();
        if c == '"' {
            chars.next();
            loop {
                match chars.next() {
                    Some('\\') => match chars.next() {
                        Some(e) => word.push(e),
                        None => return Err("unterminated escape in reply from upsd".into()),
                    },
                    Some('"') => break,
                    Some(c) => word.push(c),
                    None => return Err("unterminated quote in reply from upsd".into()),
                }
            }
        } else {
            while let Some(c) = chars.next_if(|c| *c != ' ') {
                word.push(c);
            }
        }
        words.push(word);
    }
}

fn number(v: &str) -> Option<f64> {
    v.trim().parse::<f64>().ok().filter(|x| x.is_finite())
}

#[allow(clippy::cast_possible_truncation)]
fn f32_of(v: &str) -> Option<f32> {
    number(v).map(|x| x as f32)
}

/// Converts `upsd`'s variables into a snapshot.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn snapshot_from_vars(vars_list: &[(String, String)]) -> Snapshot {
    let mut s = Snapshot::empty(Utc::now());
    for (name, value) in vars_list {
        match name.as_str() {
            vars::UPS_STATUS => {
                let (flags, unknown) = UpsStatus::from_nut(value);
                if !unknown.is_empty() {
                    debug!(?unknown, "unknown NUT status tokens");
                }
                // "WAIT" alone (driver starting) is no status at all.
                if !flags.is_empty() {
                    s.status = Some(flags);
                }
            }
            vars::BATTERY_CHARGE => s.battery_charge_pct = f32_of(value),
            vars::BATTERY_RUNTIME => {
                s.battery_runtime_s = number(value)
                    .filter(|x| *x >= 0.0 && *x < f64::from(u32::MAX))
                    .map(|x| x.round() as u32);
            }
            vars::BATTERY_VOLTAGE => s.battery_voltage_v = f32_of(value),
            vars::UPS_LOAD => s.load_pct = f32_of(value),
            vars::INPUT_VOLTAGE => s.input_voltage_v = f32_of(value),
            vars::INPUT_FREQUENCY => s.input_frequency_hz = f32_of(value),
            vars::OUTPUT_VOLTAGE => s.output_voltage_v = f32_of(value),
            vars::OUTPUT_FREQUENCY => s.output_frequency_hz = f32_of(value),
            "ups.realpower" => s.realpower_w = f32_of(value),
            "ups.power" => s.power_va = f32_of(value),
            vars::UPS_REALPOWER_NOMINAL => s.realpower_nominal_w = f32_of(value),
            vars::UPS_POWER_NOMINAL => s.power_nominal_va = f32_of(value),
            _ => {
                s.extra.insert(name.clone(), value.clone());
            }
        }
    }
    s
}

/// The UPS identity from its variables (`device.*` preferred over `ups.*`).
#[must_use]
pub fn device_from_vars(vars_list: &[(String, String)]) -> DeviceInfo {
    let get = |names: &[&str]| {
        names.iter().find_map(|n| {
            vars_list
                .iter()
                .find(|(k, v)| k == n && !v.trim().is_empty())
                .map(|(_, v)| v.trim().to_owned())
        })
    };
    let hex = |names: &[&str]| get(names).and_then(|v| u16::from_str_radix(&v, 16).ok());
    DeviceInfo {
        source: "nut".into(),
        manufacturer: get(&["device.mfr", vars::UPS_MFR]),
        model: get(&["device.model", vars::UPS_MODEL]),
        serial: get(&["device.serial", vars::UPS_SERIAL]),
        battery_type: get(&[vars::BATTERY_TYPE]),
        vendor_id: hex(&["ups.vendorid"]),
        product_id: hex(&["ups.productid"]),
    }
}

impl TelemetrySource for NutSource {
    fn kind(&self) -> &'static str {
        "nut"
    }

    fn connect(&mut self) -> Result<DeviceInfo, SourceError> {
        self.conn = None;
        let mut conn = Conn::open(&self.config)?;
        conn.ups = if let Some(u) = &self.config.ups {
            u.clone()
        } else {
            let names: Vec<String> = conn
                .list("UPS")?
                .into_iter()
                .filter(|w| w.len() >= 2 && w[0] == "UPS")
                .map(|mut w| w.swap_remove(1))
                .collect();
            match names.as_slice() {
                [] => return Err(SourceError::NotFound),
                [one] => one.clone(),
                many => {
                    return Err(SourceError::Unsupported(format!(
                        "upsd serves several UPSes ({}); choose one with `name@{}`",
                        many.join(", "),
                        self.config.host
                    )));
                }
            }
        };
        if !is_name(&conn.ups) {
            return Err(SourceError::Unsupported(format!(
                "unsupported UPS name `{}`",
                conn.ups
            )));
        }
        let vars_list = conn.vars()?;
        let device = device_from_vars(&vars_list);
        info!(ups = %conn.ups, upsd = %self.config, "connected to NUT");
        self.conn = Some(conn);
        Ok(device)
    }

    fn poll(&mut self) -> Result<Snapshot, SourceError> {
        let conn = self.conn.as_mut().ok_or(SourceError::NotConnected)?;
        match conn.vars() {
            Ok(v) => Ok(snapshot_from_vars(&v)),
            Err(e) => {
                self.conn = None;
                Err(e)
            }
        }
    }
}

impl Drop for NutSource {
    fn drop(&mut self) {
        if let Some(conn) = &mut self.conn {
            let _ = conn.send("LOGOUT");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nut_addresses() {
        assert_eq!(NutConfig::parse("localhost").unwrap(), NutConfig::default());
        assert_eq!(
            NutConfig::parse("eaton@pi.lan:3500").unwrap(),
            NutConfig {
                host: "pi.lan".into(),
                port: 3500,
                ups: Some("eaton".into())
            }
        );
        let v6 = NutConfig::parse("ups@[::1]:3493").unwrap();
        assert_eq!((v6.host.as_str(), v6.port), ("::1", 3493));
        assert_eq!(v6.to_string(), "ups@[::1]:3493");
        assert!(NutConfig::parse("::1").is_err());
        assert!(NutConfig::parse("bad name@host").is_err());
        assert!(NutConfig::parse("ups@").is_err());
        assert!(NutConfig::parse("host:http").is_err());
    }

    #[test]
    fn splits_quoted_words() {
        assert_eq!(
            split(r#"VAR eaton ups.mfr "EATON \"5E\" \\ x""#).unwrap(),
            vec!["VAR", "eaton", "ups.mfr", r#"EATON "5E" \ x"#]
        );
        assert_eq!(split(r#"VAR u x """#).unwrap(), vec!["VAR", "u", "x", ""]);
        assert!(split(r#"VAR u x "open"#).is_err());
    }

    fn v(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect()
    }

    #[test]
    fn maps_variables() {
        let s = snapshot_from_vars(&v(&[
            ("ups.status", "OB DISCHRG"),
            ("battery.charge", "87"),
            ("battery.runtime", "1830.4"),
            ("ups.load", "12"),
            ("input.voltage", "0.0"),
            ("ups.power.nominal", "900"),
            ("ups.beeper.status", "enabled"),
            ("battery.voltage", "n/a"),
        ]));
        assert_eq!(s.status, Some(UpsStatus::OB | UpsStatus::DISCHRG));
        assert_eq!(s.battery_charge_pct, Some(87.0));
        assert_eq!(s.battery_runtime_s, Some(1830));
        assert_eq!(s.load_pct, Some(12.0));
        assert_eq!(s.input_voltage_v, Some(0.0));
        assert_eq!(s.power_nominal_va, Some(900.0));
        assert_eq!(s.battery_voltage_v, None);
        assert_eq!(s.extra["ups.beeper.status"], "enabled");
        assert_eq!(
            snapshot_from_vars(&v(&[("ups.status", "WAIT")])).status,
            None
        );
    }

    #[test]
    fn reads_the_device_identity() {
        let d = device_from_vars(&v(&[
            ("device.mfr", "EATON"),
            ("ups.mfr", "ignored"),
            ("ups.model", "Eaton 5E 900"),
            ("ups.vendorid", "0463"),
            ("ups.productid", "ffff"),
            ("device.serial", " "),
        ]));
        assert_eq!(d.source, "nut");
        assert_eq!(d.manufacturer.as_deref(), Some("EATON"));
        assert_eq!(d.model.as_deref(), Some("Eaton 5E 900"));
        assert_eq!(d.serial, None);
        assert_eq!((d.vendor_id, d.product_id), (Some(0x0463), Some(0xffff)));
    }

    #[test]
    fn maps_error_replies() {
        assert_eq!(reply_error("ERR UNKNOWN-UPS"), SourceError::NotFound);
        assert!(matches!(reply_error("ERR DATA-STALE"), SourceError::Io(m) if m.contains("stale")));
        assert!(matches!(reply_error("garbage"), SourceError::Io(_)));
    }
}
