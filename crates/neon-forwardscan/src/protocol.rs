//! ForwardScan of a NOS sonar device (a chartplotter with a ForwardScan sensor, or a SonarHub),
//! decoded from network captures.
//!
//! Data: broadcast on UDP 10755 (to 255.255.255.255) while the sonar's ForwardScan sensor is in
//! ForwardScan mode. One ping is ~16 kB split over 12 datagrams of at most 1400 bytes, each with a
//! 30-byte header (little-endian):
//!   [0] 0x28 [1] 0x06 [2..3] u16 header length 30
//!   [4..7]  u32 total payload length of the whole ping (sum of all fragments' payloads)
//!   [10..13] u32 ping sequence (steps by 2: the settings blocks on 10756 share the counter)
//!   [14..17] u32 fragment index (0..)
//!   [18..21] u32 length of this fragment's payload (1370, the last one is shorter)
//!   [22..25] f32 current depth in feet
//!   [26..29] u32 byte length of the bottom profile block at the start of the payload
//! Payload: the bottom profile as f32 pairs (forward range ft, depth ft), then the echo block (15517 bytes):
//!   u16 0x1f16, 512 bytes (zero), u16 count (3000), u8, then count records of 5 bytes:
//!   s16 angle (1/512 degree, relative to the transducer angle), u16 range cell (0.0233327 ft), u8 amplitude.
//!
//! Settings: a 104-byte block on UDP 10756 (little-endian, unaligned). The sonar broadcasts its own
//! (byte 1 = 1) every ~2 s; a display changes a setting by sending the whole block back to the sonar's
//! 169.254 address with byte 1 = 2 (its 10.x address ignores it). Verified live.
//!   [0] 0x28  [1] 1 = from the sonar, 2 = from a display  [2..5] u32 sequence  [6..11] ff*6
//!   [0x0c] f32 depth shown, ft   [0x10] f32 forward range shown, ft   [0x14] u8 running (0 = paused)
//!   [0x15] i32 noise rejection 0-3   [0x19] u8 auto range   [0x1a] u8 auto gain   [0x1b] u8 auto noise rejection
//!   [0x1c] f32 critical depth ft  [0x20] f32 critical range ft  [0x47] f32 transducer angle (degrees)
//!   the rest: calibration and sensor debug parameters, copied unchanged.

use serde::Serialize;
use std::collections::HashMap;
use std::net::Ipv4Addr;

pub const PORT: u16 = 10755;
pub const CONTROL_PORT: u16 = 10756;
pub const NOISE_LEVELS: [&str; 4] = ["off", "low", "medium", "high"];

const HEADER: usize = 30;
const CONTROL_LEN: usize = 104;
const ECHO_COUNT: usize = 0x202; // offset in the echo block of the record count; the records start at 0x205
const MAX_PENDING: usize = 8; // pings being reassembled

fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn f32_at(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

pub struct Ping {
    pub seq: u32,
    /// Depth under the boat in feet (like everything here: the sonar's unit), 0 when unknown.
    pub depth: f32,
    /// The bottom ahead: (forward ft, depth ft).
    pub points: Vec<(f32, f32)>,
    /// The echo records as the sonar sent them, 5 bytes each.
    pub echo: Vec<u8>,
}

pub fn is_fragment(buf: &[u8]) -> bool {
    buf.len() > HEADER && buf[0..4] == [0x28, 0x06, 0x1e, 0x00]
}

struct Pending {
    source: Ipv4Addr,
    seq: u32,
    total: usize,
    header: Option<Vec<u8>>,
    parts: HashMap<u32, Vec<u8>>,
}

#[derive(Default)]
pub struct Reassembler {
    pending: Vec<Pending>, // oldest first
}

impl Reassembler {
    /// Feed one datagram; returns the decoded ping once its last fragment arrives.
    pub fn push(&mut self, buf: &[u8], source: Ipv4Addr) -> Option<Ping> {
        if !is_fragment(buf) {
            return None;
        }
        let total = u32_at(buf, 4) as usize;
        let seq = u32_at(buf, 10);
        let idx = u32_at(buf, 14);
        let len = u32_at(buf, 18) as usize;
        if buf.len() < HEADER + len || total == 0 || total > 65000 {
            return None;
        }
        let i = match self
            .pending
            .iter()
            .position(|p| p.source == source && p.seq == seq)
        {
            Some(i) => i,
            None => {
                self.pending.push(Pending {
                    source,
                    seq,
                    total,
                    header: None,
                    parts: HashMap::new(),
                });
                if self.pending.len() > MAX_PENDING {
                    self.pending.remove(0);
                }
                self.pending.len() - 1
            }
        };
        let p = &mut self.pending[i];
        p.parts.insert(idx, buf[HEADER..HEADER + len].to_vec());
        if idx == 0 {
            p.header = Some(buf[..HEADER].to_vec());
        }
        let have: usize = p.parts.values().map(|v| v.len()).sum();
        if have < p.total || p.header.is_none() {
            return None;
        }
        let p = self.pending.remove(i);
        let mut keys: Vec<_> = p.parts.keys().copied().collect();
        keys.sort_unstable();
        let payload: Vec<u8> = keys
            .iter()
            .flat_map(|k| p.parts[k].iter().copied())
            .collect();
        Some(decode_ping(&p.header.unwrap(), &payload, seq))
    }
}

pub fn decode_ping(header: &[u8], payload: &[u8], seq: u32) -> Ping {
    let depth_ft = f32_at(header, 22);
    let blk = (u32_at(header, 26) as usize).min(payload.len());
    let mut points = Vec::new();
    let mut o = 0;
    while o + 8 <= blk {
        let (x, y) = (f32_at(payload, o), f32_at(payload, o + 4));
        if !((0.0..1000.0).contains(&x) && (0.0..1000.0).contains(&y)) {
            break; // not a plausible point: end of the profile
        }
        points.push((x, y));
        o += 8;
    }
    let mut echo = Vec::new();
    if payload.len() >= blk + ECHO_COUNT + 3 {
        let n =
            u16::from_le_bytes([payload[blk + ECHO_COUNT], payload[blk + ECHO_COUNT + 1]]) as usize;
        let start = blk + ECHO_COUNT + 3;
        let n = n.min((payload.len() - start) / 5);
        echo = payload[start..start + n * 5].to_vec();
    }
    let depth = if depth_ft > 0.0 && depth_ft < 1000.0 {
        depth_ft
    } else {
        0.0
    };
    Ping {
        seq,
        depth,
        points,
        echo,
    }
}

pub fn is_control(buf: &[u8]) -> bool {
    buf.len() == CONTROL_LEN && buf[0] == 0x28 && buf[1] & 3 != 0
}

#[derive(Serialize, Debug, PartialEq, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub from_sonar: bool,
    pub running: bool,
    pub depth: f64,
    pub range: f64,
    pub auto_range: bool,
    pub noise_rejection: serde_json::Value,
    pub auto_noise_rejection: bool,
    pub critical_depth: f64,
    pub critical_range: f64,
    pub transducer_angle: f64,
}

fn round(v: f32, digits: i32) -> f64 {
    let f = 10f64.powi(digits);
    let v = v as f64;
    (v * f).round() / f
}

pub fn parse_control(buf: &[u8]) -> Option<Settings> {
    if !is_control(buf) {
        return None;
    }
    let ft = |o| round(f32_at(buf, o), 2);
    let nr = i32::from_le_bytes(buf[0x15..0x19].try_into().unwrap());
    Some(Settings {
        from_sonar: buf[1] & 1 == 1,
        running: buf[0x14] != 0,
        depth: ft(0x0c),
        range: ft(0x10),
        auto_range: buf[0x19] != 0,
        noise_rejection: match NOISE_LEVELS.get(nr as usize) {
            Some(s) if nr >= 0 => (*s).into(),
            _ => nr.into(),
        },
        auto_noise_rejection: buf[0x1b] != 0,
        critical_depth: ft(0x1c),
        critical_range: ft(0x20),
        transducer_angle: round(f32_at(buf, 0x47), 2),
    })
}

/// A change requested by the web page / REST API. Depth and range in feet; a fixed depth or
/// range turns auto range off. `noise_rejection`: "auto" or one of NOISE_LEVELS.
#[derive(serde::Deserialize, Default, Debug)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Change {
    pub source: Option<String>,
    pub paused: Option<bool>,
    pub depth: Option<f32>,
    pub range: Option<f32>,
    pub auto_range: Option<bool>,
    pub noise_rejection: Option<String>,
}

