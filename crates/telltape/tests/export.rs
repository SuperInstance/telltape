//! `export` verb tests: the shared fixture ledgers (see
//! tests/common) read through the library and the built
//! binary, in both formats.
//!
//! The fidelity claim under test: JSON export embeds each
//! row *verbatim* — so "parses back to exactly the rows
//! query shows, field-for-field" is checked by demanding
//! every filtered source line appear in the document
//! exactly once, and filter parity is checked by running
//! the same filters through `query` and comparing counts.

mod common;

use common::{fixture_a, run_bin, service_row, snapshot, tempdir, verdict_row};
use telltape::export::{run_export, ExportFormat};
use telltape::query::{run_query, QueryError, QueryFilters};

fn ledger_lines(dir: &str) -> Vec<String> {
    std::fs::read_to_string(format!("{dir}/events.jsonl"))
        .unwrap()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Split one CSV line, honouring RFC 4180 quoting.
fn csv_cells(line: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => {
                cells.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    cells.push(cur);
    cells
}

#[test]
fn json_export_embeds_every_row_verbatim() {
    let dir = fixture_a("json-fidelity");
    let report = run_export(&dir, &QueryFilters::default(), ExportFormat::Json).unwrap();
    assert_eq!(report.verdicts, 4);
    assert_eq!(report.service_rows, 6);
    assert_eq!(report.malformed, 0);
    let doc = &report.document;
    // Metadata: the ledger described, the counts honest.
    assert!(
        doc.contains("\"format\": \"telltape-ledger-export\""),
        "{doc}"
    );
    assert!(doc.contains("\"stream\": \"harbor\""), "{doc}");
    assert!(doc.contains("\"grid\": \"4x4\""), "{doc}");
    assert!(
        doc.contains("\"embedder_identity\": \"jev-rack@ijepa-1.0\""),
        "{doc}"
    );
    assert!(doc.contains("\"legacy_ledger\": false"), "{doc}");
    assert!(doc.contains("\"tells\": 4"), "{doc}");
    assert!(doc.contains("\"service\": 6"), "{doc}");
    assert!(doc.contains("\"malformed_rows\": 0"), "{doc}");
    // Fidelity: every source line, exactly once, byte for byte.
    for line in ledger_lines(&dir) {
        assert_eq!(
            doc.matches(&line).count(),
            1,
            "row must appear verbatim exactly once: {line}\n{doc}"
        );
    }
}

#[test]
fn export_filters_match_query_exactly() {
    let dir = fixture_a("parity");
    let cases: Vec<QueryFilters> = vec![
        QueryFilters::default(),
        QueryFilters {
            tile: Some(15),
            ..Default::default()
        },
        QueryFilters {
            tile: Some(5),
            ..Default::default()
        },
        QueryFilters {
            stream: Some("other".into()),
            ..Default::default()
        },
        QueryFilters {
            stream: Some("harbor".into()),
            ..Default::default()
        },
        QueryFilters {
            since_us: Some(1_793_651_400_000_000),
            ..Default::default()
        },
        QueryFilters {
            until_us: Some(1_793_650_200_000_000),
            ..Default::default()
        },
        QueryFilters {
            since_us: Some(1_793_651_400_000_000),
            until_us: Some(1_793_656_000_000_000),
            tile: Some(15),
            ..Default::default()
        },
    ];
    for f in &cases {
        let q = run_query(&dir, f, &common::akst()).unwrap();
        for format in [ExportFormat::Json, ExportFormat::Csv] {
            let e = run_export(&dir, f, format).unwrap();
            assert_eq!(
                (e.verdicts, e.service_rows),
                (q.verdicts, q.service_rows),
                "{format:?} must select what query selects under {f:?}"
            );
        }
        // And the filtered JSON holds exactly the rows the
        // filter admits — no row rides along unfiltered.
        let e = run_export(&dir, f, ExportFormat::Json).unwrap();
        let present = ledger_lines(&dir)
            .iter()
            .filter(|l| e.document.contains(l.as_str()))
            .count();
        assert_eq!(present, e.verdicts + e.service_rows, "{f:?}");
    }
}

#[test]
fn csv_shape_is_pinned_and_cells_are_the_ledger() {
    let dir = fixture_a("csv");
    let report = run_export(&dir, &QueryFilters::default(), ExportFormat::Csv).unwrap();
    let doc = &report.document;
    let mut lines = doc.lines();
    assert_eq!(
        lines.next().unwrap(),
        "kind,stream,frame,wall_us,wall_iso,t_capture_us,t_capture_iso,event,msg,changed_tiles,focus,lineage_mask,verdict_latency_ms,embeds_this_frame,energy_before,energy_after",
        "the CSV header is a pinned format surface"
    );
    let rows: Vec<Vec<String>> = lines.map(csv_cells).collect();
    assert_eq!(rows.len(), 10, "one row per event: {doc}");
    for r in &rows {
        assert_eq!(r.len(), 16, "flat means 16 columns: {r:?}");
    }
    // Ledger order: the service `start` leads, as written.
    assert_eq!(rows[0][0], "service");
    assert_eq!(rows[0][1], "harbor");
    assert_eq!(rows[0][7], "start");
    assert_eq!(rows[0][3], "1793649590000000");
    assert_eq!(rows[0][4], "2026-11-02T19:59:50Z");
    // The 11:44:48 AKST tell (frame 205): epoch and ISO,
    // tiles semicolon-joined, float tokens at written precision.
    let v = rows.iter().find(|r| r[2] == "205").unwrap();
    assert_eq!(v[0], "verdict");
    assert_eq!(v[3], "1793652288012500");
    assert_eq!(v[4], "2026-11-02T20:44:48.0125Z");
    assert_eq!(v[5], "1793652288000000");
    assert_eq!(v[6], "2026-11-02T20:44:48Z");
    assert_eq!(v[9], "11;15");
    assert_eq!(v[10], "11;15");
    assert_eq!(v[11], "31");
    assert_eq!(v[12], "12.500");
    assert_eq!(v[13], "6");
    assert_eq!(v[14], "0.5000");
    assert_eq!(v[15], "0.3000");
}

#[test]
fn csv_quotes_fields_that_carry_commas() {
    let dir = tempdir("csv-comma");
    std::fs::write(
        format!("{dir}/lens.txt"),
        "stream_id = s\ngrid = 4x4\nembedder_identity = m@1\n",
    )
    .unwrap();
    std::fs::write(
        format!("{dir}/events.jsonl"),
        format!(
            "{}\n{}\n",
            service_row("note", "paused, then resumed", 1_793_649_590_000_000),
            verdict_row("s", 7, 1_793_652_288_000_000, &[5, 6], 2)
        ),
    )
    .unwrap();
    let report = run_export(&dir, &QueryFilters::default(), ExportFormat::Csv).unwrap();
    assert!(
        report.document.contains("\"paused, then resumed\""),
        "{}",
        report.document
    );
    // And the quoted row still parses back to 16 cells.
    let row = report
        .document
        .lines()
        .find(|l| l.contains("paused"))
        .unwrap();
    let cells = csv_cells(row);
    assert_eq!(cells.len(), 16);
    assert_eq!(cells[8], "paused, then resumed");
}

#[test]
fn export_never_mutates_the_ledger() {
    let dir = fixture_a("readonly");
    let before = snapshot(&dir);
    let _ = run_export(&dir, &QueryFilters::default(), ExportFormat::Json).unwrap();
    let _ = run_export(&dir, &QueryFilters::default(), ExportFormat::Csv).unwrap();
    assert_eq!(snapshot(&dir), before, "an export is a read, byte for byte");
}

#[test]
fn legacy_ledger_exports_its_unknown_identity() {
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
    let report = run_export(&dir, &QueryFilters::default(), ExportFormat::Json).unwrap();
    // The marker stays a marker: null identity, legacy said
    // aloud — never an invented embedder.
    assert!(
        report.document.contains("\"embedder_identity\": null"),
        "{}",
        report.document
    );
    assert!(
        report.document.contains("\"legacy_ledger\": true"),
        "{}",
        report.document
    );
    let csv = run_export(&dir, &QueryFilters::default(), ExportFormat::Csv).unwrap();
    assert_eq!(csv.verdicts, 1);
    assert_eq!(csv.service_rows, 1);
}

#[test]
fn malformed_rows_are_counted_in_both_formats() {
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
    let json = run_export(&dir, &QueryFilters::default(), ExportFormat::Json).unwrap();
    assert_eq!(json.malformed, 2);
    assert_eq!(json.malformed_lines, vec![2, 4]);
    assert!(
        json.document.contains("\"malformed_rows\": 2"),
        "{}",
        json.document
    );
    assert!(
        json.document.contains("\"malformed_lines\": [2, 4]"),
        "{}",
        json.document
    );
    let csv = run_export(&dir, &QueryFilters::default(), ExportFormat::Csv).unwrap();
    assert_eq!(csv.malformed, 2);
    assert_eq!(csv.verdicts, 2);
    assert_eq!(csv.service_rows, 1);
}

#[test]
fn missing_and_non_ledgers_are_typed_errors() {
    match run_export(
        "/nonexistent-ledger-dir",
        &QueryFilters::default(),
        ExportFormat::Json,
    ) {
        Err(QueryError::NotFound(_)) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
    let empty = tempdir("empty");
    match run_export(&empty, &QueryFilters::default(), ExportFormat::Csv) {
        Err(QueryError::NotALedger(_)) => {}
        other => panic!("expected NotALedger, got {other:?}"),
    }
}

// ----------------------------------------------------------------
// The binary itself.

#[test]
fn binary_export_json_csv_and_out_file() {
    let dir = fixture_a("bin");
    let (code, stdout, _) = run_bin(&["export", &dir], &[]);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.starts_with("{\n"), "{stdout}");
    assert!(stdout.contains("\"tells\""), "{stdout}");
    let (code, stdout, _) = run_bin(&["export", &dir, "--format", "csv"], &[]);
    assert_eq!(code, 0);
    assert!(stdout.starts_with("kind,stream,frame,"), "{stdout}");
    // --out: the document lands in the file, stdout stays
    // empty, the note goes to stderr.
    let out_path = format!("{}/../export-out.json", dir);
    let (code, stdout, stderr) = run_bin(&["export", &dir, "--out", &out_path], &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stdout.is_empty(), "stdout stays pure: {stdout}");
    assert!(
        stderr.contains("wrote 4 tells + 6 service rows"),
        "{stderr}"
    );
    let written = std::fs::read_to_string(&out_path).unwrap();
    assert!(written.contains("\"embedder_identity\": \"jev-rack@ijepa-1.0\""));
    // Filters ride along on the binary surface too.
    let (code, stdout, _) = run_bin(&["export", &dir, "--tile", "15"], &[]);
    assert_eq!(code, 0);
    assert!(stdout.contains("\"tells\": 2"), "{stdout}");
}

#[test]
fn binary_export_exit_codes() {
    let dir = fixture_a("bin-codes");
    let (code, _, stderr) = run_bin(&["export", "/nonexistent-ledger-dir"], &[]);
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("no such ledger directory"), "{stderr}");
    let (code, _, _) = run_bin(&["export"], &[]);
    assert_eq!(code, 2, "a missing directory is a usage error");
    let (code, _, stderr) = run_bin(&["export", &dir, "--format", "xml"], &[]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("want json or csv"), "{stderr}");
    let (code, _, _) = run_bin(&["export", &dir, "--since", "banana"], &[]);
    assert_eq!(code, 2, "a bad time value is a usage error");
    let (code, _, _) = run_bin(&["export", &dir, "--bogus"], &[]);
    assert_eq!(code, 2);
}

#[test]
fn binary_csv_export_reports_malformed_on_stderr() {
    let dir = tempdir("bin-malformed");
    std::fs::write(
        format!("{dir}/lens.txt"),
        "stream_id = s\ngrid = 4x4\nembedder_identity = m@1\n",
    )
    .unwrap();
    std::fs::write(
        format!("{dir}/events.jsonl"),
        format!(
            "{}\nnot a row\n",
            verdict_row("s", 7, 1_793_652_288_000_000, &[5, 6], 2)
        ),
    )
    .unwrap();
    let (code, stdout, stderr) = run_bin(&["export", &dir, "--format", "csv"], &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("1 malformed row(s)"), "{stderr}");
    assert!(
        !stdout.contains("malformed"),
        "the note stays off stdout: {stdout}"
    );
}

/// Playtest-2 S5: the `verdict_latency_ms` numbers travel
/// with their basis. The playtest exported a 2-second stills
/// job whose latency column read 147,566 ms — processing lag
/// behind the footage timeline, presented bare as if it were
/// verdict computation time. JSON carries the basis in its
/// metadata (footage and live each named); CSV, whose header
/// is a pinned format, states it on stderr and keeps stdout
/// byte-pure.
#[test]
fn latency_basis_travels_with_the_export() {
    // Live ledger (fixture A): capture-to-ledger-write.
    let dir = fixture_a("latency-live");
    let report = run_export(&dir, &QueryFilters::default(), ExportFormat::Json).unwrap();
    assert!(
        report
            .document
            .contains("\"verdict_latency_basis\": \"capture_to_ledger_write\""),
        "{}",
        report.document
    );
    assert_eq!(report.verdict_latency_basis, "capture_to_ledger_write");

    // Footage ledger: processing lag behind the footage
    // timeline — the basis the playtest's number actually had.
    let fdir = tempdir("latency-footage");
    std::fs::write(
        format!("{fdir}/lens.txt"),
        "stream_id = yard\ngrid = 4x4\nembedder_identity = double@1\nsource_kind = footage\ntime_anchor = file_mtime\nperiod_ms = unknown\njitter_ms = unknown\nthreshold = 0.2500\nalpha = 0.1000\nframes_observed = 20\n",
    )
    .unwrap();
    std::fs::write(
        format!("{fdir}/events.jsonl"),
        format!(
            "{}\n",
            verdict_row("yard", 12, 1_793_649_700_000_000, &[5, 6], 2)
        ),
    )
    .unwrap();
    let report = run_export(&fdir, &QueryFilters::default(), ExportFormat::Json).unwrap();
    assert!(
        report
            .document
            .contains("\"verdict_latency_basis\": \"processing_lag_behind_footage_timeline\""),
        "{}",
        report.document
    );

    // CSV: the header stays exactly the pinned string, the
    // basis is stated on stderr, and stdout carries neither.
    let (code, stdout, stderr) = run_bin(&["export", &dir, "--format", "csv"], &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(
        stderr.contains("verdict_latency_ms basis: capture_to_ledger_write"),
        "{stderr}"
    );
    assert_eq!(
        stdout.lines().next().unwrap_or_default(),
        "kind,stream,frame,wall_us,wall_iso,t_capture_us,t_capture_iso,event,msg,changed_tiles,focus,lineage_mask,verdict_latency_ms,embeds_this_frame,energy_before,energy_after",
        "the pinned header is unchanged: {stdout}"
    );
    assert!(
        !stdout.contains("basis"),
        "the note stays off stdout: {stdout}"
    );
}
