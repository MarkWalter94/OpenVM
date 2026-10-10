# Architecture Decision Records

Questa directory registra le scelte tecniche che la roadmap lascia aperte o che fissa come direzione da applicare, con motivazioni, alternative e prove richieste. Riferimento: `docs/ROADMAP.md`, sezione 17.

## Regola vincolante

Un ADR **non può derogare** ai controlli obbligatori, alle esclusioni, ai criteri di uscita o alle condizioni dei rami alternativi fissati dalla roadmap (sezione 17). Un ADR sceglie *tra* le alternative ammesse; se nessuna alternativa supera le prove, l'ADR passa a **Bloccata** e il traguardo interessato resta bloccato. Un limite tecnico non autorizza a cancellare, simulare o rendere facoltativo un requisito.

Un ADR non dichiara nulla come verificato: le prove stanno in `docs/verification/` e l'ADR vi rimanda.

## Stati

| Stato | Significato |
| --- | --- |
| Proposta | Decisione aperta, da chiudere entro la fase indicata (sezione 17). Elenca alternative, criteri e prove richieste. |
| Accettata | Decisione presa. Per gli ADR che recepiscono la roadmap, la direzione è fissata; le prove richieste restano da eseguire. |
| Sostituita | Superata da un ADR successivo, indicato nel campo Stato. Il testo resta invariato per tracciabilità. |
| Bloccata | Nessuna alternativa ammessa ha superato le prove; il traguardo che ne dipende è bloccato. |

## Formato

Nome file: `NNNN-titolo-breve.md`, numerazione a quattro cifre, progressiva, mai riutilizzata.

```markdown
# ADR-NNNN — <Titolo>

- Stato: Proposta | Accettata | Sostituita da ADR-NNNN | Bloccata
- Data: AAAA-MM-GG
- Responsabile: <nome o "da assegnare">
- Da chiudere entro: F0 | F1 | ... (solo per Proposta)

## Contesto
Problema, vincoli e riferimenti alla roadmap.

## Decisione
La scelta (oppure, per una Proposta, i criteri con cui verrà presa).

## Alternative considerate
Opzioni ammesse dalla roadmap, con motivo di scelta o di esclusione.

## Conseguenze
Effetti su architettura, sicurezza, test e compatibilità; cosa succede se le prove falliscono.

## Prove richieste
Prove necessarie, con rimando ai file in `docs/verification/` (esistenti o con nome proposto).

## Riferimenti
Sezioni di `docs/ROADMAP.md` e documenti esterni.
```

Quando un ADR Proposta viene chiuso si aggiornano: lo stato e la sezione Decisione dell'ADR, `docs/STATUS.md`, `docs/RELEASE_PLAN.md` (sezione "Decisioni aperte") e, se pertinente, `docs/COMPATIBILITY.md` e `docs/SECURITY.md`.

## Indice

