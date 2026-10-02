# Changelog

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
