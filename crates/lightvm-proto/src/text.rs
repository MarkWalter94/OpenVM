//! Stringhe validate: nomi, etichette, percorsi gestiti, digest.
//!
//! Nessun campo ricevuto da un client è interpretato come percorso o opzione host
//! (ROADMAP 6.3, 7.2 punto 5): i nomi sono etichette, i percorsi gestiti sono
//! relativi alla directory controllata dal supervisor e mai esposti ai client.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Errore di validazione di una stringa tipizzata. Il valore rifiutato non è
/// incluso: potrebbe contenere dati sensibili.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextError {
    /// Tipo atteso.
    pub kind: &'static str,
    /// Motivo stabile del rifiuto.
    pub reason: &'static str,
}

impl fmt::Display for TextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} non valido: {}", self.kind, self.reason)
    }
}

impl std::error::Error for TextError {}

macro_rules! validated_string {
    ($(#[$meta:meta])* $name:ident, $validate:path, $schema:tt) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);

        impl $name {
            /// Valida e costruisce.
            pub fn parse(value: impl Into<String>) -> Result<Self, TextError> {
                let value = value.into();
                $validate(&value).map_err(|reason| TextError { kind: stringify!($name), reason })?;
                Ok(Self(value))
            }

            /// Valore testuale.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = String::deserialize(deserializer)?;
                Self::parse(s).map_err(serde::de::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                Cow::Borrowed(stringify!($name))
            }
            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                json_schema!($schema)
            }
        }
    };
}

fn validate_vm_name(s: &str) -> Result<(), &'static str> {
    let len = s.chars().count();
    if len == 0 || len > 64 {
        return Err("lunghezza fuori da 1..=64");
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
    {
        return Err("caratteri ammessi: lettere e cifre ASCII, spazio, '-', '_', '.'");
    }
    if s.starts_with([' ', '.', '-']) || s.ends_with([' ', '.']) {
        return Err("non può iniziare con spazio, '.' o '-' né finire con spazio o '.'");
    }
    Ok(())
}

validated_string!(
    /// Nome visibile di una VM: etichetta, mai percorso. 1–64 caratteri ASCII
    /// `[A-Za-z0-9 ._-]`, senza spazio/punto iniziale o finale né `-` iniziale.
    VmName,
    validate_vm_name,
    {
        "type": "string",
        "minLength": 1,
        "maxLength": 64,
        "pattern": "^[A-Za-z0-9_][A-Za-z0-9 ._-]*$",
        "description": "Nome VM: etichetta ASCII, non un percorso."
    }
);

fn validate_label(s: &str) -> Result<(), &'static str> {
    let len = s.chars().count();
    if len == 0 || len > 256 {
        return Err("lunghezza fuori da 1..=256");
    }
    if s.chars().any(|c| c.is_control()) {
        return Err("caratteri di controllo non ammessi");
    }
    if s.trim() != s {
        return Err("spazi iniziali o finali non ammessi");
    }
    Ok(())
}

validated_string!(
    /// Etichetta di testo libero dichiarata dall'utente (per esempio l'origine di
    /// un supporto). Solo testo: mai interpretata come percorso, URL o markup.
    Label,
    validate_label,
    {
        "type": "string",
        "minLength": 1,
        "maxLength": 256,
        "description": "Testo dichiarato dall'utente, senza caratteri di controllo; mai interpretato."
    }
);

fn validate_qcode(s: &str) -> Result<(), &'static str> {
    if s.is_empty() || s.len() > 32 {
        return Err("lunghezza fuori da 1..=32");
    }
    if !s
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err("caratteri ammessi: [a-z0-9_]");
    }
    Ok(())
}

validated_string!(
    /// Nome di tasto QEMU (`QKeyCode`, per esempio `a`, `ret`, `shift`,
    /// `bracket_left`). Solo grammatica `[a-z0-9_]{1,32}`: il backend rifiuta i
    /// nomi non presenti nella propria tabella.
    QCode,
    validate_qcode,
    {
        "type": "string",
        "pattern": "^[a-z0-9_]{1,32}$",
        "description": "QKeyCode QEMU."
    }
);

