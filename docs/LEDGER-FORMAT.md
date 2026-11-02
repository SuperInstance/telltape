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
| `t_capture_us` | u64 | frame capture time, epoch µs — the seek key into timestamped footage. For `index`, the footage's own timeline (see the ingest header for how the epoch anchor is derived) |
| `verdict_latency_ms` | f64, 3 dp | capture → ledger write, including that frame's embed time |
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
`source_recovered` (see health.txt), and `embedder_swap`
(see "Embedder identity and the swap rule" below).

## Embedder identity and the swap rule

Every ledger records **which embedder produced its tells**:
`embedder_identity` (`name@version`, the bare name when no
version is declared, or `unknown` when the operator declared
none) appears in lens.txt, state.txt and health.txt. The
identity comes from the config keys `embedder_name` /
`embedder_version`; the `index` verb derives it from the
adapter its own config selects when those keys are unset —
`double@1` for the built-in test double (it reports itself
honestly as the double), `http@<endpoint URL>` for the
generic HTTP adapter (the URL is the only version a generic
endpoint has).

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
**`embedder_identity`** (see above); `period_ms` / `jitter_ms`
(frame-arrival rhythm EMAs, or `unknown`); `threshold`;
`alpha`; `gate_diff` (when the ingest reported one);
`frames_observed`; `tile_score_ema` (space-separated per-tile
score EMAs, `null` = never scored).

## state.txt — counters and the budget meter

`stream_id`, `frames_seen`, `frames_novel`, `frames_repeated`,
`freshness`, `frames_changed`, `events_published`,
`last_frame`, `tiles_nominated`, `tiles_embedded`, `coverage`
(embedded ÷ nominated), `embeds_spent_s`,
`embed_everything_estimate_s` (what ungated watching would
have cost at the observed embed latency and this grid),
`spend_ratio`, `embedder` (`up`/`down`), `embedder_identity`,
`embedder_started_by_me`, `soak`, `updated_wall_us`.

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
- `embedder` — `up` / `down`.
- `embedder_identity` — the embedder that produced this
  ledger's tells (see "Embedder identity and the swap rule").
- **`freshness`** — `live` / `frozen` / `unknown`. Freshness is
  proven by *image novelty*: the ingest hashes each frame, and
  a frame identical to its predecessor is a repeat. `frozen` =
  the last 2+ frames were repeats: the poll is succeeding and
  nothing is being seen. `unknown` = no hash data has arrived
  (an ingest too old to stamp it) — a distinct state, never
  dressed as live.
- `frames_novel` / `frames_repeated` / `consecutive_repeats` —
  the novelty counters behind the state.
- **`silence_meaningful`** — true only when the embedder is
  up, frames have been seen, **and the source is not frozen**.
  This line is the difference between "nothing happened" and
  "we don't know." (Its frozen clause is gap G1's fix: in the
  2026-11 soak a dead camera was polled for ~13 h while this
  line read true; the regression test is
  `frozen_source_reads_frozen_not_meaningful_silence`.)

## The record stream (ingest → service)

Not a ledger file, but the other format a user of the toolkit
meets: the ingest writes one JSON record per embedded tile —
`{"seq","frame","tile","t_arrival_us","diff_score","embed_ms",
"embedding":[…]}` — and one control line per frame:
`{"control":"frame_end","frame","t_capture_us","nominated",
"gate_diff","frame_hash","frame_repeat"}`. The codec is
`harbor_watch::record`; `frame_hash`/`frame_repeat` are the
freshness stamps health.txt is computed from.

## Reading a ledger with shelf tools

`query` is a decided verb that is not built yet; until it is,
the format is plain enough to read directly:

```sh
jq -c 'select(.kind=="verdict") | {frame, changed_tiles}' ledger/events.jsonl
grep -c '"kind":"verdict"' ledger/events.jsonl   # tells so far
cat ledger/state.txt                              # spend and coverage
```
