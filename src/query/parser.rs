//! Recursive-descent parser for the logtail query language.
//!
//! Grammar (lowest to highest precedence):
//!
//! ```text
//! expr       := or_expr
//! or_expr    := and_expr ( "or" and_expr )*
//! and_expr   := not_expr ( "and" not_expr )*
//! not_expr   := "not" not_expr | primary
//! primary    := "(" expr ")" | has_expr | comparison
//! has_expr   := "has" field
//! comparison := field ( cmp_op literal | "~" string | "contains" string )
//! field      := ident ( "." ident )*
//! cmp_op     := "==" | "!=" | ">" | ">=" | "<" | "<="
//! literal    := number | string | "true" | "false" | "null"
//! ```

use super::ast::{CmpOp, Expr, FieldPath, Literal};
use super::lexer::{LexError, Lexer, Token, TokenKind};
use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub message: String,
    pub col: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "column {}: {}", self.col, self.message)
    }
}

impl From<LexError> for ParseError {
    fn from(e: LexError) -> Self {
        ParseError {
            message: e.message,
            col: e.col,
        }
    }
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    /// Parses `src` into an [`Expr`]. An empty (or all-whitespace) query
    /// parses to [`Expr::True`], matching every record.
    ///
    /// # Errors
    /// Returns a [`ParseError`] with a 1-based column position on the first
    /// lexing or syntax error encountered.
    pub fn parse(src: &str) -> Result<Expr, ParseError> {
        let trimmed = src.trim();
        if trimmed.is_empty() {
            return Ok(Expr::True);
        }
        let tokens = Lexer::new(src).tokenize()?;
        let mut parser = Parser { tokens, pos: 0 };
        let expr = parser.parse_or()?;
        parser.expect_eof()?;
        Ok(expr)
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn advance(&mut self) -> Token {
        let t = self.tokens[self.pos].clone();
        if self.pos + 1 < self.tokens.len() {
            self.pos += 1;
        }
        t
    }

    fn expect_eof(&self) -> Result<(), ParseError> {
        if matches!(self.peek().kind, TokenKind::Eof) {
            Ok(())
        } else {
            Err(ParseError {
                message: format!("unexpected trailing token {}", self.peek().kind),
                col: self.peek().col,
            })
        }
    }

    fn parse_or(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek().kind, TokenKind::Or) {
            self.advance();
            let rhs = self.parse_and()?;
            lhs = Expr::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_not()?;
        while matches!(self.peek().kind, TokenKind::And) {
            self.advance();
            let rhs = self.parse_not()?;
            lhs = Expr::And(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_not(&mut self) -> Result<Expr, ParseError> {
        if matches!(self.peek().kind, TokenKind::Not) {
            self.advance();
            let inner = self.parse_not()?;
            return Ok(Expr::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        match &self.peek().kind {
            TokenKind::LParen => {
                self.advance();
                let inner = self.parse_or()?;
                self.expect(&TokenKind::RParen)?;
                Ok(inner)
            }
            TokenKind::Has => {
                self.advance();
                let field = self.parse_field()?;
                Ok(Expr::Has { field })
            }
            TokenKind::Ident(_) => self.parse_comparison(),
            other => Err(ParseError {
                message: format!("expected a query expression, found {other}"),
                col: self.peek().col,
            }),
        }
    }

    fn parse_comparison(&mut self) -> Result<Expr, ParseError> {
        let field = self.parse_field()?;
        let tok = self.peek().clone();
        let op = match &tok.kind {
            TokenKind::Eq => CmpOp::Eq,
            TokenKind::Ne => CmpOp::Ne,
            TokenKind::Gt => CmpOp::Gt,
            TokenKind::Ge => CmpOp::Ge,
            TokenKind::Lt => CmpOp::Lt,
            TokenKind::Le => CmpOp::Le,
            TokenKind::Tilde => {
                self.advance();
                let pattern = self.parse_string_literal()?;
                return Ok(Expr::Match { field, pattern });
            }
            TokenKind::Contains => {
                self.advance();
                let needle = self.parse_string_literal()?;
                return Ok(Expr::Contains { field, needle });
            }
            other => {
                return Err(ParseError {
                    message: format!(
                        "expected a comparison operator (==, !=, >, >=, <, <=, ~, contains) after field, found {other}"
                    ),
                    col: tok.col,
                })
            }
        };
        self.advance();
        let value = self.parse_literal()?;
        Ok(Expr::Compare { field, op, value })
    }

    fn parse_field(&mut self) -> Result<FieldPath, ParseError> {
        let mut path = Vec::new();
        let first = self.advance();
        match first.kind {
            TokenKind::Ident(name) => path.push(name),
            other => {
                return Err(ParseError {
                    message: format!("expected a field name, found {other}"),
                    col: first.col,
                })
            }
        }
        while matches!(self.peek().kind, TokenKind::Dot) {
            self.advance();
            let tok = self.advance();
            match tok.kind {
                TokenKind::Ident(name) => path.push(name),
                other => {
                    return Err(ParseError {
                        message: format!("expected a field name after `.`, found {other}"),
                        col: tok.col,
                    })
                }
            }
        }
        Ok(path)
    }

    fn parse_string_literal(&mut self) -> Result<String, ParseError> {
        let tok = self.advance();
        match tok.kind {
            TokenKind::String(s) => Ok(s),
            other => Err(ParseError {
                message: format!("expected a string literal, found {other}"),
                col: tok.col,
            }),
        }
    }

    fn parse_literal(&mut self) -> Result<Literal, ParseError> {
        let tok = self.advance();
        match tok.kind {
            TokenKind::Number(n) => Ok(Literal::Number(n)),
            TokenKind::String(s) => Ok(Literal::String(s)),
            TokenKind::True => Ok(Literal::Bool(true)),
            TokenKind::False => Ok(Literal::Bool(false)),
            TokenKind::Null => Ok(Literal::Null),
            other => Err(ParseError {
                message: format!(
                    "expected a value (number, string, true, false, or null), found {other}"
                ),
                col: tok.col,
            }),
        }
    }

    fn expect(&mut self, kind: &TokenKind) -> Result<(), ParseError> {
        if &self.peek().kind == kind {
            self.advance();
            Ok(())
        } else {
            Err(ParseError {
                message: format!("expected {kind}, found {}", self.peek().kind),
                col: self.peek().col,
            })
        }
    }
}
