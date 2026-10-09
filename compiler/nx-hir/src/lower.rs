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

use nx_ast::{BinOp, Expr, Program, Span, Stmt, Target, UnaryOp};
use nx_types::{FnInfo, Ty};

use super::model::*;
use nx_ast::shape;

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

// ---------------------------------------------------------------------------
// Phase 1: index declarations across all modules.
// ---------------------------------------------------------------------------

/// A record declaration as written: home module, name, fields in order
/// with their type spellings. Field types resolve after every module is
/// indexed, so later and mutually recursive declarations work.
#[derive(Debug, Clone)]
struct TypeDecl {
    module: String,
    name: String,
    fields: Vec<(String, String)>,
}

/// A function or method body as written, with its declaring module.
/// Methods carry their type name plus receiver kind; the method table
/// below points at these bodies by `FuncId`.
#[derive(Debug, Clone)]
struct FuncDecl {
    module: String,
    name: String,
    params: Vec<String>,
    receiver: Option<(String, nx_ast::ReceiverKind)>,
    body: Vec<Stmt>,
    span: Span,
}

#[derive(Debug, Clone)]
struct MethodDecl {
    type_id: TypeId,
    name: String,
    func: FuncId,
    receiver: nx_ast::ReceiverKind,
}

struct Tables {
    modules_sorted: Vec<String>,
    module_id: HashMap<String, ModuleId>,
    /// (module, name) -> id, in declaration order per module.
    type_id: HashMap<(String, String), TypeId>,
    type_decls: Vec<TypeDecl>,
    /// (module, name) -> id, source order depth-first, nested included:
    /// redefinition is a checker error, so at most one exists.
    func_id: HashMap<(String, String), FuncId>,
    func_decls: Vec<FuncDecl>,
    /// (type, method name) -> id, for every method including associated
    /// functions. Duplicates are checker errors.
    method_id: HashMap<(TypeId, String), MethodId>,
    method_decls: Vec<MethodDecl>,
    /// Per-module alias env: alias -> (home module, canonical name).
    /// Module-wide (including function bodies), mirroring the checker,
    /// whose `type_alias` map is not scoped per function.
    type_alias: HashMap<(String, String), (String, String)>,
    /// Per-module globals in first-bind order (Assign/AssignOp names over
    /// top-level control flow; loop variables are slots, never globals).
    globals: HashMap<String, Vec<String>>,
}

fn build_tables(programs: &HashMap<String, Program>) -> LResult<Tables> {
    let mut modules_sorted: Vec<String> = programs.keys().cloned().collect();
    modules_sorted.sort();
    let module_id: HashMap<String, ModuleId> = modules_sorted
        .iter()
        .enumerate()
        .map(|(i, m)| (m.clone(), ModuleId(i as u32)))
        .collect();

    let mut t = Tables {
        modules_sorted,
        module_id,
        type_id: HashMap::new(),
        type_decls: Vec::new(),
        func_id: HashMap::new(),
        func_decls: Vec::new(),
        method_id: HashMap::new(),
        method_decls: Vec::new(),
        type_alias: HashMap::new(),
        globals: HashMap::new(),
    };

    // Types first: field types may name records declared later or
    // mutually, so every declaration is indexed before any resolution.
    for module in t.modules_sorted.clone() {
        let prog = &programs[&module];
        for s in &prog.stmts {
            if let Stmt::TypeDecl { name, fields, .. } = s {
                let id = TypeId(t.type_decls.len() as u32);
                t.type_id.insert((module.clone(), name.clone()), id);
                t.type_decls.push(TypeDecl {
                    module: module.clone(),
                    name: name.clone(),
                    fields: fields
                        .iter()
                        .map(|f| (f.name.clone(), f.ty.clone()))
                        .collect(),
                });
            }
        }
    }
    for module in t.modules_sorted.clone() {
        let prog = &programs[&module];
        index_decls(&mut t, &module, &prog.stmts)?;
        index_globals(&mut t, &module, &prog.stmts);
    }
    for module in t.modules_sorted.clone() {
        let prog = &programs[&module];
        index_aliases(&mut t, &module, &prog.stmts)?;
    }
    Ok(t)
}

/// Every `fn` and `impl` anywhere in the module, source order
/// depth-first. Nested declarations follow the checker (which accepts
/// them into module scope), not the legacy harvest (top level only).
fn index_decls(t: &mut Tables, module: &str, stmts: &[Stmt]) -> LResult<()> {
    for s in stmts {
        match s {
            Stmt::Fn {
                name,
                params,
                body,
                span,
            } => {
                if t.func_id.contains_key(&(module.to_string(), name.clone())) {
                    return Err(lerr(
                        *span,
                        format!("internal: duplicate function '{name}'"),
                    ));
                }
                let id = FuncId(t.func_decls.len() as u32);
                t.func_id.insert((module.to_string(), name.clone()), id);
                t.func_decls.push(FuncDecl {
                    module: module.to_string(),
                    name: name.clone(),
                    params: params.clone(),
                    receiver: None,
                    body: body.clone(),
                    span: *span,
                });
            }
            Stmt::Impl {
                type_name,
                methods,
                span,
            } => {
                let tid = *t
                    .type_id
                    .get(&(module.to_string(), type_name.clone()))
                    .ok_or_else(|| {
                        lerr(
                            *span,
                            format!("internal: impl of unknown type '{type_name}'"),
                        )
                    })?;
                for m in methods {
                    let fid = FuncId(t.func_decls.len() as u32);
                    t.func_decls.push(FuncDecl {
                        module: module.to_string(),
                        name: m.name.clone(),
                        params: m.params.clone(),
                        receiver: Some((type_name.clone(), m.receiver)),
                        body: m.body.clone(),
                        span: m.span,
                    });
                    // Every method -- associated functions included --
                    // resolves through one table; the receiver kind
                    // tells calls whether a receiver is passed.
                    let mid = MethodId(t.method_decls.len() as u32);
                    t.method_id.insert((tid, m.name.clone()), mid);
                    t.method_decls.push(MethodDecl {
                        type_id: tid,
                        name: m.name.clone(),
                        func: fid,
                        receiver: m.receiver,
                    });
                }
            }
            _ => {}
        }
        for b in shape::child_bodies(s) {
            index_decls(t, module, b)?;
        }
        if let Stmt::Fn { body, .. } = s {
            index_decls(t, module, body)?;
        }
        if let Stmt::Impl { methods, .. } = s {
            for m in methods {
                index_decls(t, module, &m.body)?;
            }
        }
    }
    Ok(())
}

/// Module globals in first-bind order: plain-name Assign/AssignOp
/// targets over top-level control flow. Loop variables are slots even
/// at top level (the backend keeps them in the enclosing frame), and
/// function bodies are separate scopes.
fn index_globals(t: &mut Tables, module: &str, stmts: &[Stmt]) {
    fn walk(t: &mut Tables, module: &str, stmts: &[Stmt]) {
        for s in stmts {
            match s {
                Stmt::Assign { targets, .. } => {
                    for tgt in targets {
                        if let Target::Name(n) = tgt {
                            let g = t.globals.entry(module.to_string()).or_default();
                            if !g.contains(n) {
                                g.push(n.clone());
                            }
                        }
                    }
                }
                Stmt::AssignOp { target, .. } => {
                    if let Target::Name(n) = target {
                        let g = t.globals.entry(module.to_string()).or_default();
                        if !g.contains(n) {
                            g.push(n.clone());
                        }
                    }
                }
                _ => {}
            }
            for b in shape::child_bodies(s) {
                walk(t, module, b);
            }
        }
    }
    walk(t, module, stmts);
}

/// From-imported type names, module-wide (including function bodies),
/// mirroring the checker's unscoped `type_alias` map. Functions and
/// value imports resolve on demand against indexed tables, so they need
/// no alias map of their own.
fn index_aliases(t: &mut Tables, module: &str, stmts: &[Stmt]) -> LResult<()> {
    fn walk(t: &mut Tables, module: &str, stmts: &[Stmt]) -> LResult<()> {
        for s in stmts {
            if let Stmt::FromImport {
                module: m, names, ..
            } = s
            {
                for (name, alias) in names {
                    let bind = alias.clone().unwrap_or_else(|| name.clone());
                    if t.type_id.contains_key(&(m.clone(), name.clone())) {
                        t.type_alias
                            .insert((module.to_string(), bind), (m.clone(), name.clone()));
                    }
                }
            }
            for b in shape::child_bodies(s) {
                walk(t, module, b)?;
            }
            if let Stmt::Fn { body, .. } = s {
                walk(t, module, body)?;
            }
            if let Stmt::Impl { methods, .. } = s {
                for m in methods {
                    walk(t, module, &m.body)?;
                }
            }
        }
        Ok(())
    }
    walk(t, module, stmts)
}

// ---------------------------------------------------------------------------
// Phase 2: assemble tables, then lower every body.
// ---------------------------------------------------------------------------

/// Entry: lower loaded programs starting at `entry`.
fn lower_loaded(
    programs: &HashMap<String, Program>,
    bases: &HashMap<String, PathBuf>,
    entry: &str,
) -> Result<HProgram, LowerError> {
    if !programs.contains_key(entry) {
        return Err(lerr_at(1, 1, format!("internal: no such module '{entry}'")));
    }
    // Types: checker inference per module, with its real name (the
    // `__main__` fix this design depends on).
    let mut inferred: HashMap<(String, String), FnInfo> = HashMap::new();
    for module in programs.keys() {
        let base = bases
            .get(module)
            .cloned()
            .unwrap_or_else(|| PathBuf::from("."));
        match nx_types::infer_program_for(&programs[module], &base, module) {
            Ok(m) => {
                for (k, v) in m {
                    inferred.insert(k, v);
                }
            }
            Err(es) => {
                let e = &es[0];
                return Err(LowerError {
                    message: format!("internal: re-inference failed: {}", e.message),
                    line: e.line,
                    col: e.col,
                });
            }
        }
    }
    let tables = build_tables(programs)?;
    Lower::new(tables, programs, inferred).program(entry)
}

