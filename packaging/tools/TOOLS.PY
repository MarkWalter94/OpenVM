#!/usr/bin/env python3
"""LightVM Tools guest component. Run inside a Linux graphical session.

The bootstrap is read from the run's read-only Tools medium. No host paths,
shell commands, credentials or file contents are logged.
"""
import argparse
import hashlib
import http.server
import json
import os
import pathlib
import socket
import struct
import subprocess
import sys
import threading
import time
import urllib.parse
import xml.sax.saxutils as xml

MAX_FRAME = 256 * 1024
CHUNK = 64 * 1024
MAX_FILE = 4 * 1024**3
MAX_HTTP = 32 * 1024


def exact(stream, size):
    result = bytearray()
    while len(result) < size:
        part = stream.recv(size - len(result))
        if not part:
            raise EOFError("channel closed")
        result.extend(part)
    return bytes(result)


def receive(stream):
    size = struct.unpack("!I", exact(stream, 4))[0]
    if not 0 < size <= MAX_FRAME:
        raise ValueError("frame limit")
    return json.loads(exact(stream, size))


def send(stream, value):
    data = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    if not 0 < len(data) <= MAX_FRAME:
        raise ValueError("frame limit")
    stream.sendall(struct.pack("!I", len(data)) + data)


class Channel:
    def __init__(self, bootstrap, control=False):
        self.config, self.control, self.stream = bootstrap, control, None
        self.lock = threading.Lock()

    def connect(self):
        cfg = self.config
        port = cfg["control_port" if self.control else "files_port"]
        stream = socket.create_connection((cfg["host"], port), timeout=5)
        stream.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        key = "control_credential" if self.control else "credential"
        try:
            send(stream, {"version": 1, "vm": cfg["vm"], "run": cfg["run"], "credential": cfg[key]})
            if receive(stream) != {"version": 1}:
                raise ValueError("protocol version")
        except BaseException:
            stream.close()
            raise
        self.stream = stream

    def close(self):
        if self.stream:
            self.stream.close()
            self.stream = None

    def call(self, request):
        with self.lock:
            try:
                if not self.stream:
                    self.connect()
                send(self.stream, request)
                return receive(self.stream)
            except (OSError, EOFError, ValueError):
                self.close()
                # Writes are never replayed after an uncertain response.
                raise


def components(value):
    if not isinstance(value, list) or len(value) > 64:
        raise ValueError("invalid path")
    for part in value:
        if (not isinstance(part, str) or not part or len(part.encode()) > 255
                or part in (".", "..") or part.endswith((".", " "))
                or part.split(".")[0].upper() in {"CON", "PRN", "AUX", "NUL", *("COM"+str(i) for i in range(1, 10)), *("LPT"+str(i) for i in range(1, 10))}
                or any(ord(c) < 32 or c in '/\\:<>"|?*' for c in part)):
            raise ValueError("invalid path")
    return value


