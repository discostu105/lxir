# IR language specification (v0)

The textual intermediate representation for managed Loxone logic. File
extension: **`.lxir`**. Encoding: UTF-8, no BOM. Line-oriented: one statement
per line; only a block's `{ … }` parameter body spans lines.

This document is normative for what `Module::parse` accepts and what
`Module::to_text` (= `lxir fmt`) emits.

## Grammar

```ebnf
module     = { line } ;
line       = comment | extern | block | wire | set
           | let | removed | moved | body-line | blank ;

comment    = "#" any-text ;                        (* whole line *)
extern     = "extern" slug ":" type "match" kind string [ comment ] ;
kind       = "uuid" | "iname" | "title" ;
block      = "block" slug ":" type [ string ] [ "{" ] [ comment ] ;
body-line  = param "=" value [ comment ] | comment | "}" [ comment ] ;
                                                   (* only inside an open block *)
wire       = "wire" slug "." port "->" slug "." port [ comment ] ;
set        = "set" slug "." port "=" value [ comment ] ;
let        = "let" slug "=" ( number | string ) [ comment ] ;
removed    = "removed" slug [ comment ] ;
moved      = "moved" slug "->" slug [ comment ] ;

slug       = lowercase-letter { lowercase-letter | digit | "_" } ;
type       = uppercase-letter { letter | digit } ;          (* PascalCase *)
port       = letter-or-digit-or-underscore-sequence ;       (* as in the XML `K=` key *)
param      = port ;
value      = number | string | const-ref ;
const-ref  = slug ;                                (* names a `let` constant *)
number     = [ "-" ] digits [ "." digits ] ;       (* exactly — `1.2.3`, `5.` are errors *)
string     = '"' { character | escape } '"' ;
escape     = '\"' | "\\" | "\n" ;
```

Notes:

- A `#` outside a string starts a comment that runs to end of line.
  All comments are preserved by the formatter: whole-line comments are AST
  items, trailing comments attach to their statement (or parameter line, or
  the closing `}`), and whole-line comments inside `{ … }` bodies are body
  items.
- The `{` opening a parameter body must be the last token of the `block`
  line (a trailing comment may follow it). The closing `}` stands on its
  own line (a trailing comment may follow it and stays there).
- Whitespace is insignificant except as a token separator. Indentation is
  conventional (the formatter uses one tab inside bodies).
- A bare identifier in value position is always a **constant reference**;
  it must name a `let` in the same module. String values are always quoted.

## Statements

### `extern` — reference an object owned by Loxone Config

```text
extern sonne: VirtualIn match iname "VI3"
extern jal_sued: AutoJalousie match title "Beschattung Süd"
extern boiler: Switch match uuid "1d844a67-0333-5301-ffffed57184a04d2"
```

Declares a slug for an existing object in the base config. The compiler
never creates, deletes, or moves externs; it only wires to their ports and
`set`s their parameters.

Match semantics: the object must have the declared `type` **and** match the
spec. Exactly one object may match — zero is a `NoMatch` error, several is
an `AmbiguousMatch` error listing candidates. Once resolved, the UUID is
pinned in the lockfile; subsequent compiles use the pin (even if the title
has changed since) as long as an object with that UUID and type still
exists.

Choosing a spec: `uuid` pins exactly; `iname` (the `IName=` attribute, e.g.
`VI1`, `AI3`) is locale-stable and preferred for built-in I/O objects;
`title` is human-friendly but locale-volatile — use it only for objects you
named yourself.

### `block` — declare a managed logic block

```text
block beschatten: And
block temp_hoch: GreaterEqual "Temp über 28" {
	Input2 = 28
}
```

The compiler owns this object end-to-end: it mints its UUIDs (pinned in the
lock), draws it on the target page, rebuilds it on every compile, and
deletes it when removal is explicitly allowed.

- The optional string is the display title; it defaults to the slug.
- The body assigns parameter defaults: each `Param = value` becomes `Def=`
  on the port with key `Param`. Values may reference `let` constants.
- The type must be in the verified builtin table
  (gates `And`/`Or`/`Not`, the comparator family `Equal`/`NotEqual`/
  `Greater`/`GreaterEqual`/`Less`/`LessEqual`, `Formula`, `Monoflop`,
  `PulseGen`, `AnalogThresholdTrigger` — see
  [connector-db.md](connector-db.md)) — anything else
  is a compile error. See [design.md](design.md) "Refuse, never guess".
