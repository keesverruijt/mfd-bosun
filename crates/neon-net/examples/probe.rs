//! `cargo run -p neon-net --example probe -- <host> [password]`: the info, and with a password a pairing and
//! the signalling socket, to try the TLS and the API against a chartplotter.

use std::net::SocketAddr;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let host = args.next().expect("usage: probe <host> [password]");
    let addr: SocketAddr = format!("{host}:{}", neon_net::api::PORT)
        .parse()
        .expect("an IP address");
    let info = neon_net::api::info(addr).await.expect("info");
    println!("{info:?}");
    let Some(password) = args.next().or_else(|| std::env::var("NEON_PSK").ok()) else {
        return;
    };
    let who = neon_net::api::Identity {
        email: "probe@navico-neon-extender.local",
        device_name: "neon-net probe",
    };
    let token = neon_net::api::pair(addr, &password, &who)
        .await
        .expect("pair");
    println!("token: {}…", &token[..20.min(token.len())]);
    let mut ws = neon_net::ws::connect_receiver(addr, 42, &token)
        .await
        .expect("receiver socket");
    println!("receiver socket open");
    let _ = ws.close(None).await;
}
