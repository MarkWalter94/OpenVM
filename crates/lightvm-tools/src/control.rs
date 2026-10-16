//! Run-owned host-to-guest RPC. The guest connects out through the isolated
//! Tools endpoint. Guest replies are bounded and treated as untrusted data.

use std::io;
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::policy::{Feature, Gate, Permit, Secret};
use crate::protocol::{self, FileRequest, Hello, invalid};

pub const RPC_TIMEOUT: Duration = Duration::from_secs(3);
pub const MAX_TREE_NODES: usize = 1000;
pub const MAX_TREE_DEPTH: u32 = 32;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    ClipboardRead,
    ClipboardWrite {
        text: String,
    },
    UiTree {
        max_depth: u32,
        max_nodes: u32,
    },
    UiInvoke {
        tree: u64,
        element: u32,
        action: UiAction,
        value: Option<String>,
    },
    File {
        request: FileRequest,
    },
    Flush,
}

pub use lightvm_proto::{ToolReply as Reply, ToolUiAction as UiAction, ToolUiNode as UiNode};

impl Command {
    pub fn feature(&self) -> Feature {
        match self {
            Self::ClipboardRead | Self::ClipboardWrite { .. } => Feature::Clipboard,
            Self::UiTree { .. } => Feature::UiInspect,
            Self::UiInvoke { .. } => Feature::UiAct,
            Self::File { .. } => Feature::FilesTransfer,
            Self::Flush => Feature::FilesTransfer,
        }
    }
    pub fn validate(&self) -> io::Result<()> {
        match self {
            Self::ClipboardWrite { text } if text.len() > 64 * 1024 || text.contains('\0') => {
                Err(invalid("clipboard text out of bounds"))
            }
            Self::UiTree {
                max_depth,
                max_nodes,
            } if *max_depth == 0
                || *max_depth > MAX_TREE_DEPTH
                || *max_nodes == 0
                || *max_nodes as usize > MAX_TREE_NODES =>
            {
                Err(invalid("tree out of bounds"))
            }
            Self::UiInvoke {
                value: Some(value), ..
            } if value.len() > 16 * 1024
                || value
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\t')) =>
            {
                Err(invalid("UI value out of bounds"))
            }
            Self::File { request } => request.validate_guest(),
            _ => Ok(()),
        }
    }
}

