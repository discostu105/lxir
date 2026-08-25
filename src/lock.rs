//! The lockfile: persistent identity between IR source and compiled config.
//!
//! Everything that must stay stable across compiles but does not belong in
//! source text lives here — the analogue of `terraform.tfstate` /
//! `package-lock.json`. Generated, but meant to be committed, so CI and
//! collaborators emit the same UUIDs.
//!
//! Invariants (enforced by the compiler):
//!
//! 1. Slug present in the lock → the compiler never re-mints; it emits
//!    exactly the recorded object *and* port UUIDs (every `<In Input=…>`
//!    in the config points at a port UUID).
//! 2. New slug → mint UUIDs, advance `NextObj` monotonically.
//! 3. Slug gone from source → hard error, unless the removal or rename is
//!    declared in source (`removed <slug>` / `moved <old> -> <new>` — the
//!    preferred, reviewable path) or resolved out-of-band via
//!    [`Lockfile::remove_object`] (the `terraform state rm` analogue) /
//!    [`Lockfile::rename_object`].
//! 4. Counters never decrease.
//! 5. An authorized removal leaves a tombstone (D31) until a compile sees
//!    a base without the object — so recompiles against a base from before
//!    the removal was deployed still delete it instead of passing it
//!    through as an unmanaged orphan, and the compile stays a lock
//!    fixpoint through the whole compile → push → download window.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// v2 adds the `removed` tombstone map (D31). v1 locks load fine (the map
/// defaults to empty); saving always writes the current version, so an old
/// binary — whose version check refuses v2 — can never silently drop
/// tombstones it does not know about.
pub const LOCKFILE_VERSION: u32 = 2;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lockfile {
    pub lockfile_version: u32,
    #[serde(default)]
    pub target: Target,
    #[serde(default)]
    pub counters: LockCounters,
    /// Managed blocks: slug → identity.
    #[serde(default)]
    pub objects: BTreeMap<String, LockedObject>,
    /// Resolved externals: slug → pinned identity.
    #[serde(default)]
    pub externals: BTreeMap<String, LockedExternal>,
    /// Wires the compiler wrote onto ports of *external* objects
    /// (`from`/`to` are port UUIDs). Needed so a wire removed from source is
    /// removed from the config without touching wires owned by Loxone Config.
    #[serde(default)]
    pub extern_wires: Vec<LockedWire>,
    /// Original `Def=` values of extern ports before the first assignment
    /// (`target.Port = value`), keyed by port UUID (`None` = the attribute
    /// was absent). Restored when the assignment disappears from source.
    #[serde(default)]
    pub set_originals: BTreeMap<String, Option<String>>,
    /// Removal tombstones (D31), keyed by object UUID. A block whose
    /// removal was authorized (`removed <slug>`, `allow_removals`, or an
    /// expression-owned block vanishing) moves here instead of being
    /// forgotten: every compile deletes the object from the base if it is
    /// still present, and drops the tombstone once a compile sees a base
    /// without it (the removal reached the Miniserver). Keyed by UUID so a
    /// slug can be reused while its old removal is still pending.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub removed: BTreeMap<String, Tombstone>,
    /// Extern wires the compiler withdrew (D31): a wire that vanished from
    /// source stays here so compiles against bases that still carry it
    /// keep deleting it; a base without the wire retires the entry.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub removed_wires: BTreeSet<LockedWire>,
    /// Withdrawn `Def=` writes on extern ports (D31), keyed by port UUID.
    /// A base whose port still carries the written value predates the
    /// deployment — the original is restored again; a base already showing
    /// the original (or a third writer's value) retires the entry.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub removed_sets: BTreeMap<String, RetiredSet>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub config_version: Option<String>,
    pub miniserver_serial: Option<String>,
    pub source_config_sha256: Option<String>,
    /// [`crate::diff::semantic_fingerprint`] of the last compiled output
    /// (at adoption: of the adopted config — the first compile is a
    /// semantic no-op, so the value is the same). The drift baseline
    /// `lxir drift` checks a fresh download against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockCounters {
    pub next_obj: u64,
    pub next_const: u64,
    pub next_note: u64,
    pub next_mem: u64,
    /// The minter's sequence counter after the last mint. Later compiles
    /// seed from it (and from every locked UUID's sequence) so a block
    /// added in a second compile session can never reuse a
    /// (time, sequence) pair from the first — with a fixed mint time the
    /// bare per-run counter would collide. Absent (0) in older locks; the
    /// locked-UUID scan covers those.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub next_mint: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedObject {
    pub uuid: String,
    #[serde(rename = "type")]
    pub block_type: String,
    /// Port key → port UUID. Every port gets its own pinned UUID.
    pub ports: BTreeMap<String, String>,
    pub layout: Option<Layout>,
    /// UUID of the `<C Type="Page">` the block lives on. Pinned on first
    /// compile (from the compile options' page) or by adoption (the page
    /// the existing block was drawn on); rebuilds place the block there.
    /// `None` only in locks from before page pinning — the next compile
    /// fills it with the options' page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_uuid: Option<String>,
    /// The block was generated by expression desugaring (D24). When its
    /// slug vanishes from the (desugared) source — the expression was
    /// edited or deleted — the compiler removes it without a `removed`
    /// statement: no hand ever wrote it, the expression is the sole owner.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expr_owned: bool,
}

