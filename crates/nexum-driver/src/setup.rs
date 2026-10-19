//! The interactive setup: `nx setup`.
//!
//! It is the one place that installs everything Nexum needs, and it asks
//! before it does anything: the LLVM toolchain, the compiler with the file
//! icon and the file association, and the editor extension.

use nexum_toolchain::{clang_install_hint, clang_installed, find_program};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const REPO: &str = "Salaou-Hasan/nexum";

/// Ask a question. The answer defaults to yes, so pressing enter goes on.
fn ask(question: &str) -> bool {
    print!("  {question} [Y/n] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    let answer = answer.trim().to_ascii_lowercase();
    answer.is_empty() || answer == "y" || answer == "yes"
}

fn step(what: &str) {
    println!();
    println!("[*] {what}");
}

fn note(message: &str) {
    println!("    {message}");
}

/// Run a program, showing what it prints.
fn run(program: &str, args: &[&str]) -> bool {
    match Command::new(program).args(args).status() {
        Ok(status) => status.success(),
        Err(_) => false,
    }
}

/// The latest release, as `major.minor.patch`.
fn latest_release() -> Result<(String, String), String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let out = Command::new("curl")
        .args(["-sL", "-f", &url])
        .output()
        .map_err(|e| format!("could not reach GitHub: {e}"))?;
    let body = String::from_utf8_lossy(&out.stdout).to_string();
    let tag = body
        .split("\"tag_name\"")
        .nth(1)
        .and_then(|s| s.split('"').nth(1))
        .unwrap_or("")
        .to_string();
    if tag.is_empty() {
        return Err("could not determine the latest release".into());
    }
    let version = tag.trim_start_matches('v').to_string();
    Ok((tag, version))
}

/// Download a release asset.
fn download(tag: &str, asset: &str) -> Result<PathBuf, String> {
    let url = format!("https://github.com/{REPO}/releases/download/{tag}/{asset}");
    let path = std::env::temp_dir().join(asset);
    note(&format!("downloading {url}"));
    let ok = Command::new("curl")
        .args(["-sL", "-f", "-o", path.to_str().unwrap_or_default(), &url])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if ok && path.exists() {
        Ok(path)
    } else {
        let _ = std::fs::remove_file(&path);
        Err(format!("could not download {url}"))
    }
}

/// The name of the portable binary for this platform.
fn host_asset() -> &'static str {
    if cfg!(target_os = "windows") {
        "nx-windows-x86_64.exe"
    } else if cfg!(target_os = "macos") {
        if cfg!(target_arch = "aarch64") {
            "nx-macos-aarch64"
        } else {
            "nx-macos-x86_64"
        }
    } else if cfg!(target_arch = "aarch64") {
        "nx-linux-aarch64"
    } else {
        "nx-linux-x86_64"
    }
}

/// Install the LLVM toolchain.
fn install_llvm() {
    if cfg!(target_os = "windows") {
        if find_program("winget").is_some() {
            note("installing LLVM with winget ...");
            run(
                "winget",
                &[
                    "install",
                    "--id",
                    "LLVM.LLVM",
                    "--accept-package-agreements",
                    "--accept-source-agreements",
                ],
            );
        } else {
            note("winget is not available; install LLVM from https://llvm.org");
        }
    } else if cfg!(target_os = "macos") {
        if find_program("brew").is_some() {
            note("installing LLVM with brew ...");
            run("brew", &["install", "llvm"]);
        } else {
            note("homebrew is not available; install LLVM from https://llvm.org");
        }
    } else if find_program("sudo").is_some() {
        note("installing clang with apt ...");
        run("sudo", &["apt", "install", "-y", "clang"]);
    } else {
        note("install clang with: sudo apt install clang");
    }
}

