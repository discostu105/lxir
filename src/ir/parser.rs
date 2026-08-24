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

fn lex(line: &str, lineno: usize) -> Result<Vec<Tok>> {
    let err = |msg: String| Error::IrParse { line: lineno, msg };
    let mut toks = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        match c {
            '#' => break, // comment to end of line
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
                    toks.push(Tok::Num(s));
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
                toks.push(Tok::Num(s));
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
    Ok(toks)
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
        // Whole-line comments are kept as items (so formatting preserves
        // them) — except inside block bodies, where they are dropped.
        if open_block.is_none()
            && let Some(text) = raw.trim_start().strip_prefix('#')
        {
            items.push(Item::Comment(text.to_string()));
            continue;
        }
        let toks = lex(raw, lineno)?;
        if toks.is_empty() {
            continue;
        }

        if let Some(block) = open_block.as_mut() {
            // Inside a `{ … }` body: `Param = value` or `}`.
            match toks.as_slice() {
                [Tok::RBrace] => {
                    items.push(Item::Block(open_block.take().unwrap()));
                }
                [Tok::Ident(key), Tok::Eq, val] => {
                    block.params.push((key.clone(), value_of(val, lineno)?));
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
                        params: Vec::new(),
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
                        params: Vec::new(),
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
                    }));
                }
                _ => return Err(err("expected `set <slug>.<Port> = <value>`".into())),
            },
            other => {
                return Err(err(format!(
                    "unknown statement `{other}` (expected extern, block, wire, or set)"
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

fn value_of(tok: &Tok, lineno: usize) -> Result<String> {
    match tok {
        Tok::Num(n) => Ok(n.clone()),
        Tok::Str(s) => Ok(s.clone()),
        Tok::Ident(s) => Ok(s.clone()),
        _ => Err(Error::IrParse {
            line: lineno,
            msg: "expected a number or quoted string".into(),
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

block temp_hoch: GreaterEqual "Temp über 28" {
    Input2 = 28
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
        let block = m.blocks().next().unwrap();
        assert_eq!(block.title.as_deref(), Some("Temp über 28"));
        assert_eq!(block.params, vec![("Input2".to_string(), "28".to_string())]);
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
