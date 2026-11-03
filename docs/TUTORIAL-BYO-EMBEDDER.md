# Bring your own embedder, safely — a hands-on tutorial

You will plug your own embedder into telltape, watch it work, break it
three ways on purpose, calibrate it from its own record, and read the
answer out of the ledger. Everything below was run against the v0.3.2
tag on a fresh clone; every output block is a real run, trimmed for
length only. Plan half an hour. Run everything from the repo root.

Prerequisites: Rust 1.85+ (`cargo build --release`; if cargo is not on
your PATH it may live at `~/.cargo/bin/cargo`), Python 3, and `jq` for
the reading commands. ffmpeg is not needed — the fixtures are bundled.

## What this protects you from

A watch tool's most dangerous output is **"0 events, everything
quiet"** produced by a broken perception layer. Every other failure is
loud — a crash, a timeout, a stack trace. A quiet ledger wears the
costume of a calm night. telltape's history has two documented cases
where it nearly did exactly that, and the guards you will exercise in
this tutorial exist because of them.

The first is the **dead embedder**: your model crashes, or the endpoint
degrades to returning placeholder zeros or NaNs. Before v0.3.1 the
pipeline scored those vectors anyway — a zero vector sits at maximal
cosine distance from everything, so a dead embedder authored a ledger
full of confident, fake "changes" and health reported the embedder
`up`. The fix is one rule enforced everywhere an embedding enters:
a vector with a non-finite component, a non-finite norm, or a zero
norm carries no information and must never become a score. The startup
probe checks it, the pipeline re-checks every record mid-stream, and a
violation stops the run with a typed refusal, receipted in the ledger.

The second, worse, case is the **collapsed embedder**: the service
answers promptly with a valid, right-width, unit-norm vector — the
*same* vector for every input. A caching bug, a batch-size-one bug, a
normalization that swallows the input. Before v0.3.2 that passed every
check, completed the run with 0 events and exit 0, and health stamped
the silence `meaningful` over footage containing a full scene change —
every score was 0 because every comparison was a vector against
itself. v0.3.2 closed it at both ends: the startup probe embeds one
pure-black and one pure-white tile — the most different inputs a tile
can be — and refuses if their embeddings sit too close together; and a
run-time variance watch convicts an embedder whose inputs change
materially while its answers never move. The invariant, stated once:
**silence is only stamped meaningful when the embedder has
demonstrated discrimination on this run's actual inputs.**

You will now make all of this happen on purpose, watch the refusals,
and read the receipts.

## Step 0 — prove the tool works

Build, make the Python venv (PEP 668 systems refuse a system pip
install; the venv is the documented path), and run the staged
acceptance — real-photo fixtures bundled in the repo (a scene cut, an
object appearing, gradual drift that must stay silent), driven through
the built-in test-double embedder, so it proves the pipeline and the
ledger, not perception:

```sh
cargo build --release
python3 -m venv .venv && .venv/bin/pip install -r crates/telltape/requirements.txt
PYTHON=.venv/bin/python3 sh crates/telltape/acceptance.sh
```

What you should see (and what this machine printed):

```
ingest: embedder double ok (dim 1280)
canonical staged acceptance (test-double embedder)
check                    expected     measured    
frames                   18           18           ok
events                   2            2            ok
embeds                   34           34           ok
frame 4 changed tiles    16 (all)     16           ok
frame 7 changed tiles    [5, 6]       [5, 6]       ok
spend ratio              0.1181       0.1181       ok
ACCEPTANCE: PASS
```

18 frames, 2 events, 34 embeds: a verdict at frame 4 naming all 16
tiles (the cut), one at frame 7 naming exactly tiles {5, 6} (the
object), silence for the drift. If this fails, stop and fix your build
— nothing downstream will mean anything.

## Step 1 — the smallest honest embedder

The sketch server in
[BRING-YOUR-OWN-EMBEDDER.md](BRING-YOUR-OWN-EMBEDDER.md) only moves
bytes; its `embed()` is a deliberate placeholder. Replace it with
something real. The smallest honest choice: decode the tile, shrink it
to 8×8 grayscale, and use those 64 numbers (scaled to 0..1) as the
embedding, with one always-on bias component in front. No model, no
training — but the vector genuinely depends on the tile's pixels,
which is the whole contract. The bias matters more than it looks: a
luminance-only embedder maps a pure-black tile to all zeros, which is
degenerate and refused (the black-tile shape, documented in the
contract doc); one constant component keeps every real input away from
the zero vector.

