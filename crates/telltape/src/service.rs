//! The live service loop.
//!
//! Flow: [ingest command] --records + frame_end controls, JSONL-->
//! this loop --one frame at a time--> LivePipeline --> Ledger.
//!
//! Records are the exact wave-3 driver schema
//! (`harbor_watch::record::InputRecord`) plus an optional `embed_ms`
//! field the ingest stamps with the embedder call's measured latency.
//! Frame boundaries are control lines:
//! `{"control":"frame_end","frame":N,"t_capture_us":<epoch µs>,
//!   "nominated":K,"gate_diff":G,"frame_hash":H,"frame_repeat":B}`.
//! The hash/repeat stamps are how freshness is proven (gap G1):
//! a source that keeps delivering the same image is *frozen*, and
//! health says so — it never reads as meaningful silence.
//!
//! Embedder lifecycle (extraction surgery, plan §2.2): the origin
//! tool hardcoded the fleet's JEV rack — a systemctl start/stop of a
//! named systemd unit welded into this loop. Here the lifecycle is
//! two config strings (`embedder_start_cmd` / `embedder_stop_cmd`),
//! **empty by default**: start nothing, stop nothing, the box is
//! left exactly as found. If a health URL is configured, the run
//! refuses to start without a healthy embedder; if the service
//! started the embedder itself, it stops it on exit iff
//! `stop_embedder_on_exit` is set. With no health URL configured
//! (the ingest's in-process test double, or an embedder managed
//! elsewhere) no external check is possible and none is made.

use crate::config::{Config, ConfigError};
use crate::ledger::{epoch_us, Ledger};
use crate::live::LivePipeline;
use harbor_watch::pipeline::{Config as PipeConfig, PipelineError};
use harbor_watch::record::{parse_record, InputRecord};
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Why a service run failed (gap G2: typed at the crate's public
/// surface — no `Result<_, String>` remains).
#[derive(Debug)]
pub enum ServiceError {
    /// The ledger directory could not be opened or written.
    Ledger(std::io::Error),
    /// Bad configuration (including an invalid grid).
    Config(ConfigError),
    /// The configured embedder never became healthy.
    EmbedderUnhealthy {
        url: String,
        autostart: bool,
        start_cmd_set: bool,
    },
    /// The ingest command could not be spawned or read.
    Ingest(std::io::Error),
    /// The cascade refused a frame (bad record in the stream).
    Pipeline(PipelineError),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceError::Ledger(e) => write!(f, "ledger: {e}"),
            ServiceError::Config(e) => write!(f, "config: {e}"),
            ServiceError::EmbedderUnhealthy {
                url,
                autostart,
                start_cmd_set,
            } => write!(
                f,
                "embedder not healthy at {url} (embedder_autostart={autostart}, start_cmd {})",
                if *start_cmd_set { "set" } else { "unset" }
            ),
            ServiceError::Ingest(e) => write!(f, "ingest: {e}"),
            ServiceError::Pipeline(e) => write!(f, "pipeline: {e}"),
        }
    }
}
impl std::error::Error for ServiceError {}
impl From<std::io::Error> for ServiceError {
    fn from(e: std::io::Error) -> Self {
        ServiceError::Ledger(e)
    }
}
impl From<ConfigError> for ServiceError {
    fn from(e: ConfigError) -> Self {
        ServiceError::Config(e)
    }
}
impl From<PipelineError> for ServiceError {
    fn from(e: PipelineError) -> Self {
        ServiceError::Pipeline(e)
    }
}

#[derive(Debug, Default)]
pub struct Summary {
    pub frames: u64,
    pub events: u64,
    pub embeds: u64,
    pub embeds_spent_s: f64,
    pub embed_everything_estimate_s: f64,
    pub verdict_latency_p50_ms: f64,
    pub verdict_latency_max_ms: f64,
    pub embedder_started_by_me: bool,
    pub embedder_stopped_by_me: bool,
}

/// Extract a numeric field from a JSON line without a JSON parser
/// (same deliberately small approach as harbor-watch's record codec).
fn json_num(line: &str, key: &str) -> Option<f64> {
    let pat = format!("\"{key}\"");
    let at = line.find(&pat)?;
    let rest = &line[at + pat.len()..];
    let colon = rest.find(':')?;
    let rest = rest[colon + 1..].trim_start();
    let end = rest
        .find(|c: char| c == ',' || c == '}' || c.is_whitespace())
        .unwrap_or(rest.len());
    rest[..end].parse::<f64>().ok()
}

