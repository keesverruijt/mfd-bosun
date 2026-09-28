//! What we know about the sonars on the network, shared by the receivers, the network task and
//! the web server. Everything the page needs goes out over one broadcast channel.

use axum::body::Bytes;
use serde_json::json;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use crate::protocol;

pub const DATA_TTL: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub enum Out {
    Frame(Bytes),
    Text(String),
}

/// A chartplotter as it announces itself (UDP multicast 239.2.1.1:2052).
#[derive(Clone, Debug, Default)]
pub struct Device {
    pub name: String,
    pub model: String,
    pub ip: Option<Ipv4Addr>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Notice {
    pub ok: bool,
    pub text: String,
}

#[derive(Clone, Debug, Default)]
pub struct ControlPath {
    #[cfg_attr(windows, allow(dead_code))]
    pub iface: Option<String>,
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub index: u32,
    pub link_local: Option<Ipv4Addr>,
}

#[derive(Default)]
pub struct Inner {
    /// Sonar 169.254 address -> its latest own ForwardScan settings block (UDP 10756).
    pub blocks: HashMap<Ipv4Addr, Vec<u8>>,
    pub last_ping: HashMap<Ipv4Addr, Instant>,
    /// 169.254 address -> the device's announcement.
    pub devices: HashMap<Ipv4Addr, Device>,
    pub control: ControlPath,
    pub notice: Option<Notice>,
    sources_sent: String,
}

#[derive(Clone)]
pub struct State {
    pub inner: Arc<Mutex<Inner>>,
    pub tx: broadcast::Sender<Out>,
    pub allow_control: bool,
    /// Differs on every start, so an open page notices a new server (a new version) and reloads.
    pub instance: String,
}

impl State {
    pub fn new(allow_control: bool) -> Self {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        State {
            inner: Default::default(),
            tx: broadcast::channel(64).0,
            allow_control,
            instance: format!("{:x}", t.as_nanos()),
        }
    }

    pub fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn send_text(&self, v: serde_json::Value) {
        let _ = self.tx.send(Out::Text(v.to_string()));
    }

    pub fn name_of(&self, ip: Ipv4Addr) -> String {
        self.lock()
            .devices
            .get(&ip)
            .map(|d| d.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| ip.to_string())
    }

    /// The messages a newly connected page gets first.
    pub fn hello(&self) -> Vec<String> {
        let g = self.lock();
        let mut v =
            vec![json!({ "type": "config", "allowControl": self.allow_control, "instance": self.instance }).to_string()];
        v.push(json!({ "type": "sources", "sources": sources(&g) }).to_string());
        for (ip, b) in &g.blocks {
            v.push(json!({ "type": "forwardscanSettings", "address": ip.to_string(), "settings": protocol::parse_control(b) }).to_string());
        }
        if let Some(n) = &g.notice {
            v.push(json!({ "type": "network", "ok": n.ok, "text": n.text }).to_string());
        }
        v
    }

    /// Send the source list when it changed.
    pub fn refresh_sources(&self) {
        let mut g = self.lock();
        let list = json!({ "type": "sources", "sources": sources(&g) }).to_string();
        if list != g.sources_sent {
            g.sources_sent = list.clone();
            drop(g);
            let _ = self.tx.send(Out::Text(list));
        }
    }

    pub fn set_notice(&self, n: Notice) {
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
        self.send_text(json!({ "type": "network", "ok": n.ok, "text": n.text }));
    }
}

/// The sonars with ForwardScan: those that sent settings or pings.
pub fn sources(g: &Inner) -> Vec<serde_json::Value> {
    let mut ips: Vec<Ipv4Addr> = g.blocks.keys().chain(g.last_ping.keys()).copied().collect();
    ips.sort();
    ips.dedup();
    ips.iter()
        .map(|ip| {
            let d = g.devices.get(ip).cloned().unwrap_or_default();
            let live = g.last_ping.get(ip).is_some_and(|t| t.elapsed() < DATA_TTL);
            json!({
                "address": ip.to_string(),
                "name": if d.name.is_empty() { ip.to_string() } else { d.name },
                "model": if d.model.is_empty() { "Sonar".to_string() } else { d.model },
                "forwardScan": true,
                "live": live,
            })
        })
        .collect()
}