Save this as `embed_server.py` in the repo root. It is the sketch's
shape — health endpoint and stdlib multipart parsing included, because
both are part of the contract — with the real `embed()` in place, plus
`--mode` switches that replay the broken shapes you will inflict on it
in Step 2:

```python
#!/usr/bin/env python3
# embed_server.py — the smallest honest embedder, built on the sketch
# in docs/BRING-YOUR-OWN-EMBEDDER.md. The sketch only moved bytes;
# this one has a real (if tiny) embed(): 8x8 grayscale + one bias
# component. Run it in the same venv as the ingest (needs numpy,
# Pillow). --mode replays the failure shapes the tutorial breaks:
#   honest    the real thing (default)
#   zeros     every call returns the zero vector        (dead embedder)
#   constant  one valid vector for every input         (collapsed)
#   smart     honest on the probe's 8x8 tiles, constant on real ones
#             (passes the probe, collapses mid-run)
#   fail      honest until embed #34, then HTTP 500 on every POST
#             (a mid-run transport failure)
import argparse, io, json
import numpy as np
from http.server import BaseHTTPRequestHandler, HTTPServer
from PIL import Image

DIM = 65  # 64 grayscale values + 1 bias component

MODE = "honest"
SERVED = 0  # completed embeds; the fail switch counts these


def embed(png_bytes):
    """Decode the tile, shrink to 8x8 grayscale, scale to 0..1, and
    prepend one always-on bias component. The bias is the standard
    remedy for the black-tile shape (BYO doc, non-degeneracy): a
    luminance-only embedder maps a pure-black tile to all zeros,
    which is degenerate and refused; one constant component keeps
    every real input away from the zero vector. No model, no
    training — but the vector genuinely depends on the tile's
    pixels, which is the whole contract."""
    img = Image.open(io.BytesIO(png_bytes)).convert("L")
    if MODE == "zeros":
        return [0.0] * DIM
    if MODE == "constant":
        return [1.0] + [0.0] * (DIM - 1)
    if MODE == "smart" and img.size != (8, 8):
        # The startup probe embeds 8x8 synthetic tiles; real staged
        # tiles are 128x128. Be honest on the probe, constant on
        # everything real — the shape that passes the probe and
        # collapses on the run.
        return [1.0] + [0.0] * (DIM - 1)
    g = np.asarray(img.resize((8, 8)), dtype=np.float64).flatten() / 255.0
    return [1.0] + [float(x) for x in g]


def multipart_field(body, content_type, field):
    """The bytes of one field of a multipart/form-data body,
    parsed with nothing but the standard library."""
    boundary = None
    for part in content_type.split(";"):
        part = part.strip()
        if part.startswith("boundary="):
            boundary = part[len("boundary="):].strip('"')
    if boundary is None:
        raise ValueError("no boundary in Content-Type")
    marker = b"--" + boundary.encode()
    for chunk in body.split(marker):
        if b"Content-Disposition" not in chunk:
            continue
        head, _, payload = chunk.partition(b"\r\n\r\n")
        if f'name="{field}"' not in head.decode("utf-8", "replace"):
            continue
        if payload.endswith(b"\r\n"):
            payload = payload[:-2]
        return payload
    raise ValueError(f"no field named {field!r}")


class H(BaseHTTPRequestHandler):
    def do_GET(self):
        # The health endpoint the contract requires: liveness
        # only — the body is never inspected.
        if self.path == "/health":
            payload = b"{}"
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        else:
            self.send_response(404)
            self.send_header("Content-Length", "0")
            self.end_headers()

    def do_POST(self):
        global SERVED
        if MODE == "fail" and SERVED >= 34:
            # 2 probe + 16 frame-0 + 16 frame-4 embeds succeeded; the
            # next embed call is frame 7's first — fail it, and keep
            # failing, like a model service dying mid-run.
            self.send_response(500)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        body = self.rfile.read(int(self.headers["Content-Length"]))
        png = multipart_field(body, self.headers.get("Content-Type", ""), "file")
        vec = embed(png)
        SERVED += 1
        payload = json.dumps({"embedding": vec}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *a):
        pass  # keep the console quiet; the ledger is the record


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", default="honest",
                    choices=["honest", "zeros", "constant", "smart", "fail"])
    ap.add_argument("--port", type=int, default=9100)
    a = ap.parse_args()
    MODE = a.mode
    print(f"embed_server: mode={MODE} dim={DIM} on http://127.0.0.1:{a.port}",
          flush=True)
    HTTPServer(("127.0.0.1", a.port), H).serve_forever()
```

