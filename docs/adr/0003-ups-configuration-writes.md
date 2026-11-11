# ADR-0003: Changing UPS settings from UPS-BMC

- Status: **Accepted for the beeper only** (2026-11-11). Every other setting is still proposed.
- Date: 2026-11-11

## Context
The user wants to adjust UPS settings from the app later, for example when the
UPS powers a router instead of the PC. The real 5E 900 G2 descriptor
(`fixtures/hid/eaton-5e900ud-g2-fw01.02.0009-windows.json`) shows which
values the device declares writable, meaning non-Constant feature fields.

| Report | Field | Current | Effect / risk |
|---|---|---|---|
| 0x14 | `UPS.PowerSummary.AudibleAlarmControl` | 1 | Beeper mode (HID: 1 = disabled, 2 = enabled, 3 = muted). Low risk. |
| 0x13 | `UPS.Flow.ConfigVoltage` | 230 | Output voltage on battery (e.g. 220/230/240 V). Medium risk. |
| 0x09 | `UPS.PowerSummary.DelayBeforeShutdown` | −1 | **Switches the UPS output off** after N seconds, which cuts power to the load (router/PC). High risk. |
| 0x0A | `UPS.PowerSummary.DelayBeforeStartup` | −1 | Restarts the output after N seconds. High risk. |

Some settings that Eaton's own software changes aren't standard writable
fields on this model. The low-battery threshold (`RemainingCapacityLimit`,
0x08) is declared Constant. The value Companion shows probably lives in the
vendor field `0xFFFF:0x004D` (report 0x12, currently 5). That has to be
confirmed against NUT's `mge-hid` mapping before anything is written there.

Windows' "critical battery level" (2 % here) is a **Windows** power setting,
not a UPS setting. When the UPS only powers a router, it doesn't matter.

## Proposed decision (to be accepted before implementation)
- A separate `ups-control` module is the only code allowed to call
  `send_feature_report`, through a reviewed exception in `clippy.toml`.
- **Allowlist only:** writes are restricted to an explicit table of (report,
  usage, valid values) per device model. Nothing generic.
- **Every write:** check that the value is in range, ask for confirmation in the
  UI (with an extra confirmation for output-affecting fields), write, read back
  and verify, and record the change in the event log.
- `DelayBeforeShutdown`/`DelayBeforeStartup` aren't exposed at all until a
  separate ADR covers a "UPS-initiated shutdown" use case.
- Through NUT (router or Pi setups), the same allowlist maps to `upsrw` /
  `instcmd`.

Planned as milestone M5, after monitoring (M1–M4) is stable.

## Implementation (beeper, M1)
The user asked for beeper on/off. It is harmless: it only controls the audible alarm.
- `crates/ups-source/src/hid/control.rs` is the only code that writes. It checks
  that the setting is on the allowlist (`Setting::Beeper`), that the field is
  writable, and that the value is valid (1 = disabled, 2 = enabled). It reads
  the report fresh, changes only the field's bits (`ups_hid::Field::insert`),
  sends it, and **reads it back to verify**.
- `ReadOnlyHid::send_feature_report` is `pub(super)`, carries a reviewed
  `#[allow(clippy::disallowed_methods)]`, and is not reachable from
  `hid-probe` or other crates.
- The collector runs the write on the poller thread (single device owner)
  through `POST /api/v1/settings/beeper` and records a `setting_changed` event.
- The endpoint refuses requests that carry an `Origin` header and requires a
  JSON body, so web pages can't change UPS settings (CSRF) through `127.0.0.1`.
- UI: Settings → UPS beeper switch, available only while a supporting UPS is
  connected.

### Deviations from the proposed decision (deliberate, for the beeper only)
- **No confirmation dialog:** the beeper only affects the audible alarm, the
  change is verified by reading it back, logged, and undone with one click.
  Output-affecting settings (M5) will still need confirmation.
- **"Per model" is enforced by structure, not a VID/PID table:** the write is
  allowed only when the beeper field is a writable Feature field *and the
  only writable field in its report* (`control::supports`). Re-sending other
  writable fields could start a battery test (`Test`) or re-arm a shutdown
  (`DelayBeforeShutdown`) on some UPSes, so such devices are refused (review
  M1-3; covered by a test with a synthetic descriptor). On the Eaton 5E,
  report 0x14 holds only `AudibleAlarmControl`. M5 settings that share
  reports with other fields need the explicit per-model table.
- If the API call times out, the queued write is dropped instead of being
  applied later.
