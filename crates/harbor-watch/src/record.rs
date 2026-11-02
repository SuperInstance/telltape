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
