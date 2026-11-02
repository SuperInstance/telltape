# BUILD-LOG — extraction of the change-ledger core

The process record of the extraction build, written as it happens:
decisions and their reasons, dead ends, deviations, numbers.
Source of truth for scope: `docs/EXTRACTION-PLAN.md` in the origin
repo (SI-Thor/rolling-perception, private), §1/§2 (the IN list and
the dependency surgery) and §10 (Casey's five rulings, 2026-11-02):

1. The standalone tool gets a standalone name: **`telltape`**
   (decided 2026-11-02 — see the naming entry at the end of this
   log; the build ran as `tellstream` through slice 1 and was
   renamed in one search-replace, exactly as structured).
2. NEW repo, clean history. This repo's first commit is the
   extraction; nothing inherits the origin history.
3. Restrictive-licence parts are never incorporated: the seam
   ships, the docs explain how to move the best part in under the
   user's own terms, the shipped default path is licence-clean.
   I-JEPA weights are never bundled or downloaded by this repo.
4. Grid is configurable; 4x4 is the default, and its measured
   rationale ships in `docs/GRID.md`.
5. Licence: MIT OR Apache-2.0 dual, as in the origin tree.

Constraints honoured throughout: no GitHub operations (local git
only; publication is a later, separately gated step); the origin
staging tree is read-only source; the two ruled-out options
(archived embeddings, change-frame snapshots) are not built.

## Slice 1 — extraction + embedder seam (2026-11-02)

- Copied, unmodified, from the origin tree:
  `crates/rolling-substrate-core` (whole crate — rings, sheaf,
  SPSC, lineage, gating), `crates/harbor-watch` (record, pipeline,
  rng, fixture, ablation, its integration tests and 1280-dim
  fixture files), `slice1/core` → `crates/telltape` (src, tests,
  ingest, fixtures 3.3 MB, prove_fixtures.py, test-server.py).
  `harbor-watch/src/main.rs` (the wave-3 file driver bin) was NOT
  carried: the IN list names the library modules; the driver was a
  lane tool for the origin repo's experiments. The library is the
  reusable surface.
- Licences copied from the origin root: LICENSE, LICENSE-MIT,
  LICENSE-APACHE.
- Renames: `confluence-watch` → `telltape` (package, binary,
  library crate `confluence_watch` → `telltape`), test tempdirs,
  env overrides `CONFLUENCE_*` → `TELLTAPE_*` (fresh tool, no
  legacy readers; the old names are not aliased in code — the
  config-file keys for the embedder ARE aliased for one release,
  per plan §2.2, because config files are technician-edited text
  that outlives binaries).
- `service.rs`: the systemctl rack lifecycle is deleted. Lifecycle
  is `embedder_start_cmd` / `embedder_stop_cmd` shell strings,
  empty by default (start nothing, stop nothing). Health is an
  optional configured URL (default: `{embed_url origin}/health`);
  with no URL the run makes no external check — "no working
  embedder, no run" survives as: a configured health URL that
  stays down is a startup error naming the URL.
- `config.rs`: `rack_url` → `embed_url` (no default — the origin
  default dialled the fleet rack's port), `rack_autostart` →
  `embedder_autostart`, `stop_rack_on_exit` →
  `stop_embedder_on_exit`; old keys aliased for one release.
  Ledger state/health lines renamed `rack =` → `embedder =`.
- `ingest/ingest.py`: `Emitter` now takes an embedder object
  behind a small seam — `embed(tile) -> (vector, embed_ms)` —
  with three adapters: `HttpEmbedder` (URL, multipart field,
  response key all from CLI config), `JevRackEmbedder` (the
  fleet rack, a subclass carrying only its URL layout), and
  `HashEmbedder`, the built-in **test double** (SHA-256-expanded
  pseudo-embedding, unit-normalised; labelled in code, CLI help
  and docs as NOT a perception model). The hardcoded
  `assert len == 1280` became `embed_dim` config, verified once
  by a startup probe embed. `prove_fixtures.py` now stubs the
  same seam with a recorder adapter instead of monkey-patching
  `Emitter.embed`. Ingest Python deps declared in
  `crates/telltape/requirements.txt` (numpy, requests, Pillow).
- One origin defect fixed in passing: `prove_fixtures.py`
  asserted fixture paths must not contain `/home/` — a condition
  no checkout under a home directory can satisfy, including the
  origin author's own. The assertion now checks the real
  invariant: default fixtures resolve inside the crate, relative
  to the script.
- Also landed here (consumed by slice 2): ingest stamps every
  frame_end with `frame_hash` (SHA-256 over a 32x32 downsample)
  and `frame_repeat` (byte-identical to the previous frame).

Slice-1 verification (this machine, debug build):
- `cargo test --workspace`: **66 passed, 0 failed** (substrate
  55 incl. property suites, harbor-watch 8, telltape 3
  integration — parity / ledger / service-interface all green
  through the surgery).
- `prove_fixtures.py`: ALL PASS (gate layer: baseline 16, cut
  16/16, object exactly {5,6}, drift none, wave-3 C landings
  16/16).
- End-to-end through the CLI with the test double: 18 staged
  frames → **34 embeds**, verdict frame 4 = 16/16 tiles, frame 7
  = exactly {5,6}, drift silent, lineage 31, spend ratio
  **0.1181** — the plan §5 P3 numbers, reproduced cold.

## Slice 2 — G1: freshness by novelty (2026-11-02, continuation)

The defect, from the origin record (plan §7 G1 / §8 E3): in the
live soak the camera pusher died and the watch polled the
receiver's last frame for ~13 h, counting polls as frames while
`health.txt` claimed `silence_meaningful = true` — `frames_seen`
was a poll counter and `last_frame_age_ms` measured the poll,
not the image.

- Slice 1 already made the ingest stamp every `frame_end` with
  `frame_hash` (SHA-256 over a 32×32 downsample) and
  `frame_repeat` (identical to the previous frame). This slice
  makes the Rust side *consume* them.
- `ledger.rs`: `Stats` gains `frames_novel`, `frames_repeated`,
  `consecutive_repeats`; `Ledger::observe_freshness(repeated)`
  returns the transition (`BecameFrozen` / `Recovered` / none).
  Frozen = **2 consecutive repeats** — one repeat can be a
  static view racing its cadence; a real camera's sensor noise
  makes two identical consecutive frames a dead source, not a
  quiet scene. `health.txt` and `state.txt` now carry
  `freshness = live | frozen | unknown` (unknown = an ingest
  too old to stamp hashes — its own state, never dressed as
  live), the novelty counters, and `silence_meaningful` now
  requires `!source_frozen()` on top of the Panel-1 conditions.
- `service.rs`: per frame_end, repeated = the ingest's
  `frame_repeat` flag when present, else our own hash
  comparison, else (no stamps at all) novel-with-unknown-basis.
  Transitions are receipted as `source_frozen` /
  `source_recovered` service events, so the ledger narrates the
  incident itself.
- **P6 regression test** (`tests/live.rs::
  frozen_source_reads_frozen_not_meaningful_silence`): frame 0
  real, frames 1–5 record-free with one unchanging hash. The
  run still counts 6 frames (polls counted as polls) and 0
  events — and health reads `freshness = frozen`,
  `silence_meaningful = false`, `frames_repeated = 5`.

Verification: `cargo test --workspace` — **67 passed, 0
failed** (66 inherited + P6).

## Slice 3 — G2 typed errors, G4 unit tests, configurable grid (2026-11-02, continuation)

**G2.** Caller errors are typed refusals everywhere on the
extracted core's public surface:

- `harbor_watch::pipeline::PipelineError` (`EmptyRecords`,
  `DimMismatch`, `TileOutOfGrid`, `ZeroDim`) — `run_records` and
  `Pipeline::run` return it; the asserts/`expect` the plan named
  (pipeline.rs) are gone.
- `telltape::live::process_frame` returns
  `Result<FrameOutput, PipelineError>` and validates the whole
  frame *before* touching state.
- `telltape::service::ServiceError` (`Ledger`, `Config`,
  `EmbedderUnhealthy`, `Ingest`, `Pipeline`) replaces
  `Result<Summary, String>` — the last string-typed surface.
- Left as-is, deliberately: constructor invariants
  (`Pipeline::new(dim>0)`) stay asserts, and `Ledger::
  record_frame` keeps `std::io::Result` (already typed). The
  distinction: *who can cause it* — a caller handing bad data
  gets a refusal; a programmer breaking an invariant gets a
  panic with the invariant named.

**G4.** The desert modules now carry unit tests where they sit:
`record.rs` (codec round-trip, named parse errors, line
numbers, grid validation — 5), `pipeline.rs` (the three
refusals + a custom-grid run — 4), `config.rs` (defaults,
aliases, health-URL derivation, key-naming errors — 6),
`ledger.rs` (percentiles, freshness transitions, frozen health,
the P5 golden verdict row, grid-scaled budget baseline — 5).
The P5 golden row lands here: a fixed frame's `events.jsonl`
row is asserted field-by-field, so the published format is
pinned by a test, not by prose.

**Grid.** Runtime `harbor_watch::record::Grid` (rows × cols,
each 1–16, ≥2 tiles, row-major) replaces the compile-time
constant through the cascade, the live pipeline, the ledger
(per-tile EMAs and the embed-everything baseline now scale with
the grid; `lens.txt` writes `grid = RxC` so every ledger
carries its own geometry), the service config
(`grid_rows`/`grid_cols`, validated at load), and the ingest
(`--grid RxC`; the staged object's placement is computed from
the grid and still lands exactly on tiles {5,6} at 4×4). The
default is 4×4 everywhere. `docs/GRID.md` ships the measured
rationale — wave-3 and slice-1 numbers with sources, the
α/λmax coupling, and what finer/coarser grids trade.

**Deviation from plan §2.3, flagged:** the plan froze the grid
as a v1 *format constant*; §10 ruling 4 (later, and Casey's)
ordered configurability with the rationale documented. The
ruling wins. The format concern behind the plan's freeze —
strangers' indexes must stay interpretable — is answered
mechanically instead: the grid is recorded in every ledger's
`lens.txt`, and LEDGER-FORMAT.md states that a verdict row is
read against it.

Also fixed in passing: one inherited clippy MSRV warning in
`rolling-substrate-core/src/spsc.rs` (`u64::is_multiple_of` →
`%`, identical semantics) so the workspace is warning-free.

Verification: `cargo test --workspace` **87 passed, 0 failed**
(67 → 87: +20 unit tests), clippy `--workspace --all-targets`
**0 warnings**, `cargo fmt --check` clean.

## Naming decision — telltape (2026-11-02)

Casey delegated the name pick to Muse; decided the same day:
**telltape**. Recorded here with its reasons, per the process
doctrine:

- **Cold-inference test** (Casey's criterion: an agent reader
  meeting the name cold should be able to infer what the tool
  does): *tell* = it reports what changed; *tape* = the footage
  it watches, live or recorded. Both halves describe the actual
  function — a change-ledger over video the user keeps.
- Rejected: *telltale* — fails descriptiveness (it names a
  symptom, not a function) and is already taken by an analytics
  product and a game studio. *watchlog* was not memorable.
- A web scan found **no existing telltape software product**.
- Vocabulary, binding on all docs from here on: each ledger
  entry is a **tell** ("the ledger holds 14 tells for that
  night"). CLI verbs: **telltape watch** (live source →
  ledger), **telltape index** (existing footage → ledger),
  **telltape query** (read a ledger). This build ships `watch`
  only; the binary refuses `index`/`query` by name rather than
  pretending, until the toolkit slices build them.
- Mechanics of the rename: built as `tellstream` through
  slice 1 per the task brief; renamed in one commit — crate
  directory (`git mv`), package/binary/library names, env
  overrides (`TELLSTREAM_*` → `TELLTAPE_*`), doc and config
  references. Zero `tellstream` references remain (grep-verified,
  excluding build artifacts), and the test suite + staged
  acceptance were re-run green under the new name before this
  entry was written (numbers in the verification commit).
