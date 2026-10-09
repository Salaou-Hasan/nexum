//! Program and statement walk: table-level ID checks (V2), the
//! per-function walk, and every statement rule (V1 binding, V2 shape,
//! V4 conditions, V6 method calls, V8 returns, V9 loop control).

use crate::model::*;

use super::compatible;
use super::label;
use super::FnCtx;
use super::Verifier;

impl<'a> Verifier<'a> {
    // -----------------------------------------------------------------
    // V2: every ID in range of its table, checked once over the tables.
    // -----------------------------------------------------------------
    pub(crate) fn tables(&mut self) {
        let nowhere = Span { line: 1, col: 1 };
        for t in &self.p.types {
            if t.module.0 as usize >= self.p.modules.len() {
                self.err("V2", nowhere, format!("type in module id {}", t.module.0));
            }
        }
        for m in &self.p.methods {
            if self.record(m.type_id).is_none() {
                self.err(
                    "V2",
                    nowhere,
                    format!("method on missing type id {}", m.type_id.0),
                );
            }
            if self.func(m.func).is_none() {
                self.err(
                    "V2",
                    nowhere,
                    format!("method body id {} missing", m.func.0),
                );
            }
        }
        for g in &self.p.globals {
            if g.module.0 as usize >= self.p.modules.len() {
                self.err("V2", nowhere, format!("global in module id {}", g.module.0));
            }
        }
        for m in &self.p.modules {
            for g in &m.globals {
                if self.p.globals.get(g.0 as usize).is_none() {
                    self.err(
                        "V2",
                        nowhere,
                        format!("module lists missing global {}", g.0),
                    );
                }
            }
            for t in &m.types {
                if self.record(*t).is_none() {
                    self.err("V2", nowhere, format!("module lists missing type {}", t.0));
                }
            }
            for f in &m.funcs {
                if self.func(*f).is_none() {
                    self.err(
                        "V2",
                        nowhere,
                        format!("module lists missing function {}", f.0),
                    );
                }
            }
            if self.func(m.top).is_none() {
                self.err(
                    "V2",
                    nowhere,
                    format!("module top {} is not a function", m.top.0),
                );
            }
        }
        if self.module(self.p.entry).is_none() {
            self.err(
                "V2",
                nowhere,
                format!("entry module {} missing", self.p.entry.0),
            );
        }
    }

    pub(crate) fn functions(&mut self) {
        for i in 0..self.p.funcs.len() {
            self.function(FuncId(i as u32));
        }
    }

    fn function(&mut self, fid: FuncId) {
        let f = self.func(fid).expect("checked above").clone();
        // Slot 0..n are the parameters, in order; every other slot is
        // bound by the body, which is why the bound set starts as just
        // the parameters and grows as the walk meets bindings.
        let mut ctx = FnCtx {
            bound: Vec::new(),
            ret: f.ret.clone(),
            loop_depth: 0,
        };
        for (i, (s, _)) in f.params.iter().enumerate() {
            if s.0 as usize != i {
                self.err(
                    "V2",
                    Span { line: 1, col: 1 },
                    format!("parameter {i} of function {} has slot {}", fid.0, s.0),
                );
            }
            ctx.bind(*s);
        }
        self.block(&f.body, &mut ctx);
    }

    fn block(&mut self, b: &[HStmt], ctx: &mut FnCtx) {
        for s in b {
            self.stmt(s, ctx);
        }
    }

