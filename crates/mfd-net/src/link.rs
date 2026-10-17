//! The Navico link layer shared by the mfd-bosun crates: finding the chartplotters by their UDP
//! announcements, advertising a web-app tile to them, and keeping a 169.254 (link-local) address on
//! a peer's interface so it can be reached. None of this is ForwardScan-specific.
//!
//! A Navico network runs a DHCP server with an APIPA (169.254.0.0/16) fallback, and every device
//! *always* also has a 169.254 address — the announcement carries it as `IP_Zeroconfig`. So a unit
//! is reachable from here whenever this computer holds an address on its network, link-local or not.

use serde::Serialize;
use serde_json::json;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::collections::{HashMap, HashSet};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::broadcast;

use crate::netif::{self, AddError, Iface};

const ANNOUNCE_GROUP: Ipv4Addr = Ipv4Addr::new(239, 2, 1, 1);
const ANNOUNCE_PORT: u16 = 2052; // the chartplotters announce themselves here
const ADVERTISE_PORT: u16 = 2053; // and look for web apps here
const ADVERTISE_INTERVAL: Duration = Duration::from_secs(5);

/// A chartplotter as it announces itself (UDP multicast 239.2.1.1:2052).
#[derive(Clone, Debug, Default)]
pub struct Device {
    pub name: String,
    pub model: String,
    /// Its routable (DHCP) address, if it has one; a device always also has a 169.254 one.
    pub ip: Option<Ipv4Addr>,
    /// The port of its remote control (`navico-mfd-rp` in its `Services`): a NOS unit's 6633. A NEON
    /// one does not list it.
    pub remote_port: Option<u16>,
}

/// A line for the page: the state of the network path, and whether it is good news.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Notice {
    pub ok: bool,
    pub text: String,
}

/// How to reach a peer for unicast (settings changes): its interface, and our 169.254 address there.
#[derive(Clone, Debug, Default)]
pub struct ControlPath {
    #[cfg_attr(windows, allow(dead_code))]
    pub iface: Option<String>,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub index: u32,
    pub link_local: Option<Ipv4Addr>,
}

#[derive(Default)]
struct NetInner {
    /// 169.254 address (the announcement's `IP_Zeroconfig`) -> the chartplotter there.
    devices: HashMap<Ipv4Addr, Device>,
    control: ControlPath,
    notice: Option<Notice>,
}

/// The shared network state: the chartplotters seen, the current control path, and the latest
/// notice. Cloning shares it. Notices also go out on a broadcast channel for a UI to forward.
#[derive(Clone)]
pub struct Network {
    inner: Arc<Mutex<NetInner>>,
    notices: broadcast::Sender<Notice>,
}

impl Default for Network {
    fn default() -> Self {
        Self::new()
    }
}

impl Network {
    pub fn new() -> Self {
        Network {
            inner: Default::default(),
            notices: broadcast::channel(16).0,
        }
    }

    fn lock(&self) -> MutexGuard<'_, NetInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The chartplotter's own name, or its address when it gave none.
    pub fn name_of(&self, ip: Ipv4Addr) -> String {
        self.lock()
            .devices
            .get(&ip)
            .map(|d| d.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| ip.to_string())
    }

    pub fn device(&self, ip: Ipv4Addr) -> Option<Device> {
        self.lock().devices.get(&ip).cloned()
    }

    /// Every chartplotter seen, as (its 169.254 address, the device).
    pub fn devices(&self) -> Vec<(Ipv4Addr, Device)> {
        self.lock()
            .devices
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    }

    /// The 169.254 keys of the devices seen (addresses to avoid giving ourselves).
    pub fn device_keys(&self) -> Vec<Ipv4Addr> {
        self.lock().devices.keys().copied().collect()
    }

    /// Records a chartplotter's announcement; returns whether it was new.
    pub fn insert_device(&self, link_local: Ipv4Addr, d: Device) -> bool {
        self.lock().devices.insert(link_local, d).is_none()
    }

    pub fn control(&self) -> ControlPath {
        self.lock().control.clone()
    }

    pub fn set_control(&self, c: ControlPath) {
        self.lock().control = c;
    }

