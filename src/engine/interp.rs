//! `pv-wasm`: a small, from-scratch WebAssembly interpreter.
//!
//! This is the spec §6 runtime done "the hard way" — a hand-written binary
//! decoder plus a structured-control stack machine, rather than reusing
//! [`wasmi`]. It deliberately targets the **integer subset** of WASM MVP: `i32`
//! and `i64` numeric ops, locals, linear-memory load/stores, the full structured
//! control set (`block`/`loop`/`if`/`else`/`br`/`br_if`/`return`/`call`), and
//! `drop`/`select`. Floats, globals, tables, imports, SIMD, and `br_table` are
//! out of scope and rejected with a clear [`PvError::Wasm`] rather than
//! mis-executed.
//!
//! It exposes the same [`WasmExec`] surface as the `wasmi` backend, so the two
//! are interchangeable — and the test module differential-checks `pv-wasm`
//! against `wasmi` to keep it honest.
//!
//! [`wasmi`]: https://docs.rs/wasmi
//! [`WasmExec`]: crate::engine::wasm::WasmExec

use std::collections::HashMap;

use crate::core::errors::{PvError, Result};
use crate::engine::wasm::WasmExec;

const PAGE_BYTES: usize = 65_536;
const MAX_CALL_DEPTH: usize = 256;

fn trap(msg: impl Into<String>) -> PvError {
    PvError::Wasm(msg.into())
}

// ---------------------------------------------------------------------------
// Values & types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValType {
    I32,
    I64,
}

/// A runtime value on the operand stack or in a local.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Val {
    /// 32-bit integer.
    I32(i32),
    /// 64-bit integer.
    I64(i64),
}

impl Val {
    fn zero(ty: ValType) -> Self {
        match ty {
            ValType::I32 => Val::I32(0),
            ValType::I64 => Val::I64(0),
        }
    }
}

struct FuncType {
    params: Vec<ValType>,
    results: Vec<ValType>,
}

struct FuncDef {
    type_idx: u32,
    locals: Vec<ValType>,
    code: Vec<Instr>,
}

struct ExportEntry {
    name: String,
    kind: u8,
    index: u32,
}

// ---------------------------------------------------------------------------
// Instructions (block targets resolved at decode time)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum LoadOp {
    I32,
    I64,
    I32_8U,
    I32_8S,
    I32_16U,
    I32_16S,
}

#[derive(Debug, Clone, Copy)]
enum StoreOp {
    I32,
    I64,
    I32_8,
    I32_16,
}

#[derive(Debug, Clone, Copy)]
enum NumOp {
    // i32 comparisons
    I32Eqz,
    I32Eq,
    I32Ne,
    I32LtS,
    I32LtU,
    I32GtS,
    I32GtU,
    I32LeS,
    I32LeU,
    I32GeS,
    I32GeU,
    // i32 arithmetic / bitwise
    I32Add,
    I32Sub,
    I32Mul,
    I32DivS,
    I32DivU,
    I32RemS,
    I32RemU,
    I32And,
    I32Or,
    I32Xor,
    I32Shl,
    I32ShrS,
    I32ShrU,
    I32Rotl,
    I32Rotr,
    I32Clz,
    I32Ctz,
    I32Popcnt,
    // i64 comparisons
    I64Eqz,
    I64Eq,
    I64Ne,
    I64LtS,
    I64LtU,
    I64GtS,
    I64GtU,
    I64LeS,
    I64LeU,
    I64GeS,
    I64GeU,
    // i64 arithmetic / bitwise
    I64Add,
    I64Sub,
    I64Mul,
    I64DivS,
    I64DivU,
    I64RemS,
    I64RemU,
    I64And,
    I64Or,
    I64Xor,
    I64Shl,
    I64ShrS,
    I64ShrU,
    // conversions
    I32WrapI64,
    I64ExtendI32S,
    I64ExtendI32U,
}

#[derive(Debug, Clone)]
enum Instr {
    Unreachable,
    Nop,
    Block {
        end: usize,
        arity: usize,
    },
    Loop,
    If {
        else_: Option<usize>,
        end: usize,
        arity: usize,
    },
    Else {
        end: usize,
    },
    End,
    Br(u32),
    BrIf(u32),
    Return,
    Call(u32),
    Drop,
    Select,
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    I32Const(i32),
    I64Const(i64),
    Load {
        op: LoadOp,
        offset: u32,
    },
    Store {
        op: StoreOp,
        offset: u32,
    },
    Num(NumOp),
}

