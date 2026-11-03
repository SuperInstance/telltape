//! The `query` verb: read a ledger, print its story.
//!
//! Plan §3.1 marks `query` as an *extract*: the reading already
//! existed inside demos (the console's projection, presence's
//! digest) and nowhere a stranger could call. This module is
//! that reader, over the format exactly as
//! `docs/LEDGER-FORMAT.md` documents it — no new data, no new
//! format.
//!
//! Discipline:
//!
//! - **Read-only.** Files are opened with `File::open` /
//!   `read_to_string` only. A query never creates, appends to,
//!   or rewrites a ledger file (a test snapshots a ledger's
//!   bytes around a query to keep this true).
//! - **Time basis.** Verdicts are filtered and displayed on
//!   `t_capture_us` — when the frame was captured, the seek key
//!   into footage. Service rows carry only `wall_us`, so they
//!   are filtered and displayed on it. The output header says
//!   so; the two clocks are never silently mixed.
//! - **Malformed rows are counted, never skipped.** A line that
//!   does not parse as a verdict or service row (unknown kind,
//!   missing or unparseable load-bearing fields) is reported by
//!   line number in the output. Filters cannot apply to a row
//!   that cannot be read, so the malformed count is a file-wide
//!   fact, labelled as such.
//! - **Legacy ledgers read fine.** A pre-0.1.1 ledger records no
//!   embedder identity; the header prints `unknown embedder`,
//!   never a guess. A ledger with events but no lens.txt has no
//!   recorded grid, so tiles print as raw numbers — the grid is
//!   read from the lens, never assumed.

use harbor_watch::record::Grid;
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

// ----------------------------------------------------------------
// Errors

/// Why a query failed before producing output. Argument-value
/// errors (`BadValue`) are usage errors — `main` maps them to
/// exit 2; the rest are read failures, exit 1.
#[derive(Debug)]
pub enum QueryError {
    /// The ledger directory does not exist (or is not a directory).
    NotFound(String),
    /// The directory exists but is not a ledger: no
    /// `events.jsonl`, or a lens whose grid cannot be read.
    NotALedger(String),
    /// A ledger file could not be read.
    Io {
        path: String,
        source: std::io::Error,
    },
    /// A flag value could not be parsed (a time bound, a tile).
    BadValue {
        flag: String,
        value: String,
        why: String,
    },
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            QueryError::NotFound(d) => write!(f, "no such ledger directory: {d}"),
            QueryError::NotALedger(m) => write!(f, "not a ledger: {m}"),
            QueryError::Io { path, source } => write!(f, "reading {path}: {source}"),
            QueryError::BadValue { flag, value, why } => {
                write!(f, "bad {flag} value {value:?}: {why}")
            }
        }
    }
}
impl std::error::Error for QueryError {}

// ----------------------------------------------------------------
// Filters

/// What a query selects. Time bounds are inclusive on both
/// ends and compare against capture time for verdicts, write
/// time for service rows (see the module docs).
#[derive(Debug, Default, Clone)]
pub struct QueryFilters {
    pub since_us: Option<u64>,
    pub until_us: Option<u64>,
    pub tile: Option<u32>,
    pub stream: Option<String>,
    pub summary: bool,
}

impl QueryFilters {
    fn describe(&self, zone: &Zone) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(s) = self.since_us {
            parts.push(format!("since {} (epoch_us {s})", fmt_local(s, zone)));
        }
        if let Some(u) = self.until_us {
            parts.push(format!("until {} (epoch_us {u})", fmt_local(u, zone)));
        }
        if let Some(t) = self.tile {
            parts.push(format!("tile {t}"));
        }
        if let Some(s) = &self.stream {
            parts.push(format!("stream {s}"));
        }
        if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join("; ")
        }
    }
}

// ----------------------------------------------------------------
// Zones and civil time (std-only; no date crate in this workspace)

/// A display zone: a fixed offset, or a parsed TZif file whose
/// offset and abbreviation vary by instant (DST, history).
/// Conversion uses the civil-from-days algorithms (Hinnant);
/// the anchors are unit-tested against `date(1)` output.
#[derive(Debug, Clone)]
pub enum Zone {
    Fixed { offset_s: i64, label: String },
    Tzif(Box<Tzif>),
}

#[derive(Debug, Clone)]
struct TtType {
    offset_s: i64,
    isdst: bool,
    abbr: String,
}

/// A parsed TZif zone file. Opaque: the public surface is
/// `Zone::resolve`; the tables stay private.
#[derive(Debug, Clone)]
pub struct Tzif {
    transitions: Vec<(i64, usize)>,
    types: Vec<TtType>,
    /// Type for instants before the first transition: the first
    /// standard-time type, as libc picks.
    pre: usize,
}

impl Zone {
    pub fn utc() -> Zone {
        Zone::Fixed {
            offset_s: 0,
            label: "UTC".to_string(),
        }
    }

    pub fn fixed(offset_s: i64, label: &str) -> Zone {
        Zone::Fixed {
            offset_s,
            label: label.to_string(),
        }
    }

    /// The system local zone: `$TZ` (a zoneinfo name, a path, or
    /// a POSIX fixed-offset string), else `/etc/localtime`,
    /// else UTC. Every failure falls back one step, never fails.
    pub fn local() -> Zone {
        if let Ok(tz) = std::env::var("TZ") {
            let tz = tz.strip_prefix(':').unwrap_or(&tz).trim().to_string();
            if !tz.is_empty() {
                if let Some(z) = load_zone_named(&tz) {
                    return z;
                }
                if let Some(z) = parse_posix_tz(&tz) {
                    return z;
                }
            }
        }
        if let Some(z) = load_tzif(Path::new("/etc/localtime")) {
            return z;
        }
        Zone::utc()
    }

    /// (offset seconds east of UTC, abbreviation) at an instant.
    pub fn resolve(&self, epoch_s: i64) -> (i64, String) {
        match self {
            Zone::Fixed { offset_s, label } => (*offset_s, label.clone()),
            Zone::Tzif(tz) => {
                let idx = if tz.transitions.is_empty() {
                    tz.pre
                } else {
                    match tz.transitions.binary_search_by(|(t, _)| t.cmp(&epoch_s)) {
                        Ok(i) => tz.transitions[i].1,
                        Err(0) => tz.pre,
                        Err(i) => tz.transitions[i - 1].1,
                    }
                };
                let tt = &tz.types[idx];
                (tt.offset_s, tt.abbr.clone())
            }
        }
    }
}

