use clap::{Parser, Subcommand};
use nexum_diagnostics::render;
use nexum_mir::{lower, optimize_level, verify};
use nexum_sema::analyze;
use nexum_target::TargetSpec;
use nexum_toolchain::Toolchain;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Parser)]
#[command(name = "nx", version, about = "Nexum compiler (bootstrap)")]
struct Cli {
    /// Optimization level (0 = none, 1 = const-fold + DCE + inlining).
    #[arg(long, global = true, default_value_t = 0u8)]
    opt_level: u8,
    /// Target profile (e.g. windows-x86_64, linux-x86_64, macos-aarch64).
    #[arg(long, global = true, default_value = "host")]
    target: String,
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run {
        file: Option<PathBuf>,
    },
    Check {
        file: Option<PathBuf>,
    },
    Build {
        file: Option<PathBuf>,
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Clang optimization level 0-3.
        #[arg(long, default_value_t = 2u8)]
        opt: u8,
        /// Enable thin LTO.
        #[arg(long)]
        lto: bool,
        /// Sysroot path.
        #[arg(long)]
        sysroot: Option<String>,
        /// PGO: generate profile data.
        #[arg(long)]
        pgo_generate: bool,
        /// PGO: use profile data from path.
        #[arg(long)]
        pgo_use: Option<String>,
        /// Release profile: opt 3 + LTO.
        #[arg(long)]
        release: bool,
    },
    /// Compile and print LLVM IR to stdout.
    EmitIr {
        file: PathBuf,
    },
    Update,
    Lex {
        file: PathBuf,
    },
    New {
        name: String,
    },
    Fmt {
        file: PathBuf,
        #[arg(long)]
        write: bool,
    },
    Test {
        #[arg(default_value = "tests")]
        dir: PathBuf,
    },
    Doctor,
    Explain {
        code: String,
    },
    Analyze {
        file: PathBuf,
        #[arg(long, default_value_t = 0u8)]
        opt_level: u8,
    },
    ExplainToolchain,
}

