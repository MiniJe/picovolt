# Extending PicoVolt

PicoVolt is meant to be built on. There are two distinct extension paths, with
different trust models — pick by who writes the extension and whether it runs
sandboxed.

## 1. Sandboxed WASM extensions (third-party, untrusted-safe)

For logic you didn't write — or that you don't want to grant native access — ship
it as a WebAssembly module and run it through the sandbox. This is the path for
plugin ecosystems and user-supplied functions.

The guest ABI is small (full details in [`engine/wasm.rs`](../src/engine/wasm.rs)):
a module exports its linear memory as `"memory"` and one or more functions of the
form `fn(ptr: i32, len: i32) -> i32`. The host writes the input at offset `0` and
interprets the return value per call site.

```rust
use picovolt::Database;

let db = Database::open_memory();
let wasm = std::fs::read("my_udf.wasm")?;

// Scalar result: fn(ptr, len) -> i32 used directly.
let total = db.run_wasm_scalar(&wasm, "sum", &[1, 2, 3, 4])?;     // -> 10

// Byte-stream result: the guest mutates the region in place and returns the
// output length; the bytes are read back out.
let out = db.run_wasm_apply(&wasm, "transform", b"payload")?;     // -> Vec<u8>
# Ok::<(), picovolt::PvError>(())
```

Both entry points are backed by the [`WasmExec`] trait, which has two
implementations behind it: the vetted [`wasmi`] interpreter (default, full WASM)
and `pv-wasm` ([`engine/interp.rs`](../src/engine/interp.rs)), a from-scratch
integer-subset interpreter. A differential test keeps them in agreement. The
decoders are bounds-checked and resource-capped (see [SECURITY.md](../SECURITY.md)),
so a malformed or hostile module errors rather than escaping the sandbox.

[`WasmExec`]: ../src/engine/wasm.rs

## 2. Native modules (first-party / trusted)

For trusted, performance-sensitive features — new index types, storage
transforms, observability — write a normal Rust crate that depends on `picovolt`
and builds on its public surface:

```toml
[dependencies]
picovolt = "0.1"
```

The types intended to build on are re-exported at the crate root:

| Surface | Use it to |
|---------|-----------|
| [`Database`], [`QueryResult`], [`Durability`] | drive the engine; choose flush semantics |
| [`Value`], [`Row`] | read and construct rows |
| [`WasmExec`], [`WasmRuntime`], [`WasmModule`] | run or embed sandboxed guest code |
| [`ComplianceMonitor`], [`RuntimeMetrics`] | hook usage-policy checks |
| `core::types` (page/file headers, `RecordEnvelope`) | parse or emit the on-disk format |

This is the seam the planned commercial **`picovolt-pro`** edition plugs into: it
is a *separate* crate that depends on this open-source core through the public
API above. The open core never contains proprietary code, and nothing is ever
removed from it to make room for the paid edition — see [ROADMAP.md](../ROADMAP.md).

## Stability

PicoVolt is pre-1.0, so the public API can still change between minor versions
(see [RELEASING.md](../RELEASING.md) for the versioning rules). The crate-root
re-exports above are the surface we treat as the extension contract and try
hardest to keep stable; the deeper module internals are public for flexibility
but may move. If you're building on something that isn't re-exported at the root
and want it stabilized, open an issue — that's exactly the feedback that shapes
the 1.0 surface.

[`Database`]: ../src/db.rs
[`QueryResult`]: ../src/db.rs
[`Durability`]: ../src/db.rs
[`Value`]: ../src/core/value.rs
[`Row`]: ../src/core/value.rs
[`WasmRuntime`]: ../src/engine/wasm.rs
[`WasmModule`]: ../src/engine/wasm.rs
[`ComplianceMonitor`]: ../src/engine/compliance.rs
[`RuntimeMetrics`]: ../src/engine/compliance.rs
[`wasmi`]: https://docs.rs/wasmi