fn load_zone_named(name: &str) -> Option<Zone> {
    let path = if name.starts_with('/') {
        name.to_string()
    } else {
        format!("/usr/share/zoneinfo/{name}")
    };
    load_tzif(Path::new(&path))
}

fn load_tzif(path: &Path) -> Option<Zone> {
    let bytes = std::fs::read(path).ok()?;
    parse_tzif(&bytes).map(|tz| Zone::Tzif(Box::new(tz)))
}

struct TzifCursor<'a> {
    b: &'a [u8],
    at: usize,
}
impl<'a> TzifCursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.at..self.at + n)?;
        self.at += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    fn u32(&mut self) -> Option<u32> {
        let s = self.take(4)?;
        Some(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn i32(&mut self) -> Option<i32> {
        Some(self.u32()? as i32)
    }
    fn i64(&mut self) -> Option<i64> {
        let s = self.take(8)?;
        Some(i64::from_be_bytes([
            s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7],
        ]))
    }
}

/// Parse a TZif file (v1 block, plus the v2 block when the
/// version byte says one follows; the v2 data wins, as libc
/// uses it). Returns None on anything malformed — the caller
/// falls back, a display zone is never worth failing a query.
fn parse_tzif(bytes: &[u8]) -> Option<Tzif> {
    if bytes.len() < 44 || &bytes[0..4] != b"TZif" {
        return None;
    }
    let version = bytes[4];
    let mut cur = TzifCursor { b: bytes, at: 4 };
    let mut parsed = parse_tzif_block(&mut cur, false)?;
    if version >= b'2' {
        // A second header (magic at the cursor) + 64-bit block
        // follows the v1 data; its data wins when it parses.
        if bytes.get(cur.at..cur.at + 4) == Some(b"TZif".as_slice()) {
            let mut cur2 = TzifCursor {
                b: bytes,
                at: cur.at + 4,
            };
            if let Some(v2) = parse_tzif_block(&mut cur2, true) {
                parsed = v2;
            }
        }
    }
    Some(parsed)
}

/// One TZif header + data block. `wide` selects 64-bit
/// transition times (the v2 block).
fn parse_tzif_block(cur: &mut TzifCursor, wide: bool) -> Option<Tzif> {
    cur.at += 16; // version byte + 15 reserved (magic already checked / re-checked by caller offset)
    let isutcnt = cur.u32()? as usize;
    let isstdcnt = cur.u32()? as usize;
    let leapcnt = cur.u32()? as usize;
    let timecnt = cur.u32()? as usize;
    let typecnt = cur.u32()? as usize;
    let charcnt = cur.u32()? as usize;
    let mut transitions: Vec<(i64, usize)> = Vec::with_capacity(timecnt);
    let mut indices: Vec<usize> = Vec::with_capacity(timecnt);
    for _ in 0..timecnt {
        transitions.push((if wide { cur.i64()? } else { cur.i32()? as i64 }, 0));
    }
    for _ in 0..timecnt {
        indices.push(cur.u8()? as usize);
    }
    for (slot, idx) in transitions.iter_mut().zip(indices.iter()) {
        slot.1 = *idx;
    }
    let mut raw_types: Vec<(i64, bool, usize)> = Vec::with_capacity(typecnt);
    for _ in 0..typecnt {
        let off = cur.i32()? as i64;
        let isdst = cur.u8()? != 0;
        let abbrind = cur.u8()? as usize;
        raw_types.push((off, isdst, abbrind));
    }
    let chars = cur.take(charcnt)?;
    let abbr = |i: usize| -> String {
        let start = i.min(chars.len());
        let end = chars[start..]
            .iter()
            .position(|&c| c == 0)
            .map(|p| start + p)
            .unwrap_or(chars.len());
        String::from_utf8_lossy(&chars[start..end]).into_owned()
    };
    let types: Vec<TtType> = raw_types
        .iter()
        .map(|(off, isdst, ai)| TtType {
            offset_s: *off,
            isdst: *isdst,
            abbr: abbr(*ai),
        })
        .collect();
    // Skip leap seconds and the std/ut indicator bytes.
    let leap_size = if wide { 12 } else { 8 };
    cur.take(leapcnt * leap_size + isstdcnt + isutcnt)?;
    if types.is_empty() {
        return None;
    }
    let pre = types.iter().position(|t| !t.isdst).unwrap_or(0);
    Some(Tzif {
        transitions,
        types,
        pre,
    })
}

/// A POSIX TZ string's fixed part: `AKST9`, `EST5EDT` (DST
/// rules ignored — fixed standard offset), `CST-8`,
/// `<AKST>9:30`. POSIX offsets are what you add to local time
/// to get UTC, so the zone offset is the negation.
fn parse_posix_tz(tz: &str) -> Option<Zone> {
    let (name, rest) = if let Some(end) = tz.strip_prefix('<').and_then(|t| t.find('>')) {
        (tz[1..1 + end].to_string(), &tz[end + 2..])
    } else {
        let end = tz.find(|c: char| c.is_ascii_digit() || c == '+' || c == '-')?;
        (tz[..end].to_string(), &tz[end..])
    };
    if name.is_empty() || rest.is_empty() {
        return None;
    }
    let (sign, body) = match rest.strip_prefix('-') {
        Some(d) => (1i64, d), // '-' in POSIX means east of UTC
        None => (-1i64, rest.strip_prefix('+').unwrap_or(rest)),
    };
    // The offset is the leading run of digits and colons; a DST
    // name and rules may follow (ignored — fixed standard time).
    let digits = &body[..body
        .find(|c: char| !(c.is_ascii_digit() || c == ':'))
        .unwrap_or(body.len())];
    if digits.is_empty() {
        return None;
    }
    let mut parts = digits.split(':');
    let h: i64 = parts.next()?.parse().ok()?;
    let m: i64 = match parts.next() {
        Some(p) => p.parse().ok()?,
        None => 0,
    };
    let s: i64 = match parts.next() {
        Some(p) => p.parse().ok()?,
        None => 0,
    };
    if h > 24 || m > 59 || s > 59 {
        return None;
    }
    Some(Zone::fixed(sign * (h * 3600 + m * 60 + s), &name))
}

