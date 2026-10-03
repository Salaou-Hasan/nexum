//! Containers and strings, end to end.
//!
//! Lists, dicts, slices and strings are where "obvious" semantics hide, so
//! most of these tests assert one specific claim each and print the container
//! back out rather than reducing it to a single number.
//!
//! Three areas get deliberately more attention than their size suggests:
//!
//! * **Dict insertion order** is a contract, not an accident of the hash map.
//!   Programs print dicts, sort them and zip them against other containers, so
//!   the order has to be the order the writes happened in. Asserted three ways.
//! * **Slice copying.** A slice that aliased its parent would make the value
//!   model a lie for the most common container operation there is.
//! * **`del`.** Deleting is where "out of range" and "not a container" and
//!   "immutable" are three different failures, and the checker only sees two
//!   of them.
//!
//! Two behaviours are known-broken and are **not** asserted here, because a
//! test that pins down a bug is worse than a missing test:
//!
//! * String length/index/slice are byte-based rather than character-based
//!   (grammar.md R3), so every string test here is ASCII.
//! * `"" in s` and a needle exactly as long as the haystack both report
//!   `false`.
//!
//! Two more were found while writing this file and have since been fixed:
//! slicing a string with an explicit step overflowed its destination buffer
//! (`ceil` where the code said `floor`, and an output offset advancing by
//! `step` instead of 1), and a slice of a list of lists shared its elements
//! with the parent. Both are asserted now -- see
//! `slice_copies_nested_containers_too`.

use nx_e2e::*;

#[track_caller]
fn assert_module_output(s: &Scratch, entry: &str, expected: &[&str]) {
    let o = run_in_dir(entry, false, &s.dir);
    assert_eq!(
        o.lines(),
        expected.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "\n  program output: {:?}",
        o.out
    );
    assert_eq!(o.code, 0, "expected exit 0, got {}", o.code);
}

/// Like `assert_rejected`, but for a program that has sibling modules: the
/// harness's own `assert_rejected` type-checks in a fresh empty directory, so
/// the imports would not resolve.
#[track_caller]
fn assert_module_rejected(s: &Scratch, entry: &str, needle: &str) {
    let o = run_in_dir(entry, false, &s.dir);
    assert_ne!(
        o.code, 0,
        "expected a rejection, but the program was accepted and printed {:?}\n  entry:\n{}",
        o.out, entry
    );
    assert!(
        o.out.contains(needle),
        "diagnostic did not contain {:?}\n  got: {}\n  entry:\n{}",
        needle,
        o.out,
        entry
    );
}

// --- lists ------------------------------------------------------------

#[test]
fn list_indexing_including_negative() {
    assert_output(
        r#"
xs = [10, 20, 30]
print(xs[0], xs[1], xs[2])
print(xs[-1], xs[-2], xs[-3])
print([1, 2, 3][1])
"#,
        &["10 20 30", "30 20 10", "2"],
    );
}

#[test]
fn list_len_as_function_and_as_sugar() {
    assert_output(
        r#"
xs = [1, 2, 3]
print(len(xs), xs.len())
empty = []
print(len(empty), empty.len())
print(len([1, 2, 3, 4, 5]))
"#,
        &["3 3", "0 0", "5"],
    );
}

#[test]
fn list_push_as_function_and_as_sugar() {
    // Both spellings are the same builtin; `xs.push(v)` is sugar for
    // `push(xs, v)` and both return None.
    assert_output(
        r#"
xs = [1, 2]
push(xs, 3)
xs.push(4)
print(xs, len(xs))
print(push(xs, 5))
print(xs)
"#,
        &["[1, 2, 3, 4] 4", "none", "[1, 2, 3, 4, 5]"],
    );
    // Appending in a loop is the ordinary way a list grows.
    assert_output(
        r#"
xs = []
for i in 0..5:
    push(xs, i * i)
print(xs)
"#,
        &["[0, 1, 4, 9, 16]"],
    );
}

