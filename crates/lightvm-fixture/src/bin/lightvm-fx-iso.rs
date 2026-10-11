//! Writes the fixture ISO of ADR-0019 (`fixtures/lightvm-fx/fx.sh` in a
//! volume labelled `LIGHTVM_FX`) to the path given as the only argument and
//! prints its SHA-256-free summary (size and label); the image is
//! byte-for-byte reproducible. Usage: `lightvm-fx-iso <out.iso>`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (Some(out), None) = (args.next(), args.next()) else {
        eprintln!("usage: lightvm-fx-iso <out.iso>");
        return ExitCode::from(2);
    };
    let img = lightvm_fixture::fixture_iso();
    match std::fs::write(&out, &img) {
        Ok(()) => {
            println!(
                "{}: {} bytes, label {}",
                std::path::Path::new(&out).display(),
                img.len(),
                lightvm_fixture::VOLUME_LABEL
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("lightvm-fx-iso: {e}");
            ExitCode::FAILURE
        }
    }
}
