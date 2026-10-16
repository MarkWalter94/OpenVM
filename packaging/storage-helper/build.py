#!/usr/bin/env python3
"""Builds the LightVM storage-driver preparation helper (ADR-0030).

A minimal Linux appliance booted headless by QEMU (-kernel/-initrd) to edit
the offline SYSTEM hive of an imported Windows guest (see ./init):

    lightvm-storage-helper.vmlinuz   Alpine linux-virt kernel, unchanged
    lightvm-storage-helper.initrd    gzip newc initramfs: ./init, busybox
                                     (static), musl, ntfs-3g, chntpw reged,
                                     three kernel modules
    SHA256SUMS, PACKAGES             hashes; packages, licences, sources

Inputs are Alpine Linux 3.24 x86_64 binary packages pinned by SHA-256
(packages.lock); nothing else is downloaded. The output is reproducible:
the same lock and the same ./init give the same bytes (sorted entries,
fixed metadata, mtime 0, gzip without name or time).

usage:
    python3 -I build.py download <empty-download-dir>
    python3 -I build.py build <download-dir> <out-dir>

Runs on any host with Python 3.8+ (no root, no Linux tools needed). The
downloaded packages are untrusted until their SHA-256 matches the lock.
"""

import gzip
import hashlib
import io
import os
import sys
import tarfile
import urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
MIRROR = "https://dl-cdn.alpinelinux.org/alpine/v3.24"
KVER = "6.18.56-0-virt"


def lock():
    """(repo, name, version, sha256) from packages.lock."""
    out = []
    with open(os.path.join(HERE, "packages.lock"), encoding="ascii") as f:
        for line in f:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            repo, name, ver, sha = line.split()
            out.append((repo, name, ver, sha))
    return out


def apk_name(name, ver):
    return f"{name}-{ver}.apk"


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def download(dst):
    os.makedirs(dst, exist_ok=True)
    for repo, name, ver, want in lock():
        p = os.path.join(dst, apk_name(name, ver))
        if not os.path.exists(p):
            url = f"{MIRROR}/{repo}/x86_64/{apk_name(name, ver)}"
            print("download", url)
            with urllib.request.urlopen(url, timeout=300) as r, open(p + ".part", "wb") as f:
                while True:
                    b = r.read(1 << 20)
                    if not b:
                        break
                    f.write(b)
            os.replace(p + ".part", p)
        got = sha256(p)
        if got != want:
            sys.exit(f"{p}: SHA-256 {got}, lock says {want}")
    print("all packages match packages.lock")


def apk_members(path):
    """Members of an .apk (concatenated gzip tar streams): name -> (TarInfo, bytes)."""
    with open(path, "rb") as f:
        raw = gzip.decompress(f.read())
    out = {}
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:", ignore_zeros=True) as t:
        for m in t:
            data = t.extractfile(m).read() if m.isfile() else b""
            out[m.name] = (m, data)
    return out


def pkginfo(members):
    text = members[".PKGINFO"][1].decode("utf-8", "replace")
    info = {}
    for line in text.splitlines():
        if " = " in line and not line.startswith("#"):
            k, v = line.split(" = ", 1)
            info.setdefault(k, v)
    return info


class Cpio:
    """Deterministic newc archive."""

    def __init__(self):
        self.entries = {}

    def add(self, path, mode, data=b"", rdev=(0, 0)):
        self.entries[path] = (mode, data, rdev)

    def dir(self, path):
        self.add(path, 0o040755)

    def file(self, path, data, exe=False):
        self.add(path, 0o100755 if exe else 0o100644, data)

    def symlink(self, path, target):
        self.add(path, 0o120777, target.encode())

    def chardev(self, path, major, minor, perm=0o600):
        self.add(path, 0o020000 | perm, b"", (major, minor))

    def bytes(self):
        out = bytearray()

        def rec(name, mode, data, ino, rdev):
            nb = name.encode() + b"\0"
            hdr = "070701" + "".join(
                f"{v:08x}"
                for v in (ino, mode, 0, 0, 1, 0, len(data), 0, 0, rdev[0], rdev[1], len(nb), 0)
            )
            out.extend(hdr.encode())
            out.extend(nb)
            out.extend(b"\0" * ((4 - (110 + len(nb)) % 4) % 4))
            out.extend(data)
            out.extend(b"\0" * ((4 - len(data) % 4) % 4))

        for ino, path in enumerate(sorted(self.entries), start=1):
            mode, data, rdev = self.entries[path]
            rec(path, mode, data, ino, rdev)
        rec("TRAILER!!!", 0, b"", 0, (0, 0))
        return bytes(out)


