# UPS-BMC — UPS & Battery Monitoring Center

Monitoring, outage analysis and battery-health estimation for home UPS units.
Built in Rust and Slint. The reference device is the Eaton 5E Gen2 900VA USB (5E900UD).

> **Read-only by design.** UPS-BMC never sends commands to the UPS (see
> [ADR-0001](docs/adr/0001-data-source.md)). Shutdown protection is handled by
> Windows Power Options (or by NUT on the Pi), not by this app.

## Before you start: enable shutdown protection
Control Panel → Power Options → Change plan settings → Change advanced power
settings → Battery:
- **Critical battery action:** Shut down (or Hibernate)
- **Critical battery level:** 20–30 %

This protects the PC whether or not UPS-BMC is running.

> **Don't install Eaton UPS Companion alongside UPS-BMC.** It swaps the UPS's
> Windows HID driver for its own libusb driver, and that driver stays behind
> after you uninstall Companion. See ADR-0001 for how to remove it.

## Roadmap
| Milestone | Scope | Status |
|-----------|-------|--------|
| M0 | Workspace, CI, ADRs, `hid-probe`, real-device capture | done |
| M1 = V1 | Collector + SQLite + API, Slint dashboard, tray, toasts | planned |
| M2 = V2 | Outage engine, outage history, monthly report (HTML/CSV) | planned |
| M3 = V3 | Load-normalised battery degradation estimate, guided runtime test | planned |
| M4 = V4 | NUT source, remote collector (Raspberry Pi, or NUT via Entware on a Keenetic router if it proves reliable), remote UI, ntfy/Telegram/email | planned |
| M5 | Controlled UPS settings: beeper, output voltage, thresholds ([ADR-0003](docs/adr/0003-ups-configuration-writes.md)) | proposed |

**Polling:** each feature report takes about 44 ms on the 5E (USB control
transfer plus UPS firmware). The collector reads the 5 live reports (status,
charge/runtime, load/battery voltage, input/output, boost/buck) every
**1 s**, which takes about 220 ms. Static reports (ratings, strings,
configuration) are read every 60 s.

## Workspace layout
```
crates/ups-model   source-independent domain model (NUT names on the wire, status flags, snapshots)
crates/ups-hid     defensive HID report-descriptor parser + Power Device / Battery System usage tables
apps/hid-probe     read-only discovery tool for HID UPS devices
docs/adr           architecture decision records
docs/reviews       code review records and their resolution
fixtures           curated real-device captures for tests
```
Planned: `ups-source`, `ups-store`, `ups-analysis`, `ups-api`, `apps/ups-collector`, `apps/ups-bmc`.

## Development
**Toolchain:** Rust 1.99, pinned in `rust-toolchain.toml` (rustup installs it
automatically). The MSRV is 1.88.

**Windows:** Visual Studio C++ build tools (MSVC).

**Linux / Raspberry Pi:**
```bash
sudo apt-get install -y libudev-dev pkg-config
```
To probe without root, allow access to HID devices with a udev rule. For the
Eaton vendor ID 0463:
```bash
echo 'KERNEL=="hidraw*", ATTRS{idVendor}=="0463", MODE="0660", GROUP="plugdev"' | sudo tee /etc/udev/rules.d/70-ups-bmc.rules
```
```bash
sudo udevadm control --reload-rules && sudo udevadm trigger
```
Note that hidapi's hidraw backend can't read USB string descriptors, so
`iProduct`-style strings show as unavailable on Linux.

**IDE:** RustRover with the Slint plugin. Open the folder, and RustRover picks
up the Cargo workspace and the pinned toolchain.

Checks (CI runs the same, plus an MSRV build and `cargo deny check`):
```bash
cargo fmt --all --check
```
```bash
cargo clippy --workspace --all-targets -- -D warnings
```
```bash
cargo test --workspace
```
```bash
cargo deny check
```

### Probing a UPS
Connect the UPS's USB cable to this computer, then run:
```bash
cargo run -p hid-probe -- --listen 10 --redact-serial
```
The probe lists HID power devices. For each one it decodes the report
descriptor, every feature report, and any input reports received during the
`--listen` seconds, then writes the result to `probe-output/*.json`. The file
is saved before listening starts, so pressing Ctrl-C keeps what was already
read. If the UPS doesn't appear as a power device, use `--list-all` and pass
`--vid/--pid`.
