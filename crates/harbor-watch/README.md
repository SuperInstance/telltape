# harbor-watch — rolling visual change detection over I-JEPA embeddings

Wave 3 of SI-Thor/rolling-perception: the first **real workload** through
the machinery. A driver on the Thor embeds webcam frames tile-by-tile
(4×4 grid) with I-JEPA (the JEV rack, :8077) and streams records; this
crate decides, per tile and per frame, what changed.

```json
{"seq":0,"frame":0,"tile":0,"t_arrival_us":123456,"diff_score":0.001,"embedding":[1280 floats]}
```

## The cascade (all on `rolling-substrate-core` rings, full 1280-dim)

| stage | what it does | ring (dimension) |
|---|---|---|
| perceive | publish the tile's embedding, untruncated | ingest (dim) |
| decide | cosine distance vs the tile's previous *published* embedding → change score | decide (dim + 1: score + embedding) |
| direct | top-4 most-changed tiles = the frame's focus set (published in verdicts; does **not** gate compute in this version — gating by focus is future work, not a claim) | focus (16) |
| refine | ONE Laplacian step (α = 0.1) over the frame's 16 change scores on the 4×4 grid graph; energy before/after reported | score (16) |
| publish | per-tile verdict: `changed` iff fresh **and** smoothed score ≥ threshold (default 0.25) | publish (2) |

The stale rule, learned from a failing test: a change verdict is a claim
about *this* frame. A stale tile (no record this frame — the CHANGED
strategy does this routinely) whose last known score is still the cut
score must not re-fire as a fresh change two frames later. Stale tiles
publish *stable*, with their last known score still reported and
labelled `stale`.

## The ablation (the point of refine, measured — fixture, 1280-dim)

One Laplacian step exists to stop a lone noisy tile firing alone. On the
synthetic fixture (cut at frame 4, tiles {3,7,11}; a one-frame noise
spike at frame 6; sustained sub-threshold drift frames 8–11):

| scorer | TP | FP | FN | precision | recall |
|---|---|---|---|---|---|
| raw cosine score | 3 | 2 | 0 | 0.60 | 1.00 |
| after one Laplacian step | 3 | 0 | 0 | **1.00** | 1.00 |

The 2 raw FPs are the spike's out-and-back pair (decide compares against
the previous *published* embedding, spike included). Smoothing rescues
both: the spike's neighbours are stable and pull it under threshold,
while the cut tiles — adjacent to each other — keep each other up.
That is the mechanism working *on this fixture*; the harness
(`ablation::evaluate` + `--truth`) is the deliverable, so the same
question can be asked of real driver data the same way.

## `--compare`: ALL vs CHANGED strategy

Definitions matter more than numbers, so: the reference change frame F*
is the first frame ALL calls changed; t_anchor is F*'s earliest arrival;
a strategy's TTFC is (its latest F* record arrival) − t_anchor — stream-
time until it held everything its F* verdict rests on, or `"missed"`.
Fixture result: **TTFC ALL 6,000 µs vs CHANGED 4,400 µs**, tile verdict
agreement 1.00 (48 pairs), wall-clock 0.72 ms vs 0.06 ms for 192 vs 20
records. Frames with no records at all produce no verdicts — a sparse
file cannot prove a frame existed, and the pipeline says so instead of
inventing frames.

## Usage

```sh
export PATH=$HOME/.cargo/bin:$PATH   # cargo is off the default PATH on our boxes
cargo test                            # 8 tests, ground-truth fixture at 256- and 1280-dim

cargo run --release -- --make-fixture fixtures/harbor-fixture-1280.jsonl --dim 1280
cargo run --release -- fixtures/harbor-fixture-1280.jsonl \
    --out verdicts.jsonl --truth fixtures/harbor-fixture-1280.jsonl.truth.jsonl
cargo run --release -- --compare all.jsonl changed.jsonl
```

The driver schema, fixture script, and ground truth are documented in
`src/record.rs` and `src/fixture.rs`. The fixture is deterministic
(seeded SplitMix64), so its ground truth is regenerable anywhere.
