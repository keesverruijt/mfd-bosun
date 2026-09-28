# mfd-bosun

The bosun looks after the boat's gear; this one looks after its Navico chartplotters (MFDs: B&G,
Simrad, Lowrance). One program for a boat's network (a Raspberry Pi, or any laptop), for the NEON
chartplotters (firmware 2.6.x: Zeus S / SR / SRX, NSS 4, …) and the older NOS ones before them:

- **Remote screen.** It finds the chartplotters by itself. Pick one on its page to see its screen on a
  phone, tablet or laptop, and tap, drag and send keys to it. It uses the chartplotter's own screen
  mirroring, so nothing is installed on the chartplotter. The older NOS units (Zeus³, NSS evo3, HDS
  Live, Vulcan and their siblings) work too, through the screen mirroring and remote control the
  vendor's phone app uses.
- **Waypoints, routes and files.** Each chartplotter's waypoints and routes as a GPX file (an older
  NOS unit's tracks too), a NEON chartplotter can be sent a file, and a NOS unit's own backups (its
  waypoints, routes and tracks as a `.usr` file, and its settings as a `.set` file, as the vendor's app
  makes them) can be downloaded.
- **ForwardScan.** It shows a NOS sonar's ForwardScan on its page, and puts a ForwardScan tile on the
  chartplotters that opens it.

It is a single file with no runtime dependencies: no Node, no browser plug-ins, nothing to install.

## Running it

```sh
./mfd-bosun
```

It prints its address, for example `http://10.56.0.1:6022/`. Open that on a phone or tablet:

- The page lists the chartplotters it has found. Tap one. The first time you connect to a chartplotter
  it asks for **that chartplotter's WiFi password**, which can differ from unit to unit. It keeps the
  password, so after that one tap is enough, from any phone or tablet.
- A NOS unit needs no password. The first time you control one, it asks on its own screen whether to
  allow "MFD Bosun": choose **Always**, and it does not ask again. (*Settings → Wireless → Remote
  controllers* on the unit lists it, and can forget it.)
- **Add to Home Screen** (Safari's share menu, or Chrome's install menu) makes it a full-screen app
  with its own icon.

Options (`--help` shows all):

