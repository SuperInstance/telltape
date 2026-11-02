#!/usr/bin/env python3
"""telltape ingest: frames -> tiles -> diff gate -> embeddings.

Owns everything the Rust service should not: frame acquisition, image
decode, tiling, the cheap frame-diff gate, and the embedder calls.
Emits, on stdout, the record schema (one JSON object per embedded
tile) plus a frame_end control line per frame:

  {"seq":0,"frame":F,"tile":T,"t_arrival_us":<capture epoch us>,
   "diff_score":D,"embed_ms":M,"embedding":[<embed_dim> floats]}
  {"control":"frame_end","frame":F,"t_capture_us":<capture epoch us>,
   "nominated":K,"gate_diff":G,"frame_hash":H,"frame_repeat":false}

Sources (identical interface — the service cannot tell them apart):
  staged   staged-events sequence (cut / object / drift) composited
           from the bundled fixture photos, ground truth written to
           --truth-out. The acceptance rig.
  wave3    the wave-3 frame sequences (A burst, B turntable,
           C interleaved cuts). Frames resolve from the bundled
           fixtures by default; see fixtures_dir() for the override
           order (explicit --fixtures-dir / env, then bundled).
  shot     IP Webcam style: poll GET <url> for one JPEG per frame.
  mjpeg    IP Webcam style: MJPEG stream at <url> (/video), frames
           taken at the lens cadence, drop-to-newest.
  file     Existing footage (the `telltape index` verb): a video
           file decoded with ffmpeg at --fps, an image sequence
           (a directory or glob of stills), or a single image.
           Frames are replayed through the identical Emitter path
           as live sources — same tiling, same gate, same records —
           so an index ledger means exactly what a watch ledger
           means. Capture times: frame k sits at start + k/fps;
           the start epoch is --start-us when given, else the
           file's mtime minus its probed duration (an export's
           mtime is when the recording ended — an approximation,
           documented as such), else the mtime itself.

Embedders (one seam, three adapters — see
docs/BRING-YOUR-OWN-EMBEDDER.md):
  double   HashEmbedder: a deterministic byte-hash pseudo-embedding.
           NOT a perception model — it exists so the whole tool runs
           end-to-end with zero infrastructure for evaluation and
           tests. Its distances saturate (~1 for any two different
           tiles), so verdicts through it exercise the pipeline, the
           gate economics and the ledger — never perception quality.
  http     HttpEmbedder: any HTTP service answering the documented
           shape (multipart POST of a tile PNG, JSON vector back).
           URL, multipart field name and response key are config.
  jev      JevRackEmbedder: the origin fleet's JEV rack (I-JEPA),
           which happens to speak exactly the http shape. One
           adapter among others, not the definition of the seam.

Gate: frame 0 embeds every tile (baseline). Later frames embed a
tile iff its mean-abs diff vs the previous frame exceeds --gate.
This is the wave-3 CHANGED strategy, live.

Freshness: every frame is hashed (cheap, over a downsampled view);
frame_end carries the hash and whether this frame is byte-identical
to the previous one, so the service can tell a frozen source from a
quiet scene (the soak incident — see docs/EXAMPLES.md E3).
"""
import argparse, glob, hashlib, io, json, os, subprocess, sys, tempfile, time
import numpy as np
import requests
from PIL import Image

# The tile grid is configurable (config keys grid_rows/grid_cols on
# the Rust side, --grid RxC here); 4x4 is the default — every
# measured number in this repo's record was taken on it
# (docs/GRID.md). The two halves must agree: tile count is the
# contract between ingest and service.
GRID_ROWS = 4
GRID_COLS = 4


