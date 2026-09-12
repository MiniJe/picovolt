import hashlib
import json
from pathlib import Path
import shutil
import tempfile
import unittest
from scripts.check_public_release import validate

ROOT = Path(__file__).resolve().parents[2]


class PublicReleaseTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        for name in ['Cargo.toml', 'LICENSE', 'legal/PUBLIC-RELEASE.json',
                     'legal/PICOVOLT-PROPRIETARY-LICENSE-1.1.md', 'legal/APACHE-2.0-LEGACY.txt',
                     'bindings/python/LICENSE', 'bindings/python/pyproject.toml', 'bindings/go/LICENSE']:
            target = self.root / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, target)

    def test_valid_release(self):
        validate(self.root)

    def test_changed_terms_rejected(self):
        (self.root / 'LICENSE').write_text('Different terms')
        with self.assertRaisesRegex(ValueError, 'digest'):
            validate(self.root)

    def test_private_paid_or_different_version_rejected(self):
        path = self.root / 'legal/PUBLIC-RELEASE.json'
        original = json.loads(path.read_text())
        for key, value in [('version', '9.0.0'), ('account_required', True), ('fee', {'amount': 1, 'currency': 'EUR'}), ('distribution', 'private')]:
            with self.subTest(key=key):
                path.write_text(json.dumps({**original, key: value}))
                with self.assertRaises(ValueError):
                    validate(self.root)

    def test_binding_cannot_ship_different_terms(self):
        (self.root / 'bindings/python/LICENSE').write_text('Old draft')
        with self.assertRaisesRegex(ValueError, 'Inconsistent terms'):
            validate(self.root)
