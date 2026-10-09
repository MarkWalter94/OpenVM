//! Supporto ai test: directory temporanee uniche. Non fa parte dell'API.

use std::path::PathBuf;

/// Crea una directory temporanea unica sotto la directory temporanea di sistema.
pub fn temp_dir(name: &str) -> PathBuf {
    let mut b = [0u8; 6];
    getrandom::fill(&mut b).expect("generatore casuale");
    let d = std::env::temp_dir()
        .join("lightvm-service-tests")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            crate::credential::hex_encode(&b)
        ));
    std::fs::create_dir_all(&d).expect("creazione directory temporanea");
    d
}
