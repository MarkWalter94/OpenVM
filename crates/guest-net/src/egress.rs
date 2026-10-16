//! Closed destination/port policy for filtered Internet. Only IPv4 TCP
//! 80/443 to globally routable addresses, through a default host route.
//! Non-default routes (including public LAN and split VPN routes) are
//! refused. No system HTTP proxy is consulted. IPv6 egress stays closed.
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub address: Ipv4Addr,
    pub prefix: u8,
}

fn contains(route: Route, ip: Ipv4Addr) -> bool {
    if route.prefix > 32 {
        return true;
    }
    let mask = u32::MAX
        .checked_shl(u32::from(32 - route.prefix))
        .unwrap_or(0);
    u32::from(ip) & mask == u32::from(route.address) & mask
}

/// Conservative subset of IANA globally reachable IPv4. Special-use
/// ranges are refused as a whole, including their global exceptions.
pub fn is_public_ipv4(ip: Ipv4Addr) -> bool {
    ![
        ("0.0.0.0", 8),
        ("10.0.0.0", 8),
        ("100.64.0.0", 10),
        ("127.0.0.0", 8),
        ("169.254.0.0", 16),
        ("172.16.0.0", 12),
        ("192.0.0.0", 24),
        ("192.0.2.0", 24),
        ("192.88.99.0", 24),
        ("192.168.0.0", 16),
        ("198.18.0.0", 15),
        ("198.51.100.0", 24),
        ("203.0.113.0", 24),
        ("224.0.0.0", 4),
        ("240.0.0.0", 4),
    ]
    .iter()
    .any(|(address, prefix)| {
        contains(
            Route {
                address: address.parse().expect("constant IPv4"),
                prefix: *prefix,
            },
            ip,
        )
    })
}

pub fn validate_routes(target: SocketAddr, routes: &[Route]) -> Result<(), &'static str> {
    let IpAddr::V4(ip) = target.ip() else {
        return Err("IPv6 Internet egress is disabled");
    };
    if !matches!(target.port(), 80 | 443) || !is_public_ipv4(ip) {
        return Err("destination is not public IPv4 TCP 80/443");
    }
    if !routes.iter().any(|r| r.prefix == 0)
        || routes.iter().any(|r| r.prefix > 0 && contains(*r, ip))
    {
        return Err("destination has a host, LAN, VPN or other non-default route");
    }
    Ok(())
}

/// Refresh routing state for each connection and during persistent
/// connections. An enumeration failure closes egress.
pub fn validate_target(target: SocketAddr) -> Result<(), &'static str> {
    let routes = host_routes().map_err(|_| "host routing table unavailable")?;
    validate_routes(target, &routes)
}

#[cfg(not(windows))]
fn host_routes() -> std::io::Result<Vec<Route>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "filtered Internet currently requires Windows route qualification",
    ))
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn host_routes() -> std::io::Result<Vec<Route>> {
    use windows_sys::Win32::{
        NetworkManagement::IpHelper::{FreeMibTable, GetIpForwardTable2, MIB_IPFORWARD_TABLE2},
        Networking::WinSock::AF_INET,
    };
    struct Table(*mut MIB_IPFORWARD_TABLE2);
    impl Drop for Table {
        fn drop(&mut self) {
            // SAFETY: this pointer is the allocation returned by the API;
            // it is released exactly once after the rows have been copied.
            unsafe {
                FreeMibTable(self.0.cast());
            }
        }
    }
    let mut raw = std::ptr::null_mut();
    // SAFETY: raw points to a writable table pointer. Windows allocates the
    // complete, aligned variable-length table on success.
    let status = unsafe { GetIpForwardTable2(AF_INET, &mut raw) };
    if status != 0 {
        return Err(std::io::Error::from_raw_os_error(status as i32));
    }
    if raw.is_null() {
        return Err(std::io::Error::other("null routing table"));
    }
    let table = Table(raw);
    // SAFETY: successful API return gives an initialized table header.
    let count = unsafe { (*table.0).NumEntries as usize };
    if count > 65536 {
        return Err(std::io::Error::other("routing table exceeds limit"));
    }
    // SAFETY: Windows owns and sized the table for NumEntries rows. The
    // allocation stays alive through Table until all fields are copied.
    let rows =
        unsafe { std::slice::from_raw_parts(std::ptr::addr_of!((*table.0).Table).cast(), count) };
    let mut out = Vec::with_capacity(count);
    for row in rows {
        let row: &windows_sys::Win32::NetworkManagement::IpHelper::MIB_IPFORWARD_ROW2 = row;
        // SAFETY: AF_INET was requested, so Prefix is the IPv4 member.
        let addr = unsafe { row.DestinationPrefix.Prefix.Ipv4.sin_addr.S_un.S_addr };
        out.push(Route {
            address: Ipv4Addr::from(addr.to_ne_bytes()),
            prefix: row.DestinationPrefix.PrefixLength,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_special_ipv6_ports_and_public_lan_are_closed() {
        let routes = [
            Route {
                address: Ipv4Addr::UNSPECIFIED,
                prefix: 0,
            },
            Route {
                address: "8.8.8.0".parse().unwrap(),
                prefix: 24,
            },
        ];
        for ip in [
            "0.1.2.3",
            "10.0.0.1",
            "100.64.0.1",
            "127.1.2.3",
            "169.254.169.254",
            "172.16.0.1",
            "192.168.1.1",
            "198.19.0.1",
            "192.0.2.1",
            "224.0.0.1",
            "255.255.255.255",
            "8.8.8.8",
            "::1",
            "::ffff:1.1.1.1",
            "2606:4700:4700::1111",
        ] {
            assert!(
                validate_routes(SocketAddr::new(ip.parse().unwrap(), 443), &routes).is_err(),
                "{ip}"
            );
        }
        assert!(validate_routes("1.1.1.1:443".parse().unwrap(), &routes).is_ok());
        assert!(validate_routes("1.1.1.1:22".parse().unwrap(), &routes).is_err());
        assert!(validate_routes("1.1.1.1:443".parse().unwrap(), &[]).is_err());
        assert!(
            validate_routes(
                "1.1.1.1:443".parse().unwrap(),
                &[
                    routes[0],
                    Route {
                        address: "0.0.0.0".parse().unwrap(),
                        prefix: 1
                    }
                ]
            )
            .is_err()
        );
    }

    #[test]
    #[cfg(windows)]
    fn actual_windows_route_inventory_is_bounded_and_has_valid_prefixes() {
        let routes = host_routes().unwrap();
        assert!(!routes.is_empty());
        assert!(routes.iter().all(|r| r.prefix <= 32));
    }
}
