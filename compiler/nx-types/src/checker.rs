//! Checker state, scoping, and module loading.

use crate::pred::{compatible, expr_label, field_ty, is_keyable};
use crate::{CheckError, FnInfo, MethodInfo, ModInfo, Ty};
use nx_ast::{Expr, Span, Stmt, Target};
use std::collections::{HashMap, HashSet};

pub(crate) struct Checker {
    pub(crate) vars: HashMap<String, Ty>,
    pub(crate) funcs: HashMap<String, (Vec<Ty>, Ty)>,
    pub(crate) modules: HashMap<String, ModInfo>,
    pub(crate) loading: Vec<String>,
    /// Modules that failed to load (circular, missing, unparseable, or
    /// checked with errors). A repeat import returns false silently:
    /// the root error is already reported, and re-running the submodule
    /// check would duplicate its diagnostics.
    pub(crate) failed_modules: HashSet<String>,
    /// Names bound from a failed module load. They hold Unknown, and
    /// every value position accepts Unknown silently -- except an
    /// attribute *call* (`x.m(...)`), which errors by the Stage 4 rule
    /// below. The carve-out there consults this set so a poisoned
    /// receiver stays silent while a merely-dynamic one still errors.
    /// Membership implies nothing once rebound: the carve-out also
    /// requires the current type to still be Unknown, so `del` and
    /// reassignment need no invalidation tracking.
    pub(crate) poisoned_names: HashSet<String>,
    pub(crate) base: std::path::PathBuf,
    pub(crate) errors: Vec<CheckError>,
    pub(crate) in_function: bool,
    pub(crate) returns: Vec<Ty>,
    pub(crate) loop_depth: usize,
    /// Parameters of the function being checked. Their types start Unknown
    /// and are narrowed from how the body uses them, which is what lets
    /// codegen keep them out of the box.
    pub(crate) param_names: Vec<String>,
    /// Parameters seen in arithmetic, candidates for the integer default.
    pub(crate) numeric_params: Vec<String>,
    /// Set when a Float appears anywhere in the current function, including
    /// in an expression that never lands in a local.
    pub(crate) saw_float: bool,
    /// `local = param` plain copies found in the current body, resolved
    /// once the pass knows a type for either end.
    pub(crate) copies: Vec<(String, String)>,
    /// Per-function inferred shapes, harvested for the optimizer.
    pub(crate) inferred: HashMap<(String, String), FnInfo>,
    /// Module the checker is currently inside, for inference keys.
    pub(crate) module_name: String,

    /// Declared `type` layouts: type name to (field name, field type name
    /// as written), in declaration order. Field types resolve lazily at
    /// each use, so a field may name a record declared later in the module
    /// -- mutually recursive types included. The backend only needs field
    /// names for offsets; types guide checking.
    pub(crate) records: HashMap<String, Vec<(String, String)>>,
    /// Where each type was declared, for field-type validation errors.
    pub(crate) record_spans: HashMap<String, Span>,
    /// Imported type aliases: alias to canonical (declared) name, so a
    /// value built as `U(...)` carries `Ty::Record("T")` and compares
    /// equal to one built as `T(...)`.
    pub(crate) type_alias: HashMap<String, String>,
    /// Methods declared by `impl` blocks: (canonical type, method) to
    /// info. Shared across the module; imported types bring their methods
    /// along (see FromImport).
    pub(crate) methods: HashMap<(String, String), MethodInfo>,
    /// The `self` parameter of the method body being checked, with its
    /// receiver kind. Present only inside a method: field writes through
    /// a read-only `self` are rejected against it.
    pub(crate) self_param: Option<(String, nx_ast::ReceiverKind)>,
}

// Default impls for the checker state.
impl Default for Checker {
    fn default() -> Self {
        Self {
            vars: HashMap::new(),
            funcs: HashMap::new(),
            modules: HashMap::new(),
            loading: Vec::new(),
            failed_modules: HashSet::new(),
            poisoned_names: HashSet::new(),
            base: ".".into(),
            errors: Vec::new(),
            in_function: false,
            returns: Vec::new(),
            loop_depth: 0,
            param_names: Vec::new(),
            numeric_params: Vec::new(),
            saw_float: false,
            copies: Vec::new(),
            inferred: HashMap::new(),
            module_name: String::new(),

            records: HashMap::new(),
            record_spans: HashMap::new(),
            type_alias: HashMap::new(),
            methods: HashMap::new(),
            self_param: None,
        }
    }
}

