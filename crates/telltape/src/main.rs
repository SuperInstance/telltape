//! telltape CLI: `telltape watch --config <file>`.
//!
//! `watch` is the verb this build ships (live source -> ledger);
//! the bare form `telltape --config <file>` is accepted as the
//! same thing. The decided verb set is watch / index / query
//! (naming record in BUILD-LOG.md); index and query are not in
//! this build and are refused by name rather than ignored.
//! Prints a one-line JSON summary of the run to stdout.

use telltape::config::load_config;
use telltape::service;

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(first) = args.first() {
        match first.as_str() {
            "watch" => {
                args.remove(0);
            }
            "index" | "query" => {
                eprintln!(
                    "telltape: `{first}` is a decided verb but is not in this build yet; only `watch` is"
                );
                std::process::exit(2);
            }
            "-h" | "--help" => {
                eprintln!("usage: telltape watch --config <file>");
                std::process::exit(0);
            }
            _ => {}
        }
    }
    let cfg_path = args
        .windows(2)
        .find(|w| w[0] == "--config")
        .map(|w| w[1].clone())
        .unwrap_or_else(|| "telltape.conf".to_string());
    let cfg = load_config(&cfg_path).unwrap_or_else(|e| {
        eprintln!("config: {e}");
        std::process::exit(2);
    });
    match service::run(&cfg) {
        Ok(s) => {
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
        Err(e) => {
            eprintln!("telltape: {e}");
            std::process::exit(1);
        }
    }
}
