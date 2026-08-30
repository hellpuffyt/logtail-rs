//! Evaluates a parsed [`Expr`] against a JSON record.

use super::ast::{CmpOp, Expr, FieldPath, Literal};
use regex::Regex;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;

/// Looks up a dotted field path inside a JSON object. Returns `None` if any
/// segment is missing or the record is not an object at that point.
#[must_use]
pub fn lookup<'a>(record: &'a Value, path: &FieldPath) -> Option<&'a Value> {
    let mut current = record;
    for segment in path {
        current = current.as_object()?.get(segment)?;
    }
    Some(current)
}

/// Compares a JSON value against a literal using `op`. Type mismatches and
/// missing values are not errors; they simply evaluate to `false`, matching
/// the "null handling" behavior of most log query languages.
fn compare(value: &Value, op: CmpOp, lit: &Literal) -> bool {
    match (value, lit) {
        (Value::Number(n), Literal::Number(x)) => {
            let Some(n) = n.as_f64() else { return false };
            match op {
                CmpOp::Eq => (n - x).abs() < f64::EPSILON,
                CmpOp::Ne => (n - x).abs() >= f64::EPSILON,
                CmpOp::Gt => n > *x,
                CmpOp::Ge => n >= *x,
                CmpOp::Lt => n < *x,
                CmpOp::Le => n <= *x,
            }
        }
        (Value::String(s), Literal::String(x)) => match op {
            CmpOp::Eq => s == x,
            CmpOp::Ne => s != x,
            CmpOp::Gt => s.as_str() > x.as_str(),
            CmpOp::Ge => s.as_str() >= x.as_str(),
            CmpOp::Lt => s.as_str() < x.as_str(),
            CmpOp::Le => s.as_str() <= x.as_str(),
        },
        (Value::Bool(b), Literal::Bool(x)) => match op {
            CmpOp::Eq => b == x,
            CmpOp::Ne => b != x,
            _ => false,
        },
        (Value::Null, Literal::Null) => matches!(op, CmpOp::Eq),
        (_, Literal::Null) => matches!(op, CmpOp::Ne),
        _ => false,
    }
}

/// Cache of compiled regexes so repeated evaluation of the same query
/// (once per record) does not recompile the pattern every time.
pub struct RegexCache {
    inner: Mutex<HashMap<String, Regex>>,
}

impl Default for RegexCache {
    fn default() -> Self {
        RegexCache {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

impl RegexCache {
    #[allow(clippy::unwrap_used)] // poisoning is unreachable: no panics occur while the lock is held
    fn is_match(&self, pattern: &str, text: &str) -> Result<bool, regex::Error> {
        let mut guard = self.inner.lock().unwrap();
        if let Some(re) = guard.get(pattern) {
            return Ok(re.is_match(text));
        }
        let re = Regex::new(pattern)?;
        let matched = re.is_match(text);
        guard.insert(pattern.to_string(), re);
        Ok(matched)
    }
}

/// Evaluates `expr` against `record`, using `cache` to avoid recompiling
/// regex patterns for `~` matches on every call.
///
/// # Errors
/// Returns an error if a `~` pattern fails to compile as a regex.
pub fn eval(expr: &Expr, record: &Value, cache: &RegexCache) -> Result<bool, regex::Error> {
    match expr {
        Expr::True => Ok(true),
        Expr::Has { field } => Ok(lookup(record, field).is_some_and(|v| !v.is_null())),
        Expr::Compare { field, op, value } => {
            Ok(lookup(record, field).is_some_and(|v| compare(v, *op, value)))
        }
        Expr::Match { field, pattern } => {
            let Some(v) = lookup(record, field) else {
                return Ok(false);
            };
            let Some(s) = v.as_str() else {
                return Ok(false);
            };
            cache.is_match(pattern, s)
        }
        Expr::Contains { field, needle } => {
            let Some(v) = lookup(record, field) else {
                return Ok(false);
            };
            Ok(v.as_str().is_some_and(|s| s.contains(needle.as_str())))
        }
        Expr::Not(inner) => eval(inner, record, cache).map(|b| !b),
        Expr::And(lhs, rhs) => Ok(eval(lhs, record, cache)? && eval(rhs, record, cache)?),
        Expr::Or(lhs, rhs) => Ok(eval(lhs, record, cache)? || eval(rhs, record, cache)?),
    }
}
