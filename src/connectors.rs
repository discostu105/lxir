//! Port-direction knowledge.
//!
//! Two sources, kept deliberately separate:
//!
//! 1. A small **builtin table** of block types the IR compiler may mint.
//!    Every entry has been verified against real Miniserver configs and/or
//!    agrees across the existing connector databases (lox-cli's map and
//!    lox-sim's signatures — which contradict each other for many other
//!    types; that is exactly why this table only contains verified entries).
//!    The list order is the connector-index order used in port UUIDs.
//!
//! 2. **Evidence-based inference** ([`observe`]) over real configs: which
//!    keys occur per type, at which connector index, whether they carry
//!    incoming wires (input), are referenced as a wire source (output), or
//!    hold `Def=` values (parameter). This is the seed for a consolidated
//!    connector database — every imported real config is a test case.

use crate::doc::{LoxoneDoc, ports};
use crate::uuid::LoxUuid;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum PortDir {
    Input,
    Output,
    Param,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortSpec {
    pub key: &'static str,
    pub dir: PortDir,
}

const fn p(key: &'static str, dir: PortDir) -> PortSpec {
    PortSpec { key, dir }
}

use PortDir::{Input, Output, Param};

// Verified live: Or{I1,I2,Q} at indexes 0,1,2 (And shares the shape).
const GATE: &[PortSpec] = &[p("I1", Input), p("I2", Input), p("Q", Output)];
// Verified live: Not{I,Q}.
const NOT: &[PortSpec] = &[p("I", Input), p("Q", Output)];
// Verified live: Equal{Input1,Input2,Q}; the whole comparator family
// (NotEqual, Greater, GreaterEqual, Less, LessEqual) shares the shape in
// both legacy connector databases.
const CMP: &[PortSpec] = &[p("Input1", Input), p("Input2", Input), p("Q", Output)];

// The entries below were admitted by the 2026-08-24 corpus consolidation
// run (6 real configs, evidence + legacy-db agreement — see
// docs/connector-db.md for the per-port rationale, including the inert-flag
// rule that classifies never-wired `Remanence` as Input).
const FORMULA: &[PortSpec] = &[
    p("Input1", Input),
    p("Input2", Input),
    p("Input3", Input),
    p("Input4", Input),
    p("AQ", Output),
    p("TQ", Output),
];
const MONOFLOP: &[PortSpec] = &[
    p("InputTrigger", Input),
    p("Reset", Input),
    p("Remanence", Input),
    p("Time", Param),
    p("Q", Output),
];
const PULSE_GEN: &[PortSpec] = &[
    p("InputEnable", Input),
    p("InputInvert", Input),
    p("Remanence", Input),
    p("TimeHigh", Param),
    p("TimeLow", Param),
    p("Q", Output),
];
const THRESHOLD: &[PortSpec] = &[
    p("Input", Input),
    p("Remanence", Input),
    p("On", Param),
    p("Off", Param),
    p("PulseTime", Param),
    p("Q", Output),
    p("RisingEdge", Output),
    p("FallingEdge", Output),
];

/// Every block type `builtin` knows — the complete mintable set.
pub const BUILTIN_TYPES: &[&str] = &[
    "And",
    "Or",
    "Not",
    "Equal",
    "NotEqual",
    "Greater",
    "GreaterEqual",
    "Less",
    "LessEqual",
    "Formula",
    "Monoflop",
    "PulseGen",
    "AnalogThresholdTrigger",
];

/// Verified port tables for the block types the compiler may create.
/// Order = connector-index order.
pub fn builtin(block_type: &str) -> Option<&'static [PortSpec]> {
    match block_type {
        "And" | "Or" => Some(GATE),
        "Not" => Some(NOT),
        "Equal" | "NotEqual" | "Greater" | "GreaterEqual" | "Less" | "LessEqual" => Some(CMP),
        "Formula" => Some(FORMULA),
        "Monoflop" => Some(MONOFLOP),
        "PulseGen" => Some(PULSE_GEN),
        "AnalogThresholdTrigger" => Some(THRESHOLD),
        _ => None,
    }
}

