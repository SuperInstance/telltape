//! # harbor-watch
//!
//! Rolling visual change detection over real I-JEPA embeddings
//! (wave 3 of SI-Thor/rolling-perception: the first *real workload*
//! through the pipeline, after the v0.1/v0.2 machinery proofs).
//!
//! A driver on the Thor embeds webcam frames tile-by-tile (4×4 grid)
//! with I-JEPA and streams records
//! `{seq, frame, tile, t_arrival_us, diff_score, embedding[1280]}`.
//! This crate runs each embedding — at full 1280 dimensions, carried
//! on `rolling-substrate-core` rings — through:
//!
//! perceive → decide (cosine distance vs the tile's previous published
//! embedding) → direct (top-k most-changed tiles = the frame's focus)
//! → refine (ONE Laplacian step over the 4×4 tile grid) → publish
//! (changed/stable verdicts).
//!
//! Two honest questions are built in as measurements, not assumptions:
//!
//! * Does the refine step help? [`ablation`] scores raw vs smoothed
//!   detection against ground truth (precision/recall, both reported).
//! * Does embedding only changed tiles (the driver's CHANGED strategy)
//!   reach a change verdict sooner than embedding everything (ALL),
//!   and do the two strategies agree? [`ablation::compare`].
//!
//! The synthetic fixture in [`fixture`] has known ground truth (a cut,
//! a one-frame noise spike, sustained sub-threshold drift) so the
//! detector's behaviour is testable before — and independently of —
//! the real driver file.

pub mod ablation;
pub mod fixture;
pub mod pipeline;
pub mod record;
pub mod rng;
