# Changelog

## v0.4.4

### ARGONE revised: MLIR removed as a prerequisite, and the audit found something worse

A ten-agent parallel investigation, reconciled against the tree. Full
report in `docs/architecture/audit.md`; plan in `docs/ARGONE.md`; gate in
`docs/ARGONE-STATUS.md`.

**MLIR is no longer a prerequisite, dependency, or completion criterion.**
Tasks G-K previously described a Nexum MLIR dialect and an MLIR lowering
pipeline. They are replaced by a backend boundary trait, structured LLVM
generation through the LLVM C API, and pipeline integration. MLIR may be
added later if a concrete requirement justifies it. JIT, ORC and LLJIT are
permanently out of scope.

### Corrections to v0.4.3, on the record

Three claims in the previous entry were wrong.

- **"`static.crates.io` is unreachable."** It is reachable. A scratch crate
  resolves and downloads (`cargo fetch`, exit 0, fetched `unicode-width
  v0.2.2`). The earlier probe fetched the CDN *root*, which returns 403
  because it serves no directory index, and I read that as blocked.
- **"No C++ toolchain."** MSVC 14.44.35207 is installed at
  `C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\`.
  It needs `vcvars64.bat` to enter the environment, not installation.
- **"No LLVM C API."** `LLVM-C.dll` (71 MB) and `LLVM-C.lib` (293 KB) are both
  present under `C:\Program Files\LLVM`. There are no `llvm-c` headers,
  which does not matter for Rust FFI.

Only one of the three blockers was real: MLIR tools genuinely do not exist
on this machine. That is no longer a blocker.

The AST-arm duplication figure (100 `Stmt` / 133 `Expr`) was also wrong.
Measured: 146 and 118 — and the total was the wrong target anyway.

### Eleven verified interpreter/native divergences

Every one executed on this host.

| Program | Interpreter | Native |
| --- | --- | --- |
| `9223372036854775807 + 1` | error `integer overflow` | `-9223372036854775808` |
| `9223372036854775807 * 2` | error | `-2` |
| `2 ** 100` | saturates to `i64::MAX` | `0` |
| `2 ** -1` | error, names the fix | `1` |
| `"" in "hello"` | `true` | `false` |
| `"hello" in "hello"` | `true` | `false` |
| `len("héllo")` | `5` | `6` |
| `"héllo"[1]` | `é` | invalid byte |
| `"héllo"[0:2]` | `hé` | `h` + invalid byte |
| `1.0e15` | `1000000000000000.0` | `1000000000000000` |
| `del d["z"]` (missing) | error | silently no-ops |

The three-way differential is green because no example or benchmark reaches
these cases: `listsum` peaks near 4e9, `fib` stops at 34, `syntax` tops out
at `2**10`. The harness is sound; the corpus is empty exactly where the
compiler is wrong.

**Root cause is architectural.** `docs/grammar.md:12` lists
`compiler/nx-codegen/src/runtime.ll` as the authority for "Runtime meaning".
The specification points at a backend. The interpreter is a cross-check on a
backend, not the reverse. Every divergence exists because a rule was written
once in Rust and once in LLVM IR with no owner.

This is now **Task 0**, gating every representation change: building a typed
HIR while the backend silently wraps integers produces two wrong
implementations instead of one.

### A bug the differential cannot see by construction

`nx_ir::expr` has a `_ => {}` arm that silently drops `Dict`, `Slice`,
`IfExpr` and `Comprehension`, so a function reading a global only inside a
dict literal is memoized. Confirmed by execution and by `--emit-ir` showing
`nx_memo_put`/`nx_memo_get` for the function:

```nexum
g = 5
fn f(k):
    d = {"a": g}
    return d["a"]
