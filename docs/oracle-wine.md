# The save oracle: Loxone Config under Wine

Loxone Config saves projects offline, without a Miniserver. Running it
under Wine on Linux turns it into a scriptable **oracle**: open a
compiled config, save it, and diff — every divergence is either a bug in
the compiler or a new fact about the format. First run: 2026-08-24,
Loxone Config 17.1 (`ConfigVersion 17010727`) under Wine on CachyOS/KDE
Wayland.

## Headline result

**Loxone Config accepts lxir-compiled output.** A real 1.3 MB config with
a compiled-in managed island (`Monoflop` + `And` + one wire, `Def=`
param, placed on an existing page) opened without complaint and survived
open → save with an **empty semantic diff**: every minted object UUID,
port UUID, wire, and param came back untouched. Loxone Config's fresh
output also round-trips **byte-identically** through the `xml` layer.

What it adjusted on minted blocks (cosmetic only):

- `Py2` corrected — the builtin height table used for layout is taller
  than Loxone's real block heights (Monoflop: ours 1080, Loxone's 696).
- `Py` grid-snapped to a multiple of 96 where placement was off-grid.
- `WF` (view-flags) rewritten (`147456` → `16384`) — also done to
  pre-existing corpus blocks, so it is a general normalization, not a
  reaction to minted objects.

## The save fingerprint

What a Loxone Config save changes in a file it considers unmodified
(open → save with no edits; confirmed on two independent runs):

| Change | Detail |
|---|---|
| `NextObj` **+2** | Consumed per open+save cycle with zero objects added — counter values are burned on transients, not only on persisted objects. `Lockfile::absorb_counters`' max() strategy handles this; "one per minted object" (D11) describes *our* minting, not Loxone's consumption. |
| `Date` / `DateS` | Document header stamped with the save wall-clock time. |
| `<In>` order rewritten | Wire lists under a `<Co>` are re-emitted in an internal canonical order — **not** the input file's order (observed: exact reversals vs. an older save). Deterministic: two saves of the same state produce the same order. `<In>` order is therefore **not semantically meaningful**; `diff` already treats wires as sets. Byte-stability of wire order across an external Loxone Config save is impossible by design. |
| Schema migrations | Older objects gain missing attributes (observed: `SwitchingTimer M="3"` gained `UserModes="3"`). This alone dirties the document — a freshly opened file already has unsaved changes. |
| `AutopilotRule` re-layout | Autopilot rule blocks get their `Px`/`Py` rewritten with no user input — their canvas positions are not stable across saves. |
| `Nc` dropped at zero | A `<Co>` whose last `<In>` disappears is written self-closing without `Nc`, matching the compiler's own rule. |

One run also dropped a Miniserver-created wire into the `AutoPilot`
block's `Input` connector (`FLG="1"` on the `<In>`); a clean keyboard-only
run did **not** reproduce this, so it is attributed to stray input during
that first run, not to load-time pruning. Re-test before treating it as a
format fact.

## The grown-gate experiment (D8) — answered, negatively

Second oracle run (2026-08-25), probing design decision D8 on the
headless Xvfb rig. Three GUI facts first, established by driving the
canvas:

- Wiring an `And` gate's last free input (`I2`) does **not** make an `I3`
  appear — not immediately, not after save + reload. The saved XML
  confirms: the wire connected, the gate stayed `I1`/`I2`/`Q`, `Nio="3"`.
- Dropping a wire on the gate *body* opens a connector picker
  ("Anschluss auswählen") that enumerates **only the existing inputs** —
  the GUI's own descriptor says a gate has exactly `I1` and `I2`.
