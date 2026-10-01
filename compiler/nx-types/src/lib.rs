//! Static type checker for Nexum (`nx check`).
//!
//! v0 rules (advisory only — `nx file.nx` still runs dynamically):
//! - Inferred types, no annotations: Int Float Bool Str List(T) None.
//! - Variables are monomorphic: the first binding fixes the type.
//! - Function params start Unknown; bodies must return consistently.
//! - `import`/`from` are followed into files; members are checked.

use std::collections::{HashMap, HashSet};
use nx_ast::{BinOp, Expr, Program, Span, Stmt, UnaryOp};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
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

impl Default for Ty {
    fn default() -> Self {
        Ty::Unknown
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

/// Static shape of one function, as the optimizer sees it. Codegen uses
/// `locals` to pick a machine representation per name; anything not listed
/// (or not a scalar) stays boxed in the dynamic value representation.
#[derive(Debug, Clone, Default)]
pub struct FnInfo {
    pub locals: HashMap<String, Ty>,
    pub params: Vec<String>,
    pub ret: Ty,
}

impl Ty {
    /// Scalars the backend can hold in a bare register (i64/double/i1).
    pub fn is_scalar(&self) -> bool {
        matches!(self, Ty::Int | Ty::Float | Ty::Bool)
    }
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
    /// Parameters of the function being checked. Their types start Unknown
    /// and are narrowed from how the body uses them, which is what lets
    /// codegen keep them out of the box.
    param_names: Vec<String>,
    /// Parameters seen in arithmetic, candidates for the integer default.
    numeric_params: Vec<String>,
    /// Set when a Float appears anywhere in the current function, including
    /// in an expression that never lands in a local.
    saw_float: bool,
    /// `local = param` plain copies found in the current body, resolved
    /// once the pass knows a type for either end.
    copies: Vec<(String, String)>,
    /// Per-function inferred shapes, harvested for the optimizer.
    inferred: HashMap<(String, String), FnInfo>,
    /// Module the checker is currently inside, for inference keys.
    module_name: String,
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
            Some(old) if compatible(old, &ty) => {
                // An unresolved value assigned to a known-typed variable
                // makes the variable itself unresolved. The backend gives
                // a known scalar a typed slot, so keeping the narrow type
                // here would let a value of the wrong dynamic type land in
                // it. Widening to Unknown costs the optimization, not
                // correctness.
                if ty == Ty::Unknown && *old != Ty::Unknown {
                    self.vars.insert(name.to_string(), Ty::Unknown);
                }
            }
            Some(old) => {
                let old = old.clone();
                self.err(span, format!("variable '{name}' is {old}, cannot rebind to {ty}"));
            }
        }
    }

    /// Narrow a parameter from the way the body uses it. NX has no
    /// overloading and no generics, so a use site fixes the type: `n - 1`
    /// proves Int, `n < 1.5` proves Float, `not n` proves Bool. Non-params
    /// and already-known types are left alone (locals are monomorphic).
    fn expect_param(&mut self, e: &Expr, ty: Ty) {
        let Expr::Var(name, _) = e else { return };
        if !self.param_names.iter().any(|p| p == name) {
            return;
        }
        match self.vars.get(name) {
            Some(Ty::Unknown) => {
                self.vars.insert(name.clone(), ty);
            }
            Some(old) if *old == ty => {}
            // Conflicting uses: leave the first answer and let the
            // expression-level check report the mismatch.
            _ => {}
        }
    }

    /// Record a parameter used in arithmetic. This does not pin the type:
    /// NX promotes mixed Int/Float, so `n * 2` is well-typed for an Int n
    /// and for a Float n, and neither operand's type is forced. It only
    /// marks the parameter as a candidate for the integer default applied
    /// once the whole body has been seen.
    fn mark_numeric(&mut self, e: &Expr) {
        if let Expr::Var(name, _) = e {
            if self.param_names.iter().any(|p| p == name) {
                self.numeric_params.push(name.clone());
            }
        }
    }