| Option | |
|---|---|
| `-p, --port N` | web page port (default 6022) |
| `--data-dir DIR` | where the passwords are kept (see below) |
| `--listen IP,…` | addresses to serve the pages on; also `MFD_BOSUN_LISTEN` (default `::`: all, IPv6 and IPv4; `0.0.0.0`: IPv4 only) |
| `--password TEXT` | a password for the remote screens; also `MFD_BOSUN_PASSWORD` (default: none) |
| `--plotter IP` | a chartplotter's address, when it is not found by itself; repeatable |
| `--no-mdns` | do not look for chartplotters with mDNS |
| `--no-forwardscan` | no ForwardScan |
| `-i, --interface NAME` | the interface the sonar is on, when it cannot tell by itself |
| `--no-link-local` | never add a 169.254 address (changing the sonar's settings needs one) |
| `--read-only` | the page cannot change the sonar's ForwardScan settings |
| `--no-advertise` | no ForwardScan tile on the chartplotters |
| `--name TEXT` | name of the ForwardScan tile (default ForwardScan) |
| `-v` | more logging |

### Where the passwords are kept

`credentials.json`, readable by its owner only, in `$STATE_DIRECTORY` (set by systemd), or else in
`~/.config/mfd-bosun/` (Linux), `~/Library/Application Support/mfd-bosun/`
(macOS) or `%APPDATA%\mfd-bosun\` (Windows). The **Forget password** item on the remote
screen's menu removes a chartplotter's password.

### Raspberry Pi as a service

On the Raspberry Pi, with 64-bit Raspberry Pi OS (for 32-bit, use
`mfd-bosun-armv7-unknown-linux-musleabihf`; `x86_64-unknown-linux-musl` for a PC):

```sh
curl -L https://keversoft.com/downloads/mfd-bosun/latest/mfd-bosun-aarch64-unknown-linux-musl.tar.gz | tar xz
sudo mfd-bosun-aarch64-unknown-linux-musl/install.sh
journalctl -u mfd-bosun -f
```

That is the latest build of `main` (`latest/BUILD` says which commit). `install.sh` puts the program
in `/usr/local/bin`, installs the systemd unit, enables and (re)starts it. Its settings go in
`/etc/default/mfd-bosun` (a password, the addresses, and any other options in `MFD_BOSUN_ARGS`); a
first install writes a commented one, and an update keeps yours. To update, do the same again.

## Security

The only thing that protects a chartplotter's screen is its **WiFi password**. Anyone on the boat's
network who knows it can view and control the chartplotter, and this program keeps those passwords.
So anyone who can open this program's page can view and control the chartplotters whose passwords it
holds, unless you give the program a password of its own: `--password`, or `MFD_BOSUN_PASSWORD` in
the environment (with systemd: in `/etc/default/mfd-bosun`). Then the remote
screens need a login, which a browser can remember (a cookie with a hash of the password, for 400 days;
a new password logs everyone out). ForwardScan stays open: the chartplotters show it themselves. Without
TLS the password crosses the network in the clear, so it keeps out the curious, not someone listening in.
A NOS unit's screen has no protection of its own: anyone on the network can watch it, with this program or
without, and control it once the unit allowed this program. The same goes for its waypoints, routes, tracks
and settings, which its web server gives to anyone who asks.
Run it only on a network you trust, and only with chartplotters you own or may use.

## How it works

```
 browser ──http/ws──► mfd-bosun ──TLS, Bearer──► chartplotter :8444 /ws/receiver
    │                                                              ▲
    └──────────── WebRTC video + input channels (peer to peer) ────┘
```

The chartplotter is the WebRTC offerer and sends its screen; the browser answers it. The program does
only what a browser cannot do. It pairs with the chartplotter's WiFi password (HTTP Basic,
`POST /api/v1/pair`) and gets a token. It opens the chartplotter's signalling WebSocket with an
`Authorization: Bearer` header over TLS, against the chartplotter's self-signed certificate. It then
passes the signalling messages through unchanged. The video never passes through the program.

- **Discovery:** the chartplotters advertise their API over mDNS as `_neonapi._tcp` (port 8444), and
  `GET /api/v1/info` there gives their model and serial number without authentication. Their
  ForwardScan announcements (UDP 239.2.1.1:2052) give their names, and find them when mDNS does not
  get through.
- **TLS:** ALPN must be `http/1.1` only: offered h2, the chartplotter hangs up.
- **Pairing:** the `snap_id` sent must be the chartplotter's own (from `/api/v1/info`), or its
  signalling socket refuses the token.

### NOS units

```
 browser ◄──WebRTC video + input channels──► mfd-bosun ◄──RTSP :554──── NOS unit
                                                    └──────────TCP :6633 input──────►
```

A NOS unit has no WebRTC, so the program plays the chartplotter's part itself and the page is the same
one. It makes the WebRTC offer ([str0m](https://github.com/algesten/str0m)), and sends the unit's RTSP
screen (`rtsp://<unit>:554/screenmirror`, H.264, no authentication; [retina](https://github.com/scottlamb/retina))
as the video, re-packetized, not re-encoded. The page's taps, drags and keys become the unit's
remote-control protocol on TCP 6633 (touch and key frames, after the unit allowed us). The units
advertise that port over mDNS as `_navico-mfd-rp._tcp`, and a session there tells model, name, screen
size and which keys they have. One RTSP stream per unit serves every page. The unit's encoder makes an
IDR frame only when its stream starts, and a browser starts decoding only at one; so for a page that
joins later, the program turns the next whole picture the unit sends (about once a second, an ordinary
frame of I slices) into an IDR frame by rewriting its slice headers, and numbers the frames after it on
from there. The vendor's app, being ffmpeg, simply starts at such a picture.

The backups are what the unit's web server (HTTP, port 80) gives the vendor's app:
`/cgi-bin/download.cgi` is `WaypointsRoutesTracks.usr`, all waypoints, routes and trails in Navico's USR
format (version 6 on the Vulcan; read here for the GPX, trails included), and `/cgi-bin/settings.cgi` is
`Settings.set`.

The protocol notes and the original Node/TypeScript reference are in
[navico-neon-remote](https://github.com/keesverruijt/navico-neon-remote).

## Layout

| Crate | |
|---|---|
| [`mfd-net`](crates/mfd-net) | discovery, trust-all TLS, the chartplotter's REST API and signalling socket, the password store, the NOS remote-control protocol |
| [`mfd-remote`](crates/mfd-remote) | the signalling proxy, the NOS bridge (RTSP and 6633 to WebRTC), and the remote screen page |
| [`neon-forwardscan`](crates/neon-forwardscan) | ForwardScan: the sonar's picture, its page and the chartplotter tile |
| [`mfd-bosun`](crates/mfd-bosun) | the `mfd-bosun` program: all of the above on one web server |

The pages are plain HTML/JS/CSS embedded in the binary. There is no build step for them.

## Building

```sh
cargo build --release          # target/release/mfd-bosun
```

Static Linux binaries, from any host with Rust and clang. No C cross toolchain is needed:
`.cargo/config.toml` links with rust-lld, and clang compiles ring's bits of C (set
`CC_<target>=clang` on Linux).

```sh
rustup target add aarch64-unknown-linux-musl   # 64-bit Raspberry Pi OS; armv7-unknown-linux-musleabihf for 32-bit
cargo build --release --target aarch64-unknown-linux-musl -p mfd-bosun
```

## License

Apache License 2.0, see [LICENSE](LICENSE).
