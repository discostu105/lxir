# lxir

**lxir** (pronounced like *elixir*) is **config-as-code for Loxone**: a
small text language for Miniserver logic — blocks, wires, parameters — and
a deterministic compiler that applies it to a real `.Loxone` config without
disturbing anything else in the file.

```text
# Beschattung Süd — Beispielmodul.
# Externe Objekte gehören Loxone Config; Blöcke gehören dem Compiler.

extern aussentemp: VirtualIn match iname "VI1"
extern wind_alarm: VirtualIn match iname "VI2"
extern sonne: VirtualIn match iname "VI3"
extern jal_sued: AutoJalousie match title "Beschattung Süd"

block temp_hoch: GreaterEqual "Temp über 28" {
	Input2 = 28
}
block beschatten: And

wire aussentemp.Q -> temp_hoch.Input1
wire temp_hoch.Q -> beschatten.I1
wire sonne.Q -> beschatten.I2
wire beschatten.Q -> jal_sued.AutoShade
wire wind_alarm.Q -> jal_sued.Safety

set jal_sued.TargetPos = 70
```

`lxir compile` turns this into exactly the XML Loxone Config would have
drawn — UUIDs, counters, per-port connectors, canvas layout — and records
every identity it minted in a lockfile, so the next compile changes only
what you changed.

## Why

A Miniserver's entire behavior lives in one opaque `.Loxone` XML file,
editable only in a Windows GUI. There is no reviewable diff, no reuse (ten
shading rules are ten hand-drawn copies), no way to test before uploading
to the house, and no safe way for scripts or AI agents to author logic —
the file's identity model (UUIDs, counters, per-port identifiers) is
undocumented, and naive editing corrupts it.

lxir treats the config the way Terraform treats infrastructure: the `.lxir`
module is source, the `.Loxone` file is the deployed artifact, and the
lockfile — like `terraform.tfstate` — pins every identity in between. A
wire change becomes a one-line diff in a pull request.

Crucially, the language describes **only the logic you choose to manage**.
Everything else — hardware, rooms, visualization, other people's logic —
passes through the compiler byte-for-byte untouched. Manage one shading
rule today; the rest of the config never notices. See
[docs/vision.md](docs/vision.md).

## How it stays safe

- **Three writers, one file.** Loxone Config, the Miniserver itself
  (app-created autopilots, device registrations), and this compiler all
  write the config. The compiler owns *only* its managed blocks, the wires
  it drew onto extern ports, and the `Def=` values it `set` — everything
  else round-trips untouched through a byte-faithful XML layer.
- **Identity is UUID, not title.** Titles are locale-volatile (one observed
  save renamed 111 built-ins). Externs match by `uuid` > `iname` > `title`;
  once resolved, the lockfile pins the UUID — object *and* every port.
- **Determinism.** Same base + module + lock + options → same output bytes.
  New UUIDs come from a deterministic minter (no clock, no RNG), are
  recorded in the lock, and never change again. Recompiling the compiler's
  own output is a byte-level fixpoint (tested).
- **Refuse, never guess.** Only live-verified block types can be minted;
  wiring a port the base config doesn't have is an error, not an invented
  UUID; a managed block vanishing from source is an error unless removal is
  explicit.

## Quickstart (CLI)

```sh
cargo run --bin lxir -- help         # or: cargo install --path .

lxir check modules/beschattung.lxir  # parse + validate (line-numbered errors)
lxir fmt --write modules/beschattung.lxir
lxir compile --base current.Loxone --module modules/beschattung.lxir \
             --lock modules/beschattung.lock.json --out out.Loxone \
             --serial 504F94112233
lxir diff current.Loxone out.Loxone  # semantic diff, locale noise flagged
lxir decompile current.Loxone        # IR view of an existing config
lxir observe current.Loxone          # port-direction evidence (JSON)
lxir roundtrip current.Loxone        # byte-fidelity self-check
```

## Quickstart (library)