struct Lower<'a> {
    tables: Tables,
    programs: &'a HashMap<String, Program>,
    inferred: HashMap<(String, String), FnInfo>,
    out: HProgram,
    /// Interned runtime strings (dynamic field names), deduplicated
    /// across the program. Moved into the output at the end.
    strings: Vec<String>,
    string_ids: HashMap<String, StrId>,
    /// GlobalId -> value type, from each module's `<top>` locals.
    global_tys: HashMap<GlobalId, HTy>,
    /// (module, name) -> GlobalId, for write positions.
    global_ids: HashMap<(String, String), GlobalId>,
    /// GlobalId -> (module, name), for diagnostics.
    global_names: HashMap<GlobalId, (String, String)>,
    /// (module, name) -> TypeId: declarations plus from-imported names
    /// under alias and canonical spelling, mirroring the checker.
    visible_types: HashMap<(String, String), TypeId>,
    /// (module, name) -> FuncId: top-level and nested functions.
    visible_funcs: HashMap<(String, String), FuncId>,
}

/// A name binding inside one function body under lowering.
#[derive(Clone)]
enum NameRef {
    Slot(Slot),
    Global(GlobalId),
    Module(ModuleId),
    Func(FuncId),
    Type(TypeId),
}

fn diag(name: &str) -> DiagInfo {
    DiagInfo::named(name)
}

fn no_diag() -> DiagInfo {
    DiagInfo { name: None }
}

/// The immutable program tables one body needs, borrowed disjointly
/// from the interning tables so a single `Lower` can lower bodies in
/// sequence while appending to its string table.
struct Ctx<'a> {
    tables: &'a Tables,
    inferred: &'a HashMap<(String, String), FnInfo>,
    global_tys: &'a HashMap<GlobalId, HTy>,
    global_ids: &'a HashMap<(String, String), GlobalId>,
    visible_types: &'a HashMap<(String, String), TypeId>,
    visible_funcs: &'a HashMap<(String, String), FuncId>,
}

impl Ctx<'_> {
    /// The checker's inference for one body key. A missing key means
    /// the checker never produced this body, which a checked program
    /// cannot do -- so it is an internal error, not a diagnostic.
    fn fn_info(&self, module: &str, func: &str) -> LResult<FnInfo> {
        fn_info(self.inferred, module, func)
    }
}

/// A checker entry for one body key, or an internal error naming the
/// gap. Methods are keyed `Type.method`; module tops are `<top>`.
fn fn_info(
    inferred: &HashMap<(String, String), FnInfo>,
    module: &str,
    func: &str,
) -> LResult<FnInfo> {
    inferred
        .get(&(module.to_string(), func.to_string()))
        .cloned()
        .ok_or_else(|| {
            lerr_at(
                1,
                1,
                format!("internal: no inference for '{module}.{func}'"),
            )
        })
}

impl<'a> Lower<'a> {
    fn new(
        tables: Tables,
        programs: &'a HashMap<String, Program>,
        inferred: HashMap<(String, String), FnInfo>,
    ) -> Self {
        Lower {
            tables,
            programs,
            inferred,
            out: HProgram {
                modules: Vec::new(),
                types: Vec::new(),
                methods: Vec::new(),
                funcs: Vec::new(),
                globals: Vec::new(),
                strings: Vec::new(),
                entry: ModuleId(0),
            },
            strings: Vec::new(),
            string_ids: HashMap::new(),
            global_tys: HashMap::new(),
            global_ids: HashMap::new(),
            global_names: HashMap::new(),
            visible_types: HashMap::new(),
            visible_funcs: HashMap::new(),
        }
    }

    /// Assemble tables, then lower every body. IDs were assigned in
    /// phase 1, so this only fills and lowers.
    fn program(&mut self, entry: &str) -> Result<HProgram, LowerError> {
        let entry_id = *self
            .tables
            .module_id
            .get(entry)
            .ok_or_else(|| lerr_at(1, 1, format!("internal: no such module '{entry}'")))?;
        for ((module, alias), (home, canon)) in self.tables.type_alias.clone() {
            if let Some(tid) = self.tables.type_id.get(&(home.clone(), canon.clone())) {
                self.visible_types.insert((module.clone(), alias), *tid);
            }
        }
        for ((module, name), tid) in self.tables.type_id.clone() {
            self.visible_types.insert((module, name), tid);
        }
        for ((module, name), fid) in self.tables.func_id.clone() {
            self.visible_funcs.insert((module, name), fid);
        }
        for tid in 0..self.tables.type_decls.len() {
            let decl = self.tables.type_decls[tid].clone();
            let mid = self.tables.module_id[&decl.module];
            let mut fields = Vec::with_capacity(decl.fields.len());
            for (_, fty) in &decl.fields {
                fields.push(conv_ty_str(&self.tables, fty, &decl.module)?);
            }
            self.out.types.push(HType {
                module: mid,
                fields,
                diag: diag(&decl.name),
            });
        }
        for m in self.tables.method_decls.clone() {
            let recv = match m.receiver {
                nx_ast::ReceiverKind::None => None,
                nx_ast::ReceiverKind::Read => Some(ReceiverKind::Read),
                nx_ast::ReceiverKind::Mut => Some(ReceiverKind::Mut),
                nx_ast::ReceiverKind::Own => Some(ReceiverKind::Own),
            };
            self.out.methods.push(HMethod {
                type_id: m.type_id,
                func: m.func,
                receiver: recv,
                diag: diag(&m.name),
            });
        }
        // Globals in first-bind order, typed from each module's `<top>`
        // inference (every bound name is there; anything else never
        // reaches lowering through checked code).
        let n_decl_funcs = self.tables.func_decls.len();
        for (k, module) in self.tables.modules_sorted.clone().into_iter().enumerate() {
            let mid = self.tables.module_id[&module];
            let mut gids = Vec::new();
            for name in self
                .tables
                .globals
                .get(&module)
                .cloned()
                .unwrap_or_default()
            {
                let gid = GlobalId(self.out.globals.len() as u32);
                let ty = self.top_local_ty(&module, &name)?;
                self.global_tys.insert(gid, ty);
                self.global_ids.insert((module.clone(), name.clone()), gid);
                self.global_names
                    .insert(gid, (module.clone(), name.clone()));
                self.out.globals.push(HGlobal {
                    module: mid,
                    diag: diag(&name),
                });
                gids.push(gid);
            }
            let fids: Vec<FuncId> = self
                .tables
                .func_decls
                .iter()
                .enumerate()
                .filter(|(_, f)| f.module == module)
                .map(|(i, _)| FuncId(i as u32))
                .collect();
            let tids: Vec<TypeId> = self
                .tables
                .type_decls
                .iter()
                .enumerate()
                .filter(|(_, d)| d.module == module)
                .map(|(i, _)| TypeId(i as u32))
                .collect();
            // The top-level function comes after every declared
            // function, one per module in sorted order.
            let top_id = FuncId(n_decl_funcs as u32 + k as u32);
            let mut module_funcs = fids;
            module_funcs.push(top_id);
            self.out.modules.push(HModule {
                diag: diag(&module),
                globals: gids,
                types: tids,
                funcs: module_funcs,
                top: top_id,
            });
        }
        // Bodies: declared functions in ID order, then each module top.
        // FuncIds were assigned in declaration order, so pushing in
        // `func_decls` order lands each body on its own ID.
        for f in self.tables.func_decls.clone() {
            let hf = self.lower_func_decl(&f)?;
            self.out.funcs.push(hf);
        }
        for module in self.tables.modules_sorted.clone() {
            let ht = self.lower_top(&module)?;
            self.out.funcs.push(ht);
        }
        self.out.entry = entry_id;
        self.out.strings = std::mem::take(&mut self.strings);
        let program = std::mem::replace(
            &mut self.out,
            HProgram {
                modules: Vec::new(),
                types: Vec::new(),
                methods: Vec::new(),
                funcs: Vec::new(),
                globals: Vec::new(),
                strings: Vec::new(),
                entry: ModuleId(0),
            },
        );
        // Phase 4: verify what was just built, so a lowering bug fails
        // here -- at the span that produced it -- instead of three
        // stages later. Verification failure is still an internal
        // error: the input was a checked program.
        if let Err(violations) = crate::verify::verify(&program) {
            let first = &violations[0];
            return Err(LowerError {
                message: format!(
                    "internal: lowered program breaks {}: {}",
                    first.rule, first.message
                ),
                line: first.span.line,
                col: first.span.col,
            });
        }
        Ok(program)
    }

    /// A global's type from its module's `<top>` inference. A name that
    /// is bound but absent from inference was `del`eted and never
    /// rebound -- the checker erases the name on `del`, exactly as it
    /// does here -- so its storage is dynamic from that point on.
    fn top_local_ty(&self, module: &str, name: &str) -> LResult<HTy> {
        let info = self
            .inferred
            .get(&(module.to_string(), "<top>".to_string()))
            .ok_or_else(|| lerr_at(1, 1, format!("internal: no top inference for '{module}'")))?;
        match info.locals.get(name) {
            Some(ty) => conv_ty(&self.tables, ty, module, Span { line: 1, col: 1 }),
            None => Ok(HTy::Unknown),
        }
    }

    /// Lower one declared function or method body.
    fn lower_func_decl(&mut self, decl: &FuncDecl) -> LResult<HFunc> {
        // Method bodies are keyed by canonical type: `Point.moved`,
        // exactly like the checker's inferred map.
        let key = match &decl.receiver {
            Some((tname, _)) => {
                let canon = canonical_name(&self.tables, &decl.module, tname);
                nx_ast::shape::method_key(&canon, &decl.name)
            }
            None => decl.name.clone(),
        };
        let info = fn_info(&self.inferred, &decl.module, &key)?;
        // Only a method with a receiver takes one. An associated
        // function is an ordinary body with no `self`, which is why it
        // resolves through the same table (see `tables`).
        let recv = match &decl.receiver {
            Some((tname, kind)) if *kind != nx_ast::ReceiverKind::None => {
                let tid =
                    resolve_type_name(&self.tables, &decl.module, tname).ok_or_else(|| {
                        lerr(
                            decl.span,
                            format!("internal: unresolvable receiver type '{tname}'"),
                        )
                    })?;
                Some(tid)
            }
            _ => None,
        };
        let Lower {
            tables,
            inferred,
            global_tys,
            global_ids,
            visible_types,
            visible_funcs,
            strings,
            string_ids,
            ..
        } = self;
        let ctx = Ctx {
            tables,
            inferred,
            global_tys,
            global_ids,
            visible_types,
            visible_funcs,
        };
        lower_fn_body(
            &ctx,
            &info,
            strings,
            string_ids,
            &decl.module,
            &decl.params,
            recv,
            &decl.body,
            &decl.span,
            false,
            &decl.name,
        )
    }

