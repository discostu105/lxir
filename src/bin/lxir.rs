//! `lxir` — command-line interface over the library, so the IR pipeline is
//! usable without writing Rust: by humans, scripts, and AI agents alike.
//!
//! Every subcommand is a thin wrapper over one public library entry point;
//! nothing here has semantics of its own.

use lxir::ir::{
    CompileOptions, DecompileOptions, DecompileScope, Module, adopt, compile, decompile,
    decompile_pages,
};
use lxir::uuid::parse_serial;
use lxir::{Lockfile, LoxoneDoc};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
lxir — Loxone config-as-code toolchain

USAGE:
  lxir check [--json] <module.lxir>
        Parse and validate an IR module: syntax, references, and managed
        block types/ports/directions against the builtin table (no base
        config needed; parse errors carry line numbers). --json prints a
        machine-readable result on stdout and still exits 1 on errors.

  lxir fmt [--write | --check] <module.lxir>
        Print the canonical form. --write rewrites the file in place;
        --check exits 1 if the file is not already canonical.

  lxir compile --base <cfg.Loxone> --module <m.lxir> --lock <lock.json> --out <out.Loxone>
              [--serial <12-hex>] [--time <unix-seconds>] [--page <title>]
              [--allow-removals]
        Compile IR against a base config, updating the lockfile.
        --serial defaults to the lockfile's recorded Miniserver serial;
        --time defaults to now (only affects newly minted UUIDs — the
        lockfile pins everything minted before);
        --page defaults to the document's first page.

  lxir decompile [--managed-only] [--out-dir <dir>] <cfg.Loxone>
        Print the IR view of a config, grouped into `# page:` sections
        (report on stderr). The default full view shows every page block
        and wire — it is for reading, not compiling. --managed-only
        restricts it to managed-type blocks and what they touch (the
        adoption subset). --out-dir writes one module per logic page
        instead of printing.

  lxir adopt <cfg.Loxone> --out-module <m.lxir> --out-lock <lock.json>
        Move every managed-type block in the config under source control:
        writes the managed-only module plus a lockfile pinning each block's
        existing identity (object/port UUIDs, layout, page), so compiling
        the pair rebuilds the blocks in place instead of minting
        duplicates. Blocks the rebuild could not reproduce faithfully are
        skipped with a warning and stay unmanaged. Never modifies the
        config; refuses existing outputs.

  lxir diff [--exit-code] <old.Loxone> <new.Loxone>
        Semantic diff. --exit-code exits 1 when the docs differ.

  lxir observe <cfg.Loxone>... [--crosscheck <legacy.json>]...
        Port-direction evidence per block type, as JSON. Multiple configs
        merge into one corpus-level view. --crosscheck compares the
        evidence against a legacy connector database (connector-map.json
        shape: type -> {c: [keys], t: {key: \"I\"|\"O\"}}) and reports
        agreements, conflicts, and coverage gaps per type.

  lxir roundtrip <cfg.Loxone>
        Verify the file re-serializes byte-identically (exit 1 if not).
";

type AnyError = Box<dyn std::error::Error>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    match run(&argrefs) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[&str]) -> Result<ExitCode, AnyError> {
    match args {
        [] | ["help"] | ["--help"] | ["-h"] => {
            print!("{USAGE}");
            Ok(ExitCode::from(if args.is_empty() { 2 } else { 0 }))
        }
        ["check", rest @ ..] => cmd_check(rest),
        ["fmt", rest @ ..] => cmd_fmt(rest),
        ["compile", rest @ ..] => cmd_compile(rest),
        ["decompile", rest @ ..] => cmd_decompile(rest),
        ["adopt", rest @ ..] => cmd_adopt(rest),
        ["diff", rest @ ..] => cmd_diff(rest),
        ["observe", rest @ ..] => cmd_observe(rest),
        ["roundtrip", path] => cmd_roundtrip(path),
        [cmd, ..] => {
            eprintln!("unknown or malformed command `{cmd}` — run `lxir help`");
            Ok(ExitCode::from(2))
        }
    }
}

fn read_module(path: &str) -> Result<Module, AnyError> {
    let src = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    Ok(Module::parse(&src).map_err(|e| format!("{path}: {e}"))?)
}

fn read_doc(path: &str) -> Result<LoxoneDoc, AnyError> {
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    Ok(LoxoneDoc::parse(&bytes).map_err(|e| format!("{path}: {e}"))?)
}

fn cmd_check(args: &[&str]) -> Result<ExitCode, AnyError> {
    let (json, path) = match args {
        [path] => (false, *path),
        ["--json", path] | [path, "--json"] => (true, *path),
        _ => return Err("usage: lxir check [--json] <module.lxir>".into()),
    };
    let src = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    // Full static validation: parse (syntax + references), then types,
    // ports, and wire directions against the builtin table.
    let checked = Module::parse(&src).and_then(|m| lxir::ir::validate_ports(&m).map(|()| m));
    match checked {
        Ok(m) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "ok": true, "path": path, "counts": {
                        "externs": m.externs().count(), "blocks": m.blocks().count(),
                        "wires": m.wire_pairs().len(), "sets": m.sets().count(),
                        "lets": m.lets().count(), "removed": m.removed().count(),
                        "moved": m.moved().count(),
                    }})
                );
            } else {
                println!(
                    "OK: {} externs, {} blocks, {} wires, {} sets, {} lets, \
                     {} removed, {} moved",
                    m.externs().count(),
                    m.blocks().count(),
                    m.wire_pairs().len(),
                    m.sets().count(),
                    m.lets().count(),
                    m.removed().count(),
                    m.moved().count()
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(e) if json => {
            // Structured diagnostics: parse errors carry a 1-based line,
            // semantic errors none. The parser is fail-fast, so there is
            // at most one error per run.
            let line = match &e {
                lxir::Error::IrParse { line, .. } => Some(*line),
                _ => None,
            };
            println!(
                "{}",
                serde_json::json!({ "ok": false, "path": path, "errors": [
                    { "line": line, "message": e.to_string() },
                ]})
            );
            Ok(ExitCode::FAILURE)
        }
        Err(e) => Err(format!("{path}: {e}").into()),
    }
}

