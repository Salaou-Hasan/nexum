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
//! second language). See `docs/ARGONE.md`, task C.

use std::path::PathBuf;

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
