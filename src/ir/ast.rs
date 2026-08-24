//! IR abstract syntax and canonical text emission.

use crate::error::{Error, Result};
use std::collections::BTreeMap;
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
    Let(LetDecl),
    Removed(RemovedDecl),
    Moved(MovedDecl),
    /// A whole-line `#` comment, stored verbatim (text after the `#`) so
    /// formatting is non-destructive. Statements carry their own trailing
    /// comments; block bodies carry theirs as [`BodyItem`]s.
    Comment(String),
}

/// A parameter/`set` value. The variant records how the value was written,
/// so canonical emission never has to guess from the content — a quoted
/// string stays quoted even when it happens to contain digits and signs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    /// Bare numeric literal, kept exactly as written (e.g. `-2.5`).
    /// Always matches `-?[0-9]+(\.[0-9]+)?`.
    Number(String),
    /// Quoted string, stored decoded. (A quoted string that reads as a
    /// number is canonicalized to [`Value::Number`] at parse time, so each
    /// value has exactly one canonical spelling.)
    Str(String),
    /// Bare identifier referencing a `let` constant.
    Ref(String),
}

impl Value {
    /// Classify a raw literal (e.g. a `Def=` value lifted from XML).
    pub fn from_literal(s: &str) -> Value {
        if is_number_literal(s) {
            Value::Number(s.to_string())
        } else {
            Value::Str(s.to_string())
        }
    }

    /// The literal content for `Number`/`Str`; `None` for a `Ref` (resolve
    /// it through [`Module::resolve_value`]).
    pub fn literal(&self) -> Option<&str> {
        match self {
            Value::Number(s) | Value::Str(s) => Some(s),
            Value::Ref(_) => None,
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Number(n) => f.write_str(n),
            Value::Str(s) => f.write_str(&quote(s)),
            Value::Ref(r) => f.write_str(r),
        }
    }
}

/// Whether `s` is exactly a number token: `-?[0-9]+(\.[0-9]+)?`.
pub(crate) fn is_number_literal(s: &str) -> bool {
    let s = s.strip_prefix('-').unwrap_or(s);
    let (int, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    digits(int) && frac.is_none_or(digits)
}

/// `extern slug: Type match kind "value"` — a reference to an object owned
/// by Loxone Config (hardware, system blocks, anything unmanaged).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternDecl {
    pub slug: String,
    pub block_type: String,
    pub match_spec: MatchSpec,
    /// Trailing `#` comment on the statement line, verbatim.
    pub comment: Option<String>,
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
    /// The `{ … }` body: parameters and whole-line comments, in source
    /// order.
    pub body: Vec<BodyItem>,
    /// Trailing `#` comment on the header line (after the `{` when a body
    /// follows).
    pub comment: Option<String>,
    /// Trailing `#` comment on the closing `}` line, verbatim.
    pub close_comment: Option<String>,
}

impl BlockDecl {
    /// The port parameters in the body, emitted as `Def=` on the
    /// corresponding connectors.
    pub fn params(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.body.iter().filter_map(|i| match i {
            BodyItem::Param(p) => Some((p.key.as_str(), &p.value)),
            BodyItem::Comment(_) => None,
        })
    }
}

/// One line of a block body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyItem {
    Param(ParamDecl),
    /// A whole-line `#` comment inside the body, verbatim.
    Comment(String),
}

/// `Key = value` inside a block body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamDecl {
    pub key: String,
    pub value: Value,
    /// Trailing `#` comment on the parameter line, verbatim.
    pub comment: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireDecl {
    pub from: PortRef,
    pub to: PortRef,
    /// Trailing `#` comment on the statement line, verbatim.
    pub comment: Option<String>,
}

/// `set slug.Port = value` — write a parameter (`Def=`) on an *extern* port;
/// the original value is preserved in the lockfile and restored when the
/// `set` is removed from source. On managed blocks, parameters belong in the
/// block body — `set` on a managed slug is a validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetDecl {
    pub target: PortRef,
    pub value: Value,
    /// Trailing `#` comment on the statement line, verbatim.
    pub comment: Option<String>,
}

/// `let name = value` — a named constant. Referenced by bare identifier in
/// any value position (block parameters, `set`). Pure substitution: the
/// compiler resolves references before emitting `Def=` values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LetDecl {
    pub name: String,
    /// Always `Number` or `Str` (constants cannot reference constants).
    pub value: Value,
    /// Trailing `#` comment on the statement line, verbatim.
    pub comment: Option<String>,
}

/// `removed slug` — declares that a managed block's absence from source is
/// intentional: the next compile deletes it from the config and drops it
/// from the lockfile. Scoped to one slug and reviewable in the diff (the
/// in-language counterpart of Terraform's `removed` block). A stale
/// `removed` whose slug is no longer in the lockfile is a no-op, so the
/// statement can be kept (or deleted) after it has been applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemovedDecl {
    pub slug: String,
    /// Trailing `#` comment on the statement line, verbatim.
    pub comment: Option<String>,
}

