//! The per-stream text ledger (SPEC §2.3): the world is text.
//!
//! ```text
//! <ledger_dir>/
//!   lens.txt      rhythm (period/jitter EMA) + appearance baseline
//!                 + the embedder identity the tells belong to
//!   state.txt     current state + budget meter + coverage counters
//!   events.jsonl  append-only: service receipts + published verdicts
//!   health.txt    Panel-1 heartbeat: embed p50/p99, queue, frame age
//! ```
//!
//! Every file is rewritten whole (they are small) except events.jsonl,
//! which is append-only. Text is canonical; any UI is a projection.

use crate::live::FrameOutput;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn epoch_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or(0)
}

pub fn fmt_opt(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{x:.4}"),
        None => "null".into(),
    }
}

/// p50 / p99 of a latency window (nearest-rank; empty → (0, 0)).
pub fn percentiles(samples: &[f64]) -> (f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let mut s: Vec<f64> = samples.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let at = |q: f64| s[((q * s.len() as f64).ceil() as usize).clamp(1, s.len()) - 1];
    (at(0.50), at(0.99))
}

/// Consecutive repeated frames after which the source is declared
/// frozen. One repeat can be a genuinely static camera view racing
/// its own cadence; two in a row at watch cadences means the image
/// has stopped changing while the poller has not (gap G1).
pub const FROZEN_AFTER_REPEATS: u64 = 2;

/// What one frame's freshness observation did to the source state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreshnessTransition {
    None,
    BecameFrozen,
    Recovered,
}

/// Running counters + windows the ledger files are rendered from.
#[derive(Debug, Default, Clone)]
pub struct Stats {
    pub frames_seen: u64,
    pub frames_changed: u64,
    pub events_published: u64,
    /// Frames whose image differed from the previous frame's
    /// (freshness by novelty — gap G1). A live source, however
    /// quiet the scene, produces novel frames (sensor noise
    /// alone guarantees it); a frozen source cannot.
    pub frames_novel: u64,
    /// Frames byte-identical (by ingest hash) to the previous one.
    pub frames_repeated: u64,
    /// Current run of consecutive repeated frames.
    pub consecutive_repeats: u64,
    pub tiles_nominated: u64,
    pub tiles_embedded: u64,
    pub embeds_spent_s: f64,
    pub embed_ms_window: Vec<f64>,
    pub verdict_latency_ms: Vec<f64>,
    pub period_ms_ema: Option<f64>,
    pub jitter_ms_ema: Option<f64>,
    /// Per-tile score EMAs, sized to the configured grid.
    pub tile_score_ema: Vec<Option<f64>>,
    /// Tile count of the configured grid — the embed-everything
    /// baseline is per-tile, so it scales with the grid.
    pub tiles: usize,
    pub last_frame: Option<u32>,
    pub last_frame_wall_us: u64,
    pub frames_pending_max: u64,
}

impl Stats {
    pub fn mean_embed_ms(&self) -> f64 {
        if self.embed_ms_window.is_empty() {
            0.0
        } else {
            self.embed_ms_window.iter().sum::<f64>() / self.embed_ms_window.len() as f64
        }
    }
    /// What embedding EVERY tile of every frame would have cost, at
    /// the observed mean embed latency — the baseline the gate is
    /// saving against.
    pub fn embed_everything_estimate_s(&self) -> f64 {
        self.frames_seen as f64 * self.tiles.max(1) as f64 * self.mean_embed_ms() / 1000.0
    }
    /// The source is frozen when the last `FROZEN_AFTER_REPEATS`
    /// frames were all repeats of one image.
    pub fn source_frozen(&self) -> bool {
        self.consecutive_repeats >= FROZEN_AFTER_REPEATS
    }
    /// `live` / `frozen` / `unknown`. Unknown means no freshness
    /// data has arrived at all (an ingest too old to stamp
    /// hashes) — reported as its own state, never as live.
    pub fn freshness(&self) -> &'static str {
        if self.frames_novel == 0 && self.frames_repeated == 0 {
            "unknown"
        } else if self.source_frozen() {
            "frozen"
        } else {
            "live"
        }
    }
    pub fn coverage(&self) -> f64 {
        if self.tiles_nominated == 0 {
            1.0
        } else {
            self.tiles_embedded as f64 / self.tiles_nominated as f64
        }
    }
    pub fn spend_ratio(&self) -> f64 {
        let est = self.embed_everything_estimate_s();
        if est <= 0.0 {
            0.0
        } else {
            self.embeds_spent_s / est
        }
    }
}