Start it and check the health endpoint — the service requires
`GET {embed origin}/health` to answer 200 *before any run starts*;
without that handler every run below would die at startup with
`EmbedderUnhealthy`:

```sh
(.venv/bin/python3 embed_server.py --mode honest > server.log 2>&1 & echo $! > server.pid)
sleep 0.3; cat server.log
curl -s http://127.0.0.1:9100/health; echo
```

```
embed_server: mode=honest dim=65 on http://127.0.0.1:9100
{}
```

The config. Note the two places the embedder is named: the ingest
command carries the adapter flags (`--embedder http`, the URL, and the
width — 65 here, not the I-JEPA default 1280), and
`embedder_name`/`embedder_version` declare the identity stamped into
the ledger. No `threshold` line — the default 0.25 applies, and in
Step 3 you will find out what that costs you:

```sh
cat > luma.conf <<'EOF'
stream_id = staged-luma
ledger_dir = ledger-luma
ingest_cmd = .venv/bin/python3 crates/telltape/ingest/ingest.py --source staged --embedder http --embed-url http://127.0.0.1:9100/embed --embed-dim 65
embed_url = http://127.0.0.1:9100/embed
embedder_name = luma
embedder_version = 1
max_frames = 18
EOF
rm -rf ledger-luma
./target/release/telltape watch --config luma.conf 2>&1
```

```
ingest: embedder http ok (dim 65)
{"frames":18,"events":1,"embeds":34,"embeds_spent_s":0.120,"embed_everything_estimate_s":1.014,"verdict_latency_p50_ms":278.189,"verdict_latency_max_ms":278.189,"verdict_latency_basis":"capture_to_ledger_write","embedder_started_by_me":false,"embedder_stopped_by_me":false}
```

A clean run through a server you wrote: 18 frames, 34 embeds, exit 0.
Read the basis of that run in the ledger — these are the lines that
carry the weight:

```sh
grep -E '^embedder|^freshness|^silence_meaningful|^max_smoothed|^near_miss|^embed_ms_p50' ledger-luma/health.txt
grep -E '^frames_seen|^events_published|^tiles_embedded|^coverage|^spend_ratio' ledger-luma/state.txt
jq -c 'select(.kind=="verdict") | {frame, changed_tiles}' ledger-luma/events.jsonl
```

```
embed_ms_p50 = 3.25
embedder = up
embedder_identity = luma@1
max_smoothed_score = 0.3171
near_miss_frames = 13
freshness = live
silence_meaningful = true
frames_seen = 18
events_published = 1
tiles_embedded = 34
coverage = 1.0000
spend_ratio = 0.1181
{"frame":4,"changed_tiles":[10,13,14]}
```

The mechanics are healthy: the embedder is `up` at a measured 3.25 ms
p50, every frame was novel, the run spent 0.1181 of the
embed-everything budget (same economics as the acceptance run — the
pixel gate, not the embedder, decides what gets embedded). But look at
the verdict: **one** event, and the cut published only tiles
{10, 13, 14} — not all 16, and the object at frame 7 is missing
entirely. The run's silence was stamped meaningful, and that stamp is
honest in its own terms (your embedder discriminates, frames arrived,
the source is live) — but it says nothing about whether 0.25 is the
right *threshold* for your embedder's distance geometry. That number
was calibrated on I-JEPA. Yours is a different space. Step 3 fixes
this from the record; first, break things.

## Step 2 — break it three ways

Each break below restarts the server in a broken mode and runs the
same staged footage into a fresh ledger. The config is identical each
time except `ledger_dir`. Watch what refuses, and what it receipts.

### a. Return all zeros — the dead embedder