    /// Lower a module top: same as a body, with no params and the
    /// `<top>` inference key.
    fn lower_top(&mut self, module: &str) -> LResult<HFunc> {
        let body = self
            .programs
            .get(module)
            .ok_or_else(|| lerr_at(1, 1, format!("internal: no such module '{module}'")))?
            .stmts
            .clone();
        let info = fn_info(&self.inferred, module, "<top>")?;
        let Lower {
            tables,
            inferred,
            global_tys,
            global_ids,
            visible_types,
            visible_funcs,
            strings,
            string_ids,
            ..
        } = self;
        let ctx = Ctx {
            tables,
            inferred,
            global_tys,
            global_ids,
            visible_types,
            visible_funcs,
        };
        lower_fn_body(
            &ctx,
            &info,
            strings,
            string_ids,
            module,
            &[],
            None,
            &body,
            &Span { line: 1, col: 1 },
            true,
            "<top>",
        )
    }
}

// ---------------------------------------------------------------------------
// Shared type conversion and per-function lowering. Free functions over
// `&Tables` so the assembler above and body lowering below use one
// implementation.
// ---------------------------------------------------------------------------

/// A checker type to its HIR twin, resolving nominal references in
/// `module`. Function and module values never appear on value nodes
/// (both are rejected in value position), so they fail loudly here
/// rather than mistyping.
fn conv_ty(tables: &Tables, ty: &Ty, module: &str, span: Span) -> LResult<HTy> {
    match ty {
        Ty::Int => Ok(HTy::Int),
        Ty::Float => Ok(HTy::Float),
        Ty::Bool => Ok(HTy::Bool),
        Ty::Str => Ok(HTy::Str),
        Ty::None => Ok(HTy::None),
        Ty::Unknown => Ok(HTy::Unknown),
        Ty::List(t) => Ok(HTy::List(Box::new(conv_ty(tables, t, module, span)?))),
        Ty::Dict(t) => Ok(HTy::Dict(Box::new(conv_ty(tables, t, module, span)?))),
        Ty::Record(name) => {
            let tid = resolve_type_name(tables, module, name)
                .ok_or_else(|| lerr(span, format!("internal: unresolvable record '{name}'")))?;
            Ok(HTy::Record(tid))
        }
        Ty::Func(..) => Err(lerr(span, "internal: function value in HIR".to_string())),
        Ty::Module(..) => Err(lerr(span, "internal: module value in HIR".to_string())),
    }
}

/// A field type spelling resolved in its declaring module, mirroring
/// the checker's `field_ty`.
fn conv_ty_str(tables: &Tables, tname: &str, module: &str) -> LResult<HTy> {
    match tname {
        "Int" => Ok(HTy::Int),
        "Float" => Ok(HTy::Float),
        "Bool" => Ok(HTy::Bool),
        "Str" => Ok(HTy::Str),
        "None" => Ok(HTy::None),
        _ => match resolve_type_name(tables, module, tname) {
            Some(tid) => Ok(HTy::Record(tid)),
            // `Any`, `List`, `Dict`, and names the checker already
            // rejected (unreachable): dynamic.
            None => Ok(HTy::Unknown),
        },
    }
}

/// A type name in a module to its `TypeId`: a same-module declaration,
/// else a from-imported name (under alias or canonical spelling),
/// mirroring the checker's canonicalization.
fn resolve_type_name(tables: &Tables, module: &str, name: &str) -> Option<TypeId> {
    if let Some((home, canon)) = tables
        .type_alias
        .get(&(module.to_string(), name.to_string()))
    {
        if let Some(tid) = tables.type_id.get(&(home.clone(), canon.clone())) {
            return Some(*tid);
        }
    }
    tables
        .type_id
        .get(&(module.to_string(), name.to_string()))
        .copied()
}

/// Canonical type name in a module, mirroring the checker's
/// `canonical_name`: a from-import alias wins, else the name itself.
fn canonical_name(tables: &Tables, module: &str, name: &str) -> String {
    tables
        .type_alias
        .get(&(module.to_string(), name.to_string()))
        .map(|(_, canon)| canon.clone())
        .unwrap_or_else(|| name.to_string())
}

/// Lattice predicates over [`HTy`], mirroring `nx_types::compatible`
/// and `is_numeric`. The shapes match; the types differ, so these live
/// beside their uses rather than across crates.
fn hty_compatible(a: &HTy, b: &HTy) -> bool {
    a == b || matches!(a, HTy::Unknown) || matches!(b, HTy::Unknown)
}

fn hty_numeric(t: &HTy) -> bool {
    matches!(t, HTy::Int | HTy::Float | HTy::Unknown)
}

/// Copy discipline from a value type (grammar §4.2).
fn copy_rule(ty: &HTy) -> CopyRule {
    match ty {
        HTy::Int | HTy::Float | HTy::Bool | HTy::None => CopyRule::CopyScalar,
        HTy::Str => CopyRule::ShareStr,
        HTy::List(_) | HTy::Dict(_) | HTy::Record(_) => CopyRule::DeepClone,
        HTy::Unknown => CopyRule::Dynamic,
    }
}

/// Binary rule from the operator and the lowered operand types. The
/// result *type* comes from the checker's matrix (`arith_result`); the
/// *rule* follows R1/R2 here: trapping integer arithmetic, saturating
/// power, float promotion. Unknown on either side defers to runtime.
fn decide_bin_rule(op: BinOp, l: &HTy, r: &HTy, span: Span) -> LResult<BinRule> {
    use BinOp::*;
    if matches!(l, HTy::Unknown) || matches!(r, HTy::Unknown) {
        return Ok(BinRule::Dynamic);
    }
    match op {
        Add if matches!((l, r), (HTy::Str, HTy::Str)) => Ok(BinRule::Concat),
        Add | Sub | Mul | Div | FloorDiv | Mod => match (l, r) {
            (HTy::Int, HTy::Int) => Ok(BinRule::Arith(ArithRule::Trap)),
            (HTy::Float, HTy::Float) => Ok(BinRule::Arith(ArithRule::Float)),
            (HTy::Int, HTy::Float) | (HTy::Float, HTy::Int) => {
                Ok(BinRule::Arith(ArithRule::PromoteFloat))
            }
            _ => Err(lerr(
                span,
                format!("internal: no binary rule for '{op:?}' on {l:?} and {r:?}"),
            )),
        },
        Pow => match (l, r) {
            (HTy::Int, HTy::Int) => Ok(BinRule::Pow(PowRule::Saturate)),
            (HTy::Float, HTy::Float) => Ok(BinRule::Arith(ArithRule::Float)),
            (HTy::Int, HTy::Float) | (HTy::Float, HTy::Int) => {
                Ok(BinRule::Arith(ArithRule::PromoteFloat))
            }
            _ => Err(lerr(
                span,
                format!("internal: no binary rule for '{op:?}' on {l:?} and {r:?}"),
            )),
        },
        BitAnd | BitOr | BitXor | Shl | Shr => match (l, r) {
            (HTy::Int, HTy::Int) => Ok(BinRule::Bitwise),
            _ => Err(lerr(
                span,
                format!("internal: no binary rule for '{op:?}' on {l:?} and {r:?}"),
            )),
        },
        _ => Err(lerr(
            span,
            format!("internal: '{op:?}' is not an arithmetic operator"),
        )),
    }
}

/// Lower one function or module-top body. `recv` is the resolved receiver
/// record for methods (`self` becomes slot 0), `None` otherwise.
/// `is_top` selects global versus slot binding for plain names, and
/// `diag_name` is the inert label the dump prints.
#[allow(clippy::too_many_arguments)]
fn lower_fn_body<'a>(
    ctx: &'a Ctx<'a>,
    info: &FnInfo,
    strings: &'a mut Vec<String>,
    string_ids: &'a mut HashMap<String, StrId>,
    module: &str,
    params: &[String],
    recv: Option<TypeId>,
    body: &[Stmt],
    span: &Span,
    is_top: bool,
    diag_name: &str,
) -> LResult<HFunc> {
    let ret = conv_ty(ctx.tables, &info.ret, module, *span)?;
    let mut fx = FnLower {
        ctx,
        module: module.to_string(),
        info: info.clone(),
        strings,
        string_ids,
        slots: Vec::new(),
        env: HashMap::new(),
        is_top,
    };
    let mut hparams = Vec::with_capacity(params.len() + 1);
    if let Some(tid) = recv {
        let s = fx.alloc_slot(HTy::Record(tid));
        debug_assert_eq!(s, Slot(0));
        fx.env.insert("self".to_string(), NameRef::Slot(s));
        hparams.push((s, HTy::Record(tid)));
    }
    for p in params {
        let ty = fx.local_ty(p, *span)?;
        let s = fx.alloc_slot(ty.clone());
        fx.env.insert(p.clone(), NameRef::Slot(s));
        hparams.push((s, ty));
    }
    let mut hbody = Vec::with_capacity(body.len());
    for s in body {
        fx.lower_stmt(s, &mut hbody)?;
    }
    Ok(HFunc {
        params: hparams,
        ret,
        body: hbody,
        diag: diag(diag_name),
    })
}

struct FnLower<'a> {
    ctx: &'a Ctx<'a>,
    module: String,
    info: FnInfo,
    strings: &'a mut Vec<String>,
    string_ids: &'a mut HashMap<String, StrId>,
    slots: Vec<HTy>,
    env: HashMap<String, NameRef>,
    /// True for module tops: plain names bind globals, not slots.
    is_top: bool,
}

impl<'a> FnLower<'a> {
    fn alloc_slot(&mut self, ty: HTy) -> Slot {
        let s = Slot(self.slots.len() as u32);
        self.slots.push(ty);
        s
    }

    fn slot_ty(&self, s: Slot) -> HTy {
        self.slots[s.0 as usize].clone()
    }

