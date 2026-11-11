# Fixtures

Curated real-device captures used by tests. Treat them as read-only reference data.

- `hid/`: `hid-probe` JSON dumps (descriptor, feature reports, input reports).
  **Always record with `--redact-serial`**, and check that `serial_redacted` is
  `true` in the file before committing it.
- `traces/`: recorded telemetry sequences (for example a 60-second unplug test),
  used to replay the outage engine.

Each dump records `os` and `descriptor_source`. Dumps taken on Windows contain
a descriptor that hidapi *reconstructed* from preparsed data. It is logically
equivalent to the device's own descriptor but not byte-identical. Linux
(`linux-hidraw`) dumps contain the real bytes. Keep at least one Linux capture
per device once the Raspberry Pi is set up.

Raw probe output goes to `probe-output/`, which git ignores. Copy a dump here
on purpose.
