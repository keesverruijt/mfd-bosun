//! Finding the chartplotters. NEON ones advertise their REST API over mDNS as `_neonapi._tcp` (port 8444),
//! and `GET /api/v1/info` there tells model and serial number without authentication. The older NOS ones
//! advertise their remote-control port as `_navico-mfd-rp._tcp` (6633), where a session tells model, name
//! and screen (see [`crate::nos`]); they have no serial number there, so model and name make their id.
//!
//! Chartplotters are keyed by serial number. Every known one is probed again now and then; one that has not
//! answered for [`OFFLINE_AFTER`] shows as offline, and is forgotten after [`FORGET_AFTER`]. Addresses can
//! also be given by hand ([`Discovery::add_address`]), for a network where mDNS does not pass.

use mdns_sd::{ServiceDaemon, ServiceEvent};
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::watch;

use crate::{api, nos};

pub const SERVICE_TYPE: &str = "_neonapi._tcp.local.";
const PROBE_INTERVAL: Duration = Duration::from_secs(15);
pub const OFFLINE_AFTER: Duration = Duration::from_secs(40);
pub const FORGET_AFTER: Duration = Duration::from_secs(3600);

#[derive(Clone, Debug)]
pub struct Plotter {
    pub serial: String,
    pub model: String,
    pub brand: String,
    pub os_version: String,
    /// The name the user gave it, when something told us (the chartplotters' own announcements)
    pub name: Option<String>,
    /// Where its REST API and signalling socket answer (NEON), or its remote-control port (NOS).
    pub addr: SocketAddr,
    pub last_seen: Instant,
    /// A NOS unit: what its remote-control port says.
    pub nos: Option<nos::Info>,
}

impl Plotter {
    pub fn online(&self) -> bool {
        self.last_seen.elapsed() < OFFLINE_AFTER
    }
}

/// What the web page gets.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlotterView {
    pub serial: String,
    pub model: String,
    pub brand: String,
    pub os_version: String,
    pub name: String,
    pub address: String,
    pub online: bool,
    /// "neon" or "nos"
    pub kind: &'static str,
    /// A NOS unit's keys
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
}

impl From<&Plotter> for PlotterView {
    fn from(p: &Plotter) -> Self {
        PlotterView {
            serial: p.serial.clone(),
            model: p.model.clone(),
            brand: p.brand.clone(),
            os_version: p.os_version.clone(),
            name: p.name.clone().unwrap_or_else(|| p.model.clone()),
            address: p.addr.ip().to_string(),
            online: p.online(),
            kind: if p.nos.is_some() { "nos" } else { "neon" },
            keys: p
                .nos
                .iter()
                .flat_map(|n| n.keys.iter().map(|(k, _)| k.clone()))
                .collect(),
        }
    }
}

#[derive(Default)]
struct Inner {
    plotters: BTreeMap<String, Plotter>,
    /// Every address worth probing: from mDNS, from the user, from what answered before.
    candidates: HashSet<SocketAddr>,
    names: BTreeMap<Ipv4Addr, String>,
    /// Why the last probe of an address failed, so that it is logged once, not every probe.
    failing: HashMap<SocketAddr, String>,
}

#[derive(Clone)]
pub struct Discovery {
    inner: Arc<Mutex<Inner>>,
    changed: watch::Sender<()>,
}

