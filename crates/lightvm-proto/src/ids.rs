//! Identificativi tipizzati (ROADMAP 6.3, 9).
//!
//! Ogni identificativo è un newtype su un UUID versione 7 (RFC 9562): 128 bit,
//! ordinabile per istante di creazione, rappresentato in JSON come stringa
//! esadecimale con trattini in minuscolo. Tipi distinti impediscono di passare un
//! `RunId` dove serve un `OperationId`.
//!
//! Gli identificativi **non sono credenziali**: un `SessionId` può comparire in
//! log e report; il segreto di sessione è [`crate::SessionToken`]. Un UUID v7
//! contiene l'istante di creazione in millisecondi: non va usato per dati che
//! debbano restare riservati.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

/// Errore di analisi di un identificativo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdParseError {
    /// La stringa non è un UUID in forma testuale valida.
    Malformed,
    /// L'UUID è valido ma non è della versione 7 con variante RFC 9562.
    NotVersion7,
}

impl fmt::Display for IdParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed => f.write_str("identificativo non valido"),
            Self::NotVersion7 => f.write_str("identificativo non UUID v7"),
        }
    }
}

impl std::error::Error for IdParseError {}

fn check_v7(uuid: Uuid) -> Result<Uuid, IdParseError> {
    if uuid.get_version_num() == 7 && uuid.get_variant() == uuid::Variant::RFC4122 {
        Ok(uuid)
    } else {
        Err(IdParseError::NotVersion7)
    }
}

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(Uuid);

        impl $name {
            /// Nuovo identificativo dall'orologio di sistema e dal generatore
            /// casuale dell'OS. Riservato al supervisor: il core usa la porta
            /// `IdGenerator`.
            #[cfg(feature = "system-clock")]
            #[must_use]
            pub fn new_v7() -> Self {
                Self(Uuid::now_v7())
            }

            /// Costruzione deterministica da millisecondi Unix e 10 byte di
            /// contatore/casualità (per generatori iniettati e test).
            #[must_use]
            pub const fn from_unix_millis(millis: u64, counter_random: [u8; 10]) -> Self {
                Self(uuid::Builder::from_unix_timestamp_millis(millis, &counter_random).into_uuid())
            }

            /// Converte un UUID esistente; rifiuta UUID che non siano v7.
            pub fn from_uuid(uuid: Uuid) -> Result<Self, IdParseError> {
                check_v7(uuid).map(Self)
            }

            /// UUID sottostante.
            #[must_use]
            pub const fn as_uuid(&self) -> Uuid {
                self.0
            }

            /// Byte in ordine di rete (per colonne BLOB di 16 byte).
            #[must_use]
            pub const fn to_bytes(&self) -> [u8; 16] {
                *self.0.as_bytes()
            }

            /// Ricostruzione dai 16 byte; rifiuta UUID che non siano v7.
            pub fn from_bytes(bytes: [u8; 16]) -> Result<Self, IdParseError> {
                Self::from_uuid(Uuid::from_bytes(bytes))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0.hyphenated(), f)
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                // Solo la forma con trattini (36 caratteri): niente graffe, URN o forma compatta.
                if s.len() != 36 {
                    return Err(IdParseError::Malformed);
                }
                let uuid = Uuid::try_parse(s).map_err(|_| IdParseError::Malformed)?;
                Self::from_uuid(uuid)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(&self.0.hyphenated())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let s = Cow::<'de, str>::deserialize(deserializer)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                Cow::Borrowed(stringify!($name))
            }
            fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
                json_schema!({
                    "type": "string",
                    "format": "uuid",
                    "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$",
                    "description": concat!(stringify!($name), ": UUID versione 7 in minuscolo con trattini.")
                })
            }
        }
    };
}

define_id!(
    /// Identificativo stabile di una VM del catalogo.
    VmId
);
define_id!(
    /// Identificativo di una singola esecuzione (un processo QEMU). Le richieste
    /// riferite a un `RunId` precedente sono rifiutate (ROADMAP 6.3).
    RunId
);
define_id!(
    /// Identificativo di un effetto richiesto: chiave di idempotenza della coda per VM.
    OperationId
);
define_id!(
    /// Identificativo di una sessione agente. Non è un segreto.
    SessionId
);
define_id!(
    /// Identificativo di una baseline a VM spenta (ROADMAP 8).
    BaselineId
);
define_id!(
    /// Identificativo di un supporto del registro media approvato (ADR-0008).
    MediaId
);
define_id!(
    /// Identificativo di un grant emesso dal supervisor (ROADMAP 9).
    GrantId
);