/// What a ledger directory already on disk says about the
/// embedder that wrote it (deficit D1). Probed *before* opening:
/// opening rewrites lens.txt, so the recorded identity must be
/// read first or the evidence of a swap is destroyed by the
/// act of looking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordedIdentity {
    /// No ledger here yet (no lens.txt, no events): a new ledger
    /// takes the configured identity without ceremony.
    New,
    /// A ledger exists but records no identity — written before
    /// 0.1.1. Its embedder is *unknown*, not "probably the same":
    /// opening it under any configuration requires the explicit
    /// recalibrate acknowledgement. Never guess.
    Legacy,
    /// The identity the ledger recorded, verbatim.
    Known(String),
}

pub struct Ledger {
    dir: String,
    stream_id: String,
    events: File,
    pub stats: Stats,
    pub embedder_up: bool,
    pub embedder_started_by_me: bool,
    pub soak: bool,
    pub threshold: f64,
    pub alpha: f64,
    pub gate_diff: Option<f64>,
    /// The grid every verdict in this ledger is numbered against.
    /// Written into lens.txt; a reader must never have to guess it.
    pub grid: harbor_watch::record::Grid,
    /// The embedder identity every tell in this ledger is
    /// produced by (deficit D1): `name@version`, or `unknown`
    /// when the operator declared none. Written into lens.txt,
    /// state.txt and health.txt; the comparability of rows
    /// across time is only defined within one identity.
    pub embedder_identity: String,
}

impl Ledger {
    /// Read the identity an existing ledger recorded, without
    /// touching anything. The lens carries the identity line;
    /// a non-empty events.jsonl with no lens (or a lens with no
    /// identity line) is a legacy ledger — it exists, and what
    /// wrote it is unknown.
    pub fn probe_identity(dir: &str) -> RecordedIdentity {
        let lens = std::fs::read_to_string(format!("{dir}/lens.txt")).ok();
        match lens {
            Some(text) => {
                for line in text.lines() {
                    if let Some(v) = line.strip_prefix("embedder_identity = ") {
                        return RecordedIdentity::Known(v.trim().to_string());
                    }
                }
                RecordedIdentity::Legacy
            }
            None => {
                let has_events = std::fs::metadata(format!("{dir}/events.jsonl"))
                    .map(|m| m.len() > 0)
                    .unwrap_or(false);
                if has_events {
                    RecordedIdentity::Legacy
                } else {
                    RecordedIdentity::New
                }
            }
        }
    }

