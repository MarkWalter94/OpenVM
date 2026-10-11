//! SBOM of this workspace (offline `cargo metadata`, real `Cargo.lock`):
//! every third-party component of the shipped binaries has a licence and a
//! lockfile hash; test-only crates are not shipped; the C code is declared.

use std::path::Path;
use std::process::Command;

#[test]
fn sbom_of_the_workspace() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = std::env::temp_dir().join(format!("lightvm-sbom-test-{}.json", std::process::id()));
    let notices = out.with_extension("txt");
    let s = Command::new(env!("CARGO_BIN_EXE_lightvm-sbom"))
        .arg("--workspace")
        .arg(&root)
        .arg("--out")
        .arg(&out)
        .arg("--notices")
        .arg(&notices)
        .arg("--check-licences")
        .arg(root.join("deny.toml"))
        .env("SOURCE_DATE_EPOCH", "1791676800")
        .output()
        .unwrap();
    assert!(s.status.success(), "{}", String::from_utf8_lossy(&s.stderr));
    let bom: serde_json::Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(bom["specVersion"], "1.5");
    assert_eq!(bom["metadata"]["timestamp"], "2026-10-11T00:00:00Z");
    let comps = bom["components"].as_array().unwrap();
    let names: Vec<&str> = comps.iter().map(|c| c["name"].as_str().unwrap()).collect();
    for n in [
        "lightvm-desktop",
        "lightvm-service",
        "lightvm-mcp",
        "rusqlite",
        "rmcp",
        "ring",
    ] {
        assert!(names.contains(&n), "{n} missing");
    }
    // Test-only and tool crates are not shipped.
    for n in ["proptest", "egui_kittest", "lightvm-sbom"] {
        assert!(!names.contains(&n), "{n} must not be in the SBOM");
    }
    for c in comps {
        assert!(c["licenses"].is_array(), "{} without licence", c["name"]);
        if c["purl"]
            .as_str()
            .is_some_and(|p| p.starts_with("pkg:cargo/"))
            && c["properties"].is_null()
        {
            let h = c["hashes"][0]["content"].as_str().unwrap_or_default();
            assert_eq!(h.len(), 64, "{} without lockfile hash", c["name"]);
        }
    }
    let sqlite = comps
        .iter()
        .find(|c| c["name"] == "libsqlite3-sys")
        .unwrap();
    assert_eq!(sqlite["components"][0]["name"], "SQLite");
    assert_ne!(sqlite["components"][0]["version"], "unknown");
    let text = std::fs::read_to_string(&notices).unwrap();
    assert!(text.contains("rusqlite"));
    let _ = std::fs::remove_file(out);
    let _ = std::fs::remove_file(notices);
}
