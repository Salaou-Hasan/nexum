# Nexum Bootstrap — Language Decision Record

Status: **living document**. Each decision below is normative for the bootstrap
compiler (`nx`) as of the Phase 3 milestone. "Decided" means the compiler
implements it and conformance fixtures pin it; "provisional" means the rule
holds but a later phase is expected to revise it; "open" means the compiler
deliberately has not chosen a rule yet.

## D001 — `mut` parameters are by-reference borrows (decided)

A parameter declared `mut` **borrows the caller's binding**: assignments inside
the callee are visible to the caller after the call. A plain parameter is an
immutable copy and cannot be assigned (T010).

* Rationale: the architecture spec's data-oriented illustration
  (`fn integrate(mut ps, dt)`) mutates an array of records in place; a
  copy-in parameter would silently discard the update.
* Consequences: the argument at a call site must be a mutable variable (T037).
  Field or element arguments (`f(a.b)`) are rejected — a borrow always targets
  a whole binding. Borrowing is a compile-time notion; it lowers to passing
  the argument's address.
* Open: aliasing (two `mut` parameters bound to the same caller binding) is not
  diagnosed; there is no `&mut`-style borrow syntax.

## D002 — One source file is one module (decided)

A `.nx` file compiles as a single module. There is no `import`, no package
namespace and no cross-file linking yet; the file-path comments in the spec's
examples are explanatory labels, not import syntax.

* Rationale: the spec explicitly defers a module system; defining imports
  before name resolution across files is specified would freeze the wrong
  shape.
* Consequences: `import "path.nx"` (top level only) loads another file into
  the same program, resolved relative to the importing file. Every file is
  loaded once, so diamond imports and import cycles are safe. Imported
  declarations share one namespace with the importing file; there is no
  per-module visibility yet, and no separate compilation. A later phase
  introduces qualified names and this document will record it.

## D003 — Error handling is `panic` plus static diagnostics (decided)

Dynamic failure is `panic(msg)`: the message plus newline go to **stderr** and
the process exits with code 1. `panic` has type `Never`, which converts to any
type, so it is usable in the tail position of value-returning functions.
Statements after a `panic` in the same block are unreachable and are not
emitted.

There is no exception mechanism, no recoverable-error type and no
`Result`-like value today. All other errors are compile-time diagnostics with
stable codes and source spans.

* Rationale: the spec's Phase 3 asks for "error handling" as a definitional
  item, not a full exception system; a defined, documented failure mode avoids
  ambiguous implicit behavior.
* Open: whether recoverable errors become a library type (`Result`-like) or a
  language feature is deferred to a later phase.

## D004 — Numeric literals and conversions (decided, provisional)

`0`, `42` are `Int` (i64). `0.5`, `1.0` are `Float` (f64); a fractional digit
after the decimal point is required. Mixing `Int` and `Float` operands is a
compile-time error (T008/T017); there is no implicit conversion. Write
`1.0 * n` (or later, an explicit conversion function) to widen.

* Rationale: the illustration `total += 0.5 * (...)` must accumulate in Float
  while `0` stays Int; implicit widening hides type errors.
* Consequences: `Int` arithmetic is **wrapping** (two's complement, no
  overflow traps); integer division by zero is a runtime `panic("division by
  zero")`; `MIN / -1` wraps to `MIN` and `MIN % -1` is `0`. `Float` follows
  IEEE 754 (division by `0.0` gives `inf`/`nan`, never traps).
* Open: exact Float printing (`%g` today) and explicit conversion intrinsics
  are pinned with the runtime ABI in Phase 4. Checked (trapping) arithmetic
  can be added later as an opt-in profile without changing these rules.

## D005 — Arrays are fixed-size value types (decided)

`[T; N]` (N ≥ 1) is a fixed-size array of `T`. Arrays are **value types**:
`let ys = xs` copies, function arguments are copies, and element assignment
`xs[i] = v` rewrites the aggregate stored in `xs`. `len(xs)` is a compile-time
constant. A constant index outside bounds is a compile-time error (T033); a
dynamic index is checked at run time and calls `panic("index out of bounds")`
on failure.

* Rationale: value semantics keeps aliasing out of the bootstrap language
  (only `mut` parameters alias), and mirrors records. Fixed size keeps layout
  static and ABI simple.
* Open: slices/views, growable lists and bounds-check elimination are later
  phases. Static relations between collection lengths (for elementwise
  operations over two arrays) are not expressed; each index is bounds-checked
  individually.

## D006 — Block scoping and shadowing (decided)

`let` bindings are scoped to their block. `if`/`while`/`for` bodies are nested
scopes; bindings introduced inside them (including the loop variable) do not
leak out. A nested `let` may shadow an outer binding without affecting the
outer binding.

* Rationale: unambiguous name resolution across control flow; required for
  for-each desugaring to be correct.

## D007 — Compound assignment evaluates the target once (decided)

`x op= e` is exactly `x = x op e` with a single evaluation of the target path:
for `xs[i] op= e`, `i` is evaluated once and shared by the read and the write.
Operand typing follows the plain binary operator rules.

* Rationale: naive textual desugaring double-evaluates index expressions with
  side effects (e.g. `xs[next()] += 1`), which is observable and surprising.

## D009 — Arenas and deterministic cleanup are specified but staged (decided)

Arenas (bulk-lifetime regions) and compiler-supported deterministic cleanup
are **specified** by the runtime ABI (see `runtime-abi.md` §4–§5) but not
yet **implemented**: there is no scoped language surface for them yet, and
implementing them before the collector handshake is proven would freeze the
wrong shape.

