
use super::*;

fn ok(src: &str) {
    if let Err(es) = check_source(src, std::path::Path::new(".")) {
        panic!("{src:?} unexpectedly failed: {es:?}");
    }
}

fn err(src: &str) -> Vec<CheckError> {
    check_source(src, std::path::Path::new(".")).expect_err("expected type errors")
}

// ---- Stage 1 ----

#[test]
fn new_operators_type_check() {
    ok("x = 7 % 3\ny = x // 2\nz = y ** 2\n");
    ok("x = 7 & 3\nx = x | 1\nx = x ^ 1\nx = x << 2\nx = x >> 1\nx = ~x\n");
    ok("x = 2 ** 0.5\nprint(x)\n");
}

/// `%`, `//` and the bitwise operators are Int-only. Widening them to
/// Float would mean inventing a rounding rule for `7 % 2.5`, so they
/// are refused instead.
#[test]
fn integral_operators_reject_floats() {
    assert!(!err("x = 7 % 2.5\n").is_empty());
    assert!(!err("x = 7 // 2.5\n").is_empty());
    assert!(!err("x = 7 & 2.5\n").is_empty());
    // The shift distance is a count, so it is Int even though the
    // shifted value could be anything integral.
    assert!(!err("x = 1 << 2.5\n").is_empty());
}

#[test]
fn membership_type_checks() {
    ok("xs = [1, 2, 3]\nprint(1 in xs)\nprint(1 not in xs)\n");
    ok("print(\"a\" in \"abc\")\n");
    ok("d = {\"a\": 1}\nprint(\"a\" in d)\n");
    assert!(!err("print(1 in 5)\n").is_empty());
}

#[test]
fn ternary_branches_must_agree() {
    ok("x = 1 if true else 2\n");
    // `None` is the optional idiom, so it is allowed to disagree.
    ok("x = 1 if true else None\n");
    ok("x = None if true else 1\n");
    assert!(!err("x = 1 if true else \"a\"\n").is_empty());
    assert!(!err("x = 1 if 5 else 2\n").is_empty());
}

#[test]
fn indexed_assignment_checks_element_type() {
    ok("xs = [1, 2, 3]\nxs[0] = 9\n");
    // A List(Int) must not be handed a String by a plain assignment.
    let es = err("xs = [1, 2, 3]\nxs[0] = \"a\"\n");
    assert!(
        es.iter().any(|e| e.message.contains("cannot store")),
        "{es:?}"
    );
    assert!(!err("xs = [1, 2, 3]\nxs[0.5] = 9\n").is_empty());
    assert!(!err("xs = [1, 2, 3]\nxs[0] += \"a\"\n").is_empty());
}

#[test]
fn dict_index_uses_value_keys() {
    ok("d = {\"a\": 1}\nd[\"b\"] = 2\nprint(d[\"a\"])\n");
    // A dict is keyed by value, so a String key is fine where a
    // positional index would have to be an Int.
    ok("d = {1: \"a\", 2.5: \"b\", true: \"c\"}\nprint(d[1])\n");
    assert!(!err("d = {\"a\": 1}\nprint(d[[1]])\n").is_empty());
}

#[test]
fn dict_keys_must_be_scalar() {
    assert!(!err("d = {[[1]]: 2}\n").is_empty());
    // Values may be anything -- that is how a dict carries
    // heterogeneity on purpose.
    ok("d = {\"a\": [1, 2], \"b\": \"x\"}\n");
}

#[test]
fn multiple_assignment_shape_is_checked() {
    ok("a, b = 1, 2\n");
    ok("fn f():\n    return 1, 2\na, b = f()\n");
    // Two targets against one value is destructuring, which the
    // resolved at runtime; one target against two values
    // is a shape error.
    assert!(!err("a = 1, 2\n").is_empty());
}

/// A mixed tuple keeps the function's type usable rather than being
/// reported as an inconsistent return.
#[test]
fn mixed_tuple_return_is_allowed() {
    ok("fn f():\n    return 1, \"a\"\nx, y = f()\nprint(x)\n");
}

