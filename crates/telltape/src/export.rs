//! The `export` verb: a ledger, as one portable document.
//!
//! `query` renders a ledger for a person reading in a
//! terminal. `export` renders the same ledger for *other
//! tools* — a spreadsheet, a jq pipeline, a video editor's
//! marker import — so nobody has to parse the ledger's text
//! formats to take the tells elsewhere. The ledger is the
//! index; export is how the index travels.
//!
//! One reader, two renderers: this module reads through
//! [`crate::query`]'s `read_ledger` and applies the same
//! filters with the same time semantics (verdicts on capture
//! time, service rows on write time, bounds inclusive). There
//! is no second parser to drift from the first.
//!
//! Fidelity:
//!
//! - **JSON** carries every field of every row: each row is
//!   embedded *verbatim*, the exact line the ledger holds, so
//!   fidelity is a property of the shared reader, not of a
//!   re-serialisation. Document metadata (stream, grid,
//!   embedder identity, exported and total counts, the
//!   malformed-row count with line numbers, the filters in
//!   force) wraps the two row arrays, `tells` and `service`,
//!   each in ledger order.
//! - **CSV** is the flat projection: one row per event in
//!   ledger order (verdicts and service in a single table, a
//!   `kind` column discriminating), scalar fields only — tile
//!   lists semicolon-joined, per-tile detail and stage
//!   timings stay in the JSON. Timestamps appear as epoch µs
//!   and as ISO 8601 UTC. A field containing a comma, quote,
//!   or newline is quoted, quotes doubled (RFC 4180).
//! - **Malformed rows** are counted in the JSON metadata and
//!   reported on stderr for CSV — counted, never silently
//!   dropped, under the same rule as `query`.
//! - **Read-only**, like `query`: the ledger is opened, never
//!   written. The document goes to stdout or to `--out`.

use crate::query::{
    json_tiles, json_token, json_u64, read_ledger, service_passes, verdict_passes, LedgerData,
    QueryError, QueryFilters, Row, ServiceRow, VerdictRow,
};

/// The export document's shape, chosen by `--format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Json,
    Csv,
}

impl ExportFormat {
    pub fn parse(value: &str) -> Option<ExportFormat> {
        match value {
            "json" => Some(ExportFormat::Json),
            "csv" => Some(ExportFormat::Csv),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            ExportFormat::Json => "json",
            ExportFormat::Csv => "csv",
        }
    }
}

/// The rendered document plus the counts behind it, for
/// callers (and tests) that want numbers without parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportReport {
    pub document: String,
    pub format: ExportFormat,
    pub verdicts: usize,
    pub service_rows: usize,
    /// Malformed rows in the ledger, file-wide (filters
    /// cannot apply to rows that do not parse).
    pub malformed: usize,
    pub malformed_lines: Vec<usize>,
    /// The basis every `verdict_latency_ms` value in the
    /// document is measured on (playtest-2 S5) — carried in
    /// the JSON metadata, and reported on stderr for CSV,
    /// whose header is a pinned format and cannot grow the
    /// column's unit.
    pub verdict_latency_basis: &'static str,
}

/// The basis of a ledger's `verdict_latency_ms` values, in
/// the watch summary's own vocabulary (playtest-2 S5): on a
/// live source the number is capture-to-ledger-write delay;
/// on recorded footage it is processing lag behind the
/// footage's own timeline — a batch job's 147,566 ms says
/// "the processor was 2.5 minutes behind the footage", not
/// "a verdict took 2.5 minutes". A bare CSV column of these
/// numbers, basis unstated, reads as the wrong one of those.
pub fn verdict_latency_basis(footage: bool) -> &'static str {
    if footage {
        "processing_lag_behind_footage_timeline"
    } else {
        "capture_to_ledger_write"
    }
}

/// Escape a string for JSON output — the metadata values
/// (directory, stream, identity) are operator- and
/// ledger-supplied, so they are escaped properly rather than
/// trusted. Row payloads are not escaped: they are embedded
/// verbatim, already JSON as the writer wrote them.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn json_opt_str(v: &Option<String>) -> String {
    match v {
        Some(s) => format!("\"{}\"", json_escape(s)),
        None => "null".to_string(),
    }
}

fn json_opt_u64(v: &Option<u64>) -> String {
    match v {
        Some(n) => n.to_string(),
        None => "null".to_string(),
    }
}

