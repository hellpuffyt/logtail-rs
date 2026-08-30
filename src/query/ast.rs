//! Abstract syntax tree for the logtail query language.

/// A field path, e.g. `http.request.method` becomes `["http", "request", "method"]`.
pub type FieldPath = Vec<String>;

/// A literal value that can appear on the right-hand side of a comparison.
#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Number(f64),
    String(String),
    Bool(bool),
    Null,
}

/// Comparison operators for `field OP value` expressions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

/// A parsed boolean expression tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// `field OP literal`
    Compare {
        field: FieldPath,
        op: CmpOp,
        value: Literal,
    },
    /// `field ~ "regex"`
    Match { field: FieldPath, pattern: String },
    /// `field contains "substr"`
    Contains { field: FieldPath, needle: String },
    /// `has field`
    Has { field: FieldPath },
    /// `not expr`
    Not(Box<Expr>),
    /// `lhs and rhs`
    And(Box<Expr>, Box<Expr>),
    /// `lhs or rhs`
    Or(Box<Expr>, Box<Expr>),
    /// Always true - the empty query matches everything.
    True,
}
