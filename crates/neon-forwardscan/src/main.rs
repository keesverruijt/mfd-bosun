//! neon-forwardscan: shows a Navico NOS sonar device's ForwardScan in a web page, and puts that page on the
//! home screen of the Navico / B&G / Simrad / Lowrance chartplotters on the same network.

use clap::Parser;
use neon_forwardscan::{netif, Advertise, ForwardScan, Options};
use std::net::{Ipv4Addr, SocketAddrV4};

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// TCP port of the web page
    #[arg(short, long, default_value_t = 6022)]
    port: u16,
    /// The interface the sonar is on (default: found from the sonar's announcement)
    #[arg(short, long)]
    interface: Option<String>,
    /// Never add a 169.254.x.x address, even when changing settings needs one
    #[arg(long)]
    no_link_local: bool,
    /// Do not let the page change the sonar's ForwardScan settings
    #[arg(long)]
    read_only: bool,
    /// Do not advertise the page to the chartplotters
    #[arg(long)]
    no_advertise: bool,
    /// Name of the tile on the chartplotters
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
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or(format!("neon_forwardscan={level},mfd_net={level}")),
    )
    .init();

    let fs = match ForwardScan::start(Options {
        interface: args.interface,
        add_link_local: !args.no_link_local,
        allow_control: !args.read_only,
        advertise: (!args.no_advertise).then(|| Advertise {
            name: args.name.clone(),
            port: args.port,
            path: "/".into(),
        }),
    }) {
        Ok(fs) => fs,
        Err(e) => {
            log::error!("cannot receive {e}");
            std::process::exit(1);
        }
    };

    let listener =
        match tokio::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, args.port))
            .await
        {
            Ok(l) => l,
            Err(e) => {
                log::error!("cannot listen on port {}: {e} (use --port)", args.port);
                std::process::exit(1);
            }
        };
    for i in netif::list().iter().filter(|i| !i.is_virtual()) {
        if let Some(a) = i.preferred() {
            log::info!("page at http://{a}:{}/ ({})", args.port, i.name);
        }
    }
    let app = fs.router();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            log::error!("web server: {e}");
        }
    });
    shutdown().await;
    fs.stop().await;
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
