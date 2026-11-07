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

## Slice 3.5 — `index`: existing footage → ledger (2026-11-02, continuation)

Plan §3.1 lists `index` in the toolkit surface as
*extract + new-thin*: the replay path already existed (live ≡
batch parity, the staged/wave3 sources are replays); the new
part is a file frame source. Built, because the plan requires
it — `query`/`export` remain refused-by-name (the plan lists
them, this build's mandate named `index`; they are recorded
here as the known remainder, not silently dropped).

- Ingest `--source file` (`ingest.py::file_frames`): a video
  file (decoded by ffmpeg at `--fps` into a temp dir; footage
  untouched), an image sequence (directory or glob), or a
  single image. Frames go through the *identical* `Emitter`
  path as live sources — same tiling, gate, records, freshness
  stamps. Capture times are the footage's own: frame k at
  start + k/fps, start = `--start-us` when given, else file
  mtime − probed duration, else mtime — the approximation is
  documented in the ingest header, not hidden.
- `Emitter.frame` takes an optional capture timestamp; live
  sources still stamp arrival time. File replay does not pace
  itself by cadence — indexing is batch work.
- Rust: `telltape::index::index_file` composes the ingest
  command from the watch config itself (embedder statement:
  `embed_url` set → generic HTTP adapter with
  `embed_field`/`embed_key`/`embed_dim`; unset → the test
  double; `ingest_script` from config or `$TELLTAPE_INGEST`,
  never a guessed home path) and runs the *identical* service
  loop. New config keys: `embed_dim`, `embed_field`,
  `embed_key`, `gate_diff`, `ingest_script`, `index_fps`,
  `index_start_us`. CLI: `telltape index <footage> --config
  <file> [--fps N]`.
- Because the loop is shared, freshness accounting applies to
  footage too: a static stretch of a recording accrues repeats
  and receipts `source_frozen` — the ledger describes the
  footage, and a frozen stretch is frozen, whoever recorded it.

Verification: unit tests for the composed command (4) + an
integration test — 5 identical stills then a held hard cut,
indexed from an image-sequence directory through the real
Python ingest and the test double: **8 frames, a 16/16 verdict
at frame 5, a `source_frozen` receipt**. Workspace: **92
passed, 0 failed**, clippy 0 warnings, fmt clean.

## Slice 4 — the docs set (2026-11-02, continuation)

- `README.md` — the front door: the seek-index idea in the
  first screen, the staged-acceptance quickstart (test double,
  labelled as proving pipeline+ledger, not perception), an
  `index` quickstart, the verb table with `query`/`export`
  honestly marked not-built, and a "what this is not" section
  carrying the anti-bloat doctrine and the open trust loop.
- `docs/LEDGER-FORMAT.md` — the four files field-by-field as
  the code writes them *now* (including slice 2's freshness
  fields and slice 3's `grid` line in lens.txt), the soak
  placeholder quoted verbatim, the record-stream schema, and
  `jq` recipes standing in for the unbuilt `query`.
- `docs/EXAMPLES.md` — plan §8's five examples (staged
  acceptance; wave-3 C; the soak **in progress**, frozen-feed
  incident included, with a note that its defect is G1 and the
  fix ships in this repo; the from-the-chair conversation,
  Qwen3-Coder-30B-A3B named as the bench winner; the
  morning-arrival reconstruction — choreography yes, scene
  no), each with its exposed limit and origin source.
- `docs/BRING-YOUR-OWN-EMBEDDER.md` — the seam contract
  (request/response/width/latency/failure semantics), the
  three shipped adapters, a stdlib service sketch, and the
  I-JEPA note written without asserting terms we didn't read:
  never bundled, the operator brings it under their own.
  Two honest statements the doc makes because they are true:
  the plan's in-process ONNX adapter is **not shipped** (the
  HTTP shape is the route), and thresholds calibrated on
  I-JEPA do not transfer to another embedder unmeasured.
- `SECURITY.md` — the ledger as occupancy metadata; the sharp
  edge named first (`ingest_cmd` is operator-trusted shell);
  reporting route that does not pretend a public tracker
  exists yet. `CONTRIBUTING.md` — subtraction doctrine,
  numbers-carry-sources, format-is-contract, the battery.

Docs describe the code as it stands after slices 2–3.5; the
README's acceptance expectation (18 / 2 / 34) is re-verified
against the shipped binary in the slice-5 stranger walk.

## Slice 5 — the proof battery, cold (2026-11-02, continuation)

All in this repo, on this machine, against the shipped code:

