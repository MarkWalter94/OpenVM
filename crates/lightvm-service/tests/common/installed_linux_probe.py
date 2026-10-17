"""Disposable installed-Linux fixture, controlled only through Tools files."""
import compileall
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import time
import zipapp

root = pathlib.Path('/var/lib/lightvm-qualification')
root.mkdir(parents=True, exist_ok=True)
shares = pathlib.Path.home() / 'Documents' / 'LightVM Tools'
inbox, outbox = shares / 'inbox', shares / 'outbox'
inbox.mkdir(parents=True, exist_ok=True)
outbox.mkdir(parents=True, exist_ok=True)
boot = pathlib.Path('/proc/sys/kernel/random/boot_id').read_text().strip()


def publish(name, data):
    temp = outbox / (name + '.tmp')
    temp.write_text(json.dumps(data), encoding='utf-8')
    temp.replace(outbox / name)


def state():
    p = root / 'state.txt'
    return p.read_text() if p.exists() else None


def status():
    return dict(boot_id=boot, state=state(), os_release=pathlib.Path('/etc/os-release').read_text(),
                tools_installed=(pathlib.Path.home() / '.local/share/lightvm-tools/tools.py').is_file(),
                desktop_autostart=(pathlib.Path.home() / '.config/autostart/lightvm-tools.desktop').is_file())


publish('ready.json', status())
command = inbox / 'command.json'
while True:
    if not command.exists():
        time.sleep(0.2)
        continue
    data = json.loads(command.read_text())
    command.unlink()
    nonce, action = data['nonce'], data['action']
    try:
        if action == 'status':
            publish('result.json', dict(nonce=nonce, **status()))
        elif action == 'build':
            (root / 'state.txt').write_text(data['state'])
            source = root / 'source'
            source.mkdir(exist_ok=True)
            (source / '__main__.py').write_bytes((inbox / 'application.py').read_bytes())
            (inbox / 'application.py').unlink()
            if not compileall.compile_dir(source, force=True, quiet=1):
                raise RuntimeError('compile failed')
            artifact = outbox / 'application.pyz'
            zipapp.create_archive(source, target=artifact, interpreter='/usr/bin/env python3')
            result = subprocess.run([sys.executable, str(artifact)], check=True, capture_output=True, text=True, timeout=20)
            (outbox / 'build.log').write_text('compileall=passed\nzipapp=passed\n' + result.stdout)
            publish('result.json', dict(nonce=nonce, boot_id=boot, state=state(),
                    sha256=hashlib.sha256(artifact.read_bytes()).hexdigest(), output=result.stdout))
        elif action == 'inspect':
            publish('result.json', dict(nonce=nonce, boot_id=boot, state=state(),
                    sha256=hashlib.sha256((outbox / 'application.pyz').read_bytes()).hexdigest()))
        elif action in ('poweroff', 'reboot'):
            os.sync()
            subprocess.run(['systemctl', action], check=True)
        else:
            raise ValueError('unknown fixture action')
    except Exception as error:
        publish('result.json', dict(nonce=nonce, error=str(error)))