// ---------------------------------------------------------------------------
// Binary reader (LEB128)
// ---------------------------------------------------------------------------

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn eof(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn byte(&mut self) -> Result<u8> {
        let b = *self
            .bytes
            .get(self.pos)
            .ok_or_else(|| trap("unexpected end of module"))?;
        self.pos += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let slice = self
            .bytes
            .get(self.pos..self.pos + n)
            .ok_or_else(|| trap("unexpected end of section"))?;
        self.pos += n;
        Ok(slice)
    }

    fn uleb(&mut self) -> Result<u64> {
        let mut result: u64 = 0;
        let mut shift = 0u32;
        loop {
            let b = self.byte()?;
            if shift >= 64 {
                return Err(trap("LEB128 overflow"));
            }
            result |= u64::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        Ok(result)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(self.uleb()? as u32)
    }

    fn sleb(&mut self) -> Result<i64> {
        let mut result: i64 = 0;
        let mut shift = 0u32;
        loop {
            let b = self.byte()?;
            result |= i64::from(b & 0x7F) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                if shift < 64 && (b & 0x40) != 0 {
                    result |= -1i64 << shift;
                }
                break;
            }
            if shift >= 64 {
                return Err(trap("signed LEB128 overflow"));
            }
        }
        Ok(result)
    }

    fn name(&mut self) -> Result<String> {
        let n = self.uleb()? as usize;
        let bytes = self.take(n)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| trap("invalid utf-8 in name"))
    }
}

fn valtype(r: &mut Reader) -> Result<ValType> {
    match r.byte()? {
        0x7F => Ok(ValType::I32),
        0x7E => Ok(ValType::I64),
        0x7D | 0x7C => Err(trap("floating-point types are not supported")),
        other => Err(trap(format!("unknown value type 0x{other:02X}"))),
    }
}

// ---------------------------------------------------------------------------
// Module decoding
// ---------------------------------------------------------------------------

/// A decoded, ready-to-run WASM module.
pub struct PvModule {
    types: Vec<FuncType>,
    funcs: Vec<FuncDef>,
    exports: Vec<ExportEntry>,
    mem_pages: u32,
    export_by_name: HashMap<String, u32>,
}

/// The `pv-wasm` engine. Compiles modules; holds no per-module state.
pub struct Interpreter;

impl Interpreter {
    /// Create an interpreter.
    pub fn new() -> Self {
        Interpreter
    }

