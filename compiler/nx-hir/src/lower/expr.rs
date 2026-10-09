//! Expression lowering.

use super::scope::FnLower;
use super::tables::NameRef;
use super::ty::{conv_ty, decide_bin_rule};
use super::{lerr, LResult};
use crate::model::*;
use nx_ast::{BinOp, Expr, Span, UnaryOp};

impl<'a> FnLower<'a> {
    /// Lower one expression.
    pub(crate) fn lower_expr(&mut self, e: &Expr) -> LResult<HExpr> {
        match e {
            Expr::Int(v, sp) => Ok(self.expr(*sp, None, HTy::Int, HExprKind::Int(*v))),
            Expr::Float(v, sp) => Ok(self.expr(*sp, None, HTy::Float, HExprKind::Float(*v))),
            Expr::Bool(v, sp) => Ok(self.expr(*sp, None, HTy::Bool, HExprKind::Bool(*v))),
            Expr::Str(v, sp) => Ok(self.expr(*sp, None, HTy::Str, HExprKind::Str(v.clone()))),
            Expr::NoneLit(sp) => Ok(self.expr(*sp, None, HTy::None, HExprKind::None)),
            Expr::List(items, sp) => {
                let mut hs = Vec::with_capacity(items.len());
                let mut elem = HTy::Unknown;
                for it in items {
                    let h = self.lower_expr(it)?;
                    if matches!(elem, HTy::Unknown) {
                        elem = h.ty.clone();
                    }
                    hs.push(h);
                }
                let ty = HTy::List(Box::new(elem));
                Ok(self.expr(*sp, None, ty, HExprKind::List(hs)))
            }
            Expr::Range { start, end, span } => {
                let s = self.lower_expr(start)?;
                let t = self.lower_expr(end)?;
                let ty = HTy::List(Box::new(HTy::Int));
                Ok(self.expr(
                    *span,
                    None,
                    ty,
                    HExprKind::Range {
                        start: Box::new(s),
                        end: Box::new(t),
                        rule: RangeRule::AscendingOrEmpty,
                    },
                ))
            }
            Expr::Dict(pairs, sp) => {
                let mut hp = Vec::with_capacity(pairs.len());
                let mut vt = HTy::Unknown;
                for (k, v) in pairs {
                    let hk = self.lower_expr(k)?;
                    let hv = self.lower_expr(v)?;
                    if matches!(vt, HTy::Unknown) {
                        vt = hv.ty.clone();
                    }
                    hp.push((hk, hv));
                }
                let ty = HTy::Dict(Box::new(vt));
                Ok(self.expr(*sp, None, ty, HExprKind::Dict(hp)))
            }
            Expr::Var(name, sp) => self.lower_var(name, *sp),
            Expr::Attr { base, attr, span } => self.lower_attr(base, attr, *span),
            Expr::Index { base, index, span } => {
                let b = self.lower_expr(base)?;
                let ix = self.lower_expr(index)?;
                let rule = self.index_rule(&b.ty, *span)?;
                let ty = self.index_elem_ty(&b.ty, *span)?;
                Ok(self.expr(
                    *span,
                    None,
                    ty,
                    HExprKind::Index {
                        base: Box::new(b),
                        index: Box::new(ix),
                        rule,
                    },
                ))
            }
            Expr::Slice {
                base,
                from,
                to,
                step,
                span,
            } => {
                let b = self.lower_expr(base)?;
                let rule = self.slice_rule(&b.ty, *span)?;
                let ty = self.slice_ty(&b.ty, *span)?;
                let hf = self.opt_expr(from)?;
                let ht = self.opt_expr(to)?;
                let hs = self.opt_expr(step)?;
                Ok(self.expr(
                    *span,
                    None,
                    ty,
                    HExprKind::Slice {
                        base: Box::new(b),
                        from: hf,
                        to: ht,
                        step: hs,
                        rule,
                    },
                ))
            }
            Expr::Unary {
                op,
                expr: operand,
                span,
            } => {
                let h = self.lower_expr(operand)?;
                let rule = self.unary_rule(*op, &h.ty, *span)?;
                // Mirrors the checker: `not` answers Bool on a known Bool
                // and stays dynamic on an unknown operand.
                let ty = match (op, &h.ty) {
                    (UnaryOp::Not, HTy::Unknown) => HTy::Unknown,
                    (UnaryOp::Not, _) => HTy::Bool,
                    _ => h.ty.clone(),
                };
                Ok(self.expr(
                    *span,
                    None,
                    ty,
                    HExprKind::Unary {
                        op: *op,
                        rule,
                        operand: Box::new(h),
                    },
                ))
            }
            Expr::Binary {
                left,
                op,
                right,
                span,
            } => self.lower_binary(left, *op, right, *span),
            Expr::IfExpr {
                cond,
                then_value,
                else_value,
                span,
            } => {
                let c = self.lower_expr(cond)?;
                let t = self.lower_expr(then_value)?;
                let f = self.lower_expr(else_value)?;
                let ty = self.join_ty(&t.ty, &f.ty, *span)?;
                Ok(self.expr(
                    *span,
                    None,
                    ty,
                    HExprKind::Select {
                        cond: Box::new(c),
                        then_value: Box::new(t),
                        else_value: Box::new(f),
                    },
                ))
            }
            Expr::Comprehension {
                element,
                var,
                iter,
                cond,
                span,
            } => {
                let it = self.lower_expr(iter)?;
                let rule = self.iter_rule(&it.ty, *span)?;
                let ety = self.iter_elem_ty(&it.ty, *span)?;
                let slot = self.alloc_slot(ety);
                let saved = self.env.insert(var.clone(), NameRef::Slot(slot));
                let el = self.lower_expr(element)?;
                let hc = self.opt_expr(cond)?;
                self.restore_env(var, saved);
                // Mirrors the checker: a comprehension over a string
                // *is* a string (one character per element), while a
                // list or dict-keys comprehension builds a list.
                let ty = match (&it.ty, rule) {
                    (HTy::Str, IterRule::StrChars) => HTy::Str,
                    (HTy::List(_), IterRule::List) => HTy::List(Box::new(el.ty.clone())),
                    (HTy::Dict(_), IterRule::DictKeys) => HTy::List(Box::new(HTy::Unknown)),
                    _ => HTy::List(Box::new(el.ty.clone())),
                };
                Ok(self.expr(
                    *span,
                    Some(var),
                    ty,
                    HExprKind::Compr {
                        element: Box::new(el),
                        var: slot,
                        iter: Box::new(it),
                        rule,
                        cond: hc,
                    },
                ))
            }
            Expr::Call { callee, args, span } => self.lower_call(callee, args, *span),
        }
    }