/// Extract a string field from a JSON line (same deliberately
/// small approach). Returns the unquoted contents.
fn json_str(line: &str, key: &str) -> Option<String> {
    let pat = format!("\"{key}\"");
    let at = line.find(&pat)?;
    let rest = &line[at + pat.len()..];
    let colon = rest.find(':')?;
    let rest = rest[colon + 1..].trim_start();
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Extract a boolean field from a JSON line.
fn json_bool(line: &str, key: &str) -> Option<bool> {
    let pat = format!("\"{key}\"");
    let at = line.find(&pat)?;
    let rest = &line[at + pat.len()..];
    let colon = rest.find(':')?;
    let rest = rest[colon + 1..].trim_start();
    if rest.starts_with("true") {
        Some(true)
    } else if rest.starts_with("false") {
        Some(false)
    } else {
        None
    }
}

/// Minimal HTTP/1.0 GET on a raw socket: enough for a health
/// endpoint. Any 200 response counts as healthy.
fn http_health(url: &str) -> bool {
    let stripped = url
        .trim_start_matches("http://")
        .trim_start_matches("https://");
    let (authority, path) = match stripped.find('/') {
        Some(i) => (&stripped[..i], &stripped[i..]),
        None => (stripped, "/"),
    };
    let Ok(mut sock) = TcpStream::connect(authority) else {
        return false;
    };
    let _ = sock.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = sock.set_write_timeout(Some(Duration::from_secs(3)));
    if sock
        .write_all(format!("GET {path} HTTP/1.0\r\nHost: x\r\n\r\n").as_bytes())
        .is_err()
    {
        return false;
    }
    let mut buf = String::new();
    let _ = sock.take(256).read_to_string(&mut buf);
    buf.starts_with("HTTP/1.1 200") || buf.starts_with("HTTP/1.0 200")
}

/// Run a lifecycle shell command; true iff it exited 0.
fn run_shell_cmd(cmd: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn run(cfg: &Config) -> Result<Summary, ServiceError> {
    let grid = cfg.grid()?;
    let mut ledger = Ledger::open(
        &cfg.ledger_dir,
        &cfg.stream_id,
        cfg.threshold,
        cfg.alpha,
        cfg.soak,
        grid,
    )?;
    ledger
        .service_event("start", &format!("stream {} starting", cfg.stream_id))
        .ok();

    // ---- embedder lifecycle: ensure up before ingest starts embedding
    let mut embedder_started_by_me = false;
    let health_url = cfg.health_url();
    let mut up = match &health_url {
        None => {
            // No external embedder to check (in-process adapter or
            // managed elsewhere): healthy by construction.
            ledger
                .service_event(
                    "embedder",
                    "no health url configured; embedder assumed in-process or managed elsewhere",
                )
                .ok();
            true
        }
        Some(u) => http_health(u),
    };
    if !up {
        let url = health_url.as_deref().unwrap_or("");
        if cfg.embedder_autostart && !cfg.embedder_start_cmd.is_empty() {
            eprintln!("[telltape] embedder down at startup; running embedder_start_cmd");
            ledger
                .service_event(
                    "embedder_start",
                    "embedder was down; started by this service",
                )
                .ok();
            if run_shell_cmd(&cfg.embedder_start_cmd) {
                for _ in 0..60 {
                    std::thread::sleep(Duration::from_secs(1));
                    if http_health(url) {
                        up = true;
                        break;
                    }
                }
                embedder_started_by_me = up;
            }
        }
    }
    ledger.embedder_up = up;
    ledger.embedder_started_by_me = embedder_started_by_me;
    ledger.write_all().ok();
    if !up {
        return Err(ServiceError::EmbedderUnhealthy {
            url: health_url.as_deref().unwrap_or("").to_string(),
            autostart: cfg.embedder_autostart,
            start_cmd_set: !cfg.embedder_start_cmd.is_empty(),
        });
    }

    // ---- spawn ingest (or read our own stdin)
    let mut child: Option<Child> = None;
    let mut reader: Box<dyn BufRead> = if cfg.ingest_cmd.is_empty() {
        Box::new(BufReader::new(std::io::stdin()))
    } else {
        let mut c = Command::new("sh")
            .arg("-c")
            .arg(&cfg.ingest_cmd)
            .stdout(Stdio::piped())
            .spawn()
            .map_err(ServiceError::Ingest)?;
        let out = c.stdout.take().expect("piped stdout");
        child = Some(c);
        Box::new(BufReader::new(out))
    };

    let pipe_cfg = PipeConfig {
        threshold: cfg.threshold,
        alpha: cfg.alpha,
        focus_k: cfg.focus_k,
        grid,
    };
    let mut pipeline: Option<LivePipeline> = None;
    let mut prev_frame_hash: Option<String> = None;
    let mut pending: Vec<InputRecord> = Vec::new();
    let mut pending_embed_ms: Vec<f64> = Vec::new();
    let mut last_frame_wall: Option<std::time::Instant> = None;
    let mut frames_processed: u64 = 0;
    let mut frames_received: u64 = 0;
    let mut verdict_latencies: Vec<f64> = Vec::new();
    let mut summary = Summary::default();

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).map_err(ServiceError::Ingest)?;
        if n == 0 {
            break; // ingest EOF: clean end of stream
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.contains("\"control\"") {
            if line.contains("\"frame_end\"") {
                let frame = json_num(line, "frame").unwrap_or(0.0) as u32;
                let t_capture_us = json_num(line, "t_capture_us").unwrap_or(0.0) as u64;
                let nominated = json_num(line, "nominated").unwrap_or(0.0) as u64;
                if let Some(g) = json_num(line, "gate_diff") {
                    ledger.gate_diff = Some(g);
                }
                frames_received += 1;
                // Freshness (gap G1): the ingest stamps each frame
                // with a hash of its image and a repeat flag. A
                // frame whose image is identical to the previous
                // one is a *repeat* — the poll succeeded but nothing
                // was seen. Trust the flag when present; fall back
                // to comparing hashes ourselves when it is not; a
                // frame with neither stamp (an old ingest) cannot
                // be judged and counts as novel, with health
                // reporting freshness = unknown until judged data
                // arrives.
                let frame_hash = json_str(line, "frame_hash");
                let repeated = match (
                    json_bool(line, "frame_repeat"),
                    &frame_hash,
                    &prev_frame_hash,
                ) {
                    (Some(r), _, _) => r,
                    (None, Some(h), Some(p)) => h == p,
                    _ => false,
                };
                if frame_hash.is_some() {
                    prev_frame_hash = frame_hash;
                }
                match ledger.observe_freshness(repeated) {
                    crate::ledger::FreshnessTransition::BecameFrozen => {
                        eprintln!("[telltape] source frozen: repeated frames, silence no longer meaningful");
                        ledger
                            .service_event(
                                "source_frozen",
                                "frame image stopped changing while polls continue; silence_meaningful = false",
                            )
                            .ok();
                    }
                    crate::ledger::FreshnessTransition::Recovered => {
                        eprintln!("[telltape] source live again: novel frames arriving");
                        ledger
                            .service_event(
                                "source_recovered",
                                "novel frames arriving again after a frozen stretch",
                            )
                            .ok();
                    }
                    crate::ledger::FreshnessTransition::None => {}
                }
                // Lens rhythm: wall interval between frame arrivals.
                let now = std::time::Instant::now();
                if let Some(prev) = last_frame_wall {
                    ledger.observe_period(now.duration_since(prev).as_secs_f64() * 1000.0);
                }
                last_frame_wall = Some(now);

                if pipeline.is_none() && !pending.is_empty() {
                    let dim = pending[0].embedding.len();
                    pipeline = Some(LivePipeline::new(dim, pipe_cfg.clone()));
                }
                if let Some(p) = pipeline.as_mut() {
                    let recs = std::mem::take(&mut pending);
                    let emb_ms = std::mem::take(&mut pending_embed_ms);
                    let embedded = recs.len() as u64;
                    let pending_now = frames_received - frames_processed - 1;
                    let out = p.process_frame(frame, recs)?;
                    if let Some(lat) = ledger.record_frame(
                        &out,
                        t_capture_us,
                        nominated,
                        embedded,
                        &emb_ms,
                        pending_now,
                    )? {
                        verdict_latencies.push(lat);
                    }
                } else {
                    // Frames with no records yet (pipeline not sized):
                    // count them so state stays honest.
                    ledger.stats.frames_seen += 1;
                    ledger.stats.tiles_nominated += nominated;
                    ledger.write_all().ok();
                }
                frames_processed += 1;

                if cfg.max_frames > 0 && frames_processed >= cfg.max_frames as u64 {
                    break;
                }
                if !cfg.stop_file.is_empty() && std::path::Path::new(&cfg.stop_file).exists() {
                    eprintln!("[telltape] stop file seen; shutting down");
                    break;
                }
            }
            continue;
        }
        match parse_record(line) {
            Ok(rec) => {
                if let Some(ms) = json_num(line, "embed_ms") {
                    pending_embed_ms.push(ms);
                }
                pending.push(rec);
            }
            Err(e) => {
                eprintln!("[telltape] skipping malformed line: {e}");
            }
        }
    }

    if let Some(mut c) = child {
        let _ = c.kill();
        let _ = c.wait();
    }

    summary.frames = ledger.stats.frames_seen;
    summary.events = ledger.stats.events_published;
    summary.embeds = ledger.stats.tiles_embedded;
    summary.embeds_spent_s = ledger.stats.embeds_spent_s;
    summary.embed_everything_estimate_s = ledger.stats.embed_everything_estimate_s();
    let (p50, _) = crate::ledger::percentiles(&verdict_latencies);
    summary.verdict_latency_p50_ms = p50;
    summary.verdict_latency_max_ms = verdict_latencies.iter().cloned().fold(0.0, f64::max);
    summary.embedder_started_by_me = embedder_started_by_me;

    // ---- embedder lifecycle: restore as found
    if embedder_started_by_me && cfg.stop_embedder_on_exit && !cfg.embedder_stop_cmd.is_empty() {
        eprintln!("[telltape] stopping embedder (this service started it)");
        ledger
            .service_event(
                "embedder_stop",
                "embedder stopped by this service (it started it)",
            )
            .ok();
        summary.embedder_stopped_by_me = run_shell_cmd(&cfg.embedder_stop_cmd);
        ledger.embedder_up = false;
    }
    ledger
        .service_event(
            "stop",
            &format!(
                "frames={} events={} embeds={}",
                summary.frames, summary.events, summary.embeds
            ),
        )
        .ok();
    ledger.write_all().ok();
    let _ = epoch_us();
    Ok(summary)
}
