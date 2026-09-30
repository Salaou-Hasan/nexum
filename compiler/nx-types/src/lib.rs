//! Static type checker for Nexum (`nx check`).
//!
//! v0 rules (advisory only — `nx file.nx` still runs dynamically):
//! - Inferred types, no annotations: Int Float Bool Str List(T) None.
//! - Variables are monomorphic: the first binding fixes the type.
//! - Function params start Unknown; bodies must return consistently.
//! - `import`/`from` are followed into files; members are checked.

use std::collections::{HashMap, HashSet};
use nx_ast::{BinOp, Expr, Program, Span, Stmt, UnaryOp};

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Str,
    List(Box<Ty>),
    Func(Vec<Ty>, Box<Ty>),
    Module(String),
    None,
    Unknown,
}

impl std::fmt::Display for Ty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ty::Int => write!(f, "Int"),
            Ty::Float => write!(f, "Float"),
            Ty::Bool => write!(f, "Bool"),
            Ty::Str => write!(f, "Str"),
            Ty::List(t) => write!(f, "List({t})"),
            Ty::Func(p, r) => {
                let ps: Vec<String> = p.iter().map(|t| t.to_string()).collect();
                write!(f, "fn({}) -> {r}", ps.join(", "))
            }
            Ty::Module(m) => write!(f, "module {m}"),
            Ty::None => write!(f, "None"),
            Ty::Unknown => write!(f, "?"),
        }
    }
}

fn compatible(a: &Ty, b: &Ty) -> bool {
    a == b || matches!(a, Ty::Unknown) || matches!(b, Ty::Unknown)
}

