//! The HIR verifier: every rule in `docs/architecture/hir.md` §7, as a
//! pass over plain data.
//!
//! HIR nodes are deliberately *representable* when malformed, so this
//! pass is the discipline. Each rule is checkable on hand-built HIR,
//! which is what makes the negative tests (one per rule, below) possible:
//! if malformed HIR were unrepresentable in Rust, those tests could not
//! exist.
//!
//! The verifier checks structure and consistency, never semantics. It
//! cannot tell a wrong-but-well-formed program from a right one; oracle
//! duty belongs to the expected-value tests. Lowering runs it on its own
//! output, so a lowering bug fails at the lowering site with the rule
//! and span named, rather than three stages later.

use super::model::*;

use nx_ast::UnaryOp;

/// One way a program breaks one verifier rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    /// The rule this breaks, e.g. `"V3"`.
    pub rule: &'static str,
    pub span: Span,
    pub message: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}:{}: {}", self.rule, self.span.line, self.span.col, self.message)
    }
}

/// Verify a whole program. Every violation found is reported, not just
/// the first: a hand-built negative test wants the reason it was built,
/// and lowering bugs want the whole story at one span.
pub fn verify(p: &HProgram) -> Result<(), Vec<Violation>> {
    let mut v = Verifier { p, out: Vec::new() };
    v.tables();
    v.functions();
    if v.out.is_empty() {
        Ok(())
    } else {
        Err(v.out)
    }
}

/// The same program with every diagnostic name erased. Used by the V12
/// strip test: verification must not depend on any original spelling.
pub fn strip_diag(p: &HProgram) -> HProgram {
    let mut q = p.clone();
    for f in &mut q.funcs {
        f.diag = DiagInfo::default();
        block_strip(&mut f.body);
    }
    for t in &mut q.types {
        t.diag = DiagInfo::default();
    }
    for m in &mut q.methods {
        m.diag = DiagInfo::default();
    }
    for g in &mut q.globals {
        g.diag = DiagInfo::default();
    }
    for m in &mut q.modules {
        m.diag = DiagInfo::default();
    }
    q
}

fn block_strip(b: &mut [HStmt]) {
    for s in b {
        s.diag = DiagInfo::default();
        match &mut s.kind {
            HStmtKind::Assign { targets, values, .. } => {
                for v in values {
                    expr_strip(v);
                }
                for t in targets {
                    target_strip(t);
                }
            }
            HStmtKind::AssignOp { target, value, .. } => {
                target_strip(target);
                expr_strip(value);
            }
            HStmtKind::Print { values } => values.iter_mut().for_each(expr_strip),
            HStmtKind::If { cond, then_body, elifs, else_body } => {
                expr_strip(cond);
                block_strip(then_body);
                for (c, b) in elifs {
                    expr_strip(c);
                    block_strip(b);
                }
                if let Some(b) = else_body {
                    block_strip(b);
                }
            }
            HStmtKind::While { cond, body } => {
                expr_strip(cond);
                block_strip(body);
            }
            HStmtKind::ForRange { start, end, body, .. } => {
                expr_strip(start);
                expr_strip(end);
                block_strip(body);
            }
            HStmtKind::ForEach { iter, body, .. } => {
                expr_strip(iter);
                block_strip(body);
            }
            HStmtKind::Return { values } => values.iter_mut().for_each(expr_strip),
            HStmtKind::Del { targets } => {
                for t in targets {
                    target_strip(&mut t.target);
                }
            }
            HStmtKind::Assert { cond, message } => {
                expr_strip(cond);
                if let Some(m) = message {
                    expr_strip(m);
                }
            }
            HStmtKind::Expr(e) => expr_strip(e),
            HStmtKind::EnsureInit { .. } | HStmtKind::Break | HStmtKind::Continue => {}
        }
    }
}

fn target_strip(t: &mut HTarget) {
    match t {
        HTarget::Index { base, index, .. } => {
            expr_strip(base);
            expr_strip(index);
        }
        HTarget::Field { base, .. } => expr_strip(base),
        HTarget::Slot(_) | HTarget::Global(_) => {}
    }
}

