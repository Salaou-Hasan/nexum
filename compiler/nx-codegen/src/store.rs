//! Storage: assignment targets, compound ops, and deletion.

use crate::core::{Gen, LoopVarScope};
use crate::value::{ll_scalar, NV};
use crate::{err, CodegenError};
use nx_ast::{BinOp, Expr, Span};
use nx_types::Ty;

impl Gen {
    /// Store to a name, allocating a local or using the module global.
    /// Write through an assignment target.
    ///
    /// A name goes through the ordinary store, so module globals and Unique
    /// locals behave exactly as they did before. An index or dict key is the
    /// interesting case: it has to write into the container's own storage,
    /// and `nx_dictset` / `nx_listset` update the value in place, which is
    /// what makes `a[i] = v` visible to every other reference to `a`.
    pub(crate) fn store_target(
        &mut self,
        target: &nx_ast::Target,
        v: &NV,
        span: Span,
    ) -> Result<(), CodegenError> {
        match target {
            nx_ast::Target::Name(name) => self.store_name(name, v, span),
            nx_ast::Target::Index { base, index } => self.store_index(base, index, v, span),
            nx_ast::Target::Attr { base, field } => {
                let b = self.emit_expr(base)?;
                let bv = self.unbox(&b);
                let vb = self.store_boxed(v);
                match &b.ty {
                    Ty::Record(t) => {
                        let (_, _, fields) = self
                            .resolve_type(t)
                            .ok_or(err(span, format!("unknown type '{t}'")))?;
                        let i = Self::field_index(&fields, t, field, span)?;
                        self.w(&format!(
                            "  call void @nx_rec_set(%NxVal {bv}, i64 {i}, %NxVal {vb})"
                        ));
                        Ok(())
                    }
                    // A dynamic base resolves the field by name at runtime.
                    Ty::Unknown => {
                        let s = self.emit_str(field, span)?;
                        let nv = NV::boxed_known(s, Ty::Str);
                        let pbits = self.payload(&nv);
                        let p = self.reg();
                        self.w(&format!("  {p} = inttoptr i64 {pbits} to ptr"));
                        self.w(&format!(
                            "  call void @nx_rec_setn(%NxVal {bv}, ptr {p}, i64 {}, %NxVal {vb})",
                            field.len()
                        ));
                        Ok(())
                    }
                    _ => Err(err(span, "only types have fields".to_string())),
                }
            }
        }
    }

    /// `a[i] = v` or `d[k] = v`.
    ///
    /// A list is written in place through its header, so the change is
    /// visible to every other reference to that list -- aliasing a list and
    /// then writing to it has to behave the same as writing to the original.
    /// A dict's length can grow, which reallocates its storage, so the
    /// updated value is written back to whoever holds it.
    pub(crate) fn store_index(
        &mut self,
        base: &Expr,
        index: &Expr,
        v: &NV,
        _span: Span,
    ) -> Result<(), CodegenError> {
        let b = self.emit_expr(base)?;
        let ix = self.emit_expr(index)?;
        let bv = self.unbox(&b);
        // The stored value is owned by the container, so it is duplicated
        // on the way in -- the same rule as every other store.
        let sv = self.store_boxed(v);
        if matches!(b.ty, Ty::Dict(_)) {
            let kb = self.unbox(&ix);
            let vb = sv;
            // nx_dictset updates the mirrored length in place, so it needs
            // an addressable copy of the dict value.
            let p = self.alloca("%NxVal");
            self.w(&format!("  store %NxVal {bv}, ptr {p}"));
            self.w(&format!(
                "  call void @nx_dictset(ptr {p}, %NxVal {kb}, %NxVal {vb})"
            ));
            let updated = self.reg();
            self.w(&format!("  {updated} = load %NxVal, ptr {p}"));
            let nty = Ty::Dict(Box::new(Ty::Unknown));
            return self.write_back(base, &NV::boxed_known(updated, nty));
        }
        if matches!(b.ty, Ty::Unknown) {
            // Unresolved base: the tag decides list versus dict at runtime.
            // The updated value comes back out because a dict may have
            // grown, which moves its mirrored length.
            let kb = self.unbox(&ix);
            let out = self.reg();
            self.w(&format!(
                "  {out} = call %NxVal @nx_storeindex(%NxVal {bv}, %NxVal {kb}, %NxVal {sv})"
            ));
            return self.write_back(base, &NV::boxed_known(out, Ty::Unknown));
        }
        let k = self.as_i64(&ix);
        self.w(&format!(
            "  call void @nx_listset(%NxVal {bv}, i64 {k}, %NxVal {sv})"
        ));
        Ok(())
    }

