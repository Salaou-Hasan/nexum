use std::collections::{HashMap, HashSet};
use nx_ast::{BinOp, Expr, Program, Stmt, UnaryOp};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    List(Vec<Value>),
    /// A declared type's instance. Fields are positional, matching
    /// declaration order. The declaring module travels with the value so
    /// field layout always resolves deterministically -- two modules may
    /// declare the same type name, and searching by name alone would pick
    /// one at hash order.
    Record { module: String, type_name: String, fields: Vec<Value> },
    /// Insertion-ordered map. Order is part of the contract rather than an
    /// implementation detail: iterating a dict has to be reproducible for
    /// `parallel:` determinism to mean anything, so this is a `Vec` of
    /// pairs with linear lookup. The alternative, a hash map, would make
    /// iteration order depend on hashing and break that guarantee.
    Dict(Vec<(Value, Value)>),
    Module(String),
    Func { module: String, name: String },
    None,
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Int(i) => write!(f, "{i}"),
            Value::Float(x) => write!(f, "{}", fmt_float(*x)),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Str(s) => write!(f, "{s}"),
            Value::List(items) => {
                let parts: Vec<String> = items.iter().map(|v| v.to_string()).collect();
                write!(f, "[{}]", parts.join(", "))
            }
            Value::Dict(pairs) => {
                let parts: Vec<String> = pairs
                    .iter()
                    .map(|(k, v)| format!("{}: {}", k.to_string(), v.to_string()))
                    .collect();
                write!(f, "{{{}}}", parts.join(", "))
            }
            Value::Record { type_name, fields, .. } => {
                let parts: Vec<String> = fields.iter().map(|v| v.to_string()).collect();
                write!(f, "{}({})", type_name, parts.join(", "))
            }
            Value::Module(m) => write!(f, "<module {m}>"),
            Value::Func { name, .. } => write!(f, "<fn {name}>"),
            Value::None => write!(f, "none"),
        }
    }
}

