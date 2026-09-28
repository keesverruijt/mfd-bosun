//! mfd-bosun: one program on the boat's network for the Navico chartplotters, NEON and the older NOS. It
//! finds them, shows their screens in a browser (to view and control them from a phone, tablet or laptop),
//! gives their waypoints and routes, and shows a NOS sonar's ForwardScan, also on the chartplotters
//! themselves.
//!
//!   /              the chartplotters, and ForwardScan
//!   /remote/       a chartplotter's screen (mfd-remote)
//!   /forwardscan/  ForwardScan (neon-forwardscan)
//!   /api/status    what the first page shows

mod auth;

use axum::extract::{Request, State};
use axum::http::{header, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::ServiceExt;
use axum::{Json, Router};
use clap::Parser;
use mfd_net::credentials::{self, Credentials};
use mfd_net::discovery::Discovery;
use mfd_remote::Remote;
use neon_forwardscan::ForwardScan;
use rust_embed::RustEmbed;
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tower::Layer;

const APP: &str = "mfd-bosun";

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// TCP port of the web pages
    #[arg(short, long, default_value_t = 6022)]
    port: u16,
    /// Addresses the web pages are served on: "::" is all, IPv6 and IPv4; "0.0.0.0" all IPv4; or given
    /// addresses. May be given more than once, or comma-separated
    #[arg(
        long,
        value_name = "IP",
        env = "MFD_BOSUN_LISTEN",
        value_delimiter = ',',
        default_value = "::"
    )]
    listen: Vec<IpAddr>,
    /// Where the chartplotters' passwords are kept (default: $STATE_DIRECTORY, else the user's config directory)
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Password for these pages' remote screens (default: none, open to everyone on the network)
    #[arg(long, env = "MFD_BOSUN_PASSWORD", hide_env_values = true)]
    password: Option<String>,
    /// A chartplotter's address, for when it is not found by itself (mDNS); may be given more than once
    #[arg(long = "plotter", value_name = "IP")]
    plotters: Vec<IpAddr>,
    /// Do not look for chartplotters with mDNS
    #[arg(long)]
    no_mdns: bool,
    /// No ForwardScan
    #[arg(long)]
    no_forwardscan: bool,
    /// The interface the sonar is on (default: found from the sonar's announcement)
    #[arg(short, long)]
    interface: Option<String>,
    /// Never add a 169.254.x.x address, even when changing ForwardScan settings needs one
    #[arg(long)]
    no_link_local: bool,
    /// Do not let the page change the sonar's ForwardScan settings
    #[arg(long)]
    read_only: bool,
    /// Do not put a ForwardScan tile on the chartplotters
    #[arg(long)]
    no_advertise: bool,
    /// Name of the ForwardScan tile on the chartplotters
    #[arg(long, default_value = "ForwardScan")]
    name: String,
    /// More logging (-v debug, -vv trace)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,
}

#[derive(RustEmbed)]
#[folder = "web/"]
struct Assets;

#[derive(Clone)]
struct App {
    remote: Remote,
    forwardscan: Option<neon_forwardscan::state::State>,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let level = ["info", "debug", "trace"][args.verbose.min(2) as usize];
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(format!(
        "mfd_bosun={level},mfd_net={level},mfd_remote={level},neon_forwardscan={level}"
    )))
    .init();

    let dir = args
        .data_dir
        .clone()
        .unwrap_or_else(|| credentials::default_dir(APP));
    let credentials = match Credentials::open(dir.join("credentials.json")) {
        Ok(c) => c,
        Err(e) => {
            log::error!("cannot read the stored passwords: {e}");
            std::process::exit(1);
        }
    };
    log::info!("passwords kept in {}", credentials.path().display());

    let mut listeners = Vec::new();
    for ip in &args.listen {
        match listen(*ip, args.port) {
            Ok(l) => listeners.push(l),
            Err(e) => {
                log::error!(
                    "cannot listen on {}: {e} (see --port, --listen)",
                    SocketAddr::new(*ip, args.port)
                );
                std::process::exit(1);
            }
        }
    }

    let discovery = Discovery::start(!args.no_mdns);
    for ip in &args.plotters {
        discovery.add_address(SocketAddr::new(*ip, mfd_net::api::PORT));
        discovery.add_address(SocketAddr::new(*ip, mfd_net::nos::PORT));
    }
    let remote = Remote::new(
        discovery.clone(),
        credentials,
        "bosun@mfd-bosun.local",
        "MFD Bosun",
    );

    let fs = if args.no_forwardscan {
        None
    } else {
        match ForwardScan::start(neon_forwardscan::Options {
            interface: args.interface.clone(),
            add_link_local: !args.no_link_local,
            allow_control: !args.read_only,
            advertise: (!args.no_advertise).then(|| neon_forwardscan::Advertise {
                name: args.name.clone(),
                port: args.port,
                path: "/forwardscan/".into(),
            }),
        }) {
            Ok(fs) => Some(fs),
            Err(e) => {
                log::warn!("no ForwardScan: cannot receive {e}");
                None
            }
        }
    };
    if let Some(fs) = &fs {
        tokio::spawn(follow_announcements(fs.state().clone(), discovery.clone()));
    }

    let app = App {
        remote: remote.clone(),
        forwardscan: fs.as_ref().map(|f| f.state().clone()),
    };
    let auth = auth::Auth::new(args.password.as_deref());
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
    let router = middleware::from_fn(sections).layer(router);

    for ip in args.listen.iter().filter(|ip| !ip.is_unspecified()) {
        log::info!("pages at http://{}/", SocketAddr::new(*ip, args.port));
    }
    for i in neon_forwardscan::netif::list()
        .iter()
        .filter(|i| !i.is_virtual())
    {
        if let Some(a) = i
            .preferred()
            .filter(|a| serves(&args.listen, IpAddr::V4(*a)))
        {
            log::info!("pages at http://{a}:{}/ ({})", args.port, i.name);
        }
    }
    for listener in listeners {
        let router = router.clone();
        tokio::spawn(async move {
            if let Err(e) = axum::serve(listener, router.into_make_service()).await {
                log::error!("web server: {e}");
            }
        });
    }

    shutdown().await;
    if let Some(fs) = fs {
        fs.stop().await;
    }
}

