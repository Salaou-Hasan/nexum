//! Call lowering: functions, builtins, methods, and writeback.

use super::scope::FnLower;
use super::ty::{builtin_op, builtin_ret};
use super::{lerr, LResult};
use crate::model::*;
use nx_ast::shape;
use nx_ast::{Expr, Span};

impl<'a> FnLower<'a> {
    /// A call. Resolution order is the checker's, and it is
    /// load-bearing: a type name shadows nothing but shares the
    /// `Name(...)` spelling with a function; `T.m(...)` resolves before
    /// the base is evaluated, because a type is not a binding; a module
    /// base always means a module function; a known record means an
    /// impl method; and only then does builtin sugar apply.
    pub(crate) fn lower_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        span: Span,
    ) -> LResult<HExpr> {
        // `T(...)`: construction, same spelling as a function call.
        if let Expr::Var(name, _) = callee {
            if let Some(tid) = self.visible_type(name) {
                let hs = self.lower_args(args)?;
                return Ok(self.expr(
                    span,
                    Some(name),
                    HTy::Record(tid),
                    HExprKind::Construct {
                        type_id: tid,
                        args: hs,
                    },
                ));
            }
        }
        if let Expr::Attr { base, attr, .. } = callee {
            // `m.T(...)`: the same construction through a module alias,
            // resolved against the module's own declarations.
            if let Some(mid) = self.module_base(base)? {
                let home = self.ctx.tables.modules_sorted[mid.0 as usize].clone();
                if let Some(tid) = self
                    .ctx
                    .tables
                    .type_id
                    .get(&(home.clone(), attr.clone()))
                    .copied()
                {
                    let hs = self.lower_args(args)?;
                    return Ok(self.expr(
                        span,
                        Some(attr),
                        HTy::Record(tid),
                        HExprKind::Construct {
                            type_id: tid,
                            args: hs,
                        },
                    ));
                }
            }
            // `T.m(...)`: an associated function. Nothing evaluates --
            // a type name has no storage.
            if let Expr::Var(n, _) = base.as_ref() {
                if let Some(tid) = self.visible_type(n) {
                    let mid = self.resolve_method(tid, attr, span)?;
                    let ret = self.method_ret(mid, span)?;
                    let hs = self.lower_args(args)?;
                    return Ok(self.expr(
                        span,
                        Some(attr),
                        ret,
                        HExprKind::CallMethod {
                            method: mid,
                            receiver: None,
                            args: hs,
                            writeback: None,
                        },
                    ));
                }
            }
            // `m.f(...)`: a module function. Modules are not values, so
            // the base contributes nothing.
            if let Some(mid) = self.module_base(base)? {
                let home = self.ctx.tables.modules_sorted[mid.0 as usize].clone();
                let fid = *self
                    .ctx
                    .tables
                    .func_id
                    .get(&(home.clone(), attr.clone()))
                    .ok_or_else(|| {
                        lerr(span, format!("internal: '{home}' has no function '{attr}'"))
                    })?;
                let ret = self.func_ret(fid, span)?;
                let hs = self.lower_args(args)?;
                return Ok(self.expr(
                    span,
                    Some(attr),
                    ret,
                    HExprKind::CallFn {
                        func: fid,
                        args: hs,
                    },
                ));
            }
            // `v.m(...)`: an impl method on a known record, else builtin
            // sugar. The base evaluates exactly once either way.
            let b = self.lower_expr(base)?;
            if let HTy::Record(tid) = b.ty {
                let mid = self.resolve_method(tid, attr, span)?;
                return self.lower_method_call(mid, b, attr, args, span);
            }
            if shape::builtin_arity(attr).is_some() {
                let op = builtin_op(attr);
                let mut hs = Vec::with_capacity(args.len() + 1);
                hs.push(b);
                for a in args {
                    hs.push(self.lower_expr(a)?);
                }
                let ty = builtin_ret(op);
                return Ok(self.expr(span, Some(attr), ty, HExprKind::Builtin { op, args: hs }));
            }
            return Err(lerr(
                span,
                format!("internal: cannot resolve method '{attr}'"),
            ));
        }
        // `f(...)`: an ambient builtin or a visible function.
        if let Expr::Var(name, _) = callee {
            if shape::builtin_arity(name).is_some() {
                let op = builtin_op(name);
                let hs = self.lower_args(args)?;
                let ty = builtin_ret(op);
                return Ok(self.expr(span, Some(name), ty, HExprKind::Builtin { op, args: hs }));
            }
            if let Some(fid) = self.visible_func(name) {
                let ret = self.func_ret(fid, span)?;
                let hs = self.lower_args(args)?;
                return Ok(self.expr(
                    span,
                    Some(name),
                    ret,
                    HExprKind::CallFn {
                        func: fid,
                        args: hs,
                    },
                ));
            }
        }
        Err(lerr(span, "internal: not a call"))
    }

    pub(crate) fn lower_args(&mut self, args: &[Expr]) -> LResult<Vec<HExpr>> {
        let mut hs = Vec::with_capacity(args.len());
        for a in args {
            hs.push(self.lower_expr(a)?);
        }
        Ok(hs)
    }

    /// An impl method call. A `mut self` method writes its result back
    /// into the receiver when the receiver has storage; a temporary
    /// base has nowhere to write, which is what lets
    /// `q.moved(1, 1).moved(2, 2)` read as one expression. The
    /// write-back target reuses the receiver's already-lowered parts, so
    /// base evaluation happens exactly once.
    pub(crate) fn lower_method_call(
        &mut self,
        mid: MethodId,
        receiver: HExpr,
        attr: &str,
        args: &[Expr],
        span: Span,
    ) -> LResult<HExpr> {
        let is_mut = self
            .ctx
            .tables
            .method_decls
            .get(mid.0 as usize)
            .map(|d| matches!(d.receiver, nx_ast::ReceiverKind::Mut))
            .unwrap_or(false);
        let ret = self.method_ret(mid, span)?;
        let hs = self.lower_args(args)?;
        let writeback = if is_mut {
            self.writeback_target(&receiver)
        } else {
            None
        };
        Ok(self.expr(
            span,
            Some(attr),
            ret,
            HExprKind::CallMethod {
                method: mid,
                receiver: Some(Box::new(receiver)),
                args: hs,
                writeback,
            },
        ))
    }

    /// The write position a `mut self` receiver denotes, if it has
    /// storage: a name, an element, or a field. Anything else (a call
    /// result, a literal) is a temporary.
    pub(crate) fn writeback_target(&self, recv: &HExpr) -> Option<HTarget> {
        match &recv.kind {
            HExprKind::Place(Place::Slot(s)) => Some(HTarget::Slot(*s)),
            HExprKind::Place(Place::Global(g)) => Some(HTarget::Global(*g)),
            HExprKind::Index { base, index, rule } => Some(HTarget::Index {
                base: base.clone(),
                index: index.clone(),
                rule: *rule,
            }),
            HExprKind::Field { base, field } => Some(HTarget::Field {
                base: base.clone(),
                field: *field,
            }),
            _ => None,
        }
    }
}
