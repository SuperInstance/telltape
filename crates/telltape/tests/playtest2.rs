//! Cold-playtest-2 regressions (v0.3.2). The second playtest
//! (~/workspace/cold-playtest-2/PLAYTEST-REPORT.md) was run by
//! a zero-shot agent against the public v0.3.1; these tests
//! pin its findings so they cannot return:
//!
//! - B1 (blocker): a *collapsed* embedder — the same finite,
//!   right-width, non-degenerate vector for every tile —
//!   passed the probe and every per-call check, completed the
//!   run with 0 events and exit 0, and had its silence stamped
//!   meaningful over footage containing an appearing object
//!   and a full scene change. The invariant now: silence is
//!   meaningful only when the embedder has DEMONSTRATED
//!   discrimination on this run's actual inputs — at the
//!   probe (black vs white must land far apart), and at the
//!   seam (the variance watch: materially varied inputs whose
//!   embeddings never move convict the run, which stops with
//!   a `collapsed_embedder` receipt and health stamped
//!   `embedder = collapsed`). A uniformly blank scene, whose
//!   inputs never vary, must pass untouched.
//! - S4: a transport failure mid-run (HTTP 500) reached the
//!   user as a raw Python traceback, left no closing receipt
//!   in the ledger, and left health claiming `embedder = up`
//!   and `silence_meaningful = true`. Now the failure is
//!   classified by the ingest, receipted by the service as
//!   `embedder_error`, and the run closes as aborted.
//! - W3: probe-refused runs kept `embedder = up` in health.
//!   A run that never processed a frame never proved its
//!   embedder; health now says `down`.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use telltape::config::Config;
use telltape::service;