class GuestFiles:
    """Guest-local inbox/outbox only. These are never host file paths."""
    def __init__(self, directory):
        self.roots = {}
        self.transfers = {}
        for name in ("inbox", "outbox"):
            root = directory / name
            root.mkdir(parents=True, exist_ok=True, mode=0o700)
            self.roots[name] = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)

    def parent(self, request):
        parts = components(request["path"])
        fd = os.dup(self.roots[request["share"]])
        try:
            for part in parts[:-1]:
                child = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
                os.close(fd)
                fd = child
            return fd, parts[-1] if parts else None
        except BaseException:
            os.close(fd)
            raise

    def cancel(self, transfer):
        item = self.transfers.pop(transfer)
        os.close(item["file"])
        try:
            os.unlink(item["stage"], dir_fd=item["parent"])
        finally:
            os.close(item["parent"])

    def clear(self):
        for transfer in list(self.transfers):
            try:
                self.cancel(transfer)
            except OSError:
                pass

    def call(self, request):
        op = request["op"]
        if op in ("write", "commit", "cancel"):
            transfer = request["transfer"]
            item = self.transfers[transfer]
            if op == "cancel":
                self.cancel(transfer)
            elif op == "write":
                data = bytes.fromhex(request["data"])
                if (not data or len(data) > CHUNK or request["offset"] != item["written"]
                        or len(data) > item["size"] - item["written"]):
                    self.cancel(transfer)
                    raise ValueError("invalid chunk")
                view = memoryview(data)
                while view:
                    count = os.write(item["file"], view)
                    if not count:
                        raise OSError("write failed")
                    view = view[count:]
                item["hash"].update(data)
                item["written"] += len(data)
            else:
                if item["written"] != item["size"] or item["hash"].hexdigest() != item["expected"]:
                    self.cancel(transfer)
                    raise ValueError("checksum mismatch")
                os.fsync(item["file"])
                os.link(item["stage"], item["name"], src_dir_fd=item["parent"], dst_dir_fd=item["parent"], follow_symlinks=False)
                self.cancel(transfer)
            return {"result": "ok"}
        parent, name = self.parent(request)
        try:
            if op == "read":
                length, offset = request["length"], request["offset"]
                if not 0 < length <= CHUNK or not 0 <= offset <= MAX_FILE or not name:
                    raise ValueError("read limit")
                fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
                try:
                    import stat
                    metadata = os.fstat(fd)
                    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > MAX_FILE or offset > metadata.st_size:
                        raise ValueError("invalid file")
                    data = os.pread(fd, length, offset)
                    return {"result": "data", "offset": offset, "data": data.hex(), "eof": offset + len(data) >= metadata.st_size}
                finally:
                    os.close(fd)
            if op == "begin":
                size, digest = request["size"], request["sha256"]
                if not name or not 0 <= size <= MAX_FILE or len(self.transfers) >= 8 or len(bytes.fromhex(digest)) != 32:
                    raise ValueError("transfer limit")
                transfer = os.urandom(32).hex()
                stage = ".lightvm-transfer-" + transfer
                fd = os.open(stage, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600, dir_fd=parent)
                self.transfers[transfer] = dict(parent=os.dup(parent), file=fd, stage=stage, name=name,
                                                size=size, written=0, expected=digest, hash=hashlib.sha256())
                return {"result": "transfer", "transfer": transfer}
            if op == "list":
                directory = parent if name is None else os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=parent)
                try:
                    import stat
                    entries = []
                    for child in os.listdir(directory):
                        if child.startswith(".lightvm-transfer-"):
                            continue
                        metadata = os.stat(child, dir_fd=directory, follow_symlinks=False)
                        if stat.S_ISLNK(metadata.st_mode):
                            continue
                        entries.append(dict(name=child, directory=stat.S_ISDIR(metadata.st_mode), size=metadata.st_size))
                        if len(entries) > 512:
                            raise ValueError("directory limit")
                    return {"result": "entries", "entries": entries}
                finally:
                    if directory != parent:
                        os.close(directory)
            raise ValueError("unsupported operation")
        finally:
            os.close(parent)


class Desktop:
    def __init__(self):
        self.tree, self.nodes = None, {}

    def clipboard(self, text=None):
        # Helpers run only in the guest, with fixed executables/arguments.
        # No shell, command strings or host paths are accepted by this API.
        if os.environ.get("WAYLAND_DISPLAY"):
            command = ["wl-copy", "--type", "text/plain;charset=utf-8"] if text is not None else ["wl-paste", "--no-newline"]
        else:
            command = ["xclip", "-selection", "clipboard"] + (["-in"] if text is not None else ["-out"])
        if text is not None and (len(text.encode()) > CHUNK or "\0" in text):
            raise ValueError("clipboard limit")
        # Capture into a bounded temporary file, never an unbounded PIPE.
        import tempfile
        with tempfile.TemporaryFile() as output:
            subprocess.run(command, input=text.encode() if text is not None else None, stdout=output,
                           stderr=subprocess.DEVNULL, timeout=3, check=True)
            if text is None:
                output.seek(0)
                result = output.read(CHUNK + 1)
                if len(result) > CHUNK:
                    raise ValueError("clipboard limit")
                return result.decode("utf-8")

    @staticmethod
    def clean(text):
        return "".join(c for c in str(text or "") if ord(c) >= 32 or c in "\n\t")[:1024]

    def inspect(self, ident, depth, count):
        import pyatspi
        if not 1 <= depth <= 32 or not 1 <= count <= 1000:
            raise ValueError("tree limit")
        desktop = pyatspi.Registry.getDesktop(0)
        window = None
        for app in desktop:
            for child in app:
                if child.getState().contains(pyatspi.STATE_ACTIVE):
                    window = child
                    break
            if window:
                break
        if window is None:
            raise ValueError("foreground window unavailable")
        self.nodes = {}
        result, pending = [], [(window, None, 0)]
        while pending and len(result) < count:
            node, parent, level = pending.pop()
            ident_node = len(result)
            self.nodes[ident_node] = node
            rectangle = node.queryComponent().getExtents(pyatspi.DESKTOP_COORDS)
            state = node.getState()
            value = None
            try:
                if node.getRole() != pyatspi.ROLE_PASSWORD_TEXT:
                    text = node.queryText()
                    value = self.clean(text.getText(0, min(text.characterCount, 1024)))
            except (NotImplementedError, AttributeError):
                pass
            result.append(dict(id=ident_node, parent=parent, control_type=self.clean(node.getRoleName()),
                               name=self.clean(node.name), automation_id="", value=value,
                               enabled=state.contains(pyatspi.STATE_ENABLED), focused=state.contains(pyatspi.STATE_FOCUSED),
                               rect=[rectangle.x, rectangle.y, max(0, rectangle.width), max(0, rectangle.height)]))
            if level < depth:
                children = [node.getChildAtIndex(i) for i in range(min(node.childCount, count))]
                pending.extend((child, ident_node, level + 1) for child in reversed(children) if child is not None)
        self.tree = ident
        self.window = window
        return dict(result="tree", tree=ident, nodes=result, truncated=bool(pending))

    def invoke(self, command):
        import pyatspi
        if not self.window.getState().contains(pyatspi.STATE_ACTIVE):
            raise ValueError("foreground window changed")
        if command["tree"] != self.tree or command["element"] not in self.nodes:
            raise ValueError("stale element")
        node = self.nodes[command["element"]]
        action = command["action"]
        if action == "set_value":
            text = command.get("value") or ""
            if len(text.encode()) > 16 * 1024:
                raise ValueError("value limit")
            if not node.queryEditableText().setTextContents(text):
                raise ValueError("set value refused")
        elif action == "select":
            if not node.parent.querySelection().selectChild(node.getIndexInParent()):
                raise ValueError("selection refused")
        elif action == "invoke":
            actions = node.queryAction()
            if not actions.nActions or not actions.doAction(0):
                raise ValueError("invoke refused")
        else:
            raise ValueError("unknown action")


