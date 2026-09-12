"""Validate explicit distribution authorization before public publication."""
from pathlib import Path
import hashlib
import json
import tomllib


def validate(root):
    package = tomllib.loads((root / 'Cargo.toml').read_text())['package']
    if package.get('publish') is False:
        raise ValueError('Package publication is disabled')
    if package.get('license') == 'Apache-2.0':
        return
    notice = json.loads((root / 'legal/PUBLIC-RELEASE.json').read_text())
    expected = 'LicenseRef-PicoVolt-Public-Source-1.1'
    if (notice.get('schema_version') != 1 or notice.get('version') != package['version']
            or notice.get('distribution') != 'public-source-free'
            or notice.get('license_id') != expected or notice.get('account_required') is not False
            or notice.get('fee') != {'amount': 0, 'currency': 'EUR'}
            or not notice.get('covered_components') or not notice.get('preserved_licenses')):
        raise ValueError('Missing or inconsistent public release notice')
    terms = (root / 'LICENSE').read_bytes()
    if hashlib.sha256(terms).hexdigest() != notice.get('license_sha256'):
        raise ValueError('License digest does not match the authorized notice')
    for name in ['legal/PICOVOLT-PROPRIETARY-LICENSE-1.1.md', 'bindings/python/LICENSE', 'bindings/go/LICENSE']:
        if (root / name).read_bytes() != terms:
            raise ValueError('Inconsistent terms: ' + name)
    python = tomllib.loads((root / 'bindings/python/pyproject.toml').read_text())['project']
    if python.get('license') != expected or python['version'] != package['version']:
        raise ValueError('Python release metadata differs')
    if package.get('license-file') != 'LICENSE' or not (root / 'legal/APACHE-2.0-LEGACY.txt').is_file():
        raise ValueError('Missing license or historical Apache terms')


if __name__ == '__main__':
    try:
        validate(Path(__file__).resolve().parents[1])
    except (ValueError, KeyError, OSError) as exc:
        raise SystemExit('Public release refused: ' + str(exc))
    print('Public release authorization, license copies and version metadata passed.')
