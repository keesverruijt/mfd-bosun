//! The vendor-neutral view of a chartplotter's remote screen: what it is ([`Screen`]) and a live control
//! connection to it ([`Control`]). The bridge in `mfd-remote` drives Navico ([`crate::nos`]), Garmin
//! ([`crate::garmin`]) and Raymarine ([`crate::raymarine`]) plotters — and, in time, others — through just
//! these.
//!
//! All share a screen as an RTSP H.264 stream and take input over a vendor TCP protocol. What differs is the
//! RTSP URL, the input wire format, the coordinate space (Navico uses screen pixels; Garmin and Raymarine a
//! 0..65535 normalization) and the access model — all hidden here. The bridge works in normalized coordinates
//! (0.0..=1.0 across the picture); each vendor maps them to its own space.

use crate::{garmin, nos, raymarine};
use std::net::{IpAddr, SocketAddr};
use tokio::sync::watch;

/// A pointer's phase within a gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Down,
    Move,
    Up,
}

/// How the plotter's RTSP server wants its RTP carried. Navico and Raymarine interleave it over the RTSP TCP
/// connection; Garmin serves only over UDP (it rejects interleaved SETUP with 461 Unsupported Transport).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtspTransport {
    Tcp,
    Udp,
}

/// Whether the plotter lets us send input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Allowed,
    /// The user has to allow it on the plotter's screen first (Navico).
    Waiting,
}

/// Who we tell the plotter we are when connecting for control (a Navico unit shows and remembers the MAC;
/// Garmin and Raymarine use neither).
#[derive(Clone)]
pub struct Client {
    pub name: String,
    pub mac: [u8; 6],
}

/// What a discovered screen is, with what the probe learned. Cloned and compared by discovery, so it carries
/// only the small identity, not the connection.
#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    Navico(nos::Info),
    Garmin(garmin::Info),
    Raymarine(raymarine::Info),
}

impl Screen {
    /// A short tag for the page: "nos", "garmin" or "raymarine".
    pub fn kind(&self) -> &'static str {
        match self {
            Screen::Navico(_) => "nos",
            Screen::Garmin(_) => "garmin",
            Screen::Raymarine(_) => "raymarine",
        }
    }

    pub fn brand(&self) -> &str {
        match self {
            Screen::Navico(i) => i.brand(),
            Screen::Garmin(i) => i.brand(),
            Screen::Raymarine(i) => i.brand(),
        }
    }

    pub fn model(&self) -> &str {
        match self {
            Screen::Navico(i) => &i.model,
            Screen::Garmin(i) => &i.model,
            Screen::Raymarine(i) => &i.model,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Screen::Navico(i) => &i.name,
            Screen::Garmin(i) => &i.name,
            Screen::Raymarine(i) => &i.name,
        }
    }

    /// Screen size in pixels, for the picture's aspect ratio and coordinate mapping.
    pub fn size(&self) -> (u16, u16) {
        match self {
            Screen::Navico(i) => (i.width, i.height),
            Screen::Garmin(i) => (i.width, i.height),
            Screen::Raymarine(i) => (i.width, i.height),
        }
    }

    /// How this plotter's RTSP server wants its RTP carried.
    pub fn rtsp_transport(&self) -> RtspTransport {
        match self {
            Screen::Navico(_) => RtspTransport::Tcp,
            Screen::Garmin(_) => RtspTransport::Udp,
            Screen::Raymarine(_) => RtspTransport::Tcp,
        }
    }

    /// The RTSP URL of the screen at `ip`.
    pub fn rtsp_url(&self, ip: IpAddr) -> String {
        match self {
            Screen::Navico(_) => {
                format!(
                    "rtsp://{}/screenmirror",
                    SocketAddr::new(ip, nos::RTSP_PORT)
                )
            }
            Screen::Garmin(i) => i.rtsp_url(ip),
            Screen::Raymarine(i) => i.rtsp_url(ip),
        }
    }

    /// The hardkeys this plotter exposes, by name (Garmin Helm and Raymarine: none yet).
    pub fn keys(&self) -> Vec<String> {
        match self {
            Screen::Navico(i) => i.keys.iter().map(|(n, _)| n.clone()).collect(),
            Screen::Garmin(_) => Vec::new(),
            Screen::Raymarine(_) => Vec::new(),
        }
    }

    /// Opens a control connection at `ip`, as `client`.
    pub async fn connect(&self, ip: IpAddr, client: &Client) -> std::io::Result<Control> {
        match self {
            Screen::Navico(_) => {
                let addr = SocketAddr::new(ip, nos::PORT);
                nos::Control::connect(addr, client.mac, &client.name)
                    .await
                    .map(Control::Navico)
            }
            Screen::Garmin(_) => garmin::Control::connect(ip).await.map(Control::Garmin),
            Screen::Raymarine(_) => raymarine::Control::connect(ip)
                .await
                .map(Control::Raymarine),
        }
    }
}

/// A live control connection: the bridge sends normalized pointer events and keys.
pub enum Control {
    Navico(nos::Control),
    Garmin(garmin::Control),
    Raymarine(raymarine::Control),
}

impl Control {
    /// A pointer at normalized (`fx`, `fy`) in 0.0..=1.0 across the picture.
    pub async fn pointer(&mut self, fx: f64, fy: f64, phase: Phase) -> std::io::Result<()> {
        match self {
            Control::Navico(c) => c.pointer(fx, fy, phase).await,
            Control::Garmin(c) => c.pointer(fx, fy, phase).await,
            Control::Raymarine(c) => c.pointer(fx, fy, phase).await,
        }
    }

    /// A hardkey by name, down or up (a plotter without that key ignores it).
    pub async fn key(&mut self, name: &str, down: bool) -> std::io::Result<()> {
        match self {
            Control::Navico(c) => c.key_named(name, down).await,
            Control::Garmin(c) => c.key(name, down).await,
            Control::Raymarine(c) => c.key(name, down).await,
        }
    }

    /// Changes as the plotter reports whether input is allowed.
    pub fn access(&self) -> watch::Receiver<Option<Access>> {
        match self {
            Control::Navico(c) => c.access.clone(),
            Control::Garmin(c) => c.access(),
            Control::Raymarine(c) => c.access(),
        }
    }
}