/// Days since 1970-01-01 → (year, month, day). Hinnant's
/// civil_from_days; valid for the full i64 day range we meet.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

/// (year, month, day) → days since 1970-01-01. The inverse.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400); // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64; // [0, 11]
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn is_leap(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn days_in_month(y: i64, m: u32) -> u32 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap(y) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// A broken-down local instant, for display and hour bucketing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalTime {
    y: i64,
    mo: u32,
    d: u32,
    h: u32,
    mi: u32,
    s: u32,
    label: String,
}

fn local_time(epoch_us: u64, zone: &Zone) -> LocalTime {
    let epoch_s = (epoch_us as i64).div_euclid(1_000_000);
    let (offset, label) = zone.resolve(epoch_s);
    let local_s = epoch_s + offset;
    let days = local_s.div_euclid(86_400);
    let tod = local_s.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    LocalTime {
        y,
        mo,
        d,
        h: (tod / 3600) as u32,
        mi: ((tod % 3600) / 60) as u32,
        s: (tod % 60) as u32,
        label,
    }
}

/// `2026-11-02 11:44:48 AKST`
fn fmt_local(epoch_us: u64, zone: &Zone) -> String {
    let t = local_time(epoch_us, zone);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} {}",
        t.y, t.mo, t.d, t.h, t.mi, t.s, t.label
    )
}

/// ISO 8601 in UTC: `2026-11-02T20:44:48Z`, with a fractional
/// part (trailing zeros trimmed) when the instant is not on a
/// whole second. The portable timestamp for exports — no zone
/// abbreviation to misread, no local offset to lose.
pub fn fmt_iso_us(epoch_us: u64) -> String {
    let t = local_time(epoch_us, &Zone::utc());
    let base = format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        t.y, t.mo, t.d, t.h, t.mi, t.s
    );
    let frac = epoch_us % 1_000_000;
    if frac == 0 {
        format!("{base}Z")
    } else {
        let digits = format!("{frac:06}");
        format!("{}.{}Z", base, digits.trim_end_matches('0'))
    }
}

/// `12h 57m` / `3m 12s` / `45s` / `2d 3h`.
fn fmt_duration(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3600)
    } else if secs >= 3600 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

// ----------------------------------------------------------------
// Time-bound parsing (--since / --until)

/// Parse a `--since` / `--until` value to epoch µs. Accepted:
/// epoch seconds / milliseconds / microseconds (bare numbers,
/// told apart by magnitude), or ISO 8601 —
/// `2026-11-02`, `2026-11-02T11:30[:05[.25]]`, the same with a
/// space separator, with a `Z` or `±HH:MM` suffix, or naive
/// (read in the display zone).
pub fn parse_time_bound(flag: &str, value: &str, zone: &Zone) -> Result<u64, QueryError> {
    let bad = |why: &str| QueryError::BadValue {
        flag: flag.to_string(),
        value: value.to_string(),
        why: why.to_string(),
    };
    let v = value.trim();
    if v.is_empty() {
        return Err(bad("empty value"));
    }
    if v.chars().all(|c| c.is_ascii_digit()) {
        let n: i64 = v.parse().map_err(|_| bad("not a number"))?;
        let us = if n >= 100_000_000_000_000 {
            n // already µs
        } else if n >= 100_000_000_000 {
            n.checked_mul(1_000).ok_or_else(|| bad("out of range"))? // ms
        } else {
            n.checked_mul(1_000_000)
                .ok_or_else(|| bad("out of range"))? // s
        };
        return Ok(us as u64);
    }
    if v.chars().all(|c| c.is_ascii_digit() || c == '.') {
        let s: f64 = v.parse().map_err(|_| bad("not a number"))?;
        if s < 0.0 {
            return Err(bad("before the epoch"));
        }
        return Ok((s * 1_000_000.0).round() as u64);
    }
    parse_iso(flag, v, zone)
}

fn parse_iso(flag: &str, v: &str, zone: &Zone) -> Result<u64, QueryError> {
    let bad = |why: &str| QueryError::BadValue {
        flag: flag.to_string(),
        value: v.to_string(),
        why: why.to_string(),
    };
    let b = v.as_bytes();
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return Err(bad("want YYYY-MM-DD[THH:MM[:SS]] [Z|±HH:MM], or an epoch"));
    }
    let num = |lo: usize, hi: usize| -> Result<i64, QueryError> {
        v[lo..hi]
            .parse()
            .map_err(|_| bad("non-numeric date/time field"))
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)? as u32, num(8, 10)? as u32);
    if !(1..=12).contains(&mo) || d < 1 || d > days_in_month(y, mo) {
        return Err(bad("date out of range"));
    }
    let mut tod_s: i64 = 0;
    let mut frac_us: i64 = 0;
    let mut rest = &v[10..];
    if !rest.is_empty() {
        if !rest.starts_with('T') && !rest.starts_with(' ') {
            return Err(bad("want T or space before the time"));
        }
        rest = &rest[1..];
        let rb = rest.as_bytes();
        if rb.len() < 5 || rb[2] != b':' {
            return Err(bad("want HH:MM"));
        }
        let hh: i64 = rest[0..2].parse().map_err(|_| bad("bad hour"))?;
        let mm: i64 = rest[3..5].parse().map_err(|_| bad("bad minute"))?;
        if hh > 23 || mm > 59 {
            return Err(bad("time out of range"));
        }
        tod_s = hh * 3600 + mm * 60;
        let mut tail = &rest[5..];
        if tail.starts_with(':') {
            if tail.len() < 3 {
                return Err(bad("want :SS"));
            }
            let ss: i64 = tail[1..3].parse().map_err(|_| bad("bad second"))?;
            if ss > 59 {
                return Err(bad("time out of range"));
            }
            tod_s += ss;
            tail = &tail[3..];
            if let Some(frac) = tail.strip_prefix('.') {
                let digits: String = frac.chars().take_while(|c| c.is_ascii_digit()).collect();
                if digits.is_empty() {
                    return Err(bad("bad fractional second"));
                }
                let mut us: i64 = digits.parse().map_err(|_| bad("bad fraction"))?;
                for _ in digits.len()..6 {
                    us *= 10;
                }
                frac_us = us;
                tail = &frac[digits.len()..];
            }
        }
        rest = tail;
    }
    let days = days_from_civil(y, mo, d);
    let naive_s = days * 86_400 + tod_s;
    let epoch_s = if rest.is_empty() {
        // Naive: read in the display zone. The offset is resolved
        // at the naive instant — exact except inside a DST gap or
        // fold, where a query bound is approximate by minutes.
        let (offset, _) = zone.resolve(naive_s);
        naive_s - offset
    } else if rest == "Z" || rest == "z" {
        naive_s
    } else if rest.starts_with('+') || rest.starts_with('-') {
        let sign: i64 = if rest.starts_with('-') { -1 } else { 1 };
        let body = &rest[1..];
        let (hh, mm) = if let Some((h, m)) = body.split_once(':') {
            (
                h.parse::<i64>().map_err(|_| bad("bad zone offset"))?,
                m.parse::<i64>().map_err(|_| bad("bad zone offset"))?,
            )
        } else if body.len() == 4 {
            (
                body[0..2]
                    .parse::<i64>()
                    .map_err(|_| bad("bad zone offset"))?,
                body[2..4]
                    .parse::<i64>()
                    .map_err(|_| bad("bad zone offset"))?,
            )
        } else if body.len() == 2 {
            (body.parse::<i64>().map_err(|_| bad("bad zone offset"))?, 0)
        } else {
            return Err(bad("want ±HH:MM zone offset"));
        };
        if hh > 23 || mm > 59 {
            return Err(bad("zone offset out of range"));
        }
        naive_s - sign * (hh * 3600 + mm * 60)
    } else {
        return Err(bad("trailing characters after the time"));
    };
    let us = epoch_s
        .checked_mul(1_000_000)
        .and_then(|s| s.checked_add(frac_us))
        .ok_or_else(|| bad("out of range"))?;
    if us < 0 {
        return Err(bad("before the epoch"));
    }
    Ok(us as u64)
}

