//! Hand-rolled parser for the IR text format. Line-oriented; only a block
//! declaration's `( … )` argument list spans lines.

use super::ast::*;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(String),
    Colon,
    Dot,
    Comma,
    Arrow,
    LArrow,
    Eq,
    LParen,
    RParen,
    LBrace,
    RBrace,
}

fn lex(line: &str, lineno: usize) -> Result<(Vec<Tok>, Option<String>)> {
    let err = |msg: String| Error::IrParse { line: lineno, msg };
    let mut toks = Vec::new();
    let mut comment = None;
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '#' => {
                chars.next();
                comment = Some(chars.collect::<String>());
                break;
            }
            c if c.is_whitespace() => {
                chars.next();
            }
            ':' => {
                chars.next();
                toks.push(Tok::Colon);
            }
            '.' => {
                chars.next();
                toks.push(Tok::Dot);
            }
            ',' => {
                chars.next();
                toks.push(Tok::Comma);
            }
            '=' => {
                chars.next();
                toks.push(Tok::Eq);
            }
            '(' => {
                chars.next();
                toks.push(Tok::LParen);
            }
            ')' => {
                chars.next();
                toks.push(Tok::RParen);
            }
            '{' => {
                chars.next();
                toks.push(Tok::LBrace);
            }
            '}' => {
                chars.next();
                toks.push(Tok::RBrace);
            }
            '<' => {
                chars.next();
                if chars.peek() == Some(&'-') {
                    chars.next();
                    toks.push(Tok::LArrow);
                } else {
                    return Err(err(
                        "unexpected `<` (a wire is `target.Port <- source.Port`)".into(),
                    ));
                }
            }
            '-' => {
                chars.next();
                if chars.peek() == Some(&'>') {
                    chars.next();
                    toks.push(Tok::Arrow);
                } else {
                    // negative number
                    let mut s = String::from('-');
                    while let Some(&d) = chars.peek() {
                        if d.is_ascii_digit() || d == '.' {
                            s.push(d);
                            chars.next();
                        } else {
                            break;
                        }
                    }
                    if s == "-" {
                        return Err(err("unexpected `-`".into()));
                    }
                    toks.push(number(s, lineno)?);
                }
            }
            '"' => {
                chars.next();
                let mut s = String::new();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('"') => s.push('"'),
                            Some('\\') => s.push('\\'),
                            Some('n') => s.push('\n'),
                            other => {
                                return Err(err(format!("invalid escape `\\{}`", opt(other))));
                            }
                        },
                        Some(c) => s.push(c),
                        None => return Err(err("unterminated string".into())),
                    }
                }
                toks.push(Tok::Str(s));
            }
            c if c.is_ascii_digit() => {
                let mut s = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_ascii_digit() || d == '.' {
                        s.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                toks.push(number(s, lineno)?);
            }
            c if c.is_alphanumeric() || c == '_' => {
                let mut s = String::new();
                while let Some(&d) = chars.peek() {
                    if d.is_alphanumeric() || d == '_' {
                        s.push(d);
                        chars.next();
                    } else {
                        break;
                    }
                }
                toks.push(Tok::Ident(s));
            }
            other => return Err(err(format!("unexpected character `{other}`"))),
        }
    }
    Ok((toks, comment))
}

/// A lexed digit-and-dot run must be exactly `-?digits(.digits)?`.
fn number(s: String, lineno: usize) -> Result<Tok> {
    if is_number_literal(&s) {
        Ok(Tok::Num(s))
    } else {
        Err(Error::IrParse {
            line: lineno,
            msg: format!("invalid number literal `{s}` (expected digits with at most one `.`)"),
        })
    }
}

fn opt(c: Option<char>) -> String {
    c.map(String::from).unwrap_or_else(|| "<eol>".into())
}

