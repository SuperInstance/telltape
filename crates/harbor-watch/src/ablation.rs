//! The refine ablation, and the ALL-vs-CHANGED strategy comparison.
//!
//! Ablation question, asked honestly: does the one Laplacian step
//! actually improve tile-change detection, or does it just blur real
//! changes into their neighbours? Method: run the identical pipeline,
//! score every (frame, tile) twice — by the raw cosine score and by
//! the smoothed score, same threshold — and compute precision/recall
//! of each against ground truth. Whichever way it lands is the
//! reported result.

use crate::pipeline::RunOutput;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default)]
pub struct Pr {
    pub tp: usize,
    pub fp: usize,
    pub fn_: usize,
    pub tn: usize,
}

impl Pr {
    pub fn precision(&self) -> f64 {
        if self.tp + self.fp == 0 {
            1.0
        } else {
            self.tp as f64 / (self.tp + self.fp) as f64
        }
    }
    pub fn recall(&self) -> f64 {
        if self.tp + self.fn_ == 0 {
            1.0
        } else {
            self.tp as f64 / (self.tp + self.fn_) as f64
        }
    }
}

#[derive(Debug)]
pub struct Ablation {
    pub raw: Pr,
    pub refined: Pr,
}

/// Score the run's tile verdicts against ground truth. Baseline
/// verdicts (score `None`: a tile's first-ever frame, or a stale
/// verdict) count as "predicted stable" — the detector made no change
/// claim there, and truth for a first frame is not-a-cut by
/// construction of every fixture/driver stream we have.
pub fn evaluate(out: &RunOutput, truth: &dyn Fn(u32, u32) -> bool) -> Ablation {
    let mut a = Ablation {
        raw: Pr::default(),
        refined: Pr::default(),
    };
    for v in &out.tiles {
        let t = truth(v.frame, v.tile);
        let (pr, pred) = (&mut a.raw, v.changed_raw);
        match (pred, t) {
            (true, true) => pr.tp += 1,
            (true, false) => pr.fp += 1,
            (false, true) => pr.fn_ += 1,
            (false, false) => pr.tn += 1,
        }
        let (pr, pred) = (&mut a.refined, v.changed);
        match (pred, t) {
            (true, true) => pr.tp += 1,
            (true, false) => pr.fp += 1,
            (false, true) => pr.fn_ += 1,
            (false, false) => pr.tn += 1,
        }
    }
    a
}

// ---------------------------------------------------------------------------
// --compare: ALL strategy vs CHANGED strategy over the same scene.
//
// Definitions (also in the README — a metric nobody defined is a
// number nobody can argue with):
// * The *reference change frame* F* is the first frame the ALL file's
//   refined verdict calls changed.
// * t_anchor = the earliest t_arrival_us among ALL's records of F*:
//   the moment the changed frame began arriving.
// * A strategy's time-to-first-change-verdict is
//   (latest t_arrival_us among that strategy's records of F*) − t_anchor,
//   i.e. how much stream-time elapsed before the strategy held all the
//   data its F* verdict rests on. If the strategy never flags F*, it
//   missed: TTFC is reported as a miss, not a number.
// * Wall-clock processing time is measured too, and reported
//   separately — stream-time and wall-time are different claims.
// * Verdict agreement: over the (frame, tile) pairs both strategies
//   emitted, the fraction whose `changed` flags agree; likewise per
//   frame.
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct CompareReport {
    pub reference_frame: Option<u32>,
    pub ttfc_all_us: Option<u64>,
    pub ttfc_changed_us: Option<u64>,
    pub wall_all_ms: f64,
    pub wall_changed_ms: f64,
    pub records_all: usize,
    pub records_changed: usize,
    pub tile_agreement: f64,
    pub tile_pairs: usize,
    pub frame_agreement: f64,
    pub frame_pairs: usize,
}

fn arrival_range(records: &[crate::record::InputRecord], frame: u32) -> Option<(u64, u64)> {
    let mut it = records.iter().filter(|r| r.frame == frame);
    let first = it.next()?;
    let mut lo = first.t_arrival_us;
    let mut hi = first.t_arrival_us;
    for r in it {
        lo = lo.min(r.t_arrival_us);
        hi = hi.max(r.t_arrival_us);
    }
    Some((lo, hi))
}

pub fn compare(
    all: &RunOutput,
    changed: &RunOutput,
    all_records: &[crate::record::InputRecord],
    changed_records: &[crate::record::InputRecord],
) -> CompareReport {
    let reference_frame = all.frames.iter().find(|f| f.changed).map(|f| f.frame);
    let (ttfc_all, ttfc_changed) = match reference_frame {
        Some(f) => {
            let anchor = arrival_range(all_records, f).map(|(lo, _)| lo).unwrap_or(0);
            let g = |out: &RunOutput, recs: &[crate::record::InputRecord]| -> Option<u64> {
                let fv = out.frames.iter().find(|x| x.frame == f)?;
                if !fv.changed {
                    return None;
                }
                arrival_range(recs, f).map(|(_, hi)| hi.saturating_sub(anchor))
            };
            (g(all, all_records), g(changed, changed_records))
        }
        None => (None, None),
    };

    let mut a_map: BTreeMap<(u32, u32), bool> = BTreeMap::new();
    for v in &all.tiles {
        a_map.insert((v.frame, v.tile), v.changed);
    }
    let (mut agree, mut pairs) = (0usize, 0usize);
    for v in &changed.tiles {
        if let Some(&a) = a_map.get(&(v.frame, v.tile)) {
            pairs += 1;
            if a == v.changed {
                agree += 1;
            }
        }
    }
    let mut af: BTreeMap<u32, bool> = BTreeMap::new();
    for f in &all.frames {
        af.insert(f.frame, f.changed);
    }
    let (mut fagree, mut fpairs) = (0usize, 0usize);
    for f in &changed.frames {
        if let Some(&a) = af.get(&f.frame) {
            fpairs += 1;
            if a == f.changed {
                fagree += 1;
            }
        }
    }

    CompareReport {
        reference_frame,
        ttfc_all_us: ttfc_all,
        ttfc_changed_us: ttfc_changed,
        wall_all_ms: 0.0, // filled by the caller, which owns the timers
        wall_changed_ms: 0.0,
        records_all: all_records.len(),
        records_changed: changed_records.len(),
        tile_agreement: if pairs > 0 {
            agree as f64 / pairs as f64
        } else {
            1.0
        },
        tile_pairs: pairs,
        frame_agreement: if fpairs > 0 {
            fagree as f64 / fpairs as f64
        } else {
            1.0
        },
        frame_pairs: fpairs,
    }
}