    /// Record `local = param`, a plain copy that makes both the same
    /// type. Neither side learns anything on its own, so the pair is
    /// resolved after the body by copying whichever end got typed.
    fn mark_copy(&mut self, local: &str, value: &Expr) {
        if let Expr::Var(p, _) = value {
            if self.param_names.iter().any(|x| x == p) {
                self.copies.push((local.to_string(), p.clone()));
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
                self.mark_copy(name, value);
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
                self.expect_param(cond, Ty::Bool);
                self.check_block(then_body);
                for (ec, eb) in elifs {
                    let t = self.check_expr(ec);
                    if !matches!(t, Ty::Bool | Ty::Unknown) {
                        self.err(ec.span(), format!("condition must be Bool, found {t}"));
                    }
                    self.expect_param(ec, Ty::Bool);
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
                self.expect_param(cond, Ty::Bool);
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
                        self.expect_param(start, Ty::Int);
                        self.expect_param(end, Ty::Int);
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
                let saved_params = std::mem::take(&mut self.param_names);
                self.in_function = true;
                self.param_names = params.clone();
                // Outer scope for parallel tasks: params + all assigned names.
                let mut outer: HashSet<String> = params.iter().cloned().collect();
                collect_assigned(body, &mut outer);
                self.fn_outer = Some(outer);
                // Two passes. A parameter's type comes from how the body
                // uses it, so pass one discovers the types and pass two
                // checks the body with them already known. Without the
                // second pass everything derived from a parameter would
                // come out unresolved: `t = n * 2` is only Int if `n` is.
                let mut rets: Vec<Ty>;
                let mut ret: Ty;
                // What we have learned about each parameter so far, fed
                // back in on the next pass. Seeding the types is what makes
                // the second pass useful: with `n` already Int, `t = n * 2`
                // types `t` instead of widening it to unresolved.
                let mut known: HashMap<String, Ty> = HashMap::new();
                let mut passes = 0;
                loop {
                    self.copies.clear();
                    self.saw_float = false;
                    self.vars.clear();
                    self.returns.clear();
                    // A function body sees the module's own variables, the
                    // same ones codegen resolves to globals. Seeding the
                    // scope with them is what lets a function read a
                    // top-level constant instead of reporting it undefined.
                    for (name, ty) in &saved_vars {
                        self.vars.insert(name.clone(), ty.clone());
                    }
                    for p in params {
                        self.vars
                            .insert(p.clone(), known.get(p).cloned().unwrap_or(Ty::Unknown));
                    }
                    let entry_vars = self.vars.clone();
                    // Only the final pass's diagnostics are reported: an
                    // earlier pass sees types that later passes refine, so
                    // its complaints can be spurious. They are still
                    // carried here and dropped by the truncate at the top
                    // of the next iteration.
                    let nerr = self.errors.len();
                    self.check_block(body);
                    let pass_errors: Vec<CheckError> = self.errors.drain(nerr..).collect();
                    // A numeric parameter nothing else pinned defaults to
                    // Int, but only in a function that never touches a
                    // Float. With no float in the body there is nothing a
                    // Float argument could mean, so Int is the only reading
                    // left and the caller gets a real signature to check.
                    //
                    // With a Float present, leaving the parameter dynamic is
                    // the honest answer: `zr - zi + cx` and `x / 2.0` are
                    // well-typed whatever cx and x are, and pinning them
                    // made `fn mandel(cx, cy, maxiter)` come out as
                    // `(Int, Int, Int)` and reject Float arguments outright.
                    if !self.saw_float {
                        for p in std::mem::take(&mut self.numeric_params) {
                            if self.vars.get(&p) == Some(&Ty::Unknown) {
                                self.vars.insert(p, Ty::Int);
                            }
                        }
                    } else {
                        self.numeric_params.clear();
                        self.saw_float = false;
                    }
                    // A plain copy makes both ends the same type; take it
                    // from whichever end this pass managed to type.
                    for (local, p) in std::mem::take(&mut self.copies) {
                        let from_local = self.vars.get(&local).cloned();
                        let from_param = self.vars.get(&p).cloned();
                        // Take the type from whichever end is known, or
                        // accept it when both already agree. Anything else
                        // (both unresolved, or a real disagreement) stays
                        // unresolved and is reported by define.
                        let resolved = match (from_local, from_param) {
                            (Some(Ty::Unknown), Some(t)) => Some(t),
                            (Some(t), Some(Ty::Unknown)) => Some(t),
                            (Some(l), Some(p)) if l == p => Some(l),
                            _ => None,
                        };
                        if let Some(t) = resolved {
                            self.vars.insert(local, t.clone());
                            self.vars.insert(p, t);
                        }
                    }
                    let snapshot = self.vars.clone();
                    for p in params {
                        if let Some(t) = snapshot.get(p) {
                            if *t != Ty::Unknown {
                                known.insert(p.clone(), t.clone());
                            }
                        }
                    }
                    rets = std::mem::take(&mut self.returns);
                    ret = Ty::None;
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
                    passes += 1;
                    // Keep going while a pass still teaches us something.
                    // Bounded because each pass can only remove Unknowns.
                    let converged = self.vars == entry_vars || passes >= 4;
                    if converged {
                        // Report the last pass, which had the best types.
                        self.errors.extend(pass_errors);
                    }
                    if converged {
                        break;
                    }
                }
                let fn_locals = self.vars.clone();
                let param_tys: Vec<Ty> =
                    params.iter().map(|p| fn_locals.get(p).cloned().unwrap_or(Ty::Unknown)).collect();
                self.vars = saved_vars;
                self.in_function = saved_in_fn;
                self.returns = saved_returns;
                self.fn_outer = saved_outer;
                self.param_names = saved_params;
                self.inferred.insert(
                    (self.module_name.clone(), name.clone()),
                    FnInfo { locals: fn_locals, params: params.clone(), ret: ret.clone() },
                );
                self.funcs.insert(
                    name.clone(),
                    (param_tys, ret),
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
        let saved_module = std::mem::replace(&mut self.module_name, name.to_string());
        self.loading.push(name.to_string());
        self.check_block(&prog.stmts);
        self.loading.pop();
        let info = ModInfo { vars: std::mem::replace(&mut self.vars, saved_vars), funcs: std::mem::replace(&mut self.funcs, saved_funcs) };
        self.inferred.insert(
            (name.to_string(), "<top>".to_string()),
            FnInfo { locals: info.vars.clone(), params: Vec::new(), ret: Ty::None },
        );
        self.base = saved_base;
        self.module_name = saved_module;
        self.modules.insert(name.to_string(), info);
        true
    }

    fn check_expr(&mut self, expr: &Expr) -> Ty {
        let t = self.check_expr_inner(expr);
        if t == Ty::Float {
            // A float can appear in an expression without ever landing in
            // a local, as in `fn half(x): return x / 2.0`. Remember it so
            // the parameter rule below can see the function is float-shaped.
            self.saw_float = true;
        }
        t
    }

    fn check_expr_inner(&mut self, expr: &Expr) -> Ty {
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
                self.expect_param(index, Ty::Int);
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
                        self.expect_param(left, Ty::Bool);
                        self.expect_param(right, Ty::Bool);
                        Ty::Bool
                    }
                    BinOp::Eq | BinOp::NotEq => {
                        if !(compatible(&l, &r)
                            || (is_numeric(&l) && is_numeric(&r)))
                        {
                            self.err(*span, format!("cannot compare {l} and {r}"));
                        }
                        // A numeric comparison against a known scalar pins
                        // the other side to the same scalar type.
                        if is_numeric(&l) && l != Ty::Unknown {
                            self.expect_param(right, l.clone());
                        }
                        if is_numeric(&r) && r != Ty::Unknown {
                            self.expect_param(left, r.clone());
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
                        if is_numeric(&l) && l != Ty::Unknown {
                            self.expect_param(right, l.clone());
                        }
                        if is_numeric(&r) && r != Ty::Unknown {
                            self.expect_param(left, r.clone());
                        }
                        if l == Ty::Str || r == Ty::Str {
                            self.expect_param(left, Ty::Str);
                            self.expect_param(right, Ty::Str);
                        }
                        Ty::Bool
                    }
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                        if matches!(op, BinOp::Add)
                            && (l == Ty::Str || r == Ty::Str)
                        {
                            self.expect_param(left, Ty::Str);
                            self.expect_param(right, Ty::Str);
                        } else {
                            self.mark_numeric(left);
                            self.mark_numeric(right);
                        }
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
                            if p != &Ty::Unknown {
                                self.expect_param(a, p.clone());
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
        module_name: "__main__".to_string(),
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

/// Inferred static shapes for every function in the program, keyed by
/// `(module, function)`. Module-level code is reported under `<top>`.
/// Returns the same errors `check_program` would, so callers can use this
/// in place of a separate check pass.
pub fn infer_program(
    prog: &Program,
    base: &std::path::Path,
) -> Result<HashMap<(String, String), FnInfo>, Vec<CheckError>> {
    let mut c = Checker {
        base: base.to_path_buf(),
        module_name: "__main__".to_string(),
        ..Default::default()
    };
    c.check_block(&prog.stmts);
    if !c.errors.is_empty() {
        return Err(c.errors);
    }
    let mut out = std::mem::take(&mut c.inferred);
    // Top-level code shares one flat scope across the module.
    out.insert(("__main__".to_string(), "<top>".to_string()), FnInfo {
        locals: c.vars.clone(),
        params: Vec::new(),
        ret: Ty::None,
    });
    Ok(out)
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
            param_names: Vec::new(),
            numeric_params: Vec::new(),
            saw_float: false,
            copies: Vec::new(),
            inferred: HashMap::new(),
            module_name: String::new(),
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

    fn infer(src: &str) -> HashMap<(String, String), FnInfo> {
        let tokens = nx_lexer::lex(src).unwrap();
        let prog = nx_parser::parse(tokens).unwrap();
        infer_program(&prog, std::path::Path::new(".")).unwrap()
    }

    #[test]
    fn param_int_from_arithmetic() {
        let m = infer("fn fib(n):\n    if n <= 1:\n        return n\n    else:\n        return fib(n - 1) + fib(n - 2)\n");
        assert_eq!(m[&("__main__".into(), "fib".into())].locals["n"], Ty::Int);
    }

    #[test]
    fn function_sees_module_globals() {
        // Codegen resolves a top-level name to a global, so the checker has
        // to see it too. It used to empty the enclosing scope and report
        // every module-level constant as undefined inside a function.
        ok("g = 4\nfn f(k):\n    return g + k\nprint(f(1))\n");
        let m = infer("g = 4\nfn f(k):\n    return g + k\n");
        let f = &m[&("__main__".into(), "f".into())];
        assert_eq!(f.locals["g"], Ty::Int);
    }

    #[test]
    fn numeric_param_defaults_to_int_in_an_int_function() {
        // Nothing in the body is a Float, so Int is the only reading left.
        let m = infer("fn f(n):\n    return n * 2\n");
        assert_eq!(m[&("__main__".into(), "f".into())].locals["n"], Ty::Int);
        ok("fn f(n):\n    return n * 2\nprint(f(21))\n");
    }

    #[test]
    fn numeric_param_stays_dynamic_in_a_float_function() {
        // `x / 2.0` is well-typed for an Int x and for a Float x, so the
        // parameter is dynamic. Pinning it made this function reject a
        // Float argument, which is ordinary code failing to compile.
        let m = infer("fn half(x):\n    return x / 2.0\n");
        assert_eq!(m[&("__main__".into(), "half".into())].locals["x"], Ty::Unknown);
        ok("fn half(x):\n    return x / 2.0\nprint(half(1))\n");
        ok("fn half(x):\n    return x / 2.0\nprint(half(1.0))\n");
    }

    #[test]
    fn float_function_takes_float_coordinates() {
        // The regression that motivated the rule: this used to infer
        // (Int, Int, Int) and refuse Float arguments.
        ok("fn mandel(cx, cy, maxiter):\n    zr = 0.0\n    zi = 0.0\n    i = 0\n    while i < maxiter:\n        zr2 = zr * zr\n        zi2 = zi * zi\n        if zr2 + zi2 > 4.0:\n            return i\n        zi = 2.0 * zr * zi + cy\n        zr = zr2 - zi2 + cx\n        i = i + 1\n    return maxiter\nprint(mandel(0.5, 0.5, 10))\n");
    }

    #[test]
    fn comparison_against_float_pins_param_to_float() {
        // An ordering comparison has no widening, so the type is forced.
        let m = infer("fn over(x):\n    if x < 1.5:\n        return 1\n    else:\n        return 0\n");
        assert_eq!(m[&("__main__".into(), "over".into())].locals["x"], Ty::Float);
    }

    #[test]
    fn param_bool_from_condition() {
        let m = infer("fn neg(b):\n    if b:\n        return 1\n    else:\n        return 0\n");
        assert_eq!(m[&("__main__".into(), "neg".into())].locals["b"], Ty::Bool);
    }

    #[test]
    fn param_int_from_index() {
        let m = infer("fn at(xs, i):\n    return xs[i]\n");
        assert_eq!(m[&("__main__".into(), "at".into())].locals["i"], Ty::Int);
    }

    #[test]
    fn unused_param_stays_unknown() {
        let m = infer("fn id(x):\n    return 1\n");
        assert_eq!(m[&("__main__".into(), "id".into())].locals["x"], Ty::Unknown);
    }

    #[test]
    fn inferred_param_rejects_wrong_argument() {
        // x is proven Int by `x - 1`, so passing a string is an error.
        assert!(!err("fn f(x):\n    return x - 1\nprint(f(\"a\"))\n").is_empty());
    }

    #[test]
    fn top_level_scope_is_reported() {
        let m = infer("a = 1\nb = \"s\"\n");
        let top = &m[&("__main__".into(), "<top>".into())];
        assert_eq!(top.locals["a"], Ty::Int);
        assert_eq!(top.locals["b"], Ty::Str);
    }

    #[test]
    fn unknown_assignment_widens_variable() {
        // t is Int from its first binding, but the loop element is
        // untyped, so t must widen: the backend gives a known scalar a
        // typed slot and a narrower type would be unsound.
        let m = infer("fn f(xs):\n    t = 0\n    for x in xs:\n        t = t + x\n    return t\n");
        let f = &m[&("__main__".into(), "f".into())];
        assert_eq!(f.locals["t"], Ty::Unknown);
    }

    #[test]
    fn widening_is_sticky() {
        // Once widened, a later known assignment must not re-narrow.
        let m = infer("fn f(xs):\n    t = 0\n    for x in xs:\n        t = t + x\n    t = 5\n    return t\n");
        assert_eq!(m[&("__main__".into(), "f".into())].locals["t"], Ty::Unknown);
    }
}