```rust
use lxir::{LoxoneDoc, Lockfile};
use lxir::ir::{compile, CompileOptions, Module};
use lxir::uuid::parse_serial;

let base = LoxoneDoc::parse(&std::fs::read("examples/configs/haus.Loxone")?)?;
let module = Module::parse(&std::fs::read_to_string("examples/ir/beschattung.lxir")?)?;
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

Runnable examples (`cargo run --example …`): `compile`, `decompile`,
`diff`, `observe`, `roundtrip_check`. The committed `examples/out/` files
are the output of the `compile` example; running it again reproduces them
byte-for-byte.

## Scope

**In scope** — the pure model, reusable by any tool:

| Module | What it does |
|---|---|
| `xml` | Lossless concrete-syntax parser/writer for `.Loxone` XML. Handles Loxone's spec-violations (attribute names starting with digits, raw newlines inside attribute values) that break conforming XML parsers. |
| `uuid` | The anatomy of Loxone UUIDs — creation time, mint counters, minting-machine id, connector index — plus a deterministic minter (no clock, no RNG). |
| `doc` | Semantic read layer: objects, ports, wires, counters, pages. |
| `connectors` | Port-direction knowledge: a small **verified** builtin table (`And`, `Or`, `Not`, `Equal`, `GreaterEqual`) and evidence-based inference (`observe`) over real configs. |
| `ir` | The text language: `extern` / `block` / `wire` / `set`; parser, canonical printer, `compile` (base + module + lockfile → config), `decompile` (config → IR view). |
| `lock` | The lockfile: slug → object *and per-port* UUIDs, counters, layout, extern-wire ownership, `set` originals. |
| `diff` | Semantic diff between two configs, with locale-rename noise flagged. |

**Out of scope** — deliberately: transport (FTP/HTTP to the Miniserver),
LoxCC compression, credentials. Those live in the `lox` / `lox-cli` CLIs,
which are the intended consumers of this crate.

## Documentation

| Doc | Contents |
|---|---|
| [docs/vision.md](docs/vision.md) | Why config-as-code for Loxone; the Terraform analogy; the two-masters workflow |
| [docs/design.md](docs/design.md) | Architecture, ownership model, compile strategy, decisions D1–D12 |
| [docs/ir-spec.md](docs/ir-spec.md) | Normative spec of the `.lxir` language (v0) |
| [docs/lockfile-spec.md](docs/lockfile-spec.md) | The lockfile format (v1) and its invariants |
| [docs/loxone-format.md](docs/loxone-format.md) | Validated reverse-engineering notes on the `.Loxone` format |
| [docs/implementation.md](docs/implementation.md) | Module map, testing strategy, how to extend |
| [docs/agents.md](docs/agents.md) | Operational guide for AI agents using the toolchain |
| [docs/roadmap.md](docs/roadmap.md) | Stufen −1…4: hardening, templates, expressions, verification, multi-module |

[`AGENTS.md`](AGENTS.md) at the repo root gives agents the build/test
commands and repo rules at a glance.

## Editor support

`editor/vscode/` contains a declarative VS Code extension for `.lxir`
files: syntax highlighting, comment/bracket support, and snippets for all
four statement forms. Install by symlinking it into `~/.vscode/extensions/`
(see [editor/vscode/README.md](editor/vscode/README.md)). A language server
is scoped on the roadmap; until then `lxir check` / `lxir fmt --check`
cover the validation loop.

## Validation

- The XML layer round-trips **byte-identically** on six real Miniserver
  configs spanning two years of history and three writers (117 KB–1.34 MB)
  — verified against a live installation. Real configs contain personal
  data and are not committed; point `LXIR_CORPUS` at a directory of
  `.Loxone` files to run the corpus test:

  ```sh
  LXIR_CORPUS=~/loxone-backups cargo test --test roundtrip
  ```

- UUID anatomy (epoch 2009-01-01, `ffff` + machine-id object tails,
  `<index>ff` + entity port tails, ports minted before their object) was
  established from live evidence and is encoded in `uuid`'s tests.
- Known assumption (no live evidence either way): connector indexes for
  grown gate inputs (`I3`+) are assigned *after* the builtin ports. To be
  verified the first time a compiled config with a grown gate passes
  through Loxone Config.

## Relationship to `lox` / `lox-cli`

[`lox`](https://github.com/discostu105/lox) (server API, transport, LoxCC)
and `lox-cli` (config manipulation CLI, DOM-level writer) already exist.
This crate is the missing foundation both lack: byte-faithful
serialization, the UUID/identity model, and the IR/lockfile pipeline. The
intended end state is `lox-cli` (or a successor) depending on `lxir` for
everything config-model-related.

## License

Dual-licensed, the same scheme as `lox`: use it under the
[GPL-3.0](LICENSE-GPL), or obtain a
[commercial license](LICENSE-COMMERCIAL) for proprietary redistribution —
see [LICENSE](LICENSE). Early v0; `publish = false` until crates.io
publication is wanted.