fn tempdir(tag: &str) -> String {
    let d = std::env::temp_dir().join(format!("telltape-playtest2-{tag}-{}", std::process::id()));
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

/// Parameterized stand-ins for the playtest-2 servers:
/// `constant` returns [1.0, 0, ...] for every tile (the
/// playtest's artefact verbatim in behaviour); `jitter` adds
/// a 1e-3 spike at a per-call rotating position on top of the
/// same constant — near-identical answers that a naive
/// "are the vectors bit-equal" check would wave through;
/// `smartcollapse` answers hash-expansion vectors for 8x8
/// bodies (the probe tiles, identified by their PNG IHDR)
/// and the constant for real 128x128 tiles — it passes ANY
/// startup probe and collapses only on the run's actual
/// inputs, which is exactly the case the variance watch
/// exists for; `boom` answers healthily for
/// its first 30 calls then returns HTTP 500 (the playtest's
/// S4 artefact: probe 2 + frame 0's 16 + 12 of frame 4's
/// tiles land healthy, the 13th frame-4 tile dies).
const SERVER_PY: &str = r#"
import hashlib, json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

MODE = sys.argv[1]
PORT = int(sys.argv[2])
DIM = 1280
BOOM_BUDGET = 30
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

def png_size(body):
    i = body.find(b"IHDR")
    if i < 0:
        return (0, 0)
    return (int.from_bytes(body[i + 4:i + 8], "big"),
            int.from_bytes(body[i + 8:i + 12], "big"))

class H(BaseHTTPRequestHandler):
    def do_POST(self):
        body = self.rfile.read(int(self.headers["Content-Length"]))
        calls["n"] += 1
        n = calls["n"]
        if MODE == "boom" and n > BOOM_BUDGET:
            payload = b"boom"
            self.send_response(500)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        if MODE == "constant":
            emb = [1.0] + [0.0] * (DIM - 1)
        elif MODE == "jitter":
            emb = [0.0] * DIM
            emb[0] = 1.0
            emb[n % DIM] += 0.001
        elif MODE == "smartcollapse":
            emb = healthy(body) if png_size(body) == (8, 8) else [1.0] + [0.0] * (DIM - 1)
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
    let script = format!("{dir}/embed_server2.py");
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

fn run_ingest_direct(port: u16) -> (Option<i32>, String, String) {
    let out = Command::new("python3")
        .arg(ingest_script())
        .args(["--source", "staged", "--embedder", "http"])
        .arg("--embed-url")
        .arg(format!("http://127.0.0.1:{port}/embed"))
        .args(["--embed-dim", "1280"])
        .output()
        .expect("run ingest");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// B1 at the probe: the constant embedder is refused before
/// the run, with the collapse named — black and white, the
/// two most different tiles there are, came back as the same
/// vector.
#[test]
fn constant_embedder_is_refused_at_the_probe() {
    let dir = tempdir("constant-probe");
    let (_server, port) = start_server(&dir, "constant");
    let (code, stdout, stderr) = run_ingest_direct(port);
    assert_eq!(code, Some(2), "stdout={stdout} stderr={stderr}");
    assert!(
        stderr.contains("probe collapse") && stderr.contains("cosine distance"),
        "the refusal must name the collapse: {stderr}"
    );
    assert!(stdout.is_empty(), "no records may be emitted: {stdout}");
}

/// B1 at the probe, the harder case: answers that are not
/// bit-identical — a whisper of per-call jitter over a
/// constant — are still collapse. The probe compares
/// distance, not equality.
#[test]
fn near_constant_embedder_is_refused_at_the_probe() {
    let dir = tempdir("jitter-probe");
    let (_server, port) = start_server(&dir, "jitter");
    let (code, _stdout, stderr) = run_ingest_direct(port);
    assert_eq!(code, Some(2), "stderr={stderr}");
    assert!(
        stderr.contains("probe collapse"),
        "jitter over a constant is still collapse: {stderr}"
    );
}

/// B1 + W3 through the whole service: the constant embedder's
/// run is an error, its ledger holds no verdicts, the abort
/// is receipted, and health does NOT claim the embedder was
/// up for a run that never processed a frame.
#[test]
fn constant_embedder_through_the_service_stamps_nothing_up() {
    let dir = tempdir("constant-service");
    let (_server, port) = start_server(&dir, "constant");
    let cfg = Config {
        stream_id: "constant-stream".into(),
        ledger_dir: format!("{dir}/ledger"),
        ingest_cmd: http_ingest_cmd(port),
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
        "a collapsed embedder must author no tells: {events}"
    );
    assert!(
        events.contains("\"event\":\"run_aborted\""),
        "the abort must be receipted: {events}"
    );
    let health = std::fs::read_to_string(format!("{dir}/ledger/health.txt")).unwrap();
    assert!(
        health.contains("embedder = down"),
        "a probe-refused run never proved its embedder (W3): {health}"
    );
    assert!(health.contains("silence_meaningful = false"), "{health}");
    let state = std::fs::read_to_string(format!("{dir}/ledger/state.txt")).unwrap();
    assert!(state.contains("run_outcome = aborted"), "{state}");
}

/// B1 at the seam: the smart-collapse server passes every
/// startup check — it genuinely discriminates the probe's
/// extremes — and then returns one constant vector for every
/// real tile of a scene that fully changes at frame 4. The
/// variance watch must convict it on the run's own inputs:
/// typed error, `collapsed_embedder` receipt, health stamped
/// collapsed with the silence stamp withdrawn, no verdicts.
#[test]
fn collapse_on_real_inputs_trips_the_variance_watch() {
    let dir = tempdir("smartcollapse");
    let (_server, port) = start_server(&dir, "smartcollapse");
    let cfg = Config {
        stream_id: "smart-stream".into(),
        ledger_dir: format!("{dir}/ledger"),
        ingest_cmd: http_ingest_cmd(port),
        embed_url: String::new(),
        max_frames: 18,
        ..Config::default()
    };
    match service::run(&cfg) {
        Err(service::ServiceError::EmbedderCollapsed { observations }) => {
            assert!(observations >= 12, "observations={observations}");
        }
        other => panic!("expected EmbedderCollapsed, got {other:?}"),
    }
    let events = std::fs::read_to_string(format!("{dir}/ledger/events.jsonl")).unwrap();
    assert!(
        events.contains("\"event\":\"collapsed_embedder\""),
        "the conviction must be receipted: {events}"
    );
    assert!(
        !events.contains("\"kind\":\"verdict\""),
        "the tripping frame publishes nothing, and no earlier frame changed: {events}"
    );
    let health = std::fs::read_to_string(format!("{dir}/ledger/health.txt")).unwrap();
    assert!(health.contains("embedder = collapsed"), "{health}");
    assert!(health.contains("silence_meaningful = false"), "{health}");
}

/// S4 through the whole service: HTTP 500 mid-frame-4. The
/// run errors typed, the ledger receipts the failure as
/// `embedder_error` with the transport class, the run closes
/// as aborted, and health withdraws the silence stamp — the
/// playtest's artefact ledger had only a `start` row and a
/// health file still claiming everything was fine.
#[test]
fn mid_run_transport_failure_is_receipted_and_closes_the_run() {
    let dir = tempdir("boom");
    let (_server, port) = start_server(&dir, "boom");
    let cfg = Config {
        stream_id: "boom-stream".into(),
        ledger_dir: format!("{dir}/ledger"),
        ingest_cmd: http_ingest_cmd(port),
        embed_url: String::new(),
        max_frames: 18,
        ..Config::default()
    };
    match service::run(&cfg) {
        Err(service::ServiceError::Ingest(e)) => {
            let msg = e.to_string();
            assert!(
                msg.contains("transport"),
                "the returned error carries the classified cause: {msg}"
            );
        }
        other => panic!("expected the ingest failure to surface, got {other:?}"),
    }
    let events = std::fs::read_to_string(format!("{dir}/ledger/events.jsonl")).unwrap();
    assert!(
        events.contains("\"event\":\"embedder_error\"")
            && events.contains("embedder call failed (transport)"),
        "the failure must be receipted with its class: {events}"
    );
    assert!(
        !events.contains("\"kind\":\"verdict\""),
        "the interrupted frame publishes nothing: {events}"
    );
    let health = std::fs::read_to_string(format!("{dir}/ledger/health.txt")).unwrap();
    assert!(health.contains("silence_meaningful = false"), "{health}");
    let state = std::fs::read_to_string(format!("{dir}/ledger/state.txt")).unwrap();
    assert!(state.contains("run_outcome = aborted"), "{state}");
}

/// S4 at the ingest's own surface: the failure reaches the
/// user as one classified line. No traceback — the playtest's
/// artefact showed the full requests stack.
#[test]
fn ingest_failure_reaches_the_user_without_a_traceback() {
    let dir = tempdir("boom-cli");
    let (_server, port) = start_server(&dir, "boom");
    let (code, stdout, stderr) = run_ingest_direct(port);
    assert_eq!(code, Some(1), "stdout={stdout} stderr={stderr}");
    assert!(
        stderr.contains("embedder call failed (transport)"),
        "one clean classified line: {stderr}"
    );
    assert!(
        !stderr.contains("Traceback"),
        "no raw traceback may reach the user: {stderr}"
    );
    assert!(
        stdout.contains("\"control\": \"ingest_error\"")
            && stdout.contains("\"class\": \"transport\""),
        "the service gets its classified control line"
    );
}

// ----------------------------------------------------------------
// The watch at the seam, driven by hand-written records (the
// ingest interface is the seam: any ingest, any embedder
// behind it). Constant embeddings throughout; only the pixel
// diffs and one embedding vary between the three runs.

fn stub_health() -> String {
    use std::io::Read;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let mut buf = [0u8; 512];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
        }
    });
    format!("http://{addr}")
}

