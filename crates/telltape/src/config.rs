//! Service configuration: a plain `key = value` text file, the same
//! shape as the LensFile — a technician can read and edit it with no
//! tooling. Unknown keys are ignored (forward compatibility); bad
//! numbers are hard errors with the key named.
//!
//! Embedder keys (extraction surgery, plan §2.2): the origin tool
//! hardcoded the fleet's JEV rack (`rack_url`, `rack_autostart`,
//! `stop_rack_on_exit`, a systemctl lifecycle). Here the embedder is
//! whatever the ingest is configured to call; this service only
//! needs to know how to check it is alive and — optionally — how to
//! start and stop it. The old `rack_*` keys are accepted as aliases
//! for one release, because config files are technician-edited text
//! that outlives binaries.

use std::collections::BTreeMap;
use std::fmt;

#[derive(Debug, Clone)]
pub struct Config {
    pub stream_id: String,
    /// Directory holding lens.txt / state.txt / events.jsonl / health.txt.
    pub ledger_dir: String,
    /// Shell command that produces ingest records on stdout (the
    /// Python ingest). Empty = read records from our own stdin
    /// (tests, manual driving).
    pub ingest_cmd: String,
    /// Base URL of the embedder the ingest calls (e.g. the generic
    /// HTTP adapter's service, or the JEV rack). **No default**: an
    /// unset embedder is a configuration the operator chose (the
    /// ingest's built-in test double needs no service), not a silent
    /// dial to a port that happens to be someone's.
    pub embed_url: String,
    /// URL polled for embedder liveness before the ingest starts.
    /// Empty = derive `{embed_url origin}/health`; if `embed_url`
    /// is also empty, no external check is possible and none is
    /// made (the embedder is in-process or managed elsewhere).
    pub embed_health_url: String,
    /// If the embedder is down at startup, run `embedder_start_cmd`
    /// (and remember we did).
    pub embedder_autostart: bool,
    /// Shell command that starts the embedder. Empty (the default)
    /// = this service starts nothing.
    pub embedder_start_cmd: String,
    /// Shell command that stops the embedder. Run at exit ONLY if
    /// this service started it and `stop_embedder_on_exit` is set.
    pub embedder_stop_cmd: String,
    /// Stop the embedder at exit ONLY if this service started it.
    pub stop_embedder_on_exit: bool,
    pub threshold: f64,
    pub alpha: f64,
    pub focus_k: usize,
    /// Soak mode: run unattended until the stop file / EOF, appending
    /// precision self-report placeholders to state.txt.
    pub soak: bool,
    /// 0 = unlimited.
    pub max_frames: u32,
    /// Checked once per frame; if it appears, shut down cleanly.
    pub stop_file: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stream_id: "stream0".into(),
            ledger_dir: "ledger".into(),
            ingest_cmd: String::new(),
            embed_url: String::new(),
            embed_health_url: String::new(),
            embedder_autostart: false,
            embedder_start_cmd: String::new(),
            embedder_stop_cmd: String::new(),
            stop_embedder_on_exit: true,
            threshold: 0.25,
            alpha: 0.1,
            focus_k: 4,
            soak: false,
            max_frames: 0,
            stop_file: String::new(),
        }
    }
}

impl Config {
    /// The URL whose liveness gates the run, or `None` when no
    /// external embedder check is configured.
    pub fn health_url(&self) -> Option<String> {
        if !self.embed_health_url.is_empty() {
            return Some(self.embed_health_url.clone());
        }
        if self.embed_url.is_empty() {
            return None;
        }
        // Derive `{origin}/health` from the embed URL: scheme +
        // authority, path discarded.
        let u = self.embed_url.trim_end_matches('/');
        match u.find("://") {
            Some(i) => {
                let rest = &u[i + 3..];
                let host = rest.split('/').next().unwrap_or(rest);
                Some(format!("{}://{}/health", &u[..i], host))
            }
            None => None,
        }
    }
}

#[derive(Debug)]
pub struct ConfigError(pub String);
impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for ConfigError {}

pub fn parse_config(text: &str) -> Result<Config, ConfigError> {
    let mut kv: BTreeMap<String, String> = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(eq) = line.find('=') {
            kv.insert(
                line[..eq].trim().to_string(),
                line[eq + 1..].trim().to_string(),
            );
        }
    }
    let mut cfg = Config::default();
    // New key wins; the origin `rack_*` name is the one-release alias.
    let get = |k: &str, alias: &str| kv.get(k).or_else(|| kv.get(alias)).cloned();
    let get1 = |k: &str| kv.get(k).cloned();
    if let Some(v) = get1("stream_id") {
        cfg.stream_id = v;
    }
    if let Some(v) = get1("ledger_dir") {
        cfg.ledger_dir = v;
    }
    if let Some(v) = get1("ingest_cmd") {
        cfg.ingest_cmd = v;
    }
    if let Some(v) = get("embed_url", "rack_url") {
        cfg.embed_url = v;
    }
    if let Some(v) = get1("embed_health_url") {
        cfg.embed_health_url = v;
    }
    if let Some(v) = get1("embedder_start_cmd") {
        cfg.embedder_start_cmd = v;
    }
    if let Some(v) = get1("embedder_stop_cmd") {
        cfg.embedder_stop_cmd = v;
    }
    if let Some(v) = get1("stop_file") {
        cfg.stop_file = v;
    }
    let num = |k: &str| -> Result<Option<f64>, ConfigError> {
        match get1(k) {
            Some(s) => s
                .parse::<f64>()
                .map(Some)
                .map_err(|_| ConfigError(format!("bad number for {k}: {s:?}"))),
            None => Ok(None),
        }
    };
    let boolean = |k: &str, alias: &str| -> Result<Option<bool>, ConfigError> {
        match get(k, alias).as_deref() {
            Some("true") => Ok(Some(true)),
            Some("false") => Ok(Some(false)),
            Some(other) => Err(ConfigError(format!("bad boolean for {k}: {other:?}"))),
            None => Ok(None),
        }
    };
    if let Some(v) = boolean("embedder_autostart", "rack_autostart")? {
        cfg.embedder_autostart = v;
    }
    if let Some(v) = boolean("stop_embedder_on_exit", "stop_rack_on_exit")? {
        cfg.stop_embedder_on_exit = v;
    }
    if let Some(v) = boolean("soak", "soak")? {
        cfg.soak = v;
    }
    if let Some(v) = num("threshold")? {
        cfg.threshold = v;
    }
    if let Some(v) = num("alpha")? {
        cfg.alpha = v;
    }
    if let Some(v) = num("focus_k")? {
        cfg.focus_k = v as usize;
    }
    if let Some(v) = num("max_frames")? {
        cfg.max_frames = v as u32;
    }
    Ok(cfg)
}

pub fn load_config(path: &str) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ConfigError(format!("cannot read config {path}: {e}")))?;
    parse_config(&text)
}