print(f(0))   # 5
g2()          # writes g = 10
print(f(0))   # 5 -- stale; should be 10
```

Both engines produce the stale `5` because both call the same
`memoizable`. Interpreter/native comparison is structurally incapable of
catching this; only a property-based test would.

A related claim — that memoization caches a freed container pointer — was
**not reproduced**: the function is not memoized in that shape, and an
AddressSanitizer build reports no error.

### Harness defects found

`tools/verify.ps1`, the oracle:

- runs `%USERPROFILE%\.cargo\bin\nx.exe`, a **stale installed binary**, so it
  can report all-green against a broken checkout
- drops blank lines and any line starting with whitespace plus `+`
- uses `Compare-Object`, a set comparison, while the header and changelog
  claim "byte-identical"
- checks no exit code on any of its three runs, so a program failing
  identically on all three compares equal and reports `ok`
- globs non-recursively, so `examples/modules/` never runs

CI natively compiles 11 of 13 examples, omitting `records.nx`, `methods.nx`
and `syntax.nx` — the only coverage of records, `mut self`, `del`,
comprehensions, slices and `None`.

### Fixes in this entry

- `tools/argone-gate.ps1` and `tools/argone-gate-tests.ps1` built paths with
  literal `\`, which PowerShell on Unix treats as a filename character. The
  `argone-gate` CI job I added in v0.4.3 therefore exited 2 on every
  `ubuntu-latest` run. Both now build paths with `Join-Path`, and the
  self-tests invoke `pwsh` on PowerShell Core rather than `powershell` by
  name.
- `docs/ARGONE.md`, `docs/ARGONE-STATUS.md` and the gate's expected-task list
  rewritten from 17 MLIR-shaped tasks to 15: Task 0 (semantic correctness)
  plus A-N. The two-state rule is unchanged. All ten adversarial gate tests
  pass.

No compiler behaviour changed. 318 tests pass; the 13 examples still agree
across interpreter / native / `NX_NOUNBOX`.

## v0.4.3

### ARGONE: the architecture stage is planned, gated, and started

`Nexum_Argone_Unified_AOT_Prompt.md` defines ARGONE as a hard stage gate:
the compiler moves from an AST-heavy, directly-to-textual-LLVM design to a
layered pipeline with its own HIR and MIR/SSA, a real MLIR dialect, and an
AOT backend. Stage 4 through Stage 8 do not resume until it is 100%
complete.

- `docs/ARGONE.md` — the breakdown: 17 tasks (Task 0 plus A-P, the prompt's
  mandated order), each with deliverables and a checkable gate. A, B and C
  are independent of the toolchain and can run in parallel; D onward is
  strictly sequential.
- `docs/ARGONE-STATUS.md` — the completion checklist. This file *is* the
  gate.
- `tools/argone-gate.ps1` — decides whether the stage is complete. Rejects
  `in progress`, `partial`, `mostly`, `pending` and `blocked` as statuses,
  because a stage that admits a third state is a stage that gets left
  halfway. A task may claim `complete` only with every gate item checked
  *and* an evidence line, which is what makes scaffolding insufficient.
- `tools/argone-gate-tests.ps1` — ten adversarial tests that try to fool the
  gate the four ways the prompt names as invalid grounds for declaring
  success: scaffolding, a partial status, unchecked boxes, and a build that
  merely compiles. It also proves the gate *opens* on a genuinely complete
  stage, so the mechanism cannot rot into something that always fails.
- CI gains an `argone-gate` job that runs the gate's self-tests and prints
  the stage state.

### Task 0: a prerequisite Argone discovered

Measuring this host before committing to an architecture:

| Check | Result |
| --- | --- |
| clang | 23.1.2, installed |
| MLIR tools (`mlir-opt`, `mlir-translate`, `mlir-tblgen`) | **absent** |
| `opt`, `llc`, `llvm-config` | **absent** |
| `static.crates.io` (crate download CDN) | **unreachable** |
| cached crates binding LLVM/MLIR | none |
| C++ toolchain (`cl.exe`) | not on PATH |

The LLVM Windows installer ships clang, lldb and lld but not MLIR, so the
MLIR stages have no toolchain here, and the blocked crate CDN means
`melior`/`inkwell`/`mlir-sys` cannot be fetched either. Nexum has **zero**
external crate dependencies and keeps it that way unless the toolchain route
forces otherwise.

So obtaining an MLIR toolchain is Task 0, an Argone task with its own gate,
resolved *before* HIR begins. Once HIR exists the lowering strategy is
committed; discovering MLIR is unobtainable at that point would strand the
work. If no route is viable, the correct outcome is a documented
renegotiation with the user — not a quietly narrowed Argone that reports
success for the easy parts.

### Measured duplication Argone targets

Five crates walk the AST independently, re-deriving the same facts:

| Crate | `Stmt` arms | `Expr` arms |
| --- | --- | --- |
| nx-types | 15 | 37 |
| nx-ir | 23 | 13 |
| nx-mem | 21 | 18 |
| nx-interp | 14 | 28 |
| nx-codegen | 27 | 37 |
| **total** | **100** | **133** |

Task C's gate is that this number falls measurably. If it does not, the new
abstraction is not earning its place.

Baseline for comparison: 17,290 Rust lines across 9 crates, 318 tests,
2,956-line runtime, zero external dependencies, 13 examples with a 3-way
differential, 5 Rust-referenced benchmarks.

## v0.4.2

### The grammar, written down
`docs/grammar.md` documents the whole language, derived from the lexer,
parser and checker rather than from intent, with every rule naming the
function that enforces it. Lexical grammar, the full statement and
expression grammar, a precedence table, the ambient surface, the static
rules, and a complete program whose output is verified by running it three
ways.

Writing it turned up **five real bugs**, four of them pre-existing.

### Fixed: `free()` on a read-only string constant
`nx_str` does not copy -- it stores the caller's pointer, so a string
literal's payload points straight into read-only static memory. But
`nx_free_val` called `free` on it. Any function whose Unique local held a
string aborted with `STATUS_HEAP_CORRUPTION`:

```
fn f():
    s = "hi"
