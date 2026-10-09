//! Statement lowering.

use super::scope::FnLower;
use super::tables::NameRef;
use super::ty::{copy_rule, decide_bin_rule, resolve_type_name};
use super::{lerr, LResult};
use crate::model::*;
use nx_ast::{Span, Stmt, Target};

impl<'a> FnLower<'a> {
    pub(crate) fn lower_block(&mut self, body: &[Stmt], out: &mut Vec<HStmt>) -> LResult<()> {
        for s in body {
            self.lower_stmt(s, out)?;
        }
        Ok(())
    }

    pub(crate) fn lower_stmt(&mut self, s: &Stmt, out: &mut Vec<HStmt>) -> LResult<()> {
        match s {
            Stmt::Assign {
                targets,
                values,
                span,
            } => {
                // All right-hand sides evaluate before any store (so
                // `a, b = b, a` swaps), exactly like the backend.
                let mut hvalues = Vec::with_capacity(values.len());
                for v in values {
                    hvalues.push(self.lower_expr(v)?);
                }
                if targets.len() > 1 && values.len() == 1 {
                    // Destructuring: every target takes from the one tuple.
                    let mut htargets = Vec::with_capacity(targets.len());
                    for t in targets {
                        htargets.push(self.lower_assign_target(t, *span)?);
                    }
                    let rules = vec![copy_rule(&hvalues[0].ty)];
                    out.push(self.stmt(
                        *span,
                        None,
                        HStmtKind::Assign {
                            targets: htargets,
                            values: hvalues,
                            rules,
                        },
                    ));
                    return Ok(());
                }
                if targets.len() != values.len() {
                    return Err(lerr(
                        *span,
                        format!(
                            "internal: {} targets but {} values",
                            targets.len(),
                            values.len()
                        ),
                    ));
                }
                let mut htargets = Vec::with_capacity(targets.len());
                let mut rules = Vec::with_capacity(values.len());
                for (t, v) in targets.iter().zip(hvalues.iter()) {
                    htargets.push(self.lower_assign_target(t, *span)?);
                    rules.push(copy_rule(&v.ty));
                }
                out.push(self.stmt(
                    *span,
                    None,
                    HStmtKind::Assign {
                        targets: htargets,
                        values: hvalues,
                        rules,
                    },
                ));
                Ok(())
            }
            Stmt::AssignOp {
                target,
                op,
                value,
                span,
            } => {
                // Name targets keep the dedicated path; element and field
                // targets stash their evaluated parts first (see
                // `stash_target`), so the target runs before the value
                // and every part runs exactly once.
                match target {
                    Target::Name(name) => {
                        let (t, cur_ty) = self.assign_name_target(name, *span)?;
                        let v = self.lower_expr(value)?;
                        let rule = decide_bin_rule(*op, &cur_ty, &v.ty, *span)?;
                        out.push(self.stmt(
                            *span,
                            None,
                            HStmtKind::AssignOp {
                                target: t,
                                op: *op,
                                rule,
                                value: v,
                            },
                        ));
                        Ok(())
                    }
                    _ => {
                        let rebuilt = self.stash_target(target, *span)?;
                        let v = self.lower_expr(value)?;
                        let cur = self.read_target(&rebuilt, *span)?;
                        let rule = decide_bin_rule(*op, &cur.ty, &v.ty, *span)?;
                        out.push(self.stmt(
                            *span,
                            None,
                            HStmtKind::AssignOp {
                                target: rebuilt,
                                op: *op,
                                rule,
                                value: v,
                            },
                        ));
                        Ok(())
                    }
                }
            }
            Stmt::Print { values, span } => {
                let mut vs = Vec::with_capacity(values.len());
                for v in values {
                    vs.push(self.lower_expr(v)?);
                }
                out.push(self.stmt(*span, None, HStmtKind::Print { values: vs }));
                Ok(())
            }
            Stmt::If {
                cond,
                then_body,
                elifs,
                else_body,
                span,
            } => {
                let c = self.lower_expr(cond)?;
                let mut then_b = Vec::new();
                self.lower_block(then_body, &mut then_b)?;
                let mut helifs = Vec::with_capacity(elifs.len());
                for (ec, eb) in elifs {
                    let hc = self.lower_expr(ec)?;
                    let mut hb = Vec::new();
                    self.lower_block(eb, &mut hb)?;
                    helifs.push((hc, hb));
                }
                let hel = match else_body {
                    Some(b) => {
                        let mut hb = Vec::new();
                        self.lower_block(b, &mut hb)?;
                        Some(hb)
                    }
                    None => None,
                };
                out.push(self.stmt(
                    *span,
                    None,
                    HStmtKind::If {
                        cond: c,
                        then_body: then_b,
                        elifs: helifs,
                        else_body: hel,
                    },
                ));
                Ok(())
            }
            Stmt::While { cond, body, span } => {
                let c = self.lower_expr(cond)?;
                let mut hb = Vec::new();
                self.lower_block(body, &mut hb)?;
                out.push(self.stmt(*span, None, HStmtKind::While { cond: c, body: hb }));
                Ok(())
            }
            Stmt::For {
                var,
                iter,
                body,
                span,
            } => match iter {
                nx_ast::ForIter::Range { start, end } => {
                    let s = self.lower_expr(start)?;
                    let e = self.lower_expr(end)?;
                    let slot = self.alloc_slot(HTy::Int);
                    let saved = self.env.insert(var.clone(), NameRef::Slot(slot));
                    let mut hb = Vec::new();
                    self.lower_block(body, &mut hb)?;
                    self.restore_env(var, saved);
                    out.push(self.stmt(
                        *span,
                        Some(var),
                        HStmtKind::ForRange {
                            var: slot,
                            start: s,
                            end: e,
                            body: hb,
                        },
                    ));
                    Ok(())
                }
                nx_ast::ForIter::Each(e) => {
                    let it = self.lower_expr(e)?;
                    let rule = self.iter_rule(&it.ty, *span)?;
                    let ety = self.iter_elem_ty(&it.ty, *span)?;
                    let slot = self.alloc_slot(ety);
                    let saved = self.env.insert(var.clone(), NameRef::Slot(slot));
                    let mut hb = Vec::new();
                    self.lower_block(body, &mut hb)?;
                    self.restore_env(var, saved);
                    out.push(self.stmt(
                        *span,
                        Some(var),
                        HStmtKind::ForEach {
                            var: slot,
                            iter: it,
                            rule,
                            body: hb,
                        },
                    ));
                    Ok(())
                }
            },
            Stmt::Return { values, span } => {
                let mut vs = Vec::with_capacity(values.len());
                for v in values {
                    vs.push(self.lower_expr(v)?);
                }
                out.push(self.stmt(*span, None, HStmtKind::Return { values: vs }));
                Ok(())
            }
            Stmt::Break { span } => {
                out.push(self.stmt(*span, None, HStmtKind::Break));
                Ok(())
            }
            Stmt::Continue { span } => {
                out.push(self.stmt(*span, None, HStmtKind::Continue));
                Ok(())
            }
            Stmt::Del { targets, span } => {
                let mut hs = Vec::with_capacity(targets.len());
                for t in targets {
                    if let Some(h) = self.lower_del_target(t, *span)? {
                        hs.push(h);
                    }
                }
                out.push(self.stmt(*span, None, HStmtKind::Del { targets: hs }));
                Ok(())
            }
            Stmt::Assert {
                cond,
                message,
                span,
            } => {
                let c = self.lower_expr(cond)?;
                let m = match message {
                    Some(e) => Some(self.lower_expr(e)?),
                    None => None,
                };
                out.push(self.stmt(
                    *span,
                    None,
                    HStmtKind::Assert {
                        cond: c,
                        message: m,
                    },
                ));
                Ok(())
            }
            Stmt::Expr(e) => {
                let h = self.lower_expr(e)?;
                out.push(self.stmt(h.span, None, HStmtKind::Expr(h)));
                Ok(())
            }
            // Declarations leave no nodes: types, methods and functions
            // were indexed in phase 1 and lower separately.
            Stmt::TypeDecl { .. } | Stmt::Fn { .. } | Stmt::Impl { .. } => Ok(()),
            Stmt::Import {
                module,
                alias,
                span,
            } => {
                let mid = self.module_id(module, *span)?;
                let bind = alias.clone().unwrap_or_else(|| module.clone());
                self.env.insert(bind.clone(), NameRef::Module(mid));
                out.push(self.stmt(*span, Some(&bind), HStmtKind::EnsureInit { module: mid }));
                Ok(())
            }
            Stmt::FromImport {
                module,
                names,
                span,
            } => {
                let mid = self.module_id(module, *span)?;
                out.push(self.stmt(*span, Some(module), HStmtKind::EnsureInit { module: mid }));
                for (name, alias) in names {
                    let bind = alias.clone().unwrap_or_else(|| name.clone());
                    self.import_name(mid, module, name, &bind, *span, out)?;
                }
                Ok(())
            }
        }
    }