/// `None` must not pin a variable or conflict with one, so the
/// `x = 1` / `x = None` / `x = 2` pattern works.
#[test]
fn none_does_not_pin_or_conflict() {
    ok("x = 1\nx = None\nx = 2\nprint(x)\n");
    ok("x = None\nx = 5\n");
    ok("x = \"a\"\nx = None\n");
}

#[test]
fn comprehension_scopes_its_variable() {
    // The loop variable is bound inside the comprehension, so a
    // same-named outer binding is neither read nor clobbered.
    ok("i = 99\nxs = [i for i in 0..3]\nprint(i)\n");
    ok("xs = [i * 2 for i in 0..5 if i > 1]\n");
    ok("i = \"a\"\nxs = [i for i in 0..3]\nprint(i)\n");
    // A filter must be a Bool, and the source must be iterable.
    assert!(!err("xs = [i for i in 0..3 if i]\n").is_empty());
    assert!(!err("xs = [i for i in 5]\n").is_empty());
}

#[test]
fn slice_bounds_must_be_int() {
    ok("xs = [1, 2, 3]\nprint(xs[1:2])\nprint(xs[:2])\nprint(xs[::2])\n");
    // Slicing a string is legal and yields a string.
    ok("s = \"abc\"\nprint(s[0:1])\n");
    assert!(!err("xs = [1, 2, 3]\nprint(xs[1.5:])\n").is_empty());
    assert!(!err("print(5[0:1])\n").is_empty());
}

#[test]
fn range_bounds_must_be_int() {
    ok("for i in 0..5:\n    print(i)\n");
    ok("xs = [i for i in 0..5]\n");
    ok("xs = 0..5\n");
    assert!(!err("for i in 0.0..5:\n    print(i)\n").is_empty());
}

#[test]
fn assert_and_del_type_check() {
    ok("assert 1 < 2\nassert 1 < 2, \"nope\"\n");
    assert!(!err("assert 1\n").is_empty());
    assert!(!err("assert true, 5\n").is_empty());
    ok("xs = [1, 2]\ndel xs[0]\n");
    assert!(!err("s = \"ab\"\ndel s[0]\n").is_empty());
    assert!(!err("del never_defined\n").is_empty());
    // `del` on a name unbinds it, so any later use is undefined --
    // which is what makes the runtime's rebind-to-None unobservable:
    // no checked program can read the slot afterwards.
    ok("x = 1\ndel x\n");
    assert!(!err("x = 1\ndel x\nprint(x)\n").is_empty());
}

/// A push into a `List(?)` pins the element type, so a list built by
/// pushing is as precisely typed as a literal. Without this, the only
/// way to grow a list would leave every such list unresolved.
#[test]
fn push_pins_list_element_type() {
    ok("xs = []\npush(xs, 1)\ny = xs[0] + 1\n");
    ok("xs = []\npush(xs, 1.5)\n");
    // ...but monomorphism still holds: a second, incompatible push is
    // refused, the same as a mixed literal would be.
    assert!(!err("xs = []\npush(xs, 1)\npush(xs, \"a\")\n").is_empty());
    assert!(!err("xs = [1]\npush(xs, \"a\")\n").is_empty());
}

#[test]
fn iterating_a_dict_yields_keys() {
    ok("d = {\"a\": 1}\nfor k in d:\n    print(k)\n");
    // A dict's keys may be any mix of scalars, so the loop variable
    // is unresolved rather than guessed at.
    ok("d = {\"a\": 1}\nfor k in d:\n    k = 5\n    print(k)\n");
    assert!(!err("for k in 5:\n    print(k)\n").is_empty());
}

/// Strings are immutable, so assigning into one is refused even
/// though indexing a string is fine.
#[test]
fn strings_are_immutable() {
    let es = err("s = \"ab\"\ns[0] = \"z\"\n");
    assert!(es.iter().any(|e| e.message.contains("immutable")), "{es:?}");
}

// ---- records ----

