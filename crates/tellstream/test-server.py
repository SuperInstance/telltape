#!/usr/bin/env python3
"""Localhost stand-in for the phone: serves the staged sequence as
IP Webcam-style JPEG stills at /shot.jpg (frame advances per GET).
Used by acceptance part 4 to exercise the exact HTTP code path the
real phone will use, without the phone."""
import argparse, io, sys
import numpy as np
from http.server import BaseHTTPRequestHandler, HTTPServer
from PIL import Image

sys.path.insert(0, "ingest")
from ingest import staged_frames  # noqa: E402

FRAMES, _ = staged_frames()
state = {"i": 0}


class H(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path != "/shot.jpg":
            self.send_response(404); self.end_headers(); return
        img = FRAMES[min(state["i"], len(FRAMES) - 1)]
        state["i"] += 1
        buf = io.BytesIO()
        Image.fromarray(np.clip(img, 0, 255).astype(np.uint8)).save(buf, format="JPEG", quality=90)
        data = buf.getvalue()
        self.send_response(200)
        self.send_header("Content-Type", "image/jpeg")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *a):
        pass


if __name__ == "__main__":
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8901)
    a = ap.parse_args()
    HTTPServer(("127.0.0.1", a.port), H).serve_forever()
