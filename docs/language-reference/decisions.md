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
* Consequences: no circular-module concerns; `nx` is single-file in and single
  binary out. A later phase introduces explicit import syntax and this document
  will record it.

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

## D010 — `Str` is the only managed type; GC is precise and non-moving (decided)

The bootstrap has exactly one managed type, `Str`. Strings produced by
`concat` live on the managed heap and are collected by a precise,
non-moving, single-threaded mark-sweep collector (`nexum_alloc`,
`nexum_collect`, `nexum_gc_maybe`). Native values (Int, Float, Bool,
records, arrays) are unboxed and never allocate.

* Roots: compiler-generated root frames holding the *addresses* of `Str`
  alloca slots, so the collector reads slot contents at collection time.
* Safepoints: every allocation and the `collect()` builtin.
* Temporary roots: a `Str`-returning call spills its result into a registered
  slot immediately, so nested constructions stay sound.
* Restriction: `Str` may not be stored in record fields or array elements
  yet (T039), because the collector has no type descriptors; this is
  enforced statically and recorded here, not left implicit.
* Open: type descriptors and in-aggregate strings, arenas, deterministic
  cleanup, concurrency, and a moving/compacting collector are later phases.
## D008 — For-each iterates a snapshot (decided)

`for v in xs:` evaluates `xs` once, then binds `v` (immutable, by value) to
each element in order. Mutating `xs` inside the body does not affect the
iteration.

* Rationale: with value semantics, iterating a snapshot is the only rule that
  is invariant under whether `xs` is a variable, a parameter or an expression.
* Open: a `mut` iteration form (iterating in place over elements of a mutable
  binding) can be added later without changing this rule's meaning.
