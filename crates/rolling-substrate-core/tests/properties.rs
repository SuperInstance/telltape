//! Randomised invariant (property) tests for rolling-substrate-core.
//!
//! Std-only, hand-rolled: the generator is the same LCG family the
//! workspace uses elsewhere (`cuda_thor::test_input`: Knuth's
//! 1664525/1013904223 constants), re-seeded per round so every
//! failure reproduces from the seed printed in the assertion message.
//! No new dependencies — that is a crate rule, not an oversight.
//!
//! Properties pinned here:
//! * ring — FIFO order and no loss under interleaved
//!   producer/consumer schedules, across many capacity wraparounds;
//!   multi-producer claims stay unique and complete under random
//!   shapes.
//! * lineage — mask/count conservation against a naive model under
//!   random op streams (including `clear`).
//! * sheaf — `energy`/`laplacian_apply` agree with a naive dense
//!   `D^T D` Laplacian on random graphs, and energy never increases
//!   under `smooth_step` at a degree-derived safe step size.

use rolling_substrate_core::gating::Viewport;
use rolling_substrate_core::lineage::LineageHeader;
use rolling_substrate_core::sheaf::SheafGraph;
use rolling_substrate_core::substrate::FluidSubstrateBuffer;
use std::collections::BTreeSet;
use std::sync::Arc;

/// The workspace LCG (same constants as `cuda_thor::test_input`),
/// seedable so failures reproduce.
struct Rng(u32);

impl Rng {
    fn new(seed: u32) -> Self {
        // Avoid the degenerate all-zero state.
        Rng(seed | 1)
    }
    fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0
    }
    /// Uniform-ish value in `0..n` (n > 0). Modulo bias is irrelevant
    /// for test generation.
    fn below(&mut self, n: usize) -> usize {
        (self.next_u32() as usize) % n
    }
    fn f64_range(&mut self, lo: f64, hi: f64) -> f64 {
        let t = (self.next_u32() as f64) / (u32::MAX as f64);
        lo + t * (hi - lo)
    }
    fn coin(&mut self, p: f64) -> bool {
        self.f64_range(0.0, 1.0) < p
    }
}

// ---------------------------------------------------------------- ring

#[test]
fn ring_fifo_no_loss_across_wraparounds() {
    for seed in 1..=30u32 {
        let mut rng = Rng::new(seed.wrapping_mul(0x9E37_79B9));
        let capacity = [2usize, 4, 8, 16][rng.below(4)];
        let buf = FluidSubstrateBuffer::new(capacity, 4);
        let mut produced: u64 = 0; // next claim to lease
        let mut consumed: u64 = 0; // next claim to read back
                                   // 400 steps at capacity <= 16 is >= 25 laps: wraparound is
                                   // exercised dozens of times, not once.
        for step in 0..400 {
            let outstanding = produced - consumed;
            let produce = outstanding < capacity as u64 && (outstanding == 0 || rng.coin(0.6));
            if produce {
                let mut lease = buf.lease();
                assert_eq!(lease.claim(), produced, "seed {seed} step {step}");
                lease.data_mut().fill(produced as f32);
                lease.finalize();
                produced += 1;
            } else {
                // Consumer contract for `read_published`: claim
                // `consumed` is published (finalized synchronously
                // above) and its slot cannot have been re-leased —
                // we never let outstanding reach capacity, and the
                // slot of `consumed` is next re-leased by claim
                // `consumed + capacity`, which requires
                // outstanding == capacity first.
                assert!(buf.is_published(consumed), "seed {seed} step {step}");
                let data = unsafe { buf.read_published(consumed) };
                assert!(
                    data.iter().all(|&v| v == consumed as f32),
                    "seed {seed} step {step}: FIFO violation at claim {consumed}"
                );
                consumed += 1;
            }
        }
        assert!(
            produced >= 3 * capacity as u64,
            "seed {seed}: only {produced} claims at capacity {capacity} — wraparound under-exercised"
        );
        // Drain: everything produced must still read back in order.
        while consumed < produced {
            let data = unsafe { buf.read_published(consumed) };
            assert!(
                data.iter().all(|&v| v == consumed as f32),
                "seed {seed} drain"
            );
            consumed += 1;
        }
    }
}