fn main() {
    let cli = Cli::parse();
    *TARGET_NAME.lock().unwrap() = cli.target.clone();
    let opt_level = cli.opt_level;
    match cli.cmd {
        Commands::Update => {
            update_nexum();
        }
        Commands::Lex { file } => {
            if let Err(e) = lex_file(&file) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
        Commands::New { name } => {
            new_project(&name);
        }
        Commands::Fmt { file, write } => {
            let text = fs::read_to_string(&file).unwrap_or_else(|e| {
                eprintln!("error: {e}");
                std::process::exit(1);
            });
            match format_nx(&text) {
                Ok(out) => {
                    if write {
                        fs::write(&file, out).unwrap_or_else(|e| {
                            eprintln!("error: {e}");
                            std::process::exit(1);
                        });
                        println!("formatted: {}", file.display());
                    } else {
                        print!("{}", out);
                    }
                }
                Err(e) => {
                    eprintln!("{}", e);
                    std::process::exit(1);
                }
            }
        }
        Commands::Test { dir } => {
            run_tests(&dir);
        }
        Commands::Doctor => {
            doctor();
        }
        Commands::Explain { code } => {
            explain(&code);
        }
        Commands::Analyze { file, opt_level } => match compile_to_mir(&file, opt_level) {
            Ok((_sm, mir)) => {
                print!("{}", nexum_mir::escape_analysis(&mir));
            }
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        },
        Commands::ExplainToolchain => match Toolchain::discover() {
            Ok(tc) => println!("{}", tc.explain()),
            Err(e) => eprintln!("error: {}", e),
        },
        Commands::Check { file } => {
            let file = resolve_entry(file);
            if let Err(e) = check_file(&file, opt_level) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
        Commands::Build {
            file,
            output,
            opt,
            lto,
            sysroot,
            pgo_generate,
            pgo_use,
            release,
        } => {
            let file = resolve_entry(file);
            *BUILD_OPTS.lock().unwrap() = nexum_toolchain::BuildOpts {
                opt: if release { 3 } else { opt },
                lto: lto || release,
                sysroot,
                pgo_generate,
                pgo_use,
            };
            if let Err(e) = build_file(&file, output, opt_level) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
        Commands::EmitIr { file } => match compile_to_mir(&file, opt_level) {
            Ok((_sm, mir)) => {
                let target = current_target();
                print!("{}", nexum_codegen::emit(&mir, &target));
            }
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        },
        Commands::Run { file } => {
            let file = resolve_entry(file);
            if let Err(e) = run_file(&file, opt_level) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
    }
}

fn new_project(name: &str) {
    use std::fs;
    let root = std::path::Path::new(name);
    if root.exists() {
        eprintln!("error: directory `{}` already exists", name);
        std::process::exit(1);
    }
    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("tests")).unwrap();
    fs::write(
        root.join("nexum.toml"),
        format!(
            "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nentry = \"src/main.nx\"\n",
            name
        ),
    )
    .unwrap();
    fs::write(
        root.join("src/main.nx"),
        "fn main():\n    print(\"Hello from Nexum!\")\n",
    )
    .unwrap();
    fs::write(
        root.join("tests/smoke.nx"),
        "fn main():\n    print(\"ok\")\n",
    )
    .unwrap();
    fs::write(
        root.join("README.md"),
        format!("# {}\n\nA Nexum project.\n", name),
    )
    .unwrap();
    println!("created project `{}`", name);
    println!("  cd {} && nx run src/main.nx", name);
}

fn update_nexum() {
    use std::process::Command;
    let current = env!("CARGO_PKG_VERSION");
    println!("current version: {}", current);
    // Fetch latest release tag via curl.
    let api = "https://api.github.com/repos/Salaou-Hasan/nexum/releases/latest";
    let out = Command::new("curl").args(["-sL", api]).output();
    let body = match out {
        Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
        Err(e) => {
            eprintln!("error: curl not available: {e}");
            std::process::exit(1);
        }
    };
    // Parse tag_name manually (avoid serde dep in driver).
    let tag = body
        .split("\"tag_name\"")
        .nth(1)
        .and_then(|s| s.split('"').nth(1))
        .unwrap_or("")
        .to_string();
    if tag.is_empty() {
        eprintln!("error: could not determine latest release");
        std::process::exit(1);
    }
    let latest = tag.trim_start_matches('v');
    println!("latest release: {}", tag);
    if latest == current {
        println!("already up to date");
        return;
    }
    // Determine host target.
    let target = if cfg!(target_os = "windows") {
        "windows-x86_64"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "macos-aarch64"
        } else {
            "macos-x86_64"
        }
    } else if cfg!(target_arch = "aarch64") {
        "linux-aarch64"
    } else {
        "linux-x86_64"
    };
    let ext = if cfg!(target_os = "windows") {
        ".exe"
    } else {
        ""
    };
    let asset = format!("nx-{}{}", target, ext);
    let url = format!(
        "https://github.com/Salaou-Hasan/nexum/releases/download/{}/{}",
        tag, asset
    );
    let tmp = std::env::temp_dir().join(&asset);
    println!("downloading {} ...", url);
    let status = Command::new("curl")
        .args(["-sL", "-o", tmp.to_str().unwrap(), &url])
        .status();
    match status {
        Ok(s) if s.success() && tmp.exists() => {}
        _ => {
            eprintln!("error: download failed");
            std::process::exit(1);
        }
    }
    // Replace current binary.
    let current_exe = std::env::current_exe().unwrap();
    let old = current_exe.with_extension("old");
    let _ = std::fs::remove_file(&old);
    if std::fs::rename(&current_exe, &old).is_err() {
        eprintln!("error: could not rename current binary (try running as admin)");
        std::process::exit(1);
    }
    if std::fs::rename(&tmp, &current_exe).is_err() {
        // restore
        let _ = std::fs::rename(&old, &current_exe);
        eprintln!("error: could not install new binary");
        std::process::exit(1);
    }
    let _ = std::fs::remove_file(&old);
    println!("updated to {} ÃƒÆ’Ã‚Â¢ÃƒÂ¢Ã¢â‚¬Å¡Ã‚Â¬ÃƒÂ¢Ã¢â€šÂ¬Ã‚Â restart nx to use the new version", tag);
}

fn format_nx(text: &str) -> Result<String, String> {
    use nexum_parser::{BinOp, Expr, ExprKind, ForIter, Stmt, StmtKind, TypeExpr, UnOp};
    let mut sm = nexum_source::SourceMap::new();
    let fid = sm.add(std::path::PathBuf::from("fmt.nx"), text.to_string());
    let sf = sm.get(fid);
    let lexed = nexum_lexer::lex(sf);
    if lexed
        .diagnostics
        .iter()
        .any(|d| d.severity == nexum_diagnostics::Severity::Error)
    {
        return Err("lex errors".into());
    }
    let parsed = nexum_parser::parse(&lexed.tokens);
    if parsed
        .diagnostics
        .iter()
        .any(|d| d.severity == nexum_diagnostics::Severity::Error)
    {
        return Err("parse errors".into());
    }

    fn ind(out: &mut String, level: usize) {
        for _ in 0..level {
            out.push_str("    ");
        }
    }
    fn fmt_type(out: &mut String, t: &TypeExpr) {
        match t {
            TypeExpr::Named(n) => out.push_str(n),
            TypeExpr::Array(e, n) => {
                out.push('[');
                fmt_type(out, e);
                out.push_str(&format!("; {}", n));
                out.push(']');
            }
        }
    }
    fn binop_str(op: BinOp) -> &'static str {
        match op {
            BinOp::Add => " + ",
            BinOp::Sub => " - ",
            BinOp::Mul => " * ",
            BinOp::Div => " / ",
            BinOp::Mod => " % ",
            BinOp::Eq => " == ",
            BinOp::Ne => " != ",
            BinOp::Lt => " < ",
            BinOp::Gt => " > ",
            BinOp::Le => " <= ",
            BinOp::Ge => " >= ",
            BinOp::And => " and ",
            BinOp::Or => " or ",
        }
    }
    fn compound_str(op: BinOp) -> &'static str {
        match op {
            BinOp::Add => " += ",
            BinOp::Sub => " -= ",
            BinOp::Mul => " *= ",
            BinOp::Div => " /= ",
            BinOp::Mod => " %= ",
            _ => " = ",
        }
    }
    fn fmt_expr(out: &mut String, e: &Expr) {
        match &e.kind {
            ExprKind::Int(v) => out.push_str(&v.to_string()),
            ExprKind::Float(v) => {
                let mut s = format!("{}", v);
                if !s.contains('.') && !s.contains('e') && !s.contains('E') {
                    s.push_str(".0");
                }
                out.push_str(&s);
            }
            ExprKind::Bool(v) => out.push_str(if *v { "true" } else { "false" }),
            ExprKind::Str(s) => {
                out.push('"');
                for ch in s.chars() {
                    match ch {
                        '\\' => out.push_str("\\\\"),
                        '"' => out.push_str("\\\""),
                        '\n' => out.push_str("\\n"),
                        '\t' => out.push_str("\\t"),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            ExprKind::Var(n) => out.push_str(n),
            ExprKind::Call { callee, args } => {
                out.push_str(callee);
                out.push('(');
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    fmt_expr(out, a);
                }
                out.push(')');
            }
            ExprKind::Binary { op, lhs, rhs } => {
                out.push('(');
                fmt_expr(out, lhs);
                out.push_str(binop_str(*op));
                fmt_expr(out, rhs);
                out.push(')');
            }
            ExprKind::Unary { op, expr } => match op {
                UnOp::Neg => {
                    out.push('-');
                    fmt_expr(out, expr);
                }
                UnOp::Not => {
                    out.push_str("not ");
                    fmt_expr(out, expr);
                }
            },
            ExprKind::Field { base, name } => {
                fmt_expr(out, base);
                out.push('.');
                out.push_str(name);
            }
            ExprKind::Index { base, index } => {
                fmt_expr(out, base);
                out.push('[');
                fmt_expr(out, index);
                out.push(']');
            }
            ExprKind::Array(elems) => {
                out.push('[');
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    fmt_expr(out, e);
                }
                out.push(']');
            }
        }
    }
    fn fmt_path(out: &mut String, root: &str, path: &[nexum_parser::PathSeg]) {
        out.push_str(root);
        for seg in path {
            match seg {
                nexum_parser::PathSeg::Field(n) => {
                    out.push('.');
                    out.push_str(n);
                }
                nexum_parser::PathSeg::Index(e) => {
                    out.push('[');
                    fmt_expr(out, e);
                    out.push(']');
                }
            }
        }
    }
    fn fmt_stmt(out: &mut String, s: &Stmt, level: usize) {
        match &s.kind {
            StmtKind::Fn {
                name,
                params,
                ret,
                body,
            } => {
                ind(out, level);
                out.push_str("fn ");
                out.push_str(name);
                out.push('(');
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    if p.mutable {
                        out.push_str("mut ");
                    }
                    out.push_str(&p.name);
                    if let Some(t) = &p.ty {
                        out.push_str(": ");
                        fmt_type(out, t);
                    }
                }
                out.push(')');
                if let Some(t) = ret {
                    out.push_str(" -> ");
                    fmt_type(out, t);
                }
                out.push_str(":\n");
                for b in body {
                    fmt_stmt(out, b, level + 1);
                    out.push('\n');
                }
            }
            StmtKind::Let {
                name,
                mutable,
                ty,
                value,
            } => {
                ind(out, level);
                out.push_str("let ");
                if *mutable {
                    out.push_str("mut ");
                }
                out.push_str(name);
                if let Some(t) = ty {
                    out.push_str(": ");
                    fmt_type(out, t);
                }
                out.push_str(" = ");
                fmt_expr(out, value);
            }
            StmtKind::Assign { name, value, op } => {
                ind(out, level);
                out.push_str(name);
                match op {
                    Some(o) => out.push_str(compound_str(*o)),
                    None => out.push_str(" = "),
                }
                fmt_expr(out, value);
            }
            StmtKind::PathAssign {
                root,
                path,
                value,
                op,
            } => {
                ind(out, level);
                fmt_path(out, root, path);
                match op {
                    Some(o) => out.push_str(compound_str(*o)),
                    None => out.push_str(" = "),
                }
                fmt_expr(out, value);
            }
            StmtKind::TypeDecl { name, fields } => {
                ind(out, level);
                out.push_str("type ");
                out.push_str(name);
                out.push_str(":\n");
                for (fname, fty) in fields {
                    ind(out, level + 1);
                    out.push_str(fname);
                    out.push_str(": ");
                    fmt_type(out, fty);
                    out.push('\n');
                }
            }
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                ind(out, level);
                out.push_str("if ");
                fmt_expr(out, cond);
                out.push_str(":\n");
                for b in then_branch {
                    fmt_stmt(out, b, level + 1);
                    out.push('\n');
                }
                if let Some(eb) = else_branch {
                    ind(out, level);
                    out.push_str("else:\n");
                    for b in eb {
                        fmt_stmt(out, b, level + 1);
                        out.push('\n');
                    }
                }
            }
            StmtKind::While { cond, body } => {
                ind(out, level);
                out.push_str("while ");
                fmt_expr(out, cond);
                out.push_str(":\n");
                for b in body {
                    fmt_stmt(out, b, level + 1);
                    out.push('\n');
                }
            }
            StmtKind::For { var, iter, body } => {
                ind(out, level);
                out.push_str("for ");
                out.push_str(var);
                out.push_str(" in ");
                match iter {
                    ForIter::Range(lo, hi) => {
                        fmt_expr(out, lo);
                        out.push_str("..");
                        fmt_expr(out, hi);
                    }
                    ForIter::Each(e) => {
                        fmt_expr(out, e);
                    }
                }
                out.push_str(":\n");
                for b in body {
                    fmt_stmt(out, b, level + 1);
                    out.push('\n');
                }
            }
            StmtKind::Return { value } => {
                ind(out, level);
                out.push_str("return");
                if let Some(v) = value {
                    out.push(' ');
                    fmt_expr(out, v);
                }
            }
            StmtKind::Expr(e) => {
                ind(out, level);
                fmt_expr(out, e);
            }
            StmtKind::Import { path } => {
                ind(out, level);
                out.push_str("import \"");
                out.push_str(path);
                out.push('"');
            }
        }
    }

    let mut out = String::new();
    for s in &parsed.module.stmts {
        fmt_stmt(&mut out, s, 0);
        out.push('\n');
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

fn run_tests(dir: &std::path::Path) {
    use std::process::Command;
    if !dir.exists() {
        eprintln!("test dir not found: {}", dir.display());
        std::process::exit(1);
    }
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "nx").unwrap_or(false))
        .collect();
    files.sort();
    if files.is_empty() {
        println!("no test files in {}", dir.display());
        return;
    }
    let mut passed = 0;
    let mut failed = 0;
    for f in &files {
        let status = Command::new(std::env::current_exe().unwrap())
            .arg("run")
            .arg(f)
            .status();
        match status {
            Ok(s) if s.success() => {
                println!("PASS  {}", f.display());
                passed += 1;
            }
            _ => {
                println!("FAIL  {}", f.display());
                failed += 1;
            }
        }
    }
    println!("\n{} passed, {} failed", passed, failed);
    if failed > 0 {
        std::process::exit(1);
    }
}

