# Working with lxir as an AI agent

Operational guidance for AI agents (and automation generally) authoring
Loxone logic through this toolchain. The IR exists so you never have to edit
`.Loxone` XML — and you must not.

## Hard rules

1. **Never edit `.Loxone` files directly.** All config changes go through
   `lxir compile`. The XML's identity model (UUIDs, counters, per-port
   identifiers, three concurrent writers) makes manual edits corrupting.
2. **Never hand-edit the lockfile**, except through the documented
   operations (`remove_object`, `rename_object` — exposed via the library;
   see [lockfile-spec.md](lockfile-spec.md)). Commit the lockfile together
   with the module.
3. **Never invent block types or port names.** Valid block types for `block`
   are exactly the builtin table (`src/connectors.rs`: gates, the comparator
   family, `Formula`, `Monoflop`, `PulseGen`, `AnalogThresholdTrigger`).
   Valid ports on an extern are whatever the base config actually has —
   discover them with `lxir decompile` / `lxir observe`, or read them from
   the error, which lists them (and suggests the closest name for typos).
   `lxir check` catches wrong types/ports/directions on managed blocks
   without needing the base config.
4. **Do not upload.** Compiling produces a file; sending it to a Miniserver
   is a separate, human-gated step (`lox`/`lox-cli`, outside this crate).
   Treat everything here as read-only toward the house.
5. **Prefer `iname`/`uuid` matching over `title`** for externs — titles are
   locale-volatile and may change under you.
6. Treat `NoMatch` / `AmbiguousMatch` as **stop-and-ask**: choosing among
   candidates is a human decision unless instructions already cover it.

## The loop

```sh
# 0. Orientation: what does the config contain, what is already managed?
lxir decompile current.Loxone            # IR view + report (stderr)
lxir observe current.Loxone              # port evidence per block type

# 1. Edit the module (.lxir). Whole-line # comments are preserved.

# 2. Validate cheaply before compiling (no base config needed — includes
#    block types, port names, and wire directions on managed blocks):
lxir check modules/beschattung.lxir       # human-readable, line numbers
lxir check --json modules/beschattung.lxir # machine-readable diagnostics
lxir fmt --write modules/beschattung.lxir # canonicalize (non-destructive)

# 3. Compile against the current base:
lxir compile --base current.Loxone \
            --module modules/beschattung.lxir \
            --lock modules/beschattung.lock.json \
            --out out.Loxone
# --serial only needed the first time (recorded in the lock afterwards);
# --time only for reproducible builds (lock pins minted UUIDs regardless).

# 4. Show your work as a SEMANTIC diff, never an XML diff:
lxir diff current.Loxone out.Loxone

# 5. Sanity: the output must round-trip.
lxir roundtrip out.Loxone
```

Present step 4's output when proposing a change: it shows added blocks,
wires, and parameter changes in reviewable form, and flags locale-rename
noise (`[locale?]`) so it isn't mistaken for a real edit.

## The language in 30 seconds

See [ir-spec.md](ir-spec.md) for the full spec.

```text
# comment (all comments survive formatting)
let schwelle = 28                                # named constant

extern sonne: VirtualIn match iname "VI3"        # existing object, not yours
extern jal:   AutoJalousie match title "Beschattung Süd"

block temp_hoch: GreaterEqual "Temp über 28" {   # yours, compiler-owned
	Input2 = schwelle                            # Def= parameter (let or literal)
}
block beschatten: And                            # gates are fixed 2-input

wire temp_hoch.Q -> beschatten.I1                # output -> input
wire beschatten.Q -> jal.AutoShade
set jal.TargetPos = 70                           # extern ports only; original
                                                 # auto-restored when removed

removed old_gate                                 # confirmed delete of a managed block
moved beschatten -> schatten_gate                # slug rename, UUIDs survive
```

Removing a `block` line makes the next compile **fail on purpose**. When
deletion is the confirmed intent, add `removed <slug>` to the module — it
is scoped to that one block and shows the intent in the diff. For a slug
rename, rename the declaration and add `moved <old> -> <new>`; both
statements become no-ops once applied. (`--allow-removals` still exists but
authorizes *every* pending removal at once — prefer `removed`.)

## Errors and remedies

Many wrong-name errors end with a ``did you mean `…`?`` suggestion — trust
it only when it matches what you intended.

| Error contains | Meaning | Remedy |
|---|---|---|
| `line N` (from `check`) | syntax/reference error in the module | fix that line; `lxir fmt` shows canonical form |
| `no match for extern` | spec matched nothing of that type | check type + spelling; `lxir decompile` the base to see candidates |
| `ambiguous` + candidate UUIDs | several objects match | switch to `match uuid "<one of the candidates>"` — ask if unclear |
| `not in the verified builtin table` | block type can't be minted in v0 | use a verified type, or declare the object `extern` (create it once in Loxone Config) |
| `unknown port … known ports: …` | wrong port name on a managed block | use a listed port (caught by `check`, no base needed) |
| `has no port … present ports: …` | extern port's `<Co>` missing from base | use a listed port, or have the port wired/set once in Loxone Config so it exists |
| `in the lockfile but not in the source` | managed block vanished from source | typo → restore the line; delete → add `removed <slug>`; rename → `moved <old> -> <new>`; unmanage → `remove_object` |
| `changed type` | slug reused for a different block type | new slug, or `remove_object` first |
| `is an output port and cannot be used as …` | wire direction wrong | swap the endpoints |
| `targets managed block` (from `set`) | `set` used on a block you declared | move the assignment into the block's `{ … }` body |
| `undeclared constant` | bare identifier in value position | declare `let <name> = …`, or quote the value if a string was intended |
| `neither slug is in the lockfile` (from `moved`) | rename references unknown identity | check both spellings against the lock; a first compile needs no `moved` |

After any failed `compile`, the lockfile on disk is untouched (the CLI saves
only on success) — just fix and re-run.

## Invariants you can rely on

- Compilation is deterministic: unchanged inputs → byte-identical output
  and lockfile. If you see a diff, something actually changed.
- Recompiling the compiler's own output is a no-op (fixpoint) — safe to
  re-run freely.
- The compiler touches only: its managed blocks, wires it drew onto extern
  ports, `Def=` values it `set`. Everything else in the config is preserved
  byte-for-byte.
- `set` originals and extern wires are tracked in the lock, so deleting IR
  lines cleanly reverts their effects on the next compile.
