# ADR-0022 — Framework di test UI

- Stato: Proposta
- Data: 2026-10-09
- Responsabile: da assegnare
- Da chiudere entro: F1 (avviata in F0, attività non bloccante)

## Contesto

Il framework di test UI è scelto e provato per ogni target; per egui il candidato è egui_kittest, che pilota l'interfaccia attraverso l'albero AccessKit e produce istantanee confrontabili, affiancato dai test headless del core (roadmap, sezione 13). Un test UI con supervisor simulato non verifica IPC, rendering nativo o accelerazione. Ganci di automazione solo negli artefatti di test (sezioni 10 F6 e 13).

## Decisione

Da prendere. Candidato principale: egui_kittest, **non ancora verificato**.

Criteri:

1. Compatibile con la versione di egui fissata in ADR-0014.
2. Esecuzione su Windows e Linux, anche in CI senza GPU.
3. Interazione tramite albero AccessKit (verifica indiretta dell'accessibilità).
4. Istantanee confrontabili e stabili tra esecuzioni.
5. Nessun gancio di automazione incluso nell'artefatto pubblico.

## Alternative considerate

- egui_kittest (candidato della roadmap).
- Test diretti sul contesto egui senza framework dedicato: copertura minore dell'interazione.
- Automazione tramite API di accessibilità del sistema operativo sull'app reale: utile per prove sull'installer, non sostituisce i test strumentati.

## Conseguenze

- I test UI usano un supervisor simulato e sono etichettati come tali; i percorsi reali restano coperti dai runner fisici.
- Il build di rilascio esclude le feature di test (verifica in F6).

## Prove richieste

- Evidenze F0 su egui_kittest 0.36.2 (compatibile con ADR-0014): test headless via albero AccessKit su Windows (`docs/verification/F0-01-toolchain-eframe.md` §7, 3 test; `docs/verification/F0-04-console-rfb-input.md`, test `kittest_ui` e `console_reale`) e su Linux in WSL2 senza GPU (`docs/verification/F0-07-linux-kvm.md` §2, 4 test). Criteri 1–3 soddisfatti da queste prove; criterio 4 (istantanee grafiche, feature `snapshot`) e criterio 5 non ancora provati.
- File di verifica dedicato in F1 per istantanee e chiusura (nome da assegnare).

## Riferimenti

Roadmap, sezioni 10 (F0, F1, F6), 13, 17.
