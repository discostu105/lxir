//! End-to-end IR pipeline tests over the shipped example fixture:
//! determinism, lockfile identity, teardown/restore semantics, and the
//! decompile view of compiled output.

use lxir::ir::{
    CompileOptions, DecompileOptions, DecompileScope, Item, Module, adopt, adopt_pages, compile,
    decompile, decompile_pages,
};
use lxir::uuid::parse_serial;
use lxir::xml::{Attr, Element};
use lxir::{Lockfile, LoxoneDoc};

const MINT_TIME: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z

fn base() -> LoxoneDoc {
    LoxoneDoc::parse(&std::fs::read("examples/configs/haus.Loxone").unwrap()).unwrap()
}

fn module() -> Module {
    Module::parse(&std::fs::read_to_string("examples/ir/beschattung.lxir").unwrap()).unwrap()
}

fn opts() -> CompileOptions {
    CompileOptions {
        machine: parse_serial("504F94112233").unwrap(),
        mint_time_unix: MINT_TIME,
        page_title: Some("Beschattung".into()),
        allow_removals: false,
        accept_version: None,
    }
}

#[test]
fn compile_is_deterministic_and_lock_pins_identity() {
    let base = base();
    let module = module();

    // Fresh lock, twice: identical bytes and identical locks.
    let mut lock_a = Lockfile::new();
    let out_a = compile(&base, &module, &mut lock_a, &opts()).unwrap();
    let mut lock_b = Lockfile::new();
    let out_b = compile(&base, &module, &mut lock_b, &opts()).unwrap();
    assert_eq!(out_a.to_bytes(), out_b.to_bytes());
    assert_eq!(lock_a.to_json(), lock_b.to_json());

    // Recompiling with the produced lock changes nothing.
    let out_c = compile(&base, &module, &mut lock_a.clone(), &opts()).unwrap();
    assert_eq!(out_a.to_bytes(), out_c.to_bytes());

    // Even with a different mint time: every UUID comes from the lock.
    let late = CompileOptions {
        mint_time_unix: MINT_TIME + 999_999,
        ..opts()
    };
    let out_d = compile(&base, &module, &mut lock_a.clone(), &late).unwrap();
    assert_eq!(out_a.to_bytes(), out_d.to_bytes());

    // Compiled output itself roundtrips byte-identically.
    let bytes = out_a.to_bytes();
    assert_eq!(LoxoneDoc::parse(&bytes).unwrap().to_bytes(), bytes);
}

#[test]
fn recompiling_own_output_converges() {
    // Simulate the real workflow: compile, upload, download the (identical)
    // config, compile again against it. The compiler must tear down exactly
    // its own edits and rebuild them — a fixpoint.
    let module = module();
    let mut lock = Lockfile::new();
    let first = compile(&base(), &module, &mut lock, &opts()).unwrap();
    let second = compile(&first, &module, &mut lock, &opts()).unwrap();
    assert_eq!(first.to_bytes(), second.to_bytes());
}

#[test]
fn counters_advance_once_per_minted_object_and_never_decrease() {
    let base = base();
    assert_eq!(base.counters().next_obj, 200);
    let mut lock = Lockfile::new();
    let out = compile(&base, &module(), &mut lock, &opts()).unwrap();
    // Two managed blocks minted → NextObj 200 + 2.
    assert_eq!(out.counters().next_obj, 202);
    // Recompile: nothing new minted.
    let out2 = compile(&base, &module(), &mut lock, &opts()).unwrap();
    assert_eq!(out2.counters().next_obj, 202);
}

#[test]
fn vanished_slug_is_a_hard_error_until_removed_from_lock() {
    let base = base();
    let mut lock = Lockfile::new();
    compile(&base, &module(), &mut lock, &opts()).unwrap();

    let smaller = Module::parse(
        "extern jal_sued = AutoJalousie(title: \"Beschattung S\u{fc}d\")\n\
         temp_hoch = GreaterEqual()\n",
    )
    .unwrap();
    let err = compile(&base, &smaller, &mut lock, &opts()).unwrap_err();
    assert!(err.to_string().contains("beschatten"), "{err}");
    assert!(err.to_string().contains("allow_removals"), "{err}");

    // Forgetting via remove_object also satisfies the sync check.
    lock.remove_object("beschatten").unwrap();
    let out = compile(&base, &smaller, &mut lock, &opts()).unwrap();
    assert!(!out.objects().iter().any(|o| o.block_type == "And"));
}

