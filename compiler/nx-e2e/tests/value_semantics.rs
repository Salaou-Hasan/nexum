//! Value semantics and mutation.
//!
//! These are the tests that matter most in the language. A value-semantics
//! language that silently becomes a reference language still prints plausible
//! numbers in most programs; the damage shows up later and elsewhere.

use nx_e2e::*;

#[test]
fn arguments_do_not_alias() {
    // The single most valuable invariant in the language: passing a container
    // copies it, for all three container kinds, independently.
    assert_output(
        "fn f(xs):
    xs[0] = 99
    return xs[0]
a = [1, 2]
print(f(a))
print(a)",
        &["99", "[1, 2]"],
    );
    assert_output(
        "type P:
    x: Int
fn bump(p):
    p.x = 99
    return p.x
a = P(1)
print(bump(a))
print(a)",
        &["99", "P(1)"],
    );
    assert_output(
        "fn f(d):
    d[\"k\"] = 99
    return d[\"k\"]
a = {\"k\": 1}
print(f(a))
print(a)",
        &["99", "{k: 1}"],
    );
}

#[test]
fn assignment_copies_lists_dicts_and_records() {
    assert_output(
        "xs = [1, 2]
ys = xs
xs[0] = 9
print(xs)
print(ys)",
        &["[9, 2]", "[1, 2]"],
    );
    assert_output(
        "d = {\"a\": 1}
e = d
d[\"a\"] = 9
print(d)
print(e)",
        &["{a: 9}", "{a: 1}"],
    );
    assert_output(
        "type P:
    x: Int
    y: Int
p = P(1, 2)
q = p
p.x = 9
print(p)
print(q)",
        &["P(9, 2)", "P(1, 2)"],
    );
    // Nested, because a shallow copy that stops one level deep is the exact
    // bug a shallow implementation produces.
    assert_output(
        "xs = [[1, 2]]
ys = xs
xs[0][0] = 9
print(xs)
print(ys)",
        &["[[9, 2]]", "[[1, 2]]"],
    );
}

#[test]
fn indexed_and_nested_assignment() {
    assert_output(
        "xs = [1, 2, 3]
xs[0] = 10
xs[2] += 5
xs[-1] = 7
print(xs)",
        &["[10, 2, 7]"],
    );
    assert_output(
        "g = [[1, 2], [3, 4]]
g[1][0] = 7
print(g)",
        &["[[1, 2], [7, 4]]"],
    );
}

#[test]
fn multiple_assignment_swaps() {
    // Every right-hand side is evaluated before any store.
    assert_output(
        "a = 1
b = 2
a, b = b, a
print(a)
print(b)",
        &["2", "1"],
    );
}

#[test]
fn multiple_element_targets_assign_positionally() {
    // `xs[0], xs[1] = 7, 8` used to be rejected: the parser only
    // accepted one target before the comma. Right-hand sides still
    // evaluate before any store, so this swaps too.
    assert_output(
        "xs = [1, 2, 3]
xs[0], xs[2] = xs[2], xs[0]
print(xs)",
        &["[3, 2, 1]"],
    );
    assert_output(
        "xs = [0, 0]
a = 9
a, xs[1] = xs[0], a
print(a, xs)",
        &["0 [0, 9]"],
    );
}

#[test]
fn multiple_field_targets_assign_positionally() {
    assert_output(
        "type P:
    x: Int
    y: Int
p = P(1, 2)
p.x, p.y = p.y, p.x
print(p)",
        &["P(2, 1)"],
    );
}

#[test]
fn multiple_return_is_destructured() {
    assert_output(
        "fn pair():
    return 1, 2
x, y = pair()
print(x)
print(y)",
        &["1", "2"],
    );
    // The checker does not check destructuring arity; the runtime rejects it.
    assert_runtime_error(
        "fn one():
    return 5
x, y = one()
print(x)",
        "type mismatch",
    );
}

#[test]
fn compound_assignment_covers_every_operator() {
    assert_output(
        "n = 10
n += 5
n -= 3
n *= 2
n //= 7
n %= 3
n **= 2
n &= 6
n |= 9
n ^= 5
n <<= 2
n >>= 1
print(n)",
        &["24"],
    );
}

#[test]
fn record_field_write_and_delete() {
    assert_output(
        "type P:
    x: Int
    y: Int
p = P(1, 2)
p.x = 9
p.x += 5
print(p)",
        &["P(14, 2)"],
    );
    // del on a field blanks it; a record's arity is fixed.
    assert_output(
        "type P:
    x: Int
    y: Int
p = P(1, 2)
del p.x
print(p)",
        &["P(none, 2)"],
    );
}

#[test]
fn record_equality_is_structural_but_nominal_across_types() {
    assert_output(
        "type P:
    x: Int
    y: Int
print(P(1, 2) == P(1, 2))
print(P(1, 2) == P(1, 3))",
        &["true", "false"],
    );
    // Two structurally identical but nominally distinct types are not
    // comparable at all: the checker refuses before any runtime question.
    assert_rejected(
        "type P:
    x: Int
    y: Int
type Q:
    x: Int
    y: Int
print(P(1, 2) == Q(1, 2))",
        "cannot compare",
    );
}

#[test]
fn dict_equality_is_structural_and_order_insensitive() {
    assert_output(
        "print({\"a\": 1, \"b\": 2} == {\"b\": 2, \"a\": 1})",
        &["true"],
    );
}

#[test]
fn records_nest_in_containers_and_each_other() {
    assert_output(
        "type P:
    x: Int
    y: Int
type Q:
    p: P
ps = [P(1, 2)]
print(ps[0])
q = Q(P(3, 4))
print(q.p.x)",
        &["P(1, 2)", "3"],
    );
}