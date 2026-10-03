//! Methods and error handling.
//!
//! **`mut self` write-back.** `p.moved(1, 2)` means `p = moved(p, 1, 2)`.
//! That is a genuinely unusual rule: the receiver is a copy, and the copy
//! becomes the receiver again only because the method is allowed to write
//! through it and is required to return it. Every consequence of the rule is
//! asserted here, including the one that is easy to get wrong -- a chain
//! writes back exactly once, to the outermost variable, and nowhere else.
//!
//! **Unboxing must not be a language.** `NX_NOUNBOX=1` selects an all-boxed
//! representation. It is supposed to be a compiler detail, so anything that
//! touches method resolution or receiver write-back is compiled and run in
//! *both* representations and the outputs are required to be byte-identical. A
//! representation change that altered a program's output would be a second
//! language wearing the same syntax, and a test suite that only ever ran the
//! default build could not tell the difference.
//!
//! `parallel:` used to be tested here too. It was removed: it asked the
//! scheduler to prove race-freedom statically, the proof had holes, and a
//! program could print the wrong answer whenever it lost a race.
use nx_e2e::*;

/// Compile and run `src` twice -- once in the default build, once with
/// unboxing switched off -- and require the same program from both.
///
/// The expected lines are asserted as well as the agreement, so this is not
/// just a differential: it says what the answer *is*.
#[track_caller]
fn both_ways(src: &str, expected: &[&str]) {
    let want: Vec<String> = expected.iter().map(|s| s.to_string()).collect();

    let unboxed = run_in(src, false);
    assert_eq!(
        unboxed.lines(),
        want,
        "\n  default build printed {:?}\n  source:\n{}",
        unboxed.out,
        src
    );
    assert_eq!(unboxed.code, 0, "default build exited {}", unboxed.code);

    let boxed = run_in(src, true);
    assert_eq!(
        boxed.lines(),
        want,
        "\n  NX_NOUNBOX build printed {:?}\n  source:\n{}",
        boxed.out,
        src
    );
    assert_eq!(boxed.code, 0, "NX_NOUNBOX build exited {}", boxed.code);

    assert_eq!(
        boxed.out, unboxed.out,
        "\n  unboxing changed what the program does\n  default:   {:?}\n  NX_NOUNBOX: {:?}\n  source:\n{}",
        unboxed.out, boxed.out, src
    );
}

// --- reading self ----------------------------------------------------

#[test]
fn a_method_reads_fields_off_self() {
    // `self` is the record itself, so a field read is just a read.
    both_ways(
        "type Point:
    x: Int
    y: Int
impl Point:
    fn area(self):
        return self.x * self.y
    fn wide(self):
        return self.x > self.y
p = Point(3, 4)
print(p.area())
print(p.wide())
print(Point(1, 9).wide())",
        &["12", "false", "false"],
    );
}

#[test]
fn self_is_a_copy_so_a_read_only_method_cannot_be_observed_writing() {
    // The reason writing through a read-only `self` is refused rather than
    // ignored: `self` is a copy, so the write would go nowhere.
    both_ways(
        "type P:
    x: Int
impl P:
    fn scaled(self, k):
        return P(self.x * k)
p = P(3)
q = p.scaled(10)
print(q)
print(p)
r = p
r.scaled(100)
print(r)
print(p)",
        &["P(30)", "P(3)", "P(3)", "P(3)"],
    );
}

#[test]
fn writing_through_a_read_only_self_is_rejected() {
    assert_rejected(
        "type P:
    x: Int
impl P:
    fn setx(self):
        self.x = 1
        return self.x",
        "cannot mutate through read-only 'self'",
    );
}

#[test]
fn writing_through_a_nested_read_only_self_is_rejected() {
    // `self.at.y = 5` is still a write rooted at `self`, so the same rule
    // applies however deep the path goes.
    assert_rejected(
        "type Point:
    x: Int
type Named:
    at: Point
impl Named:
    fn shove(self):
        self.at.x = 5
        return self.at.x",
        "cannot mutate through read-only 'self'",
    );
}

#[test]
fn a_mut_self_method_may_not_be_called_through_a_read_only_self() {
    // Same reason: the inner write-back would have nowhere to land.
    assert_rejected(
        "type P:
    x: Int
impl P:
    fn bumped(mut self):
        self.x = self.x + 1
        return self
    fn twice(self):
        return self.bumped()",
        "cannot call mut method through read-only 'self'",
    );
}

// --- mut self write-back ---------------------------------------------

