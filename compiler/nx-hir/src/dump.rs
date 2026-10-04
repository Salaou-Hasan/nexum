//! `nx dump-hir`: the verified HIR as line-based S-expressions.
//!
//! Stability contract (hir.md §9). What "stable enough to diff in tests"
//! means here, rule by rule:
//!
//! - every table iterates in ID order, which is sorted-name order, so no
//!   `HashMap` order can leak into the output;
//! - module names, never file paths; no timestamps; no counters beyond
//!   slot and ID numbering;
//! - spans print as `line:col`;
//! - original spellings print from the inert `diag` sidecar for
//!   readability, and the verifier's strip test proves nothing depends
//!   on them.
//!
//! Shape: statements occupy one line each and end with `@line:col`, with
//! nested bodies indented below them. Expressions print inline, each as
//! `(shape : Type)`. Nothing here re-derives a decision: every rule,
//! index and field reference prints as the value lowering recorded.

use super::model::*;

/// The whole program as text, ending in a newline.
pub fn dump(p: &HProgram) -> String {
    let mut out = String::new();
    for (i, m) in p.modules.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        module(&mut out, p, m, ModuleId(i as u32));
    }
    out
}

/// The original spelling if one is recorded, else the ID -- so output
/// stays complete after a strip.
fn name(d: &DiagInfo, fallback: String) -> String {
    d.name.clone().unwrap_or(fallback)
}

fn module(out: &mut String, p: &HProgram, m: &HModule, id: ModuleId) {
    out.push_str(&format!(
        "(module {}{}\n",
        name(&m.diag, format!("m{}", id.0)),
        if id == p.entry { " (entry)" } else { "" }
    ));
    for gid in &m.globals {
        let Some(g) = p.globals.get(gid.0 as usize) else { continue };
        out.push_str(&format!("  (global {})\n", name(&g.diag, format!("g{}", gid.0))));
    }
    for tid in &m.types {
        let Some(t) = p.types.get(tid.0 as usize) else { continue };
        out.push_str(&format!("  (type {} (layout", name(&t.diag, format!("t{}", tid.0))));
        for (i, f) in t.fields.iter().enumerate() {
            out.push_str(&format!(" (field {i}: {f})"));
        }
        out.push_str("))\n");
    }
    // Method bodies are ordinary function entries in the same list: the
    // method table points at them, and the receiver is slot 0 of the
    // params, so printing them as functions loses nothing.
    for fid in &m.funcs {
        let Some(f) = p.funcs.get(fid.0 as usize) else { continue };
        let label = name(&f.diag, format!("f{}", fid.0));
        out.push_str(&format!("  (fn {label}"));
        for (slot, ty) in &f.params {
            out.push_str(&format!(" (param ${} : {ty})", slot.0));
        }
        out.push_str(&format!(" -> {})\n", f.ret));
        block(out, p, &f.body, 2);
        out.push_str("  )\n");
    }
    out.push_str(")\n");
}

/// Statements, one per line, with nested bodies indented below.
fn block(out: &mut String, p: &HProgram, b: &[HStmt], indent: usize) {
    for s in b {
        out.push_str(&format!(
            "{}{} @{}:{}\n",
            "  ".repeat(indent),
            stmt(p, s),
            s.span.line,
            s.span.col
        ));
        for body in bodies(s) {
            block(out, p, body, indent + 1);
        }
    }
}

/// The nested statement bodies of one statement, in source order. These
/// are slices of the same tree, so walking them costs nothing.
fn bodies(s: &HStmt) -> Vec<&[HStmt]> {
    let mut out: Vec<&[HStmt]> = Vec::new();
    match &s.kind {
        HStmtKind::If { then_body, elifs, else_body, .. } => {
            out.push(then_body);
            for (_, b) in elifs {
                out.push(b);
            }
            if let Some(b) = else_body {
                out.push(b);
            }
        }
        HStmtKind::While { body, .. }
        | HStmtKind::ForRange { body, .. }
        | HStmtKind::ForEach { body, .. } => out.push(body),
        _ => {}
    }
    out
}