#[test]
fn record_declaration_and_use() {
    ok("type Point:\n    x: Float\n    y: Float\np = Point(1.0, 2.0)\nprint(p.x)\n");
    // The field type as written is what a read produces.
    ok("type P:\n    n: Int\np = P(1)\nq = p.n + 1\n");
    // A field may be left unresolved and pinned by use instead.
    ok("type P:\n    n\np = P(1)\nq = p.n + 1\n");
}

#[test]
fn record_constructor_arity_is_exact() {
    let es = err("type Point:\n    x: Int\n    y: Int\np = Point(1)\n");
    assert!(
        es.iter().any(|e| e.message.contains("takes 2 fields")),
        "{es:?}"
    );
    assert!(!err("type Point:\n    x: Int\np = Point(1, 2)\n").is_empty());
}

#[test]
fn record_field_types_are_checked() {
    let es = err("type Point:\n    x: Float\np = Point(1)\n");
    assert!(es.iter().any(|e| e.message.contains("is Float")), "{es:?}");
}

#[test]
fn record_field_access_is_checked() {
    let es = err("type Point:\n    x: Int\np = Point(1)\nprint(p.z)\n");
    assert!(
        es.iter().any(|e| e.message.contains("no field 'z'")),
        "{es:?}"
    );
    let es = err("type Point:\n    x: Int\np = Point(1)\np.z = 2\n");
    assert!(
        es.iter().any(|e| e.message.contains("no field 'z'")),
        "{es:?}"
    );
    // A scalar is not something with fields.
    assert!(!err("n = 1\nprint(n.x)\n").is_empty());
}

#[test]
fn record_field_write_checks_type() {
    ok("type Point:\n    x: Int\np = Point(1)\np.x = 2\n");
    let es = err("type Point:\n    x: Int\np = Point(1)\np.x = \"a\"\n");
    assert!(
        es.iter().any(|e| e.message.contains("cannot assign")),
        "{es:?}"
    );
}

#[test]
fn record_declaration_rules() {
    // Module-level only: a second layout for the same name inside a
    // function is not something a static type can express.
    assert!(!err("fn f():\n    type P:\n        x: Int\n    return 1\n").is_empty());
    assert!(!err("type P:\n    x: Int\ntype P:\n    y: Int\n").is_empty());
    // A type cannot share a name with a function, since both would be
    // written `P(...)`.
    assert!(!err("fn P():\n    return 1\ntype P:\n    x: Int\n").is_empty());
    assert!(!err("type P:\n    x: Nope\n").is_empty());
    assert!(!err("type P:\n").is_empty());
}

/// Field types resolve lazily, so order does not matter: a field may
/// name a record declared later, and mutually recursive pairs work.
/// Only genuinely unknown names are reported, once the module has
/// been fully seen.
#[test]
fn record_field_types_resolve_lazily() {
    ok("type A:\n    b: B\ntype B:\n    n: Int\na = A(B(1))\nprint(a.b.n)\n");
    ok("type A:\n    b: B\ntype B:\n    a: A\n");
    ok("type P:\n    xs: List\n    d: Dict\n    u: Any\n");
    let es = err("type P:\n    x: Nope\n");
    assert!(
        es.iter()
            .any(|e| e.message.contains("unknown field type 'Nope'")),
        "{es:?}"
    );
}

#[test]
fn unknown_type_is_an_error() {
    assert!(!err("p = Nope(1)\n").is_empty());
    // A function is not a constructor.
    assert!(!err("fn f():\n    return 1\np = f(1)\n").is_empty());
}

#[test]
fn basic_program_passes() {
    ok("x = 1\ny = x + 2.5\nprint(x, y)\n");
}

#[test]
fn rebind_different_type_errors() {
    let es = err("x = \"a\"\nx = 1\n");
    assert!(es.iter().any(|e| e.message.contains("cannot rebind")));
}

#[test]
fn undefined_var_errors() {
    assert!(!err("print(y)\n").is_empty());
}

