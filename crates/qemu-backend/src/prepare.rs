//! Preparazione di un'esecuzione sul disco dell'host (F1-05, F1-08, F1-12):
//! directory dell'esecuzione e directory privata dei socket con ACL esplicita
//! solo-utente, copia per esecuzione delle variabili UEFI, creazione del disco
//! principale e dell'overlay con `qemu-img`, risorse dell'host (memoria e
//! spazio libero) misurate invece che dichiarate.
//!
//! Tutti i percorsi sono [`ManagedPath`] forniti dal proprietario dello storage;
//! nessuna funzione sovrascrive un file esistente. `qemu-img` è eseguito senza
//! shell, con ambiente ridotto e timeout ([`crate::probe::run_capture`]); il
//! binario va verificato prima con [`crate::probe::verify_approved_binary`]
//! (lo fa [`crate::probe::probe_host`]).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::cmdline::DiskFormat;
use crate::error::{BackendError, Result};
use crate::paths::ManagedPath;
use crate::probe::{run_capture, stderr_excerpt};

/// Dimensione minima di un disco creato dal backend.
pub const MIN_DISK_MIB: u64 = 16;
/// Dimensione massima di un disco creato dal backend (2 TiB).
pub const MAX_DISK_MIB: u64 = 2 * 1024 * 1024;

/// Timeout predefinito di `qemu-img create`.
pub const QEMU_IMG_TIMEOUT: Duration = Duration::from_secs(60);

fn refuse_existing(field: &'static str, p: &ManagedPath) -> Result<()> {
    if std::fs::symlink_metadata(p.as_path()).is_ok() {
        return Err(BackendError::StateConflict(format!(
            "`{field}` esiste già: il backend non sovrascrive file"
        )));
    }
    Ok(())
}

fn qemu_img_create(qemu_img: &ManagedPath, args: &[&str], timeout: Duration) -> Result<()> {
    let o = run_capture(qemu_img.as_path(), args, timeout)?;
    if o.code != Some(0) {
        return Err(BackendError::Io(std::io::Error::other(format!(
            "qemu-img create uscito con {:?}: {}",
            o.code,
            stderr_excerpt(&o.stderr)
        ))));
    }
    Ok(())
}

/// Crea il disco principale vuoto (`qemu-img create -f <formato>`), di
/// `size_mib` MiB. Rifiuta file esistenti e dimensioni fuori da
/// [`MIN_DISK_MIB`]..=[`MAX_DISK_MIB`].
pub fn create_disk(
    qemu_img: &ManagedPath,
    disk: &ManagedPath,
    format: DiskFormat,
    size_mib: u64,
    timeout: Duration,
) -> Result<()> {
    if !(MIN_DISK_MIB..=MAX_DISK_MIB).contains(&size_mib) {
        return Err(BackendError::ResourceLimit(format!(
            "disco di {size_mib} MiB fuori dall'intervallo {MIN_DISK_MIB}..={MAX_DISK_MIB}"
        )));
    }
    refuse_existing("disk", disk)?;
    let size = format!("{size_mib}M");
    qemu_img_create(
        qemu_img,
        &[
            "create",
            "-q",
            "-f",
            format.driver(),
            disk.qemu_value(),
            &size,
        ],
        timeout,
    )
}

/// Dimensione virtuale in byte di un disco (`qemu-img info --output=json`,
/// formato esplicito: nessuna rilevazione automatica).
pub fn disk_virtual_size(
    qemu_img: &ManagedPath,
    disk: &ManagedPath,
    format: DiskFormat,
    timeout: Duration,
) -> Result<u64> {
    let o = run_capture(
        qemu_img.as_path(),
        &[
            "info",
            "-f",
            format.driver(),
            "--output=json",
            disk.qemu_value(),
        ],
        timeout,
    )?;
    if o.code != Some(0) {
        return Err(BackendError::Io(std::io::Error::other(format!(
            "qemu-img info uscito con {:?}: {}",
            o.code,
            stderr_excerpt(&o.stderr)
        ))));
    }
    let v: serde_json::Value = serde_json::from_str(&o.stdout)
        .map_err(|e| BackendError::Io(std::io::Error::other(format!("qemu-img info: {e}"))))?;
    v.get("virtual-size")
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            BackendError::Io(std::io::Error::other("qemu-img info: virtual-size assente"))
        })
}

