//! A bounded-cardinality guard for `group by`.
//!
//! Aggregating by an unexpected high-cardinality field (a request id, a raw
//! URL with query strings, ...) can otherwise grow the group table without
//! bound. [`CardinalityGuard`] caps the number of distinct groups and routes
//! any key beyond the cap into a single reported "truncated" bucket instead
//! of allocating a new group for it, so memory use stays `O(limit)`.

use std::collections::HashSet;

#[derive(Debug, Clone)]
pub struct CardinalityGuard {
    limit: usize,
    seen: HashSet<String>,
    truncated_count: u64,
}

impl CardinalityGuard {
    #[must_use]
    pub fn new(limit: usize) -> Self {
        CardinalityGuard {
            limit: limit.max(1),
            seen: HashSet::new(),
            truncated_count: 0,
        }
    }

    /// Returns `true` if `key` should be admitted as its own group, `false`
    /// if it was rejected because the cardinality limit was reached (the
    /// caller should route it to an overflow bucket).
    pub fn admit(&mut self, key: &str) -> bool {
        if self.seen.contains(key) {
            return true;
        }
        if self.seen.len() >= self.limit {
            self.truncated_count += 1;
            return false;
        }
        self.seen.insert(key.to_string());
        true
    }

    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.truncated_count > 0
    }

    #[must_use]
    pub fn truncated_count(&self) -> u64 {
        self.truncated_count
    }

    #[must_use]
    pub fn group_count(&self) -> usize {
        self.seen.len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn admits_up_to_limit() {
        let mut g = CardinalityGuard::new(3);
        assert!(g.admit("a"));
        assert!(g.admit("b"));
        assert!(g.admit("c"));
        assert!(!g.admit("d"));
        assert_eq!(g.group_count(), 3);
    }

    #[test]
    fn repeated_keys_never_count_against_limit() {
        let mut g = CardinalityGuard::new(2);
        assert!(g.admit("a"));
        assert!(g.admit("a"));
        assert!(g.admit("a"));
        assert!(g.admit("b"));
        assert_eq!(g.group_count(), 2);
        assert!(!g.is_truncated());
    }

    #[test]
    fn reports_truncation_count() {
        let mut g = CardinalityGuard::new(1);
        assert!(g.admit("a"));
        assert!(!g.admit("b"));
        assert!(!g.admit("c"));
        assert!(g.is_truncated());
        assert_eq!(g.truncated_count(), 2);
    }

    #[test]
    fn zero_limit_treated_as_one() {
        let mut g = CardinalityGuard::new(0);
        assert!(g.admit("only"));
        assert!(!g.admit("other"));
    }
}
