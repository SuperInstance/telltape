# Configuration reference

`watch` (and the config half of `index`) is driven by one
plain-text file: `key = value` lines, `#` comments, editable
with no tooling. Unknown keys are ignored (forward
compatibility); a bad number or boolean is a hard error
naming the key. This is the complete key list — the defaults
below are the compiled defaults in `src/config.rs`, so an
absent key is a documented choice, not a mystery.

## The run

| Key | Default | What it does |
|---|---|---|
| `stream_id` | `stream0` | The name stamped into every row of the ledger. |
| `ledger_dir` | `ledger` | Directory holding `lens.txt` / `state.txt` / `events.jsonl` / `health.txt`. |
| `ingest_cmd` | *(empty)* | Shell command producing ingest records on stdout (normally the Python ingest). Empty = the service reads records from its own stdin (tests, manual driving). |
| `stop_file` | *(empty)* | Checked once per frame; if the file appears, the run shuts down cleanly. Empty = no stop file. |
| `score_series_path` | *(empty)* | Path of the score-series sidecar: one JSONL row per frame the gate evaluates — its best smoothed score, near-miss and fired flags, and every tile's smoothed score — for offline analyses that need the sub-threshold life of a run (see "The score series" in [LEDGER-FORMAT.md](LEDGER-FORMAT.md)). Empty = no sidecar, and the ledger is byte-identical to a run without the feature. The file is created fresh for the run and grows at roughly 17 MB per 100k frames at the default grid (~50 MB/day at 2 fps). Also settable as `--score-series <path>` on the command line, which wins over the config key. |
| `max_frames` | `0` | Stop after this many frames. `0` = unlimited. |
| `soak` | `false` | Soak mode: run unattended to the stop file / EOF, appending precision self-report placeholders to `state.txt`. |

## The embedder

| Key | Default | What it does |
|---|---|---|
| `embed_url` | *(none)* | Base URL of the embedder the ingest calls. **No default, deliberately**: an unset embedder means the ingest's built-in test double, not a silent dial to a port that happens to be someone's. Alias: `rack_url`. |
| `embed_health_url` | *(derived)* | URL polled for embedder liveness before the ingest starts. Empty = derive `{embed_url origin}/health` (scheme + authority, path discarded). If `embed_url` is also empty, no external check is possible and none is made. See the health-endpoint requirement in [BRING-YOUR-OWN-EMBEDDER.md](BRING-YOUR-OWN-EMBEDDER.md). |
| `embedder_name` | *(undeclared)* | First half of the embedder identity recorded in every ledger (`jev-rack`, `http`, `double`, or your own BYO name). Undeclared is not an error: the run then records the identity of the adapter the config itself selects (`double@1`, or `http@<embed_url>`). |
| `embedder_version` | *(undeclared)* | Second half of the identity (`name@version`). For the generic HTTP adapter the endpoint URL is the only version a generic endpoint has; if you change the model behind an endpoint, bump this — two different embedders' spaces are related by at best a rotation, so a ledger must be able to say which one produced its tells. |
| `recalibrate` | `false` | Your explicit acknowledgement of an embedder swap (or of stamping a pre-0.1.1 ledger that records no identity): opening a ledger whose recorded identity differs from this config's is otherwise a typed refusal, never a silent continue. With it set, the run proceeds with fresh baselines and receipts the swap. |
| `embedder_autostart` | `false` | If the embedder is down at startup, run `embedder_start_cmd` (and remember we did). Alias: `rack_autostart`. |
| `embedder_start_cmd` | *(empty)* | Shell command that starts the embedder. Empty = this service starts nothing. |
| `embedder_stop_cmd` | *(empty)* | Shell command that stops the embedder, run at exit **only** if this service started it and `stop_embedder_on_exit` is set. |
| `stop_embedder_on_exit` | `true` | Stop the embedder at exit, only if this service started it. Alias: `stop_rack_on_exit`. |
| `embed_dim` | `1280` | Expected embedding width of the configured embedder (1280 is the I-JEPA width). Your embedder's width goes here **and** in the ingest command — see the contract doc. |
| `embed_field` | `file` | Multipart field name the generic HTTP adapter sends. |
| `embed_key` | `embedding` | JSON response key the generic HTTP adapter reads the vector from. |

## The decision

| Key | Default | What it does |
|---|---|---|
| `threshold` | `0.25` | Cosine-distance threshold a tile's smoothed score must reach to publish a tell. Calibrated against I-JEPA's geometry; a different embedder is a different geometry — calibrate from your own ledger's `max_smoothed_score` / `near_miss_frames` (see "Calibration" in the contract doc) before trusting it. |
| `alpha` | `0.1` | Smoothing factor of the per-tile score (exponential moving average weight given to each new observation). |
| `focus_k` | `4` | How many top-scoring tiles a verdict names as its focus. |
| `grid_rows` / `grid_cols` | `4` / `4` | The tile grid. The ingest must be run with the matching `--grid` value — the tile count is the contract between the two halves. An invalid pair is refused at load, with the keys named. |
| `gate_diff` | `3.0` | The ingest's nomination gate: mean-abs pixel diff (0–255) above which a tile is embedded at all. `watch` configs carry it inside their hand-written `ingest_cmd`; `index` composes its command from this key. |

## Indexing footage

| Key | Default | What it does |
|---|---|---|
| `ingest_script` | *(from `$TELLTAPE_INGEST`)* | Path to `ingest/ingest.py`, used by `index` to compose its command. No home-directory path is ever a default; an unset key and unset variable is an error naming the key. |
| `index_fps` | `2.0` | Frames per second `index` takes from a video file (and the rate assumed for image sequences). |
| `index_start_us` | `0` | Capture epoch (µs) of frame 0 for `index`; `0` = derive from the footage file (mtime minus probed duration). |
| `source_footage` | `false` | The source is recorded footage, not a live feed (set by the `index` verb itself; a `watch` config whose `ingest_cmd` replays files may set it too). Freshness accounting is a live-source property: for footage the ledger never declares the source frozen, never withdraws meaning from its silence on those grounds, and stamps its lens `source_kind = footage`. It also decides the `verdict_latency_ms` basis in exports (see [LEDGER-FORMAT.md](LEDGER-FORMAT.md)). |

## The aliases

`rack_url`, `rack_autostart` and `stop_rack_on_exit` are the
origin tool's key names, accepted as aliases of `embed_url`,
`embedder_autostart` and `stop_embedder_on_exit` because
config files are technician-edited text that outlives
binaries. The new name wins when both are present.
