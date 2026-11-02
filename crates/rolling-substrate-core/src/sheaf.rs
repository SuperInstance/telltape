//! Sheaf / Laplacian smoothing — Pillar 1's maths, done for real.
//!
//! ## Email claim kept
//! Local sections that disagree with their neighbours should be
//! pulled toward global consistency, and consistency should be
//! measurable. The email names the sheaf Laplacian
//! `Δ_F = D · D^T` over a cellular sheaf.
//!
//! ## Email claim fixed
//! The email's code never builds `D` or `Δ_F`. Its
//! `enforce_sheaf_constraints` is:
//! `if |v| > 10.0 { v *= 0.333 }` — a per-element clamp. A clamp knows
//! nothing about neighbours: `[100, 100, 100]` (perfectly consistent,
//! just large) gets mangled, while `[9, -9, 9, -9]` (maximally
//! inconsistent) passes untouched. That is the opposite of a
//! consistency operator.
//!
//! ## What this module actually does
//! For a graph `G = (V, E)` with scalar sections `s: V -> f64` and the
//! (unsigned) incidence matrix `D` of `G`, the graph Laplacian is
//! `L = D^T D`, and:
//!
//! * energy `E(s) = s^T L s = Σ_{(i,j)∈E} (s_i - s_j)^2 = ||D s||²`
//! * `E(s) = 0` iff `s` is constant on each connected component —
//!   i.e. iff local sections agree everywhere they overlap, which is
//!   exactly the `H^0` consistency condition the email invokes.
//! * The smoothing step `s <- s - α L s` is gradient descent on `E`,
//!   since `∇E = 2 L s`. For `0 < α < 1 / λ_max(L)` it strictly
//!   decreases `E` on any inconsistent `s`. (For line/ring graphs
//!   `λ_max ≤ 4`, so `α < 0.25` is safe; tests use `α = 0.1`.)
//!
//! `D` is never materialised as a matrix — with one scalar per node,
//! `L s` has the closed form `(L s)_i = deg(i)·s_i − Σ_{j~i} s_j`,
//! and `E` is a sum over edges. Claiming a matrix multiply here would
//! be theatre; the implicit form *is* the computation.
//!
//! General sheaf Laplacians with non-identity restriction maps are
//! out of scope and not claimed: this is the constant-sheaf case,
//! which is the case the email's prose actually describes.
//!
//! ## Allocation (2026-11-01, SHEAF-FIX — closes ARCHITECTURE D11)
//!
//! DESIGN §4 claims the sheaf step allocates nothing (pre-allocated
//! scratch); the engine book recorded that as *contradicted*:
//! `laplacian_apply` allocated a fresh `Vec<f64>` on every call, and
//! `smooth_step` inherited one allocation per step. That is now
//! closed. [`SheafGraph::laplacian_apply_into`] writes into a
//! caller-owned buffer with the *identical* accumulation order (same
//! edge order, same per-edge expressions), so its output is
//! bit-identical to the legacy path — pinned by a bitwise-equality
//! test over seeded random graphs, and an allocation-counting test
//! (`tests/sheaf_alloc.rs`) proves the into-path performs **zero**
//! allocations in steady state while the legacy path performs one
//! per call. [`SheafGraph::smooth_step_into`] is the same step with
//! caller scratch. The allocating signatures are unchanged and
//! delegate to the into-paths, so existing consumers
//! (harbor-watch, rolling-pipeline) are source- and
//! behaviour-identical. Bench: `examples/sheaf_bench.rs`.
//!
//! ## Validation — INV-S4 and the α range (2026-11-02, PROOF-SWEEP —
//! closes ARCHITECTURE D9 and D10)
//!
//! DESIGN §4 specifies two guards the first build never implemented
//! (a probe test recorded the behaviour before the fix: one NaN in a
//! section *spread* to its neighbours through the step, `α = NaN`
//! returned a plausible finite energy while poisoning the whole
//! section, and `α = 10` diverged 54 → 53,214 without a word):
//!
//! * **INV-S4 (D9).** NaN in a section ⇒ `energy` returns NaN *and
//!   the step refuses to run*, surfacing [`SheafError::NonFinite`]
//!   (failure mode F6). Silently "correcting" NaNs would make a
//!   broken producer invisible; silently *propagating* them (the
//!   old behaviour) makes one bad sensor reading poison the graph.
//!   The checked step scans the section and the step size for
//!   finiteness before touching anything, so a refused step leaves
//!   the section bit-for-bit unchanged.
//! * **α range (D10).** Step sizes are validated
//!   `0 < α ≤ 2 / λmax(L)`, with `λmax` estimated by power iteration
//!   at construction ([`SheafGraph::spectral_bound`]; fixed
//!   iteration count, [`SPECTRAL_ITERATIONS`]). An out-of-range α
//!   at construction of a [`Smoother`] — or at any checked step —
//!   is an error, not a clamp. The estimate is a Rayleigh quotient,
//!   so it never exceeds the true `λmax` in exact arithmetic; on
//!   graphs whose top eigenvalues are near-degenerate it can sit
//!   *below* `λmax`, making the admitted bound slightly generous.
//!   Callers near the boundary should keep margin; `α ≤
//!   1 / (2 · max_degree)` is always safely inside (Gershgorin:
//!   `λmax ≤ 2 · max_degree`).
//!
//! The legacy `smooth_step` / `smooth_step_into` signatures keep
//! working and delegate to the checked path, panicking on refusal
//! (see their `# Panics` docs) — so every existing caller gained
//! the refusal without a signature change. Per-step cost of the
//! checks: one O(n) finiteness scan plus an O(1) α comparison, on
//! top of the O(E) energy + apply the step already performs.