def control_loop(bootstrap):
    channel, desktop = Channel(bootstrap, True), Desktop()
    files = GuestFiles(pathlib.Path.home() / "Documents" / "LightVM Tools")
    while True:
        try:
            channel.connect()
            channel.stream.settimeout(None)
            while True:
                request = receive(channel.stream)
                command, ident = request["command"], request["id"]
                try:
                    op = command["op"]
                    if op == "clipboard_read":
                        reply = dict(result="clipboard", text=desktop.clipboard())
                    elif op == "clipboard_write":
                        desktop.clipboard(command["text"])
                        reply = dict(result="ok")
                    elif op == "ui_tree":
                        reply = desktop.inspect(ident, command["max_depth"], command["max_nodes"])
                    elif op == "ui_invoke":
                        desktop.invoke(command)
                        reply = dict(result="ok")
                    elif op == "file":
                        reply = dict(result="file", reply=files.call(command["request"]))
                    elif op == "flush":
                        os.sync()
                        reply = dict(result="ok")
                    else:
                        reply = dict(result="error", code="unsupported")
                except (Exception,):
                    reply = dict(result="error", code="guest_operation_failed")
                send(channel.stream, dict(id=ident, reply=reply))
        except (OSError, EOFError, ValueError, KeyError):
            channel.close()
            files.clear()
            desktop.tree, desktop.nodes = None, {}
            time.sleep(1)