#[test]
fn arith_mismatch_errors() {
    assert!(!err("x = 1 + \"a\"\n").is_empty());
}

#[test]
fn non_bool_condition_errors() {
    assert!(!err("if 1:\n    print(1)\n").is_empty());
}

#[test]
fn call_arity_errors() {
    assert!(!err("fn f(a):\n    return a\nprint(f(1, 2))\n").is_empty());
}

#[test]
fn len_push_checked() {
    ok("a = [1]\npush(a, 2)\nprint(len(a))\n");
    assert!(!err("print(len(1))\n").is_empty());
    assert!(!err("a = 1\npush(a, 2)\n").is_empty());
}

#[test]
fn input_checked() {
    // No prompt, a string prompt and a numeric prompt all answer
    // Str. Only the count is still capped.
    ok("name = input()\nprint(name)\n");
    ok("name = input(\"who: \")\nprint(name)\n");
    ok("n = input(5)\nprint(n)\n");
    ok("n = input(2.5)\nprint(n)\n");
    assert!(!err("x = input(1, 2)\n").is_empty());
    assert!(!err("x = input(true)\n").is_empty());
    let m = infer("x = input()\n");
    assert_eq!(m[&("__main__".into(), "<top>".into())].locals["x"], Ty::Str);
}

#[test]
fn int_float_conversion_checked() {
    // Identity, truncation source and string source all answer the
    // target type; anything else is rejected at check time, while a
    // bad string fails at runtime (tested natively).
    ok("a = int(1)\nprint(a)\n");
    ok("a = int(1.9)\nprint(a)\n");
    ok("a = int(\"-7\")\nprint(a)\n");
    ok("a = float(1)\nprint(a)\n");
    ok("a = float(1.5)\nprint(a)\n");
    ok("a = float(\"2.5\")\nprint(a)\n");
    assert!(!err("a = int()\n").is_empty());
    assert!(!err("a = int(1, 2)\n").is_empty());
    assert!(!err("a = float()\n").is_empty());
    assert!(!err("a = int(true)\n").is_empty());
    assert!(!err("a = float([1])\n").is_empty());
    assert!(!err("a = int(None)\n").is_empty());
    let m = infer("a = int(\"3\")\nb = float(2)\n");
    let top = &m[&("__main__".into(), "<top>".into())].locals;
    assert_eq!(top["a"], Ty::Int);
    assert_eq!(top["b"], Ty::Float);
}

#[test]
fn return_consistency() {
    ok("fn f(n):\n    if n:\n        return 1\n    else:\n        return 2\n");
    assert!(
        !err("fn f(n):\n    if n:\n        return 1\n    else:\n        return \"a\"\n").is_empty()
    );
}

#[test]
fn break_outside_errors() {
    assert!(!err("break\n").is_empty());
}

#[test]
fn parallel_blocks_are_gone() {
    // `parallel:` was removed, not deprecated. It asked the scheduler to
    // prove race-freedom statically and the proof had holes, so a program
    // could print the wrong answer whenever it lost a race. The block is
    // now a syntax error, and `parallel` is an ordinary identifier.
    assert!(!err("parallel:\n    a = 1\n").is_empty());
    assert!(!err("fn f():\n    x = 1\n    parallel:\n        x = 2\n").is_empty());
    ok("parallel = 1\nprint(parallel)\n");
}

#[test]
fn list_indexing() {
    ok("a = [1, 2]\nprint(a[0])\n");
    assert!(!err("a = 1\nprint(a[0])\n").is_empty());
}

fn infer(src: &str) -> HashMap<(String, String), FnInfo> {
    let tokens = nx_lexer::lex(src).unwrap();
    let prog = nx_parser::parse(tokens).unwrap();
    infer_program(&prog, std::path::Path::new(".")).unwrap()
}