/// Install the compiler, the icon and the file association.
fn install_compiler(tag: &str, version: &str) -> Result<(), String> {
    if cfg!(target_os = "windows") {
        let asset = format!("Nexum-{version}-x64.msi");
        let msi = download(tag, &asset)?;
        note("running the installer ...");
        let ok = Command::new("msiexec.exe")
            .args([
                "/i",
                msi.to_str().unwrap_or_default(),
                "/quiet",
                "/norestart",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return Err("the installer failed".into());
        }
    } else if cfg!(target_os = "linux") {
        let asset = format!("nexum-{version}-amd64.deb");
        let deb = download(tag, &asset)?;
        note("installing the package (it asks for administrator rights) ...");
        let ok = if find_program("sudo").is_some() {
            run("sudo", &["dpkg", "-i", deb.to_str().unwrap_or_default()])
        } else {
            run("dpkg", &["-i", deb.to_str().unwrap_or_default()])
        };
        if !ok {
            return Err("the package could not be installed".into());
        }
    } else {
        // macOS: the compiler goes into ~/.local/bin.
        let asset = host_asset();
        let binary = download(tag, asset)?;
        let dir = PathBuf::from(std::env::var("HOME").unwrap_or_default())
            .join(".local")
            .join("bin");
        let _ = std::fs::create_dir_all(&dir);
        let target = dir.join("nx");
        std::fs::copy(&binary, &target).map_err(|e| e.to_string())?;
        make_executable(&target);
        note(&format!("installed {target:?}"));
        note("make sure that directory is on your PATH");
    }
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) {}

/// Install the editor extension.
fn install_extension(tag: &str) -> bool {
    use std::process::Stdio;
    let code = match find_program("code") {
        Some(c) => c,
        None => {
            note("Visual Studio Code was not found; the extension was not installed");
            return false;
        }
    };
    let vsix = match download(tag, "nexum-language.vsix") {
        Ok(p) => p,
        Err(e) => {
            note(&e);
            return false;
        }
    };
    note("installing the extension ...");
    // The extension installer prints its own progress and warnings, which
    // only get in the way here.
    let ok = if cfg!(target_os = "windows") {
        Command::new("cmd.exe")
            .args([
                "/c",
                &code,
                "--install-extension",
                vsix.to_str().unwrap_or_default(),
                "--force",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    } else {
        Command::new(&code)
            .args([
                "--install-extension",
                vsix.to_str().unwrap_or_default(),
                "--force",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    if ok {
        note("the extension is installed");
    } else {
        note("the extension could not be installed; run `code --install-extension` by hand");
    }
    ok
}

/// The interactive setup.
pub fn setup() {
    println!("Nexum setup");
    println!("===========");
    println!();
    println!("This installs everything Nexum needs:");
    println!("  * the compiler, on your PATH");
    println!("  * the file icon for .nx files, and the file association");
    println!("  * the Visual Studio Code extension");
    println!("  * the LLVM toolchain, when it is missing");
    println!();

    // The LLVM toolchain: needed to build anything.
    step("checking the LLVM toolchain");
    if clang_installed() {
        note("the LLVM toolchain is installed");
    } else {
        note("clang was not found. Nexum builds programs with it.");
        if ask("install the LLVM toolchain now?") {
            install_llvm();
            if clang_installed() {
                note("the LLVM toolchain is installed");
            } else {
                note("clang is still not on the PATH; open a new shell afterwards");
            }
        } else {
            note("install it later with: nx doctor");
        }
    }
    println!();

    // The editor extension.
    step("checking the editor extension");
    if find_program("code").is_some() {
        note("Visual Studio Code is installed");
    } else {
        note("Visual Studio Code was not found.");
        note("the extension can be installed later with:");
        note("  code --install-extension nexum-language.vsix");
    }
    println!();

    // The compiler, the icon and the file association.
    step("installing Nexum");
    match latest_release() {
        Ok((tag, version)) => {
            note(&format!("the latest release is {tag}"));
            if ask("install it now?") {
                match install_compiler(&tag, &version) {
                    Ok(()) => note("Nexum is installed"),
                    Err(e) => {
                        note(&e);
                        note("you can also download it from the releases page");
                    }
                }
                // The installer puts the extension in place, but the portable
                // install does not, so it is done here as well.
                if find_program("code").is_some() {
                    install_extension(&tag);
                }
            } else {
                note("you can install it later with: nx update");
            }
        }
        Err(e) => {
            note(&e);
            note("you can also download it from the releases page");
        }
    }
    println!();

    // Check what is in place now.
    step("checking the installation");
    if clang_installed() {
        note("the LLVM toolchain is installed");
    } else {
        note("the LLVM toolchain is still missing: install it with");
        note(&format!("  {}", clang_install_hint()));
    }
    println!();
    println!("Open a new shell so that the PATH is picked up, then run:");
    println!("  nx doctor");
    println!();
}
