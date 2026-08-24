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
(typo? intentional delete?), so the compiler refuses by default. The
explicit resolutions — the in-language statements are preferred because
they are scoped and show up in the PR diff (D13):

| Intent | Mechanism | Effect on config | Effect on lock |
|---|---|---|---|
| oops, typo | fix the source | — | — |
| delete it | `removed <slug>` in source (or the global `allow_removals` / `--allow-removals`) | block removed | entry dropped |
| stop managing it | `Lockfile::remove_object` | block **stays** (orphan) | entry dropped |
| rename the slug | `moved <old> -> <new>` in source (or `Lockfile::rename_object`) | identity survives | key renamed |

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
- **D8 — Gate inputs are fixed: `And`/`Or` have exactly `I1`, `I2`, `Q`,
  and wiring `I3`+ is a compile error.** The original assumption (grown
  inputs take indexes after the builtin ports) was **refuted by the Wine
  oracle** (2026-08-25): Loxone Config 17 offers no way to grow a gate —
  wiring the last free input does not add one, the wire-drop connector
  picker lists only existing inputs — and a compiled `I3` at index 3
  *loads*, but **saving silently deletes the off-descriptor connector and
  its wire** while preserving everything else. Silent logic loss is the
  worst outcome a compiler can produce, so the compiler refuses `I3`+ with
  a hint to cascade 2-input gates. The corpus concurs: zero gates with
  more than two inputs across six configs
  ([oracle-wine.md](oracle-wine.md)).
- **D9 — Transport is out of scope.** The library is pure (bytes → bytes);
  FTP/LoxCC/credentials live in `lox` / `lox-cli`.
- **D10 — All comments survive the round trip**, so `lxir fmt` is
  non-destructive: whole-line comments are AST items, trailing comments
  attach to their statement, parameter line, or closing `}`, and comments
  inside block bodies are body items. (Originally `} # text` was
  canonicalized onto its own line; it now stays attached to the `}` —
  detaching a comment from its anchor lost intent for no gain.)
- **D11 — Counters (`NextObj`) advance by one per minted managed object**
  and never decrease (`Lockfile::absorb_counters` takes the max of lock and
  document). Whether ports also consume `NextObj` is unknown; object-only is
  the conservative reading of observed files. Oracle observation
  (2026-08-24): Loxone Config itself burns **+2** per open+save cycle even
  with zero objects added — counter consumption is not tied to persisted
  objects, and max() absorbs it safely
  ([oracle-wine.md](oracle-wine.md)).
- **D12 — A dedicated grammar, not a YAML/KDL/HCL host syntax.** Weighed in
  the sketch phase: YAML would have been the fastest start, but wiring
  expressed in YAML is exactly the ergonomics this project exists to remove,
  and the planned sugar (expressions, templates — see
  [roadmap.md](roadmap.md)) wants first-class syntax. The grammar stays
  small enough to hand-parse ([ir-spec.md](ir-spec.md)).
- **D13 — Lifecycle lives in the language: `removed` and `moved`
  statements** (2026-08-25). The original design routed deletion through a
  CLI flag and rename through a library call — out-of-band state surgery
  that never appears in a reviewable diff, the exact gap Terraform closed
  by moving `state rm`/`state mv` into `removed`/`moved` blocks. lxir's
  whole pitch is "every change is a one-line diff in a PR", so intent to
  delete or rename must be expressible in source: `removed <slug>` is
  scoped to one block (unlike `--allow-removals`, which authorizes *all*
  removals in a run), `moved <old> -> <new>` keeps object and port UUIDs
  across a slug rename, and both are idempotent no-ops once applied so
  history can keep them. The flag and library calls remain as escape
  hatches.
- **D14 — `set` is for extern ports only; managed parameters live in the
  block body** (2026-08-25). v0 allowed `set` on managed blocks as a body
  synonym with an override rule — two spellings for one thing, and a keyword
  with different ownership semantics per target (tracked-and-reverting on
  externs, plain param on blocks). One meaning per keyword: `set` now
  always means "tracked write to somebody else's port", and the validator
  points managed-block sets at the body.
- **D15 — Values are typed tokens, not sniffed strings** (2026-08-25). The
  AST records how a value was written (`Number` | `Str` | `Ref`), and the
  formatter emits by variant. The previous content-sniffing emitter printed
  string values like `"5+"` as bare tokens that did not re-parse, breaking
  the advertised `parse(to_text(m)) == m` fixpoint. One canonicalization
  remains, at parse time: a quoted string that reads exactly as a number
  becomes the bare number. Bare identifiers in value position are `let`
  references and must resolve — the previously undefined
  bare-ident-as-string lenience is gone.
