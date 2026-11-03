//! mfd-bosun as a library: everything the program serves, as one router, for a program that wants to serve
//! it next to pages of its own (nested under a path of its choosing, or at "/"). The `mfd-bosun` program is
//! this plus its command line and its web server sockets.
//!
//!   /              the chartplotters, and ForwardScan
//!   /remote/       a chartplotter's screen (mfd-remote)
//!   /forwardscan/  ForwardScan (neon-forwardscan)
//!   /api/status    what the first page shows
//!
//! All the pages use relative paths, so the router can be nested.

mod auth;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use mfd_net::credentials::Credentials;
use mfd_net::discovery::Discovery;
use mfd_remote::Remote;
use neon_forwardscan::ForwardScan;
use rust_embed::RustEmbed;
use serde_json::json;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tower::Layer;

pub use mfd_net;
pub use mfd_remote;
pub use neon_forwardscan;

pub const APP: &str = "mfd-bosun";

pub struct Options {
    /// Where the chartplotters' passwords are kept
    pub data_dir: PathBuf,
    /// Password for the remote screens; None or empty: open to everyone on the network
    pub password: Option<String>,
    /// Chartplotters' addresses, for when they are not found by themselves
    pub plotters: Vec<IpAddr>,
    /// None: no ForwardScan
    pub forwardscan: Option<neon_forwardscan::Options>,
}

#[derive(Debug)]
pub enum Error {
    /// The stored passwords cannot be read
    Credentials(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Error::Credentials(e) => write!(f, "cannot read the stored passwords: {e}"),
        }
    }
}

impl std::error::Error for Error {}

pub struct Bosun {
    remote: Remote,
    forwardscan: Option<ForwardScan>,
    router: Router,
}

#[derive(RustEmbed)]
#[folder = "web/"]
struct Assets;

#[derive(Clone)]
struct App {
    remote: Remote,
    forwardscan: Option<neon_forwardscan::state::State>,
}

impl Bosun {
    /// Starts finding the chartplotters, and ForwardScan. Needs a Tokio runtime.
    pub fn start(opts: Options) -> Result<Self, Error> {
        let credentials = Credentials::open(opts.data_dir.join("credentials.json"))
            .map_err(|e| Error::Credentials(e.to_string()))?;
        log::info!("passwords kept in {}", credentials.path().display());

        let discovery = Discovery::start();
        for ip in &opts.plotters {
            discovery.add_address(SocketAddr::new(*ip, mfd_net::api::PORT));
            discovery.add_address(SocketAddr::new(*ip, mfd_net::nos::PORT));
            discovery.add_address(SocketAddr::new(*ip, mfd_net::garmin::CONTROL_PORT));
            discovery.add_address(SocketAddr::new(*ip, mfd_net::furuno::PORT));
        }
        let remote = Remote::new(
            discovery.clone(),
            credentials,
            "bosun@mfd-bosun.local",
            "MFD Bosun",
        );

        let fs = opts.forwardscan.and_then(|o| match ForwardScan::start(o) {
            Ok(fs) => Some(fs),
            Err(e) => {
                log::warn!("no ForwardScan: cannot receive {e}");
                None
            }
        });
        if let Some(fs) = &fs {
            tokio::spawn(follow_announcements(fs.state().clone(), discovery.clone()));
        }

        let app = App {
            remote: remote.clone(),
            forwardscan: fs.as_ref().map(|f| f.state().clone()),
        };
        let auth = auth::Auth::new(opts.password.as_deref());
        if auth.required() {
            log::info!("the remote screens need a login");
        }
        let mut router = Router::new()
            .route("/api/status", get(status))
            .with_state(Arc::new(app))
            .merge(auth.router())
            .nest("/remote", remote.router());
        if let Some(fs) = &fs {
            router = router.nest("/forwardscan", fs.router());
        }
        // before routing: a nested router does not get its bare "/remote/"
        let router = router
            .fallback(get(asset))
            .layer(middleware::from_fn_with_state(auth, auth::guard));
        let router = Router::new().fallback_service(middleware::from_fn(sections).layer(router));

        Ok(Bosun {
            remote,
            forwardscan: fs,
            router,
        })
    }

    /// The pages and their APIs, to serve or to nest.
    pub fn router(&self) -> Router {
        self.router.clone()
    }

