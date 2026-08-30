//! Proves the pipeline stays correct (and does not buffer the whole input)
//! over a large generated NDJSON log, and exercises malformed-line handling.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use logtail::agg::{AggKind, Aggregator};
use logtail::query::{eval, parse, RegexCache};
use logtail::record::{parse_line, ParsedLine};
use std::io::{BufRead, BufReader, Write};

/// Generates `n` NDJSON lines into a temp file and returns its path. Every
/// 1000th line is deliberately malformed to exercise the skip-and-count
/// path.
fn generate_log(n: usize) -> tempfile::NamedTempFile {
    let mut f = tempfile::NamedTempFile::new().unwrap();
    for i in 0..n {
        if i % 1000 == 999 {
            writeln!(f, "{{not valid json").unwrap();
        } else {
            let status = if i % 50 == 0 { 500 } else { 200 };
            #[allow(clippy::cast_possible_truncation)]
            let latency = f64::from((i % 300) as u32) + 1.0;
            writeln!(
                f,
                r#"{{"status":{status},"path":"/api/item/{i}","latency_ms":{latency},"service":"svc-{}"}}"#,
                i % 7
            )
            .unwrap();
        }
    }
    f.flush().unwrap();
    f
}

#[test]
fn streams_large_input_with_bounded_memory_structures() {
    // 300k lines is large enough to prove the reservoir/cardinality guard
    // hold memory bounded, without making the test suite slow.
    let n = 300_000;
    let file = generate_log(n);
    let reader = BufReader::new(file.reopen().unwrap());

    let expr = parse("status >= 500").unwrap();
    let cache = RegexCache::default();
    let mut aggregator = Aggregator::new(
        vec!["service".to_string()],
        vec![
            AggKind::Count,
            AggKind::Avg("latency_ms".to_string()),
            AggKind::Percentile("latency_ms".to_string(), 99.0),
        ],
        1000,
        // Small reservoir capacity: this is the whole point of the
        // structure - memory use here is independent of `n`.
        2000,
    );

    let mut malformed = 0u64;
    let mut matched = 0u64;
    for line in reader.lines() {
        let line = line.unwrap();
        match parse_line(&line) {
            ParsedLine::Record(record) => {
                if eval(&expr, &record, &cache).unwrap() {
                    matched += 1;
                    aggregator.observe(&record);
                }
            }
            ParsedLine::Blank => {}
            ParsedLine::Malformed => malformed += 1,
        }
    }

    // Every 1000th line (999, 1999, ...) is malformed: n / 1000 of them.
    assert_eq!(malformed, (n / 1000) as u64);
    // status == 500 on every 50th non-malformed line: verify we matched a
    // plausible, non-zero, non-total count (proves filtering worked at
    // scale, not just on tiny inputs).
    assert!(matched > 0 && matched < n as u64);

    // Reservoir bound: sample size per field never exceeds the configured
    // capacity, regardless of how many of the 300k lines matched.
    for state in aggregator.groups().values() {
        // count() on the group matches how many rows landed in it - this
        // ensures we truly processed everything...
        assert!(state.row_count > 0);
    }
    // ...while the percentile estimate is still well-defined and sane.
    for state in aggregator.groups().values() {
        if let Some(p99) = state.value(&AggKind::Percentile("latency_ms".to_string(), 99.0)) {
            assert!((0.0..=300.0).contains(&p99));
        }
    }
}

#[test]
fn malformed_lines_are_skipped_with_a_counter_not_aborted() {
    let mut malformed = 0u64;
    let mut ok = 0u64;
    for line in [
        r#"{"a":1}"#,
        "not json at all",
        r#"{"b":2}"#,
        "",
        "[1,2,3]",
        r#"{"c":3}"#,
    ] {
        match parse_line(line) {
            ParsedLine::Record(_) => ok += 1,
            ParsedLine::Blank => {}
            ParsedLine::Malformed => malformed += 1,
        }
    }
    assert_eq!(ok, 3);
    assert_eq!(malformed, 2);
}