#[test]
fn infer_program_for_keys_by_module() {
    // The backend asks once per loaded module: inference for `utils`
    // must file under `utils`, not `__main__`, or method calls on
    // locals inside imported modules miss dispatch.
    let tokens = nx_lexer::lex("fn f(n):\n    return n\n").unwrap();
    let prog = nx_parser::parse(tokens).unwrap();
    let m = infer_program_for(&prog, std::path::Path::new("."), "utils").unwrap();
    assert!(m.contains_key(&("utils".into(), "f".into())));
    assert!(m.contains_key(&("utils".into(), "<top>".into())));
    assert!(!m.keys().any(|(md, _)| md == "__main__"));
}

#[test]
fn param_int_from_arithmetic() {
    let m = infer("fn fib(n):\n    if n <= 1:\n        return n\n    else:\n        return fib(n - 1) + fib(n - 2)\n");
    assert_eq!(m[&("__main__".into(), "fib".into())].locals["n"], Ty::Int);
}

#[test]
fn function_sees_module_globals() {
    // Codegen resolves a top-level name to a global, so the checker has
    // to see it too. It used to empty the enclosing scope and report
    // every module-level constant as undefined inside a function.
    ok("g = 4\nfn f(k):\n    return g + k\nprint(f(1))\n");
    let m = infer("g = 4\nfn f(k):\n    return g + k\n");
    let f = &m[&("__main__".into(), "f".into())];
    assert_eq!(f.locals["g"], Ty::Int);
}

#[test]
fn numeric_param_defaults_to_int_in_an_int_function() {
    // Nothing in the body is a Float, so Int is the only reading left.
    let m = infer("fn f(n):\n    return n * 2\n");
    assert_eq!(m[&("__main__".into(), "f".into())].locals["n"], Ty::Int);
    ok("fn f(n):\n    return n * 2\nprint(f(21))\n");
}

#[test]
fn numeric_param_stays_dynamic_in_a_float_function() {
    // `x / 2.0` is well-typed for an Int x and for a Float x, so the
    // parameter is dynamic. Pinning it made this function reject a
    // Float argument, which is ordinary code failing to compile.
    let m = infer("fn half(x):\n    return x / 2.0\n");
    assert_eq!(
        m[&("__main__".into(), "half".into())].locals["x"],
        Ty::Unknown
    );
    ok("fn half(x):\n    return x / 2.0\nprint(half(1))\n");
    ok("fn half(x):\n    return x / 2.0\nprint(half(1.0))\n");
}

#[test]
fn float_function_takes_float_coordinates() {
    // The regression that motivated the rule: this used to infer
    // (Int, Int, Int) and refuse Float arguments.
    ok("fn mandel(cx, cy, maxiter):\n    zr = 0.0\n    zi = 0.0\n    i = 0\n    while i < maxiter:\n        zr2 = zr * zr\n        zi2 = zi * zi\n        if zr2 + zi2 > 4.0:\n            return i\n        zi = 2.0 * zr * zi + cy\n        zr = zr2 - zi2 + cx\n        i = i + 1\n    return maxiter\nprint(mandel(0.5, 0.5, 10))\n");
}

#[test]
fn comparison_against_float_pins_param_to_float() {
    // An ordering comparison has no widening, so the type is forced.
    let m = infer("fn over(x):\n    if x < 1.5:\n        return 1\n    else:\n        return 0\n");
    assert_eq!(
        m[&("__main__".into(), "over".into())].locals["x"],
        Ty::Float
    );
}

#[test]
fn param_bool_from_condition() {
    let m = infer("fn neg(b):\n    if b:\n        return 1\n    else:\n        return 0\n");
    assert_eq!(m[&("__main__".into(), "neg".into())].locals["b"], Ty::Bool);
}

#[test]
fn param_unpinned_for_unresolved_base() {
    // An unresolved base may hold a list or a dict, so the index is
    // left unresolved: pinning Int here would reject `at(d, "k")`
    // for a dict `d`, which the runtime handles fine.
    let m = infer("fn at(xs, i):\n    return xs[i]\n");
    assert_eq!(
        m[&("__main__".into(), "at".into())].locals["i"],
        Ty::Unknown
    );
}