- `cargo test --workspace`: **93 passed, 0 failed** (66 at
  extraction → 67 slice 2 → 87 slice 3 → 92 slice 3.5 → 93
  with this slice's regression test).
- `cargo clippy --workspace --all-targets`: **0 warnings.**
  `cargo fmt --all --check`: clean.
- `prove_fixtures.py` (P2, gate layer): **ALL PASS**.
- **Staged acceptance through the binary** (P3, test double):
  `telltape watch` on the bundled staged sequence — **18
  frames, 2 events, 34 embeds**; frame 4 verdict = 16/16
  tiles, frame 7 = exactly {5,6}; drift silent; spend ratio
  **0.1181**; health `freshness = live`.
- **Frozen-stream run** (P6 through the binary): 1 novel + 5
  repeated frame_ends over stdin — 6 frames, 0 events, health
  `freshness = frozen`, `silence_meaningful = false`, and a
  `source_frozen` receipt in events.jsonl.
- **`index` battery**: the 18 staged frames exported as a PNG
  sequence and indexed — **18 / 2 / 34, identical verdicts**
  (replay parity end-to-end). The same frames encoded as an
  H.264 mp4 and indexed at 2 fps — again **18 / 2 / 34**,
  spend 0.1181 (the gate's diffs survived the lossy round
  trip unchanged on this material). Note: for indexed
  footage, `verdict_latency_ms` measures processing lag
  behind the footage's own timeline, not live latency — the
  field's definition in LEDGER-FORMAT says capture → write,
  and for a batch run those are far apart. Recorded here so
  nobody quotes an index run's latency as a live number.
- **Stranger walk** (P7): fresh `git clone` of this repo,
  empty `$HOME`, README only. Two stumbles, both fixed in
  this slice:
  1. `pip install -r requirements.txt` is refused on
     PEP 668 systems — README now builds a venv and says why.
  2. **A real defect:** an ingest that died at startup (in
     the walk, a missing Python dep) produced a *clean
     zero-frame success* — EOF read as "stream over." The
     service now checks the ingest's exit status when the
     stream ends by EOF and fails the run typed
     (`ServiceError::Ingest`) when it is nonzero; runs the
     service itself ends (max_frames / stop file) still kill
     the child without judging it. Regression test:
     `dead_ingest_fails_the_run_but_empty_ingest_is_clean`.
     Same honesty family as G1: an empty result must carry
     its basis.
  Re-walk after the fixes: venv deps install, acceptance
  reproduces 18 / 2 / 34 with the exact verdicts, and an
  index run over a synthesized recording (6 still frames,
  cut, 6 more) yields 12 frames, **1 event (frame 6, 16
  tiles), 32 embeds**, frozen→recovered receipts firing
  mid-run, and `query` refusing by name with exit 2 as
  documented. One walk caveat, honestly: the sandbox's
  network proxy env was preserved during the re-walk (an
  `env -i` first attempt failed PyPI over SSL — a harness
  artifact, not a repo property); the Python deps are the
  only network the walk needs.

The battery the plan asked for (§5) is green at the layers
that exist in this repo: P1 (tests), P2 (gate fixtures), P3
(test-double verdicts), P5 (golden row, slice 3), P6
(frozen), P7 (stranger walk). P4 — the staged acceptance
against a *real* embedder over the generic HTTP adapter —
remains the origin fleet's run to repeat; it is not claimed
here, because this machine has no embedder service, and the
plan's rule is that a layer proves only what it runs.

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

## D1 fix — embedder identity in the ledger (2026-11-02, v0.1.1)

**Defect** (paper-leverage review of arXiv:2610.39302,
`docs/PAPER-LEVERAGE-2610.39302.md` in the origin repo, deficit
D1): the ledger guarded an embedder swap only by embedding
*width*. A same-width swap — 1280-dim model A → 1280-dim model
B — passed every guard while silently invalidating the per-tile
baselines, the gate calibration, and the comparability of rows
across the swap: per the Platonic-representation robustness
results the two spaces are related by at best a rotation, and
the rotation is not recoverable from unpaired data. The ledger
could not even say *which* embedder had produced a tell.

**Fix** (surgical; no new features):

- **Identity is recorded.** `embedder_identity` (`name@version`,
  bare name, or `unknown` when undeclared) is written into
  lens.txt, state.txt and health.txt on every run. Source: the
  config keys `embedder_name` / `embedder_version`. For `index`,
  where the tool composes the ingest command itself, the identity
  is derived from the adapter the config selects — `double@1`
  for the built-in test double (it reports itself honestly as
  the double; the `1` is its hash-expansion algorithm version,
  cross-noted in ingest.py), `http@<endpoint URL>` for the
  generic HTTP adapter (the URL is the only version a generic
  endpoint has). An operator-declared identity always wins.
- **A swap refuses.** Before opening — opening rewrites the
  lens, so the recorded identity is probed first
  (`Ledger::probe_identity`) — a configured identity that
  differs from the recorded one is a typed refusal,
  `ServiceError::EmbedderMismatch`, naming both identities.
  The refused run writes nothing (test-asserted: the lens still
  belongs to the old embedder afterwards). Silent continuation
  on old baselines is impossible.
- **Recalibration is explicit.** `recalibrate = true` in the
  config (or `--recalibrate` on either verb) acknowledges the
  swap: the run proceeds on fresh baselines and receipts an
  `embedder_swap` service row naming old → new — a swap is a
  lens change, and the receipt fences pre-swap verdicts from
  post-swap ones in the same append-only stream.
- **Legacy ledgers are never guessed.** A pre-0.1.1 ledger has
  no identity line; it probes as `Legacy` and its embedder
  reads as `unknown (pre-0.1.1 ledger: no embedder identity
  recorded)`. Opening one under *any* configuration — including
  an undeclared one — requires the same one-time recalibrate
  acknowledgement, after which it is stamped and resumes
  normally. Migration note added to docs/LEDGER-FORMAT.md,
  along with the identity fields and the swap rule.

**Tests**: 93 → 99, all green; clippy `--all-targets -D warnings`
clean; `cargo fmt --check` clean. New: config identity
composition; ledger identity recording + probe (new / known /
legacy, incl. events-only and empty-events edges); index
identity derivation (double / http-URL / operator-declared
wins); integration — same-width swap refused with both
identities named and no writes, acknowledged swap receipts and
re-stamps, same-identity resume unaffected, legacy ledger
refused then stamped by the acknowledgement. Staged acceptance
unchanged, re-run cold: 18 frames, 34 embeds, 2 events, frame 4
= 16/16 tiles, frame 7 = exactly {5,6} — and the CLI refusal
path exercised end-to-end against that ledger (exit 1 naming
'unknown' → 'model-b@2'; `--recalibrate` proceeds and receipts;
resume proceeds).

**Version**: all three workspace crates 0.1.0 → 0.1.1; tagged
v0.1.1 locally. Not pushed — publication stays the gated step,
as before.

## The query verb (2026-11-02, v0.2.0)

`telltape query <ledger-dir>` built — the last decided verb
from the v0.1.0 naming record except `export`, which stays
refused by name. Query reads an existing ledger and prints
plain text: a header (stream, the lens grid, the embedder
identity), the tells (each verdict with local time and epoch,
frame, stream, changed tiles with their row/col under the lens
grid, embed calls), and the service events under the same
filters. `--summary` adds counts by hour and by tile, total
embed calls in filter, frozen stretches (`source_frozen` →
`source_recovered`), service-stopped stretches (`stop` →
`start`), embedder swaps, the full filtered service list, and
the ledger's own state.txt totals labelled unfiltered.

Decisions, and their reasons:

- **Two clocks, named.** Verdicts filter and display on
  capture time (`t_capture_us`); service rows only carry
  `wall_us` and filter on that. The header states the basis on
  every run — mixing the clocks silently is exactly the kind
  of quiet wrongness this tool exists to avoid. `wall_us` is
  still required for a verdict row to parse (its absence is a
  malformed row), but query does not keep or use it.
- **Read-only is structural.** Query only ever opens files
  for reading and holds no `Ledger`. A test snapshots every
  file's bytes (and the directory listing implicitly) around
  default and summary runs and asserts byte-equality; a
  writer round-trip test does the same around a real ledger.
- **Malformed rows are counted, never skipped.** The count is
  file-wide with 1-based line numbers (first ten), because
  filters cannot apply to rows that do not parse — the output
  says so rather than letting a filtered view imply the file
  is clean. Blank lines are not rows.
- **Filters say what they can and cannot match.** `--tile`
  excludes service rows outright (they carry no tiles).
  `--stream` matches a verdict's recorded stream; a service
  row passes a stream filter only when the filter names the
  ledger's own stream, since service rows predate the per-row
  stream field's meaning for them — they belong to the
  ledger's stream by construction.
- **Legacy reads as unknown, never guessed.** No identity in
  the lens prints `unknown embedder (pre-0.1.1 ledger: no
  identity recorded)`; a recorded `unknown` prints `unknown
  (recorded: no embedder was declared)`; an events-only
  ledger (no lens.txt) reads with grid "not recorded" and raw
  tile numbers — no geometry is invented. The grid always
  comes from the lens; a lens grid that is not RxC, or outside
  the record codec's limits, is a typed NotALedger error.
- **Errors are typed and the exit codes follow the house
  pattern.** `QueryError::{NotFound, NotALedger, Io}` exit 1;
  `BadValue` (bad `--since`/`--until`/`--tile`) and other
  usage errors exit 2; the refusal-by-name path for `export`
  is unchanged (exit 2). A directory without events.jsonl is
  NotALedger, not an empty answer.
- **Local time without a dependency.** The workspace carries
  no date/time crate, and query needed TZif parsing anyway to
  be honest about offsets (a fixed-offset guess is wrong
  across a DST boundary). `Zone` resolves `$TZ` (zoneinfo
  name/path, then POSIX string) → `/etc/localtime` → UTC, and
  never fails — worst case it renders UTC and labels it so.
  TZif v1+v2 blocks, the pre-first-transition type chosen as
  libc does (first standard-time type; verified against
  America/Anchorage's 1970 AHST −10:00 as well as modern
  AKST/AKDT). Time bounds accept epochs by magnitude
  (s/ms/µs), bare decimals as seconds, and ISO 8601 with `Z`,
  numeric offsets, or naive local readings; dates are
  validated, so 2026-02-30 is a named refusal, not a rollover.
  No new dependencies — the workspace's hand-rolled standard
  holds.
- **Stretches pair within the filter and say when they
  can't.** An opener closed outside the filter reads "still
  open at the last row in the filter"; a closer with no
  opener in view is counted in a note. Nothing is paired
  across the filter boundary silently.

Doc gap, noted per the format doc's own rule: building query
exposed no *format* ambiguity — every field it reads is
written as LEDGER-FORMAT.md describes. One stale claim: the
format doc's "Reading a ledger with shelf tools" section said
query "is not built yet"; that sentence now names it as the
reader and keeps the jq/grep recipes. No format content
changed.

**Tests**: 99 → 116 passed (+1 pre-existing ignore), all
green; clippy `--all-targets -D warnings` clean; `cargo fmt`
clean. New: 5 unit (civil-date anchors checked against
`date(1)`, TZif transitions from the host's real zoneinfo,
POSIX TZ strings, time-bound grammar, row parsing incl. the
P5 golden row and five malformed shapes) and 12 integration
(hand-written fixture ledgers — known-identity with a frozen
stretch, a swap and a stop/start gap; legacy pre-0.1.1;
events-only; malformed rows counted with line numbers; the
read-only byte-snapshot proof; a writer round-trip through
the real `Ledger`; typed errors; the binary's exit codes,
its `TZ=America/Anchorage` rendering, and the `export`
refusal). Staged acceptance untouched and re-run cold:
18 frames, 34 embeds, 2 events, frame 4 = 16/16 tiles,
frame 7 = exactly {5,6}, drift silent — and query run against
that acceptance ledger as a final live check (identity read
back as the recorded `unknown`, both tells listed).

**Version**: all three workspace crates 0.1.1 → 0.2.0;
tagged v0.2.0 locally. Not pushed — publication stays the
gated step, as before.

## The export verb (2026-11-02, v0.3.0)

The verb set is complete: `export` was the last
named-not-built verb (the plan's "Optional (slice 6+)"
surface, extraction plan §3), refused by name since v0.1.0.
What it is: the ledger rendered as **one portable document
for other tools** — a spreadsheet, a jq pipeline, a video
editor's marker import — so the tells travel without anyone
parsing the ledger's text formats. The ledger is the index;
export is how the index travels.

- **One reader, two renderers.** `export` reads through
  query's `read_ledger` and applies query's filters with
  query's time semantics (verdicts on capture time, service
  rows on write time, inclusive bounds). No second parser
  exists to drift. To make that possible the shared reader
  grew two honest limbs, both additive: parsed rows now
  retain their **original line verbatim**, and the ledger
  data keeps an **ordered row list** (the split verdict /
  service vectors lose the interleave, and CSV is written
  in ledger order).
