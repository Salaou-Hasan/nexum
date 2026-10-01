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
    analyze_map(loader.programs)
}

/// Analyze already-loaded module programs.
pub fn analyze_map(programs: HashMap<String, Program>) -> Result<Ir, IrError> {
    // Index functions.
    let mut params: HashMap<Place, Vec<String>> = HashMap::new();
    let mut bodies: HashMap<Place, Vec<Stmt>> = HashMap::new();
    for (module, prog) in &programs {
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
    for (module, prog) in &programs {
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
                summarize(&programs, &key.0, body, params[key].as_slice(), &menvs[&key.0], &cx, &mut s);
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
                Stmt::Assign { targets, .. } => {
                    for t in targets {
                        if let nx_ast::Target::Name(n) = t {
                            env.bound.insert(n.clone());
                        }
                    }
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
            Stmt::Assign { targets, .. } => {
                for t in targets {
                    if let nx_ast::Target::Name(n) = t {
                        out.insert(n.clone());
                    }
                }
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

/// Everything a summary walk needs: current module, visible locals,
/// the module's declared globals, import env, and call-graph summaries.
#[derive(Clone)]
struct Scope<'a> {
    module: &'a str,
    locals: HashSet<String>,
    mglobals: HashSet<String>,
    env: &'a Env,
    cx: &'a Cx<'a>,
}

impl<'a> Scope<'a> {
    /// A bare name is shared traffic only if it is a declared module
    /// global (temps assigned inside a task stay task-private).
    fn is_shared(&self, name: &str) -> bool {
        !self.locals.contains(name) && self.mglobals.contains(name)
    }
}

/// Module-global names: top-level Assign/For targets of a module.
fn module_globals(programs: &HashMap<String, Program>, module: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    if let Some(prog) = programs.get(module) {
        top_assigned(&prog.stmts, &mut out);
    }
    out
}

fn top_assigned(stmts: &[Stmt], out: &mut HashSet<String>) {
    for s in stmts {
        match s {
            Stmt::Assign { targets, .. } => {
                for t in targets {
                    if let nx_ast::Target::Name(n) = t {
                        out.insert(n.clone());
                    }
                }
            }
            Stmt::For { var, .. } => {
                out.insert(var.clone());
            }
            Stmt::If { then_body, elifs, else_body, .. } => {
                top_assigned(then_body, out);
                for (_, b) in elifs {
                    top_assigned(b, out);
                }
                if let Some(b) = else_body {
                    top_assigned(b, out);
                }
            }
            Stmt::While { body, .. } => top_assigned(body, out),
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn summarize(
    programs: &HashMap<String, Program>,
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
    let scope = Scope {
        module,
        locals,
        mglobals: module_globals(programs, module),
        env,
        cx,
    };
    stmts(&scope, body, out);
}

struct Cx<'a> {
    bodies: &'a HashMap<Place, Vec<Stmt>>,
    sums: &'a HashMap<Place, Summary>,
}

fn stmts(scope: &Scope, body: &[Stmt], out: &mut Summary) {
    for s in body {
        stmt(scope, s, out);
    }
}

fn stmt(scope: &Scope, s: &Stmt, out: &mut Summary) {
    let module = scope.module;
    match s {
        Stmt::Assign { targets, values, .. } => {
            // Every value is evaluated, whichever target it lands in.
            for v in values {
                expr(scope, v, out);
            }
            // `a[i] = v` writes into a container rather than binding a
            // name, so it is not a shared-name write. Reads of the
            // container still show up through the value expressions.
            for t in targets {
                match t {
                    nx_ast::Target::Name(name) => {
                        if scope.is_shared(name) {
                            out.writes.insert((module.to_string(), name.clone()));
                        }
                    }
                    // A field write mutates the record in place, which is
                    // a read-modify-write of whatever holds it. When that
                    // holder is a module global the write is shared
                    // traffic, and treating it as such is the safe
                    // direction: missing it would be a silent race.
                    nx_ast::Target::Attr { base, .. } => {
                        if let Expr::Var(n, _) = base.as_ref() {
                            if scope.is_shared(n) {
                                out.reads.insert((module.to_string(), n.clone()));
                                out.writes.insert((module.to_string(), n.clone()));
                            }
                        }
                        expr(scope, base, out);
                    }
                    nx_ast::Target::Index { base, index } => {
                        expr(scope, base, out);
                        expr(scope, index, out);
                    }
                }
            }
        }
        Stmt::AssignOp { target, value, .. } => {
            expr(scope, value, out);
            if let nx_ast::Target::Name(name) = target {
                if scope.locals.contains(name) {
                    // read-modify-write of a local: no shared traffic.
                } else if scope.mglobals.contains(name) {
                    out.reads.insert((module.to_string(), name.clone()));
                    out.writes.insert((module.to_string(), name.clone()));
                }
            } else {
                // `a[i] += v` and `p.x += v` both read and write the
                // container in place, so both are shared traffic when the
                // container is a module global.
                match target {
                    nx_ast::Target::Index { base, index } => {
                        expr(scope, base, out);
                        expr(scope, index, out);
                    }
                    nx_ast::Target::Attr { base, .. } => {
                        if let Expr::Var(n, _) = base.as_ref() {
                            if scope.is_shared(n) {
                                out.reads.insert((module.to_string(), n.clone()));
                                out.writes.insert((module.to_string(), n.clone()));
                            }
                        }
                        expr(scope, base, out);
                    }
                    nx_ast::Target::Name(_) => {}
                }
            }
        }
        Stmt::Print { values, .. } => {
            out.prints = true;
            for v in values {
                expr(scope, v, out);
            }
        }
        Stmt::If { cond, then_body, elifs, else_body, .. } => {
            expr(scope, cond, out);
            stmts(scope, then_body, out);
            for (c, b) in elifs {
                expr(scope, c, out);
                stmts(scope, b, out);
            }
            if let Some(b) = else_body {
                stmts(scope, b, out);
            }
        }
        Stmt::While { cond, body, .. } => {
            expr(scope, cond, out);
            stmts(scope, body, out);
        }
        Stmt::For { var, iter, body, .. } => {
            match iter {
                nx_ast::ForIter::Range { start, end } => {
                    expr(scope, start, out);
                    expr(scope, end, out);
                }
                nx_ast::ForIter::Each(e) => expr(scope, e, out),
            }
            let mut inner = scope.clone();
            inner.locals.insert(var.clone());
            stmts(&inner, body, out);
        }
        // A declaration is compile-time only: no reads, no writes.
        Stmt::TypeDecl { .. } => {}
        Stmt::Fn { .. } => {}
        Stmt::Return { values, .. } => {
            for e in values {
                expr(scope, e, out);
            }
        }
        Stmt::Del { targets, .. } => {
            // Removing a name makes it unbound, so the read and write both
            // happen -- otherwise a later use would look safe.
            for t in targets {
                match t {
                    nx_ast::Target::Name(name) => {
                        out.reads.insert((module.to_string(), name.clone()));
                        out.writes.insert((module.to_string(), name.clone()));
                    }
                    nx_ast::Target::Index { base, index } => {
                        expr(scope, base, out);
                        expr(scope, index, out);
                    }
                    nx_ast::Target::Attr { base, .. } => expr(scope, base, out),
                }
            }
        }
        Stmt::Assert { cond, message, .. } => {
            expr(scope, cond, out);
            if let Some(m) = message {
                expr(scope, m, out);
            }
        }
        Stmt::Break { .. } | Stmt::Continue { .. } => {}
        Stmt::Import { .. } => {}
        Stmt::FromImport { .. } => {}
        Stmt::Parallel { tasks, .. } => {
            for t in tasks {
                let mut ts = Summary::default();
                stmt_summary(scope, t, &mut ts);
                out.merge(&ts);
            }
        }
        Stmt::Expr(e) => {
            expr(scope, e, out);
        }
    }
}

/// Summary of a single statement (used for parallel task partitioning).
fn stmt_summary(scope: &Scope, s: &Stmt, out: &mut Summary) {
    stmt(scope, s, out);
}

/// One summary per task statement of a `parallel:` block.
/// `locals` are names bound in the enclosing scope (reads/writes to them
/// are task-private and ignored); everything else is shared traffic.
pub fn task_summaries(
    programs: &HashMap<String, Program>,
    module: &str,
    locals: &HashSet<String>,
    tasks: &[Stmt],
) -> Result<Vec<Summary>, IrError> {
    let ir = analyze_map(programs.clone())?;
    let menv = Env::for_body(
        module,
        &programs
            .get(module)
            .map(|p| p.stmts.clone())
            .unwrap_or_default(),
    );
    let cx_bodies: HashMap<Place, Vec<Stmt>> = HashMap::new();
    let cx_sums: HashMap<Place, Summary> = ir
        .funcs
        .iter()
        .map(|(k, f)| (k.clone(), f.summary.clone()))
        .collect();
    let cx = Cx { bodies: &cx_bodies, sums: &cx_sums };
    let scope = Scope {
        module,
        locals: locals.clone(),
        mglobals: module_globals(programs, module),
        env: &menv,
        cx: &cx,
    };
    let mut out = Vec::new();
    for t in tasks {
        let mut s = Summary::default();
        stmt(&scope, t, &mut s);
        out.push(s);
    }
    Ok(out)
}

/// Do two task summaries conflict (must not run concurrently)?
pub fn conflicts(a: &Summary, b: &Summary) -> bool {
    // Write-write or read-write on a shared global.
    if a.writes.iter().any(|w| b.reads.contains(w) || b.writes.contains(w)) {
        return true;
    }
    if b.writes.iter().any(|w| a.reads.contains(w)) {
        return true;
    }
    // Unknown targets serialize with everything except pure silence.
    if a.opaque && (b.opaque || b.prints || b.heap || !b.reads.is_empty() || !b.writes.is_empty()) {
        return true;
    }
    if b.opaque && (a.prints || a.heap || !a.reads.is_empty() || !a.writes.is_empty()) {
        return true;
    }
    // Heap mutation conflicts with any shared traffic or printing.
    if a.heap && (b.heap || b.prints || !b.reads.is_empty() || !b.writes.is_empty()) {
        return true;
    }
    if b.heap && (a.prints || !a.reads.is_empty() || !a.writes.is_empty()) {
        return true;
    }
    // Keep stdout order deterministic.
    if a.prints && b.prints {
        return true;
    }
    if a.prints && (!b.reads.is_empty() || !b.writes.is_empty() || b.heap) {
        return true;
    }
    if b.prints && (!a.reads.is_empty() || !a.writes.is_empty() || a.heap) {
        return true;
    }
    false
}

/// Memoizable: provably independent of mutable state — no shared
/// reads or writes, no printing, no heap traffic, no opaque calls.
/// Results depend only on arguments, so caching them preserves semantics.
pub fn memoizable(sum: &Summary) -> bool {
    sum.reads.is_empty()
        && sum.writes.is_empty()
        && !sum.prints
        && !sum.heap
        && !sum.opaque
}
/// Greedy in-order batching: each task joins the earliest batch it does
/// not conflict with. Deterministic; preserves program order.
pub fn partition(sums: &[Summary]) -> Vec<Vec<usize>> {
    let mut batches: Vec<Vec<usize>> = Vec::new();
    for (i, s) in sums.iter().enumerate() {
        let mut placed = false;
        for b in batches.iter_mut() {
            if b.iter().all(|&j| !conflicts(s, &sums[j])) {
                b.push(i);
                placed = true;
                break;
            }
        }
        if !placed {
            batches.push(vec![i]);
        }
    }
    batches
}

fn expr(scope: &Scope, e: &Expr, out: &mut Summary) {
    let module = scope.module;
    match e {
        Expr::Var(name, _) => {
            if scope.locals.contains(name) {
                return;
            }
            // A bare function name is a reference, not a data read.
            if scope.cx.bodies.contains_key(&(module.to_string(), name.clone())) {
                return;
            }
            if scope.is_shared(name) {
                out.reads.insert((module.to_string(), name.clone()));
            }
        }
        Expr::Attr { base, attr, .. } => {
            if let Expr::Var(m, _) = base.as_ref() {
                if let Some(target) = scope.env.mods.get(m) {
                    out.reads.insert((target.clone(), attr.clone()));
                    return;
                }
            }
            // A record field read. Reading a field of a module global is
            // still a read of that global, which is what the shared-traffic
            // analysis keys on.
            if let Expr::Var(n, _) = base.as_ref() {
                if scope.is_shared(n) {
                    out.reads.insert((scope.module.to_string(), n.clone()));
                    return;
                }
            }
            expr(scope, base, out);
        }
        Expr::Index { base, index, .. } => {
            expr(scope, base, out);
            expr(scope, index, out);
        }
        Expr::List(items, _) => {
            for it in items {
                expr(scope, it, out);
            }
        }
        Expr::Range { start, end, .. } => {
            expr(scope, start, out);
            expr(scope, end, out);
        }
        Expr::Unary { expr: inner, .. } => expr(scope, inner, out),
        Expr::Binary { left, right, .. } => {
            expr(scope, left, out);
            expr(scope, right, out);
        }
        Expr::Call { callee, args, .. } => {
            for a in args {
                expr(scope, a, out);
            }
            call(scope, callee, out);
        }
        _ => {}
    }
}

/// Merge a callee's summary (or conservative flags) into `out`.
fn call(scope: &Scope, callee: &Expr, out: &mut Summary) {
    let module = scope.module;
    if let Expr::Var(name, _) = callee {
        if name == "len" {
            return;
        }
        if name == "push" {
            out.heap = true;
            return;
        }
        if let Some((m, f)) = scope.env.aliases.get(name) {
            if let Some(s) = scope.cx.sums.get(&(m.clone(), f.clone())) {
                let s = s.clone();
                out.merge(&s);
                return;
            }
        }
        if let Some(s) = scope.cx.sums.get(&(module.to_string(), name.clone())) {
            let s = s.clone();
            out.merge(&s);
            return;
        }
        out.opaque = true;
        return;
    }
    if let Expr::Attr { base, attr, .. } = callee {
        if let Expr::Var(m, _) = base.as_ref() {
            if let Some(target) = scope.env.mods.get(m) {
                if let Some(s) = scope.cx.sums.get(&(target.clone(), attr.clone())) {
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

    #[test]
    fn disjoint_tasks_share_batch() {
        let a = Summary { writes: [("__main__".into(), "x".into())].into(), ..Default::default() };
        let b = Summary { writes: [("__main__".into(), "y".into())].into(), ..Default::default() };
        assert_eq!(partition(&[a, b]), vec![vec![0, 1]]);
    }

    #[test]
    fn conflicting_tasks_serialize() {
        let a = Summary { writes: [("__main__".into(), "x".into())].into(), ..Default::default() };
        let b = Summary { reads: [("__main__".into(), "x".into())].into(), ..Default::default() };
        assert_eq!(partition(&[a, b]), vec![vec![0], vec![1]]);
    }

    #[test]
    fn prints_serialize() {
        let a = Summary { prints: true, ..Default::default() };
        let b = Summary { prints: true, ..Default::default() };
        assert_eq!(partition(&[a, b]), vec![vec![0], vec![1]]);
    }
}