    /// Decode and validate (structurally) a WASM binary.
    pub fn load(&self, bytes: &[u8]) -> Result<PvModule> {
        decode_module(bytes)
    }
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

fn decode_module(bytes: &[u8]) -> Result<PvModule> {
    let mut r = Reader::new(bytes);
    if r.take(4)? != b"\0asm" {
        return Err(trap("bad magic: not a WASM module"));
    }
    if r.take(4)? != [1, 0, 0, 0] {
        return Err(trap("unsupported WASM version"));
    }

    let mut types: Vec<FuncType> = Vec::new();
    let mut func_type_idx: Vec<u32> = Vec::new();
    let mut mem_pages = 0u32;
    let mut exports: Vec<ExportEntry> = Vec::new();
    let mut codes: Vec<(Vec<ValType>, Vec<Instr>)> = Vec::new();

    while !r.eof() {
        let id = r.byte()?;
        let size = r.uleb()? as usize;
        let content = r.take(size)?;
        let mut s = Reader::new(content);
        match id {
            1 => decode_type_section(&mut s, &mut types)?,
            2 => {
                let import_count = s.uleb()?;
                if import_count > 0 {
                    return Err(trap("imports are not supported"));
                }
            }
            3 => {
                let n = s.uleb()?;
                for _ in 0..n {
                    func_type_idx.push(s.u32()?);
                }
            }
            5 => mem_pages = decode_memory_section(&mut s)?,
            7 => decode_export_section(&mut s, &mut exports)?,
            10 => decode_code_section(&mut s, &mut codes)?,
            // Ignored sections: custom(0), table(4), global(6), start(8),
            // element(9), data(11), datacount(12). They are not needed by the
            // integer subset; their presence is harmless.
            _ => {}
        }
    }

    if func_type_idx.len() != codes.len() {
        return Err(trap("function/code section length mismatch"));
    }
    let funcs = func_type_idx
        .into_iter()
        .zip(codes)
        .map(|(type_idx, (locals, code))| FuncDef {
            type_idx,
            locals,
            code,
        })
        .collect();

    let export_by_name = exports
        .iter()
        .filter(|e| e.kind == 0x00)
        .map(|e| (e.name.clone(), e.index))
        .collect();

    Ok(PvModule {
        types,
        funcs,
        exports,
        mem_pages,
        export_by_name,
    })
}

fn decode_type_section(s: &mut Reader, types: &mut Vec<FuncType>) -> Result<()> {
    let n = s.uleb()?;
    for _ in 0..n {
        if s.byte()? != 0x60 {
            return Err(trap("expected function type (0x60)"));
        }
        let np = s.uleb()?;
        let params = (0..np).map(|_| valtype(s)).collect::<Result<Vec<_>>>()?;
        let nr = s.uleb()?;
        let results = (0..nr).map(|_| valtype(s)).collect::<Result<Vec<_>>>()?;
        types.push(FuncType { params, results });
    }
    Ok(())
}

fn decode_memory_section(s: &mut Reader) -> Result<u32> {
    let n = s.uleb()?;
    let mut pages = 0u32;
    for i in 0..n {
        let flags = s.byte()?;
        let min = s.u32()?;
        if flags & 0x01 != 0 {
            let _max = s.u32()?;
        }
        if i == 0 {
            pages = min; // only the first memory is used by the integer subset
        }
    }
    Ok(pages)
}

fn decode_export_section(s: &mut Reader, exports: &mut Vec<ExportEntry>) -> Result<()> {
    let n = s.uleb()?;
    for _ in 0..n {
        let name = s.name()?;
        let kind = s.byte()?;
        let index = s.u32()?;
        exports.push(ExportEntry { name, kind, index });
    }
    Ok(())
}

fn decode_code_section(s: &mut Reader, codes: &mut Vec<(Vec<ValType>, Vec<Instr>)>) -> Result<()> {
    let n = s.uleb()?;
    for _ in 0..n {
        let body_size = s.uleb()? as usize;
        let body = s.take(body_size)?;
        let mut br = Reader::new(body);
        let locals = decode_locals(&mut br)?;
        let code = decode_body(&mut br)?;
        codes.push((locals, code));
    }
    Ok(())
}

fn decode_locals(r: &mut Reader) -> Result<Vec<ValType>> {
    let groups = r.uleb()?;
    let mut locals = Vec::new();
    for _ in 0..groups {
        let count = r.uleb()?;
        let ty = valtype(r)?;
        for _ in 0..count {
            locals.push(ty);
        }
    }
    Ok(locals)
}

/// Decode a function body into instructions with structured-control targets
/// (block/if `end`, `if`/`else` positions) resolved in a single pass.
fn decode_body(r: &mut Reader) -> Result<Vec<Instr>> {
    let mut instrs: Vec<Instr> = Vec::new();
    // Stack of (opener_index, else_index) for open control structures.
    let mut ctrl: Vec<(usize, Option<usize>)> = Vec::new();

    while !r.eof() {
        let op = r.byte()?;
        match op {
            0x00 => instrs.push(Instr::Unreachable),
            0x01 => instrs.push(Instr::Nop),
            0x02 => {
                let arity = blocktype(r)?;
                ctrl.push((instrs.len(), None));
                instrs.push(Instr::Block { end: 0, arity });
            }
            0x03 => {
                let _arity = blocktype(r)?;
                ctrl.push((instrs.len(), None));
                instrs.push(Instr::Loop);
            }
            0x04 => {
                let arity = blocktype(r)?;
                ctrl.push((instrs.len(), None));
                instrs.push(Instr::If {
                    else_: None,
                    end: 0,
                    arity,
                });
            }
            0x05 => {
                let idx = instrs.len();
                instrs.push(Instr::Else { end: 0 });
                ctrl.last_mut()
                    .ok_or_else(|| trap("`else` without `if`"))?
                    .1 = Some(idx);
            }
            0x0B => {
                let end = instrs.len();
                instrs.push(Instr::End);
                if let Some((opener, else_idx)) = ctrl.pop() {
                    patch_block_end(&mut instrs, opener, else_idx, end);
                }
            }
            0x0C => instrs.push(Instr::Br(r.u32()?)),
            0x0D => instrs.push(Instr::BrIf(r.u32()?)),
            0x0F => instrs.push(Instr::Return),
            0x10 => instrs.push(Instr::Call(r.u32()?)),
            0x1A => instrs.push(Instr::Drop),
            0x1B => instrs.push(Instr::Select),
            0x20 => instrs.push(Instr::LocalGet(r.u32()?)),
            0x21 => instrs.push(Instr::LocalSet(r.u32()?)),
            0x22 => instrs.push(Instr::LocalTee(r.u32()?)),
            0x28..=0x35 => instrs.push(decode_load(op, r)?),
            0x36..=0x3E => instrs.push(decode_store(op, r)?),
            0x41 => instrs.push(Instr::I32Const(r.sleb()? as i32)),
            0x42 => instrs.push(Instr::I64Const(r.sleb()?)),
            _ => instrs.push(Instr::Num(decode_num(op)?)),
        }
    }
    if !ctrl.is_empty() {
        return Err(trap("unbalanced control structure"));
    }
    Ok(instrs)
}

fn patch_block_end(instrs: &mut [Instr], opener: usize, else_idx: Option<usize>, end: usize) {
    match &mut instrs[opener] {
        Instr::Block { end: e, .. } => *e = end,
        Instr::If {
            else_: el, end: e, ..
        } => {
            *e = end;
            *el = else_idx;
        }
        // `Loop` needs no end (its branch target is its own start).
        _ => {}
    }
    if let Some(e) = else_idx {
        if let Instr::Else { end: ee } = &mut instrs[e] {
            *ee = end;
        }
    }
}

/// Decode a block type, returning the result arity (0 or 1). Only empty (`0x40`)
/// and single integer result types are supported.
fn blocktype(r: &mut Reader) -> Result<usize> {
    match r.byte()? {
        0x40 => Ok(0),
        0x7F | 0x7E => Ok(1),
        0x7D | 0x7C => Err(trap("floating-point block result not supported")),
        _ => Err(trap("function-typed blocks are not supported")),
    }
}

fn decode_load(op: u8, r: &mut Reader) -> Result<Instr> {
    let load = match op {
        0x28 => LoadOp::I32,
        0x29 => LoadOp::I64,
        0x2C => LoadOp::I32_8S,
        0x2D => LoadOp::I32_8U,
        0x2E => LoadOp::I32_16S,
        0x2F => LoadOp::I32_16U,
        other => return Err(trap(format!("unsupported load op 0x{other:02X}"))),
    };
    let _align = r.u32()?;
    let offset = r.u32()?;
    Ok(Instr::Load { op: load, offset })
}

fn decode_store(op: u8, r: &mut Reader) -> Result<Instr> {
    let store = match op {
        0x36 => StoreOp::I32,
        0x37 => StoreOp::I64,
        0x3A => StoreOp::I32_8,
        0x3B => StoreOp::I32_16,
        other => return Err(trap(format!("unsupported store op 0x{other:02X}"))),
    };
    let _align = r.u32()?;
    let offset = r.u32()?;
    Ok(Instr::Store { op: store, offset })
}

fn decode_num(op: u8) -> Result<NumOp> {
    use NumOp::*;
    Ok(match op {
        0x45 => I32Eqz,
        0x46 => I32Eq,
        0x47 => I32Ne,
        0x48 => I32LtS,
        0x49 => I32LtU,
        0x4A => I32GtS,
        0x4B => I32GtU,
        0x4C => I32LeS,
        0x4D => I32LeU,
        0x4E => I32GeS,
        0x4F => I32GeU,
        0x67 => I32Clz,
        0x68 => I32Ctz,
        0x69 => I32Popcnt,
        0x6A => I32Add,
        0x6B => I32Sub,
        0x6C => I32Mul,
        0x6D => I32DivS,
        0x6E => I32DivU,
        0x6F => I32RemS,
        0x70 => I32RemU,
        0x71 => I32And,
        0x72 => I32Or,
        0x73 => I32Xor,
        0x74 => I32Shl,
        0x75 => I32ShrS,
        0x76 => I32ShrU,
        0x77 => I32Rotl,
        0x78 => I32Rotr,
        0x50 => I64Eqz,
        0x51 => I64Eq,
        0x52 => I64Ne,
        0x53 => I64LtS,
        0x54 => I64LtU,
        0x55 => I64GtS,
        0x56 => I64GtU,
        0x57 => I64LeS,
        0x58 => I64LeU,
        0x59 => I64GeS,
        0x5A => I64GeU,
        0x7C => I64Add,
        0x7D => I64Sub,
        0x7E => I64Mul,
        0x7F => I64DivS,
        0x80 => I64DivU,
        0x81 => I64RemS,
        0x82 => I64RemU,
        0x83 => I64And,
        0x84 => I64Or,
        0x85 => I64Xor,
        0x86 => I64Shl,
        0x87 => I64ShrS,
        0x88 => I64ShrU,
        0xA7 => I32WrapI64,
        0xAC => I64ExtendI32S,
        0xAD => I64ExtendI32U,
        other => return Err(trap(format!("unsupported opcode 0x{other:02X}"))),
    })
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct CtrlFrame {
    target: usize,
    arity: usize,
    height: usize,
}

/// A fresh instance: the module's code plus a private linear memory.
struct Instance<'a> {
    module: &'a PvModule,
    mem: Vec<u8>,
}

impl PvModule {
    /// Index of an exported function by name.
    fn func_index(&self, name: &str) -> Result<usize> {
        self.export_by_name
            .get(name)
            .map(|&i| i as usize)
            .ok_or_else(|| trap(format!("no exported function `{name}`")))
    }

