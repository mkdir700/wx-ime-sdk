//! Local address discovery for LAN file transfer.
use std::net::{IpAddr, UdpSocket};

/// The address of the interface that routes to the internet/LAN. UDP `connect` sends no packet.
pub fn local_ip() -> Result<IpAddr, String> {
    let socket = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    socket.connect("8.8.8.8:80").map_err(|e| {
        format!("no default route ({e}); pass --ip with this machine's LAN address")
    })?;
    let ip = socket.local_addr().map_err(|e| e.to_string())?.ip();
    if ip.is_unspecified() || ip.is_loopback() {
        return Err("could not detect a LAN address; pass --ip".into());
    }
    Ok(ip)
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

    #[test]
    fn unusable_addresses_are_rejected() {
        for bad in ["0.0.0.0", "127.0.0.1", "224.0.0.1", "::"] {
            assert!(validate_ip(bad.parse().unwrap()).is_err(), "{bad}");
        }
        assert!(validate_ip("192.168.1.10".parse().unwrap()).is_ok());
    }
}
