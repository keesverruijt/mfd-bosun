//! Furuno TimeZero chartplotters (TZMAP, TZtouch): finding them, and what is on their NMEA 2000 bus.
//!
//! A TimeZero MFD answers the NavNet discovery requests on UDP [`PORT`], as Furuno radars do: a beacon
//! request gets a beacon report naming it (`MF…` for an MFD, `RD…` for a radar), and a model request gets
//! a model report for itself (model, software, serial and MAC) and one more for every NMEA 2000 device it
//! sees on its bus. It answers only hosts on its own network, 172.31.0.0/16, so the requests are sent only
//! out of an interface on that network, or to an address given by hand.
//!
//! Its screen cannot be shared over the network. It can be driven, though, by a Furuno MCU-006 remote on
//! its NMEA 2000 bus, and canboat's `mcu006` quirk is one (see [`crate::canboat`]): the relayed device
//! list tells whether that canboat is on this MFD's bus.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// The NavNet discovery port: requests go to it, and replies come back on it.
pub const PORT: u16 = 10010;

/// Every discovery packet, in either direction, opens with these.
const HEADER: [u8; 8] = [0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00];
/// Four bytes between the length and the body; a unit sends these, so we send them back.
const FILLER: [u8; 4] = [0x01, 0x00, 0x00, 0x00];

/// Asks for a beacon report, which names the unit.
const REQUEST_BEACON: u8 = 0x01;
/// Asks for the model reports: the unit's own, and one per NMEA 2000 device it sees.
const REQUEST_MODEL: u8 = 0x14;
/// The report types in a reply.
const REPORT_BEACON: u8 = 0x00;
const REPORT_MODEL: u8 = 0x0f;
const MODEL_REPORT_LENGTH: usize = 170;

/// The network Furuno units live on, and answer only to.
const NAVNET: (Ipv4Addr, u8) = (Ipv4Addr::new(172, 31, 0, 0), 16);

/// The NMEA 2000 device a TimeZero MFD lists for an MCU-006 remote.
pub const MCU006_MODEL: &str = "MCU-006";
/// The NMEA 2000 model canboat gives itself.
pub const CANBOAT_MODEL: &str = "CANboat";

/// A beacon request and a model request.
pub fn requests() -> [[u8; 16]; 2] {
    [REQUEST_BEACON, REQUEST_MODEL].map(|r| {
        let mut p = [0u8; 16];
        p[..8].copy_from_slice(&HEADER);
        p[8] = r;
        p[9] = 0x01;
        p[10..12].copy_from_slice(&8u16.to_be_bytes());
        p[12..].copy_from_slice(&FILLER);
        p
    })
}

/// A device in a model report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub model: String,
    pub firmware: String,
    /// What the device calls itself beyond its model (a product name, a hardware version).
    pub description: String,
    pub serial: String,
}

/// What a discovery reply says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// The unit's name: `MF…` for a TimeZero MFD.
    Beacon(String),
    /// The unit itself, with its MAC.
    Unit { mac: [u8; 6], device: Device },
    /// An NMEA 2000 device on the unit's bus, by the unit's index for it.
    Bus { index: u8, device: Device },
}

/// Reads a discovery reply; `None` for anything else, our own requests included.
pub fn parse(p: &[u8]) -> Option<Report> {
    if p.len() < 16 || p[..8] != HEADER || p[9] != 0x01 || p[12..16] != FILLER {
        return None;
    }
    if usize::from(u16::from_be_bytes([p[10], p[11]])) + 8 != p.len() {
        return None;
    }
    match p[8] {
        REPORT_BEACON if p.len() >= 24 => Some(Report::Beacon(text(&p[16..24]))),
        REPORT_MODEL if p.len() == MODEL_REPORT_LENGTH => {
            let field = |i: usize| text(&p[24 + 32 * i..56 + 32 * i]);
            let device = Device {
                model: field(0),
                firmware: field(1),
                description: field(2),
                serial: field(3),
            };
            let mac: [u8; 6] = p[18..24].try_into().ok()?;
            match (p[16], p[17]) {
                // its own report carries its MAC; the "CAN-BUS" one that heads the list does not
                (0, 0) if mac != [0xff; 6] && !device.model.is_empty() => {
                    Some(Report::Unit { mac, device })
                }
                (1, index) if !device.model.is_empty() => Some(Report::Bus { index, device }),
                _ => None,
            }
        }
        _ => None,
    }
}

