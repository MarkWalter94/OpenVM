//! DNS responder of the stack (UDP to `10.0.2.3:53`).
//!
//! Only the names of the endpoints allowed at that moment resolve (type `A`,
//! to their virtual address; `name` and `name.lightvm.internal`). A known
//! name asked with another type gets an empty `NOERROR` answer (so that a
//! client asking `AAAA` first falls back to `A`); every other name gets
//! `NXDOMAIN`. Nothing is forwarded: the stack has no upstream resolver.
//! Malformed queries are dropped without an answer.

use std::net::Ipv4Addr;

use crate::DOMAIN_SUFFIX;

/// Longest endpoint name (DNS limit of a name in text form).
pub const MAX_NAME_LEN: usize = 253;
/// Longest label.
pub const MAX_LABEL_LEN: usize = 63;
/// TTL of the answers, in seconds (short: the allowed set changes).
pub const ANSWER_TTL: u32 = 30;

const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;
const RCODE_NXDOMAIN: u8 = 3;

/// Checks an endpoint name: DNS host name syntax (labels of letters, digits
/// and `-`, not starting or ending with `-`, 1..=63 characters, at most 253
/// in total), no wildcard, not a dotted-quad address, and not under the
/// search domain (the stack adds it itself).
pub fn check_name(name: &str) -> Result<(), String> {
    let n = name.strip_suffix('.').unwrap_or(name);
    if n.is_empty() || n.len() > MAX_NAME_LEN {
        return Err(format!(
            "endpoint name must have 1..={MAX_NAME_LEN} characters"
        ));
    }
    if n.contains('*') {
        return Err("wildcards are not allowed in endpoint names".into());
    }
    for label in n.split('.') {
        if label.is_empty() || label.len() > MAX_LABEL_LEN {
            return Err(format!(
                "label `{label}` must have 1..={MAX_LABEL_LEN} characters"
            ));
        }
        if !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(format!(
                "label `{label}`: only letters, digits and `-` are allowed"
            ));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(format!("label `{label}` starts or ends with `-`"));
        }
    }
    if n.parse::<Ipv4Addr>().is_ok() || n.split('.').all(|l| l.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("an endpoint name cannot be a numeric address".into());
    }
    let lower = normalize(n);
    if lower == DOMAIN_SUFFIX || lower.ends_with(&format!(".{DOMAIN_SUFFIX}")) {
        return Err(format!(
            "endpoint names must not end with `{DOMAIN_SUFFIX}` (added by LightVM)"
        ));
    }
    Ok(())
}

/// Lower-case name without the trailing dot.
#[must_use]
pub fn normalize(name: &str) -> String {
    name.strip_suffix('.').unwrap_or(name).to_ascii_lowercase()
}

/// Name a query asks for, with the search domain removed.
fn lookup_key(qname: &str) -> String {
    let n = normalize(qname);
    match n.strip_suffix(DOMAIN_SUFFIX) {
        Some(rest) if rest.ends_with('.') && rest.len() > 1 => rest[..rest.len() - 1].to_owned(),
        _ => n,
    }
}

/// A parsed query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    /// Transaction identifier.
    pub id: u16,
    /// Recursion desired flag.
    pub rd: bool,
    /// Name in text form (without the trailing dot).
    pub name: String,
    /// Query type.
    pub qtype: u16,
    /// Query class.
    pub qclass: u16,
    /// Length of the question section (copied into the answer).
    question_end: usize,
}

/// Parses a standard query with exactly one question (no compression in
/// the question, as every resolver sends). `None` for anything else.
#[must_use]
pub fn parse_query(msg: &[u8]) -> Option<Query> {
    if msg.len() < 12 || msg.len() > 512 {
        return None;
    }
    let id = u16::from_be_bytes([msg[0], msg[1]]);
    let flags = u16::from_be_bytes([msg[2], msg[3]]);
    let qr = flags & 0x8000 != 0;
    let opcode = (flags >> 11) & 0xf;
    let qdcount = u16::from_be_bytes([msg[4], msg[5]]);
    let ancount = u16::from_be_bytes([msg[6], msg[7]]);
    let nscount = u16::from_be_bytes([msg[8], msg[9]]);
    if qr || opcode != 0 || qdcount != 1 || ancount != 0 || nscount != 0 {
        return None;
    }
    let mut i = 12;
    let mut labels: Vec<String> = Vec::new();
    let mut total = 0usize;
    loop {
        let len = usize::from(*msg.get(i)?);
        i += 1;
        if len == 0 {
            break;
        }
        if len > MAX_LABEL_LEN {
            return None; // compression pointers or invalid lengths
        }
        let label = msg.get(i..i + len)?;
        total += len + 1;
        if total > MAX_NAME_LEN + 1 {
            return None;
        }
        if !label.iter().all(|b| b.is_ascii_graphic()) {
            return None;
        }
        labels.push(String::from_utf8_lossy(label).into_owned());
        i += len;
    }
    let qtype = u16::from_be_bytes([*msg.get(i)?, *msg.get(i + 1)?]);
    let qclass = u16::from_be_bytes([*msg.get(i + 2)?, *msg.get(i + 3)?]);
    Some(Query {
        id,
        rd: flags & 0x0100 != 0,
        name: labels.join("."),
        qtype,
        qclass,
        question_end: i + 4,
    })
}

/// Outcome of a lookup, for the counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// An address was returned.
    Address(Ipv4Addr),
    /// Known name, other type: empty `NOERROR`.
    NoData,
    /// Unknown or not allowed name: `NXDOMAIN`.
    NxDomain,
}