- **JSON = full fidelity by construction.** Each row is
  embedded verbatim — the exact line the ledger holds — so
  "every field, field-for-field" is a property of the
  reader, not of a re-serialisation. Metadata wraps it:
  stream, grid, embedder identity, the filters in force,
  exported vs in-ledger counts, and the malformed-row count
  **with line numbers**. A legacy ledger exports
  `embedder_identity: null` with `legacy_ledger: true` —
  the unknown marker intact, never an invented value; a
  recorded `unknown` exports as itself.
- **CSV = the flat projection.** One row per event, both
  kinds in one table in ledger order, a `kind` column
  discriminating; scalar fields only (tile lists
  semicolon-joined; per-tile detail and stage timings live
  only in the JSON). Timestamps as epoch µs **and** ISO
  8601 UTC (fraction trimmed of trailing zeros). Float
  fields ride at their written precision — the exporter
  reads the raw token, never re-renders a parsed float.
  RFC 4180 quoting for fields carrying commas, quotes, or
  newlines. The header is a pinned format surface, held by
  a test's literal string.
- **The honesty rules are query's, inherited.** Read-only
  (the byte-snapshot proof now runs around both formats);
  malformed rows counted — in the JSON metadata, and on
  stderr for CSV, whose document has no metadata home;
  typed errors and exit codes mirror query's (1 read
  failure, 2 usage). `--out` writes the file and says so
  on stderr; stdout stays a pure document.