fn cmd_fmt(args: &[&str]) -> Result<ExitCode, AnyError> {
    let (mode, path) = match args {
        [path] => ("print", *path),
        ["--write", path] => ("write", *path),
        ["--check", path] => ("check", *path),
        _ => return Err("usage: lxir fmt [--write | --check] <module.lxir>".into()),
    };
    let src = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let canonical = Module::parse(&src)
        .map_err(|e| format!("{path}: {e}"))?
        .to_text();
    match mode {
        "print" => print!("{canonical}"),
        "write" => std::fs::write(path, &canonical)?,
        _check => {
            if src != canonical {
                eprintln!("{path}: not canonical (run `lxir fmt --write {path}`)");
                return Ok(ExitCode::FAILURE);
            }
            println!("{path}: canonical");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_compile(args: &[&str]) -> Result<ExitCode, AnyError> {
    let mut base = None;
    let mut module = None;
    let mut lock_path: Option<PathBuf> = None;
    let mut out = None;
    let mut serial = None;
    let mut time = None;
    let mut page = None;
    let mut allow_removals = false;

    let mut it = args.iter();
    while let Some(&flag) = it.next() {
        let mut value = || -> Result<&str, AnyError> {
            it.next()
                .copied()
                .ok_or_else(|| format!("{flag} needs a value").into())
        };
        match flag {
            "--base" => base = Some(value()?.to_string()),
            "--module" => module = Some(value()?.to_string()),
            "--lock" => lock_path = Some(PathBuf::from(value()?)),
            "--out" => out = Some(value()?.to_string()),
            "--serial" => serial = Some(value()?.to_string()),
            "--time" => time = Some(value()?.parse::<i64>()?),
            "--page" => page = Some(value()?.to_string()),
            "--allow-removals" => allow_removals = true,
            other => return Err(format!("unknown flag `{other}` — run `lxir help`").into()),
        }
    }
    let (Some(base), Some(module), Some(lock_path), Some(out)) = (base, module, lock_path, out)
    else {
        return Err("compile requires --base, --module, --lock, and --out".into());
    };

    let base_doc = read_doc(&base)?;
    let m = read_module(&module)?;
    let mut lock = if lock_path.exists() {
        Lockfile::load(&lock_path)?
    } else {
        Lockfile::new()
    };
    let serial = serial
        .or_else(|| lock.target.miniserver_serial.clone())
        .ok_or("no --serial given and the lockfile has none recorded")?;
    let opts = CompileOptions {
        machine: parse_serial(&serial)?,
        mint_time_unix: time.unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0)
        }),
        page_title: page,
        allow_removals,
    };
    let compiled = compile(&base_doc, &m, &mut lock, &opts)?;
    std::fs::write(&out, compiled.to_bytes())?;
    lock.save(&lock_path)?;
    println!(
        "compiled {module} against {base} -> {out} ({} objects, NextObj {}); lock: {}",
        compiled.objects().len(),
        compiled.counters().next_obj,
        lock_path.display()
    );
    Ok(ExitCode::SUCCESS)
}

