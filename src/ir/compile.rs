//! The compiler: base config + IR module + lockfile → new config.
//!
//! The compiler owns exactly three kinds of edits and touches nothing else
//! in the (losslessly parsed) base document:
//!
//! 1. **Managed blocks** — `block` declarations are (re)built from scratch on
//!    every compile, with identity pinned by the lockfile.
//! 2. **Extern wires** — wires whose sink is an extern port are added to the
//!    extern's `<Co>`; the lockfile records them so removing the `wire` from
//!    source removes the `<In>` again without disturbing wires drawn in
//!    Loxone Config.
//! 3. **Extern sets** — `set` on an extern port rewrites its `Def=`; the
//!    pre-set value is remembered and restored when the `set` disappears.
//!
//! Everything is deterministic: same base + module + lock + options → the
//! same output bytes. New UUIDs come from [`crate::uuid::Minter`] (no clock,
//! no RNG) and are recorded in the lock immediately, so later compiles reuse
//! them.
//!
//! v0 limitations (documented, checked, erroring — never silent):
//! - Only block types in [`crate::connectors::builtin`] can be created.
//! - Wiring or `set`ting an extern port requires that port's `<Co>` to exist
//!   in the base config (Loxone omits nothing we have observed, but an
//!   unverified type could; we refuse to invent port UUIDs for them).
//! - `And`/`Or` are fixed two-input blocks: wiring `I3`+ is an error. The
//!   Wine oracle showed Loxone Config 17 silently deletes off-descriptor
//!   connectors (and their wires) on save, so minting them would lose logic.

use crate::connectors::{PortDir, builtin};
use crate::doc::{Counters, LoxoneDoc, ports};
use crate::error::{Error, Result};
use crate::ir::ast::{MatchSpec, Module};
use crate::lock::{Layout, LockedExternal, LockedObject, LockedWire, Lockfile, sha256_hex};
use crate::uuid::{Minter, entity_for_slug};
use crate::xml::Element;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct CompileOptions {
    /// 6-byte machine id stamped into minted object UUIDs — conventionally
    /// the Miniserver serial ([`crate::uuid::parse_serial`]).
    pub machine: [u8; 6],
    /// Creation time recorded in minted UUIDs (segment 1). Caller-provided
    /// so compiles are reproducible.
    pub mint_time_unix: i64,
    /// Title of the `<C Type="Page">` to place managed blocks on;
    /// `None` = the document's first page.
    pub page_title: Option<String>,
    /// A managed block present in the lock but missing from source is a
    /// hard error by default (protection against accidental deletion).
    /// With `allow_removals`, it is deleted from the config and dropped
    /// from the lock instead — the destructive-apply path. The third
    /// option, [`Lockfile::remove_object`], *forgets* the block, leaving
    /// its XML in the config as an unmanaged orphan.
    pub allow_removals: bool,
}

/// Managed-block geometry, matching what Loxone Config draws:
/// blocks are 1344 wide, 504 tall plus 192 per port beyond two,
/// stacked with a 192 gap on a fixed column.
const BLOCK_X: i64 = 7392;
const BLOCK_W: i64 = 1344;
const BLOCK_H_BASE: i64 = 504;
const PORT_H: i64 = 192;
const GAP: i64 = 192;

