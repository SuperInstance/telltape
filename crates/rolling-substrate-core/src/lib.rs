//! # rolling-substrate-core
//!
//! An honest, dependency-free (std-only) implementation of the parts of
//! Casey's "Rolling perception" email (2026-11-01) that survive contact
//! with reality.
//!
//! ## What the email claimed, and what this crate does
//!
//! | Email claim | This crate |
//! |---|---|
//! | Pillar 1, SubstrateCore: lock-free 64-byte-aligned ring in unified memory, sheaf-Laplacian enforcement | [`substrate`]: real Disruptor-style sequence ring, `#[repr(align(64))]`, multi-producer atomic claim. [`sheaf`]: a *real* Laplacian smoothing step, not the email code's clamp (`if \|v\|>10 { v *= 0.333 }`). |
//! | Pillar 4 / PIR: lineage by multiplying primes, checked with `history % p == 0` | [`lineage`]: **replaced**. Prime-product overflows `u64`, loses counts and order, and gives false positives after wrap. Replaced by a visited bitmask + per-node counts. The flawed scheme is kept in [`lineage::pir_legacy`] only as a documented counterexample. |
//! | Pillar 3, FluidAttn: branchless viewport gating skipping out-of-ROI work | [`gating`]: ROI check + gated apply that demonstrably skips work and is bit-identical inside the ROI. "Branchless" is described honestly: the source is branch-free arithmetic, whether the compiler emits branches is its business. |
//! | Pillar 2, CascadeJIT / CudaClaw, NVRTC, CUPTI, persistent GPU warps | **Not implemented, by design.** That belongs to a CUDA/Thor lane; the email's Rust for it was sleeps and mock cubin bytes. Nothing here pretends otherwise. |
//! | SkaldAudio DMA PCM intercept | **Not implemented.** No DMA intercept exists in safe userspace Rust on this host; claiming one would be the same dishonesty as the original. The substrate ring is the part an audio lane would write into. |
//!
//! No external crates. No GPU. No mocked latency: there is no
//! `thread::sleep` standing in for compute anywhere in this crate.

#![warn(missing_docs)]

pub mod gating;
pub mod lineage;
pub mod sheaf;
pub mod spsc;
pub mod substrate;
