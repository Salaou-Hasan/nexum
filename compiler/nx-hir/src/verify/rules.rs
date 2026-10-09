//! Operand and operator rules: field layout (V5), index and slice
//! operands (V10), range bounds (V11), and the arithmetic, equality,
//! ordering, and unary rule tables (V3).

use nx_ast::UnaryOp;

use crate::model::*;

use super::keyable;
use super::name_of;
use super::FnCtx;
use super::Verifier;

impl<'a> Verifier<'a> {
    /// V5: a static field index is inside the base record's layout.
    pub(crate) fn field(&mut self, base: &HExpr, f: FieldRef, span: Span) {
        match (f, &base.ty) {
            (FieldRef::Static(i), HTy::Record(tid)) => match self.record(*tid) {
                None => self.err("V2", span, format!("missing record type {}", tid.0)),
                Some(t) => {
                    if i.0 >= t.fields.len() {
                        self.err(
                            "V5",
                            span,
                            format!("field {} of a {}-field record", i.0, t.fields.len()),
                        );
                    }
                }
            },
            // Only a statically unknown base keeps a runtime name, and
            // the name must be interned.
            (FieldRef::Dynamic(s), HTy::Unknown) => self.str_in_range(s, span),
            (FieldRef::Dynamic(s), other) => {
                self.str_in_range(s, span);
                self.err(
                    "V5",
                    span,
                    format!("dynamic field name on a known {}", other),
                );
            }
            (FieldRef::Static(_), other) => {
                self.err("V5", span, format!("field offset on a {}", other));
            }
        }
    }

    /// V10: index and slice operands.
    pub(crate) fn index(&mut self, base: &HExpr, index: &HExpr, rule: IndexRule, span: Span) {
        match rule {
            IndexRule::ListInt | IndexRule::StrChar => {
                if !matches!(index.ty, HTy::Int | HTy::Unknown) {
                    self.err("V10", span, format!("index is {} not Int", index.ty));
                }
            }
            IndexRule::DictKey => {
                if !keyable(&index.ty) {
                    self.err("V10", span, format!("dict key is {}", index.ty));
                }
            }
            IndexRule::Dynamic => {}
        }
        match (rule, &base.ty) {
            (IndexRule::ListInt, HTy::List(_)) | (IndexRule::ListInt, HTy::Unknown) => {}
            (IndexRule::StrChar, HTy::Str) | (IndexRule::StrChar, HTy::Unknown) => {}
            (IndexRule::DictKey, HTy::Dict(_)) | (IndexRule::DictKey, HTy::Unknown) => {}
            (IndexRule::Dynamic, _) => {}
            _ => self.err("V2", span, format!("index rule does not fit {}", base.ty)),
        }
    }

    pub(crate) fn int_bound(&mut self, b: &HExpr, span: Span) {
        if !matches!(b.ty, HTy::Int | HTy::Unknown) {
            self.err("V11", span, format!("bound is {} not Int", b.ty));
        }
    }

    /// V3: an arithmetic rule matches the operand types it was decided
    /// from, and the node carries the resulting type.
    pub(crate) fn bin_rule(&mut self, rule: BinRule, l: &HTy, r: &HTy, span: Span) {
        match rule {
            BinRule::Arith(ArithRule::Trap) => {
                if !matches!((l, r), (HTy::Int, HTy::Int)) {
                    self.err(
                        "V3",
                        span,
                        format!("trapping arithmetic on {} and {}", l, r),
                    );
                }
            }
            BinRule::Arith(ArithRule::Float) => {
                if !matches!((l, r), (HTy::Float, HTy::Float)) {
                    self.err("V3", span, format!("float arithmetic on {} and {}", l, r));
                }
            }
            BinRule::Arith(ArithRule::PromoteFloat) => {
                let ok = matches!((l, r), (HTy::Int, HTy::Float) | (HTy::Float, HTy::Int));
                if !ok {
                    self.err("V3", span, format!("float promotion on {} and {}", l, r));
                }
            }
            BinRule::Arith(ArithRule::Dynamic) => {
                if !(matches!(l, HTy::Unknown) || matches!(r, HTy::Unknown)) {
                    self.err("V3", span, "dynamic arithmetic on two known operands");
                }
            }
            BinRule::Pow(PowRule::Saturate) => {
                if !matches!((l, r), (HTy::Int, HTy::Int)) {
                    self.err("V3", span, "saturating power on non-Int operands");
                }
            }
            BinRule::Pow(PowRule::Dynamic) | BinRule::Dynamic => {
                if !(matches!(l, HTy::Unknown) || matches!(r, HTy::Unknown)) {
                    self.err("V3", span, "dynamic rule on two known operands");
                }
            }
            BinRule::Bitwise => {
                if !matches!((l, r), (HTy::Int, HTy::Int)) {
                    self.err("V3", span, "bitwise operator on non-Int operands");
                }
            }
            BinRule::Concat => {
                if !matches!((l, r), (HTy::Str, HTy::Str)) {
                    self.err("V3", span, "concatenation on non-Str operands");
                }
            }
        }
    }

