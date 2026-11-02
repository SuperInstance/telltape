# Security

## What this tool holds

By design, very little: a ledger of timestamps, tile indices,
scores, and counters. **No frames, no images, no embeddings
are stored** — the footage stays wherever the operator keeps
it. A leaked ledger reveals *when* a watched scene changed
and roughly *where in the frame*, at tile resolution. Treat
it as occupancy metadata: not nothing, and not footage.

## Trust boundaries, named plainly

- **`ingest_cmd` is a shell command from the config file,**
  executed by the service. So are `embedder_start_cmd` /
  `embedder_stop_cmd` when set. A telltape config is therefore
  *operator-trusted input, like a shell script* — never run a
  config you did not write or review. This is deliberate (a
  technician composing sources is the intended use) and is the
  sharpest edge in the tool.
- **The embedder URL is operator-chosen.** Tiles (as PNGs) are
  sent to it. Point it at services you trust; the tool cannot
  vouch for what a remote embedder does with the crops it
  receives.
- **The ingest runs as the operator's user**, reads the
  footage paths it is given, and (for `index` of video files)
  runs ffmpeg on them. Untrusted footage is still footage —
  your decoder's attack surface is ffmpeg/Pillow's, kept
  current by you.

## Reporting

This repository is prepared for publication; until it is
public, report vulnerabilities to the maintainers of the
origin project (SI-Thor/rolling-perception) through its
private channels. Once public, use the repository's private
vulnerability reporting. Please do not file public issues for
unpatched vulnerabilities.