fn stmt(_p: &HProgram, s: &HStmt) -> String {
    let list = |v: &[HExpr]| v.iter().map(expr).collect::<Vec<_>>().join(" ");
    match &s.kind {
        HStmtKind::Assign { targets, values, rules } => {
            let t = targets.iter().map(|t| target(t)).collect::<Vec<_>>().join(" ");
            let r = rules.iter().map(|r| format!("{r:?}")).collect::<Vec<_>>().join(" ");
            format!("(assign {t} <- {} [{r}])", list(values))
        }
        HStmtKind::AssignOp { target: t, op, rule, value } => {
            format!("(assign-op {} {op:?} {rule:?} {})", target(t), expr(value))
        }
        HStmtKind::Print { values } => format!("(print {})", list(values)),
        HStmtKind::EnsureInit { module } => format!("(ensure-init m{})", module.0),
        // Nested bodies print below this line, so only their sizes are
        // summarized here.
        HStmtKind::If { cond, then_body, elifs, else_body } => {
            let mut s = format!("(if {} (then ({} stmts))", expr(cond), then_body.len());
            for (c, b) in elifs {
                s.push_str(&format!(" (elif {} ({} stmts))", expr(c), b.len()));
            }
            match else_body {
                Some(b) => s.push_str(&format!(" (else ({} stmts))", b.len())),
                None => s.push_str(" (else none)"),
            }
            s.push(')');
            s
        }
        HStmtKind::While { cond, body } => {
            format!("(while {} ({} stmts))", expr(cond), body.len())
        }
        HStmtKind::ForRange { var, start, end, body } => format!(
            "(for-range ${} {}..{} ({} stmts))",
            var.0,
            expr(start),
            expr(end),
            body.len()
        ),
        HStmtKind::ForEach { var, iter, rule, body } => format!(
            "(for-each ${} {} {rule:?} ({} stmts))",
            var.0,
            expr(iter),
            body.len()
        ),
        HStmtKind::Return { values } => format!("(return {})", list(values)),
        HStmtKind::Break => "(break)".to_string(),
        HStmtKind::Continue => "(continue)".to_string(),
        HStmtKind::Del { targets } => format!(
            "(del {})",
            targets
                .iter()
                .map(|t| format!("{:?} {}", t.rule, target(&t.target)))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        HStmtKind::Assert { cond, message } => match message {
            Some(m) => format!("(assert {} {})", expr(cond), expr(m)),
            None => format!("(assert {})", expr(cond)),
        },
        HStmtKind::Expr(e) => format!("(do {})", expr(e)),
    }
}

fn target(t: &HTarget) -> String {
    match t {
        HTarget::Slot(s) => format!("${}", s.0),
        HTarget::Global(g) => format!("@g{}", g.0),
        HTarget::Index { base, index, rule } => {
            format!("(index {} {} {rule:?})", expr(base), expr(index))
        }
        HTarget::Field { base, field } => {
            format!("(field {} {})", expr(base), field_ref(*field))
        }
    }
}

fn field_ref(f: FieldRef) -> String {
    match f {
        FieldRef::Static(i) => format!("#{}", i.0),
        FieldRef::Dynamic(s) => format!("$str{}", s.0),
    }
}

/// One expression: its shape, then the type it carries. The type is
/// printed on every node because "every node typed" is the model's
/// central claim, and a dump that hid it could not show a violation.
fn expr(e: &HExpr) -> String {
    format!("({} : {})", shape(e), e.ty)
}

fn args(list: &[HExpr]) -> String {
    if list.is_empty() {
        String::new()
    } else {
        format!(" {}", list.iter().map(expr).collect::<Vec<_>>().join(" "))
    }
}

fn shape(e: &HExpr) -> String {
    match &e.kind {
        HExprKind::Int(v) => format!("int {v}"),
        HExprKind::Float(v) => format!("float {v}"),
        HExprKind::Bool(v) => format!("bool {v}"),
        HExprKind::Str(v) => format!("str {v:?}"),
        HExprKind::None => "none".to_string(),
        HExprKind::List(items) => {
            format!("list [{}]", items.iter().map(expr).collect::<Vec<_>>().join(" "))
        }
        HExprKind::Range { start, end, rule } => {
            format!("range {}..{} {rule:?}", expr(start), expr(end))
        }
        HExprKind::Dict(pairs) => format!(
            "dict {{{}}}",
            pairs
                .iter()
                .map(|(k, v)| format!("{} {}", expr(k), expr(v)))
                .collect::<Vec<_>>()
                .join(" ")
        ),
        HExprKind::Place(Place::Slot(s)) => format!("slot ${}", s.0),
        HExprKind::Place(Place::Global(g)) => format!("global @g{}", g.0),
        HExprKind::Field { base, field } => format!(".field {} {}", expr(base), field_ref(*field)),
        HExprKind::Index { base, index, rule } => {
            format!("index {} {} {rule:?}", expr(base), expr(index))
        }
        HExprKind::Slice { base, from, to, step, rule } => {
            let bound = |o: &Option<Box<HExpr>>| match o {
                Some(e) => expr(e),
                None => String::new(),
            };
            format!(
                "slice {} [{}:{}:{}] {rule:?}",
                expr(base),
                bound(from),
                bound(to),
                bound(step)
            )
        }
        HExprKind::Unary { op, rule, operand } => format!("{op:?} {} {rule:?}", expr(operand)),
        HExprKind::Binary { left, op, rule, right } => {
            format!("{} {op:?} {} {rule:?}", expr(left), expr(right))
        }
        HExprKind::Equal { left, op, rule, right } => {
            format!("{} {op:?} {} {rule:?}", expr(left), expr(right))
        }
        HExprKind::Compare { left, op, rule, right } => {
            format!("{} {op:?} {} {rule:?}", expr(left), expr(right))
        }
        HExprKind::Contains { needle, hay, rule, negated } => format!(
            "{} {}in {} {rule:?}",
            expr(needle),
            if *negated { "not " } else { "" },
            expr(hay)
        ),
        HExprKind::Select { cond, then_value, else_value } => format!(
            "{} if {} else {}",
            expr(then_value),
            expr(cond),
            expr(else_value)
        ),
        HExprKind::Logic { op, left, right } => {
            format!("{} {op:?} {}", expr(left), expr(right))
        }
        HExprKind::Compr { element, var, iter, rule, cond } => format!(
            "compr [{} for ${} in {} {rule:?}{}]",
            expr(element),
            var.0,
            expr(iter),
            match cond {
                Some(c) => format!(" if {}", expr(c)),
                None => String::new(),
            }
        ),
        HExprKind::CallFn { func, args: a } => format!("call f{}{}", func.0, args(a)),
        HExprKind::CallMethod { method, receiver, args: a, writeback } => {
            let recv = match receiver {
                Some(r) => format!("on {}", expr(r)),
                None => "on <type>".to_string(),
            };
            let wb = match writeback {
                Some(w) => format!(" write-back {}", target(w)),
                None => String::new(),
            };
            format!("method m{} {recv}({}){wb}", method.0, a.iter().map(expr).collect::<Vec<_>>().join(" "))
        }
        HExprKind::Construct { type_id, args: a } => {
            format!("construct t{}{}", type_id.0, args(a))
        }
        HExprKind::Builtin { op, args: a } => {
            format!("builtin {op:?}({})", a.iter().map(expr).collect::<Vec<_>>().join(" "))
        }
    }
}