```sh
kill $(cat server.pid); sleep 0.2
(.venv/bin/python3 embed_server.py --mode zeros > server.log 2>&1 & echo $! > server.pid)
cat > luma-zeros.conf <<'EOF'
stream_id = staged-luma
ledger_dir = ledger-luma-zeros
ingest_cmd = .venv/bin/python3 crates/telltape/ingest/ingest.py --source staged --embedder http --embed-url http://127.0.0.1:9100/embed --embed-dim 65
embed_url = http://127.0.0.1:9100/embed
embedder_name = luma
embedder_version = 1
max_frames = 18
EOF
rm -rf ledger-luma-zeros
./target/release/telltape watch --config luma-zeros.conf 2>&1; echo "exit=$?"
cat ledger-luma-zeros/events.jsonl
grep -E '^embedder =|^silence_meaningful|^freshness' ledger-luma-zeros/health.txt
grep -E '^run_outcome|^frames_seen' ledger-luma-zeros/state.txt
```

```
embed_server: mode=zeros dim=65 on http://127.0.0.1:9100
ingest: embedder probe failed: embedder http: probe embedding for a black tile is degenerate (zero norm) — a vector with no information cannot be scored; refusing to start
telltape: ingest: ingest command exited with exit status: 2 after 0 frames
exit=1
{"kind":"service","event":"start","msg":"stream staged-luma starting","wall_us":1793726319492655}
{"kind":"service","event":"run_aborted","msg":"ingest exited with exit status: 2 after 0 frames; run aborted — the tells before this point stand, and nothing after it was seen","wall_us":1793726319944109}
embedder = down
freshness = unknown
silence_meaningful = false
frames_seen = 0
run_outcome = aborted
```

The probe — two synthetic tiles, before a frame exists — refused the
embedder by name, the run aborted before seeing anything, and health
stamps the embedder `down` with `silence_meaningful = false`. Before
v0.3.1 this exact server produced the canonical acceptance ledger,
digit for digit, out of pure pixel-diff nominations.

**Conclude:** your model pipeline returned nothing usable (model not
loaded, wrong endpoint, a placeholder left in). Fix the server; do not
rerun and hope. The refusal cost you two embed calls.

### b. Return one constant valid vector — the collapsed embedder

```sh
kill $(cat server.pid); sleep 0.2
(.venv/bin/python3 embed_server.py --mode constant > server.log 2>&1 & echo $! > server.pid)
cat > luma-constant.conf <<'EOF'
stream_id = staged-luma
ledger_dir = ledger-luma-constant
ingest_cmd = .venv/bin/python3 crates/telltape/ingest/ingest.py --source staged --embedder http --embed-url http://127.0.0.1:9100/embed --embed-dim 65
embed_url = http://127.0.0.1:9100/embed
embedder_name = luma
embedder_version = 1
max_frames = 18
EOF
rm -rf ledger-luma-constant
./target/release/telltape watch --config luma-constant.conf 2>&1; echo "exit=$?"
cat ledger-luma-constant/events.jsonl
grep -E '^embedder =|^silence_meaningful' ledger-luma-constant/health.txt
```

```
embed_server: mode=constant dim=65 on http://127.0.0.1:9100
ingest: embedder probe failed: embedder http: probe collapse: a black tile and a white tile embed at cosine distance 0.000000 of each other (must exceed 0.01) — the embedder returns (near-)identical vectors for demonstrably different inputs, so nothing it reports can distinguish change from stillness; refusing to start
telltape: ingest: ingest command exited with exit status: 2 after 0 frames
exit=1
{"kind":"service","event":"start","msg":"stream staged-luma starting","wall_us":1793726324205756}
{"kind":"service","event":"run_aborted","msg":"ingest exited with exit status: 2 after 0 frames; run aborted — the tells before this point stand, and nothing after it was seen","wall_us":1793726324693070}
embedder = down
silence_meaningful = false
```

The vector is valid — finite, right width, unit norm — and the probe
still refuses it, naming the collapse and printing the measured
distance (0.000000). Validity is not information.

**Conclude:** a caching bug, a batch bug, a normalization that swallows
the input. The response being well-formed JSON at the right width is
exactly why this failure is dangerous; the probe is what catches it.
Fix the model, not the config.

