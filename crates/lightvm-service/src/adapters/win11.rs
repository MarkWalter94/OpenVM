//! Exact, hash-approved closure of the separate Windows 11 runtime bundle.
use lightvm_proto::PublicError;
use qemu_backend::{ManagedPath, storage_guard::StorageGuards};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

#[derive(Debug)]
pub(super) struct Bundle {
    pub root: PathBuf,
    pub hashes: BTreeMap<String, String>,
    pub guard: StorageGuards,
}
fn refused() -> PublicError {
    PublicError::unsupported(
        "Windows 11 needs a complete, approved QEMU/TPM/Secure Boot bundle; missing, changed or additional files were found",
    )
}
fn inventory(
    root: &Path,
    dir: &Path,
    files: &mut BTreeMap<String, ManagedPath>,
    depth: usize,
) -> Result<(), PublicError> {
    if depth > 8 || files.len() > 512 {
        return Err(refused());
    }
    ManagedPath::verify(dir.to_path_buf()).map_err(|_| refused())?;
    for entry in std::fs::read_dir(dir).map_err(|_| refused())? {
        let entry = entry.map_err(|_| refused())?;
        let kind = entry.file_type().map_err(|_| refused())?;
        let path = entry.path();
        let managed = ManagedPath::verify(path.clone()).map_err(|_| refused())?;
        if kind.is_dir() {
            inventory(root, &path, files, depth + 1)?;
        } else if kind.is_file() {
            if entry.metadata().map_err(|_| refused())?.len() > 128 * 1024 * 1024 {
                return Err(refused());
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| refused())?
                .to_str()
                .ok_or_else(refused)?
                .replace('\\', "/");
            if files.insert(relative, managed).is_some() {
                return Err(refused());
            }
        } else {
            return Err(refused());
        }
    }
    Ok(())
}
pub(super) fn open(root: &Path, approved: &[String]) -> Result<Bundle, PublicError> {
    let mut files = BTreeMap::new();
    inventory(root, root, &mut files, 0)?;
    let guard = StorageGuards::acquire(&files.values().cloned().collect::<Vec<_>>(), &[])
        .map_err(|_| refused())?;
    let manifest = root.join("SHA256SUMS");
    let hash = qemu_backend::probe::sha256_file(&manifest).map_err(|_| refused())?;
    if !approved.iter().any(|h| h.eq_ignore_ascii_case(&hash)) {
        return Err(refused());
    }
    if std::fs::metadata(&manifest).map_err(|_| refused())?.len() > 128 * 1024 {
        return Err(refused());
    }
    let text = std::fs::read_to_string(&manifest).map_err(|_| refused())?;
    let mut hashes = BTreeMap::new();
    for line in text.lines() {
        let (digest, name) = line.split_once(" *").ok_or_else(refused)?;
        if digest.len() != 64
            || !digest.bytes().all(|b| b.is_ascii_hexdigit())
            || name == "SHA256SUMS"
            || name.split('/').any(|p| {
                p.is_empty()
                    || p == "."
                    || p == ".."
                    || !p
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
            })
        {
            return Err(refused());
        }
        let file = files.get(name).ok_or_else(refused)?;
        let actual = qemu_backend::probe::sha256_file(file.as_path()).map_err(|_| refused())?;
        if !actual.eq_ignore_ascii_case(digest) || hashes.insert(name.into(), actual).is_some() {
            return Err(refused());
        }
    }
    if hashes.len() + 1 != files.len() {
        return Err(refused());
    }
    for required in [
        "qemu/qemu-system-x86_64.exe",
        "swtpm/swtpm.exe",
        "swtpm/cygwin1.dll",
        "swtpm/cygtpms-0.dll",
        "firmware/lightvm-x64-code.fd",
        "firmware/lightvm-x64-vars-win.fd",
    ] {
        if !hashes.contains_key(required) {
            return Err(refused());
        }
    }
    Ok(Bundle {
        root: root.into(),
        hashes,
        guard,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn fixture() -> (Temp, String) {
        let root = std::env::temp_dir().join(format!(
            "lightvm-win11-bundle-{}",
            lightvm_proto::RunId::new_v7()
        ));
        std::fs::create_dir(&root).unwrap();
        let names = [
            "qemu/qemu-system-x86_64.exe",
            "swtpm/swtpm.exe",
            "swtpm/cygwin1.dll",
            "swtpm/cygtpms-0.dll",
            "firmware/lightvm-x64-code.fd",
            "firmware/lightvm-x64-vars-win.fd",
        ];
        let mut manifest = String::new();
        for name in names {
            let p = root.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"fixture only").unwrap();
            manifest.push_str(&format!(
                "{} *{name}\n",
                qemu_backend::probe::sha256_file(&p).unwrap()
            ));
        }
        std::fs::write(root.join("SHA256SUMS"), manifest).unwrap();
        let hash = qemu_backend::probe::sha256_file(&root.join("SHA256SUMS")).unwrap();
        (Temp(root), hash)
    }
    #[test]
    fn bundle_requires_approved_manifest_and_exact_runtime_closure() {
        let (root, hash) = fixture();
        assert!(open(&root.0, &[]).is_err());
        let bundle = open(&root.0, std::slice::from_ref(&hash)).unwrap();
        #[cfg(windows)]
        assert!(
            std::fs::write(root.0.join("swtpm/cygwin1.dll"), b"replace locked runtime").is_err()
        );
        drop(bundle);
        std::fs::write(root.0.join("swtpm/unlisted.dll"), b"injected DLL").unwrap();
        assert!(open(&root.0, std::slice::from_ref(&hash)).is_err());
        std::fs::remove_file(root.0.join("swtpm/unlisted.dll")).unwrap();
        std::fs::write(
            root.0.join("firmware/lightvm-x64-vars-win.fd"),
            b"changed firmware",
        )
        .unwrap();
        assert!(open(&root.0, &[hash]).is_err());
    }
    #[test]
    fn even_an_approved_manifest_cannot_escape_or_duplicate_a_path() {
        for name in [
            "../escape.dll",
            "swtpm/../escape.dll",
            "C:/escape.dll",
            "swtpm\\escape.dll",
            "swtpm//escape.dll",
            "SHA256SUMS",
        ] {
            let (root, _) = fixture();
            let manifest = root.0.join("SHA256SUMS");
            let text = std::fs::read_to_string(&manifest).unwrap();
            std::fs::write(&manifest, format!("{text}{} *{name}\n", "0".repeat(64))).unwrap();
            let hash = qemu_backend::probe::sha256_file(&manifest).unwrap();
            assert!(open(&root.0, &[hash]).is_err(), "{name}");
        }
    }
}