    /// The chartplotters' remote screens, and the chartplotters found.
    pub fn remote(&self) -> &Remote {
        &self.remote
    }

    pub async fn stop(self) {
        if let Some(fs) = self.forwardscan {
            fs.stop().await;
        }
    }
}

/// The chartplotters announce themselves (for ForwardScan) with their name and address: that names them in
/// the list. mDNS finds them, on the port of the service each advertises; but a NOS unit need not advertise
/// its remote control there (a Zeus3S on 26.1 does not), while its announcement lists it in its `Services`:
/// so that port is probed too. Only that one: other devices announce themselves as well.
async fn follow_announcements(fs: neon_forwardscan::state::State, discovery: Discovery) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tick.tick().await;
        let devices = fs.net.devices();
        for (_, d) in devices {
            if let Some(ip) = d.ip {
                discovery.set_name(ip, &d.name);
                if let Some(v) = &d.app_version {
                    discovery.set_os_version(ip, v);
                }
                if let Some(port) = d.remote_port {
                    discovery.add_address(SocketAddr::new(IpAddr::V4(ip), port));
                }
            }
        }
    }
}

/// `/remote` and `/forwardscan` redirect to `/remote/` and `/forwardscan/` (their pages use relative
/// paths), and those go to the nested router as its "/" (which axum does not do by itself). The redirect is
/// relative too, so it works wherever this router is nested.
async fn sections(mut req: Request, next: Next) -> Response {
    const SECTIONS: [&str; 2] = ["/remote", "/forwardscan"];
    let p = req.uri().path();
    if let Some(s) = SECTIONS.iter().find(|s| p == **s) {
        let name = &s[1..];
        let to = match req.uri().query() {
            Some(q) => format!("{name}/?{q}"),
            None => format!("{name}/"),
        };
        return (StatusCode::PERMANENT_REDIRECT, [(header::LOCATION, to)]).into_response();
    }
    if let Some(s) = SECTIONS.iter().find(|s| p.strip_prefix(**s) == Some("/")) {
        let to = match req.uri().query() {
            Some(q) => format!("{s}/index.html?{q}"),
            None => format!("{s}/index.html"),
        };
        if let Ok(u) = to.parse() {
            *req.uri_mut() = u;
        }
    }
    next.run(req).await
}

async fn status(State(app): State<Arc<App>>) -> Json<serde_json::Value> {
    let plotters: Vec<_> = app
        .remote
        .discovery()
        .list()
        .iter()
        .map(|p| {
            let mut v =
                serde_json::to_value(mfd_net::discovery::PlotterView::from(p)).unwrap_or_default();
            v["hasPassword"] = app.remote.has_password(&p.serial).into();
            v
        })
        .collect();
    // Chartplotters we heard announce themselves but hold no address to reach: a failed probe never
    // becomes a listed plotter, so without this they would be invisible. Show the ones not already
    // listed, so the user sees they are there but on a network this computer is not on.
    let listed: std::collections::HashSet<String> = plotters
        .iter()
        .filter_map(|v| v["address"].as_str().map(str::to_string))
        .collect();
    let unreachable: Vec<serde_json::Value> = app
        .forwardscan
        .as_ref()
        .map(|s| {
            s.net
                .unreachable()
                .into_iter()
                .map(|(ll, d)| {
                    let address = d.ip.unwrap_or(ll).to_string();
                    let name = if d.name.is_empty() {
                        d.model.clone()
                    } else {
                        d.name.clone()
                    };
                    json!({ "name": name, "model": d.model, "address": address })
                })
                .filter(|v| !listed.contains(v["address"].as_str().unwrap_or("")))
                .collect()
        })
        .unwrap_or_default();
    let forwardscan = app.forwardscan.as_ref().map(|s| {
        let g = s.lock();
        json!({ "sources": neon_forwardscan::state::sources(&s.net, &g) })
    });
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "plotters": plotters,
        "unreachable": unreachable,
        "forwardScan": forwardscan,
    }))
}

async fn asset(uri: Uri) -> Response {
    let path = match uri.path().trim_start_matches('/') {
        "" => "index.html",
        p => p,
    };
    mfd_remote::serve_embedded::<Assets>(path)
}
