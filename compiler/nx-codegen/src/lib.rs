//! LLVM backend for Nexum: typed AST -> LLVM IR text.
//!
//! Principle: NX owns semantics (this crate), LLVM owns machine code.
//! v0 model: every value is a boxed `%NxVal` (helpers in `runtime.ll`),
//! all functions share one calling convention. Unboxing is a future
//! optimization, not a correctness rewrite.
//!
//! v0 limits (checked programs only; `nx build` requires `nx check` clean):
//! - Modules and from-imported names resolve at compile time.
//! - No function values in value position (`x = foo`); call them.
//! - No closures over outer function locals (matches the interpreter).

use std::collections::{HashMap, HashSet};
use nx_ast::{BinOp, Expr, Program, Span, Stmt, UnaryOp};

const PRELUDE: &str = include_str!("runtime.ll");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodegenError {
    pub message: String,
    pub line: usize,
    pub col: usize,
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "codegen error at {}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for CodegenError {}

fn err(span: Span, msg: String) -> CodegenError {
    CodegenError { message: msg, line: span.line, col: span.col }
}

fn mangle_fn(module: &str, name: &str) -> String {
    format!("nx__f_{}__{module}__{name}", name.len())
}

fn mangle_global(module: &str, name: &str) -> String {
    format!("nx__g_{module}__{name}")
}

fn mangle_init(module: &str) -> String {
    format!("nx__init_{module}")
}

fn mangle_done(module: &str) -> String {
    format!("nx__done_{module}")
}

#[cfg(test)]
mod tests {
    use super::compile_entry;

    /// Structural invariant: `define` may only appear at brace depth 0.
    /// (Catches outlined functions emitted mid-body.)
    fn assert_top_level_defines(ir: &str) {
        let mut depth = 0i32;
        for line in ir.lines() {
            let t = line.trim();
            if t.starts_with("define ") {
                assert_eq!(depth, 0, "define inside function body: {t}");
            }
            depth += line.chars().filter(|&c| c == '{').count() as i32;
            depth -= line.chars().filter(|&c| c == '}').count() as i32;
        }
    }

    #[test]
    fn parallel_outlines_at_top_level() {
        let ir = compile_entry(
            "a = 0\nb = 0\nparallel:\n    a = 1\n    b = 2\nprint(a, b)\n",
            std::path::Path::new("."),
        )
        .unwrap();
        assert_top_level_defines(&ir);
    }

    fn memo_gets(ir: &str) -> usize {
        // The prelude *defines* nx_memo_get once; count actual calls.
        ir.matches("call i1 @nx_memo_get").count()
    }

    #[test]
    fn memo_prologue_for_pure_fn() {
        let ir = compile_entry(
            "fn fib(n):\n    if n <= 1:\n        return n\n    else:\n        return fib(n - 1) + fib(n - 2)\nprint(fib(10))\n",
            std::path::Path::new("."),
        )
        .unwrap();
        assert!(memo_gets(&ir) >= 1, "pure fib must consult the cache");
        assert!(ir.contains("call void @nx_memo_put"), "pure fib must populate the cache");
        assert_top_level_defines(&ir);
    }

    #[test]
    fn no_memo_for_printing_fn() {
        let ir = compile_entry(
            "fn f(n):\n    print(n)\n    return n\nprint(f(1))\n",
            std::path::Path::new("."),
        )
        .unwrap();
        assert_eq!(memo_gets(&ir), 0, "printing fn must not be memoized");
    }

    fn free_calls(ir: &str) -> usize {
        // The prelude defines nx_free_val with one self-recursive call.
        ir.matches("call void @nx_free_val").count() - 1
    }

    #[test]
    fn unique_temp_gets_freed() {
        let ir = compile_entry(
            "fn f(n):\n    t = n * 2\n    print(t)\nprint(f(21))\n",
            std::path::Path::new("."),
        )
        .unwrap();
        assert!(free_calls(&ir) >= 1, "Unique local t must be freed");
    }

    #[test]
    fn shared_global_not_freed() {
        let ir = compile_entry("x = [1]\nprint(x)\n", std::path::Path::new(".")).unwrap();
        assert_eq!(free_calls(&ir), 0, "globals must not be freed");
    }
    /// Every `private constant [N x i8] c"..."` in the prelude must have a
    /// matching N (LLVM rejects mismatches; hand-counting is unreliable).
    #[test]
    fn prelude_string_sizes() {
        let mut bad = Vec::new();
        for line in super::PRELUDE.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("@") else { continue };
            let Some(at) = rest.find("private constant [") else { continue };
            let after = &rest[at + "private constant [".len()..];
            let Some(sp) = after.find(" x i8] c\"") else { continue };
            let want: usize = after[..sp].parse().unwrap();
            let mut esc = &after[sp + " x i8] c\"".len()..];
            esc = esc.strip_suffix('"').unwrap();
            // Count LLVM escape sequences (\XX) as one byte each.
            let mut got = 0usize;
            let b = esc.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'\\' {
                    got += 1;
                    i += 3;
                } else {
                    got += 1;
                    i += 1;
                }
            }
            if got != want {
                bad.push(format!("{line} counts {got}, says {want}"));
            }
        }
        assert!(bad.is_empty(), "mismatched string sizes:\n{}", bad.join("\n"));
    }
}

pub fn compile_entry(source: &str, base: &std::path::Path) -> Result<String, CodegenError> {
    let mut loader = Loader {
        programs: HashMap::new(),
        order: Vec::new(),
        loading: Vec::new(),
        base: base.to_path_buf(),
    };
    loader.load_main(source)?;
    let plan = nx_mem::plan(&loader.programs, "__main__");
    // Memo table ids for purity-proven functions (opt out: NX_NOMEMO=1).
    let mut memo: HashMap<(String, String), i64> = HashMap::new();
    if std::env::var("NX_NOMEMO").is_err() {
        if let Ok(ir) = nx_ir::analyze_map(loader.programs.clone()) {
            let mut keys: Vec<_> = ir.funcs.keys().cloned().collect();
            keys.sort();
            for (i, k) in keys.into_iter().enumerate() {
                if nx_ir::memoizable(&ir.funcs[&k].summary) {
                    memo.insert(k, i as i64);
                }
            }
        }
    }
    let order = loader.order.clone();
    let mut g = Gen::new(plan, loader.programs, memo);
    g.emit_prelude();
    for module in &order {
        let prog = g.programs.get(module).cloned().unwrap();
        g.declare_module_fns(module, &prog);
    }
    for module in &order {
        let prog = g.programs.get(module).cloned().unwrap();
        g.emit_module(module, &prog)?;
    }
    g.emit_main();
    Ok(g.finish())
}

struct Loader {
    programs: HashMap<String, Program>,
    order: Vec<String>,
    loading: Vec<String>,
    base: std::path::PathBuf,
}

impl Loader {
    fn load_main(&mut self, source: &str) -> Result<(), CodegenError> {
        let prog = parse(source)?;
        self.insert("__main__".to_string(), prog)
    }

