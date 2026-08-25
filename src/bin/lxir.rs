//! `lxir` — command-line interface over the library, so the IR pipeline is
//! usable without writing Rust: by humans, scripts, and AI agents alike.
//!
//! Every subcommand is a thin wrapper over one public library entry point;
//! nothing here has semantics of its own.

use lxir::ir::{
    CompileOptions, DecompileOptions, DecompileScope, Module, adopt, adopt_pages, compile,
    decompile, decompile_pages,
};
use lxir::uuid::parse_serial;
use lxir::{Lockfile, LoxoneDoc};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
lxir — Loxone config-as-code toolchain

USAGE:
  lxir check [--json] <module.lxir | module-dir>
        Parse and validate an IR module: syntax, references, and managed
        block types/ports/directions against the builtin table (no base
        config needed; parse errors carry line numbers). --json prints a
        machine-readable result on stdout and still exits 1 on errors.

  lxir fmt [--write | --check] <module.lxir | module-dir>
        Print the canonical form. --write rewrites the file(s) in place;
        --check exits 1 if a file is not already canonical.

  lxir compile --base <cfg.Loxone> --module <m.lxir | module-dir> --lock <lock.json> --out <out.Loxone>
              [--serial <12-hex>] [--time <unix-seconds>] [--page <title>]
              [--allow-removals] [--accept-version <v>]
        Compile IR against a base config, updating the lockfile.
        --serial defaults to the lockfile's recorded Miniserver serial;
        --time defaults to now (only affects newly minted UUIDs — the
        lockfile pins everything minted before);
        --page defaults to the document's first page.
        A module directory stands for a multi-file module: all *.lxir
        files inside, merged in file-name order (one file per page is
        the convention; a fragment may reference sibling-file slugs).
        A base written by a different Loxone release than the lock's
        ConfigVersion pin is refused; after qualifying the release
        (one oracle open+save run), --accept-version <v> re-pins it.

  lxir decompile [--managed-only] [--out-dir <dir>] <cfg.Loxone>
        Print the IR view of a config, grouped into `# page:` sections
        (report on stderr). The default full view shows every page block
        and wire — it is for reading, not compiling. --managed-only
        restricts it to managed-type blocks and what they touch (the
        adoption subset). --out-dir writes one module per logic page
        instead of printing.

  lxir adopt <cfg.Loxone> (--out-module <m.lxir> | --out-dir <dir>) --out-lock <lock.json>
        Move every managed-type block in the config under source control:
        writes the managed-only module plus a lockfile pinning each block's
        existing identity (object/port UUIDs, layout, page), so compiling
        the pair rebuilds the blocks in place instead of minting
        duplicates. --out-dir writes the module as a directory of
        fragments, one file per page (periphery externs in
        _periphery.lxir) — the layout `compile --module <dir>` reads.
        Blocks the rebuild could not reproduce faithfully are skipped
        with a warning and stay unmanaged. Never modifies the config;
        refuses existing outputs.

  lxir diff [--exit-code] <old.Loxone> <new.Loxone>
        Semantic diff. --exit-code exits 1 when the docs differ.

  lxir drift <cfg.Loxone> --lock <lock.json>
        Check a config (typically a fresh download) against the semantic
        fingerprint the lockfile recorded at the last adopt/compile.
        Exit 0 = in sync; 1 = another writer changed something since
        (position moves, save noise, and locale renames don't count).
        One parse, no reference config needed — `lxir diff` tells you
        *what* changed, this tells you *whether* cheaply.

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
        ["drift", rest @ ..] => cmd_drift(rest),
        ["observe", rest @ ..] => cmd_observe(rest),
        ["roundtrip", path] => cmd_roundtrip(path),
        [cmd, ..] => {
            eprintln!("unknown or malformed command `{cmd}` — run `lxir help`");
            Ok(ExitCode::from(2))
        }
    }
}