/// A NUL- or space-padded string.
fn text(b: &[u8]) -> String {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    String::from_utf8_lossy(&b[..end]).trim().to_string()
}

/// A name in a beacon report that is a TimeZero MFD's.
pub fn is_mfd(beacon: &str) -> bool {
    beacon.starts_with("MF")
}

/// What a discovered TimeZero MFD is.
#[derive(Debug, Clone, PartialEq)]
pub struct Info {
    pub model: String,
    /// The TimeZero version, e.g. "26.11" (the unit repeats it, colon-separated, per component).
    pub firmware: String,
    pub serial: String,
    pub mac: [u8; 6],
    /// What its NMEA 2000 bus has on it, as it reports.
    pub bus: Vec<Device>,
    /// The control port of the canboat whose `mcu006` quirk is on this bus, when there is one.
    pub remote: Option<SocketAddr>,
}

impl Info {
    pub fn brand(&self) -> &'static str {
        "furuno"
    }

    /// Its id in the list: its MAC, the one thing two units never share.
    pub fn id(&self) -> String {
        let hex: String = self.mac.iter().map(|b| format!("{b:02x}")).collect();
        format!("furuno-{hex}")
    }

    /// Whether an MCU-006 remote is on its bus (canboat's, or a real one).
    pub fn has_mcu006(&self) -> bool {
        self.bus.iter().any(|d| d.model == MCU006_MODEL)
    }

    /// Whether a canboat with this serial number is on its bus.
    pub fn has_canboat(&self, serial: &str) -> bool {
        self.bus
            .iter()
            .any(|d| d.model == CANBOAT_MODEL && d.serial == serial)
    }
}

/// The first part of a unit's colon-separated version list.
pub fn version(firmware: &str) -> String {
    firmware.split(':').next().unwrap_or_default().to_string()
}

/// Where to send the discovery requests: the broadcast address of every interface on 172.31.0.0/16. None
/// when this host is not on that network: then nothing is sent, as the units would not answer anyway.
pub fn broadcasts() -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = crate::netif::list()
        .iter()
        .flat_map(|i| i.addrs.clone())
        .filter(|(a, _)| on_navnet(*a))
        .map(|(a, m)| Ipv4Addr::from(u32::from(a) | !u32::from(m)))
        .collect();
    out.sort();
    out.dedup();
    out
}

fn on_navnet(a: Ipv4Addr) -> bool {
    let mask = u32::MAX << (32 - NAVNET.1);
    u32::from(a) & mask == u32::from(NAVNET.0)
}

/// The socket the requests go out of: a port of its own, as mayara sends its own requests, so that a reply
/// to the sender's port reaches only us.
pub fn sender() -> std::io::Result<tokio::net::UdpSocket> {
    let s = std::net::UdpSocket::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0))?;
    s.set_broadcast(true)?;
    s.set_nonblocking(true)?;
    tokio::net::UdpSocket::from_std(s)
}

