//! Explicit opt-in text synchronization, limited to the focused console.
//! A guest reply never overwrites a host copy made while that read was pending.
use std::time::{Duration, Instant};

use lightvm_proto::{RunId, ToolCall, ToolReply, VmId};
use sha2::{Digest, Sha256};

use crate::service::{RequestStatus, ServiceCommand, ServiceState};

/// Implementations must not read the host clipboard before `text` is called.
pub trait ClipboardText {
    fn text(&mut self) -> Option<String>;
    fn set_text(&mut self, text: String) -> bool;
}

#[derive(Default)]
struct Native {
    clipboard: Option<arboard::Clipboard>,
}
impl Native {
    fn get(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        self.clipboard.as_mut()
    }
}
impl ClipboardText for Native {
    fn text(&mut self) -> Option<String> {
        self.get()?.get_text().ok()
    }
    fn set_text(&mut self, text: String) -> bool {
        self.get().is_some_and(|c| c.set_text(text).is_ok())
    }
}

type Fingerprint = [u8; 32];
fn fingerprint(text: &str) -> Fingerprint {
    Sha256::digest(text.as_bytes()).into()
}
fn bounded(text: &str) -> bool {
    text.len() <= 64 * 1024
        && !text.contains('\0')
        && serde_json::to_vec(&ToolCall::ClipboardWrite { text: text.into() })
            .is_ok_and(|b| b.len() < 48 * 1024)
}

enum Pending {
    Write(Fingerprint),
    Read(Fingerprint),
}

