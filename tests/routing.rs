//! End-to-end tests for mirror routing (D34): a wire that names the
//! mirrored object directly is drawn through a same-page ref the base
//! already carries — and stays direct when no such ref exists.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn lxir(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_lxir"))
        .current_dir(dir)
        .args(args)
        .output()
        .expect("run lxir")
}

const PAGE_BESCHATTUNG: &str =
    "<C Type=\"Page\" V=\"175\" U=\"20000004-0000-0040-ffff504f94112233\" Title=\"Beschattung\">";
const PAGE_WERKSTATT: &str =
    "<C Type=\"Page\" V=\"175\" U=\"20000009-0000-0090-ffff504f94112233\" Title=\"Werkstatt\"></C>";

fn make_project(name: &str, page: &str, module: &str, base: Option<&Path>) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).unwrap();
    }
    std::fs::create_dir_all(dir.join("src")).unwrap();
    match base {
        Some(b) => {
            std::fs::copy(b, dir.join("haus.Loxone")).unwrap();
        }
        None => {
            // A second, empty page so cross-page behavior is testable.
            let xml = std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/examples/configs/haus.Loxone"
            ))
            .unwrap();
            let xml = xml.replace(
                PAGE_BESCHATTUNG,
                &format!("{PAGE_WERKSTATT}{PAGE_BESCHATTUNG}"),
            );
            std::fs::write(dir.join("haus.Loxone"), xml).unwrap();
        }
    }
    std::fs::write(
        dir.join("lox.toml"),
        format!(
            "base = \"haus.Loxone\"\n\
             module = \"src\"\n\
             lock = \"haus.lock.json\"\n\
             serial = \"504F94112233\"\n\
             page = \"{page}\"\n"
        ),
    )
    .unwrap();
    std::fs::write(dir.join("src/main.lxir"), module).unwrap();
    dir
}

/// Port uuid of connector `key` on the element whose tag carries `marker`.
fn port_of(xml: &str, marker: &str, key: &str) -> String {
    let at = xml.find(marker).unwrap_or_else(|| panic!("no {marker}"));
    let el_end = xml[at..].find("</C>").map_or(xml.len(), |e| at + e);
    let seg = &xml[at..el_end];
    let co = seg
        .find(&format!("<Co K=\"{key}\""))
        .unwrap_or_else(|| panic!("no Co {key} in {marker}"));
    let useg = &seg[co..];
    let u = useg.find(" U=\"").unwrap() + 4;
    useg[u..u + useg[u..].find('"').unwrap()].to_string()
}

/// The `<In Input=…>` sources of connector `key` on the marked element.
fn inputs_of(xml: &str, marker: &str, key: &str) -> Vec<String> {
    let at = xml.find(marker).unwrap_or_else(|| panic!("no {marker}"));
    let el_end = xml[at..].find("</C>").map_or(xml.len(), |e| at + e);
    let seg = &xml[at..el_end];
    let co = match seg.find(&format!("<Co K=\"{key}\"")) {
        Some(c) => c,
        None => return vec![],
    };
    let co_end = seg[co..]
        .find("</Co>")
        .map_or_else(|| seg[co..].find("/>").unwrap() + co, |e| e + co);
    regex_inputs(&seg[co..co_end])
}

fn regex_inputs(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(i) = rest.find("<In Input=\"") {
        let v = &rest[i + 11..];
        let end = v.find('"').unwrap();
        out.push(v[..end].to_string());
        rest = &v[end..];
    }
    out
}

