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

#[test]
fn invalid_utf8_line_is_skipped_not_fatal() {
    // Regression: a single non-UTF-8 byte used to abort the whole run with
    // "stream did not contain valid UTF-8".
    let mut f = tempfile::NamedTempFile::new().unwrap();
    f.write_all(b"{\"status\":500}\n\xff\xfe junk\n{\"status\":503}\n")
        .unwrap();
    f.flush().unwrap();
    Command::cargo_bin("logtail")
        .unwrap()
        .arg(f.path())
        .args(["--agg", "count", "--format", "json"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("\"count\": 2").or(predicate::str::contains("\"count\":2")),
        )
        .stderr(predicate::str::contains("skipped 1 malformed line"));
}

#[test]
fn invalid_utf8_on_stdin_is_skipped_not_fatal() {
    Command::cargo_bin("logtail")
        .unwrap()
        .args(["--agg", "count", "--format", "json"])
        .write_stdin(&b"{\"a\":1}\n\xff\n{\"a\":2}\n"[..])
        .assert()
        .success()
        .stderr(predicate::str::contains("skipped 1 malformed line"));
}

#[test]
fn over_long_line_is_skipped_and_reported() {
    // Regression: lines were buffered whole, so memory grew with the
    // longest line. Now anything over --max-line-bytes is skipped.
    let mut f = tempfile::NamedTempFile::new().unwrap();
    writeln!(f, r#"{{"status":500}}"#).unwrap();
    writeln!(f, r#"{{"pad":"{}"}}"#, "a".repeat(5_000)).unwrap();
    writeln!(f, r#"{{"status":503}}"#).unwrap();
    f.flush().unwrap();
    Command::cargo_bin("logtail")
        .unwrap()
        .arg(f.path())
        .args(["--max-line-bytes", "1024", "--format", "ndjson"])
        .assert()
        .success()
        .stdout(predicate::str::contains("500"))
        .stdout(predicate::str::contains("503"))
        .stdout(predicate::str::contains("pad").not())
        .stderr(predicate::str::contains(
            "skipped 1 line(s) longer than --max-line-bytes (1024)",
        ));
}

#[test]
fn follow_flag_is_capital_f_and_lowercase_f_is_format() {
    // The README used `-f` for follow; `-f` is --format. Pin the real flags.
    let log = write_log(&[r#"{"status":500}"#]);
    Command::cargo_bin("logtail")
        .unwrap()
        .arg(log.path())
        .args(["-f", "ndjson"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"status\":500"));
    Command::cargo_bin("logtail")
        .unwrap()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("-F, --follow"));
}

#[test]
fn invalid_regex_is_rejected_before_reading_input() {
    // Regression: an invalid `~` pattern was only compiled when a record
    // reached it, so a query whose regex branch never ran exited 0 having
    // silently matched nothing, and others failed after partial output.
    let log = write_log(&[r#"{"a":1}"#, r#"{"msg":"x"}"#]);
    for query in [r#"nope ~ "(""#, r#"a == 1 or msg ~ "(""#] {
        Command::cargo_bin("logtail")
            .unwrap()
            .arg(log.path())
            .args(["--query", query])
            .assert()
            .failure()
            .stdout(predicate::str::is_empty())
            .stderr(predicate::str::contains("invalid regex"));
    }
}
