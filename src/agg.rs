//! Aggregation pipeline: group-by with a cardinality guard, feeding one
//! [`GroupState`] per group which tracks count/sum/min/max and a percentile
//! reservoir.

use crate::cardinality::CardinalityGuard;
use crate::reservoir::ReservoirSampler;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt;

/// A single requested aggregation, e.g. `avg(latency_ms)` or `p99(latency_ms)`.
#[derive(Debug, Clone, PartialEq)]
pub enum AggKind {
    Count,
    Sum(String),
    Avg(String),
    Min(String),
    Max(String),
    Percentile(String, f64),
}

impl fmt::Display for AggKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AggKind::Count => write!(f, "count"),
            AggKind::Sum(field) => write!(f, "sum({field})"),
            AggKind::Avg(field) => write!(f, "avg({field})"),
            AggKind::Min(field) => write!(f, "min({field})"),
            AggKind::Max(field) => write!(f, "max({field})"),
            AggKind::Percentile(field, p) => write!(f, "p{p}({field})"),
        }
    }
}

impl AggKind {
    /// Parses a spec like `count`, `sum(bytes)`, `p99(latency_ms)`.
    #[must_use]
    pub fn parse(spec: &str) -> Option<AggKind> {
        let spec = spec.trim();
        if spec.eq_ignore_ascii_case("count") {
            return Some(AggKind::Count);
        }
        let open = spec.find('(')?;
        if !spec.ends_with(')') {
            return None;
        }
        let name = &spec[..open];
        let field = spec[open + 1..spec.len() - 1].trim().to_string();
        if field.is_empty() {
            return None;
        }
        match name.to_ascii_lowercase().as_str() {
            "sum" => Some(AggKind::Sum(field)),
            "avg" | "mean" => Some(AggKind::Avg(field)),
            "min" => Some(AggKind::Min(field)),
            "max" => Some(AggKind::Max(field)),
            "p50" | "median" => Some(AggKind::Percentile(field, 50.0)),
            "p90" => Some(AggKind::Percentile(field, 90.0)),
            "p95" => Some(AggKind::Percentile(field, 95.0)),
            "p99" => Some(AggKind::Percentile(field, 99.0)),
            other if other.starts_with('p') => {
                let p: f64 = other[1..].parse().ok()?;
                Some(AggKind::Percentile(field, p))
            }
            _ => None,
        }
    }

    fn field(&self) -> Option<&str> {
        match self {
            AggKind::Count => None,
            AggKind::Sum(f)
            | AggKind::Avg(f)
            | AggKind::Min(f)
            | AggKind::Max(f)
            | AggKind::Percentile(f, _) => Some(f),
        }
    }
}

/// Numeric accumulator for one field within a group.
#[derive(Debug, Clone)]
struct FieldStats {
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
    reservoir: ReservoirSampler,
}

impl FieldStats {
    fn new(capacity: usize) -> Self {
        FieldStats {
            count: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            reservoir: ReservoirSampler::new(capacity),
        }
    }

    fn observe(&mut self, v: f64) {
        self.count += 1;
        self.sum += v;
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.reservoir.observe(v);
    }
}

/// Accumulated state for one group (or the whole stream, if no `group by`).
#[derive(Debug, Clone)]
pub struct GroupState {
    pub row_count: u64,
    fields: HashMap<String, FieldStats>,
    reservoir_capacity: usize,
}

impl GroupState {
    fn new(reservoir_capacity: usize) -> Self {
        GroupState {
            row_count: 0,
            fields: HashMap::new(),
            reservoir_capacity,
        }
    }

    fn observe_record(&mut self, record: &Value, kinds: &[AggKind]) {
        self.row_count += 1;
        for kind in kinds {
            let Some(field) = kind.field() else { continue };
            let Some(v) = crate::record::lookup_number(record, field) else {
                continue;
            };
            self.fields
                .entry(field.to_string())
                .or_insert_with(|| FieldStats::new(self.reservoir_capacity))
                .observe(v);
        }
    }

