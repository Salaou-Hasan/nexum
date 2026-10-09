//! Expression walk: the per-node dispatch over [`HExprKind`], checking
//! each node's local shape and delegating operand rules to [`rules`]
//! and calls to the statement walk.

use crate::model::*;

use super::compatible;
use super::FnCtx;
use super::Verifier;

impl<'a> Verifier<'a> {
    pub(crate) fn expr(&mut self, e: &HExpr, ctx: &mut FnCtx) {
        match &e.kind {
            HExprKind::Int(_) => self.exact(e, ctx, HTy::Int),
            HExprKind::Float(_) => self.exact(e, ctx, HTy::Float),
            HExprKind::Bool(_) => self.exact(e, ctx, HTy::Bool),
            HExprKind::Str(_) => self.exact(e, ctx, HTy::Str),
            HExprKind::None => self.exact(e, ctx, HTy::None),
            HExprKind::List(items) => {
                for i in items {
                    self.expr(i, ctx);
                }
            }
            HExprKind::Range { start, end, .. } => {
                self.expr(start, ctx);
                self.expr(end, ctx);
                self.int_bound(start, e.span);
                self.int_bound(end, e.span);
                self.exact_elem(e, HTy::Int);
            }
            HExprKind::Dict(pairs) => {
                for (k, v) in pairs {
                    self.expr(k, ctx);
                    self.expr(v, ctx);
                }
            }
            HExprKind::Place(p) => self.place(p.clone(), e, ctx),
            HExprKind::Field { base, field } => {
                self.expr(base, ctx);
                self.field(base, *field, e.span);
                // A statically resolved field knows its type: the node
                // must carry it.
                if let (HTy::Record(_), FieldRef::Static(idx)) = (&base.ty, field) {
                    let want = self.fields_of(&base.ty).and_then(|f| f.get(idx.0)).cloned();
                    if let Some(want) = want {
                        if !compatible(&e.ty, &want) {
                            self.err(
                                "V5",
                                e.span,
                                format!("field {} is {} not {}", idx.0, e.ty, want),
                            );
                        }
                    }
                }
            }
            HExprKind::Index { base, index, rule } => {
                self.expr(base, ctx);
                self.expr(index, ctx);
                self.index(base, index, *rule, e.span);
            }
            HExprKind::Slice {
                base,
                from,
                to,
                step,
                rule,
            } => {
                self.expr(base, ctx);
                if !matches!(
                    rule,
                    SliceRule::ListCopy | SliceRule::StrChars | SliceRule::Dynamic
                ) {
                    self.err("V2", e.span, "unknown slice rule");
                }
                if !matches!(base.ty, HTy::List(_) | HTy::Str | HTy::Unknown) {
                    self.err("V10", e.span, format!("cannot slice {}", base.ty));
                }
                for b in [from, to, step].into_iter().flatten() {
                    self.expr(b, ctx);
                    self.int_bound(b, e.span);
                }
                match (&base.ty, rule) {
                    (HTy::List(t), SliceRule::ListCopy) => self.exact_elem(e, (**t).clone()),
                    (HTy::Str, SliceRule::StrChars) => self.exact(e, ctx, HTy::Str),
                    _ => {}
                }
            }
            HExprKind::Unary { op, rule, operand } => {
                self.expr(operand, ctx);
                self.unary_rule(*rule, *op, operand, e);
            }
            HExprKind::Binary {
                left,
                op,
                rule,
                right,
            } => {
                self.expr(left, ctx);
                self.expr(right, ctx);
                self.bin_rule(*rule, &left.ty, &right.ty, e.span);
                let want = match rule {
                    // A dynamic *rule* does not mean a dynamic *type*:
                    // the checker still knows that `x | y` is Int even
                    // when `x` is unresolved, so the node type is only
                    // forced where the rule pins it.
                    BinRule::Arith(ArithRule::Trap) | BinRule::Bitwise => Some(HTy::Int),
                    BinRule::Arith(ArithRule::Float) | BinRule::Arith(ArithRule::PromoteFloat) => {
                        Some(HTy::Float)
                    }
                    BinRule::Pow(PowRule::Saturate) => Some(HTy::Int),
                    BinRule::Concat => Some(HTy::Str),
                    BinRule::Arith(ArithRule::Dynamic)
                    | BinRule::Pow(PowRule::Dynamic)
                    | BinRule::Dynamic => None,
                };
                if let Some(want) = want {
                    self.exact(e, ctx, want);
                }
                let _ = op;
            }
            HExprKind::Equal {
                left, right, rule, ..
            } => {
                self.expr(left, ctx);
                self.expr(right, ctx);
                self.exact(e, ctx, HTy::Bool);
                self.eq_rule(*rule, &left.ty, &right.ty, e.span);
            }
            HExprKind::Compare {
                left, right, rule, ..
            } => {
                self.expr(left, ctx);
                self.expr(right, ctx);
                self.exact(e, ctx, HTy::Bool);
                self.cmp_rule(*rule, &left.ty, &right.ty, e.span);
            }
            HExprKind::Contains {
                needle, hay, rule, ..
            } => {
                self.expr(needle, ctx);
                self.expr(hay, ctx);
                self.exact(e, ctx, HTy::Bool);
                match (rule, &hay.ty) {
                    (MemberRule::ListEq, HTy::List(_)) | (MemberRule::ListEq, HTy::Unknown) => {}
                    (MemberRule::StrSub, HTy::Str) | (MemberRule::StrSub, HTy::Unknown) => {}
                    (MemberRule::DictKey, HTy::Dict(_)) | (MemberRule::DictKey, HTy::Unknown) => {}
                    (MemberRule::Dynamic, _) => {}
                    _ => self.err(
                        "V2",
                        e.span,
                        format!("membership rule does not fit {}", hay.ty),
                    ),
                }
            }
            HExprKind::Logic { left, right, .. } => {
                self.expr(left, ctx);
                self.expr(right, ctx);
                self.exact(e, ctx, HTy::Bool);
            }
            HExprKind::Select {
                cond,
                then_value,
                else_value,
            } => {
                self.cond(cond, ctx, e.span);
                self.expr(then_value, ctx);
                self.expr(else_value, ctx);
                if !compatible(&then_value.ty, &else_value.ty) {
                    self.err(
                        "V4",
                        e.span,
                        format!("branches disagree: {} and {}", then_value.ty, else_value.ty),
                    );
                }
            }
            HExprKind::Compr {
                element,
                var,
                iter,
                rule,
                cond,
            } => {
                self.expr(iter, ctx);
                ctx.bind(*var);
                self.expr(element, ctx);
                if let Some(c) = cond {
                    self.cond(c, ctx, e.span);
                }
                ctx.unbind(*var);
                // A comprehension over a string *is* a string: iterating
                // characters yields one joined string, exactly as the
                // checker types it. Iterating a list or dict keys
                // yields a list.
                match (rule, &iter.ty) {
                    (IterRule::List, HTy::List(_)) | (IterRule::DictKeys, HTy::Dict(_)) => {
                        self.exact_elem(e, element.ty.clone())
                    }
                    (IterRule::StrChars, HTy::Str) => self.exact(e, ctx, HTy::Str),
                    _ => {}
                }
            }
            HExprKind::CallFn { func, args } => {
                if self.func(*func).is_none() {
                    self.err("V2", e.span, format!("call of missing function {}", func.0));
                } else {
                    let want = self.func(*func).expect("checked").params.len();
                    if args.len() != want {
                        self.err(
                            "V2",
                            e.span,
                            format!("call passes {} of {} args", args.len(), want),
                        );
                    }
                }
                for a in args {
                    self.expr(a, ctx);
                }
            }
            HExprKind::Construct { type_id, args } => {
                match self.record(*type_id).cloned() {
                    None => self.err(
                        "V2",
                        e.span,
                        format!("construct of missing type {}", type_id.0),
                    ),
                    Some(t) => {
                        // V7: a constructor fills every field, or it is
                        // not a constructor.
                        if args.len() != t.fields.len() {
                            self.err(
                                "V7",
                                e.span,
                                format!(
                                    "{} args for a {}-field record",
                                    args.len(),
                                    t.fields.len()
                                ),
                            );
                        }
                        for (a, want) in args.iter().zip(t.fields.iter()) {
                            self.expr(a, ctx);
                            if !compatible(&a.ty, want) {
                                self.err("V7", e.span, format!("field is {} not {}", a.ty, want));
                            }
                        }
                        self.exact(e, ctx, HTy::Record(*type_id));
                    }
                }
            }
            HExprKind::Builtin { op, args } => {
                let want: Vec<usize> = match op {
                    BuiltinOp::Len => vec![1],
                    BuiltinOp::Push => vec![2],
                    BuiltinOp::Input => vec![0, 1],
                    BuiltinOp::ToInt | BuiltinOp::ToFloat => vec![1],
                };
                if !want.contains(&args.len()) {
                    self.err(
                        "V2",
                        e.span,
                        format!("builtin takes {} args, got {}", want.len(), args.len()),
                    );
                }
                for a in args {
                    self.expr(a, ctx);
                }
                // `push` mutates its first argument in place, so that
                // argument must be a place -- pushing into a temporary
                // would drop the result.
                if matches!(op, BuiltinOp::Push) {
                    if let Some(first) = args.first() {
                        if !matches!(
                            first.kind,
                            HExprKind::Place(_) | HExprKind::Index { .. } | HExprKind::Field { .. }
                        ) {
                            self.err("V2", e.span, "push() target is not a place");
                        }
                    }
                }
                let ret = match op {
                    BuiltinOp::Len => HTy::Int,
                    BuiltinOp::Push => HTy::None,
                    BuiltinOp::Input => HTy::Str,
                    BuiltinOp::ToInt => HTy::Int,
                    BuiltinOp::ToFloat => HTy::Float,
                };
                self.exact(e, ctx, ret);
            }
            HExprKind::CallMethod {
                method,
                receiver,
                args,
                writeback,
            } => self.method_call(
                e,
                *method,
                receiver.as_deref(),
                args,
                writeback.as_ref(),
                ctx,
            ),
        }
    }
}
