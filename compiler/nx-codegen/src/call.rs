//! Calls: functions, builtins, methods, and construction.

use crate::core::{Gen, MethodSig};
use crate::mangle::{mangle_fn, mangle_method};
use crate::value::{ll_scalar, NV};
use crate::{err, CodegenError};
use nx_ast::{Expr, Span};
use nx_types::Ty;

impl Gen {
    pub(crate) fn emit_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        span: Span,
    ) -> Result<NV, CodegenError> {
        // A declared type is constructed by name, sharing the call spelling
        // with a function. Checked before function resolution: a type
        // and a function cannot share a name, so reaching here with a
        // type name means construction, not a call.
        if let Expr::Var(name, _) = callee {
            if let Some((decl_module, canon, fields)) = self.resolve_type(name) {
                if args.len() != fields.len() {
                    return Err(err(
                        span,
                        format!(
                            "type '{name}' takes {} field{}, got {}",
                            fields.len(),
                            if fields.len() == 1 { "" } else { "s" },
                            args.len()
                        ),
                    ));
                }
                return self.emit_construct(&decl_module, &canon, name, args);
            }
        }
        if let Expr::Var(name, _) = callee {
            if nx_types::builtin_arity(name).is_some() {
                return self.emit_builtin(name, args, span);
            }
            let cur = self.cur_module.clone();
            if self.arity.contains_key(&(cur.clone(), name.clone())) {
                return self.emit_direct(&cur, name, args);
            }
            if let Some((m, f)) = self.falias.get(name).cloned() {
                return self.emit_direct(&m, &f, args);
            }
            return Err(err(span, format!("unknown function '{name}'")));
        }
        if let Expr::Attr { base, attr, .. } = callee {
            if let Expr::Var(m, _) = base.as_ref() {
                if let Some(module) = self.modrefs.get(m).cloned() {
                    // A type exported by the module constructs the same
                    // way a local one does.
                    if let Some(fields) = self.layouts.get(&(module.clone(), attr.clone())).cloned()
                    {
                        if args.len() != fields.len() {
                            return Err(err(
                                span,
                                format!(
                                    "type '{attr}' takes {} field{}, got {}",
                                    fields.len(),
                                    if fields.len() == 1 { "" } else { "s" },
                                    args.len()
                                ),
                            ));
                        }
                        return self.emit_construct(&module, attr, attr, args);
                    }
                    if self.is_module_fn(&module, attr) {
                        return self.emit_direct(&module, attr, args);
                    }
                    return Err(err(
                        span,
                        format!("'{attr}' is not a function of '{module}'"),
                    ));
                }
                // `T.m(...)` where T names a visible type: an associated
                // function. The base is not a value, so there is no sugar
                // fallback -- anything else is meaningless.
                if let Some((decl, canon, _)) = self.resolve_type(m) {
                    if let Some(sig) = self
                        .methods
                        .get(&(decl.clone(), canon.clone(), attr.clone()))
                        .cloned()
                    {
                        if sig.receiver != nx_ast::ReceiverKind::None {
                            return Err(err(
                                span,
                                format!("method '{attr}' needs a receiver; call it on a '{canon}' value"),
                            ));
                        }
                        return self.emit_method_call(&decl, &canon, attr, &sig, None, args, span);
                    }
                    return Err(err(
                        span,
                        format!("type '{canon}' has no associated function '{attr}'"),
                    ));
                }
            }
            // A record value dispatches to its type's method table. The
            // base is evaluated once; its static type decides method
            // versus sugar, so an unresolved base always takes sugar.
            let recv = self.emit_expr(base)?;
            // Dispatch types come from the checker, not from the unboxing
            // decision: with unboxing off every `NV` is Unknown, and a
            // method call that stopped resolving there would make the
            // opt-out a different language. A bare variable can be asked
            // directly, which is what keeps `self.area()` working.
            let bt = match &recv.ty {
                Ty::Unknown => match base.as_ref() {
                    // A field read is always dynamically typed (unboxed
                    // fields arrive with ownership), so a receiver reached
                    // through one re-derives its static type from the
                    // declaration instead. Anything still unknown stays
                    // unknown and takes sugar or the error below.
                    Expr::Var(..) | Expr::Attr { .. } => self.static_ty_of(base),
                    _ => Ty::Unknown,
                },
                t => t.clone(),
            };
            if let Ty::Record(t) = bt {
                if let Some((decl, canon, _)) = self.resolve_type(&t) {
                    if let Some(sig) = self
                        .methods
                        .get(&(decl.clone(), canon.clone(), attr.clone()))
                        .cloned()
                    {
                        return self.emit_method_call(
                            &decl,
                            &canon,
                            attr,
                            &sig,
                            Some((base, &recv)),
                            args,
                            span,
                        );
                    }
                    // Records without the method fall through to sugar, so
                    // a builtin that accepts records keeps working -- the
                    // builtin's own check names any mismatch.
                    if nx_types::builtin_arity(attr).is_none() {
                        return Err(err(span, format!("type '{canon}' has no method '{attr}'")));
                    }
                } else {
                    return Err(err(span, format!("unknown type '{t}'")));
                }
            }
            // Builtin sugar: `xs.push(1)` for `push(xs, 1)`. The base
            // expression is prepended and routed through the identical
            // builtin path as a direct call.
            if nx_types::builtin_arity(attr).is_some() {
                let mut combined: Vec<Expr> = Vec::with_capacity(args.len() + 1);
                combined.push((**base).clone());
                combined.extend(args.iter().cloned());
                return self.emit_builtin(attr, &combined, span);
            }
            return Err(err(
                span,
                "only modules, types and builtins support attribute calls".to_string(),
            ));
        }
        Err(err(span, "only direct calls are supported".to_string()))
    }

    /// Emit an ambient builtin by name. Direct calls (`push(xs, 1)`) and
    /// sugar calls (`xs.push(1)`) share this path: sugar prepends the
    /// base expression and arrives here with identical arguments, so the
    /// two spellings cannot drift apart.
    pub(crate) fn emit_builtin(
        &mut self,
        name: &str,
        args: &[Expr],
        span: Span,
    ) -> Result<NV, CodegenError> {
        match name {
            "len" => {
                if args.len() != 1 {
                    return Err(err(span, "len() expects 1 argument".to_string()));
                }
                let a = self.emit_expr(&args[0])?;
                let ab = self.unbox(&a);
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_len(%NxVal {ab})"));
                // len is always an Int, so the payload can go straight on.
                let n = self.reg();
                self.w(&format!("  {n} = extractvalue %NxVal {r}, 1"));
                Ok(NV::raw(Ty::Int, n))
            }
            "push" => {
                if args.len() != 2 {
                    return Err(err(span, "push() expects 2 arguments".to_string()));
                }
                // The target must be a variable: pushing into a temporary
                // would drop the result, so the checker rejects it and
                // this arm never sees one.
                if !matches!(&args[0], Expr::Var(..)) {
                    return Err(err(
                        span,
                        "push() first argument must be a list variable".to_string(),
                    ));
                }
                let v = self.emit_expr(&args[1])?;
                let vb = self.store_boxed(&v);
                // The pushed value is owned by the list from here on.
                match &args[0] {
                    Expr::Var(n, _) if !matches!(self.ty_of_any(n), Ty::Unknown) => {
                        let ptr = self.ptr_of(n).ok_or(err(
                            span,
                            "push() first argument must be a list variable".to_string(),
                        ))?;
                        self.w(&format!("  call void @nx_listpush(ptr {ptr}, %NxVal {vb})"));
                    }
                    Expr::Var(_, _) => {
                        // Unresolved base: only a list can be pushed to, and
                        // the tag check says so at runtime rather than
                        // corrupting a dict's entry array.
                        let lv = self.emit_expr(&args[0])?;
                        let lb = self.unbox(&lv);
                        let p = self.alloca("%NxVal");
                        self.w(&format!("  store %NxVal {lb}, ptr {p}"));
                        self.w(&format!("  call void @nx_pushdyn(ptr {p}, %NxVal {vb})"));
                        let updated = self.reg();
                        self.w(&format!("  {updated} = load %NxVal, ptr {p}"));
                        self.write_back(&args[0], &NV::boxed_known(updated, Ty::Unknown))?;
                    }
                    _ => {
                        return Err(err(
                            span,
                            "push() first argument must be a list variable".to_string(),
                        ))
                    }
                }
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_none()"));
                Ok(NV::boxed_known(r, Ty::None))
            }
            "input" => {
                // `input()` reads a line; `input(prompt)` prints the prompt
                // first. The checker caps the arity, so anything else here
                // is an internal error, not a user program.
                if args.len() > 1 {
                    return Err(err(
                        span,
                        format!("input() expects at most 1 argument, got {}", args.len()),
                    ));
                }
                let (pv, has) = match args.first() {
                    Some(p) => {
                        let v = self.emit_expr(p)?;
                        (self.unbox(&v), "true")
                    }
                    None => {
                        let n = self.reg();
                        self.w(&format!("  {n} = call %NxVal @nx_none()"));
                        (n, "false")
                    }
                };
                let r = self.reg();
                self.w(&format!(
                    "  {r} = call %NxVal @nx_input(%NxVal {pv}, i1 {has})"
                ));
                // The answer is a freshly allocated buffer, so storing it
                // needs no clone.
                Ok(NV::fresh_boxed(r, Ty::Str))
            }
            "int" => {
                // `int(x)` converts one value to Int. Parsing lives in the
                // runtime helper, once -- the checker above owns which
                // types arrive, so anything else here is unreachable
                // through checked code.
                if args.len() != 1 {
                    return Err(err(
                        span,
                        format!("int() expects 1 argument, got {}", args.len()),
                    ));
                }
                let a = self.emit_expr(&args[0])?;
                let ab = self.store_boxed(&a);
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_to_int(%NxVal {ab})"));
                Ok(NV::fresh_boxed(r, Ty::Int))
            }
            "float" => {
                // `float(x)` converts one value to Float, same shape.
                if args.len() != 1 {
                    return Err(err(
                        span,
                        format!("float() expects 1 argument, got {}", args.len()),
                    ));
                }
                let a = self.emit_expr(&args[0])?;
                let ab = self.store_boxed(&a);
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_to_float(%NxVal {ab})"));
                Ok(NV::fresh_boxed(r, Ty::Float))
            }
            "solve" => {
                // `solve(A, b)` solves A * x = b. The checker pins the
                // arity; anything else here is an internal error.
                if args.len() != 2 {
                    return Err(err(
                        span,
                        format!("solve() expects 2 arguments, got {}", args.len()),
                    ));
                }
                let a = self.emit_expr(&args[0])?;
                let b = self.emit_expr(&args[1])?;
                let ab = self.store_boxed(&a);
                let bb = self.store_boxed(&b);
                let r = self.reg();
                self.w(&format!(
                    "  {r} = call %NxVal @nx_solve(%NxVal {ab}, %NxVal {bb})"
                ));
                Ok(NV::fresh_boxed(r, Ty::Unknown))
            }
            _ => Err(err(span, format!("unknown builtin '{name}'"))),
        }
    }

    /// Emit a method or associated-function call. The receiver value (for
    /// methods) is prepended to the argument boxes, so the callee sees
    /// `self` positionally like any other parameter -- which under value
    /// semantics gives the method its own copy to mutate.
    ///
    /// A `mut self` result is written back into the receiver when the
    /// receiver has storage; a temporary base has nowhere to write, so the
    /// call evaluates to its result alone.
    pub(crate) fn emit_method_call(
        &mut self,
        decl_module: &str,
        canon: &str,
        method: &str,
        sig: &MethodSig,
        receiver: Option<(&Expr, &NV)>,
        args: &[Expr],
        span: Span,
    ) -> Result<NV, CodegenError> {
        let fname = mangle_method(decl_module, canon, method);
        // The receiver is already evaluated -- the caller emitted it once to
        // learn its type, and emitting it again here would run a `mut self`
        // chain's write-back twice.
        let mut vals: Vec<NV> = Vec::with_capacity(args.len() + 1);
        if let Some((_, rv)) = receiver {
            vals.push(rv.clone());
        }
        for a in args {
            vals.push(self.emit_expr(a)?);
        }
        // The return type comes from inference when available; a `mut self`
        // method returns the record by the checker's rule, which is what
        // makes the write-back type-correct without an annotation.
        let ret = if sig.receiver == nx_ast::ReceiverKind::Mut {
            Ty::Record(canon.to_string())
        } else {
            self.types
                .get(&(
                    decl_module.to_string(),
                    nx_ast::shape::method_key(canon, method),
                ))
                .map(|f| f.ret.clone())
                .unwrap_or(Ty::Unknown)
        };
        let out = self.emit_call_boxed(&fname, &vals, ret)?;
        // Write-back for `mut self`, when the receiver has storage. A
        // temporary base has nowhere to write, so the call evaluates to its
        // result alone -- which is what makes `q.moved(1, 1).moved(2, 2)`
        // read as one expression.
        if sig.receiver == nx_ast::ReceiverKind::Mut {
            if let Some((base, _)) = receiver {
                if let Some(t) = Self::target_of_expr(base) {
                    self.store_target(&t, &out, span)?;
                }
            }
        }
        Ok(out)
    }

    /// Reinterpret a call receiver as an assignment target, for `mut self`
    /// write-back. Only shapes that have
    /// storage qualify.
    pub(crate) fn target_of_expr(e: &Expr) -> Option<nx_ast::Target> {
        match e {
            Expr::Var(name, _) => Some(nx_ast::Target::Name(name.clone())),
            Expr::Index { base, index, .. } => Some(nx_ast::Target::Index {
                base: base.clone(),
                index: index.clone(),
            }),
            Expr::Attr { base, attr, .. } => Some(nx_ast::Target::Attr {
                base: base.clone(),
                field: attr.clone(),
            }),
            _ => None,
        }
    }

    /// `Type(v0, v1, ...)` -- allocate the record, then fill each field in
    /// declaration order. The record value is in a register throughout;
    /// `nx_rec_set` writes through the header, so no alloca is needed to
    /// hold it between the fills. The static type carries the canonical
    /// name, so a value built through an alias compares and resolves
    /// exactly like one built through the original name.
    pub(crate) fn emit_construct(
        &mut self,
        decl_module: &str,
        canon: &str,
        _written: &str,
        args: &[Expr],
    ) -> Result<NV, CodegenError> {
        let n = args.len() as i64;
        let desc = self.desc_of(decl_module, canon);
        let r = self.reg();
        self.w(&format!(
            "  {r} = call %NxVal @nx_new_record(i64 {n}, ptr {desc})"
        ));
        for (i, a) in args.iter().enumerate() {
            let v = self.emit_expr(a)?;
            let vb = self.store_boxed(&v);
            self.w(&format!(
                "  call void @nx_rec_set(%NxVal {r}, i64 {i}, %NxVal {vb})"
            ));
        }
        Ok(NV::fresh_boxed(r, Ty::Record(canon.to_string())))
    }

    pub(crate) fn emit_direct(
        &mut self,
        module: &str,
        name: &str,
        args: &[Expr],
    ) -> Result<NV, CodegenError> {
        let fname = mangle_fn(module, name);
        let ty = self.ret_ty(module, name);
        self.emit_direct_named(&fname, args, ty)
    }

    /// Call an already-mangled function symbol with boxed ABI arguments.
    /// Each argument expression is evaluated exactly once.
    pub(crate) fn emit_direct_named(
        &mut self,
        fname: &str,
        args: &[Expr],
        ty: Ty,
    ) -> Result<NV, CodegenError> {
        let mut vals: Vec<NV> = Vec::with_capacity(args.len());
        for a in args {
            vals.push(self.emit_expr(a)?);
        }
        self.emit_call_boxed(fname, &vals, ty)
    }

    /// Call an already-mangled symbol with values that are *already*
    /// evaluated. Method calls come through here rather than through
    /// `emit_direct_named` because the receiver has to be emitted once: it
    /// is evaluated to learn its static type, and a `mut self` receiver is
    /// an expression with an effect of its own. Re-emitting it to build the
    /// argument list would run that effect twice.
    pub(crate) fn emit_call_boxed(
        &mut self,
        fname: &str,
        vals: &[NV],
        ty: Ty,
    ) -> Result<NV, CodegenError> {
        let n = vals.len();
        let arr = self.alloca(&format!("[{n} x %NxVal]"));
        for (i, v) in vals.iter().enumerate() {
            // The ABI is boxed: every argument re-boxes here.
            let vb = self.unbox(v);
            let ep = self.reg();
            self.w(&format!(
                "  {ep} = getelementptr [{n} x %NxVal], ptr {arr}, i64 0, i64 {i}"
            ));
            self.w(&format!("  store %NxVal {vb}, ptr {ep}"));
        }
        let p0 = self.reg();
        if n == 0 {
            self.w(&format!("  {p0} = inttoptr i64 0 to ptr"));
        } else {
            self.w(&format!(
                "  {p0} = getelementptr [{n} x %NxVal], ptr {arr}, i64 0, i64 0"
            ));
        }
        let r = self.reg();
        self.w(&format!("  {r} = call %NxVal @{fname}(ptr {p0}, i64 {n})"));
        if let Some(ll) = ll_scalar(&ty) {
            let p1 = self.reg();
            self.w(&format!("  {p1} = extractvalue %NxVal {r}, 1"));
            return Ok(match ll {
                "double" => {
                    let d = self.reg();
                    self.w(&format!("  {d} = bitcast i64 {p1} to double"));
                    NV::raw(ty, d)
                }
                "i1" => {
                    let c = self.reg();
                    self.w(&format!("  {c} = trunc i64 {p1} to i1"));
                    NV::raw(ty, c)
                }
                _ => NV::raw(ty, p1),
            });
        }
        Ok(NV::boxed_known(r, ty))
    }
}
