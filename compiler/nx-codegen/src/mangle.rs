//! Symbol mangling and module-global collection.

use nx_ast::Stmt;

pub(crate) fn mangle_fn(module: &str, name: &str) -> String {
    format!("nx__f_{}__{module}__{name}", name.len())
}

pub(crate) fn mangle_global(module: &str, name: &str) -> String {
    format!("nx__g_{module}__{name}")
}

pub(crate) fn mangle_desc(module: &str, name: &str) -> String {
    // Length-prefixed like mangle_fn, so `a.b` and `a_bc` style collisions
    // cannot alias two descriptors.
    format!("nx__d_{}__{module}__{name}", name.len())
}

/// Names a statement binds at module level, in first-seen order.
///
/// Only what becomes a *global*: a plain name bound at the top level, an
/// import alias, and — the reason this recurses — a name assigned inside
/// a nested block at module level. A name bound inside one is still
/// module-visible and still needs its global declared up front, before any
/// function body exists to emit it into.
///
/// Loop variables, comprehension variables and function-local bindings are
/// deliberately not collected: those are slots inside the function that
/// owns them, not module globals.
pub(crate) fn collect_module_globals(s: &Stmt, out: &mut Vec<String>) {
    let mut push = |n: &String| {
        if !out.contains(n) {
            out.push(n.clone());
        }
    };
    match s {
        Stmt::AssignOp { target, .. } => {
            if let nx_ast::Target::Name(n) = target {
                push(n);
            }
        }
        Stmt::Assign { targets, .. } => {
            for t in targets {
                if let nx_ast::Target::Name(n) = t {
                    push(n);
                }
            }
        }
        Stmt::FromImport { names, .. } => {
            for (name, alias) in names {
                push(alias.as_ref().unwrap_or(name));
            }
        }
        Stmt::Import {
            module: m, alias, ..
        } => {
            push(alias.as_ref().unwrap_or(m));
        }

        // Control flow nests the same way everywhere: a loop's own
        // variable is a slot in the enclosing function, but assignments
        // in its body still bind module names. Function bodies are
        // separate scopes and contribute nothing (child_bodies skips them).
        _ => {
            for b in nx_ast::shape::child_bodies(s) {
                for t in b {
                    collect_module_globals(t, out);
                }
            }
        }
    }
}

pub(crate) fn mangle_method(module: &str, type_name: &str, method: &str) -> String {
    // Length-prefixed segments, so `a.B` + `c` can never alias `a` + `B.c`.
    format!("nx__m_{}__{module}__{type_name}__{method}", method.len())
}

pub(crate) fn mangle_init(module: &str) -> String {
    format!("nx__init_{module}")
}

pub(crate) fn mangle_done(module: &str) -> String {
    format!("nx__done_{module}")
}
