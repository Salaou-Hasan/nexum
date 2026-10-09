//! Expression emission: literals, collections, and control forms.

use crate::core::{Binding, Gen};
use crate::mangle::mangle_global;
use crate::value::{fmt_double, ll_scalar, NV};
use crate::{err, CodegenError};
use nx_ast::{BinOp, Expr, Span, UnaryOp};
use nx_types::Ty;

impl Gen {
    /// Emit a fully unboxed integer literal. LLVM folds the instruction,
    /// so this costs nothing and keeps one code path for scalars.
    pub(crate) fn emit_i64(&mut self, i: i64) -> NV {
        let r = self.reg();
        self.w(&format!("  {r} = add i64 {i}, 0"));
        NV::raw_const(Ty::Int, r, i)
    }

    /// Append `n` consecutive Ints starting at `start` to the list at `p`.
    /// The counter lives in an entry-block alloca so the loop does not
    /// grow the frame on every iteration.
    pub(crate) fn emit_range_fill(&mut self, p: &str, start: &str, n: &str) {
        let ireg = self.alloca("i64");
        self.w(&format!("  store i64 0, ptr {ireg}"));
        let condl = self.lab("rng_cond");
        let bodyl = self.lab("rng_body");
        let endl = self.lab("rng_end");
        self.w(&format!("  br label %{condl}"));
        self.block(&format!("{condl}"));
        let i = self.reg();
        self.w(&format!("  {i} = load i64, ptr {ireg}"));
        let done = self.reg();
        self.w(&format!("  {done} = icmp sge i64 {i}, {n}"));
        self.w(&format!("  br i1 {done}, label %{endl}, label %{bodyl}"));
        self.block(&format!("{bodyl}"));
        let v = self.reg();
        self.w(&format!("  {v} = add i64 {start}, {i}"));
        let bv = self.reg();
        self.w(&format!("  {bv} = call %NxVal @nx_int(i64 {v})"));
        self.w(&format!("  call void @nx_listpush(ptr {p}, %NxVal {bv})"));
        let inc = self.reg();
        self.w(&format!("  {inc} = add i64 {i}, 1"));
        self.w(&format!("  store i64 {inc}, ptr {ireg}"));
        self.w(&format!("  br label %{condl}"));
        self.block(&format!("{endl}"));
    }

