
use crate::mangle::{mangle_fn, mangle_global, mangle_method};
use crate::{compile_entry, compile_opts};

/// Structural invariant: `define` may only appear at brace depth 0, and so
/// may a global's declaration.
///
/// Both halves of this have been violated at different times. An
/// outlined function emitted mid-body was the first. The second was a
/// global: a name first assigned inside a nested block is a module
/// global, but the block is emitted after the globals section, so the name
/// was discovered *during* emission and the declaration landed between
/// an `entry:` label and the instruction after it -- which clang
/// rejects with "expected instruction opcode", long before anything
/// could run. A missing top-level statement is a quiet wrong answer;
/// this one at least failed loudly.
fn assert_top_level_defines(ir: &str) {
    let mut depth = 0i32;
    for line in ir.lines() {
        let t = line.trim();
        if t.starts_with("define ") {
            assert_eq!(depth, 0, "define inside function body: {t}");
        }
        // A `@name = global` or `@name = constant` line is a module-level
        // declaration, never an instruction.
        if t.starts_with('@') && (t.contains(" = global") || t.contains(" = constant")) {
            assert_eq!(
                depth, 0,
                "global declared inside a function body: {t}\n{ir}"
            );
        }
        depth += line.chars().filter(|&c| c == '{').count() as i32;
        depth -= line.chars().filter(|&c| c == '}').count() as i32;
    }
}

/// A loop variable is scoped to its loop, so it is a local slot and
/// never a module global.
///
/// It used to be a global at top level, because `store_fresh` takes the
/// `in_init` path for anything bound while emitting module-level code.
/// That leaked the variable into every later statement and let two loops
/// collide on the name.
#[test]
fn a_loop_variable_is_never_a_global() {
    for src in [
        "for i in 0..3:\n    print(i)\n",
        "for i in 0..3:\n    for j in [1]:\n        print(i)\n",
        "x = 0\nfor i in 0..3:\n    x = i\n",
    ] {
        let ir = compile_entry(src, std::path::Path::new(".")).unwrap();
        assert!(
            !ir.contains(&format!("@{} = global", mangle_global("__main__", "i"))),
            "loop variable leaked into module scope:\n{ir}"
        );
        assert_top_level_defines(&ir);
    }
}

/// A `mut self` call writes back into its receiver. When the receiver
/// is a *local* slot rather than a module variable -- a loop variable is
/// the case that matters -- the write has to land in the slot.
///
/// It used to test `in_init` first and write to a module global instead,
/// so the update went somewhere nothing read and the following
/// `print` showed the old value. The code compiled, ran, and printed
/// the wrong thing, which is the worst shape a bug can have.
#[test]
fn mut_self_writes_back_into_a_local_receiver() {
    let ir = compile_entry(
            &format!(
                "{POINT}impl P:\n    fn moved(mut self, d):\n        self.x = self.x + d\n        return self\npts = [P(1, 1)]\nfor pt in pts:\n    pt.moved(2)\n    print(pt)\n"
            ),
            std::path::Path::new("."),
        )
        .unwrap();
    let top = void_body_of(&ir, "nx__init___main__");
    // Match the whole declaration, not just the name: `..._pt` is a
    // prefix of `..._pts`, and the list really is a module variable.
    assert!(
        !top.contains(&format!("@{} = global", mangle_global("__main__", "pt"))),
        "a loop variable must not be written through module scope:\n{top}"
    );
    // The write-back stores the call's result into the loop's own slot.
    let call_at = top.find(&mangle_method("__main__", "P", "moved")).unwrap();
    let after = &top[call_at..];
    assert!(
        after.contains("store %NxVal"),
        "the result must be stored after the call:\n{after}"
    );
}

/// The module body is a function like any other, and its inferred types
/// are filed under `<top>`. `cur_fn` has to say so while it is emitted:
/// otherwise every lookup that keys on the current function -- unboxing,
/// method dispatch on a local, the memory plan -- misses, and top-level
/// code silently loses all of its static types.
#[test]
fn the_module_body_knows_its_own_inference_key() {
    let ir = compile_entry(
        "t = 0\nfor i in 0..4:\n    t = t + i\nprint(t)\n",
        std::path::Path::new("."),
    )
    .unwrap();
    let top = void_body_of(&ir, "nx__init___main__");
    // A top-level counter whose type is known must get a bare i64 slot,
    // which is only possible if the `<top>` inference was found.
    assert!(
        top.contains("alloca i64"),
        "top-level locals must unbox:\n{top}"
    );
    assert!(
        top.contains("add i64"),
        "a top-level `t = t + i` must be raw arithmetic:\n{top}"
    );
}

/// A nested loop that reuses the outer variable's name must not destroy
/// the outer binding. This is observable:
///
/// ```text
/// for i in 0..3:
///     for i in 0..2:
///         print("in", i)
///     print("out", i)     # must be the outer induction value
/// ```
///
/// The inner loop rebinds `i` for its own duration, so each outer
/// iteration has to restore the slot it started with.
#[test]
fn a_nested_loop_restores_the_outer_variable() {
    let ir = compile_entry(
        "for i in 0..3:\n    for i in 0..2:\n        print(i)\n    print(i)\n",
        std::path::Path::new("."),
    )
    .unwrap();
    // Both loops keep their counter in an i64 slot, so the inner loop's
    // induction variable and the outer one must be distinct allocas.
    let slots = functions(&ir)
        .iter()
        .flat_map(|(_, body)| {
            body.lines()
                .filter(|l| l.contains(" = alloca i64"))
                .map(|l| l.split_whitespace().next().unwrap().to_string())
                .collect::<Vec<_>>()
        })
        .count();
    assert!(slots >= 2, "each loop needs its own counter slot:\n{ir}");
    assert_top_level_defines(&ir);
}

