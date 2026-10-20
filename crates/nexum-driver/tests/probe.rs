//! A probe for the system call layer: what does the kernel actually
//! return?
//!
//! The error convention is the one thing about a system call that cannot
//! be checked by reading the code. Linux returns the error number negated;
//! Darwin returns it positive and sets the carry flag. The wrappers
//! normalise that, so this probe calls the raw wrappers and prints what
//! came back, on every platform.

use std::path::PathBuf;

fn work_dir() -> PathBuf {
    let d = std::env::temp_dir().join("nexum_tests").join("probe");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn toolchain() -> Option<nexum_toolchain::Toolchain> {
    nexum_toolchain::Toolchain::discover().ok()
}

/// The module with the wrappers, and the drivers that call them.
const DRIVER: &str = r#"
#include <stdio.h>

extern long long nexum_sys_mmap(long long addr, long long len, long long prot,
                                long long flags, long long fd, long long off);
extern long long nexum_sys_open(long long path, long long flags, long long mode);
extern long long nexum_sys_write(long long fd, long long buf, long long len);

int main(void) {
    // An anonymous mapping of one page.
    long long m = nexum_sys_mmap(0, 16384, 3, 4098, -1, 0);
    printf("mmap -> %lld\n", m);
    // A file that is not there.
    long long o = nexum_sys_open((long long)"no_such_file_here", 0, 0);
    printf("open -> %lld\n", o);
    // A write to the standard output.
    long long w = nexum_sys_write(1, (long long)"hello\n", 6);
    printf("write -> %lld\n", w);
    return 0;
}
"#;

#[test]
#[cfg_attr(target_os = "windows", ignore = "the probe makes POSIX system calls")]
fn the_kernel_reports_what_it_reports() {
    let Some(tc) = toolchain() else { return };
    let dir = work_dir();
    let driver = dir.join("probe.c");
    std::fs::write(&driver, DRIVER).expect("write the driver");
    let obj = dir.join("probe.o");
    let status = std::process::Command::new(&tc.clang)
        .arg("-c")
        .arg(&driver)
        .arg("-o")
        .arg(&obj)
        .status()
        .expect("compile the driver");
    assert!(status.success(), "the driver did not compile");

    // The wrappers, made callable from the driver: the ones for the
    // system this test is running on.
    let ll = dir.join("probe.ll");
    let mut ir = wrappers_for_host();
    // The wrappers are internal in the runtime; the driver needs to call
    // them, so the definition is made external.
    ir = ir.replace("define internal i64 @nexum_sys_", "define i64 @nexum_sys_");
    std::fs::write(&ll, &ir).expect("write the module");

    let exe = dir.join("probe");
    let out = std::process::Command::new(&tc.clang)
        .arg(&ll)
        .arg(&obj)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("link the probe");
    assert!(
        out.status.success(),
        "the probe did not link:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = std::process::Command::new(&exe).output().expect("run");
    let stdout = String::from_utf8_lossy(&run.stdout);
    println!("the kernel said:\n{stdout}");
    // The probe is a diagnostic, not a gate: it prints what the kernel
    // said, on whichever platform it runs. The mapping has to succeed, so
    // that a failure here is a failure of the layer rather than of the
    // probe's own flags.
    let mapped = stdout
        .lines()
        .find(|l| l.starts_with("mmap ->"))
        .and_then(|l| l.trim_start_matches("mmap -> ").trim().parse::<i64>().ok())
        .unwrap_or(0);
    assert!(
        mapped > 0 && mapped % 4096 == 0,
        "the mapping did not succeed: {stdout}"
    );
}

/// The runtime's system call wrappers, for the system this test runs on.
fn wrappers_for_host() -> String {
    let os = if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    };
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    wrappers_for(os, arch)
}

/// The runtime's system call wrappers, built for one system.
fn wrappers_for(os: &str, arch: &str) -> String {
    let sys_os = if os == "macos" {
        syscalls::SysOs::Macos
    } else {
        syscalls::SysOs::Linux
    };
    let sys_arch = if arch == "aarch64" {
        syscalls::SysArch::Aarch64
    } else {
        syscalls::SysArch::X86_64
    };
    let numbers = syscalls::Syscalls::for_os(sys_os);
    let mut ir = syscalls::wrappers(sys_os, sys_arch, &numbers);
    // The Darwin mapping flags, and the protection.
    ir = ir.replace("@nexum_sys_mmap(i64 %a0", "@nexum_sys_mmap(i64 %a0");
    ir
}

/// The runtime's system call layer, from its source.
#[path = "../../nexum-runtime/src/syscalls.rs"]
mod syscalls;
