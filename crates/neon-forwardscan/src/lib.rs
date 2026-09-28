//! neon-forwardscan: shows a Navico NOS sonar device's ForwardScan in a web page, and puts that page on the
//! home screen of the Navico / B&G / Simrad / Lowrance chartplotters on the same network.
//!
//! As a library it runs inside another program (mfd-bosun): [`ForwardScan::start`] starts the
//! receivers, the advertisement and the network upkeep; [`ForwardScan::router`] is the page, to serve or nest.

pub mod net;
pub mod netif;
pub mod protocol;
pub mod state;
pub mod web;

use std::time::Duration;

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
            net::receive_announcements(state.clone(), sockets.announcements),
        );
        if let Some(a) = opts.advertise {
            tokio::spawn(net::advertise(state.clone(), a.port, a.path, a.name));
        }

        let mut keeper = net::LinkKeeper::new(net::NetOptions {
            interface: opts.interface,
            add_link_local: opts.add_link_local,
        });
        let (stop, mut stopped) = tokio::sync::oneshot::channel();
        let st = state.clone();
        let upkeep = tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(3));
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        keeper.tick(&st);
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
