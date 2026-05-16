#![allow(
    clippy::too_many_lines,
    reason = "VM dispatch loop — see vm/run.rs roadmap for arm-group decomp"
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
        // the pool's `args` free list — most arg Vecs are
        // pool-borrowed in `Op::Call`, and reclaiming them here
        // closes the loop without an extra allocation per call.
        let mut args = args;
        for (i, arg) in args.drain(..).enumerate() {
            registers[i] = arg;
        }
        self.pool.borrow_mut().give_args(args);
        let mut pc: u32 = 0;
        let instrs: &[Op] = &chunk.instrs;
        let instr_count = instrs.len();
        loop {
            // SAFETY: every chunk emitted by `compile.rs` ends
            // with a `Return` / `ReturnUnit`, and every jump /
            // branch target is computed from the same emit-
            // counter that placed the op — so `pc` can never
            // exceed `instr_count` at this point. We keep a
            // `debug_assert!` so a corrupted chunk fails loudly
            // in debug builds, but skip the runtime branch in
            // release. `Op` is `Copy`, so dereferencing gives
            // us a by-value copy of the enum for destructuring
            // without invoking `<Op as Clone>::clone`.
            debug_assert!((pc as usize) < instr_count, "fell off end of bytecode");
            let _ = instr_count;
            let op = unsafe { *instrs.get_unchecked(pc as usize) };
            pc += 1;
            match op {
                Op::LoadConst { dst, idx } => {
                    registers[dst as usize] = chunk.consts[idx as usize].clone();
                }
                Op::LoadGlobal { dst, idx } => {
                    let name = &chunk.globals[idx as usize];
                    let value = match self.globals.get(name) {
                        Some(Global::Value(v)) => v.clone(),
                        Some(Global::Fn(_)) => Value::String(SmolStr::from(name.clone())),
                        None => return Err(RuntimeError::UnresolvedName(name.clone())),
                    };
                    let _ = self.resolve_global(name)?;
                    registers[dst as usize] = value;
                }
                Op::Move { dst, src } => {
                    registers[dst as usize] = registers[src as usize].clone();
                }
                Op::Deref { dst, src } => {
                    let v = registers[src as usize].clone();
                    let resolved = if let Value::Struct(inner) = &v {
                        if inner.name == "__Cell" {
                            let mut set_id: u64 = 0;
                            let mut flag_name = String::new();
                            for (ident, val) in inner.fields.iter() {
                                if ident.name == "__set_id"
                                    && let Value::Int(n) = val
                                {
                                    set_id = *n as u64;
                                }
                                if ident.name == "__flag_name"
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
                } => {
                    let argc_usz = argc as usize;
                    let mut arg_values = self.pool.borrow_mut().take_args(argc_usz);
                    for i in 0..argc_usz {
                        // 0.7.0 flag::Cell auto-deref at the call
                        // boundary: `f(flags.output)` passes the
                        // current backing value instead of the
                        // `__Cell` handle. Matches Rust's `Deref`
                        // coercion on fn-arg sites.
                        let raw = registers[args as usize + i].clone();
                        arg_values.push(auto_deref_cell(&raw).unwrap_or(raw));
                    }
                    let callee_val = &registers[callee as usize];
                    // Inline-cache probe. The slot is keyed by the
                    // *callee* identity (the resolved name for a
                    // `Value::String(SmolStr::from("foo"))` callee). Cache hit
                    // skips the `self.globals.get(name)` HashMap
                    // probe — typically the dominant cost in tight
                    // loops calling small helper functions.
                    let token = call_token(callee_val);
                    let live_generation = self.globals_generation();
                    let cached: Option<Global> = if token != 0 {
                        // Read-only borrow on the IC hit path; the
                        // fill (miss) path below takes borrow_mut.
                        // Splitting avoids serialising every cached
                        // hit against any concurrent borrow on the
                        // same RefCell.
                        let cache = state.call_caches.borrow();
                        let slot = &cache[cache_idx as usize];
                        if slot.type_token == token && slot.generation == live_generation {
                            // Same two-tier shape as MethodCall: fast
                            // builtin-fn-ptr first, then resolved chunk.
                            if let Some(call_fn) = slot.builtin_fn {
                                Some(Global::Value(Value::builtin("<cached>", call_fn)))
                            } else {
                                slot.fn_chunk.as_ref().map(|c| Global::Fn(Arc::clone(c)))
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let result = if let Some(g) = cached {
                        self.apply(g, arg_values)?
                    } else if token != 0 {
                        // Miss: do the full dispatch and write back.
                        let resolved_global = match callee_val {
                            Value::String(name) => self.globals.get(name.as_str()).cloned(),
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
                Op::Return { value } => return Ok(registers[value as usize].clone()),
                Op::ReturnUnit => return Ok(Value::Unit),
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
                            // call boundary — same rule as `Op::Call`.
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
                                .and_then(|qual: &str| self.globals.get(qual).cloned())
                                .or_else(|| self.globals.get(name.as_str()).cloned());
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
                            // call boundary — same rule as `Op::Call`.
                            let raw = registers[args as usize + i].clone();
                            call_args.push(auto_deref_cell(&raw).unwrap_or(raw));
                        }
                        if let Some(call_fn) = cached_builtin {
                            call_fn(&call_args)?
                        } else if let Some(g) = cached {
                            self.apply(g, call_args)?
                        } else {
                            let r = qualified_key(&call_args[0], name)
                                .and_then(|qual: &str| self.globals.get(qual).cloned())
                                .or_else(|| self.globals.get(name.as_str()).cloned());
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
                                for (n, v) in inner.fields.iter() {
                                    if n.name == "fd" {
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
                                    .and_then(|q| self.globals.get(q).cloned())
                            }
                            _ => None,
                        }
                        .or_else(|| self.globals.get("write_byte").cloned());
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
                    // dropped (extremely rare — U8Vec is held by
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
                                for (n, v) in inner.fields.iter() {
                                    if n.name == "handle" {
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
                            .and_then(|q| self.globals.get(q).cloned()),
                        _ => None,
                    }
                    .or_else(|| self.globals.get("set_byte").cloned());
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
                                for (n, v) in inner.fields.iter() {
                                    if n.name == "handle" {
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
                            .and_then(|q| self.globals.get(q).cloned()),
                        _ => None,
                    }
                    .or_else(|| self.globals.get("get_byte").cloned());
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
                Op::IndexSet { base, index, value } => {
                    let new_value = registers[value as usize].clone();
                    let i = &registers[index as usize];
                    let idx = match i {
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
                    let b = &mut registers[base as usize];
                    match b {
                        Value::Array(items) | Value::Tuple(items) => {
                            let v = Arc::make_mut(items);
                            if idx >= v.len() {
                                return Err(RuntimeError::Arithmetic(
                                    "index out of bounds".to_string(),
                                ));
                            }
                            v[idx] = new_value;
                        }
                        Value::IntArray(data) => {
                            // Mutate the underlying `Vec<i64>` in place.
                            // `Arc::make_mut` clones if shared (rare —
                            // a fresh `BuildIntArray` usually leaves
                            // the array uniquely owned).
                            let v = Arc::make_mut(data);
                            if idx >= v.len() {
                                return Err(RuntimeError::Arithmetic(
                                    "index out of bounds".to_string(),
                                ));
                            }
                            match new_value {
                                Value::Int(n) => v[idx] = n,
                                _ => {
                                    return Err(RuntimeError::Type(
                                        "IndexSet on IntArray expects i64 value".to_string(),
                                    ));
                                }
                            }
                        }
                        Value::FloatVec(data) => {
                            let v = Arc::make_mut(data);
                            if idx >= v.len() {
                                return Err(RuntimeError::Arithmetic(
                                    "index out of bounds".to_string(),
                                ));
                            }
                            match new_value {
                                Value::Float(f) => v[idx] = f,
                                Value::Int(n) => v[idx] = n as f64,
                                _ => {
                                    return Err(RuntimeError::Type(
                                        "IndexSet on FloatVec expects f64 value".to_string(),
                                    ));
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
                    // straight to `inner.fields[offset].1.clone()` —
                    // skipping the linear name-scan in `field_get`.
                    let recv = &registers[receiver as usize];
                    if let Value::Struct(inner) = recv {
                        let token = intern_type_name(inner.name).as_ptr() as u64;
                        let slot = &state.field_caches[cache_idx as usize];
                        if slot.type_token.get() == token {
                            let off = slot.offset.get() as usize;
                            if off < inner.fields.len() {
                                registers[dst as usize] = inner.fields[off].1.clone();
                                continue;
                            }
                        }
                        // Miss: linear-scan, then refill the slot
                        // for next time. `intern_type_name` returns
                        // the same `&'static str` for any struct
                        // sharing this name, so the token compare
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
                            .position(|(ident, _)| ident.name == field_name)
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
                Op::TupleIndex {
                    dst,
                    receiver,
                    index,
                } => {
                    let recv = &registers[receiver as usize];
                    let idx = index as usize;
                    registers[dst as usize] = match recv {
                        Value::Tuple(items) | Value::Array(items) => {
                            items.get(idx).cloned().ok_or_else(|| {
                                RuntimeError::Arithmetic("tuple index out of bounds".to_string())
                            })?
                        }
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
                    registers[dst as usize] = match recv {
                        Value::Tuple(items) | Value::Array(items) => {
                            let len = items.len();
                            let idx = len.saturating_sub(offset_from_end as usize + 1);
                            items.get(idx).cloned().ok_or_else(|| {
                                RuntimeError::Arithmetic(
                                    "tuple tail index out of bounds".to_string(),
                                )
                            })?
                        }
                        _ => {
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
                        RuntimeError::Arithmetic("index out of bounds".to_string())
                    })?;
                    let Value::Struct(struct_arc) = slot else {
                        return Err(RuntimeError::Type(format!(
                            "cannot assign to field `{field_name}` on non-struct"
                        )));
                    };
                    let struct_inner = Arc::make_mut(struct_arc);
                    let field_slots = Arc::make_mut(&mut struct_inner.fields);
                    let pos = field_slots
                        .iter()
                        .position(|(ident, _)| ident.name == field_name);
                    if let Some(p) = pos {
                        field_slots[p].1 = new_value;
                    } else {
                        field_slots.push((Ident::new(field_name), new_value));
                    }
                }
                Op::EvalDeferred { dst, expr_idx } => {
                    let idx = expr_idx as usize;
                    let expr = &chunk.deferred_exprs[idx];
                    let names = &chunk.deferred_envs[idx];
                    let regs = &chunk.deferred_env_regs[idx];
                    let mut env_values: Vec<(String, Value)> = Vec::with_capacity(regs.len());
                    for (i, reg) in regs.iter().enumerate() {
                        let value = registers[*reg as usize].clone();
                        let name = names.get(i).cloned().unwrap_or_default();
                        env_values.push((name, value));
                    }
                    let (result, is_return, updated) = self
                        .walker
                        .borrow_mut()
                        .eval_standalone_with_flow(expr, &env_values)?;
                    for (reg, value) in regs.iter().zip(updated) {
                        registers[*reg as usize] = value;
                    }
                    if is_return {
                        // Walker took an early-return path (e.g. the
                        // `?` desugar's `Err(_) => return Err(_)`
                        // arm). Surface as the chunk's return so
                        // the caller sees a real unwind, not a
                        // value collapsed into the next register.
                        return Ok(result);
                    }
                    registers[dst as usize] = result;
                }

                // ----- Phase 1 typed ops -----
                //
                // All float/int register accesses use
                // `get_unchecked` — the register slot index is
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
                        return Err(RuntimeError::Arithmetic("division by zero".to_string()));
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
                        return Err(RuntimeError::Arithmetic("remainder by zero".to_string()));
                    }
                    ints[dst_i as usize] = ints[lhs_i as usize].wrapping_rem(r);
                }
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
                Op::UnboxI64 { dst_i, src_v } => {
                    let v = &registers[src_v as usize];
                    // 0.7.0 flag::Cell auto-deref. The typechecker
                    // pins `flags.number` as `Cell<i64>` and emits
                    // an UnboxI64 hoping to find a `Value::Int`;
                    // without this branch the cell handle hits the
                    // type-error arm and aborts.
                    let n = match v {
                        Value::Int(n) => *n,
                        Value::Struct(inner) if inner.name == "__Cell" => {
                            match auto_deref_cell(v) {
                                Some(Value::Int(n)) => n,
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
                    for (ident, v) in struct_inner.fields.iter() {
                        if ident.name == field_name.as_str() {
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
                    let (Value::Array(items) | Value::Tuple(items)) = b else {
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
                    for (ident, v) in struct_inner.fields.iter() {
                        if ident.name == field_name.as_str() {
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
                    // FloatArray fast path — resolve the field
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
                    let (Value::Array(items) | Value::Tuple(items)) = b else {
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
                    for (ident, v) in struct_inner.fields.iter() {
                        if ident.name == field_name.as_str() {
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
                        RuntimeError::Arithmetic("index out of bounds".to_string())
                    })?;
                    let Value::Struct(struct_arc) = slot else {
                        return Err(RuntimeError::Type(format!(
                            "cannot assign to field `{field_name}` on non-struct"
                        )));
                    };
                    let struct_inner = Arc::make_mut(struct_arc);
                    let field_slots = Arc::make_mut(&mut struct_inner.fields);
                    let pos = field_slots
                        .iter()
                        .position(|(ident, _)| ident.name == field_name);
                    if let Some(p) = pos {
                        field_slots[p].1 = new_value;
                    } else {
                        field_slots.push((Ident::new(field_name), new_value));
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
                        let (Value::Array(items) | Value::Tuple(items)) = b else {
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
                    // — still one acquire-load per write, but no
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
                        let field_slots = Arc::make_mut(&mut struct_inner.fields);
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
                Op::BuildTuple { dst, first, count } => {
                    // Native counterpart to the deferred-walker
                    // path. Clones each value register into a
                    // fresh `Vec<Value>`, wraps in Arc, drops
                    // into `Value::Tuple`. No env reconstruction,
                    // no walker re-entry.
                    let n = count as usize;
                    let start = first as usize;
                    let mut items: Vec<Value> = Vec::with_capacity(n);
                    for i in 0..n {
                        items.push(registers[start + i].clone());
                    }
                    registers[dst as usize] = Value::Tuple(Arc::new(items));
                }
                Op::IntArrayGetI64 {
                    dst_i,
                    base,
                    index_i,
                } => unsafe {
                    let idx = *ints.get_unchecked(index_i as usize);
                    if idx < 0 {
                        return Err(RuntimeError::Arithmetic(
                            "negative index into sequence".to_string(),
                        ));
                    }
                    let i = idx as usize;
                    let b = registers.get_unchecked(base as usize);
                    let Value::IntArray(data) = b else {
                        return Err(RuntimeError::Type(
                            "IntArrayGetI64: receiver lost flat invariant".to_string(),
                        ));
                    };
                    if i >= data.len() {
                        return Err(RuntimeError::Arithmetic("index out of bounds".to_string()));
                    }
                    *ints.get_unchecked_mut(dst_i as usize) = *data.get_unchecked(i);
                },
                Op::IntArraySetI64 {
                    base,
                    index_i,
                    value_i,
                } => unsafe {
                    let idx = *ints.get_unchecked(index_i as usize);
                    if idx < 0 {
                        return Err(RuntimeError::Arithmetic(
                            "negative index into sequence".to_string(),
                        ));
                    }
                    let i = idx as usize;
                    let new_val = *ints.get_unchecked(value_i as usize);
                    let b = registers.get_unchecked_mut(base as usize);
                    let Value::IntArray(data) = b else {
                        return Err(RuntimeError::Type(
                            "IntArraySetI64: receiver lost flat invariant".to_string(),
                        ));
                    };
                    let v = Arc::make_mut(data);
                    if i >= v.len() {
                        return Err(RuntimeError::Arithmetic("index out of bounds".to_string()));
                    }
                    *v.get_unchecked_mut(i) = new_val;
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
                    if idx < 0 {
                        return Err(RuntimeError::Arithmetic(
                            "negative index into sequence".to_string(),
                        ));
                    }
                    let i = idx as usize;
                    let b = registers.get_unchecked(base as usize);
                    let Value::FloatVec(data) = b else {
                        return Err(RuntimeError::Type(
                            "FloatVecGetF64: receiver lost flat invariant".to_string(),
                        ));
                    };
                    if i >= data.len() {
                        return Err(RuntimeError::Arithmetic("index out of bounds".to_string()));
                    }
                    *floats.get_unchecked_mut(dst_f as usize) = *data.get_unchecked(i);
                },
                Op::FloatVecSetF64 {
                    base,
                    index_i,
                    value_f,
                } => unsafe {
                    let idx = *ints.get_unchecked(index_i as usize);
                    if idx < 0 {
                        return Err(RuntimeError::Arithmetic(
                            "negative index into sequence".to_string(),
                        ));
                    }
                    let i = idx as usize;
                    let new_f = *floats.get_unchecked(value_f as usize);
                    let b = registers.get_unchecked_mut(base as usize);
                    let Value::FloatVec(data_arc) = b else {
                        return Err(RuntimeError::Type(
                            "FloatVecSetF64: receiver lost flat invariant".to_string(),
                        ));
                    };
                    let buf = Arc::make_mut(data_arc);
                    if i >= buf.len() {
                        return Err(RuntimeError::Arithmetic("index out of bounds".to_string()));
                    }
                    *buf.get_unchecked_mut(i) = new_f;
                },
                Op::BuildIntMap { dst_v } => {
                    registers[dst_v as usize] = Value::IntMap(Arc::new(parking_lot::Mutex::new(
                        rustc_hash::FxHashMap::with_capacity_and_hasher(
                            16,
                            rustc_hash::FxBuildHasher,
                        ),
                    )));
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
                        .and_then(|qual| self.globals.get(qual).cloned())
                        .or_else(|| self.globals.get(name).cloned());
                    let mut arg_values: Vec<Value> = Vec::with_capacity(argc as usize + 1);
                    arg_values.push(recv);
                    for i in 0..argc as usize {
                        arg_values.push(registers[args as usize + i].clone());
                    }
                    let callee_val = match resolved {
                        Some(Global::Value(v)) => v,
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
