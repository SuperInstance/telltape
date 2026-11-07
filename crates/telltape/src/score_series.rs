//! The score-series sidecar: one compact JSONL row for every
//! frame the gate evaluates — fired or not.
//!
//! The ledger's design is that silence is the default: a frame
//! that publishes nothing leaves no verdict row, by intention.
//! The cost of that design landed in VALIDATION-VERDICT-1 §6:
//! offline analyses that need the *sub-threshold* life of a run
//! — above all the CfC anticipation test, whose clause (i)
//! grades the score climbing toward an event in the frames
//! *before* it fires — had nothing to grade. The near-miss
//! frames survived only as a counter. The scores themselves
//! exist inside the gate on every frame; quiet frames simply
//! never wrote them down.
//!
//! This writer is the instrument change that closes the gap,
//! and it is deliberately only that: a pure observer tap on
//! values the gate already computed, wired at the single seam
//! where the ledger itself reads them ([`crate::ledger`]'s
//! record path), so a row can never disagree with the ledger's
//! own ceiling and near-miss accounting. It changes no score,
//! no gate decision, no freshness or collapse behaviour; with
//! the feature off (the default) the writer does not exist and
//! the run is byte-identical to one that never heard of it.
//!
//! Row schema (one line per evaluated frame, in frame order):
//!
//! ```json
//! {"frame":N,"t_capture_us":N,"gate_diff_max":F|null,"max_smoothed":F|null,"near_miss":B,"fired":B,"embedder":"name@version","tiles":[F|null,...]}
//! ```
//!
//! - `frame`, `t_capture_us`: the frame's index and capture
//!   time, on the same clock basis as the ledger's verdict
//!   rows, so the series joins the ledger without translation.
//! - `gate_diff_max`: the largest per-tile driver diff (the
//!   ingest gate's mean-abs pixel diff) among the frame's
//!   tiles; `null` when no tile carried one.
//! - `max_smoothed`: the frame's best tile smoothed score —
//!   the same value the ledger folds into its
//!   `max_smoothed_score` ceiling. `null` on a frame where no
//!   tile has a score yet (the baseline frame).
//! - `near_miss`: the frame published nothing and its best
//!   score reached the ledger's near-miss band (>= 80% of the
//!   threshold) — the identical test the `near_miss_frames`
//!   counter applies, per frame.
//! - `fired`: the frame published a verdict (its row also
//!   exists in events.jsonl; the series is the superset).
//! - `embedder`: the run's embedder identity, stamped on
//!   every row: scores are only comparable within one
//!   embedder's geometry (deficit D1), so the series carries
//!   its identity with it rather than borrowing the lens's.
//! - `tiles`: every tile's smoothed score in tile order
//!   (`null` where a tile has none yet) — the per-tile streams
//!   the anticipation grading consumes.
//!
//! Scores are written at 4 decimal places, the precision the
//! ledger's own state file uses for the same quantities.
//!
//! Size: a row is ~200 bytes at the default 4x4 grid, so the
//! file grows at roughly 17 MB per 100k frames — a day at the
//! validation run's cadence (254k frames) is ~50 MB. Writes
//! are buffered and flushed every [`FLUSH_EVERY_ROWS`] rows
//! and at close; a killed run can lose the unflushed tail
//! (bounded by the flush cadence), which is acceptable for an
//! analysis sidecar and stated here rather than discovered.
//! Rows begin at the first frame the gate evaluates — the
//! first frame carrying embeddings; frames before it have no
//! scores to record, by definition.

use std::fs::File;
use std::io::{BufWriter, Write};

/// Buffered-write flush cadence, in rows. Small enough that
/// a killed run loses at most a few seconds of series at any
/// realistic cadence; large enough that the flush cost is
/// noise next to the per-frame state rewrite the ledger
/// already does.
const FLUSH_EVERY_ROWS: u64 = 128;

pub struct ScoreSeries {
    w: BufWriter<File>,
    /// The embedder identity, JSON-escaped once, stamped
    /// verbatim on every row.
    embedder: String,
    rows: u64,
}

fn fmt_score(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{x:.4}"),
        None => "null".to_string(),
    }
}

