//! Cold-playtest regressions (v0.3.1). The playtest
//! (~/workspace/cold-playtest/PLAYTEST-REPORT.md) was run by a
//! zero-shot agent with nothing but the public repo; these
//! tests pin its findings so they cannot return:
//!
//! - F1: the BYO doc's own sketch server, returning a zero
//!   vector for every tile, reproduced the canonical staged
//!   acceptance *perfectly* — the old scorer answered a
//!   zero-norm vector with maximal distance. Here the same
//!   server (and a NaN variant) must refuse loudly at the
//!   probe, and a server that turns degenerate mid-run must
//!   stop the run with a `degenerate_embedder` receipt.
//! - The hash double must be provably unable to trip the
//!   degeneracy guard it now lives under.
//! - F6: an unknown verb is refused as a usage error.

mod common;

use common::run_bin;
use std::io::Write;
use std::process::{Child, Command, Stdio};
use telltape::config::Config;
use telltape::service;

fn tempdir(tag: &str) -> String {
    let d = std::env::temp_dir().join(format!("telltape-playtest-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.to_string_lossy().to_string()
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

fn wait_ready(port: u16) {
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("embed server on port {port} never became ready");
}

struct ServerGuard(Child);
impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// One parameterized stand-in for the playtest's servers:
/// `zero` returns [0.0]*1280 (the BYO sketch, verbatim in
/// behaviour), `nan` returns NaNs, `flaky` answers healthy
/// hash-expansion vectors for its first 21 calls (the probe's
/// black AND white tiles — two calls since playtest-2 B1
/// strengthened the probe — plus frame 0's sixteen, plus
/// three of frame 4's) and zeros from call 22 on — a mid-run
/// degeneracy at frame 4, tile 3.
const SERVER_PY: &str = r#"
import hashlib, json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

MODE = sys.argv[1]
PORT = int(sys.argv[2])
DIM = 1280
BUDGET = 21
calls = {"n": 0}

def healthy(body):
    seed = hashlib.sha256(body).digest()
    vals = []
    c = 0
    while len(vals) < DIM:
        block = hashlib.sha256(seed + c.to_bytes(4, "little")).digest()
        for i in range(0, 32, 4):
            vals.append(int.from_bytes(block[i:i + 4], "little") / 2**32 - 0.5)
        c += 1
    v = vals[:DIM]
    n = sum(x * x for x in v) ** 0.5
    return [x / n for x in v]

class H(BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers["Content-Length"]))
        calls["n"] += 1
        if MODE == "zero":
            emb = [0.0] * DIM
        elif MODE == "nan":
            emb = [float("nan")] * DIM
        elif calls["n"] > BUDGET:
            emb = [0.0] * DIM
        else:
            emb = healthy(body)
        payload = json.dumps({"embedding": emb}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)
    def log_message(self, *a): pass

HTTPServer(("127.0.0.1", PORT), H).serve_forever()
"#;

fn start_server(dir: &str, mode: &str) -> (ServerGuard, u16) {
    let script = format!("{dir}/embed_server.py");
    std::fs::File::create(&script)
        .unwrap()
        .write_all(SERVER_PY.as_bytes())
        .unwrap();
    let port = free_port();
    let child = Command::new("python3")
        .arg(&script)
        .arg(mode)
        .arg(port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn embed server");
    wait_ready(port);
    (ServerGuard(child), port)
}

fn ingest_script() -> String {
    format!("{}/ingest/ingest.py", env!("CARGO_MANIFEST_DIR"))
}

fn http_ingest_cmd(port: u16) -> String {
    format!(
        "python3 {} --source staged --embedder http --embed-url http://127.0.0.1:{port}/embed --embed-dim 1280",
        ingest_script()
    )
}

/// The probe refusal, asserted where it happens: the ingest
/// itself exits 2 and names the degeneracy on stderr, before a
/// single frame exists.
#[test]
fn degenerate_embedders_are_refused_at_the_probe() {
    for (mode, reason) in [("zero", "zero norm"), ("nan", "non-finite component")] {
        let dir = tempdir(mode);
        let (_server, port) = start_server(&dir, mode);
        let out = Command::new("python3")
            .arg(ingest_script())
            .args(["--source", "staged", "--embedder", "http"])
            .arg("--embed-url")
            .arg(format!("http://127.0.0.1:{port}/embed"))
            .args(["--embed-dim", "1280"])
            .output()
            .expect("run ingest");
        assert_eq!(
            out.status.code(),
            Some(2),
            "{mode}: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("probe embedding for a black tile is degenerate")
                && stderr.contains(reason),
            "{mode}: the refusal must name the defect: {stderr}"
        );
        assert!(
            out.stdout.is_empty(),
            "{mode}: no records may be emitted: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

/// Through the whole service: the zero embedder's run is an
/// error, and its ledger contains no verdicts — the exact
/// opposite of the playtest's Appendix B, where this run
/// printed the canonical acceptance summary.
#[test]
fn zero_embedder_produces_no_ledger_verdicts() {
    let dir = tempdir("zero-service");
    let (_server, port) = start_server(&dir, "zero");
    let cfg = Config {
        stream_id: "zero-stream".into(),
        ledger_dir: format!("{dir}/ledger"),
        ingest_cmd: http_ingest_cmd(port),
        // No embed_url: the sketch server has no /health, and a
        // hand-written ingest_cmd is the playtest's own shape.
        embed_url: String::new(),
        max_frames: 18,
        ..Config::default()
    };
    match service::run(&cfg) {
        Err(service::ServiceError::Ingest(_)) => {}
        other => panic!("expected the ingest's refusal to surface, got {other:?}"),
    }
    let events = std::fs::read_to_string(format!("{dir}/ledger/events.jsonl")).unwrap();
    assert!(
        !events.contains("\"kind\":\"verdict\""),
        "a degenerate embedder must author no tells: {events}"
    );
}

/// The mid-run case: healthy through the probe and frame 0,
/// zeros from frame 4's fourth tile on. The run must stop at
/// frame 4 with the typed refusal, the ledger must receipt it
/// as `degenerate_embedder` naming frame 4, and frame 4 must
/// publish nothing — no partial-quiet continuation.
#[test]
fn mid_run_degeneracy_stops_the_run_with_the_receipt() {
    let dir = tempdir("flaky");
    let (_server, port) = start_server(&dir, "flaky");
    let cfg = Config {
        stream_id: "flaky-stream".into(),
        ledger_dir: format!("{dir}/ledger"),
        ingest_cmd: http_ingest_cmd(port),
        embed_url: String::new(),
        max_frames: 18,
        ..Config::default()
    };
    match service::run(&cfg) {
        Err(service::ServiceError::Pipeline(
            harbor_watch::pipeline::PipelineError::DegenerateEmbedding {
                frame: 4,
                tile: 3,
                reason: "zero norm",
                ..
            },
        )) => {}
        other => panic!("expected DegenerateEmbedding at frame 4 tile 3, got {other:?}"),
    }
    let events = std::fs::read_to_string(format!("{dir}/ledger/events.jsonl")).unwrap();
    assert!(
        events.contains("\"event\":\"degenerate_embedder\""),
        "the stop must be receipted: {events}"
    );
    assert!(events.contains("frame 4 tile 3"), "{events}");
    assert!(
        !events.contains("\"kind\":\"verdict\""),
        "frame 4 must publish nothing: {events}"
    );
}

/// The double lives under the same probe now: prove it can
/// never trip the guard — black, white, gradient and two
/// hundred random tiles all embed to finite, non-zero vectors,
/// and the probe passes. Playtest-2 B1 strengthened the probe
/// a second way (collapse: black and white must embed FAR
/// apart, past PROBE_DISCRIMINATION_EPSILON) and the staged
/// acceptance depends on the double passing it — so the
/// double's discrimination is proven here explicitly, not
/// just implied by probe() returning.
#[test]
fn the_hash_double_can_never_be_degenerate() {
    let dir = tempdir("double");
    let script = format!("{dir}/check_double.py");
    let ingest_dir = format!("{}/ingest", env!("CARGO_MANIFEST_DIR"));
    let mut f = std::fs::File::create(&script).unwrap();
    write!(
        f,
        "import sys\nsys.path.insert(0, {ingest_dir:?})\nfrom ingest import HashEmbedder, degenerate_reason, cosine_distance, PROBE_DISCRIMINATION_EPSILON\nimport numpy as np\nemb = HashEmbedder(dim=1280)\nemb.probe()\nblack, _ = emb.embed(np.zeros((8, 8, 3), dtype=np.float32))\nwhite, _ = emb.embed(np.full((8, 8, 3), 255.0, dtype=np.float32))\nd = cosine_distance(black, white)\nassert d > PROBE_DISCRIMINATION_EPSILON, f'double does not discriminate: black/white distance {{d}}'\nprint(f'DOUBLE-DISCRIMINATES {{d:.4f}}')\nrng = np.random.default_rng(3)\ntiles = [np.zeros((128, 128, 3), dtype=np.float32), np.full((128, 128, 3), 255.0, dtype=np.float32)]\ntiles += [rng.normal(128, 40, (128, 128, 3)).astype(np.float32) for _ in range(200)]\nfor t in tiles:\n    vec, _ = emb.embed(t)\n    assert degenerate_reason(vec) is None, degenerate_reason(vec)\nprint('DOUBLE-OK')\n"
    )
    .unwrap();
    drop(f);
    let out = Command::new("python3")
        .arg(&script)
        .output()
        .expect("run double check");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success()
            && stdout.contains("DOUBLE-OK")
            && stdout.contains("DOUBLE-DISCRIMINATES"),
        "stdout: {stdout} stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Playtest F6: an unknown verb is a usage error (exit 2)
/// naming the real verbs — not a fall-through to `watch` and
/// a complaint about a missing default config file.
#[test]
fn unknown_verbs_are_refused_with_usage() {
    let (code, _, stderr) = run_bin(&["bogusverb"], &[]);
    assert_eq!(code, 2, "{stderr}");
    assert!(stderr.contains("unknown verb \"bogusverb\""), "{stderr}");
    assert!(stderr.contains("watch, index, query, export"), "{stderr}");
    // The documented forms still parse: --help is exit 0 and
    // lists the same verbs.
    let (code, _, stderr) = run_bin(&["--help"], &[]);
    assert_eq!(code, 0, "{stderr}");
    assert!(stderr.contains("telltape watch"), "{stderr}");
}
