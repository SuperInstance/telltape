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
    /// The embedder's identity: name (e.g. `jev-rack`, `http`,
    /// `double`, or an operator-chosen name for a BYO adapter).
    /// Recorded in every ledger this run writes (deficit D1: two
    /// different embedders' spaces are related by at best a
    /// rotation, so a ledger that cannot say WHICH embedder
    /// produced its tells cannot be trusted across a swap — the
    /// width guard alone passes a same-width swap silently).
    /// Empty = undeclared; the run's identity is then `unknown`,
    /// which is stamped as such, never dressed as a real name.
    /// `index` fills this in from the adapter its own config
    /// selects when the operator leaves it empty.
    pub embedder_name: String,
    /// The embedder's version string (model / algorithm version),
    /// the second half of the identity. For the generic HTTP
    /// adapter the endpoint URL is the only version a generic
    /// endpoint has, and `index` uses it as such; an operator who
    /// changes the model behind an endpoint must bump this.
    pub embedder_version: String,
    /// Explicit operator acknowledgement of an embedder swap (or
    /// of stamping a pre-0.1.1 ledger that records no identity):
    /// proceed against the existing ledger with fresh baselines
    /// and receipt the swap. Default false — without it, opening
    /// a ledger whose recorded embedder identity differs from
    /// this config's is a typed refusal, never a silent continue.
    pub recalibrate: bool,
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
    /// Tile grid rows (default 4). Paired with `grid_cols`; the
    /// ingest must be run with the matching `--grid` value — the
    /// tile count is the contract between the two halves.
    pub grid_rows: usize,
    /// Tile grid columns (default 4).
    pub grid_cols: usize,
    /// Soak mode: run unattended until the stop file / EOF, appending
    /// precision self-report placeholders to state.txt.
    pub soak: bool,
    /// 0 = unlimited.
    pub max_frames: u32,
    /// Checked once per frame; if it appears, shut down cleanly.
    pub stop_file: String,
    /// Expected embedding width of the configured embedder
    /// (default 1280, the I-JEPA width). Used by `index` when it
    /// composes its ingest command.
    pub embed_dim: usize,
    /// Multipart field name for the generic HTTP embedder
    /// (default "file"). Used by `index`'s composed command.
    pub embed_field: String,
    /// JSON response key for the generic HTTP embedder (default
    /// "embedding"). Used by `index`'s composed command.
    pub embed_key: String,
    /// The ingest diff gate (mean-abs pixel diff above which a
    /// tile is nominated). `watch` configs carry this inside
    /// their hand-written `ingest_cmd`; `index` composes its
    /// command from this key. Default 3.0, the calibrated value
    /// of the origin record.
    pub gate_diff: f64,
    /// Path to `ingest/ingest.py`, used by `index` to compose its
    /// command. Empty = `$TELLTAPE_INGEST`, else an error naming
    /// the key (no home-directory path is ever a default).
    pub ingest_script: String,
    /// Frames per second `index` takes from a video file (and
    /// the rate assumed for image sequences). Default 2.0.
    pub index_fps: f64,
    /// Capture epoch (µs) of frame 0 for `index`; 0 = derive
    /// from the footage file (mtime minus probed duration).
    pub index_start_us: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stream_id: "stream0".into(),
            ledger_dir: "ledger".into(),
            ingest_cmd: String::new(),
            embed_url: String::new(),
            embed_health_url: String::new(),
            embedder_name: String::new(),
            embedder_version: String::new(),
            recalibrate: false,
            embedder_autostart: false,
            embedder_start_cmd: String::new(),
            embedder_stop_cmd: String::new(),
            stop_embedder_on_exit: true,
            threshold: 0.25,
            alpha: 0.1,
            focus_k: 4,
            grid_rows: 4,
            grid_cols: 4,
            soak: false,
            max_frames: 0,
            stop_file: String::new(),
            embed_dim: 1280,
            embed_field: "file".into(),
            embed_key: "embedding".into(),
            gate_diff: 3.0,
            ingest_script: String::new(),
            index_fps: 2.0,
            index_start_us: 0,
        }
    }
}

impl Config {
    /// The configured tile grid, validated. A bad grid is a
    /// config error naming the keys, not a runtime surprise.
    pub fn grid(&self) -> Result<harbor_watch::record::Grid, ConfigError> {
        harbor_watch::record::Grid::new(self.grid_rows, self.grid_cols)
            .map_err(|e| ConfigError(format!("grid_rows/grid_cols: {e}")))
    }

