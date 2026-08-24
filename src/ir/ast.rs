//! IR abstract syntax and canonical text emission.

use crate::error::{Error, Result};
use std::collections::BTreeSet;
use std::fmt;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Module {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Extern(ExternDecl),
    Block(BlockDecl),
    Wire(WireDecl),
    Set(SetDecl),
    /// A whole-line `#` comment, stored verbatim (text after the `#`) so
    /// formatting is non-destructive. Trailing comments on statement lines
    /// and comments inside block bodies are *not* preserved.
    Comment(String),
}

/// `extern slug: Type match kind "value"` — a reference to an object owned
/// by Loxone Config (hardware, system blocks, anything unmanaged).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternDecl {
    pub slug: String,
    pub block_type: String,
    pub match_spec: MatchSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchSpec {
    Uuid(String),
    IName(String),
    Title(String),
}

impl fmt::Display for MatchSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MatchSpec::Uuid(v) => write!(f, "match uuid {}", quote(v)),
            MatchSpec::IName(v) => write!(f, "match iname {}", quote(v)),
            MatchSpec::Title(v) => write!(f, "match title {}", quote(v)),
        }
    }
}

/// `block slug: Type ["Title"] [{ Param = value … }]` — a managed block the
/// compiler owns end-to-end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockDecl {
    pub slug: String,
    pub block_type: String,
    pub title: Option<String>,
    /// Port parameters, emitted as `Def=` on the corresponding connector.
    pub params: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireDecl {
    pub from: PortRef,
    pub to: PortRef,
}

/// `set slug.Port = value` — write a parameter (`Def=`) on a port; on
/// externs the original value is preserved in the lockfile and restored when
/// the `set` is removed from source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetDecl {
    pub target: PortRef,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct PortRef {
    pub slug: String,
    pub port: String,
}

impl fmt::Display for PortRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.slug, self.port)
    }
}

impl Module {
    pub fn parse(src: &str) -> Result<Module> {
        let module = super::parser::parse(src)?;
        module.validate()?;
        Ok(module)
    }

    pub fn externs(&self) -> impl Iterator<Item = &ExternDecl> {
        self.items.iter().filter_map(|i| match i {
            Item::Extern(e) => Some(e),
            _ => None,
        })
    }

    pub fn blocks(&self) -> impl Iterator<Item = &BlockDecl> {
        self.items.iter().filter_map(|i| match i {
            Item::Block(b) => Some(b),
            _ => None,
        })
    }

    pub fn wires(&self) -> impl Iterator<Item = &WireDecl> {
        self.items.iter().filter_map(|i| match i {
            Item::Wire(w) => Some(w),
            _ => None,
        })
    }

    pub fn sets(&self) -> impl Iterator<Item = &SetDecl> {
        self.items.iter().filter_map(|i| match i {
            Item::Set(s) => Some(s),
            _ => None,
        })
    }

    /// Slug uniqueness and reference resolution.
    pub fn validate(&self) -> Result<()> {
        let mut slugs = BTreeSet::new();
        for item in &self.items {
            let slug = match item {
                Item::Extern(e) => &e.slug,
                Item::Block(b) => &b.slug,
                _ => continue,
            };
            if !slugs.insert(slug.clone()) {
                return Err(Error::Compile(format!("duplicate slug `{slug}`")));
            }
        }
        let check = |r: &PortRef| -> Result<()> {
            if !slugs.contains(&r.slug) {
                return Err(Error::Compile(format!(
                    "reference to undeclared slug `{}` (in `{r}`)",
                    r.slug
                )));
            }
            Ok(())
        };
        for item in &self.items {
            match item {
                Item::Wire(w) => {
                    check(&w.from)?;
                    check(&w.to)?;
                }
                Item::Set(s) => check(&s.target)?,
                _ => {}
            }
        }
        Ok(())
    }

    /// Canonical text form. `parse(to_text(m)) == m`.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let mut prev: Option<std::mem::Discriminant<Item>> = None;
        for item in &self.items {
            let disc = std::mem::discriminant(item);
            if prev.is_some_and(|p| p != disc) {
                out.push('\n');
            }
            prev = Some(disc);
            match item {
                Item::Extern(e) => {
                    out.push_str(&format!(
                        "extern {}: {} {}\n",
                        e.slug, e.block_type, e.match_spec
                    ));
                }
                Item::Block(b) => {
                    out.push_str(&format!("block {}: {}", b.slug, b.block_type));
                    if let Some(t) = &b.title {
                        out.push_str(&format!(" {}", quote(t)));
                    }
                    if !b.params.is_empty() {
                        out.push_str(" {\n");
                        for (k, v) in &b.params {
                            out.push_str(&format!("\t{k} = {}\n", value_token(v)));
                        }
                        out.push('}');
                    }
                    out.push('\n');
                }
                Item::Wire(w) => {
                    out.push_str(&format!("wire {} -> {}\n", w.from, w.to));
                }
                Item::Set(s) => {
                    out.push_str(&format!("set {} = {}\n", s.target, value_token(&s.value)));
                }
                Item::Comment(text) => {
                    out.push_str(&format!("#{text}\n"));
                }
            }
        }
        out
    }
}

/// Emit a value as a bare token when it reads as a number, quoted otherwise.
fn value_token(v: &str) -> String {
    let bare = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+'))
        && v.chars().any(|c| c.is_ascii_digit());
    if bare { v.to_string() } else { quote(v) }
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}