/// Porta il disco a `size_mib` MiB (`qemu-img resize`). Solo ingrandimento:
/// una dimensione minore di quella attuale è rifiutata (perdita di dati nel
/// guest); una dimensione uguale non esegue nulla. Restituisce `true` se il
/// disco è stato ingrandito.
pub fn grow_disk(
    qemu_img: &ManagedPath,
    disk: &ManagedPath,
    format: DiskFormat,
    size_mib: u64,
    timeout: Duration,
) -> Result<bool> {
    if !(MIN_DISK_MIB..=MAX_DISK_MIB).contains(&size_mib) {
        return Err(BackendError::ResourceLimit(format!(
            "disco di {size_mib} MiB fuori dall'intervallo {MIN_DISK_MIB}..={MAX_DISK_MIB}"
        )));
    }
    let attuale = disk_virtual_size(qemu_img, disk, format, timeout)?;
    let richiesto = size_mib << 20;
    if richiesto < attuale {
        return Err(BackendError::invalid(
            "disk",
            "riduzione del disco non supportata: si può solo ingrandire",
        ));
    }
    if richiesto == attuale {
        return Ok(false);
    }
    let size = format!("{size_mib}M");
    let o = run_capture(
        qemu_img.as_path(),
        &[
            "resize",
            "-q",
            "-f",
            format.driver(),
            disk.qemu_value(),
            &size,
        ],
        timeout,
    )?;
    if o.code != Some(0) {
        return Err(BackendError::Io(std::io::Error::other(format!(
            "qemu-img resize uscito con {:?}: {}",
            o.code,
            stderr_excerpt(&o.stderr)
        ))));
    }
    Ok(true)
}

/// Crea un overlay qcow2 su `base` con formato del backing esplicito
/// (`-F`), senza rilevazione automatica. Il riferimento al backing è il
/// percorso assoluto di `base`.
pub fn create_overlay(
    qemu_img: &ManagedPath,
    base: &ManagedPath,
    base_format: DiskFormat,
    overlay: &ManagedPath,
    timeout: Duration,
) -> Result<()> {
    refuse_existing("overlay", overlay)?;
    if !base.as_path().is_file() {
        return Err(BackendError::invalid("base", "il disco base non esiste"));
    }
    qemu_img_create(
        qemu_img,
        &[
            "create",
            "-q",
            "-f",
            "qcow2",
            "-F",
            base_format.driver(),
            "-b",
            base.qemu_value(),
            overlay.qemu_value(),
        ],
        timeout,
    )
}

/// Copia per esecuzione delle variabili UEFI dal modello (pflash1). Rifiuta
/// una destinazione esistente e verifica la dimensione della copia.
pub fn copy_firmware_vars(template: &ManagedPath, dest: &ManagedPath) -> Result<u64> {
    refuse_existing("firmware_vars", dest)?;
    let expected = std::fs::metadata(template.as_path())?.len();
    let copied = std::fs::copy(template.as_path(), dest.as_path())?;
    if copied != expected {
        let _ = std::fs::remove_file(dest.as_path());
        return Err(BackendError::Io(std::io::Error::other(
            "copia delle variabili UEFI incompleta",
        )));
    }
    Ok(copied)
}

/// Esito della preparazione delle directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunDirs {
    /// SID dell'utente a cui è riservata la directory privata.
    pub owner_sid: String,
    /// Voci dell'ACL della directory privata dopo la restrizione (attesa 1).
    pub private_acl_entries: usize,
}

fn system32(exe: &str) -> std::path::PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    Path::new(&root).join("System32").join(exe)
}

/// SID dell'utente corrente (`whoami /user`), forma `S-1-...`.
fn current_user_sid() -> Result<String> {
    let o = run_capture(
        &system32("whoami.exe"),
        &["/user", "/fo", "csv", "/nh"],
        Duration::from_secs(10),
    )?;
    o.stdout
        .split(',')
        .map(|f| f.trim().trim_matches('"'))
        .find(|f| f.starts_with("S-1-"))
        .map(str::to_owned)
        .ok_or_else(|| BackendError::Io(std::io::Error::other("SID dell'utente non rilevato")))
}

/// Crea la directory dell'esecuzione e la directory privata dei socket QMP/VNC
/// e degli screendump; alla directory privata toglie l'ereditarietà e
/// concede il controllo completo al solo utente corrente (per SID), come
/// verificato in F0-02 §4.3, senza elevazione. Restituisce il SID e il numero
/// di voci dell'ACL risultante; con più di una voce l'operazione fallisce.
///
/// Usa `icacls.exe` di `%SystemRoot%\System32` (nessuna API `unsafe`
/// aggiuntiva). Il rifiuto di un altro utente non è provato qui (F0-02: nessun
/// secondo account disponibile).
pub fn prepare_run_dirs(run_dir: &ManagedPath, private_dir: &ManagedPath) -> Result<RunDirs> {
    std::fs::create_dir_all(run_dir.as_path())?;
    std::fs::create_dir_all(private_dir.as_path())?;
    if !cfg!(windows) {
        return Err(BackendError::Unsupported {
            what: "ACL della directory privata su questo sistema".into(),
            hint: "su Linux usare permessi 0700 (non ancora implementato)".into(),
        });
    }
    let sid = current_user_sid()?;
    let grant = format!("*{sid}:(OI)(CI)F");
    let st = Command::new(system32("icacls.exe"))
        .arg(private_dir.as_path())
        .args(["/inheritance:r", "/grant:r", &grant])
        .env_clear()
        .envs(crate::process::minimal_environment())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(BackendError::Spawn)?;
    if !st.success() {
        return Err(BackendError::Io(std::io::Error::other(format!(
            "icacls sulla directory privata uscito con {:?}",
            st.code()
        ))));
    }
    let entries = acl_entries(private_dir.as_path())?;
    if entries != 1 {
        return Err(BackendError::StateConflict(format!(
            "ACL della directory privata con {entries} voci (attesa 1)"
        )));
    }
    Ok(RunDirs {
        owner_sid: sid,
        private_acl_entries: entries,
    })
}

