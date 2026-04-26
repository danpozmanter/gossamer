//! Pattern parsing (SPEC §5).

#![forbid(unsafe_code)]

use gossamer_ast::{FieldPattern, Ident, Literal, Mutability, Pattern, PatternKind, RangeKind};
use gossamer_lex::{Keyword, Punct, TokenKind};

use crate::diagnostic::ParseError;
use crate::parser::Parser;

impl Parser<'_> {
    /// Parses a pattern that may include top-level `|` alternatives.
    pub(crate) fn parse_pattern(&mut self) -> Pattern {
        self.enter_pattern_pipe();
        let first = self.parse_pattern_no_or();
        if !self.at_punct(Punct::Pipe) {
            self.leave_pattern_pipe();
            return first;
        }
        let mut alternatives = vec![first];
        while self.eat_punct(Punct::Pipe) {
            alternatives.push(self.parse_pattern_no_or());
        }
        self.leave_pattern_pipe();
        let start_span = alternatives
            .first()
            .map_or_else(|| self.peek_span(), |pattern| pattern.span);
        let end_span = alternatives
            .last()
            .map_or_else(|| self.peek_span(), |pattern| pattern.span);
        let span = self.join(start_span, end_span);
        let id = self.alloc_id();
        Pattern::new(id, span, PatternKind::Or(alternatives))
    }

    /// Parses a pattern that never accepts a top-level `|`.
    pub(crate) fn parse_pattern_no_or(&mut self) -> Pattern {
        let start_span = self.peek_span();
        let kind = self.parse_pattern_kind();
        let end_span = self.last_span();
        let span = self.join(start_span, end_span);
        let id = self.alloc_id();
        Pattern::new(id, span, kind)
    }

    fn parse_pattern_kind(&mut self) -> PatternKind {
        if self.at_punct(Punct::DotDot) || self.at_punct(Punct::DotDotEq) {
            return self.parse_range_pattern_or_rest();
        }
        if self.eat_punct(Punct::Amp) {
            let mutability = if self.eat_keyword(Keyword::Mut) {
                Mutability::Mutable
            } else {
                Mutability::Immutable
            };
            let inner = self.parse_pattern_no_or();
            return PatternKind::Ref {
                mutability,
                inner: Box::new(inner),
            };
        }
        if self.eat_punct(Punct::LParen) {
            return self.parse_tuple_pattern();
        }
        if self.eat_keyword(Keyword::Mut) {
            return self.parse_ident_pattern(Mutability::Mutable);
        }
        if let Some(literal) = self.try_parse_literal_pattern() {
            return self.maybe_range_pattern(literal);
        }
        if matches!(self.peek().kind, TokenKind::Ident)
            && is_wildcard_ident(self.slice(self.peek_span()))
        {
            self.bump();
            return PatternKind::Wildcard;
        }
        if self.is_path_start() {
            return self.parse_path_pattern();
        }
        self.record(
            ParseError::Unexpected {
                expected: "pattern".to_string(),
                found: self.peek_text(),
            },
            self.peek_span(),
        );
        self.bump();
        PatternKind::Wildcard
    }

    fn parse_range_pattern_or_rest(&mut self) -> PatternKind {
        self.bump();
        PatternKind::Rest
    }

    fn parse_tuple_pattern(&mut self) -> PatternKind {
        if self.eat_punct(Punct::RParen) {
            return PatternKind::Literal(Literal::Unit);
        }
        let mut elements = Vec::new();
        elements.push(self.parse_pattern());
        let mut saw_comma = false;
        while self.eat_punct(Punct::Comma) {
            saw_comma = true;
            if self.at_punct(Punct::RParen) {
                break;
            }
            elements.push(self.parse_pattern());
        }
        self.expect_punct(Punct::RParen, "to close tuple pattern");
        if elements.len() == 1 && !saw_comma {
            return elements.pop().expect("single-element tuple").kind;
        }
        PatternKind::Tuple(elements)
    }

    fn parse_ident_pattern(&mut self, mutability: Mutability) -> PatternKind {
        let token = self.peek();
        if !matches!(token.kind, TokenKind::Ident) {
            self.record(
                ParseError::Unexpected {
                    expected: "identifier in `mut` pattern".to_string(),
                    found: self.peek_text(),
                },
                token.span,
            );
            return PatternKind::Wildcard;
        }
        self.bump();
        let name = Ident::new(self.slice(token.span));
        let subpattern = if self.eat_punct(Punct::At) {
            Some(Box::new(self.parse_pattern_no_or()))
        } else {
            None
        };
        PatternKind::Ident {
            mutability,
            name,
            subpattern,
        }
    }

    fn try_parse_literal_pattern(&mut self) -> Option<Literal> {
        let token = self.peek();
        match token.kind {
            TokenKind::IntLit => {
                self.bump();
                Some(Literal::Int(self.slice(token.span).to_string()))
            }
            TokenKind::FloatLit => {
                self.bump();
                Some(Literal::Float(self.slice(token.span).to_string()))
            }
            TokenKind::StringLit | TokenKind::RawStringLit { .. } => {
                self.bump();
                Some(Literal::String(string_literal_value(self.slice(token.span))))
            }
            TokenKind::CharLit => {
                self.bump();
                Some(Literal::Char(char_literal_value(self.slice(token.span))))
            }
            TokenKind::ByteLit => {
                self.bump();
                Some(Literal::Byte(byte_literal_value(self.slice(token.span))))
            }
            TokenKind::Keyword(Keyword::True) => {
                self.bump();
                Some(Literal::Bool(true))
            }
            TokenKind::Keyword(Keyword::False) => {
                self.bump();
                Some(Literal::Bool(false))
            }
            TokenKind::Punct(Punct::Minus) => {
                if matches!(self.peek_nth(1).kind, TokenKind::IntLit | TokenKind::FloatLit) {
                    self.bump();
                    let number = self.peek();
                    self.bump();
                    let spelling = format!("-{}", self.slice(number.span));
                    if matches!(number.kind, TokenKind::IntLit) {
                        return Some(Literal::Int(spelling));
                    }
                    return Some(Literal::Float(spelling));
                }
                None
            }
            _ => None,
        }
    }

    fn maybe_range_pattern(&mut self, lo: Literal) -> PatternKind {
        if self.at_punct(Punct::DotDot) || self.at_punct(Punct::DotDotEq) {
            let kind = if self.eat_punct(Punct::DotDotEq) {
                RangeKind::Inclusive
            } else {
                self.bump();
                RangeKind::Exclusive
            };
            if let Some(hi) = self.try_parse_literal_pattern() {
                return PatternKind::Range { lo, hi, kind };
            }
        }
        PatternKind::Literal(lo)
    }

    fn is_path_start(&self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::Ident
                | TokenKind::Keyword(
                    Keyword::SelfUpper
                        | Keyword::SelfLower
                        | Keyword::Super
                        | Keyword::Crate
                )
        )
    }

    fn parse_path_pattern(&mut self) -> PatternKind {
        let start_span = self.peek_span();
        let path = self.parse_type_path();
        let is_single_ident = path.segments.len() == 1 && path.segments[0].generics.is_empty();
        if self.eat_punct(Punct::LParen) {
            let mut elements = Vec::new();
            while !self.at_punct(Punct::RParen) && !self.at_eof() {
                elements.push(self.parse_pattern());
                if !self.eat_punct(Punct::Comma) {
                    break;
                }
            }
            self.expect_punct(Punct::RParen, "to close tuple-struct pattern");
            return PatternKind::TupleStruct {
                path,
                elems: elements,
            };
        }
        if self.eat_punct(Punct::LBrace) {
            let (fields, rest) = self.parse_struct_pattern_fields();
            return PatternKind::Struct { path, fields, rest };
        }
        if is_single_ident {
            let name_text = path.segments[0].name.name.clone();
            if starts_with_uppercase(&name_text) {
                return PatternKind::Path(path);
            }
            if self.eat_punct(Punct::At) {
                let subpattern = Some(Box::new(self.parse_pattern_no_or()));
                return PatternKind::Ident {
                    mutability: Mutability::Immutable,
                    name: Ident::new(name_text),
                    subpattern,
                };
            }
            let _ = start_span;
            return PatternKind::Ident {
                mutability: Mutability::Immutable,
                name: Ident::new(name_text),
                subpattern: None,
            };
        }
        PatternKind::Path(path)
    }

    fn parse_struct_pattern_fields(&mut self) -> (Vec<FieldPattern>, bool) {
        let mut fields = Vec::new();
        let mut rest = false;
        while !self.at_punct(Punct::RBrace) && !self.at_eof() {
            if self.eat_punct(Punct::DotDot) {
                rest = true;
                break;
            }
            let name_span = self.peek_span();
            if !matches!(self.peek().kind, TokenKind::Ident) {
                self.record(
                    ParseError::Unexpected {
                        expected: "field name".to_string(),
                        found: self.peek_text(),
                    },
                    name_span,
                );
                self.bump();
                break;
            }
            let name = self.slice(name_span).to_string();
            self.bump();
            let pattern = if self.eat_punct(Punct::Colon) {
                Some(self.parse_pattern())
            } else {
                None
            };
            fields.push(FieldPattern {
                name: Ident::new(name),
                pattern,
            });
            if !self.eat_punct(Punct::Comma) {
                break;
            }
        }
        self.expect_punct(Punct::RBrace, "to close struct pattern");
        (fields, rest)
    }
}

