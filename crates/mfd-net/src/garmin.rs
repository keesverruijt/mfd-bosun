//! Garmin chartplotters' remote screen ("Garmin Helm", the ActiveCaptain app's Helm section). Decoded from
//! a live session (see `~/src/rev/garmin/HELM-REMOTE-SCREEN.md`). Like the Navico NOS units (see [`crate::nos`])
//! a Garmin plotter shares its screen as an RTSP H.264 stream and takes input over a small binary TCP
//! protocol, so both drive the one bridge through [`crate::screen`].
//!
//! Screen: `rtsp://<ip>:554/helm_<WIDTH>x<HEIGHT>.h264` (H.264, Constrained Baseline; no authentication,
//! no HDCP). The path encodes the plotter's screen resolution.
//!
//! Input: TCP port 51200, little-endian, each message `<opcode:u32> <length:u32> <payload[length]>`. The
//! opcodes carry the tag `0xBEEF` in their top two bytes; a request `0x????` is answered by `0x????+1`:
//!
//!   0xBEEF083F  hello        u16 (a client value)
//!   0xBEEF0AA9  handshake    u64 token
//!   0xBEEF1648  enumerate    u32 key; reply 0xBEEF1649 = key:u32, len:u32, value[len]
//!   0xBEEF1644  new-surface  (no payload); reply 0xBEEF1645 = count:u32, then count * surface_id[4]
//!   0xBEEF164C  pointer      ctx[9], X:u32, Y:u32, pressed:u32 (1 = contact, 0 = release), pad[3]
//!
//! X and Y are **normalized 0..=65535** across the screen (origin top-left, X right, Y down), not pixels —
//! so input is resolution-independent (a captured centre tap is 0x7f9c,0x6d22 ≈ 0.498,0.426; lower-left taps
//! are small values).
//!
//! The pointer `ctx[9]` is `surface_id[4] ++ [0x01, r0, r1, r2, r3]`: the 4-byte `surface_id` is the one the
//! plotter returns in its `new-surface` reply (so it is routed to the live screen), and the five trailing
//! bytes are a client-chosen pointer tag — a `0x01` and four bytes that are constant for the session and
//! appear nowhere in the plotter's own traffic (two sessions: `01 80 66 e0 1e`, `01 c0 08 bb 21`). Decoded
//! from a bidirectional capture (see `~/src/rev/garmin/HELM-REMOTE-SCREEN.md`).
//!
//! The enumerate replies also carry the plotter's identity: key `0x0b` is the model string (e.g.
//! "GPSMAP 723xsv") and key `0x00` is a `\r\n`-separated list of the screen's own RTSP URLs, native
//! resolution first (`rtsp://<host>:554/helm_1024x600.h264`).
//!
//! Discovery: the plotter advertises the remote screen over mDNS as `_garmin-helm._tcp`, whose SRV record
//! points straight at the control port ([`SERVICE_TYPE`], [`CONTROL_PORT`]). It also serves an app API over
//! plain HTTP on :80 (`/garmin/cards/`, `/garmin/inventory/device/`, and `/garmin/build/`, whose page gives
//! the firmware version — see [`build_version`]).

use crate::screen::Phase;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::watch;

pub const RTSP_PORT: u16 = 554;
pub const CONTROL_PORT: u16 = 51200;
pub const HTTP_PORT: u16 = 80;
/// The mDNS service for the remote screen; its SRV record points at [`CONTROL_PORT`].
pub const SERVICE_TYPE: &str = "_garmin-helm._tcp.local.";

/// Pointer coordinates are normalized across the screen to this range (0..=`COORD_MAX`).
pub const COORD_MAX: u32 = 65535;

