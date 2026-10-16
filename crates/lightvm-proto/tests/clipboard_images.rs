use lightvm_proto::{ToolCall, ToolFileRequest as F, ToolRelativePath as P};

fn image() -> P {
    P::new(vec!["image".into()]).unwrap()
}

#[test]
fn clipboard_images_cannot_select_files_or_escape_the_memory_budget() {
    let valid = F::Begin {
        share: "clipboard-write".into(),
        path: image(),
        size: 16,
        sha256: "ab".repeat(32),
        replace: false,
    };
    assert!(valid.validate_clipboard_image().is_ok());
    assert!(valid.validate_guest().is_err());
    let invalid = [
        F::List {
            share: "outbox".into(),
            path: image(),
        },
        F::Read {
            share: "outbox".into(),
            path: image(),
            offset: 0,
            length: 16,
        },
        F::Read {
            share: "ab".repeat(32),
            path: P::new(vec!["secrets.txt".into()]).unwrap(),
            offset: 0,
            length: 16,
        },
        F::Begin {
            share: "inbox".into(),
            path: image(),
            size: 16,
            sha256: "ab".repeat(32),
            replace: false,
        },
        F::Begin {
            share: "clipboard-read".into(),
            path: image(),
            size: 16,
            sha256: "0".repeat(64),
            replace: false,
        },
        F::Write {
            transfer: "ab".repeat(32),
            offset: u64::MAX,
            data: "00".into(),
        },
        F::Write {
            transfer: "ab".repeat(32),
            offset: 16 * 1024 * 1024 + 12,
            data: "00".into(),
        },
    ];
    for request in invalid {
        assert!(request.validate_clipboard_image().is_err(), "{request:?}");
    }
    let call = ToolCall::ClipboardImage { request: valid };
    assert!(!format!("{call:?}").contains(&"ab".repeat(32)));
}
