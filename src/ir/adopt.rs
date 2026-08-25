//! Adoption: move existing config objects under source control.
//!
//! `adopt` is decompile-with-lock: the same lift that produces the
//! managed-only view also knows every lifted object's UUID, port UUIDs,
//! layout, and page — exactly what a lockfile pins. Compiling the returned
//! module with the returned lock therefore *rebuilds the existing blocks in
//! place* instead of minting duplicates: same object UUIDs, same port UUIDs
//! (so every wire drawn in Loxone Config keeps pointing at them), same
//! position on the same page.
//!
//! Identity comes from the lift, never from matching: adopting by title
//! would be ambiguous in real configs (duplicate titles are common), and
//! there is nothing to match — the object in front of us *is* the identity.
//!
//! The safety property is verification, not translation: the compiler
//! rebuilds managed blocks from scratch, so anything on the original
//! element that the rebuild does not reproduce would be silently lost on
//! the first compile. [`verify_rebuildable`] whitelists exactly what the
//! rebuild emits (plus known-harmless normalizations: `WF=` is rewritten to
//! the value Loxone Config itself normalizes to on save, `LtE=` and the
//! block color are display state). A block that fails verification is
//! *skipped, not translated*: it stays unmanaged (appearing as a pinned
//! extern where adopted logic wires to it) and the reason lands in
//! [`AdoptReport::refused`] — one bespoke GUI flag never blocks adopting
//! the rest of the house.

use crate::connectors::{attr_params, builtin};
use crate::doc::{LoxoneDoc, ObjectSummary, ports};
use crate::error::Result;
use crate::ir::ast::{MatchSpec, Module};
use crate::ir::decompile::{DecompileOptions, DecompileScope, Lift};
use crate::ir::validate::validate_ports;
use crate::lock::{Layout, LockedExternal, LockedObject, Lockfile, sha256_hex};
use crate::xml::{Element, Node};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdoptReport {
    /// Existing objects now managed (block declarations + lock entries).
    pub blocks: usize,
    /// Objects wired to them, declared `extern` and pinned in the lock.
    pub externs: usize,
    /// Logic pages the adopted blocks live on.
    pub pages: usize,
    /// Managed-type blocks that stayed unmanaged, with the reason each
    /// one's rebuild would not have been faithful.
    pub refused: Vec<String>,
}

/// Adopt every managed-type object in `doc` whose rebuild is verified
/// faithful: returns the managed-only module, a fresh lockfile pinning each
/// block's existing identity (object UUID, port UUIDs, layout, page) and
/// each extern's resolution, and a report. `compile(doc, module, lock)` is
/// then a semantic no-op.
pub fn adopt(doc: &LoxoneDoc) -> Result<(Module, Lockfile, AdoptReport)> {
    let mut opts = DecompileOptions {
        scope: DecompileScope::ManagedOnly,
        ..DecompileOptions::default()
    };
    let mut lift = Lift::build(doc, &opts);

    // Verification pass: refused blocks are excluded and the lift rebuilt
    // without them (they may re-enter as externs). One pass suffices —
    // verification is element-local, so exclusions cannot invalidate other
    // blocks.
    let mut refused = Vec::new();
    for &i in &lift.managed {
        let o = &lift.objects[i];
        let el = doc.element_at(&o.path).expect("path from objects()");
        let why = verify_rebuildable(el, o).err().or_else(|| {
            lift.page_of(i)
                .is_none()
                .then(|| "it is not placed on a logic page".to_string())
        });
        if let Some(why) = why {
            refused.push(format!(
                "cannot adopt {} \"{}\" (uuid {}): {why}; the block stays unmanaged",
                o.block_type,
                o.title.as_deref().unwrap_or(""),
                o.uuid
            ));
            opts.exclude.insert(o.uuid.clone());
        }
    }
    if !opts.exclude.is_empty() {
        lift = Lift::build(doc, &opts);
    }

    let module = lift.single_module();
    module.validate()?;
    validate_ports(&module)?;

    let mut lock = Lockfile::new();
    for &i in &lift.managed {
        let o = &lift.objects[i];
        let el = doc.element_at(&o.path).expect("path from objects()");
        let page = lift.page_of(i).expect("verified to be on a page");
        lock.objects.insert(
            lift.slug_of[&o.uuid].clone(),
            LockedObject {
                uuid: o.uuid.clone(),
                block_type: o.block_type.clone(),
                ports: ports(el).into_iter().map(|p| (p.key, p.uuid)).collect(),
                layout: Some(layout_of(el).expect("verified numeric")),
                page_uuid: Some(lift.page_uuids[page].clone()),
            },
        );
    }
    for &i in &lift.externs {
        let o = &lift.objects[i];
        lock.externals.insert(
            lift.slug_of[&o.uuid].clone(),
            LockedExternal {
                uuid: o.uuid.clone(),
                matched_by: match &lift.match_specs[&i] {
                    MatchSpec::Uuid(_) => "uuid",
                    MatchSpec::IName(_) => "iname",
                    MatchSpec::Title(_) => "title",
                }
                .to_string(),
                title_at_match: o.title.clone(),
                iname_at_match: o.iname.clone(),
            },
        );
    }
    // Counters stay 0: the first compile absorbs the document's. The
    // serial stays unset for the same reason (it is a compile option).
    lock.target.config_version = doc.config_version();
    lock.target.source_config_sha256 = Some(sha256_hex(&doc.to_bytes()));

    let d = lift.report();
    Ok((
        module,
        lock,
        AdoptReport {
            blocks: d.managed,
            externs: d.externs,
            pages: d.pages,
            refused,
        },
    ))
}