#[test]
fn removing_set_restores_original_def_and_wires_tear_down() {
    let base = base();
    let mut lock = Lockfile::new();
    let with_edits = compile(&base, &module(), &mut lock, &opts()).unwrap();

    // TargetPos rewritten 100 → 70; two wires landed on the extern.
    let jal = |doc: &LoxoneDoc| {
        let objs = doc.objects();
        let o = objs
            .iter()
            .find(|o| o.block_type == "AutoJalousie")
            .unwrap();
        lxir::doc::ports(doc.element_at(&o.path).unwrap())
    };
    let target = |doc: &LoxoneDoc| {
        jal(doc)
            .iter()
            .find(|p| p.key == "TargetPos")
            .unwrap()
            .def
            .clone()
    };
    assert_eq!(target(&with_edits).as_deref(), Some("70"));
    let wired: usize = jal(&with_edits).iter().map(|p| p.inputs.len()).sum();
    assert_eq!(wired, 2);

    // Drop everything from source. Without allow_removals this must refuse;
    // with it, the compiler deletes its blocks and reverts its edits.
    let empty = Module::parse("").unwrap();
    assert!(compile(&with_edits, &empty, &mut lock.clone(), &opts()).is_err());
    let destroy = CompileOptions {
        allow_removals: true,
        ..opts()
    };
    let reverted = compile(&with_edits, &empty, &mut lock, &destroy).unwrap();
    assert_eq!(
        target(&reverted).as_deref(),
        Some("100"),
        "original Def restored"
    );
    let wired: usize = jal(&reverted).iter().map(|p| p.inputs.len()).sum();
    assert_eq!(wired, 0, "our extern wires removed");
    assert!(lock.set_originals.is_empty());
    assert!(lock.extern_wires.is_empty());
    assert!(lock.objects.is_empty(), "removed blocks dropped from lock");
    // Managed blocks are gone again; only the base objects remain.
    assert_eq!(reverted.objects().len(), base.objects().len());
}

#[test]
fn ambiguity_and_no_match_are_reported() {
    let base = base();
    let mut lock = Lockfile::new();
    let nomatch = Module::parse("extern x = VirtualIn(iname: \"VI99\")\n").unwrap();
    let err = compile(&base, &nomatch, &mut lock, &opts()).unwrap_err();
    assert!(err.to_string().contains("VI99"), "{err}");

    // Titles are not unique across VirtualIns? In the fixture they are —
    // match on a type instead where two objects share nothing: use a title
    // that exists twice by matching VirtualIn Qm... simplest ambiguous spec:
    // both And/Or absent → keep to NoMatch here; ambiguity is covered by
    // the unit-level resolver behavior below.
    let unknown_port = Module::parse(
        "extern jal = AutoJalousie(title: \"Beschattung S\u{fc}d\")\n\
         b = And()\n\
         jal.DoesNotExist <- b.Q\n",
    )
    .unwrap();
    let err = compile(&base, &unknown_port, &mut Lockfile::new(), &opts()).unwrap_err();
    assert!(err.to_string().contains("DoesNotExist"), "{err}");
    assert!(
        err.to_string().contains("InputTrigger"),
        "lists available ports: {err}"
    );
}

#[test]
fn unverified_block_type_is_refused() {
    let m = Module::parse("t = Irrigation()\n").unwrap();
    let err = compile(&base(), &m, &mut Lockfile::new(), &opts()).unwrap_err();
    assert!(err.to_string().contains("builtin table"), "{err}");
}

#[test]
fn config_version_pin_refuses_an_unqualified_release() {
    let base = base(); // ConfigVersion="17010727"
    let module = module();
    let mut lock = Lockfile::new();
    compile(&base, &module, &mut lock, &opts()).unwrap();
    assert_eq!(lock.target.config_version.as_deref(), Some("17010727"));

    // A new Loxone release rewrites the base's ConfigVersion.
    let text = String::from_utf8(base.to_bytes()).unwrap();
    let bumped = LoxoneDoc::parse(
        text.replace("ConfigVersion=\"17010727\"", "ConfigVersion=\"17020101\"")
            .as_bytes(),
    )
    .unwrap();
    let err = compile(&bumped, &module, &mut lock.clone(), &opts()).unwrap_err();
    assert!(err.to_string().contains("ConfigVersion"), "{err}");
    assert!(err.to_string().contains("--accept-version"), "{err}");

    // Acceptance must name the base's version exactly.
    let wrong = CompileOptions {
        accept_version: Some("17039999".into()),
        ..opts()
    };
    let err = compile(&bumped, &module, &mut lock.clone(), &wrong).unwrap_err();
    assert!(err.to_string().contains("does not match"), "{err}");

    // Exact acceptance compiles and re-pins; the next plain compile
    // against the new release is quiet again.
    let accept = CompileOptions {
        accept_version: Some("17020101".into()),
        ..opts()
    };
    compile(&bumped, &module, &mut lock, &accept).unwrap();
    assert_eq!(lock.target.config_version.as_deref(), Some("17020101"));
    compile(&bumped, &module, &mut lock, &opts()).unwrap();
}

