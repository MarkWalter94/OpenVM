//! Validazione della grammatica delle opzioni QEMU (ROADMAP 6.3).
//!
//! Lo spawn senza shell non basta: QEMU interpreta a sua volta i valori delle
//! opzioni `chiave=valore,chiave=valore` (QemuOpts/keyval) e i prefissi come
//! `unix:`. Regole applicate:
//!
//! - i valori testuali che entrano in un'opzione (nome macchina, modello CPU,
//!   identificativi) sono **token** di un alfabeto chiuso `[A-Za-z0-9._-]`, non
//!   vuoti, lunghi al massimo [`TOKEN_MAX_LEN`] e mai inizianti con `-`;
//! - qualunque valore che entra in un'opzione separata da virgole o `=` viene
//!   rifiutato se contiene `,`, `=`, `:` o caratteri di controllo
//!   ([`check_option_value`]);
//! - i percorsi seguono [`qemu_path_value`]: assoluti, separatori `/`, l'unico
//!   `:` ammesso è quello della lettera di unità su Windows.

use crate::error::{BackendError, Result};

/// Lunghezza massima di un token.
pub const TOKEN_MAX_LEN: usize = 64;

/// Verifica che `s` sia un token: `[A-Za-z0-9._-]{1,64}`, primo carattere non `-`.
pub fn check_token(field: &'static str, s: &str) -> Result<()> {
    if s.is_empty() {
        return Err(BackendError::invalid(field, "valore vuoto"));
    }
    if s.len() > TOKEN_MAX_LEN {
        return Err(BackendError::invalid(
            field,
            format!("più di {TOKEN_MAX_LEN} caratteri"),
        ));
    }
    if s.starts_with('-') {
        return Err(BackendError::invalid(field, "inizia con `-`"));
    }
    if let Some(c) = s
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
    {
        return Err(BackendError::invalid(
            field,
            format!("carattere {c:?} non ammesso (alfabeto [A-Za-z0-9._-])"),
        ));
    }
    Ok(())
}

/// Rifiuta un valore destinato a un'opzione separata da virgole o `=` se
/// contiene `,`, `=`, `:` o caratteri di controllo.
pub fn check_option_value(field: &'static str, s: &str) -> Result<()> {
    if s.is_empty() {
        return Err(BackendError::invalid(field, "valore vuoto"));
    }
    if let Some(c) = s
        .chars()
        .find(|&c| matches!(c, ',' | '=' | ':') || c.is_control())
    {
        return Err(BackendError::invalid(
            field,
            format!("carattere {c:?} non ammesso nelle opzioni QEMU"),
        ));
    }
    Ok(())
}

/// Protocolli di blocco ammessi nel comando QEMU (ADR-0004, ADR-0010): solo
/// file locali.
pub const ALLOWED_BLOCK_PROTOCOLS: &[&str] = &["file"];

/// Driver di blocco remoti presenti nella build QEMU 11.1.0 per Windows
/// (F0-02 §3) e sempre rifiutati.
pub const REMOTE_BLOCK_PROTOCOLS: &[&str] = &["http", "https", "ftp", "ftps", "nfs", "ssh", "nbd"];

/// Verifica che `name` sia un protocollo di blocco ammesso e restituisce il
/// nome canonico. I driver remoti hanno un messaggio dedicato.
pub fn check_block_protocol(name: &str) -> Result<&'static str> {
    if let Some(p) = ALLOWED_BLOCK_PROTOCOLS.iter().find(|p| **p == name) {
        return Ok(p);
    }
    if REMOTE_BLOCK_PROTOCOLS.contains(&name) {
        return Err(BackendError::invalid(
            "block_protocol",
            format!("protocollo remoto `{name}` escluso dal profilo offline"),
        ));
    }
    Err(BackendError::invalid(
        "block_protocol",
        "protocollo di blocco non in allowlist (ammesso solo `file`)",
    ))
}

/// Lunghezza massima del percorso di un socket Unix (`sun_path`, 108 byte con
/// il terminatore su Windows e Linux).
pub const UNIX_SOCKET_PATH_MAX: usize = 107;

/// Caratteri vietati in un percorso passato a QEMU, oltre ai caratteri di
/// controllo: separatori di opzione, prefissi di protocollo, metacaratteri e
/// caratteri non validi nei nomi di file Windows.
const PATH_FORBIDDEN: &[char] = &[',', '=', ':', '"', '\'', '?', '*', '<', '>', '|', '\\'];