/// Fixed iteration count for the power iteration behind
/// [`SheafGraph::spectral_bound`] — DESIGN §4 asks for the count to
/// be fixed and recorded; this is the record.
pub const SPECTRAL_ITERATIONS: usize = 100;

/// Why a sheaf step refused to run.
///
/// DESIGN §4 names the non-finite case (`SheafError::NonFinite`,
/// failure mode F6); the α case is the same refusal shape for the
/// step-size half of the contract.
#[derive(Debug, Clone, PartialEq)]
pub enum SheafError {
    /// INV-S4: a section element or the step size was NaN or ±∞.
    /// The step did not run and the section is unchanged.
    NonFinite,
    /// α outside the validated stability range `0 < α ≤ bound`,
    /// where `bound = 2 / λmax(L)` from [`SheafGraph::spectral_bound`].
    AlphaOutOfRange {
        /// The rejected step size.
        alpha: f64,
        /// The largest admissible step size for this graph.
        bound: f64,
    },
}

impl std::fmt::Display for SheafError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SheafError::NonFinite => write!(
                f,
                "non-finite value in the section or the step size \
                 (INV-S4: the step refuses rather than propagate NaN/inf)"
            ),
            SheafError::AlphaOutOfRange { alpha, bound } => write!(
                f,
                "alpha {alpha} outside the validated range 0 < alpha <= {bound} (2/lambda_max)"
            ),
        }
    }
}

impl std::error::Error for SheafError {}