#[test]
fn grown_gate_inputs_are_refused() {
    // Oracle-verified (docs/oracle-wine.md): Loxone Config 17 silently
    // deletes off-descriptor connectors on save. A grown `I3` must be a
    // compile error, never minted.
    let m = Module::parse(
        "extern t1 = VirtualIn(iname: \"VI1\")\n\
         extern t2 = VirtualIn(iname: \"VI2\")\n\
         extern t3 = VirtualIn(iname: \"VI3\")\n\
         any = Or(I1: t1.Q, I2: t2.Q, I3: t3.Q)\n",
    )
    .unwrap();
    let err = compile(&base(), &m, &mut Lockfile::new(), &opts()).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("unknown port `I3`"), "{msg}");
    assert!(msg.contains("cascade"), "{msg}");
}

#[test]
fn decompile_of_compiled_output_reflects_the_module() {
    let mut lock = Lockfile::new();
    let out = compile(&base(), &module(), &mut lock, &opts()).unwrap();
    let managed_only = DecompileOptions {
        scope: DecompileScope::ManagedOnly,
        ..Default::default()
    };
    let (m, report) = decompile(&out, &managed_only).unwrap();
    // The fixture's AutoJalousie is a managed type since its admission, so
    // the view lifts it as a block (adopt, with verification, would refuse
    // this partial old-generation instance — but views don't verify).
    assert_eq!(report.managed, 3);
    assert_eq!(report.externs, 3);
    assert_eq!(m.blocks().count(), 3);
    // Every wire lands in a block argument list — the AutoShade and Safety
    // sinks are on the lifted AutoJalousie now.
    assert_eq!(m.wire_pairs().len(), 5);
    assert_eq!(m.extern_wires().count(), 0);
    // Everything the compiler owns sits on the one page it compiled onto.
    assert_eq!(report.pages, 1);
    // The IR text parses back to the same module (canonical fixpoint).
    let text = m.to_text();
    assert_eq!(Module::parse(&text).unwrap(), m);
}

#[test]
fn full_view_decompile_shows_every_page_block_and_wire() {
    let mut lock = Lockfile::new();
    let out = compile(&base(), &module(), &mut lock, &opts()).unwrap();
    let (m, report) = decompile(&out, &DecompileOptions::default()).unwrap();
    // The AutoJalousie lifts as a managed block since its admission.
    assert_eq!(report.managed, 3);
    // The three VirtualIns (periphery).
    assert_eq!(report.externs, 3);
    // Only the VirtualInCaption container stays raw.
    assert_eq!(report.raw_objects, 1);
    assert_eq!(report.pages, 1);
    // Every wire sinks on a lifted block now — nothing needs `<-`.
    assert_eq!(m.extern_wires().count(), 0);
    assert_eq!(m.wire_pairs().len(), 5);
    // Output is grouped into page sections.
    assert!(
        m.items
            .iter()
            .any(|i| matches!(i, Item::Comment(c) if c.contains("page: Beschattung")))
    );
    assert!(
        m.items
            .iter()
            .any(|i| matches!(i, Item::Comment(c) if c.contains("periphery")))
    );
    // The view is still canonical, parseable language text.
    let text = m.to_text();
    assert_eq!(Module::parse(&text).unwrap(), m);
}

