//! Classification of the guest's frames before smoltcp sees them, and the
//! frames the stack builds itself (DHCP, DNS).
//!
//! smoltcp runs with "any IP" so that it can terminate connections to the
//! virtual addresses; this filter is therefore what decides what reaches it:
//! ARP for the stack's own addresses, ICMP to them, TCP to them. Everything
//! else is answered here (DHCP, DNS) or dropped.

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddrV4};

use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{
    ArpOperation, ArpPacket, ArpRepr, EthernetAddress, EthernetFrame, EthernetProtocol,
    EthernetRepr, IpAddress, IpProtocol, Ipv4Packet, Ipv4Repr, TcpPacket, UdpPacket, UdpRepr,
};

use crate::{DNS_IP, GATEWAY_IP, GATEWAY_MAC, GUEST_IP};

/// Why a frame is not handed to the stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Dropped {
    /// UDP other than DHCP and DNS to the stack.
    Udp,
    /// IPv6.
    Ipv6,
    /// TCP SYN to an address that is not the stack's.
    TcpOutside(SocketAddrV4),
    /// Anything else.
    Other,
}

/// What to do with a frame from the guest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Class {
    /// Hand it to smoltcp as it is.
    Stack,
    /// TCP SYN (no ACK) to one of the stack's addresses: the caller decides
    /// whether a listener is created for it, then hands it to smoltcp (which
    /// resets it if nothing listens).
    TcpSyn {
        /// Destination.
        dst: SocketAddrV4,
        /// Guest source port.
        src_port: u16,
    },
    /// DHCP message (UDP payload) from the client with this MAC.
    Dhcp {
        /// Payload.
        payload: Vec<u8>,
    },
    /// DNS query to the stack.
    Dns {
        /// Payload.
        payload: Vec<u8>,
        /// Client MAC.
        src_mac: [u8; 6],
        /// Client address and port.
        src: SocketAddrV4,
    },
    /// Dropped.
    Drop(Dropped),
}

/// Classifies a frame. `owned` are the virtual addresses of the endpoints
/// (the gateway and DNS addresses are always owned).
pub(crate) fn classify(frame: &[u8], owned: &BTreeSet<Ipv4Addr>) -> Class {
    let is_owned = |ip: Ipv4Addr| ip == GATEWAY_IP || ip == DNS_IP || owned.contains(&ip);
    let Ok(eth) = EthernetFrame::new_checked(frame) else {
        return Class::Drop(Dropped::Other);
    };
    let dst_mac = eth.dst_addr();
    if dst_mac != EthernetAddress(GATEWAY_MAC) && !dst_mac.is_broadcast() {
        return Class::Drop(Dropped::Other);
    }
    let src_mac = eth.src_addr();
    match eth.ethertype() {
        EthernetProtocol::Arp => {
            let Ok(p) = ArpPacket::new_checked(eth.payload()) else {
                return Class::Drop(Dropped::Other);
            };
            match ArpRepr::parse(&p) {
                Ok(ArpRepr::EthernetIpv4 {
                    operation: ArpOperation::Request | ArpOperation::Reply,
                    source_protocol_addr,
                    target_protocol_addr,
                    ..
                }) if is_owned(target_protocol_addr) && source_protocol_addr == GUEST_IP => {
                    Class::Stack
                }
                _ => Class::Drop(Dropped::Other),
            }
        }
        EthernetProtocol::Ipv6 => Class::Drop(Dropped::Ipv6),
        EthernetProtocol::Ipv4 => classify_ipv4(eth.payload(), src_mac.0, &is_owned),
        _ => Class::Drop(Dropped::Other),
    }
}