/// The `*.lxir` files of a module directory, sorted by name (the merge
/// order — semantics don't depend on it, but determinism does).
fn module_dir_files(dir: &std::path::Path) -> Result<Vec<PathBuf>, (String, lxir::Error)> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| (dir.display().to_string(), lxir::Error::Io(e)))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "lxir"))
        .collect();
    files.sort();
    if files.is_empty() {
        return Err((
            dir.display().to_string(),
            lxir::Error::Compile("no .lxir files in directory".into()),
        ));
    }
    Ok(files)
}

/// Load a module from one file or a directory of `*.lxir` fragments.
/// Directory fragments parse individually (errors name the file);
/// name resolution runs once on the merged module.
fn load_module(path: &str) -> Result<Module, (String, lxir::Error)> {
    let p = std::path::Path::new(path);
    if p.is_dir() {
        let mut items = Vec::new();
        for f in module_dir_files(p)? {
            let name = f.display().to_string();
            let src =
                std::fs::read_to_string(&f).map_err(|e| (name.clone(), lxir::Error::Io(e)))?;
            items.extend(
                Module::parse_fragment(&src)
                    .map_err(|e| (name.clone(), e))?
                    .items,
            );
        }
        let module = Module { items };
        module.validate().map_err(|e| (path.to_string(), e))?;
        Ok(module)
    } else {
        let src =
            std::fs::read_to_string(path).map_err(|e| (path.to_string(), lxir::Error::Io(e)))?;
        Module::parse(&src).map_err(|e| (path.to_string(), e))
    }
}

fn read_module(path: &str) -> Result<Module, AnyError> {
    load_module(path).map_err(|(p, e)| format!("{p}: {e}").into())
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
    // Full static validation: parse (syntax + references), then types,
    // ports, and wire directions against the builtin table.
    let checked = load_module(path).and_then(|m| {
        lxir::ir::validate_ports(&m)
            .map(|()| m)
            .map_err(|e| (path.to_string(), e))
    });
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
        Err((epath, e)) if json => {
            // Structured diagnostics: parse errors carry a 1-based line,
            // semantic errors none. The parser is fail-fast, so there is
            // at most one error per run. `path` names the file the error
            // is in — for a module directory that is the fragment file.
            let line = match &e {
                lxir::Error::IrParse { line, .. } => Some(*line),
                _ => None,
            };
            println!(
                "{}",
                serde_json::json!({ "ok": false, "path": epath, "errors": [
                    { "line": line, "message": e.to_string() },
                ]})
            );
            Ok(ExitCode::FAILURE)
        }
        Err((epath, e)) => Err(format!("{epath}: {e}").into()),
    }
}

