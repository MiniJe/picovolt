# Support matrix

This page states what PicoVolt's automated release and compatibility checks
cover. A check mark means CI builds and tests that combination on every release;
it is not a promise that every downstream environment is identical.

| Distribution | Supported runtime / platform | Release artifact |
|---|---|---|
| Rust crate and `pv` CLI | Rust 1.86+, Linux x86-64, macOS arm64, Windows x86-64 | crates.io plus native GitHub downloads |
| JavaScript | Node.js 20+, modern evergreen browsers with WebAssembly | npm `picovolt` |
| Python | CPython 3.9+, Linux x86-64 (manylinux), macOS, Windows | platform wheels on PyPI |
| Go | Current two stable Go releases with cgo and a locally built PicoVolt C ABI | source binding |
| C ABI | Platforms supported by the Rust toolchain | header plus locally built shared library |

Browser persistence uses OPFS and therefore needs a secure `http://localhost` or
`https://` origin. Opening the starter through `file://` is unsupported because
browsers isolate local-file origins; run `npm install && npm run dev` in
`starters/browser` instead.

## File compatibility

- PicoVolt 1.x readers continue to read older 1.x database images unless a
  release note explicitly documents an exceptional migration.
- Current constrained images use format version 3. Older format versions remain
  covered by checked-in golden fixtures.
- Newer-format images fail with a clear unsupported-version error in older
  readers; they are never silently interpreted as an older layout.

## Known limits

- The engine has one writer. The HTTP server serializes queries through that
  writer rather than providing multi-writer transactions.
- Explicit `BEGIN`/`COMMIT`/`ROLLBACK` works for in-memory and filesystem
  databases across the maintained bindings. Filesystem transactions currently
  create a complete rollback image, so their start cost scales with workspace
  size; an incremental commit log remains a 2.0 design goal.
- A live filesystem transaction holds an OS-level transaction lock. Another
  opener fails instead of treating the live marker as crash residue.
- SQL intentionally covers a practical embedded subset, not full SQL-92.
