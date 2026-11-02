//! `query` verb tests: fixture ledgers in temp dirs, read
//! through the library and through the built binary.
//!
//! The fixtures are hand-written in the writer's own shapes
//! (lens/state key order and verdict-row field order copied
//! from ledger.rs). Times are 2026-11-02 in AKST (UTC-9):
//! 11:00:00 AKST = epoch 1793649600 s; the 11:44:48 verdict is
//! the presence receipt's instant, 1793652288.

use std::collections::BTreeMap;
use std::io::Write;
use telltape::query::{run_query, QueryError, QueryFilters, Zone};

fn tempdir(tag: &str) -> String {
    let d = std::env::temp_dir().join(format!("telltape-query-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.to_string_lossy().to_string()
}

fn akst() -> Zone {
    Zone::fixed(-9 * 3600, "AKST")
}

fn verdict_row(stream: &str, frame: u32, capture_us: u64, tiles: &[u32], embeds: u64) -> String {
    let tiles_csv = tiles
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{{\"kind\":\"verdict\",\"stream\":\"{stream}\",\"frame\":{frame},\"wall_us\":{},\"t_capture_us\":{capture_us},\"verdict_latency_ms\":12.500,\"changed_tiles\":[{tiles_csv}],\"focus\":[{tiles_csv}],\"lineage_mask\":31,\"stage_ms\":{{\"perceive_decide\":1.000,\"direct\":0.500,\"refine\":0.250,\"publish\":0.100}},\"embeds_this_frame\":{embeds},\"energy_before\":0.5000,\"energy_after\":0.3000,\"tiles\":[]}}",
        capture_us + 12_500
    )
}

fn service_row(event: &str, msg: &str, wall_us: u64) -> String {
    format!(
        "{{\"kind\":\"service\",\"event\":\"{event}\",\"msg\":\"{msg}\",\"wall_us\":{wall_us}}}"
    )
}

/// Fixture A — a known-identity ledger with a full story:
/// verdicts, a frozen stretch, an embedder swap, a stop/start
/// gap. Line order is append order.
fn fixture_a(tag: &str) -> String {
    let dir = tempdir(tag);
    std::fs::write(
        format!("{dir}/lens.txt"),
        "stream_id = harbor\ngrid = 4x4\nembedder_identity = jev-rack@ijepa-1.0\nperiod_ms = 500.00\njitter_ms = 12.00\nthreshold = 0.2500\nalpha = 0.1000\nframes_observed = 300\ntile_score_ema = null null null null null null null null null null null null null null null null\n",
    )
    .unwrap();
    std::fs::write(
        format!("{dir}/state.txt"),
        "stream_id = harbor\nframes_seen = 300\nframes_novel = 290\nframes_repeated = 10\nfreshness = live\nframes_changed = 4\nevents_published = 4\nlast_frame = 260\ntiles_nominated = 900\ntiles_embedded = 120\ncoverage = 0.1333\nembeds_spent_s = 20.760\nembed_everything_estimate_s = 830.400\nspend_ratio = 0.0250\nembedder = up\nembedder_identity = jev-rack@ijepa-1.0\nembedder_started_by_me = false\nsoak = false\nupdated_wall_us = 1793658001000000\n",
    )
    .unwrap();
    let rows = [
        service_row("start", "stream harbor starting", 1_793_649_590_000_000),
        verdict_row("harbor", 100, 1_793_649_610_000_000, &[5, 6], 6),
        verdict_row("harbor", 140, 1_793_649_900_000_000, &[5], 4),
        verdict_row("harbor", 205, 1_793_652_288_000_000, &[11, 15], 6),
        service_row(
            "source_frozen",
            "frame image stopped changing while polls continue; silence_meaningful = false",
            1_793_653_260_000_000,
        ),
        service_row(
            "source_recovered",
            "novel frames arriving again after a frozen stretch",
            1_793_655_060_000_000,
        ),
        service_row(
            "embedder_swap",
            "embedder changed: jev-rack@ijepa-1.0 -> model-b@2; baselines and gate calibration reset (lens change); recalibration explicitly acknowledged by the operator",
            1_793_655_120_000_000,
        ),
        service_row("stop", "frames=300 events=4 embeds=120", 1_793_656_800_000_000),
        service_row("start", "stream harbor starting", 1_793_657_700_000_000),
        verdict_row("harbor", 260, 1_793_658_000_000_000, &[15], 2),
    ];
    let mut f = std::fs::File::create(format!("{dir}/events.jsonl")).unwrap();
    for r in rows {
        writeln!(f, "{r}").unwrap();
    }
    dir
}

#[test]
fn default_listing_shows_tells_identity_and_the_service_story() {
    let dir = fixture_a("default");
    let report = run_query(&dir, &QueryFilters::default(), &akst()).unwrap();
    assert_eq!(report.verdicts, 4);
    assert_eq!(report.service_rows, 6);
    assert_eq!(report.malformed, 0);
    let t = &report.text;
    assert!(t.contains("embedder jev-rack@ijepa-1.0"), "{t}");
    assert!(t.contains("grid     4x4 (16 tiles)"), "{t}");
    assert!(
        t.contains("rows     4 verdicts, 6 service, 0 malformed"),
        "{t}"
    );
    // The 11:44:48 tell, in local time with its epoch and its
    // tiles read against the lens grid.
    assert!(t.contains("2026-11-02 11:44:48 AKST"), "{t}");
    assert!(t.contains("epoch_us 1793652288000000"), "{t}");
    assert!(t.contains("frame 205"), "{t}");
    assert!(t.contains("tiles [11, 15] (r2c3, r3c3)"), "{t}");
    assert!(t.contains("embeds 6"), "{t}");
    // Service events are part of the story, not a footnote.
    assert!(t.contains("source_frozen"), "{t}");
    assert!(t.contains("embedder_swap"), "{t}");
}

#[test]
fn filters_select_by_tile_stream_and_time() {
    let dir = fixture_a("filters");
    let by_tile = run_query(
        &dir,
        &QueryFilters {
            tile: Some(15),
            ..Default::default()
        },
        &akst(),
    )
    .unwrap();
    assert_eq!(by_tile.verdicts, 2, "{}", by_tile.text);
    assert!(by_tile.text.contains("frame 205"), "{}", by_tile.text);
    assert!(by_tile.text.contains("frame 260"), "{}", by_tile.text);
    // A tile filter cannot match service rows, and says nothing
    // for them: they are excluded, counted as zero.
    assert_eq!(by_tile.service_rows, 0);

    let wrong_stream = run_query(
        &dir,
        &QueryFilters {
            stream: Some("other".into()),
            ..Default::default()
        },
        &akst(),
    )
    .unwrap();
    assert_eq!(wrong_stream.verdicts, 0);
    assert_eq!(wrong_stream.service_rows, 0);
    assert!(
        wrong_stream
            .text
            .contains("(no verdicts under these filters)"),
        "{}",
        wrong_stream.text
    );

    // 11:30 AKST as an epoch bound and as naive ISO: same cut.
    let since_epoch = run_query(
        &dir,
        &QueryFilters {
            since_us: Some(1_793_651_400_000_000),
            ..Default::default()
        },
        &akst(),
    )
    .unwrap();
    assert_eq!(since_epoch.verdicts, 2);
    assert_eq!(since_epoch.service_rows, 5);
    let iso = telltape::query::parse_time_bound("--since", "2026-11-02T11:30", &akst()).unwrap();
    let since_iso = run_query(
        &dir,
        &QueryFilters {
            since_us: Some(iso),
            ..Default::default()
        },
        &akst(),
    )
    .unwrap();
    assert_eq!(since_iso.verdicts, since_epoch.verdicts);
    assert_eq!(since_iso.service_rows, since_epoch.service_rows);

    let until = run_query(
        &dir,
        &QueryFilters {
            until_us: Some(1_793_650_200_000_000), // 11:10 AKST
            ..Default::default()
        },
        &akst(),
    )
    .unwrap();
    assert_eq!(until.verdicts, 2);
    assert_eq!(until.service_rows, 1, "only the 10:59:50 start");
}

#[test]
fn summary_counts_spend_stretches_and_swaps() {
    let dir = fixture_a("summary");
    let report = run_query(
        &dir,
        &QueryFilters {
            summary: true,
            ..Default::default()
        },
        &akst(),
    )
    .unwrap();
    let t = &report.text;
    assert!(t.contains("embeds in filter  18 calls"), "{t}");
    assert!(t.contains("2026-11-02 11:00 AKST  3"), "{t}");
    assert!(t.contains("2026-11-02 13:00 AKST  1"), "{t}");
    assert!(t.contains("tile 5 (r1c1)  2"), "{t}");
    assert!(t.contains("tile 15 (r3c3)  2"), "{t}");
    assert!(t.contains("tile 6 (r1c2)  1"), "{t}");
    assert!(t.contains("tile 11 (r2c3)  1"), "{t}");
    // The frozen stretch, paired and durated.
    assert!(
        t.contains("2026-11-02 12:01:00 AKST -> 2026-11-02 12:31:00 AKST  (30m 0s)"),
        "{t}"
    );
    // The stopped stretch likewise.
    assert!(
        t.contains("2026-11-02 13:00:00 AKST -> 2026-11-02 13:15:00 AKST  (15m 0s)"),
        "{t}"
    );
    assert!(t.contains("model-b@2"), "{t}");
    // The ledger's own totals, labelled unfiltered.
    assert!(t.contains("ledger totals (state.txt, unfiltered)"), "{t}");
    assert!(t.contains("embeds_spent_s = 20.760"), "{t}");
    assert!(t.contains("spend_ratio = 0.0250"), "{t}");
}

#[test]
fn legacy_ledger_reads_unknown_embedder() {
    let dir = tempdir("legacy");
    // The pre-0.1.1 lens shape: no embedder_identity line.
    std::fs::write(
        format!("{dir}/lens.txt"),
        "stream_id = s\ngrid = 4x4\nperiod_ms = unknown\nthreshold = 0.2500\nalpha = 0.1000\nframes_observed = 12\n",
    )
    .unwrap();
    std::fs::write(
        format!("{dir}/events.jsonl"),
        format!(
            "{}\n{}\n",
            service_row("start", "stream s starting", 1_793_649_590_000_000),
            verdict_row("s", 7, 1_793_652_288_000_000, &[5, 6], 2)
        ),
    )
    .unwrap();
    let report = run_query(&dir, &QueryFilters::default(), &akst()).unwrap();
    assert_eq!(report.verdicts, 1);
    assert!(
        report
            .text
            .contains("unknown embedder (pre-0.1.1 ledger: no identity recorded)"),
        "{}",
        report.text
    );
    // The golden row's tiles still render against the lens grid.
    assert!(
        report.text.contains("tiles [5, 6] (r1c1, r1c2)"),
        "{}",
        report.text
    );
}

#[test]
fn events_only_ledger_reads_without_a_lens() {
    let dir = tempdir("events-only");
    std::fs::write(
        format!("{dir}/events.jsonl"),
        format!(
            "{}\n{}\n",
            service_row("start", "stream s starting", 1_793_649_590_000_000),
            verdict_row("s", 3, 1_793_652_288_000_000, &[3], 1)
        ),
    )
    .unwrap();
    let report = run_query(&dir, &QueryFilters::default(), &akst()).unwrap();
    assert_eq!(report.verdicts, 1);
    assert!(
        report.text.contains("grid     not recorded (no lens.txt)"),
        "{}",
        report.text
    );
    assert!(report.text.contains("unknown embedder"), "{}", report.text);
    // No grid: tiles print as raw numbers, no invented geometry.
    assert!(
        report.text.contains("tiles [3]  embeds 1"),
        "{}",
        report.text
    );
    assert!(!report.text.contains("r0c3"), "{}", report.text);
}

#[test]
fn malformed_rows_are_counted_with_line_numbers() {
    let dir = tempdir("malformed");
    std::fs::write(
        format!("{dir}/lens.txt"),
        "stream_id = s\ngrid = 4x4\nembedder_identity = m@1\n",
    )
    .unwrap();
    let lines = [
        service_row("start", "stream s starting", 1_793_649_590_000_000), // line 1
        "this is not a row".to_string(),                                  // line 2
        verdict_row("s", 7, 1_793_652_288_000_000, &[5, 6], 2),           // line 3
        "{\"kind\":\"mystery\",\"wall_us\":1}".to_string(),               // line 4
        verdict_row("s", 8, 1_793_652_300_000_000, &[9], 1),              // line 5
    ];
    std::fs::write(format!("{dir}/events.jsonl"), lines.join("\n") + "\n").unwrap();
    let report = run_query(&dir, &QueryFilters::default(), &akst()).unwrap();
    assert_eq!(
        report.verdicts, 2,
        "the good rows still read: {}",
        report.text
    );
    assert_eq!(report.malformed, 2);
    assert!(
        report
            .text
            .contains("malformed rows: 2 file-wide (lines 2, 4)"),
        "{}",
        report.text
    );
}

fn snapshot(dir: &str) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(dir).unwrap() {
        let e = e.unwrap();
        out.insert(
            e.file_name().to_string_lossy().to_string(),
            std::fs::read(e.path()).unwrap(),
        );
    }
    out
}

#[test]
fn query_never_mutates_the_ledger() {
    let dir = fixture_a("readonly");
    let before = snapshot(&dir);
    let _ = run_query(&dir, &QueryFilters::default(), &akst()).unwrap();
    let _ = run_query(
        &dir,
        &QueryFilters {
            summary: true,
            ..Default::default()
        },
        &akst(),
    )
    .unwrap();
    assert_eq!(snapshot(&dir), before, "a query is a read, byte for byte");
}

#[test]
fn writer_round_trip_through_the_real_ledger() {
    use harbor_watch::pipeline::{FrameVerdict, TileVerdict};
    use harbor_watch::record::Grid;
    use telltape::ledger::Ledger;
    use telltape::live::{FrameOutput, StageMs};

    let dir = tempdir("roundtrip");
    let ledger_dir = format!("{dir}/ledger");
    {
        let mut l = Ledger::open(
            &ledger_dir,
            "roundtrip-stream",
            0.25,
            0.1,
            false,
            Grid::default(),
            "test-emb@1",
        )
        .unwrap();
        let tiles: Vec<TileVerdict> = (0..16)
            .map(|t| TileVerdict {
                frame: 7,
                tile: t,
                t_arrival_us: 0,
                driver_diff: None,
                raw_score: (t == 5 || t == 6).then_some(0.9),
                smoothed_score: (t == 5 || t == 6).then_some(0.8),
                changed_raw: t == 5 || t == 6,
                changed: t == 5 || t == 6,
                focus: t == 5 || t == 6,
                stale: !(t == 5 || t == 6),
                lineage_mask: if t == 5 || t == 6 { 31 } else { 0 },
            })
            .collect();
        let out = FrameOutput {
            verdict: FrameVerdict {
                frame: 7,
                changed: true,
                changed_tiles: vec![5, 6],
                focus: vec![5, 6],
                energy_before: 0.5,
                energy_after: 0.3,
                records_in_frame: 2,
            },
            tiles,
            stage_ms: StageMs::default(),
        };
        l.record_frame(&out, telltape::ledger::epoch_us(), 2, 2, &[100.0, 100.0], 0)
            .unwrap();
        l.service_event("source_frozen", "frame image stopped changing")
            .unwrap();
    }
    let before = snapshot(&ledger_dir);
    let report = run_query(&ledger_dir, &QueryFilters::default(), &Zone::utc()).unwrap();
    assert_eq!(report.verdicts, 1);
    assert!(
        report.text.contains("embedder test-emb@1"),
        "{}",
        report.text
    );
    assert!(
        report.text.contains("stream   roundtrip-stream"),
        "{}",
        report.text
    );
    assert!(
        report.text.contains("tiles [5, 6] (r1c1, r1c2)"),
        "{}",
        report.text
    );
    assert!(report.text.contains("source_frozen"), "{}", report.text);
    assert_eq!(snapshot(&ledger_dir), before);
}

#[test]
fn missing_and_non_ledgers_are_typed_errors() {
    match run_query("/nonexistent-ledger-dir", &QueryFilters::default(), &akst()) {
        Err(QueryError::NotFound(_)) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
    let empty = tempdir("empty");
    match run_query(&empty, &QueryFilters::default(), &akst()) {
        Err(QueryError::NotALedger(_)) => {}
        other => panic!("expected NotALedger, got {other:?}"),
    }
    let a_file = format!("{}/a-file", tempdir("afile"));
    std::fs::write(&a_file, "not a directory").unwrap();
    match run_query(&a_file, &QueryFilters::default(), &akst()) {
        Err(QueryError::NotFound(_)) => {}
        other => panic!("expected NotFound for a file, got {other:?}"),
    }
}

// ----------------------------------------------------------------
// The binary itself: exit codes and the export refusal.

fn run_bin(args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_telltape"));
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn binary_query_exit_codes_and_output() {
    let dir = fixture_a("bin");
    let (code, stdout, _) = run_bin(&["query", &dir], &[]);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("TELLS"), "{stdout}");
    assert!(stdout.contains("frame 205"), "{stdout}");
    let (code, stdout, _) = run_bin(&["query", &dir, "--summary"], &[]);
    assert_eq!(code, 0);
    assert!(stdout.contains("SUMMARY"), "{stdout}");
    let (code, _, stderr) = run_bin(&["query", "/nonexistent-ledger-dir"], &[]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("no such ledger directory"), "{stderr}");
    let (code, _, _) = run_bin(&["query", &dir, "--since", "banana"], &[]);
    assert_eq!(code, 2, "a bad time value is a usage error");
    let (code, _, _) = run_bin(&["query"], &[]);
    assert_eq!(code, 2, "a missing directory is a usage error");
    let (code, _, stderr) = run_bin(&["query", &dir, "--bogus"], &[]);
    assert_eq!(code, 2, "{stderr}");
}

#[test]
fn binary_query_uses_the_local_zone() {
    if !std::path::Path::new("/usr/share/zoneinfo/America/Anchorage").exists() {
        return; // zoneinfo not installed here; the library tests cover the zone math
    }
    let dir = fixture_a("bin-tz");
    let (code, stdout, _) = run_bin(&["query", &dir], &[("TZ", "America/Anchorage")]);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.contains("2026-11-02 11:44:48 AKST"), "{stdout}");
}

#[test]
fn binary_export_is_refused_by_name() {
    let (code, _, stderr) = run_bin(&["export"], &[]);
    assert_eq!(code, 2);
    assert!(stderr.contains("not built"), "{stderr}");
    assert!(stderr.contains("query"), "{stderr}");
}