* Intended model: an arena is a bump-allocated region with a single release
  point; references must not escape the region; release is LIFO over nested
  scopes and runs on early returns and panics.
* Intended cleanup order: LIFO within a scope, including on early returns.
* Consequences: until these land, programs use the managed heap (`concat`,
  `collect`) for dynamic strings. The decision is recorded here so it is not
  a silent skip.

## D010 - `Str` and `List` are the managed types; tracing is mostly precise (decided)

The bootstrap has exactly two managed types, `Str` and `List`. Strings
produced by `concat` and lists produced by `list_new` live on the managed
heap and are collected by a non-moving, single-threaded mark-sweep collector
(`nexum_alloc`, `nexum_collect`, `nexum_gc_maybe`). Native values (Int,
Float, Bool, records, arrays) are unboxed and never allocate.

* Roots: compiler-generated root frames. Each root entry names an address
  and the number of bytes the collector must scan there, so the collector
  reads slot contents at collection time.
* Tracing is *mostly precise*: the compiler knows exactly which slots hold
  references and how large they are; the contents of an aggregate are
  scanned conservatively. See `decisions-gc.md` for the alternatives that
  were considered and why this one was chosen.
* Safepoints: every allocation and the `collect()` builtin.
* Temporary roots: a call whose result holds references spills its result
  into a registered slot immediately, so nested constructions stay sound.
* `Str` may be stored in record fields and array elements: the collector
  traces references inside aggregates. The former T039 restriction is
  therefore withdrawn.
* Open: arenas, deterministic cleanup, concurrency, and a moving/compacting
  collector are later phases.

## D011 - `List` is a monomorphic growable collection (decided)

`List` is the dynamic collection type. It is deliberately separate from the
fixed-size array type, whose value semantics are unchanged.

* The element type is fixed by the first `list_push`; a later push of a
  different type is a compile error (T049), and a list whose element type
  was never established cannot be read (T049).
* A list is a reference: two names may refer to the same list.
* Growth doubles the capacity; the list object itself never moves, so
  handles stay valid across growth.
* The element buffer is a separate heap object, which lets the collector
  trace the elements it holds.
* Open: lists of lists, slicing, iteration helpers, and a generalisation
  to other element types.
## D008 — For-each iterates a snapshot (decided)

`for v in xs:` evaluates `xs` once, then binds `v` (immutable, by value) to
each element in order. Mutating `xs` inside the body does not affect the
iteration.

* Rationale: with value semantics, iterating a snapshot is the only rule that
  is invariant under whether `xs` is a variable, a parameter or an expression.
* Open: a `mut` iteration form (iterating in place over elements of a mutable
  binding) can be added later without changing this rule's meaning.

## D012 - The runtime is freestanding: no libc (decided)

A program built by Nexum depends on the kernel and on nothing else. The
runtime does not link the C library: the string routines, the file routines
and the system call layer are part of the runtime.

* Rationale: "full control over every single thing in the hardware" is not
  reachable while the allocator, the string handling and the system calls
  are somebody else's. It is also the shortest path to the work Nexum is
  aimed at now: the systems work and the model framework both need an
  allocator and a math library that are ours.
* Status: the POSIX targets are freestanding for the file routines and the
  string routines; the allocator and the output formatting still use the C
  library, and are phase 11a.2. The Windows build still uses the C library
  throughout, because Windows has no direct system call instruction: its
  kernel interface lives behind `ntdll`, and generating our own stubs for it
  is phase 11a.2 as well.
* Consequences: the toolchain may still use Rust and LLVM. That is the
  compiler's business, not the language's.

## D013 - The operating system is reached through inline assembly (decided)

The system call layer is written as inline assembly in the generated LLVM
IR: the number goes in the register the ABI uses, the arguments are pinned
to the registers the instruction reads them from, and the result comes back
in the register the ABI returns it in.

* Rationale: a C binding would be a C dependency, and the assembly is
  where the control actually is - the register allocation, the calling
  convention and the error convention are all visible in it.
* Status: implemented for Linux and macOS on x86-64 and AArch64.

## D014 - Inline assembly is a first-class language feature (decided, staged)

A Nexum program can write assembly inline, so that it can reach what the
language does not model: device registers, privileged instructions, the
system call interface itself.

* Rationale: the runtime needs it, and if the runtime needs it, the
  language needs it - otherwise the parts of the machine the language cannot
  reach are exactly the parts a systems program needs.
* Status: the language surface is phase 11b. The runtime already uses the
  mechanism it will be built on.

## D015 - The numeric tower is f32 and f64 (decided, staged)

`Float` is `f64` today. The tower grows to `f32` and `f64` with the
arithmetic semantics spelled out per operation, and the math builtins
implemented by us rather than taken from `libm`.

* Rationale: a model framework needs `f32` for the memory traffic and the
  vector width, and it needs the math functions to be ours so that the
  results do not depend on the C library's version.
* Status: phase 11c.

## D016 - The program is linked by the Nexum toolchain (decided, staged)

The generated program is linked by the Nexum toolchain, not by `clang`.
The compiler may still be written in Rust and use LLVM's libraries; that is
the toolchain's business.

* Rationale: the last place a program depends on somebody else's build
  system is the link step, and it is the step that decides the ABI and the
  layout of the binary.
* Status: phase 11g. Until then the toolchain calls `clang` to link.