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
//!
//! Embedder identity (deficit D1): before opening the ledger the
//! run probes the identity the ledger recorded. A different
//! configured identity — same width or not — is a typed refusal
//! unless the operator recalibrates (`recalibrate` /
//! `--recalibrate`), in which case the swap is receipted in the
//! ledger's own event stream and the run starts from fresh
//! baselines, exactly as a lens change would.
//!
//! Degenerate embeddings (playtest F1): the cascade refuses any
//! record whose embedding cannot carry a score (zero norm,
//! non-finite), and this loop receipts that refusal as a
//! `degenerate_embedder` service event — frame and tile named —
//! before returning the typed error. A broken embedder stops
//! the watch; it never gets to author a confident ledger.
//!
//! Collapsed embeddings (playtest-2 B1): a subtler failure —
//! the embedder answers promptly, at the right width, with
//! finite unit vectors that are the *same* for every input.
//! Every per-vector check passes and every score is 0, so the
//! run completes in stamped-meaningful silence over real
//! change. The invariant this loop enforces: **silence may
//! only be stamped meaningful when the embedder has
//! demonstrated discrimination on this run's actual inputs.**
//! The ingest's probe demonstrates it synthetically (black vs
//! white) before the run; the variance watch here is the
//! run-time half — it tracks the relationship between input
//! variation (the gate's per-tile diffs) and embedding
//! variation (the decide scores) and, when materially varied
//! inputs have *never* moved an embedding past a trip count
//! of observations, receipts a `collapsed_embedder` event,
//! stamps the embedder `collapsed` in health, withdraws the
//! silence stamp, and stops the run (see [`CollapseWatch`]
//! for the thresholds and the reasoning). A uniformly blank
//! scene cannot trip it: the trigger is input variation
//! *without* embedding variation, never low variance alone.
//!
//! Transport failures (playtest-2 S4): when an embedder call
//! fails mid-run the ingest classifies it (transport /
//! response) on an `ingest_error` control line before exiting;
//! this loop receipts it as an `embedder_error` service event
//! and closes the run as aborted — health is never left
//! claiming the embedder up and the silence meaningful over
//! an unreceipted abort, and no raw traceback reaches the user.

use crate::config::{Config, ConfigError};
use crate::ledger::{epoch_us, Ledger, RecordedIdentity};
use crate::live::{FrameOutput, LivePipeline};
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
    /// The ledger on disk was written under a different
    /// embedder identity than this run's config declares
    /// (deficit D1): a swap — or a pre-0.1.1 ledger that records
    /// no identity at all. The width guard cannot see this: two
    /// embedders of the same width still live in different
    /// spaces, related by at best a rotation, so the old
    /// baselines and gate calibration do not transfer. Refused
    /// unless the operator explicitly recalibrates.
    EmbedderMismatch {
        /// The identity the ledger recorded (or the legacy
        /// explanation, verbatim).
        recorded: String,
        /// The identity this run's config declares.
        configured: String,
    },
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
    /// The embedder collapsed (playtest-2 B1): it kept
    /// answering — right width, finite, non-zero — but its
    /// answers did not discriminate this run's inputs:
    /// `observations` tile observations whose pixels changed
    /// materially produced embeddings that did not move, and
    /// no materially varied input ever moved one. The run is
    /// stopped and the stop receipted, on the degenerate
    /// path's precedent: continuing would keep writing a
    /// ledger whose silence is stamped meaningful by an
    /// embedder that cannot see — and the frames already
    /// written cannot be told apart from a genuinely quiet
    /// scene, which is exactly why the run must not go on
    /// lending them its stamp.
    EmbedderCollapsed {
        /// Varied-but-unmoved tile observations counted.
        observations: u64,
    },
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceError::Ledger(e) => write!(f, "ledger: {e}"),
            ServiceError::Config(e) => write!(f, "config: {e}"),
            ServiceError::EmbedderMismatch {
                recorded,
                configured,
            } => write!(
                f,
                "embedder identity mismatch: this ledger was written by embedder \
                 '{recorded}' but the configured embedder is '{configured}'. A swap \
                 is a lens change — the old baselines and gate calibration do not \
                 transfer. Refusing to continue; to accept the swap and recalibrate, \
                 set recalibrate = true in the config (or pass --recalibrate)"
            ),
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
            ServiceError::EmbedderCollapsed { observations } => write!(
                f,
                "embedder collapsed: {observations} tile observations whose pixels \
                 changed materially (diff >= {VARIED_DIFF_MIN}) produced embeddings \
                 that did not move (score <= {UNMOVED_SCORE_MAX}), and no varied \
                 input ever moved an embedding — the embedder is answering, but \
                 its answers do not discriminate this run's inputs, so its silence \
                 would mean nothing. Run stopped and receipted as \
                 collapsed_embedder; check the embedder service (a constant or \
                 near-constant response is the usual cause)"
            ),
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
    /// What the latency numbers above measure (playtest F4).
    /// For a live run, capture → ledger write is detection
    /// latency: `capture_to_ledger_write`. For an index run the
    /// capture times are the footage's own timeline, so the
    /// same arithmetic measures how far behind that timeline
    /// the indexer was when it wrote the verdict:
    /// `processing_lag_behind_footage_timeline`. The ledger's
    /// field keeps its name (format compatibility); the label
    /// lives here, at presentation.
    pub verdict_latency_basis: &'static str,
    pub embedder_started_by_me: bool,
    pub embedder_stopped_by_me: bool,
}

