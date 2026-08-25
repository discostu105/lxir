//! The compiler: base config + IR module + lockfile → new config.
//!
//! The compiler owns exactly three kinds of edits and touches nothing else
//! in the (losslessly parsed) base document:
//!
//! 1. **Managed blocks** — `slug = Type(…)` declarations are (re)built from
//!    scratch on every compile, with identity pinned by the lockfile.
//! 2. **Extern wires** — wires whose sink is an extern port
//!    (`target.Port <- source.Port`) are added to the extern's `<Co>`; the
//!    lockfile records them so removing the statement removes the `<In>`
//!    again without disturbing wires drawn in Loxone Config.
//! 3. **Extern sets** — `target.Port = value` on an extern port rewrites its
//!    `Def=`; the pre-set value is remembered and restored when the
//!    statement disappears.
//!
//! Everything is deterministic: same base + module + lock + options → the
//! same output bytes. New UUIDs come from [`crate::uuid::Minter`] (no clock,
//! no RNG) and are recorded in the lock immediately, so later compiles reuse
//! them.
//!
//! v0 limitations (documented, checked, erroring — never silent):
//! - Only block types in [`crate::connectors::builtin`] can be created.
//! - Wiring or assigning an extern port requires that port's `<Co>` to exist
//!   in the base config (Loxone omits nothing we have observed, but an
//!   unverified type could; we refuse to invent port UUIDs for them).
//! - `And`/`Or` are fixed two-input blocks: wiring `I3`+ is an error. The
//!   Wine oracle showed Loxone Config 17 silently deletes off-descriptor
//!   connectors (and their wires) on save, so minting them would lose logic.

use crate::connectors::{PortDir, attr_params, builtin};
use crate::doc::{Counters, GUI_OWNED_ATTRS, GUI_OWNED_CHILDREN, LoxoneDoc, ports};
use crate::error::{Error, Result};
use crate::ir::ast::{MatchSpec, Module, Value};
use crate::ir::validate::{suggest, validate_ports};
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
    /// The preferred authorization is a `removed <slug>` statement in the
    /// module — scoped and reviewable. `allow_removals` is the blunt
    /// alternative: *every* vanished block is deleted from the config and
    /// dropped from the lock. The third option,
    /// [`Lockfile::remove_object`], *forgets* the block, leaving its XML
    /// in the config as an unmanaged orphan.
    pub allow_removals: bool,
}

/// GUI-owned residue of an existing managed block (D19), harvested at
/// teardown and re-emitted verbatim by the rebuild: the GUI owns this
/// content, so it is read from the base on every compile — never stored —
/// and later GUI edits to it are carried, not reverted.
struct Residue {
    /// `Cl`/`LtE`/`WF` and [`GUI_OWNED_ATTRS`], raw values in element order.
    attrs: Vec<(String, String)>,
    /// [`GUI_OWNED_CHILDREN`] subtrees in element order.
    children: Vec<Element>,
    /// GUI-owned connectors (design decision D20): the whole `<Co>` of
    /// every `Inv=`-carrying connector, keyed by `K`, re-emitted verbatim
    /// at its spec position — inversion flag, `Def=`, and wires included.
    /// The IR is refused from wiring or setting these (see [`add_wire`] /
    /// [`apply_def`] and the param guard in the build loop), so carrying
    /// them can never contradict what the source expresses.
    gui_ports: BTreeMap<String, Element>,
    /// `FLG=` wire flags, keyed by (sink port UUID, source port UUID).
    /// Miniserver/app-created wire metadata: the oracle probe (2026-08-25)
    /// showed Loxone Config round-trips the flag verbatim, never
    /// regenerates it, and accepts its absence — so it is carried exactly
    /// like the element residue. A wire whose source changed no longer
    /// matches its key and is emitted plain, which the GUI accepts.
    wire_flags: BTreeMap<(String, String), String>,
}

