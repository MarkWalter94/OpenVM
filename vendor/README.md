# Dipendenze patchate

Copie locali di crate di terze parti con modifiche minime, collegate dal
`[patch.crates-io]` del `Cargo.toml` del workspace. Non sono membri del
workspace (`exclude = ["vendor"]`) e mantengono la licenza originale.

## `egui-winit` 0.36.2 (MIT OR Apache-2.0, https://github.com/emilk/egui)

Motivo: F1-13 (`docs/verification/F1-05-desktop-egui.md`). La roadmap esclude
la clipboard dall'MVP e la lettura automatica degli appunti dell'host
(sezioni 4, 6.4, 7.2); eframe abilita sempre la feature `clipboard` di
egui-winit e non offre un gancio sicuro prima della lettura.

Modifiche (cercare `LightVM` nei sorgenti):

1. `src/clipboard.rs`: `Clipboard::get()` restituisce sempre `None`: gli appunti
   dell'host non vengono mai letti (anche `State::clipboard_text()`, usata da
   eframe per `ViewportCommand::RequestPaste`). La scrittura (`set_text`) è
   invariata.
2. `src/lib.rs`, `State::on_keyboard_input`: rimosso il ramo che, sulla
   combinazione di incolla (Ctrl+V, Shift+Insert, tasto Paste), leggeva gli
   appunti ed emetteva `Event::Paste`; il tasto prosegue come normale
   `Event::Key`, che la console inoltra al guest via QMP.

Origine: sorgenti del pacchetto pubblicato su crates.io (`Cargo.toml`,
`README.md`, `src/`), stessa versione fissata in ADR-0014. A ogni
aggiornamento di egui/eframe la patch va riapplicata e riverificata.
