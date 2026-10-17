//! Raymarine Axiom (LightHouse) remote screen and control.
//!
//! Discovered over mDNS: an Axiom advertises `_rtsp._tcp` with TXT `raymarine-mfd-serial`,
//! `raymarine-mfd-model` and `raymarine-mfd-rtsp-path`. The screen is an RTSP H.264 stream, like a
//! Navico NOS unit (GStreamer, interleaved over TCP), at `rtsp://<ip>:8554/<path>`. Input goes to the
//! "Raymarine Remote Control" service (`_rym_rrc._tcp`) on [`RRC_PORT`] as 15-byte `ECRR` messages
//! carrying a down/move/up phase and an (x, y) coordinate normalized 0..65535 across the screen.
//!
//! The unit's "Mobile app access" setting gates this: "Viewing only" serves RTSP but closes
//! [`RRC_PORT`] (no control); "Remote control" opens both. Only one controller at a time.

use std::net::{IpAddr, SocketAddr};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::OwnedWriteHalf;
use tokio::sync::watch;

use crate::screen::{Access, Phase};

/// The Raymarine Remote Control service's input port (`_rym_rrc._tcp`).
pub const RRC_PORT: u16 = 50000;
/// The default RTSP port of the screen stream (the SRV record confirms it).
pub const RTSP_PORT: u16 = 8554;
/// The mDNS service to browse; a Raymarine screen is one whose TXT has `raymarine-mfd-serial`.
pub const SERVICE_TYPE: &str = "_rtsp._tcp.local.";

const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(4);

/// What a discovered Axiom screen is, from its mDNS TXT and the H.264 SPS.
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub model: String,
    pub name: String,
    pub serial: String,
    pub width: u16,
    pub height: u16,
    pub rtsp_port: u16,
    pub rtsp_path: String,
}

impl Info {
    pub fn brand(&self) -> &'static str {
        "raymarine"
    }

    pub fn rtsp_url(&self, ip: IpAddr) -> String {
        format!(
            "rtsp://{}/{}",
            SocketAddr::new(ip, self.rtsp_port),
            self.rtsp_path
        )
    }
}

/// A normalized (0.0..=1.0) coordinate as the wire u16 (0..65535 across the screen).
fn coord(f: f64) -> u16 {
    (f.clamp(0.0, 1.0) * 65535.0).round() as u16
}

/// One 15-byte `ECRR` input message: phase + (x, y).
fn ecrr(phase: Phase, x: u16, y: u16) -> [u8; 15] {
    let ev = match phase {
        Phase::Down => 0x01,
        Phase::Move => 0x02,
        Phase::Up => 0x03,
    };
    let mut p = [0u8; 15];
    p[0..4].copy_from_slice(b"ECRR");
    p[4..9].copy_from_slice(&[0x01, 0x0a, 0x03, 0x06, 0x00]);
    p[9] = ev;
    p[10] = 0x00;
    p[11..13].copy_from_slice(&x.to_le_bytes());
    p[13..15].copy_from_slice(&y.to_le_bytes());
    p
}

/// A live control connection to the Remote Control service on [`RRC_PORT`].
pub struct Control {
    w: OwnedWriteHalf,
    pub access: watch::Receiver<Option<Access>>,
}

impl Control {
    /// Connects to the Remote Control input port. Succeeds only when the unit is in "Remote control"
    /// mode (in "Viewing only" the port is closed and this fails; the screen still views).
    pub async fn connect(ip: IpAddr) -> std::io::Result<Control> {
        let addr = SocketAddr::new(ip, RRC_PORT);
        let s = tokio::time::timeout(CONNECT_TIMEOUT, crate::link::connect(addr))
            .await
            .map_err(|_| std::io::Error::other("no answer from the Remote Control port"))??;
        s.set_nodelay(true)?;
        let (mut r, w) = s.into_split();
        // Reaching the port means control is on (it is exclusive, with no further negotiation). Hold
        // the access sender in a task that reads the connection: the unit sends nothing we need, so a
        // read ending means it closed the session (another controller took it, or control turned off),
        // and dropping the sender then tells the bridge.
        let (tx, access) = watch::channel(None);
        tokio::spawn(async move {
            let _ = tx.send(Some(Access::Allowed));
            let mut buf = [0u8; 256];
            while matches!(r.read(&mut buf).await, Ok(n) if n > 0) {}
        });
        Ok(Control { w, access })
    }