pub fn compile(
    base: &LoxoneDoc,
    module: &Module,
    lock: &mut Lockfile,
    opts: &CompileOptions,
) -> Result<LoxoneDoc> {
    module.validate()?;

    // --- Sync check: the lock must not know managed blocks the source lost.
    let src_slugs: BTreeSet<String> = module.blocks().map(|b| b.slug.clone()).collect();
    if !opts.allow_removals
        && let Some(slug) = lock.objects.keys().find(|s| !src_slugs.contains(*s))
    {
        return Err(Error::Compile(format!(
            "managed block `{slug}` is in the lockfile but not in the source; \
             if the removal is intended, compile with allow_removals to delete it, \
             or Lockfile::remove_object(\"{slug}\") to orphan it \
             (rename_object for a rename)"
        )));
    }

    let mut doc = base.clone();
    lock.absorb_counters(doc.counters());

    // --- Resolve externs (against the untouched base).
    let extern_uuid = resolve_externs(&doc, module, lock)?;

    // --- Tear down our previous output: managed objects, extern wires,
    //     extern sets. What remains is exactly the Loxone-Config-owned state.
    for obj in lock.objects.values() {
        doc.remove_by_uuid(&obj.uuid);
    }
    // Vanished slugs (only reachable with allow_removals) are now deleted
    // from the config; drop their identity too.
    lock.objects.retain(|slug, _| src_slugs.contains(slug));
    let old_wires = std::mem::take(&mut lock.extern_wires);
    for w in &old_wires {
        remove_wire(&mut doc, &w.from, &w.to);
    }
    let old_originals = std::mem::take(&mut lock.set_originals);
    for (port_uuid, original) in &old_originals {
        restore_def(&mut doc, port_uuid, original.as_deref());
    }

    // --- Plan managed blocks: the builtin port list is the full port list
    //     and pinned identity for every one of them.
    let mut minter = Minter::new(opts.machine, opts.mint_time_unix);
    let mut managed: BTreeMap<String, PlannedBlock> = BTreeMap::new();
    let mut py_cursor = next_free_py(lock);
    for block in module.blocks() {
        let specs = builtin(&block.block_type).ok_or_else(|| {
            Error::Compile(format!(
                "block `{}`: type `{}` is not in the verified builtin table and cannot be created",
                block.slug, block.block_type
            ))
        })?;
        let keys: Vec<String> = specs.iter().map(|s| s.key.to_string()).collect();
        let mut refs: Vec<&str> = Vec::new();
        for w in module.wires() {
            for r in [&w.from, &w.to] {
                if r.slug == block.slug {
                    refs.push(&r.port);
                }
            }
        }
        for s in module.sets() {
            if s.target.slug == block.slug {
                refs.push(&s.target.port);
            }
        }
        for (k, _) in block.params() {
            refs.push(k);
        }
        for key in refs {
            if keys.iter().any(|k| k == key) {
                continue;
            }
            // Loxone Config 17 silently DELETES off-descriptor connectors
            // (and their wires) on save — verified via the Wine oracle with
            // a grown `I3` on `And`. Gates are fixed two-input; refusing here
            // is what keeps compiled logic from vanishing.
            let gate_hint = if matches!(block.block_type.as_str(), "And" | "Or")
                && key
                    .strip_prefix('I')
                    .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            {
                "; Loxone Config gates are fixed two-input (a grown I3 is \
                 silently deleted on save) — cascade 2-input gates instead"
            } else {
                ""
            };
            return Err(Error::Compile(format!(
                "unknown port `{key}` on block `{}` (type `{}`); known ports: {}{gate_hint}",
                block.slug,
                block.block_type,
                keys.join(", ")
            )));
        }

        // Pin identity: reuse the lock entry, minting only what is missing.
        // For a brand-new block, ports are minted before the object —
        // matching the counter order observed in Loxone Config output.
        let entity = entity_for_slug(&block.slug);
        let entry = lock
            .objects
            .entry(block.slug.clone())
            .or_insert_with(|| LockedObject {
                uuid: String::new(),
                block_type: block.block_type.clone(),
                ports: BTreeMap::new(),
                layout: None,
            });
        if entry.uuid.is_empty() {
            entry.block_type = block.block_type.clone();
        } else if entry.block_type != block.block_type {
            return Err(Error::Compile(format!(
                "block `{}` changed type from `{}` to `{}`; remove it from the lock first \
                 (Lockfile::remove_object) to accept new identity",
                block.slug, entry.block_type, block.block_type
            )));
        }
        for (index, key) in keys.iter().enumerate() {
            entry
                .ports
                .entry(key.clone())
                .or_insert_with(|| minter.mint_port(index as u8, entity).to_string());
        }
        if entry.uuid.is_empty() {
            entry.uuid = minter.mint_object().to_string();
            lock.counters.next_obj += 1;
        }
        let height = BLOCK_H_BASE + PORT_H * (keys.len() as i64 - 2).max(0);
        let layout = *entry.layout.get_or_insert_with(|| {
            let l = Layout {
                px: BLOCK_X,
                py: py_cursor,
                px2: BLOCK_X + BLOCK_W,
                py2: py_cursor + height,
            };
            py_cursor = l.py2 + GAP;
            l
        });
        managed.insert(
            block.slug.clone(),
            PlannedBlock {
                uuid: entry.uuid.clone(),
                block_type: block.block_type.clone(),
                keys,
                ports: entry.ports.clone(),
                layout,
            },
        );
    }

    // --- Def values on managed ports: block params first, `set` overrides.
    let mut managed_defs: BTreeMap<(String, String), String> = BTreeMap::new();
    for block in module.blocks() {
        for (k, v) in block.params() {
            managed_defs.insert((block.slug.clone(), k.to_string()), v.to_string());
        }
    }
    for s in module.sets() {
        if managed.contains_key(&s.target.slug) {
            managed_defs.insert(
                (s.target.slug.clone(), s.target.port.clone()),
                s.value.clone(),
            );
        }
    }

    // --- Build the managed <C> elements and append them to the page.
    let page_path = doc.page_path(opts.page_title.as_deref()).ok_or_else(|| {
        Error::Compile(match &opts.page_title {
            Some(t) => format!("no <C Type=\"Page\"> titled `{t}` in the base config"),
            None => "the base config has no <C Type=\"Page\"> to place blocks on".to_string(),
        })
    })?;
    let page = doc
        .element_at_mut(&page_path)
        .expect("page path just resolved");
    for block in module.blocks() {
        let plan = &managed[&block.slug];
        let mut el = Element::new("C");
        el.set_attr("Type", &block.block_type);
        el.set_attr("V", "175");
        el.set_attr("U", &plan.uuid);
        el.set_attr("Title", block.title.as_deref().unwrap_or(&block.slug));
        el.set_attr("Px", &plan.layout.px.to_string());
        el.set_attr("Py", &plan.layout.py.to_string());
        el.set_attr("Px2", &plan.layout.px2.to_string());
        el.set_attr("Py2", &plan.layout.py2.to_string());
        el.set_attr("Cl", "141,255,112");
        el.set_attr("Nio", &plan.keys.len().to_string());
        el.set_attr("WF", "147456");
        for key in &plan.keys {
            let mut co = Element::new("Co");
            co.set_attr("K", key);
            if let Some(def) = managed_defs.get(&(block.slug.clone(), key.clone())) {
                co.set_attr("Def", def);
            }
            co.set_attr("U", &plan.ports[key]);
            el.push_child(co);
        }
        page.push_child(el);
    }

    // --- Wires.
    for w in module.wires() {
        let from = resolve_port(
            &doc,
            &managed,
            &extern_uuid,
            &w.from.slug,
            &w.from.port,
            PortDir::Output,
        )?;
        let to = resolve_port(
            &doc,
            &managed,
            &extern_uuid,
            &w.to.slug,
            &w.to.port,
            PortDir::Input,
        )?;
        add_wire(&mut doc, &to.owner_uuid, &to.port_uuid, &from.port_uuid)?;
        if to.is_extern {
            lock.extern_wires.push(LockedWire {
                from: from.port_uuid,
                to: to.port_uuid,
            });
        }
    }
    lock.extern_wires.sort();
    lock.extern_wires.dedup();

    // --- Sets on extern ports (managed ones became Def= above).
    for s in module.sets() {
        if managed.contains_key(&s.target.slug) {
            continue;
        }
        let target = resolve_port(
            &doc,
            &managed,
            &extern_uuid,
            &s.target.slug,
            &s.target.port,
            PortDir::Param,
        )?;
        let original = apply_def(&mut doc, &target.owner_uuid, &target.port_uuid, &s.value)?;
        lock.set_originals
            .entry(target.port_uuid)
            .or_insert(original);
    }

    // --- Counters and target metadata.
    doc.set_counters(Counters {
        next_obj: lock.counters.next_obj,
        next_const: lock.counters.next_const,
        next_note: lock.counters.next_note,
        next_mem: lock.counters.next_mem,
    });
    lock.target.config_version = doc.config_version();
    lock.target.miniserver_serial = Some(
        opts.machine
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect::<String>(),
    );
    lock.target.source_config_sha256 = Some(sha256_hex(&base.to_bytes()));

    fixup_emptied_elements(&mut doc.xml.root);
    Ok(doc)
}