/// A web server socket on `ip`. "::" is IPv6 and IPv4 on one socket (IPv6-only switched off explicitly,
/// as Windows has it on), or IPv4 alone where the host has no IPv6.
fn listen(ip: IpAddr, port: u16) -> std::io::Result<tokio::net::TcpListener> {
    use socket2::{Domain, Socket, Type};
    let bind = |ip: IpAddr| -> std::io::Result<Socket> {
        let s = Socket::new(
            Domain::for_address(SocketAddr::new(ip, port)),
            Type::STREAM,
            None,
        )?;
        if ip.is_ipv6() {
            s.set_only_v6(!ip.is_unspecified())?;
        }
        s.set_reuse_address(true)?;
        s.bind(&SocketAddr::new(ip, port).into())?;
        Ok(s)
    };
    let s = match bind(ip) {
        Err(e)
            if ip == IpAddr::V6(Ipv6Addr::UNSPECIFIED)
                && e.kind() != std::io::ErrorKind::AddrInUse =>
        {
            log::debug!("no IPv6 ({e}): IPv4 only");
            bind(IpAddr::V4(Ipv4Addr::UNSPECIFIED))?
        }
        r => r?,
    };
    s.listen(1024)?;
    s.set_nonblocking(true)?;
    tokio::net::TcpListener::from_std(s.into())
}

/// Is `a` served, given the --listen addresses?
fn serves(listen: &[IpAddr], a: IpAddr) -> bool {
    listen
        .iter()
        .any(|l| *l == a || l.is_unspecified() && (l.is_ipv6() || a.is_ipv4() == l.is_ipv4()))
}

/// The chartplotters announce themselves (for ForwardScan) with their name and address: that names them in
/// the list, and finds them when mDNS does not.
async fn follow_announcements(fs: neon_forwardscan::state::State, discovery: Discovery) {
    let mut tick = tokio::time::interval(Duration::from_secs(5));
    loop {
        tick.tick().await;
        let devices: Vec<_> = fs.lock().devices.values().cloned().collect();
        for d in devices {
            if let Some(ip) = d.ip {
                discovery.set_name(ip, &d.name);
                discovery.add_address(SocketAddr::new(IpAddr::V4(ip), mfd_net::api::PORT));
                discovery.add_address(SocketAddr::new(IpAddr::V4(ip), mfd_net::nos::PORT));
            }
        }
    }
}

/// `/remote` and `/forwardscan` redirect to `/remote/` and `/forwardscan/` (their pages use relative
/// paths), and those go to the nested router as its "/" (which axum does not do by itself).
async fn sections(mut req: Request, next: Next) -> Response {
    const SECTIONS: [&str; 2] = ["/remote", "/forwardscan"];
    let p = req.uri().path();
    if SECTIONS.contains(&p) {
        let to = match req.uri().query() {
            Some(q) => format!("{p}/?{q}"),
            None => format!("{p}/"),
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
    let forwardscan = app.forwardscan.as_ref().map(|s| {
        let g = s.lock();
        json!({ "sources": neon_forwardscan::state::sources(&g) })
    });
    Json(
        json!({ "version": env!("CARGO_PKG_VERSION"), "plotters": plotters, "forwardScan": forwardscan }),
    )
}

async fn asset(uri: Uri) -> Response {
    let path = match uri.path().trim_start_matches('/') {
        "" => "index.html",
        p => p,
    };
    mfd_remote::serve_embedded::<Assets>(path)
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_what_it_listens_on() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let v4 = ip("10.56.0.1");
        let v6 = ip("fd9e:f410:82bf::a38");
        assert!(serves(&[ip("::")], v4) && serves(&[ip("::")], v6));
        assert!(serves(&[ip("0.0.0.0")], v4) && !serves(&[ip("0.0.0.0")], v6));
        assert!(serves(&[v4], v4) && !serves(&[v4], ip("192.168.1.2")) && !serves(&[v4], v6));
        assert!(serves(&[v4, v6], v6));
    }
}
