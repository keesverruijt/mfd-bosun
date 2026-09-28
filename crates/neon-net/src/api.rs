//! A chartplotter's REST API on :8444: the unauthenticated info, and pairing.

use base64::Engine;
use hyper::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::net::SocketAddr;

use crate::http;

pub const PORT: u16 = 8444;

/// `GET /api/v1/info`: no authentication.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct Info {
    pub model: String,
    pub brand: String,
    pub serial_number: String,
    pub os_version: String,
    pub language: String,
    /// The chartplotter's own snap_id: pairing must use it (see [`pair`]).
    pub snap_id: String,
}

pub async fn info(addr: SocketAddr) -> std::io::Result<Info> {
    let r = http::request(addr, Method::GET, "/api/v1/info", &[], None).await?;
    if r.status != StatusCode::OK {
        return Err(std::io::Error::other(format!(
            "/api/v1/info on {addr}: HTTP {}",
            r.status
        )));
    }
    serde_json::from_slice(&r.body).map_err(std::io::Error::other)
}

#[derive(Debug)]
pub enum PairError {
    /// The password (the chartplotter's WiFi password) is wrong.
    WrongPassword,
    Other(String),
}

impl std::fmt::Display for PairError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PairError::WrongPassword => write!(f, "wrong password"),
            PairError::Other(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for PairError {
    fn from(e: std::io::Error) -> Self {
        PairError::Other(e.to_string())
    }
}

/// Who we say we are in the pairing request; any values do.
pub struct Identity<'a> {
    pub email: &'a str,
    pub device_name: &'a str,
}

/// `POST /api/v1/pair`: HTTP Basic with the chartplotter's WiFi password; returns an access token (an RS256
/// JWT) for its signalling socket.
///
/// The `snap_id` sent is the chartplotter's own: it has a stored account, and its signalling socket rejects
/// a token whose `sub` is not that account's snap_id.
pub async fn pair(
    addr: SocketAddr,
    password: &str,
    who: &Identity<'_>,
) -> Result<String, PairError> {
    let info = info(addr).await?;
    let auth = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("viewer:{password}"))
    );
    let body =
        json!({ "snap_id": info.snap_id, "email": who.email, "device_name": who.device_name });
    let r = http::request(
        addr,
        Method::POST,
        "/api/v1/pair",
        &[("Authorization", &auth)],
        Some(&body),
    )
    .await?;
    match r.status {
        StatusCode::OK => {}
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => return Err(PairError::WrongPassword),
        s => {
            return Err(PairError::Other(format!(
                "pairing with {addr}: HTTP {s}: {}",
                r.text()
            )))
        }
    }
    #[derive(Deserialize)]
    struct Token {
        access_token: Option<String>,
    }
    serde_json::from_slice::<Token>(&r.body)
        .ok()
        .and_then(|t| t.access_token)
        .ok_or_else(|| {
            PairError::Other(format!(
                "pairing with {addr}: no access_token in {}",
                r.text()
            ))
        })
}

#[derive(Debug)]
pub enum SyncError {
    /// The token was refused: pair again.
    Unauthorized,
    Other(String),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Unauthorized => write!(f, "token refused"),
            SyncError::Other(e) => write!(f, "{e}"),
        }
    }
}

/// What `POST /api/v1/sync/{kind}` keeps: the chartplotter's own waypoints and routes.
#[derive(Clone, Copy, Debug)]
pub enum SyncKind {
    Waypoints,
    Routes,
}

/// All of the chartplotter's waypoints or routes, as it has them (JSON objects; deleted ones included,
/// with `deleted_at` > 0).
///
/// `POST /api/v1/sync/{kind}?syncSequenceId=0` (Bearer token) takes a list of items to merge in and answers
/// with every local item from that sequence id on. With an empty list it only reads.
pub async fn sync_read(
    addr: SocketAddr,
    token: &str,
    kind: SyncKind,
) -> Result<Vec<serde_json::Value>, SyncError> {
    let path = match kind {
        SyncKind::Waypoints => "/api/v1/sync/waypoints?syncSequenceId=0",
        SyncKind::Routes => "/api/v1/sync/routes?syncSequenceId=0",
    };
    let auth = format!("Bearer {token}");
    let r = http::request(
        addr,
        Method::POST,
        path,
        &[("Authorization", &auth)],
        Some(&json!([])),
    )
    .await
    .map_err(|e| SyncError::Other(e.to_string()))?;
    match r.status {
        StatusCode::OK => serde_json::from_slice(&r.body)
            .map_err(|e| SyncError::Other(format!("{path} on {addr}: {e}"))),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(SyncError::Unauthorized),
        s => Err(SyncError::Other(format!(
            "{path} on {addr}: HTTP {s}: {}",
            r.text()
        ))),
    }
}
