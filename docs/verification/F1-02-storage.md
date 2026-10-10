# F1-02 — `vm-storage`: catalogo, migrazioni, journal, registro media, budget e protezione dei dischi

- Data di esecuzione (UTC): 2026-10-09
- Esecutore: agente di sviluppo (sessione Claude Code), su richiesta del responsabile del progetto
- Passo della roadmap: sezione 10, F1 ("migrazioni SQLite e tipi condivisi", "registro dei media approvati", "rifiuto di percorsi e risorse non validi"); sezioni 6.3 (registro media, SQLite + journal), 7.4 (righe "Host", "Storage", "Crash", "File"), 8 (baseline a VM spenta, budget storage); backlog `docs/RELEASE_PLAN.md` F1-01 (migrazioni), F1-04 (registro media), F1-09 (share mode sui dischi, parte storage)
- ADR collegati: ADR-0005, ADR-0008, ADR-0011, ADR-0017, ADR-0025 (non modificati)
- Esito complessivo: **parziale** — tutte le voci elencate sono **verificate con test sintetici** (file finti al posto di QCOW2, OVMF e ISO; nessun QEMU, nessun `qemu-img`) su Windows 11; la protezione con QEMU reale, Linux e la scelta sulla protezione dell'overlay in uso sono **non eseguite** o aperte (vedi Limiti e Punti aperti).

Esiti ammessi: **verificato**, **fallito**, **non eseguito** (sezione 17). In questo file "verificato" significa sempre "verificato con test sintetico automatico" (sezione 16 punto 5): nessuna voce sostituisce la prova con QEMU reale richiesta dal criterio di uscita di F1.

## Scopo

Portare `crates/vm-storage` allo stato richiesto da F1 per la parte storage:

1. identificativi di `lightvm-proto` al posto di quelli provvisori (rimozione del modulo `compat`);
2. migrazioni SQLite versionate con prova di aggiornamento da **ogni** versione precedente;
3. journal di intenti con riconciliazione all'avvio, compreso l'esito "da verificare" quando la riconciliazione non riesce;
4. lock di istanza singola sotto concorrenza;
5. percorsi gestiti con rifiuto di reparse point, junction e percorsi fuori dalla radice;
6. registro dei media approvati con copia gestita in sola lettura, SHA-256, `CREATE_NEW` e nessuna sovrascrittura;
7. manifest di budget (occupazione per catalogo, VM, baseline, esecuzione e limiti dichiarati);
8. protezione dei dischi (F1-09, parte storage): baseline e suo vars aperti con il solo `FILE_SHARE_READ` per la durata dell'esecuzione, verifica dell'hash della baseline prima di ogni avvio, rilevatore di scrittori inattesi, handle dell'overlay senza `FILE_SHARE_DELETE`.

Non dimostra: comportamento con QEMU 11.1.0 reale (le share mode con QEMU sono provate in `F0-05` §7, non qui); `qemu-img check`/`compare` sui file `.partial` (ADR-0025 punto 6, compito di `qemu-backend`); comportamento su Linux; consistenza di un filesystem guest; resistenza a un amministratore o a un processo malevolo dello stesso utente (sezione 7.1); integrazione con `lightvm-service` (altro flusso).

## Ambiente

| Voce | Valore |
| --- | --- |
| Host (modello, CPU, RAM, storage) | HP ZBook Fury 16 G9, Intel Core i7-12800HX, 32 GiB RAM; directory temporanee dei test su `C:` (NTFS), sorgenti su `E:` (NTFS) |
| OS e build | Windows 11 Pro 10.0.26200; utente non amministratore; modalità sviluppatore attiva (`AllowDevelopmentWithoutDevLicense = 1`: i collegamenti simbolici si creano senza privilegi, quindi i test sui symlink sono stati eseguiti e non saltati) |
| Acceleratore e stato | non usato |
| Toolchain / versioni rilevanti | `rustc 1.99.0 (b940084d7 2026-09-28)`, `cargo 1.99.0 (5f94df478 2026-08-27)`, target `x86_64-pc-windows-msvc` con ambiente Build Tools 2022 caricato da un wrapper (`vcvars64.bat`, come `F0-01` §4.1); `rusqlite =0.40.2` con `bundled`, `sha2 =0.10.9`, `uuid =1.27.0`, `tempfile =3.27.0` (dipendenze già presenti, nessuna aggiunta) |
| Binario QEMU | non applicabile |
| Firmware | non applicabile (vars finti di pochi byte) |
| Guest / media | non applicabile (ISO finte generate dai test) |
| Sessione grafica | non applicabile |
| Altre condizioni | antivirus e criteri di gruppo non modificati; nessuna elevazione |