/// Direction evidence for one port key of one block type, accumulated over
/// every occurrence in a document (or, after [`merge`], a whole corpus).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ObservedPort {
    /// Connector index from the port UUID (when consistent across
    /// occurrences; `None` if never seen or conflicting — `index_conflict`
    /// distinguishes the two).
    pub index: Option<u8>,
    /// Two occurrences carried different connector indexes.
    pub index_conflict: bool,
    /// Occurrences where this port had incoming `<In>` wires → it is an input.
    pub wired_as_sink: u32,
    /// Occurrences where this port was referenced as a wire source → output.
    pub wired_as_source: u32,
    /// Occurrences carrying a `Def=` value → parameter-like.
    pub has_def: u32,
    pub occurrences: u32,
}

impl ObservedPort {
    /// Best direction guess from the evidence, if any.
    pub fn inferred_dir(&self) -> Option<PortDir> {
        match (self.wired_as_sink, self.wired_as_source) {
            (0, 0) if self.has_def > 0 => Some(PortDir::Param),
            (0, 0) => None,
            (s, o) if s >= o => Some(PortDir::Input),
            _ => Some(PortDir::Output),
        }
    }
}

/// Block type → port key → accumulated evidence.
pub type Observations = BTreeMap<String, BTreeMap<String, ObservedPort>>;

/// Scan a document and accumulate port-direction evidence.
pub fn observe(doc: &LoxoneDoc) -> Observations {
    let mut obs: Observations = BTreeMap::new();
    // Port UUID → (type, key), for resolving wire sources in pass 2.
    let mut owner: BTreeMap<String, (String, String)> = BTreeMap::new();

    for obj in doc.objects() {
        let el = doc.element_at(&obj.path).expect("path from objects()");
        for port in ports(el) {
            let entry = obs
                .entry(obj.block_type.clone())
                .or_default()
                .entry(port.key.clone())
                .or_default();
            entry.occurrences += 1;
            if !port.inputs.is_empty() {
                entry.wired_as_sink += 1;
            }
            if port.def.is_some() {
                entry.has_def += 1;
            }
            if let Ok(u) = LoxUuid::parse(&port.uuid)
                && let Some(idx) = u.connector_index()
            {
                match entry.index {
                    None if !entry.index_conflict => entry.index = Some(idx),
                    Some(prev) if prev != idx => {
                        entry.index = None;
                        entry.index_conflict = true;
                    }
                    _ => {}
                }
            }
            owner.insert(
                port.uuid.clone(),
                (obj.block_type.clone(), port.key.clone()),
            );
        }
    }

    for wire in doc.wires() {
        if let Some((ty, key)) = owner.get(&wire.from_port)
            && let Some(entry) = obs.get_mut(ty).and_then(|m| m.get_mut(key))
        {
            entry.wired_as_source += 1;
        }
    }
    obs
}

/// Merge evidence from another document (or corpus) into `into`.
///
/// Counters add; connector indexes stay only while every occurrence agrees
/// (a disagreement poisons the index permanently via `index_conflict`).
/// This is the aggregation step of the consolidated connector database:
/// `observe` each config separately (wire resolution is per-document),
/// then `merge` the results.
pub fn merge(into: &mut Observations, other: Observations) {
    for (ty, ports) in other {
        let target = into.entry(ty).or_default();
        for (key, o) in ports {
            let e = target.entry(key).or_default();
            e.wired_as_sink += o.wired_as_sink;
            e.wired_as_source += o.wired_as_source;
            e.has_def += o.has_def;
            e.occurrences += o.occurrences;
            e.index_conflict |= o.index_conflict;
            if let (Some(a), Some(b)) = (e.index, o.index)
                && a != b
            {
                e.index_conflict = true;
            }
            e.index = if e.index_conflict {
                None
            } else {
                e.index.or(o.index)
            };
        }
    }
}

/// One entry of a legacy connector database, in the `connector-map.json`
/// shape used by lox-cli: `c` = canonical connector order, `t` = port key →
/// `"I"` / `"O"`. (lox-sim's `block_signature` table converts into the same
/// shape: inputs+params as `"I"`, outputs as `"O"`.)
#[derive(Debug, Clone, serde::Deserialize)]
pub struct LegacyType {
    #[serde(default)]
    pub c: Vec<String>,
    #[serde(default)]
    pub t: BTreeMap<String, String>,
}