#[test]
fn per_page_decompile_produces_self_contained_modules() {
    let mut lock = Lockfile::new();
    let out = compile(&base(), &module(), &mut lock, &opts()).unwrap();
    let (pages, report) = decompile_pages(&out, &DecompileOptions::default()).unwrap();
    assert_eq!(report.pages, 1);
    assert_eq!(pages.len(), 1);
    let p = &pages[0];
    assert_eq!(p.title, "Beschattung");
    assert_eq!(p.slug, "beschattung");
    // Self-contained: the periphery VirtualIns the page references are
    // declared as foreign externs with an origin note.
    let m = &p.module;
    assert_eq!(m.blocks().count(), 3);
    assert_eq!(m.externs().count(), 3);
    assert_eq!(
        m.externs()
            .filter(|e| e.comment.as_deref() == Some(" periphery"))
            .count(),
        3
    );
    assert_eq!(m.extern_wires().count(), 0);
    // Each page module is canonical, parseable language text.
    let text = m.to_text();
    assert_eq!(&Module::parse(&text).unwrap(), m);
}

/// A module exercising everything adoption must preserve: a Formula with
/// an attribute parameter, a comparator fed by it, and a wire onto an
/// extern port. Compiling it yields the "existing config" the adopt tests
/// then treat as GUI-authored.
const ADOPT_SRC: &str = "\
extern aussentemp = VirtualIn(iname: \"VI1\")
extern sonne = VirtualIn(iname: \"VI3\")
extern jal_sued = AutoJalousie(title: \"Beschattung S\u{fc}d\")

summe = Formula(
\t\"Summe\",
\tFormula: \"I1+I2\",
\tInput1: aussentemp.Q,
\tInput2: sonne.Q,
)
heiss = GreaterEqual(
\tInput1: summe.AQ,
\tInput2: 50,
)

jal_sued.AutoShade <- heiss.Q
";

fn adopted_config() -> LoxoneDoc {
    let m = Module::parse(ADOPT_SRC).unwrap();
    compile(&base(), &m, &mut Lockfile::new(), &opts()).unwrap()
}

#[test]
fn adopt_pages_fragments_merge_to_the_single_module() {
    // `adopt --out-dir` writes these fragments; concatenated in stem
    // order they must be exactly the single-module adoption, with the
    // identical lock — so both output shapes compile byte-identically.
    let existing = adopted_config();
    let (single, lock_a, _) = adopt(&existing).unwrap();
    let (fragments, lock_b, _) = adopt_pages(&existing).unwrap();
    assert_eq!(lock_a.to_json(), lock_b.to_json());
    let stems: Vec<&str> = fragments.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(stems, ["_periphery", "beschattung"]);
    let merged = Module {
        items: fragments
            .iter()
            .flat_map(|(_, m)| m.items.iter().cloned())
            .collect(),
    };
    assert_eq!(merged, single);
    // Each fragment is parseable in fragment form (a lone fragment may
    // cross-reference sibling slugs, so full parse is not required).
    for (stem, m) in &fragments {
        let re =
            Module::parse_fragment(&m.to_text()).unwrap_or_else(|e| panic!("fragment {stem}: {e}"));
        assert_eq!(&re, m, "fragment {stem} round-trips");
    }
}

#[test]
fn adopt_then_compile_rebuilds_in_place() {
    let existing = adopted_config();
    let (module, mut lock, report) = adopt(&existing).unwrap();
    assert_eq!(report.blocks, 2);
    assert_eq!(report.pages, 1);

    // The fixture's partial old-generation AutoJalousie (4 of 49
    // connectors) refuses — its rebuild would mint the missing 45.
    assert_eq!(report.refused.len(), 1, "{:?}", report.refused);
    assert!(
        report.refused[0].contains("AutoJalousie"),
        "{:?}",
        report.refused
    );

    // The lock pins the blocks' existing identities.
    let objs = existing.objects();
    let formula = objs.iter().find(|o| o.block_type == "Formula").unwrap();
    assert_eq!(lock.objects["summe"].uuid, formula.uuid);
    let page = objs.iter().find(|o| o.block_type == "Page").unwrap();
    assert_eq!(
        lock.objects["summe"].page_uuid.as_deref(),
        Some(&*page.uuid)
    );

    // The lifted view carries the attribute parameter.
    assert!(module.to_text().contains("Formula: \"I1+I2\""));

    // Compiling the pair is a no-op: nothing minted, nothing moved —
    // byte-identical, since the "existing" config is our own output shape.
    let out = compile(&existing, &module, &mut lock, &opts()).unwrap();
    assert!(lxir::diff::diff(&existing, &out).is_empty());
    assert_eq!(out.counters().next_obj, existing.counters().next_obj);
    assert_eq!(out.to_bytes(), existing.to_bytes());
}

#[test]
fn fingerprint_is_recorded_and_survives_the_rebuild() {
    let existing = adopted_config();
    let (module, mut lock, _) = adopt(&existing).unwrap();
    let recorded = lock.target.semantic_fingerprint.clone().unwrap();
    assert_eq!(recorded, lxir::diff::semantic_fingerprint(&existing));

    // The rebuild is a semantic no-op, so compile re-records the same
    // baseline — `lxir drift` stays green across the whole cycle.
    let out = compile(&existing, &module, &mut lock, &opts()).unwrap();
    assert_eq!(
        lock.target.semantic_fingerprint.as_deref(),
        Some(&*recorded)
    );
    assert_eq!(lxir::diff::semantic_fingerprint(&out), recorded);

    // Any real edit moves it (here: a changed Def value).
    let changed_src = ADOPT_SRC.replace("Input2: 50", "Input2: 51");
    let changed = compile(
        &base(),
        &Module::parse(&changed_src).unwrap(),
        &mut Lockfile::new(),
        &opts(),
    )
    .unwrap();
    assert_ne!(lxir::diff::semantic_fingerprint(&changed), recorded);
}

#[test]
fn formula_attribute_parameter_compiles_and_diffs() {
    let existing = adopted_config();
    let objs = existing.objects();
    let f = objs.iter().find(|o| o.block_type == "Formula").unwrap();
    let el = existing.element_at(&f.path).unwrap();
    assert_eq!(el.attr("Formula"), Some("I1+I2"));
    assert_eq!(el.attr("Valid"), Some("false"));

    // A changed formula is a visible param change, not "semantically empty".
    let changed_src = ADOPT_SRC.replace("I1+I2", "I1*I2");
    let changed = compile(
        &base(),
        &Module::parse(&changed_src).unwrap(),
        &mut Lockfile::new(),
        &opts(),
    )
    .unwrap();
    let d = lxir::diff::diff(&existing, &changed);
    assert_eq!(d.param_changes.len(), 1, "{d:?}");
    assert_eq!(d.param_changes[0].port_key, "Formula");
    assert_eq!(d.param_changes[0].to.as_deref(), Some("I1*I2"));
}

#[test]
fn adopt_skips_blocks_the_rebuild_would_not_reproduce() {
    // An element attribute the rebuild does not emit would be lost: the
    // block is skipped with a reason, the rest of the config still adopts.
    let mut existing = adopted_config();
    let path = existing
        .objects()
        .iter()
        .find(|o| o.block_type == "Formula")
        .unwrap()
        .path
        .clone();
    existing.element_at_mut(&path).unwrap().set_attr("M", "3");
    let (module, lock, report) = adopt(&existing).unwrap();
    // One refusal for the M= attribute (plus the fixture's standing
    // partial-AutoJalousie refusal).
    let refused: Vec<&String> = report
        .refused
        .iter()
        .filter(|r| !r.contains("AutoJalousie"))
        .collect();
    assert_eq!(refused.len(), 1, "{:?}", report.refused);
    assert!(refused[0].contains("attribute `M="), "{}", refused[0]);
    assert!(refused[0].contains("Summe"), "{}", refused[0]);
    assert_eq!(report.blocks, 1, "the GreaterEqual still adopts");
    assert!(!lock.objects.contains_key("summe"));
    assert!(lock.objects.contains_key("heiss"));
    // The refused Formula stays visible as a pinned extern (heiss wires
    // from its AQ), and compiling the pair is still a no-op.
    assert!(module.externs().any(|e| e.slug == "summe"));
    assert!(lock.externals.contains_key("summe"));
    let out = compile(&existing, &module, &mut lock.clone(), &opts()).unwrap();
    assert!(lxir::diff::diff(&existing, &out).is_empty());
}

#[test]
fn adopt_carries_gui_owned_inv_connectors_verbatim() {
    // An `Inv=`-carrying connector is GUI-owned (D20): the block still
    // adopts, the connector's wire stays out of the source, and the
    // rebuild re-emits the whole <Co> verbatim — flag and wire included.
    let mut existing = adopted_config();
    let path = existing
        .objects()
        .iter()
        .find(|o| o.block_type == "GreaterEqual")
        .unwrap()
        .path
        .clone();
    existing
        .element_at_mut(&path)
        .unwrap()
        .child_elements_mut()
        .find(|c| c.name == "Co")
        .unwrap()
        .set_attr("Inv", "true");
    let (module, mut lock, report) = adopt(&existing).unwrap();
    // Only the fixture's standing partial-AutoJalousie refusal remains.
    assert_eq!(report.refused.len(), 1, "{:?}", report.refused);
    assert!(report.refused[0].contains("AutoJalousie"));
    assert!(lock.objects.contains_key("heiss"), "the block still adopts");
    // The inverted Input1 and its wire are GUI content now — absent from
    // the source (restating the wire would silently invert its meaning);
    // the untouched Input2 parameter is still lifted.
    let text = module.to_text();
    assert!(!text.contains("Input1: summe.AQ"), "{text}");
    assert!(text.contains("Input2: 50"), "{text}");
    // The rebuild is still a semantic no-op: the carried <Co> brings the
    // flag and the wire back verbatim.
    let out = compile(&existing, &module, &mut lock, &opts()).unwrap();
    assert!(lxir::diff::diff(&existing, &out).is_empty());
    let heiss = out
        .objects()
        .into_iter()
        .find(|o| o.block_type == "GreaterEqual")
        .unwrap();
    let co = out
        .element_at(&heiss.path)
        .unwrap()
        .child_elements()
        .find(|c| c.name == "Co")
        .unwrap();
    assert_eq!(co.attr("Inv"), Some("true"));
    assert_eq!(co.attr("Nc"), Some("1"), "the GUI wire survives");

    // Touching a GUI-owned connector from source is refused, both as a
    // wire sink and as a parameter binding.
    let wire_src = "\
extern sonne = VirtualIn(iname: \"VI3\")
extern heiss = GreaterEqual(title: \"heiss\")
heiss.Input1 <- sonne.Q
";
    let m = Module::parse(wire_src).unwrap();
    let err = compile(&existing, &m, &mut Lockfile::new(), &opts())
        .unwrap_err()
        .to_string();
    assert!(err.contains("Inv"), "{err}");
    assert!(err.contains("GUI-owned"), "{err}");
    let (module, mut lock, _) = adopt(&existing).unwrap();
    let mut text = module.to_text();
    text = text.replace("\tInput2: 50,", "\tInput1: 5,\n\tInput2: 50,");
    let m = Module::parse(&text).unwrap();
    let err = compile(&existing, &m, &mut lock, &opts())
        .unwrap_err()
        .to_string();
    assert!(err.contains("Inv"), "{err}");
    assert!(err.contains("cannot be set"), "{err}");
}

#[test]
fn adopt_carries_gui_owned_residue_verbatim() {
    // Dress the compiled Formula up like a real GUI-authored block:
    // custom color, LtE, a display attribute, and visualization children
    // — including the GUI's NON-self-closing <IoData></IoData> form,
    // which must survive as-is.
    let mut existing = adopted_config();
    let path = existing
        .objects()
        .iter()
        .find(|o| o.block_type == "Formula")
        .unwrap()
        .path
        .clone();
    let el = existing.element_at_mut(&path).unwrap();
    el.set_attr("Cl", "238,238,238");
    let wf = el.attrs.iter().position(|a| a.name == "WF").unwrap();
    el.attrs.insert(
        wf,
        Attr {
            name: "LtE".into(),
            value: "495646462".into(),
        },
    );
    el.set_attr("Tp", "2"); // display attrs trail the element
    let mut io = Element::new("IoData");
    io.set_attr("Pr", "room-uuid");
    io.self_closing = false;
    el.push_child(io);
    let mut display = Element::new("Display");
    display.set_attr("Unit", "<v.1>");
    el.push_child(display);

    // Adoption accepts the residue, and the rebuild reproduces the config
    // byte for byte — values, order, escaping, self-closing form. (The
    // fixture's standing partial-AutoJalousie refusal is not about us.)
    let (module, lock, report) = adopt(&existing).unwrap();
    assert!(
        report.refused.iter().all(|r| r.contains("AutoJalousie")),
        "{:?}",
        report.refused
    );
    let out = compile(&existing, &module, &mut lock.clone(), &opts()).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.to_bytes()),
        String::from_utf8_lossy(&existing.to_bytes())
    );

    // Carried forward, not snapshotted: a later GUI edit to the residue
    // survives the next compile instead of being reverted.
    let mut edited = out;
    let path = edited
        .objects()
        .iter()
        .find(|o| o.block_type == "Formula")
        .unwrap()
        .path
        .clone();
    edited
        .element_at_mut(&path)
        .unwrap()
        .child_elements_mut()
        .find(|c| c.name == "Display")
        .unwrap()
        .set_attr("Unit", "°C");
    let out2 = compile(&edited, &module, &mut lock.clone(), &opts()).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out2.to_bytes()),
        String::from_utf8_lossy(&edited.to_bytes())
    );
}