/// Returns the decoded value of a double-quoted string literal. For now
/// the parser accepts the raw body between the quotes verbatim; future
/// phases may implement full escape decoding.
pub(crate) fn string_literal_value(source: &str) -> String {
    if let Some(stripped) = source.strip_prefix('"').and_then(|text| text.strip_suffix('"')) {
        return decode_string_escapes(stripped);
    }
    if let Some(stripped) = source
        .strip_prefix("r\"")
        .and_then(|text| text.strip_suffix('"'))
    {
        return stripped.to_string();
    }
    source.to_string()
}

fn decode_string_escapes(body: &str) -> String {
    let mut output = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(current) = chars.next() {
        if current != '\\' {
            output.push(current);
            continue;
        }
        match chars.next() {
            Some('n') => output.push('\n'),
            Some('r') => output.push('\r'),
            Some('t') => output.push('\t'),
            Some('\\') => output.push('\\'),
            Some('\'') => output.push('\''),
            Some('"') => output.push('"'),
            Some('0') => output.push('\0'),
            Some(other) => {
                output.push('\\');
                output.push(other);
            }
            None => output.push('\\'),
        }
    }
    output
}

/// Returns the decoded char value for a `'x'` literal.
pub(crate) fn char_literal_value(source: &str) -> char {
    let body = source.trim_start_matches('\'').trim_end_matches('\'');
    let decoded = decode_string_escapes(body);
    decoded.chars().next().unwrap_or('\0')
}

/// Returns the decoded byte value for a `b'x'` literal.
pub(crate) fn byte_literal_value(source: &str) -> u8 {
    let body = source.strip_prefix("b'").unwrap_or(source);
    let body = body.strip_suffix('\'').unwrap_or(body);
    let decoded = decode_string_escapes(body);
    decoded.bytes().next().unwrap_or(0)
}

fn is_wildcard_ident(text: &str) -> bool {
    text == "_"
}

fn starts_with_uppercase(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|character| character.is_ascii_uppercase())
}
