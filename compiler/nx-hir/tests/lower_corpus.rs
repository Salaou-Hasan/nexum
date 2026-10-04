//! Every shipped program lowers. This is the widest net for lowering
//! bugs: one internal error on any corpus program is a lowering defect,
//! and the message names the span that produced it.
//!
//! The corpus is what the repository actually compiles today -- the
//! examples (including the two-module one) and the benchmark programs --
//! so this test fails before any user-visible regression does.

use std::path::{Path, PathBuf};

fn repo_dir() -> PathBuf {
    // <repo>/compiler/nx-hir/tests -> <repo>
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn nx_files_under(dir: PathBuf, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(&dir) {
        Ok(e) => e,
        // A missing corpus root is a broken checkout, not a pass.
        Err(e) => panic!("cannot read {}: {e}", dir.display()),
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            nx_files_under(path, out);
        } else if path.extension().map(|x| x == "nx").unwrap_or(false) {
            out.push(path);
        }
    }
}

fn corpus() -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in ["examples", "bench"] {
        nx_files_under(repo_dir().join(root), &mut out);
    }
    out.sort();
    out
}

#[test]
fn every_shipped_program_lowers() {
    let files = corpus();
    assert!(files.len() >= 30, "expected the shipped corpus, saw {}", files.len());
    let mut failures = Vec::new();
    for path in &files {
        let src = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{}: cannot read: {e}", path.display()));
                continue;
            }
        };
        let base = path.parent().expect("program has a directory");
        match nx_hir::lower::lower_source(&src, base) {
            Ok(h) => {
                if h.modules.is_empty() {
                    failures.push(format!("{}: lowered to a program with no modules", path.display()));
                }
            }
            Err(e) => failures.push(format!("{}: {e}", path.display())),
        }
    }
    assert!(failures.is_empty(), "lowering failed:\n{}", failures.join("\n"));
}