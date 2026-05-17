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
    pub(crate) fn lower_stdlib_free_call(
        &mut self,
        callee: &HirExpr,
        args: &[HirExpr],
        span: Span,
    ) -> Option<Local> {
        let HirExprKind::Path {
            segments,
            def: callee_def,
            ..
        } = &callee.kind
        else {
            return None;
        };
        let names: Vec<&str> = segments.iter().map(|s| s.name.as_str()).collect();
        let strip_std = if names.first() == Some(&"std") {
            &names[1..]
        } else {
            &names[..]
        };
        let joined = strip_std.join("::");
        // 0.7.0 — bare prelude names (`min`, `max`, `clamp`) shadow
        // a runtime helper only when the user hasn't defined their
        // own fn with that name. A non-None `def` here means the
        // resolver bound this path to a user fn — defer to the
        // generic user-fn dispatch below.
        if callee_def.is_some()
            && segments.len() == 1
            && matches!(joined.as_str(), "min" | "max" | "clamp")
        {
            return None;
        }
        let (rt_name, ret_ty) = match joined.as_str() {
            "errors::new" => (
                "gos_rt_error_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "errors::wrap" => (
                "gos_rt_error_wrap",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            // Returns Option<Error> as *mut GosResult (disc=0→Some, disc=1→None).
            // Takes *mut GosVec; MIR coerces the array literal before the call.
            "errors::join" => ("gos_rt_errors_join_vec", self.option_adt_ty()),
            "errors::is" => ("gos_rt_error_is", self.tcx.bool_ty()),
            "regex::compile" => (
                "gos_rt_regex_compile",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "regex::is_match" => ("gos_rt_regex_is_match", self.tcx.bool_ty()),
            // Returns Option<(start, end, text)> — disc=0 Some, disc=1 None.
            "regex::find" => ("gos_rt_regex_find_opt", self.option_tuple3_i64_i64_str_ty()),
            // Returns Option<Vec<String>> — disc=0 Some(caps), disc=1 None.
            "regex::captures" => ("gos_rt_regex_captures", self.option_vec_string_ty()),
            "regex::find_all" => {
                let s = self.tcx.string_ty();
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(s));
                ("gos_rt_regex_find_all", v)
            }
            "regex::captures_all" => {
                // Returns `Vec<Vec<String>>` — outer per-match,
                // inner per-group. Missing optional groups are
                // null pointers (rendered as empty strings on
                // print, decoded as `Option::None` by `match`).
                let s = self.tcx.string_ty();
                let inner = self.tcx.intern(gossamer_types::TyKind::Vec(s));
                let outer = self.tcx.intern(gossamer_types::TyKind::Vec(inner));
                ("gos_rt_regex_captures_all", outer)
            }
            "regex::replace_all" => ("gos_rt_regex_replace_all", self.tcx.string_ty()),
            "regex::split" => {
                let s = self.tcx.string_ty();
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(s));
                ("gos_rt_regex_split", v)
            }
            "fs::read_to_string" => ("gos_rt_fs_read_to_string", self.tcx.string_ty()),
            // `os::read_file_to_string(path) -> Result<String, IoError>`
            // is a re-spelling of `fs::read_to_string` in the
            // stdlib. Compiled mode never wired a binding for the
            // os-prefixed name, so the call previously fell through
            // to a generic dispatch that returned an empty string.
            // Mirror `fs::read_to_string`'s shape — the runtime
            // helper hands back a `*mut c_char`, the MIR type is
            // `String`, and downstream `.map_err(...)?` paths do
            // the result-wrap themselves.
            "os::read_file_to_string" => ("gos_rt_fs_read_to_string", self.tcx.string_ty()),
            // `os::read_file(path) -> Result<Vec<u8>, errors::Error>` —
            // returns the raw bytes so binary files (images,
            // archives, …) round-trip through Gossamer without the
            // UTF-8-lossy collapse `read_file_to_string` would apply.
            "os::read_file" | "fs::read" => {
                let u8_ty = self.tcx.int_ty(gossamer_types::IntTy::U8);
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(u8_ty));
                let e = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([v, e]);
                let result_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                });
                ("gos_rt_fs_read_bytes_result", result_ty)
            }
            // `os::read_dir(path) -> Result<Vec<String>, IoError>`.
            // The runtime helper hands back a `*mut GosVec` of
            // C-string names (errors land as an empty vec for now,
            // matching the interp's behaviour-by-shape). Pin the
            // dest type to `Vec<String>` so downstream `for entry
            // in entries` iterates real C-string slots instead of
            // segfaulting on the null pointer that the generic
            // fall-through used to hand back.
            "os::read_dir" | "fs::read_dir" => {
                let s = self.tcx.string_ty();
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(s));
                ("gos_rt_os_read_dir", v)
            }
            "fs::write" | "os::write_file" => {
                // Pick the bytes-shaped variant when the contents
                // argument is a Vec<u8> / &[u8] — the c-string-shaped
                // helper would truncate at the first NUL and corrupt
                // binary payloads (image writes, gzip bodies, etc.).
                // The typechecker often leaves `&local_vec`-shaped
                // args as `Ref<Var(_)>`, so we walk through the `&`
                // operator and consult `peek_collection_type`, which
                // recovers the actual MIR-pinned local type.
                let bytes_shaped = args.get(1).is_some_and(|a| {
                    use gossamer_types::{IntTy, TyKind};
                    if is_vec_u8_arg(self.tcx, a) {
                        return true;
                    }
                    let inner_expr = if let HirExprKind::Unary { op, operand } = &a.kind {
                        if matches!(op, HirUnaryOp::RefShared | HirUnaryOp::RefMut) {
                            operand.as_ref()
                        } else {
                            a
                        }
                    } else {
                        a
                    };
                    let probe = self
                        .peek_collection_type(inner_expr)
                        .or(Some(inner_expr.ty));
                    probe.is_some_and(|t| {
                        let mut walk = t;
                        while let TyKind::Ref { inner, .. } = self.tcx.kind_of(walk) {
                            walk = *inner;
                        }
                        let elem = match self.tcx.kind_of(walk) {
                            TyKind::Vec(e) | TyKind::Slice(e) => *e,
                            _ => return false,
                        };
                        matches!(self.tcx.kind_of(elem), TyKind::Int(IntTy::U8))
                    })
                });
                let sym = if bytes_shaped {
                    "gos_rt_os_write_file_bytes_result"
                } else {
                    "gos_rt_os_write_file_result"
                };
                (sym, self.result_unit_error_adt_ty())
            }
            "fs::create_dir_all" | "os::mkdir" | "os::mkdir_all" => (
                "gos_rt_os_mkdir_all_result",
                self.result_unit_error_adt_ty(),
            ),
            "fs::remove_file" | "os::remove_file" => (
                "gos_rt_os_remove_file_result",
                self.result_unit_error_adt_ty(),
            ),
            "fs::remove_all" | "os::remove_dir" | "os::remove_dir_all" => (
                "gos_rt_os_remove_dir_all_result",
                self.result_unit_error_adt_ty(),
            ),
            "path::join" => ("gos_rt_path_join", self.tcx.string_ty()),
            // 0.7.0 stdlib wiring — string-surface free fns that
            // the VM already exposes but that lacked a compiled-tier
            // runtime entry point. Each maps a fully-qualified
            // module path to the matching `gos_rt_*` helper.
            "strings::join" => ("gos_rt_strings_join", self.tcx.string_ty()),
            "strings::split_once" | "strings::rsplit_once" => {
                let s = self.tcx.string_ty();
                let tup = self.tcx.intern(gossamer_types::TyKind::Tuple(vec![s, s]));
                let substs = gossamer_types::Substs::from_types([tup]);
                let opt_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX - 1),
                    substs,
                });
                let sym = if joined == "strings::split_once" {
                    "gos_rt_str_split_once"
                } else {
                    "gos_rt_str_rsplit_once"
                };
                (sym, opt_ty)
            }
            "strings::count" => (
                "gos_rt_str_count",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "strings::strip_chars" => ("gos_rt_str_strip_chars", self.tcx.string_ty()),
            "strings::lstrip_chars" => ("gos_rt_str_lstrip_chars", self.tcx.string_ty()),
            "strings::rstrip_chars" => ("gos_rt_str_rstrip_chars", self.tcx.string_ty()),
            "strings::zfill" => ("gos_rt_str_zfill", self.tcx.string_ty()),
            "strings::center" => ("gos_rt_str_center", self.tcx.string_ty()),
            "strings::slice" => ("gos_rt_str_slice", self.result_string_error_adt_ty()),
            // String-as-receiver `rfind` returns Option<i64>; same
            // discriminant-packed shape as `find_opt`.
            "strings::rfind" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let substs = gossamer_types::Substs::from_types([i]);
                let opt_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX - 1),
                    substs,
                });
                ("gos_rt_str_rfind_opt", opt_ty)
            }
            "path::base" => ("gos_rt_path_base", self.tcx.string_ty()),
            "path::dir" => ("gos_rt_path_dir", self.tcx.string_ty()),
            "path::ext" => ("gos_rt_path_ext", self.option_string_adt_ty()),
            "uuid::v4" => ("gos_rt_uuid_v4", self.tcx.string_ty()),
            "uuid::v7" => ("gos_rt_uuid_v7", self.tcx.string_ty()),
            "uuid::is_valid" => ("gos_rt_uuid_is_valid", self.tcx.bool_ty()),
            "uuid::normalize" => ("gos_rt_uuid_normalize", self.tcx.string_ty()),
            "uuid::simple" => ("gos_rt_uuid_simple", self.tcx.string_ty()),
            "user::current_name" => ("gos_rt_os_user_current_name", self.tcx.string_ty()),
            "user::current_uid" => (
                "gos_rt_os_user_current_uid",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "user::current_gid" => (
                "gos_rt_os_user_current_gid",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "user::current_home" => ("gos_rt_os_user_current_home", self.tcx.string_ty()),
            "user::lookup_uid" => ("gos_rt_os_user_lookup_uid", self.tcx.string_ty()),
            "user::lookup_name" => (
                "gos_rt_os_user_lookup_name",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "netip::is_valid" => ("gos_rt_netip_is_valid", self.tcx.bool_ty()),
            "netip::is_v4" => ("gos_rt_netip_is_v4", self.tcx.bool_ty()),
            "netip::is_v6" => ("gos_rt_netip_is_v6", self.tcx.bool_ty()),
            "netip::is_loopback" => ("gos_rt_netip_is_loopback", self.tcx.bool_ty()),
            "netip::is_unspecified" => ("gos_rt_netip_is_unspecified", self.tcx.bool_ty()),
            "netip::is_multicast" => ("gos_rt_netip_is_multicast", self.tcx.bool_ty()),
            "netip::is_private" => ("gos_rt_netip_is_private", self.tcx.bool_ty()),
            "netip::normalize" => ("gos_rt_netip_normalize", self.tcx.string_ty()),
            "netip::host_of" => ("gos_rt_netip_host_of", self.tcx.string_ty()),
            "netip::port_of" => (
                "gos_rt_netip_port_of",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "netip::join_addr_port" => ("gos_rt_netip_join_addr_port", self.tcx.string_ty()),
            "mime::parse" => ("gos_rt_mime_parse", self.tcx.string_ty()),
            "mime::top" => ("gos_rt_mime_top", self.tcx.string_ty()),
            "mime::sub" => ("gos_rt_mime_sub", self.tcx.string_ty()),
            "mime::charset" => ("gos_rt_mime_charset", self.tcx.string_ty()),
            "mime::boundary" => ("gos_rt_mime_boundary", self.tcx.string_ty()),
            "mime::param" => ("gos_rt_mime_param", self.tcx.string_ty()),
            "mime::type_by_extension" => ("gos_rt_mime_type_by_extension", self.tcx.string_ty()),
            "mime::extension_by_type" => ("gos_rt_mime_extension_by_type", self.tcx.string_ty()),
            "mime::is_valid" => ("gos_rt_mime_is_valid", self.tcx.bool_ty()),
            "toml::to_json" => ("gos_rt_toml_to_json", self.result_string_error_adt_ty()),
            "toml::from_json" => ("gos_rt_toml_from_json", self.result_string_error_adt_ty()),
            "toml::is_valid" => ("gos_rt_toml_is_valid", self.tcx.bool_ty()),
            "toml::pretty" => ("gos_rt_toml_pretty", self.result_string_error_adt_ty()),
            "yaml::to_json" => ("gos_rt_yaml_to_json", self.result_string_error_adt_ty()),
            "yaml::from_json" => ("gos_rt_yaml_from_json", self.result_string_error_adt_ty()),
            "yaml::is_valid" => ("gos_rt_yaml_is_valid", self.tcx.bool_ty()),
            // ---------------------------------------------------------------
            // std::unicode — general-category predicates, casing,
            // normalization, segmentation. Char args lower as u32,
            // string args as `*const c_char`, bool results as i64
            // (auto-truncated to i1 by the LLVM lowerer). Vec<String>
            // returns route through `gos_rt_unicode_*` helpers that
            // build a GosVec with `elem_kind = STRING`.
            "unicode::is_letter" => ("gos_rt_unicode_is_letter", self.tcx.bool_ty()),
            "unicode::is_digit" => ("gos_rt_unicode_is_digit", self.tcx.bool_ty()),
            "unicode::is_number" => ("gos_rt_unicode_is_number", self.tcx.bool_ty()),
            "unicode::is_space" => ("gos_rt_unicode_is_space", self.tcx.bool_ty()),
            "unicode::is_upper" => ("gos_rt_unicode_is_upper", self.tcx.bool_ty()),
            "unicode::is_lower" => ("gos_rt_unicode_is_lower", self.tcx.bool_ty()),
            "unicode::is_title" => ("gos_rt_unicode_is_title", self.tcx.bool_ty()),
            "unicode::is_punct" => ("gos_rt_unicode_is_punct", self.tcx.bool_ty()),
            "unicode::is_symbol" => ("gos_rt_unicode_is_symbol", self.tcx.bool_ty()),
            "unicode::is_mark" => ("gos_rt_unicode_is_mark", self.tcx.bool_ty()),
            "unicode::is_print" => ("gos_rt_unicode_is_print", self.tcx.bool_ty()),
            "unicode::is_graphic" => ("gos_rt_unicode_is_graphic", self.tcx.bool_ty()),
            "unicode::is_control" => ("gos_rt_unicode_is_control", self.tcx.bool_ty()),
            "unicode::is_assigned" => ("gos_rt_unicode_is_assigned", self.tcx.bool_ty()),
            "unicode::combining_class" => (
                "gos_rt_unicode_combining_class",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "unicode::to_upper" => ("gos_rt_unicode_to_upper", self.tcx.char_ty()),
            "unicode::to_lower" => ("gos_rt_unicode_to_lower", self.tcx.char_ty()),
            "unicode::to_title" => ("gos_rt_unicode_to_title", self.tcx.char_ty()),
            "unicode::simple_fold" => ("gos_rt_unicode_simple_fold", self.tcx.char_ty()),
            "unicode::to_upper_str" => ("gos_rt_unicode_to_upper_str", self.tcx.string_ty()),
            "unicode::to_lower_str" => ("gos_rt_unicode_to_lower_str", self.tcx.string_ty()),
            "unicode::fold_case" => ("gos_rt_unicode_fold_case", self.tcx.string_ty()),
            "unicode::nfc" => ("gos_rt_unicode_nfc", self.tcx.string_ty()),
            "unicode::nfd" => ("gos_rt_unicode_nfd", self.tcx.string_ty()),
            "unicode::nfkc" => ("gos_rt_unicode_nfkc", self.tcx.string_ty()),
            "unicode::nfkd" => ("gos_rt_unicode_nfkd", self.tcx.string_ty()),
            "unicode::is_nfc" => ("gos_rt_unicode_is_nfc", self.tcx.bool_ty()),
            "unicode::is_nfd" => ("gos_rt_unicode_is_nfd", self.tcx.bool_ty()),
            "unicode::is_nfkc" => ("gos_rt_unicode_is_nfkc", self.tcx.bool_ty()),
            "unicode::is_nfkd" => ("gos_rt_unicode_is_nfkd", self.tcx.bool_ty()),
            "unicode::graphemes" => {
                let s = self.tcx.string_ty();
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(s));
                ("gos_rt_unicode_graphemes", v)
            }
            "unicode::grapheme_count" => (
                "gos_rt_unicode_grapheme_count",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "unicode::words" => {
                let s = self.tcx.string_ty();
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(s));
                ("gos_rt_unicode_words", v)
            }
            "unicode::word_bounds" => {
                let s = self.tcx.string_ty();
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(s));
                ("gos_rt_unicode_word_bounds", v)
            }
            "unicode::word_count" => (
                "gos_rt_unicode_word_count",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "unicode::sentences" => {
                let s = self.tcx.string_ty();
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(s));
                ("gos_rt_unicode_sentences", v)
            }
            "unicode::sentence_count" => (
                "gos_rt_unicode_sentence_count",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            // ---------------------------------------------------------------
            // std::utf8 — high-value helpers. The decode_rune family
            // returns `(char, usize)` tuples and stays interp-only
            // until the Adt-by-value ABI lands.
            "utf8::rune_count_in_string" => (
                "gos_rt_utf8_rune_count_in_string",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "utf8::count_runes" => (
                "gos_rt_utf8_count_runes",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "utf8::rune_count" => (
                "gos_rt_utf8_rune_count_in_string",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "utf8::rune_len" => (
                "gos_rt_utf8_rune_len",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "utf8::valid_rune" => ("gos_rt_utf8_valid_rune", self.tcx.bool_ty()),
            "utf8::valid_string" => ("gos_rt_utf8_valid_string", self.tcx.bool_ty()),
            "utf8::is_valid" => ("gos_rt_utf8_valid_string", self.tcx.bool_ty()),
            "utf8::full_rune_in_string" => ("gos_rt_utf8_full_rune_in_string", self.tcx.bool_ty()),
            "utf8::full_rune" => ("gos_rt_utf8_full_rune_in_string", self.tcx.bool_ty()),
            "utf8::rune_start" => ("gos_rt_utf8_rune_start", self.tcx.bool_ty()),
            "sync::Map::new" | "Map::new" => (
                "gos_rt_sync_map_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "heap::push" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let vec = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                ("gos_rt_bheap_push_i64", vec)
            }
            "heap::pop" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let vec = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                ("gos_rt_bheap_pop_i64", vec)
            }
            "heap::peek" => (
                "gos_rt_bheap_peek_i64",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "heap::len" => (
                "gos_rt_bheap_len",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "queue::push" | "stack::push" | "deque::push_back" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let vec = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                ("gos_rt_vec_push_back_i64", vec)
            }
            "queue::pop" | "deque::pop_front" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let vec = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                ("gos_rt_vec_pop_front_i64", vec)
            }
            "stack::pop" | "deque::pop_back" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let vec = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                ("gos_rt_vec_pop_back_i64", vec)
            }
            "deque::push_front" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let vec = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                ("gos_rt_vec_push_front_i64", vec)
            }
            "queue::peek" | "stack::peek_front" | "deque::peek_front" => (
                "gos_rt_vec_first_i64",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "stack::peek" | "deque::peek_back" => (
                "gos_rt_vec_last_i64",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "queue::len" | "stack::len" | "deque::len" => (
                "gos_rt_vec_len",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "ordered_vec::insert" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let vec = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                ("gos_rt_ovec_insert_i64", vec)
            }
            "ordered_vec::remove_at" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let vec = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                ("gos_rt_ovec_remove_at_i64", vec)
            }
            "ordered_vec::contains" => ("gos_rt_ovec_contains_i64", self.tcx.bool_ty()),
            "ordered_vec::index_of" => (
                "gos_rt_ovec_index_of_i64",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "ordered_vec::peek_min" => (
                "gos_rt_vec_first_i64",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "ordered_vec::peek_max" => (
                "gos_rt_vec_last_i64",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "ordered_vec::len" => (
                "gos_rt_vec_len",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "ordered_set::insert" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let vec = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                ("gos_rt_oset_insert_i64", vec)
            }
            "ordered_set::remove" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let vec = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                ("gos_rt_oset_remove_i64", vec)
            }
            "ordered_set::contains" => ("gos_rt_oset_contains_i64", self.tcx.bool_ty()),
            "ordered_set::len" => (
                "gos_rt_vec_len",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "ordered_map::insert" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let vec = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                ("gos_rt_omap_insert_i64", vec)
            }
            "ordered_map::remove" => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let vec = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                ("gos_rt_omap_remove_i64", vec)
            }
            "ordered_map::get" => (
                "gos_rt_omap_get_i64",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "ordered_map::contains_key" => ("gos_rt_omap_contains_key_i64", self.tcx.bool_ty()),
            "ordered_map::len" => (
                "gos_rt_omap_len",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "url::query_escape" => ("gos_rt_url_query_escape", self.tcx.string_ty()),
            "url::path_escape" => ("gos_rt_url_path_escape", self.tcx.string_ty()),
            "url::query_unescape" => ("gos_rt_url_query_unescape", self.tcx.string_ty()),
            "url::path_unescape" => ("gos_rt_url_path_unescape", self.tcx.string_ty()),
            "time::Duration::from_secs" => (
                "gos_rt_duration_from_secs",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "time::Duration::from_millis" => (
                "gos_rt_duration_from_millis",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "time::format_rfc3339" => {
                let s = self.tcx.string_ty();
                let substs = gossamer_types::Substs::from_types([s, s]);
                let result_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                });
                ("gos_rt_time_format_rfc3339", result_ty)
            }
            "time::parse_rfc3339" => {
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let s = self.tcx.string_ty();
                let substs = gossamer_types::Substs::from_types([i64_ty, s]);
                let result_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                });
                ("gos_rt_time_parse_rfc3339", result_ty)
            }
            "os::program_name" | "env::program_name" => {
                ("gos_rt_os_program_name", self.tcx.string_ty())
            }
            "env::temp_dir" | "os::temp_dir" => ("gos_rt_env_temp_dir", self.tcx.string_ty()),
            "env::home_dir" | "os::home_dir" => {
                ("gos_rt_env_home_dir", self.option_string_adt_ty())
            }
            "os::env" | "env::var" => ("gos_rt_os_env", self.option_string_adt_ty()),
            "os::exists" | "fs::exists" => ("gos_rt_os_exists", self.tcx.bool_ty()),
            "os::is_file" | "fs::is_file" => ("gos_rt_os_is_file", self.tcx.bool_ty()),
            "os::is_dir" | "fs::is_dir" => ("gos_rt_os_is_dir", self.tcx.bool_ty()),
            "os::is_symlink" | "fs::is_symlink" => ("gos_rt_os_is_symlink", self.tcx.bool_ty()),
            "os::file_size" | "fs::file_size" => (
                "gos_rt_os_file_size",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "os::cwd" | "env::current_dir" => ("gos_rt_os_cwd", self.result_string_error_adt_ty()),
            // `os::args() -> Vec<String>`. Pinning the dest type
            // here is what teaches `args[i].len()` to dispatch
            // through `gos_rt_str_len` instead of the generic
            // `gos_rt_arr_len`. Single-file builds got
            // `Vec<String>` for free from typeck, but cross-module
            // compilation (e.g. askq, where `cli.gos` references
            // `args` and sibling modules also exist) leaves the
            // call's HIR type as a `Var(_)` and the cranelift
            // dispatch then crashes inside `gos_rt_arr_len`
            // reading a Vec header out of a `*const c_char`
            // string pointer. The runtime now hands back a real
            // `*mut GosVec` whose data pointer is `argv + 1`, so
            // index access through the standard `header.ptr + i *
            // elem_bytes` shape Just Works.
            "os::args" | "env::args" => {
                let s = self.tcx.string_ty();
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(s));
                ("gos_rt_os_args", v)
            }
            "fs::list_dir" | "fs::walk_dir" => {
                // Return type is `Result<Vec<DirInfo>, errors::Error>`.
                // Pin the dest as a Result Adt whose first generic
                // is `Vec<DirInfo>` so `.map_err(...)?` unwraps to a
                // properly-typed Vec (driving `entries[i]` through
                // the Vec dispatch with `DirInfo` element-struct
                // tag) instead of a bare i64 pointer.
                let dir_info_def = gossamer_resolve::DefId::local(u32::MAX - 2);
                let dir_info_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: dir_info_def,
                    substs: gossamer_types::Substs::new(),
                });
                let vec_ty = self.tcx.intern(gossamer_types::TyKind::Vec(dir_info_ty));
                let err_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let substs = gossamer_types::Substs::from_types([vec_ty, err_ty]);
                let result_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                });
                let sym = if joined == "fs::list_dir" {
                    "gos_rt_fs_list_dir"
                } else {
                    "gos_rt_fs_walk_dir"
                };
                (sym, result_ty)
            }
            // `http::get(url, headers) -> Result<Response, errors::Error>`.
            // Pin the Ok payload to the sentinel-DefId Response Adt
            // so `r.status` / `r.body` / `r.content_type` /
            // `r.location` projections find the right field index
            // via `stdlib_struct_shapes`.
            "http::get" | "http::native_client::get" | "native_client::get" => {
                let resp_def = gossamer_resolve::DefId::local(u32::MAX - 5);
                let resp_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: resp_def,
                    substs: gossamer_types::Substs::new(),
                });
                let err_ty = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([resp_ty, err_ty]);
                let result_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                });
                ("gos_rt_http_get", result_ty)
            }
            // `http::stream(method, url, body, headers) -> Result<ResponseStream, errors::Error>`.
            // Pin the Ok payload to the sentinel-DefId
            // ResponseStream Adt so `.__handle` / `.status` /
            // `.content_type` projections find the right field index
            // via `stdlib_struct_shapes`. Without this binding, the
            // call lowered to a non-existent symbol and the
            // destination held an undefined pointer the caller
            // dereferenced as a Result aggregate (askq SSE chat
            // round hung when next_line read garbage).
            "http::stream" => {
                let rs_def = gossamer_resolve::DefId::local(u32::MAX - 4);
                let rs_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: rs_def,
                    substs: gossamer_types::Substs::new(),
                });
                let err_ty = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([rs_ty, err_ty]);
                let result_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                });
                ("gos_rt_http_stream", result_ty)
            }
            // `exec::run(prog, args) -> Result<Output, errors::Error>`.
            // Pin the Ok payload to the sentinel-DefId Output Adt so
            // `o.stdout` / `o.stderr` / `o.code` projections find the
            // right field index via `stdlib_struct_shapes`. Without
            // this binding, the call lowered to a non-existent
            // user-fn symbol and the destination held an undefined
            // pointer the caller then dereferenced as the Result
            // aggregate (the askq segfault).
            "exec::run" | "os::exec::run" | "process::run" => {
                let output_def = gossamer_resolve::DefId::local(u32::MAX - 3);
                let output_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: output_def,
                    substs: gossamer_types::Substs::new(),
                });
                let err_ty = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([output_ty, err_ty]);
                let result_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                });
                ("gos_rt_exec_run", result_ty)
            }
            // `os::set_env(name, value) -> Result<(), errors::Error>`.
            // Pin the Ok payload to unit and the Err to
            // `errors::Error` so callers' `?` shapes find the
            // right field layout. Without this binding the
            // compiled tier silently no-op'd `set_env` because
            // the generic free-call dispatch couldn't resolve
            // the symbol, and downstream `os::env` reads
            // returned the old value.
            "os::set_env" | "set_env" => {
                let unit_ty = self.tcx.unit();
                let err_ty = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([unit_ty, err_ty]);
                let result_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                });
                ("gos_rt_os_set_env", result_ty)
            }
            "os::unset_env" | "unset_env" => ("gos_rt_os_unset_env", self.tcx.unit()),
            // `exec::spawn(prog, args) -> Result<i64, errors::Error>`.
            // Non-blocking process launch — returns the child PID
            // so callers (daemon launchers, long-running tools)
            // don't block the calling goroutine. Pin the Ok
            // payload to `i64` and the Err to `errors::Error` so
            // downstream `?` / `match` shapes find the right field
            // layout.
            "exec::spawn" | "os::exec::spawn" | "process::spawn" => {
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let err_ty = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([i64_ty, err_ty]);
                let result_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                });
                ("gos_rt_exec_spawn", result_ty)
            }
            // `exec::kill(pid) -> bool` — best-effort SIGTERM.
            "exec::kill" | "os::exec::kill" | "process::kill" => {
                ("gos_rt_exec_kill", self.tcx.bool_ty())
            }
            // `exec::signal(pid, signum) -> bool`.
            "exec::signal" | "os::exec::signal" | "process::signal" => {
                ("gos_rt_exec_signal", self.tcx.bool_ty())
            }
            // `exec::kill_group(pid) -> bool` — kills the entire
            // process group on Unix; best-effort on Windows.
            "exec::kill_group" | "os::exec::kill_group" | "process::kill_group" => {
                ("gos_rt_exec_kill_group", self.tcx.bool_ty())
            }
            // `exec::wait_timeout(pid, ms) -> i64`. Returns the
            // child's exit code on success, -1 on timeout, -2 on
            // error (unknown pid, permission denied).
            "exec::wait_timeout" | "os::exec::wait_timeout" | "process::wait_timeout" => (
                "gos_rt_exec_wait_timeout",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            // `exec::pipeline_run(cmds: Vec<String>) -> Result<Output, errors::Error>`.
            // Same Ok-shape sentinel-DefId as `exec::run` so the
            // existing `Output { stdout, stderr, code }` field
            // projection lowers identically.
            "exec::pipeline_run" | "os::exec::pipeline_run" | "process::pipeline_run" => {
                let output_def = gossamer_resolve::DefId::local(u32::MAX - 3);
                let output_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: output_def,
                    substs: gossamer_types::Substs::new(),
                });
                let err_ty = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([output_ty, err_ty]);
                let result_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                });
                ("gos_rt_exec_pipeline_run", result_ty)
            }
            // `signal::on(sig_raw) -> i64` — registers a notifier.
            "signal::on" | "os::signal::on" => (
                "gos_rt_signal_on",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            // `Notifier::wait(handle)` — blocks until signal fires.
            "signal_wait" | "Notifier::wait" => ("gos_rt_signal_wait", self.tcx.unit()),
            // `Notifier::try_wait(handle) -> bool`.
            "signal_try_wait" | "Notifier::try_wait" => {
                ("gos_rt_signal_try_wait", self.tcx.bool_ty())
            }
            "flag::Set::new" => (
                "gos_rt_flag_set_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "bufio::Scanner::new" | "Scanner::new" => (
                "gos_rt_bufio_scanner_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "bufio::Scanner::next" | "Scanner::next" => {
                ("gos_rt_bufio_scanner_text", self.tcx.string_ty())
            }
            "http::Client::new" => (
                "gos_rt_http_client_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "http::Response::text" => (
                "gos_rt_http_response_text_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "http::Response::json" => (
                "gos_rt_http_response_json_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "http::serve" => ("gos_rt_http_serve", self.tcx.unit()),
            "http::serve_h2c" => ("gos_rt_http2_bind_and_run_h2c", self.tcx.unit()),
            // 0.4.0 HTTP-module bridges (compiled tier free-fn surface).
            // Stateful types (router::new, etc.) are interp-only and not
            // listed here — calling them in compiled mode emits an
            // "unsupported call" diagnostic via the generic fallback.
            "http::chunked::encode" | "chunked::encode" => {
                ("gos_rt_chunked_encode", self.tcx.string_ty())
            }
            "http::chunked::decode" | "chunked::decode" => {
                ("gos_rt_chunked_decode", self.tcx.string_ty())
            }
            "http::sse::encode_event" | "sse::encode_event" => {
                ("gos_rt_sse_encode_event", self.tcx.string_ty())
            }
            "http::sse::encode_comment" | "sse::encode_comment" => {
                ("gos_rt_sse_encode_comment", self.tcx.string_ty())
            }
            "http::sse::encode_retry" | "sse::encode_retry" => {
                ("gos_rt_sse_encode_retry", self.tcx.string_ty())
            }
            "http::middleware::new_request_id" | "middleware::new_request_id" => {
                ("gos_rt_mw_new_request_id", self.tcx.string_ty())
            }
            "http::middleware::accepts_gzip" | "middleware::accepts_gzip" => {
                ("gos_rt_mw_accepts_gzip", self.tcx.bool_ty())
            }
            "http::websocket::accept_key" | "websocket::accept_key" => {
                ("gos_rt_ws_accept_key", self.tcx.string_ty())
            }
            "http::static_files::mime_for_path" | "static_files::mime_for_path" => {
                ("gos_rt_static_mime_for_path", self.tcx.string_ty())
            }
            // Stateful constructors. The MIR call-path emits the
            // bare runtime symbol; user code does `Router::new()`
            // → constructor handle. Returns `*mut T` (Ptr) which
            // the caller treats as the receiver of subsequent
            // method calls.
            "http::router::Router::new"
            | "router::Router::new"
            | "Router::new"
            | "http::router::new"
            | "router::new" => (
                "gos_rt_router_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "http::websocket::ws_frame_text" | "websocket::ws_frame_text" => {
                ("gos_rt_ws_frame_text", self.tcx.string_ty())
            }
            "http::native_client::Client::new"
            | "native_client::Client::new"
            | "NativeClient::new" => (
                "gos_rt_native_client_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "http::static_files::FileServer::new"
            | "static_files::FileServer::new"
            | "FileServer::new" => (
                "gos_rt_file_server_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "http::proxy::Proxy::new" | "proxy::Proxy::new" | "Proxy::new" => (
                "gos_rt_proxy_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "gzip::encode" | "compress::gzip::encode" => {
                ("gos_rt_gzip_encode", self.tcx.string_ty())
            }
            "gzip::decode" | "compress::gzip::decode" => {
                ("gos_rt_gzip_decode", self.tcx.string_ty())
            }
            "slog::info" => ("gos_rt_slog_info", self.tcx.unit()),
            "slog::warn" => ("gos_rt_slog_warn", self.tcx.unit()),
            "slog::error" => ("gos_rt_slog_error", self.tcx.unit()),
            "slog::debug" => ("gos_rt_slog_debug", self.tcx.unit()),
            "testing::check" => ("gos_rt_testing_check", self.tcx.bool_ty()),
            "testing::check_eq" => ("gos_rt_testing_check_eq_i64", self.tcx.bool_ty()),
            "testing::check_ok" => {
                // Pass-through identity in compiled mode — assumes
                // happy path.
                ("", self.tcx.int_ty(gossamer_types::IntTy::I64))
            }
            // Stdlib collections beyond HashMap. The cranelift
            // intrinsic dispatch handles `HashSet::new` /
            // `BTreeMap::new` directly (no args); MIR routes the
            // call through these symbol names so the destination
            // local can be tagged with a runtime kind for method
            // dispatch.
            "HashSet::new" | "collections::HashSet::new" => (
                "gos_rt_set_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "BTreeMap::new" | "collections::BTreeMap::new" => (
                "gos_rt_btmap_new",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            // 0.7.0 — `HashMap::pop(m, k) -> Option<V>` free-fn shape.
            // Dispatches by the first arg's HashMap key type to the
            // string-keyed or i64-keyed runtime variant. The Option
            // payload is the previous value (i64 directly for
            // `HashMap<_, i64>`, c-string-cast-to-i64 for
            // `HashMap<_, String>`).
            "HashMap::pop" | "collections::HashMap::pop" if !args.is_empty() => {
                let key_kind = hashmap_key_kind(self.tcx, args[0].ty);
                let sym = if key_kind == VecElemKind::Str {
                    "gos_rt_map_pop_str"
                } else {
                    "gos_rt_map_pop_i64"
                };
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let substs = gossamer_types::Substs::from_types([i]);
                let opt_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX - 1),
                    substs,
                });
                (sym, opt_ty)
            }
            // 0.7.0 — `Vec::insert(xs, i, v)` / `Vec::remove(xs, i)` /
            // `Vec::slice(xs, a, b)` — free-fn forms of the same
            // Result-returning safe Vec helpers exposed as methods.
            "Vec::insert" if args.len() == 3 => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                let e = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([v, e]);
                let result_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                });
                ("gos_rt_vec_insert_safe", result_ty)
            }
            "Vec::remove" if args.len() == 2 => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let e = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([i, e]);
                let result_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                });
                ("gos_rt_vec_remove_safe", result_ty)
            }
            "Vec::slice" if args.len() == 3 => {
                let i = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let v = self.tcx.intern(gossamer_types::TyKind::Vec(i));
                let e = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([v, e]);
                let result_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                });
                ("gos_rt_vec_slice_result", result_ty)
            }
            "String::slice" if args.len() == 3 => {
                ("gos_rt_str_slice", self.result_string_error_adt_ty())
            }
            // 0.7.0 scalar cmp prelude — `min(a, b)` / `max(a, b)`
            // / `clamp(x, lo, hi)`. Two-arg shape dispatches by
            // first-arg HIR type to the i64 or f64 variant; the
            // Vec-shaped `min(xs)` / `max(xs)` fallback hits the
            // bare-name dispatch later (single-arg shape is *not*
            // matched here).
            "min" if args.len() == 2 => {
                let is_f = arg_is_float(self.tcx, &args[0]);
                let sym = if is_f {
                    "gos_rt_min_f64"
                } else {
                    "gos_rt_min_i64"
                };
                let ret = if is_f {
                    self.tcx.float_ty(gossamer_types::FloatTy::F64)
                } else {
                    self.tcx.int_ty(gossamer_types::IntTy::I64)
                };
                (sym, ret)
            }
            "max" if args.len() == 2 => {
                let is_f = arg_is_float(self.tcx, &args[0]);
                let sym = if is_f {
                    "gos_rt_max_f64"
                } else {
                    "gos_rt_max_i64"
                };
                let ret = if is_f {
                    self.tcx.float_ty(gossamer_types::FloatTy::F64)
                } else {
                    self.tcx.int_ty(gossamer_types::IntTy::I64)
                };
                (sym, ret)
            }
            "clamp" if args.len() == 3 => {
                let is_f = arg_is_float(self.tcx, &args[0]);
                let sym = if is_f {
                    "gos_rt_clamp_f64"
                } else {
                    "gos_rt_clamp_i64"
                };
                let ret = if is_f {
                    self.tcx.float_ty(gossamer_types::FloatTy::F64)
                } else {
                    self.tcx.int_ty(gossamer_types::IntTy::I64)
                };
                (sym, ret)
            }
            _ => return None,
        };
        if rt_name.is_empty() {
            // Identity passthrough for testing::check_ok and friends.
            let v = args.first().and_then(|a| self.lower_expr(a))?;
            let dest = self.fresh(ret_ty);
            self.emit_assign(
                Place::local(dest),
                Rvalue::Use(Operand::Copy(Place::local(v))),
                span,
            );
            return Some(dest);
        }
        let mut arg_locals = Vec::with_capacity(args.len());
        for arg in args {
            let local = self.lower_expr(arg)?;
            // Stdlib helpers that accept Vec/Slice expect a *mut GosVec.
            // When the caller passes an array literal `[a, b]` the MIR
            // local has type Array{elem,len} (flat stack aggregate).
            // Coerce it here so every stdlib dispatch site gets the heap
            // pointer shape the runtime ABI requires.
            let local = {
                let lt = self.locals[local.0 as usize].ty;
                if let gossamer_types::TyKind::Array { elem, len } = self.tcx.kind_of(lt).clone() {
                    self.coerce_array_to_vec(local, elem, len, span)
                } else {
                    local
                }
            };
            arg_locals.push(local);
        }
        let dest = self.fresh(ret_ty);
        // Tag the destination's runtime shape so subsequent
        // method dispatches on the same local can pick the right
        // helper. Mirrors the shape the runtime helpers return.
        let runtime_kind: Option<&'static str> = match rt_name {
            "gos_rt_flag_set_new" => Some("flag::Set"),
            "gos_rt_bufio_scanner_new" => Some("bufio::Scanner"),
            "gos_rt_http_client_new" => Some("http::Client"),
            "gos_rt_http_request_send" => Some("http::Response"),
            "gos_rt_http_client_get" | "gos_rt_http_client_post" => Some("http::Request"),
            "gos_rt_http_response_text_new" | "gos_rt_http_response_json_new" => {
                Some("http::Response")
            }
            "gos_rt_error_new" | "gos_rt_error_wrap" | "gos_rt_errors_join_vec" => {
                Some("errors::Error")
            }
            "gos_rt_regex_compile" => Some("regex::Pattern"),
            "gos_rt_set_new" => Some("collections::HashSet"),
            "gos_rt_btmap_new" => Some("collections::BTreeMap"),
            "gos_rt_sync_map_new" => Some("sync::Map"),
            // 0.4.0 stateful HTTP types.
            "gos_rt_router_new" => Some("http::Router"),
            "gos_rt_file_server_new" => Some("http::FileServer"),
            "gos_rt_native_client_new" => Some("http::NativeClient"),
            "gos_rt_proxy_new" => Some("http::Proxy"),
            _ => None,
        };
        if let Some(rk) = runtime_kind {
            self.local_runtime_kind.insert(dest, rk);
        }
        // Pin element-struct tags so `xs[i].<field>` resolves
        // positionally even when the typechecker leaves the
        // element type as `Var(_)`.
        if matches!(rt_name, "gos_rt_fs_list_dir" | "gos_rt_fs_walk_dir") {
            // Match the registered name in `stdlib_struct_shapes`
            // so `entries[i].<field>` resolves to a positional
            // `Field(idx)` projection.
            self.local_elem_struct.insert(dest, "DirInfo".to_string());
        }
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(rt_name.to_string())),
            args: arg_locals
                .into_iter()
                .map(|l| Operand::Copy(Place::local(l)))
                .collect(),
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }
}