Now the harder version — an embedder that *passes* the probe and
collapses only on real tiles. `--mode smart` is honest on the probe's
8×8 synthetic tiles and constant on everything else. The startup probe
cannot catch this by construction; the run-time variance watch has to:

```sh
kill $(cat server.pid); sleep 0.2
(.venv/bin/python3 embed_server.py --mode smart > server.log 2>&1 & echo $! > server.pid)
cat > luma-smart.conf <<'EOF'
stream_id = staged-luma
ledger_dir = ledger-luma-smart
ingest_cmd = .venv/bin/python3 crates/telltape/ingest/ingest.py --source staged --embedder http --embed-url http://127.0.0.1:9100/embed --embed-dim 65
embed_url = http://127.0.0.1:9100/embed
embedder_name = luma
embedder_version = 1
max_frames = 18
EOF
rm -rf ledger-luma-smart
./target/release/telltape watch --config luma-smart.conf 2>&1; echo "exit=$?"
cat ledger-luma-smart/events.jsonl
grep -E '^embedder =|^silence_meaningful' ledger-luma-smart/health.txt
grep -E '^run_outcome|^frames_seen|^events_published' ledger-luma-smart/state.txt
```

```
embed_server: mode=smart dim=65 on http://127.0.0.1:9100
ingest: embedder http ok (dim 65)
telltape: embedder collapsed: 16 tile observations whose pixels changed materially (diff >= 10) produced embeddings that did not move (score <= 0.005), and no varied input ever moved an embedding — the embedder is answering, but its answers do not discriminate this run's inputs, so its silence would mean nothing. Run stopped and receipted as collapsed_embedder; check the embedder service (a constant or near-constant response is the usual cause)
exit=1
{"kind":"service","event":"start","msg":"stream staged-luma starting","wall_us":1793726329936645}
{"kind":"service","event":"collapsed_embedder","msg":"embedder collapsed: 16 tile observations with materially changed pixels (diff >= 10) produced embeddings that did not move (score <= 0.005), and no varied input ever moved one; run stopped — an embedder that cannot discriminate this run's inputs must not stamp its silence meaningful","wall_us":1793726331388198}
embedder = collapsed
silence_meaningful = false
frames_seen = 4
events_published = 0
run_outcome = aborted
```

Read the mechanics: the probe passed (`embedder http ok`), frames 0–3
ran quietly (a static scene — nothing varied, nothing to convict),
then the cut at frame 4 nominated all 16 tiles, their pixels changed
materially, their embeddings did not move — 16 varied-but-unmoved
observations, and the watch convicted. The run stopped at frame 4
(the tripping frame published nothing; the frame that proves the
embedder broken does not get to author state), the ledger carries the
`collapsed_embedder` receipt with the counts, health stamps the
embedder `collapsed` — neither `up` nor `down`; it answers, but its
answers do not discriminate — and the silence is not meaningful.

**Conclude:** an embedder can ace synthetic probes and still be blind
on your material; only the run's own inputs are the real test, and the
watch runs it for you continuously. The receipted counts are the
evidence to take to whoever owns the embedder service.

### c. Kill the server mid-run — HTTP 500s

`--mode fail` serves honestly for 34 embeds (the probe's 2, frame 0's
16, frame 4's 16 — so the cut's verdict is written), then answers 500
to everything, like a model service dying under load:

```sh
kill $(cat server.pid); sleep 0.2
(.venv/bin/python3 embed_server.py --mode fail > server.log 2>&1 & echo $! > server.pid)
cat > luma-fail.conf <<'EOF'
stream_id = staged-luma
ledger_dir = ledger-luma-fail
ingest_cmd = .venv/bin/python3 crates/telltape/ingest/ingest.py --source staged --embedder http --embed-url http://127.0.0.1:9100/embed --embed-dim 65
embed_url = http://127.0.0.1:9100/embed
embedder_name = luma
embedder_version = 1
max_frames = 18
EOF
rm -rf ledger-luma-fail
./target/release/telltape watch --config luma-fail.conf 2>&1; echo "exit=$?"
jq -c 'if .kind == "verdict" then {kind, frame, changed_tiles} else . end' ledger-luma-fail/events.jsonl
grep -E '^embedder =|^silence_meaningful' ledger-luma-fail/health.txt
grep -E '^run_outcome|^frames_seen|^events_published' ledger-luma-fail/state.txt
```

