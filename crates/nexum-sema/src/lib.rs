//! Semantic analysis: name resolution, type checking, HIR (bootstrap).
//!
//! Every diagnostic carries the source span of the offending construct so the
//! renderer can point at the exact location (Phase 1 gate).

use nexum_diagnostics::Diagnostic;
use nexum_parser::{
    BinOp, Expr, ExprKind, ForIter, Module as AModule, Stmt, StmtKind, TypeExpr as ATypeExpr, UnOp,
};
use nexum_source::Span;

use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Str,
    Unit,
    /// Nominal record type, named by its `type` declaration.
    Record(String),
    /// Fixed-length homogeneous array: element type + length.
    Array(Box<Ty>, usize),
    /// Type of expressions that never produce a value (`panic`).
    Never,
    Unknown,
    Error,
}

impl Ty {
    pub fn name(&self) -> String {
        match self {
            Ty::Int => "Int".to_string(),
            Ty::Float => "Float".to_string(),
            Ty::Bool => "Bool".to_string(),
            Ty::Str => "Str".to_string(),
            Ty::Unit => "Unit".to_string(),
            Ty::Record(n) => n.clone(),
            Ty::Array(elem, n) => format!("[{}; {}]", elem.name(), n),
            Ty::Never => "Never".to_string(),
            Ty::Unknown => "?".to_string(),
            Ty::Error => "<err>".to_string(),
        }
    }
    pub fn is_numeric(&self) -> bool {
        matches!(self, Ty::Int | Ty::Float)
    }
    /// Unknown, Error and Never are compatible with everything (recovery).
    pub fn compatible(&self, other: &Ty) -> bool {
        if *self == Ty::Unknown
            || *other == Ty::Unknown
            || *self == Ty::Error
            || *other == Ty::Error
            || *self == Ty::Never
            || *other == Ty::Never
        {
            return true;
        }
        match (self, other) {
            (Ty::Array(a, n), Ty::Array(b, m)) => n == m && a.compatible(b),
            _ => self == other,
        }
    }
}

// ---- HIR ----
#[derive(Debug, Clone)]
pub struct HirModule {
    /// Record definitions: name -> fields (field name, field type).
    pub records: Vec<(String, Vec<(String, Ty)>)>,
    pub functions: Vec<HirFn>,
}

#[derive(Debug, Clone)]
pub struct HirFn {
    pub name: String,
    pub params: Vec<(String, Ty)>,
    /// Parallel to `params`: `true` means a by-reference (`mut`) parameter.
    pub mut_params: Vec<bool>,
    pub ret: Ty,
    pub body: Vec<HirStmt>,
}

#[derive(Debug, Clone)]
pub enum HirStmt {
    Let {
        name: String,
        mutable: bool,
        ty: Ty,
        value: HirExpr,
    },
    Assign {
        name: String,
        value: HirExpr,
    },
    If {
        cond: HirExpr,
        then_branch: Vec<HirStmt>,
        else_branch: Option<Vec<HirStmt>>,
    },
    While {
        cond: HirExpr,
        body: Vec<HirStmt>,
    },
    For {
        var: String,
        start: HirExpr,
        end: HirExpr,
        body: Vec<HirStmt>,
    },
    Return {
        value: Option<HirExpr>,
    },
    Expr(HirExpr),
}

#[derive(Debug, Clone)]
pub enum HirExpr {
    Int(i64, Ty),
    Float(f64, Ty),
    Bool(bool, Ty),
    Str(String, Ty),
    Var(String, Ty),
    Call {
        callee: String,
        args: Vec<HirExpr>,
        ty: Ty,
    },
    /// Record construction: `Vec2(1.0, 2.0)`.
    Record {
        name: String,
        args: Vec<HirExpr>,
        ty: Ty,
    },
    /// Field projection: `p.x`.
    Field {
        base: Box<HirExpr>,
        name: String,
        ty: Ty,
    },
    /// Functional field update: `p with x = v` (used for field assignment).
    FieldSet {
        base: Box<HirExpr>,
        name: String,
        value: Box<HirExpr>,
        ty: Ty,
    },
    /// Array literal: `[e0, e1, ...]`; `ty` is `Array(elem, n)`.
    Array(Vec<HirExpr>, Ty),
    /// Index read: `a[i]`; `ty` is the element type.
    Index {
        base: Box<HirExpr>,
        index: Box<HirExpr>,
        ty: Ty,
    },
    /// Functional index update: `a[i] = v` as a value; `ty` is the array type.
    IndexSet {
        base: Box<HirExpr>,
        index: Box<HirExpr>,
        value: Box<HirExpr>,
        ty: Ty,
    },
    Binary {
        op: BinOp,
        lhs: Box<HirExpr>,
        rhs: Box<HirExpr>,
        ty: Ty,
    },
    Unary {
        op: UnOp,
        expr: Box<HirExpr>,
        ty: Ty,
    },
}

impl HirExpr {
    pub fn ty(&self) -> Ty {
        match self {
            HirExpr::Int(_, t)
            | HirExpr::Float(_, t)
            | HirExpr::Bool(_, t)
            | HirExpr::Str(_, t)
            | HirExpr::Var(_, t)
            | HirExpr::Call { ty: t, .. }
            | HirExpr::Record { ty: t, .. }
            | HirExpr::Field { ty: t, .. }
            | HirExpr::FieldSet { ty: t, .. }
            | HirExpr::Array(_, t)
            | HirExpr::Index { ty: t, .. }
            | HirExpr::IndexSet { ty: t, .. }
            | HirExpr::Binary { ty: t, .. }
            | HirExpr::Unary { ty: t, .. } => t.clone(),
        }
    }
}

