//! Static type checker for Nexum (`nx check`).
//!
//! v0 rules (advisory only — `nx file.nx` still runs dynamically):
//! - Inferred types, no annotations: Int Float Bool Str List(T) None.
//! - Variables are monomorphic: the first binding fixes the type.
//! - Function params start Unknown; bodies must return consistently.
//! - `import`/`from` are followed into files; members are checked.

use std::collections::HashMap;

use nx_ast::{BinOp, Program};

use crate::checker::Checker;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Str,
    List(Box<Ty>),
    /// A string-keyed (or generally keyed) mapping. The value type is
    /// carried so a homogeneous dict can still be tracked, but it is
    /// advisory: assigning a different value type widens it rather than
    /// failing, because a dict is how you get heterogeneity on purpose.
    Dict(Box<Ty>),
    /// A declared `type`. The name identifies the layout, which is what
    /// lets `p.x` compile to a constant field offset when the type is
    /// known and fall back to a name lookup when it is not.
    Record(String),
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
            Ty::Dict(t) => write!(f, "Dict({t})"),
            Ty::Record(n) => write!(f, "{n}"),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckError {
    pub message: String,
    pub line: nx_ast::LineNo,
    pub col: nx_ast::ColNo,
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "type error at {}:{}: {}",
            self.line, self.col, self.message
        )
    }
}

impl std::error::Error for CheckError {}

#[derive(Debug, Clone, Default)]
struct ModInfo {
    vars: HashMap<String, Ty>,
    funcs: HashMap<String, (Vec<Ty>, Ty)>,
    /// Declared types exported by the module, so `from m import T` can
    /// bring a layout across a module boundary the same way a function
    /// comes across. Field types stay as written and resolve lazily.
    types: HashMap<String, Vec<(String, String)>>,
    /// Methods exported by the module, keyed by (canonical type, method).
    /// `from m import T` brings T's methods along with its layout.
    methods: HashMap<(String, String), MethodInfo>,
}

/// A method as the checker sees it. Parameters exclude the receiver;
/// arity at a call site counts only the explicit arguments.
#[derive(Debug, Clone)]
pub struct MethodInfo {
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub receiver: nx_ast::ReceiverKind,
    pub module: String,
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

mod checker;
mod expr;
mod func;
mod pred;
mod stmt;

#[cfg(test)]
mod tests;

pub use pred::builtin_arity;

pub fn arith_result(l: &Ty, op: BinOp, r: &Ty) -> Option<Ty> {
    use Ty::*;
    // Operator result matrix. This is the single owner of Int/Float promotion.
    if matches!(op, BinOp::Add) {
        if matches!((l, r), (Str, Str)) {
            return Some(Str);
        }
    }
    // `@` only multiplies matrices: a scalar operand is a type error,
    // not an Int or Float result. Without this, the (Int, Int) arm
    // below would accept `2 @ 3`, which the HIR lowering then has to
    // refuse as an internal error -- or worse, the runtime would read
    // an integer as a list header and crash silently.
    if matches!(op, BinOp::MatMul) {
        return match (l, r) {
            (Unknown, _) | (_, Unknown) => Some(Unknown),
            (List(_), List(_)) if matrix_scalar(l).is_some() && matrix_scalar(r).is_some() => {
                Some((*l).clone())
            }
            _ => Option::None,
        };
    }
    // The integral-only operators never widen to Float: `7 % 2.0` is a
    // mistake worth reporting rather than papering over.
    if op.is_bitwise() || matches!(op, BinOp::Mod | BinOp::FloorDiv) {
        return match (l, r) {
            (Unknown, _) | (_, Unknown) => Some(Unknown),
            (Int, Int) => Some(Int),
            _ => Option::None,
        };
    }
    match (l, r) {
        (Unknown, _) | (_, Unknown) => Some(Unknown),
        (Int, Int) => Some(Int),
        (Int, Float) | (Float, Int) | (Float, Float) => Some(Float),
        // List(List(N)) is how NX writes a 2-D numeric matrix, and `*`
        // on two of them is the matrix product. Both sides must be lists
        // of lists of numeric scalars; anything else falls through to the
        // scalar type error below.
        (List(_), List(_)) if op == BinOp::Mul => {
            if matrix_scalar(l).is_some() && matrix_scalar(r).is_some() {
                Some((*l).clone())
            } else {
                Option::None
            }
        }
        _ => Option::None,
    }
}

/// If `t` is `List(List(N))` for a numeric `N`, return `N`. NX has no
/// distinct array type: a 2-D matrix *is* a list of lists, which is exactly
/// how real code writes one.
fn matrix_scalar(t: &Ty) -> Option<&Ty> {
    match t {
        Ty::List(rows) => match &**rows {
            Ty::List(cells) => match &**cells {
                Ty::Int | Ty::Float | Ty::Unknown => Some(&**cells),
                _ => Option::None,
            },
            _ => Option::None,
        },
        _ => Option::None,
    }
}

pub fn check_source(source: &str, base: &std::path::Path) -> Result<(), Vec<CheckError>> {
    let tokens = nx_lexer::lex(source).map_err(|e| {
        vec![CheckError {
            message: e.message,
            line: e.line,
            col: e.col,
        }]
    })?;
    let prog = nx_parser::parse(tokens).map_err(|e| {
        vec![CheckError {
            message: e.message,
            line: e.line,
            col: e.col,
        }]
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
    // Field types may name records declared anywhere in the module.
    c.validate_records();
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
    infer_program_for(prog, base, "__main__")
}

/// `infer_program` for a named module. The backend calls this once per
/// loaded module with its real name: the hardcoding it replaces filed
/// every non-main module's inference under `("__main__", name)`, so a
/// method call on a local inside an imported module missed dispatch and
/// fell through to "only modules, types and builtins support attribute
/// calls". `check_program` keeps its own hardcoding: it only ever runs
/// on entry files, where `__main__` is correct.
pub fn infer_program_for(
    prog: &Program,
    base: &std::path::Path,
    module: &str,
) -> Result<HashMap<(String, String), FnInfo>, Vec<CheckError>> {
    let mut c = Checker {
        base: base.to_path_buf(),
        module_name: module.to_string(),
        ..Default::default()
    };
    c.check_block(&prog.stmts);
    // Field types may name records declared anywhere in the module.
    c.validate_records();
    if !c.errors.is_empty() {
        return Err(c.errors);
    }
    let mut out = std::mem::take(&mut c.inferred);
    // Top-level code shares one flat scope across the module.
    out.insert(
        (module.to_string(), "<top>".to_string()),
        FnInfo {
            locals: c.vars.clone(),
            params: Vec::new(),
            ret: Ty::None,
        },
    );
    Ok(out)
}
