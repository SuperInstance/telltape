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

use crate::record::{Grid, InputRecord};
use rolling_substrate_core::sheaf::SheafGraph;
use rolling_substrate_core::substrate::FluidSubstrateBuffer;
use std::collections::BTreeMap;
use std::fmt;

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
    /// Tile grid over the frame (default 4×4 — the measured
    /// configuration; see docs/GRID.md). Tile numbers in every
    /// verdict are row-major cells of this grid.
    pub grid: Grid,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            threshold: 0.25,
            alpha: 0.1,
            focus_k: 4,
            grid: Grid::default(),
        }
    }
}

/// Why a run refused its input (gap G2: caller errors are typed
/// refusals, never panics — a malformed record stream must not be
/// able to crash a long-running watch).
#[derive(Debug, Clone, PartialEq)]
pub enum PipelineError {
    /// `run_records` was given no records to size the pipeline from.
    EmptyRecords,
    /// A record's embedding width differs from the pipeline's.
    DimMismatch {
        seq: u64,
        expected: usize,
        got: usize,
    },
    /// A record names a tile outside the configured grid.
    TileOutOfGrid { tile: u32, tiles: usize },
    /// The pipeline was built with a zero embedding dimension.
    ZeroDim,
    /// A record's embedding is degenerate — zero norm, or a
    /// non-finite component or norm (playtest F1). Such a vector
    /// carries no information, yet under the old scorer it
    /// produced maximal distance scores: a broken embedder
    /// returning zeros yielded a confident, perfect-looking
    /// ledger of pixel-diff echoes. A degenerate embedding is
    /// refused, typed, before any frame is scored — the run
    /// stops rather than guess.
    DegenerateEmbedding {
        seq: u64,
        frame: u32,
        tile: u32,
        reason: &'static str,
    },
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PipelineError::EmptyRecords => {
                write!(f, "run_records needs at least one record")
            }
            PipelineError::DimMismatch { seq, expected, got } => write!(
                f,
                "record seq {seq} has dim {got}, pipeline built for {expected}"
            ),
            PipelineError::TileOutOfGrid { tile, tiles } => {
                write!(f, "tile {tile} out of grid ({tiles} tiles)")
            }
            PipelineError::ZeroDim => write!(f, "pipeline dimension must be > 0"),
            PipelineError::DegenerateEmbedding {
                seq,
                frame,
                tile,
                reason,
            } => write!(
                f,
                "record seq {seq} has a degenerate embedding ({reason}) at frame {frame} tile {tile}: \
                 the vector carries no information and must not be scored"
            ),
        }
    }
}
impl std::error::Error for PipelineError {}

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

/// Grid adjacency (right + down edges) for an arbitrary grid,
/// tiles as nodes, numbered row-major.
pub fn grid_graph_for(grid: Grid) -> SheafGraph {
    let mut edges = Vec::new();
    for r in 0..grid.rows {
        for c in 0..grid.cols {
            let t = grid.tile_at(r, c);
            if c + 1 < grid.cols {
                edges.push((t, t + 1));
            }
            if r + 1 < grid.rows {
                edges.push((t, t + grid.cols));
            }
        }
    }
    SheafGraph::from_edges(grid.tiles(), edges)
}

/// 4×4 grid adjacency (right + down edges), tiles as nodes.
pub fn grid_graph() -> SheafGraph {
    grid_graph_for(Grid::default())
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
    tiles: usize,
}

impl Pipeline {
    pub fn new(dim: usize, cfg: Config) -> Self {
        assert!(dim > 0, "pipeline dimension must be > 0");
        let tiles = cfg.grid.tiles();
        Self {
            ingest: FluidSubstrateBuffer::new(64, dim),
            decide: FluidSubstrateBuffer::new(64, dim + 1),
            focus_ring: FluidSubstrateBuffer::new(16, tiles),
            score_ring: FluidSubstrateBuffer::new(16, tiles),
            publish: FluidSubstrateBuffer::new(64, 2),
            graph: grid_graph_for(cfg.grid),
            dim,
            tiles,
            cfg,
        }
    }

    /// The configured tile grid.
    pub fn grid(&self) -> Grid {
        self.cfg.grid
    }

    pub fn ingest_dim(&self) -> usize {
        self.ingest.dimension()
    }
    pub fn decide_dim(&self) -> usize {
        self.decide.dimension()
    }