f()
```

Three lines. Every `fn` with a non-escaping string local hit it.

The fix is consistency rather than a patch: strings are shared, never
mutated in place, and `needs_clone` never copies one -- so a shared object
with many owners cannot be freed by any of them. `nx_free_val` no longer
frees strings. `nx_strcat` and `nx_slice` do allocate, so those now leak,
which is the documented model for this release.

### Fixed: a loop variable was a module global
A `for` variable took the `in_init` path and became a module global, so it
leaked into every later statement and two loops collided on the name.
Inside a `parallel:` task that collision is a data race, which would break
the determinism contract outright. Loop variables are now always local
slots.

### Fixed: a nested loop destroyed the outer loop's variable
`for i in 0..3:` containing `for i in 0..2:` left the outer `i` holding the
inner loop's last value. Observable, in both the interpreter and the
backend:

```
for i in 0..3:
    for i in 0..2:
        print("in", i)
    print("out", i)     # printed 1 every time, should be 0, 1, 2
```

A loop variable is now scoped to its loop and the previous binding is
restored on every exit -- normal, `break`, `continue`, `return` and error
-- in both the frame path (inside a function) and the globals path (module
level), which are two different binding paths.

### Fixed: a trailing `parallel:` block emitted invalid LLVM
```
x = 0
parallel:
    a = 1
    b = 2
