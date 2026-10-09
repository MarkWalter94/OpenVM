//! Percorsi gestiti: l'unica fonte di percorsi per il comando QEMU.
//!
//! Il backend non accetta stringhe di percorso dal chiamante. Ogni percorso
//! arriva come [`ManagedPath`], costruibile soltanto tramite
//! [`ManagedPath::verify`], e i percorsi di un'esecuzione sono restituiti da un
//! oggetto che implementa [`ManagedPaths`] (in produzione il registro di
//! `vm-storage`/`lightvm-service`, che crea directory e copie gestite e
//! protegge creazione/apertura da symlink, junction e sostituzioni: ROADMAP 6.3).

use std::path::{Path, PathBuf};

use crate::compat::{MediaId, RunId};
use crate::error::{BackendError, Result};
use crate::validate::{UNIX_SOCKET_PATH_MAX, check_token, qemu_path_value};

/// Percorso assoluto verificato e rappresentabile senza ambiguità nelle
/// opzioni QEMU.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ManagedPath {
    path: PathBuf,
    qemu: String,
}

impl ManagedPath {
    /// Verifica un percorso:
    /// - UTF-8, assoluto, senza componenti `.`/`..`, senza prefissi UNC,
    ///   `\\?\` o `\\.\` (vedi [`crate::validate::qemu_path_value`]);
    /// - nessun carattere che QEMU interpreterebbe (`,`, `=`, `:` oltre l'unità,
    ///   controllo, virgolette, metacaratteri);
    /// - se esiste, non è un collegamento simbolico né una junction.
    ///
    /// Le verifiche contro sostituzioni tra controllo e uso spettano al
    /// proprietario della directory (registro storage); qui si verifica la
    /// forma e l'ultimo componente.
    pub fn verify(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let s = path
            .to_str()
            .ok_or_else(|| BackendError::invalid("path", "percorso non UTF-8"))?;
        let qemu = qemu_path_value("path", s)?;
        if let Ok(meta) = std::fs::symlink_metadata(&path)
            && meta.file_type().is_symlink()
        {
            return Err(BackendError::invalid(
                "path",
                "collegamento simbolico o junction non ammesso",
            ));
        }
        Ok(Self { path, qemu })
    }

    /// Aggiunge un nome di file (token) e verifica il risultato.
    pub fn join_file(&self, name: &str) -> Result<Self> {
        check_token("file_name", name)?;
        Self::verify(self.path.join(name))
    }

    /// Percorso per le API del sistema operativo.
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// Forma usata nelle opzioni QEMU (separatori `/`).
    pub fn qemu_value(&self) -> &str {
        &self.qemu
    }

    /// Verifica che il percorso sia utilizzabile come socket Unix (lunghezza
    /// di `sun_path`).
    pub(crate) fn check_socket_len(&self, field: &'static str) -> Result<()> {
        if self.qemu.len() > UNIX_SOCKET_PATH_MAX {
            return Err(BackendError::invalid(
                field,
                format!(
                    "percorso del socket di {} byte (massimo {UNIX_SOCKET_PATH_MAX})",
                    self.qemu.len()
                ),
            ));
        }
        Ok(())
    }
}

/// Percorsi di un'esecuzione, già verificati dal proprietario dello storage.
///
/// Ogni metodo restituisce percorsi assoluti in directory controllate dal
/// supervisor; il backend non compone percorsi a partire da testo del
/// chiamante, salvo nomi di file fissi dentro le directory restituite.
pub trait ManagedPaths: Send + Sync {
    /// Eseguibile `qemu-system-x86_64` approvato.
    fn qemu_system(&self) -> Result<ManagedPath>;

    /// Codice del firmware UEFI (aperto in sola lettura come pflash0).
    fn firmware_code(&self) -> Result<ManagedPath>;

    /// Copia per esecuzione delle variabili UEFI (pflash1, scrivibile).
    fn firmware_vars(&self, run: &RunId) -> Result<ManagedPath>;

    /// Disco della VM per questa esecuzione (overlay), se presente.
    fn disk(&self, run: &RunId) -> Result<Option<ManagedPath>>;

    /// File da tenere aperti in sola lettura con esclusione della scrittura
    /// per tutta l'esecuzione (base dell'overlay, variabili della baseline).
    fn protected_readonly(&self, run: &RunId) -> Result<Vec<ManagedPath>>;

    /// Supporto approvato del registro media (copia gestita in sola lettura).
    fn media(&self, media: &MediaId) -> Result<ManagedPath>;

    /// Directory dell'esecuzione (log di stdout/stderr).
    fn run_dir(&self, run: &RunId) -> Result<ManagedPath>;

    /// Directory privata dell'esecuzione (ACL limitata all'utente del
    /// supervisor): socket QMP e VNC, file temporanei di `screendump`.
    fn private_dir(&self, run: &RunId) -> Result<ManagedPath>;
}

/// Nomi fissi dei file creati dal backend nelle directory gestite.
pub(crate) mod names {
    pub(crate) const QMP_SOCKET: &str = "qmp.sock";
    pub(crate) const VNC_SOCKET: &str = "vnc.sock";
    pub(crate) const STDOUT: &str = "qemu.stdout.log";
    pub(crate) const STDERR: &str = "qemu.stderr.log";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn verifica_e_join() {
        let p = ManagedPath::verify(r"C:\Windows\Temp\lightvm-test-dir").unwrap();
        assert_eq!(p.qemu_value(), "C:/Windows/Temp/lightvm-test-dir");
        assert!(p.join_file("qmp.sock").is_ok());
        assert!(p.join_file("..").is_err());
        assert!(p.join_file("a/b").is_err());
        assert!(p.join_file("a,b").is_err());
        assert!(ManagedPath::verify(r"relative").is_err());
        let long = format!(r"C:\{}\qmp.sock", "a".repeat(120));
        assert!(
            ManagedPath::verify(&long)
                .unwrap()
                .check_socket_len("qmp")
                .is_err()
        );
    }
}