// ----------------------------------------------------------------
// Row parsing — the ledger's JSON, read with the same small
// find-the-key approach as the record codec. The format never
// escapes strings (the writer replaces `"` in messages), so a
// string value ends at the next quote.

fn find_key<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\"");
    let at = line.find(&pat)?;
    let rest = &line[at + pat.len()..];
    let colon = rest.find(':')?;
    Some(rest[colon + 1..].trim_start())
}

/// The raw token text for a key, unparsed — for the float
/// fields (`verdict_latency_ms`, `energy_before`, …), whose
/// written precision is part of the record and must survive
/// an export byte-for-byte rather than be re-rendered.
pub(crate) fn json_token(line: &str, key: &str) -> Option<String> {
    let rest = find_key(line, key)?;
    let end = rest
        .find(|c: char| c == ',' || c == '}' || c.is_whitespace())
        .unwrap_or(rest.len());
    let tok = &rest[..end];
    if tok.is_empty() {
        None
    } else {
        Some(tok.to_string())
    }
}

fn json_str(line: &str, key: &str) -> Option<String> {
    let rest = find_key(line, key)?;
    let rest = rest.strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

pub(crate) fn json_u64(line: &str, key: &str) -> Option<u64> {
    let rest = find_key(line, key)?;
    let end = rest
        .find(|c: char| c == ',' || c == '}' || c.is_whitespace())
        .unwrap_or(rest.len());
    rest[..end].parse::<u64>().ok()
}

pub(crate) fn json_tiles(line: &str, key: &str) -> Option<Vec<u32>> {
    let rest = find_key(line, key)?;
    let rest = rest.strip_prefix('[')?;
    let end = rest.find(']')?;
    let inner = rest[..end].trim();
    if inner.is_empty() {
        return Some(Vec::new());
    }
    inner
        .split(',')
        .map(|p| p.trim().parse::<u32>().ok())
        .collect()
}

#[derive(Debug, Clone)]
pub(crate) struct VerdictRow {
    pub(crate) stream: String,
    pub(crate) frame: u32,
    // wall_us is parsed and required (a verdict without it is
    // malformed) but deliberately not kept as a field: query
    // filters and displays verdicts on capture time, and
    // keeping a second clock in the row invites mixing them.
    // It survives in `raw`, where export reads it.
    pub(crate) t_capture_us: u64,
    pub(crate) changed_tiles: Vec<u32>,
    pub(crate) embeds_this_frame: u64,
    /// The row's original line, verbatim. Export embeds it
    /// whole — full fidelity is then a property of the reader,
    /// not a second serialisation that could drift.
    pub(crate) raw: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ServiceRow {
    pub(crate) event: String,
    pub(crate) msg: String,
    pub(crate) wall_us: u64,
    /// The row's original line, verbatim (see VerdictRow).
    pub(crate) raw: String,
}

#[derive(Debug, Clone)]
pub(crate) enum Row {
    Verdict(VerdictRow),
    Service(ServiceRow),
}

fn parse_row(line: &str) -> Option<Row> {
    match json_str(line, "kind")?.as_str() {
        "verdict" => {
            // wall_us must be present and numeric (the row is
            // malformed without it) but is not kept — see the
            // VerdictRow comment.
            json_u64(line, "wall_us")?;
            Some(Row::Verdict(VerdictRow {
                stream: json_str(line, "stream")?,
                frame: u32::try_from(json_u64(line, "frame")?).ok()?,
                t_capture_us: json_u64(line, "t_capture_us")?,
                changed_tiles: json_tiles(line, "changed_tiles")?,
                embeds_this_frame: json_u64(line, "embeds_this_frame")?,
                raw: line.to_string(),
            }))
        }
        "service" => Some(Row::Service(ServiceRow {
            event: json_str(line, "event")?,
            msg: json_str(line, "msg").unwrap_or_default(),
            wall_us: json_u64(line, "wall_us")?,
            raw: line.to_string(),
        })),
        _ => None,
    }
}

// ----------------------------------------------------------------
// The ledger on disk (read-only)

pub(crate) struct LedgerData {
    pub(crate) stream: Option<String>,
    pub(crate) grid: Option<Grid>,
    /// Verbatim identity from the lens, when recorded.
    pub(crate) identity: Option<String>,
    /// A lens exists but records no identity: a pre-0.1.1 ledger.
    pub(crate) legacy: bool,
    /// The lens declares the source recorded footage
    /// (`source_kind = footage`): frozen-source accounting is
    /// a live-source property (playtest F3), so the summary
    /// does not present frozen stretches for such a ledger.
    /// Absent the line (pre-0.3.1 ledgers), the source reads
    /// as live — the behaviour those ledgers were written under.
    pub(crate) footage: bool,
    pub(crate) verdicts: Vec<VerdictRow>,
    pub(crate) service: Vec<ServiceRow>,
    /// Every parsed row, in ledger (file) order — the split
    /// vectors above lose the interleave, and export's single
    /// CSV table is written in ledger order.
    pub(crate) ordered: Vec<Row>,
    /// 1-based line numbers of rows that did not parse.
    pub(crate) malformed: Vec<usize>,
    /// Raw key = value pairs from state.txt, when present.
    pub(crate) state: Option<Vec<(String, String)>>,
}

fn read_kv(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (k, v) = l.split_once('=')?;
            Some((k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

fn kv_get<'a>(kv: &'a [(String, String)], key: &str) -> Option<&'a str> {
    kv.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

pub(crate) fn read_ledger(dir: &str) -> Result<LedgerData, QueryError> {
    let root = Path::new(dir);
    let meta = std::fs::metadata(root).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            QueryError::NotFound(dir.to_string())
        } else {
            QueryError::Io {
                path: dir.to_string(),
                source: e,
            }
        }
    })?;
    if !meta.is_dir() {
        return Err(QueryError::NotFound(dir.to_string()));
    }
    let events_path = root.join("events.jsonl");
    let events_text = match std::fs::read_to_string(&events_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(QueryError::NotALedger(format!(
                "{dir} holds no events.jsonl — a ledger is a directory the watch or index verbs wrote (lens.txt, state.txt, health.txt, events.jsonl)"
            )));
        }
        Err(e) => {
            return Err(QueryError::Io {
                path: events_path.display().to_string(),
                source: e,
            })
        }
    };
    let mut stream: Option<String> = None;
    let mut grid: Option<Grid> = None;
    let mut identity: Option<String> = None;
    // A ledger is legacy when it records no identity: a lens
    // without the line, or no lens at all (set in the branches).
    let mut legacy = true;
    let mut footage = false;
    if let Ok(lens_text) = std::fs::read_to_string(root.join("lens.txt")) {
        let kv = read_kv(&lens_text);
        stream = kv_get(&kv, "stream_id").map(str::to_string);
        identity = kv_get(&kv, "embedder_identity").map(str::to_string);
        legacy = identity.is_none();
        footage = kv_get(&kv, "source_kind") == Some("footage");
        if let Some(g) = kv_get(&kv, "grid") {
            let (r, c) = g.split_once('x').ok_or_else(|| {
                QueryError::NotALedger(format!("{dir}/lens.txt records grid {g:?}, not RxC"))
            })?;
            let (r, c) = (r.trim().parse::<usize>(), c.trim().parse::<usize>());
            match (r, c) {
                (Ok(r), Ok(c)) => {
                    grid = Some(Grid::new(r, c).map_err(|e| {
                        QueryError::NotALedger(format!("{dir}/lens.txt records grid {g:?}: {e}"))
                    })?);
                }
                _ => {
                    return Err(QueryError::NotALedger(format!(
                        "{dir}/lens.txt records grid {g:?}, not RxC"
                    )))
                }
            }
        }
    } else {
        // No lens: an events-only ledger (the probe in ledger.rs
        // calls this shape Legacy). Its embedder is unknown and
        // its grid was never recorded; `legacy` stays true.
    }
    let state = std::fs::read_to_string(root.join("state.txt"))
        .ok()
        .map(|t| read_kv(&t));
    let mut verdicts = Vec::new();
    let mut service = Vec::new();
    let mut ordered = Vec::new();
    let mut malformed = Vec::new();
    for (i, line) in events_text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue; // a blank line is not a row
        }
        match parse_row(line) {
            Some(Row::Verdict(v)) => {
                if stream.is_none() {
                    stream = Some(v.stream.clone());
                }
                ordered.push(Row::Verdict(v.clone()));
                verdicts.push(v);
            }
            Some(Row::Service(s)) => {
                ordered.push(Row::Service(s.clone()));
                service.push(s);
            }
            None => malformed.push(i + 1),
        }
    }
    Ok(LedgerData {
        stream,
        grid,
        identity,
        legacy,
        footage,
        verdicts,
        service,
        ordered,
        malformed,
        state,
    })
}

