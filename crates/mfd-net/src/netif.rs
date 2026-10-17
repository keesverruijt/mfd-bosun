//! The host's network interfaces, and its 169.254 (link-local) address on the sonar's network.
//!
//! Receiving ForwardScan needs no address at all (it is broadcast to 255.255.255.255), but a
//! settings change only works when sent to the sonar's 169.254 address, and that needs a route:
//! an address in 169.254.0.0/16 on the interface the sonar is on. When that is missing this
//! adds one (if it may: root/Administrator, or CAP_NET_ADMIN on Linux) and removes it on exit,
//! or else explains how to add one.

use network_interface::{Addr, NetworkInterface, NetworkInterfaceConfig};
use std::net::Ipv4Addr;

#[derive(Debug, Clone, PartialEq)]
pub struct Iface {
    pub name: String,
    pub index: u32,
    pub mac: Option<[u8; 6]>,
    pub addrs: Vec<(Ipv4Addr, Ipv4Addr)>, // (address, netmask)
    /// A LAN segment, as opposed to a VPN tunnel, PPP or Bluetooth link (see `lan_link`).
    pub lan: bool,
}

impl Iface {
    pub fn link_local(&self) -> Option<Ipv4Addr> {
        self.addrs.iter().map(|a| a.0).find(|a| a.is_link_local())
    }
    /// The address to show and advertise: a routable one if it has one, else its 169.254 one.
    pub fn preferred(&self) -> Option<Ipv4Addr> {
        let mut v: Vec<_> = self.addrs.iter().map(|a| a.0).collect();
        v.sort_by_key(|a| a.is_link_local());
        v.first().copied()
    }
    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        self.addrs.iter().any(|(a, m)| {
            let m = u32::from(*m);
            m != 0 && u32::from(*a) & m == u32::from(ip) & m
        })
    }
    /// Interfaces a Navico network is never on: tunnels and VPNs (by their link type), and the
    /// bridges of containers and VMs (by name, they look like any LAN).
    pub fn is_virtual(&self) -> bool {
        const PREFIXES: [&str; 7] = [
            "docker", "veth", "br-", "virbr", "vmnet", "vboxnet", "bridge",
        ];
        let n = self.name.to_ascii_lowercase();
        !self.lan || PREFIXES.iter().any(|p| n.starts_with(p)) || n.contains("vethernet")
    }
}

pub fn list() -> Vec<Iface> {
    let mut out: Vec<Iface> = Vec::new();
    for ni in NetworkInterface::show().unwrap_or_default() {
        let addrs: Vec<_> = ni
            .addr
            .iter()
            .filter_map(|a| match a {
                Addr::V4(v4) if !v4.ip.is_loopback() => {
                    Some((v4.ip, v4.netmask.unwrap_or(Ipv4Addr::new(255, 255, 255, 0))))
                }
                _ => None,
            })
            .collect();
        if addrs.is_empty() {
            continue;
        }
        let mac = ni.mac_addr.as_deref().and_then(parse_mac);
        // some platforms list an interface once per address
        match out.iter_mut().find(|i| i.name == ni.name) {
            Some(i) => i.addrs.extend(addrs),
            None => {
                let lan = lan_link(&ni.name).unwrap_or(true);
                out.push(Iface {
                    name: ni.name,
                    index: ni.index,
                    mac,
                    addrs,
                    lan,
                })
            }
        }
    }
    out
}

/// Whether an interface is a LAN segment, from its link type (as mayara-server does). A
/// point-to-point link -- a tun VPN such as WireGuard, Tailscale, tinc or OpenVPN, PPP, an IP
/// tunnel -- has no broadcast address and no chartplotter at the other end. A bridged tap VPN
/// keeps its broadcast flag and stays usable, which is right: it does carry the LAN.
/// `None` when the interface has just gone away.
#[cfg(unix)]
fn lan_link(name: &str) -> Option<bool> {
    let mut flags = None;
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return None;
        }
        let mut p = ifap;
        while !p.is_null() {
            if std::ffi::CStr::from_ptr((*p).ifa_name).to_bytes() == name.as_bytes() {
                flags = Some((*p).ifa_flags as libc::c_int);
                break;
            }
            p = (*p).ifa_next;
        }
        libc::freeifaddrs(ifap);
    }
    flags.map(|f| f & libc::IFF_POINTOPOINT == 0 && f & libc::IFF_BROADCAST != 0)
}