def fixtures_dir():
    """Where the bundled cold fixtures live, in resolution order:

    1. `--fixtures-dir DIR` (explicit config, set in main()),
    2. `$TELLTAPE_FIXTURES_DIR`,
    3. the fixtures bundled in this repo at `crates/telltape/fixtures/`
       (resolved relative to this file, never the CWD or $HOME).

    A stranger running from a fresh clone always lands on (3): no
    path in anyone's home directory is a default anywhere in this
    file. A full local frame library (e.g. a larger wave-3 set) is
    reachable only through the explicit TELLTAPE_WAVE3_A_GLOB /
    TELLTAPE_WAVE3_B_GLOB / TELLTAPE_WAVE3_B_FILE overrides in
    wave3_frames().
    """
    if getattr(fixtures_dir, "override", None):
        return fixtures_dir.override
    env = os.environ.get("TELLTAPE_FIXTURES_DIR")
    if env:
        return env
    return os.path.join(os.path.dirname(os.path.abspath(__file__)),
                        os.pardir, "fixtures")


def now_us():
    return int(time.time() * 1_000_000)


def tiles_of(frame, rows=GRID_ROWS, cols=GRID_COLS):
    h, w = frame.shape[:2]
    th, tw = h // rows, w // cols
    return [frame[r * th:(r + 1) * th, c * tw:(c + 1) * tw]
            for r in range(rows) for c in range(cols)]