- **Fixtures are shared, literally.** The query tests'
  fixture ledgers moved to `tests/common/` and both verbs'
  suites build on them; a parity test runs eight filter
  sets through `query` and `export` (both formats) and
  demands identical selection. The query suite's
  export-refusal test retired — the refusal it pinned is
  the thing this slice removes.

**Tests**: 116 → 130 passed (+1 pre-existing ignore);
clippy `--all-targets -D warnings` clean; `cargo fmt`
clean. New: 3 unit (escaping, CSV quoting, format names),
11 integration (verbatim fidelity — every filtered source
line exactly once in the document; filter parity with
query; the pinned CSV header and the frame-205 row's cells
including ISO forms; comma-quoting round-trip; read-only
proof; legacy identity; malformed counting in both
formats; typed errors; the binary's stdout purity, --out
behaviour, exit codes, and CSV stderr note), 1 unit in
query (ISO anchors). Staged acceptance untouched and
re-run cold: 18 frames, 34 embeds, 2 events, frame 4 the
cut, frame 7 = exactly {5,6}, drift silent — and export
run against that acceptance ledger in both formats as a
final live check.

**Version**: all three workspace crates 0.2.0 → 0.3.0;
tagged v0.3.0 locally. Not pushed — publication stays the
gated step, as before.