const WINDOWS_RESERVED: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];

fn is_windows_reserved(component: &str) -> bool {
    let stem = component.split('.').next().unwrap_or(component);
    let upper = stem.to_ascii_uppercase();
    if WINDOWS_RESERVED.contains(&upper.as_str()) {
        return true;
    }
    let bytes = upper.as_bytes();
    bytes.len() == 4
        && (upper.starts_with("COM") || upper.starts_with("LPT"))
        && bytes[3].is_ascii_digit()
}

fn validate_managed_path(s: &str) -> Result<(), &'static str> {
    if s.is_empty() || s.len() > 255 {
        return Err("lunghezza fuori da 1..=255");
    }
    if s.contains('\\') {
        return Err("separatore '\\' non ammesso (UNC, device path)");
    }
    if s.contains(':') {
        return Err("':' non ammesso (unità, flussi alternativi)");
    }
    let components: Vec<&str> = s.split('/').collect();
    if components.len() > 4 {
        return Err("al massimo 4 componenti");
    }
    for c in components {
        if c.is_empty() {
            return Err("percorso assoluto o componente vuoto");
        }
        if c == "." || c == ".." {
            return Err("componenti '.' e '..' non ammessi");
        }
        if c.len() > 64 {
            return Err("componente oltre 64 byte");
        }
        if !c
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        {
            return Err("caratteri ammessi: [A-Za-z0-9._-]");
        }
        if c.starts_with('.') || c.ends_with('.') {
            return Err("componente con '.' iniziale o finale");
        }
        if is_windows_reserved(c) {
            return Err("nome di dispositivo riservato");
        }
    }
    Ok(())
}

validated_string!(
    /// Percorso **relativo** alla directory gestita dal supervisor, con separatore
    /// `/`. Rifiuta percorsi assoluti, unità, UNC, device path (`\\?\`, `\\.\`),
    /// `..`, flussi alternativi e nomi riservati Windows.
    ///
    /// Solo interno al supervisor e allo storage: non compare in alcun tipo
    /// esposto ai client né negli schemi pubblici. La validazione grammaticale non
    /// sostituisce l'apertura protetta contro symlink e junction (ROADMAP 6.3).
    ManagedRelPath,
    validate_managed_path,
    {
        "type": "string",
        "description": "Percorso relativo gestito, solo interno."
    }
);

/// Digest SHA-256 (32 byte). In JSON: 64 cifre esadecimali minuscole.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    /// Digest dai 32 byte.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Byte del digest.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Analizza 64 cifre esadecimali minuscole.
    pub fn parse_hex(s: &str) -> Result<Self, TextError> {
        let err = |reason| TextError {
            kind: "Sha256Digest",
            reason,
        };
        let b = s.as_bytes();
        if b.len() != 64 {
            return Err(err("servono 64 cifre esadecimali"));
        }
        let nibble = |c: u8| match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            _ => Err(err("cifre ammesse: [0-9a-f]")),
        };
        let mut out = [0u8; 32];
        for (i, pair) in b.as_chunks::<2>().0.iter().enumerate() {
            out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Ok(Self(out))
    }

    /// Forma esadecimale minuscola.
    #[must_use]
    pub fn to_hex(&self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut s = String::with_capacity(64);
        for byte in self.0 {
            s.push(char::from(HEX[usize::from(byte >> 4)]));
            s.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        s
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256Digest({})", self.to_hex())
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for Sha256Digest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Sha256Digest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = Cow::<'de, str>::deserialize(deserializer)?;
        Self::parse_hex(&s).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for Sha256Digest {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("Sha256Digest")
    }
    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^[0-9a-f]{64}$",
            "description": "SHA-256 in esadecimale minuscolo."
        })
    }
}
