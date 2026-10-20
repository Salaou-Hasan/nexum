# The error handling model

# The error handling model

**Question.** What is Nexum's error handling model, and how does a freestanding
runtime report and recover from failure? Concretely: how does a function report
failure, how does the runtime panic, how is the stack unwound (or not), what
is the shape of an error type, and how does this interact with the
no-C-library, no-exceptions constraint? The language has no `Option` or
`Result` yet, and the runtime already has `nexum_panic` and `nexum_oom`.

**Summary.** Nexum's error model is **panic, not exceptions, and not return
codes**: a function that cannot continue calls `panic(msg)`, which writes the
message and a newline to stderr through the system-call layer and exits with
status 1. There is no stack unwinding in the freestanding runtime today; the
only "unwinding" is the defer chain, which `nexum_panic` must run before it
exits. A recoverable failure is expressed by the language's existing panic
path, and the recommendation is to add a single `Result`-shaped sum type
later, only after the defer chain and the arena release path are in place.
The allocator and the GC turn a failed mapping into `nexum_oom()` and then
into the same panic. Compiler errors are a separate channel: they are produced
by the Rust toolchain before the program ever runs.

## Recommendation

1. **Panic stays the terminal primitive.** `nexum_panic(msg)` is the one way a
   program or the runtime says "this cannot continue". It is defined at
   `crates/nexum-runtime/src/lib.rs:131-142` (POSIX branch) and
   `:132-135` (Windows branch), writes to stderr through the system-call layer
   on POSIX (`nexum_sys_write` to descriptor 2) and through `fputs`/`fputc` on
   Windows, then exits with status 1. **Rationale:** it needs no unwinding
   machinery, no exception tables, and no C library; it is the smallest thing
   that works under the freestanding constraint.

2. **No stack unwinding today.** The runtime has no exception frames, no
   landing pads, and no personality function. A panic is a process exit.
   **Rationale:** unwinding through LLVM IR would require C++-style
   `__cxa_throw` machinery or our own, which is exactly the C-library
   dependency the programme removes. The defer chain (see point 5) is the
   only cleanup that runs.

3. **Recoverable failures go through `panic` plus `defer`.** A function that
   wants to report a recoverable condition panics with a message; callers that
   need to clean up do it in `defer` blocks, which run on the panic path.
   **Rationale:** this matches the existing `nexum_panic` contract and the
   defer-stack design already pinned in `specs/research/memory-layout.md` §6.

4. **OOM is a panic, not an error value.** `nexum_oom()` at
   `crates/nexum-runtime/src/lib.rs:144-147` calls `nexum_panic(ptr
   @nexum_msg_oom)` and is `unreachable` afterwards. The allocator returns
   null on a failed mapping and `nexum_alloc_scanned` turns null into
   `nexum_oom()` (`crates/nexum-runtime/src/lib.rs:162-164`). **Rationale:**
   a freestanding runtime has no way to report a failed allocation back to a
   program that has no `Result` type, and an OOM that is not fatal would
   leave the program in an undefined state.

5. **The defer chain runs on panic.** `nexum_panic` must call
   `nexum_defer_run_all()` before it exits, so that arenas are released and
   handles are closed even on the fatal path. The defer stack is defined at
   `specs/research/memory-layout.md:1084-1090` and is the single cleanup
   mechanism. **Rationale:** D009 requires release on early returns and on
   panics; without this, a panic leaks every arena and every open file.

6. **`Option`/`Result` are not added in this slice.** The language has
   neither today (see `crates/nexum-sema/src/lib.rs:15` and the `Ty` enum).
   A `Result`-shaped sum type is a language change that needs a grammar entry,
   a decision entry, and a lowering; it is out of scope for the error *model*
   and is listed as an open question. **Rationale:** the model can be fully
   specified as "panic + defer + typed messages" without committing to a
   surface syntax.

7. **Compiler errors stay in the toolchain.** The Rust compiler reports
   errors before the program runs, through `nexum_diagnostics`
   (`crates/nexum-diagnostics/src/lib.rs:44-75`). They are not runtime errors
   and never reach `nexum_panic`. **Rationale:** keeping the two channels
   separate is what lets a program be correct-by-construction with respect to
   its own error paths.

## What the code looks like today

### The panic primitive

- `crates/nexum-runtime/src/lib.rs:114-142` — the two branches of
  `nexum_panic`. On Windows (`msvc`): `__acrt_iob_func(2)` to get stderr,
  `fputs`, `fputc(10)` for the newline, `exit(1)`. On POSIX: `nexum_strlen`
  of the message, `nexum_sys_write(2, msg, len)`, `nexum_sys_write(2, nl, 1)`,
  `nexum_sys_exit(1)`. Both end in `unreachable`.