## The cold playtest fixes (2026-11-02, v0.3.1)

This release exists because of a playtest, and the log should
say so plainly: after v0.2.0 shipped, a zero-shot agent was
given **only the public repo** and a job ("here's footage —
tell me what changed, where, and when"), instructed to be
uncharitable and to fix nothing. Its report
(`~/workspace/cold-playtest/PLAYTEST-REPORT.md`, artefacts
kept beside it) is the source of every item below. A hired
hand validates; a cold user falsifies — the battery now has
a playtest in it, and this release is what it bought.

- **F1 (blocker) — a degenerate embedder could author a
  perfect-looking, wrong ledger.** The playtester ran the
  BYO doc's own sketch server verbatim — `embed()` returns
  `[0.0] * 1280` — through the HTTP adapter and got the
  canonical acceptance back, digit for digit: 18 frames,
  2 events, 34 embeds, frame 4 = 16/16, frame 7 = {5, 6}.
  The mechanism was in our own scorer: `cosine_distance`
  returned **1.0 — maximal distance — for a zero-norm
  vector**, so every tile the pixel gate nominated scored a
  maximal "changed", and the gate's nominations on staged
  material happen to be the ground truth. The startup probe
  checked width only; health reported the embedder "up".
  The fix is one rule, enforced at every point an embedding
  can enter: *a vector with a non-finite component, a
  non-finite norm, or a zero norm carries no information and
  must never become a score.*
  - The ingest's probe now checks degeneracy alongside
    width, for every adapter through the shared base class —
    the sketch server and a NaN variant refuse at startup,
    loudly, before a frame exists.
  - The cascade refuses such a record typed
    (`PipelineError::DegenerateEmbedding`, frame and tile
    named), in both the batch and live pipelines, before any
    state is touched; the service receipts the stop in the
    ledger as a `degenerate_embedder` service event and
    returns the error. A mid-run degeneracy (healthy through
    frame 0, zeros from frame 4) stops there: tells before
    the frame stand, frame 4 publishes nothing, no quiet
    continuation.
  - Defence in depth: `cosine_distance` now returns
    `Option<f64>` — `None` (no score, the unbaselined-tile
    behaviour) for degenerate inputs — pinned by a unit test
    at the rng level so the 1.0 answer cannot creep back.
  - The hash double was proved unable to trip its own guard
    (black / white / gradient / 200 random tiles + probe).
  - Placement note: per-stream validation lives at the Rust
    seam rather than in the Python emitter deliberately —
    every adapter's records funnel through that one point,
    and only there can the stop be receipted with frame/tile
    context. The Python seam guards startup (probe) and
    width, as before.
- **F2 — the quickstart's identity trap.** The README's
  acceptance config declared no embedder identity, so the
  first ledger a new user created stamped `unknown`; doing
  what LEDGER-FORMAT teaches (declare `double@1`) was then
  refused against that very ledger, while `index` had been
  stamping the truth all along. Now both verbs derive the
  run identity from the adapter the config itself selects
  when undeclared (`Config::run_identity`; `index`'s
  derivation, shared), and the README config declares the
  identity outright — the playtester's exact two-step,
  replayed as a test, is now an ordinary resume.
- **F3 — frozen-source semantics leaked into footage.** A
  static scene in a *file* finished `freshness = frozen`,
  `silence_meaningful = false`, with stderr alarms during
  the run and 0-second "frozen stretches" in
  `query --summary`. The live-feed alarm is a good idea
  misapplied: a file cannot die, and the tool did watch
  every frame. Freshness is now a live-source property:
  footage runs are marked (`source_footage` / the lens's
  `source_kind = footage`), repeats are counted as content
  but can never freeze a footage ledger or withdraw its
  silence's meaning, and `query --summary` presents frozen
  stretches only for live-source ledgers. The existing
  index test that asserted a `source_frozen` receipt for
  identical stills is **rewritten**, with the playtest's
  category-error argument cited in its doc comment —
  verdicts in that test are byte-identical; only the
  receipts moved.
- **F4 — `verdict_latency_ms` read as detection latency in
  index summaries** (29 s "p50" on a file processed in
  3.6 s). It is processing lag behind the footage timeline.
  No field rename (the ledger format is a compatibility
  surface); the run summary now carries
  `verdict_latency_basis` (`capture_to_ledger_write` /
  `processing_lag_behind_footage_timeline`) and the format
  doc labels the field in both worlds.
- **F8 — indexed tells are anchored to the file's mtime,
  and nothing said so in the ledger.** The lens now records
  `time_anchor` (`capture_clock` / `operator_start_us` /
  `file_mtime_minus_duration` / `file_mtime`), stamped by
  the ingest on every frame_end from the derivation it
  actually used, and LEDGER-FORMAT states the consequence:
  an NVR export's mtime is export time, so a `file_mtime*`
  anchor is a labelled approximation — while frame numbers,
  the other seek key, are always exact.
- **Warts.** `acceptance.sh` — cited by fixtures/README
  and prove_fixtures' output, nonexistent (F5) — now exists
  and prints the expected-vs-measured table (the fixtures
  README's sentence about it is corrected to match what it
  runs). Unknown verbs are refused as usage errors, exit 2,
  verbs listed (F6: `telltape bogusverb` used to fall
  through to watch and complain about a missing config).
  The README's verb table gains the TZ note (F7: query
  renders in `$TZ` / system zone / UTC, in that order).

