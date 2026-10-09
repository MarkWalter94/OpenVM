//! Named pipe Windows con ACL limitata all'utente corrente.

use std::io;

use interprocess::os::windows::named_pipe::{
    PipeListenerOptions, PipeMode, pipe_mode,
    tokio::{DuplexPipeStream, PipeListener},
};
use interprocess::os::windows::security_descriptor::SecurityDescriptor;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use widestring::U16CString;

use super::{Accepted, BoxedStream, EndpointOptions, PeerInfo};

pub(super) struct Listener {
    inner: PipeListener<pipe_mode::Bytes, pipe_mode::Bytes>,
    endpoint: String,
    own_sid: String,
}

impl std::fmt::Debug for Listener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listener")
            .field("endpoint", &self.endpoint)
            .field("own_sid", &self.own_sid)
            .finish_non_exhaustive()
    }
}

/// SID dell'utente proprietario di un processo, in forma testuale `S-1-...`.
pub(crate) fn process_user_sid(pid: u32) -> io::Result<String> {
    let mut sys = System::new();
    let p = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[p]),
        false,
        ProcessRefreshKind::nothing().with_user(UpdateKind::Always),
    );
    let sid = sys
        .process(p)
        .and_then(|pr| pr.user_id())
        .map(|u| u.to_string())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("SID del processo {pid} non disponibile"),
            )
        })?;
    validate_sid(&sid)?;
    Ok(sid)
}

/// Il SID entra in una stringa SDDL: si accetta solo la forma `S-1-` con cifre
/// e trattini, per escludere ogni iniezione nel descrittore.
fn validate_sid(sid: &str) -> io::Result<()> {
    let ok = sid.starts_with("S-1-")
        && sid.len() <= 184
        && sid[4..].bytes().all(|b| b.is_ascii_digit() || b == b'-')
        && !sid.ends_with('-');
    if ok {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "SID in formato inatteso",
        ))
    }
}

/// Descrittore SDDL della pipe:
/// - `O:<sid>`: proprietario l'utente corrente;
/// - `D:P(A;;GA;;;<sid>)`: DACL protetta (nessuna ereditarietà) con un solo
///   ACE che concede l'accesso al SID dell'utente corrente; nessun altro
///   principal, nemmeno SYSTEM o Administrators;
/// - `S:(ML;;NWNRNX;;;ME)`: etichetta obbligatoria di integrità media con
///   no-write-up, no-read-up, no-execute-up: un processo a integrità bassa (o
///   AppContainer) non può aprire la pipe.
pub(crate) fn pipe_sddl(sid: &str) -> String {
    format!("O:{sid}D:P(A;;GA;;;{sid})S:(ML;;NWNRNX;;;ME)")
}

impl Listener {
    pub(super) fn bind(_opts: &EndpointOptions) -> io::Result<Self> {
        let own_sid = process_user_sid(std::process::id())?;
        let endpoint = format!(r"\\.\pipe\lightvm-{}", super::random_suffix()?);
        let sddl = U16CString::from_str(pipe_sddl(&own_sid))
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "SDDL non valido"))?;
        let sd = SecurityDescriptor::deserialize(&sddl)?;
        // `accept_remote(false)`: PIPE_REJECT_REMOTE_CLIENTS. La prima istanza è
        // creata con FILE_FLAG_FIRST_PIPE_INSTANCE da `interprocess`: se il nome
        // esiste già la creazione fallisce invece di condividere la pipe.
        let inner = PipeListenerOptions::new()
            .path(endpoint.as_str())
            .mode(PipeMode::Bytes)
            .accept_remote(false)
            .security_descriptor(Some(sd))
            .create_tokio_duplex::<pipe_mode::Bytes>()?;
        Ok(Self {
            inner,
            endpoint,
            own_sid,
        })
    }

    pub(super) fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub(super) async fn accept(&self) -> io::Result<Accepted> {
        let stream = self.inner.accept().await?;
        let pid = match stream.client_process_id() {
            Ok(p) => p,
            Err(e) => return Ok(Accepted::Rejected(format!("PID del client: {e}"))),
        };
        match (stream.client_session_id(), stream.server_session_id()) {
            (Ok(c), Ok(s)) if c == s => {}
            (Ok(c), Ok(s)) => {
                return Ok(Accepted::Rejected(format!(
                    "sessione di logon diversa (client {c}, servizio {s})"
                )));
            }
            _ => return Ok(Accepted::Rejected("sessione non verificabile".into())),
        }
        match process_user_sid(pid) {
            Ok(sid) if sid == self.own_sid => Ok(Accepted::Ok(
                Box::new(stream),
                PeerInfo {
                    pid: Some(pid),
                    os_identity: sid,
                },
            )),
            Ok(_) => Ok(Accepted::Rejected("SID del peer diverso".into())),
            Err(e) => Ok(Accepted::Rejected(format!("SID del peer: {e}"))),
        }
    }
}

pub(super) async fn connect(endpoint: &str) -> io::Result<BoxedStream> {
    if !endpoint.starts_with(r"\\.\pipe\") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "endpoint non locale",
        ));
    }
    let s = DuplexPipeStream::<pipe_mode::Bytes>::connect_by_path(endpoint).await?;
    Ok(Box::new(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_sid_is_valid() {
        let sid = process_user_sid(std::process::id()).unwrap();
        assert!(sid.starts_with("S-1-5-"), "{sid}");
    }

    #[test]
    fn sid_validation_rejects_injection() {
        assert!(validate_sid("S-1-5-21-1-2-3-1001").is_ok());
        assert!(validate_sid("S-1-5-21)(A;;GA;;;WD").is_err());
        assert!(validate_sid("WD").is_err());
        assert!(validate_sid("S-1-").is_err());
    }

    #[test]
    fn sddl_shape() {
        assert_eq!(
            pipe_sddl("S-1-5-21-1-2-3-1001"),
            "O:S-1-5-21-1-2-3-1001D:P(A;;GA;;;S-1-5-21-1-2-3-1001)S:(ML;;NWNRNX;;;ME)"
        );
    }
}