/// λmax(L) by power iteration: [`SPECTRAL_ITERATIONS`] rounds from
/// a deterministic hash start, Rayleigh quotient of the final
/// iterate. The quotient never exceeds the true λmax in exact
/// arithmetic; it converges from below. An edgeless graph has
/// L = 0, hence λmax = 0.
fn power_lambda_max(n: usize, edges: &[(usize, usize)]) -> f64 {
    if n == 0 || edges.is_empty() {
        return 0.0;
    }
    // SplitMix64 over the index: a deterministic start with energy
    // on every eigenvector, including the top one. (A constant start
    // would be annihilated — constants are L's null space.)
    let mut v: Vec<f64> = (0..n)
        .map(|i| {
            let mut z = (i as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            (z >> 11) as f64 / (1u64 << 53) as f64 - 0.5
        })
        .collect();
    let norm = v.iter().map(|x| x * x).sum::<f64>().sqrt();
    if norm == 0.0 {
        return 0.0;
    }
    for x in v.iter_mut() {
        *x /= norm;
    }
    let mut w = vec![0.0f64; n];
    let mut lambda = 0.0f64;
    for _ in 0..SPECTRAL_ITERATIONS {
        w.fill(0.0);
        for &(i, j) in edges {
            w[i] += v[i] - v[j];
            w[j] += v[j] - v[i];
        }
        lambda = v.iter().zip(w.iter()).map(|(a, b)| a * b).sum::<f64>();
        let wnorm = w.iter().map(|x| x * x).sum::<f64>().sqrt();
        if wnorm == 0.0 {
            return 0.0;
        }
        for (vi, wi) in v.iter_mut().zip(w.iter()) {
            *vi = wi / wnorm;
        }
    }
    lambda.max(0.0)
}

/// A small undirected graph over section nodes, stored as an edge list.
pub struct SheafGraph {
    n: usize,
    edges: Vec<(usize, usize)>,
    /// λmax(L), estimated once at construction by power iteration
    /// ([`SPECTRAL_ITERATIONS`] rounds) — the D10 validation bound
    /// must not be recomputed per step.
    lambda_max: f64,
}

impl SheafGraph {
    /// Graph from an explicit edge list. Duplicate edges are kept and
    /// count with multiplicity (a multigraph Laplacian — `energy` and
    /// `laplacian_apply` stay mutually consistent either way).
    ///
    /// # Panics
    /// Panics if any edge has an endpoint `>= n` or is a self-loop.
    /// Both are construction-time contract violations: the indexing in
    /// `energy`/`laplacian_apply` is provably in-range precisely
    /// because this constructor rejects such edges once, up front.
    pub fn from_edges(n: usize, edges: Vec<(usize, usize)>) -> Self {
        for &(i, j) in &edges {
            assert!(i < n && j < n, "edge ({i},{j}) out of range for n={n}");
            assert!(i != j, "self-loop ({i},{i}) is meaningless here");
        }
        let lambda_max = power_lambda_max(n, &edges);
        Self {
            n,
            edges,
            lambda_max,
        }
    }

    /// Ring graph: 0—1—…—(n-1)—0. The neighbour structure of the
    /// substrate ring, and of a vector whose ends wrap.
    pub fn ring(n: usize) -> Self {
        assert!(n >= 3, "a ring needs at least 3 nodes");
        Self::from_edges(n, (0..n).map(|i| (i, (i + 1) % n)).collect())
    }

    /// Line graph: 0—1—…—(n-1). Neighbours in a plain vector.
    pub fn line(n: usize) -> Self {
        assert!(n >= 2, "a line needs at least 2 nodes");
        Self::from_edges(n, (0..n - 1).map(|i| (i, i + 1)).collect())
    }

    /// Number of section nodes.
    pub fn n(&self) -> usize {
        self.n
    }

    /// The edge list, as given at construction.
    pub fn edges(&self) -> &[(usize, usize)] {
        &self.edges
    }

    /// DESIGN §4's `spectral_bound(graph)`: the power-iteration
    /// estimate of `λmax(L)`, computed once at construction with
    /// [`SPECTRAL_ITERATIONS`] iterations. A Rayleigh quotient, so
    /// it converges to λmax *from below* (see the module docs for
    /// the near-degenerate-spectrum caveat).
    pub fn spectral_bound(&self) -> f64 {
        self.lambda_max
    }

    /// The largest admissible step size, `2 / λmax(L)` — the top of
    /// the validated range `0 < α ≤ 2 / λmax`. Infinite for an
    /// edgeless graph (L = 0: the step is the identity at any α).
    pub fn alpha_bound(&self) -> f64 {
        if self.lambda_max > 0.0 {
            2.0 / self.lambda_max
        } else {
            f64::INFINITY
        }
    }

    /// The D10 construction check: is `alpha` inside the validated
    /// stability range `0 < α ≤ 2 / λmax(L)`?
    ///
    /// A non-finite `alpha` is [`SheafError::NonFinite`] (it is a
    /// finiteness violation before it is a range violation); a
    /// finite α outside the range is
    /// [`SheafError::AlphaOutOfRange`]. An out-of-range α is an
    /// error, never a clamp (DESIGN §4).
    pub fn validate_alpha(&self, alpha: f64) -> Result<(), SheafError> {
        if !alpha.is_finite() {
            return Err(SheafError::NonFinite);
        }
        let bound = self.alpha_bound();
        if alpha <= 0.0 || alpha > bound {
            return Err(SheafError::AlphaOutOfRange { alpha, bound });
        }
        Ok(())
    }

    /// Consistency energy `E(s) = s^T L s = Σ_edges (s_i − s_j)²`.
    /// Zero iff `s` is constant on each connected component.
    ///
    /// # Panics
    /// Panics if `s.len() != self.n()` — a section of the wrong width
    /// is a caller bug, and one clear assert beats an index panic
    /// from inside the edge loop.
    pub fn energy(&self, s: &[f64]) -> f64 {
        assert_eq!(s.len(), self.n);
        self.edges
            .iter()
            .map(|&(i, j)| {
                let d = s[i] - s[j];
                d * d
            })
            .sum()
    }

    /// Apply the Laplacian into a caller-owned buffer:
    /// `(L s)_i = deg(i)·s_i − Σ_{j~i} s_j`, written to `out`.
    ///
    /// Allocation-free: zeroes `out` and accumulates in place.
    /// The accumulation order and per-edge expressions are exactly
    /// those of the original allocating implementation, so the
    /// result is bit-identical to [`Self::laplacian_apply`]
    /// (pinned by test). `out` must not alias `s`.
    ///
    /// # Panics
    /// Panics if `s.len() != self.n()` or `out.len() != self.n()`,
    /// same contract as [`Self::energy`].
    pub fn laplacian_apply_into(&self, s: &[f64], out: &mut [f64]) {
        assert_eq!(s.len(), self.n);
        assert_eq!(out.len(), self.n);
        out.fill(0.0);
        for &(i, j) in &self.edges {
            out[i] += s[i] - s[j];
            out[j] += s[j] - s[i];
        }
    }

    /// Apply the Laplacian: `(L s)_i = deg(i)·s_i − Σ_{j~i} s_j`.
    ///
    /// Allocating convenience wrapper over
    /// [`Self::laplacian_apply_into`]; identical results.
    ///
    /// # Panics
    /// Panics if `s.len() != self.n()`, same contract as
    /// [`Self::energy`].
    pub fn laplacian_apply(&self, s: &[f64]) -> Vec<f64> {
        let mut out = vec![0.0f64; self.n];
        self.laplacian_apply_into(s, &mut out);
        out
    }

    /// The step itself, with all guards already passed: energy
    /// before, apply, update — the exact arithmetic the pre-D9/D10
    /// build shipped, unchanged.
    fn step_unchecked(&self, s: &mut [f64], alpha: f64, scratch: &mut [f64]) -> f64 {
        let before = self.energy(s);
        self.laplacian_apply_into(s, scratch);
        for (v, g) in s.iter_mut().zip(scratch.iter()) {
            *v -= alpha * g;
        }
        before
    }

    /// The checked step (INV-S4 + the D10 α range), with
    /// caller-owned scratch for `L s`. Returns the energy *before*
    /// the step.
    ///
    /// Refuses — leaving `s` bit-for-bit unchanged — with
    /// [`SheafError::NonFinite`] if any element of `s` or `alpha`
    /// itself is NaN or ±∞, and with
    /// [`SheafError::AlphaOutOfRange`] if `alpha` is outside
    /// `0 < α ≤ 2 / λmax(L)`. The checks run before any mutation:
    /// one O(n) finiteness scan and an O(1) α comparison, dominated
    /// by the O(E) energy + apply the step itself performs.
    ///
    /// # Panics
    /// Panics if `s.len() != self.n()` or `scratch.len() != self.n()`
    /// — a wrong-width section is a caller bug, not data, same
    /// contract as [`Self::energy`].
    pub fn try_smooth_step_into(
        &self,
        s: &mut [f64],
        alpha: f64,
        scratch: &mut [f64],
    ) -> Result<f64, SheafError> {
        assert_eq!(s.len(), self.n);
        assert_eq!(scratch.len(), self.n);
        self.validate_alpha(alpha)?;
        if s.iter().any(|v| !v.is_finite()) {
            return Err(SheafError::NonFinite);
        }
        Ok(self.step_unchecked(s, alpha, scratch))
    }

    /// [`Self::try_smooth_step_into`] with an allocated scratch:
    /// the checked step for callers that do not keep a buffer.
    ///
    /// # Panics
    /// Panics if `s.len() != self.n()`, same contract as
    /// [`Self::try_smooth_step_into`].
    pub fn try_smooth_step(&self, s: &mut [f64], alpha: f64) -> Result<f64, SheafError> {
        let mut scratch = vec![0.0f64; self.n];
        self.try_smooth_step_into(s, alpha, &mut scratch)
    }

    /// One gradient/projection step `s <- s − α·L s`.
    /// Returns the energy *before* the step.
    ///
    /// This is the checked step ([`Self::try_smooth_step`]) with the
    /// refusal surfaced as a panic, so the original signature keeps
    /// working for existing callers. For any valid α and a finite
    /// section the arithmetic is exactly what the pre-validation
    /// build computed; `alpha <= 1 / (2 · max_degree)` remains a
    /// safe sufficient choice for any graph.
    ///
    /// # Panics
    /// Panics if `s.len() != self.n()`, if the section or `alpha`
    /// is non-finite, or if `alpha` is outside the validated range
    /// (the [`SheafError`] is named in the panic message).
    pub fn smooth_step(&self, s: &mut [f64], alpha: f64) -> f64 {
        let mut scratch = vec![0.0f64; self.n];
        self.smooth_step_into(s, alpha, &mut scratch)
    }

    /// [`Self::smooth_step`] with caller-owned scratch for `L s`:
    /// the allocation-free refine step. `scratch.len()` must equal
    /// `self.n()`; its contents are overwritten. Bit-identical
    /// arithmetic to `smooth_step` (same apply, same update order).
    ///
    /// # Panics
    /// Panics if `s.len() != self.n()` or `scratch.len() != self.n()`,
    /// or on the same refusals as [`Self::smooth_step`].
    pub fn smooth_step_into(&self, s: &mut [f64], alpha: f64, scratch: &mut [f64]) -> f64 {
        match self.try_smooth_step_into(s, alpha, scratch) {
            Ok(before) => before,
            Err(e) => panic!("sheaf step refused: {e}"),
        }
    }

    /// `steps` smoothing steps; returns the energy trajectory,
    /// including the initial energy (length `steps + 1`).
    ///
    /// # Panics
    /// Panics if `s.len() != self.n()`, or on the same refusals as
    /// [`Self::smooth_step`] (a non-finite section or an α outside
    /// the validated range aborts the run mid-trajectory).
    pub fn smooth(&self, s: &mut [f64], alpha: f64, steps: usize) -> Vec<f64> {
        let mut traj = Vec::with_capacity(steps + 1);
        traj.push(self.energy(s));
        for _ in 0..steps {
            self.smooth_step(s, alpha);
            traj.push(self.energy(s));
        }
        traj
    }
}

/// A graph + step size with the D10 validation paid once, at
/// construction: [`Smoother::new`] rejects an out-of-range α with a
/// typed error instead of clamping it (DESIGN §4), so the per-step
/// path never re-litigates the step size. The per-step INV-S4
/// finiteness scan still runs on every step — the *data* changes
/// between steps even though the step size does not.
#[derive(Clone)]
pub struct Smoother<'g> {
    graph: &'g SheafGraph,
    alpha: f64,
}

