//! Integration tests for the query language: parsing, precedence,
//! malformed-query error positions, and evaluation semantics.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use logtail::query::ast::{CmpOp, Expr, Literal};
use logtail::query::{eval, parse, RegexCache};
use serde_json::json;

#[test]
fn empty_query_matches_everything() {
    let expr = parse("").unwrap();
    assert_eq!(expr, Expr::True);
    let cache = RegexCache::default();
    assert!(eval(&expr, &json!({"a": 1}), &cache).unwrap());
}

#[test]
fn parses_basic_comparison_operators() {
    for (op_str, op) in [
        ("==", CmpOp::Eq),
        ("!=", CmpOp::Ne),
        (">", CmpOp::Gt),
        (">=", CmpOp::Ge),
        ("<", CmpOp::Lt),
        ("<=", CmpOp::Le),
    ] {
        let query = format!("status {op_str} 500");
        let expr = parse(&query).unwrap();
        assert_eq!(
            expr,
            Expr::Compare {
                field: vec!["status".to_string()],
                op,
                value: Literal::Number(500.0),
            },
            "failed for operator {op_str}"
        );
    }
}

#[test]
fn parses_nested_field_access() {
    let expr = parse(r#"http.request.method == "GET""#).unwrap();
    match expr {
        Expr::Compare { field, .. } => {
            assert_eq!(field, vec!["http", "request", "method"]);
        }
        _ => panic!("expected Compare"),
    }
}

#[test]
fn parses_regex_match() {
    let expr = parse(r#"path ~ "^/api""#).unwrap();
    assert_eq!(
        expr,
        Expr::Match {
            field: vec!["path".to_string()],
            pattern: "^/api".to_string(),
        }
    );
}

#[test]
fn parses_contains() {
    let expr = parse(r#"msg contains "timeout""#).unwrap();
    assert_eq!(
        expr,
        Expr::Contains {
            field: vec!["msg".to_string()],
            needle: "timeout".to_string(),
        }
    );
}

#[test]
fn parses_has() {
    let expr = parse("has error").unwrap();
    assert_eq!(
        expr,
        Expr::Has {
            field: vec!["error".to_string()]
        }
    );
}

#[test]
fn parses_bool_and_null_literals() {
    assert_eq!(
        parse("ok == true").unwrap(),
        Expr::Compare {
            field: vec!["ok".to_string()],
            op: CmpOp::Eq,
            value: Literal::Bool(true),
        }
    );
    assert_eq!(
        parse("err == null").unwrap(),
        Expr::Compare {
            field: vec!["err".to_string()],
            op: CmpOp::Eq,
            value: Literal::Null,
        }
    );
}

#[test]
fn or_binds_looser_than_and() {
    // "a or b and c" must parse as "a or (b and c)"
    let expr = parse("a == 1 or b == 2 and c == 3").unwrap();
    match expr {
        Expr::Or(lhs, rhs) => {
            assert!(matches!(*lhs, Expr::Compare { .. }));
            assert!(matches!(*rhs, Expr::And(_, _)));
        }
        _ => panic!("expected top-level Or, got {expr:?}"),
    }
}

#[test]
fn and_of_and_is_left_associative_shape() {
    let expr = parse("a == 1 and b == 2 and c == 3").unwrap();
    match expr {
        Expr::And(lhs, rhs) => {
            assert!(matches!(*lhs, Expr::And(_, _)));
            assert!(matches!(*rhs, Expr::Compare { .. }));
        }
        _ => panic!("expected And, got {expr:?}"),
    }
}

#[test]
fn parentheses_override_precedence() {
    // "(a or b) and c" must NOT parse the same as "a or b and c"
    let with_parens = parse("(a == 1 or b == 2) and c == 3").unwrap();
    match with_parens {
        Expr::And(lhs, rhs) => {
            assert!(matches!(*lhs, Expr::Or(_, _)));
            assert!(matches!(*rhs, Expr::Compare { .. }));
        }
        _ => panic!("expected top-level And, got {with_parens:?}"),
    }
}

#[test]
fn not_binds_tighter_than_and_or() {
    let expr = parse("not a == 1 and b == 2").unwrap();
    match expr {
        Expr::And(lhs, _) => assert!(matches!(*lhs, Expr::Not(_))),
        _ => panic!("expected And at top level, got {expr:?}"),
    }
}

#[test]
fn double_not_parses() {
    let expr = parse("not not a == 1").unwrap();
    match expr {
        Expr::Not(inner) => assert!(matches!(*inner, Expr::Not(_))),
        _ => panic!("expected Not(Not(_)), got {expr:?}"),
    }
}

#[test]
fn complex_expression_evaluates_correctly() {
    let expr = parse(r#"status >= 500 and not path contains "/health""#).unwrap();
    let cache = RegexCache::default();
    assert!(eval(&expr, &json!({"status": 503, "path": "/api/foo"}), &cache).unwrap());
    assert!(!eval(&expr, &json!({"status": 200, "path": "/api/foo"}), &cache).unwrap());
    assert!(!eval(&expr, &json!({"status": 503, "path": "/health"}), &cache).unwrap());
}

#[test]
fn malformed_query_reports_column_not_panic() {
    let err = parse("status >=").unwrap_err();
    assert!(err.col > 0);

    let err = parse("status = 500").unwrap_err();
    assert_eq!(err.col, 8); // the lone `=`

    let err = parse("(status == 500").unwrap_err();
    assert!(err.col > 0);

    let err = parse("status == 500)").unwrap_err();
    assert!(err.col > 0);

    let err = parse("and status == 1").unwrap_err();
    assert!(err.col > 0);
}

#[test]
fn unterminated_string_reports_error_not_panic() {
    let err = parse(r#"path == "unterminated"#).unwrap_err();
    assert!(err.col > 0);
}

#[test]
fn invalid_operator_reports_error() {
    let err = parse("status <> 500").unwrap_err();
    assert!(err.col > 0);
}

#[test]
fn has_with_missing_field_is_false() {
    let expr = parse("has error").unwrap();
    let cache = RegexCache::default();
    assert!(!eval(&expr, &json!({"other": 1}), &cache).unwrap());
    assert!(eval(&expr, &json!({"error": "boom"}), &cache).unwrap());
}

#[test]
fn has_with_null_field_is_false() {
    let expr = parse("has error").unwrap();
    let cache = RegexCache::default();
    assert!(!eval(&expr, &json!({"error": null}), &cache).unwrap());
}

#[test]
fn null_equality_semantics() {
    let cache = RegexCache::default();
    let is_null = parse("error == null").unwrap();
    assert!(eval(&is_null, &json!({"error": null}), &cache).unwrap());
    assert!(!eval(&is_null, &json!({"error": "boom"}), &cache).unwrap());

    let not_null = parse("error != null").unwrap();
    assert!(eval(&not_null, &json!({"error": "boom"}), &cache).unwrap());
    assert!(!eval(&not_null, &json!({"error": null}), &cache).unwrap());
    // missing field is treated as absent, not equal to a non-null comparison
    assert!(!eval(&not_null, &json!({}), &cache).unwrap());
}

#[test]
fn type_mismatch_comparison_is_false_not_error() {
    let expr = parse(r#"status == "500""#).unwrap();
    let cache = RegexCache::default();
    // status is a number in the record but the query compares to a string
    assert!(!eval(&expr, &json!({"status": 500}), &cache).unwrap());
}

#[test]
fn string_ordering_comparison() {
    let expr = parse(r#"level > "info""#).unwrap();
    let cache = RegexCache::default();
    assert!(eval(&expr, &json!({"level": "warn"}), &cache).unwrap());
    assert!(!eval(&expr, &json!({"level": "debug"}), &cache).unwrap());
}

#[test]
fn regex_match_against_missing_or_non_string_field_is_false() {
    let expr = parse(r#"path ~ "^/api""#).unwrap();
    let cache = RegexCache::default();
    assert!(!eval(&expr, &json!({}), &cache).unwrap());
    assert!(!eval(&expr, &json!({"path": 500}), &cache).unwrap());
}

#[test]
fn invalid_regex_pattern_returns_error_not_panic() {
    let expr = parse(r#"path ~ "(unclosed""#).unwrap();
    let cache = RegexCache::default();
    assert!(eval(&expr, &json!({"path": "/api"}), &cache).is_err());
}
