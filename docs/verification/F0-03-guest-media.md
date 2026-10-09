# F0-03 — Guest di riferimento: scelta della release e verifica del supporto

- Data di esecuzione (UTC): 2026-10-09 (download e verifiche tra 10:04:49Z e 10:14:11Z)
- Esecutore: agente release/sicurezza (Claude Code)
- Passo della roadmap: sezione 4 (MVP, "un solo guest di riferimento: Ubuntu LTS x86_64, con versione e checksum fissati in F0"), sezione 10 F0 ("fissare binario, versione, firmware e guest con checksum"), sezione 17 (decisione "release guest e firmware")
- ADR collegati: ADR su release guest e firmware (da scrivere; vedi "Decisioni proposte")
- Esito complessivo: **verificato** (integrità e firma del supporto). Boot del guest: **non eseguito** (fuori ambito di questa prova).

## Scopo

Scegliere e procurare il guest di riferimento del MVP con provenienza verificata: immagine desktop amd64 di Ubuntu LTS, versione e SHA-256 fissati, firma del file dei checksum verificata con la chiave ufficiale delle immagini Ubuntu.

Questa prova **non** dimostra: che l'ISO si avvii in QEMU/WHPX, che il contenuto sia privo di vulnerabilità o di codice malevolo, né alcuna compatibilità del guest con LightVM. Il file ISO non è stato eseguito né montato.

## Ambiente

| Voce | Valore |
| --- | --- |
| Host | workstation di sviluppo, Windows 11 Pro |
| OS e build | Windows 10.0.26200.9457 (output di `ver`); shell Git Bash `MINGW64_NT-10.0-26200 3.4.10` |
| Acceleratore e stato | non rilevante per questa prova |
| Strumenti | `curl 8.7.1 (x86_64-w64-mingw32) ... Schannel`; `gpg (GnuPG) 2.4.5`, `libgcrypt 1.9.4`; `sha256sum (GNU coreutils) 8.32` |
| Binario QEMU | non usato |
| Firmware | non usato |
| Guest / media | `ubuntu-26.04.1-desktop-amd64.iso`, SHA-256 `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` |
| Sessione grafica | non rilevante |
| Altre condizioni | Keyring GPG dedicato `lab/media/gnupg` (variabile `GNUPGHOME`), separato dal keyring utente |

## Release LTS osservate e motivazione della scelta

Comando:

```bash
curl -s https://releases.ubuntu.com/ -o lab/media/evidence/releases-index.html
sed 's/<[^>]*>/ /g' lab/media/evidence/releases-index.html | grep -oE "Ubuntu [0-9.]+( LTS)? \([A-Za-z ]+\)" | sort -u
```

Output:

```text
Ubuntu 14.04.6 LTS (Trusty Tahr)
Ubuntu 16.04.7 LTS (Xenial Xerus)
Ubuntu 18.04.6 LTS (Bionic Beaver)
Ubuntu 20.04.6 LTS (Focal Fossa)
Ubuntu 22.04.5 LTS (Jammy Jellyfish)
Ubuntu 24.04.3 LTS (Noble Numbat)
Ubuntu 24.04.5 LTS (Noble Numbat)
Ubuntu 25.10 (Questing Quokka)
Ubuntu 26.04.1 LTS (Resolute Raccoon)
Ubuntu 26.10 (Stonking Stingray)
```

Directory presenti nell'indice (estratto `href`): `14.04.6/ 16.04.7/ 18.04.6/ 20.04.6/ 22.04.5/ 24.04.2/ 24.04.3/ 24.04.4/ 24.04.5/ 24.04.5.1/ 25.10/ 26.04/ 26.04.1/ 26.10/` più alias per nome in codice. 25.10 e 26.10 non sono LTS (nessuna etichetta "LTS" nel titolo).

Elenco della directory `https://releases.ubuntu.com/26.04.1/` (estratto delle immagini desktop e dei checksum; `26.04/` restituisce lo stesso elenco):