impl<'g> Smoother<'g> {
    /// Validate `alpha` against `graph`'s range
    /// (`0 < α ≤ 2 / λmax`) and bind the pair.
    ///
    /// # Errors
    /// [`SheafError::NonFinite`] for a non-finite α,
    /// [`SheafError::AlphaOutOfRange`] for a finite α outside the
    /// range.
    pub fn new(graph: &'g SheafGraph, alpha: f64) -> Result<Self, SheafError> {
        graph.validate_alpha(alpha)?;
        Ok(Self { graph, alpha })
    }

    /// The validated step size.
    pub fn alpha(&self) -> f64 {
        self.alpha
    }

    /// One checked step at the construction-validated α, with
    /// caller-owned scratch. Returns the energy *before* the step.
    ///
    /// # Errors
    /// [`SheafError::NonFinite`] if any element of `s` is NaN or
    /// ±∞; the section is then left unchanged.
    ///
    /// # Panics
    /// Panics if `s.len() != graph.n()` or `scratch.len()` differs,
    /// same contract as [`SheafGraph::try_smooth_step_into`].
    pub fn step_into(&self, s: &mut [f64], scratch: &mut [f64]) -> Result<f64, SheafError> {
        self.graph.try_smooth_step_into(s, self.alpha, scratch)
    }