fn render_json(
    data: &LedgerData,
    dir: &str,
    filters: &QueryFilters,
    verdicts: &[&VerdictRow],
    service: &[&ServiceRow],
) -> String {
    let mut t = String::new();
    t.push_str("{\n");
    t.push_str("  \"format\": \"telltape-ledger-export\",\n");
    t.push_str("  \"version\": 1,\n");
    t.push_str(&format!("  \"ledger_dir\": \"{}\",\n", json_escape(dir)));
    t.push_str(&format!("  \"stream\": {},\n", json_opt_str(&data.stream)));
    t.push_str(&format!(
        "  \"grid\": {},\n",
        json_opt_str(&data.grid.as_ref().map(|g| g.to_string()))
    ));
    // The identity marker, intact: a recorded identity exports
    // as itself; a legacy ledger exports null with
    // `legacy_ledger` saying why — never an invented value.
    t.push_str(&format!(
        "  \"embedder_identity\": {},\n",
        json_opt_str(&data.identity)
    ));
    t.push_str(&format!("  \"legacy_ledger\": {},\n", data.legacy));
    // S5: the latency column's basis travels with the numbers.
    t.push_str(&format!(
        "  \"verdict_latency_basis\": \"{}\",\n",
        verdict_latency_basis(data.footage)
    ));
    t.push_str("  \"filters\": {\n");
    t.push_str(&format!(
        "    \"since_us\": {},\n",
        json_opt_u64(&filters.since_us)
    ));
    t.push_str(&format!(
        "    \"until_us\": {},\n",
        json_opt_u64(&filters.until_us)
    ));
    t.push_str(&format!(
        "    \"tile\": {},\n",
        filters.tile.map(|n| n.to_string()).unwrap_or("null".into())
    ));
    t.push_str(&format!(
        "    \"stream\": {}\n",
        json_opt_str(&filters.stream)
    ));
    t.push_str("  },\n");
    t.push_str("  \"counts\": {\n");
    t.push_str(&format!("    \"tells\": {},\n", verdicts.len()));
    t.push_str(&format!("    \"service\": {},\n", service.len()));
    t.push_str(&format!(
        "    \"tells_in_ledger\": {},\n",
        data.verdicts.len()
    ));
    t.push_str(&format!(
        "    \"service_in_ledger\": {},\n",
        data.service.len()
    ));
    t.push_str(&format!(
        "    \"malformed_rows\": {},\n",
        data.malformed.len()
    ));
    let lines: Vec<String> = data.malformed.iter().map(usize::to_string).collect();
    t.push_str(&format!(
        "    \"malformed_lines\": [{}]\n",
        lines.join(", ")
    ));
    t.push_str("  },\n");
    t.push_str("  \"tells\": [");
    if verdicts.is_empty() {
        t.push_str("],\n");
    } else {
        t.push('\n');
        for (i, v) in verdicts.iter().enumerate() {
            t.push_str("    ");
            t.push_str(&v.raw);
            if i + 1 < verdicts.len() {
                t.push(',');
            }
            t.push('\n');
        }
        t.push_str("  ],\n");
    }
    t.push_str("  \"service\": [");
    if service.is_empty() {
        t.push_str("]\n");
    } else {
        t.push('\n');
        for (i, s) in service.iter().enumerate() {
            t.push_str("    ");
            t.push_str(&s.raw);
            if i + 1 < service.len() {
                t.push(',');
            }
            t.push('\n');
        }
        t.push_str("  ]\n");
    }
    t.push_str("}\n");
    t
}

// ----------------------------------------------------------------
// CSV

/// The pinned header. A test holds this exact string; change
/// it only as a deliberate format change, in a version bump.
pub const CSV_HEADER: &str = "kind,stream,frame,wall_us,wall_iso,t_capture_us,t_capture_iso,event,msg,changed_tiles,focus,lineage_mask,verdict_latency_ms,embeds_this_frame,energy_before,energy_after";

/// One CSV field, quoted when it carries a comma, quote, or
/// line break (RFC 4180); quotes inside double.
fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

fn tile_list_flat(tiles: &[u32]) -> String {
    tiles
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(";")
}

