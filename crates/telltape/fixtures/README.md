# Bundled cold fixtures — the repo-only acceptance material

These are the frames the Slice-1 staged/wave-3 acceptance runs on,
bundled so a fresh clone can reproduce it with **no files from
anyone's home directory** (the graduation rehearsal's blocker S2:
the ingest used to read `~/image-studio/outputs/…` and
`~/sitka-distill/…` directly, and the repo shipped zero images).

## Contents (3.35 MB total)

- `wave3/A-00.png` … `A-05.png` — six wave-3 **burst** photos
  (harbour scenes, the A sequence).
- `wave3/B-00.png` … `B-05.png` — six wave-3 **turntable** frames
  (the B sequence). Enough for sequence C, which interleaves the
  first six of each.
- `patch.png` — the staged rig's "object": a 128×256 crop of
  `A-03.png` at rows 128–256, cols 128–384 in the source frame.
  The rig pastes it at that same rectangle, which covers exactly
  tiles **{5, 6}** of the 4×4 grid.

## Provenance

All frames are **this project's own wave-3 material**, recovered
2026-11-02 from the copies embedded (base64 PNG) in
`experiments/wave3/report/report-C-real.html` — the wave-3
verdicts report carries its frames inline, in sequence-C order
(A, B alternating), which is where these were decoded from. No
external imagery, no licences to clear: generated/rendered for
this project in wave 3 (image-studio outputs + the dock
turntable). The original full libraries (12 burst frames, the
12-frame turntable webp) still live on the Thor and remain
reachable through the explicit overrides below; the bundled six
of each are the cold set, not a replacement archive.

## How paths resolve (ingest.py `fixtures_dir()`)

1. `--fixtures-dir DIR` on the ingest command line,
2. `$TELLTAPE_FIXTURES_DIR`,
3. this directory (resolved relative to the ingest script).

Full-library overrides, also explicit-only:
`CONFLUENCE_WAVE3_A_GLOB`, `CONFLUENCE_WAVE3_B_GLOB`,
`CONFLUENCE_WAVE3_B_FILE` (one multi-frame image).

## What runs cold, and what still needs the rack

`../prove_fixtures.py` runs the staged + wave-3-C sequences from
these fixtures with **no rack** and proves the **gate/diff
layer**: baseline embeds all 16 tiles; the cut frame nominates
all 16; the object frame nominates exactly {5, 6}; every drift
frame nominates none; every C landing nominates all 16. It does
so by replacing only the rack call inside the real `Emitter`
with a recorder — the embeddings in that run are placeholders
and **no verdict claim is made from them**. The verdict layer
(embedding distances → cascade → published events) is exercised
against these same fixtures by `../acceptance.sh`: the
canonical staged acceptance through the built-in test double,
printing the expected-vs-measured table. Verdicts from a real
embedder (I-JEPA on the fleet's rack) carry their own measured
record — see docs/EXAMPLES.md — and are not what this script
re-runs.
