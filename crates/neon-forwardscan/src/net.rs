//! The UDP side: ForwardScan pings and settings from the sonar, the chartplotters' announcements,
//! our own advertisement to them, and keeping a 169.254 address on the sonar's interface.

use serde_json::json;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

use crate::netif::{self, AddError, Iface};
use crate::protocol;
use crate::state::{Device, Notice, Out, State};

const ANNOUNCE_GROUP: Ipv4Addr = Ipv4Addr::new(239, 2, 1, 1);
const ANNOUNCE_PORT: u16 = 2052; // the chartplotters announce themselves here
const ADVERTISE_PORT: u16 = 2053; // and look for web apps here

/// A UDP socket on a port others on this host (Signal K, the Signal K plugin) may listen on too.
pub fn shared_socket(port: u16) -> std::io::Result<UdpSocket> {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(unix)]
    s.set_reuse_port(true)?;
    s.set_broadcast(true)?;
    s.bind(&SockAddr::from(SocketAddrV4::new(
        Ipv4Addr::UNSPECIFIED,
        port,
    )))?;
    s.set_nonblocking(true)?;
    UdpSocket::from_std(s.into())
}

fn v4(a: std::net::SocketAddr) -> Option<Ipv4Addr> {
    match a {
        std::net::SocketAddr::V4(a) => Some(*a.ip()),
        _ => None,
    }
}

/// The sockets of the three receivers, bound up front so that a failure shows at start.
pub struct Sockets {
    pub pings: UdpSocket,
    pub settings: UdpSocket,
    pub announcements: UdpSocket,
}

