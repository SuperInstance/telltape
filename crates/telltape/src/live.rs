//! Incremental (live) form of the harbor-watch cascade.
//!
//! `harbor_watch::pipeline::Pipeline::run` consumes a whole record
//! file at once; a live service cannot. `LivePipeline` carries the
//! same stage state across frames and processes one frame per call.
//! The stage semantics are deliberately identical to the batch
//! pipeline — same rings, same cosine decide, same director top-k,
//! same ONE Laplacian refine step, and the same publish rule (a
//! verdict is a claim about *this* frame; stale tiles publish
//! stable). The parity test in `tests/live.rs` runs the synthetic
//! fixture through both forms and asserts identical verdicts, so
//! "identical" is a tested claim, not a comment.

use harbor_watch::pipeline::{grid_graph_for, Config, FrameVerdict, PipelineError, TileVerdict};
use harbor_watch::pipeline::{NODE_DECIDE, NODE_DIRECT, NODE_PERCEIVE, NODE_PUBLISH, NODE_REFINE};
use harbor_watch::record::{Grid, InputRecord};
use rolling_substrate_core::sheaf::SheafGraph;
use rolling_substrate_core::substrate::FluidSubstrateBuffer;

/// Why a frame was refused (gap G2): the same typed refusals as
/// the batch pipeline. A bad record in a live stream is dropped
/// with a reason — it never panics the watch.
pub type ProcessError = PipelineError;

pub struct LivePipeline {
    cfg: Config,
    ingest: FluidSubstrateBuffer,
    decide: FluidSubstrateBuffer,
    focus_ring: FluidSubstrateBuffer,
    score_ring: FluidSubstrateBuffer,
    publish: FluidSubstrateBuffer,
    graph: SheafGraph,
    dim: usize,
    tiles: usize,
    prev_emb: Vec<Option<Vec<f32>>>,
    last_score: Vec<Option<f64>>,
    last_arrival: Vec<u64>,
    last_diff: Vec<Option<f64>>,
}

/// Wall-clock milliseconds each stage group took for one frame —
/// the per-stage receipts the ledger records.
#[derive(Debug, Clone, Copy, Default)]
pub struct StageMs {
    pub perceive_decide: f64,
    pub direct: f64,
    pub refine: f64,
    pub publish: f64,
}

pub struct FrameOutput {
    pub verdict: FrameVerdict,
    pub tiles: Vec<TileVerdict>,
    pub stage_ms: StageMs,
}

impl LivePipeline {
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
            prev_emb: vec![None; tiles],
            last_score: vec![None; tiles],
            last_arrival: vec![0; tiles],
            last_diff: vec![None; tiles],
        }
    }

    pub fn dim(&self) -> usize {
        self.dim
    }

    /// The configured tile grid.
    pub fn grid(&self) -> Grid {
        self.cfg.grid
    }

    /// Tile count of the configured grid.
    pub fn tiles(&self) -> usize {
        self.tiles
    }

    /// Process one frame's records (any subset of tiles — tiles with
    /// no record are stale this frame). Mirrors one iteration of the
    /// batch pipeline's frame loop exactly. Caller errors (a record
    /// of the wrong width, a tile outside the grid) are refused
    /// typed, before any state is touched (gap G2).
    pub fn process_frame(
        &mut self,
        frame: u32,
        mut recs: Vec<InputRecord>,
    ) -> Result<FrameOutput, ProcessError> {
        for r in &recs {
            if r.embedding.len() != self.dim {
                return Err(PipelineError::DimMismatch {
                    seq: r.seq,
                    expected: self.dim,
                    got: r.embedding.len(),
                });
            }
            if (r.tile as usize) >= self.tiles {
                return Err(PipelineError::TileOutOfGrid {
                    tile: r.tile,
                    tiles: self.tiles,
                });
            }
        }
        let tiles = self.tiles;
        let t0 = std::time::Instant::now();
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
            // ---- decide: read published, score vs previous.
            // SAFETY: same argument as the batch pipeline — the driver
            // is sequential, so this slot's next lap cannot be leased
            // before this read completes.
            let emb: Vec<f32> = unsafe { self.ingest.read_published(claim) }.to_vec();
            let score = self.prev_emb[t]
                .as_ref()
                .map(|p| harbor_watch::rng::cosine_distance(p, &emb));
            {
                let mut lease = self.decide.lease();
                lease.data_mut()[0] = score.unwrap_or(0.0) as f32;
                lease.data_mut()[1..].copy_from_slice(&emb);
                lease.record_visit(NODE_DECIDE);
                lease.finalize();
            }
            lineage_of[t] = (1 << NODE_PERCEIVE) | (1 << NODE_DECIDE);
            self.prev_emb[t] = Some(emb);
            raw[t] = score;
            fresh[t] = true;
            self.last_arrival[t] = r.t_arrival_us;
            self.last_diff[t] = Some(r.diff_score);
            if let Some(s) = score {
                self.last_score[t] = Some(s);
            }
        }
        let ms_perceive_decide = t0.elapsed().as_secs_f64() * 1e3;

        // Score vector for refine: raw where fresh, last known where
        // stale, 0 where never seen. (Identical to batch.)
        let mut scores: Vec<f64> = (0..tiles)
            .map(|t| raw[t].or(self.last_score[t]).unwrap_or(0.0))
            .collect();

        // ---- direct: top-k most-changed tiles are the focus
        let t1 = std::time::Instant::now();
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
        let ms_direct = t1.elapsed().as_secs_f64() * 1e3;

        // ---- refine: ONE Laplacian step over the tile grid
        let t2 = std::time::Instant::now();
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
        let ms_refine = t2.elapsed().as_secs_f64() * 1e3;

        // ---- publish: per-tile verdicts through the publish ring
        let t3 = std::time::Instant::now();
        let mut tiles_out: Vec<TileVerdict> = Vec::with_capacity(tiles);
        let mut frame_changed_tiles: Vec<u32> = Vec::new();
        for t in 0..tiles {
            let smoothed = if raw[t].is_some() || self.last_score[t].is_some() {
                Some(scores[t])
            } else {
                None
            };
            // The publish rule (batch-identical): a change verdict is
            // a claim about THIS frame — stale tiles publish stable.
            let changed = fresh[t] && smoothed.map(|s| s >= self.cfg.threshold).unwrap_or(false);
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
                t_arrival_us: self.last_arrival[t],
                driver_diff: self.last_diff[t],
                raw_score: raw[t],
                smoothed_score: smoothed,
                changed_raw,
                changed,
                focus: focus.contains(&(t as u32)),
                stale: !fresh[t],
                lineage_mask: if fresh[t] {
                    lineage_of[t] | (1 << NODE_DIRECT) | (1 << NODE_REFINE) | (1 << NODE_PUBLISH)
                } else {
                    0
                },
            });
        }
        let ms_publish = t3.elapsed().as_secs_f64() * 1e3;

        Ok(FrameOutput {
            verdict: FrameVerdict {
                frame,
                changed: !frame_changed_tiles.is_empty(),
                changed_tiles: frame_changed_tiles,
                focus,
                energy_before,
                energy_after,
                records_in_frame: recs.len(),
            },
            tiles: tiles_out,
            stage_ms: StageMs {
                perceive_decide: ms_perceive_decide,
                direct: ms_direct,
                refine: ms_refine,
                publish: ms_publish,
            },
        })
    }
}
