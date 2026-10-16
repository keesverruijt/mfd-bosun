//! A minimal HTTP/1.1 client for a chartplotter: HTTPS for a NEON unit's REST API on :8444
//! ([`request`]), and plain HTTP for a Garmin unit's app API on :80 ([`get`]). One request per connection.

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::time::Duration;

use crate::tls;

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Response {
    pub status: StatusCode,
    pub body: Bytes,
}

impl Response {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// `method path` on `addr`, with `headers` and a JSON `body` when given.
pub async fn request(
    addr: SocketAddr,
    method: Method,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&serde_json::Value>,
) -> std::io::Result<Response> {
    let io = |e: hyper::Error| std::io::Error::other(format!("{method} {path} on {addr}: {e}"));
    let run = async {
        let stream = tls::connect(addr).await?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(io)?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let mut req = Request::builder()
            .method(method.clone())
            .uri(path)
            .header("Host", addr.to_string());
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let body = match body {
            Some(b) => {
                req = req.header("Content-Type", "application/json");
                Bytes::from(b.to_string())
            }
            None => Bytes::new(),
        };
        let req = req.body(Full::new(body)).map_err(std::io::Error::other)?;
        let res = sender.send_request(req).await.map_err(io)?;
        let status = res.status();
        let body = res.into_body().collect().await.map_err(io)?.to_bytes();
        Ok(Response { status, body })
    };
    tokio::time::timeout(REQUEST_TIMEOUT, run)
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("{method} {path} on {addr}"),
            )
        })?
}

/// A plain-HTTP `GET path` on `addr` (no TLS) — for a Garmin unit's app API on port 80.
pub async fn get(addr: SocketAddr, path: &str) -> std::io::Result<Response> {
    let io = |e: hyper::Error| std::io::Error::other(format!("GET {path} on {addr}: {e}"));
    let run = async {
        let stream = tokio::net::TcpStream::connect(addr).await?;
        let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(io)?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let req = Request::builder()
            .method(Method::GET)
            .uri(path)
            .header("Host", addr.to_string())
            .body(Full::new(Bytes::new()))
            .map_err(std::io::Error::other)?;
        let res = sender.send_request(req).await.map_err(io)?;
        let status = res.status();
        let body = res.into_body().collect().await.map_err(io)?.to_bytes();
        Ok(Response { status, body })
    };
    tokio::time::timeout(REQUEST_TIMEOUT, run)
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, format!("GET {path} on {addr}"))
        })?
}