    fn insert(&mut self, name: String, prog: Program) -> Result<(), CodegenError> {
        if self.programs.contains_key(&name) {
            return Ok(());
        }
        if self.loading.contains(&name) {
            return Err(CodegenError {
                message: format!("circular import of '{name}'"),
                line: 1,
                col: 1,
            });
        }
        self.loading.push(name.clone());
        let mut deps = Vec::new();
        collect_imports(&prog, &mut deps);
        for dep in deps {
            let path = self.resolve(&dep).ok_or(CodegenError {
                message: format!("cannot find module '{dep}.nx'"),
                line: 1,
                col: 1,
            })?;
            let src = std::fs::read_to_string(&path).map_err(|e| CodegenError {
                message: format!("cannot read module '{dep}': {e}"),
                line: 1,
                col: 1,
            })?;
            let sub = parse(&src).map_err(|mut e: CodegenError| {
                e.message = format!("in module '{dep}': {}", e.message);
                e
            })?;
            // Resolve nested imports relative to the submodule's own dir.
            let saved = std::mem::replace(
                &mut self.base,
                path.parent().map(|p| p.to_path_buf()).unwrap_or(".".into()),
            );
            let r = self.insert(dep, sub);
            self.base = saved;
            r?;
        }
        self.loading.pop();
        self.order.push(name.clone());
        self.programs.insert(name, prog);
        Ok(())
    }

    fn resolve(&self, name: &str) -> Option<std::path::PathBuf> {
        let file = format!("{name}.nx");
        let mut dirs = vec![self.base.clone()];
        if let Ok(p) = std::env::var("NX_PATH") {
            dirs.extend(std::env::split_paths(&p));
        }
        dirs.into_iter().map(|d| d.join(&file)).find(|p| p.is_file())
    }
}

fn parse(source: &str) -> Result<Program, CodegenError> {
    let tokens = nx_lexer::lex(source).map_err(|e| CodegenError {
        message: e.message,
        line: e.line,
        col: e.col,
    })?;
    nx_parser::parse(tokens).map_err(|e| CodegenError {
        message: e.message,
        line: e.line,
        col: e.col,
    })
}

/// All source files a build depends on: the entry plus every transitively
/// imported `.nx` file. Used for incremental rebuild checks.
pub fn dependencies(
    entry: &std::path::Path,
    base: &std::path::Path,
) -> Result<Vec<std::path::PathBuf>, CodegenError> {
    let mut out = vec![entry.to_path_buf()];
    let mut queue = vec![entry.to_path_buf()];
    let mut seen = std::collections::HashSet::new();
    while let Some(path) = queue.pop() {
        let canon = path.canonicalize().unwrap_or(path.clone());
        if !seen.insert(canon) {
            continue;
        }
        let src = std::fs::read_to_string(&path).map_err(|e| CodegenError {
            message: format!("cannot read {}: {e}", path.display()),
            line: 1,
            col: 1,
        })?;
        let prog = parse(&src)?;
        let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or(base.to_path_buf());
        let mut deps = Vec::new();
        collect_imports(&prog, &mut deps);
        for dep in deps {
            let file = format!("{dep}.nx");
            let mut dirs = vec![dir.clone(), base.to_path_buf()];
            if let Ok(p) = std::env::var("NX_PATH") {
                dirs.extend(std::env::split_paths(&p));
            }
            if let Some(p) = dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file()) {
                out.push(p.clone());
                queue.push(p);
            }
        }
    }
    Ok(out)
}

fn collect_imports(prog: &Program, out: &mut Vec<String>) {    for s in &prog.stmts {
        let m = match s {
            Stmt::Import { module, .. } => Some(module),
            Stmt::FromImport { module, .. } => Some(module),
            _ => None,
        };
        if let Some(m) = m {
            if !out.contains(m) {
                out.push(m.clone());
            }
        }
    }
}

/// How a source name resolves in generated code.
#[derive(Debug, Clone)]
enum Binding {
    Local(String),
    Global(String),
    Module(String),
    ModuleFn(String, String),
}

/// Block termination state (mirrors interpreter Flow, minus values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Term {
    Ret,
    Brk,
    Ctn,
}

struct Gen {
    pre: String,
    top: String,
    out: String,
    plan: nx_mem::Plan,
    programs: HashMap<String, Program>,
    memo: HashMap<(String, String), i64>,
    tmp: u64,
    label: u64,
    strc: u64,
    arity: HashMap<(String, String), usize>,
    modrefs: HashMap<String, String>,
    falias: HashMap<String, (String, String)>,
    locals: HashMap<String, String>,
    globals: HashSet<String>,
    cur_module: String,
    cur_fn: String,
    /// When true, `w()` writes to the top-level buffer (for outlining
    /// task functions in the middle of another function body).
    to_top: bool,
    in_init: bool,
    term: Option<Term>,
    loops: Vec<(String, String)>,
}

impl Gen {
    fn new(
        plan: nx_mem::Plan,
        programs: HashMap<String, Program>,
        memo: HashMap<(String, String), i64>,
    ) -> Self {
        Self {
            pre: String::new(),
            top: String::new(),
            out: String::new(),
            plan,
            programs,
            memo,
            tmp: 0,
            label: 0,
            strc: 0,
            arity: HashMap::new(),
            modrefs: HashMap::new(),
            falias: HashMap::new(),
            locals: HashMap::new(),
            globals: HashSet::new(),
            cur_module: String::new(),
            cur_fn: String::new(),
            to_top: false,
            in_init: false,
            term: None,
            loops: Vec::new(),
        }
    }

    fn emit_prelude(&mut self) {
        self.pre.push_str(PRELUDE);
        self.pre.push('\n');
    }

    fn finish(self) -> String {
        format!("{}{}{}", self.pre, self.top, self.out)
    }

    fn reg(&mut self) -> String {
        self.tmp += 1;
        format!("%t{}", self.tmp)
    }

    fn lab(&mut self, hint: &str) -> String {
        self.label += 1;
        format!("{hint}{}", self.label)
    }

    fn w(&mut self, s: &str) {
        if self.to_top {
            self.top.push_str(s);
            self.top.push('\n');
        } else {
            self.out.push_str(s);
            self.out.push('\n');
        }
    }

    fn declare_module_fns(&mut self, module: &str, prog: &Program) {
        for s in &prog.stmts {
            if let Stmt::Fn { name, params, .. } = s {
                self.arity.insert((module.to_string(), name.clone()), params.len());
                self.globals.insert(format!("{module}\0{name}"));
            }
        }
    }

    fn is_module_fn(&self, module: &str, name: &str) -> bool {
        self.arity.contains_key(&(module.to_string(), name.to_string()))
    }

