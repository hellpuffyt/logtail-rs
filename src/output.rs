//! Rendering aggregation results and raw filtered records as a table, a
//! single JSON document, or newline-delimited JSON for piping.

use crate::agg::Aggregator;
use serde_json::{json, Value};
use std::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    Table,
    Json,
    Ndjson,
}

impl OutputFormat {
    #[must_use]
    pub fn parse(s: &str) -> Option<OutputFormat> {
        match s.to_ascii_lowercase().as_str() {
            "table" => Some(OutputFormat::Table),
            "json" => Some(OutputFormat::Json),
            "ndjson" | "jsonl" => Some(OutputFormat::Ndjson),
            _ => None,
        }
    }
}

#[allow(clippy::cast_possible_truncation)]
fn fmt_num(v: f64) -> String {
    if v.fract().abs() < f64::EPSILON && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v:.3}")
    }
}

/// One row of aggregation output: the group-by key values plus the
/// requested aggregation values.
struct Row {
    key: Vec<String>,
    values: Vec<Option<f64>>,
}

fn collect_rows(agg: &Aggregator) -> Vec<Row> {
    let mut rows: Vec<Row> = agg
        .groups()
        .iter()
        .map(|(key, state)| Row {
            key: key.clone(),
            values: agg.kinds().iter().map(|k| state.value(k)).collect(),
        })
        .collect();
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    rows
}

/// Renders the aggregation result of `agg` in the requested format.
#[must_use]
pub fn render_aggregation(agg: &Aggregator, format: OutputFormat) -> String {
    let rows = collect_rows(agg);
    match format {
        OutputFormat::Table => render_table(agg, &rows),
        OutputFormat::Json => render_json(agg, &rows),
        OutputFormat::Ndjson => render_ndjson(agg, &rows),
    }
}

fn render_table(agg: &Aggregator, rows: &[Row]) -> String {
    let mut headers: Vec<String> = agg.group_by().to_vec();
    for k in agg.kinds() {
        headers.push(k.to_string());
    }

    let mut cell_rows: Vec<Vec<String>> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut cells = row.key.clone();
        for v in &row.values {
            cells.push(v.map_or_else(|| "-".to_string(), fmt_num));
        }
        cell_rows.push(cells);
    }

    let mut widths: Vec<usize> = headers.iter().map(std::string::String::len).collect();
    for cells in &cell_rows {
        for (i, c) in cells.iter().enumerate() {
            widths[i] = widths[i].max(c.len());
        }
    }

    let mut out = String::new();
    for (i, h) in headers.iter().enumerate() {
        let _ = write!(out, "{h:<width$}  ", width = widths[i]);
    }
    out.push('\n');
    for w in &widths {
        let _ = write!(out, "{}  ", "-".repeat(*w));
    }
    out.push('\n');
    for cells in &cell_rows {
        for (i, c) in cells.iter().enumerate() {
            let _ = write!(out, "{c:<width$}  ", width = widths[i]);
        }
        out.push('\n');
    }

    if agg.is_truncated() {
        let _ = writeln!(
            out,
            "\n(warning: group-by cardinality limit reached; {} additional value(s) folded into an overflow bucket)",
            agg.truncated_count()
        );
    }
    out
}

fn row_to_json(agg: &Aggregator, row: &Row) -> Value {
    let mut obj = serde_json::Map::new();
    for (field, key_val) in agg.group_by().iter().zip(&row.key) {
        obj.insert(field.clone(), Value::String(key_val.clone()));
    }
    for (kind, val) in agg.kinds().iter().zip(&row.values) {
        let v = match val {
            Some(x) => json!(x),
            None => Value::Null,
        };
        obj.insert(kind.to_string(), v);
    }
    Value::Object(obj)
}

fn render_json(agg: &Aggregator, rows: &[Row]) -> String {
    let arr: Vec<Value> = rows.iter().map(|r| row_to_json(agg, r)).collect();
    let doc = json!({
        "rows": arr,
        "truncated": agg.is_truncated(),
        "truncated_count": agg.truncated_count(),
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

fn render_ndjson(agg: &Aggregator, rows: &[Row]) -> String {
    let mut out = String::new();
    for row in rows {
        let v = row_to_json(agg, row);
        if let Ok(s) = serde_json::to_string(&v) {
            out.push_str(&s);
            out.push('\n');
        }
    }
    out
}

/// Renders a single raw (non-aggregated) matched record.
#[must_use]
pub fn render_record(record: &Value, format: OutputFormat) -> String {
    match format {
        OutputFormat::Table | OutputFormat::Ndjson => {
            serde_json::to_string(record).unwrap_or_default()
        }
        OutputFormat::Json => serde_json::to_string_pretty(record).unwrap_or_default(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::agg::AggKind;
    use serde_json::json;

    fn sample_agg() -> Aggregator {
        let mut agg = Aggregator::new(vec!["service".into()], vec![AggKind::Count], 1000, 1000);
        agg.observe(&json!({"service": "api"}));
        agg.observe(&json!({"service": "api"}));
        agg.observe(&json!({"service": "worker"}));
        agg
    }

    #[test]
    fn table_output_contains_headers_and_values() {
        let agg = sample_agg();
        let out = render_table(&agg, &collect_rows(&agg));
        assert!(out.contains("service"));
        assert!(out.contains("count"));
        assert!(out.contains("api"));
        assert!(out.contains('2'));
    }

    #[test]
    fn json_output_is_valid_json_with_rows() {
        let agg = sample_agg();
        let out = render_json(&agg, &collect_rows(&agg));
        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert!(parsed["rows"].is_array());
        assert_eq!(parsed["rows"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn ndjson_output_has_one_line_per_row() {
        let agg = sample_agg();
        let out = render_ndjson(&agg, &collect_rows(&agg));
        assert_eq!(out.lines().count(), 2);
        for line in out.lines() {
            let _: Value = serde_json::from_str(line).unwrap();
        }
    }

    #[test]
    fn table_reports_truncation_warning() {
        let mut agg = Aggregator::new(vec!["id".into()], vec![AggKind::Count], 1, 1000);
        agg.observe(&json!({"id": "a"}));
        agg.observe(&json!({"id": "b"}));
        let out = render_table(&agg, &collect_rows(&agg));
        assert!(out.contains("cardinality limit"));
    }

    #[test]
    fn parses_format_names() {
        assert_eq!(OutputFormat::parse("table"), Some(OutputFormat::Table));
        assert_eq!(OutputFormat::parse("JSON"), Some(OutputFormat::Json));
        assert_eq!(OutputFormat::parse("ndjson"), Some(OutputFormat::Ndjson));
        assert_eq!(OutputFormat::parse("jsonl"), Some(OutputFormat::Ndjson));
        assert_eq!(OutputFormat::parse("xml"), None);
    }
}