/// A base with a GUI-style mirror: mint an InputRef of VI1 and an
/// OutputRef of VI3 (its `Qm` fed from the OutputRef's `AQ`, the way the
/// GUI wires actors), then use the *output* as the next project's base —
/// there the refs are unmanaged, exactly like GUI-created ones.
fn base_with_mirrors(name: &str) -> PathBuf {
    let dir = make_project(
        name,
        "Beschattung",
        "extern quelle = VirtualIn(iname: \"VI1\")\n\
         extern aktor = VirtualIn(iname: \"VI3\")\n\
         \n\
         spiegel = InputRef(\n\
         \tmirrors: quelle,\n\
         \tAI: quelle.Q,\n\
         \tI: quelle.Qm,\n\
         )\n\
         \n\
         halter = Memory(\n\
         \tInput: spiegel.Q,\n\
         )\n\
         \n\
         aus_spiegel = OutputRef(\n\
         \tmirrors: aktor,\n\
         \tAI: halter.Q,\n\
         )\n\
         \n\
         aktor.Qm <- aus_spiegel.AQ\n",
        None,
    );
    let out = lxir(&dir, &["compile"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    dir.join("out.Loxone")
}

#[test]
fn wires_route_through_same_page_mirrors() {
    let base = base_with_mirrors("routing-base-reuse");
    let dir = make_project(
        "routing-reuse",
        "Beschattung",
        "extern quelle = VirtualIn(iname: \"VI1\")\n\
         extern aktor = VirtualIn(iname: \"VI3\")\n\
         \n\
         anzeige = And(\n\
         \tI1: quelle.Q,\n\
         \tI2: quelle.Qm,\n\
         )\n\
         \n\
         aktor.Qm <- anzeige.Q\n",
        Some(&base),
    );
    let out = lxir(&dir, &["compile"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let xml = std::fs::read_to_string(dir.join("out.Loxone")).unwrap();
    // The consumer reads through the mirror: AI-fed serves AQ, I-fed
    // serves Q — not the VirtualIn's ports directly.
    let spiegel_aq = port_of(&xml, "<C Type=\"InputRef\"", "AQ");
    let spiegel_q = port_of(&xml, "<C Type=\"InputRef\"", "Q");
    assert_eq!(
        inputs_of(&xml, "Title=\"anzeige\"", "I1"),
        vec![spiegel_aq],
        "I1 must read the mirror's AQ"
    );
    assert_eq!(
        inputs_of(&xml, "Title=\"anzeige\"", "I2"),
        vec![spiegel_q],
        "I2 must read the mirror's Q"
    );
    // The actor write lands on the OutputRef's AI, not on the actor.
    let aus_ai = port_of(&xml, "<C Type=\"OutputRef\"", "AI");
    let anzeige_q = port_of(&xml, "Title=\"anzeige\"", "Q");
    let out_ref_at = xml.find("<C Type=\"OutputRef\"").unwrap();
    let out_ref_seg = &xml[out_ref_at..out_ref_at + xml[out_ref_at..].find("</C>").unwrap()];
    assert!(
        out_ref_seg.contains(&format!("<In Input=\"{anzeige_q}\"")),
        "anzeige.Q must land on the OutputRef's AI ({aus_ai}): {out_ref_seg}"
    );
    // Deterministic recompile.
    let before = std::fs::read(dir.join("out.Loxone")).unwrap();
    assert!(lxir(&dir, &["compile"]).status.success());
    assert_eq!(before, std::fs::read(dir.join("out.Loxone")).unwrap());
}

#[test]
fn cross_page_consumers_stay_direct() {
    let base = base_with_mirrors("routing-base-crosspage");
    let dir = make_project(
        "routing-crosspage",
        "Werkstatt",
        "extern quelle = VirtualIn(iname: \"VI1\")\n\
         extern senke = VirtualIn(iname: \"VI2\")\n\
         \n\
         anzeige = And(\n\
         \tI1: quelle.Q,\n\
         \tI2: quelle.Q,\n\
         )\n\
         \n\
         senke.Qm <- anzeige.Q\n",
        Some(&base),
    );
    let out = lxir(&dir, &["compile"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let xml = std::fs::read_to_string(dir.join("out.Loxone")).unwrap();
    // The mirror lives on Beschattung; the consumer on Werkstatt wires
    // the VirtualIn directly.
    let quelle_q = port_of(&xml, "IName=\"VI1\"", "Q");
    assert_eq!(
        inputs_of(&xml, "Title=\"anzeige\"", "I1"),
        vec![quelle_q.clone()],
        "cross-page wires must stay direct"
    );
    assert_eq!(
        inputs_of(&xml, "Title=\"anzeige\"", "I2"),
        vec![quelle_q],
        "cross-page wires must stay direct"
    );
}

#[test]
fn explicit_ref_externs_still_wire_literally() {
    let base = base_with_mirrors("routing-base-explicit");
    // The D32 form: name the ref itself, wire its ports — no rerouting,
    // and feed wires into a ref are never treated as consumer wires.
    let dir = make_project(
        "routing-explicit",
        "Beschattung",
        "extern quelle = VirtualIn(iname: \"VI1\")\n\
         extern spiegel = InputRef(mirrors: quelle)\n\
         \n\
         anzeige = And(\n\
         \tI1: spiegel.AQ,\n\
         \tI2: spiegel.Q,\n\
         )\n",
        Some(&base),
    );
    let out = lxir(&dir, &["compile"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let xml = std::fs::read_to_string(dir.join("out.Loxone")).unwrap();
    let spiegel_aq = port_of(&xml, "<C Type=\"InputRef\"", "AQ");
    assert_eq!(inputs_of(&xml, "Title=\"anzeige\"", "I1"), vec![spiegel_aq]);
}
