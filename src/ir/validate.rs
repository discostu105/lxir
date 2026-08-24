//! Static semantic validation against the verified builtin table.
//!
//! Everything here needs only the module and the compiled-in connector
//! table — no base config, no lockfile — so `lxir check` can catch the
//! whole class of wrong-type / wrong-port / wrong-direction mistakes before
//! a compile is ever attempted. `compile` runs the same checks first, so the
//! two entry points cannot drift apart.

use super::ast::Module;
use crate::connectors::{BUILTIN_TYPES, PortDir, builtin};
use crate::error::{Error, Result};

/// Check every managed block's type, port names, and wire directions
/// against the builtin table. Extern ports are open-world (they exist or
/// not in the base config) and are checked by `compile` instead.
pub fn validate_ports(module: &Module) -> Result<()> {
    // Types and body parameters.
    for block in module.blocks() {
        if builtin(&block.block_type).is_none() {
            let hint = suggest(&block.block_type, BUILTIN_TYPES.iter().copied());
            return Err(Error::Compile(format!(
                "block `{}`: type `{}` is not in the verified builtin table and \
                 cannot be created{hint}",
                block.slug, block.block_type
            )));
        }
        for (key, _) in block.params() {
            known_port(module, &block.slug, key)?;
        }
    }

    // Wire endpoints on managed blocks: the port must exist and its
    // direction must fit (source = output; sink = input or param).
    for w in module.wires() {
        for (endpoint, want) in [(&w.from, PortDir::Output), (&w.to, PortDir::Input)] {
            let Some(block) = module.blocks().find(|b| b.slug == endpoint.slug) else {
                continue; // extern — checked against the base config later
            };
            known_port(module, &block.slug, &endpoint.port)?;
            let specs = builtin(&block.block_type).expect("type validated above");
            let dir = specs
                .iter()
                .find(|s| s.key == endpoint.port)
                .map(|s| s.dir)
                .expect("port validated above");
            let ok = match want {
                PortDir::Output => dir == PortDir::Output,
                // Wire sinks accept inputs and params alike.
                PortDir::Input | PortDir::Param => dir != PortDir::Output,
            };
            if !ok {
                return Err(Error::Compile(format!(
                    "`{endpoint}` is an {} port and cannot be used as {}",
                    if dir == PortDir::Output {
                        "output"
                    } else {
                        "input"
                    },
                    if want == PortDir::Output {
                        "a wire source"
                    } else {
                        "a wire sink"
                    },
                )));
            }
        }
    }
    Ok(())
}

/// Error unless `key` is a port of the managed block `slug`.
fn known_port(module: &Module, slug: &str, key: &str) -> Result<()> {
    let block = module
        .blocks()
        .find(|b| b.slug == slug)
        .expect("caller resolved the slug to a block");
    let specs = builtin(&block.block_type).expect("caller validated the type");
    if specs.iter().any(|s| s.key == key) {
        return Ok(());
    }
    // Loxone Config 17 silently DELETES off-descriptor connectors (and
    // their wires) on save — verified via the Wine oracle with a grown `I3`
    // on `And`. Gates are fixed two-input; refusing here is what keeps
    // compiled logic from vanishing.
    let gate_hint = if matches!(block.block_type.as_str(), "And" | "Or")
        && key
            .strip_prefix('I')
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    {
        "; Loxone Config gates are fixed two-input (a grown I3 is \
         silently deleted on save) — cascade 2-input gates instead"
    } else {
        ""
    };
    let hint = suggest(key, specs.iter().map(|s| s.key));
    Err(Error::Compile(format!(
        "unknown port `{key}` on block `{slug}` (type `{}`); known ports: {}{hint}{gate_hint}",
        block.block_type,
        specs.iter().map(|s| s.key).collect::<Vec<_>>().join(", ")
    )))
}

/// A "; did you mean `…`?" hint when a candidate is within a small edit
/// distance of the input (case-insensitive), or `""` when nothing is close.
pub(crate) fn suggest<'a>(input: &str, candidates: impl IntoIterator<Item = &'a str>) -> String {
    let needle = input.to_ascii_lowercase();
    let cutoff = |candidate: &str| {
        if needle.len().min(candidate.len()) >= 6 {
            2
        } else {
            1
        }
    };
    candidates
        .into_iter()
        .filter_map(|c| {
            let d = edit_distance(&needle, &c.to_ascii_lowercase());
            (d <= cutoff(c)).then_some((d, c))
        })
        .min_by_key(|&(d, _)| d)
        .map(|(_, c)| format!("; did you mean `{c}`?"))
        .unwrap_or_default()
}

/// Levenshtein distance over bytes (inputs are ASCII identifiers).
fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let subst = prev[j] + usize::from(ca != cb);
            cur[j + 1] = subst.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance("", ""), 0);
        assert_eq!(edit_distance("abc", "abc"), 0);
        assert_eq!(edit_distance("abc", "abd"), 1);
        assert_eq!(edit_distance("abc", ""), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn suggest_finds_close_names_only() {
        assert_eq!(
            suggest("GreaterEqal", BUILTIN_TYPES.iter().copied()),
            "; did you mean `GreaterEqual`?"
        );
        // Case-insensitive.
        assert_eq!(
            suggest("greaterequal", BUILTIN_TYPES.iter().copied()),
            "; did you mean `GreaterEqual`?"
        );
        // Nothing close: no hint.
        assert_eq!(suggest("AutoJalousie", BUILTIN_TYPES.iter().copied()), "");
        // Short names use the tight cutoff.
        assert_eq!(suggest("Q1", ["Q", "I1", "I2"]), "; did you mean `Q`?");
        assert_eq!(suggest("xyz", ["Q", "I1", "I2"]), "");
    }

    #[test]
    fn unknown_type_and_port_carry_suggestions() {
        let m = Module::parse("block a: Monoflop {\n\tTme = 5\n}\n").unwrap();
        let err = validate_ports(&m).unwrap_err().to_string();
        assert!(err.contains("did you mean `Time`?"), "{err}");

        let m = Module::parse("block a: Monofop\n").unwrap();
        let err = validate_ports(&m).unwrap_err().to_string();
        assert!(err.contains("did you mean `Monoflop`?"), "{err}");
    }

    #[test]
    fn wire_ports_and_directions_are_checked_statically() {
        let m = Module::parse("block a: And\nblock b: And\nwire a.I1 -> b.I2\n").unwrap();
        let err = validate_ports(&m).unwrap_err().to_string();
        assert!(err.contains("wire source"), "{err}");

        let m = Module::parse("block a: And\nblock b: And\nwire a.Q -> b.Q\n").unwrap();
        let err = validate_ports(&m).unwrap_err().to_string();
        assert!(err.contains("wire sink"), "{err}");

        let m = Module::parse("block a: And\nblock b: And\nwire a.Q -> b.I3\n").unwrap();
        let err = validate_ports(&m).unwrap_err().to_string();
        assert!(err.contains("unknown port `I3`"), "{err}");
        assert!(err.contains("cascade"), "{err}");
    }

    #[test]
    fn valid_module_passes() {
        let m = Module::parse(
            "block a: And\nblock b: GreaterEqual {\n\tInput2 = 28\n}\nwire b.Q -> a.I1\n",
        )
        .unwrap();
        validate_ports(&m).unwrap();
    }
}