/// Block type → legacy entry.
pub type LegacyDb = BTreeMap<String, LegacyType>;

/// Comparison of corpus evidence against one legacy entry, for one type.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TypeCheck {
    /// Ports whose inferred direction matches the legacy direction.
    pub dir_agreements: u32,
    /// `"key: corpus=Input legacy=O"` — evidence contradicts the legacy db.
    pub dir_conflicts: Vec<String>,
    /// Keys seen in real configs but absent from the legacy db.
    pub only_in_corpus: Vec<String>,
    /// Keys the legacy db lists but no config ever materialized.
    pub only_in_legacy: Vec<String>,
    /// Set when the legacy `c` order disagrees with the observed
    /// connector-index order (compared over the keys both sides know).
    pub order_mismatch: Option<String>,
}

/// Cross-check corpus observations against a legacy database, reporting per
/// type present in **both**. Types known to only one side are not errors —
/// enumerate them with set operations on the two maps.
pub fn crosscheck(obs: &Observations, legacy: &LegacyDb) -> BTreeMap<String, TypeCheck> {
    let mut out = BTreeMap::new();
    for (ty, ports) in obs {
        let Some(l) = legacy.get(ty) else { continue };
        let mut c = TypeCheck::default();

        for (key, o) in ports {
            match (o.inferred_dir(), l.t.get(key).map(String::as_str)) {
                (_, None) => c.only_in_corpus.push(key.clone()),
                (None, Some(_)) => {} // no direction evidence — nothing to compare
                (Some(dir), Some(ldir)) => {
                    // Directions compare on the input/output axis: params
                    // are input-like on both sides (corpus `Param` = Def
                    // evidence; legacy `"P"` = connector-map's param flag).
                    let ours = match dir {
                        PortDir::Output => "O",
                        PortDir::Input | PortDir::Param => "I",
                    };
                    let ldir = if ldir == "P" { "I" } else { ldir };
                    if ours == ldir {
                        c.dir_agreements += 1;
                    } else {
                        c.dir_conflicts
                            .push(format!("{key}: corpus={dir:?} legacy={ldir}"));
                    }
                }
            }
        }
        for key in l.t.keys() {
            if !ports.contains_key(key) {
                c.only_in_legacy.push(key.clone());
            }
        }

        // Order: observed keys sorted by connector index, filtered to the
        // keys the legacy `c` list also names, must appear in the same order.
        let mut indexed: Vec<(&String, u8)> = ports
            .iter()
            .filter_map(|(k, o)| o.index.map(|i| (k, i)))
            .collect();
        indexed.sort_by_key(|&(_, i)| i);
        let observed_order: Vec<&String> = indexed
            .iter()
            .map(|&(k, _)| k)
            .filter(|k| l.c.contains(k))
            .collect();
        let legacy_order: Vec<&String> = l.c.iter().filter(|k| ports.contains_key(*k)).collect();
        if observed_order != legacy_order {
            c.order_mismatch = Some(format!(
                "observed {observed_order:?} vs legacy {legacy_order:?}"
            ));
        }

        out.insert(ty.clone(), c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_shapes() {
        let and = builtin("And").unwrap();
        assert_eq!(and.len(), 3);
        assert_eq!(and[2].key, "Q");
        assert_eq!(and[2].dir, PortDir::Output);
        assert!(
            builtin("AutoJalousie").is_none(),
            "not verified — must not be mintable"
        );
        // Corpus-consolidated entries: index order matches the observed
        // connector indexes (docs/connector-db.md).
        let formula = builtin("Formula").unwrap();
        assert_eq!(formula.len(), 6);
        assert_eq!((formula[0].key, formula[0].dir), ("Input1", PortDir::Input));
        assert_eq!((formula[4].key, formula[4].dir), ("AQ", PortDir::Output));
        assert_eq!((formula[5].key, formula[5].dir), ("TQ", PortDir::Output));
        let mono = builtin("Monoflop").unwrap();
        assert_eq!((mono[1].key, mono[1].dir), ("Reset", PortDir::Input));
        assert_eq!((mono[3].key, mono[3].dir), ("Time", PortDir::Param));
        assert_eq!((mono[4].key, mono[4].dir), ("Q", PortDir::Output));
        assert_eq!(builtin("Less").unwrap().len(), 3);
        assert_eq!(builtin("AnalogThresholdTrigger").unwrap().len(), 8);
        assert!(
            builtin("PulseAt").is_none(),
            "OutputAPI direction unresolved — must not be mintable yet"
        );
        assert!(builtin("Memory").is_none(), "Q direction unresolved");
        for t in BUILTIN_TYPES {
            assert!(builtin(t).is_some(), "BUILTIN_TYPES lists `{t}`");
        }
    }

    fn port(index: Option<u8>, sink: u32, source: u32, def: u32) -> ObservedPort {
        ObservedPort {
            index,
            index_conflict: false,
            wired_as_sink: sink,
            wired_as_source: source,
            has_def: def,
            occurrences: 1,
        }
    }

    fn obs(ty: &str, ports: &[(&str, ObservedPort)]) -> Observations {
        let mut o = Observations::new();
        let m = o.entry(ty.into()).or_default();
        for (k, p) in ports {
            m.insert((*k).into(), p.clone());
        }
        o
    }

    #[test]
    fn merging() {
        let mut a = obs("T", &[("Q", port(Some(2), 0, 1, 0))]);
        merge(&mut a, obs("T", &[("Q", port(Some(2), 0, 3, 0))]));
        let q = &a["T"]["Q"];
        assert_eq!((q.index, q.wired_as_source, q.occurrences), (Some(2), 4, 2));
        assert!(!q.index_conflict);

        // Conflicting index poisons permanently, even against later agreement.
        merge(&mut a, obs("T", &[("Q", port(Some(5), 0, 1, 0))]));
        assert!(a["T"]["Q"].index_conflict);
        assert_eq!(a["T"]["Q"].index, None);
        merge(&mut a, obs("T", &[("Q", port(Some(2), 0, 1, 0))]));
        assert_eq!(a["T"]["Q"].index, None);

        // New types/keys appear.
        merge(&mut a, obs("U", &[("I", port(Some(0), 1, 0, 0))]));
        assert_eq!(a["U"]["I"].index, Some(0));
    }

    #[test]
    fn crosschecking() {
        let o = obs(
            "And",
            &[
                ("I1", port(Some(0), 2, 0, 0)),
                ("I2", port(Some(1), 1, 0, 1)),
                ("Q", port(Some(2), 0, 3, 0)),
                ("Mystery", port(Some(3), 0, 0, 2)), // Param, unknown to legacy
            ],
        );
        let legacy: LegacyDb = serde_json::from_str(
            r#"{"And": {"c": ["I1","I2","Q"],
                        "t": {"I1":"I","I2":"I","Q":"I","Ghost":"O"}},
                "OnlyLegacy": {"c": [], "t": {}}}"#,
        )
        .unwrap();
        let checks = crosscheck(&o, &legacy);
        let c = &checks["And"];
        assert_eq!(c.dir_agreements, 2, "I1, I2");
        assert_eq!(c.dir_conflicts, ["Q: corpus=Output legacy=I"]);
        assert_eq!(c.only_in_corpus, ["Mystery"]);
        assert_eq!(c.only_in_legacy, ["Ghost"]);
        assert!(c.order_mismatch.is_none());
        assert!(!checks.contains_key("OnlyLegacy"), "intersection only");
    }

    #[test]
    fn inference() {
        let mut o = ObservedPort::default();
        assert_eq!(o.inferred_dir(), None);
        o.has_def = 1;
        assert_eq!(o.inferred_dir(), Some(PortDir::Param));
        o.wired_as_source = 2;
        assert_eq!(o.inferred_dir(), Some(PortDir::Output));
        o.wired_as_sink = 3;
        assert_eq!(o.inferred_dir(), Some(PortDir::Input));
    }
}