    /// Computes the value for one requested aggregation over this group's
    /// state. Returns `None` if the field was never observed in this group.
    #[must_use]
    pub fn value(&self, kind: &AggKind) -> Option<f64> {
        match kind {
            #[allow(clippy::cast_precision_loss)]
            AggKind::Count => Some(self.row_count as f64),
            AggKind::Sum(f) => self.fields.get(f).map(|s| s.sum),
            #[allow(clippy::cast_precision_loss)]
            AggKind::Avg(f) => self.fields.get(f).and_then(|s| {
                if s.count == 0 {
                    None
                } else {
                    Some(s.sum / s.count as f64)
                }
            }),
            AggKind::Min(f) => {
                self.fields
                    .get(f)
                    .and_then(|s| if s.count == 0 { None } else { Some(s.min) })
            }
            AggKind::Max(f) => {
                self.fields
                    .get(f)
                    .and_then(|s| if s.count == 0 { None } else { Some(s.max) })
            }
            AggKind::Percentile(f, p) => {
                self.fields.get(f).and_then(|s| s.reservoir.percentile(*p))
            }
        }
    }
}

/// Drives group-by aggregation over a stream of JSON records.
pub struct Aggregator {
    group_by: Vec<String>,
    kinds: Vec<AggKind>,
    groups: HashMap<Vec<String>, GroupState>,
    guard: CardinalityGuard,
    overflow: GroupState,
    reservoir_capacity: usize,
}

/// Sentinel group key used when a record is missing one of the `group by`
/// fields.
const MISSING_KEY: &str = "\u{0}missing";

impl Aggregator {
    #[must_use]
    pub fn new(
        group_by: Vec<String>,
        kinds: Vec<AggKind>,
        cardinality_limit: usize,
        reservoir_capacity: usize,
    ) -> Self {
        Aggregator {
            group_by,
            kinds,
            groups: HashMap::new(),
            guard: CardinalityGuard::new(cardinality_limit),
            overflow: GroupState::new(reservoir_capacity),
            reservoir_capacity,
        }
    }

    pub fn observe(&mut self, record: &Value) {
        let key: Vec<String> = self
            .group_by
            .iter()
            .map(|f| {
                crate::record::lookup_display(record, f).unwrap_or_else(|| MISSING_KEY.to_string())
            })
            .collect();
        let key_str = key.join("\u{1}");

        if self.groups.contains_key(&key) {
            let state = self.groups.get_mut(&key).unwrap_or_else(|| unreachable!());
            state.observe_record(record, &self.kinds);
            return;
        }

        if self.group_by.is_empty() || self.guard.admit(&key_str) {
            let state = self
                .groups
                .entry(key)
                .or_insert_with(|| GroupState::new(self.reservoir_capacity));
            state.observe_record(record, &self.kinds);
        } else {
            self.overflow.observe_record(record, &self.kinds);
        }
    }

    #[must_use]
    pub fn kinds(&self) -> &[AggKind] {
        &self.kinds
    }

    #[must_use]
    pub fn group_by(&self) -> &[String] {
        &self.group_by
    }

