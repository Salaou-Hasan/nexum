//! The emission table is a pure function of the recorded rule.
//!
//! ARGONE D, fifth box: integer overflow lives in HIR as
//! `ArithRule::Trap`, and the backend must honor the rule rather than
//! re-deriving the decision from operand types. `arith_plan` takes no
//! operand types at all -- there is no parameter to consult -- so a
//! recomputation inside the table is unrepresentable, not merely
//! untested. This file pins the table itself: every (rule, operator)
//! pair maps to exactly one emission, and the three trapping operators
//! map to the checked intrinsics.
//!
//! The other half of the proof lives in `nx-codegen`: the Trap proof
//! test there lowers real programs, counts the `Trap` nodes HIR
//! recorded, and asserts the compiled output carries exactly that many
//! checked intrinsics. If the backend ever recomputed the decision
//! (plain `add`, unchecked helper), the counts diverge and it fails.

use nx_ast::BinOp;
use nx_hir::{arith_plan, ArithPlan, ArithRule, BinRule, PowRule};

#[test]
fn trapping_int_arithmetic_selects_the_checked_intrinsics() {
    use ArithPlan::Checked;
    assert_eq!(arith_plan(BinRule::Arith(ArithRule::Trap), BinOp::Add), Checked("llvm.sadd.with.overflow.i64"));
    assert_eq!(arith_plan(BinRule::Arith(ArithRule::Trap), BinOp::Sub), Checked("llvm.ssub.with.overflow.i64"));
    assert_eq!(arith_plan(BinRule::Arith(ArithRule::Trap), BinOp::Mul), Checked("llvm.smul.with.overflow.i64"));
}

#[test]
fn trapping_division_and_remainder_keep_their_panicking_helpers() {
    // There is no checked intrinsic for division: the zero-divisor
    // panic lives in the runtime helper, exactly as the backend emits
    // today.
    use ArithPlan::IntCall;
    assert_eq!(arith_plan(BinRule::Arith(ArithRule::Trap), BinOp::Div), IntCall("nx_div_i64"));
    assert_eq!(arith_plan(BinRule::Arith(ArithRule::Trap), BinOp::FloorDiv), IntCall("nx_floordiv_i64"));
    assert_eq!(arith_plan(BinRule::Arith(ArithRule::Trap), BinOp::Mod), IntCall("nx_mod_i64"));
}

#[test]
fn float_rules_agree_on_the_instruction_regardless_of_promotion() {
    // `Float` and `PromoteFloat` emit the same instruction: the
    // promotion itself happens when the operands are coerced, which is
    // the caller's job. The rule records *why* it is float, not a
    // second decision about *what* to emit.
    use ArithPlan::{FloatCall, FloatMnem};
    for rule in [ArithRule::Float, ArithRule::PromoteFloat] {
        let rule = BinRule::Arith(rule);
        assert_eq!(arith_plan(rule, BinOp::Add), FloatMnem("fadd"));
        assert_eq!(arith_plan(rule, BinOp::Sub), FloatMnem("fsub"));
        assert_eq!(arith_plan(rule, BinOp::Mul), FloatMnem("fmul"));
        assert_eq!(arith_plan(rule, BinOp::Div), FloatCall("nx_fdiv"));
        assert_eq!(arith_plan(rule, BinOp::Pow), FloatCall("nx_fpow"));
    }
}

#[test]
fn saturating_power_calls_the_saturating_helper() {
    // R2: no intrinsic saturates, so integer power cannot inline.
    assert_eq!(
        arith_plan(BinRule::Pow(PowRule::Saturate), BinOp::Pow),
        ArithPlan::IntCall("nx_ipow")
    );
}

#[test]
fn bitwise_rules_select_raw_integer_instructions() {
    use ArithPlan::Bitwise;
    assert_eq!(arith_plan(BinRule::Bitwise, BinOp::BitAnd), Bitwise("and"));
    assert_eq!(arith_plan(BinRule::Bitwise, BinOp::BitOr), Bitwise("or"));
    assert_eq!(arith_plan(BinRule::Bitwise, BinOp::BitXor), Bitwise("xor"));
}

#[test]
fn dynamic_rules_have_no_unboxed_form() {
    // At least one operand is statically unknown, so there is no single
    // instruction to emit. The caller dispatches on tags instead.
    for rule in [
        BinRule::Arith(ArithRule::Dynamic),
        BinRule::Pow(PowRule::Dynamic),
        BinRule::Dynamic,
    ] {
        for op in [BinOp::Add, BinOp::Sub, BinOp::Mul, BinOp::Div, BinOp::Pow] {
            assert_eq!(arith_plan(rule, op), ArithPlan::Dispatch, "{rule:?} on {op:?}");
        }
    }
}

#[test]
fn mismatched_rule_operator_pairs_dispatch_rather_than_miscompile() {
    // Unreachable through checked code (the checker rejects `7 % 2.0`
    // before lowering decides anything), but a malformed node must not
    // take the compiler down mid-emission.
    assert_eq!(arith_plan(BinRule::Arith(ArithRule::Float), BinOp::Mod), ArithPlan::Dispatch);
    assert_eq!(arith_plan(BinRule::Arith(ArithRule::Trap), BinOp::Pow), ArithPlan::Dispatch);
    assert_eq!(arith_plan(BinRule::Bitwise, BinOp::Add), ArithPlan::Dispatch);
    assert_eq!(arith_plan(BinRule::Concat, BinOp::Add), ArithPlan::Dispatch);
}
