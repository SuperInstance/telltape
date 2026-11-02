//! Ground-truth tests over the synthetic fixture. The fixture is the
//! contract: if the detector's behaviour changes, these tests — not
//! anyone's intuition — say whether it changed for the better.

use harbor_watch::ablation;
use harbor_watch::fixture::{self, CUT_FRAME, CUT_TILES, DRIFT_TILE, SPIKE_FRAME, SPIKE_TILE};
use harbor_watch::pipeline::{run_records, Config};
use harbor_watch::record::{parse_jsonl, record_to_json};

const DIM: usize = 256; // speed; the 1280 case has its own test below

fn run_fixture() -> (fixture::Fixture, harbor_watch::pipeline::RunOutput) {
    let fx = fixture::generate(DIM, 7);
    let out = run_records(&fx.records, Config::default());
    (fx, out)
}

#[test]
fn fixture_script_and_ground_truth() {
    let fx = fixture::generate(DIM, 7);
    assert_eq!(fx.records.len(), (fixture::FRAMES * 16) as usize);
    assert!(fx.records.iter().all(|r| r.embedding.len() == DIM));
    for &t in &CUT_TILES {
        assert!(fx.truth_changed(CUT_FRAME, t), "cut tile {t} must be truth");
    }
    // Spike and drift are NOT truth — that is the whole point of them.
    assert!(!fx.truth_changed(SPIKE_FRAME, SPIKE_TILE));
    for f in 8..fixture::FRAMES {
        assert!(!fx.truth_changed(f, DRIFT_TILE));
    }
    let total_truth: usize = fx.truth.values().filter(|&&b| b).count();
    assert_eq!(total_truth, 3, "exactly the 3 cut tiles are truth-changed");
}

#[test]
fn cut_detected_at_the_cut_frame_not_before_not_after() {
    let (_fx, out) = run_fixture();
    let changed_frames: Vec<u32> = out
        .frames
        .iter()
        .filter(|f| f.changed)
        .map(|f| f.frame)
        .collect();
    assert_eq!(
        changed_frames,
        vec![CUT_FRAME],
        "the only changed frame must be the cut frame (within-1-frame requirement, met exactly)"
    );
    let f4 = &out.frames[CUT_FRAME as usize];
    assert_eq!(f4.changed_tiles, CUT_TILES.to_vec());
}

#[test]
fn gradual_drift_is_not_flagged_as_a_cut() {
    let (_fx, out) = run_fixture();
    for v in out
        .tiles
        .iter()
        .filter(|v| v.tile == DRIFT_TILE && v.frame >= 8)
    {
        assert!(
            !v.changed_raw,
            "drift tile raw-flagged at frame {}",
            v.frame
        );
        assert!(!v.changed, "drift tile flagged at frame {}", v.frame);
        assert!(
            v.raw_score.unwrap() < 0.25,
            "per-step drift score {} must stay below threshold",
            v.raw_score.unwrap()
        );
    }
}

#[test]
fn ablation_refine_vs_raw_on_fixture() {
    let (fx, out) = run_fixture();
    let ab = ablation::evaluate(&out, &|f, t| fx.truth_changed(f, t));
    // Raw scorer: catches all 3 cut tiles, plus the noise spike as
    // false positives — twice (out at frame 6, back at frame 7, since
    // decide compares vs the previous published embedding).
    assert_eq!(ab.raw.tp, 3);
    assert_eq!(ab.raw.fp, 2, "raw must FP exactly the spike out-and-back");
    assert_eq!(ab.raw.fn_, 0);
    // Refined: spike pulled below threshold by its stable neighbours,
    // cut tiles — whose neighbours include other cut tiles — survive.
    assert_eq!(ab.refined.tp, 3, "refine must not lose the cut");
    assert_eq!(ab.refined.fp, 0, "refine must rescue the spike FP");
    assert_eq!(ab.refined.fn_, 0);
    assert!(ab.refined.precision() > ab.raw.precision());
    assert_eq!(ab.refined.recall(), ab.raw.recall());
}