| ADR | Titolo | Stato | Sezioni roadmap |
| --- | --- | --- | --- |
| [0001](0001-ui-nativa-egui-eframe.md) | UI nativa Rust con egui/eframe e console come texture | Accettata | 6.0, 6.4 |
| [0002](0002-supervisor-processo-separato.md) | Supervisor come processo separato; QEMU figlio del supervisor | Accettata | 6.1 |
| [0003](0003-console-rfb-interno-e-ripiego.md) | Console con client RFB interno sul canale protetto e ripiego screendump | Accettata | 6.4 |
| [0004](0004-profilo-mvp-offline.md) | Profilo MVP offline senza NIC e senza condivisioni host | Accettata | 4, 7.2 |
| [0005](0005-baseline-a-vm-spenta.md) | Baseline a VM spenta con overlay per sessione | Accettata | 8 |
| [0006](0006-mcp-stdio-pairing-grant.md) | MCP stdio con pairing e grant limitato, nessun QMP generico | Accettata | 9 |
| [0007](0007-nessun-fallback-tcg-sessioni-ai.md) | Nessun fallback TCG per le sessioni AI | Accettata | 3, 7.2 |
| [0008](0008-registro-media-supporto-dati.md) | Registro media approvato e supporto dati in sola lettura | Accettata | 4, 6.3 |
| [0009](0009-emergency-stop-primitive-os.md) | Emergency Stop con primitive OS, indipendente da QMP | Accettata | 7.3 |
| [0010](0010-build-qemu-e-fonte.md) | Build QEMU e fonte | Accettata (Windows; Linux in R2a) | 6.3, 10 F0, 17 |
| [0011](0011-release-guest-e-firmware.md) | Release guest e firmware | Accettata | 4, 8, 17 |
| [0012](0012-canale-qmp-privato.md) | Canale QMP privato | Accettata (Windows; Linux in R2a) | 6.3, 7.2, 17 |
| [0013](0013-trasporto-console-protetto.md) | Trasporto console protetto e ripiego | Accettata (Windows; Linux in R2a) | 6.4, 17 |
| [0014](0014-egui-eframe-versioni-backend.md) | Versioni di egui/eframe e backend di rendering | Accettata (Windows; backend Linux in R2a) | 6.0, 14, 17 |
| [0015](0015-terminazione-figli-linux-vm-manuali.md) | Terminazione dei figli su Linux e sorte delle VM manuali senza UI | Parte B Accettata (2026-10-10); parte A Proposta (Linux, R2a) | 6.1, 7.3, 17 |
| [0016](0016-esito-laboratorio-linux.md) | Esito del laboratorio Linux | Accettata (2026-10-10): verificato solo Windows; Linux fisico in R2a | 5, 10 F0, 17 |
| [0017](0017-limiti-os-budget-storage.md) | Limiti OS e budget storage | Proposta | 7.2, 8, 17 |
| [0018](0018-hardware-di-riferimento.md) | Hardware di riferimento | Accettata | 13, 17 |
| [0019](0019-fixture-test-deterministica.md) | Fixture di test deterministica | Accettata (definizione; realizzazione entro F3) | 9, 17 |
| [0020](0020-firma-e-revisione-indipendente.md) | Responsabilità di firma e revisione indipendente | Accettata (2026-10-10): non disponibili; pubblicazione F6 bloccata | 10 F6, 14, 17 |
| [0021](0021-sdk-e-client-mcp.md) | SDK e client MCP | Accettata per l'SDK (`rmcp` 3.5.1); client aperti fino a F5 | 9, 17 |
| [0022](0022-framework-test-ui.md) | Framework di test UI | Accettata (2026-10-10): egui_kittest | 13, 17 |
| [0023](0023-dipendenze-e-licenze.md) | Dipendenze e licenze | Accettata per politica e controlli; SBOM e font di egui aperti fino a F6 | 6.0, 6.3, 17 |
| [0024](0024-crate-rfb-client-separato.md) | Crate `rfb-client` separato usato dalla UI; `qemu-backend` resta nel supervisor | Accettata | 6.1, 6.2, 6.4 |
| [0025](0025-esclusione-dischi-nel-supervisor.md) | Esclusione dei dischi garantita dal supervisor, non dal lock di QEMU | Accettata (Windows; Linux in R2a) | 3, 6.3, 7.1, 7.4, 8, 10 |
| [0026](0026-codici-autenticazione-autorizzazione.md) | Codici pubblici per autenticazione e autorizzazione | Accettata (2026-10-10) | 6.3, 7.3, 7.4, 9 |
| [0025](0025-esclusione-dischi-nel-supervisor.md) | Esclusione dei dischi garantita dal supervisor, non dal lock di QEMU | Accettata (Windows; Linux in R2a) | 3, 6.3, 7.1, 8 |
| [0026](0026-codici-autenticazione-autorizzazione.md) | Codici pubblici `unauthenticated`, `session_expired`, `permission_denied` | Proposta | 6.3, 7, 9 |
