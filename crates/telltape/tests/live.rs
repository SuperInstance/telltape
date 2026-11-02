//! Live-wrapper tests:
//! 1. parity — LivePipeline (frame-at-a-time) vs the batch
//!    harbor-watch pipeline on the synthetic fixture: identical
//!    verdicts. "The live cascade is the wave-3 cascade" is a tested
//!    claim.
//! 2. ledger — the four ledger files are written with the required
//!    keys, and verdict events carry nonzero lineage.
//! 3. service — the full service loop driven through its ingest
//!    interface (a `cat` of records + frame_end controls), with a
//!    stub rack health endpoint: frames processed, the fixture's
//!    cut published as an event, rack lifecycle untouched (we did
//!    not start it, so we must not stop it).

use harbor_watch::fixture;
use harbor_watch::pipeline::{run_records, Config as PipeConfig};
use harbor_watch::record::record_to_json;
use std::collections::BTreeMap;
use std::io::Write;
use telltape::config::Config;
use telltape::ledger::Ledger;
use telltape::live::LivePipeline;
use telltape::service;

fn tempdir(tag: &str) -> String {
    let d = std::env::temp_dir().join(format!("telltape-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.to_string_lossy().to_string()
}

#[test]
fn live_matches_batch_on_fixture() {
    let fx = fixture::generate(128, 7);
    let cfg = PipeConfig::default();
    let batch = run_records(&fx.records, cfg.clone()).unwrap();

    let mut by_frame: BTreeMap<u32, Vec<harbor_watch::record::InputRecord>> = BTreeMap::new();
    for r in &fx.records {
        by_frame.entry(r.frame).or_default().push(r.clone());
    }
    let mut live = LivePipeline::new(fx.dim, cfg);
    let mut live_tiles = Vec::new();
    let mut live_frames = Vec::new();
    for (frame, recs) in by_frame {
        let out = live.process_frame(frame, recs).unwrap();
        live_tiles.extend(out.tiles);
        live_frames.push(out.verdict);
    }

    assert_eq!(live_tiles.len(), batch.tiles.len());
    for (a, b) in live_tiles.iter().zip(batch.tiles.iter()) {
        assert_eq!(a.frame, b.frame);
        assert_eq!(a.tile, b.tile);
        assert_eq!(a.changed, b.changed, "frame {} tile {}", a.frame, a.tile);
        assert_eq!(a.changed_raw, b.changed_raw);
        assert_eq!(a.stale, b.stale);
        assert_eq!(a.lineage_mask, b.lineage_mask);
        match (a.smoothed_score, b.smoothed_score) {
            (Some(x), Some(y)) => assert!((x - y).abs() < 1e-9),
            (None, None) => {}
            other => panic!("score mismatch: {other:?}"),
        }
    }
    for (a, b) in live_frames.iter().zip(batch.frames.iter()) {
        assert_eq!(a.changed, b.changed);
        assert_eq!(a.changed_tiles, b.changed_tiles);
    }
    // And the fixture's ground truth still holds through the live path:
    let cut: Vec<_> = live_frames
        .iter()
        .filter(|f| f.changed)
        .map(|f| (f.frame, f.changed_tiles.clone()))
        .collect();
    assert_eq!(cut.len(), 1, "exactly one changed frame expected: {cut:?}");
    assert_eq!(cut[0].0, fixture::CUT_FRAME);
    assert_eq!(cut[0].1, fixture::CUT_TILES.to_vec());
}

#[test]
fn ledger_files_and_lineage() {
    let dir = tempdir("ledger");
    let mut ledger = Ledger::open(
        &dir,
        "test-stream",
        0.25,
        0.1,
        false,
        harbor_watch::record::Grid::default(),
        "test-emb@1",
    )
    .unwrap();
    let fx = fixture::generate(64, 7);
    let mut live = LivePipeline::new(fx.dim, PipeConfig::default());
    let mut by_frame: BTreeMap<u32, Vec<_>> = BTreeMap::new();
    for r in &fx.records {
        by_frame.entry(r.frame).or_default().push(r.clone());
    }
    let now = telltape::ledger::epoch_us();
    for (frame, recs) in by_frame {
        let n = recs.len() as u64;
        let out = live.process_frame(frame, recs).unwrap();
        let emb_ms: Vec<f64> = (0..n).map(|_| 173.0).collect();
        ledger.record_frame(&out, now, 16, n, &emb_ms, 0).unwrap();
    }
    let state = std::fs::read_to_string(format!("{dir}/state.txt")).unwrap();
    assert!(state.contains("embeds_spent_s"), "{state}");
    assert!(state.contains("coverage"), "{state}");
    assert!(state.contains("embed_everything_estimate_s"), "{state}");
    let health = std::fs::read_to_string(format!("{dir}/health.txt")).unwrap();
    assert!(health.contains("embed_ms_p50 = 173"), "{health}");
    assert!(health.contains("embed_ms_p99"), "{health}");
    let lens = std::fs::read_to_string(format!("{dir}/lens.txt")).unwrap();
    assert!(lens.contains("tile_score_ema"), "{lens}");
    assert!(lens.contains("embedder_identity = test-emb@1"), "{lens}");
    let events = std::fs::read_to_string(format!("{dir}/events.jsonl")).unwrap();
    let verdicts: Vec<&str> = events
        .lines()
        .filter(|l| l.contains("\"kind\":\"verdict\""))
        .collect();
    assert_eq!(verdicts.len(), 1, "only the cut frame publishes: {events}");
    assert!(
        verdicts[0].contains("\"lineage_mask\":31"),
        "{}",
        verdicts[0]
    );
    assert!(
        verdicts[0].contains("verdict_latency_ms"),
        "{}",
        verdicts[0]
    );
}

/// P6 / gap G1 — the frozen-source regression. The 2026-11 soak
/// failure as a fixture: the camera pusher died, the receiver kept
/// serving its last frame, and the watch counted polls as frames
/// for ~13 h while health claimed the silence was meaningful.
/// Here: frame 0 is real (fixture records); frames 1..=5 deliver
/// NO records and stamp `frame_repeat: true` with one unchanging
/// hash — exactly what a frozen feed looks like at this interface.
/// The ledger must call it frozen and withdraw meaning from the
/// silence, while still counting the frames honestly.
#[test]
fn frozen_source_reads_frozen_not_meaningful_silence() {
    let fx = fixture::generate(128, 7);
    let dir = tempdir("frozen");
    let recs_path = format!("{dir}/records.jsonl");
    let now = telltape::ledger::epoch_us();
    {
        let mut f = std::fs::File::create(&recs_path).unwrap();
        for r in fx.records.iter().filter(|r| r.frame == 0) {
            writeln!(f, "{}", record_to_json(r)).unwrap();
        }
        writeln!(
            f,
            "{{\"control\":\"frame_end\",\"frame\":0,\"t_capture_us\":{now},\"nominated\":16,\"gate_diff\":3.0,\"frame_hash\":\"aaaa0000\",\"frame_repeat\":false}}"
        )
        .unwrap();
        for frame in 1..=5u32 {
            writeln!(
                f,
                "{{\"control\":\"frame_end\",\"frame\":{frame},\"t_capture_us\":{now},\"nominated\":0,\"gate_diff\":3.0,\"frame_hash\":\"aaaa0000\",\"frame_repeat\":true}}"
            )
            .unwrap();
        }
    }
    let cfg = Config {
        stream_id: "frozen-stream".into(),
        ledger_dir: format!("{dir}/ledger"),
        ingest_cmd: format!("cat {recs_path}"),
        embed_url: String::new(),
        max_frames: 6,
        ..Config::default()
    };
    let summary = service::run(&cfg).expect("service run");
    assert_eq!(summary.frames, 6, "polls still counted — as polls");
    assert_eq!(summary.events, 0);
    let health = std::fs::read_to_string(format!("{dir}/ledger/health.txt")).unwrap();
    assert!(health.contains("freshness = frozen"), "{health}");
    assert!(health.contains("silence_meaningful = false"), "{health}");
    assert!(health.contains("frames_repeated = 5"), "{health}");
    assert!(health.contains("frames_novel = 1"), "{health}");
    let state = std::fs::read_to_string(format!("{dir}/ledger/state.txt")).unwrap();
    assert!(state.contains("freshness = frozen"), "{state}");
    let events = std::fs::read_to_string(format!("{dir}/ledger/events.jsonl")).unwrap();
    assert!(events.contains("source_frozen"), "{events}");
}

/// Slice 3.5 — `index` over an image sequence: five identical
/// stills (a frozen stretch of footage), then a hard cut held for
/// three frames. Replayed through the file source and the
/// identical service loop, the ledger must show all 8 frames, a
/// 16/16 verdict on the cut frame (frame 5), and a source_frozen
/// receipt for the identical stretch — freshness accounting does
/// not exempt recorded footage.
#[test]
fn index_replays_an_image_sequence_through_the_same_pipeline() {
    let dir = tempdir("index");
    let seq = format!("{dir}/seq");
    std::fs::create_dir_all(&seq).unwrap();
    let fxdir = format!("{}/fixtures/wave3", env!("CARGO_MANIFEST_DIR"));
    for k in 0..8 {
        let src = if k < 5 { "A-00.png" } else { "B-00.png" };
        std::fs::copy(format!("{fxdir}/{src}"), format!("{seq}/f-{k:03}.png")).unwrap();
    }
    let cfg = Config {
        stream_id: "index-test".into(),
        ledger_dir: format!("{dir}/ledger"),
        ingest_script: format!("{}/ingest/ingest.py", env!("CARGO_MANIFEST_DIR")),
        ..Config::default()
    };
    let summary = telltape::index::index_file(&cfg, &seq).expect("index run");
    assert_eq!(summary.frames, 8);
    assert!(summary.events >= 1, "the cut must publish: {summary:?}");
    let events = std::fs::read_to_string(format!("{dir}/ledger/events.jsonl")).unwrap();
    assert!(events.contains("\"frame\":5"), "{events}");
    assert!(events.contains("\"changed_tiles\":[0, 1, 2"), "{events}");
    assert!(events.contains("source_frozen"), "{events}");
    // D1: index selected the test double, and the ledger says so.
    let lens = std::fs::read_to_string(format!("{dir}/ledger/lens.txt")).unwrap();
    assert!(lens.contains("embedder_identity = double@1"), "{lens}");
}

/// D1 — the same-width swap. Both runs below would produce
/// records of identical width (nothing about the width changes;
/// only the declared embedder identity does) — exactly the case
/// the old width-only guard passed silently. The swap must be
/// refused with both identities named, a refusal must write
/// nothing, an acknowledged swap must proceed and receipt
/// itself in the ledger, and a same-identity resume must be
/// unaffected.
#[test]
fn same_width_embedder_swap_is_refused_until_recalibrated() {
    let dir = tempdir("swap");
    let base = Config {
        stream_id: "s".into(),
        ledger_dir: format!("{dir}/ledger"),
        ingest_cmd: "true".into(),
        embed_url: String::new(),
        ..Config::default()
    };
    let run_a = Config {
        embedder_name: "model-a".into(),
        embedder_version: "1".into(),
        ..base.clone()
    };
    service::run(&run_a).expect("first run stamps the ledger");
    let lens = std::fs::read_to_string(format!("{dir}/ledger/lens.txt")).unwrap();
    assert!(lens.contains("embedder_identity = model-a@1"), "{lens}");

    let run_b = Config {
        embedder_name: "model-b".into(),
        embedder_version: "1".into(),
        ..base.clone()
    };
    match service::run(&run_b) {
        Err(service::ServiceError::EmbedderMismatch {
            recorded,
            configured,
        }) => {
            assert_eq!(recorded, "model-a@1");
            assert_eq!(configured, "model-b@1");
        }
        other => panic!("a same-width swap must be refused, got {other:?}"),
    }
    // The refusal is not a write: the ledger still belongs to A.
    let lens = std::fs::read_to_string(format!("{dir}/ledger/lens.txt")).unwrap();
    assert!(lens.contains("embedder_identity = model-a@1"), "{lens}");
    let events = std::fs::read_to_string(format!("{dir}/ledger/events.jsonl")).unwrap();
    assert!(!events.contains("embedder_swap"), "{events}");

    // Acknowledged: the swap proceeds and receipts itself.
    let run_b_ack = Config {
        recalibrate: true,
        ..run_b.clone()
    };
    service::run(&run_b_ack).expect("an acknowledged swap recalibrates");
    let events = std::fs::read_to_string(format!("{dir}/ledger/events.jsonl")).unwrap();
    assert!(events.contains("\"event\":\"embedder_swap\""), "{events}");
    assert!(events.contains("model-a@1"), "{events}");
    assert!(events.contains("model-b@1"), "{events}");
    let lens = std::fs::read_to_string(format!("{dir}/ledger/lens.txt")).unwrap();
    assert!(lens.contains("embedder_identity = model-b@1"), "{lens}");

    // Same identity again: an ordinary resume, no flag needed.
    service::run(&run_b).expect("same-identity resume is unaffected");
}

/// D1 — the migration path. A pre-0.1.1 ledger records no
/// identity; its embedder is unknown and must never be guessed
/// to be "the same one". Even an undeclared (unknown) config is
/// refused until the operator acknowledges the recalibration
/// once — after which the ledger is stamped and resumes
/// normally.
#[test]
fn legacy_ledger_requires_the_recalibrate_acknowledgement() {
    let dir = tempdir("legacy");
    let ledger_dir = format!("{dir}/ledger");
    std::fs::create_dir_all(&ledger_dir).unwrap();
    // The pre-0.1.1 lens shape: no embedder_identity line.
    std::fs::write(
        format!("{ledger_dir}/lens.txt"),
        "stream_id = s\ngrid = 4x4\nperiod_ms = unknown\nthreshold = 0.2500\nalpha = 0.1000\nframes_observed = 12\n",
    )
    .unwrap();
    std::fs::write(
        format!("{ledger_dir}/events.jsonl"),
        "{\"kind\":\"service\",\"event\":\"start\",\"msg\":\"stream s starting\",\"wall_us\":1}\n",
    )
    .unwrap();
    let cfg = Config {
        stream_id: "s".into(),
        ledger_dir: ledger_dir.clone(),
        ingest_cmd: "true".into(),
        embed_url: String::new(),
        ..Config::default()
    };
    match service::run(&cfg) {
        Err(service::ServiceError::EmbedderMismatch { recorded, .. }) => {
            assert!(recorded.contains("pre-0.1.1"), "{recorded}");
        }
        other => panic!("a legacy ledger must be refused unacknowledged, got {other:?}"),
    }
    let ack = Config {
        recalibrate: true,
        ..cfg.clone()
    };
    service::run(&ack).expect("the acknowledgement stamps the legacy ledger");
    let events = std::fs::read_to_string(format!("{ledger_dir}/events.jsonl")).unwrap();
    assert!(events.contains("\"event\":\"embedder_swap\""), "{events}");
    assert!(events.contains("pre-0.1.1"), "{events}");
    let lens = std::fs::read_to_string(format!("{ledger_dir}/lens.txt")).unwrap();
    assert!(lens.contains("embedder_identity = unknown"), "{lens}");
    // Stamped now: an ordinary unknown-identity resume works.
    service::run(&cfg).expect("a stamped ledger resumes without the flag");
}

/// Found by the slice-5 stranger walk: an ingest that dies at
/// startup (there: a missing Python dep) used to read as a
/// clean zero-frame success. A dead ingest must fail the run;
/// a cleanly-exiting empty ingest stays a clean empty run.
#[test]
fn dead_ingest_fails_the_run_but_empty_ingest_is_clean() {
    let dir = tempdir("ingest-status");
    let base = Config {
        stream_id: "s".into(),
        ledger_dir: format!("{dir}/ledger"),
        embed_url: String::new(),
        ..Config::default()
    };
    let dead = Config {
        ingest_cmd: "exit 3".into(),
        ..base.clone()
    };
    match service::run(&dead) {
        Err(e) => assert!(e.to_string().contains("exited with"), "{e}"),
        Ok(s) => panic!("a dead ingest must not succeed: {s:?}"),
    }
    let empty = Config {
        ingest_cmd: "true".into(),
        ..base
    };
    let s = service::run(&empty).expect("a clean empty ingest is a clean run");
    assert_eq!(s.frames, 0);
    assert_eq!(s.events, 0);
}

/// A one-shot HTTP responder that answers 200 to everything: stands
/// in for the rack's /health so the service loop can run in tests
/// without a rack. (Embeds come from the records, not the rack —
/// the rack is the ingest's dependency, the service only gates on
/// its health.)
fn stub_rack() -> String {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 512];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
        }
    });
    format!("http://{addr}")
}

