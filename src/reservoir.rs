//! A fixed-capacity reservoir sampler used to estimate percentiles over a
//! stream of numbers in bounded memory.
//!
//! # Accuracy trade-off
//!
//! This is **not** an exact percentile. It keeps a uniform random sample of
//! at most `capacity` values (Algorithm R) and reports percentiles computed
//! over that sample. For a stream of `n` values with `capacity = 10_000`
//! (the default), the standard error of a reported percentile is
//! approximately `sqrt(p * (1 - p) / capacity)`, independent of `n` once
//! `n > capacity`. In practice this means p50/p90 are accurate to within a
//! fraction of a percentage point, while extreme tails (p99.9 and beyond)
//! are noisier because fewer sampled points land there. Memory use is
//! `O(capacity)` regardless of how many records are processed, which is the
//! property that makes streaming a multi-gigabyte file practical.

use std::cmp::Ordering;

/// Default reservoir capacity used when none is configured.
pub const DEFAULT_CAPACITY: usize = 10_000;

/// A simple splitmix64-based PRNG so the crate does not need an external
/// randomness dependency for something this small.
#[derive(Debug, Clone)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        SplitMix64 { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform integer in `[0, bound)`.
    fn next_below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        self.next_u64() % bound
    }
}

/// Streaming percentile estimator backed by reservoir sampling.
#[derive(Debug, Clone)]
pub struct ReservoirSampler {
    capacity: usize,
    seen: u64,
    samples: Vec<f64>,
    rng: SplitMix64,
}

impl ReservoirSampler {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        ReservoirSampler {
            capacity: capacity.max(1),
            seen: 0,
            samples: Vec::with_capacity(capacity.min(1024)),
            rng: SplitMix64::new(0xD1CE_F00D_DEAD_BEEF),
        }
    }

    #[must_use]
    pub fn with_default_capacity() -> Self {
        Self::new(DEFAULT_CAPACITY)
    }

    pub fn observe(&mut self, value: f64) {
        self.seen += 1;
        if self.samples.len() < self.capacity {
            self.samples.push(value);
        } else {
            let j = self.rng.next_below(self.seen);
            if let Ok(j) = usize::try_from(j) {
                if j < self.capacity {
                    self.samples[j] = value;
                }
            }
        }
    }

    #[must_use]
    pub fn count(&self) -> u64 {
        self.seen
    }

    #[must_use]
    pub fn sample_len(&self) -> usize {
        self.samples.len()
    }

    /// Nearest-rank percentile over the current sample. `p` is in `[0, 100]`.
    /// Returns `None` if no values have been observed.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub fn percentile(&self, p: f64) -> Option<f64> {
        if self.samples.is_empty() {
            return None;
        }
        let mut sorted = self.samples.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal));
        let p = p.clamp(0.0, 100.0);
        if sorted.len() == 1 {
            return Some(sorted[0]);
        }
        // Nearest-rank method: rank in [1, n], mapped to a 0-based index.
        let rank = (p / 100.0 * (sorted.len() as f64 - 1.0)).round();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let idx = (rank as usize).min(sorted.len() - 1);
        Some(sorted[idx])
    }

    pub fn merge(&mut self, other: &ReservoirSampler) {
        for &v in &other.samples {
            self.observe(v);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn empty_reservoir_has_no_percentile() {
        let r = ReservoirSampler::with_default_capacity();
        assert_eq!(r.percentile(50.0), None);
    }

    #[test]
    fn single_element_reservoir() {
        let mut r = ReservoirSampler::with_default_capacity();
        r.observe(42.0);
        assert_eq!(r.percentile(0.0), Some(42.0));
        assert_eq!(r.percentile(50.0), Some(42.0));
        assert_eq!(r.percentile(99.0), Some(42.0));
    }

    #[test]
    fn all_identical_values() {
        let mut r = ReservoirSampler::with_default_capacity();
        for _ in 0..1000 {
            r.observe(7.0);
        }
        assert_eq!(r.percentile(50.0), Some(7.0));
        assert_eq!(r.percentile(99.0), Some(7.0));
    }

    #[test]
    fn odd_count_exact_percentiles_under_capacity() {
        let mut r = ReservoirSampler::new(1000);
        for v in 1..=9 {
            r.observe(f64::from(v));
        }
        // 9 values 1..=9, nearest-rank p50 should be the median = 5.
        assert_eq!(r.percentile(50.0), Some(5.0));
        assert_eq!(r.percentile(0.0), Some(1.0));
        assert_eq!(r.percentile(100.0), Some(9.0));
    }

    #[test]
    fn even_count_exact_percentiles_under_capacity() {
        let mut r = ReservoirSampler::new(1000);
        for v in 1..=10 {
            r.observe(f64::from(v));
        }
        // nearest-rank on 10 elements at p50: rank index round(0.5*9)=round(4.5)=4 (0-based) -> value 5
        let p50 = r.percentile(50.0).unwrap();
        assert!((4.0..=6.0).contains(&p50));
    }

    #[test]
    fn count_tracks_all_observations_even_beyond_capacity() {
        let mut r = ReservoirSampler::new(10);
        for v in 0..10_000 {
            r.observe(f64::from(v));
        }
        assert_eq!(r.count(), 10_000);
        assert_eq!(r.sample_len(), 10);
    }

    #[test]
    fn reservoir_stays_within_capacity_bound() {
        let mut r = ReservoirSampler::new(100);
        for v in 0..1_000_000 {
            r.observe(f64::from(v % 1000));
        }
        assert!(r.sample_len() <= 100);
        assert_eq!(r.count(), 1_000_000);
    }

    #[test]
    fn percentile_is_within_reasonable_error_on_uniform_data() {
        let mut r = ReservoirSampler::new(5000);
        for v in 0..100_000 {
            r.observe(f64::from(v % 1000));
        }
        let p50 = r.percentile(50.0).unwrap();
        // True p50 of uniform [0, 999] is ~500. Allow generous slack for sampling noise.
        assert!((450.0..=550.0).contains(&p50), "p50 = {p50}");
    }

    #[test]
    fn merge_combines_two_reservoirs() {
        let mut a = ReservoirSampler::new(1000);
        let mut b = ReservoirSampler::new(1000);
        for v in 0..500 {
            a.observe(f64::from(v));
        }
        for v in 500..1000 {
            b.observe(f64::from(v));
        }
        a.merge(&b);
        assert_eq!(a.count(), 1000);
    }
}