    fn stmt(&mut self, s: &HStmt, ctx: &mut FnCtx) {
        match &s.kind {
            HStmtKind::Assign {
                targets,
                values,
                rules,
            } => {
                // Several targets against one value is destructuring, so
                // the arity check only applies to the positional form.
                let destructuring = targets.len() > 1 && values.len() == 1;
                if !destructuring && targets.len() != values.len() {
                    self.err(
                        "V2",
                        s.span,
                        format!("{} targets but {} values", targets.len(), values.len()),
                    );
                }
                for v in values {
                    self.expr(v, ctx);
                }
                for t in targets {
                    self.target(t, ctx, true);
                }
                if destructuring {
                    // One value, so exactly one copy rule.
                    if rules.len() != 1 {
                        self.err(
                            "V2",
                            s.span,
                            format!("{} copy rules for one value", rules.len()),
                        );
                    }
                } else if rules.len() != values.len() {
                    self.err(
                        "V2",
                        s.span,
                        format!("{} copy rules for {} values", rules.len(), values.len()),
                    );
                }
            }
            HStmtKind::AssignOp {
                target,
                rule,
                value,
                ..
            } => {
                self.target(target, ctx, false);
                self.expr(value, ctx);
                // V3 needs both operand types. The function table carries
                // parameter types but not slot types (hir.md section 2), so the
                // check runs only where the read type is knowable: an
                // element or field target. A name target is checked by
                // the expected-value tests instead.
                if let Some(cur) = self.target_read_ty(target) {
                    self.bin_rule(*rule, &cur, &value.ty, s.span);
                }
            }
            HStmtKind::Print { values } => {
                for v in values {
                    self.expr(v, ctx);
                }
            }
            HStmtKind::EnsureInit { module } => {
                if self.module(*module).is_none() {
                    self.err(
                        "V2",
                        s.span,
                        format!("import of missing module {}", module.0),
                    );
                }
            }
            HStmtKind::If {
                cond,
                then_body,
                elifs,
                else_body,
            } => {
                self.cond(cond, ctx, s.span);
                self.block(then_body, ctx);
                for (c, b) in elifs {
                    self.cond(c, ctx, s.span);
                    self.block(b, ctx);
                }
                if let Some(b) = else_body {
                    self.block(b, ctx);
                }
            }
            HStmtKind::While { cond, body } => {
                self.cond(cond, ctx, s.span);
                ctx.loop_depth += 1;
                self.block(body, ctx);
                ctx.loop_depth -= 1;
            }
            HStmtKind::ForRange {
                var,
                start,
                end,
                body,
            } => {
                self.expr(start, ctx);
                self.expr(end, ctx);
                self.int_bound(start, s.span);
                self.int_bound(end, s.span);
                ctx.bind(*var);
                ctx.loop_depth += 1;
                self.block(body, ctx);
                ctx.loop_depth -= 1;
            }
            HStmtKind::ForEach {
                var, iter, body, ..
            } => {
                self.expr(iter, ctx);
                ctx.bind(*var);
                ctx.loop_depth += 1;
                self.block(body, ctx);
                ctx.loop_depth -= 1;
            }
            HStmtKind::Return { values } => self.ret(values, ctx, s.span),
            HStmtKind::Break => {
                if ctx.loop_depth == 0 {
                    self.err("V9", s.span, "'break' outside any loop");
                }
            }
            HStmtKind::Continue => {
                if ctx.loop_depth == 0 {
                    self.err("V9", s.span, "'continue' outside any loop");
                }
            }
            HStmtKind::Del { targets } => {
                for t in targets {
                    self.target(&t.target, ctx, false);
                    if let HTarget::Slot(s) = t.target {
                        ctx.unbind(s);
                    }
                    match (t.rule, &t.target) {
                        (DelRule::ListRemove, HTarget::Index { rule, .. })
                        | (DelRule::DictRemove, HTarget::Index { rule, .. }) => {
                            let want = match t.rule {
                                DelRule::ListRemove => IndexRule::ListInt,
                                _ => IndexRule::DictKey,
                            };
                            if *rule != want {
                                self.err("V2", s.span, "delete rule disagrees with index rule");
                            }
                        }
                        (DelRule::RecordBlank, HTarget::Field { .. }) => {}
                        (DelRule::Unbind, HTarget::Slot(_))
                        | (DelRule::Unbind, HTarget::Global(_)) => {}
                        (DelRule::Dynamic, _) => {}
                        _ => self.err("V2", s.span, "delete rule does not fit its target"),
                    }
                }
            }
            HStmtKind::Assert { cond, message } => {
                self.cond(cond, ctx, s.span);
                if let Some(m) = message {
                    self.expr(m, ctx);
                }
            }
            HStmtKind::Expr(e) => self.expr(e, ctx),
        }
    }

    /// A write position. `binding` distinguishes a target that
    /// introduces a slot from one that must already exist: assignment
    /// binds, compound assignment and `del` read first.
    pub(crate) fn target(&mut self, t: &HTarget, ctx: &mut FnCtx, binding: bool) {
        match t {
            HTarget::Slot(s) => {
                if binding {
                    ctx.bind(*s);
                } else if !ctx.is_bound(*s) {
                    self.err(
                        "V1",
                        Span { line: 1, col: 1 },
                        format!("write to unbound slot {}", s.0),
                    );
                }
            }
            HTarget::Global(g) => {
                if g.0 as usize >= self.p.globals.len() {
                    self.err(
                        "V2",
                        Span { line: 1, col: 1 },
                        format!("missing global {}", g.0),
                    );
                }
            }
            HTarget::Index { base, index, rule } => {
                self.expr(base, ctx);
                self.expr(index, ctx);
                self.index(base, index, *rule, Span { line: 1, col: 1 });
            }
            HTarget::Field { base, field } => {
                self.expr(base, ctx);
                self.field(base, *field, Span { line: 1, col: 1 });
            }
        }
    }