fn expr_strip(e: &mut HExpr) {
    e.diag = DiagInfo::default();
    match &mut e.kind {
        HExprKind::List(items) => items.iter_mut().for_each(expr_strip),
        HExprKind::Range { start, end, .. } => {
            expr_strip(start);
            expr_strip(end);
        }
        HExprKind::Dict(pairs) => {
            for (k, v) in pairs {
                expr_strip(k);
                expr_strip(v);
            }
        }
        HExprKind::Field { base, .. } => expr_strip(base),
        HExprKind::Index { base, index, .. } => {
            expr_strip(base);
            expr_strip(index);
        }
        HExprKind::Slice { base, from, to, step, .. } => {
            expr_strip(base);
            for b in [from, to, step].into_iter().flatten() {
                expr_strip(b);
            }
        }
        HExprKind::Unary { operand, .. } => expr_strip(operand),
        HExprKind::Binary { left, right, .. }
        | HExprKind::Equal { left, right, .. }
        | HExprKind::Compare { left, right, .. }
        | HExprKind::Logic { left, right, .. } => {
            expr_strip(left);
            expr_strip(right);
        }
        HExprKind::Contains { needle, hay, .. } => {
            expr_strip(needle);
            expr_strip(hay);
        }
        HExprKind::Select { cond, then_value, else_value } => {
            expr_strip(cond);
            expr_strip(then_value);
            expr_strip(else_value);
        }
        HExprKind::Compr { element, iter, cond, .. } => {
            expr_strip(iter);
            expr_strip(element);
            if let Some(c) = cond {
                expr_strip(c);
            }
        }
        HExprKind::CallFn { args, .. } | HExprKind::Builtin { args, .. } => {
            args.iter_mut().for_each(expr_strip)
        }
        HExprKind::Construct { args, .. } => args.iter_mut().for_each(expr_strip),
        HExprKind::CallMethod { receiver, args, writeback, .. } => {
            if let Some(r) = receiver {
                expr_strip(r);
            }
            args.iter_mut().for_each(expr_strip);
            if let Some(w) = writeback {
                target_strip(w);
            }
        }
        HExprKind::Int(_)
        | HExprKind::Float(_)
        | HExprKind::Bool(_)
        | HExprKind::Str(_)
        | HExprKind::None
        | HExprKind::Place(_) => {}
    }
}

/// Per-function state: which slots are bound so far, what the function
/// returns, and how deeply loops are nested (V1, V8, V9).
struct FnCtx {
    bound: Vec<bool>,
    ret: HTy,
    loop_depth: usize,
}

impl FnCtx {
    fn bind(&mut self, s: Slot) {
        let i = s.0 as usize;
        if self.bound.len() <= i {
            self.bound.resize(i + 1, false);
        }
        self.bound[i] = true;
    }

    fn unbind(&mut self, s: Slot) {
        if let Some(slot) = self.bound.get_mut(s.0 as usize) {
            *slot = false;
        }
    }

    fn is_bound(&self, s: Slot) -> bool {
        self.bound.get(s.0 as usize).copied().unwrap_or(false)
    }
}

struct Verifier<'a> {
    p: &'a HProgram,
    out: Vec<Violation>,
}

impl<'a> Verifier<'a> {
    fn err(&mut self, rule: &'static str, span: Span, message: impl Into<String>) {
        self.out.push(Violation { rule, span, message: message.into() });
    }

    fn module(&self, m: ModuleId) -> Option<&HModule> {
        self.p.modules.get(m.0 as usize)
    }

    fn func(&self, f: FuncId) -> Option<&HFunc> {
        self.p.funcs.get(f.0 as usize)
    }

    fn method(&self, m: MethodId) -> Option<&HMethod> {
        self.p.methods.get(m.0 as usize)
    }

    fn record(&self, t: TypeId) -> Option<&HType> {
        self.p.types.get(t.0 as usize)
    }

    fn fields_of(&self, t: &HTy) -> Option<&[HTy]> {
        match t {
            HTy::Record(id) => Some(&self.record(*id)?.fields),
            _ => None,
        }
    }

    fn str_in_range(&mut self, s: StrId, span: Span) {
        if s.0 as usize >= self.p.strings.len() {
            self.err(
                "V2",
                span,
                format!("string id {} with {} interned", s.0, self.p.strings.len()),
            );
        }
    }