/// The switch defaults to off; payloads are not retained in synchronization
/// history. One Tools reply slot is held until completion, even after focus loss.
pub struct ClipboardSync {
    pub enabled: bool,
    pub error: Option<String>,
    target: Option<(VmId, RunId)>,
    pending: Option<Pending>,
    discard: bool,
    host: Option<Fingerprint>,
    guest: Option<Fingerprint>,
    next: Instant,
    clipboard: Box<dyn ClipboardText>,
}
impl std::fmt::Debug for ClipboardSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardSync")
            .field("enabled", &self.enabled)
            .field("busy", &self.busy())
            .finish_non_exhaustive()
    }
}
impl Default for ClipboardSync {
    fn default() -> Self {
        Self::new(Box::<Native>::default())
    }
}
impl ClipboardSync {
    pub fn new(clipboard: Box<dyn ClipboardText>) -> Self {
        Self {
            enabled: false,
            error: None,
            target: None,
            pending: None,
            discard: false,
            host: None,
            guest: None,
            next: Instant::now(),
            clipboard,
        }
    }
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// `active` includes viewport focus, console capture, user input ownership
    /// and current VM permissions. `blocked` reserves the slot for other Tools.
    pub fn poll(
        &mut self,
        state: Option<&ServiceState>,
        target: Option<(VmId, RunId)>,
        active: bool,
        blocked: bool,
        now: Instant,
    ) -> Option<ServiceCommand> {
        if self.target != target && self.target.is_some() {
            self.enabled = false;
        }
        if !active || !self.enabled || self.target != target {
            self.discard = true;
        }
        if self.pending.is_some() {
            let status = state
                .and_then(|s| s.tools.as_ref())
                .filter(|(v, r, _)| Some((*v, *r)) == self.target)
                .map(|(_, _, status)| status);
            let result = match status {
                Some(RequestStatus::Done(reply)) => Some(Ok(reply.clone())),
                Some(RequestStatus::Failed(f)) => Some(Err(f.message.clone())),
                _ if self.target != target => Some(Err("Console run changed".into())),
                _ if !state.is_some_and(|s| s.connected) => {
                    Some(Err("Supervisor disconnected".into()))
                }
                _ => None,
            };
            if let Some(result) = result {
                let pending = self.pending.take().expect("pending Tools operation");
                if !self.discard {
                    match (pending, result) {
                        (Pending::Write(hash), Ok(ToolReply::Ok)) => {
                            self.host = Some(hash);
                            self.guest = Some(hash);
                        }
                        (Pending::Read(before), Ok(ToolReply::Clipboard { text }))
                            if bounded(&text) =>
                        {
                            let guest = fingerprint(&text);
                            let unchanged_host = self
                                .clipboard
                                .text()
                                .is_some_and(|current| fingerprint(&current) == before);
                            if unchanged_host && self.guest != Some(guest) && guest != before {
                                if self.clipboard.set_text(text) {
                                    self.host = Some(guest);
                                } else {
                                    self.enabled = false;
                                    self.error = Some("Host clipboard is unavailable".into());
                                }
                            }
                            self.guest = Some(guest);
                        }
                        (_, Err(error)) => {
                            self.error = Some(error);
                            self.enabled = false;
                        }
                        _ => {
                            self.error = Some(
                                "Clipboard text is unavailable or exceeds the channel limit".into(),
                            );
                            self.enabled = false;
                        }
                    }
                }
                self.next = now + Duration::from_millis(500);
            }
            return None;
        }
        if self.target != target {
            self.target = target;
            self.host = None;
            self.guest = None;
        }
        if !self.enabled || !active || blocked || now < self.next {
            return None;
        }
        let (vm_id, run_id) = target?;
        self.next = now + Duration::from_millis(500);
        let text = self.clipboard.text()?;
        if !bounded(&text) {
            self.next = now + Duration::from_millis(500);
            return None;
        }
        let host = fingerprint(&text);
        let call = if self.host != Some(host) {
            self.pending = Some(Pending::Write(host));
            ToolCall::ClipboardWrite { text }
        } else {
            self.pending = Some(Pending::Read(host));
            ToolCall::ClipboardRead
        };
        self.discard = false;
        Some(ServiceCommand::GuestTools {
            vm_id,
            run_id,
            call,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Default)]
    struct Host {
        text: String,
        reads: usize,
        writes: usize,
    }
    struct Fake(Rc<RefCell<Host>>);
    impl ClipboardText for Fake {
        fn text(&mut self) -> Option<String> {
            let mut h = self.0.borrow_mut();
            h.reads += 1;
            Some(h.text.clone())
        }
        fn set_text(&mut self, text: String) -> bool {
            let mut h = self.0.borrow_mut();
            h.text = text;
            h.writes += 1;
            true
        }
    }
    fn target() -> (VmId, RunId) {
        (
            VmId::from_unix_millis(1, [1; 10]),
            RunId::from_unix_millis(1, [2; 10]),
        )
    }
    fn setup() -> (ClipboardSync, Rc<RefCell<Host>>, ServiceState, Instant) {
        let host = Rc::new(RefCell::new(Host {
            text: "host text".into(),
            ..Default::default()
        }));
        let sync = ClipboardSync::new(Box::new(Fake(host.clone())));
        (
            sync,
            host,
            ServiceState {
                connected: true,
                ..Default::default()
            },
            Instant::now(),
        )
    }
    fn reply(state: &mut ServiceState, value: ToolReply) {
        let (vm, run) = target();
        state.tools = Some((vm, run, RequestStatus::Done(value)));
    }
    fn seed(sync: &mut ClipboardSync, state: &mut ServiceState, now: Instant) {
        sync.enabled = true;
        assert!(matches!(
            sync.poll(Some(state), Some(target()), true, false, now),
            Some(ServiceCommand::GuestTools {
                call: ToolCall::ClipboardWrite { .. },
                ..
            })
        ));
        reply(state, ToolReply::Ok);
        sync.poll(Some(state), Some(target()), true, false, now);
    }

    #[test]
    fn no_host_read_without_opt_in_focus_and_available_slot() {
        let (mut sync, host, state, now) = setup();
        assert!(
            sync.poll(Some(&state), Some(target()), true, false, now)
                .is_none()
        );
        sync.enabled = true;
        assert!(
            sync.poll(Some(&state), Some(target()), false, false, now)
                .is_none()
        );
        assert!(
            sync.poll(Some(&state), Some(target()), true, true, now)
                .is_none()
        );
        assert_eq!(host.borrow().reads, 0);
    }

    #[test]
    fn guest_change_is_received_without_a_clipboard_echo() {
        let (mut sync, host, mut state, now) = setup();
        seed(&mut sync, &mut state, now);
        let now = now + Duration::from_secs(1);
        assert!(matches!(
            sync.poll(Some(&state), Some(target()), true, false, now),
            Some(ServiceCommand::GuestTools {
                call: ToolCall::ClipboardRead,
                ..
            })
        ));
        reply(
            &mut state,
            ToolReply::Clipboard {
                text: "guest text".into(),
            },
        );
        sync.poll(Some(&state), Some(target()), true, false, now);
        assert_eq!(host.borrow().text, "guest text");
        assert_eq!(host.borrow().writes, 1);
        assert!(matches!(
            sync.poll(
                Some(&state),
                Some(target()),
                true,
                false,
                now + Duration::from_secs(1)
            ),
            Some(ServiceCommand::GuestTools {
                call: ToolCall::ClipboardRead,
                ..
            })
        ));
    }

    #[test]
    fn pending_guest_reply_cannot_replace_a_new_host_copy() {
        let (mut sync, host, mut state, now) = setup();
        seed(&mut sync, &mut state, now);
        let now = now + Duration::from_secs(1);
        sync.poll(Some(&state), Some(target()), true, false, now);
        host.borrow_mut().text = "new host copy".into();
        reply(
            &mut state,
            ToolReply::Clipboard {
                text: "older guest copy".into(),
            },
        );
        sync.poll(Some(&state), Some(target()), true, false, now);
        assert_eq!(host.borrow().text, "new host copy");
        assert_eq!(host.borrow().writes, 0);
        assert!(
            matches!(sync.poll(Some(&state), Some(target()), true, false, now + Duration::from_secs(1)), Some(ServiceCommand::GuestTools { call: ToolCall::ClipboardWrite { text }, .. }) if text == "new host copy")
        );
    }

    #[test]
    fn focus_loss_discards_the_in_flight_guest_text() {
        let (mut sync, host, mut state, now) = setup();
        seed(&mut sync, &mut state, now);
        let now = now + Duration::from_secs(1);
        sync.poll(Some(&state), Some(target()), true, false, now);
        state.tools = Some((target().0, target().1, RequestStatus::Pending));
        sync.poll(Some(&state), Some(target()), false, false, now);
        reply(
            &mut state,
            ToolReply::Clipboard {
                text: "discard me".into(),
            },
        );
        sync.poll(Some(&state), Some(target()), true, false, now);
        assert_eq!(host.borrow().writes, 0);
        assert!(!sync.busy());
    }

    #[test]
    fn another_run_requires_a_new_opt_in() {
        let (mut sync, host, mut state, now) = setup();
        seed(&mut sync, &mut state, now);
        let next = (target().0, RunId::from_unix_millis(2, [3; 10]));
        assert!(
            sync.poll(
                Some(&state),
                Some(next),
                true,
                false,
                now + Duration::from_secs(1)
            )
            .is_none()
        );
        assert!(!sync.enabled);
        assert_eq!(host.borrow().writes, 0);
    }

    #[test]
    fn encoded_text_must_fit_the_ipc_frame() {
        assert!(bounded("short text"));
        assert!(!bounded(&"\u{1}".repeat(10000)));
        assert!(!bounded("nul\0text"));
    }
}