- The block's `⊕` expander only toggles *display* of existing descriptor
  connectors (e.g. Monoflop's unwired `Off`/`Remanence`/`D`); there is no
  input-count property, no context-menu entry, no resize handle.

Then the compiler-side test: `lxir compile` minted `I3` at connector
index 3 (after `Q`'s 2 — the D8 assumption) wired from an `InputRef`,
onto the previously saved base. Loxone Config **loaded the file without
complaint** — and on `Ctrl+S` **silently deleted the `I3` connector and
its wire**, reverting the gate to `Nio="3"`. Everything else survived
byte-for-byte (semantic diff: exactly the one wire removed, zero object /
param / rename changes; the `NextObj` +2 burn and the `WF`
147456 → 16384 normalization appeared again).

Conclusions, now encoded in the compiler and docs (design.md D8,
loxone-format.md):

- **Gates are fixed two-input in Loxone Config 17.** `lxir` refuses
  `I3`+ with a cascade hint instead of minting it.
- **A save writes exactly the descriptor's connector set** —
  off-descriptor `<Co>`s are dropped silently, wires included. Never
  invent connectors.
- Positive side-finding: a compiler-drawn wire from an existing
  `InputRef.AQ` into a managed block survives the save untouched, and
  re-emitting a GUI-drawn wire from source (after teardown) reproduces it
  byte-identically.

## The adopt round-trip (D18) — passed

Third oracle session (2026-08-25), directly after `lxir adopt` shipped:
the whole-config rebuild (adopt the real house config → compile the
adopted module back onto it) was opened and saved by Loxone Config. The
adopt rebuild differs from the original in exactly the normalizations the
compiler applies — Co children re-emitted in spec order, `LtE` dropped,
managed blocks re-emitted at the end of their page — and none of them had
ever been GUI-blessed. Result: **semantically empty diff** against the
original; 14 of 22 adopted blocks byte-identical after the save, the rest
differing only by the known save fingerprint (`WF` 147456 → 16384, `<In>`
order rewrite, `NextObj` +2).

New format fact from this run: Loxone writes `<Co>` attributes in the
order **`K`, `Def`, `Nc`, `U`** — the save moved a compiler-emitted
`K,Nc,Def,U` into that order. The compiler's `sync_nc` now inserts `Nc`
after `Def`, before `U`.

## The mint oracle: Memory, PushButton, PButtonT — admitted

Same session, with the rig warm: minted minimal instances of the three
corpus-blocked types (plus wires from a real `VirtualIn`) onto the real
base, opened, saved. **All blocks, wires, and `Def` params survived.**
What the GUI added on save — schema-healing of children the compiler does
not emit:

- `Memory`: `Tp="0"` attribute, `<IoData></IoData>` (note:
  **non-self-closing even when empty** — falsifying the assumption that
  empty elements always serialize as `<X/>`), and a
  `<Display Unit="&lt;v.1&gt;" StateOnly="true"/>`.
- `PushButton`: a GUI-minted `SpStates` attribute (three fresh UUIDs whose
  machine tail matches our serial — the GUI consumes counter space for
  them), `<IoData Visu="true"/>`, `<PSD .../>`, `<Display Type="1" .../>`.
- `PButtonT`: `<IoData Visu="true"/>`.
- `WF` was **dropped entirely** on these types (not rewritten to 16384).
- Geometry corrected as usual (cosmetic).

Consequence at the time of the run: because compile is teardown/rebuild,
every compile deleted these GUI-added children and the next GUI save
re-added them, and adoption of GUI-created instances refused. Both were
resolved the same day by design decision D19 (GUI-owned residue is
carried forward verbatim from the base on every rebuild) — the churn
converges after the first GUI save, and the instances adopt cleanly.

The last blocker was `Memory.Q` — never wired anywhere in the corpus,
absent from both legacy dbs. Direction probe: compile a wire **sourced**
from a minted `test_mem.Q` into a `Not.I`. Since a save deletes anything
off-descriptor (D8, above), survival is decisive — the wire **survived
the save intact**, so `Q` is an output. All three types are now in
`connectors::builtin` (see [connector-db.md](connector-db.md)).

## The D19 + AutoJalousie rebuild — passed

Fourth session (2026-08-25, same day as D19 and the AutoJalousie
admission): the full adopt-rebuild of the real house config — **49
managed blocks**, including 12 AutoJalousie with carried
`COHist`/`IoData`/`SpStates` residue, grey Memory blocks, `NDOC=`,
`StatsType=` — opened and saved by Loxone Config.

Result: **semantically empty diff in both directions** (vs the original
and vs the rebuild we handed it). Byte-level, the save touched nothing
of ours except one Formula whose `WF` it rewrote 16384 → 147456 — the
*reverse* of the earlier 147456 → 16384 normalization, settling that
`WF` is GUI view-state it freely recomputes (D19 carries whichever
value the base has, so this never fights the GUI). Everything else in
the save fingerprint was unrelated to managed blocks: a schema
migration stamping `TId=` template ids onto 62 hardware-device
elements, the known AutopilotRule re-layout, the `NextObj` +2 burn,
and a one-pixel `Py2` nudge on an unmanaged Text note.

## The FLG wire-flag probe — inert, and now carried

Fifth run (2026-08-25, warm rig). 113 of the house's 880 wires carry
`FLG="1"` (one `FLG="2"`) on their `<In>` — Miniserver/app-created wire
metadata concentrated on API-connector distributions (`OutputRef` blocks
titled "API Connector" with port `K="AI"`, `SmokeAlarm.OutSilent`,
`Alarm.Q1`, the AutoPilot input). It blocked adoption of 4 AutoJalousie
instances: the rebuild could not reproduce the flag.

Probe: strip `FLG="1"` from two of those wires in a copy of the
GUI-saved base, open + save. Result — the GUI treats the flag as
**inert stored state**: the stripped wires survived the save *plain*
(neither deleted nor re-flagged), the 111 remaining flags round-tripped
verbatim, and the only other changes were the known fingerprint
(`NextObj` +2, three more `WF` flips).

Consequence, shipped the same day as a D19 extension: `FLG=` is
GUI-owned residue on wires — harvested at teardown keyed by (sink port,
source port) and re-emitted verbatim; a wire whose source changes in the
module drops the flag, which is exactly the state the probe validated.
Adoption accepts it, taking the real config from 49 to **53 of 59**
managed-type blocks (all 16 AutoJalousie; the 6 remaining refusals are
genuine `Inv=` inversions).

Confirmation run: the full 53-block rebuild — with compiler-emitted
`FLG=` wires — opened and saved. Semantically empty diff in both
directions; all 113 flags intact; the byte delta was exclusively the
fingerprint (`NextObj` +2, ~30 `WF` rewrites *in both directions* on
mostly unmanaged blocks — settling beyond doubt that `WF` is freely
recomputed view-state — plus one unmanaged Text-note geometry nudge and
`<In>` order rewrites).

## The first real-world change — passed

Sixth run (2026-08-25, fresh rig). The first change compiled for actual
deployment: the pool UV lamp, previously running permanently on a latched
`PushButton`, gated to two weekdays by wiring `Or(Montag.Q, Dienstag.Q)`
into the button's `On` and its inversion into `Reset` (module in the
private sibling repo `~/repos/my/r50`).

The probe value beyond the change itself: the `Or` inputs are wired
**directly from the system `Mode` objects' `Q` ports** — objects that
live in the system area with no `Px/Py` and no on-page representation.
The GUI's own pattern is different (dragging a mode onto a page creates
an `InputRef` wired from `Mode.Q`; nine corpus configs show it), so this
save answered whether the visual ref is *required*. It is not: open +
save kept both minted blocks and all 5 wires verbatim, created no
`InputRef`, and the semantic diff was empty in both directions. The
fingerprint was the known set plus a grid-snap of the two new blocks'
`Py` (the compiler's `next_free_py` cursor is not grid-aligned —
cosmetic, picked up by the next adopt).