fn is_numeric(t: &Ty) -> bool {
    matches!(t, Ty::Int | Ty::Float | Ty::Unknown)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckError {
    pub message: String,
    pub line: usize,
    pub col: usize,
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "type error at {}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for CheckError {}

#[derive(Debug, Clone, Default)]
struct ModInfo {
    vars: HashMap<String, Ty>,
    funcs: HashMap<String, (Vec<Ty>, Ty)>,
}

struct Checker {
    vars: HashMap<String, Ty>,
    funcs: HashMap<String, (Vec<Ty>, Ty)>,
    modules: HashMap<String, ModInfo>,
    loading: Vec<String>,
    base: std::path::PathBuf,
    errors: Vec<CheckError>,
    in_function: bool,
    returns: Vec<Ty>,
    loop_depth: usize,
    /// Inside `parallel:` tasks: names bound in the enclosing function
    /// (writing them would be a cross-thread lost update).
    parallel_outer: Option<HashSet<String>>,
    /// Names bound so far inside the current parallel task.
    task_bound: HashSet<String>,
    /// Enclosing function's scope (params + assigned), for parallel tasks.
    fn_outer: Option<HashSet<String>>,
}

impl Checker {
    fn err(&mut self, span: Span, msg: String) {
        self.errors.push(CheckError { message: msg, line: span.line, col: span.col });
    }

    fn define(&mut self, name: &str, ty: Ty, span: Span) {
        match self.vars.get(name) {
            None => {
                self.vars.insert(name.to_string(), ty);
            }
            Some(old) if compatible(old, &ty) => {}
            Some(old) => {
                let old = old.clone();
                self.err(span, format!("variable '{name}' is {old}, cannot rebind to {ty}"));
            }
        }
    }

    fn check_block(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.check_stmt(s);
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Assign { name, value, span } => {
                let t = self.check_expr(value);
                if let Some(outer) = &self.parallel_outer {
                    if outer.contains(name) && !self.task_bound.contains(name) {
                        self.err(*span, format!("cannot assign to outer local '{name}' inside parallel (use a module global)"));
                        return;
                    }
                }
                self.task_bound.insert(name.clone());
                self.define(name, t, *span);
            }
            Stmt::AssignOp { name, op, value, span } => {
                if let Some(outer) = &self.parallel_outer.clone() {
                    if outer.contains(name) && !self.task_bound.contains(name) {
                        self.err(*span, format!("cannot assign to outer local '{name}' inside parallel (use a module global)"));
                        return;
                    }
                }
                let rhs = self.check_expr(value);
                match self.vars.get(name).cloned() {
                    None => self.err(*span, format!("undefined variable '{name}'")),
                    Some(cur) => {
                        if let Some(res) = arith_result(&cur, *op, &rhs) {
                            if !compatible(&cur, &res) {
                                self.err(*span, format!("cannot apply '{}=' of {rhs} to {cur} variable '{name}'", op.as_str()));
                            }
                        } else {
                            self.err(*span, format!("operator '{}' not supported for {cur} and {rhs}", op.as_str()));
                        }
                    }
                }
            }
            Stmt::Print { values, .. } => {
                for v in values {
                    self.check_expr(v);
                }
            }
            Stmt::If { cond, then_body, elifs, else_body, .. } => {
                let t = self.check_expr(cond);
                if !matches!(t, Ty::Bool | Ty::Unknown) {
                    self.err(cond.span(), format!("condition must be Bool, found {t}"));
                }
                self.check_block(then_body);
                for (ec, eb) in elifs {
                    let t = self.check_expr(ec);
                    if !matches!(t, Ty::Bool | Ty::Unknown) {
                        self.err(ec.span(), format!("condition must be Bool, found {t}"));
                    }
                    self.check_block(eb);
                }
                if let Some(b) = else_body {
                    self.check_block(b);
                }
            }
            Stmt::While { cond, body, .. } => {
                let t = self.check_expr(cond);
                if !matches!(t, Ty::Bool | Ty::Unknown) {
                    self.err(cond.span(), format!("condition must be Bool, found {t}"));
                }
                self.loop_depth += 1;
                self.check_block(body);
                self.loop_depth -= 1;
            }
            Stmt::For { var, iter, body, span } => {
                let elem = match iter {
                    nx_ast::ForIter::Range { start, end } => {
                        let s = self.check_expr(start);
                        let e = self.check_expr(end);
                        if !matches!(s, Ty::Int | Ty::Unknown) {
                            self.err(start.span(), format!("range start must be Int, found {s}"));
                        }
                        if !matches!(e, Ty::Int | Ty::Unknown) {
                            self.err(end.span(), format!("range end must be Int, found {e}"));
                        }
                        Ty::Int
                    }
                    nx_ast::ForIter::Each(e) => match self.check_expr(e) {
                        Ty::List(t) => *t,
                        Ty::Str => Ty::Str,
                        Ty::Unknown => Ty::Unknown,
                        other => {
                            self.err(e.span(), format!("cannot iterate over {other}"));
                            Ty::Unknown
                        }
                    },
                };
                // Loop var follows the same monomorphic rule.
                match self.vars.get(var).cloned() {
                    None => {
                        self.vars.insert(var.clone(), elem);
                    }
                    Some(old) if compatible(&old, &elem) => {}
                    Some(old) => self.err(*span, format!("loop variable '{var}' is {old}, cannot iterate {elem}")),
                }
                // A for-var shadows: later assigns in this task are fine.
                self.task_bound.insert(var.clone());
                self.loop_depth += 1;
                self.check_block(body);
                self.loop_depth -= 1;
            }
            Stmt::Fn { name, params, body, span } => {
                if self.funcs.contains_key(name) {
                    self.err(*span, format!("function '{name}' already defined"));
                    return;
                }
                // Stub first so the body can call itself recursively.
                self.funcs.insert(
                    name.clone(),
                    (vec![Ty::Unknown; params.len()], Ty::Unknown),
                );
                let saved_vars = std::mem::take(&mut self.vars);
                let saved_in_fn = self.in_function;
                let saved_returns = std::mem::take(&mut self.returns);
                let saved_outer = self.fn_outer.take();
                self.in_function = true;
                for p in params {
                    self.vars.insert(p.clone(), Ty::Unknown);
                }
                // Outer scope for parallel tasks: params + all assigned names.
                let mut outer: HashSet<String> = params.iter().cloned().collect();
                collect_assigned(body, &mut outer);
                self.fn_outer = Some(outer);
                self.check_block(body);
                let rets = std::mem::take(&mut self.returns);
                let mut ret = Ty::None;
                for t in &rets {
                    if ret == Ty::None {
                        ret = t.clone();
                    } else if compatible(&ret, t) {
                        if ret == Ty::Unknown {
                            ret = t.clone();
                        }
                    } else {
                        let r = ret.clone();
                        self.err(*span, format!("inconsistent return types: {r} vs {t}"));
                    }
                }
                self.vars = saved_vars;
                self.in_function = saved_in_fn;
                self.returns = saved_returns;
                self.fn_outer = saved_outer;
                self.funcs.insert(
                    name.clone(),
                    (vec![Ty::Unknown; params.len()], ret),
                );
            }
            Stmt::Return { value, span } => {
                if !self.in_function {
                    self.err(*span, "return outside function".to_string());
                    return;
                }
                if self.parallel_outer.is_some() {
                    self.err(*span, "return inside parallel task is not supported".to_string());
                    return;
                }
                let t = value.as_ref().map(|e| self.check_expr(e)).unwrap_or(Ty::None);
                self.returns.push(t);
            }
            Stmt::Parallel { tasks, span } => {
                if self.parallel_outer.is_some() {
                    self.err(*span, "nested parallel blocks are not supported".to_string());
                    return;
                }
                let outer = self.fn_outer.clone().unwrap_or_default();
                self.parallel_outer = Some(outer);
                for t in tasks {
                    self.task_bound.clear();
                    let saved_loop = self.loop_depth;
                    self.loop_depth = 0;
                    self.check_stmt(t);
                    self.loop_depth = saved_loop;
                }
                self.parallel_outer = None;
            }
            Stmt::Break { span } => {
                if self.loop_depth == 0 {
                    self.err(*span, "break outside loop".to_string());
                }
            }
            Stmt::Continue { span } => {
                if self.loop_depth == 0 {
                    self.err(*span, "continue outside loop".to_string());
                }
            }
            Stmt::Import { module, alias, span } => {
                if self.check_module(module, *span) {
                    let bind = alias.clone().unwrap_or_else(|| module.clone());
                    if let Some(outer) = &self.parallel_outer {
                        if outer.contains(&bind) && !self.task_bound.contains(&bind) {
                            self.err(*span, format!("cannot assign to outer local '{bind}' inside parallel (use a module global)"));
                        }
                    }
                    self.task_bound.insert(bind.clone());
                    self.vars.insert(bind, Ty::Module(module.clone()));
                }
            }
            Stmt::FromImport { module, names, span } => {
                if !self.check_module(module, *span) {
                    return;
                }
                let info = self.modules.get(module).cloned().unwrap_or_default();
                for (name, alias) in names {
                    let bind = alias.clone().unwrap_or_else(|| name.clone());
                    if let Some(outer) = &self.parallel_outer {
                        if outer.contains(&bind) && !self.task_bound.contains(&bind) {
                            self.err(*span, format!("cannot assign to outer local '{bind}' inside parallel (use a module global)"));
                            continue;
                        }
                    }
                    self.task_bound.insert(bind.clone());
                    if let Some(t) = info.vars.get(name) {
                        self.define(&bind, t.clone(), *span);
                    } else if let Some((p, r)) = info.funcs.get(name) {
                        self.define(&bind, Ty::Func(p.clone(), Box::new(r.clone())), *span);
                    } else {
                        self.err(*span, format!("module '{module}' has no member '{name}'"));
                    }
                }
            }
            Stmt::Expr(e) => {
                self.check_expr(e);
            }
        }
    }

    fn check_module(&mut self, name: &str, span: Span) -> bool {
        if self.modules.contains_key(name) {
            return true;
        }
        if self.loading.contains(&name.to_string()) {
            self.err(span, format!("circular import of '{name}'"));
            return false;
        }
        let file = format!("{name}.nx");
        let mut dirs = vec![self.base.clone()];
        if let Ok(p) = std::env::var("NX_PATH") {
            dirs.extend(std::env::split_paths(&p));
        }
        let path = match dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file()) {
            Some(p) => p,
            None => {
                self.err(span, format!("cannot find module '{name}.nx'"));
                return false;
            }
        };
        let (prog, dir) = match std::fs::read_to_string(&path)
            .ok()
            .and_then(|src| {
                let toks = nx_lexer::lex(&src).ok()?;
                nx_parser::parse(toks).ok()
            })
            .map(|prog| {
                let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or(".".into());
                (prog, dir)
            }) {
            Some(v) => v,
            None => {
                self.err(span, format!("cannot parse module '{name}'"));
                return false;
            }
        };
        // Check the submodule with isolated scopes; harvest exports.
        let saved_vars = std::mem::take(&mut self.vars);
        let saved_funcs = std::mem::take(&mut self.funcs);
        let saved_base = std::mem::replace(&mut self.base, dir);
        self.loading.push(name.to_string());
        self.check_block(&prog.stmts);
        self.loading.pop();
        let info = ModInfo { vars: std::mem::replace(&mut self.vars, saved_vars), funcs: std::mem::replace(&mut self.funcs, saved_funcs) };
        self.base = saved_base;
        self.modules.insert(name.to_string(), info);
        true
    }

    fn check_expr(&mut self, expr: &Expr) -> Ty {
        match expr {
            Expr::Int(..) => Ty::Int,
            Expr::Float(..) => Ty::Float,
            Expr::Bool(..) => Ty::Bool,
            Expr::Str(..) => Ty::Str,
            Expr::List(items, span) => {
                let mut elem = Ty::Unknown;
                for it in items {
                    let t = self.check_expr(it);
                    if elem == Ty::Unknown {
                        elem = t;
                    } else if !compatible(&elem, &t) {
                        self.err(*span, format!("mixed list element types: {elem} vs {t}"));
                        break;
                    }
                }
                Ty::List(Box::new(elem))
            }
            Expr::Var(name, span) => {
                if let Some(t) = self.vars.get(name).cloned() {
                    return t;
                }
                if let Some((p, r)) = self.funcs.get(name).cloned() {
                    return Ty::Func(p, Box::new(r));
                }
                self.err(*span, format!("undefined variable '{name}'"));
                Ty::Unknown
            }
            Expr::Attr { base, attr, span } => {
                let b = self.check_expr(base);
                match b {
                    Ty::Module(m) => match self.modules.get(&m).cloned() {
                        Some(info) => {
                            if let Some(t) = info.vars.get(attr) {
                                return t.clone();
                            }
                            if let Some((p, r)) = info.funcs.get(attr) {
                                return Ty::Func(p.clone(), Box::new(r.clone()));
                            }
                            self.err(*span, format!("module '{m}' has no member '{attr}'"));
                            Ty::Unknown
                        }
                        None => {
                            self.err(*span, format!("unknown module '{m}'"));
                            Ty::Unknown
                        }
                    },
                    other => {
                        self.err(*span, format!("only modules support attribute access, found {other}"));
                        Ty::Unknown
                    }
                }
            }
            Expr::Index { base, index, span } => {
                let b = self.check_expr(base);
                let ix = self.check_expr(index);
                if !matches!(ix, Ty::Int | Ty::Unknown) {
                    self.err(*span, format!("index must be Int, found {ix}"));
                }
                match b {
                    Ty::List(t) => *t,
                    Ty::Str => Ty::Str,
                    Ty::Unknown => Ty::Unknown,
                    other => {
                        self.err(*span, format!("only lists and strings support indexing, found {other}"));
                        Ty::Unknown
                    }
                }
            }
            Expr::Unary { op, expr, span } => {
                let t = self.check_expr(expr);
                match (op, &t) {
                    (UnaryOp::Neg, Ty::Int) => Ty::Int,
                    (UnaryOp::Neg, Ty::Float) => Ty::Float,
                    (UnaryOp::Neg, Ty::Unknown) => Ty::Unknown,
                    (UnaryOp::Not, Ty::Bool) => Ty::Bool,
                    (UnaryOp::Not, Ty::Unknown) => Ty::Unknown,
                    _ => {
                        self.err(*span, format!("operator '{}' not supported for {t}", op.as_str()));
                        Ty::Unknown
                    }
                }
            }
            Expr::Binary { left, op, right, span } => {
                let l = self.check_expr(left);
                let r = self.check_expr(right);
                match op {
                    BinOp::And | BinOp::Or => {
                        for (side, t) in [("left", &l), ("right", &r)] {
                            if !matches!(t, Ty::Bool | Ty::Unknown) {
                                self.err(*span, format!("'{0}' operand of '{1}' must be Bool, found {t}", side, op.as_str()));
                            }
                        }
                        Ty::Bool
                    }
                    BinOp::Eq | BinOp::NotEq => {
                        if !(compatible(&l, &r)
                            || (is_numeric(&l) && is_numeric(&r)))
                        {
                            self.err(*span, format!("cannot compare {l} and {r}"));
                        }
                        Ty::Bool
                    }
                    BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                        if !((is_numeric(&l) && is_numeric(&r))
                            || (matches!(l, Ty::Str | Ty::Unknown)
                                && matches!(r, Ty::Str | Ty::Unknown)))
                        {
                            self.err(*span, format!("cannot order {l} and {r}"));
                        }
                        Ty::Bool
                    }
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                        match arith_result(&l, *op, &r) {
                            Some(t) => t,
                            None => {
                                self.err(*span, format!("operator '{}' not supported for {l} and {r}", op.as_str()));
                                Ty::Unknown
                            }
                        }
                    }
                }
            }
            Expr::Call { callee, args, span } => {
                // Builtins.
                if let Expr::Var(name, _) = callee.as_ref() {
                    if name == "len" {
                        if args.len() != 1 {
                            self.err(*span, "len() expects 1 argument".to_string());
                            return Ty::Unknown;
                        }
                        let t = self.check_expr(&args[0]);
                        if !matches!(t, Ty::List(_) | Ty::Str | Ty::Unknown) {
                            self.err(*span, format!("len() only supports lists and strings, found {t}"));
                        }
                        return Ty::Int;
                    }
                    if name == "push" {
                        if args.len() != 2 {
                            self.err(*span, "push() expects 2 arguments".to_string());
                            return Ty::Unknown;
                        }
                        if !matches!(&args[0], Expr::Var(..)) {
                            self.err(*span, "push() first argument must be a list variable".to_string());
                        }
                        let lt = self.check_expr(&args[0]);
                        let et = self.check_expr(&args[1]);
                        match lt {
                            Ty::List(t) if compatible(&t, &et) => {}
                            Ty::List(_) => {
                                self.err(*span, format!("push() element type mismatch"));
                            }
                            Ty::Unknown => {}
                            other => {
                                self.err(*span, format!("push() needs a list, found {other}"));
                            }
                        }
                        return Ty::None;
                    }
                }
                let f = self.check_expr(callee);
                match f {
                    Ty::Func(params, ret) => {
                        if params.len() != args.len() {
                            self.err(*span, format!("expects {} args, got {}", params.len(), args.len()));
                        }
                        for (p, a) in params.iter().zip(args.iter()) {
                            let at = self.check_expr(a);
                            if !compatible(p, &at) {
                                self.err(a.span(), format!("argument must be {p}, found {at}"));
                            }
                        }
                        *ret
                    }
                    Ty::Unknown => {
                        for a in args {
                            self.check_expr(a);
                        }
                        Ty::Unknown
                    }
                    other => {
                        for a in args {
                            self.check_expr(a);
                        }
                        self.err(*span, format!("not callable: {other}"));
                        Ty::Unknown
                    }
                }
            }
        }
    }
}