fn doctor() {
    println!("Nexum doctor");
    println!("============");
    // rustc
    print_tool("rustc", &["--version"]);
    print_tool("cargo", &["--version"]);
    print_tool("clang", &["--version"]);
    // nx itself
    println!("nx: {}", std::env::current_exe().unwrap().display());
}

fn print_tool(name: &str, args: &[&str]) {
    use std::process::Command;
    match Command::new(name).args(args).output() {
        Ok(o) => {
            let first = String::from_utf8_lossy(&o.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            println!(
                "  {}: {}",
                name,
                if first.is_empty() { "ok" } else { &first }
            );
        }
        Err(_) => println!("  {}: NOT FOUND", name),
    }
}

fn explain(code: &str) {
    let code = code.to_uppercase();
    let msg = match code.as_str() {
        "T001" => "condition in `if` must be Bool.",
        "T002" => "condition in `while` must be Bool.",
        "T005" => "wrong number of arguments in a call.",
        "T007" => "argument type mismatch.",
        "T008" => "arithmetic operands must be numeric and of the same type.",
        "T010" => "assignment to an immutable binding.",
        "T017" => "binary operator type mismatch.",
        "T019" => "record has no such field.",
        "T020" => "field access on a non-record value.",
        "T022" => "duplicate type definition.",
        "T023" => "duplicate field in a record.",
        "T024" => "unknown type in a field annotation.",
        "T026" => "panic expects exactly one Str argument.",
        "T027" => "record must declare at least one field.",
        "T028" => "print does not support record or array values.",
        "T029" => "empty array literal (zero-length arrays unsupported).",
        "T030" => "zero-length array annotation.",
        "T031" => "indexing a non-array value.",
        "T032" => "array index must be Int.",
        "T033" => "constant array index out of bounds.",
        "T034" => "len expects an array (or Str).",
        "T035" => "array element type mismatch.",
        "T036" => "invalid array element type (Unit/Never).",
        "T037" => "`mut` argument is not a mutable variable.",
        "T038" => "for-each over a non-array value.",
        "T039" => "reserved (managed Str in aggregates is now supported).",
        "T040" => "concat expects two Str arguments.",
        "T041" => "collect expects no arguments.",
        "T042" => "heap_used expects no arguments.",
        "T043" => "str_char_at arity/type error.",
        "T044" => "slice arity/type error.",
        "T045" => "read_file arity/type error.",
        "T046" => "write_file arity/type error.",
        "T047" => "cannot import a module (missing or unreadable file).",
        "T048" => "list builtin arity/type error.",
        "T049" => "list element type error (mismatch or not inferable).",
        _ => "unknown diagnostic code.",
    };
    println!("{}: {}", code, msg);
    println!("See docs/language-reference/grammar.md for the full table.");
}

static TARGET_NAME: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

static BUILD_OPTS: std::sync::Mutex<nexum_toolchain::BuildOpts> =
    std::sync::Mutex::new(nexum_toolchain::BuildOpts {
        opt: 2,
        lto: false,
        sysroot: None,
        pgo_generate: false,
        pgo_use: None,
    });

fn current_build_opts() -> nexum_toolchain::BuildOpts {
    BUILD_OPTS.lock().unwrap().clone()
}

fn resolve_entry(file: Option<PathBuf>) -> PathBuf {
    if let Some(f) = file {
        return f;
    }
    // Auto-detect from nexum.toml in the current directory.
    let manifest = std::path::Path::new("nexum.toml");
    if manifest.exists()
        && let Ok(text) = std::fs::read_to_string(manifest) {
            for line in text.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("entry") {
                    let rest = rest.trim().trim_start_matches('=').trim();
                    let rest = rest.trim_matches('"');
                    if !rest.is_empty() {
                        return std::path::PathBuf::from(rest);
                    }
                }
            }
        }
    eprintln!("error: no input file and no nexum.toml found");
    std::process::exit(1);
}