**Tests**: 130 → 144 passed (+1 pre-existing ignore);
clippy `--all-targets -D warnings` clean; `cargo fmt`
clean. New: rng scorer semantics (2), batch refusal (1),
config derivation (1), ledger footage + stamps (2),
service-level degenerate stop with receipt (zero and NaN),
the F2 docs-flow replay, the rewritten index test, the
query footage summary, and `tests/playtest.rs` (5) — the
probe refusals asserted at the ingest's own stderr, the
zero server's verdict-less ledger, the mid-run stop at
frame 4 tile 3 with its receipt, the double's proof, the
unknown verb. Staged acceptance untouched and re-run cold
— through the new `acceptance.sh`: 18 frames, 34 embeds,
2 events, frame 4 = 16/16, frame 7 = exactly {5, 6}, drift
silent, spend 0.1181 — **PASS**, identical numbers, now
under a seam that would have caught the playtest's server.

**Version**: all three workspace crates 0.3.0 → 0.3.1;
tagged v0.3.1 locally. Not pushed — publication stays the
gated step, as before.

## The second cold playtest fixes (2026-11-03, v0.3.2)

v0.3.1 was playtested the same way: a zero-shot agent, only
the public repo, a job, instructions to be uncharitable and
fix nothing. Its report (`~/workspace/cold-playtest-2/
PLAYTEST-REPORT.md`, artefacts kept beside it — including
the collapsed-embedder server and the ledgers it produced)
is the source of every item below. It again found the hole
the hired hands had walked past, and the hole was worse
than F1: F1's embedder was *broken*; this one's was
*plausible*.

