# The `.Loxone` format — validated field notes

Everything in this document was established by examining real Miniserver
configs (six generations, Aug 2024 – Aug 2026, 117 KB – 1.34 MB, ~1900
objects) and a live Loxone Config / Miniserver installation (FW 17.x).
Facts marked **assumption** have no direct evidence yet.

There is no official specification; this is reverse-engineered knowledge and
the reason the crate's parser is hand-rolled.

## Container

- UTF-8 with **BOM**, `<?xml version="1.0" encoding="utf-8"?>` declaration,
  **CRLF** line endings, **tab** indentation, single space between
  attributes, no space before `/>`. Empty elements are *usually*
  self-closing, but not always: the GUI writes `<IoData></IoData>` even on
  freshly created blocks (oracle save 2026-08-25) — preserve the form per
  element, never normalize.
- Root: `<ControlList Version="…" NextObj="…" NextConst="…" NextNote="…"
  NextMem="…">`. The `Next*` counters are bumped by Loxone Config on save
  and must never decrease. `NextObj` correlates with auto-titles (`O1415`).
- Inside: one `<C Type="Document">` (carrying `ConfigVersion=` among many
  attributes), containing caption/folder `<C>` elements, `<C Type="Page">`
  drawing pages, and the object tree.

## Not actually XML

Two deviations break conforming parsers:

1. **Attribute names may start with digits**: `12hTF="true"`.
2. **Attribute values may contain raw, unescaped newlines** — the `Code=`
   attribute of a `Program` block holds multi-line PicoC source. A standard
   parser normalizes these to spaces on re-serialization, corrupting the
   program.

Entities used: `&amp; &lt; &gt; &quot; &apos;` plus numeric character
references. Text content appears only in `<Key>hex</Key>`-style elements and
is rendered inline (`<Key>2B35</Key>`).

## Objects: `<C>`

```xml
<C Type="Or" V="175" U="1e96b762-0130-074c-ffffed57184a04d2" Title="O1415"
   Px="5952" Py="11136" Px2="7296" Py2="11832"
   Cl="141,255,112" Nio="3" WF="147456">
```

- `Type` — block type (`Or`, `AutoJalousie`, `VirtualIn`, …).
- `V` — format/version field, `"175"` on current blocks.
- `U` — the object UUID (see below).
- `Title` — display name. **Locale-volatile** for built-ins: saving the
  config in a differently-localized Loxone Config renamed 111 built-in
  objects (Modes, weather fields, caption folders) in one observed save.
  Never use titles as identity.
- `IName` — internal name for I/O objects (`VI1`, `AI3`); locale-stable.
- `Px/Py/Px2/Py2` — drawing rectangle. Grid unit 96; logic blocks are 1344
  wide and `504 + 192·(ports−2)` tall (Or with 3 ports: 696; Not with 2:
  504).
- `Cl` — RGB display color (logic green: `141,255,112`).
- `Nio` — connector count. Matches the number of `<Co>` children: Loxone
  emits **all** connectors, including unwired outputs and inputs without
  values.
- `WF` — view flags. Freely recomputed by the GUI: oracle saves rewrote
  it in *both* directions (`147456` ↔ `16384`, plus other values) and
  drop it entirely on some types — pure view-state, never identity.
- Complex blocks carry extra attributes (`LtE=`, `SpStates=`, `rec=`,
  UUID-list attributes…). All of it round-trips untouched through the
  lossless tree. Some of it is **block logic stored outside connectors**:
  `PulseAt` keeps its fire time in `Sec=` (seconds since midnight, with
  `Typ=`/`AutP=` mode flags); `DayTimer` keeps its schedule as `<Entry>`
  child elements after the `<Co>` list (`N=` holds their count) with
  `Analog=`/`DefValue=`/`On=`/`Off=` output behavior and `Modes=`/
  `UserModes=` operating-mode gating.
- Element order of `<Co>` children is the **canonical connector order**;
  the UUID index byte is historical. Proof: the V259 schema migration
  moved DayTimer's `PulseTime` from element position 6 to 3 while its
  port UUID kept index byte `06`.

## Connectors and wires: `<Co>` / `<In>`

```xml
<Co K="I1" Nc="1" U="1e96b762-0130-0749-00ff69723a2bac9e">
	<In Input="1e96b5a8-02ec-bf33-02ffcb4248672396"/>
</Co>
<Co K="I2" Def="1" U="…-01ff…"/>
<Co K="Q" U="…-02ff…"/>
```

- `K` — port key (the name the IR uses).
- `U` — the **port's own UUID**; wires reference port UUIDs, not object
  UUIDs.
- Wires are stored at the **sink**: each `<In Input="…"/>` names the source
  port's UUID. An output being wired leaves no trace on the output itself.
- A wire source needs **no on-page representation**: the system `Mode`
  objects (operating modes; no `Px/Py`, deterministic UUIDs) source wires
  directly into page blocks. The GUI's own pattern inserts an `InputRef`
  when a mode is dragged onto a page (nine corpus configs), but a save
  accepts and preserves the direct wire without creating one — verified
  by the sixth oracle run ([oracle-wine.md](oracle-wine.md)).
- `Nc` — number of incoming wires (= count of `<In>` children); omitted
  when zero.
- `Def` — the port's parameter value; omitted at type default.
- Attribute order: `K, Def, Nc, U` (an oracle save 2026-08-25 rewrote a
  compiler-emitted `K, Nc, Def, U` into this order).
- An `<In>` may carry `FLG="1"` (rarely `"2"`) — Miniserver/app-created
  wire metadata, concentrated on API-connector and central-alarm wires.
  An oracle probe (2026-08-25) showed Loxone Config treats it as inert
  stored state: round-tripped verbatim, never regenerated, and a
  stripped flag is accepted without repair or wire loss
  ([oracle-wine.md](oracle-wine.md)).