pub fn parse(src: &str) -> Result<Module> {
    let mut items = Vec::new();
    // A block declaration whose `( … )` argument list is still open.
    let mut open_call: Option<BlockDecl> = None;

    for (i, raw) in src.lines().enumerate() {
        let lineno = i + 1;
        let err = |msg: String| Error::IrParse { line: lineno, msg };
        let (toks, comment) = lex(raw, lineno)?;

        if let Some(call) = open_call.as_mut() {
            // Inside an open argument list: arguments, comments, or `)`.
            if toks.is_empty() {
                if let Some(text) = comment {
                    call.args.push(ArgItem::Comment(text));
                }
                continue;
            }
            let outcome = parse_call_args(call, &toks, lineno)?;
            if outcome.closed {
                call.close_comment = comment;
                items.push(Item::Block(open_call.take().unwrap()));
            } else if let Some(text) = comment {
                match call.args.last_mut() {
                    Some(ArgItem::Binding(b)) if outcome.pushed_binding => b.comment = Some(text),
                    _ => call.args.push(ArgItem::Comment(text)),
                }
            }
            continue;
        }

        if toks.is_empty() {
            // Blank line, or a whole-line comment — kept verbatim as an
            // item so formatting is non-destructive.
            if let Some(text) = comment {
                items.push(Item::Comment(text));
            }
            continue;
        }

        match &toks[0] {
            Tok::Ident(kw) if kw == "let" => match toks.as_slice() {
                [_, Tok::Ident(name), Tok::Eq, val] => {
                    let value = match value_of(val, lineno)? {
                        Value::Ref(other) => {
                            return Err(err(format!(
                                "`let {name} = {other}` — a constant's value must be a \
                                 number or a quoted string, not another identifier"
                            )));
                        }
                        literal => literal,
                    };
                    items.push(Item::Let(LetDecl {
                        name: check_slug(name, lineno)?,
                        value,
                        comment,
                    }));
                }
                _ => return Err(err("expected `let <name> = <number | \"string\">`".into())),
            },
            Tok::Ident(kw) if kw == "extern" => match toks.as_slice() {
                [
                    _,
                    Tok::Ident(slug),
                    Tok::Eq,
                    Tok::Ident(ty),
                    Tok::LParen,
                    rest @ ..,
                    Tok::RParen,
                ] => {
                    // `kind: "value"` pairs, comma-separated: one primary
                    // matcher (uuid|iname|title) first, then optional
                    // `room:` / `category:` constraints.
                    let mut pairs = Vec::new();
                    let mut it = rest.iter();
                    loop {
                        match (it.next(), it.next(), it.next()) {
                            (Some(Tok::Ident(k)), Some(Tok::Colon), Some(Tok::Str(v))) => {
                                pairs.push((k.as_str(), v.clone()));
                            }
                            _ => {
                                return Err(err(
                                    "expected `extern <slug> = <Type>(uuid|iname|title: \"…\"\
                                     [, room: \"…\"] [, category: \"…\"])`"
                                        .into(),
                                ));
                            }
                        }
                        match it.next() {
                            None => break,
                            Some(Tok::Comma) => continue,
                            Some(_) => {
                                return Err(err("expected `,` between matchers".into()));
                            }
                        }
                    }
                    let mut pairs = pairs.into_iter();
                    let match_spec = match pairs.next() {
                        Some(("uuid", v)) => MatchSpec::Uuid(v),
                        Some(("iname", v)) => MatchSpec::IName(v),
                        Some(("title", v)) => MatchSpec::Title(v),
                        Some((other, _)) => {
                            return Err(err(format!(
                                "unknown matcher `{other}` (expected uuid, iname, or title \
                                 first; room/category only narrow it)"
                            )));
                        }
                        None => return Err(err("empty matcher list".into())),
                    };
                    let (mut room, mut category) = (None, None);
                    for (k, v) in pairs {
                        let slot = match k {
                            "room" => &mut room,
                            "category" => &mut category,
                            other => {
                                return Err(err(format!(
                                    "unknown constraint `{other}` (expected room or category)"
                                )));
                            }
                        };
                        if slot.replace(v).is_some() {
                            return Err(err(format!("duplicate `{k}:` constraint")));
                        }
                    }
                    if matches!(match_spec, MatchSpec::Uuid(_))
                        && (room.is_some() || category.is_some())
                    {
                        return Err(err("`uuid:` pins exactly — room/category constraints are \
                             redundant and not allowed with it"
                            .into()));
                    }
                    items.push(Item::Extern(ExternDecl {
                        slug: check_slug(slug, lineno)?,
                        block_type: check_type(ty, lineno)?,
                        match_spec,
                        room,
                        category,
                        comment,
                    }));
                }
                _ => {
                    return Err(err(
                        "expected `extern <slug> = <Type>(uuid|iname|title: \"…\")`".into(),
                    ));
                }
            },
            Tok::Ident(kw) if kw == "removed" => match toks.as_slice() {
                [_, Tok::Ident(slug)] => {
                    items.push(Item::Removed(RemovedDecl {
                        slug: check_slug(slug, lineno)?,
                        comment,
                    }));
                }
                _ => return Err(err("expected `removed <slug>`".into())),
            },
            Tok::Ident(kw) if kw == "moved" => match toks.as_slice() {
                [_, Tok::Ident(from), Tok::Arrow, Tok::Ident(to)] => {
                    items.push(Item::Moved(MovedDecl {
                        from: check_slug(from, lineno)?,
                        to: check_slug(to, lineno)?,
                        comment,
                    }));
                }
                _ => return Err(err("expected `moved <old_slug> -> <new_slug>`".into())),
            },
            // v0 keywords: reserved, with migration guidance.
            Tok::Ident(kw) if kw == "block" => {
                return Err(err(
                    "the `block` keyword is v0 syntax — declare a managed block as \
                     `<slug> = <Type>(…)` with its inputs and parameters as arguments"
                        .into(),
                ));
            }
            Tok::Ident(kw) if kw == "wire" => {
                return Err(err(
                    "the `wire` keyword is v0 syntax — a wire into a managed block is \
                     written in its argument list (`<Port>: <source>.<Port>`); a wire \
                     onto an extern port is `<extern>.<Port> <- <source>.<Port>`"
                        .into(),
                ));
            }
            Tok::Ident(kw) if kw == "set" => {
                return Err(err(
                    "the `set` keyword is v0 syntax — write `<extern>.<Port> = <value>`".into(),
                ));
            }
            Tok::LBrace | Tok::RBrace => {
                return Err(err(
                    "v0 `{ … }` block bodies were replaced by argument lists \
                     (`<slug> = <Type>(…)`)"
                        .into(),
                ));
            }
            Tok::Ident(_) => {
                parse_ident_statement(&toks, comment, lineno, &mut items, &mut open_call)?;
            }
            _ => {
                return Err(err(
                    "expected a statement: `let`, `extern`, `<slug> = <Type>(…)`, \
                     `<slug>.<Port> <- <slug>.<Port>`, `<slug>.<Port> = <value>`, \
                     `removed`, or `moved`"
                        .into(),
                ));
            }
        }
    }

    if let Some(call) = open_call {
        return Err(Error::IrParse {
            line: src.lines().count(),
            msg: format!("unclosed `(` in `{} = {}(…`", call.slug, call.block_type),
        });
    }
    Ok(Module { items })
}

