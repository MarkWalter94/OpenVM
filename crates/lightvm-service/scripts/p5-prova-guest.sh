#!/bin/bash
# Prova P-5, parte Linux (docs/STATUS.md), da eseguire nel guest Ubuntu live
# come utente "ubuntu" (sudo senza password) dopo aver montato l'ISO dati:
#   sudo mount -o ro /dev/sr1 /mnt && bash /mnt/prova.sh
# Verifica che un secondo utente locale non raggiunga l'endpoint IPC:
#   1. stesso utente: connessione accettata;
#   2. altro utente con i permessi del servizio (directory 0700, socket 0600):
#      connessione impossibile;
#   3. altro utente con permessi allentati apposta: rifiutato dal controllo
#      dell'identità del peer (SO_PEERCRED).
# Prova anche a scrivere sul CD-ROM dati (ADR-0008): deve fallire.
set -u
B=/tmp/lvmpeer
cp /mnt/lvmpeer "$B" && chmod 755 "$B"
D=/tmp/lvm-ipc
rm -rf "$D"
"$B" server --dir "$D" > /tmp/server.log 2>&1 &
sleep 2
E=$(cat "$D/endpoint.txt")
echo "== endpoint $E"
ls -ld "$D"; ls -l "$E"
echo "== 1 stesso utente ($(id -un))"; "$B" client --endpoint "$E"
sudo useradd -m altro 2>/dev/null
echo "== 2 altro utente, permessi del servizio"; sudo -u altro "$B" client --endpoint "$E"
sudo chmod 777 "$D" "$E"
echo "== 3 altro utente, permessi allentati"; sudo -u altro "$B" client --endpoint "$E"
sleep 1
echo "== log del server"; cat /tmp/server.log
# ADR-0008: il guest non può scrivere il supporto dati (CD-ROM in sola lettura).
echo "== scrittura sul CD dati"
sudo touch /mnt/scrittura 2>&1 | tail -1
sudo mount -o remount,rw /mnt 2>&1 | tail -1
sudo dd if=/dev/zero of=/dev/sr1 bs=2048 count=1 seek=100 2>&1 | head -1
echo "== FINE P5"