    fn instantiate(&self) -> Instance<'_> {
        Instance {
            module: self,
            mem: vec![0u8; self.mem_pages as usize * PAGE_BYTES],
        }
    }

    /// Invoke an exported function with explicit integer arguments (test/inspection
    /// entry point that bypasses the `(ptr, len)` host ABI).
    pub fn invoke_i32(&self, func: &str, args: &[i32]) -> Result<Vec<i32>> {
        let idx = self.func_index(func)?;
        let mut inst = self.instantiate();
        let vals: Vec<Val> = args.iter().map(|&a| Val::I32(a)).collect();
        let out = inst.call_func(idx, &vals, 0)?;
        out.iter()
            .map(|v| match v {
                Val::I32(i) => Ok(*i),
                Val::I64(i) => Ok(*i as i32),
            })
            .collect()
    }

    /// Number of exported items (for introspection / tests).
    pub fn export_count(&self) -> usize {
        self.exports.len()
    }
}

impl WasmExec for PvModule {
    fn call_scalar(&self, func: &str, input: &[u8]) -> Result<i32> {
        let idx = self.func_index(func)?;
        let mut inst = self.instantiate();
        inst.write_mem(0, input)?;
        let out = inst.call_func(idx, &[Val::I32(0), Val::I32(input.len() as i32)], 0)?;
        match out.first() {
            Some(Val::I32(v)) => Ok(*v),
            _ => Err(trap("expected an i32 result")),
        }
    }

