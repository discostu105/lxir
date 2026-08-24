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

use PortDir::{Input, Output};

// Verified live: Or{I1,I2,Q} at indexes 0,1,2 (And shares the shape).
const GATE: &[PortSpec] = &[p("I1", Input), p("I2", Input), p("Q", Output)];
// Verified live: Not{I,Q}.
const NOT: &[PortSpec] = &[p("I", Input), p("Q", Output)];
// Verified live: Equal{Input1,Input2,Q}; GreaterEqual shares the shape in
// both existing connector databases.
const CMP: &[PortSpec] = &[p("Input1", Input), p("Input2", Input), p("Q", Output)];

/// Verified port tables for the block types the compiler may create.
/// Order = connector-index order.
pub fn builtin(block_type: &str) -> Option<&'static [PortSpec]> {
    match block_type {
        "And" | "Or" => Some(GATE),
        "Not" => Some(NOT),
        "Equal" | "GreaterEqual" => Some(CMP),
        _ => None,
    }
}

/// Whether `key` is an auto-extendable input of a variadic gate
/// (Loxone Config grows `And`/`Or` inputs `I3`, `I4`, … on demand).
pub fn variadic_input(block_type: &str, key: &str) -> bool {
    matches!(block_type, "And" | "Or")
        && key.len() > 1
        && key.starts_with('I')
        && key[1..].chars().all(|c| c.is_ascii_digit())
}

/// Direction evidence for one port key of one block type, accumulated over
/// every occurrence in a document.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ObservedPort {
    /// Connector index from the port UUID (when consistent across
    /// occurrences; `None` if never seen or conflicting).
    pub index: Option<u8>,
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
                    None => entry.index = Some(idx),
                    Some(prev) if prev != idx => entry.index = None, // conflicting
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
        assert!(variadic_input("Or", "I7"));
        assert!(!variadic_input("Or", "Q"));
        assert!(!variadic_input("Not", "I2"));
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
