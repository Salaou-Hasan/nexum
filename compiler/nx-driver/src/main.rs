//! nx command-line driver.
//!
//! Parses arguments, dispatches to one subcommand, and reports its exit code.

use std::process::ExitCode;

mod commands;
mod setup;
mod update;

fn usage() -> String {
    "usage: nx <file.nx>              (build to native and run it)\n       nx run <file.nx> [-o <out>]    (build if needed, run native)\n       nx build <file.nx> [-o <out>] [--run] [--emit-ir]\n       nx check <file.nx>             (type check only, no output file)\n       nx dump-ir <file.nx>           (print the LLVM IR)
       nx dump-hir <file.nx>          (print the lowered HIR)\n       nx --lex <file.nx> | --parse <file.nx>\n       nx --version | --license\n       nx setup [--apply] | nx update [--version <ver>]\n\nCompilation is ahead-of-time: every program becomes a native executable.\nWith no -o, the executable is written next to its .nx source file.".to_string()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 2 && args[1] == "--version" {
        println!("nx {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if args.len() == 2 && args[1] == "--license" {
        // MIT text embedded at compile time, so the .exe carries it.
        print!("{}", include_str!("../../../LICENSE"));
        return ExitCode::SUCCESS;
    }
    if args.len() >= 2 && args[1] == "update" {
        return update::update_cmd(&args[2..]);
    }
    if args.len() >= 2 && args[1] == "setup" {
        return setup::setup_cmd(&args[2..]);
    }
    if args.len() == 3 && args[1] == "--lex" {
        return commands::lex_file(&args[2]);
    }
    if args.len() == 3 && args[1] == "--parse" {
        return commands::parse_file(&args[2]);
    }

    // nx run <file.nx> [-o <out>] -- build if needed, then run natively.
    if args.len() >= 3 && args[1] == "run" {
        let mut file: Option<String> = None;
        let mut out: Option<String> = None;
        let mut i = 0;
        while i < args[2..].len() {
            match args[2 + i].as_str() {
                "-o" => {
                    i += 1;
                    if 2 + i >= args.len() {
                        eprintln!("usage: nx run <file.nx> [-o <out>]");
                        return ExitCode::from(2);
                    }
                    out = Some(args[2 + i].clone());
                }
                f if file.is_none() => file = Some(f.to_string()),
                _ => {
                    eprintln!("usage: nx run <file.nx> [-o <out>]");
                    return ExitCode::from(2);
                }
            }
            i += 1;
        }
        let file = match file {
            Some(f) => f,
            None => {
                eprintln!("usage: nx run <file.nx> [-o <out>]");
                return ExitCode::from(2);
            }
        };
        return commands::run_native(&file, out.as_deref());
    }
    if args.len() == 3 && args[1] == "check" {
        return commands::check_file(&args[2]);
    }
    if args.len() == 3 && args[1] == "dump-ir" {
        return commands::dump_ir_file(&args[2]);
    }
    if args.len() == 3 && args[1] == "dump-hir" {
        return commands::dump_hir_file(&args[2]);
    }
    if args.len() >= 3 && args[1] == "build" {
        return commands::build_cmd(&args[2..]);
    }
    // Default: `nx <file.nx>` compiles to a native executable and runs it.
    // There is no interpreter path in this compiler.
    if args.len() == 2 && !args[1].starts_with('-') {
        return commands::run_native(&args[1], None);
    }
    eprintln!("{}", usage());
    ExitCode::from(2)
}
