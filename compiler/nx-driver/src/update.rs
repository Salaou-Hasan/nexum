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

pub(crate) fn update_cmd(rest: &[String]) -> ExitCode {
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
pub(crate) fn relaunch_elevated(rest: &[String]) -> ExitCode {
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

    #[cfg(windows)]
    #[test]
    fn ps_quote_escapes() {
        assert_eq!(super::ps_quote("update"), "'update'");
        assert_eq!(super::ps_quote("a'b"), "'a''b'");
    }
}
