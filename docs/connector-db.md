# Connector database consolidation

The roadmap's Stufe −1: the two pre-existing connector databases contradict
each other, so lxir builds its own from **evidence** — every real config is
a test case. This doc records the methodology, the first consolidation run,
and the admission rules that gate `connectors::builtin` (the table of
mintable types).

## The three knowledge sources

| Source | Shape | Trust |
|---|---|---|
| **Corpus evidence** (`lxir observe`) | per type/key: connector index from the port UUID, wired-as-sink / wired-as-source / `Def=` counts | ground truth for whatever it covers |
| lox-cli `docs/schemas/connector-map.json` (195 types) | `type → {c: [order], t: {key: I/O/P}}` | good ordering info; several corrupt entries (see findings) |
| lox-sim `block_signature` (237 types, extracted via [`tools/extract-sim-signature.py`](../tools/extract-sim-signature.py)) | (inputs, outputs, params) per type | best direction coverage; some simulator-internal key names diverge from XML keys; no index order |

## Reproducing the run

```sh
tools/extract-sim-signature.py ~/repos/my/lox-cli > sim-signature.json
lxir observe corpus/*.Loxone \
    --crosscheck ~/repos/my/lox-cli/docs/schemas/connector-map.json \
    --crosscheck sim-signature.json > corpus-report.json
```

The merged evidence (no identity data — types, keys, indexes, counts only)
is committed as [`data/connectors-observed.json`](data/connectors-observed.json)
and regenerated whenever the corpus grows.

## The 2026-08-24 run (6 real configs, 102 types observed)

- **connector-map.json**: 33 types comparable; after normalizing its `P`
  (param) to input-like, 3 direction conflicts remain — two of them corrupt
  legacy entries (`InputRef`/`OutputRef` store the *key name* as the
  direction), one real disagreement (below).
- **sim-signature**: 61 types comparable, exactly 1 direction conflict.
- **`Intercom.API`**: both legacy dbs call it an output; the corpus has
  wires *into* it. The corpus wins — legacy-db bug (or a mixed-direction
  API connector; either way not trustworthy).
- **Both legacy dbs are incomplete.** e.g. `Monoflop` materializes five
  connectors in every real instance (`InputTrigger`@0, `Reset`@1,
  `Remanence`@2, `Time`@3, `Q`@4); both dbs list only three
  (`InputTrigger`, `Q`, `Time`). Loxone Config emits a `<Co>` for *every*
  connector (design D4), so corpus instances reveal the complete table
  including index order — which no legacy db has at all.
- **`OutputAPI` is genuinely mixed** across types (20 sinks, 50 sources in
  the corpus) — it must be resolved per type, never by name.

## Admission rules for `connectors::builtin`

A type becomes mintable only if **all** hold:

1. **Complete, stable shape**: every instance in the corpus materializes
   the same key set, with identical connector indexes, contiguous from 0.
   (≥3 instances. There is no variadic growth: Loxone Config deletes
   off-descriptor connectors on save — design decision D8.)
2. **Every port's direction resolved**, in order of preference:
   - corpus wire evidence (sink→Input, source→Output, `Def`-only→Param);
   - agreement of a legacy db, with zero corpus contradiction;
   - the **inert-flag rule**: a key that is never wired and never carries
     `Def` *anywhere in the corpus* (e.g. `Remanence`: 201 occurrences over
     11 types, zero evidence in any direction) is classified `Input`. This
     is negative evidence, not a guess-by-name: wiring *into* such a port
     stays possible, wiring *from* it is refused, and one observed
     counterexample evicts the classification.
3. **No unresolved key remains.** One undetermined port keeps the whole
   type out (that is what blocks `PulseAt` and `DayTimer` today).

A fourth path can resolve what the corpus cannot: the **save oracle**
([oracle-wine.md](oracle-wine.md)). Loxone Config deletes anything
off-descriptor on save (D8), so a minted minimal instance surviving an
open+save proves the shape, and a compiled wire sourced from a port
surviving the save proves that port is an output.

Hardware/IO reference types (sensors, actors, `InputRef`/`OutputRef`,
Tree/Air/Modbus devices) are **never admitted** regardless of evidence —
they carry device bindings; lxir references hardware, it does not own it
(see [vision.md](vision.md)).

## Admitted in the first batch