- A `<Co>` may carry `Inv="true"` — the GUI's input inversion. Its
  dominant real-world use is not inverting wires: **unwired**, it turns
  the constant-0 input into a constant 1, and that is how the GUI
  encodes boolean checkbox settings like an enabled Remanenz (every one
  of the house's 23 `LightController2`s, its `PushButton`s and
  `DayTimer`s carry `Inv="true"` on unwired `Remanence`). Wired, it
  negates the incoming signal. Either way the connector is GUI-owned in
  lxir (design decision D20): carried verbatim, refused as a source-
  declared wire/value target.

## Rooms and categories: `<IoData Cr=… Pr=…>`

Rooms are `<C Type="Place">` elements under the `PlaceCaption` folder,
categories `<C Type="Category">` under `CategoryCaption` — plain
objects with user-given titles. A block's room/category assignment
lives in its `<IoData>` child: `Pr=` holds the Place UUID, `Cr=` the
Category UUID. Validated corpus-wide (2026-08-25): ~36 900 `Cr`/`Pr`
occurrences across the house config and all 132 web configs sit on
`IoData` and resolve to a Place/Category element (the only exceptions:
one hand-written non-Config file carrying them on `<C>`, and dangling
references inside template/fragment exports — so a resolver must treat
an unresolvable `Pr`/`Cr` as "no room", not an error). `IoData` is D19
GUI-owned residue: lxir never writes the assignment, only reads it for
composite extern matching (`room:` / `category:` in
[ir-spec.md](ir-spec.md)).

## Time functions: per-project singletons

The GUI's *Zeitfunktionen* periphery folder is a `TimeCaption` element
holding one instance each of the time-source types: `Day2009`, `Year`,
`Month`, `Day`, `DayOfWeek`, `Calendar`, `Time` (minutes since
midnight), `Hour`, `Minute`, `Second`, `SecondsBoot`, `DateTime`,
`NightTime`, the impulse family (`ImpulseSecond` … `ImpulseYear`,
`ImpulseSunrise`, `ImpulseSunset`, `ImpulseMorningtwilight`,
`ImpulseEveningtwilight`), and `StartPulse`. Each type appears exactly
once per project — the type *is* the identity. Unlike `SysVar`,
`WeatherData`, or I/O objects they carry **no `IName=`**, and their
titles are locale-volatile like every built-in's; their UUIDs are
minted at project creation by the Config PC (machine id in segment 4),
so they differ between projects. The operating modes (`Mode` under
`ModeCaption`) share the singleton nature but use deterministic
`00000000-…` system UUIDs, identical across projects.

## UUID anatomy

Format `{8}-{4}-{4}-{16}` hex — the last segment is 8 bytes, so these are
**not** RFC-4122 UUIDs.

| Segment | Meaning |
|---|---|
| 1 (u32) | creation time, seconds since **2009-01-01 UTC** (`1230768000`) |
| 2+3 (u16,u16) | mint-sequence counters |
| 4 (8 bytes) | identity tail, see below |

Tail shapes:

- `ff ff` + 6 bytes — an **object**. The 6 bytes identify the minting
  machine: a Config-PC id (`ed57184a04d2` observed) for GUI-created objects,
  the **Miniserver serial** (`504f94a26236`) for objects the Miniserver
  created itself (app-defined autopilot rules, device registrations). Block
  *states* share the `ffff` shape with a per-object random suffix.
- `<index> ff` + 6 bytes — a **connector**. Byte 0 is the connector's index
  within its block (`I1`→`00`, `I2`→`01`, `Q`→`02` on a 2-input gate);
  the remaining 6 bytes are a per-block entity shared by that block's
  connectors.
- anything else — reserved/system space. Built-ins like operating Modes
  have fully deterministic UUIDs (`00000000-0000-0001-1500000000000000`).

Mint order: Loxone Config mints a block's **ports first, then the object**
(consecutive counters `…0749, 074a, 074b` ports, `…074c` object). The
crate's minter mimics this.

Late-added connectors keep sequential indexes: a LightController2 whose
`I5`–`I8` were added years later (different time segment, different entity
bytes) still carries indexes `04`–`07` — those came from a schema
migration extending the type's descriptor, not from user action.

**Gates do not grow.** `And`/`Or` in Loxone Config 17 are fixed two-input
blocks: the GUI offers no way to add an input (wiring the last free input
does not create one; the wire-drop connector picker enumerates only
existing inputs), the corpus contains zero gates with more than two
inputs, and — verified via the Wine oracle — a hand-grown `I3 <Co>`
survives *loading* but is **silently deleted on save, together with its
wires**. Generalized: a save writes exactly the type descriptor's
connector set; off-descriptor `<Co>`s are dropped without warning. Any
tool inventing connectors will silently lose logic on the next GUI save.

## The three writers

1. **Loxone Config** — the GUI; rewrites the whole file on save, including
   locale-dependent built-in titles. The full save fingerprint (counter
   burn, `<In>` reordering, schema migrations, auto-layout) is measured in
   [oracle-wine.md](oracle-wine.md).
2. **The Miniserver** — creates objects at runtime (autopilots from the app,
   device registrations), minting UUIDs with its serial in the tail.
3. **This compiler** — see the ownership model in [design.md](design.md).

Any tooling that assumes it is the only writer, or that titles are stable,
breaks against 1 and 2.

## Related, out of crate scope

`.LoxCC` is the compressed container used for transport (magic
`0xaabbccee`); `lox config compress`/`extract` round-trips it byte-
identically. Handling lives in `lox`/`lox-cli`.