/// What a removal tombstone remembers: enough to recognize the object in
/// an old base (the map key is its UUID) and to talk about it in messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    pub slug: String,
    #[serde(rename = "type")]
    pub block_type: String,
}

/// A withdrawn `Def=` write (D31): what to restore, and what the compiler
/// had written — the marker by which a pre-deployment base is recognized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetiredSet {
    /// The pre-`set` value to restore (`None` = the attribute was absent).
    pub original: Option<String>,
    pub written: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    pub px: i64,
    pub py: i64,
    pub px2: i64,
    pub py2: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedExternal {
    pub uuid: String,
    /// How the extern was resolved: `"uuid"`, `"iname"`, or `"title"`.
    pub matched_by: String,
    pub title_at_match: Option<String>,
    pub iname_at_match: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct LockedWire {
    pub from: String,
    pub to: String,
}

impl Lockfile {
    pub fn new() -> Self {
        Lockfile {
            lockfile_version: LOCKFILE_VERSION,
            ..Default::default()
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let mut lock: Lockfile = serde_json::from_slice(&bytes)?;
        if !(1..=LOCKFILE_VERSION).contains(&lock.lockfile_version) {
            return Err(Error::Lock(format!(
                "unsupported lockfile_version {} (expected 1..={LOCKFILE_VERSION})",
                lock.lockfile_version
            )));
        }
        // Saving always writes the current version — a v1 lock upgrades on
        // its next save, and old binaries refuse the result instead of
        // silently dropping fields they do not know.
        lock.lockfile_version = LOCKFILE_VERSION;
        Ok(lock)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.to_json())?;
        Ok(())
    }

    /// Stable, pretty JSON (BTreeMaps keep key order deterministic).
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("lockfile serializes");
        s.push('\n');
        s
    }

    /// Explicitly forget a managed block (`terraform state rm` analogue).
    /// The next compile will then treat its absence from source as intended.
    pub fn remove_object(&mut self, slug: &str) -> Result<LockedObject> {
        self.objects
            .remove(slug)
            .ok_or_else(|| Error::Lock(format!("no managed object `{slug}` in lock")))
    }

    /// Rename a managed block while keeping its identity (UUIDs survive).
    pub fn rename_object(&mut self, old: &str, new: &str) -> Result<()> {
        if self.objects.contains_key(new) {
            return Err(Error::Lock(format!("slug `{new}` already exists in lock")));
        }
        let obj = self.remove_object(old)?;
        self.objects.insert(new.to_string(), obj);
        Ok(())
    }

    /// Raise lock counters to at least the document's (never decrease).
    pub fn absorb_counters(&mut self, doc: crate::doc::Counters) {
        let c = &mut self.counters;
        c.next_obj = c.next_obj.max(doc.next_obj);
        c.next_const = c.next_const.max(doc.next_const);
        c.next_note = c.next_note.max(doc.next_note);
        c.next_mem = c.next_mem.max(doc.next_mem);
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_and_stability() {
        let mut lock = Lockfile::new();
        lock.objects.insert(
            "temp_hoch".into(),
            LockedObject {
                uuid: "00000001-0000-0000-ffff504f94112233".into(),
                block_type: "GreaterEqual".into(),
                ports: BTreeMap::from([("Q".to_string(), "…q".to_string())]),
                layout: Some(Layout {
                    px: 960,
                    py: 960,
                    px2: 2304,
                    py2: 1656,
                }),
                page_uuid: Some("00000002-0000-0000-ffff504f94112233".into()),
                expr_owned: false,
            },
        );
        let json = lock.to_json();
        let back: Lockfile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, lock);
        assert_eq!(back.to_json(), json, "serialization must be stable");
    }

    #[test]
    fn rename_and_remove() {
        let mut lock = Lockfile::new();
        lock.objects.insert(
            "a".into(),
            LockedObject {
                uuid: "u".into(),
                block_type: "And".into(),
                ports: BTreeMap::new(),
                layout: None,
                page_uuid: None,
                expr_owned: false,
            },
        );
        lock.rename_object("a", "b").unwrap();
        assert!(lock.objects.contains_key("b"));
        assert!(lock.rename_object("missing", "c").is_err());
        lock.remove_object("b").unwrap();
        assert!(lock.objects.is_empty());
    }

    #[test]
    fn counters_never_decrease() {
        let mut lock = Lockfile::new();
        lock.counters.next_obj = 100;
        lock.absorb_counters(crate::doc::Counters {
            next_obj: 50,
            next_const: 2,
            next_note: 1,
            next_mem: 3,
        });
        assert_eq!(lock.counters.next_obj, 100);
        assert_eq!(lock.counters.next_mem, 3);
    }
}
