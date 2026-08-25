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

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

pub const LOCKFILE_VERSION: u32 = 1;

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
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub config_version: Option<String>,
    pub miniserver_serial: Option<String>,
    pub source_config_sha256: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockCounters {
    pub next_obj: u64,
    pub next_const: u64,
    pub next_note: u64,
    pub next_mem: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedObject {
    pub uuid: String,
    #[serde(rename = "type")]
    pub block_type: String,
    /// Port key → port UUID. Every port gets its own pinned UUID.
    pub ports: BTreeMap<String, String>,
    pub layout: Option<Layout>,
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
        let lock: Lockfile = serde_json::from_slice(&bytes)?;
        if lock.lockfile_version != LOCKFILE_VERSION {
            return Err(Error::Lock(format!(
                "unsupported lockfile_version {} (expected {LOCKFILE_VERSION})",
                lock.lockfile_version
            )));
        }
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
