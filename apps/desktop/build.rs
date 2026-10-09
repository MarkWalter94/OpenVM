//! Legge dal `Cargo.lock` del workspace le versioni effettive di egui, eframe e
//! wgpu, così che lo smoke test stampi le versioni realmente compilate e non
//! valori scritti a mano.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let lock = manifest.join("..").join("..").join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock.display());
    let text = std::fs::read_to_string(&lock).unwrap_or_default();
    for (pkg, var) in [
        ("egui", "LIGHTVM_EGUI_VERSION"),
        ("eframe", "LIGHTVM_EFRAME_VERSION"),
        ("wgpu", "LIGHTVM_WGPU_VERSION"),
        ("glow", "LIGHTVM_GLOW_VERSION"),
    ] {
        let version = find_version(&text, pkg).unwrap_or_else(|| "sconosciuta".to_owned());
        println!("cargo:rustc-env={var}={version}");
    }
}

fn find_version(lock: &str, pkg: &str) -> Option<String> {
    let needle = format!("name = \"{pkg}\"");
    let mut lines = lock.lines();
    while let Some(line) = lines.next() {
        if line.trim() == needle {
            let next = lines.next()?;
            return next
                .trim()
                .strip_prefix("version = \"")
                .and_then(|v| v.strip_suffix('"'))
                .map(str::to_owned);
        }
    }
    None
}
