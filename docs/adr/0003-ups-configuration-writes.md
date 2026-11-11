# ADR-0003: Changing UPS settings from UPS-BMC (planned)

- Status: Proposed. This doesn't lift ADR-0001's read-only rule yet.
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
