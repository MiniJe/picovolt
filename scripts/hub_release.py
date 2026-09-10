"""Prepare a verified immutable dataset bundle; no network or private keys.

This compatibility tool remains Apache-2.0 like the current engine checkout.
It is not a proprietary engine, entitlement server or hosted Hub implementation.
"""
import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile


@contextmanager
def reservation(path):
    handle = path.open('x')
    try:
        with handle:
            yield
    finally:
        path.unlink()


def digest(path):
    result = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            result.update(chunk)
    return {'sha256': result.hexdigest(), 'size_bytes': path.stat().st_size}


def prepare(image, manifest, output, dataset, release, public_key, pv):
    for value in (dataset, release):
        if not re.fullmatch(r'[a-z0-9][a-z0-9._-]{0,79}', value):
            raise ValueError('dataset and release must be lowercase safe identifiers, at most 80 characters')
    if not re.fullmatch(r'[a-fA-F0-9]{64}', public_key):
        raise ValueError('an independently trusted 32-byte public key is required')
    image, manifest, output = Path(image), Path(manifest), Path(output).absolute()
    if not image.is_file() or not manifest.is_file():
        raise ValueError('image and signed manifest must be existing files')
    if manifest.stat().st_size > 65536:
        raise ValueError('signed manifest exceeds 64 KiB')
    output.parent.mkdir(parents=True, exist_ok=True)
    lock = output.with_name(output.name + '.prepare.lock')
    # Exclusive reservation coordinates simultaneous runs; never remove another
    # process's lock. Output is immutable and never overwritten, even if empty.
    with reservation(lock):
        if output.exists() or output.is_symlink():
            raise FileExistsError('release output already exists')
        with tempfile.TemporaryDirectory(prefix='.hub-stage-', dir=output.parent) as temp:
            stage = Path(temp).resolve()
            if stage.parent != output.parent.resolve():
                raise ValueError('staging directory must stay within output parent')
            bundle = stage / 'bundle'
            bundle.mkdir()
            shutil.copyfile(image, bundle / 'dataset.pvdb')
            # Bound the actual read too: the source may change after stat.
            with manifest.open('rb') as source:
                raw = source.read(65537)
            if len(raw) > 65536:
                raise ValueError('signed manifest exceeds 64 KiB')
            (bundle / 'dataset.manifest.json').write_bytes(raw)
            envelope = json.loads(raw)
            if not isinstance(envelope, dict) or not isinstance(envelope.get('manifest'), dict):
                raise ValueError('signed manifest must contain a metadata object')
            if envelope['manifest'].get('name') != f'{dataset}@{release}':
                raise ValueError('signed identity must equal dataset@release')
            subprocess.run([str(pv), 'dataset', 'verify', str(bundle / 'dataset.pvdb'),
                            str(bundle / 'dataset.manifest.json'), '--public-key', public_key],
                           check=True, capture_output=True, text=True, timeout=120)
            subprocess.run([str(pv), 'inspect', str(bundle / 'dataset.pvdb')],
                           check=True, capture_output=True, text=True, timeout=120)
            descriptor = {
                'schema_version': 1, 'dataset': dataset, 'release': release,
                'signed_identity': f'{dataset}@{release}',
                'trusted_public_key': public_key.lower(),
                'files': {name: digest(bundle / name) for name in
                          ('dataset.pvdb', 'dataset.manifest.json')},
            }
            (bundle / 'release.json').write_text(json.dumps(descriptor, indent=2) + '\n', encoding='utf-8')
            # A second check catches accidental writers ignoring the lock.
            if output.exists() or output.is_symlink():
                raise FileExistsError('release output already exists')
            os.rename(bundle, output)
            return descriptor


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('image', type=Path)
    parser.add_argument('manifest', type=Path)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--dataset', required=True)
    parser.add_argument('--release', required=True)
    parser.add_argument('--public-key', required=True)
    parser.add_argument('--pv', default='pv')
    args = parser.parse_args()
    try:
        result = prepare(**vars(args))
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as error:
        parser.exit(1, f'Release preparation failed: {error}\n')
    print(json.dumps(result, indent=2))


if __name__ == '__main__':
    main()
