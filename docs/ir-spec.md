# IR language specification (v1)

The textual intermediate representation for managed Loxone logic. File
extension: **`.lxir`**. Encoding: UTF-8, no BOM. Line-oriented: one
statement per line; only a block declaration's `( … )` argument list spans
lines.

This document is normative for what `Module::parse` accepts and what
`Module::to_text` (= `lxir fmt`) emits.

v1 (2026-08-25) replaced v0's keyword statements (`block`/`wire`/`set`)
with constructor syntax — see design decision D16 in
[design.md](design.md). v0 keywords are reserved and produce migration
errors.

## Grammar

```ebnf
module     = { line } ;
line       = comment | let | extern | block-head | arg-line
           | wire | set | removed | moved | blank ;

comment    = "#" any-text ;                        (* whole line *)
let        = "let" slug "=" ( number | string ) [ comment ] ;
extern     = "extern" slug "=" type "(" matcher ":" string ")" [ comment ] ;
matcher    = "uuid" | "iname" | "title" ;

block-head = slug "=" type "(" [ args ] [ ")" ] [ comment ] ;
arg-line   = args [ ")" ] [ comment ] | comment ;  (* only inside an open call *)
args       = arg { "," arg } [ "," ] ;
arg        = string                                (* label; first argument only *)
           | port ":" binding ;
binding    = value                                 (* Def= parameter *)
           | slug "." port ;                       (* wire from that source *)

wire       = slug "." port "<-" slug "." port [ comment ] ;   (* extern target *)
set        = slug "." port "=" value [ comment ] ;            (* extern target *)
removed    = "removed" slug [ comment ] ;
moved      = "moved" slug "->" slug [ comment ] ;

slug       = lowercase-letter { lowercase-letter | digit | "_" } ;
type       = uppercase-letter { letter | digit } ;          (* PascalCase *)
port       = letter-or-digit-or-underscore-sequence ;       (* as in the XML `K=` key *)
value      = number | string | const-ref ;
const-ref  = slug ;                                (* names a `let` constant *)
number     = [ "-" ] digits [ "." digits ] ;       (* exactly — `1.2.3`, `5.` are errors *)
string     = '"' { character | escape } '"' ;
escape     = '\"' | "\\" | "\n" ;
```

Notes:

- A `#` outside a string starts a comment that runs to end of line. All
  comments are preserved by the formatter: whole-line comments are AST
  items, trailing comments attach to their statement (or argument line, or
  the closing `)`), and whole-line comments inside argument lists are
  argument items.
- An argument list may close on the declaration line (a single-line call)
  or span lines; nothing may follow the closing `)` except a comment.
- Whitespace is insignificant except as a token separator. Indentation is
  conventional (the formatter uses one tab inside argument lists).
- A bare identifier in value position is always a **constant reference**;
  it must name a `let` in the same module. A dotted identifier
  (`slug.Port`) is always a **port reference**. String values are always
  quoted.
- The keywords `let`, `extern`, `removed`, `moved` are reserved, as are
  v0's `block`, `wire`, and `set` (migration errors) — none can be used as
  a slug in statement position.

## Statements

### `let` — a named constant

```text
let temp_schwelle = 28
```

Declares a name for a number or string. Any value position (argument
lists, extern port assignments) may reference it by bare identifier; the
compiler substitutes the literal before emitting `Def=`, so a `let`
reference compiles byte-identically to writing the literal in place.
Constants cannot reference other constants, and constant names share the
module's one slug namespace (they have no ports and cannot be wired).

### `extern` — reference an object owned by Loxone Config

```text
extern sonne = VirtualIn(iname: "VI3")
extern jal_sued = AutoJalousie(title: "Beschattung Süd")
extern boiler = Switch(uuid: "1d844a67-0333-5301-ffffed57184a04d2")
```

Declares a slug for an existing object in the base config. The compiler
never creates, deletes, or moves externs; it only wires to their ports
(`<-`) and assigns their parameters (`=`).

