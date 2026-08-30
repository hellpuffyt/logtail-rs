//! The logtail query language: lexer, parser, AST, and evaluator.

pub mod ast;
pub mod eval;
pub mod lexer;
pub mod parser;

pub use ast::Expr;
pub use eval::{eval, RegexCache};
pub use parser::{ParseError, Parser};

/// Parses a query string into an [`Expr`]. Convenience wrapper around
/// [`Parser::parse`].
///
/// # Errors
/// Returns a [`ParseError`] with a 1-based column position on malformed
/// input.
pub fn parse(src: &str) -> Result<Expr, ParseError> {
    Parser::parse(src)
}