## Procedura

Tutti i comandi sono eseguiti dalla radice `E:\OpenVM` attraverso il wrapper che carica l'ambiente MSVC.

```text
cargo fmt -p vm-storage -- --check
cargo clippy -p vm-storage --all-targets -- -D warnings
cargo test -p vm-storage
cargo test -p vm-storage -- --nocapture      # per controllare che nessun test sia stato saltato
cargo check --workspace --all-targets
```

Metodo dei test (sintetici, dichiarati):

- **Crash**: il `Catalog` viene rilasciato senza completare l'operazione e riaperto (la riapertura esegue `recover`). Per le API di alto livello il produttore dei file (in prodotto `qemu-img`) è una closure che scrive un file parziale e poi restituisce errore, oppure va in panico per saltare l'annullamento come farebbe un processo ucciso.
- **Altro processo**: simulato con un handle aperto nello stesso processo con share mode esplicita (`OpenOptionsExt::share_mode`). Per Windows la condivisione è una proprietà dell'apertura, quindi l'effetto sull'handle di guardia è lo stesso; la prova con processi distinti e QEMU reale è in `F0-05` §7.
- **Junction**: `cmd /C mklink /J` (nessun privilegio richiesto). **Symlink**: `std::os::windows::fs::symlink_file`.

## Risultati

