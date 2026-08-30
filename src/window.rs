//! Time-range filtering (`--since` / `--until`) and tumbling windows for
//! time-series output.

use serde_json::Value;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq)]
pub struct TimeError(pub String);

impl fmt::Display for TimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Parses a `--since`/`--until` argument. Accepts either an RFC3339
/// timestamp (`2026-08-30T00:00:00Z`) or a relative duration such as `5m`,
/// `2h`, `1d`, interpreted as "that long before `now`".
///
/// # Errors
/// Returns a [`TimeError`] if the input is neither a valid RFC3339 timestamp
/// nor a valid humantime duration.
pub fn parse_time_arg(input: &str, now: SystemTime) -> Result<SystemTime, TimeError> {
    if let Ok(t) = humantime::parse_rfc3339_weak(input) {
        return Ok(t);
    }
    if let Ok(dur) = humantime::parse_duration(input) {
        return now
            .checked_sub(dur)
            .ok_or_else(|| TimeError(format!("duration `{input}` underflows the epoch")));
    }
    Err(TimeError(format!(
        "`{input}` is not a valid RFC3339 timestamp or a duration like `5m`, `2h`, `1d`"
    )))
}

/// Extracts a timestamp from a record's `field`, accepting either an
/// RFC3339 string or a Unix epoch number (seconds, or milliseconds if the
/// value looks too large to be seconds).
#[must_use]
pub fn extract_timestamp(record: &Value, field: &str) -> Option<SystemTime> {
    let v = crate::record::lookup(record, field)?;
    match v {
        Value::String(s) => humantime::parse_rfc3339_weak(s).ok(),
        Value::Number(n) => {
            let f = n.as_f64()?;
            if f > 1e12 {
                // looks like milliseconds
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let millis = f.max(0.0) as u64;
                Some(UNIX_EPOCH + Duration::from_millis(millis))
            } else {
                Some(UNIX_EPOCH + Duration::from_secs_f64(f.max(0.0)))
            }
        }
        _ => None,
    }
}

/// A `--since`/`--until` range filter.
#[derive(Debug, Clone, Copy)]
pub struct TimeRange {
    pub since: Option<SystemTime>,
    pub until: Option<SystemTime>,
}

impl TimeRange {
    #[must_use]
    pub fn contains(&self, t: SystemTime) -> bool {
        if let Some(since) = self.since {
            if t < since {
                return false;
            }
        }
        if let Some(until) = self.until {
            if t > until {
                return false;
            }
        }
        true
    }

    #[must_use]
    pub fn is_unbounded(&self) -> bool {
        self.since.is_none() && self.until.is_none()
    }
}

/// Assigns timestamps to tumbling (non-overlapping, fixed-size) windows for
/// time-series aggregation output.
#[derive(Debug, Clone, Copy)]
pub struct TumblingWindow {
    size: Duration,
}

impl TumblingWindow {
    #[must_use]
    pub fn new(size: Duration) -> Self {
        TumblingWindow {
            size: if size.is_zero() {
                Duration::from_secs(1)
            } else {
                size
            },
        }
    }

    /// Returns the start instant (as seconds since epoch) of the window
    /// containing `t`.
    #[must_use]
    pub fn bucket_start_secs(&self, t: SystemTime) -> u64 {
        let secs = t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let size = self.size.as_secs().max(1);
        (secs / size) * size
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_relative_duration() {
        let now = UNIX_EPOCH + Duration::from_secs(1000);
        let t = parse_time_arg("5m", now).unwrap();
        assert_eq!(t, now - Duration::from_secs(300));
    }

    #[test]
    fn parses_rfc3339_absolute() {
        let now = UNIX_EPOCH;
        let t = parse_time_arg("2026-01-01T00:00:00Z", now).unwrap();
        assert!(t > UNIX_EPOCH);
    }

    #[test]
    fn rejects_garbage_time_arg() {
        assert!(parse_time_arg("not-a-time", UNIX_EPOCH).is_err());
    }

    #[test]
    fn extracts_timestamp_from_rfc3339_string() {
        let v = json!({"ts": "2026-01-01T00:00:00Z"});
        assert!(extract_timestamp(&v, "ts").is_some());
    }

    #[test]
    fn extracts_timestamp_from_unix_seconds() {
        let v = json!({"ts": 1_700_000_000});
        let t = extract_timestamp(&v, "ts").unwrap();
        assert_eq!(t, UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    }

    #[test]
    fn extracts_timestamp_from_unix_millis() {
        let v = json!({"ts": 1_700_000_000_000_u64});
        let t = extract_timestamp(&v, "ts").unwrap();
        assert_eq!(t, UNIX_EPOCH + Duration::from_secs(1_700_000_000));
    }

    #[test]
    fn missing_timestamp_field_is_none() {
        let v = json!({"other": 1});
        assert!(extract_timestamp(&v, "ts").is_none());
    }

    #[test]
    fn time_range_contains_respects_both_bounds() {
        let range = TimeRange {
            since: Some(UNIX_EPOCH + Duration::from_secs(100)),
            until: Some(UNIX_EPOCH + Duration::from_secs(200)),
        };
        assert!(!range.contains(UNIX_EPOCH + Duration::from_secs(50)));
        assert!(range.contains(UNIX_EPOCH + Duration::from_secs(150)));
        assert!(!range.contains(UNIX_EPOCH + Duration::from_secs(250)));
    }

    #[test]
    fn tumbling_window_buckets_correctly() {
        let w = TumblingWindow::new(Duration::from_secs(60));
        let t1 = UNIX_EPOCH + Duration::from_secs(65);
        let t2 = UNIX_EPOCH + Duration::from_secs(119);
        let t3 = UNIX_EPOCH + Duration::from_secs(120);
        assert_eq!(w.bucket_start_secs(t1), 60);
        assert_eq!(w.bucket_start_secs(t2), 60);
        assert_eq!(w.bucket_start_secs(t3), 120);
    }
}
