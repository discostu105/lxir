# Design

How the crate is put together, and why. The companion references are
[ir-spec.md](ir-spec.md) (the language), [lockfile-spec.md](lockfile-spec.md)
(the state file), and [loxone-format.md](loxone-format.md) (the target
format's validated facts).

## Layering

```
            ┌───────────────────────────────────────────┐
            │  ir: Module ── compile ──► LoxoneDoc      │  semantics
            │            ◄─ decompile ─                 │
            │  lock: Lockfile (identity between runs)   │
            ├───────────────────────────────────────────┤
            │  doc: objects / ports / wires / counters  │  read views
            │  connectors: builtin table + observe()    │
            │  diff: semantic comparison                 │
            ├───────────────────────────────────────────┤
            │  xml: lossless CST, byte-faithful writer  │  syntax
            └───────────────────────────────────────────┘
```

- **`xml`** stores the concrete syntax losslessly (attribute order, raw
  escaping, self-closing flags, BOM, declaration). Everything above mutates
  *through* this tree, so anything the upper layers don't understand
  round-trips untouched.
- **`doc`** is a read-only *view* layer: it never caches, always derives from
  the tree, so views cannot go stale after mutations.
- **`ir` + `lock`** hold all write semantics. `compile` is the only function
  that performs coordinated mutations.

## The ownership model

A config file has **three writers**: Loxone Config (the GUI), the Miniserver
itself (app-created autopilot rules, device registrations), and this
compiler. The compiler therefore claims ownership of exactly three kinds of
edit, and records each in the lockfile:

1. **Managed blocks** (`block` declarations) — created, rebuilt, and deleted
   by the compiler alone.
2. **Extern wires** — `<In>` elements the compiler added to ports of objects
   it does *not* own. Recorded in `lock.extern_wires` so they can be removed
   again without touching wires drawn in the GUI.
3. **Extern sets** — `Def=` values the compiler rewrote on extern ports.
   The pre-set value is recorded in `lock.set_originals` and restored when
   the `set` disappears from source.

Everything else is somebody else's, and passes through unchanged.

## The compile strategy: tear down, rebuild

Each compile:

1. Validates the module; refuses if the lock knows a managed slug the source
   lost (see *Removal trichotomy* below).
2. Resolves externs (lock pin first, then match spec; ambiguity is an error).
3. **Tears down** its previous output from the base document: removes all
   locked managed objects, removes all locked extern wires, restores all
   recorded set-originals. What remains is exactly the state owned by the
   other two writers.
4. **Rebuilds** from source: plans port lists, mints missing UUIDs, emits
   managed `<C>` elements, applies wires and sets, records everything in the
   lock.
5. Writes counters (never decreasing) and target metadata.

This makes compilation *convergent*: compiling the compiler's own output
again is a byte-level fixpoint (tested in `tests/ir.rs`), which is what makes
the real workflow — compile → upload → Config saves → download → compile —
safe.

## Identity and determinism

- Object identity is the **UUID**, never the title (titles are
  locale-volatile — see [loxone-format.md](loxone-format.md)). Externs
  resolve by `uuid` > `iname` > `title`, and the winning UUID is pinned in
  the lock; later compiles use the pin even if the title has since changed.
- New UUIDs come from a **deterministic minter**: creation time and machine
  id are caller inputs; per-block port entities are `sha256(slug)[..6]`;
  sequence counters advance per mint. No clock, no RNG anywhere in the
  library. Within a run, determinism comes from the minter; across runs,
  from the lockfile (minted UUIDs are recorded immediately and reused
  forever).
- Result: same base + module + lock + options → **same output bytes**.
  Different mint time with the same lock → still the same bytes.

## The removal trichotomy

A managed slug present in the lock but missing from source is ambiguous
(typo? intentional delete?), so the compiler refuses by default. The three
explicit resolutions:

| Intent | Mechanism | Effect on config | Effect on lock |
|---|---|---|---|
| oops, typo | fix the source | — | — |
| delete it | `allow_removals` (CLI: `--allow-removals`) | block removed | entry dropped |
| stop managing it | `Lockfile::remove_object` | block **stays** (orphan) | entry dropped |
| rename the slug | `Lockfile::rename_object` | identity survives | key renamed |

## Refuse, never guess

Anything unverified is an error, not a heuristic:

- Only block types in the **verified builtin table** (`connectors::builtin`)
  can be minted. The two pre-existing connector databases (lox-cli's map,
  lox-sim's signatures) contradict each other for many types; only entries
  confirmed against real configs are admitted. Growth path: `lxir observe`
  gathers evidence → cross-check → verify live → extend the table.
- Wiring or `set`ting an extern port whose `<Co>` is absent from the base
  config is an error (the compiler will not invent port UUIDs for types
  whose connector indexes it cannot verify). The error lists the ports that
  *do* exist.
- Extern resolution with zero or multiple matches is an error listing the
  candidates.

## Decisions and rationale (ADR-style)

- **D1 — Byte-faithful CST, not a DOM.** A conforming XML parser rejects or
  corrupts real `.Loxone` files (digit-leading attribute names, raw newlines
  in attribute values). Hand-rolled parser + canonical writer; verified
  byte-identical on six real configs. This is the crate's foundation — a
  DOM-level writer (like lox-cli's) cannot prove it changed nothing else.
- **D2 — The lockfile pins *port* UUIDs, not just object UUIDs.** Wires
  reference port UUIDs; if ports re-minted on each compile, every compile
  would rewrite every wire.
- **D3 — Ports are minted before their object.** Matches the counter order
  observed in real Loxone Config output (`…0749, 074a, 074b` ports, then
  `…074c` object).
- **D4 — Managed blocks emit a `<Co>` for every connector**, with
  `Nio` = connector count. Verified live: Loxone emits all connectors, even
  unwired outputs.
- **D5 — `Nc` is maintained as the count of `<In>` children**, omitted at
  zero; `<Co>` attribute order is `K, Nc, Def, U`. Verified live.
- **D6 — Removal is a trichotomy** (see above), defaulting to refusal.
- **D7 — `decompile` lifts only wires that touch a managed block.** A wire
  between two externs — even one the compiler itself created — belongs to
  the config, not the IR view. `decompile(compile(m))` is therefore a
  faithful *subset* of `m`, not an inverse.
- **D8 — Variadic gate inputs (`I3`+) get connector indexes after the
  builtin ports.** *Assumption*: no config in the validation corpus contains
  a grown gate. To be verified the first time such a compile passes through
  Loxone Config; tracked in [roadmap.md](roadmap.md).
- **D9 — Transport is out of scope.** The library is pure (bytes → bytes);
  FTP/LoxCC/credentials live in `lox` / `lox-cli`.
- **D10 — Whole-line comments are AST items**, so `lxir fmt` is
  non-destructive. Trailing comments and comments inside block bodies are
  documented as not preserved (v0).
- **D11 — Counters (`NextObj`) advance by one per minted managed object**
  and never decrease (`Lockfile::absorb_counters` takes the max of lock and
  document). Whether ports also consume `NextObj` is unknown; object-only is
  the conservative reading of observed files.
