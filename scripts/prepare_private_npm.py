"""Complete a wasm-pack directory for private 2.1 qualification (never publish)."""
from pathlib import Path
import json
import shutil
import sys

root = Path(__file__).resolve().parents[1]
directory = root / (sys.argv[1] if len(sys.argv) > 1 else 'artifacts/npm-2.1.0')
package = json.loads((directory / 'package.json').read_text())
package.update(private=True, license='LicenseRef-PicoVolt-Proprietary-1.0')
package['exports'] = {'.': './' + package.get('module', 'picovolt.js'),
                      **{'./' + name: './' + name + '.js' for name in ('sqlite', 'browser', 'worker')}}
files = ['sqlite.js', 'browser.js', 'worker.js', 'LICENSE', 'NOTICE', 'APACHE-2.0-LEGACY.txt']
for name in files[:3]:
    shutil.copyfile(root / 'bindings/js' / name, directory / name)
for name in files[3:5]:
    shutil.copyfile(root / name, directory / name)
shutil.copyfile(root / 'legal/APACHE-2.0-LEGACY.txt', directory / files[-1])
package['files'] = sorted(set(package.get('files', []) + files))
(directory / 'package.json').write_text(json.dumps(package, indent=2) + '\n')
print('Prepared private npm package; publication disabled.')