- **B1 (blocker) — a collapsed embedder authored a quiet,
  wrong ledger, and the silence was stamped meaningful.**
  The playtest's constant server returns one valid vector —
  finite, right width, unit norm — for every tile. It passed
  the startup probe (which checked width and degeneracy; a
  constant vector is neither wrong-width nor degenerate) and
  every per-call check, completed the run with 0 events and
  exit 0, and health stamped `silence_meaningful = true`
  over footage containing an appearing object and a full
  scene change. Every score was 0 because every comparison
  was a vector against itself.
  The invariant this release implements, stated once:
  **silence may only be stamped meaningful when the
  embedder has demonstrated discrimination on this run's
  actual inputs.** Two demonstrations are now required:
  - *At the probe* (ingest): the embedder must embed two
    synthetic tiles — pure black and pure white, the most
    different inputs a tile can be — at cosine distance
    > 0.01. The epsilon's justification: decision thresholds
    in the record live between 0.05 and 0.30; float32
    near-identical vectors (the playtest's jitter artefact)
    sit at ~1e-6; 0.01 is four orders of magnitude above
    noise and five times below the weakest real geometry in
    the record. Anything that cannot separate black from
    white by more than that will not separate a door opening
    either. The refusal names the collapse and prints the
    measured distance.
  - *At the seam* (service): a variance watch
    (`CollapseWatch`) tracks, per run, the relationship
    between input variation and embedding variation. A tile
    observation counts when its gate diff reaches 10.0
    (material change — 3× the nomination gate; staged cuts
    measure ~64, the drift the gate ignores ~1.2) — and
    convicts when its score stays ≤ 0.005 (identical vectors
    score exactly 0; jitter ≲1e-3; the weakest honest
    geometry in the record scored every real change ≥0.02).
    Twelve varied-but-unmoved observations with **no**
    varied input ever moving an embedding trips it. One
    moved varied observation demonstrates the embedder for
    the whole run, permanently: collapse means *never*
    answering, not answering rarely.
  - **Design call — the run stops.** The alternative was a
    degraded mode: keep recording, stamp every frame
    "untrusted". Rejected, and the reasoning is the F1
    precedent plus one step: the degenerate path already
    established that a broken embedder stops the watch
    rather than authoring under a false premise, and a
    degraded ledger has the worse property that its frames
    cannot be told apart downstream — query and export
    consumers would have to honour a stamp the format has
    no other example of, and the frames already written
    before the trip wear the stamp of the frames before
    them. A typed stop with a receipt (`collapsed_embedder`,
    counts included) is a fact a reader can act on; a
    degraded continuation is a mood. The tripping frame
    itself is not recorded, on the same precedent as
    degeneracy: the frame that proves the embedder broken
    does not get to author state.
  - A uniformly blank scene cannot trip it. The trigger is
    input variation *without* embedding variation — a
    blank wall produces no varied observations at all,
    however constant its embeddings are. The regression
    battery holds all three fixtures: the constant server
    (probe refusal), a near-constant jitter server (probe
    refusal — the check compares distance, not bit-equality),
    a "smart collapse" server that genuinely discriminates
    the probe's extremes and collapses only on real tiles
    (the watch's conviction, `embedder = collapsed` in
    health), and the honest blank-wall run, untouched and
    still meaningful.
- **S3 — `query --summary` invented a service outage for
  every completed run.** Pairing `stop` → `start` within
  the filtered rows made the terminal stop of a normal run
  an open stretch "still open at the last row in the
  filter", plus an orphan note about the run's own opening
  start. The rule now, from the ledger's own semantics:
  `stop` is the final event a run writes, so a stop no
  later start answers is a run *ending*, not a stretch.
  Pairing happens over the full service record; the result
  is then projected onto the filtered view, where the old
  vocabulary stays true of the filter (a stretch whose
  closing start the filter clips away *is* "still open at
  the last row in the filter"; a closing start whose
  opening stop is filtered out *is* an orphan). A completed
  run shows no stretch and no note.
- **S4 — a mid-run transport failure left no trace but a
  traceback.** HTTP 500 at frame 4 of the playtest's run:
  the user's console got the raw `requests` stack, the
  ledger got *no closing receipt at all* (events.jsonl held
  only its `start` row), and health kept claiming
  `embedder = up`, `silence_meaningful = true`. The
  degenerate path was the model — the playtest called its
  receipting exemplary — and the fix gives every abort the
  same treatment:
  - The ingest classifies embedder-call failures at the
    call site — `transport` (the call failed: 500, dropped
    connection) vs `response` (the answer could not be
    read: bad JSON, missing key, wrong width) — and emits
    one `ingest_error` control line before exiting, plus
    one clean line on stderr. No traceback reaches the user
    from the frame loop; other mid-run failures also exit
    on one line.
  - The service receipts the control line on arrival as an
    `embedder_error` service event naming the class, marks
    the run aborted, and the error it returns carries the
    class too. Unclassified deaths (a stream that fails to
    read, an ingest that exits badly with no account of
    itself) get the generic `run_aborted` receipt.
  - Health is now part of the receipt: a run marked aborted
    never stamps silence meaningful, and state.txt carries
    `run_outcome = aborted`.
- **S1 — the contract doc omitted the health requirement,
  and its sketch couldn't have satisfied the contract.**
  The service derives `{origin}/health` from `embed_url`
  and requires a 200 there before a run starts; the BYO doc
  never mentioned it (the playtester found it in source),
  and the doc's sketch server had no GET handler and waved
  at multipart parsing in a comment. The doc now states
  the health requirement in the contract (with the
  `embed_health_url` override), and the sketch implements
  both: `do_GET` for `/health`, and a correct stdlib
  multipart field parser. Also new: **docs/CONFIG.md** —
  the config reference the playtest found missing entirely
  (the decision threshold was discoverable only in
  `config.rs`): every key, its default, and what it does,
  aliases included, linked from the README.
- **S2 — calibration was impossible from the record.**
  Threshold 0.25 is calibrated to I-JEPA's geometry; the
  playtest's luminance embedder scored a full scene change
  at 0.02–0.16, deaf at that threshold — and no fix was
  discoverable, because unpublished frames left no rows:
  the scores calibration most needs existed nowhere in the
  ledger. state.txt and health.txt now carry
  `max_smoothed_score` (the run's strongest tile score,
  published or not) and `near_miss_frames` (silent frames
  whose best score reached ≥ 80% of threshold), and the
  BYO doc's calibration section is rewritten as a loop run
  off those two numbers.
- **S5 + the wart list.**
  - S5: export latency numbers now travel with their
    basis. The playtest's CSV showed 147,566 ms for a 2 s
    stills job — processing lag behind the footage
    timeline, readable only as verdict computation time.
    The JSON export's metadata gains
    `verdict_latency_basis` (the watch summary's own
    vocabulary); CSV, whose header is a pinned format,
    states the basis on stderr — stdout stays byte-pure.
  - W1: the ingest's BrokenPipeError traceback on shutdown
    is gone — SIGPIPE restored to the Unix default, the
    error caught as a clean exit besides. It used to land
    on exactly the degraded runs a user inspects.
  - W3: probe-refused ledgers stamped `embedder = up`. A
    run that never processed a frame never proved its
    embedder for this run; an abort before the first frame
    now stamps `down`.
  - W4: "a black tile → zero vector" is a real embedder
    shape (sparse/luminance features with nothing to fire),
    not a defect; the BYO doc now names it and gives the
    standard remedy (a constant bias component).
  - W5 (partially declined): a `focus` explanation and
    near-miss inspection were listed as missing surfaces;
    the format doc's verdict table already explains focus,
    and S2's counters are the near-miss surface — what was
    actually missing was the config reference, now written.

**Numbers.** Suite: 144 → **163 passed + 1 ignored**
(+6 variance-watch unit tests, +1 score-observability unit
test, +9 playtest-2 integration tests — probe refusals for
the constant and jitter servers asserted at the ingest,
the smart-collapse conviction and the transport-failure
receipting asserted through the whole service, the watch's
three record-driven fixtures — +2 summary-pairing tests,
+1 export-basis test, and the playtest battery's flaky
fixture rebalanced by exactly the probe's one extra call:
its budget comment now does the arithmetic in the open).
`clippy --all-targets -D warnings` and `cargo fmt --check`
clean. Staged acceptance re-run: **18 frames, 34 embeds,
2 events, frame 4 = 16/16, frame 7 = {5, 6}, spend
0.1181 — PASS**, exact — the acceptance now runs *through*
the strengthened probe, and the hash double's discrimination
is proven explicitly in the battery (its black/white
distance asserted past the epsilon), not merely implied by
the probe passing.

**Version**: all three workspace crates 0.3.1 → 0.3.2;
tagged v0.3.2 locally. Not pushed — publication stays the
gated step, as before.

## The score-series sidecar (2026-11-06, v0.4.0)

The instrument change VALIDATION-VERDICT-1 §6 scoped as
the CfC unblock. The validation run's ledger held per-tile
scores only for the 2,243 frames that fired; its 117,292
near-miss frames survived as a counter, so the
sub-threshold run-in a CfC anticipation test grades —
scores climbing toward an event in the frames *before* it
fires — was unrecorded for quiet frames. The scores exist
inside the gate on every frame; they were simply never
written down.

**What shipped.** `telltape watch` (and `index`, which
shares the loop) can now write a score-series sidecar:
one compact JSONL row per frame the gate evaluates —
frame, capture time, gate-diff summary, the frame's best
smoothed score, near-miss and fired flags, the embedder
identity, and every tile's smoothed score. Set
`score_series_path` in the config or pass
`--score-series <path>`; empty (the default) is no
sidecar. The writer is a pure observer tap at the one
seam where the ledger itself reads those values
(`Ledger::record_frame`'s ceiling/near-miss accounting),
so a row cannot disagree with the ledger's own counters.
No scoring, gating, freshness, or collapse-watch code is
touched. An unwritable path fails the run at startup,
never after a day of unrecorded frames. The schema, size
behaviour (~17 MB per 100k frames at 4x4), and bounded
tail-loss contract are documented in
docs/LEDGER-FORMAT.md ("The score series") and
docs/CONFIG.md.

**Numbers.** Suite: 163 → **168 passed + 1 ignored**
(+1 config-parse unit test, +2 writer unit tests, +2
integration tests). The integration pair: (1) a service
run over the synthetic fixture with the sidecar on vs
off — sidecar holds exactly one row per processed frame
in frame order with only the cut frame flagged fired,
and events.jsonl / state.txt are identical across the
two runs once clock-derived fields (wall times, verdict
latency, stage timings) are normalised; (2) a synthetic
run-in — every tile's embedding rotating by a growing
step so the score climbs 0.021, 0.042, ... 0.231 through
the near-miss band without ever firing — where the
ledger publishes zero verdicts and the sidecar records
all twelve frames with the gate's own values (each row's
max checked against the pipeline's reported max to the
row's 4-decimal precision), the near-miss flags landing
exactly on the two band frames. Staged acceptance
re-run in all three modes (off, config key, CLI flag):
**18 frames, 34 embeds, 2 events, frame 4 = 16/16,
frame 7 = {5, 6}, spend 0.1181 — PASS**, exact, every
mode; the sidecar holds 18 rows in both enabled modes,
and a field-level diff of events.jsonl across modes
shows only the five timing fields differing (the
ingest's re-stamped replay clock, wall times, latencies,
stage timings) — every score, tile, and count
bit-identical. `clippy --all-targets -D warnings` and
`cargo fmt --check` clean.

**Version**: all three workspace crates 0.3.2 → 0.4.0.
Not tagged, not pushed — publication stays the gated
step, as before. The CfC anticipation question is now
gradeable on any future run that enables the sidecar;
grading it is a separate job against such a run.