```
embed_server: mode=fail dim=65 on http://127.0.0.1:9100
ingest: embedder http ok (dim 65)
ingest: embedder call failed (transport): HTTPError: 500 Server Error: Internal Server Error for url: http://127.0.0.1:9100/embed
telltape: ingest: ingest command exited with exit status: 1 after 7 frames (embedder call failed (transport): HTTPError: 500 Server Error: Internal Server Error for url: http://127.0.0.1:9100/embed)
exit=1
{"kind":"service","event":"start","msg":"stream staged-luma starting","wall_us":1793726337348268}
{"kind":"verdict","frame":4,"changed_tiles":[10,13,14]}
{"kind":"service","event":"embedder_error","msg":"embedder call failed (transport): HTTPError: 500 Server Error: Internal Server Error for url: http://127.0.0.1:9100/embed; run aborted — the tells before this point stand, and nothing after it was seen","wall_us":1793726339088294}
embedder = up
silence_meaningful = false
frames_seen = 7
events_published = 1
run_outcome = aborted
```

No stack trace: one classified line (`transport` — the call failed;
a malformed answer would read `response`), a typed abort after 7
frames, and a receipt in the ledger. The verdict written before the
failure stands; the run is marked `run_outcome = aborted`; health
keeps `embedder = up` (the health endpoint still answers — this is a
run failure, not a liveness one) but withdraws `silence_meaningful`.
The tool does not invent vectors to keep going — a watch that guesses
is worse than a watch that stops.

**Conclude:** restart the embedder service, then re-run the stretch
into a fresh ledger; the aborted ledger stands as the incident record,
receipt and all.

## Step 3 — calibrate from the record

Back on the honest server. Your Step 1 ledger (`ledger-luma`) ran
deaf at the default threshold: the cut published 3 tiles of 16 and the
object published nothing. The two numbers the ledger carries for
exactly this moment, in both `health.txt` and `state.txt`:

- **`max_smoothed_score = 0.3171`** — the strongest smoothed score any
  tile reached this run, published or not. A change *did* score
  0.3171; three tiles crossed 0.25 and published.
- **`near_miss_frames = 13`** — frames that published nothing whose
  best tile score reached at least 80% of the threshold. The band
  under 0.25 is populated.

Two caveats as you read them, both visible in this record. First,
`near_miss_frames` here counts the *afterglow* of the cut: a tile's
score EMA keeps its last value until the tile is embedded again, so
tile 14's 0.3171 keeps every quiet frame after frame 4 inside the
band. The counters tell you scores crowd the band; the per-tile rows
tell you which scores. Second, the counters are thresholds-relative —
re-read them after every threshold change.

So go to the rows. The cut's verdict row carries all 16 tiles'
smoothed scores, and the lens carries each tile's final score EMA:

```sh
jq -c 'select(.kind=="verdict") | {frame, weakest_smoothed: ([.tiles[].smoothed] | min), strongest_smoothed: ([.tiles[].smoothed] | max)}' ledger-luma/events.jsonl
grep -E '^threshold|^tile_score_ema' ledger-luma/lens.txt
```

```
{"frame":4,"weakest_smoothed":0.0257,"strongest_smoothed":0.3171}
threshold = 0.2500
tile_score_ema = 0.0639 0.0651 0.0656 0.0257 0.0505 0.0800 0.0891 0.0432 0.1371 0.2388 0.2463 0.0900 0.1916 0.2657 0.3171 0.1139
```

The whole cut spans 0.0257–0.3171 — the threshold must sit at or
below the *weakest true change* (0.0257) for the cut to publish
whole. The object's tiles (5 and 6, the sixth and seventh EMA values)
finished at 0.0800 and 0.0891 — above 0.0257, so one threshold can
catch both staged changes. Nothing else on this material can publish:
quiet frames are never nominated by the gate, and unpublished frames
carry no fresh tiles. Set `threshold = 0.02` — at or below the weakest
true signal, with margin:

