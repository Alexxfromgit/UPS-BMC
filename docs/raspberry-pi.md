# Running the collector on a Raspberry Pi

Run the UPS-BMC collector on a Raspberry Pi that stays on, next to NUT, and it records every outage
from start to end, even while your PC is off or shut down by the UPS. The desktop UI on your PC
connects to it over the LAN. The collector sends ntfy, Telegram or email notifications itself.
The design is described in [ADR-0005](adr/0005-remote-collector-and-nut.md).

```
UPS ──USB──> Raspberry Pi: NUT driver + upsd ──> ups-collector ──LAN (token)──> ups-bmc on the PC
                                │                      └──> ntfy / Telegram / email
                                └── upsmon: shuts the Pi down on low battery
```

Tested setup: Raspberry Pi OS (64-bit) Bookworm, Eaton 5E 900 G2 with NUT's `usbhid-ups` driver.
Any UPS that NUT supports works.

> **Moving the USB cable to the Pi removes Windows' own UPS protection.** Windows only shuts the
> PC down on a low battery while the UPS is plugged into the PC. Once the UPS is on the Pi, the
> PC needs a NUT client instead (step 6). The UPS-BMC collector never shuts anything down.

## 1. Install NUT and connect the UPS

Plug the UPS's USB cable into the Pi, then:
```bash
sudo apt-get update && sudo apt-get install -y nut-server nut-client
```
`/etc/nut/ups.conf`: add your UPS (the name in brackets is used everywhere below):
```ini
[eaton]
    driver = usbhid-ups
    port = auto
    vendorid = 0463
    desc = "Eaton 5E 900"
```
`/etc/nut/nut.conf`:
```ini
MODE=standalone
```
`/etc/nut/upsd.users`: a user for the Pi's own `upsmon`:
```ini
[upsmon]
    password = choose-a-password
    upsmon primary
```
`/etc/nut/upsmon.conf`: shut the Pi down cleanly when the battery runs low:
```ini
MONITOR eaton@localhost 1 upsmon choose-a-password primary
SHUTDOWNCMD "/sbin/shutdown -h +0"
```
Restart NUT and check that it reads the UPS:
```bash
sudo systemctl restart nut-server nut-monitor
```
```bash
upsc eaton@localhost
```
`upsc` should list `ups.status: OL`, `battery.charge` and so on. If it doesn't, check
`journalctl -u nut-server -u 'nut-driver*'`.

Also set the Pi's time zone, because monthly reports use the collector's local time:
```bash
sudo timedatectl set-timezone Europe/Berlin
```

## 2. Install the collector

Download `UPS-BMC-<version>-linux-arm64-collector.tar.gz` from the
[Releases](https://github.com/Alexxfromgit/UPS-BMC/releases/latest) page onto the Pi (64-bit
Raspberry Pi OS; Bookworm or newer), and unpack it:
```bash
tar xzf UPS-BMC-*-linux-arm64-collector.tar.gz && cd UPS-BMC-*-linux-arm64-collector
```
Or build it yourself, on the Pi or any arm64 Linux. The `--no-default-features` build reads NUT
only, so it needs neither hidapi nor libudev:
```bash
cargo build --release -p ups-collector --no-default-features
```
Install the binary, a service user and the configuration directory:
```bash
sudo install -m 0755 ups-collector /usr/local/bin/ups-collector
```
```bash
sudo useradd --system --no-create-home --shell /usr/sbin/nologin ups-bmc
```
```bash
sudo install -d -m 0750 -o root -g ups-bmc /etc/ups-bmc
```

## 3. Create the API token

The collector serves the LAN only with a token, which every client must send:
```bash
ups-collector --generate-token | sudo install -m 0640 -o root -g ups-bmc /dev/stdin /etc/ups-bmc/token
```
```bash
sudo cat /etc/ups-bmc/token
```
Keep the printed token for the desktop UI (step 5).

The API is plain HTTP. The token keeps other devices on your LAN out, but anyone who can watch
your LAN traffic could read it. Don't forward port 47800 on your router; to reach the Pi from
outside, use a VPN such as WireGuard or Tailscale.

## 4. Notifications and the service

```bash
sudo install -m 0640 -o root -g ups-bmc collector.toml.example /etc/ups-bmc/collector.toml
```
Edit `/etc/ups-bmc/collector.toml` and uncomment the channels you want (ntfy, Telegram, email).
Then send a test message to each of them:
```bash
sudo -u ups-bmc /usr/local/bin/ups-collector --config /etc/ups-bmc/collector.toml --test-notify
```
Install and start the service:
```bash
sudo install -m 0644 ups-collector.service /etc/systemd/system/ups-collector.service
```
```bash
sudo systemctl daemon-reload && sudo systemctl enable --now ups-collector
```
```bash
systemctl status ups-collector
```
The database is in `/var/lib/ups-bmc`, logs are in `/var/log/ups-bmc` and in
`journalctl -u ups-collector`. To check the API from the Pi itself (local requests need no token):
```bash
curl -s http://127.0.0.1:47800/api/v1/status
```

If upsd serves several UPSes, name the one to monitor: change `--nut localhost` in the service to
`--nut eaton@localhost`.

## 5. Connect the desktop UI

On the PC, create `%APPDATA%\UPS-BMC\config\ui.toml` (on Linux `~/.config/UPS-BMC/ui.toml`):
```toml
collector = "raspberrypi.lan:47800"   # the Pi's host name or IP address
token = "the token from step 3"
```
Restart UPS-BMC (Quit in the tray menu, then start it again). The footer shows
`· at raspberrypi.lan:47800`. With a remote collector, the UI never starts a local one. Stop the
local collector and remove it from autostart if it was running before: it would find no UPS once
the cable is on the Pi. You can also pass `--collector` and `--token-file` on the command line,
or set the token in the `UPS_BMC_TOKEN` environment variable.

Your history so far stays in the PC's database (`%LOCALAPPDATA%\UPS-BMC\data`); the Pi starts a
new one.

## 6. Optional: shut the PC down through NUT

To keep the PC protected after moving the cable, make it a NUT client of the Pi:
1. On the Pi, let upsd listen on the LAN as well, in `/etc/nut/upsd.conf`:
   ```ini
   LISTEN 127.0.0.1 3493
   LISTEN 0.0.0.0 3493
   ```
   Set `MODE=netserver` in `/etc/nut/nut.conf`, and add a user for the PC to
   `/etc/nut/upsd.users`:
   ```ini
   [pc]
       password = another-password
       upsmon secondary
   ```
   Then restart NUT: `sudo systemctl restart nut-server`.
2. On Windows, install a NUT client such as [WinNUT-Client](https://github.com/nutdotnet/WinNUT-Client)
   and point it at `eaton@<pi address>` with the `pc` user. It shuts the PC down on low battery
   or when the Pi's `upsmon` starts a forced shutdown.

UPS-BMC itself stays read-only: it never logs in to upsd, sets variables or sends commands.
