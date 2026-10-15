//! DHCP server of the stack: one fixed lease for the one guest.
//!
//! DISCOVER → OFFER and REQUEST → ACK (or NAK when the client asks for
//! another address or another server), with: address [`crate::GUEST_IP`],
//! mask `/24`, server identifier and DNS server, search domain
//! [`crate::DOMAIN_SUFFIX`], lease [`crate::LEASE_SECONDS`]. **No router
//! option**: the guest gets no default route (only the guest network is
//! reachable, and there only the stack answers). INFORM gets an ACK without
//! an address; RELEASE and DECLINE are ignored.

use std::net::Ipv4Addr;

use crate::{DNS_IP, DOMAIN_SUFFIX, GATEWAY_IP, GUEST_IP, LEASE_SECONDS, PREFIX_LEN};

const MAGIC: [u8; 4] = [0x63, 0x82, 0x53, 0x63];
const OP_REQUEST: u8 = 1;
const OP_REPLY: u8 = 2;
/// Fixed BOOTP header length up to the magic cookie included.
const HEADER_LEN: usize = 240;
/// Smallest BOOTP message (some clients drop shorter ones).
const MIN_REPLY_LEN: usize = 300;

/// DHCP message types used here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageType {
    /// 1.
    Discover,
    /// 2.
    Offer,
    /// 3.
    Request,
    /// 4.
    Decline,
    /// 5.
    Ack,
    /// 6.
    Nak,
    /// 7.
    Release,
    /// 8.
    Inform,
}

impl MessageType {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            1 => Self::Discover,
            2 => Self::Offer,
            3 => Self::Request,
            4 => Self::Decline,
            5 => Self::Ack,
            6 => Self::Nak,
            7 => Self::Release,
            8 => Self::Inform,
            _ => return None,
        })
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::Discover => 1,
            Self::Offer => 2,
            Self::Request => 3,
            Self::Decline => 4,
            Self::Ack => 5,
            Self::Nak => 6,
            Self::Release => 7,
            Self::Inform => 8,
        }
    }
}

/// A parsed client message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientMessage {
    /// Message type.
    pub kind: MessageType,
    /// Transaction identifier.
    pub xid: u32,
    /// Flags (broadcast bit).
    pub flags: u16,
    /// `ciaddr`.
    pub ciaddr: Ipv4Addr,
    /// `giaddr`.
    pub giaddr: Ipv4Addr,
    /// `chaddr` (16 bytes as sent).
    pub chaddr: [u8; 16],
    /// Option 50.
    pub requested_ip: Option<Ipv4Addr>,
    /// Option 54.
    pub server_id: Option<Ipv4Addr>,
}

fn ip_at(b: &[u8], i: usize) -> Option<Ipv4Addr> {
    let s = b.get(i..i + 4)?;
    Some(Ipv4Addr::new(s[0], s[1], s[2], s[3]))
}

/// Parses a BOOTREQUEST with a DHCP message type. `None` for anything else.
#[must_use]
pub fn parse(msg: &[u8]) -> Option<ClientMessage> {
    if msg.len() < HEADER_LEN || msg[0] != OP_REQUEST || msg[1] != 1 || msg[2] != 6 {
        return None;
    }
    if msg[236..240] != MAGIC {
        return None;
    }
    let xid = u32::from_be_bytes([msg[4], msg[5], msg[6], msg[7]]);
    let flags = u16::from_be_bytes([msg[10], msg[11]]);
    let ciaddr = ip_at(msg, 12)?;
    let giaddr = ip_at(msg, 24)?;
    let mut chaddr = [0u8; 16];
    chaddr.copy_from_slice(&msg[28..44]);
    let mut kind = None;
    let mut requested_ip = None;
    let mut server_id = None;
    let mut i = HEADER_LEN;
    while i < msg.len() {
        let code = msg[i];
        if code == 0 {
            i += 1;
            continue;
        }
        if code == 255 {
            break;
        }
        let len = usize::from(*msg.get(i + 1)?);
        let val = msg.get(i + 2..i + 2 + len)?;
        match (code, len) {
            (53, 1) => kind = MessageType::from_u8(val[0]),
            (50, 4) => requested_ip = ip_at(val, 0),
            (54, 4) => server_id = ip_at(val, 0),
            _ => {}
        }
        i += 2 + len;
    }
    Some(ClientMessage {
        kind: kind?,
        xid,
        flags,
        ciaddr,
        giaddr,
        chaddr,
        requested_ip,
        server_id,
    })
}