    /// [`Self::step_into`] with an allocated scratch.
    ///
    /// # Panics
    /// Panics if `s.len() != graph.n()`.
    pub fn step(&self, s: &mut [f64]) -> Result<f64, SheafError> {
        self.graph.try_smooth_step(s, self.alpha)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consistent_input_has_zero_energy_and_is_fixed_point() {
        let g = SheafGraph::ring(8);
        let mut s = vec![3.25f64; 8];
        assert_eq!(g.energy(&s), 0.0);
        g.smooth_step(&mut s, 0.1);
        assert!(
            s.iter().all(|&v| v == 3.25),
            "consistent state must not be mangled"
        );
    }

    #[test]
    fn clamp_counterexample_is_caught() {
        // The email's clamp would leave this untouched (|v| <= 10)
        // and mangle [100,100,100,...]. Energy does the opposite,
        // correctly on both:
        let g = SheafGraph::line(4);
        assert!(g.energy(&[9.0, -9.0, 9.0, -9.0]) > 0.0);
        assert_eq!(g.energy(&[100.0, 100.0, 100.0, 100.0]), 0.0);
    }

    #[test]
    fn energy_strictly_decreases_on_inconsistent_input() {
        let g = SheafGraph::ring(16);
        let mut s: Vec<f64> = (0..16).map(|i| ((i * 7) % 5) as f64 - 2.0).collect();
        let traj = g.smooth(&mut s, 0.1, 20);
        for w in traj.windows(2) {
            assert!(
                w[1] < w[0],
                "energy must strictly decrease: {} -> {}",
                w[0],
                w[1]
            );
        }
        assert!(
            traj[20] < traj[0] * 0.01,
            "should be near-consistent after 20 steps"
        );
    }

    #[test]
    fn smoothing_preserves_mean_on_ring() {
        // L's null space is the constants and 1^T L = 0, so the mean
        // is an invariant of the step — consistency is reached by
        // agreement, not by shrinking everything toward zero.
        let g = SheafGraph::ring(8);
        let mut s: Vec<f64> = (0..8).map(|i| i as f64).collect();
        let mean0: f64 = s.iter().sum::<f64>() / 8.0;
        g.smooth(&mut s, 0.1, 200);
        let mean1: f64 = s.iter().sum::<f64>() / 8.0;
        assert!((mean0 - mean1).abs() < 1e-9);
        assert!(g.energy(&s) < 1e-6);
    }

    #[test]
    fn laplacian_of_constant_is_zero() {
        let g = SheafGraph::line(5);
        assert!(g.laplacian_apply(&[1.0; 5]).iter().all(|&v| v == 0.0));
    }

    /// Minimal seeded LCG (std-only) for the parity test below.
    struct Lcg(u64);
    impl Lcg {
        fn next_f64(&mut self) -> f64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let m = (self.0 >> 11) as f64 / (1u64 << 53) as f64;
            m * 20.0 - 10.0
        }
        fn coin(&mut self, p: f64) -> bool {
            self.next_f64() + 10.0 < p * 20.0
        }
    }

