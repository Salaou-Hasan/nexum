use clap::{Parser, Subcommand};
use nexum_diagnostics::render;
use nexum_lexer::lex;
use nexum_mir::{lower, optimize_level, verify};
use nexum_parser::parse;
use nexum_sema::analyze;
use nexum_target::TargetSpec;
use nexum_toolchain::{run_clang, Toolchain};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

#[derive(Parser)]
#[command(name = "nx", version, about = "Nexum compiler (bootstrap)")]
struct Cli {
    /// Optimization level (0 = none, 1 = const-fold + DCE + inlining).
    #[arg(long, global = true, default_value_t = 0u8)]
    opt_level: u8,
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Run {
        file: PathBuf,
    },
    Check {
        file: PathBuf,
    },
    Build {
        file: PathBuf,
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Compile and print LLVM IR to stdout.
    EmitIr {
        file: PathBuf,
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
    let opt_level = cli.opt_level;
    match cli.cmd {
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
            if let Err(e) = check_file(&file, opt_level) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
        Commands::Build { file, output } => {
            if let Err(e) = build_file(&file, output, opt_level) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
        Commands::EmitIr { file } => match compile_to_mir(&file, opt_level) {
            Ok((_sm, mir)) => {
                let target = TargetSpec::host();
                print!("{}", nexum_codegen::emit(&mir, &target));
            }
            Err(e) => {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        },
        Commands::Run { file } => {
            if let Err(e) = run_file(&file, opt_level) {
                eprintln!("{}", e);
                std::process::exit(1);
            }
        }
    }
}

fn read_source(file: &PathBuf) -> Result<(nexum_source::SourceMap, nexum_source::FileId), String> {
    let text = fs::read_to_string(file).map_err(|e| e.to_string())?;
    let mut sm = nexum_source::SourceMap::new();
    let fid = sm.add(file.clone(), text);
    Ok((sm, fid))
}

fn compile_to_mir(
    file: &PathBuf,
    opt_level: u8,
) -> Result<(nexum_source::SourceMap, nexum_mir::MirModule), String> {
    let (sm, fid) = read_source(file)?;
    let sf = sm.get(fid);
    let lexed = lex(sf);
    for d in &lexed.diagnostics {
        eprintln!("{}", render(d, &sm));
    }
    if lexed
        .diagnostics
        .iter()
        .any(|d| d.severity == nexum_diagnostics::Severity::Error)
    {
        return Err("lex errors".into());
    }
    let parsed = parse(&lexed.tokens);
    for d in &parsed.diagnostics {
        eprintln!("{}", render(d, &sm));
    }
    if parsed
        .diagnostics
        .iter()
        .any(|d| d.severity == nexum_diagnostics::Severity::Error)
    {
        return Err("parse errors".into());
    }
    let sema = analyze(&parsed.module);
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

fn check_file(file: &PathBuf, opt_level: u8) -> Result<(), String> {
    let (sm, _mir) = compile_to_mir(file, opt_level)?;
    println!("check: ok");
    let _ = sm;
    Ok(())
}

fn build_file(file: &PathBuf, output: Option<PathBuf>, opt_level: u8) -> Result<(), String> {
    let (_sm, mir) = compile_to_mir(file, opt_level)?;
    let tc = Toolchain::discover().map_err(|e| e.to_string())?;
    let target = TargetSpec::host();
    let ir = nexum_codegen::emit(&mir, &target);
    // write .ll
    let out_exe = output.unwrap_or_else(|| {
        let mut p = file.clone();
        p.set_extension("exe");
        p
    });
    let ll_path = std::env::temp_dir().join("nexum_out.ll");
    fs::write(&ll_path, ir).map_err(|e| e.to_string())?;
    nexum_toolchain::verify_ir(&tc.clang, ll_path.to_str().unwrap())?;
    run_clang(
        &tc.clang,
        ll_path.to_str().unwrap(),
        out_exe.to_str().unwrap(),
    )
    .map_err(|e| e.to_string())?;
    println!("built: {}", out_exe.display());
    Ok(())
}

fn run_file(file: &PathBuf, opt_level: u8) -> Result<(), String> {
    let exe = std::env::temp_dir().join("nexum_run.exe");
    build_to(file, &exe, opt_level)?;
    let status = Command::new(&exe).status().map_err(|e| e.to_string())?;
    std::process::exit(status.code().unwrap_or(0));
}

fn build_to(file: &PathBuf, exe: &PathBuf, opt_level: u8) -> Result<(), String> {
    let (_sm, mir) = compile_to_mir(file, opt_level)?;
    let tc = Toolchain::discover().map_err(|e| e.to_string())?;
    let target = TargetSpec::host();
    let ir = nexum_codegen::emit(&mir, &target);
    let ll_path = std::env::temp_dir().join("nexum_out.ll");
    fs::write(&ll_path, ir).map_err(|e| e.to_string())?;
    nexum_toolchain::verify_ir(&tc.clang, ll_path.to_str().unwrap())?;
    run_clang(&tc.clang, ll_path.to_str().unwrap(), exe.to_str().unwrap())
        .map_err(|e| e.to_string())?;
    Ok(())
}