#[test]
fn list_push_rejects_bad_receivers_and_type_mismatches() {
    // Pushing into a temporary would drop the result, so it is a type error
    // rather than a silent no-op.
    assert_rejected("push([1], 2)", "push() first argument must be a list variable");
    assert_rejected(
        r#"
xs = [1, 2]
xs.push(3)
xs.push("s")
"#,
        "push() element type mismatch",
    );
    // The first push into an empty list pins the element type.
    assert_rejected(
        r#"
xs = []
push(xs, 7)
push(xs, "s")
"#,
        "push() element type mismatch",
    );
    assert_rejected(
        r#"
d = {"a": 1}
d.push(2)
"#,
        "push() needs a list",
    );
    // Sugar needs a list *variable*, not a list-valued expression.
    assert_rejected(
        r#"
xs = [[1, 2]]
push(xs[0], 9)
"#,
        "push() first argument must be a list variable",
    );
}

#[test]
fn list_printing_forms() {
    // The printing form is a surface the tests above all depend on, so it is
    // worth pinning: brackets, comma-space, nested containers rendered
    // recursively, strings unquoted.
    assert_output(
        r#"
print([1, 2, 3])
print([1, 2, 3,])
print([])
print(["x", "y"])
print([1.5, 2.5])
print([true, false])
print([None])
print([[1, 2], [3]])
print([[]])
print([{}])
"#,
        &[
            "[1, 2, 3]",
            "[1, 2, 3]",
            "[]",
            "[x, y]",
            "[1.5, 2.5]",
            "[true, false]",
            "[none]",
            "[[1, 2], [3]]",
            "[[]]",
            "[{}]",
        ],
    );
}

#[test]
fn list_index_out_of_range_is_a_runtime_error() {
    // The checker cannot know the length, so this one really is a runtime
    // failure and not a rejection.
    assert_runtime_error(
        r#"
xs = [1, 2, 3]
print(xs[5])
"#,
        "index 5 out of range (len 3)",
    );
    assert_runtime_error(
        r#"
xs = [1, 2, 3]
print(xs[-4])
"#,
        "index -4 out of range (len 3)",
    );
    assert_runtime_error(
        r#"
xs = [1, 2, 3]
print("abc"[3])
"#,
        "index 3 out of range (len 3)",
    );
}

#[test]
fn list_index_must_be_an_int() {
    assert_rejected("xs = [1, 2]\nprint(xs[\"k\"])", "index must be Int, found Str");
    assert_rejected("xs = [1, 2]\nprint(xs[true])", "index must be Int, found Bool");
    assert_rejected("xs = [1, 2]\nprint(xs[1.5])", "index must be Int, found Float");
}

// --- list delete ------------------------------------------------------

#[test]
fn list_delete_by_index() {
    assert_output(
        r#"
xs = [1, 2, 3]
del xs[1]
print(xs)
del xs[0]
print(xs, len(xs))
"#,
        &["[1, 3]", "[3] 1"],
    );
}

#[test]
fn list_delete_by_negative_index() {
    assert_output(
        r#"
xs = [1, 2, 3]
del xs[-1]
print(xs)
del xs[-1]
print(xs)
"#,
        &["[1, 2]", "[1]"],
    );
}

#[test]
fn list_delete_out_of_range_fails() {
    assert_runtime_error("xs = [1, 2, 3]\ndel xs[5]", "index 5 out of range (len 3)");
    assert_runtime_error("xs = [1, 2, 3]\ndel xs[-4]", "index -4 out of range (len 3)");
    assert_runtime_error("xs = []\ndel xs[0]", "index 0 out of range (len 0)");
}

#[test]
fn list_delete_reaches_into_nested_lists() {
    assert_output(
        r#"
xs = [[1, 2], [3, 4]]
del xs[0][1]
print(xs)
del xs[1]
print(xs)
"#,
        &["[[1], [3, 4]]", "[[1]]"],
    );
}

#[test]
fn delete_of_a_string_element_or_a_non_container_is_rejected() {
    // Two different rules: strings are immutable, and there is nothing to
    // delete an index *of* on a scalar. Both are compile-time.
    assert_rejected(
        r#"
s = "abc"
del s[0]
"#,
        "strings are immutable",
    );
    assert_rejected("n = 5\ndel n[0]", "cannot delete an index of Int");
}

// --- dicts ------------------------------------------------------------

