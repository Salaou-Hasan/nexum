//! Functions, methods, and module initializers.

use crate::core::{Binding, Gen, MethodSig};
use crate::mangle::{
    collect_module_globals, mangle_done, mangle_fn, mangle_global, mangle_init, mangle_method,
};
use crate::value::ll_scalar;
use crate::CodegenError;
use nx_ast::{Program, Stmt};
use nx_types::Ty;

impl Gen {
    pub(crate) fn declare_module_fns(&mut self, module: &str, prog: &Program) {
        for s in &prog.stmts {
            if let Stmt::Fn { name, params, .. } = s {
                self.arity
                    .insert((module.to_string(), name.clone()), params.len());
                self.globals.insert(format!("{module}\0{name}"));
            }
        }
    }

    pub(crate) fn is_module_fn(&self, module: &str, name: &str) -> bool {
        self.arity
            .contains_key(&(module.to_string(), name.to_string()))
    }

    pub(crate) fn emit_module(&mut self, module: &str, prog: &Program) -> Result<(), CodegenError> {
        self.cur_module = module.to_string();
        // First pass: every name that needs a module-level global, so all of
        // them can be declared before any function body is emitted.
        //
        // This has to reach inside nested blocks. A name first assigned
        // inside one is still a module global. If the pass only saw
        // top-level statements, the global would be discovered *during*
        // emission and appended to `top` in the middle of a function body,
        // which clang rejects with "expected instruction opcode".
        let mut gvars: Vec<String> = Vec::new();
        for s in &prog.stmts {
            collect_module_globals(s, &mut gvars);
        }
        for v in &gvars {
            self.globals.insert(format!("{module}\0{v}"));
            self.top.push_str(&format!(
                "@{} = global %NxVal zeroinitializer\n",
                mangle_global(module, v)
            ));
        }
        let done = mangle_done(module);
        self.top.push_str(&format!("@{done} = global i1 false\n"));
        for s in &prog.stmts {
            if let Stmt::Fn {
                name, params, body, ..
            } = s
            {
                self.emit_fn(module, name, params, body)?;
            }
        }
        // Methods emit as ordinary functions under mangled names, in a
        // stable order so the IR is reproducible run to run.
        let mut mkeys: Vec<(String, String, String)> = self
            .methods
            .keys()
            .filter(|(m, _, _)| m == module)
            .cloned()
            .collect();
        mkeys.sort();
        for (m, t, meth) in mkeys {
            if let Some(sig) = self
                .methods
                .get(&(m.clone(), t.clone(), meth.clone()))
                .cloned()
            {
                self.emit_method(&m, &t, &meth, &sig)?;
            }
        }
        // Module init body = top-level statements, guarded for import caching.
        self.in_init = true;
        self.term = None;
        // The init body is a function like any other, and the checker's
        // inference for it is filed under `<top>`. Without this, every
        // type lookup that keys on the current function -- unboxing
        // decisions, method dispatch on a local, the memory plan -- missed,
        // because `cur_fn` was still whatever the last emitted function
        // left behind. Top-level code was therefore never unboxed, and a
        // method call on a top-level local could not resolve.
        let saved_fn = self.cur_fn.clone();
        self.cur_fn = "<top>".to_string();
        let init = mangle_init(module);
        self.w(&format!("define void @{init}() {{"));
        self.block("entry");
        self.begin_allocs();
        let flag = self.reg();
        let run = self.lab("initrun");
        let skip = self.lab("initskip");
        self.w(&format!("  {flag} = load i1, ptr @{done}"));
        self.w(&format!("  br i1 {flag}, label %{skip}, label %{run}"));
        self.block(&format!("{run}"));
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
        self.block(&format!("{skip}"));
        self.w("  ret void");
        self.w("}");
        self.end_allocs();
        self.in_init = false;
        self.term = None;
        self.cur_fn = saved_fn;
        Ok(())
    }

    pub(crate) fn emit_main(&mut self) {
        self.w("define i32 @main() {");
        self.block("entry");
        self.w(&format!("  call void @{}()", mangle_init("__main__")));
        self.w("  ret i32 0");
        self.w("}");
    }

    pub(crate) fn emit_fn(
        &mut self,
        module: &str,
        name: &str,
        params: &[String],
        body: &[Stmt],
    ) -> Result<(), CodegenError> {
        let fname = mangle_fn(module, name);
        self.emit_fn_inner(
            module,
            &fname,
            Some(&(module.to_string(), name.to_string())),
            name,
            params,
            body,
        )
    }

