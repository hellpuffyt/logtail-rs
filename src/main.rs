//! `logtail`: filter and aggregate high-volume structured JSON logs with a
//! query language, fast enough to run on a live tail.
#![deny(unsafe_code)]
#![warn(clippy::all, clippy::pedantic)]
#![deny(clippy::unwrap_used, clippy::expect_used)]

use clap::Parser as ClapParser;
use logtail::agg::{AggKind, Aggregator};
use logtail::follow::Follower;
use logtail::output::{render_aggregation, render_record, OutputFormat};
use logtail::query::{self, eval, Expr, RegexCache};
use logtail::record::{parse_line, ParsedLine};
use logtail::window::{extract_timestamp, parse_time_arg, TimeRange, TumblingWindow};
use serde_json::Value;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

/// Default cap on the number of distinct group-by keys tracked at once.
const DEFAULT_CARDINALITY_LIMIT: usize = 10_000;
/// Default reservoir sample size used to estimate percentiles.
const DEFAULT_RESERVOIR_CAPACITY: usize = 10_000;
/// Poll interval for `--follow` mode.
const FOLLOW_POLL_INTERVAL: Duration = Duration::from_millis(300);

#[derive(ClapParser, Debug)]
#[command(
    name = "logtail",
    version,
    about = "Filter and aggregate JSON-lines logs with a query language."
)]
struct Cli {
    /// Path to the log file. Reads from stdin if omitted.
    file: Option<PathBuf>,

    /// Filter query, e.g. `status >= 500 and not path contains "/health"`.
    #[arg(short, long)]
    query: Option<String>,

    /// Aggregations to compute, comma-separated: count, sum(field), avg(field),
    /// min(field), max(field), p50(field), p90(field), p95(field), p99(field).
    #[arg(short, long, value_delimiter = ',')]
    agg: Vec<String>,

    /// Group aggregations by one or more (dotted) fields, comma-separated.
    #[arg(short, long, value_delimiter = ',')]
    group_by: Vec<String>,

    /// Only include records at or after this time: RFC3339 or a relative
    /// duration like `5m`, `2h`, `1d` (that long before now).
    #[arg(long)]
    since: Option<String>,

    /// Only include records at or before this time: RFC3339 or a relative
    /// duration like `5m`, `2h`, `1d` (that long before now).
    #[arg(long)]
    until: Option<String>,

    /// Field holding the record's timestamp, used by `--since`/`--until`/`--window`.
    #[arg(long, default_value = "timestamp")]
    time_field: String,

    /// Tumbling window size for time-series aggregation output, e.g. `1m`, `30s`.
    #[arg(short, long)]
    window: Option<String>,

    /// Output format: table, json, or ndjson.
    #[arg(short, long, default_value = "table")]
    format: String,

    /// Follow the file for new lines, handling rotation (like `tail -f`, but
    /// robust to the file being replaced).
    #[arg(short = 'F', long)]
    follow: bool,

    /// Maximum number of distinct group-by keys tracked before extra groups
    /// are folded into an overflow bucket.
    #[arg(long, default_value_t = DEFAULT_CARDINALITY_LIMIT)]
    cardinality_limit: usize,

    /// Reservoir sample size used to estimate percentiles (larger = more
    /// accurate, more memory).
    #[arg(long, default_value_t = DEFAULT_RESERVOIR_CAPACITY)]
    reservoir_capacity: usize,
}

struct RunConfig {
    expr: Expr,
    agg_kinds: Vec<AggKind>,
    group_by: Vec<String>,
    format: OutputFormat,
    time_range: TimeRange,
    time_field: String,
    window: Option<TumblingWindow>,
    cardinality_limit: usize,
    reservoir_capacity: usize,
    follow: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("logtail: {e}");
            ExitCode::FAILURE
        }
    }
}

fn build_config(cli: &Cli) -> Result<RunConfig, String> {
    let expr = query::parse(cli.query.as_deref().unwrap_or(""))
        .map_err(|e| format!("query error at {e}"))?;

    let mut agg_kinds = Vec::new();
    for spec in &cli.agg {
        let kind =
            AggKind::parse(spec).ok_or_else(|| format!("invalid aggregation spec `{spec}`"))?;
        agg_kinds.push(kind);
    }

    let format = OutputFormat::parse(&cli.format).ok_or_else(|| {
        format!(
            "unknown --format `{}` (expected table, json, or ndjson)",
            cli.format
        )
    })?;

    let now = SystemTime::now();
    let since = cli
        .since
        .as_deref()
        .map(|s| parse_time_arg(s, now))
        .transpose()
        .map_err(|e| format!("invalid --since: {e}"))?;
    let until = cli
        .until
        .as_deref()
        .map(|s| parse_time_arg(s, now))
        .transpose()
        .map_err(|e| format!("invalid --until: {e}"))?;

    let window = cli
        .window
        .as_deref()
        .map(humantime::parse_duration)
        .transpose()
        .map_err(|e| format!("invalid --window: {e}"))?
        .map(TumblingWindow::new);

    Ok(RunConfig {
        expr,
        agg_kinds,
        group_by: cli.group_by.clone(),
        format,
        time_range: TimeRange { since, until },
        time_field: cli.time_field.clone(),
        window,
        cardinality_limit: cli.cardinality_limit,
        reservoir_capacity: cli.reservoir_capacity,
        follow: cli.follow,
    })
}

