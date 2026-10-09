//! Inspection and build/run commands for the nx CLI.
//!
//! Covers --lex, --parse, check, dump-ir, dump-hir, build (with its clang
//! pipeline and freshness cache), and run.

use std::process::ExitCode;

pub(crate) fn read_source(path: &str) -> Result<String, ExitCode> {
    std::fs::read_to_string(path).map_err(|e| {
        eprintln!("nx: cannot read '{path}': {e}");
        ExitCode::from(1)
    })
}

pub(crate) fn lex_file(path: &str) -> ExitCode {
    let source = match read_source(path) {
        Ok(s) => s,
        Err(c) => return c,
    };
    match nx_lexer::lex(&source) {
        Ok(tokens) => {
            for t in tokens {
                println!("{:?} {:?} {}:{}", t.kind, t.lexeme, t.line, t.col);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nx: {e}");
            ExitCode::from(1)
        }
    }
}

pub(crate) fn parse_file(path: &str) -> ExitCode {
    let source = match read_source(path) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let tokens = match nx_lexer::lex(&source) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("nx: {e}");
            return ExitCode::from(1);
        }
    };
    match nx_parser::parse(tokens) {
        Ok(prog) => {
            println!("{prog:#?}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nx: {e}");
            ExitCode::from(1)
        }
    }
}

