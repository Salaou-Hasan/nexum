//! Tests for the memory plan: scalar promotion beats escape and aliasing,
//! and every non-scalar escape stays Shared.

use super::*;
use std::collections::HashMap;

fn plan_src(src: &str) -> Plan {
    plan_with(src, &HashMap::new())
}

/// Plan with the checker's inferred types, which is what promotes
/// scalars to `Stack`.
fn plan_with(src: &str, types: &HashMap<(String, String), nx_types::FnInfo>) -> Plan {
    let prog = nx_parser::parse_source(src).unwrap_or_else(|e| panic!("{e}"));
    let mut map = HashMap::new();
    map.insert("__main__".to_string(), prog);
    plan(&map, "__main__", types)
}

fn infer(src: &str) -> HashMap<(String, String), nx_types::FnInfo> {
    let prog = nx_parser::parse_source(src).unwrap_or_else(|e| panic!("{e}"));
    nx_types::infer_program(&prog, std::path::Path::new(".")).unwrap_or_else(|es| panic!("{es:?}"))
}

#[test]
fn scalar_local_is_stack() {
    let src = "fn f(n):\n    t = n * 2\n    print(t)\n";
    let types = infer(src);
    let p = plan_with(src, &types);
    assert_eq!(p.alloc_of("__main__", "f", "t"), Alloc::Stack);
}

#[test]
fn list_local_is_not_stack() {
    let src = "fn f():\n    a = [1]\n    print(a)\n";
    let types = infer(src);
    let p = plan_with(src, &types);
    assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Unique);
}

#[test]
fn stack_beats_escape() {
    // Returning a scalar copies the number, so it cannot dangle.
    let src = "fn f(n):\n    t = n * 2\n    return t + 1\n";
    let types = infer(src);
    let p = plan_with(src, &types);
    assert_eq!(p.alloc_of("__main__", "f", "t"), Alloc::Stack);
}

#[test]
fn stack_beats_aliasing() {
    // `a` and `b` are both Int, and a scalar copy is a number rather
    // than a second reference to one buffer, so neither dangles.
    let src = "fn f(n):\n    a = n * 1\n    b = a\n    print(b)\n";
    let types = infer(src);
    let p = plan_with(src, &types);
    assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Stack);
    assert_eq!(p.alloc_of("__main__", "f", "b"), Alloc::Stack);
}

#[test]
fn list_copy_is_not_stack() {
    // The same shape with a list must still be Shared: the copy shares
    // a buffer, so freeing it would be a double free.
    let src = "fn f():\n    a = [1]\n    b = a\n    print(b)\n";
    let types = infer(src);
    let p = plan_with(src, &types);
    assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Shared);
    assert_eq!(p.alloc_of("__main__", "f", "b"), Alloc::Shared);
}

#[test]
fn unknown_typed_local_is_not_stack() {
    // Without type information nothing may be promoted: doubt is Shared.
    let src = "fn f(n):\n    t = n * 2\n    print(t)\n";
    let p = plan_src(src);
    assert_eq!(p.alloc_of("__main__", "f", "t"), Alloc::Unique);
}

#[test]
fn float_and_bool_locals_are_stack() {
    let src = "fn f(n):\n    a = n / 2.0\n    b = n < 1\n    print(a, b)\n";
    let types = infer(src);
    let p = plan_with(src, &types);
    assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Stack);
    assert_eq!(p.alloc_of("__main__", "f", "b"), Alloc::Stack);
}

#[test]
fn local_temp_is_unique() {
    let p = plan_src("fn f(n):\n    t = n * 2\n    return t + 1\n");
    assert_eq!(p.alloc_of("__main__", "f", "t"), Alloc::Shared); // returned transitively
    assert_eq!(p.alloc_of("__main__", "f", "n"), Alloc::Shared); // param
}

#[test]
fn pure_temp_freed() {
    let p = plan_src("fn f(n):\n    t = n * 2\n    print(t)\n");
    assert_eq!(p.alloc_of("__main__", "f", "t"), Alloc::Unique);
}

#[test]
fn pushed_list_shared() {
    let p = plan_src("fn f():\n    a = [1]\n    push(a, 2)\n    return a\n");
    assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Shared);
}

#[test]
fn alias_shared() {
    let p = plan_src("fn f():\n    a = [1]\n    b = a\n    print(b)\n");
    assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Shared);
    assert_eq!(p.alloc_of("__main__", "f", "b"), Alloc::Shared);
}

#[test]
fn retaining_fn_poisons_caller() {
    let p = plan_src("fn keep(x):\n    g = [x]\n    return g\nfn f():\n    a = [1]\n    b = keep(a)\n    print(b)\n");
    assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Shared);
}

#[test]
fn var_held_only_inside_a_dict_literal_escapes() {
    // `a` reaches the caller through `d`, so it must be Shared: a
    // Unique `a` would be freed while `d` still holds it. `vars_in`
    // used to drop dict contents entirely.
    let p = plan_src("fn f():\n    a = [1]\n    d = {\"k\": a}\n    return d\n");
    assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Shared);
    assert_eq!(p.alloc_of("__main__", "f", "d"), Alloc::Shared);
}

#[test]
fn var_held_only_inside_a_comprehension_escapes() {
    let p = plan_src("fn f():\n    a = [1, 2]\n    b = [x for x in a]\n    return b\n");
    assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Shared);
    assert_eq!(p.alloc_of("__main__", "f", "b"), Alloc::Shared);
}

#[test]
fn var_in_slice_bounds_and_ifexpr_branches_escapes() {
    let p = plan_src("fn f(n):\n    a = [1, 2, 3]\n    b = a[n:2] if n else a\n    return b\n");
    assert_eq!(p.alloc_of("__main__", "f", "a"), Alloc::Shared);
}

#[test]
fn prompt_var_is_not_retained_by_input() {
    // `input` reads the prompt's bytes and keeps nothing. As a bare
    // statement the call can only escape through `expr_roots`, which
    // excludes it -- so a local prompt stays a Unique temporary
    // rather than escaping to Shared.
    let p = plan_src("fn f():\n    p = \"who: \"\n    input(p)\n    print(\"done\")\n");
    assert_eq!(p.alloc_of("__main__", "f", "p"), Alloc::Unique);
}
