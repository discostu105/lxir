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

## Crash: minimal synthetic configs

Handing Loxone Config the crate's synthetic `examples/out` file
SIGSEGVs it on load (minidump in the prefix's
`AppData/Local/Temp/Loxone/Loxone Config/Dumps/`). A real config loaded
the same way is fine. The synthetic corpus is sufficient for the XML/UUID
layers but is **not loadable** by the real tool — it lacks required
structure. Oracle runs must always compile onto a real base config.

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
  is ignored and clicks land at the physical cursor. Canvas interaction
  (selecting blocks, drawing wires, the D8 grown-gate experiment) needs
  `ydotool` (uinput, drives the real seat) or a headless
  `Xvfb`+`xdotool` display, where XTEST pointer control is native.
- Project bookkeeping: opening a file rewrites
  `drive_c/users/<user>/Documents/Loxone/Loxone Config/Projects/Projects.json`
  (maps project → file path + Miniserver address) — useful to verify
  *which* file an instance actually has open, since window titles don't
  show paths.
