# IR language specification (v0)

The textual intermediate representation for managed Loxone logic. File
extension: **`.lxir`**. Encoding: UTF-8, no BOM. Line-oriented: one statement
per line; only a block's `{ … }` parameter body spans lines.

This document is normative for what `Module::parse` accepts and what
`Module::to_text` (= `lxir fmt`) emits.

## Grammar

```ebnf
module     = { line } ;
line       = comment | extern | block | wire | set | body-line | blank ;

comment    = "#" any-text ;                        (* whole line *)
extern     = "extern" slug ":" type "match" kind string ;
kind       = "uuid" | "iname" | "title" ;
block      = "block" slug ":" type [ string ] [ "{" ] ;
body-line  = param "=" value | "}" ;               (* only inside an open block *)
wire       = "wire" slug "." port "->" slug "." port ;
set        = "set" slug "." port "=" value ;

slug       = lowercase-letter { lowercase-letter | digit | "_" } ;
type       = uppercase-letter { letter | digit } ;          (* PascalCase *)
port       = letter-or-digit-or-underscore-sequence ;       (* as in the XML `K=` key *)
param      = port ;
value      = number | string | bare-ident ;
number     = [ "-" ] digits [ "." digits ] ;
string     = '"' { character | escape } '"' ;
escape     = '\"' | "\\" | "\n" ;
```

Notes:

- A `#` outside a string starts a comment that runs to end of line.
  **Whole-line** comments are preserved by the formatter (they are AST
  items); trailing comments after a statement, and comments inside `{ … }`
  bodies, parse but are **not** preserved.
- The `{` opening a parameter body must be the last token of the `block`
  line. The closing `}` stands on its own line.
- Whitespace is insignificant except as a token separator. Indentation is
  conventional (the formatter uses one tab inside bodies).

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
  on the port with key `Param`.
- The type must be in the verified builtin table
  (gates `And`/`Or`/`Not`, the comparator family `Equal`/`NotEqual`/
  `Greater`/`GreaterEqual`/`Less`/`LessEqual`, `Formula`, `Monoflop`,
  `PulseGen`, `AnalogThresholdTrigger` — see
  [connector-db.md](connector-db.md)) — anything else
  is a compile error. See [design.md](design.md) "Refuse, never guess".
- `And`/`Or` are variadic: referencing `I3`, `I4`, … anywhere in the module
  grows the gate.

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

### `set` — write a parameter on a port

```text
set jal_sued.TargetPos = 70
```

On a managed block this is equivalent to a body parameter (and overrides one
with the same key). On an extern it rewrites the port's `Def=`; the original
value is recorded in the lockfile and **restored** when the `set` disappears
from source.

## Name resolution and validation

- Slugs share one namespace per module; duplicates are an error.
- Every `wire`/`set` reference must name a declared slug.
- Statement order is free; the conventional (and formatter-emitted) order is
  externs, blocks, wires, sets, with a blank line between groups.

## Canonical form

`lxir fmt` emits: statements in source order, single spaces between tokens,
one blank line whenever the item kind changes, tab-indented bodies, values
as bare tokens when they read as numbers and quoted strings otherwise.
`parse(to_text(m)) == m`, and `to_text` is a fixpoint.

## Errors

Parse errors carry a 1-based line number. Compile errors are documented with
remedies in [agents.md](agents.md#errors-and-remedies).

## Versioning

This is v0. Anything not specified here (templates, expressions, imports,
multi-file modules) is future work — see [roadmap.md](roadmap.md). Future
versions will keep v0 files parsing unchanged or provide a migration tool.