impl Checker {
    pub(crate) fn err(&mut self, span: Span, msg: String) {
        self.errors.push(CheckError {
            message: msg,
            line: span.line,
            col: span.col,
        });
    }

    /// Whether an expression is rooted at the current method's `self`
    /// with a receiver that forbids mutation. Only a `mut self` method may
    /// write through `self` -- through a field, an element, or a nested
    /// path like `self.pos.x`. Anything else mutates at most a local copy,
    /// so the checker refuses it rather than letting it look meaningful.
    pub(crate) fn self_root_denied(&self, e: &Expr) -> bool {
        let (sname, kind) = match &self.self_param {
            Some(v) => v,
            None => return false,
        };
        if *kind == nx_ast::ReceiverKind::Mut {
            return false;
        }
        let mut cur = e;
        loop {
            match cur {
                Expr::Var(n, _) => return n == sname,
                Expr::Index { base, .. } | Expr::Attr { base, .. } => cur = base,
                _ => return false,
            }
        }
    }

    /// Whether a receiver still holds the Unknown it was poisoned with:
    /// bound from a failed import and never rebound since. `del` and
    /// reassignment change the type (or unbind it), so no invalidation
    /// tracking is needed -- a rebound name simply stops qualifying.
    pub(crate) fn poisoned_unknown(&self, e: &Expr) -> bool {
        let mut cur = e;
        loop {
            match cur {
                Expr::Var(n, _) => {
                    return self.poisoned_names.contains(n)
                        && self.vars.get(n) == Some(&Ty::Unknown);
                }
                Expr::Index { base, .. } | Expr::Attr { base, .. } => cur = base,
                _ => return false,
            }
        }
    }

    /// Bind one assignment target. A name follows the usual monomorphic
    /// rule; an index or field writes into a container and is checked
    /// against the container's element type instead of defining anything.
    pub(crate) fn bind_target(&mut self, target: &Target, t: Ty, value: &Expr, span: Span) {
        match target {
            Target::Name(name) => {
                self.mark_copy(name, value);
                // `None` means "no value here", so it must not pin a
                // variable to a type or conflict with one. Widening to
                // unresolved is what lets `x = 1` / `x = None` / `x = 2`
                // work without giving up monomorphism for real values.
                let t = if t == Ty::None { Ty::Unknown } else { t };
                self.define(name, t, span);
            }
            Target::Index { base, index } => {
                // A write through read-only `self` is refused even though
                // it would only touch a local copy.
                if self.self_root_denied(base) {
                    self.err(
                        base.span(),
                        "cannot mutate through read-only 'self' (use `mut self`)".to_string(),
                    );
                    return;
                }
                let bt = self.check_expr(base);
                // Dict keys are values and positional indices are Ints; an
                // unresolved base may be either, so a scalar key is
                // accepted and the runtime dispatches. A dict checks only
                // the key; a list checks the index and the stored value.
                if matches!(bt, Ty::Dict(_) | Ty::Unknown) {
                    let kt = self.check_expr(index);
                    if !is_keyable(&kt) {
                        self.err(
                            index.span(),
                            format!("index must be Int or a dict key, found {kt}"),
                        );
                    }
                    if matches!(bt, Ty::Dict(_)) {
                        return;
                    }
                    if !matches!(kt, Ty::Int | Ty::Unknown) {
                        // A non-Int key on an unresolved base can only mean
                        // a dict, so there is no positional check and no
                        // element type to check the value against.
                        return;
                    }
                }
                let it = self.check_expr(index);
                if !matches!(it, Ty::Int | Ty::Unknown) {
                    self.err(index.span(), format!("index must be Int, found {it}"));
                }
                match bt {
                    Ty::List(elem) => {
                        // The written value has to fit the slot. Refusing
                        // here is what keeps a `List(Int)` from being handed
                        // a String by a plain assignment.
                        if !compatible(&elem, &t) {
                            self.err(span, format!("cannot store {t} into {}", expr_label(base)));
                        }
                    }
                    Ty::Str => self.err(base.span(), "strings are immutable".to_string()),
                    Ty::Unknown => {}
                    other => self.err(base.span(), format!("cannot index-assign into {other}")),
                }
            }
            Target::Attr { base, field } => {
                // A field write needs the base's type: it is what says which
                // layout is being written to, and whether the value fits.
                // A write through read-only `self` is refused even though
                // it would only touch a local copy.
                if self.self_root_denied(base) {
                    self.err(
                        base.span(),
                        "cannot mutate through read-only 'self' (use `mut self`)".to_string(),
                    );
                    return;
                }
                let bt = self.check_expr(base);
                match bt {
                    Ty::Record(rt) => {
                        if let Some(fields) = self.records.get(&rt).cloned() {
                            let recs = self.records.clone();
                            match fields.iter().find(|(n, _)| n == field) {
                                Some((_, ft)) => {
                                    let fty = field_ty(ft, &recs);
                                    if fty != Ty::Unknown && !compatible(&fty, &t) {
                                        self.err(
                                            span,
                                            format!(
                                                "field '{field}' of '{rt}' is {fty}, cannot assign {t}"
                                            ),
                                        );
                                    }
                                }
                                None => {
                                    self.err(
                                        base.span(),
                                        format!("type '{rt}' has no field '{field}'"),
                                    );
                                }
                            }
                        }
                    }
                    // A dynamic base is resolved by name at runtime.
                    Ty::Unknown => {}
                    other => self.err(
                        base.span(),
                        format!("attribute assignment needs a type, found {other}"),
                    ),
                }
            }
        }
    }

