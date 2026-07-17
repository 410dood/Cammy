//! Minimal shim for the abandoned `get_if_addrs` crate (see Cargo.toml for why).
//!
//! `hap 0.1.0-pre.15` calls exactly one thing: `get_if_addrs()` and then
//! `iface.is_loopback()` / `iface.ip()` to pick the first non-loopback local
//! address (`hap-0.1.0-pre.15/src/config.rs::get_local_ip`). Instead of
//! enumerating interfaces (the part that needed a native-linked sys crate),
//! we resolve the default-route local IP with the classic connected-UDP-socket
//! trick — no packet is sent. On a multi-homed machine this is actually an
//! improvement: it yields the address the OS would route external traffic
//! from, rather than whichever interface happens to enumerate first.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};

/// A single pseudo-interface carrying one local IP address.
pub struct Interface {
    addr: IpAddr,
}

impl Interface {
    pub fn ip(&self) -> IpAddr {
        self.addr
    }

    pub fn is_loopback(&self) -> bool {
        self.addr.is_loopback()
    }
}

/// Returns the default-route local IP as a one-element interface list, or an
/// empty list if it cannot be determined (the caller falls back to 127.0.0.1).
pub fn get_if_addrs() -> std::io::Result<Vec<Interface>> {
    let probe = || -> Option<IpAddr> {
        let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
        // connect() only sets the route; nothing is transmitted.
        sock.connect(("8.8.8.8", 80)).ok()?;
        Some(sock.local_addr().ok()?.ip())
    };
    Ok(probe()
        .filter(|ip| !ip.is_loopback() && !ip.is_unspecified())
        .map(|addr| vec![Interface { addr }])
        .unwrap_or_default())
}
