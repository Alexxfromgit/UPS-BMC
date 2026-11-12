# ADR-0002: Headless collector plus separate UI

- Status: Accepted
- Date: 2026-11-11

## Context
Outage history (V2) and battery degradation analysis (V3) only work if
telemetry is recorded continuously, including while no window is open. V4
moves monitoring to a Raspberry Pi.

## Decision
- **`ups-collector`:** a headless daemon that polls the source, writes to
  SQLite, runs the outage state machine and records events. It exposes an
  HTTP + WebSocket API and pushes events live. On Windows it is started by
  the UI (which autostarts at sign-in if enabled) and keeps running after the
  UI exits; it does not run without a signed-in user. On the Pi it will run as
  a systemd unit (M4).
- **Notifications** are shown by the UI from the live event stream (Windows
  toasts). Remote notification channels (ntfy, Telegram, email) are planned
  in the collector for M4.
- **`ups-bmc`:** the Slint desktop UI. It talks only to the collector API,
  never to the database directly.
- **API binding:** localhost by default. Exposing it on the LAN requires a
  token (implemented in M4, see ADR-0005; so are the remote notification
  channels and the systemd unit).

## Consequences
- Moving to the Pi in V4 only changes where the collector runs. The UI is
  unchanged.
- Two processes to package and version together.
- A collector running on the protected PC stops when that PC shuts down
  mid-outage. Outage ends are then recorded as "unobserved" with time bounds.
  The Pi removes this limitation.