struct PlannedBlock {
    uuid: String,
    block_type: String,
    /// All port keys in connector-index order.
    keys: Vec<String>,
    ports: BTreeMap<String, String>,
    layout: Layout,
}

struct ResolvedPort {
    owner_uuid: String,
    port_uuid: String,
    is_extern: bool,
}

fn resolve_externs(
    doc: &LoxoneDoc,
    module: &Module,
    lock: &mut Lockfile,
) -> Result<BTreeMap<String, String>> {
    let objects = doc.objects();
    let mut out = BTreeMap::new();
    for ext in module.externs() {
        // Lock pin wins as long as it still resolves to an object of the
        // declared type; otherwise fall back to fresh resolution by spec.
        if let Some(locked) = lock.externals.get(&ext.slug)
            && objects
                .iter()
                .any(|o| o.uuid == locked.uuid && o.block_type == ext.block_type)
        {
            out.insert(ext.slug.clone(), locked.uuid.clone());
            continue;
        }
        let matches: Vec<_> = objects
            .iter()
            .filter(|o| {
                o.block_type == ext.block_type
                    && match &ext.match_spec {
                        MatchSpec::Uuid(u) => &o.uuid == u,
                        MatchSpec::IName(v) => o.iname.as_deref() == Some(v),
                        MatchSpec::Title(v) => o.title.as_deref() == Some(v),
                    }
            })
            .collect();
        match matches.as_slice() {
            [] => {
                return Err(Error::NoMatch {
                    slug: ext.slug.clone(),
                    spec: format!("{} {}", ext.block_type, ext.match_spec),
                });
            }
            [only] => {
                lock.externals.insert(
                    ext.slug.clone(),
                    LockedExternal {
                        uuid: only.uuid.clone(),
                        matched_by: match &ext.match_spec {
                            MatchSpec::Uuid(_) => "uuid",
                            MatchSpec::IName(_) => "iname",
                            MatchSpec::Title(_) => "title",
                        }
                        .to_string(),
                        title_at_match: only.title.clone(),
                        iname_at_match: only.iname.clone(),
                    },
                );
                out.insert(ext.slug.clone(), only.uuid.clone());
            }
            many => {
                return Err(Error::AmbiguousMatch {
                    slug: ext.slug.clone(),
                    spec: format!("{} {}", ext.block_type, ext.match_spec),
                    count: many.len(),
                    candidates: many.iter().map(|o| o.uuid.clone()).collect(),
                });
            }
        }
    }
    // Drop resolutions for externs no longer declared.
    let declared: BTreeSet<&str> = module.externs().map(|e| e.slug.as_str()).collect();
    lock.externals
        .retain(|slug, _| declared.contains(slug.as_str()));
    Ok(out)
}

