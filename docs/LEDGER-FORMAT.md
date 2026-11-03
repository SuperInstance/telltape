# The ledger format

As written by the code today (`crates/telltape/src/ledger.rs`),
not as aspired to. A ledger directory holds four files. Only
`events.jsonl` is append-only; the other three are rewritten
whole per frame (they are small). Text is canonical — any UI is
a projection, and this document is the contract projections are
built against. The format is pinned by a golden-file unit test
(`ledger::tests::verdict_row_is_the_documented_golden_shape`).

## events.jsonl — the tells

One JSON object per line, two kinds.

**Verdict row** — written *only* when a frame published a
change (silence is the default state of the file):

| field | type | meaning |
|---|---|---|
| `kind` | `"verdict"` | row type |
| `stream` | string | the config's `stream_id` |
| `frame` | u32 | frame number in the source's own sequence — the seek key into file footage |
| `wall_us` | u64 | ledger-write time, epoch µs |
| `t_capture_us` | u64 | frame capture time, epoch µs — the seek key into timestamped footage. For `index`, the footage's own timeline, anchored as the lens's `time_anchor` records (see lens.txt) |
| `verdict_latency_ms` | f64, 3 dp | capture → ledger write, including that frame's embed time. For `watch` that is detection latency; for `index` the capture times are the footage's own timeline, so the same number measures *processing lag behind the footage timeline* — the run summary says which, in its `verdict_latency_basis` field, and **the export carries the basis with the numbers**: the JSON metadata has a `verdict_latency_basis` field, and a CSV export states the basis on stderr (the CSV header is a pinned format and cannot grow it). The ledger field keeps its name in both worlds (format compatibility); the label lives at presentation (playtest-2 S5: a bare CSV column of footage lags reads as verdict computation time) |
| `changed_tiles` | [u32] | tiles published changed, row-major over the ledger's grid (in `lens.txt`) |
| `focus` | [u32] | the director's top-`focus_k` tiles by raw score — the attention record; it does not gate compute |
| `lineage_mask` | u64 | bitmask of cascade stages that touched the verdict (bits 0–4: perceive, decide, direct, refine, publish; a full pass = 31) |
| `stage_ms` | object | per-stage wall ms: `perceive_decide`, `direct`, `refine`, `publish` |
| `embeds_this_frame` | u64 | embedder calls spent on this frame — the cost receipt |
| `energy_before` / `energy_after` | f64, 4 dp | sheaf energy across the refine step |
| `tiles` | [object ×tiles] | per tile: `tile`, `raw` (cosine distance or `null`), `smoothed` (post-refine or `null`), `changed`, `stale`, `lineage_mask` |

**Service row** — lifecycle receipts:
`{"kind":"service","event":…,"msg":…,"wall_us":…}`. Events
include `start`, `stop`, `embedder`, `embedder_start`,
`embedder_stop`, the freshness receipts `source_frozen` /
`source_recovered` (see health.txt), `embedder_swap`
(see "Embedder identity and the swap rule" below), and the
failure receipts — a run's failure is part of its record,
receipted before the run's error returns:

- `degenerate_embedder` — an embedding that cannot carry a
  score (zero norm, non-finite) was refused at a named frame
  and tile, and the run ended there rather than author tells
  from it (playtest F1; the run's error is the typed
  `DegenerateEmbedding` refusal).
- `collapsed_embedder` — the run-time variance watch
  convicted the embedder (playtest-2 B1): tiles whose pixels
  changed materially kept producing embeddings that did not
  move, and no varied input ever moved one. The receipt
  carries the counts; the run's error is the typed
  `EmbedderCollapsed` refusal; health stamps the embedder
  `collapsed`. The tripping frame publishes nothing.
- `embedder_error` — an embedder call failed mid-run and the
  ingest classified it before exiting: the message names the
  class, `(transport)` for a failed call (HTTP 500, dropped
  connection) or `(response)` for an unreadable answer.
  The run is aborted from that point (playtest-2 S4).
- `run_aborted` — the run ended in failure without a more
  specific receipt above: an ingest that died unclassified,
  a record stream that failed to read, a pipeline refusal.
  Closed runs carry `run_outcome = aborted` in state.txt,
  and health never stamps their silence meaningful.

## Embedder identity and the swap rule

Every ledger records **which embedder produced its tells**:
`embedder_identity` (`name@version`, the bare name when no
version is declared, or `unknown` when the operator declared
none) appears in lens.txt, state.txt and health.txt. The
identity comes from the config keys `embedder_name` /
`embedder_version`; when those keys are unset, **both** verbs
derive it from the adapter the config itself selects —
`double@1` for the built-in test double (it reports itself
honestly as the double), `http@<endpoint URL>` for the
generic HTTP adapter (the URL is the only version a generic
endpoint has). (Before v0.3.1 only `index` derived it; a
`watch` run left undeclared stamped `unknown`, and declaring
the true identity afterwards — as this section teaches — was
then refused against the tutorial's own ledger. The cold
playtest found that trap; F2 closed it.)

