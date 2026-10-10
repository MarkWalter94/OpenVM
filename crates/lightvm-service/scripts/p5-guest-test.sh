#!/bin/bash
# Check P-5, Linux part (docs/STATUS.md), to run in the Ubuntu live guest
# as user "ubuntu" (passwordless sudo) after mounting the data ISO:
#   sudo mount -o ro /dev/sr1 /mnt && bash /mnt/p5-guest-test.sh
# Verifies that a second local user cannot reach the IPC endpoint:
#   1. same user: connection accepted;
#   2. other user with the service permissions (directory 0700, socket 0600):
#      connection impossible;
#   3. other user with deliberately loosened permissions: rejected by the
#      peer identity check (SO_PEERCRED).
# Also tries to write to the data CD-ROM (ADR-0008): it must fail.
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
echo "== 1 same user ($(id -un))"; "$B" client --endpoint "$E"
sudo useradd -m other 2>/dev/null
echo "== 2 other user, service permissions"; sudo -u other "$B" client --endpoint "$E"
sudo chmod 777 "$D" "$E"
echo "== 3 other user, loosened permissions"; sudo -u other "$B" client --endpoint "$E"
sleep 1
echo "== server log"; cat /tmp/server.log
# ADR-0008: the guest cannot write the data medium (read-only CD-ROM).
echo "== write to the data CD"
sudo touch /mnt/write-test 2>&1 | tail -1
sudo mount -o remount,rw /mnt 2>&1 | tail -1
sudo dd if=/dev/zero of=/dev/sr1 bs=2048 count=1 seek=100 2>&1 | head -1
echo "== END P5"
