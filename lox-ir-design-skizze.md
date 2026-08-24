# lox-ir — Design-Skizze

*Diskussionsgrundlage. Eine Zwischensprache ("IR") für die Loxone-Konfiguration:
optimiert für semantische Diffs, GitOps, AI-Agent-Editierbarkeit und Modularisierung —
mit einem Compiler nach `.Loxone` und einem Decompiler zurück.*

---

## 1. Ziele und Nicht-Ziele

**Ziele**

- Quelltext statt Kompilat: Menschen und Agents editieren benannte, modulare Textdateien.
  UUIDs, Counter (`NextObj` …), `Nio`, Connector-Reihenfolge und Layout sind Compiler-Sache.
- Semantische Diffs und mergefähige PRs (keine globalen Counter im Quelltext).
- Verlustfreier Roundtrip mit Loxone Config: `compile → in Config öffnen → speichern`
  darf nichts zerstören (die P0-0-Fehlerklasse wird per CI-Orakel getestet, s. §6).
- Bestandsanlagen: Import/Decompile ist gleichwertiger Bürger, nicht Nachgedanke.

**Nicht-Ziele**

- Kein Ersatz für Loxone Config. Hardware (Air/Tree-Pairing, Extensions, Firmware,
  `Dev=`-Bindings) wird **referenziert, nicht besessen**.
- Keine Semantik-Modellierung aller ~254 Typen ab Tag 1 — unbekannte Typen laufen als
  Raw-Passthrough byte-treu durch (Open-World-Prinzip).
- Keine Formatstabilitäts-Versprechen über Loxone-Versionen hinweg; der Compiler ist
  auf eine `ConfigVersion` gepinnt (aktuell beobachtet: 17.1 / `17010727`).

---

## 2. Repo-Layout

```
haus/
  lox.toml                 # Projekt: Ziel-Miniserver, ConfigVersion-Pin, Optionen
  lox.lock.json            # Identitäten (Slug → UUID), Counter, Layout  ← generiert, aber committed
  externals.lox            # importierte, von Loxone Config besessene Objekte (Sensoren, Aktoren, Geräte)
  rooms/
    wohnzimmer.lox
    kueche.lox
  systems/
    beschattung.lox
    pool.lox
    pv-ueberschuss.lox
  templates/
    beschattung-fassade.lox
    nightlight.lox
  raw/
    unmanaged.lox          # Passthrough: alles, was die IR (noch) nicht modelliert
  tests/
    beschattung.test.lox
```

Ein `.lox`-File pro Raum oder Subsystem. Der Compiler fügt alles zu **einem**
`.Loxone`-Dokument zusammen; die Aufteilung ist reine Quelltext-Ergonomie.

---

## 3. Syntax-Kern

Bewusst netlisten-nah — das Domänenmodell *ist* Dataflow. Gezeigt am realen Beispiel
Beschattung Süd (Portnamen aus der Connector-Map von lox-cli bzw. `types.json` von
lox-config verifiziert: `GreaterEqual{Input1,Input2,Q}`, `And{I1,I2,Q}`,
`AutoJalousie{AutoShade,Safety,TargetPos,…}`).

### 3.1 Externals — Hardware referenzieren

```lox
# externals.lox — beim `lox ir import` erzeugt; Loxone Config bleibt Owner.
extern aussentemp:  AnalogInput   match title "Temperatur Außen"
extern sonne:       WeatherData   match title "Sonnenschein"
extern wind_alarm:  Switch        match uuid  "1d8af56e-036e-..."   # pinnen, wenn Titel ambig
extern jal_sued:    AutoJalousie  match title "Beschattung Süd"
```

`match title` löst beim Import/Compile gegen die reale Config auf und pinnt die UUID im
Lockfile. Titel-Ambiguität (z. B. "Smartaktor RGBW" pro Gerät dupliziert) ist ein
Compile-Fehler mit Auflösungsvorschlag — das behebt nebenbei das Selektor-Problem P1-1.

### 3.2 Blöcke, Parameter, Wires — die explizite Form

