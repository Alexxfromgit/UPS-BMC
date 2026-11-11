# ADR-0001: How UPS-BMC reads the UPS

- Status: Accepted
- Date: 2026-11-11

## Context
The Eaton 5E Gen2 900VA USB (5E900UD) is a USB HID Power Device. It uses usage
pages 0x84 (Power Device) and 0x85 (Battery System). Windows binds its own HID
UPS battery driver to it, and that driver is what lets Power Options shut the PC
down when the battery runs low.

NUT's `usbhid-ups` driver reaches the device through libusb. On Windows that
means swapping the device driver for WinUSB (for example with Zadig), which
takes the UPS away from Windows' own battery and shutdown handling. As of
2026-11, NUT's Windows builds still use libusb. Native Windows HID support is
being worked on upstream but is not in a stable release (see
<https://github.com/networkupstools/nut/wiki/NUT-for-Windows>). Re-check this
before M4.

## Decision
1. **Windows (V1–V3):** read the device through the OS HID stack with `hidapi`,
   using `GET_FEATURE` and input reports. Windows keeps owning the device and
   keeps its shutdown protection. If that turns out not to work, fall back to
   the Windows battery API (`IOCTL_BATTERY_*`), which gives less data but
   carries no risk.
2. **Raspberry Pi (V4):** run NUT `upsd` with `usbhid-ups` and read it over the
   NUT network protocol (`rups` crate).
3. **One vocabulary:** every source maps into `ups-model`, which uses NUT
   variable names (`battery.charge`, `ups.status`, ...) on the wire.
4. **Strictly read-only:** no `SET_FEATURE` and no output reports, which rules
   out beeper changes, self-tests and shutdown commands. Changing this needs a
   new ADR.

## How the read-only rule is enforced
The guarantee lives in the code. The OS doesn't provide it: on Windows, hidapi
opens the device handle for read *and* write.

- **Single entry point:** a HID device can only be opened through `ReadOnlyHid`
  (currently `apps/hid-probe/src/readonly.rs`; it moves to `ups-source` in M1).
  That wrapper exposes read methods only.
- **Clippy:** `clippy.toml` denies `hidapi::HidApi::open*` everywhere else, as
  well as every hidapi write method. CI runs clippy with `-D warnings`.
- **cargo-deny:** `deny.toml` bans other USB/HID crates (`rusb`, `nusb`, ...)
  that could bypass the guard.
- **No FFI:** `unsafe_code = "forbid"` rules out raw FFI calls.
- **Limit:** `cargo build` alone doesn't run clippy. CI and code review are the
  gate.

## Consequences
- No driver changes on Windows. The app and Windows' own shutdown work side by
  side.
- We have to interpret Eaton's HID reports ourselves (`ups-hid`). `hid-probe`
  dumps and the fixtures recorded from them are the reference for that mapping.
- **Descriptor provenance:** on Windows, hidapi doesn't return the device's
  report descriptor bytes. It rebuilds an equivalent descriptor from the HIDP
  preparsed data of one top-level collection, so the field layout matches but
  the bytes don't. Linux (hidraw) returns the real descriptor. Every dump
  records this in `descriptor_source`. Capture once on Linux before relying on
  byte-level details.
## M0 results (2026-11-11, 5E 900 G2, firmware 01.02.0009, Windows 11)
- **Confirmed:** `hidapi` reads every `GET_FEATURE` report while Windows' own
  drivers (HidUsb plus "HID UPS Battery") own the device. No driver changes are
  needed. Report lengths match the descriptor exactly.
- **Input reports are unavailable** on Windows (`ReadFile` fails with
  "Incorrect function"). The collector therefore **polls feature reports**,
  which is also what NUT does. Status flags are in feature report 0x01 and
  charge/runtime in 0x06, so nothing is lost.
- **Available values:** status flags (AC present, charging, discharging, low
  battery, replace battery, overload, shutdown imminent, boost/buck), charge %,
  runtime (s), load %, battery voltage (13.6 V, exponent −1), input and output
  voltage, output frequency, output active and apparent power, nominal 900 VA /
  480 W, chemistry (PbAc). The reference capture is
  `fixtures/hid/eaton-5e900ud-g2-fw01.02.0009-windows.json`, guarded by
  `crates/ups-hid/tests/eaton_5e_fixture.rs`.
- **Parser lesson:** Eaton declares most read-only values as *Constant* main
  items (`0x83`). Only Constant items *without* usages are padding.
- **Eaton UPS Companion conflicts with this design.** Its installer replaces
  the Windows HID driver with Eaton's libusb0 driver (`etn_libusb0.inf`). The
  UPS then disappears from the HID stack and from Windows' battery handling.
  Uninstalling Companion leaves that driver in place. Remove it from an
  administrator shell with
  `pnputil /delete-driver <oemNN.inf> /uninstall /force`, then run
  `pnputil /scan-devices`. Companion also exposes an unauthenticated read/write
  JSON service on port 4679 on all interfaces, which is another reason not to
  depend on it.
