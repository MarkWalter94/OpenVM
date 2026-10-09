//! F1-13: nessun testo degli appunti host raggiunge la UI o il guest.
//!
//! ## Dove avviene la lettura
//!
//! In egui-winit 0.36.2 `State::on_keyboard_input` riconosce la combinazione
//! di incolla (`Ctrl+V`, `Shift+Insert` su Windows, tasto `Paste`) e chiama
//! subito `Clipboard::get()` (arboard su Windows, smithay-clipboard/arboard su
//! Linux), poi accoda `egui::Event::Paste(testo)` **al posto** dell'evento
//! `Key`. Anche `ViewportCommand::RequestPaste` legge gli appunti dentro eframe;
//! LightVM non invia mai quel comando.
//!
//! ## Perché non si blocca a monte da qui
//!
//! - eframe 0.36.2 dichiara `egui-winit` con `features = ["clipboard"]` come
//!   dipendenza non opzionale sui target nativi: la feature non si disattiva
//!   dal `Cargo.toml` dell'app.
//! - eframe non offre un gancio sugli eventi winit prima di egui-winit;
//!   l'unico gancio precedente è il `msg_hook` Win32 di winit, che riceve un
//!   `*const MSG` e richiede codice `unsafe` (vietato nell'app).
//!
//! Il blocco a monte resta quindi un'azione fuori dal perimetro dell'app
//! (vedi `docs/verification/F1-05-desktop-egui.md`). Questo modulo applica il
//! blocco più a monte possibile **dentro** l'app: in `App::raw_input_hook`,
//! prima che il contesto egui elabori l'input, e di nuovo all'inizio di ogni
//! passaggio UI (stessa funzione, idempotente). Ogni `Paste` viene:
//!
//! 1. svuotato, sovrascrivendo con zeri il buffer ricevuto;
//! 2. sostituito dalla pressione e dal rilascio del tasto fisico che l'ha
//!    generato (`V` o `Insert`), così la console inoltra al guest la
//!    combinazione di tasti via QMP (i modificatori sono già premuti) e nessun
//!    widget riceve testo.

/// Esito del filtro su un passaggio di input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EsitoFiltro {
    /// Eventi `Paste` scartati.
    pub scartati: u32,
    /// Byte di testo azzerati prima del rilascio della memoria.
    pub byte_azzerati: usize,
}

/// Tasto fisico che ha prodotto l'incolla, ricostruito dai modificatori.
///
/// `Shift+Insert` (solo Windows in egui-winit) non ha Ctrl/Command premuto;
/// in tutti gli altri casi la combinazione è `Ctrl/Command+V`.
pub fn tasto_incolla(modifiers: egui::Modifiers) -> egui::Key {
    if modifiers.shift && !modifiers.command && !modifiers.ctrl {
        egui::Key::Insert
    } else {
        egui::Key::V
    }
}

/// Sostituisce ogni `Event::Paste` con pressione e rilascio del tasto
/// corrispondente. Il testo viene azzerato e scartato.
///
/// `modifiers` è lo stato dei modificatori all'inizio della sequenza; viene
/// aggiornato dagli eventi che lo riportano (`ModifiersChanged`, emesso da
/// egui-winit, e `Key`, `PointerButton`, `MouseWheel`), perché
/// `egui::RawInput` non contiene lo stato dei modificatori.
pub fn scarta_incolla(events: &mut Vec<egui::Event>, modifiers: egui::Modifiers) -> EsitoFiltro {
    let mut m = modifiers;
    scarta_incolla_con_stato(events, &mut m)
}

fn modificatori_evento(ev: &egui::Event) -> Option<egui::Modifiers> {
    match ev {
        egui::Event::Key { modifiers, .. }
        | egui::Event::PointerButton { modifiers, .. }
        | egui::Event::MouseWheel { modifiers, .. } => Some(*modifiers),
        egui::Event::ModifiersChanged(m) => Some(*m),
        _ => None,
    }
}

fn scarta_incolla_con_stato(
    events: &mut Vec<egui::Event>,
    modifiers: &mut egui::Modifiers,
) -> EsitoFiltro {
    let mut esito = EsitoFiltro::default();
    if !events.iter().any(|e| matches!(e, egui::Event::Paste(_))) {
        if let Some(m) = events.iter().rev().find_map(modificatori_evento) {
            *modifiers = m;
        }
        return esito;
    }
    let mut out = Vec::with_capacity(events.len() + 2);
    for ev in events.drain(..) {
        if let Some(m) = modificatori_evento(&ev) {
            *modifiers = m;
        }
        let modifiers = *modifiers;
        match ev {
            egui::Event::Paste(testo) => {
                esito.scartati += 1;
                esito.byte_azzerati += azzera(testo);
                let key = tasto_incolla(modifiers);
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
            altro => out.push(altro),
        }
    }
    *events = out;
    esito
}

/// Filtro con memoria dei modificatori tra un passaggio e l'altro, per il
/// gancio `raw_input_hook` di eframe (l'input grezzo non riporta lo stato dei
/// modificatori: Ctrl premuto in un frame precedente resta noto qui).
#[derive(Clone, Copy, Debug, Default)]
pub struct FiltroAppunti {
    modificatori: egui::Modifiers,
}

impl FiltroAppunti {
    /// Applica il filtro all'input grezzo.
    pub fn filtra_raw_input(&mut self, raw: &mut egui::RawInput) -> EsitoFiltro {
        if !raw.focused {
            // Alla perdita del focus i modificatori non sono più affidabili.
            self.modificatori = egui::Modifiers::NONE;
        }
        scarta_incolla_con_stato(&mut raw.events, &mut self.modificatori)
    }
}

/// Applica il filtro all'input del passaggio corrente, prima di qualsiasi
/// widget. Serve anche quando la UI è pilotata senza eframe (egui_kittest).
pub fn filtra_contesto(ctx: &egui::Context) -> EsitoFiltro {
    ctx.input_mut(|i| {
        let modifiers = i.modifiers;
        scarta_incolla(&mut i.events, modifiers)
    })
}

/// Sovrascrive con zeri i byte della stringa e la rilascia. Restituisce il
/// numero di byte azzerati. Le copie create da egui-winit prima del filtro
/// (`replace("\r\n", "\n")`) non sono raggiungibili da qui.
fn azzera(testo: String) -> usize {
    let mut byte = testo.into_bytes();
    let n = byte.len();
    byte.fill(0);
    std::hint::black_box(&byte);
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
    fn incolla_diventa_tasto_v_senza_testo() {
        let mut ev = vec![
            egui::Event::Text("a".into()),
            egui::Event::Paste("segreto".into()),
        ];
        let esito = scarta_incolla(&mut ev, ctrl());
        assert_eq!(
            esito,
            EsitoFiltro {
                scartati: 1,
                byte_azzerati: 7
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
    fn shift_insert_diventa_insert() {
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        assert_eq!(tasto_incolla(shift), egui::Key::Insert);
        assert_eq!(tasto_incolla(ctrl()), egui::Key::V);
    }

    #[test]
    fn senza_incolla_nessuna_modifica() {
        let mut ev = vec![egui::Event::Copy, egui::Event::Text("x".into())];
        assert_eq!(scarta_incolla(&mut ev, ctrl()), EsitoFiltro::default());
        assert_eq!(ev.len(), 2);
    }
}
