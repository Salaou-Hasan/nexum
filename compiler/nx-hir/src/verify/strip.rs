//! Diagnostic stripping: the same program with every diagnostic name
//! erased. Used by the V12 strip test: verification must not depend on
//! any original spelling.

use crate::model::*;

/// The same program with every diagnostic name erased. Used by the V12
/// strip test: verification must not depend on any original spelling.
pub fn strip_diag(p: &HProgram) -> HProgram {
    let mut q = p.clone();
    for f in &mut q.funcs {
        f.diag = DiagInfo::default();
        block_strip(&mut f.body);
    }
    for t in &mut q.types {
        t.diag = DiagInfo::default();
    }
    for m in &mut q.methods {
        m.diag = DiagInfo::default();
    }
    for g in &mut q.globals {
        g.diag = DiagInfo::default();
    }
    for m in &mut q.modules {
        m.diag = DiagInfo::default();
    }
    q
}

fn block_strip(b: &mut [HStmt]) {
    for s in b {
        s.diag = DiagInfo::default();
        match &mut s.kind {
            HStmtKind::Assign {
                targets, values, ..
            } => {
                for v in values {
                    expr_strip(v);
                }
                for t in targets {
                    target_strip(t);
                }
            }
            HStmtKind::AssignOp { target, value, .. } => {
                target_strip(target);
                expr_strip(value);
            }
            HStmtKind::Print { values } => values.iter_mut().for_each(expr_strip),
            HStmtKind::If {
                cond,
                then_body,
                elifs,
                else_body,
            } => {
                expr_strip(cond);
                block_strip(then_body);
                for (c, b) in elifs {
                    expr_strip(c);
                    block_strip(b);
                }
                if let Some(b) = else_body {
                    block_strip(b);
                }
            }
            HStmtKind::While { cond, body } => {
                expr_strip(cond);
                block_strip(body);
            }
            HStmtKind::ForRange {
                start, end, body, ..
            } => {
                expr_strip(start);
                expr_strip(end);
                block_strip(body);
            }
            HStmtKind::ForEach { iter, body, .. } => {
                expr_strip(iter);
                block_strip(body);
            }
            HStmtKind::Return { values } => values.iter_mut().for_each(expr_strip),
            HStmtKind::Del { targets } => {
                for t in targets {
                    target_strip(&mut t.target);
                }
            }
            HStmtKind::Assert { cond, message } => {
                expr_strip(cond);
                if let Some(m) = message {
                    expr_strip(m);
                }
            }
            HStmtKind::Expr(e) => expr_strip(e),
            HStmtKind::EnsureInit { .. } | HStmtKind::Break | HStmtKind::Continue => {}
        }
    }
}

fn target_strip(t: &mut HTarget) {
    match t {
        HTarget::Index { base, index, .. } => {
            expr_strip(base);
            expr_strip(index);
        }
        HTarget::Field { base, .. } => expr_strip(base),
        HTarget::Slot(_) | HTarget::Global(_) => {}
    }
}

fn expr_strip(e: &mut HExpr) {
    e.diag = DiagInfo::default();
    match &mut e.kind {
        HExprKind::List(items) => items.iter_mut().for_each(expr_strip),
        HExprKind::Range { start, end, .. } => {
            expr_strip(start);
            expr_strip(end);
        }
        HExprKind::Dict(pairs) => {
            for (k, v) in pairs {
                expr_strip(k);
                expr_strip(v);
            }
        }
        HExprKind::Field { base, .. } => expr_strip(base),
        HExprKind::Index { base, index, .. } => {
            expr_strip(base);
            expr_strip(index);
        }
        HExprKind::Slice {
            base,
            from,
            to,
            step,
            ..
        } => {
            expr_strip(base);
            for b in [from, to, step].into_iter().flatten() {
                expr_strip(b);
            }
        }
        HExprKind::Unary { operand, .. } => expr_strip(operand),
        HExprKind::Binary { left, right, .. }
        | HExprKind::Equal { left, right, .. }
        | HExprKind::Compare { left, right, .. }
        | HExprKind::Logic { left, right, .. } => {
            expr_strip(left);
            expr_strip(right);
        }
        HExprKind::Contains { needle, hay, .. } => {
            expr_strip(needle);
            expr_strip(hay);
        }
        HExprKind::Select {
            cond,
            then_value,
            else_value,
        } => {
            expr_strip(cond);
            expr_strip(then_value);
            expr_strip(else_value);
        }
        HExprKind::Compr {
            element,
            iter,
            cond,
            ..
        } => {
            expr_strip(iter);
            expr_strip(element);
            if let Some(c) = cond {
                expr_strip(c);
            }
        }
        HExprKind::CallFn { args, .. } | HExprKind::Builtin { args, .. } => {
            args.iter_mut().for_each(expr_strip)
        }
        HExprKind::Construct { args, .. } => args.iter_mut().for_each(expr_strip),
        HExprKind::CallMethod {
            receiver,
            args,
            writeback,
            ..
        } => {
            if let Some(r) = receiver {
                expr_strip(r);
            }
            args.iter_mut().for_each(expr_strip);
            if let Some(w) = writeback {
                target_strip(w);
            }
        }
        HExprKind::Int(_)
        | HExprKind::Float(_)
        | HExprKind::Bool(_)
        | HExprKind::Str(_)
        | HExprKind::None
        | HExprKind::Place(_) => {}
    }
}
