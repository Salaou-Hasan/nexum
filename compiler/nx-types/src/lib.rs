//! Static type checker for Nexum (`nx check`).
//!
//! v0 rules (advisory only — `nx file.nx` still runs dynamically):
//! - Inferred types, no annotations: Int Float Bool Str List(T) None.
//! - Variables are monomorphic: the first binding fixes the type.
//! - Function params start Unknown; bodies must return consistently.
//! - `import`/`from` are followed into files; members are checked.

use std::collections::HashMap;
use nx_ast::{BinOp, Expr, Program, Span, Stmt, Target, UnaryOp};

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

fn compatible(a: &Ty, b: &Ty) -> bool {
    a == b || matches!(a, Ty::Unknown) || matches!(b, Ty::Unknown)
}

/// Types that may key a dict. When the base is unresolved, any of these
/// is accepted: the runtime dispatches on the actual tag, and a scalar
/// key is valid for every dict while an Int is valid for every list.
fn is_keyable(t: &Ty) -> bool {
    matches!(t, Ty::Int | Ty::Float | Ty::Bool | Ty::Str | Ty::Unknown)
}

fn is_numeric(t: &Ty) -> bool {
    matches!(t, Ty::Int | Ty::Float | Ty::Unknown)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckError {
    pub message: String,
    pub line: nx_ast::LineNo,
    pub col: nx_ast::ColNo,
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "type error at {}:{}: {}", self.line, self.col, self.message)
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

struct Checker {
    vars: HashMap<String, Ty>,
    funcs: HashMap<String, (Vec<Ty>, Ty)>,
    modules: HashMap<String, ModInfo>,
    loading: Vec<String>,
    base: std::path::PathBuf,
    errors: Vec<CheckError>,
    in_function: bool,
    returns: Vec<Ty>,
    loop_depth: usize,
    /// Parameters of the function being checked. Their types start Unknown
    /// and are narrowed from how the body uses them, which is what lets
    /// codegen keep them out of the box.
    param_names: Vec<String>,
    /// Parameters seen in arithmetic, candidates for the integer default.
    numeric_params: Vec<String>,
    /// Set when a Float appears anywhere in the current function, including
    /// in an expression that never lands in a local.
    saw_float: bool,
    /// `local = param` plain copies found in the current body, resolved
    /// once the pass knows a type for either end.
    copies: Vec<(String, String)>,
    /// Per-function inferred shapes, harvested for the optimizer.
    inferred: HashMap<(String, String), FnInfo>,
    /// Module the checker is currently inside, for inference keys.
    module_name: String,

    /// Declared `type` layouts: type name to (field name, field type name
    /// as written), in declaration order. Field types resolve lazily at
    /// each use, so a field may name a record declared later in the module
    /// -- mutually recursive types included. The backend only needs field
    /// names for offsets; types guide checking.
    records: HashMap<String, Vec<(String, String)>>,
    /// Where each type was declared, for field-type validation errors.
    record_spans: HashMap<String, Span>,
    /// Imported type aliases: alias to canonical (declared) name, so a
    /// value built as `U(...)` carries `Ty::Record("T")` and compares
    /// equal to one built as `T(...)`.
    type_alias: HashMap<String, String>,
    /// Methods declared by `impl` blocks: (canonical type, method) to
    /// info. Shared across the module; imported types bring their methods
    /// along (see FromImport).
    methods: HashMap<(String, String), MethodInfo>,
    /// The `self` parameter of the method body being checked, with its
    /// receiver kind. Present only inside a method: field writes through
    /// a read-only `self` are rejected against it.
    self_param: Option<(String, nx_ast::ReceiverKind)>,
}

/// Resolve a field type name as written to a `Ty`, against one module's
/// declarations. Scalars map directly; any other name that matches a
/// declared type becomes that record; anything else stays Unknown here
/// and is reported by `validate_records` at the end of the module.
fn field_ty(
    tname: &str,
    records: &HashMap<String, Vec<(String, String)>>,
) -> Ty {
    match tname {
        "Int" => Ty::Int,
        "Float" => Ty::Float,
        "Bool" => Ty::Bool,
        "Str" => Ty::Str,
        "None" => Ty::None,
        _ if records.contains_key(tname) => Ty::Record(tname.to_string()),
        _ => Ty::Unknown,
    }
}

/// Arity range of an ambient builtin, if `name` is one, as
/// (minimum, maximum). The method-call sugar (`xs.push(1)` for
/// `push(xs, 1)`) consults this: an attribute name with a known range
/// rewrites to the builtin call, so sugar needs no separate table to
/// drift out of sync. The name set itself lives in `nx_ast::shape`; this
/// delegates so there is exactly one table.
/// Free function (not a method) so the backend crate can share the gate.
pub fn builtin_arity(name: &str) -> Option<(usize, usize)> {
    nx_ast::shape::builtin_arity(name)
}

impl Checker {
    fn err(&mut self, span: Span, msg: String) {
        self.errors.push(CheckError { message: msg, line: span.line, col: span.col });
    }

    /// Whether an expression is rooted at the current method's `self`
    /// with a receiver that forbids mutation. Only a `mut self` method may
    /// write through `self` -- through a field, an element, or a nested
    /// path like `self.pos.x`. Anything else mutates at most a local copy,
    /// so the checker refuses it rather than letting it look meaningful.
    fn self_root_denied(&self, e: &Expr) -> bool {
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

    /// Bind one assignment target. A name follows the usual monomorphic
    /// rule; an index or field writes into a container and is checked
    /// against the container's element type instead of defining anything.
    fn bind_target(&mut self, target: &Target, t: Ty, value: &Expr, span: Span) {
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
    fn target_ty(&mut self, target: &Target, span: Span) -> Option<Ty> {
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
    fn validate_records(&mut self) {
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

    /// Check call arguments against parameter types: exact arity, then
    /// per-argument compatibility with parameter narrowing. Shared by
    /// plain calls, module calls, and method calls so all three agree.
    fn check_call_args(&mut self, params: &[Ty], args: &[Expr], span: Span) {
        if params.len() != args.len() {
            self.err(span, format!("expects {} args, got {}", params.len(), args.len()));
        }
        for (p, a) in params.iter().zip(args.iter()) {
            let at = self.check_expr(a);
            if !compatible(p, &at) {
                self.err(a.span(), format!("argument must be {p}, found {at}"));
            }
            if p != &Ty::Unknown {
                self.expect_param(a, p.clone());
            }
        }
    }

    /// Check a builtin call by name. Every ambient builtin lives here, so
    /// direct calls (`push(xs, 1)`) and sugar calls (`xs.push(1)`) share
    /// one implementation and one set of diagnostics.
    fn check_builtin(&mut self, name: &str, args: &[Expr], span: Span) -> Ty {
        match name {
            "len" => {
                if args.len() != 1 {
                    self.err(span, "len() expects 1 argument".to_string());
                    return Ty::Unknown;
                }
                let t = self.check_expr(&args[0]);
                if !matches!(t, Ty::List(_) | Ty::Str | Ty::Dict(_) | Ty::Unknown) {
                    self.err(span, format!("len() only supports lists, strings and dicts, found {t}"));
                }
                Ty::Int
            }
            "push" => {
                if args.len() != 2 {
                    self.err(span, "push() expects 2 arguments".to_string());
                    return Ty::Unknown;
                }
                // The target must be a variable: pushing into a temporary
                // would drop the result, and the backend writes through
                // the variable's slot.
                if !matches!(&args[0], Expr::Var(..)) {
                    self.err(span, "push() first argument must be a list variable".to_string());
                }
                let lt = self.check_expr(&args[0]);
                let et = self.check_expr(&args[1]);
                match lt {
                    Ty::List(t) if compatible(&t, &et) => {
                        // A push into a `List(?)` pins the element
                        // type, the same way a literal element
                        // would. Without this, every list built by
                        // pushing (the only way to grow one) stays
                        // unresolved and poisons everything read
                        // from it back to dynamic.
                        if *t == Ty::Unknown && et != Ty::Unknown {
                            if let Expr::Var(n, _) = &args[0] {
                                self.vars.insert(n.clone(), Ty::List(Box::new(et)));
                            }
                        }
                    }
                    Ty::List(_) => {
                        self.err(span, "push() element type mismatch".to_string());
                    }
                    Ty::Unknown => {}
                    other => {
                        self.err(span, format!("push() needs a list, found {other}"));
                    }
                }
                Ty::None
            }
            "input" => {
                // `input()` reads a line from stdin; `input(prompt)`
                // prints the prompt verbatim first. The prompt must be a
                // string, and the answer always is one.
                if args.len() > 1 {
                    self.err(
                        span,
                        format!("input() expects at most 1 argument, got {}", args.len()),
                    );
                    return Ty::Unknown;
                }
                if let Some(p) = args.first() {
                    let pt = self.check_expr(p);
                    if !matches!(pt, Ty::Str | Ty::Unknown) {
                        self.err(span, format!("input() prompt must be Str, found {pt}"));
                    }
                }
                Ty::Str
            }
            _ => {
                self.err(span, format!("unknown builtin '{name}'"));
                Ty::Unknown
            }
        }
    }

    /// The canonical (declared) name for a possibly-aliased type. Unknown
    /// names pass through unchanged; the caller reports them.
    fn canonical_name(&self, name: &str) -> String {
        self.type_alias.get(name).cloned().unwrap_or_else(|| name.to_string())
    }

    /// Check constructor arguments against a record layout: exact arity,
    /// then per-field types. Positional by design -- a partial constructor
    /// would need a notion of an unset field that the value model does
    /// not have.
    fn check_record_args(
        &mut self,
        name: &str,
        fields: &[(String, Ty)],
        args: &[Expr],
        span: Span,
    ) {
        if args.len() != fields.len() {
            self.err(
                span,
                format!(
                    "type '{name}' takes {} field{}, got {}",
                    fields.len(),
                    if fields.len() == 1 { "" } else { "s" },
                    args.len()
                ),
            );
        }
        for (a, (fname, ft)) in args.iter().zip(fields.iter()) {
            let at = self.check_expr(a);
            if *ft != Ty::Unknown && !compatible(ft, &at) {
                self.err(
                    a.span(),
                    format!("field '{fname}' of '{name}' is {ft}, cannot assign {at}"),
                );
            }
        }
    }

    /// Check a function or method body with the shared two-pass inference.
    /// Returns the parameter types (excluding an implicit `self`), the
    /// return type, and the body's inferred locals for the backend.
    ///
    /// The enclosing scope is restored before returning, so a caller reads
    /// the locals out of the result rather than out of `self.vars`.
    ///
    /// `receiver` is the canonical type plus kind for a method, which
    /// pre-binds `self` to the record and arms the read-only-`self` rule
    /// for the duration of the body.
    fn check_fn_like(
        &mut self,
        params: &[String],
        body: &[Stmt],
        span: Span,
        receiver: Option<(String, nx_ast::ReceiverKind)>,
    ) -> (Vec<Ty>, Ty, HashMap<String, Ty>) {
        let saved_vars = std::mem::take(&mut self.vars);
        let saved_in_fn = self.in_function;
        let saved_returns = std::mem::take(&mut self.returns);
        let saved_params = std::mem::take(&mut self.param_names);
        let saved_self = self.self_param.take();
        self.in_function = true;
        // `self` narrows like a parameter for inference purposes, but its
        // type is fixed by the receiver rather than discovered.
        let mut all_params: Vec<String> = Vec::with_capacity(params.len() + 1);
        let mut self_ty: Option<Ty> = None;
        if let Some((canon, kind)) = &receiver {
            all_params.push("self".to_string());
            self_ty = Some(Ty::Record(canon.clone()));
            self.self_param = Some(("self".to_string(), *kind));
        }
        all_params.extend(params.iter().cloned());
        self.param_names = all_params.clone();

        // Two passes. A parameter's type comes from how the body
        // uses it, so pass one discovers the types and pass two
        // checks the body with them already known. Without the
        // second pass everything derived from a parameter would
        // come out unresolved: `t = n * 2` is only Int if `n` is.
        let mut rets: Vec<Ty>;
        let mut ret: Ty;
        // What we have learned about each parameter so far, fed
        // back in on the next pass. Seeding the types is what makes
        // the second pass useful: with `n` already Int, `t = n * 2`
        // types `t` instead of widening it to unresolved.
        let mut known: HashMap<String, Ty> = HashMap::new();
        let mut passes = 0;
        loop {
            self.copies.clear();
            self.saw_float = false;
            self.vars.clear();
            self.returns.clear();
            // A function body sees the module's own variables, the
            // same ones codegen resolves to globals. Seeding the
            // scope with them is what lets a function read a
            // top-level constant instead of reporting it undefined.
            for (name, ty) in &saved_vars {
                self.vars.insert(name.clone(), ty.clone());
            }
            if let Some(t) = &self_ty {
                self.vars.insert("self".to_string(), t.clone());
            }
            for p in params {
                self.vars
                    .insert(p.clone(), known.get(p).cloned().unwrap_or(Ty::Unknown));
            }
            let entry_vars = self.vars.clone();
            // Only the final pass's diagnostics are reported: an
            // earlier pass sees types that later passes refine, so
            // its complaints can be spurious. They are still
            // carried here and dropped by the truncate at the top
            // of the next iteration.
            let nerr = self.errors.len();
            self.check_block(body);
            let pass_errors: Vec<CheckError> = self.errors.drain(nerr..).collect();
            // A numeric parameter nothing else pinned defaults to
            // Int, but only in a function that never touches a
            // Float. With no float in the body there is nothing a
            // Float argument could mean, so Int is the only reading
            // left and the caller gets a real signature to check.
            //
            // With a Float present, leaving the parameter dynamic is
            // the honest answer: `zr - zi + cx` and `x / 2.0` are
            // well-typed whatever cx and x are, and pinning them
            // made `fn mandel(cx, cy, maxiter)` come out as
            // `(Int, Int, Int)` and reject Float arguments outright.
            if !self.saw_float {
                for p in std::mem::take(&mut self.numeric_params) {
                    if p != "self" && self.vars.get(&p) == Some(&Ty::Unknown) {
                        self.vars.insert(p, Ty::Int);
                    }
                }
            } else {
                self.numeric_params.clear();
                self.saw_float = false;
            }
            // A plain copy makes both ends the same type; take it
            // from whichever end this pass managed to type.
            for (local, p) in std::mem::take(&mut self.copies) {
                let from_local = self.vars.get(&local).cloned();
                let from_param = self.vars.get(&p).cloned();
                // Take the type from whichever end is known, or
                // accept it when both already agree. Anything else
                // (both unresolved, or a real disagreement) stays
                // unresolved and is reported by define.
                let resolved = match (from_local, from_param) {
                    (Some(Ty::Unknown), Some(t)) => Some(t),
                    (Some(t), Some(Ty::Unknown)) => Some(t),
                    (Some(l), Some(p)) if l == p => Some(l),
                    _ => None,
                };
                if let Some(t) = resolved {
                    self.vars.insert(local, t.clone());
                    self.vars.insert(p, t);
                }
            }
            let snapshot = self.vars.clone();
            for p in params {
                if let Some(t) = snapshot.get(p) {
                    if *t != Ty::Unknown {
                        known.insert(p.clone(), t.clone());
                    }
                }
            }
            rets = std::mem::take(&mut self.returns);
            ret = Ty::None;
            for t in &rets {
                if ret == Ty::None {
                    ret = t.clone();
                } else if compatible(&ret, t) {
                    if ret == Ty::Unknown {
                        ret = t.clone();
                    }
                } else {
                    let r = ret.clone();
                    self.err(span, format!("inconsistent return types: {r} vs {t}"));
                }
            }
            passes += 1;
            // Keep going while a pass still teaches us something.
            // Bounded because each pass can only remove Unknowns.
            let converged = self.vars == entry_vars || passes >= 4;
            if converged {
                // Report the last pass, which had the best types.
                self.errors.extend(pass_errors);
            }
            if converged {
                break;
            }
        }
        let fn_locals = self.vars.clone();
        let param_tys: Vec<Ty> =
            params.iter().map(|p| fn_locals.get(p).cloned().unwrap_or(Ty::Unknown)).collect();
        // The enclosing scope goes back exactly as it was found: a function
        // body must not consume the module's variables, or every statement
        // after the first `fn` would see an empty module.
        self.vars = saved_vars;
        self.in_function = saved_in_fn;
        self.returns = saved_returns;
        self.param_names = saved_params;
        self.self_param = saved_self;
        (param_tys, ret, fn_locals)
    }

    fn define(&mut self, name: &str, ty: Ty, span: Span) {
        // A variable may not share a type's name: `Point = 5` would make
        // `Point(...)` unresolvable, and the same holds for loop variables
        // and imports, which all bind through here.
        if self.records.contains_key(name) {
            self.err(span, format!("'{name}' is already a type; a variable cannot share the name"));
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
                self.err(span, format!("variable '{name}' is {old}, cannot rebind to {ty}"));
            }
        }
    }

    /// Narrow a parameter from the way the body uses it. NX has no
    /// overloading and no generics, so a use site fixes the type: `n - 1`
    /// proves Int, `n < 1.5` proves Float, `not n` proves Bool. Non-params
    /// and already-known types are left alone (locals are monomorphic).
    fn expect_param(&mut self, e: &Expr, ty: Ty) {
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
    fn mark_numeric(&mut self, e: &Expr) {
        if let Expr::Var(name, _) = e {
            if self.param_names.iter().any(|p| p == name) {
                self.numeric_params.push(name.clone());
            }
        }
    }

    /// Record `local = param`, a plain copy that makes both the same
    /// type. Neither side learns anything on its own, so the pair is
    /// resolved after the body by copying whichever end got typed.
    fn mark_copy(&mut self, local: &str, value: &Expr) {
        if let Expr::Var(p, _) = value {
            if self.param_names.iter().any(|x| x == p) {
                self.copies.push((local.to_string(), p.clone()));
            }
        }
    }

    fn check_block(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            self.check_stmt(s);
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Assign { targets, values, span } => {
                // Several targets against one value is destructuring: the
                // value is a tuple or a multiple return, so every target
                // takes the same type here. When the parts genuinely differ
                // the recorded type is unresolved and each target stays
                // dynamic, which is correct and merely unspecialised.
                if targets.len() > 1 && values.len() == 1 {
                    let t = self.check_expr(&values[0]);
                    for target in targets {
                        self.bind_target(target, t.clone(), &values[0], *span);
                    }
                    return;
                }
                if targets.len() != values.len() {
                    self.err(
                        *span,
                        format!("{} targets but {} values", targets.len(), values.len()),
                    );
                    return;
                }
                for (target, value) in targets.iter().zip(values.iter()) {
                    let t = self.check_expr(value);
                    self.bind_target(target, t, value, *span);
                }
            }
            Stmt::TypeDecl { name, fields, span } => {
                // A declaration is a compile-time fact about the module, so
                // it is only meaningful at module level. Inside a function
                // it would be a second, incompatible layout for the same
                // name, which is exactly what a static type system cannot
                // represent.
                if self.in_function {
                    self.err(*span, format!("type '{name}' must be declared at module level"));
                    return;
                }
                if self.records.contains_key(name) {
                    self.err(*span, format!("type '{name}' already declared"));
                    return;
                }
                if self.funcs.contains_key(name) {
                    self.err(
                        *span,
                        format!("'{name}' is already a function; a type cannot share the name"),
                    );
                    return;
                }
                // Field types stay as written and resolve lazily at each
                // use, so a field may name a record declared later --
                // mutually recursive types included. Unknown names are
                // reported once the whole module has been seen.
                let layout: Vec<(String, String)> = fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty.clone()))
                    .collect();
                self.records.insert(name.clone(), layout);
                self.record_spans.insert(name.clone(), *span);
            }
            Stmt::Del { targets, span } => {
                for target in targets {
                    match target {
                        Target::Name(name) => {
                            if self.vars.remove(name).is_none() {
                                self.err(*span, format!("undefined variable '{name}'"));
                            }
                        }
                        Target::Index { base, index } => {
                            let bt = self.check_expr(base);
                            // An unresolved base may hold a list or a dict,
                            // so a scalar key is accepted for either.
                            if matches!(bt, Ty::Unknown) {
                                let kt = self.check_expr(index);
                                if !is_keyable(&kt) {
                                    self.err(index.span(), format!("index must be Int or a dict key, found {kt}"));
                                }
                            } else if matches!(bt, Ty::Dict(_)) {
                                let kt = self.check_expr(index);
                                if !matches!(kt, Ty::Int | Ty::Float | Ty::Bool | Ty::Str | Ty::Unknown) {
                                    self.err(index.span(), format!("dict key must be Int, Float, Bool or Str, found {kt}"));
                                }
                            } else {
                                let it = self.check_expr(index);
                                if !matches!(it, Ty::Int | Ty::Unknown) {
                                    self.err(index.span(), format!("index must be Int, found {it}"));
                                }
                                // Strings are indexable but immutable, so
                                // there is nothing to delete from one.
                                if matches!(bt, Ty::Str) {
                                    self.err(base.span(), "strings are immutable".to_string());
                                } else if !matches!(bt, Ty::List(_) | Ty::Unknown) {
                                    self.err(base.span(), format!("cannot delete an index of {bt}"));
                                }
                            }
                        }
                        Target::Attr { base, field } => {
                            let bt = self.check_expr(base);
                            match bt {
                                Ty::Record(t) => {
                                    // The field has to exist; what it
                                    // currently holds is irrelevant to
                                    // whether removing it is meaningful.
                                    if let Some(fields) = self.records.get(&t) {
                                        if !fields.iter().any(|(n, _)| n == field) {
                                            self.err(
                                                base.span(),
                                                format!("type '{t}' has no field '{field}'"),
                                            );
                                        }
                                    }
                                }
                                Ty::Unknown => {}
                                other => self.err(
                                    base.span(),
                                    format!("cannot delete a field of {other}"),
                                ),
                            }
                        }
                    }
                }
            }
            Stmt::Assert { cond, message, .. } => {
                let t = self.check_expr(cond);
                if !matches!(t, Ty::Bool | Ty::Unknown) {
                    self.err(cond.span(), format!("assert condition must be Bool, found {t}"));
                }
                self.expect_param(cond, Ty::Bool);
                if let Some(m) = message {
                    let mt = self.check_expr(m);
                    if !matches!(mt, Ty::Str | Ty::Unknown) {
                        self.err(m.span(), format!("assert message must be Str, found {mt}"));
                    }
                }
            }
            Stmt::AssignOp { target, op, value, span } => {
                let rhs = self.check_expr(value);
                let label = target_label(target);
                if let Some(cur) = self.target_ty(target, *span) {
                    if let Some(res) = arith_result(&cur, *op, &rhs) {
                        if !compatible(&cur, &res) {
                            self.err(*span, format!("cannot apply '{}=' of {rhs} to {cur} {label}", op.as_str()));
                        }
                    } else {
                        self.err(*span, format!("operator '{}' not supported for {cur} and {rhs}", op.as_str()));
                    }
                }
            }
            Stmt::Print { values, .. } => {
                for v in values {
                    self.check_expr(v);
                }
            }
            Stmt::If { cond, then_body, elifs, else_body, .. } => {
                let t = self.check_expr(cond);
                if !matches!(t, Ty::Bool | Ty::Unknown) {
                    self.err(cond.span(), format!("condition must be Bool, found {t}"));
                }
                self.expect_param(cond, Ty::Bool);
                self.check_block(then_body);
                for (ec, eb) in elifs {
                    let t = self.check_expr(ec);
                    if !matches!(t, Ty::Bool | Ty::Unknown) {
                        self.err(ec.span(), format!("condition must be Bool, found {t}"));
                    }
                    self.expect_param(ec, Ty::Bool);
                    self.check_block(eb);
                }
                if let Some(b) = else_body {
                    self.check_block(b);
                }
            }
            Stmt::While { cond, body, .. } => {
                let t = self.check_expr(cond);
                if !matches!(t, Ty::Bool | Ty::Unknown) {
                    self.err(cond.span(), format!("condition must be Bool, found {t}"));
                }
                self.expect_param(cond, Ty::Bool);
                self.loop_depth += 1;
                self.check_block(body);
                self.loop_depth -= 1;
            }
            Stmt::For { var, iter, body, span } => {
                let elem = match iter {
                    nx_ast::ForIter::Range { start, end } => {
                        let s = self.check_expr(start);
                        let e = self.check_expr(end);
                        if !matches!(s, Ty::Int | Ty::Unknown) {
                            self.err(start.span(), format!("range start must be Int, found {s}"));
                        }
                        if !matches!(e, Ty::Int | Ty::Unknown) {
                            self.err(end.span(), format!("range end must be Int, found {e}"));
                        }
                        self.expect_param(start, Ty::Int);
                        self.expect_param(end, Ty::Int);
                        Ty::Int
                    }
                    nx_ast::ForIter::Each(e) => match self.check_expr(e) {
                        Ty::List(t) => *t,
                        Ty::Str => Ty::Str,
                        // Iterating a dict yields its keys, in insertion
                        // order. The key type is not tracked -- keys may be
                        // any mix of scalars -- so the loop variable stays
                        // dynamic, which is correct and merely unspecialised.
                        Ty::Dict(_) => Ty::Unknown,
                        Ty::Unknown => Ty::Unknown,
                        other => {
                            self.err(e.span(), format!("cannot iterate over {other}"));
                            Ty::Unknown
                        }
                    },
                };
                // Loop var follows the same monomorphic rule.
                match self.vars.get(var).cloned() {
                    None => {
                        if self.records.contains_key(var) {
                            self.err(*span, format!("'{var}' is already a type; a variable cannot share the name"));
                        } else {
                            self.vars.insert(var.clone(), elem);
                        }
                    }
                    Some(old) if compatible(&old, &elem) => {}
                    Some(old) => self.err(*span, format!("loop variable '{var}' is {old}, cannot iterate {elem}")),
                }
                self.loop_depth += 1;
                self.check_block(body);
                self.loop_depth -= 1;
            }
            Stmt::Fn { name, params, body, span } => {
                if self.funcs.contains_key(name) {
                    self.err(*span, format!("function '{name}' already defined"));
                    return;
                }
                if self.records.contains_key(name) {
                    self.err(*span, format!("'{name}' is already a type; a function cannot share the name"));
                    return;
                }
                // A parameter shadowing a type would silently change what
                // `T(...)` means inside the body, so it is refused up front.
                for p in params {
                    if self.records.contains_key(p) {
                        self.err(*span, format!("parameter '{p}' shadows type '{p}'"));
                    }
                }
                // Stub first so the body can call itself recursively.
                self.funcs.insert(
                    name.clone(),
                    (vec![Ty::Unknown; params.len()], Ty::Unknown),
                );
                let (param_tys, ret, fn_locals) = self.check_fn_like(params, body, *span, None);
                let module = self.module_name.clone();
                self.inferred.insert(
                    (module, name.clone()),
                    FnInfo { locals: fn_locals, params: params.clone(), ret: ret.clone() },
                );
                self.funcs.insert(
                    name.clone(),
                    (param_tys, ret),
                );
            }
            Stmt::Impl { type_name, methods, span } => {
                if self.in_function {
                    self.err(*span, "impl blocks must be at module level".to_string());
                    return;
                }
                // The orphan rule: an impl lives with its type. Imported
                // types carry no declaration span, which is what tells them
                // apart from types declared here.
                if !self.records.contains_key(type_name) {
                    self.err(*span, format!("unknown type '{type_name}'"));
                    return;
                }
                if !self.record_spans.contains_key(type_name) {
                    self.err(*span, format!("cannot implement type '{type_name}' from another module"));
                    return;
                }
                let canon = self.canonical_name(type_name);
                for m in methods {
                    let key = (canon.clone(), m.name.clone());
                    if self.methods.contains_key(&key) {
                        self.err(m.span, format!("method '{}' already defined for type '{canon}'", m.name));
                        continue;
                    }
                    for p in &m.params {
                        if self.records.contains_key(p) {
                            self.err(m.span, format!("parameter '{p}' shadows type '{p}'"));
                        }
                    }
                    // Stub first so the body can call itself recursively.
                    self.methods.insert(key.clone(), MethodInfo {
                        params: vec![Ty::Unknown; m.params.len()],
                        ret: Ty::Unknown,
                        receiver: m.receiver,
                        module: self.module_name.clone(),
                    });
                    let recv = match m.receiver {
                        nx_ast::ReceiverKind::None => None,
                        kind => Some((canon.clone(), kind)),
                    };
                    let (param_tys, ret, fn_locals) =
                        self.check_fn_like(&m.params, &m.body, m.span, recv);
                    // A `mut self` method writes its result back into the
                    // receiver (`p = m(p, ...)`), so it must return the
                    // record: anything else would clobber the receiver with
                    // the wrong type.
                    if m.receiver == nx_ast::ReceiverKind::Mut
                        && ret != Ty::Record(canon.clone())
                    {
                        self.err(
                            m.span,
                            format!(
                                "mut method '{}' must return '{canon}', found {ret}",
                                m.name
                            ),
                        );
                    }
                    let info = MethodInfo {
                        params: param_tys,
                        ret: ret.clone(),
                        receiver: m.receiver,
                        module: self.module_name.clone(),
                    };
                    self.methods.insert(key.clone(), info.clone());
                    let module = self.module_name.clone();
                    self.inferred.insert(
                        (module, nx_ast::shape::method_key(&canon, &m.name)),
                        FnInfo {
                            locals: fn_locals,
                            params: m.params.clone(),
                            ret,
                        },
                    );
                }
                let _ = span;
            }
            Stmt::Return { values, span } => {
                if !self.in_function {
                    self.err(*span, "return outside function".to_string());
                    return;
                }

                if values.is_empty() {
                    self.returns.push(Ty::None);
                    return;
                }
                if values.len() == 1 {
                    let t = self.check_expr(&values[0]);
                    self.returns.push(t);
                    return;
                }
                // `return a, b` produces a list, so the function's type is
                // `List(T)`. Recording it as one entry rather than several
                // keeps a mixed tuple (`return 1, "x"`) from being reported
                // as an inconsistent return, while still pinning `T` when
                // the parts agree so destructuring can stay specialised.
                let mut elem = Ty::Unknown;
                for v in values {
                    let t = self.check_expr(v);
                    elem = match elem {
                        Ty::Unknown => t,
                        prev if compatible(&prev, &t) && prev != Ty::Unknown => prev,
                        _ => Ty::Unknown,
                    };
                }
                self.returns.push(Ty::List(Box::new(elem)));
            }

            Stmt::Break { span } => {
                if self.loop_depth == 0 {
                    self.err(*span, "break outside loop".to_string());
                }
            }
            Stmt::Continue { span } => {
                if self.loop_depth == 0 {
                    self.err(*span, "continue outside loop".to_string());
                }
            }
            Stmt::Import { module, alias, span } => {
                if self.check_module(module, *span) {
                    let bind = alias.clone().unwrap_or_else(|| module.clone());
                    if self.records.contains_key(&bind) {
                        self.err(*span, format!("'{bind}' is already a type; a variable cannot share the name"));
                        return;
                    }

                    self.vars.insert(bind, Ty::Module(module.clone()));
                }
            }
            Stmt::FromImport { module, names, span } => {
                if !self.check_module(module, *span) {
                    return;
                }
                let info = self.modules.get(module).cloned().unwrap_or_default();
                for (name, alias) in names {
                    let bind = alias.clone().unwrap_or_else(|| name.clone());

                    if let Some(t) = info.vars.get(name) {
                        self.define(&bind, t.clone(), *span);
                    } else if let Some((p, r)) = info.funcs.get(name) {
                        self.define(&bind, Ty::Func(p.clone(), Box::new(r.clone())), *span);
                    } else if let Some(layout) = info.types.get(name) {
                        // Importing a type brings its layout, keyed under
                        // the alias when one is given, so `from m import T
                        // as U` followed by `U(...)` resolves. The canonical
                        // name is registered too: `U(...)` constructs
                        // `Ty::Record("T")`, and field access on the result
                        // has to find the layout under that name. The
                        // type's methods come along the same way.
                        self.records.insert(bind.clone(), layout.clone());
                        self.records.insert(name.clone(), layout.clone());
                        if bind != *name {
                            self.type_alias.insert(bind, name.clone());
                        }
                        for ((t, m), minfo) in &info.methods {
                            if t == name {
                                self.methods.insert((t.clone(), m.clone()), minfo.clone());
                            }
                        }
                    } else {
                        self.err(*span, format!("module '{module}' has no member '{name}'"));
                    }
                }
            }
            Stmt::Expr(e) => {
                self.check_expr(e);
            }
        }
    }

    fn check_module(&mut self, name: &str, span: Span) -> bool {
        if self.modules.contains_key(name) {
            return true;
        }
        if self.loading.contains(&name.to_string()) {
            self.err(span, format!("circular import of '{name}'"));
            return false;
        }
        let path = match nx_ast::shape::resolve_module_file(&[self.base.clone()], name) {
            Some(p) => p,
            None => {
                self.err(span, format!("cannot find module '{name}.nx'"));
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
        self.check_block(&prog.stmts);
        self.loading.pop();
        // Unknown field-type names are reported now that the whole module
        // has been seen: a field may name a record declared later.
        self.validate_records();
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
            FnInfo { locals: info.vars.clone(), params: Vec::new(), ret: Ty::None },
        );
        self.base = saved_base;
        self.module_name = saved_module;
        self.modules.insert(name.to_string(), info);
        true
    }

    fn check_expr(&mut self, expr: &Expr) -> Ty {
        let t = self.check_expr_inner(expr);
        if t == Ty::Float {
            // A float can appear in an expression without ever landing in
            // a local, as in `fn half(x): return x / 2.0`. Remember it so
            // the parameter rule below can see the function is float-shaped.
            self.saw_float = true;
        }
        t
    }

    fn check_expr_inner(&mut self, expr: &Expr) -> Ty {
        match expr {
            Expr::Int(..) => Ty::Int,
            Expr::Float(..) => Ty::Float,
            Expr::Bool(..) => Ty::Bool,
            Expr::Str(..) => Ty::Str,
            Expr::NoneLit(..) => Ty::None,
            Expr::Range { start, end, span } => {
                let s = self.check_expr(start);
                let e = self.check_expr(end);
                for (t, node) in [(&s, &**start), (&e, &**end)] {
                    if !matches!(t, Ty::Int | Ty::Unknown) {
                        self.err(node.span(), format!("range bound must be Int, found {t}"));
                    }
                    self.expect_param(node, Ty::Int);
                }
                let _ = span;
                // A range is a list of Ints, whatever the bounds turned out
                // to be, so it composes with everything list-shaped.
                Ty::List(Box::new(Ty::Int))
            }
            Expr::Dict(pairs, span) => {
                // Keys must be comparable, or the dict cannot be looked up.
                // Values may be anything; that is what makes a dict the
                // natural heterogeneous container.
                for (k, _) in pairs {
                    let kt = self.check_expr(k);
                    if !matches!(
                        kt,
                        Ty::Int | Ty::Float | Ty::Bool | Ty::Str | Ty::Unknown
                    ) {
                        self.err(k.span(), format!("dict key must be Int, Float, Bool or Str, found {kt}"));
                    }
                }
                let _ = span;
                Ty::Dict(Box::new(Ty::Unknown))
            }
            Expr::Slice { base, from, to, step, span } => {
                let b = self.check_expr(base);
                for part in [from, to, step].into_iter().flatten() {
                    let t = self.check_expr(part);
                    if !matches!(t, Ty::Int | Ty::Unknown) {
                        self.err(part.span(), format!("slice bound must be Int, found {t}"));
                    }
                    self.expect_param(part, Ty::Int);
                }
                match b {
                    Ty::List(_) => b,
                    Ty::Str => Ty::Str,
                    Ty::Unknown => Ty::Unknown,
                    other => {
                        self.err(*span, format!("cannot slice {other}"));
                        Ty::Unknown
                    }
                }
            }
            Expr::IfExpr { cond, then_value, else_value, .. } => {
                let c = self.check_expr(cond);
                if !matches!(c, Ty::Bool | Ty::Unknown) {
                    self.err(cond.span(), format!("condition must be Bool, found {c}"));
                }
                self.expect_param(cond, Ty::Bool);
                let t = self.check_expr(then_value);
                let e = self.check_expr(else_value);
                if compatible(&t, &e) {
                    if t == Ty::Unknown {
                        e
                    } else {
                        t
                    }
                } else if t == Ty::None {
                    // `x if c else None` is the optional idiom, so the
                    // None branch must not be an error.
                    e
                } else if e == Ty::None {
                    t
                } else {
                    self.err(
                        then_value.span(),
                        format!("branches disagree: {t} vs {e}"),
                    );
                    Ty::Unknown
                }
            }
            Expr::Comprehension { element, var, iter, cond, span } => {
                let it = self.check_expr(iter);
                let elem = match it {
                    Ty::List(t) => *t,
                    Ty::Str => Ty::Str,
                    Ty::Unknown => Ty::Unknown,
                    other => {
                        self.err(iter.span(), format!("cannot iterate over {other}"));
                        Ty::Unknown
                    }
                };
                // The loop variable is scoped to the comprehension, so it is
                // bound here and restored after rather than leaking out and
                // colliding with a same-named variable elsewhere.
                let saved = self.vars.get(var).cloned();
                if saved.is_none() && self.records.contains_key(var) {
                    self.err(iter.span(), format!("'{var}' is already a type; a variable cannot share the name"));
                } else {
                    self.vars.insert(var.clone(), elem);
                }
                if let Some(c) = cond {
                    let ct = self.check_expr(c);
                    if !matches!(ct, Ty::Bool | Ty::Unknown) {
                        self.err(c.span(), format!("filter must be Bool, found {ct}"));
                    }
                }
                let et = self.check_expr(element);
                match saved {
                    Some(t) => {
                        self.vars.insert(var.clone(), t);
                    }
                    None => {
                        self.vars.remove(var);
                    }
                }
                let _ = span;
                Ty::List(Box::new(et))
            }
            Expr::List(items, span) => {
                let mut elem = Ty::Unknown;
                for it in items {
                    let t = self.check_expr(it);
                    if elem == Ty::Unknown {
                        elem = t;
                    } else if !compatible(&elem, &t) {
                        self.err(*span, format!("mixed list element types: {elem} vs {t}"));
                        break;
                    }
                }
                Ty::List(Box::new(elem))
            }
            Expr::Var(name, span) => {
                if let Some(t) = self.vars.get(name).cloned() {
                    return t;
                }
                if let Some((p, r)) = self.funcs.get(name).cloned() {
                    return Ty::Func(p, Box::new(r));
                }
                self.err(*span, format!("undefined variable '{name}'"));
                Ty::Unknown
            }
            Expr::Attr { base, attr, span } => {
                let b = self.check_expr(base);
                match b {
                    Ty::Module(m) => match self.modules.get(&m).cloned() {
                        Some(info) => {
                            if let Some(t) = info.vars.get(attr) {
                                return t.clone();
                            }
                            if let Some((p, r)) = info.funcs.get(attr) {
                                return Ty::Func(p.clone(), Box::new(r.clone()));
                            }
                            self.err(*span, format!("module '{m}' has no member '{attr}'"));
                            Ty::Unknown
                        }
                        None => {
                            self.err(*span, format!("unknown module '{m}'"));
                            Ty::Unknown
                        }
                    },
                    // A known record resolves the field statically, which is
                    // what lets the backend use a constant offset.
                    Ty::Record(t) => match self.records.get(&t).cloned() {
                        Some(fields) => {
                            let recs = self.records.clone();
                            match fields.iter().find(|(n, _)| n == attr) {
                                Some((_, ft)) => field_ty(ft, &recs),
                                None => {
                                    self.err(*span, format!("type '{t}' has no field '{attr}'"));
                                    Ty::Unknown
                                }
                            }
                        }
                        None => Ty::Unknown,
                    },
                    // A dynamic base is resolved by name at runtime, so the
                    // field type is genuinely unknown here.
                    Ty::Unknown => Ty::Unknown,
                    other => {
                        self.err(*span, format!("attribute access needs a module or a type, found {other}"));
                        Ty::Unknown
                    }
                }
            }
            Expr::Index { base, index, span } => {
                let b = self.check_expr(base);
                // A dict is keyed by value, so its index need not be an Int.
                // An unresolved base may hold either a list or a dict, so a
                // scalar key is accepted and the runtime dispatches on the
                // actual tag; anything else is rejected either way.
                if matches!(b, Ty::Dict(_) | Ty::Unknown) {
                    let ix = self.check_expr(index);
                    if !is_keyable(&ix) {
                        self.err(
                            index.span(),
                            format!("index must be Int or a dict key, found {ix}"),
                        );
                    }
                    return match b {
                        Ty::Dict(t) => *t,
                        _ => Ty::Unknown,
                    };
                }
                let ix = self.check_expr(index);
                if !matches!(ix, Ty::Int | Ty::Unknown) {
                    self.err(index.span(), format!("index must be Int, found {ix}"));
                }
                self.expect_param(index, Ty::Int);
                match b {
                    Ty::List(t) => *t,
                    Ty::Str => Ty::Str,
                    Ty::Unknown => Ty::Unknown,
                    other => {
                        self.err(*span, format!("only lists, strings and dicts support indexing, found {other}"));
                        Ty::Unknown
                    }
                }
            }
            Expr::Unary { op, expr, span } => {
                let t = self.check_expr(expr);
                match (op, &t) {
                    (UnaryOp::Neg, Ty::Int) => Ty::Int,
                    (UnaryOp::Neg, Ty::Float) => Ty::Float,
                    (UnaryOp::Neg, Ty::Unknown) => Ty::Unknown,
                    (UnaryOp::Not, Ty::Bool) => Ty::Bool,
                    (UnaryOp::Not, Ty::Unknown) => Ty::Unknown,
                    // `~x` complements bits, so it is Int-only. Accepting a
                    // Float here would mean inventing a rounding rule.
                    (UnaryOp::BitNot, Ty::Int) => Ty::Int,
                    (UnaryOp::BitNot, Ty::Unknown) => Ty::Unknown,
                    // Unary plus preserves the type and nothing else.
                    (UnaryOp::Pos, Ty::Int) | (UnaryOp::Pos, Ty::Float) => t,
                    (UnaryOp::Pos, Ty::Unknown) => Ty::Unknown,
                    _ => {
                        self.err(*span, format!("operator '{}' not supported for {t}", op.as_str()));
                        Ty::Unknown
                    }
                }
            }
            Expr::Binary { left, op, right, span } => {
                let l = self.check_expr(left);
                let r = self.check_expr(right);
                match op {
                    BinOp::And | BinOp::Or => {
                        for (side, t) in [("left", &l), ("right", &r)] {
                            if !matches!(t, Ty::Bool | Ty::Unknown) {
                                self.err(*span, format!("'{0}' operand of '{1}' must be Bool, found {t}", side, op.as_str()));
                            }
                        }
                        self.expect_param(left, Ty::Bool);
                        self.expect_param(right, Ty::Bool);
                        Ty::Bool
                    }
                    BinOp::Eq | BinOp::NotEq => {
                        if !(compatible(&l, &r)
                            || (is_numeric(&l) && is_numeric(&r)))
                        {
                            self.err(*span, format!("cannot compare {l} and {r}"));
                        }
                        // A numeric comparison against a known scalar pins
                        // the other side to the same scalar type.
                        if is_numeric(&l) && l != Ty::Unknown {
                            self.expect_param(right, l.clone());
                        }
                        if is_numeric(&r) && r != Ty::Unknown {
                            self.expect_param(left, r.clone());
                        }
                        Ty::Bool
                    }
                    BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                        if !((is_numeric(&l) && is_numeric(&r))
                            || (matches!(l, Ty::Str | Ty::Unknown)
                                && matches!(r, Ty::Str | Ty::Unknown)))
                        {
                            self.err(*span, format!("cannot order {l} and {r}"));
                        }
                        if is_numeric(&l) && l != Ty::Unknown {
                            self.expect_param(right, l.clone());
                        }
                        if is_numeric(&r) && r != Ty::Unknown {
                            self.expect_param(left, r.clone());
                        }
                        if l == Ty::Str || r == Ty::Str {
                            self.expect_param(left, Ty::Str);
                            self.expect_param(right, Ty::Str);
                        }
                        Ty::Bool
                    }
                    BinOp::In | BinOp::NotIn => {
                        let c = self.check_expr(right);
                        match c {
                            Ty::List(_) | Ty::Str | Ty::Dict(_) | Ty::Unknown => {}
                            other => {
                                self.err(
                                    right.span(),
                                    format!("'{}' needs a list, string or dict on the right, found {other}", op.as_str()),
                                );
                            }
                        }
                        Ty::Bool
                    }
                    BinOp::Shl | BinOp::Shr => {
                        // The shift distance is a count, not a value of the
                        // shifted type, so it is checked separately. This is
                        // what lets `1 << n` work when n is an Int
                        // parameter and the left side is a literal.
                        if !matches!(r, Ty::Int | Ty::Unknown) {
                            self.err(right.span(), format!("shift distance must be Int, found {r}"));
                        }
                        self.expect_param(right, Ty::Int);
                        if !matches!(l, Ty::Int | Ty::Unknown) {
                            self.err(*span, format!("operator '{}' needs Int on the left, found {l}", op.as_str()));
                        }
                        Ty::Int
                    }
                    BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                        for (side, t) in [("left", &l), ("right", &r)] {
                            if !matches!(t, Ty::Int | Ty::Unknown) {
                                self.err(
                                    *span,
                                    format!("'{0}' operand of '{1}' must be Int, found {t}", side, op.as_str()),
                                );
                            }
                        }
                        Ty::Int
                    }
                    BinOp::FloorDiv | BinOp::Mod | BinOp::Pow => {
                        self.mark_numeric(left);
                        self.mark_numeric(right);
                        match arith_result(&l, *op, &r) {
                            Some(t) => t,
                            None => {
                                self.err(*span, format!("operator '{}' not supported for {l} and {r}", op.as_str()));
                                Ty::Unknown
                            }
                        }
                    }
                    BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                        if matches!(op, BinOp::Add)
                            && (l == Ty::Str || r == Ty::Str)
                        {
                            self.expect_param(left, Ty::Str);
                            self.expect_param(right, Ty::Str);
                        } else {
                            self.mark_numeric(left);
                            self.mark_numeric(right);
                        }
                        match arith_result(&l, *op, &r) {
                            Some(t) => t,
                            None => {
                                self.err(*span, format!("operator '{}' not supported for {l} and {r}", op.as_str()));
                                Ty::Unknown
                            }
                        }
                    }
                }
            }
            Expr::Call { callee, args, span } => {
                // A declared type shadows nothing, but it does share the
                // `Name(...)` spelling with a function, so the type case
                // is resolved first. `m.T(...)` through a module alias
                // resolves the same way, against the module's exports.
                //
                // An alias constructs the canonical name: `from m import T
                // as U` followed by `U(...)` yields `Ty::Record("T")`, so a
                // value built through the alias compares equal to one built
                // through the original name.
                if let Expr::Var(name, _) = callee.as_ref() {
                    if let Some(fields) = self.records.get(name).cloned() {
                        let canon = self.canonical_name(name);
                        // Field types resolve now, against every declaration
                        // in the module -- including later ones.
                        let recs = self.records.clone();
                        let resolved: Vec<(String, Ty)> = fields
                            .iter()
                            .map(|(n, t)| (n.clone(), field_ty(t, &recs)))
                            .collect();
                        self.check_record_args(&canon, &resolved, args, *span);
                        return Ty::Record(canon);
                    }
                }
                if let Expr::Attr { base, attr, .. } = callee.as_ref() {
                    if let Expr::Var(m, _) = base.as_ref() {
                        if let Some(info) = self.modules.get(m).cloned() {
                            if let Some(fields) = info.types.get(attr).cloned() {
                                // Resolve against the declaring module's own
                                // table: its field types name its records.
                                let resolved: Vec<(String, Ty)> = fields
                                    .iter()
                                    .map(|(n, t)| (n.clone(), field_ty(t, &info.types)))
                                    .collect();
                                self.check_record_args(attr, &resolved, args, *span);
                                return Ty::Record(attr.clone());
                            }
                        }
                    }
                }
                // Method, associated-function, and builtin-sugar calls through
                // `base.attr(...)`. Resolution order is load-bearing:
                // modules first (a module never means a value), then
                // associated functions on a type name, then impl methods on
                // a statically known record, and finally builtin sugar
                // (`xs.push(1)` for `push(xs, 1)`), which also covers
                // unresolved bases. Dynamic method dispatch is Stage 4.
                if let Expr::Attr { base, attr, .. } = callee.as_ref() {
                    // `T.m(...)` where T names a type: an associated
                    // function. Tested before the base is evaluated as a
                    // value, because a type name is not a binding -- asking
                    // `check_expr` about one would report it undefined. No
                    // sugar fallback either: the base is not a value, so
                    // anything else is meaningless.
                    if let Expr::Var(n, _) = base.as_ref() {
                        if self.records.contains_key(n) {
                            let canon = self.canonical_name(n);
                            if let Some(info) = self.methods.get(&(canon.clone(), attr.clone())).cloned() {
                                if info.receiver != nx_ast::ReceiverKind::None {
                                    self.err(
                                        *span,
                                        format!("method '{attr}' needs a receiver; call it on a '{canon}' value"),
                                    );
                                    return Ty::Unknown;
                                }
                                self.check_call_args(&info.params, args, *span);
                                return info.ret;
                            }
                            self.err(*span, format!("type '{canon}' has no associated function '{attr}'"));
                            return Ty::Unknown;
                        }
                    }
                    // A module base is a module function, checked like a
                    // plain call against the module's signature.
                    if let Ty::Module(m) = self.check_expr(base) {
                        if let Some(info) = self.modules.get(&m).cloned() {
                            if let Some((p, r)) = info.funcs.get(attr) {
                                let (p, r) = (p.clone(), r.clone());
                                self.check_call_args(&p, args, *span);
                                return r;
                            }
                        }
                        // A module variable is not callable, same as calling
                        // any other non-function value.
                        for a in args {
                            self.check_expr(a);
                        }
                        self.err(*span, format!("module '{m}' has no function '{attr}'"));
                        return Ty::Unknown;
                    }
                    // `v.m(...)` on a statically known record: an impl
                    // method. Anything less than a record falls through to
                    // builtin sugar below.
                    let bt = self.check_expr(base);
                    if let Ty::Record(t) = bt {
                        if let Some(info) = self.methods.get(&(t.clone(), attr.clone())).cloned() {
                            // A `mut self` call writes its result back into the
                            // receiver when the receiver has storage. With no
                            // storage -- a call result, a literal, anything
                            // temporary -- there is nowhere to write, and the
                            // call still evaluates to the modified record.
                            // That is what lets a chain read as a single
                            // expression: `q.moved(1, 1).moved(2, 2)`.
                            if info.receiver == nx_ast::ReceiverKind::Mut
                                && self.self_root_denied(base)
                            {
                                self.err(
                                    *span,
                                    "cannot call mut method through read-only 'self'".to_string(),
                                );
                            }
                            self.check_call_args(&info.params, args, *span);
                            return info.ret;
                        }
                        self.err(*span, format!("type '{t}' has no method '{attr}'"));
                        return Ty::Unknown;
                    }
                    // Builtin sugar: `xs.push(1)` for `push(xs, 1)`. Reached
                    // only when the base is neither a module, a type, nor a
                    // record -- including unresolved bases, which is what
                    // keeps `x.push(1)` working on dynamic values.
                    if crate::builtin_arity(attr).is_some() {
                        let mut sugared: Vec<Expr> = vec![(**base).clone()];
                        sugared.extend(args.iter().cloned());
                        return self.check_builtin(attr, &sugared, *span);
                    }
                    if matches!(bt, Ty::Unknown) {
                        self.err(
                            *span,
                            format!("cannot resolve method '{attr}' on unresolved value (dynamic dispatch arrives in Stage 4)"),
                        );
                    } else {
                        self.err(*span, format!("'{attr}' is not a method; only modules, types and builtins support attribute calls"));
                    }
                    return Ty::Unknown;
                }
                // Builtins.
                if let Expr::Var(name, _) = callee.as_ref() {
                    if crate::builtin_arity(name).is_some() {
                        return self.check_builtin(name, args, *span);
                    }
                }
                let f = self.check_expr(callee);
                match f {
                    Ty::Func(params, ret) => {
                        self.check_call_args(&params, args, *span);
                        *ret
                    }
                    Ty::Unknown => {
                        for a in args {
                            self.check_expr(a);
                        }
                        Ty::Unknown
                    }
                    other => {
                        for a in args {
                            self.check_expr(a);
                        }
                        self.err(*span, format!("not callable: {other}"));
                        Ty::Unknown
                    }
                }
            }
        }
    }
}