```sh
kill $(cat server.pid); sleep 0.2
(.venv/bin/python3 embed_server.py --mode honest > server.log 2>&1 & echo $! > server.pid)
cat > luma-cal.conf <<'EOF'
stream_id = staged-luma
ledger_dir = ledger-luma-002
ingest_cmd = .venv/bin/python3 crates/telltape/ingest/ingest.py --source staged --embedder http --embed-url http://127.0.0.1:9100/embed --embed-dim 65
embed_url = http://127.0.0.1:9100/embed
embedder_name = luma
embedder_version = 1
threshold = 0.02
max_frames = 18
EOF
rm -rf ledger-luma-002
./target/release/telltape watch --config luma-cal.conf 2>&1
jq -c 'select(.kind=="verdict") | {frame, changed_tiles}' ledger-luma-002/events.jsonl
grep -E '^max_smoothed|^near_miss|^events_published|^silence_meaningful' ledger-luma-002/state.txt ledger-luma-002/health.txt
```

```
ingest: embedder http ok (dim 65)
{"frames":18,"events":2,"embeds":34,"embeds_spent_s":0.132,"embed_everything_estimate_s":1.116,"verdict_latency_p50_ms":32.854,"verdict_latency_max_ms":313.656,"verdict_latency_basis":"capture_to_ledger_write","embedder_started_by_me":false,"embedder_stopped_by_me":false}
{"frame":4,"changed_tiles":[0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15]}
{"frame":7,"changed_tiles":[5,6]}
ledger-luma-002/state.txt:events_published = 2
ledger-luma-002/state.txt:max_smoothed_score = 0.3171
ledger-luma-002/state.txt:near_miss_frames = 12
ledger-luma-002/health.txt:max_smoothed_score = 0.3171
ledger-luma-002/health.txt:near_miss_frames = 12
ledger-luma-002/health.txt:silence_meaningful = true
```

Two events, matching the staged ground truth exactly: frame 4 all 16
tiles (the cut), frame 7 exactly {5, 6} (the object) — and the drift
frames 10–17 stayed silent at the same threshold, because the gate
never nominated them (their per-frame diff is ~1.2 against a 3.0
gate). Same 18 frames, same 34 embeds, same 0.1181 spend — only the
threshold changed, and with it the hearing.

Two honest limits of this landing point. On this staged material a low
threshold cannot publish noise, because quiet frames are gated out
before the embedder ever sees them; on live footage the noise floor is
not zero, and lowering a threshold while `near_miss_frames` grows on a
*known-quiet* stretch will publish that noise — the calibration loop
in the contract doc exists for exactly that case. And the gate is a
ranker, not a separator: recalibrate when the embedder, the scene, or
the lighting changes. Two passes settled it here; that is typical,
not guaranteed.

## Step 4 — the daily answer

The calibrated ledger now answers "what changed, where, when" in one
command:

```sh
./target/release/telltape query ledger-luma-002 --summary
```

```
ledger   ledger-luma-002 (read-only)
stream   staged-luma
grid     4x4 (16 tiles)
embedder luma@1
rows     2 verdicts, 2 service, 0 malformed (file-wide)
filters  none
times    verdicts on frame capture time (t_capture_us); service rows on write time (wall_us)

SUMMARY
tells in filter   2 verdicts, 2 service rows
embeds in filter  18 calls (filtered verdicts)

by hour (local)
  2026-11-03 17:00 UTC  2

by tile
  tile 5 (r1c1)  2
  tile 6 (r1c2)  2
  tile 0 (r0c0)  1
  tile 1 (r0c1)  1
  tile 2 (r0c2)  1
  tile 3 (r0c3)  1
  tile 4 (r1c0)  1
  tile 7 (r1c3)  1
  tile 8 (r2c0)  1
  tile 9 (r2c1)  1
  tile 10 (r2c2)  1
  tile 11 (r2c3)  1
  tile 12 (r3c0)  1
  tile 13 (r3c1)  1
  tile 14 (r3c2)  1
  tile 15 (r3c3)  1

frozen stretches (source_frozen -> source_recovered)
  (none under these filters)

service-stopped stretches (stop -> start)
  (none under these filters)

embedder swaps
  (none under these filters)

service events
  2026-11-03 17:19:04 UTC  epoch_us 1793726344941675  start  stream staged-luma starting
  2026-11-03 17:19:07 UTC  epoch_us 1793726347623710  stop  frames=18 events=2 embeds=34

ledger totals (state.txt, unfiltered)
  frames_seen = 18
  events_published = 2
  tiles_embedded = 34
  embeds_spent_s = 0.132
  spend_ratio = 0.1181
  freshness = live
```

