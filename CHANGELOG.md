# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-08-30

### Added

- Recursive-descent query language: field comparisons (`==`, `!=`, `>`, `>=`,
  `<`, `<=`), string operators (`~` regex, `contains`), boolean composition
  (`and`, `or`, `not`, parentheses), existence checks (`has`), nested field
  access (`a.b.c`), and null handling. Parse errors report a 1-based column
  position.
- Streaming aggregation: `count`, `sum`, `avg`, `min`, `max`, and percentiles
  (`p50`/`p90`/`p95`/`p99`, or any `pN`) computed over a bounded reservoir
  sample rather than buffering the whole stream.
- `group by` on one or more fields, with a bounded-cardinality guard that
  folds groups past the limit into a reported overflow bucket instead of
  growing memory unbounded.
- Time filtering with `--since`/`--until` (RFC3339 or a relative duration
  like `5m`) and tumbling time windows via `--window`.
- Follow mode (`-f`/`--follow`) that detects log rotation (file replacement)
  and in-place truncation, reopening the file rather than silently stalling.
- Table, JSON, and NDJSON output formats.
- Malformed JSON lines are skipped with a counter reported on exit, instead
  of aborting the run.

[0.1.0]: https://github.com/hellpuffyt/logtail-rs/releases/tag/v0.1.0
