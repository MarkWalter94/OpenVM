//! `lightvm-sbom`: CycloneDX 1.5 SBOM and third-party notices of the
//! shipped LightVM binaries, offline (see the library documentation).
//!
//! ```text
//! lightvm-sbom [--workspace DIR] [--target TRIPLE] [--root PKG]...
//!              [--binary FILE]... [--product-version V]
//!              [--out FILE] [--notices FILE] [--metadata FILE]
//! ```
//!
//! Without `--metadata` it runs `cargo metadata --format-version 1 --locked
//! --offline --filter-platform TRIPLE` on the workspace (the `CARGO`
//! variable names cargo, as when run through `cargo run`). Exit codes: 0
//! written, 1 error, 2 wrong usage.

use std::path::PathBuf;
use std::process::{Command, ExitCode};

use lightvm_sbom::{
    DEFAULT_TARGET, Options, build_bom, file_sha256, licence_violations, notices,
    parse_deny_policy, rfc3339,
};

const USAGE: &str = "usage: lightvm-sbom [--workspace DIR] [--target TRIPLE] [--root PKG]... \
[--binary FILE]... [--product-version V] [--out FILE] [--notices FILE] [--metadata FILE] \
[--check-licences DENY_TOML]";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, msg)) => {
            eprintln!("lightvm-sbom: {msg}");
            ExitCode::from(code)
        }
    }
}

fn run() -> Result<(), (u8, String)> {
    let mut workspace = PathBuf::from(".");
    let mut target = DEFAULT_TARGET.to_owned();
    let mut roots = Vec::new();
    let mut binaries = Vec::new();
    let mut product_version = None;
    let mut out = None;
    let mut notices_out = None;
    let mut metadata_file = None;
    let mut policy_file: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || {
            args.next()
                .ok_or((2, format!("{a}: value missing\n{USAGE}")))
        };
        match a.as_str() {
            "--workspace" => workspace = PathBuf::from(val()?),
            "--target" => target = val()?,
            "--root" => roots.push(val()?),
            "--binary" => binaries.push(PathBuf::from(val()?)),
            "--product-version" => product_version = Some(val()?),
            "--out" => out = Some(PathBuf::from(val()?)),
            "--notices" => notices_out = Some(PathBuf::from(val()?)),
            "--metadata" => metadata_file = Some(PathBuf::from(val()?)),
            "--check-licences" => policy_file = Some(PathBuf::from(val()?)),
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => return Err((2, format!("unknown argument {other}\n{USAGE}"))),
        }
    }
    let metadata_text = match metadata_file {
        Some(f) => std::fs::read_to_string(&f).map_err(|e| (1, format!("{}: {e}", f.display())))?,
        None => {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let o = Command::new(cargo)
                .args(["metadata", "--format-version", "1", "--locked", "--offline"])
                .args(["--filter-platform", &target])
                .arg("--manifest-path")
                .arg(workspace.join("Cargo.toml"))
                .output()
                .map_err(|e| (1, format!("cargo metadata: {e}")))?;
            if !o.status.success() {
                return Err((
                    1,
                    format!(
                        "cargo metadata failed: {}",
                        String::from_utf8_lossy(&o.stderr).trim()
                    ),
                ));
            }
            String::from_utf8(o.stdout).map_err(|e| (1, format!("cargo metadata: {e}")))?
        }
    };
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_text).map_err(|e| (1, format!("cargo metadata: {e}")))?;
    let lock_path = workspace.join("Cargo.lock");
    let lock = std::fs::read_to_string(&lock_path)
        .map_err(|e| (1, format!("{}: {e}", lock_path.display())))?;
    let mut bins = Vec::new();
    for b in &binaries {
        let (sha, size) = file_sha256(b).map_err(|e| (1, format!("{}: {e}", b.display())))?;
        let name = b
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        bins.push((name, sha, size));
    }
    let secs = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        });
    let opts = Options {
        roots: roots.clone(),
        product_version,
        binaries: bins,
        timestamp: rfc3339(secs),
    };
    let bom = build_bom(&metadata, &lock, &opts).map_err(|e| (1, e))?;
    let text = serde_json::to_string_pretty(&bom).map_err(|e| (1, e.to_string()))?;
    let n = bom["components"].as_array().map_or(0, Vec::len);
    match &out {
        Some(f) => {
            std::fs::write(f, text.as_bytes()).map_err(|e| (1, format!("{}: {e}", f.display())))?;
        }
        None => println!("{text}"),
    }
    eprintln!("lightvm-sbom: {n} components (target {target})");
    if let Some(f) = policy_file {
        let text = std::fs::read_to_string(&f).map_err(|e| (1, format!("{}: {e}", f.display())))?;
        let policy = parse_deny_policy(&text);
        if policy.allow.is_empty() {
            return Err((1, format!("{}: no [licenses] allow list", f.display())));
        }
        let v = licence_violations(&bom, &policy);
        if !v.is_empty() {
            return Err((
                1,
                format!("licence policy violations:\n  {}", v.join("\n  ")),
            ));
        }
        eprintln!(
            "lightvm-sbom: licences of {n} components satisfy {} ({} allowed identifiers, {} crate exceptions)",
            f.display(),
            policy.allow.len(),
            policy.exceptions.len()
        );
    }
    if let Some(f) = notices_out {
        let (text, without) = notices(&metadata, &roots).map_err(|e| (1, e))?;
        std::fs::write(&f, text.as_bytes()).map_err(|e| (1, format!("{}: {e}", f.display())))?;
        eprintln!(
            "lightvm-sbom: notices written to {}; {} packages ship no licence file{}",
            f.display(),
            without.len(),
            if without.is_empty() {
                String::new()
            } else {
                format!(": {}", without.join(", "))
            }
        );
    }
    Ok(())
}