    fn emit_module(&mut self, module: &str, prog: &Program) -> Result<(), CodegenError> {
        self.cur_module = module.to_string();
        // First pass: module-level variable globals (functions need no globals).
        let mut gvars: Vec<String> = Vec::new();
        for s in &prog.stmts {
            let mut push = |n: &String| {
                if !gvars.contains(n) {
                    gvars.push(n.clone());
                }
            };
            match s {
                Stmt::Assign { name, .. } | Stmt::AssignOp { name, .. } => push(name),
                Stmt::FromImport { names, .. } => {
                    for (name, alias) in names {
                        push(alias.as_ref().unwrap_or(name));
                    }
                }
                Stmt::Import { module: m, alias, .. } => {
                    push(alias.as_ref().unwrap_or(m));
                }
                _ => {}
            }
        }
        for v in &gvars {
            self.globals.insert(format!("{module}\0{v}"));
            self.top.push_str(&format!("@{} = global %NxVal zeroinitializer\n", mangle_global(module, v)));
        }
        let done = mangle_done(module);
        self.top.push_str(&format!("@{done} = global i1 false\n"));
        for s in &prog.stmts {
            if let Stmt::Fn { name, params, body, .. } = s {
                self.emit_fn(module, name, params, body)?;
            }
        }
        // Module init body = top-level statements, guarded for import caching.
        self.in_init = true;
        self.term = None;
        let init = mangle_init(module);
        self.w(&format!("define void @{init}() {{"));
        self.w("entry:");
        let flag = self.reg();
        let run = self.lab("initrun");
        let skip = self.lab("initskip");
        self.w(&format!("  {flag} = load i1, ptr @{done}"));
        self.w(&format!("  br i1 {flag}, label %{skip}, label %{run}"));
        self.w(&format!("{run}:"));
        self.w(&format!("  store i1 true, ptr @{done}"));
        for s in &prog.stmts {
            if matches!(s, Stmt::Fn { .. }) {
                continue;
            }
            self.emit_stmt(s)?;
            if self.term.is_some() {
                // Top-level return/break/continue: rejected by the checker,
                // so `nx build` never reaches here. Stop emitting.
                break;
            }
        }
        if self.term.is_none() {
            self.w("  ret void");
        }
        self.w(&format!("{skip}:"));
        self.w("  ret void");
        self.w("}");
        self.in_init = false;
        self.term = None;
        Ok(())
    }

    fn emit_main(&mut self) {
        self.w("define i32 @main() {");
        self.w("entry:");
        self.w(&format!("  call void @{}()", mangle_init("__main__")));
        self.w("  ret i32 0");
        self.w("}");
    }

    fn emit_fn(
        &mut self,
        module: &str,
        name: &str,
        params: &[String],
        body: &[Stmt],
    ) -> Result<(), CodegenError> {
        let fname = mangle_fn(module, name);
        self.locals.clear();
        self.term = None;
        self.w(&format!("define %NxVal @{fname}(%NxVal* %args, i64 %nargs) {{"));
        self.w("entry:");
        // Memo prologue for purity-proven functions: hit returns cached.
        let fnid = self.memo.get(&(module.to_string(), name.to_string())).copied();
        if let Some(id) = fnid {
            let slot = self.reg();
            let hit = self.reg();
            self.w(&format!("  {slot} = alloca %NxVal"));
            self.w(&format!("  store %NxVal zeroinitializer, ptr {slot}"));
            self.w(&format!("  {hit} = call i1 @nx_memo_get(i64 {id}, ptr %args, i64 %nargs, ptr {slot})"));
            let go = self.lab("mhit");
            let miss = self.lab("mmiss");
            self.w(&format!("  br i1 {hit}, label %{go}, label %{miss}"));
            self.w(&format!("{go}:"));
            let cv = self.reg();
            self.w(&format!("  {cv} = load %NxVal, ptr {slot}"));
            self.w(&format!("  ret %NxVal {cv}"));
            self.w(&format!("{miss}:"));
        }
        for (i, p) in params.iter().enumerate() {
            let slot = self.reg();
            let ep = self.reg();
            let v = self.reg();
            self.w(&format!("  {slot} = alloca %NxVal"));
            self.w(&format!("  {ep} = getelementptr %NxVal, ptr %args, i64 {i}"));
            self.w(&format!("  {v} = load %NxVal, ptr {ep}"));
            self.w(&format!("  store %NxVal {v}, ptr {slot}"));
            self.locals.insert(p.clone(), slot);
        }
        let saved = self.cur_module.clone();
        self.cur_module = module.to_string();
        self.cur_fn = name.to_string();
        for s in body {
            self.emit_stmt(s)?;
            if self.term.is_some() {
                break;
            }
        }
        if self.term.is_none() {
            // NOTE: free_scope/memo need the function's module/name context.
            self.free_scope();
            self.emit_ret(None);
        }
        self.cur_module = saved;
        self.cur_fn = String::new();
        self.w("}");
        self.locals.clear();
        self.term = None;
        Ok(())
    }

    /// Emit `ret` for a value, storing it in the memo cache first when
    /// the current function is memoized.
    fn emit_ret(&mut self, reg: Option<&str>) {
        if let Some(id) = self.memo.get(&(self.cur_module.clone(), self.cur_fn.clone())).copied() {
            if let Some(v) = reg {
                self.w(&format!("  call void @nx_memo_put(i64 {id}, ptr %args, i64 %nargs, %NxVal {v})"));
            }
        }
        match reg {
            Some(v) => self.w(&format!("  ret %NxVal {v}")),
            None => self.w("  ret %NxVal zeroinitializer"),
        }
    }
    fn is_unique(&self, name: &str) -> bool {
        self.locals.contains_key(name)
            && self.plan.alloc_of(&self.cur_module, &self.cur_fn, name) == nx_mem::Alloc::Unique
    }

    /// Free every Unique local currently in scope.
    fn free_scope(&mut self) {
        let mut names: Vec<String> = self.locals.keys().cloned().collect();
        names.sort();
        for n in names {
            if self.plan.alloc_of(&self.cur_module, &self.cur_fn, &n) == nx_mem::Alloc::Unique {
                if let Some(slot) = self.locals.get(&n).cloned() {
                    let v = self.reg();
                    self.w(&format!("  {v} = load %NxVal, ptr {slot}"));
                    self.w(&format!("  call void @nx_free_val(%NxVal {v})"));
                }
            }
        }
    }

    /// Module-global variable, declared on first use.
    fn ensure_global(&mut self, module: &str, name: &str) -> String {
        let g = mangle_global(module, name);
        if self.globals.insert(format!("{module}\0{name}")) {
            self.top.push_str(&format!("@{g} = global %NxVal zeroinitializer\n"));
        }
        format!("@{g}")
    }

    fn ptr_of(&mut self, name: &str) -> Option<String> {
        if let Some(r) = self.locals.get(name).cloned() {
            return Some(r);
        }
        let g = format!("{}\0{name}", self.cur_module);
        if self.globals.contains(&g) {
            return Some(format!("@{}", mangle_global(&self.cur_module, name)));
        }
        None
    }

    fn resolve(&self, name: &str) -> Result<Binding, (String, String)> {
        // Returns Binding or (kind, detail) for precise errors.
        if let Some(r) = self.locals.get(name) {
            return Ok(Binding::Local(r.clone()));
        }
        if let Some(m) = self.modrefs.get(name) {
            return Ok(Binding::Module(m.clone()));
        }
        if let Some((m, f)) = self.falias.get(name) {
            return Ok(Binding::ModuleFn(m.clone(), f.clone()));
        }
        let g = format!("{}\0{name}", self.cur_module);
        if self.globals.contains(&g) {
            return Ok(Binding::Global(mangle_global(&self.cur_module, name)));
        }
        if self.is_module_fn(&self.cur_module, name) {
            return Err(("fn".to_string(), name.to_string()));
        }
        Err(("undef".to_string(), name.to_string()))
    }