/// What the server answers to `m`, if anything.
#[must_use]
pub fn reply_type(m: &ClientMessage) -> Option<MessageType> {
    match m.kind {
        MessageType::Discover => Some(MessageType::Offer),
        MessageType::Request => {
            // SELECTING: server id present; INIT-REBOOT: requested ip;
            // RENEWING/REBINDING: ciaddr.
            if m.server_id.is_some_and(|s| s != GATEWAY_IP) {
                return None; // another server was chosen
            }
            let asked = m.requested_ip.unwrap_or(m.ciaddr);
            if asked == GUEST_IP {
                Some(MessageType::Ack)
            } else {
                Some(MessageType::Nak)
            }
        }
        MessageType::Inform => Some(MessageType::Ack),
        _ => None,
    }
}

fn mask() -> Ipv4Addr {
    Ipv4Addr::from(u32::MAX << (32 - u32::from(PREFIX_LEN)))
}

/// Builds the server message of type `kind` answering `m` (UDP payload).
#[must_use]
pub fn build_reply(m: &ClientMessage, kind: MessageType) -> Vec<u8> {
    build_reply_with_dns(m, kind, true)
}

/// Tools-only links have no DNS or search suffix, so another NIC retains
/// the DNS configuration that belongs to the user's NAT connection.
pub fn build_reply_with_dns(m: &ClientMessage, kind: MessageType, advertise_dns: bool) -> Vec<u8> {
    let mut out = vec![0u8; HEADER_LEN];
    out[0] = OP_REPLY;
    out[1] = 1;
    out[2] = 6;
    out[4..8].copy_from_slice(&m.xid.to_be_bytes());
    out[10..12].copy_from_slice(&m.flags.to_be_bytes());
    let inform = m.kind == MessageType::Inform;
    if inform {
        out[12..16].copy_from_slice(&m.ciaddr.octets());
    }
    if matches!(kind, MessageType::Offer | MessageType::Ack) && !inform {
        out[16..20].copy_from_slice(&GUEST_IP.octets()); // yiaddr
    }
    out[24..28].copy_from_slice(&m.giaddr.octets());
    out[28..44].copy_from_slice(&m.chaddr);
    out[236..240].copy_from_slice(&MAGIC);
    let mut opt = |code: u8, val: &[u8]| {
        out.push(code);
        out.push(u8::try_from(val.len()).unwrap_or(0));
        out.extend_from_slice(val);
    };
    opt(53, &[kind.as_u8()]);
    opt(54, &GATEWAY_IP.octets());
    if kind != MessageType::Nak {
        if !inform {
            opt(51, &LEASE_SECONDS.to_be_bytes());
            opt(58, &(LEASE_SECONDS / 2).to_be_bytes());
            opt(59, &(LEASE_SECONDS / 8 * 7).to_be_bytes());
        }
        opt(1, &mask().octets());
        if advertise_dns {
            opt(6, &DNS_IP.octets());
            opt(15, DOMAIN_SUFFIX.as_bytes());
            // Search list (RFC 3397, option 119) for clients that ignore 15.
            let mut search = Vec::new();
            for l in DOMAIN_SUFFIX.split('.') {
                search.push(u8::try_from(l.len()).unwrap_or(0));
                search.extend_from_slice(l.as_bytes());
            }
            search.push(0);
            opt(119, &search);
        }
    }
    out.push(255);
    if out.len() < MIN_REPLY_LEN {
        out.resize(MIN_REPLY_LEN, 0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn client(
        kind: u8,
        requested: Option<Ipv4Addr>,
        server: Option<Ipv4Addr>,
    ) -> Vec<u8> {
        let mut m = vec![0u8; HEADER_LEN];
        m[0] = 1;
        m[1] = 1;
        m[2] = 6;
        m[4..8].copy_from_slice(&0xdead_beef_u32.to_be_bytes());
        m[28..34].copy_from_slice(&crate::GUEST_MAC);
        m[236..240].copy_from_slice(&MAGIC);
        m.extend_from_slice(&[53, 1, kind]);
        if let Some(r) = requested {
            m.extend_from_slice(&[50, 4]);
            m.extend_from_slice(&r.octets());
        }
        if let Some(s) = server {
            m.extend_from_slice(&[54, 4]);
            m.extend_from_slice(&s.octets());
        }
        m.extend_from_slice(&[55, 3, 1, 3, 6, 255]);
        m
    }

    fn options(r: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut v = Vec::new();
        let mut i = HEADER_LEN;
        while i < r.len() && r[i] != 255 {
            let len = usize::from(r[i + 1]);
            v.push((r[i], r[i + 2..i + 2 + len].to_vec()));
            i += 2 + len;
        }
        v
    }

    #[test]
    fn discover_offer_request_ack_without_router() {
        let d = parse(&client(1, None, None)).unwrap();
        assert_eq!(d.kind, MessageType::Discover);
        let t = reply_type(&d).unwrap();
        assert_eq!(t, MessageType::Offer);
        let o = build_reply(&d, t);
        assert!(o.len() >= MIN_REPLY_LEN);
        assert_eq!(o[0], 2);
        assert_eq!(&o[4..8], &0xdead_beef_u32.to_be_bytes());
        assert_eq!(&o[16..20], &GUEST_IP.octets());
        let opts = options(&o);
        assert!(opts.contains(&(53, vec![2])));
        assert!(opts.contains(&(54, GATEWAY_IP.octets().to_vec())));
        assert!(opts.contains(&(1, vec![255, 255, 255, 0])));
        assert!(opts.contains(&(6, DNS_IP.octets().to_vec())));
        assert!(opts.contains(&(15, DOMAIN_SUFFIX.as_bytes().to_vec())));
        assert!(!opts.iter().any(|(c, _)| *c == 3), "no router option");

        let r = parse(&client(3, Some(GUEST_IP), Some(GATEWAY_IP))).unwrap();
        assert_eq!(reply_type(&r), Some(MessageType::Ack));
        let a = build_reply(&r, MessageType::Ack);
        assert!(options(&a).contains(&(53, vec![5])));
        assert!(options(&a).contains(&(51, LEASE_SECONDS.to_be_bytes().to_vec())));
    }

    #[test]
    fn other_addresses_and_servers() {
        let r = parse(&client(3, Some(Ipv4Addr::new(10, 0, 2, 99)), None)).unwrap();
        assert_eq!(reply_type(&r), Some(MessageType::Nak));
        let n = build_reply(&r, MessageType::Nak);
        assert_eq!(&n[16..20], &[0, 0, 0, 0]);
        assert!(!options(&n).iter().any(|(c, _)| *c == 6));
        let r = parse(&client(3, Some(GUEST_IP), Some(Ipv4Addr::new(10, 0, 2, 9)))).unwrap();
        assert_eq!(reply_type(&r), None);
        let r = parse(&client(7, None, None)).unwrap();
        assert_eq!(reply_type(&r), None);
        let i = parse(&client(8, None, None)).unwrap();
        assert_eq!(reply_type(&i), Some(MessageType::Ack));
    }

    #[test]
    fn malformed_messages() {
        assert!(parse(&[]).is_none());
        let mut m = client(1, None, None);
        m[236] = 0;
        assert!(parse(&m).is_none());
        let mut m = client(1, None, None);
        m[0] = 2;
        assert!(parse(&m).is_none());
        let mut m = client(1, None, None);
        m.truncate(HEADER_LEN + 2); // option without its value
        assert!(parse(&m).is_none());
        for len in 0..400 {
            let v: Vec<u8> = (0..len).map(|i| (i * 31 + 7) as u8).collect();
            let _ = parse(&v);
        }
    }
}
