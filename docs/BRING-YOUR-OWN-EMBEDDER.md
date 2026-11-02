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
  (default 1280). The ingest probes the adapter once at
  startup with a synthetic tile and fails loudly there — a
  width mismatch is a startup error, never a per-tile surprise
  mid-stream.
- **Latency:** measured client-side per call and carried in
  the record stream (`embed_ms`); the ledger's budget meter is
  built from it, so an honest service needs no instrumentation
  of its own.
- **Failures:** a non-200 or malformed response raises in the
  ingest and stops the run. The tool does not invent vectors
  to keep going — a watch that guesses is worse than a watch
  that stops.

A minimal service in Python's standard library (the shape,
not a recommendation):

```python
# embed_server.py — wrap YOUR model; this sketch only moves bytes.
import json
from http.server import BaseHTTPRequestHandler, HTTPServer

def embed(png_bytes):            # <-- your model lives here
    return [0.0] * 1280          # return its real vector

class H(BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers["Content-Length"]))
        # (parse the multipart field in real life — e.g. with the
        #  framework your model already uses)
        payload = json.dumps({"embedding": embed(body)}).encode()
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
Budget for a calibration pass on your own footage before
trusting verdicts: index a known stretch, read the raw scores
in the ledger's per-tile rows, and set the threshold from
what your scenes actually score. The gate's measured nature
applies to every embedder: it is a **ranker, not a
separator**, so calibration is recurring work, not a one-time
formality.
