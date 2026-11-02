//! telltape CLI: `telltape watch --config <file>` and
//! `telltape index <footage> --config <file>`.
//!
//! `watch` runs a live source into a ledger; `index` runs
//! existing footage (a video file, an image sequence, a still)
//! through the identical pipeline into the same ledger format.
//! The bare form `telltape --config <file>` is accepted as
//! `watch`. The decided verb set is watch / index / query
//! (naming record in BUILD-LOG.md); `query` is not in this build
//! and is refused by name rather than ignored. Both built verbs
//! print a one-line JSON summary of the run to stdout.

use telltape::config::load_config;
use telltape::{index, service};

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
    let mut verb = "watch";
    if let Some(first) = args.first() {
        match first.as_str() {
            "watch" | "index" => {
                verb = if first == "index" { "index" } else { "watch" };
                args.remove(0);
            }
            "query" => {
                eprintln!(
                    "telltape: `query` is a decided verb but is not in this build yet; `watch` and `index` are"
                );
                std::process::exit(2);
            }
            "-h" | "--help" => {
                eprintln!(
                    "usage: telltape watch --config <file>\n       telltape index <footage> --config <file> [--fps N]"
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