/// Windows says what an adapter is outright. A Bluetooth PAN claims to be Ethernet, and some VPN
/// adapters do too, so the medium and access type are checked first.
#[cfg(windows)]
fn lan_link(name: &str) -> Option<bool> {
    use windows_sys::Win32::NetworkManagement::IpHelper::{
        FreeMibTable, GetIfTable2, MIB_IF_TABLE2,
    };
    const IF_TYPE_ETHERNET_CSMACD: u32 = 6;
    const IF_TYPE_IEEE80211: u32 = 71;
    const NET_IF_ACCESS_POINT_TO_POINT: i32 = 2;
    const NDIS_PHYSICAL_MEDIUM_BLUETOOTH: i32 = 10;
    unsafe {
        let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
        if GetIfTable2(&mut table) != 0 || table.is_null() {
            return None;
        }
        // `Table` is a C flexible array: its one-element head is followed by the other rows
        let rows =
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let lan = rows
            .iter()
            .find(|r| {
                let end = r
                    .Alias
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(r.Alias.len());
                String::from_utf16_lossy(&r.Alias[..end]) == name
            })
            .map(|r| {
                r.PhysicalMediumType != NDIS_PHYSICAL_MEDIUM_BLUETOOTH
                    && r.AccessType != NET_IF_ACCESS_POINT_TO_POINT
                    && (r.Type == IF_TYPE_ETHERNET_CSMACD || r.Type == IF_TYPE_IEEE80211)
            });
        FreeMibTable(table as _);
        lan
    }
}

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let v: Vec<u8> = s
        .split([':', '-'])
        .filter_map(|h| u8::from_str_radix(h, 16).ok())
        .collect();
    let m: [u8; 6] = v.try_into().ok()?;
    (m != [0; 6]).then_some(m)
}

/// Which interface the sonar is on. `sonar_ip` is its routable (DHCP) address from its
/// announcement, if it has one; its 169.254 address says nothing, every interface could reach it.
pub fn choose<'a>(
    ifaces: &'a [Iface],
    forced: Option<&str>,
    sonar_ip: Option<Ipv4Addr>,
) -> Result<&'a Iface, String> {
    if let Some(name) = forced {
        return ifaces.iter().find(|i| i.name == name).ok_or_else(|| {
            format!(
                "there is no interface \"{name}\" with an IPv4 address (have: {})",
                names(ifaces)
            )
        });
    }
    if let Some(ip) = sonar_ip.filter(|ip| !ip.is_link_local()) {
        if let Some(i) = ifaces.iter().find(|i| i.contains(ip)) {
            return Ok(i);
        }
    }
    let real: Vec<_> = ifaces.iter().filter(|i| !i.is_virtual()).collect();
    let with_ll: Vec<_> = real.iter().filter(|i| i.link_local().is_some()).collect();
    if with_ll.len() == 1 {
        return Ok(with_ll[0]);
    }
    if real.len() == 1 {
        return Ok(real[0]);
    }
    Err(format!(
        "cannot tell which interface the sonar is on (have: {}); start with --interface NAME",
        names(ifaces)
    ))
}

