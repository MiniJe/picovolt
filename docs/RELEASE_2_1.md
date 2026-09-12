# PicoVolt 2.1.0 qualification ledger

Prepared 11 September 2026. This is a private build, not an issued public offer.
The owner selected 2.1.0 as the first proprietary development line. Cargo public
publication, npm publication and the legacy public release workflows are blocked.
Previously published 2.0 and earlier artifacts retain their original terms.

## Implemented

Ranked full-text retrieval, exact vector similarity, reusable in-memory Rust
indexes, filtered snapshot retrieval, C/Python/Go/JavaScript/WASM interfaces and
the CLI are implemented. See [API examples and bounds](RETRIEVAL_2_1.md).
This does not introduce an ANN graph, persistent indexes or a file-format change.

## Qualification

- Windows: full `cargo test --locked --all-features --all-targets` suite passed;
  `cargo clippy --locked --all-features --all-targets -- -D warnings` passed.
- Seven focused full-text/retrieval Rust tests cover ranking, exact distances,
  stable ties, admission bounds, atomic rejected updates, invalid vectors,
  SELECT-only execution, tenant filtering, large IDs, baked files and the CLI.
- Nine Python retrieval/transaction tests passed against the 2.1.0 release DLL.
- Go binding tests and `go vet ./...` passed against that DLL.
- Native Windows x86-64 release and bundler WebAssembly builds succeeded.
- Six JavaScript/WASM integration tests passed against the actual WASM build;
  the Windows Python wheel bundles the verified native DLL.
- Rust documentation examples and API documentation with warnings denied passed.

Additional package qualification is recorded in the private artifact manifest.
Linux/macOS 2.1.0 native builds, production-scale latency/relevance benchmarks,
independent review and the production Hub runtime upgrade are not claimed.
Production continues to use its existing qualified 2.0.0 runtime.

## Distribution boundary

The exact licensor, covered components, terms digest, artifact digest, price
(including an explicit zero if free), acceptance and durable receipt must be
pinned in an issued offer before customer delivery. The prepared license does
not turn a silent download into acceptance. No new offer or price was invented.
See [component scope](../legal/COMPONENT-SCOPE-2.1.md) and
[transition](../legal/TRANSITION.md). No public engine source, tag, registry
package or release asset has been pushed for this development line.

Private qualification artifacts live in `artifacts/`; their SHA-256 manifest
identifies the actual bytes. Keep them outside the public website document root.
