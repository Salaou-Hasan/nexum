//! A probe for the system call layer and the allocator.
//!
//! Two things are hard to check by reading the code, and this file is
//! about both of them.
//!
//! **The error convention.** Linux returns the error number negated;
//! Darwin returns it positive and sets the carry flag. The wrappers make
//! the result negative in the assembly so that one convention holds
//! everywhere, and this probe prints what the kernel actually returned,
//! before and after the wrappers have had their way with it.
//!
//! **Where the allocator breaks.** The allocator's own test crashed on
//! macOS with no output at all. A crash takes the buffered output with
//! it, so this driver writes unbuffered and prints after every step: the
//! step where it stops is the step where it died.

use std::path::PathBuf;

fn work_dir() -> PathBuf {
    let d = std::env::temp_dir().join("nexum_tests").join("probe");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn toolchain() -> Option<nexum_toolchain::Toolchain> {
    nexum_toolchain::Toolchain::discover().ok()
}

/// The driver: the raw calls first, so the kernel's own answers are
/// visible, and then the allocator's own sequence.
const DRIVER: &str = r#"
#include <stdio.h>
#include <stdint.h>

extern long long nexum_sys_mmap(long long addr, long long len, long long prot,
                                long long flags, long long fd, long long off);
extern long long nexum_sys_open(long long path, long long flags, long long mode);
extern long long nexum_sys_write(long long fd, long long buf, long long len);
extern void *nexum_heap_alloc(long long total);
extern void nexum_heap_free(void *p, long long total);
extern long long nexum_size_class(long long total);
extern void *nexum_alloc_raw(long long size);
extern void nexum_free_raw(void *p, long long size);

static long long class_size_of(long long c) {
    if (c <= 254) return 32 + (c << 4);
    return 8192LL << (c - 255);
}

int main(void) {
    // Unbuffered: if the program crashes, the lines printed before the
    // crash are the ones that say where it died.
    setvbuf(stdout, 0, _IONBF, 0);

    // What the kernel says, before the wrappers have had their way with it.
    long long m = nexum_sys_mmap(0, 16384, 3, 4098, -1, 0);
    printf("mmap -> %lld\n", m);
    long long o = nexum_sys_open((long long)"no_such_file_here", 0, 0);
    printf("open -> %lld\n", o);
    long long w = nexum_sys_write(1, (long long)"hello\n", 6);
    printf("write -> %lld\n", w);

    // The allocator's own sequence.
    long long c;
    for (c = 0; c <= 258; c++) {
        long long total = class_size_of(c);
        long long cls = nexum_size_class(total);
        void *p = nexum_heap_alloc(total);
        long long align = (long long)((uintptr_t)p % 16);
        int isnull = (p == 0) ? 1 : 0;
        nexum_heap_free(p, total);
        void *q = nexum_heap_alloc(total);
        long long same = (p == q) ? 1 : 0;
        nexum_heap_free(q, total);
        printf("class %lld %lld %lld %lld %d %lld\n", c, total, cls, align, isnull, same);
    }
    {
        long long total = 100000;
        long long cls = nexum_size_class(total);
        void *p = nexum_heap_alloc(total);
        long long align = (long long)((uintptr_t)p % 16);
        int isnull = (p == 0) ? 1 : 0;
        nexum_heap_free(p, total);
        printf("large %lld %lld %lld %d\n", total, cls, align, isnull);
    }
    {
        long long size = 1000;
        void *p = nexum_alloc_raw(size);
        long long align = (long long)((uintptr_t)p % 16);
        int isnull = (p == 0) ? 1 : 0;
        nexum_free_raw(p, size);
        printf("raw %lld %lld %d\n", size, align, isnull);
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

    // The same link the allocator's own test does: the module and the
    // driver in one clang invocation, the module first.
    let exe = dir.join("probe");
    let out = std::process::Command::new(&tc.clang)
        .arg("-Wno-override-module")
        .arg(&ll)
        .arg(&driver)
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

    // The mapping has to succeed, and the failed open has to be reported
    // as a negative number - the one convention the runtime relies on.
    let mapped = value_after(&stdout, "mmap ->").unwrap_or(0);
    assert!(
        mapped > 0 && mapped % 4096 == 0,
        "the mapping did not succeed:\n{stdout}"
    );
    let opened = value_after(&stdout, "open ->").unwrap_or(0);
    assert!(
        opened < 0,
        "the failed open was not reported as negative:\n{stdout}"
    );

    // The allocator's own sequence has to finish: the large class and the
    // raw path come last.
    let first = stdout
        .lines()
        .find(|l| l.starts_with("class 0 "))
        .map(|l| l.trim().to_string())
        .unwrap_or_default();
    assert!(
        !first.is_empty() && !first.ends_with("(nil)"),
        "the allocator did not return a block:\n{stdout}"
    );
    assert!(
        stdout.contains("raw 1000"),
        "the allocator did not serve every class:\n{stdout}"
    );
}

/// The number after a marker in the output.
fn value_after(text: &str, marker: &str) -> Option<i64> {
    text.lines()
        .find(|l| l.starts_with(marker))
        .and_then(|l| l.trim_start_matches(marker).trim().parse().ok())
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
#[path = "../../nexum-runtime/src/syscalls.rs"]
mod syscalls;

/// The runtime's allocator, from its source.
#[path = "../../nexum-runtime/src/heap.rs"]
mod heap;
