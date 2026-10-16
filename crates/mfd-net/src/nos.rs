//! The older Navico MFDs (NOS: Zeus³, NSS evo3, HDS Live, Vulcan and their siblings) have no WebRTC. Their
//! screen is an RTSP stream (H.264, no authentication) at `rtsp://<ip>:554/screenmirror`, and their input a
//! small binary protocol on TCP 6633, the one the vendor's phone app uses. They advertise the latter over
//! mDNS as `_navico-mfd-rp._tcp`.
//!
//! Every frame on 6633 is `u16 length, u16 command, payload`, the length counting command and payload, all
//! integers big-endian:
//!
//!   0x0001  ->  SESSION        u32 id
//!   0x0002  <-  SESSION reply  u32 id, u8, model[32], name[32], firmware[16], u32, u32 n, n × (u32 key, u32 code),
//!                              u16 width, u16 height, u16 width, u16 height, u32
//!   0x0003  ->  AUTHENTICATE   mac[6], name[32]: the unit remembers the MAC once the user allowed it
//!   0x0004  <-  STATUS         7 bytes, the last 1 when input is allowed
//!   0x1001  ->  TOUCH          u32 ms, u16 x, u16 y (screen pixels), u8 0 down / 1 move / 2 up, u8 fingers
//!   0x1003  ->  KEY            u32 code (a Linux key code), u32 1 down / 0 up
//!
//! Their web server (HTTP, port 80, no authentication) gives the backups the vendor's app makes:
//! [`WAYPOINTS`] (a USR file) and [`SETTINGS`].

use std::net::SocketAddr;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;

pub const PORT: u16 = 6633;
pub const RTSP_PORT: u16 = 554;
pub const SERVICE_TYPE: &str = "_navico-mfd-rp._tcp.local.";
const TIMEOUT: Duration = Duration::from_secs(4);
pub const HTTP_PORT: u16 = 80;
/// All waypoints, routes and trails, as `WaypointsRoutesTracks.usr`.
pub const WAYPOINTS: &str = "/cgi-bin/download.cgi";
/// The settings, as `Settings.set`.
pub const SETTINGS: &str = "/cgi-bin/settings.cgi";
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const FETCH_MAX: usize = 64 << 20;
const MAX_FRAME: usize = 0x400;

const SESSION: u16 = 0x0001;
const SESSION_REPLY: u16 = 0x0002;
const AUTHENTICATE: u16 = 0x0003;
const STATUS: u16 = 0x0004;
const TOUCH: u16 = 0x1001;
const KEY: u16 = 0x1003;

/// The keys by the numbers the units use for them; the codes that go on the wire are per model.
pub const KEY_NAMES: [&str; 12] = [
    "page", "menu", "zoomin", "zoomout", "power", "waypoint", "enter", "cancel", "mob", "goto",
    "mark", "user",
];

#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub model: String,
    pub name: String,
    pub firmware: String,
    /// (name, code) of the unit's keys
    pub keys: Vec<(String, u32)>,
    pub width: u16,
    pub height: u16,
}

impl Info {
    pub fn key(&self, name: &str) -> Option<u32> {
        self.keys.iter().find(|(n, _)| n == name).map(|(_, c)| *c)
    }

