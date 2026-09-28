//! What the navico-neon-extender crates share about talking to NEON chartplotters: finding them (mDNS +
//! `/api/v1/info`), the trust-all TLS with ALPN http/1.1 they need, their REST API and signalling socket, and
//! the store of their passwords.

pub mod api;
pub mod credentials;
pub mod discovery;
pub mod http;
pub mod tls;
pub mod ws;
