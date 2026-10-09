//! Type conversion and operator-rule decisions for lowering.

use super::tables::Tables;
use super::{lerr, LResult};
use crate::model::*;
use nx_ast::{BinOp, Span};
use nx_types::Ty;

// ---------------------------------------------------------------------------
// Shared type conversion and per-function lowering. Free functions over
// `&Tables` so the assembler above and body lowering below use one
// implementation.
// ---------------------------------------------------------------------------

/// A checker type to its HIR twin, resolving nominal references in
/// `module`. Function and module values never appear on value nodes
/// (both are rejected in value position), so they fail loudly here
/// rather than mistyping.
pub(crate) fn conv_ty(tables: &Tables, ty: &Ty, module: &str, span: Span) -> LResult<HTy> {
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
pub(crate) fn conv_ty_str(tables: &Tables, tname: &str, module: &str) -> LResult<HTy> {
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
pub(crate) fn resolve_type_name(tables: &Tables, module: &str, name: &str) -> Option<TypeId> {
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
pub(crate) fn canonical_name(tables: &Tables, module: &str, name: &str) -> String {
    tables
        .type_alias
        .get(&(module.to_string(), name.to_string()))
        .map(|(_, canon)| canon.clone())
        .unwrap_or_else(|| name.to_string())
}

/// Lattice predicates over [`HTy`], mirroring `nx_types::compatible`
/// and `is_numeric`. The shapes match; the types differ, so these live
/// beside their uses rather than across crates.
pub(crate) fn hty_compatible(a: &HTy, b: &HTy) -> bool {
    a == b || matches!(a, HTy::Unknown) || matches!(b, HTy::Unknown)
}

pub(crate) fn hty_numeric(t: &HTy) -> bool {
    matches!(t, HTy::Int | HTy::Float | HTy::Unknown)
}

/// Copy discipline from a value type (grammar §4.2).
pub(crate) fn copy_rule(ty: &HTy) -> CopyRule {
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
pub(crate) fn decide_bin_rule(op: BinOp, l: &HTy, r: &HTy, span: Span) -> LResult<BinRule> {
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

/// The ambient builtin a name spells (`Len` only for the integer
/// queries: every name the arity table accepts has its arm here, and
/// the verifier's arity table agrees -- a missing arm would mistype,
/// not misbehave).
pub(crate) fn builtin_op(name: &str) -> BuiltinOp {
    match name {
        "push" => BuiltinOp::Push,
        "input" => BuiltinOp::Input,
        "int" => BuiltinOp::ToInt,
        "float" => BuiltinOp::ToFloat,
        _ => BuiltinOp::Len,
    }
}

/// Result type of a builtin: the checker's answers, one per op.
pub(crate) fn builtin_ret(op: BuiltinOp) -> HTy {
    match op {
        BuiltinOp::Len => HTy::Int,
        BuiltinOp::Push => HTy::None,
        BuiltinOp::Input => HTy::Str,
        BuiltinOp::ToInt => HTy::Int,
        BuiltinOp::ToFloat => HTy::Float,
    }
}