```text
SHA256SUMS                            2026-08-27 22:43   588
SHA256SUMS.gpg                        2026-08-27 22:43   833
ubuntu-26.04-desktop-amd64.iso        2026-04-23 02:25   6.1G
ubuntu-26.04.1-desktop-amd64.iso      2026-08-26 06:38   6.0G
ubuntu-26.04.1-live-server-amd64.iso  2026-08-26 05:14   2.7G
```

Criterio applicato: ultima point release della LTS più recente che abbia almeno una point release pubblicata. La LTS più recente è la 26.04 (Resolute Raccoon) e ha la point release 26.04.1 pubblicata (ISO datata 2026-08-26). Nessuna 26.04.2 è presente nell'indice. **Scelta: Ubuntu 26.04.1 LTS, immagine desktop amd64** (il MVP richiede un guest grafico; l'immagine server è esclusa). La LTS precedente (24.04.x) non è necessaria.

## Procedura

Tutti i comandi eseguiti in `E:\OpenVM\lab\media` (Git Bash).

1. Checksum, firma e intestazioni HTTP dell'ISO:

```bash
B=https://releases.ubuntu.com/26.04.1
curl -L --fail --retry 5 --retry-delay 10 -o SHA256SUMS     $B/SHA256SUMS
curl -L --fail --retry 5 --retry-delay 10 -o SHA256SUMS.gpg $B/SHA256SUMS.gpg
curl -sIL $B/ubuntu-26.04.1-desktop-amd64.iso
```

Intestazioni rilevanti: `HTTP/1.1 200 OK`, `Content-Length: 6482409472`, `Last-Modified: Wed, 26 Aug 2026 06:38:13 GMT`, `ETag: "18261b800-659ed76a65f40"`. Nessun redirect.

Contenuto di `SHA256SUMS`:

```text
487f87faaf547ea30e0aba4d5b53346292571256b25333a978db1692bcee9dd2 *ubuntu-26.04-desktop-amd64.iso
dec49008a71f6098d0bcfc822021f4d042d5f2db279e4d75bdd981304f1ca5d9 *ubuntu-26.04-live-server-amd64.iso
96c7f5fb28a7fe28245331f9bfbe4375f18dd29a4850116ad3c4f60f6700c55c *ubuntu-26.04-wsl-amd64.wsl
601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f *ubuntu-26.04.1-desktop-amd64.iso
cc8a95cde20f6ced61a322420de00f10cc3c90ced545daa46cb9c1a117f1d927 *ubuntu-26.04.1-live-server-amd64.iso
48d56724b5c8e60f24893e83e73bbb58c60b3ca22fba3da977075420acd54104 *ubuntu-26.04.1-wsl-amd64.wsl
```

2. Download dell'ISO in background, con ripresa:

```bash
curl -L --fail --retry 5 --retry-delay 10 -C - -s -S \
  -o ubuntu-26.04.1-desktop-amd64.iso \
  https://releases.ubuntu.com/26.04.1/ubuntu-26.04.1-desktop-amd64.iso
```

Esito: `exit=0` (log `evidence/curl-iso.log`), nessun errore riportato, nessuna ripresa necessaria. Dimensione finale (`stat -c %s`): **6482409472 byte**, uguale al `Content-Length`.

3. Identificazione della chiave di firma:

```bash
gpg --list-packets SHA256SUMS.gpg
```

```text
:signature packet: algo 1, keyid D94AA3F0EFE21092
	version 4, created 1787870589, md5len 0, sigclass 0x00
	digest algo 10, begin of digest 24 61
	hashed subpkt 33 len 21 (issuer fpr v4 843938DF228D22F7B3742BC0D94AA3F0EFE21092)
	hashed subpkt 2 len 4 (sig created 2026-08-27)
	subpkt 16 len 8 (issuer key ID D94AA3F0EFE21092)
	data: [4096 bits]
```