pub(crate) fn check_file(path: &str) -> ExitCode {
    let source = match read_source(path) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let base = std::path::Path::new(path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(".".into());
    match nx_types::check_source(&source, &base) {
        Ok(()) => {
            println!("nx: no type errors");
            ExitCode::SUCCESS
        }
        Err(es) => {
            for e in es {
                eprintln!("nx: {e}");
            }
            ExitCode::from(1)
        }
    }
}

pub(crate) fn dump_ir_file(path: &str) -> ExitCode {
    let source = match read_source(path) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let base = std::path::Path::new(path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(".".into());
    match nx_ir::analyze(&source, &base) {
        Ok(ir) => {
            let mut keys: Vec<_> = ir.funcs.keys().collect();
            keys.sort();
            for k in keys {
                let f = &ir.funcs[k];
                println!("{}.{}({}): {}", k.0, k.1, f.params.join(", "), f.summary);
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nx: {e}");
            ExitCode::from(1)
        }
    }
}

pub(crate) fn dump_hir_file(path: &str) -> ExitCode {
    let source = match read_source(path) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let base = std::path::Path::new(path)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(".".into());
    // Lowering verifies its own output (hir.md §7), so reaching the
    // dump means the program is structurally sound. `verify` runs again
    // here so this command's contract holds on its own, whatever the
    // lowering path does.
    match nx_hir::lower::lower_source(&source, &base) {
        Ok(hir) => match nx_hir::verify::verify(&hir) {
            Ok(()) => {
                print!("{}", nx_hir::dump::dump(&hir));
                ExitCode::SUCCESS
            }
            Err(violations) => {
                for v in &violations {
                    eprintln!("nx: {v}");
                }
                ExitCode::from(1)
            }
        },
        Err(e) => {
            eprintln!("nx: {e}");
            ExitCode::from(1)
        }
    }
}

pub(crate) fn build_cmd(rest: &[String]) -> ExitCode {
    // nx build <file.nx> [-o <out>] [--run]
    let mut file: Option<String> = None;
    let mut out: Option<String> = None;
    let mut run = false;
    let mut emit_ir = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "-o" => {
                i += 1;
                if i >= rest.len() {
                    eprintln!("usage: nx build <file.nx> [-o <out>] [--run] [--emit-ir]");
                    return ExitCode::from(2);
                }
                out = Some(rest[i].clone());
            }
            "--run" => run = true,
            "--emit-ir" => emit_ir = true,
            f if file.is_none() => file = Some(f.to_string()),
            _ => {
                eprintln!("usage: nx build <file.nx> [-o <out>] [--run] [--emit-ir]");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }
    let file = match file {
        Some(f) => f,
        None => {
            eprintln!("usage: nx build <file.nx> [-o <out>] [--run] [--emit-ir]");
            return ExitCode::from(2);
        }
    };
    let out = out.unwrap_or_else(|| default_exe_name(&file));
    if emit_ir {
        // Print the LLVM IR instead of shelling out to clang: the fastest
        // way to see what the backend actually decided.
        return match build_ir(&file) {
            Ok(ir) => {
                print!("{ir}");
                ExitCode::SUCCESS
            }
            Err(c) => c,
        };
    }
    if up_to_date(&file, &out) {
        println!("nx: up to date ({out})");
    } else if let Err(c) = build_exe(&file, &out) {
        return c;
    }
    if run {
        return run_exe(&out);
    }
    ExitCode::SUCCESS
}

/// Env settings that change the emitted IR. Recorded next to the exe so a
/// flag flip (e.g. NX_NOMEMO=1) forces a rebuild instead of silently reusing
/// a stale binary.
pub(crate) fn build_stamp() -> String {
    format!(
        "nomemo={} cflags={}",
        std::env::var("NX_NOMEMO").unwrap_or_default(),
        std::env::var("NX_CFLAGS").unwrap_or_default()
    )
}

pub(crate) fn stamp_path(out: &str) -> std::path::PathBuf {
    let mut p = std::path::PathBuf::from(out);
    let name = p
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    p.set_file_name(format!("{name}.nxstamp"));
    p
}

pub(crate) fn write_stamp(out: &str) {
    let _ = std::fs::write(stamp_path(out), build_stamp());
}

/// Skip clang when the exe is newer than every input (entry + imports),
/// newer than nx itself, and was built with the same env flags.
pub(crate) fn up_to_date(file: &str, out: &str) -> bool {
    if std::fs::read_to_string(stamp_path(out)).ok().as_deref() != Some(build_stamp().as_str()) {
        return false;
    }
    let exe_meta = match std::fs::metadata(out) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let exe_time = match exe_meta.modified() {
        Ok(t) => t,
        Err(_) => return false,
    };
    let entry = std::path::Path::new(file);
    let base = entry
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(".".into());
    let mut inputs = match nx_codegen::dependencies(entry, &base) {
        Ok(v) => v,
        Err(_) => return false,
    };
    if let Ok(nx) = std::env::current_exe() {
        inputs.push(nx);
    }
    inputs.iter().all(|p| {
        std::fs::metadata(p)
            .and_then(|m| m.modified())
            .map(|t| exe_time >= t)
            .unwrap_or(false)
    })
}

/// Type-check and generate IR without invoking clang. Shared by
/// `build_exe` and `build --emit-ir`.
pub(crate) fn build_ir(file: &str) -> Result<String, ExitCode> {
    let source = read_source(file)?;
    let base = std::path::Path::new(file)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(".".into());
    // Types are mandatory for codegen.
    if let Err(es) = nx_types::check_source(&source, &base) {
        for e in es {
            eprintln!("nx: {e}");
        }
        return Err(ExitCode::from(1));
    }
    match nx_codegen::compile_entry(&source, &base) {
        Ok(ir) => Ok(ir),
        Err(e) => {
            eprintln!("nx: {e}");
            Err(ExitCode::from(1))
        }
    }
}

pub(crate) fn build_exe(file: &str, out: &str) -> Result<(), ExitCode> {
    // Create the output directory if the caller asked for one that does not
    // exist yet. Without this, `nx build a.nx -o build\release\a.exe` fails
    // deep inside the linker with a message that says nothing about nx.
    if let Some(dir) = std::path::Path::new(out).parent() {
        if !dir.as_os_str().is_empty() && !dir.exists() {
            if let Err(e) = std::fs::create_dir_all(dir) {
                eprintln!(
                    "nx: cannot create output directory '{}': {e}",
                    dir.display()
                );
                return Err(ExitCode::from(1));
            }
        }
    }
    let ir = build_ir(file)?;
    let ll = std::env::temp_dir().join(format!("nxbuild-{}.ll", std::process::id()));
    if let Err(e) = std::fs::write(&ll, ir) {
        eprintln!("nx: cannot write IR: {e}");
        return Err(ExitCode::from(1));
    }
    // Probe clang first for a helpful error.
    if std::process::Command::new("clang")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("nx: clang not found — install LLVM: winget install LLVM.LLVM");
        return Err(ExitCode::from(1));
    }
    let mut cmd = std::process::Command::new("clang");
    cmd.args(["-O2"]).arg(&ll).args(["-o"]).arg(out);
    // libm for log10/floor/pow/round on unix; MSVC links it implicitly.
    if cfg!(unix) {
        cmd.arg("-lm");
    }
    // Extra flags, e.g. NX_CFLAGS="-fsanitize=address -g" for CI.
    if let Ok(extra) = std::env::var("NX_CFLAGS") {
        for a in extra.split_whitespace() {
            cmd.arg(a);
        }
    }
    match cmd.status() {
        Ok(s) if s.success() => {
            write_stamp(out);
            println!("nx: built {out}");
            Ok(())
        }
        _ => {
            eprintln!("nx: clang failed");
            Err(ExitCode::from(1))
        }
    }
}

pub(crate) fn default_exe_name(file: &str) -> String {
    let stem = std::path::Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or("a".to_string());
    let dir = std::path::Path::new(file)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(".".into());
    let name = if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem
    };
    dir.join(name).to_string_lossy().to_string()
}

/// `nx run`: build the native exe when stale, then execute it.
pub(crate) fn run_native(path: &str, out_override: Option<&str>) -> ExitCode {
    // No -o means the executable lands beside its source file, so a project
    // stays self-contained and nothing litters the working directory.
    let out = match out_override {
        Some(o) => o.to_string(),
        None => default_exe_name(path),
    };
    if up_to_date(path, &out) {
        println!("nx: up to date ({out})");
    } else if let Err(c) = build_exe(path, &out) {
        return c;
    }
    run_exe(&out)
}

/// Run a built executable. The path is resolved against the current
/// directory first: a bare `main.exe` does not resolve through it on
/// Windows, so `nx run main.nx` in the program's own directory built the
/// file and then reported "program not found" for it. The message still
/// names the path as given, not the resolved one.
pub(crate) fn run_exe(out: &str) -> ExitCode {
    let abs = resolve_exe(out);
    match std::process::Command::new(&abs).status() {
        Ok(s) => match s.code() {
            Some(c) => ExitCode::from(c as u8),
            None => ExitCode::FAILURE,
        },
        Err(e) => {
            eprintln!("nx: cannot run {out}: {e}");
            ExitCode::from(1)
        }
    }
}

/// Absolute path of a built executable for spawning. A bare `main.exe`
/// does not resolve through the current directory on Windows, so this
/// joins it explicitly; absolute `-o` paths pass through unchanged.
pub(crate) fn resolve_exe(out: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(out);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(p),
        Err(_) => p.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn exe_spawns_by_absolute_path() {
        // A bare `main.exe` does not resolve through the current
        // directory on Windows, so the run path joins it explicitly.
        let rel = super::resolve_exe("main.exe");
        assert!(rel.is_absolute());
        assert_eq!(rel.file_name().unwrap(), "main.exe");
        // Absolute `-o` paths pass through unchanged.
        let abs = if cfg!(windows) {
            "C:\\out\\main.exe"
        } else {
            "/tmp/out/main"
        };
        assert_eq!(super::resolve_exe(abs), std::path::PathBuf::from(abs));
    }
}
