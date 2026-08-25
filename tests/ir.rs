//! End-to-end IR pipeline tests over the shipped example fixture:
//! determinism, lockfile identity, teardown/restore semantics, and the
//! decompile view of compiled output.

use lxir::ir::{
    ArgItem, Binding, BindingKind, CompileOptions, DecompileOptions, DecompileScope, Item, Module,
    PortRef, adopt, adopt_one, adopt_pages, compile, decompile, decompile_pages,
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
fn pool_showcase_compiles_end_to_end() {
    // The roadmap's showcase module: water temperature, cover interlock,
    // PV-surplus enable — committed under examples/ and kept compiling.
    let base = LoxoneDoc::parse(&std::fs::read("examples/configs/pool.Loxone").unwrap()).unwrap();
    let src = std::fs::read_to_string("examples/ir/pool.lxir").unwrap();
    let module = Module::parse(&src).unwrap();
    assert_eq!(module.to_text(), src, "example stays canonical");

    let opts = CompileOptions {
        page_title: Some("Pool".into()),
        ..opts()
    };
    let mut lock = Lockfile::new();
    let out = compile(&base, &module, &mut lock, &opts).unwrap();

    // `room: "Technikraum"` picked the right one of two "Freigabe".
    assert_eq!(
        lock.externals["wp_freigabe"].uuid,
        "30000006-0000-0060-ffff504f94112233"
    );
    assert_eq!(lock.objects.len(), 5);
    assert_eq!(out.counters().next_obj, 305);

    // The enable wire landed on the Technikraum switch's On port.
    let objs = out.objects();
    let wp = objs
        .iter()
        .find(|o| o.uuid == "30000006-0000-0060-ffff504f94112233")
        .unwrap();
    let ports = lxir::doc::ports(out.element_at(&wp.path).unwrap());
    let on = ports.iter().find(|p| p.key == "On").unwrap();
    assert_eq!(on.inputs.len(), 1);

    // Fixpoint: recompiling against its own output changes nothing.
    let again = compile(&out, &module, &mut lock, &opts).unwrap();
    assert_eq!(out.to_bytes(), again.to_bytes());
}

#[test]
fn composite_extern_matching_narrows_by_room_and_category() {
    // Two identically-titled Switches in different rooms; the room lives
    // in `<IoData Pr=…>` pointing at a Place (docs/loxone-format.md).
    let base = LoxoneDoc::parse(
        "<ControlList Version=\"1\" NextObj=\"20\">\r\n\
         \t<C Type=\"Document\" U=\"00000001-0000-0000-ffff000000000001\">\r\n\
         \t\t<C Type=\"Place\" U=\"00000002-0000-0000-ffff000000000001\" Title=\"B\u{fc}ro\"/>\r\n\
         \t\t<C Type=\"Place\" U=\"00000003-0000-0000-ffff000000000001\" Title=\"K\u{fc}che\"/>\r\n\
         \t\t<C Type=\"Category\" U=\"00000004-0000-0000-ffff000000000001\" Title=\"Beleuchtung\"/>\r\n\
         \t\t<C Type=\"Page\" U=\"00000005-0000-0000-ffff000000000001\" Title=\"P\">\r\n\
         \t\t\t<C Type=\"Switch\" U=\"00000006-0000-0000-ffff000000000001\" Title=\"Deckenlicht\">\r\n\
         \t\t\t\t<Co K=\"Trigger\" U=\"00000006-0000-0001-01ff000000000001\"/>\r\n\
         \t\t\t\t<IoData Visu=\"true\" Cr=\"00000004-0000-0000-ffff000000000001\" Pr=\"00000002-0000-0000-ffff000000000001\"/>\r\n\
         \t\t\t</C>\r\n\
         \t\t\t<C Type=\"Switch\" U=\"00000007-0000-0000-ffff000000000001\" Title=\"Deckenlicht\">\r\n\
         \t\t\t\t<Co K=\"Trigger\" U=\"00000007-0000-0001-01ff000000000001\"/>\r\n\
         \t\t\t\t<IoData Visu=\"true\" Pr=\"00000003-0000-0000-ffff000000000001\"/>\r\n\
         \t\t\t</C>\r\n\
         \t\t</C>\r\n\
         \t</C>\r\n\
         </ControlList>\r\n"
            .as_bytes(),
    )
    .unwrap();

    // Title alone is ambiguous.
    let m = Module::parse("extern licht = Switch(title: \"Deckenlicht\")\n").unwrap();
    let err = compile(&base, &m, &mut Lockfile::new(), &opts()).unwrap_err();
    assert!(err.to_string().contains("2"), "{err}");

    // The room narrows it to one.
    let m = Module::parse("extern licht = Switch(title: \"Deckenlicht\", room: \"B\u{fc}ro\")\n")
        .unwrap();
    let mut lock = Lockfile::new();
    compile(&base, &m, &mut lock, &opts()).unwrap();
    assert_eq!(
        lock.externals["licht"].uuid,
        "00000006-0000-0000-ffff000000000001"
    );

    // So does the category (only one Switch carries Cr=).
    let m =
        Module::parse("extern licht = Switch(title: \"Deckenlicht\", category: \"Beleuchtung\")\n")
            .unwrap();
    let mut lock = Lockfile::new();
    compile(&base, &m, &mut lock, &opts()).unwrap();
    assert_eq!(
        lock.externals["licht"].uuid,
        "00000006-0000-0000-ffff000000000001"
    );

    // A wrong room is NoMatch, and the error shows the full spec.
    let m =
        Module::parse("extern licht = Switch(title: \"Deckenlicht\", room: \"Bad\")\n").unwrap();
    let err = compile(&base, &m, &mut Lockfile::new(), &opts()).unwrap_err();
    assert!(err.to_string().contains("room: \"Bad\""), "{err}");

    // uuid + room is a parse error; the composite form survives fmt.
    assert!(
        Module::parse("extern x = Switch(uuid: \"00000006-0000-0000-ffff000000000001\", room: \"B\u{fc}ro\")\n")
            .is_err()
    );
    let src = "extern licht = Switch(title: \"Deckenlicht\", room: \"B\u{fc}ro\")\n";
    let m = Module::parse(src).unwrap();
    assert_eq!(m.to_text(), src, "canonical form round-trips");
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

/// Synthetic config for the incremental-adopt tests: two VirtualIns, a
/// managed And wired from the first — and slots for a GUI-drawn Or block
/// plus its wire into the And's I2.
fn incr_base(or_block: &str, and_i2: &str) -> LoxoneDoc {
    LoxoneDoc::parse(
        format!(
            "<ControlList Version=\"1\" NextObj=\"50\">\r\n\
             \t<C Type=\"Document\" U=\"00000001-0000-0000-ffff000000000001\" ConfigVersion=\"17010727\">\r\n\
             \t\t<C Type=\"VirtualInCaption\" U=\"00000002-0000-0000-ffff000000000001\">\r\n\
             \t\t\t<C Type=\"VirtualIn\" U=\"00000010-0000-0000-ffff000000000001\" Title=\"Sensor A\">\r\n\
             \t\t\t\t<Co K=\"Q\" U=\"00000010-0000-0001-01ff000000000001\"/>\r\n\
             \t\t\t</C>\r\n\
             \t\t\t<C Type=\"VirtualIn\" U=\"00000011-0000-0000-ffff000000000001\" Title=\"Sensor B\">\r\n\
             \t\t\t\t<Co K=\"Q\" U=\"00000011-0000-0001-01ff000000000001\"/>\r\n\
             \t\t\t</C>\r\n\
             \t\t</C>\r\n\
             \t\t<C Type=\"Page\" U=\"00000005-0000-0000-ffff000000000001\" Title=\"Regeln\">\r\n\
             \t\t\t<C Type=\"And\" V=\"175\" U=\"00000020-0000-0000-ffff000000000001\" Title=\"Beide\" \
             Px=\"100\" Py=\"100\" Px2=\"154\" Py2=\"136\" Nio=\"3\">\r\n\
             \t\t\t\t<Co K=\"I1\" U=\"00000020-0000-0001-01ff000000000001\" Nc=\"1\">\r\n\
             \t\t\t\t\t<In Input=\"00000010-0000-0001-01ff000000000001\"/>\r\n\
             \t\t\t\t</Co>\r\n\
             \t\t\t\t{and_i2}\r\n\
             \t\t\t\t<Co K=\"Q\" U=\"00000020-0000-0003-01ff000000000001\" Nc=\"0\"/>\r\n\
             \t\t\t</C>\r\n\
             {or_block}\
             \t\t</C>\r\n\
             \t</C>\r\n\
             </ControlList>\r\n"
        )
        .as_bytes(),
    )
    .unwrap()
}

const OR_UUID: &str = "00000030-0000-0000-ffff000000000001";
const INCR_I2_UNWIRED: &str = "<Co K=\"I2\" U=\"00000020-0000-0002-01ff000000000001\" Nc=\"0\"/>";
const INCR_I2_WIRED: &str = "<Co K=\"I2\" U=\"00000020-0000-0002-01ff000000000001\" Nc=\"1\">\
     <In Input=\"00000030-0000-0003-01ff000000000001\"/></Co>";
const INCR_OR_BLOCK: &str = "\t\t\t<C Type=\"Or\" V=\"175\" \
     U=\"00000030-0000-0000-ffff000000000001\" Title=\"Nachtlicht\" \
     Px=\"200\" Py=\"200\" Px2=\"254\" Py2=\"236\" Nio=\"3\">\
     <Co K=\"I1\" U=\"00000030-0000-0001-01ff000000000001\" Nc=\"1\">\
     <In Input=\"00000010-0000-0001-01ff000000000001\"/></Co>\
     <Co K=\"I2\" U=\"00000030-0000-0002-01ff000000000001\" Nc=\"1\">\
     <In Input=\"00000011-0000-0001-01ff000000000001\"/></Co>\
     <Co K=\"Q\" U=\"00000030-0000-0003-01ff000000000001\" Nc=\"0\"/></C>\r\n";

#[test]
fn incremental_adopt_claims_a_gui_drawn_block() {
    // Whole-adopt the starting state: one managed And, one extern.
    let base1 = incr_base("", INCR_I2_UNWIRED);
    let (mut module, lock0, report) = adopt(&base1).unwrap();
    assert_eq!(report.blocks, 1);
    assert!(lock0.objects.contains_key("beide"));
    assert!(lock0.externals.contains_key("sensor_a"));

    // Then someone draws an Or in Loxone Config: fed by both sensors,
    // feeding the managed And's I2.
    let base2 = incr_base(INCR_OR_BLOCK, INCR_I2_WIRED);

    // The wire into the managed sink is not in source yet: refused, and
    // the error names the exact line to add.
    let err = adopt_one(&base2, OR_UUID, "nachtlicht", &module, &mut lock0.clone()).unwrap_err();
    assert!(err.to_string().contains("I2: nachtlicht.Q"), "{err}");
    assert!(err.to_string().contains("`beide`"), "{err}");

    // The manual fix: declare the wire in the And's argument list.
    for item in &mut module.items {
        if let Item::Block(b) = item
            && b.slug == "beide"
        {
            b.args.push(ArgItem::Binding(Binding {
                port: "I2".into(),
                kind: BindingKind::Wire(PortRef {
                    slug: "nachtlicht".into(),
                    port: "Q".into(),
                }),
                comment: None,
            }));
        }
    }

    let mut lock = lock0.clone();
    let adopted = adopt_one(&base2, OR_UUID, "nachtlicht", &module, &mut lock).unwrap();
    assert_eq!(adopted.page_title, "Regeln");
    assert_eq!(adopted.new_externs, vec!["sensor_b".to_string()]);

    // Two items: the new extern and the block. Sensor A is referenced by
    // its existing slug, not re-declared; the outgoing wire lives in the
    // And's declaration, so no `<-` statement.
    let text = Module {
        items: adopted.items.clone(),
    }
    .to_text();
    assert_eq!(adopted.items.len(), 2, "{text}");
    assert!(
        text.contains("extern sensor_b = VirtualIn(title: \"Sensor B\")"),
        "{text}"
    );
    assert!(text.contains("I1: sensor_a.Q"), "{text}");
    assert!(text.contains("I2: sensor_b.Q"), "{text}");

    // The lock pins the block's existing identity and the new extern, and
    // the adopted-from config is the new drift baseline.
    assert_eq!(lock.objects["nachtlicht"].uuid, OR_UUID);
    assert_eq!(lock.objects["nachtlicht"].ports.len(), 3);
    assert!(lock.objects["nachtlicht"].layout.is_some());
    assert!(lock.externals.contains_key("sensor_b"));
    assert_eq!(
        lock.target.semantic_fingerprint.as_deref(),
        Some(&*lxir::diff::semantic_fingerprint(&base2))
    );

    // Appending the items yields a module whose rebuild is a semantic
    // no-op against the config — and stays deterministic.
    let mut merged = module.clone();
    merged.items.extend(adopted.items.clone());
    merged.validate().unwrap();
    let noop_opts = CompileOptions {
        page_title: None,
        ..opts()
    };
    let out = compile(&base2, &merged, &mut lock, &noop_opts).unwrap();
    let d = lxir::diff::diff(&base2, &out);
    assert!(d.is_empty(), "{d:#?}");
    let out2 = compile(&base2, &merged, &mut lock, &noop_opts).unwrap();
    assert_eq!(out.to_bytes(), out2.to_bytes());

    // Slug hygiene on the same scenario.
    let taken = adopt_one(&base2, OR_UUID, "beide", &module, &mut lock0.clone()).unwrap_err();
    assert!(taken.to_string().contains("already taken"), "{taken}");
    let bad = adopt_one(&base2, OR_UUID, "Nacht", &module, &mut lock0.clone()).unwrap_err();
    assert!(bad.to_string().contains("not a valid slug"), "{bad}");

    // An unqualified release pin refuses before anything else mutates.
    let mut pinned = lock0.clone();
    pinned.target.config_version = Some("17999999".into());
    let err = adopt_one(&base2, OR_UUID, "nachtlicht", &module, &mut pinned).unwrap_err();
    assert!(err.to_string().contains("--accept-version"), "{err}");
}

#[test]
fn incremental_adopt_refuses_claimed_or_unverified_identities() {
    let module = module();
    let mut lock = Lockfile::new();
    // The compiled output holds both the managed blocks and the externs.
    let base = compile(&base(), &module, &mut lock, &opts()).unwrap();

    let err = adopt_one(
        &base,
        "12345678-0000-0000-ffff000000000001",
        "x",
        &module,
        &mut lock.clone(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("no object"), "{err}");

    let managed_uuid = lock.objects["beschatten"].uuid.clone();
    let err = adopt_one(&base, &managed_uuid, "x", &module, &mut lock.clone()).unwrap_err();
    assert!(err.to_string().contains("already managed"), "{err}");

    let extern_uuid = lock.externals["jal_sued"].uuid.clone();
    let err = adopt_one(&base, &extern_uuid, "x", &module, &mut lock.clone()).unwrap_err();
    assert!(err.to_string().contains("extern `jal_sued`"), "{err}");

    let vi = base
        .objects()
        .iter()
        .find(|o| o.block_type == "VirtualIn")
        .unwrap()
        .uuid
        .clone();
    let err = adopt_one(&base, &vi, "x", &module, &mut lock.clone()).unwrap_err();
    assert!(err.to_string().contains("builtin table"), "{err}");
}

const FASSADE_MODULE: &str = "\
extern aussentemp = VirtualIn(iname: \"VI1\")\n\
extern wind_alarm = VirtualIn(iname: \"VI2\")\n\
extern sonne = VirtualIn(iname: \"VI3\")\n\
extern jal_sued = AutoJalousie(title: \"Beschattung S\u{fc}d\")\n\
\n\
template fassade(jalousie: AutoJalousie, schwelle = 28, pos = 70)\n\
\thoch = GreaterEqual(Input1: aussentemp.Q, Input2: schwelle)\n\
\n\
\tbeschatten = And(I1: hoch.Q, I2: sonne.Q)\n\
\n\
\tjalousie.AutoShade <- beschatten.Q\n\
\tjalousie.Safety <- wind_alarm.Q\n\
\n\
\tjalousie.TargetPos = pos\n\
end\n\
\n\
sued = fassade(jalousie: jal_sued)\n";

#[test]
fn templates_expand_with_locked_identities() {
    let base = base();
    let module = Module::parse(FASSADE_MODULE).unwrap();

    // The canonical form keeps the template intact and is a fixpoint
    // (call arguments canonicalize to one per line, so the compact
    // fixture is not itself canonical).
    let text = module.to_text();
    assert_eq!(
        Module::parse(&text).unwrap(),
        module,
        "parse ∘ to_text = id"
    );
    assert_eq!(Module::parse(&text).unwrap().to_text(), text, "fixpoint");
    assert!(
        text.contains("template fassade(jalousie: AutoJalousie, schwelle = 28, pos = 70)"),
        "{text}"
    );
    assert!(text.contains("\tjalousie.TargetPos = pos\nend"), "{text}");

    // Compiling sees only the expansion: instance `sued` owns two blocks
    // under their expanded, lockfile-keyed names.
    let mut lock = Lockfile::new();
    let out = compile(&base, &module, &mut lock, &opts()).unwrap();
    assert_eq!(
        lock.objects.keys().collect::<Vec<_>>(),
        ["sued_beschatten", "sued_hoch"]
    );

    // The captured externs and the object parameter resolved: AutoShade
    // fed by the instance's And, Safety by the wind alarm, TargetPos 70.
    let jal = |doc: &LoxoneDoc| {
        let objs = doc.objects();
        let o = objs
            .iter()
            .find(|o| o.block_type == "AutoJalousie")
            .unwrap();
        lxir::doc::ports(doc.element_at(&o.path).unwrap())
    };
    let ports = jal(&out);
    let target = ports.iter().find(|p| p.key == "TargetPos").unwrap();
    assert_eq!(target.def.as_deref(), Some("70"));
    let wired: usize = ports.iter().map(|p| p.inputs.len()).sum();
    assert_eq!(wired, 2);

    // Fixpoint, and value overrides re-parameterize without re-minting.
    let again = compile(&out, &module, &mut lock, &opts()).unwrap();
    assert_eq!(out.to_bytes(), again.to_bytes());
    let tuned = Module::parse(&FASSADE_MODULE.replace(
        "sued = fassade(jalousie: jal_sued)",
        "sued = fassade(jalousie: jal_sued, schwelle: 30, pos: 55)",
    ))
    .unwrap();
    let next_obj = out.counters().next_obj;
    let tuned_out = compile(&base, &tuned, &mut lock, &opts()).unwrap();
    assert_eq!(tuned_out.counters().next_obj, next_obj, "nothing re-minted");
    let ports = jal(&tuned_out);
    assert_eq!(
        ports
            .iter()
            .find(|p| p.key == "TargetPos")
            .unwrap()
            .def
            .as_deref(),
        Some("55")
    );

    // Growing the template body mints only the addition; shrinking it is
    // the usual vanished-slug error, per instance.
    let grown = Module::parse(&FASSADE_MODULE.replace(
        "\tjalousie.TargetPos = pos\n",
        "\tjalousie.TargetPos = pos\n\twarnung = Not(I: hoch.Q)\n",
    ))
    .unwrap();
    let grown_out = compile(&base, &grown, &mut lock, &opts()).unwrap();
    assert!(lock.objects.contains_key("sued_warnung"));
    assert_eq!(grown_out.counters().next_obj, next_obj + 1);
    let err = compile(&base, &module, &mut lock.clone(), &opts()).unwrap_err();
    assert!(err.to_string().contains("sued_warnung"), "{err}");
}

#[test]
fn two_instances_get_disjoint_identities() {
    let src = "\
extern vi_a = VirtualIn(iname: \"VI1\")\n\
extern vi_b = VirtualIn(iname: \"VI2\")\n\
\n\
template melder(quelle: VirtualIn, schwelle = 1)\n\
\talarm = GreaterEqual(Input1: quelle.Q, Input2: schwelle)\n\
end\n\
\n\
a = melder(quelle: vi_a)\n\
b = melder(quelle: vi_b, schwelle: 5)\n";
    let module = Module::parse(src).unwrap();
    let mut lock = Lockfile::new();
    let out = compile(&base(), &module, &mut lock, &opts()).unwrap();
    assert_eq!(
        lock.objects.keys().collect::<Vec<_>>(),
        ["a_alarm", "b_alarm"]
    );
    assert_ne!(lock.objects["a_alarm"].uuid, lock.objects["b_alarm"].uuid);
    let fixpoint = compile(&out, &module, &mut lock, &opts()).unwrap();
    assert_eq!(out.to_bytes(), fixpoint.to_bytes());
}

#[test]
fn template_misuse_is_reported() {
    let fails = |src: &str, needle: &str| {
        let err = match Module::parse(src) {
            Err(e) => e.to_string(),
            Ok(m) => compile(&base(), &m, &mut Lockfile::new(), &opts())
                .unwrap_err()
                .to_string(),
        };
        assert!(err.contains(needle), "`{needle}` not in: {err}");
    };
    let tmpl = "template t(x: VirtualIn, n = 1)\n\ty = Not(I: x.Q)\nend\n";
    let vi = "extern vi = VirtualIn(iname: \"VI1\")\n";

    fails("a = nirgends(x: 1)\n", "unknown template `nirgends`");
    fails(&format!("{tmpl}a = t()\n"), "`x` must be given");
    fails(
        &format!("{vi}{tmpl}a = t(x: vi, oops: 2)\n"),
        "unknown parameter `oops`",
    );
    fails(
        &format!("{vi}{tmpl}a = t(x: vi, x: vi)\n"),
        "duplicate argument",
    );
    fails(
        &format!("{vi}{tmpl}a = t(x: 5)\n"),
        "takes an extern or block slug",
    );
    fails(&format!("{vi}{tmpl}a = t(x: vi, n: vi.Q)\n"), "not a port");
    fails(
        &format!("{vi}{tmpl}a = t(\"Label\", x: vi)\n"),
        "no label string",
    );
    fails(
        "template t(x: VirtualIn)\n\tlet n = 1\nend\n",
        "not allowed inside a template body",
    );
    fails(
        "template a(x: VirtualIn)\n\ty = Not(I: x.Q)\nend\n\
           template b(x: VirtualIn)\n\tz = a(x: x)\nend\n",
        "cannot instantiate another template",
    );
    fails("use fassade(jal)\n", "v0 syntax");
    fails(
        "template t(x: VirtualIn)\n\ty = Not(I: x.Q)\n",
        "missing `end`",
    );
    fails("end\n", "`end` without an open `template`");
    // The instance slug names no object; the message points at the
    // expanded names.
    fails(
        &format!("{vi}{tmpl}a = t(x: vi)\nb = Not(I: a.Q)\n"),
        "template instance",
    );
    // A wrong object-arg type is caught where the declaration is known.
    fails(
        &format!("extern j = AutoJalousie(title: \"X\")\n{tmpl}a = t(x: j)\n"),
        "expects a VirtualIn",
    );
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

// ---------------------------------------------------------------------------
// Expression sugar (D24)

const EXPR_MODULE: &str = "\
extern aussentemp = VirtualIn(iname: \"VI1\")\n\
extern wind_alarm = VirtualIn(iname: \"VI2\")\n\
extern sonne = VirtualIn(iname: \"VI3\")\n\
extern jal_sued = AutoJalousie(title: \"Beschattung S\u{fc}d\")\n\
\n\
let schwelle = 28\n\
\n\
jal_sued.AutoShade <- sonne.Q and aussentemp.Q >= schwelle\n";

#[test]
fn expressions_desugar_with_locked_identities() {
    let base = base();
    let module = Module::parse(EXPR_MODULE).unwrap();

    // Canonical form keeps the expression as written (it is already
    // minimal-paren) and is a fixpoint.
    let text = module.to_text();
    assert_eq!(
        Module::parse(&text).unwrap(),
        module,
        "parse ∘ to_text = id"
    );
    assert_eq!(Module::parse(&text).unwrap().to_text(), text, "fixpoint");
    assert!(
        text.contains("jal_sued.AutoShade <- sonne.Q and aussentemp.Q >= schwelle"),
        "{text}"
    );

    // Desugaring emits one comparator and one gate, prefixed by the sink.
    let (plain, info) = module.expand().unwrap().desugar().unwrap();
    assert_eq!(info.expressions, 1);
    assert_eq!(
        info.synthetic.iter().collect::<Vec<_>>(),
        ["jal_sued_autoshade__and1", "jal_sued_autoshade__ge1"]
    );
    let ge = plain
        .blocks()
        .find(|b| b.slug == "jal_sued_autoshade__ge1")
        .unwrap();
    assert_eq!(ge.block_type, "GreaterEqual");
    assert_eq!(ge.title.as_deref(), Some("aussentemp.Q >= schwelle"));
    assert!(
        ge.params()
            .any(|(p, v)| p == "Input2" && v.to_string() == "schwelle"),
        "constant operand becomes a Def parameter"
    );

    // Compile: both synthetic blocks are locked and expression-owned.
    let mut lock = Lockfile::new();
    let out = compile(&base, &module, &mut lock, &opts()).unwrap();
    assert_eq!(
        lock.objects.keys().collect::<Vec<_>>(),
        ["jal_sued_autoshade__and1", "jal_sued_autoshade__ge1"]
    );
    assert!(lock.objects.values().all(|o| o.expr_owned));
    let ge_uuid = lock.objects["jal_sued_autoshade__ge1"].uuid.clone();
    let and_uuid = lock.objects["jal_sued_autoshade__and1"].uuid.clone();

    // Recompile: byte-identical, no re-mint.
    let out2 = compile(&base, &module, &mut lock, &opts()).unwrap();
    assert_eq!(out.to_bytes(), out2.to_bytes());

    // Growing the expression keeps the unchanged nodes' identities and
    // mints only the new gate — no `removed` statement anywhere.
    let grown =
        Module::parse(&EXPR_MODULE.replace(">= schwelle\n", ">= schwelle or wind_alarm.Q\n"))
            .unwrap();
    compile(&base, &grown, &mut lock, &opts()).unwrap();
    assert_eq!(lock.objects["jal_sued_autoshade__ge1"].uuid, ge_uuid);
    assert_eq!(lock.objects["jal_sued_autoshade__and1"].uuid, and_uuid);
    let or_uuid = lock.objects["jal_sued_autoshade__or1"].uuid.clone();
    // Cross-session mint collision guard: a block minted in a later
    // compile (same mint time) must not reuse any earlier (time, sequence)
    // pair — every locked UUID stays distinct.
    let all: Vec<&String> = lock
        .objects
        .values()
        .flat_map(|o| std::iter::once(&o.uuid).chain(o.ports.values()))
        .collect();
    let distinct: std::collections::BTreeSet<&&String> = all.iter().collect();
    assert_eq!(distinct.len(), all.len(), "duplicate minted UUIDs");

    // Shrinking to just the `or` auto-removes the orphaned comparator and
    // gate (they are expression-owned) while `__or1` keeps its identity.
    let shrunk = Module::parse(&EXPR_MODULE.replace(
        "sonne.Q and aussentemp.Q >= schwelle\n",
        "sonne.Q or wind_alarm.Q\n",
    ))
    .unwrap();
    compile(&base, &shrunk, &mut lock, &opts()).unwrap();
    assert_eq!(
        lock.objects.keys().collect::<Vec<_>>(),
        ["jal_sued_autoshade__or1"]
    );
    assert_eq!(lock.objects["jal_sued_autoshade__or1"].uuid, or_uuid);

    // Deleting the statement deletes everything expression-owned.
    let gone = Module::parse(&EXPR_MODULE.replace(
        "jal_sued.AutoShade <- sonne.Q and aussentemp.Q >= schwelle\n",
        "",
    ))
    .unwrap();
    let out_gone = compile(&base, &gone, &mut lock, &opts()).unwrap();
    assert!(lock.objects.is_empty());
    assert!(
        lxir::diff::diff(&base, &out_gone).is_empty(),
        "removing the expression restores the base"
    );
}

#[test]
fn expressions_expand_inside_template_bodies() {
    let base = base();
    let module = Module::parse(
        "extern aussentemp = VirtualIn(iname: \"VI1\")\n\
         extern sonne = VirtualIn(iname: \"VI3\")\n\
         extern jal_sued = AutoJalousie(title: \"Beschattung S\u{fc}d\")\n\
         template fassade(jalousie: AutoJalousie, schwelle = 28)\n\
         \tjalousie.AutoShade <- sonne.Q and aussentemp.Q >= schwelle\n\
         end\n\
         sued = fassade(jalousie: jal_sued, schwelle: 30)\n",
    )
    .unwrap();

    // The synthetic prefix uses the *expanded* sink — the passed extern.
    let mut lock = Lockfile::new();
    compile(&base, &module, &mut lock, &opts()).unwrap();
    assert_eq!(
        lock.objects.keys().collect::<Vec<_>>(),
        ["jal_sued_autoshade__and1", "jal_sued_autoshade__ge1"]
    );
    assert!(lock.objects.values().all(|o| o.expr_owned));
}

#[test]
fn expression_canonical_form_uses_minimal_parens() {
    let head = "extern a = VirtualIn(iname: \"VI1\")\n\
                extern b = VirtualIn(iname: \"VI2\")\n\
                extern c = VirtualIn(iname: \"VI3\")\n\
                extern jal = AutoJalousie(title: \"J\")\n";
    let canon = |rhs: &str| {
        let m = Module::parse(&format!("{head}jal.AutoShade <- {rhs}\n")).unwrap();
        let text = m.to_text();
        assert_eq!(Module::parse(&text).unwrap().to_text(), text, "fixpoint");
        text.lines()
            .find(|l| l.starts_with("jal.AutoShade <- "))
            .unwrap()
            .trim_start_matches("jal.AutoShade <- ")
            .to_string()
    };
    // Precedence needs no parens; grouping against it keeps them.
    assert_eq!(canon("a.Q and b.Q or c.Q"), "a.Q and b.Q or c.Q");
    assert_eq!(canon("(a.Q and b.Q) or c.Q"), "a.Q and b.Q or c.Q");
    assert_eq!(canon("a.Q and (b.Q or c.Q)"), "a.Q and (b.Q or c.Q)");
    assert_eq!(canon("(a.Q or b.Q) and c.Q"), "(a.Q or b.Q) and c.Q");
    assert_eq!(canon("a.Q or (b.Q or c.Q)"), "a.Q or (b.Q or c.Q)");
    // A comparison under `not` is always parenthesized for readability.
    assert_eq!(canon("not a.Q >= 5"), "not (a.Q >= 5)");
    assert_eq!(canon("not (a.Q >= 5)"), "not (a.Q >= 5)");
    assert_eq!(canon("not not a.Q"), "not not a.Q");
    assert_eq!(canon("not (a.Q and b.Q)"), "not (a.Q and b.Q)");
    assert_eq!(canon("a.Q < -5"), "a.Q < -5");
}

#[test]
fn expression_misuse_is_reported() {
    let head = "extern a = VirtualIn(iname: \"VI1\")\n\
                extern b = VirtualIn(iname: \"VI2\")\n\
                extern jal = AutoJalousie(title: \"J\")\n\
                let schwelle = 28\n";
    let fails = |line: &str, needle: &str| {
        let err = Module::parse(&format!("{head}{line}\n"))
            .and_then(|m| m.expand())
            .and_then(|m| m.desugar().map(|_| ()))
            .unwrap_err();
        assert!(
            err.to_string().contains(needle),
            "`{line}`: expected {needle:?} in: {err}"
        );
    };

    // Parse-level shape errors.
    fails("jal.AutoShade <- a.Q and \"x\"", "strings have no place");
    fails("jal.AutoShade <- a.Q < b.Q < 5", "comparisons do not chain");
    fails(
        "jal.AutoShade <- (a.Q and b.Q) >= 5",
        "not parenthesized expressions",
    );
    fails("jal.AutoShade <- (a.Q and b.Q", "missing `)`");
    fails("jal.AutoShade <- a.Q b.Q", "unexpected trailing tokens");
    fails(
        "jal.AutoShade <- and a.Q",
        "expected an operand, found `and`",
    );
    fails("jal.AutoShade <- 5", "use `jal.AutoShade = 5`");
    fails("let and = 5", "reserved word");
    fails("or = And(I1: a.Q, I2: b.Q)", "reserved word");

    // Validation: references resolve before desugaring, on what the user
    // wrote.
    fails(
        "jal.AutoShade <- nix.Q and a.Q",
        "reference to undeclared slug `nix`",
    );
    fails(
        "jal.AutoShade <- a.Q and nixkonst >= 5",
        "undeclared constant `nixkonst`",
    );

    // Desugar-level semantic errors.
    fails("jal.AutoShade <- 5 and a.Q", "cannot drive a gate input");
    fails("jal.AutoShade <- schwelle >= 28", "compares two constants");
    fails(
        "jal_autoshade__and1 = And(I1: a.Q, I2: b.Q)\n\
         jal.AutoShade <- a.Q and b.Q",
        "claimed by the expression",
    );

    // A managed sink refuses `<-` exactly like plain wires do.
    let err = Module::parse(&format!(
        "{head}gate = And(I1: a.Q, I2: b.Q)\ngate.I1 <- a.Q and b.Q\n"
    ))
    .unwrap_err();
    assert!(err.to_string().contains("targets managed block"), "{err}");
}

// ---------------------------------------------------------------------------
// Unit-suffixed values (D27)

#[test]
fn unit_values_compile_byte_identically_to_their_base_literal() {
    let base = base();
    let with_units = Module::parse(
        "extern sonne = VirtualIn(iname: \"VI3\")\n\
         nachlauf = Monoflop(InputTrigger: sonne.Q, Time: 1.5h)\n",
    )
    .unwrap();
    let plain = Module::parse(
        "extern sonne = VirtualIn(iname: \"VI3\")\n\
         nachlauf = Monoflop(InputTrigger: sonne.Q, Time: 5400)\n",
    )
    .unwrap();
    let mut lock_a = Lockfile::new();
    let out_a = compile(&base, &with_units, &mut lock_a, &opts()).unwrap();
    let mut lock_b = Lockfile::new();
    let out_b = compile(&base, &plain, &mut lock_b, &opts()).unwrap();
    assert_eq!(
        out_a.to_bytes(),
        out_b.to_bytes(),
        "a unit value compiles byte-identically to its base-unit literal"
    );
}

// ---------------------------------------------------------------------------
// Expressions in argument bindings (D26)

#[test]
fn expression_bindings_desugar_like_wire_expressions() {
    let base = base();
    let src = "\
extern aussentemp = VirtualIn(iname: \"VI1\")\n\
extern wind_alarm = VirtualIn(iname: \"VI2\")\n\
extern sonne = VirtualIn(iname: \"VI3\")\n\
extern jal_sued = AutoJalousie(title: \"Beschattung S\u{fc}d\")\n\
\n\
let schwelle = 28\n\
\n\
gate = And(\n\
\tI1: sonne.Q and aussentemp.Q >= schwelle,\n\
\tI2: wind_alarm.Q,\n\
)\n\
\n\
jal_sued.AutoShade <- gate.Q\n";
    let module = Module::parse(src).unwrap();

    // Canonical form keeps the expression as written and is a fixpoint.
    let text = module.to_text();
    assert!(
        text.contains("\tI1: sonne.Q and aussentemp.Q >= schwelle,"),
        "{text}"
    );
    assert_eq!(
        Module::parse(&text).unwrap(),
        module,
        "parse ∘ to_text = id"
    );
    assert_eq!(Module::parse(&text).unwrap().to_text(), text, "fixpoint");

    // Desugaring prefixes with the managed sink and rewires the binding.
    let (plain, info) = module.expand().unwrap().desugar().unwrap();
    assert_eq!(info.expressions, 1);
    assert_eq!(
        info.synthetic.iter().collect::<Vec<_>>(),
        ["gate_i1__and1", "gate_i1__ge1"]
    );
    let gate = plain.blocks().find(|b| b.slug == "gate").unwrap();
    let wires: Vec<String> = gate
        .input_wires()
        .map(|(p, src)| format!("{p}: {src}"))
        .collect();
    assert_eq!(wires, ["I1: gate_i1__and1.Q", "I2: wind_alarm.Q"]);
    assert!(gate.expr_bindings().next().is_none());

    // Compile: synthetic blocks are locked and expression-owned; the
    // recompile is byte-identical (no re-mint).
    let mut lock = Lockfile::new();
    let out = compile(&base, &module, &mut lock, &opts()).unwrap();
    assert!(lock.objects["gate_i1__and1"].expr_owned);
    assert!(lock.objects["gate_i1__ge1"].expr_owned);
    assert!(!lock.objects["gate"].expr_owned);
    let out2 = compile(&base, &module, &mut lock, &opts()).unwrap();
    assert_eq!(out.to_bytes(), out2.to_bytes());

    // Dropping the expression from the binding auto-removes its blocks.
    let shrunk =
        Module::parse(&src.replace("I1: sonne.Q and aussentemp.Q >= schwelle,", "I1: sonne.Q,"))
            .unwrap();
    compile(&base, &shrunk, &mut lock, &opts()).unwrap();
    assert_eq!(lock.objects.keys().collect::<Vec<_>>(), ["gate"]);
}

#[test]
fn expression_bindings_expand_inside_template_bodies() {
    let base = base();
    let module = Module::parse(
        "extern aussentemp = VirtualIn(iname: \"VI1\")\n\
         extern sonne = VirtualIn(iname: \"VI3\")\n\
         extern jal_sued = AutoJalousie(title: \"Beschattung S\u{fc}d\")\n\
         template fassade(jalousie: AutoJalousie, schwelle = 28)\n\
         \tgate = Not(I: sonne.Q and aussentemp.Q >= schwelle)\n\
         \tjalousie.AutoShade <- gate.Q\n\
         end\n\
         sued = fassade(jalousie: jal_sued, schwelle: 30)\n",
    )
    .unwrap();

    // The synthetic prefix uses the *expanded* block slug, and the value
    // parameter substitutes into the expression.
    let (plain, info) = module.expand().unwrap().desugar().unwrap();
    assert_eq!(
        info.synthetic.iter().collect::<Vec<_>>(),
        ["sued_gate_i__and1", "sued_gate_i__ge1"]
    );
    let ge = plain
        .blocks()
        .find(|b| b.slug == "sued_gate_i__ge1")
        .unwrap();
    assert!(
        ge.params()
            .any(|(p, v)| p == "Input2" && v.to_string() == "30"),
        "template value parameter substitutes into the expression"
    );
    let mut lock = Lockfile::new();
    compile(&base, &module, &mut lock, &opts()).unwrap();
    assert!(lock.objects["sued_gate_i__and1"].expr_owned);
}

#[test]
fn expression_binding_misuse_is_reported() {
    let head = "extern a = VirtualIn(iname: \"VI1\")\n\
                extern b = VirtualIn(iname: \"VI2\")\n\
                let schwelle = 28\n";
    let fails = |args: &str, needle: &str| {
        let err = Module::parse(&format!("{head}gate = And({args})\n"))
            .and_then(|m| m.expand())
            .and_then(|m| m.desugar().map(|_| ()))
            .unwrap_err();
        assert!(
            err.to_string().contains(needle),
            "`{args}`: expected {needle:?} in: {err}"
        );
    };

    fails("I1: a.Q and \"x\"", "strings have no place");
    fails("I1: nix.Q and a.Q", "reference to undeclared slug `nix`");
    fails("I1: 5 and a.Q", "cannot drive a gate input");
    fails("I1: schwelle >= 28", "compares two constants");
    fails(
        "I1: a.Q and b.Q, I1: a.Q and b.Q",
        "duplicate expression `I1: a.Q and b.Q`",
    );

    // A parenthesized bare port or value canonicalizes to the plain form.
    let m = Module::parse(&format!("{head}gate = And(I1: (a.Q), I2: (28))\n")).unwrap();
    let gate = m.blocks().next().unwrap();
    assert!(
        matches!(
            &gate.args[0],
            ArgItem::Binding(Binding {
                kind: BindingKind::Wire(_),
                ..
            })
        ),
        "(a.Q) is a plain wire binding"
    );
    assert!(
        matches!(
            &gate.args[1],
            ArgItem::Binding(Binding {
                kind: BindingKind::Param(_),
                ..
            })
        ),
        "(28) is a plain parameter binding"
    );

    // A template's value parameter refuses an expression argument.
    let err = Module::parse(
        "extern a = VirtualIn(iname: \"VI1\")\n\
         template t(x: VirtualIn, n = 1)\n\
         \ty = Not(I: x.Q)\n\
         end\n\
         z = t(x: a, n: a.Q and a.Qm)\n",
    )
    .unwrap_err();
    assert!(
        err.to_string().contains("not a port or expression"),
        "{err}"
    );
}
