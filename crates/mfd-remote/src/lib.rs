//! The remote screen of a NEON chartplotter in a browser.
//!
//! The chartplotter runs a WebRTC screen-mirroring sender and a signalling server (TLS, :8444). The browser
//! answers the chartplotter's WebRTC offer itself; the video and the input data channels go peer to peer
//! between browser and chartplotter. This side is only what a browser cannot do: pairing with the
//! chartplotter's WiFi password, and the signalling socket, which wants an `Authorization: Bearer` header
//! and trust-all TLS. It passes the signalling messages through unchanged.
//!
//! The older NOS units have no WebRTC: for them this side plays the chartplotter's part itself (see [`nos`]).
//!
//! Routes, all relative so the router can be nested:
//!   GET    /                                -> the remote screen page (`?plotter=<serial>`)
//!   GET    /api/plotters                    -> [{ serial, model, brand, osVersion, name, address, online, hasPassword }]
//!   POST   /api/plotters/{serial}/password  -> { password }: pairs with it, and keeps it when it works
//!   DELETE /api/plotters/{serial}/password  -> forgets it
//!   GET    /api/plotters/{serial}/export.gpx -> its waypoints and routes (and a NOS unit's tracks), as GPX
//!   GET    /api/plotters/{serial}/backup/{WaypointsRoutesTracks.usr|Settings.set} -> a NOS unit's own backups
//!   GET    /signal?plotter=<serial>&key=<n> -> WebSocket to the chartplotter's /ws/receiver/<n>

mod crypto;
mod gpx;
mod h264;
mod nos;
mod usr;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use mfd_net::api::{self, PairError};
use mfd_net::credentials::Credentials;
use mfd_net::discovery::{Discovery, Plotter, PlotterView};
use mfd_net::ws::{self as upstream, ConnectError};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message as UpMessage;

/// Close codes the page acts on.
pub mod close {
    /// The chartplotter's password is not known, or no longer works: ask for it.
    pub const PASSWORD: u16 = 4001;
    /// Pairing failed for another reason.
    pub const PAIRING: u16 = 4002;
    /// The chartplotter closed the session at the page's `receiver-online`, without a word: it will not
    /// share its screen (a Zeus S on 2.5.208). Retrying does not help, so the page stops.
    pub const REFUSED: u16 = 4003;
    /// No such chartplotter, or it does not answer.
    pub const UNKNOWN: u16 = 4004;
    pub const UPSTREAM: u16 = 1011;
}

/// A chartplotter's screen mirroring breaks, until the chartplotter is restarted, when a new receiver
/// comes online the moment the last one left, as a page reload does: so a new session to a chartplotter
/// waits until this long after the last one closed.
const SETTLE: Duration = Duration::from_secs(2);

#[derive(RustEmbed)]
#[folder = "web/"]
struct Assets;

#[derive(Clone)]
pub struct Remote {
    discovery: Discovery,
    credentials: Credentials,
    /// Access tokens by serial: they stay valid for a long time, so pair once and reuse.
    tokens: Arc<Mutex<HashMap<String, String>>>,
    /// When the last signalling session to each chartplotter (by serial) closed: see [`SETTLE`].
    closed_at: Arc<Mutex<HashMap<String, Instant>>>,
    identity: Arc<(String, String)>,
    /// NOS units' screens
    feeds: nos::Feeds,
    /// What NOS units know this program by
    nos_mac: [u8; 6],
}

impl Remote {
    /// `email` and `device_name` go into the pairing request; the chartplotter lists them as the paired app.
    pub fn new(
        discovery: Discovery,
        credentials: Credentials,
        email: &str,
        device_name: &str,
    ) -> Self {
        Remote {
            discovery,
            credentials,
            tokens: Default::default(),
            closed_at: Default::default(),
            identity: Arc::new((email.to_string(), device_name.to_string())),
            feeds: Default::default(),
            nos_mac: nos::client_mac(&format!("{device_name}@{}", hostname())),
        }
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/api/plotters", get(plotters))
            .route(
                "/api/plotters/{serial}/password",
                post(set_password).delete(forget_password),
            )
            .route("/api/plotters/{serial}/export.gpx", get(export_gpx))
            .route("/api/plotters/{serial}/backup/{file}", get(nos_backup))
            .route("/signal", get(signal))
            .fallback(get(asset))
            .with_state(self.clone())
    }

    pub fn discovery(&self) -> &Discovery {
        &self.discovery
    }

    /// Whether the chartplotter's screen can be opened without asking for a password: a NOS unit needs none.
    pub fn has_password(&self, serial: &str) -> bool {
        self.discovery.get(serial).is_some_and(|p| p.nos.is_some())
            || self.credentials.password(serial).is_some()
    }

