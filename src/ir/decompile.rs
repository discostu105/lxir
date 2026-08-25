//! The decompiler: existing config → IR view.
//!
//! `decompile` answers "what is in this config?" in the language's own
//! terms. Objects whose type is in the managed set become `slug = Type(…)`
//! declarations — their `Def=` values and incoming wires as argument
//! bindings. In the default [`DecompileScope::Full`] view, every other
//! page object with connectors becomes an `extern` declaration, and every
//! wire between lifted objects is shown (wires into managed blocks in the
//! argument list, wires onto extern ports as `target.Port <- source.Port`).
//! Output is grouped by logic page: `# page: …` sections in the
//! single-module view, one module per page from [`decompile_pages`].
//!
//! The full view is for reading, not compiling: compiling it against the
//! same base would mint duplicates of the managed blocks and claim
//! ownership of every shown wire. [`DecompileScope::ManagedOnly`] restricts
//! the view to managed-type objects and what is wired to them — the
//! starting point for adopting existing logic.
//!
//! Honest limits of the view: parameters (`Def=`) of extern objects are
//! never lifted (a `target.Port = value` statement would claim ownership
//! of the value); wires between two unlifted periphery objects stay raw
//! (the view covers page logic, not the device tree); objects whose type
//! or port keys cannot be written as language identifiers stay raw and
//! their wires are not shown. The report counts what was not lifted.
//!
//! Match specs for externs prefer stability over readability:
//! `iname` when it is unique for the type (INames are locale-stable),
//! else `title` when unique, else the exact `uuid`.

use crate::connectors::{BUILTIN_TYPES, attr_params};
use crate::doc::{LoxoneDoc, ObjectSummary, ports};
use crate::error::Result;
use crate::ir::ast::{
    ArgItem, Binding, BindingKind, BlockDecl, ExternDecl, Item, MatchSpec, Module, PortRef, Value,
    WireDecl,
};
use std::collections::{BTreeMap, BTreeSet};

/// How much of the config the view lifts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecompileScope {
    /// Every page object with connectors: managed types as block
    /// declarations, everything else as `extern`s, plus every wire between
    /// lifted objects. The orientation view — and the default.
    Full,
    /// Only managed-type objects and the objects wired to them: the
    /// faithful subset of what the compiler could own, used as the
    /// starting point for adoption.
    ManagedOnly,
}

#[derive(Debug, Clone)]
pub struct DecompileOptions {
    /// Block types to lift into managed block declarations. Defaults to the
    /// verified builtin table, so a lifted block declaration is always one
    /// the compiler could rebuild.
    pub managed_types: BTreeSet<String>,
    pub scope: DecompileScope,
    /// Object UUIDs to treat as unmanaged even though their type is in
    /// `managed_types` (they can still appear as externs). `adopt` uses
    /// this for individual blocks whose rebuild would not be faithful.
    pub exclude: BTreeSet<String>,
}

