# The tile grid — why 4×4 is the default, and what changing it trades

The grid is configurable: `grid_rows` / `grid_cols` in the service
config, `--grid RxC` in the ingest, and the two must agree. 4×4 is
the default for one reason only: **it is the configuration every
measured number in this project's record was taken on.** Change
the grid and the record stops applying to your run until you
re-measure. This document is the record, with its sources, so the
trade is a decision rather than a surprise.

Tile numbers are row-major cells of the configured grid
(tile = row × cols + col). Every ledger writes its grid into
`lens.txt` (`grid = 4x4`); a verdict row is only interpretable
against that line. That is how indexes built by strangers stay
meaningful when the grid differs: the format carries its own
geometry.

## The measured configuration

Working frames are normalised to 512×512 by the ingest, so on
4×4 each tile is a 128×128 px crop — a comfortable input for the
I-JEPA embedder the measurements were taken with, large enough
to contain recognisable scene content, small enough that a
person crossing a room moves several tiles in sequence.

**Wave 3, real embeddings (I-JEPA via HTTP, latency p50 173 ms /
p95 200 ms per tile):**

- Embed-everything vs diff-gated on the full material:
  **576 embeds / 119.6 s → 312 embeds / 55.0 s (−54%)**, with
  frame-verdict agreement of **1.0000** between the two
  strategies on the interleaved-cuts sequence.
- On the gradual turntable sequence the gate kept **23/192
  embeds (−88%)** and stayed near-silent (2/192 tiles flagged).
- Every hard cut in the interleaved sequence flagged **16/16
  tiles** on the landing frame; scoreboard precision / recall
  **1.000 / 1.000**.
- The refine step (one Laplacian smoothing over exactly this
  grid's adjacency) ablated at precision **0.60 → 1.00**, recall
  1.00: a lone noisy tile is pulled toward its four neighbours;
  a true 16-tile change is not.

**Slice 1, staged acceptance (real-photo composites):**

- Cut at frame 4: verdict, 16/16 tiles, 3,347.6 ms, 16 embeds.
- Object pasted over tiles {5,6} at frame 7: verdict naming
  **exactly {5,6}**, 392.8 ms, 2 embeds. The object was
  256×128 px — precisely two tiles. Finer localisation than
  the grid is impossible by construction; this run shows the
  grid's resolution being *reached*, not approximated.
- Embedding-space distances behind those verdicts: cut min
  0.299 / mean 0.491; object tiles 0.588 / 0.651; the loudest
  non-object tile 0.027. Total spend ratio **0.118** vs
  embed-everything.
- The still-room soak calibration: 1,200 frames, zero
  steady-state nominations, spend ratio **0.0008** — on 4×4,
  with per-tile baselines.

**Cost ceiling:** 16 tiles × ~173 ms ≈ 2.8 s to embed one whole
frame. The diff gate exists because even 16 embeds per frame is
expensive; it is also why the grid cannot grow casually (below).

**Gate behaviour is grid-coupled:** at gate 3.0, busy real
photos nominate 13–16 of 16 tiles per frame — on busy material
the gate saves little and per-stream calibration is real work.
The gate is measured to be **a ranker, not a separator**
(nominated tiles' drift vs skipped tiles' drift overlap), which
is a statement about 128×128 tiles of these scenes.

**The refine step size is grid-coupled too:** the 4×4 grid
graph's λmax ≈ 7.24, so α < 0.138 keeps the smoothing step a
strict energy descent; the default α = 0.1 sits inside that
with margin. A larger grid's λmax rises toward 8 (α = 0.1 still
descends, margin thinner); the sheaf layer validates α against
the graph's measured spectral bound and refuses rather than
clamping.

## What a different grid trades

**Finer (e.g. 8×8 = 64 tiles, 64×64 px each):** localisation
improves to 64 cells. Costs: the embed-everything ceiling
quadruples (~11 s/frame at the measured latency) so the gate
becomes load-bearing exactly where it is known to be a ranker,
not a separator; each tile carries a quarter of the pixels, so
per-tile embeddings and diffs are noisier; the staged-object
class of event splinters across more, weaker tiles. None of
the numbers above survive; the acceptance battery must be
re-run and re-published for the new grid before its verdicts
mean what 4×4's mean.

**Coarser (e.g. 2×2 = 4 tiles, 256×256 px each):** the ceiling
quarters (~0.7 s/frame) and per-tile statistics get sturdier.
Costs: localisation degrades to quadrants — the staged object
that exactly filled two 128-px tiles would share one 256-px
tile with its background and move that tile's embedding by
correspondingly less; "where in the frame" stops being a
useful answer at exactly the event sizes a household camera
mostly produces.

**Non-square frames / grids:** the ingest normalises to
512×512 today, so grid cells are square in the working frame
regardless of the source's aspect. A grid like 3×5 is legal
(validation: each side 1–16, at least 2 tiles) and the cascade,
refine graph, and ledger all follow it; no measurement in the
record covers it.

## The rule

Ship 4×4 unless your scene, embedder, or budget says otherwise
*and* you re-run the fixture acceptance
(`crates/telltape/prove_fixtures.py` for the gate layer, the
staged end-to-end run for verdicts) on your grid. The grid is
written into every ledger, so a grid you choose is a grid your
future readers can still interpret.

*Sources: origin repo (SI-Thor/rolling-perception, private):
`docs/RESULTS.md` wave-3 rows; `experiments/wave3/`;
`slice1/core/README.md` (staged acceptance, probe distances,
gate notes); `slice1/PROART-CAMERA.md` (soak calibration);
extraction plan §7–§8. Numbers are quoted, not re-derived.*