    /// An assignment target as a write position. Index and field parts
    /// are lowered (they evaluate at runtime); plain names resolve to
    /// their slot or global. Evaluation order lives in the `Assign`
    /// contract (target parts, then values, then stores), not here.
    pub(crate) fn lower_assign_target(&mut self, t: &Target, span: Span) -> LResult<HTarget> {
        match t {
            Target::Name(n) => Ok(self.assign_name_target(n, span)?.0),
            Target::Index { base, index, .. } => {
                let b = self.lower_expr(base)?;
                let ix = self.lower_expr(index)?;
                let rule = self.index_rule(&b.ty, span)?;
                Ok(HTarget::Index {
                    base: Box::new(b),
                    index: Box::new(ix),
                    rule,
                })
            }
            Target::Attr { base, field, .. } => {
                let b = self.lower_expr(base)?;
                let fr = self.field_ref(&b.ty, field, span)?;
                Ok(HTarget::Field {
                    base: Box::new(b),
                    field: fr,
                })
            }
        }
    }

    /// A bare-name write position: the existing slot in a function
    /// (rebinding never touches a same-named global), the pre-declared
    /// global at top level. Shadowing a module, function or type alias
    /// births a slot (the checker proved the name game legal).
    /// Returns the target and the slot/global type.
    pub(crate) fn assign_name_target(&mut self, name: &str, span: Span) -> LResult<(HTarget, HTy)> {
        if let Some(r) = self.env.get(name).cloned() {
            match r {
                NameRef::Slot(s) => return Ok((HTarget::Slot(s), self.slot_ty(s))),
                NameRef::Global(g) => return Ok((HTarget::Global(g), self.global_ty(g))),
                _ => {}
            }
        }
        if self.is_top {
            let g = self.global_id(name, span)?;
            let ty = self.global_ty(g);
            self.env.insert(name.to_string(), NameRef::Global(g));
            return Ok((HTarget::Global(g), ty));
        }
        let ty = self.local_ty(name, span)?;
        let s = self.alloc_slot(ty.clone());
        self.env.insert(name.to_string(), NameRef::Slot(s));
        Ok((HTarget::Slot(s), ty))
    }

