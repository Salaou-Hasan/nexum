# The memory and layout model (phase 11d)

**Question.** How should a Nexum program control memory - pointers, explicit
record layout, volatile accesses, unsafe regions, arenas and deterministic
cleanup - so that a program can own its memory instead of only using the
collector's heap, and what does that cost in the type system?

**Summary.** The recommendation is a *single* pointer type, `Ptr`, with an
optional pointee type (`Ptr` is a byte address, `Ptr(T)` is a typed view);
dereferencing it, doing pointer arithmetic and reinterpreting it are all
`unsafe` operations, and a pointer is **never a root**, so the collector never
keeps an object alive on a program's behalf. Unsafe is a *block* (`unsafe:`)
and a *function* (`unsafe fn`), never a type: it does not switch off the type
checker, the bounds checker or the mutability checker, it only unlocks the
handful of operations the compiler cannot check on raw memory. The record
layout is **Nexum's own**, computed by the compiler and emitted as explicit
padding so it is identical on every target, and a record may additionally
declare `layout c`, which *asserts* - and the compiler *checks* - that the
layout equals the C layout for the same field types. `volatile` is an
access-site qualifier (`volatile *reg`), for memory-mapped registers and for
memory somebody else writes. The arena is a lexical `arena a:` scope over a
chunked bump allocator whose memory is raw (never collected, freed at one
release point) but whose *contents are traced*, so a `Str` stored in an arena
stays alive; the arena handle cannot escape its scope, and the pointers into
it can only escape in the cases the compiler cannot see. Deterministic
cleanup is a `defer` statement, LIFO, running on scope exit, on early return
and on `panic`. None of this is verifiable in the type system, and the
document says exactly where the line is.

## Recommendation