#[test]
fn dict_literal_read_and_len() {
    assert_output(
        r#"
d = {"a": 1, "b": 2}
print(d["a"], d["b"])
print(len(d), d.len())
print(d)
"#,
        &["1 2", "2 2", "{a: 1, b: 2}"],
    );
}

#[test]
fn empty_dict_has_length_zero() {
    assert_output(
        r#"
d = {}
print(d, len(d), d.len())
print("a" in d)
"#,
        &["{} 0 0", "false"],
    );
}

#[test]
fn dict_missing_key_read_is_a_runtime_error() {
    // `None` is distinct from a missing key, so there is no way to spell
    // "absent" as a value; the read fails instead.
    assert_runtime_error("d = {\"a\": 1}\nprint(d[\"zz\"])", "key not found");
    // Same for a key of the wrong type: a dict lookup that cannot match
    // reports the same failure.
    assert_runtime_error("d = {\"a\": 1}\nprint(d[0])", "key not found");
}

#[test]
fn dict_scalar_keys() {
    // Str, Int, Bool and Float keys are all values, so all four work as
    // keys -- including a Float, which is not an integer-indexed key.
    assert_output(
        r#"
d = {"a": 1, 2: "two", true: "yes", 2.5: "frac"}
print(d["a"], d[2], d[true], d[2.5])
print(len(d))
print(d)
"#,
        &[
            "1 two yes frac",
            "4",
            "{a: 1, 2: two, true: yes, 2.5: frac}",
        ],
    );
}

#[test]
fn dict_nested_values() {
    assert_output(
        r#"
d = {"a": [1, 2], "b": {"c": 3}}
print(d)
print(d["a"][1], d["b"]["c"], len(d["a"]), len(d["b"]))
"#,
        &["{a: [1, 2], b: {c: 3}}", "2 3 2 1"],
    );
}

// --- dict insertion order: a contract ---------------------------------

#[test]
fn dict_insertion_order_is_iteration_order() {
    // Not sorted, not hashed: the order writes happened in. Asserted by reading
    // the keys back in iteration order, not just by printing the dict.
    assert_output(
        r#"
d = {}
d["z"] = 1
d["a"] = 2
d["m"] = 3
print(d)
keys = []
for k in d:
    push(keys, k)
print(keys)
"#,
        &["{z: 1, a: 2, m: 3}", "[z, a, m]"],
    );
    // The same holds for a literal.
    assert_output(
        r#"
d = {"z": 1, "a": 2, "m": 3}
print(d)
"#,
        &["{z: 1, a: 2, m: 3}"],
    );
}

#[test]
fn dict_reassigning_a_key_does_not_move_it() {
    // Updating an existing key is an update, not a remove-and-reinsert. If it
    // moved, every counter-style dict would reorder under a second pass.
    assert_output(
        r#"
d = {}
d["z"] = 1
d["a"] = 2
d["z"] = 9
print(d)
keys = []
for k in d:
    push(keys, k)
print(keys)
"#,
        &["{z: 9, a: 2}", "[z, a]"],
    );
    // And a key re-added after a later key still keeps its original slot.
    assert_output(
        r#"
d = {"a": 1, "b": 2}
d["a"] = 7
d["c"] = 3
print(d)
"#,
        &["{a: 7, b: 2, c: 3}"],
    );
}

#[test]
fn dict_duplicate_literal_key_keeps_first_position_and_last_value() {
    // The two halves of the duplicate-key rule, which are easy to get half
    // right: position comes from the first binding, value from the last.
    assert_output(
        r#"
d = {"a": 1, "b": 2, "a": 3}
print(d)
keys = []
for k in d:
    push(keys, k)
print(keys)
"#,
        &["{a: 3, b: 2}", "[a, b]"],
    );
}

// --- dict delete ------------------------------------------------------

#[test]
fn dict_delete_removes_the_entry() {
    assert_output(
        r#"
d = {"a": 1, "b": 2, "c": 3}
del d["b"]
print(d)
print(len(d), "b" in d)
"#,
        &["{a: 1, c: 3}", "2 false"],
    );
}

// --- slices ------------------------------------------------------------