    /// This run's embedder identity, as recorded in the ledger:
    /// `name@version`, the bare name when no version is declared,
    /// or `unknown` when no name is declared at all. Compared
    /// verbatim against the identity an existing ledger recorded
    /// — any difference is a swap (see `recalibrate`).
    pub fn embedder_identity(&self) -> String {
        if self.embedder_name.is_empty() {
            "unknown".to_string()
        } else if self.embedder_version.is_empty() {
            self.embedder_name.clone()
        } else {
            format!("{}@{}", self.embedder_name, self.embedder_version)
        }
    }

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
    if let Some(v) = get1("embedder_name") {
        cfg.embedder_name = v;
    }
    if let Some(v) = get1("embedder_version") {
        cfg.embedder_version = v;
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
    if let Some(v) = get1("embed_field") {
        cfg.embed_field = v;
    }
    if let Some(v) = get1("embed_key") {
        cfg.embed_key = v;
    }
    if let Some(v) = get1("ingest_script") {
        cfg.ingest_script = v;
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
    if let Some(v) = boolean("recalibrate", "recalibrate")? {
        cfg.recalibrate = v;
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
    if let Some(v) = num("grid_rows")? {
        cfg.grid_rows = v as usize;
    }
    if let Some(v) = num("grid_cols")? {
        cfg.grid_cols = v as usize;
    }
    // Validate the pair eagerly: a config that cannot run should
    // fail at load, with the keys named.
    cfg.grid()?;

    if let Some(v) = num("max_frames")? {
        cfg.max_frames = v as u32;
    }
    if let Some(v) = num("embed_dim")? {
        cfg.embed_dim = v as usize;
    }
    if let Some(v) = num("gate_diff")? {
        cfg.gate_diff = v;
    }
    if let Some(v) = num("index_fps")? {
        cfg.index_fps = v;
    }
    if let Some(v) = num("index_start_us")? {
        cfg.index_start_us = v as u64;
    }
    Ok(cfg)
}

pub fn load_config(path: &str) -> Result<Config, ConfigError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| ConfigError(format!("cannot read config {path}: {e}")))?;
    parse_config(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_the_documented_defaults() {
        let c = parse_config("").unwrap();
        assert_eq!(c.stream_id, "stream0");
        assert_eq!(c.grid_rows, 4);
        assert_eq!(c.grid_cols, 4);
        assert_eq!(c.grid().unwrap().tiles(), 16);
        assert!(
            c.embed_url.is_empty(),
            "no embedder default — see config docs"
        );
        assert_eq!(c.health_url(), None);
    }

    #[test]
    fn grid_keys_parse_and_validate() {
        let c = parse_config("grid_rows = 3\ngrid_cols = 5\n").unwrap();
        assert_eq!(c.grid().unwrap().tiles(), 15);
        let e = parse_config("grid_rows = 17\n").unwrap_err();
        assert!(e.0.contains("grid_rows"), "{e}");
    }

    #[test]
    fn rack_keys_are_accepted_as_aliases() {
        let c = parse_config("rack_url = http://127.0.0.1:8077\nrack_autostart = true\n").unwrap();
        assert_eq!(c.embed_url, "http://127.0.0.1:8077");
        assert!(c.embedder_autostart);
        // The new key wins over its alias.
        let c = parse_config("rack_url = http://old\nembed_url = http://new\n").unwrap();
        assert_eq!(c.embed_url, "http://new");
    }

    #[test]
    fn bad_values_name_the_key() {
        assert!(parse_config("threshold = abc\n")
            .unwrap_err()
            .0
            .contains("threshold"));
        assert!(parse_config("soak = maybe\n")
            .unwrap_err()
            .0
            .contains("soak"));
    }

    #[test]
    fn health_url_derives_from_the_embed_origin() {
        let c = parse_config("embed_url = http://127.0.0.1:9000/embed/image\n").unwrap();
        assert_eq!(
            c.health_url().as_deref(),
            Some("http://127.0.0.1:9000/health")
        );
        let c = parse_config("embed_url = http://h:9/e\nembed_health_url = http://elsewhere/h\n")
            .unwrap();
        assert_eq!(c.health_url().as_deref(), Some("http://elsewhere/h"));
    }

    #[test]
    fn embedder_identity_keys_parse_and_compose() {
        // Undeclared: the identity is honestly "unknown".
        let c = parse_config("").unwrap();
        assert_eq!(c.embedder_identity(), "unknown");
        assert!(!c.recalibrate);
        // Name only, then name + version.
        let c = parse_config("embedder_name = jev-rack\n").unwrap();
        assert_eq!(c.embedder_identity(), "jev-rack");
        let c = parse_config(
            "embedder_name = jev-rack\nembedder_version = ijepa-1.0\nrecalibrate = true\n",
        )
        .unwrap();
        assert_eq!(c.embedder_identity(), "jev-rack@ijepa-1.0");
        assert!(c.recalibrate);
        assert!(parse_config("recalibrate = maybe\n")
            .unwrap_err()
            .0
            .contains("recalibrate"));
    }

    #[test]
    fn unknown_keys_are_ignored_for_forward_compatibility() {
        let c = parse_config("future_key = yes\nstream_id = s1\n").unwrap();
        assert_eq!(c.stream_id, "s1");
    }
}