    #[test]
    fn into_path_is_bitwise_identical_to_legacy() {
        // The D11 fix must not change a single bit: same edge order,
        // same per-edge expressions. Compare bit patterns, not
        // tolerances, across seeded random graphs (incl. multigraph
        // duplicates and empty edge sets).
        for seed in 1..=60u64 {
            let mut rng = Lcg(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let n = 2 + (seed as usize % 29);
            let mut edges = Vec::new();
            for i in 0..n {
                for j in (i + 1)..n {
                    if rng.coin(0.25) {
                        edges.push((i, j));
                        if rng.coin(0.2) {
                            edges.push((i, j)); // duplicate: multiplicity path
                        }
                    }
                }
            }
            let g = SheafGraph::from_edges(n, edges);
            let s: Vec<f64> = (0..n).map(|_| rng.next_f64()).collect();
            let legacy = g.laplacian_apply(&s);
            let mut into = vec![f64::NAN; n]; // poisoned: must be fully overwritten
            g.laplacian_apply_into(&s, &mut into);
            for i in 0..n {
                assert_eq!(
                    legacy[i].to_bits(),
                    into[i].to_bits(),
                    "seed {seed} node {i}: legacy {} vs into {}",
                    legacy[i],
                    into[i]
                );
            }
            // smooth_step vs smooth_step_into: identical trajectory.
            let mut a = s.clone();
            let mut b = s.clone();
            let mut scratch = vec![0.0f64; n];
            for _ in 0..10 {
                let ea = g.smooth_step(&mut a, 0.05);
                let eb = g.smooth_step_into(&mut b, 0.05, &mut scratch);
                assert_eq!(ea.to_bits(), eb.to_bits(), "seed {seed}: step energy bits");
            }
            for i in 0..n {
                assert_eq!(a[i].to_bits(), b[i].to_bits(), "seed {seed}: smoothed bits");
            }
        }
    }

    #[test]
    fn line_graph_endpoints_have_degree_one() {
        let g = SheafGraph::line(3);
        // s = [1,0,0]: (Ls)_0 = 1*(1-0) = 1, (Ls)_1 = (0-1)+(0-0) = -1
        assert_eq!(g.laplacian_apply(&[1.0, 0.0, 0.0]), vec![1.0, -1.0, 0.0]);
    }

    // ------------------------------------------------------ D9 (INV-S4)
    // Before the fix, a probe recorded: energy(NaN) = NaN (correct),
    // but the step ran anyway and the NaN SPREAD to neighbours;
    // α = NaN returned a finite-looking energy while poisoning the
    // section; +∞ in the section manufactured NaNs. Every behaviour
    // below would fail against that build.

    #[test]
    fn d9_energy_of_nan_section_is_nan() {
        // The diagnostic half of INV-S4: energy must say NaN, plainly.
        let g = SheafGraph::ring(8);
        let mut s = vec![0.0f64; 8];
        s[3] = f64::NAN;
        assert!(g.energy(&s).is_nan());
    }

    #[test]
    fn d9_nan_section_is_refused_and_left_bitwise_unchanged() {
        let g = SheafGraph::ring(8);
        let mut s = vec![0.0f64; 8];
        s[3] = f64::NAN;
        let before: Vec<u64> = s.iter().map(|v| v.to_bits()).collect();
        let mut scratch = vec![0.0f64; 8];
        let err = g
            .try_smooth_step_into(&mut s, 0.1, &mut scratch)
            .unwrap_err();
        assert_eq!(err, SheafError::NonFinite);
        let after: Vec<u64> = s.iter().map(|v| v.to_bits()).collect();
        assert_eq!(before, after, "a refused step must not touch the section");
        // The pre-fix build spread the NaN to nodes 2 and 4.
        assert!(s.iter().filter(|v| v.is_nan()).count() == 1);
    }

    #[test]
    fn d9_inf_section_and_nonfinite_alpha_are_refused() {
        let g = SheafGraph::ring(8);
        let clean: Vec<f64> = (0..8).map(|i| i as f64).collect();
        let mut s = clean.clone();
        s[0] = f64::INFINITY;
        assert_eq!(g.try_smooth_step(&mut s, 0.1), Err(SheafError::NonFinite));
        assert_eq!(s[1], 1.0, "no partial step may leak through");
        for bad_alpha in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let mut s = clean.clone();
            assert_eq!(
                g.try_smooth_step(&mut s, bad_alpha),
                Err(SheafError::NonFinite),
                "alpha {bad_alpha} is a finiteness violation first"
            );
            assert_eq!(s, clean);
        }
    }

