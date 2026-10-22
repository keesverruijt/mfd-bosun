//! What the mfd-bosun crates share about talking to the chartplotters: finding NEON ones (mDNS +
//! `/api/v1/info`), the trust-all TLS with ALPN http/1.1 they need, their REST API and signalling socket, and
//! the store of their passwords; the older NOS units' remote-control protocol; and Furuno TimeZero units,
//! found over NavNet and driven through canboat's MCU-006 remote.

pub mod api;
pub mod canboat;
pub mod credentials;
pub mod discovery;
pub mod furuno;
pub mod garmin;
pub mod garmin_userdata;
pub mod http;
pub mod link;
pub mod netif;
pub mod nos;
pub mod raymarine;
pub mod screen;
pub mod tls;
pub mod ws;
