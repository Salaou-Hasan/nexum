//! Nexum IR v1: effect + dependency summaries per function.
//!
//! Tracks only *shared* state (module globals, heap lists). Function
//! locals are private by construction and never appear here.
//! Unknown/dynamic targets go `opaque` (may touch everything) —
//! never silently Pure. Consumed by stage 16 (parallel scheduler).

use std::collections::{BTreeSet, HashMap, HashSet};
use nx_ast::{Expr, Program, Stmt};

/// A module-global place: (module, name).
pub type Place = (String, String);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub reads: BTreeSet<Place>,
    pub writes: BTreeSet<Place>,
    pub prints: bool,
    pub heap: bool,
    pub opaque: bool,
}

impl Summary {
    fn merge(&mut self, other: &Summary) -> bool {
        let mut changed = false;
        for r in &other.reads {
            changed |= self.reads.insert(r.clone());
        }
        for w in &other.writes {
            changed |= self.writes.insert(w.clone());
        }
        changed |= !self.prints && other.prints;
        self.prints |= other.prints;
        changed |= !self.heap && other.heap;
        self.heap |= other.heap;
        changed |= !self.opaque && other.opaque;
        self.opaque |= other.opaque;
        changed
    }
}

impl std::fmt::Display for Summary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let show = |s: &BTreeSet<Place>| {
            let mut v: Vec<String> =
                s.iter().map(|(m, n)| format!("{m}.{n}")).collect();
            v.sort();
            format!("{{{}}}", v.join(", "))
        };
        write!(
            f,
            "reads={} writes={} prints={} heap={} opaque={}",
            show(&self.reads),
            show(&self.writes),
            self.prints,
            self.heap,
            self.opaque
        )
    }
}

#[derive(Debug, Clone)]
pub struct FuncIr {
    pub params: Vec<String>,
    pub summary: Summary,
}

#[derive(Debug, Clone, Default)]
pub struct Ir {
    pub funcs: HashMap<Place, FuncIr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrError {
    pub message: String,
}

impl std::fmt::Display for IrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ir error: {}", self.message)
    }
}

impl std::error::Error for IrError {}

