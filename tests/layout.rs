//! Placement end to end (D42): where compile puts blocks, how it takes
//! over GUI moves, and what `relayout` may move.

use lxir::ir::{CompileOptions, Module, compile};
use lxir::layout::{GRID, Layout, PageGeometry};
use lxir::render::metrics;
use lxir::uuid::parse_serial;
use lxir::{Lockfile, LoxoneDoc};

const MINT_TIME: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z
const BESCHATTUNG: &str = "20000004-0000-0040-ffff504f94112233";
const PAGE_BESCHATTUNG: &str =
    "<C Type=\"Page\" V=\"175\" U=\"20000004-0000-0040-ffff504f94112233\" Title=\"Beschattung\">";
const PAGE_WERKSTATT: &str =
    "<C Type=\"Page\" V=\"175\" U=\"20000009-0000-0090-ffff504f94112233\" Title=\"Werkstatt\"></C>";

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
        page_title: None,
        allow_removals: false,
        accept_version: None,
        relayout: Vec::new(),
    }
}

fn relayout() -> CompileOptions {
    CompileOptions {
        relayout: vec!["Beschattung".into()],
        ..opts()
    }
}

fn uuid(lock: &Lockfile, slug: &str) -> String {
    lock.objects[slug].uuid.clone()
}

fn rect(doc: &LoxoneDoc, uuid: &str) -> Layout {
    let idx = doc.index();
    Layout::of_element(doc.element_at(&idx.by_uuid[uuid]).unwrap()).unwrap()
}

/// `doc` with the object moved to (`x`, `y`), the way the GUI saves a drag.
fn gui_move(doc: &LoxoneDoc, uuid: &str, x: i64, y: i64) -> LoxoneDoc {
    let mut doc = doc.clone();
    let r = rect(&doc, uuid);
    let path = doc.index().by_uuid[uuid].clone();
    let el = doc.element_at_mut(&path).unwrap();
    let to = Layout::at(x, y, r.w(), r.h());
    for (k, v) in [
        ("Px", to.px),
        ("Py", to.py),
        ("Px2", to.px2),
        ("Py2", to.py2),
    ] {
        el.set_attr(k, &v.to_string());
    }
    LoxoneDoc::parse(&doc.to_bytes()).unwrap()
}

#[test]
fn new_blocks_are_placed_clear_on_the_grid_and_left_to_right() {
    let mut lock = Lockfile::new();
    let out = compile(&base(), &module(), &mut lock, &opts()).unwrap();
    let geo = PageGeometry::of(&out, BESCHATTUNG).unwrap();
    assert!(
        metrics(&geo).overlaps.is_empty(),
        "{:?}",
        metrics(&geo).overlaps
    );

    let temp = rect(&out, &uuid(&lock, "temp_hoch"));
    let and = rect(&out, &uuid(&lock, "beschatten"));
    // The GUI's footprint of a two-input gate, and the chain's direction.
    assert_eq!((and.w(), and.h()), (1344, 696));
    assert!(temp.px2 < and.px, "{temp:?} {and:?}");
    for r in [temp, and] {
        assert_eq!((r.px % GRID, r.py % GRID), (0, 0), "{r:?}");
    }
    // The lock records where they went, and a recompile is a fixpoint.
    assert_eq!(lock.objects["beschatten"].layout, Some(and));
    let again = compile(&out, &module(), &mut lock, &opts()).unwrap();
    assert_eq!(out.to_bytes(), again.to_bytes());
}

#[test]
fn a_gui_move_is_taken_over() {
    let mut lock = Lockfile::new();
    let out = compile(&base(), &module(), &mut lock, &opts()).unwrap();
    let and = uuid(&lock, "beschatten");
    let moved = gui_move(&out, &and, 9600, 4800);

    let out2 = compile(&moved, &module(), &mut lock, &opts()).unwrap();
    let at = rect(&out2, &and);
    assert_eq!((at.px, at.py), (9600, 4800));
    assert_eq!(lock.objects["beschatten"].layout, Some(at));
    assert_eq!(lock.objects["beschatten"].superseded_layout, None);
    let again = compile(&out2, &module(), &mut lock, &opts()).unwrap();
    assert_eq!(out2.to_bytes(), again.to_bytes());
}