    fn cached_token(&self, serial: &str) -> Option<String> {
        self.tokens
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(serial)
            .cloned()
    }

    fn set_token(&self, serial: &str, token: Option<String>) {
        let mut g = self.tokens.lock().unwrap_or_else(|e| e.into_inner());
        match token {
            Some(t) => g.insert(serial.to_string(), t),
            None => g.remove(serial),
        };
    }

    async fn pair(&self, p: &Plotter, password: &str) -> Result<String, PairError> {
        let who = api::Identity {
            email: &self.identity.0,
            device_name: &self.identity.1,
        };
        let token = api::pair(p.addr, password, &who).await?;
        log::info!("paired with {} ({})", p.model, p.serial);
        self.set_token(&p.serial, Some(token.clone()));
        Ok(token)
    }

    /// All waypoints and routes of a chartplotter (read only), pairing first when needed.
    async fn waypoints_and_routes(
        &self,
        serial: &str,
    ) -> Result<(Plotter, Vec<serde_json::Value>, Vec<serde_json::Value>), (StatusCode, String)>
    {
        use api::{SyncError, SyncKind};
        let Some(p) = self.discovery.get(serial).filter(|p| p.online()) else {
            return Err((
                StatusCode::NOT_FOUND,
                format!("chartplotter {serial} not found"),
            ));
        };
        let Some(password) = self.credentials.password(serial) else {
            return Err((
                StatusCode::FORBIDDEN,
                "no password for this chartplotter yet".into(),
            ));
        };
        let mut fresh = false;
        loop {
            let token = match self.cached_token(serial) {
                Some(t) if !fresh => t,
                _ => {
                    fresh = true;
                    self.pair(&p, &password)
                        .await
                        .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?
                }
            };
            let read = async {
                let w = api::sync_read(p.addr, &token, SyncKind::Waypoints).await?;
                let r = api::sync_read(p.addr, &token, SyncKind::Routes).await?;
                Ok::<_, SyncError>((w, r))
            };
            match read.await {
                Ok((w, r)) => return Ok((p, w, r)),
                Err(SyncError::Unauthorized) if !fresh => {
                    self.set_token(serial, None);
                    fresh = true;
                }
                Err(e) => return Err((StatusCode::BAD_GATEWAY, e.to_string())),
            }
        }
    }

    /// The chartplotter's signalling socket for receiver `key`, pairing first when needed.
    async fn open_upstream(
        &self,
        serial: &str,
        key: u64,
    ) -> Result<upstream::Receiver, (u16, String)> {
        let Some(p) = self.discovery.get(serial).filter(|p| p.online()) else {
            return Err((close::UNKNOWN, format!("chartplotter {serial} not found")));
        };
        let Some(password) = self.credentials.password(serial) else {
            return Err((close::PASSWORD, "password needed".into()));
        };
        let mut fresh = false;
        loop {
            let token = match self.cached_token(serial) {
                Some(t) if !fresh => t,
                _ => {
                    fresh = true;
                    match self.pair(&p, &password).await {
                        Ok(t) => t,
                        Err(PairError::WrongPassword) => {
                            return Err((close::PASSWORD, "the password no longer works".into()))
                        }
                        Err(PairError::Other(e)) => return Err((close::PAIRING, e)),
                    }
                }
            };
            match upstream::connect_receiver(p.addr, key, &token).await {
                Ok(u) => return Ok(u),
                // an old token: pair again, once
                Err(ConnectError::Unauthorized) if !fresh => {
                    log::info!("token for {serial} refused, pairing again");
                    self.set_token(serial, None);
                    fresh = true;
                }
                Err(e) => return Err((close::UPSTREAM, e.to_string())),
            }
        }
    }

    /// Waits until [`SETTLE`] after the last session to this chartplotter closed.
    async fn settle(&self, serial: &str) {
        let since = self
            .closed_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(serial)
            .map(|t| t.elapsed());
        if let Some(wait) = since.and_then(|e| SETTLE.checked_sub(e)) {
            log::debug!(
                "signalling for {serial}: waiting {wait:?} for the last session to be released"
            );
            tokio::time::sleep(wait).await;
        }
    }