Match semantics: the object must have the declared type **and** match the
spec. Exactly one object may match — zero is a `NoMatch` error, several is
an `AmbiguousMatch` error listing candidates. Once resolved, the UUID is
pinned in the lockfile; subsequent compiles use the pin (even if the title
has changed since) as long as an object with that UUID and type still
exists.

Choosing a matcher: `uuid` pins exactly; `iname` (the `IName=` attribute,
e.g. `VI1`, `AI3`) is locale-stable and preferred for built-in I/O
objects; `title` is human-friendly but locale-volatile — use it only for
objects you named yourself.

**Time functions and other singletons.** The periphery's time sources
(the GUI's *Zeitfunktionen* folder: `DayOfWeek`, `Time`, `Hour`,
`ImpulseMinute`, `ImpulseSunrise`, `StartPulse`, …) and the operating
modes (*Betriebsmodi*, type `Mode`) exist exactly once per project and
carry **no `IName=`**, so the matcher choice narrows: their titles are
locale-volatile like all built-ins (a save can rename `Wochentag` on
you), which leaves `uuid` as the durable pin:

```text
# Zeitfunktionen: singletons, no IName — pin by UUID
extern wochentag = DayOfWeek(uuid: "15ea0aa4-0093-39e4-ffffed57184a04d2")
extern minutenimpuls = ImpulseMinute(uuid: "15ea0aa4-01b6-3a20-ffffed57184a04d2")
extern montag = Mode(uuid: "00000000-0000-0004-1500000000000000")
```

Wiring them follows the normal rules — the singleton's output feeds a
managed block's input directly, no `InputRef` needed even across pages
(oracle-verified for `Mode` wires, sessions 6–7; the time sources are
the same class of periphery singleton). `title:` still *works* for a singleton
while the locale holds, and `adopt` lifts it that way; switching the
lifted spec to `uuid:` is a plain source edit (the lock pins the same
object either way). `Mode` UUIDs are deterministic system UUIDs
(`00000000-…`), identical across projects; time-function UUIDs are
minted per project — read them off `lxir decompile` or the XML.

### Block declaration — a managed logic block

```text
beschatten = And()
temp_hoch = GreaterEqual(
	"Temp über 28",
	Input1: aussentemp.Q,
	Input2: temp_schwelle,
)
```

The compiler owns this object end-to-end: it mints its UUIDs (pinned in
the lock), draws it on the target page, rebuilds it on every compile, and
deletes it when removal is explicitly authorized.

The argument list is the block's entire input situation in one place. The
value decides the meaning:

- `Port: 28`, `Port: "text"`, `Port: constant` — a **parameter**: the
  literal (or resolved `let` constant) becomes `Def=` on the port with key
  `Port`.
- `Port: slug.Port` — a **wire**: the referenced source port is wired into
  this port. Sources may be extern or managed ports (including the block
  itself — feedback is representable) and are referenced before or after
  their declaration.
- An optional leading string is the display **label** (the XML `Title`);
  it defaults to the slug.

Some block types carry **attribute parameters** — block logic stored as an
element attribute rather than a connector. They bind exactly like
parameters but can never take a wire:

```text
summe = Formula("Summe", Formula: "I1+I2", Input1: a.Q, Input2: b.Q)
```

`Formula:` on `Formula` blocks is the only one so far (declared per type
in `connectors::attr_params`); it compiles to `Formula="I1+I2"` on the
`<C>` element, together with the observed `Valid="false"` companion.

Rules:

- At most one parameter binding per port; multiple wire bindings on the
  same port are allowed when the sources differ (fan-in).
- The type must be in the verified builtin table
  (gates `And`/`Or`/`Not`, the comparator family `Equal`/`NotEqual`/
  `Greater`/`GreaterEqual`/`Less`/`LessEqual`, `Formula`, `Monoflop`,
  `PulseGen`, `AnalogThresholdTrigger` — see
  [connector-db.md](connector-db.md)) — anything else is an error. See
  [design.md](design.md) "Refuse, never guess".