    pub fn run(self, records: &[InputRecord]) -> Result<RunOutput, PipelineError> {
        let tiles = self.tiles;
        // Group by frame, ascending; tiles ascending within a frame.
        // Caller errors are refused, typed (gap G2) — never panics.
        let mut by_frame: BTreeMap<u32, Vec<&InputRecord>> = BTreeMap::new();
        for r in records {
            if r.embedding.len() != self.dim {
                return Err(PipelineError::DimMismatch {
                    seq: r.seq,
                    expected: self.dim,
                    got: r.embedding.len(),
                });
            }
            if (r.tile as usize) >= tiles {
                return Err(PipelineError::TileOutOfGrid {
                    tile: r.tile,
                    tiles,
                });
            }
            if let Some(reason) = crate::rng::degenerate_reason(&r.embedding) {
                return Err(PipelineError::DegenerateEmbedding {
                    seq: r.seq,
                    frame: r.frame,
                    tile: r.tile,
                    reason,
                });
            }
            by_frame.entry(r.frame).or_default().push(r);
        }

        // Lens state: last published embedding + score per tile.
        let mut prev_emb: Vec<Option<Vec<f32>>> = vec![None; tiles];
        let mut last_score: Vec<Option<f64>> = vec![None; tiles];
        let mut last_arrival: Vec<u64> = vec![0; tiles];
        let mut last_diff: Vec<Option<f64>> = vec![None; tiles];

        let mut tiles_out: Vec<TileVerdict> = Vec::new();
        let mut frames_out: Vec<FrameVerdict> = Vec::new();

        for (frame, mut recs) in by_frame {
            recs.sort_by_key(|r| r.tile);
            let mut raw: Vec<Option<f64>> = vec![None; tiles];
            let mut fresh: Vec<bool> = vec![false; tiles];
            let mut lineage_of: Vec<u64> = vec![0; tiles];

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
                    .and_then(|p| crate::rng::cosine_distance(p, &emb));
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
            let mut scores: Vec<f64> = (0..tiles)
                .map(|t| raw[t].or(last_score[t]).unwrap_or(0.0))
                .collect();

            // ---- direct: top-k most-changed tiles are the focus
            let mut ranked: Vec<usize> = (0..tiles).collect();
            ranked.sort_by(|&a, &b| {
                scores[b]
                    .partial_cmp(&scores[a])
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.cmp(&b))
            });
            let focus: Vec<u32> = ranked[..self.cfg.focus_k.min(tiles)]
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
            for t in 0..tiles {
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

        Ok(RunOutput {
            tiles: tiles_out,
            frames: frames_out,
            dim: self.dim,
            ingest_dim: self.ingest.dimension(),
            decide_dim: self.decide.dimension(),
        })
    }
}

/// Convenience: build a pipeline sized from the first record and run.
pub fn run_records(records: &[InputRecord], cfg: Config) -> Result<RunOutput, PipelineError> {
    let first = records.first().ok_or(PipelineError::EmptyRecords)?;
    if first.embedding.is_empty() {
        return Err(PipelineError::ZeroDim);
    }
    Pipeline::new(first.embedding.len(), cfg).run(records)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::Grid;

    fn rec(frame: u32, tile: u32, emb: Vec<f32>) -> InputRecord {
        InputRecord {
            seq: 0,
            frame,
            tile,
            t_arrival_us: 0,
            diff_score: 0.0,
            embedding: emb,
        }
    }

    #[test]
    fn empty_input_is_refused_typed() {
        assert_eq!(
            run_records(&[], Config::default()).unwrap_err(),
            PipelineError::EmptyRecords
        );
    }

    #[test]
    fn dim_mismatch_is_refused_not_panicked() {
        // (The sound record's vector is non-zero: since F1 an
        // all-zeros record is refused as degenerate in its own
        // right, before the second record's width is examined.)
        let recs = vec![rec(0, 0, vec![0.5; 8]), rec(0, 1, vec![0.0; 4])];
        match run_records(&recs, Config::default()) {
            Err(PipelineError::DimMismatch {
                expected: 8,
                got: 4,
                ..
            }) => {}
            other => panic!("expected DimMismatch, got {other:?}"),
        }
    }

    /// Playtest F1: a zero vector (a dead embedder's placeholder
    /// output) must be refused before scoring — under the old
    /// scorer it would have scored maximal distance and published
    /// a confident, wrong verdict. NaN is refused the same way.
    #[test]
    fn degenerate_embeddings_are_refused_not_scored() {
        let sound = vec![1.0f32, 0.0, 0.0, 0.0];
        for bad in [vec![0.0; 4], vec![f32::NAN, 0.0, 0.0, 0.0]] {
            let recs = vec![rec(0, 0, sound.clone()), rec(1, 0, bad)];
            match run_records(&recs, Config::default()) {
                Err(PipelineError::DegenerateEmbedding {
                    frame: 1, tile: 0, ..
                }) => {}
                other => panic!("expected DegenerateEmbedding, got {other:?}"),
            }
        }
    }

    #[test]
    fn tile_outside_the_grid_is_refused_not_panicked() {
        let recs = vec![rec(0, 16, vec![0.0; 8])];
        match run_records(&recs, Config::default()) {
            Err(PipelineError::TileOutOfGrid {
                tile: 16,
                tiles: 16,
            }) => {}
            other => panic!("expected TileOutOfGrid, got {other:?}"),
        }
    }

    #[test]
    fn a_custom_grid_runs_and_addresses_its_own_tiles() {
        // 2x2 grid, 2 frames; in frame 1 tile 0's vector flips to a
        // near-orthogonal one: exactly tile 0 must publish changed.
        let cfg = Config {
            grid: Grid::new(2, 2).unwrap(),
            ..Config::default()
        };
        let a = vec![1.0, 0.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0, 0.0];
        let mut recs = Vec::new();
        for f in 0..2u32 {
            for t in 0..4u32 {
                let emb = if f == 1 && t == 0 {
                    b.clone()
                } else {
                    a.clone()
                };
                recs.push(rec(f, t, emb));
            }
        }
        let out = run_records(&recs, cfg).unwrap();
        assert_eq!(out.tiles.len(), 8, "2 frames x 4 tiles");
        let f1 = &out.frames[1];
        assert_eq!(f1.changed_tiles, vec![0]);
    }
}
