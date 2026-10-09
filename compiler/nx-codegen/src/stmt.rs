//! Statement emission: control flow and declarations.

use crate::core::{Gen, Term};
use crate::mangle::{mangle_global, mangle_init};
use crate::value::NV;
use crate::{err, CodegenError};
use nx_ast::{Expr, Span, Stmt};
use nx_types::Ty;

impl Gen {
    pub(crate) fn emit_stmt(&mut self, stmt: &Stmt) -> Result<(), CodegenError> {
        if self.term.is_some() {
            return Ok(());
        }
        match stmt {
            Stmt::Assign {
                targets,
                values,
                span,
            } => {
                // Several targets against one value is destructuring.
                if targets.len() > 1 && values.len() == 1 {
                    let v = self.emit_expr(&values[0])?;
                    return self.destructure(&v, targets, *span);
                }
                if targets.len() != values.len() {
                    return Err(err(
                        *span,
                        format!("{} targets but {} values", targets.len(), values.len()),
                    ));
                }
                // All right-hand sides are evaluated before any store, so
                // `a, b = b, a` swaps rather than clobbering.
                let mut computed = Vec::with_capacity(values.len());
                for v in values {
                    computed.push(self.emit_expr(v)?);
                }
                for (i, (t, v)) in targets.iter().zip(computed.iter()).enumerate() {
                    // A plain `a = b` has to carry a module alias across,
                    // or a later `a.f()` would resolve against the wrong
                    // module. Anything else clears it.
                    if let nx_ast::Target::Name(name) = t {
                        match values.get(i) {
                            Some(Expr::Var(y, _)) if targets.len() == values.len() => {
                                if let Some(m) = self.modrefs.get(y).cloned() {
                                    self.modrefs.insert(name.clone(), m);
                                } else {
                                    self.modrefs.remove(name);
                                }
                                self.falias.remove(name);
                            }
                            _ => {
                                self.modrefs.remove(name);
                                self.falias.remove(name);
                            }
                        }
                    }
                    self.store_target(t, v, *span)?;
                }
                Ok(())
            }
            Stmt::AssignOp {
                target,
                op,
                value,
                span,
            } => {
                match target {
                    nx_ast::Target::Name(name) => {
                        let rhs = self.emit_expr(value)?;
                        self.store_compound(name, *op, &rhs, *span)
                    }
                    other => {
                        // Python order with single evaluation: the target's
                        // parts run before the value, and each runs once.
                        // Stash them in hidden `$augN` slots first (`$`
                        // cannot appear in a user identifier, so the names
                        // cannot collide), then read, compute and store off
                        // the slots. Emitting the target inline instead
                        // would run it before the value (reversed) or run
                        // it twice (once to read, once to store).
                        let rebuilt = self.stash_compound_target(other, *span)?;
                        let rhs = self.emit_expr(value)?;
                        let cur = self.emit_expr(&rebuilt.as_expr(*span))?;
                        let v = self.binop_dyn(&cur, *op, &rhs)?;
                        self.store_target(&rebuilt, &v, *span)
                    }
                }
            }
            Stmt::Del { targets, span } => {
                for t in targets {
                    self.del_target(t, *span)?;
                }
                Ok(())
            }
            Stmt::Assert { cond, message, .. } => {
                let c = self.emit_expr(cond)?;
                let b = self.as_i1(&c);
                let ok = self.lab("assert_ok");
                let fail = self.lab("assert_fail");
                let done = self.lab("assert_done");
                self.w(&format!("  br i1 {b}, label %{ok}, label %{fail}"));
                self.block(&format!("{ok}"));
                self.w(&format!("  br label %{done}"));
                // The failing path is a separate block, so an assertion that
                // holds costs one branch and nothing else.
                self.block(&format!("{fail}"));
                match message {
                    Some(m) => {
                        let mv = self.emit_expr(m)?;
                        let mb = self.unbox(&mv);
                        self.w(&format!("  call void @nx_assert_fail_msg(%NxVal {mb})"));
                    }
                    None => self.w("  call void @nx_assert_fail()"),
                }
                self.w("  unreachable");
                self.block(&format!("{done}"));
                Ok(())
            }
            Stmt::TypeDecl { .. } => {
                // A declaration is compile-time only. The layouts are
                // harvested before emission, so there is nothing to emit.
                Ok(())
            }
            Stmt::Impl { .. } => {
                // Methods are harvested before emission and emitted as
                // ordinary functions; there is nothing to emit inline.
                Ok(())
            }
            Stmt::Print { values, .. } => {
                let n = values.len();
                if n == 0 {
                    self.w("  call void @nx_print(ptr null, i64 0)");
                    return Ok(());
                }
                let arr = self.alloca(&format!("[{n} x %NxVal]"));
                for (i, e) in values.iter().enumerate() {
                    let v = self.emit_expr(e)?;
                    let b = self.unbox(&v);
                    let ep = self.reg();
                    self.w(&format!(
                        "  {ep} = getelementptr [{n} x %NxVal], ptr {arr}, i64 0, i64 {i}"
                    ));
                    self.w(&format!("  store %NxVal {b}, ptr {ep}"));
                }
                let p0 = self.reg();
                self.w(&format!(
                    "  {p0} = getelementptr [{n} x %NxVal], ptr {arr}, i64 0, i64 0"
                ));
                self.w(&format!("  call void @nx_print(ptr {p0}, i64 {n})"));
                Ok(())
            }
            Stmt::If {
                cond,
                then_body,
                elifs,
                else_body,
                ..
            } => self.emit_if_full(cond, then_body, elifs, else_body),
            Stmt::While { cond, body, .. } => {
                let condl = self.lab("wcond");
                let bodyl = self.lab("wbody");
                let endl = self.lab("wend");
                self.w(&format!("  br label %{condl}"));
                self.block(&format!("{condl}"));
                let c = self.emit_expr(cond)?;
                let b = self.as_i1(&c);
                self.w(&format!("  br i1 {b}, label %{bodyl}, label %{endl}"));
                self.block(&format!("{bodyl}"));
                self.loops.push((condl.clone(), endl.clone()));
                self.term = None;
                for s in body {
                    self.emit_stmt(s)?;
                    if self.term.is_some() {
                        break;
                    }
                }
                let body_term = self.term.take();
                self.loops.pop();
                match body_term {
                    Some(Term::Ret) => {
                        self.term = Some(Term::Ret);
                        self.block(&format!("{endl}"));
                        self.w("  unreachable");
                    }
                    Some(_) => {
                        // break/continue already branched; no back-edge.
                        self.block(&format!("{endl}"));
                        self.term = None;
                    }
                    None => {
                        self.w(&format!("  br label %{condl}"));
                        self.block(&format!("{endl}"));
                        self.term = None;
                    }
                }
                Ok(())
            }
            Stmt::For {
                var,
                iter,
                body,
                span,
            } => self.emit_for(var, iter, body, *span),
            Stmt::Fn { .. } => Ok(()),
            Stmt::Return { values, .. } => {
                // Free Unique locals on every exit path, then memoize.
                match values.len() {
                    0 => {
                        self.free_scope();
                        self.emit_ret(None);
                    }
                    1 => {
                        let v = self.emit_expr(&values[0])?;
                        // The ABI is boxed, so returns re-box.
                        let b = self.unbox(&v);
                        self.free_scope();
                        self.emit_ret(Some(&b));
                    }
                    // `return a, b` is a list, which is what the caller's
                    // `a, b = f()` destructures. Same shape as the
                    // the caller destructures, so both forms agree.
                    _ => {
                        let n = values.len() as i64;
                        let l = self.reg();
                        self.w(&format!("  {l} = call %NxVal @nx_new_list(i64 {n})"));
                        let p = self.alloca("%NxVal");
                        self.w(&format!("  store %NxVal {l}, ptr {p}"));
                        for e in values {
                            let v = self.emit_expr(e)?;
                            // The tuple owns its elements.
                            let b = self.store_boxed(&v);
                            self.w(&format!("  call void @nx_listpush(ptr {p}, %NxVal {b})"));
                        }
                        let out = self.reg();
                        self.w(&format!("  {out} = load %NxVal, ptr {p}"));
                        self.free_scope();
                        self.emit_ret(Some(&out));
                    }
                }
                self.term = Some(Term::Ret);
                Ok(())
            }
            Stmt::Break { span } => {
                let exit = self
                    .loops
                    .last()
                    .map(|(_, e)| e.clone())
                    .ok_or(err(*span, "break outside loop".to_string()))?;
                self.w(&format!("  br label %{exit}"));
                self.term = Some(Term::Brk);
                Ok(())
            }
            Stmt::Continue { span } => {
                let cond = self
                    .loops
                    .last()
                    .map(|(c, _)| c.clone())
                    .ok_or(err(*span, "continue outside loop".to_string()))?;
                self.w(&format!("  br label %{cond}"));
                self.term = Some(Term::Ctn);
                Ok(())
            }
            Stmt::Import {
                module,
                alias,
                span,
            } => {
                if !self.arity_known_module(module) && !self.module_has_vars(module) {
                    return Err(err(*span, format!("cannot find module '{module}'")));
                }
                let bind = alias.clone().unwrap_or_else(|| module.clone());
                self.modrefs.insert(bind, module.clone());
                self.w(&format!("  call void @{}()", mangle_init(module)));
                Ok(())
            }
            Stmt::FromImport {
                module,
                names,
                span,
            } => {
                if !self.module_known(module) {
                    return Err(err(*span, format!("cannot find module '{module}'")));
                }
                self.w(&format!("  call void @{}()", mangle_init(module)));
                for (name, alias) in names {
                    let bind = alias.clone().unwrap_or_else(|| name.clone());
                    // A type imports as an alias only: types are constructed,
                    // never held, so there is no global to load. The alias
                    // map (harvested up front) is what `Pt(...)` resolves
                    // through, and the descriptor global is shared.
                    if self.layouts.contains_key(&(module.clone(), name.clone())) {
                        continue;
                    }
                    if self.is_module_fn(module, name) {
                        self.falias.insert(bind, (module.clone(), name.clone()));
                    } else {
                        let src = mangle_global(module, name);
                        let v = self.reg();
                        self.w(&format!("  {v} = load %NxVal, ptr @{src}"));
                        // A from-imported module global: read boxed here,
                        // but its declared type still lets later uses of
                        // the local stay unboxed.
                        let ty = self
                            .types
                            .get(&(module.clone(), "<top>".to_string()))
                            .and_then(|f| f.locals.get(name))
                            .cloned()
                            .unwrap_or(Ty::Unknown);
                        self.store_fresh(&bind, &NV::boxed_known(v, ty));
                    }
                }
                Ok(())
            }
            Stmt::Expr(e) => {
                self.emit_expr(e)?;
                Ok(())
            }
        }
    }

