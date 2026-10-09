# ADR-0010 — Build QEMU e fonte

- Stato: Accettata (Windows); parte Linux rinviata a R2a con ADR-0016
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

QEMU è installato separatamente dall'utente, con guida e diagnostica; l'installer LightVM non è autosufficiente (roadmap, sezione 4). In F0 occorre installare QEMU con provenienza verificata e fissare binario, versione e firmware (sezione 10 F0), senza dipendere dai rami `master` (sezione 6.3). Alla ricognizione iniziale `qemu-system-x86_64` e `qemu-img` non erano nel PATH (sezione 2).

La scelta della build condiziona altre decisioni: supporto WHPX, canali privati per QMP (ADR-0012) e per il display (ADR-0013), firmware UEFI incluso (ADR-0011).

## Decisione

Su Windows 11 x86_64 la build di riferimento è **QEMU 11.1.0 per Windows pubblicata da qemu.weilnetz.de** (pagina dei build Windows indicata dal progetto QEMU), installata **in modo portabile** estraendo l'installer NSIS senza eseguirlo.

| Voce | Valore |
| --- | --- |
| Versione | 11.1.0 (`v11.1.0-12130-ge470268ff4`), release stabile del 2026-08-11 |
| Installer | `https://qemu.weilnetz.de/w64/qemu-w64-setup-20260811.exe`, 206 615 928 byte |
| SHA-512 installer | `5bcf9eed634e8575a37b74f445af41a2fe4106da512d0c30c368301d4c105037fdfab40a5287367a28a957624cddebbc8c07e16c88ab6634f554cdf3d16bf543` (coincide con il file `.sha512` pubblicato dalla stessa fonte) |
| SHA-256 installer | `f98a8aeb5f7faea9765b6dee28316c266cd179d80354a2fed8e50176f9a2e59f` (coincide con il manifest winget `SoftwareFreedomConservancy.QEMU`, repository indipendente dal sito) |
| `qemu-system-x86_64.exe` | SHA-256 `47d57a6072e0bb3bd98f87926eb129eb1736dfe818c67b3b81ef7ce4edd0b3cd` |
| `qemu-img.exe` | SHA-256 `9d930582d0d574bb106d89982c547688e10b8f356192d302cc3d0ae4e8674534` |
| `qemu-io.exe` | SHA-256 `2b4bce81239a3982d3e274a1409251a8c67b26876ff07e5ce921017e2d42da9a` |
| Firmware | `share/edk2-*` della stessa build (ADR-0011) |
| Metodo di installazione verificato | estrazione con 7-Zip in una directory dell'utente, nessuna elevazione |

Verifica dei criteri:

1. Release stabile identificata: soddisfatto (11.1.0, non release candidate né `master`).
2. Provenienza verificabile: soddisfatto con limite. Fonte documentata, SHA-512 pubblicato e SHA-256 coincidente con il manifest winget indipendente. **Rischio dichiarato:** la firma Authenticode dei binari non è valida (certificato "Universität Mannheim", GEANT Code Signing CA 4, valido dal 12/09/2022 al 12/10/2023, scaduto). La provenienza non si basa quindi su una firma del codice valida.
3. Acceleratore disponibile: soddisfatto (`-accel help` → `tcg`, `whpx`; probe reale `query-accelerators` → `enabled: whpx`).
4. Canale QMP privato e display protetto: soddisfatto con socket Unix (`-qmp unix:…`, `-vnc unix:…`) e con VeNCrypt (`tls-creds-x509`, build con GnuTLS); vedi ADR-0012 e ADR-0013.
5. Firmware UEFI con versione e hash: soddisfatto (ADR-0011).
6. Licenza: QEMU non è redistribuito da LightVM nell'MVP; la guida all'installazione rimanda alla fonte e ai checksum registrati.

**Alternativa documentata per l'utente:** `winget install --id SoftwareFreedomConservancy.QEMU --exact` installa lo stesso installer (stesso SHA-256) per macchina in `Program Files` e **richiede elevazione** (UAC). Non è stata eseguita in F0; la guida la presenta come alternativa, con la verifica dei binari contro gli hash di questo ADR.

**Linux:** nessuna build provata (nessun host fisico, ADR-0016). La scelta tra pacchetto della distribuzione e compilazione da tarball firmato è rinviata a R2a con gli stessi criteri.

## Alternative considerate

- Pacchetto MSYS2 e compilazione da tarball firmato su Windows: non provati; restano candidati per una verifica di provenienza aggiuntiva (rebuild riproducibile) prima di F6.
- Installer NSIS eseguito direttamente o via winget: stesso contenuto, richiede elevazione; alternativa documentata.
- Escluse: build da `master`, binari senza provenienza documentata, TCG come sostituto dell'acceleratore (ADR-0007).

## Conseguenze

- Versione QEMU, hash dei binari e firmware sono registrati in `docs/COMPATIBILITY.md`; ogni cambio richiede la ripetizione dei probe (sezione 10 F6).
- La build include driver di protocollo remoti in `qemu-img` (`http`, `https`, `ftp`, `ftps`, `nfs`, `ssh`, `nbd`): il backend deve usare rappresentazioni strutturate con formato e protocollo in allowlist (sezione 6.3).
- La diagnostica di F1 verifica gli hash dei binari approvati prima dell'uso.
- Prima di F6: verifica di provenienza aggiuntiva (rebuild riproducibile, fonte alternativa con firma valida o firma del manifest) oppure rischio mantenuto e dichiarato nella documentazione di installazione.
- Se una nuova versione corregge vulnerabilità, la build si aggiorna con nuova verifica (sezione 17).

## Prove richieste

- Installazione, provenienza, capacità, probe WHPX, QMP e arresto: `docs/verification/F0-02-qemu-whpx-qmp.md` §2–§7 (verificato, con il limite della firma).
- Uso della stessa build in console, storage, supervisione e limiti: `F0-04`, `F0-05`, `F0-06`, `F0-08`.
- Linux/KVM: `docs/verification/F0-07-linux-kvm.md` (non eseguito, laboratorio assente); rinvio a R2a.

## Riferimenti

Roadmap, sezioni 2, 3, 4, 6.3, 6.4, 10 (F0, F6), 14, 17. Riesame: `docs/verification/F0-09-riesame-prove.md`.
