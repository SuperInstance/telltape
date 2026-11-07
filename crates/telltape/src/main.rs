//! telltape CLI: `telltape watch --config <file>`,
//! `telltape index <footage> --config <file>`,
//! `telltape query <ledger-dir> [filters]`, and
//! `telltape export <ledger-dir> [--format json|csv]`.
//!
//! `watch` runs a live source into a ledger; `index` runs
//! existing footage (a video file, an image sequence, a still)
//! through the identical pipeline into the same ledger format.
//! The bare form `telltape --config <file>` is accepted as
//! `watch`. `query` reads an existing ledger — read-only, no
//! config — and prints its tells. `export` renders the same
//! ledger as one portable document (JSON with full row
//! fidelity, or flat CSV) for other tools, through the same
//! reader and filters. The verb set is watch / index / query
//! / export (naming record in BUILD-LOG.md). `watch` and
//! `index` print a one-line JSON summary of the run to stdout.

use telltape::config::load_config;
use telltape::export::ExportFormat;
use telltape::query::{parse_time_bound, QueryError, QueryFilters, Zone};
use telltape::{export, index, query, service};

const QUERY_USAGE: &str = "usage: telltape query <ledger-dir> [--since T] [--until T] [--tile N] [--stream ID] [--summary]\n  T is an epoch (seconds, ms or µs, by magnitude) or ISO 8601 (2026-11-02, 2026-11-02T11:30:05, ...Z, ...-09:00; naive reads as local time)\n  verdicts filter on capture time; service rows on write time; bounds are inclusive";

