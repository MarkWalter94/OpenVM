//! Offline SBOM of the shipped binaries (F6, ADR-0023).
//!
//! Input: the output of `cargo metadata --format-version 1 --locked
//! --offline --filter-platform <target>` and `Cargo.lock`. Output: a
//! CycloneDX 1.5 JSON document whose components are the packages linked
//! into the shipped binaries (normal dependencies reachable from the root
//! packages for the target; build-only and dev-only dependencies are left
//! out, procedural macros are listed with scope `excluded` because they run
//! at build time only), with:
//!
//! - `purl` `pkg:cargo/<name>@<version>` for crates.io packages;
//! - the licence expression declared by each package (old `A/B` syntax
//!   normalised to `A OR B`);
//! - the SHA-256 recorded in `Cargo.lock` for each registry package (the
//!   checksum of the `.crate` archive verified by cargo);
//! - workspace and patched (path) packages marked as such, without hash;
//! - the C code compiled into the binary declared as properties (SQLite
//!   amalgamation of `libsqlite3-sys` with its version, `ring`);
//! - optional `file` components with the SHA-256 of the binaries built;
//! - the dependency graph.
//!
//! The serial number is derived from the inputs (same inputs, same SBOM);
//! the timestamp comes from `SOURCE_DATE_EPOCH` when set.
//!
//! [`notices`] gathers the licence and notice files shipped by each crate
//! in the local registry into one third-party notices text.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

/// Default packages whose binaries are shipped (installer of F2-03).
pub const DEFAULT_ROOTS: [&str; 3] = ["lightvm-desktop", "lightvm-service", "lightvm-mcp"];

/// Target of the shipped Windows binaries.
pub const DEFAULT_TARGET: &str = "x86_64-pc-windows-msvc";

/// Entry of `Cargo.lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockEntry {
    /// Package name.
    pub name: String,
    /// Version.
    pub version: String,
    /// Source (`None` for path packages).
    pub source: Option<String>,
    /// SHA-256 of the `.crate` archive (registry packages).
    pub checksum: Option<String>,
}

/// Parses the `[[package]]` entries of a `Cargo.lock` (format v3/v4).
pub fn parse_lock(text: &str) -> Vec<LockEntry> {
    let mut out = Vec::new();
    let mut cur: Option<LockEntry> = None;
    let value = |l: &str| -> Option<String> {
        let (_, v) = l.split_once('=')?;
        Some(v.trim().trim_matches('"').to_owned())
    };
    for line in text.lines() {
        let l = line.trim();
        if l == "[[package]]" {
            out.extend(cur.take());
            cur = Some(LockEntry {
                name: String::new(),
                version: String::new(),
                source: None,
                checksum: None,
            });
            continue;
        }
        if l.starts_with('[') {
            out.extend(cur.take());
            continue;
        }
        let Some(e) = cur.as_mut() else {
            continue;
        };
        if l.starts_with("name =") {
            e.name = value(l).unwrap_or_default();
        } else if l.starts_with("version =") {
            e.version = value(l).unwrap_or_default();
        } else if l.starts_with("source =") {
            e.source = value(l);
        } else if l.starts_with("checksum =") {
            e.checksum = value(l);
        }
    }
    out.extend(cur);
    out
}

/// Options of [`build_bom`].
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Root packages (default [`DEFAULT_ROOTS`]).
    pub roots: Vec<String>,
    /// Version of the product (default: the first root's version).
    pub product_version: Option<String>,
    /// Shipped files: (file name, SHA-256 hex, size).
    pub binaries: Vec<(String, String, u64)>,
    /// Timestamp (RFC 3339, UTC).
    pub timestamp: String,
}

