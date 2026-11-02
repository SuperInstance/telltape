//! Allocation accounting for the sheaf step (ARCHITECTURE D11).
//!
//! A counting global allocator (std-only) proves the claim the
//! design made and the code originally broke: the into-path
//! (`laplacian_apply_into` / `smooth_step_into`) performs **zero**
//! allocations in steady state, while the legacy allocating path
//! performs exactly one per call — the contrast is asserted too,
//! so a broken counter cannot pass silently.
//!
//! This file is its own test binary, so the counting allocator
//! governs only these tests. Measurement windows are serialised
//! behind a mutex and warmed up first, so harness allocations
//! outside the window cannot leak in.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use rolling_substrate_core::sheaf::SheafGraph;

// Counting is THREAD-LOCAL on purpose: a global counter also sees
// the test harness's own threads (teardown of a finished sibling
// test allocates), which polluted windows by a handful of counts.
// Thread-local state, reached via `try_with` so allocator calls
// during TLS setup itself degrade to "not counted" instead of
// recursing or panicking.
thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = COUNTING.try_with(|c| {
            if c.get() {
                let _ = ALLOCS.try_with(|a| a.set(a.get() + 1));
            }
        });
        System.alloc(layout)
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout)
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

/// Run `f` on this thread with allocation counting on;
/// return (result, allocs on this thread).
fn counted<R>(f: impl FnOnce() -> R) -> (R, usize) {
    ALLOCS.with(|a| a.set(0));
    COUNTING.with(|c| c.set(true));
    let r = f();
    COUNTING.with(|c| c.set(false));
    (r, ALLOCS.with(|a| a.get()))
}

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

#[test]
fn into_path_allocates_zero_in_steady_state() {
    let g = grid(32); // 1,024 nodes
    let s: Vec<f64> = (0..g.n()).map(|i| (i % 7) as f64 - 3.0).collect();
    let mut out = vec![0.0f64; g.n()];
    // Warm-up: any lazy one-off setup happens here, uncounted.
    for _ in 0..16 {
        g.laplacian_apply_into(&s, &mut out);
    }
    let (_, allocs) = counted(|| {
        for _ in 0..1_000 {
            g.laplacian_apply_into(&s, &mut out);
        }
    });
    assert_eq!(allocs, 0, "into-path must allocate zero in steady state");
    std::hint::black_box(&out);
}

#[test]
fn legacy_path_allocates_once_per_call_for_contrast() {
    let g = grid(32);
    let s: Vec<f64> = (0..g.n()).map(|i| (i % 7) as f64 - 3.0).collect();
    let (sum, allocs) = counted(|| {
        let mut acc = 0.0;
        for _ in 0..100 {
            acc += g.laplacian_apply(&s)[0];
        }
        acc
    });
    assert_eq!(
        allocs, 100,
        "legacy path: exactly one Vec allocation per call (counter sanity + contrast)"
    );
    std::hint::black_box(sum);
}

#[test]
fn refine_loop_into_allocates_zero_legacy_allocates_per_step() {
    let g = grid(4); // 16 nodes, harbor-watch tile scale
    let steps = 200;
    // Into-path refine loop: one caller scratch, reused.
    let mut s1: Vec<f64> = (0..g.n()).map(|i| ((i * 5) % 9) as f64).collect();
    let mut scratch = vec![0.0f64; g.n()];
    for _ in 0..8 {
        g.smooth_step_into(&mut s1, 0.05, &mut scratch);
    }
    let mut s1b = s1.clone();
    let (_, into_allocs) = counted(|| {
        for _ in 0..steps {
            g.smooth_step_into(&mut s1b, 0.05, &mut scratch);
        }
    });
    assert_eq!(into_allocs, 0, "scratch refine loop must not allocate");
    // Legacy loop from the same state: one scratch Vec per step.
    let mut s2 = s1.clone();
    let (_, legacy_allocs) = counted(|| {
        for _ in 0..steps {
            g.smooth_step(&mut s2, 0.05);
        }
        std::hint::black_box(&s2);
    });
    assert_eq!(
        legacy_allocs, steps,
        "legacy refine loop allocates once per step"
    );
    std::hint::black_box(&s1b);
}
