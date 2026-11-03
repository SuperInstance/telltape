#!/bin/sh
# acceptance.sh — the canonical staged acceptance, as one command.
#
# Runs the README quickstart's staged run (bundled real-photo
# fixtures: a scene cut, an object appearing, then gradual drift
# that must stay silent; the test-double embedder, so this proves
# the pipeline and the ledger, not perception) and prints the
# expected-vs-measured table. Exit 0 iff every number matches.
#
# Referenced by fixtures/README.md and prove_fixtures.py's
# closing note; it did not exist until v0.3.1 (playtest F5) —
# the README's inline config was the real path all along, and
# this script simply makes that path one command.
#
# Env: TELLTAPE_BIN (default: the workspace's release binary,
# then debug, then a release build), PYTHON (default python3 —
# keep your venv active, as the README says).
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
PY=${PYTHON:-python3}

if [ -n "${TELLTAPE_BIN:-}" ]; then
    BIN=$TELLTAPE_BIN
elif [ -x "$ROOT/target/release/telltape" ]; then
    BIN=$ROOT/target/release/telltape
elif [ -x "$ROOT/target/debug/telltape" ]; then
    BIN=$ROOT/target/debug/telltape
else
    echo "acceptance: no built telltape found; building release…" >&2
    (cd "$ROOT" && cargo build --release)
    BIN=$ROOT/target/release/telltape
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

cat > "$TMP/acceptance.conf" <<EOF
stream_id = acceptance
ledger_dir = $TMP/ledger
ingest_cmd = $PY crates/telltape/ingest/ingest.py --source staged --embedder double
embedder_name = double
embedder_version = 1
max_frames = 18
EOF

(cd "$ROOT" && "$BIN" watch --config "$TMP/acceptance.conf") > "$TMP/summary.json"

"$PY" - "$TMP" <<'PYEOF'
import json, sys

tmp = sys.argv[1]
with open(f"{tmp}/summary.json") as f:
    summary = json.load(f)
verdicts = []
with open(f"{tmp}/ledger/events.jsonl") as f:
    for line in f:
        row = json.loads(line)
        if row.get("kind") == "verdict":
            verdicts.append(row)
state = {}
with open(f"{tmp}/ledger/state.txt") as f:
    for line in f:
        if "=" in line:
            k, v = line.split("=", 1)
            state[k.strip()] = v.strip()

by_frame = {v["frame"]: v["changed_tiles"] for v in verdicts}
# (name, expected label, measured label, matches?)
checks = [
    ("frames", "18", str(summary["frames"]), summary["frames"] == 18),
    ("events", "2", str(summary["events"]), summary["events"] == 2),
    ("embeds", "34", str(summary["embeds"]), summary["embeds"] == 34),
    ("frame 4 changed tiles", "16 (all)", str(len(by_frame.get(4, []))),
     len(by_frame.get(4, [])) == 16),
    ("frame 7 changed tiles", "[5, 6]", str(by_frame.get(7)),
     by_frame.get(7) == [5, 6]),
    ("spend ratio", "0.1181", state.get("spend_ratio", "?"),
     abs(float(state.get("spend_ratio", "nan")) - 0.1181) <= 0.001),
]
ok = True
print("canonical staged acceptance (test-double embedder)")
print(f"{'check':<24} {'expected':<12} {'measured':<12}")
for name, want, got, good in checks:
    ok = ok and good
    print(f"{name:<24} {want:<12} {got:<12} {'ok' if good else 'MISMATCH'}")
print("ACCEPTANCE:", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
PYEOF
