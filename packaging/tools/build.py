#!/usr/bin/env python3
"""Build the guest bundle beside lightvm-service (Python 3 + mingw-w64)."""
import argparse
import hashlib
import pathlib
import re
import shutil
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=pathlib.Path, default=ROOT / 'target/debug/tools')
    parser.add_argument('--cc', default='i686-w64-mingw32-gcc')
    parser.add_argument('--objdump', default='i686-w64-mingw32-objdump')
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    binary = args.output / 'TOOLS.EXE'
    subprocess.run([args.cc, '-Wall', '-Wextra', '-Werror', '-Wno-misleading-indentation', '-Os', '-ffreestanding', '-fno-builtin', '-fno-stack-protector', '-D_WIN32_WINNT=0x0501', '-DWINVER=0x0501', '-D__USE_MINGW_ANSI_STDIO=0', '-nostdlib', '-Wl,--entry,_mainCRTStartup', '-Wl,--subsystem,windows:5.01', '-Wl,--gc-sections', str(ROOT/'apps/guest-tools/windows/tools.c'), str(ROOT/'apps/guest-tools/windows/json.c'), '-o', str(binary), '-lkernel32', '-lws2_32', '-lole32', '-loleaut32', '-luuid', '-luser32', '-ladvapi32', '-lshell32', '-lmpr', '-lgcc'], check=True)
    headers = subprocess.check_output([args.objdump, '-p', str(binary)], text=True)
    allowed = {'advapi32.dll', 'kernel32.dll', 'mpr.dll', 'ole32.dll', 'oleaut32.dll', 'shell32.dll', 'user32.dll', 'ws2_32.dll'}
    imports = {n.lower() for n in re.findall(r'DLL Name:\s*(\S+)', headers)}
    if not imports or imports - allowed:
        raise SystemExit('unexpected DLL imports: ' + repr(imports - allowed))
    if not re.search(r'MajorSubsystemVersion\s+5\s+MinorSubsystemVersion\s+1', headers):
        raise SystemExit('unexpected PE subsystem version')
    shutil.copyfile(ROOT/'packaging/tools/TOOLS.PY', args.output/'TOOLS.PY')
    shutil.copyfile(ROOT/'packaging/tools/README.txt', args.output/'README.txt')
    shutil.copyfile(ROOT/'LICENSE', args.output/'LICENSE.txt')
    for name in ('TOOLS.EXE', 'TOOLS.PY'):
        path = args.output / name
        path.with_name(name+'.sha256').write_text(hashlib.sha256(path.read_bytes()).hexdigest()+'\n', encoding='ascii')
    print('Guest Tools bundle: ' + str(args.output.resolve()))
    print('PE import/subsystem checks passed. Windows runtime qualification is separate.')

if __name__ == '__main__':
    main()
