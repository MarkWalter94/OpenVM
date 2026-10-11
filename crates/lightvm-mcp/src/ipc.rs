//! Multiplexed connection to the supervisor (pairing role): several
//! requests in flight on one connection (a long `snapshot` or
//! `await_session` must not block the lease renewals), replies matched by
//! `request_id`, `closing` from the service recorded as the reason.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lightvm_proto::ipc::{
    ClientMessage, CloseReason, Request, RequestId, ResponseBody, Role, ServerMessage,
};
use lightvm_proto::{PublicError, ReplyError};
use lightvm_service::credential::Credential;
use lightvm_service::frame::{self, DEFAULT_MAX_FRAME};
use lightvm_service::transport::{self, BoxedStream};
use tokio::io::WriteHalf;
use tokio::sync::oneshot;

/// Failure of a call to the supervisor.
#[derive(Debug, Clone)]
pub enum CallError {
    /// Operational error of the service (code, message, details).
    Operational(PublicError),
    /// Protocol error (message not understood).
    Protocol(String),
    /// Connection closed (revocation, service stopping, I/O).
    Closed(String),
    /// No reply in time (the request may still produce its effect: never
    /// retried blindly).
    Timeout,
}

type Waiters = Arc<Mutex<HashMap<RequestId, oneshot::Sender<ServerMessage>>>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Connection with the pairing role.
pub struct Ipc {
    wr: tokio::sync::Mutex<WriteHalf<BoxedStream>>,
    waiters: Waiters,
    next: AtomicU64,
    closed: Arc<Mutex<Option<String>>>,
    reader: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for Ipc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ipc").finish_non_exhaustive()
    }
}

impl Drop for Ipc {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Ipc {
    /// Connects and authenticates with the pairing credential.
    pub async fn connect(
        endpoint: &str,
        credential: &Credential,
        client_name: &str,
    ) -> Result<Self, CallError> {
        let stream = tokio::time::timeout(Duration::from_secs(10), transport::connect(endpoint))
            .await
            .map_err(|_| CallError::Closed("supervisor not reachable (timeout)".into()))?
            .map_err(|_| CallError::Closed("supervisor not reachable".into()))?;
        let (mut rd, mut wr) = tokio::io::split(stream);
        let hello = ClientMessage::Hello {
            request_id: 0,
            protocol_versions: vec![lightvm_proto::PROTOCOL_VERSION],
            credential: Some(credential.to_hex()),
            client_name: Some(client_name.chars().take(64).collect()),
        };
        frame::write_frame(&mut wr, &hello, DEFAULT_MAX_FRAME)
            .await
            .map_err(|_| CallError::Closed("handshake failed".into()))?;
        drop(hello);
        let reply: ServerMessage = tokio::time::timeout(
            Duration::from_secs(10),
            frame::read_frame(&mut rd, DEFAULT_MAX_FRAME),
        )
        .await
        .map_err(|_| CallError::Closed("handshake timeout".into()))?
        .map_err(|_| CallError::Closed("handshake failed".into()))?;
        match reply {
            ServerMessage::HelloOk {
                role: Role::Pairing,
                ..
            } => {}
            ServerMessage::HelloOk { .. } => {
                return Err(CallError::Closed(
                    "the credential is not a pairing credential".into(),
                ));
            }
            ServerMessage::Error {
                error: ReplyError::Operational(e),
                ..
            } => return Err(CallError::Operational(e)),
            _ => return Err(CallError::Closed("handshake refused".into())),
        }
        let waiters: Waiters = Arc::default();
        let closed: Arc<Mutex<Option<String>>> = Arc::default();
        let (w2, c2) = (waiters.clone(), closed.clone());
        let reader = tokio::spawn(async move {
            loop {
                match frame::read_frame::<_, ServerMessage>(&mut rd, DEFAULT_MAX_FRAME).await {
                    Ok(
                        m @ (ServerMessage::Response { .. }
                        | ServerMessage::Error {
                            request_id: Some(_),
                            ..
                        }),
                    ) => {
                        let id = match &m {
                            ServerMessage::Response { request_id, .. }
                            | ServerMessage::Error {
                                request_id: Some(request_id),
                                ..
                            } => *request_id,
                            _ => continue,
                        };
                        if let Some(tx) = lock(&w2).remove(&id) {
                            let _ = tx.send(m);
                        }
                    }
                    Ok(ServerMessage::Closing { reason }) => {
                        *lock(&c2) = Some(
                            match reason {
                                CloseReason::Revoked => "session revoked or ended by LightVM",
                                CloseReason::ServiceStopping => "LightVM service stopping",
                            }
                            .into(),
                        );
                    }
                    Ok(_) => {}
                    Err(_) => {
                        lock(&c2).get_or_insert_with(|| "connection closed".into());
                        // Waiting calls fail at once (senders dropped).
                        lock(&w2).clear();
                        return;
                    }
                }
            }
        });
        Ok(Self {
            wr: tokio::sync::Mutex::new(wr),
            waiters,
            next: AtomicU64::new(1),
            closed,
            reader,
        })
    }

    /// Reason the connection closed, if it did.
    pub fn closed(&self) -> Option<String> {
        lock(&self.closed).clone()
    }

    /// One request and its reply.
    pub async fn call(&self, body: Request, timeout: Duration) -> Result<ResponseBody, CallError> {
        if let Some(r) = self.closed() {
            return Err(CallError::Closed(r));
        }
        let request_id = self.next.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        lock(&self.waiters).insert(request_id, tx);
        let msg = ClientMessage::Request { request_id, body };
        let sent = {
            let mut wr = self.wr.lock().await;
            frame::write_frame(&mut *wr, &msg, DEFAULT_MAX_FRAME).await
        };
        drop(msg);
        if sent.is_err() {
            lock(&self.waiters).remove(&request_id);
            return Err(CallError::Closed(
                self.closed().unwrap_or_else(|| "connection closed".into()),
            ));
        }
        match tokio::time::timeout(timeout, rx).await {
            Err(_) => {
                lock(&self.waiters).remove(&request_id);
                Err(CallError::Timeout)
            }
            Ok(Err(_)) => Err(CallError::Closed(
                self.closed().unwrap_or_else(|| "connection closed".into()),
            )),
            Ok(Ok(ServerMessage::Response { result, .. })) => Ok(result),
            Ok(Ok(ServerMessage::Error {
                error: ReplyError::Operational(e),
                ..
            })) => Err(CallError::Operational(e)),
            Ok(Ok(ServerMessage::Error {
                error: ReplyError::Protocol(p),
                ..
            })) => Err(CallError::Protocol(p.message)),
            Ok(Ok(_)) => Err(CallError::Protocol("unexpected reply".into())),
        }
    }
}