    fn emit_stmt(&mut self, stmt: &Stmt) -> Result<(), CodegenError> {
        if self.term.is_some() {
            return Ok(());
        }
        match stmt {
            Stmt::Assign { name, value, span } => {
                match value {
                    Expr::Var(y, _) => {
                        if let Some(m) = self.modrefs.get(y).cloned() {
                            self.modrefs.insert(name.clone(), m);
                        } else {
                            self.modrefs.remove(name);
                        }
                        self.falias.remove(name);
                    }
                    _ => {
                        self.modrefs.remove(name);
                        self.falias.remove(name);
                    }
                }
                let v = self.emit_expr(value)?;
                self.store_name(name, &v, *span)?;
                Ok(())
            }
            Stmt::AssignOp { name, op, value, span } => {
                let rhs = self.emit_expr(value)?;
                let ptr = self.ptr_of(name).ok_or(err(*span, format!("undefined variable '{name}'")))?;
                let cur = self.reg();
                self.w(&format!("  {cur} = load %NxVal, ptr {ptr}"));
                let helper = match op {
                    BinOp::Add => "nx_add",
                    BinOp::Sub => "nx_sub",
                    BinOp::Mul => "nx_mul",
                    BinOp::Div => "nx_div",
                    _ => return Err(err(*span, "not an arithmetic assignment".to_string())),
                };
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @{helper}(%NxVal {cur}, %NxVal {rhs})"));
                if !self.in_init && self.is_unique(name) {
                    self.w(&format!("  call void @nx_free_val(%NxVal {cur})"));
                }
                self.w(&format!("  store %NxVal {r}, ptr {ptr}"));
                Ok(())
            }
            Stmt::Print { values, .. } => {
                let n = values.len();
                if n == 0 {
                    self.w("  call void @nx_print(ptr null, i64 0)");
                    return Ok(());
                }
                let arr = self.reg();
                self.w(&format!("  {arr} = alloca [{n} x %NxVal]"));
                for (i, e) in values.iter().enumerate() {
                    let v = self.emit_expr(e)?;
                    let ep = self.reg();
                    self.w(&format!("  {ep} = getelementptr [{n} x %NxVal], ptr {arr}, i64 0, i64 {i}"));
                    self.w(&format!("  store %NxVal {v}, ptr {ep}"));
                }
                let p0 = self.reg();
                self.w(&format!("  {p0} = getelementptr [{n} x %NxVal], ptr {arr}, i64 0, i64 0"));
                self.w(&format!("  call void @nx_print(ptr {p0}, i64 {n})"));
                Ok(())
            }
            Stmt::If { cond, then_body, elifs, else_body, .. } => {
                self.emit_if_full(cond, then_body, elifs, else_body)
            }
            Stmt::While { cond, body, .. } => {
                let condl = self.lab("wcond");
                let bodyl = self.lab("wbody");
                let endl = self.lab("wend");
                self.w(&format!("  br label %{condl}"));
                self.w(&format!("{condl}:"));
                let c = self.emit_expr(cond)?;
                let b = self.bool_of(&c);
                self.w(&format!("  br i1 {b}, label %{bodyl}, label %{endl}"));
                self.w(&format!("{bodyl}:"));
                self.loops.push((condl.clone(), endl.clone()));
                self.term = None;
                for s in body {
                    self.emit_stmt(s)?;
                    if self.term.is_some() {
                        break;
                    }
                }
                let body_term = self.term.take();
                self.loops.pop();
                match body_term {
                    Some(Term::Ret) => {
                        self.term = Some(Term::Ret);
                        self.w(&format!("{endl}:"));
                        self.w("  unreachable");
                    }
                    Some(_) => {
                        // break/continue already branched; no back-edge.
                        self.w(&format!("{endl}:"));
                        self.term = None;
                    }
                    None => {
                        self.w(&format!("  br label %{condl}"));
                        self.w(&format!("{endl}:"));
                        self.term = None;
                    }
                }
                Ok(())
            }
            Stmt::For { var, iter, body, span } => self.emit_for(var, iter, body, *span),
            Stmt::Fn { .. } => Ok(()),
            Stmt::Return { value, .. } => {
                // Free Unique locals on every exit path, then memoize.
                match value {
                    Some(e) => {
                        let v = self.emit_expr(e)?;
                        self.free_scope();
                        self.emit_ret(Some(&v));
                    }
                    None => {
                        self.free_scope();
                        self.emit_ret(None);
                    }
                }
                self.term = Some(Term::Ret);
                Ok(())
            }
            Stmt::Break { span } => {
                let exit = self.loops.last().map(|(_, e)| e.clone()).ok_or(err(
                    *span,
                    "break outside loop".to_string(),
                ))?;
                self.w(&format!("  br label %{exit}"));
                self.term = Some(Term::Brk);
                Ok(())
            }
            Stmt::Continue { span } => {
                let cond = self.loops.last().map(|(c, _)| c.clone()).ok_or(err(
                    *span,
                    "continue outside loop".to_string(),
                ))?;
                self.w(&format!("  br label %{cond}"));
                self.term = Some(Term::Ctn);
                Ok(())
            }
            Stmt::Import { module, alias, span } => {
                if !self.arity_known_module(module) && !self.module_has_vars(module) {
                    return Err(err(*span, format!("cannot find module '{module}'")));
                }
                let bind = alias.clone().unwrap_or_else(|| module.clone());
                self.modrefs.insert(bind, module.clone());
                self.w(&format!("  call void @{}()", mangle_init(module)));
                Ok(())
            }
            Stmt::FromImport { module, names, span } => {
                if !self.module_known(module) {
                    return Err(err(*span, format!("cannot find module '{module}'")));
                }
                self.w(&format!("  call void @{}()", mangle_init(module)));
                for (name, alias) in names {
                    let bind = alias.clone().unwrap_or_else(|| name.clone());
                    if self.is_module_fn(module, name) {
                        self.falias.insert(bind, (module.clone(), name.clone()));
                    } else {
                        let src = mangle_global(module, name);
                        let v = self.reg();
                        self.w(&format!("  {v} = load %NxVal, ptr @{src}"));
                        self.store_fresh(&bind, &v);
                    }
                }
                Ok(())
            }
            Stmt::Parallel { tasks, span } => self.emit_parallel(tasks, *span),
            Stmt::Expr(e) => {
                self.emit_expr(e)?;
                Ok(())
            }
        }
    }