fn arith_result(l: &Ty, op: BinOp, r: &Ty) -> Option<Ty> {
    use Ty::*;
    // v0 operator matrix (mirrors the interpreter).
    if matches!(op, BinOp::Add) {
        if matches!((l, r), (Str, Str)) {
            return Some(Str);
        }
    }
    match (l, r) {
        (Unknown, _) | (_, Unknown) => Some(Unknown),
        (Int, Int) => Some(Int),
        (Int, Float) | (Float, Int) | (Float, Float) => Some(Float),
        _ => Option::None,
    }
}

pub fn check_source(source: &str, base: &std::path::Path) -> Result<(), Vec<CheckError>> {
    let tokens = nx_lexer::lex(source).map_err(|e| {
        vec![CheckError { message: e.message, line: e.line, col: e.col }]
    })?;
    let prog = nx_parser::parse(tokens).map_err(|e| {
        vec![CheckError { message: e.message, line: e.line, col: e.col }]
    })?;
    check_program(&prog, base)
}

pub fn check_program(prog: &Program, base: &std::path::Path) -> Result<(), Vec<CheckError>> {
    let mut c = Checker {
        base: base.to_path_buf(),
        ..Default::default()
    };
    // Checker needs Default for HashMaps/Vecs/bool/usize/String.
    c.check_block(&prog.stmts);
    if c.errors.is_empty() {
        Ok(())
    } else {
        Err(c.errors)
    }
}

