//! neon-forwardscan: shows a Navico NOS sonar device's ForwardScan in a web page, and puts that page on the
//! home screen of the Navico / B&G / Simrad / Lowrance chartplotters on the same network.
//!
//! As a library it runs inside another program (mfd-bosun): [`ForwardScan::start`] starts the
//! receivers, the advertisement and the network upkeep; [`ForwardScan::router`] is the page, to serve or nest.
//! The general networking (finding the chartplotters, advertising, keeping a 169.254 address) lives
//! in the common [`mfd_net::link`]; this crate adds the ForwardScan sonar on top.

pub mod net;
pub use mfd_net::netif;
pub mod protocol;
pub mod state;
pub mod web;

use std::time::{Duration, Instant};

use mfd_net::link::{self, LinkKeeper, NetOptions, Upkeep};

pub struct Options {
    /// The interface the sonar is on (default: found from the sonar's announcement)
    pub interface: Option<String>,
    /// May add a 169.254.x.x address when changing settings needs one
    pub add_link_local: bool,
    /// May the page change the sonar's ForwardScan settings
    pub allow_control: bool,
    /// Put a tile on the chartplotters that opens the page
    pub advertise: Option<Advertise>,
}

pub struct Advertise {
    /// Name of the tile
    pub name: String,
    /// TCP port the page is served on
    pub port: u16,
    /// Path of the page, with a trailing slash: "/" or "/forwardscan/"
    pub path: String,
}

pub struct ForwardScan {
    state: state::State,
    stop: tokio::sync::oneshot::Sender<()>,
    upkeep: tokio::task::JoinHandle<()>,
}

impl ForwardScan {
    /// Binds the UDP sockets (the only step that can fail) and starts everything. Needs a Tokio runtime.
    pub fn start(opts: Options) -> std::io::Result<Self> {
        let sockets = net::Sockets::bind()?;
        let state = state::State::new(opts.allow_control);
        spawn_receiver(
            "ForwardScan",
            net::receive_pings(state.clone(), sockets.pings),
        );
        spawn_receiver(
            "ForwardScan settings",
            net::receive_settings(state.clone(), sockets.settings),
        );
        spawn_receiver(
            "chartplotter announcements",
            link::receive_announcements(state.net.clone(), sockets.announcements),
        );
        if let Some(a) = opts.advertise {
            tokio::spawn(link::advertise(
                state.net.clone(),
                "NeonForwardScan".to_string(),
                a.name,
                a.port,
                a.path,
            ));
        }
        // forward the common layer's network notices to the page
        spawn_notice_forwarder(state.clone());

        let mut keeper = LinkKeeper::new(NetOptions {
            interface: opts.interface,
            add_link_local: opts.add_link_local,
        });
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let st = state.clone();
        let started = Instant::now();
        let upkeep = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(3));
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        sonar_upkeep(&mut keeper, &st, started);
                        st.refresh_sources();
                    }
                    _ = &mut stopped => break,
                }
            }
            keeper.cleanup();
        });
        Ok(ForwardScan {
            state,
            stop,
            upkeep,
        })
    }

    pub fn state(&self) -> &state::State {
        &self.state
    }

    /// The page, its WebSocket stream and its settings API, all on relative paths.
    pub fn router(&self) -> axum::Router {
        web::router(self.state.clone())
    }

    /// Stops the network upkeep and removes a 169.254 address it added.
    pub async fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.upkeep.await;
    }
}

/// Finds the ForwardScan sonar (if any), keeps a 169.254 path to it, and sets the page's notice.
/// Only the sonar-finding and the wording are ForwardScan's; the link upkeep is the common layer's.
fn sonar_upkeep(keeper: &mut LinkKeeper, state: &state::State, started: Instant) {
    let (sonar, sonar_ip, avoid) = {
        let g = state.lock();
        let v = g.blocks.keys().chain(g.last_ping.keys()).next().copied();
        let ip = v.and_then(|v| state.net.device(v)).and_then(|d| d.ip);
        let mut avoid = state.net.device_keys();
        if let Some(v) = v {
            avoid.push(v);
        }
        (v, ip, avoid)
    };
    let Some(sonar) = sonar else {
        if started.elapsed() > Duration::from_secs(20) {
            // A chartplotter we heard but cannot reach is the more useful thing to say than that no
            // sonar was found: it tells the user the network, not the sonar, is the problem.
            if let Some((ll, d)) = state.net.unreachable().first() {
                let label = match (d.model.as_str(), d.name.as_str()) {
                    ("", "") => "a chartplotter".to_string(),
                    (m, "") => m.to_string(),
                    ("", n) => format!("\"{n}\""),
                    (m, n) => format!("{m} \"{n}\""),
                };
                let addr = d.ip.unwrap_or(*ll);
                state.net.set_notice(
                    false,
                    format!(
                        "Sees {label} at {addr} but cannot reach it from here: this computer has no \
                         address on its network. Give this computer a 169.254.x.x address, or put it \
                         on the chartplotter's network."
                    ),
                );
            } else {
                let mut text = "No Sonar detected yet.".to_string();
                if link::is_wsl() {
                    text += " On WSL2 use mirrored networking (see the README): the default NAT network does not pass the broadcasts.";
                }
                state.net.set_notice(false, text);
            }
        }
        return;
    };
    let name = state.net.name_of(sonar);
    match keeper.ensure(&state.net, sonar_ip, &avoid) {
        Upkeep::NoInterface(e) => {
            state.net.set_notice(
                false,
                format!("Settings changes may not reach {name}: {e}."),
            );
        }
        Upkeep::Ready { iface, addr } => {
            state.net.set_notice(
                true,
                format!("{name} ({sonar}) is on {iface}; this computer has {addr} there."),
            );
        }
        Upkeep::NoLinkLocal { iface, how } => {
            state.net.set_notice(
                false,
                format!(
                    "Showing {name}'s ForwardScan, but its settings cannot be changed from here: that needs a \
                     169.254.x.x address on {iface}. {how}"
                ),
            );
        }
    }
}

fn spawn_notice_forwarder(state: state::State) {
    let mut rx = state.net.subscribe();
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(n) => state.send_text(
                    serde_json::json!({ "type": "network", "ok": n.ok, "text": n.text }),
                ),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn spawn_receiver(
    what: &'static str,
    task: impl std::future::Future<Output = std::io::Result<()>> + Send + 'static,
) {
    tokio::spawn(async move {
        if let Err(e) = task.await {
            log::error!("receiving {what}: {e}");
        }
    });
}
