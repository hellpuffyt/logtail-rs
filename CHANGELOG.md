# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- A single line that was not valid UTF-8 aborted the whole run ("stream
  did not contain valid UTF-8"), for files, stdin and follow mode. Such
  lines are now skipped and counted as malformed.
- Lines were buffered whole, so memory grew with the longest line (a
  200 MiB line used ~426 MB RSS). Lines longer than the new
  `--max-line-bytes` (default 1 MiB) are now skipped and counted without
  being held in memory (~5.5 MB RSS for the same input).
- Follow mode collected everything appended between polls into memory at
  once; it now hands data over in batches of at most 8 MiB.
- An invalid `~` regex was only compiled when a record reached it: queries
  whose regex branch never ran exited 0 having matched nothing, others
  failed after partial output. Regexes are now validated before reading.
- Numeric timestamps far outside `SystemTime`'s range could panic on
  platforms with a narrower range; they are now treated as missing.
- `cargo doc` failed under `-D warnings` (links to a private item).

### Changed

- README: follow mode is `-F` (`-f` is `--format`); the follow example
  used `-f` and did not run. Performance numbers re-measured on the audit
  host with the commands shown. Added `--max-line-bytes` to the usage.

### Added

- `--max-line-bytes`, `logtail::lines::LineReader`,
  `Follower::poll_lines`, `query::validate_regexes`.
- `SECURITY.md`, `.editorconfig`, `docs/PRODUCTION_READINESS.md`.

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