/// Converte un percorso assoluto nella forma usata nelle opzioni QEMU e la
/// valida. Su Windows: `X:/a/b` (lettera di unità, separatori `/`); altrove
/// `/a/b`. Rifiuta componenti vuoti, `.` e `..`, prefissi UNC o di dispositivo
/// e ogni carattere di [`PATH_FORBIDDEN`] fuori dalla lettera di unità.
pub fn qemu_path_value(field: &'static str, raw: &str) -> Result<String> {
    if raw.chars().any(char::is_control) {
        return Err(BackendError::invalid(
            field,
            "carattere di controllo nel percorso",
        ));
    }
    let unified = raw.replace('\\', "/");
    let (prefix, rest) = split_root(field, &unified)?;
    if rest.is_empty() {
        return Err(BackendError::invalid(field, "il percorso è una radice"));
    }
    for comp in rest.split('/') {
        match comp {
            "" => {
                return Err(BackendError::invalid(
                    field,
                    "componente vuoto (separatori doppi o finali)",
                ));
            }
            "." | ".." => {
                return Err(BackendError::invalid(
                    field,
                    "componenti `.` o `..` non ammessi",
                ));
            }
            _ => {}
        }
        if let Some(c) = comp.chars().find(|c| PATH_FORBIDDEN.contains(c)) {
            return Err(BackendError::invalid(
                field,
                format!("carattere {c:?} non ammesso nel percorso"),
            ));
        }
        if comp.ends_with(' ') || comp.ends_with('.') {
            return Err(BackendError::invalid(
                field,
                "componente che termina con spazio o punto",
            ));
        }
    }
    Ok(format!("{prefix}{rest}"))
}

#[cfg(windows)]
fn split_root<'a>(field: &'static str, p: &'a str) -> Result<(String, &'a str)> {
    let b = p.as_bytes();
    if p.starts_with("//") {
        return Err(BackendError::invalid(
            field,
            "percorsi UNC o di dispositivo non ammessi",
        ));
    }
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'/' {
        let letter = (b[0] as char).to_ascii_uppercase();
        return Ok((format!("{letter}:/"), &p[3..]));
    }
    Err(BackendError::invalid(
        field,
        "atteso un percorso assoluto con lettera di unità (X:\\...)",
    ))
}

#[cfg(not(windows))]
fn split_root<'a>(field: &'static str, p: &'a str) -> Result<(String, &'a str)> {
    if p.starts_with("//") {
        return Err(BackendError::invalid(field, "doppio separatore iniziale"));
    }
    match p.strip_prefix('/') {
        Some(rest) => Ok(("/".to_owned(), rest)),
        None => Err(BackendError::invalid(field, "atteso un percorso assoluto")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token() {
        for ok in ["q35", "pc-q35-11.1", "qemu64", "Skylake-Client-v4", "a_b.c"] {
            assert!(check_token("t", ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "-S",
            "a,b",
            "a=b",
            "a:b",
            "a b",
            "a\nb",
            "è",
            &"x".repeat(65),
        ] {
            assert!(check_token("t", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn protocolli_di_blocco() {
        assert_eq!(check_block_protocol("file").unwrap(), "file");
        for p in REMOTE_BLOCK_PROTOCOLS {
            let e = check_block_protocol(p).unwrap_err().to_string();
            assert!(e.contains("remoto"), "{p}: {e}");
        }
        for p in ["vvfat", "host_device", "FILE", "file,x", "", "iscsi"] {
            assert!(check_block_protocol(p).is_err(), "{p:?}");
        }
    }

    #[test]
    fn valore_opzione() {
        assert!(check_option_value("v", "abc").is_ok());
        for bad in ["a,b", "a=b", "a:b", "a\u{0}b", "a\tb", ""] {
            assert!(check_option_value("v", bad).is_err(), "{bad:?}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn percorsi_windows() {
        assert_eq!(
            qemu_path_value("p", r"e:\OpenVM\lab\vms\a b\x.qcow2").unwrap(),
            "E:/OpenVM/lab/vms/a b/x.qcow2"
        );
        for bad in [
            r"\\server\share\x",
            r"\\?\E:\x",
            r"\\.\PhysicalDrive0",
            r"relative\x",
            r"E:x",
            r"E:\a\..\b",
            r"E:\a\.\b",
            r"E:\a,b",
            r"E:\a=b",
            r"E:\a:b",
            r"E:\a\",
            r"E:\a\\b",
            "E:\\a\nb",
            r"E:\",
            r"E:\a.",
            r"E:\a\b ",
            r"E:\a?b",
        ] {
            assert!(qemu_path_value("p", bad).is_err(), "{bad:?}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn percorsi_unix() {
        assert_eq!(qemu_path_value("p", "/srv/a b/x").unwrap(), "/srv/a b/x");
        for bad in ["rel/x", "//x", "/a/../b", "/a,b", "/a:b", "/"] {
            assert!(qemu_path_value("p", bad).is_err(), "{bad:?}");
        }
    }
}