#[test]
fn slice_all_five_spellings() {
    // `a[:]`, `a[b:]`, `a[:b]`, `a[::c]`, `a[b:e:c]`. All are half-open.
    assert_output(
        r#"
xs = [0, 1, 2, 3, 4, 5, 6]
print(xs[:])
print(xs[2:])
print(xs[:3])
print(xs[::2])
print(xs[1:8:3])
"#,
        &[
            "[0, 1, 2, 3, 4, 5, 6]",
            "[2, 3, 4, 5, 6]",
            "[0, 1, 2]",
            "[0, 2, 4, 6]",
            "[1, 4]",
        ],
    );
}

#[test]
fn slice_out_of_range_bounds_are_clamped_not_rejected() {
    // Clamping, not an error: a bound past the end is the end. This is what
    // makes `xs[i:]` safe to write without first comparing against len(xs).
    assert_output(
        r#"
xs = [0, 1, 2, 3, 4]
print(xs[3:100])
print(xs[-100:2])
print(xs[100:])
print(xs[-100:-90])
print(xs[0:0])
"#,
        &["[3, 4]", "[0, 1]", "[]", "[]", "[]"],
    );
}

#[test]
fn slice_negative_bounds() {
    // A negative bound counts from the end, and two negatives are resolved
    // against the same length.
    assert_output(
        r#"
xs = [0, 1, 2, 3, 4, 5]
print(xs[-2:])
print(xs[:-3])
print(xs[-4:-2])
print(xs[-100:2])
"#,
        &["[4, 5]", "[0, 1, 2]", "[2, 3]", "[0, 1]"],
    );
}

#[test]
fn slice_with_a_step() {
    assert_output(
        r#"
xs = [0, 1, 2, 3, 4, 5, 6, 7]
print(xs[::2])
print(xs[1:7:3])
print(xs[::7])
print(xs[6:1:2])
"#,
        &["[0, 2, 4, 6]", "[1, 4]", "[0, 7]", "[]"],
    );
    // The step counts from `from`, so a step larger than the span yields the
    // single element at `from`.
    assert_output(
        r#"
xs = [0, 1, 2, 3, 4, 5]
print(xs[2:5:4])
print(xs[4:2:1])
"#,
        &["[2]", "[]"],
    );
}

#[test]
fn slice_step_zero_is_a_runtime_error() {
    // The checker cannot see it: the step is usually only known at runtime.
    assert_runtime_error("xs = [0, 1, 2, 3]\nprint(xs[::0])", "slice step must be positive");
    assert_runtime_error("xs = [0, 1, 2, 3]\nprint(xs[::-1])", "slice step must be positive");
}

#[test]
fn slice_copies_rather_than_aliases() {
    // A slice is a new value, so writing through one side must not be
    // visible on the other.
    assert_output(
        r#"
xs = [1, 2, 3, 4]
ys = xs[1:3]
xs[1] = 99
print(xs, ys)
"#,
        &["[1, 99, 3, 4] [2, 3]"],
    );
    // And the other direction: growing the slice does not grow the source.
    assert_output(
        r#"
xs = [1, 2, 3, 4, 5]
ys = xs[1:4]
push(ys, 99)
print(xs, ys)
"#,
        &["[1, 2, 3, 4, 5] [2, 3, 4, 99]"],
    );
}

#[test]
fn slice_copies_nested_containers_too() {
    // The test above only reaches top-level `Int` elements, which a shallow
    // slice already handled. The elements of a list of lists are themselves
    // mutable, so sharing them makes the slice write through into its parent --
    // and then `ys = xs` and `ys = xs[0:2]` mean different things, which is
    // the one thing value semantics is not allowed to do.
    assert_output(
        r#"
xs = [[1, 2], [3, 4], [5, 6]]
ys = xs[0:2]
ys[0][0] = 99
print(xs)
print(ys)
"#,
        &["[[1, 2], [3, 4], [5, 6]]", "[[99, 2], [3, 4]]"],
    );
    // Dicts.
    assert_output(
        r#"
xs = [{"a": 1}, {"b": 2}]
ys = xs[0:1]
ys[0]["a"] = 9
print(xs)
print(ys)
"#,
        &["[{a: 1}, {b: 2}]", "[{a: 9}]"],
    );
    // Records.
    assert_output(
        r#"
type P:
    x: Int

xs = [P(1), P(2)]
ys = xs[0:1]
ys[0].x = 9
print(xs[0].x)
print(ys[0].x)
"#,
        &["1", "9"],
    );
    // Two levels down, and through a slice of a slice.
    assert_output(
        r#"
xs = [[[1]]]
ys = xs[0:1]
ys[0][0][0] = 7
print(xs)
print(ys)
"#,
        &["[[[1]]]", "[[[7]]]"],
    );
    assert_output(
        r#"
xs = [[1, 2], [3, 4], [5, 6]]
ys = xs[0:3][1:3]
ys[0][0] = 99
print(xs)
print(ys)
"#,
        &["[[1, 2], [3, 4], [5, 6]]", "[[99, 4], [5, 6]]"],
    );
}