impl Sockets {
    pub fn bind() -> std::io::Result<Self> {
        let bind = |what: &str, port| {
            shared_socket(port)
                .map_err(|e| std::io::Error::new(e.kind(), format!("{what} (UDP {port}): {e}")))
        };
        Ok(Sockets {
            pings: bind("ForwardScan", protocol::PORT)?,
            settings: bind("ForwardScan settings", protocol::CONTROL_PORT)?,
            announcements: bind("chartplotter announcements", ANNOUNCE_PORT)?,
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

/// The chartplotters' JSON announcements: name, model, and both their addresses.
pub async fn receive_announcements(state: State, sock: UdpSocket) -> std::io::Result<()> {
    let mut joined: HashSet<Ipv4Addr> = HashSet::new();
    let mut buf = vec![0u8; 8192];
    let mut rejoin = tokio::time::interval(Duration::from_secs(10));
    loop {
        tokio::select! {
            _ = rejoin.tick() => {
                // join on every interface, again when interfaces come and go
                for a in netif::list().iter().flat_map(|i| i.addrs.iter().map(|a| a.0)) {
                    if !joined.contains(&a) && sock.join_multicast_v4(ANNOUNCE_GROUP, a).is_ok() {
                        joined.insert(a);
                    }
                }
            }
            r = sock.recv_from(&mut buf) => {
                let (n, _) = r?;
                let Ok(v) = serde_json::from_slice::<serde_json::Value>(&buf[..n]) else { continue };
                let ip = |k: &str| v.get(k).and_then(|x| x.as_str()).and_then(|s| s.parse::<Ipv4Addr>().ok());
                let Some(ll) = ip("IP_Zeroconfig") else { continue };
                let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
                let d = Device { name: s("Name"), model: s("Model"), ip: ip("IP") };
                let new = state.lock().devices.insert(ll, d.clone()).is_none();
                if new {
                    log::info!("{} \"{}\" at {} / {ll}", d.model, d.name, d.ip.map(|a| a.to_string()).unwrap_or_default());
                    state.refresh_sources();
                }
            }
        }
    }
}

pub struct NetOptions {
    pub interface: Option<String>,
    pub add_link_local: bool,
}

/// Keeps the path for settings changes working: finds the sonar's interface, and when it has no
/// 169.254 address adds one if we may. Returns (interface, address) of one we added, for removal.
pub struct LinkKeeper {
    opts: NetOptions,
    added: Option<(Iface, Ipv4Addr)>,
    refused: Option<String>, // privilege missing: do not retry
    started: Instant,
}

impl LinkKeeper {
    pub fn new(opts: NetOptions) -> Self {
        LinkKeeper {
            opts,
            added: None,
            refused: None,
            started: Instant::now(),
        }
    }

    pub fn tick(&mut self, state: &State) {
        let ifaces = netif::list();
        let (sonar, sonar_ip, devices) = {
            let g = state.lock();
            let v = g.blocks.keys().chain(g.last_ping.keys()).next().copied();
            let ip = v.and_then(|v| g.devices.get(&v)).and_then(|d| d.ip);
            (v, ip, g.devices.keys().copied().collect::<Vec<_>>())
        };
        let Some(sonar) = sonar else {
            if self.started.elapsed() > Duration::from_secs(20) {
                let mut text = "No ForwardScan from a sonar yet. Start ForwardScan on the sonar, and check that this \
                    computer is on the boat's Ethernet."
                    .to_string();
                if is_wsl() {
                    text += " On WSL2 use mirrored networking (see the README): the default NAT network does not pass the broadcasts.";
                }
                state.set_notice(Notice { ok: false, text });
            }
            return;
        };
        let name = state.name_of(sonar);
        let iface = match netif::choose(&ifaces, self.opts.interface.as_deref(), sonar_ip) {
            Ok(i) => i.clone(),
            Err(e) => {
                state.lock().control = Default::default();
                return state.set_notice(Notice {
                    ok: false,
                    text: format!("Settings changes may not reach {name}: {e}."),
                });
            }
        };
        let mut ll = iface.link_local();
        if ll.is_none() && self.opts.add_link_local && self.refused.is_none() {
            let mut avoid = devices.clone();
            avoid.push(sonar);
            let addr = netif::pick_link_local(&iface, &avoid);
            match netif::add_link_local(&iface, addr) {
                Ok(()) => {
                    log::info!("added {addr}/16 to {} (removed again on exit)", iface.name);
                    self.added = Some((iface.clone(), addr));
                    ll = Some(addr);
                }
                Err(AddError::Privilege(how)) => self.refused = Some(how),
                Err(AddError::Other(e)) => log::warn!("could not add a 169.254 address: {e}"),
            }
        }
        state.lock().control = crate::state::ControlPath {
            iface: Some(iface.name.clone()),
            index: iface.index,
            link_local: ll,
        };
        let notice = match ll {
            Some(a) => Notice {
                ok: true,
                text: format!(
                    "{name} ({sonar}) is on {}; this computer has {a} there.",
                    iface.name
                ),
            },
            None => {
                let how = self.refused.clone().unwrap_or_else(|| {
                    netif::manual_instructions(&iface, netif::pick_link_local(&iface, &devices))
                });
                Notice {
                    ok: false,
                    text: format!(
                        "Showing {name}'s ForwardScan, but its settings cannot be changed from here: that needs a \
                         169.254.x.x address on {}. {how}",
                        iface.name
                    ),
                }
            }
        };
        state.set_notice(notice);
    }

    pub fn cleanup(&mut self) {
        if let Some((iface, addr)) = self.added.take() {
            match netif::remove_link_local(&iface, addr) {
                Ok(()) => log::info!("removed {addr} from {}", iface.name),
                Err(e) => log::warn!("could not remove {addr} from {}: {e}", iface.name),
            }
        }
    }
}

fn is_wsl() -> bool {
    cfg!(target_os = "linux")
        && std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .is_ok_and(|s| s.to_ascii_lowercase().contains("microsoft"))
}

/// Send a changed settings block to the sonar's 169.254 address, out of the sonar's interface.
pub fn send_control(state: &State, dest: Ipv4Addr, block: &[u8]) -> std::io::Result<()> {
    let path = state.lock().control.clone();
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

/// Tell the chartplotters about the page: a JSON message to 239.2.1.1:2053 that puts a tile on
/// their home screen that opens it. A chartplotter keys the tile on Source + FeatureName, reloads
/// the page whenever anything else in the message changes, and raises a "not found" alarm 20-25 s
/// after the last one. So: every 5 s, and from one interface only, since each interface would give
/// another URL.
pub async fn advertise(state: State, port: u16, path: String, name: String) {
    // the chartplotters announce themselves every second: hear them first, so the first advert
    // already goes out on their interface (a later switch would reload the page on them)
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut t = tokio::time::interval(ADVERTISE_INTERVAL);
    let mut current: Option<(String, Ipv4Addr)> = None;
    loop {
        t.tick().await;
        let ifaces: Vec<Iface> = netif::list()
            .into_iter()
            .filter(|i| !i.is_virtual())
            .collect();
        let (seen, sonar_iface) = {
            let g = state.lock();
            let seen: Vec<Ipv4Addr> = g.devices.values().filter_map(|d| d.ip).collect();
            (seen, g.control.iface.clone())
        };
        let Some((iface, ip)) = advertise_on(&ifaces, &seen, sonar_iface.as_deref()) else {
            continue;
        };
        if current.as_ref() != Some(&(iface.name.clone(), ip)) {
            log::info!(
                "advertising the page to the chartplotters on {} as http://{ip}:{port}{path}",
                iface.name
            );
            current = Some((iface.name.clone(), ip));
        }
        let base = format!("http://{ip}:{port}{path}");
        // BrowserPanel is unused on NEON but kept, well-formed, for the older NOS chartplotters
        let msg = json!({
            "Version": "1",
            "Source": "NeonForwardScan",
            "FeatureName": name,
            "IP": ip.to_string(),
            "Text": [{ "Language": "en", "Name": name }],
            "Icon": format!("{base}icon.png?v=2"), // a new URL makes the MFDs fetch the icon again
            "URL": base,
            "BrowserPanel": {
                "Enable": true,
                "ProgressBarEnable": true,
                "MenuEnabled": true,
                "MenuText": [{ "Language": "en", "Name": name }]
            }
        });
        let sent = (|| -> std::io::Result<()> {
            let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
            s.set_multicast_ttl_v4(4)?;
            s.set_multicast_if_v4(&ip)?;
            s.bind(&SockAddr::from(SocketAddrV4::new(ip, 0)))?;
            s.send_to(
                msg.to_string().as_bytes(),
                &SockAddr::from(SocketAddrV4::new(ANNOUNCE_GROUP, ADVERTISE_PORT)),
            )?;
            Ok(())
        })();
        if let Err(e) = sent {
            log::debug!("advertising on {} ({ip}): {e}", iface.name);
        }
    }
}

const ADVERTISE_INTERVAL: Duration = Duration::from_secs(5);

/// The one interface (and its address) to advertise on: where a chartplotter announced itself,
/// else where the sonar is, else the first by name. Stable while the network is.
fn advertise_on<'a>(
    ifaces: &'a [Iface],
    mfds: &[Ipv4Addr],
    sonar_iface: Option<&str>,
) -> Option<(&'a Iface, Ipv4Addr)> {
    let mut sorted: Vec<&Iface> = ifaces.iter().filter(|i| i.preferred().is_some()).collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let pick = sorted
        .iter()
        .find(|i| mfds.iter().any(|a| i.contains(*a)))
        .or_else(|| sorted.iter().find(|i| Some(i.name.as_str()) == sonar_iface))
        .or_else(|| sorted.first())?;
    // the address in the chartplotter's subnet if it has several
    let same_net = |(a, m): &(Ipv4Addr, Ipv4Addr), b: Ipv4Addr| {
        let m = u32::from(*m);
        u32::from(*a) & m == u32::from(b) & m
    };
    let ip = pick
        .addrs
        .iter()
        .find(|a| !a.0.is_link_local() && mfds.iter().any(|m| same_net(a, *m)))
        .map(|a| a.0)
        .or_else(|| pick.preferred())?;
    Some((pick, ip))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(name: &str, addrs: &[&str]) -> Iface {
        Iface {
            name: name.into(),
            index: 1,
            mac: None,
            addrs: addrs
                .iter()
                .map(|a| (a.parse().unwrap(), "255.255.255.0".parse().unwrap()))
                .collect(),
            lan: true,
        }
    }

    #[test]
    fn advertises_on_one_interface() {
        let ifs = vec![
            iface("wlan0", &["192.168.1.20"]),
            iface("eth0", &["10.56.0.1"]),
        ];
        let mfd = ["10.56.0.23".parse().unwrap()];
        let (i, ip) = advertise_on(&ifs, &mfd, None).unwrap();
        assert_eq!(
            (i.name.as_str(), ip.to_string().as_str()),
            ("eth0", "10.56.0.1")
        );
        // no chartplotter seen yet: where the sonar is, else the first by name
        assert_eq!(
            advertise_on(&ifs, &[], Some("wlan0")).unwrap().0.name,
            "wlan0"
        );
        assert_eq!(advertise_on(&ifs, &[], None).unwrap().0.name, "eth0");
    }
}