1. **One pointer type: `Ptr`, with an optional pointee type.** `Ptr` is a
   byte address (the equivalent of C's `void *`). `Ptr(T)` is the same eight
   bytes with a *view*: the compiler knows the layout of `T` and computes the
   offsets of `(*p).field` and `(*p)[i]` from it. Both are ordinary value
   types: eight bytes, copied like an `Int`.
   *Reason:* the phase's central promise is that a program can control
   memory, and a layout declaration that cannot be used at the point of use
   is documentation, not control. A typed view is what makes `layout c`
   checkable and what makes `size_of`/`align_of` meaningful. One type
   constructor with an optional argument is the whole price; a general
   generics system is not needed and is not proposed.

2. **A pointer is never a root, and that is structural, not advisory.** The
   collector's root computation already asks one question - "can this type
   hold a reference?" - in `crates/nexum-codegen/src/lib.rs:23`
   (`has_refs`) and `crates/nexum-codegen/src/lib.rs:85` (`scan_bytes`). A
   `Ptr` answers "no", so it is not registered, it is not scanned, and the
   collector will not follow it. A program that wants a managed object to
   stay alive holds a **managed** reference to it (`Str`, `List`), which is a
   root; the raw pointer is only a way to *look at* the object.
   *Reason:* "a raw pointer must not be a root" is not a rule the runtime can
   enforce; it is a rule the *type system* has to express, and it does so by
   making managed references and raw pointers different types.

3. **Dereferencing is unsafe; `unsafe` is a block and a function, never a
   type.** The unsafe operations are: reading or writing through a pointer
   (`*p`, `(*p).f`, `(*p)[i]`), pointer arithmetic, reinterpreting a `Ptr` as
   a `Ptr(T)`, freeing raw memory, and calling an `unsafe fn`. The surface
   syntax is `unsafe:` (a block, like `if:`) and `unsafe fn` (a function
   whose *callers* must be in an unsafe region). There is no `unsafe type`.
   *Reason:* an unsafe region that disabled checks would be a lie about what
   the compiler knows. The honest form is the one Rust uses: the compiler
   cannot check pointer validity, alignment, initialization or lifetime *at
   all*, so those checks do not exist inside an unsafe region either - the
   region is the marker that says the programmer takes responsibility, and its
   only mechanical effect is to unlock the operations listed above.

4. **The layout is Nexum's own, computed by the compiler, and emitted as
   explicit padding.** Field offsets follow the declaration order; each
   field is placed at the next multiple of its alignment; the record's
   alignment is the maximum of its fields'; its size is rounded up to that
   alignment. The compiler emits the record as a struct *with the padding
   spelled out*, so the layout does not depend on LLVM's data layout and is
   identical on every target. A record may declare `layout c`, which asserts
   that the computed layout equals the C layout for the same field types;
   the compiler checks the assertion independently and rejects the record if
   it does not hold. `layout packed` (alignment 1) and `layout align N` are
   the two knobs, and neither is C-compatible, so `layout c` and either of
   them together is an error.
   *Reason:* the layout rules themselves belong to `record-layout.md`; what
   this document settles is the interface - the layout must be computed by
   the compiler, not inherited from the backend, because the typed pointer
   and the arena both need offsets and sizes that the compiler knows.

5. **`volatile` is an access-site qualifier.** `volatile *reg` and
   `volatile (*hdr).flags`. A volatile access is performed exactly as
   written, is not elided, and is not reordered with respect to other
   volatile accesses. It gives **no** atomicity and **no** ordering with
   respect to non-volatile accesses; that is the atomics model's job
   (`atomics-model.md`, phase 11e).
   *Reason:* the qualifier belongs to the *object being accessed* in C, and
   the access-site form says the same thing without making the pointer type
   carry a second qualifier. The collector's own writes - the mark bit and
   the next-object link - are volatile in the runtime's IR for the same
   reason: the optimizer must not assume they are dead.

6. **The arena is a lexical scope over a chunked bump allocator, and its
   memory is raw.** `arena a:` opens a scope; the arena is created on entry
   and released on exit - on the normal path, on an early `return`, and on
   `panic`. Nested arenas release inside-out. The arena's memory comes from
   the runtime allocator and is **not** a collector object: the collector
   never reclaims it, and the release point is the only thing that frees it.
   The arena's *contents* are traced: the collector walks the live arenas
   and scans the used bytes of each chunk, so a `Str` stored in arena memory
   stays alive exactly as long as the arena does.
   *Reason:* D009 says an arena is a bump-allocated region with a single
   release point, and the architecture spec puts arenas at "bulk lifetime
   for temporary batches, parsing, frames, requests or jobs". A parsing arena
   holds `Str` fields, so "the arena's memory is not collected" and "the
   references in it stay alive" must both be true; they are compatible
   because tracing and reclaiming are separate questions.

7. **The arena handle cannot escape its scope; the pointers into it can,
   only where the compiler cannot see it.** The handle is a distinct type
   (`Arena`), so "the handle must not be returned, stored into a managed
   container, or assigned to a binding outside the arena scope" is a
   *type-directed* check. A pointer produced by `arena_alloc` is an ordinary
   `Ptr`, so the same rule for it is enforced by a **provenance** check in
   the MIR: a value tainted "came from this arena" may not be returned,
   stored into a value that outlives the scope, or stored into a `List`.
   What the compiler *cannot* see is a pointer handed to another function
   that stores it somewhere the compiler cannot see. That case is a
   documented contract, not a guarantee.
   *Reason:* the compiler cannot verify non-escape in general - there is no
   borrow checker, no region inference and no lifetime system in Nexum - so
   the language must not pretend otherwise. It expresses the rule with a
   scope, a checked subset and an explicit "you are on your own" for the
   rest.

8. **Deterministic cleanup is `defer`.** `defer close(f)` evaluates the
   call's arguments where the `defer` stands and makes the call when the
   enclosing block exits, LIFO within the scope, on the normal path, on an
   early `return` and on `panic`. The arena's release is itself a `defer`,
   so there is one cleanup mechanism, not two.
   *Reason:* the runtime ABI already specifies "LIFO within a scope,
   including on early returns" (`docs/runtime-abi.md` §7) and the
   architecture spec wants deterministic cleanup for files, sockets, locks and
   handles - not only for arenas.

9. **What the type system cannot do, said out loud.** It cannot check
   pointer validity, alignment, initialization or lifetime. It cannot keep an
   object alive through a raw pointer. It cannot check that an arena pointer
   does not escape, in general. It cannot give `volatile` atomicity. It
   cannot express "this memory belongs to the device" (that is 11h). Each of
   these is expressed by the `unsafe` marker plus a written rule, never by a
   type.

10. **The order of work.** The layout engine, the pointer type, `unsafe` and
    `volatile` need nothing from phase 11a.2 and can land first; `defer`
    needs a small runtime addition and nothing else; the arena and the
    program-level `alloc`/`free` need the runtime allocator, but only its
    existing two functions - the arena knows the size of every chunk it
    frees, so the allocator's raw-allocation prologue (its open question 7)
    is *not* on the critical path.

## What the code looks like today

The facts the recommendation depends on.

### The managed heap and the collector

* `crates/nexum-runtime/src/lib.rs:147` - `nexum_alloc_scanned(payload,
  scan_bytes)`: rounds the payload up to 16, adds a 32-byte header, and calls
  `malloc`. **Both entry points return the payload pointer, not the
  header.** The header is `[0] size, [8] mark, [16] next, [24] scan_bytes`,
  and the payload starts at `+32`.
* `crates/nexum-runtime/src/lib.rs:162` - `nexum_mark_ptr`: walks the global
  object list and reports whether the candidate pointer is the payload of an
  object it has just marked. A candidate that matches nothing (a static
  literal, an integer that looks like an address) is ignored.
* `crates/nexum-runtime/src/lib.rs:168` - `nexum_scan_range(base, bytes)`:
  scans `bytes / 8` words from `base`, treating each word as a candidate
  pointer.
* `crates/nexum-runtime/src/lib.rs:174` - `nexum_collect`: walks the root
  frames, scans each entry's range, then traces the heap to a fixpoint and
  sweeps.
* `crates/nexum-runtime/src/lib.rs:186` - `nexum_sweep`: unlinks every
  unmarked object and frees it. The collector never frees an object
  individually; the sweep is the only place a block goes back.
* `crates/nexum-runtime/src/lib.rs:108` - `@nexum_threshold = 65536`: a
  collection runs inside an allocation when the watermark is exceeded, or
  when the program calls `collect()`.

The consequence for phase 11d: **the collector's only entry points are
`nexum_alloc` and `nexum_alloc_scanned`, and every object it manages has a
32-byte header.** Raw memory (an arena chunk, a `Ptr` target, a mapped
region) is not an object, is not in `@nexum_obj_head`, and is never freed by
the sweep.

### Roots

* `crates/nexum-runtime/src/lib.rs:102` - the root frame is
  `{ ptr prev, ptr entries, i64 count }` and a root entry is
  `{ ptr addr, i64 scan_bytes }`. `scan_bytes == 0` means "not a root".
* `crates/nexum-codegen/src/lib.rs:370` - the compiler builds the entry list:
  every local whose type can hold a reference is registered, with 8 bytes
  for a `Str` or a `List`, the whole size for an aggregate that contains
  references, and nothing at all for everything else.
* `crates/nexum-codegen/src/lib.rs:23` - `has_refs(t)`: the single place
  that decides "is this a reference?". `Ty::Str` and `Ty::List(_)` are
  references; records and arrays are references when one of their fields is.
* `crates/nexum-codegen/src/lib.rs:85` - `scan_bytes(t)`: the number of bytes
  the collector must scan for a type.
* `crates/nexum-codegen/src/lib.rs:435` - the frame and the entry array are
  allocated in the function's prologue; `crates/nexum-codegen/src/lib.rs:1194`
  pops the frame before every return.

The consequence for phase 11d: **"a raw pointer is not a root" is a one-line
change in `has_refs`, plus one line in `scan_bytes`.** The mechanism exists;
what is missing is a type that means "raw".

### Types

* `crates/nexum-sema/src/lib.rs:15` - `Ty`: `Int`, `Float`, `Bool`, `Str`,
  `Unit`, `Record(String)`, `Array(Box<Ty>, usize)`, `List(Box<Ty>)`,
  `Never`, `Unknown`, `Error`. `Ty::compatible` is at
  `crates/nexum-sema/src/lib.rs:54`.
* `crates/nexum-parser/src/lib.rs:13` - `TypeExpr`: `Named(String)` and
  `Array(Box<TypeExpr>, usize)`. The builtin type names (`Int`, `Str`, ...)
  are ordinary `Named` names, resolved in the semantic pass.
* `crates/nexum-lexer/src/lib.rs` - `TokenKind` holds the keywords. The
  grammar's keyword list is in `docs/language-reference/grammar.md:19`.
* `crates/nexum-codegen/src/lib.rs:8` - `llvm_ty`: the type-to-LLVM map.
  `Ty::Str` and `Ty::List(_)` are both `ptr`.

### Layout

* `crates/nexum-codegen/src/lib.rs:36` - `llvm_align`, `:55` - `llvm_size`,
  `:70` - `record_size`: the compiler already computes a natural layout,
  with the field offsets rounded up to each field's alignment.
* `crates/nexum-codegen/src/lib.rs:124` - but the record is emitted as
  `%Name = type { ... }` and **LLVM lays it out**. The computed
  `record_size` is used for the scan size and for the codegen's own
  arithmetic, not to pin the layout.
* `docs/runtime-abi.md:86` - the ABI says "field order = declaration order",
  and says nothing about padding or alignment.

The consequence for phase 11d: **the layout exists as arithmetic but not as
a promise.** A typed pointer needs the offsets to be the compiler's, not
LLVM's.

### The MIR and the escape analysis

* `crates/nexum-mir/src/lib.rs:47` - `Instr`, `:138` - `Term`. There is no
  scope construct, no cleanup construct and no pointer instruction.
* `crates/nexum-mir/src/lib.rs:1317` - `escape_analysis`: a *report* that
  says which `concat` allocations escape their function. It is conservative
  (any store counts as an escape) and it changes nothing.

The consequence for phase 11d: **the arena's escape check is the first
*enforcing* use of escape analysis in the compiler**, and the first use of
provenance.

### The decisions already taken

* `docs/language-reference/decisions.md:112` - **D009**: arenas and
  deterministic cleanup are specified but staged. "An arena is a
  bump-allocated region with a single release point; references must not
  escape the region; release is LIFO over nested scopes and runs on early
  returns and panics."
* `docs/runtime-abi.md:205` (§6) and `:219` (§7) - the same model in the ABI.
* `docs/language-reference/decisions.md:128` - **D010**: `Str` and `List` are
  the managed types; the collector is non-moving and mostly precise.
* `docs/native-roadmap.md:72` - phase 11d: "Memory and layout: pointers,
  packed records, `unsafe` regions, `volatile`, explicit arenas and
  deterministic cleanup (finishing D009)."
* `docs/native-roadmap.md:45-56` - the audit that says what is missing: no
  pointer type, no volatile, no memory control, no unsafe.

### The allocator phase 11a.2

The allocator research (`specs/research/allocator-design.md`, in flight)
pins two functions, `nexum_heap_alloc(total) -> ptr` and
`nexum_heap_free(ptr, total)`, and notes in its open question 7 that a raw
allocation entry point should be added when a second kind of caller exists -
"phase 11d (arenas, explicit memory)" is the caller it names. Its open
question 6 notes that the allocator is currently coupled to the collector's
32-byte header, and that a prologue (the size at -16, the link at -8) would
be needed for a caller that has no header.

**The recommendation below does not need that prologue**: the arena tracks
the size of every chunk it frees, and the language-level `free` takes the
size explicitly. The allocator's raw entry point is therefore *not* on the
critical path for 11d.

## The design

### 1. The pointer type

#### Syntax

```
type-expr = ... | "Ptr" [ "(" type-expr ")" ]
```

`Ptr` is a builtin type name, like `Int` and `Str`, and is reserved.
`Ptr(T)` is the typed form. The two forms differ in what the *compiler*
knows, not in what the value is: both are eight bytes, both are copied like
an `Int`.

```
type Header:
    magic: Int
    count: Int
    flags: Bool

fn poke(p: Ptr(Header), v: Int):
    unsafe:
        (*p).count = v
```

* `Ptr` (no argument) is a byte address. The only things that can be done
  with it are the raw intrinsics listed below.
* `Ptr(T)` is the same address with a *view* of `T`. The compiler computes
  the offsets of `T`'s fields and elements and the size and alignment of
  `T` from the layout rules in §4.

The grammar does not use `<...>` for the type argument. `<` and `>` are
comparison operators in this language, and `Ptr<Ptr<Int>>` is a shift
ambiguity waiting to happen; `(...)` is already the shape used for array
lengths (`[T; N]`) and for calls, and it nests without a rule.

#### The operations

The place expressions are the existing ones. The grammar's lvalue is
`identifier { "." identifier | "[" expr "]" }`
(`docs/language-reference/grammar.md:71`); the pointer form adds a base that
is a dereference:

```
lvalue = [ "volatile" ] ( identifier { "." identifier | "[" expr "]" }
                         | "*" postfix )
```

and the expression form adds a unary `*`:

```
unary-expr = [ "-" | "not" | "*" ] postfix
```

`postfix` binds tighter than the prefix, so `*p.f` is `*(p.f)` and
`(*p).f` is the field of the pointee - the C convention, and the one that
keeps a single rule for every place expression.

| Operation | Form | Unsafe | Meaning |
| --------- | ---- | ------ | ------- |
| load the pointee | `*p` | yes | the value at the address |
| store the pointee | `*p = v` | yes | write the value at the address |
| load a field | `(*p).f` | yes | the field at its computed offset |
| store a field | `(*p).f = v` | yes | write the field at its computed offset |
| index | `(*p)[i]` | yes | the element at `i * size_of(T)` (bounds-checked when `T` is an array) |
| byte offset | `ptr_add(p, n)` | yes | `p + n` bytes; the result is a `Ptr` |
| element offset | `ptr_offset(p, n)` | yes | `p + n * size_of(T)`; the result is a `Ptr(T)` |
| reinterpret | `let q: Ptr(T) = p` | yes | the same address, seen as `T` |
| widen | `let q: Ptr = p` | no | the same address, seen as bytes |

The reinterpretation is written as a **type annotation on a `let`**, not as
a cast function. It has to be written down somewhere, and the annotation is
the place where the programmer states the pointee type; the runtime cost is
zero (both are pointers) and the only thing that changes is how the compiler
computes the offsets. A `Ptr` may be widened to a `Ptr(T)` anywhere; a
`Ptr` may be narrowed to a `Ptr(T)` only inside an unsafe region, and only
because the compiler cannot check the alignment or the validity.

Storing through a pointer does **not** require the pointer's own binding to
be `mut`: the binding is not reassigned, the memory is. That is a deliberate
departure from the language's "assignment to an immutable binding is an
error" rule, and it applies only to the pointer's binding - assigning to the
pointer itself still needs `mut`.

#### The raw intrinsics

A `Ptr` - the untyped form - is accessed through explicit intrinsics, one
pair per scalar type. The set follows the numeric tower (D015, phase 11c),
so it is exactly the tower's scalar types:

| Builtin | Signature | Meaning |
| ------- | --------- | ------- |
| `ptr_load_u8` | `(p: Ptr, off: Int) -> Int` | one unsigned byte at `p + off` |
| `ptr_load_i8` | `(p: Ptr, off: Int) -> Int` | one signed byte |
| `ptr_load_u16` / `ptr_load_i16` | `(p: Ptr, off: Int) -> Int` | two bytes |
| `ptr_load_u32` / `ptr_load_i32` | `(p: Ptr, off: Int) -> Int` | four bytes |
| `ptr_load_u64` / `ptr_load_i64` | `(p: Ptr, off: Int) -> Int` | eight bytes |
| `ptr_load_f32` | `(p: Ptr, off: Int) -> Float` | an `f32` |
| `ptr_load_f64` | `(p: Ptr, off: Int) -> Float` | an `f64` |
| `ptr_store_*` | `(p: Ptr, off: Int, v: T) -> Unit` | the matching store |

They are all unsafe, and they are all byte-addressed: the offset is in
bytes, not in elements, and it is not checked against any allocation. The
typed pointer (`Ptr(I32)`, `Ptr(Float)`) is the checked-width way to reach
the same bytes, and the two must agree - a `Ptr(I32)` over a 32-bit
register is the same access as `ptr_load_i32` with offset 0.

There is no `ptr_load_bool`: a boolean in memory is one byte, and
`ptr_load_u8` followed by a comparison is the honest spelling.

#### What a pointer is *not*

* It is not a reference. `Str` and `List` are references: the collector
  traces them, and a local of those types is registered as a root.
  `Ptr` is not.
* It is not a `Str`. There is no implicit conversion in either direction.
  `ptr_of(s)` converts a `Str` into a `Ptr` explicitly and unsafely, and
  says "I know the string is kept alive by something else".
* It does not keep anything alive. See §7.

### 2. Unsafe regions

#### Syntax

```
unsafe:
    let p = arena_alloc(a, 64)
    *ptr_add(p, 8) = 7

unsafe fn poke(p: Ptr(Header), v: Int):
    (*p).count = v
```

* `unsafe:` is a **block statement**, at the same level as `if:`, `while:`
  and `for:`. It introduces a scope; the bindings inside it are scoped to it
  like any other block.
* `unsafe fn` is a **function** whose body is implicitly an unsafe region
  and whose *callers* must be inside one. This is how a low-level library
  exposes an operation it cannot make safe: the boundary is the call, not
  the file.
* There is **no unsafe type**. A type cannot be "unsafe" in a way the
  compiler can check; the risk lives in the operations, and the operations
  are what the region unlocks.

#### What the compiler checks inside a region

Nothing is switched off. Inside an unsafe region the compiler still:

* type-checks every expression,
* bounds-checks every array index,
* enforces mutability (`T010`) and binding rules,
* computes the layout and the offsets,
* registers roots exactly as it does outside.

The region changes exactly one thing: it makes the unsafe operations of §1
legal. Everything the compiler *cannot* check on raw memory - the
address is valid, the address is aligned, the memory is initialized, the
memory is still alive, nothing else writes it - is not checked inside the
region either. The region says "the programmer is responsible", not "the
compiler stopped looking".

That is the whole point of making it a *marker* rather than a mode. A
region that disabled the bounds checker would let a program corrupt memory
through an ordinary array; a region that only unlocks pointer operations
cannot.

#### The rules the compiler enforces

| Rule | Diagnostic |
| ---- | ---------- |
| a pointer is dereferenced outside an unsafe region | T050 |
| pointer arithmetic outside an unsafe region | T051 |
| a `Ptr` is reinterpreted as a `Ptr(T)` outside an unsafe region | T052 |
| an `unsafe fn` is called outside an unsafe region | T053 |
| raw memory is freed outside an unsafe region | T054 |
| an arena handle escapes its scope | T055 |
| an arena pointer escapes its scope | T056 |
| a record declared `layout c` is not C-compatible | T057 |
| two conflicting layout attributes | T058 |
| `defer` of something that is not a call | T059 |
| `volatile` applied to something that is not a place | T060 |
| a pointer reinterpretation between incompatible pointees | T061 |

### 3. `volatile`

```
let v = volatile *reg
volatile *reg = 5
let f = volatile (*hdr).flags
```

`volatile` is a **prefix on a place expression**, in both a read and a write
position. It qualifies the *access*, not the type - which is the difference
from C, where the qualifier is on the pointee and the pointer's own type
carries it around.

What a volatile access guarantees:

* it is performed, exactly as written, even if the result is unused;
* it is not moved, merged or reordered with respect to other volatile
  accesses;
* it is not cached in a register across another volatile access to the same
  location.

What it does **not** guarantee:

* atomicity - a 32-bit access to a device register may be torn;
* any ordering with respect to non-volatile accesses;
* any ordering with respect to another thread. That is the atomics model
  (`atomics-model.md`, phase 11e), and the architecture spec is explicit
  that "volatile is not a substitute for atomic operations, thread
  synchronization or a memory fence".

Where it matters in Nexum:

1. **Memory-mapped registers.** A device register read is an ordinary load
   from an address that is not the program's memory. The compiler may not
   cache it, may not fold two reads of it into one, and may not move it
   across an unrelated store. Without `volatile`, a read of a "status"
   register in a polling loop is a legal target for the optimizer to hoist
   out of the loop.
2. **Memory somebody else writes.** A device that fills a buffer, or a
   second core, is not visible to the abstract machine unless the accesses
   are volatile (or atomic, which is stronger).
3. **The collector's own writes.** The collector writes the mark bit at
   `+8` and the next-object link at `+16` of a block. Those writes are
   volatile in the runtime's IR: the sweep and the trace must observe them
   exactly, and the optimizer must not assume that a mark bit that was
   written is dead. (The slot at `+16` is the one phase 11a.2's allocator
   reuses as its free-list link, so it is written twice over the life of a
   block - another reason not to let the optimizer fold it.) The same
   applies to the collector reading `+16` while it walks the list. This is a
   runtime-internal use, but it is the same concept, and it belongs in the
   ABI document next to the object layout.

The collector does not know about volatile: a volatile store into a managed
object's payload is an ordinary write for the collector's purposes, and the
payload is still traced the same way. Volatile changes the optimizer's
behaviour, not the collector's view.

### 4. Explicit layout

#### The rules

The layout is computed by the compiler and is **the language's**, not the
backend's.

```
type Header:
    magic: Int
    count: Int
    flags: Bool
```

* Fields are placed in **declaration order**.
* A field is placed at the next offset that is a multiple of its
  **alignment**.
* A type's alignment and size:

  | Type | Alignment | Size |
  | ---- | --------- | ---- |
  | `Bool` | 1 | 1 |
  | `Int` | 8 | 8 |
  | `Float` | 8 | 8 |
  | `Str` | 8 | 8 |
  | `Ptr` | 8 | 8 |
  | `Unit` | 1 | 0 |
  | `[T; N]` | alignment of `T` | `N * size_of(T)` |
  | a record | the maximum of its fields' | the total, rounded up to the alignment |

* A record's size is the end of its last field, rounded up to the record's
  alignment.
* A record's alignment is at least 1.

The compiler **emits the padding explicitly** - a record is emitted as a
struct with `[N x i8]` padding fields between the real ones - so the layout
is the same on every target and does not depend on LLVM's data layout
string. Today the record is emitted as `%Name = type { i64, i64, i1 }`
(`crates/nexum-codegen/src/lib.rs:124`) and LLVM lays it out; that has to
change for the layout to be a promise rather than an accident.

#### The attributes

```
type Header:
    layout c
    magic: Int
    count: Int

type Packed:
    layout packed
    a: Int
    b: Bool

type CacheLine:
    layout align 64
    counter: Int
```

* **no attribute** - the rules above.
* **`layout c`** - the record's layout must equal the C layout for the same
  field types. The compiler computes the C layout independently (C's
  alignment rules, `Bool` as a one-byte `_Bool`, `Int` as an eight-byte
  `int64_t`, `Str` and `Ptr` as pointers) and compares. If they differ, the
  record is rejected (T057). For the current type set they never differ, so
  `layout c` is mostly a *promise* - and it becomes a real check the moment
  a record is `packed`, is given a larger alignment, or contains a type with
  no C counterpart.
* **`layout packed`** - the record's alignment is 1 and the fields are
  placed with no padding. A packed record is not C-compatible, so
  `layout c` and `layout packed` together is an error.
* **`layout align N`** - the record's alignment is `N` (a power of two) and
  its size is rounded up to `N`. Also not C-compatible.

The architecture spec asks for "explicit layout attributes only where a
stable meaning can be specified (for example, C-compatible layout, packed
layout or requested alignment)" - those three are exactly the three above.

#### What the layout is *not*

* The size is **computed, not declared**. A record that declared its own
  size would have two sources of truth; `size_of(Header)` and the layout
  report are the answers.
* `layout c` does not mean "what some C compiler does". It means "the
  layout the C ABI specifies for these types on this target". A C compiler
  with `#pragma pack` or bitfields is outside the promise.
* There is no explicit padding field in this phase. A protocol struct that
  needs a four-byte hole cannot express it until the numeric tower
  (phase 11c) gives the language a one-byte and a four-byte integer type
  that can be used as padding. This is listed as an open question.

#### The interface with the rest of 11d

* `size_of(T) -> Int` and `align_of(T) -> Int` builtins. `size_of` is a
  compile-time constant for a concrete type.
* A typed pointer `Ptr(T)` promises the pointee is aligned as `T` requires.
  The compiler cannot check it - the promise is the programmer's, made in an
  unsafe region - but the *allocation* side can: an allocation is 16-byte
  aligned, so `align_of(T) <= 16` is required for a `Ptr(T)` over
  allocator memory. A type that needs more (a 32-byte-aligned SIMD type in
  phase 11f) needs a dedicated aligned allocation.
* The layout report - `nx explain-layout`, the name the architecture spec
  uses - prints every record's size, alignment and field offsets. It is the
  same computation the compiler uses, so it cannot disagree with it.

### 5. Arenas

#### The model

D009's model, made concrete:

```
arena a:
    let p = arena_alloc(a, 64)
    let q = arena_alloc(a, 128)
    # a is released here, on every exit from this block
```

* The arena is **bump-allocated**: `arena_alloc` moves a bump pointer and
  returns the address. Nothing is ever freed inside the arena.
* The arena has a **single release point**: the end of its scope. Nested
  arenas release inside-out.
* The release runs on the normal path, on an early `return` out of the
  scope, and on `panic`.
* The arena's memory is **not collected**: it is not a collector object, it
  is not in `@nexum_obj_head`, and the sweep never sees it. It comes from
  the runtime allocator and it goes back to the runtime allocator at the
  release point.
* The arena's **contents are traced**: the collector scans the used bytes of
  every live arena, so a `Str` stored in arena memory stays alive.

#### The syntax and the API

```
arena a:
    let raw = arena_alloc(a, size_of(Header))
    unsafe:
        let h: Ptr(Header) = raw
        (*h).magic = 0x4E455855
```

* `arena a:` is a **statement** that introduces a scope and binds `a` to a
  handle of type `Arena`.
* `arena_alloc(a, size) -> Ptr` allocates `size` bytes in the arena,
  16-byte aligned, and returns the address. The memory is **zeroed** when
  the chunk is carved, so an allocation starts zeroed and the unused tail of
  a chunk never holds a stale pointer.
* `arena_release(a)` releases the arena early. It is what the compiler
  emits at the end of the scope; a program may call it explicitly, and then
  the scope's implicit release is a no-op.

`Arena` is a distinct type. It is not a pointer the program can dereference
and it is not a managed reference; it is a handle to a runtime structure.

#### The runtime

The arena is a **chunked bump allocator**. The chunks come from the runtime
allocator (`nexum_heap_alloc` / `nexum_heap_free` from phase 11a.2, `malloc`
until then), so they are ordinary allocator blocks, 16-byte aligned, and the
arena knows each chunk's size and frees it with the same size it allocated
it with - which is exactly the allocator's contract, and which means the
allocator's raw-allocation prologue is *not* required.

The chunk layout mirrors the collector's object layout, so the runtime has
one convention instead of two:

```
+0   used      (i64)  bytes used in the data region
+8   capacity  (i64)  bytes in the data region
+16  next      (ptr)  the next chunk of this arena
+24  pad       (i64)
+32  data              the data region, 16-byte aligned
```

The arena descriptor is a small raw block:

```
+0   next_arena (ptr)  the next live arena
+8   head       (ptr)  the first chunk
+16  current    (ptr)  the chunk being bumped
+24  chunk_cap  (i64)  the capacity of the chunk to carve next
+32  chunk_no   (i64)  how many chunks have been carved
```

The runtime keeps `@nexum_arenas`, the head of the list of live arenas.
`nexum_arena_release` unlinks the arena, frees every chunk and frees the
descriptor.

The chunk sizes double: 4096, 8192, 16384, 32768, 65536, and 65536 after
that. Those are the allocator's own class boundaries, so a chunk is carved
out of the right size class and the waste is only the page rounding.

The runtime interface:

```
define internal ptr  @nexum_arena_new(i64 %chunk_cap)      ; the descriptor
define internal ptr  @nexum_arena_alloc(ptr %arena, i64 %size)
define internal void @nexum_arena_release(ptr %arena)
```

and the collector grows one loop: after the frames are scanned, walk
`@nexum_arenas`, and for each arena walk its chunks and
`nexum_scan_range(chunk + 32, chunk.used)`.

#### Why the arena's contents are traced

A parsing arena holds records whose fields are `Str`. If the collector did
not scan the arena, the strings would be collected while the arena still
pointed at them - the same defect the self-hosted lexer found in the
collector (`docs/self-hosting.md:91`), one level down. Scanning the arena
costs time proportional to the bytes the program has actually used, which is
the same rule the collector already follows everywhere else.

The scan is conservative, exactly like the scan of a heap object's payload:
a word in arena memory that happens to equal a live object's payload address
keeps that object alive for one cycle. That is a delayed free, never a wrong
result - the same bound the collector already has
(`docs/language-reference/decisions-gc.md`).

#### The escape rule

The rule is: **a reference into an arena must not escape the region.**

What the compiler checks:

* **The handle.** `Arena` is a distinct type, so the check is
  type-directed: an `Arena` value may not be returned from a function, may
  not be stored into a `List` or into a record that is stored into a `List`,
  and may not be assigned to a binding outside the arena's scope. T055.
* **Directly derived pointers.** `arena_alloc` produces a value that the
  MIR pass taints with "came from arena `a`". The taint propagates through
  copies and through record and array construction. A tainted value may not
  be returned from the function, may not be stored into a value that
  outlives the arena's scope, and may not be stored into a `List`. T056.

What the compiler cannot check:

* a pointer handed to another function, which stores it somewhere the
  compiler cannot see;
* a pointer stored into a `List` **through** another function;
* a pointer that reaches a global - Nexum has no globals today, but a
  module-level store is the obvious future hole.

Those are a **contract**, not a guarantee, and the contract is what the
`unsafe` marker is for: the region says "the programmer is responsible for
the arena's rules". A pointer that escapes and is used after the release is
a use-after-free, which is memory unsafety, not a language guarantee.

#### Why the handle is checked and the pointer is not

The handle check is free (the type says so). The pointer check is a
provenance pass over the MIR, which is a *sound* check only for the cases
it can see. Making the pointer's non-escape type-directed as well would
need a second pointer type (`ArenaPtr`), a conversion between the two, and
rules about what may hold which - roughly double the pointer machinery, for
a check that the provenance pass already covers in the direct cases. The
recommendation is to ship the provenance pass and treat the type-directed
version as a later refinement if the escape rate turns out to matter.

### 6. Deterministic cleanup

#### The syntax

```
defer close_file(f)
defer arena_release(a)
```

`defer` takes a **call expression**. The call's arguments are evaluated
where the `defer` stands - Go's rule, and the one that makes
`defer close_file(f)` close the file that was open when the `defer` was
written. The call happens when the enclosing **block** exits.

* Within a scope, the deferred calls run **LIFO**.
* A scope's deferred calls run when the scope exits: at the end of the
  block, on an early `return` out of it, and on `panic`.
* A `defer` inside an `if` body belongs to that body, not to the function.

#### The implementation

The runtime keeps a **defer stack**, a singly-linked list of records
`{ ptr thunk, ptr capture, ptr prev }`. The compiler pushes one record per
`defer` statement; the *thunk* is a compiler-generated function that makes
the call, and the *capture* is a stack slot in the frame holding the
evaluated arguments.

* Scope entry records the defer stack's depth; scope exit pops back to it.
* An early `return` emits the pending cleanups of every scope being left,
  in LIFO order, before the return.
* `panic` runs the whole chain from the top before it exits. This is why
  the chain has to be a *runtime* structure and not a compiler-emitted
  sequence: a `panic` can be raised in a callee, and the callee's and the
  caller's cleanups both have to run.

The order of the operations in a returning function is fixed:

```
run the deferred cleanups   (the runtime's defer stack)
pop the root frame
ret
```

The cleanup runs **before** the root frame is popped, because the deferred
call may allocate and the collector must see the frame's roots while it
does. The captured arguments live in the frame, so they are rooted for as
long as the defer is pending; the compiler registers the capture slot as a
root like any other local of a reference type.

#### The arena's release is a defer

The compiler emits the arena's release at the end of an arena scope as a
`defer` on the runtime's defer stack. That gives the arena exactly the
semantics D009 asks for - LIFO over nested scopes, running on early returns
and on panics - with one mechanism instead of two, and it means an arena
nested inside a function that panics is released before the process exits.

#### What `defer` does *not* do

* It does not make the *managed heap* deterministic. A `Str` allocated with
  `concat` is still collected when it becomes unreachable; `defer` cannot
  change that, and it is not meant to.
* It does not run on `exit()` from the C library, or on a signal. There is
  no unwinding in the language yet (`error-handling-model.md` is in flight);
  `panic` runs the chain because the runtime controls that path.

### 7. The collector's interaction

This is the part that has to be exactly right, because the collector is
already written and the soundness argument in `docs/runtime-abi.md` §4
depends on it.

#### How a root is registered

The compiler registers a root for every local and temporary whose type can
hold a reference. The question is asked in one place -
`has_refs` (`crates/nexum-codegen/src/lib.rs:23`) - and answered by the
type:

| Type | A reference? | `scan_bytes` | Registered? |
| ---- | ----------- | ------------ | ----------- |
| `Str` | yes | 8 | yes |
| `List(T)` | yes | 8 | yes |
| a record or array containing one | yes | the whole size | yes |
| `Int`, `Float`, `Bool` | no | 0 | no |
| **`Ptr`, `Ptr(T)`** | **no** | **0** | **no** |
| `Arena` | no | 0 | no |

The last two rows are the whole of phase 11d's effect on the collector:
**a pointer is not a reference, so it is not a root.** The change is two
lines in `has_refs` and two in `scan_bytes`, and the consequence is the
rule the language documents:

> A raw pointer does not keep a managed object alive. If an object must stay
> alive, hold a managed reference to it.

#### What happens when a raw pointer is stored in a managed value

There are three cases, and they are different.

1. **A `List` of pointers.** The element buffer's scan size is computed
   from the element type (`crates/nexum-runtime/src/lib.rs:224`,
   `nexum_list_push`). For a `Ptr` element type the compiler passes **0**,
   so the collector never even looks at the pointer words. This is precise:
   the collector does not follow the pointer.
2. **A record with a pointer field, stored in a `List`.** The buffer's scan
   size is the whole element size, because the element type contains a
   reference. The collector scans the record's words conservatively, so it
   *may* look at the pointer word. If the pointer happens to equal the
   payload address of a live object, that object is marked and kept alive
   for one cycle. **This is the documented conservative imprecision of the
   collector** (`docs/language-reference/decisions-gc.md`), not a new one -
   it is the same thing that happens today with an `Int` field that happens
   to look like a pointer.
3. **A pointer stored in a managed object that outlives the pointee.** The
   pointer is not a root, so the pointee may be collected while the pointer
   is still stored. The pointer then dangles. **This is the hazard the
   language is honest about, and it is why the pointer's *use* is unsafe
   and the pointer's *creation* from a managed value is unsafe.**

#### How a program does it safely

The pattern is: keep the managed reference alive in a rooted slot, and use
the pointer only inside an unsafe region.

```
fn first_byte(s: Str) -> Int:
    # `s` is a local of a reference type, so it is a root for the whole
    # function; the pointer derived from it is valid as long as `s` is.
    unsafe:
        let p = ptr_of(s)
        return ptr_load_u8(p, 0)
```

The `Str` local is registered as a root by the compiler (8 bytes), so the
object cannot be collected while `s` is in scope, and the pointer derived
from it cannot dangle *because of the collector*. What the compiler still
cannot check is that the pointer is not used after `s` goes out of scope -
but the pointer is local to the function, so that is the programmer's
structure, not the collector's.

For a `List`, `ptr_of(l)` gives the address of the list object and
`list_data(l)` gives the address of its element buffer. Both are unsafe.

#### The rules the language states

1. A pointer is never a root. A managed object reachable only through a
   pointer may be collected.
2. A pointer to a managed object is valid only while some managed reference
   to that object is live. The compiler enforces the *absence* of a root;
   the programmer enforces the *presence* of one.
3. Storing a pointer into a managed object does not make the pointee a root.
   The only effect is the documented conservative false retention.
4. The collector never follows a pointer: it does not scan the memory the
   pointer points at. It only ever compares the pointer's *value* against
   the set of live objects.
5. The arena is the exception, and it is an exception by design: the
   arena's contents are scanned, because the arena's memory is the program's
   own memory and the program put references into it.

### 8. The order of work

#### Independent of phase 11a.2

| Piece | Why it is independent | What it touches |
| ----- | --------------------- | -------------- |
| **The layout engine** | pure compiler work | `nexum-codegen` (emit the padding), a new layout module, `size_of`/`align_of`, the `layout` attributes, the `c` check |
| **The pointer type** | needs no new runtime memory | `nexum-sema` (`Ty::Ptr`), `nexum-parser` (`TypeExpr::Ptr`), `nexum-codegen` (`llvm_ty`, the place lowering) |
| **`unsafe` regions** | a check, not a feature | `nexum-sema` (the unsafe state), the parser (the block and the `unsafe fn` marker), the diagnostics |
| **`volatile`** | an LLVM load/store flag | `nexum-codegen` (the volatile loads and stores) |
| **`defer`** | one runtime list and one lowering rule | `nexum-runtime` (the defer stack), `nexum-mir` (the `Defer` instruction and the scope lowering), `nexum-codegen` (the thunk) |

The layout engine comes first, because the typed pointer's offsets come from
it and the arena's typed allocations need `size_of`.

#### Needs phase 11a.2

| Piece | What it needs | Blocking? |
| ----- | ------------- | --------- |
| **The arena** | `nexum_heap_alloc` / `nexum_heap_free` with an explicit size | **No.** It can be built on `malloc`/`free` (what the runtime uses today) and moved onto the new allocator later, because the arena's contract with the allocator is "the same total in and out", which both satisfy. |
| **Program-level `alloc` / `free`** | the same two functions | **No**, for the same reason - but `free` takes the size explicitly (`free(p, size)`), so the allocator's raw prologue (its open question 7) is not needed. If the orchestrator wants `free(p)` without the size, that is where the prologue migration belongs. |
| **`map_region`** (memory-mapped registers) | the `mmap`/`munmap` wrappers, which exist | The syscall layer is done, but `mmap` has never been called; the allocator research flags a bug in the wrapper's flags register. |

#### The order that falls out

1. The layout engine and the layout report (independent).
2. `Ptr`, `Ptr(T)`, the place-expression lowering, `unsafe:`, `unsafe fn`,
   `volatile` (independent).
3. `defer` (independent, one runtime list).
4. The arena: the runtime's chunked bump allocator, the arena list, the
   collector's arena scan, the scope and the escape check.
5. The program-level memory builtins: `alloc`, `free`, `ptr_of`,
   `ptr_add`, `ptr_offset`, the raw loads and stores - on top of whichever
   allocator is in place.

Steps 1-3 can be implemented in parallel by different agents: they touch
disjoint files (the layout is `nexum-codegen` plus a new module, the pointer
type is `nexum-sema`/`nexum-parser`/`nexum-codegen`, `defer` is
`nexum-runtime`/`nexum-mir`). Step 4 needs the runtime's allocator
interface but not the allocator itself.

### 9. The changes, per crate

The interfaces are pinned here so that an implementer does not have to
guess. The file names are the ones in the tree today.

#### `crates/nexum-lexer`

New keywords: `unsafe`, `defer`, `arena`, `volatile`. The keyword list is
in `docs/language-reference/grammar.md:19` and the tokens in
`crates/nexum-lexer/src/lib.rs`.

#### `crates/nexum-parser`

```rust
pub enum TypeExpr {
    Named(String),
    Array(Box<TypeExpr>, usize),
    Ptr(Box<TypeExpr>),      // `Ptr(T)`; `Ptr` alone stays `Named("Ptr")`
}

pub enum StmtKind {
    // ... today ...
    Unsafe { body: Vec<Stmt> },                       // `unsafe:`
    Defer { call: Expr },                             // `defer f(x)`
    Arena { name: String, body: Vec<Stmt> },          // `arena a:`
}

pub enum FnDecl { Unsafe { ... } }                    // `unsafe fn`
```

The expression grammar gains the unary `*`; the lvalue grammar gains the
dereference base and the `volatile` prefix.

#### `crates/nexum-sema`

```rust
pub enum Ty {
    // ... today ...
    Ptr(Option<Box<Ty>>),   // None = the byte pointer
    Arena,
}
```

* `Ty::compatible`: `Ptr(None)` is compatible with `Ptr(None)`; `Ptr(Some(a))`
  with `Ptr(Some(b))` when `a == b`; `Ptr(None)` with `Ptr(Some(_))` only in
  an unsafe region (the narrowing), never the other way round without one.
* The analysis context carries an `unsafe_depth: usize`. The five unsafe
  operations check it and report T050-T054.
* The escape checks: the `Arena` handle check (type-directed) and the
  arena-pointer provenance check (a MIR pass, see below).
* The layout computation lives here or in a new module, and is the single
  source of truth for the offsets, the sizes and the alignments.

#### `crates/nexum-mir`

```rust
pub enum Instr {
    // ... today ...
    DerefLoad  { dest: Value, ptr: Value, offset: Option<usize>, ty: Ty, volatile: bool },
    DerefStore { ptr: Value, offset: Option<usize>, value: Value, ty: Ty, volatile: bool },
    PtrOffset  { dest: Value, ptr: Value, bytes: Value },
    Defer      { callee: String, args: Vec<Value> },
    ArenaNew   { dest: Value, chunk_cap: Value },
    ArenaAlloc { dest: Value, arena: Value, size: Value },
    ArenaRelease { arena: Value },
}
```

New terms or a new scope construct for the arena scope and the defer scopes,
so that the lowering can emit the cleanups on every exit path. The
existing `Term::Return` is where the cleanups are emitted.

New pass: `arena_escape_check(m: &MirModule) -> Vec<Diagnostic>` - the
provenance check. It is the enforcing version of the existing report-only
`escape_analysis` (`crates/nexum-mir/src/lib.rs:1317`), and it should share
its code: the report becomes the enforcement.

#### `crates/nexum-codegen`

* `llvm_ty`: `Ty::Ptr(_)` and `Ty::Arena` are `ptr`.
* `has_refs` / `scan_bytes`: `Ptr` and `Arena` are not references. **This is
  the change that makes "a raw pointer is not a root" true.**
* The record emission (`crates/nexum-codegen/src/lib.rs:124`) emits the
  computed layout with explicit padding, so the layout does not depend on
  LLVM.
* The place lowering: a dereference becomes a `load`/`store` at
  `ptr + offset`, with the offset from the layout; a volatile access
  becomes a `volatile load`/`volatile store`.
* The arena scope: a prologue that creates the arena, an epilogue that
  releases it on every exit path, and the defer thunk.
* The root entries (`crates/nexum-codegen/src/lib.rs:370`): the capture slot
  of a `defer` is registered like any other local of a reference type.

#### `crates/nexum-runtime`

```llvm
; the defer stack
%nexum.defer = type { ptr, ptr, ptr }   ; thunk, capture, prev
@nexum_defers = internal global ptr null, align 8

define internal void @nexum_defer_push(ptr %thunk, ptr %capture)
define internal void @nexum_defer_pop_to(i64 %depth)
define internal void @nexum_defer_run_all()      ; called by nexum_panic

; the arena
@nexum_arenas = internal global ptr null, align 8
define internal ptr  @nexum_arena_new(i64 %chunk_cap)
define internal ptr  @nexum_arena_alloc(ptr %arena, i64 %size)
define internal void @nexum_arena_release(ptr %arena)

; the raw memory interface the language exposes
define internal ptr  @nexum_raw_alloc(i64 %size)          ; 16-byte aligned
define internal void @nexum_raw_free(ptr %p, i64 %size)
```

* `nexum_panic` (`crates/nexum-runtime/src/lib.rs:122`, the POSIX branch)
  calls `@nexum_defer_run_all()` before it exits.
* `nexum_collect` (`crates/nexum-runtime/src/lib.rs:174`) gains the arena
  scan after the frame scan.
* The mark bit and the next-object link become `volatile` accesses, and the
  ABI document says so.

#### `crates/nexum-driver`

* `nx explain-layout` - the layout report: every record's size, alignment
  and field offsets, and the layout of the builtin types.
* The builtins map (the place the builtins are lowered) gains `alloc`,
  `free`, `ptr_of`, `ptr_add`, `ptr_offset`, the raw loads and stores,
  `arena_alloc`, `arena_release`, `size_of` and `align_of`.

#### The documents

* `docs/runtime-abi.md` gains: the pointer's representation and its
  operations, the layout rules, the arena's chunk and descriptor layouts and
  its release semantics, the defer stack's layout, and the note that the
  mark bit and the free link are volatile. §6 and §7 stop saying "staged".
* `docs/language-reference/grammar.md` gains the new syntax and the new
  diagnostic codes.
* `docs/language-reference/decisions.md` gains the decisions this phase
  takes: D017 (the pointer type and the root rule), D018 (the layout is
  Nexum's own, with an optional C assertion), D019 (`unsafe` is a block
  and a function; `volatile` is an access-site qualifier), D020 (the arena
  model, finishing D009), D021 (`defer`). D009 is updated to point at them.

## Risks

**The layout changes every ABI.** Computing the layout in the compiler and
emitting explicit padding changes the generated code for every existing
record. The natural layout the compiler computes today and the layout LLVM
computes agree for the current type set, so the *bytes* do not change - but
the emitted IR does, and the differential tests will notice. The risk is
low and the tests will catch it.

**`Bool` is one byte in Nexum and one byte in C, but not for the reason
anyone expects.** LLVM's `i1` has an *allocation* size of one byte, so the
two agree by accident. The layout rules must state it, and the `c` check
must use the stated rule rather than LLVM's default, or the check will be
right by luck.

**The provenance check is incomplete, and incomplete checks are dangerous in
a different way from no checks.** A compiler that rejects *some* escapes
teaches the programmer that the language prevents escapes. It does not. The
documentation must say, next to the diagnostic, exactly which escapes the
check catches and which it does not, and the wording must be the same
everywhere it appears. The alternative - no check at all - is worse: an
arena pointer returned from a function would be silently dangling.

**Scanning the arenas costs time.** The collector now scans every live
arena's used bytes at every collection. A program that puts a large arena in
a loop pays for it at every cycle. This is the same trade the collector
already makes for aggregates, and it is proportional to what is used rather
than to what was reserved - the arena tracks `used`, not `capacity` - but it
is a new cost and it should be measured, not assumed.

**Eager arena release and the collector's accounting.** The arena's chunks
are not collector objects, so the collector's `@nexum_live` does not see
them. That is correct - they are not managed memory - but it means
`heap_used()` does not include the arena's bytes, and a program cannot use
`heap_used()` to reason about its footprint. If that matters, the arena
should report its own bytes.

**`volatile` in LLVM is weaker than people expect.** LLVM's `volatile` says
"do not elide, do not reorder with respect to other volatile accesses on
*this* thread". It does not say what the hardware does. A volatile access
to a device register is the right tool; a volatile access used as a
synchronisation primitive is a bug. The documentation has to say this in
the same words the architecture spec uses.

**The defer stack is global.** It works because the runtime is
single-threaded. Phase 11e (concurrency) has to make it thread-local, and
the `defer` lowering has to survive that. A per-thread defer stack is a
small change, but it is a change to a global that phase 11d introduces.

**The `unsafe` region becomes a place where the rules are forgotten.** This
is the risk every language with `unsafe` carries. The mitigation is that the
region unlocks five operations and nothing else, so the code inside an
unsafe region is still fully type-checked and still bounds-checked.

**`Ptr(T)` is a typed claim the compiler cannot back.** `Ptr(Header)` says
"this address holds a `Header`"; the compiler believes the programmer. That
is exactly what the unsafe region is for, but the *type* makes the claim
look checked. The documentation must be explicit that the type says how to
interpret the bytes, not that the bytes are there.

**The overlap with the other research in flight.** `record-layout.md` owns
the layout rules, `allocator-design.md` owns the allocator, `atomics-model.md`
owns the orderings, `syntax-evolution.md` owns the syntax growth, and
`interop-policy.md` owns what C interop may look like. This document takes
positions on the first two's interfaces (§4 and §8) and on the syntax
(§1-§3, §5-§6); the lead has to reconcile them. The most likely conflict
is the layout: this document assumes the compiler computes the layout and
emits the padding, because the typed pointer needs the offsets to be the
compiler's.

## Open questions

Each question is one the orchestrator has to decide, with a recommendation.

1. **Does the language get a second pointer type for arena memory
   (`ArenaPtr`)?** It would make the escape rule type-directed instead of
   provenance-based, at the cost of a second pointer type, a conversion
   between them, and rules about what may hold which.
   *Recommendation:* no in 11d. Ship the provenance check, which covers the
   direct cases, and revisit if the escapes that get through it turn out to
   matter.

2. **Should `defer` take a call expression only, or any expression?** A
   call only is simpler to lower (one thunk shape) and safer (there is no
   way to defer an expression with a surprising evaluation order). Zig
   allows any expression.
   *Recommendation:* a call expression only. If the standard library needs
   more, a `defer` of a block-like form can be added later.

3. **Should the arena's release be implicit only, or also explicit?**
   `arena_release(a)` called by hand, inside the arena's own scope, would
   free memory that the scope's implicit release will free again.
   *Recommendation:* the explicit call exists and is idempotent (the second
   release is a no-op), because a program that wants to free early needs it;
   but the compiler should warn if it can see the double release.

4. **Should a `defer` inside a `for` body run once per iteration?** The
   scope is the loop body, so yes - and that is usually what a programmer
   wants, but it is a per-iteration cost.
   *Recommendation:* yes, per iteration, and say so in the documentation.

5. **Does `unsafe` need a "safe" wrapper marker - a function that promises
   to uphold the invariants on the caller's behalf?** `unsafe fn` already
   marks the *caller's* obligation; a "safe wrapper" is just a normal
   function that contains an unsafe region.
   *Recommendation:* no separate marker. A normal function containing an
   `unsafe:` region is the safe wrapper, and the compiler can check it (it
   has to contain the unsafe region to use the pointer).

6. **Should the `c` layout be checked against a real C compiler?** The
   compiler computes the C layout from the documented C rules. A C compiler
   could disagree on a platform whose ABI is not the textbook one.
   *Recommendation:* compute it from the rules, and say in the ABI document
   which platforms the `c` layout is claimed for. A differential test
   against a C compiler is the way to *earn* the claim on a new platform,
   not a way to define it.

7. **Should `Ptr` be usable in a `List` at all?** The collector does not
   follow it, and storing one in a managed container is nearly always a
   mistake - it is how a dangling pointer gets a long life.
   *Recommendation:* allow it (the language must be able to hold an address
   in a data structure), but forbid an `Arena` handle in a `List`, and make
   the `List` of `Ptr` case a documented hazard.

8. **Should the arena's chunk sizes be configurable?** They are pinned to
   the allocator's class boundaries so that a chunk is carved out of the
   right size class.
   *Recommendation:* no. A runtime constant, matched to the allocator's
   classes. A program-level knob would be a second source of truth about
   memory sizes.

9. **Do the raw loads and stores get one builtin per width, or one builtin
   per type?** One per width is mechanical and needs no type dispatch:
   `ptr_load_u8`, `ptr_load_i32`, `ptr_load_f64`, and the matching stores -
   one pair per scalar type in the numeric tower (phase 11c). Twenty
   builtins is a lot of surface, but each is one line in the lowering.
   *Recommendation:* one pair per scalar type, and add them with 11c so
   the set is exactly the numeric tower.

10. **Should `free` take the size?** `free(p, size)` is explicit and needs
    no allocator prologue; `free(p)` is convenient and needs the prologue
    migration the allocator research already describes.
    *Recommendation:* `free(p, size)` in 11d. The prologue is a good change
    but it belongs to the allocator's slice, and 11d should not block on it.

11. **Is the collector's arena scan a separate loop, or should the arena
    register itself as a root?** A separate loop over the arena list is
    simple and keeps the root-frame mechanism untouched. Registering the
    arena as a root would need a root entry per chunk, and the number of
    chunks is not known at compile time.
    *Recommendation:* the separate loop. It is the only place where the
    collector looks at memory that is not a collector object, and it should
    be one clearly marked function in the runtime.

12. **Should the record layout be exposed as a runtime type descriptor, so
    that a program can ask for its own layout?** The architecture spec
    mentions type descriptors as open work (D010) and as a subsystem of its
    own. A runtime descriptor would let a program build a record at run time
    from a description.
    *Recommendation:* no in 11d. The compiler's layout and the layout
    report are enough; a runtime descriptor is the D010 subsystem and should
    not be smuggled in through the pointer work.