    // -----------------------------------------------------------------
    // V2: every ID in range of its table, checked once over the tables.
    // -----------------------------------------------------------------
    fn tables(&mut self) {
        let nowhere = Span { line: 1, col: 1 };
        for t in &self.p.types {
            if t.module.0 as usize >= self.p.modules.len() {
                self.err("V2", nowhere, format!("type in module id {}", t.module.0));
            }
        }
        for m in &self.p.methods {
            if self.record(m.type_id).is_none() {
                self.err("V2", nowhere, format!("method on missing type id {}", m.type_id.0));
            }
            if self.func(m.func).is_none() {
                self.err("V2", nowhere, format!("method body id {} missing", m.func.0));
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
                    self.err("V2", nowhere, format!("module lists missing global {}", g.0));
                }
            }
            for t in &m.types {
                if self.record(*t).is_none() {
                    self.err("V2", nowhere, format!("module lists missing type {}", t.0));
                }
            }
            for f in &m.funcs {
                if self.func(*f).is_none() {
                    self.err("V2", nowhere, format!("module lists missing function {}", f.0));
                }
            }
            if self.func(m.top).is_none() {
                self.err("V2", nowhere, format!("module top {} is not a function", m.top.0));
            }
        }
        if self.module(self.p.entry).is_none() {
            self.err("V2", nowhere, format!("entry module {} missing", self.p.entry.0));
        }
    }

    fn functions(&mut self) {
        for i in 0..self.p.funcs.len() {
            self.function(FuncId(i as u32));
        }
    }

    fn function(&mut self, fid: FuncId) {
        let f = self.func(fid).expect("checked above").clone();
        // Slot 0..n are the parameters, in order; every other slot is
        // bound by the body, which is why the bound set starts as just
        // the parameters and grows as the walk meets bindings.
        let mut ctx = FnCtx { bound: Vec::new(), ret: f.ret.clone(), loop_depth: 0 };
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
            HStmtKind::Assign { targets, values, rules } => {
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
                        self.err("V2", s.span, format!("{} copy rules for one value", rules.len()));
                    }
                } else if rules.len() != values.len() {
                    self.err(
                        "V2",
                        s.span,
                        format!("{} copy rules for {} values", rules.len(), values.len()),
                    );
                }
            }
            HStmtKind::AssignOp { target, rule, value, .. } => {
                self.target(target, ctx, false);
                self.expr(value, ctx);
                // V3 needs both operand types. The function table carries
                // parameter types but not slot types (hir.md §2), so the
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
                    self.err("V2", s.span, format!("import of missing module {}", module.0));
                }
            }
            HStmtKind::If { cond, then_body, elifs, else_body } => {
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
            HStmtKind::ForRange { var, start, end, body } => {
                self.expr(start, ctx);
                self.expr(end, ctx);
                self.int_bound(start, s.span);
                self.int_bound(end, s.span);
                ctx.bind(*var);
                ctx.loop_depth += 1;
                self.block(body, ctx);
                ctx.loop_depth -= 1;
            }
            HStmtKind::ForEach { var, iter, body, .. } => {
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
                        (DelRule::Unbind, HTarget::Slot(_)) | (DelRule::Unbind, HTarget::Global(_)) => {}
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
    fn target(&mut self, t: &HTarget, ctx: &mut FnCtx, binding: bool) {
        match t {
            HTarget::Slot(s) => {
                if binding {
                    ctx.bind(*s);
                } else if !ctx.is_bound(*s) {
                    self.err("V1", Span { line: 1, col: 1 }, format!("write to unbound slot {}", s.0));
                }
            }
            HTarget::Global(g) => {
                if g.0 as usize >= self.p.globals.len() {
                    self.err("V2", Span { line: 1, col: 1 }, format!("missing global {}", g.0));
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
    fn cond(&mut self, e: &HExpr, ctx: &mut FnCtx, span: Span) {
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

    fn expr(&mut self, e: &HExpr, ctx: &mut FnCtx) {
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
                    let want = self
                        .fields_of(&base.ty)
                        .and_then(|f| f.get(idx.0))
                        .cloned();
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
            HExprKind::Slice { base, from, to, step, rule } => {
                self.expr(base, ctx);
                if !matches!(rule, SliceRule::ListCopy | SliceRule::StrChars | SliceRule::Dynamic) {
                    self.err("V2", e.span, "unknown slice rule");
                }
                if !matches!(
                    base.ty,
                    HTy::List(_) | HTy::Str | HTy::Unknown
                ) {
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
            HExprKind::Binary { left, op, rule, right } => {
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
                    BinRule::Arith(ArithRule::Dynamic) | BinRule::Pow(PowRule::Dynamic)
                    | BinRule::Dynamic => None,
                };
                if let Some(want) = want {
                    self.exact(e, ctx, want);
                }
                let _ = op;
            }
            HExprKind::Equal { left, right, rule, .. } => {
                self.expr(left, ctx);
                self.expr(right, ctx);
                self.exact(e, ctx, HTy::Bool);
                self.eq_rule(*rule, &left.ty, &right.ty, e.span);
            }
            HExprKind::Compare { left, right, rule, .. } => {
                self.expr(left, ctx);
                self.expr(right, ctx);
                self.exact(e, ctx, HTy::Bool);
                self.cmp_rule(*rule, &left.ty, &right.ty, e.span);
            }
            HExprKind::Contains { needle, hay, rule, .. } => {
                self.expr(needle, ctx);
                self.expr(hay, ctx);
                self.exact(e, ctx, HTy::Bool);
                match (rule, &hay.ty) {
                    (MemberRule::ListEq, HTy::List(_))
                    | (MemberRule::ListEq, HTy::Unknown) => {}
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
            HExprKind::Select { cond, then_value, else_value } => {
                self.cond(cond, ctx, e.span);
                self.expr(then_value, ctx);
                self.expr(else_value, ctx);
                if !compatible(&then_value.ty, &else_value.ty) {
                    self.err(
                        "V4",
                        e.span,
                        format!(
                            "branches disagree: {} and {}",
                            then_value.ty,
                            else_value.ty
                        ),
                    );
                }
            }
            HExprKind::Compr { element, var, iter, rule, cond } => {
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
                        self.err("V2", e.span, format!("call passes {} of {} args", args.len(), want));
                    }
                }
                for a in args {
                    self.expr(a, ctx);
                }
            }
            HExprKind::Construct { type_id, args } => {
                match self.record(*type_id).cloned() {
                    None => self.err("V2", e.span, format!("construct of missing type {}", type_id.0)),
                    Some(t) => {
                        // V7: a constructor fills every field, or it is
                        // not a constructor.
                        if args.len() != t.fields.len() {
                            self.err(
                                "V7",
                                e.span,
                                format!("{} args for a {}-field record", args.len(), t.fields.len()),
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
                    self.err("V2", e.span, format!("builtin takes {} args, got {}", want.len(), args.len()));
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
                            HExprKind::Place(_)
                                | HExprKind::Index { .. }
                                | HExprKind::Field { .. }
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
            HExprKind::CallMethod { method, receiver, args, writeback } => self.method_call(
                e,
                *method,
                receiver.as_deref(),
                args,
                writeback.as_ref(),
                ctx,
            ),
        }
    }

    /// V6: the receiver is the method's declared type, arity matches,
    /// and the call's type is the body's return type.
    fn method_call(
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
                self.err("V2", e.span, format!("method '{}' needs a receiver", label(&m.diag, mid)));
            }
            (false, true) => {
                self.err("V2", e.span, format!("associated function '{}' takes no receiver", label(&m.diag, mid)));
            }
            _ => {}
        }
        if let Some(r) = receiver {
            if r.ty != HTy::Record(m.type_id) {
                self.err(
                    "V6",
                    e.span,
                    format!("receiver is {} but the method is on type {}", r.ty, m.type_id.0),
                );
            }
        }
        // A write-back only makes sense for a `mut self` method.
        if writeback.is_some() && !matches!(m.receiver, Some(ReceiverKind::Mut)) {
            self.err("V6", e.span, "write-back on a method that is not mut self");
        }
        let Some(body) = self.func(m.func).cloned() else { return };
        let want = body.params.len() - usize::from(takes_receiver);
        if args.len() != want {
            self.err("V2", e.span, format!("method '{}' takes {want} args, got {}", label(&m.diag, mid), args.len()));
        }
        if !compatible(&e.ty, &body.ret) {
            self.err(
                "V6",
                e.span,
                format!("method '{}' returns {} here", label(&m.diag, mid), body.ret),
            );
        }
    }

    fn place(&mut self, p: Place, e: &HExpr, ctx: &mut FnCtx) {
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

    /// V5: a static field index is inside the base record's layout.
    fn field(&mut self, base: &HExpr, f: FieldRef, span: Span) {
        match (f, &base.ty) {
            (FieldRef::Static(i), HTy::Record(tid)) => match self.record(*tid) {
                None => self.err("V2", span, format!("missing record type {}", tid.0)),
                Some(t) => {
                    if i.0 >= t.fields.len() {
                        self.err(
                            "V5",
                            span,
                            format!("field {} of a {}-field record", i.0, t.fields.len()),
                        );
                    }
                }
            },
            // Only a statically unknown base keeps a runtime name, and
            // the name must be interned.
            (FieldRef::Dynamic(s), HTy::Unknown) => self.str_in_range(s, span),
            (FieldRef::Dynamic(s), other) => {
                self.str_in_range(s, span);
                self.err(
                    "V5",
                    span,
                    format!("dynamic field name on a known {}", other),
                );
            }
            (FieldRef::Static(_), other) => {
                self.err("V5", span, format!("field offset on a {}", other));
            }
        }
    }

    /// V10: index and slice operands.
    fn index(&mut self, base: &HExpr, index: &HExpr, rule: IndexRule, span: Span) {
        match rule {
            IndexRule::ListInt | IndexRule::StrChar => {
                if !matches!(index.ty, HTy::Int | HTy::Unknown) {
                    self.err("V10", span, format!("index is {} not Int", index.ty));
                }
            }
            IndexRule::DictKey => {
                if !keyable(&index.ty) {
                    self.err("V10", span, format!("dict key is {}", index.ty));
                }
            }
            IndexRule::Dynamic => {}
        }
        match (rule, &base.ty) {
            (IndexRule::ListInt, HTy::List(_)) | (IndexRule::ListInt, HTy::Unknown) => {}
            (IndexRule::StrChar, HTy::Str) | (IndexRule::StrChar, HTy::Unknown) => {}
            (IndexRule::DictKey, HTy::Dict(_)) | (IndexRule::DictKey, HTy::Unknown) => {}
            (IndexRule::Dynamic, _) => {}
            _ => self.err("V2", span, format!("index rule does not fit {}", base.ty)),
        }
    }

    fn int_bound(&mut self, b: &HExpr, span: Span) {
        if !matches!(b.ty, HTy::Int | HTy::Unknown) {
            self.err("V11", span, format!("bound is {} not Int", b.ty));
        }
    }

    /// V3: an arithmetic rule matches the operand types it was decided
    /// from, and the node carries the resulting type.
    fn bin_rule(&mut self, rule: BinRule, l: &HTy, r: &HTy, span: Span) {
        match rule {
            BinRule::Arith(ArithRule::Trap) => {
                if !matches!((l, r), (HTy::Int, HTy::Int)) {
                    self.err(
                        "V3",
                        span,
                        format!("trapping arithmetic on {} and {}", l, r),
                    );
                }
            }
            BinRule::Arith(ArithRule::Float) => {
                if !matches!((l, r), (HTy::Float, HTy::Float)) {
                    self.err(
                        "V3",
                        span,
                        format!("float arithmetic on {} and {}", l, r),
                    );
                }
            }
            BinRule::Arith(ArithRule::PromoteFloat) => {
                let ok = matches!(
                    (l, r),
                    (HTy::Int, HTy::Float) | (HTy::Float, HTy::Int)
                );
                if !ok {
                    self.err(
                        "V3",
                        span,
                        format!("float promotion on {} and {}", l, r),
                    );
                }
            }
            BinRule::Arith(ArithRule::Dynamic) => {
                if !(matches!(l, HTy::Unknown) || matches!(r, HTy::Unknown)) {
                    self.err("V3", span, "dynamic arithmetic on two known operands");
                }
            }
            BinRule::Pow(PowRule::Saturate) => {
                if !matches!((l, r), (HTy::Int, HTy::Int)) {
                    self.err("V3", span, "saturating power on non-Int operands");
                }
            }
            BinRule::Pow(PowRule::Dynamic) | BinRule::Dynamic => {
                if !(matches!(l, HTy::Unknown) || matches!(r, HTy::Unknown)) {
                    self.err("V3", span, "dynamic rule on two known operands");
                }
            }
            BinRule::Bitwise => {
                if !matches!((l, r), (HTy::Int, HTy::Int)) {
                    self.err("V3", span, "bitwise operator on non-Int operands");
                }
            }
            BinRule::Concat => {
                if !matches!((l, r), (HTy::Str, HTy::Str)) {
                    self.err("V3", span, "concatenation on non-Str operands");
                }
            }
        }
    }

    fn eq_rule(&mut self, rule: EqRule, l: &HTy, r: &HTy, span: Span) {
        let numeric = |t: &HTy| matches!(t, HTy::Int | HTy::Float | HTy::Unknown);
        let ok = match rule {
            EqRule::Numeric => numeric(l) && numeric(r),
            EqRule::StrEq => matches!(l, HTy::Str | HTy::Unknown) && matches!(r, HTy::Str | HTy::Unknown),
            EqRule::Structural => {
                matches!(l, HTy::List(_) | HTy::Dict(_) | HTy::Record(_))
                    && matches!(r, HTy::List(_) | HTy::Dict(_) | HTy::Record(_))
            }
            EqRule::IdentityNone => matches!(l, HTy::None) && matches!(r, HTy::None),
            EqRule::Dynamic => matches!(l, HTy::Unknown) || matches!(r, HTy::Unknown),
        };
        if !ok {
            self.err("V3", span, format!("{} equality on {} and {}", name_of(rule), l, r));
        }
    }

    fn cmp_rule(&mut self, rule: CmpRule, l: &HTy, r: &HTy, span: Span) {
        let numeric = |t: &HTy| matches!(t, HTy::Int | HTy::Float | HTy::Unknown);
        let ok = match rule {
            CmpRule::Numeric => numeric(l) && numeric(r),
            CmpRule::StrOrder => matches!(l, HTy::Str | HTy::Unknown) && matches!(r, HTy::Str | HTy::Unknown),
            CmpRule::Dynamic => matches!(l, HTy::Unknown) || matches!(r, HTy::Unknown),
        };
        if !ok {
            self.err("V3", span, format!("ordering on {} and {}", l, r));
        }
    }

    fn unary_rule(&mut self, rule: UnaryRule, op: UnaryOp, operand: &HExpr, e: &HExpr) {
        let span = e.span;
        let ok = match rule {
            UnaryRule::Neg(ArithRule::Trap) => matches!(op, UnaryOp::Neg) && operand.ty == HTy::Int,
            UnaryRule::Neg(ArithRule::Float) => matches!(op, UnaryOp::Neg) && operand.ty == HTy::Float,
            UnaryRule::Neg(ArithRule::PromoteFloat) | UnaryRule::Neg(ArithRule::Dynamic) => matches!(op, UnaryOp::Neg),
            UnaryRule::Not => matches!(op, UnaryOp::Not) && matches!(operand.ty, HTy::Bool | HTy::Unknown),
            UnaryRule::BitNot => matches!(op, UnaryOp::BitNot) && matches!(operand.ty, HTy::Int | HTy::Unknown),
            UnaryRule::Pos => matches!(op, UnaryOp::Pos) && matches!(operand.ty, HTy::Int | HTy::Float | HTy::Unknown),
            UnaryRule::Dynamic => operand.ty == HTy::Unknown,
        };
        if !ok {
            self.err("V3", span, format!("{rule:?} on {}", operand.ty));
        }
    }

    fn exact(&mut self, e: &HExpr, ctx: &mut FnCtx, want: HTy) {
        let _ = ctx;
        if e.ty != want {
            self.err("V3", e.span, format!("node is {} but must be {}", e.ty, want));
        }
    }

    /// A list-typed node whose element type is fixed.
    fn exact_elem(&mut self, e: &HExpr, want: HTy) {
        if e.ty != HTy::List(Box::new(want.clone())) {
            self.err("V3", e.span, format!("node is {} but must be a list of {}", e.ty, want));
        }
    }
}

/// A method's original spelling for a message, or its ID when the
/// diagnostic sidecar was stripped.
fn label(d: &DiagInfo, id: MethodId) -> String {
    d.name.clone().unwrap_or_else(|| format!("#{}", id.0))
}

fn name_of(rule: EqRule) -> &'static str {
    match rule {
        EqRule::Numeric => "numeric",
        EqRule::StrEq => "string",
        EqRule::Structural => "structural",
        EqRule::IdentityNone => "none-identity",
        EqRule::Dynamic => "dynamic",
    }
}

/// Lattice compatibility, mirroring the checker's `compatible`.
fn compatible(a: &HTy, b: &HTy) -> bool {
    a == b || matches!(a, HTy::Unknown) || matches!(b, HTy::Unknown)
}

/// A dict key type, mirroring the checker's `is_keyable`.
fn keyable(t: &HTy) -> bool {
    matches!(t, HTy::Int | HTy::Float | HTy::Bool | HTy::Str | HTy::Unknown)
}