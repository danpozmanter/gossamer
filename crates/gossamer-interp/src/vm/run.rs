#![allow(
    clippy::too_many_lines,
    reason = "VM dispatch loop - see vm/run.rs roadmap for arm-group decomp"
)]
use super::*;

impl Vm {
    pub(crate) fn run(
        &self,
        chunk: &FnChunk,
        state: &ChunkState,
        args: Vec<Value>,
    ) -> RuntimeResult<Value> {
        if chunk.arity as usize != args.len() {
            return Err(RuntimeError::Arity {
                expected: chunk.arity as usize,
                found: args.len(),
            });
        }
        // Pool guard: takes the three register-file `Vec`s on
        // entry and returns them on Drop, so `?` and early
        // returns inside the dispatch loop don't leak buffers.
        let mut guard = FrameGuard::take(
            &self.pool,
            chunk.register_count as usize,
            chunk.float_count as usize,
            chunk.int_count as usize,
        );
        let registers = &mut guard.registers;
        let floats = &mut guard.floats;
        let ints = &mut guard.ints;
        // Drain (not consume) so the empty Vec can go back to
        // the pool's `args` free list - most arg Vecs are
        // pool-borrowed in `Op::Call`, and reclaiming them here
        // closes the loop without an extra allocation per call.
        let mut args = args;
        for (i, arg) in args.drain(..).enumerate() {
            registers[i] = arg;
        }
        self.pool.borrow_mut().give_args(args);
        // Write-back cell protocol for `&mut Vec<T>` / `&mut [T]`
        // parameters: unwrap each incoming `MutCell` into its param
        // register and remember the cell so every return path below
        // publishes the final register value back to the caller.
        let mut ref_cells: Vec<(usize, Arc<parking_lot::Mutex<Value>>)> = Vec::new();
        if !chunk.mut_ref_params.is_empty() {
            for &idx in &chunk.mut_ref_params {
                let slot = idx as usize;
                if let Value::MutCell(cell) = &registers[slot] {
                    let cell = Arc::clone(cell);
                    // Move the inner value out of the cell into the param
                    // register. With a `CellNewMove`-created cell the caller's
                    // home register was already emptied, so this keeps the
                    // value's refcount at one and the first field write mutates
                    // in place. `publish_ref_cells` repopulates the cell on
                    // return.
                    registers[slot] = std::mem::replace(&mut *cell.lock(), Value::Unit);
                    ref_cells.push((slot, cell));
                }
            }
        }
        crate::profile::enter_frame();
        #[cfg(feature = "profile")]
        struct ProfDump(&'static str);
        #[cfg(feature = "profile")]
        impl Drop for ProfDump {
            fn drop(&mut self) {
                if self.0 == "main" && std::env::var_os("GOS_VM_PROFILE").is_some() {
                    eprint!("{}", crate::profile::dump_report());
                }
            }
        }
        #[cfg(feature = "profile")]
        let _prof_dump = ProfDump(chunk.name);
        let mut pc: u32 = 0;
        #[cfg(feature = "fuel")]
        let mut prev_pc: u32 = 0;
        let instrs: &[Op] = &chunk.instrs;
        let instr_count = instrs.len();
        loop {
            // Execution budget: a backward (or self) jump is a loop iteration.
            // Counting them bounds total iterations so an unbounded loop aborts
            // cleanly instead of hanging. Compiled in only under the `fuel`
            // feature (the wasm playground); native builds carry none of this.
            #[cfg(feature = "fuel")]
            {
                if pc <= prev_pc && crate::fuel::consume() {
                    return Err(RuntimeError::FuelExhausted);
                }
                prev_pc = pc;
            }
            // SAFETY: every chunk emitted by `compile.rs` ends
            // with a `Return` / `ReturnUnit`, and every jump /
            // branch target is computed from the same emit-
            // counter that placed the op - so `pc` can never
            // exceed `instr_count` at this point. We keep a
            // `debug_assert!` so a corrupted chunk fails loudly
            // in debug builds, but skip the runtime branch in
            // release. `Op` is `Copy`, so dereferencing gives
            // us a by-value copy of the enum for destructuring
            // without invoking `<Op as Clone>::clone`.
            debug_assert!((pc as usize) < instr_count, "fell off end of bytecode");
            let _ = instr_count;
            let op = unsafe { *instrs.get_unchecked(pc as usize) };
            crate::profile::record_op(op);
            pc += 1;
            match op {
                Op::LoadConst { dst, idx } => {
                    registers[dst as usize] = chunk.consts[idx as usize].clone();
                }
                Op::LoadGlobal { dst, idx } => {
                    let name: &str = &chunk.globals[idx as usize];
                    let value = match self.lookup_global_ref(name) {
                        Some(Global::Value(v)) => v.clone(),
                        Some(Global::MutStatic(cell)) => cell.lock().clone(),
                        Some(Global::Fn(_)) => Value::String(SmolStr::from(name)),
                        None => return Err(RuntimeError::UnresolvedName(name.to_string())),
                    };
                    registers[dst as usize] = value;
                }
                Op::StoreStatic { name_idx, src } => {
                    let name: &str = &chunk.globals[name_idx as usize];
                    match self.lookup_global_ref(name) {
                        Some(Global::MutStatic(cell)) => {
                            *cell.lock() = registers[src as usize].clone();
                        }
                        _ => return Err(RuntimeError::UnresolvedName(name.to_string())),
                    }
                }
                Op::Move { dst, src } => {
                    registers[dst as usize] = registers[src as usize].clone();
                }
                Op::MoveConsume { dst, src } => {
                    // Hand the value over instead of cloning: the source
                    // local is read exactly once at this point (proven by
                    // `compile::consume`), so the emptied slot is never read.
                    let v = std::mem::replace(&mut registers[src as usize], Value::Void);
                    registers[dst as usize] = v;
                }
                Op::Deref { dst, src } => {
                    let v = registers[src as usize].clone();
                    let resolved = if let Value::Struct(inner) = &v {
                        if inner.name == "__Cell" {
                            let mut set_id: u64 = 0;
                            let mut flag_name = String::new();
                            for (ident, val) in &inner.fields {
                                if (*ident) == "__set_id"
                                    && let Value::Int(n) = val
                                {
                                    set_id = *n as u64;
                                }
                                if (*ident) == "__flag_name"
                                    && let Value::String(s) = val
                                {
                                    flag_name = s.as_str().to_string();
                                }
                            }
                            crate::builtins::resolve_cell(set_id, &flag_name)
                                .unwrap_or_else(|| v.clone())
                        } else {
                            v
                        }
                    } else {
                        v
                    };
                    registers[dst as usize] = resolved;
                }
                Op::AddInt {
                    dst,
                    lhs,
                    rhs,
                    cache_idx,
                } => {
                    let a = &registers[lhs as usize];
                    let b = &registers[rhs as usize];
                    let shape = state.arith_caches[cache_idx as usize].shape.get();
                    registers[dst as usize] = adaptive_add(state, cache_idx, shape, a, b)?;
                }
                Op::SubInt {
                    dst,
                    lhs,
                    rhs,
                    cache_idx,
                } => {
                    let a = &registers[lhs as usize];
                    let b = &registers[rhs as usize];
                    let shape = state.arith_caches[cache_idx as usize].shape.get();
                    registers[dst as usize] = adaptive_arith(
                        state,
                        cache_idx,
                        shape,
                        a,
                        b,
                        i64::wrapping_sub,
                        |x, y| x - y,
                        "subtraction",
                    )?;
                }
                Op::MulInt {
                    dst,
                    lhs,
                    rhs,
                    cache_idx,
                } => {
                    let a = &registers[lhs as usize];
                    let b = &registers[rhs as usize];
                    let shape = state.arith_caches[cache_idx as usize].shape.get();
                    registers[dst as usize] = adaptive_arith(
                        state,
                        cache_idx,
                        shape,
                        a,
                        b,
                        i64::wrapping_mul,
                        |x, y| x * y,
                        "multiplication",
                    )?;
                }
                Op::DivInt {
                    dst,
                    lhs,
                    rhs,
                    cache_idx,
                } => {
                    let a = &registers[lhs as usize];
                    let b = &registers[rhs as usize];
                    let shape = state.arith_caches[cache_idx as usize].shape.get();
                    registers[dst as usize] = adaptive_div(state, cache_idx, shape, a, b)?;
                }
                Op::RemInt {
                    dst,
                    lhs,
                    rhs,
                    cache_idx,
                } => {
                    let a = &registers[lhs as usize];
                    let b = &registers[rhs as usize];
                    let shape = state.arith_caches[cache_idx as usize].shape.get();
                    registers[dst as usize] = adaptive_rem(state, cache_idx, shape, a, b)?;
                }
                Op::Neg { dst, operand } => {
                    registers[dst as usize] = neg(&registers[operand as usize])?;
                }
                Op::Not { dst, operand } => {
                    registers[dst as usize] = not(&registers[operand as usize])?;
                }
                Op::Eq { dst, lhs, rhs } => {
                    // `values_equal` already auto-derefs `__Cell`.
                    registers[dst as usize] = Value::Bool(values_equal(
                        &registers[lhs as usize],
                        &registers[rhs as usize],
                    ));
                }
                Op::Ne { dst, lhs, rhs } => {
                    registers[dst as usize] = Value::Bool(!values_equal(
                        &registers[lhs as usize],
                        &registers[rhs as usize],
                    ));
                }
                Op::Lt { dst, lhs, rhs } => {
                    registers[dst as usize] = compare(
                        &registers[lhs as usize],
                        &registers[rhs as usize],
                        std::cmp::Ordering::Less,
                        false,
                    )?;
                }
                Op::Le { dst, lhs, rhs } => {
                    registers[dst as usize] = compare(
                        &registers[lhs as usize],
                        &registers[rhs as usize],
                        std::cmp::Ordering::Less,
                        true,
                    )?;
                }
                Op::Gt { dst, lhs, rhs } => {
                    registers[dst as usize] = compare(
                        &registers[lhs as usize],
                        &registers[rhs as usize],
                        std::cmp::Ordering::Greater,
                        false,
                    )?;
                }
                Op::Ge { dst, lhs, rhs } => {
                    registers[dst as usize] = compare(
                        &registers[lhs as usize],
                        &registers[rhs as usize],
                        std::cmp::Ordering::Greater,
                        true,
                    )?;
                }
                Op::Jump { target } => pc = target,
                Op::BranchIf { cond, target } => {
                    if truthy(&registers[cond as usize])? {
                        pc = target;
                    }
                }
                Op::BranchIfNot { cond, target } => {
                    if !truthy(&registers[cond as usize])? {
                        pc = target;
                    }
                }
                Op::Call {
                    dst,
                    callee,
                    args,
                    argc,
                    cache_idx,
                    may_have_cells,
                } => {
                    let argc_usz = argc as usize;
                    let mut arg_values = self.pool.borrow_mut().take_args(argc_usz);
                    for i in 0..argc_usz {
                        // Move the argument out of its scratch slot rather
                        // than cloning: the contiguous `args` region is
                        // written once by the call's arg setup and read
                        // only here, so handing the value to the callee
                        // gives it unique ownership (the basis for
                        // move-on-last-use draining the input) and saves a
                        // refcount bump per argument. A `&mut` write-back
                        // cell lives in a separate register that `CellTake`
                        // reads after the call, so emptying the slot is
                        // safe.
                        //
                        // 0.7.0 flag::Cell auto-deref at the call boundary:
                        // `f(flags.output)` passes the current backing
                        // value instead of the `__Cell` handle. Matches
                        // Rust's `Deref` coercion on fn-arg sites. Skipped
                        // when the compiler proved every argument is scalar.
                        let raw = std::mem::replace(&mut registers[args as usize + i], Value::Void);
                        let v = if may_have_cells {
                            auto_deref_cell(&raw).unwrap_or(raw)
                        } else {
                            raw
                        };
                        arg_values.push(v);
                    }
                    let callee_val = &registers[callee as usize];
                    // Inline-cache probe. The slot is keyed by the
                    // *callee* identity (the resolved name for a
                    // `Value::String(SmolStr::from("foo"))` callee). Cache hit
                    // skips the `self.globals.get(name)` HashMap
                    // probe - typically the dominant cost in tight
                    // loops calling small helper functions.
                    let token = call_token(callee_val);
                    let live_generation = self.globals_generation();
                    // Two-tier IC probe (same shape as MethodCall): a
                    // resolved builtin is returned as a raw `fn` pointer so
                    // the hit path calls it directly - no per-call
                    // `Arc<BuiltinInner>` allocation just to thread it
                    // through `apply`.
                    type BuiltinFn = fn(&[Value]) -> RuntimeResult<Value>;
                    let (cached_builtin, cached): (Option<BuiltinFn>, Option<Global>) =
                        if token != 0 {
                            // Read-only borrow on the IC hit path; the
                            // fill (miss) path below takes borrow_mut.
                            // Splitting avoids serialising every cached
                            // hit against any concurrent borrow on the
                            // same RefCell.
                            let cache = state.call_caches.borrow();
                            let slot = &cache[cache_idx as usize];
                            if slot.type_token == token && slot.generation == live_generation {
                                if let Some(call_fn) = slot.builtin_fn {
                                    (Some(call_fn), None)
                                } else {
                                    (
                                        None,
                                        slot.fn_chunk.as_ref().map(|c| Global::Fn(Arc::clone(c))),
                                    )
                                }
                            } else {
                                (None, None)
                            }
                        } else {
                            (None, None)
                        };
                    let result = if let Some(call_fn) = cached_builtin {
                        // Hottest hit: direct fn-ptr call. Unwrap any `&mut`
                        // write-back cells (free builtins take aggregates by
                        // value) and return the pooled arg buffer - both
                        // things `apply`'s builtin arm does, minus the alloc.
                        let call_args = crate::value::unwrap_mut_cells(arg_values);
                        let r = call_fn(&call_args)?;
                        self.pool.borrow_mut().give_args(call_args);
                        r
                    } else if let Some(g) = cached {
                        self.apply(g, arg_values)?
                    } else if token != 0 {
                        // Miss: do the full dispatch and write back.
                        let resolved_global = match callee_val {
                            Value::String(name) => self.lookup_global(name.as_str()),
                            _ => None,
                        };
                        if let Some(ref g) = resolved_global {
                            let mut cache = state.call_caches.borrow_mut();
                            cache[cache_idx as usize] = fill_cache_slot(token, live_generation, g);
                        }
                        match resolved_global {
                            Some(g) => self.apply(g, arg_values)?,
                            None => self.dispatch_call(callee_val, arg_values)?,
                        }
                    } else {
                        // Non-cacheable callee shape (Builtin,
                        // Closure, Native, …): straight to the
                        // existing slow-path dispatcher.
                        self.dispatch_call(callee_val, arg_values)?
                    };
                    registers[dst as usize] = result;
                }
                Op::Return { value } => {
                    // Capture the return value before publishing: a function
                    // may return one of its own `&mut` params, and publishing
                    // moves that register's value into the cell.
                    let ret = registers[value as usize].clone();
                    publish_ref_cells(&ref_cells, registers);
                    return Ok(ret);
                }
                Op::ReturnUnit => {
                    publish_ref_cells(&ref_cells, registers);
                    return Ok(Value::Unit);
                }
                Op::ClearRegs { start, count } => {
                    let from = start as usize;
                    let to = from + count as usize;
                    for slot in &mut registers[from..to] {
                        *slot = Value::Void;
                    }
                }
                Op::Panic { msg } => {
                    let message = match &chunk.consts[msg as usize] {
                        Value::String(s) => s.as_str().to_string(),
                        _ => "panic".to_string(),
                    };
                    return Err(RuntimeError::Panic(message));
                }
                Op::MethodCall {
                    dst,
                    receiver,
                    name_idx,
                    args,
                    argc,
                    cache_idx,
                } => {
                    // Inline-cache probe. We key the slot on the
                    // *receiver* type (interned struct-name pointer
                    // or a per-variant constant). Hit returns the
                    // resolved `Global` directly, skipping the
                    // qualified-key build + double `HashMap::get`
                    // lookup chain that dominates tight per-element
                    // method call loops.
                    let name = &chunk.globals[name_idx as usize];
                    let argc_usz = argc as usize;
                    let total = argc_usz + 1;
                    let recv_token = type_token(&registers[receiver as usize]);
                    let live_generation = self.globals_generation();
                    // Two-tier IC probe. The hottest hit is the
                    // builtin fn-pointer fast path: the slot's
                    // `builtin_fn` is the resolved
                    // `fn(&[Value]) -> RuntimeResult<Value>`,
                    // called directly with no `match Global { … }`
                    // chain. Closures / JIT-promoted bodies fall to
                    // the slower `resolved` field.
                    type BuiltinFn = fn(&[Value]) -> RuntimeResult<Value>;
                    let (cached_builtin, cached_general): (Option<BuiltinFn>, Option<Global>) =
                        if recv_token != 0 {
                            // Read-only borrow on the IC hit path
                            // (>99 % of calls in steady state).
                            // Miss-and-fill below takes borrow_mut.
                            let cache = state.call_caches.borrow();
                            let slot = &cache[cache_idx as usize];
                            if slot.type_token == recv_token && slot.generation == live_generation {
                                let general =
                                    slot.fn_chunk.as_ref().map(|c| Global::Fn(Arc::clone(c)));
                                (slot.builtin_fn, general)
                            } else {
                                (None, None)
                            }
                        } else {
                            (None, None)
                        };
                    let cached = cached_general;

                    // Materialise call args. Stack buffer for argc
                    // ≤ 7 (recv + 7 args fits 8 slots).
                    const SMALL: usize = 8;
                    let result = if total <= SMALL {
                        let mut buf: [Value; SMALL] = [
                            Value::Void,
                            Value::Void,
                            Value::Void,
                            Value::Void,
                            Value::Void,
                            Value::Void,
                            Value::Void,
                            Value::Void,
                        ];
                        buf[0] = registers[receiver as usize].clone();
                        for i in 0..argc_usz {
                            // 0.7.0 flag::Cell auto-deref at the
                            // call boundary - same rule as `Op::Call`.
                            let raw = registers[args as usize + i].clone();
                            buf[i + 1] = auto_deref_cell(&raw).unwrap_or(raw);
                        }
                        if let Some(call_fn) = cached_builtin {
                            // Hottest hit path: direct fn ptr call,
                            // no enum match.
                            call_fn(&buf[..total])?
                        } else if let Some(g) = cached {
                            // Cached non-builtin (closure / JIT).
                            let v: Vec<Value> = buf[..total].to_vec();
                            self.apply(g, v)?
                        } else {
                            // Miss: full resolution + cache fill.
                            let r = qualified_key(&buf[0], name)
                                .and_then(|qual: &str| self.lookup_global(qual))
                                .or_else(|| self.lookup_global(name.as_str()));
                            if recv_token != 0 {
                                if let Some(ref g) = r {
                                    let mut cache = state.call_caches.borrow_mut();
                                    cache[cache_idx as usize] =
                                        fill_cache_slot(recv_token, live_generation, g);
                                }
                            }
                            match r {
                                Some(Global::Value(Value::Builtin(builtin_inner))) => {
                                    (builtin_inner.call)(&buf[..total])?
                                }
                                Some(g) => {
                                    let v: Vec<Value> = buf[..total].to_vec();
                                    self.apply(g, v)?
                                }
                                None => {
                                    return Err(RuntimeError::UnresolvedName(name.clone()));
                                }
                            }
                        }
                    } else {
                        let recv = registers[receiver as usize].clone();
                        let mut call_args: Vec<Value> = Vec::with_capacity(total);
                        call_args.push(recv);
                        for i in 0..argc_usz {
                            // 0.7.0 flag::Cell auto-deref at the
                            // call boundary - same rule as `Op::Call`.
                            let raw = registers[args as usize + i].clone();
                            call_args.push(auto_deref_cell(&raw).unwrap_or(raw));
                        }
                        if let Some(call_fn) = cached_builtin {
                            call_fn(&call_args)?
                        } else if let Some(g) = cached {
                            self.apply(g, call_args)?
                        } else {
                            let r = qualified_key(&call_args[0], name)
                                .and_then(|qual: &str| self.lookup_global(qual))
                                .or_else(|| self.lookup_global(name.as_str()));
                            if recv_token != 0 {
                                if let Some(ref g) = r {
                                    let mut cache = state.call_caches.borrow_mut();
                                    cache[cache_idx as usize] =
                                        fill_cache_slot(recv_token, live_generation, g);
                                }
                            }
                            match r {
                                Some(Global::Value(Value::Builtin(builtin_inner))) => {
                                    (builtin_inner.call)(&call_args)?
                                }
                                Some(g) => self.apply(g, call_args)?,
                                None => {
                                    return Err(RuntimeError::UnresolvedName(name.clone()));
                                }
                            }
                        }
                    };
                    registers[dst as usize] = result;
                }
                Op::StreamWriteByte {
                    dst,
                    stream_reg,
                    byte_reg,
                } => {
                    // Super-instruction for `<stream>.write_byte(<b>)`.
                    // Hot path: receiver is a `Value::Struct{name="Stream",
                    // fields=[("fd", Int(fd))]}`, byte is a
                    // `Value::Int`. Inline the same work
                    // `builtins::builtin_stream_write_byte` does
                    // but without going through the
                    // MethodCall + IC + Vec-args path.
                    let recv = &registers[stream_reg as usize];
                    let byte_val = &registers[byte_reg as usize];
                    let stream_match = matches!(
                        recv,
                        Value::Struct(inner) if inner.name == "Stream"
                    );
                    let byte_match = matches!(byte_val, Value::Int(_));
                    if stream_match && byte_match {
                        let fd = match recv {
                            Value::Struct(inner) => {
                                let mut fd = 1i64;
                                for (n, v) in &inner.fields {
                                    if (*n) == "fd" {
                                        if let Value::Int(f) = v {
                                            fd = *f;
                                            break;
                                        }
                                    }
                                }
                                fd
                            }
                            _ => 1,
                        };
                        let b = match byte_val {
                            Value::Int(n) => *n,
                            _ => unreachable!(),
                        };
                        crate::builtins::stream_write_one_byte(fd, b);
                        registers[dst as usize] = Value::Unit;
                    } else {
                        // Fallback: full method dispatch through
                        // the regular qualified-key path. Keeps
                        // the op correct for any user-defined
                        // `write_byte` method on a non-Stream
                        // receiver, at the cost of one extra
                        // hash lookup per call (uncached for the
                        // miss case, since this op doesn't carry
                        // an IC slot).
                        let recv_clone = recv.clone();
                        let byte_clone = byte_val.clone();
                        let resolved = match &recv_clone {
                            Value::Struct(_) | Value::Channel(_) => {
                                qualified_key(&recv_clone, "write_byte")
                                    .and_then(|q| self.lookup_global(q))
                            }
                            _ => None,
                        }
                        .or_else(|| self.lookup_global("write_byte"));
                        let args = vec![recv_clone, byte_clone];
                        let result = match resolved {
                            Some(Global::Value(Value::Builtin(builtin_inner))) => {
                                (builtin_inner.call)(&args)?
                            }
                            Some(g) => self.apply(g, args)?,
                            None => {
                                return Err(RuntimeError::UnresolvedName("write_byte".to_string()));
                            }
                        };
                        registers[dst as usize] = result;
                    }
                }
                Op::U8VecSetByte {
                    dst,
                    u8vec_reg,
                    idx_reg,
                    byte_reg,
                } => {
                    // Super-instruction for `<u8vec>.set_byte(<idx>, <byte>)`.
                    // Same fast-path / fallback shape as
                    // [`Op::StreamWriteByte`]. The inline helper
                    // returns `false` when the handle has been
                    // dropped (extremely rare - U8Vec is held by
                    // the user-side struct for its full lifetime),
                    // letting us fall through to the generic
                    // dispatch path for correctness.
                    let recv = &registers[u8vec_reg as usize];
                    let idx_val = &registers[idx_reg as usize];
                    let byte_val = &registers[byte_reg as usize];
                    let fast = matches!(
                        recv,
                        Value::Struct(inner) if inner.name == "U8Vec"
                    ) && matches!(idx_val, Value::Int(_))
                        && matches!(byte_val, Value::Int(_));
                    if fast {
                        let handle = match recv {
                            Value::Struct(inner) => {
                                let mut h = 0i64;
                                for (n, v) in &inner.fields {
                                    if (*n) == "handle" {
                                        if let Value::Int(x) = v {
                                            h = *x;
                                            break;
                                        }
                                    }
                                }
                                h
                            }
                            _ => unreachable!(),
                        };
                        let idx = match idx_val {
                            Value::Int(n) => *n,
                            _ => unreachable!(),
                        };
                        let byte = match byte_val {
                            Value::Int(n) => *n,
                            _ => unreachable!(),
                        };
                        if crate::builtins::u8vec_set_byte_inline(handle, idx, byte) {
                            registers[dst as usize] = Value::Unit;
                            continue;
                        }
                    }
                    // Fallback: same generic-dispatch shape as
                    // `StreamWriteByte`'s miss path.
                    let recv_clone = recv.clone();
                    let idx_clone = idx_val.clone();
                    let byte_clone = byte_val.clone();
                    let resolved = match &recv_clone {
                        Value::Struct(_) => qualified_key(&recv_clone, "set_byte")
                            .and_then(|q| self.lookup_global(q)),
                        _ => None,
                    }
                    .or_else(|| self.lookup_global("set_byte"));
                    let args = vec![recv_clone, idx_clone, byte_clone];
                    let result = match resolved {
                        Some(Global::Value(Value::Builtin(builtin_inner))) => {
                            (builtin_inner.call)(&args)?
                        }
                        Some(g) => self.apply(g, args)?,
                        None => {
                            return Err(RuntimeError::UnresolvedName("set_byte".to_string()));
                        }
                    };
                    registers[dst as usize] = result;
                }
                Op::U8VecGetByte {
                    dst_i,
                    u8vec_reg,
                    idx_reg,
                } => {
                    let recv = &registers[u8vec_reg as usize];
                    let idx_val = &registers[idx_reg as usize];
                    let fast = matches!(
                        recv,
                        Value::Struct(inner) if inner.name == "U8Vec"
                    ) && matches!(idx_val, Value::Int(_));
                    if fast {
                        let handle = match recv {
                            Value::Struct(inner) => {
                                let mut h = 0i64;
                                for (n, v) in &inner.fields {
                                    if (*n) == "handle" {
                                        if let Value::Int(x) = v {
                                            h = *x;
                                            break;
                                        }
                                    }
                                }
                                h
                            }
                            _ => unreachable!(),
                        };
                        let idx = match idx_val {
                            Value::Int(n) => *n,
                            _ => unreachable!(),
                        };
                        if let Some(b) = crate::builtins::u8vec_get_byte_inline(handle, idx) {
                            // SAFETY: `dst_i` is a compile-allocated
                            // i64 register slot.
                            unsafe {
                                *ints.get_unchecked_mut(dst_i as usize) = b;
                            }
                            continue;
                        }
                    }
                    // Fallback: dispatch through the generic
                    // `get_byte` builtin, then unbox the resulting
                    // `Value::Int` into the typed register.
                    let recv_clone = recv.clone();
                    let idx_clone = idx_val.clone();
                    let resolved = match &recv_clone {
                        Value::Struct(_) => qualified_key(&recv_clone, "get_byte")
                            .and_then(|q| self.lookup_global(q)),
                        _ => None,
                    }
                    .or_else(|| self.lookup_global("get_byte"));
                    let args = vec![recv_clone, idx_clone];
                    let result = match resolved {
                        Some(Global::Value(Value::Builtin(builtin_inner))) => {
                            (builtin_inner.call)(&args)?
                        }
                        Some(g) => self.apply(g, args)?,
                        None => {
                            return Err(RuntimeError::UnresolvedName("get_byte".to_string()));
                        }
                    };
                    let n = match result {
                        Value::Int(n) => n,
                        _ => 0,
                    };
                    unsafe {
                        *ints.get_unchecked_mut(dst_i as usize) = n;
                    }
                }
                Op::StrSubstring {
                    dst,
                    recv_reg,
                    start_reg,
                    end_reg,
                } => {
                    // Fast path: string receiver + integer bounds. The
                    // borrow of `registers` ends with the match, since it
                    // yields an owned `SmolStr`, so the destination write
                    // below does not alias it.
                    let bounds =
                        match (&registers[start_reg as usize], &registers[end_reg as usize]) {
                            (Value::Int(a), Value::Int(b)) => Some((*a, *b)),
                            _ => None,
                        };
                    let fast = match (&registers[recv_reg as usize], bounds) {
                        (Value::String(s), Some((a, b))) => {
                            Some(crate::builtins::str_substring_inline(s.as_str(), a, b))
                        }
                        _ => None,
                    };
                    if let Some(out) = fast {
                        registers[dst as usize] = Value::String(out);
                        continue;
                    }
                    // Fallback: generic `substring` dispatch, same shape as
                    // `U8VecGetByte`'s miss path.
                    let recv_clone = registers[recv_reg as usize].clone();
                    let start_clone = registers[start_reg as usize].clone();
                    let end_clone = registers[end_reg as usize].clone();
                    let resolved = match &recv_clone {
                        Value::Struct(_) => qualified_key(&recv_clone, "substring")
                            .and_then(|q| self.lookup_global(q)),
                        _ => None,
                    }
                    .or_else(|| self.lookup_global("substring"));
                    let args = vec![recv_clone, start_clone, end_clone];
                    let result = match resolved {
                        Some(Global::Value(Value::Builtin(builtin_inner))) => {
                            (builtin_inner.call)(&args)?
                        }
                        Some(g) => self.apply(g, args)?,
                        None => {
                            return Err(RuntimeError::UnresolvedName("substring".to_string()));
                        }
                    };
                    registers[dst as usize] = result;
                }
                Op::MapIncMethod {
                    dst,
                    map_reg,
                    key_reg,
                    by_reg,
                } => {
                    let by = match &registers[by_reg as usize] {
                        Value::Int(n) => *n,
                        _ => 1,
                    };
                    // Each arm holds the map lock only within the match and
                    // yields an owned `Value::Int`, so the borrow of
                    // `registers` is released before the destination write.
                    let result = match &registers[map_reg as usize] {
                        Value::StrIntMap(map) => {
                            if let Value::String(k) = &registers[key_reg as usize] {
                                let mut guard = map.lock();
                                let new_val = if let Some(slot) = guard.get_mut(k) {
                                    *slot += by;
                                    *slot
                                } else {
                                    guard.insert(k.clone(), by);
                                    by
                                };
                                Some(Value::Int(new_val))
                            } else {
                                None
                            }
                        }
                        Value::IntMap(map) => {
                            if let Value::Int(k) = &registers[key_reg as usize] {
                                let mut guard = map.lock();
                                let new_val = guard.get(k).copied().unwrap_or(0) + by;
                                guard.insert(*k, new_val);
                                Some(Value::Int(new_val))
                            } else {
                                None
                            }
                        }
                        Value::Map(map) => {
                            let key = MapKey::from_value(&registers[key_reg as usize]);
                            let mut guard = map.lock();
                            let new_val = match guard.get(&key) {
                                Some(Value::Int(v)) => v + by,
                                _ => by,
                            };
                            guard.insert(key, Value::Int(new_val));
                            Some(Value::Int(new_val))
                        }
                        _ => None,
                    };
                    if let Some(v) = result {
                        registers[dst as usize] = v;
                        continue;
                    }
                    // Fallback: generic `inc` dispatch (non-map receiver or
                    // key-type mismatch), same shape as `StrSubstring`'s miss.
                    let map_clone = registers[map_reg as usize].clone();
                    let key_clone = registers[key_reg as usize].clone();
                    let by_clone = registers[by_reg as usize].clone();
                    let resolved = match &map_clone {
                        Value::Struct(_) => {
                            qualified_key(&map_clone, "inc").and_then(|q| self.lookup_global(q))
                        }
                        _ => None,
                    }
                    .or_else(|| self.lookup_global("inc"));
                    let args = vec![map_clone, key_clone, by_clone];
                    let result = match resolved {
                        Some(Global::Value(Value::Builtin(builtin_inner))) => {
                            (builtin_inner.call)(&args)?
                        }
                        Some(g) => self.apply(g, args)?,
                        None => {
                            return Err(RuntimeError::UnresolvedName("inc".to_string()));
                        }
                    };
                    registers[dst as usize] = result;
                }
                Op::Wide { idx } => {
                    // Side-table indirection for the rare
                    // 6-payload-field ops. Reads the actual
                    // operation out of `chunk.wide_ops` and
                    // dispatches inline. Adding a new wide op
                    // means adding a new arm to the inner match
                    // here AND a new variant to `WideOp` in
                    // bytecode.rs (and a matching emit site in
                    // compile.rs). The hot path stays unchanged
                    // for every non-wide op.
                    let wide = unsafe { chunk.wide_ops.get_unchecked(idx as usize) };
                    match wide {
                        crate::bytecode::WideOp::MapIncAt {
                            dst,
                            map_reg,
                            seq_reg,
                            start_reg,
                            len_reg,
                            by_reg,
                        } => {
                            let dst = *dst;
                            let map_reg = *map_reg;
                            let seq_reg = *seq_reg;
                            let start_reg = *start_reg;
                            let len_reg = *len_reg;
                            let by_reg = *by_reg;
                            // Zero-copy slice-hash counter that mirrors
                            // `*m.entry(&seq[start..start+len]).or_insert(0) += by`.
                            let result_int: i64 =
                                if let Value::Map(map) = &registers[map_reg as usize] {
                                    let seq_bytes: &[u8] = match &registers[seq_reg as usize] {
                                        Value::String(s) => s.as_bytes(),
                                        _ => &[],
                                    };
                                    let start = match &registers[start_reg as usize] {
                                        Value::Int(n) if *n >= 0 => *n as usize,
                                        _ => 0,
                                    };
                                    let len = match &registers[len_reg as usize] {
                                        Value::Int(n) if *n >= 0 => *n as usize,
                                        _ => 0,
                                    };
                                    let by = match &registers[by_reg as usize] {
                                        Value::Int(n) => *n,
                                        _ => 1,
                                    };
                                    if len == 0 || start + len > seq_bytes.len() {
                                        0
                                    } else {
                                        let key_bytes = &seq_bytes[start..start + len];
                                        // SAFETY: `seq_bytes` came from a
                                        // UTF-8 `String`, so any sub-slice
                                        // on a char boundary is also UTF-8.
                                        // ASCII inputs are always safe.
                                        let key_str =
                                            unsafe { std::str::from_utf8_unchecked(key_bytes) };
                                        let key = MapKey::Str(crate::value::SmolStr::from(
                                            key_str.to_string(),
                                        ));
                                        let mut guard = map.lock();
                                        let entry = guard.entry(key).or_insert(Value::Int(0));
                                        let new_val = match entry {
                                            Value::Int(cur) => *cur + by,
                                            _ => by,
                                        };
                                        *entry = Value::Int(new_val);
                                        new_val
                                    }
                                } else {
                                    0
                                };
                            registers[dst as usize] = Value::Int(result_int);
                        }
                        crate::bytecode::WideOp::BuildFloatArray {
                            dst_v,
                            name_idx,
                            fields_idx,
                            stride,
                            elem_count,
                            first_f,
                        } => {
                            let dst_v = *dst_v;
                            let name_idx = *name_idx;
                            let fields_idx = *fields_idx;
                            let stride = *stride;
                            let elem_count = *elem_count;
                            let first_f = *first_f;
                            let Value::String(name_arc) = &chunk.consts[name_idx as usize] else {
                                return Err(RuntimeError::Panic(
                                    "BuildFloatArray: name must be string const".to_string(),
                                ));
                            };
                            let name = name_arc.as_str().to_string();
                            let Value::Array(field_names_arr) = &chunk.consts[fields_idx as usize]
                            else {
                                return Err(RuntimeError::Panic(
                                    "BuildFloatArray: fields must be array of strings".to_string(),
                                ));
                            };
                            let field_names: Vec<String> = field_names_arr
                                .iter()
                                .filter_map(|v| match v {
                                    Value::String(s) => Some(s.as_str().to_string()),
                                    _ => None,
                                })
                                .collect();
                            let total = stride as usize * elem_count as usize;
                            let start = first_f as usize;
                            let end = start + total;
                            let data: Vec<f64> = floats[start..end].to_vec();
                            registers[dst_v as usize] = Value::float_array(
                                name,
                                stride,
                                Arc::new(field_names),
                                Arc::new(data),
                            );
                        }
                    }
                }
                Op::MapInc {
                    dst,
                    map_reg,
                    key_reg,
                    by_reg,
                } => {
                    // Fused `m.insert(k, m.get_or(k, 0) + by)`. The
                    // compiler only emits this op for receivers
                    // statically typed `HashMap`, so the fast arm
                    // is the only one that runs in practice. The
                    // generic arm handles polymorphic-by-promotion
                    // value shapes (i.e. a slot already holding
                    // something other than `Value::Int`) by going
                    // through the normal `bin_arith` path.
                    let result = if let Value::Map(map) = &registers[map_reg as usize] {
                        let key = MapKey::from_value(&registers[key_reg as usize]);
                        let by_val = &registers[by_reg as usize];
                        let mut guard = map.lock();
                        let entry = guard.entry(key).or_insert(Value::Int(0));
                        if let (Value::Int(cur), Value::Int(b)) = (&*entry, by_val) {
                            *entry = Value::Int(*cur + *b);
                        } else {
                            let cur = entry.clone();
                            let sum =
                                bin_arith(&cur, by_val, i64::wrapping_add, |a, b| a + b, "+")?;
                            *entry = sum;
                        }
                        let cloned = Arc::clone(map);
                        drop(guard);
                        Value::Map(cloned)
                    } else {
                        // Receiver isn't a Map (shouldn't happen for
                        // a HashMap-typed receiver, but stay total).
                        registers[map_reg as usize].clone()
                    };
                    registers[dst as usize] = result;
                }
                Op::IndexGet { dst, base, index } => {
                    let b = &registers[base as usize];
                    let i = &registers[index as usize];
                    registers[dst as usize] = index_get(b, i)?;
                }
                Op::StrByteAt { dst, recv, idx } => {
                    // Matches `builtin_str_byte_at`: non-string receiver,
                    // non-integer index, or out-of-range index all yield 0.
                    let byte = if let Value::String(s) = &registers[recv as usize] {
                        let i = match &registers[idx as usize] {
                            Value::Int(n) => *n,
                            other => match auto_deref_cell(other) {
                                Some(Value::Int(n)) => n,
                                _ => -1,
                            },
                        };
                        let bytes = s.as_str().as_bytes();
                        if i < 0 || (i as usize) >= bytes.len() {
                            0
                        } else {
                            i64::from(bytes[i as usize])
                        }
                    } else {
                        0
                    };
                    registers[dst as usize] = Value::Int(byte);
                }
                Op::IndexGetChecked { dst, base, index } => {
                    let b = &registers[base as usize];
                    let i = &registers[index as usize];
                    registers[dst as usize] = index_get_checked(b, i)?;
                }
                Op::IndexSet { base, index, value } => {
                    let new_value = registers[value as usize].clone();
                    let i = &registers[index as usize];
                    let raw = match i {
                        Value::Int(n) => *n,
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let b = &mut registers[base as usize];
                    // An `[f64]` vec whose elements so far were all
                    // integer-valued sits in `IntArray` storage (an `[i64]`
                    // can never receive a float store); widen it to flat
                    // float storage before the store below.
                    if let (Value::IntArray(data), Value::Float(_)) = (&*b, &new_value) {
                        *b = Value::FloatVec(Arc::new(data.iter().map(|n| *n as f64).collect()));
                    }
                    match b {
                        Value::Array(items) | Value::Tuple(items) => {
                            // A whole-element indexed write is a lenient no-op
                            // out of range on both tiers (the compiled inline
                            // vec store is bounds-guarded for scalar and
                            // aggregate elements alike; only `v[i].field = x`
                            // field projection panics, via a separate op).
                            if raw >= 0 && (raw as usize) < items.len() {
                                Arc::make_mut(items)[raw as usize] = new_value;
                            }
                        }
                        // Scalar arrays specialize to `IntArray` / `FloatVec`,
                        // which the compiled tier indexes with an inline
                        // bounds-guarded store: an out-of-range write (negative
                        // or past the end) is a lenient no-op on both tiers,
                        // matching the lenient zero-value read contract.
                        Value::IntArray(data) => {
                            if raw >= 0 {
                                let v = Arc::make_mut(data);
                                let idx = raw as usize;
                                if idx < v.len() {
                                    match new_value {
                                        Value::Int(n) => v[idx] = n,
                                        _ => {
                                            return Err(RuntimeError::Type(
                                                "IndexSet on IntArray expects i64 value"
                                                    .to_string(),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        Value::FloatVec(data) => {
                            if raw >= 0 {
                                let v = Arc::make_mut(data);
                                let idx = raw as usize;
                                if idx < v.len() {
                                    match new_value {
                                        Value::Float(f) => v[idx] = f,
                                        Value::Int(n) => v[idx] = n as f64,
                                        _ => {
                                            return Err(RuntimeError::Type(
                                                "IndexSet on FloatVec expects f64 value"
                                                    .to_string(),
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                        _ => {
                            return Err(RuntimeError::Type(format!(
                                "value of kind `{b}` is not indexable"
                            )));
                        }
                    }
                }
                Op::FieldGet {
                    dst,
                    receiver,
                    name_idx,
                    cache_idx,
                } => {
                    // PEP 659-style inline cache. Fast path: when the
                    // observed receiver shape (struct-name interned
                    // pointer) matches the slot's `type_token`, jump
                    // straight to `inner.fields[offset].1.clone()` -
                    // skipping the linear name-scan in `field_get`.
                    let recv = &registers[receiver as usize];
                    if let Value::Struct(inner) = recv {
                        // `inner.name` is a globally-interned `&'static str`
                        // (canonical, pointer-stable across every clone of
                        // any instance of this type), so its address is a
                        // ready-made guard token - no second hash needed.
                        let token = inner.name.as_ptr() as u64;
                        let slot = &state.field_caches[cache_idx as usize];
                        if slot.type_token.get() == token {
                            let off = slot.offset.get() as usize;
                            if off < inner.fields.len() {
                                registers[dst as usize] = inner.fields[off].1.clone();
                                continue;
                            }
                        }
                        // Miss: linear-scan, then refill the slot for next
                        // time. Every instance of a struct shares one
                        // globally-interned `name`, so the token compare
                        // above is `O(1)` after fill.
                        let field_name = match &chunk.consts[name_idx as usize] {
                            Value::String(s) => s.as_str(),
                            _ => {
                                return Err(RuntimeError::Panic(
                                    "FieldGet: name must be string const".to_string(),
                                ));
                            }
                        };
                        if let Some(pos) = inner
                            .fields
                            .iter()
                            .position(|(ident, _)| (*ident) == field_name)
                        {
                            slot.type_token.set(token);
                            slot.offset.set(pos as u16);
                            registers[dst as usize] = inner.fields[pos].1.clone();
                        } else {
                            registers[dst as usize] = Value::Unit;
                        }
                    } else {
                        let field_name = match &chunk.consts[name_idx as usize] {
                            Value::String(s) => s.clone(),
                            _ => {
                                return Err(RuntimeError::Panic(
                                    "FieldGet: name must be string const".to_string(),
                                ));
                            }
                        };
                        let v = field_get(recv, field_name.as_str())?;
                        registers[dst as usize] = v;
                    }
                }
                Op::FieldSet {
                    receiver,
                    name_idx,
                    value,
                } => {
                    let field_name = match &chunk.consts[name_idx as usize] {
                        Value::String(s) => s.clone(),
                        _ => {
                            return Err(RuntimeError::Panic(
                                "FieldSet: name must be string const".to_string(),
                            ));
                        }
                    };
                    let new_value = registers[value as usize].clone();
                    let recv = &mut registers[receiver as usize];
                    field_set(recv, field_name.as_str(), new_value)?;
                }
                Op::VecPush { receiver, value } => {
                    let new_value = registers[value as usize].clone();
                    let recv = &mut registers[receiver as usize];
                    if let Some(promoted) = promote_scalar_push(recv, &new_value) {
                        *recv = promoted;
                    } else {
                        match recv {
                            Value::Array(items) => Arc::make_mut(items).push(new_value),
                            Value::IntArray(data) => {
                                if let Value::Int(n) = new_value {
                                    Arc::make_mut(data).push(n);
                                }
                            }
                            Value::FloatVec(data) => match new_value {
                                Value::Float(f) => Arc::make_mut(data).push(f),
                                Value::Int(n) => Arc::make_mut(data).push(n as f64),
                                _ => {}
                            },
                            _ => {}
                        }
                    }
                }
                Op::StrAppend { receiver, value } => {
                    // Read the RHS first (a cheap SmolStr clone: an Arc
                    // bump or inline copy), so the receiver can then be
                    // borrowed mutably without aliasing the value register
                    // (and `s += s` stays correct).
                    let rhs = match &registers[value as usize] {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    };
                    if let Some(rhs) = rhs {
                        if let Value::String(dst) = &mut registers[receiver as usize] {
                            dst.push_str(rhs.as_str());
                        }
                    }
                }
                Op::VecPop { dst, receiver } => {
                    let popped = match &mut registers[receiver as usize] {
                        Value::Array(items) => Arc::make_mut(items).pop(),
                        Value::IntArray(data) => Arc::make_mut(data).pop().map(Value::Int),
                        Value::FloatVec(data) => Arc::make_mut(data).pop().map(Value::Float),
                        _ => None,
                    };
                    registers[dst as usize] = match popped {
                        Some(v) => Value::variant("Some", vec![v]),
                        None => Value::variant("None", vec![]),
                    };
                }
                Op::VecInsert {
                    receiver,
                    index,
                    value,
                } => {
                    // A negative index is a no-op, matching the
                    // `builtin_insert` fallback; a positive index past
                    // the end clamps to the length (an append).
                    let idx = match &registers[index as usize] {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        _ => continue,
                    };
                    let new_value = registers[value as usize].clone();
                    let recv = &mut registers[receiver as usize];
                    // Same typed-storage routing as `Op::VecPush`: a scalar
                    // insert into an empty generic array switches it to flat
                    // storage, and a float insert widens an integer-valued
                    // `[f64]` out of `IntArray` storage first.
                    match (&*recv, &new_value) {
                        (Value::Array(items), Value::Int(_)) if items.is_empty() => {
                            *recv = Value::IntArray(Arc::new(Vec::new()));
                        }
                        (Value::Array(items), Value::Float(_)) if items.is_empty() => {
                            *recv = Value::FloatVec(Arc::new(Vec::new()));
                        }
                        (Value::IntArray(data), Value::Float(_)) => {
                            *recv =
                                Value::FloatVec(Arc::new(data.iter().map(|n| *n as f64).collect()));
                        }
                        _ => {}
                    }
                    match recv {
                        Value::Array(items) => {
                            let v = Arc::make_mut(items);
                            v.insert(idx.min(v.len()), new_value);
                        }
                        Value::IntArray(data) => {
                            if let Value::Int(n) = new_value {
                                let v = Arc::make_mut(data);
                                v.insert(idx.min(v.len()), n);
                            }
                        }
                        Value::FloatVec(data) => {
                            let f = match new_value {
                                Value::Float(f) => Some(f),
                                Value::Int(n) => Some(n as f64),
                                _ => None,
                            };
                            if let Some(f) = f {
                                let v = Arc::make_mut(data);
                                v.insert(idx.min(v.len()), f);
                            }
                        }
                        _ => {}
                    }
                }
                Op::VecRemove { receiver, index } => {
                    let idx = match &registers[index as usize] {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        _ => continue,
                    };
                    match &mut registers[receiver as usize] {
                        Value::Array(items) => {
                            let v = Arc::make_mut(items);
                            if idx < v.len() {
                                v.remove(idx);
                            }
                        }
                        Value::IntArray(data) => {
                            let v = Arc::make_mut(data);
                            if idx < v.len() {
                                v.remove(idx);
                            }
                        }
                        Value::FloatVec(data) => {
                            let v = Arc::make_mut(data);
                            if idx < v.len() {
                                v.remove(idx);
                            }
                        }
                        _ => {}
                    }
                }
                Op::VecRemoveAt {
                    dst,
                    receiver,
                    index,
                } => {
                    let idx = match &registers[index as usize] {
                        Value::Int(n) => *n,
                        _ => -1,
                    };
                    let removed = match &mut registers[receiver as usize] {
                        Value::Array(items) => {
                            let v = Arc::make_mut(items);
                            if idx >= 0 && (idx as usize) < v.len() {
                                Some(v.remove(idx as usize))
                            } else {
                                None
                            }
                        }
                        Value::IntArray(data) => {
                            let v = Arc::make_mut(data);
                            if idx >= 0 && (idx as usize) < v.len() {
                                Some(Value::Int(v.remove(idx as usize)))
                            } else {
                                None
                            }
                        }
                        Value::FloatVec(data) => {
                            let v = Arc::make_mut(data);
                            if idx >= 0 && (idx as usize) < v.len() {
                                Some(Value::Float(v.remove(idx as usize)))
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    registers[dst as usize] = match removed {
                        Some(elem) => Value::variant("Ok", vec![elem]),
                        None => {
                            crate::builtins::slice_err(format!("remove: index {idx} out of bounds"))
                        }
                    };
                }
                Op::TupleIndex {
                    dst,
                    receiver,
                    index,
                } => {
                    let recv = &registers[receiver as usize];
                    let idx = index as usize;
                    registers[dst as usize] = match recv {
                        Value::Tuple(items) => items.get(idx).cloned().ok_or_else(|| {
                            RuntimeError::Arithmetic("tuple index out of bounds".to_string())
                        })?,
                        Value::Array(items) => items.get(idx).cloned().ok_or_else(|| {
                            RuntimeError::Arithmetic("tuple index out of bounds".to_string())
                        })?,
                        // A tuple struct stores its positional fields as named
                        // "0".."N-1"; `.N` projects field N, matching the
                        // compiled tiers' offset load.
                        Value::Struct(inner) => inner
                            .fields
                            .get(idx)
                            .map(|(_, v)| v.clone())
                            .ok_or_else(|| {
                                RuntimeError::Arithmetic("tuple index out of bounds".to_string())
                            })?,
                        _ => {
                            return Err(RuntimeError::Type(format!(
                                "value of kind `{recv}` has no tuple fields"
                            )));
                        }
                    };
                }
                Op::TupleTailIndex {
                    dst,
                    receiver,
                    offset_from_end,
                } => {
                    let recv = &registers[receiver as usize];
                    registers[dst as usize] = match recv.as_value_slice() {
                        Some(items) => {
                            let len = items.len();
                            let idx = len.saturating_sub(offset_from_end as usize + 1);
                            items.get(idx).cloned().ok_or_else(|| {
                                RuntimeError::Arithmetic(
                                    "tuple tail index out of bounds".to_string(),
                                )
                            })?
                        }
                        None => {
                            return Err(RuntimeError::Type(format!(
                                "value of kind `{recv}` has no tuple fields"
                            )));
                        }
                    };
                }
                Op::IndexedFieldSet {
                    base,
                    index,
                    name_idx,
                    value,
                } => {
                    let idx = match &registers[index as usize] {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let field_name_arc = match &chunk.consts[name_idx as usize] {
                        Value::String(s) => s.clone(),
                        _ => {
                            return Err(RuntimeError::Panic(
                                "IndexedFieldSet: name must be string const".to_string(),
                            ));
                        }
                    };
                    let field_name: &str = &field_name_arc;
                    let new_value = registers[value as usize].clone();
                    let b = &mut registers[base as usize];
                    let (Value::Array(items) | Value::Tuple(items)) = b else {
                        return Err(RuntimeError::Type(format!(
                            "value of kind `{b}` is not indexable"
                        )));
                    };
                    let slots = Arc::make_mut(items);
                    let slot = slots.get_mut(idx).ok_or_else(|| {
                        // `v[i].field = x` out of range panics on the compiled
                        // tier (the place-form bounds assert); match it here.
                        RuntimeError::Panic("index out of bounds".to_string())
                    })?;
                    let Value::Struct(struct_arc) = slot else {
                        return Err(RuntimeError::Type(format!(
                            "cannot assign to field `{field_name}` on non-struct"
                        )));
                    };
                    let struct_inner = Arc::make_mut(struct_arc);
                    let field_slots = &mut struct_inner.fields;
                    let pos = field_slots
                        .iter()
                        .position(|(ident, _)| (*ident) == field_name);
                    if let Some(p) = pos {
                        field_slots[p].1 = new_value;
                    } else {
                        // Dynamic field add (e.g. `json::Object`): the
                        // fixed-arity slice grows by one, rebuilt once.
                        let mut grown = std::mem::take(field_slots).into_vec();
                        grown.push((crate::value::intern_type_name(field_name), new_value));
                        *field_slots = grown.into_boxed_slice();
                    }
                }
                Op::MakeClosure { dst, proto } => {
                    let proto = &chunk.closure_protos[proto as usize];
                    // Snapshot the captured upvalue registers. A `Value`
                    // clone is a by-value snapshot for scalars and an
                    // `Arc` refcount bump for aggregates, so a captured
                    // aggregate mutated through the closure stays visible
                    // to the original binding.
                    let capture_values: Vec<Value> = proto
                        .capture_regs
                        .iter()
                        .map(|r| registers[*r as usize].clone())
                        .collect();
                    registers[dst as usize] = Value::Closure(Arc::new(crate::value::Closure {
                        chunk: Arc::clone(&proto.chunk),
                        capture_values,
                    }));
                }
                Op::Select { first, count } => {
                    let start = first as usize;
                    let arms = &chunk.select_arms[start..start + count as usize];
                    pc = select_dispatch(arms, &mut registers[..]);
                }
                Op::CovHit { slot } => {
                    gossamer_runtime::coverage::bump(slot as usize);
                }

                // ----- Phase 1 typed ops -----
                //
                // All float/int register accesses use
                // `get_unchecked` - the register slot index is
                // always less than `chunk.float_count` /
                // `chunk.int_count` by construction of the
                // bytecode (the compiler emits a fresh index for
                // every destination and carries it through
                // compile_expr_ex).
                Op::LoadConstF64 { dst_f, idx } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) =
                        *chunk.f64_consts.get_unchecked(idx as usize);
                },
                Op::AddF64 {
                    dst_f,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) = *floats
                        .get_unchecked(lhs_f as usize)
                        + *floats.get_unchecked(rhs_f as usize);
                },
                Op::SubF64 {
                    dst_f,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) = *floats
                        .get_unchecked(lhs_f as usize)
                        - *floats.get_unchecked(rhs_f as usize);
                },
                Op::MulF64 {
                    dst_f,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) = *floats
                        .get_unchecked(lhs_f as usize)
                        * *floats.get_unchecked(rhs_f as usize);
                },
                Op::DivF64 {
                    dst_f,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) = *floats
                        .get_unchecked(lhs_f as usize)
                        / *floats.get_unchecked(rhs_f as usize);
                },
                Op::NegF64 { dst_f, src_f } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) =
                        -*floats.get_unchecked(src_f as usize);
                },
                Op::LtF64 {
                    dst_v,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *floats.get_unchecked(lhs_f as usize)
                            < *floats.get_unchecked(rhs_f as usize),
                    );
                },
                Op::LeF64 {
                    dst_v,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *floats.get_unchecked(lhs_f as usize)
                            <= *floats.get_unchecked(rhs_f as usize),
                    );
                },
                Op::GtF64 {
                    dst_v,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *floats.get_unchecked(lhs_f as usize)
                            > *floats.get_unchecked(rhs_f as usize),
                    );
                },
                Op::GeF64 {
                    dst_v,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *floats.get_unchecked(lhs_f as usize)
                            >= *floats.get_unchecked(rhs_f as usize),
                    );
                },
                Op::EqF64 {
                    dst_v,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *floats.get_unchecked(lhs_f as usize)
                            == *floats.get_unchecked(rhs_f as usize),
                    );
                },
                Op::NeF64 {
                    dst_v,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *floats.get_unchecked(lhs_f as usize)
                            != *floats.get_unchecked(rhs_f as usize),
                    );
                },
                Op::UnboxF64 { dst_f, src_v } => {
                    let v = &registers[src_v as usize];
                    let f = match v {
                        Value::Float(f) => *f,
                        Value::Int(n) => *n as f64,
                        // 0.7.0 flag::Cell auto-deref for `Cell<f64>`
                        // / `Cell<i64>` operand mixing.
                        Value::Struct(inner) if inner.name == "__Cell" => {
                            match auto_deref_cell(v) {
                                Some(Value::Float(f)) => f,
                                Some(Value::Int(n)) => n as f64,
                                _ => {
                                    return Err(RuntimeError::Type(format!(
                                        "expected f64 at register, got `{v}`"
                                    )));
                                }
                            }
                        }
                        _ => {
                            return Err(RuntimeError::Type(format!(
                                "expected f64 at register, got `{v}`"
                            )));
                        }
                    };
                    floats[dst_f as usize] = f;
                }
                Op::BoxF64 { dst_v, src_f } => {
                    registers[dst_v as usize] = Value::Float(floats[src_f as usize]);
                }
                Op::SqrtF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].sqrt();
                }
                Op::SinF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].sin();
                }
                Op::CosF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].cos();
                }
                Op::AbsF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].abs();
                }
                Op::FloorF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].floor();
                }
                Op::CeilF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].ceil();
                }
                Op::ExpF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].exp();
                }
                Op::LnF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].ln();
                }
                Op::MulAddF64 {
                    dst_f,
                    a_f,
                    b_f,
                    c_f,
                } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) =
                        floats.get_unchecked(a_f as usize).mul_add(
                            *floats.get_unchecked(b_f as usize),
                            *floats.get_unchecked(c_f as usize),
                        );
                },
                Op::MulSubF64 {
                    dst_f,
                    a_f,
                    b_f,
                    c_f,
                } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) =
                        floats.get_unchecked(a_f as usize).mul_add(
                            -*floats.get_unchecked(b_f as usize),
                            *floats.get_unchecked(c_f as usize),
                        );
                },

                Op::LoadConstI64 { dst_i, idx } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) =
                        *chunk.i64_consts.get_unchecked(idx as usize);
                },
                Op::AddI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) = ints
                        .get_unchecked(lhs_i as usize)
                        .wrapping_add(*ints.get_unchecked(rhs_i as usize));
                },
                Op::SubI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) = ints
                        .get_unchecked(lhs_i as usize)
                        .wrapping_sub(*ints.get_unchecked(rhs_i as usize));
                },
                Op::MulI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) = ints
                        .get_unchecked(lhs_i as usize)
                        .wrapping_mul(*ints.get_unchecked(rhs_i as usize));
                },
                Op::DivI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => {
                    let r = ints[rhs_i as usize];
                    if r == 0 {
                        return Err(RuntimeError::Panic("divide by zero".to_string()));
                    }
                    ints[dst_i as usize] = ints[lhs_i as usize].wrapping_div(r);
                }
                Op::RemI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => {
                    let r = ints[rhs_i as usize];
                    if r == 0 {
                        return Err(RuntimeError::Panic("divide by zero".to_string()));
                    }
                    ints[dst_i as usize] = ints[lhs_i as usize].wrapping_rem(r);
                }
                Op::ArithImmI64 {
                    kind,
                    dst_i,
                    lhs_i,
                    imm,
                } => unsafe {
                    let lhs = *ints.get_unchecked(lhs_i as usize);
                    let imm = imm as i64;
                    // Div/Rem immediates are non-zero by the emitter's
                    // contract, so no zero check is paid here.
                    *ints.get_unchecked_mut(dst_i as usize) = match kind {
                        ImmArithKind::Add => lhs.wrapping_add(imm),
                        ImmArithKind::Sub => lhs.wrapping_sub(imm),
                        ImmArithKind::Mul => lhs.wrapping_mul(imm),
                        ImmArithKind::Div => lhs.wrapping_div(imm),
                        ImmArithKind::Rem => lhs.wrapping_rem(imm),
                    };
                },
                Op::NegI64 { dst_i, src_i } => {
                    ints[dst_i as usize] = ints[src_i as usize].wrapping_neg();
                }
                Op::BitAndI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) =
                        ints.get_unchecked(lhs_i as usize) & ints.get_unchecked(rhs_i as usize);
                },
                Op::BitOrI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) =
                        ints.get_unchecked(lhs_i as usize) | ints.get_unchecked(rhs_i as usize);
                },
                Op::BitXorI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) =
                        ints.get_unchecked(lhs_i as usize) ^ ints.get_unchecked(rhs_i as usize);
                },
                Op::ShlI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    let shift = (*ints.get_unchecked(rhs_i as usize) & 63) as u32;
                    *ints.get_unchecked_mut(dst_i as usize) =
                        ints.get_unchecked(lhs_i as usize).wrapping_shl(shift);
                },
                Op::ShrI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    let shift = (*ints.get_unchecked(rhs_i as usize) & 63) as u32;
                    *ints.get_unchecked_mut(dst_i as usize) =
                        ints.get_unchecked(lhs_i as usize).wrapping_shr(shift);
                },
                Op::ShrU64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    let shift = (*ints.get_unchecked(rhs_i as usize) & 63) as u32;
                    *ints.get_unchecked_mut(dst_i as usize) =
                        (*ints.get_unchecked(lhs_i as usize) as u64).wrapping_shr(shift) as i64;
                },
                Op::LtI64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *ints.get_unchecked(lhs_i as usize) < *ints.get_unchecked(rhs_i as usize),
                    );
                },
                Op::LeI64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *ints.get_unchecked(lhs_i as usize) <= *ints.get_unchecked(rhs_i as usize),
                    );
                },
                Op::GtI64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *ints.get_unchecked(lhs_i as usize) > *ints.get_unchecked(rhs_i as usize),
                    );
                },
                Op::GeI64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *ints.get_unchecked(lhs_i as usize) >= *ints.get_unchecked(rhs_i as usize),
                    );
                },
                Op::EqI64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *ints.get_unchecked(lhs_i as usize) == *ints.get_unchecked(rhs_i as usize),
                    );
                },
                Op::NeI64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *ints.get_unchecked(lhs_i as usize) != *ints.get_unchecked(rhs_i as usize),
                    );
                },
                Op::LtU64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        (*ints.get_unchecked(lhs_i as usize) as u64)
                            < (*ints.get_unchecked(rhs_i as usize) as u64),
                    );
                },
                Op::LeU64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        (*ints.get_unchecked(lhs_i as usize) as u64)
                            <= (*ints.get_unchecked(rhs_i as usize) as u64),
                    );
                },
                Op::GtU64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        (*ints.get_unchecked(lhs_i as usize) as u64)
                            > (*ints.get_unchecked(rhs_i as usize) as u64),
                    );
                },
                Op::GeU64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        (*ints.get_unchecked(lhs_i as usize) as u64)
                            >= (*ints.get_unchecked(rhs_i as usize) as u64),
                    );
                },
                Op::UnboxI64 { dst_i, src_v } => {
                    let v = &registers[src_v as usize];
                    // 0.7.0 flag::Cell auto-deref. The typechecker
                    // pins `flags.number` as `Cell<i64>` and emits
                    // an UnboxI64 hoping to find a `Value::Int`;
                    // without this branch the cell handle hits the
                    // type-error arm and aborts.
                    let n = match v {
                        Value::Int(n) => *n,
                        // `as usize` / `as u64` produce a `Uint` (for unsigned
                        // display); every ≤64-bit integer shares i64 arithmetic,
                        // so unboxing it into the typed i64 register is correct
                        // (and lets `(x as usize) < (y as usize)` take the typed
                        // comparison path instead of type-erroring).
                        Value::Uint(n) => *n as i64,
                        Value::Struct(inner) if inner.name == "__Cell" => {
                            match auto_deref_cell(v) {
                                Some(Value::Int(n)) => n,
                                Some(Value::Uint(n)) => n as i64,
                                _ => {
                                    return Err(RuntimeError::Type(format!(
                                        "expected i64 at register, got `{v}`"
                                    )));
                                }
                            }
                        }
                        _ => {
                            return Err(RuntimeError::Type(format!(
                                "expected i64 at register, got `{v}`"
                            )));
                        }
                    };
                    ints[dst_i as usize] = n;
                }
                Op::BoxI64 { dst_v, src_i } => {
                    registers[dst_v as usize] = Value::Int(ints[src_i as usize]);
                }
                Op::MoveF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize];
                }
                Op::MoveI64 { dst_i, src_i } => {
                    ints[dst_i as usize] = ints[src_i as usize];
                }

                // ----- Phase 2 fused / typed field access -----
                Op::FieldGetF64 {
                    dst_f,
                    receiver,
                    name_idx,
                } => {
                    let Value::String(field_name) = &chunk.consts[name_idx as usize] else {
                        return Err(RuntimeError::Panic(
                            "FieldGetF64: name must be string const".to_string(),
                        ));
                    };
                    let recv = &registers[receiver as usize];
                    let Value::Struct(struct_inner) = recv else {
                        return Err(RuntimeError::Type(format!(
                            "field access on non-struct `{recv}`"
                        )));
                    };
                    let mut val = 0.0f64;
                    for (ident, v) in &struct_inner.fields {
                        if (*ident) == field_name.as_str() {
                            val = match v {
                                Value::Float(f) => *f,
                                Value::Int(n) => *n as f64,
                                _ => 0.0,
                            };
                            break;
                        }
                    }
                    floats[dst_f as usize] = val;
                }
                Op::IndexedFieldGet {
                    dst,
                    base,
                    index,
                    name_idx,
                } => {
                    let idx = match &registers[index as usize] {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let Value::String(field_name) = &chunk.consts[name_idx as usize] else {
                        return Err(RuntimeError::Panic(
                            "IndexedFieldGet: name must be string const".to_string(),
                        ));
                    };
                    let b = &registers[base as usize];
                    let Some(items) = b.as_value_slice() else {
                        return Err(RuntimeError::Type(format!(
                            "value of kind `{b}` is not indexable"
                        )));
                    };
                    let slot = items.get(idx).ok_or_else(|| {
                        RuntimeError::Arithmetic("index out of bounds".to_string())
                    })?;
                    let Value::Struct(struct_inner) = slot else {
                        return Err(RuntimeError::Type(
                            "value at index is not a struct".to_string(),
                        ));
                    };
                    let mut found = None;
                    for (ident, v) in &struct_inner.fields {
                        if (*ident) == field_name.as_str() {
                            found = Some(v);
                            break;
                        }
                    }
                    registers[dst as usize] = found.cloned().unwrap_or(Value::Unit);
                }
                Op::IndexedFieldGetF64 {
                    dst_f,
                    base,
                    index,
                    name_idx,
                } => {
                    let idx = match &registers[index as usize] {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let Value::String(field_name) = &chunk.consts[name_idx as usize] else {
                        return Err(RuntimeError::Panic(
                            "IndexedFieldGetF64: name must be string const".to_string(),
                        ));
                    };
                    let b = &registers[base as usize];
                    // FloatArray fast path - resolve the field
                    // name against the stored declaration order
                    // and pull the f64 directly out of flat data.
                    if let Value::FloatArray(fa_inner) = b {
                        let off = fa_inner
                            .field_names
                            .iter()
                            .position(|n| n.as_str() == field_name.as_str())
                            .unwrap_or(0);
                        let stride = fa_inner.stride as usize;
                        let pos = idx * stride + off;
                        floats[dst_f as usize] = *fa_inner.data.get(pos).unwrap_or(&0.0);
                        continue;
                    }
                    let Some(items) = b.as_value_slice() else {
                        return Err(RuntimeError::Type(format!(
                            "value of kind `{b}` is not indexable"
                        )));
                    };
                    let slot = items.get(idx).ok_or_else(|| {
                        RuntimeError::Arithmetic("index out of bounds".to_string())
                    })?;
                    let Value::Struct(struct_inner) = slot else {
                        return Err(RuntimeError::Type(
                            "value at index is not a struct".to_string(),
                        ));
                    };
                    let mut val = 0.0f64;
                    for (ident, v) in &struct_inner.fields {
                        if (*ident) == field_name.as_str() {
                            val = match v {
                                Value::Float(f) => *f,
                                Value::Int(n) => *n as f64,
                                _ => 0.0,
                            };
                            break;
                        }
                    }
                    floats[dst_f as usize] = val;
                }
                Op::IndexedFieldSetF64 {
                    base,
                    index,
                    name_idx,
                    value_f,
                } => {
                    let idx = match &registers[index as usize] {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let field_name_arc = match &chunk.consts[name_idx as usize] {
                        Value::String(s) => s.clone(),
                        _ => {
                            return Err(RuntimeError::Panic(
                                "IndexedFieldSetF64: name must be string const".to_string(),
                            ));
                        }
                    };
                    let field_name = field_name_arc.as_str();
                    let new_value = Value::Float(floats[value_f as usize]);
                    let b = &mut registers[base as usize];
                    let (Value::Array(items) | Value::Tuple(items)) = b else {
                        return Err(RuntimeError::Type(format!(
                            "value of kind `{b}` is not indexable"
                        )));
                    };
                    let slots = Arc::make_mut(items);
                    let slot = slots.get_mut(idx).ok_or_else(|| {
                        // `v[i].field = x` out of range panics on the compiled
                        // tier (the place-form bounds assert); match it here.
                        RuntimeError::Panic("index out of bounds".to_string())
                    })?;
                    let Value::Struct(struct_arc) = slot else {
                        return Err(RuntimeError::Type(format!(
                            "cannot assign to field `{field_name}` on non-struct"
                        )));
                    };
                    let struct_inner = Arc::make_mut(struct_arc);
                    let field_slots = &mut struct_inner.fields;
                    let pos = field_slots
                        .iter()
                        .position(|(ident, _)| (*ident) == field_name);
                    if let Some(p) = pos {
                        field_slots[p].1 = new_value;
                    } else {
                        // Dynamic field add (e.g. `json::Object`): the
                        // fixed-arity slice grows by one, rebuilt once.
                        let mut grown = std::mem::take(field_slots).into_vec();
                        grown.push((crate::value::intern_type_name(field_name), new_value));
                        *field_slots = grown.into_boxed_slice();
                    }
                }

                // ----- Phase 2 offset-resolved ops -----
                Op::IndexedFieldGetF64ByOffset {
                    dst_f,
                    base,
                    index,
                    offset,
                } => {
                    // SAFETY: `index`, `base`, `dst_f` are
                    // compile-time allocated register slots,
                    // so the indexed accesses into `registers`
                    // and `floats` are always in bounds.
                    let idx = match unsafe { registers.get_unchecked(index as usize) } {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let b = unsafe { registers.get_unchecked(base as usize) };
                    // Flat-f64 fast path: direct f64 load out
                    // of the flat data buffer, no `Value`
                    // discriminant, no `Arc::clone`.
                    if let Value::FloatArray(fa_inner) = b {
                        let pos = idx * (fa_inner.stride as usize) + offset as usize;
                        // SAFETY: the FloatArray was built with
                        // `data.len() == stride * elem_count`, and
                        // `offset < stride` by construction
                        // (compile-time checked). `idx` is the
                        // caller's responsibility; we bounds-check
                        // it once here.
                        if pos >= fa_inner.data.len() {
                            return Err(RuntimeError::Arithmetic(
                                "index out of bounds".to_string(),
                            ));
                        }
                        let f = unsafe { *fa_inner.data.get_unchecked(pos) };
                        unsafe {
                            *floats.get_unchecked_mut(dst_f as usize) = f;
                        }
                    } else {
                        let Some(items) = b.as_value_slice() else {
                            return Err(RuntimeError::Type(format!(
                                "value of kind `{b}` is not indexable"
                            )));
                        };
                        let slot = items.get(idx).ok_or_else(|| {
                            RuntimeError::Arithmetic("index out of bounds".to_string())
                        })?;
                        let Value::Struct(struct_inner) = slot else {
                            return Err(RuntimeError::Type(
                                "value at index is not a struct".to_string(),
                            ));
                        };
                        let f = match struct_inner.fields.get(offset as usize).map(|(_, v)| v) {
                            Some(Value::Float(f)) => *f,
                            Some(Value::Int(n)) => *n as f64,
                            _ => 0.0,
                        };
                        floats[dst_f as usize] = f;
                    }
                }
                Op::IndexedFieldSetF64ByOffset {
                    base,
                    index,
                    offset,
                    value_f,
                } => {
                    let idx = match unsafe { registers.get_unchecked(index as usize) } {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    // SAFETY: `value_f` and `base` are
                    // compile-allocated register slots.
                    let new_f = unsafe { *floats.get_unchecked(value_f as usize) };
                    let b = unsafe { registers.get_unchecked_mut(base as usize) };
                    // Flat-f64 fast path: one `Arc::make_mut`
                    // plus a direct memory store. The common
                    // case is a refcount-1 Arc, so `make_mut`
                    // returns the inner mut ref without cloning
                    // - still one acquire-load per write, but no
                    // struct clone and no field scan.
                    if let Value::FloatArray(fa_arc) = b {
                        let fa_inner = Arc::make_mut(fa_arc);
                        let stride = fa_inner.stride as usize;
                        let pos = idx * stride + offset as usize;
                        let buf = Arc::make_mut(&mut fa_inner.data);
                        // SAFETY: `pos < stride * elem_count == buf.len()`
                        // when `idx < elem_count`; we verify that.
                        if pos < buf.len() {
                            unsafe {
                                *buf.get_unchecked_mut(pos) = new_f;
                            }
                        } else {
                            // `v[i].field = x` out of range panics on the
                            // compiled tier; match it here.
                            return Err(RuntimeError::Panic("index out of bounds".to_string()));
                        }
                    } else {
                        let new_value = Value::Float(new_f);
                        let (Value::Array(items) | Value::Tuple(items)) = b else {
                            return Err(RuntimeError::Type(format!(
                                "value of kind `{b}` is not indexable"
                            )));
                        };
                        let slots = Arc::make_mut(items);
                        let slot = slots.get_mut(idx).ok_or_else(|| {
                            RuntimeError::Arithmetic("index out of bounds".to_string())
                        })?;
                        let Value::Struct(struct_arc) = slot else {
                            return Err(RuntimeError::Type(
                                "cannot assign to field on non-struct".to_string(),
                            ));
                        };
                        let struct_inner = Arc::make_mut(struct_arc);
                        let field_slots = &mut struct_inner.fields;
                        if let Some(entry) = field_slots.get_mut(offset as usize) {
                            entry.1 = new_value;
                        }
                    }
                }
                Op::BranchIfLtI64 {
                    lhs_i,
                    rhs_i,
                    target,
                } => unsafe {
                    if *ints.get_unchecked(lhs_i as usize) < *ints.get_unchecked(rhs_i as usize) {
                        pc = target;
                    }
                },
                Op::BranchIfGeI64 {
                    lhs_i,
                    rhs_i,
                    target,
                } => unsafe {
                    if *ints.get_unchecked(lhs_i as usize) >= *ints.get_unchecked(rhs_i as usize) {
                        pc = target;
                    }
                },
                Op::BranchIfGtI64 {
                    lhs_i,
                    rhs_i,
                    target,
                } => unsafe {
                    if *ints.get_unchecked(lhs_i as usize) > *ints.get_unchecked(rhs_i as usize) {
                        pc = target;
                    }
                },
                Op::BranchIfLtF64 {
                    lhs_f,
                    rhs_f,
                    target,
                } => unsafe {
                    if *floats.get_unchecked(lhs_f as usize) < *floats.get_unchecked(rhs_f as usize)
                    {
                        pc = target;
                    }
                },
                Op::BranchIfGeF64 {
                    lhs_f,
                    rhs_f,
                    target,
                } => unsafe {
                    if *floats.get_unchecked(lhs_f as usize)
                        >= *floats.get_unchecked(rhs_f as usize)
                    {
                        pc = target;
                    }
                },
                Op::IncJumpIfLtI64 {
                    counter_i,
                    end_i,
                    target,
                } => unsafe {
                    // Bottom-of-loop fused tick for `for i in a..b`.
                    // SAFETY: counter_i and end_i are typed-i64 regs
                    // allocated by `try_compile_for_loop_range`; the
                    // counter_i slot is the same one tracked by the
                    // pre-loop bounds check, and the int register
                    // file size is sized to hold both.
                    let next = (*ints.get_unchecked(counter_i as usize)).wrapping_add(1);
                    *ints.get_unchecked_mut(counter_i as usize) = next;
                    if next < *ints.get_unchecked(end_i as usize) {
                        pc = target;
                    }
                },
                Op::IncJumpIfLeI64 {
                    counter_i,
                    end_i,
                    target,
                } => unsafe {
                    let next = (*ints.get_unchecked(counter_i as usize)).wrapping_add(1);
                    *ints.get_unchecked_mut(counter_i as usize) = next;
                    if next <= *ints.get_unchecked(end_i as usize) {
                        pc = target;
                    }
                },

                Op::FieldGetF64ByOffset {
                    dst_f,
                    receiver,
                    offset,
                } => {
                    let recv = &registers[receiver as usize];
                    let Value::Struct(struct_inner) = recv else {
                        return Err(RuntimeError::Type(format!(
                            "field access on non-struct `{recv}`"
                        )));
                    };
                    let f = match struct_inner.fields.get(offset as usize).map(|(_, v)| v) {
                        Some(Value::Float(f)) => *f,
                        Some(Value::Int(n)) => *n as f64,
                        _ => 0.0,
                    };
                    floats[dst_f as usize] = f;
                }
                Op::FlatGetF64 {
                    dst_f,
                    base,
                    index,
                    stride,
                    offset,
                } => unsafe {
                    let idx = match registers.get_unchecked(index as usize) {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let b = registers.get_unchecked(base as usize);
                    // Compiler-proven FloatArray: skip discriminant match.
                    let Value::FloatArray(fa_inner) = b else {
                        return Err(RuntimeError::Type(
                            "FlatGetF64: receiver lost flat invariant".to_string(),
                        ));
                    };
                    let pos = idx * stride as usize + offset as usize;
                    if pos >= fa_inner.data.len() {
                        return Err(RuntimeError::Arithmetic("index out of bounds".to_string()));
                    }
                    *floats.get_unchecked_mut(dst_f as usize) = *fa_inner.data.get_unchecked(pos);
                },
                Op::FlatSetF64 {
                    base,
                    index,
                    stride,
                    offset,
                    value_f,
                } => unsafe {
                    let idx = match registers.get_unchecked(index as usize) {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let new_f = *floats.get_unchecked(value_f as usize);
                    let b = registers.get_unchecked_mut(base as usize);
                    let Value::FloatArray(fa_arc) = b else {
                        return Err(RuntimeError::Type(
                            "FlatSetF64: receiver lost flat invariant".to_string(),
                        ));
                    };
                    let fa_inner = Arc::make_mut(fa_arc);
                    let pos = idx * stride as usize + offset as usize;
                    let buf = Arc::make_mut(&mut fa_inner.data);
                    if pos < buf.len() {
                        *buf.get_unchecked_mut(pos) = new_f;
                    } else {
                        // `v[i].field = x` out of range panics on the compiled
                        // tier (place-form bounds assert); match it here.
                        return Err(RuntimeError::Panic("index out of bounds".to_string()));
                    }
                },
                Op::FlatGetF64I {
                    dst_f,
                    base,
                    index_i,
                    stride,
                    offset,
                } => unsafe {
                    let raw = *ints.get_unchecked(index_i as usize);
                    if raw < 0 {
                        return Err(RuntimeError::Arithmetic(
                            "negative index into sequence".to_string(),
                        ));
                    }
                    let idx = raw as usize;
                    let b = registers.get_unchecked(base as usize);
                    let Value::FloatArray(fa_inner) = b else {
                        return Err(RuntimeError::Type(
                            "FlatGetF64I: receiver lost flat invariant".to_string(),
                        ));
                    };
                    let pos = idx * stride as usize + offset as usize;
                    if pos >= fa_inner.data.len() {
                        return Err(RuntimeError::Arithmetic("index out of bounds".to_string()));
                    }
                    *floats.get_unchecked_mut(dst_f as usize) = *fa_inner.data.get_unchecked(pos);
                },
                Op::FlatSetF64I {
                    base,
                    index_i,
                    stride,
                    offset,
                    value_f,
                } => unsafe {
                    let raw = *ints.get_unchecked(index_i as usize);
                    if raw < 0 {
                        return Err(RuntimeError::Arithmetic(
                            "negative index into sequence".to_string(),
                        ));
                    }
                    let idx = raw as usize;
                    let new_f = *floats.get_unchecked(value_f as usize);
                    let b = registers.get_unchecked_mut(base as usize);
                    let Value::FloatArray(fa_arc) = b else {
                        return Err(RuntimeError::Type(
                            "FlatSetF64I: receiver lost flat invariant".to_string(),
                        ));
                    };
                    let fa_inner = Arc::make_mut(fa_arc);
                    let pos = idx * stride as usize + offset as usize;
                    let buf = Arc::make_mut(&mut fa_inner.data);
                    if pos < buf.len() {
                        *buf.get_unchecked_mut(pos) = new_f;
                    } else {
                        // `v[i].field = x` out of range panics on the compiled
                        // tier (place-form bounds assert); match it here.
                        return Err(RuntimeError::Panic("index out of bounds".to_string()));
                    }
                },

                Op::BuildIntArray {
                    dst_v,
                    first_i,
                    count,
                } => {
                    let start = first_i as usize;
                    let end = start + count as usize;
                    let data: Vec<i64> = ints[start..end].to_vec();
                    registers[dst_v as usize] = Value::IntArray(Arc::new(data));
                }
                Op::IntToFloatF64 { dst_f, src_i } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) =
                        *ints.get_unchecked(src_i as usize) as f64;
                },
                Op::FloatToIntI64 { dst_i, src_f } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) =
                        *floats.get_unchecked(src_f as usize) as i64;
                },
                Op::TruncCastI64 {
                    dst_i,
                    src_i,
                    shift,
                    signed,
                } => unsafe {
                    let v = *ints.get_unchecked(src_i as usize);
                    let result = if signed {
                        // Arithmetic: shift left to fill MSB with sign bit,
                        // then arithmetic right shift back.
                        v.wrapping_shl(u32::from(shift))
                            .wrapping_shr(u32::from(shift))
                    } else {
                        // Logical: zero-extend by masking upper bits.
                        ((v as u64)
                            .wrapping_shl(u32::from(shift))
                            .wrapping_shr(u32::from(shift))) as i64
                    };
                    *ints.get_unchecked_mut(dst_i as usize) = result;
                },
                Op::I64ToUint { dst_v, src_i } => unsafe {
                    registers[dst_v as usize] =
                        Value::Uint(*ints.get_unchecked(src_i as usize) as u64);
                },
                Op::CastScalar { dst, src, target } => {
                    let v = &registers[src as usize];
                    let Some(result) = crate::cast::cast_scalar(v, target) else {
                        return Err(RuntimeError::Type(format!(
                            "cannot cast value of kind `{v}` to {target:?}"
                        )));
                    };
                    registers[dst as usize] = result;
                }
                Op::CellNew { dst, src } => {
                    let inner = registers[src as usize].clone();
                    registers[dst as usize] =
                        Value::MutCell(Arc::new(parking_lot::Mutex::new(inner)));
                }
                Op::CellNewMove { dst, src } => {
                    let inner = std::mem::replace(&mut registers[src as usize], Value::Unit);
                    registers[dst as usize] =
                        Value::MutCell(Arc::new(parking_lot::Mutex::new(inner)));
                }
                Op::CellTake { dst, cell } => {
                    // Last use of the cell, so move its inner out rather than
                    // clone - the caller re-homes it through this register.
                    let taken = match &registers[cell as usize] {
                        Value::MutCell(c) => std::mem::replace(&mut *c.lock(), Value::Unit),
                        other => other.clone(),
                    };
                    registers[dst as usize] = taken;
                }
                Op::BuildTuple { dst, first, count } => {
                    // Clones each value register into a fresh
                    // `Vec<Value>`, wraps in Arc, drops into
                    // `Value::Tuple`.
                    let n = count as usize;
                    let start = first as usize;
                    let mut items: Vec<Value> = Vec::with_capacity(n);
                    for i in 0..n {
                        items.push(registers[start + i].clone());
                    }
                    registers[dst as usize] = Value::Tuple(Arc::from(items));
                }
                Op::BuildArray { dst, first, count } => {
                    let n = count as usize;
                    let start = first as usize;
                    let mut items: Vec<Value> = Vec::with_capacity(n);
                    for i in 0..n {
                        items.push(registers[start + i].clone());
                    }
                    registers[dst as usize] = Value::Array(Arc::new(items));
                }
                Op::BuildArrayRepeat { dst, value, count } => {
                    let n = match &registers[count as usize] {
                        Value::Int(c) if *c >= 0 => *c as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Type("negative repeat count".to_string()));
                        }
                        _ => {
                            return Err(RuntimeError::Type("repeat count must be int".to_string()));
                        }
                    };
                    // A scalar repeat lands in flat typed storage - 8 bytes
                    // per element instead of a 16-byte boxed `Value` - the
                    // same routing `Op::BuildRange` and integer array
                    // literals use. Every consumer (indexing, `len`,
                    // iteration, the collection helpers) handles `IntArray` /
                    // `FloatVec` identically to a boxed array of the same
                    // scalars, so this is a pure representation change.
                    registers[dst as usize] = match &registers[value as usize] {
                        Value::Int(elem) => Value::IntArray(Arc::new(vec![*elem; n])),
                        Value::Float(elem) => Value::FloatVec(Arc::new(vec![*elem; n])),
                        elem => Value::Array(Arc::new(vec![elem.clone(); n])),
                    };
                }
                Op::BuildRange {
                    dst,
                    start,
                    end,
                    inclusive,
                } => {
                    // A non-`Int` bound degrades to `0` / `start` so a
                    // partially-typed program keeps running rather than
                    // trapping.
                    let start_val = match &registers[start as usize] {
                        Value::Int(n) => *n,
                        _ => 0,
                    };
                    let end_val = match &registers[end as usize] {
                        Value::Int(n) => *n,
                        _ => start_val,
                    };
                    // A materialised range is integer by construction, so
                    // it lands in flat `Value::IntArray` storage (8 bytes
                    // per element) rather than boxed `Value::Array` (16).
                    // Every consumer (indexing, `len`, iteration, the
                    // read-only collection helpers) handles `IntArray`
                    // identically to a boxed array of `Value::Int`.
                    let elems: Vec<i64> = if inclusive {
                        if end_val >= start_val {
                            (start_val..=end_val).collect()
                        } else {
                            Vec::new()
                        }
                    } else if end_val > start_val {
                        (start_val..end_val).collect()
                    } else {
                        Vec::new()
                    };
                    registers[dst as usize] = Value::IntArray(Arc::new(elems));
                }
                Op::VariantIs {
                    dst,
                    src,
                    name_idx,
                    arity,
                } => {
                    // Both names come from `intern_type_name`: one
                    // pointer compare replaces string content equality.
                    let expected: &'static str = chunk.shape_names[name_idx as usize];
                    let matches = match &registers[src as usize] {
                        Value::Variant(inner) => {
                            std::ptr::eq(inner.name, expected)
                                && inner.fields.len() == arity as usize
                        }
                        Value::NativeEnum(owner) => {
                            let disc = crate::value::native_enum_disc(owner.ptr, owner.shape);
                            owner.shape.variants.get(disc).is_some_and(|v| {
                                std::ptr::eq(v.name, expected) && v.fields.len() == arity as usize
                            })
                        }
                        _ => false,
                    };
                    registers[dst as usize] = Value::Bool(matches);
                }
                Op::VariantField { dst, src, idx } => {
                    registers[dst as usize] = match &registers[src as usize] {
                        Value::Variant(inner) => inner
                            .fields
                            .get(idx as usize)
                            .cloned()
                            .unwrap_or(Value::Unit),
                        Value::NativeEnum(owner) => {
                            crate::value::native_enum_field(owner, idx as usize)
                        }
                        _ => Value::Unit,
                    };
                }
                Op::StructIs { dst, src, name_idx } => {
                    let expected: &'static str = chunk.shape_names[name_idx as usize];
                    let matches = matches!(
                        &registers[src as usize],
                        Value::Struct(inner) if std::ptr::eq(inner.name, expected)
                    );
                    registers[dst as usize] = Value::Bool(matches);
                }
                Op::VariantFieldConsume { dst, src, idx } => {
                    // Drain the payload when the scrutinee is uniquely
                    // owned; clone (matching `VariantField`) when shared
                    // or when the value is not a `Variant`.
                    let result = match &mut registers[src as usize] {
                        Value::Variant(arc) => match Arc::get_mut(arc) {
                            Some(inner) => inner
                                .fields
                                .get_mut(idx as usize)
                                .map(|slot| std::mem::replace(slot, Value::Void))
                                .unwrap_or(Value::Unit),
                            None => arc.fields.get(idx as usize).cloned().unwrap_or(Value::Unit),
                        },
                        Value::NativeEnum(owner) => {
                            crate::value::native_enum_field(owner, idx as usize)
                        }
                        _ => Value::Unit,
                    };
                    registers[dst as usize] = result;
                }
                Op::IndexGetConsume { dst, base, index } => {
                    // Read the integer index before the mutable base
                    // borrow so the two register accesses don't overlap.
                    let idx_int = match &registers[index as usize] {
                        Value::Int(n) => Some(*n),
                        _ => None,
                    };
                    let result = match idx_int {
                        Some(raw) => index_get_consume(&mut registers[base as usize], raw)?,
                        None => {
                            let idx_val = registers[index as usize].clone();
                            index_get(&registers[base as usize], &idx_val)?
                        }
                    };
                    registers[dst as usize] = result;
                }
                Op::TupleIndexConsume {
                    dst,
                    receiver,
                    index,
                } => {
                    // Drain a uniquely-owned tuple / array field; clone
                    // (matching `TupleIndex`) when the aggregate is shared,
                    // and keep `TupleIndex`'s error semantics otherwise.
                    let idx = index as usize;
                    let oob = || RuntimeError::Arithmetic("tuple index out of bounds".to_string());
                    let result = match &mut registers[receiver as usize] {
                        Value::Tuple(arc) | Value::Array(arc) => match Arc::get_mut(arc) {
                            Some(items) => items
                                .get_mut(idx)
                                .map(|slot| std::mem::replace(slot, Value::Void))
                                .ok_or_else(oob)?,
                            None => arc.get(idx).cloned().ok_or_else(oob)?,
                        },
                        Value::Struct(inner) => inner
                            .fields
                            .get(idx)
                            .map(|(_, v)| v.clone())
                            .ok_or_else(oob)?,
                        other => {
                            return Err(RuntimeError::Type(format!(
                                "value of kind `{other}` has no tuple fields"
                            )));
                        }
                    };
                    registers[dst as usize] = result;
                }
                Op::IntArrayGetI64 {
                    dst_i,
                    base,
                    index_i,
                } => unsafe {
                    let idx = *ints.get_unchecked(index_i as usize);
                    let b = registers.get_unchecked(base as usize);
                    // `IntArrayGetI64` is the typed fast path that the
                    // bytecode compiler emits when `flat_int_locals`
                    // tracks the base register as an `IntArray`. On a
                    // call shape like `fn slide(arr: [i64; 4])`,
                    // `arr` is a parameter register whose tracking
                    // can outlive its actual `Value::IntArray`
                    // payload - e.g. when the caller passes a
                    // generic `Value::Array` (an ABI shape the
                    // call-args path doesn't typed-promote). Rather
                    // than panic, fall back to a generic array read
                    // when the receiver isn't the expected typed
                    // shape; the surrounding hot loop pays one
                    // discriminant match per index instead of
                    // aborting.
                    //
                    // An out-of-range index (negative or past the end) yields
                    // the lenient zero value, matching the generic `IndexGet`,
                    // the compiled tiers' bounds-guarded read, and the sibling
                    // `IntArraySetI64` no-op write.
                    let value = if idx < 0 {
                        0
                    } else {
                        let i = idx as usize;
                        match b {
                            Value::IntArray(data) => data.get(i).copied().unwrap_or(0),
                            Value::Array(items) => match items.get(i) {
                                Some(Value::Int(n)) => *n,
                                _ => 0,
                            },
                            Value::FloatVec(_) | Value::FloatArray(_) => {
                                return Err(RuntimeError::Type(
                                    "IntArrayGetI64: receiver is a float array".to_string(),
                                ));
                            }
                            _ => {
                                return Err(RuntimeError::Type(
                                    "IntArrayGetI64: receiver lost flat invariant".to_string(),
                                ));
                            }
                        }
                    };
                    *ints.get_unchecked_mut(dst_i as usize) = value;
                },
                Op::IntArraySetI64 {
                    base,
                    index_i,
                    value_i,
                } => unsafe {
                    let idx = *ints.get_unchecked(index_i as usize);
                    let new_val = *ints.get_unchecked(value_i as usize);
                    let b = registers.get_unchecked_mut(base as usize);
                    // Like `IntArrayGetI64`, the `flat_int_locals` tracking
                    // on a `&mut Vec<i64>` parameter can outlive the actual
                    // `Value::IntArray` payload when the caller passes a
                    // generic `Value::Array` (a struct-field or call-arg
                    // literal). Fall back to a generic element write rather
                    // than aborting the hot loop. An out-of-range index (negative
                    // or past the end) is a lenient no-op, matching the compiled
                    // tier's bounds-guarded store and the zero-value read contract.
                    match b {
                        Value::IntArray(data) => {
                            if idx >= 0 && (idx as usize) < data.len() {
                                *Arc::make_mut(data).get_unchecked_mut(idx as usize) = new_val;
                            }
                        }
                        Value::Array(items) => {
                            if idx >= 0 && (idx as usize) < items.len() {
                                Arc::make_mut(items)[idx as usize] = Value::Int(new_val);
                            }
                        }
                        _ => {
                            return Err(RuntimeError::Type(
                                "IntArraySetI64: receiver lost flat invariant".to_string(),
                            ));
                        }
                    }
                },
                Op::IntArraySwap { base, i_i, j_i } => unsafe {
                    let i_idx = *ints.get_unchecked(i_i as usize);
                    let j_idx = *ints.get_unchecked(j_i as usize);
                    if i_idx < 0 || j_idx < 0 {
                        return Err(RuntimeError::Arithmetic(
                            "negative index into sequence".to_string(),
                        ));
                    }
                    let i = i_idx as usize;
                    let j = j_idx as usize;
                    let b = registers.get_unchecked_mut(base as usize);
                    let Value::IntArray(data) = b else {
                        return Err(RuntimeError::Type(
                            "IntArraySwap: receiver lost flat invariant".to_string(),
                        ));
                    };
                    let v = Arc::make_mut(data);
                    if i >= v.len() || j >= v.len() {
                        return Err(RuntimeError::Arithmetic("index out of bounds".to_string()));
                    }
                    v.swap(i, j);
                },
                Op::FloatVecSwap { base, i_i, j_i } => unsafe {
                    let i_idx = *ints.get_unchecked(i_i as usize);
                    let j_idx = *ints.get_unchecked(j_i as usize);
                    if i_idx < 0 || j_idx < 0 {
                        return Err(RuntimeError::Arithmetic(
                            "negative index into sequence".to_string(),
                        ));
                    }
                    let i = i_idx as usize;
                    let j = j_idx as usize;
                    let b = registers.get_unchecked_mut(base as usize);
                    let Value::FloatVec(data) = b else {
                        return Err(RuntimeError::Type(
                            "FloatVecSwap: receiver lost flat invariant".to_string(),
                        ));
                    };
                    let v = Arc::make_mut(data);
                    if i >= v.len() || j >= v.len() {
                        return Err(RuntimeError::Arithmetic("index out of bounds".to_string()));
                    }
                    v.swap(i, j);
                },
                Op::BuildFloatVec {
                    dst_v,
                    first_f,
                    count,
                } => {
                    let n = count as usize;
                    let start = first_f as usize;
                    let mut data: Vec<f64> = Vec::with_capacity(n);
                    // SAFETY: `first_f .. first_f + count` is a
                    // compile-allocated span in the float register
                    // file (mirrors `BuildIntArray`).
                    unsafe {
                        for i in 0..n {
                            data.push(*floats.get_unchecked(start + i));
                        }
                    }
                    registers[dst_v as usize] = Value::FloatVec(Arc::new(data));
                }
                Op::FloatVecGetF64 {
                    dst_f,
                    base,
                    index_i,
                } => unsafe {
                    let idx = *ints.get_unchecked(index_i as usize);
                    let b = registers.get_unchecked(base as usize);
                    // Tolerate generic `Value::Array(Vec<Value::Float>)`
                    // alongside `Value::FloatVec(Vec<f64>)` - same
                    // tracking-vs-actual-shape skew the IntArray fast
                    // path has to handle when a typed receiver passes
                    // through a non-promoting ABI boundary.
                    //
                    // An out-of-range index (negative or past the end) yields
                    // the lenient zero value, matching the generic `IndexGet`,
                    // the compiled tiers' bounds-guarded read, and the sibling
                    // `FloatVecSetF64` no-op write.
                    let value = if idx < 0 {
                        0.0
                    } else {
                        let i = idx as usize;
                        match b {
                            Value::FloatVec(data) => data.get(i).copied().unwrap_or(0.0),
                            Value::Array(items) => match items.get(i) {
                                Some(Value::Float(f)) => *f,
                                _ => 0.0,
                            },
                            _ => {
                                return Err(RuntimeError::Type(
                                    "FloatVecGetF64: receiver lost flat invariant".to_string(),
                                ));
                            }
                        }
                    };
                    *floats.get_unchecked_mut(dst_f as usize) = value;
                },
                Op::FloatVecSetF64 {
                    base,
                    index_i,
                    value_f,
                } => unsafe {
                    let idx = *ints.get_unchecked(index_i as usize);
                    let new_f = *floats.get_unchecked(value_f as usize);
                    let b = registers.get_unchecked_mut(base as usize);
                    let Value::FloatVec(data_arc) = b else {
                        return Err(RuntimeError::Type(
                            "FloatVecSetF64: receiver lost flat invariant".to_string(),
                        ));
                    };
                    // Out-of-range whole-element write is a lenient no-op, like
                    // the compiled tier's bounds-guarded store.
                    if idx >= 0 {
                        let buf = Arc::make_mut(data_arc);
                        let i = idx as usize;
                        if i < buf.len() {
                            *buf.get_unchecked_mut(i) = new_f;
                        }
                    }
                },
                Op::BuildIntMap { dst_v } => {
                    registers[dst_v as usize] = Value::IntMap(Arc::new(parking_lot::Mutex::new(
                        rustc_hash::FxHashMap::with_capacity_and_hasher(
                            16,
                            rustc_hash::FxBuildHasher,
                        ),
                    )));
                }
                Op::BuildStrIntMap { dst_v } => {
                    registers[dst_v as usize] = Value::StrIntMap(Arc::new(
                        parking_lot::Mutex::new(rustc_hash::FxHashMap::with_capacity_and_hasher(
                            16,
                            rustc_hash::FxBuildHasher,
                        )),
                    ));
                }
                Op::IntMapInc {
                    dst_i,
                    map_reg,
                    key_i,
                    by_i,
                } => unsafe {
                    let key = *ints.get_unchecked(key_i as usize);
                    let by = *ints.get_unchecked(by_i as usize);
                    let m = registers.get_unchecked(map_reg as usize);
                    let Value::IntMap(map) = m else {
                        return Err(RuntimeError::Type(
                            "IntMapInc: receiver lost typed invariant".to_string(),
                        ));
                    };
                    let mut guard = map.lock();
                    let entry = guard.entry(key).or_insert(0);
                    *entry += by;
                    let post = *entry;
                    drop(guard);
                    *ints.get_unchecked_mut(dst_i as usize) = post;
                },
                Op::IntMapGetOr {
                    dst_i,
                    map_reg,
                    key_i,
                    default_i,
                } => unsafe {
                    let key = *ints.get_unchecked(key_i as usize);
                    let default = *ints.get_unchecked(default_i as usize);
                    let m = registers.get_unchecked(map_reg as usize);
                    let Value::IntMap(map) = m else {
                        return Err(RuntimeError::Type(
                            "IntMapGetOr: receiver lost typed invariant".to_string(),
                        ));
                    };
                    let v = map.lock().get(&key).copied().unwrap_or(default);
                    *ints.get_unchecked_mut(dst_i as usize) = v;
                },
                Op::IntMapInsert {
                    dst_v,
                    map_reg,
                    key_i,
                    value_i,
                } => unsafe {
                    let key = *ints.get_unchecked(key_i as usize);
                    let val = *ints.get_unchecked(value_i as usize);
                    let m = registers.get_unchecked(map_reg as usize);
                    let Value::IntMap(map) = m else {
                        return Err(RuntimeError::Type(
                            "IntMapInsert: receiver lost typed invariant".to_string(),
                        ));
                    };
                    map.lock().insert(key, val);
                    let cloned = Arc::clone(map);
                    registers[dst_v as usize] = Value::IntMap(cloned);
                },
                Op::IntMapLen { dst_i, map_reg } => unsafe {
                    let m = registers.get_unchecked(map_reg as usize);
                    let Value::IntMap(map) = m else {
                        return Err(RuntimeError::Type(
                            "IntMapLen: receiver lost typed invariant".to_string(),
                        ));
                    };
                    let n = map.lock().len() as i64;
                    *ints.get_unchecked_mut(dst_i as usize) = n;
                },
                Op::IntMapContainsKey {
                    dst_v,
                    map_reg,
                    key_i,
                } => unsafe {
                    let key = *ints.get_unchecked(key_i as usize);
                    let m = registers.get_unchecked(map_reg as usize);
                    let Value::IntMap(map) = m else {
                        return Err(RuntimeError::Type(
                            "IntMapContainsKey: receiver lost typed invariant".to_string(),
                        ));
                    };
                    let has = map.lock().contains_key(&key);
                    registers[dst_v as usize] = Value::Bool(has);
                },
                Op::Spawn { callee, args, argc } => {
                    let callee_val = registers[callee as usize].clone();
                    let arg_values: Vec<Value> = (0..argc as usize)
                        .map(|i| registers[args as usize + i].clone())
                        .collect();
                    self.spawn_goroutine_native(callee_val, arg_values);
                }
                Op::SpawnMethod {
                    receiver,
                    name_idx,
                    args,
                    argc,
                } => {
                    let recv = registers[receiver as usize].clone();
                    let name = chunk.globals[name_idx as usize].as_str();
                    // Resolve method dispatch the same way `Op::MethodCall`
                    // does (qualified key first, bare name fallback). The
                    // resolved global is what the spawned goroutine
                    // applies; the receiver is prepended to the arg
                    // vector so the callee sees `[receiver, a0, a1, …]`.
                    let resolved = qualified_key(&recv, name)
                        .and_then(|qual| self.lookup_global(qual))
                        .or_else(|| self.lookup_global(name));
                    let mut arg_values: Vec<Value> = Vec::with_capacity(argc as usize + 1);
                    arg_values.push(recv);
                    for i in 0..argc as usize {
                        arg_values.push(registers[args as usize + i].clone());
                    }
                    let callee_val = match resolved {
                        Some(Global::Value(v)) => v,
                        Some(Global::MutStatic(cell)) => cell.lock().clone(),
                        Some(Global::Fn(_)) => Value::String(SmolStr::from(name.to_string())),
                        None => {
                            return Err(RuntimeError::UnresolvedName(name.to_string()));
                        }
                    };
                    self.spawn_goroutine_native(callee_val, arg_values);
                }
            }
        }
    }
}

