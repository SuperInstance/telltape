//! Hand-rolled bench (std::time only) for the sheaf D11 fix:
//! `laplacian_apply` (allocating) vs `laplacian_apply_into`
//! (caller buffer), plus the refine loop's total effect.
//!
//! Run: `cargo run --release -p rolling-substrate-core --example sheaf_bench`
//!
//! Measured numbers live in the run output, not in this file —
//! run it on the machine you care about (dev VM numbers are not
//! Thor numbers).

use std::time::Instant;

use rolling_substrate_core::sheaf::SheafGraph;

fn grid(side: usize) -> SheafGraph {
    let mut edges = Vec::new();
    for r in 0..side {
        for c in 0..side {
            let i = r * side + c;
            if c + 1 < side {
                edges.push((i, i + 1));
            }
            if r + 1 < side {
                edges.push((i, i + side));
            }
        }
    }
    SheafGraph::from_edges(side * side, edges)
}

fn bench_apply(name: &str, g: &SheafGraph, iters: usize) {
    let s: Vec<f64> = (0..g.n()).map(|i| ((i * 7) % 13) as f64 - 6.0).collect();
    let mut out = vec![0.0f64; g.n()];
    // Warm-up.
    for _ in 0..100 {
        g.laplacian_apply_into(&s, &mut out);
        std::hint::black_box(g.laplacian_apply(&s));
    }
    let t = Instant::now();
    for _ in 0..iters {
        std::hint::black_box(g.laplacian_apply(&s));
    }
    let legacy = t.elapsed();
    let t = Instant::now();
    for _ in 0..iters {
        g.laplacian_apply_into(&s, &mut out);
        std::hint::black_box(&out);
    }
    let into = t.elapsed();
    println!(
        "{name}: n={} edges={} iters={} | legacy {:>9.1} ns/call | into {:>9.1} ns/call | speedup {:.2}x",
        g.n(),
        g.edges().len(),
        iters,
        legacy.as_nanos() as f64 / iters as f64,
        into.as_nanos() as f64 / iters as f64,
        legacy.as_nanos() as f64 / into.as_nanos() as f64,
    );
}

fn bench_refine(name: &str, g: &SheafGraph, steps: usize) {
    let mk = || -> Vec<f64> { (0..g.n()).map(|i| ((i * 5) % 9) as f64).collect() };
    let t = Instant::now();
    let mut s = mk();
    for _ in 0..steps {
        g.smooth_step(&mut s, 0.05);
    }
    let legacy = t.elapsed();
    let e_legacy = g.energy(&s);
    let t = Instant::now();
    let mut s2 = mk();
    let mut scratch = vec![0.0f64; g.n()];
    for _ in 0..steps {
        g.smooth_step_into(&mut s2, 0.05, &mut scratch);
    }
    let into = t.elapsed();
    assert_eq!(e_legacy.to_bits(), g.energy(&s2).to_bits());
    println!(
        "{name}: refine {} steps | legacy {:?} ({} allocs) | into {:?} (0 allocs) | final energy bits identical",
        steps,
        legacy,
        steps,
        into
    );
}

fn main() {
    // Harbor-watch scale: 16 tiles as a 4x4 grid.
    bench_apply("apply 4x4 grid  ", &grid(4), 200_000);
    // Larger synthetic graph: 1,024 nodes as a 32x32 grid.
    bench_apply("apply 32x32 grid", &grid(32), 20_000);
    bench_refine("refine 4x4  ", &grid(4), 10_000);
    bench_refine("refine 32x32", &grid(32), 1_000);
}