impl Default for DecompileOptions {
    fn default() -> Self {
        DecompileOptions {
            managed_types: BUILTIN_TYPES.iter().map(|t| String::from(*t)).collect(),
            scope: DecompileScope::Full,
            exclude: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecompileReport {
    /// Objects lifted into managed block declarations.
    pub managed: usize,
    /// Objects lifted into `extern`s.
    pub externs: usize,
    /// Logic pages with lifted content: sections in the single-module
    /// view, files from [`decompile_pages`].
    pub pages: usize,
    /// Objects left as raw XML (not counting Document/Page containers) —
    /// the honest measure of what the view does not cover.
    pub raw_objects: usize,
}

/// One logic page's slice of the view, from [`decompile_pages`].
#[derive(Debug, Clone)]
pub struct PageModule {
    /// The page's display title as in the config.
    pub title: String,
    /// Slugified file-name stem, unique across the document's pages
    /// (`eg_buero` for "EG Büro").
    pub slug: String,
    /// A self-contained module: objects the page references but does not
    /// contain are declared as externs with a `# page: …` / `# periphery`
    /// origin comment.
    pub module: Module,
}

/// The whole config as one module, grouped into `# page: …` sections
/// (objects not on any logic page lead in a `# periphery` section).
pub fn decompile(doc: &LoxoneDoc, opts: &DecompileOptions) -> Result<(Module, DecompileReport)> {
    let lift = Lift::build(doc, opts);
    let module = lift.single_module();
    module.validate()?;
    Ok((module, lift.report()))
}

/// The config as one module per logic page (pages without lifted content
/// are skipped). Objects outside any page appear only as foreign externs
/// in the modules that reference them.
pub fn decompile_pages(
    doc: &LoxoneDoc,
    opts: &DecompileOptions,
) -> Result<(Vec<PageModule>, DecompileReport)> {
    let lift = Lift::build(doc, opts);
    let pages = lift.page_modules()?;
    Ok((pages, lift.report()))
}

/// Everything both output shapes need, computed once: which objects are
/// lifted (and as what), their slugs, their page, and the lifted wires.
/// `pub(super)` because `adopt` builds on the same lift.
pub(super) struct Lift {
    pub(super) objects: Vec<ObjectSummary>,
    /// Page titles, document order — all pages, with or without content.
    page_titles: Vec<String>,
    /// Page UUIDs, parallel to `page_titles`.
    pub(super) page_uuids: Vec<String>,
    /// Object UUID → index into `page_titles` (absent: not on any page).
    page_of: BTreeMap<String, usize>,
    /// Object UUID → slug, one namespace across the whole document, and
    /// the reverse direction (slug → object index).
    pub(super) slug_of: BTreeMap<String, String>,
    obj_of_slug: BTreeMap<String, usize>,
    /// Object indexes lifted as managed blocks / externs, emission order.
    pub(super) managed: Vec<usize>,
    pub(super) externs: Vec<usize>,
    /// Prebuilt declarations, keyed by object index. `match_specs` also
    /// covers managed objects, for foreign-extern declarations in the
    /// per-page view.
    extern_decls: BTreeMap<usize, ExternDecl>,
    block_decls: BTreeMap<usize, BlockDecl>,
    pub(super) match_specs: BTreeMap<usize, MatchSpec>,
    /// `<-` statements with the sink and source object indexes,
    /// deduplicated, document order.
    wires: Vec<(WireDecl, usize, usize)>,
}

/// One page's (or the periphery's) share of the lifted items, as indexes
/// into the [`Lift`] tables.
#[derive(Default)]
struct Bucket {
    externs: Vec<usize>,
    blocks: Vec<usize>,
    wires: Vec<usize>,
}

impl Lift {
    pub(super) fn build(doc: &LoxoneDoc, opts: &DecompileOptions) -> Lift {
        let objects = doc.objects();
        let idx = doc.index();
        let obj_index: BTreeMap<&str, usize> = objects
            .iter()
            .enumerate()
            .map(|(i, o)| (o.uuid.as_str(), i))
            .collect();

        // Pages in document order; an object belongs to its deepest
        // ancestor page (by element-path prefix).
        let mut page_paths: Vec<&[usize]> = Vec::new();
        let mut page_titles: Vec<String> = Vec::new();
        let mut page_uuids: Vec<String> = Vec::new();
        for o in &objects {
            if o.block_type == "Page" {
                page_paths.push(&o.path);
                page_titles.push(o.title.clone().unwrap_or_else(|| "Page".into()));
                page_uuids.push(o.uuid.clone());
            }
        }
        let mut page_of: BTreeMap<String, usize> = BTreeMap::new();
        for o in &objects {
            if matches!(o.block_type.as_str(), "Document" | "Page") {
                continue;
            }
            let mut best: Option<usize> = None;
            for (p, pp) in page_paths.iter().enumerate() {
                if pp.len() < o.path.len()
                    && o.path[..pp.len()] == **pp
                    && best.is_none_or(|b| page_paths[b].len() < pp.len())
                {
                    best = Some(p);
                }
            }
            if let Some(p) = best {
                page_of.insert(o.uuid.clone(), p);
            }
        }

        // Seed: managed-type objects; in the full view also every page
        // object with connectors and a language-writable type.
        let mut lifted = vec![false; objects.len()];
        let mut managed = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            if opts.managed_types.contains(&o.block_type) && !opts.exclude.contains(&o.uuid) {
                lifted[i] = true;
                managed.push(i);
            }
        }
        let mut externs = Vec::new();
        if opts.scope == DecompileScope::Full {
            for (i, o) in objects.iter().enumerate() {
                if !lifted[i]
                    && !matches!(o.block_type.as_str(), "Document" | "Page")
                    && page_of.contains_key(&o.uuid)
                    && is_ident(&o.block_type)
                    && !ports(doc.element_at(&o.path).expect("path from objects()")).is_empty()
                {
                    lifted[i] = true;
                    externs.push(i);
                }
            }
        }

        // Wires touching the seed pull their other endpoint in as an
        // extern. Depth 1 on purpose: an extern lifted here does not pull
        // in *its* other wires, so periphery-to-periphery wiring stays out.
        let seed = lifted.clone();
        let mut seen = BTreeSet::new();
        let mut touched: Vec<(usize, String, usize, String)> = Vec::new();
        for w in doc.wires() {
            let (Some((fo, fk)), Some((to, tk))) = (
                idx.port_owner.get(&w.from_port),
                idx.port_owner.get(&w.to_port),
            ) else {
                continue; // dangling <In> — not representable, stays raw
            };
            let (fi, ti) = (obj_index[fo.as_str()], obj_index[to.as_str()]);
            if !(seed[fi] || seed[ti]) {
                continue;
            }
            if !is_ident(fk)
                || !is_ident(tk)
                || !is_ident(&objects[fi].block_type)
                || !is_ident(&objects[ti].block_type)
            {
                continue; // not representable, stays raw
            }
            if idx.inv_ports.contains(&w.to_port) {
                // Wire into a GUI-owned (`Inv=`) connector: the inversion
                // would silently negate a lifted statement, so the wire is
                // GUI content — carried verbatim by the rebuild (managed
                // sink) or left untouched in the base (extern sink), and
                // its source is not pulled in as an extern.
                continue;
            }
            if !seen.insert((w.from_port.clone(), w.to_port.clone())) {
                continue;
            }
            for i in [fi, ti] {
                if !lifted[i] {
                    lifted[i] = true;
                    externs.push(i);
                }
            }
            touched.push((fi, fk.clone(), ti, tk.clone()));
        }

        // Slug assignment: managed first (document order), then externs
        // (seed order, then order of first wire reference), all in one
        // namespace.
        let mut slugs = SlugTable::default();
        let mut slug_of: BTreeMap<String, String> = BTreeMap::new();
        let mut obj_of_slug: BTreeMap<String, usize> = BTreeMap::new();
        for &i in managed.iter().chain(&externs) {
            let o = &objects[i];
            let base = o.title.as_deref().or(o.iname.as_deref());
            let slug = slugs.assign(base.unwrap_or(&o.block_type));
            obj_of_slug.insert(slug.clone(), i);
            slug_of.insert(o.uuid.clone(), slug);
        }

        // Match specs for every lifted object (managed ones may be needed
        // as foreign externs in the per-page view).
        let match_spec = |i: usize| -> MatchSpec {
            let obj = &objects[i];
            let unique = |get: fn(&ObjectSummary) -> Option<&str>| {
                get(obj).filter(|v| {
                    objects
                        .iter()
                        .filter(|o| o.block_type == obj.block_type && get(o) == Some(v))
                        .count()
                        == 1
                })
            };
            if let Some(iname) = unique(|o| o.iname.as_deref()) {
                MatchSpec::IName(iname.to_string())
            } else if let Some(title) = unique(|o| o.title.as_deref()) {
                MatchSpec::Title(title.to_string())
            } else {
                MatchSpec::Uuid(obj.uuid.clone())
            }
        };
        let match_specs: BTreeMap<usize, MatchSpec> = managed
            .iter()
            .chain(&externs)
            .map(|&i| (i, match_spec(i)))
            .collect();
        let extern_decls: BTreeMap<usize, ExternDecl> = externs
            .iter()
            .map(|&i| {
                (
                    i,
                    ExternDecl {
                        room: None,
                        category: None,
                        slug: slug_of[&objects[i].uuid].clone(),
                        block_type: objects[i].block_type.clone(),
                        match_spec: match_specs[&i].clone(),
                        comment: None,
                    },
                )
            })
            .collect();

        // Blocks: per port in connector order, the `Def=` value becomes a
        // parameter binding, then each incoming wire a wire binding.
        let mut block_decls: BTreeMap<usize, BlockDecl> = BTreeMap::new();
        for &i in &managed {
            let o = &objects[i];
            let el = doc.element_at(&o.path).expect("path from objects()");
            let mut args = Vec::new();
            // Attribute parameters (block logic stored as an element
            // attribute, e.g. `Formula=`) lead the argument list.
            for name in attr_params(&o.block_type) {
                if let Some(v) = el.attr_decoded(name) {
                    args.push(ArgItem::Binding(Binding {
                        port: (*name).to_string(),
                        kind: BindingKind::Param(Value::from_literal(&v)),
                        comment: None,
                    }));
                }
            }
            for p in ports(el) {
                if !is_ident(&p.key) {
                    continue; // not representable, stays raw
                }
                if p.inv {
                    // GUI-owned connector (D20): its Def and wires are
                    // carried verbatim by the rebuild, never restated in
                    // source — the Inv flag would silently invert them.
                    continue;
                }
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
                    if !is_ident(src_key) {
                        continue;
                    }
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
            block_decls.insert(
                i,
                BlockDecl {
                    block_type: o.block_type.clone(),
                    // A title identical to the slug is what `compile`
                    // defaults to — dropping it keeps the view minimal.
                    title: o.title.clone().filter(|t| t != &slug),
                    slug,
                    args,
                    comment: None,
                    close_comment: None,
                },
            );
        }

        // `<-` statements: wires with an extern sink. Wires into managed
        // blocks were lifted into the blocks' argument lists above. The
        // check is per *instance*, not per type: an excluded managed-type
        // object (adopt refusal) is an extern here, and its incoming wires
        // must appear as `<-` statements, not vanish.
        let is_managed: BTreeSet<usize> = managed.iter().copied().collect();
        let mut wires = Vec::new();
        for (fi, fk, ti, tk) in touched {
            if is_managed.contains(&ti) {
                continue;
            }
            wires.push((
                WireDecl {
                    to: PortRef {
                        slug: slug_of[&objects[ti].uuid].clone(),
                        port: tk,
                    },
                    from: PortRef {
                        slug: slug_of[&objects[fi].uuid].clone(),
                        port: fk,
                    },
                    comment: None,
                },
                ti,
                fi,
            ));
        }

        Lift {
            objects,
            page_titles,
            page_uuids,
            page_of,
            slug_of,
            obj_of_slug,
            managed,
            externs,
            extern_decls,
            block_decls,
            match_specs,
            wires,
        }
    }

    pub(super) fn page_of(&self, obj: usize) -> Option<usize> {
        self.page_of.get(&self.objects[obj].uuid).copied()
    }

    /// Lifted items grouped by page, in document order; `None` (objects on
    /// no page — the periphery) sorts first. Only non-empty groups. A wire
    /// belongs to its sink's page, falling back to its source's.
    fn buckets(&self) -> Vec<(Option<usize>, Bucket)> {
        let mut map: BTreeMap<Option<usize>, Bucket> = BTreeMap::new();
        for &i in &self.externs {
            map.entry(self.page_of(i)).or_default().externs.push(i);
        }
        for &i in &self.managed {
            map.entry(self.page_of(i)).or_default().blocks.push(i);
        }
        for (wi, &(_, ti, fi)) in self.wires.iter().enumerate() {
            let page = self.page_of(ti).or_else(|| self.page_of(fi));
            map.entry(page).or_default().wires.push(wi);
        }
        map.into_iter().collect()
    }

    pub(super) fn single_module(&self) -> Module {
        let mut items = Vec::new();
        for (page, b) in self.buckets() {
            items.extend(self.bucket_items(page, &b));
        }
        Module { items }
    }

    fn bucket_items(&self, page: Option<usize>, b: &Bucket) -> Vec<Item> {
        let mut items = vec![Item::Comment(match page {
            Some(p) => format!(" page: {}", self.page_titles[p]),
            None => " periphery (not placed on a page)".to_string(),
        })];
        for &i in &b.externs {
            items.push(Item::Extern(self.extern_decls[&i].clone()));
        }
        for &i in &b.blocks {
            items.push(Item::Block(self.block_decls[&i].clone()));
        }
        for &wi in &b.wires {
            items.push(Item::Wire(self.wires[wi].0.clone()));
        }
        items
    }

    /// The lifted view as module-directory fragments: one `(file stem,
    /// fragment)` per non-empty page bucket, the periphery leading as
    /// `_periphery` (its underscore also sorts it first in the merge
    /// order). Unlike [`Lift::page_modules`]'s self-contained modules,
    /// the fragments share one namespace — concatenated in this order
    /// they are exactly [`Lift::single_module`], so declarations appear
    /// once and cross-file references resolve on the merged whole
    /// (`Module::parse_fragment` semantics).
    pub(super) fn fragment_modules(&self) -> Vec<(String, Module)> {
        // File stems are assigned over ALL pages in document order, so a
        // page gaining content never renames another page's file.
        let mut names = SlugTable::default();
        let file_slugs: Vec<String> = self.page_titles.iter().map(|t| names.assign(t)).collect();
        self.buckets()
            .into_iter()
            .map(|(page, b)| {
                let stem = match page {
                    Some(p) => file_slugs[p].clone(),
                    None => "_periphery".to_string(),
                };
                (
                    stem,
                    Module {
                        items: self.bucket_items(page, &b),
                    },
                )
            })
            .collect()
    }

    fn page_modules(&self) -> Result<Vec<PageModule>> {
        // File slugs are assigned over ALL pages in document order, so a
        // page gaining content never renames another page's file.
        let mut names = SlugTable::default();
        let file_slugs: Vec<String> = self.page_titles.iter().map(|t| names.assign(t)).collect();

        let mut out = Vec::new();
        for (page, b) in self.buckets() {
            let Some(p) = page else {
                // Periphery objects appear only as foreign externs in the
                // modules that reference them.
                continue;
            };
            let mut items: Vec<Item> = b
                .externs
                .iter()
                .map(|&i| Item::Extern(self.extern_decls[&i].clone()))
                .collect();

            // Referenced objects the page does not contain become foreign
            // externs, annotated with where they live.
            let mut declared: BTreeSet<&str> = b
                .externs
                .iter()
                .chain(&b.blocks)
                .map(|&i| self.slug_of[&self.objects[i].uuid].as_str())
                .collect();
            let mut foreign: Vec<usize> = Vec::new();
            {
                let mut reference = |slug: &str| {
                    if !declared.contains(slug) {
                        foreign.push(self.obj_of_slug[slug]);
                    }
                };
                for &i in &b.blocks {
                    for (_, src) in self.block_decls[&i].input_wires() {
                        reference(&src.slug);
                    }
                }
                for &wi in &b.wires {
                    let w = &self.wires[wi].0;
                    reference(&w.from.slug);
                    reference(&w.to.slug);
                }
            }
            let mut seen: BTreeSet<usize> = BTreeSet::new();
            for i in foreign {
                if !seen.insert(i) {
                    continue;
                }
                let origin = match self.page_of(i) {
                    Some(op) => format!(" page: {}", self.page_titles[op]),
                    None => " periphery".to_string(),
                };
                let mut decl = self.extern_decls.get(&i).cloned().unwrap_or_else(|| {
                    // A managed block on another page: referenced here as
                    // an extern.
                    ExternDecl {
                        room: None,
                        category: None,
                        slug: self.slug_of[&self.objects[i].uuid].clone(),
                        block_type: self.objects[i].block_type.clone(),
                        match_spec: self.match_specs[&i].clone(),
                        comment: None,
                    }
                });
                decl.comment = Some(origin);
                items.push(Item::Extern(decl));
                declared.insert(self.slug_of[&self.objects[i].uuid].as_str());
            }

            for &i in &b.blocks {
                items.push(Item::Block(self.block_decls[&i].clone()));
            }
            for &wi in &b.wires {
                items.push(Item::Wire(self.wires[wi].0.clone()));
            }

            let module = Module { items };
            module.validate()?;
            out.push(PageModule {
                title: self.page_titles[p].clone(),
                slug: file_slugs[p].clone(),
                module,
            });
        }
        Ok(out)
    }

    pub(super) fn report(&self) -> DecompileReport {
        DecompileReport {
            managed: self.managed.len(),
            externs: self.externs.len(),
            pages: self.buckets().iter().filter(|(p, _)| p.is_some()).count(),
            raw_objects: self
                .objects
                .iter()
                .filter(|o| !matches!(o.block_type.as_str(), "Document" | "Page"))
                .filter(|o| !self.slug_of.contains_key(&o.uuid))
                .count(),
        }
    }
}

/// Whether `s` can be written as a language identifier
/// (`[A-Za-z][A-Za-z0-9_]*`) — required for lifted types and port keys.
fn is_ident(s: &str) -> bool {
    match s.as_bytes().first() {
        Some(b) if b.is_ascii_alphabetic() => {
            s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        }
        _ => false,
    }
}

/// Statement-initial keywords (v1, plus reserved v0 words). A slug
/// colliding with one would change the meaning of the line it starts, so
/// [`SlugTable`] never hands them out.
const RESERVED: &[&str] = &["let", "extern", "removed", "moved", "block", "wire", "set"];

/// Slug generation with umlaut transliteration and `_2`-style
/// deduplication; statement keywords are pre-claimed.
struct SlugTable {
    used: BTreeSet<String>,
}

impl Default for SlugTable {
    fn default() -> Self {
        SlugTable {
            used: RESERVED.iter().map(|s| s.to_string()).collect(),
        }
    }
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

    #[test]
    fn statement_keywords_never_become_slugs() {
        // A block titled "Set" must not produce `set = …` — that line
        // would parse as a (reserved) keyword statement.
        let mut t = SlugTable::default();
        assert_eq!(t.assign("Set"), "set_2");
        assert_eq!(t.assign("Wire"), "wire_2");
        assert_eq!(t.assign("Let"), "let_2");
    }

    #[test]
    fn ident_rule() {
        assert!(is_ident("AutoShade"));
        assert!(is_ident("Q"));
        assert!(is_ident("I1"));
        assert!(!is_ident("2"), "numeric type ids are not writable");
        assert!(!is_ident("174"));
        assert!(!is_ident(""));
        assert!(!is_ident("Türkontakt"), "non-ASCII stays raw");
    }
}
