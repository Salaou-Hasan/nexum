use std::process::ExitCode;

fn usage() -> String {
    "usage: nx <file.nx>\n       nx --lex <file.nx>\n       nx --parse <file.nx>\n       nx --run <file.nx>\n       nx check <file.nx>\n       nx build <file.nx> [-o <out>]\n       nx --version\n       nx --license\n       nx setup [--apply]\n       nx update [--version <ver>]".to_string()
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
    if args.len() >= 2 && args[1] == "setup" {
        return setup_cmd(&args[2..]);
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
    if args.len() == 3 && args[1] == "check" {
        return check_file(&args[2]);
    }
    if args.len() >= 3 && args[1] == "build" {
        return build_cmd(&args[2..]);
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

fn check_file(path: &str) -> ExitCode {
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

fn build_cmd(rest: &[String]) -> ExitCode {
    // nx build <file.nx> [-o <out>] [--run]
    let mut file: Option<String> = None;
    let mut out: Option<String> = None;
    let mut run = false;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "-o" => {
                i += 1;
                if i >= rest.len() {
                    eprintln!("usage: nx build <file.nx> [-o <out>] [--run]");
                    return ExitCode::from(2);
                }
                out = Some(rest[i].clone());
            }
            "--run" => run = true,
            f if file.is_none() => file = Some(f.to_string()),
            _ => {
                eprintln!("usage: nx build <file.nx> [-o <out>] [--run]");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }
    let file = match file {
        Some(f) => f,
        None => {
            eprintln!("usage: nx build <file.nx> [-o <out>] [--run]");
            return ExitCode::from(2);
        }
    };
    let source = match read_source(&file) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let base = std::path::Path::new(&file)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(".".into());
    // Types are mandatory for codegen.
    if let Err(es) = nx_types::check_source(&source, &base) {
        for e in es {
            eprintln!("nx: {e}");
        }
        return ExitCode::from(1);
    }
    let ir = match nx_codegen::compile_entry(&source, &base) {
        Ok(ir) => ir,
        Err(e) => {
            eprintln!("nx: {e}");
            return ExitCode::from(1);
        }
    };
    let ll = std::env::temp_dir().join(format!("nxbuild-{}.ll", std::process::id()));
    if let Err(e) = std::fs::write(&ll, ir) {
        eprintln!("nx: cannot write IR: {e}");
        return ExitCode::from(1);
    }
    let out = out.unwrap_or_else(|| default_exe_name(&file));
    // Probe clang first for a helpful error.
    if std::process::Command::new("clang").arg("--version").output().is_err() {
        eprintln!("nx: clang not found — install LLVM: winget install LLVM.LLVM");
        return ExitCode::from(1);
    }
    let mut cmd = std::process::Command::new("clang");
    cmd.args(["-O2"]).arg(&ll).args(["-o"]).arg(&out);
    // libm for log10/floor/pow/round on unix; MSVC links it implicitly.
    if cfg!(unix) {
        cmd.arg("-lm");
    }
    match cmd.status() {
        Ok(s) if s.success() => {
            println!("nx: built {out}");
        }
        _ => {
            eprintln!("nx: clang failed");
            return ExitCode::from(1);
        }
    }
    if run {
        match std::process::Command::new(&out).status() {
            Ok(s) => {
                return match s.code() {
                    Some(c) => ExitCode::from(c as u8),
                    None => ExitCode::FAILURE,
                };
            }
            Err(e) => {
                eprintln!("nx: cannot run {out}: {e}");
                return ExitCode::from(1);
            }
        }
    }
    ExitCode::SUCCESS
}

fn default_exe_name(file: &str) -> String {
    let stem = std::path::Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or("a".to_string());
    let dir = std::path::Path::new(file)
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or(".".into());
    let name = if cfg!(windows) { format!("{stem}.exe") } else { stem };
    dir.join(name).to_string_lossy().to_string()
}

fn run_file(path: &str) -> ExitCode {
    let source = match read_source(path) {
        Ok(s) => s,
        Err(c) => return c,
    };
    let path = path.to_string();
    // Run the interpreter on a thread with a large stack so deep (but
    // bounded) Nexum recursion hits our CALL_LIMIT error instead of
    // overflowing the small default Windows main-thread stack.
    let child = std::thread::Builder::new()
        .name("nx-run".to_string())
        .stack_size(64 * 1024 * 1024)
        .spawn(move || -> Result<Vec<String>, String> {
            let tokens = nx_lexer::lex(&source).map_err(|e| e.to_string())?;
            let prog = nx_parser::parse(tokens).map_err(|e| e.to_string())?;
            let base = std::path::Path::new(&path)
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or(".".into());
            nx_interp::run_with_base(&prog, &base).map_err(|e| e.to_string())
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
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            #[cfg(windows)]
            {
                eprintln!("nx update: administrator rights needed, requesting elevation...");
                return relaunch_elevated(rest);
            }
            #[cfg(unix)]
            eprintln!("nx update: permission denied, re-run with sudo: sudo nx update");
        }
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

#[cfg(windows)]
fn ps_quote(a: &str) -> String {
    format!("'{}'", a.replace('\'', "''"))
}

#[cfg(windows)]
fn relaunch_elevated(rest: &[String]) -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("nx update: cannot locate current exe: {e}");
            return ExitCode::from(1);
        }
    };
    // Re-run the same update command elevated via UAC prompt.
    let mut args = vec![ps_quote("update")];
    args.extend(rest.iter().map(|a| ps_quote(a)));
    let script = format!(
        "Start-Process -FilePath '{}' -ArgumentList {} -Verb RunAs -Wait",
        exe.display().to_string().replace('\'', "''"),
        args.join(",")
    );
    match std::process::Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .status()
    {
        Ok(s) if s.success() => {
            println!("nx: elevated update finished");
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("nx update: elevation failed or was declined");
            ExitCode::from(1)
        }
    }
}

fn find_code() -> Option<std::path::PathBuf> {
    // 1. Whatever is on PATH.
    if matches!(
        std::process::Command::new("code").arg("--version").output(),
        Ok(o) if o.status.success()
    ) {
        return Some("code".into());
    }
    // 2. Well-known install locations (per-user and per-machine).
    // Elevated shells often lack the per-user PATH entry, so probe directly.
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        candidates.push(format!("{local}\\Programs\\Microsoft VS Code\\bin\\code.cmd").into());
    }
    if let Ok(pf) = std::env::var("ProgramFiles") {
        candidates.push(format!("{pf}\\Microsoft VS Code\\bin\\code.cmd").into());
    }
    if let Ok(pfx) = std::env::var("ProgramFiles(x86)") {
        candidates.push(format!("{pfx}\\Microsoft VS Code\\bin\\code.cmd").into());
    }
    candidates.into_iter().find(|p| {
        matches!(
            std::process::Command::new(p).arg("--version").output(),
            Ok(o) if o.status.success()
        )
    })
}

const NX_SVG: &str = include_str!("../../../editors/vscode-nexum/icons/file_type_nx.svg");

fn vscode_extensions_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE").ok().map(std::path::PathBuf::from)?;
    #[cfg(not(windows))]
    let home = std::env::var("HOME").ok().map(std::path::PathBuf::from)?;
    Some(home.join(".vscode/extensions"))
}

fn vscode_settings_path() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let base = std::env::var("APPDATA").ok().map(std::path::PathBuf::from)?;
    #[cfg(target_os = "macos")]
    let base = std::env::var("HOME").ok().map(|h| {
        std::path::PathBuf::from(h).join("Library/Application Support")
    })?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var("HOME").ok().map(|h| {
        std::path::PathBuf::from(h).join(".config")
    })?;
    Some(base.join("Code/User/settings.json"))
}

