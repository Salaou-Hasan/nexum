//! Memory planner for Nexum (`nx-mem`).
//!
//! Decides per function-local binding how it is stored:
//! - `Stack`: a scalar, held in a register or a typed stack slot. No
//!   buffer to own, so no free is ever emitted and nothing can dangle.
//! - `Unique`: owns heap buffers, single owner, freed at function exit
//!   (all `ret` paths) and before reassignment.
//! - `Shared`: escapes (globals, returns, retained params, aliases,
//!   ambiguous cases) — lives for the process lifetime, as before.
//!
//! Soundness rule: doubt means Shared. A wrong Unique would be
//! use-after-free; a wrong Shared only costs memory. `Stack` is only
//! chosen when the value is provably a scalar, so it can never dangle.
//!
//! v0 limits: params are Shared unless proven scalar (caller owns the
//! buffers); for-each loop vars are Shared (they alias list elements);
//! analysis is per function with a fixpoint over retains-summaries; no
//! cross-module inference beyond direct same-module calls (unknown
//! callees retain).

use std::collections::{HashMap, HashSet};
use nx_ast::{Expr, Program, Stmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alloc {
    /// Scalar: register or typed stack slot, never freed.
    Stack,
    Unique,
    Shared,
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    /// (module, function, variable) -> allocation strategy.
    pub locals: HashMap<(String, String, String), Alloc>,
    /// (module, function) -> true if the function may retain its params.
    pub retains: HashMap<(String, String), bool>,
}

impl Plan {
    pub fn alloc_of(&self, module: &str, func: &str, var: &str) -> Alloc {
        self.locals
            .get(&(module.to_string(), func.to_string(), var.to_string()))
            .copied()
            .unwrap_or(Alloc::Shared)
    }
}

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
                        if params.contains(v) || aliases.get(v).map(|s| s.iter().any(|a| params.contains(a))).unwrap_or(false) {
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
    let mut plan = Plan { locals: HashMap::new(), retains: retains.clone() };
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
            plan.locals.insert((module.clone(), name.clone(), var), alloc);
        }
        // for-each loop vars alias list elements: always Shared.
        for s in body {
            if let Stmt::For { var, iter, .. } = s {
                if matches!(iter, nx_ast::ForIter::Each(_)) {
                    plan.locals.insert(
                        (module.clone(), name.clone(), var.clone()),
                        Alloc::Shared,
                    );
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
            Stmt::Fn { name, params, body, .. } => {
                out.insert((module.to_string(), name.clone()), (params.clone(), body.clone()));
                index_fns(module, body, out);
            }
            Stmt::If { then_body, elifs, else_body, .. } => {
                index_fns(module, then_body, out);
                for (_, b) in elifs {
                    index_fns(module, b, out);
                }
                if let Some(b) = else_body {
                    index_fns(module, b, out);
                }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } => {
                index_fns(module, body, out);
            }
            _ => {}
        }
    }
}

/// All assigned names in a body, recursively (excluding nested fn scopes,
/// which are planned as their own functions).
fn assigned_in(body: &[Stmt], out: &mut HashSet<String>) {
    for s in body {
        match s {
            Stmt::Assign { targets, .. } => {
                for t in targets {
                    if let nx_ast::Target::Name(n) = t {
                        out.insert(n.clone());
                    }
                }
            }
            Stmt::For { var, body, .. } => {
                out.insert(var.clone());
                assigned_in(body, out);
            }
            Stmt::If { then_body, elifs, else_body, .. } => {
                assigned_in(then_body, out);
                for (_, b) in elifs {
                    assigned_in(b, out);
                }
                if let Some(b) = else_body {
                    assigned_in(b, out);
                }
            }
            Stmt::While { body, .. } => assigned_in(body, out),
            _ => {}
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Root {
    Var(String),
    ArgTo { callee: Callee, var: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Callee {
    Same(String),
    Attr(String, String),
    Other,
}

/// Static callee resolution for the planner.
fn resolve_callee(
    module: &str,
    callee: &Callee,
    fns: &HashMap<(String, String), (Vec<String>, Vec<Stmt>)>,
) -> Option<(String, String)> {
    match callee {
        Callee::Same(n) => {
            if fns.contains_key(&(module.to_string(), n.clone())) {
                Some((module.to_string(), n.clone()))
            } else {
                // Builtins and unknown names: unknown targets count as
                // retaining (conservative) at the call site.
                None
            }
        }
        Callee::Attr(m, f) => {
            if fns.contains_key(&(m.clone(), f.clone())) {
                Some((m.clone(), f.clone()))
            } else {
                None
            }
        }
        Callee::Other => None,
    }
}

/// Copy-alias pairs `x = y` in a body (flow-insensitive, recursive).
fn aliases_in(body: &[Stmt], params: &[String]) -> HashMap<String, HashSet<String>> {
    let mut map: HashMap<String, HashSet<String>> = HashMap::new();
    for p in params {
        map.entry(p.clone()).or_default();
    }
    collect_alias_pairs(body, &mut map);
    map
}

fn collect_alias_pairs(body: &[Stmt], map: &mut HashMap<String, HashSet<String>>) {
    for s in body {
        match s {
            Stmt::Assign { targets, values, .. } => {
                // `x = y` aliases; `a[i] = y` writes into a container and
                // does not rebind a name, so it creates no alias.
                if targets.len() == 1 && values.len() == 1 {
                    if let (nx_ast::Target::Name(name), Expr::Var(y, _)) = (&targets[0], &values[0]) {
                        map.entry(name.clone()).or_default().insert(y.clone());
                        map.entry(y.clone()).or_default().insert(name.clone());
                    }
                }
            }
            Stmt::If { then_body, elifs, else_body, .. } => {
                collect_alias_pairs(then_body, map);
                for (_, b) in elifs {
                    collect_alias_pairs(b, map);
                }
                if let Some(b) = else_body {
                    collect_alias_pairs(b, map);
                }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } => {
                collect_alias_pairs(body, map);
            }
            _ => {}
        }
    }
}

/// Roots that escape a function body: returned vars, pushed vars,
/// list-literal vars, and (callee, arg-var) pairs for every call.
fn escaping_roots(body: &[Stmt], out: &mut HashSet<Root>) {
    for s in body {
        match s {
            Stmt::Return { values, .. } => {
                for e in values {
                    for v in vars_in(e) {
                        out.insert(Root::Var(v));
                    }
                }
            }
            Stmt::Assign { targets, values, .. } => {
                // A multiple assignment reads every source, so any of them
                // can reach the caller through a returned tuple.
                for e in values {
                    for v in vars_in(e) {
                        out.insert(Root::Var(v));
                    }
                }
                for t in targets {
                    if let nx_ast::Target::Index { base, .. } = t {
                        for v in vars_in(base) {
                            out.insert(Root::Var(v));
                        }
                    }
                }
            }
            Stmt::Del { targets, .. } => {
                // `del a[i]` hands the container's storage onward; `del a`
                // only drops a binding.
                for t in targets {
                    if let nx_ast::Target::Index { base, .. } = t {
                        for v in vars_in(base) {
                            out.insert(Root::Var(v));
                        }
                    }
                }
            }
            Stmt::Assert { cond, message, .. } => {
                for e in [Some(cond), message.as_ref()].into_iter().flatten() {
                    expr_roots(e, out);
                }
            }
            Stmt::If { cond, then_body, elifs, else_body, .. } => {
                escaping_roots(then_body, out);
                for (_, b) in elifs {
                    escaping_roots(b, out);
                }
                if let Some(b) = else_body {
                    escaping_roots(b, out);
                }
                let _ = cond;
            }
            Stmt::While { body, .. } => escaping_roots(body, out),
            Stmt::For { body, .. } => escaping_roots(body, out),
            Stmt::Parallel { tasks, .. } => {
                for t in tasks {
                    escaping_roots(std::slice::from_ref(t), out);
                }
            }
            // A type declaration binds nothing at runtime, so it
            // contributes no roots.
            Stmt::TypeDecl { .. } => {}
            Stmt::Fn { .. } => {}
            Stmt::Expr(e) => expr_roots(e, out),
            Stmt::AssignOp { .. } => {}
            Stmt::Print { .. } | Stmt::Import { .. } | Stmt::FromImport { .. } => {}
            Stmt::Break { .. } | Stmt::Continue { .. } => {}
        }
    }
}

fn expr_roots(e: &Expr, out: &mut HashSet<Root>) {
    match e {
        Expr::Call { callee, args, .. } => {
            let c = match callee.as_ref() {
                Expr::Var(n, _) if n == "len" || n == "push" || n == "print" => None,
                Expr::Var(n, _) => Some(Callee::Same(n.clone())),
                Expr::Attr { base, attr, .. } => match base.as_ref() {
                    Expr::Var(m, _) => Some(Callee::Attr(m.clone(), attr.clone())),
                    _ => Some(Callee::Other),
                },
                _ => Some(Callee::Other),
            };
            // push(lst, v): first arg is retained by the list.
            if let Expr::Var(n, _) = callee.as_ref() {
                if n == "push" {
                    if let Some(Expr::Var(v, _)) = args.first() {
                        out.insert(Root::Var(v.clone()));
                    }
                    return;
                }
            }
            if let Some(c) = c {
                for a in args {
                    for v in vars_in(a) {
                        out.insert(Root::ArgTo { callee: c.clone(), var: v });
                    }
                }
            }
        }
        Expr::List(items, _) => {
            for it in items {
                for v in vars_in(it) {
                    out.insert(Root::Var(v));
                }
            }
        }
        Expr::Binary { left, right, .. } => {
            expr_roots(left, out);
            expr_roots(right, out);
        }
        Expr::Unary { expr, .. } => expr_roots(expr, out),
        Expr::Index { base, index, .. } => {
            expr_roots(base, out);
            expr_roots(index, out);
        }
        Expr::Attr { base, .. } => expr_roots(base, out),
        _ => {}
    }
}

fn vars_in(e: &Expr) -> Vec<String> {
    match e {
        Expr::Var(n, _) => vec![n.clone()],
        Expr::Binary { left, right, .. } => {
            let mut v = vars_in(left);
            v.extend(vars_in(right));
            v
        }
        Expr::Unary { expr, .. } => vars_in(expr),
        Expr::Index { base, index, .. } => {
            let mut v = vars_in(base);
            v.extend(vars_in(index));
            v
        }
        Expr::Call { callee, args, .. } => {
            let mut v = vars_in(callee);
            for a in args {
                v.extend(vars_in(a));
            }
            v
        }
        Expr::Attr { base, .. } => vars_in(base),
        Expr::List(items, _) => items.iter().flat_map(vars_in).collect(),
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_src(src: &str) -> Plan {
        plan_with(src, &HashMap::new())
    }

    /// Plan with the checker's inferred types, which is what promotes
    /// scalars to `Stack`.
    fn plan_with(src: &str, types: &HashMap<(String, String), nx_types::FnInfo>) -> Plan {
        let prog = nx_parser::parse_source(src).unwrap_or_else(|e| panic!("{e}"));
        let mut map = HashMap::new();
        map.insert("__main__".to_string(), prog);
        plan(&map, "__main__", types)
    }

    fn infer(src: &str) -> HashMap<(String, String), nx_types::FnInfo> {
        let prog = nx_parser::parse_source(src).unwrap_or_else(|e| panic!("{e}"));
        nx_types::infer_program(&prog, std::path::Path::new("."))
            .unwrap_or_else(|es| panic!("{es:?}"))
    }

    #[test]
    fn scalar_local_is_stack() {
        let src = "fn f(n):\n    t = n * 2\n    print(t)\n";
        let types = infer(src);
        let p = plan_with(src, &types);
        assert_eq!(p.alloc_of("__main__", "f", "t"), Alloc::Stack);
    }

    #[test]
    fn list_local_is_not_stack() {
        let src = "fn f():\n    a = [1]\n    print(a)\n";
        let types = infer(src);
        let p = plan_with(src, &types);
        assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Unique);
    }

    #[test]
    fn stack_beats_escape() {
        // Returning a scalar copies the number, so it cannot dangle.
        let src = "fn f(n):\n    t = n * 2\n    return t + 1\n";
        let types = infer(src);
        let p = plan_with(src, &types);
        assert_eq!(p.alloc_of("__main__", "f", "t"), Alloc::Stack);
    }

    #[test]
    fn stack_beats_aliasing() {
        // `a` and `b` are both Int, and a scalar copy is a number rather
        // than a second reference to one buffer, so neither dangles.
        let src = "fn f(n):\n    a = n * 1\n    b = a\n    print(b)\n";
        let types = infer(src);
        let p = plan_with(src, &types);
        assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Stack);
        assert_eq!(p.alloc_of("__main__", "f", "b"), Alloc::Stack);
    }

    #[test]
    fn list_copy_is_not_stack() {
        // The same shape with a list must still be Shared: the copy shares
        // a buffer, so freeing it would be a double free.
        let src = "fn f():\n    a = [1]\n    b = a\n    print(b)\n";
        let types = infer(src);
        let p = plan_with(src, &types);
        assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Shared);
        assert_eq!(p.alloc_of("__main__", "f", "b"), Alloc::Shared);
    }

    #[test]
    fn unknown_typed_local_is_not_stack() {
        // Without type information nothing may be promoted: doubt is Shared.
        let src = "fn f(n):\n    t = n * 2\n    print(t)\n";
        let p = plan_src(src);
        assert_eq!(p.alloc_of("__main__", "f", "t"), Alloc::Unique);
    }

    #[test]
    fn float_and_bool_locals_are_stack() {
        let src = "fn f(n):\n    a = n / 2.0\n    b = n < 1\n    print(a, b)\n";
        let types = infer(src);
        let p = plan_with(src, &types);
        assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Stack);
        assert_eq!(p.alloc_of("__main__", "f", "b"), Alloc::Stack);
    }

    #[test]
    fn local_temp_is_unique() {
        let p = plan_src("fn f(n):\n    t = n * 2\n    return t + 1\n");
        assert_eq!(p.alloc_of("__main__", "f", "t"), Alloc::Shared); // returned transitively
        assert_eq!(p.alloc_of("__main__", "f", "n"), Alloc::Shared); // param
    }

    #[test]
    fn pure_temp_freed() {
        let p = plan_src("fn f(n):\n    t = n * 2\n    print(t)\n");
        assert_eq!(p.alloc_of("__main__", "f", "t"), Alloc::Unique);
    }

    #[test]
    fn pushed_list_shared() {
        let p = plan_src("fn f():\n    a = [1]\n    push(a, 2)\n    return a\n");
        assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Shared);
    }

    #[test]
    fn alias_shared() {
        let p = plan_src("fn f():\n    a = [1]\n    b = a\n    print(b)\n");
        assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Shared);
        assert_eq!(p.alloc_of("__main__", "f", "b"), Alloc::Shared);
    }

    #[test]
    fn retaining_fn_poisons_caller() {
        let p = plan_src("fn keep(x):\n    g = [x]\n    return g\nfn f():\n    a = [1]\n    b = keep(a)\n    print(b)\n");
        assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Shared);
    }
}