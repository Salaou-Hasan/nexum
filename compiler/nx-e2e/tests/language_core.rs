//! Language core: operators, literals, control flow, scoping.
//!
//! The tests here assert on *values*, not on the fact that a program ran.
//! Which of `//` and `%` takes the sign of which operand, what `-2 ** 2`
//! parses to, whether a `for` variable outlives its loop -- each has one
//! correct answer, and getting one wrong is a bug that a smoke test passes
//! happily.
//!
//! Every expectation was taken from the implementation and then checked
//! against `docs/grammar.md`. Where the two disagree the test follows the
//! specification's *stated* rule and the disagreement is reported rather
//! than pinned here.

use nx_e2e::*;

// ---------------------------------------------------------------------------
// 1. Integer and float arithmetic
// ---------------------------------------------------------------------------

#[test]
fn integer_add_sub_mul_are_exact() {
    assert_output(
        r#"
print(1 + 2, 5 - 3, 3 * 4)
"#,
        &["3 2 12"],
    );
}

#[test]
fn integer_division_and_floor_division_both_floor() {
    // `/` on two Ints agrees with `//` on all four sign combinations.
    // Truncation toward zero would give 3 for -7/2 and -3 for 7/-2 is only
    // right by accident; -7 // 3 is the case a truncating implementation
    // gets wrong while looking entirely plausible.
    assert_output(
        r#"
print(7 / 2, -7 / 2, 7 / -2, -7 / -2)
print(7 // 3, -7 // 3, 7 // -3, -7 // -3)
"#,
        &["3 -3 -3 3", "2 -3 -3 2"],
    );
}

#[test]
fn integer_remainder_takes_the_sign_of_the_divisor() {
    // Python's convention and the opposite of C's: the result has the
    // divisor's sign, so `a == (a // b) * b + a % b` holds for every sign
    // combination. The two edges are here because they are where a
    // "sign of the dividend" implementation differs most visibly.
    assert_output(
        r#"
print(7 % 3, -7 % 3, 7 % -3, -7 % -3)
print(-1 % 5, 5 % 5)
"#,
        &["1 2 -2 -1", "4 0"],
    );
}

#[test]
fn integer_power_raises_to_an_exact_power() {
    assert_output(
        r#"
print(2 ** 10, 0 ** 0, 0 ** 3, (-1) ** 3)
"#,
        &["1024 1 0 -1"],
    );
}

#[test]
fn an_int_operand_promotes_to_float() {
    // Promotion is visible in the value, not in how it is spelled: an Int
    // promoted by a Float operand prints as an integral float, so "3" and
    // not "3.0".
    assert_output(
        r#"
print(1 + 2.0, 6.0 - 3.0, 2.0 * 2.0)
print(2.0 + 2, 3 / 2.0, 7.0 / 2.0, 7.0 / 4.0)
"#,
        &["3 3 4", "4 1.5 3.5 1.75"],
    );
}

#[test]
fn floats_print_to_fifteen_significant_digits() {
    assert_output(
        r#"
print(1.0 / 3.0)
print(2.0 / 3.0)
"#,
        &["0.333333333333333", "0.666666666666667"],
    );
}

#[test]
fn float_arithmetic_rounds_away_binary_noise() {
    // 10.8 + 20.1 is 30.899999999999998 in binary floating point. Printing
    // must show 30.9, and doubling it 61.8 -- not the representation error.
    assert_output(
        r#"
print(10.8 + 20.1)
x = 10.8 + 20.1
print(x * 2)
print(0.1 + 0.2)
"#,
        &["30.9", "61.8", "0.3"],
    );
}

#[test]
fn division_by_zero_is_a_runtime_error() {
    assert_runtime_error("print(1 / 0)", "division by zero");
    assert_runtime_error(
        r#"
z = 0
print(1 / z)
"#,
        "division by zero",
    );
}

#[test]
fn modulo_by_zero_is_a_runtime_error() {
    assert_runtime_error("print(1 % 0)", "modulo by zero");
}

#[test]
fn an_integer_literal_too_large_for_i64_is_rejected() {
    // Range is caught where the literal is read, not left to become a
    // silently wrapped constant.
    assert_rejected("print(99999999999999999999)", "invalid integer");
}

// ---------------------------------------------------------------------------
// 2. Bitwise operators
// ---------------------------------------------------------------------------

#[test]
fn bitwise_operators_on_integers() {
    assert_output(
        r#"
print(6 & 3, 6 | 3, 6 ^ 3)
print(255 & 15, 240 | 15, 255 ^ 15)
print(~6, ~0, ~-1)
"#,
        &["2 7 5", "15 255 240", "-7 -1 0"],
    );
}

#[test]
fn left_shift_moves_bits_up() {
    assert_output(
        r#"
print(1 << 10, 1 << 62, 1 << 63, 0 << 63)
"#,
        &["1024 4611686018427387904 -9223372036854775808 0"],
    );
}

#[test]
fn right_shift_of_a_negative_keeps_the_sign() {
    // Arithmetic, not logical: -8 is ...11111000, so shifting right by one
    // gives -4, not 9223372036854775804.
    assert_output(
        r#"
print(-8 >> 1, -1 >> 63, -16 >> 2)
"#,
        &["-4 -1 -4"],
    );
}

#[test]
fn a_shift_distance_outside_the_word_fails_at_runtime() {
    // The distance is a count of bits in a 64-bit word, so 63 is the
    // largest legal one and 64 is a runtime failure in both directions.
    assert_runtime_error("print(1 << 64)", "shift distance out of range");
    assert_runtime_error("print(1 >> 64)", "shift distance out of range");
}

#[test]
fn a_negative_shift_distance_fails_at_runtime() {
    assert_runtime_error(
        r#"
n = -1
print(1 << n)
"#,
        "shift distance out of range",
    );
}

#[test]
fn a_shift_needs_integers_on_both_sides() {
    // The distance is a count rather than a value of the shifted type, so
    // it is checked on its own.
    assert_rejected("print(1 << 1.0)", "shift distance must be Int");
    assert_rejected("print(1.0 << 1)", "needs Int on the left");
}

// ---------------------------------------------------------------------------
// 3. Operator precedence
// ---------------------------------------------------------------------------

#[test]
fn multiplication_binds_tighter_than_addition() {
    assert_output(
        r#"
print(1 + 2 * 3, 2 + 3 * 4 - 1, 2 * 3 ** 2)
"#,
        &["7 13 18"],
    );
}

#[test]
fn power_binds_tighter_than_prefix_minus() {
    // `-2 ** 2` is -(2 ** 2), not (-2) ** 2. The base is a postfix
    // expression and the exponent is a full unary expression, so the
    // prefix operator is the outer one.
    assert_output(
        r#"
print(-2 ** 2, 2 ** 3, (-2) ** 3)
"#,
        &["-4 8 -8"],
    );
}

#[test]
fn not_binds_looser_than_comparison() {
    // `not a in b` reads as `not (a in b)`. At the unary level it would be
    // `(not a) in b`, which cannot type-check at all.
    assert_output(
        r#"
a = 1
xs = [1, 2]
print(not a in xs)
print(not 9 in xs)
print(not 1 == 2)
"#,
        &["false", "true", "true"],
    );
}

#[test]
fn comparison_chains_parse_left_associatively() {
    // `a < b < c` is `(a < b) < c`, not Python's chained form. The tell is
    // that the left operand of the second `<` is a Bool, which cannot be
    // ordered -- so a chain is refused rather than quietly meaning
    // something else.
    assert_rejected("print(1 < 2 < 3)", "cannot order Bool and Int");

    // The same level, left-associative, yielding a Bool.
    assert_output(
        r#"
print(1 < 2 == true, 1 > 2 == false, 3 == 3 != false)
"#,
        &["true true true"],
    );
}

#[test]
fn bitwise_and_binds_tighter_than_xor_and_or() {
    // `&` 7, `^` 8, `|` 9 in the precedence table -- so `1 | 2 ^ 3` is
    // `1 | (2 ^ 3)` and is 1, not 3. All three are looser than `+` at 5.
    assert_output(
        r#"
print(1 | 2 & 3, 1 ^ 3 & 1, 1 | 2 ^ 3, 2 << 1 + 1, 8 >> 1 + 1)
"#,
        &["3 0 1 8 2"],
    );
}

// ---------------------------------------------------------------------------
// 4. Number literals
// ---------------------------------------------------------------------------

#[test]
fn digit_separators_are_stripped() {
    assert_output(
        r#"
print(1_000_000, 1_0, 10_00)
"#,
        &["1000000 10 1000"],
    );
}

#[test]
fn radix_literals_decode_at_either_case() {
    // Radix literals are decoded to decimal at lexing time, so every
    // consumer downstream sees a plain digit string.
    assert_output(
        r#"
print(0xFF, 0X10, 0xdeadBEEF, 0xFF_FF)
print(0o17, 0O7, 0b1011, 0b1010)
"#,
        &["255 16 3735928559 65535", "15 7 11 10"],
    );
}

#[test]
fn exponent_notation_makes_floats() {
    assert_output(
        r#"
print(1e3, 2.5e-3, 1E2, 1.5e2)
"#,
        &["1000 0.0025 100 150"],
    );
}

// ---------------------------------------------------------------------------
// 5. Control flow
// ---------------------------------------------------------------------------

#[test]
fn if_else_runs_exactly_one_branch() {
    assert_output(
        r#"
fn classify(n):
    if n > 3:
        return "big"
    elif n == 3:
        return "three"
    elif n > 1:
        return "mid"
    else:
        return "small"

print(classify(5), classify(3), classify(2), classify(0))
"#,
        &["big three mid small"],
    );
}

#[test]
fn while_repeats_until_its_condition_flips() {
    assert_output(
        r#"
i = 0
while i < 3:
    print(i)
    i = i + 1
print("done")
"#,
        &["0", "1", "2", "done"],
    );
}

#[test]
fn for_over_a_range_visits_every_int_in_it() {
    assert_output(
        r#"
for i in 0..5:
    print(i)
print("done")
"#,
        &["0", "1", "2", "3", "4", "done"],
    );
}

#[test]
fn for_over_a_list_yields_its_elements() {
    assert_output(
        r#"
xs = ["a", "b", "c"]
for x in xs:
    print(x)
"#,
        &["a", "b", "c"],
    );
}

#[test]
fn break_leaves_the_loop_early() {
    // The loop variable is still live at the point of the break, and the
    // iteration it stopped on does not run its body.
    assert_output(
        r#"
for i in 0..5:
    if i == 3:
        print("at", i)
        break
    print(i)
print("after")

j = 0
while true:
    if j == 2:
        break
    print(j)
    j = j + 1
"#,
        &["0", "1", "2", "at 3", "after", "0", "1"],
    );
}

#[test]
fn continue_skips_the_rest_of_the_body_in_a_while_loop() {
    // `continue` jumps to the condition, so nothing after it in the body
    // runs. The loop's own increment has to come before it or the loop
    // never terminates -- the same rule Python has.
    assert_output(
        r#"
i = 0
while i < 4:
    if i == 1:
        i = i + 1
        continue
    print(i)
    i = i + 1
print("after")
"#,
        &["0", "2", "3", "after"],
    );
}

#[test]
fn ternary_evaluates_only_the_branch_it_picks() {
    // The untaken branch is never evaluated, so the `1 / 0` below is not a
    // division by zero.
    assert_output(
        r#"
print(1 if true else 1 / 0)
print(1 / 0 if false else 2)
n = 5
print("big" if n > 3 else "small")
"#,
        &["1", "2", "big"],
    );
}

// ---------------------------------------------------------------------------
// 6. Functions and recursion
// ---------------------------------------------------------------------------

#[test]
fn a_function_returns_what_it_returns() {
    assert_output(
        r#"
fn add(a, b):
    return a + b

print(add(20, 22))
"#,
        &["42"],
    );
}

#[test]
fn a_function_that_returns_nothing_gives_none() {
    // Both ways of not returning a value: an explicit bare `return`, and
    // running off the end of the body.
    assert_output(
        r#"
fn bare():
    return

fn falls_off():
    x = 1

print(bare(), falls_off())
"#,
        &["none none"],
    );
}

#[test]
fn recursion_computes_a_factorial() {
    assert_output(
        r#"
fn fact(n):
    if n <= 1:
        return 1
    return n * fact(n - 1)

print(fact(5), fact(10), fact(0))
"#,
        &["120 3628800 1"],
    );
}

// ---------------------------------------------------------------------------
// 7. Ranges
// ---------------------------------------------------------------------------

#[test]
fn a_range_is_a_half_open_list_of_ints() {
    assert_output(
        r#"
print(0..5, 0..1, 1..3, len(0..5))
"#,
        &["[0, 1, 2, 3, 4] [0] [1, 2] 5"],
    );
}

#[test]
fn a_range_that_covers_nothing_is_empty() {
    // Equal bounds and inverted bounds are both empty lists rather than
    // errors. Iterating one as an ordinary list therefore runs zero times.
    assert_output(
        r#"
xs = 5..1
print(xs, len(xs))
n = 0
for x in xs:
    n = n + 1
print(n)
print(3..3, len(3..3))
"#,
        &["[] 0", "0", "[] 0"],
    );
}

// ---------------------------------------------------------------------------
// 8. Comprehensions
// ---------------------------------------------------------------------------

#[test]
fn a_comprehension_variable_does_not_escape() {
    // The variable belongs to the comprehension. Reading it afterwards is
    // a type error, not a leftover binding.
    //
    // Contrast the `for` loop below: there the checker leaves the name in
    // scope and the refusal comes from the backend instead.
    assert_rejected(
        r#"
xs = [i * i for i in 0..5]
print(xs)
print(i)
"#,
        "undefined variable 'i'",
    );
}

#[test]
fn a_comprehension_filter_keeps_only_matching_elements() {
    assert_output(
        r#"
print([i * i for i in 0..5])
print([i for i in 0..10 if i % 2 == 0])
xs = [1, 2, 3]
print([x * 10 for x in xs])
print([x for x in xs if x > 1])
"#,
        &["[0, 1, 4, 9, 16]", "[0, 2, 4, 6, 8]", "[10, 20, 30]", "[2, 3]"],
    );
}

// ---------------------------------------------------------------------------
// 9. Booleans and None
// ---------------------------------------------------------------------------

#[test]
fn and_or_not_produce_bools_not_operands() {
    assert_output(
        r#"
print(true and false, true or false, not true)
print(true and true, false or true, not false)
"#,
        &["false true false", "true true true"],
    );
}

#[test]
fn logical_operators_take_bool_operands_only() {
    // If `and` handed back an operand, `1 == 1 and 2` would have an answer.
    // It does not, and `not` will not coerce either.
    assert_rejected(
        "print(1 == 1 and 2)",
        "'right' operand of 'and' must be Bool",
    );
    assert_rejected("print(not 1)", "operator 'not' not supported for Int");
}

#[test]
fn none_prints_as_none_and_equals_itself() {
    assert_output(
        r#"
x = None
print(x)
print(None == None)
print(x == None)
print(None != None)
"#,
        &["none", "true", "true", "false"],
    );
}

#[test]
fn a_non_bool_condition_is_a_type_error() {
    // There is no truthiness: a condition must already be a Bool. The
    // same holds for `while`.
    assert_rejected(
        r#"
if 1:
    print(1)
"#,
        "condition must be Bool, found Int",
    );
    assert_rejected(
        r#"
while 1:
    print(1)
"#,
        "condition must be Bool, found Int",
    );
}

// ---------------------------------------------------------------------------
// 10. assert
// ---------------------------------------------------------------------------

#[test]
fn a_holding_assert_lets_the_program_continue() {
    assert_output(
        r#"
assert 1 == 1
assert 2 * 3 == 6, "arithmetic is broken"
print("ok")
"#,
        &["ok"],
    );
}

#[test]
fn a_failing_assert_stops_the_program() {
    assert_runtime_error("assert 1 == 2, \"boom\"", "boom: assertion failed");
    assert_runtime_error("assert 1 == 2", "assertion failed");
}

#[test]
fn assert_of_a_non_bool_is_a_type_error() {
    // `assert 1` is not a truthiness test; it is a Bool with the wrong
    // type, caught before anything runs.
    assert_rejected("assert 1", "assert condition must be Bool, found Int");
    assert_rejected("assert \"a\"", "assert condition must be Bool, found Str");
}

// ---------------------------------------------------------------------------
// 11. Loop variable scoping
// ---------------------------------------------------------------------------

#[test]
fn a_for_variable_does_not_escape_the_loop() {
    // The variable is scoped to its loop, so reading `i` afterwards must
    // fail rather than hand back the last value.
    //
    // Note the stage: the type checker's scope table keeps the name, so the
    // refusal comes from the backend and `run` reports exit 102 rather
    // than a type error. `check` alone accepts this program.
    let o = run(
        r#"
for i in 0..3:
    print(i)
print(i)
"#,
    );
    assert_eq!(o.code, 102, "expected a compile failure, got {:?}", o.out);
    assert!(
        o.out.contains("undefined variable 'i'"),
        "unexpected diagnostic: {:?}",
        o.out
    );
}

#[test]
fn a_for_variable_does_not_escape_a_loop_inside_a_function() {
    // The same rule on the function's local scope rather than the module's
    // top level -- a different binding path, so it is checked separately.
    let o = run(
        r#"
fn f():
    for i in 0..3:
        print(i)
    print(i)

f()
"#,
    );
    assert_eq!(o.code, 102, "expected a compile failure, got {:?}", o.out);
    assert!(
        o.out.contains("undefined variable 'i'"),
        "unexpected diagnostic: {:?}",
        o.out
    );
}

#[test]
fn an_inner_loop_restores_the_outer_induction_variable() {
    // After the inner loop, `j` is gone and `i` still holds the outer
    // loop's current value -- not the inner loop's last one.
    assert_output(
        r#"
for i in 0..2:
    for j in 0..2:
        print(i, j)
    print("outer", i)
"#,
        &["0 0", "0 1", "outer 0", "1 0", "1 1", "outer 1"],
    );
}

#[test]
fn an_inner_loop_restores_the_outer_variable_inside_a_function() {
    // A function body is a different binding path from the module top
    // level, so it is checked separately.
    assert_output(
        r#"
fn f():
    for i in 0..3:
        for j in 0..2:
            print(i, j)
        print("outer", i)

f()
"#,
        &[
            "0 0", "0 1", "outer 0", "1 0", "1 1", "outer 1", "2 0", "2 1", "outer 2",
        ],
    );
}

#[test]
fn an_inner_loop_reusing_the_name_restores_the_outer_value() {
    // The hardest case: the inner `for i` must not destroy the outer `i`.
    // If it does, "out" prints 1 after the first outer iteration.
    assert_output(
        r#"
for i in 0..3:
    for i in 0..2:
        print("in", i)
    print("out", i)
"#,
        &[
            "in 0", "in 1", "out 0", "in 0", "in 1", "out 1", "in 0", "in 1", "out 2",
        ],
    );
}

#[test]
fn a_loop_variable_is_restored_to_the_enclosing_binding() {
    // A pre-existing `i` is shadowed by the loop and comes back afterwards,
    // so the loop does not silently overwrite the enclosing variable -- and
    // the restore happens on the `break` path too, not only the normal one.
    assert_output(
        r#"
i = 99
for i in 0..3:
    print(i)
print(i)
"#,
        &["0", "1", "2", "99"],
    );
    assert_output(
        r#"
i = 99
for i in 0..3:
    print(i)
    break
print(i)
"#,
        &["0", "99"],
    );
}

// ---------------------------------------------------------------------------
// 12. The integral-only operators refuse floats
// ---------------------------------------------------------------------------

#[test]
fn floor_division_and_modulo_do_not_widen_to_float() {
    // These never widen to Float: `7 // 2.0` is a mistake worth reporting
    // rather than papering over.
    assert_rejected(
        "print(7 // 2.0)",
        "operator '//' not supported for Int and Float",
    );
    assert_rejected(
        "print(7.5 % 2.0)",
        "operator '%' not supported for Float and Float",
    );
}
  // ---------------------------------------------------------------------------
  // 13. R1 and R2: integer arithmetic has a defined answer or none at all
  // ---------------------------------------------------------------------------
  //
  // Both rules used to hold only on the boxed path. The default build unboxes,
  // and the unboxed helpers inherited neither rule, so an ordinary program got
  // a silently wrapped number instead of an answer. Every case below is the
  // default build.

  #[test]
  fn integer_arithmetic_that_does_not_fit_traps() {
      assert_runtime_error("print(9223372036854775807 + 1)", "integer overflow");
      assert_runtime_error("print(-9223372036854775807 - 2)", "integer overflow");
      assert_runtime_error("print(9223372036854775807 * 2)", "integer overflow");
      assert_runtime_error("print(-9223372036854775807 * 2)", "integer overflow");
  }

  #[test]
  fn the_minimum_divided_by_minus_one_traps() {
      // sdiv i64 INT64_MIN, -1 is poison rather than a wrapped value: the
      // quotient does not exist. All three division spellings must refuse it,
      // because each one reaches a different helper.
      let min = "x = -9223372036854775807 - 1\n";
      assert_runtime_error(&format!("{min}print(x / -1)"), "integer overflow");
      assert_runtime_error(&format!("{min}print(x // -1)"), "integer overflow");
      assert_runtime_error(&format!("{min}print(x % -1)"), "integer overflow");
  }

  #[test]
  fn arithmetic_that_fits_is_unaffected() {
      assert_output("print(2 + 3, 7 - 9, 6 * 7)", &["5 -2 42"]);
      assert_output("print(7 // 2, -7 // 2, 7 // -2, -7 // -2)", &["3 -4 -4 3"]);
      assert_output("print(7 % 3, -7 % 3, 7 % -3, -7 % -3)", &["1 2 -2 -1"]);
      // Float printing trims to 15 significant digits with no trailing `.0`,
      // so 4.0 prints as `4`. That is the formatting examples/methods.nx and
      // examples/records.nx already depend on; 4.0 and 4 printing alike is a
      // real wart, but changing it is a formatting decision, not an
      // arithmetic one.
      assert_output("print(1.5 + 2.5, 1.5 * 2.0)", &["4 3"]);
  }

  #[test]
  fn a_bounded_accumulator_never_trips_the_overflow_check() {
      // The check has to be free when it cannot fire. Summing a range and
      // running a countdown both stay far inside i64 and must not trap.
      assert_output(
          "t = 0\nfor i in 1..100001:\n    t = t + i\nprint(t)",
          &["5000050000"],
      );
      assert_output(
          "n = 100000\nt = 0\nwhile n > 0:\n    t = t + n\n    n = n - 1\nprint(t)",
          &["5000050000"],
      );
  }

  #[test]
  fn an_integer_power_saturates_rather_than_wrapping() {
      assert_one("print(2 ** 63)", "9223372036854775807");
      assert_one("print(2 ** 100)", "9223372036854775807");
      assert_one("print((-2) ** 63)", "-9223372036854775808");
      assert_one("print(3 ** 62)", "5069619362125685561");
  }

  #[test]
  fn an_integer_power_answers_the_exact_cases_instead_of_saturating() {
      // 0, 1 and -1 never grow, so clamping them would be a lie.
      assert_one("print(0 ** 100)", "0");
      assert_one("print(1 ** 100)", "1");
      assert_one("print((-1) ** 100)", "1");
      assert_one("print((-1) ** 101)", "-1");
      assert_one("print(2 ** 0)", "1");
      assert_one("print(0 ** 0)", "1");
  }

  #[test]
  fn a_negative_exponent_on_ints_is_rejected() {
      assert_runtime_error("print(2 ** -1)", "negative exponent");
      // The Float path asks a different question and still answers it.
      assert_one("print(2.0 ** -1)", "0.5");
      assert_one("print(2.0 ** 10)", "1024");
  }