#[test]
fn slice_of_a_string_is_a_string() {
    // A string slice is a Str, not a List(Str): it concatenates and it
    // compares against another string.
    assert_output(
        r#"
s = "abcdef"
print(s[1:4], s[:2], s[3:], s[-2:])
print(s[1:4] + "!")
print(s[1:4] == "bcd")
"#,
        &["bcd ab def ef", "bcd!", "true"],
    );
    // Out-of-range clamps for strings too, and the result is still a Str:
    // concatenating brackets onto it is the evidence, since an empty string
    // prints as nothing at all.
    assert_output(
        r#"
s = "abc"
print("[" + s[3:] + "]", "[" + s[10:20] + "]", "[" + s[0:0] + "]")
print(len(s[3:]), len(s[10:20]), len(s[0:0]))
"#,
        &["[] [] []", "0 0 0"],
    );
    // NOT asserted, and deliberately: `s[a:b:step]` on a *string* is broken
    // independently of the byte-vs-character issue. `@nx_slice` sizes the
    // destination buffer with `sdiv(len, step)` instead of the
    // `ceil(len, step)` bytes the loop actually writes, so the result is
    // heap garbage -- `"abcdef"[::2]` prints `alc`, not `ace`. That is a
    // memory-safety bug, not a semantics disagreement, so it gets a bug
    // report and not a pinned expectation. Every string slice with an
    // explicit step is skipped here until it is fixed.
}

// --- strings -----------------------------------------------------------

#[test]
fn string_printing_len_and_indexing() {
    assert_output(
        r#"
s = "hello"
print(s)
print(len(s), s.len())
print(s[0], s[4], s[-1], s[-5])
print("", len(""))
"#,
        &["hello", "5 5", "h o o h", " 0"],
    );
}

#[test]
fn string_concatenation() {
    assert_output(
        r#"
print("foo" + "bar")
print("a" + "b" + "c")
print("" + "x" + "")
xs = ["a", "b"]
print("x" + xs[0] + xs[1])
s = ""
for w in ["one", "two"]:
    s = s + w + " "
print(s, len(s))
"#,
        &["foobar", "abc", "x", "xab", "one two  8"],
    );
}

#[test]
fn string_concatenation_with_a_non_string_is_rejected() {
    // There is no implicit conversion, so a mistake here is a static error
    // rather than a runtime surprise.
    assert_rejected("print(\"n=\" + 5)", "operator '+' not supported for Str and Int");
    assert_rejected("print(\"n=\" + true)", "operator '+' not supported for Str and Bool");
}

// --- membership --------------------------------------------------------

#[test]
fn membership_in_lists() {
    assert_output(
        r#"
xs = [1, 2, 3]
print(2 in xs, 9 in xs)
print("a" in ["a", "b"], "z" in ["a", "b"])
print(1 in [])
"#,
        &["true false", "true false", "false"],
    );
}

#[test]
fn membership_in_dicts() {
    // Over a dict, `in` asks about keys, not values.
    assert_output(
        r#"
d = {"a": 1, "b": 2}
print("a" in d, "z" in d)
print(1 in d, 2 in d)
"#,
        &["true false", "false false"],
    );
}

#[test]
fn membership_in_strings_is_substring() {
    // Over a string, `in` is a substring test, not a character test.
    assert_output(
        r#"
print("ell" in "hello")
print("zz" in "hello")
print("hel" in "hello", "ello" in "hello")
print("h" in "hello", "o" in "hello")
"#,
        &["true", "false", "true true", "true true"],
    );
    // Note the two cases that are NOT asserted anywhere in this file because
    // they are known-broken: `"" in "hello"` and `"hello" in "hello"` both
    // report false. See the module note at the top.
}

