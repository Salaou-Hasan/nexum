//! Normalized form of a parsed module.
//!
//! The self-hosted compiler (Phase 10) parses the same source with its own
//! parser, so the two need a way to compare what they built. The normalized
//! form is a line-oriented tree: one node per line, two spaces of indent per
//! level, so a difference between the two parsers is a visible diff rather
//! than a deep structural mismatch.
//!
//! The form is deliberately mechanical - it prints what the parser built, not
//! what the program means - so that it stays a parser-level contract.

use crate::{BinOp, Expr, ExprKind, ForIter, Module, PathSeg, Stmt, StmtKind, TypeExpr, UnOp};

fn binop_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "add",
        BinOp::Sub => "sub",
        BinOp::Mul => "mul",
        BinOp::Div => "div",
        BinOp::Mod => "mod",
        BinOp::Eq => "eq",
        BinOp::Ne => "ne",
        BinOp::Lt => "lt",
        BinOp::Gt => "gt",
        BinOp::Le => "le",
        BinOp::Ge => "ge",
        BinOp::And => "and",
        BinOp::Or => "or",
    }
}

fn unop_name(op: UnOp) -> &'static str {
    match op {
        UnOp::Neg => "neg",
        UnOp::Not => "not",
    }
}

/// Escape a string so that it occupies exactly one line.
fn escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
    out
}

fn type_expr(out: &mut String, indent: usize, ty: &TypeExpr) {
    match ty {
        TypeExpr::Named(n) => {
            out.push_str(&format!("{}named {}\n", " ".repeat(indent), n));
        }
        TypeExpr::Array(elem, n) => {
            out.push_str(&format!("{}array {}\n", " ".repeat(indent), n));
            type_expr(out, indent + 2, elem);
        }
    }
}

fn expr(out: &mut String, indent: usize, e: &Expr) {
    let pad = " ".repeat(indent);
    match &e.kind {
        ExprKind::Int(v) => out.push_str(&format!("{}int {}\n", pad, v)),
        ExprKind::Float(v) => out.push_str(&format!("{}float {:?}\n", pad, v)),
        ExprKind::Bool(b) => out.push_str(&format!("{}bool {}\n", pad, b)),
        ExprKind::Str(s) => out.push_str(&format!("{}str {}\n", pad, escape(s))),
        ExprKind::Var(n) => out.push_str(&format!("{}var {}\n", pad, n)),
        ExprKind::Call { callee, args } => {
            out.push_str(&format!("{}call {}\n", pad, callee));
            for a in args {
                expr(out, indent + 2, a);
            }
        }
        ExprKind::Binary { op, lhs, rhs } => {
            out.push_str(&format!("{}binop {}\n", pad, binop_name(*op)));
            expr(out, indent + 2, lhs);
            expr(out, indent + 2, rhs);
        }
        ExprKind::Unary { op, expr: inner } => {
            out.push_str(&format!("{}unop {}\n", pad, unop_name(*op)));
            expr(out, indent + 2, inner);
        }
        ExprKind::Field { base, name } => {
            out.push_str(&format!("{}field {}\n", pad, name));
            expr(out, indent + 2, base);
        }
        ExprKind::Index { base, index } => {
            out.push_str(&format!("{}index\n", pad));
            expr(out, indent + 2, base);
            expr(out, indent + 2, index);
        }
        ExprKind::Array(items) => {
            out.push_str(&format!("{}array\n", pad));
            for it in items {
                expr(out, indent + 2, it);
            }
        }
    }
}