    /// The current type of whatever a target designates, so an `op=` can be
    /// checked against what is already there. `None` means the target is
    /// not readable at all and an error has already been reported.
    pub(crate) fn target_ty(&mut self, target: &Target, span: Span) -> Option<Ty> {
        match target {
            Target::Name(name) => match self.vars.get(name).cloned() {
                None => {
                    self.err(span, format!("undefined variable '{name}'"));
                    None
                }
                Some(t) => Some(t),
            },
            Target::Index { base, index } => {
                if self.self_root_denied(base) {
                    self.err(
                        base.span(),
                        "cannot mutate through read-only 'self' (use `mut self`)".to_string(),
                    );
                    return None;
                }
                let bt = self.check_expr(base);
                // An unresolved base may hold a list or a dict; a scalar
                // key is accepted for either and the runtime dispatches. A
                // non-Int key can only mean a dict, so the value has no
                // element type to check against.
                if matches!(bt, Ty::Unknown) {
                    let kt = self.check_expr(index);
                    if !is_keyable(&kt) {
                        self.err(
                            index.span(),
                            format!("index must be Int or a dict key, found {kt}"),
                        );
                    }
                    return Some(Ty::Unknown);
                }
                if matches!(bt, Ty::Dict(_)) {
                    let kt = self.check_expr(index);
                    if !is_keyable(&kt) {
                        self.err(
                            index.span(),
                            format!("dict key must be Int, Float, Bool or Str, found {kt}"),
                        );
                    }
                    return Some(bt);
                }
                let it = self.check_expr(index);
                if !matches!(it, Ty::Int | Ty::Unknown) {
                    self.err(index.span(), format!("index must be Int, found {it}"));
                }
                match bt {
                    Ty::List(elem) => Some(*elem),
                    Ty::Unknown => Some(Ty::Unknown),
                    other => {
                        self.err(base.span(), format!("cannot index-assign into {other}"));
                        None
                    }
                }
            }
            Target::Attr { base, field } => {
                if self.self_root_denied(base) {
                    self.err(
                        base.span(),
                        "cannot mutate through read-only 'self' (use `mut self`)".to_string(),
                    );
                    return None;
                }
                let bt = self.check_expr(base);
                match bt {
                    Ty::Record(t) => {
                        if let Some(fields) = self.records.get(&t).cloned() {
                            let recs = self.records.clone();
                            match fields.iter().find(|(n, _)| n == field) {
                                Some((_, ft)) => return Some(field_ty(ft, &recs)),
                                None => {
                                    self.err(
                                        base.span(),
                                        format!("type '{t}' has no field '{field}'"),
                                    );
                                    return None;
                                }
                            }
                        }
                        Some(Ty::Unknown)
                    }
                    Ty::Unknown => Some(Ty::Unknown),
                    other => {
                        self.err(
                            base.span(),
                            format!("attribute assignment needs a type, found {other}"),
                        );
                        None
                    }
                }
            }
        }
    }