    /// Store to a name, allocating a local or using the module global.
    fn store_name(&mut self, name: &str, reg: &str, _span: Span) -> Result<(), CodegenError> {
        if self.in_init {
            let g = self.ensure_global(&self.cur_module.clone(), name);
            self.w(&format!("  store %NxVal {reg}, ptr {g}"));
            return Ok(());
        }
        if let Some(slot) = self.locals.get(name).cloned() {
            // Rebinding a Unique local: release the old buffers first.
            if self.is_unique(name) {
                let old = self.reg();
                self.w(&format!("  {old} = load %NxVal, ptr {slot}"));
                self.w(&format!("  call void @nx_free_val(%NxVal {old})"));
            }
            self.w(&format!("  store %NxVal {reg}, ptr {slot}"));
        } else {
            let slot = self.reg();
            self.w(&format!("  {slot} = alloca %NxVal"));
            self.w(&format!("  store %NxVal zeroinitializer, ptr {slot}"));
            self.w(&format!("  store %NxVal {reg}, ptr {slot}"));
            self.locals.insert(name.to_string(), slot);
        }
        Ok(())
    }

    /// Always-allocate store (from-imports, loop vars).
    fn store_fresh(&mut self, name: &str, reg: &str) {
        if self.in_init {
            let g = self.ensure_global(&self.cur_module.clone(), name);
            self.w(&format!("  store %NxVal {reg}, ptr {g}"));
            return;
        }
        let slot = self.reg();
        self.w(&format!("  {slot} = alloca %NxVal"));
        self.w(&format!("  store %NxVal zeroinitializer, ptr {slot}"));
        self.w(&format!("  store %NxVal {reg}, ptr {slot}"));
        self.locals.insert(name.to_string(), slot);
    }

    fn arity_known_module(&self, module: &str) -> bool {
        self.arity.keys().any(|(m, _)| m == module)
    }

    fn module_has_vars(&self, _module: &str) -> bool {
        // Vars are discovered during emission; the loader guarantees the
        // module exists, so any import we emit for was resolvable.
        // (Unknown members are the checker's job.)
        true
    }

    fn module_known(&self, _module: &str) -> bool {
        true
    }

    fn emit_if_full(
        &mut self,
        cond: &Expr,
        then_body: &[Stmt],
        elifs: &[(Expr, Vec<Stmt>)],
        else_body: &Option<Vec<Stmt>>,
    ) -> Result<(), CodegenError> {
        let endl = self.lab("iend");
        // Every arm gets: cond -> br body/next; body falls to endl or returns.
        // Returns true if the merge is reachable (i.e. some path falls through).
        let mut reachable = false;
        let mut arms: Vec<(&Expr, &[Stmt])> = vec![(cond, then_body)];
        for (ec, eb) in elifs {
            arms.push((ec, eb));
        }
        for (c, b) in arms {
            let bodyl = self.lab("ibranch");
            let next = self.lab("inext");
            let cv = self.emit_expr(c)?;
            let bv = self.bool_of(&cv);
            self.w(&format!("  br i1 {bv}, label %{bodyl}, label %{next}"));
            self.w(&format!("{bodyl}:"));
            self.term = None;
            for s in b {
                self.emit_stmt(s)?;
                if self.term.is_some() {
                    break;
                }
            }
            if self.term.is_none() {
                self.w(&format!("  br label %{endl}"));
                reachable = true;
            }
            self.term = None;
            self.w(&format!("{next}:"));
        }
        if let Some(b) = else_body {
            self.term = None;
            for s in b {
                self.emit_stmt(s)?;
                if self.term.is_some() {
                    break;
                }
            }
            if self.term.is_none() {
                self.w(&format!("  br label %{endl}"));
                reachable = true;
            }
            self.term = None;
        } else {
            // All-false path falls through to the merge.
            self.w(&format!("  br label %{endl}"));
            reachable = true;
        }
        self.w(&format!("{endl}:"));
        if reachable {
            self.term = None;
        } else {
            self.w("  unreachable");
            self.term = Some(Term::Ret);
        }
        Ok(())
    }

    fn bool_of(&mut self, v: &str) -> String {
        let r = self.reg();
        self.w(&format!("  {r} = extractvalue %NxVal {v}, 1"));
        let b = self.reg();
        self.w(&format!("  {b} = trunc i64 {r} to i1"));
        b
    }

    fn emit_for(
        &mut self,
        var: &str,
        iter: &nx_ast::ForIter,
        body: &[Stmt],
        span: Span,
    ) -> Result<(), CodegenError> {
        match iter {
            nx_ast::ForIter::Range { start, end } => {
                let s = self.emit_expr(start)?;
                let e = self.emit_expr(end)?;
                let a = self.reg();
                let b = self.reg();
                self.w(&format!("  {a} = extractvalue %NxVal {s}, 1"));
                self.w(&format!("  {b} = extractvalue %NxVal {e}, 1"));
                let up = self.reg();
                let step = self.reg();
                self.w(&format!("  {up} = icmp sle i64 {a}, {b}"));
                self.w(&format!("  {step} = select i1 {up}, i64 1, i64 -1"));
                let slot = self.reg();
                self.w(&format!("  {slot} = alloca i64"));
                self.w(&format!("  store i64 {a}, ptr {slot}"));
                let condl = self.lab("fcond");
                let bodyl = self.lab("fbody");
                let endl = self.lab("fend");
                self.w(&format!("  br label %{condl}"));
                self.w(&format!("{condl}:"));
                let cur = self.reg();
                let go = self.reg();
                let goup = self.reg();
                let godn = self.reg();
                self.w(&format!("  {cur} = load i64, ptr {slot}"));
                self.w(&format!("  {goup} = icmp slt i64 {cur}, {b}"));
                self.w(&format!("  {godn} = icmp sgt i64 {cur}, {b}"));
                self.w(&format!("  {go} = select i1 {up}, i1 {goup}, i1 {godn}"));
                self.w(&format!("  br i1 {go}, label %{bodyl}, label %{endl}"));
                self.w(&format!("{bodyl}:"));
                let vs = self.reg();
                self.w(&format!("  {vs} = call %NxVal @nx_int(i64 {cur})"));
                self.store_fresh(var, &vs);
                self.loops.push((condl.clone(), endl.clone()));
                self.term = None;
                for st in body {
                    self.emit_stmt(st)?;
                    if self.term.is_some() {
                        break;
                    }
                }
                let bt = self.term.take();
                self.loops.pop();
                match bt {
                    Some(Term::Ret) => {
                        self.term = Some(Term::Ret);
                        self.w(&format!("{endl}:"));
                        self.w("  unreachable");
                    }
                    Some(_) => {
                        self.w(&format!("{endl}:"));
                        self.term = None;
                    }
                    None => {
                        let cur3 = self.reg();
                        let nxt = self.reg();
                        self.w(&format!("  {cur3} = load i64, ptr {slot}"));
                        self.w(&format!("  {nxt} = add i64 {cur3}, {step}"));
                        self.w(&format!("  store i64 {nxt}, ptr {slot}"));
                        self.w(&format!("  br label %{condl}"));
                        self.w(&format!("{endl}:"));
                        self.term = None;
                    }
                }
                let _ = span;
                Ok(())
            }
            nx_ast::ForIter::Each(e) => {
                let v = self.emit_expr(e)?;
                let len = self.reg();
                let n = self.reg();
                self.w(&format!("  {len} = call %NxVal @nx_len(%NxVal {v})"));
                self.w(&format!("  {n} = extractvalue %NxVal {len}, 1"));
                let islot = self.reg();
                self.w(&format!("  {islot} = alloca i64"));
                self.w(&format!("  store i64 0, ptr {islot}"));
                let condl = self.lab("econd");
                let bodyl = self.lab("ebody");
                let endl = self.lab("eend");
                self.w(&format!("  br label %{condl}"));
                self.w(&format!("{condl}:"));
                let i = self.reg();
                let go = self.reg();
                self.w(&format!("  {i} = load i64, ptr {islot}"));
                self.w(&format!("  {go} = icmp slt i64 {i}, {n}"));
                self.w(&format!("  br i1 {go}, label %{bodyl}, label %{endl}"));
                self.w(&format!("{bodyl}:"));
                let iv = self.reg();
                let el = self.reg();
                self.w(&format!("  {iv} = call %NxVal @nx_int(i64 {i})"));
                self.w(&format!("  {el} = call %NxVal @nx_index(%NxVal {v}, %NxVal {iv})"));
                self.store_fresh(var, &el);
                self.loops.push((condl.clone(), endl.clone()));
                self.term = None;
                for st in body {
                    self.emit_stmt(st)?;
                    if self.term.is_some() {
                        break;
                    }
                }
                let bt = self.term.take();
                self.loops.pop();
                match bt {
                    Some(Term::Ret) => {
                        self.term = Some(Term::Ret);
                        self.w(&format!("{endl}:"));
                        self.w("  unreachable");
                    }
                    Some(_) => {
                        self.w(&format!("{endl}:"));
                        self.term = None;
                    }
                    None => {
                        let i2 = self.reg();
                        let i3 = self.reg();
                        self.w(&format!("  {i2} = load i64, ptr {islot}"));
                        self.w(&format!("  {i3} = add i64 {i2}, 1"));
                        self.w(&format!("  store i64 {i3}, ptr {islot}"));
                        self.w(&format!("  br label %{condl}"));
                        self.w(&format!("{endl}:"));
                        self.term = None;
                    }
                }
                let _ = span;
                Ok(())
            }
        }
    }