/// Statements that start with a plain identifier: block declarations
/// (`slug = Type(…)`) and extern port statements (`slug.Port <- …` /
/// `slug.Port = …`).
fn parse_ident_statement(
    toks: &[Tok],
    comment: Option<String>,
    lineno: usize,
    items: &mut Vec<Item>,
    open_call: &mut Option<BlockDecl>,
) -> Result<()> {
    let err = |msg: String| Error::IrParse { line: lineno, msg };
    match toks {
        // slug = Type( … — a block declaration (possibly spanning lines).
        [
            Tok::Ident(slug),
            Tok::Eq,
            Tok::Ident(ty),
            Tok::LParen,
            rest @ ..,
        ] => {
            let mut call = BlockDecl {
                slug: check_slug(slug, lineno)?,
                block_type: check_type(ty, lineno)?,
                title: None,
                args: Vec::new(),
                comment: None,
                close_comment: None,
            };
            let outcome = parse_call_args(&mut call, rest, lineno)?;
            // The header line's comment stays on the header — for a call
            // closed on this same line it is the statement comment.
            call.comment = comment;
            if outcome.closed {
                items.push(Item::Block(call));
            } else {
                *open_call = Some(call);
            }
            Ok(())
        }
        [Tok::Ident(slug), Tok::Eq, Tok::Ident(ty)] => Err(err(format!(
            "expected `(` after the type — a block declaration is `{slug} = {ty}(…)` \
             (a constant is `let {slug} = …`)"
        ))),
        [Tok::Ident(slug), Tok::Eq, Tok::Num(v)] => Err(err(format!(
            "`{slug} = {v}` — did you mean `let {slug} = {v}`? (a bare name on the \
             left is a constant or block declaration; an extern port is `{slug}.<Port> = {v}`)"
        ))),
        [Tok::Ident(slug), Tok::Eq, Tok::Str(_)] => Err(err(format!(
            "`{slug} = \"…\"` — did you mean `let {slug} = \"…\"`?"
        ))),
        // slug.Port <- slug.Port — a wire onto an extern port.
        [
            Tok::Ident(ts),
            Tok::Dot,
            Tok::Ident(tp),
            Tok::LArrow,
            Tok::Ident(fs),
            Tok::Dot,
            Tok::Ident(fp),
        ] => {
            items.push(Item::Wire(WireDecl {
                to: PortRef {
                    slug: ts.clone(),
                    port: tp.clone(),
                },
                from: PortRef {
                    slug: fs.clone(),
                    port: fp.clone(),
                },
                comment,
            }));
            Ok(())
        }
        [Tok::Ident(ts), Tok::Dot, Tok::Ident(tp), Tok::LArrow, ..] => Err(err(format!(
            "expected a source port after `<-` (`{ts}.{tp} <- <slug>.<Port>`); \
             to assign a value, use `{ts}.{tp} = <value>`"
        ))),
        // slug.Port = value — a Def write on an extern port.
        [
            Tok::Ident(ts),
            Tok::Dot,
            Tok::Ident(tp),
            Tok::Eq,
            Tok::Ident(vs),
            Tok::Dot,
            Tok::Ident(vp),
        ] => Err(err(format!(
            "`{ts}.{tp} = {vs}.{vp}` — use `<-` to wire a port: `{ts}.{tp} <- {vs}.{vp}`"
        ))),
        [Tok::Ident(ts), Tok::Dot, Tok::Ident(tp), Tok::Eq, val] => {
            items.push(Item::Set(SetDecl {
                target: PortRef {
                    slug: ts.clone(),
                    port: tp.clone(),
                },
                value: value_of(val, lineno)?,
                comment,
            }));
            Ok(())
        }
        [Tok::Ident(ts), Tok::Dot, ..] => Err(err(format!(
            "expected `{ts}.<Port> <- <slug>.<Port>` or `{ts}.<Port> = <value>`"
        ))),
        _ => Err(err(
            "expected a statement: `let`, `extern`, `<slug> = <Type>(…)`, \
             `<slug>.<Port> <- <slug>.<Port>`, `<slug>.<Port> = <value>`, \
             `removed`, or `moved`"
                .into(),
        )),
    }
}

