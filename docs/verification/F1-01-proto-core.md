# F1-01 — Tipi condivisi, modello VM, macchina a stati, risorse, sessioni e audit minimo

- Data di esecuzione (UTC): 2026-10-09
- Esecutore: agente di sviluppo (flusso proto/core)
- Passo della roadmap: sezione 10, F1 (backlog `RELEASE_PLAN.md`: F1-01 per i tipi e gli schemi, F1-04 per modello, stati, risorse e registro media come modello, F1-10 per l'audit minimo)
- ADR collegati: ADR-0005, ADR-0006, ADR-0007, ADR-0008, ADR-0017, ADR-0026 (Proposta)
- Esito complessivo: parziale (tutte le voci di questo file sono verificate **con test sintetici**; nessuna voce riguarda QEMU, file reali o hardware)

## Scopo

Dimostrare con test automatici, su porte finte in memoria, che:

1. `lightvm-proto` espone tipi condivisi con JSON Schema generati da fonte unica, confrontati con i file committati da un test che fallisce se gli schemi non sono rigenerati, e una tabella di codici d'errore pubblici stabile (sezione 6.3);
2. `vm-core` applica la macchina a stati, le guardie per stato, la coda per VM, la validazione delle risorse per utente e per host, la verifica dei media approvati, le sessioni agente con grant, allowlist, quote, scadenze e revoca, e registra l'audit di creazione, avvio, arresto e dinieghi (sezioni 6.3, 7.2 punti 7 e 9, 7.3, 7.4, 10 F1);
3. lo stato deriva dalle osservazioni del backend (`BackendObservation`) e non dal solo stato persistito né dagli esiti tardivi del backend;
4. la distinzione tra `unauthenticated` e `permission_denied` è definita, implementata e testata (proposta in ADR-0026).

Questa prova **non** dimostra: boot reale, gestione di processi, QMP, apertura protetta dei file, copia gestita in sola lettura dei media, hash calcolati su file reali, persistenza SQLite, IPC. Sono test sintetici con backend, catalogo, orologio, audit e ispettore dei media simulati: non sostituiscono il boot reale richiesto dal criterio di uscita F1 (sezioni 10 F1 e 16 punto 5).

## Ambiente

| Voce | Valore |
| --- | --- |
| Host (modello, CPU, RAM, storage) | host di sviluppo; non rilevante per test senza I/O |
| OS e build | Windows 11 Pro 10.0.26200 |
| Acceleratore e stato | non usato |
| Toolchain / versioni rilevanti | rustc 1.99.0 (b940084d7 2026-09-28), host `x86_64-pc-windows-msvc`; cargo 1.99.0; Build Tools 2022 caricati dal wrapper `msvc.cmd`; `schemars` =1.2.2, `proptest` 1.11.0, `serde_json` 1.0.151 (da `Cargo.lock`) |
| Binario QEMU (versione, provenienza, SHA-256) | non usato |
| Firmware (file, versione, SHA-256) | non usato |
| Guest / media (nome, versione, SHA-256) | nessuno: media simulati con hash fittizi |
| Sessione grafica (locale, RDP, headless) | non rilevante |
| Altre condizioni (antivirus, criteri di gruppo, ecc.) | workspace con modifiche in corso di altri flussi su `qemu-backend` e `lightvm-service` (vedi Limiti) |

## Procedura

Tutti i comandi sono eseguiti da `E:\OpenVM` tramite il wrapper che carica l'ambiente MSVC.

1. Misura iniziale dei test, prima delle modifiche:

   ```text
   cargo test -p lightvm-proto -p vm-core
   ```

2. Modifiche:
   - `lightvm-proto`: nuovo codice `ErrorCode::Unauthenticated` in coda (additivo), scorciatoia `PublicError::unauthenticated`, documentazione della semantica dei tre codici di autenticazione/autorizzazione nel modulo `error`; `ProtocolErrorCode::Unauthenticated` documentato come mantenuto ma non emesso.
   - `vm-core`: sessioni con codici `unauthenticated` / `session_expired` / `permission_denied` (scadenza valutata prima della pausa); coda scartata alla terminazione con il codice coerente con la causa (`discard_code_for`); coda dell'utente scartata dall'Emergency Stop con `state_conflict`; controllo all'avvio della memoria guest complessiva rispetto alla memoria host meno la riserva.
   - Test: `lightvm-proto/tests/error_codes.rs` (tabella congelata a 10 codici, coerenza con lo schema committato, semantica dei codici di autenticazione); `vm-core/tests/negatives.rs` e `vm-core/tests/observations.rs` (nuovi); aggiornamento di `vm-core/tests/scenarios.rs` ai nuovi codici; fake estesi in `vm-core/tests/common/mod.rs` (registro delle transizioni persistite, audit che può fallire, media scrivibili/spostati/illeggibili).

3. Verifica della severità del test di snapshot, prima di rigenerare:

   ```text
   cargo test -p lightvm-proto --test schema_snapshot
   ```

4. Rigenerazione degli schemi dalla fonte unica:

   ```text
   LIGHTVM_UPDATE_SCHEMAS=1 cargo test -p lightvm-proto --test schema_snapshot
   ```

5. Controllo che il test di proprietà rilevi una transizione non ammessa: disattivazione temporanea della guardia `can_transition_to` in `Core::set_state` (copia di sicurezza del file nello scratchpad), esecuzione di `cargo test -p vm-core --test observations`, ripristino del file e verifica che la modifica temporanea non sia più presente.

6. Comandi finali:

   ```text
   cargo fmt -p lightvm-proto -p vm-core -- --check
   cargo clippy -p lightvm-proto -p vm-core --all-targets -- -D warnings
   cargo test -p lightvm-proto -p vm-core
   cargo check --workspace --all-targets
   cargo check --workspace --all-targets --exclude qemu-backend
   cargo check -p vm-storage -p lightvm-proto -p vm-core -p rfb-client -p lightvm-desktop --all-targets
   ```

## Risultati

### Conteggio dei test

| Crate / file | Prima | Dopo |
| --- | --- | --- |
| `lightvm-proto` `tests/error_codes.rs` | 4 | 6 |
| `lightvm-proto` `tests/roundtrip.rs` | 18 | 18 |
| `lightvm-proto` `tests/schema_snapshot.rs` | 3 | 3 |
| `lightvm-proto` test unitari in `src/` | 0 | 4 (in `src/ipc.rs`, aggiunti da un altro flusso, non da questo) |
| `vm-core` `tests/scenarios.rs` | 28 | 28 (7 asserzioni aggiornate ai nuovi codici) |
| `vm-core` `tests/negatives.rs` | — | 20 |
| `vm-core` `tests/observations.rs` | — | 8 (di cui 1 test di proprietà, 256 casi) |
| **Totale `lightvm-proto`** | 25 | 31 |
| **Totale `vm-core`** | 28 | 56 |

### Voci

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Il test di snapshot fallisce se gli schemi non sono rigenerati | verificato (sintetico) | passo 3: `committed_schemas_match_generated ... FAILED` con elenco `error_code.json`, `public_error.json`, `protocol_error.json`, `reply_error.json`, `operation_record.json`, `audit_event.json`, `caller.json: diverso dal generato`; il test segnala anche file mancanti e file non più generati |
| Rigenerazione da fonte unica con `LIGHTVM_UPDATE_SCHEMAS=1` | verificato (sintetico) | passo 4: `3 passed`; `git diff --stat` sugli schemi: 7 file modificati, 33 righe aggiunte e 8 rimosse, solo i tipi che contengono `ErrorCode` o la documentazione cambiata |
| Tabella dei codici d'errore stabile | verificato (sintetico) | `error_codes_are_frozen`: 10 codici in ordine congelato con nome serializzato e `retryable` predefinito; `committed_error_code_schema_lists_every_code`: lo schema committato elenca esattamente i 10 nomi |
| Distinzione `unauthenticated` / `session_expired` / `permission_denied` | verificato (sintetico) | `authentication_codes_are_distinct_and_not_retryable`; `unauthenticated_and_permission_denied_are_distinct` (stessa risposta per credenziale errata su VM autorizzata e non autorizzata; credenziale valida fuori allowlist o senza capability → `permission_denied`); `every_non_expiry_termination_cause_yields_unauthenticated`; `expired_session_is_session_expired_before_and_after_tick`; `missing_wrong_expired_revoked_or_restarted_token_is_denied_without_effect` |
| Stato non ammesso | verificato (sintetico) | `failed_vm_rejects_every_mutation_until_reconciled`, `starting_vm_accepts_only_force_stop`, `incompatible_operations_are_rejected`, `guard_table_matches_architecture` |
| VM occupata | verificato (sintetico) | `busy_vm_rejects_concurrent_lifecycle_mutations` (`vm_busy` ritentabile, un solo force-stop), `only_one_operation_in_flight_per_vm`, `hard_queue_limit_applies_to_the_user_too` |
| Quote superate | verificato (sintetico) | `quotas_are_enforced`, `quota_denials_are_audited_and_global_quotas_bound_the_grant` (la policy globale limita anche un grant illimitato), `screenshot_over_byte_quota_fails_without_updating_geometry` |
| Sessione revocata o scaduta | verificato (sintetico) | `revoked_session_is_unauthenticated_everywhere_and_queue_is_discarded`, `expired_session_is_session_expired_before_and_after_tick`, `renewal_cannot_extend_beyond_grant_or_lease_limit`, `lease_expiry_revokes_and_requests_emergency_stop`, `revocation_between_receipt_and_forwarding_discards_without_effect` |
| Media non approvato | verificato (sintetico) | `unapproved_media_never_reaches_the_backend` (copia scrivibile, identità OS diversa, contenuto sostituito → `permission_denied`; ispezione impossibile → `infrastructure_error`; nessuna chiamata `start` al backend), `media_removed_from_registry_discards_start`, `empty_media_cannot_be_registered`, `replaced_media_is_not_used`, `verify_media_checks_every_field` |
| Risorse oltre limite (utente e host) | verificato (sintetico) | `user_limits_and_host_limits_are_intersected`, `total_guest_memory_is_bounded_by_host_memory_minus_reserve` (16+16 GiB su 28 GiB disponibili rifiutati, 16+8 accettati), `host_capacity_change_is_rechecked_at_start_and_before_forwarding`, `create_vm_validates_resources_and_media`, `running_vm_limit_is_enforced` |
| Riconciliazione da `BackendObservation` | verificato (sintetico) | `qmp_ready_promotes_starting_before_the_backend_result`, `start_failed_observation_wins_over_a_late_success`, `unexpected_exit_while_running_fails_and_discards_the_queue`, `exit_during_stop_is_a_clean_stop_even_if_backend_reports_late_or_fails`, `force_stop_failure_requires_explicit_reconciliation`, `stale_and_unknown_observations_are_ignored`, `load_catalog_never_trusts_a_persisted_process_state`, `stale_run_id_is_rejected_and_queued_ops_invalidated` |
| Proprietà: solo transizioni ammesse dalla tabella | verificato (sintetico) | `prop_any_sequence_respects_the_transition_table`: 256 sequenze casuali fino a 59 passi di richieste, esiti anche tardivi o falliti, osservazioni anche non correnti, Emergency Stop e riconciliazione; invarianti: ogni transizione persistita è ammessa da `VmState::can_transition_to`, processo ⇒ `run_id`, `stopped` ⇒ nessun `run_id`, al più un'esecuzione aperta, ogni avvio del backend ha un'esecuzione registrata, audit con `seq` consecutivi |
| Il test di proprietà rileva una guardia rimossa | verificato (sintetico) | passo 5: con la guardia disattivata `prop_any_sequence_respects_the_transition_table ... FAILED` (`transizione stopped → recovering non ammessa`) e `force_stop_failure_requires_explicit_reconciliation ... FAILED`; dopo il ripristino la modifica temporanea non è presente (conteggio 0) |
| Audit minimo di creazione, avvio, arresto e dinieghi | verificato (sintetico) | `minimal_audit_covers_create_start_stop_and_denials` (creazione riuscita e negata; avvio e arresto `accepted`, `forwarded`, `succeeded`; dinieghi `permission_denied`, `unauthenticated`, `state_conflict` con VM, operazione e capability), `start_stop_cycles_use_fresh_run_ids_and_audit_in_order`, `audit_is_ordered_and_contains_no_secrets`, `agent_session_limit_and_rejected_sessions_are_audited` |
| Audit non disponibile prima dell'effetto | verificato (sintetico) | `audit_failure_before_effect_blocks_the_operation`: errore `infrastructure_error`, nessuna operazione accodata, nessuna chiamata al backend |
| Formattazione | verificato | `cargo fmt -p lightvm-proto -p vm-core -- --check`: nessuna differenza, codice di uscita 0 |
| Lint | verificato | `cargo clippy -p lightvm-proto -p vm-core --all-targets -- -D warnings`: `Finished`, nessun avviso |
| Test dei due crate | verificato | `cargo test -p lightvm-proto -p vm-core`: `lightvm-proto` 4 + 6 + 18 + 3, `vm-core` 20 + 8 + 28, tutti `ok`, 0 falliti |
| Dipendenti non rotti | parziale | `cargo check --workspace --all-targets` fallisce in `qemu-backend` (`file not found for module compat`, `unresolved import crate::input`, …) e, escluso `qemu-backend`, in `lightvm-service` (`file not found for module backend`, `cannot find module or crate vm_core`, `unresolved import vm_storage`): errori di moduli e dipendenze di lavori in corso di altri flussi, nessuno riferito a `ErrorCode` o ad API di `vm-core`. `cargo check -p vm-storage -p lightvm-proto -p vm-core -p rfb-client -p lightvm-desktop --all-targets`: `Finished`. In `lightvm-service` non esistono `match` esaustivi su `ErrorCode` né usi di `ErrorCode::ALL` (ricerca nel sorgente). Riesecuzione finale di `cargo check --workspace --all-targets`, dopo l'avanzamento degli altri flussi: `qemu-backend` e la libreria di `lightvm-service` (che ora usa `vm_core::Core` e le porte) compilano; resta un solo errore nel test `lightvm-service/tests/ipc.rs` (`unresolved import lightvm_service::compat`, `lightvm_service::supervisor::fake`), dovuto alla riorganizzazione dei moduli del servizio e non a questo flusso |

## Decisione documentata: `unauthenticated` e `permission_denied`

Proposta in ADR-0026, implementata in `lightvm-proto` (documentazione del modulo `error`) e in `vm-core` (`session.rs`):

| Codice | Quando | Esempi |
| --- | --- | --- |
| `unauthenticated` | nessuna credenziale valida | credenziale IPC o token di sessione assente, malformato, sconosciuto, revocato, riusato dopo riavvio del supervisor; sessione terminata per causa diversa dalla scadenza; richiesta prima dell'handshake |
| `session_expired` | credenziale riconosciuta ma scaduta | lease non rinnovato, grant oltre la scadenza; prevale sulla pausa |
| `permission_denied` | chiamante identificato ma non autorizzato | VM fuori allowlist (anche inesistente), capability assente nella policy effettiva, mutazione in pausa, ruolo IPC insufficiente, creazione VM da agente, supporto diverso da quello approvato |

Dentro `unauthenticated` i motivi non sono distinti verso il client (stesso messaggio `UNAUTHENTICATED_MESSAGE`). Il codice di protocollo `unauthenticated` resta nel contratto ma non si emette. Nessuno dei tre codici è ritentabile automaticamente.

## Limiti

- **Tutti i risultati sono sintetici.** Backend, catalogo, orologio, generatore di identificativi, audit e ispettore dei media sono finti e in memoria. Nessun processo QEMU, nessun file, nessun SQLite, nessuna IPC.
- Il registro media è verificato solo come **modello**: confronto tra fatti osservati e registro. Copia gestita in sola lettura, apertura protetta contro reparse point (`CREATE_NEW`) e calcolo dell'hash sul file aperto appartengono a `vm-storage`/`qemu-backend` (F1-04, ADR-0008) e non sono provati qui.
- Il budget di storage complessivo (overlay, log, screenshot, riserva) non è contabilizzato nel core (ADR-0017); il limite di disco vale per singola VM rispetto allo spazio libero meno la riserva.
- I limiti di risorse per singola VM (`effective_resource_limits` con livello VM) esistono come funzione pura ma il core non li riceve ancora: valgono solo limiti utente e host.
- Le vCPU non sono sommate tra VM in esecuzione (sovrallocazione ammessa); con `max_running_vms = 1` (MVP) il controllo della memoria complessiva non scatta.
- Lo spegnimento del guest dall'interno mentre la VM è `running` arriva come uscita non attesa e porta a `failed`, salvo che il backend la classifichi come attesa (vedi Decisioni proposte).
- Le consultazioni di sola lettura `Core::operation` negate non producono un evento di audit; `vm_status` e `list_vms` negati sono registrati con azione `vm_state_observed`.
- `cargo check --workspace --all-targets` non è verde per lavori in corso di altri flussi: all'ultima esecuzione fallisce solo il test `lightvm-service/tests/ipc.rs` per importazioni di moduli rimossi. La libreria di `lightvm-service` compila con le API di `vm-core` modificate da questo flusso.

## Azioni richieste

1. `lightvm-service` (flusso F1-04/F1-11): restituire `unauthenticated` invece di `permission_denied` per credenziale assente, malformata, sconosciuta o revocata e per richieste prima di `hello`; mantenere `permission_denied` per ruolo insufficiente; aggiornare `tests/ipc.rs` e `F1-04-service-ipc-ci.md`. Rieseguire `cargo check --workspace --all-targets` quando `qemu-backend` e `lightvm-service` compilano.
2. `docs/ARCHITECTURE.md`: aggiungere `unauthenticated` all'elenco degli errori pubblici. `docs/adr/README.md`: aggiungere ADR-0026 all'indice.
3. `vm-storage`/`qemu-backend` (F1-04): implementare `MediaInspector` sull'handle aperto con le protezioni di ADR-0008, così che la verifica copra anche l'uso.
4. Collegare i limiti per VM (`effective_resource_limits` con livello VM) quando il catalogo li persiste.
5. Prove reali del criterio di uscita F1 (F1-12): restano da eseguire; questo file non le sostituisce.

## Decisioni proposte

- ADR-0026 (Proposta): codici `unauthenticated` / `session_expired` / `permission_denied` come sopra.
- Spegnimento dal guest: proporre che il backend marchi come `expected` l'uscita preceduta da un evento QMP `SHUTDOWN` con origine guest, così che la VM torni `stopped` e non `failed`; da decidere con il flusso QMP (F1-06).
- Coda dell'utente scartata da un Emergency Stop: codice `state_conflict` (la VM va in arresto), invece di `permission_denied`.
