//! Embedded WASM extension runtime (spec §6).
//!
//! User-defined functions run inside a sandboxed [`wasmi`] interpreter. The host
//! ↔ guest bridge follows the spec's six-step flow: the host allocates guest
//! linear memory, copies the input byte stream in, invokes an exported function
//! by name, and reads the result back out — no structural serialization across
//! the boundary, just shared linear-memory pointers.
//!
//! ## Guest ABI
//!
//! A guest module must export its linear memory as `"memory"` and one or more
//! functions of the form `fn(ptr: i32, len: i32) -> i32`. The host writes the
//! input at offset `0`; the return value is interpreted per call site (a scalar
//! result, or an output length for [`WasmModule::apply_in_place`]).

use wasmi::{Engine, Linker, Module, Store};

use crate::core::errors::{PvError, Result};

fn wasm_err<E: std::fmt::Display>(e: E) -> PvError {
    PvError::Wasm(e.to_string())
}

/// A uniform interface over WASM backends — the [`wasmi`] engine and PicoVolt's
/// own [`crate::engine::interp`] interpreter — so call sites and differential
/// tests can treat them interchangeably.
pub trait WasmExec {
    /// Write `input` to guest memory at offset 0, call `func(ptr, len) -> i32`,
    /// and return the scalar result.
    fn call_scalar(&self, func: &str, input: &[u8]) -> Result<i32>;

    /// As [`call_scalar`], but read the (in-place mutated) region back out.
    ///
    /// [`call_scalar`]: WasmExec::call_scalar
    fn apply_in_place(&self, func: &str, input: &[u8]) -> Result<Vec<u8>>;
}

impl WasmExec for WasmModule {
    fn call_scalar(&self, func: &str, input: &[u8]) -> Result<i32> {
        WasmModule::call_scalar(self, func, input)
    }
    fn apply_in_place(&self, func: &str, input: &[u8]) -> Result<Vec<u8>> {
        WasmModule::apply_in_place(self, func, input)
    }
}

/// Owns a `wasmi` [`Engine`]; compiles guest modules.
pub struct WasmRuntime {
    engine: Engine,
}

impl WasmRuntime {
    /// Create a runtime with the default engine configuration.
    pub fn new() -> Self {
        Self {
            engine: Engine::default(),
        }
    }

    /// Validate and compile a WebAssembly binary into a reusable [`WasmModule`].
    pub fn load(&self, wasm_bytes: &[u8]) -> Result<WasmModule> {
        let module = Module::new(&self.engine, wasm_bytes).map_err(wasm_err)?;
        Ok(WasmModule {
            engine: self.engine.clone(),
            module,
        })
    }
}

impl Default for WasmRuntime {
    fn default() -> Self {
        Self::new()
    }
}

/// A compiled guest module, instantiable on demand.
pub struct WasmModule {
    engine: Engine,
    module: Module,
}

impl WasmModule {
    /// Instantiate the module, write `input` to guest memory at offset `0`, call
    /// `func_name(ptr, len) -> i32`, and return the scalar result.
    ///
    /// Steps 1–6 of the spec's bridge: allocate (instantiate) → copy bytes in →
    /// invoke exported function → read the returned scalar.
    pub fn call_scalar(&self, func_name: &str, input: &[u8]) -> Result<i32> {
        let mut store = Store::new(&self.engine, ());
        let linker = <Linker<()>>::new(&self.engine);
        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(wasm_err)?
            .start(&mut store)
            .map_err(wasm_err)?;
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| PvError::Wasm("guest module exports no `memory`".into()))?;
        memory.write(&mut store, 0, input).map_err(wasm_err)?;
        let func = instance
            .get_typed_func::<(i32, i32), i32>(&store, func_name)
            .map_err(wasm_err)?;
        func.call(&mut store, (0, input.len() as i32))
            .map_err(wasm_err)
    }

    /// Like [`call_scalar`], but the guest is expected to mutate the input region
    /// in place and return the number of output bytes; those bytes are then read
    /// back from offset `0`. Demonstrates the full byte-stream round trip.
    ///
    /// [`call_scalar`]: WasmModule::call_scalar
    pub fn apply_in_place(&self, func_name: &str, input: &[u8]) -> Result<Vec<u8>> {
        let mut store = Store::new(&self.engine, ());
        let linker = <Linker<()>>::new(&self.engine);
        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(wasm_err)?
            .start(&mut store)
            .map_err(wasm_err)?;
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| PvError::Wasm("guest module exports no `memory`".into()))?;
        memory.write(&mut store, 0, input).map_err(wasm_err)?;
        let func = instance
            .get_typed_func::<(i32, i32), i32>(&store, func_name)
            .map_err(wasm_err)?;
        let out_len = func
            .call(&mut store, (0, input.len() as i32))
            .map_err(wasm_err)? as usize;
        let mut out = vec![0u8; out_len];
        memory.read(&store, 0, &mut out).map_err(wasm_err)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A guest that sums `len` bytes starting at `ptr` and returns the total.
    const SUM_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "sum_bytes") (param $ptr i32) (param $len i32) (result i32)
            (local $i i32)
            (local $acc i32)
            (block $done
              (loop $loop
                (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
                (local.set $acc
                  (i32.add (local.get $acc)
                    (i32.load8_u (i32.add (local.get $ptr) (local.get $i)))))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $loop)))
            (local.get $acc)))
    "#;

    // A guest that adds 1 to each of `len` bytes in place, returns `len`.
    const INC_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "inc") (param $ptr i32) (param $len i32) (result i32)
            (local $i i32)
            (block $done
              (loop $loop
                (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
                (i32.store8 (i32.add (local.get $ptr) (local.get $i))
                  (i32.add (i32.load8_u (i32.add (local.get $ptr) (local.get $i))) (i32.const 1)))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $loop)))
            (local.get $len)))
    "#;

    #[test]
    fn scalar_call_reads_guest_memory() {
        let rt = WasmRuntime::new();
        let module = rt.load(&wat::parse_str(SUM_WAT).unwrap()).unwrap();
        let sum = module.call_scalar("sum_bytes", &[1, 2, 3, 4, 10]).unwrap();
        assert_eq!(sum, 20);
    }

    #[test]
    fn in_place_transform_round_trips_bytes() {
        let rt = WasmRuntime::new();
        let module = rt.load(&wat::parse_str(INC_WAT).unwrap()).unwrap();
        let out = module.apply_in_place("inc", &[0, 9, 254]).unwrap();
        assert_eq!(out, vec![1, 10, 255]);
    }

    #[test]
    fn missing_export_is_reported() {
        let rt = WasmRuntime::new();
        let module = rt.load(&wat::parse_str(SUM_WAT).unwrap()).unwrap();
        assert!(matches!(
            module.call_scalar("does_not_exist", &[]),
            Err(PvError::Wasm(_))
        ));
    }
}
