//! Whole-program planning driver: function index, retains fixpoint, and
//! per-binding classification into `Alloc` strategies.

use nx_ast::{Program, Stmt};
use std::collections::{HashMap, HashSet};

use crate::escape::{aliases_in, escaping_roots, resolve_callee, Root};
use crate::types::{Alloc, Plan};

/// Compute the allocation plan for a whole program (all modules).
/// `entry` is the main module name ("__main__").
///
/// `types` supplies the checker's inferred types, which decide which
/// bindings are scalars and therefore stack-allocated. Pass an empty map
/// to get the pre-unboxing plan (everything Unique or Shared).
pub fn plan(
    programs: &HashMap<String, Program>,
    entry: &str,
    types: &HashMap<(String, String), nx_types::FnInfo>,
) -> Plan {
    let _ = entry;
    // Index all functions, including nested ones.
    let mut fns: HashMap<(String, String), (Vec<String>, Vec<Stmt>)> = HashMap::new();
    for (module, prog) in programs {
        index_fns(module, &prog.stmts, &mut fns);
    }
    // Fixpoint: a function retains params if a param (or its alias) is
    // returned, pushed into a list, put in a list literal, passed to a
    // retaining callee, or if any call target is unknown.
    let mut retains: HashMap<(String, String), bool> = HashMap::new();
    for k in fns.keys() {
        retains.insert(k.clone(), false);
    }
    loop {
        let mut changed = false;
        for ((module, name), (params, body)) in &fns {
            if retains[&(module.clone(), name.clone())] {
                continue;
            }
            let aliases = aliases_in(body, params);
            let mut esc = HashSet::new();
            escaping_roots(body, &mut esc);
            let mut hit = false;
            'outer: for root in &esc {
                // root escapes if it is a param/alias, or passed to retainer.
                match root {
                    Root::Var(v) => {
                        if params.contains(v)
                            || aliases
                                .get(v)
                                .map(|s| s.iter().any(|a| params.contains(a)))
                                .unwrap_or(false)
                        {
                            hit = true;
                            break 'outer;
                        }
                    }
                    Root::ArgTo { callee, var } => {
                        let target = resolve_callee(module, callee, &fns);
                        match target {
                            None => {
                                hit = true;
                                break 'outer;
                            }
                            Some(t) => {
                                if t != (module.clone(), name.clone())
                                    && *retains.get(&t).unwrap_or(&false)
                                {
                                    hit = true;
                                    break 'outer;
                                }
                                if params.contains(var) {
                                    // Passed to a non-retaining callee: the
                                    // value itself is not retained. OK.
                                }
                            }
                        }
                    }
                }
            }
            if hit {
                retains.insert((module.clone(), name.clone()), true);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Per-binding plans.
    let mut plan = Plan {
        locals: HashMap::new(),
        retains: retains.clone(),
    };
    for ((module, name), (params, body)) in &fns {
        let aliases = aliases_in(body, params);
        let mut esc_vars: HashSet<String> = HashSet::new();
        let mut esc = HashSet::new();
        escaping_roots(body, &mut esc);
        for root in &esc {
            match root {
                Root::Var(v) => {
                    esc_vars.insert(v.clone());
                    if let Some(al) = aliases.get(v) {
                        esc_vars.extend(al.iter().cloned());
                    }
                    // Anything aliased TO an escaping var escapes as well.
                    for (a, set) in &aliases {
                        if set.contains(v) {
                            esc_vars.insert(a.clone());
                        }
                    }
                }
                Root::ArgTo { callee, var } => {
                    let target = resolve_callee(module, callee, &fns);
                    let retained = match target {
                        None => true,
                        Some(t) => {
                            t == (module.clone(), name.clone())
                                || *retains.get(&t).unwrap_or(&false)
                        }
                    };
                    if retained {
                        esc_vars.insert(var.clone());
                        if let Some(al) = aliases.get(var) {
                            esc_vars.extend(al.iter().cloned());
                        }
                    }
                }
            }
        }
        // Params are always Shared: the caller owns the buffers.
        // Bindings that share storage with another binding are Shared.
        // (Alias edges are bidirectional: `b = a` taints both.)
        let mut aliased: HashSet<String> = HashSet::new();
        for (v, set) in &aliases {
            if set.iter().any(|w| w != v) {
                aliased.insert(v.clone());
            }
        }
        let mut assigned: HashSet<String> = HashSet::new();
        assigned_in(body, &mut assigned);
        // Scalars are decided first: a value with no buffers cannot
        // dangle, so Stack is sound regardless of escape or aliasing.
        // Aliasing a scalar copies the number, not a pointer, so `b = a`
        // on two Ints still leaves both in registers.
        //
        // The key is (module, function) and `module`/`name` are borrowed
        // from `fns`, so build the owned key once.
        let key = (module.clone(), name.clone());
        let info = types.get(&key);
        let mut stackable: HashSet<String> = HashSet::new();
        if let Some(info) = info {
            for var in &assigned {
                if info.locals.get(var).map(|t| t.is_scalar()).unwrap_or(false) {
                    stackable.insert(var.clone());
                }
            }
        }
        for var in assigned {
            let alloc = if stackable.contains(&var) {
                Alloc::Stack
            } else if params.contains(&var) {
                Alloc::Shared
            } else if esc_vars.contains(&var) || aliased.contains(&var) {
                Alloc::Shared
            } else {
                Alloc::Unique
            };
            plan.locals
                .insert((module.clone(), name.clone(), var), alloc);
        }
        // for-each loop vars alias list elements: always Shared.
        for s in body {
            if let Stmt::For { var, iter, .. } = s {
                if matches!(iter, nx_ast::ForIter::Each(_)) {
                    plan.locals
                        .insert((module.clone(), name.clone(), var.clone()), Alloc::Shared);
                }
            }
        }
    }
    plan
}

fn index_fns(
    module: &str,
    stmts: &[Stmt],
    out: &mut HashMap<(String, String), (Vec<String>, Vec<Stmt>)>,
) {
    for s in stmts {
        match s {
            Stmt::Fn {
                name, params, body, ..
            } => {
                out.insert(
                    (module.to_string(), name.clone()),
                    (params.clone(), body.clone()),
                );
                index_fns(module, body, out);
            }
            // Methods plan like functions under `Type.method` keys. `self`
            // is a real parameter (it may be retained by `return self`),
            // so it joins the param list; associated functions have none.
            Stmt::Impl {
                type_name, methods, ..
            } => {
                for m in methods {
                    let key = (
                        module.to_string(),
                        nx_ast::shape::method_key(type_name, &m.name),
                    );
                    let mut ps = Vec::new();
                    if m.receiver != nx_ast::ReceiverKind::None {
                        ps.push("self".to_string());
                    }
                    ps.extend(m.params.clone());
                    out.insert(key, (ps, m.body.clone()));
                    index_fns(module, &m.body, out);
                }
            }
            _ => {
                for b in nx_ast::shape::child_bodies(s) {
                    index_fns(module, b, out);
                }
            }
        }
    }
}

/// All assigned names in a body, recursively (excluding nested fn scopes,
/// which are planned as their own functions). One of three identical
/// collectors; the set itself lives in `nx_ast::shape`.
fn assigned_in(body: &[Stmt], out: &mut HashSet<String>) {
    nx_ast::shape::assigned_names(body, out);
}
