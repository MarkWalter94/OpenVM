//! F1-13: no host clipboard text reaches the UI or the guest.
//!
//! ## Where the read happens
//!
//! In egui-winit 0.36.2 `State::on_keyboard_input` recognises the paste
//! combination (`Ctrl+V`, `Shift+Insert` on Windows, `Paste` key) and
//! immediately calls `Clipboard::get()` (arboard on Windows,
//! smithay-clipboard/arboard on Linux), then queues
//! `egui::Event::Paste(text)` **instead of** the `Key` event. Also
//! `ViewportCommand::RequestPaste` reads the clipboard inside eframe; LightVM
//! never sends that command.
//!
//! ## Why it is not blocked upstream from here
//!
//! - eframe 0.36.2 declares `egui-winit` with `features = ["clipboard"]` as a
//!   non-optional dependency on native targets: the feature cannot be
//!   disabled from the app's `Cargo.toml`.
//! - eframe offers no hook on winit events before egui-winit; the only
//!   earlier hook is winit's Win32 `msg_hook`, which receives a
//!   `*const MSG` and requires `unsafe` code (forbidden in the app).
//!
//! Since 2026-10-10 the read is removed upstream with a patched copy of
//! egui-winit (`vendor/egui-winit`, `vendor/README.md`): this module remains
//! as a second defence.
//!
//! Blocking upstream therefore remains an action outside the app's perimeter
//! (see `docs/verification/F1-05-desktop-egui.md`). This module applies the
//! block as far upstream as possible **inside** the app: in
//! `App::raw_input_hook`, before the egui context processes the input, and
//! again at the start of each UI pass (same function, idempotent). Each
//! `Paste` is:
//!
//! 1. emptied, overwriting the received buffer with zeros;
//! 2. replaced by the press and release of the physical key that generated
//!    it (`V` or `Insert`), so the console forwards the key combination to the
//!    guest via QMP (the modifiers are already pressed) and no widget receives
//!    text.

/// Outcome of the filter on an input pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FilterOutcome {
    /// `Paste` events discarded.
    pub discarded: u32,
    /// Text bytes zeroed before the memory was released.
    pub zeroed_bytes: usize,
    /// `Copy`/`Cut` events replaced by their keys (F2: the VM name field
    /// must not write to the host clipboard either).
    pub copies: u32,
}

/// Physical key that produced the paste, reconstructed from the modifiers.
///
/// `Shift+Insert` (Windows only in egui-winit) has no Ctrl/Command pressed;
/// in every other case the combination is `Ctrl/Command+V`.
pub fn paste_key(modifiers: egui::Modifiers) -> egui::Key {
    if modifiers.shift && !modifiers.command && !modifiers.ctrl {
        egui::Key::Insert
    } else {
        egui::Key::V
    }
}

/// Replaces every `Event::Paste` with press and release of the corresponding
/// key. The text is zeroed and discarded.
///
/// `modifiers` is the modifier state at the start of the sequence; it is
/// updated by the events that report it (`ModifiersChanged`, emitted by
/// egui-winit, and `Key`, `PointerButton`, `MouseWheel`), because
/// `egui::RawInput` does not contain the modifier state.
pub fn discard_paste(events: &mut Vec<egui::Event>, modifiers: egui::Modifiers) -> FilterOutcome {
    let mut m = modifiers;
    discard_paste_with_state(events, &mut m)
}

fn event_modifiers(ev: &egui::Event) -> Option<egui::Modifiers> {
    match ev {
        egui::Event::Key { modifiers, .. }
        | egui::Event::PointerButton { modifiers, .. }
        | egui::Event::MouseWheel { modifiers, .. } => Some(*modifiers),
        egui::Event::ModifiersChanged(m) => Some(*m),
        _ => None,
    }
}

fn discard_paste_with_state(
    events: &mut Vec<egui::Event>,
    modifiers: &mut egui::Modifiers,
) -> FilterOutcome {
    let mut outcome = FilterOutcome::default();
    if !events.iter().any(|e| {
        matches!(
            e,
            egui::Event::Paste(_) | egui::Event::Copy | egui::Event::Cut
        )
    }) {
        if let Some(m) = events.iter().rev().find_map(event_modifiers) {
            *modifiers = m;
        }
        return outcome;
    }
    let mut out = Vec::with_capacity(events.len() + 2);
    for ev in events.drain(..) {
        if let Some(m) = event_modifiers(&ev) {
            *modifiers = m;
        }
        let modifiers = *modifiers;
        match ev {
            egui::Event::Paste(text) => {
                outcome.discarded += 1;
                outcome.zeroed_bytes += zeroize(text);
                let key = paste_key(modifiers);
                for pressed in [true, false] {
                    out.push(egui::Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed,
                        repeat: false,
                        modifiers,
                    });
                }
            }
            egui::Event::Copy | egui::Event::Cut => {
                outcome.copies += 1;
                let key = if matches!(ev, egui::Event::Copy) {
                    egui::Key::C
                } else {
                    egui::Key::X
                };
                for pressed in [true, false] {
                    out.push(egui::Event::Key {
                        key,
                        physical_key: Some(key),
                        pressed,
                        repeat: false,
                        modifiers,
                    });
                }
            }
            other => out.push(other),
        }
    }
    *events = out;
    outcome
}

