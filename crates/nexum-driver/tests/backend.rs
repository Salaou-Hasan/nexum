//! Phase 2 backend gate: LLVM IR verification, codegen invariants, and
//! native output comparison.

mod common;

use common::{
    compile_file, compile_file_opt, emit_ir, error_lines, examples_dir, fixtures_dir, nx_files,
};
use std::path::Path;
use std::process::Command;

fn toolchain() -> Option<nexum_toolchain::Toolchain> {
    match nexum_toolchain::Toolchain::discover() {
        Ok(tc) => Some(tc),
        Err(_) => None,
    }
}

fn tmp_dir() -> std::path::PathBuf {
    let d = std::env::temp_dir().join("nexum_tests");
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn ir_verifies(path: &Path, tc: &nexum_toolchain::Toolchain) {
    let p = common::compile_file(path);
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
fn ir_verifies_for_all_targets() {
    let Some(tc) = toolchain() else { return };
    let f = examples_dir().join("hello.nx");
    let p = common::compile_file(&f);
    let mir = p.mir.expect("mir");
    for name in nexum_target::TargetSpec::list() {
        let target = nexum_target::TargetSpec::by_name(name).unwrap();
        let ir = nexum_codegen::emit(&mir, &target);
        let ll_path = tmp_dir().join(format!("hello_{}.ll", name));
        std::fs::write(&ll_path, ir).expect("write ir");
        nexum_toolchain::verify_ir(&tc.clang, ll_path.to_str().unwrap())
            .unwrap_or_else(|e| panic!("target {}: {e}", name));
    }
}

#[test]
fn codegen_emits_expected_fragments() {
    let p = common::compile_file(&fixtures_dir("pass").join("numbers.nx"));
    let mir = p.mir.expect("mir");
    let ir = emit_ir(&mir);
    assert!(
        ir.contains("define i32 @main("),
        "main signature missing:\n{ir}"
    );
    assert!(ir.contains("fadd double"), "float add missing:\n{ir}");
    assert!(ir.contains("mul i64"), "int mul missing:\n{ir}");
    assert!(
        !ir.contains("nsw"),
        "int arithmetic must be wrapping (no nsw):\n{ir}"
    );
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
            .unwrap_or_else(|e| panic!("missing {}: {e}", expected_path.display()))
            .replace("\r\n", "\n");
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

#[test]
fn differential_opt_levels_agree() {
    let Some(tc) = toolchain() else { return };
    let files = [
        examples_dir().join("hello.nx"),
        examples_dir().join("fact.nx"),
        examples_dir().join("loop.nx"),
        examples_dir().join("particles.nx"),
        examples_dir().join("strings.nx"),
        fixtures_dir("run").join("strings.nx"),
        fixtures_dir("run").join("gc_stress.nx"),
        fixtures_dir("run").join("int_wrap.nx"),
        fixtures_dir("run").join("foreach.nx"),
        fixtures_dir("run").join("compound.nx"),
    ];
    for f in files {
        if !f.exists() {
            continue;
        }
        for level in [0u8, 1u8] {
            let p = compile_file_opt(&f, level);
            let errs = error_lines(&p.diagnostics);
            assert!(
                errs.is_empty(),
                "{} (opt {}): {:?}",
                f.display(),
                level,
                errs
            );
            let mir = p.mir.expect("mir");
            let ir = emit_ir(&mir);
            let stem = format!("{}_opt{}", f.file_stem().unwrap().to_string_lossy(), level);
            let ll_path = tmp_dir().join(format!("{stem}.ll"));
            let exe_path = tmp_dir().join(format!("{stem}.exe"));
            std::fs::write(&ll_path, ir).expect("write ir");
            nexum_toolchain::verify_ir(&tc.clang, ll_path.to_str().unwrap())
                .unwrap_or_else(|e| panic!("{} (opt {}): verify: {e}", f.display(), level));
            nexum_toolchain::run_clang(
                &tc.clang,
                ll_path.to_str().unwrap(),
                exe_path.to_str().unwrap(),
            )
            .unwrap_or_else(|e| panic!("{} (opt {}): link: {e}", f.display(), level));
            let out = Command::new(&exe_path).output().expect("run exe");
            let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
            let exit = out.status.code().unwrap_or(-1);
            let slot = tmp_dir().join(format!("{stem}.result"));
            std::fs::write(&slot, format!("{exit}\n{stdout}")).expect("write result");
        }
        let base = f.file_stem().unwrap().to_string_lossy().to_string();
        let r0 = std::fs::read_to_string(tmp_dir().join(format!("{base}_opt0.result"))).unwrap();
        let r1 = std::fs::read_to_string(tmp_dir().join(format!("{base}_opt1.result"))).unwrap();
        assert_eq!(
            r0,
            r1,
            "{}: opt0 and opt1 disagree:\n--- opt0 ---\n{}\n--- opt1 ---\n{}",
            f.display(),
            r0,
            r1
        );
    }
}
