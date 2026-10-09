//! Self-update command for the nx CLI.
//!
//! Resolves the wanted release, downloads the matching asset, swaps the
//! executable in place, and refreshes the VS Code extension.

use std::process::ExitCode;

const UPDATE_REPO: &str = "Salaou-Hasan/nexum";

pub(crate) fn normalize_tag(v: &str) -> String {
    let v = v.strip_prefix('v').unwrap_or(v);
    let v = v.strip_prefix("==").unwrap_or(v);
    format!("v{v}")
}

pub(crate) fn asset_name(tag: &str) -> String {
    if cfg!(windows) {
        format!("nx-{tag}-{}.exe", env!("NX_TARGET"))
    } else {
        format!("nx-{tag}-{}", env!("NX_TARGET"))
    }
}

pub(crate) fn download_url(tag: &str) -> String {
    format!(
        "https://github.com/{UPDATE_REPO}/releases/download/{tag}/{}",
        asset_name(tag)
    )
}

pub(crate) fn vsix_name(tag: &str) -> String {
    format!("nexum-{tag}.vsix")
}

pub(crate) fn vsix_url(tag: &str) -> String {
    format!(
        "https://github.com/{UPDATE_REPO}/releases/download/{tag}/{}",
        vsix_name(tag)
    )
}