// ----------------------------------------------------------------
// Filtering

pub(crate) fn verdict_passes(v: &VerdictRow, f: &QueryFilters) -> bool {
    if let Some(t) = f.tile {
        if !v.changed_tiles.contains(&t) {
            return false;
        }
    }
    if let Some(s) = &f.stream {
        if v.stream != *s {
            return false;
        }
    }
    if let Some(s) = f.since_us {
        if v.t_capture_us < s {
            return false;
        }
    }
    if let Some(u) = f.until_us {
        if v.t_capture_us > u {
            return false;
        }
    }
    true
}

pub(crate) fn service_passes(
    s: &ServiceRow,
    f: &QueryFilters,
    ledger_stream: &Option<String>,
) -> bool {
    // Service rows carry no tiles and no stream field: a tile
    // filter cannot match them, and a stream filter matches
    // only when it names the ledger's own stream.
    if f.tile.is_some() {
        return false;
    }
    if let Some(want) = &f.stream {
        match ledger_stream {
            Some(have) if have == want => {}
            _ => return false,
        }
    }
    if let Some(since) = f.since_us {
        if s.wall_us < since {
            return false;
        }
    }
    if let Some(until) = f.until_us {
        if s.wall_us > until {
            return false;
        }
    }
    true
}

// ----------------------------------------------------------------
// Rendering

pub(crate) fn identity_display(data: &LedgerData) -> String {
    match &data.identity {
        Some(id) if id == "unknown" => "unknown (recorded: no embedder was declared)".to_string(),
        Some(id) => id.clone(),
        None if data.legacy => {
            "unknown embedder (pre-0.1.1 ledger: no identity recorded)".to_string()
        }
        None => "unknown embedder".to_string(),
    }
}

fn tile_annotation(tiles: &[u32], grid: &Option<Grid>) -> String {
    match grid {
        Some(g) => {
            let parts: Vec<String> = tiles
                .iter()
                .map(|&t| {
                    if (t as usize) < g.tiles() {
                        let (r, c) = g.row_col(t as usize);
                        format!("r{r}c{c}")
                    } else {
                        format!("#{t} (outside the lens grid {g})")
                    }
                })
                .collect();
            format!(" ({})", parts.join(", "))
        }
        None => String::new(),
    }
}

