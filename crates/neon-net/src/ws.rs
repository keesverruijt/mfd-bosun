//! The chartplotter's signalling socket, `wss://<plotter>:8444/ws/receiver/{key}`, which wants
//! `Authorization: Bearer <token>`: a browser can do neither that header nor trust-all TLS.

use std::net::SocketAddr;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::WebSocketStream;

use crate::tls;

pub type Receiver = WebSocketStream<TlsStream<TcpStream>>;

#[derive(Debug)]
pub enum ConnectError {
    /// The token was refused: expired, or from another pairing. Pair again.
    Unauthorized,
    Other(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::Unauthorized => write!(f, "token refused"),
            ConnectError::Other(e) => write!(f, "{e}"),
        }
    }
}

pub async fn connect_receiver(
    addr: SocketAddr,
    key: u64,
    token: &str,
) -> Result<Receiver, ConnectError> {
    let url = format!("wss://{addr}/ws/receiver/{key}");
    let mut req = url
        .as_str()
        .into_client_request()
        .map_err(|e| ConnectError::Other(e.to_string()))?;
    req.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| ConnectError::Other(e.to_string()))?,
    );
    let stream = tls::connect(addr)
        .await
        .map_err(|e| ConnectError::Other(e.to_string()))?;
    match tokio::time::timeout(
        tls::CONNECT_TIMEOUT,
        tokio_tungstenite::client_async(req, stream),
    )
    .await
    {
        Err(_) => Err(ConnectError::Other(format!("{url}: no handshake response"))),
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(WsError::Http(r))) if r.status() == 401 || r.status() == 403 => {
            Err(ConnectError::Unauthorized)
        }
        // a refused token can also show as the connection closed before the handshake completes
        Ok(Err(WsError::ConnectionClosed | WsError::AlreadyClosed | WsError::Protocol(_))) => {
            Err(ConnectError::Unauthorized)
        }
        Ok(Err(e)) => Err(ConnectError::Other(format!("{url}: {e}"))),
    }
}
