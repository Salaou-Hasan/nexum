//! Declaration tables: indexing types, functions, and globals across modules.

use super::{lerr, lerr_at, LResult};
use crate::model::*;
use nx_ast::shape;
use nx_ast::{Program, Span, Stmt, Target};
use nx_types::FnInfo;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Phase 1: index declarations across all modules.
// ---------------------------------------------------------------------------

/// A record declaration as written: home module, name, fields in order
/// with their type spellings. Field types resolve after every module is
/// indexed, so later and mutually recursive declarations work.
#[derive(Debug, Clone)]
pub(crate) struct TypeDecl {
    pub(crate) module: String,
    pub(crate) name: String,
    pub(crate) fields: Vec<(String, String)>,
}

/// A function or method body as written, with its declaring module.
/// Methods carry their type name plus receiver kind; the method table
/// below points at these bodies by `FuncId`.
#[derive(Debug, Clone)]
pub(crate) struct FuncDecl {
    pub(crate) module: String,
    pub(crate) name: String,
    pub(crate) params: Vec<String>,
    pub(crate) receiver: Option<(String, nx_ast::ReceiverKind)>,
    pub(crate) body: Vec<Stmt>,
    pub(crate) span: Span,
}

#[derive(Debug, Clone)]
pub(crate) struct MethodDecl {
    pub(crate) type_id: TypeId,
    pub(crate) name: String,
    pub(crate) func: FuncId,
    pub(crate) receiver: nx_ast::ReceiverKind,
}

pub(crate) struct Tables {
    pub(crate) modules_sorted: Vec<String>,
    pub(crate) module_id: HashMap<String, ModuleId>,
    /// (module, name) -> id, in declaration order per module.
    pub(crate) type_id: HashMap<(String, String), TypeId>,
    pub(crate) type_decls: Vec<TypeDecl>,
    /// (module, name) -> id, source order depth-first, nested included:
    /// redefinition is a checker error, so at most one exists.
    pub(crate) func_id: HashMap<(String, String), FuncId>,
    pub(crate) func_decls: Vec<FuncDecl>,
    /// (type, method name) -> id, for every method including associated
    /// functions. Duplicates are checker errors.
    pub(crate) method_id: HashMap<(TypeId, String), MethodId>,
    pub(crate) method_decls: Vec<MethodDecl>,
    /// Per-module alias env: alias -> (home module, canonical name).
    /// Module-wide (including function bodies), mirroring the checker,
    /// whose `type_alias` map is not scoped per function.
    pub(crate) type_alias: HashMap<(String, String), (String, String)>,
    /// Per-module globals in first-bind order (Assign/AssignOp names over
    /// top-level control flow; loop variables are slots, never globals).
    pub(crate) globals: HashMap<String, Vec<String>>,
}

pub(crate) fn build_tables(programs: &HashMap<String, Program>) -> LResult<Tables> {
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
pub(crate) fn index_decls(t: &mut Tables, module: &str, stmts: &[Stmt]) -> LResult<()> {
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
pub(crate) fn index_globals(t: &mut Tables, module: &str, stmts: &[Stmt]) {
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
pub(crate) fn index_aliases(t: &mut Tables, module: &str, stmts: &[Stmt]) -> LResult<()> {
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

/// A name binding inside one function body under lowering.
#[derive(Clone)]
pub(crate) enum NameRef {
    Slot(Slot),
    Global(GlobalId),
    Module(ModuleId),
    Func(FuncId),
    Type(TypeId),
}

pub(crate) fn diag(name: &str) -> DiagInfo {
    DiagInfo::named(name)
}

pub(crate) fn no_diag() -> DiagInfo {
    DiagInfo { name: None }
}

/// The immutable program tables one body needs, borrowed disjointly
/// from the interning tables so a single `Lower` can lower bodies in
/// sequence while appending to its string table.
pub(crate) struct Ctx<'a> {
    pub(crate) tables: &'a Tables,
    pub(crate) inferred: &'a HashMap<(String, String), FnInfo>,
    pub(crate) global_tys: &'a HashMap<GlobalId, HTy>,
    pub(crate) global_ids: &'a HashMap<(String, String), GlobalId>,
    pub(crate) visible_types: &'a HashMap<(String, String), TypeId>,
    pub(crate) visible_funcs: &'a HashMap<(String, String), FuncId>,
}

impl Ctx<'_> {
    /// The checker's inference for one body key. A missing key means
    /// the checker never produced this body, which a checked program
    /// cannot do -- so it is an internal error, not a diagnostic.
    pub(crate) fn fn_info(&self, module: &str, func: &str) -> LResult<FnInfo> {
        fn_info(self.inferred, module, func)
    }
}

/// A checker entry for one body key, or an internal error naming the
/// gap. Methods are keyed `Type.method`; module tops are `<top>`.
pub(crate) fn fn_info(
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
