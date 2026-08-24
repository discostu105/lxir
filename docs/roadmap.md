# Roadmap

Ordered roughly by dependency, not date. The Stufen numbering comes from
the original design sketch (preserved in the git history); everything from
it that is still relevant and not yet implemented is embedded below.

## Stufe −1 — Connector database consolidation

The two pre-existing databases (lox-cli `connector-map.json`, 195 types,
missing e.g. *all* AutoJalousie inputs; lox-sim `block_signature`, 240
types) contradict each other. The evidence path exists (`lxir observe`);
what's missing:

- [ ] An aggregator that merges `observe` output across a whole corpus into
      one reviewed `connectors.json` (type → ordered ports → direction,
      confidence, sources).
- [ ] Cross-check tooling against both legacy databases, reporting
      agreements/conflicts.
- [ ] Feed verified entries into `connectors::builtin` (workflow in
      [implementation.md](implementation.md)).

## Stufe 0 — Hardening the v0 pipeline

- [ ] **Verify the I3+ index assumption** (design decision D8): compile a
      grown `Or`, load in Loxone Config, save, diff. First open question to
      close, since it gates variadic confidence.
- [ ] Round-trip a compiled config through a real Loxone Config save and
      assert `lxir diff` semantic-emptiness (the ultimate oracle test).
      Loxone Config saves projects **offline, without a Miniserver**, so
      this is automatable as CI on a Windows VM (UI automation: open,
      Ctrl+S, close, `lxir diff`) — catching UUID regeneration, dropped
      wires, and `Nio` repair systematically instead of via live failures.
- [ ] More verified block types: timers (`TimerDelay`…), flip-flops,
      `Formula`, `Switch` — prioritized by what real modules need.
- [ ] Minting ports for extern types with observed (not just builtin)
      connector indexes, lifting the "port must exist in base" limitation.
- [ ] Preserve trailing comments and comments inside block bodies (D10
      currently drops them).

## Stufe 1 — Templates

Instantiate one definition N times (`template jalousie(...)` +
`instance`), slug-namespaced, lockfile-aware. Design open: parameter
passing, per-instance extern binding.

## Stufe 2 — Expression sugar

`jal.AutoShade = sonne.Q and (aussentemp.AQ >= 28)` desugaring into managed
comparator/gate blocks. Requires stable synthetic-slug derivation (e.g.
`__expr1_ge`) so re-desugaring never re-mints — the lockfile keys them like
hand-written blocks. The sketch proposed two backends, selectable per
expression:

| Backend | emits | pro | con |
|---|---|---|---|
| `discrete` (default) | GreaterEqual / And / Or / Not … | readable on the Config canvas | many blocks |
| `formula` | one `Formula` block | compact (a 14-block rule becomes 2) | 4-input limit, opaque in the canvas |

Open question: how far the expression semantics go — boolean/comparison
only (maps 1:1 onto discrete blocks) or the full Formula grammar (`IF`,
arithmetic).

## Stufe 3 — Verification loop

- [ ] Integration with `lox-cli sim`: compile → simulate → assert, as a
      test harness (`lxir test`?).
- [ ] A test DSL compiled straight to the simulator, bypassing the XML
      round-trip entirely (the sketch's first-class tests):

      ```text
      test "Windalarm gewinnt" {
        given { aussentemp = 30, sonne = 1, wind_alarm = 1 }
        after 10 ticks (dt = 0.1)
        expect jal_sued.Safety == 1
      }
      ```

      Time-dependent logic needs simulated-clock support (`clock 23:00`),
      a known gap in the current simulator.
- [ ] CI recipe: `check` + `fmt --check` + compile against a pinned base +
      `diff --exit-code` against the committed expected output.

## Stufe 4 — Multi-module projects

The sketch's target shape for a whole installation, once modules can
reference each other:

```text
haus/
  lox.toml            # target Miniserver, ConfigVersion pin, options
  haus.lock.json      # identities — generated, but committed
  externals.lxir      # externs for everything Loxone Config owns
  rooms/…  systems/…  templates/…
```

- [ ] `import` between modules; the compiler merges all modules into the
      one `.Loxone` document (the file split is source ergonomics only).
- [ ] `adopt`: move an existing unmanaged object under source control
      (`lxir adopt <uuid> --as beschattung.vorhandener_block`) — the
      block-by-block migration path for existing installations.
- [ ] Drift fingerprint: hash the unmanaged remainder (the sketch's
      `raw_digest`) so tooling can cheaply detect "another writer changed
      something" without a full diff.
- [ ] ConfigVersion pin policy: refuse compiling against a base whose
      ConfigVersion differs from the project pin; qualifying a new Loxone
      release = one oracle-CI run for that version.
- [ ] A complete end-to-end showcase module (the sketch's `pool` idea:
      water temperature, cover interlock, PV-surplus enable for the heat
      pump).

## Tooling & ecosystem

- **Editor**
  - [x] VS Code: syntax highlighting + snippets (`editor/vscode/`).
  - [ ] LSP server (`lxir lsp`): diagnostics from `Module::parse`/`validate`
        (already line-precise), completion for port names sourced from the
        base config + builtin table, go-to-definition for slugs, hover with
        resolved extern identity. The library API was shaped so this is a
        thin layer.
  - [ ] tree-sitter grammar (enables highlighting beyond VS Code and
        structural editing for agents).
- **Distribution**
  - [x] License: dual GPL-3.0-or-later / commercial, the same scheme as
        `lox` — see [../LICENSE](../LICENSE). Caveat before accepting
        outside PRs: selling commercial licenses requires unified
        copyright, so external contributions need a CLA (or equivalent) —
        undecided until it becomes relevant.
  - [ ] crates.io publication (the name `lxir` was free 2026-08-24);
        `publish = false` until wanted.
  - [ ] `lox-cli` adopting the crate for its config model (the intended end
        state), wiring transport to `lxir compile` output.

## Explicit non-goals (unchanged)

Transport, LoxCC, credentials (stay in `lox`/`lox-cli`); simulating block
semantics (stays in `lox-cli sim`); replacing Loxone Config for hardware/
visualization authoring.