    fn apply_in_place(&self, func: &str, input: &[u8]) -> Result<Vec<u8>> {
        let idx = self.func_index(func)?;
        let mut inst = self.instantiate();
        inst.write_mem(0, input)?;
        let out = inst.call_func(idx, &[Val::I32(0), Val::I32(input.len() as i32)], 0)?;
        let out_len = match out.first() {
            Some(Val::I32(v)) => *v as usize,
            _ => return Err(trap("expected an i32 length result")),
        };
        let slice = inst
            .mem
            .get(0..out_len)
            .ok_or_else(|| trap("result length exceeds linear memory"))?;
        Ok(slice.to_vec())
    }
}

impl Instance<'_> {
    fn write_mem(&mut self, offset: usize, data: &[u8]) -> Result<()> {
        let end = offset
            .checked_add(data.len())
            .ok_or_else(|| trap("memory write overflow"))?;
        let dst = self
            .mem
            .get_mut(offset..end)
            .ok_or_else(|| trap("memory write out of bounds"))?;
        dst.copy_from_slice(data);
        Ok(())
    }

    fn call_func(&mut self, func_idx: usize, args: &[Val], depth: usize) -> Result<Vec<Val>> {
        if depth >= MAX_CALL_DEPTH {
            return Err(trap("call stack exhausted"));
        }
        let module = self.module; // `&PvModule` is Copy; independent of `self.mem`
        let func = module
            .funcs
            .get(func_idx)
            .ok_or_else(|| trap("call to undefined function"))?;
        let ftype = module
            .types
            .get(func.type_idx as usize)
            .ok_or_else(|| trap("function references unknown type"))?;

        let mut locals: Vec<Val> = Vec::with_capacity(ftype.params.len() + func.locals.len());
        locals.extend_from_slice(args);
        for &ty in &func.locals {
            locals.push(Val::zero(ty));
        }

        let mut stack: Vec<Val> = Vec::new();
        let mut ctrl: Vec<CtrlFrame> = Vec::new();
        let last = func.code.len().saturating_sub(1);
        ctrl.push(CtrlFrame {
            target: last,
            arity: ftype.results.len(),
            height: 0,
        });

        let mut pc = 0usize;
        loop {
            let instr = func
                .code
                .get(pc)
                .ok_or_else(|| trap("program counter out of range"))?;
            match instr {
                Instr::Unreachable => return Err(trap("unreachable executed")),
                Instr::Nop => {}
                Instr::Block { end, arity } => ctrl.push(CtrlFrame {
                    target: *end,
                    arity: *arity,
                    height: stack.len(),
                }),
                Instr::Loop => ctrl.push(CtrlFrame {
                    target: pc + 1,
                    arity: 0,
                    height: stack.len(),
                }),
                Instr::If { else_, end, arity } => {
                    let cond = pop_i32(&mut stack)?;
                    ctrl.push(CtrlFrame {
                        target: *end,
                        arity: *arity,
                        height: stack.len(),
                    });
                    if cond == 0 {
                        pc = else_.map(|e| e + 1).unwrap_or(*end);
                        continue;
                    }
                }
                Instr::Else { end } => {
                    pc = *end;
                    continue;
                }
                Instr::End => {
                    if ctrl.len() == 1 {
                        let arity = ctrl[0].arity;
                        let at = stack
                            .len()
                            .checked_sub(arity)
                            .ok_or_else(|| trap("missing function results"))?;
                        return Ok(stack.split_off(at));
                    }
                    ctrl.pop();
                }
                Instr::Br(l) => {
                    pc = do_branch(&mut stack, &mut ctrl, *l)?;
                    continue;
                }
                Instr::BrIf(l) => {
                    if pop_i32(&mut stack)? != 0 {
                        pc = do_branch(&mut stack, &mut ctrl, *l)?;
                        continue;
                    }
                }
                Instr::Return => {
                    let outermost = ctrl.len() as u32 - 1;
                    pc = do_branch(&mut stack, &mut ctrl, outermost)?;
                    continue;
                }
                Instr::Call(x) => {
                    let callee = module
                        .types
                        .get(
                            module
                                .funcs
                                .get(*x as usize)
                                .ok_or_else(|| trap("call to undefined function"))?
                                .type_idx as usize,
                        )
                        .ok_or_else(|| trap("callee references unknown type"))?;
                    let n = callee.params.len();
                    let at = stack
                        .len()
                        .checked_sub(n)
                        .ok_or_else(|| trap("call: not enough arguments on stack"))?;
                    let cargs = stack.split_off(at);
                    let res = self.call_func(*x as usize, &cargs, depth + 1)?;
                    stack.extend_from_slice(&res);
                }
                Instr::Drop => {
                    stack.pop().ok_or_else(|| trap("drop: empty stack"))?;
                }
                Instr::Select => {
                    let c = pop_i32(&mut stack)?;
                    let b = stack.pop().ok_or_else(|| trap("select: empty stack"))?;
                    let a = stack.pop().ok_or_else(|| trap("select: empty stack"))?;
                    stack.push(if c != 0 { a } else { b });
                }
                Instr::LocalGet(i) => {
                    let v = *locals
                        .get(*i as usize)
                        .ok_or_else(|| trap("local.get out of range"))?;
                    stack.push(v);
                }
                Instr::LocalSet(i) => {
                    let v = stack.pop().ok_or_else(|| trap("local.set: empty stack"))?;
                    *locals
                        .get_mut(*i as usize)
                        .ok_or_else(|| trap("local.set out of range"))? = v;
                }
                Instr::LocalTee(i) => {
                    let v = *stack.last().ok_or_else(|| trap("local.tee: empty stack"))?;
                    *locals
                        .get_mut(*i as usize)
                        .ok_or_else(|| trap("local.tee out of range"))? = v;
                }
                Instr::I32Const(v) => stack.push(Val::I32(*v)),
                Instr::I64Const(v) => stack.push(Val::I64(*v)),
                Instr::Load { op, offset } => exec_load(*op, *offset, &mut stack, &self.mem)?,
                Instr::Store { op, offset } => exec_store(*op, *offset, &mut stack, &mut self.mem)?,
                Instr::Num(op) => exec_num(*op, &mut stack)?,
            }
            pc += 1;
        }
    }
}