Handing Loxone Config the crate's synthetic `examples/out` file
SIGSEGVs it on load (minidump in the prefix's
`AppData/Local/Temp/Loxone/Loxone Config/Dumps/`). A real config loaded
the same way is fine. The synthetic corpus is sufficient for the XML/UUID
layers but is **not loadable** by the real tool — it lacks required
structure. Oracle runs must always compile onto a real base config.

## The second-batch bless + mint oracle (session 8) — passed

Ran 2026-08-25 against the freshest live download (post-UV-push R50,
100 managed-type blocks after the second admission batch and D20).
Two configs, one Loxone Config instance (isolated `wine-oracle`
prefix on Xvfb `:5`), saved via Ctrl+S per tab:

- **Bless the 100-block rebuild.** `adopt` → `compile` output (all 23
  LightController2, Switch2Button, CentralShadeControl,
  CentralLightControl, Code16, and every D20-carried `Inv=` connector
  included) opened and saved with a **completely empty semantic diff**
  — every rebuilt element survived byte-faithfully at the semantic
  layer. `lxir drift` (D21) stayed green across the GUI save: the save
  fingerprint (NextObj burn, Date stamps, WF churn) never touches the
  drift baseline.
- **Mint oracle: PulseAt, DayTimer, Switch2Button.** Three blocks
  minted from bare declarations (`t = PulseAt("Oracle PulseAt")`, no
  args) on page Testing. All three survived the open+save with their
  **exact connector sets intact** (5, 13, 9 — the descriptor-law
  check: an off-descriptor key would have been dropped silently, a
  missing one materialized). The GUI backfilled only known D19 residue:
  recomputed block extents (`Px2`/`Py2`), `<IoData Visu="true"/>`,
  `<Display Unit>`, and DayTimer's ten default `<Entry To="1440"
  V="1">` schedule rows — all invisible to the semantic layer and
  carried verbatim by the next adopt/compile. PulseAt and DayTimer
  were admitted from web-corpus evidence only; this closes their mint
  validation.

## The rig

- Prefix: `~/.local/share/loxone-config/wine`, exe under
  `drive_c/Program Files (x86)/Loxone/LoxoneConfig/`. `LoxoneFormat.exe`
  is an SD-card formatter, not a CLI — there is no headless mode; the GUI
  must be driven.
- Open a file directly (spawns/reuses the GUI):
  `wine LoxoneConfig.exe 'Z:\path\to\copy.Loxone'` — always on a **copy**;
  never open the live project file. Opening never contacts the
  Miniserver; the connect action is a separate explicit button that
  automation must never touch.
- An auto-backup recovery dialog ("Soll diese wiederhergestellt
  werden?") may appear for a project identity that was open before —
  answer **No** (`Alt+N`) so the file opens as compiled.
- **Keyboard injection works** on KDE Wayland: activate the window via a
  KWin script (`workspace.activeWindow = w` over qdbus, matching caption
  `Loxone Config`), then send X11 XTEST key events (python-xlib) —
  XWayland delivers them to the focused Wine window. `Ctrl+S` saves;
  a freshly opened file is already dirty (schema migration), so the save
  always writes.
- **Pointer injection does not work** via XTEST under KWin: fake motion
  is ignored and clicks land at the physical cursor. The fix is the
  **headless rig** (used for the grown-gate experiment): run
  `Xvfb :5 -screen 0 2560x1440x24 -fbdir <dir> +extension GLX +render
  -noreset`, launch with `DISPLAY=:5` and `LIBGL_ALWAYS_SOFTWARE=1` —
  there `xdotool` has full native pointer *and* keyboard control
  (block selection, wire drags with mid-drag screenshots, menus).
  Screenshots come straight off the framebuffer:
  `magick xwd:<dir>/Xvfb_screen0 shot.png`.
- Xvfb quirks: the QtWebEngine news panel renders as a white overlay
  child window (~630×500) — find it via `xdotool search --name ".*"` +
  geometry and remove it with `xdotool windowunmap <id>`. Wine popup
  menus/pickers ignore Escape and outside clicks under Xvfb; unmap those
  the same way. Trust `xdotool search --onlyvisible` over screenshots
  (stale repaints), and trust the saved XML over the canvas.
- Project bookkeeping: opening a file rewrites
  `drive_c/users/<user>/Documents/Loxone/Loxone Config/Projects/Projects.json`
  (maps project → file path + Miniserver address) — useful to verify
  *which* file an instance actually has open, since window titles don't
  show paths.
