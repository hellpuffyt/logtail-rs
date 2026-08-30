//! Tokenizer for the logtail query language.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    Ident(String),
    Number(f64),
    String(String),
    And,
    Or,
    Not,
    Has,
    Contains,
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Tilde,
    Dot,
    LParen,
    RParen,
    True,
    False,
    Null,
    Eof,
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TokenKind::Ident(s) => write!(f, "identifier `{s}`"),
            TokenKind::Number(n) => write!(f, "number `{n}`"),
            TokenKind::String(s) => write!(f, "string \"{s}\""),
            TokenKind::And => write!(f, "`and`"),
            TokenKind::Or => write!(f, "`or`"),
            TokenKind::Not => write!(f, "`not`"),
            TokenKind::Has => write!(f, "`has`"),
            TokenKind::Contains => write!(f, "`contains`"),
            TokenKind::Eq => write!(f, "`==`"),
            TokenKind::Ne => write!(f, "`!=`"),
            TokenKind::Gt => write!(f, "`>`"),
            TokenKind::Ge => write!(f, "`>=`"),
            TokenKind::Lt => write!(f, "`<`"),
            TokenKind::Le => write!(f, "`<=`"),
            TokenKind::Tilde => write!(f, "`~`"),
            TokenKind::Dot => write!(f, "`.`"),
            TokenKind::LParen => write!(f, "`(`"),
            TokenKind::RParen => write!(f, "`)`"),
            TokenKind::True => write!(f, "`true`"),
            TokenKind::False => write!(f, "`false`"),
            TokenKind::Null => write!(f, "`null`"),
            TokenKind::Eof => write!(f, "end of input"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    /// 1-based column position where the token starts.
    pub col: usize,
}

/// A lexing error with a 1-based column position.
#[derive(Debug, Clone, PartialEq)]
pub struct LexError {
    pub message: String,
    pub col: usize,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "column {}: {}", self.col, self.message)
    }
}

pub struct Lexer<'a> {
    chars: Vec<(usize, char)>,
    src: &'a str,
    pos: usize,
}

impl<'a> Lexer<'a> {
    #[must_use]
    pub fn new(src: &'a str) -> Self {
        Lexer {
            chars: src.char_indices().collect(),
            src,
            pos: 0,
        }
    }

    fn peek_char(&self) -> Option<char> {
        self.chars.get(self.pos).map(|(_, c)| *c)
    }

    fn peek_char_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).map(|(_, c)| *c)
    }

    fn col_at(&self, idx: usize) -> usize {
        self.chars.get(idx).map_or(self.src.len() + 1, |(b, _)| {
            self.src[..*b].chars().count() + 1
        })
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek_char();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek_char(), Some(c) if c.is_whitespace()) {
            self.pos += 1;
        }
    }

    /// Tokenizes the whole input in one pass.
    ///
    /// # Errors
    /// Returns a [`LexError`] with a 1-based column position on the first
    /// invalid character, unterminated string, or malformed number.
    #[allow(clippy::too_many_lines)]
    pub fn tokenize(mut self) -> Result<Vec<Token>, LexError> {
        let mut tokens = Vec::new();
        loop {
            self.skip_whitespace();
            let start_col = self.col_at(self.pos);
            let Some(c) = self.peek_char() else {
                tokens.push(Token {
                    kind: TokenKind::Eof,
                    col: start_col,
                });
                break;
            };

            match c {
                '(' => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::LParen,
                        col: start_col,
                    });
                }
                ')' => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::RParen,
                        col: start_col,
                    });
                }
                '.' => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::Dot,
                        col: start_col,
                    });
                }
                '~' => {
                    self.advance();
                    tokens.push(Token {
                        kind: TokenKind::Tilde,
                        col: start_col,
                    });
                }
                '=' => {
                    self.advance();
                    if self.peek_char() == Some('=') {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::Eq,
                            col: start_col,
                        });
                    } else {
                        return Err(LexError {
                            message: "expected `==`, found a single `=`".to_string(),
                            col: start_col,
                        });
                    }
                }
                '!' => {
                    self.advance();
                    if self.peek_char() == Some('=') {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::Ne,
                            col: start_col,
                        });
                    } else {
                        return Err(LexError {
                            message: "expected `!=`, found a lone `!`".to_string(),
                            col: start_col,
                        });
                    }
                }
                '>' => {
                    self.advance();
                    if self.peek_char() == Some('=') {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::Ge,
                            col: start_col,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Gt,
                            col: start_col,
                        });
                    }
                }
                '<' => {
                    self.advance();
                    if self.peek_char() == Some('=') {
                        self.advance();
                        tokens.push(Token {
                            kind: TokenKind::Le,
                            col: start_col,
                        });
                    } else {
                        tokens.push(Token {
                            kind: TokenKind::Lt,
                            col: start_col,
                        });
                    }
                }
                '"' => {
                    self.advance();
                    let mut s = String::new();
                    loop {
                        match self.advance() {
                            Some('"') => break,
                            Some('\\') => match self.advance() {
                                Some('n') => s.push('\n'),
                                Some('t') => s.push('\t'),
                                Some('"') => s.push('"'),
                                Some('\\') => s.push('\\'),
                                Some(other) => s.push(other),
                                None => {
                                    return Err(LexError {
                                        message: "unterminated string escape".to_string(),
                                        col: start_col,
                                    })
                                }
                            },
                            Some(other) => s.push(other),
                            None => {
                                return Err(LexError {
                                    message: "unterminated string literal".to_string(),
                                    col: start_col,
                                })
                            }
                        }
                    }
                    tokens.push(Token {
                        kind: TokenKind::String(s),
                        col: start_col,
                    });
                }
                c if c.is_ascii_digit()
                    || (c == '-' && self.peek_char_at(1).is_some_and(|c2| c2.is_ascii_digit())) =>
                {
                    let start = self.pos;
                    self.advance();
                    while matches!(self.peek_char(), Some(c) if c.is_ascii_digit() || c == '.' || c == 'e' || c == 'E' || c == '-' || c == '+')
                    {
                        self.advance();
                    }
                    let text: String = self.chars[start..self.pos]
                        .iter()
                        .map(|(_, c)| *c)
                        .collect();
                    let n: f64 = text.parse().map_err(|_| LexError {
                        message: format!("invalid number literal `{text}`"),
                        col: start_col,
                    })?;
                    tokens.push(Token {
                        kind: TokenKind::Number(n),
                        col: start_col,
                    });
                }
                c if c.is_alphabetic() || c == '_' => {
                    let start = self.pos;
                    while matches!(self.peek_char(), Some(c) if c.is_alphanumeric() || c == '_' || c == '-')
                    {
                        self.advance();
                    }
                    let text: String = self.chars[start..self.pos]
                        .iter()
                        .map(|(_, c)| *c)
                        .collect();
                    let kind = match text.as_str() {
                        "and" => TokenKind::And,
                        "or" => TokenKind::Or,
                        "not" => TokenKind::Not,
                        "has" => TokenKind::Has,
                        "contains" => TokenKind::Contains,
                        "true" => TokenKind::True,
                        "false" => TokenKind::False,
                        "null" => TokenKind::Null,
                        _ => TokenKind::Ident(text),
                    };
                    tokens.push(Token {
                        kind,
                        col: start_col,
                    });
                }
                other => {
                    return Err(LexError {
                        message: format!("unexpected character `{other}`"),
                        col: start_col,
                    });
                }
            }
        }
        Ok(tokens)
    }
}