// Message opcodes (the low byte and 0xBEEF; little-endian on the wire). A request `0x????` is answered by
// `0x????+1`.
const OP_HELLO: u32 = 0xBEEF_083F;
const OP_HANDSHAKE: u32 = 0xBEEF_0AA9;
const OP_ENUMERATE: u32 = 0xBEEF_1648;
const OP_ENUMERATE_REPLY: u32 = 0xBEEF_1649;
const OP_NEW_SURFACE: u32 = 0xBEEF_1644;
const OP_NEW_SURFACE_REPLY: u32 = 0xBEEF_1645;
const OP_POINTER: u32 = 0xBEEF_164C;

/// What a Garmin plotter is. The screen size is needed for the RTSP path and the picture's aspect ratio;
/// Garmin exposes no hardkeys in Helm.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    pub model: String,
    pub serial: String,
    pub name: String,
    pub width: u16,
    pub height: u16,
}

impl Info {
    /// Every Garmin marine brand is "garmin".
    pub fn brand(&self) -> &'static str {
        "garmin"
    }

    /// The RTSP URL of the screen at `ip`.
    pub fn rtsp_url(&self, ip: IpAddr) -> String {
        format!(
            "rtsp://{}/helm_{}x{}.h264",
            std::net::SocketAddr::new(ip, RTSP_PORT),
            self.width,
            self.height
        )
    }
}

// Enumerate keys whose replies carry the plotter's identity.
const KEY_RTSP_URLS: u32 = 0x00; // \r\n-separated RTSP URLs, native resolution first
const KEY_MODEL: u32 = 0x0b; // model string, e.g. "GPSMAP 723xsv"

/// Probes a Garmin plotter's control port for its identity: model, screen resolution and a stable id. Does
/// the handshake and reads the two identity enumerate keys, without taking a surface.
pub async fn info(addr: SocketAddr) -> std::io::Result<Info> {
    let s = tokio::time::timeout(TIMEOUT, crate::link::connect(addr))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "no answer"))??;
    s.set_nodelay(true)?;
    let (mut r, mut w) = s.into_split();
    w.write_all(&message(OP_HELLO, &rand_u16().to_le_bytes()))
        .await?;
    w.write_all(&message(OP_HANDSHAKE, &rand_u64().to_le_bytes()))
        .await?;
    w.write_all(&message(OP_ENUMERATE, &KEY_MODEL.to_le_bytes()))
        .await?;
    w.write_all(&message(OP_ENUMERATE, &KEY_RTSP_URLS.to_le_bytes()))
        .await?;
    let read = async {
        let model = enum_string(&read_enum(&mut r, KEY_MODEL).await?);
        let urls = enum_string(&read_enum(&mut r, KEY_RTSP_URLS).await?);
        Ok::<_, std::io::Error>((model, urls))
    };
    let (model, urls) = tokio::time::timeout(TIMEOUT, read)
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "no identity"))??;
    let (host, width, height) =
        parse_helm_url(&urls).ok_or_else(|| std::io::Error::other("no helm RTSP URL"))?;
    Ok(Info {
        model,
        serial: host_id(&host),
        name: String::new(),
        width,
        height,
    })
}

/// The plotter's firmware version from its `/garmin/build/` page on HTTP :80 (e.g. "42.05"), best effort —
/// `None` if the page does not answer or has no version line.
pub async fn build_version(ip: IpAddr) -> Option<String> {
    let addr = SocketAddr::new(ip, HTTP_PORT);
    let r = crate::http::get(addr, "/garmin/build/").await.ok()?;
    parse_build_version(&r.text())
}

/// The version from a `/garmin/build/` page: the text after "System Version:" up to the next tag.
fn parse_build_version(html: &str) -> Option<String> {
    let after = html.split_once("System Version:")?.1;
    let v = after
        .split('<')
        .next()?
        .trim()
        .trim_end_matches(|c: char| !c.is_ascii_alphanumeric());
    (!v.is_empty()).then(|| v.to_string())
}

