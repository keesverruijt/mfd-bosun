//! `cargo run -p mfd-net --example probe -- <host> [password]`: the info, and with a password a pairing and
//! the signalling socket, to try the TLS and the API against a chartplotter.

use std::net::SocketAddr;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().expect("usage: probe <host> [password]");
    let addr: SocketAddr = format!("{host}:{}", mfd_net::api::PORT)
        .parse()
        .expect("an IP address");
    let info = mfd_net::api::info(addr).await.expect("info");
    println!("{info:?}");
    let Some(password) = args.next().or_else(|| std::env::var("NEON_PSK").ok()) else {
        return;
    };
    let who = mfd_net::api::Identity {
        email: "probe@mfd-bosun.local",
        device_name: "mfd-net probe",
    };
    let token = mfd_net::api::pair(addr, &password, &who)
        .await
        .expect("pair");
    println!("token: {}…", &token[..20.min(token.len())]);
    let mut ws = mfd_net::ws::connect_receiver(addr, 42, &token)
        .await
        .expect("receiver socket");
    println!("receiver socket open");
    let _ = ws.close(None).await;
}
