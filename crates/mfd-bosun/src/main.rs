//! mfd-bosun: one program on the boat's network for the Navico chartplotters, NEON and the older NOS. It
//! finds them, shows their screens in a browser (to view and control them from a phone, tablet or laptop),
//! gives their waypoints and routes, and shows a NOS sonar's ForwardScan, also on the chartplotters
//! themselves. What it serves is the library ([`mfd_bosun::Bosun`]); this is its command line and sockets.

use clap::Parser;
use mfd_bosun::{Bosun, APP};
use mfd_net::credentials;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;

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
    /// A chartplotter's address, for when it is not found by itself (mDNS, or NavNet for Furuno); may be
    /// given more than once
    #[arg(long = "plotter", value_name = "IP")]
    plotters: Vec<IpAddr>,
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

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let level = ["info", "debug", "trace"][args.verbose.min(2) as usize];
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(format!(
        "mfd_bosun={level},mfd_net={level},mfd_remote={level},neon_forwardscan={level}"
    )))
    .init();

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

    let bosun = match Bosun::start(mfd_bosun::Options {
        data_dir: args
            .data_dir
            .clone()
            .unwrap_or_else(|| credentials::default_dir(APP)),
        password: args.password.clone(),
        plotters: args.plotters.clone(),
        forwardscan: (!args.no_forwardscan).then(|| neon_forwardscan::Options {
            interface: args.interface.clone(),
            add_link_local: !args.no_link_local,
            allow_control: !args.read_only,
            advertise: (!args.no_advertise).then(|| neon_forwardscan::Advertise {
                name: args.name.clone(),
                port: args.port,
                path: "/forwardscan/".into(),
            }),
        }),
    }) {
        Ok(b) => b,
        Err(e) => {
            log::error!("{e}");
            std::process::exit(1);
        }
    };
    let router = bosun.router();

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
    bosun.stop().await;
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