    /// The type a write position reads back as, when the program can
    /// say: an element's element type, a field's declared type. A name
    /// target has no slot type table to consult, so it answers `None`
    /// rather than a guess.
    fn target_read_ty(&self, t: &HTarget) -> Option<HTy> {
        match t {
            HTarget::Index { base, .. } => match &base.ty {
                HTy::List(e) => Some((**e).clone()),
                HTy::Str => Some(HTy::Str),
                HTy::Unknown => None,
                _ => None,
            },
            HTarget::Field { base, field } => match (&base.ty, field) {
                (HTy::Record(id), FieldRef::Static(i)) => {
                    Some(self.record(*id)?.fields.get(i.0)?.clone())
                }
                _ => None,
            },
            HTarget::Slot(_) | HTarget::Global(_) => None,
        }
    }

    /// V4: a condition is Bool, or dynamic.
    pub(crate) fn cond(&mut self, e: &HExpr, ctx: &mut FnCtx, span: Span) {
        self.expr(e, ctx);
        if !matches!(e.ty, HTy::Bool | HTy::Unknown) {
            self.err("V4", span, format!("condition is {} not Bool", e.ty));
        }
    }

    /// V8: a return matches the declared type. `return a, b` is a list
    /// result (the checker's rule), so its declared type must be a list
    /// and every part must fit the element type.
    fn ret(&mut self, values: &[HExpr], ctx: &mut FnCtx, span: Span) {
        for v in values {
            self.expr(v, ctx);
        }
        match values {
            [] => {
                if !matches!(ctx.ret, HTy::None | HTy::Unknown) {
                    self.err("V8", span, format!("returns nothing from -> {}", ctx.ret));
                }
            }
            [one] => {
                if !compatible(&one.ty, &ctx.ret) {
                    self.err(
                        "V8",
                        span,
                        format!("returns {} from -> {}", one.ty, ctx.ret),
                    );
                }
            }
            many => match &ctx.ret {
                HTy::Unknown => {}
                HTy::List(elem) => {
                    for v in many {
                        if !compatible(&v.ty, elem) {
                            self.err(
                                "V8",
                                span,
                                format!("returns {} inside -> {}", v.ty, ctx.ret),
                            );
                        }
                    }
                }
                other => self.err(
                    "V8",
                    span,
                    format!("returns {} values from -> {other}", many.len()),
                ),
            },
        }
    }

    /// V6: the receiver is the method's declared type, arity matches,
    /// and the call's type is the body's return type.
    pub(crate) fn method_call(
        &mut self,
        e: &HExpr,
        mid: MethodId,
        receiver: Option<&HExpr>,
        args: &[HExpr],
        writeback: Option<&HTarget>,
        ctx: &mut FnCtx,
    ) {
        for a in args {
            self.expr(a, ctx);
        }
        if let Some(r) = receiver {
            self.expr(r, ctx);
        }
        if let Some(w) = writeback {
            self.target(w, ctx, false);
        }
        let Some(m) = self.method(mid).cloned() else {
            self.err("V2", e.span, format!("call of missing method {}", mid.0));
            return;
        };
        let takes_receiver = m.receiver.is_some();
        match (takes_receiver, receiver.is_some()) {
            (true, false) => {
                self.err(
                    "V2",
                    e.span,
                    format!("method '{}' needs a receiver", label(&m.diag, mid)),
                );
            }
            (false, true) => {
                self.err(
                    "V2",
                    e.span,
                    format!(
                        "associated function '{}' takes no receiver",
                        label(&m.diag, mid)
                    ),
                );
            }
            _ => {}
        }
        if let Some(r) = receiver {
            if r.ty != HTy::Record(m.type_id) {
                self.err(
                    "V6",
                    e.span,
                    format!(
                        "receiver is {} but the method is on type {}",
                        r.ty, m.type_id.0
                    ),
                );
            }
        }
        // A write-back only makes sense for a `mut self` method.
        if writeback.is_some() && !matches!(m.receiver, Some(ReceiverKind::Mut)) {
            self.err("V6", e.span, "write-back on a method that is not mut self");
        }
        let Some(body) = self.func(m.func).cloned() else {
            return;
        };
        let want = body.params.len() - usize::from(takes_receiver);
        if args.len() != want {
            self.err(
                "V2",
                e.span,
                format!(
                    "method '{}' takes {want} args, got {}",
                    label(&m.diag, mid),
                    args.len()
                ),
            );
        }
        if !compatible(&e.ty, &body.ret) {
            self.err(
                "V6",
                e.span,
                format!("method '{}' returns {} here", label(&m.diag, mid), body.ret),
            );
        }
    }

    pub(crate) fn place(&mut self, p: Place, e: &HExpr, ctx: &mut FnCtx) {
        match p {
            Place::Slot(s) => {
                if !ctx.is_bound(s) {
                    self.err(
                        "V1",
                        e.span,
                        format!("read of unbound slot {} in function body", s.0),
                    );
                }
            }
            Place::Global(g) => {
                if g.0 as usize >= self.p.globals.len() {
                    self.err("V2", e.span, format!("read of missing global {}", g.0));
                }
            }
        }
    }
}
