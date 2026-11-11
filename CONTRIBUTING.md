# Contributing to UPS-BMC

Thanks for helping! Bug reports, ideas, captures from other UPS models and pull requests are all
welcome.

## Getting started

You need Rust (`rust-toolchain.toml` pins the version, so rustup installs it automatically).

- **Windows:** the MSVC toolchain (Visual Studio Build Tools with *Desktop development with C++*).
  The installer additionally needs [Inno Setup 6](https://jrsoftware.org/isinfo.php).
- **Linux:** for example `sudo apt-get install -y libudev-dev pkg-config libfontconfig1-dev libxkbcommon-dev`.

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
cargo run -p ups-bmc -- --sim          # the UI with a simulated UPS, no hardware needed
.\installer\build.ps1                  # release build + installer (Windows)
```

See the [README](README.md) for the architecture and layout, and `docs/adr` for the decisions
behind it.

## Pull requests

- Keep each PR to one change and describe what it does and how you tested it.
- Title the PR as a [Conventional Commit](https://www.conventionalcommits.org): PRs are
  squash-merged, and the title becomes the commit that drives versioning and release notes.
  - `feat: show the battery temperature` → new feature, minor release
  - `fix: keep the outage list after a reconnect`, `perf: …`, `deps: …` → patch release
  - `feat!: …`, or a `BREAKING CHANGE:` line in the description → breaking change
  - `docs:`, `refactor:`, `test:`, `build:`, `ci:`, `chore:` → no release on their own, not in the notes

  Write the subject for users: it shows up as-is in the changelog.
- Add or update tests. The analysis code (`crates/ups-analysis`) is pure and fully unit-tested;
  `apps/ups-collector/tests/e2e_sim.rs` runs the real collector against the simulator.
- CI must stay green: `cargo fmt --check`, clippy (pedantic, warnings are errors), tests on Windows
  and Linux, rustdoc, the MSRV check and `cargo deny check`.
- Database changes go in a new migration at the end of `MIGRATIONS` (`crates/ups-store/src/lib.rs`);
  never edit an existing one.

## Safety rules for device code

UPS-BMC runs next to the UPS that protects real equipment, so:

- **Read-only by default.** Devices are opened only through `ReadOnlyHid`; `clippy.toml` denies
  every other way to open a device or write to it. The only write is the beeper setting
  ([ADR-0003](docs/adr/0003-ups-configuration-writes.md)). New writable settings need an ADR and
  a reviewed allowlist entry, and must be verified by reading them back.
- **Never replace the OS driver.** No libusb/WinUSB/Zadig: Windows' own HID UPS driver keeps
  handling the battery and the critical-battery shutdown. `deny.toml` bans other USB crates.
- **Loopback API only.** The collector API has no authentication; it must keep refusing
  non-loopback binds, foreign `Host` headers and requests with an `Origin` header.

## Captures from other UPS models

`hid-probe` reads a UPS's HID descriptor and reports **without writing anything** and saves them as
JSON. Run it with `--redact-serial` for anything you share: that removes the serial number and the
device instance path. Captures of other models are very useful for tests and mappings: attach one
to an issue, and check it for anything you don't want to share first.

## Reporting bugs

Use the issue templates. For security problems, follow [SECURITY.md](SECURITY.md) instead of
opening a public issue.
