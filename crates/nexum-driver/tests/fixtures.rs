//! Fixture harness for the Phase 1 front-end gate.
//!
//! * `tests/fixtures/pass/*.nx` must lex, parse, type-check, lower to MIR and
//!   pass MIR verification with zero error diagnostics.
//! * `tests/fixtures/fail/*.nx` must produce diagnostics whose codes include
//!   every code listed in `# EXPECT: Txxx` comments, and every emitted
//!   diagnostic must carry a source span.

mod common;

use common::{compile_file, error_lines, fixtures_dir, nx_files, run_frontend};
use nexum_diagnostics::Severity;

#[test]
fn compile_pass_fixtures() {
    let dir = fixtures_dir("pass");
    let files = nx_files(&dir);
    assert!(
        !files.is_empty(),
        "no pass fixtures found in {}",
        dir.display()
    );
    for f in files {
        let p = compile_file(&f);
        let errs = error_lines(&p.diagnostics);
        assert!(
            errs.is_empty(),
            "{}: expected clean compile, got:\n{}",
            f.display(),
            errs.join("\n")
        );
        assert!(
            p.mir.is_some(),
            "{}: no MIR produced despite clean diagnostics",
            f.display()
        );
    }
}

#[test]
fn compile_fail_fixtures() {
    let dir = fixtures_dir("fail");
    let files = nx_files(&dir);
    assert!(
        !files.is_empty(),
        "no fail fixtures found in {}",
        dir.display()
    );
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let expected: Vec<String> = text
            .lines()
            .filter_map(|l| l.split('#').nth(1))
            .filter_map(|c| c.trim().strip_prefix("EXPECT:"))
            .map(|s| s.trim().to_string())
            .collect();
        assert!(
            !expected.is_empty(),
            "{}: fail fixture must declare at least one `# EXPECT: Txxx`",
            f.display()
        );

        let fe = run_frontend(&f);
        let codes: Vec<String> = fe
            .diagnostics
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.code.0.to_string())
            .collect();
        assert!(
            !codes.is_empty(),
            "{}: expected error diagnostics, got none",
            f.display()
        );
        for want in &expected {
            assert!(
                codes.contains(want),
                "{}: expected diagnostic `{}` not produced (got {:?})",
                f.display(),
                want,
                codes
            );
        }
        for d in &fe.diagnostics {
            assert!(
                d.span.is_some(),
                "{}: diagnostic {} has no source span: {}",
                f.display(),
                d.code.0,
                d.message
            );
        }
    }
}