/// The host and resolution of the first `helm_<W>x<H>.h264` RTSP URL in a `\r\n`-separated list.
fn parse_helm_url(urls: &str) -> Option<(String, u16, u16)> {
    let url = urls.lines().map(str::trim).find(|l| l.contains("/helm_"))?;
    let host = url
        .strip_prefix("rtsp://")?
        .split([':', '/'])
        .next()?
        .to_string();
    let dims = url.rsplit_once("/helm_")?.1.strip_suffix(".h264")?;
    let (w, h) = dims.split_once('x')?;
    Some((host, w.parse().ok()?, h.parse().ok()?))
}

/// A stable id for the unit from its RTSP hostname: the trailing numeric label (`garmin-j6-sailfish-3529725371`
/// -> `3529725371`), else the whole host label.
fn host_id(host: &str) -> String {
    let label = host.strip_suffix(".local").unwrap_or(host);
    match label.rsplit_once('-') {
        Some((_, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
            tail.to_string()
        }
        _ => label.to_string(),
    }
}

/// A screen fraction (0.0..=1.0) as a wire coordinate (0..=[`COORD_MAX`]).
pub fn coord(fraction: f64) -> u32 {
    (fraction.clamp(0.0, 1.0) * COORD_MAX as f64).round() as u32
}

/// A message: opcode, then the little-endian length, then the payload.
fn message(opcode: u32, payload: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(8 + payload.len());
    m.extend_from_slice(&opcode.to_le_bytes());
    m.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    m.extend_from_slice(payload);
    m
}

/// The pointer `ctx` for a session: the plotter's 4-byte `surface_id`, then a client pointer tag (`0x01` and
/// four session-random bytes — the plotter does not supply these).
fn session_ctx(surface_id: [u8; 4], tag: [u8; 4]) -> [u8; 9] {
    [
        surface_id[0],
        surface_id[1],
        surface_id[2],
        surface_id[3],
        0x01,
        tag[0],
        tag[1],
        tag[2],
        tag[3],
    ]
}

/// The 4-byte `surface_id` from a `new-surface` reply payload (`count:u32`, then `count` ids of 4 bytes). We
/// take the first.
fn surface_id(reply: &[u8]) -> Option<[u8; 4]> {
    if reply.len() < 8 || u32::from_le_bytes(reply[0..4].try_into().ok()?) == 0 {
        return None;
    }
    reply[4..8].try_into().ok()
}

/// A pointer event for the session `ctx` at normalized (`x`, `y`), pressed or released.
fn pointer(ctx: &[u8; 9], x: u32, y: u32, pressed: bool) -> Vec<u8> {
    let mut p = Vec::with_capacity(24);
    p.extend_from_slice(ctx);
    p.extend_from_slice(&x.to_le_bytes());
    p.extend_from_slice(&y.to_le_bytes());
    p.extend_from_slice(&(pressed as u32).to_le_bytes());
    p.extend_from_slice(&[0, 0, 0]); // pad to 24
    message(OP_POINTER, &p)
}

const TIMEOUT: Duration = Duration::from_secs(4);

/// The enumerate values the client subscribes to at connect, in the observed order.
const SUBSCRIBE: [u32; 12] = [0x0b, 0, 1, 6, 8, 0x0a, 3, 4, 5, 9, 2, 0x0c];

/// A live control connection to a Garmin plotter: normalized pointer events on TCP 51200. [`connect`] does
/// the hello/handshake, subscribes the enumerate keys the app does, then asks the plotter for a surface and
/// builds the pointer [`ctx`](session_ctx) from the id it returns.
pub struct Control {
    w: OwnedWriteHalf,
    ctx: [u8; 9],
    /// Garmin shows no "allow on the plotter" prompt, so access is granted at once.
    access: watch::Receiver<Option<crate::screen::Access>>,
    _keep_access: watch::Sender<Option<crate::screen::Access>>,
}

impl Control {
    /// Connects to the plotter's control port, does the handshake and obtains a drawing surface to steer.
    pub async fn connect(ip: IpAddr) -> std::io::Result<Control> {
        let addr = SocketAddr::new(ip, CONTROL_PORT);
        let s = tokio::time::timeout(TIMEOUT, crate::link::connect(addr))
            .await
            .map_err(|_| std::io::Error::other("no answer"))??;
        s.set_nodelay(true)?;
        let (mut r, mut w) = s.into_split();
        // client side of the handshake, as observed
        w.write_all(&message(OP_HELLO, &rand_u16().to_le_bytes()))
            .await?;
        w.write_all(&message(OP_HANDSHAKE, &rand_u64().to_le_bytes()))
            .await?;
        for v in SUBSCRIBE {
            w.write_all(&message(OP_ENUMERATE, &v.to_le_bytes()))
                .await?;
        }
        // ask for a surface and wait for the plotter to name one; its id routes our pointer events.
        w.write_all(&message(OP_NEW_SURFACE, &[])).await?;
        let reply = tokio::time::timeout(TIMEOUT, read_message(&mut r, OP_NEW_SURFACE_REPLY))
            .await
            .map_err(|_| std::io::Error::other("no surface from plotter"))??;
        let id = surface_id(&reply)
            .ok_or_else(|| std::io::Error::other("plotter offered no surface"))?;
        let ctx = session_ctx(id, rand_u32().to_le_bytes());
        let (tx, access) = watch::channel(Some(crate::screen::Access::Allowed));
        Ok(Control {
            w,
            ctx,
            access,
            _keep_access: tx,
        })
    }

    pub fn access(&self) -> watch::Receiver<Option<crate::screen::Access>> {
        self.access.clone()
    }

    /// A pointer at normalized (`fx`, `fy`) in 0.0..=1.0, pressed on [`Phase::Down`]/[`Phase::Move`].
    pub async fn pointer(&mut self, fx: f64, fy: f64, phase: Phase) -> std::io::Result<()> {
        let pressed = !matches!(phase, Phase::Up);
        let frame = pointer(&self.ctx, coord(fx), coord(fy), pressed);
        self.w.write_all(&frame).await
    }

    /// Garmin Helm exposes no hardkeys, so there is nothing to send.
    pub async fn key(&mut self, _name: &str, _down: bool) -> std::io::Result<()> {
        Ok(())
    }
}

/// Reads framed messages (`<opcode:u32><len:u32><payload>`) until one with `want`, returning its payload.
/// Other messages (enumerate replies, video) are skipped.
async fn read_message(r: &mut OwnedReadHalf, want: u32) -> std::io::Result<Vec<u8>> {
    loop {
        let mut head = [0u8; 8];
        r.read_exact(&mut head).await?;
        let opcode = u32::from_le_bytes(head[0..4].try_into().unwrap());
        let len = u32::from_le_bytes(head[4..8].try_into().unwrap()) as usize;
        let mut payload = vec![0u8; len];
        r.read_exact(&mut payload).await?;
        if opcode == want {
            return Ok(payload);
        }
    }
}

/// Reads enumerate replies until one for `key`, returning its value. Reply payload is
/// `key:u32, len:u32, value[len]`.
async fn read_enum(r: &mut OwnedReadHalf, key: u32) -> std::io::Result<Vec<u8>> {
    loop {
        let p = read_message(r, OP_ENUMERATE_REPLY).await?;
        if p.len() >= 8 && u32::from_le_bytes(p[0..4].try_into().unwrap()) == key {
            let len = u32::from_le_bytes(p[4..8].try_into().unwrap()) as usize;
            let end = (8 + len).min(p.len());
            return Ok(p[8..end].to_vec());
        }
    }
}

/// An enumerate value as text (trailing NUL dropped).
fn enum_string(value: &[u8]) -> String {
    String::from_utf8_lossy(value)
        .trim_end_matches('\0')
        .to_string()
}

fn rand_u32() -> u32 {
    let mut b = [0; 4];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b);
    u32::from_le_bytes(b)
}

