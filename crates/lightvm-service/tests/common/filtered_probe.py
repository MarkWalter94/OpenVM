"""Disposable Linux guest probe. Results return through authenticated Tools."""
import json
import compileall
import hashlib
import pathlib
import socket
import ssl
import subprocess
import sys
import time
import zipapp

out = pathlib.Path.home() / "Documents" / "LightVM Tools" / "outbox"
out.mkdir(parents=True, exist_ok=True)
result = {"nonce": sys.argv[1]}

# Guest route changes must not open another path around the host policy.
route = subprocess.run(["sudo", "-n", "ip", "route", "replace", "default", "via", "10.0.2.2"], capture_output=True)
result["default_route"] = "changed" if route.returncode == 0 else "failed"

# Build and execute only inside this disposable guest, then return the
# real application artifact and its log through the authenticated Tools.
try:
    source = pathlib.Path.home() / "lightvm-test-application"
    source.mkdir(exist_ok=True)
    expected_output = "LightVM application " + sys.argv[1]
    (source / "__main__.py").write_text("print(" + repr(expected_output) + ")\n", encoding="utf-8")
    if not compileall.compile_dir(source, force=True, quiet=1):
        raise RuntimeError("application compilation failed")
    artifact = out / "application.pyz"
    zipapp.create_archive(source, target=artifact, interpreter="/usr/bin/env python3")
    execution = subprocess.run([sys.executable, str(artifact)], check=True, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    if execution.stdout.strip() != expected_output:
        raise RuntimeError("application output differs")
    (out / "build.log").write_text("compileall=passed\nzipapp=passed\nexecute=passed\n" + execution.stdout, encoding="utf-8")
    result["build"] = "passed"
    result["artifact_sha256"] = hashlib.sha256(artifact.read_bytes()).hexdigest()
except Exception as error:
    result["build_error"] = type(error).__name__ + ": " + str(error)

try:
    result["allowed_dns"] = socket.gethostbyname("example.com")
    with socket.create_connection(("example.com", 443), timeout=20) as raw:
        with ssl.create_default_context().wrap_socket(raw, server_hostname="example.com") as stream:
            result["tls"] = stream.version()
            stream.sendall(b"GET / HTTP/1.1\r\nHost: example.com\r\nConnection: close\r\n\r\n")
            data = stream.recv(4096)
            result["https"] = data.split(b"\r\n", 1)[0].decode("ascii")
except Exception as error:
    result["allowed_error"] = type(error).__name__ + ": " + str(error)

for name in ("www.iana.org", "localtest.me", "unlisted.invalid"):
    try:
        result["dns_" + name] = socket.gethostbyname(name)
    except OSError:
        result["dns_" + name] = "blocked"

for name, host, port in (
    ("other_port", "10.0.2.101", 22),
    ("ungranted_ip", "10.0.2.102", 443),
    ("internet_ip", "1.1.1.1", 443),
    ("host", "10.0.2.2", 443),
    ("metadata", "169.254.169.254", 80),
):
    try:
        with socket.create_connection((host, port), timeout=3):
            result[name] = "REACHED"
    except OSError:
        result[name] = "blocked"

try:
    with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as stream:
        stream.settimeout(3)
        stream.connect(("2606:4700:4700::1111", 443))
        result["ipv6"] = "REACHED"
except OSError:
    result["ipv6"] = "blocked"

try:
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as stream:
        stream.settimeout(3)
        query = bytes.fromhex("123401000001000000000000076578616d706c6503636f6d0000010001")
        stream.sendto(query, ("8.8.8.8", 53))
        stream.recv(4096)
        result["external_dns"] = "REACHED"
except OSError:
    result["external_dns"] = "blocked"

# Publish complete output, then leave an established TLS connection for
# the service's real session revocation / shutdown checks.
temp = out / "filtered.part"
temp.write_text(json.dumps(result, sort_keys=True), encoding="utf-8")
temp.replace(out / "filtered.json")
try:
    with socket.create_connection(("example.com", 443), timeout=20) as raw:
        with ssl.create_default_context().wrap_socket(raw, server_hostname="example.com") as stream:
            (out / "persistent.txt").write_text(sys.argv[1], encoding="ascii")
            stream.settimeout(90)
            stream.recv(1)
except OSError:
    pass
time.sleep(2)