/// `moved old_slug -> new_slug` — renames a managed block's lockfile entry
/// so its identity (object and port UUIDs) survives a slug rename in source
/// (the in-language counterpart of Terraform's `moved` block). Idempotent:
/// once applied, a compile that finds `new_slug` already in the lock treats
/// the statement as done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovedDecl {
    pub from: String,
    pub to: String,
    /// Trailing `#` comment on the statement line, verbatim.
    pub comment: Option<String>,
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

/// What a declared name refers to, for reference checking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameKind {
    Extern,
    Block,
    Let,
}

impl NameKind {
    fn describe(self) -> &'static str {
        match self {
            NameKind::Extern => "an extern",
            NameKind::Block => "a managed block",
            NameKind::Let => "a `let` constant",
        }
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

    pub fn lets(&self) -> impl Iterator<Item = &LetDecl> {
        self.items.iter().filter_map(|i| match i {
            Item::Let(l) => Some(l),
            _ => None,
        })
    }

    pub fn removed(&self) -> impl Iterator<Item = &RemovedDecl> {
        self.items.iter().filter_map(|i| match i {
            Item::Removed(r) => Some(r),
            _ => None,
        })
    }

    pub fn moved(&self) -> impl Iterator<Item = &MovedDecl> {
        self.items.iter().filter_map(|i| match i {
            Item::Moved(m) => Some(m),
            _ => None,
        })
    }

    /// Resolve a value to the literal string that becomes `Def=`: literals
    /// resolve to themselves, `Ref`s through the module's `let` constants.
    pub fn resolve_value<'m>(&'m self, value: &'m Value) -> Result<&'m str> {
        match value {
            Value::Number(s) | Value::Str(s) => Ok(s),
            Value::Ref(name) => self
                .lets()
                .find(|l| l.name == *name)
                .and_then(|l| l.value.literal())
                .ok_or_else(|| Error::Compile(format!("undeclared constant `{name}`"))),
        }
    }