    pub(crate) fn emit_if_full(
        &mut self,
        cond: &Expr,
        then_body: &[Stmt],
        elifs: &[(Expr, Vec<Stmt>)],
        else_body: &Option<Vec<Stmt>>,
    ) -> Result<(), CodegenError> {
        let endl = self.lab("iend");
        // Every arm gets: cond -> br body/next; body falls to endl or returns.
        // Returns true if the merge is reachable (i.e. some path falls through).
        let mut reachable = false;
        let mut arms: Vec<(&Expr, &[Stmt])> = vec![(cond, then_body)];
        for (ec, eb) in elifs {
            arms.push((ec, eb));
        }
        for (c, b) in arms {
            let bodyl = self.lab("ibranch");
            let next = self.lab("inext");
            let cv = self.emit_expr(c)?;
            let bv = self.as_i1(&cv);
            self.w(&format!("  br i1 {bv}, label %{bodyl}, label %{next}"));
            self.block(&format!("{bodyl}"));
            self.term = None;
            for s in b {
                self.emit_stmt(s)?;
                if self.term.is_some() {
                    break;
                }
            }
            if self.term.is_none() {
                self.w(&format!("  br label %{endl}"));
                reachable = true;
            }
            self.term = None;
            self.block(&format!("{next}"));
        }
        if let Some(b) = else_body {
            self.term = None;
            for s in b {
                self.emit_stmt(s)?;
                if self.term.is_some() {
                    break;
                }
            }
            if self.term.is_none() {
                self.w(&format!("  br label %{endl}"));
                reachable = true;
            }
            self.term = None;
        } else {
            // All-false path falls through to the merge.
            self.w(&format!("  br label %{endl}"));
            reachable = true;
        }
        self.block(&format!("{endl}"));
        if reachable {
            self.term = None;
        } else {
            self.w("  unreachable");
            self.term = Some(Term::Ret);
        }
        Ok(())
    }