fn do_branch(stack: &mut Vec<Val>, ctrl: &mut Vec<CtrlFrame>, label: u32) -> Result<usize> {
    let idx = ctrl
        .len()
        .checked_sub(1 + label as usize)
        .ok_or_else(|| trap("branch label out of range"))?;
    let frame = ctrl[idx];
    let keep_from = stack
        .len()
        .checked_sub(frame.arity)
        .ok_or_else(|| trap("branch: stack underflow"))?;
    let kept: Vec<Val> = stack.split_off(keep_from);
    stack.truncate(frame.height);
    stack.extend_from_slice(&kept);
    ctrl.truncate(idx + 1); // leave the targeted frame on top
    Ok(frame.target)
}

// --- operand helpers --------------------------------------------------------

fn pop_i32(stack: &mut Vec<Val>) -> Result<i32> {
    match stack.pop() {
        Some(Val::I32(v)) => Ok(v),
        Some(Val::I64(_)) => Err(trap("type mismatch: expected i32, found i64")),
        None => Err(trap("operand stack underflow")),
    }
}

fn pop_i64(stack: &mut Vec<Val>) -> Result<i64> {
    match stack.pop() {
        Some(Val::I64(v)) => Ok(v),
        Some(Val::I32(_)) => Err(trap("type mismatch: expected i64, found i32")),
        None => Err(trap("operand stack underflow")),
    }
}

fn exec_load(op: LoadOp, offset: u32, stack: &mut Vec<Val>, mem: &[u8]) -> Result<()> {
    let base = pop_i32(stack)? as u32 as usize;
    let addr = base
        .checked_add(offset as usize)
        .ok_or_else(|| trap("address overflow"))?;
    let read = |n: usize| -> Result<&[u8]> {
        mem.get(addr..addr + n)
            .ok_or_else(|| trap("load out of bounds"))
    };
    let value = match op {
        LoadOp::I32 => Val::I32(i32::from_le_bytes(read(4)?.try_into().unwrap())),
        LoadOp::I64 => Val::I64(i64::from_le_bytes(read(8)?.try_into().unwrap())),
        LoadOp::I32_8U => Val::I32(i32::from(read(1)?[0])),
        LoadOp::I32_8S => Val::I32(i32::from(read(1)?[0] as i8)),
        LoadOp::I32_16U => Val::I32(i32::from(u16::from_le_bytes(read(2)?.try_into().unwrap()))),
        LoadOp::I32_16S => Val::I32(i32::from(i16::from_le_bytes(read(2)?.try_into().unwrap()))),
    };
    stack.push(value);
    Ok(())
}

fn exec_store(op: StoreOp, offset: u32, stack: &mut Vec<Val>, mem: &mut [u8]) -> Result<()> {
    // Operand order: address is pushed first, value last (so value is on top).
    let bytes: Vec<u8> = match op {
        StoreOp::I32 => pop_i32(stack)?.to_le_bytes().to_vec(),
        StoreOp::I64 => pop_i64(stack)?.to_le_bytes().to_vec(),
        StoreOp::I32_8 => vec![pop_i32(stack)? as u8],
        StoreOp::I32_16 => (pop_i32(stack)? as u16).to_le_bytes().to_vec(),
    };
    let base = pop_i32(stack)? as u32 as usize;
    let addr = base
        .checked_add(offset as usize)
        .ok_or_else(|| trap("address overflow"))?;
    let dst = mem
        .get_mut(addr..addr + bytes.len())
        .ok_or_else(|| trap("store out of bounds"))?;
    dst.copy_from_slice(&bytes);
    Ok(())
}

