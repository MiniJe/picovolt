"""Bundle dependency licenses and source references with binary packages."""
from pathlib import Path
import json
import shutil
import subprocess
import sys

root = Path(__file__).resolve().parents[1]
destination = Path(sys.argv[1]).resolve()
destination.mkdir(parents=True, exist_ok=True)
for name in ['LICENSE', 'NOTICE']:
    if (root / name).resolve() != destination / name:
        shutil.copyfile(root / name, destination / name)
shutil.copyfile(root / 'legal/APACHE-2.0-LEGACY.txt', destination / 'APACHE-2.0-LEGACY.txt')
shutil.copyfile(root / 'legal/PUBLIC-RELEASE.json', destination / 'PUBLIC-RELEASE.json')
metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--locked', '--format-version', '1', '--all-features'], cwd=root))
inventory = []
for package in sorted(metadata['packages'], key=lambda p: (p['name'], p['version'])):
    if package['name'] == 'picovolt':
        continue
    directory = Path(package['manifest_path']).parent
    identity = package['name'] + '-' + package['version']
    paths = set()
    for pattern in ['LICENSE*', 'COPYING*', 'NOTICE*']:
        paths.update(directory.glob(pattern))
    if package.get('license_file'):
        candidate = (directory / package['license_file']).resolve()
        if candidate.is_relative_to(directory.resolve()):
            paths.add(candidate)
    for path in sorted(paths):
        if path.is_file() and path.stat().st_size <= 1048576:
            target = destination / 'third-party-notices' / identity / path.name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, target)
    inventory.append({**{k: package.get(k) for k in ['name', 'version', 'license', 'repository']},
                      'source_archive': f'https://crates.io/api/v1/crates/{package["name"]}/{package["version"]}/download'})
(destination / 'dependency-inventory.json').write_text(json.dumps({'scope': 'Resolved dependency superset, including build/test dependencies', 'packages': inventory}, indent=2) + '\n', encoding='utf-8')
print('Bundled license notices and source references for', len(inventory), 'dependencies.')