/// Normalises a cargo licence field into an SPDX expression.
pub fn spdx_expression(license: &str) -> String {
    license
        .split('/')
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn purl(name: &str, version: &str) -> String {
    format!("pkg:cargo/{name}@{version}")
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

/// Package ids reachable from `roots` through normal dependencies.
fn closure(metadata: &Value, roots: &[String]) -> Result<BTreeSet<String>, String> {
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata: no packages")?;
    let members: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .ok_or("cargo metadata: no workspace_members")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let nodes: BTreeMap<&str, &Value> = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata: no resolve graph (run without --no-deps)")?
        .iter()
        .filter_map(|n| Some((str_of(n, "id")?, n)))
        .collect();
    let mut todo: Vec<String> = Vec::new();
    for r in roots {
        let id = packages
            .iter()
            .find(|p| {
                str_of(p, "name") == Some(r) && members.contains(str_of(p, "id").unwrap_or(""))
            })
            .and_then(|p| str_of(p, "id"))
            .ok_or_else(|| format!("root package `{r}` is not a workspace member"))?;
        todo.push(id.to_owned());
    }
    let mut seen = BTreeSet::new();
    while let Some(id) = todo.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(node) = nodes.get(id.as_str()) else {
            continue;
        };
        for d in node["deps"].as_array().into_iter().flatten() {
            let normal = d["dep_kinds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|k| k["kind"].is_null());
            if normal && let Some(p) = str_of(d, "pkg") {
                todo.push(p.to_owned());
            }
        }
    }
    Ok(seen)
}

/// Version of the SQLite amalgamation bundled by `libsqlite3-sys` (read
/// from its `sqlite3/sqlite3.h` in the local registry).
fn bundled_sqlite_version(manifest_path: &Path) -> Option<String> {
    let h = manifest_path.parent()?.join("sqlite3").join("sqlite3.h");
    let text = std::fs::read_to_string(h).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("#define SQLITE_VERSION "))
        .map(|v| v.trim().trim_matches('"').to_owned())
}

/// Builds the CycloneDX 1.5 document.
pub fn build_bom(metadata: &Value, lock: &str, opts: &Options) -> Result<Value, String> {
    let roots: Vec<String> = if opts.roots.is_empty() {
        DEFAULT_ROOTS.iter().map(|s| (*s).to_owned()).collect()
    } else {
        opts.roots.clone()
    };
    let ids = closure(metadata, &roots)?;
    let lock_entries = parse_lock(lock);
    let checksum = |name: &str, version: &str| {
        lock_entries
            .iter()
            .find(|e| e.name == name && e.version == version)
            .and_then(|e| e.checksum.clone())
    };
    let members: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let packages: BTreeMap<&str, &Value> = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((str_of(p, "id")?, p)))
        .collect();
    let bom_ref = |p: &Value| -> String {
        let name = str_of(p, "name").unwrap_or("?");
        let version = str_of(p, "version").unwrap_or("?");
        if p["source"].is_null() {
            format!("path:{name}@{version}")
        } else {
            purl(name, version)
        }
    };
    let mut components = Vec::new();
    let mut refs: BTreeMap<&str, String> = BTreeMap::new();
    for id in &ids {
        let Some(p) = packages.get(id.as_str()) else {
            return Err(format!("package {id} missing from cargo metadata"));
        };
        refs.insert(id.as_str(), bom_ref(p));
    }
    let mut missing_licence = Vec::new();
    for id in &ids {
        let p = packages[id.as_str()];
        let name = str_of(p, "name").unwrap_or_default();
        let version = str_of(p, "version").unwrap_or_default();
        let registry = !p["source"].is_null();
        let proc_macro = p["targets"].as_array().into_iter().flatten().any(|t| {
            t["kind"]
                .as_array()
                .is_some_and(|k| k.iter().any(|x| x == "proc-macro"))
        });
        let mut c = json!({
            "type": if members.contains(id.as_str()) { "application" } else { "library" },
            "bom-ref": refs[id.as_str()],
            "name": name,
            "version": version,
            "scope": if proc_macro { "excluded" } else { "required" },
        });
        if let Some(d) = str_of(p, "description") {
            c["description"] = json!(d.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        match (str_of(p, "license"), str_of(p, "license_file")) {
            (Some(l), _) => c["licenses"] = json!([{ "expression": spdx_expression(l) }]),
            (None, Some(f)) => {
                c["licenses"] =
                    json!([{ "license": { "name": format!("see {f} in the package") } }]);
            }
            (None, None) => missing_licence.push(format!("{name} {version}")),
        }
        let mut props = Vec::new();
        if registry {
            c["purl"] = json!(purl(name, version));
            match checksum(name, version) {
                Some(h) => c["hashes"] = json!([{ "alg": "SHA-256", "content": h }]),
                None => return Err(format!("no Cargo.lock checksum for {name} {version}")),
            }
            if let Some(r) = str_of(p, "repository") {
                c["externalReferences"] = json!([{ "type": "vcs", "url": r }]);
            }
        } else if members.contains(id.as_str()) {
            props.push(json!({"name": "lightvm:origin", "value": "workspace crate (MIT, this repository)"}));
        } else {
            props.push(json!({"name": "lightvm:origin", "value": "patched copy of the crates.io package in vendor/ ([patch.crates-io], ADR-0023)"}));
            c["purl"] = json!(purl(name, version));
        }
        if proc_macro {
            props.push(json!({"name": "lightvm:proc-macro", "value": "runs at build time only, not linked into the binaries"}));
        }
        if name == "libsqlite3-sys" {
            let v = str_of(p, "manifest_path")
                .and_then(|m| bundled_sqlite_version(Path::new(m)))
                .unwrap_or_else(|| "unknown".into());
            props.push(json!({"name": "lightvm:bundled-c", "value": format!("SQLite amalgamation {v} (public domain), compiled into the binary (feature `bundled`)")}));
            c["components"] = json!([{
                "type": "library",
                "bom-ref": format!("sqlite@{v}"),
                "name": "SQLite",
                "version": v,
                "licenses": [{ "license": { "name": "Public Domain (SQLite blessing)" } }],
                "purl": format!("pkg:generic/sqlite@{v}"),
            }]);
        }
        if name == "ring" {
            props.push(json!({"name": "lightvm:bundled-c", "value": "C and assembly derived from BoringSSL, compiled into the binary"}));
        }
        if !props.is_empty() {
            c["properties"] = json!(props);
        }
        components.push(c);
    }
    if !missing_licence.is_empty() {
        return Err(format!(
            "packages without a declared licence: {}",
            missing_licence.join(", ")
        ));
    }
    for (file, sha, size) in &opts.binaries {
        components.push(json!({
            "type": "file",
            "bom-ref": format!("file:{file}"),
            "name": file,
            "hashes": [{ "alg": "SHA-256", "content": sha }],
            "properties": [{ "name": "lightvm:size_bytes", "value": size.to_string() }],
        }));
    }
    let mut dependencies = Vec::new();
    for n in metadata["resolve"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
    {
        let Some(id) = str_of(n, "id") else { continue };
        let Some(r) = refs.get(id) else { continue };
        let mut on: Vec<String> = n["deps"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|d| {
                d["dep_kinds"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|k| k["kind"].is_null())
            })
            .filter_map(|d| refs.get(str_of(d, "pkg")?).cloned())
            .collect();
        on.sort();
        on.dedup();
        dependencies.push(json!({ "ref": r, "dependsOn": on }));
    }
    let first_root = packages
        .values()
        .find(|p| str_of(p, "name") == Some(roots[0].as_str()))
        .and_then(|p| str_of(p, "version"))
        .unwrap_or("0.0.0");
    let version = opts
        .product_version
        .clone()
        .unwrap_or_else(|| first_root.to_owned());
    let mut h = Sha256::new();
    h.update(lock.as_bytes());
    for r in &roots {
        h.update(r.as_bytes());
    }
    for (f, s, _) in &opts.binaries {
        h.update(f.as_bytes());
        h.update(s.as_bytes());
    }
    let d = h.finalize();
    Ok(json!({
        "bomFormat": "CycloneDX",
        "specVersion": "1.5",
        "serialNumber": format!("urn:uuid:{}", uuid_from(&d)),
        "version": 1,
        "metadata": {
            "timestamp": opts.timestamp,
            "tools": { "components": [{
                "type": "application",
                "name": "lightvm-sbom",
                "version": env!("CARGO_PKG_VERSION"),
                "description": "offline generator from cargo metadata and Cargo.lock (crates/lightvm-sbom)"
            }]},
            "component": {
                "type": "application",
                "bom-ref": "lightvm",
                "name": "LightVM",
                "version": version,
                "licenses": [{ "expression": "MIT" }],
                "properties": [
                    { "name": "lightvm:roots", "value": roots.join(",") },
                    { "name": "lightvm:not-included", "value": "QEMU, UEFI firmware and guest media are not redistributed (ADR-0023 item 7)" }
                ]
            }
        },
        "components": components,
        "dependencies": dependencies,
    }))
}

/// Deterministic UUID (version 8, RFC 9562) from a digest.
fn uuid_from(d: &[u8]) -> String {
    let mut b = [0u8; 16];
    b.copy_from_slice(&d[..16]);
    b[6] = (b[6] & 0x0f) | 0x80;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// RFC 3339 UTC timestamp of `secs` since the Unix epoch.
pub fn rfc3339(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Civil date from days (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// SHA-256 (hex) and size of a file.
pub fn file_sha256(p: &Path) -> std::io::Result<(String, u64)> {
    let bytes = std::fs::read(p)?;
    let d = Sha256::digest(&bytes);
    Ok((
        d.iter().map(|b| format!("{b:02x}")).collect(),
        bytes.len() as u64,
    ))
}

/// Third-party notices: for every non-workspace component of `metadata`
/// reachable from the roots, its licence expression and the text of the
/// licence/notice files it ships (`LICENSE*`, `LICENCE*`, `COPYING*`,
/// `NOTICE*`, `UNLICENSE`), read from the local registry. Returns the text
/// and the packages that ship no licence file (their SPDX expression is
/// listed; the standard text must be added by hand).
pub fn notices(metadata: &Value, roots: &[String]) -> Result<(String, Vec<String>), String> {
    let roots: Vec<String> = if roots.is_empty() {
        DEFAULT_ROOTS.iter().map(|s| (*s).to_owned()).collect()
    } else {
        roots.to_vec()
    };
    let ids = closure(metadata, &roots)?;
    let members: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut out = String::from(
        "LightVM third-party notices\n\nLicences and notices of the third-party packages linked into the LightVM binaries\n(generated by crates/lightvm-sbom from Cargo.lock; procedural macros run at build time only).\n",
    );
    let mut without = Vec::new();
    let mut pkgs: Vec<&Value> = metadata["packages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| str_of(p, "id").is_some_and(|id| ids.contains(id) && !members.contains(id)))
        .collect();
    pkgs.sort_by_key(|p| (str_of(p, "name"), str_of(p, "version")));
    for p in pkgs {
        let name = str_of(p, "name").unwrap_or_default();
        let version = str_of(p, "version").unwrap_or_default();
        let licence = str_of(p, "license")
            .map(spdx_expression)
            .or_else(|| str_of(p, "license_file").map(|f| format!("see {f}")))
            .unwrap_or_default();
        let _ = write!(out, "\n{}\n{name} {version} — {licence}\n", "=".repeat(78));
        let dir: PathBuf = str_of(p, "manifest_path")
            .map(|m| {
                Path::new(m)
                    .parent()
                    .unwrap_or(Path::new("."))
                    .to_path_buf()
            })
            .unwrap_or_default();
        let mut files = licence_files(&dir, true);
        if files.is_empty() {
            // Some crates keep them one level down (for example the font
            // licences of `epaint_default_fonts` in `fonts/`).
            for sub in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                if sub.path().is_dir() {
                    files.extend(licence_files(&sub.path(), false));
                }
            }
        }
        files.sort();
        if files.is_empty() {
            without.push(format!("{name} {version} ({licence})"));
            let _ = writeln!(
                out,
                "(the package ships no licence file; licence by SPDX identifier: {licence})"
            );
        }
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            let _ = write!(
                out,
                "\n--- {} ---\n{}\n",
                f.file_name().unwrap_or_default().to_string_lossy(),
                text.trim_end()
            );
        }
    }
    Ok((out, without))
}

/// Licence policy read from `deny.toml` (`[licenses]` `allow` list and the
/// per-crate `exceptions`), for an offline check when `cargo-deny` is not
/// installed. It evaluates the declared SPDX expressions only (no licence
/// text detection, unlike `cargo-deny`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LicencePolicy {
    /// Allowed SPDX identifiers (a `WITH` exception is one entry).
    pub allow: BTreeSet<String>,
    /// Extra identifiers allowed for one crate.
    pub exceptions: BTreeMap<String, BTreeSet<String>>,
}

fn quoted(s: &str) -> Vec<String> {
    s.split('"')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, t)| t.to_owned())
        .collect()
}

/// Parses the licence policy of a `deny.toml`.
pub fn parse_deny_policy(text: &str) -> LicencePolicy {
    let mut p = LicencePolicy::default();
    let mut section = String::new();
    let mut in_allow = false;
    let mut in_exceptions = false;
    for line in text.lines() {
        let l = line.split('#').next().unwrap_or_default().trim();
        if l.starts_with('[') && !l.starts_with("[[") && l.ends_with(']') && !l.contains('=') {
            section = l.to_owned();
            continue;
        }
        if section != "[licenses]" {
            continue;
        }
        if l.starts_with("allow = [") {
            in_allow = !l.ends_with(']');
            p.allow.extend(quoted(l));
            continue;
        }
        if l.starts_with("exceptions = [") {
            in_exceptions = !l.ends_with(']');
            continue;
        }
        if in_allow {
            if l.starts_with(']') {
                in_allow = false;
            } else {
                p.allow.extend(quoted(l));
            }
        } else if in_exceptions {
            if l.starts_with(']') {
                in_exceptions = false;
            } else if let Some(rest) = l.split("crate =").nth(1) {
                let q = quoted(rest);
                if let Some((krate, ids)) = q.split_first() {
                    p.exceptions
                        .entry(krate.clone())
                        .or_default()
                        .extend(ids.iter().cloned());
                }
            }
        }
    }
    p
}

/// `true` if the SPDX `expr` is satisfied by identifiers for which
/// `ok(id)` holds (`OR`: one branch; `AND`: every part; `A WITH B` is one
/// identifier; parentheses). A malformed expression is not satisfied.
pub fn spdx_satisfied(expr: &str, ok: &dyn Fn(&str) -> bool) -> bool {
    let spaced = expr.replace('(', " ( ").replace(')', " ) ");
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    let mut pos = 0;
    parse_or(&tokens, &mut pos, ok) == Some(true) && pos == tokens.len()
}

fn parse_or(t: &[&str], pos: &mut usize, ok: &dyn Fn(&str) -> bool) -> Option<bool> {
    let mut v = parse_and(t, pos, ok)?;
    while t.get(*pos) == Some(&"OR") {
        *pos += 1;
        let r = parse_and(t, pos, ok)?;
        v = v || r;
    }
    Some(v)
}

fn parse_and(t: &[&str], pos: &mut usize, ok: &dyn Fn(&str) -> bool) -> Option<bool> {
    let mut v = parse_atom(t, pos, ok)?;
    while t.get(*pos) == Some(&"AND") {
        *pos += 1;
        let r = parse_atom(t, pos, ok)?;
        v = v && r;
    }
    Some(v)
}

fn parse_atom(t: &[&str], pos: &mut usize, ok: &dyn Fn(&str) -> bool) -> Option<bool> {
    match t.get(*pos) {
        Some(&"(") => {
            *pos += 1;
            let v = parse_or(t, pos, ok)?;
            (t.get(*pos) == Some(&")")).then(|| {
                *pos += 1;
                v
            })
        }
        Some(id) if !matches!(*id, ")" | "AND" | "OR" | "WITH") => {
            *pos += 1;
            if t.get(*pos) == Some(&"WITH") {
                *pos += 1;
                let exc = t.get(*pos)?;
                *pos += 1;
                Some(ok(&format!("{id} WITH {exc}")))
            } else {
                Some(ok(id))
            }
        }
        _ => None,
    }
}

/// Components of `bom` whose licence does not satisfy `policy` (workspace
/// crates excluded: they are MIT and `deny.toml` ignores them).
pub fn licence_violations(bom: &Value, policy: &LicencePolicy) -> Vec<String> {
    let mut out = Vec::new();
    for c in bom["components"].as_array().into_iter().flatten() {
        if c["type"] == "file" || c["type"] == "application" {
            continue;
        }
        let name = c["name"].as_str().unwrap_or_default();
        let version = c["version"].as_str().unwrap_or_default();
        let Some(expr) = c["licenses"][0]["expression"].as_str() else {
            out.push(format!("{name} {version}: no SPDX expression"));
            continue;
        };
        let extra = policy.exceptions.get(name);
        let ok = |id: &str| policy.allow.contains(id) || extra.is_some_and(|e| e.contains(id));
        if !spdx_satisfied(expr, &ok) {
            out.push(format!(
                "{name} {version}: `{expr}` not allowed by the policy"
            ));
        }
    }
    out
}

/// Licence and notice files directly in `dir`. At the package root the
/// conventional names; below it also the licence texts of bundled assets
/// (`*license*.txt`, `OFL.txt`, `UFL.txt`, the `.txt` next to a font).
fn licence_files(dir: &Path, root: bool) -> Vec<PathBuf> {
    let fonts = dir
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("fonts"));
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|f| {
                    let n = f
                        .file_name()
                        .map(|n| n.to_string_lossy().to_ascii_uppercase())
                        .unwrap_or_default();
                    let conventional = [
                        "LICENSE",
                        "LICENCE",
                        "COPYING",
                        "NOTICE",
                        "UNLICENSE",
                        "COPYRIGHT",
                    ]
                    .iter()
                    .any(|k| n.starts_with(k));
                    let nested = !root
                        && n.ends_with(".TXT")
                        && (n.contains("LICENSE") || n == "OFL.TXT" || n == "UFL.TXT" || fonts);
                    f.is_file() && (conventional || nested)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_metadata() -> Value {
        let reg = "registry+https://github.com/rust-lang/crates.io-index";
        json!({
            "workspace_members": ["path+file:///w/app#lightvm-desktop@0.0.0", "path+file:///w/tool#tool@0.0.0"],
            "packages": [
                {"id": "path+file:///w/app#lightvm-desktop@0.0.0", "name": "lightvm-desktop", "version": "0.0.0", "source": null, "license": "MIT", "targets": [{"kind": ["bin"]}]},
                {"id": "path+file:///w/tool#tool@0.0.0", "name": "tool", "version": "0.0.0", "source": null, "license": "MIT", "targets": [{"kind": ["bin"]}]},
                {"id": format!("{reg}#serde@1.0.0"), "name": "serde", "version": "1.0.0", "source": reg, "license": "MIT/Apache-2.0", "targets": [{"kind": ["lib"]}], "repository": "https://example.invalid/serde"},
                {"id": format!("{reg}#derive@1.0.0"), "name": "derive", "version": "1.0.0", "source": reg, "license": "MIT", "targets": [{"kind": ["proc-macro"]}]},
                {"id": format!("{reg}#cc@1.0.0"), "name": "cc", "version": "1.0.0", "source": reg, "license": "MIT", "targets": [{"kind": ["lib"]}]},
                {"id": format!("{reg}#proptest@1.0.0"), "name": "proptest", "version": "1.0.0", "source": reg, "license": "MIT", "targets": [{"kind": ["lib"]}]},
                {"id": "path+file:///w/vendor/egui-winit#egui-winit@0.36.2", "name": "egui-winit", "version": "0.36.2", "source": null, "license": "MIT OR Apache-2.0", "targets": [{"kind": ["lib"]}]}
            ],
            "resolve": {"nodes": [
                {"id": "path+file:///w/app#lightvm-desktop@0.0.0", "deps": [
                    {"pkg": format!("{reg}#serde@1.0.0"), "dep_kinds": [{"kind": null}]},
                    {"pkg": format!("{reg}#derive@1.0.0"), "dep_kinds": [{"kind": null}]},
                    {"pkg": format!("{reg}#cc@1.0.0"), "dep_kinds": [{"kind": "build"}]},
                    {"pkg": format!("{reg}#proptest@1.0.0"), "dep_kinds": [{"kind": "dev"}]},
                    {"pkg": "path+file:///w/vendor/egui-winit#egui-winit@0.36.2", "dep_kinds": [{"kind": null}]}
                ]},
                {"id": format!("{reg}#serde@1.0.0"), "deps": []},
                {"id": format!("{reg}#derive@1.0.0"), "deps": []},
                {"id": format!("{reg}#cc@1.0.0"), "deps": []},
                {"id": format!("{reg}#proptest@1.0.0"), "deps": []},
                {"id": "path+file:///w/vendor/egui-winit#egui-winit@0.36.2", "deps": []},
                {"id": "path+file:///w/tool#tool@0.0.0", "deps": [{"pkg": format!("{reg}#cc@1.0.0"), "dep_kinds": [{"kind": null}]}]}
            ]}
        })
    }

    const LOCK: &str = r#"version = 4

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "aa"

[[package]]
name = "derive"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "bb"
dependencies = [
 "serde",
]

[[package]]
name = "egui-winit"
version = "0.36.2"
"#;

    #[test]
    fn lockfile_entries() {
        let e = parse_lock(LOCK);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0].checksum.as_deref(), Some("aa"));
        assert_eq!(e[2].source, None);
        assert_eq!(e[2].checksum, None);
    }

    #[test]
    fn components_are_the_shipped_closure() {
        let opts = Options {
            roots: vec!["lightvm-desktop".into()],
            binaries: vec![("lightvm-desktop.exe".into(), "cc".repeat(32), 42)],
            timestamp: rfc3339(0),
            ..Options::default()
        };
        let bom = build_bom(&fake_metadata(), LOCK, &opts).unwrap();
        assert_eq!(bom["bomFormat"], "CycloneDX");
        assert_eq!(bom["specVersion"], "1.5");
        let names: Vec<&str> = bom["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["name"].as_str().unwrap())
            .collect();
        // Build-only (`cc`) and dev-only (`proptest`) dependencies and the
        // other workspace tool are left out.
        assert!(!names.contains(&"cc") && !names.contains(&"proptest") && !names.contains(&"tool"));
        for n in [
            "lightvm-desktop",
            "serde",
            "derive",
            "egui-winit",
            "lightvm-desktop.exe",
        ] {
            assert!(names.contains(&n), "{n}: {names:?}");
        }
        let c = |n: &str| {
            bom["components"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["name"] == n)
                .unwrap()
                .clone()
        };
        let serde = c("serde");
        assert_eq!(serde["purl"], "pkg:cargo/serde@1.0.0");
        assert_eq!(serde["licenses"][0]["expression"], "MIT OR Apache-2.0");
        assert_eq!(serde["hashes"][0]["content"], "aa");
        assert_eq!(serde["scope"], "required");
        assert_eq!(c("derive")["scope"], "excluded");
        assert!(
            c("egui-winit")["properties"][0]["value"]
                .as_str()
                .unwrap()
                .contains("patched")
        );
        assert_eq!(
            c("lightvm-desktop.exe")["hashes"][0]["content"],
            "cc".repeat(32)
        );
        let deps = bom["dependencies"].as_array().unwrap();
        let root = deps
            .iter()
            .find(|d| d["ref"] == "path:lightvm-desktop@0.0.0")
            .unwrap();
        assert_eq!(root["dependsOn"].as_array().unwrap().len(), 3);
        // Deterministic.
        assert_eq!(bom, build_bom(&fake_metadata(), LOCK, &opts).unwrap());
        assert!(
            bom["serialNumber"]
                .as_str()
                .unwrap()
                .starts_with("urn:uuid:")
        );
    }

    #[test]
    fn registry_package_without_checksum_is_an_error() {
        let lock = LOCK.replace("checksum = \"aa\"\n", "");
        let opts = Options {
            roots: vec!["lightvm-desktop".into()],
            timestamp: rfc3339(0),
            ..Options::default()
        };
        let e = build_bom(&fake_metadata(), &lock, &opts).unwrap_err();
        assert!(e.contains("serde"), "{e}");
        let e = build_bom(
            &fake_metadata(),
            LOCK,
            &Options {
                roots: vec!["nope".into()],
                ..opts
            },
        )
        .unwrap_err();
        assert!(e.contains("nope"), "{e}");
    }

    #[test]
    fn timestamps() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(rfc3339(1_791_676_800 + 3661), "2026-10-11T01:01:01Z");
    }

    #[test]
    fn licence_policy_from_deny_toml() {
        let toml = r#"
[graph]
targets = ["x"]

[licenses]
# comment
allow = [
    "MIT",
    "Apache-2.0", # trailing
    "Apache-2.0 WITH LLVM-exception",
]
exceptions = [
    { crate = "fonts", allow = ["OFL-1.1", "Ubuntu-font-1.0"] },
]

[licenses.private]
ignore = true
"#;
        let p = parse_deny_policy(toml);
        assert_eq!(p.allow.len(), 3);
        assert!(p.exceptions["fonts"].contains("OFL-1.1"));
        let ok = |id: &str| p.allow.contains(id);
        assert!(spdx_satisfied("MIT OR Apache-2.0", &ok));
        assert!(spdx_satisfied("Apache-2.0 OR GPL-2.0-only", &ok));
        assert!(!spdx_satisfied("MIT AND GPL-3.0", &ok));
        assert!(spdx_satisfied(
            "(MIT OR Apache-2.0) AND Apache-2.0 WITH LLVM-exception",
            &ok
        ));
        assert!(!spdx_satisfied("GPL-2.0 WITH Classpath-exception-2.0", &ok));
        assert!(!spdx_satisfied("MIT OR", &ok));
        let bom = json!({"components": [
            {"type": "library", "name": "a", "version": "1", "licenses": [{"expression": "MIT"}]},
            {"type": "library", "name": "fonts", "version": "1", "licenses": [{"expression": "(MIT OR Apache-2.0) AND OFL-1.1"}]},
            {"type": "library", "name": "bad", "version": "1", "licenses": [{"expression": "LGPL-2.1"}]},
            {"type": "application", "name": "ours", "version": "0", "licenses": [{"expression": "MIT"}]}
        ]});
        let v = licence_violations(&bom, &p);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].starts_with("bad 1"));
    }

    #[test]
    fn spdx_normalisation() {
        assert_eq!(spdx_expression("MIT/Apache-2.0"), "MIT OR Apache-2.0");
        assert_eq!(spdx_expression("MIT OR Apache-2.0"), "MIT OR Apache-2.0");
    }
}