#[test]
fn mut_self_writes_back_into_the_callers_variable() {
    // The flagship rule: the caller's variable is reassigned to the method's
    // result, so the change is visible afterwards.
    both_ways(
        "type P:
    x: Int
    y: Int
impl P:
    fn moved(mut self, dx, dy):
        self.x = self.x + dx
        self.y = self.y + dy
        return self
p = P(1, 1)
print(p)
p.moved(2, 3)
print(p)
p.moved(-1, -1)
print(p)",
        &["P(1, 1)", "P(3, 4)", "P(2, 3)"],
    );
}

#[test]
fn a_mut_self_call_in_expression_position_yields_its_result() {
    both_ways(
        "type P:
    x: Int
impl P:
    fn bumped(mut self):
        self.x = self.x + 1
        return self
p = P(10)
print(p.bumped())
print(p)
n = p.bumped().x
print(n)
print(p)",
        &["P(11)", "P(11)", "12", "P(12)"],
    );
}

#[test]
fn a_chain_writes_back_exactly_once() {
    // THE subtle one. `p.plus(1)` is a real call with storage, so it writes
    // P(1, 1) back into `p`. The second `.plus(1)` has no storage to write
    // to -- its receiver is a temporary -- so it only contributes to the
    // chain's value. The chain prints P(2, 2); `p` ends up as P(1, 1).
    both_ways(
        "type P:
    x: Int
    y: Int
impl P:
    fn plus(mut self, d):
        self.x = self.x + d
        self.y = self.y + d
        return self
p = P(0, 0)
print(p.plus(1).plus(1))
print(p)",
        &["P(2, 2)", "P(1, 1)"],
    );
}

#[test]
fn a_chain_on_a_pure_temporary_writes_back_nowhere() {
    // Same chain, but the base has no storage at all, so nothing is written
    // anywhere -- the result is only observable as the chain's value.
    both_ways(
        "type P:
    x: Int
    y: Int
impl P:
    fn plus(mut self, d):
        self.x = self.x + d
        self.y = self.y + d
        return self
print(P(0, 0).plus(1).plus(1).plus(1))",
        &["P(3, 3)"],
    );
}

#[test]
fn a_mut_self_method_recurses_through_self() {
    // `self.up(n - 1)` is a `mut self` call on a value that has storage, so
    // each level writes its result back into the level above it, and the
    // outermost call writes back into `p`.
    both_ways(
        "type P:
    x: Int
    y: Int
impl P:
    fn up(mut self, n):
        if n <= 0:
            return self
        else:
            self.x = self.x + 1
            self.y = self.y + 2
            return self.up(n - 1)
    fn packed(self):
        return self.x * 100 + self.y
p = P(0, 0)
p.up(3)
print(p)
print(p.packed())
print(P(0, 0).up(2))
q = P(5, 5)
print(q.up(2).up(2))
print(q)",
        &["P(3, 6)", "306", "P(2, 4)", "P(9, 13)", "P(7, 9)"],
    );
}

#[test]
fn a_mut_self_method_that_returns_something_else_is_rejected() {
    // Without this the write-back would install a value of the wrong shape
    // in the caller's variable, and the rule would be unenforceable.
    assert_rejected(
        "type P:
    x: Int
impl P:
    fn bad(mut self):
        self.x = self.x + 1
        return self.x",
        "mut method 'bad' must return 'P', found Int",
    );
}

#[test]
fn a_mut_self_method_that_returns_nothing_is_rejected() {
    assert_rejected(
        "type P:
    x: Int
impl P:
    fn bad(mut self):
        self.x = self.x + 1",
        "must return 'P', found None",
    );
}

#[test]
fn a_mut_self_method_may_return_a_fresh_record_of_the_same_type() {
    // The rule is about the *type*, not about the identity of the object: a
    // newly built record of the same type is a legal thing to write back.
    both_ways(
        "type P:
    x: Int
impl P:
    fn triple_then_one(mut self, k):
        self.x = self.x * k
        return P(self.x + 1)
p = P(2)
p.triple_then_one(3)
print(p)",
        &["P(7)"],
    );
}

#[test]
fn mut_self_may_write_through_a_nested_field() {
    both_ways(
        "type Inner:
    v: Int
type Outer:
    inner: Inner
impl Outer:
    fn grow(mut self):
        self.inner.v = self.inner.v + 10
        return self
o = Outer(Inner(1))
o.grow()
print(o)",
        &["Outer(Inner(11))"],
    );
}

#[test]
fn mut_self_on_a_list_element_writes_that_element_back() {
    both_ways(
        "type P:
    x: Int
impl P:
    fn bumped(mut self):
        self.x = self.x + 1
        return self
ps = [P(1), P(10)]
ps[0].bumped()
print(ps)",
        &["[P(2), P(10)]"],
    );
}