/// The `query` verb: parse its own arguments (a ledger
/// directory, not a config) and print the rendered answer.
/// Exit codes follow the house pattern: 0 ok, 1 the ledger
/// could not be read, 2 usage errors and bad values.
fn run_query_cli(args: &[String]) -> ! {
    let zone = Zone::local();
    let mut filters = QueryFilters::default();
    let mut dir: Option<String> = None;
    let bound = |flag: &str, v: &str| -> u64 {
        parse_time_bound(flag, v, &zone).unwrap_or_else(|e| {
            eprintln!("telltape query: {e}");
            std::process::exit(2);
        })
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--summary" => filters.summary = true,
            "--since" | "--until" | "--tile" | "--stream" => {
                let Some(v) = it.next() else {
                    eprintln!("telltape query: {a} needs a value\n{QUERY_USAGE}");
                    std::process::exit(2);
                };
                match a.as_str() {
                    "--since" => filters.since_us = Some(bound("--since", v)),
                    "--until" => filters.until_us = Some(bound("--until", v)),
                    "--tile" => {
                        filters.tile = Some(v.parse::<u32>().unwrap_or_else(|_| {
                            eprintln!("telltape query: bad --tile value {v:?}: want a tile number");
                            std::process::exit(2);
                        }))
                    }
                    _ => filters.stream = Some(v.clone()),
                }
            }
            "-h" | "--help" => {
                eprintln!("{QUERY_USAGE}");
                std::process::exit(0);
            }
            _ if a.starts_with('-') => {
                eprintln!("telltape query: unknown flag {a}\n{QUERY_USAGE}");
                std::process::exit(2);
            }
            _ => {
                if dir.is_some() {
                    eprintln!("telltape query: more than one ledger directory\n{QUERY_USAGE}");
                    std::process::exit(2);
                }
                dir = Some(a.clone());
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("{QUERY_USAGE}");
        std::process::exit(2);
    };
    match query::run_query(&dir, &filters, &zone) {
        Ok(report) => {
            print!("{}", report.text);
            std::process::exit(0);
        }
        Err(e @ QueryError::BadValue { .. }) => {
            eprintln!("telltape query: {e}");
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("telltape query: {e}");
            std::process::exit(1);
        }
    }
}

const EXPORT_USAGE: &str = "usage: telltape export <ledger-dir> [--format json|csv] [--out FILE] [--since T] [--until T] [--tile N] [--stream ID]\n  T is an epoch (seconds, ms or µs, by magnitude) or ISO 8601 — the same bounds, filters and time semantics as `telltape query`\n  json (default) is one document with full row fidelity; csv is the flat projection, one row per event in ledger order\n  the document goes to stdout unless --out names a file; the ledger is only ever read";

/// The `export` verb: parse its own arguments (a ledger
/// directory, not a config) and emit the document. Exit
/// codes mirror `query`'s: 0 ok, 1 the ledger could not be
/// read (or the --out file could not be written), 2 usage
/// errors and bad values. Stdout carries the document and
/// nothing else; notes go to stderr.
fn run_export_cli(args: &[String]) -> ! {
    let zone = Zone::local();
    let mut filters = QueryFilters::default();
    let mut dir: Option<String> = None;
    let mut format = ExportFormat::Json;
    let mut out: Option<String> = None;
    let bound = |flag: &str, v: &str| -> u64 {
        parse_time_bound(flag, v, &zone).unwrap_or_else(|e| {
            eprintln!("telltape export: {e}");
            std::process::exit(2);
        })
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--format" | "--out" | "--since" | "--until" | "--tile" | "--stream" => {
                let Some(v) = it.next() else {
                    eprintln!("telltape export: {a} needs a value\n{EXPORT_USAGE}");
                    std::process::exit(2);
                };
                match a.as_str() {
                    "--format" => {
                        format = ExportFormat::parse(v).unwrap_or_else(|| {
                            eprintln!(
                                "telltape export: bad --format value {v:?}: want json or csv"
                            );
                            std::process::exit(2);
                        })
                    }
                    "--out" => out = Some(v.clone()),
                    "--since" => filters.since_us = Some(bound("--since", v)),
                    "--until" => filters.until_us = Some(bound("--until", v)),
                    "--tile" => {
                        filters.tile = Some(v.parse::<u32>().unwrap_or_else(|_| {
                            eprintln!(
                                "telltape export: bad --tile value {v:?}: want a tile number"
                            );
                            std::process::exit(2);
                        }))
                    }
                    _ => filters.stream = Some(v.clone()),
                }
            }
            "-h" | "--help" => {
                eprintln!("{EXPORT_USAGE}");
                std::process::exit(0);
            }
            _ if a.starts_with('-') => {
                eprintln!("telltape export: unknown flag {a}\n{EXPORT_USAGE}");
                std::process::exit(2);
            }
            _ => {
                if dir.is_some() {
                    eprintln!("telltape export: more than one ledger directory\n{EXPORT_USAGE}");
                    std::process::exit(2);
                }
                dir = Some(a.clone());
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("{EXPORT_USAGE}");
        std::process::exit(2);
    };
    let report = match export::run_export(&dir, &filters, format) {
        Ok(r) => r,
        Err(e @ QueryError::BadValue { .. }) => {
            eprintln!("telltape export: {e}");
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("telltape export: {e}");
            std::process::exit(1);
        }
    };
    // For CSV the latency basis has no metadata home either:
    // the header is a pinned format, so the column's basis is
    // stated on stderr, where the malformed note already lives
    // (playtest-2 S5 — a bare 147,566 in the column reads as
    // verdict computation time; on footage it is processing
    // lag). Stdout stays pure CSV either way.
    if format == ExportFormat::Csv {
        eprintln!(
            "telltape export: note: verdict_latency_ms basis: {} (what the number measures depends on the source kind — see docs/LEDGER-FORMAT.md)",
            report.verdict_latency_basis
        );
    }
    // For CSV the malformed count has no metadata home in the
    // document, so it is reported on stderr — counted, never
    // silently dropped. (JSON carries it in its metadata.)
    if format == ExportFormat::Csv && report.malformed > 0 {
        let lines: Vec<String> = report
            .malformed_lines
            .iter()
            .take(10)
            .map(usize::to_string)
            .collect();
        eprintln!(
            "telltape export: note: {} malformed row(s) in the ledger (lines {}) are not in the document — they do not parse, and are counted rather than dropped silently",
            report.malformed,
            lines.join(", ")
        );
    }
    match out {
        Some(path) => {
            if let Err(e) = std::fs::write(&path, &report.document) {
                eprintln!("telltape export: writing {path}: {e}");
                std::process::exit(1);
            }
            eprintln!(
                "telltape export: wrote {} tells + {} service rows ({}) to {path}",
                report.verdicts,
                report.service_rows,
                format.name()
            );
            std::process::exit(0);
        }
        None => {
            print!("{}", report.document);
            std::process::exit(0);
        }
    }
}

fn print_summary(s: &service::Summary) {
    println!(
        "{{\"frames\":{},\"events\":{},\"embeds\":{},\"embeds_spent_s\":{:.3},\"embed_everything_estimate_s\":{:.3},\"verdict_latency_p50_ms\":{:.3},\"verdict_latency_max_ms\":{:.3},\"verdict_latency_basis\":\"{}\",\"embedder_started_by_me\":{},\"embedder_stopped_by_me\":{}}}",
        s.frames,
        s.events,
        s.embeds,
        s.embeds_spent_s,
        s.embed_everything_estimate_s,
        s.verdict_latency_p50_ms,
        s.verdict_latency_max_ms,
        s.verdict_latency_basis,
        s.embedder_started_by_me,
        s.embedder_stopped_by_me
    );
}

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|w| w[0] == flag)
        .map(|w| w[1].as_str())
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `query` and `export` are dispatched before everything
    // else: they take a ledger directory, not a config, and
    // never see the write verbs' flags.
    if let Some(first) = args.first() {
        match first.as_str() {
            "query" => {
                args.remove(0);
                run_query_cli(&args);
            }
            "export" => {
                args.remove(0);
                run_export_cli(&args);
            }
            _ => {}
        }
    }
    // --recalibrate: the operator's explicit acknowledgement of
    // an embedder swap against an existing ledger (deficit D1).
    // A bare flag, valid for both write verbs; stripped before
    // verb and positional parsing.
    let recalibrate_flag = args.iter().any(|a| a == "--recalibrate");
    args.retain(|a| a != "--recalibrate");
    let mut verb = "watch";
    if let Some(first) = args.first() {
        match first.as_str() {
            "watch" | "index" => {
                verb = if first == "index" { "index" } else { "watch" };
                args.remove(0);
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: telltape watch --config <file> [--recalibrate] [--score-series <path>]\n       telltape index <footage> --config <file> [--fps N] [--recalibrate] [--score-series <path>]\n       telltape query <ledger-dir> [--since T] [--until T] [--tile N] [--stream ID] [--summary]\n       telltape export <ledger-dir> [--format json|csv] [--out FILE] [--since T] [--until T] [--tile N] [--stream ID]\n       (--recalibrate acknowledges an embedder swap against an existing ledger;\n        --score-series writes one score row per evaluated frame to a JSONL sidecar — see docs/CONFIG.md)"
                );
                std::process::exit(0);
            }
            // Playtest F6: a bare word that is not a verb is a
            // usage error, named as one — it used to fall through
            // to `watch` and report a missing default config,
            // pointing the user at the wrong problem entirely.
            // (A leading flag is the documented bare watch form:
            // `telltape --config f` means watch.)
            _ if !first.starts_with('-') => {
                eprintln!(
                    "telltape: unknown verb {first:?} — the verbs are watch, index, query, export\nusage: telltape watch --config <file> [--recalibrate] [--score-series <path>]\n       telltape index <footage> --config <file> [--fps N] [--recalibrate] [--score-series <path>]\n       telltape query <ledger-dir> [--since T] [--until T] [--tile N] [--stream ID] [--summary]\n       telltape export <ledger-dir> [--format json|csv] [--out FILE] [--since T] [--until T] [--tile N] [--stream ID]"
                );
                std::process::exit(2);
            }
            _ => {}
        }
    }
    let cfg_path = arg_value(&args, "--config")
        .unwrap_or("telltape.conf")
        .to_string();
    let mut cfg = load_config(&cfg_path).unwrap_or_else(|e| {
        eprintln!("config: {e}");
        std::process::exit(2);
    });
    if recalibrate_flag {
        cfg.recalibrate = true;
    }
    // --score-series <path>: the v0.4.0 sidecar, as a CLI
    // override of the config key (same option, two surfaces,
    // like --recalibrate). A flag with no value after it is a
    // usage error, not a silently disabled instrument.
    if args.iter().any(|a| a == "--score-series") {
        match arg_value(&args, "--score-series") {
            Some(p) => cfg.score_series_path = p.to_string(),
            None => {
                eprintln!("telltape: --score-series needs a path");
                std::process::exit(2);
            }
        }
    }
    if verb == "index" {
        if let Some(fps) = arg_value(&args, "--fps") {
            match fps.parse::<f64>() {
                Ok(v) if v > 0.0 => cfg.index_fps = v,
                _ => {
                    eprintln!("telltape: bad --fps {fps:?} (want a number > 0)");
                    std::process::exit(2);
                }
            }
        }
        // The source is the one positional: anything that is not
        // a flag or a flag's value.
        let mut source = None;
        let mut it = args.iter();
        while let Some(a) = it.next() {
            if a == "--config" || a == "--fps" || a == "--score-series" {
                it.next();
            } else if !a.starts_with('-') && source.is_none() {
                source = Some(a.clone());
            }
        }
        let Some(source) = source else {
            eprintln!("usage: telltape index <footage> --config <file> [--fps N]");
            std::process::exit(2);
        };
        match index::index_file(&cfg, &source) {
            Ok(s) => print_summary(&s),
            Err(e) => {
                eprintln!("telltape: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    match service::run(&cfg) {
        Ok(s) => print_summary(&s),
        Err(e) => {
            eprintln!("telltape: {e}");
            std::process::exit(1);
        }
    }
}