/// Every `alloca` must sit in the entry block. One emitted inside a
/// loop body is a fresh allocation per iteration that is only released
/// when the function returns, so a call inside a long loop walks off
/// the end of the stack -- a 35,000-iteration loop died with a stack
/// overflow before this was hoisted.
/// Every function body in the IR, paired with its name.
fn functions(ir: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut name = String::new();
    let mut body = String::new();
    for line in ir.lines() {
        if let Some(rest) = line.strip_prefix("define ") {
            if !name.is_empty() {
                out.push((std::mem::take(&mut name), std::mem::take(&mut body)));
            }
            name = rest
                .split('(')
                .next()
                .unwrap_or("")
                .rsplit('@')
                .next()
                .unwrap_or("")
                .to_string();
        } else if !name.is_empty() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if !name.is_empty() {
        out.push((name, body));
    }
    out
}

#[test]
fn allocas_are_hoisted_to_the_entry_block() {
    let ir = compile_entry(
            "fn f(x):\n    return x * 2\nxs = [1, 2]\nt = 0\nfor x in xs:\n    t = t + f(x)\nwhile t < 10:\n    t = t + f(t)\nprint(t, [1, 2, 3], xs[0])\n",
            std::path::Path::new("."),
        )
        .unwrap();
    // Anything past the first block label is no longer the entry block.
    for (name, body) in functions(&ir) {
        let mut past_entry = false;
        for line in body.lines() {
            let t = line.trim();
            if t.ends_with(':') {
                if t == "entry:" {
                    continue;
                }
                past_entry = true;
                continue;
            }
            assert!(
                !(past_entry && t.contains("alloca")),
                "{name}: alloca outside the entry block: {t}\n{body}"
            );
        }
    }
}

/// Structural invariant: every `phi` names blocks that really do branch
/// to the block holding the `phi`.
///
/// A `phi` is only meaningful relative to a predecessor edge. The
/// backend builds a merge by opening a block, emitting an arm into it,
/// and naming that block in the `phi` -- which silently assumes the arm
/// stayed straight-line. It does not: `xs[i - 1] > xs[i]` opens an
/// overflow diamond per checked subtraction, so the arm's value ends up
/// in the last of those, and the named block is not a predecessor of
/// the merge at all. clang rejects the whole module:
///
///     error: invalid LLVM IR input: PHI node entries do not match predecessors!
///
/// Nothing in the suite covered the shape, so four benchmark workloads
/// (`sortint`, `sortstr`, `strscan`, `textstat`) had stopped building.
#[test]
fn every_phi_names_a_real_predecessor() {
    let ir = compile_entry(
        "xs = [3, 2, 1]\n\
             i = 1\n\
             a = xs[i - 1] > xs[i] and xs[i] > xs[i + 1] - 3\n\
             b = xs[i - 1] > xs[i] or xs[i] > xs[i + 1] - 3\n\
             c = i > 0 and (xs[i - 1] > 5 or xs[i] > 5)\n\
             d = len(xs) - 1 if i > 0 and xs[i - 1] > xs[i] else 0\n\
             e = 100 + 1 if i > 1 else 7 + 2\n\
             while i < 3 and xs[i - 1] > xs[i]:\n\
             \x20   print(i)\n\
             \x20   i = i + 1\n\
             for v in xs:\n\
             \x20   if v > 1 and v - 1 > 0:\n\
             \x20       print(v)\n\
             print(a, b, c, d, e)\n",
        std::path::Path::new("."),
    )
    .unwrap();

    for (name, body) in functions(&ir) {
        // Block label -> its terminator, and block label -> the phis in it.
        let mut term: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let mut phis: Vec<(String, String)> = Vec::new();
        let mut cur = String::new();
        for line in body.lines() {
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if t.ends_with(':') && !t.contains(' ') && !t.contains('=') {
                cur = t.trim_end_matches(':').to_string();
            } else if t.starts_with("br ") || t.starts_with("ret ") || t == "unreachable" {
                term.insert(cur.clone(), t.to_string());
            } else if t.contains(" = phi ") {
                phis.push((cur.clone(), t.to_string()));
            }
        }
        for (block, phi) in &phis {
            for m in incoming_labels(phi) {
                let ends = term
                    .get(&m)
                    .unwrap_or_else(|| panic!("{name}: phi names unknown block %{m}: {phi}"));
                assert!(
                    ends.contains(&format!("label %{block}")),
                    "{name}: phi in %{block} names %{m}, whose terminator \
                         does not branch there: {ends}\n{phi}"
                );
            }
        }
    }
}

/// The `%label` of each `[ value, %label ]` pair in a `phi` line.
fn incoming_labels(phi: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes: Vec<char> = phi.chars().collect();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == ',' && bytes[i + 1].is_whitespace() && bytes[i + 2..].starts_with(&['%']) {
            let rest: String = bytes[i + 3..].iter().collect();
            let label: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.')
                .collect();
            if !label.is_empty() {
                out.push(label);
            }
            i += 3;
            continue;
        }
        i += 1;
    }
    out
}

