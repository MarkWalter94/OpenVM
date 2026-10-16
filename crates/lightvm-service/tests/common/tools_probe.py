"""Run inside the disposable Ubuntu guest, through the production WebDAV bridge."""
import urllib.error
import urllib.request

BASE = "http://127.0.0.1:47081"


def request(method, path, body=None, **headers):
    req = urllib.request.Request(BASE + path, data=body, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=20) as reply:
            return reply.status, reply.read(1024 * 1024 + 1)
    except urllib.error.HTTPError as error:
        return error.code, error.read(1024)


assert request("MKCOL", "/Builds/probe")[0] == 201
payload = b"real guest share write\n"
assert request("PUT", "/Builds/probe/first.txt", payload)[0] == 201
assert request("GET", "/Builds/probe/first.txt") == (200, payload)
assert request("MOVE", "/Builds/probe/first.txt", Destination=BASE + "/Builds/probe/final.txt")[0] == 201
assert request("PUT", "/Builds/probe/final.txt", b"replacement\n")[0] == 201
assert request("GET", "/ReadOnly/probe/final.txt") == (200, b"replacement\n")
assert request("PUT", "/ReadOnly/probe/final.txt", b"forbidden")[0] == 403
assert request("MKCOL", "/ReadOnly/forbidden")[0] == 403
assert request("DELETE", "/ReadOnly/probe/final.txt")[0] == 403
assert request("GET", "/Builds/%2e%2e/outside.txt")[0] == 403
assert request("GET", "/Builds/%2e%2e%5coutside.txt")[0] == 403
assert request("GET", "/Unapproved/probe/final.txt")[0] == 403
assert request("DELETE", "/Builds/probe/final.txt")[0] == 204
assert request("DELETE", "/Builds/probe")[0] == 204
print("WEBDAV_PROBE_PASSED: mkdir/read/write/replace/rename/delete/read-only/traversal/unknown-share")
