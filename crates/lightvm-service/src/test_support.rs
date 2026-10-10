//! Test support: unique temporary directories. Not part of the API.

use std::path::PathBuf;

/// Creates a unique temporary directory under the system temporary directory.
pub fn temp_dir(name: &str) -> PathBuf {
    let mut b = [0u8; 6];
    getrandom::fill(&mut b).expect("random generator");
    let d = std::env::temp_dir()
        .join("lightvm-service-tests")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            crate::credential::hex_encode(&b)
        ));
    std::fs::create_dir_all(&d).expect("temporary directory creation");
    d
}
