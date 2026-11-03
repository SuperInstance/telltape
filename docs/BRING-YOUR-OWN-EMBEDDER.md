# Bring your own embedder

The cascade never sees an image. It sees **embeddings** —
vectors that say what a tile contains, in a space where
distance means change. Which model produces those vectors is
the operator's choice, behind one seam:

```
embed(tile image) -> (vector, embed_ms)
```

Nothing else about a model leaks into the tool. That is a
licence posture as much as an architecture: **weights with
restrictive terms are never bundled with, downloaded by, or
incorporated into this repo.** The seam ships; the docs
explain how to move the best part in; the operator brings the
model under their own terms.

## What ships

| Adapter | Ingest flag | What it is |
|---|---|---|
| Test double | `--embedder double` | A deterministic byte-hash pseudo-embedding (SHA-256 expansion, unit-normalised). **Not a perception model.** It runs the whole tool end-to-end with zero infrastructure — pipeline, gate economics, ledger, freshness — and proves nothing about perception quality. Its distances saturate (~1 between any two different tiles), which is why the acceptance numbers through it are labelled test-double wherever quoted. |
| Generic HTTP | `--embedder http --embed-url U [--embed-field file] [--embed-key embedding]` | Any HTTP service speaking the contract below. |
| JEV rack | `--embedder jev --rack U` | The origin fleet's I-JEPA rack — which happens to speak exactly the generic shape. It is an adapter with a name, not the definition of the seam. |

An in-process adapter (e.g. a local ONNX runtime) was designed
in the extraction plan and is **not shipped**; the HTTP shape
is the supported route for real models today. Wrapping a local
model in a tiny HTTP service is a page of code and keeps the
model's runtime, and its licence, entirely on your side of the
seam.

## The HTTP contract

- **Request:** `POST <embed-url>`, multipart form, one field
  (default `file`) carrying the tile as PNG bytes. Tiles are
  square crops of the working frame (128×128 px at 512×512 on
  the default 4×4 grid).
- **Response:** JSON; the vector is the array of numbers under
  the configured key (default `embedding`).
- **Width:** the vector's length must equal `--embed-dim`
  (default 1280). The ingest probes the adapter at startup
  with synthetic tiles and fails loudly there — a width
  mismatch is a startup error, never a per-tile surprise
  mid-stream.
- **Health endpoint:** when `embed_url` is set, the service
  requires `GET {origin}/health` — the embed URL's scheme and
  authority, path discarded — to answer 200 **before the run
  starts** (and, with `embedder_autostart`, to come up after
  the start command). The body is not inspected; liveness is
  all it attests. The config key `embed_health_url`
  ([CONFIG.md](CONFIG.md)) overrides the derived URL when
  your service puts health somewhere else. With no
  `embed_url` (the test double, or a hand-driven ingest) no
  check is possible and none is made. A service that only
  implements the POST below will be reported down at startup
  — this is the requirement a second cold playtest had to
  discover by reading source, which is why it is stated here,
  in the contract, in v0.3.2.
- **Non-degeneracy:** the vector must carry information —
  every component finite, and a finite, non-zero norm. The
  startup probe checks this too, and the pipeline re-checks
  every embedding it is handed, mid-stream included: a
  degenerate vector (a dead model returning zeros, an
  endpoint returning placeholder NaNs) stops the run with a
  typed refusal, receipted in the ledger as a
  `degenerate_embedder` service event. This rule exists
  because the failure it prevents is silent and confident:
  a zero vector is at maximal cosine distance from
  *everything*, so an unchecked seam turns a broken embedder
  into a ledger of maximal "changes" that are really just
  pixel-diff nominations. (A cold playtest reproduced exactly
  that with the sketch below; v0.3.1 closed it.)
  - *One shape to know about:* an embedder built from sparse
    or luminance-only features can return the **zero vector
    for a genuinely black tile** — not because it is broken,
    but because an all-zero input has no features to fire.
    That vector is degenerate by the rule above and will be
    refused. The standard remedy is a constant bias component
    (one always-on feature, e.g. a fixed first component), so
    no real input maps to the zero vector.
