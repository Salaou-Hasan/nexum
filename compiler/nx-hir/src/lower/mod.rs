//! Lowering: checked AST plus checker tables to [`crate::model::HProgram`].
//!
//! Lowering trusts the checker the way codegen does: inputs are checked
//! programs, so every name resolves, every arity lines up, and every
//! operand combination was already accepted or rejected. A lowering
//! failure is therefore an internal error, never a user diagnostic.
//! Wherever a judgment call exists (resolution order, joins, writeback),
//! the code cites the checker rule it mirrors.

use std::collections::HashMap;

use std::path::{Path, PathBuf};

use nx_ast::{Program, Span};

use nx_ast::shape;

use crate::model::HProgram;

#[derive(Debug)]
pub struct LowerError {
    pub message: String,
    pub line: u32,
    pub col: u32,
}

impl std::fmt::Display for LowerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "lower error at {}:{}: {}",
            self.line, self.col, self.message
        )
    }
}

type LResult<T> = Result<T, LowerError>;

fn lerr(span: Span, msg: impl Into<String>) -> LowerError {
    LowerError {
        message: msg.into(),
        line: span.line,
        col: span.col,
    }
}

fn lerr_at(line: u32, col: u32, msg: impl Into<String>) -> LowerError {
    LowerError {
        message: msg.into(),
        line,
        col,
    }
}

/// Lower one entry source plus everything it imports.
pub fn lower_source(source: &str, base: &Path) -> Result<HProgram, LowerError> {
    let mut loader = Loader::default();
    loader.load_main(source, base)?;
    lower_loaded(&loader.programs, &loader.bases, "__main__")
}

/// Lower already-loaded module programs. `entry` names the entry module.
pub fn lower(programs: &HashMap<String, Program>, entry: &str) -> Result<HProgram, LowerError> {
    let mut bases = HashMap::new();
    for name in programs.keys() {
        bases.insert(name.clone(), PathBuf::from("."));
    }
    lower_loaded(programs, &bases, entry)
}

#[derive(Default)]
struct Loader {
    programs: HashMap<String, Program>,
    bases: HashMap<String, PathBuf>,
    loading: Vec<String>,
}

impl Loader {
    fn load_main(&mut self, source: &str, base: &Path) -> Result<(), LowerError> {
        let prog = parse(source)?;
        self.insert("__main__".to_string(), prog, base.to_path_buf())
    }

    fn insert(&mut self, name: String, prog: Program, base: PathBuf) -> Result<(), LowerError> {
        if self.programs.contains_key(&name) {
            return Ok(());
        }
        if self.loading.contains(&name) {
            return Err(lerr_at(1, 1, format!("circular import of '{name}'")));
        }
        self.loading.push(name.clone());
        // Recursive: imports are legal inside function bodies, and the
        // checker binds them there, so a top-level-only scan would miss
        // dependencies and emit references to undeclared globals.
        let deps = shape::imported_modules(&prog);
        self.programs.insert(name.clone(), prog);
        self.bases.insert(name.clone(), base.clone());
        for dep in deps {
            let path = shape::resolve_module_file(&[base.clone()], &dep)
                .ok_or_else(|| lerr_at(1, 1, format!("cannot find module '{dep}.nx'")))?;
            let src = std::fs::read_to_string(&path)
                .map_err(|e| lerr_at(1, 1, format!("cannot read module '{dep}': {e}")))?;
            let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or(".".into());
            let sub = parse(&src).map_err(|e| lerr_at(1, 1, format!("in module '{dep}': {e}")))?;
            self.insert(dep, sub, dir)?;
        }
        self.loading.pop();
        Ok(())
    }
}

fn parse(source: &str) -> Result<Program, LowerError> {
    let tokens = nx_lexer::lex(source).map_err(|e| LowerError {
        message: e.message,
        line: e.line,
        col: e.col,
    })?;
    nx_parser::parse(tokens).map_err(|e| LowerError {
        message: e.message,
        line: e.line,
        col: e.col,
    })
}

mod call;
mod driver;
mod expr;
mod scope;
mod stmt;
mod tables;
mod ty;

use driver::lower_loaded;