| Type | Ports (index order) | Basis |
|---|---|---|
| `Formula` | Input1‑4 → AQ, TQ | 39 corpus instances; TQ direction from lox-sim agreement |
| `Monoflop` | InputTrigger, Reset, Remanence, Time(P), Q | 20 instances; Reset sink-evidenced; Remanence inert-flag |
| `PulseGen` | InputEnable, InputInvert, Remanence, TimeHigh(P), TimeLow(P), Q | 5 instances; InputInvert agreed by both dbs |
| `AnalogThresholdTrigger` | Input, Remanence, On(P), Off(P), PulseTime(P), Q, RisingEdge, FallingEdge | 4 instances; edge outputs from lox-sim agreement |
| `NotEqual`, `Greater`, `Less`, `LessEqual` | Input1, Input2, Q | comparator family: both dbs agree on the shape shared with live-verified `Equal` |

Kept out of the first batch, with the blocking key: `Memory` (`Q` —
absent from both dbs, no corpus wires), `PulseAt` / `PushButton` /
`DayTimer` (`OutputAPI`, plus `RtD`/`AQm`/`AQmt` on DayTimer).

## Admitted 2026-08-25 via the mint oracle

Minted minimal instances of three of the blocked types survived a Loxone
Config open+save on the Xvfb rig (evidence and healing details in
[oracle-wine.md](oracle-wine.md)); `Memory.Q` was proven an output by a
compiled wire sourced from it surviving the save.

| Type | Ports (index order) | Basis |
|---|---|---|
| `Memory` | Input, AQ, Q | 7 corpus instances; `Q` direction from the oracle probe |
| `PushButton` | InputTrigger, On, Reset, InputDisable, Remanence, Q, Qon, Qoff, OutputAPI | 5 instances; whole shape oracle-survived, `OutputAPI` resolved by the inert-flag rule for this type |
| `PButtonT` | InputTrigger, Reset, InputDisable, Remanence, Time(P), Q, OutputAPI | 8 instances; same basis as PushButton |

The GUI heals these types with visualization children (`IoData`,
`Display`, `PSD`) and display attributes (`Tp=`, `SpStates=`) the
compiler never authors. Since design decision D19 that content is
**GUI-owned residue**: rebuilds carry it forward verbatim from the base
and adoption accepts it, so GUI-created instances adopt cleanly (the
only remaining refusals in the real config are genuine `Inv=` input
inversions).

Still kept out: `PulseAt` and `DayTimer` (no oracle run yet; DayTimer
additionally has `RtD`/`AQm`/`AQmt` unresolved).

## AutoJalousie, admitted 2026-08-25

The strongest evidence base yet: 78 corpus occurrences, 16 of them the
house's Config-17 instances with an **identical 49-key element order**.
The corpus-wide `index_conflict`s are older-generation configs plus
connectors added later by schema migration (their UUIDs, minted on the
house Miniserver, reuse index bytes — `Rdd`, `TargetPos`) — the element
order, not the UUID index byte, is the canonical order.

Directions: `EndUp`/`EndDown` corpus-sink ×70; 18 params carry `Def=` on
every instance with both legacy dbs agreeing; the 9 `Output*`/`TargetPos`
outputs agree across both legacy dbs with zero corpus contradiction; the
19 never-touched keys classify Input by the inert-flag rule.

`OutputAPI` forced a model extension: the house wires **into** it on 4
instances and **from** it on 12 (corpus: 12 sink / 36 source) — the API
connector is genuinely bidirectional. It is classified `PortDir::Api`:
valid as wire source *and* sink, never a `Def=` target. (PushButton/
PButtonT keep `OutputAPI` as Output — zero sink evidence there; one
observed counterexample flips them to Api.)

Known limitation: 4 house instances carry `FLG="1"` on their incoming
API wires (Miniserver/app-created wire metadata of unknown meaning) —
the rebuild cannot reproduce the flag, so those instances refuse
adoption. Resolving what `FLG` means is a future oracle experiment.

## Growing the table

Add configs to the corpus (any installation helps — foreign corpora
especially, since one house exercises few types), re-run the command above,
and re-apply the rules. The final gate stays [implementation.md's
workflow](implementation.md): a new entry should eventually be confirmed by
compiling it and passing the config through Loxone Config (the Stufe 0
oracle).