/// The socket replies on [`PORT`] arrive at, shared with other programs that listen there (mayara, for
/// Furuno radars). The units broadcast their replies: mayara, on Linux, binds the broadcast address and
/// hears nothing else. A broadcast reaches every socket on the port, so sharing it costs neither program a
/// reply. Were a unit to answer one of them directly instead, Linux would hand that to one socket only.
pub fn listener() -> std::io::Result<tokio::net::UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(unix)]
    s.set_reuse_port(true)?;
    s.set_broadcast(true)?;
    s.bind(&SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), PORT).into())?;
    s.set_nonblocking(true)?;
    tokio::net::UdpSocket::from_std(s.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn requests_are_mayaras() {
        let [beacon, model] = requests();
        assert_eq!(beacon.to_vec(), hex("01000001000000000101000801000000"));
        assert_eq!(model[8], 0x14);
        // our own requests, heard back as broadcast, are not replies
        assert_eq!(parse(&beacon), None);
        assert_eq!(parse(&model), None);
    }

    #[test]
    fn reads_a_tzmap9_beacon() {
        let p = hex("01000001000000000001001e010000004d463235323136370101000500170015001100140016");
        assert_eq!(parse(&p), Some(Report::Beacon("MF252167".into())));
        assert!(is_mfd("MF252167") && !is_mfd("RD252168"));
    }

    #[test]
    fn reads_a_tzmap9_model_report() {
        let p = hex("01000001000000000f0100a201000000000000d01dbb8159545a4d415039000000000000000000000000000000000000000000000000000032362e31313a32362e31313a32362e31313a000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000003130303231332d31303036313500000000000000000000000000000000000000190400080800000000000000000000000000");
        let Some(Report::Unit { mac, device }) = parse(&p) else {
            panic!("not a unit report")
        };
        assert_eq!(mac, [0x00, 0xd0, 0x1d, 0xbb, 0x81, 0x59]);
        assert_eq!(device.model, "TZMAP9");
        assert_eq!(version(&device.firmware), "26.11");
        assert_eq!(device.serial, "100213-100615");
    }

    #[test]
    fn reads_the_bus_it_relays() {
        let canboat = hex("01000001000000000f0100a2010000000106ffffffffffff43414e626f617400000000000000000000000000000000000000000000000000392e302e3000000000000000000000000000000000000000000000000000000063616e626f61742d706970656c696e652d7273000000000000000000000000003635323633000000000000000000000000000000000000000000000000000000ffffff81ffffffffffffffffffffffff0000");
        let Some(Report::Bus { index: 6, device }) = parse(&canboat) else {
            panic!("not a bus report")
        };
        assert_eq!(
            (device.model.as_str(), device.serial.as_str()),
            (CANBOAT_MODEL, "65263")
        );
        // space-padded fields come out trimmed
        let yden = hex("01000001000000000f0100a2010000000102ffffffffffff5944454e2d303320202020202020202020202020202020202020202020202020302e39392031392f30352f32303235202020202020202020202020202020202045746865726e65742047617465776179202020202020202020202020202020203030353332353939202020202020202020202020202020202020202020202020ffffff81ffffffffffffffffffffffff0000");
        let Some(Report::Bus { device, .. }) = parse(&yden) else {
            panic!("not a bus report")
        };
        assert_eq!(device.model, "YDEN-03");
        assert_eq!(device.description, "Ethernet Gateway");
        // the "CAN-BUS" report heading the list is neither the unit nor a device
        let mut can_bus = canboat.clone();
        can_bus[16..18].copy_from_slice(&[0, 0]);
        can_bus[24..56].fill(0);
        can_bus[24..31].copy_from_slice(b"CAN-BUS");
        assert_eq!(can_bus.len(), 170);
        assert_eq!(parse(&can_bus), None);
    }

    #[test]
    fn knows_its_bus() {
        let dev = |model: &str, serial: &str| Device {
            model: model.into(),
            firmware: String::new(),
            description: String::new(),
            serial: serial.into(),
        };
        let info = Info {
            model: "TZMAP9".into(),
            firmware: "26.11".into(),
            serial: "100213-100615".into(),
            mac: [0x00, 0xd0, 0x1d, 0xbb, 0x81, 0x59],
            bus: vec![dev("CANboat", "65263"), dev("MCU-006", "0000")],
            remote: None,
        };
        assert_eq!(info.id(), "furuno-00d01dbb8159");
        assert!(info.has_mcu006());
        assert!(info.has_canboat("65263") && !info.has_canboat("1"));
    }

    #[test]
    fn only_navnet_addresses_count() {
        assert!(on_navnet(Ipv4Addr::new(172, 31, 3, 150)));
        assert!(!on_navnet(Ipv4Addr::new(172, 30, 3, 150)));
        assert!(!on_navnet(Ipv4Addr::new(10, 56, 0, 1)));
    }
}
