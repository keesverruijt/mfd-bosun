//! Finding the chartplotters. NEON ones advertise their REST API over mDNS as `_neonapi._tcp` (port 8444),
//! and `GET /api/v1/info` there tells model and serial number without authentication. The older NOS ones
//! advertise their remote-control port as `_navico-mfd-rp._tcp` (6633), where a session tells model, name
//! and screen (see [`crate::nos`]); they have no serial number there, so model and name make their id.
//! Garmin ones advertise `_garmin-mrn-card._tcp`; a session on their control port tells model and screen
//! resolution and carries a stable id in the RTSP hostname (see [`crate::garmin`]).
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

use crate::{api, garmin, nos, raymarine, screen::Screen};

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
    /// Whether its screen can be controlled, not only viewed (a Raymarine unit set to "Viewing only"
    /// can only be viewed).
    pub can_control: bool,
    /// A remote-screen plotter (NOS or Garmin): what its probe learned. `None` for a NEON unit, whose
    /// browser does the WebRTC itself.
    pub screen: Option<Screen>,
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
    /// "neon", "nos", "garmin" or "raymarine"
    pub kind: &'static str,
    /// Its screen can be controlled, not only viewed (false for a Raymarine unit in "Viewing only").
    pub can_control: bool,
    /// Its waypoints and routes can be exported as GPX (NEON over its REST API, NOS from its own backup,
    /// Garmin off its user-data bus).
    pub waypoints: bool,
    /// A file can be sent to it from the browser (NEON only; NOS and Garmin take none).
    pub files: bool,
    /// A NOS unit's keys
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<String>,
}

impl From<&Plotter> for PlotterView {
    fn from(p: &Plotter) -> Self {
        // What each kind can do beyond its screen: NEON (no `screen`) has the REST waypoint API and takes
        // file drops; NOS gives its waypoints as its own backup but takes no files; Garmin gives its
        // waypoints and routes off its user-data bus but takes no files.
        let (waypoints, files) = match &p.screen {
            None => (true, true),
            Some(Screen::Navico(_)) => (true, false),
            Some(Screen::Garmin(_)) => (true, false),
            // Raymarine: screen (and control) only for now; no waypoint export yet.
            Some(Screen::Raymarine(_)) => (false, false),
        };
        PlotterView {
            serial: p.serial.clone(),
            model: p.model.clone(),
            brand: p.brand.clone(),
            os_version: p.os_version.clone(),
            name: p.name.clone().unwrap_or_else(|| p.model.clone()),
            address: p.addr.ip().to_string(),
            online: p.online(),
            kind: p.screen.as_ref().map(|s| s.kind()).unwrap_or("neon"),
            can_control: p.can_control,
            waypoints,
            files,
            keys: p.screen.as_ref().map(Screen::keys).unwrap_or_default(),
        }
    }
}

#[derive(Default)]
struct Inner {
    plotters: BTreeMap<String, Plotter>,
    /// Every address worth probing: from mDNS, from the user, from what answered before.
    candidates: HashSet<SocketAddr>,
    names: BTreeMap<Ipv4Addr, String>,
    /// The software version a chartplotter announced, by address: a NOS unit does not give a real one
    /// over its control port (it answers 0.0.0), but its announcement carries it.
    versions: BTreeMap<Ipv4Addr, String>,
    /// A Raymarine screen's identity from its mDNS TXT (model/serial/rtsp-path), by address. The probe
    /// to its RTSP port fills in the pixel size and refreshes liveness, but the id is mDNS-only.
    raymarine: BTreeMap<Ipv4Addr, (String, raymarine::Info)>,
    /// Why the last probe of an address failed, so that it is logged once, not every probe.
    failing: HashMap<SocketAddr, String>,
}

#[derive(Clone)]
pub struct Discovery {
    inner: Arc<Mutex<Inner>>,
    changed: watch::Sender<()>,
}

