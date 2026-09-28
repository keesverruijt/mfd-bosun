//! The web server: the ForwardScan page (embedded), its WebSocket stream and the settings API.
//!   GET  /                -> the page
//!   GET  /stream          -> WebSocket: binary ForwardScan frames, JSON config/sources/settings/network messages
//!   GET  /api/state       -> { allowControl, forwardScan: [{ address, name, settings }], network }
//!   POST /api/forwardscan -> { source?, paused?, depth?, range?, autoRange?, noiseRejection? }
//! Depths and distances are in feet throughout (frames, settings, API), as the sonar has them.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State as AxState;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rust_embed::RustEmbed;
use serde_json::json;
use std::net::Ipv4Addr;
use tokio::sync::broadcast::error::RecvError;

use crate::protocol;
use crate::state::{Out, State};

#[derive(RustEmbed)]
#[folder = "web/"]
struct Assets;

pub fn router(state: State) -> Router {
    Router::new()
        .route("/stream", get(stream))
        .route("/api/state", get(api_state))
        .route("/api/forwardscan", post(api_forwardscan))
        .fallback(get(asset))
        .with_state(state)
}

async fn asset(uri: Uri) -> Response {
    let path = match uri.path().trim_start_matches('/') {
        "" | "index.html" => "forwardscan.html",
        p => p,
    };
    let Some(f) = Assets::get(path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    let mime = match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        _ => "application/octet-stream",
    };
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        f.data,
    )
        .into_response()
}

async fn stream(ws: WebSocketUpgrade, AxState(state): AxState<State>) -> Response {
    ws.on_upgrade(move |socket| client(socket, state))
}

async fn client(mut socket: WebSocket, state: State) {
    let mut rx = state.tx.subscribe();
    for m in state.hello() {
        if socket.send(Message::Text(m.into())).await.is_err() {
            return;
        }
    }
    loop {
        tokio::select! {
            out = rx.recv() => {
                let msg = match out {
                    Ok(Out::Frame(b)) => Message::Binary(b),
                    Ok(Out::Text(t)) => Message::Text(t.into()),
                    Err(RecvError::Lagged(_)) => continue, // a slow client skips pings
                    Err(RecvError::Closed) => return,
                };
                if socket.send(msg).await.is_err() {
                    return;
                }
            }
            r = socket.recv() => match r {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                _ => {}
            }
        }
    }
}

async fn api_state(AxState(state): AxState<State>) -> Json<serde_json::Value> {
    let g = state.lock();
    let fs: Vec<_> = g
        .blocks
        .iter()
        .map(|(ip, b)| {
            let name = g.devices.get(ip).map(|d| d.name.clone()).unwrap_or_else(|| ip.to_string());
            json!({ "address": ip.to_string(), "name": name, "settings": protocol::parse_control(b) })
        })
        .collect();
    Json(json!({ "allowControl": state.allow_control, "forwardScan": fs, "network": g.notice }))
}

fn fail(status: StatusCode, error: impl Into<String>) -> Response {
    (status, Json(json!({ "ok": false, "error": error.into() }))).into_response()
}

async fn api_forwardscan(AxState(state): AxState<State>, body: String) -> Response {
    if !state.allow_control {
        return fail(
            StatusCode::FORBIDDEN,
            "changing settings is off (started with --read-only)",
        );
    }
    let change: protocol::Change =
        match serde_json::from_str(if body.trim().is_empty() { "{}" } else { &body }) {
            Ok(c) => c,
            Err(e) => return fail(StatusCode::BAD_REQUEST, e.to_string()),
        };
    let fields = change.fields();
    if fields.is_empty() {
        return fail(StatusCode::BAD_REQUEST, "nothing to change");
    }
    let (ip, latest, have_ll, notice) = {
        let g = state.lock();
        let sonars: Vec<Ipv4Addr> = g.blocks.keys().copied().collect();
        let ip = match &change.source {
            Some(s) => sonars
                .iter()
                .copied()
                .find(|a| a.to_string() == *s || g.devices.get(a).is_some_and(|d| d.name == *s)),
            None if sonars.len() == 1 => Some(sonars[0]),
            None => None,
        };
        let Some(ip) = ip else {
            return match (&change.source, sonars.len()) {
                (Some(s), _) => fail(
                    StatusCode::BAD_REQUEST,
                    format!("no ForwardScan settings from \"{s}\""),
                ),
                (None, 0) => fail(
                    StatusCode::CONFLICT,
                    "no ForwardScan settings seen from a sonar yet",
                ),
                _ => fail(StatusCode::BAD_REQUEST, "several sonars: give \"source\""),
            };
        };
        (
            ip,
            g.blocks[&ip].clone(),
            g.control.link_local.is_some(),
            g.notice.clone(),
        )
    };
    let block = match protocol::build_control(&latest, &change) {
        Ok(b) => b,
        Err(e) => return fail(StatusCode::BAD_REQUEST, e),
    };
    if let Err(e) = crate::net::send_control(&state, ip, &block) {
        let why = if have_ll {
            e.to_string()
        } else {
            notice.map(|n| n.text).unwrap_or_else(|| e.to_string())
        };
        return fail(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("could not send to {ip}: {why}"),
        );
    }
    log::info!("settings to {} ({ip}): {}", state.name_of(ip), body.trim());
    Json(json!({ "ok": true, "sent": fields })).into_response()
}