    async fn bridge(self, browser: WebSocket, serial: String, key: u64) {
        if let Some(p) = self.discovery.get(&serial).filter(|p| p.nos.is_some()) {
            if p.online() {
                let name = self.identity.1.clone();
                return nos::serve(self.feeds.clone(), browser, p, key, self.nos_mac, name).await;
            }
        }
        self.settle(&serial).await;
        let (mut to_browser, mut from_browser) = browser.split();
        let up = match self.open_upstream(&serial, key).await {
            Ok(u) => u,
            Err((code, reason)) => {
                log::info!("signalling for {serial}: {reason}");
                let _ = to_browser.send(close_message(code, &reason)).await;
                return;
            }
        };
        log::info!("signalling for {serial} open (receiver {key})");
        let (mut to_plotter, mut from_plotter) = up.split();
        // whether the chartplotter said anything, and whether the page asked it for its screen
        let (mut heard, mut asked) = (false, false);
        loop {
            tokio::select! {
                m = from_plotter.next() => match m {
                    Some(Ok(UpMessage::Text(t))) => {
                        heard = true;
                        log::debug!("signalling for {serial}: from the chartplotter: {}", brief(t.as_str()));
                        if to_browser.send(Message::Text(t.as_str().into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(UpMessage::Binary(b))) => {
                        if to_browser.send(Message::Binary(b)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(UpMessage::Close(f))) => {
                        let (code, reason) = f
                            .map(|f| (u16::from(f.code), f.reason.to_string()))
                            .unwrap_or((1000, String::new()));
                        log::info!("signalling for {serial}: closed by the chartplotter ({code} {reason})");
                        let m = if asked && !heard && code == 1000 {
                            log::info!("signalling for {serial}: the chartplotter refuses to share its screen");
                            close_message(close::REFUSED, "the chartplotter refuses to share its screen")
                        } else {
                            close_message(if sendable(code) { code } else { close::UPSTREAM }, &reason)
                        };
                        let _ = to_browser.send(m).await;
                        break;
                    }
                    Some(Ok(_)) => {} // ping, pong: tungstenite answers them
                    Some(Err(e)) => {
                        log::info!("signalling for {serial}: {e}");
                        let _ = to_browser.send(close_message(close::UPSTREAM, &e.to_string())).await;
                        break;
                    }
                    None => {
                        let _ = to_browser.send(close_message(close::UPSTREAM, "chartplotter went away")).await;
                        break;
                    }
                },
                m = from_browser.next() => match m {
                    Some(Ok(Message::Text(t))) => {
                        log::debug!("signalling for {serial}: from the page: {}", brief(t.as_str()));
                        asked |= t.as_str().contains("\"receiver-online\"");
                        if to_plotter.send(UpMessage::Text(t.as_str().into())).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Binary(b))) => {
                        if to_plotter.send(UpMessage::Binary(b)).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {}
                },
            }
        }
        let _ = to_plotter.send(UpMessage::Close(None)).await;
        self.closed_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(serial.clone(), Instant::now());
        log::info!("signalling for {serial} closed");
    }
}

/// This machine's name, for what a NOS unit knows this program by.
/// A signalling message for the log: SDP and candidates run long.
fn brief(t: &str) -> &str {
    match t.char_indices().nth(300) {
        Some((i, _)) => &t[..i],
        None => t,
    }
}

fn hostname() -> String {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .find_map(|f| std::fs::read_to_string(f).ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// Close codes a server may send.
fn sendable(code: u16) -> bool {
    matches!(code, 1000..=1003 | 1007..=1011 | 3000..=4999)
}

fn close_message(code: u16, reason: &str) -> Message {
    // a close frame's reason is at most 123 bytes
    let mut end = reason.len().min(123);
    while !reason.is_char_boundary(end) {
        end -= 1;
    }
    Message::Close(Some(CloseFrame {
        code,
        reason: reason[..end].into(),
    }))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PlotterJson {
    #[serde(flatten)]
    view: PlotterView,
    has_password: bool,
}

async fn plotters(State(r): State<Remote>) -> Json<Vec<PlotterJson>> {
    Json(
        r.discovery
            .list()
            .iter()
            .map(|p| PlotterJson {
                view: p.into(),
                has_password: r.has_password(&p.serial),
            })
            .collect(),
    )
}

fn fail(status: StatusCode, error: impl Into<String>) -> Response {
    (status, Json(json!({ "ok": false, "error": error.into() }))).into_response()
}

#[derive(Deserialize)]
struct PasswordBody {
    password: String,
}

async fn set_password(
    State(r): State<Remote>,
    Path(serial): Path<String>,
    Json(body): Json<PasswordBody>,
) -> Response {
    let Some(p) = r.discovery.get(&serial) else {
        return fail(
            StatusCode::NOT_FOUND,
            format!("chartplotter {serial} not found"),
        );
    };
    if body.password.is_empty() {
        return fail(StatusCode::BAD_REQUEST, "no password");
    }
    match r.pair(&p, &body.password).await {
        Ok(_) => {}
        Err(PairError::WrongPassword) => return fail(StatusCode::UNAUTHORIZED, "wrong password"),
        Err(PairError::Other(e)) => return fail(StatusCode::BAD_GATEWAY, e),
    }
    if let Err(e) = r.credentials.set_password(&serial, &body.password) {
        log::error!("cannot store the password: {e}");
        return fail(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("paired, but cannot store the password: {e}"),
        );
    }
    log::info!("password for {} ({serial}) stored", p.model);
    Json(json!({ "ok": true })).into_response()
}

async fn forget_password(State(r): State<Remote>, Path(serial): Path<String>) -> Response {
    r.set_token(&serial, None);
    match r.credentials.forget(&serial) {
        Ok(()) => Json(json!({ "ok": true })).into_response(),
        Err(e) => fail(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
    }
}

async fn export_gpx(State(r): State<Remote>, Path(serial): Path<String>) -> Response {
    let (p, waypoints, routes, tracks) = match r.discovery.get(&serial).filter(|p| p.nos.is_some())
    {
        // a NOS unit: its own backup, read
        Some(p) => {
            let d = match mfd_net::nos::fetch(p.addr.ip(), mfd_net::nos::WAYPOINTS).await {
                Ok(d) => d,
                Err(e) => return fail(StatusCode::BAD_GATEWAY, e.to_string()),
            };
            match usr::parse(&d) {
                Ok(u) => (p, u.waypoints, u.routes, u.tracks),
                Err(e) => {
                    log::info!("the waypoints of {serial}: {e}");
                    return fail(
                        StatusCode::BAD_GATEWAY,
                        format!("the chartplotter's file: {e}"),
                    );
                }
            }
        }
        None => match r.waypoints_and_routes(&serial).await {
            Ok((p, w, rt)) => (p, w, rt, Vec::new()),
            Err((status, e)) => return fail(status, e),
        },
    };
    let live = |v: &Vec<serde_json::Value>| {
        v.iter()
            .filter(|x| x.get("deleted_at").and_then(|d| d.as_i64()).unwrap_or(0) <= 0)
            .count()
    };
    log::info!(
        "GPX of {} ({serial}): {} waypoints, {} routes, {} tracks",
        p.model,
        live(&waypoints),
        live(&routes),
        tracks.len()
    );
    let name = p.name.clone().unwrap_or_else(|| p.model.clone());
    (
        [
            (header::CONTENT_TYPE, "application/gpx+xml".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}.gpx\"", file_name(&name)),
            ),
        ],
        gpx::gpx(&format!("mfd-bosun ({name})"), &waypoints, &routes, &tracks),
    )
        .into_response()
}

/// A NOS unit's own backups, as its vendor's app makes them: `WaypointsRoutesTracks.usr` or `Settings.set`.
async fn nos_backup(
    State(r): State<Remote>,
    Path((serial, file)): Path<(String, String)>,
) -> Response {
    let path = match file.as_str() {
        "WaypointsRoutesTracks.usr" => mfd_net::nos::WAYPOINTS,
        "Settings.set" => mfd_net::nos::SETTINGS,
        _ => return fail(StatusCode::NOT_FOUND, "no such backup"),
    };
    let Some(p) = r.discovery.get(&serial).filter(|p| p.nos.is_some()) else {
        return fail(StatusCode::NOT_FOUND, format!("no NOS unit {serial}"));
    };
    match mfd_net::nos::fetch(p.addr.ip(), path).await {
        Ok(d) => {
            let name = p.name.clone().unwrap_or_else(|| p.model.clone());
            log::info!("{file} of {} ({serial}): {} bytes", p.model, d.len());
            (
                [
                    (header::CONTENT_TYPE, "application/octet-stream".to_string()),
                    (
                        header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{}-{file}\"", file_name(&name)),
                    ),
                ],
                d,
            )
                .into_response()
        }
        Err(e) => fail(StatusCode::BAD_GATEWAY, e.to_string()),
    }
}

/// A chartplotter's name as part of a file name.
fn file_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[derive(Deserialize)]
struct SignalQuery {
    plotter: String,
    key: u64,
}

async fn signal(
    ws: WebSocketUpgrade,
    Query(q): Query<SignalQuery>,
    State(r): State<Remote>,
) -> Response {
    ws.on_upgrade(move |socket| r.bridge(socket, q.plotter, q.key))
}

async fn asset(uri: Uri) -> Response {
    let path = match uri.path().trim_start_matches('/') {
        "" => "index.html",
        p => p,
    };
    serve_embedded::<Assets>(path)
}

/// A file of an embedded folder, with its content type. Pages are never cached, so an update shows at once.
pub fn serve_embedded<A: RustEmbed>(path: &str) -> Response {
    let Some(f) = A::get(path) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    (
        [
            (header::CONTENT_TYPE, content_type(path)),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        f.data,
    )
        .into_response()
}

pub fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("webmanifest") => "application/manifest+json",
        Some("png") => "image/png",
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        _ => "application/octet-stream",
    }
}