    /// One binary operator, split across four nodes because four
    /// relations answer different questions with different rules:
    /// `and`/`or` short-circuit, `==`/`!=` compare structurally,
    /// orderings order, `in`/`not in` test membership.
    pub(crate) fn lower_binary(
        &mut self,
        left: &Expr,
        op: BinOp,
        right: &Expr,
        span: Span,
    ) -> LResult<HExpr> {
        use BinOp::*;
        let l = self.lower_expr(left)?;
        let r = self.lower_expr(right)?;
        let kind = match op {
            And | Or => HExprKind::Logic {
                op,
                left: Box::new(l),
                right: Box::new(r),
            },
            Eq | NotEq => {
                let rule = self.eq_rule(&l.ty, &r.ty, span)?;
                HExprKind::Equal {
                    left: Box::new(l),
                    op,
                    rule,
                    right: Box::new(r),
                }
            }
            Lt | LtEq | Gt | GtEq => {
                let rule = self.cmp_rule(&l.ty, &r.ty, span)?;
                HExprKind::Compare {
                    left: Box::new(l),
                    op,
                    rule,
                    right: Box::new(r),
                }
            }
            In | NotIn => {
                let rule = self.member_rule(&r.ty, span)?;
                HExprKind::Contains {
                    needle: Box::new(l),
                    hay: Box::new(r),
                    rule,
                    negated: matches!(op, NotIn),
                }
            }
            _ => {
                let rule = decide_bin_rule(op, &l.ty, &r.ty, span)?;
                let ty = self.bin_result_ty(op, &l.ty, &r.ty, span)?;
                return Ok(self.expr(
                    span,
                    None,
                    ty,
                    HExprKind::Binary {
                        left: Box::new(l),
                        op,
                        rule,
                        right: Box::new(r),
                    },
                ));
            }
        };
        Ok(self.expr(span, None, HTy::Bool, kind))
    }