    /// Report field types that name nothing. Runs once the whole module has
    /// been seen, because a field may name a record declared later --
    /// including mutually recursive pairs. Scalar names plus `Any`,
    /// `List` and `Dict` (unresolved containers) are always fine.
    pub(crate) fn validate_records(&mut self) {
        let mut bad: Vec<(String, String, String, Span)> = Vec::new();
        for (tname, fields) in &self.records {
            for (fname, fty) in fields {
                match fty.as_str() {
                    "Int" | "Float" | "Bool" | "Str" | "None" | "Any" | "List" | "Dict" => {}
                    _ if self.records.contains_key(fty) => {}
                    _ => {
                        let span = self
                            .record_spans
                            .get(tname)
                            .cloned()
                            .unwrap_or(Span { line: 1, col: 1 });
                        bad.push((tname.clone(), fname.clone(), fty.clone(), span));
                    }
                }
            }
        }
        // Sorted so the diagnostics are reproducible run to run.
        bad.sort_by(|a, b| (&a.0, &a.1, a.3.line, a.3.col).cmp(&(&b.0, &b.1, b.3.line, b.3.col)));
        for (tname, fname, fty, span) in bad {
            self.err(
                span,
                format!("unknown field type '{fty}' on '{tname}.{fname}'"),
            );
        }
    }

    pub(crate) fn define(&mut self, name: &str, ty: Ty, span: Span) {
        // A variable may not share a type's name: `Point = 5` would make
        // `Point(...)` unresolvable, and the same holds for loop variables
        // and imports, which all bind through here.
        if self.records.contains_key(name) {
            self.err(
                span,
                format!("'{name}' is already a type; a variable cannot share the name"),
            );
            return;
        }
        match self.vars.get(name) {
            None => {
                self.vars.insert(name.to_string(), ty);
            }
            Some(old) if compatible(old, &ty) => {
                // An unresolved value assigned to a known-typed variable
                // makes the variable itself unresolved. The backend gives
                // a known scalar a typed slot, so keeping the narrow type
                // here would let a value of the wrong dynamic type land in
                // it. Widening to Unknown costs the optimization, not
                // correctness.
                if ty == Ty::Unknown && *old != Ty::Unknown {
                    self.vars.insert(name.to_string(), Ty::Unknown);
                }
            }
            Some(old) => {
                let old = old.clone();
                self.err(
                    span,
                    format!("variable '{name}' is {old}, cannot rebind to {ty}"),
                );
            }
        }
    }

    /// Narrow a parameter from the way the body uses it. NX has no
    /// overloading and no generics, so a use site fixes the type: `n - 1`
    /// proves Int, `n < 1.5` proves Float, `not n` proves Bool. Non-params
    /// and already-known types are left alone (locals are monomorphic).
    pub(crate) fn expect_param(&mut self, e: &Expr, ty: Ty) {
        let Expr::Var(name, _) = e else { return };
        if !self.param_names.iter().any(|p| p == name) {
            return;
        }
        match self.vars.get(name) {
            Some(Ty::Unknown) => {
                self.vars.insert(name.clone(), ty);
            }
            Some(old) if *old == ty => {}
            // Conflicting uses: leave the first answer and let the
            // expression-level check report the mismatch.
            _ => {}
        }
    }

    /// Record a parameter used in arithmetic. This does not pin the type:
    /// NX promotes mixed Int/Float, so `n * 2` is well-typed for an Int n
    /// and for a Float n, and neither operand's type is forced. It only
    /// marks the parameter as a candidate for the integer default applied
    /// once the whole body has been seen.
    pub(crate) fn mark_numeric(&mut self, e: &Expr) {
        if let Expr::Var(name, _) = e {
            if self.param_names.iter().any(|p| p == name) {
                self.numeric_params.push(name.clone());
            }
        }
    }