#[test]
fn param_int_from_known_list_index() {
    // A statically known list still pins its index to Int.
    let m = infer("fn at(i):\n    xs = [1, 2, 3]\n    return xs[i]\n");
    assert_eq!(m[&("__main__".into(), "at".into())].locals["i"], Ty::Int);
}

#[test]
fn unused_param_stays_unknown() {
    let m = infer("fn id(x):\n    return 1\n");
    assert_eq!(
        m[&("__main__".into(), "id".into())].locals["x"],
        Ty::Unknown
    );
}

#[test]
fn inferred_param_rejects_wrong_argument() {
    // x is proven Int by `x - 1`, so passing a string is an error.
    assert!(!err("fn f(x):\n    return x - 1\nprint(f(\"a\"))\n").is_empty());
}

#[test]
fn top_level_scope_is_reported() {
    let m = infer("a = 1\nb = \"s\"\n");
    let top = &m[&("__main__".into(), "<top>".into())];
    assert_eq!(top.locals["a"], Ty::Int);
    assert_eq!(top.locals["b"], Ty::Str);
}

#[test]
fn unknown_assignment_widens_variable() {
    // t is Int from its first binding, but the loop element is
    // untyped, so t must widen: the backend gives a known scalar a
    // typed slot and a narrower type would be unsound.
    let m = infer("fn f(xs):\n    t = 0\n    for x in xs:\n        t = t + x\n    return t\n");
    let f = &m[&("__main__".into(), "f".into())];
    assert_eq!(f.locals["t"], Ty::Unknown);
}

#[test]
fn widening_is_sticky() {
    // Once widened, a later known assignment must not re-narrow.
    let m = infer(
        "fn f(xs):\n    t = 0\n    for x in xs:\n        t = t + x\n    t = 5\n    return t\n",
    );
    assert_eq!(m[&("__main__".into(), "f".into())].locals["t"], Ty::Unknown);
}

// --- impl blocks and methods ------------------------------------

const POINT: &str = "type P:\n    x: Int\n    y: Int\n";

#[test]
fn method_return_types_are_inferred() {
    let m = infer(&format!(
            "{POINT}impl P:\n    fn area(self):\n        return self.x * self.y\n    fn zero():\n        return P(0, 0)\n"
        ));
    // Methods report under `Type.method`, matching what codegen looks
    // up, so the two can never disagree about which name a body has.
    assert_eq!(m[&("__main__".into(), "P.area".into())].ret, Ty::Int);
    assert_eq!(
        m[&("__main__".into(), "P.zero".into())].ret,
        Ty::Record("P".into())
    );
}

#[test]
fn mut_self_binds_self_to_the_record() {
    let m = infer(&format!(
            "{POINT}impl P:\n    fn moved(mut self, d):\n        self.x = self.x + d\n        return self\n"
        ));
    let f = &m[&("__main__".into(), "P.moved".into())];
    assert_eq!(f.locals["self"], Ty::Record("P".into()));
    // `d` is Int: the body only ever adds it to an Int field.
    assert_eq!(f.params, vec!["d".to_string()]);
}

#[test]
fn writing_through_read_only_self_is_an_error() {
    // `self` is a copy, so a write through it would be discarded. The
    // compiler says so rather than letting it look meaningful.
    let e = err(&format!(
        "{POINT}impl P:\n    fn bad(self):\n        self.x = 1\n        return self\n"
    ));
    assert!(
        e.iter().any(|m| m.message.contains("read-only 'self'")),
        "expected a read-only self error, got {e:?}"
    );
}

#[test]
fn writing_through_mut_self_is_allowed() {
    ok(&format!(
        "{POINT}impl P:\n    fn ok(mut self):\n        self.x = 1\n        return self\n"
    ));
}

#[test]
fn mut_self_must_return_the_record() {
    // The result is written back into the receiver, so anything other
    // than the record would clobber it with the wrong type.
    let e = err(&format!(
        "{POINT}impl P:\n    fn bad(mut self):\n        self.x = 1\n        return 7\n"
    ));
    assert!(
        e.iter().any(|m| m.message.contains("must return 'P'")),
        "expected a return-type error, got {e:?}"
    );
}

