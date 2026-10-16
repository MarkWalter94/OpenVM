"""Guest component tests using temporary guest folders, without desktop APIs."""
import hashlib
import importlib.machinery
import importlib.util
import pathlib
import tempfile
import unittest
import threading
import urllib.error
import urllib.request

loader = importlib.machinery.SourceFileLoader('guest_tools', str(pathlib.Path(__file__).with_name('TOOLS.PY')))
spec = importlib.util.spec_from_loader(loader.name, loader)
guest = importlib.util.module_from_spec(spec)
loader.exec_module(guest)

class Files(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temp.name)
        self.files = guest.GuestFiles(self.root)
    def tearDown(self):
        self.files.clear()
        for fd in self.files.roots.values(): guest.os.close(fd)
        self.temp.cleanup()
    def begin(self, data, name='payload.zip'):
        return self.files.call(dict(op='begin', share='inbox', path=[name], size=len(data), sha256=hashlib.sha256(data).hexdigest()))['transfer']
    def test_verified_commit_cancel_and_no_replace(self):
        data = b'payload' * 5000
        token = self.begin(data)
        self.assertFalse((self.root/'inbox/payload.zip').exists())
        self.files.call(dict(op='write', transfer=token, offset=0, data=data.hex()))
        self.files.call(dict(op='commit', transfer=token))
        self.assertEqual((self.root/'inbox/payload.zip').read_bytes(), data)
        token = self.begin(b'new')
        self.files.call(dict(op='write', transfer=token, offset=0, data=b'new'.hex()))
        with self.assertRaises(FileExistsError): self.files.call(dict(op='commit', transfer=token))
        self.files.clear()
        self.assertEqual((self.root/'inbox/payload.zip').read_bytes(), data)
        self.assertEqual([p.name for p in (self.root/'inbox').iterdir()], ['payload.zip'])
    def test_bad_hash_and_gap_never_publish(self):
        token = self.begin(b'abc')
        with self.assertRaises(ValueError): self.files.call(dict(op='write', transfer=token, offset=1, data='616263'))
        token = self.begin(b'abc')
        self.files.call(dict(op='write', transfer=token, offset=0, data='646566'))
        with self.assertRaises(ValueError): self.files.call(dict(op='commit', transfer=token))
        self.assertEqual(list((self.root/'inbox').iterdir()), [])
    def test_symlink_and_reserved_paths_refused(self):
        (self.root/'outbox/link').symlink_to(self.root)
        with self.assertRaises(OSError): self.files.call(dict(op='read', share='outbox', path=['link','outside'], offset=0, length=10))
        for name in ('..', 'a/b', 'NUL', 'COM1.txt', 'x:stream', 'trail.'):
            with self.assertRaises(ValueError): guest.components([name])
    def test_streaming_read(self):
        data = b'outbox-data' * 10000
        (self.root/'outbox/output').write_bytes(data)
        result = bytearray()
        while len(result) < len(data):
            r = self.files.call(dict(op='read', share='outbox', path=['output'], offset=len(result), length=16384))
            result.extend(bytes.fromhex(r['data']))
        self.assertEqual(bytes(result), data)

class Images(unittest.TestCase):
    def setUp(self):
        class Desktop:
            data = guest.struct.pack('<4sII', b'LIMG', 101, 99) + b'\x42' * (101 * 99 * 4)
            writes = 0
            def image(self, data=None):
                if data is None: return self.data
                self.data = data
                self.writes += 1
        self.desktop = Desktop()
        self.images = guest.ClipboardImages(self.desktop)
    def test_verified_image_upload_and_stable_capture(self):
        data = self.desktop.data
        token = self.images.call(dict(op='begin', share='clipboard-write', path=['image'], size=len(data), sha256=hashlib.sha256(data).hexdigest()))['transfer']
        for offset in range(0, len(data), 16384):
            self.images.call(dict(op='write', transfer=token, offset=offset, data=data[offset:offset+16384].hex()))
        self.images.call(dict(op='commit', transfer=token))
        self.assertEqual(self.desktop.writes, 1)
        token = self.images.call(dict(op='begin', share='clipboard-read', path=['image'], size=0))['transfer']
        self.desktop.data = b'changed clipboard'
        got = bytearray()
        while True:
            reply = self.images.call(dict(op='read', share=token, path=['image'], offset=len(got), length=16384))
            got.extend(bytes.fromhex(reply['data']))
            if reply['eof']: break
        self.assertEqual(bytes(got), data)
        self.images.call(dict(op='cancel', transfer=token))
        self.assertIsNone(self.images.capture)
    def test_bad_digest_and_disconnect_do_not_replace_clipboard(self):
        data = self.desktop.data
        token = self.images.call(dict(op='begin', share='clipboard-write', path=['image'], size=len(data), sha256='0'*64))['transfer']
        for offset in range(0, len(data), 16384):
            self.images.call(dict(op='write', transfer=token, offset=offset, data=data[offset:offset+16384].hex()))
        with self.assertRaises(ValueError): self.images.call(dict(op='commit', transfer=token))
        self.assertEqual(self.desktop.writes, 0)
        self.images.clear()
        self.assertIsNone(self.images.upload)
    def test_image_dimensions_are_limited(self):
        for width, height in ((0, 1), (8193, 1), (4096, 4096)):
            with self.assertRaises(ValueError): self.images.dimensions(guest.struct.pack('<4sII', b'LIMG', width, height))

class WebDAV(unittest.TestCase):
    def test_invalid_paths_are_refused_before_any_host_request(self):
        class Channel:
            def call(self, request):
                raise AssertionError("invalid paths must not reach the host")
        server = guest.http.server.HTTPServer(('127.0.0.1', 0), guest.webdav_handler(Channel(), {'Builds'}))
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        try:
            for path in ('/Builds/%2e%2e/escape', '/Builds/a%5cb', '/Unapproved/secret'):
                with self.assertRaises(urllib.error.HTTPError) as error:
                    urllib.request.urlopen('http://127.0.0.1:%d%s' % (server.server_port, path), timeout=2)
                self.assertEqual(error.exception.code, 403)
                error.exception.close()
        finally:
            server.shutdown()
            server.server_close()
            worker.join()

if __name__ == '__main__': unittest.main()