impl Discovery {
    /// Starts browsing mDNS (when `mdns`) and probing. Needs a Tokio runtime.
    pub fn start(mdns: bool) -> Self {
        let d = Discovery {
            inner: Default::default(),
            changed: watch::channel(()).0,
        };
        if mdns {
            match ServiceDaemon::new() {
                Ok(daemon) => {
                    tokio::spawn(d.clone().browse(daemon.clone(), SERVICE_TYPE));
                    tokio::spawn(d.clone().browse(daemon, nos::SERVICE_TYPE));
                }
                Err(e) => log::warn!(
                    "mDNS unavailable, chartplotters will not be found by themselves: {e}"
                ),
            }
        }
        tokio::spawn(d.clone().probe_all());
        d
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Probe this address too, from now on.
    pub fn add_address(&self, addr: SocketAddr) {
        if self.lock().candidates.insert(addr) {
            tokio::spawn(self.clone().probe(addr));
        }
    }

    /// The name a chartplotter announced for itself at one of its addresses.
    pub fn set_name(&self, ip: Ipv4Addr, name: &str) {
        let mut g = self.lock();
        if name.is_empty() || g.names.get(&ip).is_some_and(|n| n == name) {
            return;
        }
        g.names.insert(ip, name.to_string());
        let mut changed = false;
        for p in g.plotters.values_mut() {
            if p.addr.ip() == IpAddr::V4(ip) && p.name.as_deref() != Some(name) {
                p.name = Some(name.to_string());
                changed = true;
            }
        }
        drop(g);
        if changed {
            self.changed.send_replace(());
        }
    }

    pub fn list(&self) -> Vec<Plotter> {
        self.lock().plotters.values().cloned().collect()
    }

    pub fn get(&self, serial: &str) -> Option<Plotter> {
        self.lock().plotters.get(serial).cloned()
    }

    /// Changes whenever the list (or a chartplotter's online state) changes.
    pub fn subscribe(&self) -> watch::Receiver<()> {
        self.changed.subscribe()
    }

    async fn browse(self, daemon: ServiceDaemon, service: &'static str) {
        let rx = match daemon.browse(service) {
            Ok(rx) => rx,
            Err(e) => {
                log::warn!("mDNS browse for {service}: {e}");
                return;
            }
        };
        while let Ok(ev) = rx.recv_async().await {
            if let ServiceEvent::ServiceResolved(s) = ev {
                // routable addresses first: the probe that answers first wins the address
                let mut addrs: Vec<Ipv4Addr> = s.get_addresses_v4().into_iter().collect();
                addrs.sort_by_key(|a| (a.is_link_local(), *a));
                log::debug!("mDNS: {} at {addrs:?}:{}", s.fullname, s.port);
                for a in addrs {
                    self.add_address(SocketAddr::new(IpAddr::V4(a), s.port));
                }
            }
        }
    }

    async fn probe_all(self) {
        let mut tick = tokio::time::interval(PROBE_INTERVAL);
        let mut shown = BTreeMap::new();
        loop {
            tick.tick().await;
            let addrs: Vec<SocketAddr> = self.lock().candidates.iter().copied().collect();
            let probes = addrs.into_iter().map(|a| self.clone().probe(a));
            futures_join_all(probes).await;
            // going offline, and being forgotten, happen by time alone
            let mut g = self.lock();
            g.plotters
                .retain(|_, p| p.last_seen.elapsed() < FORGET_AFTER);
            let now: BTreeMap<String, bool> = g
                .plotters
                .iter()
                .map(|(s, p)| (s.clone(), p.online()))
                .collect();
            drop(g);
            if now != shown {
                shown = now;
                self.changed.send_replace(());
            }
        }
    }

    async fn probe(self, addr: SocketAddr) {
        let found = if addr.port() == nos::PORT {
            nos::info(addr).await.map(|n| Found {
                serial: nos_id(&n),
                model: n.model.clone(),
                brand: n.brand().to_string(),
                os_version: n.firmware.clone(),
                name: Some(n.name.clone()).filter(|s| !s.is_empty()),
                nos: Some(n),
            })
        } else {
            api::info(addr).await.map(|i| Found {
                serial: i.serial_number,
                model: i.model,
                brand: i.brand,
                os_version: i.os_version,
                name: None,
                nos: None,
            })
        };
        let info = match found {
            Ok(f) if !f.serial.is_empty() => f,
            Ok(_) => return self.probe_failed(addr, "no serial number".into()),
            Err(e) => return self.probe_failed(addr, e.to_string()),
        };
        let mut g = self.lock();
        g.failing.remove(&addr);
        let name = match addr.ip() {
            IpAddr::V4(ip) => g.names.get(&ip).cloned(),
            _ => None,
        }
        .or(info.name);
        let changed = match g.plotters.get_mut(&info.serial) {
            Some(p) => {
                // it answers on several addresses: keep the one we have while it answers there, but
                // prefer a routable one over 169.254
                let was_online = p.online();
                let switch = p.addr != addr
                    && (!was_online || (is_link_local(p.addr) && !is_link_local(addr)));
                if switch {
                    p.addr = addr;
                }
                if p.addr != addr {
                    return;
                }
                p.last_seen = Instant::now();
                p.os_version = info.os_version;
                if name.is_some() {
                    p.name = name;
                }
                let nos_changed = p.nos != info.nos;
                p.nos = info.nos;
                switch || !was_online || nos_changed
            }
            None => {
                log::info!("{} {} ({}) at {addr}", info.brand, info.model, info.serial);
                g.plotters.insert(
                    info.serial.clone(),
                    Plotter {
                        serial: info.serial,
                        model: info.model,
                        brand: info.brand,
                        os_version: info.os_version,
                        name,
                        addr,
                        last_seen: Instant::now(),
                        nos: info.nos,
                    },
                );
                true
            }
        };
        drop(g);
        if changed {
            self.changed.send_replace(());
        }
    }

    /// Every address is probed again every [`PROBE_INTERVAL`], and a chartplotter's other port (8444 or
    /// 6633) never answers: so log why only when that changes.
    fn probe_failed(&self, addr: SocketAddr, why: String) {
        if self.lock().failing.insert(addr, why.clone()).as_ref() != Some(&why) {
            log::debug!("no chartplotter at {addr}: {why}");
        }
    }
}

/// What a probe found.
struct Found {
    serial: String,
    model: String,
    brand: String,
    os_version: String,
    name: Option<String>,
    nos: Option<nos::Info>,
}

/// A NOS unit's id in the list (it tells no serial number): "nos-" and its model and name.
fn nos_id(n: &nos::Info) -> String {
    let mut id = String::from("nos");
    for part in [&n.model, &n.name] {
        id.push('-');
        id.extend(
            part.chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }),
        );
    }
    id
}

fn is_link_local(a: SocketAddr) -> bool {
    matches!(a.ip(), IpAddr::V4(ip) if ip.is_link_local())
}

/// Runs the futures concurrently and waits for all of them.
async fn futures_join_all(
    fs: impl IntoIterator<Item = impl std::future::Future<Output = ()> + Send + 'static>,
) {
    let handles: Vec<_> = fs.into_iter().map(tokio::spawn).collect();
    for h in handles {
        let _ = h.await;
    }
}