impl Discovery {
    /// Starts browsing mDNS and probing. Needs a Tokio runtime.
    pub fn start() -> Self {
        let d = Discovery {
            inner: Default::default(),
            changed: watch::channel(()).0,
        };
        match ServiceDaemon::new() {
            Ok(daemon) => {
                tokio::spawn(d.clone().browse(daemon.clone(), SERVICE_TYPE));
                tokio::spawn(d.clone().browse(daemon.clone(), nos::SERVICE_TYPE));
                tokio::spawn(d.clone().browse(daemon.clone(), garmin::SERVICE_TYPE));
                tokio::spawn(d.clone().browse(daemon, raymarine::SERVICE_TYPE));
            }
            Err(e) => {
                log::warn!("mDNS unavailable, chartplotters will not be found by themselves: {e}")
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

    /// The software version a chartplotter announced for itself. Used only where a probe learned no
    /// real version (a NOS unit's control port gives 0.0.0), so a NEON unit's REST version stands.
    pub fn set_os_version(&self, ip: Ipv4Addr, version: &str) {
        if version.is_empty() || version == "0.0.0" {
            return;
        }
        let mut g = self.lock();
        if g.versions.get(&ip).is_some_and(|v| v == version) {
            return;
        }
        g.versions.insert(ip, version.to_string());
        let mut changed = false;
        for p in g.plotters.values_mut() {
            if p.addr.ip() == IpAddr::V4(ip) && (p.os_version.is_empty() || p.os_version == "0.0.0")
            {
                p.os_version = version.to_string();
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
                // Raymarine browses `_rtsp._tcp`, shared with cameras: take only MFDs (their TXT has
                // `raymarine-mfd-serial`), cache the id from the TXT, and probe the RTSP port for size.
                if service == raymarine::SERVICE_TYPE {
                    let txt = |k: &str| s.get_property_val_str(k).map(str::to_string);
                    if let Some((serial, mut info)) = raymarine::info_from_txt(txt, s.port) {
                        info.firmware = raymarine::firmware_from_instance(&s.fullname);
                        {
                            let mut g = self.lock();
                            for a in &addrs {
                                g.raymarine.insert(*a, (serial.clone(), info.clone()));
                            }
                        }
                        for a in &addrs {
                            self.add_address(SocketAddr::new(IpAddr::V4(*a), info.rtsp_port));
                        }
                    }
                    continue;
                }
                // The Garmin `_garmin-helm._tcp` SRV already points at the control port, as the others
                // point at their service's port.
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
        let found = if addr.port() == raymarine::RTSP_PORT {
            // identity comes from the cached mDNS TXT; the RTSP DESCRIBE gives the pixel size and
            // confirms it is alive.
            let cached = match addr.ip() {
                IpAddr::V4(ip) => self.lock().raymarine.get(&ip).cloned(),
                _ => None,
            };
            match cached {
                Some((serial, mut info)) => {
                    match raymarine::screen_size(addr.ip(), info.rtsp_port, &info.rtsp_path).await {
                        Ok((w, h)) => {
                            info.width = w;
                            info.height = h;
                            let os_version = info.firmware.clone();
                            // "Remote control" vs "Viewing only": is the RRC input port listening?
                            let can_control = raymarine::control_available(addr.ip()).await;
                            Ok(Found {
                                serial,
                                model: info.model.clone(),
                                brand: info.brand().to_string(),
                                os_version,
                                name: Some(info.name.clone()).filter(|s| !s.is_empty()),
                                can_control,
                                screen: Some(Screen::Raymarine(info)),
                            })
                        }
                        Err(e) => Err(e),
                    }
                }
                None => Err(std::io::Error::other("not a known Raymarine screen")),
            }
        } else if addr.port() == nos::PORT {
            nos::info(addr).await.map(|n| Found {
                serial: nos_id(&n),
                model: n.model.clone(),
                brand: n.brand().to_string(),
                os_version: n.firmware.clone(),
                name: Some(n.name.clone()).filter(|s| !s.is_empty()),
                can_control: true,
                screen: Some(Screen::Navico(n)),
            })
        } else if addr.port() == garmin::CONTROL_PORT {
            match garmin::info(addr).await {
                Ok(g) => Ok(Found {
                    serial: format!("garmin-{}", g.serial),
                    model: g.model.clone(),
                    brand: g.brand().to_string(),
                    // the firmware version lives on the plotter's HTTP app API, not the control port
                    os_version: garmin::build_version(addr.ip()).await.unwrap_or_default(),
                    name: Some(g.name.clone()).filter(|s| !s.is_empty()),
                    can_control: true,
                    screen: Some(Screen::Garmin(g)),
                }),
                Err(e) => Err(e),
            }
        } else {
            api::info(addr).await.map(|i| Found {
                serial: i.serial_number,
                model: i.model,
                brand: i.brand,
                os_version: i.os_version,
                name: None,
                can_control: true,
                screen: None,
            })
        };
        let info = match found {
            Ok(f) if !f.serial.is_empty() => f,
            Ok(_) => return self.probe_failed(addr, "no serial number".into()),
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => {
                return self.probe_failed(addr, "no answer".into())
            }
            Err(e) => return self.probe_failed(addr, e.to_string()),
        };
        let mut g = self.lock();
        g.failing.remove(&addr);
        let name = match addr.ip() {
            IpAddr::V4(ip) => g.names.get(&ip).cloned(),
            _ => None,
        }
        .or(info.name);
        // a NOS unit answers 0.0.0 over its control port; prefer the version it announced instead
        let os_version = match addr.ip() {
            IpAddr::V4(ip) if info.os_version.is_empty() || info.os_version == "0.0.0" => {
                g.versions.get(&ip).cloned().unwrap_or(info.os_version)
            }
            _ => info.os_version,
        };
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
                p.os_version = os_version;
                p.can_control = info.can_control;
                if name.is_some() {
                    p.name = name;
                }
                let screen_changed = p.screen != info.screen;
                p.screen = info.screen;
                switch || !was_online || screen_changed
            }
            None => {
                log::info!("{} {} ({}) at {addr}", info.brand, info.model, info.serial);
                g.plotters.insert(
                    info.serial.clone(),
                    Plotter {
                        serial: info.serial,
                        model: info.model,
                        brand: info.brand,
                        os_version,
                        name,
                        addr,
                        last_seen: Instant::now(),
                        can_control: info.can_control,
                        screen: info.screen,
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

    /// Every address is probed again every [`PROBE_INTERVAL`], and one given by hand on both ports, of which
    /// one never answers: so log why only when that changes.
    fn probe_failed(&self, addr: SocketAddr, why: String) {
        if self.lock().failing.insert(addr, why.clone()).as_ref() != Some(&why) {
            let what = if addr.port() == nos::PORT {
                "NOS remote control"
            } else if addr.port() == garmin::CONTROL_PORT {
                "Garmin Helm"
            } else {
                "NEON screen sharing"
            };
            log::debug!("{what} connection to {addr}: {why}");
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
    /// Whether we can control it, not only view it (a Raymarine unit set to "Viewing only" cannot).
    can_control: bool,
    screen: Option<Screen>,
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