    /// Record `local = param`, a plain copy that makes both the same
    /// type. Neither side learns anything on its own, so the pair is
    /// resolved after the body by copying whichever end got typed.
    pub(crate) fn mark_copy(&mut self, local: &str, value: &Expr) {
        if let Expr::Var(p, _) = value {
            if self.param_names.iter().any(|x| x == p) {
                self.copies.push((local.to_string(), p.clone()));
            }
        }
    }

    pub(crate) fn check_block(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.check_stmt(s);
        }
    }

    pub(crate) fn check_module(&mut self, name: &str, span: Span) -> bool {
        if self.modules.contains_key(name) {
            return true;
        }
        // Already failed once: the root error is reported, so a repeat
        // import stays silent instead of re-running the submodule check
        // and duplicating its diagnostics.
        if self.failed_modules.contains(name) {
            return false;
        }
        if self.loading.contains(&name.to_string()) {
            self.err(span, format!("circular import of '{name}'"));
            self.failed_modules.insert(name.to_string());
            return false;
        }
        let path = match nx_ast::shape::resolve_module_file(&[self.base.clone()], name) {
            Some(p) => p,
            None => {
                self.err(span, format!("cannot find module '{name}.nx'"));
                self.failed_modules.insert(name.to_string());
                return false;
            }
        };
        let (prog, dir) = match std::fs::read_to_string(&path)
            .ok()
            .and_then(|src| {
                let toks = nx_lexer::lex(&src).ok()?;
                nx_parser::parse(toks).ok()
            })
            .map(|prog| {
                let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or(".".into());
                (prog, dir)
            }) {
            Some(v) => v,
            None => {
                self.err(span, format!("cannot parse module '{name}'"));
                self.failed_modules.insert(name.to_string());
                return false;
            }
        };
        // Check the submodule with isolated scopes; harvest exports.
        let saved_vars = std::mem::take(&mut self.vars);
        let saved_funcs = std::mem::take(&mut self.funcs);
        let saved_records = std::mem::take(&mut self.records);
        let saved_spans = std::mem::take(&mut self.record_spans);
        let saved_alias = std::mem::take(&mut self.type_alias);
        let saved_methods = std::mem::take(&mut self.methods);
        let saved_base = std::mem::replace(&mut self.base, dir);
        let saved_module = std::mem::replace(&mut self.module_name, name.to_string());
        self.loading.push(name.to_string());
        let nerr = self.errors.len();
        self.check_block(&prog.stmts);
        self.loading.pop();
        // Unknown field-type names are reported now that the whole module
        // has been seen: a field may name a record declared later.
        self.validate_records();
        // A module that does not check clean is a failed load, even
        // though its exports were harvested: the importer binds
        // Unknowns and stays silent, so only the module's own errors
        // are reported instead of a second cascade at every use site.
        // The summary names the import at fault -- without it, a broken
        // dependency surfaces only as spans inside another file, which
        // no filename in the diagnostic can attribute. Repeat imports
        // hit `failed_modules` above and stay silent.
        // (`nx build` already requires a clean check, so nothing that
        // compiled before stops compiling.)
        if self.errors.len() != nerr {
            self.err(span, format!("module '{name}' has errors"));
            self.failed_modules.insert(name.to_string());
        }
        let info = ModInfo {
            vars: std::mem::replace(&mut self.vars, saved_vars),
            funcs: std::mem::replace(&mut self.funcs, saved_funcs),
            types: std::mem::replace(&mut self.records, saved_records),
            methods: std::mem::replace(&mut self.methods, saved_methods),
        };
        self.record_spans = saved_spans;
        self.type_alias = saved_alias;
        self.inferred.insert(
            (name.to_string(), "<top>".to_string()),
            FnInfo {
                locals: info.vars.clone(),
                params: Vec::new(),
                ret: Ty::None,
            },
        );
        self.base = saved_base;
        self.module_name = saved_module;
        // No entry for a failed module: a partial export table invites
        // "has no member" lookups from paths that consult `modules`
        // directly, which is exactly the cascade this suppression ends.
        if self.failed_modules.contains(name) {
            return false;
        }
        self.modules.insert(name.to_string(), info);
        true
    }
}