fn current_target() -> TargetSpec {
    let name = TARGET_NAME.lock().unwrap().clone();
    if name.is_empty() || name == "host" {
        TargetSpec::host()
    } else {
        TargetSpec::by_name(&name).unwrap_or_else(TargetSpec::host)
    }
}

fn compile_to_mir(
    file: &Path,
    opt_level: u8,
) -> Result<(nexum_source::SourceMap, nexum_mir::MirModule), String> {
    // Load the file and every module it imports into one source map, so that
    // diagnostics always point at the file they came from.
    let mut sm = nexum_source::SourceMap::new();
    let (module, load_diags) = nexum_parser::load_module(&mut sm, file);
    for d in &load_diags {
        eprintln!("{}", render(d, &sm));
    }
    if load_diags
        .iter()
        .any(|d| d.severity == nexum_diagnostics::Severity::Error)
    {
        return Err("parse errors".into());
    }
    let sema = analyze(&module);
    for d in &sema.diagnostics {
        eprintln!("{}", render(d, &sm));
    }
    if sema
        .diagnostics
        .iter()
        .any(|d| d.severity == nexum_diagnostics::Severity::Error)
    {
        return Err("type errors".into());
    }
    let hir = sema.hir.ok_or("no hir")?;
    let lowered = lower(&hir);
    let mut mir = lowered.module;
    optimize_level(&mut mir, opt_level);
    let vdiags = verify(&mir);
    for d in &vdiags {
        eprintln!("{}", render(d, &sm));
    }
    Ok((sm, mir))
}

