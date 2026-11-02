//! The harbor-watch cascade, on substrate rings.
//!
//! Stages (every payload physically transits a
//! [`FluidSubstrateBuffer`]; stage lineage nodes 0–4):
//!
//! 0. **perceive** — lease an ingest-ring slot (dimension = the full
//!    embedding size, 1280 for real I-JEPA; never truncated), copy the
//!    embedding in, publish.
//! 1. **decide** — read the published slot, cosine distance vs the
//!    tile's previous *published* embedding (the lens state), write
//!    `[score, embedding…]` into the decide ring (dimension dim+1).
//! 2. **direct** — once per frame: rank tiles by raw score, mark the
//!    top-`focus_k` as the frame's focus set, write the focus mask to
//!    the focus ring. The focus set is published in the verdicts as the
//!    director's attention record. Honest scope note: in this version
//!    focus does not gate compute — refine still runs over all tiles,
//!    because the ablation must compare like with like. Gating by
//!    focus is future work, not a claim here.
//! 3. **refine** — ONE Laplacian step (`SheafGraph::smooth_step`,
//!    α = 0.1) over the frame's 16 raw change scores, on the 4×4 grid
//!    adjacency graph. Purpose, stated plainly: spatial smoothing, so
//!    a lone noisy tile is pulled toward its (stable) neighbours and a
//!    true multi-tile change — whose neighbours also moved — is not.
//!    Whether it actually helps is measured by the ablation in
//!    [`crate::ablation`], not assumed. Scores for tiles with no fresh
//!    embedding this frame enter as their last known score (staleness
//!    is labelled in the verdict, not hidden).
//! 4. **publish** — per-tile verdict (changed/stable by the *smoothed*
//!    score vs threshold; the raw-score verdict is kept alongside for
//!    the ablation) written through the publish ring.
//!
//! The driver is sequential and single-threaded in this version: each
//! frame is fully processed before the next begins, which keeps the
//! `read_published` safety contract trivially satisfied (a slot's next
//! lap is never leased before its consumer has read it — ring
//! capacities exceed one frame's payloads). Concurrency is the
//! substrate's proven property (its own multi-producer tests), not
//! something this lane needs to re-claim.

use crate::record::{InputRecord, GRID, TILES};
use rolling_substrate_core::sheaf::SheafGraph;
use rolling_substrate_core::substrate::FluidSubstrateBuffer;
use std::collections::BTreeMap;

pub const NODE_PERCEIVE: usize = 0;
pub const NODE_DECIDE: usize = 1;
pub const NODE_DIRECT: usize = 2;
pub const NODE_REFINE: usize = 3;
pub const NODE_PUBLISH: usize = 4;

#[derive(Debug, Clone)]
pub struct Config {
    /// Changed iff smoothed score >= threshold.
    pub threshold: f64,
    /// Laplacian step size. Grid λmax ≈ 7.24, so α < 0.138 keeps the
    /// step a strict energy descent; 0.1 is the default.
    pub alpha: f64,
    /// How many most-changed tiles the director marks per frame.
    pub focus_k: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threshold: 0.25,
            alpha: 0.1,
            focus_k: 4,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TileVerdict {
    pub frame: u32,
    pub tile: u32,
    /// Arrival of the record that fed this verdict; for stale tiles,
    /// the arrival of the tile's last real record.
    pub t_arrival_us: u64,
    pub driver_diff: Option<f64>,
    /// Cosine distance vs previous published embedding; `None` on the
    /// tile's first-ever frame (baseline — no change claim is made).
    pub raw_score: Option<f64>,
    pub smoothed_score: Option<f64>,
    pub changed_raw: bool,
    /// The published verdict: threshold on the smoothed score, and
    /// only for tiles with fresh data this frame — stale tiles always
    /// publish stable (see the rule in the publish stage).
    pub changed: bool,
    pub focus: bool,
    /// True when the tile had no record in this frame and the verdict
    /// rests on its last known score (CHANGED-strategy files do this).
    pub stale: bool,
    /// Pipeline lineage for the underlying payload: which stage nodes
    /// touched it. For stale tiles this is the empty set.
    pub lineage_mask: u64,
}

#[derive(Debug, Clone)]
pub struct FrameVerdict {
    pub frame: u32,
    pub changed: bool,
    pub changed_tiles: Vec<u32>,
    pub focus: Vec<u32>,
    pub energy_before: f64,
    pub energy_after: f64,
    pub records_in_frame: usize,
}

#[derive(Debug)]
pub struct RunOutput {
    pub tiles: Vec<TileVerdict>,
    pub frames: Vec<FrameVerdict>,
    pub dim: usize,
    /// Ring dimensions actually used — proof the payloads were carried
    /// at full size: ingest = dim, decide = dim + 1.
    pub ingest_dim: usize,
    pub decide_dim: usize,
}

/// 4×4 grid adjacency (right + down edges), tiles as nodes.
pub fn grid_graph() -> SheafGraph {
    let mut edges = Vec::new();
    for r in 0..GRID {
        for c in 0..GRID {
            let t = r * GRID + c;
            if c + 1 < GRID {
                edges.push((t, t + 1));
            }
            if r + 1 < GRID {
                edges.push((t, t + GRID));
            }
        }
    }
    SheafGraph::from_edges(TILES, edges)
}

pub struct Pipeline {
    cfg: Config,
    ingest: FluidSubstrateBuffer,
    decide: FluidSubstrateBuffer,
    focus_ring: FluidSubstrateBuffer,
    score_ring: FluidSubstrateBuffer,
    publish: FluidSubstrateBuffer,
    graph: SheafGraph,
    dim: usize,
}

impl Pipeline {
    pub fn new(dim: usize, cfg: Config) -> Self {
        assert!(dim > 0);
        Self {
            ingest: FluidSubstrateBuffer::new(64, dim),
            decide: FluidSubstrateBuffer::new(64, dim + 1),
            focus_ring: FluidSubstrateBuffer::new(16, TILES),
            score_ring: FluidSubstrateBuffer::new(16, TILES),
            publish: FluidSubstrateBuffer::new(64, 2),
            graph: grid_graph(),
            dim,
            cfg,
        }
    }

