//! Compiled values: raw-vs-boxed representation and scalar helpers.

use nx_types::Ty;

/// A compiled value. Two facts, deliberately kept apart:
/// - `raw` is the *physical* form: Some(t) means `reg` holds a bare scalar
///   of type t, None means it holds a boxed `%NxVal`.
/// - `ty` is the *static* type, which is what picks an operator. A boxed
///   value can still have a known type (a module global, a call result, a
///   list), and then arithmetic on it can skip the tag dispatch even
///   though the register itself is a box.
#[derive(Debug, Clone)]
pub(crate) struct NV {
    pub(crate) reg: String,
    pub(crate) raw: Option<Ty>,
    pub(crate) ty: Ty,
    /// The literal this register holds, when it came straight from source.
    /// Shifts need it: a shift distance outside 0..63 is a runtime panic,
    /// and only a constant lets the unboxed path skip that check.
    pub(crate) const_i: Option<i64>,
    /// Uniquely owned storage: freshly allocated by this expression, with
    /// no other binding referencing it. Storing a fresh value needs no
    /// `nx_clone` -- there is nothing to separate from. Anything that may
    /// alias (loads, calls, reads of stored containers) is not fresh.
    pub(crate) fresh: bool,
}

impl NV {
    /// A bare scalar already sitting in a register.
    pub(crate) fn raw(t: Ty, reg: String) -> NV {
        NV {
            reg,
            raw: Some(t.clone()),
            ty: t,
            const_i: None,
            fresh: false,
        }
    }
    /// A box whose dynamic type the backend does not know.
    pub(crate) fn dyn_boxed(reg: String) -> NV {
        NV {
            reg,
            raw: None,
            ty: Ty::Unknown,
            const_i: None,
            fresh: false,
        }
    }
    /// A box whose static type is known: usable unboxed where the caller
    /// needs the payload, but still physically a `%NxVal`.
    pub(crate) fn boxed_known(reg: String, ty: Ty) -> NV {
        NV {
            reg,
            raw: None,
            ty,
            const_i: None,
            fresh: false,
        }
    }
    /// A bare Int whose value is known at compile time.
    pub(crate) fn raw_const(t: Ty, reg: String, value: i64) -> NV {
        NV {
            reg,
            raw: Some(t.clone()),
            ty: t,
            const_i: Some(value),
            fresh: false,
        }
    }
    /// A freshly allocated box: uniquely owned, so storing it clones
    /// nothing. Only for values this expression itself created --
    /// literals, runtime constructors, and operators that allocate.
    pub(crate) fn fresh_boxed(reg: String, ty: Ty) -> NV {
        NV {
            reg,
            raw: None,
            ty,
            const_i: None,
            fresh: true,
        }
    }
}

/// LLVM type holding a scalar of this NX type, or None if it stays boxed.
pub(crate) fn ll_scalar(t: &Ty) -> Option<&'static str> {
    match t {
        Ty::Int => Some("i64"),
        Ty::Float => Some("double"),
        Ty::Bool => Some("i1"),
        _ => None,
    }
}

/// LLVM's exact float literal form: the raw bit pattern, so no decimal
/// rounding can creep in between the parser and the instruction.
pub(crate) fn fmt_double(x: f64) -> String {
    format!("0x{:016X}", x.to_bits())
}