    pub(crate) fn emit_expr(&mut self, expr: &Expr) -> Result<NV, CodegenError> {
        match expr {
            Expr::Int(i, _) => Ok(self.emit_i64(*i)),
            Expr::Float(x, _) => {
                let d = self.reg();
                self.w(&format!("  {d} = fadd double {}, 0.0", fmt_double(*x)));
                Ok(NV::raw(Ty::Float, d))
            }
            Expr::Bool(b, _) => {
                let r = self.reg();
                self.w(&format!(
                    "  {r} = add i1 {}, 0",
                    if *b { "true" } else { "false" }
                ));
                Ok(NV::raw(Ty::Bool, r))
            }
            Expr::Str(s, span) => {
                let r = self.emit_str(s, *span)?;
                Ok(NV::boxed_known(r, Ty::Str))
            }
            Expr::NoneLit(_) => {
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_none()"));
                Ok(NV::boxed_known(r, Ty::None))
            }
            Expr::Range { start, end, .. } => {
                // Materialised, unlike a `for i in a..b` header which stays
                // a counted loop. Half-open and ascending, so an empty
                // range produces an empty list rather than underflowing.
                let s = self.emit_expr(start)?;
                let e = self.emit_expr(end)?;
                let a = self.as_i64(&s);
                let b = self.as_i64(&e);
                let cnt = self.reg();
                self.w(&format!("  {cnt} = sub i64 {b}, {a}"));
                let nonneg = self.reg();
                self.w(&format!("  {nonneg} = icmp sgt i64 {cnt}, 0"));
                let n = self.reg();
                self.w(&format!("  {n} = select i1 {nonneg}, i64 {cnt}, i64 0"));
                let l = self.reg();
                self.w(&format!("  {l} = call %NxVal @nx_new_list(i64 {n})"));
                let p = self.alloca("%NxVal");
                self.w(&format!("  store %NxVal {l}, ptr {p}"));
                self.emit_range_fill(&p, &a, &n);
                let out = self.reg();
                self.w(&format!("  {out} = load %NxVal, ptr {p}"));
                Ok(NV::fresh_boxed(out, Ty::List(Box::new(Ty::Int))))
            }
            Expr::Dict(pairs, _) => {
                let n = pairs.len() as i64;
                let d = self.reg();
                self.w(&format!("  {d} = call %NxVal @nx_new_dict(i64 {n})"));
                let p = self.alloca("%NxVal");
                self.w(&format!("  store %NxVal {d}, ptr {p}"));
                for (k, v) in pairs {
                    let kv = self.emit_expr(k)?;
                    let vv = self.emit_expr(v)?;
                    let kb = self.unbox(&kv);
                    let vb = self.store_boxed(&vv);
                    self.w(&format!(
                        "  call void @nx_dictset(ptr {p}, %NxVal {kb}, %NxVal {vb})"
                    ));
                }
                let out = self.reg();
                self.w(&format!("  {out} = load %NxVal, ptr {p}"));
                Ok(NV::fresh_boxed(out, Ty::Dict(Box::new(Ty::Unknown))))
            }
            Expr::Slice {
                base,
                from,
                to,
                step,
                span,
            } => {
                let b = self.emit_expr(base)?;
                let bv = self.unbox(&b);
                // An absent bound is the runtime's sentinel for "to the
                // end", which is why the default is a large Int rather than
                // a separate flag.
                let mk = |me: &mut Self,
                          e: &Option<Box<Expr>>,
                          dflt: i64|
                 -> Result<String, CodegenError> {
                    match e {
                        Some(x) => {
                            let v = me.emit_expr(x)?;
                            Ok(me.as_i64(&v))
                        }
                        // An absent `from` is i64::MIN and an absent `to` is i64::MAX. The
                        // runtime clamps a negative `from` by adding the length
                        // and then flooring it at zero, so MIN lands on 0 for any
                        // list length, which is exactly "from the start".
                        None => Ok(me.emit_i64(dflt).reg),
                    }
                };
                let f = mk(self, from, i64::MIN)?;
                let t = mk(self, to, i64::MAX)?;
                let s = mk(self, step, 1)?;
                let out = self.reg();
                self.w(&format!(
                    "  {out} = call %NxVal @nx_slice(%NxVal {bv}, i64 {f}, i64 {t}, i64 {s})"
                ));
                let _ = span;
                // Slicing a list of known scalars keeps that element type.
                // Fresh storage, like every other constructor here.
                match &b.ty {
                    Ty::List(t) => Ok(NV::fresh_boxed(out, Ty::List(t.clone()))),
                    Ty::Str => Ok(NV::fresh_boxed(out, Ty::Str)),
                    _ => Ok(NV::fresh_boxed(out, Ty::Unknown)),
                }
            }
            Expr::IfExpr {
                cond,
                then_value,
                else_value,
                ..
            } => {
                let c = self.emit_expr(cond)?;
                let b = self.as_i1(&c);
                let tl = self.lab("if_then");
                let el = self.lab("if_else");
                let jn = self.lab("if_join");
                self.w(&format!("  br i1 {b}, label %{tl}, label %{el}"));
                self.block(&format!("{tl}"));
                let tv = self.emit_expr(then_value)?;
                let tb = self.unbox(&tv);
                // An arm can open blocks of its own -- `xs[i - 1] if c else y`
                // leaves the subtraction's `ovf_done` holding the value -- so
                // the phi below may not name `%if_then`/`%if_else` directly.
                let theld = self.funnel(&tl);
                self.w(&format!("  br label %{jn}"));
                self.block(&format!("{el}"));
                let ev = self.emit_expr(else_value)?;
                let eb = self.unbox(&ev);
                let eheld = self.funnel(&el);
                self.w(&format!("  br label %{jn}"));
                self.block(&format!("{jn}"));
                // Both arms carry the same `%NxVal` type, so a phi over the
                // boxed form is all that is needed to merge them.
                let out = self.reg();
                self.w(&format!(
                    "  {out} = phi %NxVal [ {tb}, %{theld} ], [ {eb}, %{eheld} ]"
                ));
                // The phi merges the boxed form, so the result type only has to be
                // precise when both arms agree. Otherwise it stays dynamic,
                // which is correct and merely unspecialised. Fresh only when
                // both arms are: the taken arm's box is then uniquely owned
                // no matter which arm was taken.
                let arms_agree =
                    tv.ty == ev.ty || matches!(tv.ty, Ty::Unknown) || matches!(ev.ty, Ty::Unknown);
                let ty = if arms_agree {
                    if tv.ty == Ty::Unknown {
                        ev.ty.clone()
                    } else {
                        tv.ty.clone()
                    }
                } else {
                    Ty::Unknown
                };
                let mut nv = NV::boxed_known(out, ty);
                nv.fresh = tv.fresh && ev.fresh;
                Ok(nv)
            }
            Expr::Comprehension {
                element,
                var,
                iter,
                cond,
                ..
            } => {
                // Emitted as a real loop rather than a recursive call, so it
                // stays inside the current frame and the loop variable gets
                // an ordinary slot.
                let it = self.emit_expr(iter)?;
                let elem = match &it.ty {
                    Ty::List(t) => (**t).clone(),
                    Ty::Str => Ty::Str,
                    _ => Ty::Unknown,
                };
                let items_ty = match &it.ty {
                    Ty::List(_) => Ty::List(Box::new(Ty::Unknown)),
                    // A comprehension always yields a list, even over a
                    // string (a list of one-character strings).
                    Ty::Str => Ty::List(Box::new(Ty::Str)),
                    other => other.clone(),
                };
                let itb = self.unbox(&it);
                // The accumulator is addressed so nx_listpush can grow it.
                let slot = self.alloca("%NxVal");
                let acc = self.reg();
                self.w(&format!("  {acc} = call %NxVal @nx_new_list(i64 8)"));
                self.w(&format!("  store %NxVal {acc}, ptr {slot}"));
                // The loop bound must count what the body fetches. A
                // string's `b` field is a BYTE length, so reading it
                // directly walks bytes -- an R3 violation the old code
                // had (`[c for c in "str"]` yielded one broken byte per
                // byte). nx_len counts characters, matching the
                // `for c in s` loop, which fetches through nx_index.
                let n = self.reg();
                match &it.ty {
                    Ty::List(_) => {
                        self.w(&format!("  {n} = extractvalue %NxVal {itb}, 2"));
                    }
                    _ => {
                        let lc = self.reg();
                        self.w(&format!("  {lc} = call %NxVal @nx_len(%NxVal {itb})"));
                        self.w(&format!("  {n} = extractvalue %NxVal {lc}, 1"));
                    }
                }
                let ireg = self.alloca("i64");
                self.w(&format!("  store i64 0, ptr {ireg}"));
                let condl = self.lab("comp_cond");
                let bodyl = self.lab("comp_body");
                let endl = self.lab("comp_end");
                self.w(&format!("  br label %{condl}"));
                self.block(&format!("{condl}"));
                let i = self.reg();
                self.w(&format!("  {i} = load i64, ptr {ireg}"));
                let done = self.reg();
                self.w(&format!("  {done} = icmp sge i64 {i}, {n}"));
                self.w(&format!("  br i1 {done}, label %{endl}, label %{bodyl}"));
                self.block(&format!("{bodyl}"));
                // Fetch the current element through the same helpers the
                // `for` loop uses, so both spellings agree: a string
                // yields one-character strings (nx_index), a dynamic
                // iterable dispatches on its tag (nx_each), a list reads
                // its stored value.
                let cur = self.reg();
                match &it.ty {
                    Ty::List(_) => {
                        self.w(&format!(
                            "  {cur} = call %NxVal @nx_listget(%NxVal {itb}, i64 {i})"
                        ));
                    }
                    Ty::Str => {
                        let iv = self.reg();
                        self.w(&format!("  {iv} = call %NxVal @nx_int(i64 {i})"));
                        self.w(&format!(
                            "  {cur} = call %NxVal @nx_index(%NxVal {itb}, %NxVal {iv})"
                        ));
                    }
                    _ => {
                        self.w(&format!(
                            "  {cur} = call %NxVal @nx_each(%NxVal {itb}, i64 {i})"
                        ));
                    }
                }
                // The loop variable is scoped to the comprehension, so a
                // same-named outer variable is neither read nor clobbered.
                let vty = ll_scalar(&elem).map(|s| match s {
                    "i64" => Ty::Int,
                    "double" => Ty::Float,
                    _ => Ty::Bool,
                });
                self.new_slot(var, vty);
                self.store_slot(var, &NV::boxed_known(cur, elem.clone()));
                // The filter and the append share one tail block, so the
                // element expression is emitted exactly once. Elements are
                // owned by the result list.
                let emit_push = |me: &mut Self| -> Result<(), CodegenError> {
                    let ev = me.emit_expr(element)?;
                    let eb = me.store_boxed(&ev);
                    me.w(&format!(
                        "  call void @nx_listpush(ptr {slot}, %NxVal {eb})"
                    ));
                    Ok(())
                };
                match cond {
                    Some(cnd) => {
                        let cv = self.emit_expr(cnd)?;
                        let cb = self.as_i1(&cv);
                        let take = self.lab("comp_take");
                        let drop = self.lab("comp_drop");
                        let adv = self.lab("comp_adv");
                        self.w(&format!("  br i1 {cb}, label %{take}, label %{drop}"));
                        self.block(&format!("{take}"));
                        emit_push(self)?;
                        self.w(&format!("  br label %{adv}"));
                        self.block(&format!("{drop}"));
                        self.w(&format!("  br label %{adv}"));
                        self.block(&format!("{adv}"));
                    }
                    None => emit_push(self)?,
                }
                let inc = self.reg();
                self.w(&format!("  {inc} = add i64 {i}, 1"));
                self.w(&format!("  store i64 {inc}, ptr {ireg}"));
                self.w(&format!("  br label %{condl}"));
                self.block(&format!("{endl}"));
                let res = self.reg();
                self.w(&format!("  {res} = load %NxVal, ptr {slot}"));
                Ok(NV::fresh_boxed(res, items_ty))
            }
            Expr::List(items, _) => {
                let n = items.len() as i64;
                let l = self.reg();
                self.w(&format!("  {l} = call %NxVal @nx_new_list(i64 {n})"));
                let p = self.alloca("%NxVal");
                self.w(&format!("  store %NxVal {l}, ptr {p}"));
                let mut elem = Ty::Unknown;
                for it in items {
                    let v = self.emit_expr(it)?;
                    if elem == Ty::Unknown {
                        elem = v.ty.clone();
                    }
                    // Elements are owned by the list, so a container
                    // element is duplicated on the way in.
                    let b = self.store_boxed(&v);
                    self.w(&format!("  call void @nx_listpush(ptr {p}, %NxVal {b})"));
                }
                let out = self.reg();
                self.w(&format!("  {out} = load %NxVal, ptr {p}"));
                // A list of proven scalars has a known element type, so
                // reading from it can stay unboxed. It is mutable, so it
                // never qualifies for memoization. Fresh storage, owned by
                // whoever binds it.
                Ok(NV::fresh_boxed(out, Ty::List(Box::new(elem))))
            }
            Expr::Var(name, span) => match self.resolve(name) {
                Ok(Binding::Local) => Ok(self.load_slot(name)),
                Ok(Binding::Global(g)) => {
                    let v = self.reg();
                    self.w(&format!("  {v} = load %NxVal, ptr @{g}"));
                    // Globals are boxed, but their static type is known,
                    // so reads of them can still feed unboxed arithmetic.
                    Ok(NV::boxed_known(v, self.global_ty(name)))
                }
                Ok(Binding::Module(m)) => Err(err(
                    *span,
                    format!("module '{m}' is compile-time only in value position"),
                )),
                Ok(Binding::ModuleFn(m, f)) => Err(err(
                    *span,
                    format!("function '{m}.{f}' cannot be used as a value; call it"),
                )),
                Err((kind, _)) if kind == "fn" => Err(err(
                    *span,
                    format!("function '{name}' cannot be used as a value; call it"),
                )),
                Err(_) => Err(err(*span, format!("undefined variable '{name}'"))),
            },
            Expr::Attr { base, attr, span } => {
                if let Expr::Var(m, _) = base.as_ref() {
                    if let Some(module) = self.modrefs.get(m).cloned() {
                        if self.is_module_fn(&module, attr) {
                            return Err(err(
                                *span,
                                format!(
                                    "function '{module}.{attr}' cannot be used as a value; call it"
                                ),
                            ));
                        }
                        // A type used as `m.T` in value position is not
                        // meaningful: types construct, they are not values.
                        if self.layouts.contains_key(&(module.clone(), attr.clone())) {
                            return Err(err(
                                *span,
                                format!("type '{module}.{attr}' cannot be used as a value; construct it"),
                            ));
                        }
                        let g = mangle_global(&module, attr);
                        let v = self.reg();
                        self.w(&format!("  {v} = load %NxVal, ptr @{g}"));
                        let ty = self
                            .types
                            .get(&(module.clone(), "<top>".to_string()))
                            .and_then(|f| f.locals.get(attr))
                            .cloned()
                            .unwrap_or(Ty::Unknown);
                        return Ok(NV::boxed_known(v, ty));
                    }
                }
                // A record field. When the static type is known the offset
                // is a constant; when it is not, the name is resolved
                // against the value's own descriptor at runtime.
                let b = self.emit_expr(base)?;
                let bv = self.unbox(&b);
                match &b.ty {
                    Ty::Record(t) => {
                        let (decl_module, _, fields) = self
                            .resolve_type(t)
                            .ok_or(err(*span, format!("unknown type '{t}'")))?;
                        let _ = decl_module;
                        let i = Self::field_index(&fields, t, attr, *span)?;
                        let r = self.reg();
                        self.w(&format!(
                            "  {r} = call %NxVal @nx_rec_get(%NxVal {bv}, i64 {i})"
                        ));
                        // The field's static type is not tracked past the
                        // declaration (it may be `Any`), so the result is
                        // dynamic. Specialising it is the unboxed-fields
                        // pass, which comes with the ownership work.
                        Ok(NV::dyn_boxed(r))
                    }
                    Ty::Unknown => {
                        // The field-name bytes come from an ordinary
                        // string constant; its payload is the byte
                        // pointer `nx_rec_getn` compares, and the length
                        // is known at compile time.
                        let s = self.emit_str(attr, *span)?;
                        let nv = NV::boxed_known(s, Ty::Str);
                        let pbits = self.payload(&nv);
                        let p = self.reg();
                        self.w(&format!("  {p} = inttoptr i64 {pbits} to ptr"));
                        let r = self.reg();
                        self.w(&format!(
                            "  {r} = call %NxVal @nx_rec_getn(%NxVal {bv}, ptr {p}, i64 {})",
                            attr.len()
                        ));
                        Ok(NV::dyn_boxed(r))
                    }
                    _ => Err(err(
                        *span,
                        "only modules and types have attributes".to_string(),
                    )),
                }
            }
            Expr::Index { base, index, .. } => {
                let b = self.emit_expr(base)?;
                let ix = self.emit_expr(index)?;
                let elem = match &b.ty {
                    Ty::List(t) => (**t).clone(),
                    Ty::Str => Ty::Str,
                    _ => Ty::Unknown,
                };
                let bv = self.unbox(&b);
                let iv = self.unbox(&ix);
                // nx_index returns the element itself, so pulling the
                // payload straight out of the result costs nothing and
                // keeps its bounds check.
                let r = self.reg();
                self.w(&format!(
                    "  {r} = call %NxVal @nx_index(%NxVal {bv}, %NxVal {iv})"
                ));
                if ll_scalar(&elem).is_some() {
                    if let Some(raw) = self.as_raw(&NV::boxed_known(r.clone(), elem.clone())) {
                        return Ok(raw);
                    }
                }
                Ok(NV::boxed_known(r, elem))
            }
            Expr::Unary { op, expr, .. } => {
                let v = self.emit_expr(expr)?;
                // Only a value already in a raw register takes the direct
                // path; a box of a known scalar still goes through the
                // runtime helper, which also re-checks the tag.
                if matches!(v.raw, Some(Ty::Int)) && matches!(op, UnaryOp::Neg) {
                    // Negating MIN overflows, so this goes through the
                    // checked intrinsic like every other Int subtraction.
                    let r = self.reg();
                    self.emit_i64_checked("llvm.ssub.with.overflow.i64", "0", &v.reg, &r);
                    return Ok(NV::raw(Ty::Int, r));
                }
                if matches!(v.raw, Some(Ty::Float)) && matches!(op, UnaryOp::Neg) {
                    let r = self.reg();
                    self.w(&format!("  {r} = fneg double {}", v.reg));
                    return Ok(NV::raw(Ty::Float, r));
                }
                if matches!(v.raw, Some(Ty::Bool)) && matches!(op, UnaryOp::Not) {
                    let r = self.reg();
                    self.w(&format!("  {r} = xor i1 {}, true", v.reg));
                    return Ok(NV::raw(Ty::Bool, r));
                }
                if matches!(v.raw, Some(Ty::Int)) && matches!(op, UnaryOp::BitNot) {
                    let r = self.reg();
                    self.w(&format!("  {r} = xor i64 {}, -1", v.reg));
                    return Ok(NV::raw(Ty::Int, r));
                }
                if v.raw.is_some() && matches!(op, UnaryOp::Pos) {
                    return Ok(v);
                }
                let b = self.unbox(&v);
                let r = self.reg();
                match op {
                    UnaryOp::Neg => self.w(&format!("  {r} = call %NxVal @nx_neg(%NxVal {b})")),
                    UnaryOp::Not => self.w(&format!("  {r} = call %NxVal @nx_not(%NxVal {b})")),
                    UnaryOp::BitNot => {
                        self.w(&format!("  {r} = call %NxVal @nx_bitnot(%NxVal {b})"))
                    }
                    // Unary plus changes nothing, so it returns the operand.
                    UnaryOp::Pos => return Ok(v),
                }
                let ty = match op {
                    UnaryOp::BitNot => Ty::Int,
                    _ => Ty::Unknown,
                };
                // Every helper above allocates its result box.
                Ok(NV::fresh_boxed(r, ty))
            }
            Expr::Binary {
                left,
                op,
                right,
                span,
            } => {
                if matches!(op, BinOp::And | BinOp::Or) {
                    return self.emit_logic(left, *op, right);
                }
                let l = self.emit_expr(left)?;
                let r = self.emit_expr(right)?;
                let _ = span;
                self.binop_dyn(&l, *op, &r)
            }
            Expr::Call { callee, args, span } => self.emit_call(callee, args, *span),
        }
    }

