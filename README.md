# neon-forwardscan

Shows the ForwardScan of a Navico NOS sonar device (a chartplotter with a ForwardScan sensor, or a
SonarHub) in a web page, and advertises that page so it shows up as a ForwardScan tile on Navico
NEON chartplotters. 

One small program, no installation: a static binary for Linux (e.g. a Raspberry Pi), a
self-contained one for macOS and for Windows. 

The protocol was reverse-engineered from network captures; see [src/protocol.rs](src/protocol.rs). 
Tested with a Vulcan 7R, with a Zeus SRX-16 showing the page.

## Running it

Download the binary for your computer from the releases, and start it on a computer that is on the
boat's Ethernet:

```sh
./neon-forwardscan
```

It prints the address of the page (port **6022**), for example `http://10.56.0.1:6022/`. Within
10 seconds the chartplotters show a **ForwardScan** tile that opens it. 

The tile opens the page full screen, without the databar: a web app's page never has one on the
chartplotters. For a databar, make your own page in the split editor with only ForwardScan in it
and the databar not removed.

## On the chartplotters

- The page's settings are behind the **menu button** at the top right, like the chartplotter
  apps' own menus. The menu slides in over the view and hides again after a tap on the view, or
  after 15 seconds without use. "Paused" in the middle means ForwardScan is not running; the
  menu's Run button starts it.
- The tile is advertised every 5 seconds, from one interface: the one the chartplotters were heard
  on. An open page reloads itself when the program is restarted or updated.
- When the program stops, the chartplotters show a **"ForwardScan not found"** alarm after about
  25 seconds. It clears by itself when the program runs again.
- A tile is never removed by the chartplotter itself: remove it with the **Uninstall** button on
  the page the tile shows while the program is not running. `--name` gives the tile another name,
  and so a new tile; the old one stays until you remove it.

Options (`--help` shows all):

| Option | |
|---|---|
| `-p, --port N` | web page port (default 6022) |
| `-i, --interface NAME` | the interface the sonar is on, when it cannot tell by itself |
| `--no-link-local` | never add a 169.254 address (see below) |
| `--read-only` | the page cannot change the sonar's settings |
| `--no-advertise` | no tile on the chartplotters |
| `--name TEXT` | name of the tile (default ForwardScan) |
| `-v` | more logging |

## The 169.254 address

Showing ForwardScan needs no network setup: the sonar broadcasts it to everyone on the Ethernet.
Changing its settings (run/pause, depth, forward range, noise rejection) does: the sonar only
takes a settings change sent to its **169.254.x.x** (link-local) address, the one the Navico
devices use among themselves, and the computer can only send there when it has a 169.254.x.x
address on that interface itself.

- If it has one, it is used.
- If it has none and the program **may** add one (run as root / Administrator, or on Linux after
  `sudo setcap cap_net_admin+ep ./neon-forwardscan`), it adds one next to the address it already
  has (derived from the MAC address, so always the same) and removes it again when it stops.
- Otherwise the page shows the ForwardScan, and its **Network** section and the log say how to add
  the address (the exact command for your system).

Which interface the sonar is on is found from the sonar's own announcement (its other address);
with several interfaces and no clue, use `--interface`. VPN tunnels are never used or advertised on.

### Raspberry Pi as a service

```sh
sudo install -m 755 neon-forwardscan /usr/local/bin/
sudo tee /etc/systemd/system/neon-forwardscan.service >/dev/null <<'EOF'
[Unit]
Description=ForwardScan web page for Navico chartplotters
After=network-online.target
Wants=network-online.target

[Service]
ExecStart=/usr/local/bin/neon-forwardscan
Restart=always
DynamicUser=yes
AmbientCapabilities=CAP_NET_ADMIN
CapabilityBoundingSet=CAP_NET_ADMIN

[Install]
WantedBy=multi-user.target
EOF
sudo systemctl enable --now neon-forwardscan
journalctl -u neon-forwardscan -f
```

`CAP_NET_ADMIN` lets it add the 169.254 address without running as root.

### Windows

Run it from a terminal. Windows asks whether to allow it through the firewall: allow it on
private networks (the chartplotters need to reach the page, and the sonar's broadcasts need to
come in). Run it as Administrator once if it should add the 169.254 address itself.

### macOS

The binary is not signed: after downloading, `xattr -d com.apple.quarantine neon-forwardscan`, or
right-click it in the Finder and choose Open. macOS asks to allow incoming connections; allow them.
With `sudo` it can add the 169.254 address itself.

## Building

```sh
cargo build --release
```

Static Linux binaries, from any host with Rust: no C cross toolchain is needed, `.cargo/config.toml`
links them with rust-lld.

```sh
rustup target add aarch64-unknown-linux-musl   # a 64-bit Raspberry Pi OS; armv7-unknown-linux-musleabihf for 32-bit
cargo build --release --target aarch64-unknown-linux-musl
```

## License

Apache License 2.0, see [LICENSE](LICENSE).