#[test]
fn sheaf_energy_never_increases() {
    let (_fx, out) = run_fixture();
    for f in &out.frames {
        assert!(
            f.energy_after <= f.energy_before + 1e-9,
            "frame {} energy {} -> {}",
            f.frame,
            f.energy_before,
            f.energy_after
        );
    }
    // And at the cut frame there is real energy to remove.
    let f4 = &out.frames[CUT_FRAME as usize];
    assert!(f4.energy_before > 1.0 && f4.energy_after < f4.energy_before);
}

#[test]
fn jsonl_round_trip() {
    let fx = fixture::generate(64, 3);
    let mut text = String::new();
    for r in &fx.records {
        text.push_str(&record_to_json(r));
        text.push('\n');
    }
    let back = parse_jsonl(&text).unwrap();
    assert_eq!(back.len(), fx.records.len());
    for (a, b) in fx.records.iter().zip(back.iter()) {
        assert_eq!(
            (a.seq, a.frame, a.tile, a.t_arrival_us),
            (b.seq, b.frame, b.tile, b.t_arrival_us)
        );
        assert!((a.diff_score - b.diff_score).abs() < 1e-5);
        for (x, y) in a.embedding.iter().zip(b.embedding.iter()) {
            assert!((x - y).abs() < 2e-6, "embedding drift {x} vs {y}");
        }
    }
}

#[test]
fn full_1280_dimension_is_carried_not_truncated() {
    let fx = fixture::generate(1280, 7);
    let out = run_records(&fx.records, Config::default());
    assert_eq!(out.dim, 1280);
    assert_eq!(
        out.ingest_dim, 1280,
        "ingest ring must carry the full embedding"
    );
    assert_eq!(
        out.decide_dim, 1281,
        "decide ring carries score + full embedding"
    );
    assert_eq!(
        out.frames
            .iter()
            .find(|f| f.frame == CUT_FRAME)
            .unwrap()
            .changed_tiles,
        CUT_TILES.to_vec()
    );
}

#[test]
fn changed_strategy_file_stale_tiles_and_compare() {
    let fx = fixture::generate(DIM, 7);
    let all = run_records(&fx.records, Config::default());
    // Simulate a CHANGED-strategy file: baseline frame complete, after
    // that only records whose cheap diff fired (cuts + the spike).
    let ch_records: Vec<_> = fx
        .records
        .iter()
        .filter(|r| r.frame == 0 || r.diff_score > 0.01)
        .cloned()
        .collect();
    assert!(ch_records.len() < fx.records.len());
    let ch = run_records(&ch_records, Config::default());
    // Stale verdicts exist and are labelled: at frame 4 only the cut
    // tiles have records in the sparse file; their neighbours' verdicts
    // rest on last known scores. (Frames with no records at all — 1, 2,
    // 3, 5… — produce no verdicts: the pipeline cannot know a frame
    // existed from an empty file. That gap is a property of the
    // strategy's data, stated here rather than papered over.)
    assert!(ch
        .tiles
        .iter()
        .any(|v| v.stale && v.frame == CUT_FRAME && v.tile == 0));
    assert!(ch
        .tiles
        .iter()
        .all(|v| v.frame == 0 || v.frame == CUT_FRAME || v.frame == SPIKE_FRAME));
    // The cut is still detected in the sparse file.
    assert!(
        ch.frames
            .iter()
            .find(|f| f.frame == CUT_FRAME)
            .unwrap()
            .changed
    );
    let rep = ablation::compare(&all, &ch, &fx.records, &ch_records);
    assert_eq!(rep.reference_frame, Some(CUT_FRAME));
    // CHANGED holds all its frame-4 data earlier (its last frame-4
    // record is tile 11's; ALL must wait for tile 15's).
    assert!(rep.ttfc_changed_us.unwrap() < rep.ttfc_all_us.unwrap());
    assert!(
        rep.tile_agreement >= 0.95,
        "strategies should mostly agree, got {}",
        rep.tile_agreement
    );
    assert_eq!(rep.frame_agreement, 1.0);
}