    /// The brand, from the model name.
    pub fn brand(&self) -> &'static str {
        let m = self.model.to_ascii_lowercase();
        if ["vulcan", "zeus", "triton"]
            .iter()
            .any(|b| m.starts_with(b))
        {
            "bandg"
        } else if ["nss", "nso", "go", "cruise", "nsx", "evo"]
            .iter()
            .any(|b| m.starts_with(b))
        {
            "simrad"
        } else if ["hds", "elite", "hook", "eagle"]
            .iter()
            .any(|b| m.starts_with(b))
        {
            "lowrance"
        } else {
            ""
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Touch {
    Down = 0,
    Move = 1,
    Up = 2,
}

impl From<crate::screen::Phase> for Touch {
    fn from(p: crate::screen::Phase) -> Touch {
        match p {
            crate::screen::Phase::Down => Touch::Down,
            crate::screen::Phase::Move => Touch::Move,
            crate::screen::Phase::Up => Touch::Up,
        }
    }
}

fn frame(cmd: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(4 + payload.len());
    f.extend_from_slice(&(2 + payload.len() as u16).to_be_bytes());
    f.extend_from_slice(&cmd.to_be_bytes());
    f.extend_from_slice(payload);
    f
}

async fn read_frame(r: &mut OwnedReadHalf) -> std::io::Result<(u16, Vec<u8>)> {
    let n = r.read_u16().await? as usize;
    if !(2..=MAX_FRAME).contains(&n) {
        return Err(std::io::Error::other(format!("bad frame length {n}")));
    }
    let cmd = r.read_u16().await?;
    let mut body = vec![0; n - 2];
    r.read_exact(&mut body).await?;
    Ok((cmd, body))
}

fn text(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}

pub fn parse_session_reply(b: &[u8]) -> Option<Info> {
    let n = be32(b, 89)? as usize;
    if n > 32 {
        return None;
    }
    let mut keys = Vec::with_capacity(n);
    for i in 0..n {
        let at = 93 + 8 * i;
        let (k, code) = (be32(b, at)?, be32(b, at + 4)?);
        let name = KEY_NAMES
            .get((k as usize).wrapping_sub(1))
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("key{k}"));
        keys.push((name, code));
    }
    let at = 93 + 8 * n;
    Some(Info {
        model: text(b.get(5..37)?),
        name: text(b.get(37..69)?),
        firmware: text(b.get(69..85)?),
        keys,
        width: be16(b, at)?,
        height: be16(b, at + 2)?,
    })
}

async fn open(addr: SocketAddr) -> std::io::Result<(OwnedReadHalf, OwnedWriteHalf, Info)> {
    let s = tokio::time::timeout(TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| std::io::Error::other("no answer"))??;
    s.set_nodelay(true)?;
    let (mut r, mut w) = s.into_split();
    let id: u32 = rand_u32();
    w.write_all(&frame(SESSION, &id.to_be_bytes())).await?;
    let info = tokio::time::timeout(TIMEOUT, async {
        loop {
            let (cmd, body) = read_frame(&mut r).await?;
            if cmd == SESSION_REPLY {
                return parse_session_reply(&body)
                    .ok_or_else(|| std::io::Error::other("a session reply we do not understand"));
            }
        }
    })
    .await
    .map_err(|_| std::io::Error::other("no session reply"))??;
    Ok((r, w, info))
}

fn rand_u32() -> u32 {
    let mut b = [0; 4];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b);
    u32::from_be_bytes(b)
}

/// What the unit is, from a session it answers (and nothing more is sent).
pub async fn info(addr: SocketAddr) -> std::io::Result<Info> {
    let (_r, mut w, info) = open(addr).await?;
    let _ = w.shutdown().await;
    Ok(info)
}

/// `path` from the unit's web server: the body of a 200 answer.
pub async fn fetch(ip: std::net::IpAddr, path: &str) -> std::io::Result<Vec<u8>> {
    let addr = SocketAddr::new(ip, HTTP_PORT);
    let get = async {
        let mut s = TcpStream::connect(addr).await?;
        let host = match ip {
            std::net::IpAddr::V6(v6) => format!("[{v6}]"),
            v4 => v4.to_string(),
        };
        s.write_all(
            format!("GET {path} HTTP/1.0\r\nHost: {host}\r\nAccept: */*\r\n\r\n").as_bytes(),
        )
        .await?;
        // HTTP/1.0: the answer ends where the connection does
        let mut d = Vec::new();
        (&mut s)
            .take(FETCH_MAX as u64 + 1)
            .read_to_end(&mut d)
            .await?;
        if d.len() > FETCH_MAX {
            return Err(std::io::Error::other("too big"));
        }
        Ok::<_, std::io::Error>(d)
    };
    let d = tokio::time::timeout(FETCH_TIMEOUT, get)
        .await
        .map_err(|_| std::io::Error::other("no answer"))??;
    // the head ends at an empty line; the unit's CGI ends its header lines with a bare \n
    let (head_end, body) = (0..d.len())
        .find_map(|i| {
            if d[i..].starts_with(b"\n\n") {
                Some((i, i + 2))
            } else if d[i..].starts_with(b"\n\r\n") {
                Some((i, i + 3))
            } else {
                None
            }
        })
        .ok_or_else(|| std::io::Error::other("not an HTTP answer"))?;
    let status = String::from_utf8_lossy(&d[..head_end]);
    let status = status.lines().next().unwrap_or("").trim();
    if status.split_whitespace().nth(1) != Some("200") {
        return Err(std::io::Error::other(format!("{path}: {status}")));
    }
    Ok(d[body..].to_vec())
}

