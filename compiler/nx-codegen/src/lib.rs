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
//! - No closures over outer function locals (matches the interpreter).

use std::collections::{HashMap, HashSet};
use nx_ast::{BinOp, Expr, Program, Span, Stmt, UnaryOp};
use nx_types::Ty;

const PRELUDE: &str = include_str!("runtime.ll");

/// OS thread bindings. Only the two shims differ between platforms; the
/// pool itself is shared, so it is tested identically everywhere.
#[cfg(windows)]
const THREADS: &str = include_str!("runtime_threads_win.ll");
#[cfg(not(windows))]
const THREADS: &str = include_str!("runtime_threads_unix.ll");

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
}

impl NV {
    /// A bare scalar already sitting in a register.
    fn raw(t: Ty, reg: String) -> NV {
        NV { reg, raw: Some(t.clone()), ty: t, const_i: None }
    }
    /// A box whose dynamic type the backend does not know.
    fn dyn_boxed(reg: String) -> NV {
        NV { reg, raw: None, ty: Ty::Unknown, const_i: None }
    }
    /// A box whose static type is known: usable unboxed where the caller
    /// needs the payload, but still physically a `%NxVal`.
    fn boxed_known(reg: String, ty: Ty) -> NV {
        NV { reg, raw: None, ty, const_i: None }
    }
    /// A bare Int whose value is known at compile time.
    fn raw_const(t: Ty, reg: String, value: i64) -> NV {
        NV { reg, raw: Some(t.clone()), ty: t, const_i: Some(value) }
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
    pub line: usize,
    pub col: usize,
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

fn mangle_init(module: &str) -> String {
    format!("nx__init_{module}")
}

fn mangle_done(module: &str) -> String {
    format!("nx__done_{module}")
}

#[cfg(test)]
mod tests {
    use super::{compile_entry, compile_opts, mangle_fn};

    /// Structural invariant: `define` may only appear at brace depth 0.
    /// (Catches outlined functions emitted mid-body.)
    fn assert_top_level_defines(ir: &str) {
        let mut depth = 0i32;
        for line in ir.lines() {
            let t = line.trim();
            if t.starts_with("define ") {
                assert_eq!(depth, 0, "define inside function body: {t}");
            }
            depth += line.chars().filter(|&c| c == '{').count() as i32;
            depth -= line.chars().filter(|&c| c == '}').count() as i32;
        }
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

    #[test]
    fn parallel_outlines_at_top_level() {
        let ir = compile_entry(
            "a = 0\nb = 0\nparallel:\n    a = 1\n    b = 2\nprint(a, b)\n",
            std::path::Path::new("."),
        )
        .unwrap();
        assert_top_level_defines(&ir);
    }

    /// A real batch must actually start workers, not fall back to running
    /// the tasks inline. Windows used to lower sequentially here, which
    /// made `parallel:` a no-op on the platform most users are on.
    #[test]
    fn parallel_batch_starts_a_pool() {
        let ir = compile_entry(
            "a = 0\nb = 0\nc = 0\nparallel:\n    a = 1\n    b = 2\n    c = 3\nprint(a, b, c)\n",
            std::path::Path::new("."),
        )
        .unwrap();
        // The shim wraps the OS call, so the pool is what codegen targets.
        let starts = ir.matches("call ptr @nx_thread_start").count();
        let joins = ir.matches("call void @nx_thread_join").count();
        assert_eq!(starts, 3, "one worker per task expected in:\n{ir}");
        assert_eq!(joins, 3, "every worker must be joined:\n{ir}");
        assert_top_level_defines(&ir);
    }

    /// The calling thread works too, which both saves a thread and
    /// guarantees the batch drains even if a spawn were to fail.
    #[test]
    fn calling_thread_joins_the_pool() {
        let ir = compile_entry(
            "a = 0\nb = 0\nparallel:\n    a = 1\n    b = 2\nprint(a, b)\n",
            std::path::Path::new("."),
        )
        .unwrap();
        assert_eq!(ir.matches("call ptr @nx_pool_worker(ptr").count(), 1);
    }

    /// Every worker must be started before any join, and the caller must
    /// have finished its own share before joining, or the batch would
    /// partly run inline and lose the overlap.
    #[test]
    fn parallel_starts_all_before_joining_any() {
        let ir = compile_entry(
            "a = 0\nb = 0\nparallel:\n    a = 1\n    b = 2\nprint(a, b)\n",
            std::path::Path::new("."),
        )
        .unwrap();
        let last_start = ir.rfind("call ptr @nx_thread_start").expect("a start call");
        let first_join = ir.find("call void @nx_thread_join").expect("a join call");
        let self_work = ir.find("call ptr @nx_pool_worker(ptr").expect("self work");
        assert!(last_start < self_work, "start every worker before working");
        assert!(self_work < first_join, "finish your own share before joining");
    }

    /// The pool hands out each task index exactly once, so no task runs
    /// twice and none is skipped. The count is stored, not recomputed,
    /// so it has to match the number of workers.
    #[test]
    fn pool_publishes_task_count() {
        let ir = compile_entry(
            "a = 0\nb = 0\nparallel:\n    a = 1\n    b = 2\nprint(a, b)\n",
            std::path::Path::new("."),
        )
        .unwrap();
        // The count lives in field 1. Anchor on the caller's own pool
        // alloca so the prelude's unrelated %NxPool uses cannot match.
        let anchor = ir.find("= alloca %NxPool").expect("pool alloca");
        let after = &ir[anchor..];
        let field1 = after.find("i32 1").expect("pool count field") + "i32 1".len();
        let rest = &after[field1..];
        let store = rest.find("store i64").expect("pool count store") + "store i64".len();
        let line = &rest[store..];
        let line = &line[..line.find('\n').unwrap_or(0)];
        assert!(line.trim_start().starts_with('2'), "two tasks queued: {line}");
    }

    /// Conflicting tasks must serialize rather than race, so a batch of
    /// one is emitted inline with no pool at all. Making both tasks write
    /// the same name is what forces the conflict.
    #[test]
    fn conflicting_tasks_stay_inline() {
        let ir = compile_entry(
            "a = 0\nparallel:\n    a = 1\n    a = 2\nprint(a)\n",
            std::path::Path::new("."),
        )
        .unwrap();
        assert_eq!(
            ir.matches("call ptr @nx_thread_start").count(),
            0,
            "conflicting tasks serialize, so no pool:\n{ir}"
        );
        assert_top_level_defines(&ir);
    }

    /// Body of one emitted function, so a test can assert on its code
    /// without matching the prelude.
    fn body_of(ir: &str, mangled: &str) -> String {
        let key = format!("define %NxVal @{mangled}(");
        let start = ir
            .find(&key)
            .unwrap_or_else(|| panic!("no function {mangled} in output"));
        let rest = &ir[start..];
        let end = rest[1..].find("\ndefine ").map(|i| i + 1).unwrap_or(rest.len());
        rest[..end].to_string()
    }

    #[test]
    fn int_param_stays_unboxed() {
        let ir = compile_entry(
            "fn f(n):\n    return n * 3 + 1\nprint(f(2))\n",
            std::path::Path::new("."),
        )
        .unwrap();
        let b = body_of(&ir, &mangle_fn("__main__", "f"));
        assert!(b.contains("mul i64"), "Int arithmetic must be a raw mul:\n{b}");
        assert!(b.contains("add i64"), "Int arithmetic must be a raw add:\n{b}");
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
        assert!(b.contains("mul i64"), "loop body should be raw arithmetic:\n{b}");
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
        // The prelude defines nx_free_val with one self-recursive call.
        ir.matches("call void @nx_free_val").count() - 1
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
    let file = format!("{module}.nx");
    let mut dirs = vec![base.to_path_buf()];
    if let Ok(p) = std::env::var("NX_PATH") {
        dirs.extend(std::env::split_paths(&p));
    }
    dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file())
}

pub fn compile_entry(source: &str, base: &std::path::Path) -> Result<String, CodegenError> {
    compile_opts(source, base, std::env::var("NX_NOUNBOX").is_err())
}

/// Compile with an explicit unboxing switch. `compile_entry` reads
/// NX_NOUNBOX; tests use this directly so they do not race on the
/// process environment.
fn compile_opts(
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
            match nx_types::infer_program(prog, &module_dir(&loader.base, module)) {
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
        let file = format!("{name}.nx");
        let mut dirs = vec![self.base.clone()];
        if let Ok(p) = std::env::var("NX_PATH") {
            dirs.extend(std::env::split_paths(&p));
        }
        dirs.into_iter().map(|d| d.join(&file)).find(|p| p.is_file())
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
            let file = format!("{dep}.nx");
            let mut dirs = vec![dir.clone(), base.to_path_buf()];
            if let Ok(p) = std::env::var("NX_PATH") {
                dirs.extend(std::env::split_paths(&p));
            }
            if let Some(p) = dirs.iter().map(|d| d.join(&file)).find(|p| p.is_file()) {
                out.push(p.clone());
                queue.push(p);
            }
        }
    }
    Ok(out)
}

fn collect_imports(prog: &Program, out: &mut Vec<String>) {    for s in &prog.stmts {
        let m = match s {
            Stmt::Import { module, .. } => Some(module),
            Stmt::FromImport { module, .. } => Some(module),
            _ => None,
        };
        if let Some(m) = m {
            if !out.contains(m) {
                out.push(m.clone());
            }
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

/// Block termination state (mirrors interpreter Flow, minus values).
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
}

/// Where a function's allocas have to be spliced back in: the byte offset
/// just past its `entry:` label, and which buffer it is being written to.
struct AllocFrame {
    to_top: bool,
    at: usize,
    items: Vec<(String, String)>,
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
            to_top: false,
            in_init: false,
            term: None,
            loops: Vec::new(),
            alloc_frames: Vec::new(),
        }
    }

    fn emit_prelude(&mut self) {
        self.pre.push_str(PRELUDE);
        self.pre.push('\n');
        self.pre.push_str(THREADS);
        self.pre.push('\n');
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
                let val = self.unbox(v);
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
        // First pass: module-level variable globals (functions need no globals).
        let mut gvars: Vec<String> = Vec::new();
        for s in &prog.stmts {
            let mut push = |n: &String| {
                if !gvars.contains(n) {
                    gvars.push(n.clone());
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
                _ => {}
            }
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
        // Module init body = top-level statements, guarded for import caching.
        self.in_init = true;
        self.term = None;
        let init = mangle_init(module);
        self.w(&format!("define void @{init}() {{"));
        self.w("entry:");
        self.begin_allocs();
        let flag = self.reg();
        let run = self.lab("initrun");
        let skip = self.lab("initskip");
        self.w(&format!("  {flag} = load i1, ptr @{done}"));
        self.w(&format!("  br i1 {flag}, label %{skip}, label %{run}"));
        self.w(&format!("{run}:"));
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
        self.w(&format!("{skip}:"));
        self.w("  ret void");
        self.w("}");
        self.end_allocs();
        self.in_init = false;
        self.term = None;
        Ok(())
    }

    fn emit_main(&mut self) {
        self.w("define i32 @main() {");
        self.w("entry:");
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
        self.locals.clear();
        self.rep.clear();
        self.term = None;
        self.w(&format!("define %NxVal @{fname}(%NxVal* %args, i64 %nargs) {{"));
        self.w("entry:");
        self.begin_allocs();
        // Memo prologue for purity-proven functions: hit returns cached.
        let fnid = self.memo.get(&(module.to_string(), name.to_string())).copied();
        if let Some(id) = fnid {
            let slot = self.alloca("%NxVal");
            let hit = self.reg();
            self.w(&format!("  store %NxVal zeroinitializer, ptr {slot}"));
            self.w(&format!("  {hit} = call i1 @nx_memo_get(i64 {id}, ptr %args, i64 %nargs, ptr {slot})"));
            let go = self.lab("mhit");
            let miss = self.lab("mmiss");
            self.w(&format!("  br i1 {hit}, label %{go}, label %{miss}"));
            self.w(&format!("{go}:"));
            let cv = self.reg();
            self.w(&format!("  {cv} = load %NxVal, ptr {slot}"));
            self.w(&format!("  ret %NxVal {cv}"));
            self.w(&format!("{miss}:"));
        }
        let saved = self.cur_module.clone();
        let saved_fn = self.cur_fn.clone();
        self.cur_module = module.to_string();
        self.cur_fn = name.to_string();
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
            // needs no runtime guard: take the payload directly.
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
                None => v.clone(),
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
        if let Some(id) = self.memo.get(&(self.cur_module.clone(), self.cur_fn.clone())).copied() {
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
                let rhs = self.emit_expr(value)?;
                match target {
                    nx_ast::Target::Name(name) => self.store_compound(name, *op, &rhs, *span),
                    other => {
                        let cur = self.emit_expr(&other.as_expr(*span))?;
                        let v = self.binop_dyn(&cur, *op, &rhs)?;
                        self.store_target(other, &v, *span)
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
                self.w(&format!("{ok}:"));
                self.w(&format!("  br label %{done}"));
                // The failing path is a separate block, so an assertion that
                // holds costs one branch and nothing else.
                self.w(&format!("{fail}:"));
                match message {
                    Some(m) => {
                        let mv = self.emit_expr(m)?;
                        let mb = self.unbox(&mv);
                        self.w(&format!("  call void @nx_assert_fail_msg(%NxVal {mb})"));
                    }
                    None => self.w("  call void @nx_assert_fail()"),
                }
                self.w("  unreachable");
                self.w(&format!("{done}:"));
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
                self.w(&format!("{condl}:"));
                let c = self.emit_expr(cond)?;
                let b = self.as_i1(&c);
                self.w(&format!("  br i1 {b}, label %{bodyl}, label %{endl}"));
                self.w(&format!("{bodyl}:"));
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
                        self.w(&format!("{endl}:"));
                        self.w("  unreachable");
                    }
                    Some(_) => {
                        // break/continue already branched; no back-edge.
                        self.w(&format!("{endl}:"));
                        self.term = None;
                    }
                    None => {
                        self.w(&format!("  br label %{condl}"));
                        self.w(&format!("{endl}:"));
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
                    // interpreter, so both paths agree.
                    _ => {
                        let n = values.len() as i64;
                        let l = self.reg();
                        self.w(&format!("  {l} = call %NxVal @nx_new_list(i64 {n})"));
                        let p = self.alloca("%NxVal");
                        self.w(&format!("  store %NxVal {l}, ptr {p}"));
                        for e in values {
                            let v = self.emit_expr(e)?;
                            let b = self.unbox(&v);
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
            Stmt::Parallel { tasks, span } => self.emit_parallel(tasks, *span),
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
            nx_ast::Target::Attr { base, field } => Err(err(
                base.span(),
                format!("cannot assign field '{field}': fields come from a type declaration"),
            )),
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
        if matches!(b.ty, Ty::Dict(_)) {
            let kb = self.unbox(&ix);
            let vb = self.unbox(v);
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
        let k = self.as_i64(&ix);
        let vb = self.unbox(v);
        self.w(&format!(
            "  call void @nx_listset(%NxVal {bv}, i64 {k}, %NxVal {vb})"
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
                if self.in_init {
                    let g = self.ensure_global(&self.cur_module.clone(), name);
                    let b = self.unbox(updated);
                    self.w(&format!("  store %NxVal {b}, ptr {g}"));
                } else if self.locals.contains_key(name) {
                    self.store_slot(name, updated);
                } else {
                    self.new_slot(name, None);
                    self.store_slot(name, updated);
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
            nx_ast::Target::Attr { base, field } => Err(err(
                base.span(),
                format!("cannot delete field '{field}': fields come from a type declaration"),
            )),
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
        if !self.in_init && self.rep_of(name).is_some() {
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
                return Ok(NV::boxed_known(c, Ty::Bool));
            }
            BinOp::NotIn => {
                let c = self.reg();
                let n = self.reg();
                self.w(&format!("  {c} = call %NxVal @nx_in(%NxVal {lb}, %NxVal {rb})"));
                self.w(&format!("  {n} = call %NxVal @nx_not(%NxVal {c})"));
                return Ok(NV::boxed_known(n, Ty::Bool));
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
        Ok(NV::boxed_known(out, ty))
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
        Ok(NV::boxed_known(out, Ty::Bool))
    }

    fn store_name(&mut self, name: &str, v: &NV, _span: Span) -> Result<(), CodegenError> {
        if self.in_init {
            // Module globals are the boxed boundary: other modules and
            // parallel tasks reach them by address.
            let g = self.ensure_global(&self.cur_module.clone(), name);
            let b = self.unbox(v);
            self.w(&format!("  store %NxVal {b}, ptr {g}"));
            return Ok(());
        }
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
        } else {
            self.new_slot(name, self.unboxed_ty(name));
            self.store_slot(name, v);
        }
        Ok(())
    }

    /// Always-allocate store (from-imports, loop vars).
    fn store_fresh(&mut self, name: &str, v: &NV) {
        if self.in_init {
            let g = self.ensure_global(&self.cur_module.clone(), name);
            let b = self.unbox(v);
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
            self.w(&format!("{bodyl}:"));
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
            self.w(&format!("{next}:"));
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
        self.w(&format!("{endl}:"));
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
                self.w(&format!("  br label %{condl}"));
                self.w(&format!("{condl}:"));
                let cur = self.reg();
                let go = self.reg();
                let goup = self.reg();
                let godn = self.reg();
                self.w(&format!("  {cur} = load i64, ptr {slot}"));
                self.w(&format!("  {goup} = icmp slt i64 {cur}, {b}"));
                self.w(&format!("  {godn} = icmp sgt i64 {cur}, {b}"));
                self.w(&format!("  {go} = select i1 {up}, i1 {goup}, i1 {godn}"));
                self.w(&format!("  br i1 {go}, label %{bodyl}, label %{endl}"));
                self.w(&format!("{bodyl}:"));
                // The induction variable is statically Int.
                let iv = NV::raw(Ty::Int, cur.clone());
                self.store_fresh(var, &iv);
                self.loops.push((condl.clone(), endl.clone()));
                self.term = None;
                for st in body {
                    self.emit_stmt(st)?;
                    if self.term.is_some() {
                        break;
                    }
                }
                let bt = self.term.take();
                self.loops.pop();
                match bt {
                    Some(Term::Ret) => {
                        self.term = Some(Term::Ret);
                        self.w(&format!("{endl}:"));
                        self.w("  unreachable");
                    }
                    Some(_) => {
                        self.w(&format!("{endl}:"));
                        self.term = None;
                    }
                    None => {
                        let cur3 = self.reg();
                        let nxt = self.reg();
                        self.w(&format!("  {cur3} = load i64, ptr {slot}"));
                        self.w(&format!("  {nxt} = add i64 {cur3}, {step}"));
                        self.w(&format!("  store i64 {nxt}, ptr {slot}"));
                        self.w(&format!("  br label %{condl}"));
                        self.w(&format!("{endl}:"));
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
                self.w(&format!("  br label %{condl}"));
                self.w(&format!("{condl}:"));
                let i = self.reg();
                let go = self.reg();
                self.w(&format!("  {i} = load i64, ptr {islot}"));
                self.w(&format!("  {go} = icmp slt i64 {i}, {n}"));
                self.w(&format!("  br i1 {go}, label %{bodyl}, label %{endl}"));
                self.w(&format!("{bodyl}:"));
                let el = self.reg();
                if is_dict {
                    // Iterating a dict yields its keys, in insertion order.
                    // The runtime helper reads entry `i`'s key directly,
                    // which avoids building the key list first.
                    self.w(&format!(
                        "  {el} = call %NxVal @nx_dictkeyat(%NxVal {vb}, i64 {i})"
                    ));
                    self.store_fresh(var, &NV::dyn_boxed(el));
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
                self.store_fresh(var, &ev);
                }
                self.loops.push((condl.clone(), endl.clone()));
                self.term = None;
                for st in body {
                    self.emit_stmt(st)?;
                    if self.term.is_some() {
                        break;
                    }
                }
                let bt = self.term.take();
                self.loops.pop();
                match bt {
                    Some(Term::Ret) => {
                        self.term = Some(Term::Ret);
                        self.w(&format!("{endl}:"));
                        self.w("  unreachable");
                    }
                    Some(_) => {
                        self.w(&format!("{endl}:"));
                        self.term = None;
                    }
                    None => {
                        let i2 = self.reg();
                        let i3 = self.reg();
                        self.w(&format!("  {i2} = load i64, ptr {islot}"));
                        self.w(&format!("  {i3} = add i64 {i2}, 1"));
                        self.w(&format!("  store i64 {i3}, ptr {islot}"));
                        self.w(&format!("  br label %{condl}"));
                        self.w(&format!("{endl}:"));
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
        self.w(&format!("{condl}:"));
        let i = self.reg();
        self.w(&format!("  {i} = load i64, ptr {ireg}"));
        let done = self.reg();
        self.w(&format!("  {done} = icmp sge i64 {i}, {n}"));
        self.w(&format!("  br i1 {done}, label %{endl}, label %{bodyl}"));
        self.w(&format!("{bodyl}:"));
        let v = self.reg();
        self.w(&format!("  {v} = add i64 {start}, {i}"));
        let bv = self.reg();
        self.w(&format!("  {bv} = call %NxVal @nx_int(i64 {v})"));
        self.w(&format!("  call void @nx_listpush(ptr {p}, %NxVal {bv})"));
        let inc = self.reg();
        self.w(&format!("  {inc} = add i64 {i}, 1"));
        self.w(&format!("  store i64 {inc}, ptr {ireg}"));
        self.w(&format!("  br label %{condl}"));
        self.w(&format!("{endl}:"));
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
                Ok(NV::boxed_known(out, Ty::List(Box::new(Ty::Int))))
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
                    let vb = self.unbox(&vv);
                    self.w(&format!(
                        "  call void @nx_dictset(ptr {p}, %NxVal {kb}, %NxVal {vb})"
                    ));
                }
                let out = self.reg();
                self.w(&format!("  {out} = load %NxVal, ptr {p}"));
                Ok(NV::boxed_known(out, Ty::Dict(Box::new(Ty::Unknown))))
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
                match &b.ty {
                    Ty::List(t) => Ok(NV::boxed_known(out, Ty::List(t.clone()))),
                    Ty::Str => Ok(NV::boxed_known(out, Ty::Str)),
                    _ => Ok(NV::boxed_known(out, Ty::Unknown)),
                }
            }
            Expr::IfExpr { cond, then_value, else_value, .. } => {
                let c = self.emit_expr(cond)?;
                let b = self.as_i1(&c);
                let tl = self.lab("if_then");
                let el = self.lab("if_else");
                let jn = self.lab("if_join");
                self.w(&format!("  br i1 {b}, label %{tl}, label %{el}"));
                self.w(&format!("{tl}:"));
                let tv = self.emit_expr(then_value)?;
                let tb = self.unbox(&tv);
                self.w(&format!("  br label %{jn}"));
                self.w(&format!("{el}:"));
                let ev = self.emit_expr(else_value)?;
                let eb = self.unbox(&ev);
                self.w(&format!("  br label %{jn}"));
                self.w(&format!("{jn}:"));
                // Both arms carry the same `%NxVal` type, so a phi over the
                // boxed form is all that is needed to merge them.
                let out = self.reg();
                self.w(&format!("  {out} = phi %NxVal [ {tb}, %{tl} ], [ {eb}, %{el} ]"));
                // The phi merges the boxed form, so the result type only has to be
                // precise when both arms agree. Otherwise it stays dynamic,
                // which is correct and merely unspecialised.
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
                Ok(NV::boxed_known(out, ty))
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
                    other => other.clone(),
                };
                let itb = self.unbox(&it);
                let is_str = matches!(elem, Ty::Str);
                // The accumulator is addressed so nx_listpush can grow it.
                let slot = self.alloca("%NxVal");
                let acc = self.reg();
                self.w(&format!("  {acc} = call %NxVal @nx_new_list(i64 8)"));
                self.w(&format!("  store %NxVal {acc}, ptr {slot}"));
                // Both containers carry their length in the `b` field.
                let n = self.reg();
                self.w(&format!("  {n} = extractvalue %NxVal {itb}, 2"));
                let ireg = self.alloca("i64");
                self.w(&format!("  store i64 0, ptr {ireg}"));
                let condl = self.lab("comp_cond");
                let bodyl = self.lab("comp_body");
                let endl = self.lab("comp_end");
                self.w(&format!("  br label %{condl}"));
                self.w(&format!("{condl}:"));
                let i = self.reg();
                self.w(&format!("  {i} = load i64, ptr {ireg}"));
                let done = self.reg();
                self.w(&format!("  {done} = icmp sge i64 {i}, {n}"));
                self.w(&format!("  br i1 {done}, label %{endl}, label %{bodyl}"));
                self.w(&format!("{bodyl}:"));
                // Fetch the current element: a string yields a one-byte
                // string, a list yields the stored value.
                let cur = self.reg();
                if is_str {
                    let sp = self.reg();
                    self.w(&format!("  {sp} = extractvalue %NxVal {itb}, 1"));
                    let s = self.reg();
                    self.w(&format!("  {s} = inttoptr i64 {sp} to ptr"));
                    let cp = self.reg();
                    self.w(&format!("  {cp} = getelementptr i8, ptr {s}, i64 {i}"));
                    let c = self.reg();
                    self.w(&format!("  {c} = load i8, ptr {cp}"));
                    // Fresh heap storage per character, not a hoisted
                    // alloca: the string value keeps the pointer, so a
                    // shared buffer would make every element the same byte.
                    let buf = self.reg();
                    self.w(&format!("  {buf} = call ptr @malloc(i64 1)"));
                    self.w(&format!("  store i8 {c}, ptr {buf}"));
                    self.w(&format!("  {cur} = call %NxVal @nx_str(ptr {buf}, i64 1)"));
                } else {
                    self.w(&format!("  {cur} = call %NxVal @nx_listget(%NxVal {itb}, i64 {i})"));
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
                // element expression is emitted exactly once.
                let emit_push = |me: &mut Self| -> Result<(), CodegenError> {
                    let ev = me.emit_expr(element)?;
                    let eb = me.unbox(&ev);
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
                        self.w(&format!("{take}:"));
                        emit_push(self)?;
                        self.w(&format!("  br label %{adv}"));
                        self.w(&format!("{drop}:"));
                        self.w(&format!("  br label %{adv}"));
                        self.w(&format!("{adv}:"));
                    }
                    None => emit_push(self)?,
                }
                let inc = self.reg();
                self.w(&format!("  {inc} = add i64 {i}, 1"));
                self.w(&format!("  store i64 {inc}, ptr {ireg}"));
                self.w(&format!("  br label %{condl}"));
                self.w(&format!("{endl}:"));
                let res = self.reg();
                self.w(&format!("  {res} = load %NxVal, ptr {slot}"));
                Ok(NV::boxed_known(res, items_ty))
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
                    let b = self.unbox(&v);
                    self.w(&format!("  call void @nx_listpush(ptr {p}, %NxVal {b})"));
                }
                let out = self.reg();
                self.w(&format!("  {out} = load %NxVal, ptr {p}"));
                // A list of proven scalars has a known element type, so
                // reading from it can stay unboxed. It is mutable, so it
                // never qualifies for memoization.
                Ok(NV::boxed_known(out, Ty::List(Box::new(elem))))
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
                Err(err(*span, "only direct module.attribute access is supported".to_string()))
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
                    let r = self.reg();
                    self.w(&format!("  {r} = sub i64 0, {}", v.reg));
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
                Ok(NV::boxed_known(r, ty))
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
                let out = self.reg();
                match (op, float) {
                    (BinOp::Add, false) => self.w(&format!("  {out} = add i64 {a}, {b}")),
                    (BinOp::Sub, false) => self.w(&format!("  {out} = sub i64 {a}, {b}")),
                    (BinOp::Mul, false) => self.w(&format!("  {out} = mul i64 {a}, {b}")),
                    // Division keeps the runtime's divide-by-zero panic.
                    (BinOp::Div, false) => {
                        self.w(&format!("  {out} = call i64 @nx_div_i64(i64 {a}, i64 {b})"))
                    }
                    (BinOp::Add, true) => self.w(&format!("  {out} = fadd double {a}, {b}")),
                    (BinOp::Sub, true) => self.w(&format!("  {out} = fsub double {a}, {b}")),
                    (BinOp::Mul, true) => self.w(&format!("  {out} = fmul double {a}, {b}")),
                    (BinOp::Div, true) => {
                        self.w(&format!("  {out} = call double @nx_fdiv(double {a}, double {b})"))
                    }
                    _ => unreachable!(),
                }
                Some(NV::raw(ty, out))
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
                let out = self.reg();
                match op {
                    // The runtime helpers keep the zero-divisor panic.
                    BinOp::Mod => self
                        .w(&format!("  {out} = call i64 @nx_mod_i64(i64 {}, i64 {})", l.reg, r.reg)),
                    BinOp::FloorDiv => self.w(&format!(
                        "  {out} = call i64 @nx_floordiv_i64(i64 {}, i64 {})",
                        l.reg, r.reg
                    )),
                    BinOp::BitAnd => self.w(&format!("  {out} = and i64 {}, {}", l.reg, r.reg)),
                    BinOp::BitOr => self.w(&format!("  {out} = or i64 {}, {}", l.reg, r.reg)),
                    _ => self.w(&format!("  {out} = xor i64 {}, {}", l.reg, r.reg)),
                }
                Some(NV::raw(Ty::Int, out))
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
                let out = self.reg();
                if float {
                    self.w(&format!("  {out} = call double @nx_fpow(double {a}, double {b})"));
                } else {
                    // A negative exponent has no integer answer, and an
                    // oversized one overflows; both go to the runtime,
                    // which panics or saturates exactly as the interpreter does.
                    self.w(&format!("  {out} = call i64 @nx_ipow(i64 {a}, i64 {b})"));
                }
                Some(NV::raw(ty, out))
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
        self.w(&format!("{short}:"));
        self.w(&format!("  br label %{merge}"));
        self.w(&format!("{rhs}:"));
        let rv = self.emit_expr(right)?;
        let rb = self.as_i1(&rv);
        // RHS is an expression: it cannot terminate (no return/break inside).
        self.w(&format!("  br label %{merge}"));
        self.w(&format!("{merge}:"));
        // The result is always a proven Bool: both operands had to be.
        let phi = self.reg();
        self.w(&format!("  {phi} = phi i1 [{decided}, %{short}], [{rb}, %{rhs}]"));
        Ok(NV::raw(Ty::Bool, phi))
    }

    fn emit_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        span: Span,
    ) -> Result<NV, CodegenError> {
        if let Expr::Var(name, _) = callee {
            if name == "len" {
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
                return Ok(NV::raw(Ty::Int, n));
            }
            if name == "push" {
                if args.len() != 2 {
                    return Err(err(span, "push() expects 2 arguments".to_string()));
                }
                let ptr = match &args[0] {
                    Expr::Var(n, _) => self.ptr_of(n).ok_or(err(
                        span,
                        "push() first argument must be a list variable".to_string(),
                    ))?,
                    _ => {
                        return Err(err(
                            span,
                            "push() first argument must be a list variable".to_string(),
                        ))
                    }
                };
                let v = self.emit_expr(&args[1])?;
                let vb = self.unbox(&v);
                self.w(&format!("  call void @nx_listpush(ptr {ptr}, %NxVal {vb})"));
                let r = self.reg();
                self.w(&format!("  {r} = call %NxVal @nx_none()"));
                return Ok(NV::boxed_known(r, Ty::None));
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
                    if self.is_module_fn(&module, attr) {
                        return self.emit_direct(&module, attr, args);
                    }
                    return Err(err(span, format!("'{attr}' is not a function of '{module}'")));
                }
            }
            return Err(err(span, "only direct module.attr() calls are supported".to_string()));
        }
        Err(err(span, "only direct calls are supported".to_string()))
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
        let n = args.len();
        let arr = self.alloca(&format!("[{n} x %NxVal]"));
        for (i, a) in args.iter().enumerate() {
            let v = self.emit_expr(a)?;
            // The ABI is boxed: every argument re-boxes here.
            let vb = self.unbox(&v);
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
        let ty = self.ret_ty(module, name);
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

    fn emit_parallel(&mut self, tasks: &[Stmt], span: Span) -> Result<(), CodegenError> {
        if tasks.is_empty() {
            return Ok(());
        }
        let module = self.cur_module.clone();
        let locals: HashSet<String> = self.locals.keys().cloned().collect();
        let sums = nx_ir::task_summaries(&self.programs, &module, &locals, tasks).map_err(|e| {
            CodegenError { message: e.message, line: span.line, col: span.col }
        })?;
        for batch in nx_ir::partition(&sums) {
            if batch.len() == 1 {
                self.emit_stmt(&tasks[batch[0]])?;
                if self.term.is_some() {
                    // Return inside a task: rejected by the checker.
                    return Err(err(span, "return inside parallel task is not supported".to_string()));
                }
                continue;
            }
            self.emit_threaded_batch(&module, tasks, &batch, span)?;
        }
        Ok(())
    }

    /// Outline each task in `batch` as its own function, run them all
    /// concurrently, then join. Reads of enclosing locals are snapshotted
    /// into globals first, so a task only ever sees a consistent copy.
    fn emit_threaded_batch(
        &mut self,
        module: &str,
        tasks: &[Stmt],
        batch: &[usize],
        span: Span,
    ) -> Result<(), CodegenError> {
        // Snapshot the enclosing locals each task reads (read-only sharing).
        let mut reads: Vec<String> = Vec::new();
        for &i in batch {
            collect_outer_reads(&tasks[i], &self.locals, &mut reads);
        }
        let site = self.lab("par");
        let mut snap_globals = Vec::new();
        for (k, name) in reads.iter().enumerate() {
            let g = format!("nx__snap_{site}_{k}");
            self.top.push_str(&format!("@{g} = global %NxVal zeroinitializer\n"));
            let v = self.emit_expr(&Expr::Var(name.clone(), span))?;
            // Tasks read the snapshot through globals, so it stays boxed.
            let vb = self.unbox(&v);
            self.w(&format!("  store %NxVal {vb}, ptr @{g}"));
            snap_globals.push(g);
        }
        // Outline each task; save ambient codegen state around it.
        let saved_locals = self.locals.clone();
        let saved_rep = self.rep.clone();
        let saved_modrefs = self.modrefs.clone();
        let saved_falias = self.falias.clone();
        let saved_loops = std::mem::take(&mut self.loops);
        let mut fnames = Vec::new();
        for &i in batch {
            let fname = format!("nx__task_{site}_{i}");
            self.locals.clear();
            // Task bodies run on another thread: every slot they see is
            // boxed, regardless of the enclosing function's unboxing.
            self.rep.clear();
            self.term = None;
            self.to_top = true;
            self.w(&format!("define ptr @{fname}(ptr %_) {{"));
            self.w("entry:");
            self.begin_allocs();
            // Rehydrate snapshot reads as task locals.
            for (k, name) in reads.iter().enumerate() {
                let v = self.reg();
                let slot = self.alloca("%NxVal");
                self.w(&format!("  store %NxVal zeroinitializer, ptr {slot}"));
                self.w(&format!("  {v} = load %NxVal, ptr @{}", snap_globals[k]));
                self.w(&format!("  store %NxVal {v}, ptr {slot}"));
                self.locals.insert(name.clone(), slot);
            }
            self.emit_stmt(&tasks[i])?;
            if self.term.is_some() {
                return Err(err(span, "return inside parallel task is not supported".to_string()));
            }
            self.w("  ret ptr null");
            self.w("}");
            self.end_allocs();
            self.to_top = false;
            fnames.push(fname);
        }
        self.locals = saved_locals;
        self.rep = saved_rep;
        self.modrefs = saved_modrefs;
        self.falias = saved_falias;
        self.loops = saved_loops;
        self.term = None;
        self.run_pool(&fnames);
        let _ = module;
        Ok(())
    }

    /// Run a batch through a pool sized to the batch: one worker per
    /// task, each claiming from a shared cursor until it is drained, then
    /// join everyone. A worker that finishes early picks up the next
    /// task, so uneven tasks still balance.
    ///
    /// The pool is per-block rather than global on purpose: no process
    /// wide mutable state, nothing to shut down, and no question about
    /// whether a pool outlives the code that queued work into it.
    fn run_pool(&mut self, fnames: &[String]) {
        let n = fnames.len() as i64;

        // The pool block lives in the caller's frame, so it outlives every
        // worker: they are all joined before this function returns.
        let pool = self.alloca("%NxPool");
        let arr = self.alloca(&format!("[{n} x ptr]"));
        for (i, fname) in fnames.iter().enumerate() {
            let slot = self.reg();
            self.w(&format!("  {slot} = getelementptr [{n} x ptr], ptr {arr}, i64 0, i64 {i}"));
            self.w(&format!("  store ptr @{fname}, ptr {slot}"));
        }
        let cursor = self.reg();
        self.w(&format!("  {cursor} = getelementptr %NxPool, ptr {pool}, i64 0, i32 0"));
        self.w(&format!("  store i64 0, ptr {cursor}"));
        let cnt = self.reg();
        self.w(&format!("  {cnt} = getelementptr %NxPool, ptr {pool}, i64 0, i32 1"));
        self.w(&format!("  store i64 {n}, ptr {cnt}"));
        let tasks = self.reg();
        self.w(&format!("  {tasks} = getelementptr %NxPool, ptr {pool}, i64 0, i32 2"));
        self.w(&format!("  store ptr {arr}, ptr {tasks}"));

        // Start the workers. All of them before any join, so the batch
        // overlaps instead of running one task at a time.
        let mut handles = Vec::new();
        for _ in fnames {
            let h = self.reg();
            self.w(&format!("  {h} = call ptr @nx_thread_start(ptr @nx_pool_worker, ptr {pool})"));
            handles.push(h);
        }
        // The calling thread is a worker too: with n tasks it saves a
        // thread, and it guarantees forward progress even if a spawn
        // were to fail.
        let me = self.reg();
        self.w(&format!("  {me} = call ptr @nx_pool_worker(ptr {pool})"));
        for h in handles {
            self.w(&format!("  call void @nx_thread_join(ptr {h})"));
        }
    }
}

/// Enclosing-local names read anywhere inside a task statement.
fn collect_outer_reads(s: &Stmt, locals: &HashMap<String, String>, out: &mut Vec<String>) {
    match s {
        Stmt::Assign { targets, values, .. } => {
            // `a[i] = v` reads the container, so the enclosing name is
            // still captured; a plain bind is not a read.
            for t in targets {
                match t {
                    nx_ast::Target::Index { base, .. } | nx_ast::Target::Attr { base, .. } => {
                        collect_outer_reads_expr(base, locals, out)
                    }
                    nx_ast::Target::Name(_) => {}
                }
            }
            for v in values {
                collect_expr_reads(v, locals, out);
            }
        }
        Stmt::AssignOp { target, value, .. } => {
            match target {
                nx_ast::Target::Name(name) => {
                    if locals.contains_key(name) && !out.contains(name) {
                        out.push(name.clone());
                    }
                }
                nx_ast::Target::Index { base, index } => {
                    collect_outer_reads_expr(base, locals, out);
                    collect_expr_reads(index, locals, out);
                }
                nx_ast::Target::Attr { base, .. } => collect_outer_reads_expr(base, locals, out),
            }
            collect_expr_reads(value, locals, out);
        }
        Stmt::Del { targets, .. } => {
            for t in targets {
                match t {
                    nx_ast::Target::Index { base, .. } | nx_ast::Target::Attr { base, .. } => {
                        collect_outer_reads_expr(base, locals, out)
                    }
                    nx_ast::Target::Name(_) => {}
                }
            }
        }
        Stmt::Assert { cond, message, .. } => {
            collect_expr_reads(cond, locals, out);
            if let Some(m) = message {
                collect_expr_reads(m, locals, out);
            }
        }
        Stmt::Print { values, .. } => {
            for v in values {
                collect_expr_reads(v, locals, out);
            }
        }
        Stmt::If { cond, then_body, elifs, else_body, .. } => {
            collect_expr_reads(cond, locals, out);
            for t in then_body {
                collect_outer_reads(t, locals, out);
            }
            for (_, b) in elifs {
                for t in b {
                    collect_outer_reads(t, locals, out);
                }
            }
            if let Some(b) = else_body {
                for t in b {
                    collect_outer_reads(t, locals, out);
                }
            }
        }
        Stmt::While { cond, body, .. } => {
            collect_expr_reads(cond, locals, out);
            for t in body {
                collect_outer_reads(t, locals, out);
            }
        }
        Stmt::For { var, iter, body, .. } => {
            match iter {
                nx_ast::ForIter::Range { start, end } => {
                    collect_expr_reads(start, locals, out);
                    collect_expr_reads(end, locals, out);
                }
                nx_ast::ForIter::Each(e) => collect_expr_reads(e, locals, out),
            }
            // The loop var shadows; reads of it inside are task-local.
            let mut inner = locals.clone();
            inner.remove(var);
            for t in body {
                collect_outer_reads(t, &inner, out);
            }
        }
        Stmt::Return { values, .. } => {
            for e in values {
                collect_expr_reads(e, locals, out);
            }
        }
        Stmt::Parallel { tasks, .. } => {
            for t in tasks {
                collect_outer_reads(t, locals, out);
            }
        }
        Stmt::Expr(e) => collect_expr_reads(e, locals, out),
        _ => {}
    }
}

/// Captured enclosing-local reads under an expression used as an
/// assignment base (`a[i] = v`). Kept separate from `collect_expr_reads`
/// because the name it finds is the container's, not a plain read.
fn collect_outer_reads_expr(
    e: &Expr,
    locals: &HashMap<String, String>,
    out: &mut Vec<String>,
) {
    collect_expr_reads(e, locals, out)
}

fn collect_expr_reads(e: &Expr, locals: &HashMap<String, String>, out: &mut Vec<String>) {
    match e {
        Expr::Var(n, _) => {
            if locals.contains_key(n) && !out.contains(n) {
                out.push(n.clone());
            }
        }
        Expr::Attr { base, .. } => collect_expr_reads(base, locals, out),
        Expr::Index { base, index, .. } => {
            collect_expr_reads(base, locals, out);
            collect_expr_reads(index, locals, out);
        }
        Expr::List(items, _) => {
            for it in items {
                collect_expr_reads(it, locals, out);
            }
        }
        Expr::Dict(pairs, _) => {
            for (k, v) in pairs {
                collect_expr_reads(k, locals, out);
                collect_expr_reads(v, locals, out);
            }
        }
        Expr::Range { start, end, .. } => {
            collect_expr_reads(start, locals, out);
            collect_expr_reads(end, locals, out);
        }
        Expr::Slice { base, from, to, step, .. } => {
            collect_expr_reads(base, locals, out);
            for part in [from, to, step].into_iter().flatten() {
                collect_expr_reads(part, locals, out);
            }
        }
        Expr::IfExpr { cond, then_value, else_value, .. } => {
            collect_expr_reads(cond, locals, out);
            collect_expr_reads(then_value, locals, out);
            collect_expr_reads(else_value, locals, out);
        }
        Expr::Comprehension { element, var, iter, cond, .. } => {
            // The loop variable is bound inside the comprehension, so it
            // must not be counted as a read of an enclosing binding.
            let mut inner = locals.clone();
            inner.remove(var);
            collect_expr_reads(iter, locals, out);
            collect_expr_reads(element, &inner, out);
            if let Some(c) = cond {
                collect_expr_reads(c, &inner, out);
            }
        }
        Expr::Unary { expr, .. } => collect_expr_reads(expr, locals, out),
        Expr::Binary { left, right, .. } => {
            collect_expr_reads(left, locals, out);
            collect_expr_reads(right, locals, out);
        }
        Expr::Call { callee, args, .. } => {
            collect_expr_reads(callee, locals, out);
            for a in args {
                collect_expr_reads(a, locals, out);
            }
        }
        _ => {}
    }
}
