#!/usr/bin/env python3
"""Create a disposable FAT answer-file medium (requires mkfs.fat and mtools).

The answer file only starts the lab probe: it never installs Windows, selects
a disk or changes guest requirements. All writes are under --output.
"""
import argparse
import pathlib
import shutil
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[2]

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    image = args.output / 'answer.img'
    with image.open('xb') as stream:
        stream.truncate(64 * 1024 * 1024)
    subprocess.run(['mkfs.fat', '-F', '16', '-n', 'LIGHTVMTEST', str(image)], check=True)
    xml = ROOT / 'crates/lightvm-service/tests/common/winpe-unattend.xml'
    subprocess.run(['mcopy', '-i', str(image), str(xml), '::autounattend.xml'], check=True)
    shutil.copyfile(ROOT / 'crates/lightvm-service/tests/common/winpe-probe.cmd', args.output / 'PROBE.CMD')
    print('Disposable answer-file medium: ' + str(image))

if __name__ == '__main__':
    main()