#[test]
fn adopt_carries_wire_flags_verbatim() {
    // FLG= is Miniserver/app-created wire metadata the oracle probe showed
    // to be inert stored state: dress a managed block's incoming wire with
    // it and the round-trip must reproduce it byte for byte.
    let mut existing = adopted_config();
    let path = existing
        .objects()
        .iter()
        .find(|o| o.block_type == "GreaterEqual")
        .unwrap()
        .path
        .clone();
    existing
        .element_at_mut(&path)
        .unwrap()
        .child_elements_mut()
        .find(|c| c.name == "Co" && c.attr("Nc").is_some())
        .expect("the fixture wires into the GreaterEqual")
        .child_elements_mut()
        .find(|i| i.name == "In")
        .unwrap()
        .set_attr("FLG", "1");

    let (module, lock, report) = adopt(&existing).unwrap();
    assert!(
        report.refused.iter().all(|r| r.contains("AutoJalousie")),
        "{:?}",
        report.refused
    );
    let out = compile(&existing, &module, &mut lock.clone(), &opts()).unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.to_bytes()),
        String::from_utf8_lossy(&existing.to_bytes())
    );
}

#[test]
fn wire_direction_is_checked_on_managed_blocks() {
    let m = Module::parse("a = And()\nb = And(I2: a.I1)\n").unwrap();
    let err = compile(&base(), &m, &mut Lockfile::new(), &opts()).unwrap_err();
    assert!(err.to_string().contains("wire source"), "{err}");
}