    /// A global by name in the current module (reads only; writes go
    /// through `assign_name_target`).
    pub(crate) fn global_id(&self, name: &str, span: Span) -> LResult<GlobalId> {
        self.ctx
            .global_ids
            .get(&(self.module.clone(), name.to_string()))
            .copied()
            .ok_or_else(|| {
                lerr(
                    span,
                    format!("internal: global '{name}' was never declared"),
                )
            })
    }

    /// Read a write position back as a value (compound assignment and
    /// `mut self` write-back). Reuses the target's already-lowered parts:
    /// lowering them again would run side effects twice.
    pub(crate) fn read_target(&self, t: &HTarget, span: Span) -> LResult<HExpr> {
        match t {
            HTarget::Slot(s) => {
                let ty = self.slot_ty(*s);
                Ok(self.expr(span, None, ty, HExprKind::Place(Place::Slot(*s))))
            }
            HTarget::Global(g) => {
                let ty = self.global_ty(*g);
                Ok(self.expr(span, None, ty, HExprKind::Place(Place::Global(*g))))
            }
            HTarget::Index { base, index, rule } => {
                let ty = self.index_elem_ty(&base.ty, span)?;
                Ok(self.expr(
                    span,
                    None,
                    ty,
                    HExprKind::Index {
                        base: base.clone(),
                        index: index.clone(),
                        rule: *rule,
                    },
                ))
            }
            HTarget::Field { base, field } => {
                let ty = self.field_ty_of(&base.ty, *field, span)?;
                Ok(self.expr(
                    span,
                    None,
                    ty,
                    HExprKind::Field {
                        base: base.clone(),
                        field: *field,
                    },
                ))
            }
        }
    }

    /// Evaluate a compound-assignment target's parts once, in order, and
    /// rebuild the target off the lowered parts. The caller lowers the
    /// value next, then reads through `read_target`: each source
    /// expression is lowered exactly once, target parts before the value,
    /// which is Python's evaluation order.
    pub(crate) fn stash_target(&mut self, t: &Target, span: Span) -> LResult<HTarget> {
        self.lower_assign_target(t, span)
    }

    pub(crate) fn restore_env(&mut self, var: &str, saved: Option<NameRef>) {
        match saved {
            Some(r) => {
                self.env.insert(var.to_string(), r);
            }
            None => {
                self.env.remove(var);
            }
        }
    }

    pub(crate) fn module_id(&self, name: &str, span: Span) -> LResult<ModuleId> {
        // A module alias in this body, else any known module. Imports
        // bind the alias; the bare name resolves when some loaded module
        // has it (checked programs always import before use here).
        if let Some(NameRef::Module(mid)) = self.env.get(name) {
            return Ok(*mid);
        }
        if let Some(mid) = self.ctx.tables.module_id.get(name) {
            return Ok(*mid);
        }
        Err(lerr(
            span,
            format!("internal: module '{name}' is not imported here"),
        ))
    }

