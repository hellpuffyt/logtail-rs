//! End-to-end CLI smoke tests using the compiled `logtail` binary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;
use std::io::Write;

fn write_log(lines: &[&str]) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    for line in lines {
        writeln!(f, "{line}").unwrap();
    }
    f.flush().unwrap();
    f
}

#[test]
fn filters_records_with_query() {
    let log = write_log(&[
        r#"{"status":200,"path":"/a"}"#,
        r#"{"status":500,"path":"/b"}"#,
        r#"{"status":404,"path":"/c"}"#,
    ]);

    Command::cargo_bin("logtail")
        .unwrap()
        .arg(log.path())
        .arg("--query")
        .arg("status >= 400")
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\":500"))
        .stdout(predicate::str::contains("\"status\":404"))
        .stdout(predicate::str::contains("\"status\":200").not());
}

#[test]
fn aggregates_with_group_by_and_count() {
    let log = write_log(&[
        r#"{"service":"api","status":200}"#,
        r#"{"service":"api","status":500}"#,
        r#"{"service":"worker","status":200}"#,
    ]);

    Command::cargo_bin("logtail")
        .unwrap()
        .arg(log.path())
        .arg("--agg")
        .arg("count")
        .arg("--group-by")
        .arg("service")
        .assert()
        .success()
        .stdout(predicate::str::contains("api"))
        .stdout(predicate::str::contains("worker"));
}

#[test]
fn json_output_is_parseable() {
    let log = write_log(&[r#"{"service":"api"}"#, r#"{"service":"api"}"#]);

    let output = Command::cargo_bin("logtail")
        .unwrap()
        .arg(log.path())
        .arg("--agg")
        .arg("count")
        .arg("--group-by")
        .arg("service")
        .arg("--format")
        .arg("json")
        .output()
        .unwrap();
    assert!(output.status.success());
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(parsed["rows"].is_array());
}

#[test]
fn malformed_query_exits_with_error_and_message() {
    let log = write_log(&[r#"{"a":1}"#]);
    Command::cargo_bin("logtail")
        .unwrap()
        .arg(log.path())
        .arg("--query")
        .arg("status =")
        .assert()
        .failure()
        .stderr(predicate::str::contains("column"));
}

#[test]
fn reports_malformed_line_count_on_stderr() {
    let log = write_log(&[r#"{"a":1}"#, "not json", r#"{"b":2}"#]);
    Command::cargo_bin("logtail")
        .unwrap()
        .arg(log.path())
        .assert()
        .success()
        .stderr(predicate::str::contains("skipped 1 malformed"));
}

#[test]
fn percentile_aggregation_works_end_to_end() {
    let mut lines = Vec::new();
    for i in 1..=100 {
        lines.push(format!(r#"{{"latency_ms":{i}}}"#));
    }
    let refs: Vec<&str> = lines.iter().map(std::string::String::as_str).collect();
    let log = write_log(&refs);

    Command::cargo_bin("logtail")
        .unwrap()
        .arg(log.path())
        .arg("--agg")
        .arg("p50(latency_ms),p99(latency_ms)")
        .assert()
        .success();
}

#[test]
fn reads_from_stdin_when_no_file_given() {
    Command::cargo_bin("logtail")
        .unwrap()
        .arg("--query")
        .arg("status == 500")
        .write_stdin("{\"status\":500}\n{\"status\":200}\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("500"))
        .stdout(predicate::str::contains("200").not());
}