/// Publishes the final values of `&mut Vec<T>` / `&mut [T]` parameter
/// registers back into their caller-provided write-back cells. Runs
/// on every successful return path of [`Vm::run`]; error unwinds skip
/// it (the caller's aggregate keeps its pre-call value, matching the
/// compiled tiers' no-partial-publish behaviour for panics).
/// Typed-storage promotion for a scalar push. A first `i64` / `f64` push
/// into an empty generic `Array` switches the vector to flat typed storage
/// (`IntArray` / `FloatVec`, 8 bytes per element instead of a 16-byte boxed
/// `Value`), so a push-built scalar vec costs the same as a literal-built
/// one. A float push onto an `IntArray` widens it to `FloatVec`: an `[i64]`
/// can never receive a float, so the receiver is an `[f64]` whose elements
/// so far were integer-valued. Returns the replacement value, or `None`
/// when the ordinary push applies.
fn promote_scalar_push(recv: &Value, new_value: &Value) -> Option<Value> {
    match (recv, new_value) {
        (Value::Array(items), Value::Int(n)) if items.is_empty() => {
            Some(Value::IntArray(Arc::new(vec![*n])))
        }
        (Value::Array(items), Value::Float(f)) if items.is_empty() => {
            Some(Value::FloatVec(Arc::new(vec![*f])))
        }
        (Value::IntArray(data), Value::Float(f)) => {
            let mut wide: Vec<f64> = data.iter().map(|n| *n as f64).collect();
            wide.push(*f);
            Some(Value::FloatVec(Arc::new(wide)))
        }
        _ => None,
    }
}

