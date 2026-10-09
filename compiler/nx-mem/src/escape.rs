//! Escape analysis for the memory plan.
//!
//! Roots that may outlive a function body (returned vars, retained args,
//! container contents) plus the copy-alias pairs that spread them. Doubt
//! means Shared: an extra root only costs memory, never soundness.

use nx_ast::{Expr, Stmt};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Root {
    Var(String),
    ArgTo { callee: Callee, var: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum Callee {
    Same(String),
    Attr(String, String),
    Other,
}

/// Static callee resolution for the planner.
pub(crate) fn resolve_callee(
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
pub(crate) fn aliases_in(body: &[Stmt], params: &[String]) -> HashMap<String, HashSet<String>> {
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
            Stmt::Assign {
                targets, values, ..
            } => {
                // `x = y` aliases; `a[i] = y` writes into a container and
                // does not rebind a name, so it creates no alias.
                if targets.len() == 1 && values.len() == 1 {
                    if let (nx_ast::Target::Name(name), Expr::Var(y, _)) = (&targets[0], &values[0])
                    {
                        map.entry(name.clone()).or_default().insert(y.clone());
                        map.entry(y.clone()).or_default().insert(name.clone());
                    }
                }
            }
            _ => {
                for b in nx_ast::shape::child_bodies(s) {
                    collect_alias_pairs(b, map);
                }
            }
        }
    }
}

/// Roots that escape a function body: returned vars, pushed vars,
/// list-literal vars, and (callee, arg-var) pairs for every call.
pub(crate) fn escaping_roots(body: &[Stmt], out: &mut HashSet<Root>) {
    for s in body {
        match s {
            Stmt::Return { values, .. } => {
                for e in values {
                    for v in vars_in(e) {
                        out.insert(Root::Var(v));
                    }
                }
            }
            Stmt::Assign {
                targets, values, ..
            } => {
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
            // Only bodies are walked here, as before: conditions and
            // iterables contribute no roots of their own. Binds clone on
            // the way in, so nothing a condition or iterable touches can
            // outlive the scope through this statement alone.
            Stmt::If { .. } | Stmt::While { .. } | Stmt::For { .. } => {
                for b in nx_ast::shape::child_bodies(s) {
                    escaping_roots(b, out);
                }
            }
            // A type declaration binds nothing at runtime, so it
            // contributes no roots.
            Stmt::TypeDecl { .. } => {}
            // Functions and methods are planned under their own keys, not
            // as part of the enclosing body.
            Stmt::Fn { .. } | Stmt::Impl { .. } => {}
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
                // Builtins retain nothing they are given (`push` hands its
                // first argument to the list, handled just below). The set
                // is owned by `nx_ast::shape`; `print` is not one of them
                // (it is a statement, never a call callee) and stays a
                // local exemption.
                Expr::Var(n, _) if nx_ast::shape::builtin_arity(n).is_some() || n == "print" => {
                    None
                }
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
                        out.insert(Root::ArgTo {
                            callee: c.clone(),
                            var: v,
                        });
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
        Expr::Dict(pairs, _) => {
            for (k, v) in pairs {
                expr_roots(k, out);
                expr_roots(v, out);
            }
        }
        Expr::Slice {
            base,
            from,
            to,
            step,
            ..
        } => {
            expr_roots(base, out);
            for bound in [from, to, step].into_iter().flatten() {
                expr_roots(bound, out);
            }
        }
        Expr::IfExpr {
            cond,
            then_value,
            else_value,
            ..
        } => {
            expr_roots(cond, out);
            expr_roots(then_value, out);
            expr_roots(else_value, out);
        }
        Expr::Comprehension {
            element,
            iter,
            cond,
            ..
        } => {
            // The loop variable is not filtered here: an extra root only
            // pushes toward Shared, which is the safe direction.
            expr_roots(element, out);
            expr_roots(iter, out);
            if let Some(c) = cond {
                expr_roots(c, out);
            }
        }
        Expr::Range { start, end, .. } => {
            expr_roots(start, out);
            expr_roots(end, out);
        }
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
        Expr::Dict(pairs, _) => pairs
            .iter()
            .flat_map(|(k, v)| vars_in(k).into_iter().chain(vars_in(v)))
            .collect(),
        Expr::Slice {
            base,
            from,
            to,
            step,
            ..
        } => {
            let mut v = vars_in(base);
            for bound in [from, to, step].into_iter().flatten() {
                v.extend(vars_in(bound));
            }
            v
        }
        Expr::IfExpr {
            cond,
            then_value,
            else_value,
            ..
        } => {
            let mut v = vars_in(cond);
            v.extend(vars_in(then_value));
            v.extend(vars_in(else_value));
            v
        }
        // The loop variable is not filtered: keeping it only pushes
        // toward Shared, which is the safe direction.
        Expr::Comprehension {
            element,
            iter,
            cond,
            ..
        } => {
            let mut v = vars_in(element);
            v.extend(vars_in(iter));
            if let Some(c) = cond {
                v.extend(vars_in(c));
            }
            v
        }
        Expr::Range { start, end, .. } => {
            let mut v = vars_in(start);
            v.extend(vars_in(end));
            v
        }
        _ => vec![],
    }
}
