# telltape

**Watch a video stream — live or recorded — and write a
structured, append-only text ledger of what changed, where in
the frame, when, and how strongly.**

telltape never copies or keeps the footage. The ledger's frame
numbers and timestamps are a **seek index into footage you keep
elsewhere** — an NVR recording, a file on disk — so "show me
everything that happened" becomes "here are the few moments
worth opening, and where they sit in your recording." When
nothing changes, the ledger says nothing, and that silence
carries its basis (the lane is alive, frames are arriving
*and are new*, the embedder is answering), so quiet is
distinguishable from dead.

Each ledger entry is a **tell**. A night with 14 tells is a
night you can review in a minute.

## How it works

```
frames ──► tiles (4×4 default) ──► cheap pixel diff (the gate)
   ──► only nominated tiles are embedded (the expensive step)
   ──► cascade: perceive → decide → direct → refine → publish
   ──► ledger: events.jsonl (the tells) + lens/state/health.txt
```

The cascade scores each tile's embedding against its own
history, takes one Laplacian smoothing step over the tile grid
(a lone noisy tile is pulled toward stable neighbours; a real
multi-tile change is not), and publishes a verdict only for
tiles fresh this frame — *a verdict is a claim about this
frame*. Perception spend is metered in embed-seconds and
printed next to what watching without the gate would have
cost; on the measured record the gate spent **11.8%** of the
embed-everything budget on staged material and **0.08%** on a
calm room, while catching every cut it was shown.

The embedder is a **seam**, not a dependency: the tool ships
with a test-double embedder (zero infrastructure, explicitly
not a perception model), a generic HTTP adapter for any
embedding service, and an example adapter for the JEV rack
(I-JEPA) it was first built against. Models with restrictive
licences are never bundled — see
[docs/BRING-YOUR-OWN-EMBEDDER.md](docs/BRING-YOUR-OWN-EMBEDDER.md).

## Quickstart

Prerequisites: Rust (1.85+), Python 3 with the ingest's deps,
and ffmpeg only if you index video files.

```sh
cargo build --release
python3 -m venv .venv && .venv/bin/pip install -r crates/telltape/requirements.txt
```

(Keep the venv active — or call `.venv/bin/python3` wherever
the configs below say `python3` — so the ingest finds its
deps. A venv, not a system pip install: modern distributions
refuse the latter, PEP 668.)

**See it work — the staged acceptance** (real-photo fixtures
bundled in the repo; a scene cut, an object appearing, then
gradual drift that must stay silent; the test-double embedder,
so this proves the pipeline and the ledger, not perception):

```sh
cat > acceptance.conf <<'EOF'
stream_id = acceptance
ledger_dir = ledger-acceptance
ingest_cmd = python3 crates/telltape/ingest/ingest.py --source staged --embedder double
embedder_name = double
embedder_version = 1
max_frames = 18
EOF
./target/release/telltape watch --config acceptance.conf
```

(The two `embedder_*` lines name the embedder this run's
tells belong to — every ledger records its embedder's
identity, and declaring it here is the habit the format doc
builds on. Leave them out and the run derives the same
identity from the config's own shape; declare a *different*
one later and the ledger will refuse it as the swap it is —
see docs/LEDGER-FORMAT.md. This whole run is also packaged
as `crates/telltape/acceptance.sh`, which prints the
expected-vs-measured table.)

Expected shape of the run (measured; re-verified by the proof
battery): 18 frames, **2 events, 34 embeds** — a verdict at
frame 4 naming all 16 tiles (the cut) and one at frame 7 naming
exactly tiles {5, 6} (the object), and silence for the drift.
Read the result:

```sh
cat ledger-acceptance/health.txt   # freshness, embedder, silence basis
grep '"kind":"verdict"' ledger-acceptance/events.jsonl
```

**Index your own footage** (a recording becomes a ledger whose
timestamps seek into the original file):

```sh
cat > index.conf <<'EOF'
stream_id = front-door
ledger_dir = ledger-front-door
ingest_script = crates/telltape/ingest/ingest.py
EOF
./target/release/telltape index path/to/recording.mp4 --config index.conf
```