fn cmd_decompile(args: &[&str]) -> Result<ExitCode, AnyError> {
    let mut path = None;
    let mut out_dir: Option<PathBuf> = None;
    let mut scope = DecompileScope::Full;
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        match a {
            "--managed-only" => scope = DecompileScope::ManagedOnly,
            "--out-dir" => {
                out_dir = Some(PathBuf::from(
                    it.next().copied().ok_or("--out-dir needs a value")?,
                ));
            }
            flag if flag.starts_with("--") => {
                return Err(format!("unknown flag `{flag}` — run `lxir help`").into());
            }
            p if path.is_none() => path = Some(p),
            _ => {
                return Err("usage: lxir decompile [--managed-only] [--out-dir <dir>] \
                            <cfg.Loxone>"
                    .into());
            }
        }
    }
    let Some(path) = path else {
        return Err("usage: lxir decompile [--managed-only] [--out-dir <dir>] <cfg.Loxone>".into());
    };
    let doc = read_doc(path)?;
    let opts = DecompileOptions {
        scope,
        ..Default::default()
    };

    let report = if let Some(dir) = out_dir {
        let (pages, report) = decompile_pages(&doc, &opts)?;
        std::fs::create_dir_all(&dir)?;
        for p in &pages {
            let file = dir.join(format!("{}.lxir", p.slug));
            std::fs::write(&file, p.module.to_text())?;
            println!(
                "wrote {} ({} externs, {} blocks, {} wires)",
                file.display(),
                p.module.externs().count(),
                p.module.blocks().count(),
                p.module.wire_pairs().len()
            );
        }
        report
    } else {
        let (module, report) = decompile(&doc, &opts)?;
        print!("{}", module.to_text());
        report
    };
    eprintln!(
        "# {path}: {} managed, {} externs across {} pages, {} raw objects untouched",
        report.managed, report.externs, report.pages, report.raw_objects
    );
    Ok(ExitCode::SUCCESS)
}

fn cmd_adopt(args: &[&str]) -> Result<ExitCode, AnyError> {
    const USAGE: &str =
        "usage: lxir adopt <cfg.Loxone> --out-module <m.lxir> --out-lock <lock.json>";
    let mut path = None;
    let mut out_module: Option<PathBuf> = None;
    let mut out_lock: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        let mut value = || -> Result<&str, AnyError> {
            it.next()
                .copied()
                .ok_or_else(|| format!("{a} needs a value").into())
        };
        match a {
            "--out-module" => out_module = Some(PathBuf::from(value()?)),
            "--out-lock" => out_lock = Some(PathBuf::from(value()?)),
            flag if flag.starts_with("--") => {
                return Err(format!("unknown flag `{flag}` — run `lxir help`").into());
            }
            p if path.is_none() => path = Some(p),
            _ => return Err(USAGE.into()),
        }
    }
    let (Some(path), Some(out_module), Some(out_lock)) = (path, out_module, out_lock) else {
        return Err(USAGE.into());
    };
    // Adoption is a one-time claim of identity; overwriting an existing
    // module or lock would silently discard identities already pinned.
    for existing in [&out_module, &out_lock] {
        if existing.exists() {
            return Err(format!(
                "{} already exists — adopt refuses to overwrite (move it away first)",
                existing.display()
            )
            .into());
        }
    }

    let doc = read_doc(path)?;
    let (module, lock, report) = adopt(&doc)?;
    std::fs::write(&out_module, module.to_text())?;
    lock.save(&out_lock)?;
    for r in &report.refused {
        eprintln!("warning: {r}");
    }
    println!(
        "adopted {} blocks ({} externs pinned) across {} pages from {path}{}",
        report.blocks,
        report.externs,
        report.pages,
        if report.refused.is_empty() {
            String::new()
        } else {
            format!(" ({} refused, see warnings)", report.refused.len())
        }
    );
    println!(
        "wrote {} and {}\nnext: lxir compile --base {path} --module {} --lock {} \
         --serial <miniserver-serial> --out <out.Loxone>, then `lxir diff {path} \
         <out.Loxone>` — it should be empty",
        out_module.display(),
        out_lock.display(),
        out_module.display(),
        out_lock.display()
    );
    Ok(ExitCode::SUCCESS)
}

