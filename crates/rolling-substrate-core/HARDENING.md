# Hardening record — rolling-substrate-core

Audit → fix → prove pass, 2026-11-01. Scope: every `unwrap`/`expect`/
`panic!`/assert, index, and arithmetic site in the library code of
this crate (tests and examples keep theirs, except where noted).
Pre-fix references are file + function + the pre-fix snippet, so each
item can be found in the pre-hardening tree.

End state: 32 tests in this crate (27 unit + 5 property in
`tests/properties.rs`), `cargo clippy --all-targets -- -D warnings`
clean, `cargo fmt` applied, `#![warn(missing_docs)]` on with zero
warnings.

## Fixed

1. **Build-profile-dependent ring protocol** — `src/substrate.rs`,
   `Lease::finalize`. Pre-fix: `.store(self.claim + self.buffer.capacity
   as u64, …)`. Plain `+` panics on overflow in debug builds and wraps
   in release, so the Disruptor sequence protocol changed behaviour
   with the build profile at the 2^64-claim era boundary. The protocol
   is inherently modulo-2^64 (capacity is a power of two, hence divides
   2^64, so slot selection and sequence equality stay consistent
   across the wrap) — fixed with `wrapping_add`, the reasoning written
   at the call site, and a boundary test that seeds the claim counter
   at `u64::MAX - 1` and leases/finalizes across the wrap
   (`claim_counter_wrap_is_modulo_2_to_the_64`).
2. **`Viewport::area` underflow** — `src/gating.rs`. Pre-fix:
   `(self.xmax - self.xmin) as u64 * …`. `Viewport`'s fields are
   public, so a literal `Viewport { xmin: 5, xmax: 2, .. }` bypasses
   `new`'s ordering assert: `area()` then underflowed (debug panic,
   release wrap to a huge area). Fixed with `saturating_sub` — an
   inverted viewport has area 0 — and pinned by
   `literal_inverted_viewport_is_total_not_a_panic` (`mask`,
   `roi_coords`, `gated_apply` were already total on such values and
   the test proves all four).
3. **Silent mis-gating on absurd grids** — `src/gating.rs`,
   `gated_apply`. The mask math casts coordinates to `u32`; a grid
   dimension above `u32::MAX` would have wrapped the cast and gated
   the wrong points *silently*. Practically unreachable (it needs a
   >4-billion-element slice), but the failure mode was wrong answers,
   not a crash. Fixed with an up-front assert, documented under
   `# Panics`.

## Fenced (kept, with the invariant written down and a test pinning it)

4. **Constructor asserts** — `FluidSubstrateBuffer::new` (power-of-two
   capacity, non-zero dimension), `SheafGraph::from_edges` (endpoints
   in range, no self-loops), `SheafGraph::ring`/`line` (minimum node
   counts), `Viewport::new` (ordered bounds). These are
   construction-time contracts on caller-chosen constants, and each
   now carries an explicit `# Panics` doc. The sheaf asserts are also
   what makes the indexing in `energy`/`laplacian_apply` provably
   in-range: edges are validated once, here, instead of per step.
5. **Section-length asserts** — `SheafGraph::energy`,
   `laplacian_apply`, `smooth_step`, `smooth` assert `s.len() == n`.
   One clear contract panic instead of an index panic from inside the
   edge loop; `# Panics` documented per method.
6. **`LineageHeader::record` / `visited` assert `node < 64`** — a
   hot-path contract (one comparison per visit). The invariant: node
   ids are topology constants in `0..MAX_NODES`. Kept as a loud panic
   because the alternative (silently dropping a node) corrupts
   lineage. Pinned from both sides: the property test exercises all
   64 ids against a naive model, and `record_at_max_nodes_panics`
   pins the fence at 64.
7. **Counts saturate at `u32::MAX`** — `LineageHeader::record`'s CAS
   loop already saturated by design (a wrapped count is a wrong
   count), but the boundary was untested. `counts_saturate_at_u32_max_
   instead_of_wrapping` seeds the counter at `u32::MAX - 1` and pins
   saturation.
8. **`is_published` is era-local** — `src/substrate.rs`. Its `>`
   comparison cannot see across the claim counter's 2^64 wrap: a
   pre-wrap claim reads as unpublished in the new era. Documented on
   the method and pinned by the wrap test in (1). At a sustained
   10^9 leases/second the boundary is ~585 years out; the honest
   fence is the documentation, not machinery.
9. **`pir_legacy::visited` stays verbatim** — the module is a faithful
   counterexample of the email's PIR query, so `state % prime == 0`
   is *not* modernised to `is_multiple_of`: the two differ at
   `prime == 0` (`is_multiple_of(0)` is defined as `state == 0`;
   `%` panics). Clippy's `manual_is_multiple_of` is allowed at that
   one site with the reason written there, and the `prime != 0`
   precondition is documented.
10. **`smooth_step`'s `alpha`** — a caller-owned step-size constant,
    not data. The stability range (`0 < alpha < 2 / λ_max`, with
    `1 / (2 · max_degree)` as a safe sufficient bound) was only in the
    module docs; it is now on the method too. The property test
    exercises exactly the guaranteed regime on random graphs.

## Provably safe (read closely, no change)

11. `lease()`'s `(claim as usize) & mask` — lossless on 64-bit; the
    mask keeps slot selection identical to `is_published` /
    `read_published` on any width.
12. Sheaf edge-loop indexing — in-range by (4) plus the length assert
    in (5).
13. `Slot` alignment/size arithmetic and the `unsafe impl Sync` —
    unchanged; the existing SAFETY comment already states the lease
    exclusivity invariant. `read_published`'s internal deref gained
    an inline `// SAFETY:` pointing at its caller contract.

## Property tests added (`tests/properties.rs`)

Std-only, seeded LCG (the workspace's 1664525/1013904223 family);
every assertion message carries its seed.

- Ring: FIFO order + no loss under interleaved producer/consumer
  schedules (≥25 laps per seed at small capacities); multi-producer
  claim uniqueness/completeness under random shapes.
- Lineage: mask/count conservation vs a naive model under random op
  streams including `clear`.
- Sheaf: `energy`/`laplacian_apply` vs a naive dense `D^T D`
  Laplacian on random graphs; energy never increases under
  `smooth_step` at `alpha = 1 / (2 · dmax)`; section sum preserved.
- Gating: exact processed/skipped counts and untouched-outside under
  random viewports (including out-of-grid and empty).
