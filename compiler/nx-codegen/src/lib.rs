//! LLVM backend for Nexum: typed AST -> LLVM IR text.
//!
//! Principle: NX owns semantics (this crate), LLVM owns machine code.
//! Dynamic values are boxed `%NxVal` (helpers in `runtime.ll`) and all
//! functions share one calling convention, so every boundary -- call
//! arguments, returns, list elements, module globals -- stays boxed.
//! Inside a function the unboxing pass keeps a value whose static type is
//! a known scalar in a bare register (`i64`/`double`/`i1`) and re-boxes
//! it only where a boundary demands it. Opt out with NX_NOUNBOX=1.
//!
//! v0 limits (checked programs only; `nx build` requires `nx check` clean):
//! - Modules and from-imported names resolve at compile time.
//! - No function values in value position (`x = foo`); call them.
//! - No closures over outer function locals.

use std::collections::{HashMap, HashSet};
use nx_ast::{BinOp, Expr, Program, Span, Stmt, UnaryOp};
use nx_types::Ty;

// The rules and the emission table that reads them. Re-exported from
// `nx-hir` rather than redefined here: the rule is a lowering decision,
// and a backend copy of it would be a second owner (ARGONE D, docs/
// architecture/hir.md sections 4 and 10).
use nx_hir::{arith_plan, ArithPlan, BinRule, BinRule as Birule, PowRule};
use nx_hir::ArithRule::{Float, PromoteFloat, Trap};

const PRELUDE: &str = include_str!("runtime.ll");

/// A compiled value. Two facts, deliberately kept apart:
/// - `raw` is the *physical* form: Some(t) means `reg` holds a bare scalar
///   of type t, None means it holds a boxed `%NxVal`.
/// - `ty` is the *static* type, which is what picks an operator. A boxed
///   value can still have a known type (a module global, a call result, a
///   list), and then arithmetic on it can skip the tag dispatch even
///   though the register itself is a box.
#[derive(Debug, Clone)]
struct NV {
    reg: String,
    raw: Option<Ty>,
    ty: Ty,
    /// The literal this register holds, when it came straight from source.
    /// Shifts need it: a shift distance outside 0..63 is a runtime panic,
    /// and only a constant lets the unboxed path skip that check.
    const_i: Option<i64>,
    /// Uniquely owned storage: freshly allocated by this expression, with
    /// no other binding referencing it. Storing a fresh value needs no
    /// `nx_clone` -- there is nothing to separate from. Anything that may
    /// alias (loads, calls, reads of stored containers) is not fresh.
    fresh: bool,
}

impl NV {
    /// A bare scalar already sitting in a register.
    fn raw(t: Ty, reg: String) -> NV {
        NV { reg, raw: Some(t.clone()), ty: t, const_i: None, fresh: false }
    }
    /// A box whose dynamic type the backend does not know.
    fn dyn_boxed(reg: String) -> NV {
        NV { reg, raw: None, ty: Ty::Unknown, const_i: None, fresh: false }
    }
    /// A box whose static type is known: usable unboxed where the caller
    /// needs the payload, but still physically a `%NxVal`.
    fn boxed_known(reg: String, ty: Ty) -> NV {
        NV { reg, raw: None, ty, const_i: None, fresh: false }
    }
    /// A bare Int whose value is known at compile time.
    fn raw_const(t: Ty, reg: String, value: i64) -> NV {
        NV { reg, raw: Some(t.clone()), ty: t, const_i: Some(value), fresh: false }
    }
    /// A freshly allocated box: uniquely owned, so storing it clones
    /// nothing. Only for values this expression itself created --
    /// literals, runtime constructors, and operators that allocate.
    fn fresh_boxed(reg: String, ty: Ty) -> NV {
        NV { reg, raw: None, ty, const_i: None, fresh: true }
    }
}

/// LLVM type holding a scalar of this NX type, or None if it stays boxed.
fn ll_scalar(t: &Ty) -> Option<&'static str> {
    match t {
        Ty::Int => Some("i64"),
        Ty::Float => Some("double"),
        Ty::Bool => Some("i1"),
        _ => None,
    }
}

/// LLVM's exact float literal form: the raw bit pattern, so no decimal
/// rounding can creep in between the parser and the instruction.
fn fmt_double(x: f64) -> String {
    format!("0x{:016X}", x.to_bits())
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodegenError {
    pub message: String,
    pub line: nx_ast::LineNo,
    pub col: nx_ast::ColNo,
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "codegen error at {}:{}: {}", self.line, self.col, self.message)
    }
}

impl std::error::Error for CodegenError {}

fn err(span: Span, msg: String) -> CodegenError {
    CodegenError { message: msg, line: span.line, col: span.col }
}

fn mangle_fn(module: &str, name: &str) -> String {
    format!("nx__f_{}__{module}__{name}", name.len())
}

fn mangle_global(module: &str, name: &str) -> String {
    format!("nx__g_{module}__{name}")
}

fn mangle_desc(module: &str, name: &str) -> String {
    // Length-prefixed like mangle_fn, so `a.b` and `a_bc` style collisions
    // cannot alias two descriptors.
    format!("nx__d_{}__{module}__{name}", name.len())
}

/// Names a statement binds at module level, in first-seen order.
///
/// Only what becomes a *global*: a plain name bound at the top level, an
/// import alias, and — the reason this recurses — a name assigned inside
/// a nested block at module level. A name bound inside one is still
/// module-visible and still needs its global declared up front, before any
/// function body exists to emit it into.
///
/// Loop variables, comprehension variables and function-local bindings are
/// deliberately not collected: those are slots inside the function that
/// owns them, not module globals.
fn collect_module_globals(s: &Stmt, out: &mut Vec<String>) {
    let mut push = |n: &String| {
        if !out.contains(n) {
            out.push(n.clone());
        }
    };
    match s {
        Stmt::AssignOp { target, .. } => {
            if let nx_ast::Target::Name(n) = target {
                push(n);
            }
        }
        Stmt::Assign { targets, .. } => {
            for t in targets {
                if let nx_ast::Target::Name(n) = t {
                    push(n);
                }
            }
        }
        Stmt::FromImport { names, .. } => {
            for (name, alias) in names {
                push(alias.as_ref().unwrap_or(name));
            }
        }
        Stmt::Import { module: m, alias, .. } => {
            push(alias.as_ref().unwrap_or(m));
        }

        // Control flow nests the same way everywhere: a loop's own
        // variable is a slot in the enclosing function, but assignments
        // in its body still bind module names. Function bodies are
        // separate scopes and contribute nothing (child_bodies skips them).
        _ => {
            for b in nx_ast::shape::child_bodies(s) {
                for t in b {
                    collect_module_globals(t, out);
                }
            }
        }
    }
}

fn mangle_method(module: &str, type_name: &str, method: &str) -> String {
    // Length-prefixed segments, so `a.B` + `c` can never alias `a` + `B.c`.
    format!("nx__m_{}__{module}__{type_name}__{method}", method.len())
}

fn mangle_init(module: &str) -> String {
    format!("nx__init_{module}")
}

fn mangle_done(module: &str) -> String {
    format!("nx__done_{module}")
}