    /// After an in-place container update the header pointer and length may
    /// have moved, so the owning binding is refreshed. `a` is the common
    /// case and a nested index writes its container back through the same
    /// path recursively.
    pub(crate) fn write_back(&mut self, base: &Expr, updated: &NV) -> Result<(), CodegenError> {
        match base {
            Expr::Var(name, _) => {
                // No clone: `updated` derives from this same binding's
                // storage (an in-place update refreshed the header or
                // length), so there is no second owner to separate from.
                // Cloning here would deep-copy the container on every
                // indexed write.
                if !self.locals.contains_key(name) && self.in_init {
                    let g = self.ensure_global(&self.cur_module.clone(), name);
                    let b = self.unbox(updated);
                    self.w(&format!("  store %NxVal {b}, ptr {g}"));
                } else if self.locals.contains_key(name) {
                    self.store_slot_owned(name, updated);
                } else {
                    self.new_slot(name, None);
                    self.store_slot_owned(name, updated);
                }
                Ok(())
            }
            Expr::Index {
                base: inner,
                index,
                span,
            } => {
                // `a[i][j] = v`: the element list is written back into
                // `a[i]`, and its own length has to be refreshed too.
                self.store_index(inner, index, updated, *span)
            }
            _ => Ok(()),
        }
    }

    /// A hidden `$augN` slot for one evaluated value. `$` cannot appear in
    /// a user identifier (the lexer rejects it), so the name cannot collide
    /// with any binding the program declares -- including another temp.
    pub(crate) fn temp_slot(&mut self) -> String {
        let n = self.tmp;
        self.tmp += 1;
        let name = format!("$aug{n}");
        self.new_slot(&name, None);
        name
    }

    /// Evaluate a compound-assignment target's parts once, in order, and
    /// rebuild the target off hidden slots holding the results. The caller
    /// then emits the value, reads the rebuilt target, computes, and
    /// stores back -- every source expression runs exactly once, target
    /// parts before the value, which is Python's evaluation order.
    ///
    /// The slots are boxed and unknown to the memory planner, so they read
    /// as `Shared`: nothing is freed early, and the final store clones on
    /// the way into the container exactly as a direct emission would.
    pub(crate) fn stash_compound_target(
        &mut self,
        target: &nx_ast::Target,
        span: Span,
    ) -> Result<nx_ast::Target, CodegenError> {
        match target {
            nx_ast::Target::Name(_) => Ok(target.clone()),
            nx_ast::Target::Index { base, index } => {
                let b = self.emit_expr(base)?;
                let tb = self.temp_slot();
                self.store_slot_owned(&tb, &b);
                let ix = self.emit_expr(index)?;
                let ti = self.temp_slot();
                self.store_slot_owned(&ti, &ix);
                Ok(nx_ast::Target::Index {
                    base: Box::new(Expr::Var(tb, span)),
                    index: Box::new(Expr::Var(ti, span)),
                })
            }
            nx_ast::Target::Attr { base, field } => {
                let b = self.emit_expr(base)?;
                let tb = self.temp_slot();
                self.store_slot_owned(&tb, &b);
                Ok(nx_ast::Target::Attr {
                    base: Box::new(Expr::Var(tb, span)),
                    field: field.clone(),
                })
            }
        }
    }

    /// `a, b = f()` -- pull a list's elements into separate bindings.
    pub(crate) fn destructure(
        &mut self,
        v: &NV,
        targets: &[nx_ast::Target],
        span: Span,
    ) -> Result<(), CodegenError> {
        let b = self.unbox(v);
        for (i, t) in targets.iter().enumerate() {
            let idx = self.emit_i64(i as i64);
            let idxreg = self.unbox(&idx);
            let item = self.reg();
            self.w(&format!(
                "  {item} = call %NxVal @nx_index(%NxVal {b}, %NxVal {idxreg})"
            ));
            // Unpacked elements keep whatever type the list carried; a
            // heterogeneous tuple therefore stays dynamic, which is correct.
            let ety = match &v.ty {
                Ty::List(t) => (**t).clone(),
                _ => Ty::Unknown,
            };
            let nv = NV::boxed_known(item, ety);
            self.store_target(t, &nv, span)?;
        }
        Ok(())
    }

