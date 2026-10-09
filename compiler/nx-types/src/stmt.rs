//! Statement checking.

use crate::arith_result;
use crate::pred::{compatible, is_keyable, target_label};
use crate::{FnInfo, MethodInfo, Ty};
use nx_ast::{Stmt, Target};

use crate::checker::Checker;

impl Checker {
    pub(crate) fn check_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Assign {
                targets,
                values,
                span,
            } => {
                // Several targets against one value is destructuring: the
                // value is a tuple or a multiple return, so every target
                // takes the same type here. When the parts genuinely differ
                // the recorded type is unresolved and each target stays
                // dynamic, which is correct and merely unspecialised.
                if targets.len() > 1 && values.len() == 1 {
                    let t = self.check_expr(&values[0]);
                    for target in targets {
                        self.bind_target(target, t.clone(), &values[0], *span);
                    }
                    return;
                }
                if targets.len() != values.len() {
                    self.err(
                        *span,
                        format!("{} targets but {} values", targets.len(), values.len()),
                    );
                    return;
                }
                for (target, value) in targets.iter().zip(values.iter()) {
                    let t = self.check_expr(value);
                    self.bind_target(target, t, value, *span);
                }
            }
            Stmt::TypeDecl { name, fields, span } => {
                // A declaration is a compile-time fact about the module, so
                // it is only meaningful at module level. Inside a function
                // it would be a second, incompatible layout for the same
                // name, which is exactly what a static type system cannot
                // represent.
                if self.in_function {
                    self.err(
                        *span,
                        format!("type '{name}' must be declared at module level"),
                    );
                    return;
                }
                if self.records.contains_key(name) {
                    self.err(*span, format!("type '{name}' already declared"));
                    return;
                }
                if self.funcs.contains_key(name) {
                    self.err(
                        *span,
                        format!("'{name}' is already a function; a type cannot share the name"),
                    );
                    return;
                }
                // Field types stay as written and resolve lazily at each
                // use, so a field may name a record declared later --
                // mutually recursive types included. Unknown names are
                // reported once the whole module has been seen.
                let layout: Vec<(String, String)> = fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty.clone()))
                    .collect();
                self.records.insert(name.clone(), layout);
                self.record_spans.insert(name.clone(), *span);
            }
            Stmt::Del { targets, span } => {
                for target in targets {
                    match target {
                        Target::Name(name) => {
                            if self.vars.remove(name).is_none() {
                                self.err(*span, format!("undefined variable '{name}'"));
                            }
                        }
                        Target::Index { base, index } => {
                            let bt = self.check_expr(base);
                            // An unresolved base may hold a list or a dict,
                            // so a scalar key is accepted for either.
                            if matches!(bt, Ty::Unknown) {
                                let kt = self.check_expr(index);
                                if !is_keyable(&kt) {
                                    self.err(
                                        index.span(),
                                        format!("index must be Int or a dict key, found {kt}"),
                                    );
                                }
                            } else if matches!(bt, Ty::Dict(_)) {
                                let kt = self.check_expr(index);
                                if !matches!(
                                    kt,
                                    Ty::Int | Ty::Float | Ty::Bool | Ty::Str | Ty::Unknown
                                ) {
                                    self.err(
                                        index.span(),
                                        format!(
                                            "dict key must be Int, Float, Bool or Str, found {kt}"
                                        ),
                                    );
                                }
                            } else {
                                let it = self.check_expr(index);
                                if !matches!(it, Ty::Int | Ty::Unknown) {
                                    self.err(
                                        index.span(),
                                        format!("index must be Int, found {it}"),
                                    );
                                }
                                // Strings are indexable but immutable, so
                                // there is nothing to delete from one.
                                if matches!(bt, Ty::Str) {
                                    self.err(base.span(), "strings are immutable".to_string());
                                } else if !matches!(bt, Ty::List(_) | Ty::Unknown) {
                                    self.err(
                                        base.span(),
                                        format!("cannot delete an index of {bt}"),
                                    );
                                }
                            }
                        }
                        Target::Attr { base, field } => {
                            let bt = self.check_expr(base);
                            match bt {
                                Ty::Record(t) => {
                                    // The field has to exist; what it
                                    // currently holds is irrelevant to
                                    // whether removing it is meaningful.
                                    if let Some(fields) = self.records.get(&t) {
                                        if !fields.iter().any(|(n, _)| n == field) {
                                            self.err(
                                                base.span(),
                                                format!("type '{t}' has no field '{field}'"),
                                            );
                                        }
                                    }
                                }
                                Ty::Unknown => {}
                                other => self
                                    .err(base.span(), format!("cannot delete a field of {other}")),
                            }
                        }
                    }
                }
            }
            Stmt::Assert { cond, message, .. } => {
                let t = self.check_expr(cond);
                if !matches!(t, Ty::Bool | Ty::Unknown) {
                    self.err(
                        cond.span(),
                        format!("assert condition must be Bool, found {t}"),
                    );
                }
                self.expect_param(cond, Ty::Bool);
                if let Some(m) = message {
                    let mt = self.check_expr(m);
                    if !matches!(mt, Ty::Str | Ty::Unknown) {
                        self.err(m.span(), format!("assert message must be Str, found {mt}"));
                    }
                }
            }
            Stmt::AssignOp {
                target,
                op,
                value,
                span,
            } => {
                let rhs = self.check_expr(value);
                let label = target_label(target);
                if let Some(cur) = self.target_ty(target, *span) {
                    if let Some(res) = arith_result(&cur, *op, &rhs) {
                        if !compatible(&cur, &res) {
                            self.err(
                                *span,
                                format!(
                                    "cannot apply '{}=' of {rhs} to {cur} {label}",
                                    op.as_str()
                                ),
                            );
                        }
                    } else {
                        self.err(
                            *span,
                            format!(
                                "operator '{}' not supported for {cur} and {rhs}",
                                op.as_str()
                            ),
                        );
                    }
                }
            }
            Stmt::Print { values, .. } => {
                for v in values {
                    self.check_expr(v);
                }
            }
            Stmt::If {
                cond,
                then_body,
                elifs,
                else_body,
                ..
            } => {
                let t = self.check_expr(cond);
                if !matches!(t, Ty::Bool | Ty::Unknown) {
                    self.err(cond.span(), format!("condition must be Bool, found {t}"));
                }
                self.expect_param(cond, Ty::Bool);
                self.check_block(then_body);
                for (ec, eb) in elifs {
                    let t = self.check_expr(ec);
                    if !matches!(t, Ty::Bool | Ty::Unknown) {
                        self.err(ec.span(), format!("condition must be Bool, found {t}"));
                    }
                    self.expect_param(ec, Ty::Bool);
                    self.check_block(eb);
                }
                if let Some(b) = else_body {
                    self.check_block(b);
                }
            }
            Stmt::While { cond, body, .. } => {
                let t = self.check_expr(cond);
                if !matches!(t, Ty::Bool | Ty::Unknown) {
                    self.err(cond.span(), format!("condition must be Bool, found {t}"));
                }
                self.expect_param(cond, Ty::Bool);
                self.loop_depth += 1;
                self.check_block(body);
                self.loop_depth -= 1;
            }
            Stmt::For {
                var,
                iter,
                body,
                span,
            } => {
                let elem = match iter {
                    nx_ast::ForIter::Range { start, end } => {
                        let s = self.check_expr(start);
                        let e = self.check_expr(end);
                        if !matches!(s, Ty::Int | Ty::Unknown) {
                            self.err(start.span(), format!("range start must be Int, found {s}"));
                        }
                        if !matches!(e, Ty::Int | Ty::Unknown) {
                            self.err(end.span(), format!("range end must be Int, found {e}"));
                        }
                        self.expect_param(start, Ty::Int);
                        self.expect_param(end, Ty::Int);
                        Ty::Int
                    }
                    nx_ast::ForIter::Each(e) => match self.check_expr(e) {
                        Ty::List(t) => *t,
                        Ty::Str => Ty::Str,
                        // Iterating a dict yields its keys, in insertion
                        // order. The key type is not tracked -- keys may be
                        // any mix of scalars -- so the loop variable stays
                        // dynamic, which is correct and merely unspecialised.
                        Ty::Dict(_) => Ty::Unknown,
                        Ty::Unknown => Ty::Unknown,
                        other => {
                            self.err(e.span(), format!("cannot iterate over {other}"));
                            Ty::Unknown
                        }
                    },
                };
                // Loop var follows the same monomorphic rule.
                match self.vars.get(var).cloned() {
                    None => {
                        if self.records.contains_key(var) {
                            self.err(
                                *span,
                                format!(
                                    "'{var}' is already a type; a variable cannot share the name"
                                ),
                            );
                        } else {
                            self.vars.insert(var.clone(), elem);
                        }
                    }
                    Some(old) if compatible(&old, &elem) => {}
                    Some(old) => self.err(
                        *span,
                        format!("loop variable '{var}' is {old}, cannot iterate {elem}"),
                    ),
                }
                self.loop_depth += 1;
                self.check_block(body);
                self.loop_depth -= 1;
            }
            Stmt::Fn {
                name,
                params,
                body,
                span,
            } => {
                if self.funcs.contains_key(name) {
                    self.err(*span, format!("function '{name}' already defined"));
                    return;
                }
                if self.records.contains_key(name) {
                    self.err(
                        *span,
                        format!("'{name}' is already a type; a function cannot share the name"),
                    );
                    return;
                }
                // A parameter shadowing a type would silently change what
                // `T(...)` means inside the body, so it is refused up front.
                for p in params {
                    if self.records.contains_key(p) {
                        self.err(*span, format!("parameter '{p}' shadows type '{p}'"));
                    }
                }
                // Stub first so the body can call itself recursively.
                self.funcs
                    .insert(name.clone(), (vec![Ty::Unknown; params.len()], Ty::Unknown));
                let (param_tys, ret, fn_locals) = self.check_fn_like(params, body, *span, None);
                let module = self.module_name.clone();
                self.inferred.insert(
                    (module, name.clone()),
                    FnInfo {
                        locals: fn_locals,
                        params: params.clone(),
                        ret: ret.clone(),
                    },
                );
                self.funcs.insert(name.clone(), (param_tys, ret));
            }
            Stmt::Impl {
                type_name,
                methods,
                span,
            } => {
                if self.in_function {
                    self.err(*span, "impl blocks must be at module level".to_string());
                    return;
                }
                // The orphan rule: an impl lives with its type. Imported
                // types carry no declaration span, which is what tells them
                // apart from types declared here.
                if !self.records.contains_key(type_name) {
                    self.err(*span, format!("unknown type '{type_name}'"));
                    return;
                }
                if !self.record_spans.contains_key(type_name) {
                    self.err(
                        *span,
                        format!("cannot implement type '{type_name}' from another module"),
                    );
                    return;
                }
                let canon = self.canonical_name(type_name);
                for m in methods {
                    let key = (canon.clone(), m.name.clone());
                    if self.methods.contains_key(&key) {
                        self.err(
                            m.span,
                            format!("method '{}' already defined for type '{canon}'", m.name),
                        );
                        continue;
                    }
                    for p in &m.params {
                        if self.records.contains_key(p) {
                            self.err(m.span, format!("parameter '{p}' shadows type '{p}'"));
                        }
                    }
                    // Stub first so the body can call itself recursively.
                    self.methods.insert(
                        key.clone(),
                        MethodInfo {
                            params: vec![Ty::Unknown; m.params.len()],
                            ret: Ty::Unknown,
                            receiver: m.receiver,
                            module: self.module_name.clone(),
                        },
                    );
                    let recv = match m.receiver {
                        nx_ast::ReceiverKind::None => None,
                        kind => Some((canon.clone(), kind)),
                    };
                    let (param_tys, ret, fn_locals) =
                        self.check_fn_like(&m.params, &m.body, m.span, recv);
                    // A `mut self` method writes its result back into the
                    // receiver (`p = m(p, ...)`), so it must return the
                    // record: anything else would clobber the receiver with
                    // the wrong type.
                    if m.receiver == nx_ast::ReceiverKind::Mut && ret != Ty::Record(canon.clone()) {
                        self.err(
                            m.span,
                            format!("mut method '{}' must return '{canon}', found {ret}", m.name),
                        );
                    }
                    let info = MethodInfo {
                        params: param_tys,
                        ret: ret.clone(),
                        receiver: m.receiver,
                        module: self.module_name.clone(),
                    };
                    self.methods.insert(key.clone(), info.clone());
                    let module = self.module_name.clone();
                    self.inferred.insert(
                        (module, nx_ast::shape::method_key(&canon, &m.name)),
                        FnInfo {
                            locals: fn_locals,
                            params: m.params.clone(),
                            ret,
                        },
                    );
                }
                let _ = span;
            }
            Stmt::Return { values, span } => {
                if !self.in_function {
                    self.err(*span, "return outside function".to_string());
                    return;
                }

                if values.is_empty() {
                    self.returns.push(Ty::None);
                    return;
                }
                if values.len() == 1 {
                    let t = self.check_expr(&values[0]);
                    self.returns.push(t);
                    return;
                }
                // `return a, b` produces a list, so the function's type is
                // `List(T)`. Recording it as one entry rather than several
                // keeps a mixed tuple (`return 1, "x"`) from being reported
                // as an inconsistent return, while still pinning `T` when
                // the parts agree so destructuring can stay specialised.
                let mut elem = Ty::Unknown;
                for v in values {
                    let t = self.check_expr(v);
                    elem = match elem {
                        Ty::Unknown => t,
                        prev if compatible(&prev, &t) && prev != Ty::Unknown => prev,
                        _ => Ty::Unknown,
                    };
                }
                self.returns.push(Ty::List(Box::new(elem)));
            }

            Stmt::Break { span } => {
                if self.loop_depth == 0 {
                    self.err(*span, "break outside loop".to_string());
                }
            }
            Stmt::Continue { span } => {
                if self.loop_depth == 0 {
                    self.err(*span, "continue outside loop".to_string());
                }
            }
            Stmt::Import {
                module,
                alias,
                span,
            } => {
                if self.check_module(module, *span) {
                    let bind = alias.clone().unwrap_or_else(|| module.clone());
                    if self.records.contains_key(&bind) {
                        self.err(
                            *span,
                            format!("'{bind}' is already a type; a variable cannot share the name"),
                        );
                        return;
                    }

                    self.vars.insert(bind, Ty::Module(module.clone()));
                } else {
                    // The load error is already reported; bind the name
                    // to Unknown so uses stay silent instead of piling
                    // "undefined variable" cascades onto the root cause.
                    // Plain insert, not define(): poisoning must never
                    // itself error, whatever was bound before.
                    let bind = alias.clone().unwrap_or_else(|| module.clone());
                    self.vars.insert(bind.clone(), Ty::Unknown);
                    self.poisoned_names.insert(bind);
                }
            }
            Stmt::FromImport {
                module,
                names,
                span,
            } => {
                if !self.check_module(module, *span) {
                    // Same story as a failed `import`: the load error
                    // stands alone, and every requested name goes quiet
                    // in value position (constructor and associated calls
                    // included: an unbound callee checks as Unknown, and
                    // the poisoned carve-out covers the rest). Layouts
                    // are not faked -- `records` stays untouched, so
                    // `impl T` on a failed import keeps today's error
                    // rather than methods landing on a phantom layout.
                    for (name, alias) in names {
                        let bind = alias.clone().unwrap_or_else(|| name.clone());
                        self.vars.insert(bind.clone(), Ty::Unknown);
                        self.poisoned_names.insert(bind);
                    }
                    return;
                }
                let info = self.modules.get(module).cloned().unwrap_or_default();
                for (name, alias) in names {
                    let bind = alias.clone().unwrap_or_else(|| name.clone());

                    if let Some(t) = info.vars.get(name) {
                        self.define(&bind, t.clone(), *span);
                    } else if let Some((p, r)) = info.funcs.get(name) {
                        self.define(&bind, Ty::Func(p.clone(), Box::new(r.clone())), *span);
                    } else if let Some(layout) = info.types.get(name) {
                        // Importing a type brings its layout, keyed under
                        // the alias when one is given, so `from m import T
                        // as U` followed by `U(...)` resolves. The canonical
                        // name is registered too: `U(...)` constructs
                        // `Ty::Record("T")`, and field access on the result
                        // has to find the layout under that name. The
                        // type's methods come along the same way.
                        self.records.insert(bind.clone(), layout.clone());
                        self.records.insert(name.clone(), layout.clone());
                        if bind != *name {
                            self.type_alias.insert(bind, name.clone());
                        }
                        for ((t, m), minfo) in &info.methods {
                            if t == name {
                                self.methods.insert((t.clone(), m.clone()), minfo.clone());
                            }
                        }
                    } else {
                        self.err(*span, format!("module '{module}' has no member '{name}'"));
                    }
                }
            }
            Stmt::Expr(e) => {
                self.check_expr(e);
            }
        }
    }
}
