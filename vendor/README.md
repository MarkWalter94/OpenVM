# Patched dependencies

Local copies of third-party crates with minimal changes, wired through the
`[patch.crates-io]` section of the workspace `Cargo.toml`. They are not members
of the workspace (`exclude = ["vendor"]`) and keep their original license.

## `egui-winit` 0.36.2 (MIT OR Apache-2.0, https://github.com/emilk/egui)

Reason: F1-13 (`docs/verification/F1-05-desktop-egui.md`). The roadmap excludes
the clipboard from the MVP as well as automatic reading of the host clipboard
(sections 4, 6.4, 7.2); eframe always enables egui-winit's `clipboard` feature
and offers no safe hook before the read.

Changes (search for `LightVM` in the sources):

1. `src/clipboard.rs`: `Clipboard::get()` always returns `None`: the host
   clipboard is never read (including `State::clipboard_text()`, used by
   eframe for `ViewportCommand::RequestPaste`). Writing (`set_text`) is
   unchanged.
2. `src/lib.rs`, `State::on_keyboard_input`: removed the branch that, on the
   paste shortcut (Ctrl+V, Shift+Insert, Paste key), read the clipboard and
   emitted `Event::Paste`; the key continues as a normal `Event::Key`, which
   the console forwards to the guest via QMP.

Origin: sources of the package published on crates.io (`Cargo.toml`,
`README.md`, `src/`), same version pinned in ADR-0014. On every egui/eframe
update the patch must be reapplied and verified again.