fn cmd_diff(args: &[&str]) -> Result<ExitCode, AnyError> {
    let (exit_code, old, new) = match args {
        [old, new] => (false, *old, *new),
        ["--exit-code", old, new] => (true, *old, *new),
        _ => return Err("usage: lxir diff [--exit-code] <old.Loxone> <new.Loxone>".into()),
    };
    let a = read_doc(old)?;
    let b = read_doc(new)?;
    let d = lxir::diff::diff(&a, &b);

    for o in &d.added {
        println!("+ {} {} {:?}", o.block_type, o.uuid, o.title);
    }
    for o in &d.removed {
        println!("- {} {} {:?}", o.block_type, o.uuid, o.title);
    }
    for r in &d.renamed {
        let tag = if r.locale_suspect { " [locale?]" } else { "" };
        println!(
            "~ {} {} {:?} -> {:?}{tag}",
            r.block_type, r.uuid, r.from, r.to
        );
    }
    for p in &d.param_changes {
        println!(
            "* {} {}.{}: {:?} -> {:?}",
            p.block_type, p.object_uuid, p.port_key, p.from, p.to
        );
    }
    for w in &d.wires_added {
        println!("+wire {} -> {}", w.from_port, w.to_port);
    }
    for w in &d.wires_removed {
        println!("-wire {} -> {}", w.from_port, w.to_port);
    }
    let locale_noise = d.renamed.iter().filter(|r| r.locale_suspect).count();
    eprintln!(
        "{} added, {} removed, {} renamed ({locale_noise} locale-suspect), \
         {} param changes, {}/{} wires added/removed",
        d.added.len(),
        d.removed.len(),
        d.renamed.len(),
        d.param_changes.len(),
        d.wires_added.len(),
        d.wires_removed.len()
    );
    if exit_code && !d.is_empty() {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_observe(args: &[&str]) -> Result<ExitCode, AnyError> {
    use lxir::connectors::{self, LegacyDb, Observations};

    let mut paths: Vec<&str> = Vec::new();
    let mut legacy_paths: Vec<&str> = Vec::new();
    let mut it = args.iter();
    while let Some(&a) = it.next() {
        if a == "--crosscheck" {
            legacy_paths.push(it.next().copied().ok_or("--crosscheck needs a value")?);
        } else {
            paths.push(a);
        }
    }
    if paths.is_empty() {
        return Err("usage: lxir observe <cfg.Loxone>... [--crosscheck <legacy.json>]...".into());
    }

    let mut obs = Observations::new();
    for p in &paths {
        connectors::merge(&mut obs, connectors::observe(&read_doc(p)?));
    }

    if legacy_paths.is_empty() {
        println!("{}", serde_json::to_string_pretty(&obs)?);
        return Ok(ExitCode::SUCCESS);
    }

    let mut checks = serde_json::Map::new();
    for lp in legacy_paths {
        let bytes = std::fs::read(lp).map_err(|e| format!("{lp}: {e}"))?;
        let legacy: LegacyDb = serde_json::from_slice(&bytes).map_err(|e| format!("{lp}: {e}"))?;
        let only_corpus: Vec<&String> = obs.keys().filter(|t| !legacy.contains_key(*t)).collect();
        let only_legacy: Vec<&String> = legacy.keys().filter(|t| !obs.contains_key(*t)).collect();
        checks.insert(
            lp.to_string(),
            serde_json::json!({
                "types": connectors::crosscheck(&obs, &legacy),
                "types_only_in_corpus": only_corpus,
                "types_only_in_legacy": only_legacy,
            }),
        );
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "observations": obs,
            "crosscheck": checks,
        }))?
    );
    Ok(ExitCode::SUCCESS)
}

fn cmd_roundtrip(path: &str) -> Result<ExitCode, AnyError> {
    let input = std::fs::read(Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    let doc = LoxoneDoc::parse(&input).map_err(|e| format!("{path}: {e}"))?;
    let output = doc.to_bytes();
    if input == output {
        println!("{path}: byte-identical roundtrip ({} bytes)", input.len());
        Ok(ExitCode::SUCCESS)
    } else {
        let pos = input
            .iter()
            .zip(output.iter())
            .position(|(a, b)| a != b)
            .unwrap_or(input.len().min(output.len()));
        eprintln!(
            "{path}: roundtrip DIVERGES at byte {pos} (in {} bytes, out {} bytes)",
            input.len(),
            output.len()
        );
        Ok(ExitCode::FAILURE)
    }
}
