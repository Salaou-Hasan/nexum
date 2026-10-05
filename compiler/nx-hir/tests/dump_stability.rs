//! `dump-hir` stability: the hir.md §9 contract, enforced.
//!
//! Three claims, three tests: two lowerings of one source produce
//! byte-identical dumps (so no `HashMap` order leaks), the output names
//! modules rather than paths, and one example still matches its
//! checked-in snapshot (so a change in the format itself fails loudly
//! rather than quietly).

use std::path::{Path, PathBuf};

fn repo_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn nx_files_under(dir: PathBuf, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(&dir).expect("corpus root is readable").filter_map(|e| e.ok()) {
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

fn dump_of(path: &Path) -> String {
    let src = std::fs::read_to_string(path).expect("program is readable");
    let base = path.parent().expect("program has a directory");
    let hir = nx_hir::lower::lower_source(&src, base)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    nx_hir::verify::verify(&hir).unwrap_or_else(|v| panic!("{}: {v:?}", path.display()));
    nx_hir::dump::dump(&hir)
}

#[test]
fn two_lowerings_dump_identically() {
    let files = corpus();
    assert!(files.len() >= 30, "expected the shipped corpus, saw {}", files.len());
    for path in &files {
        let a = dump_of(path);
        let b = dump_of(path);
        assert_eq!(a, b, "{} dumps differently on a second lowering", path.display());
        assert!(!a.is_empty(), "{} dumped nothing", path.display());
    }
}

#[test]
fn a_dump_names_modules_and_never_paths() {
    for path in corpus() {
        let text = dump_of(&path);
        let mut modules = 0;
        for line in text.lines() {
            let Some(rest) = line.strip_prefix("(module ") else { continue };
            modules += 1;
            let name = rest.split_whitespace().next().unwrap_or("");
            assert!(!name.is_empty(), "{}: a module printed without a name", path.display());
            // A module name is an identifier from the source; a path
            // separator or drive letter in one would mean the dump was
            // printing file locations.
            assert!(
                !name.contains(['\\', '/', ':']),
                "{}: module name '{name}' looks like a path",
                path.display()
            );
        }
        assert!(modules > 0, "{}: no module header at all", path.display());
    }
}

#[test]
fn one_example_still_matches_its_snapshot() {
    let example = repo_dir().join("examples").join("control.nx");
    let snapshot = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("snapshots")
        .join("control.hir.txt");
    let expected = std::fs::read_to_string(&snapshot)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", snapshot.display()));
    // The snapshot is committed LF but checks out CRLF on Windows
    // (core.autocrlf); the dump always uses `\n`, and spans are
    // line:col numbers either way, so compare line-ending-insensitively
    // rather than byte-wise. This exact mismatch failed CI's Windows
    // job while every LF checkout stayed green.
    let expected = expected.replace("\r\n", "\n");
    assert_eq!(
        dump_of(&example),
        expected,
        "the dump format drifted; review the new output before updating the snapshot"
    );
}