fn fmt_float(v: f64) -> String {
    if !v.is_finite() {
        return v.to_string();
    }
    if v == 0.0 {
        return "0".to_string();
    }
    let abs = v.abs();
    if abs >= 1e15 || abs < 1e-12 {
        return format!("{v:?}");
    }
    const SIG: i32 = 15;
    let exp = abs.log10().floor() as i32;
    let scale = 10f64.powi(SIG - 1 - exp);
    let r = (v * scale).round() / scale;
    if r == 0.0 {
        return "0".to_string();
    }
    let decimals = (SIG - 1 - exp).clamp(0, 15) as usize;
    let s = format!("{r:.decimals$}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeError {
    pub message: String,
    pub line: nx_ast::LineNo,
    pub col: nx_ast::ColNo,
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "runtime error at {}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for RuntimeError {}

const LOOP_LIMIT: u64 = 10_000_000;
const CALL_LIMIT: usize = 500;
/// Memo cache bound (entries across all functions; cleared when full).
const MEMO_CAP: usize = 4096;

/// Hashable scalar subset for memo keys. Only immutable scalars qualify;
/// lists (mutable) never enter the cache.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Scalar {
    I(i64),
    F(u64),
    B(bool),
    S(String),
}

fn scalar_of(v: &Value) -> Option<Scalar> {
    match v {
        Value::Int(i) => Some(Scalar::I(*i)),
        Value::Float(x) => Some(Scalar::F(x.to_bits())),
        Value::Bool(b) => Some(Scalar::B(*b)),
        Value::Str(s) => Some(Scalar::S(s.clone())),
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MemoKey {
    module: String,
    name: String,
    args: Vec<Scalar>,
}

#[derive(Debug, Default)]
struct Memo {
    map: HashMap<MemoKey, Value>,
}

#[derive(Debug, Clone, Default)]
struct Function {
    params: Vec<String>,
    body: Vec<Stmt>,
}

/// A method as the interpreter sees it. Parameters exclude the receiver;
/// `receiver` governs the write-back in `call_method`.
#[derive(Debug, Clone)]
struct MethodDef {
    name: String,
    params: Vec<String>,
    body: Vec<Stmt>,
    module: String,
    receiver: nx_ast::ReceiverKind,
}

#[derive(Debug, Clone, Default)]
struct Module {
    vars: HashMap<String, Value>,
    funcs: HashMap<String, Function>,
    /// Declared `type` layouts: type name to field names in order. The
    /// name is kept on the value as well, so a record still prints
    /// usefully and its fields stay resolvable after it has moved.
    types: HashMap<String, Vec<String>>,
    dir: std::path::PathBuf,
}

#[derive(Debug, Clone, Default)]
struct Frame {
    vars: HashMap<String, Value>,
    module: String,
}

const MAIN_MODULE: &str = "__main__";

#[derive(Debug, Clone)]
enum Flow {
    Return(Value),
    Break,
    Continue,
}

#[derive(Default)]
pub struct Interpreter {
    frames: Vec<Frame>,
    modules: HashMap<String, Module>,
    programs: HashMap<String, Program>,
    loading: Vec<String>,
    current: String,
    pub output: Vec<String>,
    call_depth: usize,
    /// Module writes performed at top level (for parallel join merges).
    touched: HashSet<(String, String)>,
    /// Inside a parallel task: names readable from the enclosing scope.
    /// Writing one is a cross-thread lost update -> runtime error.
    outer: Option<HashSet<String>>,
    /// Task spawned at top level (no enclosing frame): assigns behave
    /// like module-level assigns so results survive the join.
    task_top: bool,
    /// Functions proven memoizable (pure, closed over no shared state).
    memo_ok: HashSet<(String, String)>,
    /// Functions already classified (avoid re-analyzing per call).
    memo_seen: HashSet<(String, String)>,
    /// `from m import T [as U]` in a module: (importer, alias) to
    /// (declaring module, type). Types are constructed, never held, so an
    /// imported type registers an alias here rather than a value binding.
    type_alias: HashMap<(String, String), (String, String)>,
    /// Methods by (canonical type, method). The declaring module travels
    /// with each definition so a method keeps resolving after crossing an
    /// `import` boundary.
    methods: HashMap<(String, String), HashMap<String, MethodDef>>,
    /// Shared memo cache (across parallel tasks). None when NX_NOMEMO=1.
    memo: Option<std::sync::Arc<std::sync::Mutex<Memo>>>,
}

impl Interpreter {
    pub fn new() -> Self {
        Self::with_base(&std::env::current_dir().unwrap_or(".".into()))
    }

    pub fn with_base(base: &std::path::Path) -> Self {
        let mut modules = HashMap::new();
        modules.insert(
            MAIN_MODULE.to_string(),
            Module { dir: base.to_path_buf(), ..Default::default() },
        );
        Self {
            modules,
            current: MAIN_MODULE.to_string(),
            ..Default::default()
        }
    }

    pub fn run(&mut self, prog: &Program) -> Result<(), RuntimeError> {
        self.programs.insert(MAIN_MODULE.to_string(), prog.clone());
        match self.exec_block(&prog.stmts)? {
            None => Ok(()),
            Some(Flow::Return(v)) => Err(RuntimeError {
                message: format!("return outside function ({v})"),
                line: 1,
                col: 1,
            }),
            Some(Flow::Break) => Err(RuntimeError {
                message: "break outside loop".to_string(),
                line: 1,
                col: 1,
            }),
            Some(Flow::Continue) => Err(RuntimeError {
                message: "continue outside loop".to_string(),
                line: 1,
                col: 1,
            }),
        }
    }

    fn exec_block(&mut self, stmts: &[Stmt]) -> Result<Option<Flow>, RuntimeError> {
        for stmt in stmts {
            if let Some(f) = self.exec_stmt(stmt)? {
                return Ok(Some(f));
            }
        }
        Ok(None)
    }

    fn exec_stmt(&mut self, stmt: &Stmt) -> Result<Option<Flow>, RuntimeError> {
        match stmt {
            // Split out into its own `inline(never)` method: `exec_stmt` shares a
            // single stack frame across every arm, so a bulky arm costs
            // stack on every recursive call rather than only its own.
            Stmt::Assign { targets, values, span } => {
                self.exec_assign(targets, values, *span)?;
                Ok(None)
            }
            Stmt::AssignOp { target, op, value, span } => {
                let cur = self.eval_expr(&target.as_expr(*span))?;
                let rhs = self.eval_expr(value)?;
                // `apply_binop`, not `apply_arith`: `x %= 3` and `x <<= 2`
                // are valid, and routing them through the arithmetic-only
                // path would hit an unreachable arm.
                let v = self.apply_binop(cur, *op, rhs, span.line, span.col)?;
                self.store_target(target, v)?;
                Ok(None)
            }
            Stmt::Del { targets, span } => {
                self.exec_del(targets, *span)?;
                Ok(None)
            }
            Stmt::Assert { cond, message, span } => {
                let v = self.eval_expr(cond)?;
                if !Self::expect_bool(v, cond.span())? {
                    return Err(self.assertion_failed(message, *span));
                }
                Ok(None)
            }
            // Registering the layout is all a declaration does; it binds no
            // runtime value.
            Stmt::TypeDecl { name, fields, .. } => {
                let cur = self.current_module();
                if let Some(m) = self.modules.get_mut(&cur) {
                    m.types
                        .insert(name.clone(), fields.iter().map(|f| f.name.clone()).collect());
                }
                Ok(None)
            }
            Stmt::Impl { type_name, methods, span } => {
                // Registering the methods is all an impl block does. The
                // type must be declared in this module: anything else is an
                // orphan impl, rejected statically and mistyped dynamically
                // alike. (Imported types resolve through the alias map at
                // each call site instead.)
                let cur = self.current_module();
                let known = self
                    .modules
                    .get(&cur)
                    .map(|m| m.types.contains_key(type_name))
                    .unwrap_or(false);
                if !known {
                    return Err(RuntimeError {
                        message: format!("unknown type '{type_name}'"),
                        line: span.line,
                        col: span.col,
                    });
                }
                let entry = self
                    .methods
                    .entry((cur.clone(), type_name.clone()))
                    .or_default();
                for m in methods {
                    entry.insert(m.name.clone(), MethodDef {
                        name: m.name.clone(),
                        params: m.params.clone(),
                        body: m.body.clone(),
                        module: cur.clone(),
                        receiver: m.receiver,
                    });
                }
                Ok(None)
            }
            Stmt::Print { values, .. } => {
                let mut parts = Vec::new();
                for v in values {
                    parts.push(self.eval_expr(v)?.to_string());
                }
                self.output.push(parts.join(" "));
                Ok(None)
            }
            Stmt::If { cond, then_body, elifs, else_body, .. } => {
                let v = self.eval_expr(cond)?;
                let c = Self::expect_bool(v, cond.span())?;
                if c {
                    return self.exec_block(then_body);
                }
                for (ec, eb) in elifs {
                    let v = self.eval_expr(ec)?;
                    if Self::expect_bool(v, ec.span())? {
                        return self.exec_block(eb);
                    }
                }
                if let Some(else_body) = else_body {
                    return self.exec_block(else_body);
                }
                Ok(None)
            }
            Stmt::While { cond, body, .. } => {
                let mut iter: u64 = 0;
                loop {
                    let v = self.eval_expr(cond)?;
                    if !Self::expect_bool(v, cond.span())? {
                        break;
                    }
                    match self.exec_block(body)? {
                        None => {}
                        Some(Flow::Return(v)) => return Ok(Some(Flow::Return(v))),
                        Some(Flow::Break) => break,
                        Some(Flow::Continue) => {}
                    }
                    iter += 1;
                    if iter > LOOP_LIMIT {
                        let s = cond.span();
                        return Err(RuntimeError {
                            message: format!("loop limit exceeded ({LOOP_LIMIT} iterations)"),
                            line: s.line,
                            col: s.col,
                        });
                    }
                }
                Ok(None)
            }
            Stmt::For { var, iter, body, .. } => self.exec_for(var, iter, body),
            Stmt::Fn { name, params, body, .. } => {
                let cur = self.current_module();
                if let Some(m) = self.modules.get_mut(&cur) {
                    m.funcs.insert(
                        name.clone(),
                        Function { params: params.clone(), body: body.clone() },
                    );
                }
                Ok(None)
            }
            Stmt::Import { module, alias, span } => {
                self.load_module(module, span.line, span.col)?;
                let bind = alias.clone().unwrap_or_else(|| module.clone());
                self.assign(&bind, Value::Module(module.clone()))?;
                Ok(None)
            }
            Stmt::Parallel { tasks, span } => self.exec_parallel(tasks, *span),
            Stmt::FromImport { module, names, span } => {
                self.load_module(module, span.line, span.col)?;
                for (name, alias) in names {
                    let bind = alias.clone().unwrap_or_else(|| name.clone());
                    // A type imports as an alias, not a value: types are
                    // constructed, never held, so there is nothing to bind.
                    if self
                        .modules
                        .get(module)
                        .and_then(|m| m.types.get(name))
                        .is_some()
                    {
                        let cur = self.current_module();
                        self.type_alias.insert(
                            (cur, bind),
                            (module.clone(), (*name).clone()),
                        );
                        continue;
                    }
                    let v = self.module_member(module, name, span.line, span.col)?;
                    self.assign(&bind, v)?;
                }
                Ok(None)
            }
            Stmt::Return { values, .. } => self.exec_return(values),
            Stmt::Break { span } => {
                let _ = span;
                Ok(Some(Flow::Break))
            }
            Stmt::Continue { span } => {
                let _ = span;
                Ok(Some(Flow::Continue))
            }
            Stmt::Expr(e) => {
                self.eval_expr(e)?;
                Ok(None)
            }
        }
    }

/// Build the value a `return` produces.
    ///
    /// No values is `None`. One value is itself, not a one-element list, so
    /// `return f(x)` and `return x` agree. Several values become a list,
    /// which is what the caller's `a, b = f()` destructures.
    fn exec_return(&mut self, values: &[Expr]) -> Result<Option<Flow>, RuntimeError> {
        let v = match values.len() {
            0 => Value::None,
            1 => self.eval_expr(&values[0])?,
            _ => {
                let mut parts = Vec::with_capacity(values.len());
                for e in values {
                    parts.push(self.eval_expr(e)?);
                }
                Value::List(parts)
            }
        };
        Ok(Some(Flow::Return(v)))
    }

    /// Build the error a failed `assert` raises. Its own function so the
    /// formatting machinery stays out of `exec_stmt`'s frame.
    #[cold]
    #[inline(never)]
    fn assertion_failed(
        &mut self,
        message: &Option<Expr>,
        span: nx_ast::Span,
    ) -> RuntimeError {
        let detail = match message {
            Some(m) => match self.eval_expr(m) {
                Ok(v) => format!(": {v}"),
                Err(e) => return e,
            },
            None => String::new(),
        };
        RuntimeError {
            message: format!("assertion failed{detail}"),
            line: span.line,
            col: span.col,
        }
    }

    /// `a = v`, `a, b = ...`, and `a, b = f()`.
    ///
    /// Split out of `exec_stmt` so that function's stack frame stays
    /// small: a single frame is shared by every arm, so a bulky arm costs
    /// stack on every recursive call rather than only its own.
    #[inline(never)]
    fn exec_assign(
        &mut self,
        targets: &[nx_ast::Target],
        values: &[Expr],
        span: nx_ast::Span,
    ) -> Result<(), RuntimeError> {
        // Several targets against one value destructures: the value is a
        // tuple or multiple return, carried as a list.
        if targets.len() > 1 && values.len() == 1 {
            let v = self.eval_expr(&values[0])?;
            let parts = match v {
                Value::List(items) => items,
                other => {
                    return Err(RuntimeError {
                        message: format!("cannot destructure {} into {} targets", other, targets.len()),
                        line: span.line,
                        col: span.col,
                    })
                }
            };
            if parts.len() != targets.len() {
                return Err(RuntimeError {
                    message: format!(
                        "expected {} values to unpack, got {}",
                        targets.len(),
                        parts.len()
                    ),
                    line: span.line,
                    col: span.col,
                });
            }
            for (t, p) in targets.iter().zip(parts) {
                self.store_target(t, p)?;
            }
            return Ok(());
        }
        if targets.len() != values.len() {
            return Err(RuntimeError {
                message: format!("{} targets but {} values", targets.len(), values.len()),
                line: span.line,
                col: span.col,
            });
        }
        // Values are evaluated left to right before any is stored, so
        // `a, b = b, a` swaps rather than clobbering.
        let mut computed = Vec::with_capacity(values.len());
        for v in values {
            computed.push(self.eval_expr(v)?);
        }
        for (t, v) in targets.iter().zip(computed) {
            self.store_target(t, v)?;
        }
        Ok(())
    }

    /// `del a`, `del a[i]`, `del d[k]`, `del p.x`.
    #[inline(never)]
    fn exec_del(
        &mut self,
        targets: &[nx_ast::Target],
        span: nx_ast::Span,
    ) -> Result<(), RuntimeError> {
        for t in targets {
            match t {
                nx_ast::Target::Name(name) => {
                    if self.lookup(name).is_none() {
                        return Err(RuntimeError {
                            message: format!("undefined variable '{name}'"),
                            line: span.line,
                            col: span.col,
                        });
                    }
                    self.unbind(name)?;
                }
                nx_ast::Target::Index { base, index } => {
                    let kv = self.eval_expr(index)?;
                    // A dict is keyed by value, so it is matched before
                    // the index has to be an Int.
                    if let nx_ast::Expr::Var(name, _) = base.as_ref() {
                        if let Some(entries) = self.lookup_mut_dict(name) {
                            let before = entries.len();
                            entries.retain(|(k, _)| !values_equal(k, &kv));
                            if entries.len() == before {
                                return Err(RuntimeError {
                                    message: format!("key {kv} not found"),
                                    line: span.line,
                                    col: span.col,
                                });
                            }
                            continue;
                        }
                    }
                    let b = self.eval_expr(base)?;
                    if let Value::Dict(entries) = b {
                        let before = entries.len();
                        let kept: Vec<(Value, Value)> = entries
                            .into_iter()
                            .filter(|(k, _)| !values_equal(k, &kv))
                            .collect();
                        if kept.len() == before {
                            return Err(RuntimeError {
                                message: format!("key {kv} not found"),
                                line: span.line,
                                col: span.col,
                            });
                        }
                        self.store_expr(base, Value::Dict(kept))?;
                        continue;
                    }
                    let i = Self::as_index(kv, span)?;
                    match b {
                        Value::List(mut items) => {
                            let n = items.len() as i64;
                            let at = if i < 0 { i + n } else { i };
                            if at < 0 || at >= n {
                                return Err(RuntimeError {
                                    message: format!("index {i} out of range (len {n})"),
                                    line: span.line,
                                    col: span.col,
                                });
                            }
                            items.remove(at as usize);
                            self.store_expr(base, Value::List(items))?;
                        }
                        other => {
                            return Err(RuntimeError {
                                message: format!("cannot delete an index of {other}"),
                                line: span.line,
                                col: span.col,
                            })
                        }
                    }
                }
                nx_ast::Target::Attr { base, field } => {
                    // A record's arity is fixed, so removing a value means
                    // blanking the field rather than changing the layout.
                    let t = nx_ast::Target::Attr {
                        base: base.clone(),
                        field: field.clone(),
                    };
                    self.store_target(&t, Value::None)?;
                }
            }
        }
        Ok(())
    }
    fn exec_for(
        &mut self,
        var: &str,
        iter: &nx_ast::ForIter,
        body: &[Stmt],
    ) -> Result<Option<Flow>, RuntimeError> {
        match iter {
            nx_ast::ForIter::Range { start, end } => {
                let s = self.eval_expr(start)?;
                let e = self.eval_expr(end)?;
                let (a, b) = match (s, e) {
                    (Value::Int(a), Value::Int(b)) => (a, b),
                    _ => {
                        let sp = start.span();
                        return Err(RuntimeError {
                            message: "for range must be integers".to_string(),
                            line: sp.line,
                            col: sp.col,
                        });
                    }
                };
                let mut count: u64 = 0;
                let step: i64 = if a <= b { 1 } else { -1 };
                let mut cur = a;
                while (step > 0 && cur < b) || (step < 0 && cur > b) {
                    // Loop vars always shadow: task-private, never an
                    // outer-local write.
                    if let Some(top) = self.frames.last_mut() {
                        top.vars.insert(var.to_string(), Value::Int(cur));
                    } else {
                        self.assign(var, Value::Int(cur))?;
                    }
                    match self.exec_block(body)? {
                        None => {}
                        Some(Flow::Return(v)) => return Ok(Some(Flow::Return(v))),
                        Some(Flow::Break) => break,
                        Some(Flow::Continue) => {}
                    }
                    cur += step;
                    count += 1;
                    if count > LOOP_LIMIT {
                        let sp = start.span();
                        return Err(RuntimeError {
                            message: format!("loop limit exceeded ({LOOP_LIMIT} iterations)"),
                            line: sp.line,
                            col: sp.col,
                        });
                    }
                }
                Ok(None)
            }
            nx_ast::ForIter::Each(expr) => {
                let v = self.eval_expr(expr)?;
                let items: Vec<Value> = match v {
                    Value::List(items) => items,
                    Value::Str(s) => s.chars().map(|c| Value::Str(c.to_string())).collect(),
                    // Iterating a dict yields its keys, in insertion order,
                    // which is what makes a loop over one reproducible.
                    Value::Dict(entries) => entries.into_iter().map(|(k, _)| k).collect(),
                    other => {
                        let sp = expr.span();
                        return Err(RuntimeError {
                            message: format!(
                                "for loop only supports ranges, lists, strings and dicts, found {other}"
                            ),
                            line: sp.line,
                            col: sp.col,
                        });
                    }
                };
                let mut count: u64 = 0;
                for item in items {
                    if let Some(top) = self.frames.last_mut() {
                        top.vars.insert(var.to_string(), item);
                    } else {
                        self.assign(var, item)?;
                    }
                    match self.exec_block(body)? {
                        None => {}
                        Some(Flow::Return(v)) => return Ok(Some(Flow::Return(v))),
                        Some(Flow::Break) => break,
                        Some(Flow::Continue) => {}
                    }
                    count += 1;
                    if count > LOOP_LIMIT {
                        let sp = expr.span();
                        return Err(RuntimeError {
                            message: format!("loop limit exceeded ({LOOP_LIMIT} iterations)"),
                            line: sp.line,
                            col: sp.col,
                        });
                    }
                }
                Ok(None)
            }
        }
    }

    fn current_module(&self) -> String {
        self.frames
            .last()
            .map(|f| f.module.clone())
            .unwrap_or_else(|| self.current.clone())
    }

    fn assign(&mut self, name: &str, v: Value) -> Result<(), RuntimeError> {
        // Inside a parallel task, snapshot names are read-only views:
        // assigning them would be a cross-thread lost update.
        if let Some(outer) = &self.outer {
            if outer.contains(name) {
                return Err(RuntimeError {
                    message: format!("cannot assign to outer local '{name}' inside parallel (use a module global)"),
                    line: 1,
                    col: 1,
                });
            }
        }
        if let Some(top) = self.frames.last_mut() {
            if self.task_top && !top.vars.contains_key(name) {
                // Top-level task: mirror sequential semantics (module global).
            } else {
                top.vars.insert(name.to_string(), v);
                return Ok(());
            }
        }
        let cur = self.current.clone();
        if let Some(m) = self.modules.get_mut(&cur) {
            m.vars.insert(name.to_string(), v);
            self.touched.insert((cur, name.to_string()));
        }
        Ok(())
    }

    /// Write through an assignment target.
///
/// A name is an ordinary bind. An index is the interesting case: it has to
/// mutate the container *in place* when the container is a plain variable,
/// or the write would land on a temporary copy and be lost. When the base
/// is not a simple variable (`f()[0] = v`) the modified copy is written
/// back through the base, which is the only coherent thing to do.
fn store_target(&mut self, target: &nx_ast::Target, v: Value) -> Result<(), RuntimeError> {
    match target {
        nx_ast::Target::Name(name) => self.assign(name, v),
        nx_ast::Target::Index { base, index } => {
            let span = index.span();
            let kv = self.eval_expr(index)?;
            // A dict is keyed by value, so it is matched before the index
            // is required to be an Int.
            if let nx_ast::Expr::Var(name, _) = base.as_ref() {
                if let Some(entries) = self.lookup_mut_dict(name) {
                    let updated = dict_set(entries.clone(), kv, v);
                    *entries = updated;
                    return Ok(());
                }
            }
            let b = self.eval_expr(base)?;
            if let Value::Dict(entries) = b {
                let updated = dict_set(entries, kv, v);
                return self.store_expr(base, Value::Dict(updated));
            }
            let i = Self::as_index(kv, span)?;
            if let nx_ast::Expr::Var(name, _) = base.as_ref() {
                if let Some(items) = self.lookup_mut_list(name) {
                    let n = items.len() as i64;
                    let at = if i < 0 { i + n } else { i };
                    if at < 0 || at >= n {
                        return Err(RuntimeError {
                            message: format!("index {i} out of range (len {n})"),
                            line: span.line,
                            col: span.col,
                        });
                    }
                    items[at as usize] = v;
                    return Ok(());
                }
            }
            match self.eval_expr(base)? {
                Value::List(mut items) => {
                    let n = items.len() as i64;
                    let at = if i < 0 { i + n } else { i };
                    if at < 0 || at >= n {
                        return Err(RuntimeError {
                            message: format!("index {i} out of range (len {n})"),
                            line: span.line,
                            col: span.col,
                        });
                    }
                    items[at as usize] = v;
                    self.store_expr(base, Value::List(items))
                }
                other => Err(RuntimeError {
                    message: format!("cannot index-assign into {other}"),
                    line: span.line,
                    col: span.col,
                }),
            }
        }
        nx_ast::Target::Attr { base, field } => {
            // Shared with `store_expr`'s attribute case: evaluate the
            // holder, set the field, and write the holder back. Nesting
            // (`n.at.y = v`) recurses through the same path.
            let span = base.span();
            let e = Expr::Attr { base: base.clone(), attr: field.clone(), span };
            self.store_expr(&e, v)
        }
    }
}

/// Write a value back through a bare expression used as the base of an
/// index assignment. Only a variable or a further index can be written
/// through; anything else has no storage to write to.
fn store_expr(&mut self, e: &Expr, v: Value) -> Result<(), RuntimeError> {
    match e {
        Expr::Var(name, _) => self.assign(name, v),
        Expr::Index { base, index, span } => {
            let ispan = index.span();
            let i = Self::as_index(self.eval_expr(index)?, ispan)?;
            if let Expr::Var(name, _) = base.as_ref() {
                if let Some(items) = self.lookup_mut_list(name) {
                    let n = items.len() as i64;
                    let at = if i < 0 { i + n } else { i };
                    if at < 0 || at >= n {
                        return Err(RuntimeError {
                            message: format!("index {i} out of range (len {n})"),
                            line: ispan.line,
                            col: ispan.col,
                        });
                    }
                    items[at as usize] = v;
                    return Ok(());
                }
            }
            match self.eval_expr(base)? {
                Value::List(mut items) => {
                    let n = items.len() as i64;
                    let at = if i < 0 { i + n } else { i };
                    if at < 0 || at >= n {
                        return Err(RuntimeError {
                            message: format!("index {i} out of range (len {n})"),
                            line: ispan.line,
                            col: ispan.col,
                        });
                    }
                    items[at as usize] = v;
                    self.store_expr(base, Value::List(items))
                }
                other => Err(RuntimeError {
                    message: format!("cannot index-assign into {other}"),
                    line: span.line,
                    col: span.col,
                }),
            }
        }
        // `n.at.y = v`: evaluate the holder, set the field on the copy,
        // and write the holder back through the same path recursively.
        // Copies compose, so each level stays independent.
        Expr::Attr { base, attr, span } => {
            let b = self.eval_expr(base)?;
            let (module, type_name, fields) = match b {
                Value::Record { module, type_name, fields } => (module, type_name, fields),
                other => {
                    return Err(RuntimeError {
                        message: format!("only types have fields, found {other}"),
                        line: span.line,
                        col: span.col,
                    })
                }
            };
            let layout = self.type_fields(&module, &type_name);
            match layout.iter().position(|n| n == attr) {
                Some(i) if i < fields.len() => {
                    let mut updated = fields;
                    updated[i] = v;
                    self.store_expr(
                        base,
                        Value::Record { module, type_name, fields: updated },
                    )
                }
                _ => Err(RuntimeError {
                    message: format!("type '{type_name}' has no field '{attr}'"),
                    line: span.line,
                    col: span.col,
                }),
            }
        }
        other => Err(RuntimeError {
            message: "cannot assign into this expression".to_string(),
            line: other.span().line,
            col: other.span().col,
        }),
    }
}

/// Remove a binding from the innermost frame that holds it, falling back to
/// the module's globals. Used by `del`.
fn unbind(&mut self, name: &str) -> Result<(), RuntimeError> {
    if let Some(top) = self.frames.last_mut() {
        if top.vars.remove(name).is_some() {
            return Ok(());
        }
    }
    let cur = self.current.clone();
    if let Some(m) = self.modules.get_mut(&cur) {
        if m.vars.remove(name).is_some() {
            self.touched.insert((cur, name.to_string()));
            return Ok(());
        }
    }
    Ok(())
}

/// Borrow a named dict in place so a keyed write can mutate it.
fn lookup_mut_dict(&mut self, name: &str) -> Option<&mut Vec<(Value, Value)>> {
    if let Some(top) = self.frames.last_mut() {
        if top.vars.contains_key(name) {
            return match top.vars.get_mut(name) {
                Some(Value::Dict(e)) => Some(e),
                _ => None,
            };
        }
        let mname = top.module.clone();
        if let Some(m) = self.modules.get_mut(&mname) {
            return match m.vars.get_mut(name) {
                Some(Value::Dict(e)) => Some(e),
                _ => None,
            };
        }
        return None;
    }
    let cur = self.current.clone();
    if let Some(m) = self.modules.get_mut(&cur) {
        return match m.vars.get_mut(name) {
            Some(Value::Dict(e)) => Some(e),
            _ => None,
        };
    }
    None
}

/// Borrow a named list in place so an indexed write can mutate it.
/// Returns `None` if the name is not bound or is not a list.
fn lookup_mut_list(&mut self, name: &str) -> Option<&mut Vec<Value>> {
    // The frame's own bindings shadow the module's, so they are tried
    // first. Each borrow is confined to its own block so the frame and the
    // module maps are not both held at once.
    if let Some(top) = self.frames.last_mut() {
        if top.vars.contains_key(name) {
            return match top.vars.get_mut(name) {
                Some(Value::List(items)) => Some(items),
                _ => None,
            };
        }
        let mname = top.module.clone();
        if let Some(m) = self.modules.get_mut(&mname) {
            return match m.vars.get_mut(name) {
                Some(Value::List(items)) => Some(items),
                _ => None,
            };
        }
        return None;
    }
    let cur = self.current.clone();
    if let Some(m) = self.modules.get_mut(&cur) {
        return match m.vars.get_mut(name) {
            Some(Value::List(items)) => Some(items),
            _ => None,
        };
    }
    None
}

/// Normalise an index value: must be Int, and negative indices count from
/// the end. Returns the raw (possibly negative) index; callers that have a
/// length in hand do the wrap.
fn as_index(v: Value, span: nx_ast::Span) -> Result<i64, RuntimeError> {
    match v {
        Value::Int(i) => Ok(i),
        other => Err(RuntimeError {
            message: format!("index must be Int, found {other}"),
            line: span.line,
            col: span.col,
        }),
    }
}

/// Field names of a declared type. The declaring module is tried first,
    /// so two modules declaring the same type name never collide; the
    /// global fallback covers values built before an alias was registered.
    fn type_fields(&self, module: &str, type_name: &str) -> Vec<String> {
        if let Some(m) = self.modules.get(module) {
            if let Some(f) = m.types.get(type_name) {
                return f.clone();
            }
        }
        for m in self.modules.values() {
            if let Some(f) = m.types.get(type_name) {
                return f.clone();
            }
        }
        Vec::new()
    }

    fn lookup(&self, name: &str) -> Option<Value> {
        // Own call frame first, then the defining module's globals.
        // No dynamic fallback into caller frames (v0 scoping rule).
        if let Some(top) = self.frames.last() {
            if let Some(v) = top.vars.get(name) {
                return Some(v.clone());
            }
            if let Some(m) = self.modules.get(&top.module) {
                if let Some(v) = m.vars.get(name) {
                    return Some(v.clone());
                }
            }
            return None;
        }
        self.modules
            .get(&self.current)
            .and_then(|m| m.vars.get(name))
            .cloned()
    }

    fn module_member(
        &self,
        module: &str,
        name: &str,
        line: nx_ast::LineNo,
        col: nx_ast::ColNo,
    ) -> Result<Value, RuntimeError> {
        let m = self.modules.get(module).ok_or(RuntimeError {
            message: format!("unknown module '{module}'"),
            line,
            col,
        })?;
        if let Some(v) = m.vars.get(name) {
            return Ok(v.clone());
        }
        if m.funcs.contains_key(name) {
            return Ok(Value::Func { module: module.to_string(), name: name.to_string() });
        }
        // A type is constructed, never held, so using one as a value names
        // the fix instead of just failing the lookup.
        if m.types.contains_key(name) {
            return Err(RuntimeError {
                message: format!("type '{module}.{name}' cannot be used as a value; construct it"),
                line,
                col,
            });
        }
        Err(RuntimeError {
            message: format!("module '{module}' has no member '{name}'"),
            line,
            col,
        })
    }

    fn resolve_module(&self, importer: &str, name: &str) -> Option<std::path::PathBuf> {
        let file = format!("{name}.nx");
        let mut dirs = Vec::new();
        if let Some(m) = self.modules.get(importer) {
            dirs.push(m.dir.clone());
        }
        if let Ok(nx_path) = std::env::var("NX_PATH") {
            dirs.extend(std::env::split_paths(&nx_path));
        }
        if let Ok(cwd) = std::env::current_dir() {
            dirs.push(cwd);
        }
        dirs.into_iter()
            .map(|d| d.join(&file))
            .find(|p| p.is_file())
    }

    fn load_module(&mut self, name: &str, line: nx_ast::LineNo, col: nx_ast::ColNo) -> Result<(), RuntimeError> {
        if self.loading.contains(&name.to_string()) {
            return Err(RuntimeError {
                message: format!("circular import of '{name}'"),
                line,
                col,
            });
        }
        if self.modules.contains_key(name) {
            return Ok(());
        }
        let importer = self.current_module();
        let path = self.resolve_module(&importer, name).ok_or(RuntimeError {
            message: format!("cannot find module '{name}.nx'"),
            line,
            col,
        })?;
        let source = std::fs::read_to_string(&path).map_err(|e| RuntimeError {
            message: format!("cannot read module '{name}': {e}"),
            line,
            col,
        })?;
        let tokens = nx_lexer::lex(&source).map_err(|e| RuntimeError {
            message: format!("in module '{name}': {e}"),
            line,
            col,
        })?;
        let prog = nx_parser::parse(tokens).map_err(|e| RuntimeError {
            message: format!("in module '{name}': {e}"),
            line,
            col,
        })?;
        let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or(".".into());
        self.modules.insert(name.to_string(), Module { dir, ..Default::default() });
        self.programs.insert(name.to_string(), prog.clone());
        self.loading.push(name.to_string());
        // Top-level module code runs with no call frames so its bindings
        // land in the module table, not in some caller's locals.
        let saved_frames = std::mem::take(&mut self.frames);
        let saved_current = std::mem::replace(&mut self.current, name.to_string());
        let ret = self.exec_block(&prog.stmts);
        self.current = saved_current;
        self.frames = saved_frames;
        self.loading.pop();
        match ret {
            Ok(None) => Ok(()),
            Ok(Some(_)) => Err(RuntimeError {
                message: format!("module '{name}' cannot break/continue/return at top level"),
                line,
                col,
            }),
            Err(e) => Err(e),
        }
    }

    fn exec_parallel(&mut self, tasks: &[Stmt], span: nx_ast::Span) -> Result<Option<Flow>, RuntimeError> {
        if tasks.is_empty() {
            return Ok(None);
        }
        let cur = self.current_module();
        // Enclosing frame keys are task-private reads; anything else shared.
        let mut locals: HashSet<String> = HashSet::new();
        if let Some(top) = self.frames.last() {
            locals.extend(top.vars.keys().cloned());
        }
        let sums = nx_ir::task_summaries(&self.programs, &cur, &locals, tasks).map_err(|e| {
            RuntimeError { message: e.message, line: span.line, col: span.col }
        })?;
        // Imports inside tasks run up-front (single-threaded) so every
        // worker finds them cached in its private clone.
        let mut mods = Vec::new();
        for t in tasks {
            collect_import_names(t, &mut mods);
        }
        for m in &mods {
            self.load_module(m, span.line, span.col)?;
        }
        let outer: HashSet<String> = locals.clone();
        let snapshot: HashMap<String, Value> = self
            .frames
            .last()
            .map(|f| f.vars.clone())
            .unwrap_or_default();
        for batch in nx_ir::partition(&sums) {
            if batch.len() == 1 {
                // Fast path: still isolated (uniform semantics), no threads.
                let mut worker = self.spawn_worker(&snapshot, &outer, &cur);
                match worker.exec_stmt(&tasks[batch[0]])? {
                    None => {}
                    Some(Flow::Return(_)) => {
                        return Err(RuntimeError {
                            message: "return inside parallel task is not supported".to_string(),
                            line: span.line,
                            col: span.col,
                        });
                    }
                    Some(_) => {
                        return Err(RuntimeError {
                            message: "break/continue cannot cross a parallel boundary".to_string(),
                            line: span.line,
                            col: span.col,
                        });
                    }
                }
                self.merge_worker(worker);
                continue;
            }
            let mut results = Vec::new();
            let mut spawn_err: Option<RuntimeError> = None;
            std::thread::scope(|s| {
                let mut handles = Vec::new();
                for &i in &batch {
                    let mut worker = self.spawn_worker(&snapshot, &outer, &cur);
                    let stmt = tasks[i].clone();
                    // Same roomy stack as the main run thread: deep
                    // (memoized) recursion must not overflow workers.
                    match std::thread::Builder::new()
                        .name("nx-task".to_string())
                        .stack_size(64 * 1024 * 1024)
                        .spawn_scoped(s, move || {
                            let r = worker.exec_stmt(&stmt);
                            (worker, r)
                        }) {
                        Ok(h) => handles.push(h),
                        Err(_) => {
                            spawn_err = Some(RuntimeError {
                                message: "cannot spawn parallel task".to_string(),
                                line: span.line,
                                col: span.col,
                            });
                            return;
                        }
                    }
                }
                for h in handles {
                    results.push(h.join());
                }
            });
            if let Some(e) = spawn_err {
                return Err(e);
            }
            for r in results {
                let (worker, r) = r.map_err(|_| RuntimeError {
                    message: "parallel task crashed".to_string(),
                    line: span.line,
                    col: span.col,
                })?;
                match r {
                    Err(e) => return Err(e),
                    Ok(None) => {}
                    Ok(Some(Flow::Return(_))) => {
                        return Err(RuntimeError {
                            message: "return inside parallel task is not supported".to_string(),
                            line: span.line,
                            col: span.col,
                        });
                    }
                    Ok(Some(_)) => {
                        return Err(RuntimeError {
                            message: "break/continue cannot cross a parallel boundary".to_string(),
                            line: span.line,
                            col: span.col,
                        });
                    }
                }
                self.merge_worker(worker);
            }
        }
        Ok(None)
    }

    fn spawn_worker(
        &self,
        snapshot: &HashMap<String, Value>,
        outer: &HashSet<String>,
        module: &str,
    ) -> Interpreter {
        Interpreter {
            frames: vec![Frame { vars: snapshot.clone(), module: module.to_string() }],
            modules: self.modules.clone(),
            programs: self.programs.clone(),
            loading: self.loading.clone(),
            current: module.to_string(),
            output: Vec::new(),
            call_depth: self.call_depth,
            touched: HashSet::new(),
            outer: Some(outer.clone()),
            task_top: self.frames.is_empty(),
            memo_ok: self.memo_ok.clone(),
            memo_seen: self.memo_seen.clone(),
            memo: self.memo.clone(),
            type_alias: self.type_alias.clone(),
            methods: self.methods.clone(),
        }
    }

    fn merge_worker(&mut self, worker: Interpreter) {
        // Disjoint by construction (partitioning); merge touched globals.
        for (m, k) in &worker.touched {
            if let (Some(src), Some(dst)) = (
                worker.modules.get(m).and_then(|x| x.vars.get(k)),
                self.modules.get_mut(m),
            ) {
                dst.vars.insert(k.clone(), src.clone());
            }
        }
        self.output.extend(worker.output);
    }

    fn expect_bool(v: Value, span: nx_ast::Span) -> Result<bool, RuntimeError> {
        match v {
            Value::Bool(b) => Ok(b),
            _ => Err(RuntimeError {
                message: "condition must be a boolean".to_string(),
                line: span.line,
                col: span.col,
            }),
        }
    }

    fn eval_expr(&mut self, expr: &Expr) -> Result<Value, RuntimeError> {
        match expr {
            Expr::Int(i, _) => Ok(Value::Int(*i)),
            Expr::Float(x, _) => Ok(Value::Float(*x)),
            Expr::Bool(b, _) => Ok(Value::Bool(*b)),
            Expr::Str(s, _) => Ok(Value::Str(s.clone())),
            Expr::NoneLit(_) => Ok(Value::None),
            Expr::Range { start, end, .. } => self.eval_range(start, end),
            Expr::Dict(pairs, _) => self.eval_dict(pairs),
            Expr::Slice { base, from, to, step, .. } => {
                self.eval_slice(base, from, to, step)
            }
            Expr::IfExpr { cond, then_value, else_value, .. } => {
                self.eval_if_expr(cond, then_value, else_value)
            }
            Expr::Comprehension { element, var, iter, cond, .. } => {
                self.eval_comprehension(element, var, iter, cond)
            }
            Expr::List(items, _) => {
                let mut vs = Vec::with_capacity(items.len());
                for it in items {
                    vs.push(self.eval_expr(it)?);
                }
                Ok(Value::List(vs))
            }
            Expr::Var(name, span) => self.lookup(name).or_else(|| {
                // Bare function names double as first-class references.
                let cur = self.current_module();
                self.modules
                    .get(&cur)
                    .filter(|m| m.funcs.contains_key(name))
                    .map(|_| Value::Func { module: cur, name: name.clone() })
            }).ok_or(RuntimeError {
                message: format!("undefined variable '{name}'"),
                line: span.line,
                col: span.col,
            }),
            Expr::Index { base, index, span } => {
                let b = self.eval_expr(base)?;
                let ix = self.eval_expr(index)?;
                self.eval_index(b, ix, span.line, span.col)
            }
            Expr::Unary { op, expr, span } => {
                let v = self.eval_expr(expr)?;
                match (op, v) {
                    (UnaryOp::Neg, Value::Int(i)) => i.checked_neg().map(Value::Int).ok_or(RuntimeError {
                        message: "integer overflow".to_string(),
                        line: span.line,
                        col: span.col,
                    }),
                    (UnaryOp::Neg, Value::Float(x)) => Ok(Value::Float(-x)),
                    (UnaryOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (UnaryOp::BitNot, Value::Int(i)) => Ok(Value::Int(!i)),
                    (UnaryOp::Pos, v) => Ok(v),
                    _ => Err(RuntimeError {
                        message: format!("operator '{}' type mismatch", op.as_str()),
                        line: span.line,
                        col: span.col,
                    }),
                }
            }
            Expr::Binary { left, op, right, span } => {
                match op {
                    BinOp::And => {
                        let lv = self.eval_expr(left)?;
                        let l = Self::expect_bool(lv, left.span())?;
                        if !l {
                            return Ok(Value::Bool(false));
                        }
                        let rv = self.eval_expr(right)?;
                        let r = Self::expect_bool(rv, right.span())?;
                        return Ok(Value::Bool(r));
                    }
                    BinOp::Or => {
                        let lv = self.eval_expr(left)?;
                        let l = Self::expect_bool(lv, left.span())?;
                        if l {
                            return Ok(Value::Bool(true));
                        }
                        let rv = self.eval_expr(right)?;
                        let r = Self::expect_bool(rv, right.span())?;
                        return Ok(Value::Bool(r));
                    }
                    _ => {}
                }
                let l = self.eval_expr(left)?;
                let r = self.eval_expr(right)?;
                self.apply_binop(l, *op, r, span.line, span.col)
            }
            Expr::Attr { base, attr, span } => self.eval_attr(base, attr, *span),
            Expr::Call { callee, args, span } => self.eval_call(callee, args, *span),
        }
    }

    /// `a.b`: a module member or a record field. Split out of `eval_expr`
    /// to keep that function's frame small; see the note below.
    #[inline(never)]
    fn eval_attr(
        &mut self,
        base: &Expr,
        attr: &str,
        span: nx_ast::Span,
    ) -> Result<Value, RuntimeError> {
        let b = self.eval_expr(base)?;
        match b {
            Value::Module(m) => self.module_member(&m, attr, span.line, span.col),
            // The field name resolves against the layout of the value's own
            // type, so a record keeps working after it has moved.
            Value::Record { module, type_name, fields } => {
                let layout = self.type_fields(&module, &type_name);
                match layout.iter().position(|n| n == attr) {
                    Some(i) if i < fields.len() => Ok(fields[i].clone()),
                    _ => Err(RuntimeError {
                        message: format!("type '{type_name}' has no field '{attr}'"),
                        line: span.line,
                        col: span.col,
                    }),
                }
            }
            other => Err(RuntimeError {
                message: format!("only modules and types have attributes, found {other}"),
                line: span.line,
                col: span.col,
            }),
        }
    }

    /// `start..end` -- ascending, half-open, materialised as a list.
    ///
    /// The container-shaped arms of `eval_expr` are all extracted into
    /// their own functions. `eval_expr` compiles to a single stack frame
    /// holding every arm's locals, so a bulky arm costs stack on every
    /// recursive call rather than only its own -- and this interpreter
    /// recurses once per call frame.
    #[inline(never)]
    fn eval_range(
        &mut self,
        start: &Expr,
        end: &Expr,
    ) -> Result<Value, RuntimeError> {
        let s = self.eval_expr(start)?;
        let e = self.eval_expr(end)?;
        let (Value::Int(a), Value::Int(b)) = (s, e) else {
            return Err(RuntimeError {
                message: "range bounds must be Int".to_string(),
                line: start.span().line,
                col: start.span().col,
            });
        };
        // An empty or inverted range is an empty list, so the count never
        // goes negative.
        let mut items = Vec::new();
        let mut i = a;
        while i < b {
            items.push(Value::Int(i));
            i += 1;
        }
        Ok(Value::List(items))
    }

    /// `{k: v, ...}`. A later duplicate of a key overwrites the value but
    /// keeps the original position, so iteration order stays stable.
    #[inline(never)]
    fn eval_dict(&mut self, pairs: &[(Expr, Expr)]) -> Result<Value, RuntimeError> {
        let mut entries: Vec<(Value, Value)> = Vec::with_capacity(pairs.len());
        for (k, v) in pairs {
            let key = self.eval_expr(k)?;
            let val = self.eval_expr(v)?;
            match entries.iter_mut().find(|(ek, _)| values_equal(ek, &key)) {
                Some(slot) => slot.1 = val,
                None => entries.push((key, val)),
            }
        }
        Ok(Value::Dict(entries))
    }

    /// `base[from:to:step]`. Copies rather than aliases: a view would make
    /// later mutation of either side surprising.
    #[inline(never)]
    fn eval_slice(
        &mut self,
        base: &Expr,
        from: &Option<Box<Expr>>,
        to: &Option<Box<Expr>>,
        step: &Option<Box<Expr>>,
    ) -> Result<Value, RuntimeError> {
        let span = base.span();
        let b = self.eval_expr(base)?;
        let items = match &b {
            Value::List(items) => items.clone(),
            Value::Str(s) => s.chars().map(|c| Value::Str(c.to_string())).collect(),
            other => {
                return Err(RuntimeError {
                    message: format!("cannot slice {other}"),
                    line: span.line,
                    col: span.col,
                })
            }
        };
        let n = items.len() as i64;
        let norm = |v: Option<i64>| -> i64 {
            match v {
                None => 0,
                Some(x) if x < 0 => (x + n).max(0),
                Some(x) => x.min(n),
            }
        };
        let read = |me: &mut Self, e: &Option<Box<Expr>>, absent: i64| -> Result<i64, RuntimeError> {
            match e {
                Some(x) => {
                    let v = me.eval_expr(x)?;
                    Self::as_index(v, x.span())
                }
                None => Ok(absent),
            }
        };
        // An absent `from` means zero and an absent `to` means the length,
        // which is what `norm` then clamps.
        let start = norm(Some(read(self, from, 0)?));
        let stop = match to {
            None => n,
            Some(e) => {
                let raw = read(self, &Some(e.clone()), 0)?;
                if raw < 0 {
                    (raw + n).max(0)
                } else {
                    raw.min(n)
                }
            }
        };
        let stride = read(self, step, 1)?;
        if stride <= 0 {
            return Err(RuntimeError {
                message: format!("slice step must be positive, found {stride}"),
                line: span.line,
                col: span.col,
            });
        }
        let mut out = Vec::new();
        let mut i = start;
        while i < stop {
            out.push(items[i as usize].clone());
            i += stride;
        }
        // Slicing a string gives a string, not a list of characters.
        if matches!(b, Value::Str(_)) {
            let s: String = out
                .iter()
                .map(|v| match v {
                    Value::Str(s) => s.clone(),
                    _ => String::new(),
                })
                .collect();
            return Ok(Value::Str(s));
        }
        Ok(Value::List(out))
    }

    /// `a if cond else b`. Only the taken branch is evaluated, so this is
    /// safe for guarding an operation that would otherwise fail.
    #[inline(never)]
    fn eval_if_expr(
        &mut self,
        cond: &Expr,
        then_value: &Expr,
        else_value: &Expr,
    ) -> Result<Value, RuntimeError> {
        let c = self.eval_expr(cond)?;
        if Self::expect_bool(c, cond.span())? {
            self.eval_expr(then_value)
        } else {
            self.eval_expr(else_value)
        }
    }

    /// `[expr for var in iter if cond]`. The loop variable is saved and
    /// restored, so a comprehension neither leaks its binding nor clobbers
    /// an outer variable of the same name.
    #[inline(never)]
    fn eval_comprehension(
        &mut self,
        element: &Expr,
        var: &str,
        iter: &Expr,
        cond: &Option<Box<Expr>>,
    ) -> Result<Value, RuntimeError> {
        let it = self.eval_expr(iter)?;
        let items = match it {
            Value::List(items) => items,
            Value::Str(s) => s.chars().map(|c| Value::Str(c.to_string())).collect(),
            other => {
                return Err(RuntimeError {
                    message: format!("cannot iterate over {other}"),
                    line: iter.span().line,
                    col: iter.span().col,
                })
            }
        };
        let mut out = Vec::with_capacity(items.len());
        let saved = self.lookup(var);
        for item in items {
            self.assign(var, item)?;
            if let Some(c) = cond {
                let cv = self.eval_expr(c)?;
                if !Self::expect_bool(cv, c.span())? {
                    continue;
                }
            }
            out.push(self.eval_expr(element)?);
        }
        match saved {
            Some(v) => self.assign(var, v)?,
            None => self.unbind(var)?,
        }
        Ok(Value::List(out))
    }

    /// `f(...)`. Builtins, type constructors and direct calls; anything
    /// else falls through to calling a function value.
    #[inline(never)]
    fn eval_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        span: nx_ast::Span,
    ) -> Result<Value, RuntimeError> {
        if let Expr::Var(name, _) = callee {
            if name == "len" || name == "push" {
                return self.call_builtin(name, args, span.line, span.col);
            }
            let cur = self.current_module();
            // `from m import T [as U]`: the alias resolves to the
            // declaring module's layout, exactly as the checker sees it.
            if let Some((decl, real)) =
                self.type_alias.get(&(cur.clone(), name.clone())).cloned()
            {
                return self.construct(&decl, &real, args, span);
            }
            // A declared type is constructed by name, sharing the call
            // spelling with a function.
            if self.modules.get(&cur).and_then(|m| m.types.get(name)).is_some() {
                return self.construct(&cur, name, args, span);
            }
            // Plain `foo(...)`: a function in the current module.
            if self
                .modules
                .get(&cur)
                .map(|m| m.funcs.contains_key(name))
                .unwrap_or(false)
            {
                return self.call_func(&cur, name, args, span.line, span.col);
            }
        }
        // `m.T(...)` through a plain `import m`: the layout lives in the
        // imported module, not the current one.
        if let Expr::Attr { base, attr, .. } = callee {
            if let Expr::Var(m, _) = base.as_ref() {
                if let Some(Value::Module(modname)) = self.lookup(m) {
                    if self
                        .modules
                        .get(&modname)
                        .and_then(|mm| mm.types.get(attr))
                        .is_some()
                    {
                        return self.construct(&modname, attr, args, span);
                    }
                }
            }
        }
        // Attribute callees never evaluate as values first: `p.m` alone is
        // not a value (methods are called, not referenced), so resolving
        // the call directly is what keeps `p.m(...)` working.
        // Module calls and `m.T(...)` construction returned above; what
        // reaches here is methods and sugar.
        if let Expr::Attr { base, attr, .. } = callee {
            return self.eval_attr_call(base, attr, args, span);
        }
        let target = self.eval_expr(callee)?;
        match target {
            Value::Func { module, name } => {
                self.call_func(&module, &name, args, span.line, span.col)
            }
            _ => Err(RuntimeError {
                message: "not callable".to_string(),
                line: span.line,
                col: span.col,
            }),
        }
    }

    /// `base.attr(...)`: a module member, a type's associated function, an
    /// impl method on a record value, or builtin sugar. Split out of
    /// `eval_call` to keep that function's frame small; see the note there.
    ///
    /// Resolution order matches the checker exactly: modules, associated
    /// functions, methods on records, then sugar. Anything else is a
    /// runtime error naming what was actually found.
    #[inline(never)]
    fn eval_attr_call(
        &mut self,
        base: &Expr,
        attr: &str,
        args: &[Expr],
        span: nx_ast::Span,
    ) -> Result<Value, RuntimeError> {
        // A module base calls into the module, sharing the value path:
        // a function member becomes a function value, anything else fails
        // the same way a bare attribute read would.
        if let Expr::Var(m, _) = base {
            if let Some(Value::Module(modname)) = self.lookup(m) {
                let member = self.module_member(&modname, attr, span.line, span.col)?;
                if let Value::Func { module, name } = member {
                    return self.call_func(&module, &name, args, span.line, span.col);
                }
                return Err(RuntimeError {
                    message: format!("'{attr}' of module '{modname}' is not callable"),
                    line: span.line,
                    col: span.col,
                });
            }
            // `T.m(...)` where T names a type: an associated function. No
            // sugar fallback here -- the base is not a value at all.
            let cur = self.current_module();
            if let Some((decl, real)) = self.resolve_type_name(&cur, m) {
                if let Some(def) = self
                    .methods
                    .get(&(decl.clone(), real.clone()))
                    .and_then(|mm| mm.get(attr))
                    .cloned()
                {
                    if def.receiver != nx_ast::ReceiverKind::None {
                        return Err(RuntimeError {
                            message: format!(
                                "method '{attr}' needs a receiver; call it on a '{real}' value"
                            ),
                            line: span.line,
                            col: span.col,
                        });
                    }
                    return self.call_method(&def, None, args, span);
                }
                return Err(RuntimeError {
                    message: format!("type '{real}' has no associated function '{attr}'"),
                    line: span.line,
                    col: span.col,
                });
            }
        }
        let b = self.eval_expr(base)?;
        // A record value dispatches to its type's method table.
        if let Value::Record { module, type_name, .. } = &b {
            if let Some(def) = self
                .methods
                .get(&(module.clone(), type_name.clone()))
                .and_then(|mm| mm.get(attr))
                .cloned()
            {
                return self.call_method(&def, Some((base, b)), args, span);
            }
            // Records without the method fall through to sugar below, so
            // `p.keys()`-style builtin calls keep working on them only when
            // the builtin accepts a record -- otherwise the builtin's own
            // error names the mismatch.
            if nx_types::builtin_arity(attr).is_none() {
                return Err(RuntimeError {
                    message: format!("type '{type_name}' has no method '{attr}'"),
                    line: span.line,
                    col: span.col,
                });
            }
        }
        // Builtin sugar: `xs.push(1)` for `push(xs, 1)`. The base
        // expression is prepended and routed through the identical builtin
        // path as a direct call.
        if nx_types::builtin_arity(attr).is_some() {
            return self.call_builtin_sugar(attr, base, args, span);
        }
        Err(RuntimeError {
            message: format!("'{attr}' is not callable on {b}"),
            line: span.line,
            col: span.col,
        })
    }

    /// Resolve a possibly-aliased type name in a module to its declaring
    /// module and canonical name. Mirrors the checker's rule: a local
    /// declaration wins, then an import alias.
    fn resolve_type_name(&self, cur: &str, name: &str) -> Option<(String, String)> {
        if let Some(m) = self.modules.get(cur) {
            if m.types.contains_key(name) {
                return Some((cur.to_string(), name.to_string()));
            }
        }
        if let Some((decl, real)) = self.type_alias.get(&(cur.to_string(), name.to_string())) {
            return Some((decl.clone(), real.clone()));
        }
        None
    }

    /// Call a method definition. `receiver` is the base expression plus its
    /// evaluated record for methods with one; associated functions pass
    /// none. A `mut self` result is written back into the receiver target,
    /// which is what makes the mutation caller-visible.
    fn call_method(
        &mut self,
        def: &MethodDef,
        receiver: Option<(&Expr, Value)>,
        args: &[Expr],
        span: nx_ast::Span,
    ) -> Result<Value, RuntimeError> {
        if args.len() != def.params.len() {
            return Err(RuntimeError {
                message: format!(
                    "method '{}' expects {} args, got {}",
                    def.name,
                    def.params.len(),
                    args.len()
                ),
                line: span.line,
                col: span.col,
            });
        }
        let mut vals = Vec::with_capacity(args.len() + 1);
        for a in args {
            vals.push(self.eval_expr(a)?);
        }
        if self.call_depth >= CALL_LIMIT {
            return Err(RuntimeError {
                message: "call stack overflow (possible infinite recursion)".to_string(),
                line: span.line,
                col: span.col,
            });
        }
        self.call_depth += 1;
        self.frames.push(Frame { module: def.module.clone(), ..Default::default() });
        // `self` binds the receiver's (already owned) value; the rest bind
        // positionally like function parameters. The base expression is
        // borrowed, not moved: the write-back below needs it afterwards.
        let mut names: Vec<String> = Vec::with_capacity(vals.len() + 1);
        if let Some((_, rec)) = &receiver {
            names.push("self".to_string());
            if let Some(top) = self.frames.last_mut() {
                top.vars.insert("self".to_string(), rec.clone());
            }
        }
        for (p, v) in def.params.iter().zip(vals) {
            names.push(p.clone());
            if let Some(top) = self.frames.last_mut() {
                top.vars.insert(p.clone(), v);
            }
        }
        let ret = self.exec_block(&def.body)?;
        self.frames.pop();
        self.call_depth -= 1;
        let out = match ret {
            None | Some(Flow::Continue) | Some(Flow::Break) => Ok(Value::None),
            Some(Flow::Return(v)) => Ok(v),
        }?;
        // Write-back for `mut self`: the result returns into the receiver's
        // storage when the receiver has any. A temporary base -- a call
        // result, a literal -- has nowhere to write, so the call simply
        // evaluates to its result, which is what makes a chain like
        // `q.moved(1, 1).moved(2, 2)` read as one expression.
        if def.receiver == nx_ast::ReceiverKind::Mut {
            if let Some((base, _)) = receiver {
                if let Some(t) = target_of_expr(base) {
                    self.store_target(&t, out.clone())?;
                }
            }
        }
        Ok(out)
    }

    /// `xs.push(1)` for `push(xs, 1)`: prepend the base expression and
    /// route through the identical builtin path as a direct call --
    /// including the variable-target checks mutating builtins need. One
    /// implementation serves both spellings by construction.
    fn call_builtin_sugar(
        &mut self,
        name: &str,
        base: &Expr,
        args: &[Expr],
        span: nx_ast::Span,
    ) -> Result<Value, RuntimeError> {
        let mut combined: Vec<Expr> = Vec::with_capacity(args.len() + 1);
        combined.push(base.clone());
        combined.extend(args.iter().cloned());
        self.call_builtin(name, &combined, span.line, span.col)
    }

    /// Build a record of a declared type: exact arity, then positional
    /// fields. The declaring module is recorded on the value so its
    /// layout stays resolvable no matter where the value travels.
    fn construct(
        &mut self,
        module: &str,
        name: &str,
        args: &[Expr],
        span: nx_ast::Span,
    ) -> Result<Value, RuntimeError> {
        let layout = self.type_fields(module, name);
        if args.len() != layout.len() {
            return Err(RuntimeError {
                message: format!(
                    "type '{name}' takes {} field{}, got {}",
                    layout.len(),
                    if layout.len() == 1 { "" } else { "s" },
                    args.len()
                ),
                line: span.line,
                col: span.col,
            });
        }
        let mut fields = Vec::with_capacity(args.len());
        for a in args {
            fields.push(self.eval_expr(a)?);
        }
        Ok(Value::Record {
            module: module.to_string(),
            type_name: name.to_string(),
            fields,
        })
    }

    fn eval_index(
        &mut self,
        base: Value,
        index: Value,
        line: nx_ast::LineNo,
        col: nx_ast::ColNo,
    ) -> Result<Value, RuntimeError> {
        let err = |msg: &str| RuntimeError { message: msg.to_string(), line, col };
        // A dict is keyed by value, not position, so it is matched before
        // the integer index is demanded.
        if let Value::Dict(pairs) = base {
            return match pairs.iter().find(|(k, _)| values_equal(k, &index)) {
                Some((_, v)) => Ok(v.clone()),
                None => Err(err(&format!("key {index} not found"))),
            };
        }
        let i = match index {
            Value::Int(i) => i,
            _ => return Err(err("index must be an integer")),
        };
        match base {
            Value::List(items) => {
                let n = items.len() as i64;
                let pos = if i < 0 { n + i } else { i };
                if pos < 0 || pos >= n {
                    return Err(err(&format!("index {i} out of range (len {n})")));
                }
                Ok(items[pos as usize].clone())
            }
            Value::Str(s) => {
                let chars: Vec<char> = s.chars().collect();
                let n = chars.len() as i64;
                let pos = if i < 0 { n + i } else { i };
                if pos < 0 || pos >= n {
                    return Err(err(&format!("index {i} out of range (len {n})")));
                }
                Ok(Value::Str(chars[pos as usize].to_string()))
            }
            _ => Err(err("only lists, strings and dicts support indexing")),
        }
    }

    fn call_builtin(
        &mut self,
        func: &str,
        args: &[Expr],
        line: nx_ast::LineNo,
        col: nx_ast::ColNo,
    ) -> Result<Value, RuntimeError> {
        // Builtins first.
        if func == "len" {
            if args.len() != 1 {
                return Err(RuntimeError {
                    message: "len() expects 1 argument".to_string(),
                    line,
                    col,
                });
            }
            let v = self.eval_expr(&args[0])?;
            return match v {
                Value::List(items) => Ok(Value::Int(items.len() as i64)),
                Value::Str(s) => Ok(Value::Int(s.chars().count() as i64)),
                Value::Dict(entries) => Ok(Value::Int(entries.len() as i64)),
                _ => Err(RuntimeError {
                    message: "len() only supports lists, strings and dicts".to_string(),
                    line,
                    col,
                }),
            };
        }
        if func == "push" {
            if args.len() != 2 {
                return Err(RuntimeError {
                    message: "push() expects 2 arguments: push(list, value)".to_string(),
                    line,
                    col,
                });
            }
            // First arg must be a variable so we can mutate it in place.
            let name = match &args[0] {
                Expr::Var(n, _) => n.clone(),
                _ => {
                    return Err(RuntimeError {
                        message: "push() first argument must be a list variable".to_string(),
                        line,
                        col,
                    });
                }
            };
            let v = self.eval_expr(&args[1])?;
            let mut lst = match self.lookup(&name) {
                Some(Value::List(items)) => items,
                Some(_) => {
                    return Err(RuntimeError {
                        message: format!("'{name}' is not a list"),
                        line,
                        col,
                    });
                }
                None => {
                    return Err(RuntimeError {
                        message: format!("undefined variable '{name}'"),
                        line,
                        col,
                    });
                }
            };
            lst.push(v);
            self.assign(&name, Value::List(lst))?;
            return Ok(Value::None);
        }
        Err(RuntimeError {
            message: format!("unknown function '{func}'"),
            line,
            col,
        })
    }

    fn call_func(
        &mut self,
        module: &str,
        func: &str,
        args: &[Expr],
        line: nx_ast::LineNo,
        col: nx_ast::ColNo,
    ) -> Result<Value, RuntimeError> {
        let f = self
            .modules
            .get(module)
            .and_then(|m| m.funcs.get(func))
            .cloned()
            .ok_or(RuntimeError {
                message: format!("unknown function '{func}'"),
                line,
                col,
            })?;
        if args.len() != f.params.len() {
            return Err(RuntimeError {
                message: format!(
                    "function '{func}' expects {} args, got {}",
                    f.params.len(),
                    args.len()
                ),
                line,
                col,
            });
        }
        if self.call_depth >= CALL_LIMIT {
            return Err(RuntimeError {
                message: "call stack overflow (possible infinite recursion)".to_string(),
                line,
                col,
            });
        }
        let mut vals = Vec::new();
        for a in args {
            vals.push(self.eval_expr(a)?);
        }
        // Memoized fast path: proven-pure functions of scalar args.
        let key = self.memo_key(module, func, &vals);
        if let Some(k) = &key {
            if let Some(memo) = &self.memo {
                if let Ok(guard) = memo.lock() {
                    if let Some(v) = guard.map.get(k) {
                        return Ok(v.clone());
                    }
                }
            }
        }
        self.call_depth += 1;
        self.frames.push(Frame { module: module.to_string(), ..Default::default() });
        for (p, v) in f.params.iter().zip(vals) {
            // Params are always fresh bindings, never outer writes.
            if let Some(top) = self.frames.last_mut() {
                top.vars.insert(p.clone(), v);
            }
        }
        let ret = self.exec_block(&f.body)?;
        self.frames.pop();
        self.call_depth -= 1;
        let out = match ret {
            None | Some(Flow::Continue) | Some(Flow::Break) => Ok(Value::None),
            Some(Flow::Return(v)) => Ok(v),
        };
        // Populate the cache on the way out.
        if let (Some(k), Ok(v)) = (key, &out) {
            if let Some(memo) = &self.memo {
                if let Ok(mut guard) = memo.lock() {
                    if guard.map.len() >= MEMO_CAP {
                        guard.map.clear();
                    }
                    guard.map.insert(k, v.clone());
                }
            }
        }
        out
    }

    /// Classify once per function; memoizable = pure + closed.
    fn memo_key(&mut self, module: &str, func: &str, vals: &[Value]) -> Option<MemoKey> {
        if std::env::var("NX_NOMEMO").is_ok() {
            return None;
        }
        let id = (module.to_string(), func.to_string());
        if !self.memo_seen.contains(&id) {
            self.memo_seen.insert(id.clone());
            if let Ok(ir) = nx_ir::analyze_map(self.programs.clone()) {
                if let Some(f) = ir.funcs.get(&id) {
                    if nx_ir::memoizable(&f.summary) {
                        self.memo_ok.insert(id.clone());
                    }
                }
            }
            if self.memo.is_none() {
                self.memo = Some(std::sync::Arc::new(std::sync::Mutex::new(Memo::default())));
            }
        }
        if !self.memo_ok.contains(&id) {
            return None;
        }
        let mut args = Vec::with_capacity(vals.len());
        for v in vals {
            args.push(scalar_of(v)?);
        }
        Some(MemoKey { module: module.to_string(), name: func.to_string(), args })
    }

    fn apply_binop(
        &self,
        l: Value,
        op: BinOp,
        r: Value,
        line: nx_ast::LineNo,
        col: nx_ast::ColNo,
    ) -> Result<Value, RuntimeError> {
        let err = |msg: &str| RuntimeError { message: msg.to_string(), line, col };
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Pow => {
                self.apply_arith(l, op, r, line, col)
            }
            BinOp::Mod | BinOp::FloorDiv | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor
            | BinOp::Shl | BinOp::Shr => self.apply_integral(l, op, r, line, col),
            BinOp::Eq => Ok(Value::Bool(values_equal(&l, &r))),
            BinOp::NotEq => Ok(Value::Bool(!values_equal(&l, &r))),
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => self.apply_cmp(l, op, r, line, col),
            BinOp::In => Ok(Value::Bool(self.membership(l, r)?)),
            BinOp::NotIn => Ok(Value::Bool(!self.membership(l, r)?)),
            BinOp::And | BinOp::Or => Err(err("unreachable")),
        }
    }

    /// `x in y`. Lists compare by equality, strings by substring, and a
    /// dict by key. A missing key is `false` rather than an error, which is
    /// what makes `in` usable as a guard.
    fn membership(&self, needle: Value, haystack: Value) -> Result<bool, RuntimeError> {
        match haystack {
            Value::List(items) => Ok(items.iter().any(|v| values_equal(v, &needle))),
            Value::Str(s) => match &needle {
                Value::Str(sub) => Ok(s.contains(sub.as_str())),
                _ => Ok(false),
            },
            Value::Dict(pairs) => Ok(pairs.iter().any(|(k, _)| values_equal(k, &needle))),
            other => Err(RuntimeError {
                message: format!("'in' needs a list, string or dict on the right, found {other}"),
                line: 1,
                col: 1,
            }),
        }
    }

    /// Integer-only operators. These deliberately do not accept a Float:
    /// `7 % 2.5` has no agreed answer, and silently rounding would hide a
    /// bug that the checker would otherwise have caught.
    fn apply_integral(
        &self,
        l: Value,
        op: BinOp,
        r: Value,
        line: nx_ast::LineNo,
        col: nx_ast::ColNo,
    ) -> Result<Value, RuntimeError> {
        let err = |msg: String| RuntimeError { message: msg, line, col };
        let mismatch = || {
            err(format!(
                "operator '{}' needs Int operands, found {} and {}",
                op.as_str(),
                l,
                r
            ))
        };
        let (Value::Int(a), Value::Int(b)) = (&l, &r) else {
            return Err(mismatch());
        };
        let (a, b) = (*a, *b);
        // Both division-derived operators reject a zero divisor up front,
        // before the shared overflow handling below.
        if matches!(op, BinOp::Mod | BinOp::FloorDiv) && b == 0 {
            return Err(err(if matches!(op, BinOp::Mod) {
                "modulo by zero"
            } else {
                "division by zero"
            }
            .to_string()));
        }
        // `div_euclid` rounds the quotient toward negative infinity, which is
        // the floor for every sign combination -- so it is exactly what both
        // `//` and Python's `%` need as a building block.
        let v: Option<i64> = match op {
            // Python's sign convention: the remainder takes the sign of the
            // divisor, so -7 % 3 is 2 and 7 % -3 is -2. Deriving it as
            // `a - floor(a/b) * b` is the only form that gets all four sign
            // combinations right; `rem` gives -1 for -7 % 3.
            BinOp::Mod => floor_div(a, b)
                .and_then(|q| q.checked_mul(b))
                .and_then(|p| a.checked_sub(p)),
            BinOp::FloorDiv => floor_div(a, b),
            BinOp::BitAnd => Some(a & b),
            BinOp::BitOr => Some(a | b),
            BinOp::BitXor => Some(a ^ b),
            BinOp::Shl => {
                if !(0..64).contains(&b) {
                    return Err(err(format!("shift distance {b} out of range")));
                }
                a.checked_shl(b as u32)
            }
            BinOp::Shr => {
                if !(0..64).contains(&b) {
                    return Err(err(format!("shift distance {b} out of range")));
                }
                a.checked_shr(b as u32)
            }
            _ => unreachable!(),
        };
        v.map(Value::Int).ok_or_else(|| err("integer overflow".to_string()))
    }

    fn apply_arith(
        &self,
        l: Value,
        op: BinOp,
        r: Value,
        line: nx_ast::LineNo,
        col: nx_ast::ColNo,
    ) -> Result<Value, RuntimeError> {
        let err = |msg: &str| RuntimeError { message: msg.to_string(), line, col };
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => {
                let v = match op {
                    BinOp::Add => a.checked_add(b),
                    BinOp::Sub => a.checked_sub(b),
                    BinOp::Mul => a.checked_mul(b),
                    BinOp::Div => {
                        if b == 0 {
                            return Err(err("division by zero"));
                        }
                        a.checked_div(b)
                    }
                    // Integer `**` saturates rather than wrapping. An overflowing
                    // power is far more likely to be a runaway loop than a
                    // number the caller wanted. The bases whose power is
                    // always exactly representable are answered exactly
                    // rather than clamped: 0 and +-1 do not saturate.
                    BinOp::Pow => {
                        if b < 0 {
                            // Raising an integer to a negative power has no
                            // integer answer. Promoting here would make
                            // `2 ** 10` a Float on some runs and an Int on
                            // others, so it stays a type error and the
                            // message names the fix.
                            return Err(err(
                                "negative exponent on Int; use a Float exponent for a fractional result",
                            ));
                        }
                        if b > 62 {
                            return Ok(Value::Int(match a {
                                0 => 0,
                                1 => 1,
                                -1 => {
                                    if b % 2 == 0 {
                                        1
                                    } else {
                                        -1
                                    }
                                }
                                x if x < 0 => i64::MIN,
                                _ => i64::MAX,
                            }));
                        }
                        a.checked_pow(b as u32)
                    }
                    _ => unreachable!(),
                }
                .ok_or(err("integer overflow"))?;
                Ok(Value::Int(v))
            }
            (Value::Int(a), Value::Float(b)) => {
                self.apply_arith(Value::Float(a as f64), op, Value::Float(b), line, col)
            }
            (Value::Float(a), Value::Int(b)) => {
                self.apply_arith(Value::Float(a), op, Value::Float(b as f64), line, col)
            }
            (Value::Float(a), Value::Float(b)) => {
                let v = match op {
                    BinOp::Add => a + b,
                    BinOp::Sub => a - b,
                    BinOp::Mul => a * b,
                    BinOp::Div => {
                        if b == 0.0 {
                            return Err(err("division by zero"));
                        }
                        a / b
                    }
                    BinOp::Pow => a.powf(b),
                    _ => unreachable!(),
                };
                Ok(Value::Float(v))
            }
            (Value::Str(a), Value::Str(b)) if op == BinOp::Add => {
                Ok(Value::Str(format!("{a}{b}")))
            }
            _ => Err(err(&format!("operator '{}' type mismatch", op.as_str()))),
        }
    }

    fn apply_cmp(
        &self,
        l: Value,
        op: BinOp,
        r: Value,
        line: nx_ast::LineNo,
        col: nx_ast::ColNo,
    ) -> Result<Value, RuntimeError> {
        let err = |msg: &str| RuntimeError { message: msg.to_string(), line, col };
        let ord = match (&l, &r) {
            (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
            (Value::Int(a), Value::Float(b)) => (*a as f64).partial_cmp(b),
            (Value::Float(a), Value::Int(b)) => a.partial_cmp(&(*b as f64)),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            (Value::Str(a), Value::Str(b)) => Some(a.cmp(b)),
            _ => return Err(err(&format!("operator '{}' type mismatch", op.as_str()))),
        }
        .ok_or(err("comparison failed"))?;
        let b = match op {
            BinOp::Lt => ord.is_lt(),
            BinOp::LtEq => ord.is_le(),
            BinOp::Gt => ord.is_gt(),
            BinOp::GtEq => ord.is_ge(),
            _ => unreachable!(),
        };
        Ok(Value::Bool(b))
    }
}

/// Floor division: the quotient rounded toward negative infinity.
///
/// Rust's `div_euclid` is *not* this. It normalises the remainder to be
/// non-negative instead, which gives `7 / -3 == -2` where floor division
/// is `-3`. Since `%` is defined in terms of the floor quotient, getting
/// this wrong makes `7 % -3` come out 1 rather than -2 -- so the correction
/// is spelled out here rather than delegated.
///
/// Only called with a non-zero divisor; the caller rejects zero.
fn floor_div(a: i64, b: i64) -> Option<i64> {
    let q = a.checked_div(b)?;
    let r = a.checked_rem(b)?;
    if r != 0 && ((r < 0) != (b < 0)) {
        q.checked_sub(1)
    } else {
        Some(q)
    }
}

/// Reinterpret a call receiver as an assignment target, for `mut self`
/// write-back. Only shapes with storage qualify; anything else means the
/// update would be lost, so the caller reports it instead.
fn target_of_expr(e: &Expr) -> Option<nx_ast::Target> {
    match e {
        Expr::Var(name, _) => Some(nx_ast::Target::Name(name.clone())),
        Expr::Index { base, index, .. } => Some(nx_ast::Target::Index {
            base: base.clone(),
            index: index.clone(),
        }),
        Expr::Attr { base, attr, .. } => Some(nx_ast::Target::Attr {
            base: base.clone(),
            field: attr.clone(),
        }),
        _ => None,
    }
}

/// Insert or overwrite a dict key. An existing key keeps its position, so
/// iteration order stays stable across repeated assignment -- the same rule
/// the runtime's `nx_dictset` follows.
fn dict_set(
    mut entries: Vec<(Value, Value)>,
    k: Value,
    v: Value,
) -> Vec<(Value, Value)> {
    match entries.iter_mut().find(|(ek, _)| values_equal(ek, &k)) {
        Some(slot) => slot.1 = v,
        None => entries.push((k, v)),
    }
    entries
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Int(x), Value::Float(y)) => (*x as f64) == *y,
        (Value::Float(x), Value::Int(y)) => *x == (*y as f64),
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::List(x), Value::List(y)) => x == y,
        (Value::Module(x), Value::Module(y)) => x == y,
        (Value::Func { module: m1, name: n1 }, Value::Func { module: m2, name: n2 }) => {
            m1 == m2 && n1 == n2
        }
        (Value::None, Value::None) => true,
        // Order-insensitive: `{a: 1, b: 2}` equals `{b: 2, a: 1}`. Same
        // rule as the runtime's `nx_dicteq`, which is what `==` compiles
        // to on the native path.
        (Value::Dict(x), Value::Dict(y)) => {
            x.len() == y.len()
                && x.iter().all(|(k, v)| {
                    y.iter().any(|(k2, v2)| values_equal(k, k2) && values_equal(v, v2))
                })
        }
        // Same type name, then field by field. The name comparison is by
        // string rather than declaration identity so two structurally
        // identical declarations agree even across modules -- the same
        // rule as the runtime's `nx_receq`.
        (
            Value::Record { type_name: t1, fields: f1, .. },
            Value::Record { type_name: t2, fields: f2, .. },
        ) => t1 == t2 && f1.len() == f2.len() && f1.iter().zip(f2.iter()).all(|(a, b)| values_equal(a, b)),
        _ => false,
    }
}