    /// `del a`, `del a[i]`, `del d[k]`.
    pub(crate) fn del_target(
        &mut self,
        target: &nx_ast::Target,
        span: Span,
    ) -> Result<(), CodegenError> {
        match target {
            nx_ast::Target::Name(name) => {
                // Unbind by rebinding to None: NX has no destructors, and
                // dropping the only reference to a Unique buffer would leak
                // it. The binding goes dead for every later read.
                self.modrefs.remove(name);
                self.falias.remove(name);
                // Rebinding to None is enough: NX has no destructors, so the
                // buffer a Unique local held is released by the process
                // rather than here. What matters is that later reads see
                // a defined binding instead of stale data.
                let n = self.reg();
                self.w(&format!("  {n} = call %NxVal @nx_none()"));
                self.store_name(name, &NV::boxed_known(n, Ty::None), span)
            }
            nx_ast::Target::Index { base, index } => {
                let b = self.emit_expr(base)?;
                let ix = self.emit_expr(index)?;
                // An unresolved base dispatches on the tag at runtime;
                // the updated container comes back out for the write-back.
                if matches!(b.ty, Ty::Unknown) {
                    let bv = self.unbox(&b);
                    let kb = self.unbox(&ix);
                    let out = self.reg();
                    self.w(&format!(
                        "  {out} = call %NxVal @nx_delindex(%NxVal {bv}, %NxVal {kb})"
                    ));
                    return self.write_back(base, &NV::boxed_known(out, Ty::Unknown));
                }
                match b.ty {
                    Ty::Dict(_) => {
                        let bv = self.unbox(&b);
                        let kb = self.unbox(&ix);
                        let out = self.reg();
                        self.w(&format!(
                            "  {out} = call %NxVal @nx_dictdel(%NxVal {bv}, %NxVal {kb})"
                        ));
                        self.write_back(
                            base,
                            &NV::boxed_known(out, Ty::Dict(Box::new(Ty::Unknown))),
                        )
                    }
                    _ => {
                        let bv = self.unbox(&b);
                        let k = self.as_i64(&ix);
                        let out = self.reg();
                        self.w(&format!(
                            "  {out} = call %NxVal @nx_listdel(%NxVal {bv}, i64 {k})"
                        ));
                        let bt = match &b.ty {
                            Ty::List(t) => Ty::List(t.clone()),
                            other => other.clone(),
                        };
                        self.write_back(base, &NV::boxed_known(out, bt))
                    }
                }
            }
            nx_ast::Target::Attr { base, field } => {
                let b = self.emit_expr(base)?;
                let bv = self.unbox(&b);
                // A record's arity is fixed, so removing a value means
                // blanking the field rather than changing the layout --
                // a record's arity is fixed, so deleting a field blanks it.
                let none = self.reg();
                self.w(&format!("  {none} = call %NxVal @nx_none()"));
                match &b.ty {
                    Ty::Record(t) => {
                        let (_, _, fields) = self
                            .resolve_type(t)
                            .ok_or(err(span, format!("unknown type '{t}'")))?;
                        let i = Self::field_index(&fields, t, field, span)?;
                        self.w(&format!(
                            "  call void @nx_rec_set(%NxVal {bv}, i64 {i}, %NxVal {none})"
                        ));
                        Ok(())
                    }
                    Ty::Unknown => {
                        let s = self.emit_str(field, span)?;
                        let nv = NV::boxed_known(s, Ty::Str);
                        let pbits = self.payload(&nv);
                        let p = self.reg();
                        self.w(&format!("  {p} = inttoptr i64 {pbits} to ptr"));
                        self.w(&format!(
                            "  call void @nx_rec_setn(%NxVal {bv}, ptr {p}, i64 {}, %NxVal {none})",
                            field.len()
                        ));
                        Ok(())
                    }
                    _ => Err(err(span, "only types have fields".to_string())),
                }
            }
        }
    }

