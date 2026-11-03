//! Shared fixture ledgers for the reader verbs' tests
//! (`query`, `export`). One set of fixtures, both verbs held
//! to them: a filter or a row shape that changes under one
//! verb must fail the other's tests too.
//!
//! The fixtures are hand-written in the writer's own shapes
//! (lens/state key order and verdict-row field order copied
//! from ledger.rs). Times are 2026-11-02 in AKST (UTC-9):
//! 11:00:00 AKST = epoch 1793649600 s; the 11:44:48 verdict is
//! the presence receipt's instant, 1793652288.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::io::Write;
use telltape::query::Zone;

pub fn tempdir(tag: &str) -> String {
    let d = std::env::temp_dir().join(format!("telltape-reader-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.to_string_lossy().to_string()
}

pub fn akst() -> Zone {
    Zone::fixed(-9 * 3600, "AKST")
}

pub fn verdict_row(
    stream: &str,
    frame: u32,
    capture_us: u64,
    tiles: &[u32],
    embeds: u64,
) -> String {
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

pub fn service_row(event: &str, msg: &str, wall_us: u64) -> String {
    format!(
        "{{\"kind\":\"service\",\"event\":\"{event}\",\"msg\":\"{msg}\",\"wall_us\":{wall_us}}}"
    )
}

/// Fixture A — a known-identity ledger with a full story:
/// verdicts, a frozen stretch, an embedder swap, a stop/start
/// gap. Line order is append order.
pub fn fixture_a(tag: &str) -> String {
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

/// Every file in a directory, byte for byte — the read-only
/// proof's snapshot.
pub fn snapshot(dir: &str) -> BTreeMap<String, Vec<u8>> {
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

/// Run the built binary; returns (exit code, stdout, stderr).
pub fn run_bin(args: &[&str], envs: &[(&str, &str)]) -> (i32, String, String) {
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