    pub fn notice(&self) -> Option<Notice> {
        self.lock().notice.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Notice> {
        self.notices.subscribe()
    }

    /// Sets the current notice (logged, de-duplicated) and sends it to any subscriber.
    pub fn set_notice(&self, ok: bool, text: String) {
        let n = Notice { ok, text };
        let mut g = self.lock();
        if g.notice.as_ref() == Some(&n) {
            return;
        }
        if n.ok {
            log::info!("{}", n.text);
        } else {
            log::warn!("{}", n.text);
        }
        g.notice = Some(n.clone());
        drop(g);
        let _ = self.notices.send(n);
    }

    /// Chartplotters heard announcing themselves that this computer holds no address to reach.
    /// A device always has a 169.254 address (the announcement key), so it is reachable whenever we
    /// hold any 169.254 address, or an address on the same subnet as its routable one.
    pub fn unreachable(&self) -> Vec<(Ipv4Addr, Device)> {
        let ifaces: Vec<Iface> = netif::list()
            .into_iter()
            .filter(|i| !i.is_virtual())
            .collect();
        let have_link_local = ifaces.iter().any(|i| i.link_local().is_some());
        self.lock()
            .devices
            .iter()
            .filter(|(_, d)| !reachable(&ifaces, have_link_local, d))
            .map(|(ll, d)| (*ll, d.clone()))
            .collect()
    }
}

/// Whether a chartplotter can be reached for unicast from this computer: we hold any 169.254 address
/// (every device always has one), or an address on the same subnet as its routable address.
fn reachable(ifaces: &[Iface], have_link_local: bool, d: &Device) -> bool {
    have_link_local || d.ip.is_some_and(|ip| ifaces.iter().any(|i| i.contains(ip)))
}

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

pub fn v4(a: std::net::SocketAddr) -> Option<Ipv4Addr> {
    match a {
        std::net::SocketAddr::V4(a) => Some(*a.ip()),
        _ => None,
    }
}

/// A UDP socket bound to the chartplotter-announcement port, ready for [`receive_announcements`].
pub fn announcement_socket() -> std::io::Result<UdpSocket> {
    shared_socket(ANNOUNCE_PORT)
}

/// The chartplotters' JSON announcements (UDP multicast 239.2.1.1:2052): name, model, both addresses.
pub async fn receive_announcements(net: Network, sock: UdpSocket) -> std::io::Result<()> {
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
                let d = Device { name: s("Name"), model: s("Model"), ip: ip("IP"), remote_port: remote_port(&v) };
                if net.insert_device(ll, d.clone()) {
                    log::info!("{} \"{}\" at {} / {ll}", d.model, d.name, d.ip.map(|a| a.to_string()).unwrap_or_default());
                }
            }
        }
    }
}

/// The port an announcement lists for `navico-mfd-rp`, the remote control of a NOS unit.
fn remote_port(v: &serde_json::Value) -> Option<u16> {
    v.get("Services")?
        .as_array()?
        .iter()
        .find(|s| s.get("Service").and_then(|x| x.as_str()) == Some("navico-mfd-rp"))?
        .get("Port")?
        .as_u64()?
        .try_into()
        .ok()
}

pub struct NetOptions {
    pub interface: Option<String>,
    pub add_link_local: bool,
}

/// What ensuring a link-local path to a peer came to.
pub enum Upkeep {
    /// No interface leads to the peer (with the reason).
    NoInterface(String),
    /// We hold a 169.254 address on the peer's interface.
    Ready { iface: String, addr: Ipv4Addr },
    /// The peer's interface has no 169.254 address and we could not add one (with how to).
    NoLinkLocal { iface: String, how: String },
}

/// Keeps a 169.254 address on a peer's interface so unicast to it works, adding one if allowed
/// (root/Administrator, or CAP_NET_ADMIN on Linux) and removing it on exit.
pub struct LinkKeeper {
    opts: NetOptions,
    added: Option<(Iface, Ipv4Addr)>,
    refused: Option<String>, // privilege missing: do not retry
}

impl LinkKeeper {
    pub fn new(opts: NetOptions) -> Self {
        LinkKeeper {
            opts,
            added: None,
            refused: None,
        }
    }