#[test]
fn not_in_over_containers() {
    assert_output(
        r#"
xs = [1, 2, 3]
print(9 not in xs, 2 not in xs)
d = {"a": 1}
print("z" not in d, "a" not in d)
print("zz" not in "hello")
"#,
        &["true false", "true false", "true"],
    );
    // `not in` binds as one operator, so it needs no parentheses.
    assert_one("print(not (2 in [1, 2]))", "false");
}

#[test]
fn membership_needs_a_container_on_the_right() {
    assert_rejected("print(1 in 5)", "'in' needs a list, string or dict on the right");
    assert_rejected("print(\"a\" in 5)", "'in' needs a list, string or dict on the right");
}

// --- for loops ---------------------------------------------------------

#[test]
fn for_over_a_list() {
    assert_output(
        r#"
xs = [10, 20, 30]
for x in xs:
    print(x)
total = 0
for x in xs:
    total += x
print(total)
for x in []:
    print(x)
print("done")
"#,
        &["10", "20", "30", "60", "done"],
    );
}

#[test]
fn for_over_a_dict_yields_keys() {
    // Iterating a dict yields keys, in insertion order; the value comes from
    // a lookup. That is the shape every counter idiom in the language uses.
    assert_output(
        r#"
counts = {"apple": 3, "pear": 1}
for word in counts:
    print(word, counts[word])
d = {}
for k in ["z", "a", "m"]:
    d[k] = 1
for k in d:
    print(k)
"#,
        &["apple 3", "pear 1", "z", "a", "m"],
    );
}

#[test]
fn for_over_a_string_yields_characters() {
    assert_output(
        r#"
for c in "abc":
    print(c)
print(len("abc"))
"#,
        &["a", "b", "c", "3"],
    );
}

// --- comprehensions -----------------------------------------------------

#[test]
fn comprehension_over_a_list() {
    assert_output(
        r#"
print([i * i for i in 0..5])
print([x * 10 for x in [1, 2, 3]])
print([x for x in []])
"#,
        &["[0, 1, 4, 9, 16]", "[10, 20, 30]", "[]"],
    );
}

#[test]
fn comprehension_over_a_string() {
    // The loop variable is bound to a Str, so it is a one-character string
    // and the result is a list of them.
    assert_output(
        r#"
print([c for c in "abc"])
print([c + "!" for c in "abc"])
"#,
        &["[a, b, c]", "[a!, b!, c!]"],
    );
}

#[test]
fn comprehension_with_a_filter() {
    // At most one `if`, and it is the comprehension's own -- not a ternary
    // on the iterable.
    assert_output(
        r#"
print([i for i in 0..10 if i % 2 == 0])
xs = [1, 2, 3, 4]
print([x * 10 for x in xs if x > 2])
print(len([x for x in xs if x % 2 == 1]))
print([i for i in 0..12 if i % 3 == 0])
"#,
        &["[0, 2, 4, 6, 8]", "[30, 40]", "2", "[0, 3, 6, 9]"],
    );
}

// --- modules -----------------------------------------------------------

#[test]
fn import_and_attribute_access() {
    // `import m` binds the module; functions and types are reached through it.
    let s = Scratch::new("import");
    s.write(
        "utils.nx",
        r#"
fn double(x):
    return x * 2

type Pair:
    a: Int
    b: Int

fn make():
    return Pair(1, 2)
"#,
    );
    assert_module_output(
        &s,
        r#"
import utils
print(utils.double(21))
p = utils.make()
print(p, p.a + p.b)
print(utils.double(1) + 1)
"#,
        &["42", "Pair(1, 2) 3", "3"],
    );
}

#[test]
fn from_import_with_an_alias() {
    let s = Scratch::new("fromimport");
    s.write(
        "utils.nx",
        r#"
fn double(x):
    return x * 2

type Pair:
    a: Int
    b: Int
"#,
    );
    // An alias for a *type* is canonicalised, so a value built through the
    // alias compares equal to one built through the original name.
    assert_module_output(
        &s,
        r#"
from utils import double, Pair as P
print(double(21))
print(P(3, 4))
print(P(1, 2) == P(1, 2))
"#,
        &["42", "Pair(3, 4)", "true"],
    );
}