Il file contiene una sola firma (RSA 4096, digest algo 10 = SHA-512). Prima dell'importazione, `gpg --verify` riportava `Can't check signature: No public key`.

4. Importazione della chiave dal keyserver ufficiale, in un keyring dedicato:

```bash
export GNUPGHOME=/e/OpenVM/lab/media/gnupg
gpg --keyserver hkps://keyserver.ubuntu.com --recv-keys 843938DF228D22F7B3742BC0D94AA3F0EFE21092
gpg --list-keys --with-fingerprint --with-subkey-fingerprints --keyid-format long
```

```text
gpg: key D94AA3F0EFE21092: public key "Ubuntu CD Image Automatic Signing Key (2012) <cdimage@ubuntu.com>" imported
gpg: Total number processed: 1
gpg:               imported: 1
pub   rsa4096/D94AA3F0EFE21092 2012-05-11 [SC]
      Key fingerprint = 8439 38DF 228D 22F7 B374  2BC0 D94A A3F0 EFE2 1092
uid                 [ unknown] Ubuntu CD Image Automatic Signing Key (2012) <cdimage@ubuntu.com>
```

5. Confronto con la documentazione ufficiale Ubuntu: `curl -sL https://ubuntu.com/tutorials/how-to-verify-ubuntu` (URL effettivo invariato, HTTP 200; copia in `lab/media/evidence/tutorial-verify.html`, SHA-256 `0d729e63cf9f896e430c412b6837362a96c756accbc6a926116f9afb19b8399c`). Impronte indicate nella pagina:

```text
pub   dsa1024/46181433FBB75451 2004-12-30 [SC]
      Key fingerprint = C598 6B4F 1257 FFA8 6632  CBA7 4618 1433 FBB7 5451
pub   rsa4096/D94AA3F0EFE21092 2012-05-11 [SC]
      Key fingerprint = 8439 38DF 228D 22F7 B374  2BC0 D94A A3F0 EFE2 1092
```

La chiave che ha firmato `SHA256SUMS` (D94AA3F0EFE21092) coincide carattere per carattere con l'impronta della chiave 2012 indicata nel tutorial. La chiave DSA 2004 (46181433FBB75451) citata nel tutorial non è usata da questa firma e non è stata importata.

6. Verifica del checksum (solo la riga pertinente):

```bash
grep ' \*ubuntu-26.04.1-desktop-amd64.iso$' SHA256SUMS > evidence/SHA256SUMS.desktop-26.04.1
sha256sum -c evidence/SHA256SUMS.desktop-26.04.1
```

```text
ubuntu-26.04.1-desktop-amd64.iso: OK
exit=0
```

7. Verifica della firma:

```bash
GNUPGHOME=/e/OpenVM/lab/media/gnupg gpg --keyid-format long --verify SHA256SUMS.gpg SHA256SUMS
```

```text
gpg: Signature made Fri Aug 28 00:43:09 2026
gpg:                using RSA key 843938DF228D22F7B3742BC0D94AA3F0EFE21092
gpg: Good signature from "Ubuntu CD Image Automatic Signing Key (2012) <cdimage@ubuntu.com>" [unknown]
gpg: WARNING: This key is not certified with a trusted signature!
gpg:          There is no indication that the signature belongs to the owner.
Primary key fingerprint: 8439 38DF 228D 22F7 B374  2BC0 D94A A3F0 EFE2 1092
exit=0
```

L'orario `Fri Aug 28 00:43:09 2026` è in ora locale (CEST); il pacchetto firma riporta `sig created 2026-08-27` (UTC).

**Avvertenza di fiducia spiegata.** `Good signature` significa che `SHA256SUMS` è stato firmato con la chiave privata corrispondente alla chiave pubblica importata e non è stato alterato. L'avvertenza `not certified with a trusted signature` e il marcatore `[unknown]` indicano soltanto che nel keyring locale nessuna chiave fidata ha certificato questa chiave: non esiste un percorso nel Web of Trust. Non è un errore di verifica. Il legame tra chiave e proprietario è stato stabilito fuori banda, confrontando l'impronta completa con la pagina ufficiale `ubuntu.com` servita via HTTPS (passo 5). Non è stata assegnata fiducia locale alla chiave (`--edit-key trust` non eseguito), per non mascherare questa distinzione.