/// A tile's pixels "varied materially" at this mean-abs diff
/// (0..=255, the ingest gate's own units). The gate nominates
/// at `gate_diff` (default 3.0); 10.0 is more than 3× that —
/// a change the pixel layer is sure about. In the measured
/// record: staged cut tiles diff ~64, the object tiles ~20–60,
/// the drift the gate must ignore ~1.2 per frame. A tile whose
/// diff never reaches this line is not evidence about the
/// embedder either way.
pub const VARIED_DIFF_MIN: f64 = 10.0;

/// An embedding "did not move" at or below this cosine
/// distance. Identical vectors score exactly 0; a
/// near-constant server's float32 jitter scores ≲1e-3; the
/// weakest honest geometry in the record (cold playtest 2's
/// luminance embedder) scored ≥0.02 on every real change it
/// was shown. 0.005 sits between jitter and signal.
pub const UNMOVED_SCORE_MAX: f64 = 0.005;

/// Varied-but-unmoved observations that trip the watch. On
/// the default 4×4 grid this is three quarters of a full
/// frame changing at once, or the same evidence spread over
/// several frames — far beyond one tile's repeated subtlety
/// (a single tile whose change a real embedder legitimately
/// scores near zero can contribute at most one observation
/// per frame, and exoneration needs only one moved varied
/// input anywhere in the run).
pub const COLLAPSE_TRIP_OBSERVATIONS: u64 = 12;

/// The run-time variance watch (playtest-2 B1): fed each
/// processed frame's tile verdicts, it tracks the relationship
/// between input variation and embedding variation.
///
/// - A tile observation is *varied* when its driver diff (the
///   gate's mean-abs pixel diff for this frame) reaches
///   [`VARIED_DIFF_MIN`], and *moved* when its raw score (the
///   decide stage's cosine distance from the tile's previous
///   embedding) exceeds [`UNMOVED_SCORE_MAX`].
/// - One moved varied observation *demonstrates* the embedder
///   for the whole run: it can, in fact, tell this run's
///   inputs apart. The watch then stands down permanently —
///   demonstration is the invariant's currency, and it is not
///   un-earned by later quiet.
/// - Varied-but-unmoved observations accumulate. At
///   [`COLLAPSE_TRIP_OBSERVATIONS`] with no demonstration,
///   the watch trips: for this run, on this run's actual
///   inputs, the embedder has answered materially different
///   tiles with indistinguishable vectors every single time.
///
/// A uniformly blank scene produces no varied observations
/// at all, so the watch cannot trip on quiet — the trigger is
/// variation without response, never stillness itself.
#[derive(Debug, Default)]
pub struct CollapseWatch {
    varied_unmoved: u64,
    demonstrated: bool,
}