#[test]
fn a_relayout_survives_a_stale_base() {
    let mut lock = Lockfile::new();
    let out = compile(&base(), &module(), &mut lock, &opts()).unwrap();
    let and = uuid(&lock, "beschatten");
    // Dragged far off, downloaded: the stale base.
    let stale = gui_move(&out, &and, 28800, 19200);
    let off = rect(&stale, &and);

    let laid = compile(&stale, &module(), &mut lock, &relayout()).unwrap();
    let fresh = rect(&laid, &and);
    assert_ne!(fresh, off);
    assert_eq!(lock.objects["beschatten"].layout, Some(fresh));
    assert_eq!(lock.objects["beschatten"].superseded_layout, Some(off));

    // Compiling against the stale base again keeps the fresh placement:
    // the old position is the one the relayout replaced, not a GUI move.
    let before = lock.to_json();
    let again = compile(&stale, &module(), &mut lock, &opts()).unwrap();
    assert_eq!(rect(&again, &and), fresh);
    assert_eq!(lock.to_json(), before);

    // Once the base carries it, the guard goes; a later drag is taken over.
    let synced = compile(&laid, &module(), &mut lock, &opts()).unwrap();
    assert_eq!(synced.to_bytes(), laid.to_bytes());
    assert_eq!(lock.objects["beschatten"].superseded_layout, None);
    let dragged = gui_move(&synced, &and, 28800, 19200);
    let out = compile(&dragged, &module(), &mut lock, &opts()).unwrap();
    assert_eq!(rect(&out, &and), off);
}

/// A base whose Beschattung page carries refs nobody in the next lock
/// owns — exactly like GUI-drawn ones: an InputRef mirroring VI1.
fn base_with_gui_ref() -> LoxoneDoc {
    let mirrors = Module::parse(
        "page \"Beschattung\"\n\
         extern quelle = VirtualIn(iname: \"VI1\")\n\
         \n\
         spiegel = InputRef(\n\
         \tmirrors: quelle,\n\
         \tAI: quelle.Q,\n\
         \tI: quelle.Qm,\n\
         )\n\
         \n\
         halter = Memory(\n\
         \tInput: spiegel.Q,\n\
         )\n",
    )
    .unwrap();
    compile(&base(), &mirrors, &mut Lockfile::new(), &opts()).unwrap()
}

#[test]
fn a_relayout_keeps_blocks_wired_to_foreign_refs() {
    let module = Module::parse(
        "page \"Beschattung\"\n\
         extern quelle = VirtualIn(iname: \"VI1\")\n\
         \n\
         anzeige = And(\n\
         \tI1: quelle.Q,\n\
         )\n\
         \n\
         folge = And(\n\
         \tI1: anzeige.Q,\n\
         )\n",
    )
    .unwrap();
    let mut lock = Lockfile::new();
    let out = compile(&base_with_gui_ref(), &module, &mut lock, &opts()).unwrap();
    let (anzeige, folge) = (uuid(&lock, "anzeige"), uuid(&lock, "folge"));
    let moved = gui_move(&gui_move(&out, &anzeige, 19200, 19200), &folge, 38400, 960);

    let laid = compile(&moved, &module, &mut lock, &relayout()).unwrap();
    // anzeige reads through the GUI's ref: it stays where the GUI put it.
    let a = rect(&laid, &anzeige);
    assert_eq!((a.px, a.py), (19200, 19200));
    // folge is ours alone: it moves up beside what feeds it.
    let f = rect(&laid, &folge);
    assert!(f.px > a.px2 && f.px - a.px2 < 4000, "{a:?} {f:?}");
    assert!((f.py - a.py).abs() < 1000, "{a:?} {f:?}");
}

#[test]
fn placement_is_per_page() {
    let xml = std::fs::read_to_string("examples/configs/haus.Loxone").unwrap();
    let two_pages = LoxoneDoc::parse(
        xml.replace(
            PAGE_BESCHATTUNG,
            &format!("{PAGE_WERKSTATT}{PAGE_BESCHATTUNG}"),
        )
        .as_bytes(),
    )
    .unwrap();
    let one = "page \"Beschattung\"\n\
               extern sonne = VirtualIn(iname: \"VI3\")\n\
               fern = And(I1: sonne.Q)\n";
    let mut lock = Lockfile::new();
    let out = compile(&two_pages, &Module::parse(one).unwrap(), &mut lock, &opts()).unwrap();
    // Far down on its own page.
    let moved = gui_move(&out, &uuid(&lock, "fern"), 960, 96000);

    // A new block on another page starts at that page's top, not below
    // the lowest block of the whole lock.
    let two = format!("{one}\npage \"Werkstatt\"\nnah = And(I1: sonne.Q)\n");
    let out = compile(&moved, &Module::parse(&two).unwrap(), &mut lock, &opts()).unwrap();
    let nah = rect(&out, &uuid(&lock, "nah"));
    assert!(nah.py < 4800, "{nah:?}");
}