fn csv_verdict(v: &VerdictRow) -> String {
    let wall_us = json_u64(&v.raw, "wall_us").unwrap_or(0);
    let fields: Vec<String> = vec![
        "verdict".to_string(),
        v.stream.clone(),
        v.frame.to_string(),
        wall_us.to_string(),
        crate::query::fmt_iso_us(wall_us),
        v.t_capture_us.to_string(),
        crate::query::fmt_iso_us(v.t_capture_us),
        String::new(),
        String::new(),
        tile_list_flat(&v.changed_tiles),
        tile_list_flat(&json_tiles(&v.raw, "focus").unwrap_or_default()),
        json_token(&v.raw, "lineage_mask").unwrap_or_default(),
        json_token(&v.raw, "verdict_latency_ms").unwrap_or_default(),
        v.embeds_this_frame.to_string(),
        json_token(&v.raw, "energy_before").unwrap_or_default(),
        json_token(&v.raw, "energy_after").unwrap_or_default(),
    ];
    fields
        .iter()
        .map(|f| csv_field(f))
        .collect::<Vec<_>>()
        .join(",")
}

fn csv_service(s: &ServiceRow, ledger_stream: &Option<String>) -> String {
    let fields: Vec<String> = vec![
        "service".to_string(),
        // Service rows carry no stream of their own; the
        // column holds the ledger's stream, their context.
        ledger_stream.clone().unwrap_or_default(),
        String::new(),
        s.wall_us.to_string(),
        crate::query::fmt_iso_us(s.wall_us),
        String::new(),
        String::new(),
        s.event.clone(),
        s.msg.clone(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    ];
    fields
        .iter()
        .map(|f| csv_field(f))
        .collect::<Vec<_>>()
        .join(",")
}

fn render_csv(data: &LedgerData, filters: &QueryFilters) -> (String, usize, usize) {
    let mut t = String::new();
    t.push_str(CSV_HEADER);
    t.push('\n');
    let mut verdicts = 0;
    let mut service_rows = 0;
    // Ledger order, both kinds in one table — the interleave
    // is the story a marker import wants.
    for row in &data.ordered {
        match row {
            Row::Verdict(v) if verdict_passes(v, filters) => {
                t.push_str(&csv_verdict(v));
                t.push('\n');
                verdicts += 1;
            }
            Row::Service(s) if service_passes(s, filters, &data.stream) => {
                t.push_str(&csv_service(s, &data.stream));
                t.push('\n');
                service_rows += 1;
            }
            _ => {}
        }
    }
    (t, verdicts, service_rows)
}

// ----------------------------------------------------------------
// The verb

/// Read the ledger at `dir` and render the export document.
/// Same reader, same filters, same errors as `query`; opens
/// every file read-only and writes nothing.
pub fn run_export(
    dir: &str,
    filters: &QueryFilters,
    format: ExportFormat,
) -> Result<ExportReport, QueryError> {
    let data = read_ledger(dir)?;
    let (document, verdicts, service_rows) = match format {
        ExportFormat::Json => {
            let verdicts: Vec<&VerdictRow> = data
                .verdicts
                .iter()
                .filter(|v| verdict_passes(v, filters))
                .collect();
            let service: Vec<&ServiceRow> = data
                .service
                .iter()
                .filter(|s| service_passes(s, filters, &data.stream))
                .collect();
            let doc = render_json(&data, dir, filters, &verdicts, &service);
            (doc, verdicts.len(), service.len())
        }
        ExportFormat::Csv => render_csv(&data, filters),
    };
    Ok(ExportReport {
        document,
        format,
        verdicts,
        service_rows,
        malformed: data.malformed.len(),
        malformed_lines: data.malformed.clone(),
        verdict_latency_basis: verdict_latency_basis(data.footage),
    })
}

// ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_escaping_covers_the_hostile_characters() {
        assert_eq!(json_escape("plain"), "plain");
        assert_eq!(json_escape("a\"b"), "a\\\"b");
        assert_eq!(json_escape("a\\b"), "a\\\\b");
        assert_eq!(json_escape("a\nb\tc"), "a\\nb\\tc");
        assert_eq!(json_escape("a\u{1}b"), "a\\u0001b");
    }

    #[test]
    fn csv_fields_quote_only_when_they_must() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("semi;colon"), "semi;colon");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_field("two\nlines"), "\"two\nlines\"");
    }

    #[test]
    fn format_names_parse_and_refuse() {
        assert_eq!(ExportFormat::parse("json"), Some(ExportFormat::Json));
        assert_eq!(ExportFormat::parse("csv"), Some(ExportFormat::Csv));
        assert_eq!(ExportFormat::parse("xml"), None);
        assert_eq!(ExportFormat::parse("JSON"), None);
    }
}