fn tile_list(tiles: &[u32]) -> String {
    let parts: Vec<String> = tiles.iter().map(u32::to_string).collect();
    format!("[{}]", parts.join(", "))
}

fn malformed_note(malformed: &[usize]) -> Option<String> {
    if malformed.is_empty() {
        return None;
    }
    let shown: Vec<String> = malformed.iter().take(10).map(usize::to_string).collect();
    let mut s = format!(
        "malformed rows: {} file-wide (lines {}) — counted, never skipped silently; filters cannot apply to rows that do not parse",
        malformed.len(),
        shown.join(", ")
    );
    if malformed.len() > 10 {
        s.push_str(&format!(" and {} more", malformed.len() - 10));
    }
    Some(s)
}

fn header(data: &LedgerData, dir: &str, filters: &QueryFilters, zone: &Zone) -> String {
    let mut t = String::new();
    t.push_str(&format!("ledger   {dir} (read-only)\n"));
    t.push_str(&format!(
        "stream   {}\n",
        data.stream.as_deref().unwrap_or("not recorded")
    ));
    match &data.grid {
        Some(g) => t.push_str(&format!("grid     {g} ({} tiles)\n", g.tiles())),
        None => t.push_str("grid     not recorded (no lens.txt)\n"),
    }
    t.push_str(&format!("embedder {}\n", identity_display(data)));
    t.push_str(&format!(
        "rows     {} verdicts, {} service, {} malformed (file-wide)\n",
        data.verdicts.len(),
        data.service.len(),
        data.malformed.len()
    ));
    t.push_str(&format!("filters  {}\n", filters.describe(zone)));
    t.push_str(
        "times    verdicts on frame capture time (t_capture_us); service rows on write time (wall_us)\n",
    );
    t
}

fn verdict_line(v: &VerdictRow, grid: &Option<Grid>, zone: &Zone) -> String {
    format!(
        "{}  epoch_us {}  frame {}  stream {}  tiles {}{}  embeds {}",
        fmt_local(v.t_capture_us, zone),
        v.t_capture_us,
        v.frame,
        v.stream,
        tile_list(&v.changed_tiles),
        tile_annotation(&v.changed_tiles, grid),
        v.embeds_this_frame
    )
}

fn service_line(s: &ServiceRow, zone: &Zone) -> String {
    if s.msg.is_empty() {
        format!(
            "{}  epoch_us {}  {}",
            fmt_local(s.wall_us, zone),
            s.wall_us,
            s.event
        )
    } else {
        format!(
            "{}  epoch_us {}  {}  {}",
            fmt_local(s.wall_us, zone),
            s.wall_us,
            s.event,
            s.msg
        )
    }
}

/// Pair open/close service events into stretches. An opener
/// while one is already open is ignored (the stretch runs from
/// the first); a closer with no open stretch is counted as an
/// orphan and reported separately — filters can clip the
/// opener out of view, and the reader should say so.
fn stretches(rows: &[&ServiceRow], open: &str, close: &str) -> (Vec<(u64, Option<u64>)>, usize) {
    let mut out = Vec::new();
    let mut open_at: Option<u64> = None;
    let mut orphans = 0;
    for r in rows {
        if r.event == open {
            if open_at.is_none() {
                open_at = Some(r.wall_us);
            }
        } else if r.event == close {
            match open_at.take() {
                Some(start) => out.push((start, Some(r.wall_us))),
                None => orphans += 1,
            }
        }
    }
    if let Some(start) = open_at {
        out.push((start, None));
    }
    (out, orphans)
}

fn render_stretches(
    title: &str,
    found: &[(u64, Option<u64>)],
    orphans: usize,
    zone: &Zone,
) -> String {
    let mut t = format!("{title}\n");
    if found.is_empty() {
        t.push_str("  (none under these filters)\n");
    }
    for (start, end) in found {
        match end {
            Some(e) => t.push_str(&format!(
                "  {} -> {}  ({})\n",
                fmt_local(*start, zone),
                fmt_local(*e, zone),
                fmt_duration(e.saturating_sub(*start) / 1_000_000)
            )),
            None => t.push_str(&format!(
                "  {} -> still open at the last row in the filter\n",
                fmt_local(*start, zone)
            )),
        }
    }
    if orphans > 0 {
        t.push_str(&format!(
            "  note: {orphans} closing row(s) with no opening row in view (the opening row is outside the filter, or predates this ledger)\n"
        ));
    }
    t
}

fn render_default(
    data: &LedgerData,
    verdicts: &[&VerdictRow],
    service: &[&ServiceRow],
    zone: &Zone,
) -> String {
    let mut t = String::new();
    t.push_str("\nTELLS\n");
    if verdicts.is_empty() {
        t.push_str("(no verdicts under these filters)\n");
    }
    for v in verdicts {
        t.push_str(&verdict_line(v, &data.grid, zone));
        t.push('\n');
    }
    t.push_str("\nSERVICE EVENTS\n");
    if service.is_empty() {
        t.push_str("(none under these filters)\n");
    }
    for s in service {
        t.push_str(&service_line(s, zone));
        t.push('\n');
    }
    if data.verdicts.is_empty() && data.service.is_empty() && data.malformed.is_empty() {
        t.push_str("\n(this ledger has no rows yet — silence with no basis is not evidence; see health.txt)\n");
    }
    t
}

