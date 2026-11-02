//! telltape CLI: `telltape watch --config <file>`,
//! `telltape index <footage> --config <file>`, and
//! `telltape query <ledger-dir> [filters]`.
//!
//! `watch` runs a live source into a ledger; `index` runs
//! existing footage (a video file, an image sequence, a still)
//! through the identical pipeline into the same ledger format.
//! The bare form `telltape --config <file>` is accepted as
//! `watch`. `query` reads an existing ledger — read-only, no
//! config — and prints its tells. The decided verb set is
//! watch / index / query / export (naming record in
//! BUILD-LOG.md); `export` is not in this build and is refused
//! by name rather than ignored. `watch` and `index` print a
//! one-line JSON summary of the run to stdout.

use telltape::config::load_config;
use telltape::query::{parse_time_bound, QueryError, QueryFilters, Zone};
use telltape::{index, query, service};

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

fn print_summary(s: &service::Summary) {
    println!(
        "{{\"frames\":{},\"events\":{},\"embeds\":{},\"embeds_spent_s\":{:.3},\"embed_everything_estimate_s\":{:.3},\"verdict_latency_p50_ms\":{:.3},\"verdict_latency_max_ms\":{:.3},\"embedder_started_by_me\":{},\"embedder_stopped_by_me\":{}}}",
        s.frames,
        s.events,
        s.embeds,
        s.embeds_spent_s,
        s.embed_everything_estimate_s,
        s.verdict_latency_p50_ms,
        s.verdict_latency_max_ms,
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
    // else: query takes a ledger directory, not a config, and
    // never sees the write verbs' flags; export is refused by
    // name (designed in the extraction plan, not built).
    if let Some(first) = args.first() {
        match first.as_str() {
            "query" => {
                args.remove(0);
                run_query_cli(&args);
            }
            "export" => {
                eprintln!(
                    "telltape: `export` is a designed verb but is not built yet; `query` is the ledger reader in this build"
                );
                std::process::exit(2);
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
                    "usage: telltape watch --config <file> [--recalibrate]\n       telltape index <footage> --config <file> [--fps N] [--recalibrate]\n       telltape query <ledger-dir> [--since T] [--until T] [--tile N] [--stream ID] [--summary]\n       (--recalibrate acknowledges an embedder swap against an existing ledger)"
                );
                std::process::exit(0);
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
            if a == "--config" || a == "--fps" {
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