fn rand_u16() -> u16 {
    let mut b = [0; 2];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b);
    u16::from_le_bytes(b)
}

fn rand_u64() -> u64 {
    let mut b = [0; 8];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b);
    u64::from_le_bytes(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coords_are_normalized_0_to_65535() {
        assert_eq!(coord(0.0), 0);
        assert_eq!(coord(1.0), 65535);
        assert_eq!(coord(0.5), 32768); // centre
        assert_eq!(coord(2.0), 65535); // clamped
    }

    #[test]
    fn surface_reply_yields_the_id() {
        // the plotter's new-surface reply: count=1, then the 4-byte surface id
        assert_eq!(
            surface_id(&hex("01000000 5cdbee27")),
            Some([0x5c, 0xdb, 0xee, 0x27])
        );
        assert_eq!(surface_id(&hex("00000000")), None); // no surface offered
        assert_eq!(surface_id(&[]), None);
    }

    #[test]
    fn ctx_is_the_surface_id_then_a_client_tag() {
        // surface id from the plotter, four client-chosen tag bytes -> the 9-byte ctx seen on the wire
        let ctx = session_ctx([0x5c, 0xdb, 0xee, 0x27], [0x80, 0x66, 0xe0, 0x1e]);
        assert_eq!(ctx, [0x5c, 0xdb, 0xee, 0x27, 0x01, 0x80, 0x66, 0xe0, 0x1e]);
    }

    #[test]
    fn pointer_matches_a_captured_centre_tap() {
        // a captured centre tap (bidirectional session): surface 5c db ee 27, tag 80 66 e0 1e,
        // X=0x7f9c Y=0x6d22 (≈ 0.498, 0.426), pressed
        let ctx = session_ctx([0x5c, 0xdb, 0xee, 0x27], [0x80, 0x66, 0xe0, 0x1e]);
        let got = pointer(&ctx, 0x7f9c, 0x6d22, true);
        let want = hex("4c16efbe18000000 5cdbee2701 8066e01e 9c7f0000 226d0000 01000000 000000");
        assert_eq!(got, want);
    }

    #[test]
    fn pointer_matches_an_earlier_tap() {
        // an earlier, client-only capture (lower-left): ctx 03 d9 56 e1 01 c0 08 bb 21
        let ctx = [0x03, 0xd9, 0x56, 0xe1, 0x01, 0xc0, 0x08, 0xbb, 0x21];
        let got = pointer(&ctx, 0x08eb, 0x1074, true);
        let want = hex("4c16efbe18000000 03d956e101c008bb21 eb080000 74100000 01000000 000000");
        assert_eq!(got, want);
    }

    #[test]
    fn reads_model_and_resolution_from_the_rtsp_list() {
        // the captured enumerate-0x00 value: native resolution first, two smaller alternatives
        let host = "garmin-j6-sailfish-3529725371.local";
        let urls = format!(
            "rtsp://{host}:554/helm_1024x600.h264\r\n\
             rtsp://{host}:554/helm_800x480.h264\r\n\
             rtsp://{host}:554/helm_640x400.h264\r\n"
        );
        assert_eq!(parse_helm_url(&urls), Some((host.to_string(), 1024, 600)));
        assert_eq!(host_id(host), "3529725371");
        assert_eq!(host_id("plotter.local"), "plotter");
    }

    #[test]
    fn parses_the_firmware_version_from_build_page() {
        let html = "<h2>\"Garmin x3xsv\" - garmin-j6-sailfish-3529725371<h2>\n<h2>System Version: 42.05<h2>";
        assert_eq!(parse_build_version(html).as_deref(), Some("42.05"));
        assert_eq!(parse_build_version("<h2>no version here</h2>"), None);
    }

    #[test]
    fn message_framing() {
        // enumerate for 0x0b: opcode LE, length 4 LE, payload
        assert_eq!(
            message(OP_ENUMERATE, &0x0b_u32.to_le_bytes()),
            hex("4816efbe 04000000 0b000000")
        );
    }

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }
}