With no `embed_url` in the config, indexing uses the test
double; add `embed_url = http://…/embed` (plus `embed_dim`,
`embed_field`, `embed_key` if your service differs) to index
with a real embedder.

**Watch a live source:** write an `ingest_cmd` for the
`shot`/`mjpeg` sources (see `ingest/ingest.py --help`) and run
`telltape watch`. Camera-specific recipes live with the origin
project's records; the source interface is the four sources
the ingest implements.

## The verbs

| Verb | Does | Status |
|---|---|---|
| `watch` | Live source → ledger | shipped |
| `index` | Existing footage → ledger, through the identical pipeline | shipped |
| `query` | Read a ledger: `telltape query <ledger-dir> [--since T] [--until T] [--tile N] [--stream ID] [--summary]` — the tells and service events in local time, counts by hour and tile, embed spend, frozen stretches; read-only | shipped (v0.2.0) |
| `export` | Ledger → one portable document: `telltape export <ledger-dir> [--format json\|csv] [--out FILE]` (same filters as `query`) — JSON carries every row verbatim, full fidelity, with the ledger's metadata and malformed-row count; CSV is the flat projection, one row per event in ledger order, epoch + ISO timestamps. This is how the tells travel into other tools — spreadsheets, jq pipelines, a video editor's marker import — without anyone parsing the ledger's text formats; read-only | shipped (v0.3.0) |

Times in `query` (and the ISO column of a CSV `export`)
render in your local zone: `$TZ` when set (e.g.
`TZ=America/Anchorage`), otherwise the system zone, otherwise
UTC. The examples in these docs were read in a set zone — on
a machine with neither, the same tell prints in UTC and can
carry a different calendar date than your local day. Every
row also prints its epoch, which no zone can move.

## Documents

- [docs/LEDGER-FORMAT.md](docs/LEDGER-FORMAT.md) — the four
  ledger files, field by field, as the code writes them.
- [docs/GRID.md](docs/GRID.md) — why 4×4 is the default (the
  measured record), and what changing the grid trades. The
  grid is configurable (`grid_rows`/`grid_cols`, `--grid`);
  every ledger records its own grid.
- [docs/EXAMPLES.md](docs/EXAMPLES.md) — five worked examples
  from real runs, each with the limit it exposed.
- [docs/BRING-YOUR-OWN-EMBEDDER.md](docs/BRING-YOUR-OWN-EMBEDDER.md)
  — the embedder seam contract and how to plug in I-JEPA or
  any other model under your own terms.
- [BUILD-LOG.md](BUILD-LOG.md) — how this extraction was
  built: decisions, dead ends, numbers, in the body of the work.

## Proving it

```sh
cargo test --workspace            # cascade, rings, ledger, parity, freshness, index
python3 crates/telltape/prove_fixtures.py   # gate layer, cold, no embedder
```

Layer honesty: the fixture script proves the diff *gate*; the
staged run above proves the *verdict* layer through a test
double; verdicts from a real embedder carry the measured record
in [docs/EXAMPLES.md](docs/EXAMPLES.md). Simulated numbers are
never quoted beside measured ones without their label.

## What this is not

- Not a recorder: no frames, no thumbnails, no embeddings are
  archived — by design (the reconstruction options were
  considered and ruled unnecessary; a change-witness is safe
  to keep forever and hand to any reader).
- Not a platform: no UI, voice, or playback ships here. Those
  exist as demos *on top of* the ledger in the origin project
  (Confluence) — proof the ledger is buildable-on, not part of
  the tool.
- Not finished being honest: the ledger's trust loop is open
  (corrections are not yet accounted into a precision number;
  `state.txt` says so, verbatim, in soak mode). Gaps are filed
  in the build record, not smoothed over.

## Licence

MIT OR Apache-2.0, at your option (LICENSE, LICENSE-MIT,
LICENSE-APACHE). Use it in all ways, build on it, make it
better — improvements are the point.
