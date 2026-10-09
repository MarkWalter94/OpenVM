# ADR-0004 — Profilo MVP offline senza NIC e senza condivisioni host

- Stato: Accettata
- Data: 2026-10-09
- Responsabile: da assegnare

## Contesto

NAT o un proxy configurato nel guest non applicano una policy di rete; l'opzione `restrict=on` non basta (roadmap, sezioni 3 e 12 R1). Cartelle condivise, clipboard e passthrough aprono percorsi verso l'host. La rete filtrata ha un traguardo dedicato (R1).

## Decisione

- Nessuna NIC guest nell'MVP, con esclusione esplicita delle periferiche di rete predefinite di QEMU; nessun forwarding.
- Nessuna cartella condivisa, clipboard, USB passthrough, disco fisico, guest agent o shell host.
- Dispositivi HID virtuali in allowlist (ad esempio puntatore assoluto) ammessi.
- Il wizard espone soltanto la rete "Offline" (sezione 10 F2).
- L'unico ingresso di file nel guest è il supporto dati in sola lettura (ADR-0008).
- Il profilo offline è il predefinito dalla prima generazione dei comandi QEMU (F1).

## Alternative considerate

- NAT user-mode con `restrict=on`: non è un filtro verificato (sezione 12 R1).
- Proxy configurato nel guest: aggirabile dal guest (sezione 3).
- Rete filtrata già nell'MVP: rinviata a R1 (sezione 4).

## Conseguenze

- Il primo test dimostrativo usa un'applicazione già presente nel guest o fornita tramite supporto di installazione o supporto dati (sezione 4); il download di Firefox non è un test di accettazione.
- La configurazione QEMU generata va ispezionata e confrontata con i dispositivi osservati.

## Prove richieste

- "Nessuna NIC inattesa" (criterio di uscita F1).
- Test negativo "Rete" della sezione 7.4: guest verso host, LAN, Internet, IPv6.
- Prime osservazioni sui dispositivi in F0: `docs/verification/F0-02-qemu-whpx-qmp.md` §5 (verificato: senza `-nodefaults` q35 riceve `e1000e` su rete `user`; con `-nodefaults -nic none` nessuna NIC; `query-rx-filter` non è un indicatore affidabile, si usano `info network` e `query-pci`).

## Riferimenti

Roadmap, sezioni 3, 4, 7.2 (punti 2 e 3), 7.4, 10 (F1, F2), 12 (R1).
