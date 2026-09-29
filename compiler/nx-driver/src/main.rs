use std::process::ExitCode;

fn usage() -> String {
    "usage: nx <file.nx>\n       nx --lex <file.nx>\n       nx --parse <file.nx>\n       nx --run <file.nx>\n       nx --version\n       nx --license\n       nx update [--version <ver>]".to_string()
}

const UPDATE_REPO: &str = "Salaou-Hasan/nexum";

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
        return update_cmd(&args[2..]);
    }
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

fn normalize_tag(v: &str) -> String {
    let v = v.strip_prefix('v').unwrap_or(v);
    let v = v.strip_prefix("==").unwrap_or(v);
    format!("v{v}")
}

fn asset_name(tag: &str) -> String {
    if cfg!(windows) {
        format!("nx-{tag}-{}.exe", env!("NX_TARGET"))
    } else {
        format!("nx-{tag}-{}", env!("NX_TARGET"))
    }
}

fn download_url(tag: &str) -> String {
    format!(
        "https://github.com/{UPDATE_REPO}/releases/download/{tag}/{}",
        asset_name(tag)
    )
}

fn vsix_name(tag: &str) -> String {
    format!("nexum-{tag}.vsix")
}

fn vsix_url(tag: &str) -> String {
    format!(
        "https://github.com/{UPDATE_REPO}/releases/download/{tag}/{}",
        vsix_name(tag)
    )
}

fn latest_tag() -> Result<String, String> {
    // No HTTP deps: use the system curl (present on Win10+, macOS, most Linux).
    let out = std::process::Command::new("curl")
        .args([
            "-fsSL",
            "-H",
            "Accept: application/vnd.github+json",
            &format!("https://api.github.com/repos/{UPDATE_REPO}/releases/latest"),
        ])
        .output()
        .map_err(|e| format!("curl not found or failed: {e}"))?;
    if !out.status.success() {
        return Err("could not reach github api (network?)".to_string());
    }
    let body = String::from_utf8_lossy(&out.stdout);
    body.split("\"tag_name\"")
        .nth(1)
        .and_then(|s| s.split('"').nth(1))
        .map(|s| s.to_string())
        .ok_or("could not parse latest release".to_string())
}

fn update_cmd(rest: &[String]) -> ExitCode {
    // nx update | nx update <ver> | nx update --version <ver>
    let wanted: Option<String> = match rest {
        [] => None,
        [v] if v == "--version" => {
            eprintln!("nx update: --version needs a value, e.g. nx update --version 0.0.2");
            return ExitCode::from(2);
        }
        [v] => Some(normalize_tag(v)),
        [f, v] if f == "--version" => Some(normalize_tag(v)),
        _ => {
            eprintln!("usage: nx update [--version <ver>]");
            return ExitCode::from(2);
        }
    };
    let tag = match wanted {
        Some(t) => t,
        None => match latest_tag() {
            Ok(t) => {
                println!("nx: latest release is {t}");
                t
            }
            Err(e) => {
                eprintln!("nx update: {e}");
                return ExitCode::from(1);
            }
        },
    };
    if format!("v{}", env!("CARGO_PKG_VERSION")) == tag {
        println!("nx: already on {tag}");
        return ExitCode::SUCCESS;
    }
    let url = download_url(&tag);
    println!("nx: downloading {url}");
    let tmp = std::env::temp_dir().join(asset_name(&tag));
    let dl = std::process::Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&tmp)
        .arg(&url)
        .status();
    match dl {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!("nx update: download failed (no {tag} build for {}?)", env!("NX_TARGET"));
            return ExitCode::from(1);
        }
    }
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("nx update: cannot locate current exe: {e}");
            return ExitCode::from(1);
        }
    };
    // Windows can't overwrite a running exe, but it can rename it.
    let backup = exe.with_extension(format!("{}.bak", env!("CARGO_PKG_VERSION")));
    let _ = std::fs::remove_file(&backup);
    if let Err(e) = std::fs::rename(&exe, &backup) {
        eprintln!("nx update: cannot replace {}: {e}", exe.display());
        return ExitCode::from(1);
    }
    if let Err(e) = std::fs::rename(&tmp, &exe) {
        let _ = std::fs::rename(&backup, &exe);
        eprintln!("nx update: install failed: {e}");
        return ExitCode::from(1);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755));
    }
    println!("nx: updated to {tag} (previous kept at {})", backup.display());
    update_extension(&tag);
    println!("nx: restart your terminal to use it");
    ExitCode::SUCCESS
}

fn update_extension(tag: &str) {
    let url = vsix_url(tag);
    let tmp = std::env::temp_dir().join(vsix_name(tag));
    println!("nx: downloading {url}");
    let dl = std::process::Command::new("curl")
        .args(["-fsSL", "-o"])
        .arg(&tmp)
        .arg(&url)
        .status();
    if !matches!(dl, Ok(s) if s.success()) {
        eprintln!("nx update: extension download failed, skipping");
        return;
    }
    // `code` CLI may be absent (VS Code not installed / not on PATH).
    let probe = std::process::Command::new("code")
        .arg("--version")
        .output();
    if !matches!(probe, Ok(o) if o.status.success()) {
        eprintln!("nx update: `code` CLI not found, extension not updated");
        eprintln!("nx update: install it manually: code --install-extension {}", tmp.display());
        return;
    }
    match std::process::Command::new("code")
        .args(["--install-extension"])
        .arg(&tmp)
        .arg("--force")
        .status()
    {
        Ok(s) if s.success() => println!("nx: extension updated to {tag}"),
        _ => eprintln!("nx update: `code --install-extension` failed"),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tag_normalizes() {
        assert_eq!(super::normalize_tag("0.0.2"), "v0.0.2");
        assert_eq!(super::normalize_tag("v0.0.2"), "v0.0.2");
    }

    #[test]
    fn asset_matches_release_workflow() {
        let a = super::asset_name("v0.0.1");
        assert!(a.starts_with("nx-v0.0.1-"));
        assert!(a.ends_with(if cfg!(windows) { ".exe" } else { "" }) || !cfg!(windows));
    }

    #[test]
    fn url_shape() {
        let u = super::download_url("v0.0.2");
        assert!(u.contains("Salaou-Hasan/nexum/releases/download/v0.0.2/nx-v0.0.2-"));
    }

    #[test]
    fn vsix_url_shape() {
        let u = super::vsix_url("v0.0.2");
        assert!(u.ends_with("releases/download/v0.0.2/nexum-v0.0.2.vsix"));
    }
}