    /// `name op= value`, preserving the unboxed fast path for scalars.
    pub(crate) fn store_compound(
        &mut self,
        name: &str,
        op: BinOp,
        rhs: &NV,
        span: Span,
    ) -> Result<(), CodegenError> {
        // Local slot: reuse the scalar path when the variable's
        // representation allows it, so `x += 1` stays unboxed.
        //
        // No `in_init` guard: `rep_of` is Some only for a name that has a
        // local slot, which is what this is really asking. The old guard
        // also skipped every top-level compound assignment, so `t += 1` in
        // module-level code went through the boxed helper even though the
        // slot was a bare i64 right there.
        if self.rep_of(name).is_some() {
            let cur = self.load_slot(name);
            if let Some(v) = self.emit_named_binop(&cur, op, rhs) {
                self.store_slot(name, &v);
                return Ok(());
            }
        }
        let ptr = self
            .ptr_of(name)
            .ok_or(err(span, format!("undefined variable '{name}'")))?;
        let cur = self.reg();
        self.w(&format!("  {cur} = load %NxVal, ptr {ptr}"));
        let cur_v = NV::dyn_boxed(cur.clone());
        let v = self.binop_dyn(&cur_v, op, rhs)?;
        let b = self.unbox(&v);
        if self.is_unique(name) {
            self.w(&format!("  call void @nx_free_val(%NxVal {cur})"));
        }
        self.w(&format!("  store %NxVal {b}, ptr {ptr}"));
        Ok(())
    }

    pub(crate) fn store_name(
        &mut self,
        name: &str,
        v: &NV,
        _span: Span,
    ) -> Result<(), CodegenError> {
        // A local slot wins over module scope when one exists.
        //
        // The order matters and used to be wrong. A loop variable has a slot
        // (it is scoped to its loop), but this function used to test
        // `in_init` first and write to a module global instead -- so a
        // `mut self` method's write-back landed in module scope while the
        // read on the next line read the local slot. The update was
        // silently lost, which is the worst shape a bug can have: the code
        // compiled, ran, and printed the wrong thing.
        //
        // A genuine module variable has no local slot, so it still reaches
        // the global branch, which other modules read by address.
        if self.locals.contains_key(name) {
            // Rebinding a Unique local: release the old buffers first. An
            // unboxed slot holds a bare scalar with nothing to free.
            if self.is_unique(name) && self.rep_of(name).is_none() {
                let slot = self.locals[name].clone();
                let old = self.reg();
                self.w(&format!("  {old} = load %NxVal, ptr {slot}"));
                self.w(&format!("  call void @nx_free_val(%NxVal {old})"));
            }
            self.store_slot(name, v);
            return Ok(());
        }
        if self.in_init {
            let g = self.ensure_global(&self.cur_module.clone(), name);
            let b = self.store_boxed(v);
            self.w(&format!("  store %NxVal {b}, ptr {g}"));
            return Ok(());
        }
        self.new_slot(name, self.unboxed_ty(name));
        self.store_slot(name, v);
        Ok(())
    }

    /// Always-allocate store (from-imports, loop vars).
    pub(crate) fn store_fresh(&mut self, name: &str, v: &NV) {
        if self.in_init {
            let g = self.ensure_global(&self.cur_module.clone(), name);
            let b = self.store_boxed(v);
            self.w(&format!("  store %NxVal {b}, ptr {g}"));
            return;
        }
        // Loop variables and imports are fresh bindings: any previous slot
        // for the name (a loop re-entry, a shadowed import) is replaced.
        self.locals.remove(name);
        self.rep.remove(name);
        self.new_slot(name, self.unboxed_ty(name));
        self.store_slot(name, v);
    }

    /// Bind a loop variable for the duration of its body, and return the
    /// scope that puts the previous binding back.
    ///
    /// Always a local slot, never a module global — including at top level,
    /// where `store_fresh` would take the `in_init` path. A loop variable is
    /// not visible after the loop, so giving it module scope leaks it into
    /// every later statement and lets two loops collide on the name. Inside
    /// a nested scope that the collision would make two bindings share one
    /// slot.
    pub(crate) fn bind_loop_var(&mut self, name: &str, v: &NV) -> LoopVarScope {
        let saved_slot = self.locals.remove(name);
        let saved_rep = self.rep.remove(name);
        self.new_slot(name, self.unboxed_ty(name));
        self.store_slot(name, v);
        LoopVarScope {
            name: name.to_string(),
            saved_slot,
            saved_rep,
        }
    }

    /// Representation a fresh local should get: the inferred scalar type
    /// when unboxing is on, otherwise None for a boxed slot.
    pub(crate) fn unboxed_ty(&self, name: &str) -> Option<Ty> {
        if !self.unbox_on {
            return None;
        }
        let t = self.ty_of(name);
        ll_scalar(&t)?;
        Some(t)
    }
}
