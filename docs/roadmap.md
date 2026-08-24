# Roadmap

Ordered roughly by dependency, not date. The Stufen numbering follows the
original sketch (`../lox-ir-design-skizze.md`).

## Stufe −1 — Connector database consolidation

The two pre-existing databases (lox-cli `connector-map.json`, 195 types,
missing e.g. *all* AutoJalousie inputs; lox-sim `block_signature`, 240
types) contradict each other. The evidence path exists (`lxc observe`);
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
      assert `lxc diff` semantic-emptiness (the ultimate oracle test).
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

`wire (aussentemp.Q >= 28) and sonne.Q -> jal.AutoShade` desugaring to
managed comparator/gate blocks with generated slugs. Requires stable slug
derivation so re-desugaring doesn't re-mint.

## Stufe 3 — Verification loop

- [ ] Integration with `lox-cli sim`: compile → simulate → assert, as a test
      harness (`lxc test`?).
- [ ] CI recipe: `check` + `fmt --check` + compile against a pinned base +
      `diff --exit-code` against the committed expected output.

## Tooling & ecosystem

- **Editor**
  - [x] VS Code: syntax highlighting + snippets (`editor/vscode/`).
  - [ ] LSP server (`lxc lsp`): diagnostics from `Module::parse`/`validate`
        (already line-precise), completion for port names sourced from the
        base config + builtin table, go-to-definition for slugs, hover with
        resolved extern identity. The library API was shaped so this is a
        thin layer.
  - [ ] tree-sitter grammar (enables highlighting beyond VS Code and
        structural editing for agents).
- **Distribution**
  - [ ] Decide license (Skizze §9.5; sibling repos are GPL-3/AGPL-3 +
        commercial) and crates.io name (`lxc` is taken) — both deliberately
        open; `publish = false` until then.
  - [ ] `lox-cli` adopting the crate for its config model (the intended end
        state), wiring transport to `lxc compile` output.

## Explicit non-goals (unchanged)

Transport, LoxCC, credentials (stay in `lox`/`lox-cli`); simulating block
semantics (stays in `lox-cli sim`); replacing Loxone Config for hardware/
visualization authoring.