fn arith_result(l: &Ty, op: BinOp, r: &Ty) -> Option<Ty> {
    use Ty::*;
    // Operator result matrix. This is the single owner of Int/Float promotion.
    if matches!(op, BinOp::Add) {
        if matches!((l, r), (Str, Str)) {
            return Some(Str);
        }
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
        _ => Option::None,
    }
}

pub fn check_source(source: &str, base: &std::path::Path) -> Result<(), Vec<CheckError>> {
    let tokens = nx_lexer::lex(source).map_err(|e| {
        vec![CheckError { message: e.message, line: e.line, col: e.col }]
    })?;
    let prog = nx_parser::parse(tokens).map_err(|e| {
        vec![CheckError { message: e.message, line: e.line, col: e.col }]
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
    out.insert((module.to_string(), "<top>".to_string()), FnInfo {
        locals: c.vars.clone(),
        params: Vec::new(),
        ret: Ty::None,
    });
    Ok(out)
}

// Default impls for the checker state.
impl Default for Checker {
    fn default() -> Self {
        Self {
            vars: HashMap::new(),
            funcs: HashMap::new(),
            modules: HashMap::new(),
            loading: Vec::new(),
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


#[cfg(test)]
mod tests {
    use super::*;

    fn ok(src: &str) {
        if let Err(es) = check_source(src, std::path::Path::new(".")) {
            panic!("{src:?} unexpectedly failed: {es:?}");
        }
    }

    fn err(src: &str) -> Vec<CheckError> {
        check_source(src, std::path::Path::new(".")).expect_err("expected type errors")
    }

    // ---- Stage 1 ----

    #[test]
    fn new_operators_type_check() {
        ok("x = 7 % 3\ny = x // 2\nz = y ** 2\n");
        ok("x = 7 & 3\nx = x | 1\nx = x ^ 1\nx = x << 2\nx = x >> 1\nx = ~x\n");
        ok("x = 2 ** 0.5\nprint(x)\n");
    }

    /// `%`, `//` and the bitwise operators are Int-only. Widening them to
    /// Float would mean inventing a rounding rule for `7 % 2.5`, so they
    /// are refused instead.
    #[test]
    fn integral_operators_reject_floats() {
        assert!(!err("x = 7 % 2.5\n").is_empty());
        assert!(!err("x = 7 // 2.5\n").is_empty());
        assert!(!err("x = 7 & 2.5\n").is_empty());
        // The shift distance is a count, so it is Int even though the
        // shifted value could be anything integral.
        assert!(!err("x = 1 << 2.5\n").is_empty());
    }

    #[test]
    fn membership_type_checks() {
        ok("xs = [1, 2, 3]\nprint(1 in xs)\nprint(1 not in xs)\n");
        ok("print(\"a\" in \"abc\")\n");
        ok("d = {\"a\": 1}\nprint(\"a\" in d)\n");
        assert!(!err("print(1 in 5)\n").is_empty());
    }

    #[test]
    fn ternary_branches_must_agree() {
        ok("x = 1 if true else 2\n");
        // `None` is the optional idiom, so it is allowed to disagree.
        ok("x = 1 if true else None\n");
        ok("x = None if true else 1\n");
        assert!(!err("x = 1 if true else \"a\"\n").is_empty());
        assert!(!err("x = 1 if 5 else 2\n").is_empty());
    }

    #[test]
    fn indexed_assignment_checks_element_type() {
        ok("xs = [1, 2, 3]\nxs[0] = 9\n");
        // A List(Int) must not be handed a String by a plain assignment.
        let es = err("xs = [1, 2, 3]\nxs[0] = \"a\"\n");
        assert!(es.iter().any(|e| e.message.contains("cannot store")), "{es:?}");
        assert!(!err("xs = [1, 2, 3]\nxs[0.5] = 9\n").is_empty());
        assert!(!err("xs = [1, 2, 3]\nxs[0] += \"a\"\n").is_empty());
    }

    #[test]
    fn dict_index_uses_value_keys() {
        ok("d = {\"a\": 1}\nd[\"b\"] = 2\nprint(d[\"a\"])\n");
        // A dict is keyed by value, so a String key is fine where a
        // positional index would have to be an Int.
        ok("d = {1: \"a\", 2.5: \"b\", true: \"c\"}\nprint(d[1])\n");
        assert!(!err("d = {\"a\": 1}\nprint(d[[1]])\n").is_empty());
    }

    #[test]
    fn dict_keys_must_be_scalar() {
        assert!(!err("d = {[[1]]: 2}\n").is_empty());
        // Values may be anything -- that is how a dict carries
        // heterogeneity on purpose.
        ok("d = {\"a\": [1, 2], \"b\": \"x\"}\n");
    }

    #[test]
    fn multiple_assignment_shape_is_checked() {
        ok("a, b = 1, 2\n");
        ok("fn f():\n    return 1, 2\na, b = f()\n");
        // Two targets against one value is destructuring, which the
        // resolved at runtime; one target against two values
        // is a shape error.
        assert!(!err("a = 1, 2\n").is_empty());
    }

    /// A mixed tuple keeps the function's type usable rather than being
    /// reported as an inconsistent return.
    #[test]
    fn mixed_tuple_return_is_allowed() {
        ok("fn f():\n    return 1, \"a\"\nx, y = f()\nprint(x)\n");
    }

    /// `None` must not pin a variable or conflict with one, so the
    /// `x = 1` / `x = None` / `x = 2` pattern works.
    #[test]
    fn none_does_not_pin_or_conflict() {
        ok("x = 1\nx = None\nx = 2\nprint(x)\n");
        ok("x = None\nx = 5\n");
        ok("x = \"a\"\nx = None\n");
    }

    #[test]
    fn comprehension_scopes_its_variable() {
        // The loop variable is bound inside the comprehension, so a
        // same-named outer binding is neither read nor clobbered.
        ok("i = 99\nxs = [i for i in 0..3]\nprint(i)\n");
        ok("xs = [i * 2 for i in 0..5 if i > 1]\n");
        ok("i = \"a\"\nxs = [i for i in 0..3]\nprint(i)\n");
        // A filter must be a Bool, and the source must be iterable.
        assert!(!err("xs = [i for i in 0..3 if i]\n").is_empty());
        assert!(!err("xs = [i for i in 5]\n").is_empty());
    }

    #[test]
    fn slice_bounds_must_be_int() {
        ok("xs = [1, 2, 3]\nprint(xs[1:2])\nprint(xs[:2])\nprint(xs[::2])\n");
        // Slicing a string is legal and yields a string.
        ok("s = \"abc\"\nprint(s[0:1])\n");
        assert!(!err("xs = [1, 2, 3]\nprint(xs[1.5:])\n").is_empty());
        assert!(!err("print(5[0:1])\n").is_empty());
    }

    #[test]
    fn range_bounds_must_be_int() {
        ok("for i in 0..5:\n    print(i)\n");
        ok("xs = [i for i in 0..5]\n");
        ok("xs = 0..5\n");
        assert!(!err("for i in 0.0..5:\n    print(i)\n").is_empty());
    }

    #[test]
    fn assert_and_del_type_check() {
        ok("assert 1 < 2\nassert 1 < 2, \"nope\"\n");
        assert!(!err("assert 1\n").is_empty());
        assert!(!err("assert true, 5\n").is_empty());
        ok("xs = [1, 2]\ndel xs[0]\n");
        assert!(!err("s = \"ab\"\ndel s[0]\n").is_empty());
        assert!(!err("del never_defined\n").is_empty());
        // `del` on a name unbinds it, so any later use is undefined --
        // which is what makes the runtime's rebind-to-None unobservable:
        // no checked program can read the slot afterwards.
        ok("x = 1\ndel x\n");
        assert!(!err("x = 1\ndel x\nprint(x)\n").is_empty());
    }

    /// A push into a `List(?)` pins the element type, so a list built by
    /// pushing is as precisely typed as a literal. Without this, the only
    /// way to grow a list would leave every such list unresolved.
    #[test]
    fn push_pins_list_element_type() {
        ok("xs = []\npush(xs, 1)\ny = xs[0] + 1\n");
        ok("xs = []\npush(xs, 1.5)\n");
        // ...but monomorphism still holds: a second, incompatible push is
        // refused, the same as a mixed literal would be.
        assert!(!err("xs = []\npush(xs, 1)\npush(xs, \"a\")\n").is_empty());
        assert!(!err("xs = [1]\npush(xs, \"a\")\n").is_empty());
    }

    #[test]
    fn iterating_a_dict_yields_keys() {
        ok("d = {\"a\": 1}\nfor k in d:\n    print(k)\n");
        // A dict's keys may be any mix of scalars, so the loop variable
        // is unresolved rather than guessed at.
        ok("d = {\"a\": 1}\nfor k in d:\n    k = 5\n    print(k)\n");
        assert!(!err("for k in 5:\n    print(k)\n").is_empty());
    }

    /// Strings are immutable, so assigning into one is refused even
    /// though indexing a string is fine.
    #[test]
    fn strings_are_immutable() {
        let es = err("s = \"ab\"\ns[0] = \"z\"\n");
        assert!(es.iter().any(|e| e.message.contains("immutable")), "{es:?}");
    }

    // ---- records ----

    #[test]
    fn record_declaration_and_use() {
        ok("type Point:\n    x: Float\n    y: Float\np = Point(1.0, 2.0)\nprint(p.x)\n");
        // The field type as written is what a read produces.
        ok("type P:\n    n: Int\np = P(1)\nq = p.n + 1\n");
        // A field may be left unresolved and pinned by use instead.
        ok("type P:\n    n\np = P(1)\nq = p.n + 1\n");
    }

    #[test]
    fn record_constructor_arity_is_exact() {
        let es = err("type Point:\n    x: Int\n    y: Int\np = Point(1)\n");
        assert!(es.iter().any(|e| e.message.contains("takes 2 fields")), "{es:?}");
        assert!(!err("type Point:\n    x: Int\np = Point(1, 2)\n").is_empty());
    }

    #[test]
    fn record_field_types_are_checked() {
        let es = err("type Point:\n    x: Float\np = Point(1)\n");
        assert!(es.iter().any(|e| e.message.contains("is Float")), "{es:?}");
    }

    #[test]
    fn record_field_access_is_checked() {
        let es = err("type Point:\n    x: Int\np = Point(1)\nprint(p.z)\n");
        assert!(es.iter().any(|e| e.message.contains("no field 'z'")), "{es:?}");
        let es = err("type Point:\n    x: Int\np = Point(1)\np.z = 2\n");
        assert!(es.iter().any(|e| e.message.contains("no field 'z'")), "{es:?}");
        // A scalar is not something with fields.
        assert!(!err("n = 1\nprint(n.x)\n").is_empty());
    }

    #[test]
    fn record_field_write_checks_type() {
        ok("type Point:\n    x: Int\np = Point(1)\np.x = 2\n");
        let es = err("type Point:\n    x: Int\np = Point(1)\np.x = \"a\"\n");
        assert!(es.iter().any(|e| e.message.contains("cannot assign")), "{es:?}");
    }

    #[test]
    fn record_declaration_rules() {
        // Module-level only: a second layout for the same name inside a
        // function is not something a static type can express.
        assert!(!err("fn f():\n    type P:\n        x: Int\n    return 1\n").is_empty());
        assert!(!err("type P:\n    x: Int\ntype P:\n    y: Int\n").is_empty());
        // A type cannot share a name with a function, since both would be
        // written `P(...)`.
        assert!(!err("fn P():\n    return 1\ntype P:\n    x: Int\n").is_empty());
        assert!(!err("type P:\n    x: Nope\n").is_empty());
        assert!(!err("type P:\n").is_empty());
    }

    /// Field types resolve lazily, so order does not matter: a field may
    /// name a record declared later, and mutually recursive pairs work.
    /// Only genuinely unknown names are reported, once the module has
    /// been fully seen.
    #[test]
    fn record_field_types_resolve_lazily() {
        ok("type A:\n    b: B\ntype B:\n    n: Int\na = A(B(1))\nprint(a.b.n)\n");
        ok("type A:\n    b: B\ntype B:\n    a: A\n");
        ok("type P:\n    xs: List\n    d: Dict\n    u: Any\n");
        let es = err("type P:\n    x: Nope\n");
        assert!(es.iter().any(|e| e.message.contains("unknown field type 'Nope'")), "{es:?}");
    }

    #[test]
    fn unknown_type_is_an_error() {
        assert!(!err("p = Nope(1)\n").is_empty());
        // A function is not a constructor.
        assert!(!err("fn f():\n    return 1\np = f(1)\n").is_empty());
    }

    #[test]
    fn basic_program_passes() {
        ok("x = 1\ny = x + 2.5\nprint(x, y)\n");
    }

    #[test]
    fn rebind_different_type_errors() {
        let es = err("x = \"a\"\nx = 1\n");
        assert!(es.iter().any(|e| e.message.contains("cannot rebind")));
    }

    #[test]
    fn undefined_var_errors() {
        assert!(!err("print(y)\n").is_empty());
    }

    #[test]
    fn arith_mismatch_errors() {
        assert!(!err("x = 1 + \"a\"\n").is_empty());
    }

    #[test]
    fn non_bool_condition_errors() {
        assert!(!err("if 1:\n    print(1)\n").is_empty());
    }

    #[test]
    fn call_arity_errors() {
        assert!(!err("fn f(a):\n    return a\nprint(f(1, 2))\n").is_empty());
    }

    #[test]
    fn len_push_checked() {
        ok("a = [1]\npush(a, 2)\nprint(len(a))\n");
        assert!(!err("print(len(1))\n").is_empty());
        assert!(!err("a = 1\npush(a, 2)\n").is_empty());
    }

    #[test]
    fn input_checked() {
        // No prompt and a string prompt both answer Str.
        ok("name = input()\nprint(name)\n");
        ok("name = input(\"who: \")\nprint(name)\n");
        assert!(!err("x = input(1, 2)\n").is_empty());
        assert!(!err("x = input(5)\n").is_empty());
        let m = infer("x = input()\n");
        assert_eq!(m[&("__main__".into(), "<top>".into())].locals["x"], Ty::Str);
    }

    #[test]
    fn return_consistency() {
        ok("fn f(n):\n    if n:\n        return 1\n    else:\n        return 2\n");
        assert!(!err("fn f(n):\n    if n:\n        return 1\n    else:\n        return \"a\"\n").is_empty());
    }

    #[test]
    fn break_outside_errors() {
        assert!(!err("break\n").is_empty());
    }

    #[test]
    fn parallel_blocks_are_gone() {
        // `parallel:` was removed, not deprecated. It asked the scheduler to
        // prove race-freedom statically and the proof had holes, so a program
        // could print the wrong answer whenever it lost a race. The block is
        // now a syntax error, and `parallel` is an ordinary identifier.
        assert!(!err("parallel:\n    a = 1\n").is_empty());
        assert!(!err("fn f():\n    x = 1\n    parallel:\n        x = 2\n").is_empty());
        ok("parallel = 1\nprint(parallel)\n");
    }

    #[test]
    fn list_indexing() {
        ok("a = [1, 2]\nprint(a[0])\n");
        assert!(!err("a = 1\nprint(a[0])\n").is_empty());
    }

    fn infer(src: &str) -> HashMap<(String, String), FnInfo> {
        let tokens = nx_lexer::lex(src).unwrap();
        let prog = nx_parser::parse(tokens).unwrap();
        infer_program(&prog, std::path::Path::new(".")).unwrap()
    }

    #[test]
    fn infer_program_for_keys_by_module() {
        // The backend asks once per loaded module: inference for `utils`
        // must file under `utils`, not `__main__`, or method calls on
        // locals inside imported modules miss dispatch.
        let tokens = nx_lexer::lex("fn f(n):\n    return n\n").unwrap();
        let prog = nx_parser::parse(tokens).unwrap();
        let m = infer_program_for(&prog, std::path::Path::new("."), "utils").unwrap();
        assert!(m.contains_key(&("utils".into(), "f".into())));
        assert!(m.contains_key(&("utils".into(), "<top>".into())));
        assert!(!m.keys().any(|(md, _)| md == "__main__"));
    }

    #[test]
    fn param_int_from_arithmetic() {
        let m = infer("fn fib(n):\n    if n <= 1:\n        return n\n    else:\n        return fib(n - 1) + fib(n - 2)\n");
        assert_eq!(m[&("__main__".into(), "fib".into())].locals["n"], Ty::Int);
    }

    #[test]
    fn function_sees_module_globals() {
        // Codegen resolves a top-level name to a global, so the checker has
        // to see it too. It used to empty the enclosing scope and report
        // every module-level constant as undefined inside a function.
        ok("g = 4\nfn f(k):\n    return g + k\nprint(f(1))\n");
        let m = infer("g = 4\nfn f(k):\n    return g + k\n");
        let f = &m[&("__main__".into(), "f".into())];
        assert_eq!(f.locals["g"], Ty::Int);
    }

    #[test]
    fn numeric_param_defaults_to_int_in_an_int_function() {
        // Nothing in the body is a Float, so Int is the only reading left.
        let m = infer("fn f(n):\n    return n * 2\n");
        assert_eq!(m[&("__main__".into(), "f".into())].locals["n"], Ty::Int);
        ok("fn f(n):\n    return n * 2\nprint(f(21))\n");
    }

    #[test]
    fn numeric_param_stays_dynamic_in_a_float_function() {
        // `x / 2.0` is well-typed for an Int x and for a Float x, so the
        // parameter is dynamic. Pinning it made this function reject a
        // Float argument, which is ordinary code failing to compile.
        let m = infer("fn half(x):\n    return x / 2.0\n");
        assert_eq!(m[&("__main__".into(), "half".into())].locals["x"], Ty::Unknown);
        ok("fn half(x):\n    return x / 2.0\nprint(half(1))\n");
        ok("fn half(x):\n    return x / 2.0\nprint(half(1.0))\n");
    }

    #[test]
    fn float_function_takes_float_coordinates() {
        // The regression that motivated the rule: this used to infer
        // (Int, Int, Int) and refuse Float arguments.
        ok("fn mandel(cx, cy, maxiter):\n    zr = 0.0\n    zi = 0.0\n    i = 0\n    while i < maxiter:\n        zr2 = zr * zr\n        zi2 = zi * zi\n        if zr2 + zi2 > 4.0:\n            return i\n        zi = 2.0 * zr * zi + cy\n        zr = zr2 - zi2 + cx\n        i = i + 1\n    return maxiter\nprint(mandel(0.5, 0.5, 10))\n");
    }

    #[test]
    fn comparison_against_float_pins_param_to_float() {
        // An ordering comparison has no widening, so the type is forced.
        let m = infer("fn over(x):\n    if x < 1.5:\n        return 1\n    else:\n        return 0\n");
        assert_eq!(m[&("__main__".into(), "over".into())].locals["x"], Ty::Float);
    }

    #[test]
    fn param_bool_from_condition() {
        let m = infer("fn neg(b):\n    if b:\n        return 1\n    else:\n        return 0\n");
        assert_eq!(m[&("__main__".into(), "neg".into())].locals["b"], Ty::Bool);
    }

    #[test]
    fn param_unpinned_for_unresolved_base() {
        // An unresolved base may hold a list or a dict, so the index is
        // left unresolved: pinning Int here would reject `at(d, "k")`
        // for a dict `d`, which the runtime handles fine.
        let m = infer("fn at(xs, i):\n    return xs[i]\n");
        assert_eq!(m[&("__main__".into(), "at".into())].locals["i"], Ty::Unknown);
    }

    #[test]
    fn param_int_from_known_list_index() {
        // A statically known list still pins its index to Int.
        let m = infer("fn at(i):\n    xs = [1, 2, 3]\n    return xs[i]\n");
        assert_eq!(m[&("__main__".into(), "at".into())].locals["i"], Ty::Int);
    }

    #[test]
    fn unused_param_stays_unknown() {
        let m = infer("fn id(x):\n    return 1\n");
        assert_eq!(m[&("__main__".into(), "id".into())].locals["x"], Ty::Unknown);
    }

    #[test]
    fn inferred_param_rejects_wrong_argument() {
        // x is proven Int by `x - 1`, so passing a string is an error.
        assert!(!err("fn f(x):\n    return x - 1\nprint(f(\"a\"))\n").is_empty());
    }

    #[test]
    fn top_level_scope_is_reported() {
        let m = infer("a = 1\nb = \"s\"\n");
        let top = &m[&("__main__".into(), "<top>".into())];
        assert_eq!(top.locals["a"], Ty::Int);
        assert_eq!(top.locals["b"], Ty::Str);
    }

    #[test]
    fn unknown_assignment_widens_variable() {
        // t is Int from its first binding, but the loop element is
        // untyped, so t must widen: the backend gives a known scalar a
        // typed slot and a narrower type would be unsound.
        let m = infer("fn f(xs):\n    t = 0\n    for x in xs:\n        t = t + x\n    return t\n");
        let f = &m[&("__main__".into(), "f".into())];
        assert_eq!(f.locals["t"], Ty::Unknown);
    }

    #[test]
    fn widening_is_sticky() {
        // Once widened, a later known assignment must not re-narrow.
        let m = infer("fn f(xs):\n    t = 0\n    for x in xs:\n        t = t + x\n    t = 5\n    return t\n");
        assert_eq!(m[&("__main__".into(), "f".into())].locals["t"], Ty::Unknown);
    }

    // --- impl blocks and methods ------------------------------------

    const POINT: &str = "type P:\n    x: Int\n    y: Int\n";

    #[test]
    fn method_return_types_are_inferred() {
        let m = infer(&format!(
            "{POINT}impl P:\n    fn area(self):\n        return self.x * self.y\n    fn zero():\n        return P(0, 0)\n"
        ));
        // Methods report under `Type.method`, matching what codegen looks
        // up, so the two can never disagree about which name a body has.
        assert_eq!(m[&("__main__".into(), "P.area".into())].ret, Ty::Int);
        assert_eq!(m[&("__main__".into(), "P.zero".into())].ret, Ty::Record("P".into()));
    }

    #[test]
    fn mut_self_binds_self_to_the_record() {
        let m = infer(&format!(
            "{POINT}impl P:\n    fn moved(mut self, d):\n        self.x = self.x + d\n        return self\n"
        ));
        let f = &m[&("__main__".into(), "P.moved".into())];
        assert_eq!(f.locals["self"], Ty::Record("P".into()));
        // `d` is Int: the body only ever adds it to an Int field.
        assert_eq!(f.params, vec!["d".to_string()]);
    }

    #[test]
    fn writing_through_read_only_self_is_an_error() {
        // `self` is a copy, so a write through it would be discarded. The
        // compiler says so rather than letting it look meaningful.
        let e = err(&format!(
            "{POINT}impl P:\n    fn bad(self):\n        self.x = 1\n        return self\n"
        ));
        assert!(
            e.iter().any(|m| m.message.contains("read-only 'self'")),
            "expected a read-only self error, got {e:?}"
        );
    }

    #[test]
    fn writing_through_mut_self_is_allowed() {
        ok(&format!(
            "{POINT}impl P:\n    fn ok(mut self):\n        self.x = 1\n        return self\n"
        ));
    }

    #[test]
    fn mut_self_must_return_the_record() {
        // The result is written back into the receiver, so anything other
        // than the record would clobber it with the wrong type.
        let e = err(&format!(
            "{POINT}impl P:\n    fn bad(mut self):\n        self.x = 1\n        return 7\n"
        ));
        assert!(
            e.iter().any(|m| m.message.contains("must return 'P'")),
            "expected a return-type error, got {e:?}"
        );
    }

    #[test]
    fn impl_of_unknown_type_is_an_error() {
        let e = err("impl Nope:\n    fn a(self):\n        return 1\n");
        assert!(
            e.iter().any(|m| m.message.contains("unknown type 'Nope'")),
            "expected an unknown-type error, got {e:?}"
        );
    }

    #[test]
    fn duplicate_method_is_an_error() {
        let e = err(&format!(
            "{POINT}impl P:\n    fn a(self):\n        return 1\n    fn a(self):\n        return 2\n"
        ));
        assert!(
            e.iter().any(|m| m.message.contains("duplicate method")),
            "expected a duplicate error, got {e:?}"
        );
    }

    #[test]
    fn unknown_method_on_a_known_record_is_an_error() {
        let e = err(&format!("{POINT}p = P(1, 2)\nprint(p.nope())\n"));
        assert!(
            e.iter().any(|m| m.message.contains("has no method 'nope'")),
            "expected an unknown-method error, got {e:?}"
        );
    }

    #[test]
    fn method_needs_a_receiver_but_associated_function_does_not() {
        // A method on the type name is a mistake worth naming.
        let e = err(&format!(
            "{POINT}impl P:\n    fn area(self):\n        return self.x\np = P(1, 2)\nprint(P.area())\n"
        ));
        assert!(
            e.iter().any(|m| m.message.contains("needs a receiver")),
            "expected a needs-a-receiver error, got {e:?}"
        );
        // An associated function takes no receiver, so a value of the type
        // is as good a base as the type itself: there is nothing to read
        // off it, and refusing the call would only add a rule.
        ok(&format!(
            "{POINT}impl P:\n    fn zero():\n        return P(0, 0)\np = P(1, 2)\nprint(p.zero())\nprint(P.zero())\n"
        ));
    }

    /// A type may define a method whose name shadows an ambient builtin.
    /// Methods resolve before sugar, so the type's meaning wins.
    #[test]
    fn a_method_may_shadow_a_builtin() {
        ok(&format!(
            "{POINT}impl P:\n    fn push(self, v):\n        return self.x + v\np = P(1, 2)\nprint(p.push(4))\n"
        ));
    }

    #[test]
    fn method_arity_is_checked() {
        let e = err(&format!(
            "{POINT}impl P:\n    fn scaled(self, k):\n        return self.x * k\np = P(1, 2)\nprint(p.scaled())\n"
        ));
        assert!(!e.is_empty(), "expected an arity error, got none");
    }

    /// A method on a type this module imported is an orphan impl: the
    /// layout belongs to another module, so the two could disagree about
    /// what the fields mean.
    #[test]
    fn impl_on_an_imported_type_is_refused() {
        let dir = std::env::temp_dir().join("nx_orphan_impl");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("shapes.nx"), "type Sq:\n    side: Int\n").unwrap();
        let src = dir.join("main.nx");
        std::fs::write(&src, "from shapes import Sq\nimpl Sq:\n    fn area(self):\n        return self.side\n").unwrap();
        let toks = nx_lexer::lex(&std::fs::read_to_string(&src).unwrap()).unwrap();
        let prog = nx_parser::parse(toks).unwrap();
        let e = check_program(&prog, &dir).unwrap_err();
        assert!(
            e.iter().any(|m| m.message.contains("another module")),
            "expected an orphan-impl error, got {e:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `xs.push(1)` is sugar for `push(xs, 1)`, so an element type pinned
    /// by one spelling is pinned by the other -- and a mismatch is caught
    /// through the sugar exactly as it is through the direct call.
    #[test]
    fn builtin_sugar_pins_the_element_type() {
        let m = infer("xs = []\nxs.push(1)\nxs.push(2)\n");
        let top = &m[&("__main__".into(), "<top>".into())];
        assert_eq!(
            top.locals["xs"],
            Ty::List(Box::new(Ty::Int)),
            "the first push pins the element type"
        );
        let e = err("xs = []\nxs.push(1)\nxs.push(\"s\")\n");
        assert!(
            e.iter().any(|m| m.message.contains("element type mismatch")),
            "expected a pinned-element error, got {e:?}"
        );
    }

    #[test]
    fn builtin_sugar_rejects_a_non_list_base() {
        let e = err("x = 5\nx.push(1)\n");
        assert!(
            e.iter().any(|m| m.message.contains("push() needs a list")),
            "expected a push type error, got {e:?}"
        );
    }
}

/// What an assignment target refers to, for error messages. Reads as
/// `variable 'n'` or `element of 'xs'`, which is the wording the
/// diagnostics have always used for a name.
fn target_label(target: &Target) -> String {
    match target {
        Target::Name(n) => format!("variable '{n}'"),
        Target::Index { base, .. } => format!("element of {}", expr_label(base)),
        Target::Attr { base, field } => format!("field '{field}' of {}", expr_label(base)),
    }
}

/// Short human-readable rendering of an expression, for diagnostics only.
fn expr_label(e: &Expr) -> String {
    match e {
        Expr::Var(n, _) => format!("'{n}'"),
        Expr::Call { callee, .. } => match &**callee {
            Expr::Var(n, _) => format!("'{n}'"),
            _ => "expression".to_string(),
        },
        _ => "expression".to_string(),
    }
}
