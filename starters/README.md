# PicoVolt starters

Each directory is a minimal, copyable first project:

- `rust-cli` — embedded Rust database and prepared insert
- `python` — Python DB-API 2.0 usage
- `go` — Go `database/sql` usage
- `browser` — Vite, WebAssembly, and durable OPFS storage
- `node` — Node.js and the synchronous SQLite-style npm adapter

Every dependency manifest is pinned to the matching PicoVolt release. The
release gate copies these projects outside the checkout, installs only public
registry artifacts, and runs each starter before a release is considered
complete. See [`../docs/RELEASING.md`](../docs/RELEASING.md) for the local
command and the Go native-library details.


These starters pin 2.0.0 and exercise public registry installations. They become
installable when the corresponding release packages are published. See the
[release ledger](../docs/RELEASE_2_0.md) for publication status. Tagged release
checks require exact version pins and genuine registry checksums.
The 2.x Go module uses `github.com/MiniJe/picovolt/bindings/go/v2`.