#[test]
fn removed_statement_authorizes_scoped_removal() {
    let base = base();
    let mut lock = Lockfile::new();
    let full = compile(&base, &module(), &mut lock, &opts()).unwrap();

    // Drop `beschatten` from source with an explicit `removed` — no
    // allow_removals needed, and the removal is scoped to that one slug.
    let without = Module::parse(
        "extern aussentemp = VirtualIn(iname: \"VI1\")\n\
         temp_hoch = GreaterEqual(\n\
         \t\"Temp \u{fc}ber 28\",\n\
         \tInput1: aussentemp.Q,\n\
         \tInput2: 28,\n\
         )\n\
         removed beschatten\n",
    )
    .unwrap();
    let out = compile(&full, &without, &mut lock, &opts()).unwrap();
    assert!(!out.objects().iter().any(|o| o.block_type == "And"));
    assert!(!lock.objects.contains_key("beschatten"));
    assert!(lock.objects.contains_key("temp_hoch"), "others survive");

    // The stale `removed` is a no-op: recompiling is still a fixpoint.
    let again = compile(&out, &without, &mut lock, &opts()).unwrap();
    assert_eq!(out.to_bytes(), again.to_bytes());

    // A `removed` authorizes exactly its slug — another vanished slug still
    // refuses, and the error suggests the statement.
    let mut lock2 = Lockfile::new();
    compile(&base, &module(), &mut lock2, &opts()).unwrap();
    let only_one = Module::parse("removed beschatten\n").unwrap();
    let err = compile(&base, &only_one, &mut lock2, &opts()).unwrap_err();
    assert!(err.to_string().contains("`temp_hoch`"), "{err}");
    assert!(err.to_string().contains("removed temp_hoch"), "{err}");
}

