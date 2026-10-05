//! Shared semantic shapes: facts several compiler stages re-derive
//! independently, owned once here so they cannot drift apart.
//!
//! `nx-ast` is the universal ancestor crate, so this module adds zero
//! dependency edges. One rule governs what belongs here: a shape states
//! what the language means, never how a stage represents it. That is why
//! the exhaustive statement walkers stay per-crate (a new `Stmt` variant
//! is a compile error in three crates at once, which is the cheapest sync
//! mechanism available) and why each stage keeps its own dispatch
//! predicate (`NX_NOUNBOX` must stay a representation switch, not a
//! second language). Two more deliberate non-actions live here as
//! documentation, so nobody "fixes" them later:
//!
//! - Read-name sets are NOT shared. `nx-ir` needs shadowing precision
//!   (a comprehension variable is local, keeping the function pure);
//!   `nx-mem` needs over-approximation (an extra root only costs memory,
//!   a missed one costs soundness). Sharing either direction breaks the
//!   other stage.
//! - Receiver predicates are NOT shared. The checker's `self_root_denied`
//!   walks to a root name; the backend's `target_of_expr` rebuilds a
//!   storage target. Same three shapes, different questions. See
//!   `docs/ARGONE.md`, task C.

use std::collections::HashSet;
use std::path::PathBuf;

use super::Stmt;

