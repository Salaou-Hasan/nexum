use std::collections::HashMap;
use nx_ast::{BinOp, Expr, Program, Stmt, UnaryOp};

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    List(Vec<Value>),
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
    pub line: usize,
    pub col: usize,
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "runtime error at {}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for RuntimeError {}

const LOOP_LIMIT: u64 = 10_000_000;
const CALL_LIMIT: usize = 500;

#[derive(Debug, Clone)]
struct Function {
    params: Vec<String>,
    body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
enum Flow {
    Return(Value),
    Break,
    Continue,
}

#[derive(Default)]
pub struct Interpreter {
    scopes: Vec<HashMap<String, Value>>,
    functions: HashMap<String, Function>,
    pub output: Vec<String>,
    call_depth: usize,
}

impl Interpreter {
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
            functions: HashMap::new(),
            output: Vec::new(),
            call_depth: 0,
        }
    }

    pub fn run(&mut self, prog: &Program) -> Result<(), RuntimeError> {
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
            Stmt::Assign { name, value, .. } => {
                let v = self.eval_expr(value)?;
                self.assign(name, v);
                Ok(None)
            }
            Stmt::AssignOp { name, op, value, span } => {
                let cur = self.lookup(name).ok_or(RuntimeError {
                    message: format!("undefined variable '{name}'"),
                    line: span.line,
                    col: span.col,
                })?;
                let rhs = self.eval_expr(value)?;
                let v = self.apply_arith(cur, *op, rhs, span.line, span.col)?;
                self.assign(name, v);
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
                self.functions.insert(
                    name.clone(),
                    Function { params: params.clone(), body: body.clone() },
                );
                Ok(None)
            }
            Stmt::Return { value, .. } => {
                let v = match value {
                    Some(e) => self.eval_expr(e)?,
                    None => Value::None,
                };
                Ok(Some(Flow::Return(v)))
            }
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
                    self.assign(var, Value::Int(cur));
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
                    _ => {
                        let sp = expr.span();
                        return Err(RuntimeError {
                            message: "for loop only supports ranges, lists and strings".to_string(),
                            line: sp.line,
                            col: sp.col,
                        });
                    }
                };
                let mut count: u64 = 0;
                for item in items {
                    self.assign(var, item);
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

    fn assign(&mut self, name: &str, v: Value) {
        if let Some(top) = self.scopes.last_mut() {
            top.insert(name.to_string(), v);
        }
    }

    fn lookup(&self, name: &str) -> Option<Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v.clone());
            }
        }
        None
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
            Expr::List(items, _) => {
                let mut vs = Vec::with_capacity(items.len());
                for it in items {
                    vs.push(self.eval_expr(it)?);
                }
                Ok(Value::List(vs))
            }
            Expr::Var(name, span) => self.lookup(name).ok_or(RuntimeError {
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
            Expr::Call { func, args, span } => self.call_func(func, args, span.line, span.col),
        }
    }

    fn eval_index(
        &mut self,
        base: Value,
        index: Value,
        line: usize,
        col: usize,
    ) -> Result<Value, RuntimeError> {
        let err = |msg: &str| RuntimeError { message: msg.to_string(), line, col };
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
            _ => Err(err("only lists and strings support indexing")),
        }
    }

    fn call_func(
        &mut self,
        func: &str,
        args: &[Expr],
        line: usize,
        col: usize,
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
                _ => Err(RuntimeError {
                    message: "len() only supports lists and strings".to_string(),
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
            self.assign(&name, Value::List(lst));
            return Ok(Value::None);
        }

        let f = self.functions.get(func).cloned().ok_or(RuntimeError {
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
        self.call_depth += 1;
        self.scopes.push(HashMap::new());
        for (p, v) in f.params.iter().zip(vals) {
            self.assign(p, v);
        }
        let ret = self.exec_block(&f.body)?;
        self.scopes.pop();
        self.call_depth -= 1;
        match ret {
            None | Some(Flow::Continue) | Some(Flow::Break) => Ok(Value::None),
            Some(Flow::Return(v)) => Ok(v),
        }
    }

    fn apply_binop(
        &self,
        l: Value,
        op: BinOp,
        r: Value,
        line: usize,
        col: usize,
    ) -> Result<Value, RuntimeError> {
        let err = |msg: &str| RuntimeError { message: msg.to_string(), line, col };
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => self.apply_arith(l, op, r, line, col),
            BinOp::Eq => Ok(Value::Bool(values_equal(&l, &r))),
            BinOp::NotEq => Ok(Value::Bool(!values_equal(&l, &r))),
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => self.apply_cmp(l, op, r, line, col),
            BinOp::And | BinOp::Or => Err(err("unreachable")),
        }
    }

    fn apply_arith(
        &self,
        l: Value,
        op: BinOp,
        r: Value,
        line: usize,
        col: usize,
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
        line: usize,
        col: usize,
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

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Int(x), Value::Float(y)) => (*x as f64) == *y,
        (Value::Float(x), Value::Int(y)) => *x == (*y as f64),
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::List(x), Value::List(y)) => x == y,
        (Value::None, Value::None) => true,
        _ => false,
    }
}

pub fn run(prog: &Program) -> Result<Vec<String>, RuntimeError> {
    let mut interp = Interpreter::new();
    interp.run(prog)?;
    Ok(interp.output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_src(src: &str) -> Result<Vec<String>, RuntimeError> {
        let prog = nx_parser::parse_source(src).unwrap_or_else(|e| panic!("{e}"));
        run(&prog)
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
}