#[cfg(test)]
mod tests {
    use super::{compile_entry, compile_opts, mangle_fn, mangle_global, mangle_method};

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
        let ir = compile_entry("t = 0\nfor i in 0..4:\n    t = t + i\nprint(t)\n", std::path::Path::new("."))
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
                let label: String =
                    rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.').collect();
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
        let end = rest[1..].find("\ndefine ").map(|i| i + 1).unwrap_or(rest.len());
        rest[..end].to_string()
    }

    /// Body of a `void` function -- the module initializer, which is where
    /// every top-level statement lands.
    fn void_body_of(ir: &str, mangled: &str) -> String {
        let start = ir
            .find(&format!("define void @{mangled}("))
            .unwrap_or_else(|| panic!("no function {mangled} in output"));
        let rest = &ir[start..];
        let end = rest[1..].find("\ndefine ").map(|i| i + 1).unwrap_or(rest.len());
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
        assert!(body.contains("zext i32"), "count must widen to i64:\n{body}");
        assert!(body.contains("trunc i64"), "count must narrow on store:\n{body}");
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
        assert!(ir.contains(&format!("define %NxVal @{sym}(")), "missing {sym}:\n{ir}");
        assert!(
            !ir.contains(&format!("define %NxVal @{}(", mangle_fn("__main__", "P.sum"))),
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
        assert!(b.contains("call %NxVal @nx_clone"), "self must be copied:\n{b}");
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
            top.matches(&format!("call %NxVal @{}", mangle_method("__main__", "P", "plus"))).count(),
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
            let ir =
                compile_opts(&src, std::path::Path::new("."), unbox_on).unwrap();
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
        assert!(b.contains("@llvm.smul.with.overflow.i64"), "Int multiply must stay unboxed and checked:\n{b}");
        assert!(b.contains("@llvm.sadd.with.overflow.i64"), "Int add must stay unboxed and checked:\n{b}");
        assert!(!b.contains("@nx_mul"), "boxed mul helper must be gone:\n{b}");
        assert!(!b.contains("@nx_add"), "boxed add helper must be gone:\n{b}");
    }

    #[test]
    fn int_local_slot_is_typed() {
        let ir = compile_entry("fn f(n):\n    t = n + 1\n    return t\nprint(f(2))\n", std::path::Path::new("."))
            .unwrap();
        let b = body_of(&ir, &mangle_fn("__main__", "f"));
        assert!(b.contains("alloca i64"), "Int local must get an i64 slot:\n{b}");
    }

    #[test]
    fn comparison_stays_unboxed() {
        let ir = compile_entry(
            "fn f(n):\n    if n < 10:\n        return 1\n    else:\n        return 0\nprint(f(2))\n",
            std::path::Path::new("."),
        )
        .unwrap();
        let b = body_of(&ir, &mangle_fn("__main__", "f"));
        assert!(b.contains("icmp slt i64"), "Int compare must be a raw icmp:\n{b}");
        assert!(!b.contains("@nx_cmp"), "boxed cmp helper must be gone:\n{b}");
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
        assert!(b.contains("sitofp i64"), "Int global must widen to double:\n{b}");
        assert!(b.contains("fmul double"), "mixed product must be a float mul:\n{b}");
    }

    #[test]
    fn float_division_keeps_the_zero_check() {
        let ir = compile_entry(
            "g = 4\nfn f(k):\n    return g / 0.5 + k\nprint(f(1))\n",
            std::path::Path::new("."),
        )
        .unwrap();
        let b = body_of(&ir, &mangle_fn("__main__", "f"));
        assert!(b.contains("@nx_fdiv"), "float division keeps the zero check:\n{b}");
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
        assert!(top.contains("i1 false"), "bare input() passes no prompt:\n{top}");
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
                    HStmtKind::Assign { targets, values, .. } => {
                        for t in targets {
                            walk_target(t, op, out);
                        }
                        for v in values {
                            expr(v, op, out);
                        }
                    }
                    HStmtKind::AssignOp { target: tgt, value, .. } => {
                        walk_target(tgt, op, out);
                        expr(value, op, out);
                    }
                    HStmtKind::Print { values } | HStmtKind::Return { values } => {
                        for v in values {
                            expr(v, op, out);
                        }
                    }
                    HStmtKind::EnsureInit { .. } | HStmtKind::Break | HStmtKind::Continue => {}
                    HStmtKind::If { cond, then_body, elifs, else_body } => {
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
                    HStmtKind::ForRange { start, end, body, .. } => {
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
                HExprKind::Binary { left, op: o, rule, right } => {
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
                HExprKind::Slice { base, from, to, step, .. } => {
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
                HExprKind::Select { cond, then_value, else_value } => {
                    expr(cond, op, out);
                    expr(then_value, op, out);
                    expr(else_value, op, out);
                }
                HExprKind::Compr { element, iter, cond, .. } => {
                    expr(element, op, out);
                    expr(iter, op, out);
                    if let Some(c) = cond {
                        expr(c, op, out);
                    }
                }
                HExprKind::CallFn { args, .. } | HExprKind::Construct { args, .. } | HExprKind::Builtin { args, .. } => {
                    for a in args {
                        expr(a, op, out);
                    }
                }
                HExprKind::CallMethod { receiver, args, writeback, .. } => {
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
            ("print(1 + 2)\n", nx_ast::BinOp::Add, 1, "llvm.sadd.with.overflow.i64"),
            ("print(7 - 9)\n", nx_ast::BinOp::Sub, 1, "llvm.ssub.with.overflow.i64"),
            ("print(3 * 4)\n", nx_ast::BinOp::Mul, 1, "llvm.smul.with.overflow.i64"),
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
            assert_eq!(traps, want_traps, "HIR must record {want_traps} Trap {op:?} node(s) in {src:?}");
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
        assert_eq!(traps_in(&hir, nx_ast::BinOp::Add), 0, "float addition records no Trap");
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
        assert!(!b.contains("sitofp"), "an untyped param must not be widened:\n{b}");
        assert!(b.contains("@nx_div"), "it takes the boxed division path:\n{b}");
    }

    #[test]
    fn untyped_param_stays_boxed() {
        // `x` is never used, so its type is unknown: no unboxing.
        let ir = compile_entry("fn f(x):\n    return 1\nprint(f(2))\n", std::path::Path::new("."))
            .unwrap();
        let b = body_of(&ir, &mangle_fn("__main__", "f"));
        assert!(b.contains("alloca %NxVal"), "unknown-typed param stays boxed:\n{b}");
    }

    #[test]
    fn list_of_ints_indexes_unboxed() {
        let ir = compile_entry(
            "fn f(xs):\n    return xs[0] + 1\nprint(f([4]))\n",
            std::path::Path::new("."),
        )
        .unwrap();
        let b = body_of(&ir, &mangle_fn("__main__", "f"));
        assert!(b.contains("add i64"), "known Int element must stay unboxed:\n{b}");
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
        assert!(b.contains("@llvm.smul.with.overflow.i64"), "loop body should be raw checked arithmetic:\n{b}");
    }

    #[test]
    fn opt_out_handles_indexing_and_iteration() {
        // Indexing a list of known scalars and iterating one both go
        // through as_raw, which declines when unboxing is off. They must
        // fall back to the box rather than unwrapping nothing.
        let src = "xs = [1, 2]\nfn first(ys):\n    return ys[0]\nfn total(ys):\n    t = 0\n    for y in ys:\n        t = t + y\n    return t\nprint(first(xs), total(xs))\n";
        let unboxed = compile_opts(src, std::path::Path::new("."), true).unwrap();
        let boxed = compile_opts(src, std::path::Path::new("."), false).unwrap();
        assert!(unboxed.contains("add i64"), "unboxed sum should be raw:\n{unboxed}");
        assert!(boxed.contains("@nx_add"), "boxed sum should use the helper:\n{boxed}");
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
        assert!(b.contains("@nx_mul"), "opt-out must keep the boxed path:\n{b}");
        assert!(!b.contains("mul i64"), "opt-out must not emit raw mul:\n{b}");
        assert!(!b.contains("alloca i64"), "opt-out must not use typed slots:\n{b}");
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
        assert!(ir.contains("call void @nx_memo_put"), "pure fib must populate the cache");
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
        assert!(b.contains("alloca i64"), "scalar temp should be typed:\n{b}");
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
            let Some(rest) = line.strip_prefix("@") else { continue };
            let Some(at) = rest.find("private constant [") else { continue };
            let after = &rest[at + "private constant [".len()..];
            let Some(sp) = after.find(" x i8] c\"") else { continue };
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
        assert!(bad.is_empty(), "mismatched string sizes:\n{}", bad.join("\n"));
    }
}

/// Directory a module's own file lives in, so the checker resolves its
/// relative imports the same way the loader did.
fn module_dir(base: &std::path::Path, module: &str) -> std::path::PathBuf {
    if module == "__main__" {
        return base.to_path_buf();
    }
    match nx_codegen_loader_path(base, module) {
        Some(p) => p,
        None => base.to_path_buf(),
    }
}

fn nx_codegen_loader_path(base: &std::path::Path, module: &str) -> Option<std::path::PathBuf> {
    nx_ast::shape::resolve_module_file(&[base.to_path_buf()], module)
}

pub fn compile_entry(source: &str, base: &std::path::Path) -> Result<String, CodegenError> {
    compile_opts(source, base, std::env::var("NX_NOUNBOX").is_err())
}

/// Compile with an explicit unboxing switch. `compile_entry` reads
/// NX_NOUNBOX; tests use this directly so they do not race on the
/// process environment.
pub fn compile_opts(
    source: &str,
    base: &std::path::Path,
    unbox_on: bool,
) -> Result<String, CodegenError> {
    let mut loader = Loader {
        programs: HashMap::new(),
        order: Vec::new(),
        loading: Vec::new(),
        base: base.to_path_buf(),
    };
    loader.load_main(source)?;
    let order = loader.order.clone();
    // Inferred types drive unboxing and the memory plan. The driver
    // type-checks first, so a failure here means an unreachable path.
    let mut types: HashMap<(String, String), nx_types::FnInfo> = HashMap::new();
    for module in &order {
        if let Some(prog) = loader.programs.get(module) {
            match nx_types::infer_program_for(prog, &module_dir(&loader.base, module), module) {
                Ok(m) => {
                    for (k, v) in m {
                        types.insert(k, v);
                    }
                }
                Err(_) => return Err(CodegenError {
                    message: "internal: program reached codegen without a clean type check".to_string(),
                    line: 0,
                    col: 0,
                }),
            }
        }
    }
    // The planner only promotes scalars to Stack when the unboxing pass is
    // actually on, so NX_NOUNBOX gets the pre-unboxing plan too.
    let no_types: HashMap<(String, String), nx_types::FnInfo> = HashMap::new();
    let plan_types = if unbox_on { &types } else { &no_types };
    let plan = nx_mem::plan(&loader.programs, "__main__", plan_types);
    // Memo table ids for purity-proven functions (opt out: NX_NOMEMO=1).
    let mut memo: HashMap<(String, String), i64> = HashMap::new();
    if std::env::var("NX_NOMEMO").is_err() {
        if let Ok(ir) = nx_ir::analyze_map(loader.programs.clone()) {
            let mut keys: Vec<_> = ir.funcs.keys().cloned().collect();
            keys.sort();
            for (i, k) in keys.into_iter().enumerate() {
                if nx_ir::memoizable(&ir.funcs[&k].summary) {
                    memo.insert(k, i as i64);
                }
            }
        }
    }
    let mut g = Gen::new(plan, loader.programs, memo, types);
    g.unbox_on = unbox_on;
    g.emit_prelude();
    g.harvest_layouts();
    for module in &order {
        let prog = g.programs.get(module).cloned().unwrap();
        g.declare_module_fns(module, &prog);
    }
    for module in &order {
        let prog = g.programs.get(module).cloned().unwrap();
        g.emit_module(module, &prog)?;
    }
    g.emit_main();
    Ok(g.finish())
}

struct Loader {
    programs: HashMap<String, Program>,
    order: Vec<String>,
    loading: Vec<String>,
    base: std::path::PathBuf,
}

impl Loader {
    fn load_main(&mut self, source: &str) -> Result<(), CodegenError> {
        let prog = parse(source)?;
        self.insert("__main__".to_string(), prog)
    }

    fn insert(&mut self, name: String, prog: Program) -> Result<(), CodegenError> {
        if self.programs.contains_key(&name) {
            return Ok(());
        }
        if self.loading.contains(&name) {
            return Err(CodegenError {
                message: format!("circular import of '{name}'"),
                line: 1,
                col: 1,
            });
        }
        self.loading.push(name.clone());
        let mut deps = Vec::new();
        collect_imports(&prog, &mut deps);
        for dep in deps {
            let path = self.resolve(&dep).ok_or(CodegenError {
                message: format!("cannot find module '{dep}.nx'"),
                line: 1,
                col: 1,
            })?;
            let src = std::fs::read_to_string(&path).map_err(|e| CodegenError {
                message: format!("cannot read module '{dep}': {e}"),
                line: 1,
                col: 1,
            })?;
            let sub = parse(&src).map_err(|mut e: CodegenError| {
                e.message = format!("in module '{dep}': {}", e.message);
                e
            })?;
            // Resolve nested imports relative to the submodule's own dir.
            let saved = std::mem::replace(
                &mut self.base,
                path.parent().map(|p| p.to_path_buf()).unwrap_or(".".into()),
            );
            let r = self.insert(dep, sub);
            self.base = saved;
            r?;
        }
        self.loading.pop();
        self.order.push(name.clone());
        self.programs.insert(name, prog);
        Ok(())
    }

    fn resolve(&self, name: &str) -> Option<std::path::PathBuf> {
        nx_ast::shape::resolve_module_file(&[self.base.clone()], name)
    }
}

fn parse(source: &str) -> Result<Program, CodegenError> {
    let tokens = nx_lexer::lex(source).map_err(|e| CodegenError {
        message: e.message,
        line: e.line,
        col: e.col,
    })?;
    nx_parser::parse(tokens).map_err(|e| CodegenError {
        message: e.message,
        line: e.line,
        col: e.col,
    })
}

/// All source files a build depends on: the entry plus every transitively
/// imported `.nx` file. Used for incremental rebuild checks.
pub fn dependencies(
    entry: &std::path::Path,
    base: &std::path::Path,
) -> Result<Vec<std::path::PathBuf>, CodegenError> {
    let mut out = vec![entry.to_path_buf()];
    let mut queue = vec![entry.to_path_buf()];
    let mut seen = std::collections::HashSet::new();
    while let Some(path) = queue.pop() {
        let canon = path.canonicalize().unwrap_or(path.clone());
        if !seen.insert(canon) {
            continue;
        }
        let src = std::fs::read_to_string(&path).map_err(|e| CodegenError {
            message: format!("cannot read {}: {e}", path.display()),
            line: 1,
            col: 1,
        })?;
        let prog = parse(&src)?;
        let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or(base.to_path_buf());
        let mut deps = Vec::new();
        collect_imports(&prog, &mut deps);
        for dep in deps {
            // The importing file's own dir first, then the entry dir,
            // then NX_PATH -- the only copy that ever searched two bases.
            if let Some(p) =
                nx_ast::shape::resolve_module_file(&[dir.clone(), base.to_path_buf()], &dep)
            {
                out.push(p.clone());
                queue.push(p);
            }
        }
    }
    Ok(out)
}

fn collect_imports(prog: &Program, out: &mut Vec<String>) {
    // Recursive since imports are legal inside function bodies; the
    // loader must not miss a dependency the checker accepted.
    for m in nx_ast::shape::imported_modules(prog) {
        if !out.contains(&m) {
            out.push(m);
        }
    }
}

/// How a source name resolves in generated code. `Local` carries no slot:
/// the slot (and its representation) live in `locals`/`rep`, which the
/// unboxing pass rewrites as it goes.
#[derive(Debug, Clone)]
enum Binding {
    Local,
    Global(String),
    Module(String),
    ModuleFn(String, String),
}

/// Block termination state: which terminator the current block ends with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Term {
    Ret,
    Brk,
    Ctn,
}

struct Gen {
    pre: String,
    top: String,
    out: String,
    plan: nx_mem::Plan,
    programs: HashMap<String, Program>,
    memo: HashMap<(String, String), i64>,
    /// Inferred static types per (module, function); drives unboxing.
    types: HashMap<(String, String), nx_types::FnInfo>,
    /// Global switch: NX_NOUNBOX=1 keeps the old all-boxed behavior.
    unbox_on: bool,
    /// Representation of each *local* slot: present only when the name is
    /// a proven scalar, absent when the slot holds a boxed `%NxVal`. Kept
    /// in lockstep with `locals` so a slot's type always matches its alloca.
    rep: HashMap<String, Ty>,
    tmp: u64,
    label: u64,
    strc: u64,
    arity: HashMap<(String, String), usize>,
    modrefs: HashMap<String, String>,
    falias: HashMap<String, (String, String)>,
    locals: HashMap<String, String>,
    globals: HashSet<String>,
    cur_module: String,
    cur_fn: String,
    /// Name of the basic block the emitter is currently writing into: the
    /// last label passed to `block()`. A phi names the block its incoming
    /// value was computed in, so anything that assumes "I am still in the
    /// block I just opened" has to be able to check that. It is not: a
    /// subexpression can open blocks of its own -- every checked
    /// arithmetic op does, and so does a nested `and`/`or` -- and leave
    /// the emitter somewhere else entirely.
    cur_block: String,
    /// When true, `w()` writes to the top-level buffer (for outlining
    /// task functions in the middle of another function body).
    to_top: bool,
    in_init: bool,
    term: Option<Term>,
    loops: Vec<(String, String)>,
    /// Open entry-block frames, one per function currently being emitted.
    /// Allocas are collected here and spliced in at the end, because an
    /// alloca inside a loop body allocates fresh stack every iteration and
    /// is only released when the function returns.
    alloc_frames: Vec<AllocFrame>,
    /// Memo id for the function being emitted, decided once in the
    /// prologue so the epilogue cannot disagree with it.
    memo_id: Option<i64>,
    /// Declared `type` layouts: (module, type) to field names in
    /// declaration order. Harvested from every program before emission,
    /// so a constructor in one module can use a layout from another.
    layouts: HashMap<(String, String), Vec<String>>,
    /// Declared field types: (module, type, field) to the type name as
    /// written (`"Any"` when the field is unannotated). Harvested with the
    /// layouts. Lets method dispatch see through a field access (`q.p.get()`)
    /// without re-reading the AST: the field's declared record type is
    /// resolved exactly the way the checker resolves it.
    field_types: HashMap<(String, String, String), String>,
    /// `from m import T [as U]` in module `cur`: (cur, alias) to
    /// (declaring module, type). A bare `T(...)` in `cur` resolves
    /// through this exactly the way the checker does.
    type_alias: HashMap<(String, String), (String, String)>,
    /// Methods by (declaring module, canonical type, method). Harvested
    /// like layouts; `from m import T` brings T's methods along.
    methods: HashMap<(String, String, String), MethodSig>,
    /// Type-descriptor globals already emitted, so two records in one
    /// module share one descriptor.
    desc_emitted: HashSet<String>,
}

/// A method signature as the backend sees it: explicit parameter names
/// (excluding an implicit `self`), the receiver kind, and the body to
/// emit. Bodies are emitted as ordinary functions under a mangled name.
#[derive(Debug, Clone)]
struct MethodSig {
    params: Vec<String>,
    receiver: nx_ast::ReceiverKind,
    body: Vec<Stmt>,
    span: Span,
}

/// Where a function's allocas have to be spliced back in: the byte offset
/// Where a function's allocas have to be spliced back in: the byte offset
/// just past its `entry:` label, and which buffer it is being written to.
struct AllocFrame {
    to_top: bool,
    at: usize,
    items: Vec<(String, String)>,
}

/// A loop variable's binding, saved so it can be put back when the loop
/// ends.
///
/// Exists because a loop variable is scoped to its loop. An inner `for i`
/// inside an outer `for i` used to destroy the outer binding, which was
/// observable:
///
/// ```text
/// for i in 0..3:
///     for i in 0..2:
///         print("in", i)
///     print("out", i)     # printed the inner loop's last value, not its own
/// ```
struct LoopVarScope {
    name: String,
    saved_slot: Option<String>,
    saved_rep: Option<Ty>,
}

impl LoopVarScope {
    /// Put the loop's binding back, so the enclosing scope sees exactly
    /// what it saw before.
    fn restore(self, g: &mut Gen) {
        g.locals.remove(&self.name);
        g.rep.remove(&self.name);
        if let Some(s) = self.saved_slot {
            g.locals.insert(self.name.clone(), s);
        }
        if let Some(r) = self.saved_rep {
            g.rep.insert(self.name.clone(), r);
        }
    }
}

impl Gen {
    fn new(
        plan: nx_mem::Plan,
        programs: HashMap<String, Program>,
        memo: HashMap<(String, String), i64>,
        types: HashMap<(String, String), nx_types::FnInfo>,
    ) -> Self {
        Self {
            pre: String::new(),
            top: String::new(),
            out: String::new(),
            plan,
            programs,
            memo,
            types,
            unbox_on: std::env::var("NX_NOUNBOX").is_err(),
            rep: HashMap::new(),
            tmp: 0,
            label: 0,
            strc: 0,
            arity: HashMap::new(),
            modrefs: HashMap::new(),
            falias: HashMap::new(),
            locals: HashMap::new(),
            globals: HashSet::new(),
            cur_module: String::new(),
            cur_fn: String::new(),
            cur_block: String::new(),
            to_top: false,
            in_init: false,
            term: None,
            loops: Vec::new(),
            alloc_frames: Vec::new(),
            memo_id: None,
            layouts: HashMap::new(),
            field_types: HashMap::new(),
            type_alias: HashMap::new(),
            methods: HashMap::new(),
            desc_emitted: HashSet::new(),
        }
    }

    fn emit_prelude(&mut self) {
        self.pre.push_str(PRELUDE);
        self.pre.push('\n');
    }

    /// Walk every program's top-level statements for `type` declarations
    /// and `from ... import` type aliases, before any module is emitted.
    /// A constructor in one module can use a layout declared in another,
    /// so this cannot be done lazily during emission.
    fn harvest_layouts(&mut self) {
        let modules: Vec<String> = self.programs.keys().cloned().collect();
        for module in &modules {
            let prog = match self.programs.get(module) {
                Some(p) => p.clone(),
                None => continue,
            };
            for s in &prog.stmts {
                match s {
                    Stmt::TypeDecl { name, fields, .. } => {
                        self.layouts.insert(
                            (module.clone(), name.clone()),
                            fields.iter().map(|f| f.name.clone()).collect(),
                        );
                        for f in fields {
                            self.field_types.insert(
                                (module.clone(), name.clone(), f.name.clone()),
                                f.ty.clone(),
                            );
                        }
                    }
                    // Methods harvest like layouts: the checker has already
                    // enforced the orphan rule, so the named type is declared
                    // in this same module and the name is canonical.
                    Stmt::Impl { type_name, methods, .. } => {
                        for m in methods {
                            self.methods.insert(
                                (module.clone(), type_name.clone(), m.name.clone()),
                                MethodSig {
                                    params: m.params.clone(),
                                    receiver: m.receiver,
                                    body: m.body.clone(),
                                    span: m.span,
                                },
                            );
                        }
                    }
                    Stmt::FromImport { module: m, names, .. } => {
                        for (name, alias) in names {
                            let bind = alias.clone().unwrap_or_else(|| name.clone());
                            self.type_alias.insert(
                                (module.clone(), bind),
                                (m.clone(), name.clone()),
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        // Descriptor globals, one per declared type, in a stable order so
        // the emitted IR is reproducible run to run.
        let mut keys: Vec<(String, String)> = self.layouts.keys().cloned().collect();
        keys.sort();
        for (module, name) in keys {
            self.emit_descriptor(&module, &name);
        }
    }

    /// The static type descriptor for one declared type: the type name and
    /// the field names, which is what the dynamic field-by-name path and
    /// `print` read at runtime.
    fn emit_descriptor(&mut self, module: &str, name: &str) {
        let key = format!("{module}::{name}");
        if !self.desc_emitted.insert(key.clone()) {
            return;
        }
        let fields = match self.layouts.get(&(module.to_string(), name.to_string())) {
            Some(f) => f.clone(),
            None => return,
        };
        let d = mangle_desc(module, name);
        let tname = format!("@.rec.tname.{d}");
        let tn = name.len();
        // The type name as a byte string. Not NUL-terminated: the length
        // travels alongside it, the same convention strings use.
        self.pre.push_str(&format!("{tname} = private constant [{tn} x i8] c\""));
        for b in name.bytes() {
            if (32..=126).contains(&b) && b != b'"' && b != b'\\' {
                self.pre.push(b as char);
            } else {
                self.pre.push_str(&format!("\\{b:02X}"));
            }
        }
        self.pre.push_str("\"\n");
        // One { len, bytes } entry per field name.
        let mut entries = Vec::new();
        for (i, f) in fields.iter().enumerate() {
            let g = format!("@.rec.fname.{d}.{i}");
            self.pre.push_str(&format!("{g} = private constant [{} x i8] c\"", f.len()));
            for b in f.bytes() {
                if (32..=126).contains(&b) && b != b'"' && b != b'\\' {
                    self.pre.push(b as char);
                } else {
                    self.pre.push_str(&format!("\\{b:02X}"));
                }
            }
            self.pre.push_str("\"\n");
            entries.push(format!("%NxRecName {{ i64 {}, ptr {g} }}", f.len()));
        }
        let n = fields.len();
        let names = format!("@.rec.names.{d}");
        self.pre.push_str(&format!("{names} = private constant [{n} x %NxRecName] ["));
        self.pre.push_str(&entries.join(", "));
        self.pre.push_str("]\n");
        // The array's own address is the name table: it points at element
        // zero, which is exactly what the runtime indexes. An extra global
        // holding the pointer would add an indirection the reader would
        // have to load through.
        self.pre.push_str(&format!(
            "@{d} = private constant %NxDesc {{ ptr {tname}, i64 {tn}, i64 {n}, ptr {names} }}\n"
        ));
    }

    /// Resolve a constructor or field-access type name in the current
    /// module to its declaring module, canonical name and layout. Follows
    /// `from ... import` aliases exactly the way the checker does; a local
    /// declaration wins over an alias, matching source-order semantics.
    /// The global fallback covers a canonical name that is reachable but
    /// neither declared nor aliased locally -- the checker guarantees such
    /// a program only when the declaration exists somewhere, and the
    /// sorted search keeps the choice deterministic.
    fn resolve_type(&self, name: &str) -> Option<(String, String, Vec<String>)> {
        self.resolve_type_in(&self.cur_module.clone(), name)
    }

    /// `resolve_type` against an explicit module, so a field's declared
    /// type resolves in its own type's context rather than the caller's.
    fn resolve_type_in(&self, module: &str, name: &str) -> Option<(String, String, Vec<String>)> {
        if let Some(fields) = self.layouts.get(&(module.to_string(), name.to_string())) {
            return Some((module.to_string(), name.to_string(), fields.clone()));
        }
        if let Some((m, t)) = self.type_alias.get(&(module.to_string(), name.to_string())) {
            if let Some(fields) = self.layouts.get(&(m.clone(), t.clone())) {
                return Some((m.clone(), t.clone(), fields.clone()));
            }
        }
        let mut mods: Vec<String> = self.layouts.keys().map(|(m, _)| m.clone()).collect();
        mods.sort();
        mods.dedup();
        for m in mods {
            if let Some(fields) = self.layouts.get(&(m.clone(), name.to_string())) {
                return Some((m, name.to_string(), fields.clone()));
            }
        }
        None
    }

    /// The static type of a field read from a value of a known record
    /// type, from the declaration alone. Mirrors the checker's `field_ty`:
    /// scalars map directly, a name that resolves to a declared type is
    /// that record, and anything else (`Any`, containers, unknown names)
    /// is not statically known here.
    fn declared_field_ty(&self, type_name: &str, field: &str) -> Option<Ty> {
        let (decl_module, canon, _) = self.resolve_type(type_name)?;
        let written = self
            .field_types
            .get(&(decl_module.clone(), canon, field.to_string()))?;
        match written.as_str() {
            "Int" => Some(Ty::Int),
            "Float" => Some(Ty::Float),
            "Bool" => Some(Ty::Bool),
            "Str" => Some(Ty::Str),
            "None" => Some(Ty::None),
            other => {
                let (_, other_canon, _) = self.resolve_type_in(&decl_module, other)?;
                Some(Ty::Record(other_canon))
            }
        }
    }

    /// The static record type behind an arbitrary receiver expression.
    /// A bare variable asks the checker's inference; a field read recurses
    /// through the holder into the field's declared type, so `q.p` is
    /// known when `q` is a record with a record-typed field. Anything
    /// else is dynamic, exactly as before.
    fn static_ty_of(&self, e: &Expr) -> Ty {
        match e {
            Expr::Var(n, _) => self.ty_dispatch(n),
            Expr::Attr { base, attr, .. } => match self.static_ty_of(base) {
                Ty::Record(t) => self.declared_field_ty(&t, attr).unwrap_or(Ty::Unknown),
                _ => Ty::Unknown,
            },
            _ => Ty::Unknown,
        }
    }

    /// Descriptor global for a resolved type.
    fn desc_of(&self, module: &str, name: &str) -> String {
        format!("@{}", mangle_desc(module, name))
    }

    /// Field offset of `field` in a resolved layout, or an error naming
    /// the type rather than the index.
    fn field_index(fields: &[String], type_name: &str, field: &str, span: Span) -> Result<usize, CodegenError> {
        fields.iter().position(|f| f == field).ok_or(err(
            span,
            format!("type '{type_name}' has no field '{field}'"),
        ))
    }

    fn finish(self) -> String {
        format!("{}{}{}", self.pre, self.top, self.out)
    }

    fn reg(&mut self) -> String {
        self.tmp += 1;
        format!("%t{}", self.tmp)
    }

    fn lab(&mut self, hint: &str) -> String {
        self.label += 1;
        format!("{hint}{}", self.label)
    }

    fn w(&mut self, s: &str) {
        if self.to_top {
            self.top.push_str(s);
            self.top.push('\n');
        } else {
            self.out.push_str(s);
            self.out.push('\n');
        }
    }

    /// Open a basic block. Every label in the generated IR goes through
    /// here, so `cur_block` is always the truth about where the next
    /// instruction lands.
    fn block(&mut self, name: &str) {
        self.w(&format!("{name}:"));
        self.cur_block = name.to_string();
    }

    /// Close the current block and continue in a fresh one, unless the
    /// emitter is still in `opened`. Returns the label that now holds
    /// whatever the just-emitted expression produced.
    ///
    /// This exists because a phi has to name the block its incoming value
    /// was computed in, and "the block I opened before emitting that
    /// expression" is only true when the expression stayed straight-line.
    /// `xs[i - 1] > xs[i]` does not: the two checked subtractions each open
    /// an `ovf_ok`/`ovf_done` diamond, so the comparison lands in the last
    /// of those, not in the block the caller opened. Naming the caller's
    /// block there produced IR clang rejects --
    /// `PHI node entries do not match predecessors!` -- because the named
    /// block was never a predecessor of the merge at all.
    ///
    /// The new block has exactly one predecessor, so the value dominates it
    /// and a phi naming it is well formed.
    fn funnel(&mut self, opened: &str) -> String {
        if self.cur_block == opened {
            return opened.to_string();
        }
        let j = self.lab("funnel");
        self.w(&format!("  br label %{j}"));
        self.block(&j);
        j
    }

    // --- entry-block allocation -------------------------------------
    //
    // Every temporary needs stack space, and the obvious place to put an
    // `alloca` is wherever the value is first needed. That is wrong inside
    // a loop: LLVM gives the allocation a fresh address per iteration and
    // only releases it when the enclosing function returns, so a call
    // inside a long loop walks off the end of the stack. A program with a
    // function call in a 35,000-iteration loop died with a stack overflow.
    //
    // Hoisting all allocas to the entry block fixes that and is also what
    // mem2reg needs to promote them to registers at all.

    /// Open an allocas frame at the current position, which must be just
    /// after the function's `entry:` label.
    fn begin_allocs(&mut self) {
        let at = if self.to_top { self.top.len() } else { self.out.len() };
        self.alloc_frames.push(AllocFrame { to_top: self.to_top, at, items: Vec::new() });
    }

    /// Splice the collected allocas in and close the frame.
    fn end_allocs(&mut self) {
        let f = match self.alloc_frames.pop() {
            Some(f) => f,
            None => return,
        };
        if f.items.is_empty() {
            return;
        }
        let mut text = String::new();
        for (reg, ty) in &f.items {
            text.push_str(&format!("  {reg} = alloca {ty}\n"));
        }
        if f.to_top {
            self.top.insert_str(f.at, &text);
        } else {
            self.out.insert_str(f.at, &text);
        }
    }

    /// Reserve stack space of `ty`, hoisted to the current entry block.
    fn alloca(&mut self, ty: &str) -> String {
        let r = self.reg();
        match self.alloc_frames.last_mut() {
            Some(f) => f.items.push((r.clone(), ty.to_string())),
            // No open frame means top-level emission, which cannot happen
            // for a value; fall back to writing it in place.
            None => self.w(&format!("  {r} = alloca {ty}")),
        }
        r
    }

    // --- unboxing -----------------------------------------------------

    /// Static type of `name` in the function being emitted, if known.
    /// Unknowable when unboxing is off, which is what makes the opt-out
    /// reproduce the old all-boxed code exactly.
    fn ty_of(&self, name: &str) -> Ty {
        if !self.unbox_on {
            return Ty::Unknown;
        }
        self.types
            .get(&(self.cur_module.clone(), self.cur_fn.clone()))
            .and_then(|f| f.locals.get(name))
            .cloned()
            .unwrap_or(Ty::Unknown)
    }

    /// Static type of `name` in the function being emitted, ignoring the
    /// unboxing opt-out. Method dispatch needs this: `self`'s type comes
    /// from the receiver rather than from inference, so it is known even in
    /// the all-boxed build. Unboxing is a representation choice, not a
    /// typing one -- a call that resolves in one build must resolve in the
    /// other, or NX_NOUNBOX stops being a debug switch and becomes a
    /// different language.
    fn ty_dispatch(&self, name: &str) -> Ty {
        self.types
            .get(&(self.cur_module.clone(), self.cur_fn.clone()))
            .and_then(|f| f.locals.get(name))
            .cloned()
            .unwrap_or(Ty::Unknown)
    }

    /// Static type of `name` wherever it lives: function locals first,
    /// then module globals. `ty_of` alone misses globals (it keys on the
    /// current function), which is what sent every top-level `push` down
    /// the dynamic path.
    fn ty_of_any(&self, name: &str) -> Ty {
        if self.locals.contains_key(name) {
            return self.ty_of(name);
        }
        self.global_ty(name)
    }

    /// Representation chosen for `name`: Some(t) means the slot holds a
    /// bare `ll_scalar(t)`, None means it holds a boxed `%NxVal`.
    fn rep_of(&self, name: &str) -> Option<Ty> {
        self.rep.get(name).cloned()
    }

    /// Declare a local slot for `name` and record its representation.
    /// `hint` is the static type to unbox into, if any.
    fn new_slot(&mut self, name: &str, hint: Option<Ty>) -> String {
        match hint.as_ref().and_then(ll_scalar) {
            Some(ll) => {
                let slot = self.alloca(ll);
                self.rep.insert(name.to_string(), hint.unwrap());
                self.locals.insert(name.to_string(), slot.clone());
                slot
            }
            None => {
                let slot = self.alloca("%NxVal");
                self.w(&format!("  store %NxVal zeroinitializer, ptr {slot}"));
                self.rep.remove(name);
                self.locals.insert(name.to_string(), slot.clone());
                slot
            }
        }
    }

    /// Load a local as a value of its slot type.
    fn load_slot(&mut self, name: &str) -> NV {
        let slot = match self.locals.get(name).cloned() {
            Some(s) => s,
            None => return NV::dyn_boxed("zeroinitializer".to_string()),
        };
        match self.rep_of(name) {
            Some(t) => {
                let ll = ll_scalar(&t).unwrap();
                let v = self.reg();
                self.w(&format!("  {v} = load {ll}, ptr {slot}"));
                NV::raw(t, v)
            }
            None => {
                let v = self.reg();
                self.w(&format!("  {v} = load %NxVal, ptr {slot}"));
                NV::boxed_known(v, self.ty_of(name))
            }
        }
    }

    /// Whether storing a value of this static type must duplicate container
    /// storage. NX has value semantics for containers: `ys = xs` leaves
    /// `ys` independent, and a function argument never aliases the
    /// caller's value. Scalars need no copy, and strings are never mutated
    /// in place, so sharing one is observably identical to copying it.
    /// Everything else goes through `nx_clone`, which passes non-container
    /// tags through unchanged at runtime.
    fn needs_clone(ty: &Ty) -> bool {
        matches!(ty, Ty::List(_) | Ty::Dict(_) | Ty::Record(_) | Ty::Unknown)
    }

    /// Box a value for storage, cloning container storage so the stored
    /// binding owns its value outright. See `needs_clone`. A fresh value
    /// (newly allocated by this expression) skips the clone: there is no
    /// other owner to separate from, which is what keeps `s = s + a*b`
    /// from copying on every iteration.
    fn store_boxed(&mut self, v: &NV) -> String {
        let b = self.unbox(v);
        if v.fresh || !Self::needs_clone(&v.ty) {
            return b;
        }
        let c = self.reg();
        self.w(&format!("  {c} = call %NxVal @nx_clone(%NxVal {b})"));
        c
    }

    /// Store a value into a local that already owns this storage: the
    /// write-back path after an in-place container update. Unlike
    /// `store_slot`, this never clones -- the value derives from the very
    /// binding it is stored into.
    fn store_slot_owned(&mut self, name: &str, v: &NV) {
        let slot = match self.locals.get(name).cloned() {
            Some(s) => s,
            None => return,
        };
        match self.rep_of(name) {
            Some(t) => {
                let ll = ll_scalar(&t).unwrap();
                let val = self.coerce(v, &t);
                self.w(&format!("  store {ll} {val}, ptr {slot}"));
            }
            None => {
                let val = self.unbox(v);
                self.w(&format!("  store %NxVal {val}, ptr {slot}"));
            }
        }
    }

    /// Store a value into a local, boxing if the slot is dynamic.
    fn store_slot(&mut self, name: &str, v: &NV) {
        let slot = match self.locals.get(name).cloned() {
            Some(s) => s,
            None => return,
        };
        match self.rep_of(name) {
            Some(t) => {
                let ll = ll_scalar(&t).unwrap();
                let val = self.coerce(v, &t);
                self.w(&format!("  store {ll} {val}, ptr {slot}"));
            }
            None => {
                let val = self.store_boxed(v);
                self.w(&format!("  store %NxVal {val}, ptr {slot}"));
            }
        }
    }

    /// Force a value into a boxed `%NxVal`, which is what every dynamic
    /// boundary (call argument, return, list element, global, print)
    /// takes. Returns the incoming register when it is already boxed.
    fn unbox(&mut self, v: &NV) -> String {
        let t = match v.raw.clone() {
            None => return v.reg.clone(),
            Some(t) => t,
        };
        let r = self.reg();
        let call = match t {
            Ty::Int => format!("@nx_int(i64 {})", v.reg),
            Ty::Float => format!("@nx_float(double {})", v.reg),
            Ty::Bool => format!("@nx_bool(i1 {})", v.reg),
            _ => unreachable!("only scalars are held raw"),
        };
        self.w(&format!("  {r} = call %NxVal {call}"));
        r
    }

    /// Payload of a value as a bare `i64`: field 1 of the box, or the
    /// register itself when it is already an integer. Float payloads come
    /// back as raw bits, so callers bitcast when they want a double.
    fn payload(&mut self, v: &NV) -> String {
        match v.raw {
            Some(Ty::Float) => {
                let r = self.reg();
                self.w(&format!("  {r} = bitcast double {} to i64", v.reg));
                r
            }
            Some(Ty::Int) => v.reg.clone(),
            _ => {
                let r = self.reg();
                self.w(&format!("  {r} = extractvalue %NxVal {}, 1", v.reg));
                r
            }
        }
    }

    /// Force a value into an `i1` branch condition.
    fn as_i1(&mut self, v: &NV) -> String {
        match v.raw {
            Some(Ty::Bool) => v.reg.clone(),
            Some(Ty::Int) => {
                let c = self.reg();
                self.w(&format!("  {c} = icmp ne i64 {}, 0", v.reg));
                c
            }
            Some(Ty::Float) => {
                let c = self.reg();
                self.w(&format!("  {c} = fcmp une double {}, 0.0", v.reg));
                c
            }
            _ => {
                // Untyped condition: keep the payload-is-nonzero rule the
                // boxed path has always used (the checker rejects non-Bool
                // conditions in any program that reaches the backend).
                let b = self.unbox(v);
                let r = self.reg();
                self.w(&format!("  {r} = extractvalue %NxVal {b}, 1"));
                let c = self.reg();
                self.w(&format!("  {c} = trunc i64 {r} to i1"));
                c
            }
        }
    }

    /// Force an integer operand to a bare `i64`.
    fn as_i64(&mut self, v: &NV) -> String {
        match v.raw {
            Some(Ty::Int) => v.reg.clone(),
            Some(Ty::Float) => {
                let r = self.reg();
                self.w(&format!("  {r} = fptosi double {} to i64", v.reg));
                r
            }
            _ => {
                let b = self.unbox(v);
                let r = self.reg();
                self.w(&format!("  {r} = extractvalue %NxVal {b}, 1"));
                r
            }
        }
    }

    /// Reinterpret a value as a `double`, unboxing it if needed.
    fn as_f64(&mut self, v: &NV) -> String {
        match v.raw {
            Some(Ty::Float) => v.reg.clone(),
            Some(Ty::Int) => {
                let r = self.reg();
                self.w(&format!("  {r} = sitofp i64 {} to double", v.reg));
                r
            }
            _ => {
                let b = self.unbox(v);
                let p = self.reg();
                self.w(&format!("  {p} = extractvalue %NxVal {b}, 1"));
                let r = self.reg();
                self.w(&format!("  {r} = bitcast i64 {p} to double"));
                r
            }
        }
    }

    /// Read a value as a scalar of its static type in a typed register,
    /// pulling the payload straight out of a box when needed. This is the
    /// bridge that lets a boxed global feed unboxed arithmetic. Returns
    /// None when unboxing is off or the type is unknown, so the caller
    /// takes the boxed path.
    fn as_raw(&mut self, v: &NV) -> Option<NV> {
        if !self.unbox_on {
            return None;
        }
        match v.raw {
            Some(_) => Some(v.clone()),
            None => {
                let t = v.ty.clone();
                let reg = match &t {
                    Ty::Int => self.payload(v),
                    Ty::Float => self.as_f64(v),
                    Ty::Bool => {
                        let p = self.payload(v);
                        let c = self.reg();
                        self.w(&format!("  {c} = trunc i64 {p} to i1"));
                        c
                    }
                    _ => return None,
                };
                // Unboxing preserves literal-ness: a boxed constant is
                // still a constant once its payload is pulled out.
                match v.const_i {
                    Some(k) => Some(NV::raw_const(t, reg, k)),
                    None => Some(NV::raw(t, reg)),
                }
            }
        }
    }

    /// The compile-time value of a register, when it is a literal. Used
    /// where a value has to be validated before emitting a raw
    /// instruction rather than deferred to a runtime helper.
    fn const_int(r: &NV) -> Option<i64> {
        r.const_i
    }

    /// Coerce a value to a target scalar type, inserting the numeric
    /// conversion the runtime's mixed Int/Float operators would apply.
    fn coerce(&mut self, v: &NV, want: &Ty) -> String {
        let src = match self.as_raw(v) {
            Some(s) => s,
            // Untyped value into a typed slot. The checker widens the slot
            // to Unknown when this can happen, so reaching here would mean
            // inference and codegen disagree; take the payload anyway
            // rather than emit a mismatched store.
            None => {
                if ll_scalar(want).is_none() {
                    return self.unbox(v);
                }
                let p = self.payload(v);
                return match want {
                    Ty::Float => {
                        let r = self.reg();
                        self.w(&format!("  {r} = bitcast i64 {p} to double"));
                        r
                    }
                    Ty::Bool => {
                        let r = self.reg();
                        self.w(&format!("  {r} = trunc i64 {p} to i1"));
                        r
                    }
                    _ => p,
                };
            }
        };
        if src.ty == *want {
            return src.reg;
        }
        let r = self.reg();
        match want {
            Ty::Float => self.w(&format!("  {r} = sitofp i64 {} to double", src.reg)),
            Ty::Int => self.w(&format!("  {r} = fptosi double {} to i64", src.reg)),
            _ => self.w(&format!("  {r} = trunc i64 {} to i1", src.reg)),
        }
        r
    }

    fn declare_module_fns(&mut self, module: &str, prog: &Program) {
        for s in &prog.stmts {
            if let Stmt::Fn { name, params, .. } = s {
                self.arity.insert((module.to_string(), name.clone()), params.len());
                self.globals.insert(format!("{module}\0{name}"));
            }
        }
    }

    fn is_module_fn(&self, module: &str, name: &str) -> bool {
        self.arity.contains_key(&(module.to_string(), name.to_string()))
    }

    fn emit_module(&mut self, module: &str, prog: &Program) -> Result<(), CodegenError> {
        self.cur_module = module.to_string();
        // First pass: every name that needs a module-level global, so all of
        // them can be declared before any function body is emitted.
        //
        // This has to reach inside nested blocks. A name first assigned
        // inside one is still a module global. If the pass only saw
        // top-level statements, the global would be discovered *during*
        // emission and appended to `top` in the middle of a function body,
        // which clang rejects with "expected instruction opcode".
        let mut gvars: Vec<String> = Vec::new();
        for s in &prog.stmts {
            collect_module_globals(s, &mut gvars);
        }
        for v in &gvars {
            self.globals.insert(format!("{module}\0{v}"));
            self.top.push_str(&format!("@{} = global %NxVal zeroinitializer\n", mangle_global(module, v)));
        }
        let done = mangle_done(module);
        self.top.push_str(&format!("@{done} = global i1 false\n"));
        for s in &prog.stmts {
            if let Stmt::Fn { name, params, body, .. } = s {
                self.emit_fn(module, name, params, body)?;
            }
        }
        // Methods emit as ordinary functions under mangled names, in a
        // stable order so the IR is reproducible run to run.
        let mut mkeys: Vec<(String, String, String)> =
            self.methods.keys().filter(|(m, _, _)| m == module).cloned().collect();
        mkeys.sort();
        for (m, t, meth) in mkeys {
            if let Some(sig) = self.methods.get(&(m.clone(), t.clone(), meth.clone())).cloned() {
                self.emit_method(&m, &t, &meth, &sig)?;
            }
        }
        // Module init body = top-level statements, guarded for import caching.
        self.in_init = true;
        self.term = None;
        // The init body is a function like any other, and the checker's
        // inference for it is filed under `<top>`. Without this, every
        // type lookup that keys on the current function -- unboxing
        // decisions, method dispatch on a local, the memory plan -- missed,
        // because `cur_fn` was still whatever the last emitted function
        // left behind. Top-level code was therefore never unboxed, and a
        // method call on a top-level local could not resolve.
        let saved_fn = self.cur_fn.clone();
        self.cur_fn = "<top>".to_string();
        let init = mangle_init(module);
        self.w(&format!("define void @{init}() {{"));
        self.block("entry");
        self.begin_allocs();
        let flag = self.reg();
        let run = self.lab("initrun");
        let skip = self.lab("initskip");
        self.w(&format!("  {flag} = load i1, ptr @{done}"));
        self.w(&format!("  br i1 {flag}, label %{skip}, label %{run}"));
        self.block(&format!("{run}"));
        self.w(&format!("  store i1 true, ptr @{done}"));
        for s in &prog.stmts {
            if matches!(s, Stmt::Fn { .. }) {
                continue;
            }
            self.emit_stmt(s)?;
            if self.term.is_some() {
                // Top-level return/break/continue: rejected by the checker,
                // so `nx build` never reaches here. Stop emitting.
                break;
            }
        }
        if self.term.is_none() {
            self.w("  ret void");
        }
        self.block(&format!("{skip}"));
        self.w("  ret void");
        self.w("}");
        self.end_allocs();
        self.in_init = false;
        self.term = None;
        self.cur_fn = saved_fn;
        Ok(())
    }

    fn emit_main(&mut self) {
        self.w("define i32 @main() {");
        self.block("entry");
        self.w(&format!("  call void @{}()", mangle_init("__main__")));
        self.w("  ret i32 0");
        self.w("}");
    }

    fn emit_fn(
        &mut self,
        module: &str,
        name: &str,
        params: &[String],
        body: &[Stmt],
    ) -> Result<(), CodegenError> {
        let fname = mangle_fn(module, name);
        self.emit_fn_inner(
            module,
            &fname,
            Some(&(module.to_string(), name.to_string())),
            name,
            params,
            body,
        )
    }

    /// Emit a method body as an ordinary function. `fname` is the mangled
    /// symbol, `key` the memo-table identity, and `scope` the
    /// type-inference key (`Type.method`, matching the checker's
    /// `inferred` map) used for unboxing decisions inside the body.
    /// `self` (when present) binds positionally like any other parameter,
    /// which under value semantics gives the method its own copy.
    fn emit_method(
        &mut self,
        module: &str,
        type_name: &str,
        method: &str,
        sig: &MethodSig,
    ) -> Result<(), CodegenError> {
        let fname = mangle_method(module, type_name, method);
        let key = (module.to_string(), nx_ast::shape::method_key(type_name, method));
        let mut params = Vec::new();
        if sig.receiver != nx_ast::ReceiverKind::None {
            params.push("self".to_string());
        }
        params.extend(sig.params.clone());
        let sig_body = sig.body.clone();
        let _ = sig.span;
        // A method is never memoized. Purity analysis reasons about a
        // function's own body, but a `mut self` method's contract extends
        // past it: the call writes its result back into the receiver. That
        // write is real, caller-visible effect that a cache hit would skip.
        self.emit_fn_inner(module, &fname, None, &key.1, &params, &sig_body)
    }

    fn emit_fn_inner(
        &mut self,
        module: &str,
        fname: &str,
        memo_key: Option<&(String, String)>,
        scope: &str,
        params: &[String],
        body: &[Stmt],
    ) -> Result<(), CodegenError> {
        self.locals.clear();
        self.rep.clear();
        self.term = None;
        self.w(&format!("define %NxVal @{fname}(%NxVal* %args, i64 %nargs) {{"));
        self.block("entry");
        self.begin_allocs();
        // Memo prologue for purity-proven functions: hit returns cached.
        // `memo_key` is None for anything whose call has effects the cache
        // cannot replay -- methods, whose `mut self` result writes back into
        // the receiver at the call site.
        let fnid = memo_key.and_then(|k| self.memo.get(k).copied());
        self.memo_id = fnid;
        if let Some(id) = fnid {
            let slot = self.alloca("%NxVal");
            let hit = self.reg();
            self.w(&format!("  store %NxVal zeroinitializer, ptr {slot}"));
            self.w(&format!("  {hit} = call i1 @nx_memo_get(i64 {id}, ptr %args, i64 %nargs, ptr {slot})"));
            let go = self.lab("mhit");
            let miss = self.lab("mmiss");
            self.w(&format!("  br i1 {hit}, label %{go}, label %{miss}"));
            self.block(&format!("{go}"));
            let cv = self.reg();
            self.w(&format!("  {cv} = load %NxVal, ptr {slot}"));
            // The cache holds one box shared across calls; the caller gets
            // a copy, or a mutation through one call site would corrupt the
            // next: a memo hit clones, so every call site gets its own copy.
            let cc = self.reg();
            self.w(&format!("  {cc} = call %NxVal @nx_clone(%NxVal {cv})"));
            self.w(&format!("  ret %NxVal {cc}"));
            self.block(&format!("{miss}"));
        }
        let saved = self.cur_module.clone();
        let saved_fn = self.cur_fn.clone();
        self.cur_module = module.to_string();
        self.cur_fn = scope.to_string();
        for (i, p) in params.iter().enumerate() {
            // A proven scalar parameter lands straight in a typed slot;
            // everything else keeps the boxed ABI value.
            let hint = self.unboxed_ty(p);
            self.new_slot(p, hint.clone());
            let ep = self.reg();
            self.w(&format!("  {ep} = getelementptr %NxVal, ptr %args, i64 {i}"));
            let v = self.reg();
            self.w(&format!("  {v} = load %NxVal, ptr {ep}"));
            // The call site is type-checked, so a proven scalar's tag
            // needs no runtime guard: take the payload directly. Anything
            // else is a bind, and binds own their containers outright.
            let val = match hint {
                Some(Ty::Float) => {
                    let p1 = self.reg();
                    self.w(&format!("  {p1} = extractvalue %NxVal {v}, 1"));
                    let d = self.reg();
                    self.w(&format!("  {d} = bitcast i64 {p1} to double"));
                    d
                }
                Some(Ty::Bool) => {
                    let p1 = self.reg();
                    self.w(&format!("  {p1} = extractvalue %NxVal {v}, 1"));
                    let c = self.reg();
                    self.w(&format!("  {c} = trunc i64 {p1} to i1"));
                    c
                }
                Some(_) => {
                    let p1 = self.reg();
                    self.w(&format!("  {p1} = extractvalue %NxVal {v}, 1"));
                    p1
                }
                None => {
                    // A bind owns its containers outright. The static type
                    // decides: scalars (and strings) store as-is, anything
                    // that may hold container storage duplicates it. When
                    // unboxing is off every parameter lands here, so the
                    // static check is what keeps scalar calls cheap.
                    if Self::needs_clone(&self.ty_of(p)) {
                        let vc = self.reg();
                        self.w(&format!("  {vc} = call %NxVal @nx_clone(%NxVal {v})"));
                        vc
                    } else {
                        v.clone()
                    }
                }
            };
            let ll = hint.as_ref().and_then(ll_scalar).unwrap_or("%NxVal");
            self.w(&format!("  store {ll} {val}, ptr {}", self.locals[p].clone()));
        }
        for s in body {
            self.emit_stmt(s)?;
            if self.term.is_some() {
                break;
            }
        }
        if self.term.is_none() {
            // NOTE: free_scope/memo need the function's module/name context.
            self.free_scope();
            self.emit_ret(None);
        }
        self.cur_module = saved;
        self.cur_fn = saved_fn;
        self.w("}");
        self.end_allocs();
        self.locals.clear();
        self.rep.clear();
        self.term = None;
        Ok(())
    }

    /// Emit `ret` for a value, storing it in the memo cache first when
    /// the current function is memoized.
    fn emit_ret(&mut self, reg: Option<&str>) {
        // The memo id comes from the prologue's decision, never from a
        // second lookup: a `put` without a matching `get` would populate
        // the cache with an entry this function never reads, and worse,
        // could reuse another function's id.
        if let Some(id) = self.memo_id {
            if let Some(v) = reg {
                self.w(&format!("  call void @nx_memo_put(i64 {id}, ptr %args, i64 %nargs, %NxVal {v})"));
            }
        }
        match reg {
            Some(v) => self.w(&format!("  ret %NxVal {v}")),
            None => self.w("  ret %NxVal zeroinitializer"),
        }
    }
    fn is_unique(&self, name: &str) -> bool {
        self.locals.contains_key(name)
            && self.plan.alloc_of(&self.cur_module, &self.cur_fn, name) == nx_mem::Alloc::Unique
    }

    /// Free every Unique local currently in scope. An unboxed slot holds
    /// a bare scalar, so there is no buffer to release.
    fn free_scope(&mut self) {
        let mut names: Vec<String> = self.locals.keys().cloned().collect();
        names.sort();
        for n in names {
            if self.plan.alloc_of(&self.cur_module, &self.cur_fn, &n) == nx_mem::Alloc::Unique
                && self.rep_of(&n).is_none()
            {
                if let Some(slot) = self.locals.get(&n).cloned() {
                    let v = self.reg();
                    self.w(&format!("  {v} = load %NxVal, ptr {slot}"));
                    self.w(&format!("  call void @nx_free_val(%NxVal {v})"));
                }
            }
        }
    }

    /// Module-global variable, declared on first use.
    fn ensure_global(&mut self, module: &str, name: &str) -> String {
        let g = mangle_global(module, name);
        if self.globals.insert(format!("{module}\0{name}")) {
            self.top.push_str(&format!("@{g} = global %NxVal zeroinitializer\n"));
        }
        format!("@{g}")
    }

    fn ptr_of(&mut self, name: &str) -> Option<String> {
        if let Some(r) = self.locals.get(name).cloned() {
            return Some(r);
        }
        let g = format!("{}\0{name}", self.cur_module);
        if self.globals.contains(&g) {
            return Some(format!("@{}", mangle_global(&self.cur_module, name)));
        }
        None
    }

    fn resolve(&self, name: &str) -> Result<Binding, (String, String)> {
        // Returns Binding or (kind, detail) for precise errors.
        if self.locals.contains_key(name) {
            return Ok(Binding::Local);
        }
        if let Some(m) = self.modrefs.get(name) {
            return Ok(Binding::Module(m.clone()));
        }
        if let Some((m, f)) = self.falias.get(name) {
            return Ok(Binding::ModuleFn(m.clone(), f.clone()));
        }
        let g = format!("{}\0{name}", self.cur_module);
        if self.globals.contains(&g) {
            return Ok(Binding::Global(mangle_global(&self.cur_module, name)));
        }
        if self.is_module_fn(&self.cur_module, name) {
            return Err(("fn".to_string(), name.to_string()));
        }
        Err(("undef".to_string(), name.to_string()))
    }

    fn emit_stmt(&mut self, stmt: &Stmt) -> Result<(), CodegenError> {
        if self.term.is_some() {
            return Ok(());
        }
        match stmt {
            Stmt::Assign { targets, values, span } => {
                // Several targets against one value is destructuring.
                if targets.len() > 1 && values.len() == 1 {
                    let v = self.emit_expr(&values[0])?;
                    return self.destructure(&v, targets, *span);
                }
                if targets.len() != values.len() {
                    return Err(err(
                        *span,
                        format!("{} targets but {} values", targets.len(), values.len()),
                    ));
                }
                // All right-hand sides are evaluated before any store, so
                // `a, b = b, a` swaps rather than clobbering.
                let mut computed = Vec::with_capacity(values.len());
                for v in values {
                    computed.push(self.emit_expr(v)?);
                }
                for (i, (t, v)) in targets.iter().zip(computed.iter()).enumerate() {
                    // A plain `a = b` has to carry a module alias across,
                    // or a later `a.f()` would resolve against the wrong
                    // module. Anything else clears it.
                    if let nx_ast::Target::Name(name) = t {
                        match values.get(i) {
                            Some(Expr::Var(y, _)) if targets.len() == values.len() => {
                                if let Some(m) = self.modrefs.get(y).cloned() {
                                    self.modrefs.insert(name.clone(), m);
                                } else {
                                    self.modrefs.remove(name);
                                }
                                self.falias.remove(name);
                            }
                            _ => {
                                self.modrefs.remove(name);
                                self.falias.remove(name);
                            }
                        }
                    }
                    self.store_target(t, v, *span)?;
                }
                Ok(())
            }
            Stmt::AssignOp { target, op, value, span } => {
                match target {
                    nx_ast::Target::Name(name) => {
                        let rhs = self.emit_expr(value)?;
                        self.store_compound(name, *op, &rhs, *span)
                    }
                    other => {
                        // Python order with single evaluation: the target's
                        // parts run before the value, and each runs once.
                        // Stash them in hidden `$augN` slots first (`$`
                        // cannot appear in a user identifier, so the names
                        // cannot collide), then read, compute and store off
                        // the slots. Emitting the target inline instead
                        // would run it before the value (reversed) or run
                        // it twice (once to read, once to store).
                        let rebuilt = self.stash_compound_target(other, *span)?;
                        let rhs = self.emit_expr(value)?;
                        let cur = self.emit_expr(&rebuilt.as_expr(*span))?;
                        let v = self.binop_dyn(&cur, *op, &rhs)?;
                        self.store_target(&rebuilt, &v, *span)
                    }
                }
            }
            Stmt::Del { targets, span } => {
                for t in targets {
                    self.del_target(t, *span)?;
                }
                Ok(())
            }
            Stmt::Assert { cond, message, .. } => {
                let c = self.emit_expr(cond)?;
                let b = self.as_i1(&c);
                let ok = self.lab("assert_ok");
                let fail = self.lab("assert_fail");
                let done = self.lab("assert_done");
                self.w(&format!("  br i1 {b}, label %{ok}, label %{fail}"));
                self.block(&format!("{ok}"));
                self.w(&format!("  br label %{done}"));
                // The failing path is a separate block, so an assertion that
                // holds costs one branch and nothing else.
                self.block(&format!("{fail}"));
                match message {
                    Some(m) => {
                        let mv = self.emit_expr(m)?;
                        let mb = self.unbox(&mv);
                        self.w(&format!("  call void @nx_assert_fail_msg(%NxVal {mb})"));
                    }
                    None => self.w("  call void @nx_assert_fail()"),
                }
                self.w("  unreachable");
                self.block(&format!("{done}"));
                Ok(())
            }
            Stmt::TypeDecl { .. } => {
                // A declaration is compile-time only. The layouts are
                // harvested before emission, so there is nothing to emit.
                Ok(())
            }
            Stmt::Impl { .. } => {
                // Methods are harvested before emission and emitted as
                // ordinary functions; there is nothing to emit inline.
                Ok(())
            }
            Stmt::Print { values, .. } => {
                let n = values.len();
                if n == 0 {
                    self.w("  call void @nx_print(ptr null, i64 0)");
                    return Ok(());
                }
                let arr = self.alloca(&format!("[{n} x %NxVal]"));
                for (i, e) in values.iter().enumerate() {
                    let v = self.emit_expr(e)?;
                    let b = self.unbox(&v);
                    let ep = self.reg();
                    self.w(&format!("  {ep} = getelementptr [{n} x %NxVal], ptr {arr}, i64 0, i64 {i}"));
                    self.w(&format!("  store %NxVal {b}, ptr {ep}"));
                }
                let p0 = self.reg();
                self.w(&format!("  {p0} = getelementptr [{n} x %NxVal], ptr {arr}, i64 0, i64 0"));
                self.w(&format!("  call void @nx_print(ptr {p0}, i64 {n})"));
                Ok(())
            }
            Stmt::If { cond, then_body, elifs, else_body, .. } => {
                self.emit_if_full(cond, then_body, elifs, else_body)
            }
            Stmt::While { cond, body, .. } => {
                let condl = self.lab("wcond");
                let bodyl = self.lab("wbody");
                let endl = self.lab("wend");
                self.w(&format!("  br label %{condl}"));
                self.block(&format!("{condl}"));
                let c = self.emit_expr(cond)?;
                let b = self.as_i1(&c);
                self.w(&format!("  br i1 {b}, label %{bodyl}, label %{endl}"));
                self.block(&format!("{bodyl}"));
                self.loops.push((condl.clone(), endl.clone()));
                self.term = None;
                for s in body {
                    self.emit_stmt(s)?;
                    if self.term.is_some() {
                        break;
                    }
                }
                let body_term = self.term.take();
                self.loops.pop();
                match body_term {
                    Some(Term::Ret) => {
                        self.term = Some(Term::Ret);
                        self.block(&format!("{endl}"));
                        self.w("  unreachable");
                    }
                    Some(_) => {
                        // break/continue already branched; no back-edge.
                        self.block(&format!("{endl}"));
                        self.term = None;
                    }
                    None => {
                        self.w(&format!("  br label %{condl}"));
                        self.block(&format!("{endl}"));
                        self.term = None;
                    }
                }
                Ok(())
            }
            Stmt::For { var, iter, body, span } => self.emit_for(var, iter, body, *span),
            Stmt::Fn { .. } => Ok(()),
            Stmt::Return { values, .. } => {
                // Free Unique locals on every exit path, then memoize.
                match values.len() {
                    0 => {
                        self.free_scope();
                        self.emit_ret(None);
                    }
                    1 => {
                        let v = self.emit_expr(&values[0])?;
                        // The ABI is boxed, so returns re-box.
                        let b = self.unbox(&v);
                        self.free_scope();
                        self.emit_ret(Some(&b));
                    }
                    // `return a, b` is a list, which is what the caller's
                    // `a, b = f()` destructures. Same shape as the
                    // the caller destructures, so both forms agree.
                    _ => {
                        let n = values.len() as i64;
                        let l = self.reg();
                        self.w(&format!("  {l} = call %NxVal @nx_new_list(i64 {n})"));
                        let p = self.alloca("%NxVal");
                        self.w(&format!("  store %NxVal {l}, ptr {p}"));
                        for e in values {
                            let v = self.emit_expr(e)?;
                            // The tuple owns its elements.
                            let b = self.store_boxed(&v);
                            self.w(&format!("  call void @nx_listpush(ptr {p}, %NxVal {b})"));
                        }
                        let out = self.reg();
                        self.w(&format!("  {out} = load %NxVal, ptr {p}"));
                        self.free_scope();
                        self.emit_ret(Some(&out));
                    }
                }
                self.term = Some(Term::Ret);
                Ok(())
            }
            Stmt::Break { span } => {
                let exit = self.loops.last().map(|(_, e)| e.clone()).ok_or(err(
                    *span,
                    "break outside loop".to_string(),
                ))?;
                self.w(&format!("  br label %{exit}"));
                self.term = Some(Term::Brk);
                Ok(())
            }
            Stmt::Continue { span } => {
                let cond = self.loops.last().map(|(c, _)| c.clone()).ok_or(err(
                    *span,
                    "continue outside loop".to_string(),
                ))?;
                self.w(&format!("  br label %{cond}"));
                self.term = Some(Term::Ctn);
                Ok(())
            }
            Stmt::Import { module, alias, span } => {
                if !self.arity_known_module(module) && !self.module_has_vars(module) {
                    return Err(err(*span, format!("cannot find module '{module}'")));
                }
                let bind = alias.clone().unwrap_or_else(|| module.clone());
                self.modrefs.insert(bind, module.clone());
                self.w(&format!("  call void @{}()", mangle_init(module)));
                Ok(())
            }
            Stmt::FromImport { module, names, span } => {
                if !self.module_known(module) {
                    return Err(err(*span, format!("cannot find module '{module}'")));
                }
                self.w(&format!("  call void @{}()", mangle_init(module)));
                for (name, alias) in names {
                    let bind = alias.clone().unwrap_or_else(|| name.clone());
                    // A type imports as an alias only: types are constructed,
                    // never held, so there is no global to load. The alias
                    // map (harvested up front) is what `Pt(...)` resolves
                    // through, and the descriptor global is shared.
                    if self.layouts.contains_key(&(module.clone(), name.clone())) {
                        continue;
                    }
                    if self.is_module_fn(module, name) {
                        self.falias.insert(bind, (module.clone(), name.clone()));
                    } else {
                        let src = mangle_global(module, name);
                        let v = self.reg();
                        self.w(&format!("  {v} = load %NxVal, ptr @{src}"));
                        // A from-imported module global: read boxed here,
                        // but its declared type still lets later uses of
                        // the local stay unboxed.
                        let ty = self
                            .types
                            .get(&(module.clone(), "<top>".to_string()))
                            .and_then(|f| f.locals.get(name))
                            .cloned()
                            .unwrap_or(Ty::Unknown);
                        self.store_fresh(&bind, &NV::boxed_known(v, ty));
                    }
                }
                Ok(())
            }
            Stmt::Expr(e) => {
                self.emit_expr(e)?;
                Ok(())
            }
        }
    }

    /// Store to a name, allocating a local or using the module global.
    /// Write through an assignment target.
    ///
    /// A name goes through the ordinary store, so module globals and Unique
    /// locals behave exactly as they did before. An index or dict key is the
    /// interesting case: it has to write into the container's own storage,
    /// and `nx_dictset` / `nx_listset` update the value in place, which is
    /// what makes `a[i] = v` visible to every other reference to `a`.
    fn store_target(
        &mut self,
        target: &nx_ast::Target,
        v: &NV,
        span: Span,
    ) -> Result<(), CodegenError> {
        match target {
            nx_ast::Target::Name(name) => self.store_name(name, v, span),
            nx_ast::Target::Index { base, index } => self.store_index(base, index, v, span),
            nx_ast::Target::Attr { base, field } => {
                let b = self.emit_expr(base)?;
                let bv = self.unbox(&b);
                let vb = self.store_boxed(v);
                match &b.ty {
                    Ty::Record(t) => {
                        let (_, _, fields) = self
                            .resolve_type(t)
                            .ok_or(err(span, format!("unknown type '{t}'")))?;
                        let i = Self::field_index(&fields, t, field, span)?;
                        self.w(&format!(
                            "  call void @nx_rec_set(%NxVal {bv}, i64 {i}, %NxVal {vb})"
                        ));
                        Ok(())
                    }
                    // A dynamic base resolves the field by name at runtime.
                    Ty::Unknown => {
                        let s = self.emit_str(field, span)?;
                        let nv = NV::boxed_known(s, Ty::Str);
                        let pbits = self.payload(&nv);
                        let p = self.reg();
                        self.w(&format!("  {p} = inttoptr i64 {pbits} to ptr"));
                        self.w(&format!(
                            "  call void @nx_rec_setn(%NxVal {bv}, ptr {p}, i64 {}, %NxVal {vb})",
                            field.len()
                        ));
                        Ok(())
                    }
                    _ => Err(err(span, "only types have fields".to_string())),
                }
            }
        }
    }

    /// `a[i] = v` or `d[k] = v`.
    ///
    /// A list is written in place through its header, so the change is
    /// visible to every other reference to that list -- aliasing a list and
    /// then writing to it has to behave the same as writing to the original.
    /// A dict's length can grow, which reallocates its storage, so the
    /// updated value is written back to whoever holds it.
    fn store_index(
        &mut self,
        base: &Expr,
        index: &Expr,
        v: &NV,
        _span: Span,
    ) -> Result<(), CodegenError> {
        let b = self.emit_expr(base)?;
        let ix = self.emit_expr(index)?;
        let bv = self.unbox(&b);
        // The stored value is owned by the container, so it is duplicated
        // on the way in -- the same rule as every other store.
        let sv = self.store_boxed(v);
        if matches!(b.ty, Ty::Dict(_)) {
            let kb = self.unbox(&ix);
            let vb = sv;
            // nx_dictset updates the mirrored length in place, so it needs
            // an addressable copy of the dict value.
            let p = self.alloca("%NxVal");
            self.w(&format!("  store %NxVal {bv}, ptr {p}"));
            self.w(&format!("  call void @nx_dictset(ptr {p}, %NxVal {kb}, %NxVal {vb})"));
            let updated = self.reg();
            self.w(&format!("  {updated} = load %NxVal, ptr {p}"));
            let nty = Ty::Dict(Box::new(Ty::Unknown));
            return self.write_back(base, &NV::boxed_known(updated, nty));
        }
        if matches!(b.ty, Ty::Unknown) {
            // Unresolved base: the tag decides list versus dict at runtime.
            // The updated value comes back out because a dict may have
            // grown, which moves its mirrored length.
            let kb = self.unbox(&ix);
            let out = self.reg();
            self.w(&format!(
                "  {out} = call %NxVal @nx_storeindex(%NxVal {bv}, %NxVal {kb}, %NxVal {sv})"
            ));
            return self.write_back(base, &NV::boxed_known(out, Ty::Unknown));
        }
        let k = self.as_i64(&ix);
        self.w(&format!(
            "  call void @nx_listset(%NxVal {bv}, i64 {k}, %NxVal {sv})"
        ));
        Ok(())
    }

    /// After an in-place container update the header pointer and length may
    /// have moved, so the owning binding is refreshed. `a` is the common
    /// case and a nested index writes its container back through the same
    /// path recursively.
    fn write_back(&mut self, base: &Expr, updated: &NV) -> Result<(), CodegenError> {
        match base {
            Expr::Var(name, _) => {
                // No clone: `updated` derives from this same binding's
                // storage (an in-place update refreshed the header or
                // length), so there is no second owner to separate from.
                // Cloning here would deep-copy the container on every
                // indexed write.
                if !self.locals.contains_key(name) && self.in_init {
                    let g = self.ensure_global(&self.cur_module.clone(), name);
                    let b = self.unbox(updated);
                    self.w(&format!("  store %NxVal {b}, ptr {g}"));
                } else if self.locals.contains_key(name) {
                    self.store_slot_owned(name, updated);
                } else {
                    self.new_slot(name, None);
                    self.store_slot_owned(name, updated);
                }
                Ok(())
            }
            Expr::Index { base: inner, index, span } => {
                // `a[i][j] = v`: the element list is written back into
                // `a[i]`, and its own length has to be refreshed too.
                self.store_index(inner, index, updated, *span)
            }
            _ => Ok(()),
        }
    }

    /// A hidden `$augN` slot for one evaluated value. `$` cannot appear in
    /// a user identifier (the lexer rejects it), so the name cannot collide
    /// with any binding the program declares -- including another temp.
    fn temp_slot(&mut self) -> String {
        let n = self.tmp;
        self.tmp += 1;
        let name = format!("$aug{n}");
        self.new_slot(&name, None);
        name
    }

    /// Evaluate a compound-assignment target's parts once, in order, and
    /// rebuild the target off hidden slots holding the results. The caller
    /// then emits the value, reads the rebuilt target, computes, and
    /// stores back -- every source expression runs exactly once, target
    /// parts before the value, which is Python's evaluation order.
    ///
    /// The slots are boxed and unknown to the memory planner, so they read
    /// as `Shared`: nothing is freed early, and the final store clones on
    /// the way into the container exactly as a direct emission would.
    fn stash_compound_target(
        &mut self,
        target: &nx_ast::Target,
        span: Span,
    ) -> Result<nx_ast::Target, CodegenError> {
        match target {
            nx_ast::Target::Name(_) => Ok(target.clone()),
            nx_ast::Target::Index { base, index } => {
                let b = self.emit_expr(base)?;
                let tb = self.temp_slot();
                self.store_slot_owned(&tb, &b);
                let ix = self.emit_expr(index)?;
                let ti = self.temp_slot();
                self.store_slot_owned(&ti, &ix);
                Ok(nx_ast::Target::Index {
                    base: Box::new(Expr::Var(tb, span)),
                    index: Box::new(Expr::Var(ti, span)),
                })
            }
            nx_ast::Target::Attr { base, field } => {
                let b = self.emit_expr(base)?;
                let tb = self.temp_slot();
                self.store_slot_owned(&tb, &b);
                Ok(nx_ast::Target::Attr {
                    base: Box::new(Expr::Var(tb, span)),
                    field: field.clone(),
                })
            }
        }
    }

    /// `a, b = f()` -- pull a list's elements into separate bindings.
    fn destructure(
        &mut self,
        v: &NV,
        targets: &[nx_ast::Target],
        span: Span,
    ) -> Result<(), CodegenError> {
        let b = self.unbox(v);
        for (i, t) in targets.iter().enumerate() {
            let idx = self.emit_i64(i as i64);
            let idxreg = self.unbox(&idx);
            let item = self.reg();
            self.w(&format!(
                "  {item} = call %NxVal @nx_index(%NxVal {b}, %NxVal {idxreg})"
            ));
            // Unpacked elements keep whatever type the list carried; a
            // heterogeneous tuple therefore stays dynamic, which is correct.
            let ety = match &v.ty {
                Ty::List(t) => (**t).clone(),
                _ => Ty::Unknown,
            };
            let nv = NV::boxed_known(item, ety);
            self.store_target(t, &nv, span)?;
        }
        Ok(())
    }

    /// `del a`, `del a[i]`, `del d[k]`.
    fn del_target(&mut self, target: &nx_ast::Target, span: Span) -> Result<(), CodegenError> {
        match target {
            nx_ast::Target::Name(name) => {
                // Unbind by rebinding to None: NX has no destructors, and
                // dropping the only reference to a Unique buffer would leak
                // it. The binding goes dead for every later read.
                self.modrefs.remove(name);
                self.falias.remove(name);
                // Rebinding to None is enough: NX has no destructors, so the
                // buffer a Unique local held is released by the process
                // rather than here. What matters is that later reads see
                // a defined binding instead of stale data.
                let n = self.reg();
                self.w(&format!("  {n} = call %NxVal @nx_none()"));
                self.store_name(name, &NV::boxed_known(n, Ty::None), span)
            }
            nx_ast::Target::Index { base, index } => {
                let b = self.emit_expr(base)?;
                let ix = self.emit_expr(index)?;
                // An unresolved base dispatches on the tag at runtime;
                // the updated container comes back out for the write-back.
                if matches!(b.ty, Ty::Unknown) {
                    let bv = self.unbox(&b);
                    let kb = self.unbox(&ix);
                    let out = self.reg();
                    self.w(&format!(
                        "  {out} = call %NxVal @nx_delindex(%NxVal {bv}, %NxVal {kb})"
                    ));
                    return self.write_back(base, &NV::boxed_known(out, Ty::Unknown));
                }
                match b.ty {
                    Ty::Dict(_) => {
                        let bv = self.unbox(&b);
                        let kb = self.unbox(&ix);
                        let out = self.reg();
                        self.w(&format!(
                            "  {out} = call %NxVal @nx_dictdel(%NxVal {bv}, %NxVal {kb})"
                        ));
                        self.write_back(base, &NV::boxed_known(out, Ty::Dict(Box::new(Ty::Unknown))))
                    }
                    _ => {
                        let bv = self.unbox(&b);
                        let k = self.as_i64(&ix);
                        let out = self.reg();
                        self.w(&format!(
                            "  {out} = call %NxVal @nx_listdel(%NxVal {bv}, i64 {k})"
                        ));
                        let bt = match &b.ty {
                            Ty::List(t) => Ty::List(t.clone()),
                            other => other.clone(),
                        };
                        self.write_back(base, &NV::boxed_known(out, bt))
                    }
                }
            }
            nx_ast::Target::Attr { base, field } => {
                let b = self.emit_expr(base)?;
                let bv = self.unbox(&b);
                // A record's arity is fixed, so removing a value means
                // blanking the field rather than changing the layout --
                // a record's arity is fixed, so deleting a field blanks it.
                let none = self.reg();
                self.w(&format!("  {none} = call %NxVal @nx_none()"));
                match &b.ty {
                    Ty::Record(t) => {
                        let (_, _, fields) = self
                            .resolve_type(t)
                            .ok_or(err(span, format!("unknown type '{t}'")))?;
                        let i = Self::field_index(&fields, t, field, span)?;
                        self.w(&format!(
                            "  call void @nx_rec_set(%NxVal {bv}, i64 {i}, %NxVal {none})"
                        ));
                        Ok(())
                    }
                    Ty::Unknown => {
                        let s = self.emit_str(field, span)?;
                        let nv = NV::boxed_known(s, Ty::Str);
                        let pbits = self.payload(&nv);
                        let p = self.reg();
                        self.w(&format!("  {p} = inttoptr i64 {pbits} to ptr"));
                        self.w(&format!(
                            "  call void @nx_rec_setn(%NxVal {bv}, ptr {p}, i64 {}, %NxVal {none})",
                            field.len()
                        ));
                        Ok(())
                    }
                    _ => Err(err(span, "only types have fields".to_string())),
                }
            }
        }
    }

    /// `name op= value`, preserving the unboxed fast path for scalars.
    fn store_compound(
        &mut self,
        name: &str,
        op: BinOp,
        rhs: &NV,
        span: Span,
    ) -> Result<(), CodegenError> {
        // Local slot: reuse the scalar path when the variable's
        // representation allows it, so `x += 1` stays unboxed.
        //
        // No `in_init` guard: `rep_of` is Some only for a name that has a
        // local slot, which is what this is really asking. The old guard
        // also skipped every top-level compound assignment, so `t += 1` in
        // module-level code went through the boxed helper even though the
        // slot was a bare i64 right there.
        if self.rep_of(name).is_some() {
            let cur = self.load_slot(name);
            if let Some(v) = self.emit_named_binop(&cur, op, rhs) {
                self.store_slot(name, &v);
                return Ok(());
            }
        }
        let ptr = self
            .ptr_of(name)
            .ok_or(err(span, format!("undefined variable '{name}'")))?;
        let cur = self.reg();
        self.w(&format!("  {cur} = load %NxVal, ptr {ptr}"));
        let cur_v = NV::dyn_boxed(cur.clone());
        let v = self.binop_dyn(&cur_v, op, rhs)?;
        let b = self.unbox(&v);
        if self.is_unique(name) {
            self.w(&format!("  call void @nx_free_val(%NxVal {cur})"));
        }
        self.w(&format!("  store %NxVal {b}, ptr {ptr}"));
        Ok(())
    }

    /// Apply a binary operator, taking the unboxed instruction path when
    /// both operands are proven scalars and the boxed helper otherwise.
    fn binop_dyn(&mut self, l: &NV, op: BinOp, r: &NV) -> Result<NV, CodegenError> {
        if let Some(v) = self.emit_scalar_binop(l, op, r) {
            return Ok(v);
        }
        let lb = self.unbox(l);
        let rb = self.unbox(r);
        let out = self.reg();
        match op {
            BinOp::Add => self.w(&format!("  {out} = call %NxVal @nx_add(%NxVal {lb}, %NxVal {rb})")),
            BinOp::Sub => self.w(&format!("  {out} = call %NxVal @nx_sub(%NxVal {lb}, %NxVal {rb})")),
            BinOp::Mul => self.w(&format!("  {out} = call %NxVal @nx_mul(%NxVal {lb}, %NxVal {rb})")),
            BinOp::Div => self.w(&format!("  {out} = call %NxVal @nx_div(%NxVal {lb}, %NxVal {rb})")),
            BinOp::Mod => self.w(&format!("  {out} = call %NxVal @nx_mod(%NxVal {lb}, %NxVal {rb})")),
            BinOp::FloorDiv => {
                self.w(&format!("  {out} = call %NxVal @nx_floordiv(%NxVal {lb}, %NxVal {rb})"))
            }
            BinOp::Pow => self.w(&format!("  {out} = call %NxVal @nx_pow(%NxVal {lb}, %NxVal {rb})")),
            BinOp::BitAnd => {
                self.w(&format!("  {out} = call %NxVal @nx_bitand(%NxVal {lb}, %NxVal {rb})"))
            }
            BinOp::BitOr => {
                self.w(&format!("  {out} = call %NxVal @nx_bitor(%NxVal {lb}, %NxVal {rb})"))
            }
            BinOp::BitXor => {
                self.w(&format!("  {out} = call %NxVal @nx_bitxor(%NxVal {lb}, %NxVal {rb})"))
            }
            BinOp::Shl => self.w(&format!("  {out} = call %NxVal @nx_shl(%NxVal {lb}, %NxVal {rb})")),
            BinOp::Shr => self.w(&format!("  {out} = call %NxVal @nx_shr(%NxVal {lb}, %NxVal {rb})")),
            BinOp::In => {
                let c = self.reg();
                self.w(&format!("  {c} = call %NxVal @nx_in(%NxVal {lb}, %NxVal {rb})"));
                return Ok(NV::fresh_boxed(c, Ty::Bool));
            }
            BinOp::NotIn => {
                let c = self.reg();
                let n = self.reg();
                self.w(&format!("  {c} = call %NxVal @nx_in(%NxVal {lb}, %NxVal {rb})"));
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
            BinOp::Mod | BinOp::FloorDiv | BinOp::Pow | BinOp::BitAnd | BinOp::BitOr
            | BinOp::BitXor | BinOp::Shl | BinOp::Shr => Ty::Int,
            _ => Ty::Unknown,
        };
        // Every helper above allocates its result box, so it is fresh.
        Ok(NV::fresh_boxed(out, ty))
    }

    fn emit_cmp_dyn(&mut self, l: &NV, op: BinOp, r: &NV) -> Result<NV, CodegenError> {
        let lb = self.unbox(l);
        let rb = self.unbox(r);
        let out = self.reg();
        match op {
            BinOp::Eq => self.w(&format!("  {out} = call %NxVal @nx_eq(%NxVal {lb}, %NxVal {rb})")),
            BinOp::NotEq => {
                let c = self.reg();
                self.w(&format!("  {c} = call %NxVal @nx_eq(%NxVal {lb}, %NxVal {rb})"));
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
                self.w(&format!("  {c} = call i32 @nx_cmp(%NxVal {lb}, %NxVal {rb})"));
                self.w(&format!("  {b} = icmp {pred} i32 {c}, 0"));
                self.w(&format!("  {out} = call %NxVal @nx_bool(i1 {b})"));
            }
            _ => unreachable!(),
        }
        // `nx_bool` allocates its result, like every other helper here.
        Ok(NV::fresh_boxed(out, Ty::Bool))
    }

    fn store_name(&mut self, name: &str, v: &NV, _span: Span) -> Result<(), CodegenError> {
        // A local slot wins over module scope when one exists.
        //
        // The order matters and used to be wrong. A loop variable has a slot
        // (it is scoped to its loop), but this function used to test
        // `in_init` first and write to a module global instead -- so a
        // `mut self` method's write-back landed in module scope while the
        // read on the next line read the local slot. The update was
        // silently lost, which is the worst shape a bug can have: the code
        // compiled, ran, and printed the wrong thing.
        //
        // A genuine module variable has no local slot, so it still reaches
        // the global branch, which other modules read by address.
        if self.locals.contains_key(name) {
            // Rebinding a Unique local: release the old buffers first. An
            // unboxed slot holds a bare scalar with nothing to free.
            if self.is_unique(name) && self.rep_of(name).is_none() {
                let slot = self.locals[name].clone();
                let old = self.reg();
                self.w(&format!("  {old} = load %NxVal, ptr {slot}"));
                self.w(&format!("  call void @nx_free_val(%NxVal {old})"));
            }
            self.store_slot(name, v);
            return Ok(());
        }
        if self.in_init {
            let g = self.ensure_global(&self.cur_module.clone(), name);
            let b = self.store_boxed(v);
            self.w(&format!("  store %NxVal {b}, ptr {g}"));
            return Ok(());
        }
        self.new_slot(name, self.unboxed_ty(name));
        self.store_slot(name, v);
        Ok(())
    }

/// Always-allocate store (from-imports, loop vars).
    fn store_fresh(&mut self, name: &str, v: &NV) {
        if self.in_init {
            let g = self.ensure_global(&self.cur_module.clone(), name);
            let b = self.store_boxed(v);
            self.w(&format!("  store %NxVal {b}, ptr {g}"));
            return;
        }
        // Loop variables and imports are fresh bindings: any previous slot
        // for the name (a loop re-entry, a shadowed import) is replaced.
        self.locals.remove(name);
        self.rep.remove(name);
        self.new_slot(name, self.unboxed_ty(name));
        self.store_slot(name, v);
    }

    /// Bind a loop variable for the duration of its body, and return the
    /// scope that puts the previous binding back.
    ///
    /// Always a local slot, never a module global — including at top level,
    /// where `store_fresh` would take the `in_init` path. A loop variable is
    /// not visible after the loop, so giving it module scope leaks it into
    /// every later statement and lets two loops collide on the name. Inside
    /// a nested scope that the collision would make two bindings share one
    /// slot.
    fn bind_loop_var(&mut self, name: &str, v: &NV) -> LoopVarScope {
        let saved_slot = self.locals.remove(name);
        let saved_rep = self.rep.remove(name);
        self.new_slot(name, self.unboxed_ty(name));
        self.store_slot(name, v);
        LoopVarScope { name: name.to_string(), saved_slot, saved_rep }
    }

    /// Representation a fresh local should get: the inferred scalar type
    /// when unboxing is on, otherwise None for a boxed slot.
    fn unboxed_ty(&self, name: &str) -> Option<Ty> {
        if !self.unbox_on {
            return None;
        }
        let t = self.ty_of(name);
        ll_scalar(&t)?;
        Some(t)
    }

    fn arity_known_module(&self, module: &str) -> bool {
        self.arity.keys().any(|(m, _)| m == module)
    }

    fn module_has_vars(&self, _module: &str) -> bool {
        // Vars are discovered during emission; the loader guarantees the
        // module exists, so any import we emit for was resolvable.
        // (Unknown members are the checker's job.)
        true
    }

    fn module_known(&self, _module: &str) -> bool {
        true
    }

    fn emit_if_full(
        &mut self,
        cond: &Expr,
        then_body: &[Stmt],
        elifs: &[(Expr, Vec<Stmt>)],
        else_body: &Option<Vec<Stmt>>,
    ) -> Result<(), CodegenError> {
        let endl = self.lab("iend");
        // Every arm gets: cond -> br body/next; body falls to endl or returns.
        // Returns true if the merge is reachable (i.e. some path falls through).
        let mut reachable = false;
        let mut arms: Vec<(&Expr, &[Stmt])> = vec![(cond, then_body)];
        for (ec, eb) in elifs {
            arms.push((ec, eb));
        }
        for (c, b) in arms {
            let bodyl = self.lab("ibranch");
            let next = self.lab("inext");
            let cv = self.emit_expr(c)?;
            let bv = self.as_i1(&cv);
            self.w(&format!("  br i1 {bv}, label %{bodyl}, label %{next}"));
            self.block(&format!("{bodyl}"));
            self.term = None;
            for s in b {
                self.emit_stmt(s)?;
                if self.term.is_some() {
                    break;
                }
            }
            if self.term.is_none() {
                self.w(&format!("  br label %{endl}"));
                reachable = true;
            }
            self.term = None;
            self.block(&format!("{next}"));
        }
        if let Some(b) = else_body {
            self.term = None;
            for s in b {
                self.emit_stmt(s)?;
                if self.term.is_some() {
                    break;
                }
            }
            if self.term.is_none() {
                self.w(&format!("  br label %{endl}"));
                reachable = true;
            }
            self.term = None;
        } else {
            // All-false path falls through to the merge.
            self.w(&format!("  br label %{endl}"));
            reachable = true;
        }
        self.block(&format!("{endl}"));
        if reachable {
            self.term = None;
        } else {
            self.w("  unreachable");
            self.term = Some(Term::Ret);
        }
        Ok(())
    }

    fn emit_for(
        &mut self,
        var: &str,
        iter: &nx_ast::ForIter,
        body: &[Stmt],
        span: Span,
    ) -> Result<(), CodegenError> {
        match iter {
            nx_ast::ForIter::Range { start, end } => {
                let s = self.emit_expr(start)?;
                let e = self.emit_expr(end)?;
                let a = self.as_i64(&s);
                let b = self.as_i64(&e);
                let up = self.reg();
                let step = self.reg();
                self.w(&format!("  {up} = icmp sle i64 {a}, {b}"));
                self.w(&format!("  {step} = select i1 {up}, i64 1, i64 -1"));
                let slot = self.alloca("i64");
                self.w(&format!("  store i64 {a}, ptr {slot}"));
                let condl = self.lab("fcond");
                let bodyl = self.lab("fbody");
                let endl = self.lab("fend");
                // `continue` has to advance the induction variable, so it lands
                // here rather than on the condition. Jumping straight back to
                // the condition re-tests the same index and never terminates.
                let latchl = self.lab("flatch");
                self.w(&format!("  br label %{condl}"));
                self.block(&format!("{condl}"));
                let cur = self.reg();
                let go = self.reg();
                let goup = self.reg();
                let godn = self.reg();
                self.w(&format!("  {cur} = load i64, ptr {slot}"));
                self.w(&format!("  {goup} = icmp slt i64 {cur}, {b}"));
                self.w(&format!("  {godn} = icmp sgt i64 {cur}, {b}"));
                self.w(&format!("  {go} = select i1 {up}, i1 {goup}, i1 {godn}"));
                self.w(&format!("  br i1 {go}, label %{bodyl}, label %{endl}"));
                self.block(&format!("{bodyl}"));
                // The induction variable is statically Int.
                let iv = NV::raw(Ty::Int, cur.clone());
                let scope = self.bind_loop_var(var, &iv);
                self.loops.push((latchl.clone(), endl.clone()));
                self.term = None;
                for st in body {
                    self.emit_stmt(st)?;
                    if self.term.is_some() {
                        break;
                    }
                }
                let bt = self.term.take();
                self.loops.pop();
                // The loop variable's scope ends with the loop, so the
                // enclosing binding is visible again from here on.
                scope.restore(self);
                match bt {
                    Some(Term::Ret) => {
                        self.term = Some(Term::Ret);
                        self.block(&format!("{endl}"));
                        self.w("  unreachable");
                    }
                    Some(_) => {
                        self.block(&format!("{endl}"));
                        self.term = None;
                    }
                    None => {
                        self.w(&format!("  br label %{latchl}"));
                        self.block(&format!("{latchl}"));
                        let cur3 = self.reg();
                        let nxt = self.reg();
                        self.w(&format!("  {cur3} = load i64, ptr {slot}"));
                        self.w(&format!("  {nxt} = add i64 {cur3}, {step}"));
                        self.w(&format!("  store i64 {nxt}, ptr {slot}"));
                        self.w(&format!("  br label %{condl}"));
                        self.block(&format!("{endl}"));
                        self.term = None;
                    }
                }
                let _ = span;
                Ok(())
            }
            nx_ast::ForIter::Each(e) => {
                let v = self.emit_expr(e)?;
                let vb = self.unbox(&v);
                let is_dict = matches!(v.ty, Ty::Dict(_));
                let len = self.reg();
                let n = self.reg();
                self.w(&format!("  {len} = call %NxVal @nx_len(%NxVal {vb})"));
                self.w(&format!("  {n} = extractvalue %NxVal {len}, 1"));
                let islot = self.alloca("i64");
                self.w(&format!("  store i64 0, ptr {islot}"));
                let condl = self.lab("econd");
                let bodyl = self.lab("ebody");
                let endl = self.lab("eend");
                // See the range arm: `continue` must go through the increment.
                let latchl = self.lab("elatch");
                self.w(&format!("  br label %{condl}"));
                self.block(&format!("{condl}"));
                let i = self.reg();
                let go = self.reg();
                self.w(&format!("  {i} = load i64, ptr {islot}"));
                self.w(&format!("  {go} = icmp slt i64 {i}, {n}"));
                self.w(&format!("  br i1 {go}, label %{bodyl}, label %{endl}"));
                self.block(&format!("{bodyl}"));
                let el = self.reg();
                let scope = if is_dict {
                    // Iterating a dict yields its keys, in insertion order.
                    // The runtime helper reads entry `i`'s key directly,
                    // which avoids building the key list first.
                    self.w(&format!(
                        "  {el} = call %NxVal @nx_dictkeyat(%NxVal {vb}, i64 {i})"
                    ));
                    self.bind_loop_var(var, &NV::dyn_boxed(el))
                } else if matches!(v.ty, Ty::Unknown) {
                    // Unresolved iterable: the tag decides list, string or
                    // dict at runtime. A dict yields its keys, matching the
                    // the statically-known-dict path.
                    self.w(&format!(
                        "  {el} = call %NxVal @nx_each(%NxVal {vb}, i64 {i})"
                    ));
                    self.bind_loop_var(var, &NV::dyn_boxed(el))
                } else {
                    let iv = self.reg();
                    let ix = NV::raw(Ty::Int, i.clone());
                    let ivb = self.unbox(&ix);
                    self.w(&format!("  {iv} = call %NxVal @nx_int(i64 {i})"));
                    self.w(&format!("  {el} = call %NxVal @nx_index(%NxVal {vb}, %NxVal {ivb})"));
                    // Element type comes from the list, so a list of scalars
                    // iterates without re-boxing. as_raw declines when
                    // unboxing is off, leaving the box in place.
                    let elem_ty = match &v.ty {
                        Ty::List(t) => (**t).clone(),
                        Ty::Str => Ty::Str,
                        _ => Ty::Unknown,
                    };
                    let boxed_elem = NV::boxed_known(el, elem_ty);
                    let ev = self.as_raw(&boxed_elem).unwrap_or(boxed_elem);
                    self.bind_loop_var(var, &ev)
                };
                self.loops.push((latchl.clone(), endl.clone()));
                self.term = None;
                for st in body {
                    self.emit_stmt(st)?;
                    if self.term.is_some() {
                        break;
                    }
                }
                let bt = self.term.take();
                self.loops.pop();
                // The loop variable's scope ends with the loop, so the
                // enclosing binding is visible again from here on.
                scope.restore(self);
                match bt {
                    Some(Term::Ret) => {
                        self.term = Some(Term::Ret);
                        self.block(&format!("{endl}"));
                        self.w("  unreachable");
                    }
                    Some(_) => {
                        self.block(&format!("{endl}"));
                        self.term = None;
                    }
                    None => {
                        self.w(&format!("  br label %{latchl}"));
                        self.block(&format!("{latchl}"));
                        let i2 = self.reg();
                        let i3 = self.reg();
                        self.w(&format!("  {i2} = load i64, ptr {islot}"));
                        self.w(&format!("  {i3} = add i64 {i2}, 1"));
                        self.w(&format!("  store i64 {i3}, ptr {islot}"));
                        self.w(&format!("  br label %{condl}"));
                        self.block(&format!("{endl}"));
                        self.term = None;
                    }
                }
                let _ = span;
                Ok(())
            }
        }
    }

    /// Emit a fully unboxed integer literal. LLVM folds the instruction,
    /// so this costs nothing and keeps one code path for scalars.
    fn emit_i64(&mut self, i: i64) -> NV {
        let r = self.reg();
        self.w(&format!("  {r} = add i64 {i}, 0"));
        NV::raw_const(Ty::Int, r, i)
    }

    /// Append `n` consecutive Ints starting at `start` to the list at `p`.
    /// The counter lives in an entry-block alloca so the loop does not
    /// grow the frame on every iteration.
    fn emit_range_fill(&mut self, p: &str, start: &str, n: &str) {
        let ireg = self.alloca("i64");
        self.w(&format!("  store i64 0, ptr {ireg}"));
        let condl = self.lab("rng_cond");
        let bodyl = self.lab("rng_body");
        let endl = self.lab("rng_end");
        self.w(&format!("  br label %{condl}"));
        self.block(&format!("{condl}"));
        let i = self.reg();
        self.w(&format!("  {i} = load i64, ptr {ireg}"));
        let done = self.reg();
        self.w(&format!("  {done} = icmp sge i64 {i}, {n}"));
        self.w(&format!("  br i1 {done}, label %{endl}, label %{bodyl}"));
        self.block(&format!("{bodyl}"));
        let v = self.reg();
        self.w(&format!("  {v} = add i64 {start}, {i}"));
        let bv = self.reg();
        self.w(&format!("  {bv} = call %NxVal @nx_int(i64 {v})"));
        self.w(&format!("  call void @nx_listpush(ptr {p}, %NxVal {bv})"));
        let inc = self.reg();
        self.w(&format!("  {inc} = add i64 {i}, 1"));
        self.w(&format!("  store i64 {inc}, ptr {ireg}"));
        self.w(&format!("  br label %{condl}"));
        self.block(&format!("{endl}"));
    }

    fn emit_expr(&mut self, expr: &Expr) -> Result<NV, CodegenError> {
        match expr {
            Expr::Int(i, _) => Ok(self.emit_i64(*i)),
            Expr::Float(x, _) => {
                let d = self.reg();
                self.w(&format!("  {d} = fadd double {}, 0.0", fmt_double(*x)));
                Ok(NV::raw(Ty::Float, d))
            }
            Expr::Bool(b, _) => {
                let r = self.reg();
                self.w(&format!("  {r} = add i1 {}, 0", if *b { "true" } else { "false" }));
                Ok(NV::raw(Ty::Bool, r))
            }
            Expr::Str(s, span) => {
                let r = self.emit_str(s, *span)?;
                Ok(NV::boxed_known(r, Ty::Str))
            }
            Expr::NoneLit(_) => {
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_none()"));
                Ok(NV::boxed_known(r, Ty::None))
            }
            Expr::Range { start, end, .. } => {
                // Materialised, unlike a `for i in a..b` header which stays
                // a counted loop. Half-open and ascending, so an empty
                // range produces an empty list rather than underflowing.
                let s = self.emit_expr(start)?;
                let e = self.emit_expr(end)?;
                let a = self.as_i64(&s);
                let b = self.as_i64(&e);
                let cnt = self.reg();
                self.w(&format!("  {cnt} = sub i64 {b}, {a}"));
                let nonneg = self.reg();
                self.w(&format!("  {nonneg} = icmp sgt i64 {cnt}, 0"));
                let n = self.reg();
                self.w(&format!("  {n} = select i1 {nonneg}, i64 {cnt}, i64 0"));
                let l = self.reg();
                self.w(&format!("  {l} = call %NxVal @nx_new_list(i64 {n})"));
                let p = self.alloca("%NxVal");
                self.w(&format!("  store %NxVal {l}, ptr {p}"));
                self.emit_range_fill(&p, &a, &n);
                let out = self.reg();
                self.w(&format!("  {out} = load %NxVal, ptr {p}"));
                Ok(NV::fresh_boxed(out, Ty::List(Box::new(Ty::Int))))
            }
            Expr::Dict(pairs, _) => {
                let n = pairs.len() as i64;
                let d = self.reg();
                self.w(&format!("  {d} = call %NxVal @nx_new_dict(i64 {n})"));
                let p = self.alloca("%NxVal");
                self.w(&format!("  store %NxVal {d}, ptr {p}"));
                for (k, v) in pairs {
                    let kv = self.emit_expr(k)?;
                    let vv = self.emit_expr(v)?;
                    let kb = self.unbox(&kv);
                    let vb = self.store_boxed(&vv);
                    self.w(&format!(
                        "  call void @nx_dictset(ptr {p}, %NxVal {kb}, %NxVal {vb})"
                    ));
                }
                let out = self.reg();
                self.w(&format!("  {out} = load %NxVal, ptr {p}"));
                Ok(NV::fresh_boxed(out, Ty::Dict(Box::new(Ty::Unknown))))
            }
            Expr::Slice { base, from, to, step, span } => {
                let b = self.emit_expr(base)?;
                let bv = self.unbox(&b);
                // An absent bound is the runtime's sentinel for "to the
                // end", which is why the default is a large Int rather than
                // a separate flag.
                let mk = |me: &mut Self, e: &Option<Box<Expr>>, dflt: i64| -> Result<String, CodegenError> {
                    match e {
                        Some(x) => {
                            let v = me.emit_expr(x)?;
                            Ok(me.as_i64(&v))
                        }
                        // An absent `from` is i64::MIN and an absent `to` is i64::MAX. The
                // runtime clamps a negative `from` by adding the length
                // and then flooring it at zero, so MIN lands on 0 for any
                // list length, which is exactly "from the start".
                None => Ok(me.emit_i64(dflt).reg),
                    }
                };
                let f = mk(self, from, i64::MIN)?;
                let t = mk(self, to, i64::MAX)?;
                let s = mk(self, step, 1)?;
                let out = self.reg();
                self.w(&format!(
                    "  {out} = call %NxVal @nx_slice(%NxVal {bv}, i64 {f}, i64 {t}, i64 {s})"
                ));
                let _ = span;
                // Slicing a list of known scalars keeps that element type.
                // Fresh storage, like every other constructor here.
                match &b.ty {
                    Ty::List(t) => Ok(NV::fresh_boxed(out, Ty::List(t.clone()))),
                    Ty::Str => Ok(NV::fresh_boxed(out, Ty::Str)),
                    _ => Ok(NV::fresh_boxed(out, Ty::Unknown)),
                }
            }
            Expr::IfExpr { cond, then_value, else_value, .. } => {
                let c = self.emit_expr(cond)?;
                let b = self.as_i1(&c);
                let tl = self.lab("if_then");
                let el = self.lab("if_else");
                let jn = self.lab("if_join");
                self.w(&format!("  br i1 {b}, label %{tl}, label %{el}"));
                self.block(&format!("{tl}"));
                let tv = self.emit_expr(then_value)?;
                let tb = self.unbox(&tv);
                // An arm can open blocks of its own -- `if c: xs[i-1] else: y`
                // leaves the subtraction's `ovf_done` holding the value -- so
                // the phi below may not name `%if_then`/`%if_else` directly.
                let theld = self.funnel(&tl);
                self.w(&format!("  br label %{jn}"));
                self.block(&format!("{el}"));
                let ev = self.emit_expr(else_value)?;
                let eb = self.unbox(&ev);
                let eheld = self.funnel(&el);
                self.w(&format!("  br label %{jn}"));
                self.block(&format!("{jn}"));
                // Both arms carry the same `%NxVal` type, so a phi over the
                // boxed form is all that is needed to merge them.
                let out = self.reg();
                self.w(&format!("  {out} = phi %NxVal [ {tb}, %{theld} ], [ {eb}, %{eheld} ]"));
                // The phi merges the boxed form, so the result type only has to be
                // precise when both arms agree. Otherwise it stays dynamic,
                // which is correct and merely unspecialised. Fresh only when
                // both arms are: the taken arm's box is then uniquely owned
                // no matter which arm was taken.
                let arms_agree =
                    tv.ty == ev.ty || matches!(tv.ty, Ty::Unknown) || matches!(ev.ty, Ty::Unknown);
                let ty = if arms_agree {
                    if tv.ty == Ty::Unknown {
                        ev.ty.clone()
                    } else {
                        tv.ty.clone()
                    }
                } else {
                    Ty::Unknown
                };
                let mut nv = NV::boxed_known(out, ty);
                nv.fresh = tv.fresh && ev.fresh;
                Ok(nv)
            }
            Expr::Comprehension { element, var, iter, cond, .. } => {
                // Emitted as a real loop rather than a recursive call, so it
                // stays inside the current frame and the loop variable gets
                // an ordinary slot.
                let it = self.emit_expr(iter)?;
                let elem = match &it.ty {
                    Ty::List(t) => (**t).clone(),
                    Ty::Str => Ty::Str,
                    _ => Ty::Unknown,
                };
                let items_ty = match &it.ty {
                    Ty::List(_) => Ty::List(Box::new(Ty::Unknown)),
                    // A comprehension always yields a list, even over a
                    // string (a list of one-character strings).
                    Ty::Str => Ty::List(Box::new(Ty::Str)),
                    other => other.clone(),
                };
                let itb = self.unbox(&it);
                // The accumulator is addressed so nx_listpush can grow it.
                let slot = self.alloca("%NxVal");
                let acc = self.reg();
                self.w(&format!("  {acc} = call %NxVal @nx_new_list(i64 8)"));
                self.w(&format!("  store %NxVal {acc}, ptr {slot}"));
                // The loop bound must count what the body fetches. A
                // string's `b` field is a BYTE length, so reading it
                // directly walks bytes -- an R3 violation the old code
                // had (`[c for c in "str"]` yielded one broken byte per
                // byte). nx_len counts characters, matching the
                // `for c in s` loop, which fetches through nx_index.
                let n = self.reg();
                match &it.ty {
                    Ty::List(_) => {
                        self.w(&format!("  {n} = extractvalue %NxVal {itb}, 2"));
                    }
                    _ => {
                        let lc = self.reg();
                        self.w(&format!("  {lc} = call %NxVal @nx_len(%NxVal {itb})"));
                        self.w(&format!("  {n} = extractvalue %NxVal {lc}, 1"));
                    }
                }
                let ireg = self.alloca("i64");
                self.w(&format!("  store i64 0, ptr {ireg}"));
                let condl = self.lab("comp_cond");
                let bodyl = self.lab("comp_body");
                let endl = self.lab("comp_end");
                self.w(&format!("  br label %{condl}"));
                self.block(&format!("{condl}"));
                let i = self.reg();
                self.w(&format!("  {i} = load i64, ptr {ireg}"));
                let done = self.reg();
                self.w(&format!("  {done} = icmp sge i64 {i}, {n}"));
                self.w(&format!("  br i1 {done}, label %{endl}, label %{bodyl}"));
                self.block(&format!("{bodyl}"));
                // Fetch the current element through the same helpers the
                // `for` loop uses, so both spellings agree: a string
                // yields one-character strings (nx_index), a dynamic
                // iterable dispatches on its tag (nx_each), a list reads
                // its stored value.
                let cur = self.reg();
                match &it.ty {
                    Ty::List(_) => {
                        self.w(&format!("  {cur} = call %NxVal @nx_listget(%NxVal {itb}, i64 {i})"));
                    }
                    Ty::Str => {
                        let iv = self.reg();
                        self.w(&format!("  {iv} = call %NxVal @nx_int(i64 {i})"));
                        self.w(&format!(
                            "  {cur} = call %NxVal @nx_index(%NxVal {itb}, %NxVal {iv})"
                        ));
                    }
                    _ => {
                        self.w(&format!("  {cur} = call %NxVal @nx_each(%NxVal {itb}, i64 {i})"));
                    }
                }
                // The loop variable is scoped to the comprehension, so a
                // same-named outer variable is neither read nor clobbered.
                let vty = ll_scalar(&elem).map(|s| match s {
                    "i64" => Ty::Int,
                    "double" => Ty::Float,
                    _ => Ty::Bool,
                });
                self.new_slot(var, vty);
                self.store_slot(var, &NV::boxed_known(cur, elem.clone()));
                // The filter and the append share one tail block, so the
                // element expression is emitted exactly once. Elements are
                // owned by the result list.
                let emit_push = |me: &mut Self| -> Result<(), CodegenError> {
                    let ev = me.emit_expr(element)?;
                    let eb = me.store_boxed(&ev);
                    me.w(&format!("  call void @nx_listpush(ptr {slot}, %NxVal {eb})"));
                    Ok(())
                };
                match cond {
                    Some(cnd) => {
                        let cv = self.emit_expr(cnd)?;
                        let cb = self.as_i1(&cv);
                        let take = self.lab("comp_take");
                        let drop = self.lab("comp_drop");
                        let adv = self.lab("comp_adv");
                        self.w(&format!("  br i1 {cb}, label %{take}, label %{drop}"));
                        self.block(&format!("{take}"));
                        emit_push(self)?;
                        self.w(&format!("  br label %{adv}"));
                        self.block(&format!("{drop}"));
                        self.w(&format!("  br label %{adv}"));
                        self.block(&format!("{adv}"));
                    }
                    None => emit_push(self)?,
                }
                let inc = self.reg();
                self.w(&format!("  {inc} = add i64 {i}, 1"));
                self.w(&format!("  store i64 {inc}, ptr {ireg}"));
                self.w(&format!("  br label %{condl}"));
                self.block(&format!("{endl}"));
                let res = self.reg();
                self.w(&format!("  {res} = load %NxVal, ptr {slot}"));
                Ok(NV::fresh_boxed(res, items_ty))
            }
            Expr::List(items, _) => {
                let n = items.len() as i64;
                let l = self.reg();
                self.w(&format!("  {l} = call %NxVal @nx_new_list(i64 {n})"));
                let p = self.alloca("%NxVal");
                self.w(&format!("  store %NxVal {l}, ptr {p}"));
                let mut elem = Ty::Unknown;
                for it in items {
                    let v = self.emit_expr(it)?;
                    if elem == Ty::Unknown {
                        elem = v.ty.clone();
                    }
                    // Elements are owned by the list, so a container
                    // element is duplicated on the way in.
                    let b = self.store_boxed(&v);
                    self.w(&format!("  call void @nx_listpush(ptr {p}, %NxVal {b})"));
                }
                let out = self.reg();
                self.w(&format!("  {out} = load %NxVal, ptr {p}"));
                // A list of proven scalars has a known element type, so
                // reading from it can stay unboxed. It is mutable, so it
                // never qualifies for memoization. Fresh storage, owned by
                // whoever binds it.
                Ok(NV::fresh_boxed(out, Ty::List(Box::new(elem))))
            }
            Expr::Var(name, span) => match self.resolve(name) {
                Ok(Binding::Local) => Ok(self.load_slot(name)),
                Ok(Binding::Global(g)) => {
                    let v = self.reg();
                    self.w(&format!("  {v} = load %NxVal, ptr @{g}"));
                    // Globals are boxed, but their static type is known,
                    // so reads of them can still feed unboxed arithmetic.
                    Ok(NV::boxed_known(v, self.global_ty(name)))
                }
                Ok(Binding::Module(m)) => Err(err(
                    *span,
                    format!("module '{m}' is compile-time only in value position"),
                )),
                Ok(Binding::ModuleFn(m, f)) => Err(err(
                    *span,
                    format!("function '{m}.{f}' cannot be used as a value; call it"),
                )),
                Err((kind, _)) if kind == "fn" => Err(err(
                    *span,
                    format!("function '{name}' cannot be used as a value; call it"),
                )),
                Err(_) => Err(err(*span, format!("undefined variable '{name}'"))),
            },
            Expr::Attr { base, attr, span } => {
                if let Expr::Var(m, _) = base.as_ref() {
                    if let Some(module) = self.modrefs.get(m).cloned() {
                        if self.is_module_fn(&module, attr) {
                            return Err(err(
                                *span,
                                format!("function '{module}.{attr}' cannot be used as a value; call it"),
                            ));
                        }
                        // A type used as `m.T` in value position is not
                        // meaningful: types construct, they are not values.
                        if self.layouts.contains_key(&(module.clone(), attr.clone())) {
                            return Err(err(
                                *span,
                                format!("type '{module}.{attr}' cannot be used as a value; construct it"),
                            ));
                        }
                        let g = mangle_global(&module, attr);
                        let v = self.reg();
                        self.w(&format!("  {v} = load %NxVal, ptr @{g}"));
                        let ty = self
                            .types
                            .get(&(module.clone(), "<top>".to_string()))
                            .and_then(|f| f.locals.get(attr))
                            .cloned()
                            .unwrap_or(Ty::Unknown);
                        return Ok(NV::boxed_known(v, ty));
                    }
                }
                // A record field. When the static type is known the offset
                // is a constant; when it is not, the name is resolved
                // against the value's own descriptor at runtime.
                let b = self.emit_expr(base)?;
                let bv = self.unbox(&b);
                match &b.ty {
                    Ty::Record(t) => {
                        let (decl_module, _, fields) = self
                            .resolve_type(t)
                            .ok_or(err(*span, format!("unknown type '{t}'")))?;
                        let _ = decl_module;
                        let i = Self::field_index(&fields, t, attr, *span)?;
                        let r = self.reg();
                        self.w(&format!("  {r} = call %NxVal @nx_rec_get(%NxVal {bv}, i64 {i})"));
                        // The field's static type is not tracked past the
                        // declaration (it may be `Any`), so the result is
                        // dynamic. Specialising it is the unboxed-fields
                        // pass, which comes with the ownership work.
                        Ok(NV::dyn_boxed(r))
                    }
                    Ty::Unknown => {
                        // The field-name bytes come from an ordinary
                        // string constant; its payload is the byte
                        // pointer `nx_rec_getn` compares, and the length
                        // is known at compile time.
                        let s = self.emit_str(attr, *span)?;
                        let nv = NV::boxed_known(s, Ty::Str);
                        let pbits = self.payload(&nv);
                        let p = self.reg();
                        self.w(&format!("  {p} = inttoptr i64 {pbits} to ptr"));
                        let r = self.reg();
                        self.w(&format!(
                            "  {r} = call %NxVal @nx_rec_getn(%NxVal {bv}, ptr {p}, i64 {})",
                            attr.len()
                        ));
                        Ok(NV::dyn_boxed(r))
                    }
                    _ => Err(err(*span, "only modules and types have attributes".to_string())),
                }
            }
            Expr::Index { base, index, .. } => {
                let b = self.emit_expr(base)?;
                let ix = self.emit_expr(index)?;
                let elem = match &b.ty {
                    Ty::List(t) => (**t).clone(),
                    Ty::Str => Ty::Str,
                    _ => Ty::Unknown,
                };
                let bv = self.unbox(&b);
                let iv = self.unbox(&ix);
                // nx_index returns the element itself, so pulling the
                // payload straight out of the result costs nothing and
                // keeps its bounds check.
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_index(%NxVal {bv}, %NxVal {iv})"));
                if ll_scalar(&elem).is_some() {
                    if let Some(raw) = self.as_raw(&NV::boxed_known(r.clone(), elem.clone())) {
                        return Ok(raw);
                    }
                }
                Ok(NV::boxed_known(r, elem))
            }
            Expr::Unary { op, expr, .. } => {
                let v = self.emit_expr(expr)?;
                // Only a value already in a raw register takes the direct
                // path; a box of a known scalar still goes through the
                // runtime helper, which also re-checks the tag.
                if matches!(v.raw, Some(Ty::Int)) && matches!(op, UnaryOp::Neg) {
                    // Negating MIN overflows, so this goes through the
                    // checked intrinsic like every other Int subtraction.
                    let r = self.reg();
                    self.emit_i64_checked("llvm.ssub.with.overflow.i64", "0", &v.reg, &r);
                    return Ok(NV::raw(Ty::Int, r));
                }
                if matches!(v.raw, Some(Ty::Float)) && matches!(op, UnaryOp::Neg) {
                    let r = self.reg();
                    self.w(&format!("  {r} = fneg double {}", v.reg));
                    return Ok(NV::raw(Ty::Float, r));
                }
                if matches!(v.raw, Some(Ty::Bool)) && matches!(op, UnaryOp::Not) {
                    let r = self.reg();
                    self.w(&format!("  {r} = xor i1 {}, true", v.reg));
                    return Ok(NV::raw(Ty::Bool, r));
                }
                if matches!(v.raw, Some(Ty::Int)) && matches!(op, UnaryOp::BitNot) {
                    let r = self.reg();
                    self.w(&format!("  {r} = xor i64 {}, -1", v.reg));
                    return Ok(NV::raw(Ty::Int, r));
                }
                if v.raw.is_some() && matches!(op, UnaryOp::Pos) {
                    return Ok(v);
                }
                let b = self.unbox(&v);
                let r = self.reg();
                match op {
                    UnaryOp::Neg => self.w(&format!("  {r} = call %NxVal @nx_neg(%NxVal {b})")),
                    UnaryOp::Not => self.w(&format!("  {r} = call %NxVal @nx_not(%NxVal {b})")),
                    UnaryOp::BitNot => {
                        self.w(&format!("  {r} = call %NxVal @nx_bitnot(%NxVal {b})"))
                    }
                    // Unary plus changes nothing, so it returns the operand.
                    UnaryOp::Pos => return Ok(v),
                }
                let ty = match op {
                    UnaryOp::BitNot => Ty::Int,
                    _ => Ty::Unknown,
                };
                // Every helper above allocates its result box.
                Ok(NV::fresh_boxed(r, ty))
            }
            Expr::Binary { left, op, right, span } => {
                if matches!(op, BinOp::And | BinOp::Or) {
                    return self.emit_logic(left, *op, right);
                }
                let l = self.emit_expr(left)?;
                let r = self.emit_expr(right)?;
                let _ = span;
                self.binop_dyn(&l, *op, &r)
            }
            Expr::Call { callee, args, span } => self.emit_call(callee, args, *span),
        }
    }

    /// Static type of a module-level variable (always stored boxed).
    fn global_ty(&self, name: &str) -> Ty {
        self.types
            .get(&(self.cur_module.clone(), "<top>".to_string()))
            .and_then(|f| f.locals.get(name))
            .cloned()
            .unwrap_or(Ty::Unknown)
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
    fn emit_i64_checked(&mut self, intrin: &str, a: &str, b: &str, out: &str) {
        let pair = self.reg();
        let flag = self.reg();
        let bad = self.lab("ovf");
        let ok = self.lab("ovf_ok");
        let done = self.lab("ovf_done");
        self.w(&format!("  {pair} = call {{ i64, i1 }} @{intrin}(i64 {a}, i64 {b})"));
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
    fn emit_arith(&mut self, rule: BinRule, op: BinOp, a: &str, b: &str, ty: Ty) -> NV {
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
            ArithPlan::FloatCall(helper) => {
                self.w(&format!("  {out} = call double @{helper}(double {a}, double {b})"))
            }
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
    fn emit_scalar_binop(&mut self, l: &NV, op: BinOp, r: &NV) -> Option<NV> {
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
    fn emit_named_binop(&mut self, l: &NV, op: BinOp, r: &NV) -> Option<NV> {
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

    fn emit_str(&mut self, s: &str, _span: Span) -> Result<String, CodegenError> {
        let bytes = s.as_bytes();
        let n = bytes.len();
        let id = self.strc;
        self.strc += 1;
        let mut esc = String::new();
        for b in bytes {
            match b {
                b'"' => esc.push_str("\\22"),
                b'\\' => esc.push_str("\\5C"),
                b'\n' => esc.push_str("\\0A"),
                32..=126 => esc.push(*b as char),
                _ => esc.push_str(&format!("\\{b:02X}")),
            }
        }
        if n == 0 {
            self.top.push_str(&format!("@.nxstr.{id} = private constant [1 x i8] zeroinitializer\n"));
        } else {
            self.top.push_str(&format!("@.nxstr.{id} = private constant [{n} x i8] c\"{esc}\"\n"));
        }
        let rawlen = if n == 0 { 1 } else { n };
        let p = self.reg();
        let r = self.reg();
        self.w(&format!("  {p} = getelementptr [{rawlen} x i8], ptr @.nxstr.{id}, i64 0, i64 0"));
        self.w(&format!("  {r} = call %NxVal @nx_str(ptr {p}, i64 {n})"));
        Ok(r)
    }

    fn emit_logic(&mut self, left: &Expr, op: BinOp, right: &Expr) -> Result<NV, CodegenError> {
        // Diamond with a single deciding branch:
        //   br i1 <decide>, label %short, label %rhs     (and)
        //   br i1 <decide>, label %rhs, label %short     (or)
        // short: br merge      (result = decided value)
        // rhs:   <right> -> rb; br merge
        // merge: phi [decided, short], [rb, rhs]
        let l = self.emit_expr(left)?;
        let lb = self.as_i1(&l);
        let rhs = self.lab("rhs");
        let short = self.lab("short");
        let merge = self.lab("merge");
        match op {
            BinOp::And => self.w(&format!("  br i1 {lb}, label %{rhs}, label %{short}")),
            _ => self.w(&format!("  br i1 {lb}, label %{short}, label %{rhs}")),
        }
        let decided = match op {
            BinOp::And => "false",
            _ => "true",
        };
        self.block(&format!("{short}"));
        self.w(&format!("  br label %{merge}"));
        self.block(&format!("{rhs}"));
        let rv = self.emit_expr(right)?;
        let rb = self.as_i1(&rv);
        // The right operand can open blocks of its own -- `i < n and xs[i-1] > xs[i]`
        // puts two checked subtractions and their diamonds in here -- so the
        // value may no longer live in `%rhs`. The phi below names the block
        // it does live in, so funnel first.
        let held = self.funnel(&rhs);
        // RHS is an expression: it cannot terminate (no return/break inside).
        self.w(&format!("  br label %{merge}"));
        self.block(&format!("{merge}"));
        // The result is always a proven Bool: both operands had to be.
        let phi = self.reg();
        self.w(&format!("  {phi} = phi i1 [{decided}, %{short}], [{rb}, %{held}]"));
        Ok(NV::raw(Ty::Bool, phi))
    }

    fn emit_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        span: Span,
    ) -> Result<NV, CodegenError> {
        // A declared type is constructed by name, sharing the call spelling
            // with a function. Checked before function resolution: a type
            // and a function cannot share a name, so reaching here with a
            // type name means construction, not a call.
            if let Expr::Var(name, _) = callee {
                if let Some((decl_module, canon, fields)) = self.resolve_type(name) {
                    if args.len() != fields.len() {
                        return Err(err(
                            span,
                            format!(
                                "type '{name}' takes {} field{}, got {}",
                                fields.len(),
                                if fields.len() == 1 { "" } else { "s" },
                                args.len()
                            ),
                        ));
                    }
                    return self.emit_construct(&decl_module, &canon, name, args);
                }
            }
        if let Expr::Var(name, _) = callee {
            if nx_types::builtin_arity(name).is_some() {
                return self.emit_builtin(name, args, span);
            }
            let cur = self.cur_module.clone();
            if self.arity.contains_key(&(cur.clone(), name.clone())) {
                return self.emit_direct(&cur, name, args);
            }
            if let Some((m, f)) = self.falias.get(name).cloned() {
                return self.emit_direct(&m, &f, args);
            }
            return Err(err(span, format!("unknown function '{name}'")));
        }
        if let Expr::Attr { base, attr, .. } = callee {
            if let Expr::Var(m, _) = base.as_ref() {
                if let Some(module) = self.modrefs.get(m).cloned() {
                    // A type exported by the module constructs the same
                    // way a local one does.
                    if let Some(fields) = self.layouts.get(&(module.clone(), attr.clone())).cloned() {
                        if args.len() != fields.len() {
                            return Err(err(
                                span,
                                format!(
                                    "type '{attr}' takes {} field{}, got {}",
                                    fields.len(),
                                    if fields.len() == 1 { "" } else { "s" },
                                    args.len()
                                ),
                            ));
                        }
                        return self.emit_construct(&module, attr, attr, args);
                    }
                    if self.is_module_fn(&module, attr) {
                        return self.emit_direct(&module, attr, args);
                    }
                    return Err(err(span, format!("'{attr}' is not a function of '{module}'")));
                }
                // `T.m(...)` where T names a visible type: an associated
                // function. The base is not a value, so there is no sugar
                // fallback -- anything else is meaningless.
                if let Some((decl, canon, _)) = self.resolve_type(m) {
                    if let Some(sig) = self.methods.get(&(decl.clone(), canon.clone(), attr.clone())).cloned() {
                        if sig.receiver != nx_ast::ReceiverKind::None {
                            return Err(err(
                                span,
                                format!("method '{attr}' needs a receiver; call it on a '{canon}' value"),
                            ));
                        }
                        return self.emit_method_call(&decl, &canon, attr, &sig, None, args, span);
                    }
                    return Err(err(span, format!("type '{canon}' has no associated function '{attr}'")));
                }
            }
            // A record value dispatches to its type's method table. The
            // base is evaluated once; its static type decides method
            // versus sugar, so an unresolved base always takes sugar.
            let recv = self.emit_expr(base)?;
            // Dispatch types come from the checker, not from the unboxing
            // decision: with unboxing off every `NV` is Unknown, and a
            // method call that stopped resolving there would make the
            // opt-out a different language. A bare variable can be asked
            // directly, which is what keeps `self.area()` working.
            let bt = match &recv.ty {
                Ty::Unknown => match base.as_ref() {
                    // A field read is always dynamically typed (unboxed
                    // fields arrive with ownership), so a receiver reached
                    // through one re-derives its static type from the
                    // declaration instead. Anything still unknown stays
                    // unknown and takes sugar or the error below.
                    Expr::Var(..) | Expr::Attr { .. } => self.static_ty_of(base),
                    _ => Ty::Unknown,
                },
                t => t.clone(),
            };
            if let Ty::Record(t) = bt {
                if let Some((decl, canon, _)) = self.resolve_type(&t) {
                    if let Some(sig) = self.methods.get(&(decl.clone(), canon.clone(), attr.clone())).cloned() {
                        return self.emit_method_call(&decl, &canon, attr, &sig, Some((base, &recv)), args, span);
                    }
                    // Records without the method fall through to sugar, so
                    // a builtin that accepts records keeps working -- the
                    // builtin's own check names any mismatch.
                    if nx_types::builtin_arity(attr).is_none() {
                        return Err(err(span, format!("type '{canon}' has no method '{attr}'")));
                    }
                } else {
                    return Err(err(span, format!("unknown type '{t}'")));
                }
            }
            // Builtin sugar: `xs.push(1)` for `push(xs, 1)`. The base
            // expression is prepended and routed through the identical
            // builtin path as a direct call.
            if nx_types::builtin_arity(attr).is_some() {
                let mut combined: Vec<Expr> = Vec::with_capacity(args.len() + 1);
                combined.push((**base).clone());
                combined.extend(args.iter().cloned());
                return self.emit_builtin(attr, &combined, span);
            }
            return Err(err(span, "only modules, types and builtins support attribute calls".to_string()));
        }
        Err(err(span, "only direct calls are supported".to_string()))
    }
    /// Emit an ambient builtin by name. Direct calls (`push(xs, 1)`) and
    /// sugar calls (`xs.push(1)`) share this path: sugar prepends the
    /// base expression and arrives here with identical arguments, so the
    /// two spellings cannot drift apart.
    fn emit_builtin(&mut self, name: &str, args: &[Expr], span: Span) -> Result<NV, CodegenError> {
        match name {
            "len" => {
                if args.len() != 1 {
                    return Err(err(span, "len() expects 1 argument".to_string()));
                }
                let a = self.emit_expr(&args[0])?;
                let ab = self.unbox(&a);
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_len(%NxVal {ab})"));
                // len is always an Int, so the payload can go straight on.
                let n = self.reg();
                self.w(&format!("  {n} = extractvalue %NxVal {r}, 1"));
                Ok(NV::raw(Ty::Int, n))
            }
            "push" => {
                if args.len() != 2 {
                    return Err(err(span, "push() expects 2 arguments".to_string()));
                }
                // The target must be a variable: pushing into a temporary
                // would drop the result, so the checker rejects it and
                // this arm never sees one.
                if !matches!(&args[0], Expr::Var(..)) {
                    return Err(err(
                        span,
                        "push() first argument must be a list variable".to_string(),
                    ));
                }
                let v = self.emit_expr(&args[1])?;
                let vb = self.store_boxed(&v);
                // The pushed value is owned by the list from here on.
                match &args[0] {
                    Expr::Var(n, _) if !matches!(self.ty_of_any(n), Ty::Unknown) => {
                        let ptr = self.ptr_of(n).ok_or(err(
                            span,
                            "push() first argument must be a list variable".to_string(),
                        ))?;
                        self.w(&format!("  call void @nx_listpush(ptr {ptr}, %NxVal {vb})"));
                    }
                    Expr::Var(_, _) => {
                        // Unresolved base: only a list can be pushed to, and
                        // the tag check says so at runtime rather than
                        // corrupting a dict's entry array.
                        let lv = self.emit_expr(&args[0])?;
                        let lb = self.unbox(&lv);
                        let p = self.alloca("%NxVal");
                        self.w(&format!("  store %NxVal {lb}, ptr {p}"));
                        self.w(&format!("  call void @nx_pushdyn(ptr {p}, %NxVal {vb})"));
                        let updated = self.reg();
                        self.w(&format!("  {updated} = load %NxVal, ptr {p}"));
                        self.write_back(&args[0], &NV::boxed_known(updated, Ty::Unknown))?;
                    }
                    _ => {
                        return Err(err(
                            span,
                            "push() first argument must be a list variable".to_string(),
                        ))
                    }
                }
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_none()"));
                Ok(NV::boxed_known(r, Ty::None))
            }
            "input" => {
                // `input()` reads a line; `input(prompt)` prints the prompt
                // first. The checker caps the arity, so anything else here
                // is an internal error, not a user program.
                if args.len() > 1 {
                    return Err(err(
                        span,
                        format!("input() expects at most 1 argument, got {}", args.len()),
                    ));
                }
                let (pv, has) = match args.first() {
                    Some(p) => {
                        let v = self.emit_expr(p)?;
                        (self.unbox(&v), "true")
                    }
                    None => {
                        let n = self.reg();
                        self.w(&format!("  {n} = call %NxVal @nx_none()"));
                        (n, "false")
                    }
                };
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_input(%NxVal {pv}, i1 {has})"));
                // The answer is a freshly allocated buffer, so storing it
                // needs no clone.
                Ok(NV::fresh_boxed(r, Ty::Str))
            }
            "int" => {
                // `int(x)` converts one value to Int. Parsing lives in the
                // runtime helper, once -- the checker above owns which
                // types arrive, so anything else here is unreachable
                // through checked code.
                if args.len() != 1 {
                    return Err(err(span, format!("int() expects 1 argument, got {}", args.len())));
                }
                let a = self.emit_expr(&args[0])?;
                let ab = self.store_boxed(&a);
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_to_int(%NxVal {ab})"));
                Ok(NV::fresh_boxed(r, Ty::Int))
            }
            "float" => {
                // `float(x)` converts one value to Float, same shape.
                if args.len() != 1 {
                    return Err(err(span, format!("float() expects 1 argument, got {}", args.len())));
                }
                let a = self.emit_expr(&args[0])?;
                let ab = self.store_boxed(&a);
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_to_float(%NxVal {ab})"));
                Ok(NV::fresh_boxed(r, Ty::Float))
            }
            _ => Err(err(span, format!("unknown builtin '{name}'"))),
        }
    }

    /// Emit a method or associated-function call. The receiver value (for
    /// methods) is prepended to the argument boxes, so the callee sees
    /// `self` positionally like any other parameter -- which under value
    /// semantics gives the method its own copy to mutate.
    ///
    /// A `mut self` result is written back into the receiver when the
    /// receiver has storage; a temporary base has nowhere to write, so the
    /// call evaluates to its result alone.
    fn emit_method_call(
        &mut self,
        decl_module: &str,
        canon: &str,
        method: &str,
        sig: &MethodSig,
        receiver: Option<(&Expr, &NV)>,
        args: &[Expr],
        span: Span,
    ) -> Result<NV, CodegenError> {
        let fname = mangle_method(decl_module, canon, method);
        // The receiver is already evaluated -- the caller emitted it once to
        // learn its type, and emitting it again here would run a `mut self`
        // chain's write-back twice.
        let mut vals: Vec<NV> = Vec::with_capacity(args.len() + 1);
        if let Some((_, rv)) = receiver {
            vals.push(rv.clone());
        }
        for a in args {
            vals.push(self.emit_expr(a)?);
        }
        // The return type comes from inference when available; a `mut self`
        // method returns the record by the checker's rule, which is what
        // makes the write-back type-correct without an annotation.
        let ret = if sig.receiver == nx_ast::ReceiverKind::Mut {
            Ty::Record(canon.to_string())
        } else {
            self.types
                .get(&(
                    decl_module.to_string(),
                    nx_ast::shape::method_key(canon, method),
                ))
                .map(|f| f.ret.clone())
                .unwrap_or(Ty::Unknown)
        };
        let out = self.emit_call_boxed(&fname, &vals, ret)?;
        // Write-back for `mut self`, when the receiver has storage. A
        // temporary base has nowhere to write, so the call evaluates to its
        // result alone -- which is what makes `q.moved(1, 1).moved(2, 2)`
        // read as one expression.
        if sig.receiver == nx_ast::ReceiverKind::Mut {
            if let Some((base, _)) = receiver {
                if let Some(t) = Self::target_of_expr(base) {
                    self.store_target(&t, &out, span)?;
                }
            }
        }
        Ok(out)
    }

    /// Reinterpret a call receiver as an assignment target, for `mut self`
    /// write-back. Only shapes that have
    /// storage qualify.
    fn target_of_expr(e: &Expr) -> Option<nx_ast::Target> {
        match e {
            Expr::Var(name, _) => Some(nx_ast::Target::Name(name.clone())),
            Expr::Index { base, index, .. } => Some(nx_ast::Target::Index {
                base: base.clone(),
                index: index.clone(),
            }),
            Expr::Attr { base, attr, .. } => Some(nx_ast::Target::Attr {
                base: base.clone(),
                field: attr.clone(),
            }),
            _ => None,
        }
    }

    /// `Type(v0, v1, ...)` -- allocate the record, then fill each field in
    /// declaration order. The record value is in a register throughout;
    /// `nx_rec_set` writes through the header, so no alloca is needed to
    /// hold it between the fills. The static type carries the canonical
    /// name, so a value built through an alias compares and resolves
    /// exactly like one built through the original name.
    fn emit_construct(
        &mut self,
        decl_module: &str,
        canon: &str,
        _written: &str,
        args: &[Expr],
    ) -> Result<NV, CodegenError> {
        let n = args.len() as i64;
        let desc = self.desc_of(decl_module, canon);
        let r = self.reg();
        self.w(&format!("  {r} = call %NxVal @nx_new_record(i64 {n}, ptr {desc})"));
        for (i, a) in args.iter().enumerate() {
            let v = self.emit_expr(a)?;
            let vb = self.store_boxed(&v);
            self.w(&format!("  call void @nx_rec_set(%NxVal {r}, i64 {i}, %NxVal {vb})"));
        }
        Ok(NV::fresh_boxed(r, Ty::Record(canon.to_string())))
    }

    /// Declared return type of `module`.`name`, used to keep a call
    /// result unboxed when it feeds straight back into arithmetic.
    fn ret_ty(&self, module: &str, name: &str) -> Ty {
        self.types
            .get(&(module.to_string(), name.to_string()))
            .map(|f| f.ret.clone())
            .unwrap_or(Ty::Unknown)
    }

    fn emit_direct(
        &mut self,
        module: &str,
        name: &str,
        args: &[Expr],
    ) -> Result<NV, CodegenError> {
        let fname = mangle_fn(module, name);
        let ty = self.ret_ty(module, name);
        self.emit_direct_named(&fname, args, ty)
    }

    /// Call an already-mangled function symbol with boxed ABI arguments.
    /// Each argument expression is evaluated exactly once.
    fn emit_direct_named(
        &mut self,
        fname: &str,
        args: &[Expr],
        ty: Ty,
    ) -> Result<NV, CodegenError> {
        let mut vals: Vec<NV> = Vec::with_capacity(args.len());
        for a in args {
            vals.push(self.emit_expr(a)?);
        }
        self.emit_call_boxed(fname, &vals, ty)
    }

    /// Call an already-mangled symbol with values that are *already*
    /// evaluated. Method calls come through here rather than through
    /// `emit_direct_named` because the receiver has to be emitted once: it
    /// is evaluated to learn its static type, and a `mut self` receiver is
    /// an expression with an effect of its own. Re-emitting it to build the
    /// argument list would run that effect twice.
    fn emit_call_boxed(
        &mut self,
        fname: &str,
        vals: &[NV],
        ty: Ty,
    ) -> Result<NV, CodegenError> {
        let n = vals.len();
        let arr = self.alloca(&format!("[{n} x %NxVal]"));
        for (i, v) in vals.iter().enumerate() {
            // The ABI is boxed: every argument re-boxes here.
            let vb = self.unbox(v);
            let ep = self.reg();
            self.w(&format!("  {ep} = getelementptr [{n} x %NxVal], ptr {arr}, i64 0, i64 {i}"));
            self.w(&format!("  store %NxVal {vb}, ptr {ep}"));
        }
        let p0 = self.reg();
        if n == 0 {
            self.w(&format!("  {p0} = inttoptr i64 0 to ptr"));
        } else {
            self.w(&format!("  {p0} = getelementptr [{n} x %NxVal], ptr {arr}, i64 0, i64 0"));
        }
        let r = self.reg();
        self.w(&format!("  {r} = call %NxVal @{fname}(ptr {p0}, i64 {n})"));
        if let Some(ll) = ll_scalar(&ty) {
            let p1 = self.reg();
            self.w(&format!("  {p1} = extractvalue %NxVal {r}, 1"));
            return Ok(match ll {
                "double" => {
                    let d = self.reg();
                    self.w(&format!("  {d} = bitcast i64 {p1} to double"));
                    NV::raw(ty, d)
                }
                "i1" => {
                    let c = self.reg();
                    self.w(&format!("  {c} = trunc i64 {p1} to i1"));
                    NV::raw(ty, c)
                }
                _ => NV::raw(ty, p1),
            });
        }
        Ok(NV::boxed_known(r, ty))
    }

}