    /// A bound name's static type from inference. Every bound name is in
    /// `locals` on checked programs, with one exception: `del` erases
    /// the name (as the checker does), so a name deleted and never
    /// rebound is genuinely unresolved. That case answers `Unknown`,
    /// which is what the checker knows; anything else is an internal
    /// error caught by the debug assertion, never a silent `Unknown`.
    fn local_ty(&self, name: &str, span: Span) -> LResult<HTy> {
        let ty = match self.info.locals.get(name) {
            Some(t) => t.clone(),
            None => {
                debug_assert!(
                    false,
                    "internal: '{name}' has no inferred type at {}:{}",
                    span.line, span.col
                );
                return Ok(HTy::Unknown);
            }
        };
        conv_ty(self.ctx.tables, &ty, &self.module, span)
    }

    fn global_ty(&self, g: GlobalId) -> HTy {
        self.ctx.global_tys.get(&g).cloned().unwrap_or(HTy::Unknown)
    }

    fn stmt(&self, span: Span, diag_name: Option<&str>, kind: HStmtKind) -> HStmt {
        HStmt {
            span,
            diag: diag_name.map(diag).unwrap_or_else(no_diag),
            kind,
        }
    }

    fn expr(&self, span: Span, diag_name: Option<&str>, ty: HTy, kind: HExprKind) -> HExpr {
        HExpr {
            span,
            diag: diag_name.map(diag).unwrap_or_else(no_diag),
            ty,
            kind,
        }
    }

    fn lower_block(&mut self, body: &[Stmt], out: &mut Vec<HStmt>) -> LResult<()> {
        for s in body {
            self.lower_stmt(s, out)?;
        }
        Ok(())
    }

    fn lower_stmt(&mut self, s: &Stmt, out: &mut Vec<HStmt>) -> LResult<()> {
        match s {
            Stmt::Assign {
                targets,
                values,
                span,
            } => {
                // All right-hand sides evaluate before any store (so
                // `a, b = b, a` swaps), exactly like the backend.
                let mut hvalues = Vec::with_capacity(values.len());
                for v in values {
                    hvalues.push(self.lower_expr(v)?);
                }
                if targets.len() > 1 && values.len() == 1 {
                    // Destructuring: every target takes from the one tuple.
                    let mut htargets = Vec::with_capacity(targets.len());
                    for t in targets {
                        htargets.push(self.lower_assign_target(t, *span)?);
                    }
                    let rules = vec![copy_rule(&hvalues[0].ty)];
                    out.push(self.stmt(
                        *span,
                        None,
                        HStmtKind::Assign {
                            targets: htargets,
                            values: hvalues,
                            rules,
                        },
                    ));
                    return Ok(());
                }
                if targets.len() != values.len() {
                    return Err(lerr(
                        *span,
                        format!(
                            "internal: {} targets but {} values",
                            targets.len(),
                            values.len()
                        ),
                    ));
                }
                let mut htargets = Vec::with_capacity(targets.len());
                let mut rules = Vec::with_capacity(values.len());
                for (t, v) in targets.iter().zip(hvalues.iter()) {
                    htargets.push(self.lower_assign_target(t, *span)?);
                    rules.push(copy_rule(&v.ty));
                }
                out.push(self.stmt(
                    *span,
                    None,
                    HStmtKind::Assign {
                        targets: htargets,
                        values: hvalues,
                        rules,
                    },
                ));
                Ok(())
            }
            Stmt::AssignOp {
                target,
                op,
                value,
                span,
            } => {
                // Name targets keep the dedicated path; element and field
                // targets stash their evaluated parts first (see
                // `stash_target`), so the target runs before the value
                // and every part runs exactly once.
                match target {
                    Target::Name(name) => {
                        let (t, cur_ty) = self.assign_name_target(name, *span)?;
                        let v = self.lower_expr(value)?;
                        let rule = decide_bin_rule(*op, &cur_ty, &v.ty, *span)?;
                        out.push(self.stmt(
                            *span,
                            None,
                            HStmtKind::AssignOp {
                                target: t,
                                op: *op,
                                rule,
                                value: v,
                            },
                        ));
                        Ok(())
                    }
                    _ => {
                        let rebuilt = self.stash_target(target, *span)?;
                        let v = self.lower_expr(value)?;
                        let cur = self.read_target(&rebuilt, *span)?;
                        let rule = decide_bin_rule(*op, &cur.ty, &v.ty, *span)?;
                        out.push(self.stmt(
                            *span,
                            None,
                            HStmtKind::AssignOp {
                                target: rebuilt,
                                op: *op,
                                rule,
                                value: v,
                            },
                        ));
                        Ok(())
                    }
                }
            }
            Stmt::Print { values, span } => {
                let mut vs = Vec::with_capacity(values.len());
                for v in values {
                    vs.push(self.lower_expr(v)?);
                }
                out.push(self.stmt(*span, None, HStmtKind::Print { values: vs }));
                Ok(())
            }
            Stmt::If {
                cond,
                then_body,
                elifs,
                else_body,
                span,
            } => {
                let c = self.lower_expr(cond)?;
                let mut then_b = Vec::new();
                self.lower_block(then_body, &mut then_b)?;
                let mut helifs = Vec::with_capacity(elifs.len());
                for (ec, eb) in elifs {
                    let hc = self.lower_expr(ec)?;
                    let mut hb = Vec::new();
                    self.lower_block(eb, &mut hb)?;
                    helifs.push((hc, hb));
                }
                let hel = match else_body {
                    Some(b) => {
                        let mut hb = Vec::new();
                        self.lower_block(b, &mut hb)?;
                        Some(hb)
                    }
                    None => None,
                };
                out.push(self.stmt(
                    *span,
                    None,
                    HStmtKind::If {
                        cond: c,
                        then_body: then_b,
                        elifs: helifs,
                        else_body: hel,
                    },
                ));
                Ok(())
            }
            Stmt::While { cond, body, span } => {
                let c = self.lower_expr(cond)?;
                let mut hb = Vec::new();
                self.lower_block(body, &mut hb)?;
                out.push(self.stmt(*span, None, HStmtKind::While { cond: c, body: hb }));
                Ok(())
            }
            Stmt::For {
                var,
                iter,
                body,
                span,
            } => match iter {
                nx_ast::ForIter::Range { start, end } => {
                    let s = self.lower_expr(start)?;
                    let e = self.lower_expr(end)?;
                    let slot = self.alloc_slot(HTy::Int);
                    let saved = self.env.insert(var.clone(), NameRef::Slot(slot));
                    let mut hb = Vec::new();
                    self.lower_block(body, &mut hb)?;
                    self.restore_env(var, saved);
                    out.push(self.stmt(
                        *span,
                        Some(var),
                        HStmtKind::ForRange {
                            var: slot,
                            start: s,
                            end: e,
                            body: hb,
                        },
                    ));
                    Ok(())
                }
                nx_ast::ForIter::Each(e) => {
                    let it = self.lower_expr(e)?;
                    let rule = self.iter_rule(&it.ty, *span)?;
                    let ety = self.iter_elem_ty(&it.ty, *span)?;
                    let slot = self.alloc_slot(ety);
                    let saved = self.env.insert(var.clone(), NameRef::Slot(slot));
                    let mut hb = Vec::new();
                    self.lower_block(body, &mut hb)?;
                    self.restore_env(var, saved);
                    out.push(self.stmt(
                        *span,
                        Some(var),
                        HStmtKind::ForEach {
                            var: slot,
                            iter: it,
                            rule,
                            body: hb,
                        },
                    ));
                    Ok(())
                }
            },
            Stmt::Return { values, span } => {
                let mut vs = Vec::with_capacity(values.len());
                for v in values {
                    vs.push(self.lower_expr(v)?);
                }
                out.push(self.stmt(*span, None, HStmtKind::Return { values: vs }));
                Ok(())
            }
            Stmt::Break { span } => {
                out.push(self.stmt(*span, None, HStmtKind::Break));
                Ok(())
            }
            Stmt::Continue { span } => {
                out.push(self.stmt(*span, None, HStmtKind::Continue));
                Ok(())
            }
            Stmt::Del { targets, span } => {
                let mut hs = Vec::with_capacity(targets.len());
                for t in targets {
                    if let Some(h) = self.lower_del_target(t, *span)? {
                        hs.push(h);
                    }
                }
                out.push(self.stmt(*span, None, HStmtKind::Del { targets: hs }));
                Ok(())
            }
            Stmt::Assert {
                cond,
                message,
                span,
            } => {
                let c = self.lower_expr(cond)?;
                let m = match message {
                    Some(e) => Some(self.lower_expr(e)?),
                    None => None,
                };
                out.push(self.stmt(
                    *span,
                    None,
                    HStmtKind::Assert {
                        cond: c,
                        message: m,
                    },
                ));
                Ok(())
            }
            Stmt::Expr(e) => {
                let h = self.lower_expr(e)?;
                out.push(self.stmt(h.span, None, HStmtKind::Expr(h)));
                Ok(())
            }
            // Declarations leave no nodes: types, methods and functions
            // were indexed in phase 1 and lower separately.
            Stmt::TypeDecl { .. } | Stmt::Fn { .. } | Stmt::Impl { .. } => Ok(()),
            Stmt::Import {
                module,
                alias,
                span,
            } => {
                let mid = self.module_id(module, *span)?;
                let bind = alias.clone().unwrap_or_else(|| module.clone());
                self.env.insert(bind.clone(), NameRef::Module(mid));
                out.push(self.stmt(*span, Some(&bind), HStmtKind::EnsureInit { module: mid }));
                Ok(())
            }
            Stmt::FromImport {
                module,
                names,
                span,
            } => {
                let mid = self.module_id(module, *span)?;
                out.push(self.stmt(*span, Some(module), HStmtKind::EnsureInit { module: mid }));
                for (name, alias) in names {
                    let bind = alias.clone().unwrap_or_else(|| name.clone());
                    self.import_name(mid, module, name, &bind, *span, out)?;
                }
                Ok(())
            }
        }
    }

