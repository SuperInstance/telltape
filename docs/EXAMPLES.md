# Worked examples — specific things this tool has done

Drawn only from real runs, documented in the origin project
(SI-Thor/rolling-perception) as they happened. Each: what was
pointed at it, what it produced, the numbers, and the failure
or limit it exposed. Simulated numbers never appear here
without their label.

## E1 — The staged acceptance: cut, object, drift (2026-11-01)

- **Pointed at:** an 18-frame staged sequence composited from
  real photos (the fixtures bundled in this repo): frames 0–3
  scene A, a hard cut to scene B at frame 4, a real-photo
  object pasted over tiles {5,6} at frame 7, then 8 frames of
  gradual brightening.
- **Produced:** a verdict at frame 4 flagging **16/16 tiles**
  (3,347.6 ms, 16 embeds); a verdict at frame 7 flagging
  **exactly tiles {5,6}** (392.8 ms, 2 embeds); **nothing** for
  the drift frames. 34 embeds total; spend 13.4 s vs a 113.2 s
  embed-everything estimate — **spend ratio 0.118**. Lineage
  mask 31 (all five stages) on the verdicts.
- **Limit it exposed:** the *first* version of this rig used
  synthetic flat-gradient scenes and the cut published
  **nothing** — I-JEPA embeds two flat fields nearly
  identically (probe distances after the rebuild: cut min
  0.299 / mean 0.491; object tiles 0.588 / 0.651; non-object
  max 0.027). A staged scene the perception model cannot tell
  apart tests nothing; the rig is real-photo composites
  permanently, and the fixtures ship with this repo.
  Source: origin `slice1/core/README.md`, `slice1/README.md`.

## E2 — Wave-3 sequence C: a real cut at every boundary (2026-11-01)

- **Pointed at:** two real frame sets — A (12 burst photos of
  one scene), B (12 turntable frames of another) — interleaved
  A/B/A/B…, i.e. a genuine scene cut at **all 11 boundaries**,
  embedded by the real I-JEPA (rack latency p50 173 ms /
  p95 200 ms).
- **Produced:** every landing frame flagged **16/16 tiles**;
  scoreboard **precision 1.000 / recall 1.000** on C. The diff
  gate's economics on the full material: ALL = 576 embeds /
  119.6 s vs CHANGED = **312 embeds / 55.0 s (−54%)**, with
  frame-verdict agreement between the two strategies of
  **1.0000** on C; on the gradual turntable (B) the gate kept
  23/192 embeds (−88%) and stayed near-silent (2/192 tiles
  flagged). The sheaf refine ablation on the controlled
  fixture: precision **0.60 → 1.00** at recall 1.00 — the
  refine step earns its place by measurement.
- **Limit it exposed:** the gate is **a ranker, not a
  separator** — skipped tiles' drift (0.20) vs embedded tiles'
  (0.48) overlap; no pixel threshold cleanly splits changed
  from unchanged, so per-scene gate calibration is real,
  recurring work. Sequence A is kept in the record as the
  honest counterweight: regenerated "burst" frames that
  genuinely differ in embedding space flagged 148/192 tiles —
  the detector believed the embeddings, and it was right to.
  Source: origin `docs/RESULTS.md` (wave-3 rows),
  `experiments/wave3/`.

## E3 — The live 48-hour soak, IN PROGRESS — including the frozen feed (2026-11-01 → )

- **Pointed at:** a real room, via a laptop webcam dialling OUT
  to the Thor (the Windows Firewall drops unsolicited inbound;
  the camera side pushes ~2 JPEG/s to a receiver the watch
  polls through its ordinary `shot` source). Gate calibrated
  on the still night room first: 1,200 frames, nominations
  only on the baseline frame, **0 events, spend ratio 0.0008**.
- **Produced (so far):** the soak started 2026-11-01 18:18
  AKST and completes 2026-11-03 18:18 AKST; the verdict is due
  after. It live-detected the homeowner arriving at the desk
  that evening (events 43 → 48 as he crossed the frame and
  sat; the gate then reabsorbed seated-him into the baseline).
  At the last full check the ledger was still advancing and
  events were in the hundreds.
- **The failure, in the record on purpose:** on 2026-11-02 the
  owner noticed the camera light was off. The Thor side was
  alive and counting — but the feed was **frozen**: two
  `/shot.jpg` grabs 10 s apart were byte-identical, the camera
  pusher (a foreground window) had died, and the watch had
  spent ~13 h (2026-11-01 ≈21:05 → 2026-11-02 ≈10:03 AKST)
  re-polling the receiver's last frame and counting polls as
  frames. The owner relaunched the pusher; live frames and
  events resumed within minutes (393 → 415). The soak's end
  time does not move; the verdict must fence the gap.