#[test]
fn ring_multi_producer_claims_unique_under_random_shapes() {
    for seed in 1..=8u32 {
        let mut rng = Rng::new(seed.wrapping_mul(0x85EB_CA6B));
        let capacity = [2usize, 8, 32][rng.below(3)];
        let threads = 2 + rng.below(3); // 2..=4 producers
        let ops: Vec<u64> = (0..threads).map(|_| 10 + rng.below(50) as u64).collect();
        let total: u64 = ops.iter().sum();
        let buf = Arc::new(FluidSubstrateBuffer::new(capacity, 8));
        let handles: Vec<_> = ops
            .iter()
            .map(|&n| {
                let b = Arc::clone(&buf);
                std::thread::spawn(move || {
                    let mut claims = Vec::new();
                    for _ in 0..n {
                        let mut lease = b.lease();
                        let c = lease.claim();
                        lease.data_mut().fill(c as f32);
                        claims.push(c);
                        lease.finalize();
                    }
                    claims
                })
            })
            .collect();
        let mut all: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().expect("producer thread panicked"))
            .collect();
        all.sort_unstable();
        assert_eq!(
            all,
            (0..total).collect::<Vec<_>>(),
            "seed {seed}: claims must be exactly 0..{total}, each once"
        );
        for c in 0..total {
            assert!(buf.is_published(c), "seed {seed}: claim {c} unpublished");
        }
    }
}

// ------------------------------------------------------------- lineage

#[test]
fn lineage_matches_naive_model_under_random_ops() {
    for seed in 1..=30u32 {
        let mut rng = Rng::new(seed.wrapping_mul(0xC2B2_AE35));
        let h = LineageHeader::new();
        let mut model_mask: u64 = 0;
        let mut model_counts = [0u32; 16];
        for step in 0..300 {
            if rng.coin(0.03) {
                h.clear();
                model_mask = 0;
                model_counts = [0; 16];
            } else {
                let node = rng.below(64);
                h.record(node);
                model_mask |= 1u64 << node;
                if node < 16 {
                    model_counts[node] += 1;
                }
            }
            if step % 37 == 0 || step == 299 {
                assert_eq!(h.visited_mask(), model_mask, "seed {seed} step {step}");
                for n in 0..64 {
                    assert_eq!(
                        h.visited(n),
                        model_mask & (1u64 << n) != 0,
                        "seed {seed} step {step} node {n}"
                    );
                }
                for (n, &want) in model_counts.iter().enumerate() {
                    assert_eq!(h.count(n), Some(want), "seed {seed} node {n}");
                }
                for n in 16..64 {
                    assert_eq!(h.count(n), None, "seed {seed} node {n}");
                }
            }
        }
    }
}

// --------------------------------------------------------------- sheaf

/// Naive dense Laplacian from the edge list: `L = D^T D` accumulated
/// entry by entry. Deliberately the textbook construction, not the
/// crate's closed form — agreement between the two is the property.
fn naive_laplacian(n: usize, edges: &[(usize, usize)]) -> Vec<Vec<f64>> {
    let mut l = vec![vec![0.0f64; n]; n];
    for &(i, j) in edges {
        l[i][i] += 1.0;
        l[j][j] += 1.0;
        l[i][j] -= 1.0;
        l[j][i] -= 1.0;
    }
    l
}

