# mfd-bosun

The bosun looks after the boat's gear; this one looks after its chartplotters. mfd-bosun is a small
program that runs on your boat's network (on a Raspberry Pi, or any laptop) and puts your B&G,
Simrad, Lowrance, Garmin, Raymarine and Furuno chartplotters on your phone, tablet or laptop.

## What it does

- **Remote screen.** See a chartplotter's screen in your browser, and tap and drag on it as if you
  were standing at the helm. It uses the chartplotter's own screen sharing, so nothing needs to be
  installed on the chartplotter.
- **Waypoints, routes and tracks.** Download them as a GPX file, for use in any navigation app.
- **ForwardScan.** See the ForwardScan sonar picture in your browser, and get a ForwardScan tile on
  the chartplotters that opens it.

It finds the chartplotters by itself. It is a single file: nothing else to install, no apps, no
browser plug-ins.

## Which chartplotters

| Brand | Models | Remote screen | GPX download | Also |
|---|---|---|---|---|
| B&G, Simrad | Newer units (Zeus S, Zeus SR, Zeus SRX, NSS 4, …) | ✓ | waypoints, routes | keys, send a file |
| B&G, Simrad, Lowrance | Older units (Zeus³, Vulcan, NSS evo3, HDS Live, …) | ✓ | waypoints, routes, tracks | keys, the unit's own backups |
| Garmin | Units that work with the ActiveCaptain app's Helm (GPSMAP, …) | ✓ | waypoints, routes | |
| Raymarine | Axiom | ✓ | | |
| Furuno | TimeZero units (TZMAP, …) | | | remote control, with canboat |

Other models from the same families should work too.

## Getting started

### Install

On a Raspberry Pi, a Linux PC or a Mac, in a terminal:

```sh
curl -fsSL https://keversoft.com/downloads/mfd-bosun/latest/install.sh | sh
```

On Windows, in PowerShell:

```powershell
irm https://keversoft.com/downloads/mfd-bosun/latest/install.ps1 | iex
```

To update, run the same line again.

**On Linux** (any Raspberry Pi but the first Pi and the first Pi Zero, or a PC) this installs mfd-bosun as a service that starts with the computer. Settings go in
`/etc/default/mfd-bosun`; to see what it is doing, run `journalctl -u mfd-bosun -f`.

**On a Mac or Windows** it installs the `mfd-bosun` command; run it when you are on the boat:

```sh
mfd-bosun
```

Rather download it yourself? The builds are all in `https://keversoft.com/downloads/mfd-bosun/latest/`:
`mfd-bosun-macos`, `mfd-bosun-windows-x86_64.exe`, and for Linux
`mfd-bosun-<target>.tar.gz` (`aarch64-unknown-linux-musl`, `armv7-unknown-linux-musleabihf` or
`x86_64-unknown-linux-musl`), with an `install.sh` inside that sets up the service.

### Using it

mfd-bosun prints its address, for example `http://10.56.0.1:6022/`. Open that on a phone or tablet
on the boat's network. The page lists the chartplotters it has found; tap one.

Some chartplotters ask for permission the first time:

- **Newer B&G and Simrad units** ask for **that chartplotter's WiFi password** (each unit can have
  its own). mfd-bosun remembers it, so after that one tap is enough, from any device.
- **Older B&G, Simrad and Lowrance units** ask on their own screen whether to allow "MFD Bosun":
  choose **Always**. (*Settings → Wireless → Remote controllers* on the unit can forget it again.)
- **Raymarine** shows its screen when its *Mobile app access* setting is *Viewing only*, and can be
  controlled when it is *Remote control*.
- **Furuno** does not share its screen, but mfd-bosun gives it a remote control: the keys, cursor and
  knob of Furuno's MCU-006 remote. This needs [canboat](https://github.com/canboat/canboat) running
  `canboat server --quirk mcu006` on the chartplotter's NMEA 2000 network, and the computer running
  mfd-bosun on the Furuno network (172.31.x.x).

**Add to Home Screen** (Safari's share menu, or Chrome's install menu) turns the page into a
full-screen app with its own icon.

## Options

The most useful ones (`--help` shows them all):

| Option | |
|---|---|
| `-p, --port N` | the web page's port (default 6022) |
| `--password TEXT` | require a password to open the remote screens (see Security) |
| `--plotter IP` | a chartplotter's address, if it is not found by itself; can be repeated |
| `--no-forwardscan` | turn ForwardScan off |
| `--read-only` | the page can show ForwardScan but not change its settings |
| `--no-advertise` | no ForwardScan tile on the chartplotters |
| `-v` | more logging |

## Security

Most chartplotters do little to protect their screen: anyone on the boat's network can watch it,
and often control it, with or without mfd-bosun. The newer B&G and Simrad units are protected by
their WiFi password, but mfd-bosun remembers those passwords, so anyone who can open its page can
control those chartplotters too.

To keep that page to yourself, give mfd-bosun a password of its own with `--password` (on a
Raspberry Pi: in `/etc/default/mfd-bosun`). The browser can remember the login. The password is sent
without encryption, so it keeps out the curious, not a determined snoop.

Run mfd-bosun only on a network you trust, and only with chartplotters you own or may use. A saved
WiFi password can be removed with **Forget password** in the remote screen's menu.

## Building from source

```sh
cargo build --release          # target/release/mfd-bosun
```

For a Raspberry Pi, from any computer with Rust and clang:

```sh
rustup target add aarch64-unknown-linux-musl
cargo build --release --target aarch64-unknown-linux-musl -p mfd-bosun
```

## License

Apache License 2.0, see [LICENSE](LICENSE).