    pub fn open(
        dir: &str,
        stream_id: &str,
        threshold: f64,
        alpha: f64,
        soak: bool,
        grid: harbor_watch::record::Grid,
        embedder_identity: &str,
    ) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let events = OpenOptions::new()
            .create(true)
            .append(true)
            .open(format!("{dir}/events.jsonl"))?;
        let stats = Stats {
            tiles: grid.tiles(),
            tile_score_ema: vec![None; grid.tiles()],
            ..Stats::default()
        };
        let l = Self {
            dir: dir.to_string(),
            stream_id: stream_id.to_string(),
            events,
            stats,
            embedder_up: false,
            embedder_started_by_me: false,
            soak,
            threshold,
            alpha,
            gate_diff: None,
            grid,
            embedder_identity: embedder_identity.to_string(),
        };
        l.write_all()?;
        Ok(l)
    }

    fn path(&self, name: &str) -> String {
        format!("{}/{name}", self.dir)
    }

    pub fn service_event(&mut self, kind: &str, msg: &str) -> std::io::Result<()> {
        writeln!(
            self.events,
            "{{\"kind\":\"service\",\"event\":\"{kind}\",\"msg\":\"{}\",\"wall_us\":{}}}",
            msg.replace('"', "'"),
            epoch_us()
        )?;
        self.events.flush()
    }

    /// Record one processed frame. Verdict events are appended to
    /// events.jsonl only when the frame published a change (silence
    /// is the default — that is the product); counters, state, lens
    /// and health are updated for every frame.
    pub fn record_frame(
        &mut self,
        out: &FrameOutput,
        t_capture_us: u64,
        nominated: u64,
        embedded: u64,
        embed_ms: &[f64],
        frames_pending: u64,
    ) -> std::io::Result<Option<f64>> {
        let now = epoch_us();
        let s = &mut self.stats;
        s.frames_seen += 1;
        s.last_frame = Some(out.verdict.frame);
        s.last_frame_wall_us = now;
        s.tiles_nominated += nominated;
        s.tiles_embedded += embedded;
        for &ms in embed_ms {
            s.embeds_spent_s += ms / 1000.0;
            s.embed_ms_window.push(ms);
            if s.embed_ms_window.len() > 512 {
                s.embed_ms_window.remove(0);
            }
        }
        s.frames_pending_max = s.frames_pending_max.max(frames_pending);
        for tv in &out.tiles {
            if let Some(sc) = tv.smoothed_score {
                let e = s.tile_score_ema[tv.tile as usize];
                s.tile_score_ema[tv.tile as usize] =
                    Some(e.map(|p| p * 0.9 + sc * 0.1).unwrap_or(sc));
            }
        }

        let mut latency_ms = None;
        if out.verdict.changed {
            s.frames_changed += 1;
            s.events_published += 1;
            let lat = (now.saturating_sub(t_capture_us)) as f64 / 1000.0;
            latency_ms = Some(lat);
            s.verdict_latency_ms.push(lat);
            let lineage = out.tiles.iter().fold(0u64, |m, t| m | t.lineage_mask);
            let mut tiles_json = String::new();
            for tv in &out.tiles {
                if !tiles_json.is_empty() {
                    tiles_json.push(',');
                }
                tiles_json.push_str(&format!(
                    "{{\"tile\":{},\"raw\":{},\"smoothed\":{},\"changed\":{},\"stale\":{},\"lineage_mask\":{}}}",
                    tv.tile,
                    fmt_opt(tv.raw_score),
                    fmt_opt(tv.smoothed_score),
                    tv.changed,
                    tv.stale,
                    tv.lineage_mask
                ));
            }
            writeln!(
                self.events,
                "{{\"kind\":\"verdict\",\"stream\":\"{}\",\"frame\":{},\"wall_us\":{},\"t_capture_us\":{},\"verdict_latency_ms\":{:.3},\"changed_tiles\":{:?},\"focus\":{:?},\"lineage_mask\":{},\"stage_ms\":{{\"perceive_decide\":{:.3},\"direct\":{:.3},\"refine\":{:.3},\"publish\":{:.3}}},\"embeds_this_frame\":{},\"energy_before\":{:.4},\"energy_after\":{:.4},\"tiles\":[{}]}}",
                self.stream_id,
                out.verdict.frame,
                now,
                t_capture_us,
                lat,
                out.verdict.changed_tiles,
                out.verdict.focus,
                lineage,
                out.stage_ms.perceive_decide,
                out.stage_ms.direct,
                out.stage_ms.refine,
                out.stage_ms.publish,
                embedded,
                out.verdict.energy_before,
                out.verdict.energy_after,
                tiles_json
            )?;
            self.events.flush()?;
        }
        self.write_all()?;
        Ok(latency_ms)
    }

    /// Observe one frame's freshness (gap G1): `repeated` is true
    /// when the frame's image is identical to the previous frame's
    /// (the ingest stamps a hash per frame and flags repeats).
    /// Returns the state transition, if any, so the caller can
    /// receipt it. This is the counter the frozen soak feed could
    /// not move: polls are not freshness; novelty is.
    pub fn observe_freshness(&mut self, repeated: bool) -> FreshnessTransition {
        let was_frozen = self.stats.source_frozen();
        if repeated {
            self.stats.frames_repeated += 1;
            self.stats.consecutive_repeats += 1;
        } else {
            self.stats.frames_novel += 1;
            self.stats.consecutive_repeats = 0;
        }
        let is_frozen = self.stats.source_frozen();
        match (was_frozen, is_frozen) {
            (false, true) => FreshnessTransition::BecameFrozen,
            (true, false) => FreshnessTransition::Recovered,
            _ => FreshnessTransition::None,
        }
    }

    /// Observe a frame-arrival interval for the lens rhythm stats.
    pub fn observe_period(&mut self, period_ms: f64) {
        let s = &mut self.stats;
        match s.period_ms_ema {
            None => {
                s.period_ms_ema = Some(period_ms);
                s.jitter_ms_ema = Some(0.0);
            }
            Some(p) => {
                let dev = (period_ms - p).abs();
                s.jitter_ms_ema = Some(s.jitter_ms_ema.unwrap_or(0.0) * 0.9 + dev * 0.1);
                s.period_ms_ema = Some(p * 0.9 + period_ms * 0.1);
            }
        }
    }

    pub fn write_all(&self) -> std::io::Result<()> {
        std::fs::write(self.path("lens.txt"), self.lens_text())?;
        std::fs::write(self.path("state.txt"), self.state_text())?;
        std::fs::write(self.path("health.txt"), self.health_text())?;
        Ok(())
    }

    pub fn lens_text(&self) -> String {
        let s = &self.stats;
        let mut t = String::new();
        t.push_str(&format!("stream_id = {}\n", self.stream_id));
        t.push_str(&format!("grid = {}\n", self.grid));
        t.push_str(&format!("embedder_identity = {}\n", self.embedder_identity));
        t.push_str(&format!(
            "period_ms = {}\n",
            s.period_ms_ema
                .map(|v| format!("{v:.2}"))
                .unwrap_or_else(|| "unknown".into())
        ));
        t.push_str(&format!(
            "jitter_ms = {}\n",
            s.jitter_ms_ema
                .map(|v| format!("{v:.2}"))
                .unwrap_or_else(|| "unknown".into())
        ));
        t.push_str(&format!("threshold = {:.4}\n", self.threshold));
        t.push_str(&format!("alpha = {:.4}\n", self.alpha));
        if let Some(g) = self.gate_diff {
            t.push_str(&format!("gate_diff = {g:.4}\n"));
        }
        t.push_str(&format!("frames_observed = {}\n", s.frames_seen));
        t.push_str("tile_score_ema =");
        for e in s.tile_score_ema.iter() {
            t.push_str(&format!(" {}", fmt_opt(*e)));
        }
        t.push('\n');
        t
    }

    pub fn state_text(&self) -> String {
        let s = &self.stats;
        let mut t = String::new();
        t.push_str(&format!("stream_id = {}\n", self.stream_id));
        t.push_str(&format!("frames_seen = {}\n", s.frames_seen));
        t.push_str(&format!("frames_novel = {}\n", s.frames_novel));
        t.push_str(&format!("frames_repeated = {}\n", s.frames_repeated));
        t.push_str(&format!("freshness = {}\n", s.freshness()));
        t.push_str(&format!("frames_changed = {}\n", s.frames_changed));
        t.push_str(&format!("events_published = {}\n", s.events_published));
        t.push_str(&format!(
            "last_frame = {}\n",
            s.last_frame
                .map(|f| f.to_string())
                .unwrap_or_else(|| "none".into())
        ));
        t.push_str(&format!("tiles_nominated = {}\n", s.tiles_nominated));
        t.push_str(&format!("tiles_embedded = {}\n", s.tiles_embedded));
        t.push_str(&format!("coverage = {:.4}\n", s.coverage()));
        t.push_str(&format!("embeds_spent_s = {:.3}\n", s.embeds_spent_s));
        t.push_str(&format!(
            "embed_everything_estimate_s = {:.3}\n",
            s.embed_everything_estimate_s()
        ));
        t.push_str(&format!("spend_ratio = {:.4}\n", s.spend_ratio()));
        t.push_str(&format!(
            "embedder = {}\n",
            if self.embedder_up { "up" } else { "down" }
        ));
        t.push_str(&format!("embedder_identity = {}\n", self.embedder_identity));
        t.push_str(&format!(
            "embedder_started_by_me = {}\n",
            self.embedder_started_by_me
        ));
        t.push_str(&format!("soak = {}\n", self.soak));
        if self.soak {
            // Precision self-report placeholders: in soak the human
            // labels arrive out of band (console lane); until then the
            // honest report is the raw counts, labelled as such.
            t.push_str("precision_self_report = placeholder (events fired and spend below; labels pending)\n");
            t.push_str(&format!("soak_events_fired = {}\n", s.events_published));
            t.push_str(&format!("soak_spend_ratio = {:.4}\n", s.spend_ratio()));
        }
        t.push_str(&format!("updated_wall_us = {}\n", epoch_us()));
        t
    }

    pub fn health_text(&self) -> String {
        let s = &self.stats;
        let (p50, p99) = percentiles(&s.embed_ms_window);
        let age_ms = if s.last_frame_wall_us == 0 {
            -1.0
        } else {
            epoch_us().saturating_sub(s.last_frame_wall_us) as f64 / 1000.0
        };
        format!(
            "embed_ms_p50 = {:.2}\nembed_ms_p99 = {:.2}\nembed_ms_n = {}\nframes_pending_max = {}\nlast_frame_age_ms = {:.1}\nembedder = {}\nembedder_identity = {}\nfreshness = {}\nframes_novel = {}\nframes_repeated = {}\nconsecutive_repeats = {}\nsilence_meaningful = {}\nupdated_wall_us = {}\n",
            p50,
            p99,
            s.embed_ms_window.len(),
            s.frames_pending_max,
            age_ms,
            if self.embedder_up { "up" } else { "down" },
            self.embedder_identity,
            s.freshness(),
            s.frames_novel,
            s.frames_repeated,
            s.consecutive_repeats,
            // Panel-1 amendment: silence is only "normal" when health
            // says so — a starved lane must not look calm. Gap G1
            // amendment: a *frozen* lane must not look calm either —
            // polls arriving is not evidence of life; image novelty
            // is. (13 h of the 2026-11 soak were counted this way.)
            self.embedder_up && s.frames_seen > 0 && !s.source_frozen(),
            epoch_us()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::{FrameOutput, StageMs};
    use harbor_watch::pipeline::{FrameVerdict, TileVerdict};
    use harbor_watch::record::Grid;

    fn tempdir(tag: &str) -> String {
        let d = std::env::temp_dir().join(format!("telltape-ledger-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.to_string_lossy().to_string()
    }

    fn tile_verdict(tile: u32, changed: bool) -> TileVerdict {
        TileVerdict {
            frame: 7,
            tile,
            t_arrival_us: 0,
            driver_diff: None,
            raw_score: changed.then_some(0.9),
            smoothed_score: changed.then_some(0.8),
            changed_raw: changed,
            changed,
            focus: changed,
            stale: !changed,
            lineage_mask: if changed { 31 } else { 0 },
        }
    }

    fn changed_output() -> FrameOutput {
        FrameOutput {
            verdict: FrameVerdict {
                frame: 7,
                changed: true,
                changed_tiles: vec![5, 6],
                focus: vec![5, 6],
                energy_before: 0.5,
                energy_after: 0.3,
                records_in_frame: 2,
            },
            tiles: (0..16).map(|t| tile_verdict(t, t == 5 || t == 6)).collect(),
            stage_ms: StageMs::default(),
        }
    }

    #[test]
    fn percentiles_and_fmt_opt() {
        assert_eq!(percentiles(&[]), (0.0, 0.0));
        let (p50, p99) = percentiles(&[10.0, 20.0, 30.0, 40.0]);
        assert_eq!(p50, 20.0);
        assert_eq!(p99, 40.0);
        assert_eq!(fmt_opt(Some(0.5)), "0.5000");
        assert_eq!(fmt_opt(None), "null");
    }

    #[test]
    fn freshness_transitions_and_the_frozen_threshold() {
        let dir = tempdir("fresh");
        let mut l =
            Ledger::open(&dir, "s", 0.25, 0.1, false, Grid::default(), "test-emb@1").unwrap();
        assert_eq!(l.stats.freshness(), "unknown");
        assert_eq!(l.observe_freshness(false), FreshnessTransition::None);
        assert_eq!(l.stats.freshness(), "live");
        assert_eq!(l.observe_freshness(true), FreshnessTransition::None);
        assert!(!l.stats.source_frozen(), "one repeat is not frozen");
        assert_eq!(l.observe_freshness(true), FreshnessTransition::BecameFrozen);
        assert_eq!(l.stats.freshness(), "frozen");
        assert_eq!(l.observe_freshness(true), FreshnessTransition::None);
        assert_eq!(l.observe_freshness(false), FreshnessTransition::Recovered);
        assert_eq!(l.stats.frames_novel, 2);
        assert_eq!(l.stats.frames_repeated, 3);
    }

    #[test]
    fn frozen_health_withdraws_meaning_from_silence() {
        let dir = tempdir("health");
        let mut l =
            Ledger::open(&dir, "s", 0.25, 0.1, false, Grid::default(), "test-emb@1").unwrap();
        l.embedder_up = true;
        l.stats.frames_seen = 3;
        l.observe_freshness(false);
        let h = l.health_text();
        assert!(h.contains("freshness = live"), "{h}");
        assert!(h.contains("silence_meaningful = true"), "{h}");
        l.observe_freshness(true);
        l.observe_freshness(true);
        let h = l.health_text();
        assert!(h.contains("freshness = frozen"), "{h}");
        assert!(h.contains("silence_meaningful = false"), "{h}");
    }

    /// P5 — the verdict row is the format strangers will parse; pin
    /// it byte-for-byte in the fields that carry meaning.
    #[test]
    fn verdict_row_is_the_documented_golden_shape() {
        let dir = tempdir("golden");
        let mut l = Ledger::open(
            &dir,
            "golden-stream",
            0.25,
            0.1,
            false,
            Grid::default(),
            "test-emb@1",
        )
        .unwrap();
        let out = changed_output();
        l.record_frame(&out, epoch_us(), 2, 2, &[100.0, 100.0], 0)
            .unwrap();
        let events = std::fs::read_to_string(format!("{dir}/events.jsonl")).unwrap();
        let row = events
            .lines()
            .find(|x| x.contains("\"kind\":\"verdict\""))
            .expect("one verdict row");
        for needle in [
            "\"stream\":\"golden-stream\"",
            "\"frame\":7",
            "\"changed_tiles\":[5, 6]",
            "\"focus\":[5, 6]",
            "\"lineage_mask\":31",
            "\"embeds_this_frame\":2",
            "\"energy_before\":0.5000",
            "\"energy_after\":0.3000",
            "\"tile\":5,\"raw\":0.9000,\"smoothed\":0.8000,\"changed\":true",
        ] {
            assert!(row.contains(needle), "row missing {needle}: {row}");
        }
        // A silent frame appends nothing — silence is the default.
        let mut silent = changed_output();
        silent.verdict.changed = false;
        silent.verdict.changed_tiles = vec![];
        l.record_frame(&silent, epoch_us(), 0, 0, &[], 0).unwrap();
        let events2 = std::fs::read_to_string(format!("{dir}/events.jsonl")).unwrap();
        assert_eq!(events2.matches("\"kind\":\"verdict\"").count(), 1);
    }

    /// D1 — identity is recorded in all three human surfaces, and
    /// the probe reads back exactly what a previous run stamped.
    #[test]
    fn identity_is_recorded_and_probeable() {
        let dir = tempdir("identity");
        assert_eq!(Ledger::probe_identity(&dir), RecordedIdentity::New);
        let l = Ledger::open(
            &dir,
            "s",
            0.25,
            0.1,
            false,
            Grid::default(),
            "jev-rack@ijepa-1.0",
        )
        .unwrap();
        assert!(l
            .lens_text()
            .contains("embedder_identity = jev-rack@ijepa-1.0"));
        assert!(l
            .state_text()
            .contains("embedder_identity = jev-rack@ijepa-1.0"));
        assert!(l
            .health_text()
            .contains("embedder_identity = jev-rack@ijepa-1.0"));
        assert_eq!(
            Ledger::probe_identity(&dir),
            RecordedIdentity::Known("jev-rack@ijepa-1.0".to_string())
        );
    }

    /// D1 — a pre-0.1.1 ledger (lens with no identity line, or
    /// events with no lens at all) probes as Legacy, never as the
    /// identity a caller would like it to be.
    #[test]
    fn pre_identity_ledgers_probe_as_legacy() {
        let dir = tempdir("legacy");
        std::fs::write(
            format!("{dir}/lens.txt"),
            "stream_id = s\ngrid = 4x4\nthreshold = 0.2500\n",
        )
        .unwrap();
        assert_eq!(Ledger::probe_identity(&dir), RecordedIdentity::Legacy);
        let dir2 = tempdir("legacy-events-only");
        std::fs::write(format!("{dir2}/events.jsonl"), "{\"kind\":\"service\"}\n").unwrap();
        assert_eq!(Ledger::probe_identity(&dir2), RecordedIdentity::Legacy);
        // An empty events file alone is not a ledger anyone wrote.
        let dir3 = tempdir("empty-events");
        std::fs::write(format!("{dir3}/events.jsonl"), "").unwrap();
        assert_eq!(Ledger::probe_identity(&dir3), RecordedIdentity::New);
    }

    #[test]
    fn embed_everything_baseline_scales_with_the_grid() {
        let dir = tempdir("grid");
        let mut l = Ledger::open(
            &dir,
            "s",
            0.25,
            0.1,
            true,
            Grid::new(2, 2).unwrap(),
            "test-emb@1",
        )
        .unwrap();
        let mut out = changed_output();
        out.tiles = (0..4).map(|t| tile_verdict(t, t == 1)).collect();
        out.verdict.changed_tiles = vec![1];
        l.record_frame(&out, epoch_us(), 2, 2, &[100.0, 100.0], 0)
            .unwrap();
        // 1 frame x 4 tiles x 100 ms mean = 0.4 s of embed-everything.
        assert!(l
            .state_text()
            .contains("embed_everything_estimate_s = 0.400"));
        assert!(l.lens_text().contains("grid = 2x2"));
        assert!(l
            .state_text()
            .contains("precision_self_report = placeholder"));
    }
}
