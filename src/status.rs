//! Drift triage (`lxir status`): classify how a config moved away from the
//! lockfile's baseline and say what resolves each change — read-only.
//!
//! [`crate::diff`] lists raw UUID-keyed changes between two files, and
//! `lxir drift` answers *whether* another writer changed something. This
//! module sits on top of both and answers *what to do*: it names changes
//! by their managed slug, sorts them into "a recompile will undo this
//! unless you adopt it in source", "new block one incremental adopt away",
//! "push the output you already compiled", and "outside managed scope",
//! and pairs each finding with the statement or command that resolves it
//! (design decision D38). Nothing here writes anything.

use crate::connectors;
use crate::diff;
use crate::doc::{LoxoneDoc, ObjectSummary, ports};
use crate::ir::slugify;
use crate::lock::{Lockfile, Tombstone};
use std::collections::{BTreeMap, BTreeSet};

/// One foreign edit touching identity the compiler owns, with the action
/// that resolves it. `slug` is the managed block or extern concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedChange {
    pub slug: String,
    pub block_type: String,
    /// What changed, e.g. `param I2: "1" -> "2"`.
    pub detail: String,
    /// What resolves it, e.g. `a recompile restores "1"; …`.
    pub action: String,
}

/// A new block of a managed type — one incremental adopt away from source
/// control. `slug` is a suggestion derived from the title, unique against
/// the lockfile and the other suggestions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adoptable {
    pub uuid: String,
    pub block_type: String,
    pub title: Option<String>,
    pub slug: String,
}

/// What [`triage`] found. Empty vectors and zero counts mean the drift —
/// if the fingerprint said there was any — is entirely save noise the
/// diff projection cannot see (position moves), or locale renames.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusReport {
    /// Removals the last compile already made that this config still
    /// carries (D31 tombstones): the compiled output was never pushed.
    pub pending_push: Vec<Tombstone>,
    /// Foreign edits to managed identity, each with its resolving action.
    pub managed: Vec<ManagedChange>,
    /// New managed-type blocks, ready for `lxir adopt --uuid`.
    /// Only detected with a reference document.
    pub adoptable: Vec<Adoptable>,
    /// Changes outside managed scope — compiles pass them through.
    pub unmanaged_changes: usize,
    /// Locale-suspect renames (save noise, not drift).
    pub locale_renames: usize,
    /// Whether a reference (the last compiled output) was available; the
    /// diff-based findings above need one, the lock-based ones do not.
    pub has_reference: bool,
}

impl StatusReport {
    /// Anything that needs a decision or an action?
    pub fn needs_attention(&self) -> bool {
        !self.pending_push.is_empty() || !self.managed.is_empty() || !self.adoptable.is_empty()
    }
}

