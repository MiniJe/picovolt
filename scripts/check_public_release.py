"""Fail closed before any public registry/release publication of private code."""
from pathlib import Path
import tomllib
root=Path(__file__).resolve().parents[1]
package=tomllib.loads((root/'Cargo.toml').read_text())['package']
if package.get('publish') is False or package.get('license')!='Apache-2.0':
    raise SystemExit('This engine line is proprietary/private. Public registry and GitHub artifact publication are disabled. Use reviewed private delivery.')
print('Public release licensing gate passed.')