- **Limit it exposed — and its fix, which ships in this
  repo:** `frames_seen` counted polls, not fresh images. That
  defect was gap G1 in the extraction plan; it is closed here:
  freshness is proven by image novelty (per-frame hash +
  repeat flag), a frozen source reads `freshness = frozen` and
  `silence_meaningful = false`, and the incident is a
  regression test (P6). A tool whose product is trustworthy
  silence was caught manufacturing it; the example ships with
  the tool because that is the standard the tool is held to.
  Source: origin `slice1/PROART-CAMERA.md`; incident recorded
  2026-11-02, verdict pending at the time of writing.

## E4 — A conversation *about* the ledger: presence from the chair (2026-11-02)

- **Pointed at:** the live soak ledger itself — read-only,
  through a rendered per-stream view; the soak's files were
  never written. A browser page on the laptop lent the system
  its mic and speaker; the brain was **Qwen3-Coder-30B-A3B**,
  the model that won the presence brain bench (grounded all
  content questions, refused the planted trick, quoted figures
  exactly — origin `slice2/BRAIN-BENCH.md`); speech in via
  faster-whisper (`tiny.en`, CPU), out via a resident Piper
  worker; every factual sentence fenced by a deterministic
  claim checker before it was spoken.
- **Produced:** the owner asked aloud, *"What has changed in
  the last hour?"* Transcript exact (STT 436 ms; brain
  2,620 ms; TTS 252 ms). The spoken reply cited that morning's
  real verdicts — `[proart-cam-114023]`, tiles 12, 11:43 AM,
  plus two more 11:4x events (the owner's own activity at the
  desk while setting up). Grounding violation: **null**;
  claim violations: **[]**; `reply_raw == reply` — every fence
  passed the model's raw answer untouched, and the voice was
  heard in the room.
- **Limits it exposed:** the first attempt failed *silently* —
  browsers deny `getUserMedia` on plain-http LAN origins
  (secure-context rule), so the talk button was dead until a
  browser flag was set; a household member cannot be asked to
  do that, and the page must say so instead of failing quietly
  (a product requirement for the demo, not a core gap). The
  performed reply's last sentence was clipped mid-phrase by
  the 64-token brevity cap ("…at 11:"). This example is a
  **demo on top of the core**, included because it proves the
  ledger is a thing other software can build on — which is
  the tool's whole claim. Source: origin
  `slice2/presence/README.md` (from-the-chair section).

## E5 — Reconstructing a morning from ledger numbers alone (2026-11-02)

- **Pointed at:** nothing but the soak's `events.jsonl` — no
  footage exists to consult (the receiver holds only the
  latest frame; frames were never archived, by design).
- **Produced:** the owner's arrival at his desk, read cold off
  the tile grid (4×4, row-major): 11:38:07 — six tiles fire at
  once (5, 6, 9, 10, 13, 14, the frame's centre-left mass);
  11:38–11:39 — activity settles into the lower-centre tiles
  (9, 10, 13, 14); 11:41:46 and 11:43:24 — corner tile 12
  (bottom-left) pulses alone, twice, ~90 s apart; from 11:44 —
  the bottom rows churn (15, then 14+15, a wide burst at
  11:44:25) through the presence conversation at 11:44:48.
  35 verdict rows in the 11:38–11:50 window. Read as a
  sentence: *something large crossed the middle of the room,
  work concentrated at the bottom of the frame where the desk
  is, one corner kept being disturbed, then sustained activity
  at the desk.* That is precisely what happened.
- **Limit it exposed — the honest boundary of the whole
  tool:** the tape yields a **choreography, not a scene**. It
  cannot say it was a person, let alone who; "a man sat down"
  and "a coat was hung on the chair" produce near-identical
  rows if they move the same tiles. Sixteen cells is the
  finest spatial resolution that will ever exist for that
  morning, and the within-tile content is gone by design.
  This is why the two reconstruction options (archived
  embeddings, change-frame snapshots) were ruled *wonderful
  but not necessary* (2026-11-02): paired with footage the
  user keeps, the ledger is already a seek index; without
  footage, it is a change-witness, not a recorder — which is
  what makes it safe to keep forever and hand to any reader,
  as E4 did. Source: live-ledger reading, 2026-11-02 (soak
  ledger, `run-proart`).