Conteggio dei test di `vm-storage`: **50 prima** del lavoro, **77 dopo** (tutti superati, nessuno ignorato, nessuno saltato: l'output con `--nocapture` non contiene messaggi "prova saltata"). `cargo fmt --check`, `cargo clippy -D warnings` e `cargo check --workspace --all-targets` senza errori né avvisi. Il crate non contiene codice `unsafe` (`unsafe_code = deny` del workspace).

| File di test | Prima | Dopo |
| --- | --- | --- |
| unità (`src/`) | 5 | 5 |
| `budget_manifest.rs` | 4 | 6 |
| `concurrency.rs` (nuovo) | — | 3 |
| `constraints.rs` | 9 | 9 |
| `interruption.rs` (nuovo) | — | 6 |
| `journal.rs` | 20 | 20 |
| `media_registry.rs` (nuovo) | — | 5 |
| `migrations.rs` | 4 | 6 |
| `paths_lock.rs` | 8 | 9 |
| `protection.rs` (nuovo) | — | 8 |

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Identificativi di `lightvm-proto` (`VmId`, `RunId`, `OperationId`, `BaselineId`, `MediaId`, `SessionId`, `GrantId`); modulo `compat` e `TODO(F1)` rimossi | verificato (compilazione e test) | `src/ids.rs`; riesportati dalla radice del crate; nessun `TODO(F1)` nel crate |
| Migrazione 0003 (`needs_review` nel journal, tabella ricostruita) | verificato (sintetico) | `migrations.rs::needs_review_state_requires_the_latest_schema`: lo stato è rifiutato dal CHECK in v2 e accettato dopo la migrazione |
| Aggiornamento da **ogni** versione precedente (v1, v2 → v3) con backup datato, dati e journal conservati, sequenza `AUTOINCREMENT` proseguita, intento aperto riconciliato, schema finale identico a un catalogo nuovo | verificato (sintetico) | `migrations.rs::upgrade_from_every_previous_version_matches_a_fresh_schema` (confronto di `sqlite_master` normalizzato) |
| Rifiuto del downgrade e di una migrazione alterata | verificato (sintetico, test preesistenti) | `newer_schema_is_refused_without_changes`, `tampered_migration_is_detected` |
| Riconciliazione: intento irrisolvibile → `needs_review` ("da verificare"), VM `recovering` e occupata, apertura del catalogo non bloccata; nuovo tentativo riuscito dopo la rimozione della causa | verificato (sintetico, Windows) | `interruption.rs::unresolvable_intent_is_marked_needs_review_and_retried`: temporaneo tenuto aperto con share mode 0 → apertura riuscita, esito `NeedsReview`; rilasciato l'handle, `recover()` → `RolledBack` |
| Interruzione della promozione di una baseline durante la scrittura del disco | verificato (sintetico) | `promotion_interrupted_while_writing_the_disk_leaves_nothing`: nessun file, nessuna directory, nessuna riga, VM libera |
| Interruzione della promozione durante la scrittura del vars (disco già pubblicato) | verificato (sintetico) | `promotion_interrupted_while_writing_vars_removes_the_published_disk`: disco e directory rimossi, baseline precedente integra (hash e manifest) |
| Interruzione della preparazione di un'esecuzione (errore e crash con panico) | verificato (sintetico) | `preparation_interrupted_while_writing_the_overlay_leaves_nothing`, `interrupted_preparation_then_crash_is_reconciled_at_open` |
| Crash tra collegamento fisico e rimozione del temporaneo: completamento in avanti con base di nuovo in sola lettura | verificato (sintetico) | `crash_between_link_and_temp_removal_keeps_the_base_readonly` |
| Lock a istanza singola: 8 thread aprono lo stesso catalogo insieme → una sola istanza; grafie diverse (`.`, `sub\..`, maiuscole) rifiutate; cataloghi distinti coesistono | verificato (sintetico) | `concurrency.rs` (3 test) |
| Percorsi assoluti accettati solo sotto la radice; `..`, nomi riservati, radice vuota, junction verso l'esterno rifiutati | verificato (sintetico) | `paths_lock.rs::absolute_paths_are_accepted_only_inside_the_root` (`CatalogLayout::managed_from_absolute`) |
| Registro media: copia gestita in sola lettura, SHA-256 dei byte copiati, nessun temporaneo residuo; ISO sostituita dopo l'approvazione non cambia la copia | verificato (sintetico) | `media_registry.rs::approved_media_is_a_readonly_managed_copy_with_hash` |
| Registro media: copia alterata rilevata (`IntegrityMismatch`), copia rimossa → `missing` | verificato (sintetico) | `altered_managed_copy_is_detected` |
| Registro media: sorgente directory, collegamento simbolico o con scrittore attivo rifiutata; origine con caratteri di controllo rifiutata | verificato (sintetico; scrittore solo Windows) | `source_that_is_a_directory_or_link_is_refused`, `source_open_by_a_writer_is_refused` |
| Nessuna sovrascrittura: destinazione comparsa prima della pubblicazione mai sostituita né rimossa dall'annullamento | verificato (sintetico) | `destination_appearing_before_publication_is_never_overwritten` |
| Manifest di budget per catalogo, VM, baseline ed esecuzione con limiti e flag di superamento; scritto in modo atomico con hash registrato | verificato (sintetico) | `budget_manifest.rs::budget_manifest_reports_usage_per_vm_baseline_and_run` |
| Budget superato: esecuzione rifiutata (`resource_limit`) prima di qualunque intento o file; entro il budget preparata | verificato (sintetico) | `run_over_budget_is_refused_before_any_file_or_intent` |
| Guardia di esecuzione: hash di disco base e vars ricalcolato attraverso l'handle di guardia e uguale al catalogo | verificato (sintetico) | `protection.rs::guard_verifies_baseline_hash_through_the_handle` |
| Baseline o vars alterati fuori dal catalogo: avvio rifiutato (`IntegrityMismatch` → `state_conflict`) | verificato (sintetico) | `altered_baseline_is_refused_before_start` |
| Attributo di sola lettura tolto: ripristinato e riportato | verificato (sintetico) | `missing_readonly_attribute_is_restored_and_reported` |
| Baseline con guardia attiva: apertura in scrittura, rinomina e cancellazione rifiutate (errore 32), lettura condivisa possibile | verificato (sintetico, Windows) | `guarded_baseline_rejects_writers_rename_and_delete` |
| Rilevatore di scrittori: base aperta in scrittura da altri → `WriterProbe::Busy`, avvio e preparazione rifiutati (`vm_busy`), nessun intento lasciato | verificato (sintetico, Windows) | `writer_detector_refuses_start_and_preparation` |
| Overlay in uso: rinomina e cancellazione rifiutate (errore 32) | verificato (sintetico, Windows) | `overlay_in_use_cannot_be_renamed_or_deleted_but_remains_writable` |
| Overlay in uso **scrivibile** da un altro handle dello stesso utente | verificato come **limite** (sintetico, Windows) | stesso test: la scrittura di terzi riesce, coerente con `F0-05` §7.3 e ADR-0025 punto 4 |
| Cancellazione della baseline tramite journal con guardia attiva: rinomina fallita, file intatto | verificato (sintetico, Windows) | `guarded_baseline_cannot_be_deleted_through_the_journal` |
| Esecuzione già conclusa: guardia rifiutata | verificato (sintetico) | `ended_run_cannot_be_guarded` |
| Prova con QEMU reale e processi distinti | non eseguito | fuori perimetro di questo flusso; base sperimentale in `F0-05` §7 |
| Linux | non eseguito | share mode non disponibili; rinvio a R2a (ADR-0016, ADR-0025) |

Misure di prestazione: nessuna in questo file. La verifica dell'hash della baseline legge l'intero disco base a ogni avvio; il costo con dischi reali non è stato misurato.

### Cambiamenti di comportamento rilevanti

- I file nuovi sono pubblicati con un collegamento fisico (`std::fs::hard_link`, che fallisce se la destinazione esiste) seguito dalla rimozione del nome temporaneo, invece di `std::fs::rename`, che su Windows sostituisce una destinazione esistente. Su volumi senza collegamenti fisici (FAT, exFAT; errore `Unsupported`) il codice ripiega su controllo di esistenza + `rename`, con finestra tra controllo e uso.
- Una destinazione trovata occupata alla pubblicazione è registrata nel journal (`destination_conflict`): annullamento e riconciliazione non la rimuovono più (prima veniva trattata come file orfano dell'intento e cancellata).
- La preparazione di un'esecuzione tiene aperta la guardia della baseline (rilevatore di scrittori, senza ricalcolo dell'hash); la verifica completa dell'hash avviene in `Catalog::guard_run`, da chiamare prima di avviare QEMU.
- Un errore durante la riconciliazione di un intento non impedisce più l'apertura del catalogo: l'intento passa a `needs_review`; una presenza di file non verificabile è riportata come `IntegrityIssue::Unverifiable`.

## Limiti

- Test sintetici: file di pochi byte al posto di immagini QCOW2, vars OVMF e ISO; nessun QEMU né `qemu-img`. "Altro processo" simulato con handle nello stesso processo.
- Un solo host, un solo utente non amministratore, volumi NTFS. Nessuna prova su FAT/exFAT (ripiego di `publish_no_replace` non provato), su Linux o con un secondo account.
- La protezione degli handle vale finché il supervisor è vivo; non protegge da un amministratore, da accessi a livello di volume o da mappature in memoria (non provati); lo stesso utente può togliere l'attributo di sola lettura (viene ripristinato e riportato all'uso successivo, non impedito).
- L'overlay in uso resta scrivibile da un altro processo dello stesso utente (limite dichiarato, decisione aperta).
- L'identità OS dei file (`FileIdentity` di `lightvm-proto`: numero di serie del volume e indice del file) non è registrata: su Windows non è leggibile con le API stabili di `std` senza codice `unsafe`.
- Il controllo di collegamenti sui componenti intermedi dei percorsi gestiti avviene prima dell'uso (finestra tra controllo e uso dichiarata in `paths.rs`); solo l'ultimo componente delle aperture protette è verificato sull'handle.
- Il budget è applicativo (ADR-0017): calcolato dai byte registrati e aggiornato da `refresh_disk_sizes`; tra due controlli il consumo può superarlo.
- La verifica dell'hash a ogni avvio ha un costo proporzionale alla dimensione del disco base, non misurato.

