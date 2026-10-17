//! Shared helpers for integration tests (included with `mod common;`).
#![allow(dead_code)]

use nexum_diagnostics::{Diagnostic, Severity};
use std::path::{Path, PathBuf};

pub fn has_errors(diags: &[Diagnostic]) -> bool {
    diags.iter().any(|d| d.severity == Severity::Error)
}

pub fn error_lines(diags: &[Diagnostic]) -> Vec<String> {
    diags
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .map(|d| format!("{}: {}", d.code.0, d.message))
        .collect()
}

pub struct Frontend {
    pub diagnostics: Vec<Diagnostic>,
    pub hir: Option<nexum_sema::HirModule>,
}

pub fn run_frontend(path: &Path) -> Frontend {
    let text = std::fs::read_to_string(path).unwrap();
    let mut sm = nexum_source::SourceMap::new();
    let fid = sm.add(path.to_path_buf(), text);
    let sf = sm.get(fid);
    let mut diagnostics = Vec::new();

    let lexed = nexum_lexer::lex(sf);
    diagnostics.extend(lexed.diagnostics.clone());
    if has_errors(&diagnostics) {
        return Frontend {
            diagnostics,
            hir: None,
        };
    }

    let parsed = nexum_parser::parse(&lexed.tokens);
    diagnostics.extend(parsed.diagnostics.clone());
    if has_errors(&diagnostics) {
        return Frontend {
            diagnostics,
            hir: None,
        };
    }

    let sema = nexum_sema::analyze(&parsed.module);
    let nexum_sema::SemaResult {
        hir,
        diagnostics: sema_diags,
    } = sema;
    diagnostics.extend(sema_diags);
    Frontend { diagnostics, hir }
}

/// Full front-end + MIR pipeline for a fixture file.
pub struct Pipeline {
    pub diagnostics: Vec<Diagnostic>,
    pub mir: Option<nexum_mir::MirModule>,
}

pub fn compile_file(path: &Path) -> Pipeline {
    let fe = run_frontend(path);
    if has_errors(&fe.diagnostics) {
        return Pipeline {
            diagnostics: fe.diagnostics,
            mir: None,
        };
    }
    let hir = fe.hir.expect("clean front end must produce HIR");
    let lowered = nexum_mir::lower(&hir);
    let mut mir = lowered.module;
    nexum_mir::optimize(&mut mir);
    let vdiags = nexum_mir::verify(&mir);
    let diagnostics: Vec<Diagnostic> = fe.diagnostics.into_iter().chain(vdiags).collect();
    Pipeline {
        diagnostics,
        mir: Some(mir),
    }
}

pub fn emit_ir(mir: &nexum_mir::MirModule) -> String {
    nexum_codegen::emit(mir, &nexum_target::TargetSpec::host())
}

pub fn nx_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().map(|e| e == "nx").unwrap_or(false))
        .collect();
    files.sort();
    files
}

pub fn fixtures_dir(sub: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(sub)
}

pub fn examples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("examples")
}