    pub fn ingest_dim(&self) -> usize {
        self.ingest.dimension()
    }
    pub fn decide_dim(&self) -> usize {
        self.decide.dimension()
    }

    pub fn run(self, records: &[InputRecord]) -> RunOutput {
        // Group by frame, ascending; tiles ascending within a frame.
        let mut by_frame: BTreeMap<u32, Vec<&InputRecord>> = BTreeMap::new();
        for r in records {
            assert_eq!(
                r.embedding.len(),
                self.dim,
                "record seq {} has dim {}, pipeline built for {}",
                r.seq,
                r.embedding.len(),
                self.dim
            );
            assert!((r.tile as usize) < TILES, "tile {} out of grid", r.tile);
            by_frame.entry(r.frame).or_default().push(r);
        }

        // Lens state: last published embedding + score per tile.
        let mut prev_emb: Vec<Option<Vec<f32>>> = vec![None; TILES];
        let mut last_score: Vec<Option<f64>> = vec![None; TILES];
        let mut last_arrival: Vec<u64> = vec![0; TILES];
        let mut last_diff: Vec<Option<f64>> = vec![None; TILES];

        let mut tiles_out: Vec<TileVerdict> = Vec::new();
        let mut frames_out: Vec<FrameVerdict> = Vec::new();

        for (frame, mut recs) in by_frame {
            recs.sort_by_key(|r| r.tile);
            let mut raw: Vec<Option<f64>> = vec![None; TILES];
            let mut fresh: Vec<bool> = vec![false; TILES];
            let mut lineage_of: Vec<u64> = vec![0; TILES];

            for r in &recs {
                let t = r.tile as usize;
                // ---- perceive: publish the full embedding to the ring
                let claim = {
                    let mut lease = self.ingest.lease();
                    lease.data_mut().copy_from_slice(&r.embedding);
                    lease.record_visit(NODE_PERCEIVE);
                    let c = lease.claim();
                    lease.finalize();
                    c
                };
                // ---- decide: read published, score vs previous
                // SAFETY: claim just finalized above; the next lap of
                // this slot cannot be leased until 64 further leases,
                // and this read completes before any of them matter —
                // in fact before the next lease of any kind, since the
                // driver is sequential.
                let emb: Vec<f32> = unsafe { self.ingest.read_published(claim) }.to_vec();
                debug_assert_eq!(emb, r.embedding);
                let score = prev_emb[t]
                    .as_ref()
                    .map(|p| crate::rng::cosine_distance(p, &emb));
                {
                    let mut lease = self.decide.lease();
                    lease.data_mut()[0] = score.unwrap_or(0.0) as f32;
                    lease.data_mut()[1..].copy_from_slice(&emb);
                    lease.record_visit(NODE_DECIDE);
                    lease.finalize();
                }
                lineage_of[t] = (1 << NODE_PERCEIVE) | (1 << NODE_DECIDE);
                prev_emb[t] = Some(emb);
                raw[t] = score;
                fresh[t] = true;
                last_arrival[t] = r.t_arrival_us;
                last_diff[t] = Some(r.diff_score);
                if let Some(s) = score {
                    last_score[t] = Some(s);
                }
            }

            // Score vector for refine: raw where fresh, last known
            // where stale, 0 where never seen.
            let mut scores: Vec<f64> = (0..TILES)
                .map(|t| raw[t].or(last_score[t]).unwrap_or(0.0))
                .collect();

            // ---- direct: top-k most-changed tiles are the focus
            let mut ranked: Vec<usize> = (0..TILES).collect();
            ranked.sort_by(|&a, &b| {
                scores[b]
                    .partial_cmp(&scores[a])
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.cmp(&b))
            });
            let focus: Vec<u32> = ranked[..self.cfg.focus_k.min(TILES)]
                .iter()
                .map(|&t| t as u32)
                .collect();
            {
                let mut lease = self.focus_ring.lease();
                for &t in &focus {
                    lease.data_mut()[t as usize] = 1.0;
                }
                lease.record_visit(NODE_DIRECT);
                lease.finalize();
            }

            // ---- refine: ONE Laplacian step over the tile grid
            let energy_before = self.graph.energy(&scores);
            {
                let mut lease = self.score_ring.lease();
                lease
                    .data_mut()
                    .copy_from_slice(&scores.iter().map(|&s| s as f32).collect::<Vec<_>>());
                lease.record_visit(NODE_REFINE);
                lease.finalize();
            }
            self.graph.smooth_step(&mut scores, self.cfg.alpha);
            let energy_after = self.graph.energy(&scores);
            debug_assert!(energy_after <= energy_before + 1e-12);

            // ---- publish: per-tile verdicts through the publish ring
            let mut frame_changed_tiles: Vec<u32> = Vec::new();
            for t in 0..TILES {
                let smoothed = if raw[t].is_some() || last_score[t].is_some() {
                    Some(scores[t])
                } else {
                    None
                };
                // A change verdict is a claim about THIS frame. A
                // stale tile's last known score may still be above
                // threshold (e.g. the cut score, two frames later, in
                // a sparse CHANGED-strategy file) — republishing it as
                // a fresh change is a false alarm the ALL strategy
                // never makes, and the compare test caught exactly
                // that. Rule: stale tiles publish *stable*; their
                // last known smoothed score is still reported above
                // for context, labelled stale.
                let changed =
                    fresh[t] && smoothed.map(|s| s >= self.cfg.threshold).unwrap_or(false);
                let changed_raw = raw[t].map(|s| s >= self.cfg.threshold).unwrap_or(false);
                {
                    let mut lease = self.publish.lease();
                    lease.data_mut()[0] = smoothed.unwrap_or(0.0) as f32;
                    lease.data_mut()[1] = if changed { 1.0 } else { 0.0 };
                    lease.record_visit(NODE_PUBLISH);
                    lease.finalize();
                }
                if changed {
                    frame_changed_tiles.push(t as u32);
                }
                tiles_out.push(TileVerdict {
                    frame,
                    tile: t as u32,
                    t_arrival_us: last_arrival[t],
                    driver_diff: last_diff[t],
                    raw_score: raw[t],
                    smoothed_score: smoothed,
                    changed_raw,
                    changed,
                    focus: focus.contains(&(t as u32)),
                    stale: !fresh[t],
                    lineage_mask: if fresh[t] {
                        lineage_of[t]
                            | (1 << NODE_DIRECT)
                            | (1 << NODE_REFINE)
                            | (1 << NODE_PUBLISH)
                    } else {
                        0
                    },
                });
            }
            frames_out.push(FrameVerdict {
                frame,
                changed: !frame_changed_tiles.is_empty(),
                changed_tiles: frame_changed_tiles,
                focus,
                energy_before,
                energy_after,
                records_in_frame: recs.len(),
            });
        }

        RunOutput {
            tiles: tiles_out,
            frames: frames_out,
            dim: self.dim,
            ingest_dim: self.ingest.dimension(),
            decide_dim: self.decide.dimension(),
        }
    }
}

/// Convenience: build a pipeline sized from the first record and run.
pub fn run_records(records: &[InputRecord], cfg: Config) -> RunOutput {
    let dim = records
        .first()
        .map(|r| r.embedding.len())
        .expect("run_records needs at least one record");
    Pipeline::new(dim, cfg).run(records)
}
