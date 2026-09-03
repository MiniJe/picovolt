# Integration gallery

| Surface | Entry point | Best for |
|---|---|---|
| Rust | `picovolt::Database` | Embedded applications and data pipelines |
| Command line | `pv` | Import/export, inspection, scripting, and baking |
| JavaScript | `picovolt`, `picovolt/sqlite` | Node and bundler applications |
| Browser | `picovolt/browser`, `picovolt/worker` | OPFS-backed local-first apps |
| Python | `picovolt`, `picovolt.dbapi2` | Data scripts and DB-API consumers |
| Go | `picovolt` and `database/sql` driver | Native services and tools |
| HTTP | `picovolt-server` | Language-neutral read/query services |

Starter applications live in [`starters/`](../starters/README.md). New adapters
should include a runnable smoke test, use parameter binding, and document which
part of PicoVolt's intentionally compact SQL subset they expose.