/// Decides the answer: `resolve` maps an allowed endpoint name (normalized)
/// to its virtual address.
#[must_use]
pub fn decide(query: &Query, resolve: &dyn Fn(&str) -> Option<Ipv4Addr>) -> Answer {
    match resolve(&lookup_key(&query.name)) {
        Some(ip) if query.qtype == TYPE_A && query.qclass == CLASS_IN => Answer::Address(ip),
        Some(_) => Answer::NoData,
        None => Answer::NxDomain,
    }
}

/// Builds the response message for `query` (`msg` is the original query).
#[must_use]
pub fn build_response(msg: &[u8], query: &Query, answer: Answer) -> Vec<u8> {
    let mut out = Vec::with_capacity(query.question_end + 16);
    out.extend_from_slice(&query.id.to_be_bytes());
    // QR=1, opcode 0, AA=1, TC=0, RD copied, RA=1.
    let mut flags: u16 = 0x8000 | 0x0400 | 0x0080;
    if query.rd {
        flags |= 0x0100;
    }
    if answer == Answer::NxDomain {
        flags |= u16::from(RCODE_NXDOMAIN);
    }
    out.extend_from_slice(&flags.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    let an: u16 = u16::from(matches!(answer, Answer::Address(_)));
    out.extend_from_slice(&an.to_be_bytes()); // ANCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    out.extend_from_slice(&msg[12..query.question_end]);
    if let Answer::Address(ip) = answer {
        out.extend_from_slice(&[0xc0, 0x0c]); // pointer to the question name
        out.extend_from_slice(&TYPE_A.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&ANSWER_TTL.to_be_bytes());
        out.extend_from_slice(&4u16.to_be_bytes());
        out.extend_from_slice(&ip.octets());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Query for `name` with type `qtype`.
    pub(crate) fn query(name: &str, qtype: u16) -> Vec<u8> {
        let mut q = vec![0x12, 0x34, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        for l in name.split('.') {
            q.push(u8::try_from(l.len()).unwrap());
            q.extend_from_slice(l.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&1u16.to_be_bytes());
        q
    }

    fn resolver(n: &str) -> Option<Ipv4Addr> {
        (n == "sql.lan").then_some(Ipv4Addr::new(10, 0, 2, 101))
    }

    #[test]
    fn names() {
        for ok in ["sql", "sql.lan", "WCF-host.corp.example", "a1", "x.y."] {
            check_name(ok).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            "",
            "*.lan",
            "a*b",
            "-a",
            "a-",
            "a..b",
            "a b",
            "10.0.2.5",
            "123",
            "under_score",
            "x.lightvm.internal",
            "lightvm.internal",
            &"a".repeat(64),
            &format!("{}.b", "a.".repeat(130)),
        ] {
            assert!(check_name(bad).is_err(), "{bad:?} accepted");
        }
        assert_eq!(normalize("SQL.Lan."), "sql.lan");
        assert_eq!(lookup_key("sql.lan.lightvm.internal"), "sql.lan");
        assert_eq!(lookup_key("SQL.LIGHTVM.INTERNAL."), "sql");
        assert_eq!(lookup_key("lightvm.internal"), "lightvm.internal");
    }

    #[test]
    fn allowed_name_resolves_others_do_not() {
        let m = query("SQL.lan", 1);
        let q = parse_query(&m).unwrap();
        assert_eq!(q.name, "SQL.lan");
        let a = decide(&q, &resolver);
        assert_eq!(a, Answer::Address(Ipv4Addr::new(10, 0, 2, 101)));
        let r = build_response(&m, &q, a);
        assert_eq!(&r[..2], &[0x12, 0x34]);
        assert_eq!(r[3] & 0x0f, 0, "NOERROR");
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 1, "one answer");
        assert_eq!(&r[r.len() - 4..], &[10, 0, 2, 101]);

        let m = query("sql.lan.lightvm.internal", 1);
        let q = parse_query(&m).unwrap();
        assert!(matches!(decide(&q, &resolver), Answer::Address(_)));

        let m = query("sql.lan", 28);
        let q = parse_query(&m).unwrap();
        let a = decide(&q, &resolver);
        assert_eq!(a, Answer::NoData);
        let r = build_response(&m, &q, a);
        assert_eq!(r[3] & 0x0f, 0);
        assert_eq!(u16::from_be_bytes([r[6], r[7]]), 0);

        for other in ["example.com", "sql", "lan", "sql.lan.example"] {
            let m = query(other, 1);
            let q = parse_query(&m).unwrap();
            let a = decide(&q, &resolver);
            assert_eq!(a, Answer::NxDomain, "{other}");
            let r = build_response(&m, &q, a);
            assert_eq!(r[3] & 0x0f, 3, "{other}: NXDOMAIN");
        }
    }

    #[test]
    fn malformed_queries_are_dropped() {
        assert!(parse_query(&[]).is_none());
        assert!(parse_query(&[0; 11]).is_none());
        let mut m = query("sql.lan", 1);
        m[2] |= 0x80; // a response, not a query
        assert!(parse_query(&m).is_none());
        let mut m = query("sql.lan", 1);
        m[5] = 2; // two questions
        assert!(parse_query(&m).is_none());
        let mut m = query("sql.lan", 1);
        m.truncate(m.len() - 3);
        assert!(parse_query(&m).is_none());
        let mut m = query("sql.lan", 1);
        m[12] = 0xc0; // compression pointer in the question
        assert!(parse_query(&m).is_none());
        // Arbitrary bytes never panic.
        for len in 0..80 {
            let v: Vec<u8> = (0..len).map(|i| (i * 37 + len) as u8).collect();
            let _ = parse_query(&v);
        }
    }
}
