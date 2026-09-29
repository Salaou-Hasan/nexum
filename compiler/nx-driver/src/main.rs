use std::process::ExitCode;

fn usage() -> String {
    "usage: nx <file.nx>\n       nx --lex <file.nx>\n       nx --parse <file.nx>\n       nx --run <file.nx>".to_string()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 3 && args[1] == "--lex" {
        return lex_file(&args[2]);
    }
    if args.len() == 3 && args[1] == "--parse" {
        return parse_file(&args[2]);
    }
    if args.len() == 3 && args[1] == "--run" {
        return run_file(&args[2]);
    }
    // Default: nx <file.nx> runs the program.
    if args.len() == 2 && !args[1].starts_with('-') {
        return run_file(&args[1]);
    }
    eprintln!("{}", usage());
    ExitCode::from(2)
}

fn read_source(path: &str) -> Result<String, ExitCode> {
    std::fs::read_to_string(path).map_err(|e| {
        eprintln!("nx: cannot read '{path}': {e}");
        ExitCode::from(1)
    })
}

fn lex_file(path: &str) -> ExitCode {
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

fn parse_file(path: &str) -> ExitCode {
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

fn run_file(path: &str) -> ExitCode {
    let source = match read_source(path) {
        Ok(s) => s,
        Err(c) => return c,
    };
    // Run the interpreter on a thread with a large stack so deep (but
    // bounded) Nexum recursion hits our CALL_LIMIT error instead of
    // overflowing the small default Windows main-thread stack.
    let child = std::thread::Builder::new()
        .name("nx-run".to_string())
        .stack_size(64 * 1024 * 1024)
        .spawn(move || -> Result<Vec<String>, String> {
            let tokens = nx_lexer::lex(&source).map_err(|e| e.to_string())?;
            let prog = nx_parser::parse(tokens).map_err(|e| e.to_string())?;
            nx_interp::run(&prog).map_err(|e| e.to_string())
        });
    let child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("nx: cannot spawn run thread: {e}");
            return ExitCode::from(1);
        }
    };
    match child.join() {
        Ok(Ok(lines)) => {
            for line in lines {
                println!("{line}");
            }
            ExitCode::SUCCESS
        }
        Ok(Err(e)) => {
            eprintln!("nx: {e}");
            ExitCode::from(1)
        }
        Err(_) => {
            eprintln!("nx: interpreter crashed (stack overflow)");
            ExitCode::from(1)
        }
    }
}
