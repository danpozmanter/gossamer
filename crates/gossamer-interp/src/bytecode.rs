//! Register-based bytecode for the VM.
//! Each `FnChunk` owns a flat vector of [`Op`] instructions plus a
//! constant pool. Registers are virtual `u16` indices into the active
//! frame's register file. The compiler in [`crate::compile`] allocates
//! a contiguous register for every HIR local and intermediate value.

#![forbid(unsafe_code)]
#![allow(missing_docs, unreachable_pub)]

use std::sync::Arc;

use crate::value::Value;

/// Virtual register index within a frame's register file.
pub type Reg = u16;

/// Index into a function's constant pool.
pub type ConstIdx = u16;

/// Global symbol index resolved at link time.
pub type GlobalIdx = u16;

/// Absolute instruction index inside a chunk.
pub type InstrIdx = u32;

/// Bytecode instructions. The VM dispatch loop is a `match` over this
/// enum — fast enough for the "parity with the tree-walker" bar
/// and trivially safe. Every variant's payload is `Copy`, so the
/// dispatch loop can pull instructions without cloning. The
/// explicit `u16` discriminant keeps the per-op memory footprint
/// (and therefore the memcpy per dispatch) as small as the
/// largest variant's payload allows.
#[derive(Debug, Clone, Copy)]
#[repr(u16)]
pub enum Op {
    /// `dst = consts[idx]`.
    LoadConst { dst: Reg, idx: ConstIdx },
    /// `dst = globals[idx]`.
    LoadGlobal { dst: Reg, idx: GlobalIdx },
    /// `dst = src`.
    Move { dst: Reg, src: Reg },
    /// Generic boxed-`Value` addition: `dst = lhs + rhs`. Carries
    /// an inline-cache slot index that the runtime fills on first
    /// execution with the observed `(lhs, rhs)` shape (see
    /// [`ArithCacheSlot`] / `ARITH_*` constants); subsequent
    /// dispatches branch directly into the specialised arm and
    /// skip the per-call `(Value, Value)` discriminant match.
    /// Tier C2 of the interp wow plan.
    AddInt {
        dst: Reg,
        lhs: Reg,
        rhs: Reg,
        cache_idx: u16,
    },
    /// `dst = lhs - rhs` on boxed `Value`. Adaptive — see [`Op::AddInt`].
    SubInt {
        dst: Reg,
        lhs: Reg,
        rhs: Reg,
        cache_idx: u16,
    },
    /// `dst = lhs * rhs` on boxed `Value`. Adaptive — see [`Op::AddInt`].
    MulInt {
        dst: Reg,
        lhs: Reg,
        rhs: Reg,
        cache_idx: u16,
    },
    /// `dst = lhs / rhs` on boxed `Value`. Adaptive — see [`Op::AddInt`].
    DivInt {
        dst: Reg,
        lhs: Reg,
        rhs: Reg,
        cache_idx: u16,
    },
    /// `dst = lhs % rhs` on boxed `Value`. Adaptive — see [`Op::AddInt`].
    RemInt {
        dst: Reg,
        lhs: Reg,
        rhs: Reg,
        cache_idx: u16,
    },
    /// `dst = -operand` on `Int` or `Float`.
    Neg { dst: Reg, operand: Reg },
    /// `dst = !operand` on `Bool`.
    Not { dst: Reg, operand: Reg },
    /// `dst = lhs == rhs`, kind-aware.
    Eq { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs != rhs`.
    Ne { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs < rhs`.
    Lt { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs <= rhs`.
    Le { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs > rhs`.
    Gt { dst: Reg, lhs: Reg, rhs: Reg },
    /// `dst = lhs >= rhs`.
    Ge { dst: Reg, lhs: Reg, rhs: Reg },
    /// Unconditional jump to `target`.
    Jump { target: InstrIdx },
    /// Branch to `target` when `cond` is truthy; fall through otherwise.
    BranchIf { cond: Reg, target: InstrIdx },
    /// Branch to `target` when `cond` is falsy.
    BranchIfNot { cond: Reg, target: InstrIdx },
    /// Call `callee` with `argc` arguments drawn from consecutive
    /// registers starting at `args`. Stores the result in `dst`.
    Call {
        /// Destination register for the returned value.
        dst: Reg,
        /// Register holding the callee value.
        callee: Reg,
        /// First argument register. Arguments live in
        /// `[args .. args + argc)`.
        args: Reg,
        /// Number of arguments.
        argc: u16,
        /// Index into the chunk's `call_caches` slot. The slot
        /// caches the resolved [`crate::vm::Global`] for the most
        /// recently seen callee identity, skipping the
        /// `Value::String → globals.get` path on subsequent calls
        /// from the same site.
        cache_idx: u16,
    },
    /// `ret value`.
    Return { value: Reg },
    /// `ret ()`.
    ReturnUnit,
    /// Delegates a single HIR expression evaluation to a bundled
    /// tree-walker. The VM compiler falls back to this op for any
    /// expression kind that doesn't yet have a native opcode —
    /// method calls, match, closures, struct literals, etc. The
    /// expression is stored in the chunk's `deferred_exprs` table
    /// at `expr_idx`; runtime evaluation uses the outer `Vm`'s
    /// bundled interpreter so the result shares the same Value
    /// representation. Local-binding registers available to the
    /// delegated expression start at `env_start` and span
    /// `env_count` entries paired with names in the chunk's
    /// `deferred_env` table.
    EvalDeferred {
        /// Destination register.
        dst: Reg,
        /// Index into `FnChunk::deferred_exprs`. The matching
        /// `deferred_envs[expr_idx]` entry carries the binding
        /// names paired with the registers listed in
        /// `deferred_env_regs[expr_idx]`. Mutations the walker
        /// makes through those bindings (`bodies[i].vx = x`)
        /// flow back into the same registers after the call.
        expr_idx: u32,
    },
    /// `dst = receiver.method_name(args…)` — native method
    /// dispatch. `name_idx` is a `ConstIdx` into the chunk's
    /// globals table (keyed by the bare method name). The VM
    /// puts the receiver value at `args` and the remaining args
    /// at `args+1..args+argc+1`, then calls the looked-up
    /// builtin / closure. Skips the `EvalDeferred` env-rebuild
    /// cost for the most common hot-path shape.
    MethodCall {
        /// Destination register.
        dst: Reg,
        /// Register holding the receiver value.
        receiver: Reg,
        /// Index into `FnChunk::globals` — holds the bare
        /// method name.
        name_idx: GlobalIdx,
        /// First user-arg register. Receiver is stored at
        /// `args - 1` during dispatch so the call frame sees
        /// `[receiver, a0, a1, …]`.
        args: Reg,
        /// Number of user-supplied arguments.
        argc: u16,
        /// Index into the chunk's `call_caches` slot. The slot
        /// caches the resolved [`crate::vm::Global`] for the most
        /// recently seen receiver type, skipping the
        /// `qualified_key`/`HashMap::get` chain on subsequent
        /// calls from the same site.
        cache_idx: u16,
    },
    /// Specialised `<stream>.write_byte(<byte>)` — fused
    /// super-instruction emitted whenever the compiler sees a
    /// method call whose name is `write_byte` and whose argc is 1.
    /// fasta's hot loop is dominated by per-character calls
    /// through this exact shape; bypassing the
    /// `MethodCall` + IC + Vec-args + builtin-extract chain saves
    /// the receiver clone + per-call buf-init + indirect dispatch.
    /// The handler verifies the receiver is a `Value::Struct`
    /// named `"Stream"` at runtime and falls back to a regular
    /// `MethodCall`-shaped lookup if not — so emitting this op
    /// for any user-defined `write_byte` is still correct, just
    /// not as fast.
    StreamWriteByte {
        /// Destination register (always written `Value::Unit`
        /// since `write_byte` returns unit).
        dst: Reg,
        /// Register holding the stream value (a
        /// `Value::struct_("Stream", [(fd)`
        /// in the steady state).
        stream_reg: Reg,
        /// Register holding the byte (a `Value::Int` in
        /// `[0, 255]` in the steady state).
        byte_reg: Reg,
    },
    /// Specialised `<u8vec>.set_byte(<idx>, <byte>)` — the
    /// `U8Vec` counterpart to [`Op::StreamWriteByte`]. The runtime
    /// inlines the handle lookup and `AtomicU8::store`, skipping
    /// the `Op::MethodCall` IC + builtin `&[Value]` round-trip
    /// per call. fasta's per-byte buffer fill rides this op.
    /// Falls back to a generic method dispatch on shape miss.
    U8VecSetByte {
        /// Destination register (always `Value::Unit` since
        /// `set_byte` returns unit).
        dst: Reg,
        /// Register holding the `U8Vec` receiver
        /// (`Value::Struct{ name: "U8Vec", … }`).
        u8vec_reg: Reg,
        /// Register holding the byte index (`Value::Int`).
        idx_reg: Reg,
        /// Register holding the byte value (`Value::Int` in
        /// `[0, 255]`).
        byte_reg: Reg,
    },
    /// Specialised `<u8vec>.get_byte(<idx>)` returning into a
    /// typed `i64` register. Mirror of [`Op::U8VecSetByte`] for
    /// reads — the typed destination lets a downstream `Op::AddI64`
    /// chain off the result without a `Value::Int` round-trip.
    U8VecGetByte {
        /// Destination `i64` register.
        dst_i: Reg,
        /// Register holding the `U8Vec` receiver.
        u8vec_reg: Reg,
        /// Register holding the byte index (`Value::Int`).
        idx_reg: Reg,
    },
    /// Specialised `m.insert(k, m.get_or(k, 0) + by)` — fused
    /// counter-increment super-instruction. Collapses the two
    /// `MethodCall`s, two IC probes, two arg-vec materialisations,
    /// and (crucially) the two `parking_lot::Mutex` acquisitions
    /// into a single `entry()`-API increment under one lock.
    /// Counter-style hot loops are dominated by this pattern.
    MapInc {
        /// Destination register (the resulting Map handle, mirroring
        /// the original `insert` return value).
        dst: Reg,
        /// Register holding the Map (`Value::Map`).
        map_reg: Reg,
        /// Register holding the key (any hashable Value).
        key_reg: Reg,
        /// Register holding the increment (`Value::Int`).
        by_reg: Reg,
    },
    /// Specialised `m.inc_at(seq, start, len, by)` — zero-copy
    /// slice-hash counter that hashes `seq[start..start+len]`
    /// directly, matching `*m.entry(&seq[i..i+k]).or_insert(0)
    /// += by`. Skips the generic builtin-call overhead by
    /// inlining the slice-hash + entry increment under one Mutex
    /// acquisition. Result register holds the post-increment
    /// value as a `Value::Int`. Carried via `WideOp::MapIncAt` in
    /// the chunk's `wide_ops` side-table — see `Op::Wide`.
    Wide {
        /// Index into `FnChunk::wide_ops`.
        idx: u16,
    },
    /// Builds a `Value::IntArray` from `count` consecutive typed
    /// `i64` registers starting at `first_i`. Counterpart of
    /// [`Op::BuildFloatArray`] for primitive integer arrays
    /// (`[i64; N]` literals).
    BuildIntArray {
        /// Destination value register.
        dst_v: Reg,
        /// First `i64` register holding the array's elements
        /// (contiguous, length `count`).
        first_i: Reg,
        /// Number of elements.
        count: u16,
    },
    /// Builds a `Value::Tuple` from `count` consecutive value
    /// registers starting at `first`. Replaces the `EvalDeferred`
    /// tree-walker fallback that previously fired on every
    /// `(a, b, …)` literal — that path allocated a bindings
    /// `Vec`, rebuilt an env, re-evaluated each element through
    /// the walker, and packed the result. The native op just
    /// `Arc::clone`s each register and assembles the tuple.
    BuildTuple {
        /// Destination value register.
        dst: Reg,
        /// First value register holding the tuple's elements.
        first: Reg,
        /// Number of elements.
        count: u16,
    },
    /// Typed numeric cast: `i64 as f64`. Reads from the `i64`
    /// register file and writes to the `f64` register file with
    /// no boxing. Replaces the deferred-walker path that
    /// previously rebuilt an env per cast.
    IntToFloatF64 {
        /// Destination `f64` register.
        dst_f: Reg,
        /// Source `i64` register.
        src_i: Reg,
    },
    /// Typed numeric cast: `f64 as i64` (truncation toward
    /// zero, matching Rust `as` semantics).
    FloatToIntI64 {
        /// Destination `i64` register.
        dst_i: Reg,
        /// Source `f64` register.
        src_f: Reg,
    },
    /// Typed read into an `i64` register from a `Value::IntArray`
    /// base. Skips the per-read enum match + boxing the generic
    /// `Op::IndexGet` performs. fasta's TWO/THREE inner loops
    /// hit this op ~5 times per output byte.
    IntArrayGetI64 {
        /// Destination `i64` register.
        dst_i: Reg,
        /// Value register holding the `Value::IntArray`.
        base: Reg,
        /// `i64` register holding the index. Negative indices
        /// surface as a runtime error.
        index_i: Reg,
    },
    /// Builds a `Value::FloatVec` by copying `count` consecutive
    /// `f64` registers starting at `first_f`. Mirrors `BuildIntArray`
    /// but for primitive `[f64; N]` literals so subsequent indexed
    /// reads route through the typed-`f64` fast path.
    BuildFloatVec {
        /// Destination `Value` register.
        dst_v: Reg,
        /// First float register in the source span.
        first_f: Reg,
        /// Number of f64 elements to gather.
        count: u16,
    },
    /// Typed read into an `f64` register from a `Value::FloatVec`
    /// at `index_i`. Skips the boxed `Value::Float` round-trip the
    /// generic `Op::IndexGet` would impose.
    FloatVecGetF64 {
        /// Destination `f64` register.
        dst_f: Reg,
        /// Value register holding the `Value::FloatVec`.
        base: Reg,
        /// `i64` register holding the index.
        index_i: Reg,
    },
    /// Typed write into a `Value::FloatVec` from an `f64` register.
    /// `Arc::make_mut` mutates the inner `Vec<f64>` in place when
    /// the `FloatVec` has unique ownership.
    FloatVecSetF64 {
        /// Value register holding the `Value::FloatVec`.
        base: Reg,
        /// `i64` register holding the index.
        index_i: Reg,
        /// Source `f64` register.
        value_f: Reg,
    },
    /// Constructs an empty `Value::IntMap` (typed `HashMap<i64, i64>`).
    /// Emitted in place of a `HashMap::new()` call when the type
    /// checker can prove the map's key + value types are both
    /// `i64`. Hot integer counter loops route through this op.
    BuildIntMap {
        /// Destination `Value` register.
        dst_v: Reg,
    },
    /// Typed counterpart to [`Op::MapInc`] for `Value::IntMap`. Reads
    /// the key and increment from the i64 register file, mutates
    /// the map's slot in place, and writes the post-increment value
    /// to `dst_i`. Skips the [`MapKey`] enum dispatch and the
    /// [`Value::Int`] box that the generic `Op::MapInc` does.
    IntMapInc {
        /// Destination `i64` register receiving the post-increment value.
        dst_i: Reg,
        /// `Value` register holding the `Value::IntMap`.
        map_reg: Reg,
        /// `i64` register holding the key.
        key_i: Reg,
        /// `i64` register holding the increment amount.
        by_i: Reg,
    },
    /// `dst_i = map.get_or(key, default)` for `Value::IntMap`.
    IntMapGetOr {
        /// Destination `i64` register.
        dst_i: Reg,
        /// `Value` register holding the `Value::IntMap`.
        map_reg: Reg,
        /// `i64` register holding the key.
        key_i: Reg,
        /// `i64` register holding the default to return on miss.
        default_i: Reg,
    },
    /// `map.insert(key, value)` for `Value::IntMap`. The map handle
    /// stays in `map_reg`; `dst_v` receives a clone of the handle
    /// so callers using the result `m.insert(...)` form get the
    /// same semantics as `Op::MapInc`'s generic counterpart.
    IntMapInsert {
        /// Destination `Value` register receiving the map handle.
        dst_v: Reg,
        /// `Value` register holding the `Value::IntMap`.
        map_reg: Reg,
        /// `i64` register holding the key.
        key_i: Reg,
        /// `i64` register holding the value to store.
        value_i: Reg,
    },
    /// `dst_i = map.len()` for `Value::IntMap`. Locks once, reads
    /// `len()`, returns. No `MapKey` allocation.
    IntMapLen {
        /// Destination `i64` register.
        dst_i: Reg,
        /// `Value` register holding the `Value::IntMap`.
        map_reg: Reg,
    },
    /// `dst_v = bool(map.contains_key(key))` for `Value::IntMap`.
    IntMapContainsKey {
        /// Destination `Value` register (holds `Value::Bool`).
        dst_v: Reg,
        /// `Value` register holding the `Value::IntMap`.
        map_reg: Reg,
        /// `i64` register holding the key.
        key_i: Reg,
    },
    /// `go callee(args[0..argc])` — spawns a goroutine that runs
    /// `callee` with the supplied args entirely through the bytecode
    /// VM (no tree-walker re-entry). Replaces the prior
    /// `compile_deferred(Go)` path that bounced every goroutine
    /// body through the slow walker; required `FnChunk` to be
    /// `Send + Sync` (call/arith caches now `parking_lot::Mutex`
    /// rather than `RefCell`).
    Spawn {
        /// Register holding the callee value (`Value::Closure` /
        /// `Value::Builtin` / `Value::String` global name / etc.).
        callee: Reg,
        /// First register of the argument span. The block of `argc`
        /// registers starting here is cloned into the new
        /// goroutine's frame at spawn time.
        args: Reg,
        /// Number of arguments to pass.
        argc: u16,
    },
    /// `go receiver.method_name(args[0..argc])` — spawns a
    /// goroutine running the method whose name lives in the
    /// chunk's globals at `name_idx`. Mirrors `Op::MethodCall`'s
    /// resolution chain (`qualified_key` then bare name) so a
    /// freshly-spawned goroutine takes the same dispatch path the
    /// synchronous call would. Without this op, `go obj.method()`
    /// fell through to the deferred tree-walker which ran the body
    /// on the calling thread synchronously — defeating goroutine
    /// semantics.
    SpawnMethod {
        /// Register holding the receiver value.
        receiver: Reg,
        /// Index into `FnChunk::globals` — holds the bare method name.
        name_idx: GlobalIdx,
        /// First register of the argument span.
        args: Reg,
        /// Number of user-supplied arguments (receiver excluded).
        argc: u16,
    },
    /// `dst = base[index]` — native indexed read over arrays,
    /// strings, tuples, vecs, and structs (tuple-struct
    /// projection). Mirrors the tree-walker's `eval_index`.
    IndexGet {
        /// Destination register.
        dst: Reg,
        /// Register holding the base (array / string / …).
        base: Reg,
        /// Register holding the index value.
        index: Reg,
    },
    /// `base[index] = value` — native indexed write.
    IndexSet {
        /// Register holding the base.
        base: Reg,
        /// Register holding the index value.
        index: Reg,
        /// Register holding the value to store.
        value: Reg,
    },
    /// `dst = receiver.field_name` — native struct-field read.
    /// `name_idx` is a const-pool index holding a
    /// `Value::String` with the field name.
    FieldGet {
        /// Destination register.
        dst: Reg,
        /// Register holding the struct value.
        receiver: Reg,
        /// Const-pool index of the field-name string.
        name_idx: ConstIdx,
        /// Per-`Vm` field-cache slot. On hit, the dispatcher
        /// jumps straight to `inner.fields[offset].1.clone()`,
        /// skipping the linear name scan that the generic
        /// fallback does. On miss (observed struct shape
        /// changed), refill the slot. PEP 659-style.
        cache_idx: u16,
    },
    /// `receiver.field_name = value` — native struct-field
    /// write. Mutates the fields vector in place (`Arc::make_mut`
    /// semantics).
    FieldSet {
        /// Register holding the struct value.
        receiver: Reg,
        /// Const-pool index of the field-name string.
        name_idx: ConstIdx,
        /// Register holding the value to store.
        value: Reg,
    },
    /// `dst = receiver.N` — native tuple / positional-field
    /// read.
    TupleIndex {
        /// Destination register.
        dst: Reg,
        /// Register holding the tuple.
        receiver: Reg,
        /// Zero-based index.
        index: u32,
    },
    /// `base[index].field_name = value` — fused in-place
    /// write. Avoids the `IndexGet` / `FieldSet` / `IndexSet`
    /// round-trip (and its O(n) Vec clones) that dominates
    /// hot loops iterating over arrays of structs
    /// (e.g. nbody's `bodies[i].vx = ...`). `base` must be a
    /// local register holding the array; since no other
    /// register holds the same Arc, `Arc::make_mut` hits the
    /// non-cloning path and the whole op becomes O(1).
    IndexedFieldSet {
        /// Register holding the base array.
        base: Reg,
        /// Register holding the index value.
        index: Reg,
        /// Const-pool index of the field-name string.
        name_idx: ConstIdx,
        /// Register holding the value to store.
        value: Reg,
    },

    // ----- Phase 1: unboxed f64 register-file ops -----
    //
    // Operands named `*_f` live in the frame's float register
    // file (`Vec<f64>`); operands named `*_v` live in the
    // regular `Value` register file. All other Reg slots in
    // these ops refer to the indicated file — the compiler
    // keeps them straight.
    /// `floats[dst_f] = f64_consts[idx]`. Uses a dedicated
    /// f64 constant pool so the `Op` enum stays small (the
    /// largest variant drives enum size, which the dispatch
    /// loop copies per instruction).
    LoadConstF64 { dst_f: Reg, idx: ConstIdx },
    /// `floats[dst_f] = floats[lhs_f] + floats[rhs_f]`.
    AddF64 { dst_f: Reg, lhs_f: Reg, rhs_f: Reg },
    /// `floats[dst_f] = floats[lhs_f] - floats[rhs_f]`.
    SubF64 { dst_f: Reg, lhs_f: Reg, rhs_f: Reg },
    /// `floats[dst_f] = floats[lhs_f] * floats[rhs_f]`.
    MulF64 { dst_f: Reg, lhs_f: Reg, rhs_f: Reg },
    /// `floats[dst_f] = floats[lhs_f] / floats[rhs_f]`.
    DivF64 { dst_f: Reg, lhs_f: Reg, rhs_f: Reg },
    /// `floats[dst_f] = -floats[src_f]`.
    NegF64 { dst_f: Reg, src_f: Reg },
    /// `registers[dst_v] = Bool(floats[lhs_f] < floats[rhs_f])`.
    LtF64 { dst_v: Reg, lhs_f: Reg, rhs_f: Reg },
    /// `registers[dst_v] = Bool(floats[lhs_f] <= floats[rhs_f])`.
    LeF64 { dst_v: Reg, lhs_f: Reg, rhs_f: Reg },
    /// `registers[dst_v] = Bool(floats[lhs_f] > floats[rhs_f])`.
    GtF64 { dst_v: Reg, lhs_f: Reg, rhs_f: Reg },
    /// `registers[dst_v] = Bool(floats[lhs_f] >= floats[rhs_f])`.
    GeF64 { dst_v: Reg, lhs_f: Reg, rhs_f: Reg },
    /// `registers[dst_v] = Bool(floats[lhs_f] == floats[rhs_f])`.
    EqF64 { dst_v: Reg, lhs_f: Reg, rhs_f: Reg },
    /// `registers[dst_v] = Bool(floats[lhs_f] != floats[rhs_f])`.
    NeF64 { dst_v: Reg, lhs_f: Reg, rhs_f: Reg },
    /// `floats[dst_f] = src_v.as_float()` — unbox an f64 out
    /// of a `Value::Float` for use with the typed ops.
    UnboxF64 { dst_f: Reg, src_v: Reg },
    /// `registers[dst_v] = Value::Float(floats[src_f])` —
    /// re-box an f64 register for ABI-crossing use (calls,
    /// field stores, returns).
    BoxF64 { dst_v: Reg, src_f: Reg },
    /// `floats[dst_f] = sqrt(floats[src_f])` — inlined
    /// `math::sqrt` intrinsic.
    SqrtF64 { dst_f: Reg, src_f: Reg },
    /// `floats[dst_f] = sin(floats[src_f])`.
    SinF64 { dst_f: Reg, src_f: Reg },
    /// `floats[dst_f] = cos(floats[src_f])`.
    CosF64 { dst_f: Reg, src_f: Reg },
    /// `floats[dst_f] = floats[src_f].abs()`.
    AbsF64 { dst_f: Reg, src_f: Reg },
    /// `floats[dst_f] = floats[src_f].floor()`.
    FloorF64 { dst_f: Reg, src_f: Reg },
    /// `floats[dst_f] = floats[src_f].ceil()`.
    CeilF64 { dst_f: Reg, src_f: Reg },
    /// `floats[dst_f] = floats[src_f].exp()`.
    ExpF64 { dst_f: Reg, src_f: Reg },
    /// `floats[dst_f] = floats[src_f].ln()`.
    LnF64 { dst_f: Reg, src_f: Reg },
    /// Fused multiply-add: `floats[dst_f] = floats[a_f] *
    /// floats[b_f] + floats[c_f]`. Emitted when the compiler
    /// sees `a * b + c` (or `c + a * b`), which is extremely
    /// common in vector math (`x + dt * vx`). Saves one op
    /// per use plus enables a single `fma` or `vfmadd*`
    /// instruction when a JIT later consumes this bytecode.
    MulAddF64 {
        dst_f: Reg,
        a_f: Reg,
        b_f: Reg,
        c_f: Reg,
    },
    /// Fused multiply-subtract: `floats[dst_f] = floats[c_f] -
    /// floats[a_f] * floats[b_f]`. Matches the nbody
    /// inner-loop pattern `vx - dx * mag`.
    MulSubF64 {
        dst_f: Reg,
        a_f: Reg,
        b_f: Reg,
        c_f: Reg,
    },

    // ----- Phase 1: unboxed i64 register-file ops -----
    /// `ints[dst_i] = i64_consts[idx]`.
    LoadConstI64 { dst_i: Reg, idx: ConstIdx },
    /// Wrapping `ints[dst_i] = ints[lhs_i] + ints[rhs_i]`.
    AddI64 { dst_i: Reg, lhs_i: Reg, rhs_i: Reg },
    /// Wrapping `ints[dst_i] = ints[lhs_i] - ints[rhs_i]`.
    SubI64 { dst_i: Reg, lhs_i: Reg, rhs_i: Reg },
    /// Wrapping `ints[dst_i] = ints[lhs_i] * ints[rhs_i]`.
    MulI64 { dst_i: Reg, lhs_i: Reg, rhs_i: Reg },
    /// Checked `ints[dst_i] = ints[lhs_i] / ints[rhs_i]`.
    DivI64 { dst_i: Reg, lhs_i: Reg, rhs_i: Reg },
    /// Checked `ints[dst_i] = ints[lhs_i] % ints[rhs_i]`.
    RemI64 { dst_i: Reg, lhs_i: Reg, rhs_i: Reg },
    /// Wrapping `ints[dst_i] = -ints[src_i]`.
    NegI64 { dst_i: Reg, src_i: Reg },
    /// `registers[dst_v] = Bool(ints[lhs_i] < ints[rhs_i])`.
    LtI64 { dst_v: Reg, lhs_i: Reg, rhs_i: Reg },
    /// `registers[dst_v] = Bool(ints[lhs_i] <= ints[rhs_i])`.
    LeI64 { dst_v: Reg, lhs_i: Reg, rhs_i: Reg },
    /// `registers[dst_v] = Bool(ints[lhs_i] > ints[rhs_i])`.
    GtI64 { dst_v: Reg, lhs_i: Reg, rhs_i: Reg },
    /// `registers[dst_v] = Bool(ints[lhs_i] >= ints[rhs_i])`.
    GeI64 { dst_v: Reg, lhs_i: Reg, rhs_i: Reg },
    /// `registers[dst_v] = Bool(ints[lhs_i] == ints[rhs_i])`.
    EqI64 { dst_v: Reg, lhs_i: Reg, rhs_i: Reg },
    /// `registers[dst_v] = Bool(ints[lhs_i] != ints[rhs_i])`.
    NeI64 { dst_v: Reg, lhs_i: Reg, rhs_i: Reg },
    /// Bitwise `ints[dst_i] = ints[lhs_i] & ints[rhs_i]`.
    BitAndI64 { dst_i: Reg, lhs_i: Reg, rhs_i: Reg },
    /// Bitwise `ints[dst_i] = ints[lhs_i] | ints[rhs_i]`.
    BitOrI64 { dst_i: Reg, lhs_i: Reg, rhs_i: Reg },
    /// Bitwise `ints[dst_i] = ints[lhs_i] ^ ints[rhs_i]`.
    BitXorI64 { dst_i: Reg, lhs_i: Reg, rhs_i: Reg },
    /// Wrapping `ints[dst_i] = ints[lhs_i] << (ints[rhs_i] & 63)`.
    ShlI64 { dst_i: Reg, lhs_i: Reg, rhs_i: Reg },
    /// Arithmetic `ints[dst_i] = ints[lhs_i] >> (ints[rhs_i] & 63)`
    /// (matches Rust's `i64 >> i64` semantics — sign-preserving).
    ShrI64 { dst_i: Reg, lhs_i: Reg, rhs_i: Reg },
    /// `ints[dst_i] = src_v.as_int()`.
    UnboxI64 { dst_i: Reg, src_v: Reg },
    /// `registers[dst_v] = Value::Int(ints[src_i])`.
    BoxI64 { dst_v: Reg, src_i: Reg },
    /// `floats[dst_f] = floats[src_f]` — float-file copy,
    /// used for `x = y` when both are in the float file.
    MoveF64 { dst_f: Reg, src_f: Reg },
    /// `ints[dst_i] = ints[src_i]`.
    MoveI64 { dst_i: Reg, src_i: Reg },

    // ----- Phase 2: fused / typed field access -----
    //
    // These opcodes let the compiler avoid the intermediate
    // `Value::Struct` clone that would otherwise happen between
    // `IndexGet` and `FieldGet`. The receiver's aggregate is
    // walked by-reference and only the scalar field value is
    // cloned or unboxed.
    /// `floats[dst_f] = receiver.field_name` for a
    /// `Value::Struct` whose named field is a `Value::Float`.
    /// Skips the intermediate `Value::Float` → `UnboxF64`
    /// round-trip that would otherwise happen between
    /// `FieldGet` and a typed arithmetic consumer.
    FieldGetF64 {
        /// Destination float register.
        dst_f: Reg,
        /// Register holding the struct value.
        receiver: Reg,
        /// Const-pool index of the field-name string.
        name_idx: ConstIdx,
    },
    /// `dst = base[index].field_name` — fused indexed field
    /// read. Avoids cloning the inner struct `Arc` that a
    /// separate `IndexGet` + `FieldGet` would produce; reads
    /// the field directly from the array slot by reference.
    IndexedFieldGet {
        /// Destination register.
        dst: Reg,
        /// Register holding the base array.
        base: Reg,
        /// Register holding the index value.
        index: Reg,
        /// Const-pool index of the field-name string.
        name_idx: ConstIdx,
    },
    /// `floats[dst_f] = base[index].field_name` — fused
    /// typed indexed field read. Same `Arc`-clone savings as
    /// `IndexedFieldGet` plus the `Value::Float` unbox into
    /// the float register file happens in one step. This is
    /// nbody's hot-loop primitive.
    IndexedFieldGetF64 {
        /// Destination float register.
        dst_f: Reg,
        /// Register holding the base array.
        base: Reg,
        /// Register holding the index value.
        index: Reg,
        /// Const-pool index of the field-name string.
        name_idx: ConstIdx,
    },
    /// `base[index].field_name = floats[value_f]` — fused
    /// typed indexed field write. Counterpart to
    /// `IndexedFieldGetF64`.
    IndexedFieldSetF64 {
        /// Register holding the base array.
        base: Reg,
        /// Register holding the index value.
        index: Reg,
        /// Const-pool index of the field-name string.
        name_idx: ConstIdx,
        /// Source float register.
        value_f: Reg,
    },

    // ----- Phase 2: offset-resolved typed field ops -----
    //
    // The VM compiler emits these when the receiver's struct
    // type is known at compile time. `__struct` lays out
    // every matching literal in declaration order, so a
    // compile-time `offset` is guaranteed correct and the
    // runtime scan over field names goes away.
    /// `floats[dst_f] = base[index].<struct field at offset>`.
    IndexedFieldGetF64ByOffset {
        /// Destination float register.
        dst_f: Reg,
        /// Register holding the base array.
        base: Reg,
        /// Register holding the index value.
        index: Reg,
        /// Declaration-order offset into the struct's
        /// field vec.
        offset: u16,
    },
    /// `base[index].<struct field at offset> = floats[value_f]`.
    IndexedFieldSetF64ByOffset {
        /// Register holding the base array.
        base: Reg,
        /// Register holding the index value.
        index: Reg,
        /// Declaration-order offset.
        offset: u16,
        /// Source float register.
        value_f: Reg,
    },
    /// Fused compare-and-branch ops. Halve the dispatch
    /// overhead on the common `while i < n { ... }` shape by
    /// combining the compare with the conditional jump into a
    /// single opcode — saves ~one match + one register write
    /// per loop iteration.
    ///
    /// Branch to `target` when `ints[lhs_i] < ints[rhs_i]`.
    BranchIfLtI64 {
        lhs_i: Reg,
        rhs_i: Reg,
        target: InstrIdx,
    },
    /// Branch to `target` when `ints[lhs_i] >= ints[rhs_i]`.
    BranchIfGeI64 {
        lhs_i: Reg,
        rhs_i: Reg,
        target: InstrIdx,
    },
    /// Branch to `target` when `ints[lhs_i] > ints[rhs_i]`.
    /// Used by the inclusive-range for-loop fast path: `for i in a..=b`
    /// exits when `i > b`.
    BranchIfGtI64 {
        lhs_i: Reg,
        rhs_i: Reg,
        target: InstrIdx,
    },
    /// Branch to `target` when `floats[lhs_f] < floats[rhs_f]`.
    BranchIfLtF64 {
        lhs_f: Reg,
        rhs_f: Reg,
        target: InstrIdx,
    },
    /// Branch to `target` when `floats[lhs_f] >= floats[rhs_f]`.
    BranchIfGeF64 {
        lhs_f: Reg,
        rhs_f: Reg,
        target: InstrIdx,
    },

    /// `floats[dst_f] = receiver.<struct field at offset>`.
    FieldGetF64ByOffset {
        /// Destination float register.
        dst_f: Reg,
        /// Register holding the struct value.
        receiver: Reg,
        /// Declaration-order offset.
        offset: u16,
    },

    /// FloatArray-only fused read, statically proven. Skips
    /// the `Value::FloatArray` discriminant check since the
    /// compiler proved `base` holds a flat aggregate via a
    /// preceding `BuildFloatArray`. Drops ~1 branch + one
    /// enum match per iteration on the nbody-shape hot loop.
    FlatGetF64 {
        /// Destination float register.
        dst_f: Reg,
        /// Register holding the `Value::FloatArray`.
        base: Reg,
        /// Register holding the index value.
        index: Reg,
        /// Element stride (f64s per element).
        stride: u16,
        /// Field offset within an element.
        offset: u16,
    },
    /// FloatArray-only fused write, statically proven.
    FlatSetF64 {
        /// Register holding the `Value::FloatArray`.
        base: Reg,
        /// Register holding the index value.
        index: Reg,
        /// Element stride (f64s per element).
        stride: u16,
        /// Field offset within an element.
        offset: u16,
        /// Source float register.
        value_f: Reg,
    },

    // BuildFloatArray (assembles `Value::FloatArray` from a
    // contiguous block of float registers for `[S; N]` literals
    // where `S` has all-f64 fields) lives in the `wide_ops`
    // side-table — see `Op::Wide` and `WideOp::BuildFloatArray`.
}

/// Resolved builtin call pointer cached in [`CacheSlot::builtin_fn`].
/// Same shape as the value the [`Value::Builtin`] variant carries
/// internally; pulled out into a type alias because clippy's
/// `very_complex_type` lint flags the inlined form on the slot.
pub(crate) type BuiltinFnPtr =
    fn(&[crate::value::Value]) -> crate::value::RuntimeResult<crate::value::Value>;

/// One inline-cache slot, one per dispatch-shaped opcode
/// (`Op::Call` / `Op::MethodCall`).
///
/// Hit when the slot's `type_token` matches the receiver / callee's
/// current token; the cached `Global` is used directly, skipping
/// the qualified-key build + `HashMap::get` chain. Miss falls
/// through to the slow path which writes back into the slot.
///
/// `type_token == 0` is the empty sentinel: a fresh chunk starts
/// with all slots zero-initialised, and the dispatch path treats
/// "non-cacheable receiver" (primitives, etc.) the same way by
/// returning a zero token.
///
/// Layout target: 24 B (3 × 8 B) so 16-aligned `Vec<CacheSlot>`
/// fits two slots per cache line. Pre-D8 the `resolved`
/// field stored a full `Option<Global>` (~24 B by itself) for
/// 40 B total per slot; we now cache only the resolved
/// `Arc<FnChunk>` (the dominant hit shape) and let closures /
/// `Value::Native` / `Value::Variant` callees take the slow
/// path on every call.
#[derive(Debug, Clone, Default)]
pub(crate) struct CacheSlot {
    /// Stable identity for the receiver / callee the slot last
    /// resolved against. `0` means empty / non-cacheable.
    pub type_token: u64,
    /// Snapshot of the owning `Vm`'s `globals_generation` when the
    /// slot was populated. The dispatch arm compares this against
    /// the live counter on every hit; a mismatch (i.e. globals
    /// were reassigned since this slot was filled) demotes the
    /// hit to a miss and forces a fresh resolution. `0` is the
    /// empty-slot sentinel and never matches a live counter
    /// (which starts at 1).
    pub generation: u32,
    /// Fast path: when the resolved dispatch target is a
    /// `Value::Builtin`, we cache its raw `call` fn pointer
    /// here so the hit path is a single indirect call, no
    /// `match Global::Value(Value::Builtin { .. })` chain. This
    /// is the steady state for the vast majority of method
    /// dispatches in the bench programs and the wider stdlib.
    /// Mirrors `CPython` 3.11's `LOAD_METHOD_NO_DICT` specialised
    /// opcode storing the resolved `__call__` directly.
    pub builtin_fn: Option<BuiltinFnPtr>,
    /// General path: when the resolved target is a Gossamer
    /// function (`Global::Fn(Arc<FnChunk>)`) — i.e. user code
    /// or stdlib body, not a builtin — its chunk is cached
    /// here. `None` covers both the empty-slot state and any
    /// resolved-but-uncached shape (closures / native / value).
    pub fn_chunk: Option<std::sync::Arc<FnChunk>>,
}

/// Side-table-backed payload for [`Op::Wide`]. Members carry the
/// payload of the rare 6-field ops (`MapIncAt`, `BuildFloatArray`)
/// so the in-line `Op` enum can stay narrow on the hot path.
#[derive(Debug, Clone)]
pub enum WideOp {
    /// `m.inc_at(seq, start, len, by)` — see the original
    /// `Op::MapIncAt` doc; moved to the side table because the
    /// 6-register payload bloated every `Op` slot.
    MapIncAt {
        /// Destination register (post-increment value, `Value::Int`).
        dst: Reg,
        /// Register holding the Map (`Value::Map`).
        map_reg: Reg,
        /// Register holding the seq String (`Value::String`).
        seq_reg: Reg,
        /// Register holding the slice start offset (`Value::Int`).
        start_reg: Reg,
        /// Register holding the slice length (`Value::Int`).
        len_reg: Reg,
        /// Register holding the increment (`Value::Int`).
        by_reg: Reg,
    },
    /// Builds a `Value::FloatArray` from `stride * elem_count`
    /// consecutive `f64` registers starting at `first_f`. Same
    /// shape as the original `Op::BuildFloatArray`; moved here
    /// because the 6-field payload was the other op driving the
    /// in-line `Op` enum to its widest case.
    BuildFloatArray {
        /// Destination value register.
        dst_v: Reg,
        /// Const-pool index of a `Value::String` holding the
        /// element struct's name.
        name_idx: ConstIdx,
        /// Const-pool index of a `Value::Array<Value::String>`
        /// holding the field names in declaration order.
        fields_idx: ConstIdx,
        /// Number of `f64` fields per element.
        stride: u16,
        /// Number of struct elements.
        elem_count: u16,
        /// First float register of the flat data block.
        first_f: Reg,
    },
}

/// Compiled function — the unit of bytecode the VM can call.
#[derive(Debug)]
pub struct FnChunk {
    /// Source-level name (useful in diagnostics).
    pub name: String,
    /// Number of parameters the function takes.
    pub arity: u16,
    /// Total Value register file size reserved per call.
    pub register_count: u16,
    /// Unboxed `f64` register file size — Phase 1.
    pub float_count: u16,
    /// Unboxed `i64` register file size — Phase 1.
    pub int_count: u16,
    /// Linear instruction stream.
    pub instrs: Vec<Op>,
    /// Side-table for op payloads that don't fit in the in-line
    /// `Op` variant width without forcing every other op to
    /// pay the worst-case slot. Indexed by `Op::Wide(idx)`. The
    /// dispatch loop takes one extra deref through this Vec for
    /// the rare wide ops, in exchange for keeping the per-op
    /// memcpy on the hot path narrow.
    pub wide_ops: Vec<WideOp>,
    /// Interned constants referenced by `LoadConst`.
    pub consts: Vec<Value>,
    /// Raw `f64` constants referenced by `LoadConstF64`. Kept
    /// separate from `consts` so the `Op` enum can stay narrow
    /// (the dispatch loop copies each op per instruction).
    pub f64_consts: Vec<f64>,
    /// Raw `i64` constants referenced by `LoadConstI64`.
    pub i64_consts: Vec<i64>,
    /// Global names referenced by `LoadGlobal`.
    pub globals: Vec<String>,
    /// HIR expressions kept alongside the bytecode for
    /// `Op::EvalDeferred` — expression kinds the VM compiler
    /// doesn't yet native-lower. Indexed by `EvalDeferred::expr_idx`.
    pub deferred_exprs: Vec<gossamer_hir::HirExpr>,
    /// Binding names paired with `deferred_env_regs` by the
    /// matching index.
    pub deferred_envs: Vec<Vec<String>>,
    /// Registers exposed to each delegated expression. The VM
    /// reads these into the walker's env before the call and
    /// writes them back afterwards so in-place mutations through
    /// the walker flow back into the VM's register file.
    pub deferred_env_regs: Vec<Vec<Reg>>,
    /// Number of inline-cache slots this chunk needs (`Op::Call`
    /// / `Op::MethodCall` sites). The actual `Vec<CacheSlot>` lives
    /// per-`Vm` inside [`crate::vm::ChunkState`], not on the chunk —
    /// goroutines spawned from a parent VM each get their own
    /// `ChunkState` so cache writes don't bounce cache lines across
    /// CPUs. `FnChunk` stays purely-immutable and `Sync`.
    pub call_cache_count: u16,
    /// Number of adaptive-arith cache slots this chunk needs
    /// (`Op::AddInt` / `Op::SubInt` / etc. sites). Same per-`Vm`
    /// ownership story as [`Self::call_cache_count`].
    pub arith_cache_count: u16,
    /// Number of field-access cache slots this chunk needs
    /// (`Op::FieldGet` sites). PEP 659-style per-instruction
    /// inline caching for struct field reads.
    pub field_cache_count: u16,
}

/// One adaptive-arith inline-cache slot. Tier C2 of the interp
/// wow plan — held inside [`crate::vm::ChunkState`].
#[derive(Debug, Default)]
pub(crate) struct ArithCacheSlot {
    /// Observed operand shape, encoded as one of the `ARITH_*`
    /// constants. `Cell<u8>` because the slot lives in per-`Vm`
    /// state; only the owning thread mutates it.
    pub(crate) shape: std::cell::Cell<u8>,
}

/// PEP 659-style inline cache for `Op::FieldGet`. Records the
/// last observed struct-name pointer + the offset its fields
/// list resolved to. On hit, the dispatcher reads the field by
/// offset directly; on miss, it refills the slot.
#[derive(Debug, Default)]
pub(crate) struct FieldCacheSlot {
    /// Stable interned-name pointer of the receiver struct
    /// (`intern_type_name(name).as_ptr() as u64`). `0` means
    /// empty / non-cacheable receiver.
    pub(crate) type_token: std::cell::Cell<u64>,
    /// Offset of the named field within the struct's fields
    /// vector, valid only when `type_token != 0`.
    pub(crate) offset: std::cell::Cell<u16>,
}

/// Sentinel for an arith cache slot that has not yet observed an
/// operand pair. Forces the dispatcher into the slow observe-and-
/// specialise path on the first call from the site.
pub(crate) const ARITH_UNKNOWN: u8 = 0;
/// Slot specialised on `(Value::Int, Value::Int)`. The dispatcher
/// reads the integers directly and emits a wrapping op without a
/// discriminant match.
pub(crate) const ARITH_INT_INT: u8 = 1;
/// Slot specialised on `(Value::Float, Value::Float)`.
pub(crate) const ARITH_FLOAT_FLOAT: u8 = 2;
/// Slot specialised on `(Value::String, Value::String)` — only
/// reached for `Op::AddInt` (string concatenation). The other
/// arith ops never set this shape; their observers degrade to
/// polymorphic when they see strings.
pub(crate) const ARITH_STRING_STRING: u8 = 3;
/// Slot has seen multiple incompatible shapes (e.g. an
/// `(Int, Float)` after specialising on `(Int, Int)`). Future
/// dispatches go straight through the generic helper without
/// trying to re-specialise.
pub(crate) const ARITH_POLYMORPHIC: u8 = 255;

/// Number of times a chunk must be entered before it triggers the
/// deferred JIT compile. Conservative enough that a one-shot
/// `gos run hello.gos` never trips it (its `main` fn runs exactly
/// once); aggressive enough that nbody-style inner loops trip
/// almost immediately. The plan suggested 10K back-edges; for the
/// per-call-entry counter used here a much lower value is
/// equivalent because hot loops fan calls out by orders of
/// magnitude faster than they accrue back-edges.
pub(crate) const HOT_THRESHOLD: i32 = 100;

/// Sentinel that the `hot_counter` is initialised to when the JIT
/// is permanently disabled at chunk construction time. The Cell
/// can never realistically be decremented past `i32::MIN + 1`, so
/// using `i32::MAX` as a "never trips" marker is safe.
pub(crate) const HOT_DISABLED: i32 = i32::MAX;

impl FnChunk {
    /// Produces a `Arc<Self>` so multiple callers share the same chunk.
    /// `FnChunk` carries `RefCell` and `Cell` interior mutability that
    /// makes it `!Sync`; the VM is single-threaded today and an `Arc`
    /// is the right shape for shared-ownership semantics, so the
    /// `arc_with_non_send_sync` lint is suppressed at this single
    /// construction site.
    #[must_use]
    #[allow(clippy::arc_with_non_send_sync)]
    pub fn into_shared(self) -> Arc<Self> {
        Arc::new(self)
    }
}