    /// A pointer at normalized (`fx`, `fy`) in 0.0..=1.0 across the picture.
    pub async fn pointer(&mut self, fx: f64, fy: f64, phase: Phase) -> std::io::Result<()> {
        self.w.write_all(&ecrr(phase, coord(fx), coord(fy))).await
    }

    /// Hardkeys are not yet mapped for Raymarine; ignored.
    pub async fn key(&mut self, _name: &str, _down: bool) -> std::io::Result<()> {
        Ok(())
    }

    pub fn access(&self) -> watch::Receiver<Option<Access>> {
        self.access.clone()
    }
}

/// Builds [`Info`] from an Axiom's mDNS `_rtsp` TXT, if it is a Raymarine MFD. `txt` looks up a TXT
/// value by key; `srv_port` is the service's SRV port. Size is filled in later by [`screen_size`].
pub fn info_from_txt(
    txt: impl Fn(&str) -> Option<String>,
    srv_port: u16,
) -> Option<(String, Info)> {
    let serial = txt("raymarine-mfd-serial")?;
    if serial.is_empty() {
        return None;
    }
    let model = txt("raymarine-mfd-model").unwrap_or_default();
    let path = txt("raymarine-mfd-rtsp-path").unwrap_or_else(|| "RAYMARINEMFD".to_string());
    let info = Info {
        name: if model.is_empty() {
            serial.clone()
        } else {
            model.clone()
        },
        model,
        serial: serial.clone(),
        width: 0,
        height: 0,
        rtsp_port: if srv_port != 0 { srv_port } else { RTSP_PORT },
        rtsp_path: path,
    };
    Some((serial, info))
}

/// RTSP `DESCRIBE`s the screen and returns its pixel size from the H.264 SPS. Used to fill in
/// [`Info::width`]/[`Info::height`], and doubles as a liveness probe.
pub async fn screen_size(ip: IpAddr, port: u16, path: &str) -> std::io::Result<(u16, u16)> {
    use tokio::io::AsyncReadExt;
    let addr = SocketAddr::new(ip, port);
    let mut s = tokio::time::timeout(CONNECT_TIMEOUT, crate::link::connect(addr))
        .await
        .map_err(|_| std::io::Error::other("no answer"))??;
    let req = format!(
        "DESCRIBE rtsp://{addr}/{path} RTSP/1.0\r\nCSeq: 1\r\nAccept: application/sdp\r\n\r\n"
    );
    s.write_all(req.as_bytes()).await?;
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        let n = tokio::time::timeout(CONNECT_TIMEOUT, s.read(&mut tmp))
            .await
            .map_err(|_| std::io::Error::other("no answer"))??;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() > 200 {
            break;
        }
        if buf.len() > 16384 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let sps = text
        .split("sprop-parameter-sets=")
        .nth(1)
        .and_then(|s| s.split([',', ';', '\r', '\n']).next())
        .ok_or_else(|| std::io::Error::other("no sprop-parameter-sets in SDP"))?;
    let sps = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, sps.trim())
        .map_err(std::io::Error::other)?;
    sps_resolution(&sps).ok_or_else(|| std::io::Error::other("could not parse the SPS"))
}

/// Width and height from an H.264 SPS NAL (handles the baseline/main/high profiles Axioms use).
fn sps_resolution(nal: &[u8]) -> Option<(u16, u16)> {
    let data = nal.strip_prefix(&[0]).unwrap_or(nal);
    // skip a NAL start code / header byte if present (type 7 = SPS)
    let data = if data.first()? & 0x1f == 7 {
        &data[1..]
    } else {
        data
    };
    let mut br = BitReader::new(data);
    let profile = br.u(8)?;
    br.u(8)?; // constraint flags + reserved
    br.u(8)?; // level
    br.ue()?; // seq_parameter_set_id
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        let chroma = br.ue()?;
        if chroma == 3 {
            br.u(1)?;
        }
        br.ue()?; // bit_depth_luma
        br.ue()?; // bit_depth_chroma
        br.u(1)?; // qpprime
        if br.u(1)? == 1 {
            return None; // scaling lists present: not handled (Axioms do not use them)
        }
    }
    br.ue()?; // log2_max_frame_num
    let poc = br.ue()?;
    if poc == 0 {
        br.ue()?;
    } else if poc == 1 {
        br.u(1)?;
        br.se()?;
        br.se()?;
        let n = br.ue()?;
        for _ in 0..n {
            br.se()?;
        }
    }
    br.ue()?; // max_num_ref_frames
    br.u(1)?; // gaps_in_frame_num
    let wmbs = br.ue()? + 1;
    let hmap = br.ue()? + 1;
    let frame_mbs_only = br.u(1)?;
    if frame_mbs_only == 0 {
        br.u(1)?;
    }
    br.u(1)?; // direct_8x8
    let (mut cl, mut cr, mut ct, mut cb) = (0u64, 0u64, 0u64, 0u64);
    if br.u(1)? == 1 {
        cl = br.ue()?;
        cr = br.ue()?;
        ct = br.ue()?;
        cb = br.ue()?;
    }
    let interlace = 2 - frame_mbs_only;
    let w = wmbs * 16 - (cl + cr) * 2;
    let h = hmap * 16 * interlace - (ct + cb) * 2 * interlace;
    Some((w.try_into().ok()?, h.try_into().ok()?))
}

