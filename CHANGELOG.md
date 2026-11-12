# Changelog

## [0.1.1](https://github.com/Alexxfromgit/UPS-BMC/compare/v0.1.0...v0.1.1) (2026-11-12)


### Bug Fixes

* build on Linux without dead-code errors in the autostart module ([04b5643](https://github.com/Alexxfromgit/UPS-BMC/commit/04b564321469e18384292fde0d03c324c8b037b6))

## 0.1.0 (2026-11-12)

First public version.

### Features

* live monitoring of USB HID UPSes on Windows (tested with the Eaton 5E 900 Gen2): charge, runtime, load, voltages, status, desktop notifications and a tray icon
* outage history with statistics, mains availability and monthly HTML/CSV reports
* battery health estimate from recorded discharges, with a guided runtime test
* UPS beeper on/off, load estimate for loads below the UPS's measuring range, autostart and a per-user installer
