//! Phase 2 backend gate: LLVM IR verification, codegen invariants, and
//! native output comparison.
//!
//! These tests require `clang` on PATH. When it is missing they print a note
//! and pass (the compile-only gate still runs on machines without clang).

mod common;

use common::{compile_file, emit_ir, error_lines, examples_dir, fixtures_dir, nx_files};
use std::path::Path;
use std::process::Command;

fn toolchain() -> Option<nexum_toolchain::Toolchain> {
    match nexum_toolchain::Toolchain::discover() {
        Ok(tc) => Some(tc),
        Err(e) => {
            eprintln!("SKIP: toolchain unavailable: {e}");
            None
        }
    }
}

fn tmp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("nexum_tests");
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// Emit IR for the fixture, run it through the LLVM IR parser + verifier.
fn ir_verifies(path: &Path, tc: &nexum_toolchain::Toolchain) {
    let p = compile_file(path);
    let errs = error_lines(&p.diagnostics);
    assert!(
        errs.is_empty(),
        "{}: diagnostics: {:?}",
        path.display(),
        errs
    );
    let mir = p.mir.expect("mir");
    let ir = emit_ir(&mir);
    let ll_path = tmp_dir().join(format!(
        "{}.ll",
        path.file_stem().unwrap().to_string_lossy()
    ));
    std::fs::write(&ll_path, &ir).expect("write ir");
    if let Err(e) = nexum_toolchain::verify_ir(&tc.clang, ll_path.to_str().unwrap()) {
        panic!("{}: IR verification failed:\n{}", path.display(), e);
    }
}

#[test]
fn ir_verifies_for_pass_fixtures() {
    let Some(tc) = toolchain() else { return };
    for f in nx_files(&fixtures_dir("pass")) {
        ir_verifies(&f, &tc);
    }
}

#[test]
fn ir_verifies_for_examples() {
    let Some(tc) = toolchain() else { return };
    for f in nx_files(&examples_dir()) {
        ir_verifies(&f, &tc);
    }
}

#[test]
fn codegen_emits_expected_fragments() {
    // numbers.nx exercises int/float math, bool logic and bool printing.
    let p = compile_file(&fixtures_dir("pass").join("numbers.nx"));
    let mir = p.mir.expect("mir");
    let ir = emit_ir(&mir);
    assert!(
        ir.contains("define i32 @main("),
        "main must return i32:\n{ir}"
    );
    assert!(ir.contains("fadd double"), "float add missing:\n{ir}");
    assert!(ir.contains("mul i64"), "int mul missing:\n{ir}");
    assert!(
        !ir.contains("nsw"),
        "int arithmetic must be wrapping (no nsw):\n{ir}"
    );
    assert!(
        ir.contains("zext i1"),
        "bool must widen before printf:\n{ir}"
    );
    assert!(
        ir.contains("declare i32 @printf(ptr, ...)"),
        "printf declaration missing"
    );
    assert!(ir.trim_end().ends_with('}'), "module must close");
}

#[test]
fn run_outputs_match_fixtures() {
    let Some(tc) = toolchain() else { return };
    let dir = fixtures_dir("run");
    let files = nx_files(&dir);
    assert!(!files.is_empty(), "no run fixtures in {}", dir.display());
    for f in files {
        let expected_path = f.with_extension("out");
        let expected = std::fs::read_to_string(&expected_path)
            .unwrap_or_else(|e| panic!("missing {}: {e}", expected_path.display()));
        let p = compile_file(&f);
        let errs = error_lines(&p.diagnostics);
        assert!(errs.is_empty(), "{}: {:?}", f.display(), errs);
        let mir = p.mir.expect("mir");
        let ir = emit_ir(&mir);
        let stem = f.file_stem().unwrap().to_string_lossy().to_string();
        let ll_path = tmp_dir().join(format!("{stem}.ll"));
        let exe_path = tmp_dir().join(format!("{stem}.exe"));
        std::fs::write(&ll_path, ir).expect("write ir");
        nexum_toolchain::verify_ir(&tc.clang, ll_path.to_str().unwrap()).expect("verify ir");
        nexum_toolchain::run_clang(
            &tc.clang,
            ll_path.to_str().unwrap(),
            exe_path.to_str().unwrap(),
        )
        .expect("link");
        let out = Command::new(&exe_path).output().expect("run exe");
        let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
        assert_eq!(stdout, expected, "{}: stdout mismatch", f.display());
        let expected_exit = std::fs::read_to_string(f.with_extension("exit"))
            .ok()
            .map(|s| {
                s.trim()
                    .parse::<i32>()
                    .expect("exit fixture must be an integer")
            })
            .unwrap_or(0);
        assert_eq!(
            out.status.code(),
            Some(expected_exit),
            "{}: exit code mismatch",
            f.display()
        );
    }
}