/// Analyze a whole program (entry source + its imports).
pub fn analyze(source: &str, base: &std::path::Path) -> Result<Ir, IrError> {
    let mut loader = Loader {
        programs: HashMap::new(),
        loading: Vec::new(),
        base: base.to_path_buf(),
    };
    loader.load("__main__".to_string(), source)?;
    // Index functions.
    let mut params: HashMap<Place, Vec<String>> = HashMap::new();
    let mut bodies: HashMap<Place, Vec<Stmt>> = HashMap::new();
    for (module, prog) in &loader.programs {
        index_fns(module, &prog.stmts, &mut params, &mut bodies);
        // Top-level statements form a synthetic entry per module so
        // scripts without functions still show their effects.
        let top: Vec<Stmt> = prog
            .stmts
            .iter()
            .filter(|s| !matches!(s, Stmt::Fn { .. }))
            .cloned()
            .collect();
        params.insert((module.clone(), "<top>".to_string()), Vec::new());
        bodies.insert((module.clone(), "<top>".to_string()), top);
    }
    // Static environment per module: top-level imports/aliases, shared by
    // every function defined in it.
    let mut menvs: HashMap<String, Env> = HashMap::new();
    for (module, prog) in &loader.programs {
        menvs.insert(module.clone(), Env::for_body(module, &prog.stmts));
    }
    // Fixpoint over call graph (summaries only grow).
    let mut sums: HashMap<Place, Summary> = HashMap::new();
    for k in bodies.keys() {
        sums.insert(k.clone(), Summary::default());
    }
    loop {
        let mut fresh = Vec::new();
        {
            let cx = Cx { bodies: &bodies, sums: &sums };
            for (key, body) in &bodies {
                let mut s = Summary::default();
                summarize(&key.0, body, params[key].as_slice(), &menvs[&key.0], &cx, &mut s);
                fresh.push((key.clone(), s));
            }
        }
        let mut changed = false;
        for (key, s) in fresh {
            if sums.get_mut(&key).map(|s0| s0.merge(&s)).unwrap_or(false) {
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut ir = Ir::default();
    for (key, summary) in sums {
        ir.funcs.insert(
            key.clone(),
            FuncIr { params: params[&key].clone(), summary },
        );
    }
    Ok(ir)
}

#[derive(Debug, Clone, Default)]
struct Env {
    /// import-bound name -> module
    mods: HashMap<String, String>,
    /// from-bound name -> (module, member)
    aliases: HashMap<String, (String, String)>,
    /// names bound locally at top level (masks globals for callees below)
    bound: HashSet<String>,
}

impl Env {
    fn for_body(_module: &str, body: &[Stmt]) -> Self {
        let mut env = Env::default();
        for s in body {
            match s {
                Stmt::Import { module: m, alias, .. } => {
                    env.mods.insert(alias.clone().unwrap_or_else(|| m.clone()), m.clone());
                }
                Stmt::FromImport { module: m, names, .. } => {
                    for (n, a) in names {
                        env.aliases.insert(
                            a.clone().unwrap_or_else(|| n.clone()),
                            (m.clone(), n.clone()),
                        );
                    }
                }
                Stmt::Assign { name, .. } => {
                    env.bound.insert(name.clone());
                }
                _ => {}
            }
        }
        env
    }
}

fn index_fns(
    module: &str,
    stmts: &[Stmt],
    params: &mut HashMap<Place, Vec<String>>,
    bodies: &mut HashMap<Place, Vec<Stmt>>,
) {
    for s in stmts {
        match s {
            Stmt::Fn { name, params: ps, body, .. } => {
                params.insert((module.to_string(), name.clone()), ps.clone());
                bodies.insert((module.to_string(), name.clone()), body.clone());
                index_fns(module, body, params, bodies);
            }
            Stmt::If { then_body, elifs, else_body, .. } => {
                index_fns(module, then_body, params, bodies);
                for (_, b) in elifs {
                    index_fns(module, b, params, bodies);
                }
                if let Some(b) = else_body {
                    index_fns(module, b, params, bodies);
                }
            }
            Stmt::While { body, .. } | Stmt::For { body, .. } => {
                index_fns(module, body, params, bodies);
            }
            _ => {}
        }
    }
}

/// Names assigned anywhere in a body (flow-insensitive locals).
fn assigned(body: &[Stmt], out: &mut HashSet<String>) {
    for s in body {
        match s {
            Stmt::Assign { name, .. } => {
                out.insert(name.clone());
            }
            Stmt::For { var, body, .. } => {
                out.insert(var.clone());
                assigned(body, out);
            }
            Stmt::If { then_body, elifs, else_body, .. } => {
                assigned(then_body, out);
                for (_, b) in elifs {
                    assigned(b, out);
                }
                if let Some(b) = else_body {
                    assigned(b, out);
                }
            }
            Stmt::While { body, .. } => assigned(body, out),
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn summarize(
    module: &str,
    body: &[Stmt],
    params: &[String],
    env: &Env,
    cx: &Cx,
    out: &mut Summary,
) {
    let mut locals: HashSet<String> = params.iter().cloned().collect();
    let mut asn = HashSet::new();
    assigned(body, &mut asn);
    locals.extend(asn);
    // from-bound and import-bound names are local bindings too.
    locals.extend(env.aliases.keys().cloned());
    locals.extend(env.mods.keys().cloned());
    stmts(module, body, &locals, env, cx, out);
}

struct Cx<'a> {
    bodies: &'a HashMap<Place, Vec<Stmt>>,
    sums: &'a HashMap<Place, Summary>,
}

fn stmts(
    module: &str,
    body: &[Stmt],
    locals: &HashSet<String>,
    env: &Env,
    cx: &Cx,
    out: &mut Summary,
) {
    for s in body {
        stmt(module, s, locals, env, cx, out);
    }
}

fn stmt(
    module: &str,
    s: &Stmt,
    locals: &HashSet<String>,
    env: &Env,
    cx: &Cx,
    out: &mut Summary,
) {
    match s {
        Stmt::Assign { name, value, .. } => {
            expr(module, value, locals, env, cx, out);
            if !locals.contains(name) {
                out.writes.insert((module.to_string(), name.clone()));
            }
        }
        Stmt::AssignOp { name, value, .. } => {
            expr(module, value, locals, env, cx, out);
            if locals.contains(name) {
                // read-modify-write of a local: no shared traffic.
            } else {
                out.reads.insert((module.to_string(), name.clone()));
                out.writes.insert((module.to_string(), name.clone()));
            }
        }
        Stmt::Print { values, .. } => {
            out.prints = true;
            for v in values {
                expr(module, v, locals, env, cx, out);
            }
        }
        Stmt::If { cond, then_body, elifs, else_body, .. } => {
            expr(module, cond, locals, env, cx, out);
            stmts(module, then_body, locals, env, cx, out);
            for (c, b) in elifs {
                expr(module, c, locals, env, cx, out);
                stmts(module, b, locals, env, cx, out);
            }
            if let Some(b) = else_body {
                stmts(module, b, locals, env, cx, out);
            }
        }
        Stmt::While { cond, body, .. } => {
            expr(module, cond, locals, env, cx, out);
            stmts(module, body, locals, env, cx, out);
        }
        Stmt::For { var, iter, body, .. } => {
            match iter {
                nx_ast::ForIter::Range { start, end } => {
                    expr(module, start, locals, env, cx, out);
                    expr(module, end, locals, env, cx, out);
                }
                nx_ast::ForIter::Each(e) => expr(module, e, locals, env, cx, out),
            }
            let mut inner = locals.clone();
            inner.insert(var.clone());
            stmts(module, body, &inner, env, cx, out);
        }
        Stmt::Fn { .. } => {}
        Stmt::Return { value, .. } => {
            if let Some(e) = value {
                expr(module, e, locals, env, cx, out);
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Import { .. } => {}
        Stmt::FromImport { .. } => {}
        Stmt::Expr(e) => {
            expr(module, e, locals, env, cx, out);
        }
    }
}

fn expr(
    module: &str,
    e: &Expr,
    locals: &HashSet<String>,
    env: &Env,
    cx: &Cx,
    out: &mut Summary,
) {
    match e {
        Expr::Var(name, _) => {
            if locals.contains(name) {
                return;
            }
            // A bare function name is a reference, not a data read.
            if cx.bodies.contains_key(&(module.to_string(), name.clone())) {
                return;
            }
            out.reads.insert((module.to_string(), name.clone()));
        }
        Expr::Attr { base, attr, .. } => {
            if let Expr::Var(m, _) = base.as_ref() {
                if let Some(target) = env.mods.get(m) {
                    out.reads.insert((target.clone(), attr.clone()));
                    return;
                }
            }
            expr(module, base, locals, env, cx, out);
        }
        Expr::Index { base, index, .. } => {
            expr(module, base, locals, env, cx, out);
            expr(module, index, locals, env, cx, out);
        }
        Expr::List(items, _) => {
            for it in items {
                expr(module, it, locals, env, cx, out);
            }
        }
        Expr::Unary { expr: inner, .. } => expr(module, inner, locals, env, cx, out),
        Expr::Binary { left, right, .. } => {
            expr(module, left, locals, env, cx, out);
            expr(module, right, locals, env, cx, out);
        }
        Expr::Call { callee, args, .. } => {
            for a in args {
                expr(module, a, locals, env, cx, out);
            }
            call(module, callee, env, cx, out);
        }
        _ => {}
    }
}

/// Merge a callee's summary (or conservative flags) into `out`.
fn call(module: &str, callee: &Expr, env: &Env, cx: &Cx, out: &mut Summary) {
    if let Expr::Var(name, _) = callee {
        if name == "len" {
            return;
        }
        if name == "push" {
            out.heap = true;
            return;
        }
        if let Some((m, f)) = env.aliases.get(name) {
            if let Some(s) = cx.sums.get(&(m.clone(), f.clone())) {
                let s = s.clone();
                out.merge(&s);
                return;
            }
        }
        if let Some(s) = cx.sums.get(&(module.to_string(), name.clone())) {
            let s = s.clone();
            out.merge(&s);
            return;
        }
        out.opaque = true;
        return;
    }
    if let Expr::Attr { base, attr, .. } = callee {
        if let Expr::Var(m, _) = base.as_ref() {
            if let Some(target) = env.mods.get(m) {
                if let Some(s) = cx.sums.get(&(target.clone(), attr.clone())) {
                    let s = s.clone();
                    out.merge(&s);
                    return;
                }
            }
        }
        out.opaque = true;
        return;
    }
    out.opaque = true;
}

struct Loader {
    programs: HashMap<String, Program>,
    loading: Vec<String>,
    base: std::path::PathBuf,
}

impl Loader {
    fn load(&mut self, name: String, source: &str) -> Result<(), IrError> {
        if self.programs.contains_key(&name) {
            return Ok(());
        }
        if self.loading.contains(&name) {
            return Err(IrError { message: format!("circular import of '{name}'") });
        }
        let prog = parse(source)?;
        self.loading.push(name.clone());
        let mut deps = Vec::new();
        collect_imports(&prog, &mut deps);
        for dep in deps {
            let path = self.resolve(&dep).ok_or(IrError {
                message: format!("cannot find module '{dep}.nx'"),
            })?;
            let src = std::fs::read_to_string(&path).map_err(|e| IrError {
                message: format!("cannot read module '{dep}': {e}"),
            })?;
            let saved = std::mem::replace(
                &mut self.base,
                path.parent().map(|p| p.to_path_buf()).unwrap_or(".".into()),
            );
            let r = self.load(dep, &src);
            self.base = saved;
            r?;
        }
        self.loading.pop();
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

fn parse(source: &str) -> Result<Program, IrError> {
    let tokens = nx_lexer::lex(source)
        .map_err(|e| IrError { message: e.to_string() })?;
    nx_parser::parse(tokens).map_err(|e| IrError { message: e.to_string() })
}

fn collect_imports(prog: &Program, out: &mut Vec<String>) {
    for s in &prog.stmts {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sums_of(src: &str) -> HashMap<Place, Summary> {
        let ir = analyze(src, std::path::Path::new(".")).unwrap();
        ir.funcs.into_iter().map(|(k, f)| (k, f.summary)).collect()
    }

    #[test]
    fn pure_fn() {
        let m = sums_of("fn add(a, b):\n    return a + b\n");
        let s = &m[&("__main__".to_string(), "add".to_string())];
        assert!(!s.prints && !s.heap && !s.opaque);
        assert!(s.reads.is_empty() && s.writes.is_empty());
    }

    #[test]
    fn print_and_mutation() {
        let m = sums_of("fn f():\n    print(\"hi\")\nfn g():\n    a = [1]\n    push(a, 2)\n");
        assert!(m[&("__main__".to_string(), "f".to_string())].prints);
        assert!(m[&("__main__".to_string(), "g".to_string())].heap);
    }

    #[test]
    fn transitive_union() {
        let m = sums_of("fn inner():\n    print(1)\nfn outer():\n    inner()\n");
        assert!(m[&("__main__".to_string(), "outer".to_string())].prints);
    }

    #[test]
    fn global_read_write() {
        let m = sums_of("x = 1\nfn r():\n    print(x)\nfn w():\n    x = 2\n");
        // Note: `x = 2` inside w is a LOCAL (assigned in body), so no write.
        let r = &m[&("__main__".to_string(), "r".to_string())];
        assert!(r.reads.contains(&("__main__".to_string(), "x".to_string())));
        assert!(r.prints);
    }

    #[test]
    fn unknown_call_is_opaque() {
        let m = sums_of("fn f(g):\n    g(1)\n");
        assert!(m[&("__main__".to_string(), "f".to_string())].opaque);
    }
}