fn classify_ipv4(payload: &[u8], src_mac: [u8; 6], is_owned: &dyn Fn(Ipv4Addr) -> bool) -> Class {
    let caps = ChecksumCapabilities::default();
    let Ok(p) = Ipv4Packet::new_checked(payload) else {
        return Class::Drop(Dropped::Other);
    };
    if p.more_frags() || p.frag_offset() != 0 {
        return Class::Drop(Dropped::Other);
    }
    let Ok(ip) = Ipv4Repr::parse(&p, &caps) else {
        return Class::Drop(Dropped::Other);
    };
    let body = p.payload();
    match ip.next_header {
        IpProtocol::Udp => {
            let Ok(u) = UdpPacket::new_checked(body) else {
                return Class::Drop(Dropped::Udp);
            };
            let Ok(repr) = UdpRepr::parse(
                &u,
                &IpAddress::Ipv4(ip.src_addr),
                &IpAddress::Ipv4(ip.dst_addr),
                &caps,
            ) else {
                return Class::Drop(Dropped::Udp);
            };
            if repr.dst_port == 67
                && repr.src_port == 68
                && (ip.dst_addr.is_broadcast() || ip.dst_addr == GATEWAY_IP)
            {
                Class::Dhcp {
                    payload: u.payload().to_vec(),
                }
            } else if repr.dst_port == 53 && ip.dst_addr == DNS_IP && ip.src_addr == GUEST_IP {
                Class::Dns {
                    payload: u.payload().to_vec(),
                    src_mac,
                    src: SocketAddrV4::new(ip.src_addr, repr.src_port),
                }
            } else {
                Class::Drop(Dropped::Udp)
            }
        }
        _ if ip.src_addr != GUEST_IP => Class::Drop(Dropped::Other),
        IpProtocol::Tcp => {
            let Ok(t) = TcpPacket::new_checked(body) else {
                return Class::Drop(Dropped::Other);
            };
            let syn = t.syn() && !t.ack();
            let dst = SocketAddrV4::new(ip.dst_addr, t.dst_port());
            match (is_owned(ip.dst_addr), syn) {
                (true, true) => Class::TcpSyn {
                    dst,
                    src_port: t.src_port(),
                },
                (true, false) => Class::Stack,
                (false, true) => Class::Drop(Dropped::TcpOutside(dst)),
                (false, false) => Class::Drop(Dropped::Other),
            }
        }
        IpProtocol::Icmp if is_owned(ip.dst_addr) => Class::Stack,
        _ => Class::Drop(Dropped::Other),
    }
}