fn harvest_residue(el: &Element) -> Residue {
    Residue {
        attrs: el
            .attrs
            .iter()
            .filter(|a| {
                matches!(a.name.as_str(), "Cl" | "LtE" | "WF")
                    || GUI_OWNED_ATTRS.contains(&a.name.as_str())
            })
            .map(|a| (a.name.clone(), a.value.clone()))
            .collect(),
        children: el
            .child_elements()
            .filter(|c| GUI_OWNED_CHILDREN.contains(&c.name.as_str()))
            .cloned()
            .collect(),
        wire_flags: el
            .child_elements()
            .filter(|c| c.name == "Co")
            .filter_map(|co| Some((co.attr("U")?.to_string(), co)))
            .flat_map(|(port, co)| {
                co.child_elements()
                    .filter(|i| i.name == "In")
                    .filter_map(move |i| {
                        Some((
                            (port.clone(), i.attr("Input")?.to_string()),
                            i.attr("FLG")?.to_string(),
                        ))
                    })
            })
            .collect(),
        gui_ports: el
            .child_elements()
            .filter(|c| c.name == "Co" && c.attr("Inv").is_some())
            .filter_map(|co| Some((co.attr_decoded("K")?.into_owned(), co.clone())))
            .collect(),
    }
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
    validate_ports(module)?;

    // --- Apply `moved` statements to the lock (identity surgery, in
    //     source). Idempotent: once the new slug carries the entry, the
    //     statement is done.
    for mv in module.moved() {
        match (
            lock.objects.contains_key(&mv.from),
            lock.objects.contains_key(&mv.to),
        ) {
            (true, false) => lock.rename_object(&mv.from, &mv.to)?,
            (true, true) => {
                return Err(Error::Compile(format!(
                    "moved `{from} -> {to}`: both slugs are in the lockfile — \
                     `{to}` already has its own identity; remove one entry first \
                     (Lockfile::remove_object)",
                    from = mv.from,
                    to = mv.to
                )));
            }
            (false, true) => {} // already applied — the statement is done
            (false, false) => {
                return Err(Error::Compile(format!(
                    "moved `{} -> {}`: neither slug is in the lockfile — nothing to rename",
                    mv.from, mv.to
                )));
            }
        }
    }

    // --- Sync check: the lock must not know managed blocks the source
    //     lost, unless the removal is authorized (per-slug via `removed`,
    //     or globally via allow_removals).
    let src_slugs: BTreeSet<String> = module.blocks().map(|b| b.slug.clone()).collect();
    let removed_slugs: BTreeSet<&str> = module.removed().map(|r| r.slug.as_str()).collect();
    if !opts.allow_removals
        && let Some(slug) = lock
            .objects
            .keys()
            .find(|s| !src_slugs.contains(*s) && !removed_slugs.contains(s.as_str()))
    {
        return Err(Error::Compile(format!(
            "managed block `{slug}` is in the lockfile but not in the source; \
             if the removal is intended, add `removed {slug}` to the module \
             (or compile with allow_removals); use `moved {slug} -> <new_slug>` \
             for a rename, or Lockfile::remove_object(\"{slug}\") to orphan it"
        )));
    }

    let mut doc = base.clone();
    lock.absorb_counters(doc.counters());

    // --- Resolve externs (against the untouched base).
    let extern_uuid = resolve_externs(&doc, module, lock)?;

    // --- Tear down our previous output: managed objects, extern wires,
    //     extern sets. What remains is exactly the Loxone-Config-owned state.
    //     Each removed element hands back its GUI-owned residue (D19) so
    //     the rebuild carries it forward verbatim.
    let mut residue: BTreeMap<String, Residue> = BTreeMap::new();
    for obj in lock.objects.values() {
        if let Some(el) = doc.remove_by_uuid(&obj.uuid) {
            residue.insert(obj.uuid.clone(), harvest_residue(&el));
        }
    }
    // Vanished slugs (authorized via `removed` or allow_removals) are now
    // deleted from the config; drop their identity too.
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
    //     and pinned identity for every one of them. Each block is pinned
    //     to a page: adopted blocks to the page they were drawn on, new
    //     blocks to the options' page (resolved lazily so a block-free
    //     module compiles against a page-less config).
    let default_page_uuid: Option<String> =
        doc.page_path(opts.page_title.as_deref()).and_then(|p| {
            doc.element_at(&p)
                .and_then(|el| el.attr("U"))
                .map(String::from)
        });
    let mut minter = Minter::new(opts.machine, opts.mint_time_unix);
    let mut managed: BTreeMap<String, PlannedBlock> = BTreeMap::new();
    let mut py_cursor = next_free_py(lock);
    for block in module.blocks() {
        // Type, port names, and directions were checked by validate_ports.
        let specs = builtin(&block.block_type).expect("validate_ports admitted the type");
        let keys: Vec<String> = specs.iter().map(|s| s.key.to_string()).collect();

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
                page_uuid: None,
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
        let page_uuid = match &entry.page_uuid {
            Some(u) => u.clone(),
            None => {
                let u = default_page_uuid.clone().ok_or_else(|| {
                    Error::Compile(match &opts.page_title {
                        Some(t) => format!("no <C Type=\"Page\"> titled `{t}` in the base config"),
                        None => "the base config has no <C Type=\"Page\"> to place blocks on"
                            .to_string(),
                    })
                })?;
                entry.page_uuid = Some(u.clone());
                u
            }
        };
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
                page_uuid,
            },
        );
    }

    // --- Def values on managed ports come from the blocks' parameter
    //     bindings (port assignment statements are for extern ports only —
    //     enforced by validate). `let` references resolve to their literal
    //     here.
    let resolve = |v: &Value| -> Result<String> { Ok(module.resolve_value(v)?.to_string()) };
    let mut managed_defs: BTreeMap<(String, String), String> = BTreeMap::new();
    let mut managed_attrs: BTreeMap<(String, String), String> = BTreeMap::new();
    for block in module.blocks() {
        let attrs = attr_params(&block.block_type);
        for (k, v) in block.params() {
            let target = if attrs.contains(&k) {
                &mut managed_attrs // element attribute, not a connector Def
            } else {
                &mut managed_defs
            };
            target.insert((block.slug.clone(), k.to_string()), resolve(v)?);
        }
    }

    // --- Build the managed <C> elements and append each to its pinned
    //     page. The index is taken once after teardown; appending children
    //     never shifts the recorded page paths.
    let page_index = doc.index();
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
        // D19: a block that existed in the base re-emits its GUI-owned
        // display state verbatim — including *absence* (some types lose
        // `WF` on a GUI save). Only a fresh mint writes the defaults.
        let res = residue.get(&plan.uuid);
        let carried = |name: &str| {
            res.and_then(|r| r.attrs.iter().find(|(n, _)| n == name))
                .map(|(_, v)| v.as_str())
        };
        match (res, carried("Cl")) {
            (Some(_), Some(v)) => el.set_attr_raw("Cl", v),
            (Some(_), None) => {}
            (None, _) => el.set_attr("Cl", "141,255,112"),
        }
        el.set_attr("Nio", &plan.keys.len().to_string());
        if let Some(v) = carried("LtE") {
            el.set_attr_raw("LtE", v);
        }
        match (res, carried("WF")) {
            (Some(_), Some(v)) => el.set_attr_raw("WF", v),
            (Some(_), None) => {}
            (None, _) => el.set_attr("WF", "147456"),
        }
        for name in attr_params(&block.block_type) {
            if let Some(v) = managed_attrs.get(&(block.slug.clone(), (*name).to_string())) {
                el.set_attr(name, v);
            }
        }
        // Every observed Formula= attribute travels with Valid="false"
        // (the GUI revalidates the expression on load).
        if block.block_type == "Formula"
            && managed_attrs.contains_key(&(block.slug.clone(), "Formula".to_string()))
        {
            el.set_attr("Valid", "false");
        }
        if let Some(r) = res {
            for (n, v) in &r.attrs {
                if !matches!(n.as_str(), "Cl" | "LtE" | "WF") {
                    el.set_attr_raw(n, v);
                }
            }
        }
        for key in &plan.keys {
            // A GUI-owned (`Inv=`) connector is re-emitted verbatim at its
            // spec position (D20); a parameter binding on it would be
            // silently overridden, so it is refused instead.
            if let Some(carried) = res.and_then(|r| r.gui_ports.get(key)) {
                if managed_defs.contains_key(&(block.slug.clone(), key.clone())) {
                    return Err(Error::Compile(format!(
                        "block `{}`: connector `{key}` carries the GUI's input \
                         inversion (`Inv=`) and is GUI-owned — its value cannot \
                         be set from source; change it in Loxone Config instead",
                        block.slug
                    )));
                }
                el.push_child(carried.clone());
                continue;
            }
            let mut co = Element::new("Co");
            co.set_attr("K", key);
            if let Some(def) = managed_defs.get(&(block.slug.clone(), key.clone())) {
                co.set_attr("Def", def);
            }
            co.set_attr("U", &plan.ports[key]);
            el.push_child(co);
        }
        if let Some(r) = res {
            for c in &r.children {
                el.push_child(c.clone());
            }
        }
        let page_path = page_index.by_uuid.get(&plan.page_uuid).ok_or_else(|| {
            Error::Compile(format!(
                "the page `{}` recorded for block `{}` no longer exists in the \
                 base config; re-pin it by editing the lockfile entry's page_uuid \
                 (or remove the block from the lock to place it afresh)",
                plan.page_uuid, block.slug
            ))
        })?;
        let page = doc.element_at_mut(page_path).expect("indexed path");
        if page.attr("Type") != Some("Page") {
            return Err(Error::Compile(format!(
                "object `{}` recorded as the page for block `{}` is a `{}`, not a Page",
                plan.page_uuid,
                block.slug,
                page.attr("Type").unwrap_or("?")
            )));
        }
        page.push_child(el);
    }

    // --- Wires: block argument bindings (sink = the declaring block) and
    //     `<-` statements (sink = an extern port), in source order.
    for (from_ref, to_ref) in module.wire_pairs() {
        let from = resolve_port(
            &doc,
            &managed,
            &extern_uuid,
            &from_ref.slug,
            &from_ref.port,
            PortDir::Output,
        )?;
        let to = resolve_port(
            &doc,
            &managed,
            &extern_uuid,
            &to_ref.slug,
            &to_ref.port,
            PortDir::Input,
        )?;
        let flg = residue.get(&to.owner_uuid).and_then(|r| {
            r.wire_flags
                .get(&(to.port_uuid.clone(), from.port_uuid.clone()))
        });
        add_wire(
            &mut doc,
            &to.owner_uuid,
            &to.port_uuid,
            &from.port_uuid,
            flg.map(String::as_str),
        )?;
        if to.is_extern {
            lock.extern_wires.push(LockedWire {
                from: from.port_uuid,
                to: to.port_uuid,
            });
        }
    }
    lock.extern_wires.sort();
    lock.extern_wires.dedup();

    // --- Port assignments (always extern ports — validate rejects managed
    //     targets).
    for s in module.sets() {
        let target = resolve_port(
            &doc,
            &managed,
            &extern_uuid,
            &s.target.slug,
            &s.target.port,
            PortDir::Param,
        )?;
        let value = resolve(&s.value)?;
        let original = apply_def(&mut doc, &target.owner_uuid, &target.port_uuid, &value)?;
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
    lock.target.semantic_fingerprint = Some(crate::diff::semantic_fingerprint(&doc));
    Ok(doc)
}