fn nx_data_icons() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let base = std::env::var("APPDATA").ok().map(std::path::PathBuf::from)?;
    #[cfg(target_os = "macos")]
    let base = std::env::var("HOME").ok().map(|h| {
        std::path::PathBuf::from(h).join("Library/Application Support/Nexum")
    })?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var("HOME").ok().map(|h| {
        std::path::PathBuf::from(h).join(".local/share/Nexum")
    })?;
    #[cfg(windows)]
    return Some(base.join("Nexum/icons"));
    #[cfg(not(windows))]
    return Some(base.join("icons"));
}

fn settings_theme(text: &str) -> Option<String> {
    text.split("\"workbench.iconTheme\"")
        .nth(1)?
        .split(':')
        .nth(1)?
        .split('"')
        .nth(1)
        .map(|s| s.to_string())
}

/// Insert `"key": value` before the root closing brace. Returns (text, changed).
fn json_insert_key(text: &str, key: &str, value: &str) -> (String, bool) {
    if text.contains(&format!("\"{key}\"")) {
        return (text.to_string(), false);
    }
    let close = match text.rfind('}') {
        Some(i) => i,
        None => return (text.to_string(), false),
    };
    let before: String = text[..close].chars().rev().take_while(|c| c.is_whitespace()).collect();
    let trimmed = text[..close].trim_end();
    // Empty object -> no comma; existing trailing comma -> don't double it.
    let need_comma = !trimmed.ends_with('{') && !trimmed.ends_with(',');
    let mut out = String::with_capacity(text.len() + key.len() + value.len() + 8);
    out.push_str(trimmed);
    if need_comma {
        out.push(',');
    }
    out.push_str(&format!("\n    \"{key}\": {value}"));
    out.push_str(&before);
    out.push('}');
    out.push_str(&text[close + 1..]);
    (out, true)
}