#[test]
fn service_end_to_end_via_ingest_interface() {
    let fx = fixture::generate(128, 7);
    let dir = tempdir("service");
    let recs_path = format!("{dir}/records.jsonl");
    let now = telltape::ledger::epoch_us();
    {
        let mut f = std::fs::File::create(&recs_path).unwrap();
        let mut by_frame: BTreeMap<u32, Vec<_>> = BTreeMap::new();
        for r in &fx.records {
            by_frame.entry(r.frame).or_default().push(r);
        }
        for (frame, recs) in &by_frame {
            for r in recs {
                writeln!(f, "{}", record_to_json(r)).unwrap();
            }
            writeln!(
                f,
                "{{\"control\":\"frame_end\",\"frame\":{frame},\"t_capture_us\":{now},\"nominated\":16,\"gate_diff\":3.0}}"
            )
            .unwrap();
        }
    }
    let cfg = Config {
        stream_id: "test-stream".into(),
        ledger_dir: format!("{dir}/ledger"),
        ingest_cmd: format!("cat {recs_path}"),
        embed_url: stub_rack(),
        embedder_autostart: false,
        stop_embedder_on_exit: true,
        max_frames: fixture::FRAMES,
        ..Config::default()
    };
    let summary = service::run(&cfg).expect("service run");
    assert_eq!(summary.frames, fixture::FRAMES as u64);
    assert_eq!(summary.events, 1, "only the cut publishes");
    assert!(!summary.embedder_started_by_me);
    assert!(!summary.embedder_stopped_by_me);
    let events = std::fs::read_to_string(format!("{dir}/ledger/events.jsonl")).unwrap();
    assert!(events.contains("\"frame\":4"), "{events}");
    assert!(events.contains("\"changed_tiles\":[3, 7, 11]"), "{events}");
    let health = std::fs::read_to_string(format!("{dir}/ledger/health.txt")).unwrap();
    assert!(health.contains("embedder = up"), "{health}");
}
