//! Windows TPM loopback transport ownership checks (ADR-0035).
//! Do not connect to probe readiness: a foreign first client can take swtpm's
//! unauthenticated endpoint. Inspect the kernel TCP table and fail closed.
use crate::error::{BackendError, Result};

#[cfg(windows)]
pub(crate) fn ready(ports: [u16; 2], server: u32) -> Result<bool> {
    let rows = crate::sys::windows::tcp_rows()?;
    let mut ready = true;
    for port in ports {
        let listeners: Vec<_> = rows
            .iter()
            .filter(|r| r.listening && r.local.port() == port)
            .collect();
        if listeners
            .iter()
            .any(|r| r.pid != server || !r.local.ip().is_loopback())
        {
            return Err(BackendError::StateConflict(
                "TPM port owned by another process".into(),
            ));
        }
        ready &= listeners.len() == 1;
    }
    Ok(ready)
}

#[cfg(windows)]
pub(crate) fn peers(ports: [u16; 2], server: u32, client: u32) -> Result<bool> {
    let rows = crate::sys::windows::tcp_rows()?;
    let mut ready = true;
    for port in ports {
        let incoming: Vec<_> = rows
            .iter()
            .filter(|r| r.established && r.local.port() == port)
            .collect();
        if incoming.is_empty() {
            ready = false;
            continue;
        }
        if incoming.len() != 1
            || incoming[0].pid != server
            || !incoming[0].local.ip().is_loopback()
            || !incoming[0].remote.ip().is_loopback()
        {
            return Err(BackendError::StateConflict(
                "TPM server has missing or unexpected peers".into(),
            ));
        }
        let connection = incoming[0];
        let outgoing: Vec<_> = rows
            .iter()
            .filter(|r| {
                r.established && r.local == connection.remote && r.remote == connection.local
            })
            .collect();
        if outgoing.len() != 1 || outgoing[0].pid != client {
            return Err(BackendError::StateConflict(
                "TPM connection is not owned by this QEMU run".into(),
            ));
        }
    }
    Ok(ready)
}

#[cfg(not(windows))]
pub(crate) fn ready(_: [u16; 2], _: u32) -> Result<bool> {
    Err(BackendError::StateConflict(
        "Windows TPM transport on another host".into(),
    ))
}
#[cfg(not(windows))]
pub(crate) fn peers(_: [u16; 2], _: u32, _: u32) -> Result<bool> {
    Err(BackendError::StateConflict(
        "Windows TPM transport on another host".into(),
    ))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    #[test]
    fn kernel_table_checks_listeners_and_both_peer_directions() {
        use std::net::{Ipv4Addr, TcpListener, TcpStream};
        let listeners = [
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
            TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap(),
        ];
        let ports = listeners.each_ref().map(|l| l.local_addr().unwrap().port());
        let pid = std::process::id();
        assert!(ready(ports, pid).unwrap());
        assert!(ready(ports, 0).is_err());
        assert!(!peers(ports, pid, pid).unwrap());
        let _clients = ports.map(|p| TcpStream::connect((Ipv4Addr::LOCALHOST, p)).unwrap());
        let _accepted = listeners.each_ref().map(|l| l.accept().unwrap().0);
        assert!(peers(ports, pid, pid).unwrap());
        assert!(peers(ports, pid, 0).is_err());
        // An extra local client is never accepted as a legitimate run peer.
        let _foreign = TcpStream::connect((Ipv4Addr::LOCALHOST, ports[0])).unwrap();
        assert!(peers(ports, pid, pid).is_err());
    }
}