#[test]
fn impl_of_unknown_type_is_an_error() {
    let e = err("impl Nope:\n    fn a(self):\n        return 1\n");
    assert!(
        e.iter().any(|m| m.message.contains("unknown type 'Nope'")),
        "expected an unknown-type error, got {e:?}"
    );
}

#[test]
fn duplicate_method_is_an_error() {
    let e = err(&format!(
        "{POINT}impl P:\n    fn a(self):\n        return 1\n    fn a(self):\n        return 2\n"
    ));
    assert!(
        e.iter().any(|m| m.message.contains("duplicate method")),
        "expected a duplicate error, got {e:?}"
    );
}

#[test]
fn unknown_method_on_a_known_record_is_an_error() {
    let e = err(&format!("{POINT}p = P(1, 2)\nprint(p.nope())\n"));
    assert!(
        e.iter().any(|m| m.message.contains("has no method 'nope'")),
        "expected an unknown-method error, got {e:?}"
    );
}

#[test]
fn method_needs_a_receiver_but_associated_function_does_not() {
    // A method on the type name is a mistake worth naming.
    let e = err(&format!(
        "{POINT}impl P:\n    fn area(self):\n        return self.x\np = P(1, 2)\nprint(P.area())\n"
    ));
    assert!(
        e.iter().any(|m| m.message.contains("needs a receiver")),
        "expected a needs-a-receiver error, got {e:?}"
    );
    // An associated function takes no receiver, so a value of the type
    // is as good a base as the type itself: there is nothing to read
    // off it, and refusing the call would only add a rule.
    ok(&format!(
            "{POINT}impl P:\n    fn zero():\n        return P(0, 0)\np = P(1, 2)\nprint(p.zero())\nprint(P.zero())\n"
        ));
}

/// A type may define a method whose name shadows an ambient builtin.
/// Methods resolve before sugar, so the type's meaning wins.
#[test]
fn a_method_may_shadow_a_builtin() {
    ok(&format!(
            "{POINT}impl P:\n    fn push(self, v):\n        return self.x + v\np = P(1, 2)\nprint(p.push(4))\n"
        ));
}

#[test]
fn method_arity_is_checked() {
    let e = err(&format!(
            "{POINT}impl P:\n    fn scaled(self, k):\n        return self.x * k\np = P(1, 2)\nprint(p.scaled())\n"
        ));
    assert!(!e.is_empty(), "expected an arity error, got none");
}