- `crates/nexum-runtime/src/lib.rs:125` — `@nexum_msg_oom` is the constant
  `"out of memory\0"` (14 bytes); `:126-127` — `@nexum_msg_oob` is
  `"index out of bounds\0"` (20 bytes).
- `crates/nexum-runtime/src/lib.rs:144-147` — `nexum_oom()` calls
  `nexum_panic(ptr @nexum_msg_oom)` and is `unreachable`.

### Call sites of the panic path in the runtime

- `crates/nexum-runtime/src/lib.rs:162-164` — `nexum_alloc_scanned`: on a
  null return from `nexum_heap_alloc`, calls `nexum_oom()`.
- `crates/nexum-runtime/src/lib.rs:212-213` — `nexum_str_char_at`: on a
  negative or out-of-range index, calls `nexum_panic(ptr @nexum_msg_oob)`.
- `crates/nexum-runtime/src/lib.rs:217-218` — `nexum_slice`: on a bad range,
  the same panic.
- `crates/nexum-runtime/src/lib.rs:243-244` — `nexum_list_get`: on a bad
  index, the same panic.
- `crates/nexum-runtime/src/lib.rs:246-247` — `nexum_list_set`: on a bad
  index, the same panic.
- `crates/nexum-runtime/src/strings.rs:40` and `:44` — `nexum_read_file` and
  `nexum_write_file`: on a failed `open`, call `nexum_panic(ptr
  @nexum_msg_notfound)`.
- `crates/nexum-runtime/src/bytes.rs:56` — `nexum_byte_value`: on a bad
  index, `nexum_panic(ptr @nexum_msg_oob)`.
- `crates/nexum-runtime/src/bytes.rs:63` — `nexum_byte_from_value`: on a value
  outside 0..=255, `nexum_panic(ptr @nexum_msg_byte_range)`.

### Call sites in the code generator

- `crates/nexum-codegen/src/lib.rs:587`, `:1073`, `:1146`, `:1198` —
  the code generator emits `call void @nexum_panic(ptr @...)` for the checked
  arithmetic overflow path (`MIN / -1` handling and `sadd.with.overflow` and
  friends), with the message selected per site.

### The allocator's OOM path

- `crates/nexum-runtime/src/lib.rs:162-164` — the null check lives in
  `nexum_alloc_scanned`, so every allocator failure, including a failed
  `mmap` in `nexum_heap_alloc`, ends in `nexum_oom()`.
- `crates/nexum-runtime/src/heap.rs` (allocator IR) — `nexum_heap_alloc`
  returns null on a failed mapping; the allocator design
  (`specs/research/allocator-design.md` §9) recommends a collect-and-retry
  before declaring OOM.

### The compiler's own error channel

- `crates/nexum-diagnostics/src/lib.rs:44-75` — `render`: the diagnostic
  renderer, used by the Rust toolchain before the program runs.
- `crates/nexum-sema/src/lib.rs:15` — `Ty` has no `Option`/`Result` variant;
  errors are reported as diagnostics, not values.
- `crates/nexum-driver/tests/fixtures/run/panic.nx` — the only end-to-end
  panic fixture: a program that calls `panic("boom")` and is expected to
  exit 1.

## The design

### The shape of an error

There is no error type in the language today. The only error-shaped value is
a **message string** passed to `panic`. The runtime defines two message
constants (`@nexum_msg_oom`, `@nexum_msg_oob`) and one per-site message for
file-not-found (`@nexum_msg_notfound`); the rest are inline string globals
emitted by the code generator for checked-arithmetic failures.

When a `Result`-shaped type is added, it should be a sum type with two
variants — an `Ok(T)` carrying a value and an `Err(E)` carrying an error
value — and `E` should be a record (or an enum) defined by the standard
library, not a builtin. The lowering is the same as for any other record:
a tag word plus the payload, on the managed heap, scanned by the collector
only if `E` contains references.

### How a function reports failure

Today, the only way a function reports failure is to call `panic(msg)` and
not return. The call sites above are the complete set. There is no
`throw`, no `raise`, no `try`/`catch`, and no return-code convention in the
language or the runtime. A function that wants to signal a recoverable
condition has to do it out of band (for example, by returning a sentinel
value), and the standard library does not yet define such a convention.

### How the runtime panics

1. The program (or the runtime itself) calls `nexum_panic(ptr %msg)`.
2. The function writes the message and a single newline to descriptor 2.
   On POSIX this goes through `nexum_sys_write` (`crates/nexum-runtime/src/lib.rs:137-141`);
   on Windows through `fputs`/`fputc` (`:132-135`).