/// Write a records file: `frames` frames of 16 tiles each;
/// every tile carries the constant embedding e0 except tile
/// `moved_tile` in frames >= `moved_from`, which carries e1.
/// Every record's diff_score is `diff`.
fn records_file(
    dir: &str,
    frames: u32,
    diff: f64,
    moved_tile: Option<u32>,
    moved_from: u32,
) -> String {
    use harbor_watch::record::{record_to_json, InputRecord};
    let e0: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let e1: Vec<f32> = vec![0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
    let path = format!("{dir}/records.jsonl");
    let mut f = std::fs::File::create(&path).unwrap();
    let mut seq = 0u64;
    for frame in 0..frames {
        for tile in 0..16u32 {
            let emb = if moved_tile == Some(tile) && frame >= moved_from {
                &e1
            } else {
                &e0
            };
            let r = InputRecord {
                seq,
                frame,
                tile,
                t_arrival_us: 1_700_000_000_000_000 + u64::from(frame) * 500_000,
                diff_score: diff,
                embedding: emb.clone(),
            };
            writeln!(f, "{}", record_to_json(&r)).unwrap();
            seq += 1;
        }
        writeln!(
            f,
            "{{\"control\":\"frame_end\",\"frame\":{frame},\"t_capture_us\":{},\"nominated\":16,\"gate_diff\":3.0}}",
            1_700_000_000_000_000u64 + u64::from(frame) * 500_000
        )
        .unwrap();
    }
    path
}

fn records_cfg(dir: &str, records: &str, frames: u32) -> Config {
    Config {
        stream_id: "records-stream".into(),
        ledger_dir: format!("{dir}/ledger"),
        ingest_cmd: format!("cat {records}"),
        embed_url: stub_health(),
        embedder_autostart: false,
        max_frames: frames,
        ..Config::default()
    }
}

/// Varied pixels (diff 60 — a hard cut on every tile, every
/// frame), constant embeddings: the watch trips on frame 1's
/// sixteen observations and stops the run.
#[test]
fn varied_pixels_constant_embeddings_trip_the_watch() {
    let dir = tempdir("watch-trip");
    let records = records_file(&dir, 4, 60.0, None, 0);
    let cfg = records_cfg(&dir, &records, 4);
    match service::run(&cfg) {
        Err(service::ServiceError::EmbedderCollapsed { observations: 16 }) => {}
        other => panic!("expected EmbedderCollapsed at 16 observations, got {other:?}"),
    }
    let events = std::fs::read_to_string(format!("{dir}/ledger/events.jsonl")).unwrap();
    assert!(
        events.contains("\"event\":\"collapsed_embedder\""),
        "{events}"
    );
    let health = std::fs::read_to_string(format!("{dir}/ledger/health.txt")).unwrap();
    assert!(health.contains("embedder = collapsed"), "{health}");
    assert!(health.contains("silence_meaningful = false"), "{health}");
}

/// The honest blank wall: same constant embeddings, but the
/// pixels never vary (diff 0.4). The trigger is variation
/// without response — stillness alone convicts nobody. The
/// run completes and its silence stays meaningful.
#[test]
fn blank_wall_with_constant_embeddings_stays_meaningful() {
    let dir = tempdir("watch-blank");
    let records = records_file(&dir, 4, 0.4, None, 0);
    let cfg = records_cfg(&dir, &records, 4);
    let summary = service::run(&cfg).expect("a blank wall is a clean run");
    assert_eq!(summary.frames, 4);
    let events = std::fs::read_to_string(format!("{dir}/ledger/events.jsonl")).unwrap();
    assert!(
        !events.contains("collapsed_embedder"),
        "stillness must not be receipted as collapse: {events}"
    );
    let health = std::fs::read_to_string(format!("{dir}/ledger/health.txt")).unwrap();
    assert!(health.contains("embedder = up"), "{health}");
    assert!(health.contains("silence_meaningful = true"), "{health}");
}

/// Demonstration exonerates: frame 1 alone would trip the
/// watch (16 varied, unmoved), but in frame 2 one varied
/// tile's embedding actually moves — the embedder CAN tell
/// this run's inputs apart — and the run completes quietly
/// even though every other observation stays unmoved.
#[test]
fn one_varied_tile_that_moves_exonerates_the_run() {
    let dir = tempdir("watch-exonerated");
    // The moving tile is in the first scored frame (frame 1:
    // tile 0's embedding jumps e0 -> e1 while its pixels vary
    // — demonstration), alongside 15 varied-but-unmoved tiles;
    // frame 2 adds 16 more unmoved. The count passes the trip
    // threshold, but demonstration stands the watch down for
    // the run: collapse means NEVER answering, not answering
    // rarely.
    let records = records_file(&dir, 3, 60.0, Some(0), 1);
    let cfg = records_cfg(&dir, &records, 3);
    let summary = service::run(&cfg).expect("a demonstrated embedder completes");
    assert_eq!(summary.frames, 3);
    let events = std::fs::read_to_string(format!("{dir}/ledger/events.jsonl")).unwrap();
    assert!(!events.contains("collapsed_embedder"), "{events}");
    let health = std::fs::read_to_string(format!("{dir}/ledger/health.txt")).unwrap();
    assert!(health.contains("silence_meaningful = true"), "{health}");
}