fn cmd_fmt(args: &[&str]) -> Result<ExitCode, AnyError> {
    let (mode, path) = match args {
        [path] => ("print", *path),
        ["--write", path] => ("write", *path),
        ["--check", path] => ("check", *path),
        _ => return Err("usage: lxir fmt [--write | --check] <module.lxir | module-dir>".into()),
    };
    // Formatting is per file and needs no name resolution (a fragment of
    // a module directory may reference slugs from sibling files), so
    // every target parses as a fragment.
    let targets: Vec<PathBuf> = if std::path::Path::new(path).is_dir() {
        if mode == "print" {
            return Err("fmt on a module directory requires --write or --check".into());
        }
        module_dir_files(std::path::Path::new(path)).map_err(|(p, e)| format!("{p}: {e}"))?
    } else {
        vec![PathBuf::from(path)]
    };
    let mut dirty = false;
    for f in &targets {
        let name = f.display();
        let src = std::fs::read_to_string(f).map_err(|e| format!("{name}: {e}"))?;
        let canonical = Module::parse_fragment(&src)
            .map_err(|e| format!("{name}: {e}"))?
            .to_text();
        match mode {
            "print" => print!("{canonical}"),
            "write" => {
                if src != canonical {
                    std::fs::write(f, &canonical)?;
                }
            }
            _check => {
                if src != canonical {
                    eprintln!("{name}: not canonical (run `lxir fmt --write {name}`)");
                    dirty = true;
                } else {
                    println!("{name}: canonical");
                }
            }
        }
    }
    if dirty {
        return Ok(ExitCode::FAILURE);
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
    let mut accept_version = None;

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
            "--accept-version" => accept_version = Some(value()?.to_string()),
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
        accept_version,
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
    const USAGE: &str = "usage: lxir adopt <cfg.Loxone> \
         (--out-module <m.lxir> | --out-dir <dir>) --out-lock <lock.json>";
    let mut path = None;
    let mut out_module: Option<PathBuf> = None;
    let mut out_dir: Option<PathBuf> = None;
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
            "--out-dir" => out_dir = Some(PathBuf::from(value()?)),
            "--out-lock" => out_lock = Some(PathBuf::from(value()?)),
            flag if flag.starts_with("--") => {
                return Err(format!("unknown flag `{flag}` — run `lxir help`").into());
            }
            p if path.is_none() => path = Some(p),
            _ => return Err(USAGE.into()),
        }
    }
    let (Some(path), Some(out_lock)) = (path, out_lock) else {
        return Err(USAGE.into());
    };
    if out_module.is_some() == out_dir.is_some() {
        return Err(USAGE.into());
    }
    // Adoption is a one-time claim of identity; overwriting an existing
    // module or lock would silently discard identities already pinned.
    for existing in [out_module.as_ref(), out_dir.as_ref(), Some(&out_lock)]
        .into_iter()
        .flatten()
    {
        if existing.exists() {
            return Err(format!(
                "{} already exists — adopt refuses to overwrite (move it away first)",
                existing.display()
            )
            .into());
        }
    }

    let doc = read_doc(path)?;
    let (lock, report) = if let Some(dir) = &out_dir {
        let (fragments, lock, report) = adopt_pages(&doc)?;
        std::fs::create_dir_all(dir)?;
        for (stem, fragment) in &fragments {
            std::fs::write(dir.join(format!("{stem}.lxir")), fragment.to_text())?;
        }
        (lock, report)
    } else {
        let out_module = out_module.as_ref().expect("checked above");
        let (module, lock, report) = adopt(&doc)?;
        std::fs::write(out_module, module.to_text())?;
        (lock, report)
    };
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
    let module_arg = out_module
        .as_ref()
        .or(out_dir.as_ref())
        .expect("one is set");
    println!(
        "wrote {} and {}\nnext: lxir compile --base {path} --module {} --lock {} \
         --serial <miniserver-serial> --out <out.Loxone>, then `lxir diff {path} \
         <out.Loxone>` — it should be empty",
        module_arg.display(),
        out_lock.display(),
        module_arg.display(),
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

fn cmd_drift(args: &[&str]) -> Result<ExitCode, AnyError> {
    let (config, lock_path) = match args {
        [config, "--lock", lock] | ["--lock", lock, config] => (*config, *lock),
        _ => return Err("usage: lxir drift <cfg.Loxone> --lock <lock.json>".into()),
    };
    let doc = read_doc(config)?;
    let lock = Lockfile::load(Path::new(lock_path))?;
    let Some(recorded) = &lock.target.semantic_fingerprint else {
        return Err(format!(
            "{lock_path} records no semantic fingerprint (it predates the \
             feature) — one adopt or compile establishes the baseline"
        )
        .into());
    };
    let current = lxir::diff::semantic_fingerprint(&doc);
    if &current == recorded {
        println!("in sync: {config} matches the fingerprint in {lock_path}");
        Ok(ExitCode::SUCCESS)
    } else {
        println!(
            "drift: {config} no longer matches the fingerprint recorded in \
             {lock_path} — another writer changed something since the last \
             adopt/compile; run `lxir diff <last-compiled.Loxone> {config}` \
             to see what"
        );
        Ok(ExitCode::FAILURE)
    }
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
