# logtail

Filter and aggregate high-volume structured JSON logs with a real query
language, streaming throughout so a multi-gigabyte file never has to fit in
memory.

## What

`logtail` reads newline-delimited JSON (NDJSON) logs - one JSON object per
line - and lets you:

- Filter records with a small expression language (`status >= 500 and not
  path contains "/health"`).
- Aggregate matched records: `count`, `sum`, `avg`, `min`, `max`, and
  percentiles (`p50`, `p90`, `p95`, `p99`, or any `pN`), optionally grouped
  by one or more fields.
- Restrict to a time range (`--since`/`--until`) or bucket into tumbling
  time windows (`--window`) for a time series.
- Follow a live file (`-f`/`--follow`) the way `tail -f` does, but survive
  the file being rotated out from under it.
- Emit a table for a terminal, or JSON/NDJSON for piping into another tool.

Everything is streaming: `logtail` reads one line at a time and never loads
the whole file into memory, regardless of file size (see
[Performance](#performance) for a measured 370 MB / 3,000,000-line run).

## Why

`grep` and `awk` can filter lines, but they cannot answer "what's the p99
latency by endpoint for the last 5 minutes, excluding health checks" without
turning into a small essay of pipeline stages, and they have no concept of
nested JSON fields or numeric comparison against a JSON number. Shipping
every log line to a hosted observability platform to ask that question is
slow to set up and not free, and often the log file you need is already
sitting right there on disk or scrolling past in a live tail. `logtail` is
the tool for that: a real query language and real aggregations, running
locally, streaming, with bounded memory.

## Query language reference

### Grammar

```text
expr       := or_expr
or_expr    := and_expr ( "or" and_expr )*
and_expr   := not_expr ( "and" not_expr )*
not_expr   := "not" not_expr | primary
primary    := "(" expr ")" | has_expr | comparison
has_expr   := "has" field
comparison := field ( cmp_op literal | "~" string | "contains" string )
field      := ident ( "." ident )*
cmp_op     := "==" | "!=" | ">" | ">=" | "<" | "<="
literal    := number | string | "true" | "false" | "null"
```

Precedence, loosest to tightest: `or` < `and` < `not` < comparison/parens.
So `a or b and c` parses as `a or (b and c)`, and `not a and b` parses as
`(not a) and b`. Use parentheses to override.

An empty query (`--query ""` or no `--query` at all) matches every record.

### Operators

| Syntax | Meaning |
| --- | --- |
| `field == value` | Equality (numbers, strings, booleans, or `null`) |
| `field != value` | Inequality |
| `field > / >= / < / <=` | Numeric or lexicographic string ordering |
| `field ~ "regex"` | Regex match (field must be a string) |
| `field contains "substr"` | Substring match (field must be a string) |
| `has field` | Field is present and not JSON `null` |
| `not expr` | Boolean negation |
| `a and b`, `a or b` | Boolean composition |
| `( expr )` | Grouping |

### Nested fields

`http.request.method == "GET"` walks into nested JSON objects. If any
segment of the path is missing, or the value at that point isn't an object,
the field is treated as absent.

### Null and type-mismatch handling

A missing field never matches a comparison (except `field != null`, which is
true for both "present and non-null" and treated as false for "missing" -
matching the intuition that a query about a field's *value* shouldn't fire
on a record that never had the field). Comparing values of mismatched JSON
types (e.g. `status == "200"` against a numeric `status` field) evaluates to
`false` rather than raising an error - this is a query language for
heterogeneous real-world logs, not a type-checked language.

### Errors

Malformed queries report a 1-based column position rather than a stack
trace:

```text
$ logtail app.log --query 'status = 500'
logtail: query error at column 8: expected `==`, found a single `=`
```

## Aggregations

Specify one or more with `--agg`, comma-separated:

- `count`
- `sum(field)`, `avg(field)` (alias `mean`), `min(field)`, `max(field)`
- `p50(field)` / `median(field)`, `p90(field)`, `p95(field)`, `p99(field)`,
  or any `pN(field)` for a custom percentile

Combine with `--group-by field1,field2` to compute per-group results. With
no `--group-by`, aggregates run over the whole stream.

### Percentile accuracy trade-off

Percentiles are computed over a **fixed-capacity reservoir sample**
(10,000 values by default, `--reservoir-capacity` to change it), not the
full data set. This is what makes `p99(latency_ms)` over a 10 GB file run
in bounded memory instead of buffering every value: memory use for
percentiles is `O(reservoir_capacity)`, independent of how many records
were observed.

The trade-off is approximation: for a stream of `n` values with capacity
`c`, the standard error of a reported percentile is approximately
`sqrt(p(1-p)/c)`, independent of `n` once `n > c`. In practice this means
p50/p90 are accurate to a fraction of a percentage point at the default
capacity, while extreme tails (p99.9 and beyond) are noisier because fewer
sampled points land there. If you need exact percentiles and the data set
comfortably fits in memory, raise `--reservoir-capacity` above the total
row count for that group.

### Cardinality guard

`--group-by` on an unexpectedly high-cardinality field (a raw request ID, a
URL with a query string) would otherwise grow the group table without
bound. `logtail` caps the number of distinct groups at
`--cardinality-limit` (default 10,000); once the cap is hit, further new
keys are folded into a single overflow bucket and a warning is printed
reporting how many additional values were folded in - rather than the
process silently consuming unbounded memory.

## Architecture

```text
  file/stdin              lexer/parser        evaluator
 ───────────►  raw line ──────────────► Expr ──────────► bool ─┐
  (BufReader,               (query.rs)        (query::eval)    │ matched
   line at a time)                                              │
                                                                  ▼
                                              ┌── raw record ──► output (table/json/ndjson)
                                              │
                        Aggregator.observe() ─┘── group-by key + cardinality guard
                              │                     │
                              ▼                     ▼
                      per-group FieldStats:   overflow GroupState
                      count/sum/min/max +
                      ReservoirSampler (percentiles)
```

Key modules (`src/`):

- `query/{lexer,parser,ast,eval}.rs` - the query language: hand-written
  recursive-descent parser producing an `Expr` tree, evaluated directly
  against `serde_json::Value` records (no compilation to bytecode; the tree
  is small and evaluation is already the fast path).
- `reservoir.rs` - `ReservoirSampler`, a bounded-memory percentile
  estimator (Algorithm R reservoir sampling).
- `cardinality.rs` - `CardinalityGuard`, the bounded-group-by-key tracker.
- `agg.rs` - `Aggregator` and `GroupState`, tying group-by, the
  cardinality guard, and per-field reservoirs together.
- `follow.rs` - `Follower`, tailing a file with rotation/truncation
  detection (inode/file-index comparison plus a length check).
- `window.rs` - `--since`/`--until` parsing and tumbling-window bucketing.
- `output.rs` - table/JSON/NDJSON rendering.
- `record.rs` - dotted-field lookup and tolerant NDJSON line parsing.

The whole pipeline is single-pass and streaming: `main.rs` reads one line,
parses it, evaluates the query, and either emits it immediately (raw
filter) or folds it into the aggregator - it is never buffered as a
collection of records.

## Installation

Build from source (requires Rust 1.85 or newer - see [MSRV](#msrv) below):

```sh
git clone https://github.com/hellpuffyt/logtail-rs.git
cd logtail-rs
cargo build --release
# binary at target/release/logtail
```

## Usage

```text
logtail [FILE] [OPTIONS]

  FILE                     Path to the log file. Reads from stdin if omitted.

  -q, --query <QUERY>      Filter query.
  -a, --agg <AGG>...       Aggregations, comma-separated: count, sum(field),
                            avg(field), min(field), max(field), p50(field), ...
  -g, --group-by <FIELD>...  Group aggregations by field(s), comma-separated.
      --since <WHEN>       RFC3339 timestamp or relative duration (e.g. `5m`).
      --until <WHEN>       RFC3339 timestamp or relative duration.
      --time-field <FIELD> Field holding the record's timestamp (default: timestamp).
  -w, --window <DURATION>  Tumbling window size for time-series aggregation.
  -f, --format <FORMAT>    table | json | ndjson (default: table).
  -F, --follow             Follow the file for new lines, handling rotation.
      --cardinality-limit <N>    Max distinct group-by keys (default: 10000).
      --reservoir-capacity <N>   Percentile sample size (default: 10000).
```

## Examples

Filter server errors, excluding health checks:

```sh
logtail app.log --query 'status >= 500 and not path contains "/health"'
```

p99 latency by endpoint, last 5 minutes:

```sh
logtail app.log --since 5m --agg 'count,p50(latency_ms),p99(latency_ms)' \
  --group-by path
```

Regex match and nested field access:

```sh
logtail app.log --query 'http.request.path ~ "^/api/v[0-9]+/" and has error'
```

Live-tail error rate by service, surviving log rotation:

```sh
logtail /var/log/app/current.log -f --query 'status >= 500' \
  --agg count --group-by service
```

Pipe NDJSON output into `jq`:

```sh
logtail app.log --query 'status == 500' --format ndjson | jq '.path'
```

Time-series: request count per minute:

```sh
logtail app.log --window 1m --agg count --format json
```

## Performance

Measured with the release binary (`cargo build --release`) in the same
`rust:1` Debian container used for CI, on a synthetic **370 MB, 3,000,000
line** NDJSON log (six fields per record, realistic field-value
cardinality):

| Query | Time | Throughput |
| --- | ---: | ---: |
| `--query 'status >= 500 and not path contains "/health"'` (raw filter → NDJSON) | 1.34 s | ~276 MB/s, ~2.24M lines/s |
| `--agg 'count,avg(latency_ms),p50/p90/p99(latency_ms)' --group-by service,status` (36 groups) | 1.78 s | ~208 MB/s, ~1.68M lines/s |

These are single-run wall-clock numbers on shared CI-class hardware, not a
tuned benchmark harness - treat them as an order-of-magnitude indication,
not a guarantee. Memory use is independent of file size: the streaming
tests (`tests/streaming_tests.rs`) exercise a 300,000-line generated log to
confirm the reservoir and cardinality-guard structures stay within their
configured bounds regardless of how much data flows through them.

## Testing

```sh
cargo test --all-targets
```

85 tests across unit tests (co-located with each module) and integration
tests (`tests/query_tests.rs`, `tests/streaming_tests.rs`,
`tests/cli_tests.rs`), covering:

- Every comparison/match operator, operator precedence (`a or b and c`
  binds as `a or (b and c)`), parenthesization, and malformed queries
  producing a positioned error rather than a panic.
- Aggregations against hand-computed expected values, including percentile
  edge cases (empty, single element, all-identical, even/odd counts,
  values beyond reservoir capacity).
- Malformed JSON lines skipped with a counter, not aborting the stream.
- The cardinality guard truncating and reporting an overflow count.
- Follow-mode rotation (file replaced) and in-place truncation
  (copytruncate-style).
- A 300,000-line generated log proving the pipeline stays correct (and its
  memory-bounded structures stay within their configured capacity)
  end-to-end.
- CLI end-to-end behavior via the compiled binary (`assert_cmd`): filtering,
  aggregation, output formats, error reporting, and stdin input.

## Security

- `unsafe_code = "forbid"` at the crate level - there is no `unsafe` in this
  codebase.
- Input is treated as untrusted: malformed JSON lines are skipped with a
  counter rather than causing a panic or aborting the run; a JSON line that
  parses but isn't a top-level object is likewise treated as malformed.
  Regex patterns from `--query` are compiled with the `regex` crate, which
  guards against catastrophic backtracking by construction (no backtracking
  engine).
- `logtail` only reads the file(s) you point it at and writes to
  stdout/stderr; it makes no network calls.
- Dependencies are kept intentionally minimal (`clap`, `serde`/`serde_json`,
  `regex`, `thiserror`, `humantime`) to keep the audit surface and the MSRV
  small.

Found a security issue? Please open an issue describing it; there is no
dedicated security contact for this project yet.

## MSRV

Minimum supported Rust version: **1.85**, verified by building
(`cargo build --all-targets`) and running the full test suite
(`cargo test --all-targets`) against the `rust:1.85` Docker image - both
pass. Building against `rust:1.84` fails (a dependency requires the
`edition2024` Cargo feature, stabilized in 1.85), which is why the MSRV is
1.85 rather than lower; this is checked in CI's dedicated MSRV job.

## License

MIT - see [LICENSE](LICENSE).
