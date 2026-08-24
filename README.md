# lxc — Loxone config model

A standalone Rust library for treating Loxone Miniserver configurations
(`.Loxone` XML) as **source code**: parse them losslessly, understand their
identity model, express logic in a small text IR, and compile that IR back
into a config deterministically — with a lockfile pinning every UUID, the way
`terraform.tfstate` / `package-lock.json` pin identity for their ecosystems.

This crate implements the core of the design sketched in
[`lox-ir-design-skizze.md`](lox-ir-design-skizze.md), validated against real
Miniserver configs (see *Validation* below).

## Scope

**In scope** — the pure model, reusable by any tool:

| Module | What it does |
|---|---|
| `xml` | Lossless concrete-syntax parser/writer for `.Loxone` XML. Handles Loxone's spec-violations (attribute names starting with digits, raw newlines inside attribute values) that break conforming XML parsers. |
| `uuid` | The anatomy of Loxone UUIDs — creation time, mint counters, minting-machine id, connector index — plus a deterministic minter (no clock, no RNG). |
| `doc` | Semantic read layer: objects, ports, wires, counters, pages. |
| `connectors` | Port-direction knowledge: a small **verified** builtin table (`And`, `Or`, `Not`, `Equal`, `GreaterEqual`) and evidence-based inference (`observe`) over real configs. |
| `ir` | The text IR: `extern` / `block` / `wire` / `set` statements; parser, canonical printer, `compile` (base + IR + lockfile → config), `decompile` (config → IR view). |
| `lock` | The lockfile: slug → object *and per-port* UUIDs, counters, layout, extern-wire ownership, `set` originals. |
| `diff` | Semantic diff between two configs, with locale-rename noise flagged (a locale switch renames every built-in object title). |

**Out of scope** — deliberately: transport (FTP/HTTP to the Miniserver),
LoxCC compression, credentials. Those live in the `lox` / `lox-cli` CLIs,
which are the intended consumers of this crate.

## Quickstart

```rust
use lxc::{LoxoneDoc, Lockfile};
use lxc::ir::{compile, CompileOptions, Module};
use lxc::uuid::parse_serial;

let base = LoxoneDoc::parse(&std::fs::read("examples/configs/haus.Loxone")?)?;
let module = Module::parse(&std::fs::read_to_string("examples/ir/beschattung.lox")?)?;
let mut lock = Lockfile::new(); // or Lockfile::load(path)

let out = compile(&base, &module, &mut lock, &CompileOptions {
    machine: parse_serial("504F94112233")?,   // stamped into minted UUIDs
    mint_time_unix: 1_767_225_600,            // caller-provided → reproducible
    page_title: Some("Beschattung".into()),
    allow_removals: false,
})?;
std::fs::write("out.Loxone", out.to_bytes())?;
lock.save(std::path::Path::new("beschattung.lock.json"))?;
```

The IR itself ([`examples/ir/beschattung.lox`](examples/ir/beschattung.lox)):

```text
extern sonne: VirtualIn match iname "VI3"
extern jal_sued: AutoJalousie match title "Beschattung Süd"

block temp_hoch: GreaterEqual "Temp über 28" {
	Input2 = 28
}
block beschatten: And

wire temp_hoch.Q -> beschatten.I1
wire beschatten.Q -> jal_sued.AutoShade
set jal_sued.TargetPos = 70
```

Runnable examples (`cargo run --example …`): `compile`, `decompile`, `diff`,
`observe`, `roundtrip_check`. The committed `examples/out/` files are the
output of the `compile` example; running it again reproduces them
byte-for-byte.

## The model

- **Three writers, one file.** Loxone Config, the Miniserver itself
  (app-created autopilots, device registrations), and this compiler all
  write the config. The compiler therefore owns *only* its managed blocks,
  the wires it drew onto extern ports, and the `Def=` values it `set` —
  everything else round-trips untouched through the lossless XML layer.
- **Identity is UUID, not title.** Titles are locale-volatile (a save in a
  differently-localized Loxone Config renamed 111 built-ins in one observed
  case). Externs match by `uuid` > `iname` > `title`; once resolved, the
  lockfile pins the UUID.
- **Determinism.** Same base + module + lock + options → same output bytes.
  New UUIDs come from a deterministic minter (time and machine id are inputs;
  port entities derive from `sha256(slug)`), and are recorded in the lock, so
  they never change again. Recompiling the compiler's own output is a
  fixpoint (tested).
- **Refuse, never guess.** Only verified block types can be minted; wiring an
  extern port whose `<Co>` is absent is an error, not an invented UUID;
  a managed block vanishing from source is an error unless removal is
  explicit (`allow_removals`, or `Lockfile::remove_object` to orphan it).

## Validation

- The XML layer round-trips **byte-identically** on six real Miniserver
  configs spanning two years of history and three writers (117 KB–1.34 MB) —
  verified against a live installation. Real configs contain personal data
  and are not committed; point `LXC_CORPUS` at a directory of `.Loxone`
  files to run the corpus test:

  ```sh
  LXC_CORPUS=~/loxone-backups cargo test --test roundtrip
  ```

- UUID anatomy (epoch 2009-01-01, `ffff` + machine-id object tails,
  `<index>ff` + entity port tails, ports minted before their object) was
  established from live evidence and is encoded in `uuid`'s tests.
- Known assumption (no live evidence either way): connector indexes for
  grown gate inputs (`I3`+) are assigned *after* the builtin ports. To be
  verified the first time a compiled config with a grown gate passes through
  Loxone Config.

## Relationship to `lox` / `lox-cli`

`lox` (server API, transport, LoxCC) and `lox-cli` (config manipulation CLI,
DOM-level writer) already exist. This crate is the missing foundation both
lack: byte-faithful serialization, the UUID/identity model, and the
IR/lockfile pipeline. The intended end state is `lox-cli` (or a successor)
depending on `lxc` for everything config-model-related.

## Status / License

Early v0. The crate is `publish = false` and **the license is deliberately
not yet chosen** (see Skizze §9.5 — the surrounding repos are GPL-3/AGPL-3 +
commercial dual-licensed, and the crate name `lxc` collides with an existing
crates.io package, so both license and name are open decisions before any
publication).