/// Set `inner` to `value_json` inside the top-level object `top`.
/// Creates the object if missing. Handles JSONC comments. Returns (text, changed).
fn json_object_set(text: &str, top: &str, inner: &str, value_json: &str) -> (String, bool) {
    let key = format!("\"{top}\"");
    let kpos = match text.find(&key) {
        Some(p) => p,
        None => {
            let (t, _) = json_insert_key(text, top, &format!("{{\"{inner}\": {value_json}}}"));
            return (t, true);
        }
    };
    // Find object start after the key.
    let bytes = text.as_bytes();
    let mut i = kpos + key.len();
    let n = bytes.len();
    // skip whitespace/comments/colon
    let mut colon = false;
    let mut obj_start = None;
    while i < n {
        let c = bytes[i] as char;
        if c == ':' && !colon {
            colon = true;
            i += 1;
            continue;
        }
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '/' && i + 1 < n && bytes[i + 1] as char == '/' {
            while i < n && bytes[i] as char != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '{' && colon {
            obj_start = Some(i);
            break;
        }
        break;
    }
    let obj_start = match obj_start {
        Some(p) => p,
        None => return (text.to_string(), false),
    };
    let obj_end = match match_brace(text, obj_start) {
        Some(p) => p,
        None => return (text.to_string(), false),
    };
    let inner_key = format!("\"{inner}\"");
    let body = &text[obj_start..=obj_end];
    if let Some(rel) = body.find(&inner_key) {
        // Replace existing value.
        let abs = obj_start + rel + inner_key.len();
        let mut j = abs;
        while j < n && (bytes[j] as char).is_whitespace() {
            j += 1;
        }
        if j >= n || bytes[j] as char != ':' {
            return (text.to_string(), false);
        }
        j += 1;
        while j < n && (bytes[j] as char).is_whitespace() {
            j += 1;
        }
        let vend = match value_end(text, j) {
            Some(p) => p,
            None => return (text.to_string(), false),
        };
        if text[j..vend].trim() == value_json {
            return (text.to_string(), false);
        }
        let mut out = String::new();
        out.push_str(&text[..j]);
        out.push_str(value_json);
        out.push_str(&text[vend..]);
        return (out, true);
    }
    // Insert new entry before the closing brace.
    let mut out = String::new();
    out.push_str(text[..obj_end].trim_end());
    if !text[obj_start + 1..obj_end].trim().is_empty()
        && !text[..obj_end].trim_end().ends_with(',')
    {
        out.push(',');
    }
    out.push_str(&format!("\n        \"{inner}\": {value_json}"));
    out.push_str(&text[obj_end..]);
    (out, true)
}

/// Find the matching close brace/bracket from an opener, JSONC-aware.
fn match_brace(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let n = bytes.len();
    let (mut depth, opener, closer) = (0i32, bytes[open] as char, match bytes[open] as char {
        '{' => '}',
        '[' => ']',
        _ => return None,
    });
    let _ = opener;
    let mut i = open;
    let mut in_str = false;
    let mut esc = false;
    while i < n {
        let c = bytes[i] as char;
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
        } else if c == '/' && i + 1 < n && bytes[i + 1] as char == '/' {
            while i < n && bytes[i] as char != '\n' {
                i += 1;
            }
        } else if c == '/' && i + 1 < n && bytes[i + 1] as char == '*' {
            i += 2;
            while i + 1 < n && !(bytes[i] as char == '*' && bytes[i + 1] as char == '/') {
                i += 1;
            }
            i += 1;
        } else if c == '{' || c == '[' {
            depth += 1;
        } else if c == '}' || c == ']' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        let _ = closer;
        i += 1;
    }
    None
}

