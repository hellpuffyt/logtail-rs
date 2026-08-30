//! Helpers for reading fields out of a JSON log record, and for parsing
//! NDJSON lines while tolerating malformed input.

use serde_json::Value;

/// Looks up a dotted field path (`a.b.c`) and returns it as `f64` if it is a
/// JSON number. Returns `None` for missing fields, non-numeric fields, or a
/// path that walks through a non-object.
#[must_use]
pub fn lookup_number(record: &Value, path: &str) -> Option<f64> {
    lookup(record, path)?.as_f64()
}

/// Looks up a dotted field path and renders it as a display string, used as
/// a group-by key. Numbers, strings, and bools all get a stable textual
/// form; missing fields return `None`.
#[must_use]
pub fn lookup_display(record: &Value, path: &str) -> Option<String> {
    let v = lookup(record, path)?;
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// Looks up a dotted field path inside a JSON object.
#[must_use]
pub fn lookup<'a>(record: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = record;
    for segment in path.split('.') {
        current = current.as_object()?.get(segment)?;
    }
    Some(current)
}

/// Result of parsing one line of an NDJSON log.
pub enum ParsedLine {
    /// Successfully parsed JSON object.
    Record(Value),
    /// A blank line, silently skipped (not counted as malformed).
    Blank,
    /// A line that failed to parse as JSON, or parsed but was not a JSON
    /// object (`{...}`) at the top level.
    Malformed,
}

/// Parses one NDJSON line into a [`ParsedLine`]. Never panics on malformed
/// input; the caller is expected to track a counter of [`ParsedLine::Malformed`]
/// results rather than aborting the stream.
#[must_use]
pub fn parse_line(line: &str) -> ParsedLine {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return ParsedLine::Blank;
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(v @ Value::Object(_)) => ParsedLine::Record(v),
        Ok(_) | Err(_) => ParsedLine::Malformed,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn looks_up_top_level_field() {
        let v = json!({"status": 200});
        assert_eq!(lookup_number(&v, "status"), Some(200.0));
    }

    #[test]
    fn looks_up_nested_field() {
        let v = json!({"http": {"request": {"method": "GET"}}});
        assert_eq!(lookup(&v, "http.request.method").unwrap(), "GET");
    }

    #[test]
    fn missing_nested_field_returns_none() {
        let v = json!({"http": {"request": {}}});
        assert!(lookup(&v, "http.request.method").is_none());
    }

    #[test]
    fn walking_through_non_object_returns_none() {
        let v = json!({"a": 5});
        assert!(lookup(&v, "a.b").is_none());
    }

    #[test]
    fn parses_valid_json_line() {
        match parse_line(r#"{"a": 1}"#) {
            ParsedLine::Record(v) => assert_eq!(v["a"], 1),
            _ => panic!("expected Record"),
        }
    }

    #[test]
    fn blank_line_is_not_malformed() {
        assert!(matches!(parse_line(""), ParsedLine::Blank));
        assert!(matches!(parse_line("   "), ParsedLine::Blank));
    }

    #[test]
    fn invalid_json_is_malformed() {
        assert!(matches!(parse_line("{not json"), ParsedLine::Malformed));
    }

    #[test]
    fn non_object_top_level_is_malformed() {
        assert!(matches!(parse_line("[1,2,3]"), ParsedLine::Malformed));
        assert!(matches!(parse_line("42"), ParsedLine::Malformed));
        assert!(matches!(
            parse_line("\"just a string\""),
            ParsedLine::Malformed
        ));
    }

    #[test]
    fn display_string_for_group_key() {
        let v = json!({"s": "hello", "n": 5, "b": true});
        assert_eq!(lookup_display(&v, "s"), Some("hello".to_string()));
        assert_eq!(lookup_display(&v, "n"), Some("5".to_string()));
        assert_eq!(lookup_display(&v, "b"), Some("true".to_string()));
    }
}