```lox
# systems/beschattung.lox
import externals

block temp_hoch: GreaterEqual {
  Input2 = 28            # Schwellwert als Parameter (Def=)
}

block beschatten: And

wire aussentemp.AQ  -> temp_hoch.Input1
wire temp_hoch.Q    -> beschatten.I1
wire sonne.Q        -> beschatten.I2
wire beschatten.Q   -> jal_sued.AutoShade
wire wind_alarm.Q   -> jal_sued.Safety

set jal_sued.TargetPos = 70   # Parameter an einem External setzen (erlaubt; Struktur nicht)
```

Referenzen sind **Slugs** (projektweit eindeutig, erzwungen), nie UUIDs, nie Titel.
`Title` ist ein optionales Attribut (`block temp_hoch: GreaterEqual "Temp über 28" { … }`).

### 3.3 Expression-Sugar — die verdichtete Form

Gleiche Logik, eine Zeile:

```lox
jal_sued.AutoShade = sonne.Q and (aussentemp.AQ >= 28)
jal_sued.Safety    = wind_alarm.Q
```

Der Compiler **desugart deterministisch** in genau die Blöcke aus 3.2 (synthetische
Slugs wie `beschattung.__expr1_ge`, stabil über Compiles via Lockfile). Zwei Backends,
pro Expression wählbar:

| Backend | erzeugt | Vorteil | Nachteil |
|---|---|---|---|
| `discrete` (Default) | GreaterEqual/And/Or/Not… | im Config-Canvas lesbar | viele Blöcke |
| `formula` | 1 Formula-Block (`IF(...)`-Übersetzung) | kompakt (14→2-Effekt) | max. `I1..I4`, im Canvas opak |

```lox
@backend(formula)
nachtlicht.BrightInact = if(minuten_seit_mitternacht.AQ >= 300 and minuten_seit_mitternacht.AQ < 1320, 30, 0)
```

### 3.4 Templates — Wiederverwendung

```lox
# templates/beschattung-fassade.lox
template beschattung_fassade(jalousie: AutoJalousie, temp_schwelle = 28, pos = 70) {
  block hoch: GreaterEqual { Input2 = temp_schwelle }
  wire aussentemp.AQ -> hoch.Input1
  jalousie.AutoShade = hoch.Q and sonne.Q
  jalousie.Safety    = wind_alarm.Q
  set jalousie.TargetPos = pos
}

# systems/beschattung.lox
use beschattung_fassade(jal_sued,  pos = 70)
use beschattung_fassade(jal_west,  pos = 85, temp_schwelle = 26)
```