    #[must_use]
    pub fn groups(&self) -> &HashMap<Vec<String>, GroupState> {
        &self.groups
    }

    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.guard.is_truncated()
    }

    #[must_use]
    pub fn truncated_count(&self) -> u64 {
        self.guard.truncated_count()
    }

    #[must_use]
    pub fn overflow(&self) -> &GroupState {
        &self.overflow
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_count() {
        assert_eq!(AggKind::parse("count"), Some(AggKind::Count));
        assert_eq!(AggKind::parse("COUNT"), Some(AggKind::Count));
    }

    #[test]
    fn parses_sum_avg_min_max() {
        assert_eq!(
            AggKind::parse("sum(bytes)"),
            Some(AggKind::Sum("bytes".into()))
        );
        assert_eq!(
            AggKind::parse("avg(latency_ms)"),
            Some(AggKind::Avg("latency_ms".into()))
        );
        assert_eq!(AggKind::parse("min(x)"), Some(AggKind::Min("x".into())));
        assert_eq!(AggKind::parse("max(x)"), Some(AggKind::Max("x".into())));
    }

    #[test]
    fn parses_percentiles() {
        assert_eq!(
            AggKind::parse("p99(latency_ms)"),
            Some(AggKind::Percentile("latency_ms".into(), 99.0))
        );
        assert_eq!(
            AggKind::parse("p50(latency_ms)"),
            Some(AggKind::Percentile("latency_ms".into(), 50.0))
        );
        assert_eq!(
            AggKind::parse("p999(latency_ms)"),
            Some(AggKind::Percentile("latency_ms".into(), 999.0))
        );
    }

    #[test]
    fn rejects_malformed_spec() {
        assert_eq!(AggKind::parse("sum"), None);
        assert_eq!(AggKind::parse("sum()"), None);
        assert_eq!(AggKind::parse("bogus(x)"), None);
    }

    #[test]
    fn aggregates_without_group_by() {
        let mut agg = Aggregator::new(
            vec![],
            vec![AggKind::Count, AggKind::Sum("v".into())],
            1000,
            1000,
        );
        for v in [1.0, 2.0, 3.0] {
            agg.observe(&json!({"v": v}));
        }
        let state = agg.groups().values().next().unwrap();
        assert_eq!(state.value(&AggKind::Count), Some(3.0));
        assert_eq!(state.value(&AggKind::Sum("v".into())), Some(6.0));
    }

    #[test]
    fn groups_by_field() {
        let mut agg = Aggregator::new(vec!["service".into()], vec![AggKind::Count], 1000, 1000);
        agg.observe(&json!({"service": "api"}));
        agg.observe(&json!({"service": "api"}));
        agg.observe(&json!({"service": "worker"}));
        assert_eq!(agg.groups().len(), 2);
        let api_key = vec!["api".to_string()];
        assert_eq!(agg.groups()[&api_key].value(&AggKind::Count), Some(2.0));
    }

    #[test]
    fn avg_matches_hand_computed_value() {
        let mut agg = Aggregator::new(vec![], vec![AggKind::Avg("v".into())], 1000, 1000);
        for v in [10.0, 20.0, 30.0, 40.0] {
            agg.observe(&json!({"v": v}));
        }
        let state = agg.groups().values().next().unwrap();
        assert_eq!(state.value(&AggKind::Avg("v".into())), Some(25.0));
    }

    #[test]
    fn min_max_hand_computed() {
        let mut agg = Aggregator::new(
            vec![],
            vec![AggKind::Min("v".into()), AggKind::Max("v".into())],
            1000,
            1000,
        );
        for v in [5.0, -3.0, 42.0, 0.0] {
            agg.observe(&json!({"v": v}));
        }
        let state = agg.groups().values().next().unwrap();
        assert_eq!(state.value(&AggKind::Min("v".into())), Some(-3.0));
        assert_eq!(state.value(&AggKind::Max("v".into())), Some(42.0));
    }

    #[test]
    fn missing_field_yields_none_not_zero() {
        let mut agg = Aggregator::new(vec![], vec![AggKind::Avg("v".into())], 1000, 1000);
        agg.observe(&json!({"other": 1}));
        let state = agg.groups().values().next().unwrap();
        assert_eq!(state.value(&AggKind::Avg("v".into())), None);
    }

    #[test]
    fn cardinality_guard_truncates_and_routes_overflow() {
        let mut agg = Aggregator::new(vec!["id".into()], vec![AggKind::Count], 2, 1000);
        agg.observe(&json!({"id": "a"}));
        agg.observe(&json!({"id": "b"}));
        agg.observe(&json!({"id": "c"}));
        agg.observe(&json!({"id": "d"}));
        assert_eq!(agg.groups().len(), 2);
        assert!(agg.is_truncated());
        assert_eq!(agg.truncated_count(), 2);
        assert_eq!(agg.overflow().row_count, 2);
    }

    #[test]
    fn multi_field_group_by() {
        let mut agg = Aggregator::new(
            vec!["service".into(), "status".into()],
            vec![AggKind::Count],
            1000,
            1000,
        );
        agg.observe(&json!({"service": "api", "status": 200}));
        agg.observe(&json!({"service": "api", "status": 500}));
        agg.observe(&json!({"service": "api", "status": 200}));
        assert_eq!(agg.groups().len(), 2);
    }
}