// Default impls for the checker state.
impl Default for Checker {
    fn default() -> Self {
        Self {
            vars: HashMap::new(),
            funcs: HashMap::new(),
            modules: HashMap::new(),
            loading: Vec::new(),
            base: ".".into(),
            errors: Vec::new(),
            in_function: false,
            returns: Vec::new(),
            loop_depth: 0,
            parallel_outer: None,
            task_bound: HashSet::new(),
            fn_outer: None,
        }
    }
}

/// All assigned names in a body (for parallel outer-scope computation).
fn collect_assigned(body: &[Stmt], out: &mut HashSet<String>) {
    for s in body {
        match s {
            Stmt::Assign { name, .. } => {
                out.insert(name.clone());
            }
            Stmt::For { var, body, .. } => {
                out.insert(var.clone());
                collect_assigned(body, out);
            }
            Stmt::If { then_body, elifs, else_body, .. } => {
                collect_assigned(then_body, out);
                for (_, b) in elifs {
                    collect_assigned(b, out);
                }
                if let Some(b) = else_body {
                    collect_assigned(b, out);
                }
            }
            Stmt::While { body, .. } => collect_assigned(body, out),
            Stmt::Parallel { tasks, .. } => {
                for t in tasks {
                    collect_assigned(std::slice::from_ref(t), out);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) {
        if let Err(es) = check_source(src, std::path::Path::new(".")) {
            panic!("{src:?} unexpectedly failed: {es:?}");
        }
    }

    fn err(src: &str) -> Vec<CheckError> {
        check_source(src, std::path::Path::new(".")).expect_err("expected type errors")
    }

    #[test]
    fn basic_program_passes() {
        ok("x = 1\ny = x + 2.5\nprint(x, y)\n");
    }

    #[test]
    fn rebind_different_type_errors() {
        let es = err("x = \"a\"\nx = 1\n");
        assert!(es.iter().any(|e| e.message.contains("cannot rebind")));
    }

    #[test]
    fn undefined_var_errors() {
        assert!(!err("print(y)\n").is_empty());
    }

    #[test]
    fn arith_mismatch_errors() {
        assert!(!err("x = 1 + \"a\"\n").is_empty());
    }

    #[test]
    fn non_bool_condition_errors() {
        assert!(!err("if 1:\n    print(1)\n").is_empty());
    }

    #[test]
    fn call_arity_errors() {
        assert!(!err("fn f(a):\n    return a\nprint(f(1, 2))\n").is_empty());
    }

    #[test]
    fn len_push_checked() {
        ok("a = [1]\npush(a, 2)\nprint(len(a))\n");
        assert!(!err("print(len(1))\n").is_empty());
        assert!(!err("a = 1\npush(a, 2)\n").is_empty());
    }

    #[test]
    fn return_consistency() {
        ok("fn f(n):\n    if n:\n        return 1\n    else:\n        return 2\n");
        assert!(!err("fn f(n):\n    if n:\n        return 1\n    else:\n        return \"a\"\n").is_empty());
    }

    #[test]
    fn break_outside_errors() {
        assert!(!err("break\n").is_empty());
    }

    #[test]
    fn parallel_outer_write_errors() {
        assert!(!err("fn f():\n    x = 1\n    parallel:\n        x = 2\n").is_empty());
    }

    #[test]
    fn parallel_return_errors() {
        assert!(!err("fn f():\n    parallel:\n        return 1\n").is_empty());
    }

    #[test]
    fn parallel_nested_errors() {
        assert!(!err("parallel:\n    parallel:\n        print(1)\n").is_empty());
    }

    #[test]
    fn parallel_ok() {
        ok("a = 0\nparallel:\n    a = 1\n    print(2)\n");
    }

    #[test]
    fn list_indexing() {
        ok("a = [1, 2]\nprint(a[0])\n");
        assert!(!err("a = 1\nprint(a[0])\n").is_empty());
    }
}