def frame_hash(frame):
    """Cheap novelty hash: SHA-256 over a 32x32 downsampled view of
    the frame. Byte-identical source frames hash identically; any
    real camera noise changes the hash. Used only for freshness
    (is this the same image again?), never for perception."""
    small = frame[::max(1, frame.shape[0] // 32),
                  ::max(1, frame.shape[1] // 32)]
    return hashlib.sha256(np.ascontiguousarray(
        np.clip(small, 0, 255).astype(np.uint8)).tobytes()).hexdigest()[:16]


# ---------------- embedder seam ----------------

class Embedder:
    """The seam: embed(tile_img) -> (vector, embed_ms).

    `dim` is the vector width this adapter produces; the Emitter
    checks it against the configured embed_dim once at startup
    (probe), so a width mismatch fails once, loudly, at start —
    not per tile, mid-stream."""

    name = "abstract"
    dim = 0

    def embed(self, tile_img):
        raise NotImplementedError

    def probe(self):
        """Embed one tiny synthetic tile and verify the width."""
        tile = np.zeros((8, 8, 3), dtype=np.float32)
        vec, _ = self.embed(tile)
        if len(vec) != self.dim:
            raise RuntimeError(
                f"embedder {self.name}: probe returned dim {len(vec)}, "
                f"expected {self.dim} — fix --embed-dim or the service")
        return True


class HttpEmbedder(Embedder):
    """Generic HTTP embedding service: POST the tile as a PNG in a
    multipart form field, read a JSON vector from a response key."""

    name = "http"

    def __init__(self, url, field="file", key="embedding", dim=1280):
        self.url = url
        self.field = field
        self.key = key
        self.dim = dim
        self.session = requests.Session()

    def embed(self, tile_img):
        buf = io.BytesIO()
        Image.fromarray(tile_img.astype(np.uint8)).save(buf, format="PNG")
        data = buf.getvalue()
        t0 = time.perf_counter()
        r = self.session.post(
            self.url,
            files={self.field: ("tile.png", data, "image/png")},
            timeout=600)
        dt = (time.perf_counter() - t0) * 1000.0
        r.raise_for_status()
        emb = r.json()[self.key]
        return [float(x) for x in emb], dt


class JevRackEmbedder(HttpEmbedder):
    """The origin fleet's JEV rack (I-JEPA over HTTP) — the adapter
    the tool was first built against. It speaks the generic shape,
    so this class is only the rack's URL layout with a name."""

    name = "jev-rack"

    def __init__(self, rack_url, dim=1280):
        super().__init__(rack_url.rstrip("/") + "/embed/image",
                         field="file", key="embedding", dim=dim)


class HashEmbedder(Embedder):
    """Test double: a deterministic pseudo-embedding derived from
    the tile's PNG bytes (SHA-256 expansion, unit-normalised).

    NOT a perception model. Two different tiles get near-orthogonal
    vectors (cosine distance ~1); the same bytes get the same
    vector. It exists so the pipeline, the diff gate's economics
    and the ledger can be exercised end-to-end with zero
    infrastructure — see the header of this file for exactly what
    a run through it does and does not prove."""

    name = "double"

    def __init__(self, dim=1280):
        self.dim = dim

    def embed(self, tile_img):
        t0 = time.perf_counter()
        buf = io.BytesIO()
        Image.fromarray(tile_img.astype(np.uint8)).save(buf, format="PNG")
        seed = hashlib.sha256(buf.getvalue()).digest()
        vals = []
        counter = 0
        while len(vals) < self.dim:
            block = hashlib.sha256(
                seed + counter.to_bytes(4, "little")).digest()
            for i in range(0, 32, 4):
                vals.append(
                    int.from_bytes(block[i:i + 4], "little") / 2**32 - 0.5)
            counter += 1
        v = np.array(vals[:self.dim], dtype=np.float64)
        n = float(np.linalg.norm(v))
        if n > 0:
            v = v / n
        dt = (time.perf_counter() - t0) * 1000.0
        return [float(x) for x in v], dt


# ---------------- emitter ----------------

class Emitter:
    def __init__(self, embedder, gate, rows=GRID_ROWS, cols=GRID_COLS):
        self.embedder = embedder
        self.gate = gate
        self.rows = rows
        self.cols = cols
        self.prev_tiles = None
        self.prev_hash = None

    def embed(self, tile_img):
        vec, ms = self.embedder.embed(tile_img)
        if len(vec) != self.embedder.dim:
            raise RuntimeError(
                f"embedder {self.embedder.name}: dim {len(vec)} "
                f"!= configured {self.embedder.dim}")
        return vec, ms

    def frame(self, img, frame_idx, t_capture_us=None):
        """Tile, gate, embed nominated tiles, emit records + frame_end."""
        t_capture = t_capture_us if t_capture_us is not None else now_us()
        tls = tiles_of(img, self.rows, self.cols)
        nominated = []
        diffs = []
        for ti, t in enumerate(tls):
            if self.prev_tiles is None:
                d = 255.0
            else:
                d = float(np.mean(np.abs(t - self.prev_tiles[ti])))
            diffs.append(d)
            if self.prev_tiles is None or d > self.gate:
                nominated.append(ti)
        for ti in nominated:
            emb, ms = self.embed(tls[ti])
            rec = {"seq": 0, "frame": frame_idx, "tile": ti,
                   "t_arrival_us": t_capture,
                   "diff_score": round(diffs[ti], 4),
                   "embed_ms": round(ms, 2),
                   "embedding": [round(float(x), 6) for x in emb]}
            print(json.dumps(rec), flush=True)
        h = frame_hash(img)
        repeat = self.prev_hash is not None and h == self.prev_hash
        self.prev_hash = h
        print(json.dumps({"control": "frame_end", "frame": frame_idx,
                          "t_capture_us": t_capture,
                          "nominated": len(nominated),
                          "gate_diff": self.gate,
                          "frame_hash": h,
                          "frame_repeat": repeat}), flush=True)
        self.prev_tiles = tls
        return len(nominated)


# ---------------- staged sequence (bundled fixture photos) ----------------

def _load_png(path):
    return np.asarray(Image.open(path).convert("RGB"), dtype=np.float32)


def staged_frames(rows=GRID_ROWS, cols=GRID_COLS):
    """(frames, truth). 18 frames: 0-3 scene A, 4 CUT to scene B,
    7 object added (a real-photo patch over the two tiles right of
    centre in the second row — tiles {5,6} on the default 4x4
    grid), 10-17 gradual drift (slow brightening: per-frame diff
    must stay under the gate).

    The scenes are composited from REAL photos — the bundled wave-3
    fixture frames (fixtures/wave3/A-00.png, B-00.png) and the
    fixture patch (fixtures/patch.png, a crop of A-03 at exactly the
    placement rectangle below) — not synthetic colour fields: the
    first version of this rig used flat gradient scenes and I-JEPA
    embedded the two "different" scenes nearly identically (both are
    flat fields with simple shapes to a model trained on natural
    images) — the cut scored under threshold and published nothing.
    A rig whose scenes the perception model cannot tell apart tests
    nothing. There is deliberately NO synthetic fallback: if the
    fixtures are missing the checkout is broken, and failing loudly
    beats silently testing nothing."""
    rng = np.random.default_rng(7)
    fx = fixtures_dir()
    s1 = _load_png(os.path.join(fx, "wave3", "A-00.png"))
    s2 = _load_png(os.path.join(fx, "wave3", "B-00.png"))
    patch = _load_png(os.path.join(fx, "patch.png"))  # 128x256, real photo

    def noisy(base):
        return np.clip(base + rng.normal(0, 1.0, base.shape), 0, 255)

    th, tw = 512 // rows, 512 // cols
    obj_tiles = [cols + 1, cols + 2]  # second row, tiles right of centre

    def with_object(base):
        out = base.copy()
        p = np.asarray(Image.fromarray(patch.astype(np.uint8)).resize(
            (2 * tw, th)), dtype=np.float32)
        out[th:2 * th, tw:3 * tw] = p
        return out

    frames = []
    for _ in range(4):
        frames.append(noisy(s1))
    for _ in range(3):
        frames.append(noisy(s2))
    for _ in range(3):
        frames.append(noisy(with_object(s2)))
    for k in range(8):  # drift: +1.2 brightness/frame
        frames.append(noisy(with_object(s2) + 1.2 * (k + 1)))
    truth = {"cut_frame": 4, "object_frame": 7, "object_tiles": obj_tiles,
             "drift_frames": list(range(10, 18)), "frames": len(frames)}
    return frames, truth


# ---------------- wave-3 sequences (bundled fixtures by default) ----------------

def _fixture_frames(name):
    paths = sorted(glob.glob(os.path.join(
        fixtures_dir(), "wave3", name + "-*.png")))
    if not paths:
        raise FileNotFoundError(
            f"no bundled wave-3 fixtures matching {name}-*.png under "
            f"{fixtures_dir()} — pass --fixtures-dir or set "
            f"TELLTAPE_FIXTURES_DIR (see fixtures/README.md)")
    return [_load_png(p) for p in paths]


def wave3_frames(seq):
    """A = burst photos, B = turntable frames, C = interleaved cuts.

    Default material is the bundled fixture set (6 A + 6 B — enough
    for the full C sequence, which interleaves the first 6 of each).
    A larger local library is used ONLY when explicitly configured:
    TELLTAPE_WAVE3_A_GLOB / TELLTAPE_WAVE3_B_GLOB (globs of
    image files) or TELLTAPE_WAVE3_B_FILE (one multi-frame image,
    e.g. the original animated webp — frames are centre-cropped to
    square and resized to 512x512, as the original rig did)."""
    a_glob = os.environ.get("TELLTAPE_WAVE3_A_GLOB")
    b_glob = os.environ.get("TELLTAPE_WAVE3_B_GLOB")
    b_file = os.environ.get("TELLTAPE_WAVE3_B_FILE")
    if a_glob:
        A = [_load_png(p) for p in sorted(glob.glob(a_glob))]
    else:
        A = _fixture_frames("A")
    if b_glob:
        B = [_load_png(p) for p in sorted(glob.glob(b_glob))]
    elif b_file:
        img = Image.open(b_file)
        B = []
        for i in range(getattr(img, "n_frames", 1)):
            img.seek(i)
            fr = img.convert("RGB")
            w, h = fr.size
            fr = fr.crop((0, (h - w) // 2, w, (h - w) // 2 + w)).resize((512, 512))
            B.append(np.asarray(fr, dtype=np.float32))
    else:
        B = _fixture_frames("B")
    if seq == "A":
        return A
    if seq == "B":
        return B
    C = []
    for i in range(6):
        C.append(A[i]); C.append(B[i])
    return C


# ---------------- live HTTP sources ----------------

def shot_frames(url):
    s = requests.Session()
    while True:
        r = s.get(url, timeout=30)
        r.raise_for_status()
        img = Image.open(io.BytesIO(r.content)).convert("RGB")
        yield np.asarray(img, dtype=np.float32)


def mjpeg_frames(url, cadence_ms):
    s = requests.Session()
    resp = s.get(url, stream=True, timeout=30)
    resp.raise_for_status()
    buf = bytearray()
    last = 0.0
    for chunk in resp.iter_content(chunk_size=65536):
        buf.extend(chunk)
        while True:
            soi = buf.find(b"\xff\xd8")
            if soi < 0:
                break
            eoi = buf.find(b"\xff\xd9", soi + 2)
            if eoi < 0:
                break
            jpg = bytes(buf[soi:eoi + 2])
            del buf[:eoi + 2]
            now = time.time()
            if (now - last) * 1000.0 >= cadence_ms:
                last = now
                try:
                    img = Image.open(io.BytesIO(jpg)).convert("RGB")
                except Exception:
                    continue
                yield np.asarray(img, dtype=np.float32)


# ---------------- file source (telltape index) ----------------

IMG_EXTS = (".png", ".jpg", ".jpeg", ".webp", ".bmp")


def _probe_duration_s(path):
    """Container duration via ffprobe, or None when unavailable."""
    try:
        out = subprocess.run(
            ["ffprobe", "-v", "error", "-show_entries", "format=duration",
             "-of", "csv=p=0", path],
            capture_output=True, text=True, timeout=30)
        return float(out.stdout.strip())
    except Exception:
        return None


def file_frames(path, fps, start_us):
    """Yield (frame, t_capture_us) for existing footage.

    A directory or glob is an image sequence (frames at --fps);
    an image file is a one-frame sequence; anything else is a
    video, decoded by ffmpeg at --fps into a temp dir (the
    footage itself is never modified; the temp frames are the
    decoder's working set, removed with the process's temp dir).
    """
    if os.path.isdir(path):
        files = sorted(f for f in glob.glob(os.path.join(path, "*"))
                       if f.lower().endswith(IMG_EXTS))
    elif any(ch in path for ch in "*?["):
        files = sorted(f for f in glob.glob(path)
                       if f.lower().endswith(IMG_EXTS))
    elif path.lower().endswith(IMG_EXTS):
        files = [path]
    else:
        files = None
    if files is not None:
        if not files:
            raise FileNotFoundError(f"no image frames under {path!r}")
        base = start_us or int(os.path.getmtime(files[0]) * 1_000_000)
        step = int(1_000_000 / fps)
        for k, f in enumerate(files):
            yield _load_png(f), base + k * step
        return
    # video file: anchor the epoch at (mtime - duration) when the
    # container can be probed — the recording's own start, as
    # closely as an export's metadata allows.
    dur = _probe_duration_s(path)
    if start_us:
        base = start_us
    elif dur:
        base = int((os.path.getmtime(path) - dur) * 1_000_000)
    else:
        base = int(os.path.getmtime(path) * 1_000_000)
    tmp = tempfile.mkdtemp(prefix="telltape-index-")
    subprocess.run(
        ["ffmpeg", "-v", "error", "-i", path, "-vf", f"fps={fps}",
         "-start_number", "0", os.path.join(tmp, "f-%06d.png")],
        check=True)
    step = int(1_000_000 / fps)
    for k, f in enumerate(sorted(glob.glob(os.path.join(tmp, "f-*.png")))):
        yield _load_png(f), base + k * step


# ---------------- embedder construction ----------------

def build_embedder(args):
    if args.embedder == "double":
        return HashEmbedder(dim=args.embed_dim)
    if args.embedder == "http":
        if not args.embed_url:
            raise SystemExit("--embedder http needs --embed-url")
        return HttpEmbedder(args.embed_url, field=args.embed_field,
                            key=args.embed_key, dim=args.embed_dim)
    if args.embedder == "jev":
        return JevRackEmbedder(args.rack, dim=args.embed_dim)
    raise SystemExit(f"unknown embedder {args.embedder}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--source", required=True,
                    choices=["staged", "wave3", "shot", "mjpeg", "file"])
    ap.add_argument("--file", default="",
                    help="for --source file: a video file, an image "
                         "file, a directory of images, or an image glob")
    ap.add_argument("--fps", type=float, default=2.0,
                    help="for --source file: frames per second to "
                         "take from the footage (and the rate assumed "
                         "for image sequences)")
    ap.add_argument("--start-us", type=int, default=0,
                    help="for --source file: capture epoch (us) of "
                         "frame 0; 0 = derive from the file (mtime "
                         "minus probed duration)")
    ap.add_argument("--url", default="")
    ap.add_argument("--seq", default="C", choices=["A", "B", "C"])
    ap.add_argument("--gate", type=float, default=3.0)
    ap.add_argument("--cadence-ms", type=float, default=100.0)
    ap.add_argument("--embedder", default="double",
                    choices=["double", "http", "jev"],
                    help="double = built-in test double (zero "
                         "infrastructure, NOT a perception model); "
                         "http = generic HTTP embedding service; "
                         "jev = the JEV rack adapter (I-JEPA)")
    ap.add_argument("--embed-url", default="",
                    help="for --embedder http: the embed endpoint URL")
    ap.add_argument("--embed-field", default="file",
                    help="multipart form field carrying the tile PNG")
    ap.add_argument("--embed-key", default="embedding",
                    help="JSON response key holding the vector")
    ap.add_argument("--embed-dim", type=int, default=1280,
                    help="expected embedding width; checked once at "
                         "startup against the adapter")
    ap.add_argument("--rack", default="http://127.0.0.1:8077",
                    help="for --embedder jev: the rack base URL")
    ap.add_argument("--grid", default="4x4",
                    help="tile grid ROWSxCOLS; must match the "
                         "service config's grid_rows/grid_cols")
    ap.add_argument("--max-frames", type=int, default=0)
    ap.add_argument("--truth-out", default="")
    ap.add_argument("--fixtures-dir", default="",
                    help="fixture directory override (else "
                         "$TELLTAPE_FIXTURES_DIR, else the fixtures "
                         "bundled in this repo)")
    args = ap.parse_args()
    if args.fixtures_dir:
        fixtures_dir.override = args.fixtures_dir

    embedder = build_embedder(args)
    try:
        embedder.probe()
    except Exception as e:
        print(f"ingest: embedder probe failed: {e}", file=sys.stderr)
        return 2
    print(f"ingest: embedder {embedder.name} ok (dim {embedder.dim})",
          file=sys.stderr)

    try:
        grows, gcols = (int(x) for x in args.grid.lower().split("x"))
        if not (1 <= grows <= 16 and 1 <= gcols <= 16) or grows * gcols < 2:
            raise ValueError
    except ValueError:
        print(f"ingest: bad --grid {args.grid!r} (want ROWSxCOLS, "
              f"each 1..=16, at least 2 tiles)", file=sys.stderr)
        return 2
    em = Emitter(embedder, args.gate, grows, gcols)
    timed = args.source == "file"
    if args.source == "staged":
        frames, truth = staged_frames(grows, gcols)
        if args.truth_out:
            with open(args.truth_out, "w") as fh:
                json.dump(truth, fh)
        gen = iter(frames)
    elif args.source == "wave3":
        gen = iter(wave3_frames(args.seq))
    elif args.source == "shot":
        gen = shot_frames(args.url)
    elif args.source == "file":
        if not args.file:
            print("ingest: --source file needs --file", file=sys.stderr)
            return 2
        if args.fps <= 0:
            print("ingest: --fps must be > 0", file=sys.stderr)
            return 2
        gen = file_frames(args.file, args.fps, args.start_us)
    else:
        gen = mjpeg_frames(args.url, args.cadence_ms)

    idx = 0
    for item in gen:
        t0 = time.time()
        img, t_capture = item if timed else (item, None)
        if img.shape[0] != 512 or img.shape[1] != 512:
            img = np.asarray(Image.fromarray(img.astype(np.uint8)).resize((512, 512)),
                             dtype=np.float32)
        em.frame(img, idx, t_capture)
        idx += 1
        if args.max_frames and idx >= args.max_frames:
            break
        if not timed:
            # Live sources pace themselves by cadence; file replay
            # is batch work and runs as fast as it can be read.
            wait = args.cadence_ms / 1000.0 - (time.time() - t0)
            if wait > 0:
                time.sleep(wait)
    return 0


if __name__ == "__main__":
    sys.exit(main())
