//! Negative test of section 7.4 (row "Console"): the RFB client has no
//! path at all to send input, clipboard or power control.
//!
//! Three complementary checks:
//! 1. the only public type writable to the transport, `ClientMessage`, produces
//!    only types 0, 2 and 3 (exhaustive match: adding a
//!    variant without updating this test does not compile);
//! 2. the public constant `CLIENT_MESSAGE_TYPES_SENT` does not contain 4, 5, 6;
//! 3. a textual search of the crate code (comments excluded) for
//!    identifiers and constructs related to KeyEvent, PointerEvent and
//!    ClientCutText.

use rfb_client::{CLIENT_MESSAGE_TYPES_SENT, ClientMessage, Encoding, PixelFormat, Rect};

/// All `ClientMessage` variants. The `match` is exhaustive and without `_`:
/// a new variant breaks the build until it is added here.
fn all_variants() -> Vec<ClientMessage> {
    let samples = vec![
        ClientMessage::SetPixelFormat(PixelFormat::BGRX8888),
        ClientMessage::SetEncodings(vec![Encoding::Raw, Encoding::DesktopSize]),
        ClientMessage::FramebufferUpdateRequest {
            incremental: true,
            rect: Rect::new(0, 0, 1, 1),
        },
    ];
    for m in &samples {
        match m {
            ClientMessage::SetPixelFormat(_)
            | ClientMessage::SetEncodings(_)
            | ClientMessage::FramebufferUpdateRequest { .. } => {}
        }
    }
    samples
}

#[test]
fn client_message_produces_only_types_0_2_3() {
    for m in all_variants() {
        let bytes = m.encode();
        assert_eq!(bytes[0], m.message_type());
        assert!(
            [0u8, 2, 3].contains(&bytes[0]),
            "type {} not allowed",
            bytes[0]
        );
        assert!(CLIENT_MESSAGE_TYPES_SENT.contains(&bytes[0]));
    }
}

#[test]
fn no_input_or_clipboard_type_declared() {
    for forbidden in [4u8, 5, 6] {
        assert!(!CLIENT_MESSAGE_TYPES_SENT.contains(&forbidden));
    }
    assert_eq!(CLIENT_MESSAGE_TYPES_SENT, [0, 2, 3]);
}

/// Removes line comments (`//`, `///`, `//!`) so as not to count the
/// documentation that states the absence of these messages.
fn code_without_comments(src: &str) -> String {
    src.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn textual_search_in_crate_code() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = 0;
    let forbidden_words = [
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
        files += 1;
        let src = std::fs::read_to_string(&path).unwrap();
        let code = code_without_comments(&src).to_lowercase();
        for v in forbidden_words {
            let occurrences = code.matches(v).count();
            assert_eq!(occurrences, 0, "{v} found in {}", path.display());
        }
    }
    assert!(files >= 7, "source files analysed: {files}");
}