fn publish_ref_cells(cells: &[(usize, Arc<parking_lot::Mutex<Value>>)], registers: &mut [Value]) {
    // Move (not clone) each `&mut` param's final value back into its cell.
    // Cloning would leave the value referenced by both the cell and the
    // returning frame's register, so the caller's `CellTake` would receive
    // a shared value - and a subsequent in-place mutation (`v.push` /
    // `*s += …`) would copy-on-write the whole collection on every call,
    // turning a build loop into O(n^2). The frame is exiting, so emptying
    // the register is safe.
    for (slot, cell) in cells {
        *cell.lock() = std::mem::replace(&mut registers[*slot], Value::Unit);
    }
}

/// One non-blocking poll over every `select` arm in source order.
/// Returns the chosen arm's `body_block` - writing a received value
/// into the recv arm's `bind_reg`, or completing a send - or `None`
/// when no arm (including `default`) is ready. The two-pass scan
/// (recv/send first, then `default`) chooses a `default` arm only once
/// every recv/send arm has been found not-ready.
fn select_try_once(
    arms: &[crate::bytecode::SelectArmMeta],
    registers: &mut [Value],
) -> Option<crate::bytecode::InstrIdx> {
    use crate::bytecode::SelectArmKind;
    for arm in arms {
        match arm.kind {
            SelectArmKind::Recv => {
                let Value::Channel(ch) = &registers[arm.channel_reg as usize] else {
                    continue;
                };
                let ch = ch.clone();
                if let Some(value) = ch.try_recv() {
                    registers[arm.bind_reg as usize] = value;
                    return Some(arm.body_block);
                }
                // Go semantics: a recv arm on a closed (and drained)
                // channel is always ready, binding the element-type
                // zero value. The 8-byte select payload contract is
                // i64-shaped on every tier, so the VM mirrors the
                // compiled tier's `last_value = 0`.
                if ch.is_closed() {
                    registers[arm.bind_reg as usize] = Value::Int(0);
                    return Some(arm.body_block);
                }
            }
            SelectArmKind::Send => {
                let Value::Channel(ch) = &registers[arm.channel_reg as usize] else {
                    continue;
                };
                let ch = ch.clone();
                let value = registers[arm.value_reg as usize].clone();
                // A bounded channel at capacity makes this send arm
                // not-ready; fall through to the next arm rather than
                // blocking inside the non-blocking probe.
                if ch.try_send(value) {
                    return Some(arm.body_block);
                }
            }
            SelectArmKind::Default => {}
        }
    }
    for arm in arms {
        if arm.kind == SelectArmKind::Default {
            return Some(arm.body_block);
        }
    }
    None
}