fn names(ifaces: &[Iface]) -> String {
    ifaces
        .iter()
        .map(|i| i.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A 169.254 address for this interface: derived from its MAC, so the same every time, and away
/// from 169.254.0.x and 169.254.255.x which RFC 3927 reserves.
pub fn pick_link_local(iface: &Iface, avoid: &[Ipv4Addr]) -> Ipv4Addr {
    let seed = iface
        .mac
        .unwrap_or([0x4e, 0x46, 0x53, 0, 0, iface.index as u8]);
    let mut h: u32 = 2166136261;
    for n in 0u32.. {
        for b in seed.iter().chain(n.to_le_bytes().iter()) {
            h = (h ^ *b as u32).wrapping_mul(16777619);
        }
        let a = Ipv4Addr::new(169, 254, 1 + (h % 254) as u8, 1 + ((h >> 8) % 254) as u8);
        if !avoid.contains(&a) {
            return a;
        }
    }
    unreachable!()
}

pub enum AddError {
    /// Not allowed: run with more privilege. The text says how, and how to add it by hand.
    Privilege(String),
    Other(String),
}

pub fn add_link_local(iface: &Iface, addr: Ipv4Addr) -> Result<(), AddError> {
    imp::add(iface, addr)
}

pub fn remove_link_local(iface: &Iface, addr: Ipv4Addr) -> Result<(), String> {
    imp::remove(iface, addr)
}

/// How to add the address by hand on this OS, for when we may not.
pub fn manual_instructions(iface: &Iface, addr: Ipv4Addr) -> String {
    imp::manual(iface, addr)
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{AddError, Iface};
    use std::net::Ipv4Addr;

    // rtnetlink RTM_NEWADDR / RTM_DELADDR, so that `setcap cap_net_admin+ep` on the binary is enough
    // and no `ip` command is needed.
    fn netlink(msg_type: u16, flags: u16, iface: &Iface, addr: Ipv4Addr) -> std::io::Result<()> {
        const IFA_ADDRESS: u16 = 1;
        const IFA_LOCAL: u16 = 2;
        const IFA_BROADCAST: u16 = 4;
        let mut body = vec![libc::AF_INET as u8, 16, 0, libc::RT_SCOPE_LINK];
        body.extend_from_slice(&iface.index.to_ne_bytes());
        for (t, a) in [
            (IFA_LOCAL, addr),
            (IFA_ADDRESS, addr),
            (IFA_BROADCAST, Ipv4Addr::new(169, 254, 255, 255)),
        ] {
            body.extend_from_slice(&8u16.to_ne_bytes());
            body.extend_from_slice(&t.to_ne_bytes());
            body.extend_from_slice(&a.octets());
        }
        let mut msg = Vec::with_capacity(16 + body.len());
        msg.extend_from_slice(&((16 + body.len()) as u32).to_ne_bytes());
        msg.extend_from_slice(&msg_type.to_ne_bytes());
        msg.extend_from_slice(
            &(flags | (libc::NLM_F_REQUEST | libc::NLM_F_ACK) as u16).to_ne_bytes(),
        );
        msg.extend_from_slice(&1u32.to_ne_bytes()); // sequence
        msg.extend_from_slice(&0u32.to_ne_bytes()); // port id: the kernel fills it in
        msg.extend_from_slice(&body);
        unsafe {
            let fd = libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                libc::NETLINK_ROUTE,
            );
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let mut sa: libc::sockaddr_nl = std::mem::zeroed();
            sa.nl_family = libc::AF_NETLINK as u16;
            let r = libc::sendto(
                fd,
                msg.as_ptr().cast(),
                msg.len(),
                0,
                (&sa as *const libc::sockaddr_nl).cast(),
                std::mem::size_of::<libc::sockaddr_nl>() as u32,
            );
            if r < 0 {
                let e = std::io::Error::last_os_error();
                libc::close(fd);
                return Err(e);
            }
            let mut buf = [0u8; 1024];
            let n = libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0);
            let e = std::io::Error::last_os_error();
            libc::close(fd);
            if n < 0 {
                return Err(e);
            }
            // the ack: nlmsghdr (type NLMSG_ERROR) then i32 error, 0 = done
            if n >= 20 && u16::from_ne_bytes([buf[4], buf[5]]) == libc::NLMSG_ERROR as u16 {
                let err = i32::from_ne_bytes(buf[16..20].try_into().unwrap());
                if err != 0 {
                    return Err(std::io::Error::from_raw_os_error(-err));
                }
            }
        }
        Ok(())
    }

    pub fn add(iface: &Iface, addr: Ipv4Addr) -> Result<(), AddError> {
        let flags = (libc::NLM_F_CREATE | libc::NLM_F_EXCL) as u16;
        match netlink(libc::RTM_NEWADDR, flags, iface, addr) {
            Ok(()) => Ok(()),
            Err(e)
                if e.raw_os_error() == Some(libc::EPERM)
                    || e.raw_os_error() == Some(libc::EACCES) =>
            {
                Err(AddError::Privilege(manual(iface, addr)))
            }
            Err(e) => Err(AddError::Other(format!(
                "adding {addr}/16 to {}: {e}",
                iface.name
            ))),
        }
    }

    pub fn remove(iface: &Iface, addr: Ipv4Addr) -> Result<(), String> {
        netlink(libc::RTM_DELADDR, 0, iface, addr).map_err(|e| e.to_string())
    }

    pub fn manual(iface: &Iface, addr: Ipv4Addr) -> String {
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "neon-forwardscan".into());
        let mut s = format!(
            "Run it as root, or allow it once with `sudo setcap cap_net_admin+ep {exe}`. \
             Or add the address yourself: `sudo ip addr add {addr}/16 dev {}` (until the next boot)",
            iface.name
        );
        let conn = std::process::Command::new("nmcli")
            .args(["-g", "GENERAL.CONNECTION", "device", "show", &iface.name])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|c| !c.is_empty());
        if let Some(c) = conn {
            s += &format!(
                ", or for good with NetworkManager: `sudo nmcli connection modify \"{c}\" +ipv4.addresses {addr}/16` \
                 then `sudo nmcli connection up \"{c}\"`"
            );
        }
        s + "."
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{AddError, Iface};
    use std::net::Ipv4Addr;
    use std::process::Command;

    pub fn add(iface: &Iface, addr: Ipv4Addr) -> Result<(), AddError> {
        if unsafe { libc::geteuid() } != 0 {
            return Err(AddError::Privilege(manual(iface, addr)));
        }
        let a = addr.to_string();
        let out = Command::new("/sbin/ifconfig")
            .args([&iface.name, "alias", &a, "netmask", "255.255.0.0"])
            .output()
            .map_err(|e| AddError::Other(e.to_string()))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(AddError::Other(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ))
        }
    }

    pub fn remove(iface: &Iface, addr: Ipv4Addr) -> Result<(), String> {
        let a = addr.to_string();
        let out = Command::new("/sbin/ifconfig")
            .args([&iface.name, "-alias", &a])
            .output()
            .map_err(|e| e.to_string())?;
        out.status
            .success()
            .then_some(())
            .ok_or_else(|| String::from_utf8_lossy(&out.stderr).trim().to_string())
    }

    pub fn manual(iface: &Iface, addr: Ipv4Addr) -> String {
        format!(
            "Run it with sudo, or add the address yourself: `sudo ifconfig {} alias {addr} netmask 255.255.0.0` \
             (until the next reboot).",
            iface.name
        )
    }
}

