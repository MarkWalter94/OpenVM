"""Create a disposable VMware-format fixture from the official Ubuntu QCOW2.

Requires pycdlib on the host. All outputs go to a new directory on E:.
No package installation or Internet access is needed inside the guest.
"""
import argparse
import hashlib
import io
import json
import pathlib
import subprocess
import pycdlib

parser = argparse.ArgumentParser()
parser.add_argument('--image', type=pathlib.Path, required=True)
parser.add_argument('--qemu-img', type=pathlib.Path, required=True)
parser.add_argument('--out', type=pathlib.Path, required=True)
parser.add_argument('--refresh-seed', action='store_true', help='Refresh only seed.iso in this disposable fixture; VM must be stopped')
args = parser.parse_args()
expected = '6a81c37564db9b1ee84e141922625e1d7c5b389b99bb3c572e0243607d5bb4d2'
with args.image.open('rb') as f:
    assert hashlib.file_digest(f, 'sha256').hexdigest() == expected, 'official image SHA-256 mismatch'
assert args.out.is_absolute() and args.out.drive.upper() == 'E:'
if args.refresh_seed:
    assert (args.out / 'ubuntu.vmdk').is_file()
    assert json.loads((args.out / 'SOURCE.json').read_text())['sha256'] == expected
else:
    args.out.mkdir(exist_ok=False, parents=True)
    subprocess.run([str(args.qemu_img), 'convert', '-f', 'qcow2', '-O', 'vmdk', '-o',
                    'subformat=monolithicSparse', str(args.image), str(args.out / 'ubuntu.vmdk')], check=True)
(args.out / 'ubuntu.vmx').write_text('''.encoding = "UTF-8"
config.version = "8"
virtualHW.version = "19"
displayName = "Installed Ubuntu Tools qualification"
guestOS = "ubuntu-64"
numvcpus = "2"
memsize = "3072"
firmware = "efi"
cleanShutdown = "TRUE"
nvme0.present = "TRUE"
nvme0:0.present = "TRUE"
nvme0:0.fileName = "ubuntu.vmdk"
''', encoding='utf-8')
launcher = '''#!/bin/sh
set -eu
mkdir -p /mnt/LIGHTVM_TOOLS
for n in $(seq 1 120); do
  mountpoint -q /mnt/LIGHTVM_TOOLS || mount -r -L LIGHTVM_TOOLS /mnt/LIGHTVM_TOOLS || true
  test -f /mnt/LIGHTVM_TOOLS/tools.cfg && break
  sleep 1
done
test -f /root/.local/share/lightvm-tools/tools.py || python3 /mnt/LIGHTVM_TOOLS/tools.py --install
exec python3 /root/.local/share/lightvm-tools/tools.py --config /mnt/LIGHTVM_TOOLS/tools.cfg
'''
tools_unit = '''[Unit]
Description=LightVM installed Tools qualification
After=network-online.target
[Service]
ExecStart=/opt/lightvm-qualification/start-tools
Restart=on-failure
RestartSec=2
[Install]
WantedBy=multi-user.target
'''
probe_unit = '''[Unit]
Description=LightVM disposable guest probe
After=lightvm-qualification-tools.service
[Service]
ExecStart=/usr/bin/python3 /opt/lightvm-qualification/probe.py
Restart=on-failure
[Install]
WantedBy=multi-user.target
'''
def file(path, content, mode='0644'):
    return dict(path=path, content=content, permissions=mode)
config = dict(package_update=False, package_upgrade=False, growpart=dict(mode='off'),
              resize_rootfs=False, ssh_pwauth=False, disable_root=True,
              write_files=[file('/opt/lightvm-qualification/start-tools', launcher, '0755'),
                 file('/opt/lightvm-qualification/probe.py', pathlib.Path(__file__).with_name('installed_linux_probe.py').read_text()),
                 file('/etc/systemd/system/lightvm-qualification-tools.service', tools_unit),
                 file('/etc/systemd/system/lightvm-qualification-probe.service', probe_unit)],
              runcmd=[['systemctl', 'daemon-reload'],
                      ['systemctl', 'enable', '--now', 'lightvm-qualification-tools.service', 'lightvm-qualification-probe.service']])
iso = pycdlib.PyCdlib()
iso.new(interchange_level=3, vol_ident='CIDATA', rock_ridge='1.09')
for name, iso_name, content in [('user-data', 'USERDATA.;1', '#cloud-config\n' + json.dumps(config)),
                               ('meta-data', 'METADATA.;1', 'instance-id: lightvm-installed-linux\nlocal-hostname: lightvm-linux\n'),
                               ('network-config', 'NETWORK.;1', 'version: 2\nethernets:\n  lightvm:\n    match:\n      name: "e*"\n    dhcp4: true\n    dhcp6: false\n')]:
    data = content.encode()
    iso.add_fp(io.BytesIO(data), len(data), iso_path='/' + iso_name, rr_name=name)
iso.write(str(args.out / 'seed.iso'))
iso.close()
(args.out / 'SOURCE.json').write_text(json.dumps(dict(
    url='https://cloud-images.ubuntu.com/releases/noble/release-20260926/ubuntu-24.04-server-cloudimg-amd64.img',
    sha256=expected, fixture='preinstalled Ubuntu Server; synthetic VMware wrapper, not a VMware-origin installation'), indent=2))
print(args.out)
