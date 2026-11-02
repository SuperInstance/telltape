# Contributing

## The shape of this project

telltape is a small tool on purpose. The extraction that
produced it was an exercise in *subtraction*: the change
ledger is the product, and everything else — UIs, voice,
playback — is a reader of the ledger that belongs in its own
project. Contributions that add surface need either a
measurement showing the core is wrong without them, or a
design discussion first. "It would be nice if it also…" is
how the tool this was extracted from got its clutter.

## Ground rules

- **Numbers carry sources.** A performance or accuracy claim
  in docs or commit messages names the run it came from.
  Simulated/test-double numbers are labelled as such and never
  quoted beside measured ones without the label.
- **The ledger format is a contract.** Changes to what
  `ledger.rs` writes are format changes: they need the golden
  test updated in the same commit and a note in
  docs/LEDGER-FORMAT.md.
- **Process is part of the product.** Work lands with its
  reasoning in BUILD-LOG.md — decisions and their reasons,
  dead ends included — written as it happens, not summarised
  afterwards.
- **Fail loudly, refuse typed.** New caller-facing errors are
  typed error variants, not strings and not panics.

## Running the battery

```sh
cargo test --workspace
cargo clippy --workspace --all-targets   # zero warnings is the bar
cargo fmt --all --check
python3 crates/telltape/prove_fixtures.py
pip install -r crates/telltape/requirements.txt   # for the ingest
```

The staged end-to-end acceptance (README quickstart) should
still produce 18 frames / 2 events / 34 embeds after any
change to the pipeline, gate, or ledger.

## Licence of contributions

Dual-licensed MIT OR Apache-2.0, as the project is. By
contributing you agree your contribution ships under those
terms.