The tells themselves, without `--summary`, print one line each in
local time (this machine renders UTC; `$TZ` or the system zone sets
it), with frame numbers and tile positions under the lens grid — the
two seek keys into the footage:

```
TELLS
2026-11-03 17:19:06 UTC  epoch_us 1793726346098237  frame 4  stream staged-luma  tiles [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15] (r0c0, r0c1, r0c2, r0c3, r1c0, r1c1, r1c2, r1c3, r2c0, r2c1, r2c2, r2c3, r3c0, r3c1, r3c2, r3c3)  embeds 16
2026-11-03 17:19:06 UTC  epoch_us 1793726346612043  frame 7  stream staged-luma  tiles [5, 6] (r1c1, r1c2)  embeds 2
```

To carry the answer into other tools, export it — CSV is the flat
projection (one row per event, epoch and ISO timestamps, tile lists
semicolon-joined); JSON is full fidelity, every row verbatim under the
ledger's metadata:

```sh
./target/release/telltape export ledger-luma-002 --format csv --out staged-luma.csv
cat staged-luma.csv
./target/release/telltape export ledger-luma-002 --format json --out staged-luma.json
```

```
telltape export: note: verdict_latency_ms basis: capture_to_ledger_write (what the number measures depends on the source kind — see docs/LEDGER-FORMAT.md)
telltape export: wrote 2 tells + 2 service rows (csv) to staged-luma.csv
kind,stream,frame,wall_us,wall_iso,t_capture_us,t_capture_iso,event,msg,changed_tiles,focus,lineage_mask,verdict_latency_ms,embeds_this_frame,energy_before,energy_after
service,staged-luma,,1793726344941675,2026-11-03T17:19:04.941675Z,,,start,stream staged-luma starting,,,,,,,
verdict,staged-luma,4,1793726346411893,2026-11-03T17:19:06.411893Z,1793726346098237,2026-11-03T17:19:06.098237Z,,,0;1;2;3;4;5;6;7;8;9;10;11;12;13;14;15,14;10;9;13,31,313.656,16,0.2662,0.1566
verdict,staged-luma,7,1793726346644897,2026-11-03T17:19:06.644897Z,1793726346612043,2026-11-03T17:19:06.612043Z,,,5;6,14;10;9;13,31,32.854,2,0.2898,0.1630
service,staged-luma,,1793726347623710,2026-11-03T17:19:07.62371Z,,,stop,frames=18 events=2 embeds=34,,,,,,,
telltape export: wrote 2 tells + 2 service rows (json) to staged-luma.json
```

That is the whole loop: a ledger your own embedder produced, guarded
against its own failure modes, calibrated on its own record, and read
back as an answer.

## Rules of thumb

- **Probe first.** Two synthetic tiles at startup catch the dead and
  the collapsed embedder before a frame exists. If your server cannot
  pass `black ≠ white`, nothing downstream matters. (And serve
  `GET /health` — the run checks it before it starts.)
- **A refusal is the tool working.** `exit=1` with a typed line and a
  receipted ledger is the good outcome; the bad outcome is a quiet,
  confident ledger authored by a broken embedder. Do not treat
  refusals as obstacles to route around.
- **Never trust a quiet ledger whose embedder hasn't demonstrated
  discrimination on this run's inputs.** That is what `embedder =
  up/collapsed`, the variance watch, and `silence_meaningful` encode —
  read all three before believing "nothing happened."
- **Calibrate from the record, not by feel.** `max_smoothed_score`,
  `near_miss_frames`, the verdict rows' per-tile scores, and the
  lens's `tile_score_ema` are the whole loop; the default threshold
  belongs to I-JEPA's geometry, not yours. Use a fresh ledger for
  every calibration pass.
- **Declare your embedder's identity** (`embedder_name` /
  `embedder_version`), and bump the version when the model behind the
  endpoint changes — two embedders' spaces are related by at best a
  rotation, and a ledger must be able to say which one produced its
  tells.
- **The gate is a ranker, not a separator.** Recalibrate when the
  embedder, the scene, or the lighting changes; the two counters exist
  so that recurring calibration is a read, not a guess.