fn stmt(out: &mut String, indent: usize, s: &Stmt) {
    let pad = " ".repeat(indent);
    match &s.kind {
        StmtKind::Fn {
            name,
            params,
            ret,
            body,
        } => {
            out.push_str(&format!("{}fn {}\n", pad, name));
            for p in params {
                out.push_str(&format!("{}param {}\n", " ".repeat(indent + 2), p.name));
                if let Some(t) = &p.ty {
                    out.push_str(&format!("{}param-type\n", " ".repeat(indent + 2)));
                    type_expr(out, indent + 4, t);
                }
            }
            if let Some(t) = ret {
                out.push_str(&format!("{}return-type\n", " ".repeat(indent + 2)));
                type_expr(out, indent + 4, t);
            }
            for st in body {
                stmt(out, indent + 2, st);
            }
        }
        StmtKind::Let {
            name,
            mutable,
            ty,
            value,
        } => {
            out.push_str(&format!(
                "{}let{}\n",
                pad,
                if *mutable { "-mut" } else { "" }
            ));
            out.push_str(&format!("{}name {}\n", " ".repeat(indent + 2), name));
            if let Some(t) = ty {
                out.push_str(&format!("{}let-type\n", " ".repeat(indent + 2)));
                type_expr(out, indent + 4, t);
            }
            expr(out, indent + 2, value);
        }
        StmtKind::Assign { name, value, op } => {
            match op {
                Some(op) => out.push_str(&format!("{}assign-op {}\n", pad, binop_name(*op))),
                None => out.push_str(&format!("{}assign\n", pad)),
            }
            out.push_str(&format!("{}name {}\n", " ".repeat(indent + 2), name));
            expr(out, indent + 2, value);
        }
        StmtKind::PathAssign {
            root,
            path,
            value,
            op,
        } => {
            match op {
                Some(op) => out.push_str(&format!("{}assign-path-op {}\n", pad, binop_name(*op))),
                None => out.push_str(&format!("{}assign-path\n", pad)),
            }
            out.push_str(&format!("{}root {}\n", " ".repeat(indent + 2), root));
            for seg in path {
                match seg {
                    PathSeg::Field(f) => {
                        out.push_str(&format!("{}path-field {}\n", " ".repeat(indent + 2), f))
                    }
                    PathSeg::Index(ix) => {
                        out.push_str(&format!("{}path-index\n", " ".repeat(indent + 2)));
                        expr(out, indent + 4, ix);
                    }
                }
            }
            expr(out, indent + 2, value);
        }
        StmtKind::TypeDecl { name, fields } => {
            out.push_str(&format!("{}type {}\n", pad, name));
            for (fname, fty) in fields {
                out.push_str(&format!("{}field {}\n", " ".repeat(indent + 2), fname));
                type_expr(out, indent + 4, fty);
            }
        }
        StmtKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            out.push_str(&format!("{}if\n", pad));
            expr(out, indent + 2, cond);
            out.push_str(&format!("{}then\n", " ".repeat(indent + 2)));
            for st in then_branch {
                stmt(out, indent + 4, st);
            }
            if let Some(eb) = else_branch {
                out.push_str(&format!("{}else\n", " ".repeat(indent + 2)));
                for st in eb {
                    stmt(out, indent + 4, st);
                }
            }
        }
        StmtKind::While { cond, body } => {
            out.push_str(&format!("{}while\n", pad));
            expr(out, indent + 2, cond);
            for st in body {
                stmt(out, indent + 2, st);
            }
        }
        StmtKind::For { var, iter, body } => {
            out.push_str(&format!("{}for {}\n", pad, var));
            match iter {
                ForIter::Range(lo, hi) => {
                    out.push_str(&format!("{}range\n", " ".repeat(indent + 2)));
                    expr(out, indent + 4, lo);
                    expr(out, indent + 4, hi);
                }
                ForIter::Each(e) => {
                    out.push_str(&format!("{}each\n", " ".repeat(indent + 2)));
                    expr(out, indent + 4, e);
                }
            }
            for st in body {
                stmt(out, indent + 2, st);
            }
        }
        StmtKind::Return { value } => {
            out.push_str(&format!("{}return\n", pad));
            if let Some(v) = value {
                expr(out, indent + 2, v);
            }
        }
        StmtKind::Expr(e) => {
            out.push_str(&format!("{}expr\n", pad));
            expr(out, indent + 2, e);
        }
        StmtKind::Import { path } => {
            out.push_str(&format!("{}import {}\n", pad, path));
        }
    }
}

/// Render a parsed module in the normalized form.
pub fn normalize(module: &Module) -> String {
    let mut out = String::new();
    for s in &module.stmts {
        stmt(&mut out, 0, s);
    }
    out
}