#[cfg(windows)]
mod imp {
    use super::{AddError, Iface};
    use std::net::Ipv4Addr;
    use std::process::Command;

    fn netsh(args: &[&str]) -> Result<(), String> {
        let out = Command::new("netsh")
            .args(args)
            .output()
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() {
            Ok(())
        } else {
            Err(if text.is_empty() {
                String::from_utf8_lossy(&out.stderr).trim().to_string()
            } else {
                text
            })
        }
    }

    pub fn add(iface: &Iface, addr: Ipv4Addr) -> Result<(), AddError> {
        let (name, a) = (format!("name={}", iface.index), format!("address={addr}"));
        // store=active: gone after a reboot, like on the other platforms
        match netsh(&[
            "interface",
            "ipv4",
            "add",
            "address",
            &name,
            &a,
            "mask=255.255.0.0",
            "store=active",
        ]) {
            Ok(()) => Ok(()),
            Err(e)
                if e.to_ascii_lowercase().contains("elevation") || e.contains("Administrator") =>
            {
                Err(AddError::Privilege(manual(iface, addr)))
            }
            Err(e) => Err(AddError::Other(e)),
        }
    }

    pub fn remove(iface: &Iface, addr: Ipv4Addr) -> Result<(), String> {
        let (name, a) = (format!("name={}", iface.index), format!("address={addr}"));
        netsh(&["interface", "ipv4", "delete", "address", &name, &a])
    }