    /// Finds the interface that reaches the peer (whose routable address, if any, is `target_ip`),
    /// ensures a 169.254 address on it, records the control path in `net`, and reports the outcome.
    /// `avoid` are addresses not to give ourselves (the devices, and the peer itself).
    pub fn ensure(
        &mut self,
        net: &Network,
        target_ip: Option<Ipv4Addr>,
        avoid: &[Ipv4Addr],
    ) -> Upkeep {
        let ifaces = netif::list();
        let iface = match netif::choose(&ifaces, self.opts.interface.as_deref(), target_ip) {
            Ok(i) => i.clone(),
            Err(e) => {
                net.set_control(ControlPath::default());
                return Upkeep::NoInterface(e.to_string());
            }
        };
        let mut ll = iface.link_local();
        if ll.is_none() && self.opts.add_link_local && self.refused.is_none() {
            let addr = netif::pick_link_local(&iface, avoid);
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
        net.set_control(ControlPath {
            iface: Some(iface.name.clone()),
            index: iface.index,
            link_local: ll,
        });
        match ll {
            Some(addr) => Upkeep::Ready {
                iface: iface.name,
                addr,
            },
            None => {
                let how = self.refused.clone().unwrap_or_else(|| {
                    netif::manual_instructions(&iface, netif::pick_link_local(&iface, avoid))
                });
                Upkeep::NoLinkLocal {
                    iface: iface.name,
                    how,
                }
            }
        }
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

pub fn is_wsl() -> bool {
    cfg!(target_os = "linux")
        && std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .is_ok_and(|s| s.to_ascii_lowercase().contains("microsoft"))
}

/// Tell the chartplotters about a web page: a JSON message to 239.2.1.1:2053 that puts a tile on
/// their home screen that opens it. A chartplotter keys the tile on Source + FeatureName, reloads
/// the page whenever anything else in the message changes, and raises a "not found" alarm 20-25 s
/// after the last one. So: every 5 s, and from one interface only, since each interface would give
/// another URL.
pub async fn advertise(net: Network, source: String, name: String, port: u16, path: String) {
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
        let seen: Vec<Ipv4Addr> = net.lock().devices.values().filter_map(|d| d.ip).collect();
        let sonar_iface = net.control().iface;
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
            "Source": source,
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
    fn remote_port_from_services() {
        // a Zeus3S (NOS, 26.1), which does not advertise navico-mfd-rp over mDNS
        let nos: serde_json::Value = serde_json::from_str(
            r#"{"DeviceType":"MFD","Model":"Zeus3S 9","Name":"NAVPOD","IP":"10.40.2.26","Services":[{"Port":10110,"Service":"NMEA0183","Version":"1"},{"Port":554,"Service":"rtsp","Version":"1"},{"Port":6633,"Service":"navico-mfd-rp","Version":"1"}]}"#,
        )
        .unwrap();
        assert_eq!(remote_port(&nos), Some(6633));
        // a Zeus S (NEON, 2.5.208)
        let neon: serde_json::Value = serde_json::from_str(
            r#"{"DeviceType":"NEON_MFD","Model":"Zeus S 9","Services":[{"Port":10110,"Service":"nmea-0183","Version":"1"}]}"#,
        )
        .unwrap();
        assert_eq!(remote_port(&neon), None);
        assert_eq!(remote_port(&serde_json::json!({"Model":"Sonar Hub"})), None);
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

    #[test]
    fn reachability_needs_a_same_network_address() {
        let dev = |ip: Option<&str>| Device {
            name: "x".into(),
            model: "Vulcan".into(),
            ip: ip.map(|s| s.parse().unwrap()),
            remote_port: Some(6633),
        };
        // a link-local-only chartplotter, and we hold only a routable address elsewhere: no path
        let ifs = vec![iface("en0", &["172.16.3.150"])];
        assert!(!reachable(&ifs, false, &dev(None)));
        assert!(!reachable(&ifs, false, &dev(Some("169.254.43.149"))));
        // holding any 169.254 address reaches it (a device always has one too)
        assert!(reachable(&ifs, true, &dev(Some("169.254.43.149"))));
        // or its routable address is on one of our subnets
        let ifs = vec![iface("en0", &["10.56.0.4"])];
        assert!(reachable(&ifs, false, &dev(Some("10.56.0.23"))));
    }
}