struct ArgOutcome {
    /// The closing `)` was consumed.
    closed: bool,
    /// At least one binding was appended by this line (comment attach).
    pushed_binding: bool,
}

/// Parse argument-list tokens (label string, `Port: value` bindings,
/// commas, closing `)`), appending to `call`.
fn parse_call_args(call: &mut BlockDecl, toks: &[Tok], lineno: usize) -> Result<ArgOutcome> {
    let err = |msg: String| Error::IrParse { line: lineno, msg };
    let mut i = 0;
    let mut pushed_binding = false;
    loop {
        match toks.get(i) {
            None => {
                return Ok(ArgOutcome {
                    closed: false,
                    pushed_binding,
                });
            }
            Some(Tok::RParen) => {
                if i + 1 != toks.len() {
                    return Err(err("nothing may follow `)` on the line".into()));
                }
                return Ok(ArgOutcome {
                    closed: true,
                    pushed_binding,
                });
            }
            Some(Tok::Str(s)) => {
                if call.title.is_some() {
                    return Err(err(
                        "duplicate label — a block takes one label string".into()
                    ));
                }
                if !call.args.iter().any(|a| matches!(a, ArgItem::Binding(_))) {
                    call.title = Some(s.clone());
                } else {
                    return Err(err("the label string must be the first argument".into()));
                }
                i += 1;
            }
            Some(Tok::Ident(port)) => {
                if toks.get(i + 1) != Some(&Tok::Colon) {
                    return Err(err(format!(
                        "expected `:` after argument name `{port}` (`{port}: <value>`)"
                    )));
                }
                let (kind, consumed) = match toks.get(i + 2) {
                    Some(Tok::Num(n)) => (BindingKind::Param(Value::Number(n.clone())), 1),
                    // A quoted string that reads as a number canonicalizes
                    // to the bare number — one canonical spelling per value.
                    Some(Tok::Str(s)) => (BindingKind::Param(Value::from_literal(s)), 1),
                    Some(Tok::Ident(name)) => {
                        if toks.get(i + 3) == Some(&Tok::Dot) {
                            match toks.get(i + 4) {
                                Some(Tok::Ident(src_port)) => (
                                    BindingKind::Wire(PortRef {
                                        slug: name.clone(),
                                        port: src_port.clone(),
                                    }),
                                    3,
                                ),
                                _ => {
                                    return Err(err(format!("expected a port after `{name}.`")));
                                }
                            }
                        } else {
                            (BindingKind::Param(Value::Ref(name.clone())), 1)
                        }
                    }
                    _ => {
                        return Err(err(format!(
                            "expected a value after `{port}:` — a number, quoted string, \
                             constant name, or source port (`<slug>.<Port>`)"
                        )));
                    }
                };
                call.args.push(ArgItem::Binding(Binding {
                    port: port.clone(),
                    kind,
                    comment: None,
                }));
                pushed_binding = true;
                i += 2 + consumed;
            }
            Some(_) => {
                return Err(err(
                    "expected `Port: value`, a label string, or `)` in the argument list".into(),
                ));
            }
        }
        // Separator: a comma, or the end of the list / line.
        match toks.get(i) {
            Some(Tok::Comma) => i += 1,
            Some(Tok::RParen) | None => {}
            Some(_) => return Err(err("expected `,` between arguments".into())),
        }
    }
}

