//! Arithmetic: scalar fast paths and rule-driven emission.

use crate::core::Gen;
use crate::value::NV;
use crate::CodegenError;
use nx_ast::BinOp;
use nx_hir::ArithRule::{Float, PromoteFloat, Trap};
use nx_hir::{arith_plan, ArithPlan, BinRule, BinRule as Birule, PowRule};
use nx_types::Ty;

impl Gen {
    /// Apply a binary operator, taking the unboxed instruction path when
    /// both operands are proven scalars and the boxed helper otherwise.
    pub(crate) fn binop_dyn(&mut self, l: &NV, op: BinOp, r: &NV) -> Result<NV, CodegenError> {
        if let Some(v) = self.emit_scalar_binop(l, op, r) {
            return Ok(v);
        }
        let lb = self.unbox(l);
        let rb = self.unbox(r);
        let out = self.reg();
        match op {
            BinOp::Add => self.w(&format!(
                "  {out} = call %NxVal @nx_add(%NxVal {lb}, %NxVal {rb})"
            )),
            BinOp::Sub => self.w(&format!(
                "  {out} = call %NxVal @nx_sub(%NxVal {lb}, %NxVal {rb})"
            )),
            BinOp::Mul => self.w(&format!(
                "  {out} = call %NxVal @nx_mul(%NxVal {lb}, %NxVal {rb})"
            )),
            BinOp::Div => self.w(&format!(
                "  {out} = call %NxVal @nx_div(%NxVal {lb}, %NxVal {rb})"
            )),
            BinOp::Mod => self.w(&format!(
                "  {out} = call %NxVal @nx_mod(%NxVal {lb}, %NxVal {rb})"
            )),
            BinOp::FloorDiv => self.w(&format!(
                "  {out} = call %NxVal @nx_floordiv(%NxVal {lb}, %NxVal {rb})"
            )),
            BinOp::Pow => self.w(&format!(
                "  {out} = call %NxVal @nx_pow(%NxVal {lb}, %NxVal {rb})"
            )),
            BinOp::BitAnd => self.w(&format!(
                "  {out} = call %NxVal @nx_bitand(%NxVal {lb}, %NxVal {rb})"
            )),
            BinOp::BitOr => self.w(&format!(
                "  {out} = call %NxVal @nx_bitor(%NxVal {lb}, %NxVal {rb})"
            )),
            BinOp::BitXor => self.w(&format!(
                "  {out} = call %NxVal @nx_bitxor(%NxVal {lb}, %NxVal {rb})"
            )),
            BinOp::Shl => self.w(&format!(
                "  {out} = call %NxVal @nx_shl(%NxVal {lb}, %NxVal {rb})"
            )),
            BinOp::Shr => self.w(&format!(
                "  {out} = call %NxVal @nx_shr(%NxVal {lb}, %NxVal {rb})"
            )),
            BinOp::In => {
                let c = self.reg();
                self.w(&format!(
                    "  {c} = call %NxVal @nx_in(%NxVal {lb}, %NxVal {rb})"
                ));
                return Ok(NV::fresh_boxed(c, Ty::Bool));
            }
            BinOp::NotIn => {
                let c = self.reg();
                let n = self.reg();
                self.w(&format!(
                    "  {c} = call %NxVal @nx_in(%NxVal {lb}, %NxVal {rb})"
                ));
                self.w(&format!("  {n} = call %NxVal @nx_not(%NxVal {c})"));
                return Ok(NV::fresh_boxed(n, Ty::Bool));
            }
            BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                let v = self.emit_cmp_dyn(l, op, r)?;
                return Ok(v);
            }
            BinOp::And | BinOp::Or => unreachable!("handled by emit_logic"),
        }
        let ty = match op {
            BinOp::Mod
            | BinOp::FloorDiv
            | BinOp::Pow
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::Shl
            | BinOp::Shr => Ty::Int,
            _ => Ty::Unknown,
        };
        // Every helper above allocates its result box, so it is fresh.
        Ok(NV::fresh_boxed(out, ty))
    }

    pub(crate) fn emit_cmp_dyn(&mut self, l: &NV, op: BinOp, r: &NV) -> Result<NV, CodegenError> {
        let lb = self.unbox(l);
        let rb = self.unbox(r);
        let out = self.reg();
        match op {
            BinOp::Eq => self.w(&format!(
                "  {out} = call %NxVal @nx_eq(%NxVal {lb}, %NxVal {rb})"
            )),
            BinOp::NotEq => {
                let c = self.reg();
                self.w(&format!(
                    "  {c} = call %NxVal @nx_eq(%NxVal {lb}, %NxVal {rb})"
                ));
                self.w(&format!("  {out} = call %NxVal @nx_not(%NxVal {c})"));
            }
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                let pred = match op {
                    BinOp::Lt => "slt",
                    BinOp::LtEq => "sle",
                    BinOp::Gt => "sgt",
                    _ => "sge",
                };
                let c = self.reg();
                let b = self.reg();
                self.w(&format!(
                    "  {c} = call i32 @nx_cmp(%NxVal {lb}, %NxVal {rb})"
                ));
                self.w(&format!("  {b} = icmp {pred} i32 {c}, 0"));
                self.w(&format!("  {out} = call %NxVal @nx_bool(i1 {b})"));
            }
            _ => unreachable!(),
        }
        // `nx_bool` allocates its result, like every other helper here.
        Ok(NV::fresh_boxed(out, Ty::Bool))
    }

    /// One checked i64 operation, leaving the result in `out`.
    ///
    /// `intrin` is an llvm.*.with.overflow intrinsic, which returns the value
    /// and the overflow flag from a single operation -- so the check costs
    /// nothing the arithmetic did not already cost, and LLVM folds the whole
    /// thing away whenever it can prove the range. That covers most loop
    /// counters and index arithmetic, which is where a checked add would
    /// otherwise cost the most.
    ///
    /// The failing path is a separate block for the same reason the assertion
    /// path is: an operation that does not overflow costs one never-taken
    /// branch and nothing else.
    pub(crate) fn emit_i64_checked(&mut self, intrin: &str, a: &str, b: &str, out: &str) {
        let pair = self.reg();
        let flag = self.reg();
        let bad = self.lab("ovf");
        let ok = self.lab("ovf_ok");
        let done = self.lab("ovf_done");
        self.w(&format!(
            "  {pair} = call {{ i64, i1 }} @{intrin}(i64 {a}, i64 {b})"
        ));
        self.w(&format!("  {out} = extractvalue {{ i64, i1 }} {pair}, 0"));
        self.w(&format!("  {flag} = extractvalue {{ i64, i1 }} {pair}, 1"));
        self.w(&format!("  br i1 {flag}, label %{bad}, label %{ok}"));
        self.block(&format!("{ok}"));
        self.w(&format!("  br label %{done}"));
        self.block(&format!("{bad}"));
        self.w("  call void @nx_panic(ptr @.msg.overflow)");
        self.w("  unreachable");
        self.block(&format!("{done}"));
    }

    /// Emit one binary arithmetic or bitwise operation from the rule
    /// HIR recorded, never from the operand types.
    ///
    /// `arith_plan` is the only thing consulted here: this function
    /// reads no `ty` field of either operand, so a rule and a type that
    /// disagree are resolved in the rule's favour by construction. That
    /// is the property the Trap proof test asserts, and it is why the
    /// arm-selection table lives beside the rule rather than beside the
    /// types.
    pub(crate) fn emit_arith(&mut self, rule: BinRule, op: BinOp, a: &str, b: &str, ty: Ty) -> NV {
        let out = self.reg();
        match arith_plan(rule, op) {
            // R1 (docs/grammar.md 3.1.1): an Int result that does not fit
            // in i64 traps rather than wrapping. Checked inline because
            // that is where the operands are still proven scalars --
            // boxing them to reach a helper would undo the unboxing this
            // whole path exists for.
            ArithPlan::Checked(intrin) => self.emit_i64_checked(intrin, a, b, &out),
            ArithPlan::FloatMnem(mnem) => self.w(&format!("  {out} = {mnem} double {a}, {b}")),
            ArithPlan::IntCall(helper) => {
                self.w(&format!("  {out} = call i64 @{helper}(i64 {a}, i64 {b})"))
            }
            ArithPlan::FloatCall(helper) => self.w(&format!(
                "  {out} = call double @{helper}(double {a}, double {b})"
            )),
            ArithPlan::Bitwise(mnem) => self.w(&format!("  {out} = {mnem} i64 {a}, {b}")),
            // The rule is dynamic, so this node has no unboxed form. The
            // caller checks the plan before getting here (the scalar path
            // only asks for rules it has proven), so reaching this is a
            // lowering bug rather than a dynamic program.
            ArithPlan::Dispatch => unreachable!("emit_arith asked for a dynamic rule"),
        }
        NV::raw(ty, out)
    }

    /// Arithmetic and comparison on two proven scalars, straight to LLVM
    /// instructions with no box in between. Returns None when either
    /// operand is not statically known, so the caller falls back to the
    /// boxed helpers.
    pub(crate) fn emit_scalar_binop(&mut self, l: &NV, op: BinOp, r: &NV) -> Option<NV> {
        // Normalize both sides to raw scalars first. A boxed global or call
        // result of known type becomes a bare register here, so the
        // instruction below is the same whether or not a box was involved.
        let l = self.as_raw(l)?;
        let r = self.as_raw(r)?;
        if !(l.ty.is_scalar() && r.ty.is_scalar()) {
            return None;
        }
        // Result type: mixed Int/Float promotes to Float, like the runtime.
        let numeric = l.ty != Ty::Bool && r.ty != Ty::Bool;
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                if !numeric {
                    return None;
                }
                let float = l.ty == Ty::Float || r.ty == Ty::Float;
                let ty = if float { Ty::Float } else { Ty::Int };
                let a = self.coerce(&l, &ty);
                let b = self.coerce(&r, &ty);
                // The one place a rule is derived from types today, while
                // the backend still consumes the AST. Task G moves this to
                // read the rule HIR already recorded; the emitter below is
                // already written against the rule alone, so that change
                // is one line here and no change to `emit_arith`.
                let rule = match (op, float) {
                    (BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div, false) => {
                        Birule::Arith(Trap)
                    }
                    (_, true) => {
                        if l.ty == Ty::Float && r.ty == Ty::Float {
                            Birule::Arith(Float)
                        } else {
                            Birule::Arith(PromoteFloat)
                        }
                    }
                    _ => return None,
                };
                Some(self.emit_arith(rule, op, &a, &b, ty))
            }
            BinOp::Eq | BinOp::NotEq => {
                if l.ty != r.ty {
                    return None;
                }
                let b = match l.ty {
                    Ty::Int => {
                        let c = self.reg();
                        self.w(&format!("  {c} = icmp eq i64 {}, {}", l.reg, r.reg));
                        c
                    }
                    Ty::Float => {
                        let c = self.reg();
                        self.w(&format!("  {c} = fcmp oeq double {}, {}", l.reg, r.reg));
                        c
                    }
                    _ => {
                        let c = self.reg();
                        self.w(&format!("  {c} = icmp eq i1 {}, {}", l.reg, r.reg));
                        c
                    }
                };
                let out = if op == BinOp::NotEq {
                    let n = self.reg();
                    self.w(&format!("  {n} = xor i1 {b}, true"));
                    n
                } else {
                    b
                };
                Some(NV::raw(Ty::Bool, out))
            }
            BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq => {
                if !numeric {
                    return None;
                }
                let float = l.ty == Ty::Float || r.ty == Ty::Float;
                let ty = if float { Ty::Float } else { Ty::Int };
                let a = self.coerce(&l, &ty);
                let b = self.coerce(&r, &ty);
                let out = self.reg();
                if float {
                    let pred = match op {
                        BinOp::Lt => "olt",
                        BinOp::LtEq => "ole",
                        BinOp::Gt => "ogt",
                        _ => "oge",
                    };
                    self.w(&format!("  {out} = fcmp {pred} double {a}, {b}"));
                } else {
                    let pred = match op {
                        BinOp::Lt => "slt",
                        BinOp::LtEq => "sle",
                        BinOp::Gt => "sgt",
                        _ => "sge",
                    };
                    self.w(&format!("  {out} = icmp {pred} i64 {a}, {b}"));
                }
                Some(NV::raw(Ty::Bool, out))
            }
            // The integral-only operators never promote to Float: `7 % 2.0` has no
            // agreed answer, and the checker rejects it before it gets
            // here, so a Float operand means "fall back to the boxed path".
            BinOp::Mod | BinOp::FloorDiv | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                if !numeric || l.ty != Ty::Int || r.ty != Ty::Int {
                    return None;
                }
                // Integral-only: no promotion, so the rule is Trap for the
                // dividing pair (the runtime helpers own the zero-divisor
                // panic) and Bitwise for the rest. Same derivation site as
                // above, same rule-only emitter.
                let rule = match op {
                    BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => Birule::Bitwise,
                    _ => Birule::Arith(Trap),
                };
                Some(self.emit_arith(rule, op, &l.reg, &r.reg, Ty::Int))
            }
            // A shift distance outside 0..63 is a runtime panic, which the raw
            // instruction cannot express. The distance is only checked here
            // when it is a constant; otherwise the boxed helper does it,
            // which keeps the common `1 << n` case unboxed and correct.
            BinOp::Shl | BinOp::Shr => {
                if l.ty != Ty::Int || r.ty != Ty::Int {
                    return None;
                }
                let distance = Self::const_int(&r)?;
                if !(0..64).contains(&distance) {
                    return None;
                }
                // Shifts are integral-only with no overflow question, so
                // the rule is Bitwise; the literal distance (not a
                // register) is why this arm builds its own line rather
                // than going through `emit_arith`'s register pair.
                let out = self.reg();
                let ins = if op == BinOp::Shl { "shl" } else { "ashr" };
                self.w(&format!("  {out} = {ins} i64 {}, {distance}", l.reg));
                Some(NV::raw(Ty::Int, out))
            }
            BinOp::Pow => {
                if !numeric {
                    return None;
                }
                let float = l.ty == Ty::Float || r.ty == Ty::Float;
                let ty = if float { Ty::Float } else { Ty::Int };
                let a = self.coerce(&l, &ty);
                let b = self.coerce(&r, &ty);
                // R2: integer power saturates, so its rule is
                // Pow(Saturate); float power has no such question. A
                // negative exponent on an Int base and an oversized one
                // are both checked in @nx_ipow.
                let rule = if float {
                    Birule::Arith(Float)
                } else {
                    Birule::Pow(PowRule::Saturate)
                };
                Some(self.emit_arith(rule, op, &a, &b, ty))
            }
            BinOp::In | BinOp::NotIn => {
                // Membership is about containers, so it never has an
                // unboxed form.
                None
            }
            BinOp::And | BinOp::Or => None,
        }
    }

    /// `emit_scalar_binop` for the compound-assignment operators, which
    /// the checker has already restricted to arithmetic.
    pub(crate) fn emit_named_binop(&mut self, l: &NV, op: BinOp, r: &NV) -> Option<NV> {
        match op {
            // Arithmetic, the integral-only set, and `**` all have an
            // unboxed form. Membership has none: it is about containers.
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Mod
            | BinOp::FloorDiv
            | BinOp::Pow
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::Shl
            | BinOp::Shr => self.emit_scalar_binop(l, op, r),
            _ => None,
        }
    }
}
