# Lockfile specification (v1)

The lockfile is the persistent identity between IR source and compiled
config — the analogue of `terraform.tfstate` / `package-lock.json`.
Conventional name: `<module>.lock.json`, next to the module. **Commit it**:
collaborators and CI must emit the same UUIDs.

It is generated and consumed by `compile`; the only supported manual
operations are the ones listed under *Operations*. Do not hand-edit fields
otherwise — a wrong port UUID silently breaks wire identity.

## Structure

```jsonc
{
  "lockfile_version": 1,

  "target": {                       // informational metadata (nullable)
    "config_version": "17010727",   // ConfigVersion of the last base
    "miniserver_serial": "504F94112233",  // machine id minted into UUIDs;
                                    // also the CLI's --serial fallback
    "source_config_sha256": "…"     // sha256 of the last base's bytes
  },

  "counters": {                     // ControlList Next* counters, monotone
    "next_obj": 202,                // raised by 1 per minted managed object
    "next_const": 1,
    "next_note": 1,
    "next_mem": 1
  },

  "objects": {                      // managed blocks: slug → identity
    "beschatten": {
      "uuid": "1ff9b180-0000-0007-ffff504f94112233",
      "type": "And",
      "ports": {                    // EVERY port has a pinned UUID —
        "I1": "…-00ff…",            // wires reference port UUIDs, so
        "I2": "…-01ff…",            // these must never change
        "Q":  "…-02ff…"
      },
      "layout": {                   // drawing rectangle; kept stable so
        "px": 7392, "py": 1080,     // recompiles don't shuffle the page
        "px2": 8736, "py2": 1776
      },
      "page_uuid": "…"              // <C Type="Page"> the block lives on;
                                    // pinned on first compile (options'
                                    // page) or by adopt (original page).
                                    // Absent in pre-page-pinning locks —
                                    // the next compile fills it.
    }
  },

  "externals": {                    // resolved externs: slug → pin
    "sonne": {
      "uuid": "20000003-0000-0030-ffff504f94112233",
      "matched_by": "iname",        // "uuid" | "iname" | "title"
      "title_at_match": "Sonnenschein",   // for humans reading diffs
      "iname_at_match": "VI3"
    }
  },

  "extern_wires": [                 // <In> elements WE added to extern
    { "from": "<port-uuid>",        // ports; sorted, deduplicated.
      "to":   "<port-uuid>" }       // Teardown removes exactly these.
  ],

  "set_originals": {                // extern port uuid → Def value before
    "<port-uuid>": "100"            // our first assignment (null = attr
  }                                 // was absent). Restored when the
                                    // assignment leaves the source.
}
```

Serialization is stable: `serde_json` pretty-printing over `BTreeMap`s,
trailing newline. Recompiling with unchanged inputs reproduces the file
byte-for-byte, so lockfile diffs in review always mean something.

## Invariants (enforced by `compile`)

1. A slug present in `objects` is never re-minted; the recorded object *and
   port* UUIDs are emitted exactly.
2. A new slug mints UUIDs and appends them here before the output is
   produced.
3. A slug in `objects` but missing from source aborts the compile unless
   removal is explicit (see the removal trichotomy in
   [design.md](design.md)).
4. `counters` never decrease; they absorb the base document's counters
   (max) and advance by one per minted object.
5. `externals` entries are dropped when the extern leaves the source;
   `extern_wires` and `set_originals` are rebuilt each compile from what the
   source actually declares (teardown first, so nothing leaks).

## Operations

| Task | How |
|---|---|
| stop managing a block (keep its XML) | `Lockfile::remove_object(slug)` |
| rename a slug, keep identity | `Lockfile::rename_object(old, new)` |
| delete a block from the config | recompile with `allow_removals` |
| adopt a different Miniserver | new lockfile (UUIDs embed the machine id) |

After a compile **error**, discard the in-memory lock and reload from disk —
`compile` may have partially advanced it before failing. (The CLI does this
naturally: it only saves the lock after a successful compile.)

## Versioning

`lockfile_version` is checked on load; unknown versions are rejected rather
than reinterpreted. Schema changes bump the version and ship a migration.