fn render_summary(
    data: &LedgerData,
    verdicts: &[&VerdictRow],
    service: &[&ServiceRow],
    zone: &Zone,
) -> String {
    let mut t = String::new();
    let embeds: u64 = verdicts.iter().map(|v| v.embeds_this_frame).sum();
    t.push_str(&format!(
        "\nSUMMARY\ntells in filter   {} verdicts, {} service rows\nembeds in filter  {} calls (filtered verdicts)\n",
        verdicts.len(),
        service.len(),
        embeds
    ));
    // Counts by local hour.
    let mut by_hour: BTreeMap<(i64, u32, u32, u32), (u64, String)> = BTreeMap::new();
    for v in verdicts {
        let lt = local_time(v.t_capture_us, zone);
        let e = by_hour
            .entry((lt.y, lt.mo, lt.d, lt.h))
            .or_insert((0, lt.label));
        e.0 += 1;
    }
    t.push_str("\nby hour (local)\n");
    if by_hour.is_empty() {
        t.push_str("  (no verdicts under these filters)\n");
    }
    for ((y, mo, d, h), (n, label)) in &by_hour {
        t.push_str(&format!("  {y:04}-{mo:02}-{d:02} {h:02}:00 {label}  {n}\n"));
    }
    // Counts by tile: busiest first, tile number breaks ties.
    let mut by_tile: BTreeMap<u32, u64> = BTreeMap::new();
    for v in verdicts {
        for tile in &v.changed_tiles {
            *by_tile.entry(*tile).or_insert(0) += 1;
        }
    }
    let mut tile_counts: Vec<(u32, u64)> = by_tile.into_iter().collect();
    tile_counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    t.push_str("\nby tile\n");
    if tile_counts.is_empty() {
        t.push_str("  (no verdicts under these filters)\n");
    }
    for (tile, n) in &tile_counts {
        let ann = tile_annotation(&[*tile], &data.grid);
        t.push_str(&format!("  tile {tile}{ann}  {n}\n"));
    }
    // Frozen and stopped stretches, from the service rows.
    // Frozen stretches are a live-source accounting (playtest
    // F3): a footage ledger's lens says so, and the section is
    // not presented for it — repeated frames in footage are
    // content, and pairing them into "frozen stretches" would
    // dress a category error as a finding.
    t.push('\n');
    if data.footage {
        t.push_str("frozen stretches (source_frozen -> source_recovered)\n  (not applicable: this ledger is recorded footage — frozen-source accounting is a live-source property)\n");
    } else {
        let (frozen, frozen_orphans) = stretches(service, "source_frozen", "source_recovered");
        t.push_str(&render_stretches(
            "frozen stretches (source_frozen -> source_recovered)",
            &frozen,
            frozen_orphans,
            zone,
        ));
    }
    let (stopped, stopped_orphans) = stretches(service, "stop", "start");
    t.push('\n');
    t.push_str(&render_stretches(
        "service-stopped stretches (stop -> start)",
        &stopped,
        stopped_orphans,
        zone,
    ));
    // Embedder swaps fence eras; list them individually.
    t.push_str("\nembedder swaps\n");
    let swaps: Vec<&&ServiceRow> = service
        .iter()
        .filter(|s| s.event == "embedder_swap")
        .collect();
    if swaps.is_empty() {
        t.push_str("  (none under these filters)\n");
    }
    for s in swaps {
        t.push_str(&format!("  {}\n", service_line(s, zone)));
    }
    // Every service row under the same filters: the lifecycle
    // is part of the story, not a footnote.
    t.push_str("\nservice events\n");
    if service.is_empty() {
        t.push_str("  (none under these filters)\n");
    }
    for s in service {
        t.push_str(&format!("  {}\n", service_line(s, zone)));
    }
    // The ledger's own totals, unfiltered — labelled as such.
    t.push_str("\nledger totals (state.txt, unfiltered)\n");
    match &data.state {
        Some(kv) => {
            for key in [
                "frames_seen",
                "events_published",
                "tiles_embedded",
                "embeds_spent_s",
                "spend_ratio",
                "freshness",
            ] {
                if let Some(v) = kv_get(kv, key) {
                    t.push_str(&format!("  {key} = {v}\n"));
                }
            }
        }
        None => t.push_str("  (no state.txt in this ledger)\n"),
    }
    t
}

// ----------------------------------------------------------------
// The verb

/// Counts behind a rendered query, for callers (and tests)
/// that want the numbers without parsing the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryReport {
    pub text: String,
    pub verdicts: usize,
    pub service_rows: usize,
    pub malformed: usize,
}

/// Read the ledger at `dir` and render the answer. Opens every
/// file read-only and writes nothing.
pub fn run_query(
    dir: &str,
    filters: &QueryFilters,
    zone: &Zone,
) -> Result<QueryReport, QueryError> {
    let data = read_ledger(dir)?;
    let verdicts: Vec<&VerdictRow> = data
        .verdicts
        .iter()
        .filter(|v| verdict_passes(v, filters))
        .collect();
    let service: Vec<&ServiceRow> = data
        .service
        .iter()
        .filter(|s| service_passes(s, filters, &data.stream))
        .collect();
    let mut text = header(&data, dir, filters, zone);
    if filters.summary {
        text.push_str(&render_summary(&data, &verdicts, &service, zone));
    } else {
        text.push_str(&render_default(&data, &verdicts, &service, zone));
    }
    if let Some(note) = malformed_note(&data.malformed) {
        text.push_str(&format!("\n{note}\n"));
    }
    Ok(QueryReport {
        text,
        verdicts: verdicts.len(),
        service_rows: service.len(),
        malformed: data.malformed.len(),
    })
}

// ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn akst() -> Zone {
        Zone::fixed(-9 * 3600, "AKST")
    }

    /// Anchors checked against `date(1)`: 1793652288 is
    /// 2026-11-02 20:44:48 UTC = 11:44:48 AKST; Anchorage in
    /// July is AKDT (UTC-8), in November AKST (UTC-9), and in
    /// 1970 was AHST (UTC-10) — the TZif test below covers the
    /// transitions when the system zoneinfo is installed.
    #[test]
    fn civil_conversions_hit_known_anchors() {
        assert_eq!(
            fmt_local(1_793_652_288_000_000, &Zone::utc()),
            "2026-11-02 20:44:48 UTC"
        );
        assert_eq!(
            fmt_local(1_793_652_288_000_000, &akst()),
            "2026-11-02 11:44:48 AKST"
        );
        assert_eq!(fmt_local(0, &Zone::utc()), "1970-01-01 00:00:00 UTC");
        // Round trip over a spread of days, including leap days.
        for (y, m, d) in [
            (1970, 1, 1),
            (2000, 2, 29),
            (2024, 2, 29),
            (2026, 11, 2),
            (2038, 1, 19),
        ] {
            let days = days_from_civil(y, m, d);
            assert_eq!(civil_from_days(days), (y, m, d), "{y}-{m}-{d}");
        }
        assert_eq!(days_from_civil(2026, 1, 1) * 86_400, 1_767_225_600);
    }

    #[test]
    fn iso_format_is_utc_with_trimmed_fraction() {
        assert_eq!(fmt_iso_us(1_793_652_288_000_000), "2026-11-02T20:44:48Z");
        assert_eq!(
            fmt_iso_us(1_793_652_288_012_500),
            "2026-11-02T20:44:48.0125Z"
        );
        assert_eq!(fmt_iso_us(0), "1970-01-01T00:00:00Z");
        assert_eq!(fmt_iso_us(1), "1970-01-01T00:00:00.000001Z");
    }

    #[test]
    fn tzif_zone_reads_real_transitions_when_installed() {
        let Some(zone) = load_tzif(Path::new("/usr/share/zoneinfo/America/Anchorage")) else {
            return; // zoneinfo not installed on this host; the fixed-zone tests still stand
        };
        // November: AKST, UTC-9. July: AKDT, UTC-8.
        assert_eq!(
            fmt_local(1_793_652_288_000_000, &zone),
            "2026-11-02 11:44:48 AKST"
        );
        let july_us = 1_783_200_000_000_000u64; // 2026-07-03 approx
        let (off, label) = zone.resolve((july_us / 1_000_000) as i64);
        assert_eq!((off, label.as_str()), (-8 * 3600, "AKDT"));
        // 1970 predates the file's modern rules: AHST, UTC-10.
        let (off0, label0) = zone.resolve(0);
        assert_eq!((off0, label0.as_str()), (-10 * 3600, "AHST"));
    }

    #[test]
    fn posix_tz_strings_parse_to_fixed_zones() {
        let z = parse_posix_tz("AKST9").unwrap();
        assert_eq!(z.resolve(0), (-9 * 3600, "AKST".to_string()));
        let z = parse_posix_tz("CST-8").unwrap();
        assert_eq!(z.resolve(0), (8 * 3600, "CST".to_string()));
        let z = parse_posix_tz("<AKST>9:30").unwrap();
        assert_eq!(z.resolve(0), (-(9 * 3600 + 1800), "AKST".to_string()));
        assert!(parse_posix_tz("EST5EDT").is_some());
        assert!(parse_posix_tz("").is_none());
        assert!(parse_posix_tz("9").is_none());
    }

    #[test]
    fn time_bounds_accept_epochs_and_iso() {
        let z = akst();
        // Bare epochs, told apart by magnitude.
        assert_eq!(
            parse_time_bound("--since", "1793652288", &z).unwrap(),
            1_793_652_288_000_000
        );
        assert_eq!(
            parse_time_bound("--since", "1793652288000", &z).unwrap(),
            1_793_652_288_000_000
        );
        assert_eq!(
            parse_time_bound("--since", "1793652288000000", &z).unwrap(),
            1_793_652_288_000_000
        );
        // ISO with Z, with an explicit offset, naive in the zone,
        // and date-only (local midnight).
        assert_eq!(
            parse_time_bound("--since", "2026-11-02T20:44:48Z", &z).unwrap(),
            1_793_652_288_000_000
        );
        assert_eq!(
            parse_time_bound("--since", "2026-11-02T11:44:48-09:00", &z).unwrap(),
            1_793_652_288_000_000
        );
        assert_eq!(
            parse_time_bound("--since", "2026-11-02 11:44:48", &z).unwrap(),
            1_793_652_288_000_000
        );
        assert_eq!(
            parse_time_bound("--since", "2026-11-02", &z).unwrap(),
            parse_time_bound("--since", "2026-11-02T09:00:00Z", &z).unwrap()
        );
        // Fractional seconds survive.
        assert_eq!(
            parse_time_bound("--since", "2026-11-02T20:44:48.5Z", &z).unwrap(),
            1_793_652_288_500_000
        );
        // Refusals are BadValue, never a guess.
        for bad in [
            "",
            "yesterday",
            "2026-13-01",
            "2026-02-30",
            "2026-11-02T25:00",
            "-5",
        ] {
            match parse_time_bound("--since", bad, &z) {
                Err(QueryError::BadValue { .. }) => {}
                other => panic!("{bad:?} must be a BadValue, got {other:?}"),
            }
        }
    }

    #[test]
    fn rows_parse_and_malformed_rows_are_identified() {
        // The P5 golden row's load-bearing shape, verbatim field
        // order as the writer emits it.
        let golden = "{\"kind\":\"verdict\",\"stream\":\"golden-stream\",\"frame\":7,\"wall_us\":1793652288000001,\"t_capture_us\":1793652288000000,\"verdict_latency_ms\":65.500,\"changed_tiles\":[5, 6],\"focus\":[5, 6],\"lineage_mask\":31,\"stage_ms\":{\"perceive_decide\":1.000,\"direct\":0.500,\"refine\":0.250,\"publish\":0.100},\"embeds_this_frame\":2,\"energy_before\":0.5000,\"energy_after\":0.3000,\"tiles\":[{\"tile\":5,\"raw\":0.9000,\"smoothed\":0.8000,\"changed\":true,\"stale\":false,\"lineage_mask\":31}]}";
        match parse_row(golden) {
            Some(Row::Verdict(v)) => {
                assert_eq!(v.stream, "golden-stream");
                assert_eq!(v.frame, 7);
                assert_eq!(v.changed_tiles, vec![5, 6]);
                assert_eq!(v.embeds_this_frame, 2);
                assert_eq!(v.t_capture_us, 1_793_652_288_000_000);
            }
            other => panic!("golden row must parse, got {other:?}"),
        }
        let svc = "{\"kind\":\"service\",\"event\":\"source_frozen\",\"msg\":\"frame image stopped changing\",\"wall_us\":1793652288000000}";
        match parse_row(svc) {
            Some(Row::Service(s)) => {
                assert_eq!(s.event, "source_frozen");
                assert_eq!(s.msg, "frame image stopped changing");
            }
            other => panic!("service row must parse, got {other:?}"),
        }
        // None of these may parse — each is malformed, counted.
        for bad in [
            "not json at all",
            "{\"kind\":\"mystery\",\"wall_us\":1}",
            "{\"kind\":\"verdict\",\"stream\":\"s\"}", // missing the load-bearing fields
            "{\"kind\":\"service\",\"event\":\"start\"}", // no wall_us
            "{\"kind\":\"verdict\",\"stream\":\"s\",\"frame\":1,\"wall_us\":2,\"t_capture_us\":3,\"changed_tiles\":\"wide\",\"embeds_this_frame\":0}",
        ] {
            assert!(parse_row(bad).is_none(), "must not parse: {bad}");
        }
    }
}