    pub fn manual(iface: &Iface, addr: Ipv4Addr) -> String {
        format!(
            "Run it as Administrator, or add the address yourself in an Administrator prompt: \
             `netsh interface ipv4 add address \"{}\" {addr} 255.255.0.0`.",
            iface.name
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(name: &str, addrs: &[(&str, &str)]) -> Iface {
        Iface {
            name: name.into(),
            index: 2,
            mac: Some([0xb8, 0x27, 0xeb, 1, 2, 3]),
            addrs: addrs
                .iter()
                .map(|(a, m)| (a.parse().unwrap(), m.parse().unwrap()))
                .collect(),
            lan: true,
        }
    }

    #[test]
    fn chooses_the_sonars_interface() {
        let ifs = vec![
            iface("wlan0", &[("192.168.1.20", "255.255.255.0")]),
            iface("eth0", &[("10.56.0.1", "255.255.255.0")]),
            Iface {
                lan: false,
                ..iface("tailscale0", &[("100.64.1.2", "255.192.0.0")])
            },
        ];
        assert_eq!(
            choose(&ifs, None, Some("10.56.0.139".parse().unwrap()))
                .unwrap()
                .name,
            "eth0"
        );
        assert!(choose(&ifs, None, None).is_err()); // two real interfaces, no clue
        assert_eq!(choose(&ifs, Some("wlan0"), None).unwrap().name, "wlan0");
        assert!(choose(&ifs, Some("nope"), None).is_err());

        let ifs = vec![
            iface("wlan0", &[("192.168.1.20", "255.255.255.0")]),
            iface(
                "eth0",
                &[
                    ("10.56.0.1", "255.255.255.0"),
                    ("169.254.44.101", "255.255.0.0"),
                ],
            ),
        ];
        assert_eq!(
            choose(&ifs, None, Some("169.254.6.133".parse().unwrap()))
                .unwrap()
                .name,
            "eth0"
        );
        assert_eq!(ifs[1].preferred(), Some("10.56.0.1".parse().unwrap()));

        let ifs = vec![
            iface("en0", &[("192.168.1.20", "255.255.255.0")]),
            Iface {
                lan: false,
                ..iface("moinnet", &[("10.8.0.2", "255.255.255.0")])
            },
        ];
        assert_eq!(choose(&ifs, None, None).unwrap().name, "en0");
    }

    #[test]
    fn link_local_is_stable_and_valid() {
        let i = iface("eth0", &[]);
        let a = pick_link_local(&i, &[]);
        assert_eq!(a, pick_link_local(&i, &[]));
        let o = a.octets();
        assert!(a.is_link_local() && (1..=254).contains(&o[2]) && (1..=254).contains(&o[3]));
        assert_ne!(pick_link_local(&i, &[a]), a);
    }

    #[test]
    fn mac() {
        assert_eq!(
            parse_mac("00:0E:91:0C:CC:0C"),
            Some([0, 0x0e, 0x91, 0x0c, 0xcc, 0x0c])
        );
        assert_eq!(
            parse_mac("00-0E-91-0C-CC-0C"),
            Some([0, 0x0e, 0x91, 0x0c, 0xcc, 0x0c])
        );
        assert_eq!(parse_mac("00:00:00:00:00:00"), None);
    }
}
