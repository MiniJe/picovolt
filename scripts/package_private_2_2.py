"""Package local qualification builds and dependency notices; never upload."""
from pathlib import Path
import hashlib
import json
import subprocess
import zipfile
import tomllib

root = Path(__file__).resolve().parents[1]
assert tomllib.loads((root / 'Cargo.toml').read_text())['package']['version'] == '2.2.0'
assert not subprocess.check_output(['git','status','--porcelain'],cwd=root), 'Commit source before packaging'
assert subprocess.check_output([str(root/'target/release/pv.exe'),'--version'],text=True).strip() == 'pv 2.2.0'
out = root / 'artifacts'
source_archive = out / 'picovolt-2.2.0-private-source.zip'
subprocess.run(['git', 'archive', '--format=zip', '--output=' + str(source_archive), 'HEAD'], cwd=root, check=True)
metadata = json.loads(subprocess.check_output(
    ['cargo', 'metadata', '--locked', '--format-version', '1', '--features', 'capi,data-tools,wasm'], cwd=root))
inventory = []
notices = {}
for package in metadata['packages']:
    if package['name'] == 'picovolt':
        continue
    directory = Path(package['manifest_path']).parent
    identity = package['name'] + '-' + package['version']
    inventory.append({key: package.get(key) for key in ('name', 'version', 'license', 'source')})
    candidates = set()
    for pattern in ('LICENSE*', 'COPYING*', 'NOTICE*'):
        candidates.update(directory.glob(pattern))
    if package.get('license_file'):
        candidate = (directory / package['license_file']).resolve()
        if candidate.is_relative_to(directory.resolve()):
            candidates.add(candidate)
    for path in sorted(candidates):
        if path.is_file() and path.stat().st_size <= 1048576:
            notices['third-party-notices/' + identity + '/' + path.name] = path.read_bytes()
inventory.sort(key=lambda p: (p['name'], p['version']))
inventory_bytes = json.dumps({'scope': 'Resolved dependency superset, including build and test dependencies; not a legal clearance',
                              'packages': inventory}, indent=2).encode()
(out / 'picovolt-2.2.0-dependencies.json').write_bytes(inventory_bytes)
archive = out / 'picovolt-2.2.0-private-windows-x86_64.zip'
files = {
    'pv.exe': 'target/release/pv.exe',
    'picovolt.dll': 'target/release/picovolt.dll',
    'libpicovolt.dll.a': 'target/release/libpicovolt.dll.a',
    'include/picovolt.h': 'include/picovolt.h',
    **{name: name for name in ('LICENSE', 'NOTICE', 'legal/APACHE-2.0-LEGACY.txt',
       'legal/COMPONENT-SCOPE-2.2.md', 'legal/COMPONENT-SCOPE-2.1.md', 'legal/TRANSITION.md',
       'docs/RETRIEVAL_2_1.md', 'docs/RELEASE_2_2.md', 'docs/ENCRYPTION_2_2.md', 'docs/HYBRID_2_2.md')},
}
with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as output:
    for name, source in files.items():
        output.write(root / source, name)
    output.writestr('dependency-inventory.json', inventory_bytes)
    for name, data in notices.items():
        output.writestr(name, data)
    output.writestr('README.txt', 'PicoVolt 2.2.0 private qualification build.\nRead docs/RELEASE_2_2.md and LICENSE before any delivery.\nRun pv.exe --version; see docs/RETRIEVAL_2_1.md for retrieval.\n')
linux_archive=out/'picovolt-2.2.0-private-linux-x86_64.zip'
with zipfile.ZipFile(linux_archive,'w',zipfile.ZIP_DEFLATED) as output:
    for name in ('pv','libpicovolt.so'):
        info=zipfile.ZipInfo(name);info.create_system=3;info.external_attr=(0o100755<<16);info.compress_type=zipfile.ZIP_DEFLATED
        output.writestr(info,(out/'linux-2.2.0'/name).read_bytes())
    for name,source in files.items():
        if not source.startswith('target/'):
            output.write(root/source,name)
    output.writestr('dependency-inventory.json',inventory_bytes)
    for name,data in notices.items():output.writestr(name,data)
    output.writestr('README.txt','Native Linux x86-64 with capi and default encryption/retrieval features. Optional data-tools are not included. Read docs/ENCRYPTION_2_2.md. Private qualification only.\n')
artifacts = [archive, linux_archive, source_archive, out / 'picovolt-2.2.0.tgz',
             out / 'python-2.2.0/picovolt-2.2.0-py3-none-win_amd64.whl',
             out / 'picovolt-2.2.0-dependencies.json']
manifest = {'version': '2.2.0', 'availability': 'private qualification; not a public offer',
            'platforms': {'windows': 'x86_64-pc-windows-gnu; capi,data-tools and default features',
                          'linux': 'x86_64-unknown-linux-gnu; Ubuntu 24.04 WSL; capi and default features',
                          'wasm': 'wasm32-unknown-unknown; retrieval, no vault encryption',
                          'unqualified': ['macOS', 'ARM', 'other Linux distributions']},
            'source_commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip(),
            'source_dirty': bool(subprocess.check_output(['git', 'status', '--porcelain'], cwd=root)),
            'qualification': ['Rust all-feature/all-target tests and clippy passed',
                              'Python: 11 tests passed against release DLL',
                              'Go: tests and vet passed against release DLL',
                              'JavaScript/WASM: 6 integration tests passed (including hybrid)',
                              'Linux encryption/retrieval and process-crash tests passed',
                              'PyNaCl/libsodium interoperability passed in both directions',
                              'cargo audit: no reported vulnerabilities'],
            'artifacts': [{'file': p.relative_to(out).as_posix(), 'bytes': p.stat().st_size,
                           'sha256': hashlib.sha256(p.read_bytes()).hexdigest()} for p in artifacts]}
(out / 'picovolt-2.2.0-manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
print(json.dumps(manifest, indent=2))
