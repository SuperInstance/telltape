//! The `index` verb: existing footage → ledger.
//!
//! Plan §3.1: replay exists (the live pipeline is parity-tested
//! against the batch cascade, and the ingest's staged/wave3
//! sources have always been replays); the new-thin part is the
//! file frame source in the ingest. This module is only the
//! composition: it builds the ingest command for the footage
//! (video file, image sequence, or single image) from the same
//! config a `watch` run uses, then hands it to the *identical*
//! service loop. An index ledger therefore means exactly what a
//! watch ledger means — same tiling, same gate, same verdicts,
//! same freshness accounting (a static stretch of footage reads
//! frozen, because it is).
//!
//! The embedder for the composed command follows the config's
//! own statement: a non-empty `embed_url` means the generic HTTP
//! embedder at that URL (with `embed_field` / `embed_key` /
//! `embed_dim`); an empty one means the ingest's built-in test
//! double. Nothing else is inferred.

use crate::config::{Config, ConfigError};
use crate::service::{self, ServiceError, Summary};

/// Single-quote a string for a POSIX shell command line.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Resolve the ingest script path: config key, else the
/// `TELLTAPE_INGEST` environment variable, else a typed refusal.
fn ingest_script(cfg: &Config) -> Result<String, ServiceError> {
    if !cfg.ingest_script.is_empty() {
        return Ok(cfg.ingest_script.clone());
    }
    if let Ok(p) = std::env::var("TELLTAPE_INGEST") {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    Err(ServiceError::Config(ConfigError(
        "ingest_script is not set: point it at ingest/ingest.py \
         (or set TELLTAPE_INGEST)"
            .into(),
    )))
}

/// Build the ingest command `index` runs for `source`.
pub fn index_ingest_cmd(cfg: &Config, source: &str) -> Result<String, ServiceError> {
    if cfg.index_fps <= 0.0 {
        return Err(ServiceError::Config(ConfigError(format!(
            "index_fps must be > 0, got {}",
            cfg.index_fps
        ))));
    }
    let grid = cfg.grid()?;
    let mut cmd = format!(
        "python3 {} --source file --file {} --fps {} --start-us {} --gate {} --grid {}x{} --embed-dim {}",
        shell_quote(&ingest_script(cfg)?),
        shell_quote(source),
        cfg.index_fps,
        cfg.index_start_us,
        cfg.gate_diff,
        grid.rows,
        grid.cols,
        cfg.embed_dim,
    );
    if cfg.embed_url.is_empty() {
        cmd.push_str(" --embedder double");
    } else {
        cmd.push_str(&format!(
            " --embedder http --embed-url {} --embed-field {} --embed-key {}",
            shell_quote(&cfg.embed_url),
            shell_quote(&cfg.embed_field),
            shell_quote(&cfg.embed_key),
        ));
    }
    if cfg.max_frames > 0 {
        cmd.push_str(&format!(" --max-frames {}", cfg.max_frames));
    }
    Ok(cmd)
}

/// The identity `index` records for the ingest's built-in test
/// double (deficit D1). The version component is the double's
/// pseudo-embedding algorithm version — `HashEmbedder` in
/// ingest.py. The double reports itself honestly as the double;
/// if the hash expansion ever changes, bump this version and
/// the ingest together: a changed expansion is a changed
/// embedding space, and old ledgers must refuse it as a swap.
const DOUBLE_NAME: &str = "double";
const DOUBLE_VERSION: &str = "1";

/// Fill the run's embedder identity from the adapter the config
/// itself selects for the composed command — when the operator
/// has not declared one. A declared identity always wins: the
/// operator may know the endpoint as a name (`jev-rack@…`) that
/// no URL can express. For the generic HTTP adapter, the
/// endpoint URL is the version component — it is the only
/// version a generic endpoint has, so re-pointing the config at
/// a different endpoint reads as the swap it is.
fn declare_adapter_identity(run_cfg: &mut Config) {
    if !run_cfg.embedder_name.is_empty() {
        return;
    }
    if run_cfg.embed_url.is_empty() {
        run_cfg.embedder_name = DOUBLE_NAME.to_string();
        run_cfg.embedder_version = DOUBLE_VERSION.to_string();
    } else {
        run_cfg.embedder_name = "http".to_string();
        run_cfg.embedder_version = run_cfg.embed_url.clone();
    }
}

/// Index one footage source into the configured ledger.
pub fn index_file(cfg: &Config, source: &str) -> Result<Summary, ServiceError> {
    let mut run_cfg = cfg.clone();
    run_cfg.ingest_cmd = index_ingest_cmd(cfg, source)?;
    declare_adapter_identity(&mut run_cfg);
    service::run(&run_cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::parse_config;

    #[test]
    fn empty_embed_url_means_the_test_double() {
        let cfg = parse_config("ingest_script = /x/ingest.py\n").unwrap();
        let cmd = index_ingest_cmd(&cfg, "/tape/a b.mp4").unwrap();
        assert!(cmd.contains("--source file"), "{cmd}");
        assert!(cmd.contains("--file '/tape/a b.mp4'"), "{cmd}");
        assert!(cmd.contains("--embedder double"), "{cmd}");
        assert!(cmd.contains("--grid 4x4"), "{cmd}");
    }

    #[test]
    fn embed_url_means_the_generic_http_adapter() {
        let cfg = parse_config(
            "ingest_script = /x/ingest.py\nembed_url = http://h:9/embed\nembed_dim = 768\n",
        )
        .unwrap();
        let cmd = index_ingest_cmd(&cfg, "/tape/a.mp4").unwrap();
        assert!(cmd.contains("--embedder http"), "{cmd}");
        assert!(cmd.contains("--embed-url 'http://h:9/embed'"), "{cmd}");
        assert!(cmd.contains("--embed-dim 768"), "{cmd}");
    }

    #[test]
    fn index_identity_comes_from_the_selected_adapter() {
        // No embed URL: the composed command runs the test
        // double, and the ledger must say so, honestly.
        let mut cfg = parse_config("").unwrap();
        declare_adapter_identity(&mut cfg);
        assert_eq!(cfg.embedder_identity(), "double@1");
        // An endpoint: the URL is the version a generic
        // endpoint has.
        let mut cfg = parse_config("embed_url = http://h:9/embed\n").unwrap();
        declare_adapter_identity(&mut cfg);
        assert_eq!(cfg.embedder_identity(), "http@http://h:9/embed");
        // A declared identity is the operator's word; keep it.
        let mut cfg =
            parse_config("embed_url = http://h:9/embed\nembedder_name = jev-rack\nembedder_version = ijepa-1.0\n")
                .unwrap();
        declare_adapter_identity(&mut cfg);
        assert_eq!(cfg.embedder_identity(), "jev-rack@ijepa-1.0");
    }

    #[test]
    fn missing_ingest_script_is_a_named_refusal() {
        let cfg = parse_config("").unwrap();
        match index_ingest_cmd(&cfg, "/tape/a.mp4") {
            Err(ServiceError::Config(e)) => assert!(e.0.contains("ingest_script"), "{e}"),
            other => panic!("expected a config refusal, got {other:?}"),
        }
    }

    #[test]
    fn bad_fps_is_a_named_refusal() {
        let cfg = parse_config("ingest_script = /x/ingest.py\nindex_fps = 0\n").unwrap();
        assert!(matches!(
            index_ingest_cmd(&cfg, "/tape/a.mp4"),
            Err(ServiceError::Config(_))
        ));
    }
}