/// Resolve a module `name` to its `<name>.nx` file: each of `base_dirs`
/// in order, then each `NX_PATH` entry in order. Returns the first path
/// that is a file, or `None` when nothing matches.
///
/// This is the one owner of module search. Five copies of it used to live
/// across `nx-types`, `nx-ir` and `nx-codegen`, identical except for what
/// they did with a miss. The miss handling stays with the caller (a type
/// error, an `IrError`, a fallback, a silent skip); the search itself is
/// here, so the order, the `NX_PATH` splitting and the `is_file` check
/// cannot drift apart again.
///
/// Contract, kept byte-for-byte from the copies it replaces:
/// - the file name is exactly `{name}.nx`: no sanitizing, no canonicalizing
/// - `NX_PATH` splits with `std::env::split_paths` (`;` on Windows, `:` on
///   Unix) and is silently absent when unset or invalid
/// - a directory named `<name>.nx` does not match (`is_file`, not `exists`)
pub fn resolve_module_file(base_dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    let file = format!("{name}.nx");
    let mut dirs: Vec<PathBuf> = base_dirs.to_vec();
    if let Ok(p) = std::env::var("NX_PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file())
}

/// Child statement bodies of control flow, in source order: `then` plus
/// each `elif` plus `else`, or a loop body. Function and method bodies
/// are NOT included -- they are separate scopes, and every caller that
/// needs them (indexers) handles `Fn`/`Impl` explicitly while callers
/// that must not see them (module-global collection) rely on it.
///
/// Seven hand-rolled copies of the `elifs`/`else_body` unpacking used to
/// live across `nx-ir`, `nx-mem` and `nx-codegen`. Walkers that also need
/// conditions, iterables or loop variables keep their own structure;
/// this covers the pure body recursion.
pub fn child_bodies(stmt: &Stmt) -> Vec<&[Stmt]> {
    match stmt {
        Stmt::If { then_body, elifs, else_body, .. } => {
            let mut out = Vec::with_capacity(2 + elifs.len());
            out.push(then_body.as_slice());
            for (_, b) in elifs {
                out.push(b.as_slice());
            }
            if let Some(b) = else_body {
                out.push(b.as_slice());
            }
            out
        }
        Stmt::While { body, .. } | Stmt::For { body, .. } => vec![body.as_slice()],
        _ => Vec::new(),
    }
}

/// Names bound anywhere in a body: plain assignment targets and `for`
/// loop variables, flow-insensitively, recursing through control flow
/// but never into function or method bodies (separate scopes).
/// Index and field targets rebind nothing, so they contribute no names;
/// neither do `del`, compound assignment, imports or loop depth.
pub fn assigned_names(body: &[Stmt], out: &mut HashSet<String>) {
    for s in body {
        match s {
            Stmt::Assign { targets, .. } => {
                for t in targets {
                    if let super::Target::Name(n) = t {
                        out.insert(n.clone());
                    }
                }
            }
            Stmt::For { var, .. } => {
                out.insert(var.clone());
            }
            _ => {}
        }
        for b in child_bodies(s) {
            assigned_names(b, out);
        }
    }
}

/// Module names imported anywhere in a program, top-level or nested in
/// function and method bodies, first mention first, duplicates removed.
/// Imports are legal inside functions (the checker binds them there), so
/// a top-level-only scan misses dependencies and the loader emits
/// references to globals that were never declared.
pub fn imported_modules(prog: &super::Program) -> Vec<String> {
    fn walk(stmts: &[Stmt], out: &mut Vec<String>) {
        for s in stmts {
            match s {
                Stmt::Import { module, .. } => {
                    if !out.contains(module) {
                        out.push(module.clone());
                    }
                }
                Stmt::FromImport { module, .. } => {
                    if !out.contains(module) {
                        out.push(module.clone());
                    }
                }
                Stmt::Fn { body, .. } => walk(body, out),
                Stmt::Impl { methods, .. } => {
                    for m in methods {
                        walk(&m.body, out);
                    }
                }
                _ => {}
            }
            for b in child_bodies(s) {
                walk(b, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(&prog.stmts, &mut out);
    out
}

/// Method key `Type.method`: how `nx-ir` and `nx-mem` file per-method
/// summaries and plans. Type and method names are identifiers, so neither
/// half ever contains a dot and the key is unambiguous.
pub fn method_key(type_name: &str, method: &str) -> String {
    format!("{type_name}.{method}")
}

/// Split a method key back into `(type, method)`. Total: `None` when
/// there is no dot or either half is empty, so callers cannot misread
/// a plain function name as a method key.
pub fn split_method_key(key: &str) -> Option<(&str, &str)> {
    let (t, m) = key.rsplit_once('.')?;
    if t.is_empty() || m.is_empty() {
        return None;
    }
    Some((t, m))
}

/// Ambient builtins: (name, minimum args, maximum args). The SET is owned
/// here; each stage keeps its own checking and emission rules beside its
/// diagnostics, where a missing rule is a compile error or a loud
/// "unknown builtin" rather than silent agreement. `input` takes an
/// optional prompt, so it is the only builtin with a range. `int` and
/// `float` convert one value (sugar `x.int()` works like `xs.push(1)`).
pub const BUILTINS: &[(&str, usize, usize)] =
    &[("len", 1, 1), ("push", 2, 2), ("input", 0, 1), ("int", 1, 1), ("float", 1, 1)];

/// Arity range of an ambient builtin, if `name` is one.
pub fn builtin_arity(name: &str) -> Option<(usize, usize)> {
    BUILTINS
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, lo, hi)| (*lo, *hi))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    static SEQ: AtomicU64 = AtomicU64::new(0);
    // The NX_PATH test mutates process-global state, so those tests
    // serialize on this lock and restore the variable afterwards. Any
    // other test resolving modules concurrently only looks up names that
    // do not exist in the probe dir, so it resolves exactly as before.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn scratch_dir(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "nxshape-{tag}-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    fn write(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, contents).expect("write probe module");
        p
    }

    #[test]
    fn finds_a_module_under_the_base_dir() {
        let dir = scratch_dir("base");
        let want = write(&dir, "m.nx", "x = 1\n");
        assert_eq!(resolve_module_file(&[dir.clone()], "m"), Some(want));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_module_is_none() {
        let dir = scratch_dir("missing");
        assert_eq!(resolve_module_file(&[dir.clone()], "nope"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_base_dir_wins() {
        // Two base dirs, as `dependencies` passes them: the importing
        // file's own dir, then the entry dir.
        let a = scratch_dir("first-a");
        let b = scratch_dir("first-b");
        write(&a, "m.nx", "x = 1\n");
        let wb = write(&b, "m.nx", "x = 2\n");
        assert_eq!(
            resolve_module_file(&[a.clone(), b.clone()], "m"),
            Some(a.join("m.nx"))
        );
        assert_eq!(resolve_module_file(&[b.clone()], "m"), Some(wb));
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn a_directory_named_dot_nx_does_not_match() {
        let dir = scratch_dir("dirnamed");
        std::fs::create_dir_all(dir.join("m.nx")).expect("create probe dir");
        assert_eq!(resolve_module_file(&[dir.clone()], "m"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nx_path_entries_extend_the_search() {
        let _guard = ENV_LOCK.lock().unwrap();
        let base = scratch_dir("nxpath-base");
        let extra = scratch_dir("nxpath-extra");
        let want = write(&extra, "nx_shape_probe_xyz.nx", "x = 1\n");
        let saved = std::env::var_os("NX_PATH");
        std::env::set_var("NX_PATH", extra.as_os_str());
        let found = resolve_module_file(&[base.clone()], "nx_shape_probe_xyz");
        match saved {
            Some(v) => std::env::set_var("NX_PATH", v),
            None => std::env::remove_var("NX_PATH"),
        }
        assert_eq!(found, Some(want));
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&extra);
    }
}
