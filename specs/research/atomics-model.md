# The memory model: the atomic types, the orderings, and the happens-before rules

**Question.** What memory model does phase 11e need so that concurrency in
Nexum is *defined* rather than accidental: which atomic types the language
has and what they are built from, which operations exist, which orderings
exist and what each one guarantees, what the default is, how the whole thing
maps onto LLVM's atomic instructions, what the language promises and what it
leaves to the programmer, what has to change in the collector, and what the
code generator has to emit so that the ordering survives the optimiser.

**Summary.** Nexum gets four atomic types - `AtomicInt` (64-bit),
`AtomicInt32` (32-bit), `AtomicBool` (1 byte) and `AtomicPtr` (pointer
width) - all of them handles onto a cell on the managed heap, all of them
natively lock-free on every supported target, so the freestanding runtime
stays freestanding. Five operations: `atomic_load`, `atomic_store`,
`atomic_exchange`, `atomic_fetch_add`, `atomic_compare_exchange`, plus
`atomic_fence` for the one case an atomic instruction cannot cover. Four
orderings - `relaxed`, `acquire`, `release`, `seq_cst` - given as values of
a builtin `Ordering` type; the ordering argument is optional and **the
default is `seq_cst`**, because the default must never be a correctness trap.
The mapping onto LLVM is mechanical and total: `relaxed` is `monotonic`,
`acquire` is `acquire`, `release` is `release`, `seq_cst` is `seq_cst`; a
load is `load atomic`, a store is `store atomic`, an exchange is
`atomicrmw xchg`, a fetch-and-add is `atomicrmw add`, and a compare-and-
exchange is `cmpxchg` with a *derived* failure ordering. The language
guarantees that the atomics behave - atomicity, a modification order, and
release/acquire happens-before - and it explicitly does **not** guarantee
that a data race is impossible; a race on a non-atomic location is undefined
behaviour, and the compiler does not look for it. What the language *can*
guarantee is a documented safe subset: data that travels through the
standard library's channel, or that is published with a release store and
read after an acquire load, is not a data race. The collector's part is
separate from the language's part and does not come from atomics: the global
root list becomes per-thread, the object list and the watermark come under a
heap lock, and the collector reads an atomic pointer cell with an *atomic*
load - a plain load there would be a data race in LLVM's model, which the
optimiser is entitled to exploit.

## Recommendation

1. **Four atomic types, all of them handles onto a heap cell.**
   `AtomicInt` (64-bit), `AtomicInt32` (32-bit), `AtomicBool` (1 byte) and
   `AtomicPtr` (pointer width). They are *not* value types: a value type
   would be copied on assignment, and the copy is a plain load, which would
   silently destroy the atomicity. They are reference types, like `List`:
   `atomic_new(0)` returns an `AtomicInt` handle; the handle is a pointer
   to a cell on the managed heap; the cell holds the value; the handle may
   be copied and both copies name the same cell.
   *Reason:* the language already has managed reference types that the
   collector traces (`Str` and `List`), so the machinery - rooting,
   tracing, the object list - already exists and an atomic cell is one
   more object kind. A value-typed atomic would also make "is this access
   atomic?" a property of the *binding* rather than of the *access*, which
   is the property that actually matters.

2. **The cell is a managed-heap object, and an `AtomicPtr` cell is a traced
   object whose payload is scanned with an atomic load.** The object header
   gains a `flags` word; the `ATOMIC` flag says "read this payload with
   `load atomic ... acquire`". The collector may not read an atomic cell
   with a plain load: in LLVM's model a non-atomic load that races with a
   store is undefined behaviour, and the optimiser may assume the value it
   reads is the one the same thread last wrote.
   *Reason:* the payload of an `AtomicPtr` cell is a pointer to a managed
   object. If the collector does not trace it, the object it points at can
   be collected while the atomic still holds it.

3. **Five operations, plus a fence.** `atomic_load`, `atomic_store`,
   `atomic_exchange`, `atomic_fetch_add`, `atomic_compare_exchange`, and
   `atomic_fence`. Each of the first five takes an optional ordering
   argument; `atomic_fence` takes a mandatory one.
   *Reason:* these are the five operations the phase-11e work actually
   needs, and they are the five LLVM instructions that cover them without a
   library call. `atomic_compare_exchange` returns a `Bool` and writes the
   value it found back into a `mut expected` parameter, which is the C++
   shape and needs no new type in a language whose only composite types are
   records.

4. **Four orderings, as values of a builtin type `Ordering`, spelled with
   four new keywords: `relaxed`, `acquire`, `release`, `seq_cst`.**
   *Reason:* the orderings are a vocabulary that the standard library's
   mutex, channel and lock-free structures will all want, so they are a
   type rather than a naming convention. Spelling them as keywords rather
   than as suffixes on every builtin keeps the builtin count at six instead
   of fifteen.

5. **The default ordering is `seq_cst`.**
   *Reason:* the default must not be a correctness trap. A program that
   writes `atomic_store(c, v)` and reads it back with `atomic_load(c)` is
   race-free under the default and racy under a `relaxed` default, and the
   programmer who wants the cheaper ordering has to say so. The cost is
   bounded: on x86-64 a `seq_cst` *load* is a plain `mov` and a `seq_cst`
   read-modify-write is already a `lock`ed instruction; only a `seq_cst`
   *store* costs an `xchg`. `relaxed` must always be written out.