pub struct SemaResult {
    pub hir: Option<HirModule>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn analyze(am: &AModule) -> SemaResult {
    // Pass 0a: collect record names so field types can reference records.
    let mut records: HashMap<String, Vec<(String, Ty)>> = HashMap::new();
    let mut decl_diags: Vec<Diagnostic> = Vec::new();
    let mut record_order: Vec<String> = Vec::new();
    for s in &am.stmts {
        if let StmtKind::TypeDecl { name, .. } = &s.kind {
            if records.contains_key(name) {
                decl_diags.push(
                    Diagnostic::error("T022", format!("duplicate definition of type `{}`", name))
                        .with_span(s.span),
                );
                continue;
            }
            records.insert(name.clone(), Vec::new());
            record_order.push(name.clone());
        }
    }
    // Pass 0b: resolve record fields.
    for s in &am.stmts {
        if let StmtKind::TypeDecl { name, fields } = &s.kind {
            if !records.contains_key(name) {
                continue;
            }
            let mut resolved: Vec<(String, Ty)> = Vec::new();
            for (fname, fty) in fields {
                if resolved.iter().any(|(n, _)| n == fname) {
                    decl_diags.push(
                        Diagnostic::error(
                            "T023",
                            format!("duplicate field `{}` in type `{}`", fname, name),
                        )
                        .with_span(s.span),
                    );
                    continue;
                }
                let t0 = tty(fty, &records);
                let (t, tdiags) = ty_check(s.span, &t0);
                decl_diags.extend(tdiags);
                if t == Ty::Unknown {
                    decl_diags.push(
                        Diagnostic::error(
                            "T024",
                            format!("unknown type `{}` for field `{}`", fty_name(fty), fname),
                        )
                        .with_span(s.span),
                    );
                }
                if t == Ty::Str {
                    decl_diags.push(
                        Diagnostic::error(
                            "T039",
                            "managed `Str` values cannot be stored in records yet: the collector has no type descriptors; keep strings in locals, parameters and returns",
                        )
                        .with_span(s.span),
                    );
                }
                resolved.push((fname.clone(), t));
            }
            if resolved.is_empty() {
                decl_diags.push(
                    Diagnostic::error(
                        "T027",
                        format!("record `{}` must declare at least one field", name),
                    )
                    .with_span(s.span),
                );
            }
            records.insert(name.clone(), resolved);
        }
    }

    // Pass 0c: declared function signatures.
    let mut table: HashMap<String, FnSig> = HashMap::new();
    for s in &am.stmts {
        if let StmtKind::Fn {
            name, params, ret, ..
        } = &s.kind
        {
            if table.contains_key(name) {
                decl_diags.push(
                    Diagnostic::error(
                        "T014",
                        format!("duplicate definition of function `{}`", name),
                    )
                    .with_span(s.span),
                );
                continue;
            }
            let mut ps = Vec::new();
            let mut mps = Vec::new();
            for p in params {
                let t0 = tty_opt(p.ty.as_ref(), &records);
                let (t, tdiags) = ty_check(s.span, &t0);
                decl_diags.extend(tdiags);
                ps.push((p.name.clone(), t));
                mps.push(p.mutable);
            }
            let r0 = ret
                .as_ref()
                .map(|te| tty(te, &records))
                .unwrap_or(Ty::Unknown);
            let (r, rdiags) = ty_check(s.span, &r0);
            decl_diags.extend(rdiags);
            table.insert(
                name.clone(),
                FnSig {
                    params: ps,
                    mut_params: mps,
                    ret: r,
                },
            );
        }
    }

    // Passes 1..N: analyze bodies, refining Unknown parameter and return types
    // from concrete types discovered at call sites and return statements, until
    // the signature table stops changing (refinements are monotone). When the
    // table converges with remaining Unknown entries, one final pass runs with
    // those defaults applied so the retained HIR and diagnostics are computed
    // against a table without Unknown types.
    let mut last: (Vec<Diagnostic>, Vec<HirFn>) = (decl_diags.clone(), Vec::new());
    let mut pass_table = table.clone();
    let mut defaults_applied = false;
    for _ in 0..65 {
        let mut ctx = Ctx {
            diagnostics: decl_diags.clone(),
            functions: pass_table.clone(),
            records: records.clone(),
            current_ret: Ty::Unit,
            current_fn: String::new(),
            synthetic: 0,
        };
        let mut fns = Vec::new();
        for s in &am.stmts {
            if let StmtKind::Fn { name, body, .. } = &s.kind {
                let Some(sig) = ctx.functions.get(name).cloned() else {
                    continue;
                };
                ctx.current_ret = sig.ret.clone();
                ctx.current_fn = name.clone();
                let mut scope = Scope::new();
                for (i, (n, t)) in sig.params.iter().enumerate() {
                    // Plain parameters are immutable copies; `mut` parameters
                    // are mutable by-reference borrows of the caller's binding.
                    scope.insert(
                        n.clone(),
                        t.clone(),
                        sig.mut_params.get(i).copied().unwrap_or(false),
                    );
                }
                let body_hir = analyze_block(&mut ctx, &mut scope, body);
                // Keep the return type Unknown while refining so that later
                // iterations can still learn it; functions that genuinely return
                // no value are resolved to Unit after convergence.
                let final_ret = ctx.current_ret.clone();
                if let Some(entry) = ctx.functions.get_mut(name) {
                    if entry.ret == Ty::Unknown {
                        entry.ret = final_ret.clone();
                    }
                }
                fns.push(HirFn {
                    name: name.clone(),
                    params: sig.params,
                    mut_params: sig.mut_params,
                    ret: final_ret,
                    body: body_hir,
                });
            }
        }
        let converged = ctx.functions == pass_table;
        let diags = ctx.diagnostics.clone();
        pass_table = ctx.functions;
        last = (diags, fns);
        if converged {
            if defaults_applied {
                break;
            }
            defaults_applied = true;
            let has_unknown = pass_table
                .values()
                .any(|s| s.ret == Ty::Unknown || s.params.iter().any(|(_, t)| *t == Ty::Unknown));
            if !has_unknown {
                break;
            }
            // Apply ABI defaults for signatures that never refined and run the
            // final pass: untyped parameters become Int; an untyped return is
            // Int when the body returns values and Unit otherwise.
            for (name, sig) in pass_table.iter_mut() {
                for (_, t) in &mut sig.params {
                    if *t == Ty::Unknown {
                        *t = Ty::Int;
                    }
                }
                if sig.ret == Ty::Unknown {
                    let has_value_ret = am.stmts.iter().any(|s| match &s.kind {
                        StmtKind::Fn {
                            name: fname, body, ..
                        } => fname == name && block_has_value_return(body),
                        _ => false,
                    });
                    sig.ret = if has_value_ret { Ty::Int } else { Ty::Unit };
                }
            }
        }
    }

    let (diags, mut hir_fns) = last;
    // A safety net: any signature entry still Unknown (should not happen after
    // the final pass) is defaulted here as well.
    for f in &mut hir_fns {
        if f.ret == Ty::Unknown {
            f.ret = Ty::Unit;
        }
        for (_, t) in &mut f.params {
            if *t == Ty::Unknown {
                *t = Ty::Int;
            }
        }
    }
    let hir = if diags
        .iter()
        .any(|d| d.severity == nexum_diagnostics::Severity::Error)
    {
        None
    } else {
        let ordered_records = record_order
            .iter()
            .map(|n| (n.clone(), records.get(n).cloned().unwrap_or_default()))
            .collect();
        Some(HirModule {
            records: ordered_records,
            functions: hir_fns,
        })
    };
    SemaResult {
        hir,
        diagnostics: diags,
    }
}

#[derive(Clone, PartialEq)]
struct FnSig {
    params: Vec<(String, Ty)>,
    /// Parallel to `params`: `true` means a by-reference (`mut`) parameter.
    mut_params: Vec<bool>,
    ret: Ty,
}

struct Ctx {
    diagnostics: Vec<Diagnostic>,
    functions: HashMap<String, FnSig>,
    records: HashMap<String, Vec<(String, Ty)>>,
    current_ret: Ty,
    current_fn: String,
    /// Counter for compiler-generated binding names (for-each loops and
    /// compound-assignment temporaries). Generated names contain `$`, which
    /// user identifiers cannot, so they can never collide.
    synthetic: usize,
}

#[derive(Clone)]
struct VarInfo {
    ty: Ty,
    mutable: bool,
}

#[derive(Clone)]
struct Scope {
    vars: HashMap<String, VarInfo>,
}

impl Scope {
    fn new() -> Self {
        Self {
            vars: HashMap::new(),
        }
    }
    fn insert(&mut self, n: String, t: Ty, mutable: bool) {
        self.vars.insert(n, VarInfo { ty: t, mutable });
    }
    fn get(&self, n: &str) -> Option<VarInfo> {
        self.vars.get(n).cloned()
    }
}

fn fty_name(t: &ATypeExpr) -> String {
    match t {
        ATypeExpr::Named(n) => n.clone(),
        ATypeExpr::Array(elem, n) => format!("[{}; {}]", fty_name(elem), n),
    }
}

/// Does this block (recursively) contain a `return <value>`?
fn block_has_value_return(stmts: &[Stmt]) -> bool {
    stmts.iter().any(|s| match &s.kind {
        StmtKind::Return { value: Some(_) } => true,
        StmtKind::If {
            then_branch,
            else_branch,
            ..
        } => {
            block_has_value_return(then_branch)
                || else_branch
                    .as_ref()
                    .map(|b| block_has_value_return(b))
                    .unwrap_or(false)
        }
        StmtKind::While { body, .. } | StmtKind::For { body, .. } => block_has_value_return(body),
        _ => false,
    })
}

/// Resolve a type expression in the context of known record definitions.
fn tty(t: &ATypeExpr, records: &HashMap<String, Vec<(String, Ty)>>) -> Ty {
    match t {
        ATypeExpr::Named(n) => match n.as_str() {
            "Int" => Ty::Int,
            "Float" => Ty::Float,
            "Bool" => Ty::Bool,
            "Str" => Ty::Str,
            "Unit" => Ty::Unit,
            _ => {
                if records.contains_key(n) {
                    Ty::Record(n.clone())
                } else {
                    Ty::Unknown
                }
            }
        },
        ATypeExpr::Array(elem, n) => Ty::Array(Box::new(tty(elem, records)), *n),
    }
}

/// Validate a resolved annotation type, reporting zero-length arrays and
/// non-value element types (T030/T036). Returns the type unchanged otherwise.
fn ty_check(span: Span, t: &Ty) -> (Ty, Vec<Diagnostic>) {
    match t {
        Ty::Array(_elem, 0) => (
            Ty::Error,
            vec![Diagnostic::error(
                "T030",
                "zero-length arrays are not supported (length must be at least 1)",
            )
            .with_span(span)],
        ),
        Ty::Array(elem, n) => {
            let mut diags = Vec::new();
            let (e2, ediags) = ty_check(span, elem);
            diags.extend(ediags);
            if e2 == Ty::Unit || e2 == Ty::Never {
                diags.push(
                    Diagnostic::error(
                        "T036",
                        format!(
                            "array element type must be a value type, found {}",
                            e2.name()
                        ),
                    )
                    .with_span(span),
                );
                return (Ty::Error, diags);
            }
            if e2 == Ty::Str {
                diags.push(
                    Diagnostic::error(
                        "T039",
                        "managed `Str` values cannot be stored in arrays yet: the collector has no type descriptors; keep strings in locals, parameters and returns",
                    )
                    .with_span(span),
                );
                return (Ty::Array(Box::new(e2), *n), diags);
            }
            (Ty::Array(Box::new(e2), *n), diags)
        }
        _ => (t.clone(), Vec::new()),
    }
}

fn tty_opt(t: Option<&ATypeExpr>, records: &HashMap<String, Vec<(String, Ty)>>) -> Ty {
    match t {
        None => Ty::Unknown,
        Some(te) => tty(te, records),
    }
}

fn analyze_block(ctx: &mut Ctx, scope: &mut Scope, stmts: &[Stmt]) -> Vec<HirStmt> {
    let mut out = Vec::new();
    for s in stmts {
        match &s.kind {
            StmtKind::Let {
                name,
                mutable,
                ty,
                value,
            } => {
                let v = analyze_expr(ctx, scope, value);
                let vt = v.ty();
                let t = if let Some(te) = ty {
                    let tt0 = tty(te, &ctx.records);
                    let (tt, tdiags) = ty_check(s.span, &tt0);
                    ctx.diagnostics.extend(tdiags);
                    if tt != Ty::Unknown {
                        if !tt.compatible(&vt) {
                            ctx.diagnostics.push(
                                Diagnostic::error(
                                    "T009",
                                    format!(
                                        "type annotation mismatch: annotated {}, value is {}",
                                        tt.name(),
                                        vt.name()
                                    ),
                                )
                                .with_span(s.span),
                            );
                        }
                        tt
                    } else {
                        vt.clone()
                    }
                } else {
                    vt.clone()
                };
                scope.insert(name.clone(), t.clone(), *mutable);
                out.push(HirStmt::Let {
                    name: name.clone(),
                    mutable: *mutable,
                    ty: t,
                    value: v,
                });
            }
            StmtKind::Assign { name, value, op } => {
                let rhs = analyze_expr(ctx, scope, value);
                // `x += e` reads the current binding once: `x = x + e`.
                let v = if let Some(bop) = op {
                    let lhs_ty = scope.get(name).map(|i| i.ty).unwrap_or(Ty::Unknown);
                    let lhs = HirExpr::Var(name.clone(), lhs_ty);
                    let t = binop_type(ctx, s.span, *bop, &lhs.ty(), &rhs.ty());
                    HirExpr::Binary {
                        op: *bop,
                        lhs: Box::new(lhs),
                        rhs: Box::new(rhs),
                        ty: t,
                    }
                } else {
                    rhs
                };
                match scope.get(name) {
                    None => {
                        ctx.diagnostics.push(
                            Diagnostic::error(
                                "T011",
                                format!("assignment to undefined variable `{}`", name),
                            )
                            .with_span(s.span),
                        );
                    }
                    Some(info) => {
                        if !info.mutable {
                            ctx.diagnostics.push(
                                Diagnostic::error(
                                    "T010",
                                    format!("cannot assign to immutable variable `{}` (declare with `let mut`)", name),
                                )
                                .with_span(s.span),
                            );
                        }
                        if !info.ty.compatible(&v.ty()) {
                            ctx.diagnostics.push(
                                Diagnostic::error(
                                    "T012",
                                    format!(
                                        "assignment type mismatch: `{}` is {}, value is {}",
                                        name,
                                        info.ty.name(),
                                        v.ty().name()
                                    ),
                                )
                                .with_span(s.span),
                            );
                        }
                    }
                }
                out.push(HirStmt::Assign {
                    name: name.clone(),
                    value: v,
                });
            }
            StmtKind::PathAssign {
                root,
                path,
                value,
                op,
            } => {
                out.extend(analyze_path_assign(
                    ctx, scope, s.span, root, path, value, *op,
                ));
            }
            StmtKind::TypeDecl { .. } => {
                // Handled in the record passes; nothing to lower here.
            }
            StmtKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let c = analyze_expr(ctx, scope, cond);
                if !c.ty().compatible(&Ty::Bool) {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T001",
                            format!("if condition must be Bool, found {}", c.ty().name()),
                        )
                        .with_span(cond.span),
                    );
                }
                let mut sc = scope.clone();
                let th = analyze_block(ctx, &mut sc, then_branch);
                let el = else_branch.as_ref().map(|b| {
                    let mut sc2 = scope.clone();
                    analyze_block(ctx, &mut sc2, b)
                });
                out.push(HirStmt::If {
                    cond: c,
                    then_branch: th,
                    else_branch: el,
                });
            }
            StmtKind::While { cond, body } => {
                let c = analyze_expr(ctx, scope, cond);
                if !c.ty().compatible(&Ty::Bool) {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T002",
                            format!("while condition must be Bool, found {}", c.ty().name()),
                        )
                        .with_span(cond.span),
                    );
                }
                let mut sc = scope.clone();
                let b = analyze_block(ctx, &mut sc, body);
                out.push(HirStmt::While { cond: c, body: b });
            }
            StmtKind::For { var, iter, body } => match iter {
                ForIter::Range(start, end) => {
                    let st = analyze_expr(ctx, scope, start);
                    if !st.ty().compatible(&Ty::Int) {
                        ctx.diagnostics.push(
                            Diagnostic::error(
                                "T015",
                                format!("for range start must be Int, found {}", st.ty().name()),
                            )
                            .with_span(start.span),
                        );
                    }
                    let e = analyze_expr(ctx, scope, end);
                    if !e.ty().compatible(&Ty::Int) {
                        ctx.diagnostics.push(
                            Diagnostic::error(
                                "T016",
                                format!("for range end must be Int, found {}", e.ty().name()),
                            )
                            .with_span(end.span),
                        );
                    }
                    let mut sc = scope.clone();
                    sc.insert(var.clone(), Ty::Int, false);
                    let b = analyze_block(ctx, &mut sc, body);
                    out.push(HirStmt::For {
                        var: var.clone(),
                        start: st,
                        end: e,
                        body: b,
                    });
                }
                ForIter::Each(arr) => {
                    // `for v in xs:` is defined as: evaluate `xs` once, then
                    // bind `v` to each element in order (by value).
                    let a = analyze_expr(ctx, scope, arr);
                    let at = a.ty();
                    let elem_ty = match &at {
                        Ty::Array(e, _) => (**e).clone(),
                        Ty::Unknown | Ty::Error => Ty::Unknown,
                        other => {
                            ctx.diagnostics.push(
                                Diagnostic::error(
                                    "T038",
                                    format!("for-each requires an array, found {}", other.name()),
                                )
                                .with_span(arr.span),
                            );
                            Ty::Error
                        }
                    };
                    let mut sc = scope.clone();
                    sc.insert(var.clone(), elem_ty.clone(), false);
                    let b = analyze_block(ctx, &mut sc, body);
                    let n = if let Ty::Array(_, n) = &at { *n } else { 0 };
                    let arr_name = format!("$fea{}", ctx.synthetic);
                    let idx_name = format!("$fei{}", ctx.synthetic);
                    ctx.synthetic += 1;
                    let mut while_body: Vec<HirStmt> = Vec::new();
                    while_body.push(HirStmt::Let {
                        name: var.clone(),
                        mutable: false,
                        ty: elem_ty.clone(),
                        value: HirExpr::Index {
                            base: Box::new(HirExpr::Var(arr_name.clone(), at.clone())),
                            index: Box::new(HirExpr::Var(idx_name.clone(), Ty::Int)),
                            ty: elem_ty.clone(),
                        },
                    });
                    while_body.extend(b);
                    while_body.push(HirStmt::Assign {
                        name: idx_name.clone(),
                        value: HirExpr::Binary {
                            op: BinOp::Add,
                            lhs: Box::new(HirExpr::Var(idx_name.clone(), Ty::Int)),
                            rhs: Box::new(HirExpr::Int(1, Ty::Int)),
                            ty: Ty::Int,
                        },
                    });
                    out.push(HirStmt::Let {
                        name: arr_name.clone(),
                        mutable: false,
                        ty: at.clone(),
                        value: a,
                    });
                    out.push(HirStmt::Let {
                        name: idx_name.clone(),
                        mutable: true,
                        ty: Ty::Int,
                        value: HirExpr::Int(0, Ty::Int),
                    });
                    out.push(HirStmt::While {
                        cond: HirExpr::Binary {
                            op: BinOp::Lt,
                            lhs: Box::new(HirExpr::Var(idx_name.clone(), Ty::Int)),
                            rhs: Box::new(HirExpr::Int(n as i64, Ty::Int)),
                            ty: Ty::Bool,
                        },
                        body: while_body,
                    });
                }
            },
            StmtKind::Return { value } => {
                let v = value.as_ref().map(|e| analyze_expr(ctx, scope, e));
                if let Some(vv) = &v {
                    let vt = vv.ty();
                    if ctx.current_ret == Ty::Unknown {
                        ctx.current_ret = vt.clone();
                    } else if !vt.compatible(&ctx.current_ret) {
                        ctx.diagnostics.push(
                            Diagnostic::error(
                                "T003",
                                format!(
                                    "return type mismatch: expected {}, found {}",
                                    ctx.current_ret.name(),
                                    vt.name()
                                ),
                            )
                            .with_span(s.span),
                        );
                    }
                    // Record the inferred return type for callers.
                    if let Some(entry) = ctx.functions.get_mut(&ctx.current_fn.clone()) {
                        if entry.ret == Ty::Unknown && vt != Ty::Unknown && vt != Ty::Error {
                            entry.ret = vt;
                        }
                    }
                }
                out.push(HirStmt::Return { value: v });
            }
            StmtKind::Expr(e) => {
                out.push(HirStmt::Expr(analyze_expr(ctx, scope, e)));
            }
            StmtKind::Fn { .. } => { /* nested functions not supported yet */ }
        }
    }
    out
}