    fn emit_expr(&mut self, expr: &Expr) -> Result<String, CodegenError> {
        match expr {
            Expr::Int(i, _) => {
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_int(i64 {i})"));
                Ok(r)
            }
            Expr::Float(x, _) => {
                let bits = x.to_bits();
                let d = self.reg();
                let r = self.reg();
                self.w(&format!("  {d} = bitcast i64 {bits} to double"));
                self.w(&format!("  {r} = call %NxVal @nx_float(double {d})"));
                Ok(r)
            }
            Expr::Bool(b, _) => {
                let r = self.reg();
                self.w(&format!(
                    "  {r} = call %NxVal @nx_bool(i1 {})",
                    if *b { "true" } else { "false" }
                ));
                Ok(r)
            }
            Expr::Str(s, span) => self.emit_str(s, *span),
            Expr::List(items, _) => {
                let n = items.len() as i64;
                let l = self.reg();
                let p = self.reg();
                self.w(&format!("  {l} = call %NxVal @nx_new_list(i64 {n})"));
                self.w(&format!("  {p} = alloca %NxVal"));
                self.w(&format!("  store %NxVal {l}, ptr {p}"));
                for it in items {
                    let v = self.emit_expr(it)?;
                    self.w(&format!("  call void @nx_listpush(ptr {p}, %NxVal {v})"));
                }
                let out = self.reg();
                self.w(&format!("  {out} = load %NxVal, ptr {p}"));
                Ok(out)
            }
            Expr::Var(name, span) => match self.resolve(name) {
                Ok(Binding::Local(r)) => {
                    let v = self.reg();
                    self.w(&format!("  {v} = load %NxVal, ptr {r}"));
                    Ok(v)
                }
                Ok(Binding::Global(g)) => {
                    let v = self.reg();
                    self.w(&format!("  {v} = load %NxVal, ptr @{g}"));
                    Ok(v)
                }
                Ok(Binding::Module(m)) => Err(err(
                    *span,
                    format!("module '{m}' is compile-time only in value position"),
                )),
                Ok(Binding::ModuleFn(m, f)) => Err(err(
                    *span,
                    format!("function '{m}.{f}' cannot be used as a value; call it"),
                )),
                Err((kind, _)) if kind == "fn" => Err(err(
                    *span,
                    format!("function '{name}' cannot be used as a value; call it"),
                )),
                Err(_) => Err(err(*span, format!("undefined variable '{name}'"))),
            },
            Expr::Attr { base, attr, span } => {
                if let Expr::Var(m, _) = base.as_ref() {
                    if let Some(module) = self.modrefs.get(m).cloned() {
                        if self.is_module_fn(&module, attr) {
                            return Err(err(
                                *span,
                                format!("function '{module}.{attr}' cannot be used as a value; call it"),
                            ));
                        }
                        let g = mangle_global(&module, attr);
                        let v = self.reg();
                        self.w(&format!("  {v} = load %NxVal, ptr @{g}"));
                        return Ok(v);
                    }
                }
                Err(err(*span, "only direct module.attribute access is supported".to_string()))
            }
            Expr::Index { base, index, .. } => {
                let b = self.emit_expr(base)?;
                let ix = self.emit_expr(index)?;
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_index(%NxVal {b}, %NxVal {ix})"));
                Ok(r)
            }
            Expr::Unary { op, expr, .. } => {
                let v = self.emit_expr(expr)?;
                let r = self.reg();
                match op {
                    UnaryOp::Neg => self.w(&format!("  {r} = call %NxVal @nx_neg(%NxVal {v})")),
                    UnaryOp::Not => self.w(&format!("  {r} = call %NxVal @nx_not(%NxVal {v})")),
                }
                Ok(r)
            }
            Expr::Binary { left, op, right, span } => {
                if matches!(op, BinOp::And | BinOp::Or) {
                    return self.emit_logic(left, *op, right);
                }
                let l = self.emit_expr(left)?;
                let r = self.emit_expr(right)?;
                let out = self.reg();
                match op {
                    BinOp::Add => self.w(&format!("  {out} = call %NxVal @nx_add(%NxVal {l}, %NxVal {r})")),
                    BinOp::Sub => self.w(&format!("  {out} = call %NxVal @nx_sub(%NxVal {l}, %NxVal {r})")),
                    BinOp::Mul => self.w(&format!("  {out} = call %NxVal @nx_mul(%NxVal {l}, %NxVal {r})")),
                    BinOp::Div => self.w(&format!("  {out} = call %NxVal @nx_div(%NxVal {l}, %NxVal {r})")),
                    BinOp::Eq => self.w(&format!("  {out} = call %NxVal @nx_eq(%NxVal {l}, %NxVal {r})")),
                    BinOp::NotEq => {
                        let c = self.reg();
                        self.w(&format!("  {c} = call %NxVal @nx_eq(%NxVal {l}, %NxVal {r})"));
                        self.w(&format!("  {out} = call %NxVal @nx_not(%NxVal {c})"));
                    }
                    BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                        let pred = match op {
                            BinOp::Lt => "slt",
                            BinOp::LtEq => "sle",
                            BinOp::Gt => "sgt",
                            _ => "sge",
                        };
                        let c = self.reg();
                        let b = self.reg();
                        self.w(&format!("  {c} = call i32 @nx_cmp(%NxVal {l}, %NxVal {r})"));
                        self.w(&format!("  {b} = icmp {pred} i32 {c}, 0"));
                        self.w(&format!("  {out} = call %NxVal @nx_bool(i1 {b})"));
                    }
                    BinOp::And | BinOp::Or => unreachable!(),
                }
                let _ = span;
                Ok(out)
            }
            Expr::Call { callee, args, span } => self.emit_call(callee, args, *span),
        }
    }

    fn emit_str(&mut self, s: &str, _span: Span) -> Result<String, CodegenError> {
        let bytes = s.as_bytes();
        let n = bytes.len();
        let id = self.strc;
        self.strc += 1;
        let mut esc = String::new();
        for b in bytes {
            match b {
                b'"' => esc.push_str("\\22"),
                b'\\' => esc.push_str("\\5C"),
                b'\n' => esc.push_str("\\0A"),
                32..=126 => esc.push(*b as char),
                _ => esc.push_str(&format!("\\{b:02X}")),
            }
        }
        if n == 0 {
            self.top.push_str(&format!("@.nxstr.{id} = private constant [1 x i8] zeroinitializer\n"));
        } else {
            self.top.push_str(&format!("@.nxstr.{id} = private constant [{n} x i8] c\"{esc}\"\n"));
        }
        let rawlen = if n == 0 { 1 } else { n };
        let p = self.reg();
        let r = self.reg();
        self.w(&format!("  {p} = getelementptr [{rawlen} x i8], ptr @.nxstr.{id}, i64 0, i64 0"));
        self.w(&format!("  {r} = call %NxVal @nx_str(ptr {p}, i64 {n})"));
        Ok(r)
    }

    fn emit_logic(&mut self, left: &Expr, op: BinOp, right: &Expr) -> Result<String, CodegenError> {
        // Diamond with a single deciding branch:
        //   br i1 <decide>, label %short, label %rhs     (and)
        //   br i1 <decide>, label %rhs, label %short     (or)
        // short: br merge      (result = decided value)
        // rhs:   <right> -> rb; br merge
        // merge: phi [decided, short], [rb, rhs]
        let l = self.emit_expr(left)?;
        let lb = self.bool_of(&l);
        let rhs = self.lab("rhs");
        let short = self.lab("short");
        let merge = self.lab("merge");
        match op {
            BinOp::And => self.w(&format!("  br i1 {lb}, label %{rhs}, label %{short}")),
            _ => self.w(&format!("  br i1 {lb}, label %{short}, label %{rhs}")),
        }
        let decided = match op {
            BinOp::And => "false",
            _ => "true",
        };
        self.w(&format!("{short}:"));
        self.w(&format!("  br label %{merge}"));
        self.w(&format!("{rhs}:"));
        let rv = self.emit_expr(right)?;
        let rb = self.bool_of(&rv);
        // RHS is an expression: it cannot terminate (no return/break inside).
        self.w(&format!("  br label %{merge}"));
        self.w(&format!("{merge}:"));
        let phi = self.reg();
        self.w(&format!("  {phi} = phi i1 [{decided}, %{short}], [{rb}, %{rhs}]"));
        let out = self.reg();
        self.w(&format!("  {out} = call %NxVal @nx_bool(i1 {phi})"));
        Ok(out)
    }

    fn emit_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        span: Span,
    ) -> Result<String, CodegenError> {
        if let Expr::Var(name, _) = callee {
            if name == "len" {
                if args.len() != 1 {
                    return Err(err(span, "len() expects 1 argument".to_string()));
                }
                let a = self.emit_expr(&args[0])?;
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_len(%NxVal {a})"));
                return Ok(r);
            }
            if name == "push" {
                if args.len() != 2 {
                    return Err(err(span, "push() expects 2 arguments".to_string()));
                }
                let ptr = match &args[0] {
                    Expr::Var(n, _) => self.ptr_of(n).ok_or(err(
                        span,
                        "push() first argument must be a list variable".to_string(),
                    ))?,
                    _ => {
                        return Err(err(
                            span,
                            "push() first argument must be a list variable".to_string(),
                        ))
                    }
                };
                let v = self.emit_expr(&args[1])?;
                self.w(&format!("  call void @nx_listpush(ptr {ptr}, %NxVal {v})"));
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_none()"));
                return Ok(r);
            }
            let cur = self.cur_module.clone();
            if self.arity.contains_key(&(cur.clone(), name.clone())) {
                return self.emit_direct(&cur, name, args);
            }
            if let Some((m, f)) = self.falias.get(name).cloned() {
                return self.emit_direct(&m, &f, args);
            }
            return Err(err(span, format!("unknown function '{name}'")));
        }
        if let Expr::Attr { base, attr, .. } = callee {
            if let Expr::Var(m, _) = base.as_ref() {
                if let Some(module) = self.modrefs.get(m).cloned() {
                    if self.is_module_fn(&module, attr) {
                        return self.emit_direct(&module, attr, args);
                    }
                    return Err(err(span, format!("'{attr}' is not a function of '{module}'")));
                }
            }
            return Err(err(span, "only direct module.attr() calls are supported".to_string()));
        }
        Err(err(span, "only direct calls are supported".to_string()))
    }

    fn emit_direct(
        &mut self,
        module: &str,
        name: &str,
        args: &[Expr],
    ) -> Result<String, CodegenError> {
        let fname = mangle_fn(module, name);
        let n = args.len();
        let arr = self.reg();
        self.w(&format!("  {arr} = alloca [{n} x %NxVal]"));
        for (i, a) in args.iter().enumerate() {
            let v = self.emit_expr(a)?;
            let ep = self.reg();
            self.w(&format!("  {ep} = getelementptr [{n} x %NxVal], ptr {arr}, i64 0, i64 {i}"));
            self.w(&format!("  store %NxVal {v}, ptr {ep}"));
        }
        let p0 = self.reg();
        if n == 0 {
            self.w(&format!("  {p0} = inttoptr i64 0 to ptr"));
        } else {
            self.w(&format!("  {p0} = getelementptr [{n} x %NxVal], ptr {arr}, i64 0, i64 0"));
        }
        let r = self.reg();
        self.w(&format!("  {r} = call %NxVal @{fname}(ptr {p0}, i64 {n})"));
        Ok(r)
    }

    fn emit_parallel(&mut self, tasks: &[Stmt], span: Span) -> Result<(), CodegenError> {
        if tasks.is_empty() {
            return Ok(());
        }
        let module = self.cur_module.clone();
        let locals: HashSet<String> = self.locals.keys().cloned().collect();
        let sums = nx_ir::task_summaries(&self.programs, &module, &locals, tasks).map_err(|e| {
            CodegenError { message: e.message, line: span.line, col: span.col }
        })?;
        for batch in nx_ir::partition(&sums) {
            if batch.len() == 1 {
                self.emit_stmt(&tasks[batch[0]])?;
                if self.term.is_some() {
                    // Return inside a task: rejected by the checker.
                    return Err(err(span, "return inside parallel task is not supported".to_string()));
                }
                continue;
            }
            #[cfg(windows)]
            {
                // No portable thread primitive in hand-written IR for
                // MSVC targets in v0: run the batch in program order.
                for &i in &batch {
                    self.emit_stmt(&tasks[i])?;
                    if self.term.is_some() {
                        return Err(err(span, "return inside parallel task is not supported".to_string()));
                    }
                }
            }
            #[cfg(not(windows))]
            {
                self.emit_threaded_batch(&module, tasks, &batch, span)?;
            }
        }
        Ok(())
    }

    #[cfg(not(windows))]
    fn emit_threaded_batch(
        &mut self,
        module: &str,
        tasks: &[Stmt],
        batch: &[usize],
        span: Span,
    ) -> Result<(), CodegenError> {
        // Snapshot the enclosing locals each task reads (read-only sharing).
        let mut reads: Vec<String> = Vec::new();
        for &i in batch {
            collect_outer_reads(&tasks[i], &self.locals, &mut reads);
        }
        let site = self.lab("par");
        let mut snap_globals = Vec::new();
        for (k, name) in reads.iter().enumerate() {
            let g = format!("nx__snap_{site}_{k}");
            self.top.push_str(&format!("@{g} = global %NxVal zeroinitializer\n"));
            let v = self.emit_expr(&Expr::Var(name.clone(), span))?;
            self.w(&format!("  store %NxVal {v}, ptr @{g}"));
            snap_globals.push(g);
        }
        // Outline each task; save ambient codegen state around it.
        let saved_locals = self.locals.clone();
        let saved_modrefs = self.modrefs.clone();
        let saved_falias = self.falias.clone();
        let saved_loops = std::mem::take(&mut self.loops);
        let mut fnames = Vec::new();
        for &i in batch {
            let fname = format!("nx__task_{site}_{i}");
            self.locals.clear();
            self.term = None;
            self.to_top = true;
            self.w(&format!("define ptr @{fname}(ptr %_) {{"));
            self.w("entry:");
            // Rehydrate snapshot reads as task locals.
            for (k, name) in reads.iter().enumerate() {
                let slot = self.reg();
                let v = self.reg();
                self.w(&format!("  {slot} = alloca %NxVal"));
                self.w(&format!("  store %NxVal zeroinitializer, ptr {slot}"));
                self.w(&format!("  {v} = load %NxVal, ptr @{}", snap_globals[k]));
                self.w(&format!("  store %NxVal {v}, ptr {slot}"));
                self.locals.insert(name.clone(), slot);
            }
            self.emit_stmt(&tasks[i])?;
            if self.term.is_some() {
                return Err(err(span, "return inside parallel task is not supported".to_string()));
            }
            self.w("  ret ptr null");
            self.w("}");
            self.to_top = false;
            fnames.push(fname);
        }
        self.locals = saved_locals;
        self.modrefs = saved_modrefs;
        self.falias = saved_falias;
        self.loops = saved_loops;
        self.term = None;
        // Spawn all, join all.
        let mut tids = Vec::new();
        for fname in &fnames {
            let tid = self.reg();
            self.w(&format!("  {tid} = alloca i64"));
            self.w(&format!("  store i64 0, ptr {tid}"));
            self.w(&format!("  call i32 @pthread_create(ptr {tid}, ptr null, ptr @{fname}, ptr null)"));
            tids.push(tid);
        }
        for tid in tids {
            let t = self.reg();
            self.w(&format!("  {t} = load i64, ptr {tid}"));
            self.w(&format!("  call i32 @pthread_join(i64 {t}, ptr null)"));
        }
        let _ = module;
        Ok(())
    }
}

