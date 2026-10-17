//! The ForwardScan UDP side: pings and settings from the sonar, and sending a settings change back.
//! Finding the chartplotters, advertising the page and keeping a 169.254 address are general and
//! live in [`mfd_net::link`].

use serde_json::json;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::net::SocketAddrV4;
use std::time::Instant;
use tokio::net::UdpSocket;

use mfd_net::link::{self, v4};

use crate::protocol;
use crate::state::{Out, State};

/// The sockets of the three receivers, bound up front so that a failure shows at start.
pub struct Sockets {
    pub pings: UdpSocket,
    pub settings: UdpSocket,
    pub announcements: UdpSocket,
}

impl Sockets {
    pub fn bind() -> std::io::Result<Self> {
        let bind = |what: &str, s: std::io::Result<UdpSocket>| {
            s.map_err(|e| std::io::Error::new(e.kind(), format!("{what}: {e}")))
        };
        Ok(Sockets {
            pings: bind("ForwardScan", link::shared_socket(protocol::PORT))?,
            settings: bind(
                "ForwardScan settings",
                link::shared_socket(protocol::CONTROL_PORT),
            )?,
            announcements: bind("chartplotter announcements", link::announcement_socket())?,
        })
    }
}

pub async fn receive_pings(state: State, sock: UdpSocket) -> std::io::Result<()> {
    let mut r = protocol::Reassembler::default();
    let mut buf = vec![0u8; 2048];
    loop {
        let (n, from) = sock.recv_from(&mut buf).await?;
        let Some(src) = v4(from) else { continue };
        let Some(ping) = r.push(&buf[..n], src) else {
            continue;
        };
        let new = state
            .lock()
            .last_ping
            .insert(src, Instant::now())
            .is_none_or(|t| t.elapsed() > crate::state::DATA_TTL);
        if new {
            log::info!("ForwardScan from {} ({})", state.name_of(src), src);
            state.refresh_sources();
        }
        let _ = state
            .tx
            .send(Out::Frame(protocol::pack_frame(src, &ping).into()));
    }
}

pub async fn receive_settings(state: State, sock: UdpSocket) -> std::io::Result<()> {
    let mut buf = vec![0u8; 2048];
    loop {
        let (n, from) = sock.recv_from(&mut buf).await?;
        let Some(src) = v4(from) else { continue };
        let b = &buf[..n];
        if !protocol::is_control(b) || b[1] & 1 == 0 {
            continue; // only a sonar's own block, not a display's request
        }
        let settings = protocol::parse_control(b);
        let prev = state.lock().blocks.insert(src, b.to_vec());
        if prev.is_none() {
            log::info!("ForwardScan settings from {} ({src})", state.name_of(src));
            state.refresh_sources();
        }
        if prev.and_then(|p| protocol::parse_control(&p)) != settings {
            state.send_text(json!({ "type": "forwardscanSettings", "address": src.to_string(), "settings": settings }));
        }
    }
}

/// Send a changed settings block to the sonar's 169.254 address, out of the sonar's interface.
pub fn send_control(state: &State, dest: std::net::Ipv4Addr, block: &[u8]) -> std::io::Result<()> {
    let path = state.net.control();
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // With 169.254.0.0/16 on several interfaces the routing table cannot tell which one leads to the
    // sonar; pin it where the OS lets us (Linux needs 5.7+ or CAP_NET_RAW, so failing is fine).
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if let Some(name) = &path.iface {
        let _ = s.bind_device(Some(name.as_bytes()));
    }
    #[cfg(target_os = "macos")]
    if path.iface.is_some() {
        let _ = s.bind_device_by_index_v4(std::num::NonZeroU32::new(path.index));
    }
    let _ = &path;
    s.send_to(
        block,
        &SockAddr::from(SocketAddrV4::new(dest, protocol::CONTROL_PORT)),
    )?;
    Ok(())
}