def build(src, out):
    os.makedirs(out, exist_ok=True)
    pkgs = {}
    for repo, name, ver, want in lock():
        p = os.path.join(src, apk_name(name, ver))
        got = sha256(p)
        if got != want:
            sys.exit(f"{p}: SHA-256 {got}, lock says {want}")
        pkgs[name] = apk_members(p)

    def take(pkg, member):
        m = pkgs[pkg].get(member)
        if m is None:
            sys.exit(f"{pkg}: {member} not found")
        info, data = m
        if info.issym():
            return take(pkg, os.path.normpath(os.path.join(os.path.dirname(member), info.linkname)))
        return data

    c = Cpio()
    for d in ("bin", "dev", "etc", "lib", "lib/modules", "mnt", "proc", "root", "sys", "tmp", "usr", "usr/bin", "usr/lib"):
        c.dir(d)
    c.chardev("dev/console", 5, 1)
    c.chardev("dev/null", 1, 3, 0o666)
    with open(os.path.join(HERE, "init"), "rb") as f:
        c.file("init", f.read().replace(b"\r\n", b"\n"), exe=True)
    c.file("bin/busybox", take("busybox-static", "bin/busybox.static"), exe=True)
    c.symlink("bin/sh", "busybox")
    c.file("lib/ld-musl-x86_64.so.1", take("musl", "lib/ld-musl-x86_64.so.1"), exe=True)
    c.symlink("lib/libc.musl-x86_64.so.1", "ld-musl-x86_64.so.1")
    c.file("usr/bin/ntfs-3g", take("ntfs-3g", "bin/ntfs-3g"), exe=True)
    c.file("usr/lib/libntfs-3g.so.89", take("ntfs-3g-libs", "usr/lib/libntfs-3g.so.89"), exe=True)
    c.file("usr/bin/reged", take("chntpw", "usr/bin/reged"), exe=True)
    for mod in (
        "kernel/drivers/block/virtio_blk.ko.gz",
        "kernel/fs/fuse/fuse.ko.gz",
        "kernel/drivers/firmware/qemu_fw_cfg.ko.gz",
    ):
        data = gzip.decompress(take("linux-virt", f"lib/modules/{KVER}/{mod}"))
        c.file("lib/modules/" + os.path.basename(mod)[: -len(".gz")], data)

    initrd = gzip.compress(c.bytes(), compresslevel=9, mtime=0)
    kernel = take("linux-virt", "boot/vmlinuz-virt")
    files = {
        "lightvm-storage-helper.vmlinuz": kernel,
        "lightvm-storage-helper.initrd": initrd,
    }
    lines = []
    for repo, name, ver, want in lock():
        i = pkginfo(pkgs[name])
        lines.append(
            f"{name} {ver} license={i.get('license', '?')} origin={i.get('origin', '?')} "
            f"aports-commit={i.get('commit', '?')} url={i.get('url', '?')} sha256={want}"
        )
    lines.append("")
    lines.append("Sources: https://gitlab.alpinelinux.org/alpine/aports at the commits above")
    lines.append(f"(the source packages are at {MIRROR}/ and in the aports history).")
    files["PACKAGES"] = ("\n".join(lines) + "\n").encode()
    for n, data in files.items():
        with open(os.path.join(out, n), "wb") as f:
            f.write(data)
    sums = "".join(
        f"{hashlib.sha256(files[n]).hexdigest()}  {n}\n"
        for n in ("lightvm-storage-helper.vmlinuz", "lightvm-storage-helper.initrd")
    )
    with open(os.path.join(out, "SHA256SUMS"), "w", encoding="ascii", newline="\n") as f:
        f.write(sums)
    print(sums, end="")
    print(f"initrd {len(initrd)} bytes, kernel {len(kernel)} bytes")


def main(argv):
    if len(argv) == 3 and argv[1] == "download":
        download(argv[2])
    elif len(argv) == 4 and argv[1] == "build":
        build(argv[2], argv[3])
    else:
        sys.exit(__doc__)


if __name__ == "__main__":
    main(sys.argv)