    #[test]
    #[should_panic(expected = "sheaf step refused")]
    fn d9_legacy_signature_refuses_as_a_panic() {
        // harbor-watch / slice1 / the pipeline call smooth_step; the
        // refusal reaches them as a loud panic naming the error, not
        // as silent NaN propagation.
        let g = SheafGraph::ring(8);
        let mut s = vec![0.0f64; 8];
        s[5] = f64::NAN;
        g.smooth_step(&mut s, 0.1);
    }

    #[test]
    fn d9_smoother_refuses_a_section_poisoned_mid_run() {
        let g = SheafGraph::ring(8);
        let sm = Smoother::new(&g, 0.1).unwrap();
        let mut s: Vec<f64> = (0..8).map(|i| ((i * 7) % 5) as f64 - 2.0).collect();
        assert!(sm.step(&mut s).is_ok(), "clean section steps fine");
        s[2] = f64::NAN;
        let snapshot: Vec<u64> = s.iter().map(|v| v.to_bits()).collect();
        assert_eq!(sm.step(&mut s), Err(SheafError::NonFinite));
        let after: Vec<u64> = s.iter().map(|v| v.to_bits()).collect();
        assert_eq!(after, snapshot, "refusal leaves even the NaN where it was");
    }

    // ------------------------------------------------------ D10 (α range)