/// Numero di voci ACE riportate da `icacls <dir>` (righe con `:(`).
fn acl_entries(dir: &Path) -> Result<usize> {
    let dir_s = dir
        .to_str()
        .ok_or_else(|| BackendError::invalid("path", "percorso non UTF-8"))?;
    let o = run_capture(&system32("icacls.exe"), &[dir_s], Duration::from_secs(10))?;
    Ok(o.stdout.lines().filter(|l| l.contains(":(")).count())
}

/// Risorse misurate dell'host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostResources {
    /// Memoria fisica totale in byte.
    pub total_memory_bytes: u64,
    /// Memoria fisica disponibile in byte al momento della misura.
    pub available_memory_bytes: u64,
    /// Spazio libero disponibile all'utente sul volume della directory indicata.
    pub free_disk_bytes: u64,
}

/// Misura memoria fisica e spazio libero sul volume di `storage_dir`.
pub fn host_resources(storage_dir: &ManagedPath) -> Result<HostResources> {
    #[cfg(windows)]
    {
        let (total, avail) = crate::sys::windows::physical_memory()?;
        let free = crate::sys::windows::free_disk_bytes(storage_dir.as_path())?;
        Ok(HostResources {
            total_memory_bytes: total,
            available_memory_bytes: avail,
            free_disk_bytes: free,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = storage_dir;
        Err(BackendError::Unsupported {
            what: "misura delle risorse dell'host su questo sistema".into(),
            hint: "implementazione Linux non ancora presente".into(),
        })
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn tmp_dir(name: &str) -> ManagedPath {
        let d = std::env::temp_dir()
            .join("lightvm-qemu-backend-tests")
            .join(format!("{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        ManagedPath::verify(d).unwrap()
    }

    #[test]
    fn directory_privata_solo_utente() {
        let base = tmp_dir("dirs");
        let run = base.join_file("run").unwrap();
        let p = base.join_file("priv").unwrap();
        let r = prepare_run_dirs(&run, &p).unwrap();
        assert!(r.owner_sid.starts_with("S-1-"));
        assert_eq!(r.private_acl_entries, 1);
        assert!(run.as_path().is_dir() && p.as_path().is_dir());
        let _ = std::fs::remove_dir_all(base.as_path());
    }

    #[test]
    fn copia_vars_senza_sovrascrivere() {
        let base = tmp_dir("vars");
        std::fs::create_dir_all(base.as_path()).unwrap();
        let t = base.join_file("template.fd").unwrap();
        std::fs::write(t.as_path(), vec![7u8; 4096]).unwrap();
        let d = base.join_file("vars.fd").unwrap();
        assert_eq!(copy_firmware_vars(&t, &d).unwrap(), 4096);
        assert!(matches!(
            copy_firmware_vars(&t, &d),
            Err(BackendError::StateConflict(_))
        ));
        let _ = std::fs::remove_dir_all(base.as_path());
    }

    #[test]
    fn risorse_misurate() {
        let d = std::env::temp_dir().join("lightvm-qemu-backend-tests");
        std::fs::create_dir_all(&d).unwrap();
        let r = host_resources(&ManagedPath::verify(d).unwrap()).unwrap();
        assert!(r.total_memory_bytes > 0);
        assert!(r.available_memory_bytes <= r.total_memory_bytes);
        assert!(r.free_disk_bytes > 0);
    }

    #[test]
    fn dimensione_disco_validata() {
        let base = tmp_dir("disk");
        let img = base.join_file("qemu-img.exe").unwrap();
        let d = base.join_file("d.qcow2").unwrap();
        for size in [0, MIN_DISK_MIB - 1, MAX_DISK_MIB + 1] {
            assert!(matches!(
                create_disk(&img, &d, DiskFormat::Qcow2, size, QEMU_IMG_TIMEOUT),
                Err(BackendError::ResourceLimit(_))
            ));
        }
    }
}
