#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::if_not_else)]
#![allow(clippy::single_match_else)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::redundant_else)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::large_enum_variant)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::single_match)]
#![allow(clippy::useless_conversion)]

use std::collections::HashMap;

use gossamer_ast::Ident;
use gossamer_hir::{
    HirAdtKind, HirBinaryOp, HirBlock, HirExpr, HirExprKind, HirFn, HirItem, HirItemKind,
    HirLiteral, HirMatchArm, HirPat, HirPatKind, HirProgram, HirStmt, HirStmtKind, HirUnaryOp,
};
use gossamer_lex::Span;
use gossamer_types::{Ty, TyCtxt};

use crate::ir::{
    BasicBlock, BinOp, BlockId, Body, ConstValue, Local, LocalDecl, Operand, Place, Rvalue,
    Statement, StatementKind, Terminator, UnOp,
};

use super::*;

use super::Builder;

impl<'a> Builder<'a> {
    /// Dispatches a method call onto its `gos_rt_*` runtime helper
    /// resolved by [`Self::lower_method_call`]. Extracted to keep
    /// `lower_method_call` itself under the file-size budget.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_lines)]
    pub(crate) fn dispatch_via_runtime_symbol(
        &mut self,
        sym: &'static str,
        receiver: &HirExpr,
        method: &Ident,
        _args: &[HirExpr],
        ty: Ty,
        span: Span,
        receiver_local: Local,
        arg_operands: Vec<Operand>,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        // Re-derive locals the original lower_method_call computed
        // earlier in its body - this helper was extracted from the
        // tail of that function and the captured-state plumbing is
        // simpler when the helper re-derives from what it already
        // has (receiver / receiver_local).
        let receiver_ty = self
            .receiver_local_from_path(receiver)
            .map_or(receiver.ty, |local| self.locals[local.0 as usize].ty);
        // A `<parent>.<field>` receiver keeps the parent struct's declared
        // field type as ground truth: the HIR field-access type can be left
        // degraded (a match-payload binding loses a `HashMap<String, _>`
        // field's substitution), which the key/value-typed dest computations
        // below - the `gos_rt_map_keys_vec` / `_values_vec` element type - read
        // as `i64`, formatting string keys through the integer formatter.
        let receiver_ty = self.field_declared_ty(receiver).unwrap_or(receiver_ty);
        let lowered_recv_ty = self.locals[receiver_local.0 as usize].ty;
        if sym.is_empty() {
            // Identity method - just copy the receiver to the
            // destination. Lets `"lit".to_string()` lower
            // without involving the runtime.
            //
            // Pin the destination's MIR type to the receiver's
            // own type rather than the method-call expression's
            // (often still unresolved) inference variable, so
            // downstream passes see a concrete `String` /
            // `Vec<T>` / etc. - crucial for the binary-op
            // lowering in `lower_binary` to route `s + t`
            // through `gos_rt_str_concat`.
            //
            // For `unwrap` / `unwrap_or` / `ok` / `err` /
            // `expect` the receiver is a `Result<T,E>` /
            // `Option<T>` and the unwrapped value is the
            // first generic argument. Dig into the receiver's
            // generic substitution so the destination is the
            // inner `T` instead of the wrapper Adt - keeps
            // `println!("{v}")` of the unwrapped value on the
            // right scalar dispatch.
            // For Option/Result `unwrap`, default the inner to
            // i64 when neither the receiver type nor the call
            // expression's type knows the wrapped element. The
            // common case where neither has a concrete type is
            // `m.get(k).unwrap()` for `HashMap<_, i64>` - the
            // type checker leaves both call expressions as
            // unresolved and the MIR has to assume something.
            let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
            // For the identity unwraps, prefer the inner generic
            // when the receiver is a `Result<T, E>` / `Option<T>`
            // wrapper, then the LOWERED receiver's own MIR type
            // if it's already concrete (the pinned-`Call` path
            // may have flattened the wrapper away - `let s =
            // fs::read_to_string(...).unwrap()` hits exactly
            // this shape now that `gos_rt_fs_read_to_string`
            // pins to `String`), and only as a last resort i64.
            // The HIR-side `receiver_ty` is often Var even when
            // the lowered local has been pinned by lower_call,
            // so consult both.
            let mir_recv_ty = self.locals[receiver_local.0 as usize].ty;
            let kind_is_concrete = |kind: TyKind| {
                matches!(
                    kind,
                    TyKind::Bool
                        | TyKind::Char
                        | TyKind::Int(_)
                        | TyKind::Float(_)
                        | TyKind::String
                        | TyKind::Vec(_)
                        | TyKind::Slice(_)
                        | TyKind::Array { .. }
                        | TyKind::Tuple(_)
                        | TyKind::HashMap { .. }
                        | TyKind::JsonValue
                )
            };
            let unwrap_inner = matches!(
                method.name.as_str(),
                "unwrap" | "unwrap_or" | "ok" | "expect"
            )
            .then(|| {
                // Bug-fix corollary of the inverse-fix-up:
                // when the lowered receiver is already a real
                // scalar (the typechecker thought
                // `json::as_str(v)` returned `Option<&str>`
                // but the runtime hands back a raw c-string
                // ptr typed `String`), the inner type IS the
                // lowered receiver - there is no Option to
                // peel. Without this short-circuit the
                // first_generic_of(receiver_ty) call below
                // pulled the typechecker-side `&str` out of
                // the Option<&str> Adt and dest got typed as
                // a reference, which the codegen then
                // dereferenced as a pointer-to-pointer.
                let mir_kind = self.tcx.kind_of(mir_recv_ty);
                if kind_is_concrete(mir_kind.clone()) && !matches!(mir_kind, TyKind::Adt { .. }) {
                    return mir_recv_ty;
                }
                self.first_generic_of(receiver_ty)
                    .or_else(|| self.first_generic_of(mir_recv_ty))
                    .unwrap_or_else(|| {
                        let mir_kind = self.tcx.kind_of(mir_recv_ty);
                        let recv_kind = self.tcx.kind_of(receiver_ty);
                        if kind_is_concrete(mir_kind.clone()) {
                            mir_recv_ty
                        } else if kind_is_concrete(recv_kind.clone()) {
                            receiver_ty
                        } else {
                            i64_ty
                        }
                    })
            });
            // Same shape for `.map` / `.map_err` on a scalar:
            // when the inverse fix-up forced runtime_symbol
            // to identity for these names, the destination
            // should also be the lowered scalar type - not
            // the typechecker's Option<T> wrapper.
            let map_inner = matches!(method.name.as_str(), "map" | "map_err")
                .then(|| {
                    let mir_kind = self.tcx.kind_of(mir_recv_ty);
                    if kind_is_concrete(mir_kind.clone()) && !matches!(mir_kind, TyKind::Adt { .. })
                    {
                        Some(mir_recv_ty)
                    } else {
                        None
                    }
                })
                .flatten();
            let err_inner = matches!(method.name.as_str(), "err")
                .then(|| self.second_generic_of(receiver_ty).unwrap_or(i64_ty));
            // For Option/Result identity unwraps, prefer the
            // generic argument over the call expression's HIR
            // type - the latter is `Adt { Result, .. }` /
            // `Adt { Option, .. }` if the type checker assumed
            // Wrapped semantics, but the compiled tier always
            // returns the inner value directly.
            let dest_ty = if let Some(inner) = unwrap_inner.or(err_inner).or(map_inner) {
                inner
            } else {
                match self.tcx.kind_of(ty) {
                    TyKind::Bool
                    | TyKind::Char
                    | TyKind::Int(_)
                    | TyKind::Float(_)
                    | TyKind::String
                    | TyKind::Vec(_)
                    | TyKind::Array { .. }
                    | TyKind::Slice(_)
                    | TyKind::Adt { .. }
                    | TyKind::JsonValue
                    | TyKind::Tuple(_) => ty,
                    _ => {
                        let mir_recv_ty = self.locals[receiver_local.0 as usize].ty;
                        let mir_recv_kind = self.tcx.kind_of(mir_recv_ty);
                        // Preserve `JsonValue` through the
                        // identity copy so subsequent
                        // `json::get(&m, ...)` / `m.as_str()`
                        // calls dispatch through the json
                        // runtime helpers instead of falling
                        // through to a Var-typed user-fn
                        // lookup. Without this, askq's
                        // `let tc = tcs[k].clone()` made every
                        // downstream tool-call field probe miss
                        // and the LLM's tool name / args came
                        // back as the empty string.
                        if matches!(
                            mir_recv_kind,
                            TyKind::Adt { .. }
                                | TyKind::Vec(_)
                                | TyKind::String
                                | TyKind::Int(_)
                                | TyKind::Float(_)
                                | TyKind::Bool
                                | TyKind::Tuple(_)
                                | TyKind::JsonValue
                        ) {
                            mir_recv_ty
                        } else {
                            receiver_ty
                        }
                    }
                }
            };
            let dest = self.fresh(dest_ty);
            // Propagate runtime kind / struct tags so chained
            // identity-method calls (`.clone()`, `.unwrap()`,
            // `.map_err(...)`) keep the receiver's surface
            // type for downstream dispatch.
            if let Some(rk) = self.local_runtime_kind.get(&receiver_local).copied() {
                self.local_runtime_kind.insert(dest, rk);
            }
            if let Some(inner) = unwrap_inner.or(err_inner)
                && let Some(sname) = self.struct_name_of(inner)
            {
                let runtime_kind: Option<&'static str> = match sname.as_str() {
                    "Error" => Some("errors::Error"),
                    "Response" => Some("http::Response"),
                    "Request" => Some("http::Request"),
                    "Client" => Some("http::Client"),
                    "Scanner" => Some("bufio::Scanner"),
                    "Pattern" => Some("regex::Pattern"),
                    _ => None,
                };
                self.local_struct.insert(dest, sname);
                if let Some(rk) = runtime_kind {
                    self.local_runtime_kind.insert(dest, rk);
                }
            }
            if let Some(sn) = self.local_struct.get(&receiver_local).cloned() {
                self.local_struct.insert(dest, sn);
            }
            if let Some(en) = self.local_elem_struct.get(&receiver_local).cloned() {
                self.local_elem_struct.insert(dest, en);
            }
            self.emit_assign(
                Place::local(dest),
                Rvalue::Use(Operand::Copy(Place::local(receiver_local))),
                span,
            );
            return Some(dest);
        }
        // Pin the destination's MIR type to the helper's
        // known return shape when the HIR expression type is
        // still opaque (inference variable or Error). Keeps
        // operand_print_kind + codegen inference grounded on
        // a concrete scalar/string kind.
        let pinned_ret: Ty = match sym {
            "gos_rt_str_concat"
            | "gos_rt_str_trim"
            | "gos_rt_str_to_lower"
            | "gos_rt_str_to_upper"
            | "gos_rt_str_replace"
            | "gos_rt_str_repeat"
            | "gos_rt_str_substring"
            | "gos_rt_heap_u8_to_string"
            | "gos_rt_i64_to_str"
            | "gos_rt_f64_to_str"
            | "gos_rt_stream_read_to_string"
            | "gos_rt_map_get_str_str"
            | "gos_rt_map_get_or_str_str"
            | "gos_rt_map_get_or_i64_str"
            | "gos_rt_map_get_i64_str"
            | "gos_rt_json_as_str"
            | "gos_rt_json_render"
            | "gos_rt_error_message"
            | "gos_rt_bufio_scanner_text"
            | "gos_rt_http_response_body"
            | "gos_rt_http_response_content_type"
            | "gos_rt_http_response_location"
            | "gos_rt_fs_read_to_string"
            | "gos_rt_path_join"
            | "gos_rt_http_request_path"
            | "gos_rt_http_request_method"
            | "gos_rt_http_request_query"
            | "gos_rt_http_request_body_str"
            | "gos_rt_str_to_title"
            | "gos_rt_str_trim_matches"
            | "gos_rt_str_replacen"
            | "gos_rt_str_pad_left"
            | "gos_rt_str_pad_right"
            | "gos_rt_str_push_char"
            | "gos_rt_str_push_byte"
            | "gos_rt_regex_find" => self.tcx.string_ty(),
            "gos_rt_str_split"
            | "gos_rt_str_lines"
            | "gos_rt_str_split_whitespace"
            | "gos_rt_str_splitn" => {
                let s = self.tcx.string_ty();
                self.tcx.intern(gossamer_types::TyKind::Vec(s))
            }
            "gos_rt_str_chars" => {
                // Return shape is `Vec<char>` - one i64 codepoint per
                // slot - so `for ch in s.chars()` reads each via
                // `gos_rt_vec_get_i64` and binds a `char`.
                let ch = self.tcx.intern(gossamer_types::TyKind::Char);
                self.tcx.intern(gossamer_types::TyKind::Vec(ch))
            }
            "gos_rt_str_contains_any" | "gos_rt_str_contains_rune" | "gos_rt_str_equal_fold" => {
                self.tcx.bool_ty()
            }
            "gos_rt_str_index_any" | "gos_rt_str_index_rune" | "gos_rt_str_last_index_any" => {
                self.option_i64_adt_ty()
            }
            "gos_rt_str_strip_prefix" | "gos_rt_str_strip_suffix" => self.option_string_adt_ty(),
            "gos_rt_str_as_bytes" => {
                // Return shape is `Vec<i64>` - the runtime
                // helper materialises one i64 slot per byte
                // (zero-extended) so downstream `bytes[i]`
                // indexing dispatches through the Slice/Vec
                // path (`gos_rt_vec_get_ptr` + `gos_load`)
                // instead of the flat-stride Place::Index
                // walk that reads into the GosVec header.
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                self.tcx.intern(gossamer_types::TyKind::Vec(i))
            }
            "gos_rt_http_response_raw_bytes" | "gos_rt_http_request_raw_body" => {
                let u8_ty = self.tcx.int_ty(gossamer_types::IntTy::U8);
                self.tcx.intern(gossamer_types::TyKind::Vec(u8_ty))
            }
            "gos_rt_http_response_headers" | "gos_rt_http_request_headers" => {
                let s = self.tcx.string_ty();
                let tup = self.tcx.intern(gossamer_types::TyKind::Tuple(vec![s, s]));
                self.tcx.intern(gossamer_types::TyKind::Vec(tup))
            }
            "gos_rt_sync_map_len" | "gos_rt_deque_len" => {
                self.tcx.int_ty(gossamer_types::IntTy::I64)
            }
            "gos_rt_sync_map_contains" | "gos_rt_deque_is_empty" => self.tcx.bool_ty(),
            "gos_rt_sync_map_keys" | "gos_rt_btmap_keys" => {
                let s = self.tcx.string_ty();
                self.tcx.intern(gossamer_types::TyKind::Vec(s))
            }
            "gos_rt_sync_map_get" => self.option_string_adt_ty(),
            "gos_rt_map_keys_i64" | "gos_rt_map_values_i64" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                self.tcx.intern(gossamer_types::TyKind::Vec(i))
            }
            "gos_rt_map_keys_str" | "gos_rt_map_values_str" => {
                let s = self.tcx.string_ty();
                self.tcx.intern(gossamer_types::TyKind::Vec(s))
            }
            "gos_rt_str_contains" | "gos_rt_str_starts_with" | "gos_rt_str_ends_with" => {
                self.tcx.bool_ty()
            }
            "gos_rt_str_len"
            | "gos_rt_str_byte_at"
            | "gos_rt_arr_len"
            | "gos_rt_len"
            | "gos_rt_map_len"
            | "gos_rt_map_get_i64"
            | "gos_rt_map_get_str_i64"
            | "gos_rt_json_as_i64"
            | "gos_rt_json_len"
            | "gos_rt_http_response_status"
            | "gos_rt_parse_i64" => self.tcx.int_ty(gossamer_types::IntTy::I64),
            // `m.get_or(k, default)` returns the stored value word.
            // For Vec-valued maps (`iter::chunk_by` results) that word
            // is a vec pointer - pin the dest to the value type so
            // for-loops and indexing dispatch through the vec helpers
            // instead of treating it as a scalar.
            "gos_rt_map_get_or_i64"
            | "gos_rt_map_get_or_str_i64"
            | "gos_rt_map_or_insert_i64_i64"
            | "gos_rt_map_or_insert_str_i64" => {
                let value_ty = self.hash_map_kv_tys(receiver_ty).map(|(_, v)| v);
                match value_ty.map(|v| self.tcx.kind_of(v).clone()) {
                    Some(TyKind::Vec(_) | TyKind::Slice(_)) => {
                        value_ty.expect("kind matched above")
                    }
                    _ => self.tcx.int_ty(gossamer_types::IntTy::I64),
                }
            }
            // Both `chan.recv()` and `chan.try_recv()` return
            // `Option<T>` packed as `*mut GosResult { disc, payload }`.
            // The single-arg wrappers build the Option internally so
            // all backends can call with just the channel pointer.
            "gos_rt_chan_recv_option" | "gos_rt_chan_try_recv_option" => {
                use gossamer_types::TyKind;
                // Derive the element type from the receiver's
                // `Receiver<T>` so `rx.recv()` yields `Option<T>` with the
                // right payload. A hardcoded `Option<i64>` made a
                // `channel<String>` recv print the String payload's
                // pointer as a number on the compiled tier.
                let elem = [receiver_ty, lowered_recv_ty]
                    .into_iter()
                    .find_map(|t| match self.tcx.kind_of(t) {
                        TyKind::Receiver(e) | TyKind::Sender(e) => Some(*e),
                        _ => None,
                    })
                    .filter(|e| {
                        !matches!(
                            self.tcx.kind_of(*e),
                            TyKind::Var(_) | TyKind::Error | TyKind::Never
                        )
                    })
                    .unwrap_or_else(|| self.tcx.int_ty(gossamer_types::IntTy::I64));
                self.option_payload_adt_ty(elem)
            }
            "gos_rt_parse_i64_result" => self.result_i64_error_adt_ty(),
            "gos_rt_str_find_opt" | "gos_rt_str_to_i64_opt" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let substs = gossamer_types::Substs::from_types([i]);
                self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX - 1),
                    substs,
                })
            }
            "gos_rt_str_to_f64_opt" => {
                let f = self.tcx.float_ty(gossamer_types::FloatTy::F64);
                let substs = gossamer_types::Substs::from_types([f]);
                self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX - 1),
                    substs,
                })
            }
            "gos_rt_str_to_bool_opt" => {
                let b = self.tcx.bool_ty();
                let substs = gossamer_types::Substs::from_types([b]);
                self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX - 1),
                    substs,
                })
            }
            // 0.7.0 string-surface returning Option<(String, String)>.
            "gos_rt_str_split_once" | "gos_rt_str_rsplit_once" => {
                let s = self.tcx.string_ty();
                let tup = self.tcx.intern(gossamer_types::TyKind::Tuple(vec![s, s]));
                let substs = gossamer_types::Substs::from_types([tup]);
                self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX - 1),
                    substs,
                })
            }
            "gos_rt_str_count"
            | "gos_rt_vec_count_of_i64"
            | "gos_rt_vec_count_of_str"
            | "gos_rt_min_i64"
            | "gos_rt_max_i64"
            | "gos_rt_clamp_i64" => self.tcx.int_ty(gossamer_types::IntTy::I64),
            "gos_rt_str_strip_chars"
            | "gos_rt_str_lstrip_chars"
            | "gos_rt_str_rstrip_chars"
            | "gos_rt_str_zfill"
            | "gos_rt_str_center"
            | "gos_rt_path_base"
            | "gos_rt_path_dir"
            | "gos_rt_strings_join"
            | "gos_rt_vec_join_i64"
            | "gos_rt_vec_join_f64"
            | "gos_rt_vec_join_bool"
            | "gos_rt_uuid_v4"
            | "gos_rt_uuid_v7"
            | "gos_rt_uuid_normalize"
            | "gos_rt_uuid_simple"
            | "gos_rt_os_user_current_name"
            | "gos_rt_os_user_current_home"
            | "gos_rt_os_user_lookup_uid"
            | "gos_rt_netip_normalize"
            | "gos_rt_netip_host_of"
            | "gos_rt_netip_join_addr_port"
            | "gos_rt_mime_parse"
            | "gos_rt_mime_top"
            | "gos_rt_mime_sub"
            | "gos_rt_mime_charset"
            | "gos_rt_mime_boundary"
            | "gos_rt_mime_param"
            | "gos_rt_mime_type_by_extension"
            | "gos_rt_mime_extension_by_type"
            | "gos_rt_url_query_escape"
            | "gos_rt_url_path_escape"
            | "gos_rt_url_query_unescape"
            | "gos_rt_url_path_unescape" => self.tcx.string_ty(),
            "gos_rt_uuid_is_valid"
            | "gos_rt_netip_is_valid"
            | "gos_rt_netip_is_v4"
            | "gos_rt_netip_is_v6"
            | "gos_rt_netip_is_loopback"
            | "gos_rt_netip_is_unspecified"
            | "gos_rt_netip_is_multicast"
            | "gos_rt_netip_is_private"
            | "gos_rt_mime_is_valid"
            | "gos_rt_toml_is_valid"
            | "gos_rt_yaml_is_valid" => self.tcx.bool_ty(),
            "gos_rt_os_user_current_uid"
            | "gos_rt_os_user_current_gid"
            | "gos_rt_os_user_lookup_name"
            | "gos_rt_netip_port_of"
            | "gos_rt_bheap_peek_i64"
            | "gos_rt_bheap_len" => self.tcx.int_ty(gossamer_types::IntTy::I64),
            "gos_rt_min_f64" | "gos_rt_max_f64" | "gos_rt_clamp_f64" => {
                self.tcx.float_ty(gossamer_types::FloatTy::F64)
            }
            "gos_rt_vec_contains_i64" | "gos_rt_vec_contains_str" => self.tcx.bool_ty(),
            // `index_of` / `rfind` yield an `Option<i64>` index; the
            // payload genuinely is an integer regardless of element type.
            "gos_rt_str_rfind_opt" | "gos_rt_vec_index_of_i64" | "gos_rt_vec_index_of_str" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let substs = gossamer_types::Substs::from_types([i]);
                self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX - 1),
                    substs,
                })
            }
            // `first` / `last` / `pop` over a sequence (and `pop` over a
            // deque / map) return `Option<elem>`. Prefer the typeck-resolved
            // Option Adt; otherwise synthesise it from the receiver's element
            // type so a `Vec<String>` binds its Some-payload as a String even
            // when the call is consumed inline - the match-arm element-type
            // recovery only fires for path-bound receivers, leaving an inline
            // `match xs.first() { Some(s) => .. }` to render the pointer bits.
            "gos_rt_vec_first"
            | "gos_rt_vec_last"
            | "gos_rt_vec_pop_opt"
            | "gos_rt_map_pop_i64"
            | "gos_rt_map_pop_str"
            | "gos_rt_deque_pop_front" => {
                use gossamer_types::TyKind;
                if matches!(self.tcx.kind_of(ty), TyKind::Adt { .. }) {
                    ty
                } else {
                    let elem = self
                        .seq_elem_of(receiver_ty)
                        .or_else(|| self.seq_elem_of(lowered_recv_ty))
                        .or_else(|| {
                            // HashMap pop yields `Option<value>` - take the
                            // value type, not the leading key generic.
                            let mut flat = receiver_ty;
                            while let TyKind::Ref { inner, .. } = self.tcx.kind_of(flat) {
                                flat = *inner;
                            }
                            if let TyKind::HashMap { value, .. } = self.tcx.kind_of(flat) {
                                Some(*value)
                            } else {
                                None
                            }
                        })
                        // VecDeque<T> pop_front - the element is the sole generic.
                        .or_else(|| self.first_generic_of(receiver_ty))
                        .or_else(|| self.first_generic_of(lowered_recv_ty))
                        .unwrap_or_else(|| self.tcx.int_ty(gossamer_types::IntTy::I64));
                    let substs = gossamer_types::Substs::from_types([elem]);
                    self.tcx.intern(TyKind::Adt {
                        def: gossamer_resolve::DefId::local(u32::MAX - 1),
                        substs,
                    })
                }
            }
            // HashMap::get returns Option<V>. Prefer the HIR call type
            // when it's already an Adt (proper Option<V> wrapper from
            // typeck); otherwise synthesise Option<V> from the receiver's
            // HashMap value Ty. The pattern lowerer reads `adt_generic_at`
            // to recover V for the Some-binding's payload type, so
            // struct-valued maps bind `p: &Struct` instead of `i64` and
            // `p.field` lowers as a Ref<Struct> field projection.
            "gos_rt_map_get_i64_opt" | "gos_rt_map_get_str_opt" => {
                use gossamer_types::TyKind;
                let ty_kind = self.tcx.kind_of(ty).clone();
                if matches!(ty_kind, TyKind::Adt { .. }) {
                    ty
                } else {
                    let mut flat = receiver_ty;
                    while let TyKind::Ref { inner, .. } = self.tcx.kind_of(flat) {
                        flat = *inner;
                    }
                    let value_ty = if let TyKind::HashMap { value, .. } = self.tcx.kind_of(flat) {
                        *value
                    } else {
                        self.tcx.int_ty(gossamer_types::IntTy::I64)
                    };
                    let substs = gossamer_types::Substs::from_types([value_ty]);
                    self.tcx.intern(TyKind::Adt {
                        def: gossamer_resolve::DefId::local(u32::MAX - 1),
                        substs,
                    })
                }
            }
            // `path::extension` returns `Option<String>`.
            "gos_rt_path_ext" => self.option_string_adt_ty(),
            "gos_rt_vec_slice_result" | "gos_rt_vec_insert_safe" | "gos_rt_intarr_slice_result" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                let e = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([v, e]);
                self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                })
            }
            "gos_rt_floatarr_slice_result" => {
                let f = self.tcx.float_ty(gossamer_types::FloatTy::F64);
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(f));
                let e = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([v, e]);
                self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                })
            }
            "gos_rt_vec_remove_safe" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let e = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([i, e]);
                self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                })
            }
            // In-place Vec mutators return nothing.
            "gos_rt_vec_insert_at" | "gos_rt_vec_remove_at" => self.tcx.unit(),
            // `rev()` / `take(n)` / `step_by(s)` copy the receiver -
            // preserve its element type so byte-packed (`Vec<u8>`)
            // receivers keep their stride-1 indexing downstream.
            "gos_rt_vec_reversed" | "gos_rt_vec_take" | "gos_rt_vec_step_by"
                if {
                    let mut flat = receiver_ty;
                    while let TyKind::Ref { inner, .. } = self.tcx.kind_of(flat) {
                        flat = *inner;
                    }
                    matches!(
                        self.tcx.kind_of(flat),
                        TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. }
                    )
                } =>
            {
                let mut flat = receiver_ty;
                while let TyKind::Ref { inner, .. } = self.tcx.kind_of(flat) {
                    flat = *inner;
                }
                // A `[T; N]` array receiver is coerced to a `GosVec` before the
                // call, so the rev copy is a heap `Vec<T>`, not a flat
                // `[T; N]` - indexing the result as an inline array would read
                // the GosVec header. `Vec` / `Slice` keep their own type.
                match self.tcx.kind_of(flat).clone() {
                    TyKind::Array { elem, .. } => {
                        self.tcx.intern(gossamer_types::TyKind::Vec(elem))
                    }
                    _ => flat,
                }
            }
            "gos_rt_vec_reversed"
            | "gos_rt_bheap_push_i64"
            | "gos_rt_bheap_pop_i64"
            | "gos_rt_vec_pop_front_i64"
            | "gos_rt_vec_pop_back_i64"
            | "gos_rt_vec_push_front_i64"
            | "gos_rt_vec_push_back_i64"
            | "gos_rt_ovec_insert_i64"
            | "gos_rt_ovec_remove_at_i64"
            | "gos_rt_oset_insert_i64"
            | "gos_rt_oset_remove_i64"
            | "gos_rt_omap_insert_i64"
            | "gos_rt_omap_remove_i64" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                self.tcx.intern(gossamer_types::TyKind::Vec(i))
            }
            "gos_rt_vec_first_i64"
            | "gos_rt_vec_last_i64"
            | "gos_rt_ovec_index_of_i64"
            | "gos_rt_omap_get_i64"
            | "gos_rt_omap_len" => self.tcx.int_ty(gossamer_types::IntTy::I64),
            "gos_rt_ovec_contains_i64"
            | "gos_rt_oset_contains_i64"
            | "gos_rt_omap_contains_key_i64" => self.tcx.bool_ty(),
            "gos_rt_map_keys_vec" => {
                // The element type is the map's KEY type, so a bound
                // `let ks = m.keys()` on a `HashMap<String, _>` iterates
                // strings rather than reading the key pointers as i64.
                use gossamer_types::TyKind;
                let mut flat = receiver_ty;
                while let TyKind::Ref { inner, .. } = self.tcx.kind_of(flat) {
                    flat = *inner;
                }
                let elem = match self.tcx.kind_of(flat) {
                    TyKind::HashMap { key, .. } => *key,
                    _ => self.tcx.int_ty(gossamer_types::IntTy::I64),
                };
                self.tcx.intern(TyKind::Vec(elem))
            }
            // `m.values()` yields the stored value words. A struct value is
            // stored as a boxed pointer, so the element is typed as a
            // reference to the value type: `for v in m.values()` then binds a
            // box pointer (a single word) and field access derefs it, instead
            // of materialising an inline struct from the pointer bits. Scalar
            // and string values keep their direct element type.
            "gos_rt_map_values_vec" => {
                use gossamer_types::TyKind;
                let mut flat = receiver_ty;
                while let TyKind::Ref { inner, .. } = self.tcx.kind_of(flat) {
                    flat = *inner;
                }
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let elem = match self.tcx.kind_of(flat) {
                    TyKind::HashMap { value, .. } => {
                        let value = *value;
                        if self.struct_name_of(value).is_some() {
                            self.tcx.intern(TyKind::Ref {
                                mutability: gossamer_types::Mutbl::Not,
                                inner: value,
                            })
                        } else {
                            value
                        }
                    }
                    _ => i64_ty,
                };
                self.tcx.intern(TyKind::Vec(elem))
            }
            "gos_rt_str_slice"
            | "gos_rt_toml_to_json"
            | "gos_rt_toml_from_json"
            | "gos_rt_toml_pretty"
            | "gos_rt_yaml_to_json"
            | "gos_rt_yaml_from_json" => self.result_string_error_adt_ty(),
            "gos_rt_flag_cell_load_str" => self.tcx.string_ty(),
            "gos_rt_flag_cell_load_i64" => self.tcx.int_ty(gossamer_types::IntTy::I64),
            "gos_rt_flag_cell_load_bool" => self.tcx.bool_ty(),
            "gos_rt_result_map_err"
            | "gos_rt_result_map"
            | "gos_rt_result_map_err_bare"
            | "gos_rt_result_map_bare" => {
                use gossamer_types::TyKind;
                let mut t = receiver_ty;
                while let TyKind::Ref { inner, .. } = self.tcx.kind_of(t) {
                    t = *inner;
                }
                if matches!(self.tcx.kind_of(t), TyKind::Adt { .. }) {
                    t
                } else {
                    // Receiver type lost - fall back to the lowered
                    // local's MIR type so we still pin a Result/Adt
                    // when the typechecker handed us a Var. Without
                    // this the call's destination defaults to Var
                    // and `match mapped` collapses to the Ok arm.
                    let mut lt = lowered_recv_ty;
                    while let TyKind::Ref { inner, .. } = self.tcx.kind_of(lt) {
                        lt = *inner;
                    }
                    if matches!(self.tcx.kind_of(lt), TyKind::Adt { .. }) {
                        lt
                    } else {
                        self.result_i64_error_adt_ty()
                    }
                }
            }
            // `result.ok_or(new_err)` - the returned Result's
            // first generic is the original Ok-payload type,
            // the second generic is the type of `new_err` (the
            // replacement). Build a fresh Result Adt from
            // `(receiver's first generic, new_err's type)` so
            // downstream `match` arms / `?` dispatches see the
            // post-replacement Err type and propagate the right
            // payload shape.
            "gos_rt_result_ok_or" => {
                use gossamer_types::TyKind;
                let inner_ok = self
                    .first_generic_of(receiver_ty)
                    .or_else(|| self.first_generic_of(lowered_recv_ty))
                    .unwrap_or_else(|| self.tcx.int_ty(gossamer_types::IntTy::I64));
                let new_err_ty = arg_operands
                    .get(1)
                    .and_then(|op| match op {
                        Operand::Copy(p) => Some(p.local),
                        _ => None,
                    })
                    .map_or_else(|| self.tcx.string_ty(), |l| self.locals[l.0 as usize].ty);
                let substs = gossamer_types::Substs::from_types([inner_ok, new_err_ty]);
                self.tcx.intern(TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                })
            }
            // Result/Option unwrap helpers return the inner
            // T as a raw 64-bit slot. Pin to the wrapper's
            // first generic arg so downstream codegen sees a
            // concrete int / string / etc. instead of the
            // sentinel Adt. The HIR `receiver_ty` is often a
            // Var for chained calls; consult the lowered
            // local's MIR type as a fallback before defaulting
            // to i64.
            "gos_rt_result_unwrap" | "gos_rt_result_unwrap_or" | "gos_rt_result_ok" => self
                .first_generic_of(receiver_ty)
                .or_else(|| self.first_generic_of(lowered_recv_ty))
                .unwrap_or_else(|| self.tcx.int_ty(gossamer_types::IntTy::I64)),
            "gos_rt_result_err" => self
                .second_generic_of(receiver_ty)
                .or_else(|| self.second_generic_of(lowered_recv_ty))
                .unwrap_or_else(|| self.tcx.int_ty(gossamer_types::IntTy::I64)),
            "gos_rt_result_is_ok" | "gos_rt_result_is_err" => self.tcx.bool_ty(),
            "gos_rt_json_as_f64" => self.tcx.float_ty(gossamer_types::FloatTy::F64),
            "gos_rt_chan_try_send"
            | "gos_rt_map_remove"
            | "gos_rt_map_remove_i64"
            | "gos_rt_map_remove_str"
            | "gos_rt_map_contains_key_i64"
            | "gos_rt_map_contains_key_str"
            | "gos_rt_json_is_null"
            | "gos_rt_json_as_bool"
            | "gos_rt_error_is"
            | "gos_rt_regex_is_match"
            | "gos_rt_fs_write"
            | "gos_rt_fs_create_dir_all"
            | "gos_rt_bufio_scanner_scan"
            | "gos_rt_testing_check"
            | "gos_rt_testing_check_eq_i64"
            | "gos_rt_str_is_empty"
            | "gos_rt_len_is_zero" => self.tcx.bool_ty(),
            "gos_rt_json_get" | "gos_rt_json_at" | "gos_rt_json_parse" => self.tcx.json_value_ty(),
            "gos_rt_error_cause" => self.option_adt_ty(),
            // `ResponseStream::next_line() -> Option<String>`.
            // Pin the dest type so `while let Some(line) =
            // stream.next_line()` binds `line: String` (printed
            // via the str c-pointer path) instead of an i64
            // (printed as a raw pointer numeral).
            "gos_rt_http_stream_next_line" => self.option_string_adt_ty(),
            // `Child::read_line() -> Option<String>` - same pin as
            // the HTTP stream's line reader.
            "gos_rt_child_read_line" => self.option_string_adt_ty(),
            "gos_rt_stream_read_line" => self.result_i64_error_adt_ty(),
            "gos_rt_child_read_stdout" => self.tcx.string_ty(),
            "gos_rt_child_write_stdin" | "gos_rt_child_kill" => self.tcx.bool_ty(),
            // `Child::wait() -> Result<i64, errors::Error>`.
            "gos_rt_child_wait" => {
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let err_ty = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([i64_ty, err_ty]);
                self.tcx.intern(TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                })
            }
            // `ResponseStream::next_chunk(max) -> Option<[u8]>`.
            // Pin the dest so `while let Some(chunk) = ...` binds
            // `chunk: Vec<u8>` and the payload extraction /
            // per-iteration drop treat it as a byte vec.
            "gos_rt_http_stream_next_chunk" => self.option_vec_u8_ty(),
            // `Request::send() -> Result<Response, errors::Error>`
            // - same packed-i128 Result shape and sentinel Ok
            // payload as the `http::get` free call.
            "gos_rt_http_request_send" => self.result_response_error_adt_ty(),
            // Pin the iterator to the receiver's vec type so
            // `.next()` dispatch can recover the element kind.
            "gos_rt_arr_iter" => {
                let mut flat = self.locals[receiver_local.0 as usize].ty;
                while let TyKind::Ref { inner, .. } = self.tcx.kind_of(flat) {
                    flat = *inner;
                }
                match self.tcx.kind_of(flat) {
                    TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. } => flat,
                    _ => receiver_ty,
                }
            }
            // `.clone()` on a Vec/Slice: dest holds a fresh `*mut
            // GosVec` of the same element shape as the receiver.
            // The HIR `ty` is often a `Var(_)` here because the
            // typechecker leaves the method's return wrapper
            // unresolved (`xs[i].clone()` is a chained index +
            // method) so we recover the shape from the lowered
            // receiver. Without this pin the dest defaults to i64
            // and `row[1]` later misses the `gos_rt_vec_get_i64`
            // helper.
            "gos_rt_vec_clone" => {
                let mut flat = self.locals[receiver_local.0 as usize].ty;
                while let TyKind::Ref { inner, .. } = self.tcx.kind_of(flat) {
                    flat = *inner;
                }
                match self.tcx.kind_of(flat) {
                    TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. } => flat,
                    _ => match self.tcx.kind_of(ty) {
                        TyKind::Vec(_) | TyKind::Slice(_) => ty,
                        _ => receiver_ty,
                    },
                }
            }
            _ => match self.tcx.kind_of(ty) {
                TyKind::Error | TyKind::Var(_) => self.tcx.int_ty(gossamer_types::IntTy::I64),
                _ => ty,
            },
        };
        let dest = self.fresh(pinned_ret);
        // Propagate element-struct tag so `xs.map_err(...)?[i].field`
        // chains keep the DirInfo / other elem-struct annotations.
        if let Some(en) = self.local_elem_struct.get(&receiver_local).cloned() {
            self.local_elem_struct.insert(dest, en);
        }
        // Tag the destination's runtime kind so chained
        // method calls + `?` propagation continue to dispatch
        // correctly on the result of the runtime helper.
        let dest_kind: Option<&'static str> = match sym {
            "gos_rt_http_request_header" | "gos_rt_http_request_body" => Some("http::Request"),
            "gos_rt_http_request_set_value" => Some("http::Request"),
            "gos_rt_http_response_with_header" => Some("http::Response"),
            "gos_rt_http_client_get"
            | "gos_rt_http_client_post"
            | "gos_rt_http_client_put"
            | "gos_rt_http_client_options"
            | "gos_rt_http_client_delete"
            | "gos_rt_http_client_head" => Some("http::Request"),
            "gos_rt_arr_iter" => Some("vec::Iter"),
            _ => None,
        };
        if let Some(rk) = dest_kind {
            self.local_runtime_kind.insert(dest, rk);
        }
        // Payload-extracting result helpers inherit the receiver's
        // runtime kind: a Result-typed local is tagged with its Ok
        // payload's kind (`regex::compile` dest → "regex::Pattern"),
        // so `compile(..).unwrap().replace_all(..)` keeps dispatching
        // on the pattern kind after the unwrap.
        if dest_kind.is_none()
            && matches!(sym, "gos_rt_result_unwrap" | "gos_rt_result_unwrap_or")
            && let Some(rk) = self.local_runtime_kind.get(&receiver_local).copied()
        {
            self.local_runtime_kind.insert(dest, rk);
        }
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(sym.to_string())),
            args: arg_operands,
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }
}
