# ADR-0016 — Esito del laboratorio Linux

- Stato: Accettata (2026-10-10): **verificato solo Windows** per l'MVP; host Linux fisico disponibile in seguito per R2a
- Data: 2026-10-09; conferma: 2026-10-10
- Responsabile: da assegnare

## Contesto

Il criterio di uscita di F0 richiede prove Windows e Linux, oppure un risultato di blocco esplicito per il target fallito, oppure l'esito "verificato solo Windows" (roadmap, sezione 10 F0). La disponibilità di un host Linux fisico con KVM non è confermata all'avvio di F0.

## Decisione

Esiti ammessi dalla sezione 10:

| Esito | Condizione | Effetto |
| --- | --- | --- |
| Windows e Linux verificati | Smoke test reale su host Linux fisico con KVM superato | Windows primo target; cella Ubuntu con esito registrato |
| Verificato solo Windows | Nessun host Linux fisico con KVM disponibile in F0 | Windows unico target MVP; cella Ubuntu "non verificato"; qualificazione Linux rinviata a R2a, che non si chiude senza quella prova |
| Linux primo target | Windows fallisce i controlli; Linux li supera con laboratorio disponibile | Cambio target senza riduzione dei criteri |
| Bloccato | Nessun host supera i controlli | Progetto fermo su quel criterio |

**Esito proposto: "verificato solo Windows".**

Fatti (`F0-07`): sulla macchina disponibile non esiste un host Linux fisico con KVM; esiste soltanto WSL2 (kernel `6.18.33.1-microsoft-standard-WSL2`) con `/dev/kvm` annidato e non accessibile all'utente, che la sezione 10 esclude come prova. WSL2 è stato usato solo per build e test indipendenti dall'acceleratore e per i meccanismi di processo (etichette **[build]** e **[processi]**). Windows supera i controlli del criterio di uscita con le deviazioni e i pendenti pubblicati in `docs/verification/F0-09-riesame-prove.md`.

Condizioni per l'efficacia:

1. **Conferma dell'utente** che in F0 non è disponibile un host Linux fisico con KVM (`F0-09`, azione U-5). Se l'host è disponibile, lo smoke test Linux/KVM va eseguito in F0 e questo ADR registra "Windows e Linux verificati" oppure un blocco esplicito per Linux.
2. Windows supera i controlli del criterio di uscita (pendenti P-2…P-7 di `F0-09` chiusi).
3. Registrazione in `docs/STATUS.md` e `docs/COMPATIBILITY.md`, con Windows unico target del MVP e la cella Ubuntu "non verificato".

**Prove rinviate a R2a** (R2a non si chiude senza di esse):

1. Smoke test reale su host Linux fisico con KVM: QEMU con provenienza verificata, boot accelerato del guest, QMP privato su socket Unix, console e input, arresto forzato.
2. ADR-0015 sull'host fisico con QEMU reale: crash della UI, crash del supervisor, Emergency Stop con 100 campioni, nessun orfano.
3. Limiti cgroup applicati a QEMU con KVM (memoria del guest, vCPU, `cgroup.kill`).
4. Build eframe con toolchain standard Ubuntu e smoke con GPU reale, Wayland e X11 nativi.
5. Pacchetti Linux e policy host (sezione 5, righe Ubuntu e Debian/Fedora).

### Conferma del 2026-10-10

Il responsabile del progetto conferma (azione U-5) che in F0 non è disponibile un host Linux fisico con KVM; un server Linux per le prove sarà reperito **dopo**, per R2a. Esito registrato: **verificato solo Windows**; cella Ubuntu di `docs/COMPATIBILITY.md` "non verificato".

Indicazione del responsabile: provare Linux nel prossimo passo anche eseguendo LightVM dentro una VM Linux su Windows. Per la sezione 10 (paragrafo "Laboratorio Linux assente") una macchina virtuale annidata **non sostituisce** l'host fisico: la VM su Windows si usa per build, test indipendenti dall'acceleratore e meccanismi di processo (etichette **[build]** e **[processi]**, come WSL2 in `F0-07`), non per qualificare la cella Ubuntu né per chiudere R2a. Le prove rinviate restano quelle elencate sopra, sul server fisico.

## Alternative considerate

WSL2, VM annidate o runner senza accelerazione verificabile **non** sostituiscono l'host fisico per questa prova; servono soltanto per build e test indipendenti dall'acceleratore (sezione 10 F0). Il probe KVM annidato in WSL2 (`F0-07`, azione 3) resta facoltativo e non valido per la matrice.

## Conseguenze

- La build Linux di eframe (ADR-0014) e i test indipendenti dall'acceleratore restano richiesti e sono previsti nella CI di F1 (job Linux senza acceleratore, backlog in `docs/RELEASE_PLAN.md`).
- Hardware mancante significa verifica pendente, non verifica superata.
- ADR-0010, ADR-0012, ADR-0013, ADR-0014 e ADR-0015 hanno la parte Linux rinviata a R2a.

## Prove richieste

- `docs/verification/F0-07-linux-kvm.md` (smoke Linux/KVM su host fisico non eseguito; build e processi in WSL2 eseguiti con validità limitata).
- Riesame: `docs/verification/F0-09-riesame-prove.md` §1 (E11), §6.

## Riferimenti

Roadmap, sezioni 5, 10 (F0), 12 (R2a), 14, 15 (passo 5), 17.