```

`clang` rejected it with "expected instruction opcode". The globals pass
only scanned top-level statements, so a name first assigned inside a task
was discovered *during* emission and its declaration landed between an
`entry:` label and the next instruction. The pass now walks task bodies.

### Fixed: a `mut self` write-back could be silently dropped
`store_name` tested `in_init` before checking for a local slot, so a
write-back to a loop variable went to a module global while the next read
used the local slot. The update vanished: the code compiled, ran, and
printed the old value. A name with a local slot is a local, full stop.

Fixing that exposed a latent one: `cur_fn` was never set to `<top>` while
emitting the module body, so every lookup keyed on the current function --
unboxing, method dispatch on a local, the memory plan -- missed. Top-level
code was never unboxed at all, and a method call on a top-level local could
not resolve. Both now work.

### Known gaps, not fixed here
- `del p` after a `mut self` write-back on a record fails to compile
  ("only modules, types and builtins support attribute calls"). Plain
  `del p`, `del xs[0]` and `del d["a"]` are all fine.
- `del d["a"]` on a single-key dict makes the checker report `d` as
  undefined afterwards.

### Verification
322 tests pass. All 13 examples agree across interpreter / native /
`NX_NOUNBOX`. All 5 benchmarks match their Rust reference output. The
grammar's example program runs identically on all three paths and its
documented output is machine-checked.

## v0.4.1

### Numeric representation audit
Full audit in `docs/numeric-audit.md`. Every numeric width in the compiler
and the emitted runtime is classified with a measurement or a reason.

- **Source positions are `u32`.** `LineNo`/`ColNo` live in `nx-lexer`,
  where positions are born, and every crate uses them. `Span` 16 -> 8
  bytes, `Expr` 64 -> 56, `Stmt` 192 -> 168. A position is bounded by the
  file it describes and is never used in arithmetic, so there was nothing
  to lose; `nx check` peak working set on a 400-function program drops
  12.4 -> 11.3 MiB (-8.9%).
- **Container headers are 16 bytes.** `%NxList` and `%NxDict` count
  elements, not bytes, and 2^31 elements needs 32 GiB behind a runtime
  with no collector, so both counters are `i32`. 24 -> 16 bytes per
  header, measured against the CRT at 8 bytes per block.
- **The header `malloc` moved with the struct.** Narrowing the type while
  leaving `malloc(24)` saves nothing and leaves 8 bytes of tail padding.
  The first attempt did exactly that and measured as no change at all.
  A test now pins struct and allocation together.
- **Counts cannot wrap.** `nx_listpush` and `nx_dictset` check for
  saturation before narrowing, on the growth path, so a push costs a 32-bit
  store instead of a 64-bit one -- same instruction count, half the bytes.
- Rejected, with measurements: narrowing `%NxVal.tag` (lands in padding and
  adds an extension to every tag check), narrowing `%NxVal.extra` (also
  padding; and it is the reason `len()` is a load rather than a pointer
  chase), narrowing `%NxRec.nfields` and `%NxDesc` (padding, and there is
  one descriptor per *type*), dropping the memo slot's `used` flag (3% of
  a table that is mostly its argument slots), narrowing the memo lock and
  hash (a contended atomic and FNV-1a's definition), `f64` -> `f32`
  anywhere (changes results), and boxing `Expr::Comprehension`'s variable
  (trades an allocation per comprehension for 8 bytes).
- New structure-size tests in `nx-ast` and `nx-codegen` pin every width
  above, so a widening is a test failure rather than a quiet regression.

### Not done, and why
Deleting `%NxVal.extra` would take every NX value from 24 to 16 bytes --
33% off every list element, dict entry, memo slot and call argument array.
It is 53 sites in `runtime.ll` in the core value representation of a
language with no garbage collector, so it wants its own stage and a
sanitizer sweep rather than an audit applied in passing. Also deferred:
the 960 KiB memo table in every binary, constant-range narrowing below
`i64`, and flat scalar array storage. See `docs/numeric-audit.md` §6.

### Verification
307 tests pass. All 13 examples agree across interpreter / native /
`NX_NOUNBOX`. All 5 benchmarks match their Rust reference output. A
same-session A/B of the benchmark suite shows mixed signs across five
benchmarks (two faster), i.e. noise rather than a regression.

## v0.4.0

### Stage 3: `impl` blocks and methods
- `impl T:` attaches functions to a type declared in the same module.
  `fn name(self)` is a method, called `v.name(...)`; `fn name(mut self)`
  additionally writes its result back into the receiver, so
  `p.moved(1.0, 1.0)` means `p = moved(p, 1.0, 1.0)`; `fn name()` is an
  associated function, called `T.name(...)`.
- Writing through a read-only `self` is a compile error. `self` is a copy
  under value semantics, so such a write would be silently discarded --
  the compiler says so instead of letting it look meaningful.
- A `mut self` method must return the record: its result *is* the new
  receiver, so anything else would clobber it with the wrong type.
- A `mut self` call on a receiver with no storage still evaluates, it just
  has nowhere to write. That is what makes `q.moved(1, 1).moved(2, 2)`
  read as one expression.
- Orphan impls are refused: an `impl` must live with its `type`. Two
  modules holding incompatible layouts for one name is exactly what a
  static type system cannot represent.
- Resolution order for `base.attr(...)` is module, associated function,
  impl method, then builtin sugar. Methods therefore win over the sugar,
  so a type may define its own `push`. An unresolved base still takes
  sugar, which is what keeps `x.push(1)` working on dynamic values.
  Dynamic dispatch on an unresolved receiver is Stage 4.
- Methods are their own analysis scope under `Type.method` keys in
  `nx-ir` and `nx-mem`, so they never collide with same-named plain
  functions in the effect or memory plans.
- A method is never memoized. Purity analysis reasons about a function's
  own body, but a `mut self` method's contract extends past it: the call
  writes back at the call site, and a cache hit would skip that write.
- Method dispatch types come from the checker, not from the unboxing
  decision. `NX_NOUNBOX=1` keeps working method for method, so the opt-out
  stays a debug switch instead of becoming a second language.

### Fixed
- A method receiver was emitted twice in the backend -- once to learn its
  static type, once to build the argument list. For a `mut self` receiver
  that ran the write-back twice, so `q.moved(1, 1).moved(1, 1)` advanced
  `q` three times instead of once. Receivers are now evaluated once.
- `nx_memo_put` was emitted from a second lookup of the memo id, so a
  function could write a cache entry it never read, under an id belonging
  to another function. The prologue's decision is now the only one.
- Checking a function body consumed the enclosing scope, so every
  statement after the first `fn` in a module saw an empty module and
  reported its variables undefined. The locals are now returned instead
  of taken.
- `T.m(...)` asked the expression checker about `T`, which is a type and
  not a binding, so it was reported undefined. Associated-function calls
  are now resolved before the base is evaluated as a value.
- `self` was a keyword the expression parser did not accept, so
  `self.x = 1` -- the whole reason `mut self` exists -- did not parse.

### Tooling
- `tools/verify.ps1` runs every example three ways -- interpreter, native,
  and native with `NX_NOUNBOX=1` -- and diffs the output. All 13 examples
  must agree; a disagreement is a build failure.
- All five benchmarks still match their Rust reference output.

## v0.3.0

### Purity-directed automatic memoization
- `nx-ir` proves a function Pure; the compiler caches it automatically,
  with no annotation and no source change. Scalar arguments only, so
  list mutation stays visible. 4096-entry bounded cache, cmpxchg
  spinlock in the runtime (no pthreads, so it links on Windows too).
- Opt out with `NX_NOMEMO=1`. Interpreter `fib(30)`: 80.17s -> 0.09s;
  native `fib(90)`: 0.19s.
- Parallel worker threads get a 64 MiB stack: memoized recursion reaches
  a depth that overflowed the 2 MiB default.

### Unboxing optimizer
- Values with a statically known scalar type live in bare `i64`/`double`
  /`i1` registers and stack slots, with raw LLVM arithmetic. Boxing is
  re-inserted only at dynamic boundaries: call arguments, returns, list
  elements, module globals, `print`. Function boundaries stay boxed.
- `nx-types` now infers parameter types from the body. Uses with exactly
  one answer pin the type (`xs[i]` is `Int`, `if b` is `Bool`,
  `x < 1.5` is `Float`); a parameter used only in arithmetic is numeric
  and defaults to `Int`, matching how a numeric local is fixed by its
  first binding.
- Fixed a soundness hole the unboxing exposed: an unresolved expression
  assigned to a known-typed variable now widens that variable to
  unresolved, instead of silently keeping the narrow type.
- `nx_eq` on floats now uses IEEE comparison rather than bit equality, so
  `NaN != NaN` and `0.0 == -0.0` hold and the boxed and unboxed paths
  agree with the interpreter.
- Opt out with `NX_NOUNBOX=1`. ~6.3x faster on numeric loops, identical
  output.

### Task pool for `parallel:`
- A batch gets a pool sized to the batch instead of one thread per task.
  Workers claim task indices from a shared cursor, so a thread that
  finishes early picks up the next task instead of idling, and uneven
  tasks still balance. The calling thread works too, which saves a
  thread and guarantees the batch drains even if a spawn fails.
- The pool is per-block, not global: no process-wide mutable state,
  nothing to shut down, and no question about a pool outliving the code
  that queued work into it.
- Only the two OS bindings differ between platforms, so they moved to
  `runtime_threads_win.ll` / `runtime_threads_unix.ll`. The pool logic
  is shared and therefore tested identically everywhere.
- 8 independent tasks: 0.190s sequential -> 0.033s, a 6x speedup on 16
  logical cores (4.42x with thread-per-task), output byte-identical.
- CI now runs the parallel example 8 times and diffs, since threads make
  output order a real risk.

### Windows threaded `parallel:`
- `parallel:` batches now run on real threads on Windows instead of
  lowering sequentially, so the block means the same thing on every
  platform. 8 independent tasks: 0.193s sequential -> 0.044s, a 4.42x
  speedup on 16 logical cores, with byte-identical output.
- The HANDLE comes from `CreateThread`'s return value. Its last argument
  is `lpThreadId`, a DWORD id, and waiting on that silently failed
  every time -- which is why the first Windows build produced zeros.
- `runtime.ll` declares both platforms' threading APIs and stays
  platform-neutral: an unused declaration emits no symbol reference.
- `collect_outer_reads` and the batch emitter are no longer unix-only.
- CI asserts the generated IR actually contains spawn calls. A batch
  that quietly fell back to inline execution would still pass the
  differential test while doing no parallel work at all.

### Tooling
- `nx build --emit-ir` prints the generated LLVM IR.
- Build stamp invalidates the cache when `NX_NOMEMO` or `NX_CFLAGS`
  change, so a flag flip rebuilds instead of reusing a stale binary.
- CI now checks that unboxed and `NX_NOUNBOX` builds produce identical
  output.

## v0.2.0
- Deterministic `parallel:` blocks (threaded interpreter, pthread codegen)
- Effects analysis (`nx-ir`, `nx dump-ir`)
- `nx setup` per-theme icon plus-one; icon coexistence policy
- `nx build` incremental + `--run`; `NX_CFLAGS` passthrough

## v0.1.0
- Memory planner (`nx-mem`): Unique locals freed at scope exit, Shared arena
- `NX_CFLAGS` passthrough; ASan differential in CI (Linux)

## v0.0.8
- LLVM backend: `nx build` native exes, `nx build --run`
- Interpreter and binaries verified byte-identical in CI

## v0.0.7
- Modules: `import` / `as` / `from…import`, `mod.member`, circular detection
- `nx check` static type checker (advisory)

## v0.0.6
- `nx update` finds the `code` CLI outside PATH

## v0.0.5
- `nx update` self-elevates via UAC on Windows (no more Access denied)

## v0.0.4
- VS Code: Nexum file icon theme (blue N on `.nx` files)
- Extension publisher `HasanSalaou`

## v0.0.3
- Windows MSI installer (PATH + bundled VS Code extension)
- `nx update` also updates the VS Code extension

## v0.0.2
- `nx update` self-updater (`nx update --version <ver>` to pin)
- `nx --version` / `nx --license` (MIT embedded in exe)
- Raw `nx` binaries per release (no more zips)

## v0.0.1
- Initial interpreter: lexer, parser, tree-walk `nx file.nx`
- Indent blocks, `if/elif/else`, `while`, `for i in a..b` + `for x in list`
- `fn` + `return` + recursion, scopes
- Lists, indexing incl. negative, `len()`, `push()`
- `break` / `continue`, `+= -= *= /=`
- `Int` / `Float` (15-sig display) / `Bool` / `Str`
- VS Code extension: highlighting + `:` auto-indent