#[test]
fn missing_module_is_rejected() {
    let s = Scratch::new("nomodule");
    assert_module_rejected(
        &s,
        r#"
import nosuchmodule
print(nosuchmodule.f())
"#,
        "cannot find module 'nosuchmodule.nx'",
    );
    // And on the `from` form too.
    let s2 = Scratch::new("nomodule2");
    assert_module_rejected(
        &s2,
        r#"
from nosuchmodule import thing
print(thing())
"#,
        "cannot find module 'nosuchmodule.nx'",
    );
}

#[test]
fn circular_import_is_rejected() {
    // Module resolution must terminate: a.nx imports b.nx and b.nx imports
    // a.nx. Left alone this is unbounded recursion.
    let s = Scratch::new("circular");
    s.write("a.nx", "import b\nfn af():\n    return 1\n");
    s.write("b.nx", "import a\nfn bf():\n    return 2\n");
    assert_module_rejected(
        &s,
        r#"
import a
print(a.af())
"#,
        "circular import",
    );
    // The other direction is the same cycle.
    let s2 = Scratch::new("circular2");
    s2.write("x.nx", "import y\nfn xf():\n    return 1\n");
    s2.write("y.nx", "import x\nfn yf():\n    return 2\n");
    assert_module_rejected(
        &s2,
        r#"
import y
print(y.yf())
"#,
        "circular import",
    );
}
// --- string slicing with a step: regression -------------------------
//
// These were a heap buffer overflow. nx_slice's string path malloc'd
// floor(cap/step) and wrote the result at source-relative offsets, so a
// step of 2 wrote at 0, 2, 4... into a buffer sized for the packed
// answer: every odd byte stayed uninitialised and the tail ran off the
// end. The list path was unaffected because it allocates then pushes,
// which is why slicing a list always looked right.

#[test]
fn string_slice_with_a_step_packs_the_output() {
    // cap divides evenly: the case that hid the bug, because floor agrees
    assert_one(r#"print("abcdef"[::2])"#, "ace");
    assert_one(r#"print("abcdef"[1::2])"#, "bdf");
    // cap does NOT divide evenly: this is what floor got wrong
    assert_one(r#"print("abcde"[::2])"#, "ace");
    assert_one(r#"print("xyz"[::2])"#, "xz");
    assert_one(r#"print("abcdef"[::3])"#, "ad");
    assert_one(r#"print("abcdef"[::5])"#, "af");
    assert_one(r#"print("abcdefghij"[::2])"#, "acegi");
}

#[test]
fn string_slice_step_of_one_is_the_whole_slice() {
    assert_one(r#"print("abcdef"[::1])"#, "abcdef");
    assert_one(r#"print("abcdef"[1::1])"#, "bcdef");
}

#[test]
fn string_slice_without_a_step_is_unaffected() {
    assert_one(r#"print("abcdef"[1:])"#, "bcdef");
    assert_one(r#"print("abcdef"[:3])"#, "abc");
    assert_one(r#"print("abcdef"[2:4])"#, "cd");
    assert_one(r#"print("abcdef"[:])"#, "abcdef");
}

#[test]
fn the_list_step_slice_still_works() {
    // The list path allocates then pushes, so it was never affected. Pin it
    // anyway: the string fix must not have disturbed the sibling path.
    assert_one("print([0,1,2,3,4,5][::2])", "[0, 2, 4]");
    assert_one("print([0,1,2,3,4,5][1::2])", "[1, 3, 5]");
    assert_one("print([0,1,2,3,4,5][::3])", "[0, 3]");
}

#[test]
fn a_stepped_string_slice_is_packed_not_strided() {
    // Guards the specific defect: if the destination offset regresses to
    // mirroring the source offset, this prints a string longer than the
    // source, which is the observable symptom of the overflow.
    let o = run(r#"print(len("abcdefgh"[::2]))"#);
    assert_eq!(o.lines(), vec!["4".to_string()]);
}