/// Ethernet + IPv4 + UDP frame from the stack to the guest.
pub(crate) fn udp_frame(
    dst_mac: [u8; 6],
    src: SocketAddrV4,
    dst: SocketAddrV4,
    payload: &[u8],
) -> Vec<u8> {
    let caps = ChecksumCapabilities::default();
    let udp = UdpRepr {
        src_port: src.port(),
        dst_port: dst.port(),
    };
    let ip = Ipv4Repr {
        src_addr: *src.ip(),
        dst_addr: *dst.ip(),
        next_header: IpProtocol::Udp,
        payload_len: udp.header_len() + payload.len(),
        hop_limit: 64,
    };
    let eth = EthernetRepr {
        src_addr: EthernetAddress(GATEWAY_MAC),
        dst_addr: EthernetAddress(dst_mac),
        ethertype: EthernetProtocol::Ipv4,
    };
    let mut buf = vec![0u8; eth.buffer_len() + ip.buffer_len() + ip.payload_len];
    let mut f = EthernetFrame::new_unchecked(&mut buf[..]);
    eth.emit(&mut f);
    let mut p = Ipv4Packet::new_unchecked(f.payload_mut());
    ip.emit(&mut p, &caps);
    let mut u = UdpPacket::new_unchecked(p.payload_mut());
    udp.emit(
        &mut u,
        &IpAddress::Ipv4(ip.src_addr),
        &IpAddress::Ipv4(ip.dst_addr),
        payload.len(),
        |b| b.copy_from_slice(payload),
        &caps,
    );
    buf
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// IPv4 frame from the guest with the given protocol body.
    pub(crate) fn ipv4_frame(
        src: Ipv4Addr,
        dst: Ipv4Addr,
        proto: IpProtocol,
        body: &[u8],
        dst_mac: [u8; 6],
    ) -> Vec<u8> {
        let caps = ChecksumCapabilities::default();
        let ip = Ipv4Repr {
            src_addr: src,
            dst_addr: dst,
            next_header: proto,
            payload_len: body.len(),
            hop_limit: 64,
        };
        let eth = EthernetRepr {
            src_addr: EthernetAddress(crate::GUEST_MAC),
            dst_addr: EthernetAddress(dst_mac),
            ethertype: EthernetProtocol::Ipv4,
        };
        let mut buf = vec![0u8; eth.buffer_len() + ip.buffer_len() + body.len()];
        let mut f = EthernetFrame::new_unchecked(&mut buf[..]);
        eth.emit(&mut f);
        let mut p = Ipv4Packet::new_unchecked(f.payload_mut());
        ip.emit(&mut p, &caps);
        p.payload_mut().copy_from_slice(body);
        buf
    }

    /// UDP datagram from the guest.
    pub(crate) fn udp_from_guest(src: SocketAddrV4, dst: SocketAddrV4, payload: &[u8]) -> Vec<u8> {
        let caps = ChecksumCapabilities::default();
        let udp = UdpRepr {
            src_port: src.port(),
            dst_port: dst.port(),
        };
        let mut body = vec![0u8; udp.header_len() + payload.len()];
        let mut u = UdpPacket::new_unchecked(&mut body[..]);
        udp.emit(
            &mut u,
            &IpAddress::Ipv4(*src.ip()),
            &IpAddress::Ipv4(*dst.ip()),
            payload.len(),
            |b| b.copy_from_slice(payload),
            &caps,
        );
        let mac = if dst.ip().is_broadcast() {
            [0xff; 6]
        } else {
            GATEWAY_MAC
        };
        ipv4_frame(*src.ip(), *dst.ip(), IpProtocol::Udp, &body, mac)
    }

    /// Bare TCP header (SYN or ACK) from the guest, checksum computed.
    pub(crate) fn tcp_from_guest(dst: SocketAddrV4, src_port: u16, syn: bool) -> Vec<u8> {
        let mut h = vec![0u8; 20];
        h[0..2].copy_from_slice(&src_port.to_be_bytes());
        h[2..4].copy_from_slice(&dst.port().to_be_bytes());
        h[4..8].copy_from_slice(&1000u32.to_be_bytes());
        h[12] = 5 << 4;
        h[13] = if syn { 0x02 } else { 0x10 };
        h[14..16].copy_from_slice(&64240u16.to_be_bytes());
        let mut t = TcpPacket::new_unchecked(&mut h[..]);
        t.fill_checksum(&IpAddress::Ipv4(GUEST_IP), &IpAddress::Ipv4(*dst.ip()));
        ipv4_frame(GUEST_IP, *dst.ip(), IpProtocol::Tcp, &h, GATEWAY_MAC)
    }

    fn owned() -> BTreeSet<Ipv4Addr> {
        [Ipv4Addr::new(10, 0, 2, 101)].into_iter().collect()
    }

    #[test]
    fn classification() {
        let vip = SocketAddrV4::new(Ipv4Addr::new(10, 0, 2, 101), 1433);
        let g = SocketAddrV4::new(GUEST_IP, 40000);
        assert_eq!(
            classify(&tcp_from_guest(vip, 40000, true), &owned()),
            Class::TcpSyn {
                dst: vip,
                src_port: 40000
            }
        );
        assert_eq!(
            classify(&tcp_from_guest(vip, 40000, false), &owned()),
            Class::Stack
        );
        let lan = SocketAddrV4::new(Ipv4Addr::new(192, 168, 1, 1), 445);
        assert_eq!(
            classify(&tcp_from_guest(lan, 40000, true), &owned()),
            Class::Drop(Dropped::TcpOutside(lan))
        );
        let dns = udp_from_guest(g, SocketAddrV4::new(DNS_IP, 53), b"q");
        assert!(matches!(classify(&dns, &owned()), Class::Dns { .. }));
        let other_dns = udp_from_guest(g, SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 53), b"q");
        assert_eq!(classify(&other_dns, &owned()), Class::Drop(Dropped::Udp));
        let dhcp = udp_from_guest(
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 68),
            SocketAddrV4::new(Ipv4Addr::BROADCAST, 67),
            b"x",
        );
        assert!(matches!(classify(&dhcp, &owned()), Class::Dhcp { .. }));
        // IPv6 ethertype.
        let mut v6 = tcp_from_guest(vip, 1, true);
        v6[12..14].copy_from_slice(&[0x86, 0xdd]);
        assert_eq!(classify(&v6, &owned()), Class::Drop(Dropped::Ipv6));
        // Frame to another MAC.
        let mut other = tcp_from_guest(vip, 1, true);
        other[0..6].copy_from_slice(&[2, 0, 0, 0, 0, 1]);
        assert_eq!(classify(&other, &owned()), Class::Drop(Dropped::Other));
        // Truncated.
        assert_eq!(classify(&[0u8; 10], &owned()), Class::Drop(Dropped::Other));
        // Spoofed source.
        let mut h = vec![0u8; 20];
        h[13] = 0x02;
        h[12] = 5 << 4;
        let spoof = ipv4_frame(
            Ipv4Addr::new(10, 0, 2, 50),
            *vip.ip(),
            IpProtocol::Tcp,
            &h,
            GATEWAY_MAC,
        );
        assert_eq!(classify(&spoof, &owned()), Class::Drop(Dropped::Other));
    }
}