fn exec_num(op: NumOp, stack: &mut Vec<Val>) -> Result<()> {
    use NumOp::*;
    match op {
        // --- i32 unary ---
        I32Eqz => {
            let a = pop_i32(stack)?;
            stack.push(Val::I32((a == 0) as i32));
        }
        I32Clz => {
            let a = pop_i32(stack)?;
            stack.push(Val::I32(a.leading_zeros() as i32));
        }
        I32Ctz => {
            let a = pop_i32(stack)?;
            stack.push(Val::I32(a.trailing_zeros() as i32));
        }
        I32Popcnt => {
            let a = pop_i32(stack)?;
            stack.push(Val::I32(a.count_ones() as i32));
        }
        I64Eqz => {
            let a = pop_i64(stack)?;
            stack.push(Val::I32((a == 0) as i32));
        }
        // --- i32 binary ---
        I32Eq | I32Ne | I32LtS | I32LtU | I32GtS | I32GtU | I32LeS | I32LeU | I32GeS | I32GeU
        | I32Add | I32Sub | I32Mul | I32DivS | I32DivU | I32RemS | I32RemU | I32And | I32Or
        | I32Xor | I32Shl | I32ShrS | I32ShrU | I32Rotl | I32Rotr => {
            let b = pop_i32(stack)?;
            let a = pop_i32(stack)?;
            stack.push(exec_i32_binop(op, a, b)?);
        }
        // --- i64 binary ---
        I64Eq | I64Ne | I64LtS | I64LtU | I64GtS | I64GtU | I64LeS | I64LeU | I64GeS | I64GeU
        | I64Add | I64Sub | I64Mul | I64DivS | I64DivU | I64RemS | I64RemU | I64And | I64Or
        | I64Xor | I64Shl | I64ShrS | I64ShrU => {
            let b = pop_i64(stack)?;
            let a = pop_i64(stack)?;
            stack.push(exec_i64_binop(op, a, b)?);
        }
        // --- conversions ---
        I32WrapI64 => {
            let a = pop_i64(stack)?;
            stack.push(Val::I32(a as i32));
        }
        I64ExtendI32S => {
            let a = pop_i32(stack)?;
            stack.push(Val::I64(i64::from(a)));
        }
        I64ExtendI32U => {
            let a = pop_i32(stack)?;
            stack.push(Val::I64(i64::from(a as u32)));
        }
    }
    Ok(())
}

fn exec_i32_binop(op: NumOp, a: i32, b: i32) -> Result<Val> {
    use NumOp::*;
    let sh = (b as u32) & 31;
    let val = match op {
        I32Eq => Val::I32((a == b) as i32),
        I32Ne => Val::I32((a != b) as i32),
        I32LtS => Val::I32((a < b) as i32),
        I32LtU => Val::I32(((a as u32) < (b as u32)) as i32),
        I32GtS => Val::I32((a > b) as i32),
        I32GtU => Val::I32(((a as u32) > (b as u32)) as i32),
        I32LeS => Val::I32((a <= b) as i32),
        I32LeU => Val::I32(((a as u32) <= (b as u32)) as i32),
        I32GeS => Val::I32((a >= b) as i32),
        I32GeU => Val::I32(((a as u32) >= (b as u32)) as i32),
        I32Add => Val::I32(a.wrapping_add(b)),
        I32Sub => Val::I32(a.wrapping_sub(b)),
        I32Mul => Val::I32(a.wrapping_mul(b)),
        I32DivS => {
            if b == 0 {
                return Err(trap("i32.div_s by zero"));
            }
            if a == i32::MIN && b == -1 {
                return Err(trap("i32.div_s overflow"));
            }
            Val::I32(a / b)
        }
        I32DivU => {
            if b == 0 {
                return Err(trap("i32.div_u by zero"));
            }
            Val::I32(((a as u32) / (b as u32)) as i32)
        }
        I32RemS => {
            if b == 0 {
                return Err(trap("i32.rem_s by zero"));
            }
            Val::I32(a.wrapping_rem(b)) // wrapping_rem handles MIN % -1 == 0
        }
        I32RemU => {
            if b == 0 {
                return Err(trap("i32.rem_u by zero"));
            }
            Val::I32(((a as u32) % (b as u32)) as i32)
        }
        I32And => Val::I32(a & b),
        I32Or => Val::I32(a | b),
        I32Xor => Val::I32(a ^ b),
        I32Shl => Val::I32(a.wrapping_shl(sh)),
        I32ShrS => Val::I32(a.wrapping_shr(sh)),
        I32ShrU => Val::I32(((a as u32).wrapping_shr(sh)) as i32),
        I32Rotl => Val::I32(a.rotate_left(sh)),
        I32Rotr => Val::I32(a.rotate_right(sh)),
        _ => return Err(trap("internal: non-i32-binop dispatched")),
    };
    Ok(val)
}