    /// An assignment target as a write position. Index and field parts
    /// are lowered (they evaluate at runtime); plain names resolve to
    /// their slot or global. Evaluation order lives in the `Assign`
    /// contract (target parts, then values, then stores), not here.
    fn lower_assign_target(&mut self, t: &Target, span: Span) -> LResult<HTarget> {
        match t {
            Target::Name(n) => Ok(self.assign_name_target(n, span)?.0),
            Target::Index { base, index, .. } => {
                let b = self.lower_expr(base)?;
                let ix = self.lower_expr(index)?;
                let rule = self.index_rule(&b.ty, span)?;
                Ok(HTarget::Index {
                    base: Box::new(b),
                    index: Box::new(ix),
                    rule,
                })
            }
            Target::Attr { base, field, .. } => {
                let b = self.lower_expr(base)?;
                let fr = self.field_ref(&b.ty, field, span)?;
                Ok(HTarget::Field {
                    base: Box::new(b),
                    field: fr,
                })
            }
        }
    }

    /// A bare-name write position: the existing slot in a function
    /// (rebinding never touches a same-named global), the pre-declared
    /// global at top level. Shadowing a module, function or type alias
    /// births a slot (the checker proved the name game legal).
    /// Returns the target and the slot/global type.
    fn assign_name_target(&mut self, name: &str, span: Span) -> LResult<(HTarget, HTy)> {
        if let Some(r) = self.env.get(name).cloned() {
            match r {
                NameRef::Slot(s) => return Ok((HTarget::Slot(s), self.slot_ty(s))),
                NameRef::Global(g) => return Ok((HTarget::Global(g), self.global_ty(g))),
                _ => {}
            }
        }
        if self.is_top {
            let g = self.global_id(name, span)?;
            let ty = self.global_ty(g);
            self.env.insert(name.to_string(), NameRef::Global(g));
            return Ok((HTarget::Global(g), ty));
        }
        let ty = self.local_ty(name, span)?;
        let s = self.alloc_slot(ty.clone());
        self.env.insert(name.to_string(), NameRef::Slot(s));
        Ok((HTarget::Slot(s), ty))
    }

    /// A global by name in the current module (reads only; writes go
    /// through `assign_name_target`).
    fn global_id(&self, name: &str, span: Span) -> LResult<GlobalId> {
        self.ctx
            .global_ids
            .get(&(self.module.clone(), name.to_string()))
            .copied()
            .ok_or_else(|| {
                lerr(
                    span,
                    format!("internal: global '{name}' was never declared"),
                )
            })
    }

    /// Read a write position back as a value (compound assignment and
    /// `mut self` write-back). Reuses the target's already-lowered parts:
    /// lowering them again would run side effects twice.
    fn read_target(&self, t: &HTarget, span: Span) -> LResult<HExpr> {
        match t {
            HTarget::Slot(s) => {
                let ty = self.slot_ty(*s);
                Ok(self.expr(span, None, ty, HExprKind::Place(Place::Slot(*s))))
            }
            HTarget::Global(g) => {
                let ty = self.global_ty(*g);
                Ok(self.expr(span, None, ty, HExprKind::Place(Place::Global(*g))))
            }
            HTarget::Index { base, index, rule } => {
                let ty = self.index_elem_ty(&base.ty, span)?;
                Ok(self.expr(
                    span,
                    None,
                    ty,
                    HExprKind::Index {
                        base: base.clone(),
                        index: index.clone(),
                        rule: *rule,
                    },
                ))
            }
            HTarget::Field { base, field } => {
                let ty = self.field_ty_of(&base.ty, *field, span)?;
                Ok(self.expr(
                    span,
                    None,
                    ty,
                    HExprKind::Field {
                        base: base.clone(),
                        field: *field,
                    },
                ))
            }
        }
    }

    /// Evaluate a compound-assignment target's parts once, in order, and
    /// rebuild the target off the lowered parts. The caller lowers the
    /// value next, then reads through `read_target`: each source
    /// expression is lowered exactly once, target parts before the value,
    /// which is Python's evaluation order.
    fn stash_target(&mut self, t: &Target, span: Span) -> LResult<HTarget> {
        self.lower_assign_target(t, span)
    }

    fn restore_env(&mut self, var: &str, saved: Option<NameRef>) {
        match saved {
            Some(r) => {
                self.env.insert(var.to_string(), r);
            }
            None => {
                self.env.remove(var);
            }
        }
    }

    fn module_id(&self, name: &str, span: Span) -> LResult<ModuleId> {
        // A module alias in this body, else any known module. Imports
        // bind the alias; the bare name resolves when some loaded module
        // has it (checked programs always import before use here).
        if let Some(NameRef::Module(mid)) = self.env.get(name) {
            return Ok(*mid);
        }
        if let Some(mid) = self.ctx.tables.module_id.get(name) {
            return Ok(*mid);
        }
        Err(lerr(
            span,
            format!("internal: module '{name}' is not imported here"),
        ))
    }

    /// Bind one from-imported name: module refs, functions and types
    /// alias directly; a value global materializes as an initializing
    /// assignment into a fresh slot (eager, exactly like the backend's
    /// fresh local -- a lazy global read would see later mutations).
    fn import_name(
        &mut self,
        mid: ModuleId,
        module: &str,
        name: &str,
        bind: &str,
        span: Span,
        out: &mut Vec<HStmt>,
    ) -> LResult<()> {
        let home = &self.ctx.tables.modules_sorted[mid.0 as usize];
        if self
            .ctx
            .tables
            .type_id
            .contains_key(&(home.clone(), name.to_string()))
        {
            let tid = resolve_type_name(self.ctx.tables, home, name).ok_or_else(|| {
                lerr(span, format!("internal: unresolvable import type '{name}'"))
            })?;
            self.env.insert(bind.to_string(), NameRef::Type(tid));
            return Ok(());
        }
        if let Some(fid) = self
            .ctx
            .tables
            .func_id
            .get(&(home.clone(), name.to_string()))
        {
            self.env.insert(bind.to_string(), NameRef::Func(*fid));
            return Ok(());
        }
        let gid = self
            .ctx
            .global_ids
            .get(&(home.clone(), name.to_string()))
            .copied()
            .ok_or_else(|| {
                lerr(
                    span,
                    format!("internal: '{module}.{name}' is not an importable value"),
                )
            })?;
        let ty = self.global_ty(gid);
        let s = self.alloc_slot(ty.clone());
        self.env.insert(bind.to_string(), NameRef::Slot(s));
        let read = self.expr(
            span,
            Some(name),
            ty.clone(),
            HExprKind::Place(Place::Global(gid)),
        );
        out.push(self.stmt(
            span,
            Some(bind),
            HStmtKind::Assign {
                targets: vec![HTarget::Slot(s)],
                values: vec![read],
                rules: vec![copy_rule(&ty)],
            },
        ));
        Ok(())
    }

    fn lower_del_target(&mut self, t: &Target, span: Span) -> LResult<Option<HDelTarget>> {
        match t {
            Target::Name(n) => match self.env.get(n).cloned() {
                Some(NameRef::Slot(s)) => {
                    self.env.remove(n);
                    Ok(Some(HDelTarget {
                        target: HTarget::Slot(s),
                        rule: DelRule::Unbind,
                    }))
                }
                Some(NameRef::Global(g)) => {
                    self.env.remove(n);
                    Ok(Some(HDelTarget {
                        target: HTarget::Global(g),
                        rule: DelRule::Unbind,
                    }))
                }
                // Module, function and type aliases hold no storage: the
                // checker proved any later use undefined, so forgetting
                // the alias is the whole effect.
                Some(_) => {
                    self.env.remove(n);
                    Ok(None)
                }
                None => Err(lerr(span, format!("internal: deleting unbound '{n}'"))),
            },
            Target::Index { base, index, .. } => {
                let b = self.lower_expr(base)?;
                let ix = self.lower_expr(index)?;
                let rule = match &b.ty {
                    HTy::List(_) => DelRule::ListRemove,
                    HTy::Dict(_) => DelRule::DictRemove,
                    HTy::Unknown => DelRule::Dynamic,
                    other => {
                        return Err(lerr(
                            span,
                            format!("internal: cannot delete into {other:?}"),
                        ));
                    }
                };
                let index_rule = match rule {
                    DelRule::ListRemove => IndexRule::ListInt,
                    DelRule::DictRemove => IndexRule::DictKey,
                    _ => IndexRule::Dynamic,
                };
                Ok(Some(HDelTarget {
                    target: HTarget::Index {
                        base: Box::new(b),
                        index: Box::new(ix),
                        rule: index_rule,
                    },
                    rule,
                }))
            }
            Target::Attr { base, field, .. } => {
                let b = self.lower_expr(base)?;
                match &b.ty {
                    HTy::Record(_) => {
                        let fr = self.field_ref(&b.ty, field, span)?;
                        Ok(Some(HDelTarget {
                            target: HTarget::Field {
                                base: Box::new(b),
                                field: fr,
                            },
                            rule: DelRule::RecordBlank,
                        }))
                    }
                    HTy::Unknown => {
                        let fr = self.field_ref(&b.ty, field, span)?;
                        Ok(Some(HDelTarget {
                            target: HTarget::Field {
                                base: Box::new(b),
                                field: fr,
                            },
                            rule: DelRule::Dynamic,
                        }))
                    }
                    other => Err(lerr(
                        span,
                        format!("internal: cannot delete a field of {other:?}"),
                    )),
                }
            }
        }
    }

    fn intern(&mut self, name: &str) -> StrId {
        if let Some(id) = self.string_ids.get(name) {
            return *id;
        }
        let id = StrId(self.strings.len() as u32);
        self.strings.push(name.to_string());
        self.string_ids.insert(name.to_string(), id);
        id
    }

    /// A field of a value: constant offset for known records, interned
    /// runtime name for dynamic bases. Anything else was rejected by
    /// the checker.
    fn field_ref(&mut self, base: &HTy, field: &str, span: Span) -> LResult<FieldRef> {
        match base {
            HTy::Record(tid) => {
                let names: Vec<String> = self
                    .ctx
                    .tables
                    .type_decls
                    .get(tid.0 as usize)
                    .map(|d| d.fields.iter().map(|(n, _)| n.clone()).collect())
                    .unwrap_or_default();
                match names.iter().position(|f| f == field) {
                    Some(i) => Ok(FieldRef::Static(FieldIdx(i))),
                    None => Err(lerr(span, format!("internal: no field '{field}'"))),
                }
            }
            HTy::Unknown => {
                let id = self.intern(field);
                Ok(FieldRef::Dynamic(id))
            }
            other => Err(lerr(span, format!("internal: no fields on {other:?}"))),
        }
    }

