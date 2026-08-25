//! The decompiler: existing config → IR module.
//!
//! Import path for adopting an existing config: objects whose type is in the
//! managed set become `slug = Type(…)` declarations — with their `Def=`
//! values and incoming wires as argument bindings; objects *wired to them*
//! become `extern` declarations; wires landing on extern ports become
//! `target.Port <- source.Port` statements. Everything else stays untouched
//! raw XML (counted in the report).
//!
//! Match specs for externs prefer stability over readability:
//! `iname` when it is unique for the type (INames are locale-stable),
//! else `title` when unique, else the exact `uuid`.
//!
//! Note: only wires touching a *managed* object are lifted. A wire between
//! two externs — even one the compiler itself created — belongs to the
//! config, not the IR view, so `decompile(compile(m))` is a faithful subset
//! of `m`, not necessarily all of it.

use crate::connectors::BUILTIN_TYPES;
use crate::doc::{LoxoneDoc, ports};
use crate::error::Result;
use crate::ir::ast::{
    ArgItem, Binding, BindingKind, BlockDecl, ExternDecl, Item, MatchSpec, Module, PortRef, Value,
    WireDecl,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct DecompileOptions {
    /// Block types to lift into managed block declarations. Defaults to the
    /// verified builtin table, so `compile(decompile(doc))` can always
    /// rebuild what was lifted.
    pub managed_types: BTreeSet<String>,
}

impl Default for DecompileOptions {
    fn default() -> Self {
        DecompileOptions {
            managed_types: BUILTIN_TYPES.iter().map(|t| String::from(*t)).collect(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecompileReport {
    /// Objects lifted into managed block declarations.
    pub managed: usize,
    /// Objects referenced by managed wires, lifted into `extern`s.
    pub externs: usize,
    /// Logic-page objects left as raw XML (not counting Document/Page
    /// containers) — the honest measure of what the IR does not yet cover.
    pub raw_objects: usize,
}

pub fn decompile(doc: &LoxoneDoc, opts: &DecompileOptions) -> Result<(Module, DecompileReport)> {
    let objects = doc.objects();
    let idx = doc.index();

    // Managed objects, in document order.
    let managed: Vec<_> = objects
        .iter()
        .filter(|o| opts.managed_types.contains(&o.block_type))
        .collect();
    let managed_uuids: BTreeSet<&str> = managed.iter().map(|o| o.uuid.as_str()).collect();

    // Wires that touch a managed object (via the port-owner index).
    let mut touched: Vec<(String, String, String, String)> = Vec::new(); // (from_obj, from_key, to_obj, to_key)
    for w in doc.wires() {
        let (Some((fo, fk)), Some((to, tk))) = (
            idx.port_owner.get(&w.from_port),
            idx.port_owner.get(&w.to_port),
        ) else {
            continue;
        };
        if managed_uuids.contains(fo.as_str()) || managed_uuids.contains(to.as_str()) {
            touched.push((fo.clone(), fk.clone(), to.clone(), tk.clone()));
        }
    }

    // Slug assignment: managed first (document order), then externs
    // (order of first reference), all deduplicated in one namespace.
    let mut slugs = SlugTable::default();
    let mut slug_of: BTreeMap<String, String> = BTreeMap::new(); // uuid → slug
    for o in &managed {
        let base = o.title.as_deref().or(o.iname.as_deref());
        let slug = slugs.assign(base.unwrap_or(&o.block_type));
        slug_of.insert(o.uuid.clone(), slug);
    }
    let mut extern_order: Vec<String> = Vec::new();
    for (fo, _, to, _) in &touched {
        for uuid in [fo, to] {
            if !managed_uuids.contains(uuid.as_str()) && !slug_of.contains_key(uuid) {
                let obj = objects.iter().find(|o| &o.uuid == uuid).expect("indexed");
                let base = obj.title.as_deref().or(obj.iname.as_deref());
                let slug = slugs.assign(base.unwrap_or(&obj.block_type));
                slug_of.insert(uuid.clone(), slug);
                extern_order.push(uuid.clone());
            }
        }
    }

    let mut items = Vec::new();

    // Externs, with the most stable unique match spec available.
    for uuid in &extern_order {
        let obj = objects.iter().find(|o| &o.uuid == uuid).expect("indexed");
        let unique = |get: fn(&crate::doc::ObjectSummary) -> Option<&str>| {
            get(obj).filter(|v| {
                objects
                    .iter()
                    .filter(|o| o.block_type == obj.block_type && get(o) == Some(v))
                    .count()
                    == 1
            })
        };
        let match_spec = if let Some(iname) = unique(|o| o.iname.as_deref()) {
            MatchSpec::IName(iname.to_string())
        } else if let Some(title) = unique(|o| o.title.as_deref()) {
            MatchSpec::Title(title.to_string())
        } else {
            MatchSpec::Uuid(obj.uuid.clone())
        };
        items.push(Item::Extern(ExternDecl {
            slug: slug_of[uuid].clone(),
            block_type: obj.block_type.clone(),
            match_spec,
            comment: None,
        }));
    }

    // Blocks: per port in connector order, the `Def=` value becomes a
    // parameter binding, then each incoming wire a wire binding.
    for o in &managed {
        let el = doc.element_at(&o.path).expect("path from objects()");
        let mut args = Vec::new();
        for p in ports(el) {
            if let Some(def) = &p.def {
                args.push(ArgItem::Binding(Binding {
                    port: p.key.clone(),
                    kind: BindingKind::Param(Value::from_literal(def)),
                    comment: None,
                }));
            }
            for input in &p.inputs {
                let Some((src_obj, src_key)) = idx.port_owner.get(input) else {
                    continue; // dangling <In> — not representable, stays raw
                };
                let Some(src_slug) = slug_of.get(src_obj) else {
                    continue;
                };
                args.push(ArgItem::Binding(Binding {
                    port: p.key.clone(),
                    kind: BindingKind::Wire(PortRef {
                        slug: src_slug.clone(),
                        port: src_key.clone(),
                    }),
                    comment: None,
                }));
            }
        }
        let slug = slug_of[&o.uuid].clone();
        items.push(Item::Block(BlockDecl {
            block_type: o.block_type.clone(),
            // A title identical to the slug is what `compile` defaults to —
            // dropping it keeps decompile(compile(m)) minimal.
            title: o.title.clone().filter(|t| t != &slug),
            slug,
            args,
            comment: None,
            close_comment: None,
        }));
    }

    // Wires landing on extern ports (`<-` statements), deduplicated, in
    // document order. Wires into managed blocks were lifted into the
    // blocks' argument lists above.
    let mut seen = BTreeSet::new();
    for (fo, fk, to, tk) in &touched {
        if managed_uuids.contains(to.as_str()) {
            continue;
        }
        let wire = WireDecl {
            to: PortRef {
                slug: slug_of[to].clone(),
                port: tk.clone(),
            },
            from: PortRef {
                slug: slug_of[fo].clone(),
                port: fk.clone(),
            },
            comment: None,
        };
        if seen.insert((wire.from.clone(), wire.to.clone())) {
            items.push(Item::Wire(wire));
        }
    }

    let module = Module { items };
    module.validate()?;
    let report = DecompileReport {
        managed: managed.len(),
        externs: extern_order.len(),
        raw_objects: objects
            .iter()
            .filter(|o| !matches!(o.block_type.as_str(), "Document" | "Page"))
            .filter(|o| !slug_of.contains_key(&o.uuid))
            .count(),
    };
    Ok((module, report))
}

/// Slug generation with umlaut transliteration and `_2`-style deduplication.
#[derive(Default)]
struct SlugTable {
    used: BTreeSet<String>,
}

impl SlugTable {
    fn assign(&mut self, name: &str) -> String {
        let base = slugify(name);
        let mut candidate = base.clone();
        let mut n = 1;
        while !self.used.insert(candidate.clone()) {
            n += 1;
            candidate = format!("{base}_{n}");
        }
        candidate
    }
}

/// Turn a display title into a valid IR slug (`[a-z][a-z0-9_]*`).
/// German umlauts transliterate (`ü`→`ue`); everything else non-alphanumeric
/// collapses to `_`.
pub fn slugify(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for c in name.chars() {
        match c {
            'ä' | 'Ä' => out.push_str("ae"),
            'ö' | 'Ö' => out.push_str("oe"),
            'ü' | 'Ü' => out.push_str("ue"),
            'ß' => out.push_str("ss"),
            c if c.is_ascii_alphanumeric() => out.push(c.to_ascii_lowercase()),
            _ => {
                if !out.ends_with('_') && !out.is_empty() {
                    out.push('_');
                }
            }
        }
    }
    let out = out.trim_matches('_').to_string();
    match out.chars().next() {
        Some(c) if c.is_ascii_lowercase() => out,
        Some(_) => format!("x{out}"),
        None => "x".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_rules() {
        assert_eq!(slugify("Beschattung Süd"), "beschattung_sued");
        assert_eq!(slugify("Temp über 28"), "temp_ueber_28");
        assert_eq!(slugify("3. OG Licht"), "x3_og_licht");
        assert_eq!(slugify("Größe"), "groesse");
        assert_eq!(slugify("---"), "x");
    }

    #[test]
    fn slug_table_dedupes() {
        let mut t = SlugTable::default();
        assert_eq!(t.assign("Or"), "or");
        assert_eq!(t.assign("Or"), "or_2");
        assert_eq!(t.assign("Or"), "or_3");
    }
}