- `And`/`Or` are fixed two-input blocks (`I1`, `I2`, `Q`): referencing
  `I3`+ is a compile error — Loxone Config silently deletes grown inputs
  on save (design decision D8). Need more inputs? Cascade gates:
  `a.Q -> c.I1`, `b.Q -> c.I2`.
- Types, port names, and wire directions on managed blocks are validated
  **statically** (`lxir check`, no base config needed); unknown names get a
  "did you mean" suggestion when a close candidate exists.

### `wire` — connect two ports

```text
wire sonne.Q -> beschatten.I2
wire beschatten.Q -> jal_sued.AutoShade
```

`from` must be an output, `to` an input (checked against the builtin table
for managed blocks; externs are open-world — the port must merely exist in
the base config). Wires whose sink is an extern port are recorded in the
lockfile so removing the statement removes the wire again without touching
GUI-drawn wires.

### `set` — write a parameter on an extern port

```text
set jal_sued.TargetPos = 70
```

Rewrites the extern port's `Def=`; the original value is recorded in the
lockfile and **restored** when the `set` disappears from source.

`set` is for **extern ports only**. A managed block's parameters belong in
its `{ … }` body — one spelling, one owner — so `set` on a managed slug is
a validation error pointing at the body.

### `let` — a named constant

```text
let temp_schwelle = 28

block temp_hoch: GreaterEqual {
	Input2 = temp_schwelle
}
```

Declares a name for a number or string. Any value position (block
parameters, `set`) may reference it by bare identifier; the compiler
substitutes the literal before emitting `Def=`, so a `let` reference
compiles byte-identically to writing the literal in place. Constants cannot
reference other constants, and constant names share the module's one slug
namespace (they cannot be wired or `set`).

### `removed` — authorize deleting a managed block

```text
removed temp_hoch
```

Declares that `temp_hoch`'s absence from source is intentional: the next
compile deletes the block from the config and drops it from the lockfile.
Scoped to exactly one slug and reviewable in the diff — prefer it over the
global `--allow-removals` flag. Once applied (the slug is no longer in the
lock), the statement is a no-op and can be deleted. Declaring a slug and
`removed`-ing it in the same module is an error.

### `moved` — rename a managed block, keeping identity

```text
moved beschatten -> schatten_gate
```

Renames the lockfile entry so the block's identity — object *and* port
UUIDs, layout — survives a slug rename in source: rename the `block` (and
its `wire`/reference sites), add the `moved` line, compile. Idempotent:
once the new slug carries the lock entry, the statement is done and can be
deleted. It is an error when neither slug is in the lockfile (typo guard),
when the old slug is still declared, or when moves are chained
(`a -> b`, `b -> c`).

## Name resolution and validation

- Externs, blocks, and `let` constants share one namespace per module;
  duplicates are an error.
- Every `wire`/`set` reference must name a declared extern or block; every
  bare-identifier value must name a declared `let`.
- `set` targets must be externs (see above).
- `removed`/`moved` must not contradict declarations or each other
  (details under the statements).
- Statement order is free; the conventional order is lets, externs, blocks,
  wires, sets, with lifecycle statements (`moved`, `removed`) last and a
  blank line between groups.

`lxir check` performs all of the above plus the static builtin-table checks
(block types, port names, wire directions) — everything that does not need
the base config. `--json` emits the result machine-readably.

## Canonical form

`lxir fmt` emits: statements in source order, single spaces between tokens,
one blank line whenever the item kind changes, tab-indented bodies. Values
keep their variant: numbers bare, strings quoted, constant references bare.
The one value canonicalization happens at parse time: a quoted string that
reads exactly as a number (`"28"`) becomes the bare number, so every value
has one canonical spelling. `parse(to_text(m)) == m`, and `to_text` is a
fixpoint.

## Errors

Parse errors carry a 1-based line number. Unknown types, ports, and
constants suggest the closest known name when one is within a small edit
distance. Compile errors are documented with remedies in
[agents.md](agents.md#errors-and-remedies).

## Versioning

This is v0. Anything not specified here (templates, expressions, imports,
multi-file modules, composite match qualifiers, unit-suffixed values) is
future work — see [roadmap.md](roadmap.md). Future versions will keep v0
files parsing unchanged or provide a migration tool.
