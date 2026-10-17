"""Prepare an isolated Ubuntu/Xfce fixture; only the new VM uses Internet.

Run on the Windows lab host. Does not change the host, reference cloud disk,
or another VM. Requires pycdlib in the host's lab Python directory.
"""
import argparse
import hashlib
import io
import json
import pathlib
import subprocess
import pycdlib

p = argparse.ArgumentParser()
p.add_argument('--root', type=pathlib.Path, required=True)
p.add_argument('--image', type=pathlib.Path, required=True)
p.add_argument('--qemu', type=pathlib.Path, required=True)
p.add_argument('--refresh-probe', action='store_true', help='Stopped owned fixture only: refresh seed/probe, retaining prepared disk')
a = p.parse_args()
assert str(a.root) == r'C:\lvdesktop-20261017\fixture'
assert a.root.is_dir() if a.refresh_probe else not a.root.exists(), 'new directory, or existing owned fixture for refresh'
expected = '6a81c37564db9b1ee84e141922625e1d7c5b389b99bb3c572e0243607d5bb4d2'
with a.image.open('rb') as f:
    assert hashlib.file_digest(f, 'sha256').hexdigest() == expected
a.root.mkdir(parents=True, exist_ok=a.refresh_probe)
disk = a.root / 'desktop.qcow2'
if not a.refresh_probe:
    subprocess.run([str(a.qemu / 'qemu-img.exe'), 'create', '-f', 'qcow2', '-F',
                    'qcow2', '-b', str(a.image), str(disk), '12G'], check=True)

def f(path, content, permissions='0644'):
    return dict(path=path, content=content, permissions=permissions)

start = '''#!/bin/sh
set -eu
mkdir -p /media/LIGHTVM_TOOLS
for n in $(seq 1 90); do
  mountpoint -q /media/LIGHTVM_TOOLS || mount -r -L LIGHTVM_TOOLS /media/LIGHTVM_TOOLS || true
  test -f /media/LIGHTVM_TOOLS/tools.cfg && break
  sleep 1
done
test -f /media/LIGHTVM_TOOLS/tools.cfg
uid=$(id -u desktop)
install -d -o desktop -g desktop -m 0700 /run/user/$uid
exec runuser -u desktop -- env DISPLAY=:99 XDG_RUNTIME_DIR=/run/user/$uid XDG_CURRENT_DESKTOP=XFCE dbus-run-session -- /opt/lightvm-desktop/session
'''
session = '''#!/bin/sh
set -eu
Xvfb :99 -screen 0 1280x800x24 -nolisten tcp &
xpid=$!
trap 'kill "$xpid" 2>/dev/null || true' EXIT
sleep 2
if test ! -f "$HOME/.local/share/lightvm-tools/tools.py"; then
  # Shipped installer starts Tools in this session after installing them.
  # Stop this first instance so subsequent connection proves XDG autostart.
  timeout 4 sh /media/LIGHTVM_TOOLS/install.sh > "$HOME/installer.log" 2>&1 || test "$?" -eq 124
fi
exec xfce4-session
'''
unit = '''[Unit]
Description=Disposable LightVM Xfce qualification session
After=network.target
[Service]
ExecStart=/opt/lightvm-desktop/start
Restart=on-failure
RestartSec=3
KillMode=control-group
[Install]
WantedBy=multi-user.target
'''
probe_desktop = '''[Desktop Entry]
Type=Application
Name=LightVM desktop qualification probe
Exec=python3 /opt/lightvm-desktop/probe.py
Terminal=false
'''
probe = pathlib.Path(__file__).with_name('desktop_linux_probe.py').read_text()
cfg = dict(package_update=True, package_upgrade=False, ssh_pwauth=False,
           disable_root=True, users=[dict(name='desktop', lock_passwd=True, shell='/bin/bash')],
           packages=['xvfb', 'xfce4-session', 'xfwm4', 'xfdesktop4', 'gvfs-backends',
                     'dbus-x11', 'xclip', 'python3-pil'],
           write_files=[f('/opt/lightvm-desktop/start', start, '0755'),
                        f('/opt/lightvm-desktop/session', session, '0755'),
                        f('/opt/lightvm-desktop/probe.py', probe),
                        f('/etc/systemd/system/lightvm-desktop.service', unit),
                        f('/home/desktop/.config/autostart/lightvm-probe.desktop', probe_desktop)],
           runcmd=[['chown', '-R', 'desktop:desktop', '/home/desktop'],
                   ['systemctl', 'daemon-reload'],
                   ['systemctl', 'enable', 'lightvm-desktop.service'],
                   ['sh', '-c', 'dpkg-query -W xfce4-session gvfs-backends xclip python3-pil xvfb > /opt/lightvm-desktop/packages.txt'],
                   ['sh', '-c', 'echo LIGHTVM_DESKTOP_PREPARED > /dev/ttyS0']],
           power_state=dict(mode='poweroff', timeout=30, condition=True))
iso = pycdlib.PyCdlib()
iso.new(interchange_level=3, vol_ident='CIDATA', rock_ridge='1.09')
for name, iso_name, content in [
    ('user-data', 'USERDATA.;1', '#cloud-config\n' + json.dumps(cfg)),
    ('meta-data', 'METADATA.;1', 'instance-id: lightvm-xfce-' + hashlib.sha256(probe.encode()).hexdigest()[:16] + '\nlocal-hostname: lightvm-xfce\n'),
    ('network-config', 'NETWORK.;1', 'version: 2\nethernets:\n  lab:\n    match:\n      name: "e*"\n    dhcp4: true\n    dhcp6: false\n')]:
    data = content.encode()
    iso.add_fp(io.BytesIO(data), len(data), iso_path='/' + iso_name, rr_name=name)
iso.write(str(a.root / 'prepare.iso'))
iso.close()
# Qualification uses the already configured disk without reapplying cloud-init.
(a.root / 'ubuntu.vmx').write_text('''.encoding = "UTF-8"
config.version = "8"
virtualHW.version = "19"
displayName = "Ubuntu Xfce Tools qualification"
guestOS = "ubuntu-64"
numvcpus = "2"
memsize = "4096"
firmware = "efi"
cleanShutdown = "TRUE"
nvme0.present = "TRUE"
nvme0:0.present = "TRUE"
nvme0:0.fileName = "ubuntu.vmdk"
''')
(a.root / 'SOURCE.json').write_text(json.dumps(dict(sha256=expected,
    source='official Ubuntu release-20260926 cloud image; lab-added Xfce packages',
    scope='synthetic desktop session on Xvfb, not display-manager login'), indent=2))
print(a.root)
