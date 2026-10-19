//! The allocator's tests.
//!
//! The allocator is LLVM IR text, so there are two kinds of test here.
//!
//! * **The shape of the IR.** The class ladder is computed and not
//!   searched, the free list link is the collector's `+16` word, the
//!   mapping arguments are in the order the system call expects, and the
//!   page size is 16 KiB on macOS on Apple Silicon and 4 KiB everywhere
//!   else. Those are facts about the text, and they are checked on the
//!   text.
//! * **The behaviour.** The allocator is compiled and run: every class is
//!   served, a freed block comes back out of the free list, a block that
//!   is larger than the biggest class has a mapping of its own, and every
//!   block that comes back is 16-byte aligned.
//! * **The collector on the allocator.** The runtime's collector is wired
//!   to the allocator, and the programs that lean on the collector keep
//!   their output. That is the wiring the runtime still has to make, and
//!   this test makes it before the runtime does.
//!
//! The runtime crate is not a dependency of the driver, so its two
//! modules are included by path: the test needs the very source that is
//! shipped, not a copy of it.

#[path = "../../nexum-runtime/src/heap.rs"]
#[allow(dead_code)]
mod heap;
#[path = "../../nexum-runtime/src/syscalls.rs"]
#[allow(dead_code)]
mod syscalls;

mod common;

use common::{compile_file, fixtures_dir};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The C driver: it calls the allocator and prints what it saw.
///
/// The class ladder is computed here as well, so the test does not take
/// the allocator's word for its own arithmetic.
const DRIVER: &str = r#"
#include <stdio.h>
#include <stdint.h>

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
    return 0;
}
"#;

/// The mapping layer, replaced by an arena.
///
/// The allocator's arithmetic is what the test is about; the kernel's
/// mapping is not, and the real system calls are exercised by the POSIX
/// test instead.
const FAKE_MAP: &str = r#"
#include <stdint.h>
#include <stddef.h>

#define ARENA_SIZE (16 * 1024 * 1024)

static unsigned char arena[ARENA_SIZE] __attribute__((aligned(64)));
static size_t arena_used = 0;

long long nexum_sys_mmap(long long addr, long long len, long long prot, long long flags, long long fd, long long off) {
    (void)addr; (void)prot; (void)flags; (void)fd; (void)off;
    if (len <= 0) return -1;
    size_t need = ((size_t)len + 15u) & ~(size_t)15u;
    if (arena_used + need > ARENA_SIZE) return -1;
    unsigned char *p = arena + arena_used;
    arena_used += need;
    return (long long)(intptr_t)p;
}

long long nexum_sys_munmap(long long addr, long long len) {
    (void)addr; (void)len;
    return 0;
}
"#;

/// The mapping layer, with every mapping refused.
///
/// The failure mode of the allocator is a null: the collector turns it
/// into `nexum_oom()`.
const FAILING_MAP: &str = r#"
#include <stdint.h>

long long nexum_sys_mmap(long long addr, long long len, long long prot, long long flags, long long fd, long long off) {
    (void)addr; (void)len; (void)prot; (void)flags; (void)fd; (void)off;
    return -1;
}

long long nexum_sys_munmap(long long addr, long long len) {
    (void)addr; (void)len;
    return 0;
}
"#;

/// The driver of the failure case: it allocates and prints whether it got
/// a block.
const FAIL_DRIVER: &str = r#"
#include <stdio.h>

extern void *nexum_heap_alloc(long long total);

int main(void) {
    void *p = nexum_heap_alloc(48);
    printf("null %d\n", p == 0 ? 1 : 0);
    return 0;
}
"#;

fn toolchain() -> Option<nexum_toolchain::Toolchain> {
    nexum_toolchain::Toolchain::discover().ok()
}

/// A private work directory.
fn work_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir()
        .join("nexum_tests")
        .join("allocator")
        .join(name);
    std::fs::create_dir_all(&d).expect("work dir");
    d
}

/// The name of an executable on this platform.
fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