fn value_of(tok: &Tok, lineno: usize) -> Result<Value> {
    match tok {
        Tok::Num(n) => Ok(Value::Number(n.clone())),
        // A quoted string that reads as a number canonicalizes to the bare
        // number — one canonical spelling per value.
        Tok::Str(s) => Ok(Value::from_literal(s)),
        Tok::Ident(s) => Ok(Value::Ref(s.clone())),
        _ => Err(Error::IrParse {
            line: lineno,
            msg: "expected a number, quoted string, or constant name".into(),
        }),
    }
}

fn check_slug(s: &str, lineno: usize) -> Result<String> {
    let ok = s.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok {
        Ok(s.to_string())
    } else {
        Err(Error::IrParse {
            line: lineno,
            msg: format!("invalid slug `{s}` (expected [a-z][a-z0-9_]*)"),
        })
    }
}

fn check_type(s: &str, lineno: usize) -> Result<String> {
    let ok = s.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && s.chars().all(|c| c.is_ascii_alphanumeric());
    if ok {
        Ok(s.to_string())
    } else {
        Err(Error::IrParse {
            line: lineno,
            msg: format!("invalid block type `{s}` (expected PascalCase, e.g. GreaterEqual)"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
# Beschattung
extern sonne = VirtualIn(iname: "VI3")
extern jal = AutoJalousie(title: "Beschattung Süd")

let schwelle = 28

temp_hoch = GreaterEqual(
	"Temp über 28",
	Input1: sonne.Q,
	Input2: schwelle,
)
beschatten = And(I1: temp_hoch.Q, I2: sonne.Q)

jal.AutoShade <- beschatten.Q
jal.TargetPos = 70
"#;

    #[test]
    fn parses_sample() {
        let m = Module::parse(SAMPLE).unwrap();
        assert_eq!(m.externs().count(), 2);
        assert_eq!(m.blocks().count(), 2);
        assert_eq!(m.extern_wires().count(), 1);
        assert_eq!(m.wire_pairs().len(), 4);
        assert_eq!(m.sets().count(), 1);
        assert_eq!(m.lets().count(), 1);
        let block = m.blocks().next().unwrap();
        assert_eq!(block.title.as_deref(), Some("Temp über 28"));
        let params: Vec<_> = block.params().collect();
        assert_eq!(params, vec![("Input2", &Value::Ref("schwelle".into()))]);
        assert_eq!(m.resolve_value(params[0].1).unwrap(), "28");
        let wires: Vec<_> = block.input_wires().collect();
        assert_eq!(wires.len(), 1);
        assert_eq!(wires[0].0, "Input1");
        assert_eq!(wires[0].1.to_string(), "sonne.Q");
    }

    #[test]
    fn text_roundtrip() {
        let m = Module::parse(SAMPLE).unwrap();
        let text = m.to_text();
        let again = Module::parse(&text).unwrap();
        assert_eq!(m, again);
        assert_eq!(again.to_text(), text, "canonical form is a fixpoint");
    }

    #[test]
    fn single_line_calls_canonicalize_to_one_argument_per_line() {
        let m = Module::parse("extern x = VirtualIn(iname: \"VI1\")\nb = And(I1: x.Q, I2: x.Qm)\n")
            .unwrap();
        let text = m.to_text();
        assert!(
            text.contains("b = And(\n\tI1: x.Q,\n\tI2: x.Qm,\n)"),
            "{text}"
        );
        let again = Module::parse(&text).unwrap();
        assert_eq!(m, again);
        assert_eq!(again.to_text(), text);
        // Argument-free calls stay on one line.
        let m = Module::parse("b = And()\nc = Or(\"Oder\")\n").unwrap();
        let text = m.to_text();
        assert!(text.contains("b = And()\n"), "{text}");
        assert!(text.contains("c = Or(\"Oder\")\n"), "{text}");
    }

    #[test]
    fn comments_are_preserved() {
        let src = "\
# header
extern sonne = VirtualIn(iname: \"VI3\") # the sun
t = GreaterEqual( # threshold block
\t# arg note
\tInput1: sonne.Q, # main wire
\tInput2: 28, # degrees
) # done

sonne.Qm = 30 # override
";
        let m = Module::parse(src).unwrap();
        let text = m.to_text();
        for needle in [
            "# header",
            "\"VI3\") # the sun",
            "GreaterEqual( # threshold block",
            "\t# arg note",
            "sonne.Q, # main wire",
            "28, # degrees",
            ") # done",
            "= 30 # override",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in:\n{text}");
        }
        // Canonical form is a fixpoint.
        let again = Module::parse(&text).unwrap();
        assert_eq!(m, again);
        assert_eq!(again.to_text(), text);
    }

    #[test]
    fn string_values_never_emit_as_unparsable_bare_tokens() {
        // Regression: values like "5+" or "1-2" read as bare-number-ish
        // character runs; a content-sniffing emitter once printed them bare,
        // breaking `parse(to_text(m)) == m`.
        for tricky in ["5+", "1-2", "--1", "1.2.3", "+", "."] {
            let src = format!("extern s = VirtualIn(iname: \"VI1\")\ns.Q = \"{tricky}\"\n");
            let m = Module::parse(&src).unwrap();
            let text = m.to_text();
            let again = Module::parse(&text).unwrap_or_else(|e| {
                panic!("canonical form for {tricky:?} does not re-parse: {e}\n{text}")
            });
            assert_eq!(m, again, "value {tricky:?} must survive the round trip");
        }
        // A quoted number canonicalizes to the bare spelling — one canonical
        // form per value, in port assignments and argument lists alike.
        let m = Module::parse("extern s = VirtualIn(iname: \"VI1\")\ns.Q = \"28\"\n").unwrap();
        assert!(m.to_text().contains("s.Q = 28"), "{}", m.to_text());
        let m = Module::parse("b = GreaterEqual(Input2: \"28\")\n").unwrap();
        assert!(m.to_text().contains("Input2: 28,"), "{}", m.to_text());
    }

    #[test]
    fn malformed_numbers_are_rejected() {
        for bad in ["1.2.3", "5.", "-5."] {
            let src = format!("b = GreaterEqual(Input2: {bad})\n");
            let e = Module::parse(&src).unwrap_err();
            assert!(
                e.to_string().contains("invalid number literal"),
                "{bad}: {e}"
            );
        }
    }

    #[test]
    fn let_statements_parse_and_resolve() {
        let m = Module::parse(
            "let schwelle = 28\nlet gruss = \"hallo\"\nb = GreaterEqual(Input2: schwelle)\n",
        )
        .unwrap();
        assert_eq!(m.lets().count(), 2);
        let (_, v) = m.blocks().next().unwrap().params().next().unwrap();
        assert_eq!(m.resolve_value(v).unwrap(), "28");

        // Constants cannot reference constants.
        let e = Module::parse("let a = 1\nlet b = a\n").unwrap_err();
        assert!(e.to_string().contains("number or a quoted string"), "{e}");

        // Undeclared references are errors, with a suggestion.
        let e =
            Module::parse("let schwelle = 28\nb = GreaterEqual(Input2: schwele)\n").unwrap_err();
        assert!(
            e.to_string().contains("undeclared constant `schwele`"),
            "{e}"
        );
        assert!(e.to_string().contains("did you mean `schwelle`?"), "{e}");

        // Constants share the slug namespace and cannot be wired.
        let e = Module::parse("let x = 1\nx = And()\n").unwrap_err();
        assert!(e.to_string().contains("duplicate name `x`"), "{e}");
        let e = Module::parse("let x = 1\nb = And(I1: x.Q)\n").unwrap_err();
        assert!(e.to_string().contains("not a block or extern"), "{e}");
    }

    #[test]
    fn removed_and_moved_parse_and_are_checked() {
        let m = Module::parse("removed old_block # gone\nmoved a -> b\n").unwrap();
        assert_eq!(m.removed().count(), 1);
        assert_eq!(m.moved().count(), 1);
        let text = m.to_text();
        assert!(text.contains("removed old_block # gone"), "{text}");
        assert!(text.contains("moved a -> b"), "{text}");
        assert_eq!(Module::parse(&text).unwrap(), m);

        let e = Module::parse("a = And()\nremoved a\n").unwrap_err();
        assert!(e.to_string().contains("contradicts the declaration"), "{e}");
        let e = Module::parse("removed a\nremoved a\n").unwrap_err();
        assert!(e.to_string().contains("duplicate `removed a`"), "{e}");

        let e = Module::parse("a = And()\nmoved a -> b\n").unwrap_err();
        assert!(e.to_string().contains("must no longer be declared"), "{e}");
        let e = Module::parse("moved a -> a\n").unwrap_err();
        assert!(e.to_string().contains("to itself"), "{e}");
        let e = Module::parse("moved a -> b\nmoved b -> c\n").unwrap_err();
        assert!(e.to_string().contains("chained `moved`"), "{e}");
        let e = Module::parse("moved a -> b\nremoved a\n").unwrap_err();
        assert!(e.to_string().contains("conflicts with a `removed`"), "{e}");
        let e = Module::parse("extern e = VirtualIn(iname: \"VI1\")\nmoved a -> e\n").unwrap_err();
        assert!(e.to_string().contains("not a managed block"), "{e}");

        let e = Module::parse("removed Bad\n").unwrap_err();
        assert!(e.to_string().contains("invalid slug"), "{e}");
        let e = Module::parse("moved a b\n").unwrap_err();
        assert!(
            e.to_string().contains("moved <old_slug> -> <new_slug>"),
            "{e}"
        );
    }

    #[test]
    fn port_statements_on_managed_blocks_are_rejected() {
        // `=` on a managed port: parameters belong in the argument list.
        let e = Module::parse("b = And()\nb.I1 = 3\n").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("targets managed block `b`"), "{msg}");
        assert!(msg.contains("argument list"), "{msg}");
        // `<-` on a managed port: wires belong in the argument list.
        let e = Module::parse("a = And()\nb = And()\nb.I1 <- a.Q\n").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("targets managed block `b`"), "{msg}");
        assert!(msg.contains("I1: a.Q"), "{msg}");
    }

    #[test]
    fn duplicate_arguments_are_rejected() {
        let e = Module::parse("b = GreaterEqual(Input2: 1, Input2: 2)\n").unwrap_err();
        assert!(
            e.to_string().contains("duplicate parameter `Input2`"),
            "{e}"
        );
        let e = Module::parse("extern x = VirtualIn(iname: \"VI1\")\nb = And(I1: x.Q, I1: x.Q)\n")
            .unwrap_err();
        assert!(e.to_string().contains("duplicate wire `I1: x.Q`"), "{e}");
        // Two different sources into one port (fan-in) are representable.
        Module::parse("extern x = VirtualIn(iname: \"VI1\")\nb = And(I1: x.Q, I1: x.Qm)\n")
            .unwrap();
    }

    #[test]
    fn label_must_come_first() {
        let e = Module::parse("extern x = VirtualIn(iname: \"VI1\")\nb = And(I1: x.Q, \"L\")\n")
            .unwrap_err();
        assert!(e.to_string().contains("first argument"), "{e}");
        let e = Module::parse("b = And(\"L\", \"M\")\n").unwrap_err();
        assert!(e.to_string().contains("one label"), "{e}");
    }

    #[test]
    fn v0_keywords_carry_migration_guidance() {
        let e = Module::parse("block b: And\n").unwrap_err();
        assert!(e.to_string().contains("v0 syntax"), "{e}");
        assert!(e.to_string().contains("= <Type>("), "{e}");
        let e = Module::parse("wire a.Q -> b.I1\n").unwrap_err();
        assert!(e.to_string().contains("argument list"), "{e}");
        assert!(e.to_string().contains("<-"), "{e}");
        let e = Module::parse("set a.B = 3\n").unwrap_err();
        assert!(e.to_string().contains("<extern>.<Port> = <value>"), "{e}");
        let e = Module::parse("extern s: VirtualIn match iname \"VI1\"\n").unwrap_err();
        assert!(e.to_string().contains("uuid|iname|title"), "{e}");
    }

    #[test]
    fn wrong_arrow_and_value_forms_are_guided() {
        let e = Module::parse("extern j = AutoJalousie(title: \"J\")\nj.Pos = j.Q\n").unwrap_err();
        assert!(e.to_string().contains("use `<-` to wire"), "{e}");
        let e = Module::parse("extern j = AutoJalousie(title: \"J\")\nj.Pos <- 5\n").unwrap_err();
        assert!(
            e.to_string().contains("use `j.Pos = <value>`") || e.to_string().contains("= <value>"),
            "{e}"
        );
        let e = Module::parse("x = 28\n").unwrap_err();
        assert!(e.to_string().contains("let x = 28"), "{e}");
    }

    #[test]
    fn errors_carry_line_numbers() {
        let e = Module::parse("t.Q <- \n").unwrap_err();
        assert!(e.to_string().contains("line 1"), "{e}");
        let e = Module::parse("Bad = And()\n").unwrap_err();
        assert!(e.to_string().contains("invalid slug"), "{e}");
        let e = Module::parse("ok = And(\nI1: x.Q,\n").unwrap_err();
        assert!(e.to_string().contains("unclosed"), "{e}");
    }

    #[test]
    fn undeclared_reference_is_rejected() {
        let e = Module::parse("a.Q <- b.I\n").unwrap_err();
        assert!(e.to_string().contains("undeclared slug"), "{e}");
    }
}
