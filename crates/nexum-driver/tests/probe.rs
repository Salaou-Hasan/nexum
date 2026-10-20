//! A probe for the system call layer: what does the kernel actually
//! return, and where does the allocator break?
//!
//! The error convention is the one thing about a system call that cannot
//! be checked by reading the code. Linux returns the error number negated;
//! Darwin returns it positive and sets the carry flag. The probe calls the
//! raw wrappers and prints what came back, and then asks the allocator for
//! a block, so a mismatch is visible instead of theoretical.

use std::path::PathBuf;

fn work_dir() -> PathBuf {
    let d = std::env::temp_dir().join("nexum_tests").join("probe");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn toolchain() -> Option<nexum_toolchain::Toolchain> {
    nexum_toolchain::Toolchain::discover().ok()
}

/// The driver: the raw calls first, then the allocator, then the same
/// sequence the allocator's own test walks - every class, a free, and the
/// same block again - printing after each step.
const DRIVER: &str = r#"
#include <stdio.h>

extern long long nexum_sys_mmap(long long addr, long long len, long long prot,
                                long long flags, long long fd, long long off);
extern long long nexum_sys_open(long long path, long long flags, long long mode);
extern long long nexum_sys_write(long long fd, long long buf, long long len);
extern void *nexum_heap_alloc(long long total);
extern void nexum_heap_free(void *p, long long total);
extern long long nexum_size_class(long long total);

static long long class_size_of(long long c) {
    if (c <= 254) return 32 + (c << 4);
    return 8192LL << (c - 255);
}

int main(void) {
    // Unbuffered: if the program crashes, the lines printed before the
    // crash are the ones that say where it died.
    setvbuf(stdout, 0, _IONBF, 0);
    printf("size_class(48) -> %lld\n", nexum_size_class(48));
    long long m = nexum_sys_mmap(0, 16384, 3, 4098, -1, 0);
    printf("mmap -> %lld\n", m);
    long long o = nexum_sys_open((long long)"no_such_file_here", 0, 0);
    printf("open -> %lld\n", o);
    long long w = nexum_sys_write(1, (long long)"hello\n", 6);
    printf("write -> %lld\n", w);

    long long c;
    for (c = 0; c <= 258; c++) {
        long long total = class_size_of(c);
        long long cls = nexum_size_class(total);
        void *p = nexum_heap_alloc(total);
        printf("class %lld %lld %lld -> %p\n", c, total, cls, p);
        nexum_heap_free(p, total);
        void *q = nexum_heap_alloc(total);
        printf("class %lld again -> %p\n", c, q);
        nexum_heap_free(q, total);
    }
    printf("done\n");
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

    // The wrappers and the allocator, for the system this test runs on.
    let ll = dir.join("probe.ll");
    let mut ir = wrappers_for_host();
    ir.push_str(&heap::allocator_ir(host_os(), host_arch()));
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
    // said, on whichever platform it runs. The mapping and the allocation
    // have to succeed, so that a failure here is a failure of the layer
    // rather than of the probe's own flags.
    let mapped = stdout
        .lines()
        .find(|l| l.starts_with("mmap ->"))
        .and_then(|l| l.trim_start_matches("mmap -> ").trim().parse::<i64>().ok())
        .unwrap_or(0);
    assert!(
        mapped > 0 && mapped % 4096 == 0,
        "the mapping did not succeed:\n{stdout}"
    );
    let allocated = stdout
        .lines()
        .find(|l| l.starts_with("class 0 "))
        .map(|l| l.rsplit(" -> ").next().unwrap_or("").trim().to_string())
        .unwrap_or_default();
    assert!(
        !allocated.is_empty() && allocated != "(nil)" && allocated != "0x0",
        "the allocator did not return a block:\n{stdout}"
    );
    assert!(
        allocated != "0x0" && !allocated.is_empty(),
        "the allocator did not return a block:\n{stdout}"
    );
    // The whole sequence the allocator's own test walks has to finish.
    assert!(
        stdout.contains("done"),
        "the allocator did not serve every class:\n{stdout}"
    );
    // The contract the runtime relies on: the wrappers report an error as a
    // negative number, on every system. Linux returns the error number
    // negated; Darwin returns it positive and sets the carry flag, and the
    // wrapper has to make it negative. If this fails, the value in the
    // message says what the kernel returned.
    let opened = stdout
        .lines()
        .find(|l| l.starts_with("open ->"))
        .and_then(|l| l.trim_start_matches("open -> ").trim().parse::<i64>().ok())
        .unwrap_or(0);
    assert!(
        opened < 0,
        "the failed open was not reported as negative:\n{stdout}"
    );
}

/// The system this test runs on.
fn host_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "linux"
    }
}

fn host_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

/// The runtime's system call wrappers, for the system this test runs on.
fn wrappers_for_host() -> String {
    let sys_os = if host_os() == "macos" {
        syscalls::SysOs::Macos
    } else {
        syscalls::SysOs::Linux
    };
    let sys_arch = if host_arch() == "aarch64" {
        syscalls::SysArch::Aarch64
    } else {
        syscalls::SysArch::X86_64
    };
    let numbers = syscalls::Syscalls::for_os(sys_os);
    syscalls::wrappers(sys_os, sys_arch, &numbers)
}

/// The runtime's system call layer, from its source.
///
/// The source is included rather than depended on, because the runtime
/// crate is not a dependency of the driver. The items the test does
/// not use are not dead here.
#[path = "../../nexum-runtime/src/syscalls.rs"]
#[allow(dead_code)]
mod syscalls;

/// The runtime's allocator, from its source.
#[path = "../../nexum-runtime/src/heap.rs"]
#[allow(dead_code)]
mod heap;
