# ADR-0002: Headless collector plus separate UI

- Status: Accepted
- Date: 2026-11-11

## Context
Outage history (V2) and battery degradation analysis (V3) only work if
telemetry is recorded continuously, including while no window is open. V4
moves monitoring to a Raspberry Pi.

## Decision
- **`ups-collector`:** a headless daemon that polls the source, writes to
  SQLite, runs the outage state machine and sends notifications. It exposes an
  HTTP + WebSocket API. It runs as a Windows service or autostart task on
  Windows, and as a systemd unit on the Pi.
- **`ups-bmc`:** the Slint desktop UI. It talks only to the collector API,
  never to the database directly.
- **API binding:** localhost by default. Exposing it on the LAN requires a
  token.

## Consequences
- Moving to the Pi in V4 only changes where the collector runs. The UI is
  unchanged.
- Two processes to package and version together.
- A collector running on the protected PC stops when that PC shuts down
  mid-outage. Outage ends are then recorded as "unobserved" with time bounds.
  The Pi removes this limitation.