pub(crate) fn latest_tag() -> Result<String, String> {
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

/// Split the internal resume flag off the argument list. `relaunch_elevated`
/// re-invokes `nx update --from <tmp> <original args>` so the elevated child
/// reuses the already-downloaded file instead of fetching the same bytes a
/// second time; anything else parses as usual. A lone `--from` with no value
/// is not a pair and passes through to fail as a bogus version downstream.
fn take_from_flag(rest: &[String]) -> (Option<String>, Vec<String>) {
    if rest.len() >= 2 && rest[0] == "--from" {
        (Some(rest[1].clone()), rest[2..].to_vec())
    } else {
        (None, rest.to_vec())
    }
}

/// Whether the `--from` candidate may be installed as-is: the exact file
/// this update already downloaded (same file name as the wanted asset),
/// present and non-empty. Anything else falls back to a fresh download
/// rather than installing the wrong bytes.
fn reusable_source(expected: &std::path::Path, candidate: &str) -> bool {
    let p = std::path::Path::new(candidate);
    p.file_name() == expected.file_name() && matches!(std::fs::metadata(p), Ok(m) if m.len() > 0)
}

/// Fetch `url` into `dest`. `--progress-bar` instead of `-s`: on a slow
/// link the transfer must read as slow, not stuck.
fn download_file(url: &str, dest: &std::path::Path) -> bool {
    matches!(
        std::process::Command::new("curl")
            .args(["-fSL", "--progress-bar", "-o"])
            .arg(dest)
            .arg(url)
            .status(),
        Ok(s) if s.success()
    )
}

pub(crate) fn update_cmd(rest: &[String]) -> ExitCode {
    // nx update | nx update <ver> | nx update --version <ver>
    // Plus the internal resume flag below, which is never shown in usage.
    let (from_flag, args) = take_from_flag(rest);
    let wanted: Option<String> = match args.as_slice() {
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
    let tmp = std::env::temp_dir().join(asset_name(&tag));
    if from_flag
        .as_deref()
        .is_some_and(|p| reusable_source(&tmp, p))
    {
        println!("nx: using already-downloaded {}", tmp.display());
    } else {
        println!("nx: downloading {url}");
        if !download_file(&url, &tmp) {
            eprintln!(
                "nx update: download failed (no {tag} build for {}?)",
                env!("NX_TARGET")
            );
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
                let code = relaunch_elevated(&args, &tmp);
                // The elevated child moves tmp into place on success, so a
                // declined or failed elevation is the only path that leaves
                // the download behind.
                let _ = std::fs::remove_file(&tmp);
                return code;
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
    println!(
        "nx: updated to {tag} (previous kept at {})",
        backup.display()
    );
    update_extension(&tag);
    println!("nx: restart your terminal to use it");
    ExitCode::SUCCESS
}

#[cfg(windows)]
pub(crate) fn ps_quote(a: &str) -> String {
    format!("'{}'", a.replace('\'', "''"))
}

#[cfg(windows)]
pub(crate) fn relaunch_elevated(rest: &[String], from: &std::path::Path) -> ExitCode {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("nx update: cannot locate current exe: {e}");
            return ExitCode::from(1);
        }
    };
    // Re-run the same update command elevated via UAC prompt. `--from`
    // carries the already-downloaded file so the child does not fetch the
    // same bytes a second time over a slow link.
    let mut args = vec![ps_quote("update")];
    args.push(ps_quote("--from"));
    args.push(ps_quote(&from.display().to_string()));
    args.extend(rest.iter().map(|a| ps_quote(a)));
    let script = format!(
        "Start-Process -FilePath '{}' -ArgumentList {} -Verb RunAs -Wait",
        exe.display().to_string().replace('\'', "''"),
        args.join(",")
    );
    match std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            &script,
        ])
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

pub(crate) fn find_code() -> Option<std::path::PathBuf> {
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

pub(crate) fn update_extension(tag: &str) {
    let url = vsix_url(tag);
    let tmp = std::env::temp_dir().join(vsix_name(tag));
    println!("nx: downloading {url}");
    if !download_file(&url, &tmp) {
        eprintln!("nx update: extension download failed, skipping");
        return;
    }
    // `code` CLI may be absent from PATH (VS Code not installed, or an
    // elevated shell missing the per-user entry) — probe known spots.
    let code = match find_code() {
        Some(c) => c,
        None => {
            eprintln!("nx update: `code` CLI not found, extension not updated");
            eprintln!(
                "nx update: install it manually: code --install-extension {}",
                tmp.display()
            );
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

    #[test]
    fn from_flag_splits_off() {
        let (f, a) = super::take_from_flag(&[
            "--from".to_string(),
            "C:\\t\\nx.exe".to_string(),
            "0.0.2".to_string(),
        ]);
        assert_eq!(f, Some("C:\\t\\nx.exe".to_string()));
        assert_eq!(a, vec!["0.0.2".to_string()]);
    }

    #[test]
    fn from_flag_absent_passes_through() {
        let (f, a) = super::take_from_flag(&["0.0.2".to_string()]);
        assert_eq!(f, None);
        assert_eq!(a, vec!["0.0.2".to_string()]);
        let (f, a) = super::take_from_flag(&[]);
        assert_eq!(f, None);
        assert!(a.is_empty());
    }

    #[test]
    fn from_flag_needs_a_value() {
        // A lone `--from` is not a pair; it passes through to fail as a
        // bogus version downstream, never as a reused path.
        let (f, a) = super::take_from_flag(&["--from".to_string()]);
        assert_eq!(f, None);
        assert_eq!(a, vec!["--from".to_string()]);
    }

    #[test]
    fn reusable_source_accepts_matching_nonempty_file() {
        let p = std::env::temp_dir().join("nx-v9.9.9-reuse-ok.exe");
        std::fs::write(&p, b"x").unwrap();
        let s = p.to_string_lossy().into_owned();
        assert!(super::reusable_source(&p, &s));
        std::fs::remove_file(&p).unwrap();
    }

    #[test]
    fn reusable_source_rejects_wrong_name_empty_and_missing() {
        let dir = std::env::temp_dir();
        let expected = dir.join("nx-v9.9.9-reuse-no.exe");
        let other = dir.join("something-else.exe");
        std::fs::write(&other, b"x").unwrap();
        let s = other.to_string_lossy().into_owned();
        assert!(!super::reusable_source(&expected, &s));
        let empty = dir.join("nx-v9.9.9-reuse-empty.exe");
        std::fs::write(&empty, b"").unwrap();
        let s = empty.to_string_lossy().into_owned();
        assert!(!super::reusable_source(&empty, &s));
        let missing = dir.join("nx-v9.9.9-reuse-gone.exe");
        let s = missing.to_string_lossy().into_owned();
        assert!(!super::reusable_source(&missing, &s));
        let _ = std::fs::remove_file(&other);
        let _ = std::fs::remove_file(&empty);
    }

    #[cfg(windows)]
    #[test]
    fn ps_quote_escapes() {
        assert_eq!(super::ps_quote("update"), "'update'");
        assert_eq!(super::ps_quote("a'b"), "'a''b'");
    }
}
