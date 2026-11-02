//! Lineage tracking — replacing the email's Prime Integer Relations (PIR).
//!
//! ## Email claim kept
//! Lineage should be cheap to record and cheap to query, in the slot
//! header, without following pointer tables or scanning a list.
//!
//! ## Email claim fixed: PIR prime-product does not work
//! The email (§II.4, §III) tracks history as one `u64`
//! `S <- S * p_node` with distinct primes per node (3, 5, 7, 11, 13) and
//! queries `S % p == 0`. That is unsound for four independent reasons:
//!
//! 1. **Overflow wrap.** `u64` holds e.g. `3^40` but not `3^41`.
//!    `wrapping_mul` (what the email's code uses) reduces mod `2^64`,
//!    and divisibility in the integers does not survive reduction
//!    mod `2^64`: after wrap, `S % 5 == 0` can hold even though node 5
//!    never ran. See `pir_legacy::tests::overflow_creates_false_positive`.
//! 2. **Counts are lost.** `3 * 3` and `3` differ, but a history that
//!    runs node 3 seventy times is indistinguishable in kind from any
//!    other large power once wrapped; even before wrap, recovering a
//!    count requires factoring, not one modulo.
//! 3. **Order is lost.** Multiplication commutes: `3*5 == 5*3`. PIR can
//!    never say which ran first. (The email never claims order, but a
//!    lineage system that silently cannot represent it invites misuse.)
//! 4. **False positives before overflow, by construction.** Any
//!    composite state that happens to include `p` as a factor for a
//!    *different* reason (e.g. an initial seed value, or a count
//!    encoding multiplied in) reads as "visited".
//!
//! ## The honest replacement
//! * `visited_mask: u64` — bit `n` set iff node `n` ran at least once.
//!   Query is one AND. Exact, no false positives, for up to 64 nodes.
//! * `counts: [u32; 16]` — exact visit counts for the first 16 nodes
//!   (saturating at `u32::MAX`, never wrapping silently).
//!
//! What this still does **not** give you: order. If order matters,
//! store a sequence log; do not pretend a scalar encodes it. That
//! limitation is stated here rather than hidden inside a "proof".

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Nodes whose exact counts are tracked. Visited bits cover 64 nodes.
pub const COUNTED_NODES: usize = 16;
/// Nodes coverable by the visited bitmask.
pub const MAX_NODES: usize = 64;

/// Slot-header lineage: visited bitmask + per-node counts.
///
/// Email claim fixed: replaces the single PIR `u64` product
/// (`token_history`) in the email's `UnifiedMemorySlot`.
pub struct LineageHeader {
    visited_mask: AtomicU64,
    counts: [AtomicU32; COUNTED_NODES],
}

impl LineageHeader {
    /// A fresh header: nothing visited, all counts zero.
    pub fn new() -> Self {
        Self {
            visited_mask: AtomicU64::new(0),
            counts: std::array::from_fn(|_| AtomicU32::new(0)),
        }
    }

    /// Record one visit by `node` (0..64). Panics on `node >= 64`,
    /// because silently dropping a node would corrupt lineage.
    pub fn record(&self, node: usize) {
        assert!(node < MAX_NODES, "node id {node} exceeds bitmask");
        self.visited_mask.fetch_or(1u64 << node, Ordering::AcqRel);
        if node < COUNTED_NODES {
            // Saturating: a wrapped count is a wrong count.
            let counter = &self.counts[node];
            let mut cur = counter.load(Ordering::Acquire);
            while cur < u32::MAX {
                match counter.compare_exchange_weak(
                    cur,
                    cur + 1,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(actual) => cur = actual,
                }
            }
        }
    }

    /// True iff `node` has been recorded at least once.
    ///
    /// # Panics
    /// Panics on `node >= 64`, same contract as [`Self::record`].
    pub fn visited(&self, node: usize) -> bool {
        assert!(node < MAX_NODES, "node id {node} exceeds bitmask");
        self.visited_mask.load(Ordering::Acquire) & (1u64 << node) != 0
    }

    /// Exact count for nodes < [`COUNTED_NODES`]; `None` otherwise
    /// (for those nodes only visited/not-visited is tracked).
    pub fn count(&self, node: usize) -> Option<u32> {
        if node < COUNTED_NODES {
            Some(self.counts[node].load(Ordering::Acquire))
        } else {
            None
        }
    }

    /// The raw visited bitmask: bit `n` set iff node `n` ran.
    pub fn visited_mask(&self) -> u64 {
        self.visited_mask.load(Ordering::Acquire)
    }

