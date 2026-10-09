//! Test negativo della sezione 7.4 (riga "Console"): il client RFB non ha
//! alcun percorso per inviare input, appunti o power control.
//!
//! Tre controlli complementari:
//! 1. l'unico tipo pubblico scrivibile sul trasporto, `ClientMessage`, produce
//!    soltanto i tipi 0, 2 e 3 (corrispondenza esaustiva: aggiungere una
//!    variante senza aggiornare questo test non compila);
//! 2. la costante pubblica `CLIENT_MESSAGE_TYPES_SENT` non contiene 4, 5, 6;
//! 3. ricerca testuale nel codice del crate (commenti esclusi) di
//!    identificatori e costrutti legati a KeyEvent, PointerEvent e
//!    ClientCutText.

use rfb_client::{CLIENT_MESSAGE_TYPES_SENT, ClientMessage, Encoding, PixelFormat, Rect};

/// Tutte le varianti di `ClientMessage`. Il `match` è esaustivo e senza `_`:
/// una nuova variante rompe la compilazione finché non viene aggiunta qui.
fn tutte_le_varianti() -> Vec<ClientMessage> {
    let campioni = vec![
        ClientMessage::SetPixelFormat(PixelFormat::BGRX8888),
        ClientMessage::SetEncodings(vec![Encoding::Raw, Encoding::DesktopSize]),
        ClientMessage::FramebufferUpdateRequest {
            incremental: true,
            rect: Rect::new(0, 0, 1, 1),
        },
    ];
    for m in &campioni {
        match m {
            ClientMessage::SetPixelFormat(_)
            | ClientMessage::SetEncodings(_)
            | ClientMessage::FramebufferUpdateRequest { .. } => {}
        }
    }
    campioni
}

#[test]
fn client_message_produce_solo_tipi_0_2_3() {
    for m in tutte_le_varianti() {
        let bytes = m.encode();
        assert_eq!(bytes[0], m.message_type());
        assert!(
            [0u8, 2, 3].contains(&bytes[0]),
            "tipo {} non ammesso",
            bytes[0]
        );
        assert!(CLIENT_MESSAGE_TYPES_SENT.contains(&bytes[0]));
    }
}

#[test]
fn nessun_tipo_di_input_o_clipboard_dichiarato() {
    for vietato in [4u8, 5, 6] {
        assert!(!CLIENT_MESSAGE_TYPES_SENT.contains(&vietato));
    }
    assert_eq!(CLIENT_MESSAGE_TYPES_SENT, [0, 2, 3]);
}

/// Rimuove i commenti di riga (`//`, `///`, `//!`) per non contare la
/// documentazione che dichiara l'assenza di questi messaggi.
fn codice_senza_commenti(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn ricerca_testuale_nel_codice_del_crate() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut file = 0;
    let vietati = [
        "keyevent",
        "key_event",
        "pointerevent",
        "pointer_event",
        "clientcuttext",
        "client_cut_text",
        "clipboard",
        "button_mask",
        "keysym",
        "qemuextended",
        "power",
    ];
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        file += 1;
        let src = std::fs::read_to_string(&path).unwrap();
        let code = codice_senza_commenti(&src).to_lowercase();
        for v in vietati {
            let occorrenze = code.matches(v).count();
            assert_eq!(occorrenze, 0, "{v} trovato in {}", path.display());
        }
    }
    assert!(file >= 7, "file sorgente analizzati: {file}");
}