impl From<lightvm_proto::ToolCall> for Command {
    fn from(call: lightvm_proto::ToolCall) -> Self {
        match call {
            lightvm_proto::ToolCall::ClipboardRead => Self::ClipboardRead,
            lightvm_proto::ToolCall::ClipboardWrite { text } => Self::ClipboardWrite { text },
            lightvm_proto::ToolCall::UiTree {
                max_depth,
                max_nodes,
            } => Self::UiTree {
                max_depth,
                max_nodes,
            },
            lightvm_proto::ToolCall::UiInvoke {
                tree,
                element,
                action,
                value,
            } => Self::UiInvoke {
                tree,
                element,
                action,
                value,
            },
            lightvm_proto::ToolCall::File { request } => Self::File { request },
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: u64,
    pub command: Command,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub id: u64,
    pub reply: Reply,
}

#[derive(Debug)]
struct Pending {
    request: Request,
    permit: Permit,
    deadline: Instant,
    response: mpsc::SyncSender<io::Result<Reply>>,
}

#[derive(Debug, Default)]
struct State {
    stopped: AtomicBool,
    connected: AtomicBool,
    stream: Mutex<Option<TcpStream>>,
}

#[derive(Clone, Debug)]
pub struct Close {
    state: Arc<State>,
}
impl Close {
    pub fn disconnect(&self) {
        self.state.connected.store(false, Ordering::SeqCst);
        if let Some(s) = self
            .state
            .stream
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
    pub fn close(&self) {
        self.state.stopped.store(true, Ordering::SeqCst);
        if let Some(s) = self
            .state
            .stream
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

#[derive(Debug)]
pub struct ControlServer {
    address: SocketAddr,
    close: Close,
    gate: Gate,
    sequence: AtomicU64,
    queue: mpsc::SyncSender<Pending>,
    worker: Option<JoinHandle<()>>,
}

impl ControlServer {
    pub fn start(vm: String, run: String, secret: Secret, gate: Gate) -> io::Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let state = Arc::new(State::default());
        let close = Close {
            state: state.clone(),
        };
        let (tx, rx) = mpsc::sync_channel::<Pending>(16);
        let worker = std::thread::Builder::new()
            .name("lightvm-tools-control".into())
            .spawn(move || {
                while !state.stopped.load(Ordering::SeqCst) {
                    let (mut stream, _) = match listener.accept() {
                        Ok(s) => s,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                            continue;
                        }
                        Err(_) => break,
                    };
                    let connected = (|| -> io::Result<()> {
                        stream.set_nonblocking(false)?;
                        stream.set_nodelay(true)?;
                        stream.set_read_timeout(Some(RPC_TIMEOUT))?;
                        stream.set_write_timeout(Some(RPC_TIMEOUT))?;
                        *state.stream.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some(stream.try_clone()?);
                        if state.stopped.load(Ordering::SeqCst) {
                            return Ok(());
                        }
                        let hello = protocol::read::<_, Hello>(&mut stream)?
                            .ok_or_else(|| invalid("hello required"))?;
                        if !secret.authenticate(&hello, &vm, &run) {
                            return Err(io::ErrorKind::PermissionDenied.into());
                        }
                        drop(hello);
                        protocol::write(
                            &mut stream,
                            &crate::server::Welcome {
                                version: protocol::VERSION,
                            },
                        )?;
                        state.connected.store(true, Ordering::SeqCst);
                        while !state.stopped.load(Ordering::SeqCst) {
                            let p = match rx.recv_timeout(Duration::from_millis(50)) {
                                Ok(p) => p,
                                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                                Err(_) => break,
                            };
                            let result = p.permit.with(p.request.command.feature(), None, || {
                                if Instant::now() >= p.deadline {
                                    return Err(io::ErrorKind::TimedOut.into());
                                }
                                protocol::write(&mut stream, &p.request)?;
                                let response: Response = protocol::read(&mut stream)?
                                    .ok_or_else(|| invalid("guest disconnected"))?;
                                if response.id != p.request.id {
                                    return Err(invalid("wrong reply id"));
                                }
                                response.reply.validate()?;
                                if !matches!(p.request.command, Command::Flush) {
                                    response.reply.validate_for(
                                        &p.request.command.as_call().expect("non-flush command"),
                                    )?;
                                }
                                Ok(response.reply)
                            });
                            let broken = result
                                .as_ref()
                                .is_err_and(|e| e.kind() != io::ErrorKind::PermissionDenied);
                            let _ = p.response.send(result);
                            if broken {
                                break;
                            }
                        }
                        Ok(())
                    })();
                    drop(connected);
                    state.connected.store(false, Ordering::SeqCst);
                    state
                        .stream
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take();
                    // Queued requests belong to the old connection and must never
                    // be replayed after reconnect or a guest rollback.
                    while let Ok(p) = rx.try_recv() {
                        let _ = p.response.send(Err(io::ErrorKind::NotConnected.into()));
                    }
                }
            })?;
        Ok(Self {
            address,
            close,
            gate,
            sequence: AtomicU64::new(1),
            queue: tx,
            worker: Some(worker),
        })
    }
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn close_handle(&self) -> Close {
        self.close.clone()
    }
    pub fn connected(&self) -> bool {
        self.close.state.connected.load(Ordering::SeqCst)
    }
    pub fn call(&self, command: Command) -> io::Result<Reply> {
        command.validate()?;
        if !self.connected() || self.close.state.stopped.load(Ordering::SeqCst) {
            return Err(io::ErrorKind::NotConnected.into());
        }
        let permit = self.gate.permit();
        permit.with(command.feature(), None, || Ok(()))?;
        let (tx, rx) = mpsc::sync_channel(1);
        let request = Request {
            id: self.sequence.fetch_add(1, Ordering::SeqCst),
            command,
        };
        self.queue
            .try_send(Pending {
                request,
                permit,
                deadline: Instant::now() + RPC_TIMEOUT,
                response: tx,
            })
            .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
        rx.recv_timeout(RPC_TIMEOUT + Duration::from_millis(100))
            .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
    }
    pub fn stop(&mut self) {
        self.close.close();
        if let Some(w) = self.worker.take() {
            let _ = w.join();
        }
    }
}
impl Drop for ControlServer {
    fn drop(&mut self) {
        self.stop();
    }
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::File { request } => f.debug_tuple("File").field(request).finish(),
            _ => f.write_str(match self {
                Self::ClipboardRead => "ClipboardRead",
                Self::ClipboardWrite { .. } => "ClipboardWrite(<redacted>)",
                Self::UiTree { .. } => "UiTree",
                Self::UiInvoke { .. } => "UiInvoke(<redacted>)",
                Self::Flush => "Flush",
                Self::File { .. } => unreachable!(),
            }),
        }
    }
}
impl Command {
    fn as_call(&self) -> Option<lightvm_proto::ToolCall> {
        use lightvm_proto::ToolCall as T;
        Some(match self.clone() {
            Command::ClipboardRead => T::ClipboardRead,
            Command::ClipboardWrite { text } => T::ClipboardWrite { text },
            Command::UiTree {
                max_depth,
                max_nodes,
            } => T::UiTree {
                max_depth,
                max_nodes,
            },
            Command::UiInvoke {
                tree,
                element,
                action,
                value,
            } => T::UiInvoke {
                tree,
                element,
                action,
                value,
            },
            Command::File { request } => T::File { request },
            Command::Flush => return None,
        })
    }
}
