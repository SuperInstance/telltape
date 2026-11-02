# rolling-substrate-core

Honest, dependency-free (std-only) Rust for the salvageable parts of the
"Rolling perception" email: `substrate` (Disruptor-style 64-byte-aligned
ring), `lineage` (bitmask + counts replacing PIR prime-product, with the
PIR failure demonstrated in a test), `sheaf` (real graph-Laplacian
smoothing: energy `sᵀLs`, gradient step, energy strictly decreases),
`gating` (ROI/FluidAttn with exact skip counts).

Not implemented, deliberately: CascadeJIT/CudaClaw/NVRTC/CUPTI and
SkaldAudio DMA — those need a CUDA/Thor lane and the email's versions
were mocks. See `src/lib.rs` for the claim-by-claim table.

## Run it

From **this crate's directory** (`crates/rolling-substrate-core/`):

```sh
cargo test   # 32 tests, this crate only (27 unit + 5 randomised property
             # tests in tests/properties.rs)
             # (from the repository root, cargo test runs the whole
             # workspace suite — 75 tests at the time of the 2026-11-01
             # hardening pass; see HARDENING.md)
```

On the Thor over SSH, first:
`export PATH=$HOME/.cargo/bin:/usr/local/cuda/bin:$PATH`
(cargo is not on the default PATH there).

There is no binary in this crate — it is a library. The tests are the
demonstration: ring claim/publish across laps, multi-producer claim
uniqueness, Laplacian energy strictly decreasing on inconsistent
input, the PIR wrap false-positive shown as a counterexample, and
gating skip counts checked exactly.

## 2026-11-01 (night) — SHEAF-FIX: the allocation lie is closed

DESIGN §4 claims the sheaf step allocates nothing; the engine book
(ARCHITECTURE D11) recorded that as contradicted — `laplacian_apply`
allocated a fresh `Vec` per call. Now: `laplacian_apply_into` and
`smooth_step_into` write into caller-owned buffers with the identical
accumulation order (output **bit-identical** to the legacy path,
pinned by a seeded random-graph parity test); the legacy signatures
are unchanged and delegate. `tests/sheaf_alloc.rs` (counting
allocator, thread-local) asserts **0 allocations** for the into-path
in steady state vs exactly 1 per call legacy. Bench
(`examples/sheaf_bench.rs`, dev VM, release): apply on the 16-node
harbor-scale grid 99.5 → 54.9 ns/call (1.81×); on a 1,024-node grid
4,793 → 4,909 ns/call (≈1.0× — at that size the edge work dominates
and the allocation was never the cost; the win there is the refine
loop's allocator traffic, 1,000 allocs → 0 over 1,000 steps,
7.57 → 6.85 ms). Tests: substrate-core 32 → 36, workspace 75 → 79.
