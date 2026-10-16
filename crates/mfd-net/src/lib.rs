//! What the mfd-bosun crates share about talking to the chartplotters: finding NEON ones (mDNS +
//! `/api/v1/info`), the trust-all TLS with ALPN http/1.1 they need, their REST API and signalling socket, and
//! the store of their passwords; and the older NOS units' remote-control protocol.

pub mod api;
pub mod credentials;
pub mod discovery;
pub mod garmin;
pub mod garmin_userdata;
pub mod http;
pub mod nos;
pub mod screen;
pub mod tls;
pub mod ws;
