# navico-neon-extender

One program for a boat's network (a Raspberry Pi, or any laptop) that extends the Navico NEON
chartplotters (B&G Zeus / Simrad NSS / Lowrance, firmware 2.6.x):

- **Remote screen.** It finds the chartplotters by itself. Pick one on its page to see its screen on a
  phone, tablet or laptop, and tap, drag and send keys to it. It uses the chartplotter's own screen
  mirroring, so nothing is installed on the chartplotter. The older NOS units (Zeus³, NSS evo3, HDS
  Live, Vulcan and their siblings) work too, through the screen mirroring and remote control the
  vendor's phone app uses.
- **ForwardScan.** It shows a NOS sonar's ForwardScan on its page, and puts a ForwardScan tile on the
  chartplotters that opens it (this is [neon-forwardscan](crates/neon-forwardscan), built in).

It is a single file with no runtime dependencies: no Node, no browser plug-ins, nothing to install.

## Running it

```sh
./navico-neon-extender
```

It prints its address, for example `http://10.56.0.1:6022/`. Open that on a phone or tablet:

- The page lists the chartplotters it has found. Tap one. The first time you connect to a chartplotter
  it asks for **that chartplotter's WiFi password**, which can differ from unit to unit. It keeps the
  password, so after that one tap is enough, from any phone or tablet.
- A NOS unit needs no password. The first time you control one, it asks on its own screen whether to
  allow "NEON Extender": choose **Always**, and it does not ask again. (*Settings → Wireless → Remote
  controllers* on the unit lists it, and can forget it.) Its screen shows only while no other app
  watches it, such as the vendor's Link app: the unit makes a picture that a new viewer can start
  from only when its stream starts.
- **Add to Home Screen** (Safari's share menu, or Chrome's install menu) makes it a full-screen app
  with its own icon.

Options (`--help` shows all):

| Option | |
|---|---|
| `-p, --port N` | web page port (default 6022) |
| `--data-dir DIR` | where the passwords are kept (see below) |
| `--listen IP,…` | addresses to serve the pages on; also `NEON_EXTENDER_LISTEN` (default `::`: all, IPv6 and IPv4; `0.0.0.0`: IPv4 only) |
| `--password TEXT` | a password for the remote screens; also `NEON_EXTENDER_PASSWORD` (default: none) |
| `--plotter IP` | a chartplotter's address, when it is not found by itself; repeatable |
| `--no-mdns` | do not look for chartplotters with mDNS |
| `--no-forwardscan` | no ForwardScan |
| `-i, --interface NAME`, `--no-link-local`, `--read-only`, `--no-advertise`, `--name TEXT` | ForwardScan, as in [neon-forwardscan](crates/neon-forwardscan#on-the-chartplotters) |
| `-v` | more logging |

### Where the passwords are kept

`credentials.json`, readable by its owner only, in `$STATE_DIRECTORY` (set by systemd), or else in
`~/.config/navico-neon-extender/` (Linux), `~/Library/Application Support/navico-neon-extender/`
(macOS) or `%APPDATA%\navico-neon-extender\` (Windows). The **Forget password** item on the remote
screen's menu removes a chartplotter's password.

### Raspberry Pi as a service

```sh
sudo install -m 755 navico-neon-extender /usr/local/bin/
sudo install -m 644 packaging/navico-neon-extender.service /etc/systemd/system/
sudo systemctl enable --now navico-neon-extender
journalctl -u navico-neon-extender -f
```

It replaces a `neon-forwardscan` service: run one or the other. Both advertise the same ForwardScan
tile, and when both run, the tile keeps switching between them.

## Security

The only thing that protects a chartplotter's screen is its **WiFi password**. Anyone on the boat's
network who knows it can view and control the chartplotter, and this program keeps those passwords.
So anyone who can open this program's page can view and control the chartplotters whose passwords it
holds, unless you give the program a password of its own: `--password`, or `NEON_EXTENDER_PASSWORD` in
the environment (with systemd: in `/etc/default/navico-neon-extender`, see the unit). Then the remote
screens need a login, which a browser can remember (a cookie with a hash of the password, for 400 days;
a new password logs everyone out). ForwardScan stays open: the chartplotters show it themselves. Without
TLS the password crosses the network in the clear, so it keeps out the curious, not someone listening in.
A NOS unit's screen has no protection of its own: anyone on the network can watch it, with this program or
without, and control it once the unit allowed this program.
Run it only on a network you trust, and only with chartplotters you own or may use.

## How it works

```
 browser ──http/ws──► navico-neon-extender ──TLS, Bearer──► chartplotter :8444 /ws/receiver
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
 browser ◄──WebRTC video + input channels──► navico-neon-extender ◄──RTSP :554──── NOS unit
                                                    └──────────TCP :6633 input──────►
```

A NOS unit has no WebRTC, so the program plays the chartplotter's part itself and the page is the same
one. It makes the WebRTC offer ([str0m](https://github.com/algesten/str0m)), and sends the unit's RTSP
screen (`rtsp://<unit>:554/screenmirror`, H.264, no authentication; [retina](https://github.com/scottlamb/retina))
as the video, re-packetized, not re-encoded. The page's taps, drags and keys become the unit's
remote-control protocol on TCP 6633 (touch and key frames, after the unit allowed us). The units
advertise that port over mDNS as `_navico-mfd-rp._tcp`, and a session there tells model, name, screen
size and which keys they have. One RTSP stream per unit serves every page; it is started again when a
page joins, because the unit's encoder makes a key frame only when a stream starts.

The protocol notes and the original Node/TypeScript reference are in
[navico-neon-remote](https://github.com/keesverruijt/navico-neon-remote).

## Layout

| Crate | |
|---|---|
| [`neon-net`](crates/neon-net) | discovery, trust-all TLS, the chartplotter's REST API and signalling socket, the password store, the NOS remote-control protocol |
| [`neon-remote`](crates/neon-remote) | the signalling proxy, the NOS bridge (RTSP and 6633 to WebRTC), and the remote screen page |
| [`neon-forwardscan`](crates/neon-forwardscan) | ForwardScan, also a program of its own (absorbed with `git subtree`, history kept) |
| [`neon-extender`](crates/neon-extender) | the `navico-neon-extender` program: all of the above on one web server |

The pages are plain HTML/JS/CSS embedded in the binary. There is no build step for them.

## Building

```sh
cargo build --release          # target/release/navico-neon-extender
```

Static Linux binaries, from any host with Rust and clang. No C cross toolchain is needed:
`.cargo/config.toml` links with rust-lld, and clang compiles ring's bits of C (set
`CC_<target>=clang` on Linux).

```sh
rustup target add aarch64-unknown-linux-musl   # 64-bit Raspberry Pi OS; armv7-unknown-linux-musleabihf for 32-bit
cargo build --release --target aarch64-unknown-linux-musl -p neon-extender
```

To pull later changes from the standalone neon-forwardscan repository:

```sh
git subtree pull --prefix crates/neon-forwardscan git@github.com:keesverruijt/neon-forwardscan.git main
```

## License

Apache License 2.0, see [LICENSE](LICENSE).