    /// Name uniqueness, reference resolution, and statement-level
    /// consistency (no base config needed). Port existence and directions
    /// on managed blocks are checked separately by
    /// [`super::validate_ports`].
    pub fn validate(&self) -> Result<()> {
        let compile_err = |msg: String| Err(Error::Compile(msg));

        // One namespace for externs, blocks, and constants.
        let mut names: BTreeMap<&str, NameKind> = BTreeMap::new();
        for item in &self.items {
            let (name, kind) = match item {
                Item::Extern(e) => (e.slug.as_str(), NameKind::Extern),
                Item::Block(b) => (b.slug.as_str(), NameKind::Block),
                Item::Let(l) => (l.name.as_str(), NameKind::Let),
                _ => continue,
            };
            if names.insert(name, kind).is_some() {
                return compile_err(format!("duplicate name `{name}`"));
            }
        }

        let object_ref = |r: &PortRef| -> Result<()> {
            match names.get(r.slug.as_str()) {
                None => compile_err(format!(
                    "reference to undeclared slug `{}` (in `{r}`)",
                    r.slug
                )),
                Some(NameKind::Let) => compile_err(format!(
                    "`{}` is a `let` constant, not a block or extern (in `{r}`)",
                    r.slug
                )),
                Some(_) => Ok(()),
            }
        };
        let value_refs = |value: &Value| -> Result<()> {
            let Value::Ref(name) = value else {
                return Ok(());
            };
            match names.get(name.as_str()) {
                Some(NameKind::Let) => Ok(()),
                Some(kind) => compile_err(format!(
                    "`{name}` is {}, not a `let` constant (quote the value if a \
                     string was intended)",
                    kind.describe()
                )),
                None => {
                    let hint = super::validate::suggest(name, self.lets().map(|l| l.name.as_str()));
                    compile_err(format!(
                        "undeclared constant `{name}`{hint} (declare it with \
                         `let {name} = …`, or quote the value if a string was intended)"
                    ))
                }
            }
        };

        for item in &self.items {
            match item {
                Item::Block(b) => {
                    for (_, value) in b.params() {
                        value_refs(value)?;
                    }
                }
                Item::Wire(w) => {
                    object_ref(&w.from)?;
                    object_ref(&w.to)?;
                }
                Item::Set(s) => {
                    object_ref(&s.target)?;
                    if names.get(s.target.slug.as_str()) == Some(&NameKind::Block) {
                        return compile_err(format!(
                            "`set {}` targets managed block `{slug}` — assign the \
                             parameter in the block body instead (`{port} = …` inside \
                             `block {slug}`); `set` is for extern ports only",
                            s.target,
                            slug = s.target.slug,
                            port = s.target.port,
                        ));
                    }
                    value_refs(&s.value)?;
                }
                _ => {}
            }
        }

        // Lifecycle statements must not contradict declarations or each
        // other.
        let mut removed_seen: BTreeMap<&str, ()> = BTreeMap::new();
        for r in self.removed() {
            if let Some(kind) = names.get(r.slug.as_str()) {
                return compile_err(format!(
                    "`removed {slug}` contradicts the declaration of {} `{slug}` — \
                     delete the `removed` line or the declaration",
                    kind.describe(),
                    slug = r.slug,
                ));
            }
            if removed_seen.insert(&r.slug, ()).is_some() {
                return compile_err(format!("duplicate `removed {}`", r.slug));
            }
        }
        let mut moved_from: BTreeMap<&str, ()> = BTreeMap::new();
        let mut moved_to: BTreeMap<&str, ()> = BTreeMap::new();
        for m in self.moved() {
            if m.from == m.to {
                return compile_err(format!("`moved {0} -> {0}` moves a slug to itself", m.from));
            }
            if let Some(kind) = names.get(m.from.as_str()) {
                return compile_err(format!(
                    "`moved {from} -> {to}` conflicts with the declaration of {} \
                     `{from}` — the old slug must no longer be declared",
                    kind.describe(),
                    from = m.from,
                    to = m.to,
                ));
            }
            if matches!(
                names.get(m.to.as_str()),
                Some(NameKind::Extern | NameKind::Let)
            ) {
                return compile_err(format!(
                    "`moved {} -> {to}` — `{to}` is not a managed block",
                    m.from,
                    to = m.to,
                ));
            }
            if moved_from.insert(&m.from, ()).is_some() {
                return compile_err(format!("duplicate `moved` from `{}`", m.from));
            }
            if moved_to.insert(&m.to, ()).is_some() {
                return compile_err(format!("duplicate `moved` to `{}`", m.to));
            }
            if removed_seen.contains_key(m.from.as_str())
                || removed_seen.contains_key(m.to.as_str())
            {
                return compile_err(format!(
                    "`moved {} -> {}` conflicts with a `removed` of the same slug",
                    m.from, m.to
                ));
            }
        }
        // No chains: a move's target must not be another move's source.
        for m in self.moved() {
            if moved_from.contains_key(m.to.as_str()) {
                return compile_err(format!(
                    "chained `moved` through `{}` — collapse into one `moved` statement",
                    m.to
                ));
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
            let tail =
                |c: &Option<String>| c.as_ref().map(|t| format!(" #{t}")).unwrap_or_default();
            match item {
                Item::Extern(e) => {
                    out.push_str(&format!(
                        "extern {}: {} {}{}\n",
                        e.slug,
                        e.block_type,
                        e.match_spec,
                        tail(&e.comment)
                    ));
                }
                Item::Block(b) => {
                    out.push_str(&format!("block {}: {}", b.slug, b.block_type));
                    if let Some(t) = &b.title {
                        out.push_str(&format!(" {}", quote(t)));
                    }
                    if b.body.is_empty() {
                        out.push_str(&tail(&b.comment));
                    } else {
                        out.push_str(&format!(" {{{}\n", tail(&b.comment)));
                        for bi in &b.body {
                            match bi {
                                BodyItem::Param(p) => out.push_str(&format!(
                                    "\t{} = {}{}\n",
                                    p.key,
                                    p.value,
                                    tail(&p.comment)
                                )),
                                BodyItem::Comment(text) => {
                                    out.push_str(&format!("\t#{text}\n"));
                                }
                            }
                        }
                        out.push('}');
                        out.push_str(&tail(&b.close_comment));
                    }
                    out.push('\n');
                }
                Item::Wire(w) => {
                    out.push_str(&format!(
                        "wire {} -> {}{}\n",
                        w.from,
                        w.to,
                        tail(&w.comment)
                    ));
                }
                Item::Set(s) => {
                    out.push_str(&format!(
                        "set {} = {}{}\n",
                        s.target,
                        s.value,
                        tail(&s.comment)
                    ));
                }
                Item::Let(l) => {
                    out.push_str(&format!(
                        "let {} = {}{}\n",
                        l.name,
                        l.value,
                        tail(&l.comment)
                    ));
                }
                Item::Removed(r) => {
                    out.push_str(&format!("removed {}{}\n", r.slug, tail(&r.comment)));
                }
                Item::Moved(m) => {
                    out.push_str(&format!(
                        "moved {} -> {}{}\n",
                        m.from,
                        m.to,
                        tail(&m.comment)
                    ));
                }
                Item::Comment(text) => {
                    out.push_str(&format!("#{text}\n"));
                }
            }
        }
        out
    }
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