/// A minimal MSB-first bit reader for exp-Golomb SPS parsing.
struct BitReader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(b: &'a [u8]) -> Self {
        BitReader { b, pos: 0 }
    }
    fn u(&mut self, n: usize) -> Option<u64> {
        let mut v = 0u64;
        for _ in 0..n {
            let byte = *self.b.get(self.pos >> 3)?;
            v = (v << 1) | ((byte >> (7 - (self.pos & 7))) & 1) as u64;
            self.pos += 1;
        }
        Some(v)
    }
    fn ue(&mut self) -> Option<u64> {
        let mut z = 0;
        while self.u(1)? == 0 {
            z += 1;
            if z > 31 {
                return None;
            }
        }
        Some((1u64 << z) - 1 + if z > 0 { self.u(z)? } else { 0 })
    }
    fn se(&mut self) -> Option<i64> {
        let k = self.ue()?;
        Some(if k % 2 == 1 {
            k.div_ceil(2) as i64
        } else {
            -((k / 2) as i64)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecrr_touch_message_matches_the_capture() {
        // the capture's wire bytes `e5 82 32 f3` are the coordinate little-endian: x=0x82e5, y=0xf332
        let m = ecrr(Phase::Down, 0x82e5, 0xf332);
        assert_eq!(&m[0..4], b"ECRR");
        assert_eq!(&m[4..11], &[0x01, 0x0a, 0x03, 0x06, 0x00, 0x01, 0x00]);
        assert_eq!(&m[11..15], &[0xe5, 0x82, 0x32, 0xf3]);
        // UP of the same tap keeps the coordinate, changes only the phase byte
        assert_eq!(ecrr(Phase::Up, 0x82e5, 0xf332)[9], 0x03);
        assert_eq!(ecrr(Phase::Move, 0, 0)[9], 0x02);
    }

    #[test]
    fn coord_normalizes_to_u16() {
        assert_eq!(coord(0.0), 0);
        assert_eq!(coord(1.0), 65535);
        assert_eq!(coord(0.5), 32768);
        assert_eq!(coord(2.0), 65535); // clamped
    }

    #[test]
    fn parses_an_axiom_sps_resolution() {
        // the Axiom+ 7 RV's H.264 SPS (base64 from the RTSP SDP) -> 1024x600
        let sps = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            "J0LgH41oBABN+WEAAAMAAQAAAwAUDxB6gA==",
        )
        .unwrap();
        assert_eq!(sps_resolution(&sps), Some((1024, 600)));
    }

    #[test]
    fn info_from_a_raymarine_txt() {
        let txt = |k: &str| match k {
            "raymarine-mfd-serial" => Some("E70635 0660091".to_string()),
            "raymarine-mfd-model" => Some("AXIOM PLUS 7 RV".to_string()),
            "raymarine-mfd-rtsp-path" => Some("RAYMARINEMFD".to_string()),
            _ => None,
        };
        let (serial, info) = info_from_txt(txt, 8554).unwrap();
        assert_eq!(serial, "E70635 0660091");
        assert_eq!(info.model, "AXIOM PLUS 7 RV");
        assert_eq!(info.rtsp_path, "RAYMARINEMFD");
        assert_eq!(info.rtsp_port, 8554);
    }
}
