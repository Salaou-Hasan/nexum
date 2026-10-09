//! Type predicates, field resolution, builtin arity, and diagnostic labels.

use crate::Ty;
use nx_ast::{Expr, Target};
use std::collections::HashMap;

pub(crate) fn compatible(a: &Ty, b: &Ty) -> bool {
    a == b || matches!(a, Ty::Unknown) || matches!(b, Ty::Unknown)
}

/// Types that may key a dict. When the base is unresolved, any of these
/// is accepted: the runtime dispatches on the actual tag, and a scalar
/// key is valid for every dict while an Int is valid for every list.
pub(crate) fn is_keyable(t: &Ty) -> bool {
    matches!(t, Ty::Int | Ty::Float | Ty::Bool | Ty::Str | Ty::Unknown)
}

pub(crate) fn is_numeric(t: &Ty) -> bool {
    matches!(t, Ty::Int | Ty::Float | Ty::Unknown)
}

/// Resolve a field type name as written to a `Ty`, against one module's
/// declarations. Scalars map directly; any other name that matches a
/// declared type becomes that record; anything else stays Unknown here
/// and is reported by `validate_records` at the end of the module.
pub(crate) fn field_ty(tname: &str, records: &HashMap<String, Vec<(String, String)>>) -> Ty {
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

/// What an assignment target refers to, for error messages. Reads as
/// `variable 'n'` or `element of 'xs'`, which is the wording the
/// diagnostics have always used for a name.
pub(crate) fn target_label(target: &Target) -> String {
    match target {
        Target::Name(n) => format!("variable '{n}'"),
        Target::Index { base, .. } => format!("element of {}", expr_label(base)),
        Target::Attr { base, field } => format!("field '{field}' of {}", expr_label(base)),
    }
}

/// Short human-readable rendering of an expression, for diagnostics only.
pub(crate) fn expr_label(e: &Expr) -> String {
    match e {
        Expr::Var(n, _) => format!("'{n}'"),
        Expr::Call { callee, .. } => match &**callee {
            Expr::Var(n, _) => format!("'{n}'"),
            _ => "expression".to_string(),
        },
        _ => "expression".to_string(),
    }
}