def webdav_handler(channel, shares):
    class WebDAV(http.server.BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        def log_message(self, *args):
            pass

        def parts(self, url=None):
            value = urllib.parse.unquote(urllib.parse.urlsplit(url or self.path).path)
            parts = components([p for p in value.split("/") if p])
            if parts and parts[0] == "DavWWWRoot":
                parts.pop(0)
            if not parts or parts[0] not in shares:
                raise ValueError("unknown share")
            return parts[0], parts[1:]

        def request_file(self, op, **kwargs):
            share, path = self.parts()
            reply = channel.call(dict(op=op, share=share, path=path, **kwargs))
            if reply.get("result") == "error":
                raise PermissionError(reply.get("code", "file_error"))
            return reply

        def finish_response(self, code, data=b"", content_type="application/octet-stream"):
            self.send_response(code)
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Content-Type", content_type)
            self.send_header("DAV", "1")
            self.send_header("Connection", "close")
            self.end_headers()
            if self.command != "HEAD":
                self.wfile.write(data)
            self.close_connection = True

        def do_OPTIONS(self):
            self.send_response(200)
            self.send_header("Allow", "OPTIONS, PROPFIND, GET, HEAD, PUT, MKCOL, DELETE, MOVE, LOCK, UNLOCK, PROPPATCH")
            self.send_header("DAV", "1")
            self.send_header("Content-Length", "0")
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True

        def do_PROPFIND(self):
            try:
                if self.headers.get("Depth", "1") not in ("0", "1"):
                    return self.finish_response(403)
                if urllib.parse.urlsplit(self.path).path.rstrip("/") in ("", "/DavWWWRoot"):
                    entries = [("/", dict(name="LightVM Tools", directory=True, size=0))]
                    if self.headers.get("Depth", "1") == "1":
                        entries.extend(("/" + urllib.parse.quote(name) + "/", dict(name=name, directory=True, size=0)) for name in sorted(shares))
                    return self.multistatus(entries)
                share, path = self.parts()
                parent = channel.call(dict(op="list", share=share, path=path[:-1])) if path else None
                if path:
                    item = next((e for e in parent.get("entries", []) if e["name"] == path[-1]), None)
                    if item is None:
                        return self.finish_response(404)
                else:
                    item = dict(name=share, directory=True, size=0)
                entries = [(self.path, item)]
                if item["directory"] and self.headers.get("Depth", "1") == "1":
                    listing = self.request_file("list")
                    entries.extend((self.path.rstrip("/") + "/" + urllib.parse.quote(e["name"]), e) for e in listing["entries"])
                self.multistatus(entries)
            except (OSError, ValueError, KeyError, EOFError):
                self.finish_response(403)

        def multistatus(self, entries):
            data = ['<?xml version="1.0" encoding="utf-8"?><D:multistatus xmlns:D="DAV:">']
            for href, entry in entries:
                kind = "<D:collection/>" if entry["directory"] else ""
                data.append("<D:response><D:href>" + xml.escape(href) + "</D:href><D:propstat><D:prop><D:displayname>" + xml.escape(entry["name"]) + "</D:displayname><D:resourcetype>" + kind + "</D:resourcetype><D:getcontentlength>" + str(entry["size"]) + "</D:getcontentlength></D:prop><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>")
            data.append("</D:multistatus>")
            self.finish_response(207, "".join(data).encode(), "application/xml; charset=utf-8")

        def do_GET(self):
            try:
                share, path = self.parts()
                entries = channel.call(dict(op="list", share=share, path=path[:-1]))
                entry = next((e for e in entries.get("entries", []) if path and e["name"] == path[-1]), None)
                if entry is None or entry["directory"]:
                    return self.finish_response(404)
                self.send_response(200)
                self.send_header("Content-Length", str(entry["size"]))
                self.send_header("Content-Type", "application/octet-stream")
                self.send_header("Connection", "close")
                self.end_headers()
                self.close_connection = True
                if self.command == "HEAD":
                    return
                offset = 0
                while offset < entry["size"]:
                    reply = self.request_file("read", offset=offset, length=CHUNK)
                    data = bytes.fromhex(reply["data"])
                    if not data or len(data) > CHUNK:
                        raise ValueError("invalid chunk")
                    self.wfile.write(data)
                    offset += len(data)
            except (OSError, ValueError, KeyError, EOFError):
                self.close_connection = True

        do_HEAD = do_GET

        def do_PUT(self):
            transfer = None
            try:
                size = int(self.headers.get("Content-Length", "-1"))
                if not 0 <= size <= MAX_FILE:
                    return self.finish_response(413)
                # WebDAV has no preannounced SHA-256. Spool inside the guest
                # with bounded RAM, then send with the native verified path.
                import tempfile
                with tempfile.TemporaryFile() as file:
                    digest, remaining = hashlib.sha256(), size
                    while remaining:
                        data = self.rfile.read(min(CHUNK, remaining))
                        if not data:
                            raise EOFError("short upload")
                        file.write(data)
                        digest.update(data)
                        remaining -= len(data)
                    transfer = self.request_file("begin", size=size, sha256=digest.hexdigest(), replace=True)["transfer"]
                    file.seek(0)
                    offset = 0
                    while True:
                        data = file.read(CHUNK)
                        if not data:
                            break
                        reply = channel.call(dict(op="write", transfer=transfer, offset=offset, data=data.hex()))
                        if reply.get("result") != "ok":
                            raise PermissionError("write refused")
                        offset += len(data)
                    if channel.call(dict(op="commit", transfer=transfer)).get("result") != "ok":
                        raise PermissionError("commit refused")
                    transfer = None
                self.finish_response(201)
            except (OSError, ValueError, KeyError, EOFError):
                self.finish_response(403)
            finally:
                if transfer:
                    try:
                        channel.call(dict(op="cancel", transfer=transfer))
                    except (OSError, EOFError, ValueError):
                        pass

        def do_MKCOL(self):
            try:
                self.request_file("mkdir")
                self.finish_response(201)
            except (OSError, ValueError, KeyError, EOFError):
                self.finish_response(403)

        def do_DELETE(self):
            try:
                self.request_file("remove")
                self.finish_response(204)
            except (OSError, ValueError, KeyError, EOFError):
                self.finish_response(403)

        def do_MOVE(self):
            try:
                share, source = self.parts()
                destination_share, destination = self.parts(self.headers["Destination"])
                if share != destination_share:
                    raise ValueError("cross-share move")
                reply = channel.call(dict(op="rename", share=share, **{"from": source}, to=destination, replace=self.headers.get("Overwrite", "F") == "T"))
                if reply.get("result") != "ok":
                    raise PermissionError("move refused")
                self.finish_response(201)
            except (OSError, ValueError, KeyError, EOFError):
                self.finish_response(403)

        def do_LOCK(self):
            # Locks are advisory within this guest's single file channel.
            token = "opaquelocktoken:" + os.urandom(16).hex()
            self.send_response(200)
            self.send_header("Lock-Token", "<" + token + ">")
            data = ('<D:prop xmlns:D="DAV:"><D:lockdiscovery><D:activelock><D:locktype><D:write/></D:locktype><D:lockscope><D:exclusive/></D:lockscope><D:depth>0</D:depth><D:timeout>Second-300</D:timeout><D:locktoken><D:href>' + token + '</D:href></D:locktoken></D:activelock></D:lockdiscovery></D:prop>').encode()
            self.send_header("Content-Type", "application/xml")
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(data)
            self.close_connection = True

        def do_UNLOCK(self):
            self.finish_response(204)

        def do_PROPPATCH(self):
            self.finish_response(207, b'<D:multistatus xmlns:D="DAV:"/>', "application/xml")

        def handle_one_request(self):
            self.connection.settimeout(15)
            super().handle_one_request()
    return WebDAV


def run(config):
    if config.get("version") != 1 or config.get("host") != "10.0.2.99":
        raise ValueError("invalid Tools bootstrap")
    for key in ("credential", "control_credential"):
        if len(config[key]) != 64 or len(bytes.fromhex(config[key])) != 32:
            raise ValueError("invalid Tools credential")
    channel = Channel(config)
    threading.Thread(target=control_loop, args=(config,), daemon=True).start()
    # The listener is guest-loopback only, never a host/LAN service.
    server = http.server.HTTPServer(("127.0.0.1", 47081), webdav_handler(channel, {s["name"] for s in config["shares"]}))
    server.serve_forever(poll_interval=0.5)


def install(source):
    directory = pathlib.Path.home() / ".local" / "share" / "lightvm-tools"
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    target = directory / "tools.py"
    target.write_bytes(source.read_bytes())
    target.chmod(0o700)
    autostart = pathlib.Path.home() / ".config" / "autostart"
    autostart.mkdir(parents=True, exist_ok=True)
    # Desktop Exec arguments are quoted as literal paths, never shell text.
    escaped = str(target).replace("\\", "\\\\").replace('"', '\\"').replace("`", "\\`").replace("$", "\\$").replace("%", "%%")
    (autostart / "lightvm-tools.desktop").write_text('[Desktop Entry]\nType=Application\nName=LightVM Tools\nExec=python3 "' + escaped + '"\nTerminal=false\n', encoding="utf-8")


def children(folder):
    try:
        if folder.is_symlink() or not folder.is_dir():
            return []
        import itertools
        return list(itertools.islice(folder.iterdir(), 64))
    except OSError:
        return []


def find_bootstrap():
    for base in ("/media", "/run/media", "/mnt"):
        root = pathlib.Path(base)
        if not root.exists():
            continue
        # At most user/volume nesting, no recursive scan of guest disks.
        for folder in [root] + children(root):
            if folder.is_dir():
                for candidate in [folder] + children(folder):
                    if candidate.name.upper() == "LIGHTVM_TOOLS":
                        for name in ("tools.cfg", "TOOLS.CFG"):
                            file = candidate / name
                            if file.is_file() and file.stat().st_size <= 16 * 1024:
                                return file
    return None


def main():
    parser = argparse.ArgumentParser(description="LightVM guest Tools")
    parser.add_argument("--install", action="store_true")
    parser.add_argument("--config", type=pathlib.Path)
    arguments = parser.parse_args()
    if arguments.install:
        install(pathlib.Path(__file__))
    while True:
        file = arguments.config or find_bootstrap()
        if file:
            with file.open("rb") as stream:
                data = stream.read(16 * 1024 + 1)
            if len(data) > 16 * 1024:
                raise ValueError("bootstrap limit")
            run(json.loads(data))
            return
        time.sleep(2)


if __name__ == "__main__":
    main()