6. **The ordering argument of a load may not be `release`, and the ordering
   argument of a store may not be `acquire`.** LLVM's verifier rejects both
   ("atomic load cannot use Release ordering", "atomic store cannot use
   Acquire ordering"), so these become Nexum diagnostics rather than
   undefined behaviour. `atomic_fence(relaxed)` is likewise a diagnostic:
   LLVM rejects a monotonic fence.
   *Reason:* the language should reject at compile time what the IR
   verifier would reject at build time.

7. **The exact LLVM mapping, total and closed.** With `T` the cell's LLVM
   type and `A` its natural alignment:

   | Nexum | LLVM | Orderings allowed |
   | ----- | ---- | ----------------- |
   | `atomic_load(a, o)` | `%v = load atomic T, ptr %p <o>, align A` | `monotonic`, `acquire`, `seq_cst` |
   | `atomic_store(a, v, o)` | `store atomic T %v, ptr %p <o>, align A` | `monotonic`, `release`, `seq_cst` |
   | `atomic_exchange(a, v, o)` | `%o = atomicrmw xchg ptr %p, T %v <o>, align A` | all four |
   | `atomic_fetch_add(a, d, o)` | `%o = atomicrmw add ptr %p, T %d <o>, align A` | all four |
   | `atomic_compare_exchange(a, e, d, o)` | `%r = cmpxchg ptr %p, T %e, T %d <success> <failure>, align A` | all four |
   | `atomic_fence(o)` | `fence <o>` | `acquire`, `release`, `seq_cst` |

   with `<o>` being `monotonic` for `relaxed`, `acquire` for `acquire`,
   `release` for `release` and `seq_cst` for `seq_cst`, and the
   compare-and-exchange's two orderings *derived* from the one the
   programmer wrote:

   | Ordering | success | failure |
   | -------- | ------- | ------- |
   | `relaxed` | `monotonic` | `monotonic` |
   | `acquire` | `acquire` | `acquire` |
   | `release` | `release` | `monotonic` |
   | `seq_cst` | `seq_cst` | `seq_cst` |

   *Reason:* this is exactly what clang emits for the equivalent C11
   operations (verified below), so the mapping has no room for a decision
   left open, and the failure ordering can never be stronger than the
   success ordering because the programmer never writes it.

8. **The alignment of an atomic is its natural alignment, and the heap
   layout already guarantees it.** `AtomicInt` 8, `AtomicInt32` 4,
   `AtomicBool` 1, `AtomicPtr` 8 (the pointer width, 8 on all five
   supported targets). The cell's payload starts 32 bytes into a
   16-byte-aligned block, so it is 16-byte aligned and every one of those
   requirements is met with room to spare. The codegen always emits the
   natural `align`; a `load atomic`/`store atomic` *must* carry an explicit
   non-zero alignment in textual LLVM IR or the parser rejects the module.

9. **The language guarantees the atomics; it does not guarantee the absence
   of races.** The guaranteed part: every atomic operation is atomic (no
   torn reads or writes), the modifications of one atomic object form a
   single total order, a release operation that an acquire operation reads
   from establishes a happens-before edge, and all `seq_cst` operations
   form one total order. The unguaranteed part: a data race on a
   non-atomic location is undefined behaviour, the compiler does not detect
   it, and the language makes no attempt to.

10. **The safe subset is a documented rule, not a checked one.** Data that
    travels through the standard library's channel is not a data race: the
    channel's send and receive are a release/acquire pair, so the payload
    is published and observed. The same holds for the general publish
    pattern - write the data, `atomic_store` it with `release`, and read it
    after an `atomic_load` with `acquire` that observes the store. This is
    a guarantee the language can make *about the pattern*, and it is the
    only concurrency guarantee that is not purely about the atomics
    themselves. The language cannot check it: there is no borrow checker
    and no thread-affinity analysis, so a program that shares the same
    payload by another route is still racy. A race *detector* (a
    ThreadSanitizer-style shadow-memory build of the runtime) is the honest
    way to help the programmer, and it is a build-time option, not a
    language feature.

11. **The collector's part comes from a lock and a stop-the-world, not from
    atomics.** The global root list becomes per-thread, so a thread's push
    and pop touch only its own list; the list of thread control blocks is
    the one genuinely shared list and it is protected by a mutex; the
    object list, the mark bits and the watermark counters come under a
    global heap lock that the allocator and the collector both take. The
    one place the language's atomics are used *inside* the runtime is the
    "a collection was requested" flag, which is an `AtomicBool`.

12. **The code generator needs new MIR instructions, and the MIR passes
    need to know about them.** `AtomicLoad`, `AtomicStore`, `AtomicRmw`,
    `AtomicCas` and `Fence`, each carrying an `AtomicOrder`. The
    read-modify-write and the compare-and-exchange are side-effecting and
    must be marked as such in `is_side_effecting`, or dead-code
    elimination will delete them; the constant folder must not fold them;
    and the inliner must refuse functions that contain them. The ordering
    lives in the LLVM instruction itself, so **no separate barrier syntax
    is needed** for the atomic operations - the one extra surface is
    `atomic_fence`, for the fence-fence message-passing pattern and for a
    full barrier.

13. **The language does not need an `acq_rel` ordering, and this is a
    deliberate limitation.** A lock-free stack needs a compare-and-exchange
    that is both acquire and release. With the four orderings above the
    stack is still expressible - the acquire lives on the *load* of the
    head and the release lives on the successful compare-and-exchange - and
    that is the pattern the document should pin down. If the orchestrator
    prefers to have `acq_rel` on the compare-and-exchange, it is one
    keyword and one more entry in the derivation table; it is not needed
    for phase 11e, and adding it later is not a breaking change.

## What the code looks like today

Facts from the repository at commit `bac9d0c` (the `main` this branch is
based on). Line numbers are against that commit.

* **There is no concurrency in the language at all.** `docs/native-roadmap.md:53`
  lists "No concurrency. No threads, no atomics, no channels." as a gap, and
  `docs/native-roadmap.md:73` makes phase 11e "Concurrency: threads,
  atomics, a scheduler, channels." The word "atomic" appears nowhere in
  `crates/`: there is no atomic instruction, no ordering, no fence.

* **The type system has no atomic type.** `crates/nexum-sema/src/lib.rs:15-32`
  is the whole `Ty` enum: `Int`, `Float`, `Bool`, `Str`, `Unit`,
  `Record(String)`, `Array(Box<Ty>, usize)`, `List(Box<Ty>)`, `Never`,
  `Unknown`, `Error`. `crates/nexum-codegen/src/lib.rs:8-20` maps that to
  LLVM: `i64`, `double`, `i1`, `ptr`, `void`, a struct, an array, `ptr`.

* **Every memory access the compiler emits is a plain access.**
  `crates/nexum-codegen/src/lib.rs:1140-1153` lowers `Instr::Load` to
  `load <ty>, ptr <slot>` and `crates/nexum-codegen/src/lib.rs:1155-1171`
  lowers `Instr::Store` to `store <ty> <value>, ptr <slot>`. There is no
  `volatile`, no `atomic`, no `fence` anywhere in the emitter.

* **The MIR has no atomic instruction.** `crates/nexum-mir/src/lib.rs:47-135`
  is the `Instr` enum: `Const`, `BinOp`, `UnOp`, `Call`, `Copy`, `Alloca`,
  `Load`, `Store`, `Record`, `FieldGet`, `FieldSet`, `Panic`, `ArrayLit`,
  `IndexGet`, `IndexSet`. Three passes would have to learn about atomics:
  `collect_uses` (`crates/nexum-mir/src/lib.rs:993-1051`) has to count the
  new operands as uses, `is_side_effecting`
  (`crates/nexum-mir/src/lib.rs:1091-1096`) has to treat the
  read-modify-write, the compare-and-exchange and the fence as
  side-effecting or the dead-code pass will delete them, and `inlinable`
  (`crates/nexum-mir/src/lib.rs:1108-1135`) refuses any function that
  contains a `Load`, a `Store`, a `Call` or a `Panic` and would have to
  refuse the atomic instructions too.

* **The collector's two global lists are plain, unsynchronised globals.**
  `crates/nexum-runtime/src/lib.rs:104` declares
  `@nexum_obj_head = internal global ptr null, align 8` and
  `crates/nexum-runtime/src/lib.rs:105` declares
  `@nexum_roots_head = internal global ptr null, align 8`. Beside them,
  `crates/nexum-runtime/src/lib.rs:106-108` declare `@nexum_live`,
  `@nexum_since` and `@nexum_threshold`, all plain `i64` globals.

  * The **object list** is pushed by the allocator,
    `crates/nexum-runtime/src/lib.rs:147` (`nexum_alloc_scanned`), with a
    plain load of the head followed by two plain stores - a read-modify-write
    on a shared location with no protection at all.
  * The **root list** is pushed and popped by
    `crates/nexum-runtime/src/lib.rs:136` (`nexum_roots_push`) and
    `crates/nexum-runtime/src/lib.rs:139` (`nexum_roots_pop`), again with
    plain loads and stores. The compiler calls the push on every function
    that has roots (`crates/nexum-codegen/src/lib.rs:456`) and the pop
    before every return (`crates/nexum-codegen/src/lib.rs:1194`), so this
    is the hottest shared write in the program.
  * The **mark bit** is written by `nexum_mark_ptr`
    (`crates/nexum-runtime/src/lib.rs:162`), the **trace** walks the object
    list (`nexum_gc_trace`, `crates/nexum-runtime/src/lib.rs:180`), the
    **sweep** unlinks from it and frees (`nexum_sweep`,
    `crates/nexum-runtime/src/lib.rs:186`), and the **collection** walks the
    root list (`nexum_collect`, `crates/nexum-runtime/src/lib.rs:174`).
  * The **watermark** is checked by `nexum_gc_maybe`
    (`crates/nexum-runtime/src/lib.rs:155`).

* **The collector is documented as single-threaded.**
  `docs/runtime-abi.md:118` says "non-moving, single-threaded mark-sweep",
  `docs/runtime-abi.md:231` lists "Single-threaded only" under the current
  restrictions, and `docs/language-reference/decisions.md:149` (D010) leaves
  concurrency open. `crates/nexum-runtime/src/lib.rs:38` repeats it in the
  crate comment: "Collector policy: non-moving, single-threaded,
  mostly-precise."

* **The heap object layout leaves room for a flags word.**
  `docs/runtime-abi.md:111-117`: `+0` size, `+8` mark, `+16` next, `+24`
  scan bytes, `+32` payload, 16-byte aligned. The header is 32 bytes, so
  there is no spare word; a `flags` field means the header becomes 40 bytes
  and the payload moves to a 48-byte offset, or the flag is stolen from the
  top bit of `scan_bytes`.

* **The alignment helpers already exist and would have to learn the atomic
  types.** `crates/nexum-codegen/src/lib.rs:36-53` (`llvm_align`),
  `:55-68` (`llvm_size`), `:85-99` (`scan_bytes`) and `:23-34`
  (`has_refs`). `crates/nexum-target/src/lib.rs:85-100` is the data layout
  per target, and every supported target is 64-bit with an 8-byte pointer.

* **The grammar has no place for a type constructor and no place for an
  ordering value.** `docs/language-reference/grammar.md:66-68` gives
  `type-expr = identifier | "[" type-expr ";" int-lit "]"`, and
  `docs/language-reference/grammar.md:19` is the keyword list. Both need a
  small change: four new keywords for the orderings, and one new type name
  per atomic type (which fits the existing `identifier` rule).

## The design

### The types

| Nexum type | Handle (LLVM) | Cell payload (LLVM) | Cell payload size | Cell payload alignment | Cell payload holds a reference? |
| ---------- | -------------- | -------------------- | ------------------ | ---------------------- | ----------------------------- |
| `AtomicInt` | `ptr` | `i64` | 8 | 8 | no |
| `AtomicInt32` | `ptr` | `i32` | 4 | 4 | no |
| `AtomicBool` | `ptr` | `i8` | 1 | 1 | no |
| `AtomicPtr` | `ptr` | `ptr` | 8 | 8 | **yes** |

The **handle** is a pointer in every case, and it is a reference in every
case: it points at the cell, so it is rooted and scanned like a `List`
handle. Only the *cell's payload* differs, and only the `AtomicPtr` cell
holds something the collector has to trace.

Three decisions are worth spelling out.

* **`AtomicBool` is `i8`, not `i1`.** LLVM rejects an atomic access whose
  type is not byte-sized ("atomic memory access' size must be byte-sized",
  verified below), and clang's own `_Bool` atomics are `i8`. The value
  stored is 0 or 1; the operations return a Nexum `Bool`.
* **`AtomicInt` is 64 bits, matching `Int`.** The width is a property of
  the *type*, so the family rule is: an atomic integer type is an integer
  type plus a width, and the two widths above are the ones the phase-11e
  work needs. Adding `AtomicInt8` or `AtomicInt16` later is one row in the
  table in the code generator and one constructor, with no change to any of
  the rules below.
* **`AtomicPtr` is a reference, and the collector must know it.** The
  `AtomicPtr` cell's *payload* is a pointer to a managed object. The cell
  itself is marked like any other object, and that payload - the pointer -
  has to be traced with an atomic load, not a plain one.

The cell is an opaque object on the managed heap whose *payload* is the
value; the table above gives the payload's type, size and alignment for
each kind.

All four cells are created by the same runtime entry point with a flag
saying whether the payload is atomic:

```
nexum_atomic_new(i64 payload_size, i64 scan_bytes) -> ptr
```

which allocates through `nexum_alloc_scanned` and marks the object
`ATOMIC` in its header. `AtomicPtr` cells are allocated with
`scan_bytes = 8`; the other three with `scan_bytes = 0`.

The **handle** the program holds is the payload pointer, exactly as a
`List` handle is (`docs/runtime-abi.md:203`). The handle is 8 bytes, is a
reference, and is rooted like any other reference; the cell itself is
reached through the object list.

Two couplings to the other phases, stated here so that they are not
discovered late:

* `atomic_new32` takes an `Int` and stores its low 32 bits, because the
  language has no 32-bit integer type and no explicit conversion function
  yet (`docs/language-reference/decisions.md:74`). When phase 11c's
  numeric tower lands, the constructor should take that type and the
  truncation should become a diagnostic.
* `atomic_new_ptr` needs a pointer value, and the language has no pointer
  type until phase 11d. Until then `AtomicPtr` cannot be constructed by a
  program, so the type can be specified and implemented in 11e and become
  usable in 11d.

### The operations

```
atomic_new(value: Int)            -> AtomicInt
atomic_new32(value: Int)          -> AtomicInt32
atomic_new_bool(value: Bool)      -> AtomicBool
atomic_new_ptr(value: Ptr)        -> AtomicPtr

atomic_load(a: AtomicInt,  o?: Ordering)          -> Int
atomic_store(a: AtomicInt, v: Int, o?: Ordering)   -> Unit
atomic_exchange(a: AtomicInt, v: Int, o?: Ordering) -> Int
atomic_fetch_add(a: AtomicInt, d: Int, o?: Ordering) -> Int
atomic_compare_exchange(a: AtomicInt, mut expected: Int, v: Int, o?: Ordering) -> Bool
atomic_fence(o: Ordering)          -> Unit
```

with the same shape for `AtomicInt32`, `AtomicBool` and `AtomicPtr`
(`atomic_fetch_add` does not exist for `AtomicBool` or `AtomicPtr`).

* `atomic_load` returns the value that was in the cell.
* `atomic_store` replaces it.
* `atomic_exchange` replaces it and returns the value that was there.
* `atomic_fetch_add` adds to it and returns the value that was there. The
  addition is two's-complement wrapping, like every other `Int` arithmetic
  in the language (`docs/language-reference/decisions.md:70`).
* `atomic_compare_exchange` compares the cell against `expected`, and on
  success replaces it with `v` and returns `true`; on failure it leaves
  the cell alone, writes the value that *was* in the cell into `expected`,
  and returns `false`. The `mut expected` parameter is the language's
  existing by-reference borrow (`docs/language-reference/decisions.md:9-23`,
  D001), and the call site must pass a mutable binding (T037).
* `atomic_fence` is the barrier that an atomic instruction cannot express.

The `mut expected` write-back needs no branch in the generated code: on
success the value the cell held *was* `expected`, so writing the old value
back is a no-op. The lowering is therefore:

```
%r = cmpxchg ptr %cell, i64 %cmp, i64 %new, <success> <failure>, align 8
%old = extractvalue { i64, i1 } %r, 0
%ok  = extractvalue { i64, i1 } %r, 1
store i64 %old, ptr %expected_slot      ; a plain store: the slot is the caller's own stack
```

### The orderings

| Ordering | Guarantees | Does not guarantee |
| -------- | ---------- | ------------------ |
| `relaxed` | The operation is atomic. The modifications of one atomic object form a single total order (the modification order). | That the operation synchronises with anything. Two relaxed accesses to the same cell are ordered, but nothing outside the cell is ordered with respect to them. |
| `acquire` | Everything the reading thread did *after* the load happens-before everything the writing thread did *before* the matching release, if the load observes that release. | That the load is freshest. It may read an older value in the modification order. |
| `release` | Everything the writing thread did *before* the store happens-before everything the reading thread does *after* the matching acquire, if the acquire observes that store. | That the store is visible immediately. It may sit in a store buffer. |
| `seq_cst` | All `seq_cst` operations participate in a single total order that is consistent with the modification orders and with happens-before. A `seq_cst` load observes the last `seq_cst` store to that object in that total order. | That the order is the one the programmer would have chosen. The total order exists, but it is not "the order the source reads in". |

`seq_cst` is the strongest and the most expensive; on x86-64 a `seq_cst`
store is an `xchg` and everything else is free or already locked.

### The default

**`seq_cst`.** The ordering argument is optional and its absence means
`seq_cst`.

The reason is that a default must not be a correctness trap. A program that
writes `atomic_store(c, v)` and later reads it with `atomic_load(c)` is
race-free under `seq_cst` and is a data race under `relaxed`. A programmer
who wants the cheaper ordering writes it out; a programmer who does not
think about ordering gets the safe one. The cost is bounded and local: on
x86-64 a `seq_cst` load is a plain `mov`, a `seq_cst` read-modify-write is
already a `lock`ed instruction, and only a `seq_cst` *store* costs an
`xchg` instead of a `mov`. On AArch64 a `seq_cst` load is `ldar` and a
`seq_cst` store is `stlr`.

The escape hatch is explicit: `atomic_store(c, v, relaxed)`.

### The compare-and-swap loop

The one structure the phase needs is a Treiber stack, and it is where the
orderings show what they are for. The head is an `AtomicPtr`; the nodes
are ordinary records. The sketch below is the *pattern*; the exact
spelling depends on the pointer type and the field-assignment rules that
phase 11d lands, and on a null pointer constant that arrives with them.

```
fn push(head: AtomicPtr, mut node: Ptr):
    # publish the node: the release on the successful CAS makes the
    # node's contents visible to whoever takes the head with an acquire load
    let mut old = atomic_load(head, acquire)
    node.next = old
    if atomic_compare_exchange(head, old, node, release):
        return
    # lost the race: retry from the top
    push(head, node)
```

and the pop:

```
fn pop(head: AtomicPtr) -> Ptr:
    while true:
        # the acquire load is what licenses the plain read of old.next
        let mut old = atomic_load(head, acquire)
        if old == null:
            return null
        let next = old.next            # a plain read, licensed by the acquire
        if atomic_compare_exchange(head, old, next, relaxed):
            return old
```

The two things this shows:

* The **acquire lives on the load** of the head and the **release lives on
  the successful compare-and-exchange**. There is no `acq_rel` ordering in
  the language, and this is the pattern that does not need one: the load
  that reads the head is what establishes the edge with whoever published
  it, and the plain read of `old.next` is safe because of that edge.
* The **failure path of the compare-and-exchange is `relaxed`**, derived
  from the `release` success ordering. The failure path only means "somebody
  else got there first"; it carries no ordering of its own, and it must
  not, because a failed compare-and-exchange may not have release
  semantics.

The `relaxed` compare-and-exchange in the pop is correct because the
acquire has already happened on the load, and because the compare-and-
exchange only succeeds if the head has not moved since that load. The
value taken out of the stack (`old`) is the one the acquire load observed,
so its contents are visible.

Note that the compare-and-exchange's *success* ordering carries the
release, and the *failure* ordering is derived; the programmer writes one
ordering and gets the pair. The same shape works for a work queue: a
`head` pointer swapped with a `compare_exchange` under `release`, and a
`tail` counter bumped with `atomic_fetch_add`.

### The syntax

Four new keywords and four new type names:

```
relaxed  acquire  release  seq_cst          # values of the builtin type `Ordering`
AtomicInt  AtomicInt32  AtomicBool  AtomicPtr
```

The grammar changes are small:

* `docs/language-reference/grammar.md:19` - the keyword list gains the four
  ordering words.
* `docs/language-reference/grammar.md:66-68` - `type-expr` already accepts
  an `identifier`, so `AtomicInt` and its siblings need no grammar rule; the
  *builtin type* `Ordering` is a name in the same position, and its four
  values are keywords rather than identifiers, so the type checker resolves
  them as constants.

Because `relaxed`, `acquire`, `release` and `seq_cst` become reserved
words, a program that used one of them as a variable name stops compiling.
The language is at 0.3.6 and pre-1.0, so this is cheap now and expensive
later.

The ordering values are keywords rather than identifiers so that they can
be written in a position where a type expression is expected without an
ambiguity: `atomic_load(c, acquire)` cannot be confused with a variable
named `acquire`.

### The lowering: the exact LLVM IR

The code generator emits, for an atomic of LLVM type `T` at a cell pointer
`%p` with natural alignment `A`:

| Operation | IR |
| --------- | -- |
| load | `%v = load atomic T, ptr %p <ord>, align A` |
| store | `store atomic T %val, ptr %p <ord>, align A` |
| exchange | `%v = atomicrmw xchg ptr %p, T %val <ord>, align A` |
| fetch-and-add | `%v = atomicrmw add ptr %p, T %delta <ord>, align A` |
| compare-and-exchange | `%r = cmpxchg ptr %p, T %cmp, T %new <success> <failure>, align A` |
| fence | `fence <ord>` |

`<ord>` is `monotonic` for `relaxed`, `acquire` for `acquire`, `release` for
`release`, `seq_cst` for `seq_cst`. The compare-and-exchange's two
orderings come from the derivation table in the recommendation.

Rules the emitter must respect, all of them checked by LLVM's parser or
verifier:

* A `load atomic` and a `store atomic` **must** carry an explicit
  non-zero `align`. Without it the module does not parse: "atomic load must
  have explicit non-zero alignment".
* A `load atomic` may not use the `release` ordering ("atomic load cannot
  use Release ordering"), and a `store atomic` may not use `acquire`
  ("atomic store cannot use Acquire ordering"). Nexum rejects both at
  compile time.
* A `fence` may not be `monotonic` or `unordered` ("fence cannot be
  monotonic"). Nexum's `atomic_fence(relaxed)` is a compile-time error.
* A `cmpxchg` needs **two** orderings: the success ordering and the
  failure ordering, in that order, then the alignment. A `cmpxchg` with
  one ordering does not parse: "Expected ordering on atomic instruction".
  This is the single most likely thing to get wrong, because the older
  LLVM spelling had only one ordering.
* The `align` on an `atomicrmw` and a `cmpxchg` is optional, but the
  emitter should always write it, so that the alignment in the IR is the
  alignment the language promised.
* The size of an atomic access must be a whole number of bytes: `i1` is
  rejected ("atomic memory access' size must be byte-sized"), which is
  why `AtomicBool` is `i8` and not `i1`.

### The alignment

| Target | `AtomicInt` | `AtomicInt32` | `AtomicBool` | `AtomicPtr` |
| ------ | ----------- | ------------- | ------------ | ----------- |
| `x86_64-pc-windows-msvc` | 8 | 4 | 1 | 8 |
| `x86_64-unknown-linux-gnu` | 8 | 4 | 1 | 8 |
| `x86_64-apple-darwin` | 8 | 4 | 1 | 8 |
| `aarch64-unknown-linux-gnu` | 8 | 4 | 1 | 8 |
| `aarch64-apple-darwin` | 8 | 4 | 1 | 8 |

The alignment is the natural alignment of the type, and the heap layout
already provides it: the cell's payload starts at block offset 32 and the
block is 16-byte aligned, so the payload is 16-byte aligned and every
requirement above is met with room to spare. A cell therefore never needs
a special allocator path.

The requirement is not cosmetic. A misaligned atomic access is a fault on
both supported architectures - a `lock cmpxchg` on a misaligned address
raises `#GP` on x86-64, and an unaligned exclusive access is an alignment
fault on AArch64 - so the language's promise that the cell is naturally
aligned is part of the memory model's correctness.

**Lock-freedom.** Every one of the five cell operations (load, store,
exchange, fetch-and-add, compare-and-exchange) on every one of these four
types must be natively lock-free on every supported target, because the
runtime is freestanding (D012, `docs/language-reference/decisions.md:178`)
and a call to `__atomic_fetch_add_1` would drag the C library back in.
Verified below for the 1-byte and 8-byte cases on x86-64 and AArch64: the
backends emit `xchgb` / `lock xaddb` / `lock cmpxchgb` and
`ldxrb`/`stxrb` / `ldaxrb`/`stlxrb` and call nothing.

### The happens-before rules

These are the rules the language documents. They are the C++ rules, which
are the ones the hardware and LLVM already implement, and they are what the
ordering arguments mean.

1. **Modification order.** For each atomic object, all the modifications of
   that object form a single total order, the modification order, which is
   consistent with happens-before. A `relaxed` operation is in this order;
   it just does not order anything else.
2. **Release/acquire.** A release operation `A` on an object `M`
   *synchronises with* an acquire operation `B` on `M` when `B` takes its
   value from `A`, or from a release sequence headed by `A`. Then `A`
   *happens before* `B`.
3. **Happens-before** is the transitive closure of "sequenced before"
   (program order) and "synchronises with".
4. **The consequence that matters** is the publish rule: everything the
   writing thread did before the release store is visible to the reading
   thread after the acquire load that observed it. The reading thread may
   then read the published data **with a plain, non-atomic load** and that
   is not a data race, because the two accesses are ordered by
   happens-before.
5. **`seq_cst`.** All `seq_cst` operations form a single total order `S`
   that is consistent with the modification orders and with happens-before.
   A `seq_cst` load observes the last `seq_cst` store to that object in
   `S`.

Point 4 is the one the programmer has to know: the acquire load is what
licenses the plain read of the published data. This is the whole content of
the safe subset.

### What the language promises, and what it does not

**Promised.**

* Every atomic operation is atomic: no torn reads, no torn writes, and no
  two operations on the same cell can take effect at the same time.
* The modifications of one cell form a single total order.
* Release/acquire gives happens-before, and therefore the publish rule.
* All `seq_cst` operations form a single total order.
* The alignment of a cell is at least the natural alignment of its type, on
  every supported target.
* Every atomic operation is lock-free: it is a single instruction on every
  supported target and never calls into a library.

**Not promised.**

* **That a data race is impossible.** A race on a non-atomic location
  shared between threads is undefined behaviour. The compiler does not
  detect it, does not insert checks for it, and does not serialise it.
* **That non-atomic accesses are made safe by the atomics nearby.** Only a
  happens-before edge makes a non-atomic access safe, and only the
  documented patterns create one.
* **That `relaxed` accesses are harmless.** A `relaxed` access is atomic,
  but it is not ordered with respect to anything else, so the program that
  uses it for communication without an acquire/release pair somewhere is
  racy.
* **That the atomics give fairness or progress.** A lock-free algorithm may
  starve a thread; the language says nothing about wait-freedom.
* **That an atomic is atomic in the abstract machine when the location is
  thread-local.** LLVM's optimiser is allowed to replace an atomic access
  with a non-atomic one when it can prove the memory cannot be shared. In
  practice the optimiser keeps the atomic whenever the location can
  actually be reached by another thread - verified below - but the
  guarantee the language makes is about the *access*, not about the
  location: only accesses made through the atomic operations are atomic.
* **That the collector is concurrent.** The collector is not thread-safe
  today and will not be in 11e; see below.

A language that promised to prevent all races would be a lie the
implementation cannot keep: the collector, the allocator and the user's
own data structures all touch memory that the compiler cannot see. What
the language can do is make the *atomics* precise, document the patterns
that are safe, and offer a detector.

### The safe subset

The guaranteed-safe patterns are:

1. **The channel.** The standard library's channel is built on a release
   store and an acquire load (or a mutex, which is itself built on them).
   Data sent through a channel is published and observed, so the payload
   transfer is not a data race. A `Str` may be sent through a channel: the
   string's pointer crosses the thread boundary through a release/acquire
   pair, so the receiving thread's copy of it is a properly published
   reference, and the collector traces it the way it traces any other
   rooted reference.
2. **The publish pattern.** Write the data, `atomic_store` the flag with
   `release`, and read the data after an `atomic_load` of the flag with
   `acquire` that observes the store. The plain read of the data is
   licensed by the happens-before edge.
3. **The fence-fence pattern.** The same, with the flag accessed
   `relaxed` and the ordering carried by fences:
   `atomic_store(flag, 1, relaxed); atomic_fence(release);` on the writer
   and `atomic_fence(acquire); if atomic_load(flag, relaxed) == 1 { ... }`
   on the reader. This is the case `atomic_fence` exists for, and it is
   why the fence is in the language.

None of these can be *checked* by the compiler: there is no borrow checker
and no thread-affinity analysis, so a program that also shares the same
data by another route is still racy. The guarantee is a documented rule
that the programmer is responsible for, and the honest support for it is a
race detector in the runtime (a ThreadSanitizer-style shadow-memory
build), not a compile-time check.

### The collector's part

The collector walks two global lists, and neither of them can stay as it
is. The two lists are `@nexum_obj_head` and `@nexum_roots_head`
(`crates/nexum-runtime/src/lib.rs:104-105`).

**The root list has to become per-thread.** This is the urgent one. The
root list is pushed and popped on *every function entry and return* of
*every* thread (`crates/nexum-codegen/src/lib.rs:456` and `:1194`), so it
is the most contended write in the program. If two threads push at the
same time, one frame is lost, and the collector will not see that thread's
roots: it will free objects that are still reachable, and the program will
read freed memory. That is not a leak, it is a use-after-free.

The fix is to make the root list *private to its thread*. The head moves
from a global into the thread control block. Push and pop then touch only
the owning thread's list, and need no synchronisation at all. The
collector, which stops the world, walks the list of thread control blocks
and each block's root list.

**The thread control block list is the one genuinely shared list.** It is
append-only (a thread registers itself once) and it is short-lived
(a thread that exits deregisters). It is protected by a mutex, or by a
compare-and-exchange push with a lock-free list. A mutex is the simpler
choice and the contention is once per thread.

**The object list comes under a heap lock.** The allocator pushes onto it
(`crates/nexum-runtime/src/lib.rs:147`) and the collector's sweep unlinks
from it and frees (`crates/nexum-runtime/src/lib.rs:186`). If two threads
allocate at the same time, the plain load-then-store in the allocator loses
an update and the object is dropped out of the list: it is then never swept
and never freed, a leak, and the collector's mark phase cannot reach it
either. The fix is not an atomic head - it is a **lock around the
allocator and around the collection**, because the collector needs more
than atomicity: it needs a *consistent view* of the heap while it marks
and sweeps. Atomic access gives atomicity, not consistency.

The minimal correct design for 11e is therefore:

* A global **heap lock** (a spin lock with a park, or a futex on the
  POSIX targets) held by the allocator for the duration of the list push
  and the watermark check, and by the collector for the whole of the
  mark-and-sweep.
* A **collection request flag**, an `AtomicBool`, set by a thread that
  cannot allocate and cleared by the collector. This is the one place the
  language's own atomics are used inside the runtime, and it is the
  natural exercise of a `release` store and an `acquire` load.
* The mark bits, `@nexum_live`, `@nexum_since` and `@nexum_threshold`
  become safe automatically, because they are only touched under the lock.

A full stop-the-world protocol - a request flag, a park of every other
thread at its next safepoint, and a collect by the requester - is better
and is where this should end up, but it is not required for 11e and the
lock is the version that can be written and proven correct now.

**The atomic cell has to be scanned atomically.** This is the part that
the language's atomics actually change in the collector. An `AtomicPtr`
cell holds a pointer to a managed object. The collector must read that
pointer to mark the object. If it reads it with a plain load, the read
races with the atomic store that writes it, and in LLVM's model that is
undefined behaviour: the optimiser is entitled to assume that a non-atomic
load is not concurrently modified and may hoist, narrow or eliminate it.
The collector must therefore read the cell with
`load atomic ptr, ptr %p acquire, align 8`.

That means the heap object header has to say "this payload is atomic", and
the collector's scan needs an atomic variant of `nexum_scan_range`
(`crates/nexum-runtime/src/lib.rs:168`). The header is 32 bytes today
(`docs/runtime-abi.md:111-117`) with no spare word, so either the header
grows to 40 bytes (payload at +48, still 16-byte aligned) or the top bit
of `scan_bytes` is stolen as the flag. Growing the header is cleaner; the
runtime ABI is version `v0` and is explicitly allowed to change
(`docs/runtime-abi.md:235-238`).

The same rule applies to the **root frame**. A root entry names an address
and a number of bytes to scan (`docs/runtime-abi.md:147-154`). A root that
holds an `AtomicPtr` handle is an ordinary reference (the handle points at
a cell), so the root entry does not change. What changes is the *cell*,
which is scanned by the collector, and the cell is the thing that needs
the atomic read. So the root frame's layout is unchanged and the object
header is what grows.

**A `Str` shared between threads follows the same rule as any other
pointer.** A `Str` is a managed pointer. The pointer itself has to cross
the thread boundary through a synchronised path - a channel, or a
release/acquire pair - and once it has, the receiving thread's root slot
holds a properly published reference and the collector traces it the way it
traces any other rooted reference. A `Str` copied across without such a
path is a data race on the pointer, and the program is undefined. Nothing
special is needed in the collector for this: the root slot is written by
its owning thread and read by the collector, and the two are ordered by
the stop-the-world.

### The compiler's part

**New MIR instructions.** `crates/nexum-mir/src/lib.rs:47-135` gains:

```
Instr::AtomicLoad  { dest, slot, ty, order }
Instr::AtomicStore { slot, value, ty, order }
Instr::AtomicRmw  { dest, slot, op, value, ty, order }
Instr::AtomicCas  { dest_ok, dest_old, slot, cmp, new, ty, order }
Instr::Fence      { order }
```

with `enum AtomicOrder { Relaxed, Acquire, Release, SeqCst }`. The
`AtomicRmw` operation is `Xchg` or `Add`.

**The MIR passes have to learn about them.** `collect_uses`
(`crates/nexum-mir/src/lib.rs:993-1051`) must count the new operands;
`is_side_effecting` (`:1091-1096`) must treat `AtomicStore`, `AtomicRmw`,
`AtomicCas` and `Fence` as side-effecting, or the dead-code pass will
delete a read-modify-write; the constant folder must not fold an atomic
load; and `inlinable` (`:1108-1135`) must refuse a function containing an
atomic instruction, the way it already refuses one containing a load or a
store.

**The type checker.** `crates/nexum-sema/src/lib.rs:15-32` gains
`Ty::Atomic(AtomicTy)` with `AtomicTy { I64, I32, Bool, Ptr }`. The
`Ordering` type is a builtin type with four constant values. The checker
enforces:

* the ordering argument of a load may not be `release`;
* the ordering argument of a store may not be `acquire`;
* `atomic_fence(relaxed)` is an error;
* `atomic_fetch_add` exists only for the integer atomics;
* `atomic_compare_exchange` takes a `mut` parameter, so the call site must
  pass a mutable binding (T037 already exists).

**The code generator.** `crates/nexum-codegen/src/lib.rs` gains the
emission rules in the table above. The five helper functions that decide
layout and tracing - `llvm_ty` (`:8-20`), `has_refs` (`:23-34`),
`llvm_align` (`:36-53`), `llvm_size` (`:55-68`) and `scan_bytes`
(`:85-99`) - need rows for the atomic types:

| Type | `llvm_ty` (the handle) | `llvm_align` | `llvm_size` | `has_refs` | `scan_bytes` | cell payload |
| ---- | --------------------- | ------------ | ----------- | ---------- | ------------ | ------------ |
| `AtomicInt` | `ptr` | 8 | 8 | true | 8 | `i64` |
| `AtomicInt32` | `ptr` | 8 | 8 | true | 8 | `i32` |
| `AtomicBool` | `ptr` | 8 | 8 | true | 8 | `i8` |
| `AtomicPtr` | `ptr` | 8 | 8 | true | 8 | `ptr` |

The handle is a reference to the cell, so it is rooted like a `List`
handle and the collector scans its 8 bytes to mark the cell; the *cell* is
a heap object whose payload is scanned with the atomic variant of
`nexum_scan_range` when the cell is an `AtomicPtr`. The cell's payload
type and alignment are not derived from `llvm_ty` - they come from the
atomic kind, and they are the ones the atomic instruction uses.

**Does the language need barrier syntax?** No, not for the atomic
operations: the ordering argument *is* the barrier, and it lives in the
LLVM instruction itself, so LLVM's optimiser will not move a non-atomic
access across an acquire load or a release store. The one extra surface is
`atomic_fence`, which maps to `fence <ordering>` and is needed for the
fence-fence message-passing pattern and for a full barrier. LLVM has no
separate compiler-only barrier that the language would need.

**What the optimiser does.** The guarantee that the ordering survives the
optimiser is LLVM's, not the code generator's, and it is worth stating
precisely: LLVM's optimiser honours the ordering argument of an atomic
instruction, so a release store is not moved after the stores that precede
it, and an acquire load is not moved before the loads that follow it. The
one thing the code generator must not do is weaken an atomic into a plain
access, which it cannot do by accident as long as the atomic instructions
are separate MIR instructions and the emitter prints the ordering.

### Changes needed elsewhere

* `crates/nexum-sema/src/lib.rs` - the `Ty` enum, the `Ordering` type, the
  builtin signatures, the new diagnostic codes.
* `crates/nexum-parser/src/lib.rs` and
  `docs/language-reference/grammar.md` - the four ordering keywords.
* `crates/nexum-mir/src/lib.rs` - the five new instructions, the
  `AtomicOrder` enum, and the three passes.
* `crates/nexum-codegen/src/lib.rs` - the emission, the five layout
  helpers, the root registration of the atomic handles.
* `crates/nexum-runtime/src/lib.rs` - `nexum_atomic_new`, the atomic
  variant of `nexum_scan_range`, the object header's `flags` word, the
  per-thread root list, the heap lock, the collection request flag.
* `docs/runtime-abi.md` - the object header layout, the per-thread root
  list, the thread control block, the atomic cell.
* `docs/language-reference/decisions.md` - a new numbered decision for the
  memory model (the document ends at D016).
* `docs/language-reference/grammar.md` - the keywords, and the static
  semantics for the atomics.

## Risks

* **The default is wrong.** If `seq_cst` as the default is too expensive
  for the ML workloads phase 11e exists for, the answer is not to change
  the default to `relaxed` (which turns a performance question into a
  correctness trap) but to make the standard library's channel and mutex
  carry the ordering internally, so the *program* never writes an ordering
  at all. The default then only matters to the programmer who is building
  a data structure.

* **The `acq_rel` gap bites.** A lock-free stack written with the four
  orderings needs the acquire on the head *load* and the release on the
  successful compare-and-exchange. That works, but it is the shape people
  do not expect, and a reviewer who assumes `compare_exchange` is
  acquire-release will be wrong. The mitigation is to write the pattern
  down in the standard library's documentation and to name the compare-and-
  exchange's derived orderings in the reference.

* **The cell on the managed heap is a burden.** Every atomic allocates a
  heap object, and an atomic in a hot loop allocates. The alternatives -
  a cell in a caller-provided arena (D009 is staged) or a cell that is not
  on the managed heap at all (which makes an `AtomicPtr` untraceable and
  therefore unsafe) - are both worse right now. When arenas land, the
  atomic cell should be allocatable in an arena, and the collector's
  atomic scan then applies only to the managed-heap case.

* **The collector's lock serialises allocation.** With a global heap
  lock, every allocation contends. This is the honest cost of a
  stop-the-world, single-threaded collector with a shared heap, and it is
  the reason the stop-the-world protocol - not a better lock - is the
  direction. It is also the reason the per-thread allocation buffer and
  the per-thread nursery belong on the 11e list.

* **The atomic scan of a cell is a performance cliff in the collector.**
  Every `AtomicPtr` cell in the heap is scanned with an atomic load, and
  an atomic load in the collector's inner loop is not free. The number of
  atomic pointer cells should be small, and the collector's scan of a
  non-atomic payload stays a plain load.

* **The four new keywords are a source break.** `relaxed`, `acquire`,
  `release` and `seq_cst` become reserved words. Cheap now, expensive
  later.

* **The language's guarantees are only as good as LLVM's.** Everything in
  this document rests on LLVM's memory model being the one the hardware
  implements. That is the right bet - it is the same bet every language
  that compiles to LLVM makes - but it means the language's memory model is
  LLVM's memory model, and it should say so in the reference rather than
  inventing a second one.

* **The header change breaks the runtime ABI.** The object header grows
  from 32 to 40 or 48 bytes, and every hard-coded `+32` in
  `crates/nexum-runtime/src/lib.rs` has to change. The ABI is `v0` and
  explicitly unstable (`docs/runtime-abi.md:235-238`), so this is allowed,
  but it is a wide edit in a file whose offsets are written as literals.

## Open questions

1. **Should the atomic integer family start at one width or two?**
   *Recommendation:* ship `AtomicInt` (64-bit) and `AtomicInt32`, because
   a 32-bit atomic is what a training loop's counters want and it costs one
   row in the table. The orchestrator should confirm this against what
   phase 11c's numeric tower lands, so that the atomic and the plain
   integer types stay mirror images.

2. **Should the ordering be a keyword or a value of a named type?**
   *Recommendation:* keywords. A named type with four constant values
   (`Ordering.relaxed`) would need a namespace the language does not have,
   and the constant would be indistinguishable from a variable of the same
   type at the call site. The cost is four reserved words.

3. **Should `atomic_fence` be in the language at all in 11e, or only in the
   standard library?**
   *Recommendation:* in the language, because it is the only way to express
   the fence-fence message-passing pattern, and the pattern is the one
   place where the language's guarantee is worth stating. If the
   orchestrator would rather keep the surface small, the fence can be a
   standard-library function over a primitive the language exposes - but
   then the language has to expose the primitive anyway.

4. **Should the compare-and-exchange return a `Bool` and write back
   through a `mut` parameter, or return a record?**
   *Recommendation:* the `Bool` plus `mut expected`. A record would need a
   record per atomic type (`AtomicCasInt`, `AtomicCasBool`, ...), because
   the language's records are nominal and the old value's type varies. The
   `mut` parameter is the mechanism the language already has.

5. **Should the collector's stop-the-world be a lock or a handshake?**
   *Recommendation:* the lock for 11e, the handshake (a request flag plus a
   park at the next safepoint) as the follow-up. The lock is the version
   that can be written and proven correct in the phase; the handshake is
   the version that scales. This is the orchestrator's call, because it
   affects the runtime's shape for the rest of 11e.

6. **Should `AtomicPtr` exist at all in 11e, given that it forces the
   collector to scan atomically?**
   *Recommendation:* yes, and early. A pointer to a managed object that
   another thread holds is the canonical use of an atomic in a language
   with a collector, and discovering late that it cannot be done safely
   would be a redesign. The cost is the atomic variant of the collector's
   scan and the `flags` word in the object header.

7. **Should a race detector be part of 11e?**
   *Recommendation:* a runtime build option, not a language feature. A
   ThreadSanitizer-style shadow memory in the collector's allocator is
   enough to catch the races that the documented patterns do not prevent,
   and it is the only honest answer to "the language does not prevent
   races". It does not change the language at all, and it can be added
   after 11e without changing the memory model.

8. **Should the language ever add an `acq_rel` ordering?**
   *Recommendation:* not in 11e, but the decision should be recorded now so
   that the derivation table has a place for it. If it is added, the
   compare-and-exchange's success ordering becomes `acq_rel` and the
   failure ordering `acquire`, exactly as clang emits for C++'s
   `memory_order_acq_rel` / `memory_order_acquire` pair.

## Verification

Everything below was run against the LLVM in
`C:\Program Files\LLVM\bin\clang.exe`, version 23.1.3, on the machine this
was written on. The code generator emits textual LLVM IR
(`crates/nexum-codegen/src/lib.rs`), so the textual form is what the
implementation has to get right, and that is what was checked.

**1. What clang emits for the equivalent C11 operations** - the reference
the mapping is copied from:

```
$ clang -S -emit-llvm -O1 -o ref.ll ref.c
  %2 = load atomic i64, ptr %0 monotonic, align 8
  %2 = load atomic i64, ptr %0 acquire, align 8
  %2 = load atomic i64, ptr %0 seq_cst, align 8
  store atomic i64 %1, ptr %0 monotonic, align 8
  store atomic i64 %1, ptr %0 release, align 8
  store atomic i64 %1, ptr %0 seq_cst, align 8
  %3 = atomicrmw xchg ptr %0, i64 %1 monotonic, align 8
  %3 = atomicrmw xchg ptr %0, i64 %1 acq_rel, align 8
  %3 = atomicrmw xchg ptr %0, i64 %1 seq_cst, align 8
  %3 = atomicrmw add ptr %0, i64 %1 monotonic, align 8
  %3 = atomicrmw add ptr %0, i64 %1 acq_rel, align 8
  %3 = atomicrmw add ptr %0, i64 %1 seq_cst, align 8
  fence acquire
  fence release
  fence seq_cst
  %2 = load atomic ptr, ptr %0 acquire, align 8
  store atomic ptr %1, ptr %0 release, align 8
  %2 = load atomic i8, ptr %0 monotonic, align 1
  store atomic i8 %3, ptr %0 monotonic, align 1
```

**2. The compare-and-exchange's two orderings**, and the derivation table
checked against clang for each of the four language orderings:

```
$ clang -S -emit-llvm -O1 -o ref_cas.ll ref_cas.c        # C11 success/failure
  %5 = cmpxchg ptr %0, i64 %4, i64 %2 release monotonic, align 8   # release/relaxed
  %5 = cmpxchg ptr %0, i64 %4, i64 %2 acq_rel acquire, align 8     # acq_rel/acquire
  %5 = cmpxchg ptr %0, i64 %4, i64 %2 seq_cst seq_cst, align 8     # seq_cst/seq_cst
  %5 = cmpxchg ptr %0, i64 %4, i64 %2 monotonic monotonic, align 8   # relaxed/relaxed
  %5 = cmpxchg ptr %0, i64 %4, i64 %2 acquire acquire, align 8       # acquire/acquire
```

The derivation table in the recommendation is exactly these five lines
minus the `acq_rel` case.

**3. The constraints the emitter has to respect.** Each line is a
separate module compiled with `clang -c`:

```
rmw_no_align     OK
rmw_align1       OK
cas_no_align     OK
cas_rel_acq      OK          (release success, acquire failure)
cas_rel_rel      FAIL  invalid cmpxchg failure ordering
cas_unordered    FAIL  invalid cmpxchg success ordering
load_no_align    FAIL  atomic load must have explicit non-zero alignment
load_rel         FAIL  atomic load cannot use Release ordering
store_no_align   FAIL  atomic store must have explicit non-zero alignment
store_acq        FAIL  atomic store cannot use Acquire ordering
load_i1          FAIL  invalid LLVM IR input: atomic memory access' size must be byte-sized
fence_mon        FAIL  fence cannot be monotonic
fence_unord      FAIL  fence cannot be unordered
fence_acqrel     OK
```

and the `cmpxchg` spelling, which is the one most likely to be got wrong:

```
cmpxchg ptr %p, i64 %c, i64 %n acq_rel              FAIL  Expected ordering on atomic instruction
cmpxchg ptr %p, i64 %c, i64 %n acq_rel acquire      OK
```

A `cmpxchg` needs the success ordering *and* the failure ordering. The
older single-ordering spelling does not parse.

**4. What the hardware does.** The same operations compiled for both
architectures:

```
              x86-64                                    AArch64
load    relaxed    movq (%rdi), %rcx                    ldr  xzr, [x0]
load    acquire    movq (%rdi), %rcx                    ldar xzr, [x0]
load    seq_cst    movq (%rdi), %rcx                    ldar xzr, [x0]
store   relaxed    movq %rsi, (%rdi)                    str  x1, [x8]
store   release    movq %rsi, (%rdi)                    stlr x1, [x8]
store   seq_cst    xchgq %rsi, (%rdi)                   stlr x1, [x8]
xchg    seq_cst    xchgq %rsi, (%rdi)                   ldaxr/stlxr
fadd    seq_cst    lock addq %rsi, (%rdi)               ldaxr/stlxr
cas     seq_cst    lock cmpxchgq %rsi, (%rdi)           ldaxr/stlxr
fence   acquire    (nothing)                            dmb ishld
fence   release    (nothing)                            dmb ish
fence   seq_cst    lock orl $0, -64(%rsp)               dmb ish
```

**5. The byte-sized atomics are natively lock-free**, which is what keeps
the runtime freestanding (D012) - no `__atomic_*` call is emitted:

```
x86-64:  xchgb %al, (%rdi) ; lock xaddb %al, (%rdi) ; lock cmpxchgb %dl, (%rdi)
AArch64: ldxrb/stxrb ; ldxrb/stxrb ; ldaxrb/stlxrb
```

**6. The optimiser keeps the ordering.** The message-passing pattern
survives `-O2` with its fences intact and its plain store before the
release fence:

```
define void @writer(i64 %x) {
  store i64 %x, ptr @data, align 8
  store atomic i64 1, ptr @flag monotonic, align 8
  fence release
  ret void
}
define i64 @reader() {
  fence acquire
  %f = load atomic i64, ptr @flag monotonic, align 8
  %d = load i64, ptr @data, align 8
  ret i64 %d
}
```

**7. What the optimiser does to an atomic whose location is shared.** An
atomic store to a location whose address escapes is kept:

```
define void @escapes() {
  call void @opaque(ptr nonnull %p)
  store atomic i64 3, ptr %p monotonic, align 8
  ret void
}
```

and an atomic to an `internal` global is kept as well. The cases that
were removed were removed at the value level - the loaded value was
already known, so the access was dead - not by weakening the atomicity
of an access whose value was not known.

**Not verified.** No Nexum program was built and run: this is a research
deliverable and the language has no atomics yet, so there is nothing to
execute. The AArch64 output was produced by cross-compiling to assembly
with `clang -target aarch64-unknown-linux-gnu -S`; it was not run on
AArch64 hardware. The Windows target's data layout was taken from
`crates/nexum-target/src/lib.rs:87-89`; the x86-64 instructions are the
same, but the Windows assembler was not exercised.