/// Polls every `select` arm, parking on the receive arms' condvar when
/// nothing is ready and no `default` exists, and re-polling on each
/// wake. Returns the chosen arm's `body_block`. Blocking semantics over
/// `Value::Channel` - `Channel::send`/`close` notify every waiter, so
/// the first push wakes the park; the bounded wait keeps a missed
/// notify from stranding the goroutine, and a (spec-disallowed)
/// send-only select with no default yields briefly rather than
/// busy-spinning.
fn select_dispatch(
    arms: &[crate::bytecode::SelectArmMeta],
    registers: &mut [Value],
) -> crate::bytecode::InstrIdx {
    use crate::bytecode::SelectArmKind;
    use std::time::Duration;
    if let Some(target) = select_try_once(arms, registers) {
        return target;
    }
    let recv_channels: Vec<crate::value::Channel> = arms
        .iter()
        .filter(|a| a.kind == SelectArmKind::Recv)
        .filter_map(|a| match &registers[a.channel_reg as usize] {
            Value::Channel(ch) => Some(ch.clone()),
            _ => None,
        })
        .collect();
    loop {
        if let Some(target) = select_try_once(arms, registers) {
            return target;
        }
        if recv_channels.is_empty() {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        let _ = recv_channels[0].wait_for(Duration::from_millis(50));
    }
}
