//! Toolchain discovery (clang, linker).

use std::process::Command;

#[derive(Debug, Clone)]
pub struct Toolchain {
    pub clang: String,
    pub linker_kind: String,
}

impl Toolchain {
    pub fn discover() -> Result<Self, String> {
        // find clang
        let clang = find_exe("clang").ok_or_else(|| "clang not found on PATH".to_string())?;
        // mold not available on windows; use clang's default linker
        let linker_kind = "system (clang default)".to_string();
        Ok(Self { clang, linker_kind })
    }
    pub fn explain(&self) -> String {
        format!("clang: {}\nlinker: {}", self.clang, self.linker_kind)
    }
}

fn find_exe(name: &str) -> Option<String> {
    // check PATH
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(';') {
            let cand = std::path::Path::new(dir).join(format!("{}.exe", name));
            if cand.exists() {
                return Some(cand.to_string_lossy().to_string());
            }
            let cand2 = std::path::Path::new(dir).join(name);
            if cand2.exists() {
                return Some(cand2.to_string_lossy().to_string());
            }
        }
    }
    None
}

#[derive(Debug, Clone, Default)]
pub struct BuildOpts {
    /// Clang optimization level (0-3).
    pub opt: u8,
    /// Enable thin LTO.
    pub lto: bool,
    /// Optional sysroot path passed to clang.
    pub sysroot: Option<String>,
    /// PGO: generate profile data.
    pub pgo_generate: bool,
    /// PGO: use profile data from this path.
    pub pgo_use: Option<String>,
}

pub fn run_clang(clang: &str, ll_path: &str, out_path: &str) -> Result<(), String> {
    run_clang_with(clang, ll_path, out_path, &BuildOpts::default())
}

pub fn run_clang_with(
    clang: &str,
    ll_path: &str,
    out_path: &str,
    opts: &BuildOpts,
) -> Result<(), String> {
    let mut cmd = Command::new(clang);
    cmd.arg("-Wno-override-module");
    let o = opts.opt.min(3);
    cmd.arg(format!("-O{}", o));
    if opts.lto {
        cmd.arg("-flto=thin");
        cmd.arg("-fuse-ld=lld");
    }
    if let Some(s) = &opts.sysroot {
        cmd.arg(format!("--sysroot={}", s));
    }
    if opts.pgo_generate {
        cmd.arg("-fprofile-generate");
    }
    if let Some(p) = &opts.pgo_use {
        cmd.arg(format!("-fprofile-use={}", p));
    }
    cmd.arg(ll_path).arg("-o").arg(out_path);
    let status = cmd.status().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("clang failed with {}", status))
    }
}

/// Parse and verify an LLVM IR module without producing a usable artifact.
/// Compiles the IR to a throwaway object file, which runs the LLVM IR parser,
/// verifier, and backend (the strongest check available without standalone
/// `opt`/`llvm-as` binaries).
pub fn verify_ir(clang: &str, ll_path: &str) -> Result<(), String> {
    let obj_path = format!("{}.verify.obj", ll_path);
    let output = Command::new(clang)
        .arg("-Wno-override-module")
        .arg("-x")
        .arg("ir")
        .arg("-c")
        .arg(ll_path)
        .arg("-o")
        .arg(&obj_path)
        .output()
        .map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&obj_path);
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("LLVM IR verification failed:\n{}", stderr.trim()))
    }
}
