"""Guest component tests using temporary guest folders, without desktop APIs."""
import hashlib
import importlib.machinery
import importlib.util
import pathlib
import tempfile
import unittest

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

if __name__ == '__main__': unittest.main()