/// Classify how `cfg` (typically a fresh download) drifted from the
/// lockfile's baseline. `reference` is the last compiled output (the
/// project's `out` file) — without it only the lock-derivable findings
/// are produced: pending removals, deleted managed blocks, deleted
/// externs.
pub fn triage(cfg: &LoxoneDoc, reference: Option<&LoxoneDoc>, lock: &Lockfile) -> StatusReport {
    let mut report = StatusReport {
        has_reference: reference.is_some(),
        ..Default::default()
    };
    let cfg_objects = cfg.objects();
    let cfg_uuids: BTreeSet<&str> = cfg_objects.iter().map(|o| o.uuid.as_str()).collect();

    // --- Lock vs. config: findings that need no reference document. ---
    for (uuid, tomb) in &lock.removed {
        if cfg_uuids.contains(uuid.as_str()) {
            report.pending_push.push(tomb.clone());
        }
    }
    for (slug, obj) in &lock.objects {
        if !cfg_uuids.contains(obj.uuid.as_str()) {
            report.managed.push(ManagedChange {
                slug: slug.clone(),
                block_type: obj.block_type.clone(),
                detail: "deleted by another writer".into(),
                action: format!(
                    "a recompile re-creates it in place; if the removal is intended, \
                     declare `removed {slug}` and recompile"
                ),
            });
        }
    }
    for (slug, ext) in &lock.externals {
        if !cfg_uuids.contains(ext.uuid.as_str()) {
            report.managed.push(ManagedChange {
                slug: slug.clone(),
                block_type: String::new(),
                detail: "the external object no longer exists".into(),
                action: format!(
                    "the module still references `{slug}` — the next compile fails to \
                     resolve it unless another object matches its spec; restore the \
                     object in Loxone Config, or remove the extern and its wires \
                     from source"
                ),
            });
        }
    }

    let Some(reference) = reference else {
        return report;
    };

    // --- Reference vs. config: the classified semantic diff. ---
    let d = diff::diff(reference, cfg);

    // Lookups: locked identity by object UUID and by port UUID.
    let obj_slug: BTreeMap<&str, &str> = lock
        .objects
        .iter()
        .map(|(s, o)| (o.uuid.as_str(), s.as_str()))
        .collect();
    let ext_slug: BTreeMap<&str, &str> = lock
        .externals
        .iter()
        .map(|(s, e)| (e.uuid.as_str(), s.as_str()))
        .collect();
    let managed_ports: BTreeMap<&str, (&str, &str)> = lock
        .objects
        .iter()
        .flat_map(|(s, o)| {
            o.ports
                .iter()
                .map(move |(k, u)| (u.as_str(), (s.as_str(), k.as_str())))
        })
        .collect();
    let compiler_wires: BTreeSet<(&str, &str)> = lock
        .extern_wires
        .iter()
        .map(|w| (w.from.as_str(), w.to.as_str()))
        .collect();
    let set_ports: BTreeSet<&str> = lock
        .set_originals
        .keys()
        .chain(lock.removed_sets.keys())
        .map(String::as_str)
        .collect();

    // Port UUID → (owner UUID, key) and owner UUID → summary, from both
    // documents so removed wires still resolve to names.
    let mut owners: BTreeMap<String, ObjectSummary> = BTreeMap::new();
    let mut port_owner: BTreeMap<String, (String, String)> = BTreeMap::new();
    for doc in [reference, cfg] {
        for o in doc.objects() {
            if let Some(el) = doc.element_at(&o.path) {
                for p in ports(el) {
                    port_owner.insert(p.uuid, (o.uuid.clone(), p.key));
                }
            }
            owners.insert(o.uuid.clone(), o);
        }
    }
    let port_label = |port_uuid: &str| -> String {
        if let Some((slug, key)) = managed_ports.get(port_uuid) {
            return format!("{slug}.{key}");
        }
        let Some((owner, key)) = port_owner.get(port_uuid) else {
            return format!("port {port_uuid}");
        };
        if let Some(slug) = ext_slug.get(owner.as_str()) {
            return format!("{slug}.{key}");
        }
        match owners.get(owner) {
            Some(o) => match &o.title {
                Some(t) => format!("{} \"{t}\".{key}", o.block_type),
                None => format!("{}.{key}", o.block_type),
            },
            None => format!("port {port_uuid}"),
        }
    };
    let fmt_val = |v: &Option<String>| -> String {
        match v {
            Some(v) => format!("\"{v}\""),
            None => "(absent)".into(),
        }
    };

    // Slug suggestions must not collide with anything named in the lock,
    // nor with each other.
    let mut used: BTreeSet<String> = lock
        .objects
        .keys()
        .chain(lock.externals.keys())
        .cloned()
        .collect();

    for o in &d.added {
        if lock.removed.contains_key(&o.uuid) {
            continue; // already reported as pending_push
        }
        let adoptable = connectors::builtin(&o.block_type).is_some()
            && !matches!(o.block_type.as_str(), "InputRef" | "OutputRef")
            && !obj_slug.contains_key(o.uuid.as_str())
            && !ext_slug.contains_key(o.uuid.as_str());
        if !adoptable {
            report.unmanaged_changes += 1;
            continue;
        }
        let base = slugify(
            o.title
                .as_deref()
                .or(o.iname.as_deref())
                .unwrap_or(&o.block_type),
        );
        let mut slug = base.clone();
        let mut n = 2;
        while used.contains(&slug) {
            slug = format!("{base}_{n}");
            n += 1;
        }
        used.insert(slug.clone());
        report.adoptable.push(Adoptable {
            uuid: o.uuid.clone(),
            block_type: o.block_type.clone(),
            title: o.title.clone(),
            slug,
        });
    }

    for o in &d.removed {
        // Deletions of locked identity were already reported above.
        if !obj_slug.contains_key(o.uuid.as_str()) && !ext_slug.contains_key(o.uuid.as_str()) {
            report.unmanaged_changes += 1;
        }
    }

    for r in &d.renamed {
        if r.locale_suspect {
            report.locale_renames += 1;
        } else if let Some(slug) = obj_slug.get(r.uuid.as_str()) {
            report.managed.push(ManagedChange {
                slug: (*slug).to_string(),
                block_type: r.block_type.clone(),
                detail: format!("retitled {} -> {}", fmt_val(&r.from), fmt_val(&r.to)),
                action: format!(
                    "a recompile restores {}; to keep the new title, update the \
                     block's label in source",
                    fmt_val(&r.from)
                ),
            });
        } else if let Some(slug) = ext_slug.get(r.uuid.as_str()) {
            // A retitle only matters for an extern the module matches by
            // that title — the lock's uuid pin keeps it resolving either
            // way (compile tolerates title drift on a pinned extern), but
            // the source matcher now lies.
            if lock.externals[*slug].matched_by == "title" {
                report.managed.push(ManagedChange {
                    slug: (*slug).to_string(),
                    block_type: r.block_type.clone(),
                    detail: format!("retitled {} -> {}", fmt_val(&r.from), fmt_val(&r.to)),
                    action: format!(
                        "the lock's uuid pin keeps `{slug}` resolving, but its `title:` \
                         matcher no longer matches the object — update the matcher \
                         (or pin by uuid:) so source stays truthful"
                    ),
                });
            } else {
                report.unmanaged_changes += 1;
            }
        } else {
            report.unmanaged_changes += 1;
        }
    }

    for p in &d.param_changes {
        if let Some(slug) = obj_slug.get(p.object_uuid.as_str()) {
            report.managed.push(ManagedChange {
                slug: (*slug).to_string(),
                block_type: p.block_type.clone(),
                detail: format!(
                    "param {}: {} -> {}",
                    p.port_key,
                    fmt_val(&p.from),
                    fmt_val(&p.to)
                ),
                action: format!(
                    "a recompile restores {}; to keep the new value, update the module",
                    fmt_val(&p.from)
                ),
            });
            continue;
        }
        // A `Def=` the compiler wrote onto an extern port (`slug.Port =
        // value`) is compiler-owned even though the object is not. A param
        // change implies the object exists in both documents, and `owners`
        // keeps the config's path (inserted last), so look it up there.
        let port_uuid = owners
            .get(&p.object_uuid)
            .and_then(|o| cfg.element_at(&o.path))
            .and_then(|el| ports(el).into_iter().find(|v| v.key == p.port_key))
            .map(|v| v.uuid);
        match (port_uuid, ext_slug.get(p.object_uuid.as_str())) {
            (Some(u), Some(slug)) if set_ports.contains(u.as_str()) => {
                report.managed.push(ManagedChange {
                    slug: (*slug).to_string(),
                    block_type: p.block_type.clone(),
                    detail: format!(
                        "set {}.{}: {} -> {}",
                        slug,
                        p.port_key,
                        fmt_val(&p.from),
                        fmt_val(&p.to)
                    ),
                    action: format!(
                        "the compiler owns this Def= (a `{}.{} = …` assignment); a \
                         recompile restores {} — update the assignment to keep {}",
                        slug,
                        p.port_key,
                        fmt_val(&p.from),
                        fmt_val(&p.to)
                    ),
                });
            }
            _ => report.unmanaged_changes += 1,
        }
    }

    for w in &d.wires_added {
        if let Some((slug, key)) = managed_ports.get(w.to_port.as_str()) {
            report.managed.push(ManagedChange {
                slug: (*slug).to_string(),
                block_type: lock.objects[*slug].block_type.clone(),
                detail: format!("new wire {} -> {slug}.{key}", port_label(&w.from_port)),
                action: format!(
                    "a recompile removes it (the compiler rebuilds `{slug}` from \
                     source); declare `{key}: {}` in `{slug}`'s argument list to \
                     keep it",
                    port_label(&w.from_port)
                ),
            });
        } else {
            report.unmanaged_changes += 1;
        }
    }

    for w in &d.wires_removed {
        let sink_managed = managed_ports.get(w.to_port.as_str());
        let compiler_drawn = compiler_wires.contains(&(w.from_port.as_str(), w.to_port.as_str()));
        if let Some((slug, key)) = sink_managed {
            report.managed.push(ManagedChange {
                slug: (*slug).to_string(),
                block_type: lock.objects[*slug].block_type.clone(),
                detail: format!("wire {} -> {slug}.{key} deleted", port_label(&w.from_port)),
                action: format!(
                    "a recompile redraws it; remove `{key}: …` from `{slug}` in \
                     source to accept the removal"
                ),
            });
        } else if compiler_drawn {
            let sink = port_label(&w.to_port);
            report.managed.push(ManagedChange {
                slug: sink.clone(),
                block_type: String::new(),
                detail: format!("wire {} -> {sink} deleted", port_label(&w.from_port)),
                action: format!(
                    "the compiler drew this wire (a `{sink} <- …` statement); a \
                     recompile redraws it — delete the statement to accept the \
                     removal"
                ),
            });
        } else {
            report.unmanaged_changes += 1;
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lock::{LockedExternal, LockedObject, LockedWire};

    /// Reference (last compiled output) and a drifted download of the same
    /// little installation. The drift covers every classification arm.
    fn reference() -> LoxoneDoc {
        parse(
            "<C Type=\"And\" U=\"AND\" Title=\"Beschatten\">\r\n\
             \t\t\t\t<Co K=\"I1\" U=\"AND-I1\"/>\r\n\
             \t\t\t\t<Co K=\"I2\" Def=\"1\" U=\"AND-I2\"/>\r\n\
             \t\t\t\t<Co K=\"Q\" U=\"AND-Q\"/>\r\n\
             \t\t\t</C>\r\n\
             \t\t\t<C Type=\"GreaterEqual\" U=\"GE\" Title=\"Temp hoch\">\r\n\
             \t\t\t\t<Co K=\"Q\" U=\"GE-Q\"/>\r\n\
             \t\t\t</C>\r\n\
             \t\t\t<C Type=\"VirtualIn\" U=\"VI\" Title=\"Sonne\" IName=\"VI1\">\r\n\
             \t\t\t\t<Co K=\"Q\" U=\"VI-Q\"/>\r\n\
             \t\t\t</C>\r\n\
             \t\t\t<C Type=\"AutoJalousie\" U=\"JAL\" Title=\"Jalousie\">\r\n\
             \t\t\t\t<Co K=\"AutoShade\" U=\"JAL-AS\">\r\n\
             \t\t\t\t\t<In Input=\"AND-Q\"/>\r\n\
             \t\t\t\t</Co>\r\n\
             \t\t\t</C>",
        )
    }

    fn drifted() -> LoxoneDoc {
        parse(
            // I2 retuned, a foreign wire drawn into I1, block retitled.
            "<C Type=\"And\" U=\"AND\" Title=\"Beschatten NEU\">\r\n\
             \t\t\t\t<Co K=\"I1\" U=\"AND-I1\">\r\n\
             \t\t\t\t\t<In Input=\"VI-Q\"/>\r\n\
             \t\t\t\t</Co>\r\n\
             \t\t\t\t<Co K=\"I2\" Def=\"2\" U=\"AND-I2\"/>\r\n\
             \t\t\t\t<Co K=\"Q\" U=\"AND-Q\"/>\r\n\
             \t\t\t</C>\r\n\
             // GE deleted; extern retitled; the compiler's AutoShade wire deleted.
             \t\t\t<C Type=\"VirtualIn\" U=\"VI\" Title=\"Sonnenfühler\" IName=\"VI1\">\r\n\
             \t\t\t\t<Co K=\"Q\" U=\"VI-Q\"/>\r\n\
             \t\t\t</C>\r\n\
             \t\t\t<C Type=\"AutoJalousie\" U=\"JAL\" Title=\"Jalousie\">\r\n\
             \t\t\t\t<Co K=\"AutoShade\" U=\"JAL-AS\"/>\r\n\
             \t\t\t</C>\r\n\
             // Two new managed-type blocks with a colliding title, one\r\n
             // unmanaged-type block, and one block the last compile removed.
             \t\t\t<C Type=\"Monoflop\" U=\"MONO1\" Title=\"Treppenlicht\"/>\r\n\
             \t\t\t<C Type=\"Monoflop\" U=\"MONO2\" Title=\"Treppenlicht\"/>\r\n\
             \t\t\t<C Type=\"AnalogInput\" U=\"AIN\" Title=\"Sensor\"/>\r\n\
             \t\t\t<C Type=\"Or\" U=\"TOMB\" Title=\"Alt\"/>",
        )
    }

    fn parse(blocks: &str) -> LoxoneDoc {
        // The test XML uses `//` comment markers for readability; strip them.
        let blocks: String = blocks
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let s = format!(
            "<ControlList Version=\"1\" NextObj=\"20\">\r\n\
             \t<C Type=\"Document\" U=\"DOC\">\r\n\
             \t\t<C Type=\"Page\" U=\"PAGE\" Title=\"P\">\r\n\
             \t\t\t{blocks}\r\n\
             \t\t</C>\r\n\
             \t</C>\r\n\
             </ControlList>\r\n"
        );
        LoxoneDoc::parse(s.as_bytes()).unwrap()
    }

    fn lock() -> Lockfile {
        let mut lock = Lockfile::new();
        let obj = |uuid: &str, ty: &str, ports: &[(&str, &str)]| LockedObject {
            uuid: uuid.into(),
            block_type: ty.into(),
            ports: ports
                .iter()
                .map(|(k, u)| ((*k).to_string(), (*u).to_string()))
                .collect(),
            layout: None,
            page_uuid: None,
            expr_owned: false,
        };
        lock.objects.insert(
            "beschatten".into(),
            obj(
                "AND",
                "And",
                &[("I1", "AND-I1"), ("I2", "AND-I2"), ("Q", "AND-Q")],
            ),
        );
        lock.objects.insert(
            "temp_hoch".into(),
            obj("GE", "GreaterEqual", &[("Q", "GE-Q")]),
        );
        let ext = |uuid: &str, matched_by: &str| LockedExternal {
            uuid: uuid.into(),
            matched_by: matched_by.into(),
            title_at_match: None,
            iname_at_match: None,
        };
        lock.externals.insert("sonne".into(), ext("VI", "title"));
        lock.externals.insert("jal_sued".into(), ext("JAL", "uuid"));
        lock.extern_wires.push(LockedWire {
            from: "AND-Q".into(),
            to: "JAL-AS".into(),
        });
        lock.removed.insert(
            "TOMB".into(),
            Tombstone {
                slug: "alt".into(),
                block_type: "Or".into(),
            },
        );
        lock
    }

    fn find<'a>(report: &'a StatusReport, slug: &str, detail_part: &str) -> &'a ManagedChange {
        report
            .managed
            .iter()
            .find(|m| m.slug == slug && m.detail.contains(detail_part))
            .unwrap_or_else(|| {
                panic!("no managed change `{slug}` / `{detail_part}` in {report:#?}")
            })
    }

    #[test]
    fn classifies_every_arm() {
        let report = triage(&drifted(), Some(&reference()), &lock());

        // The last compile's removal, still present → push, not drift.
        assert_eq!(report.pending_push.len(), 1);
        assert_eq!(report.pending_push[0].slug, "alt");

        // Foreign edits to managed identity, each with a pointed action.
        let deleted = find(&report, "temp_hoch", "deleted by another writer");
        assert!(deleted.action.contains("removed temp_hoch"));
        let param = find(&report, "beschatten", "param I2");
        assert!(param.detail.contains("\"1\" -> \"2\""));
        let retitle = find(&report, "beschatten", "retitled");
        assert!(retitle.action.contains("label"));
        let wire_in = find(&report, "beschatten", "new wire sonne.Q -> beschatten.I1");
        assert!(wire_in.action.contains("I1: sonne.Q"));
        let ext_retitle = find(&report, "sonne", "retitled");
        assert!(ext_retitle.action.contains("title:"));
        let wire_gone = find(&report, "jal_sued.AutoShade", "deleted");
        assert!(wire_gone.action.contains("<-"));
        assert_eq!(report.managed.len(), 6, "{report:#?}");

        // New managed-type blocks: adopt suggestions with deduped slugs.
        assert_eq!(report.adoptable.len(), 2);
        assert_eq!(report.adoptable[0].slug, "treppenlicht");
        assert_eq!(report.adoptable[1].slug, "treppenlicht_2");
        assert!(report.adoptable.iter().all(|a| a.block_type == "Monoflop"));

        // The AnalogInput is not lxir's business.
        assert_eq!(report.unmanaged_changes, 1);
        assert_eq!(report.locale_renames, 0);
        assert!(report.needs_attention());
    }

    #[test]
    fn without_a_reference_only_lock_findings_remain() {
        let report = triage(&drifted(), None, &lock());
        assert!(!report.has_reference);
        assert_eq!(report.pending_push.len(), 1);
        // Deleted managed block and nothing diff-based.
        assert_eq!(report.managed.len(), 1);
        assert_eq!(report.managed[0].slug, "temp_hoch");
        assert!(report.adoptable.is_empty());
        assert_eq!(report.unmanaged_changes, 0);
    }

    #[test]
    fn in_sync_config_reports_nothing() {
        let doc = reference();
        let mut lock = lock();
        lock.removed.clear();
        let report = triage(&doc, Some(&doc), &lock);
        assert!(!report.needs_attention(), "{report:#?}");
        assert_eq!(report.unmanaged_changes, 0);
    }
}