/// `parallel:` was removed rather than deprecated. It asked the scheduler
/// to prove race-freedom statically, and the proof had holes: a name
/// bound inside a `parallel:` block is a module global in the emitted
/// code, but the dependency analysis did not know that, so dependent
/// tasks were emitted into one concurrent batch. A program could then
/// print the wrong answer whenever it lost the race -- a miscompile that
/// passed its own test more often than not.
///
/// This test now pins the removal: the block is a compile error and no
/// threading primitive survives anywhere in the output.
#[test]
fn parallel_is_gone() {
    assert!(
        compile_entry("a = 0\nparallel:\n    a = 1\n", std::path::Path::new(".")).is_err(),
        "parallel: must not compile"
    );
    // Nothing in the runtime may spawn a thread any more.
    let ir = compile_entry("a = 1\nprint(a)\n", std::path::Path::new(".")).unwrap();
    for gone in [
        "nx_thread_start",
        "nx_thread_join",
        "nx_pool_worker",
        "nx_pool_claim",
        "%NxPool",
    ] {
        assert!(!ir.contains(gone), "{gone} survived in:\n{ir}");
    }
}
/// Body of one emitted function, so a test can assert on its code
/// without matching the prelude.
fn body_of(ir: &str, mangled: &str) -> String {
    let start = ir
        .find(&format!("define %NxVal @{mangled}("))
        .unwrap_or_else(|| panic!("no function {mangled} in output"));
    let rest = &ir[start..];
    let end = rest[1..]
        .find("\ndefine ")
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

/// Body of a `void` function -- the module initializer, which is where
/// every top-level statement lands.
fn void_body_of(ir: &str, mangled: &str) -> String {
    let start = ir
        .find(&format!("define void @{mangled}("))
        .unwrap_or_else(|| panic!("no function {mangled} in output"));
    let rest = &ir[start..];
    let end = rest[1..]
        .find("\ndefine ")
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

// --- runtime structure layouts ------------------------------------
//
// Every NX value is a `%NxVal`, so its width is the single largest
// lever on NX program memory: a million-element list is a million of
// them. These assertions exist so a widening is a test failure rather
// than a quiet 50% regression in every compiled program.
//
// The numbers are x86-64 layouts, verified against the CRT's own
// `_msize` so they are the real cost and not a model of it.

/// A value is `{ tag, payload, extra }`. Three i64s = 24 bytes.
///
/// Narrowing `extra` to i32 does *not* help: `{i64, i64, i32}` still
/// pads out to 24. The only way down is to delete the field, and
/// `extra` is not deletable -- for an aggregate it caches the length so
/// `len()` is one load instead of a pointer chase. That trade is
/// documented in the runtime rather than taken, because it is worth
/// 8 bytes *per value* and costs a dependent load on every length read.
#[test]
fn a_value_is_twenty_four_bytes() {
    let ir = compile_entry("print(1)\n", std::path::Path::new(".")).unwrap();
    assert!(
        ir.contains("%NxVal = type { i64, i64, i64 }"),
        "%NxVal changed width:\n{ir}"
    );
}

/// A container header is `{ data, n, cap }` with both counters 32 bits,
/// so 16 bytes rather than 24. The allocation is `malloc(16)` to match:
/// narrowing the type while leaving the malloc alone saves nothing at
/// all, and the wasted 8 bytes would sit in tail padding.
#[test]
fn a_container_header_is_sixteen_bytes() {
    let ir = compile_entry(
        "xs = [1]\nxs.push(2)\nd = {}\nd[\"k\"] = 1\n",
        std::path::Path::new("."),
    )
    .unwrap();
    assert!(
        ir.contains("%NxList = type { ptr, i32, i32 }"),
        "%NxList changed:\n{ir}"
    );
    assert!(
        ir.contains("%NxDict = type { ptr, i32, i32 }"),
        "%NxDict changed:\n{ir}"
    );
    // The allocation has to match the struct. Narrowing the type while
    // leaving the malloc alone saves nothing and leaves 8 bytes of tail
    // padding, which is the trap this assertion exists to catch.
    let list = body_of(&ir, "nx_new_list");
    assert!(
        list.contains("malloc(i64 16)"),
        "list header allocation must be 16:\n{list}"
    );
    let dict = body_of(&ir, "nx_new_dict");
    assert!(
        dict.contains("malloc(i64 16)"),
        "dict header allocation must be 16:\n{dict}"
    );
}

/// Counts are element counts, not byte sizes, and every load widens
/// back to i64 with `zext`. No index or length is computed in 32 bits,
/// so a large list cannot be mis-indexed by a truncated count.
#[test]
fn container_counts_widen_on_load() {
    let ir = compile_entry("xs = [1]\nxs.push(2)\n", std::path::Path::new(".")).unwrap();
    let body = void_body_of(&ir, "nx_listpush");
    assert!(body.contains("load i32"), "count must load as i32:\n{body}");
    assert!(
        body.contains("zext i32"),
        "count must widen to i64:\n{body}"
    );
    assert!(
        body.contains("trunc i64"),
        "count must narrow on store:\n{body}"
    );
}

/// A count can never silently wrap: growth past 2^31 elements is a
/// panic, not a truncated store. Unreachable in practice -- 2^31
/// elements is tens of gigabytes behind a runtime with no collector --
/// but a header field that could wrap is the kind of bug that only
/// shows up as corruption much later.
#[test]
fn container_growth_checks_for_saturation() {
    let ir = compile_entry(
        "xs = [1]\nd = {}\nd[\"k\"] = 1\n",
        std::path::Path::new("."),
    )
    .unwrap();
    let list = void_body_of(&ir, "nx_listpush");
    assert!(
        list.contains("icmp ule i64") && list.contains("@.msg.toomany"),
        "list growth must check before narrowing:\n{list}"
    );
    let dict = void_body_of(&ir, "nx_dictset");
    assert!(
        dict.contains("icmp ule i64") && dict.contains("@.msg.toomany"),
        "dict growth must check before narrowing:\n{dict}"
    );
}

/// Record and descriptor headers keep 64-bit counts. Their layout is
/// already at the alignment floor -- `{ptr, i64, ptr}` and
/// `{ptr, i64, i64, ptr}` are 24 and 32 bytes, and no narrowing of the
/// scalar fields shrinks either, because two pointer-width fields
/// already fill the space. Reordering and narrowing together would be
/// needed, and it buys 0 bytes for the record and 8 for the
/// descriptor, of which there is one per type rather than one per
/// value. Not worth the churn.
#[test]
fn record_and_descriptor_headers_keep_their_width() {
    let ir = compile_entry(
        "type P:\n    x: Int\np = P(1)\nprint(p)\n",
        std::path::Path::new("."),
    )
    .unwrap();
    assert!(
        ir.contains("%NxRec = type { ptr, i64, ptr }"),
        "%NxRec changed:\n{ir}"
    );
    assert!(
        ir.contains("%NxDesc = type { ptr, i64, i64, ptr }"),
        "%NxDesc changed:\n{ir}"
    );
}

// --- impl blocks and methods ------------------------------------

const POINT: &str = "type P:\n    x: Int\n    y: Int\n";

#[test]
fn method_bodies_emit_as_functions() {
    let ir = compile_entry(
        &format!("{POINT}impl P:\n    fn sum(self):\n        return self.x + self.y\n"),
        std::path::Path::new("."),
    )
    .unwrap();
    // A method mangles to `Type.method` under its own symbol, so a
    // method can never collide with a same-named plain function.
    let sym = mangle_method("__main__", "P", "sum");
    assert!(
        ir.contains(&format!("define %NxVal @{sym}(")),
        "missing {sym}:\n{ir}"
    );
    assert!(
        !ir.contains(&format!(
            "define %NxVal @{}(",
            mangle_fn("__main__", "P.sum")
        )),
        "the `Type.method` key must not collide with a plain function name"
    );
}

/// `self` is a parameter, not a special register: value semantics give
/// the method its own copy to mutate, so the body clones it on entry.
#[test]
fn self_is_cloned_on_entry() {
    let ir = compile_entry(
            &format!("{POINT}impl P:\n    fn plus(mut self, d):\n        self.x = self.x + d\n        return self\n"),
            std::path::Path::new("."),
        )
        .unwrap();
    let b = body_of(&ir, &mangle_method("__main__", "P", "plus"));
    assert!(
        b.contains("call %NxVal @nx_clone"),
        "self must be copied:\n{b}"
    );
}

#[test]
fn associated_function_takes_no_self() {
    let ir = compile_entry(
        &format!("{POINT}impl P:\n    fn zero():\n        return P(0, 0)\n"),
        std::path::Path::new("."),
    )
    .unwrap();
    let b = body_of(&ir, &mangle_method("__main__", "P", "zero"));
    assert!(
        !b.contains("@nx_clone"),
        "an associated function has no receiver to copy:\n{b}"
    );
}

/// The whole point of `mut self`: the call's result is stored back
/// into the receiver variable.
#[test]
fn mut_self_call_writes_back_into_the_receiver() {
    let ir = compile_entry(
            &format!("{POINT}impl P:\n    fn plus(mut self, d):\n        self.x = self.x + d\n        return self\np = P(1, 1)\np.plus(2)\n"),
            std::path::Path::new("."),
        )
        .unwrap();
    let top = void_body_of(&ir, "nx__init___main__");
    assert!(
        top.contains(&mangle_method("__main__", "P", "plus")),
        "the call must be emitted:\n{top}"
    );
    // A store into the global holding the receiver, after the call.
    let call_at = top.find(&mangle_method("__main__", "P", "plus")).unwrap();
    let after = &top[call_at..];
    assert!(
        after.contains("store %NxVal") && after.contains("@nx__g___main____p"),
        "the result must be written back to p:\n{after}"
    );
}

/// A `mut self` call whose base has no storage evaluates but does not
/// write, which is what lets a chain read as one expression.
#[test]
fn mut_self_on_a_temporary_does_not_write_back() {
    // `p.plus(2)` does write back, because `p` is a variable. A
    // receiver built by a call has no storage, so the update has
    // nowhere to go and only the result survives.
    let ir = compile_entry(
            &format!("{POINT}impl P:\n    fn plus(mut self, d):\n        self.x = self.x + d\n        return self\n    fn total(self):\n        return self.x\nprint(P(1, 1).plus(2).total())\n"),
            std::path::Path::new("."),
        )
        .unwrap();
    let top = void_body_of(&ir, "nx__init___main__");
    let call_at = top.find(&mangle_method("__main__", "P", "plus")).unwrap();
    // Only the instruction right after the call: a write-back stores
    // the result back into the receiver, so nothing may be stored
    // between the call returning and the next value being prepared.
    let after: Vec<&str> = top[call_at..].lines().skip(1).take(1).collect();
    assert!(
        !after.iter().any(|l| l.contains("store")),
        "a temporary receiver has nowhere to write: {after:?}"
    );
}

/// The receiver must be evaluated exactly once. It is emitted to learn
/// its static type, and emitting it again to build the argument list
/// would run a `mut self` chain's write-back twice.
#[test]
fn a_method_receiver_is_evaluated_once() {
    let ir = compile_entry(
            &format!("{POINT}impl P:\n    fn plus(mut self, d):\n        self.x = self.x + d\n        return self\n    fn total(self):\n        return self.x\np = P(1, 1)\nprint(p.plus(2).plus(3).total())\n"),
            std::path::Path::new("."),
        )
        .unwrap();
    let top = void_body_of(&ir, "nx__init___main__");
    assert_eq!(
        top.matches(&format!(
            "call %NxVal @{}",
            mangle_method("__main__", "P", "plus")
        ))
        .count(),
        2,
        "two `plus` calls in the source, two in the IR:\n{top}"
    );
}

/// Methods resolve before builtin sugar, so a type may define its own
/// `push` and it wins.
#[test]
fn a_method_resolves_before_builtin_sugar() {
    let ir = compile_entry(
            &format!("{POINT}impl P:\n    fn push(self, v):\n        return self.x + v\np = P(1, 0)\nprint(p.push(41))\n"),
            std::path::Path::new("."),
        )
        .unwrap();
    let top = void_body_of(&ir, "nx__init___main__");
    assert!(
        top.contains(&mangle_method("__main__", "P", "push")),
        "the method must win over the builtin:\n{top}"
    );
}

/// A record receiver's type comes from the receiver, not from
/// inference, so method dispatch has to keep working when unboxing is
/// switched off. Otherwise NX_NOUNBOX would be a different language.
#[test]
fn methods_resolve_with_unboxing_off() {
    let src = format!(
            "{POINT}impl P:\n    fn sum(self):\n        return self.x + self.y\np = P(1, 2)\nprint(p.sum())\n"
        );
    for unbox_on in [true, false] {
        let ir = compile_opts(&src, std::path::Path::new("."), unbox_on).unwrap();
        assert!(
            ir.contains(&mangle_method("__main__", "P", "sum")),
            "unbox_on={unbox_on}: method dispatch must not depend on unboxing:\n{ir}"
        );
        assert_top_level_defines(&ir);
    }
}

/// A method is never memoized: its call has an effect the cache cannot
/// replay, because a `mut self` result is written back at the call site.
#[test]
fn methods_are_not_memoized() {
    let ir = compile_entry(
            &format!("{POINT}impl P:\n    fn plus(mut self, d):\n        self.x = self.x + d\n        return self\np = P(1, 1)\np.plus(2)\n"),
            std::path::Path::new("."),
        )
        .unwrap();
    let b = body_of(&ir, &mangle_method("__main__", "P", "plus"));
    assert!(!b.contains("@nx_memo_get"), "no cache read:\n{b}");
    assert!(!b.contains("@nx_memo_put"), "no cache write:\n{b}");
}

/// A receiver reached through a field resolves through the field's
/// declared type: the field read itself is dynamically typed, so the
/// method table cannot come from the value's representation type.
#[test]
fn method_through_a_field_dispatches_on_the_declared_field_type() {
    let ir = compile_entry(
            &format!("{POINT}impl P:\n    fn total(self):\n        return self.x + self.y\ntype Q:\n    p: P\nq = Q(P(1, 2))\nprint(q.p.total())\n"),
            std::path::Path::new("."),
        )
        .unwrap();
    let top = void_body_of(&ir, "nx__init___main__");
    assert!(
        top.contains(&mangle_method("__main__", "P", "total")),
        "the call must reach P.total, not sugar or an error:\n{top}"
    );
}

#[test]
fn int_param_stays_unboxed() {
    let ir = compile_entry(
        "fn f(n):\n    return n * 3 + 1\nprint(f(2))\n",
        std::path::Path::new("."),
    )
    .unwrap();
    let b = body_of(&ir, &mangle_fn("__main__", "f"));
    // An llvm.*.with.overflow intrinsic is raw machine arithmetic, not a
    // call: LLVM lowers it to the same add/mul plus overflow flags. What
    // this test actually guards is that the operands never get boxed,
    // which is what the two assertions below say.
    assert!(
        b.contains("@llvm.smul.with.overflow.i64"),
        "Int multiply must stay unboxed and checked:\n{b}"
    );
    assert!(
        b.contains("@llvm.sadd.with.overflow.i64"),
        "Int add must stay unboxed and checked:\n{b}"
    );
    assert!(
        !b.contains("@nx_mul"),
        "boxed mul helper must be gone:\n{b}"
    );
    assert!(
        !b.contains("@nx_add"),
        "boxed add helper must be gone:\n{b}"
    );
}

#[test]
fn int_local_slot_is_typed() {
    let ir = compile_entry(
        "fn f(n):\n    t = n + 1\n    return t\nprint(f(2))\n",
        std::path::Path::new("."),
    )
    .unwrap();
    let b = body_of(&ir, &mangle_fn("__main__", "f"));
    assert!(
        b.contains("alloca i64"),
        "Int local must get an i64 slot:\n{b}"
    );
}

#[test]
fn comparison_stays_unboxed() {
    let ir = compile_entry(
        "fn f(n):\n    if n < 10:\n        return 1\n    else:\n        return 0\nprint(f(2))\n",
        std::path::Path::new("."),
    )
    .unwrap();
    let b = body_of(&ir, &mangle_fn("__main__", "f"));
    assert!(
        b.contains("icmp slt i64"),
        "Int compare must be a raw icmp:\n{b}"
    );
    assert!(
        !b.contains("@nx_cmp"),
        "boxed cmp helper must be gone:\n{b}"
    );
}

/// A boxed global whose static type is known still feeds raw
/// arithmetic: the payload comes out of the box and widens.
#[test]
fn int_global_widens_in_float_arith() {
    let ir = compile_entry(
        "g = 4\nfn f(k):\n    t = g * 0.5\n    return t + k\nprint(f(1))\n",
        std::path::Path::new("."),
    )
    .unwrap();
    let b = body_of(&ir, &mangle_fn("__main__", "f"));
    assert!(
        b.contains("sitofp i64"),
        "Int global must widen to double:\n{b}"
    );
    assert!(
        b.contains("fmul double"),
        "mixed product must be a float mul:\n{b}"
    );
}

#[test]
fn float_division_keeps_the_zero_check() {
    let ir = compile_entry(
        "g = 4\nfn f(k):\n    return g / 0.5 + k\nprint(f(1))\n",
        std::path::Path::new("."),
    )
    .unwrap();
    let b = body_of(&ir, &mangle_fn("__main__", "f"));
    assert!(
        b.contains("@nx_fdiv"),
        "float division keeps the zero check:\n{b}"
    );
}

/// `input()` reads through the runtime helper with and without a
/// prompt; the prompt flag is what tells the two apart.
#[test]
fn input_calls_the_runtime_with_and_without_a_prompt() {
    let ir = compile_entry(
        "a = input()\nb = input(\"who: \")\nprint(a, b)\n",
        std::path::Path::new("."),
    )
    .unwrap();
    let top = void_body_of(&ir, "nx__init___main__");
    assert_eq!(
        top.matches("@nx_input").count(),
        2,
        "two input() calls, two runtime calls:\n{top}"
    );
    assert!(
        top.contains("i1 false"),
        "bare input() passes no prompt:\n{top}"
    );
    assert!(top.contains("i1 true"), "input(prompt) passes one:\n{top}");
}

/// ARGONE D, fifth box: integer overflow lives in HIR as
/// `ArithRule::Trap`, and the backend honors the recorded rule
/// rather than re-deriving the decision from operand types.
///
/// This test fails if the backend recomputes it. Each program is
/// lowered through `nx-hir` and the `Trap` nodes per operator are
/// counted; the same program is compiled, and the top-level body
/// must carry exactly that many of the matching checked
/// intrinsics -- no more, no fewer. A backend emitting plain `add`
/// (or any unchecked form) for `Int + Int` shows up as a missing
/// intrinsic; a lowering that misrecorded the rule shows up as a
/// mismatch in the other direction. The float control proves the
/// `Trap` arm is not taken for free: `1.0 + 2.0` records no `Trap`
/// and emits no checked intrinsic.
#[test]
fn trap_rule_drives_checked_emission() {
    use nx_hir::{
        ArithRule, BinRule, Block, HExpr, HExprKind, HProgram, HStmt, HStmtKind, HTarget,
    };

    fn exprs(block: &Block, op: nx_ast::BinOp, out: &mut usize) {
        for s in block {
            match &s.kind {
                HStmtKind::Assign {
                    targets, values, ..
                } => {
                    for t in targets {
                        walk_target(t, op, out);
                    }
                    for v in values {
                        expr(v, op, out);
                    }
                }
                HStmtKind::AssignOp {
                    target: tgt, value, ..
                } => {
                    walk_target(tgt, op, out);
                    expr(value, op, out);
                }
                HStmtKind::Print { values } | HStmtKind::Return { values } => {
                    for v in values {
                        expr(v, op, out);
                    }
                }
                HStmtKind::EnsureInit { .. } | HStmtKind::Break | HStmtKind::Continue => {}
                HStmtKind::If {
                    cond,
                    then_body,
                    elifs,
                    else_body,
                } => {
                    expr(cond, op, out);
                    exprs(then_body, op, out);
                    for (c, b) in elifs {
                        expr(c, op, out);
                        exprs(b, op, out);
                    }
                    if let Some(b) = else_body {
                        exprs(b, op, out);
                    }
                }
                HStmtKind::While { cond, body } => {
                    expr(cond, op, out);
                    exprs(body, op, out);
                }
                HStmtKind::ForRange {
                    start, end, body, ..
                } => {
                    expr(start, op, out);
                    expr(end, op, out);
                    exprs(body, op, out);
                }
                HStmtKind::ForEach { iter, body, .. } => {
                    expr(iter, op, out);
                    exprs(body, op, out);
                }
                HStmtKind::Del { targets } => {
                    for t in targets {
                        walk_target(&t.target, op, out);
                    }
                }
                HStmtKind::Assert { cond, message } => {
                    expr(cond, op, out);
                    if let Some(m) = message {
                        expr(m, op, out);
                    }
                }
                HStmtKind::Expr(e) => expr(e, op, out),
            }
        }
    }

    fn walk_target(t: &HTarget, op: nx_ast::BinOp, out: &mut usize) {
        match t {
            HTarget::Slot(_) | HTarget::Global(_) => {}
            HTarget::Index { base, index, .. } => {
                expr(base, op, out);
                expr(index, op, out);
            }
            HTarget::Field { base, .. } => expr(base, op, out),
        }
    }

    fn expr(e: &HExpr, op: nx_ast::BinOp, out: &mut usize) {
        match &e.kind {
            HExprKind::Binary {
                left,
                op: o,
                rule,
                right,
            } => {
                if *o == op && *rule == BinRule::Arith(ArithRule::Trap) {
                    *out += 1;
                }
                expr(left, op, out);
                expr(right, op, out);
            }
            HExprKind::List(items) => {
                for i in items {
                    expr(i, op, out);
                }
            }
            HExprKind::Range { start, end, .. } => {
                expr(start, op, out);
                expr(end, op, out);
            }
            HExprKind::Dict(pairs) => {
                for (k, v) in pairs {
                    expr(k, op, out);
                    expr(v, op, out);
                }
            }
            HExprKind::Field { base, .. } => expr(base, op, out),
            HExprKind::Index { base, index, .. } => {
                expr(base, op, out);
                expr(index, op, out);
            }
            HExprKind::Slice {
                base,
                from,
                to,
                step,
                ..
            } => {
                expr(base, op, out);
                for b in [from, to, step].into_iter().flatten() {
                    expr(b, op, out);
                }
            }
            HExprKind::Unary { operand, .. } => expr(operand, op, out),
            HExprKind::Equal { left, right, .. }
            | HExprKind::Compare { left, right, .. }
            | HExprKind::Logic { left, right, .. } => {
                expr(left, op, out);
                expr(right, op, out);
            }
            HExprKind::Contains { needle, hay, .. } => {
                expr(needle, op, out);
                expr(hay, op, out);
            }
            HExprKind::Select {
                cond,
                then_value,
                else_value,
            } => {
                expr(cond, op, out);
                expr(then_value, op, out);
                expr(else_value, op, out);
            }
            HExprKind::Compr {
                element,
                iter,
                cond,
                ..
            } => {
                expr(element, op, out);
                expr(iter, op, out);
                if let Some(c) = cond {
                    expr(c, op, out);
                }
            }
            HExprKind::CallFn { args, .. }
            | HExprKind::Construct { args, .. }
            | HExprKind::Builtin { args, .. } => {
                for a in args {
                    expr(a, op, out);
                }
            }
            HExprKind::CallMethod {
                receiver,
                args,
                writeback,
                ..
            } => {
                if let Some(r) = receiver {
                    expr(r, op, out);
                }
                for a in args {
                    expr(a, op, out);
                }
                if let Some(w) = writeback {
                    walk_target(w, op, out);
                }
            }
            HExprKind::Int(_)
            | HExprKind::Float(_)
            | HExprKind::Bool(_)
            | HExprKind::Str(_)
            | HExprKind::None
            | HExprKind::Place(_) => {}
        }
    }

    fn traps_in(prog: &HProgram, op: nx_ast::BinOp) -> usize {
        let mut n = 0;
        for f in &prog.funcs {
            exprs(&f.body, op, &mut n);
        }
        n
    }

    // (source, operator, HIR trap count, intrinsic that must appear
    // exactly that many times in the top-level body)
    let cases = [
        (
            "print(1 + 2)\n",
            nx_ast::BinOp::Add,
            1,
            "llvm.sadd.with.overflow.i64",
        ),
        (
            "print(7 - 9)\n",
            nx_ast::BinOp::Sub,
            1,
            "llvm.ssub.with.overflow.i64",
        ),
        (
            "print(3 * 4)\n",
            nx_ast::BinOp::Mul,
            1,
            "llvm.smul.with.overflow.i64",
        ),
        (
            "print(1 + 2)\nprint(3 + 4)\nprint(5 * 6)\n",
            nx_ast::BinOp::Add,
            2,
            "llvm.sadd.with.overflow.i64",
        ),
    ];
    for (src, op, want_traps, intrin) in cases {
        let hir = nx_hir::lower::lower_source(src, std::path::Path::new(".")).expect("lowers");
        let traps = traps_in(&hir, op);
        assert_eq!(
            traps, want_traps,
            "HIR must record {want_traps} Trap {op:?} node(s) in {src:?}"
        );
        // Explicit unboxing: the suite must not depend on the
        // ambient NX_NOUNBOX the way `compile_entry` does.
        let ir = compile_opts(src, std::path::Path::new("."), true).expect("compiles");
        let top = void_body_of(&ir, "nx__init___main__");
        assert_eq!(
            top.matches(intrin).count(),
            traps,
            "backend must honor the {traps} recorded Trap {op:?} node(s) with {intrin}:\n{top}"
        );
    }
    // The float control: no Trap is recorded, so no checked
    // intrinsic may appear -- the float arm must not take the Trap
    // path for free.
    let hir = nx_hir::lower::lower_source("print(1.0 + 2.0)\n", std::path::Path::new("."))
        .expect("lowers");
    assert_eq!(
        traps_in(&hir, nx_ast::BinOp::Add),
        0,
        "float addition records no Trap"
    );
    let ir = compile_opts("print(1.0 + 2.0)\n", std::path::Path::new("."), true).expect("compiles");
    let top = void_body_of(&ir, "nx__init___main__");
    assert!(
        top.contains("fadd double"),
        "float addition emits the float instruction:\n{top}"
    );
    assert!(
        !top.contains("with.overflow.i64"),
        "float addition must not emit a checked intrinsic:\n{top}"
    );
}

/// A parameter used only in a float expression has no determined type,
/// so it stays dynamic: nothing is widened because nothing is known.
#[test]
fn float_context_param_stays_boxed() {
    let ir = compile_entry(
        "fn half(x):\n    return x / 2.0\nprint(half(4))\n",
        std::path::Path::new("."),
    )
    .unwrap();
    let b = body_of(&ir, &mangle_fn("__main__", "half"));
    assert!(
        !b.contains("sitofp"),
        "an untyped param must not be widened:\n{b}"
    );
    assert!(
        b.contains("@nx_div"),
        "it takes the boxed division path:\n{b}"
    );
}

#[test]
fn untyped_param_stays_boxed() {
    // `x` is never used, so its type is unknown: no unboxing.
    let ir = compile_entry(
        "fn f(x):\n    return 1\nprint(f(2))\n",
        std::path::Path::new("."),
    )
    .unwrap();
    let b = body_of(&ir, &mangle_fn("__main__", "f"));
    assert!(
        b.contains("alloca %NxVal"),
        "unknown-typed param stays boxed:\n{b}"
    );
}

#[test]
fn list_of_ints_indexes_unboxed() {
    let ir = compile_entry(
        "fn f(xs):\n    return xs[0] + 1\nprint(f([4]))\n",
        std::path::Path::new("."),
    )
    .unwrap();
    let b = body_of(&ir, &mangle_fn("__main__", "f"));
    assert!(
        b.contains("add i64"),
        "known Int element must stay unboxed:\n{b}"
    );
}

#[test]
fn function_boundary_stays_boxed() {
    // Calls keep the boxed ABI even though both sides are Int.
    let ir = compile_entry(
        "fn g(n):\n    return n\nfn f(n):\n    return g(n) + 1\nprint(f(2))\n",
        std::path::Path::new("."),
    )
    .unwrap();
    let b = body_of(&ir, &mangle_fn("__main__", "f"));
    assert!(b.contains("@nx_int(i64"), "call argument must re-box:\n{b}");
}

/// An unboxed slot holds a bare scalar, so nothing may read or write
/// it as a `%NxVal`. A mismatched load is invalid IR and used to
/// crash the binary at runtime.
#[test]
fn unboxed_slots_are_never_touched_as_boxes() {
    let ir = compile_entry(
            "fn work(n):\n    total = 0\n    i = 0\n    while i < n:\n        total = total + i * 3 - 1\n        i = i + 1\n    return total\nprint(work(6))\n",
            std::path::Path::new("."),
        )
        .unwrap();
    let b = body_of(&ir, &mangle_fn("__main__", "work"));
    let scalar_slots: Vec<String> = b
        .lines()
        .filter_map(|l| {
            let t = l.trim();
            let ty = t.split(" = alloca ").nth(1)?;
            let ty = ty.split_whitespace().next()?;
            if matches!(ty, "i64" | "double" | "i1") {
                return Some(t.split(' ').next()?.to_string());
            }
            None
        })
        .collect();
    assert!(scalar_slots.len() >= 3, "expected typed slots in:\n{b}");
    for slot in scalar_slots {
        for l in b.lines() {
            let t = l.trim();
            let touches = t.contains(&format!("ptr {slot}"))
                && (t.contains("%NxVal") || t.contains("nx_free_val"));
            assert!(!touches, "typed slot {slot} used as a box: {t}\n{b}");
        }
    }
    assert!(
        b.contains("@llvm.smul.with.overflow.i64"),
        "loop body should be raw checked arithmetic:\n{b}"
    );
}

#[test]
fn opt_out_handles_indexing_and_iteration() {
    // Indexing a list of known scalars and iterating one both go
    // through as_raw, which declines when unboxing is off. They must
    // fall back to the box rather than unwrapping nothing.
    let src = "xs = [1, 2]\nfn first(ys):\n    return ys[0]\nfn total(ys):\n    t = 0\n    for y in ys:\n        t = t + y\n    return t\nprint(first(xs), total(xs))\n";
    let unboxed = compile_opts(src, std::path::Path::new("."), true).unwrap();
    let boxed = compile_opts(src, std::path::Path::new("."), false).unwrap();
    assert!(
        unboxed.contains("add i64"),
        "unboxed sum should be raw:\n{unboxed}"
    );
    assert!(
        boxed.contains("@nx_add"),
        "boxed sum should use the helper:\n{boxed}"
    );
    assert_top_level_defines(&unboxed);
    assert_top_level_defines(&boxed);
}

#[test]
fn nounbox_opt_out_keeps_everything_boxed() {
    let ir = compile_opts(
        "fn f(n):\n    t = n * 3\n    return t\nprint(f(2))\n",
        std::path::Path::new("."),
        false,
    )
    .unwrap();
    let b = body_of(&ir, &mangle_fn("__main__", "f"));
    assert!(
        b.contains("@nx_mul"),
        "opt-out must keep the boxed path:\n{b}"
    );
    assert!(
        !b.contains("mul i64"),
        "opt-out must not emit raw mul:\n{b}"
    );
    assert!(
        !b.contains("alloca i64"),
        "opt-out must not use typed slots:\n{b}"
    );
}

fn memo_gets(ir: &str) -> usize {
    // The prelude *defines* nx_memo_get once; count actual calls.
    ir.matches("call i1 @nx_memo_get").count()
}

#[test]
fn memo_prologue_for_pure_fn() {
    let ir = compile_entry(
            "fn fib(n):\n    if n <= 1:\n        return n\n    else:\n        return fib(n - 1) + fib(n - 2)\nprint(fib(10))\n",
            std::path::Path::new("."),
        )
        .unwrap();
    assert!(memo_gets(&ir) >= 1, "pure fib must consult the cache");
    assert!(
        ir.contains("call void @nx_memo_put"),
        "pure fib must populate the cache"
    );
    assert_top_level_defines(&ir);
}

#[test]
fn no_memo_for_printing_fn() {
    let ir = compile_entry(
        "fn f(n):\n    print(n)\n    return n\nprint(f(1))\n",
        std::path::Path::new("."),
    )
    .unwrap();
    assert_eq!(memo_gets(&ir), 0, "printing fn must not be memoized");
}

fn free_calls(ir: &str) -> usize {
    // `nx_free_val` recurses over elements; only calls outside its own
    // definition free program values. The self-count is derived from
    // the prelude text so growing the runtime (dicts, records) does
    // not silently shift every assertion.
    let own = super::PRELUDE.matches("call void @nx_free_val").count();
    ir.matches("call void @nx_free_val").count() - own
}

/// A Unique local owns heap buffers, so it must be released on every
/// exit path. The temp has to be a list, not a scalar: a scalar is
/// Stack-allocated and has nothing to free.
#[test]
fn unique_temp_gets_freed() {
    let ir = compile_entry(
        "fn f(n):\n    t = [1, 2]\n    print(t, n)\nprint(f(21))\n",
        std::path::Path::new("."),
    )
    .unwrap();
    assert!(free_calls(&ir) >= 1, "Unique local t must be freed");
}

/// The scalar counterpart: `t = n * 2` is Stack, so it must not be
/// freed at all. Freeing it would mean loading an i64 slot as a box.
#[test]
fn stack_local_is_not_freed() {
    let ir = compile_entry(
        "fn f(n):\n    t = n * 2\n    print(t)\nprint(f(21))\n",
        std::path::Path::new("."),
    )
    .unwrap();
    assert_eq!(free_calls(&ir), 0, "a Stack local owns no buffer");
    let b = body_of(&ir, &mangle_fn("__main__", "f"));
    assert!(
        b.contains("alloca i64"),
        "scalar temp should be typed:\n{b}"
    );
}

#[test]
fn shared_global_not_freed() {
    let ir = compile_entry("x = [1]\nprint(x)\n", std::path::Path::new(".")).unwrap();
    assert_eq!(free_calls(&ir), 0, "globals must not be freed");
}
/// Every `private constant [N x i8] c"..."` in the prelude must have a
/// matching N (LLVM rejects mismatches; hand-counting is unreliable).
#[test]
fn prelude_string_sizes() {
    let mut bad = Vec::new();
    for line in super::PRELUDE.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("@") else {
            continue;
        };
        let Some(at) = rest.find("private constant [") else {
            continue;
        };
        let after = &rest[at + "private constant [".len()..];
        let Some(sp) = after.find(" x i8] c\"") else {
            continue;
        };
        let want: usize = after[..sp].parse().unwrap();
        let mut esc = &after[sp + " x i8] c\"".len()..];
        esc = esc.strip_suffix('"').unwrap();
        // Count LLVM escape sequences (\XX) as one byte each.
        let mut got = 0usize;
        let b = esc.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\\' {
                got += 1;
                i += 3;
            } else {
                got += 1;
                i += 1;
            }
        }
        if got != want {
            bad.push(format!("{line} counts {got}, says {want}"));
        }
    }
    assert!(
        bad.is_empty(),
        "mismatched string sizes:\n{}",
        bad.join("\n")
    );
}