This exists because the width guard is not enough (deficit
D1): two embedders of the *same width* still live in
different embedding spaces — related by at best a rotation —
so per-tile baselines, the gate calibration, and the
comparability of verdict rows across time are all defined
only *within* one identity. The rule, enforced at open,
before anything is written:

- Same identity as recorded → ordinary resume.
- **Different identity → typed refusal** naming both
  identities. The run writes nothing. To accept the swap,
  the operator sets `recalibrate = true` in the config (or
  passes `--recalibrate`): the run then proceeds with fresh
  baselines and receipts the swap as an `embedder_swap`
  service row naming the old and new identities — a swap is
  a lens change, and the event stream fences the verdicts
  written before it from those written after. Silent
  continuation on old baselines is impossible.

**Migration — pre-0.1.1 ledgers.** A ledger written before
embedder identity existed has no `embedder_identity` line;
its embedder reads as *unknown*, never as "probably the
same one". Opening such a ledger under any configuration —
including an undeclared (`unknown`) one — requires the same
explicit recalibrate acknowledgement, once. The run then
receipts the stamping as an `embedder_swap` from
`unknown (pre-0.1.1 ledger: no embedder identity recorded)`
and the ledger carries a recorded identity from then on.

## lens.txt — the stream's learned normal

`key = value` lines: `stream_id`; **`grid`** (`RxC` — the
geometry every tile number in this ledger is read against);
**`embedder_identity`** (see above); **`source_kind`** —
`live` or `footage` (an `index` run): freshness accounting
applies to live sources only, and readers (e.g. `query
--summary`) present frozen stretches only for live ledgers;
**`time_anchor`** — how the ledger's capture times were
anchored to the epoch: `capture_clock` for live sources
(frames stamped as they arrive); for indexed footage,
`operator_start_us` (the operator supplied frame 0's epoch),
`file_mtime_minus_duration` (the footage file's mtime minus
its probed duration), or `file_mtime`. Indexed tells' wall
times inherit the anchor's truth: for an NVR export, mtime
is export/copy time, so `file_mtime*` anchors are labelled
approximations, while frame numbers — the other seek key —
are always exact. (Ledgers written before v0.3.1 record
neither line; their anchor was the ingest's derivation,
unstamped.) `period_ms` / `jitter_ms`
(frame-arrival rhythm EMAs, or `unknown`); `threshold`;
`alpha`; `gate_diff` (when the ingest reported one);
`frames_observed`; `tile_score_ema` (space-separated per-tile
score EMAs, `null` = never scored).

## state.txt — counters and the budget meter

`stream_id`, `frames_seen`, `frames_novel`, `frames_repeated`,
`freshness`, `frames_changed`, `events_published`,
`max_smoothed_score` (the strongest smoothed score any tile
reached this run, published or not — playtest-2 S2: before
it, a near-miss left no row anywhere, so the scores
calibration most needs were unobservable), `near_miss_frames`
(frames that published nothing whose best tile score reached
≥ 80% of the threshold — the near-miss band the calibration
loop in BRING-YOUR-OWN-EMBEDDER.md reads), `last_frame`,
`tiles_nominated`, `tiles_embedded`, `coverage`
(embedded ÷ nominated), `embeds_spent_s`,
`embed_everything_estimate_s` (what ungated watching would
have cost at the observed embed latency and this grid),
`spend_ratio`, `embedder` (`up`/`down`/`collapsed` — see
health.txt), `embedder_identity`,
`embedder_started_by_me`, `soak`, `updated_wall_us`.
A run that ended in a receipted failure (a degenerate or
collapsed embedder, an embedder transport failure, a dead
ingest) additionally carries `run_outcome = aborted`;
completed runs carry no such line.

In soak mode (`soak = true`) state.txt additionally carries,
verbatim:

```
precision_self_report = placeholder (events fired and spend below; labels pending)
soak_events_fired = N
soak_spend_ratio = R
```

That placeholder is documented here because it is in every
soak ledger: trust accounting does not exist yet. Corrections
(labels) arrive out of band; until a reader computes precision
from them, the honest report is the raw counts, labelled.

## health.txt — the basis of silence

An empty events.jsonl proves nothing by itself. health.txt is
what makes quiet meaningful — or says it isn't:

- `embed_ms_p50` / `embed_ms_p99` / `embed_ms_n` — embedder
  latency window (last 512 calls).
- `frames_pending_max` — deepest ingest backlog observed.
- `last_frame_age_ms` — wall time since the last frame was
  recorded (−1.0 before any frame).
- `embedder` — `up` / `down` / `collapsed`. `collapsed` is
  the variance watch's verdict (playtest-2 B1): the embedder
  answers, promptly and at the right width, but its answers
  do not discriminate this run's inputs. It is neither up
  (it must not vouch for silence) nor down (it is not a
  liveness failure). A run that never processed a frame —
  a probe refusal, a dead ingest — reports `down`, never
  `up`: it never proved its embedder for this run (W3).
- `embedder_identity` — the embedder that produced this
  ledger's tells (see "Embedder identity and the swap rule").
- `max_smoothed_score` / `near_miss_frames` — the run's
  score ceiling and near-miss count, as in state.txt
  (calibration observability, playtest-2 S2).
- **`freshness`** — `live` / `frozen` / `unknown` / `footage`.
  Freshness is proven by *image novelty*: the ingest hashes
  each frame, and a frame identical to its predecessor is a
  repeat. `frozen` = the last 2+ frames were repeats: the poll
  is succeeding and nothing is being seen. `unknown` = no hash
  data has arrived (an ingest too old to stamp it) — a
  distinct state, never dressed as live. `footage` = the
  source is recorded footage (an `index` run): the frozen
  distinction does not apply — a file cannot die mid-watch,
  its repeated frames are content the tool watched, and its
  silence keeps its meaning (`silence_meaningful` below is
  not withdrawn). Before v0.3.1, index runs inherited the
  live accounting and a static scene's ledger ended
  `freshness = frozen` — the playtest's category error, F3.
- `frames_novel` / `frames_repeated` / `consecutive_repeats` —
  the novelty counters behind the state.
- **`silence_meaningful`** — true only when **all** of: the
  embedder is up, it has not collapsed, the run has not
  aborted, frames have been seen, and the source is not
  frozen. This line is the difference between "nothing
  happened" and "we don't know." Each clause is a failure
  that once wore the stamp: the frozen clause is gap G1's
  fix (in the 2026-11 soak a dead camera was polled for
  ~13 h while this line read true; the regression test is
  `frozen_source_reads_frozen_not_meaningful_silence`); the
  collapse clause is playtest-2 B1 (a constant embedder's
  run completed in stamped silence over a changing scene;
  the invariant now is that silence is stamped only when
  the embedder has demonstrated discrimination on this
  run's actual inputs); the abort clause is playtest-2 S4
  (a transport failure left health claiming `up` and
  meaningful over an unreceipted, unclosed run).

## The record stream (ingest → service)

Not a ledger file, but the other format a user of the toolkit
meets: the ingest writes one JSON record per embedded tile —
`{"seq","frame","tile","t_arrival_us","diff_score","embed_ms",
"embedding":[…]}` — and one control line per frame:
`{"control":"frame_end","frame","t_capture_us","nominated",
"gate_diff","frame_hash","frame_repeat","time_anchor"}`. The
codec is `harbor_watch::record`; `frame_hash`/`frame_repeat`
are the freshness stamps health.txt is computed from, and
`time_anchor` is the anchor stamp the lens records (above).
One further control line exists since v0.3.2:
`{"control":"ingest_error","class","msg"}`, emitted once,
on the way out, when an embedder call fails mid-run
(playtest-2 S4) — `class` is `transport` or `response`, and
the service receipts it as the `embedder_error` service row
above before the stream ends.

## Reading a ledger with shelf tools

`query` is the ledger reader (`telltape query <ledger-dir>
[--since T] [--until T] [--tile N] [--stream ID] [--summary]`,
read-only). `export` is the same reader rendering one
portable document for other tools (`telltape export
<ledger-dir> [--format json|csv] [--out FILE]`, same filters,
same time semantics): the JSON carries every row verbatim
under the ledger's metadata (stream, grid, embedder
identity, the `verdict_latency_basis`, counts, malformed
rows counted with line numbers — for CSV the basis is
stated on stderr instead);
the CSV is the flat projection — one row per event in
ledger order, a `kind` column discriminating, timestamps
as epoch µs and ISO 8601 UTC, tile lists semicolon-joined,
per-tile detail and stage timings only in the JSON. The
format is also plain enough to read directly with shelf
tools:

```sh
jq -c 'select(.kind=="verdict") | {frame, changed_tiles}' ledger/events.jsonl
grep -c '"kind":"verdict"' ledger/events.jsonl   # tells so far
cat ledger/state.txt                              # spend and coverage
```
