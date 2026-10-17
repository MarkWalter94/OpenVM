"""Runs as an unprivileged Xfce XDG-autostart application in the lab VM."""
import hashlib
import importlib.machinery
import importlib.util
import json
import os
import pathlib
import subprocess
import time
import traceback

home = pathlib.Path.home()
inbox = home / 'Documents/LightVM Tools/inbox'
outbox = home / 'Documents/LightVM Tools/outbox'
outbox.mkdir(parents=True, exist_ok=True)
inbox.mkdir(parents=True, exist_ok=True)
boot = pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()

def publish(name, obj):
    tmp = outbox / (name + '.tmp')
    tmp.write_text(json.dumps(obj))
    tmp.replace(outbox / name)

def gio(*args, success=True):
    result = subprocess.run(['gio', *args], capture_output=True, timeout=30)
    if success and result.returncode != 0:
        raise RuntimeError(f'gio {args}: {result.stderr.decode(errors="replace")}')
    return result

def status():
    tools = []
    for proc in pathlib.Path('/proc').iterdir():
        if not proc.name.isdigit():
            continue
        try:
            cmd = (proc / 'cmdline').read_bytes().split(b'\0')
            if str(home / '.local/share/lightvm-tools/tools.py').encode() in cmd:
                tools.append(dict(pid=int(proc.name), uid=proc.stat().st_uid,
                                  args=[x.decode() for x in cmd if x]))
        except (OSError, ValueError):
            pass
    session_id = os.environ.get('XDG_SESSION_ID')
    session = {}
    if session_id:
        report = subprocess.run(['loginctl', 'show-session', session_id, '-p', 'Active', '-p', 'Remote', '-p', 'Type', '-p', 'Seat'], capture_output=True, text=True, timeout=10)
        session = dict(line.split('=', 1) for line in report.stdout.splitlines() if '=' in line)
    mounts = [line for line in pathlib.Path('/proc/self/mountinfo').read_text().splitlines() if 'LIGHTVM_TOOLS' in line]
    return dict(boot_id=boot, uid=os.getuid(), desktop=os.environ.get('XDG_CURRENT_DESKTOP'),
                display=os.environ.get('DISPLAY'), tools=tools,
                session=session, tools_mounts=mounts,
                input_devices=pathlib.Path('/proc/bus/input/devices').read_text(),
                inhibitors=subprocess.run(['systemd-inhibit', '--list', '--no-pager'], capture_output=True, text=True).stdout[-4000:],
                logind_journal=subprocess.run(['journalctl', '-b', '-u', 'systemd-logind', '-n', '30', '--no-pager'], capture_output=True, text=True).stdout[-4000:],
                services=subprocess.run(['systemctl', 'is-active', 'lightdm', 'acpid', 'systemd-logind'], capture_output=True, text=True).stdout,
                packages=subprocess.check_output(['cat', '/opt/lightvm-desktop/packages.txt']).decode())

for _ in range(90):
    if status()['tools']:
        break
    time.sleep(1)
publish('ready.json', status())
while True:
    command = inbox / 'command.json'
    if not command.exists():
        time.sleep(.2)
        continue
    data = json.loads(command.read_text())
    command.unlink()
    nonce = data['nonce']
    try:
        if data['action'] == 'status':
            result = status()
        elif data['action'] == 'gio':
            rw, ro = 'dav://127.0.0.1:47081/Builds/', 'dav://127.0.0.1:47081/ReadOnly/'
            gio('mount', rw)
            # GVfs reuses the same mount for shares on one DAV authority.
            payload = b'Xfce GIO shared folder: caf\xc3\xa8\n'
            source = home / 'gio-source.txt'
            source.write_bytes(payload)
            folder = 'desktop-' + nonce
            gio('mkdir', rw + folder)
            path = rw + folder + '/spazio%20%C3%A8.txt'
            gio('copy', str(source), path)
            assert gio('cat', path).stdout == payload
            renamed = rw + folder + '/renamed.txt'
            gio('move', path, renamed)
            source.write_bytes(b'replaced\n')
            gio('copy', str(source), renamed)
            assert gio('cat', ro + folder + '/renamed.txt').stdout == b'replaced\n'
            assert gio('copy', str(source), ro + folder + '/forbidden.txt', success=False).returncode != 0
            assert gio('remove', ro + folder + '/renamed.txt', success=False).returncode != 0
            gio('remove', renamed)
            gio('remove', rw + folder)
            gio('mount', '--unmount', rw)
            result = dict(gio='passed', read_only='passed', unicode='passed', mount='passed')
        elif data['action'] == 'clipboard':
            loader = importlib.machinery.SourceFileLoader('installed_tools', str(home / '.local/share/lightvm-tools/tools.py'))
            spec = importlib.util.spec_from_loader(loader.name, loader)
            module = importlib.util.module_from_spec(spec)
            loader.exec_module(module)
            desktop = module.Desktop()
            text = 'Xfce clipboard: caffè\nsecond line'
            desktop.clipboard(text)
            assert desktop.clipboard() == text
            image = module.struct.pack('<4sII', b'LIMG', 31, 23) + bytes([13, 71, 202, 255]) * (31 * 23)
            desktop.image(image)
            assert desktop.image() == image
            result = dict(text='passed', image='passed', image_sha256=hashlib.sha256(image).hexdigest())
        else:
            raise ValueError('unsupported lab action')
        publish('result.json', dict(nonce=nonce, **result))
    except Exception:
        publish('result.json', dict(nonce=nonce, error=traceback.format_exc()))