impl CollapseWatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one processed frame's output.
    pub fn observe(&mut self, out: &FrameOutput) {
        for tv in &out.tiles {
            if tv.stale {
                continue;
            }
            let (Some(score), Some(diff)) = (tv.raw_score, tv.driver_diff) else {
                continue;
            };
            if diff >= VARIED_DIFF_MIN {
                if score > UNMOVED_SCORE_MAX {
                    self.demonstrated = true;
                } else {
                    self.varied_unmoved += 1;
                }
            }
        }
    }

    /// Whether the watch has tripped: enough varied inputs
    /// went unanswered, and none was ever answered.
    pub fn tripped(&self) -> bool {
        !self.demonstrated && self.varied_unmoved >= COLLAPSE_TRIP_OBSERVATIONS
    }

    /// Varied-but-unmoved observations counted so far.
    pub fn observations(&self) -> u64 {
        self.varied_unmoved
    }

    /// Whether discrimination has been demonstrated on this
    /// run's actual inputs.
    pub fn demonstrated(&self) -> bool {
        self.demonstrated
    }
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

/// How a pre-0.1.1 ledger's missing identity is named in
/// refusals and swap receipts: unknown, with the reason attached
/// — never a guessed name.
const LEGACY_IDENTITY: &str = "unknown (pre-0.1.1 ledger: no embedder identity recorded)";

