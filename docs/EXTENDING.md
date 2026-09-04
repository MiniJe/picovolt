# Extending PicoVolt

PicoVolt is meant to be built on. There are two extension paths with different
trust models. Choose by who writes the extension and whether it needs to run
sandboxed.

## 1. Sandboxed WASM extensions (third-party, untrusted-safe)

For logic you did not write, or that you do not want to grant native access, ship
it as a WebAssembly module and run it through the sandbox. This is the path for
plugin ecosystems and user-supplied functions.

The guest ABI is small (full details in [`engine/wasm.rs`](../src/engine/wasm.rs)).
A module exports its linear memory as `"memory"` and one or more functions of the
form `fn(ptr: i32, len: i32) -> i32`. The host writes the input at offset `0` and
interprets the return value per call site.

```rust
use picovolt::Database;

let db = Database::open_memory();
let wasm = std::fs::read("my_udf.wasm")?;

// Scalar result: fn(ptr, len) -> i32 used directly.
let total = db.run_wasm_scalar(&wasm, "sum", &[1, 2, 3, 4])?;     // 10

// Byte-stream result: the guest mutates the region in place and returns the
// output length; the bytes are read back out.
let out = db.run_wasm_apply(&wasm, "transform", b"payload")?;     // Vec<u8>
# Ok::<(), picovolt::PvError>(())
```

Both entry points are backed by the [`WasmExec`] trait, which has two
implementations: the vetted [`wasmi`] interpreter (the default, full WASM) and
`pv-wasm` ([`engine/interp.rs`](../src/engine/interp.rs)), a from-scratch
integer-subset interpreter. A differential test keeps them in agreement. The
decoders are bounds-checked and resource-capped (see [SECURITY.md](../SECURITY.md)),
so a malformed or hostile module returns an error rather than escaping the sandbox.

[`WasmExec`]: ../src/engine/wasm.rs

## 2. Native modules (first-party, trusted)

For trusted, performance-sensitive features such as new index types, storage
transforms, or observability, write a normal Rust crate that depends on `picovolt`
and builds on its public surface:

```toml
[dependencies]
picovolt = "1"
```

The types intended for building on are re-exported at the crate root:

| Surface | Use it to |
|---------|-----------|
| [`Database`], [`QueryResult`], [`Durability`] | drive the engine, group transactions, and choose flush semantics |
| [`Value`], [`Row`] | read and construct rows |
| [`WasmExec`], [`WasmRuntime`], [`WasmModule`] | run or embed sandboxed guest code |
| [`ComplianceMonitor`], [`RuntimeMetrics`] | hook usage-policy checks |
| `enterprise` feature: [`EnterpriseConfig`], [`AuditSink`], [`EnterpriseCapabilities`] | attach a host-owned audit/control plane without sending data from the engine |
| `core::types` (page and file headers, `RecordEnvelope`) | parse or emit the on-disk format |

A downstream crate depends on PicoVolt through this public API and implements its
feature on top, without modifying the engine itself.

## Stability

PicoVolt follows Semantic Versioning. The crate-root re-exports above are the
stable 1.x extension contract; incompatible changes require a major release.
Deeper module internals remain public for flexibility but may be deprecated and
moved across minor releases. If you depend on an internal surface, open an issue
so it can be considered for the stable contract.

[`Database`]: ../src/db.rs
[`QueryResult`]: ../src/db.rs
[`Durability`]: ../src/db.rs
[`Value`]: ../src/core/value.rs
[`Row`]: ../src/core/value.rs
[`WasmRuntime`]: ../src/engine/wasm.rs
[`WasmModule`]: ../src/engine/wasm.rs
[`ComplianceMonitor`]: ../src/engine/compliance.rs
[`RuntimeMetrics`]: ../src/engine/compliance.rs
[`EnterpriseConfig`]: ../src/enterprise.rs
[`AuditSink`]: ../src/enterprise.rs
[`EnterpriseCapabilities`]: ../src/enterprise.rs
[`wasmi`]: https://docs.rs/wasmi