#[test]
fn sheaf_matches_naive_laplacian_and_refine_never_raises_energy() {
    for seed in 1..=40u32 {
        let mut rng = Rng::new(seed.wrapping_mul(0x27D4_EB2F));
        let n = 2 + rng.below(24); // 2..=25 nodes
        let mut edge_set = BTreeSet::new();
        for i in 0..n {
            for j in (i + 1)..n {
                if rng.coin(0.22) {
                    edge_set.insert((i, j));
                }
            }
        }
        let edges: Vec<(usize, usize)> = edge_set.into_iter().collect();
        let g = SheafGraph::from_edges(n, edges.clone());
        let s: Vec<f64> = (0..n).map(|_| rng.f64_range(-10.0, 10.0)).collect();

        // Agreement with the naive dense construction.
        let l = naive_laplacian(n, &edges);
        let naive_apply: Vec<f64> = (0..n)
            .map(|i| (0..n).map(|j| l[i][j] * s[j]).sum())
            .collect();
        let naive_energy: f64 = s.iter().zip(&naive_apply).map(|(a, b)| a * b).sum();
        let apply = g.laplacian_apply(&s);
        for i in 0..n {
            assert!(
                (apply[i] - naive_apply[i]).abs() <= 1e-9 * (1.0 + naive_apply[i].abs()),
                "seed {seed}: laplacian_apply[{i}] {} vs naive {}",
                apply[i],
                naive_apply[i]
            );
        }
        let e0 = g.energy(&s);
        assert!(
            (e0 - naive_energy).abs() <= 1e-9 * (1.0 + naive_energy.abs()),
            "seed {seed}: energy {e0} vs naive {naive_energy}"
        );

        // Refine: at a degree-derived safe step (alpha = 1/(2*dmax)
        // implies alpha * lambda_max <= 1 < 2), energy must never
        // increase, and the section sum is invariant (1^T L = 0).
        let mut degree = vec![0usize; n];
        for &(i, j) in &edges {
            degree[i] += 1;
            degree[j] += 1;
        }
        let dmax = degree.iter().copied().max().unwrap_or(0);
        if dmax == 0 {
            assert_eq!(e0, 0.0, "seed {seed}: edgeless graph must have zero energy");
            continue;
        }
        let alpha = 1.0 / (2.0 * dmax as f64);
        let sum0: f64 = s.iter().sum();
        let mut cur = s.clone();
        let mut prev = e0;
        for step in 0..30 {
            let before = g.smooth_step(&mut cur, alpha);
            assert!(
                (before - prev).abs() <= 1e-9 * (1.0 + prev.abs()),
                "seed {seed} step {step}: smooth_step must return the pre-step energy"
            );
            let after = g.energy(&cur);
            assert!(
                after <= prev + 1e-9 * (1.0 + prev.abs()),
                "seed {seed} step {step}: energy increased {prev} -> {after}"
            );
            prev = after;
        }
        let sum1: f64 = cur.iter().sum();
        assert!(
            (sum0 - sum1).abs() <= 1e-6 * (1.0 + sum0.abs()),
            "seed {seed}: section sum not preserved {sum0} -> {sum1}"
        );
    }
}

// -------------------------------------------------------------- gating

#[test]
fn gating_counts_and_untouched_outside_under_random_viewports() {
    for seed in 1..=30u32 {
        let mut rng = Rng::new(seed.wrapping_mul(0x1656_6671));
        let (w, h) = (1 + rng.below(24), 1 + rng.below(24));
        // Bounds may exceed the grid and may be empty, but are never
        // inverted (Viewport::new's contract).
        let xa = rng.below(w + 4);
        let xb = xa + rng.below(6);
        let ya = rng.below(h + 3).min(h + 2);
        let yb = ya + rng.below(6);
        let vp = Viewport::new(xa as u32, xb as u32, ya as u32, yb as u32);
        let original: Vec<i64> = (0..w * h).map(|i| i as i64).collect();
        let mut data = original.clone();
        let stats = rolling_substrate_core::gating::gated_apply(&mut data, w, h, &vp, |v| {
            *v = -*v - 1;
        });
        let mut want_processed = 0usize;
        for y in 0..h {
            for x in 0..w {
                let idx = y * w + x;
                if vp.contains(x as u32, y as u32) {
                    want_processed += 1;
                    assert_eq!(data[idx], -original[idx] - 1, "seed {seed} at ({x},{y})");
                } else {
                    assert_eq!(data[idx], original[idx], "seed {seed}: outside ROI touched");
                }
            }
        }
        assert_eq!(stats.processed, want_processed, "seed {seed}");
        assert_eq!(stats.processed + stats.skipped, w * h, "seed {seed}");
    }
}