pub fn run(cfg: &Config) -> Result<Summary, ServiceError> {
    let grid = cfg.grid()?;
    // ---- embedder identity guard (deficit D1), BEFORE opening:
    // opening rewrites the lens, so the recorded identity is
    // probed first. A mismatch — including a legacy ledger whose
    // embedder is simply unknown — is a refusal unless the
    // operator explicitly recalibrates; a swap is a lens change
    // and silently continuing on the old baselines must be
    // impossible.
    let configured_identity = cfg.run_identity();
    let recalibrated_from: Option<String> = match Ledger::probe_identity(&cfg.ledger_dir) {
        RecordedIdentity::New => None,
        RecordedIdentity::Known(id) if id == configured_identity => None,
        RecordedIdentity::Known(id) => {
            if cfg.recalibrate {
                Some(id)
            } else {
                return Err(ServiceError::EmbedderMismatch {
                    recorded: id,
                    configured: configured_identity,
                });
            }
        }
        RecordedIdentity::Legacy => {
            if cfg.recalibrate {
                Some(LEGACY_IDENTITY.to_string())
            } else {
                return Err(ServiceError::EmbedderMismatch {
                    recorded: LEGACY_IDENTITY.to_string(),
                    configured: configured_identity,
                });
            }
        }
    };
    let mut ledger = Ledger::open(
        &cfg.ledger_dir,
        &cfg.stream_id,
        cfg.threshold,
        cfg.alpha,
        cfg.soak,
        grid,
        &configured_identity,
    )?;
    // Score series (v0.4.0): enable the sidecar before the
    // first frame, so an unwritable path fails the run at
    // startup — never after a day of unrecorded frames. The
    // ledger's own files are untouched by the feature; the
    // enable is announced on stderr only, like the source's
    // frozen/recovered transitions.
    if !cfg.score_series_path.is_empty() {
        ledger.enable_score_series(&cfg.score_series_path)?;
        eprintln!(
            "[telltape] score series sidecar enabled: {}",
            cfg.score_series_path
        );
    }
    // Footage mode (playtest F3) and the time anchor (F8). The
    // ingest stamps the anchor it actually used on every
    // frame_end; until the first stamp arrives a footage ledger
    // says `unstamped` rather than guess, unless the operator
    // supplied the start epoch themselves.
    ledger.footage = cfg.source_footage;
    if cfg.source_footage {
        ledger.time_anchor = if cfg.index_start_us > 0 {
            "operator_start_us".to_string()
        } else {
            "unstamped".to_string()
        };
    }
    ledger
        .service_event("start", &format!("stream {} starting", cfg.stream_id))
        .ok();
    if let Some(old) = &recalibrated_from {
        eprintln!(
            "[telltape] embedder swap accepted by operator: {old} -> {configured_identity}; recalibrating"
        );
        ledger
            .service_event(
                "embedder_swap",
                &format!(
                    "embedder changed: {old} -> {configured_identity}; baselines and gate calibration reset (lens change); recalibration explicitly acknowledged by the operator"
                ),
            )
            .ok();
    }

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
    let mut anchor_stamped = false;
    let mut pending: Vec<InputRecord> = Vec::new();
    let mut pending_embed_ms: Vec<f64> = Vec::new();
    let mut last_frame_wall: Option<std::time::Instant> = None;
    let mut frames_processed: u64 = 0;
    let mut frames_received: u64 = 0;
    let mut verdict_latencies: Vec<f64> = Vec::new();
    let mut summary = Summary::default();
    // Playtest-2 B1: the variance watch over this run's frames.
    let mut watch = CollapseWatch::new();
    // Playtest-2 S4: the ingest's classified account of its own
    // failure, when it sent one (`ingest_error` control line):
    // (class, message) — receipted on arrival, and used to
    // enrich the error this run returns when the stream ends.
    let mut ingest_failure: Option<(String, String)> = None;
    let mut read_error: Option<std::io::Error> = None;

    let mut ended_by_eof = false;
    let mut line = String::new();
    loop {
        line.clear();
        let n = match reader.read_line(&mut line) {
            Ok(n) => n,
            Err(e) => {
                read_error = Some(e);
                break;
            }
        };
        if n == 0 {
            ended_by_eof = true;
            break; // ingest EOF: end of stream (its exit status is checked below)
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if line.contains("\"control\"") {
            if line.contains("\"ingest_error\"") {
                // Playtest-2 S4: the ingest classified its own
                // failure (an embedder call died mid-run) and is
                // on its way out. Receipt it NOW, in the ledger
                // itself — the failure is part of the run's
                // record, next to the tells it interrupted.
                let class = json_str(line, "class").unwrap_or_else(|| "internal".to_string());
                let msg = json_str(line, "msg").unwrap_or_default();
                ledger
                    .service_event(
                        "embedder_error",
                        &format!(
                            "embedder call failed ({class}): {msg}; run aborted — the tells before this point stand, and nothing after it was seen"
                        ),
                    )
                    .ok();
                ledger.run_aborted = true;
                ledger.write_all().ok();
                ingest_failure = Some((class, msg));
                continue;
            }
            if line.contains("\"frame_end\"") {
                let frame = json_num(line, "frame").unwrap_or(0.0) as u32;
                let t_capture_us = json_num(line, "t_capture_us").unwrap_or(0.0) as u64;
                let nominated = json_num(line, "nominated").unwrap_or(0.0) as u64;
                // Time anchor (playtest F8): the ingest stamps
                // how its capture times were derived; the first
                // stamp is the run's anchor and the lens records
                // it, so a reader knows what the tells' wall
                // times rest on.
                if !anchor_stamped {
                    if let Some(a) = json_str(line, "time_anchor") {
                        ledger.time_anchor = a;
                        anchor_stamped = true;
                    }
                }
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
                    let out = match p.process_frame(frame, recs) {
                        Ok(out) => out,
                        Err(e) => {
                            // Playtest F1: a degenerate embedding
                            // stops the run, and the stop is
                            // receipted in the ledger itself —
                            // the tell reader must be able to see
                            // why the tells end, with the frame
                            // and tile that carried the defect.
                            // Tells written before this frame
                            // stand; nothing is written from the
                            // degenerate vector. Any other
                            // pipeline refusal is receipted as a
                            // plain abort (playtest-2 S4's rule:
                            // a run's failure is part of its
                            // record, and health must not go on
                            // stamping the silence meaningful).
                            if let PipelineError::DegenerateEmbedding {
                                frame: df,
                                tile,
                                reason,
                                ..
                            } = e
                            {
                                ledger
                                    .service_event(
                                        "degenerate_embedder",
                                        &format!(
                                            "degenerate embedding refused at frame {df} tile {tile} ({reason}); run stopped — a broken embedder must not produce a confident ledger"
                                        ),
                                    )
                                    .ok();
                            } else {
                                ledger
                                    .service_event(
                                        "run_aborted",
                                        &format!(
                                            "pipeline refused a frame: {e}; run aborted — the tells before this point stand, and nothing after it was seen"
                                        ),
                                    )
                                    .ok();
                            }
                            ledger.run_aborted = true;
                            ledger.write_all().ok();
                            if let Some(mut c) = child.take() {
                                let _ = c.kill();
                                let _ = c.wait();
                            }
                            return Err(e.into());
                        }
                    };
                    // Playtest-2 B1: the variance watch. Fed
                    // before the frame is recorded — the frame
                    // that trips it is not written, on the
                    // degenerate path's precedent (the frame that
                    // proves the embedder broken does not get to
                    // author state), and the stop is receipted
                    // with the counts that convicted it.
                    watch.observe(&out);
                    if watch.tripped() {
                        let observations = watch.observations();
                        ledger
                            .service_event(
                                "collapsed_embedder",
                                &format!(
                                    "embedder collapsed: {observations} tile observations with materially changed pixels (diff >= {VARIED_DIFF_MIN}) produced embeddings that did not move (score <= {UNMOVED_SCORE_MAX}), and no varied input ever moved one; run stopped — an embedder that cannot discriminate this run's inputs must not stamp its silence meaningful"
                                ),
                            )
                            .ok();
                        ledger.embedder_collapsed = true;
                        ledger.run_aborted = true;
                        ledger.write_all().ok();
                        if let Some(mut c) = child.take() {
                            let _ = c.kill();
                            let _ = c.wait();
                        }
                        return Err(ServiceError::EmbedderCollapsed { observations });
                    }
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

    if let Some(e) = read_error {
        // The record stream itself failed to read. The run is
        // over and its end is receipted like any other abort
        // (playtest-2 S4): health must not be left claiming a
        // clean watch over a stream that broke.
        ledger
            .service_event(
                "run_aborted",
                &format!(
                    "ingest stream failed to read: {e}; run aborted — the tells before this point stand, and nothing after it was seen"
                ),
            )
            .ok();
        ledger.run_aborted = true;
        if frames_processed == 0 {
            ledger.embedder_up = false;
        }
        ledger.write_all().ok();
        if let Some(mut c) = child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
        return Err(ServiceError::Ingest(e));
    }

    if let Some(mut c) = child {
        if ended_by_eof {
            // The ingest ended the stream itself. Its exit status
            // is part of the truth of this run: an ingest that died
            // (bad flags, missing deps, a crashed embedder) must
            // not read as a clean, empty watch — a zero-frame
            // "success" is how broken installs hide. (When WE end
            // the run — max_frames / stop file — the child is
            // killed below and its status is meaningless.)
            match c.wait() {
                Ok(status) if !status.success() => {
                    // Playtest-2 S4 / W3: the abort is receipted
                    // and stamped before the error returns. If
                    // the ingest classified its own failure, its
                    // `embedder_error` receipt already stands and
                    // the returned error carries the class; an
                    // unclassified death gets the generic abort
                    // receipt. A run that never processed a frame
                    // never proved its embedder for this run —
                    // health must not stamp it `up` (the probe-
                    // refused ledgers of both playtests did).
                    if ingest_failure.is_none() {
                        ledger
                            .service_event(
                                "run_aborted",
                                &format!(
                                    "ingest exited with {status} after {frames_received} frames; run aborted — the tells before this point stand, and nothing after it was seen"
                                ),
                            )
                            .ok();
                    }
                    ledger.run_aborted = true;
                    if frames_processed == 0 {
                        ledger.embedder_up = false;
                    }
                    ledger.write_all().ok();
                    let detail = match &ingest_failure {
                        Some((class, msg)) => {
                            format!(" (embedder call failed ({class}): {msg})")
                        }
                        None => String::new(),
                    };
                    return Err(ServiceError::Ingest(std::io::Error::other(format!(
                        "ingest command exited with {status} after {frames_received} frames{detail}"
                    ))));
                }
                Ok(_) => {}
                Err(e) => return Err(ServiceError::Ingest(e)),
            }
        } else {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    summary.frames = ledger.stats.frames_seen;
    summary.events = ledger.stats.events_published;
    summary.embeds = ledger.stats.tiles_embedded;
    summary.embeds_spent_s = ledger.stats.embeds_spent_s;
    summary.embed_everything_estimate_s = ledger.stats.embed_everything_estimate_s();
    let (p50, _) = crate::ledger::percentiles(&verdict_latencies);
    summary.verdict_latency_p50_ms = p50;
    summary.verdict_latency_max_ms = verdict_latencies.iter().cloned().fold(0.0, f64::max);
    summary.verdict_latency_basis = if cfg.source_footage {
        "processing_lag_behind_footage_timeline"
    } else {
        "capture_to_ledger_write"
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    use harbor_watch::pipeline::{FrameVerdict, TileVerdict};

    fn tile(tile: u32, diff: Option<f64>, score: Option<f64>, stale: bool) -> TileVerdict {
        TileVerdict {
            frame: 1,
            tile,
            t_arrival_us: 0,
            driver_diff: diff,
            raw_score: score,
            smoothed_score: score,
            changed_raw: false,
            changed: false,
            focus: false,
            stale,
            lineage_mask: 0,
        }
    }

    fn frame(tiles: Vec<TileVerdict>) -> FrameOutput {
        FrameOutput {
            verdict: FrameVerdict {
                frame: 1,
                changed: false,
                changed_tiles: vec![],
                focus: vec![],
                energy_before: 0.0,
                energy_after: 0.0,
                records_in_frame: tiles.len(),
            },
            tiles,
            stage_ms: crate::live::StageMs::default(),
        }
    }

    /// Playtest-2 B1, the conviction: a full frame of tiles
    /// whose pixels changed materially and whose embeddings
    /// did not move at all trips the watch in one frame.
    #[test]
    fn varied_inputs_that_never_move_embeddings_trip_the_watch() {
        let mut w = CollapseWatch::new();
        w.observe(&frame(
            (0..16)
                .map(|t| tile(t, Some(64.0), Some(0.0), false))
                .collect(),
        ));
        assert!(w.tripped());
        assert_eq!(w.observations(), 16);
        assert!(!w.demonstrated());
    }

    /// The trip is a count across frames, not a per-frame
    /// verdict: partial change spread over frames accumulates.
    #[test]
    fn the_trip_count_accumulates_across_frames() {
        let mut w = CollapseWatch::new();
        for _ in 0..2 {
            w.observe(&frame(
                (0..5)
                    .map(|t| tile(t, Some(30.0), Some(0.001), false))
                    .collect(),
            ));
            assert!(!w.tripped(), "10 observations is not yet a conviction");
        }
        w.observe(&frame(
            (0..2)
                .map(|t| tile(t, Some(30.0), Some(0.0), false))
                .collect(),
        ));
        assert!(w.tripped(), "the 12th unmoved varied input trips it");
    }

    /// The honest blank wall: embeddings never vary AND
    /// inputs never vary. Nothing is counted; nothing trips;
    /// the run's silence keeps its meaning.
    #[test]
    fn a_uniformly_blank_scene_can_never_trip_it() {
        let mut w = CollapseWatch::new();
        for _ in 0..50 {
            w.observe(&frame(
                (0..16)
                    .map(|t| tile(t, Some(0.4), Some(0.0), false))
                    .collect(),
            ));
        }
        assert!(!w.tripped());
        assert_eq!(w.observations(), 0);
    }

    /// One varied input that moves an embedding demonstrates
    /// the embedder for the run — permanently. Later unmoved
    /// observations cannot un-demonstrate it.
    #[test]
    fn one_moved_varied_input_demonstrates_for_the_run() {
        let mut w = CollapseWatch::new();
        w.observe(&frame(vec![tile(3, Some(50.0), Some(0.4), false)]));
        assert!(w.demonstrated());
        w.observe(&frame(
            (0..16)
                .map(|t| tile(t, Some(64.0), Some(0.0), false))
                .collect(),
        ));
        assert!(!w.tripped());
    }

    /// Unscored and stale tiles are not evidence: frame 0's
    /// baselines (no score), tiles the frame did not refresh,
    /// and varied-but-unmeasured tiles count for nothing.
    #[test]
    fn unscored_and_stale_tiles_are_not_evidence() {
        let mut w = CollapseWatch::new();
        w.observe(&frame(
            (0..16).map(|t| tile(t, Some(64.0), None, false)).collect(),
        ));
        w.observe(&frame(
            (0..16)
                .map(|t| tile(t, Some(64.0), Some(0.0), true))
                .collect(),
        ));
        w.observe(&frame(
            (0..16).map(|t| tile(t, None, Some(0.0), false)).collect(),
        ));
        assert_eq!(w.observations(), 0);
        assert!(!w.tripped());
    }

    /// Sub-threshold score movement on a varied input is
    /// still "did not move" — the near-constant embedder's
    /// jitter must not exonerate it.
    #[test]
    fn jitter_below_the_floor_does_not_demonstrate() {
        let mut w = CollapseWatch::new();
        w.observe(&frame(
            (0..16)
                .map(|t| tile(t, Some(64.0), Some(UNMOVED_SCORE_MAX), false))
                .collect(),
        ));
        assert!(w.tripped());
        assert!(!w.demonstrated());
    }
}