    pub(crate) fn emit_str(&mut self, s: &str, _span: Span) -> Result<String, CodegenError> {
        let bytes = s.as_bytes();
        let n = bytes.len();
        let id = self.strc;
        self.strc += 1;
        let mut esc = String::new();
        for b in bytes {
            match b {
                b'"' => esc.push_str("\\22"),
                b'\\' => esc.push_str("\\5C"),
                b'\n' => esc.push_str("\\0A"),
                32..=126 => esc.push(*b as char),
                _ => esc.push_str(&format!("\\{b:02X}")),
            }
        }
        if n == 0 {
            self.top.push_str(&format!(
                "@.nxstr.{id} = private constant [1 x i8] zeroinitializer\n"
            ));
        } else {
            self.top.push_str(&format!(
                "@.nxstr.{id} = private constant [{n} x i8] c\"{esc}\"\n"
            ));
        }
        let rawlen = if n == 0 { 1 } else { n };
        let p = self.reg();
        let r = self.reg();
        self.w(&format!(
            "  {p} = getelementptr [{rawlen} x i8], ptr @.nxstr.{id}, i64 0, i64 0"
        ));
        self.w(&format!("  {r} = call %NxVal @nx_str(ptr {p}, i64 {n})"));
        Ok(r)
    }

    pub(crate) fn emit_logic(
        &mut self,
        left: &Expr,
        op: BinOp,
        right: &Expr,
    ) -> Result<NV, CodegenError> {
        // Diamond with a single deciding branch:
        //   br i1 <decide>, label %short, label %rhs     (and)
        //   br i1 <decide>, label %rhs, label %short     (or)
        // short: br merge      (result = decided value)
        // rhs:   <right> -> rb; br merge
        // merge: phi [decided, short], [rb, rhs]
        let l = self.emit_expr(left)?;
        let lb = self.as_i1(&l);
        let rhs = self.lab("rhs");
        let short = self.lab("short");
        let merge = self.lab("merge");
        match op {
            BinOp::And => self.w(&format!("  br i1 {lb}, label %{rhs}, label %{short}")),
            _ => self.w(&format!("  br i1 {lb}, label %{short}, label %{rhs}")),
        }
        let decided = match op {
            BinOp::And => "false",
            _ => "true",
        };
        self.block(&format!("{short}"));
        self.w(&format!("  br label %{merge}"));
        self.block(&format!("{rhs}"));
        let rv = self.emit_expr(right)?;
        let rb = self.as_i1(&rv);
        // The right operand can open blocks of its own -- `xs[i - 1] > xs[i]`
        // puts a checked subtraction and its diamond in here -- so the value
        // may no longer live in `%rhs`. The phi below names the block it does
        // live in, so funnel first.
        let held = self.funnel(&rhs);
        // RHS is an expression: it cannot terminate (no return/break inside).
        self.w(&format!("  br label %{merge}"));
        self.block(&format!("{merge}"));
        // The result is always a proven Bool: both operands had to be.
        let phi = self.reg();
        self.w(&format!(
            "  {phi} = phi i1 [{decided}, %{short}], [{rb}, %{held}]"
        ));
        Ok(NV::raw(Ty::Bool, phi))
    }
}
