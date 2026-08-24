# Agent notes for this repository

`lxc` is a Rust library + CLI that treats Loxone Miniserver configs
(`.Loxone` XML) as source code: a text IR (`.lox` files) compiles against a
base config with a lockfile pinning all UUIDs. Start with
[docs/agents.md](docs/agents.md) — it is the operational guide for using the
toolchain (the loop, hard rules, error remedies).

## Build & test

```sh
cargo test                       # 37+ tests: unit, pipeline, roundtrip
cargo clippy --all-targets       # must be warning-free
cargo fmt --check
LXC_CORPUS=<dir> cargo test --test roundtrip   # opt-in: real configs
cargo run --bin lxc -- help      # the CLI
```

## Rules that matter here

- **Never edit `.Loxone` files by hand** — only through `lxc compile`.
  Never hand-edit `*.lock.json` beyond the documented operations.
- **Never commit real Miniserver configs** (personal data). Synthetic
  fixtures live in `examples/configs/`; real ones go in an untracked corpus
  dir referenced via `LXC_CORPUS`.
- The builtin connector table (`src/connectors.rs`) only accepts
  **live-verified** entries — the growth workflow is in
  [docs/implementation.md](docs/implementation.md).
- Format knowledge is documented, not guessed: check
  [docs/loxone-format.md](docs/loxone-format.md) before assuming anything
  about the XML.
- Keep `cargo test`, clippy, and fmt clean; regenerate `examples/out/` via
  `cargo run --example compile` when compiler output changes.
- License and crates.io name are **deliberately undecided** — do not add a
  license file or publish metadata.

## Doc map

| File | Purpose |
|---|---|
| [README.md](README.md) | overview, quickstart |
| [docs/vision.md](docs/vision.md) | why this exists |
| [docs/design.md](docs/design.md) | architecture, decisions D1–D11 |
| [docs/ir-spec.md](docs/ir-spec.md) | normative `.lox` language spec |
| [docs/lockfile-spec.md](docs/lockfile-spec.md) | lockfile v1 spec |
| [docs/loxone-format.md](docs/loxone-format.md) | validated `.Loxone` format facts |
| [docs/implementation.md](docs/implementation.md) | module map, testing, gotchas |
| [docs/agents.md](docs/agents.md) | **how agents use the toolchain** |
| [docs/roadmap.md](docs/roadmap.md) | Stufen, open questions |