    #[test]
    fn d10_spectral_bound_matches_known_spectra() {
        // Ring C8: λmax = 4 exactly. Line P4: λmax = 2 + √2.
        let ring8 = SheafGraph::ring(8);
        assert!(
            (ring8.spectral_bound() - 4.0).abs() < 1e-9,
            "ring(8) spectral bound {}",
            ring8.spectral_bound()
        );
        assert!((ring8.alpha_bound() - 0.5).abs() < 1e-9);
        let line4 = SheafGraph::line(4);
        let lmax = 2.0 + std::f64::consts::SQRT_2;
        assert!(
            (line4.spectral_bound() - lmax).abs() < 1e-9,
            "line(4) spectral bound {} vs {lmax}",
            line4.spectral_bound()
        );
        // Converges from below, never above the true value.
        assert!(line4.spectral_bound() <= lmax);
        // Edgeless graph: L = 0, bound infinite, any finite α > 0 ok.
        let bare = SheafGraph::from_edges(4, vec![]);
        assert_eq!(bare.spectral_bound(), 0.0);
        assert_eq!(bare.alpha_bound(), f64::INFINITY);
        assert!(bare.validate_alpha(123.0).is_ok());
    }

    #[test]
    fn d10_alpha_boundaries_are_pinned() {
        // ring(8): bound = 2/λmax = 0.5 (to <1e-9, see above).
        let g = SheafGraph::ring(8);
        let bound = g.alpha_bound();
        // Exactly at the bound: admitted (DESIGN: 0 < α ≤ 2/λmax).
        assert!(g.validate_alpha(bound).is_ok(), "α == bound must pass");
        assert!(Smoother::new(&g, bound).is_ok());
        // Just inside.
        assert!(g.validate_alpha(bound * (1.0 - 1e-9)).is_ok());
        // Just outside: rejected, with the bound in the error.
        match g.validate_alpha(bound * (1.0 + 1e-9)) {
            Err(SheafError::AlphaOutOfRange { alpha, bound: b }) => {
                assert_eq!(alpha, bound * (1.0 + 1e-9));
                assert_eq!(b, bound);
            }
            other => panic!("expected AlphaOutOfRange, got {other:?}"),
        }
        assert!(matches!(
            Smoother::new(&g, bound * (1.0 + 1e-9)),
            Err(SheafError::AlphaOutOfRange { .. })
        ));
        // Zero and negative: rejected (the old build silently no-opped
        // at α = 0 and anti-smoothed at α < 0).
        assert!(matches!(
            g.validate_alpha(0.0),
            Err(SheafError::AlphaOutOfRange { .. })
        ));
        assert!(matches!(
            g.validate_alpha(-0.5),
            Err(SheafError::AlphaOutOfRange { .. })
        ));
        // The probe's divergence case: α = 10 took energy 54 -> 53214.
        let mut s: Vec<f64> = (0..8).map(|i| ((i * 7) % 5) as f64 - 2.0).collect();
        assert!(matches!(
            g.try_smooth_step(&mut s, 10.0),
            Err(SheafError::AlphaOutOfRange { .. })
        ));
        assert_eq!(g.energy(&s), 54.0, "refused step leaves energy untouched");
    }

    #[test]
    fn d10_step_at_exactly_the_bound_does_not_increase_energy() {
        // α = 2/λmax is the admitted edge: the top mode oscillates
        // without decay, every mode's energy multiplier is ≤ 1.
        let g = SheafGraph::ring(8);
        let mut s: Vec<f64> = (0..8).map(|i| ((i * 7) % 5) as f64 - 2.0).collect();
        let sm = Smoother::new(&g, g.alpha_bound()).unwrap();
        for _ in 0..16 {
            let before = sm.step(&mut s).unwrap();
            let after = g.energy(&s);
            assert!(
                after <= before + 1e-9,
                "energy at the bound must not increase: {before} -> {after}"
            );
        }
    }
}