    /// A field read. A module base reads the module's global (a module
    /// is not a value, so nothing evaluates); a record base resolves to
    /// a constant offset; an unknown base keeps the interned name for
    /// runtime lookup. Every other base shape was rejected.
    pub(crate) fn lower_attr(&mut self, base: &Expr, attr: &str, span: Span) -> LResult<HExpr> {
        if let Some(mid) = self.module_base(base)? {
            let home = self.ctx.tables.modules_sorted[mid.0 as usize].clone();
            let gid = self
                .ctx
                .global_ids
                .get(&(home.clone(), attr.to_string()))
                .copied()
                .ok_or_else(|| lerr(span, format!("internal: '{home}' has no member '{attr}'")))?;
            let ty = self.global_ty(gid);
            return Ok(self.expr(span, Some(attr), ty, HExprKind::Place(Place::Global(gid))));
        }
        let b = self.lower_expr(base)?;
        let field = self.field_ref(&b.ty, attr, span)?;
        let ty = self.field_ty_of(&b.ty, field, span)?;
        Ok(self.expr(
            span,
            Some(attr),
            ty,
            HExprKind::Field {
                base: Box::new(b),
                field,
            },
        ))
    }

    /// The module a bare name refers to, if any.
    pub(crate) fn module_base(&self, base: &Expr) -> LResult<Option<ModuleId>> {
        let Expr::Var(name, span) = base else {
            return Ok(None);
        };
        if let Some(NameRef::Module(mid)) = self.env.get(name) {
            return Ok(Some(*mid));
        }
        match self.ctx.tables.module_id.get(name) {
            Some(mid) => Ok(Some(*mid)),
            None => {
                let _ = span;
                Ok(None)
            }
        }
    }

    /// A type visible here by bare name.
    pub(crate) fn visible_type(&self, name: &str) -> Option<TypeId> {
        if let Some(NameRef::Type(tid)) = self.env.get(name) {
            return Some(*tid);
        }
        self.ctx
            .visible_types
            .get(&(self.module.clone(), name.to_string()))
            .copied()
    }

    /// A function visible here by bare name.
    pub(crate) fn visible_func(&self, name: &str) -> Option<FuncId> {
        if let Some(NameRef::Func(fid)) = self.env.get(name) {
            return Some(*fid);
        }
        self.ctx
            .visible_funcs
            .get(&(self.module.clone(), name.to_string()))
            .copied()
    }

    /// The declared return type of a function body, from the checker's
    /// inference for its key.
    pub(crate) fn func_ret(&self, fid: FuncId, span: Span) -> LResult<HTy> {
        let decl = self
            .ctx
            .tables
            .func_decls
            .get(fid.0 as usize)
            .ok_or_else(|| lerr(span, format!("internal: no such function id {}", fid.0)))?;
        let info = self.ctx.fn_info(&decl.module, &decl.name)?;
        conv_ty(self.ctx.tables, &info.ret, &decl.module, decl.span)
    }

    /// A method body key is its canonical type plus the method name,
    /// mirroring the checker's inference keys. The declaring module
    /// comes from the body the method points at, so a from-imported
    /// type's methods are typed against their own module's inference.
    pub(crate) fn method_ret(&self, mid: MethodId, span: Span) -> LResult<HTy> {
        let decl = self
            .ctx
            .tables
            .method_decls
            .get(mid.0 as usize)
            .ok_or_else(|| lerr(span, format!("internal: no such method id {}", mid.0)))?;
        let body = self
            .ctx
            .tables
            .func_decls
            .get(decl.func.0 as usize)
            .ok_or_else(|| {
                lerr(
                    span,
                    format!("internal: no such function id {}", decl.func.0),
                )
            })?;
        let canon = self.type_name(decl.type_id);
        let info = self
            .ctx
            .fn_info(&body.module, &nx_ast::shape::method_key(&canon, &decl.name))?;
        conv_ty(self.ctx.tables, &info.ret, &body.module, span)
    }
}
