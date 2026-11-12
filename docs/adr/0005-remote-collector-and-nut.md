# ADR-0005: NUT source, remote collector and notification channels (M4)

- Status: Accepted
- Date: 2026-11-12

## Context
A collector on the PC it protects stops when that PC shuts down mid-outage (ADR-0002), so outage
ends go unobserved and nothing reports the outage while the PC is off. M4 moves the collector to
an always-on Raspberry Pi. There, NUT is the well-supported way to read a USB UPS: its driver
owns the device, and `upsmon` shuts the Pi down safely. The desktop UI must reach that collector
over the LAN, and notifications must not depend on a desktop being on.

## Decision

### NUT source (`ups-source::nut`)
- A small synchronous client of the NUT network protocol (RFC 9271) over `std::net::TcpStream`,
  driven by the existing poller thread like the other sources. The async `rups` crate would add
  a runtime to the poller for four commands.
- **Read-only:** it only sends `LIST UPS`, `LIST VAR <ups>` and `LOGOUT`. It never sends
  `USERNAME`/`PASSWORD`/`LOGIN`, so upsd doesn't count it as a `upsmon` client and it has no way
  to run `SET VAR` or `INSTCMD`. A fake-upsd test checks that nothing else is sent.
- NUT names are already the app's vocabulary, so values map directly. Unknown variables go to
  `Snapshot::extra`. `ERR DATA-STALE` and `DRIVER-NOT-CONNECTED` disconnect the source, and the
  poller reconnects as it does for HID.
- Address: NUT's own `[upsname@]host[:port]`. Without a name, the only UPS served is used.
- **The HID source is behind the `hid` cargo feature** (on by default). The Pi build uses
  `--no-default-features`, so it needs neither hidapi nor libudev. There, NUT's driver owns the
  device anyway.

### Remote access to the API
- The collector may bind to a non-loopback address only with an **API token** (`--token-file`
  or `UPS_BMC_TOKEN`, at least 16 characters; `--generate-token` prints 32 random bytes as hex).
  Without one it refuses to start.
- A request is allowed when it has no `Origin` header (unchanged: no web pages), and either
  - comes from a loopback peer with a loopback `Host` (unchanged: local UI, DNS-rebinding
    protection), or
  - carries `Authorization: Bearer <token>`, compared in constant time.
- `POST /api/v1/shutdown` is accepted from loopback peers only.
- **Plain HTTP:** the token protects against other LAN devices, not against someone who can
  watch LAN traffic. Reaching the Pi from outside the LAN is out of scope; the guide says to use
  a VPN rather than forward the port. TLS can be added later without changing the API.
- The UI reads the collector address and token from `--collector`/`--token-file`,
  `UPS_BMC_TOKEN`, or `ui.toml` in the user's config directory. The file is what makes autostart
  work, since autostart passes only `--minimized`. The UI never auto-starts a collector for a
  non-loopback address.

### Notification channels in the collector
- **ntfy** (HTTP POST with Title/Priority/Tags headers), **Telegram** (Bot API `sendMessage`) and
  **email** (SMTP via `lettre`, STARTTLS by default) are configured in an optional TOML file
  (`--config`). Unknown keys are errors.
- They fire on the same events as the desktop notifications by default; `notify.events`
  overrides that. The titles come from one shared function (`ups_api::notification_title`).
- Delivery runs on the blocking pool with retries after 10 s, 60 s and 5 min, because the network
  may be down during the very outage being reported. Bodies carry the event time, since retries
  can deliver late or out of order.
- HTTPS uses rustls with the `ring` provider and the webpki root store. That avoids system
  OpenSSL and aws-lc's build tools on Windows and arm64.
- Secrets are never logged: they are wrapped in a `Secret` type with a redacting `Debug`, and the
  bot token is removed from Telegram error texts. `--test-notify` sends one test message per
  channel.
- Desktop notifications stay in the UI (ADR-0002), so a PC running the UI next to a Pi collector
  gets both.

### Deployment
- `deploy/raspberry-pi/`: a hardened systemd unit (dedicated user, `ProtectSystem=strict`, state
  and log directories) and an example configuration. `docs/raspberry-pi.md` covers NUT setup,
  installation, the token and the UI.
- On Unix the collector also stops gracefully on SIGTERM, which is how systemd stops services.
- CI builds, lints and tests the NUT-only collector natively on `ubuntu-22.04-arm`. Releases add
  `UPS-BMC-<version>-linux-arm64-collector.tar.gz`, built there too, so it needs glibc 2.35 or
  newer (Raspberry Pi OS Bookworm has 2.36).

## Consequences
- Outages are recorded end to end while the PC is off, and notifications reach a phone without
  any desktop running.
- With the USB cable on the Pi, Windows' own UPS shutdown no longer applies. The PC needs a NUT
  client (e.g. WinNUT-Client as a `secondary` of the Pi's upsd). UPS-BMC still shuts nothing
  down; it only documents this.
- Settings writes (the beeper, ADR-0003) aren't available through NUT. Mapping them to `upsrw`
  remains M5.
- A history recorded on the PC stays there; the Pi starts its own database. Merging databases is
  not planned.