## Punti aperti

1. **Protezione dell'overlay in uso (decisione dell'utente, ADR-0025 punto 4, backlog F1-09).** Opzioni: directory per esecuzione con ACL esplicita concessa solo all'identità di QEMU; identità di processo distinta per QEMU (token ristretto o AppContainer); limite dichiarato in via definitiva. Il codice implementa soltanto l'handle senza `FILE_SHARE_DELETE` e non sceglie.
2. **Chi tiene gli handle di guardia.** `qemu-backend` ha già `StorageGuards` (stesse share mode); `vm-storage` ora offre `guard_run`/`guard_baseline` con verifica dell'hash attraverso l'handle. Va deciso nel supervisor quale dei due usare (o la composizione), per non aprire due volte gli stessi file con regole diverse.
3. **Lifecycle dopo `needs_review`.** Risolto l'intento, la VM resta `recovering`: il ritorno a `stopped` è lasciato al supervisor dopo l'osservazione del processo. Da confermare.
4. **Costo della verifica completa dell'hash a ogni avvio** con dischi base reali (misura da registrare; eventuale alternativa da decidere con ADR).
5. **Identità OS dei media** (`FileIdentity`): richiede API di sistema (`GetFileInformationByHandleEx`) e quindi codice `unsafe` confinato in un altro crate, oppure la stabilizzazione delle API `std` corrispondenti.