    /// A field value's type: layout lookup for static offsets, dynamic
    /// for runtime-resolved names.
    fn field_ty_of(&self, base: &HTy, field: FieldRef, span: Span) -> LResult<HTy> {
        match (base, field) {
            (HTy::Record(tid), FieldRef::Static(idx)) => self.lower_field_ty(*tid, idx, span),
            (_, FieldRef::Dynamic(_)) => Ok(HTy::Unknown),
            (other, _) => Err(lerr(span, format!("internal: no fields on {other:?}"))),
        }
    }

    fn lower_field_ty(&self, tid: TypeId, idx: FieldIdx, span: Span) -> LResult<HTy> {
        let decl = self
            .ctx
            .tables
            .type_decls
            .get(tid.0 as usize)
            .ok_or_else(|| lerr(span, format!("internal: no such type id {}", tid.0)))?;
        let (_, spelling) = decl.fields.get(idx.0).ok_or_else(|| {
            lerr(
                span,
                format!("internal: field index {} out of range", idx.0),
            )
        })?;
        conv_ty_str(self.ctx.tables, spelling, &decl.module.clone())
    }

    /// Element type of an index read, mirroring the checker.
    fn index_elem_ty(&self, base: &HTy, span: Span) -> LResult<HTy> {
        match base {
            HTy::List(t) => Ok((**t).clone()),
            HTy::Str => Ok(HTy::Str),
            HTy::Dict(_) | HTy::Unknown => Ok(HTy::Unknown),
            other => Err(lerr(span, format!("internal: cannot index into {other:?}"))),
        }
    }

    fn index_rule(&self, base: &HTy, span: Span) -> LResult<IndexRule> {
        match base {
            HTy::List(_) => Ok(IndexRule::ListInt),
            HTy::Str => Ok(IndexRule::StrChar),
            HTy::Dict(_) => Ok(IndexRule::DictKey),
            HTy::Unknown => Ok(IndexRule::Dynamic),
            other => Err(lerr(span, format!("internal: cannot index into {other:?}"))),
        }
    }

    fn slice_rule(&self, base: &HTy, span: Span) -> LResult<SliceRule> {
        match base {
            HTy::List(_) => Ok(SliceRule::ListCopy),
            HTy::Str => Ok(SliceRule::StrChars),
            HTy::Unknown => Ok(SliceRule::Dynamic),
            other => Err(lerr(span, format!("internal: cannot slice {other:?}"))),
        }
    }

    fn iter_rule(&self, it: &HTy, span: Span) -> LResult<IterRule> {
        match it {
            HTy::List(_) => Ok(IterRule::List),
            HTy::Str => Ok(IterRule::StrChars),
            HTy::Dict(_) => Ok(IterRule::DictKeys),
            HTy::Unknown => Ok(IterRule::Dynamic),
            other => Err(lerr(
                span,
                format!("internal: cannot iterate over {other:?}"),
            )),
        }
    }

    fn iter_elem_ty(&self, it: &HTy, span: Span) -> LResult<HTy> {
        match it {
            HTy::List(t) => Ok((**t).clone()),
            HTy::Str => Ok(HTy::Str),
            HTy::Dict(_) | HTy::Unknown => Ok(HTy::Unknown),
            other => Err(lerr(
                span,
                format!("internal: cannot iterate over {other:?}"),
            )),
        }
    }

    fn member_rule(&self, hay: &HTy, span: Span) -> LResult<MemberRule> {
        match hay {
            HTy::List(_) => Ok(MemberRule::ListEq),
            HTy::Str => Ok(MemberRule::StrSub),
            HTy::Dict(_) => Ok(MemberRule::DictKey),
            HTy::Unknown => Ok(MemberRule::Dynamic),
            other => Err(lerr(
                span,
                format!("internal: cannot test membership in {other:?}"),
            )),
        }
    }

    fn eq_rule(&self, l: &HTy, r: &HTy, span: Span) -> LResult<EqRule> {
        if matches!(l, HTy::Unknown) || matches!(r, HTy::Unknown) {
            return Ok(EqRule::Dynamic);
        }
        if hty_numeric(l) && hty_numeric(r) {
            return Ok(EqRule::Numeric);
        }
        match (l, r) {
            (HTy::Str, HTy::Str) => Ok(EqRule::StrEq),
            (HTy::List(_), HTy::List(_))
            | (HTy::Dict(_), HTy::Dict(_))
            | (HTy::Record(_), HTy::Record(_)) => Ok(EqRule::Structural),
            (HTy::None, HTy::None) => Ok(EqRule::IdentityNone),
            _ => Err(lerr(
                span,
                format!("internal: cannot compare {l:?} and {r:?}"),
            )),
        }
    }

    fn cmp_rule(&self, l: &HTy, r: &HTy, span: Span) -> LResult<CmpRule> {
        if matches!(l, HTy::Unknown) || matches!(r, HTy::Unknown) {
            return Ok(CmpRule::Dynamic);
        }
        if hty_numeric(l) && hty_numeric(r) {
            return Ok(CmpRule::Numeric);
        }
        match (l, r) {
            (HTy::Str, HTy::Str) => Ok(CmpRule::StrOrder),
            _ => Err(lerr(
                span,
                format!("internal: cannot order {l:?} and {r:?}"),
            )),
        }
    }

    fn unary_rule(&self, op: UnaryOp, operand: &HTy, span: Span) -> LResult<UnaryRule> {
        match (op, operand) {
            (UnaryOp::Neg, HTy::Int) => Ok(UnaryRule::Neg(ArithRule::Trap)),
            (UnaryOp::Neg, HTy::Float) => Ok(UnaryRule::Neg(ArithRule::Float)),
            (UnaryOp::Not, HTy::Bool) => Ok(UnaryRule::Not),
            (UnaryOp::BitNot, HTy::Int) => Ok(UnaryRule::BitNot),
            (UnaryOp::Pos, HTy::Int) | (UnaryOp::Pos, HTy::Float) => Ok(UnaryRule::Pos),
            (_, HTy::Unknown) => Ok(UnaryRule::Dynamic),
            _ => Err(lerr(
                span,
                format!("internal: '{op:?}' not supported for {operand:?}"),
            )),
        }
    }

    /// If-expression join, mirroring the checker exactly: compatible
    /// sides keep the known one, `None` on either side is the optional
    /// idiom, anything else was already rejected.
    fn join_ty(&self, t: &HTy, e: &HTy, span: Span) -> LResult<HTy> {
        if hty_compatible(t, e) {
            if matches!(t, HTy::Unknown) {
                return Ok(e.clone());
            }
            return Ok(t.clone());
        }
        if matches!(t, HTy::None) {
            return Ok(e.clone());
        }
        if matches!(e, HTy::None) {
            return Ok(t.clone());
        }
        Err(lerr(
            span,
            format!("internal: branches disagree: {t:?} vs {e:?}"),
        ))
    }

    /// A bare variable read. Resolution mirrors the checker's
    /// `check_expr` order for values: locals, then globals, then import
    /// aliases. Function, type and module aliases are callable,
    /// constructible or callable-through -- never values -- so reaching
    /// one here means the checker already rejected the program.
    fn lower_var(&mut self, name: &str, span: Span) -> LResult<HExpr> {
        if let Some(r) = self.env.get(name).cloned() {
            match r {
                NameRef::Slot(s) => {
                    let ty = self.slot_ty(s);
                    return Ok(self.expr(span, Some(name), ty, HExprKind::Place(Place::Slot(s))));
                }
                NameRef::Global(g) => {
                    let ty = self.global_ty(g);
                    return Ok(self.expr(span, Some(name), ty, HExprKind::Place(Place::Global(g))));
                }
                _ => {
                    return Err(lerr(span, format!("internal: '{name}' is not a value")));
                }
            }
        }
        // Globals bound before any use in this body are pre-declared;
        // anything else never reaches lowering through checked code.
        if let Some(g) = self
            .ctx
            .global_ids
            .get(&(self.module.clone(), name.to_string()))
        {
            let ty = self.global_ty(*g);
            self.env.insert(name.to_string(), NameRef::Global(*g));
            return Ok(self.expr(span, Some(name), ty, HExprKind::Place(Place::Global(*g))));
        }
        Err(lerr(span, format!("internal: undefined variable '{name}'")))
    }

    /// A method on a record type visible here: the type's own impls
    /// (the orphan rule puts every impl with its type), which covers
    /// from-imported types too -- their declarations traveled with them.
    /// Anything missing was rejected by the checker.
    fn resolve_method(&self, tid: TypeId, method: &str, span: Span) -> LResult<MethodId> {
        self.ctx
            .tables
            .method_id
            .get(&(tid, method.to_string()))
            .copied()
            .ok_or_else(|| lerr(span, format!("internal: no such method '{method}'")))
    }

    /// An [`HTy`] back to the checker's spelling, so operator result
    /// types come from `nx_types::arith_result` -- the single owner of
    /// Int/Float promotion -- instead of a second matrix here. Records
    /// become their canonical declared name, which is what the checker's
    /// `Ty::Record` holds.
    fn to_ty(&self, h: &HTy) -> Ty {
        match h {
            HTy::Int => Ty::Int,
            HTy::Float => Ty::Float,
            HTy::Bool => Ty::Bool,
            HTy::Str => Ty::Str,
            HTy::None => Ty::None,
            HTy::Unknown => Ty::Unknown,
            HTy::List(t) => Ty::List(Box::new(self.to_ty(t))),
            HTy::Dict(t) => Ty::Dict(Box::new(self.to_ty(t))),
            HTy::Record(tid) => Ty::Record(self.type_name(*tid)),
        }
    }