fn analyze_expr(ctx: &mut Ctx, scope: &mut Scope, e: &Expr) -> HirExpr {
    match &e.kind {
        ExprKind::Int(v) => HirExpr::Int(*v, Ty::Int),
        ExprKind::Float(v) => HirExpr::Float(*v, Ty::Float),
        ExprKind::Bool(v) => HirExpr::Bool(*v, Ty::Bool),
        ExprKind::Str(v) => HirExpr::Str(v.clone(), Ty::Str),
        ExprKind::Var(n) => {
            if let Some(info) = scope.get(n) {
                HirExpr::Var(n.clone(), info.ty)
            } else if ctx.functions.contains_key(n) {
                // Function reference is not first-class yet.
                HirExpr::Var(n.clone(), Ty::Unknown)
            } else {
                ctx.diagnostics.push(
                    Diagnostic::error("T004", format!("undefined variable `{}`", n))
                        .with_span(e.span),
                );
                HirExpr::Var(n.clone(), Ty::Error)
            }
        }
        ExprKind::Unary { op, expr } => {
            let inner = analyze_expr(ctx, scope, expr);
            let t = match op {
                UnOp::Neg => {
                    if !inner.ty().is_numeric() && !inner.ty().compatible(&Ty::Unknown) {
                        ctx.diagnostics.push(
                            Diagnostic::error(
                                "T017",
                                format!("cannot negate value of type {}", inner.ty().name()),
                            )
                            .with_span(e.span),
                        );
                    }
                    inner.ty()
                }
                UnOp::Not => {
                    if !inner.ty().compatible(&Ty::Bool) {
                        ctx.diagnostics.push(
                            Diagnostic::error(
                                "T018",
                                format!("`not` expects Bool, found {}", inner.ty().name()),
                            )
                            .with_span(e.span),
                        );
                    }
                    Ty::Bool
                }
            };
            HirExpr::Unary {
                op: *op,
                expr: Box::new(inner),
                ty: t,
            }
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let l = analyze_expr(ctx, scope, lhs);
            let r = analyze_expr(ctx, scope, rhs);
            let t = binop_type(ctx, e.span, *op, &l.ty(), &r.ty());
            HirExpr::Binary {
                op: *op,
                lhs: Box::new(l),
                rhs: Box::new(r),
                ty: t,
            }
        }
        ExprKind::Field { base, name } => {
            let b = analyze_expr(ctx, scope, base);
            let bt = b.ty();
            match &bt {
                Ty::Record(tname) => {
                    let fields = ctx.records.get(tname).cloned().unwrap_or_default();
                    match fields.iter().find(|(n, _)| n == name) {
                        Some((_, fty)) => HirExpr::Field {
                            base: Box::new(b),
                            name: name.clone(),
                            ty: fty.clone(),
                        },
                        None => {
                            ctx.diagnostics.push(
                                Diagnostic::error(
                                    "T019",
                                    format!("type `{}` has no field `{}`", tname, name),
                                )
                                .with_span(e.span),
                            );
                            HirExpr::Field {
                                base: Box::new(b),
                                name: name.clone(),
                                ty: Ty::Error,
                            }
                        }
                    }
                }
                Ty::Unknown | Ty::Error => HirExpr::Field {
                    base: Box::new(b),
                    name: name.clone(),
                    ty: Ty::Unknown,
                },
                other => {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T020",
                            format!("field access on non-record value of type {}", other.name()),
                        )
                        .with_span(e.span),
                    );
                    HirExpr::Field {
                        base: Box::new(b),
                        name: name.clone(),
                        ty: Ty::Error,
                    }
                }
            }
        }
        ExprKind::Index { base, index } => {
            let b = analyze_expr(ctx, scope, base);
            let i = analyze_expr(ctx, scope, index);
            if !i.ty().compatible(&Ty::Int) {
                ctx.diagnostics.push(
                    Diagnostic::error(
                        "T032",
                        format!("array index must be Int, found {}", i.ty().name()),
                    )
                    .with_span(index.span),
                );
            }
            let bt = b.ty();
            match &bt {
                Ty::Array(elem, n) => {
                    if let HirExpr::Int(v, _) = &i {
                        if *v < 0 || *v as usize >= *n {
                            ctx.diagnostics.push(
                                Diagnostic::error(
                                    "T033",
                                    format!(
                                        "constant index {} is out of bounds for array of length {}",
                                        v, n
                                    ),
                                )
                                .with_span(index.span),
                            );
                        }
                    }
                    HirExpr::Index {
                        base: Box::new(b),
                        index: Box::new(i),
                        ty: (**elem).clone(),
                    }
                }
                Ty::Unknown | Ty::Error => HirExpr::Index {
                    base: Box::new(b),
                    index: Box::new(i),
                    ty: Ty::Unknown,
                },
                other => {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T031",
                            format!("indexing requires an array, found {}", other.name()),
                        )
                        .with_span(e.span),
                    );
                    HirExpr::Index {
                        base: Box::new(b),
                        index: Box::new(i),
                        ty: Ty::Error,
                    }
                }
            }
        }
        ExprKind::Array(elems) => {
            let he: Vec<HirExpr> = elems
                .iter()
                .map(|el| analyze_expr(ctx, scope, el))
                .collect();
            if he.is_empty() {
                // Zero-length arrays are not supported in the bootstrap; there
                // is also no element type to infer from, so this is an error.
                ctx.diagnostics.push(
                    Diagnostic::error(
                        "T029",
                        "empty array literal: zero-length arrays are not supported",
                    )
                    .with_span(e.span),
                );
                HirExpr::Array(he, Ty::Array(Box::new(Ty::Error), 0))
            } else {
                let mut elem_ty = he[0].ty();
                for (k, item) in he.iter().enumerate().skip(1) {
                    if !elem_ty.compatible(&item.ty()) {
                        ctx.diagnostics.push(
                            Diagnostic::error(
                                "T035",
                                format!(
                                    "array element type mismatch: element {} is {}, first element is {}",
                                    k,
                                    item.ty().name(),
                                    elem_ty.name()
                                ),
                            )
                            .with_span(elems[k].span),
                        );
                    }
                    if elem_ty == Ty::Unknown {
                        elem_ty = item.ty();
                    }
                }
                if elem_ty == Ty::Str {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T039",
                            "managed `Str` values cannot be stored in arrays yet: the collector has no type descriptors; keep strings in locals, parameters and returns",
                        )
                        .with_span(e.span),
                    );
                }
                let n = he.len();
                HirExpr::Array(he, Ty::Array(Box::new(elem_ty), n))
            }
        }
        ExprKind::Call { callee, args } => {
            let mut ha = Vec::new();
            for a in args {
                ha.push(analyze_expr(ctx, scope, a));
            }
            // Record construction: `Vec2(1.0, 2.0)`.
            if let Some(fields) = ctx.records.get(callee).cloned() {
                if fields.len() != ha.len() {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T005",
                            format!(
                                "record `{}` expects {} field(s), got {}",
                                callee,
                                fields.len(),
                                ha.len()
                            ),
                        )
                        .with_span(e.span),
                    );
                } else {
                    for (i, (arg, (fname, fty))) in ha.iter().zip(fields.iter()).enumerate() {
                        if !fty.compatible(&arg.ty()) {
                            ctx.diagnostics.push(
                                Diagnostic::error(
                                    "T007",
                                    format!(
                                        "field `{}` of `{}` expects {}, found {}",
                                        fname,
                                        callee,
                                        fty.name(),
                                        arg.ty().name()
                                    ),
                                )
                                .with_span(args[i].span),
                            );
                        }
                    }
                }
                return HirExpr::Record {
                    name: callee.clone(),
                    args: ha,
                    ty: Ty::Record(callee.clone()),
                };
            }
            if callee == "panic" {
                if ha.len() != 1 || !ha[0].ty().compatible(&Ty::Str) {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T026",
                            format!("panic expects exactly one Str argument, got {}", ha.len()),
                        )
                        .with_span(e.span),
                    );
                }
                return HirExpr::Call {
                    callee: callee.clone(),
                    args: ha,
                    ty: Ty::Never,
                };
            }
            if callee == "concat" {
                if ha.len() != 2 {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T040",
                            format!("concat expects exactly 2 Str arguments, got {}", ha.len()),
                        )
                        .with_span(e.span),
                    );
                }
                for a in &ha {
                    if !a.ty().compatible(&Ty::Str) {
                        ctx.diagnostics.push(
                            Diagnostic::error(
                                "T040",
                                format!("concat expects Str arguments, found {}", a.ty().name()),
                            )
                            .with_span(e.span),
                        );
                    }
                }
                return HirExpr::Call {
                    callee: callee.clone(),
                    args: ha,
                    ty: Ty::Str,
                };
            }
            if callee == "collect" {
                if !ha.is_empty() {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T041",
                            format!("collect expects no arguments, got {}", ha.len()),
                        )
                        .with_span(e.span),
                    );
                }
                return HirExpr::Call {
                    callee: callee.clone(),
                    args: ha,
                    ty: Ty::Unit,
                };
            }
            if callee == "heap_used" {
                if !ha.is_empty() {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T042",
                            format!("heap_used expects no arguments, got {}", ha.len()),
                        )
                        .with_span(e.span),
                    );
                }
                return HirExpr::Call {
                    callee: callee.clone(),
                    args: ha,
                    ty: Ty::Int,
                };
            }
            if callee == "len" {
                if ha.len() != 1 {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T034",
                            format!("len expects exactly 1 argument, got {}", ha.len()),
                        )
                        .with_span(e.span),
                    );
                    return HirExpr::Int(0, Ty::Int);
                }
                match ha[0].ty() {
                    Ty::Array(_, n) => return HirExpr::Int(n as i64, Ty::Int),
                    Ty::Str => {
                        // Runtime length of a (possibly heap-allocated)
                        // string: lowered to a `strlen` runtime call.
                        return HirExpr::Call {
                            callee: "strlen".to_string(),
                            args: ha,
                            ty: Ty::Int,
                        };
                    }
                    Ty::Unknown | Ty::Error => return HirExpr::Int(0, Ty::Int),
                    other => {
                        ctx.diagnostics.push(
                            Diagnostic::error(
                                "T034",
                                format!("len expects an array, found {}", other.name()),
                            )
                            .with_span(e.span),
                        );
                        return HirExpr::Int(0, Ty::Int);
                    }
                }
            }
            if callee == "print" {
                if ha.len() != 1 {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T013",
                            format!("print expects exactly 1 argument, got {}", ha.len()),
                        )
                        .with_span(e.span),
                    );
                }
                if let Some(a0) = ha.first() {
                    match a0.ty() {
                        Ty::Record(n) => {
                            ctx.diagnostics.push(
                                Diagnostic::error(
                                    "T028",
                                    format!("print does not support record values (found `{}`)", n),
                                )
                                .with_span(e.span),
                            );
                        }
                        Ty::Array(_, _) => {
                            ctx.diagnostics.push(
                                Diagnostic::error(
                                    "T028",
                                    format!(
                                        "print does not support array values (found {})",
                                        a0.ty().name()
                                    ),
                                )
                                .with_span(e.span),
                            );
                        }
                        _ => {}
                    }
                }
                return HirExpr::Call {
                    callee: callee.clone(),
                    args: ha,
                    ty: Ty::Unit,
                };
            }
            // Refine unknown parameter types from concrete argument types seen
            // at this call site (monotone; the analysis loop re-runs until the
            // signature table stops changing).
            if let Some(sig) = ctx.functions.get_mut(callee) {
                for (i, arg) in ha.iter().enumerate() {
                    if let Some((_, pty)) = sig.params.get_mut(i) {
                        if *pty == Ty::Unknown {
                            let at = arg.ty();
                            if at != Ty::Unknown && at != Ty::Error {
                                *pty = at;
                            }
                        }
                    }
                }
            }
            if let Some(sig) = ctx.functions.get(callee).cloned() {
                if sig.params.len() != ha.len() {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T005",
                            format!(
                                "function `{}` expects {} argument(s), got {}",
                                callee,
                                sig.params.len(),
                                ha.len()
                            ),
                        )
                        .with_span(e.span),
                    );
                } else {
                    for (i, (arg, (pname, pty))) in ha.iter().zip(sig.params.iter()).enumerate() {
                        if !pty.compatible(&arg.ty()) {
                            ctx.diagnostics.push(
                                Diagnostic::error(
                                    "T007",
                                    format!(
                                        "argument {} of `{}` expects {}, found {}",
                                        i + 1,
                                        callee,
                                        pty.name(),
                                        arg.ty().name()
                                    ),
                                )
                                .with_span(args[i].span),
                            );
                        }
                        let _ = pname;
                        // `mut` parameters borrow the caller's binding: the
                        // argument must be a mutable variable.
                        if sig.mut_params.get(i).copied().unwrap_or(false) {
                            let ok = match &args[i].kind {
                                ExprKind::Var(n) => {
                                    scope.get(n).map(|v| v.mutable).unwrap_or(false)
                                }
                                _ => false,
                            };
                            if !ok {
                                ctx.diagnostics.push(
                                    Diagnostic::error(
                                        "T037",
                                        format!(
                                            "argument {} of `{}` is a `mut` (by-reference) parameter: the argument must be a mutable variable (declare it with `let mut`)",
                                            i + 1,
                                            callee
                                        ),
                                    )
                                    .with_span(args[i].span),
                                );
                            }
                        }
                    }
                }
                HirExpr::Call {
                    callee: callee.clone(),
                    args: ha,
                    ty: sig.ret.clone(),
                }
            } else {
                ctx.diagnostics.push(
                    Diagnostic::error("T006", format!("undefined function `{}`", callee))
                        .with_span(e.span),
                );
                HirExpr::Call {
                    callee: callee.clone(),
                    args: ha,
                    ty: Ty::Error,
                }
            }
        }
    }
}

