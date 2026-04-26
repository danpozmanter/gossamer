//! Printing of types, patterns, and paths.

#![forbid(unsafe_code)]

use crate::expr::PathExpr;
use crate::pattern::{FieldPattern, Pattern, PatternKind};
use crate::ty::{GenericArg, Type, TypeKind, TypePath, TypePathSegment};

use super::Printer;

impl Printer {
    /// Renders a type expression.
    pub fn print_type(&mut self, ty: &Type) {
        match &ty.kind {
            TypeKind::Unit => self.write("()"),
            TypeKind::Never => self.write("!"),
            TypeKind::Infer => self.write("_"),
            TypeKind::Path(path) => self.print_type_path(path),
            TypeKind::Tuple(items) => self.print_tuple_type(items),
            TypeKind::Array { elem, len } => {
                self.write("[");
                self.print_type(elem);
                self.write("; ");
                self.print_expr(len);
                self.write("]");
            }
            TypeKind::Slice(elem) => {
                self.write("[");
                self.print_type(elem);
                self.write("]");
            }
            TypeKind::Ref { mutability, inner } => {
                self.write("&");
                if mutability.is_mutable() {
                    self.write("mut ");
                }
                self.print_type(inner);
            }
            TypeKind::Fn { kind, params, ret } => {
                self.write(kind.as_str());
                self.write("(");
                for (index, param) in params.iter().enumerate() {
                    if index > 0 {
                        self.write(", ");
                    }
                    self.print_type(param);
                }
                self.write(")");
                if let Some(ret) = ret {
                    self.write(" -> ");
                    self.print_type(ret);
                }
            }
        }
    }

    fn print_tuple_type(&mut self, items: &[Type]) {
        self.write("(");
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                self.write(", ");
            }
            self.print_type(item);
        }
        if items.len() == 1 {
            self.write(",");
        }
        self.write(")");
    }

    /// Renders a type path.
    pub fn print_type_path(&mut self, path: &TypePath) {
        for (index, segment) in path.segments.iter().enumerate() {
            if index > 0 {
                self.write("::");
            }
            self.print_type_path_segment(segment);
        }
    }

    fn print_type_path_segment(&mut self, segment: &TypePathSegment) {
        self.write_ident(&segment.name);
        if !segment.generics.is_empty() {
            self.write("<");
            for (index, arg) in segment.generics.iter().enumerate() {
                if index > 0 {
                    self.write(", ");
                }
                self.print_generic_arg(arg);
            }
            self.write(">");
        }
    }

    /// Renders a generic argument inside `<...>` or a turbofish.
    pub(super) fn print_generic_arg(&mut self, arg: &GenericArg) {
        match arg {
            GenericArg::Type(ty) => self.print_type(ty),
            GenericArg::Const(expr) => self.print_expr(expr),
        }
    }

    /// Renders an expression-position path (no turbofish).
    pub(super) fn print_path_expr(&mut self, path: &PathExpr) {
        for (index, segment) in path.segments.iter().enumerate() {
            if index > 0 {
                self.write("::");
            }
            self.write_ident(&segment.name);
            if !segment.generics.is_empty() {
                self.write("::<");
                for (gen_index, arg) in segment.generics.iter().enumerate() {
                    if gen_index > 0 {
                        self.write(", ");
                    }
                    self.print_generic_arg(arg);
                }
                self.write(">");
            }
        }
    }

    /// Renders a pattern.
    pub fn print_pattern(&mut self, pattern: &Pattern) {
        match &pattern.kind {
            PatternKind::Wildcard => self.write("_"),
            PatternKind::Rest => self.write(".."),
            PatternKind::Literal(lit) => self.print_literal(lit),
            PatternKind::Ident {
                mutability,
                name,
                subpattern,
            } => {
                if mutability.is_mutable() {
                    self.write("mut ");
                }
                self.write_ident(name);
                if let Some(sub) = subpattern {
                    self.write(" @ ");
                    self.print_pattern(sub);
                }
            }
            PatternKind::Path(path) => self.print_type_path(path),
            PatternKind::Tuple(items) => {
                self.write("(");
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        self.write(", ");
                    }
                    self.print_pattern(item);
                }
                if items.len() == 1 {
                    self.write(",");
                }
                self.write(")");
            }
            PatternKind::Struct { path, fields, rest } => {
                self.print_struct_pattern(path, fields, *rest);
            }
            PatternKind::TupleStruct { path, elems } => {
                self.print_type_path(path);
                self.write("(");
                for (index, elem) in elems.iter().enumerate() {
                    if index > 0 {
                        self.write(", ");
                    }
                    self.print_pattern(elem);
                }
                self.write(")");
            }
            PatternKind::Range { lo, hi, kind } => {
                self.print_literal(lo);
                self.write(kind.as_str());
                self.print_literal(hi);
            }
            PatternKind::Or(alts) => {
                for (index, alt) in alts.iter().enumerate() {
                    if index > 0 {
                        self.write(" | ");
                    }
                    self.print_pattern(alt);
                }
            }
            PatternKind::Ref { mutability, inner } => {
                self.write("&");
                if mutability.is_mutable() {
                    self.write("mut ");
                }
                self.print_pattern(inner);
            }
        }
    }

    fn print_struct_pattern(&mut self, path: &TypePath, fields: &[FieldPattern], rest: bool) {
        self.print_type_path(path);
        self.write(" {");
        let mut any = false;
        for (index, field) in fields.iter().enumerate() {
            if index > 0 {
                self.write(",");
            }
            self.write(" ");
            self.write_ident(&field.name);
            if let Some(sub) = &field.pattern {
                self.write(": ");
                self.print_pattern(sub);
            }
            any = true;
        }
        if rest {
            if any {
                self.write(", ..");
            } else {
                self.write(" ..");
            }
        }
        self.write(if any || rest { " }" } else { "}" });
    }
}