fn escape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

impl ScoreSeries {
    pub fn open(path: &str, embedder_identity: &str) -> std::io::Result<Self> {
        let f = File::create(path)?;
        Ok(Self {
            w: BufWriter::new(f),
            embedder: escape_json(embedder_identity),
            rows: 0,
        })
    }

    /// Append one frame's row. `tiles` is every tile's
    /// smoothed score in tile order (`None` where a tile has
    /// none yet); the remaining values are the frame's own
    /// summaries, computed by the caller from the same frame
    /// output the ledger records — this writer formats and
    /// stores, it does not derive.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        frame: u32,
        t_capture_us: u64,
        gate_diff_max: Option<f64>,
        max_smoothed: Option<f64>,
        near_miss: bool,
        fired: bool,
        tiles: &[Option<f64>],
    ) -> std::io::Result<()> {
        let mut line = String::with_capacity(32 + tiles.len() * 8);
        line.push_str(&format!(
            "{{\"frame\":{frame},\"t_capture_us\":{t_capture_us},\"gate_diff_max\":{},\"max_smoothed\":{},\"near_miss\":{near_miss},\"fired\":{fired},\"embedder\":\"{}\",\"tiles\":[",
            fmt_score(gate_diff_max),
            fmt_score(max_smoothed),
            self.embedder,
        ));
        for (i, t) in tiles.iter().enumerate() {
            if i > 0 {
                line.push(',');
            }
            line.push_str(&fmt_score(*t));
        }
        line.push_str("]}\n");
        self.w.write_all(line.as_bytes())?;
        self.rows += 1;
        if self.rows % FLUSH_EVERY_ROWS == 0 {
            self.w.flush()?;
        }
        Ok(())
    }

    /// Rows written so far.
    pub fn rows(&self) -> u64 {
        self.rows
    }

    pub fn flush(&mut self) -> std::io::Result<()> {
        self.w.flush()
    }
}

impl Drop for ScoreSeries {
    fn drop(&mut self) {
        // Best effort: a close-time flush failure has nowhere
        // left to be reported to; the bounded-loss contract is
        // in the module doc.
        let _ = self.w.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> String {
        let d = std::env::temp_dir().join(format!("telltape-scores-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.join("series.jsonl").to_string_lossy().to_string()
    }

    #[test]
    fn rows_carry_the_schema_and_the_identity() {
        let path = temp_path("schema");
        let mut s = ScoreSeries::open(&path, "jev-rack@ijepa-1.0").unwrap();
        s.record(0, 111, None, None, false, false, &[None, None])
            .unwrap();
        s.record(
            1,
            222,
            Some(64.0),
            Some(0.2123),
            true,
            false,
            &[Some(0.2123), Some(0.0)],
        )
        .unwrap();
        s.record(
            2,
            333,
            Some(3.25),
            Some(0.3001),
            false,
            true,
            &[Some(0.3001), Some(0.1)],
        )
        .unwrap();
        s.flush().unwrap();
        assert_eq!(s.rows(), 3);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(
            lines[0],
            "{\"frame\":0,\"t_capture_us\":111,\"gate_diff_max\":null,\"max_smoothed\":null,\"near_miss\":false,\"fired\":false,\"embedder\":\"jev-rack@ijepa-1.0\",\"tiles\":[null,null]}"
        );
        assert!(
            lines[1].contains("\"gate_diff_max\":64.0000"),
            "{}",
            lines[1]
        );
        assert!(lines[1].contains("\"max_smoothed\":0.2123"), "{}", lines[1]);
        assert!(lines[1].contains("\"near_miss\":true"), "{}", lines[1]);
        assert!(lines[2].contains("\"fired\":true"), "{}", lines[2]);
    }

    #[test]
    fn an_identity_with_quotes_is_escaped_not_mangled() {
        let path = temp_path("escape");
        let mut s = ScoreSeries::open(&path, "we\"ird\\name").unwrap();
        s.record(0, 1, None, None, false, false, &[]).unwrap();
        s.flush().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("\"embedder\":\"we\\\"ird\\\\name\""),
            "{text}"
        );
    }
}