    /// Emit a method body as an ordinary function. `fname` is the mangled
    /// symbol, `key` the memo-table identity, and `scope` the
    /// type-inference key (`Type.method`, matching the checker's
    /// `inferred` map) used for unboxing decisions inside the body.
    /// `self` (when present) binds positionally like any other parameter,
    /// which under value semantics gives the method its own copy.
    pub(crate) fn emit_method(
        &mut self,
        module: &str,
        type_name: &str,
        method: &str,
        sig: &MethodSig,
    ) -> Result<(), CodegenError> {
        let fname = mangle_method(module, type_name, method);
        let key = (
            module.to_string(),
            nx_ast::shape::method_key(type_name, method),
        );
        let mut params = Vec::new();
        if sig.receiver != nx_ast::ReceiverKind::None {
            params.push("self".to_string());
        }
        params.extend(sig.params.clone());
        let sig_body = sig.body.clone();
        let _ = sig.span;
        // A method is never memoized. Purity analysis reasons about a
        // function's own body, but a `mut self` method's contract extends
        // past it: the call writes its result back into the receiver. That
        // write is real, caller-visible effect that a cache hit would skip.
        self.emit_fn_inner(module, &fname, None, &key.1, &params, &sig_body)
    }

    pub(crate) fn emit_fn_inner(
        &mut self,
        module: &str,
        fname: &str,
        memo_key: Option<&(String, String)>,
        scope: &str,
        params: &[String],
        body: &[Stmt],
    ) -> Result<(), CodegenError> {
        self.locals.clear();
        self.rep.clear();
        self.term = None;
        self.w(&format!(
            "define %NxVal @{fname}(%NxVal* %args, i64 %nargs) {{"
        ));
        self.block("entry");
        self.begin_allocs();
        // Memo prologue for purity-proven functions: hit returns cached.
        // `memo_key` is None for anything whose call has effects the cache
        // cannot replay -- methods, whose `mut self` result writes back into
        // the receiver at the call site.
        let fnid = memo_key.and_then(|k| self.memo.get(k).copied());
        self.memo_id = fnid;
        if let Some(id) = fnid {
            let slot = self.alloca("%NxVal");
            let hit = self.reg();
            self.w(&format!("  store %NxVal zeroinitializer, ptr {slot}"));
            self.w(&format!(
                "  {hit} = call i1 @nx_memo_get(i64 {id}, ptr %args, i64 %nargs, ptr {slot})"
            ));
            let go = self.lab("mhit");
            let miss = self.lab("mmiss");
            self.w(&format!("  br i1 {hit}, label %{go}, label %{miss}"));
            self.block(&format!("{go}"));
            let cv = self.reg();
            self.w(&format!("  {cv} = load %NxVal, ptr {slot}"));
            // The cache holds one box shared across calls; the caller gets
            // a copy, or a mutation through one call site would corrupt the
            // next: a memo hit clones, so every call site gets its own copy.
            let cc = self.reg();
            self.w(&format!("  {cc} = call %NxVal @nx_clone(%NxVal {cv})"));
            self.w(&format!("  ret %NxVal {cc}"));
            self.block(&format!("{miss}"));
        }
        let saved = self.cur_module.clone();
        let saved_fn = self.cur_fn.clone();
        self.cur_module = module.to_string();
        self.cur_fn = scope.to_string();
        for (i, p) in params.iter().enumerate() {
            // A proven scalar parameter lands straight in a typed slot;
            // everything else keeps the boxed ABI value.
            let hint = self.unboxed_ty(p);
            self.new_slot(p, hint.clone());
            let ep = self.reg();
            self.w(&format!(
                "  {ep} = getelementptr %NxVal, ptr %args, i64 {i}"
            ));
            let v = self.reg();
            self.w(&format!("  {v} = load %NxVal, ptr {ep}"));
            // The call site is type-checked, so a proven scalar's tag
            // needs no runtime guard: take the payload directly. Anything
            // else is a bind, and binds own their containers outright.
            let val = match hint {
                Some(Ty::Float) => {
                    let p1 = self.reg();
                    self.w(&format!("  {p1} = extractvalue %NxVal {v}, 1"));
                    let d = self.reg();
                    self.w(&format!("  {d} = bitcast i64 {p1} to double"));
                    d
                }
                Some(Ty::Bool) => {
                    let p1 = self.reg();
                    self.w(&format!("  {p1} = extractvalue %NxVal {v}, 1"));
                    let c = self.reg();
                    self.w(&format!("  {c} = trunc i64 {p1} to i1"));
                    c
                }
                Some(_) => {
                    let p1 = self.reg();
                    self.w(&format!("  {p1} = extractvalue %NxVal {v}, 1"));
                    p1
                }
                None => {
                    // A bind owns its containers outright. The static type
                    // decides: scalars (and strings) store as-is, anything
                    // that may hold container storage duplicates it. When
                    // unboxing is off every parameter lands here, so the
                    // static check is what keeps scalar calls cheap.
                    if Self::needs_clone(&self.ty_of(p)) {
                        let vc = self.reg();
                        self.w(&format!("  {vc} = call %NxVal @nx_clone(%NxVal {v})"));
                        vc
                    } else {
                        v.clone()
                    }
                }
            };
            let ll = hint.as_ref().and_then(ll_scalar).unwrap_or("%NxVal");
            self.w(&format!(
                "  store {ll} {val}, ptr {}",
                self.locals[p].clone()
            ));
        }
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
        self.cur_fn = saved_fn;
        self.w("}");
        self.end_allocs();
        self.locals.clear();
        self.rep.clear();
        self.term = None;
        Ok(())
    }