Template-Instanzen bekommen namespaced Slugs (`beschattung.jal_sued.hoch`), damit
Identitäten pro Instanz stabil bleiben. Das ersetzt Copy-Paste-Räume ("15 Blöcke,
30 Wires pro Raum") und Markus' Nightlight-Pattern (P1-2) wird eine `use`-Zeile.

### 3.5 Raw-Passthrough — Open World

```lox
# raw/unmanaged.lox — vom Importer erzeugt, Attribut-byte-treu
raw block intercom_eg type "IntercomDevice" uuid "…" {
  attrs { Title = "Intercom EG"  V = "175"  … }        # unverändert durchgereicht
  ports { Trigger = "…-uuid"  Q = "…-uuid" }           # damit Managed-Code hierhin wiren darf
}
```

Regel: `raw`-Objekte werden beim Compile **niemals** strukturell angefasst — nur ihre
Ports sind als Wire-Ziele verfügbar. So überlebt jede reale Config den ersten
Import→Compile-Zyklus unbeschädigt, auch wenn die IR erst 50 von 254 Typen "versteht".

### 3.6 Tests — first-class, kompiliert nach lox-sim

```lox
# tests/beschattung.test.lox
test "schliesst bei Hitze und Sonne" {
  given { aussentemp = 30, sonne = 1, wind_alarm = 0 }
  after 10 ticks (dt = 0.1)
  expect jal_sued.AutoShade > 0.5
}

test "Windalarm gewinnt" {
  given { aussentemp = 30, sonne = 1, wind_alarm = 1 }
  expect jal_sued.Safety == 1
}

test "Nachtlicht aus um 23:00" {
  clock 23:00                      # setzt Simulationszeit voraus (heute Gap P0-2)
  expect nachtlicht.BrightInact == 0
}
```

Wichtig: der Sim-Compiler (`lox-sim/compiler.rs`) kann die IR **direkt** konsumieren —
am XML-Parser vorbei. Damit ist Testen unabhängig von P0-1 (Parse-Fehler auf realen
Configs), und `--sim`-JSON-Specs werden zu lesbarem Quelltext.

---

## 4. Das Lockfile — das eigentliche Herzstück

Ohne persistente Identität sieht jeder Compile wie delete-all/recreate-all aus:
Wires, Statistiken, Logger-Historie und App-Favoriten hängen an UUIDs. Das Lockfile
ist das Analogon zu `terraform.tfstate` / `package-lock.json` — **generiert, aber
committed**, damit CI und Kollegen dieselben Identitäten emittieren.

```jsonc
{
  "lockfile_version": 1,
  "target": {
    "config_version": "17010727",          // Pin; Compile gegen andere Version = Fehler
    "miniserver_serial": "504F94AABBCC",   // fließt in UUID-Segment 4 ein
    "source_config_sha256": "…"            // Stand der zuletzt importierten .Loxone
  },
  "counters": { "NextObj": 109928, "NextConst": 1, "NextNote": 1, "NextMem": 49 },

  "objects": {
    "beschattung.temp_hoch": {
      "uuid": "1d8af56e-0d21-39d8-ffffed57184a04d2",
      "type": "GreaterEqual",
      "ports": {                            // JEDER Port hat eigene UUID → einzeln pinnen!
        "Input1": "1d8af56e-0d22-…",
        "Input2": "1d8af56e-0d23-…",
        "Q":      "1d8af56e-0d24-…"
      },
      "layout": { "Px": 420, "Py": 260, "Px2": 520, "Py2": 340, "page": "Beschattung" }
    }
  },

  "externals": {
    "aussentemp": { "uuid": "…", "matched_by": "title", "title_at_match": "Temperatur Außen" }
  },

  "raw_digest": "sha256:…"                  // Fingerprint des unmanaged Teils (Drift-Erkennung)
}
```

**Invarianten**

1. Slug im Lock vorhanden → Compiler **mintet nie neu**, emittiert exakt diese
   Objekt- *und* Port-UUIDs. (Ports einzeln, sonst reißt jedes `<In Input=…>`.)
2. Slug neu → UUID minten (Serial-Schema), `NextObj` monoton fortschreiben.
3. Slug aus Quelle verschwunden → Compile-Fehler, außer explizit:
   `lox ir rm beschattung.temp_hoch` (analog `terraform state rm`) oder
   Rename via `moved beschattung.temp_hoch -> beschattung.hitze` im Quelltext.
4. **Layout lebt im Lock, nicht im Quelltext.** Verschiebt jemand Blöcke in Loxone
   Config, aktualisiert der Import nur das Lock — der Quelltext-Diff bleibt sauber.
5. Counter dürfen nie sinken (deckt sich mit dem `loxcheck.py`-Tamper-Indikator).

---

## 5. Compiler-Pipeline

```
lox ir compile   sources + lock  →  config.Loxone        (deterministisch, byte-stabil)
lox ir import    config.Loxone   →  Quelltext-Diff + Lock-Update   (Decompiler)
lox ir plan      zeigt semantischen Diff Quelle ↔ zuletzt bekannte Config
lox ir test      Tests direkt gegen lox-sim (ohne XML-Umweg)
lox ir verify    Orakel-Test, s. §6
lox ir push      compile + `lox config push` + Version-Tag
```

**Determinismus:** gleiche Quelle + gleiches Lock ⇒ byte-identisches `.Loxone`
(stabile Element-Reihenfolge, kanonische Connector-Reihenfolge inputs→outputs,
kanonisches Attribut-Layout). Nur so sind Git-Diffs des Kompilats aussagekräftig
und das GitOps-Feature von `lox` bleibt nutzbar.

**Decompiler (`import`):** normalisiert (à la `loxnorm.py`: nur wert-identische
Attribut-Duplikate entfernen, Multi-`ControlList`-Dateien splitten), matcht Objekte
gegen das Lock (UUID-first), erzeugt für Unbekanntes `raw`-Einträge, und schreibt
Änderungen an managed Objekten als Quelltext-Diff zur Review.

---

## 6. Verifikation: zwei unabhängige Orakel

1. **Loxone Config selbst** (die entscheidende Idee aus tobsch/`experiments.md`:
   Config speichert Projekte **offline, ohne Miniserver**):

   ```
   compile → out.Loxone → in Loxone Config öffnen → speichern → loxdiff
   erwartet: Δ = ∅  (bzw. nur bekannte Normalisierungen)
   ```

   Als CI mit Windows-VM automatisierbar (UI-Automation: öffnen, Ctrl+S, schließen).
   Testet exakt die P0-0-Klasse — UUID-Regeneration, verworfene Wires, `Nio`-Repair —
   systematisch statt per Live-Ausfall.

2. **lox-sim** für Verhalten: jede `test`-Sektion läuft gegen den Simulator; die
   Eval-Suite von lox-cli (322 Cases) wird auf IR-Ebene portierbar und *lesbar*.

Dazu Property-Tests: `import(compile(src)) == src` (modulo Layout) auf echten,
anonymisierten Configs.

---

## 7. Zwei-Meister-Workflow

Loxone Config bleibt Schreiber (Hardware, gelegentliche manuelle Edits). Drift wird
nicht verhindert, sondern zum expliziten Schritt:

```
Alltag:   edit .lox → plan → test → compile → verify → push
Nach UX-Session:  lox config pull → lox ir import
                  → managed geändert?  Quelltext-Diff reviewen & committen
                  → nur Layout/Hardware? Lock-Update, Quelle unberührt
                  → neue unbekannte Objekte? landen in raw/, später "adoptierbar":
                    lox ir adopt <uuid> --as beschattung.neuer_block
```

`adopt` ist der Migrationspfad für Bestandsanlagen: eine reale Config wird Stück für
Stück von `raw` nach managed überführt — nie big-bang.

---

## 8. Staffelung

| Stufe | Inhalt | Risiko | Nutzen sofort |
|---|---|---|---|
| **0** | Decompiler als reine *View* + Lockfile-Design. Kein Schreibpfad. | ~0 | lesbare Diffs/Reviews im bestehenden GitOps-Flow; zwingt Identitätsmodell vor Schreibcode |
| **1** | Compile für die Connector-Map-Teilmenge (190 Typen), Rest raw. Orakel-CI. | mittel | „edit → sim → push" ohne UUID-Jonglage; P0-0 strukturell erledigt |
| **2** | Templates, Expressions (beide Backends), `clock` im Sim, `adopt`. | inkrementell | Agent- und Mensch-Ergonomie |

Bausteine existieren bereits: `ConfigEditor` + Connector-Map (lox-cli) ≈ Codegen-Backend ·
`types.json`/KB-Mapping (lox-config) ≈ Typ-Doku-Schicht · lox-sim ≈ Test-Backend ·
`lox config pull/diff` ≈ Transport + GitOps-Rahmen. Die IR ist der fehlende gemeinsame
Treffpunkt der drei Repos.

---

## 9. Offene Fragen (für den Call)

1. **Syntax-Host:** eigene Grammatik (wie skizziert) vs. KDL/HCL vs. YAML?
   (These: eigene Grammatik lohnt wegen `wire`/Expression-Sugar; YAML wäre der
   schnellste Start, aber Wiring in YAML ist genau die Ergonomie, die man loswerden will.)
2. **Lockfile in Git:** committed (Reproduzierbarkeit, Review von Identitätsänderungen)
   vs. lokal (kleinere Diffs)? → Vorschlag: committed, Layout-Teil ggf. separat.
3. **Expression-Semantik:** wie weit? Nur bool/vergleich (→ discrete) oder volle
   Formula-Grammatik (`IF`, Arithmetik, 4-Input-Limit)?
4. **ConfigVersion-Politik:** ein Pin pro Repo? Wie testen wir neue Loxone-Releases
   (→ Orakel-CI pro Version)?
5. **Repo-/Lizenzfrage:** IR als neues gemeinsames Repo? GPL-3 (lox) vs. AGPL-3
   (lox-cli) vs. MIT (lox-config) — für ein gemeinsames Projekt braucht es eine
   Entscheidung inkl. CLA, sonst zerbricht Dual-Licensing.
6. **Naming:** `lox ir` als Subcommand vs. eigenes Tool (`loxc`)?

---

*Anhang-Idee für später: vollständiges Beispiel `pool.lox` (DS18B20-Temperatur,
Abdeckungs-Interlock, PV-Überschuss-Freigabe der Wärmepumpe) als realer End-to-End-Case.*
