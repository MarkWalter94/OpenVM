//! Esclusione dei file della VM indipendente dal lock di QEMU.
//!
//! Su Windows il lock delle immagini di QEMU 11.1.0 non protegge: QEMU apre
//! base e overlay con `FILE_SHARE_WRITE` (F0-02 8.3, F0-05 7.1). Il backend
//! tiene quindi aperti per tutta l'esecuzione:
//!
//! - i file da proteggere in sola lettura (base dell'overlay, variabili UEFI
//!   della baseline) con accesso in lettura e share mode `FILE_SHARE_READ`:
//!   nessun altro processo può aprirli in scrittura, rinominarli o
//!   cancellarli; se un altro processo li ha già aperti in scrittura,
//!   l'apertura fallisce (errore 32) e l'avvio è rifiutato con `vm_busy`
//!   (F0-05 7.2, 7.4);
//! - l'overlay e le variabili dell'esecuzione con share
//!   `FILE_SHARE_READ | FILE_SHARE_WRITE` e senza `FILE_SHARE_DELETE`: impedisce
//!   rinomina e cancellazione durante l'uso. Non impedisce la scrittura di un
//!   terzo processo dello stesso utente (F0-05 7.3, limite dichiarato).
//!
//! Limiti: protezione contro errori e strumenti concorrenti, non un confine di
//! sicurezza; cessa con la chiusura degli handle (fine esecuzione o morte del
//! supervisor, nel qual caso il Job Object termina anche QEMU).
//! Su sistemi non Windows i file sono aperti senza esclusione (TODO(F1/R2a):
//! lock consultivo e permessi, da verificare su Linux).

use std::fs::{File, OpenOptions};

use crate::error::{BackendError, Result};
use crate::paths::ManagedPath;

/// Handle tenuti aperti per la durata di un'esecuzione.
#[derive(Debug, Default)]
pub struct StorageGuards {
    readonly: Vec<(ManagedPath, File)>,
    no_delete: Vec<(ManagedPath, File)>,
}

impl StorageGuards {
    /// Apre i file da proteggere. In caso di errore nessun handle resta aperto.
    pub fn acquire(readonly: &[ManagedPath], no_delete: &[ManagedPath]) -> Result<Self> {
        let mut g = Self::default();
        for p in readonly {
            g.readonly.push((p.clone(), open_readonly_exclusive(p)?));
        }
        for p in no_delete {
            g.no_delete.push((p.clone(), open_no_delete(p)?));
        }
        Ok(g)
    }

    /// File protetti in sola lettura.
    pub fn readonly_paths(&self) -> impl Iterator<Item = &ManagedPath> {
        self.readonly.iter().map(|(p, _)| p)
    }

    /// File protetti da rinomina/cancellazione.
    pub fn no_delete_paths(&self) -> impl Iterator<Item = &ManagedPath> {
        self.no_delete.iter().map(|(p, _)| p)
    }
}

fn map_open_error(p: &ManagedPath, e: std::io::Error) -> BackendError {
    #[cfg(windows)]
    if e.raw_os_error() == Some(crate::sys::windows::ERROR_SHARING_VIOLATION) {
        return BackendError::VmBusy(format!(
            "`{}` è aperto da un altro processo in modo incompatibile",
            p.as_path().display()
        ));
    }
    BackendError::Io(std::io::Error::new(
        e.kind(),
        format!("apertura di `{}`: {e}", p.as_path().display()),
    ))
}

/// Apre in lettura con share mode `FILE_SHARE_READ`.
pub fn open_readonly_exclusive(p: &ManagedPath) -> Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(crate::sys::windows::FILE_SHARE_READ);
    }
    o.open(p.as_path()).map_err(|e| map_open_error(p, e))
}

/// Apre in lettura con share mode `FILE_SHARE_READ | FILE_SHARE_WRITE`
/// (senza `FILE_SHARE_DELETE`).
pub fn open_no_delete(p: &ManagedPath) -> Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(crate::sys::windows::FILE_SHARE_READ | crate::sys::windows::FILE_SHARE_WRITE);
    }
    o.open(p.as_path()).map_err(|e| map_open_error(p, e))
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    fn tmp(name: &str) -> ManagedPath {
        let dir = std::env::temp_dir().join("lightvm-qemu-backend-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let p = ManagedPath::verify(dir.join(name)).unwrap();
        std::fs::write(p.as_path(), b"base").unwrap();
        p
    }

    #[test]
    fn base_protetta_da_scrittura_e_cancellazione() {
        let p = tmp("guard-base.bin");
        let g = StorageGuards::acquire(std::slice::from_ref(&p), &[]).unwrap();
        // lettura consentita
        assert_eq!(std::fs::read(p.as_path()).unwrap(), b"base");
        // scrittura, cancellazione e rinomina rifiutate
        assert!(OpenOptions::new().write(true).open(p.as_path()).is_err());
        assert!(std::fs::remove_file(p.as_path()).is_err());
        assert!(std::fs::rename(p.as_path(), p.as_path().with_extension("x")).is_err());
        drop(g);
        std::fs::remove_file(p.as_path()).unwrap();
    }

    #[test]
    fn scrittore_preesistente_rilevato() {
        let p = tmp("guard-writer.bin");
        let w = OpenOptions::new().write(true).open(p.as_path()).unwrap();
        let r = StorageGuards::acquire(std::slice::from_ref(&p), &[]);
        assert!(matches!(r, Err(BackendError::VmBusy(_))), "{r:?}");
        drop(w);
        std::fs::remove_file(p.as_path()).unwrap();
    }

    #[test]
    fn overlay_non_cancellabile_ma_scrivibile() {
        let p = tmp("guard-overlay.bin");
        let g = StorageGuards::acquire(&[], std::slice::from_ref(&p)).unwrap();
        assert!(OpenOptions::new().write(true).open(p.as_path()).is_ok());
        assert!(std::fs::remove_file(p.as_path()).is_err());
        drop(g);
        std::fs::remove_file(p.as_path()).unwrap();
    }
}