#[test]
fn mut_self_on_a_loop_variable_leaves_the_list_alone() {
    // The loop variable is a real variable with storage, so `mut self` does
    // write back into it -- but the list the loop walked is a separate copy,
    // and value semantics say the list is unchanged.
    both_ways(
        "type P:
    x: Int
impl P:
    fn bumped(mut self):
        self.x = self.x + 1
        return self
ps = [P(1), P(2)]
for p in ps:
    p.bumped()
    print(p)
print(ps)",
        &["P(2)", "P(3)", "[P(1), P(2)]"],
    );
}

// --- associated functions --------------------------------------------

#[test]
fn an_associated_function_is_called_on_the_type_or_on_a_value() {
    both_ways(
        "type Counter:
    n: Int
impl Counter:
    fn zero():
        return Counter(0)
    fn counted(n):
        return Counter(n)
    fn get(self):
        return self.n
print(Counter.zero())
print(Counter.counted(41).get())
c = Counter(7)
print(c.zero())
print(c.get())",
        &["Counter(0)", "41", "Counter(0)", "7"],
    );
}

#[test]
fn a_method_with_a_receiver_is_not_an_associated_function() {
    // `P.add` names a real method, so calling it on the type has to fail --
    // silently dropping the receiver would pass the wrong arity.
    assert_rejected(
        "type P:
    x: Int
impl P:
    fn add(mut self, k):
        self.x = self.x + k
        return self
print(P.add(P(5), 3))",
        "needs a receiver",
    );
}

#[test]
fn an_unknown_method_on_a_type_is_rejected_by_name() {
    // The diagnostic has to name the type: "no method 'nope'" alone would be
    // useless with more than one type in scope.
    assert_rejected(
        "type P:
    x: Int
impl P:
    fn area(self):
        return self.x * self.x
p = P(1)
print(p.nope())",
        "type 'P' has no method 'nope'",
    );
}

#[test]
fn an_impl_for_an_unknown_type_is_rejected() {
    assert_rejected(
        "impl Nope:
    fn f(self):
        return self.x",
        "unknown type 'Nope'",
    );
}

#[test]
fn method_arity_is_checked_in_both_directions() {
    assert_rejected(
        "type P:
    x: Int
impl P:
    fn scaled(self, k):
        return self.x * k
p = P(1)
print(p.scaled())",
        "expects 1 args, got 0",
    );
    assert_rejected(
        "type P:
    x: Int
impl P:
    fn scaled(self, k):
        return self.x * k
p = P(1)
print(p.scaled(1, 2))",
        "expects 1 args, got 2",
    );
}

// --- dispatch --------------------------------------------------------

#[test]
fn methods_resolve_on_a_record_held_in_a_list() {
    // The list pins the element type, so the loop variable is a statically
    // known record and its methods resolve.
    both_ways(
        "type Rect:
    w: Int
    h: Int
impl Rect:
    fn area(self):
        return self.w * self.h
    fn grow(mut self):
        self.w = self.w + 1
        return self
rs = [Rect(1, 2), Rect(3, 4)]
total = 0
for r in rs:
    total = total + r.area()
print(total)
for r in rs:
    r.grow()
    print(r)
print(rs)",
        &["14", "Rect(2, 2)", "Rect(4, 4)", "[Rect(1, 2), Rect(3, 4)]"],
    );
}

#[test]
fn methods_resolve_on_the_receivers_own_type() {
    // Two types, one method name: resolution is by receiver, not by name.
    both_ways(
        "type A:
    v: Int
type B:
    v: Int
impl A:
    fn tag(self):
        return \"A\"
impl B:
    fn tag(self):
        return \"B\"
print(A(1).tag())
print(B(2).tag())
for x in [A(3), A(4)]:
    print(x.tag())
for y in [B(5), B(6)]:
    print(y.tag())",
        &["A", "B", "A", "A", "B", "B"],
    );
}

#[test]
fn a_user_method_beats_the_builtin_push_sugar() {
    // Resolution order: methods are tried before builtin sugar, so a type may
    // define its own `push`. The sugar still works for plain lists, because
    // there is no method to shadow it with.
    both_ways(
        "type Bag:
    xs: Int
impl Bag:
    fn push(self, v):
        return v + 100
b = Bag(0)
print(b.push(9))
xs = [1, 2]
xs.push(3)
print(xs)
print(push(xs, 4))
print(xs)",
        &["109", "[1, 2, 3]", "none", "[1, 2, 3, 4]"],
    );
}

