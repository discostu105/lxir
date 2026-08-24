//! Hand-rolled parser for the IR text format. Line-oriented; only block
//! parameter bodies (`{ … }`) span lines.

use super::ast::*;
use crate::error::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Ident(String),
    Str(String),
    Num(String),
    Colon,
    Dot,
    Arrow,
    Eq,
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
            '=' => {
                chars.next();
                toks.push(Tok::Eq);
            }
            '{' => {
                chars.next();
                toks.push(Tok::LBrace);
            }
            '}' => {
                chars.next();
                toks.push(Tok::RBrace);
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
    // A block whose `{ … }` body is still open.
    let mut open_block: Option<BlockDecl> = None;

    for (i, raw) in src.lines().enumerate() {
        let lineno = i + 1;
        let err = |msg: String| Error::IrParse { line: lineno, msg };
        let (toks, comment) = lex(raw, lineno)?;
        if toks.is_empty() {
            // Blank line, or a whole-line comment — kept verbatim as an
            // item (or body item) so formatting is non-destructive.
            if let Some(text) = comment {
                match open_block.as_mut() {
                    Some(block) => block.body.push(BodyItem::Comment(text)),
                    None => items.push(Item::Comment(text)),
                }
            }
            continue;
        }

        if let Some(block) = open_block.as_mut() {
            // Inside a `{ … }` body: `Param = value` or `}`.
            match toks.as_slice() {
                [Tok::RBrace] => {
                    block.close_comment = comment;
                    items.push(Item::Block(open_block.take().unwrap()));
                }
                [Tok::Ident(key), Tok::Eq, val] => {
                    block.body.push(BodyItem::Param(ParamDecl {
                        key: key.clone(),
                        value: value_of(val, lineno)?,
                        comment,
                    }));
                }
                _ => {
                    return Err(err(
                        "expected `Param = value` or `}` inside block body".into()
                    ));
                }
            }
            continue;
        }

        let Tok::Ident(kw) = &toks[0] else {
            return Err(err("expected a statement keyword".into()));
        };
        match kw.as_str() {
            "extern" => match toks.as_slice() {
                [
                    _,
                    Tok::Ident(slug),
                    Tok::Colon,
                    Tok::Ident(ty),
                    Tok::Ident(m),
                    Tok::Ident(kind),
                    Tok::Str(value),
                ] if m == "match" => {
                    let match_spec = match kind.as_str() {
                        "uuid" => MatchSpec::Uuid(value.clone()),
                        "iname" => MatchSpec::IName(value.clone()),
                        "title" => MatchSpec::Title(value.clone()),
                        other => {
                            return Err(err(format!(
                                "unknown match kind `{other}` (expected uuid, iname, or title)"
                            )));
                        }
                    };
                    items.push(Item::Extern(ExternDecl {
                        slug: check_slug(slug, lineno)?,
                        block_type: check_type(ty, lineno)?,
                        match_spec,
                        comment,
                    }));
                }
                _ => {
                    return Err(err(
                        "expected `extern <slug>: <Type> match (uuid|iname|title) \"…\"`".into(),
                    ));
                }
            },
            "block" => {
                let mut rest = toks[1..].to_vec();
                let ends_open = rest.last() == Some(&Tok::LBrace);
                if ends_open {
                    rest.pop();
                }
                let decl = match rest.as_slice() {
                    [Tok::Ident(slug), Tok::Colon, Tok::Ident(ty)] => BlockDecl {
                        slug: check_slug(slug, lineno)?,
                        block_type: check_type(ty, lineno)?,
                        title: None,
                        body: Vec::new(),
                        comment,
                        close_comment: None,
                    },
                    [
                        Tok::Ident(slug),
                        Tok::Colon,
                        Tok::Ident(ty),
                        Tok::Str(title),
                    ] => BlockDecl {
                        slug: check_slug(slug, lineno)?,
                        block_type: check_type(ty, lineno)?,
                        title: Some(title.clone()),
                        body: Vec::new(),
                        comment,
                        close_comment: None,
                    },
                    _ => {
                        return Err(err("expected `block <slug>: <Type> [\"Title\"] [{]`".into()));
                    }
                };
                if ends_open {
                    open_block = Some(decl);
                } else {
                    items.push(Item::Block(decl));
                }
            }
            "wire" => match toks.as_slice() {
                [
                    _,
                    Tok::Ident(fs),
                    Tok::Dot,
                    Tok::Ident(fp),
                    Tok::Arrow,
                    Tok::Ident(ts),
                    Tok::Dot,
                    Tok::Ident(tp),
                ] => {
                    items.push(Item::Wire(WireDecl {
                        from: PortRef {
                            slug: fs.clone(),
                            port: fp.clone(),
                        },
                        to: PortRef {
                            slug: ts.clone(),
                            port: tp.clone(),
                        },
                        comment,
                    }));
                }
                _ => {
                    return Err(err("expected `wire <slug>.<Port> -> <slug>.<Port>`".into()));
                }
            },
            "set" => match toks.as_slice() {
                [
                    _,
                    Tok::Ident(slug),
                    Tok::Dot,
                    Tok::Ident(port),
                    Tok::Eq,
                    val,
                ] => {
                    items.push(Item::Set(SetDecl {
                        target: PortRef {
                            slug: slug.clone(),
                            port: port.clone(),
                        },
                        value: value_of(val, lineno)?,
                        comment,
                    }));
                }
                _ => return Err(err("expected `set <slug>.<Port> = <value>`".into())),
            },
            "let" => match toks.as_slice() {
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
            "removed" => match toks.as_slice() {
                [_, Tok::Ident(slug)] => {
                    items.push(Item::Removed(RemovedDecl {
                        slug: check_slug(slug, lineno)?,
                        comment,
                    }));
                }
                _ => return Err(err("expected `removed <slug>`".into())),
            },
            "moved" => match toks.as_slice() {
                [_, Tok::Ident(from), Tok::Arrow, Tok::Ident(to)] => {
                    items.push(Item::Moved(MovedDecl {
                        from: check_slug(from, lineno)?,
                        to: check_slug(to, lineno)?,
                        comment,
                    }));
                }
                _ => return Err(err("expected `moved <old_slug> -> <new_slug>`".into())),
            },
            other => {
                return Err(err(format!(
                    "unknown statement `{other}` (expected extern, block, wire, set, \
                     let, removed, or moved)"
                )));
            }
        }
    }

    if let Some(block) = open_block {
        return Err(Error::IrParse {
            line: src.lines().count(),
            msg: format!("unclosed `{{` in block `{}`", block.slug),
        });
    }
    Ok(Module { items })
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
extern sonne: VirtualIn match iname "VI3"
extern jal:   AutoJalousie match title "Beschattung Süd"

let schwelle = 28

block temp_hoch: GreaterEqual "Temp über 28" {
    Input2 = schwelle
}
block beschatten: And

wire sonne.Q -> beschatten.I2
wire temp_hoch.Q -> beschatten.I1
set jal.TargetPos = 70
"#;

    #[test]
    fn parses_sample() {
        let m = Module::parse(SAMPLE).unwrap();
        assert_eq!(m.externs().count(), 2);
        assert_eq!(m.blocks().count(), 2);
        assert_eq!(m.wires().count(), 2);
        assert_eq!(m.sets().count(), 1);
        assert_eq!(m.lets().count(), 1);
        let block = m.blocks().next().unwrap();
        assert_eq!(block.title.as_deref(), Some("Temp über 28"));
        let params: Vec<_> = block.params().collect();
        assert_eq!(params, vec![("Input2", &Value::Ref("schwelle".into()))]);
        assert_eq!(m.resolve_value(params[0].1).unwrap(), "28");
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
    fn comments_are_preserved() {
        let src = "\
# header
extern sonne: VirtualIn match iname \"VI3\" # the sun
block t: GreaterEqual { # threshold block
\t# body note
\tInput2 = 28 # degrees
} # done

wire sonne.Q -> t.Input1 # main wire
set sonne.Qm = 30 # override
";
        let m = Module::parse(src).unwrap();
        let text = m.to_text();
        for needle in [
            "# header",
            "\"VI3\" # the sun",
            "{ # threshold block",
            "\t# body note",
            "28 # degrees",
            "} # done",
            "-> t.Input1 # main wire",
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
            let src = format!("extern s: VirtualIn match iname \"VI1\"\nset s.Q = \"{tricky}\"\n");
            let m = Module::parse(&src).unwrap();
            let text = m.to_text();
            let again = Module::parse(&text).unwrap_or_else(|e| {
                panic!("canonical form for {tricky:?} does not re-parse: {e}\n{text}")
            });
            assert_eq!(m, again, "value {tricky:?} must survive the round trip");
        }
        // A quoted number canonicalizes to the bare spelling — one canonical
        // form per value.
        let m = Module::parse(
            "block b: And\nextern s: VirtualIn match iname \"VI1\"\nset s.Q = \"28\"\n",
        )
        .unwrap();
        assert!(m.to_text().contains("set s.Q = 28"), "{}", m.to_text());
    }

    #[test]
    fn malformed_numbers_are_rejected() {
        for bad in ["Input2 = 1.2.3", "Input2 = 5.", "Input2 = -5."] {
            let src = format!("block b: GreaterEqual {{\n{bad}\n}}\n");
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
            "let schwelle = 28\nlet gruss = \"hallo\"\nblock b: GreaterEqual {\n\tInput2 = schwelle\n}\n",
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
            Module::parse("let schwelle = 28\nblock b: GreaterEqual {\n\tInput2 = schwele\n}\n")
                .unwrap_err();
        assert!(
            e.to_string().contains("undeclared constant `schwele`"),
            "{e}"
        );
        assert!(e.to_string().contains("did you mean `schwelle`?"), "{e}");

        // Constants share the slug namespace and cannot be wired.
        let e = Module::parse("let x = 1\nblock x: And\n").unwrap_err();
        assert!(e.to_string().contains("duplicate name `x`"), "{e}");
        let e = Module::parse("let x = 1\nblock b: And\nwire x.Q -> b.I1\n").unwrap_err();
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

        let e = Module::parse("block a: And\nremoved a\n").unwrap_err();
        assert!(e.to_string().contains("contradicts the declaration"), "{e}");
        let e = Module::parse("removed a\nremoved a\n").unwrap_err();
        assert!(e.to_string().contains("duplicate `removed a`"), "{e}");

        let e = Module::parse("block a: And\nmoved a -> b\n").unwrap_err();
        assert!(e.to_string().contains("must no longer be declared"), "{e}");
        let e = Module::parse("moved a -> a\n").unwrap_err();
        assert!(e.to_string().contains("to itself"), "{e}");
        let e = Module::parse("moved a -> b\nmoved b -> c\n").unwrap_err();
        assert!(e.to_string().contains("chained `moved`"), "{e}");
        let e = Module::parse("moved a -> b\nremoved a\n").unwrap_err();
        assert!(e.to_string().contains("conflicts with a `removed`"), "{e}");
        let e =
            Module::parse("extern e: VirtualIn match iname \"VI1\"\nmoved a -> e\n").unwrap_err();
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
    fn set_on_managed_block_is_rejected() {
        let e = Module::parse("block b: And\nset b.I1 = 3\n").unwrap_err();
        let msg = e.to_string();
        assert!(msg.contains("targets managed block `b`"), "{msg}");
        assert!(msg.contains("block body"), "{msg}");
    }

    #[test]
    fn errors_carry_line_numbers() {
        let e = Module::parse("wire a.b -> \n").unwrap_err();
        assert!(e.to_string().contains("line 1"), "{e}");
        let e = Module::parse("block Bad: And\n").unwrap_err();
        assert!(e.to_string().contains("invalid slug"), "{e}");
        let e = Module::parse("block ok: And {\nInput2 = 3\n").unwrap_err();
        assert!(e.to_string().contains("unclosed"), "{e}");
    }

    #[test]
    fn undeclared_reference_is_rejected() {
        let e = Module::parse("wire a.Q -> b.I\n").unwrap_err();
        assert!(e.to_string().contains("undeclared slug"), "{e}");
    }
}