/// The body of one function of the runtime, as text.
fn function_body(ir: &str, name: &str) -> String {
    let needle = format!("@{name}(");
    let start = ir
        .find(&needle)
        .unwrap_or_else(|| panic!("the runtime has no {name}:\n{ir}"));
    let rest = &ir[start..];
    let end = rest.find("\n}\n").unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Compile the allocator with the driver and run it.
///
/// The returned text is the driver's report. `None` means there is no
/// clang, and the test has nothing to say.
fn build_and_run(name: &str, ll: &str, extra: &[(&str, &str)]) -> Option<String> {
    build_and_run_with(name, ll, extra, DRIVER)
}

/// The same, with a driver of the test's choosing.
fn build_and_run_with(
    name: &str,
    ll: &str,
    extra: &[(&str, &str)],
    driver: &str,
) -> Option<String> {
    let tc = toolchain()?;
    let dir = work_dir(name);
    let ll_path = dir.join("allocator.ll");
    std::fs::write(&ll_path, ll).expect("write ir");

    let mut sources: Vec<PathBuf> = Vec::new();
    for (i, (stem, text)) in extra.iter().enumerate() {
        let p = dir.join(format!("{stem}{i}.c"));
        std::fs::write(&p, text).expect("write c");
        sources.push(p);
    }
    let driver_path = dir.join("driver.c");
    std::fs::write(&driver_path, driver).expect("write driver");
    sources.push(driver_path);

    let exe = dir.join(exe_name("allocator_test"));
    let mut cmd = Command::new(&tc.clang);
    cmd.arg("-Wno-override-module").arg(&ll_path);
    for s in &sources {
        cmd.arg(s);
    }
    cmd.arg("-o").arg(&exe);
    let out = cmd.output().expect("clang");
    assert!(
        out.status.success(),
        "clang failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&exe).output().expect("run");
    assert!(
        run.status.success(),
        "the allocator program failed:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );
    Some(String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n"))
}

/// Check the driver's report against the design.
fn check_report(report: &str) {
    let mut classes = 0;
    let mut large = 0;
    let mut raw = 0;
    for line in report.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f.first().copied() {
            Some("class") => {
                // class <c> <total> <class> <align> <null> <recycled>
                assert_eq!(f.len(), 7, "a class line has seven fields: {line}");
                let c: i64 = f[1].parse().expect("the class index");
                let total: i64 = f[2].parse().expect("the total");
                let cls: i64 = f[3].parse().expect("the class");
                let align: i64 = f[4].parse().expect("the alignment");
                let isnull: i64 = f[5].parse().expect("the null flag");
                let recycled: i64 = f[6].parse().expect("the recycling flag");
                assert_eq!(
                    cls,
                    heap::size_class(total),
                    "class {c}: a request of {total} bytes is not in the class it belongs to"
                );
                assert_eq!(
                    heap::class_size(c),
                    total,
                    "class {c}: the class does not have the size it is supposed to have"
                );
                assert_eq!(
                    align, 0,
                    "class {c}: a block of {total} bytes is not 16-byte aligned"
                );
                assert_eq!(
                    isnull, 0,
                    "class {c}: the allocation of {total} bytes failed"
                );
                assert_eq!(
                    recycled, 1,
                    "class {c}: a freed block of {total} bytes did not come back out of the free list"
                );
                classes += 1;
            }
            Some("large") => {
                // large <total> <class> <align> <null>
                assert_eq!(f.len(), 5, "a large line has five fields: {line}");
                let total: i64 = f[1].parse().expect("the total");
                let cls: i64 = f[2].parse().expect("the class");
                let align: i64 = f[3].parse().expect("the alignment");
                let isnull: i64 = f[4].parse().expect("the null flag");
                assert_eq!(
                    cls, 259,
                    "a request of {total} bytes is above the biggest class, so it has a class of its own"
                );
                assert_eq!(align, 0, "a block of {total} bytes is not 16-byte aligned");
                assert_eq!(isnull, 0, "the allocation of {total} bytes failed");
                large += 1;
            }
            Some("raw") => {
                // raw <size> <align> <null>
                assert_eq!(f.len(), 4, "a raw line has four fields: {line}");
                let size: i64 = f[1].parse().expect("the size");
                let align: i64 = f[2].parse().expect("the alignment");
                let isnull: i64 = f[3].parse().expect("the null flag");
                assert_eq!(
                    align, 0,
                    "a raw block of {size} bytes is not 16-byte aligned"
                );
                assert_eq!(isnull, 0, "the raw allocation of {size} bytes failed");
                raw += 1;
            }
            other => panic!("unexpected line in the report: {other:?}"),
        }
    }
    assert_eq!(classes, 259, "every class but the large one must be served");
    assert_eq!(large, 1, "a block above the biggest class must be served");
    assert_eq!(raw, 1, "a raw block must be served");
}

/// The class of a size is computed, not searched.
#[test]
fn the_class_ladder_is_the_one_the_design_says() {
    assert_eq!(heap::size_class(32), 0);
    assert_eq!(heap::size_class(48), 1);
    assert_eq!(heap::size_class(64), 2);
    assert_eq!(heap::size_class(4096), 254);
    assert_eq!(heap::size_class(4097), 255);
    assert_eq!(heap::size_class(8192), 255);
    assert_eq!(heap::size_class(8193), 256);
    assert_eq!(heap::size_class(16384), 256);
    assert_eq!(heap::size_class(16385), 257);
    assert_eq!(heap::size_class(32768), 257);
    assert_eq!(heap::size_class(32769), 258);
    assert_eq!(heap::size_class(65536), 258);
    assert_eq!(heap::size_class(65537), 259);
    // A size the allocator does not serve has no class.
    assert_eq!(heap::size_class(31), 260);
}

/// Every size fits the class it is put in, and the fine classes are exact.
#[test]
fn every_size_fits_its_class() {
    for total in 32..=65536 {
        let c = heap::size_class(total);
        assert!(
            heap::class_size(c) >= total,
            "a request of {total} bytes does not fit in class {c}"
        );
    }
    // The collector asks for 32 + a multiple of 16, and the fine classes
    // fit such a request exactly: there is no internal fragmentation.
    for k in 0..=254i64 {
        let total = 32 + 16 * k;
        assert_eq!(heap::class_size(heap::size_class(total)), total);
    }
}

/// The free list lives in the collector's `+16` word.
#[test]
fn the_free_list_link_is_the_collectors_next_word() {
    let ir = heap::allocator_ir("linux", "x86_64");
    let free = function_body(&ir, "nexum_heap_free");
    assert!(
        free.contains("getelementptr i8, ptr %p, i64 16"),
        "the free list link is not the collector's `+16` word:\n{free}"
    );
    // The allocator pops by reading the same word and never writes it.
    let alloc = function_body(&ir, "nexum_heap_alloc");
    assert!(
        alloc.contains("getelementptr i8, ptr %head, i64 16"),
        "the allocator does not read the link word:\n{alloc}"
    );
}

/// There is one free list for every class.
#[test]
fn there_is_a_free_list_for_every_class() {
    let ir = heap::allocator_ir("linux", "x86_64");
    assert!(
        ir.contains("@nexum_free_lists = internal global [260 x ptr] zeroinitializer"),
        "{ir}"
    );
}

/// The mapping arguments are in the order the system call expects:
/// address, length, protection, flags, descriptor, offset. The flags go
/// in the fourth register and the descriptor in the fifth, and swapping
/// them maps the world with the wrong flags.
#[test]
fn the_mapping_arguments_are_in_order() {
    let linux = heap::allocator_ir("linux", "x86_64");
    assert!(
        linux.contains("call i64 @nexum_sys_mmap(i64 0, i64 %len, i64 3, i64 34, i64 -1, i64 0)"),
        "the mapping arguments are not in order:\n{linux}"
    );
    let macos = heap::allocator_ir("macos", "x86_64");
    assert!(
        macos.contains("call i64 @nexum_sys_mmap(i64 0, i64 %len, i64 3, i64 4098, i64 -1, i64 0)"),
        "the mapping arguments are not in order:\n{macos}"
    );
}

/// The page size is 16 KiB on macOS on Apple Silicon and 4 KiB everywhere
/// else, and the same constant is used for the mapping length and for the
/// unmap length: `munmap` refuses a length that is not a multiple of it.
#[test]
fn the_page_size_is_per_target() {
    let macos = heap::allocator_ir("macos", "aarch64");
    assert!(
        macos.contains("add i64 %n, 16383") && macos.contains("and i64 %a, -16384"),
        "the page size of macOS on Apple Silicon is not 16 KiB:\n{macos}"
    );
    for (os, arch) in [
        ("linux", "x86_64"),
        ("linux", "aarch64"),
        ("macos", "x86_64"),
        ("windows", "x86_64"),
    ] {
        let ir = heap::allocator_ir(os, arch);
        assert!(
            ir.contains("add i64 %n, 4095") && ir.contains("and i64 %a, -4096"),
            "the page size of {os}-{arch} is not 4 KiB:\n{ir}"
        );
    }
}

/// The phase's rule: no 128-bit division, because LLVM lowers it to a
/// call into the C library.
#[test]
fn the_allocator_does_not_divide_in_128_bits() {
    for (os, arch) in [
        ("linux", "x86_64"),
        ("macos", "aarch64"),
        ("windows", "x86_64"),
    ] {
        let ir = heap::allocator_ir(os, arch);
        assert!(!ir.contains("div i128"), "{os}-{arch}:\n{ir}");
        assert!(!ir.contains("mul i128"), "{os}-{arch}:\n{ir}");
    }
}

/// The allocator's behaviour, with the mapping layer replaced by an arena.
///
/// Every class is served, every block is 16-byte aligned, and a freed
/// block comes back out of the free list.
#[test]
fn the_allocator_serves_every_class_and_recycles() {
    let mut ll = heap::allocator_ir("linux", "x86_64");
    ll.push_str("declare i64 @nexum_sys_mmap(i64, i64, i64, i64, i64, i64)\n");
    ll.push_str("declare i64 @nexum_sys_munmap(i64, i64)\n");
    let Some(report) = build_and_run("arena", &ll, &[("arena", FAKE_MAP)]) else {
        return;
    };
    check_report(&report);
}

/// A failed mapping is a null, and the collector turns it into
/// `nexum_oom()`.
#[test]
fn a_failed_mapping_is_a_null() {
    let mut ll = heap::allocator_ir("linux", "x86_64");
    ll.push_str("declare i64 @nexum_sys_mmap(i64, i64, i64, i64, i64, i64)\n");
    ll.push_str("declare i64 @nexum_sys_munmap(i64, i64)\n");
    let Some(report) = build_and_run_with("failing", &ll, &[("failing", FAILING_MAP)], FAIL_DRIVER)
    else {
        return;
    };
    assert_eq!(
        report.trim(),
        "null 1",
        "a failed mapping did not give a null:\n{report}"
    );
}

/// The Windows build's allocator: the same arithmetic over a different raw
/// block layer.
#[cfg(windows)]
#[test]
fn the_windows_allocator_serves_every_class_and_recycles() {
    let ll = heap::allocator_ir("windows", "x86_64");
    let Some(report) = build_and_run("windows", &ll, &[]) else {
        return;
    };
    check_report(&report);
}

/// The POSIX allocator, with the real system call layer.
///
/// This is the test that proves the mapping arguments are in the order
/// the kernel expects: the kernel rejects the call otherwise.
#[cfg(unix)]
#[test]
fn the_posix_allocator_serves_every_class_and_recycles() {
    let sys_os = if cfg!(target_os = "macos") {
        syscalls::SysOs::Macos
    } else {
        syscalls::SysOs::Linux
    };
    let sys_arch = if cfg!(target_arch = "aarch64") {
        syscalls::SysArch::Aarch64
    } else {
        syscalls::SysArch::X86_64
    };
    let (os, arch) = match (sys_os, sys_arch) {
        (syscalls::SysOs::Macos, syscalls::SysArch::Aarch64) => ("macos", "aarch64"),
        (syscalls::SysOs::Macos, syscalls::SysArch::X86_64) => ("macos", "x86_64"),
        (syscalls::SysOs::Linux, syscalls::SysArch::Aarch64) => ("linux", "aarch64"),
        (syscalls::SysOs::Linux, syscalls::SysArch::X86_64) => ("linux", "x86_64"),
    };
    let mut ll = heap::allocator_ir(os, arch);
    ll.push_str(&syscalls::wrappers(
        sys_os,
        sys_arch,
        &syscalls::Syscalls::for_os(sys_os),
    ));
    let Some(report) = build_and_run("posix", &ll, &[]) else {
        return;
    };
    check_report(&report);
}

/// The allocator's IR verifies for every target.
///
/// The system call layer is part of it, so this also checks the assembly
/// of the wrappers on the targets the host is not.
#[test]
fn the_allocator_verifies_for_every_target() {
    let Some(tc) = toolchain() else { return };
    let dir = work_dir("verify");
    for (os, arch) in [
        ("linux", "x86_64"),
        ("linux", "aarch64"),
        ("macos", "x86_64"),
        ("macos", "aarch64"),
        ("windows", "x86_64"),
    ] {
        let mut ll = heap::allocator_ir(os, arch);
        if !os.contains("windows") {
            let sys_os = if os.contains("macos") {
                syscalls::SysOs::Macos
            } else {
                syscalls::SysOs::Linux
            };
            let sys_arch = if arch.contains("aarch64") {
                syscalls::SysArch::Aarch64
            } else {
                syscalls::SysArch::X86_64
            };
            ll.push_str(&syscalls::wrappers(
                sys_os,
                sys_arch,
                &syscalls::Syscalls::for_os(sys_os),
            ));
        }
        let path = dir.join(format!("allocator_{os}_{arch}.ll"));
        std::fs::write(&path, &ll).expect("write ir");
        let name = format!("{os}-{arch}");
        let spec = nexum_target::TargetSpec::by_name(&name)
            .unwrap_or_else(|| panic!("unknown target {name}"));
        nexum_toolchain::verify_ir_for(&tc.clang, path.to_str().unwrap(), Some(&spec.triple))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
    }
}

/// Wire the collector to the allocator, the way the runtime has to.
///
/// The `malloc` becomes the heap allocation, and the `free` becomes the
/// heap free with the size the header holds: the sweep reads the payload
/// size out of the header already, so the total is that size plus the
/// header. The order matters - the sweep unlinks an object before it
/// frees it, so the `+16` word is free when the allocator writes the
/// free-list link into it.
fn wire_the_allocator(ir: &str, os: &str, arch: &str) -> String {
    let mut ir = ir.to_string();
    ir = ir.replace("declare ptr @malloc(i64)\n", "");
    ir = ir.replace("declare void @free(ptr)\n", "");
    ir = ir.replace(
        "call ptr @malloc(i64 %total)",
        "call ptr @nexum_heap_alloc(i64 %total)",
    );
    ir = ir.replace(
        "call void @free(ptr %cur)",
        "%total = add i64 %sz, 32\n  call void @nexum_heap_free(ptr %cur, i64 %total)",
    );
    assert!(
        !ir.contains("@malloc(") && !ir.contains("@free("),
        "the runtime still calls the C library:\n{ir}"
    );
    format!("{ir}\n{}", heap::allocator_ir(os, arch))
}

/// The collector runs on the allocator.
///
/// The runtime's collector is wired to the allocator, and the programs
/// that lean on the collector keep their output. Before that change is
/// made in the runtime, the test makes it here; afterwards the runtime
/// has it and the test runs it as it is.
#[test]
fn the_collector_runs_on_the_allocator() {
    let Some(tc) = toolchain() else { return };
    let spec = nexum_target::TargetSpec::host();
    let dir = work_dir("collector");
    for fixture in ["gc_stress.nx", "gc_reclaim.nx", "list_gc.nx", "strings.nx"] {
        let path = fixtures_dir("run").join(fixture);
        let expected = std::fs::read_to_string(path.with_extension("out"))
            .unwrap_or_else(|e| panic!("missing the output of {fixture}: {e}"))
            .replace("\r\n", "\n");
        let p = compile_file(&path);
        assert!(
            p.diagnostics.is_empty(),
            "{fixture} must compile: {:?}",
            p.diagnostics
        );
        let mir = p.mir.expect("mir");
        let mut ir = nexum_codegen::emit(&mir, &spec);
        if !ir.contains("@nexum_heap_alloc(") {
            ir = wire_the_allocator(&ir, &spec.os, &spec.arch);
        }
        let stem = fixture.trim_end_matches(".nx");
        let ll_path = dir.join(format!("{stem}.ll"));
        let exe_path = dir.join(exe_name(stem));
        std::fs::write(&ll_path, &ir).expect("write ir");
        nexum_toolchain::verify_ir(&tc.clang, ll_path.to_str().unwrap())
            .unwrap_or_else(|e| panic!("{fixture}: {e}"));
        let mut cmd = Command::new(&tc.clang);
        cmd.arg("-Wno-override-module")
            .arg(ll_path)
            .arg("-o")
            .arg(&exe_path);
        let out = cmd.output().expect("clang");
        assert!(
            out.status.success(),
            "{fixture}: clang failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let run = Command::new(&exe_path).output().expect("run");
        assert!(
            run.status.success(),
            "{fixture}: the program failed:\n{}",
            String::from_utf8_lossy(&run.stderr)
        );
        let stdout = String::from_utf8_lossy(&run.stdout).replace("\r\n", "\n");
        assert_eq!(
            stdout, expected,
            "{fixture}: the output changed when the collector ran on the allocator"
        );
    }
}

/// The file is a test, not a program: it must not leave anything behind.
#[test]
fn the_work_directory_is_inside_the_temporary_directory() {
    let dir: &Path = &work_dir("shape");
    assert!(dir.starts_with(std::env::temp_dir()));
}