## Azioni richieste

1. Integrazione in `lightvm-service`: chiamare `Catalog::guard_run` prima di avviare QEMU e tenere la guardia viva fino alla fine del processo; usare `prepare_run_within_budget` (o `check_new_run`) prima di ogni nuova esecuzione; trattare `RecoveryOutcome::NeedsReview` e `IntegrityIssue::Unverifiable` all'avvio come stato "da verificare" visibile all'utente.
2. Prova con QEMU reale delle guardie di `vm-storage` (baseline e overlay) sul runner fisico, nell'ambito di F1-09/F1-12.
3. Prova su FAT/exFAT del ripiego di pubblicazione o decisione di rifiutare cataloghi su volumi senza collegamenti fisici.
4. Aggiornare l'indice di `docs/verification/README.md`, `docs/STATUS.md` e `docs/RELEASE_PLAN.md` (fuori dal perimetro di questo flusso).

## Decisioni proposte

- Per **ADR-0025**: pubblicazione dei file gestiti con collegamento fisico + rimozione del temporaneo (nessuna sovrascrittura anche con destinazioni comparse dopo il controllo) e verifica dell'hash della baseline attraverso lo stesso handle di guardia, così che tra verifica e avvio il contenuto non possa cambiare. Da registrare nell'ADR se accettata.
- Per **ADR-0005**: stato `needs_review` del journal come esito esplicito di una riconciliazione non riuscita, con VM bloccata fino alla risoluzione.
- Nessuna proposta sulla protezione dell'overlay in uso: la scelta spetta all'utente (Punti aperti, punto 1).


## Aggiornamento del 2026-10-10: sessione dell'operazione (migrazione v4)

Sezione aggiunta in coda (regola 8 di `docs/verification/README.md`). Eseguito sull'host di `F1-03-qemu-backend-reale.md` (Windows 11, Rust 1.99.0 MSVC).

- Migrazione `0004_operation_session.sql`: `ALTER TABLE operations ADD COLUMN session_id TEXT` (nullable, senza vincolo verso `sessions`: le sessioni agente vivono nel core, F4). `OperationRecord` di `vm-storage` ha il campo `session_id`; l'adattatore del servizio lo scrive e lo rilegge (chiuso il `TODO(F1)` in `lightvm-service/src/adapters/catalog.rs`).

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Aggiornamento da v1, v2 e v3 a v4 con schema identico a un catalogo nuovo e dati conservati | verificato (sintetico) | `migrations.rs::upgrade_from_every_previous_version_matches_a_fresh_schema` e gli altri test di `migrations.rs`, verdi con `LATEST_VERSION = 4` |
| Sessione salvata e riletta; operazioni dell'utente senza sessione | verificato (sintetico) | `constraints.rs::operation_session_is_persisted` |
| Test di `vm-storage` e `lightvm-service` | verificato | `cargo test -p vm-storage -p lightvm-service`: tutti verdi |

Resta aperto: identità OS del file e apertura protetta contro reparse point nell'ispezione dei media (`ManagedMediaInspector`, `TODO(F1-04)`): `vm-storage` non espone l'identità del file con API stabili (vedi `src/media.rs`).
