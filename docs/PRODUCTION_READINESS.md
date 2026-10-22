# Production readiness — logtail-rs

Audit date: 2026-10-22. Branch: `audit/production-readiness`.
Tested commit: `2b4bbf8` (code at `70deabe`; the final commit on the
branch adds only this file).

## Scope

`logtail` is a CLI (plus library) that filters and aggregates
newline-delimited JSON logs from a file or stdin, with a small query
language (`query/{lexer,parser,ast,eval}.rs`), streaming aggregations
(`agg.rs`, reservoir-sampled percentiles in `reservoir.rs`, a group-by
cardinality guard in `cardinality.rs`), time filtering/windows
(`window.rs`) and a rotation-aware follow mode (`follow.rs`). Input is
read through a bounded line reader (`lines.rs`). It makes no network
calls and writes only to stdout/stderr.

## Platforms and prerequisites

- Rust 1.85+ (declared MSRV; `rust-version = "1.85"`; CI has an MSRV job).
- Verified locally on macOS 27 (Apple M5). CI covers Linux, macOS and
  Windows; the Windows follow-mode identity path (creation time) was not
  exercised locally.

## Clean-clone verification (literal README steps)

```
git clone -b audit/production-readiness <repo> logtail-rs && cd logtail-rs
cargo build --release                 # exit 0; binary at target/release/logtail
cargo test --all-targets              # exit 0 — 98 passed, 0 failed, 0 ignored
cargo fmt --all -- --check            # exit 0
cargo clippy --all-targets -- -D warnings   # exit 0
```

Every README example except the `/var/log` path and `jq` (a Python JSON
reader was used instead) was run against a small `app.log`; all exited 0
with the expected rows, and the malformed-query example printed
`query error at column 8: expected ==, found a single =` with exit 1.

Follow mode was run live (background process, killed by PID): appended
lines, a non-UTF-8 line, a rename-based rotation, an in-place truncation
and a 3 MiB over-long line were all handled (matching lines printed,
the bad and over-long lines skipped).

## Commands and results (audit host)

| Command | Exit | Result |
| --- | --- | --- |
| `cargo fmt --all -- --check` | 0 | clean |
| `cargo clippy --all-targets --all-features --locked -- -D warnings` | 0 | clean (crate denies `clippy::all`, warns pedantic) |
| `cargo test --all-targets --locked` (baseline) | 0 | 85 passed, 0 failed, 0 ignored |
| `cargo test --all-targets --locked` (after) | 0 | 98 passed, 0 failed, 0 ignored |
| `cargo test --doc` | 0 | 0 doctests |
| `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --locked` (baseline) | 101 | 2 errors: links to private `file_identity` |
| same (after) | 0 | clean |
| `cargo audit --db <local advisory-db> --no-fetch` | 0 | 0 vulnerabilities in 60 crates |
| `cargo deny check` | — | not configured (no `deny.toml`) |

Breakdown after: lib unit tests 60, `tests/cli_tests.rs` 12,
`tests/query_tests.rs` 24, `tests/streaming_tests.rs` 2.

## Performance (re-measured)

Apple M5, 10 cores, 16 GB, macOS 27.0.1, rustc 1.98.0, release profile;
402 MB / 3,000,000-line generated NDJSON; median of 3 runs of
`/usr/bin/time -l`, output to `/dev/null`:

| Command | Time | Max RSS | Same command, baseline `main` build |
| --- | ---: | ---: | ---: |
| `--query 'status >= 500 and not path contains "/health"' --format ndjson` | 2.35 s | 2.9 MB | 2.64 s |
| `--agg 'count,avg(latency_ms),p50(latency_ms),p90(latency_ms),p99(latency_ms)' --group-by service,status` | 3.22 s | 10.1 MB | 4.44 s |

Single 200 MiB line + 2 normal lines, `--agg count`: baseline 426 MB max
RSS; after the fix 5.5 MB (line skipped and reported).

The README previously quoted numbers from a container run that could not
be reproduced here (no Docker on the audit host); they were replaced by
the numbers above.

## CI status

Default branch: `success` (run on `0a5e4c4`, 2026-08-30).
Audit branch: pending until pushed. Actions are pinned by tag, not SHA;
unchanged because not broken.

## Bugs found and fixed (each with a regression test)

