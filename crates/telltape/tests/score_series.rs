//! Score-series sidecar tests (v0.4.0):
//! 1. At the service level: with the sidecar enabled, the
//!    ledger's own files are unchanged (the deterministic
//!    content of events.jsonl and state.txt, clock fields
//!    normalised) and the sidecar holds exactly one row per
//!    processed frame; with it disabled, no sidecar exists.
//! 2. At the ledger level: a synthetic sub-threshold run-in —
//!    scores climbing frame by frame through the near-miss
//!    band without ever firing — is recorded in the sidecar
//!    with the gate's own values, while the ledger itself
//!    publishes nothing. This is the exact data the CfC
//!    anticipation test's clause (i) consumes and the exact
//!    data whose absence VALIDATION-VERDICT-1 §6 recorded.

use harbor_watch::fixture;
use harbor_watch::pipeline::Config as PipeConfig;
use harbor_watch::record::{record_to_json, Grid, InputRecord};
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

/// Replace the value of a numeric JSON field with 0 — the
/// clock-derived fields (wall_us, verdict_latency_ms) that
/// legitimately differ between any two runs, with or without
/// this feature.
fn blank_field(text: &str, field: &str) -> String {
    let pat = format!("\"{field}\":");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(&pat) {
        out.push_str(&rest[..at + pat.len()]);
        rest = &rest[at + pat.len()..];
        let end = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
            .unwrap_or(rest.len());
        out.push('0');
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn normalize_events(text: &str) -> String {
    // wall_us and verdict_latency_ms are clock readings; the
    // four stage_ms values are measured stage timings. All
    // five vary between any two runs of the unmodified tool —
    // they are the comparison's noise floor, not its signal.
    let mut t = blank_field(text, "wall_us");
    for f in [
        "verdict_latency_ms",
        "perceive_decide",
        "direct",
        "refine",
        "publish",
    ] {
        t = blank_field(&t, f);
    }
    t
}

fn normalize_state(text: &str) -> String {
    text.lines()
        .filter(|l| !l.starts_with("updated_wall_us"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Read a numeric-or-null field out of one sidecar row
/// without a JSON parser (the test-double approach the crate
/// itself uses for control lines).
fn row_num(row: &str, field: &str) -> Option<f64> {
    let pat = format!("\"{field}\":");
    let at = row.find(&pat)?;
    let rest = &row[at + pat.len()..];
    if rest.starts_with("null") {
        return None;
    }
    let end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
        .unwrap_or(rest.len());
    rest[..end].parse::<f64>().ok()
}

fn fixture_records_file(dir: &str) -> String {
    let fx = fixture::generate(128, 7);
    let recs_path = format!("{dir}/records.jsonl");
    let now = telltape::ledger::epoch_us();
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
    recs_path
}

#[test]
fn sidecar_counts_processed_frames_and_the_ledger_is_unchanged() {
    let fx_frames = fixture::FRAMES as u64;
    let dir = tempdir("series-service");
    let recs_path = fixture_records_file(&dir);

    let base = Config {
        stream_id: "series-stream".into(),
        ingest_cmd: format!("cat {recs_path}"),
        embed_url: String::new(),
        embedder_name: "test-emb".into(),
        embedder_version: "1".into(),
        max_frames: fixture::FRAMES,
        ..Config::default()
    };

    // Off: the run is a pre-feature run; no sidecar anywhere.
    let off = Config {
        ledger_dir: format!("{dir}/ledger-off"),
        ..base.clone()
    };
    let s_off = service::run(&off).expect("run without sidecar");
    assert_eq!(s_off.frames, fx_frames);
    assert!(
        !std::path::Path::new(&format!("{dir}/series.jsonl")).exists(),
        "no sidecar may appear when the feature is off"
    );

    // On: same records, sidecar enabled.
    let series_path = format!("{dir}/series.jsonl");
    let on = Config {
        ledger_dir: format!("{dir}/ledger-on"),
        score_series_path: series_path.clone(),
        ..base
    };
    let s_on = service::run(&on).expect("run with sidecar");
    assert_eq!(s_on.frames, s_off.frames);
    assert_eq!(s_on.events, s_off.events);
    assert_eq!(s_on.embeds, s_off.embeds);

    // The ledger's deterministic content is identical.
    let ev_off = std::fs::read_to_string(format!("{dir}/ledger-off/events.jsonl")).unwrap();
    let ev_on = std::fs::read_to_string(format!("{dir}/ledger-on/events.jsonl")).unwrap();
    assert_eq!(
        normalize_events(&ev_off),
        normalize_events(&ev_on),
        "events.jsonl must not change when the sidecar is enabled"
    );
    let st_off = std::fs::read_to_string(format!("{dir}/ledger-off/state.txt")).unwrap();
    let st_on = std::fs::read_to_string(format!("{dir}/ledger-on/state.txt")).unwrap();
    assert_eq!(
        normalize_state(&st_off),
        normalize_state(&st_on),
        "state.txt must not change when the sidecar is enabled"
    );

    // The sidecar: one row per processed frame, in frame
    // order, identity-stamped, and the fired flags agree
    // with the ledger's single verdict (the fixture's cut).
    let series = std::fs::read_to_string(&series_path).unwrap();
    let rows: Vec<&str> = series.lines().collect();
    assert_eq!(
        rows.len() as u64,
        fx_frames,
        "one row per processed frame: {series}"
    );
    for (i, row) in rows.iter().enumerate() {
        assert!(
            row.contains(&format!("\"frame\":{i},")),
            "row {i} out of order: {row}"
        );
        assert!(
            row.contains("\"embedder\":\"test-emb@1\""),
            "row {i} carries the run identity: {row}"
        );
        assert!(row.contains("\"t_capture_us\":"), "row {i}: {row}");
        assert!(row.contains("\"tiles\":["), "row {i}: {row}");
    }
    let fired: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.contains("\"fired\":true"))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        fired,
        vec![fixture::CUT_FRAME as usize],
        "exactly the cut frame fired: {series}"
    );
}

/// The run-in fixture: every tile's embedding rotates by a
/// growing step each frame, so the raw score climbs
/// 0.021, 0.042, ... 0.231 — through the near-miss band
/// (>= 0.20) and toward the threshold (0.25) without ever
/// reaching it. (The step size keeps every frame clear of
/// both edges: the band crossing must be a property of the
/// construction, not of float noise at the boundary.)
/// Nothing may publish; the sidecar must show the whole
/// climb, frame by frame, in the gate's own numbers.
/// Twelve frames: a baseline plus eleven steps.
#[test]
fn sub_threshold_run_in_is_recorded_frame_by_frame() {
    let dir = tempdir("series-runin");
    let ledger_dir = format!("{dir}/ledger");
    let series_path = format!("{dir}/series.jsonl");
    let mut ledger = Ledger::open(
        &ledger_dir,
        "runin",
        0.25,
        0.1,
        false,
        Grid::default(),
        "test-emb@1",
    )
    .unwrap();
    ledger.enable_score_series(&series_path).unwrap();

    let dim = 128;
    let mut live = LivePipeline::new(dim, PipeConfig::default());
    let mut seq = 0u64;
    let mut angle = 0.0f64;
    let mut pipeline_max: Vec<Option<f64>> = Vec::new();
    for frame in 0..=11u32 {
        if frame > 0 {
            // The step that makes this frame's raw score
            // 0.021 * frame against the previous frame.
            angle += (1.0f64 - 0.021 * f64::from(frame)).acos();
        }
        let emb: Vec<f32> = {
            let mut v = vec![0.0f32; dim];
            v[0] = angle.cos() as f32;
            v[1] = angle.sin() as f32;
            v
        };
        let recs: Vec<InputRecord> = (0..16)
            .map(|tile| {
                seq += 1;
                InputRecord {
                    seq,
                    frame,
                    tile,
                    t_arrival_us: 0,
                    diff_score: 20.0,
                    embedding: emb.clone(),
                }
            })
            .collect();
        let out = live.process_frame(frame, recs).unwrap();
        pipeline_max.push(
            out.tiles
                .iter()
                .filter_map(|tv| tv.smoothed_score)
                .fold(None::<f64>, |acc, s| Some(acc.map_or(s, |a: f64| a.max(s)))),
        );
        let emb_ms: Vec<f64> = (0..16).map(|_| 173.0).collect();
        ledger
            .record_frame(&out, u64::from(frame) * 500_000, 16, 16, &emb_ms, 0)
            .unwrap();
    }
    drop(ledger);

    // The ledger published nothing and counted the near
    // misses; the sidecar is where the climb lives.
    let events = std::fs::read_to_string(format!("{ledger_dir}/events.jsonl")).unwrap();
    assert!(
        !events.contains("\"kind\":\"verdict\""),
        "a run-in that never crosses must publish nothing: {events}"
    );
    let state = std::fs::read_to_string(format!("{ledger_dir}/state.txt")).unwrap();
    assert!(state.contains("near_miss_frames = 2"), "{state}");
    assert!(state.contains("max_smoothed_score = 0.231"), "{state}");

    let series = std::fs::read_to_string(&series_path).unwrap();
    let rows: Vec<&str> = series.lines().collect();
    assert_eq!(rows.len(), 12, "one row per evaluated frame: {series}");

    // Frame 0 is the baseline: no scores anywhere yet.
    assert_eq!(row_num(rows[0], "max_smoothed"), None, "{}", rows[0]);
    assert!(rows[0].contains("\"near_miss\":false"), "{}", rows[0]);

    let mut prev = 0.0f64;
    for (k, row) in rows.iter().enumerate().skip(1) {
        let got = row_num(row, "max_smoothed").expect("scored frame has a max");
        let want = 0.021 * k as f64;
        assert!(
            (got - want).abs() < 0.005,
            "frame {k}: sidecar max {got} vs constructed climb {want}: {row}"
        );
        // The sidecar's value is the gate's own value for
        // the frame, at the row's stated precision.
        let gate = pipeline_max[k].expect("pipeline scored the frame");
        assert!(
            (got - gate).abs() < 5.1e-5,
            "frame {k}: sidecar {got} vs pipeline {gate}"
        );
        assert!(got >= prev, "the climb is monotonic: frame {k}");
        prev = got;
        assert!(row.contains("\"fired\":false"), "frame {k}: {row}");
        assert!(
            row.contains("\"gate_diff_max\":20.0000"),
            "frame {k}: {row}"
        );
        // 16 tile scores ride along.
        let tiles_at = row.find("\"tiles\":[").unwrap();
        assert_eq!(
            row[tiles_at..].matches(',').count(),
            15,
            "frame {k}: 16 tile scores: {row}"
        );
    }
    // The band crossings are flagged where they happen:
    // frames 10 and 11 sit at 0.210 and 0.231 — inside the
    // near-miss band — and no earlier frame does.
    for (k, row) in rows.iter().enumerate() {
        let want_near = k >= 10;
        assert_eq!(
            row.contains("\"near_miss\":true"),
            want_near,
            "frame {k} near-miss flag: {row}"
        );
    }
}