/// One resolved assignment-target step.
#[derive(Clone)]
enum Step {
    /// Record field: name and field type.
    Field(String, Ty),
    /// Array element: index expression and element type.
    Index(HirExpr, Ty),
}

impl Step {
    /// The type produced by applying this step (indexing yields the element
    /// type; field access yields the field type).
    fn value_ty(&self) -> Ty {
        match self {
            Step::Field(_, t) => t.clone(),
            Step::Index(_, t) => t.clone(),
        }
    }
}

/// Desugar `a.b[i].c = v` into nested functional updates rooted at an
/// assignment to `a`: `a = set(... set(set(a.b, "c", v)) ...)`.
///
/// A compound assignment (`a.b[i].c += v`) is lowered as a read-modify-write;
/// each index expression in the target path is bound to a temporary first, so
/// it is evaluated exactly once.
fn analyze_path_assign(
    ctx: &mut Ctx,
    scope: &mut Scope,
    span: Span,
    root: &str,
    path: &[nexum_parser::PathSeg],
    value: &Expr,
    op: Option<BinOp>,
) -> Vec<HirStmt> {
    let Some(info) = scope.get(root) else {
        ctx.diagnostics.push(
            Diagnostic::error(
                "T011",
                format!("assignment to undefined variable `{}`", root),
            )
            .with_span(span),
        );
        return Vec::new();
    };
    if !info.mutable {
        ctx.diagnostics.push(
            Diagnostic::error(
                "T010",
                format!(
                    "cannot assign to field or element of immutable variable `{}` (declare with `let mut`)",
                    root
                ),
            )
            .with_span(span),
        );
    }
    // Walk the path, resolving each step against record definitions and arrays.
    let mut cur_ty = info.ty.clone();
    let mut steps: Vec<Step> = Vec::new();
    for seg in path {
        match seg {
            nexum_parser::PathSeg::Field(fname) => {
                let Ty::Record(tname) = cur_ty.clone() else {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T020",
                            format!(
                                "field assignment on non-record value of type {}",
                                cur_ty.name()
                            ),
                        )
                        .with_span(span),
                    );
                    return Vec::new();
                };
                let Some(fields) = ctx.records.get(&tname).cloned() else {
                    return Vec::new();
                };
                let Some((_, fty)) = fields.iter().find(|(n, _)| n == fname) else {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T019",
                            format!("type `{}` has no field `{}`", tname, fname),
                        )
                        .with_span(span),
                    );
                    return Vec::new();
                };
                cur_ty = fty.clone();
                steps.push(Step::Field(fname.clone(), fty.clone()));
            }
            nexum_parser::PathSeg::Index(iexpr) => {
                let Ty::Array(elem, n) = cur_ty.clone() else {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T031",
                            format!(
                                "index assignment on non-array value of type {}",
                                cur_ty.name()
                            ),
                        )
                        .with_span(span),
                    );
                    return Vec::new();
                };
                let i = analyze_expr(ctx, scope, iexpr);
                if !i.ty().compatible(&Ty::Int) {
                    ctx.diagnostics.push(
                        Diagnostic::error(
                            "T032",
                            format!("array index must be Int, found {}", i.ty().name()),
                        )
                        .with_span(iexpr.span),
                    );
                }
                if let HirExpr::Int(v, _) = &i {
                    if *v < 0 || *v as usize >= n {
                        ctx.diagnostics.push(
                            Diagnostic::error(
                                "T033",
                                format!(
                                    "constant index {} is out of bounds for array of length {}",
                                    v, n
                                ),
                            )
                            .with_span(iexpr.span),
                        );
                    }
                }
                cur_ty = (*elem).clone();
                steps.push(Step::Index(i, (*elem).clone()));
            }
        }
    }
    // For compound assignment, bind each index expression to a temporary so
    // the read and the write below use the same index values: `xs[f()] += e`
    // evaluates `f()` exactly once.
    let mut prefix: Vec<HirStmt> = Vec::new();
    if op.is_some() {
        for st in steps.iter_mut() {
            if let Step::Index(ix, _) = st {
                let tmp = format!("$t{}", ctx.synthetic);
                ctx.synthetic += 1;
                prefix.push(HirStmt::Let {
                    name: tmp.clone(),
                    mutable: false,
                    ty: Ty::Int,
                    value: ix.clone(),
                });
                *ix = HirExpr::Var(tmp, Ty::Int);
            }
        }
    }
    // The assigned value must match the innermost element/field type.
    let rhs = analyze_expr(ctx, scope, value);
    let v = if let Some(bop) = op {
        let read = path_project(root, &steps, &info.ty);
        let t = binop_type(ctx, span, bop, &read.ty(), &rhs.ty());
        HirExpr::Binary {
            op: bop,
            lhs: Box::new(read),
            rhs: Box::new(rhs),
            ty: t,
        }
    } else {
        rhs
    };
    if !cur_ty.compatible(&v.ty()) {
        let what = match steps.last() {
            Some(Step::Field(n, _)) => format!("field `{}`", n),
            _ => "array element".to_string(),
        };
        ctx.diagnostics.push(
            Diagnostic::error(
                "T025",
                format!(
                    "assignment type mismatch: {} is {}, value is {}",
                    what,
                    cur_ty.name(),
                    v.ty().name()
                ),
            )
            .with_span(span),
        );
    }
    // Build nested updates from the inside out.
    let mut expr = v;
    for i in (0..steps.len()).rev() {
        let base = path_project(root, &steps[..i], &info.ty);
        let result_ty = if i == 0 {
            info.ty.clone()
        } else {
            steps[i - 1].value_ty()
        };
        expr = match &steps[i] {
            Step::Field(fname, _) => HirExpr::FieldSet {
                base: Box::new(base),
                name: fname.clone(),
                value: Box::new(expr),
                ty: result_ty,
            },
            Step::Index(ix, _) => HirExpr::IndexSet {
                base: Box::new(base),
                index: Box::new(ix.clone()),
                value: Box::new(expr),
                ty: result_ty,
            },
        };
    }
    prefix.push(HirStmt::Assign {
        name: root.to_string(),
        value: expr,
    });
    prefix
}