fn resolve_port(
    doc: &LoxoneDoc,
    managed: &BTreeMap<String, PlannedBlock>,
    extern_uuid: &BTreeMap<String, String>,
    slug: &str,
    port: &str,
    want: PortDir,
) -> Result<ResolvedPort> {
    if let Some(plan) = managed.get(slug) {
        let port_uuid =
            plan.ports.get(port).cloned().ok_or_else(|| {
                Error::Compile(format!("no port `{port}` on managed block `{slug}`"))
            })?;
        // Direction check against the builtin table (extras are inputs).
        let index = plan.keys.iter().position(|k| k == port).expect("planned");
        let dir = builtin(&plan.block_type)
            .and_then(|specs| specs.get(index).map(|s| s.dir))
            .unwrap_or(PortDir::Input);
        let ok = match want {
            PortDir::Output => dir == PortDir::Output,
            // Wire sinks and `set` targets are inputs/params on blocks.
            PortDir::Input | PortDir::Param => dir != PortDir::Output,
        };
        if !ok {
            return Err(Error::Compile(format!(
                "`{slug}.{port}` is an {} port and cannot be used as {}",
                if dir == PortDir::Output {
                    "output"
                } else {
                    "input"
                },
                if want == PortDir::Output {
                    "a wire source"
                } else {
                    "a wire sink / set target"
                },
            )));
        }
        return Ok(ResolvedPort {
            owner_uuid: plan.uuid.clone(),
            port_uuid,
            is_extern: false,
        });
    }
    let owner_uuid = extern_uuid
        .get(slug)
        .ok_or_else(|| Error::Compile(format!("`{slug}` is not a declared block or extern")))?;
    // Externs are open-world: the port must merely exist in the base config.
    let idx = doc.index();
    let path = idx
        .by_uuid
        .get(owner_uuid)
        .ok_or_else(|| Error::Compile(format!("extern `{slug}` vanished from the document")))?;
    let el = doc.element_at(path).expect("indexed path");
    let known = ports(el);
    match known.iter().find(|p| p.key == port) {
        Some(p) => Ok(ResolvedPort {
            owner_uuid: owner_uuid.clone(),
            port_uuid: p.uuid.clone(),
            is_extern: true,
        }),
        None => Err(Error::Compile(format!(
            "extern `{slug}` has no port `{port}` in the base config; present ports: {}. \
             (v0 cannot mint ports for unverified types — wire or set it once in Loxone \
             Config so the connector exists.)",
            known
                .iter()
                .map(|p| p.key.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// Find the `<C>` element with this object UUID, mutably.
fn find_c_mut<'a>(el: &'a mut Element, uuid: &str) -> Option<&'a mut Element> {
    if el.name == "C" && el.attrs.iter().any(|a| a.name == "U" && a.value == uuid) {
        return Some(el);
    }
    el.child_elements_mut().find_map(|c| find_c_mut(c, uuid))
}

/// Find the `<C>` owning a port and the `<Co>` itself, mutably.
fn find_co_mut<'a>(el: &'a mut Element, port_uuid: &str) -> Option<&'a mut Element> {
    let owns = el.name == "C"
        && el
            .child_elements()
            .any(|c| c.name == "Co" && c.attr("U") == Some(port_uuid));
    if owns {
        return el
            .child_elements_mut()
            .find(|c| c.name == "Co" && c.attr("U") == Some(port_uuid));
    }
    el.child_elements_mut()
        .find_map(|c| find_co_mut(c, port_uuid))
}

/// Append `<In Input=from/>` under the sink `<Co>` and maintain `Nc`.
fn add_wire(doc: &mut LoxoneDoc, owner_uuid: &str, to_port: &str, from_port: &str) -> Result<()> {
    let owner = find_c_mut(&mut doc.xml.root, owner_uuid)
        .ok_or_else(|| Error::Compile(format!("wire sink object `{owner_uuid}` not found")))?;
    let co = owner
        .child_elements_mut()
        .find(|c| c.name == "Co" && c.attr("U") == Some(to_port))
        .ok_or_else(|| Error::Compile(format!("wire sink port `{to_port}` not found")))?;
    let duplicate = co
        .child_elements()
        .any(|i| i.name == "In" && i.attr("Input") == Some(from_port));
    if !duplicate {
        let mut input = Element::new("In");
        input.set_attr("Input", from_port);
        co.push_child(input);
    }
    sync_nc(co);
    Ok(())
}

/// Remove `<In Input=from/>` from the sink `<Co>` (used to tear down wires
/// the previous compile owned).
fn remove_wire(doc: &mut LoxoneDoc, from_port: &str, to_port: &str) {
    if let Some(co) = find_co_mut(&mut doc.xml.root, to_port) {
        co.children.retain(|n| {
            !matches!(n, crate::xml::Node::Element(e)
                if e.name == "In" && e.attr("Input") == Some(from_port))
        });
        sync_nc(co);
    }
}

/// Keep `Nc` equal to the number of `<In>` children (absent when zero),
/// in canonical position right after `K`.
fn sync_nc(co: &mut Element) {
    let n = co.child_elements().filter(|c| c.name == "In").count();
    if n == 0 {
        co.remove_attr("Nc");
        if co.children.is_empty() {
            co.self_closing = true;
        }
    } else {
        set_attr_ordered(co, "Nc", &n.to_string(), &["Def", "U"]);
    }
}

/// Rewrite an extern port's `Def=`, returning the previous (decoded) value.
fn apply_def(
    doc: &mut LoxoneDoc,
    owner_uuid: &str,
    port_uuid: &str,
    value: &str,
) -> Result<Option<String>> {
    let owner = find_c_mut(&mut doc.xml.root, owner_uuid)
        .ok_or_else(|| Error::Compile(format!("set target object `{owner_uuid}` not found")))?;
    let co = owner
        .child_elements_mut()
        .find(|c| c.name == "Co" && c.attr("U") == Some(port_uuid))
        .ok_or_else(|| Error::Compile(format!("set target port `{port_uuid}` not found")))?;
    let original = co.attr_decoded("Def").map(|v| v.into_owned());
    set_attr_ordered(co, "Def", value, &["U"]);
    Ok(original)
}

/// Restore an extern port's `Def=` to its pre-`set` state.
fn restore_def(doc: &mut LoxoneDoc, port_uuid: &str, original: Option<&str>) {
    if let Some(co) = find_co_mut(&mut doc.xml.root, port_uuid) {
        match original {
            Some(v) => set_attr_ordered(co, "Def", v, &["U"]),
            None => {
                co.remove_attr("Def");
            }
        }
    }
}

/// Set an attribute, inserting it before the first of `before` when absent
/// (Loxone keeps `<Co>` attributes in the order `K, Nc, Def, U`).
fn set_attr_ordered(el: &mut Element, name: &str, value: &str, before: &[&str]) {
    if el.attr(name).is_some() {
        el.set_attr(name, value);
        return;
    }
    let pos = el
        .attrs
        .iter()
        .position(|a| before.contains(&a.name.as_str()))
        .unwrap_or(el.attrs.len());
    el.attrs.insert(
        pos,
        crate::xml::Attr {
            name: name.to_string(),
            value: crate::xml::escape(value).into_owned(),
        },
    );
}

/// Elements our removals emptied must serialize as `<X/>`, like everything
/// empty in Loxone output.
fn fixup_emptied_elements(el: &mut Element) {
    if el.children.is_empty() {
        el.self_closing = true;
    }
    for child in el.child_elements_mut() {
        fixup_emptied_elements(child);
    }
}

/// First stacking position below every layout already in the lock, so new
/// blocks never land on top of existing ones.
fn next_free_py(lock: &Lockfile) -> i64 {
    lock.objects
        .values()
        .filter_map(|o| o.layout.map(|l| l.py2 + GAP))
        .max()
        .unwrap_or(GAP)
}