    /// Bind one from-imported name: module refs, functions and types
    /// alias directly; a value global materializes as an initializing
    /// assignment into a fresh slot (eager, exactly like the backend's
    /// fresh local -- a lazy global read would see later mutations).
    pub(crate) fn import_name(
        &mut self,
        mid: ModuleId,
        module: &str,
        name: &str,
        bind: &str,
        span: Span,
        out: &mut Vec<HStmt>,
    ) -> LResult<()> {
        let home = &self.ctx.tables.modules_sorted[mid.0 as usize];
        if self
            .ctx
            .tables
            .type_id
            .contains_key(&(home.clone(), name.to_string()))
        {
            let tid = resolve_type_name(self.ctx.tables, home, name).ok_or_else(|| {
                lerr(span, format!("internal: unresolvable import type '{name}'"))
            })?;
            self.env.insert(bind.to_string(), NameRef::Type(tid));
            return Ok(());
        }
        if let Some(fid) = self
            .ctx
            .tables
            .func_id
            .get(&(home.clone(), name.to_string()))
        {
            self.env.insert(bind.to_string(), NameRef::Func(*fid));
            return Ok(());
        }
        let gid = self
            .ctx
            .global_ids
            .get(&(home.clone(), name.to_string()))
            .copied()
            .ok_or_else(|| {
                lerr(
                    span,
                    format!("internal: '{module}.{name}' is not an importable value"),
                )
            })?;
        let ty = self.global_ty(gid);
        let s = self.alloc_slot(ty.clone());
        self.env.insert(bind.to_string(), NameRef::Slot(s));
        let read = self.expr(
            span,
            Some(name),
            ty.clone(),
            HExprKind::Place(Place::Global(gid)),
        );
        out.push(self.stmt(
            span,
            Some(bind),
            HStmtKind::Assign {
                targets: vec![HTarget::Slot(s)],
                values: vec![read],
                rules: vec![copy_rule(&ty)],
            },
        ));
        Ok(())
    }

    pub(crate) fn lower_del_target(
        &mut self,
        t: &Target,
        span: Span,
    ) -> LResult<Option<HDelTarget>> {
        match t {
            Target::Name(n) => match self.env.get(n).cloned() {
                Some(NameRef::Slot(s)) => {
                    self.env.remove(n);
                    Ok(Some(HDelTarget {
                        target: HTarget::Slot(s),
                        rule: DelRule::Unbind,
                    }))
                }
                Some(NameRef::Global(g)) => {
                    self.env.remove(n);
                    Ok(Some(HDelTarget {
                        target: HTarget::Global(g),
                        rule: DelRule::Unbind,
                    }))
                }
                // Module, function and type aliases hold no storage: the
                // checker proved any later use undefined, so forgetting
                // the alias is the whole effect.
                Some(_) => {
                    self.env.remove(n);
                    Ok(None)
                }
                None => Err(lerr(span, format!("internal: deleting unbound '{n}'"))),
            },
            Target::Index { base, index, .. } => {
                let b = self.lower_expr(base)?;
                let ix = self.lower_expr(index)?;
                let rule = match &b.ty {
                    HTy::List(_) => DelRule::ListRemove,
                    HTy::Dict(_) => DelRule::DictRemove,
                    HTy::Unknown => DelRule::Dynamic,
                    other => {
                        return Err(lerr(
                            span,
                            format!("internal: cannot delete into {other:?}"),
                        ));
                    }
                };
                let index_rule = match rule {
                    DelRule::ListRemove => IndexRule::ListInt,
                    DelRule::DictRemove => IndexRule::DictKey,
                    _ => IndexRule::Dynamic,
                };
                Ok(Some(HDelTarget {
                    target: HTarget::Index {
                        base: Box::new(b),
                        index: Box::new(ix),
                        rule: index_rule,
                    },
                    rule,
                }))
            }
            Target::Attr { base, field, .. } => {
                let b = self.lower_expr(base)?;
                match &b.ty {
                    HTy::Record(_) => {
                        let fr = self.field_ref(&b.ty, field, span)?;
                        Ok(Some(HDelTarget {
                            target: HTarget::Field {
                                base: Box::new(b),
                                field: fr,
                            },
                            rule: DelRule::RecordBlank,
                        }))
                    }
                    HTy::Unknown => {
                        let fr = self.field_ref(&b.ty, field, span)?;
                        Ok(Some(HDelTarget {
                            target: HTarget::Field {
                                base: Box::new(b),
                                field: fr,
                            },
                            rule: DelRule::Dynamic,
                        }))
                    }
                    other => Err(lerr(
                        span,
                        format!("internal: cannot delete a field of {other:?}"),
                    )),
                }
            }
        }
    }
}
