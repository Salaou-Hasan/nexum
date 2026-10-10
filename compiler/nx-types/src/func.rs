//! Function, call, and method checking with inference.

use crate::pred::compatible;
use crate::{CheckError, Ty};
use nx_ast::{Expr, Span, Stmt};
use std::collections::HashMap;

use crate::checker::Checker;

impl Checker {
    /// Check call arguments against parameter types: exact arity, then
    /// per-argument compatibility with parameter narrowing. Shared by
    /// plain calls, module calls, and method calls so all three agree.
    pub(crate) fn check_call_args(&mut self, params: &[Ty], args: &[Expr], span: Span) {
        if params.len() != args.len() {
            self.err(
                span,
                format!("expects {} args, got {}", params.len(), args.len()),
            );
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
    pub(crate) fn check_builtin(&mut self, name: &str, args: &[Expr], span: Span) -> Ty {
        match name {
            "len" => {
                if args.len() != 1 {
                    self.err(span, "len() expects 1 argument".to_string());
                    return Ty::Unknown;
                }
                let t = self.check_expr(&args[0]);
                if !matches!(t, Ty::List(_) | Ty::Str | Ty::Dict(_) | Ty::Unknown) {
                    self.err(
                        span,
                        format!("len() only supports lists, strings and dicts, found {t}"),
                    );
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
                    self.err(
                        span,
                        "push() first argument must be a list variable".to_string(),
                    );
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
                // prints the prompt first. The prompt prints the way
                // `print` prints a value, so Str, Int and Float all work;
                // The answer is always a Str: `int(input())` parses one.
                if args.len() > 1 {
                    self.err(
                        span,
                        format!("input() expects at most 1 argument, got {}", args.len()),
                    );
                    return Ty::Unknown;
                }
                if let Some(p) = args.first() {
                    let pt = self.check_expr(p);
                    if !matches!(pt, Ty::Str | Ty::Int | Ty::Float | Ty::Unknown) {
                        self.err(
                            span,
                            format!("input() prompt must be Str, Int or Float, found {pt}"),
                        );
                    }
                }
                Ty::Str
            }
            "int" => {
                // `int(x)` converts to Int. Int is the identity; Float
                // truncates toward zero (what the backend's fptosi does);
                // Str must be integer syntax (optional sign, digits) and
                // is parsed at runtime, so a bad string is a runtime
                // error, not a type error. Bool has no numeric reading
                // worth blessing, so it is rejected outright.
                if args.len() != 1 {
                    self.err(
                        span,
                        format!("int() expects 1 argument, got {}", args.len()),
                    );
                    return Ty::Unknown;
                }
                let t = self.check_expr(&args[0]);
                if !matches!(t, Ty::Int | Ty::Float | Ty::Str | Ty::Unknown) {
                    self.err(span, format!("int() needs Int, Float or Str, found {t}"));
                }
                Ty::Int
            }
            "float" => {
                // `float(x)` converts to Float. Int widens; Float is the
                // identity; Str must be decimal syntax and is parsed at
                // runtime. Anything else is rejected like `int`.
                if args.len() != 1 {
                    self.err(
                        span,
                        format!("float() expects 1 argument, got {}", args.len()),
                    );
                    return Ty::Unknown;
                }
                let t = self.check_expr(&args[0]);
                if !matches!(t, Ty::Int | Ty::Float | Ty::Str | Ty::Unknown) {
                    self.err(span, format!("float() needs Int, Float or Str, found {t}"));
                }
                Ty::Float
            }
            "solve" => {
                // `solve(A, b)` solves A * x = b for x. A is a square
                // matrix List(List(N)); b is a vector List(N) or a
                // matrix List(List(N)) with a numeric N. Elimination
                // divides, so x always comes back Float shaped like b.
                // No numeric defaulting here, for the same reason as
                // `@`: matrix parameters must stay dynamic.
                if args.len() != 2 {
                    self.err(
                        span,
                        format!("solve() expects 2 arguments, got {}", args.len()),
                    );
                    return Ty::Unknown;
                }
                let a_ty = self.check_expr(&args[0]);
                let b_ty = self.check_expr(&args[1]);
                if matches!(a_ty, Ty::Unknown) || matches!(b_ty, Ty::Unknown) {
                    return Ty::Unknown;
                }
                let a_good = match &a_ty {
                    Ty::List(rows) => match &**rows {
                        Ty::List(cells) => {
                            matches!(**cells, Ty::Int | Ty::Float | Ty::Unknown)
                        }
                        _ => false,
                    },
                    _ => false,
                };
                if !a_good {
                    self.err(
                        span,
                        "solve() needs A to be a matrix (List(List(N))) with a numeric element type".to_string(),
                    );
                    return Ty::Unknown;
                }
                match &b_ty {
                    Ty::List(rows) => match &**rows {
                        Ty::List(cells) if matches!(**cells, Ty::Int | Ty::Float | Ty::Unknown) => {
                            Ty::List(Box::new(Ty::List(Box::new(Ty::Float))))
                        }
                        Ty::Int | Ty::Float | Ty::Unknown => Ty::List(Box::new(Ty::Float)),
                        _ => {
                            self.err(
                                span,
                                "solve() needs b to be a vector or matrix with a numeric element type".to_string(),
                            );
                            Ty::Unknown
                        }
                    },
                    _ => {
                        self.err(
                            span,
                            "solve() needs b to be a vector or matrix with a numeric element type"
                                .to_string(),
                        );
                        Ty::Unknown
                    }
                }
            }
            _ => {
                self.err(span, format!("unknown builtin '{name}'"));
                Ty::Unknown
            }
        }
    }

    /// The canonical (declared) name for a possibly-aliased type. Unknown
    /// names pass through unchanged; the caller reports them.
    pub(crate) fn canonical_name(&self, name: &str) -> String {
        self.type_alias
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    /// Check constructor arguments against a record layout: exact arity,
    /// then per-field types. Positional by design -- a partial constructor
    /// would need a notion of an unset field that the value model does
    /// not have.
    pub(crate) fn check_record_args(
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
    pub(crate) fn check_fn_like(
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
        let param_tys: Vec<Ty> = params
            .iter()
            .map(|p| fn_locals.get(p).cloned().unwrap_or(Ty::Unknown))
            .collect();
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
}
