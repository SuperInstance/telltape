//! Synthetic fixture in the exact driver schema, with ground truth.
//!
//! The scene script (frames are 4x4 tile grids, tile = row*4 + col):
//!
//! | frames | what happens |
//! |---|---|
//! | 0–3 | Scene A, stable. Each tile's embedding drifts slowly (per-step cosine distance ≈ 0.006). |
//! | 4 | **CUT**: tiles {3, 7, 11} (the right-hand column, rows 0–2) jump to unrelated scene-B embeddings (distance ≈ 1.0). These are the only ground-truth *changed* tiles in the fixture. |
//! | 5–7 | Scene B, stable, slow drift. At frame 6 tile 9 gets a one-frame noise **spike** (distance ≈ 0.35, returns at frame 7): ground truth says NOT changed. Because decide compares against the previous *published* embedding, the spike produces TWO raw change events — out at frame 6 and back at frame 7 — i.e. 2 raw false positives that spatial smoothing should rescue both of. |
//! | 8–11 | Tile 5 **drifts gradually**: per-step distance ≈ 0.03 (below threshold) sustained for 4 frames. Cumulatively large, per-step small: ground truth says NOT a cut, and a per-frame change detector must not flag it. |
//!
//! Arrival times model a staggered driver: frame period 100 ms, tiles
//! arriving 400 µs apart in tile order. `diff_score` is filled with the
//! cheap frame-diff the driver would report — roughly tracking the
//! embedding change for cuts, noisy for the spike — but the pipeline
//! never uses it to decide; it is carried for context only.

use crate::record::{InputRecord, TILES};
use crate::rng::{perturb, sigma_for_distance, Rng};
use std::collections::BTreeMap;

pub const CUT_FRAME: u32 = 4;
pub const CUT_TILES: [u32; 3] = [3, 7, 11];
pub const SPIKE_FRAME: u32 = 6;
pub const SPIKE_TILE: u32 = 9;
pub const DRIFT_TILE: u32 = 5;
pub const FRAMES: u32 = 12;

pub struct Fixture {
    pub records: Vec<InputRecord>,
    /// (frame, tile) -> is a true change (cut). Drift and the noise
    /// spike are deliberately `false`/absent: absence == not changed.
    pub truth: BTreeMap<(u32, u32), bool>,
    pub dim: usize,
}

impl Fixture {
    pub fn truth_changed(&self, frame: u32, tile: u32) -> bool {
        self.truth.get(&(frame, tile)).copied().unwrap_or(false)
    }
}

pub fn generate(dim: usize, seed: u64) -> Fixture {
    let mut rng = Rng::new(seed);
    let slow = sigma_for_distance(dim, 0.006);
    let drift = sigma_for_distance(dim, 0.03);
    let spike = sigma_for_distance(dim, 0.35);

    // Current embedding per tile.
    let mut cur: Vec<Vec<f32>> = (0..TILES).map(|_| rng.unit_vec(dim)).collect();
    let mut records = Vec::new();
    let mut truth = BTreeMap::new();
    let mut seq: u64 = 0;

    for frame in 0..FRAMES {
        for tile in 0..TILES as u32 {
            let t = tile as usize;
            if frame == CUT_FRAME && CUT_TILES.contains(&tile) {
                cur[t] = rng.unit_vec(dim); // unrelated scene-B content
                truth.insert((frame, tile), true);
            } else if frame == SPIKE_FRAME && tile == SPIKE_TILE {
                // One-frame excursion; cur[] is NOT updated, so the tile
                // returns to its drifted position next frame.
                let spiked = perturb(&mut rng, &cur[t], spike);
                records.push(rec(seq, frame, tile, 0.09, spiked));
                seq += 1;
                continue;
            } else if frame >= 8 && tile == DRIFT_TILE {
                cur[t] = perturb(&mut rng, &cur[t], drift);
            } else if frame > 0 {
                cur[t] = perturb(&mut rng, &cur[t], slow);
            }
            // Cheap-diff proxy: cuts read high, spike medium, rest low.
            let diff = if frame == CUT_FRAME && CUT_TILES.contains(&tile) {
                0.42
            } else {
                0.004
            };
            records.push(rec(seq, frame, tile, diff, cur[t].clone()));
            seq += 1;
        }
    }
    Fixture {
        records,
        truth,
        dim,
    }
}

fn rec(seq: u64, frame: u32, tile: u32, diff: f64, embedding: Vec<f32>) -> InputRecord {
    InputRecord {
        seq,
        frame,
        tile,
        // 100 ms frame period, 400 µs stagger across tiles.
        t_arrival_us: frame as u64 * 100_000 + tile as u64 * 400,
        diff_score: diff,
        embedding,
    }
}
