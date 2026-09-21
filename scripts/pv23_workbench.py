"""Temporary hosted workbench: initial implementation is already committed."""
from pathlib import Path

required = ('src/persistent.rs', 'src/db/persistent.rs', 'tests/persistent_retrieval.rs')
for path in required:
    if not Path(path).is_file():
        raise SystemExit(f'Missing implemented mandate source: {path}')
print('PV-2.3-M-001: validate committed implementation and behavior tests')