fn exec_i64_binop(op: NumOp, a: i64, b: i64) -> Result<Val> {
    use NumOp::*;
    let sh = (b as u64) & 63;
    let val = match op {
        I64Eq => Val::I32((a == b) as i32),
        I64Ne => Val::I32((a != b) as i32),
        I64LtS => Val::I32((a < b) as i32),
        I64LtU => Val::I32(((a as u64) < (b as u64)) as i32),
        I64GtS => Val::I32((a > b) as i32),
        I64GtU => Val::I32(((a as u64) > (b as u64)) as i32),
        I64LeS => Val::I32((a <= b) as i32),
        I64LeU => Val::I32(((a as u64) <= (b as u64)) as i32),
        I64GeS => Val::I32((a >= b) as i32),
        I64GeU => Val::I32(((a as u64) >= (b as u64)) as i32),
        I64Add => Val::I64(a.wrapping_add(b)),
        I64Sub => Val::I64(a.wrapping_sub(b)),
        I64Mul => Val::I64(a.wrapping_mul(b)),
        I64DivS => {
            if b == 0 {
                return Err(trap("i64.div_s by zero"));
            }
            if a == i64::MIN && b == -1 {
                return Err(trap("i64.div_s overflow"));
            }
            Val::I64(a / b)
        }
        I64DivU => {
            if b == 0 {
                return Err(trap("i64.div_u by zero"));
            }
            Val::I64(((a as u64) / (b as u64)) as i64)
        }
        I64RemS => {
            if b == 0 {
                return Err(trap("i64.rem_s by zero"));
            }
            Val::I64(a.wrapping_rem(b))
        }
        I64RemU => {
            if b == 0 {
                return Err(trap("i64.rem_u by zero"));
            }
            Val::I64(((a as u64) % (b as u64)) as i64)
        }
        I64And => Val::I64(a & b),
        I64Or => Val::I64(a | b),
        I64Xor => Val::I64(a ^ b),
        I64Shl => Val::I64(a.wrapping_shl(sh as u32)),
        I64ShrS => Val::I64(a.wrapping_shr(sh as u32)),
        I64ShrU => Val::I64(((a as u64).wrapping_shr(sh as u32)) as i64),
        _ => return Err(trap("internal: non-i64-binop dispatched")),
    };
    Ok(val)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::wasm::WasmRuntime;

    const SUM_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "sum_bytes") (param $ptr i32) (param $len i32) (result i32)
            (local $i i32) (local $acc i32)
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

    // Triangular number via a counting loop with an `if` guard.
    const TRI_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func (export "tri") (param $n i32) (result i32)
            (local $i i32) (local $acc i32)
            (block $done
              (loop $loop
                (br_if $done (i32.gt_s (local.get $i) (local.get $n)))
                (local.set $acc (i32.add (local.get $acc) (local.get $i)))
                (local.set $i (i32.add (local.get $i) (i32.const 1)))
                (br $loop)))
            (local.get $acc)))
    "#;

    // Exercises `call` between two functions.
    const CALL_WAT: &str = r#"
        (module
          (memory (export "memory") 1)
          (func $add1 (param $x i32) (result i32)
            (i32.add (local.get $x) (i32.const 1)))
          (func (export "add2") (param $x i32) (result i32)
            (call $add1 (call $add1 (local.get $x)))))
    "#;

    fn compile(wat: &str) -> PvModule {
        Interpreter::new()
            .load(&wat::parse_str(wat).unwrap())
            .unwrap()
    }

    #[test]
    fn runs_loop_summing_memory() {
        let m = compile(SUM_WAT);
        assert_eq!(m.call_scalar("sum_bytes", &[1, 2, 3, 4, 10]).unwrap(), 20);
    }

    #[test]
    fn runs_in_place_store_loop() {
        let m = compile(INC_WAT);
        assert_eq!(
            m.apply_in_place("inc", &[0, 9, 254]).unwrap(),
            vec![1, 10, 255]
        );
    }

    #[test]
    fn runs_if_guarded_loop() {
        let m = compile(TRI_WAT);
        assert_eq!(m.invoke_i32("tri", &[5]).unwrap(), vec![15]);
        assert_eq!(m.invoke_i32("tri", &[0]).unwrap(), vec![0]);
        assert_eq!(m.invoke_i32("tri", &[100]).unwrap(), vec![5050]);
    }

    #[test]
    fn runs_function_calls() {
        let m = compile(CALL_WAT);
        assert_eq!(m.invoke_i32("add2", &[40]).unwrap(), vec![42]);
    }

    #[test]
    fn matches_wasmi_differentially() {
        // The hand-written interpreter must agree with the reference engine.
        let bytes = wat::parse_str(SUM_WAT).unwrap();
        let ours = Interpreter::new().load(&bytes).unwrap();
        let reference = WasmRuntime::new().load(&bytes).unwrap();
        for input in [
            vec![],
            vec![255u8],
            vec![1, 2, 3],
            (0..200u16).map(|b| b as u8).collect::<Vec<u8>>(),
        ] {
            let a = WasmExec::call_scalar(&ours, "sum_bytes", &input).unwrap();
            let b = WasmExec::call_scalar(&reference, "sum_bytes", &input).unwrap();
            assert_eq!(a, b, "mismatch on input {input:?}");
        }
    }

    #[test]
    fn rejects_unsupported_and_garbage() {
        assert!(Interpreter::new().load(&[0, 1, 2, 3]).is_err()); // bad magic
                                                                  // A float op should be rejected rather than mis-run.
        let float_wat = r#"(module (func (export "f") (result f32) (f32.const 1)))"#;
        assert!(wat::parse_str(float_wat)
            .map(|b| Interpreter::new().load(&b))
            .unwrap()
            .is_err());
    }
}