/// Enclosing-local names read anywhere inside a task statement.
#[cfg(not(windows))]
fn collect_outer_reads(s: &Stmt, locals: &HashMap<String, String>, out: &mut Vec<String>) {
    match s {
        Stmt::Assign { value, .. } => collect_expr_reads(value, locals, out),
        Stmt::AssignOp { name, value, .. } => {
            if locals.contains_key(name) && !out.contains(name) {
                out.push(name.clone());
            }
            collect_expr_reads(value, locals, out);
        }
        Stmt::Print { values, .. } => {
            for v in values {
                collect_expr_reads(v, locals, out);
            }
        }
        Stmt::If { cond, then_body, elifs, else_body, .. } => {
            collect_expr_reads(cond, locals, out);
            for t in then_body {
                collect_outer_reads(t, locals, out);
            }
            for (_, b) in elifs {
                for t in b {
                    collect_outer_reads(t, locals, out);
                }
            }
            if let Some(b) = else_body {
                for t in b {
                    collect_outer_reads(t, locals, out);
                }
            }
        }
        Stmt::While { cond, body, .. } => {
            collect_expr_reads(cond, locals, out);
            for t in body {
                collect_outer_reads(t, locals, out);
            }
        }
        Stmt::For { var, iter, body, .. } => {
            match iter {
                nx_ast::ForIter::Range { start, end } => {
                    collect_expr_reads(start, locals, out);
                    collect_expr_reads(end, locals, out);
                }
                nx_ast::ForIter::Each(e) => collect_expr_reads(e, locals, out),
            }
            // The loop var shadows; reads of it inside are task-local.
            let mut inner = locals.clone();
            inner.remove(var);
            for t in body {
                collect_outer_reads(t, &inner, out);
            }
        }
        Stmt::Return { value, .. } => {
            if let Some(e) = value {
                collect_expr_reads(e, locals, out);
            }
        }
        Stmt::Parallel { tasks, .. } => {
            for t in tasks {
                collect_outer_reads(t, locals, out);
            }
        }
        Stmt::Expr(e) => collect_expr_reads(e, locals, out),
        _ => {}
    }
}

#[cfg(not(windows))]
fn collect_expr_reads(e: &Expr, locals: &HashMap<String, String>, out: &mut Vec<String>) {    match e {
        Expr::Var(n, _) => {
            if locals.contains_key(n) && !out.contains(n) {
                out.push(n.clone());
            }
        }
        Expr::Attr { base, .. } => collect_expr_reads(base, locals, out),
        Expr::Index { base, index, .. } => {
            collect_expr_reads(base, locals, out);
            collect_expr_reads(index, locals, out);
        }
        Expr::List(items, _) => {
            for it in items {
                collect_expr_reads(it, locals, out);
            }
        }
        Expr::Unary { expr, .. } => collect_expr_reads(expr, locals, out),
        Expr::Binary { left, right, .. } => {
            collect_expr_reads(left, locals, out);
            collect_expr_reads(right, locals, out);
        }
        Expr::Call { callee, args, .. } => {
            collect_expr_reads(callee, locals, out);
            for a in args {
                collect_expr_reads(a, locals, out);
            }
        }
        _ => {}
    }
}