8. Attestazione: con entrambe le verifiche riuscite è stato creato `lab/media/ubuntu-26.04.1-desktop-amd64.iso.verified` (data UTC, SHA-256, dimensione, impronta della chiave, URL) ed è stato completato `lab/media/MANIFEST.md`.

## Risultati

| Voce | Esito | Evidenza |
| --- | --- | --- |
| Scelta della release (criterio point release) | verificato | indice `releases.ubuntu.com` osservato il 2026-10-09: 26.04.1 LTS è l'ultima point release della LTS più recente |
| Download ISO completo | verificato | `curl` exit 0; 6482409472 byte = `Content-Length` |
| SHA-256 ISO contro `SHA256SUMS` | verificato | `ubuntu-26.04.1-desktop-amd64.iso: OK` |
| Firma GPG di `SHA256SUMS` | verificato | `Good signature`, chiave `8439 38DF 228D 22F7 B374 2BC0 D94A A3F0 EFE2 1092` |
| Impronta chiave contro documentazione ufficiale | verificato | coincidente con https://ubuntu.com/tutorials/how-to-verify-ubuntu |
| Presenza della catena UEFI/Secure Boot nel media | verificato sull'elenco pubblicato, non sull'ISO | vedi sezione Firmware |
| Boot del guest in QEMU/WHPX | non eseguito | fuori ambito (F0-02 / prove successive) |

Artefatti fissati:

| Artefatto | SHA-256 | Dimensione |
| --- | --- | --- |
| `ubuntu-26.04.1-desktop-amd64.iso` | `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f` | 6482409472 byte |
| `SHA256SUMS` | `4adde9f7e7760a624db5821ad75679a76b9cde1e0d3e9cb2117cfe7d8043fa43` | 588 byte |
| `SHA256SUMS.gpg` | `055292b53dffbc5387c1691e8c2357df0b4db88e7d7fd4c1b36fadf068479c5d` | 833 byte |

## Firmware richiesto e profilo VM di riferimento

Per non montare l'ISO è stato scaricato l'elenco dei contenuti che Ubuntu pubblica accanto all'immagine: `https://releases.ubuntu.com/26.04.1/ubuntu-26.04.1-desktop-amd64.list`, 951 righe. Questo file non è coperto da `SHA256SUMS`, quindi ha valore indicativo. Righe rilevanti:

```text
/EFI/boot/bootx64.efi
/EFI/boot/grubx64.efi
/EFI/boot/mmx64.efi
/pool/main/s/shim-signed/shim-signed_1.59+15.8-0ubuntu2_amd64.deb
/boot/grub/x86_64-efi/...
/boot/grub/i386-pc/eltorito.img
```

- Firmware previsto per il riferimento: **UEFI x86_64** (in QEMU, OVMF/edk2). Il media contiene il loader EFI di fallback (`bootx64.efi`, lo shim firmato in Ubuntu), GRUB EFI e MokManager (`mmx64.efi`): la catena di avvio Secure Boot è presente.
- **Secure Boot non è necessario** per Ubuntu: la stessa catena shim/GRUB si avvia anche con Secure Boot disattivato. È però supportato, quindi in futuro si può provare anche una variante OVMF con Secure Boot attivo. Questa nota si basa sull'elenco dei file e sul comportamento documentato di Ubuntu, non su un boot eseguito.
- Il media contiene anche `i386-pc/eltorito.img` (avvio BIOS legacy). La roadmap include l'NVRAM UEFI nel modello di consistenza (sezione 8), quindi il BIOS legacy non è proposto come riferimento.
- UEFI implica un file di variabili NVRAM per ogni VM, da includere in baseline, overlay e ripristino (roadmap, sezione 8).
- Profilo minimo di VM consigliato dalla roadmap per il riferimento (sezione 13): **2 vCPU, 4 GiB di RAM, display 1280×720**, su un host di riferimento con almeno 4 core/8 thread, 16 GiB di RAM, SSD e almeno 100 GiB liberi.