#[test]
fn moved_statement_renames_identity() {
    let base = base();
    let mut lock = Lockfile::new();
    let first = compile(&base, &module(), &mut lock, &opts()).unwrap();
    let old = lock.objects["beschatten"].clone();

    let renamed_src = "\
extern aussentemp = VirtualIn(iname: \"VI1\")
extern wind_alarm = VirtualIn(iname: \"VI2\")
extern sonne = VirtualIn(iname: \"VI3\")
extern jal_sued = AutoJalousie(title: \"Beschattung S\u{fc}d\")

temp_hoch = GreaterEqual(
\t\"Temp \u{fc}ber 28\",
\tInput1: aussentemp.Q,
\tInput2: 28,
)
schatten_gate = And(
\tI1: temp_hoch.Q,
\tI2: sonne.Q,
)

jal_sued.AutoShade <- schatten_gate.Q
jal_sued.Safety <- wind_alarm.Q

jal_sued.TargetPos = 70

moved beschatten -> schatten_gate
";
    let renamed = Module::parse(renamed_src).unwrap();
    let second = compile(&first, &renamed, &mut lock, &opts()).unwrap();

    // Identity survived the rename: same object and port UUIDs under the
    // new slug; the only semantic change is the display title (which
    // defaults to the slug).
    assert!(!lock.objects.contains_key("beschatten"));
    let new = &lock.objects["schatten_gate"];
    assert_eq!(new.uuid, old.uuid);
    assert_eq!(new.ports, old.ports);
    let d = lxir::diff::diff(&first, &second);
    assert!(d.added.is_empty() && d.removed.is_empty(), "no re-mint");
    assert!(d.wires_added.is_empty() && d.wires_removed.is_empty());
    assert_eq!(d.renamed.len(), 1, "only the title changed");

    // Idempotent: the applied `moved` is a no-op on the next compile.
    let third = compile(&second, &renamed, &mut lock, &opts()).unwrap();
    assert_eq!(second.to_bytes(), third.to_bytes());

    // With no matching lock entry at all, `moved` is an error (typo guard).
    let err = compile(&base, &renamed, &mut Lockfile::new(), &opts()).unwrap_err();
    assert!(err.to_string().contains("neither slug"), "{err}");
}

#[test]
fn let_constants_resolve_to_defs() {
    let with_let = Module::parse(
        "let schwelle = 28\n\
         extern aussentemp = VirtualIn(iname: \"VI1\")\n\
         temp_hoch = GreaterEqual(Input1: aussentemp.Q, Input2: schwelle)\n\
         aussentemp.Qm = schwelle\n",
    )
    .unwrap();
    let literal = Module::parse(
        "extern aussentemp = VirtualIn(iname: \"VI1\")\n\
         temp_hoch = GreaterEqual(Input1: aussentemp.Q, Input2: 28)\n\
         aussentemp.Qm = 28\n",
    )
    .unwrap();
    let a = compile(&base(), &with_let, &mut Lockfile::new(), &opts()).unwrap();
    let b = compile(&base(), &literal, &mut Lockfile::new(), &opts()).unwrap();
    assert_eq!(
        a.to_bytes(),
        b.to_bytes(),
        "a `let` reference compiles exactly like its literal"
    );
}

#[test]
fn extern_port_errors_suggest_close_names() {
    let m = Module::parse(
        "extern jal = AutoJalousie(title: \"Beschattung S\u{fc}d\")\n\
         b = And()\n\
         jal.AutoShad <- b.Q\n",
    )
    .unwrap();
    let err = compile(&base(), &m, &mut Lockfile::new(), &opts()).unwrap_err();
    assert!(
        err.to_string().contains("did you mean `AutoShade`?"),
        "{err}"
    );
}

#[test]
fn module_fragments_merge_and_cross_reference() {
    // One file per page: the block in fragment B wires from an extern
    // declared in fragment A. Each fragment parses alone (no name
    // resolution), the merged module validates.
    let frag_a = "# page: A\nextern sonne = InputRef(title: \"Sonne\")\n";
    let frag_b = "# page: B\ngate = Not(\"Gate\", I: sonne.AQ)\n";
    assert!(
        Module::parse(frag_b).is_err(),
        "a lone fragment with a cross-file reference must not pass full parse"
    );
    let mut items = Module::parse_fragment(frag_a).unwrap().items;
    items.extend(Module::parse_fragment(frag_b).unwrap().items);
    let merged = Module { items };
    merged.validate().unwrap();
    assert_eq!(merged.externs().count(), 1);
    assert_eq!(merged.blocks().count(), 1);
}

#[test]
fn module_fragments_duplicate_slug_is_rejected() {
    let frag = "x = Not(\"X\", I: x.Q)\n";
    let mut items = Module::parse_fragment(frag).unwrap().items;
    items.extend(Module::parse_fragment(frag).unwrap().items);
    let err = Module { items }.validate().unwrap_err();
    assert!(err.to_string().contains("duplicate name `x`"), "{err}");
}
