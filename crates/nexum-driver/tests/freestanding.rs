//! The freestanding gate: a Nexum program depends on the kernel and on
//! nothing else.
//!
//! The POSIX targets reach the operating system through the runtime's
//! system call layer, so the only external symbols a program needs are the
//! ones the runtime has not replaced yet. This gate pins that list: the
//! number may only go down, and every symbol that is still there has to be
//! there on purpose.
//!
//! Phase 11a.2 replaces the allocator and the output formatting, which
//! takes the last six away. When it does, this file changes with it, and
//! the change is the point of the test.

mod common;

use common::{compile_file, examples_dir};

/// The external symbols a program declares, for a target.
fn external_symbols(target: &str) -> Vec<String> {
    let p = compile_file(&examples_dir().join("hello.nx"));
    let mir = p.mir.expect("mir");
    let spec = nexum_target::TargetSpec::by_name(target)
        .unwrap_or_else(|| panic!("unknown target {target}"));
    let ir = nexum_codegen::emit(&mir, &spec);
    let mut out = Vec::new();
    for line in ir.lines() {
        if let Some(rest) = line.strip_prefix("declare ")
            && let Some(open) = rest.find('(')
            && let Some(at) = rest[..open].find('@')
        {
            out.push(rest[at + 1..open].to_string());
        }
    }
    out.sort();
    out
}

/// The POSIX targets are freestanding for everything but the allocator and
/// the output formatting.
#[test]
fn posix_programs_need_only_the_remaining_libc() {
    for target in [
        "linux-x86_64",
        "linux-aarch64",
        "macos-x86_64",
        "macos-aarch64",
    ] {
        let symbols = external_symbols(target);
        assert_eq!(
            symbols,
            vec!["exit", "fputc", "fputs", "printf"],
            "{target} declares {symbols:?}"
        );
    }
}

/// The Windows build is still on the C library for its output and its file
/// routines: its kernel interface lives behind `ntdll`, and the system call
/// stubs are phase 11a.2. Its heap is ours, though: the allocator uses
/// `VirtualAlloc`, which is the kernel and not the C library.
#[test]
fn windows_still_needs_the_c_library() {
    let symbols = external_symbols("windows-x86_64");
    assert_eq!(
        symbols,
        vec![
            "VirtualAlloc",
            "VirtualFree",
            "__acrt_iob_func",
            "exit",
            "fclose",
            "fopen",
            "fputc",
            "fputs",
            "fread",
            "fseek",
            "ftell",
            "fwrite",
            "printf",
        ],
        "windows declares {symbols:?}"
    );
}

/// The POSIX targets reach the kernel through the system call layer and
/// not through the C library.
#[test]
fn posix_programs_have_the_system_call_layer() {
    let p = compile_file(&examples_dir().join("hello.nx"));
    let mir = p.mir.expect("mir");
    for target in ["linux-x86_64", "macos-aarch64"] {
        let spec = nexum_target::TargetSpec::by_name(target).unwrap();
        let ir = nexum_codegen::emit(&mir, &spec);
        for call in ["read", "write", "open", "close", "lseek", "exit"] {
            assert!(
                ir.contains(&format!("@nexum_sys_{call}")),
                "{target} is missing the {call} wrapper"
            );
        }
        // The layer is inline assembly, not a call into a library.
        assert!(
            ir.contains("asm sideeffect"),
            "{target} has no system calls"
        );
    }
}

/// The runtime's own string routines replace the C library's, on every
/// platform.
#[test]
fn the_string_routines_are_ours() {
    let p = compile_file(&examples_dir().join("hello.nx"));
    let mir = p.mir.expect("mir");
    for target in ["linux-x86_64", "macos-aarch64", "windows-x86_64"] {
        let spec = nexum_target::TargetSpec::by_name(target).unwrap();
        let ir = nexum_codegen::emit(&mir, &spec);
        for routine in ["nexum_strlen", "nexum_memcpy", "nexum_strcmp"] {
            assert!(
                ir.contains(&format!("@{routine}(")),
                "{target} is missing {routine}"
            );
        }
        assert!(
            !ir.contains("declare i64 @strlen"),
            "{target} still declares the C library's strlen"
        );
        assert!(
            !ir.contains("declare ptr @memcpy"),
            "{target} still declares the C library's memcpy"
        );
        assert!(
            !ir.contains("declare i32 @strcmp"),
            "{target} still declares the C library's strcmp"
        );
    }
}