impl Change {
    pub fn fields(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.paused.is_some() {
            v.push("paused")
        }
        if self.depth.is_some() {
            v.push("depth")
        }
        if self.range.is_some() {
            v.push("range")
        }
        if self.auto_range.is_some() {
            v.push("autoRange")
        }
        if self.noise_rejection.is_some() {
            v.push("noiseRejection")
        }
        v
    }
}

/// A display's block: the sonar's latest one with the change applied. The sonar does not
/// range-check what it gets (it took noise rejection 4), so this does.
pub fn build_control(latest: &[u8], change: &Change) -> Result<Vec<u8>, String> {
    if !is_control(latest) {
        return Err("no ForwardScan settings from this sonar yet".into());
    }
    let mut b = latest.to_vec();
    b[1] = 0x02;
    let feet = |name: &str, v: f32, max: f32| -> Result<[u8; 4], String> {
        if !(v >= 3.0 && v <= max) {
            return Err(format!("{name} must be 3 to {max} ft"));
        }
        Ok(v.to_le_bytes())
    };
    if let Some(p) = change.paused {
        b[0x14] = if p { 0 } else { 1 };
    }
    if let Some(d) = change.depth {
        b[0x0c..0x10].copy_from_slice(&feet("depth", d, 330.0)?);
        b[0x19] = 0;
    }
    if let Some(r) = change.range {
        b[0x10..0x14].copy_from_slice(&feet("range", r, 660.0)?);
        b[0x19] = 0;
    }
    if let Some(a) = change.auto_range {
        b[0x19] = a as u8;
    }
    if let Some(n) = &change.noise_rejection {
        if n == "auto" {
            b[0x1b] = 1;
        } else {
            let i = NOISE_LEVELS.iter().position(|l| l == n).ok_or_else(|| {
                format!("noiseRejection must be auto, {}", NOISE_LEVELS.join(", "))
            })?;
            b[0x15..0x19].copy_from_slice(&(i as i32).to_le_bytes());
            b[0x1b] = 0;
        }
    }
    Ok(b)
}

