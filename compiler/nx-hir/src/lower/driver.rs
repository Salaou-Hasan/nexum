//! Lowering driver: assembling tables and lowering every body.

use super::scope::FnLower;
use super::tables::{build_tables, diag, Ctx, NameRef, Tables};
use super::tables::{fn_info, FuncDecl};
use super::ty::{canonical_name, conv_ty, conv_ty_str, resolve_type_name};
use super::{lerr, lerr_at, LResult, LowerError};
use crate::model::*;
use nx_ast::{Program, Span, Stmt};
use nx_types::FnInfo;
use std::collections::HashMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Phase 2: assemble tables, then lower every body.
// ---------------------------------------------------------------------------

/// Entry: lower loaded programs starting at `entry`.
pub(crate) fn lower_loaded(
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
