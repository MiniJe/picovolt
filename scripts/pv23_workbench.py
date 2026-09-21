"""PV-2.3-M-001: strict lint fixes and one-time golden fixture generation."""
from pathlib import Path
import subprocess

changes = {}
def replace(path, old, new):
    value = changes.get(path, Path(path).read_text())
    if new in value:
        return
    if value.count(old) != 1:
        raise SystemExit(f'Unexpected source anchor in {path}: {old[:100]}')
    changes[path] = value.replace(old, new, 1)

replace('src/persistent.rs', "impl<'de> serde::de::Visitor<'de> for Identifier", "impl serde::de::Visitor<'_> for Identifier")
replace('src/vector.rs', 'allowed.map_or(true, |ids| ids.contains(id))', 'allowed.is_none_or(|ids| ids.contains(id))')
replace('src/db.rs', 'validate_check_shape(check, columns, 1, &mut nodes).map_err(&invalid)?;', 'validate_check_shape(check, columns, 1, &mut nodes).map_err(invalid)?;')
path = 'src/engine/query.rs'
value = Path(path).read_text()
function = value.index('fn parse_retrieval_index(')
tests = value.index('#[cfg(test)]\nmod tests')
if function > tests:
    value = value[:tests] + value[function:] + '\n\n' + value[tests:function]
    changes[path] = value
for path, content in changes.items():
    Path(path).write_text(content)
    print('PATCHED', path)

fixture = Path('tests/fixtures/format_v8.pvdb')
if not fixture.exists():
    subprocess.run(['cargo', 'run', '--locked', '--example', 'persistent_retrieval_envelope', '--', '--fixture'], check=True)
else:
    print('Keeping existing immutable format-8 golden fixture')
