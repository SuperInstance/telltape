#!/usr/bin/env python3
"""Cold proof for the bundled fixtures — GATE/DIFF LAYER ONLY.

Runs the real ingest code (staged rig + wave-3 C) from the
fixtures bundled in this repo, from whatever directory this tree
sits in, with no home-directory paths involved. No embedder
service is available in a cold checkout, so the embedder seam —
the single place ingest calls out for embeddings — is stubbed by
a recorder returning a constant placeholder vector. That means:

  PROVED HERE : fixture resolution (no home-directory paths),
                tiling, the mean-abs diff gate, and therefore the
                per-frame NOMINATED TILE SETS — the gate layer's
                complete output: baseline 16, cut frame 16/16,
                object frame exactly {5,6}, drift frames silent,
                every wave-3 C landing 16/16.
  NOT PROVED  : the verdict layer. Embedding distances, the
                cascade, and published events need an embedder —
                the built-in test double (acceptance.sh) or a real
                one. The records this run prints carry placeholder
                embeddings and must never be quoted as verdicts.

Exit 0 iff every gate-layer assertion holds.
"""
import contextlib, io, json, os, sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "ingest"))
import numpy as np
import ingest


def nominated_sets(frames):
    """Run the real Emitter over frames with the embedder seam
    stubbed by a recorder; return the nominated tile set per frame,
    read back from the actual record stream the Emitter emits."""

    class Recorder(ingest.Embedder):
        name = "recorder"
        dim = 1280

        def __init__(self):
            self.calls = []

        def embed(self, tile_img):
            self.calls.append(tile_img.shape)
            return [0.0] * self.dim, 0.0

    em = ingest.Emitter(Recorder(), gate=3.0)
    sets = []
    for i, img in enumerate(frames):
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            em.frame(img, i)
        tiles = set()
        for line in buf.getvalue().splitlines():
            rec = json.loads(line)
            if "tile" in rec:
                tiles.add(rec["tile"])
        sets.append(tiles)
    return sets


def main():
    fx = ingest.fixtures_dir()
    print(f"fixtures_dir() -> {fx}")
    assert os.path.isfile(os.path.join(fx, "wave3", "A-00.png")), "fixtures missing"
    crate_dir = os.path.dirname(os.path.abspath(__file__))
    assert os.path.abspath(fx).startswith(crate_dir), (
        "default fixtures must resolve inside this crate (relative "
        "to the script), never from a home-directory default")

    ok = True
    def chk(name, cond):
        nonlocal ok
        print(("PASS: " if cond else "FAIL: ") + name)
        ok = ok and cond

    frames, truth = ingest.staged_frames()
    sets = nominated_sets(frames)
    ALL = set(range(16))
    chk("staged baseline frame 0 nominates all 16", sets[0] == ALL)
    chk("staged frames 1-3 nominate none", all(sets[i] == set() for i in (1, 2, 3)))
    chk(f"staged cut frame {truth['cut_frame']} nominates all 16",
        sets[truth['cut_frame']] == ALL)
    chk("staged frames 5-6 nominate none", all(sets[i] == set() for i in (5, 6)))
    chk(f"staged object frame {truth['object_frame']} nominates exactly "
        f"{truth['object_tiles']}", sets[truth['object_frame']] == set(truth['object_tiles']))
    chk("staged frames 8-9 nominate none", all(sets[i] == set() for i in (8, 9)))
    chk(f"staged drift frames {truth['drift_frames']} nominate none",
        all(sets[i] == set() for i in truth['drift_frames']))

    c = ingest.wave3_frames("C")
    chk("wave-3 C has 12 frames from fixtures", len(c) == 12)
    csets = nominated_sets(c)
    chk("wave-3 C frame 0 nominates all 16", csets[0] == ALL)
    chk("wave-3 C every landing (1-11) nominates all 16",
        all(csets[i] == ALL for i in range(1, 12)))

    print("NOTE: embeddings in this run are placeholders at the "
          "embedder seam; verdict-layer acceptance runs through the "
          "built-in test double (acceptance.sh) or a real embedder.")
    print("COLD GATE-LAYER PROOF:", "ALL PASS" if ok else "FAILURES ABOVE")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
