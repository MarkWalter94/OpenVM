//! Registro dei media approvati (ROADMAP 6.3, 7.4 riga "File"; ADR-0008).
//!
//! Un percorso scelto dall'utente non autorizza qualunque file lo sostituisca
//! in seguito: all'approvazione il catalogo produce una **copia gestita in sola
//! lettura** in `media/<media_id>.iso` e ne registra SHA-256 e dimensione.
//! QEMU riceve soltanto la copia gestita.
//!
//! Procedura di [`Catalog::approve_media`]:
//!
//! 1. la sorgente è aperta una sola volta in lettura senza seguire un reparse
//!    point sull'ultimo componente (Windows: `FILE_FLAG_OPEN_REPARSE_POINT`) e
//!    con il solo `FILE_SHARE_READ`, così nessuno può modificarla durante la
//!    copia; il tipo (file regolare, non collegamento) è verificato sui
//!    metadati dell'handle;
//! 2. un intento `import_media` del journal riserva il temporaneo in `tmp/`,
//!    creato con `CREATE_NEW` e share mode esclusiva;
//! 3. copia e SHA-256 sono calcolati in un'unica lettura dallo stesso handle:
//!    l'hash registrato è quello dei byte copiati, non di un file riaperto;
//! 4. pubblicazione senza sovrascrittura (collegamento fisico) e attributo di
//!    sola lettura; riga `media` e conferma dell'intento nella stessa
//!    transazione.
//!
//! Limiti: l'identità OS del file (numero di serie del volume e indice del
//! file, `FileIdentity` di `lightvm-proto`) non è leggibile con le API stabili
//! di `std` su Windows senza codice `unsafe`: non è registrata. L'attributo di
//! sola lettura è rimovibile dallo stesso utente; [`Catalog::verify_media`]
//! ricalcola l'hash attraverso un handle con il solo `FILE_SHARE_READ` prima
//! dell'uso.

use std::io::{Read, Write};
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::catalog::{Catalog, IntentSpec};
use crate::enums::{IntentKind, MediaKind, MediaStatus, TargetKind};
use crate::error::{Result, StorageError};
use crate::fsx::{self, Share};
use crate::ids::MediaId;
use crate::layout::CatalogLayout;
use crate::records::MediaRecord;
use crate::time::now_rfc3339;

/// Lunghezza massima dell'origine dichiarata (testo libero, mai un percorso da aprire).
pub const MAX_DECLARED_ORIGIN: usize = 512;

/// Esito di [`Catalog::verify_media`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaCheck {
    /// Voce del registro verificata.
    pub record: MediaRecord,
    /// L'attributo di sola lettura mancava ed è stato ripristinato.
    pub readonly_restored: bool,
}

impl Catalog {
    /// Approva un supporto: copia gestita in sola lettura, hash e riga del
    /// registro. `source` è il file scelto dall'utente con il selettore nativo
    /// (mai un percorso ricevuto da un client). Nessuna sovrascrittura: la
    /// copia nasce con `CREATE_NEW` e viene pubblicata con un collegamento
    /// fisico che fallisce se la destinazione esiste.
    pub fn approve_media(
        &mut self,
        source: &Path,
        kind: MediaKind,
        declared_origin: &str,
    ) -> Result<MediaRecord> {
        if declared_origin.len() > MAX_DECLARED_ORIGIN
            || declared_origin.chars().any(char::is_control)
        {
            return Err(StorageError::Constraint(
                "origine dichiarata troppo lunga o con caratteri di controllo".into(),
            ));
        }
        let label = "sorgente del media";
        let mut src = fsx::open_regular(source, Share::ReadOnly, label)?;
        let expected_len = src
            .metadata()
            .map_err(|e| StorageError::io(source, e))?
            .len();
        let media_id = MediaId::new_v7();
        let final_rel = CatalogLayout::media_file(&media_id, "iso")?;
        let intent = self.begin_intent(IntentSpec {
            kind: IntentKind::ImportMedia,
            vm_id: None,
            operation_id: None,
            target_kind: TargetKind::Media,
            target_id: Some(media_id.to_string()),
            final_path: Some(final_rel.clone()),
            read_only: true,
        })?;
        let result = (|| -> Result<MediaRecord> {
            let (sha256, size) = {
                let mut dst = fsx::create_new_exclusive(&intent.temp_path)?;
                let mut h = Sha256::new();
                let mut buf = vec![0u8; 1 << 16];
                let mut total = 0u64;
                loop {
                    let n = src
                        .read(&mut buf)
                        .map_err(|e| StorageError::io(source, e))?;
                    if n == 0 {
                        break;
                    }
                    h.update(&buf[..n]);
                    dst.write_all(&buf[..n])
                        .map_err(|e| StorageError::io(&intent.temp_path, e))?;
                    total = total.saturating_add(n as u64);
                }
                dst.sync_all()
                    .map_err(|e| StorageError::io(&intent.temp_path, e))?;
                (crate::hex(&h.finalize()), total)
            };
            if size != expected_len {
                return Err(StorageError::IntegrityMismatch {
                    path: label.into(),
                    detail: format!("letti {size} byte, attesi {expected_len}"),
                });
            }
            drop(src);
            self.mark_applied(&intent)?;
            let record = MediaRecord {
                media_id,
                sha256,
                size_bytes: size,
                kind,
                declared_origin: declared_origin.to_owned(),
                approved_at: now_rfc3339(),
                managed_path: final_rel.clone(),
                status: MediaStatus::Approved,
            };
            self.commit(&intent, |tx| tx.insert_media(&record))?;
            Ok(record)
        })();
        if result.is_err() {
            // Un annullamento non riuscito lascia l'intento aperto: lo risolve
            // la riconciliazione al prossimo avvio.
            let _ = self.abort(&intent);
        }
        result
    }

    /// Verifica una copia gestita prima dell'uso: apertura con il solo
    /// `FILE_SHARE_READ` senza seguire collegamenti, SHA-256 e dimensione
    /// ricalcolati attraverso l'handle, attributo di sola lettura ripristinato
    /// se manca. Copia assente: media `missing` e [`StorageError::NotFound`].
    pub fn verify_media(&mut self, id: &MediaId) -> Result<MediaCheck> {
        let record = self.read(|tx| tx.media(id))?;
        if record.status != MediaStatus::Approved {
            return Err(StorageError::StateConflict(format!(
                "media {id} non approvato ({})",
                record.status
            )));
        }
        let abs = self.layout.resolve(&record.managed_path)?;
        if !crate::catalog::exists_no_follow(&abs)? {
            let id_s = id.to_string();
            self.with_tx(|tx| {
                tx.conn()
                    .execute(
                        "UPDATE media SET status = 'missing' WHERE media_id = ?1",
                        rusqlite::params![id_s],
                    )
                    .map_err(crate::error::map_sqlite)?;
                Ok(())
            })?;
            return Err(StorageError::NotFound {
                kind: "copia gestita del media",
                id: record.managed_path.to_string(),
            });
        }
        let file = fsx::open_regular(&abs, Share::ReadOnly, record.managed_path.as_str())?;
        let (sha, size) = fsx::sha256_handle(&file, &abs)?;
        if sha != record.sha256 || size != record.size_bytes {
            return Err(StorageError::IntegrityMismatch {
                path: record.managed_path.to_string(),
                detail: format!(
                    "atteso {} ({} byte), trovato {sha} ({size} byte)",
                    record.sha256, record.size_bytes
                ),
            });
        }
        let readonly_restored = fsx::ensure_readonly(&abs)?;
        drop(file);
        Ok(MediaCheck {
            record,
            readonly_restored,
        })
    }
}