pub fn run(prog: &Program) -> Result<Vec<String>, RuntimeError> {
    run_with_base(prog, &std::env::current_dir().unwrap_or(".".into()))
}

pub fn run_with_base(
    prog: &Program,
    base: &std::path::Path,
) -> Result<Vec<String>, RuntimeError> {
    let mut interp = Interpreter::with_base(base);
    interp.run(prog)?;
    Ok(interp.output)
}

/// Module names imported anywhere inside a statement (for pre-loading
/// before parallel batches spawn).
fn collect_import_names(s: &Stmt, out: &mut Vec<String>) {
    match s {
        Stmt::Import { module, .. } | Stmt::FromImport { module, .. } => {
            if !out.contains(module) {
                out.push(module.clone());
            }
        }
        Stmt::If { then_body, elifs, else_body, .. } => {
            for t in then_body {
                collect_import_names(t, out);
            }
            for (_, b) in elifs {
                for t in b {
                    collect_import_names(t, out);
                }
            }
            if let Some(b) = else_body {
                for t in b {
                    collect_import_names(t, out);
                }
            }
        }
        Stmt::While { body, .. } | Stmt::For { body, .. } => {
            for t in body {
                collect_import_names(t, out);
            }
        }
        Stmt::Fn { body, .. } => {
            for t in body {
                collect_import_names(t, out);
            }
        }
        Stmt::Parallel { tasks, .. } => {
            for t in tasks {
                collect_import_names(t, out);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_src(src: &str) -> Result<Vec<String>, RuntimeError> {
        let prog = nx_parser::parse_source(src).unwrap_or_else(|e| panic!("{e}"));
        run(&prog)
    }

    /// Run one printing expression and return its single output line.
    fn one(src: &str) -> String {
        let out = lines(src);
        assert_eq!(out.len(), 1, "{src} printed {} lines", out.len());
        out.into_iter().next().unwrap()
    }

    /// Run a program and return every output line.
    fn lines(src: &str) -> Vec<String> {
        run_src(src).unwrap_or_else(|e| panic!("{src}: {}", e.message))
    }

    /// `%` and `//` both key off the floor quotient, and getting either
    /// one wrong silently corrupts every hash bucket, wraparound counter
    /// and slice index that uses them. All four sign combinations are
    /// pinned here because a truncating implementation gets half of them
    /// right, which is the worst possible failure mode.
    #[test]
    fn mod_follows_python_sign_convention() {
        assert_eq!(one("print(7 % 3)"), "1");
        assert_eq!(one("print(-7 % 3)"), "2");
        assert_eq!(one("print(7 % -3)"), "-2");
        assert_eq!(one("print(-7 % -3)"), "-1");
        assert_eq!(one("print(0 % 5)"), "0");
        assert_eq!(one("print(5 % 5)"), "0");
        // A negative modulus with a negative dividend still lands in
        // (-|b|, 0].
        assert_eq!(one("print(-1 % 5)"), "4");
    }

    #[test]
    fn floor_div_rounds_toward_negative_infinity() {
        assert_eq!(one("print(7 // 3)"), "2");
        assert_eq!(one("print(-7 // 3)"), "-3");
        assert_eq!(one("print(7 // -3)"), "-3");
        assert_eq!(one("print(-7 // -3)"), "2");
        assert_eq!(one("print(6 // 3)"), "2");
    }

    #[test]
    fn mod_and_floordiv_by_zero_error() {
        assert!(run_src("print(1 % 0)").is_err());
        assert!(run_src("print(1 // 0)").is_err());
    }

    #[test]
    fn power() {
        assert_eq!(one("print(2 ** 10)"), "1024");
        assert_eq!(one("print(0 ** 0)"), "1");
        assert_eq!(one("print(2 ** 0)"), "1");
        assert_eq!(one("print((-2) ** 3)"), "-8");
        assert_eq!(one("print((-2) ** 4)"), "16");
        assert_eq!(one("print(2 ** 62)"), "4611686018427387904");
        // Past the representable range the answer saturates rather than
        // wrapping to a negative number.
        assert_eq!(one("print(2 ** 63)"), "9223372036854775807");
        assert_eq!(one("print((-2) ** 63)"), "-9223372036854775808");
        // 0 and +-1 do not saturate: their powers are always exact.
        assert_eq!(one("print(0 ** 100)"), "0");
        assert_eq!(one("print(1 ** 100)"), "1");
        assert_eq!(one("print((-1) ** 100)"), "1");
        assert_eq!(one("print((-1) ** 101)"), "-1");
        assert_eq!(one("print(2.0 ** 10)"), "1024");
        assert_eq!(one("print(2 ** 0.5)"), "1.4142135623731");
        // A negative exponent on an Int has no integer answer. Promoting
        // would make the result type depend on the run, so it is refused
        // and the message points at the Float form.
        let e = run_src("print(2 ** -1)").unwrap_err();
        assert!(e.message.contains("Float exponent"), "{}", e.message);
        assert_eq!(one("print(2.0 ** -1)"), "0.5");
    }

    #[test]
    fn bitwise_operators() {
        assert_eq!(one("print(6 & 3)"), "2");
        assert_eq!(one("print(6 | 3)"), "7");
        assert_eq!(one("print(6 ^ 3)"), "5");
        assert_eq!(one("print(~6)"), "-7");
        assert_eq!(one("print(1 << 10)"), "1024");
        assert_eq!(one("print(1024 >> 3)"), "128");
        assert_eq!(one("print(-8 >> 1)"), "-4");
        assert_eq!(one("print(0xF0 & 0x3C)"), "48");
    }

    #[test]
    fn shift_distance_out_of_range_errors() {
        assert!(run_src("print(1 << 64)").is_err());
        assert!(run_src("print(1 << -1)").is_err());
        assert!(run_src("print(1 >> 100)").is_err());
    }

    #[test]
    fn bitwise_on_float_is_a_type_error() {
        // There is no agreed answer for `7 % 2.5`, so it is refused
        // rather than rounded.
        assert!(run_src("print(7 % 2.5)").is_err());
        assert!(run_src("print(7 & 2.5)").is_err());
    }

    #[test]
    fn augmented_assignment_covers_every_operator() {
        assert_eq!(one("x = 7\nx %= 3\nprint(x)"), "1");
        assert_eq!(one("x = 7\nx //= 2\nprint(x)"), "3");
        assert_eq!(one("x = 2\nx **= 10\nprint(x)"), "1024");
        assert_eq!(one("x = 6\nx &= 3\nprint(x)"), "2");
        assert_eq!(one("x = 6\nx |= 1\nprint(x)"), "7");
        assert_eq!(one("x = 6\nx ^= 3\nprint(x)"), "5");
        assert_eq!(one("x = 1\nx <<= 4\nprint(x)"), "16");
        assert_eq!(one("x = 16\nx >>= 2\nprint(x)"), "4");
    }

    #[test]
    fn membership() {
        assert_eq!(one("print(2 in [1, 2, 3])"), "true");
        assert_eq!(one("print(9 in [1, 2, 3])"), "false");
        assert_eq!(one("print(2 not in [1, 2, 3])"), "false");
        assert_eq!(one("print(\"ell\" in \"hello\")"), "true");
        assert_eq!(one("print(\"zz\" in \"hello\")"), "false");
        assert_eq!(one("print(\"\" in \"hello\")"), "true");
        assert_eq!(one("print(\"a\" in {\"a\": 1})"), "true");
        assert_eq!(one("print(\"b\" in {\"a\": 1})"), "false");
    }

    #[test]
    fn not_binds_looser_than_comparison() {
        // `not a in b` has to mean `not (a in b)`. At the unary level it
        // would read as `(not a) in b`, which is never what was meant.
        assert_eq!(one("print(not 1 in [1, 2])"), "false");
        assert_eq!(one("print(3 not in [1, 2])"), "true");
        assert_eq!(one("print(not 1 == 2)"), "true");
    }

    #[test]
    fn number_literal_forms() {
        assert_eq!(one("print(1_000_000)"), "1000000");
        assert_eq!(one("print(0xff)"), "255");
        assert_eq!(one("print(0o17)"), "15");
        assert_eq!(one("print(0b1011)"), "11");
        // The float formatter trims a trailing ".0", matching how a
        // whole-valued float has always printed.
        assert_eq!(one("print(1e3)"), "1000");
        assert_eq!(one("print(2.5e-3)"), "0.0025");
        assert_eq!(one("print(0xFF)"), "255");
        assert_eq!(one("print(0b1010_1010)"), "170");
    }

    #[test]
    fn none_literal() {
        assert_eq!(one("print(None)"), "none");
        assert_eq!(one("x = None\nprint(x == None)"), "true");
    }

    #[test]
    fn ternary_evaluates_only_the_taken_branch() {
        assert_eq!(one("print(1 if true else 2)"), "1");
        assert_eq!(one("print(1 if false else 2)"), "2");
        // The untaken branch is never evaluated, so it may be anything.
        assert_eq!(one("print(1 if true else 1/0)"), "1");
        assert_eq!(one("n = 5\nprint(\"big\" if n > 3 else \"small\")"), "big");
    }

    #[test]
    fn ranges_as_expressions() {
        assert_eq!(one("print(0..5)"), "[0, 1, 2, 3, 4]");
        assert_eq!(one("print(3..3)"), "[]");
        // An inverted range is empty rather than an error.
        assert_eq!(one("print(5..1)"), "[]");
        assert_eq!(one("print([i for i in 0..4])"), "[0, 1, 2, 3]");
    }

    #[test]
    fn comprehension_scopes_its_loop_variable() {
        assert_eq!(
            lines("i = 99\nprint([i for i in 0..3])\nprint(i)"),
            vec!["[0, 1, 2]", "99"]
        );
        assert_eq!(one("print([i * i for i in 0..5])"), "[0, 1, 4, 9, 16]");
        assert_eq!(one("print([i for i in 0..6 if i % 2 == 0])"), "[0, 2, 4]");
        // A comprehension over a string iterates its characters.
        assert_eq!(one("print([c for c in \"abc\"])"), "[a, b, c]");
    }

    #[test]
    fn indexed_assignment() {
        assert_eq!(one("xs = [1, 2, 3]\nxs[0] = 10\nprint(xs)"), "[10, 2, 3]");
        assert_eq!(one("xs = [1, 2, 3]\nxs[2] += 5\nprint(xs)"), "[1, 2, 8]");
        assert_eq!(one("xs = [1, 2, 3]\nxs[-1] = 9\nprint(xs)"), "[1, 2, 9]");
        assert!(run_src("xs = [1]\nxs[5] = 0").is_err());
    }

    /// Assignment copies the list rather than aliasing it, so a later
    /// write through the original is not visible through the copy. This
    /// is the same rule for every container and it is what makes values
    /// safe to pass around without aliasing hazards.
    #[test]
    fn assignment_copies_containers() {
        assert_eq!(one("xs = [1, 2, 3]\nys = xs\nxs[0] = 7\nprint(ys)"), "[1, 2, 3]");
        assert_eq!(
            one("xs = [1, 2, 3]\nys = xs\nys[0] = 7\nprint(xs)"),
            "[1, 2, 3]"
        );
        assert_eq!(
            one("a = {\"k\": 1}\nb = a\na[\"k\"] = 2\nprint(b)"),
            "{k: 1}"
        );
    }

    #[test]
    fn nested_indexed_assignment() {
        assert_eq!(
            one("g = [[0, 0], [0, 0]]\ng[1][0] = 7\nprint(g)"),
            "[[0, 0], [7, 0]]"
        );
    }

    #[test]
    fn multiple_assignment_swaps() {
        // Both right-hand sides are evaluated before either store, so a
        // swap swaps rather than clobbering.
        assert_eq!(
            lines("a = 1\nb = 2\na, b = b, a\nprint(a)\nprint(b)"),
            vec!["2", "1"]
        );
    }

    #[test]
    fn multiple_return_is_destructured() {
        assert_eq!(
            one("fn p():\n    return 1, 2\nx, y = p()\nprint(x + y)"),
            "3"
        );
        // A mixed tuple stays dynamic rather than being rejected.
        assert_eq!(
            lines("fn p():\n    return 1, \"a\"\nx, y = p()\nprint(x)\nprint(y)"),
            vec!["1", "a"]
        );
        // Unpacking the wrong number of values is an error.
        assert!(run_src("fn p():\n    return 1, 2\nx, y, z = p()").is_err());
        // Destructuring a non-tuple is an error rather than a silent bind.
        assert!(run_src("x, y = 5").is_err());
    }

    #[test]
    fn single_value_return_is_not_wrapped() {
        assert_eq!(one("fn f():\n    return 5\nprint(f())"), "5");
        assert_eq!(one("fn f():\n    return\nprint(f())"), "none");
    }

    #[test]
    fn dict_basics() {
        assert_eq!(one("d = {\"a\": 1, \"b\": 2}\nprint(d)"), "{a: 1, b: 2}");
        assert_eq!(one("d = {\"a\": 1}\nprint(d[\"a\"])"), "1");
        assert_eq!(one("d = {\"a\": 1}\nprint(len(d))"), "1");
        assert_eq!(one("d = {}\nprint(d)"), "{}");
        assert!(run_src("d = {\"a\": 1}\nprint(d[\"z\"])").is_err());
    }

    /// Insertion order is the iteration order, and re-assigning an
    /// existing key does not move it. `parallel:`'s determinism guarantee
    /// rests on this, so it is pinned rather than left to chance.
    #[test]
    fn dict_preserves_insertion_order() {
        assert_eq!(one("d = {}\nd[\"z\"] = 1\nd[\"a\"] = 2\nd[\"m\"] = 3\nprint(d)"), "{z: 1, a: 2, m: 3}");
        assert_eq!(one("d = {}\nd[\"z\"] = 1\nd[\"a\"] = 2\nd[\"z\"] = 9\nprint(d)"), "{z: 9, a: 2}");
        // A duplicate key in a literal keeps the first position.
        assert_eq!(one("d = {\"a\": 1, \"b\": 2, \"a\": 3}\nprint(d)"), "{a: 3, b: 2}");
    }

    #[test]
    fn dicts_accept_scalar_keys() {
        assert_eq!(one("d = {1: \"one\", 2: \"two\"}\nprint(d[2])"), "two");
        assert_eq!(one("d = {true: 1}\nprint(d[true])"), "1");
        assert_eq!(one("d = {1.5: \"x\"}\nprint(d[1.5])"), "x");
    }

    #[test]
    fn dict_delete() {
        assert_eq!(one("d = {\"a\": 1, \"b\": 2}\ndel d[\"a\"]\nprint(d)"), "{b: 2}");
        assert!(run_src("d = {\"a\": 1}\ndel d[\"z\"]").is_err());
    }

    #[test]
    fn list_delete() {
        assert_eq!(one("xs = [1, 2, 3]\ndel xs[1]\nprint(xs)"), "[1, 3]");
        assert_eq!(one("xs = [1, 2, 3]\ndel xs[-1]\nprint(xs)"), "[1, 2]");
        assert!(run_src("xs = [1]\ndel xs[4]").is_err());
    }

    #[test]
    fn del_unbinds_a_name() {
        // `del` really removes the binding, so a later read is an error
        // rather than seeing the old value.
        assert!(run_src("x = 1\ndel x\nprint(x)").is_err());
        assert!(run_src("del never_defined").is_err());
        // The name can be bound again afterwards.
        assert_eq!(one("x = 1\ndel x\nx = 2\nprint(x)"), "2");
    }

    #[test]
    fn assert_passes_and_fails() {
        assert_eq!(one("assert 1 < 2\nprint(\"ok\")"), "ok");
        assert!(run_src("assert 1 > 2").is_err());
        assert!(run_src("assert 1 > 2, \"too small\"").is_err());
        // A non-Bool condition is a type error, not a truthiness test.
        assert!(run_src("assert 1").is_err());
    }

    #[test]
    fn slices_copy() {
        assert_eq!(one("xs = [0, 1, 2, 3, 4, 5]\nprint(xs[1:4])"), "[1, 2, 3]");
        assert_eq!(one("xs = [0, 1, 2, 3, 4, 5]\nprint(xs[:2])"), "[0, 1]");
        assert_eq!(one("xs = [0, 1, 2, 3, 4, 5]\nprint(xs[3:])"), "[3, 4, 5]");
        assert_eq!(one("xs = [0, 1, 2, 3, 4, 5]\nprint(xs[::2])"), "[0, 2, 4]");
        assert_eq!(one("xs = [0, 1, 2, 3, 4, 5]\nprint(xs[:])"), "[0, 1, 2, 3, 4, 5]");
        // Bounds are clamped, not errors.
        assert_eq!(one("xs = [0, 1, 2]\nprint(xs[1:99])"), "[1, 2]");
        assert_eq!(one("xs = [0, 1, 2]\nprint(xs[-2:])"), "[1, 2]");
        // A slice is a copy, so mutating the original leaves it alone.
        assert_eq!(
            one("xs = [0, 1, 2, 3]\ny = xs[1:3]\nxs[1] = 9\nprint(y)"),
            "[1, 2]"
        );
        assert!(run_src("xs = [1, 2]\nprint(xs[::0])").is_err());
    }

    #[test]
    fn string_slices_produce_strings() {
        assert_eq!(one("print(\"hello\"[1:4])"), "ell");
        assert_eq!(one("print(\"hello\"[:2])"), "he");
        assert_eq!(one("print(\"hello\"[3:])"), "lo");
        assert_eq!(one("print(\"hello\"[::2])"), "hlo");
    }

    #[test]
    fn unary_plus_and_bitnot() {
        assert_eq!(one("print(+5)"), "5");
        assert_eq!(one("print(+2.5)"), "2.5");
        assert_eq!(one("print(~0)"), "-1");
    }

    #[test]
    fn pow_binds_tighter_than_a_prefix_operator() {
        // Python's rule: -2 ** 2 is -(2 ** 2) = -4, not 4.
        assert_eq!(one("print(-2 ** 2)"), "-4");
        // The exponent may itself be signed, as long as it stays a Float.
        assert_eq!(one("print(2.0 ** -1)"), "0.5");
    }

    // ---- records ----

    #[test]
    fn record_construction_and_field_read() {
        assert_eq!(
            one("type P:\n    x: Int\n    y: Int\np = P(1, 2)\nprint(p)"),
            "P(1, 2)"
        );
        assert_eq!(
            one("type P:\n    x: Int\n    y: Int\np = P(1, 2)\nprint(p.x + p.y)"),
            "3"
        );
    }

    #[test]
    fn record_field_write() {
        assert_eq!(
            one("type P:\n    x: Int\np = P(1)\np.x = 9\nprint(p.x)"),
            "9"
        );
        assert_eq!(
            one("type P:\n    x: Int\np = P(1)\np.x += 5\nprint(p)"),
            "P(6)"
        );
    }

    #[test]
    fn record_constructor_arity_is_exact() {
        assert!(run_src("type P:\n    x: Int\n    y: Int\np = P(1)\nprint(p)").is_err());
        assert!(run_src("type P:\n    x: Int\np = P(1, 2)\nprint(p)").is_err());
    }

    #[test]
    fn record_unknown_field_errors() {
        assert!(run_src("type P:\n    x: Int\np = P(1)\nprint(p.z)").is_err());
        assert!(run_src("type P:\n    x: Int\np = P(1)\np.z = 2").is_err());
    }

    #[test]
    fn record_equality_is_structural() {
        assert_eq!(
            one("type P:\n    x: Int\n    y: Int\nprint(P(1, 2) == P(1, 2))"),
            "true"
        );
        assert_eq!(
            one("type P:\n    x: Int\n    y: Int\nprint(P(1, 2) == P(1, 3))"),
            "false"
        );
        // Different types never compare equal, even field-identical ones.
        assert_eq!(
            one("type P:\n    x: Int\ntype Q:\n    x: Int\nprint(P(1) == Q(1))"),
            "false"
        );
    }

    #[test]
    fn dict_equality_is_structural() {
        assert_eq!(one("print({\"a\": 1} == {\"a\": 1})"), "true");
        assert_eq!(one("print({\"a\": 1} == {\"a\": 2})"), "false");
        assert_eq!(one("print({\"a\": 1} == {\"a\": 1, \"b\": 2})"), "false");
        // Order-insensitive: iteration order is stable, equality is not
        // order.
        assert_eq!(one("print({\"a\": 1, \"b\": 2} == {\"b\": 2, \"a\": 1})"), "true");
    }

    /// Containers have value semantics: binding copies, so a later write
    /// through the original is invisible through the copy. This is the
    /// rule the native backend's `nx_clone` implements.
    #[test]
    fn assignment_copies_lists_dicts_and_records() {
        assert_eq!(
            one("xs = [1, 2]\nys = xs\nxs[0] = 9\nprint(ys)"),
            "[1, 2]"
        );
        assert_eq!(
            one("a = {\"k\": 1}\nb = a\na[\"k\"] = 9\nprint(b)"),
            "{k: 1}"
        );
        assert_eq!(
            one("type P:\n    x: Int\np = P(1)\nq = p\np.x = 9\nprint(q)"),
            "P(1)"
        );
        // Nested containers copy all the way down.
        assert_eq!(
            one("xs = [[1]]\nys = xs\nxs[0][0] = 9\nprint(ys)"),
            "[[1]]"
        );
    }

    /// Arguments never alias the caller's value: mutating a parameter is
    /// invisible outside the call.
    #[test]
    fn arguments_do_not_alias() {
        assert_eq!(
            lines("fn bump(xs):\n    xs[0] = 99\n    return xs[0]\nxs = [1]\nprint(bump(xs))\nprint(xs)"),
            vec!["99", "[1]"]
        );
        assert_eq!(
            lines("type P:\n    x: Int\nfn bump(p):\n    p.x = 99\n    return p.x\np = P(1)\nprint(bump(p))\nprint(p)"),
            vec!["99", "P(1)"]
        );
        assert_eq!(
            lines("fn put(d):\n    d[\"k\"] = 99\n    return d[\"k\"]\nd = {\"k\": 1}\nprint(put(d))\nprint(d)"),
            vec!["99", "{k: 1}"]
        );
    }

    #[test]
    fn dynamic_field_access() {
        // Through an unresolved parameter the field resolves by name at
        // runtime -- the same path the native backend's `nx_rec_getn`
        // takes.
        assert_eq!(
            one("type P:\n    x: Int\nfn get(p):\n    return p.x\nprint(get(P(7)))"),
            "7"
        );
        assert_eq!(
            one("type P:\n    x: Int\nfn bump(p):\n    p.x += 1\n    return p.x\nprint(bump(P(7)))"),
            "8"
        );
        assert!(run_src("type P:\n    x: Int\nfn get(p):\n    return p.z\nprint(get(P(7)))").is_err());
    }

    #[test]
    fn record_del_blanks_the_field() {
        // A record's arity is fixed, so `del` blanks rather than removes.
        assert_eq!(
            one("type P:\n    x: Int\n    y: Int\np = P(1, 2)\ndel p.x\nprint(p)"),
            "P(none, 2)"
        );
    }

    #[test]
    fn records_in_containers() {
        assert_eq!(
            one("type P:\n    x: Int\npts = [P(1), P(2)]\nprint(pts[1].x)"),
            "2"
        );
        assert_eq!(
            one("type P:\n    x: Int\ntype Line:\n    a\n    b\nl = Line(P(1), P(2))\nprint(l.b.x)"),
            "2"
        );
    }

    #[test]
    fn unresolved_index_accepts_scalar_keys() {
        // An unresolved base may hold a list or a dict; a scalar key is
        // accepted for either and dispatches at runtime.
        assert_eq!(
            one("fn get(d, k):\n    return d[k]\nprint(get({\"a\": 5}, \"a\"))"),
            "5"
        );
        assert_eq!(
            one("fn get(xs, k):\n    return xs[k]\nprint(get([10, 20], 1))"),
            "20"
        );
        assert_eq!(
            one("fn set(d, k, v):\n    d[k] = v\n    return d[k]\nprint(set({\"a\": 1}, \"b\", 2))"),
            "2"
        );
    }

    #[test]
    fn hello_arithmetic() {
        let out = run_src("x = 10 + 20\nprint(x)").unwrap();
        assert_eq!(out, vec!["30"]);
    }

    #[test]
    fn precedence() {
        let out = run_src("print(1 + 2 * 3)").unwrap();
        assert_eq!(out, vec!["7"]);
    }

    #[test]
    fn undefined_var_errors() {
        assert!(run_src("print(y)").is_err());
    }

    #[test]
    fn div_by_zero_errors() {
        assert!(run_src("print(1 / 0)").is_err());
    }

    #[test]
    fn string_print() {
        let out = run_src("print(\"hi\")").unwrap();
        assert_eq!(out, vec!["hi"]);
    }

    #[test]
    fn multi_arg_print() {
        let out = run_src("y = \"Hasan\"\nx = \"Hasanat\"\nprint(y, x)").unwrap();
        assert_eq!(out, vec!["Hasan Hasanat"]);
    }

    #[test]
    fn float_mixed_math() {
        let out = run_src("x = 10.8 + 20\ny = x * 2\nprint(y)").unwrap();
        assert_eq!(out.len(), 1);
        let v: f64 = out[0].parse().unwrap();
        assert!((v - 61.6).abs() < 1e-9);
    }

    #[test]
    fn float_noise_rounded() {
        let out = run_src("x = 10.8 + 20.1\ny = x * 2\nprint(y)").unwrap();
        assert_eq!(out, vec!["61.8"]);
    }

    #[test]
    fn div_keeps_precision() {
        let out = run_src("print(1.0 / 3.0)").unwrap();
        assert_eq!(out, vec!["0.333333333333333"]);
    }

    #[test]
    fn reassign_different_type() {
        let out = run_src("x = \"a\"\nx = 1\nprint(x)").unwrap();
        assert_eq!(out, vec!["1"]);
    }

    #[test]
    fn if_else_true() {
        let out = run_src("x = 5\nif x > 3:\n    print(\"big\")\nelse:\n    print(\"small\")").unwrap();
        assert_eq!(out, vec!["big"]);
    }

    #[test]
    fn if_else_false() {
        let out = run_src("x = 1\nif x > 3:\n    print(\"big\")\nelse:\n    print(\"small\")").unwrap();
        assert_eq!(out, vec!["small"]);
    }

    #[test]
    fn while_loop() {
        let out = run_src("i = 0\nwhile i < 3:\n    print(i)\n    i = i + 1").unwrap();
        assert_eq!(out, vec!["0", "1", "2"]);
    }

    #[test]
    fn bool_logic() {
        let out = run_src("print(true and false, true or false, not false)").unwrap();
        assert_eq!(out, vec!["false true true"]);
    }

    #[test]
    fn non_bool_condition_errors() {
        assert!(run_src("if 1:\n    print(1)").is_err());
    }

    #[test]
    fn for_range_up() {
        let out = run_src("for i in 0..5:\n    print(i)").unwrap();
        assert_eq!(out, vec!["0", "1", "2", "3", "4"]);
    }

    #[test]
    fn for_each_list() {
        let out = run_src("nums = [\"a\", \"b\"]\nfor i in nums:\n    print(i)").unwrap();
        assert_eq!(out, vec!["a", "b"]);
    }

    #[test]
    fn for_no_manual_increment() {
        let out = run_src("for i in 0..3:\n    print(i * 2)").unwrap();
        assert_eq!(out, vec!["0", "2", "4"]);
    }

    #[test]
    fn fn_add() {
        let out = run_src("fn add(a, b):\n    return a + b\nprint(add(2, 3))").unwrap();
        assert_eq!(out, vec!["5"]);
    }

    #[test]
    fn fn_no_return_is_none() {
        let out = run_src("fn f():\n    print(\"hi\")\nprint(f())").unwrap();
        assert_eq!(out, vec!["hi", "none"]);
    }

    #[test]
    fn fn_recursion() {
        let out = run_src("fn fact(n):\n    if n <= 1:\n        return 1\n    else:\n        return n * fact(n - 1)\nprint(fact(5))").unwrap();
        assert_eq!(out, vec!["120"]);
    }

    #[test]
    fn memo_fib() {
        // Naive fib(30) would take ~1.6M calls; memoization makes it 31.
        let out = run_src("fn fib(n):\n    if n <= 1:\n        return n\n    else:\n        return fib(n - 1) + fib(n - 2)\nprint(fib(30))").unwrap();
        assert_eq!(out, vec!["832040"]);
    }

    #[test]
    fn memo_skips_mutable_args() {
        // List args never enter the cache: mutation stays visible.
        let out = run_src("fn first(a):\n    return a[0]\nl = [1]\nprint(first(l))\npush(l, 2)\nl2 = [1, 9]\nprint(first(l2))").unwrap();
        assert_eq!(out, vec!["1", "1"]);
    }

    #[test]
    fn return_outside_errors() {
        assert!(run_src("return 1").is_err());
    }

    #[test]
    fn elif_chain() {
        let out = run_src("x = 2\nif x == 1:\n    print(1)\nelif x == 2:\n    print(2)\nelse:\n    print(3)").unwrap();
        assert_eq!(out, vec!["2"]);
    }

    #[test]
    fn break_continue() {
        let out = run_src("for i in 0..10:\n    if i == 2:\n        continue\n    if i == 4:\n        break\n    print(i)").unwrap();
        assert_eq!(out, vec!["0", "1", "3"]);
    }

    #[test]
    fn aug_assign() {
        let out = run_src("x = 1\nx += 2\nx *= 3\nprint(x)").unwrap();
        assert_eq!(out, vec!["9"]);
    }

    #[test]
    fn list_index_len_push() {
        let out = run_src("a = [1, 2, 3]\nprint(a[0], a[-1], len(a))\npush(a, 4)\nprint(a, len(a))").unwrap();
        assert_eq!(out, vec!["1 3 3", "[1, 2, 3, 4] 4"]);
    }

    fn mod_dir(tag: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "nxmod-{tag}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, src) in files {
            std::fs::write(dir.join(name), src).unwrap();
        }
        dir
    }

    fn run_entry(dir: &std::path::Path, entry: &str) -> Result<Vec<String>, RuntimeError> {
        let src = std::fs::read_to_string(dir.join(entry)).unwrap();
        let prog = nx_parser::parse_source(&src).unwrap_or_else(|e| panic!("{e}"));
        run_with_base(&prog, dir)
    }

    #[test]
    fn import_attr_and_from() {
        let dir = mod_dir(
            "basic",
            &[
                ("utils.nx", "VERSION = \"1.0\"\nfn double(n):\n    return n * 2\n"),
                (
                    "main.nx",
                    "import utils\nfrom utils import double as d\nprint(utils.VERSION)\nprint(utils.double(21))\nprint(d(4))\n",
                ),
            ],
        );
        let out = run_entry(&dir, "main.nx").unwrap();
        assert_eq!(out, vec!["1.0", "42", "8"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn import_missing_errors() {
        let dir = mod_dir("missing", &[("main.nx", "import nope\n")]);
        assert!(run_entry(&dir, "main.nx").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn import_circular_errors() {
        let dir = mod_dir(
            "circ",
            &[
                ("a.nx", "import b\n"),
                ("b.nx", "import a\n"),
                ("main.nx", "import a\n"),
            ],
        );
        assert!(run_entry(&dir, "main.nx").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parallel_join_merges() {
        let out = run_src("a = 0\nb = 0\nparallel:\n    a = 1\n    b = 2\nprint(a, b)").unwrap();
        assert_eq!(out, vec!["1 2"]);
    }

    #[test]
    fn parallel_conflicts_serialize_in_order() {
        let out = run_src("x = 0\nparallel:\n    x = x + 1\n    x = x + 1\nprint(x)").unwrap();
        assert_eq!(out, vec!["2"]);
    }

    #[test]
    fn parallel_outer_write_errors() {
        assert!(run_src("fn f():\n    x = 1\n    parallel:\n        x = 2\n    print(x)\nf()").is_err());
    }

    #[test]
    fn parallel_fn_tasks() {
        let out = run_src("fn one():\n    return 1\nfn two():\n    return 2\nparallel:\n    a = one()\n    b = two()\nprint(a + b)").unwrap();
        assert_eq!(out, vec!["3"]);
    }

    // --- impl blocks and methods ------------------------------------

    const POINT: &str = "type P:\n    x: Int\n    y: Int\n";

    #[test]
    fn method_reads_self() {
        assert_eq!(
            one(&format!("{POINT}impl P:\n    fn sum(self):\n        return self.x + self.y\np = P(3, 4)\nprint(p.sum())\n")),
            "7"
        );
    }

    /// `mut self` writes its result back into the receiver, which is what
    /// makes the change visible to the caller.
    #[test]
    fn mut_self_writes_back_into_the_receiver() {
        let out = lines(&format!(
            "{POINT}impl P:\n    fn moved(mut self, d):\n        self.x = self.x + d\n        self.y = self.y + d\n        return self\np = P(1, 1)\np.moved(2)\nprint(p)\n"
        ));
        assert_eq!(out, vec!["P(3, 3)"]);
    }

    /// A `mut self` method also evaluates to its result, so it works in an
    /// expression.
    #[test]
    fn mut_self_evaluates_to_its_result() {
        assert_eq!(
            one(&format!("{POINT}impl P:\n    fn plus(mut self, d):\n        self.x = self.x + d\n        return self\np = P(1, 1)\nprint(p.plus(9).x)\n")),
            "10"
        );
    }

    /// With no storage on the base there is nowhere to write back, so the
    /// call still evaluates -- and a chain reads as one expression.
    #[test]
    fn mut_self_chain_writes_back_only_where_there_is_storage() {
        let out = lines(&format!(
            "{POINT}impl P:\n    fn plus(mut self, d):\n        self.x = self.x + d\n        self.y = self.y + d\n        return self\np = P(0, 0)\nprint(p.plus(1).plus(1))\nprint(p)\n"
        ));
        // The inner call sees a variable, so it writes back once: (1, 1).
        // The outer call sits on a call result, so it adds without writing.
        assert_eq!(out, vec!["P(2, 2)", "P(1, 1)"]);
    }

    #[test]
    fn associated_function_needs_no_receiver() {
        let out = lines(&format!(
            "{POINT}impl P:\n    fn zero():\n        return P(0, 0)\nprint(P.zero())\n"
        ));
        assert_eq!(out, vec!["P(0, 0)"]);
    }

    #[test]
    fn methods_resolve_on_a_record_held_in_a_list() {
        let out = lines(&format!(
            "{POINT}impl P:\n    fn sum(self):\n        return self.x + self.y\nps = [P(1, 2), P(3, 4)]\nt = 0\nfor p in ps:\n    t = t + p.sum()\nprint(t)\n"
        ));
        assert_eq!(out, vec!["10"]);
    }

    /// Methods resolve before builtin sugar, so a type may define its own
    /// `push`.
    #[test]
    fn a_method_may_shadow_a_builtin() {
        assert_eq!(
            one(&format!(
                "{POINT}impl P:\n    fn push(self, v):\n        return self.x + v\np = P(1, 0)\nprint(p.push(41))\n"
            )),
            "42"
        );
        // And the sugar still works on a plain list.
        assert_eq!(one("xs = [1]\nxs.push(2)\nprint(xs)\n"), "[1, 2]");
    }

    #[test]
    fn unknown_method_names_the_type() {
        let e = run_src(&format!("{POINT}p = P(1, 2)\nprint(p.nope())\n")).unwrap_err();
        assert!(e.message.contains("has no method 'nope'"), "{}", e.message);
    }

    #[test]
    fn impl_of_unknown_type_is_a_runtime_error() {
        let e = run_src("impl Nope:\n    fn a(self):\n        return 1\n").unwrap_err();
        assert!(e.message.contains("unknown type 'Nope'"), "{}", e.message);
    }

    #[test]
    fn method_recursion_calls_itself() {
        assert_eq!(
            one(&format!(
                "{POINT}impl P:\n    fn up(mut self, n):\n        if n <= 0:\n            return self\n        self.x = self.x + 1\n        return self.up(n - 1)\np = P(0, 0)\nprint(p.up(5).x)\n"
            )),
            "5"
        );
    }

    #[test]
    fn method_recursion_terminates_on_the_depth_limit() {
        // The limit is checked per call, so runaway recursion is a clean
        // error rather than an unbounded native stack. The body recurses
        // through several native frames per level, so the check runs on a
        // thread with room to spare: what is under test is the limit, not
        // the host's stack size.
        let src = format!(
            "{POINT}impl P:\n    fn down(mut self, n):\n        if n <= 0:\n            return self\n        return self.down(n - 1)\np = P(0, 0)\nprint(p.down(100000))\n"
        );
        let msg = std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(move || run_src(&src).unwrap_err().message)
            .unwrap()
            .join()
            .unwrap();
        assert!(msg.contains("call stack overflow"), "{msg}");
    }
}