struct PlannedBlock {
    uuid: String,
    block_type: String,
    /// All port keys in connector-index order.
    keys: Vec<String>,
    ports: BTreeMap<String, String>,
    layout: Layout,
    /// UUID of the page the block is (re)built on.
    page_uuid: String,
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
                    spec: format!("{}({})", ext.block_type, ext.match_spec),
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
                    spec: format!("{}({})", ext.block_type, ext.match_spec),
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
            // Api connectors are wire-bidirectional (see PortDir::Api).
            PortDir::Output => matches!(dir, PortDir::Output | PortDir::Api),
            // Wire sinks accept inputs, params, and Api connectors; a Def
            // assignment does not (Api ports never carry Def= — evidence
            // on PortDir::Api).
            PortDir::Input => dir != PortDir::Output,
            PortDir::Param => !matches!(dir, PortDir::Output | PortDir::Api),
            PortDir::Api => unreachable!("no caller wants Api"),
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
            "extern `{slug}` has no port `{port}` in the base config; present ports: {}{}. \
             (v0 cannot mint ports for unverified types — wire or set it once in Loxone \
             Config so the connector exists.)",
            known
                .iter()
                .map(|p| p.key.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            suggest(port, known.iter().map(|p| p.key.as_str()))
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
/// `flg` re-emits a harvested `FLG=` wire flag verbatim (D19 residue).
fn add_wire(
    doc: &mut LoxoneDoc,
    owner_uuid: &str,
    to_port: &str,
    from_port: &str,
    flg: Option<&str>,
) -> Result<()> {
    let owner = find_c_mut(&mut doc.xml.root, owner_uuid)
        .ok_or_else(|| Error::Compile(format!("wire sink object `{owner_uuid}` not found")))?;
    let co = owner
        .child_elements_mut()
        .find(|c| c.name == "Co" && c.attr("U") == Some(to_port))
        .ok_or_else(|| Error::Compile(format!("wire sink port `{to_port}` not found")))?;
    if co.attr("Inv").is_some() {
        return Err(Error::Compile(format!(
            "wire sink connector `{}` carries the GUI's input inversion \
             (`Inv=`) and is GUI-owned — a declared wire into it would be \
             silently inverted; remove the inversion in Loxone Config first",
            co.attr_decoded("K").unwrap_or_default()
        )));
    }
    // A declared wire always ends up as the LAST <In> of its sink: an
    // already-present duplicate (first compile after adopt — the wire
    // exists in the base) is moved there instead of re-created, keeping
    // its attributes and making the first compile byte-identical to every
    // later one (which tears the wire down and re-appends it). `<In>`
    // order is semantically unordered — the GUI rewrites it on save.
    let existing = co.children.iter().position(|n| {
        matches!(n, crate::xml::Node::Element(e)
            if e.name == "In" && e.attr("Input") == Some(from_port))
    });
    if let Some(pos) = existing {
        let node = co.children.remove(pos);
        co.children.push(node);
    } else {
        let mut input = Element::new("In");
        input.set_attr("Input", from_port);
        if let Some(v) = flg {
            input.set_attr_raw("FLG", v);
        }
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
/// in Loxone's canonical position: after `Def`, before `U` (oracle save
/// 2026-08-25 rewrote a compiler-emitted `K,Nc,Def,U` to `K,Def,Nc,U`).
fn sync_nc(co: &mut Element) {
    let n = co.child_elements().filter(|c| c.name == "In").count();
    if n == 0 {
        co.remove_attr("Nc");
        if co.children.is_empty() {
            co.self_closing = true;
        }
    } else {
        set_attr_ordered(co, "Nc", &n.to_string(), &["U"]);
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
    if co.attr("Inv").is_some() {
        return Err(Error::Compile(format!(
            "set target connector `{}` carries the GUI's input inversion \
             (`Inv=`) and is GUI-owned — its value cannot be set from \
             source; change it in Loxone Config instead",
            co.attr_decoded("K").unwrap_or_default()
        )));
    }
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

/// First stacking position below every layout already in the lock, so new
/// blocks never land on top of existing ones.
fn next_free_py(lock: &Lockfile) -> i64 {
    lock.objects
        .values()
        .filter_map(|o| o.layout.map(|l| l.py2 + GAP))
        .max()
        .unwrap_or(GAP)
}
