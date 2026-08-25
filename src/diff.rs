//! Semantic diff between two `.Loxone` documents, keyed by UUID.
//!
//! Its main job is telling *real* edits apart from noise, in particular the
//! locale renames: saving a config in a differently-localized Loxone Config
//! renames every built-in object (Modes, weather fields, caption folders …)
//! into the UI language — 111 renames in one observed save with zero
//! semantic change. [`Rename::locale_suspect`] flags those.

use crate::connectors::attr_params;
use crate::doc::{LoxoneDoc, ObjectSummary, WireView, ports};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocDiff {
    pub added: Vec<ObjectSummary>,
    pub removed: Vec<ObjectSummary>,
    pub renamed: Vec<Rename>,
    pub param_changes: Vec<ParamChange>,
    pub wires_added: Vec<WireView>,
    pub wires_removed: Vec<WireView>,
}

impl DocDiff {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.renamed.is_empty()
            && self.param_changes.is_empty()
            && self.wires_added.is_empty()
            && self.wires_removed.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename {
    pub uuid: String,
    pub block_type: String,
    pub from: Option<String>,
    pub to: Option<String>,
    /// `true` when this looks like a locale-volatile built-in title rather
    /// than a deliberate user edit.
    pub locale_suspect: bool,
}

/// A `Def=` change on one port of an object present in both documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamChange {
    pub object_uuid: String,
    pub block_type: String,
    pub port_key: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// Compare two documents object-by-object (matched on UUID).
pub fn diff(a: &LoxoneDoc, b: &LoxoneDoc) -> DocDiff {
    let objs_a: BTreeMap<String, ObjectSummary> = a
        .objects()
        .into_iter()
        .map(|o| (o.uuid.clone(), o))
        .collect();
    let objs_b: BTreeMap<String, ObjectSummary> = b
        .objects()
        .into_iter()
        .map(|o| (o.uuid.clone(), o))
        .collect();

    let mut out = DocDiff::default();

    for (uuid, ob) in &objs_b {
        match objs_a.get(uuid) {
            None => out.added.push(ob.clone()),
            Some(oa) => {
                if oa.title != ob.title {
                    out.renamed.push(Rename {
                        uuid: uuid.clone(),
                        block_type: ob.block_type.clone(),
                        from: oa.title.clone(),
                        to: ob.title.clone(),
                        locale_suspect: locale_suspect(ob),
                    });
                }
                // Def= comparison per port key (ports matched by key: port
                // UUIDs are stable, but keys read better and are unique
                // within a block).
                let pa: BTreeMap<String, Option<String>> = doc_ports(a, oa);
                let pb: BTreeMap<String, Option<String>> = doc_ports(b, ob);
                for (key, def_b) in &pb {
                    let def_a = pa.get(key).cloned().unwrap_or(None);
                    if &def_a != def_b {
                        out.param_changes.push(ParamChange {
                            object_uuid: uuid.clone(),
                            block_type: ob.block_type.clone(),
                            port_key: key.clone(),
                            from: def_a,
                            to: def_b.clone(),
                        });
                    }
                }
                for (key, def_a) in &pa {
                    if !pb.contains_key(key) && def_a.is_some() {
                        out.param_changes.push(ParamChange {
                            object_uuid: uuid.clone(),
                            block_type: ob.block_type.clone(),
                            port_key: key.clone(),
                            from: def_a.clone(),
                            to: None,
                        });
                    }
                }
                // Attribute parameters (block logic stored as an element
                // attribute, e.g. `Formula=`) — a diff blind to them would
                // call a changed formula "semantically empty".
                for name in attr_params(&ob.block_type) {
                    let attr = |doc: &LoxoneDoc, o: &ObjectSummary| {
                        doc.element_at(&o.path)
                            .and_then(|el| el.attr_decoded(name).map(|v| v.into_owned()))
                    };
                    let (va, vb) = (attr(a, oa), attr(b, ob));
                    if va != vb {
                        out.param_changes.push(ParamChange {
                            object_uuid: uuid.clone(),
                            block_type: ob.block_type.clone(),
                            port_key: (*name).to_string(),
                            from: va,
                            to: vb,
                        });
                    }
                }
            }
        }
    }
    for (uuid, oa) in &objs_a {
        if !objs_b.contains_key(uuid) {
            out.removed.push(oa.clone());
        }
    }

    let wires_a: BTreeSet<WireView> = a.wires().into_iter().collect();
    let wires_b: BTreeSet<WireView> = b.wires().into_iter().collect();
    out.wires_added = wires_b.difference(&wires_a).cloned().collect();
    out.wires_removed = wires_a.difference(&wires_b).cloned().collect();
    out
}

fn doc_ports(doc: &LoxoneDoc, obj: &ObjectSummary) -> BTreeMap<String, Option<String>> {
    doc.element_at(&obj.path)
        .map(|el| ports(el).into_iter().map(|p| (p.key, p.def)).collect())
        .unwrap_or_default()
}

/// Heuristic for titles Loxone Config rewrites on locale change: caption
/// folders, built-in system objects, and objects with deterministic
/// (`00000000-…`) system UUIDs.
fn locale_suspect(obj: &ObjectSummary) -> bool {
    obj.uuid.starts_with("00000000-")
        || obj.block_type.ends_with("Caption")
        || matches!(
            obj.block_type.as_str(),
            "Mode"
                | "SysVar"
                | "GlobalStates"
                | "WeatherData"
                | "Calendar"
                | "CalendarEntry"
                | "RemoteControls"
                | "MessageCenter"
                | "AutoPilot"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(title: &str, def: &str, wired: bool) -> LoxoneDoc {
        let wire = if wired {
            "<In Input=\"00000009-0000-0001-00ff000000000009\"/>"
        } else {
            ""
        };
        let s = format!(
            "<ControlList Version=\"1\" NextObj=\"5\">\r\n\
             \t<C Type=\"Document\" U=\"00000001-0000-0000-ffff000000000001\">\r\n\
             \t\t<C Type=\"Page\" U=\"00000002-0000-0000-ffff000000000001\" Title=\"P\">\r\n\
             \t\t\t<C Type=\"Or\" U=\"00000003-0000-0000-ffff000000000001\" Title=\"{title}\">\r\n\
             \t\t\t\t<Co K=\"I2\" Def=\"{def}\" U=\"00000003-0000-0002-01ff000000000002\">{wire}</Co>\r\n\
             \t\t\t</C>\r\n\
             \t\t\t<C Type=\"ModeCaption\" U=\"00000000-0000-0004-1500000000000000\" Title=\"{title}\"/>\r\n\
             \t\t</C>\r\n\
             \t</C>\r\n\
             </ControlList>\r\n"
        );
        LoxoneDoc::parse(s.as_bytes()).unwrap()
    }

    #[test]
    fn detects_rename_param_and_wire_changes() {
        let a = doc("Alt", "1", false);
        let b = doc("Neu", "2", true);
        let d = diff(&a, &b);
        assert!(d.added.is_empty() && d.removed.is_empty());
        assert_eq!(d.renamed.len(), 2);
        let or = d.renamed.iter().find(|r| r.block_type == "Or").unwrap();
        assert!(!or.locale_suspect);
        let cap = d
            .renamed
            .iter()
            .find(|r| r.block_type == "ModeCaption")
            .unwrap();
        assert!(cap.locale_suspect, "Caption + system UUID → locale noise");
        assert_eq!(d.param_changes.len(), 1);
        assert_eq!(d.param_changes[0].from.as_deref(), Some("1"));
        assert_eq!(d.param_changes[0].to.as_deref(), Some("2"));
        assert_eq!(d.wires_added.len(), 1);
        assert!(d.wires_removed.is_empty());
    }

    #[test]
    fn identical_docs_diff_empty() {
        let a = doc("X", "1", true);
        assert!(diff(&a, &a).is_empty());
    }
}