    /// The declared (canonical) name of a record type.
    fn type_name(&self, tid: TypeId) -> String {
        self.ctx
            .tables
            .type_decls
            .get(tid.0 as usize)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| format!("type#{}", tid.0))
    }

    /// Result type of an arithmetic, power or bitwise operator. The
    /// matrix is the checker's; only the bitwise arm is spelled out,
    /// because the checker answers `Int` for it regardless of unknown
    /// operands (shifts and bit operators never widen).
    fn bin_result_ty(&self, op: BinOp, l: &HTy, r: &HTy, span: Span) -> LResult<HTy> {
        match op {
            BinOp::Shl | BinOp::Shr | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => Ok(HTy::Int),
            _ => {
                let lt = self.to_ty(l);
                let rt = self.to_ty(r);
                match nx_types::arith_result(&lt, op, &rt) {
                    Some(t) => conv_ty(self.ctx.tables, &t, &self.module, span),
                    None => Err(lerr(
                        span,
                        format!("internal: no result type for '{op:?}' on {l:?} and {r:?}"),
                    )),
                }
            }
        }
    }

    /// Element type of a slice: a fresh list for a list base,
    /// characters for a string base, dynamic otherwise.
    fn slice_ty(&self, base: &HTy, span: Span) -> LResult<HTy> {
        match base {
            HTy::List(t) => Ok(HTy::List(t.clone())),
            HTy::Str => Ok(HTy::Str),
            HTy::Unknown => Ok(HTy::Unknown),
            other => Err(lerr(span, format!("internal: cannot slice {other:?}"))),
        }
    }

    fn opt_expr(&mut self, e: &Option<Box<Expr>>) -> LResult<Option<Box<HExpr>>> {
        match e {
            Some(x) => Ok(Some(Box::new(self.lower_expr(x)?))),
            None => Ok(None),
        }
    }

    /// Lower one expression.
    fn lower_expr(&mut self, e: &Expr) -> LResult<HExpr> {
        match e {
            Expr::Int(v, sp) => Ok(self.expr(*sp, None, HTy::Int, HExprKind::Int(*v))),
            Expr::Float(v, sp) => Ok(self.expr(*sp, None, HTy::Float, HExprKind::Float(*v))),
            Expr::Bool(v, sp) => Ok(self.expr(*sp, None, HTy::Bool, HExprKind::Bool(*v))),
            Expr::Str(v, sp) => Ok(self.expr(*sp, None, HTy::Str, HExprKind::Str(v.clone()))),
            Expr::NoneLit(sp) => Ok(self.expr(*sp, None, HTy::None, HExprKind::None)),
            Expr::List(items, sp) => {
                let mut hs = Vec::with_capacity(items.len());
                let mut elem = HTy::Unknown;
                for it in items {
                    let h = self.lower_expr(it)?;
                    if matches!(elem, HTy::Unknown) {
                        elem = h.ty.clone();
                    }
                    hs.push(h);
                }
                let ty = HTy::List(Box::new(elem));
                Ok(self.expr(*sp, None, ty, HExprKind::List(hs)))
            }
            Expr::Range { start, end, span } => {
                let s = self.lower_expr(start)?;
                let t = self.lower_expr(end)?;
                let ty = HTy::List(Box::new(HTy::Int));
                Ok(self.expr(
                    *span,
                    None,
                    ty,
                    HExprKind::Range {
                        start: Box::new(s),
                        end: Box::new(t),
                        rule: RangeRule::AscendingOrEmpty,
                    },
                ))
            }
            Expr::Dict(pairs, sp) => {
                let mut hp = Vec::with_capacity(pairs.len());
                let mut vt = HTy::Unknown;
                for (k, v) in pairs {
                    let hk = self.lower_expr(k)?;
                    let hv = self.lower_expr(v)?;
                    if matches!(vt, HTy::Unknown) {
                        vt = hv.ty.clone();
                    }
                    hp.push((hk, hv));
                }
                let ty = HTy::Dict(Box::new(vt));
                Ok(self.expr(*sp, None, ty, HExprKind::Dict(hp)))
            }
            Expr::Var(name, sp) => self.lower_var(name, *sp),
            Expr::Attr { base, attr, span } => self.lower_attr(base, attr, *span),
            Expr::Index { base, index, span } => {
                let b = self.lower_expr(base)?;
                let ix = self.lower_expr(index)?;
                let rule = self.index_rule(&b.ty, *span)?;
                let ty = self.index_elem_ty(&b.ty, *span)?;
                Ok(self.expr(
                    *span,
                    None,
                    ty,
                    HExprKind::Index {
                        base: Box::new(b),
                        index: Box::new(ix),
                        rule,
                    },
                ))
            }
            Expr::Slice {
                base,
                from,
                to,
                step,
                span,
            } => {
                let b = self.lower_expr(base)?;
                let rule = self.slice_rule(&b.ty, *span)?;
                let ty = self.slice_ty(&b.ty, *span)?;
                let hf = self.opt_expr(from)?;
                let ht = self.opt_expr(to)?;
                let hs = self.opt_expr(step)?;
                Ok(self.expr(
                    *span,
                    None,
                    ty,
                    HExprKind::Slice {
                        base: Box::new(b),
                        from: hf,
                        to: ht,
                        step: hs,
                        rule,
                    },
                ))
            }
            Expr::Unary {
                op,
                expr: operand,
                span,
            } => {
                let h = self.lower_expr(operand)?;
                let rule = self.unary_rule(*op, &h.ty, *span)?;
                // Mirrors the checker: `not` answers Bool on a known Bool
                // and stays dynamic on an unknown operand.
                let ty = match (op, &h.ty) {
                    (UnaryOp::Not, HTy::Unknown) => HTy::Unknown,
                    (UnaryOp::Not, _) => HTy::Bool,
                    _ => h.ty.clone(),
                };
                Ok(self.expr(
                    *span,
                    None,
                    ty,
                    HExprKind::Unary {
                        op: *op,
                        rule,
                        operand: Box::new(h),
                    },
                ))
            }
            Expr::Binary {
                left,
                op,
                right,
                span,
            } => self.lower_binary(left, *op, right, *span),
            Expr::IfExpr {
                cond,
                then_value,
                else_value,
                span,
            } => {
                let c = self.lower_expr(cond)?;
                let t = self.lower_expr(then_value)?;
                let f = self.lower_expr(else_value)?;
                let ty = self.join_ty(&t.ty, &f.ty, *span)?;
                Ok(self.expr(
                    *span,
                    None,
                    ty,
                    HExprKind::Select {
                        cond: Box::new(c),
                        then_value: Box::new(t),
                        else_value: Box::new(f),
                    },
                ))
            }
            Expr::Comprehension {
                element,
                var,
                iter,
                cond,
                span,
            } => {
                let it = self.lower_expr(iter)?;
                let rule = self.iter_rule(&it.ty, *span)?;
                let ety = self.iter_elem_ty(&it.ty, *span)?;
                let slot = self.alloc_slot(ety);
                let saved = self.env.insert(var.clone(), NameRef::Slot(slot));
                let el = self.lower_expr(element)?;
                let hc = self.opt_expr(cond)?;
                self.restore_env(var, saved);
                // Mirrors the checker: a comprehension over a string
                // *is* a string (one character per element), while a
                // list or dict-keys comprehension builds a list.
                let ty = match (&it.ty, rule) {
                    (HTy::Str, IterRule::StrChars) => HTy::Str,
                    (HTy::List(_), IterRule::List) => HTy::List(Box::new(el.ty.clone())),
                    (HTy::Dict(_), IterRule::DictKeys) => HTy::List(Box::new(HTy::Unknown)),
                    _ => HTy::List(Box::new(el.ty.clone())),
                };
                Ok(self.expr(
                    *span,
                    Some(var),
                    ty,
                    HExprKind::Compr {
                        element: Box::new(el),
                        var: slot,
                        iter: Box::new(it),
                        rule,
                        cond: hc,
                    },
                ))
            }
            Expr::Call { callee, args, span } => self.lower_call(callee, args, *span),
        }
    }

    /// One binary operator, split across four nodes because four
    /// relations answer different questions with different rules:
    /// `and`/`or` short-circuit, `==`/`!=` compare structurally,
    /// orderings order, `in`/`not in` test membership.
    fn lower_binary(&mut self, left: &Expr, op: BinOp, right: &Expr, span: Span) -> LResult<HExpr> {
        use BinOp::*;
        let l = self.lower_expr(left)?;
        let r = self.lower_expr(right)?;
        let kind = match op {
            And | Or => HExprKind::Logic {
                op,
                left: Box::new(l),
                right: Box::new(r),
            },
            Eq | NotEq => {
                let rule = self.eq_rule(&l.ty, &r.ty, span)?;
                HExprKind::Equal {
                    left: Box::new(l),
                    op,
                    rule,
                    right: Box::new(r),
                }
            }
            Lt | LtEq | Gt | GtEq => {
                let rule = self.cmp_rule(&l.ty, &r.ty, span)?;
                HExprKind::Compare {
                    left: Box::new(l),
                    op,
                    rule,
                    right: Box::new(r),
                }
            }
            In | NotIn => {
                let rule = self.member_rule(&r.ty, span)?;
                HExprKind::Contains {
                    needle: Box::new(l),
                    hay: Box::new(r),
                    rule,
                    negated: matches!(op, NotIn),
                }
            }
            _ => {
                let rule = decide_bin_rule(op, &l.ty, &r.ty, span)?;
                let ty = self.bin_result_ty(op, &l.ty, &r.ty, span)?;
                return Ok(self.expr(
                    span,
                    None,
                    ty,
                    HExprKind::Binary {
                        left: Box::new(l),
                        op,
                        rule,
                        right: Box::new(r),
                    },
                ));
            }
        };
        Ok(self.expr(span, None, HTy::Bool, kind))
    }

    /// A field read. A module base reads the module's global (a module
    /// is not a value, so nothing evaluates); a record base resolves to
    /// a constant offset; an unknown base keeps the interned name for
    /// runtime lookup. Every other base shape was rejected.
    fn lower_attr(&mut self, base: &Expr, attr: &str, span: Span) -> LResult<HExpr> {
        if let Some(mid) = self.module_base(base)? {
            let home = self.ctx.tables.modules_sorted[mid.0 as usize].clone();
            let gid = self
                .ctx
                .global_ids
                .get(&(home.clone(), attr.to_string()))
                .copied()
                .ok_or_else(|| lerr(span, format!("internal: '{home}' has no member '{attr}'")))?;
            let ty = self.global_ty(gid);
            return Ok(self.expr(span, Some(attr), ty, HExprKind::Place(Place::Global(gid))));
        }
        let b = self.lower_expr(base)?;
        let field = self.field_ref(&b.ty, attr, span)?;
        let ty = self.field_ty_of(&b.ty, field, span)?;
        Ok(self.expr(
            span,
            Some(attr),
            ty,
            HExprKind::Field {
                base: Box::new(b),
                field,
            },
        ))
    }

    /// The module a bare name refers to, if any.
    fn module_base(&self, base: &Expr) -> LResult<Option<ModuleId>> {
        let Expr::Var(name, span) = base else {
            return Ok(None);
        };
        if let Some(NameRef::Module(mid)) = self.env.get(name) {
            return Ok(Some(*mid));
        }
        match self.ctx.tables.module_id.get(name) {
            Some(mid) => Ok(Some(*mid)),
            None => {
                let _ = span;
                Ok(None)
            }
        }
    }

    /// A type visible here by bare name.
    fn visible_type(&self, name: &str) -> Option<TypeId> {
        if let Some(NameRef::Type(tid)) = self.env.get(name) {
            return Some(*tid);
        }
        self.ctx
            .visible_types
            .get(&(self.module.clone(), name.to_string()))
            .copied()
    }

    /// A function visible here by bare name.
    fn visible_func(&self, name: &str) -> Option<FuncId> {
        if let Some(NameRef::Func(fid)) = self.env.get(name) {
            return Some(*fid);
        }
        self.ctx
            .visible_funcs
            .get(&(self.module.clone(), name.to_string()))
            .copied()
    }

    /// The declared return type of a function body, from the checker's
    /// inference for its key.
    fn func_ret(&self, fid: FuncId, span: Span) -> LResult<HTy> {
        let decl = self
            .ctx
            .tables
            .func_decls
            .get(fid.0 as usize)
            .ok_or_else(|| lerr(span, format!("internal: no such function id {}", fid.0)))?;
        let info = self.ctx.fn_info(&decl.module, &decl.name)?;
        conv_ty(self.ctx.tables, &info.ret, &decl.module, decl.span)
    }

    /// A method body key is its canonical type plus the method name,
    /// mirroring the checker's inference keys. The declaring module
    /// comes from the body the method points at, so a from-imported
    /// type's methods are typed against their own module's inference.
    fn method_ret(&self, mid: MethodId, span: Span) -> LResult<HTy> {
        let decl = self
            .ctx
            .tables
            .method_decls
            .get(mid.0 as usize)
            .ok_or_else(|| lerr(span, format!("internal: no such method id {}", mid.0)))?;
        let body = self
            .ctx
            .tables
            .func_decls
            .get(decl.func.0 as usize)
            .ok_or_else(|| {
                lerr(
                    span,
                    format!("internal: no such function id {}", decl.func.0),
                )
            })?;
        let canon = self.type_name(decl.type_id);
        let info = self
            .ctx
            .fn_info(&body.module, &nx_ast::shape::method_key(&canon, &decl.name))?;
        conv_ty(self.ctx.tables, &info.ret, &body.module, span)
    }

    /// A call. Resolution order is the checker's, and it is
    /// load-bearing: a type name shadows nothing but shares the
    /// `Name(...)` spelling with a function; `T.m(...)` resolves before
    /// the base is evaluated, because a type is not a binding; a module
    /// base always means a module function; a known record means an
    /// impl method; and only then does builtin sugar apply.
    fn lower_call(&mut self, callee: &Expr, args: &[Expr], span: Span) -> LResult<HExpr> {
        // `T(...)`: construction, same spelling as a function call.
        if let Expr::Var(name, _) = callee {
            if let Some(tid) = self.visible_type(name) {
                let hs = self.lower_args(args)?;
                return Ok(self.expr(
                    span,
                    Some(name),
                    HTy::Record(tid),
                    HExprKind::Construct {
                        type_id: tid,
                        args: hs,
                    },
                ));
            }
        }
        if let Expr::Attr { base, attr, .. } = callee {
            // `m.T(...)`: the same construction through a module alias,
            // resolved against the module's own declarations.
            if let Some(mid) = self.module_base(base)? {
                let home = self.ctx.tables.modules_sorted[mid.0 as usize].clone();
                if let Some(tid) = self
                    .ctx
                    .tables
                    .type_id
                    .get(&(home.clone(), attr.clone()))
                    .copied()
                {
                    let hs = self.lower_args(args)?;
                    return Ok(self.expr(
                        span,
                        Some(attr),
                        HTy::Record(tid),
                        HExprKind::Construct {
                            type_id: tid,
                            args: hs,
                        },
                    ));
                }
            }
            // `T.m(...)`: an associated function. Nothing evaluates --
            // a type name has no storage.
            if let Expr::Var(n, _) = base.as_ref() {
                if let Some(tid) = self.visible_type(n) {
                    let mid = self.resolve_method(tid, attr, span)?;
                    let ret = self.method_ret(mid, span)?;
                    let hs = self.lower_args(args)?;
                    return Ok(self.expr(
                        span,
                        Some(attr),
                        ret,
                        HExprKind::CallMethod {
                            method: mid,
                            receiver: None,
                            args: hs,
                            writeback: None,
                        },
                    ));
                }
            }
            // `m.f(...)`: a module function. Modules are not values, so
            // the base contributes nothing.
            if let Some(mid) = self.module_base(base)? {
                let home = self.ctx.tables.modules_sorted[mid.0 as usize].clone();
                let fid = *self
                    .ctx
                    .tables
                    .func_id
                    .get(&(home.clone(), attr.clone()))
                    .ok_or_else(|| {
                        lerr(span, format!("internal: '{home}' has no function '{attr}'"))
                    })?;
                let ret = self.func_ret(fid, span)?;
                let hs = self.lower_args(args)?;
                return Ok(self.expr(
                    span,
                    Some(attr),
                    ret,
                    HExprKind::CallFn {
                        func: fid,
                        args: hs,
                    },
                ));
            }
            // `v.m(...)`: an impl method on a known record, else builtin
            // sugar. The base evaluates exactly once either way.
            let b = self.lower_expr(base)?;
            if let HTy::Record(tid) = b.ty {
                let mid = self.resolve_method(tid, attr, span)?;
                return self.lower_method_call(mid, b, attr, args, span);
            }
            if shape::builtin_arity(attr).is_some() {
                let op = builtin_op(attr);
                let mut hs = Vec::with_capacity(args.len() + 1);
                hs.push(b);
                for a in args {
                    hs.push(self.lower_expr(a)?);
                }
                let ty = builtin_ret(op);
                return Ok(self.expr(span, Some(attr), ty, HExprKind::Builtin { op, args: hs }));
            }
            return Err(lerr(
                span,
                format!("internal: cannot resolve method '{attr}'"),
            ));
        }
        // `f(...)`: an ambient builtin or a visible function.
        if let Expr::Var(name, _) = callee {
            if shape::builtin_arity(name).is_some() {
                let op = builtin_op(name);
                let hs = self.lower_args(args)?;
                let ty = builtin_ret(op);
                return Ok(self.expr(span, Some(name), ty, HExprKind::Builtin { op, args: hs }));
            }
            if let Some(fid) = self.visible_func(name) {
                let ret = self.func_ret(fid, span)?;
                let hs = self.lower_args(args)?;
                return Ok(self.expr(
                    span,
                    Some(name),
                    ret,
                    HExprKind::CallFn {
                        func: fid,
                        args: hs,
                    },
                ));
            }
        }
        Err(lerr(span, "internal: not a call"))
    }

    fn lower_args(&mut self, args: &[Expr]) -> LResult<Vec<HExpr>> {
        let mut hs = Vec::with_capacity(args.len());
        for a in args {
            hs.push(self.lower_expr(a)?);
        }
        Ok(hs)
    }

    /// An impl method call. A `mut self` method writes its result back
    /// into the receiver when the receiver has storage; a temporary
    /// base has nowhere to write, which is what lets
    /// `q.moved(1, 1).moved(2, 2)` read as one expression. The
    /// write-back target reuses the receiver's already-lowered parts, so
    /// base evaluation happens exactly once.
    fn lower_method_call(
        &mut self,
        mid: MethodId,
        receiver: HExpr,
        attr: &str,
        args: &[Expr],
        span: Span,
    ) -> LResult<HExpr> {
        let is_mut = self
            .ctx
            .tables
            .method_decls
            .get(mid.0 as usize)
            .map(|d| matches!(d.receiver, nx_ast::ReceiverKind::Mut))
            .unwrap_or(false);
        let ret = self.method_ret(mid, span)?;
        let hs = self.lower_args(args)?;
        let writeback = if is_mut {
            self.writeback_target(&receiver)
        } else {
            None
        };
        Ok(self.expr(
            span,
            Some(attr),
            ret,
            HExprKind::CallMethod {
                method: mid,
                receiver: Some(Box::new(receiver)),
                args: hs,
                writeback,
            },
        ))
    }

    /// The write position a `mut self` receiver denotes, if it has
    /// storage: a name, an element, or a field. Anything else (a call
    /// result, a literal) is a temporary.
    fn writeback_target(&self, recv: &HExpr) -> Option<HTarget> {
        match &recv.kind {
            HExprKind::Place(Place::Slot(s)) => Some(HTarget::Slot(*s)),
            HExprKind::Place(Place::Global(g)) => Some(HTarget::Global(*g)),
            HExprKind::Index { base, index, rule } => Some(HTarget::Index {
                base: base.clone(),
                index: index.clone(),
                rule: *rule,
            }),
            HExprKind::Field { base, field } => Some(HTarget::Field {
                base: base.clone(),
                field: *field,
            }),
            _ => None,
        }
    }
}

/// The ambient builtin a name spells (`Len` only for the integer
/// queries: every name the arity table accepts has its arm here, and
/// the verifier's arity table agrees -- a missing arm would mistype,
/// not misbehave).
fn builtin_op(name: &str) -> BuiltinOp {
    match name {
        "push" => BuiltinOp::Push,
        "input" => BuiltinOp::Input,
        "int" => BuiltinOp::ToInt,
        "float" => BuiltinOp::ToFloat,
        _ => BuiltinOp::Len,
    }
}

/// Result type of a builtin: the checker's answers, one per op.
fn builtin_ret(op: BuiltinOp) -> HTy {
    match op {
        BuiltinOp::Len => HTy::Int,
        BuiltinOp::Push => HTy::None,
        BuiltinOp::Input => HTy::Str,
        BuiltinOp::ToInt => HTy::Int,
        BuiltinOp::ToFloat => HTy::Float,
    }
}