    pub(crate) fn emit_for(
        &mut self,
        var: &str,
        iter: &nx_ast::ForIter,
        body: &[Stmt],
        span: Span,
    ) -> Result<(), CodegenError> {
        match iter {
            nx_ast::ForIter::Range { start, end } => {
                let s = self.emit_expr(start)?;
                let e = self.emit_expr(end)?;
                let a = self.as_i64(&s);
                let b = self.as_i64(&e);
                let up = self.reg();
                let step = self.reg();
                self.w(&format!("  {up} = icmp sle i64 {a}, {b}"));
                self.w(&format!("  {step} = select i1 {up}, i64 1, i64 -1"));
                let slot = self.alloca("i64");
                self.w(&format!("  store i64 {a}, ptr {slot}"));
                let condl = self.lab("fcond");
                let bodyl = self.lab("fbody");
                let endl = self.lab("fend");
                // `continue` has to advance the induction variable, so it lands
                // here rather than on the condition. Jumping straight back to
                // the condition re-tests the same index and never terminates.
                let latchl = self.lab("flatch");
                self.w(&format!("  br label %{condl}"));
                self.block(&format!("{condl}"));
                let cur = self.reg();
                let go = self.reg();
                let goup = self.reg();
                let godn = self.reg();
                self.w(&format!("  {cur} = load i64, ptr {slot}"));
                self.w(&format!("  {goup} = icmp slt i64 {cur}, {b}"));
                self.w(&format!("  {godn} = icmp sgt i64 {cur}, {b}"));
                self.w(&format!("  {go} = select i1 {up}, i1 {goup}, i1 {godn}"));
                self.w(&format!("  br i1 {go}, label %{bodyl}, label %{endl}"));
                self.block(&format!("{bodyl}"));
                // The induction variable is statically Int.
                let iv = NV::raw(Ty::Int, cur.clone());
                let scope = self.bind_loop_var(var, &iv);
                self.loops.push((latchl.clone(), endl.clone()));
                self.term = None;
                for st in body {
                    self.emit_stmt(st)?;
                    if self.term.is_some() {
                        break;
                    }
                }
                let bt = self.term.take();
                self.loops.pop();
                // The loop variable's scope ends with the loop, so the
                // enclosing binding is visible again from here on.
                scope.restore(self);
                match bt {
                    Some(Term::Ret) => {
                        self.term = Some(Term::Ret);
                        self.block(&format!("{endl}"));
                        self.w("  unreachable");
                    }
                    Some(_) => {
                        self.block(&format!("{endl}"));
                        self.term = None;
                    }
                    None => {
                        self.w(&format!("  br label %{latchl}"));
                        self.block(&format!("{latchl}"));
                        let cur3 = self.reg();
                        let nxt = self.reg();
                        self.w(&format!("  {cur3} = load i64, ptr {slot}"));
                        self.w(&format!("  {nxt} = add i64 {cur3}, {step}"));
                        self.w(&format!("  store i64 {nxt}, ptr {slot}"));
                        self.w(&format!("  br label %{condl}"));
                        self.block(&format!("{endl}"));
                        self.term = None;
                    }
                }
                let _ = span;
                Ok(())
            }
            nx_ast::ForIter::Each(e) => {
                let v = self.emit_expr(e)?;
                let vb = self.unbox(&v);
                let is_dict = matches!(v.ty, Ty::Dict(_));
                let len = self.reg();
                let n = self.reg();
                self.w(&format!("  {len} = call %NxVal @nx_len(%NxVal {vb})"));
                self.w(&format!("  {n} = extractvalue %NxVal {len}, 1"));
                let islot = self.alloca("i64");
                self.w(&format!("  store i64 0, ptr {islot}"));
                let condl = self.lab("econd");
                let bodyl = self.lab("ebody");
                let endl = self.lab("eend");
                // See the range arm: `continue` must go through the increment.
                let latchl = self.lab("elatch");
                self.w(&format!("  br label %{condl}"));
                self.block(&format!("{condl}"));
                let i = self.reg();
                let go = self.reg();
                self.w(&format!("  {i} = load i64, ptr {islot}"));
                self.w(&format!("  {go} = icmp slt i64 {i}, {n}"));
                self.w(&format!("  br i1 {go}, label %{bodyl}, label %{endl}"));
                self.block(&format!("{bodyl}"));
                let el = self.reg();
                let scope = if is_dict {
                    // Iterating a dict yields its keys, in insertion order.
                    // The runtime helper reads entry `i`'s key directly,
                    // which avoids building the key list first.
                    self.w(&format!(
                        "  {el} = call %NxVal @nx_dictkeyat(%NxVal {vb}, i64 {i})"
                    ));
                    self.bind_loop_var(var, &NV::dyn_boxed(el))
                } else if matches!(v.ty, Ty::Unknown) {
                    // Unresolved iterable: the tag decides list, string or
                    // dict at runtime. A dict yields its keys, matching the
                    // the statically-known-dict path.
                    self.w(&format!(
                        "  {el} = call %NxVal @nx_each(%NxVal {vb}, i64 {i})"
                    ));
                    self.bind_loop_var(var, &NV::dyn_boxed(el))
                } else {
                    let iv = self.reg();
                    let ix = NV::raw(Ty::Int, i.clone());
                    let ivb = self.unbox(&ix);
                    self.w(&format!("  {iv} = call %NxVal @nx_int(i64 {i})"));
                    self.w(&format!(
                        "  {el} = call %NxVal @nx_index(%NxVal {vb}, %NxVal {ivb})"
                    ));
                    // Element type comes from the list, so a list of scalars
                    // iterates without re-boxing. as_raw declines when
                    // unboxing is off, leaving the box in place.
                    let elem_ty = match &v.ty {
                        Ty::List(t) => (**t).clone(),
                        Ty::Str => Ty::Str,
                        _ => Ty::Unknown,
                    };
                    let boxed_elem = NV::boxed_known(el, elem_ty);
                    let ev = self.as_raw(&boxed_elem).unwrap_or(boxed_elem);
                    self.bind_loop_var(var, &ev)
                };
                self.loops.push((latchl.clone(), endl.clone()));
                self.term = None;
                for st in body {
                    self.emit_stmt(st)?;
                    if self.term.is_some() {
                        break;
                    }
                }
                let bt = self.term.take();
                self.loops.pop();
                // The loop variable's scope ends with the loop, so the
                // enclosing binding is visible again from here on.
                scope.restore(self);
                match bt {
                    Some(Term::Ret) => {
                        self.term = Some(Term::Ret);
                        self.block(&format!("{endl}"));
                        self.w("  unreachable");
                    }
                    Some(_) => {
                        self.block(&format!("{endl}"));
                        self.term = None;
                    }
                    None => {
                        self.w(&format!("  br label %{latchl}"));
                        self.block(&format!("{latchl}"));
                        let i2 = self.reg();
                        let i3 = self.reg();
                        self.w(&format!("  {i2} = load i64, ptr {islot}"));
                        self.w(&format!("  {i3} = add i64 {i2}, 1"));
                        self.w(&format!("  store i64 {i3}, ptr {islot}"));
                        self.w(&format!("  br label %{condl}"));
                        self.block(&format!("{endl}"));
                        self.term = None;
                    }
                }
                let _ = span;
                Ok(())
            }
        }
    }
}