3. The function calls the system exit (POSIX) or `exit` (Windows) with
   status 1.
4. `unreachable` follows, so the IR has no fall-through.

The defer chain runs before the exit (see below). There is no unwinding of
stack frames and no destruction of locals other than what `defer` covers.

### How the stack is unwound (or not)

It is not. The runtime has no exception frames, no landing pads, and no
personality function. The only cleanup on a panic is the defer chain:

- `nexum_defer_run_all()` runs every pending deferred call, LIFO, including
  arena releases (`specs/research/memory-layout.md` §6).
- The root frame is *not* popped before exit; the process is terminating.
- No managed object is freed deterministically on panic; the GC does not run.

This is a deliberate choice under the no-exceptions constraint: unwinding
through LLVM IR would require machinery that the freestanding runtime does
not have, and the defer chain covers the cases that matter (arena release,
file close, handle release).

### How this interacts with the no-C-library constraint

- On POSIX, `nexum_panic` uses `nexum_sys_write` and `nexum_sys_exit`
  (`crates/nexum-runtime/src/lib.rs:137-141`), which are the system-call
  layer, not libc. The freestanding gate
  (`crates/nexum-driver/tests/freestanding.rs`) pins the absence of external
  symbols.
- On Windows, `nexum_panic` still uses the C library (`__acrt_iob_func`,
  `fputs`, `fputc`, `exit`) because Windows has no direct system-call
  instruction; that is the phase 11a.2 work.
- The OOM path (`nexum_oom`) is identical on both platforms: it calls
  `nexum_panic`, so it inherits whatever exit mechanism the platform uses.

### Interaction with the GC

- The GC never reports an error to the program. A failed allocation becomes
  `nexum_oom()` and then a panic.
- The GC does not run on panic. A program that panics leaks its managed heap;
  that is acceptable because the process is exiting.
- The recommended collect-and-retry before OOM
  (`specs/research/allocator-design.md` §9) would run one collection and
  retry the allocation; if it still fails, the panic path is taken.

## Risks

- **No recovery.** A panic is a process exit. A program that wants to
  recover from a failure has no mechanism today; it must either not fail or
  accept termination. This is the main limitation of the model.
- **The defer chain must run on panic.** If `nexum_panic` is changed to exit
  without calling `nexum_defer_run_all()`, arenas and handles leak on the
  fatal path and D009's "release on panic" guarantee is broken.
- **Windows still depends on the C library for the panic path.** Until phase
  11a.2, a Windows panic goes through `fputs`/`fputc`/`exit`, so the
  freestanding property does not hold there.
- **Message strings are not structured.** There is no error code, no
  location, and no stack trace. A `Result` type and a structured error record
  are the eventual answer, but they require language changes.
- **Checked arithmetic panics are not catchable.** The code generator's
  overflow paths (`crates/nexum-codegen/src/lib.rs:587`, `:1073`, `:1146`,
  `:1198`) call `nexum_panic` directly, so an overflow terminates the
  program. There is no way to ask for wrapping or saturating arithmetic at
  the call site yet (that is phase 11c).

## Open questions

1. **Should `Result` be a language builtin or a library type?** The language
   has no sum types today (`Ty` at `crates/nexum-sema/src/lib.rs:15` has
   `Record` and `Array` but no enum). *Recommendation:* a library record
   with a tag field, until the language gains a real sum type; then migrate.
2. **Should `panic` carry a code as well as a message?** Today it takes a
   single `ptr` (the message). *Recommendation:* keep the single message for
   now; a code is a structured-error concern and belongs with `Result`.
3. **Should the GC run on panic?** Today it does not. *Recommendation:* no;
   the process is exiting and the cost is not justified.
4. **Should `nexum_panic` on POSIX use `exit_group` instead of `exit`?**
   `exit` (60) is the thread exit; the process exit needs `exit_group`
   (231), which is not in the syscall table
   (`crates/nexum-runtime/src/syscalls.rs:60-72`). *Recommendation:* yes,
   this is a correctness fix and belongs with the syscall-layer work.
5. **What does the defer chain do on a panic raised inside a thread?**
   `specs/research/concurrency.md:509` notes that a panic inside a thread is
   the exception to the normal rule. *Recommendation:* the thread's defer
   chain runs, then the thread exits with a non-zero status; the process
   continues unless the panic is in the main thread.
6. **Should checked-arithmetic failures be recoverable?** Today they call
   `nexum_panic` and terminate. *Recommendation:* no for this phase; phase
   11c adds wrapping/saturating variants that do not panic, and the checked
   variants stay fatal.