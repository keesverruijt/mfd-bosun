//! neon-forwardscan: shows a Navico NOS sonar device's ForwardScan in a web page, and puts that page on the
//! home screen of the Navico / B&G / Simrad / Lowrance chartplotters on the same network.

mod net;
mod netif;
mod protocol;
mod state;
mod web;

use clap::Parser;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::time::Duration;

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
        env_logger::Env::default().default_filter_or(format!("neon_forwardscan={level}")),
    )
    .init();

    let state = state::State::new(!args.read_only);
    for (what, task) in [
        (
            "ForwardScan (UDP 10755)",
            tokio::spawn(net::receive_pings(state.clone())),
        ),
        (
            "ForwardScan settings (UDP 10756)",
            tokio::spawn(net::receive_settings(state.clone())),
        ),
        (
            "chartplotter announcements (UDP 2052)",
            tokio::spawn(net::receive_announcements(state.clone())),
        ),
    ] {
        tokio::spawn(async move {
            match task.await {
                Ok(Err(e)) => {
                    log::error!("cannot receive {what}: {e}");
                    std::process::exit(1);
                }
                Err(e) => log::error!("{what}: {e}"),
                Ok(Ok(())) => {}
            }
        });
    }

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
    let app = web::router(state.clone());
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            log::error!("web server: {e}");
        }
    });
    if !args.no_advertise {
        tokio::spawn(net::advertise(state.clone(), args.port, args.name.clone()));
    }

    let mut keeper = net::LinkKeeper::new(net::NetOptions {
        interface: args.interface,
        add_link_local: !args.no_link_local,
    });
    let mut tick = tokio::time::interval(Duration::from_secs(3));
    let stop = shutdown();
    tokio::pin!(stop);
    loop {
        tokio::select! {
            _ = tick.tick() => {
                keeper.tick(&state);
                state.refresh_sources();
            }
            _ = &mut stop => break,
        }
    }
    keeper.cleanup();
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
