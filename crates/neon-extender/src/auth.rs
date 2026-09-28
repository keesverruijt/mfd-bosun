//! The optional password for this program's own pages (`--password` / `NEON_EXTENDER_PASSWORD`).
//!
//! Without one, everything is open. With one, the chartplotters' remote screens (and the list of them)
//! need a login; ForwardScan stays open, since the chartplotters show it themselves and cannot log in.
//! A login sets a cookie that holds a hash of the password, so the browser sends it with every request
//! and WebSocket by itself, and a new password logs everyone out. "Remember" makes the cookie last
//! [`REMEMBER`]; else it goes when the browser closes.
//!
//!   GET  /api/auth    -> { required, ok }
//!   POST /api/login   -> { password, remember }: sets the cookie, or 401
//!   POST /api/logout  -> clears the cookie

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use std::sync::Arc;

const COOKIE: &str = "neon_extender";
const REMEMBER: u64 = 400 * 24 * 3600; // the most browsers keep a cookie
/// Paths that need a login when there is a password.
const PROTECTED: [&str; 2] = ["/api/status", "/remote/"];

#[derive(Clone)]
pub struct Auth {
    /// The cookie value that proves a login; None: no password.
    token: Option<Arc<String>>,
}

impl Auth {
    pub fn new(password: Option<&str>) -> Self {
        Auth {
            token: password
                .filter(|p| !p.is_empty())
                .map(|p| Arc::new(token(p))),
        }
    }

    pub fn required(&self) -> bool {
        self.token.is_some()
    }

    fn ok(&self, headers: &HeaderMap) -> bool {
        let Some(want) = &self.token else {
            return true;
        };
        cookies(headers).any(|(k, v)| k == COOKIE && same(v, want))
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/api/auth", get(status))
            .route("/api/login", post(login))
            .route("/api/logout", post(logout))
            .with_state(self.clone())
    }
}

fn token(password: &str) -> String {
    let d = ring::digest::digest(
        &ring::digest::SHA256,
        format!("navico-neon-extender\0{password}").as_bytes(),
    );
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Equal, in a time that does not depend on where they differ.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

fn cookies(headers: &HeaderMap) -> impl Iterator<Item = (&str, &str)> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|c| c.trim().split_once('='))
}

/// The layer that turns away requests for protected paths without a login.
pub async fn guard(State(auth): State<Auth>, req: Request, next: Next) -> Response {
    if is_protected(req.uri().path()) && !auth.ok(req.headers()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "ok": false, "error": "login needed" })),
        )
            .into_response();
    }
    next.run(req).await
}

/// Under /remote/ the page itself (html, js, css) may load: it asks for the login.
fn is_protected(path: &str) -> bool {
    PROTECTED.iter().any(|p| match path.strip_prefix(p) {
        Some("") => !p.ends_with('/'),
        Some(rest) => p.ends_with('/') && (rest.starts_with("api/") || rest.starts_with("signal")),
        None => false,
    })
}

async fn status(State(auth): State<Auth>, headers: HeaderMap) -> Json<serde_json::Value> {
    Json(json!({ "required": auth.required(), "ok": auth.ok(&headers) }))
}

#[derive(serde::Deserialize)]
struct Login {
    password: String,
    #[serde(default)]
    remember: bool,
}

async fn login(State(auth): State<Auth>, Json(l): Json<Login>) -> Response {
    let Some(want) = &auth.token else {
        return Json(json!({ "ok": true })).into_response();
    };
    let got = token(&l.password);
    if !same(&got, want) {
        log::info!("login with a wrong password");
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "ok": false, "error": "wrong password" })),
        )
            .into_response();
    }
    let age = if l.remember {
        format!("; Max-Age={REMEMBER}")
    } else {
        String::new()
    };
    let cookie = format!("{COOKIE}={got}; Path=/; HttpOnly; SameSite=Strict{age}");
    ([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response()
}

async fn logout() -> Response {
    let cookie = format!("{COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0");
    ([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn with_cookie(v: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, HeaderValue::from_str(v).unwrap());
        h
    }

    #[test]
    fn checks_the_cookie() {
        let open = Auth::new(None);
        assert!(!open.required() && open.ok(&HeaderMap::new()));
        assert!(!Auth::new(Some("")).required());

        let a = Auth::new(Some("neon"));
        assert!(a.required());
        assert!(!a.ok(&HeaderMap::new()));
        assert!(a.ok(&with_cookie(&format!("x=1; {COOKIE}={}", token("neon")))));
        assert!(!a.ok(&with_cookie(&format!("{COOKIE}={}", token("other")))));
    }

    #[test]
    fn protects_the_remote_api_but_not_its_page() {
        let p = is_protected;
        assert!(p("/api/status") && p("/remote/api/plotters") && p("/remote/signal"));
        assert!(
            !p("/remote/")
                && !p("/remote/remote.js")
                && !p("/forwardscan/api/state")
                && !p("/api/auth")
        );
    }
}