- **Discrimination:** non-degenerate is not enough — the
  vector must *depend on the input*. The probe embeds two
  synthetic tiles, one pure black and one pure white (the
  most different inputs a tile can be), and requires their
  embeddings to sit at cosine distance **> 0.01**; an
  embedder answering both with the same valid vector — or
  with vectors a jitter apart — is refused at startup with
  the collapse named. The same requirement follows the run:
  a variance watch at the seam tracks the run's actual
  inputs, and if tiles whose pixels change materially keep
  producing embeddings that do not move — and no varied
  input ever moves one — the run stops with a typed error,
  receipted as a `collapsed_embedder` service event, and
  health stamps the embedder `collapsed` rather than `up`.
  The invariant, stated once: **silence is only stamped
  meaningful when the embedder has demonstrated
  discrimination on this run's actual inputs.** A constant
  embedder's quiet over a changing scene is not evidence of
  a quiet scene, and since v0.3.2 the tool will not let it
  be filed as any. (A uniformly blank scene never trips the
  watch: the trigger is input variation *without* embedding
  variation, never stillness itself.)
- **Latency:** measured client-side per call and carried in
  the record stream (`embed_ms`); the ledger's budget meter is
  built from it, so an honest service needs no instrumentation
  of its own.
- **Failures:** a non-200 or malformed response stops the
  run, classified: *transport* (the call itself failed — a
  500, a dropped connection) or *response* (the answer could
  not be read — bad JSON, the key missing, the width wrong).
  The ingest reports the class to the service before exiting,
  the service receipts it in the ledger as an `embedder_error`
  service event and closes the run as aborted (health withdraws
  the silence stamp), and the user sees one clean classified
  line — not a stack trace. The tool does not invent vectors
  to keep going — a watch that guesses is worse than a watch
  that stops.

A minimal service in Python's standard library (the shape,
not a recommendation) — health endpoint and real multipart
parsing included, because both are part of the contract:

```python
# embed_server.py — wrap YOUR model; this sketch only moves bytes.
import json
from http.server import BaseHTTPRequestHandler, HTTPServer

def embed(png_bytes):            # <-- your model lives here
    return [0.0] * 1280          # return its real vector —
                                 # as written, this placeholder
                                 # is degenerate and the startup
                                 # probe will refuse it (by design:
                                 # see Non-degeneracy above). The
                                 # sketch is the shape, not a
                                 # runnable embedder.

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
        body = self.rfile.read(int(self.headers["Content-Length"]))
        png = multipart_field(body, self.headers.get("Content-Type", ""), "file")
        payload = json.dumps({"embedding": embed(png)}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

HTTPServer(("127.0.0.1", 9000), H).serve_forever()
```

## I-JEPA, specifically

The measured record in this repo (wave 3, the staged
acceptance, the soak) was produced with Meta's I-JEPA
(ViT-H, 1280-dim) served over HTTP. I-JEPA's weights are
distributed by Meta under Meta's own terms; this repo takes
no position on them and ships none of them. If your use fits
those terms: serve the model behind the HTTP contract above
(the JEV rack is one such service, and the `jev` adapter its
URL layout) and set `embed_dim = 1280`. The tool cannot tell
the difference — that is the point of the seam.

## Calibration is part of bringing your own

The numbers in the record — decision threshold 0.25,
gate 3.0, α 0.1 — were calibrated against I-JEPA embeddings of
these scenes. A different embedder is a different distance
geometry: the same threshold can be hair-trigger or deaf.
(A second cold playtest's honest luminance embedder scored a
*full scene change* at 0.02–0.16; at the default 0.25 it
would have reported nothing at all, meaningfully stamped.)

Calibration reads from the ledger, and since v0.3.2 the
ledger carries the two numbers the loop needs — in both
`state.txt` and `health.txt`, updated every frame:

- **`max_smoothed_score`** — the strongest smoothed score any
  tile reached this run, published or not.
- **`near_miss_frames`** — frames that published nothing but
  whose best tile score reached at least 80% of the current
  threshold.

The loop:

1. Run a stretch of footage you know — including at least
   one change you care about and one quiet stretch.
2. Read the two numbers. If the known change did not
   publish, `max_smoothed_score` is what it actually scored:
   your threshold must sit at or below it. If
   `near_miss_frames` is large on the quiet stretch, scores
   crowd the band just under your threshold: lower it only
   with care, or quiet hours will start publishing noise.
3. Set `threshold` in the config (see
   [CONFIG.md](CONFIG.md)), rerun the same stretch, and
   compare the published tells against what you know
   happened. Two or three passes usually separate cleanly;
   a geometry where the quiet band and the change scores
   overlap has no good threshold, and learning that from the
   numbers *is* the calibration succeeding.

Published verdicts' per-tile rows remain in the ledger for
the scores above threshold. What the two counters add is
everything below it — before them, an unpublished frame left
no row at all, so a near-miss was unobservable and the loop
above could not be run from the record. The gate's measured
nature applies to every embedder: it is a **ranker, not a
separator**, so calibration is recurring work, not a one-time
formality.
