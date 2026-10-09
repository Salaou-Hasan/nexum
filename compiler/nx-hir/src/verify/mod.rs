//! The HIR verifier: every rule in `docs/architecture/hir.md` section 7,
//! as a pass over plain data.
//!
//! HIR nodes are deliberately *representable* when malformed, so this
//! pass is the discipline. Each rule is checkable on hand-built HIR,
//! which is what makes the negative tests (one per rule) possible:
//! if malformed HIR were unrepresentable in Rust, those tests could not
//! exist.
//!
//! The verifier checks structure and consistency, never semantics. It
//! cannot tell a wrong-but-well-formed program from a right one; oracle
//! duty belongs to the expected-value tests. Lowering runs it on its own
//! output, so a lowering bug fails at the lowering site with the rule
//! and span named, rather than three stages later.
//!
//! Layout: this file holds the entry point, the shared context, and the
//! helpers every rule domain needs. The walk lives in [`walk`] (program
//! tables, statements, calls), the expression dispatch in [`expr`], the
//! operator and operand rules in [`rules`], and diagnostic stripping in
//! [`strip`].

mod expr;
mod rules;
mod strip;
mod walk;

pub use strip::strip_diag;

use crate::model::*;

/// One way a program breaks one verifier rule.
#[derive(Debug, Clone, PartialEq)]
pub struct Violation {
    /// The rule this breaks, e.g. `"V3"`.
    pub rule: &'static str,
    pub span: Span,
    pub message: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} at {}:{}: {}",
            self.rule, self.span.line, self.span.col, self.message
        )
    }
}

/// Verify a whole program. Every violation found is reported, not just
/// the first: a hand-built negative test wants the reason it was built,
/// and lowering bugs want the whole story at one span.
pub fn verify(p: &HProgram) -> Result<(), Vec<Violation>> {
    let mut v = Verifier { p, out: Vec::new() };
    v.tables();
    v.functions();
    if v.out.is_empty() {
        Ok(())
    } else {
        Err(v.out)
    }
}

/// Per-function state: which slots are bound so far, what the function
/// returns, and how deeply loops are nested (V1, V8, V9).
pub(crate) struct FnCtx {
    pub(crate) bound: Vec<bool>,
    pub(crate) ret: HTy,
    pub(crate) loop_depth: usize,
}

impl FnCtx {
    pub(crate) fn bind(&mut self, s: Slot) {
        let i = s.0 as usize;
        if self.bound.len() <= i {
            self.bound.resize(i + 1, false);
        }
        self.bound[i] = true;
    }

    pub(crate) fn unbind(&mut self, s: Slot) {
        if let Some(slot) = self.bound.get_mut(s.0 as usize) {
            *slot = false;
        }
    }

    pub(crate) fn is_bound(&self, s: Slot) -> bool {
        self.bound.get(s.0 as usize).copied().unwrap_or(false)
    }
}

pub(crate) struct Verifier<'a> {
    pub(crate) p: &'a HProgram,
    pub(crate) out: Vec<Violation>,
}

impl<'a> Verifier<'a> {
    pub(crate) fn err(&mut self, rule: &'static str, span: Span, message: impl Into<String>) {
        self.out.push(Violation {
            rule,
            span,
            message: message.into(),
        });
    }

    pub(crate) fn module(&self, m: ModuleId) -> Option<&HModule> {
        self.p.modules.get(m.0 as usize)
    }

    pub(crate) fn func(&self, f: FuncId) -> Option<&HFunc> {
        self.p.funcs.get(f.0 as usize)
    }

    pub(crate) fn method(&self, m: MethodId) -> Option<&HMethod> {
        self.p.methods.get(m.0 as usize)
    }

    pub(crate) fn record(&self, t: TypeId) -> Option<&HType> {
        self.p.types.get(t.0 as usize)
    }

    pub(crate) fn fields_of(&self, t: &HTy) -> Option<&[HTy]> {
        match t {
            HTy::Record(id) => Some(&self.record(*id)?.fields),
            _ => None,
        }
    }

    pub(crate) fn str_in_range(&mut self, s: StrId, span: Span) {
        if s.0 as usize >= self.p.strings.len() {
            self.err(
                "V2",
                span,
                format!("string id {} with {} interned", s.0, self.p.strings.len()),
            );
        }
    }
}

/// A method's original spelling for a message, or its ID when the
/// diagnostic sidecar was stripped.
pub(crate) fn label(d: &DiagInfo, id: MethodId) -> String {
    d.name.clone().unwrap_or_else(|| format!("#{}", id.0))
}

pub(crate) fn name_of(rule: EqRule) -> &'static str {
    match rule {
        EqRule::Numeric => "numeric",
        EqRule::StrEq => "string",
        EqRule::Structural => "structural",
        EqRule::IdentityNone => "none-identity",
        EqRule::Dynamic => "dynamic",
    }
}

/// Lattice compatibility, mirroring the checker's `compatible`.
pub(crate) fn compatible(a: &HTy, b: &HTy) -> bool {
    a == b || matches!(a, HTy::Unknown) || matches!(b, HTy::Unknown)
}

/// A dict key type, mirroring the checker's `is_keyable`.
pub(crate) fn keyable(t: &HTy) -> bool {
    matches!(
        t,
        HTy::Int | HTy::Float | HTy::Bool | HTy::Str | HTy::Unknown
    )
}