| # | Severity | Defect | Fix | Regression test |
| --- | --- | --- | --- | --- |
| 1 | High (availability) | One non-UTF-8 byte anywhere aborted the whole run (`stream did not contain valid UTF-8`, exit 1) — file, stdin and follow mode — contradicting "malformed lines skipped with a counter". | `LineReader` reports `Line::InvalidUtf8`; counted as malformed. | `cli_tests::invalid_utf8_line_is_skipped_not_fatal`, `invalid_utf8_on_stdin_is_skipped_not_fatal`, `lines::tests::invalid_utf8_is_reported_not_an_error`, `follow::tests::poll_lines_reports_invalid_utf8_and_over_long_lines` |
| 2 | High (resource exhaustion) | Lines were buffered whole: memory scaled with the longest line (200 MiB line → 426 MB RSS), contradicting "bounded memory". | `--max-line-bytes` (default 1 MiB); longer lines are skipped without buffering and reported on stderr. | `cli_tests::over_long_line_is_skipped_and_reported`, `lines::tests::{over_long_lines_are_discarded_and_the_next_line_survives, buffer_never_exceeds_the_cap, a_line_exactly_at_the_cap_is_kept}` |
| 3 | Medium | Follow mode collected everything appended between polls into one `Vec`. | `Follower::poll_lines` consumes at most 8 MiB per call. | `follow::tests::poll_lines_hands_over_large_appends_in_bounded_batches` |
| 4 | Medium (wrong result) | Invalid `~` regex compiled lazily: if no record reached it the run exited 0 having silently matched nothing; otherwise it failed after partial output. | `query::validate_regexes` before reading input. | `cli_tests::invalid_regex_is_rejected_before_reading_input`, `query_tests::validate_regexes_finds_invalid_patterns_anywhere_in_the_tree` |
| 5 | Low | `UNIX_EPOCH + Duration` on absurd numeric timestamps can panic where `SystemTime` is narrower (Windows). | `checked_add`; treated as no timestamp. | existing `window` tests (not reproducible on macOS; defensive) |
| 6 | Docs | README follow examples used `-f`, which is `--format` (`logtail app.log -f --query ...` fails: "a value is required for '--format'"). | README uses `-F`. | `cli_tests::follow_flag_is_capital_f_and_lowercase_f_is_format` |
| 7 | Docs | `cargo doc -D warnings` failed (private intra-doc links). | Links made plain code. | `cargo doc` above |

## Security review

- `unsafe_code = "forbid"`; no network; only reads the named file/stdin.
- Untrusted input: invalid UTF-8, over-long lines, non-object JSON and
  deeply nested JSON (serde_json recursion limit) are all skipped, not fatal.
- Regex: `regex` crate (no backtracking); patterns come only from the
  user's own `--query`.
- Memory: per line bounded by `--max-line-bytes`; groups by
  `--cardinality-limit`; percentiles by `--reservoir-capacity` (each
  configurable by the operator, so an operator can still choose large
  values).
- Secret scan of the tree and all history: no hits.
- Workflow permissions: `contents: read`.

## Known limitations and unresolved risks

- Follow mode cannot see a copytruncate-style truncation if the file is
  refilled to at least the previously read length within one poll
  interval (300 ms) — observed live with identical-length lines. Same
  class of limitation as `tail -F`.
- Windows rotation detection relies on creation time plus a size check
  (documented in README); not exercised on this host.
- Percentiles use the index `round(p/100 * (n-1))` over the sorted
  reservoir (the code comment calls it "nearest-rank"); e.g. p50 of two
  values returns the larger one.
- `--follow` runs until killed; the malformed/over-long summary is only
  printed for finite input.
- `cargo deny` is not configured.

## Acceptance checklist

| Item | Expected behaviour | Command / test | Result | Actual | Reference |
| --- | --- | --- | --- | --- | --- |
| Query language | Operators, precedence, nested fields, null/type semantics | `cargo test --test query_tests` | pass | 24/24 | `src/query/` |
| Positioned errors | Malformed query → column + exit 1 | README example | pass | `column 8` | `src/query/parser.rs` |
| Aggregations | count/sum/avg/min/max/pN, group-by | `cargo test` (`agg`, `cli_tests`) | pass | hand-computed values match | `src/agg.rs` |
| Malformed input | Bad JSON, non-UTF-8, over-long lines skipped and counted | `cargo test --test cli_tests` | pass (UTF-8 and long-line cases failed before) | counts on stderr | `src/lines.rs` |
| Bounded memory | RSS independent of file size and line length | `/usr/bin/time -l` runs above | pass | 2.9–10.1 MB on 402 MB; 5.5 MB with a 200 MiB line | `src/lines.rs`, `src/follow.rs` |
| Follow + rotation | Appends, rename rotation, truncation | live run + `follow` unit tests | pass (with the limitation above) | matching lines printed | `src/follow.rs` |
| Time filters/windows | `--since/--until/--window` | README examples + `window` tests | pass | 2 buckets for sample | `src/window.rs` |
| Lint/docs | fmt, clippy, rustdoc `-D warnings` | table above | pass | clean | — |

## Classification proposal

**Partially Verified** — documented behaviour passes on macOS with four
real defects fixed; not yet verified: CI on Linux/Windows/MSRV for the
audit branch (runs after push), and Windows follow-mode rotation detection.