/// Filter remembering the modifiers from one pass to the next, for eframe's
/// `raw_input_hook` (the raw input does not report the modifier state: Ctrl
/// pressed in a previous frame stays known here).
#[derive(Clone, Copy, Debug, Default)]
pub struct ClipboardFilter {
    modifiers: egui::Modifiers,
}

impl ClipboardFilter {
    /// Applies the filter to the raw input.
    pub fn filter_raw_input(&mut self, raw: &mut egui::RawInput) -> FilterOutcome {
        if !raw.focused {
            // When focus is lost the modifiers are no longer reliable.
            self.modifiers = egui::Modifiers::NONE;
        }
        discard_paste_with_state(&mut raw.events, &mut self.modifiers)
    }
}

/// Applies the filter to the input of the current pass, before any widget.
/// Also needed when the UI is driven without eframe (egui_kittest).
pub fn filter_context(ctx: &egui::Context) -> FilterOutcome {
    ctx.input_mut(|i| {
        let modifiers = i.modifiers;
        discard_paste(&mut i.events, modifiers)
    })
}

/// Removes from the output of the frame any request to write the host
/// clipboard (`CopyText`, `CopyImage`): no widget of LightVM copies to the
/// clipboard (ROADMAP: no clipboard features). Returns how many were removed.
pub fn strip_clipboard_output(ctx: &egui::Context) -> usize {
    ctx.output_mut(|o| {
        let before = o.commands.len();
        o.commands.retain(|c| {
            !matches!(
                c,
                egui::OutputCommand::CopyText(_) | egui::OutputCommand::CopyImage(_)
            )
        });
        before - o.commands.len()
    })
}

/// Overwrites the bytes of the string with zeros and releases it. Returns the
/// number of zeroed bytes. Copies made by egui-winit before the filter
/// (`replace("\r\n", "\n")`) are not reachable from here.
fn zeroize(text: String) -> usize {
    let mut bytes = text.into_bytes();
    let n = bytes.len();
    bytes.fill(0);
    std::hint::black_box(&bytes);
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctrl() -> egui::Modifiers {
        egui::Modifiers {
            ctrl: true,
            command: !cfg!(target_os = "macos"),
            ..Default::default()
        }
    }

    #[test]
    fn paste_becomes_v_key_without_text() {
        let mut ev = vec![
            egui::Event::Text("a".into()),
            egui::Event::Paste("secret!".into()),
        ];
        let outcome = discard_paste(&mut ev, ctrl());
        assert_eq!(
            outcome,
            FilterOutcome {
                discarded: 1,
                zeroed_bytes: 7,
                copies: 0
            }
        );
        assert_eq!(ev.len(), 3);
        assert!(matches!(&ev[0], egui::Event::Text(t) if t == "a"));
        assert!(!ev.iter().any(|e| matches!(e, egui::Event::Paste(_))));
        assert!(matches!(
            ev[1],
            egui::Event::Key {
                key: egui::Key::V,
                physical_key: Some(egui::Key::V),
                pressed: true,
                ..
            }
        ));
        assert!(matches!(
            ev[2],
            egui::Event::Key {
                key: egui::Key::V,
                pressed: false,
                ..
            }
        ));
    }

    #[test]
    fn shift_insert_becomes_insert() {
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(paste_key(shift), egui::Key::Insert);
        assert_eq!(paste_key(ctrl()), egui::Key::V);
    }

    #[test]
    fn without_clipboard_events_nothing_changes() {
        let mut ev = vec![egui::Event::Text("x".into())];
        assert_eq!(discard_paste(&mut ev, ctrl()), FilterOutcome::default());
        assert_eq!(ev.len(), 1);
    }

    #[test]
    fn copy_and_cut_become_keys() {
        let mut ev = vec![egui::Event::Copy, egui::Event::Cut];
        let o = discard_paste(&mut ev, ctrl());
        assert_eq!(o.copies, 2);
        assert_eq!(ev.len(), 4);
        assert!(
            !ev.iter()
                .any(|e| matches!(e, egui::Event::Copy | egui::Event::Cut))
        );
        assert!(matches!(
            ev[0],
            egui::Event::Key {
                key: egui::Key::C,
                pressed: true,
                ..
            }
        ));
    }
}