/// A method on a type this module imported is an orphan impl: the
/// layout belongs to another module, so the two could disagree about
/// what the fields mean.
#[test]
fn impl_on_an_imported_type_is_refused() {
    let dir = std::env::temp_dir().join("nx_orphan_impl");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("shapes.nx"), "type Sq:\n    side: Int\n").unwrap();
    let src = dir.join("main.nx");
    std::fs::write(
        &src,
        "from shapes import Sq\nimpl Sq:\n    fn area(self):\n        return self.side\n",
    )
    .unwrap();
    let toks = nx_lexer::lex(&std::fs::read_to_string(&src).unwrap()).unwrap();
    let prog = nx_parser::parse(toks).unwrap();
    let e = check_program(&prog, &dir).unwrap_err();
    assert!(
        e.iter().any(|m| m.message.contains("another module")),
        "expected an orphan-impl error, got {e:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A module that fails to load reports once, at the import, and
/// every name it was asked for goes quiet: the load error stands
/// alone instead of dragging an "undefined variable" cascade behind
/// it. Sixteen diagnostics for one self-import used to be the norm;
/// this test pins the exact list so the cascade cannot creep back.
#[test]
fn failed_import_reports_once_and_poisoned_uses_stay_silent() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nx_failed_import_{}_{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // A file importing itself: the load fails circularly, and every
    // use below would cascade without the suppression.
    std::fs::write(
            dir.join("test.nx"),
            "import test\nfrom test import double as do, greet as hello\n\nprint(test.VERSION)\nprint(do(21))\nprint(do(4))\nprint(hello(\"Ada\"))\n\nfor i in 0..3:\n    print(test.double(i))\n",
        )
        .unwrap();
    let src = std::fs::read_to_string(dir.join("test.nx")).unwrap();
    let toks = nx_lexer::lex(&src).unwrap();
    let prog = nx_parser::parse(toks).unwrap();
    let es = check_program(&prog, &dir).unwrap_err();
    let got: Vec<String> = es
        .iter()
        .map(|e| format!("{}:{}: {}", e.line, e.col, e.message))
        .collect();
    assert_eq!(
        got,
        vec![
            "1:1: circular import of 'test'".to_string(),
            "1:1: module 'test' has errors".to_string(),
        ],
        "a failed import must report its root causes and nothing else, got {got:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A missing module behaves the same way: one load error, and the
/// names it would have provided check as Unknown instead of
/// erroring again at every use.
#[test]
fn missing_module_reports_once_and_poisoned_uses_stay_silent() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ2: AtomicU64 = AtomicU64::new(0);
    let n = SEQ2.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nx_missing_import_{}_{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = "import nosuchmodule\nfrom nosuchmodule import thing\nprint(nosuchmodule.f())\nprint(thing(1))\n";
    let toks = nx_lexer::lex(src).unwrap();
    let prog = nx_parser::parse(toks).unwrap();
    let es = check_program(&prog, &dir).unwrap_err();
    let got: Vec<String> = es
        .iter()
        .map(|e| format!("{}:{}: {}", e.line, e.col, e.message))
        .collect();
    assert_eq!(
        got,
        vec!["1:1: cannot find module 'nosuchmodule.nx'".to_string()],
        "a missing module must report once, got {got:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The suppression is scoped to failed loads: a module that loads
/// fine but lacks a member still errors, and a dynamic (merely
/// unknown) receiver still errors by the Stage 4 rule. Each guard
/// below would go quiet if the poison leaked past its boundary.
#[test]
fn healthy_modules_and_dynamic_receivers_still_error() {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ3: AtomicU64 = AtomicU64::new(0);
    let n = SEQ3.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("nx_import_guards_{}_{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("utils.nx"), "VERSION = \"1.0\"\n").unwrap();
    // A member that is genuinely missing is a real error, not a cascade.
    let src = "from utils import nosuch\nprint(nosuch)\n";
    let toks = nx_lexer::lex(src).unwrap();
    let prog = nx_parser::parse(toks).unwrap();
    let es = check_program(&prog, &dir).unwrap_err();
    assert!(
        es.iter()
            .any(|e| e.message.contains("has no member 'nosuch'")),
        "a missing member of a healthy module must still error, got {es:?}"
    );
    // So is a method call on a merely-dynamic receiver (Stage 4 rule).
    let es = err("fn f(q):\n    return q.m()\n");
    assert!(
        es.iter().any(|e| e.message.contains("dynamic dispatch")),
        "a dynamic receiver must still error, got {es:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
/// by one spelling is pinned by the other -- and a mismatch is caught
/// through the sugar exactly as it is through the direct call.
#[test]
fn builtin_sugar_pins_the_element_type() {
    let m = infer("xs = []\nxs.push(1)\nxs.push(2)\n");
    let top = &m[&("__main__".into(), "<top>".into())];
    assert_eq!(
        top.locals["xs"],
        Ty::List(Box::new(Ty::Int)),
        "the first push pins the element type"
    );
    let e = err("xs = []\nxs.push(1)\nxs.push(\"s\")\n");
    assert!(
        e.iter()
            .any(|m| m.message.contains("element type mismatch")),
        "expected a pinned-element error, got {e:?}"
    );
}

#[test]
fn builtin_sugar_rejects_a_non_list_base() {
    let e = err("x = 5\nx.push(1)\n");
    assert!(
        e.iter().any(|m| m.message.contains("push() needs a list")),
        "expected a push type error, got {e:?}"
    );
}