## Limiti

- La verifica GPG dimostra l'integrità e la provenienza di `SHA256SUMS` rispetto alla chiave di firma delle immagini Ubuntu e, tramite lo SHA-256, quelle dell'ISO. Non dimostra che il contenuto sia sicuro, privo di vulnerabilità o adatto al prodotto.
- La fiducia nella chiave si basa sul keyserver `keyserver.ubuntu.com` e sulla pagina HTTPS `ubuntu.com`, cioè sull'infrastruttura dello stesso editore. Non c'è un'attestazione indipendente tramite Web of Trust.
- La chiave D94AA3F0EFE21092 risale al 2012 (RSA 4096) e l'elenco non mostra una data di scadenza. Lo stato di revoca è quello restituito dal keyserver al momento dell'importazione: nessuna revoca mostrata.
- `releases.ubuntu.com/26.04/` e `/26.04.1/` mostrano lo stesso contenuto. Una futura 26.04.2 o una ripubblicazione cambierebbe `SHA256SUMS` in quella directory: il riferimento resta fissato all'hash indicato sopra, non all'URL.
- L'ISO non è stata avviata. Firmware e compatibilità con QEMU/WHPX restano da provare.
- Un primo `gpg --verify`, eseguito prima di impostare `GNUPGHOME`, ha creato la directory vuota `~/.gnupg` dell'utente (`gpg: directory '/c/Users/mortali/.gnupg' created`). In quel keyring non è stata importata nessuna chiave.
- L'ISO in `lab/media/` non è ancora una copia gestita in sola lettura nel senso del registro media approvato (sezione 6.3): è soltanto un artefatto di laboratorio.

## Azioni richieste

- Prova di boot del guest con QEMU/WHPX e firmware OVMF fissato (F0-02 o file successivo), registrando l'hash del firmware usato.
- Registrare release guest e firmware nell'ADR dedicato, e in `docs/COMPATIBILITY.md` solo dopo il boot.
- Prima di ogni release, controllare se sono usciti una nuova point release o un avviso di sicurezza che rendano necessario aggiornare il riferimento (sezione 17: non si congela una versione vulnerabile).

## Decisioni proposte

Proposte per l'ADR "release guest e firmware" (non sono decisioni finché l'ADR non è scritto):

1. Guest di riferimento del MVP: **Ubuntu 26.04.1 LTS desktop amd64**, file `ubuntu-26.04.1-desktop-amd64.iso`, SHA-256 `601e30fbf5d97759367c632e2c33630665039b7e2158fd068403da3ccf1bda1f`, 6482409472 byte.
2. L'identità del media è fissata dall'hash, non dall'URL o dal nome del file. Un cambio di hash richiede un aggiornamento dell'ADR con una nuova verifica.
3. Catena di fiducia accettata: `SHA256SUMS` firmato dalla chiave `8439 38DF 228D 22F7 B374 2BC0 D94A A3F0 EFE2 1092` (Ubuntu CD Image Automatic Signing Key 2012). L'impronta va fissata nel repository e confrontata a ogni nuova acquisizione, usando un keyring dedicato.
4. Firmware: UEFI x86_64 (OVMF/edk2, versione e hash da fissare con F0-02), con NVRAM per VM inclusa nel modello di ripristino. Secure Boot non è richiesto per il riferimento; la variante con Secure Boot attivo è un test opzionale.
5. Profilo VM di riferimento: 2 vCPU, 4 GiB, display 1280×720 (roadmap, sezione 13).
6. Criterio di aggiornamento: adottare una nuova point release della stessa LTS solo dopo una nuova verifica (checksum, firma, boot). Il passaggio a una nuova LTS si valuta solo dopo la sua prima point release.
7. Alternativa scartata: 24.04.x LTS. È disponibile, ma non è la LTS più recente con una point release.