/// The binary WebSocket frame the page draws (same as the Signal K plugin's, 20-byte header):
///   [0x02][0][4B src IPv4][u32 seq][f32 depth ft][u16 nPoints][u16 nEcho][2 bytes 0]
///   [nPoints x (f32 forward ft, f32 depth ft)][nEcho x 5-byte echo records as the sonar sent them]
pub fn pack_frame(source: Ipv4Addr, p: &Ping) -> Vec<u8> {
    let mut b = Vec::with_capacity(20 + p.points.len() * 8 + p.echo.len());
    b.extend_from_slice(&[0x02, 0]);
    b.extend_from_slice(&source.octets());
    b.extend_from_slice(&p.seq.to_le_bytes());
    b.extend_from_slice(&p.depth.to_le_bytes());
    b.extend_from_slice(&(p.points.len() as u16).to_le_bytes());
    b.extend_from_slice(&((p.echo.len() / 5) as u16).to_le_bytes());
    b.extend_from_slice(&[0, 0]);
    for (x, y) in &p.points {
        b.extend_from_slice(&x.to_le_bytes());
        b.extend_from_slice(&y.to_le_bytes());
    }
    b.extend_from_slice(&p.echo);
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    // settings block as a NOS sonar device broadcast it: 35 x 110 ft, auto range, noise rejection 2 (auto)
    const LIVE: &str = "28012a1f0100ffffffffffff00000c420000dc420102000000010101000020410000c84200000000000000000000000000010047000000640000004100000000000000000000000000f04100000000000000000000000000000000c8000000000000000000000000";

    #[test]
    fn control_parse_and_build() {
        let live = hex(LIVE);
        let st = parse_control(&live).unwrap();
        assert_eq!(
            st,
            Settings {
                from_sonar: true,
                running: true,
                depth: 35.0,
                range: 110.0,
                auto_range: true,
                noise_rejection: "medium".into(),
                auto_noise_rejection: true,
                critical_depth: 10.0,
                critical_range: 100.0,
                transducer_angle: 30.0,
            }
        );
        let b = build_control(
            &live,
            &Change {
                range: Some(50.0),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(b[1], 2);
        assert_eq!(b.len(), 104);
        let s = parse_control(&b).unwrap();
        assert_eq!(
            (s.from_sonar, s.range, s.auto_range, s.depth),
            (false, 50.0, false, 35.0)
        );
        assert_eq!(b[0x1c..], live[0x1c..]); // calibration and sensor parameters untouched

        let b = build_control(
            &live,
            &Change {
                paused: Some(true),
                noise_rejection: Some("high".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let s = parse_control(&b).unwrap();
        assert_eq!(
            (
                s.running,
                s.noise_rejection.as_str(),
                s.auto_noise_rejection
            ),
            (false, Some("high"), false)
        );
        let b = build_control(
            &b,
            &Change {
                noise_rejection: Some("auto".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(parse_control(&b).unwrap().auto_noise_rejection);

        assert!(build_control(
            &live,
            &Change {
                noise_rejection: Some("4".into()),
                ..Default::default()
            }
        )
        .is_err());
        assert!(build_control(
            &live,
            &Change {
                depth: Some(0.0),
                ..Default::default()
            }
        )
        .is_err());
        assert!(build_control(
            &[],
            &Change {
                depth: Some(5.0),
                ..Default::default()
            }
        )
        .is_err());
    }

    fn fragments(seq: u32, depth_ft: f32, pts: &[(f32, f32)], echo_blk: &[u8]) -> Vec<Vec<u8>> {
        let mut payload: Vec<u8> = pts
            .iter()
            .flat_map(|(x, y)| [x.to_le_bytes(), y.to_le_bytes()].concat())
            .collect();
        let blk_len = payload.len() as u32;
        payload.extend_from_slice(echo_blk);
        payload
            .chunks(1370)
            .enumerate()
            .map(|(i, part)| {
                let mut h = vec![0u8; 30];
                h[..4].copy_from_slice(&[0x28, 0x06, 0x1e, 0x00]);
                h[4..8].copy_from_slice(&(payload.len() as u32).to_le_bytes());
                h[10..14].copy_from_slice(&seq.to_le_bytes());
                h[14..18].copy_from_slice(&(i as u32).to_le_bytes());
                h[18..22].copy_from_slice(&(part.len() as u32).to_le_bytes());
                h[22..26].copy_from_slice(&depth_ft.to_le_bytes());
                h[26..30].copy_from_slice(&blk_len.to_le_bytes());
                [h, part.to_vec()].concat()
            })
            .collect()
    }

    #[test]
    fn reassemble_ping() {
        let pts = [(16.25, 18.75), (16.5, 18.8), (82.0, 20.0)];
        let mut echo = vec![0u8; 15100];
        echo[..2].copy_from_slice(&0x1f16u16.to_le_bytes());
        echo[0x202..0x204].copy_from_slice(&3u16.to_le_bytes());
        echo[0x204] = 2;
        for (i, (a, r, c)) in [
            (-512i16, 100u16, 200u8),
            (0, 2000, 94),
            (5 * 512, 8000, 255),
        ]
        .iter()
        .enumerate()
        {
            let o = 0x205 + i * 5;
            echo[o..o + 2].copy_from_slice(&a.to_le_bytes());
            echo[o + 2..o + 4].copy_from_slice(&r.to_le_bytes());
            echo[o + 4] = *c;
        }
        let mut frags = fragments(100, 19.5, &pts, &echo);
        frags.reverse(); // out of order
        let src = Ipv4Addr::new(169, 254, 6, 133);
        let mut r = Reassembler::default();
        let mut got = None;
        for f in &frags {
            assert!(got.is_none());
            got = r.push(f, src);
        }
        let p = got.expect("a ping");
        assert_eq!(p.seq, 100);
        assert!((p.depth - 19.5).abs() < 1e-4);
        assert_eq!(p.points.len(), 3);
        assert!((p.points[2].0 - 82.0).abs() < 1e-4);
        assert_eq!(p.echo.len(), 15);
        assert_eq!(&p.echo[..5], &[0x00, 0xfe, 100, 0, 200]);

        let f = pack_frame(src, &p);
        assert_eq!(f.len(), 20 + 3 * 8 + 15);
        assert_eq!(&f[..6], &[2, 0, 169, 254, 6, 133]);
        assert_eq!(u16::from_le_bytes([f[16], f[17]]), 3);
    }
}