/// Whether input is allowed, as the unit says. Shared with the other vendors (see [`crate::screen`]).
pub use crate::screen::Access;

/// A connection that sends input.
pub struct Control {
    w: OwnedWriteHalf,
    pub info: Info,
    t0: Instant,
    /// Access changes, as the unit reports them.
    pub access: tokio::sync::watch::Receiver<Option<Access>>,
}

impl Control {
    /// Connects and asks for input access as `mac` / `name` (what the unit shows and remembers).
    pub async fn connect(addr: SocketAddr, mac: [u8; 6], name: &str) -> std::io::Result<Control> {
        let (mut r, mut w, info) = open(addr).await?;
        let mut p = [0u8; 38];
        p[..6].copy_from_slice(&mac);
        let n = name.len().min(31);
        p[6..6 + n].copy_from_slice(&name.as_bytes()[..n]);
        w.write_all(&frame(AUTHENTICATE, &p)).await?;
        let (tx, access) = tokio::sync::watch::channel(None);
        tokio::spawn(async move {
            loop {
                match read_frame(&mut r).await {
                    Ok((STATUS, body)) => {
                        log::debug!("NOS status {body:02x?}");
                        let a = if body.last() == Some(&1) {
                            Access::Allowed
                        } else {
                            Access::Waiting
                        };
                        tx.send_replace(Some(a));
                    }
                    Ok((cmd, body)) => log::debug!("NOS {cmd:#06x} {body:02x?}"),
                    Err(_) => break, // the watch closing tells
                }
            }
        });
        Ok(Control {
            w,
            info,
            t0: Instant::now(),
            access,
        })
    }

    pub async fn touch(&mut self, x: u16, y: u16, kind: Touch) -> std::io::Result<()> {
        let mut p = [0u8; 10];
        p[..4].copy_from_slice(&(self.t0.elapsed().as_millis() as u32).to_be_bytes());
        p[4..6].copy_from_slice(&x.to_be_bytes());
        p[6..8].copy_from_slice(&y.to_be_bytes());
        p[8] = kind as u8;
        p[9] = 1;
        self.w.write_all(&frame(TOUCH, &p)).await
    }

    pub async fn key(&mut self, code: u32, down: bool) -> std::io::Result<()> {
        let mut p = [0u8; 8];
        p[..4].copy_from_slice(&code.to_be_bytes());
        p[4..].copy_from_slice(&(down as u32).to_be_bytes());
        self.w.write_all(&frame(KEY, &p)).await
    }

    /// A pointer at normalized (`fx`, `fy`) in 0.0..=1.0, mapped to the unit's pixels (see [`crate::screen`]).
    pub async fn pointer(
        &mut self,
        fx: f64,
        fy: f64,
        phase: crate::screen::Phase,
    ) -> std::io::Result<()> {
        let (w, h) = (self.info.width.max(1), self.info.height.max(1));
        let x = (fx.clamp(0.0, 1.0) * (w - 1) as f64).round() as u16;
        let y = (fy.clamp(0.0, 1.0) * (h - 1) as f64).round() as u16;
        self.touch(x, y, phase.into()).await
    }

    /// A hardkey by name; one the unit does not have is ignored.
    pub async fn key_named(&mut self, name: &str, down: bool) -> std::io::Result<()> {
        match self.info.key(name) {
            Some(code) => self.key(code, down).await,
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_vulcan_session_reply() {
        // a B&G Vulcan 7R's reply
        let hex = "000012340056756c63616e203752000000000000000000000000000000000000000000000076756c63616e37\
                   00000000000000000000000000000000000000000000000000302e302e300000000000000000000000000000\
                   000000000200000001000000190000000500000010032001e0032001e000000010";
        let b: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let i = parse_session_reply(&b).unwrap();
        assert_eq!(
            (i.model.as_str(), i.name.as_str()),
            ("Vulcan 7R", "vulcan7")
        );
        assert_eq!(i.firmware, "0.0.0");
        assert_eq!(i.keys, vec![("page".into(), 25), ("power".into(), 16)]);
        assert_eq!((i.width, i.height), (800, 480));
        assert_eq!(i.brand(), "bandg");
    }

    #[test]
    fn frames() {
        assert_eq!(frame(SESSION, &[0, 0, 0, 7]), vec![0, 6, 0, 1, 0, 0, 0, 7]);
    }
}