#[test]
fn a_user_method_beats_the_builtin_len_sugar() {
    both_ways(
        "type C:
    n: Int
impl C:
    fn len(self):
        return self.n * 2
print(C(21).len())
xs = [1, 2, 3]
print(xs.len())
print(len(\"abcd\"))",
        &["42", "3", "4"],
    );
}

#[test]
fn a_duplicate_method_name_in_one_impl_is_rejected() {
    assert_rejected(
        "type P:
    x: Int
impl P:
    fn f(self):
        return self.x
    fn f(self):
        return 0",
        "duplicate method 'f'",
    );
}

#[test]
fn an_impl_block_accepts_only_fn_definitions() {
    assert_rejected(
        "type P:
    x: Int
impl P:
    x = 1",
        "only `fn` definitions allowed in impl block",
    );
}

#[test]
fn two_impl_blocks_for_one_type_merge() {
    both_ways(
        "type P:
    x: Int
impl P:
    fn bumped(mut self):
        self.x = self.x + 1
        return self
impl P:
    fn peek(self):
        return self.x
p = P(1)
p.bumped()
print(p.peek())",
        &["2"],
    );
}

#[test]
fn self_outside_a_method_is_undefined() {
    assert_rejected(
        "fn f():
    return self.x
print(f())",
        "undefined variable 'self'",
    );
}

#[test]
fn own_self_parses_today_and_reads_the_receiver() {
    // Reserved for Stage 6; today it behaves like a read-only receiver.
    both_ways(
        "type P:
    x: Int
impl P:
    fn peek(own self):
        return self.x
print(P(7).peek())",
        &["7"],
    );
}

// --- unboxing --------------------------------------------------------

#[test]
fn the_whole_methods_surface_agrees_across_both_representations() {
    // One program touching every part of the method surface at once. If
    // unboxing were allowed to change meaning, receiver write-back, chaining,
    // recursion, dispatch through a list and builtin shadowing are exactly
    // where it would show.
    both_ways(
        "type Point:
    x: Int
    y: Int

impl Point:
    fn area(self):
        return self.x * self.y

    fn moved(mut self, dx, dy):
        self.x = self.x + dx
        self.y = self.y + dy
        return self

    fn origin():
        return Point(0, 0)

p = Point(3, 4)
print(p.area())
p.moved(1, -2)
print(p)
print(p.area())
q = Point(0, 0)
print(q.moved(1, 1).moved(1, 1).area())
print(q)
print(Point.origin())
print(Point(2, 5).area())
pts = [Point(1, 2), Point(3, 4)]
total = 0
for pt in pts:
    total = total + pt.area()
print(total)
for pt in pts:
    pt.moved(10, 10)
    print(pt)
print(pts)
xs = [1, 2]
xs.push(3)
print(xs)
print(xs.len())",
        &[
            "12",
            "Point(4, 2)",
            "8",
            "4",
            "Point(1, 1)",
            "Point(0, 0)",
            "10",
            "14",
            "Point(11, 12)",
            "Point(13, 14)",
            "[Point(1, 2), Point(3, 4)]",
            "[1, 2, 3]",
            "3",
        ],
    );
}

// --- error handling --------------------------------------------------

#[test]
fn a_true_assert_is_silent() {
    both_ways(
        "print(\"before\")
assert 1 == 1
assert 2 + 2 == 4, \"math is broken\"
print(\"after\")",
        &["before", "after"],
    );
}

#[test]
fn a_failed_assert_reaches_stdout_with_its_message() {
    // The runtime prints to stdout, not stderr: a failing program must be
    // able to say why in the same stream as the output it was in the middle
    // of producing.
    assert_runtime_error(
        "print(\"before\")
assert 1 == 2, \"one is not two\"
print(\"after\")",
        "one is not two: assertion failed",
    );
}

#[test]
fn a_failed_assert_without_a_message_still_reports() {
    assert_runtime_error(
        "print(\"before\")
assert false
print(\"after\")",
        "assertion failed",
    );
}

#[test]
fn a_runtime_failure_inside_a_method_stops_the_program() {
    // Both the trace of what ran first and the diagnostic survive, which is
    // what makes a failure debuggable.
    assert_runtime_error(
        "type P:
    x: Int
impl P:
    fn half(self, k):
        return self.x // k
    fn checked(self, k):
        assert k != 0, \"divisor must not be zero\"
        return self.half(k)
p = P(10)
print(p.checked(2))
print(P(10).checked(0))",
        "divisor must not be zero: assertion failed",
    );
}
