//! What we know about the sonars on the network, shared by the receivers, the network task and the
//! web server. The general network state (chartplotters seen, the control path, the notice) lives in
//! the common [`mfd_net::link::Network`]; this holds only the ForwardScan-specific sonar data.
//! Everything the page needs goes out over one broadcast channel.

use axum::body::Bytes;
use serde_json::json;
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

use crate::protocol;

pub use mfd_net::link::{ControlPath, Device, Network, Notice};

pub const DATA_TTL: Duration = Duration::from_secs(10);

#[derive(Clone)]
pub enum Out {
    Frame(Bytes),
    Text(String),
}

/// The ForwardScan-specific state: the sonars' latest settings blocks and when they last pinged.
#[derive(Default)]
pub struct Inner {
    /// Sonar 169.254 address -> its latest own ForwardScan settings block (UDP 10756).
    pub blocks: HashMap<Ipv4Addr, Vec<u8>>,
    pub last_ping: HashMap<Ipv4Addr, Instant>,
    sources_sent: String,
}

#[derive(Clone)]
pub struct State {
    /// The common network state: the chartplotters seen, the control path and the notice.
    pub net: Network,
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
            net: Network::new(),
            inner: Default::default(),
            tx: broadcast::channel(64).0,
            allow_control,
            instance: format!("{:x}", t.as_nanos()),
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn send_text(&self, v: serde_json::Value) {
        let _ = self.tx.send(Out::Text(v.to_string()));
    }

    pub fn name_of(&self, ip: Ipv4Addr) -> String {
        self.net.name_of(ip)
    }

    /// The messages a newly connected page gets first.
    pub fn hello(&self) -> Vec<String> {
        let g = self.lock();
        let mut v =
            vec![json!({ "type": "config", "allowControl": self.allow_control, "instance": self.instance }).to_string()];
        v.push(json!({ "type": "sources", "sources": sources(&self.net, &g) }).to_string());
        for (ip, b) in &g.blocks {
            v.push(json!({ "type": "forwardscanSettings", "address": ip.to_string(), "settings": protocol::parse_control(b) }).to_string());
        }
        drop(g);
        if let Some(n) = self.net.notice() {
            v.push(json!({ "type": "network", "ok": n.ok, "text": n.text }).to_string());
        }
        v
    }

    /// Send the source list when it changed.
    pub fn refresh_sources(&self) {
        let mut g = self.lock();
        let list = json!({ "type": "sources", "sources": sources(&self.net, &g) }).to_string();
        if list != g.sources_sent {
            g.sources_sent = list.clone();
            drop(g);
            let _ = self.tx.send(Out::Text(list));
        }
    }
}

/// The sonars with ForwardScan: those that sent settings or pings.
pub fn sources(net: &Network, g: &Inner) -> Vec<serde_json::Value> {
    let mut ips: Vec<Ipv4Addr> = g.blocks.keys().chain(g.last_ping.keys()).copied().collect();
    ips.sort();
    ips.dedup();
    ips.iter()
        .map(|ip| {
            let d = net.device(*ip).unwrap_or_default();
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
