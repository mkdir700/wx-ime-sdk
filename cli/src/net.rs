//! Local address discovery for LAN file transfer.
//!
//! The routing-table answer ("which interface reaches 8.8.8.8") is wrong whenever a
//! proxy/VPN TUN device owns the default route (e.g. Clash/Surge fake-IP `198.18.0.0/15`),
//! so real LAN interfaces are enumerated and filtered instead.
use std::net::{IpAddr, Ipv4Addr, UdpSocket};

/// Interface name prefixes that never carry the physical LAN.
const VIRTUAL_PREFIXES: &[&str] = &[
    "utun",
    "tun",
    "tap",
    "ppp",
    "wg",
    "tailscale",
    "docker",
    "veth",
    "br-",
    "virbr",
    "vmnet",
    "vboxnet",
    "awdl",
    "llw",
    "lo",
    "anpi",
    "ap",
    "gif",
    "stf",
];

fn virtual_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    VIRTUAL_PREFIXES.iter().any(|p| lower.starts_with(p))
}

/// Ranges that are not a reachable home/office LAN.
fn usable_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_link_local()
        || ip.is_broadcast()
        || (a == 198 && (b == 18 || b == 19)) // benchmarking range, used for fake-IP proxies
        || (a == 100 && (64..128).contains(&b))) // CGNAT, e.g. Tailscale
}

/// Pure selection: usable IPv4 addresses of non-virtual interfaces, the one the routing
/// table prefers first (when it survived filtering), private ranges before public ones.
pub fn select_lan(candidates: &[(String, IpAddr)], routed: Option<IpAddr>) -> Vec<IpAddr> {
    let mut ips: Vec<IpAddr> = candidates
        .iter()
        .filter(|(name, _)| !virtual_name(name))
        .filter_map(|(_, ip)| match ip {
            IpAddr::V4(v4) if usable_v4(*v4) => Some(*ip),
            _ => None,
        })
        .collect();
    ips.dedup();
    ips.sort_by_key(|ip| {
        let private = matches!(ip, IpAddr::V4(v4) if v4.is_private());
        (Some(*ip) != routed, !private)
    });
    ips
}

fn routed_ip() -> Option<IpAddr> {
    let socket = UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip())
}

/// Addresses to advertise to the peer. The sender tries every advertised address.
pub fn lan_ips() -> Result<Vec<IpAddr>, String> {
    let candidates: Vec<(String, IpAddr)> = if_addrs::get_if_addrs()
        .map_err(|e| format!("cannot list network interfaces ({e}); pass --ip"))?
        .into_iter()
        .map(|i| (i.name.clone(), i.ip()))
        .collect();
    let ips = select_lan(&candidates, routed_ip());
    if ips.is_empty() {
        return Err("no LAN address found on a physical interface; pass --ip".into());
    }
    Ok(ips)
}

/// Rejects addresses the peer can never reach.
pub fn validate_ip(ip: IpAddr) -> Result<IpAddr, String> {
    if ip.is_unspecified() || ip.is_multicast() || ip.is_loopback() {
        return Err(format!("{ip} is not a usable LAN address"));
    }
    Ok(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(name: &str, ip: &str) -> (String, IpAddr) {
        (name.to_owned(), ip.parse().unwrap())
    }

    #[test]
    fn unusable_addresses_are_rejected() {
        for bad in ["0.0.0.0", "127.0.0.1", "224.0.0.1", "::"] {
            assert!(validate_ip(bad.parse().unwrap()).is_err(), "{bad}");
        }
        assert!(validate_ip("192.168.1.10".parse().unwrap()).is_ok());
    }

    #[test]
    fn fake_ip_tun_is_never_chosen() {
        let found = [
            c("utun4", "198.18.0.1"),
            c("lo0", "127.0.0.1"),
            c("en0", "192.168.1.20"),
            c("en0", "fe80::1"),
            c("awdl0", "169.254.3.3"),
            c("utun5", "100.100.1.1"),
            c("bridge100", "192.168.64.1"),
        ];
        let routed = Some("198.18.0.1".parse().unwrap());
        let picked = select_lan(&found, routed);
        assert_eq!(picked[0], "192.168.1.20".parse::<IpAddr>().unwrap());
        assert!(!picked.contains(&"198.18.0.1".parse().unwrap()));
        assert!(!picked.contains(&"100.100.1.1".parse().unwrap()));
    }

    #[test]
    fn routed_interface_is_preferred_and_private_before_public() {
        let found = [
            c("eth1", "10.0.0.5"),
            c("eth0", "172.16.4.9"),
            c("eth2", "203.0.113.7"),
        ];
        let picked = select_lan(&found, Some("172.16.4.9".parse().unwrap()));
        assert_eq!(
            picked,
            ["172.16.4.9", "10.0.0.5", "203.0.113.7"].map(|s| s.parse::<IpAddr>().unwrap())
        );
        let none = select_lan(&[c("utun1", "198.18.0.1")], None);
        assert!(none.is_empty());
    }
}