    pub(crate) fn eq_rule(&mut self, rule: EqRule, l: &HTy, r: &HTy, span: Span) {
        let numeric = |t: &HTy| matches!(t, HTy::Int | HTy::Float | HTy::Unknown);
        let ok = match rule {
            EqRule::Numeric => numeric(l) && numeric(r),
            EqRule::StrEq => {
                matches!(l, HTy::Str | HTy::Unknown) && matches!(r, HTy::Str | HTy::Unknown)
            }
            EqRule::Structural => {
                matches!(l, HTy::List(_) | HTy::Dict(_) | HTy::Record(_))
                    && matches!(r, HTy::List(_) | HTy::Dict(_) | HTy::Record(_))
            }
            EqRule::IdentityNone => matches!(l, HTy::None) && matches!(r, HTy::None),
            EqRule::Dynamic => matches!(l, HTy::Unknown) || matches!(r, HTy::Unknown),
        };
        if !ok {
            self.err(
                "V3",
                span,
                format!("{} equality on {} and {}", name_of(rule), l, r),
            );
        }
    }

    pub(crate) fn cmp_rule(&mut self, rule: CmpRule, l: &HTy, r: &HTy, span: Span) {
        let numeric = |t: &HTy| matches!(t, HTy::Int | HTy::Float | HTy::Unknown);
        let ok = match rule {
            CmpRule::Numeric => numeric(l) && numeric(r),
            CmpRule::StrOrder => {
                matches!(l, HTy::Str | HTy::Unknown) && matches!(r, HTy::Str | HTy::Unknown)
            }
            CmpRule::Dynamic => matches!(l, HTy::Unknown) || matches!(r, HTy::Unknown),
        };
        if !ok {
            self.err("V3", span, format!("ordering on {} and {}", l, r));
        }
    }

    pub(crate) fn unary_rule(&mut self, rule: UnaryRule, op: UnaryOp, operand: &HExpr, e: &HExpr) {
        let span = e.span;
        let ok = match rule {
            UnaryRule::Neg(ArithRule::Trap) => matches!(op, UnaryOp::Neg) && operand.ty == HTy::Int,
            UnaryRule::Neg(ArithRule::Float) => {
                matches!(op, UnaryOp::Neg) && operand.ty == HTy::Float
            }
            UnaryRule::Neg(ArithRule::PromoteFloat) | UnaryRule::Neg(ArithRule::Dynamic) => {
                matches!(op, UnaryOp::Neg)
            }
            UnaryRule::Not => {
                matches!(op, UnaryOp::Not) && matches!(operand.ty, HTy::Bool | HTy::Unknown)
            }
            UnaryRule::BitNot => {
                matches!(op, UnaryOp::BitNot) && matches!(operand.ty, HTy::Int | HTy::Unknown)
            }
            UnaryRule::Pos => {
                matches!(op, UnaryOp::Pos)
                    && matches!(operand.ty, HTy::Int | HTy::Float | HTy::Unknown)
            }
            UnaryRule::Dynamic => operand.ty == HTy::Unknown,
        };
        if !ok {
            self.err("V3", span, format!("{rule:?} on {}", operand.ty));
        }
    }

    pub(crate) fn exact(&mut self, e: &HExpr, ctx: &mut FnCtx, want: HTy) {
        let _ = ctx;
        if e.ty != want {
            self.err(
                "V3",
                e.span,
                format!("node is {} but must be {}", e.ty, want),
            );
        }
    }

    /// A list-typed node whose element type is fixed.
    pub(crate) fn exact_elem(&mut self, e: &HExpr, want: HTy) {
        if e.ty != HTy::List(Box::new(want.clone())) {
            self.err(
                "V3",
                e.span,
                format!("node is {} but must be a list of {}", e.ty, want),
            );
        }
    }
}