/// End (exclusive) of a JSON value starting at `start`.
fn value_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let n = bytes.len();
    if start >= n {
        return None;
    }
    match bytes[start] as char {
        '"' => {
            let mut i = start + 1;
            while i < n {
                let c = bytes[i] as char;
                if c == '\\' {
                    i += 2;
                    continue;
                }
                if c == '"' {
                    return Some(i + 1);
                }
                i += 1;
            }
            None
        }
        '{' | '[' => match_brace(text, start).map(|p| p + 1),
        _ => {
            let mut i = start;
            while i < n && !matches!(bytes[i] as char, ',' | '}' | ']') {
                i += 1;
            }
            Some(i)
        }
    }
}

fn setup_cmd(rest: &[String]) -> ExitCode {
    let apply = rest.iter().any(|a| a == "--apply");
    let quiet = rest.iter().any(|a| a == "--quiet");
    let log = |msg: &str| {
        if !quiet {
            println!("{msg}");
        }
    };

    let settings = match vscode_settings_path() {
        Some(p) => p,
        None => {
            eprintln!("nx setup: cannot locate VS Code settings dir on this platform");
            return ExitCode::from(1);
        }
    };
    let text = match std::fs::read_to_string(&settings) {
        Ok(t) => t,
        Err(_) => {
            log("nx setup: no VS Code settings.json found (is VS Code installed?)");
            log("nx setup: open VS Code once, then re-run: nx setup --apply");
            return ExitCode::SUCCESS;
        }
    };
    let theme = settings_theme(&text).unwrap_or_default();
    log(&format!("nx setup: icon theme is '{}'", if theme.is_empty() { "(default)" } else { &theme }));

    if theme.contains("vsicons") {
        // vscode-icons: point it at our N icon, add the .nx mapping.
        let icons = match nx_data_icons() {
            Some(d) => d,
            None => {
                eprintln!("nx setup: cannot locate app data dir");
                return ExitCode::from(1);
            }
        };
        if let Err(e) = std::fs::create_dir_all(&icons)
            .and_then(|_| std::fs::write(icons.join("file_type_nx.svg"), NX_SVG))
        {
            eprintln!("nx setup: cannot write icon pack: {e}");
            return ExitCode::from(1);
        }
        let folder = icons.to_string_lossy().replace('\\', "\\\\");
        let assoc = r#"[{"icon": "nx", "extensions": ["nx"], "format": "svg"}]"#;
        if !apply {
            println!("nx setup: would add to {}", settings.display());
            println!("  \"vsicons.customIconFolderPath\": \"{folder}\"");
            println!("  \"vsicons.associations.files\": {assoc}");
            println!("nx setup: re-run with --apply to write it (a .bak backup is kept)");
            return ExitCode::SUCCESS;
        }
        let (t2, _) = json_insert_key(&text, "vsicons.customIconFolderPath", &format!("\"{folder}\""));
        // Merge the nx entry into the associations array when present.
        let mut t3 = t2.clone();
        if t3.contains("\"vsicons.associations.files\"") {
            if !t3.contains("\"nx\"") {
                let entry = r#"{"icon": "nx", "extensions": ["nx"], "format": "svg"}"#;
                let key_pos = t3.find("\"vsicons.associations.files\"").unwrap();
                let open = t3[key_pos..].find('[').map(|b| key_pos + b).unwrap();
                let rest = &t3[open + 1..];
                let is_empty = rest.trim_start().starts_with(']');
                if is_empty {
                    t3.insert_str(open + 1, entry);
                } else {
                    t3.insert_str(open + 1, &format!("{entry},"));
                }
            }
        } else {
            let (t, _) = json_insert_key(&t3, "vsicons.associations.files", assoc);
            t3 = t;
        }
        if t3 == text {
            log("nx setup: already configured");
            return ExitCode::SUCCESS;
        }
        let bak = settings.with_extension("json.bak");
        let _ = std::fs::copy(&settings, &bak);
        match std::fs::write(&settings, t3) {
            Ok(_) => {
                log(&format!("nx setup: updated {} (backup at {})", settings.display(), bak.display()));
                log("nx setup: reload VS Code window to see the N icon");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("nx setup: cannot write settings: {e}");
                ExitCode::from(1)
            }
        }
    } else if theme.contains("material-icon") {
        // Material Icon Theme supports custom SVGs referenced by path
        // (relative to the theme's dist folder, no .svg suffix).
        // Restriction: the folder must live inside .vscode/extensions.
        let ext_dir = match vscode_extensions_dir() {
            Some(d) => d,
            None => {
                eprintln!("nx setup: cannot locate VS Code extensions dir");
                return ExitCode::from(1);
            }
        };
        let icons = ext_dir.join("icons");
        if let Err(e) = std::fs::create_dir_all(&icons)
            .and_then(|_| std::fs::write(icons.join("nx.svg"), NX_SVG))
        {
            eprintln!("nx setup: cannot write icon: {e}");
            return ExitCode::from(1);
        }
        const WANT: &str = "\"../../icons/nx\"";
        if !apply {
            println!("nx setup: icon written to {}", icons.join("nx.svg").display());
            println!("nx setup: would set \"material-icon-theme.files.associations\": {{ \"*.nx\": {WANT} }}");
            println!("nx setup: re-run with --apply to write it (a .bak backup is kept)");
            return ExitCode::SUCCESS;
        }
        let (t2, _) = json_object_set(
            &text,
            "material-icon-theme.files.associations",
            "*.nx",
            WANT,
        );
        if t2 == text {
            log("nx setup: already configured");
            return ExitCode::SUCCESS;
        }
        let bak = settings.with_extension("json.bak");
        let _ = std::fs::copy(&settings, &bak);
        match std::fs::write(&settings, t2) {
            Ok(_) => {
                log("nx setup: updated with the original N icon, reload VS Code window");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("nx setup: cannot write settings: {e}");
                ExitCode::from(1)
            }
        }
    } else {
        // Policy: only vscode-icons and Material Icon Theme accept custom
        // SVGs (verified; Catppuccin explicitly refuses, the rest have no
        // API). Never borrow another pack's icon — leave the theme alone.
        log(&format!("nx setup: '{theme}' has no custom-icon support, leaving it untouched"));
        log("nx setup: to see the N logo, select the 'Nexum Icons' file icon theme");
        log("nx setup: Preferences -> File Icon Theme -> Nexum Icons (ships in the vsix)");
        ExitCode::SUCCESS
    }
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
    // `code` CLI may be absent from PATH (VS Code not installed, or an
    // elevated shell missing the per-user entry) — probe known spots.
    let code = match find_code() {
        Some(c) => c,
        None => {
            eprintln!("nx update: `code` CLI not found, extension not updated");
            eprintln!("nx update: install it manually: code --install-extension {}", tmp.display());
            return;
        }
    };
    match std::process::Command::new(&code)
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

    #[cfg(windows)]
    #[test]
    fn ps_quote_escapes() {
        assert_eq!(super::ps_quote("update"), "'update'");
        assert_eq!(super::ps_quote("a'b"), "'a''b'");
    }

    #[test]
    fn json_object_set_inserts_and_replaces() {
        let (t, c) = super::json_object_set("{\n    \"a\": 1\n}", "material-icon-theme.files.associations", "*.nx", "\"../../icons/nx\"");
        assert!(c);
        assert!(t.contains("\"*.nx\": \"../../icons/nx\""));
        // Idempotent.
        let (t2, c2) = super::json_object_set(&t, "material-icon-theme.files.associations", "*.nx", "\"../../icons/nx\"");
        assert!(!c2);
        assert_eq!(t, t2);
        // Replace python mapping with the custom icon.
        let (t3, c3) = super::json_object_set("{\"material-icon-theme.files.associations\": {\"*.nx\": \"python\"}}", "material-icon-theme.files.associations", "*.nx", "\"../../icons/nx\"");
        assert!(c3);
        assert!(t3.contains("\"*.nx\": \"../../icons/nx\""));
        assert!(!t3.contains("python"));
    }
}