/// Dump the token stream of a file, one token per line, as
/// `<kind>\t<text>`. The format is stable so that the self-hosted lexer can be
/// compared against this one token by token.
fn lex_file(file: &Path) -> Result<(), String> {
    let text = fs::read_to_string(file).map_err(|e| e.to_string())?;
    let mut sm = nexum_source::SourceMap::new();
    let fid = sm.add(file.to_path_buf(), text);
    let sf = sm.get(fid);
    let out = nexum_lexer::lex(sf);
    for d in &out.diagnostics {
        eprintln!("{}", render(d, &sm));
    }
    let mut buf = String::new();
    for t in &out.tokens {
        buf.push_str(t.kind.name());
        buf.push('\t');
        buf.push('\n');
        buf.push_str(&t.text);
        buf.push('\n');
    }
    print!("{}", buf);
    Ok(())
}

fn check_file(file: &Path, opt_level: u8) -> Result<(), String> {
    let (sm, _mir) = compile_to_mir(file, opt_level)?;
    println!("check: ok");
    let _ = sm;
    Ok(())
}

fn build_file(file: &Path, output: Option<PathBuf>, opt_level: u8) -> Result<(), String> {
    let (_sm, mir) = compile_to_mir(file, opt_level)?;
    let tc = Toolchain::discover().map_err(|e| e.to_string())?;
    let target = current_target();
    let ir = nexum_codegen::emit(&mir, &target);
    // write .ll
    let out_exe = output.unwrap_or_else(|| {
        let mut p = file.to_path_buf();
        p.set_extension("exe");
        p
    });
    let ll_path = std::env::temp_dir().join("nexum_out.ll");
    fs::write(&ll_path, ir).map_err(|e| e.to_string())?;
    nexum_toolchain::verify_ir(&tc.clang, ll_path.to_str().unwrap())?;
    nexum_toolchain::run_clang_with(
        &tc.clang,
        ll_path.to_str().unwrap(),
        out_exe.to_str().unwrap(),
        &current_build_opts(),
    )
    .map_err(|e| e.to_string())?;
    println!("built: {}", out_exe.display());
    Ok(())
}

fn run_file(file: &Path, opt_level: u8) -> Result<(), String> {
    let exe = std::env::temp_dir().join("nexum_run.exe");
    build_to(file, &exe, opt_level)?;
    let status = Command::new(&exe).status().map_err(|e| e.to_string())?;
    std::process::exit(status.code().unwrap_or(0));
}

fn build_to(file: &Path, exe: &Path, opt_level: u8) -> Result<(), String> {
    let (_sm, mir) = compile_to_mir(file, opt_level)?;
    let tc = Toolchain::discover().map_err(|e| e.to_string())?;
    let target = current_target();
    let ir = nexum_codegen::emit(&mir, &target);
    let ll_path = std::env::temp_dir().join("nexum_out.ll");
    fs::write(&ll_path, ir).map_err(|e| e.to_string())?;
    nexum_toolchain::verify_ir(&tc.clang, ll_path.to_str().unwrap())?;
    nexum_toolchain::run_clang_with(
        &tc.clang,
        ll_path.to_str().unwrap(),
        exe.to_str().unwrap(),
        &current_build_opts(),
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