- `And`/`Or` are fixed two-input blocks (`I1`, `I2`, `Q`): binding `I3`+
  is an error — Loxone Config silently deletes grown inputs on save
  (design decision D8). Need more inputs? Cascade gates:
  `c = And(I1: a.Q, I2: b.Q)`.
- Types, port names, and wire directions on managed blocks are validated
  **statically** (`lxir check`, no base config needed); unknown names get
  a "did you mean" suggestion when a close candidate exists.

### `<-` — wire onto an extern port

```text
jal_sued.AutoShade <- beschatten.Q
jal_sued.Safety <- wind_alarm.Q
```

Wires a source port onto an extern port. The source must be an output; the
extern port is open-world — it must merely exist in the base config. These
wires are recorded in the lockfile so removing the statement removes the
wire again without touching GUI-drawn wires.

`<-` is for **extern targets only**: a wire into a managed block is
written in that block's argument list — one canonical spelling per fact —
so `<-` on a managed slug is a validation error pointing at the argument
list.

### `=` on a port — write a parameter on an extern port

```text
jal_sued.TargetPos = 70
```

Rewrites the extern port's `Def=`; the original value is recorded in the
lockfile and **restored** when the statement disappears from source.

Like `<-`, this is for **extern targets only**. A managed block's
parameters belong in its argument list — assigning a managed port is a
validation error pointing there. Assigning a port reference
(`x.Port = y.Q`) is a parse error suggesting `<-`.

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
UUIDs, layout — survives a slug rename in source: rename the declaration
(and its reference sites), add the `moved` line, compile. Idempotent: once
the new slug carries the lock entry, the statement is done and can be
deleted. It is an error when neither slug is in the lockfile (typo guard),
when the old slug is still declared, or when moves are chained
(`a -> b`, `b -> c`).

## Name resolution and validation

- Externs, blocks, and `let` constants share one namespace per module;
  duplicates are an error.
- Every port reference (argument bindings, `<-`, `=`) must name a declared
  extern or block; every bare-identifier value must name a declared `let`.
- `<-` and port-`=` targets must be externs (see above).
- Duplicate parameter bindings, and duplicate identical wire bindings, in
  one argument list are errors.
- `removed`/`moved` must not contradict declarations or each other
  (details under the statements).
- Statement order is free; the conventional order is lets, externs,
  blocks, extern wires, extern assignments, with lifecycle statements
  (`moved`, `removed`) last.

`lxir check` performs all of the above plus the static builtin-table
checks (block types, port names, wire directions) — everything that does
not need the base config. `--json` emits the result machine-readably.

## Canonical form

`lxir fmt` emits: statements in source order, single spaces between
tokens, one blank line whenever the item kind changes and after every
multi-line block declaration. An argument-free call stays on one line
(`b = And()`, `c = Or("Oder")`); a call with bindings puts the label and
each argument on its own tab-indented line with a trailing comma, and the
`)` on its own line. Values keep their variant: numbers bare, strings
quoted, constant references bare, port references dotted. The one value
canonicalization happens at parse time: a quoted string that reads exactly
as a number (`"28"`) becomes the bare number, so every value has one
canonical spelling. `parse(to_text(m)) == m`, and `to_text` is a fixpoint.

## Errors

Parse errors carry a 1-based line number. Unknown types, ports, and
constants suggest the closest known name when one is within a small edit
distance. v0 statements (`block`, `wire`, `set`, `extern … match …`) get
migration errors describing the v1 spelling. Compile errors are documented
with remedies in [agents.md](agents.md#errors-and-remedies).

## Versioning

This is v1; there are no v0 files in the wild (pre-release revision).
Anything not specified here (templates, expressions, imports, multi-file
modules, composite match qualifiers, unit-suffixed values) is future work —
see [roadmap.md](roadmap.md). Future versions will keep v1 files parsing
unchanged or provide a migration tool.
