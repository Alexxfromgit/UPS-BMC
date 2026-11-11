# Security policy

UPS-BMC talks to the UPS that protects your equipment and runs a local API on your PC, so
security reports are taken seriously.

## Reporting a vulnerability

Please **do not open a public issue**. Report it privately through
[GitHub's private vulnerability reporting](https://github.com/Alexxfromgit/UPS-BMC/security/advisories/new)
(Security tab → *Report a vulnerability*). Include the version, OS, UPS model, steps to reproduce
and the impact you see.

You can expect an acknowledgement within a few days. Once a fix is released, the advisory is
published with credit to the reporter unless you prefer otherwise.

## Supported versions

Only the latest release receives fixes.

## Scope

Of particular interest:

- Anything that makes UPS-BMC **write to the UPS** other than the beeper setting
  ([ADR-0003](docs/adr/0003-ups-configuration-writes.md)), for example starting a self-test,
  changing output settings or scheduling a shutdown.
- Ways to reach the collector API (`127.0.0.1:47800`) from a web page or another machine:
  bypassing the `Host`/`Origin` checks, DNS rebinding, CSRF, or anything that makes it listen
  on a non-loopback address.
- Memory-safety or denial-of-service problems when parsing data from a malicious or broken USB
  device (`crates/ups-hid`, `crates/ups-source/src/hid`).
- Anything that interferes with Windows' own UPS battery handling and shutdown protection.
- The installer and autostart entry.