#[allow(clippy::too_many_lines)]
fn run(cli: &Cli) -> Result<(), String> {
    let config = build_config(cli)?;
    let stdout = io::stdout();
    let mut out = stdout.lock();

    let mut malformed = 0u64;
    let mut matched = 0u64;
    let cache = RegexCache::default();

    let group_by = if config.window.is_some() {
        let mut gb = vec!["_window_start".to_string()];
        gb.extend(config.group_by.clone());
        gb
    } else {
        config.group_by.clone()
    };
    let aggregating = !config.agg_kinds.is_empty();
    let mut aggregator = Aggregator::new(
        group_by,
        config.agg_kinds.clone(),
        config.cardinality_limit,
        config.reservoir_capacity,
    );

    let process_line = |line: &str,
                        aggregator: &mut Aggregator,
                        out: &mut dyn Write,
                        malformed: &mut u64,
                        matched: &mut u64|
     -> Result<(), String> {
        let mut record = match parse_line(line) {
            ParsedLine::Record(v) => v,
            ParsedLine::Blank => return Ok(()),
            ParsedLine::Malformed => {
                *malformed += 1;
                return Ok(());
            }
        };

        if !config.time_range.is_unbounded() {
            match extract_timestamp(&record, &config.time_field) {
                Some(t) if config.time_range.contains(t) => {}
                _ => return Ok(()),
            }
        }

        let is_match =
            eval(&config.expr, &record, &cache).map_err(|e| format!("regex error: {e}"))?;
        if !is_match {
            return Ok(());
        }
        *matched += 1;

        if let Some(win) = &config.window {
            let Some(t) = extract_timestamp(&record, &config.time_field) else {
                return Ok(());
            };
            let bucket = win.bucket_start_secs(t);
            if let Value::Object(map) = &mut record {
                #[allow(clippy::cast_precision_loss)]
                map.insert("_window_start".to_string(), Value::from(bucket));
            }
        }

        if aggregating {
            aggregator.observe(&record);
        } else {
            let rendered = render_record(&record, config.format);
            writeln!(out, "{rendered}").map_err(|e| e.to_string())?;
        }
        Ok(())
    };

    if let Some(path) = &cli.file {
        let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let reader = BufReader::new(file);
        for line in reader.lines() {
            let line = line.map_err(|e| e.to_string())?;
            process_line(
                &line,
                &mut aggregator,
                &mut out,
                &mut malformed,
                &mut matched,
            )?;
        }

        if config.follow {
            let mut follower = Follower::open_at_end(path)
                .map_err(|e| format!("cannot follow {}: {e}", path.display()))?;
            loop {
                let lines = follower.poll().map_err(|e| e.to_string())?;
                if lines.is_empty() {
                    std::thread::sleep(FOLLOW_POLL_INTERVAL);
                    continue;
                }
                for line in lines {
                    process_line(
                        &line,
                        &mut aggregator,
                        &mut out,
                        &mut malformed,
                        &mut matched,
                    )?;
                }
                if aggregating {
                    let rendered = render_aggregation(&aggregator, config.format);
                    writeln!(out, "{rendered}").map_err(|e| e.to_string())?;
                }
                out.flush().map_err(|e| e.to_string())?;
            }
        }
    } else {
        let stdin = io::stdin();
        let mut reader = BufReader::new(stdin.lock());
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            let line = buf.trim_end_matches(['\r', '\n']);
            process_line(
                line,
                &mut aggregator,
                &mut out,
                &mut malformed,
                &mut matched,
            )?;
        }
    }

    if aggregating {
        let rendered = render_aggregation(&aggregator, config.format);
        writeln!(out, "{rendered}").map_err(|e| e.to_string())?;
    }

    if malformed > 0 {
        eprintln!("logtail: skipped {malformed} malformed line(s)");
    }
    let _ = matched;
    Ok(())
}
