//! Minimal IPC client: used by the tests, the probe and, later, by the UI and
//! the MCP adapter (F5).

use std::collections::VecDeque;
use std::io;

use tokio::io::{ReadHalf, WriteHalf};

use crate::credential::Credential;
use crate::frame::{self, FrameError};
use crate::transport::{self, BoxedStream};
use lightvm_proto::ipc::{
    ClientMessage, EMERGENCY_ENDPOINT_FILE, EmergencyCommand, EmergencyStopReport, Request,
    RequestId, ResponseBody, ServerMessage, ServiceEvent,
};

/// Client connection.
pub struct Client {
    rd: ReadHalf<BoxedStream>,
    wr: WriteHalf<BoxedStream>,
    max_frame: u32,
    next_id: RequestId,
    events: VecDeque<ServiceEvent>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("next_id", &self.next_id)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Connects to the endpoint.
    pub async fn connect(endpoint: &str) -> io::Result<Self> {
        let s = transport::connect(endpoint).await?;
        let (rd, wr) = tokio::io::split(s);
        Ok(Self {
            rd,
            wr,
            max_frame: frame::DEFAULT_MAX_FRAME,
            next_id: 1,
            events: VecDeque::new(),
        })
    }

    fn take_id(&mut self) -> RequestId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// The two halves of the connection (tests that pipeline requests).
    pub fn into_parts(self) -> (ReadHalf<BoxedStream>, WriteHalf<BoxedStream>) {
        (self.rd, self.wr)
    }

    /// Sends an arbitrary message.
    pub async fn send(&mut self, m: &ClientMessage) -> io::Result<()> {
        frame::write_frame(&mut self.wr, m, u32::MAX).await
    }

    /// Sends raw bytes (for protocol tests).
    pub async fn send_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        tokio::io::AsyncWriteExt::write_all(&mut self.wr, bytes).await?;
        tokio::io::AsyncWriteExt::flush(&mut self.wr).await
    }

    /// Receives the next message from the service.
    pub async fn recv(&mut self) -> Result<ServerMessage, FrameError> {
        frame::read_frame(&mut self.rd, self.max_frame).await
    }

    /// Handshake with the given versions and the optional credential.
    pub async fn hello_with(
        &mut self,
        versions: Vec<u32>,
        credential: Option<String>,
    ) -> Result<ServerMessage, FrameError> {
        let request_id = self.take_id();
        self.send(&ClientMessage::Hello {
            request_id,
            protocol_versions: versions,
            credential,
            client_name: Some("lightvm-client".into()),
        })
        .await
        .map_err(FrameError::Io)?;
        self.recv().await
    }

    /// Handshake with version 1 and the credential.
    pub async fn hello(&mut self, credential: &Credential) -> Result<ServerMessage, FrameError> {
        self.hello_with(vec![1], Some(credential.to_hex())).await
    }

    /// Sends a request and waits for the response with the same `request_id`;
    /// events received in the meantime stay queued for [`Self::next_event`].
    pub async fn request(&mut self, body: Request) -> Result<ServerMessage, FrameError> {
        let request_id = self.take_id();
        self.send(&ClientMessage::Request { request_id, body })
            .await
            .map_err(FrameError::Io)?;
        loop {
            match self.recv().await? {
                ServerMessage::Event { event } => self.events.push_back(event),
                m @ ServerMessage::Response { request_id: r, .. } if r == request_id => {
                    return Ok(m);
                }
                m @ ServerMessage::Error {
                    request_id: Some(r),
                    ..
                } if r == request_id => return Ok(m),
                m => return Ok(m),
            }
        }
    }

    /// Next event (from the queue or the stream). Other messages are returned
    /// as a protocol error for the caller.
    pub async fn next_event(&mut self) -> Result<Result<ServiceEvent, ServerMessage>, FrameError> {
        if let Some(e) = self.events.pop_front() {
            return Ok(Ok(e));
        }
        match self.recv().await? {
            ServerMessage::Event { event } => Ok(Ok(event)),
            other => Ok(Err(other)),
        }
    }
}

/// Emergency endpoint published by the running service of a catalog
/// (`<catalog>/emergency.endpoint`, F4). Not a secret.
pub fn read_emergency_endpoint(catalog_dir: &std::path::Path) -> io::Result<String> {
    let p = catalog_dir.join(EMERGENCY_ENDPOINT_FILE);
    let meta = std::fs::symlink_metadata(&p)?;
    if !meta.is_file() || meta.len() > 512 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "emergency endpoint file not valid",
        ));
    }
    let s = std::fs::read_to_string(&p)?;
    let s = s.trim();
    if s.is_empty() || s.chars().any(char::is_control) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "emergency endpoint file not valid",
        ));
    }
    Ok(s.to_owned())
}

/// Local emergency command (F4, ROADMAP 7.3): sends the only verb of the
/// emergency endpoint and returns the report of the service (completion only
/// after the end of the processes was observed). It needs no credential and
/// does not depend on the UI nor on the supervisor thread.
pub async fn emergency_stop(endpoint: &str) -> io::Result<EmergencyStopReport> {
    let s = transport::connect(endpoint).await?;
    let (mut rd, mut wr) = tokio::io::split(s);
    frame::write_frame(&mut wr, &EmergencyCommand::EmergencyStop, 256).await?;
    let m: ServerMessage = tokio::time::timeout(
        std::time::Duration::from_secs(60),
        frame::read_frame(&mut rd, frame::DEFAULT_MAX_FRAME),
    )
    .await
    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "no reply from the service"))?
    .map_err(|e| io::Error::other(e.to_string()))?;
    match m {
        ServerMessage::Response {
            result: ResponseBody::EmergencyStop(r),
            ..
        } => Ok(r),
        other => Err(io::Error::other(format!("unexpected reply: {other:?}"))),
    }
}