/// Build a projection chain `root.f0[i].f1...` for the given resolved steps.
fn path_project(root: &str, steps: &[Step], root_ty: &Ty) -> HirExpr {
    let mut e = HirExpr::Var(root.to_string(), root_ty.clone());
    for s in steps {
        e = match s {
            Step::Field(fname, fty) => HirExpr::Field {
                base: Box::new(e),
                name: fname.clone(),
                ty: fty.clone(),
            },
            Step::Index(ix, ety) => HirExpr::Index {
                base: Box::new(e),
                index: Box::new(ix.clone()),
                ty: ety.clone(),
            },
        };
    }
    e
}

fn binop_type(ctx: &mut Ctx, span: Span, op: BinOp, l: &Ty, r: &Ty) -> Ty {
    use BinOp::*;
    let err = |ctx: &mut Ctx, msg: String| {
        ctx.diagnostics
            .push(Diagnostic::error("T008", msg).with_span(span));
    };
    match op {
        Add | Sub | Mul | Div | Mod => {
            if l.is_numeric() && r.is_numeric() {
                if !l.compatible(r) {
                    err(
                        ctx,
                        format!(
                            "operands must have the same numeric type, found {} and {}",
                            l.name(),
                            r.name()
                        ),
                    );
                }
                if *l == Ty::Float || *r == Ty::Float {
                    Ty::Float
                } else {
                    Ty::Int
                }
            } else {
                if !l.compatible(&Ty::Unknown) || !r.compatible(&Ty::Unknown) {
                    // fall through to compatibility check below
                }
                if !l.is_numeric() && *l != Ty::Unknown && *l != Ty::Error {
                    err(
                        ctx,
                        format!("operand of arithmetic must be numeric, found {}", l.name()),
                    );
                }
                if !r.is_numeric() && *r != Ty::Unknown && *r != Ty::Error {
                    err(
                        ctx,
                        format!("operand of arithmetic must be numeric, found {}", r.name()),
                    );
                }
                if *l == Ty::Float || *r == Ty::Float {
                    Ty::Float
                } else if *l == Ty::Unknown || *r == Ty::Unknown {
                    // At least one operand's type is not known yet; defer the
                    // commitment until the fixpoint loop re-analyzes with a
                    // refined signature table.
                    Ty::Unknown
                } else {
                    Ty::Int
                }
            }
        }
        Eq | Ne => {
            if !l.compatible(r) {
                err(
                    ctx,
                    format!("cannot compare {} with {}", l.name(), r.name()),
                );
            }
            // `Str` equality is byte-wise content comparison (allowed; the
            // contents may live on the managed heap). Ordering is still
            // rejected below.
            Ty::Bool
        }
        Lt | Gt | Le | Ge => {
            if !l.compatible(r) {
                err(
                    ctx,
                    format!("cannot compare {} with {}", l.name(), r.name()),
                );
            }
            if !l.is_numeric() && *l != Ty::Unknown && *l != Ty::Error {
                err(
                    ctx,
                    format!("ordering comparisons require numerics, found {}", l.name()),
                );
            }
            Ty::Bool
        }
        And | Or => {
            if !l.compatible(&Ty::Bool) {
                err(ctx, format!("operand must be Bool, found {}", l.name()));
            }
            if !r.compatible(&Ty::Bool) {
                err(ctx, format!("operand must be Bool, found {}", r.name()));
            }
            Ty::Bool
        }
    }
}
