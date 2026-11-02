//! The driver→pipeline record format and its std-only JSONL codec.
//!
//! Schema (one JSON object per line), produced on the Thor by the
//! driver lane from the JEV rack's I-JEPA embeddings:
//!
//! ```json
//! {"seq":0,"frame":0,"tile":0,"t_arrival_us":123456,"diff_score":0.001,"embedding":[1280 floats]}
//! ```
//!
//! The parser is deliberately small and strict-ish: it finds the named
//! scalar fields and the `embedding` array by key, and errors with the
//! line number on anything malformed. It is not a general JSON parser
//! and does not pretend to be one.

use std::fmt;

pub const GRID: usize = 4; // 4x4 tiles
pub const TILES: usize = GRID * GRID;
pub const DEFAULT_DIM: usize = 1280; // I-JEPA ViT-H embedding size

/// The tile grid over a frame: `rows` × `cols`, tiles numbered
/// row-major (tile = row * cols + col). 4×4 is the default — every
/// measured number this tool ships with (wave 3, slice 1, the
/// soak) was taken on it; see `docs/GRID.md` for the rationale and
/// what changing it trades. The grid is a *runtime* value, not a
/// format constant: every verdict row is interpreted against the
/// `grid` field of its own ledger's `lens.txt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub rows: usize,
    pub cols: usize,
}

impl Grid {
    pub fn new(rows: usize, cols: usize) -> Result<Grid, ParseError> {
        if !(1..=16).contains(&rows) || !(1..=16).contains(&cols) || rows * cols < 2 {
            return Err(ParseError(format!(
                "grid {rows}x{cols} out of range: each side must be 1..=16 and the grid needs at least 2 tiles"
            )));
        }
        Ok(Grid { rows, cols })
    }
    pub fn tiles(&self) -> usize {
        self.rows * self.cols
    }
    pub fn tile_at(&self, row: usize, col: usize) -> usize {
        row * self.cols + col
    }
    pub fn row_col(&self, tile: usize) -> (usize, usize) {
        (tile / self.cols, tile % self.cols)
    }
}

impl Default for Grid {
    fn default() -> Self {
        Grid {
            rows: GRID,
            cols: GRID,
        }
    }
}

impl std::fmt::Display for Grid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}", self.rows, self.cols)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InputRecord {
    pub seq: u64,
    pub frame: u32,
    pub tile: u32,
    pub t_arrival_us: u64,
    pub diff_score: f64,
    pub embedding: Vec<f32>,
}

#[derive(Debug)]
pub struct ParseError(pub String);
impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
impl std::error::Error for ParseError {}

fn find_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\"");
    let at = line.find(&pat)?;
    let rest = &line[at + pat.len()..];
    let colon = rest.find(':')?;
    Some(rest[colon + 1..].trim_start())
}

fn scalar<T: std::str::FromStr>(line: &str, key: &str) -> Result<T, ParseError> {
    let rest = find_key(line, key).ok_or_else(|| ParseError(format!("missing key \"{key}\"")))?;
    let end = rest
        .find(|c: char| c == ',' || c == '}' || c.is_whitespace())
        .unwrap_or(rest.len());
    rest[..end]
        .parse::<T>()
        .map_err(|_| ParseError(format!("bad number for \"{key}\": {:?}", &rest[..end])))
}

pub fn parse_record(line: &str) -> Result<InputRecord, ParseError> {
    let emb_rest = find_key(line, "embedding")
        .ok_or_else(|| ParseError("missing key \"embedding\"".into()))?;
    let open = emb_rest
        .find('[')
        .ok_or_else(|| ParseError("embedding is not an array".into()))?;
    let close = emb_rest
        .rfind(']')
        .ok_or_else(|| ParseError("embedding array not closed".into()))?;
    if close <= open {
        return Err(ParseError("empty embedding array".into()));
    }
    let inner = &emb_rest[open + 1..close];
    let mut embedding = Vec::new();
    if !inner.trim().is_empty() {
        for tok in inner.split(',') {
            embedding.push(
                tok.trim()
                    .parse::<f32>()
                    .map_err(|_| ParseError(format!("bad embedding value {tok:?}")))?,
            );
        }
    }
    Ok(InputRecord {
        seq: scalar(line, "seq")?,
        frame: scalar(line, "frame")?,
        tile: scalar(line, "tile")?,
        t_arrival_us: scalar(line, "t_arrival_us")?,
        diff_score: scalar(line, "diff_score")?,
        embedding,
    })
}

pub fn parse_jsonl(text: &str) -> Result<Vec<InputRecord>, ParseError> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(parse_record(line).map_err(|e| ParseError(format!("line {}: {}", i + 1, e.0)))?);
    }
    Ok(out)
}

/// Serialise one record in the exact driver schema (used by the fixture
/// generator and the round-trip test).
pub fn record_to_json(r: &InputRecord) -> String {
    let mut s = String::with_capacity(r.embedding.len() * 10 + 128);
    s.push_str(&format!(
        "{{\"seq\":{},\"frame\":{},\"tile\":{},\"t_arrival_us\":{},\"diff_score\":{:.6},\"embedding\":[",
        r.seq, r.frame, r.tile, r.t_arrival_us, r.diff_score
    ));
    for (i, v) in r.embedding.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("{v:.6}"));
    }
    s.push_str("]}");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> InputRecord {
        InputRecord {
            seq: 3,
            frame: 2,
            tile: 5,
            t_arrival_us: 123_456,
            diff_score: 0.25,
            embedding: vec![0.5, -0.25, 1.0],
        }
    }

    #[test]
    fn round_trip_preserves_the_record() {
        let r = sample();
        let back = parse_record(&record_to_json(&r)).unwrap();
        assert_eq!(back.seq, r.seq);
        assert_eq!(back.frame, r.frame);
        assert_eq!(back.tile, r.tile);
        assert_eq!(back.t_arrival_us, r.t_arrival_us);
        assert!((back.diff_score - r.diff_score).abs() < 1e-6);
        assert_eq!(back.embedding.len(), 3);
    }

    #[test]
    fn missing_embedding_is_a_named_error() {
        let e = parse_record("{\"seq\":0,\"frame\":0}").unwrap_err();
        assert!(e.0.contains("embedding"), "{e}");
    }

    #[test]
    fn bad_number_is_a_named_error() {
        let e = parse_record(
            "{\"seq\":\"x\",\"frame\":0,\"tile\":0,\"t_arrival_us\":0,\"diff_score\":0,\"embedding\":[1]}",
        )
        .unwrap_err();
        assert!(e.0.contains("seq"), "{e}");
    }

    #[test]
    fn parse_jsonl_reports_the_line_number() {
        let good = record_to_json(&sample());
        let text = format!("{good}\nnot json\n{good}");
        let e = parse_jsonl(&text).unwrap_err();
        assert!(e.0.contains("line 2"), "{e}");
    }

    #[test]
    fn grid_validation_and_addressing() {
        assert_eq!(Grid::default().tiles(), 16);
        assert_eq!(format!("{}", Grid::default()), "4x4");
        let g = Grid::new(3, 5).unwrap();
        assert_eq!(g.tiles(), 15);
        assert_eq!(g.tile_at(1, 2), 7);
        assert_eq!(g.row_col(7), (1, 2));
        assert!(Grid::new(0, 4).is_err());
        assert!(Grid::new(4, 17).is_err());
        assert!(
            Grid::new(1, 1).is_err(),
            "a 1-tile grid has no neighbours to refine against"
        );
    }
}
