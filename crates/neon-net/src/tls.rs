//! TLS to a chartplotter: trust-all, ALPN pinned to http/1.1.
//!
//! Every chartplotter serves its own self-signed certificate, so there is nothing to verify it against.
//! The ALPN pin is required: offered h2, the chartplotter's TLS server hangs up the connection.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The one client config for every connection to a chartplotter.
pub fn config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let mut c = ClientConfig::builder_with_provider(provider.clone())
                .with_safe_default_protocol_versions()
                .expect("ring supports the default protocol versions")
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(TrustAll(provider)))
                .with_no_client_auth();
            c.alpn_protocols = vec![b"http/1.1".to_vec()];
            Arc::new(c)
        })
        .clone()
}

/// A TCP + TLS connection to `addr`.
pub async fn connect(addr: SocketAddr) -> std::io::Result<TlsStream<TcpStream>> {
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("connecting to {addr}"),
            )
        })??;
    tcp.set_nodelay(true)?;
    // SNI is not sent for an IP address; the name only matters to our verifier, which ignores it
    let name = ServerName::IpAddress(addr.ip().into());
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        TlsConnector::from(config()).connect(name, tcp),
    )
    .await
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("TLS handshake with {addr}"),
        )
    })?
}

/// Accepts any certificate, but still checks the handshake signatures, so the peer holds the key of the
/// certificate it showed.
#[derive(Debug)]
struct TrustAll(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for TrustAll {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