    /// Emit `ret` for a value, storing it in the memo cache first when
    /// the current function is memoized.
    pub(crate) fn emit_ret(&mut self, reg: Option<&str>) {
        // The memo id comes from the prologue's decision, never from a
        // second lookup: a `put` without a matching `get` would populate
        // the cache with an entry this function never reads, and worse,
        // could reuse another function's id.
        if let Some(id) = self.memo_id {
            if let Some(v) = reg {
                self.w(&format!(
                    "  call void @nx_memo_put(i64 {id}, ptr %args, i64 %nargs, %NxVal {v})"
                ));
            }
        }
        match reg {
            Some(v) => self.w(&format!("  ret %NxVal {v}")),
            None => self.w("  ret %NxVal zeroinitializer"),
        }
    }

    pub(crate) fn is_unique(&self, name: &str) -> bool {
        self.locals.contains_key(name)
            && self.plan.alloc_of(&self.cur_module, &self.cur_fn, name) == nx_mem::Alloc::Unique
    }

    /// Free every Unique local currently in scope. An unboxed slot holds
    /// a bare scalar, so there is no buffer to release.
    pub(crate) fn free_scope(&mut self) {
        let mut names: Vec<String> = self.locals.keys().cloned().collect();
        names.sort();
        for n in names {
            if self.plan.alloc_of(&self.cur_module, &self.cur_fn, &n) == nx_mem::Alloc::Unique
                && self.rep_of(&n).is_none()
            {
                if let Some(slot) = self.locals.get(&n).cloned() {
                    let v = self.reg();
                    self.w(&format!("  {v} = load %NxVal, ptr {slot}"));
                    self.w(&format!("  call void @nx_free_val(%NxVal {v})"));
                }
            }
        }
    }

    /// Module-global variable, declared on first use.
    pub(crate) fn ensure_global(&mut self, module: &str, name: &str) -> String {
        let g = mangle_global(module, name);
        if self.globals.insert(format!("{module}\0{name}")) {
            self.top
                .push_str(&format!("@{g} = global %NxVal zeroinitializer\n"));
        }
        format!("@{g}")
    }

    pub(crate) fn ptr_of(&mut self, name: &str) -> Option<String> {
        if let Some(r) = self.locals.get(name).cloned() {
            return Some(r);
        }
        let g = format!("{}\0{name}", self.cur_module);
        if self.globals.contains(&g) {
            return Some(format!("@{}", mangle_global(&self.cur_module, name)));
        }
        None
    }

    pub(crate) fn resolve(&self, name: &str) -> Result<Binding, (String, String)> {
        // Returns Binding or (kind, detail) for precise errors.
        if self.locals.contains_key(name) {
            return Ok(Binding::Local);
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

    pub(crate) fn arity_known_module(&self, module: &str) -> bool {
        self.arity.keys().any(|(m, _)| m == module)
    }

    pub(crate) fn module_has_vars(&self, _module: &str) -> bool {
        // Vars are discovered during emission; the loader guarantees the
        // module exists, so any import we emit for was resolvable.
        // (Unknown members are the checker's job.)
        true
    }

    pub(crate) fn module_known(&self, _module: &str) -> bool {
        true
    }

    /// Declared return type of `module`.`name`, used to keep a call
    /// result unboxed when it feeds straight back into arithmetic.
    pub(crate) fn ret_ty(&self, module: &str, name: &str) -> Ty {
        self.types
            .get(&(module.to_string(), name.to_string()))
            .map(|f| f.ret.clone())
            .unwrap_or(Ty::Unknown)
    }
}
