//! Lexer for the Gossamer language.
//! Converts a UTF-8 source string into a stream of `Token` values with
//! precise byte-range `Span`s and recoverable diagnostics. Populated as
//! part of of the implementation plan.

// `deny` rather than `forbid`: the symbol interner needs exactly one
// contained `unsafe` block (`reset_interner` reclaims the leak-on-purpose
// arena it hands out as `&'static str`), which `forbid` cannot permit.
#![deny(unsafe_code)]

mod comment;
mod cursor;
mod diagnostic;
mod lexer;
mod number;
mod punct;
mod source_map;
mod span;
mod string;
mod symbol;
mod token;

pub use diagnostic::LexError;
pub use lexer::{Lexer, tokenize};
pub use source_map::SourceMap;
pub use span::{FileId, LineCol, Span};
pub use symbol::{Symbol, reset_interner};
pub use token::{Keyword, Punct, Token, TokenKind};