/// Element attributes the compiler's rebuild emits (or knowingly
/// normalizes: `WF` is rewritten to the save-normalized value, `LtE` and
/// `Cl` are display state).
const KNOWN_ATTRS: &[&str] = &[
    "Type", "V", "U", "Title", "Px", "Py", "Px2", "Py2", "Cl", "Nio", "WF", "LtE",
];

/// `Err(reason)` unless rebuilding this block from its lifted declaration
/// reproduces everything on the element (modulo the documented
/// normalizations).
fn verify_rebuildable(el: &Element, o: &ObjectSummary) -> std::result::Result<(), String> {
    let attrs = attr_params(&o.block_type);
    for a in &el.attrs {
        let known = KNOWN_ATTRS.contains(&a.name.as_str())
            || attrs.contains(&a.name.as_str())
            // Validity state travels with attribute parameters (observed:
            // Valid="false" on every Formula=); the rebuild re-emits it.
            || (a.name == "Valid" && !attrs.is_empty());
        if !known {
            return Err(format!(
                "attribute `{}=\"{}\"` is not understood by the rebuild and would be lost",
                a.name, a.value
            ));
        }
    }
    if el.attr("V") != Some("175") {
        return Err(format!(
            "block version V=\"{}\" differs from the verified \"175\"",
            el.attr("V").unwrap_or("")
        ));
    }
    if layout_of(el).is_none() {
        return Err("its position (Px/Py/Px2/Py2) is not numeric".to_string());
    }

    for child in &el.children {
        let co = match child {
            Node::Element(c) if c.name == "Co" => c,
            Node::Element(c) => {
                return Err(format!(
                    "child element <{}> is not understood by the rebuild",
                    c.name
                ));
            }
            Node::Text(t) if t.trim().is_empty() => continue,
            Node::Text(_) => return Err("unexpected text content".to_string()),
        };
        let key = co.attr_decoded("K").unwrap_or_default().into_owned();
        for a in &co.attrs {
            if !matches!(a.name.as_str(), "K" | "Nc" | "Def" | "U") {
                let hint = if a.name == "Inv" {
                    " (the GUI's input-inversion flag; invert via a Not block \
                     in Loxone Config first)"
                } else {
                    ""
                };
                return Err(format!(
                    "connector `{key}`: attribute `{}` is not understood{hint}",
                    a.name
                ));
            }
        }
        let mut in_count: u64 = 0;
        for child in &co.children {
            match child {
                Node::Element(i) if i.name == "In" => {
                    if let Some(a) = i.attrs.iter().find(|a| a.name != "Input") {
                        return Err(format!(
                            "connector `{key}`: wire attribute `{}` is not understood",
                            a.name
                        ));
                    }
                    in_count += 1;
                }
                Node::Element(c) => {
                    return Err(format!(
                        "connector `{key}`: child element <{}> is not understood",
                        c.name
                    ));
                }
                Node::Text(t) if t.trim().is_empty() => {}
                Node::Text(_) => {
                    return Err(format!("connector `{key}`: unexpected text content"));
                }
            }
        }
        let nc: u64 = co.attr("Nc").unwrap_or("0").parse().unwrap_or(u64::MAX);
        if nc != in_count {
            return Err(format!(
                "connector `{key}`: Nc=\"{}\" does not match its {in_count} wire(s)",
                co.attr("Nc").unwrap_or("")
            ));
        }
    }

    // The connector SET must equal the spec's (order is cosmetic — the
    // port-UUID index tails prove the spec order is the creation order,
    // Loxone Config just serializes elements in GUI order sometimes).
    let specs = builtin(&o.block_type).expect("lift only manages builtin types");
    let spec_keys: BTreeSet<&str> = specs.iter().map(|s| s.key).collect();
    let real_keys: BTreeSet<String> = ports(el).into_iter().map(|p| p.key).collect();
    let real_keys_ref: BTreeSet<&str> = real_keys.iter().map(String::as_str).collect();
    if real_keys_ref != spec_keys {
        return Err(format!(
            "its connectors [{}] differ from the verified [{}]",
            real_keys_ref.iter().copied().collect::<Vec<_>>().join(", "),
            specs.iter().map(|s| s.key).collect::<Vec<_>>().join(", ")
        ));
    }
    if el.attr("Nio") != Some(specs.len().to_string().as_str()) {
        return Err(format!(
            "Nio=\"{}\" does not match the {} verified connectors",
            el.attr("Nio").unwrap_or(""),
            specs.len()
        ));
    }
    Ok(())
}

fn layout_of(el: &Element) -> Option<Layout> {
    let coord = |name: &str| el.attr(name)?.parse().ok();
    Some(Layout {
        px: coord("Px")?,
        py: coord("Py")?,
        px2: coord("Px2")?,
        py2: coord("Py2")?,
    })
}
