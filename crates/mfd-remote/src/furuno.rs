//! A Furuno TimeZero unit's remote: the page's MCU-006 keys, pointer and knob, passed to the canboat that
//! plays the remote on the unit's bus (see [`mfd_net::canboat`]).
//!
//! One canboat connection per page: what a page holds down is let go when it leaves, as canboat lets go of
//! whatever a connection held when it closes.

use crate::{close, close_message};
use axum::extract::ws::{Message, WebSocket};
use futures_util::StreamExt;
use mfd_net::canboat::{self, Remote};
use mfd_net::discovery::Discovery;

pub async fn serve(discovery: Discovery, mut browser: WebSocket, serial: String) {
    let Some(addr) = discovery
        .get(&serial)
        .filter(|p| p.online())
        .and_then(|p| p.furuno?.remote)
    else {
        let _ = browser
            .send(close_message(close::UNKNOWN, "no remote control for it"))
            .await;
        return;
    };
    let mut remote = match Remote::connect(addr).await {
        Ok(r) => r,
        Err(e) => {
            log::info!("remote for {serial}: canboat at {addr}: {e}");
            let _ = browser
                .send(close_message(close::UPSTREAM, &e.to_string()))
                .await;
            return;
        }
    };
    log::info!("remote for {serial} open, through canboat at {addr}");
    while let Some(m) = browser.next().await {
        let t = match m {
            Ok(Message::Text(t)) => t,
            Ok(Message::Close(_)) | Err(_) => break,
            Ok(_) => continue,
        };
        let reply = match serde_json::from_str(t.as_str()) {
            Ok(cmd) => match canboat::checked(&cmd) {
                Some(cmd) => match remote.command(cmd).await {
                    Ok(r) => r,
                    Err(e) => {
                        log::info!("remote for {serial}: {e}");
                        let _ = browser
                            .send(close_message(close::UPSTREAM, &e.to_string()))
                            .await;
                        break;
                    }
                },
                None => serde_json::json!({ "ok": false, "error": "not a remote command" }),
            },
            Err(e) => serde_json::json!({ "ok": false, "error": e.to_string() }),
        };
        if browser
            .send(Message::Text(reply.to_string().into()))
            .await
            .is_err()
        {
            break;
        }
    }
    log::info!("remote for {serial} closed");
}
