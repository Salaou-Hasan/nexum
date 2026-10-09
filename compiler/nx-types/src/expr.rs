//! Expression checking and inference.

use crate::arith_result;
use crate::pred::{compatible, field_ty, is_keyable, is_numeric};
use crate::Ty;
use nx_ast::{BinOp, Expr, UnaryOp};

use crate::checker::Checker;

impl Checker {
    pub(crate) fn check_expr(&mut self, expr: &Expr) -> Ty {
        let t = self.check_expr_inner(expr);
        if t == Ty::Float {
            // A float can appear in an expression without ever landing in
            // a local, as in `fn half(x): return x / 2.0`. Remember it so
            // the parameter rule below can see the function is float-shaped.
            self.saw_float = true;
        }
        t
    }

    pub(crate) fn check_expr_inner(&mut self, expr: &Expr) -> Ty {
        match expr {
            Expr::Int(..) => Ty::Int,
            Expr::Float(..) => Ty::Float,
            Expr::Bool(..) => Ty::Bool,
            Expr::Str(..) => Ty::Str,
            Expr::NoneLit(..) => Ty::None,
            Expr::Range { start, end, span } => {
                let s = self.check_expr(start);
                let e = self.check_expr(end);
                for (t, node) in [(&s, &**start), (&e, &**end)] {
                    if !matches!(t, Ty::Int | Ty::Unknown) {
                        self.err(node.span(), format!("range bound must be Int, found {t}"));
                    }
                    self.expect_param(node, Ty::Int);
                }
                let _ = span;
                // A range is a list of Ints, whatever the bounds turned out
                // to be, so it composes with everything list-shaped.
                Ty::List(Box::new(Ty::Int))
            }
            Expr::Dict(pairs, span) => {
                // Keys must be comparable, or the dict cannot be looked up.
                // Values may be anything; that is what makes a dict the
                // natural heterogeneous container.
                for (k, _) in pairs {
                    let kt = self.check_expr(k);
                    if !matches!(kt, Ty::Int | Ty::Float | Ty::Bool | Ty::Str | Ty::Unknown) {
                        self.err(
                            k.span(),
                            format!("dict key must be Int, Float, Bool or Str, found {kt}"),
                        );
                    }
                }
                let _ = span;
                Ty::Dict(Box::new(Ty::Unknown))
            }
            Expr::Slice {
                base,
                from,
                to,
                step,
                span,
            } => {
                let b = self.check_expr(base);
                for part in [from, to, step].into_iter().flatten() {
                    let t = self.check_expr(part);
                    if !matches!(t, Ty::Int | Ty::Unknown) {
                        self.err(part.span(), format!("slice bound must be Int, found {t}"));
                    }
                    self.expect_param(part, Ty::Int);
                }
                match b {
                    Ty::List(_) => b,
                    Ty::Str => Ty::Str,
                    Ty::Unknown => Ty::Unknown,
                    other => {
                        self.err(*span, format!("cannot slice {other}"));
                        Ty::Unknown
                    }
                }
            }
            Expr::IfExpr {
                cond,
                then_value,
                else_value,
                ..
            } => {
                let c = self.check_expr(cond);
                if !matches!(c, Ty::Bool | Ty::Unknown) {
                    self.err(cond.span(), format!("condition must be Bool, found {c}"));
                }
                self.expect_param(cond, Ty::Bool);
                let t = self.check_expr(then_value);
                let e = self.check_expr(else_value);
                if compatible(&t, &e) {
                    if t == Ty::Unknown {
                        e
                    } else {
                        t
                    }
                } else if t == Ty::None {
                    // `x if c else None` is the optional idiom, so the
                    // None branch must not be an error.
                    e
                } else if e == Ty::None {
                    t
                } else {
                    self.err(then_value.span(), format!("branches disagree: {t} vs {e}"));
                    Ty::Unknown
                }
            }
            Expr::Comprehension {
                element,
                var,
                iter,
                cond,
                span,
            } => {
                let it = self.check_expr(iter);
                let elem = match it {
                    Ty::List(t) => *t,
                    Ty::Str => Ty::Str,
                    Ty::Unknown => Ty::Unknown,
                    other => {
                        self.err(iter.span(), format!("cannot iterate over {other}"));
                        Ty::Unknown
                    }
                };
                // The loop variable is scoped to the comprehension, so it is
                // bound here and restored after rather than leaking out and
                // colliding with a same-named variable elsewhere.
                let saved = self.vars.get(var).cloned();
                if saved.is_none() && self.records.contains_key(var) {
                    self.err(
                        iter.span(),
                        format!("'{var}' is already a type; a variable cannot share the name"),
                    );
                } else {
                    self.vars.insert(var.clone(), elem);
                }
                if let Some(c) = cond {
                    let ct = self.check_expr(c);
                    if !matches!(ct, Ty::Bool | Ty::Unknown) {
                        self.err(c.span(), format!("filter must be Bool, found {ct}"));
                    }
                }
                let et = self.check_expr(element);
                match saved {
                    Some(t) => {
                        self.vars.insert(var.clone(), t);
                    }
                    None => {
                        self.vars.remove(var);
                    }
                }
                let _ = span;
                Ty::List(Box::new(et))
            }
            Expr::List(items, span) => {
                let mut elem = Ty::Unknown;
                for it in items {
                    let t = self.check_expr(it);
                    if elem == Ty::Unknown {
                        elem = t;
                    } else if !compatible(&elem, &t) {
                        self.err(*span, format!("mixed list element types: {elem} vs {t}"));
                        break;
                    }
                }
                Ty::List(Box::new(elem))
            }
            Expr::Var(name, span) => {
                if let Some(t) = self.vars.get(name).cloned() {
                    return t;
                }
                if let Some((p, r)) = self.funcs.get(name).cloned() {
                    return Ty::Func(p, Box::new(r));
                }
                self.err(*span, format!("undefined variable '{name}'"));
                Ty::Unknown
            }
            Expr::Attr { base, attr, span } => {
                let b = self.check_expr(base);
                match b {
                    Ty::Module(m) => match self.modules.get(&m).cloned() {
                        Some(info) => {
                            if let Some(t) = info.vars.get(attr) {
                                return t.clone();
                            }
                            if let Some((p, r)) = info.funcs.get(attr) {
                                return Ty::Func(p.clone(), Box::new(r.clone()));
                            }
                            self.err(*span, format!("module '{m}' has no member '{attr}'"));
                            Ty::Unknown
                        }
                        None => {
                            self.err(*span, format!("unknown module '{m}'"));
                            Ty::Unknown
                        }
                    },
                    // A known record resolves the field statically, which is
                    // what lets the backend use a constant offset.
                    Ty::Record(t) => match self.records.get(&t).cloned() {
                        Some(fields) => {
                            let recs = self.records.clone();
                            match fields.iter().find(|(n, _)| n == attr) {
                                Some((_, ft)) => field_ty(ft, &recs),
                                None => {
                                    self.err(*span, format!("type '{t}' has no field '{attr}'"));
                                    Ty::Unknown
                                }
                            }
                        }
                        None => Ty::Unknown,
                    },
                    // A dynamic base is resolved by name at runtime, so the
                    // field type is genuinely unknown here.
                    Ty::Unknown => Ty::Unknown,
                    other => {
                        self.err(
                            *span,
                            format!("attribute access needs a module or a type, found {other}"),
                        );
                        Ty::Unknown
                    }
                }
            }
            Expr::Index { base, index, span } => {
                let b = self.check_expr(base);
                // A dict is keyed by value, so its index need not be an Int.
                // An unresolved base may hold either a list or a dict, so a
                // scalar key is accepted and the runtime dispatches on the
                // actual tag; anything else is rejected either way.
                if matches!(b, Ty::Dict(_) | Ty::Unknown) {
                    let ix = self.check_expr(index);
                    if !is_keyable(&ix) {
                        self.err(
                            index.span(),
                            format!("index must be Int or a dict key, found {ix}"),
                        );
                    }
                    return match b {
                        Ty::Dict(t) => *t,
                        _ => Ty::Unknown,
                    };
                }
                let ix = self.check_expr(index);
                if !matches!(ix, Ty::Int | Ty::Unknown) {
                    self.err(index.span(), format!("index must be Int, found {ix}"));
                }
                self.expect_param(index, Ty::Int);
                match b {
                    Ty::List(t) => *t,
                    Ty::Str => Ty::Str,
                    Ty::Unknown => Ty::Unknown,
                    other => {
                        self.err(
                            *span,
                            format!(
                                "only lists, strings and dicts support indexing, found {other}"
                            ),
                        );
                        Ty::Unknown
                    }
                }
            }
            Expr::Unary { op, expr, span } => {
                let t = self.check_expr(expr);
                match (op, &t) {
                    (UnaryOp::Neg, Ty::Int) => Ty::Int,
                    (UnaryOp::Neg, Ty::Float) => Ty::Float,
                    (UnaryOp::Neg, Ty::Unknown) => Ty::Unknown,
                    (UnaryOp::Not, Ty::Bool) => Ty::Bool,
                    (UnaryOp::Not, Ty::Unknown) => Ty::Unknown,
                    // `~x` complements bits, so it is Int-only. Accepting a
                    // Float here would mean inventing a rounding rule.
                    (UnaryOp::BitNot, Ty::Int) => Ty::Int,
                    (UnaryOp::BitNot, Ty::Unknown) => Ty::Unknown,
                    // Unary plus preserves the type and nothing else.
                    (UnaryOp::Pos, Ty::Int) | (UnaryOp::Pos, Ty::Float) => t,
                    (UnaryOp::Pos, Ty::Unknown) => Ty::Unknown,
                    _ => {
                        self.err(
                            *span,
                            format!("operator '{}' not supported for {t}", op.as_str()),
                        );
                        Ty::Unknown
                    }
                }
            }
            Expr::Binary {
                left,
                op,
                right,
                span,
            } => {
                let l = self.check_expr(left);
                let r = self.check_expr(right);
                match op {
                    BinOp::And | BinOp::Or => {
                        for (side, t) in [("left", &l), ("right", &r)] {
                            if !matches!(t, Ty::Bool | Ty::Unknown) {
                                self.err(
                                    *span,
                                    format!(
                                        "'{0}' operand of '{1}' must be Bool, found {t}",
                                        side,
                                        op.as_str()
                                    ),
                                );
                            }
                        }
                        self.expect_param(left, Ty::Bool);
                        self.expect_param(right, Ty::Bool);
                        Ty::Bool
                    }
                    BinOp::Eq | BinOp::NotEq => {
                        if !(compatible(&l, &r) || (is_numeric(&l) && is_numeric(&r))) {
                            self.err(*span, format!("cannot compare {l} and {r}"));
                        }
                        // A numeric comparison against a known scalar pins
                        // the other side to the same scalar type.
                        if is_numeric(&l) && l != Ty::Unknown {
                            self.expect_param(right, l.clone());
                        }
                        if is_numeric(&r) && r != Ty::Unknown {
                            self.expect_param(left, r.clone());
                        }
                        Ty::Bool
                    }
                    BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                        if !((is_numeric(&l) && is_numeric(&r))
                            || (matches!(l, Ty::Str | Ty::Unknown)
                                && matches!(r, Ty::Str | Ty::Unknown)))
                        {
                            self.err(*span, format!("cannot order {l} and {r}"));
                        }
                        if is_numeric(&l) && l != Ty::Unknown {
                            self.expect_param(right, l.clone());
                        }
                        if is_numeric(&r) && r != Ty::Unknown {
                            self.expect_param(left, r.clone());
                        }
                        if l == Ty::Str || r == Ty::Str {
                            self.expect_param(left, Ty::Str);
                            self.expect_param(right, Ty::Str);
                        }
                        Ty::Bool
                    }
                    BinOp::In | BinOp::NotIn => {
                        let c = self.check_expr(right);
                        match c {
                            Ty::List(_) | Ty::Str | Ty::Dict(_) | Ty::Unknown => {}
                            other => {
                                self.err(
                                    right.span(),
                                    format!("'{}' needs a list, string or dict on the right, found {other}", op.as_str()),
                                );
                            }
                        }
                        Ty::Bool
                    }
                    BinOp::Shl | BinOp::Shr => {
                        // The shift distance is a count, not a value of the
                        // shifted type, so it is checked separately. This is
                        // what lets `1 << n` work when n is an Int
                        // parameter and the left side is a literal.
                        if !matches!(r, Ty::Int | Ty::Unknown) {
                            self.err(
                                right.span(),
                                format!("shift distance must be Int, found {r}"),
                            );
                        }
                        self.expect_param(right, Ty::Int);
                        if !matches!(l, Ty::Int | Ty::Unknown) {
                            self.err(
                                *span,
                                format!(
                                    "operator '{}' needs Int on the left, found {l}",
                                    op.as_str()
                                ),
                            );
                        }
                        Ty::Int
                    }
                    BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                        for (side, t) in [("left", &l), ("right", &r)] {
                            if !matches!(t, Ty::Int | Ty::Unknown) {
                                self.err(
                                    *span,
                                    format!(
                                        "'{0}' operand of '{1}' must be Int, found {t}",
                                        side,
                                        op.as_str()
                                    ),
                                );
                            }
                        }
                        Ty::Int
                    }
                    BinOp::FloorDiv | BinOp::Mod | BinOp::Pow => {
                        self.mark_numeric(left);
                        self.mark_numeric(right);
                        match arith_result(&l, *op, &r) {
                            Some(t) => t,
                            None => {
                                self.err(
                                    *span,
                                    format!(
                                        "operator '{}' not supported for {l} and {r}",
                                        op.as_str()
                                    ),
                                );
                                Ty::Unknown
                            }
                        }
                    }
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                        if matches!(op, BinOp::Add) && (l == Ty::Str || r == Ty::Str) {
                            self.expect_param(left, Ty::Str);
                            self.expect_param(right, Ty::Str);
                        } else {
                            self.mark_numeric(left);
                            self.mark_numeric(right);
                        }
                        match arith_result(&l, *op, &r) {
                            Some(t) => t,
                            None => {
                                self.err(
                                    *span,
                                    format!(
                                        "operator '{}' not supported for {l} and {r}",
                                        op.as_str()
                                    ),
                                );
                                Ty::Unknown
                            }
                        }
                    }
                }
            }
            Expr::Call { callee, args, span } => {
                // A declared type shadows nothing, but it does share the
                // `Name(...)` spelling with a function, so the type case
                // is resolved first. `m.T(...)` through a module alias
                // resolves the same way, against the module's exports.
                //
                // An alias constructs the canonical name: `from m import T
                // as U` followed by `U(...)` yields `Ty::Record("T")`, so a
                // value built through the alias compares equal to one built
                // through the original name.
                if let Expr::Var(name, _) = callee.as_ref() {
                    if let Some(fields) = self.records.get(name).cloned() {
                        let canon = self.canonical_name(name);
                        // Field types resolve now, against every declaration
                        // in the module -- including later ones.
                        let recs = self.records.clone();
                        let resolved: Vec<(String, Ty)> = fields
                            .iter()
                            .map(|(n, t)| (n.clone(), field_ty(t, &recs)))
                            .collect();
                        self.check_record_args(&canon, &resolved, args, *span);
                        return Ty::Record(canon);
                    }
                }
                if let Expr::Attr { base, attr, .. } = callee.as_ref() {
                    if let Expr::Var(m, _) = base.as_ref() {
                        if let Some(info) = self.modules.get(m).cloned() {
                            if let Some(fields) = info.types.get(attr).cloned() {
                                // Resolve against the declaring module's own
                                // table: its field types name its records.
                                let resolved: Vec<(String, Ty)> = fields
                                    .iter()
                                    .map(|(n, t)| (n.clone(), field_ty(t, &info.types)))
                                    .collect();
                                self.check_record_args(attr, &resolved, args, *span);
                                return Ty::Record(attr.clone());
                            }
                        }
                    }
                }
                // Method, associated-function, and builtin-sugar calls through
                // `base.attr(...)`. Resolution order is load-bearing:
                // modules first (a module never means a value), then
                // associated functions on a type name, then impl methods on
                // a statically known record, and finally builtin sugar
                // (`xs.push(1)` for `push(xs, 1)`), which also covers
                // unresolved bases. Dynamic method dispatch is Stage 4.
                if let Expr::Attr { base, attr, .. } = callee.as_ref() {
                    // `T.m(...)` where T names a type: an associated
                    // function. Tested before the base is evaluated as a
                    // value, because a type name is not a binding -- asking
                    // `check_expr` about one would report it undefined. No
                    // sugar fallback either: the base is not a value, so
                    // anything else is meaningless.
                    if let Expr::Var(n, _) = base.as_ref() {
                        if self.records.contains_key(n) {
                            let canon = self.canonical_name(n);
                            if let Some(info) =
                                self.methods.get(&(canon.clone(), attr.clone())).cloned()
                            {
                                if info.receiver != nx_ast::ReceiverKind::None {
                                    self.err(
                                        *span,
                                        format!("method '{attr}' needs a receiver; call it on a '{canon}' value"),
                                    );
                                    return Ty::Unknown;
                                }
                                self.check_call_args(&info.params, args, *span);
                                return info.ret;
                            }
                            self.err(
                                *span,
                                format!("type '{canon}' has no associated function '{attr}'"),
                            );
                            return Ty::Unknown;
                        }
                    }
                    // A module base is a module function, checked like a
                    // plain call against the module's signature.
                    if let Ty::Module(m) = self.check_expr(base) {
                        if let Some(info) = self.modules.get(&m).cloned() {
                            if let Some((p, r)) = info.funcs.get(attr) {
                                let (p, r) = (p.clone(), r.clone());
                                self.check_call_args(&p, args, *span);
                                return r;
                            }
                        }
                        // A module variable is not callable, same as calling
                        // any other non-function value.
                        for a in args {
                            self.check_expr(a);
                        }
                        self.err(*span, format!("module '{m}' has no function '{attr}'"));
                        return Ty::Unknown;
                    }
                    // `v.m(...)` on a statically known record: an impl
                    // method. Anything less than a record falls through to
                    // builtin sugar below.
                    let bt = self.check_expr(base);
                    if let Ty::Record(t) = bt {
                        if let Some(info) = self.methods.get(&(t.clone(), attr.clone())).cloned() {
                            // A `mut self` call writes its result back into the
                            // receiver when the receiver has storage. With no
                            // storage -- a call result, a literal, anything
                            // temporary -- there is nowhere to write, and the
                            // call still evaluates to the modified record.
                            // That is what lets a chain read as a single
                            // expression: `q.moved(1, 1).moved(2, 2)`.
                            if info.receiver == nx_ast::ReceiverKind::Mut
                                && self.self_root_denied(base)
                            {
                                self.err(
                                    *span,
                                    "cannot call mut method through read-only 'self'".to_string(),
                                );
                            }
                            self.check_call_args(&info.params, args, *span);
                            return info.ret;
                        }
                        self.err(*span, format!("type '{t}' has no method '{attr}'"));
                        return Ty::Unknown;
                    }
                    // Builtin sugar: `xs.push(1)` for `push(xs, 1)`. Reached
                    // only when the base is neither a module, a type, nor a
                    // record -- including unresolved bases, which is what
                    // keeps `x.push(1)` working on dynamic values.
                    if crate::builtin_arity(attr).is_some() {
                        let mut sugared: Vec<Expr> = vec![(**base).clone()];
                        sugared.extend(args.iter().cloned());
                        return self.check_builtin(attr, &sugared, *span);
                    }
                    if matches!(bt, Ty::Unknown) {
                        // A receiver poisoned by a failed import stays
                        // silent: its load error stands alone, and piling
                        // a resolution error on top would bury the root
                        // cause under a contradiction. Anything else
                        // unresolved still errors -- dynamic dispatch
                        // arrives in Stage 4.
                        if self.poisoned_unknown(base) {
                            return Ty::Unknown;
                        }
                        self.err(
                            *span,
                            format!("cannot resolve method '{attr}' on unresolved value (dynamic dispatch arrives in Stage 4)"),
                        );
                    } else {
                        self.err(*span, format!("'{attr}' is not a method; only modules, types and builtins support attribute calls"));
                    }
                    return Ty::Unknown;
                }
                // Builtins.
                if let Expr::Var(name, _) = callee.as_ref() {
                    if crate::builtin_arity(name).is_some() {
                        return self.check_builtin(name, args, *span);
                    }
                }
                let f = self.check_expr(callee);
                match f {
                    Ty::Func(params, ret) => {
                        self.check_call_args(&params, args, *span);
                        *ret
                    }
                    Ty::Unknown => {
                        for a in args {
                            self.check_expr(a);
                        }
                        Ty::Unknown
                    }
                    other => {
                        for a in args {
                            self.check_expr(a);
                        }
                        self.err(*span, format!("not callable: {other}"));
                        Ty::Unknown
                    }
                }
            }
        }
    }
}
