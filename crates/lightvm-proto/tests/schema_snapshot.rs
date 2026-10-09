//! Snapshot degli schemi: i file in `schema/` devono coincidere con quelli
//! generati. Per rigenerarli: `LIGHTVM_UPDATE_SCHEMAS=1 cargo test -p lightvm-proto --test schema_snapshot`.

use std::collections::BTreeSet;
use std::path::PathBuf;

use lightvm_proto::schema::{public_schemas, render};

fn schema_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("schema")
}

#[test]
fn committed_schemas_match_generated() {
    let dir = schema_dir();
    let update = std::env::var_os("LIGHTVM_UPDATE_SCHEMAS").is_some_and(|v| v == "1");
    let mut expected_files = BTreeSet::new();
    let mut mismatches = Vec::new();
    for (name, schema) in public_schemas() {
        let file = format!("{name}.json");
        let path = dir.join(&file);
        let generated = render(&schema);
        expected_files.insert(file.clone());
        if update {
            std::fs::create_dir_all(&dir).expect("creazione directory schema");
            std::fs::write(&path, &generated).expect("scrittura schema");
            continue;
        }
        match std::fs::read_to_string(&path) {
            // Normalizza CRLF eventualmente introdotti dal checkout su Windows.
            Ok(committed) if committed.replace("\r\n", "\n") == generated => {}
            Ok(_) => mismatches.push(format!("{file}: diverso dal generato")),
            Err(_) => mismatches.push(format!("{file}: mancante")),
        }
    }
    let on_disk: BTreeSet<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.ends_with(".json"))
                .collect()
        })
        .unwrap_or_default();
    for extra in on_disk.difference(&expected_files) {
        mismatches.push(format!("{extra}: file non più generato"));
    }
    assert!(
        mismatches.is_empty(),
        "schemi non allineati (rigenerare con LIGHTVM_UPDATE_SCHEMAS=1):\n{}",
        mismatches.join("\n")
    );
}

#[test]
fn schema_names_are_unique_and_generation_is_deterministic() {
    let a: Vec<_> = public_schemas().into_iter().map(|(n, s)| (n, render(&s))).collect();
    let b: Vec<_> = public_schemas().into_iter().map(|(n, s)| (n, render(&s))).collect();
    assert_eq!(a, b);
    let names: BTreeSet<_> = a.iter().map(|(n, _)| *n).collect();
    assert_eq!(names.len(), a.len());
}

#[test]
fn internal_types_have_no_public_schema() {
    for (name, schema) in public_schemas() {
        let text = render(&schema);
        assert!(!text.contains("managed_path"), "{name} espone managed_path");
        assert!(!text.contains("ManagedRelPath"), "{name} espone ManagedRelPath");
        assert!(!text.contains("file_identity"), "{name} espone file_identity");
    }
}
