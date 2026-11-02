//! telltape: the live change-ledger engine.
//!
//! Architecture:
//!
//! - **ingest** (Python, `ingest/ingest.py`): owns frame acquisition
//!   (MJPEG/JPEG-over-HTTP, or file replay of the staged/wave-3
//!   sequences through the identical interface), tiling, the µs
//!   frame-diff gate, and embedding the nominated tiles through a
//!   pluggable embedder seam (generic HTTP, the JEV-rack adapter,
//!   or the built-in test double). Emits record-schema lines +
//!   frame_end control lines.
//! - **this crate**: spawns the ingest, runs the harbor-watch
//!   cascade incrementally ([`live::LivePipeline`], batch-parity
//!   tested), optionally manages the embedder's lifecycle (start if
//!   down, stop on exit iff we started it — only when the operator
//!   configured lifecycle commands), and publishes verdicts to the
//!   per-stream text ledger ([`ledger`]).
//! - **the ledger** is canonical: lens.txt (rhythm + appearance),
//!   state.txt (state + budget meter + coverage), events.jsonl
//!   (service receipts + published verdicts with lineage),
//!   health.txt (embed p50/p99, queue, frame age, freshness —
//!   silence is only "normal" when health says so).

pub mod config;
pub mod ledger;
pub mod live;
pub mod service;
