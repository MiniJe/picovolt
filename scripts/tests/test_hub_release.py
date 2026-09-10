"""Exercise release packaging with the real signing/verification CLI."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('hub_release', ROOT / 'scripts/hub_release.py')
hub = importlib.util.module_from_spec(spec)
spec.loader.exec_module(hub)
PV = Path(os.environ.get('PICOVOLT_TEST_PV', ROOT / 'target/debug' / ('pv.exe' if os.name == 'nt' else 'pv')))


@unittest.skipUnless(PV.is_file(), 'build pv with --features data-tools to run real Hub integration tests')
class HubReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.image = self.root / 'catalog.pvdb'
        self.manifest = self.root / 'signed.json'
        self.output = self.root / 'release'
        self.run_pv('query', str(self.root / 'data'), 'CREATE TABLE products (id, name)')
        self.run_pv('query', str(self.root / 'data'), "INSERT INTO products VALUES (1, 'Keyboard')")
        self.run_pv('bake', str(self.root / 'data'), str(self.image))
        secret = self.root / 'publisher.secret'
        self.key = self.run_pv('dataset', 'keygen', str(secret)).strip()
        self.run_pv('dataset', 'sign', str(self.image), '--key', str(secret),
                    '--name', 'catalog@r1', '--output', str(self.manifest))

    def run_pv(self, *args):
        return subprocess.run([str(PV), *args], check=True, capture_output=True, text=True).stdout

    def prepare(self, **changes):
        args = dict(image=self.image, manifest=self.manifest, output=self.output,
                    dataset='catalog', release='r1', public_key=self.key, pv=PV)
        args.update(changes)
        return hub.prepare(**args)

    def test_real_signed_bundle_and_no_overwrite(self):
        result = self.prepare()
        copied = (self.output / 'dataset.pvdb').read_bytes()
        self.assertEqual(hashlib.sha256(copied).hexdigest(), result['files']['dataset.pvdb']['sha256'])
        self.assertEqual(result, json.loads((self.output / 'release.json').read_text()))
        self.assertFalse((self.root / 'release.prepare.lock').exists())
        with self.assertRaises(FileExistsError):
            self.prepare()
        self.assertEqual(copied, (self.output / 'dataset.pvdb').read_bytes())

    def test_corruption_and_wrong_key_leave_no_release(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.prepare(public_key='00' * 32)
        self.image.write_bytes(self.image.read_bytes() + b'tamper')
        with self.assertRaises(subprocess.CalledProcessError):
            self.prepare()
        self.assertFalse(self.output.exists())
        self.assertFalse((self.root / 'release.prepare.lock').exists())

    def test_replay_identity_and_traversal_rejected(self):
        for changes in ({'release': 'r2'}, {'dataset': '../escape'}, {'public_key': 'bad'}):
            with self.assertRaises(ValueError):
                self.prepare(**changes)
        self.assertFalse(self.output.exists())

    def test_existing_lock_is_not_removed(self):
        lock = self.root / 'release.prepare.lock'
        lock.write_text('other operation')
        with self.assertRaises(FileExistsError):
            self.prepare()
        self.assertEqual(lock.read_text(), 'other operation')


if __name__ == '__main__':
    unittest.main()