    /// Reset for slot reuse (call only while holding the slot lease —
    /// see `substrate`).
    pub fn clear(&self) {
        self.visited_mask.store(0, Ordering::Release);
        for c in &self.counts {
            c.store(0, Ordering::Release);
        }
    }
}

impl Default for LineageHeader {
    fn default() -> Self {
        Self::new()
    }
}

/// The email's PIR scheme, preserved only to demonstrate its failure.
///
/// Do not use for real lineage. See the module docs.
pub mod pir_legacy {
    /// `S <- S * prime`, wrapping — exactly as the email's code does.
    pub fn update(state: u64, prime: u64) -> u64 {
        state.wrapping_mul(prime)
    }
    /// `history % prime == 0` — exactly the email's query.
    ///
    /// Kept verbatim on purpose (hence the clippy allow): this module
    /// is a faithful counterexample, and `%`-vs-`is_multiple_of` is
    /// not merely stylistic here — `is_multiple_of(0)` is defined as
    /// `state == 0` where the email's `%` panics. Precondition, as in
    /// the email: `prime` is one of the node primes, i.e. non-zero;
    /// `visited(_, 0)` divides by zero and panics.
    #[allow(clippy::manual_is_multiple_of)]
    pub fn visited(state: u64, prime: u64) -> bool {
        state % prime == 0
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        /// Failing-by-design demonstration: multiply by prime 3
        /// repeatedly. In the integers `3^k` is never divisible by 5,
        /// so node 5 (prime 5) can never legitimately appear. After
        /// `u64` overflow wrap it does — a false positive produced
        /// purely by the representation.
        #[test]
        fn overflow_creates_false_positive() {
            let mut s: u64 = 1;
            let mut false_positive_at = None;
            for k in 1..=200u32 {
                s = update(s, 3);
                if visited(s, 5) {
                    false_positive_at = Some(k);
                    break;
                }
            }
            assert!(
                false_positive_at.is_some(),
                "expected PIR to produce a false positive for prime 5 after overflow"
            );
            // And the wrap really happened well before that point:
            // 3^41 does not fit in u64.
            assert!(3u128.pow(41) > u64::MAX as u128);
        }

        #[test]
        fn pir_loses_order() {
            // 3 then 5 == 5 then 3: order is unrepresentable.
            assert_eq!(update(update(1, 3), 5), update(update(1, 5), 3));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitmask_and_counts_are_exact() {
        let h = LineageHeader::new();
        h.record(3);
        h.record(3);
        h.record(13);
        assert!(h.visited(3));
        assert!(h.visited(13));
        assert!(!h.visited(5));
        assert_eq!(h.count(3), Some(2));
        assert_eq!(h.count(13), Some(1));
        assert_eq!(h.count(5), Some(0));
    }

    #[test]
    fn many_visits_do_not_create_false_positives() {
        let h = LineageHeader::new();
        for _ in 0..10_000 {
            h.record(0);
        }
        assert_eq!(h.count(0), Some(10_000));
        for n in 1..MAX_NODES {
            assert!(!h.visited(n), "false positive for node {n}");
        }
    }

    #[test]
    fn clear_resets() {
        let h = LineageHeader::new();
        h.record(7);
        h.clear();
        assert!(!h.visited(7));
        assert_eq!(h.count(7), Some(0));
        assert_eq!(h.visited_mask(), 0);
    }

    #[test]
    fn high_nodes_visited_but_not_counted() {
        let h = LineageHeader::new();
        h.record(40);
        assert!(h.visited(40));
        assert_eq!(h.count(40), None);
    }

    #[test]
    fn counts_saturate_at_u32_max_instead_of_wrapping() {
        // Boundary behaviour, seeded directly (4 billion real visits
        // are not a test plan): at the cap the counter must pin at
        // u32::MAX — a wrap to 0 would read as "never visited with a
        // count of 0" while the visited bit says otherwise.
        let h = LineageHeader::new();
        h.counts[0].store(u32::MAX - 1, Ordering::SeqCst);
        h.record(0);
        assert_eq!(h.count(0), Some(u32::MAX));
        h.record(0);
        h.record(0);
        assert_eq!(h.count(0), Some(u32::MAX), "count must saturate, not wrap");
        assert!(h.visited(0));
    }

    #[test]
    #[should_panic(expected = "exceeds bitmask")]
    fn record_at_max_nodes_panics() {
        // The fence on the other side of the boundary: node 63 is the
        // last legal id (see the property tests), 64 is a loud
        // contract failure, never a silent drop.
        LineageHeader::new().record(MAX_NODES);
    }
}
