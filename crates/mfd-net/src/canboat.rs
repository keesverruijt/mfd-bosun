//! canboat servers on the network, and the MCU-006 remote that one plays on its NMEA 2000 bus.
//!
//! `canboat server` advertises itself over mDNS as `_canboat._tcp`, with a TXT key per open port and the
//! quirks it runs. With `--quirk mcu006` it is a Furuno MCU-006 remote on its bus, which a TimeZero MFD pairs
//! with, and its `mcu006` port takes the remote's keys, pointer and knob as one JSON command per line,
//! answered by one JSON line:
//!
//! ```text
//! {"key":"F5","down":true}  {"tap":"Up"}  {"pointer":{"dx":40,"dy":-10}}  {"knob":-2}
//! {"click":true}  {"keypad":true}  {"release":true}  {"status":true}
//! ```
//!
//! The keys are the keyboard keys the remote sends (`1`–`4`, `F4`, `F5`, `F8`, `F12`, `y`); the MFD decides
//! what each does, and a key held long does something else. Whatever a connection holds is let go when it
//! closes.
//!
//! The advert's `serial` is the serial number canboat gives itself on the bus, which a TimeZero MFD relays
//! in its device list (see [`crate::furuno`]): that ties a canboat to the MFDs on its bus.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};

pub const SERVICE_TYPE: &str = "_canboat._tcp.local.";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);
/// canboat answers at once, but a command that holds a key or moves far waits for the bus.
const REPLY_TIMEOUT: Duration = Duration::from_secs(4);
/// The longest reply line we read; canboat's are short.
const MAX_LINE: usize = 4096;

/// A canboat server, from its advert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    /// Its addresses, routable ones first.
    pub addrs: Vec<Ipv4Addr>,
    /// The serial number it gives itself on the bus.
    pub serial: String,
    /// Its MCU-006 control port, when it runs the `mcu006` quirk.
    pub mcu006: Option<u16>,
}

impl Server {
    /// Builds it from its advert. `txt` looks up a TXT value by key.
    pub fn from_txt(txt: impl Fn(&str) -> Option<String>, mut addrs: Vec<Ipv4Addr>) -> Server {
        addrs.sort_by_key(|a| (a.is_link_local(), *a));
        let runs_mcu006 = txt("quirks").is_some_and(|q| q.split(',').any(|q| q.trim() == "mcu006"));
        Server {
            addrs,
            serial: txt("serial").unwrap_or_default(),
            mcu006: txt("mcu006")
                .and_then(|p| p.parse().ok())
                .filter(|p| runs_mcu006 && *p != 0),
        }
    }

    /// Where its MCU-006 control port answers.
    pub fn mcu006_addr(&self) -> Option<SocketAddr> {
        Some(SocketAddr::new((*self.addrs.first()?).into(), self.mcu006?))
    }
}

/// A connection to an MCU-006 control port.
pub struct Remote {
    r: BufReader<OwnedReadHalf>,
    w: OwnedWriteHalf,
}

impl Remote {
    pub async fn connect(addr: SocketAddr) -> std::io::Result<Remote> {
        let s = tokio::time::timeout(CONNECT_TIMEOUT, crate::link::connect(addr))
            .await
            .map_err(|_| std::io::Error::other("canboat does not answer"))??;
        s.set_nodelay(true)?;
        let (r, w) = s.into_split();
        Ok(Remote {
            r: BufReader::new(r),
            w,
        })
    }

    /// Sends one command and returns canboat's answer.
    pub async fn command(&mut self, cmd: &serde_json::Value) -> std::io::Result<serde_json::Value> {
        let mut line = serde_json::to_string(cmd)?;
        line.push('\n');
        self.w.write_all(line.as_bytes()).await?;
        let mut reply = Vec::new();
        let read = async {
            let mut r = (&mut self.r).take(MAX_LINE as u64);
            r.read_until(b'\n', &mut reply).await
        };
        match tokio::time::timeout(REPLY_TIMEOUT, read).await {
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "canboat does not answer",
            )),
            Ok(Ok(0)) => Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "canboat closed the connection",
            )),
            Ok(Ok(_)) => serde_json::from_slice(&reply).map_err(std::io::Error::other),
            Ok(Err(e)) => Err(e),
        }
    }
}

/// A command from the page, checked: one of the control port's commands, as an object with one key. It is
/// written out again by us, so it is always one line.
pub fn checked(cmd: &serde_json::Value) -> Option<&serde_json::Value> {
    const COMMANDS: [&str; 8] = [
        "key", "tap", "pointer", "knob", "click", "keypad", "release", "status",
    ];
    let o = cmd.as_object()?;
    let first = o.keys().find(|k| *k != "down")?;
    (COMMANDS.contains(&first.as_str()) && o.len() <= 2).then_some(cmd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reads_the_advert() {
        let txt = |k: &str| {
            match k {
                "serial" => Some("65263"),
                "mcu006" => Some("2607"),
                "quirks" => Some("wmm,mcu006"),
                _ => None,
            }
            .map(str::to_string)
        };
        let s = Server::from_txt(
            txt,
            vec![
                Ipv4Addr::new(169, 254, 1, 2),
                Ipv4Addr::new(172, 31, 3, 150),
            ],
        );
        assert_eq!(s.serial, "65263");
        assert_eq!(s.mcu006_addr(), Some("172.31.3.150:2607".parse().unwrap()));
    }

    #[test]
    fn no_remote_without_the_quirk() {
        let txt = |k: &str| (k == "mcu006").then(|| "2607".to_string());
        let s = Server::from_txt(txt, vec![Ipv4Addr::new(172, 31, 3, 150)]);
        assert_eq!(s.mcu006_addr(), None);
    }

    #[test]
    fn passes_only_control_commands() {
        for ok in [
            json!({"key": "3", "down": true}),
            json!({"tap": "F5"}),
            json!({"pointer": {"dx": 4, "dy": -2}}),
            json!({"knob": -1}),
            json!({"keypad": true}),
            json!({"status": true}),
        ] {
            assert!(checked(&ok).is_some(), "{ok}");
        }
        for bad in [
            json!("tap"),
            json!({}),
            json!({"down": true}),
            json!({"reboot": true}),
            json!({"tap": "1", "knob": 1, "down": true}),
        ] {
            assert!(checked(&bad).is_none(), "{bad}");
        }
    }
}
