//! LLVM backend for Nexum: typed AST -> LLVM IR text.
//!
//! Principle: NX owns semantics (this crate), LLVM owns machine code.
//! Dynamic values are boxed `%NxVal` (helpers in `runtime.ll`) and all
//! functions share one calling convention, so every boundary -- call
//! arguments, returns, list elements, module globals -- stays boxed.
//! Inside a function the unboxing pass keeps a value whose static type is
//! a known scalar in a bare register (`i64`/`double`/`i1`) and re-boxes
//! it only where a boundary demands it. Opt out with NX_NOUNBOX=1.
//!
//! v0 limits (checked programs only; `nx build` requires `nx check` clean):
//! - Modules and from-imported names resolve at compile time.
//! - No function values in value position (`x = foo`); call them.
//! - No closures over outer function locals.

use std::collections::HashMap;

use nx_ast::{Program, Span};

use crate::core::Gen;

const PRELUDE: &str = include_str!("runtime.ll");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodegenError {
    pub message: String,
    pub line: nx_ast::LineNo,
    pub col: nx_ast::ColNo,
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "codegen error at {}:{}: {}",
            self.line, self.col, self.message
        )
    }
}

impl std::error::Error for CodegenError {}

fn err(span: Span, msg: String) -> CodegenError {
    CodegenError {
        message: msg,
        line: span.line,
        col: span.col,
    }
}

mod arith;
mod call;
mod core;
mod expr;
mod func;
mod mangle;
mod slots;
mod stmt;
mod store;
mod value;

#[cfg(test)]
mod tests;

/// Directory a module's own file lives in, so the checker resolves its
/// relative imports the same way the loader did.
fn module_dir(base: &std::path::Path, module: &str) -> std::path::PathBuf {
    if module == "__main__" {
        return base.to_path_buf();
    }
    match nx_codegen_loader_path(base, module) {
        Some(p) => p,
        None => base.to_path_buf(),
    }
}

fn nx_codegen_loader_path(base: &std::path::Path, module: &str) -> Option<std::path::PathBuf> {
    nx_ast::shape::resolve_module_file(&[base.to_path_buf()], module)
}

pub fn compile_entry(source: &str, base: &std::path::Path) -> Result<String, CodegenError> {
    compile_opts(source, base, std::env::var("NX_NOUNBOX").is_err())
}

/// Compile with an explicit unboxing switch. `compile_entry` reads
/// NX_NOUNBOX; tests use this directly so they do not race on the
/// process environment.
pub fn compile_opts(
    source: &str,
    base: &std::path::Path,
    unbox_on: bool,
) -> Result<String, CodegenError> {
    let mut loader = Loader {
        programs: HashMap::new(),
        order: Vec::new(),
        loading: Vec::new(),
        base: base.to_path_buf(),
    };
    loader.load_main(source)?;
    let order = loader.order.clone();
    // Inferred types drive unboxing and the memory plan. The driver
    // type-checks first, so a failure here means an unreachable path.
    let mut types: HashMap<(String, String), nx_types::FnInfo> = HashMap::new();
    for module in &order {
        if let Some(prog) = loader.programs.get(module) {
            match nx_types::infer_program_for(prog, &module_dir(&loader.base, module), module) {
                Ok(m) => {
                    for (k, v) in m {
                        types.insert(k, v);
                    }
                }
                Err(_) => {
                    return Err(CodegenError {
                        message: "internal: program reached codegen without a clean type check"
                            .to_string(),
                        line: 0,
                        col: 0,
                    })
                }
            }
        }
    }
    // The planner only promotes scalars to Stack when the unboxing pass is
    // actually on, so NX_NOUNBOX gets the pre-unboxing plan too.
    let no_types: HashMap<(String, String), nx_types::FnInfo> = HashMap::new();
    let plan_types = if unbox_on { &types } else { &no_types };
    let plan = nx_mem::plan(&loader.programs, "__main__", plan_types);
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
    let mut g = Gen::new(plan, loader.programs, memo, types);
    g.unbox_on = unbox_on;
    g.emit_prelude();
    g.harvest_layouts();
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
        nx_ast::shape::resolve_module_file(&[self.base.clone()], name)
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
        let dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or(base.to_path_buf());
        let mut deps = Vec::new();
        collect_imports(&prog, &mut deps);
        for dep in deps {
            // The importing file's own dir first, then the entry dir,
            // then NX_PATH -- the only copy that ever searched two bases.
            if let Some(p) =
                nx_ast::shape::resolve_module_file(&[dir.clone(), base.to_path_buf()], &dep)
            {
                out.push(p.clone());
                queue.push(p);
            }
        }
    }
    Ok(out)
}

fn collect_imports(prog: &Program, out: &mut Vec<String>) {
    // Recursive since imports are legal inside function bodies; the
    // loader must not miss a dependency the checker accepted.
    for m in nx_ast::shape::imported_modules(prog) {
        if !out.contains(&m) {
            out.push(m);
        }
    }
}
