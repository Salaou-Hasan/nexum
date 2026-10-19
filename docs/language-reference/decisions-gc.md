# Decision record: GC tracing model and dynamic collections (Phase 10)

Status: **decided and implemented** (Phase 10b).

## Problem

The bootstrap collector traces only *individual* `Str` local slots that the
compiler registers in a root frame. Two consequences follow:

1. References held **inside aggregates** (record fields, array elements) are
   invisible to the collector, which is why `Str` in a record or array is
   rejected (T039).
2. A growable collection of tokens (records containing `Str`) cannot be
   represented at all, which blocks the self-hosted lexer/parser.

The collector also has no way to discover pointers held inside heap objects,
so a heap-allocated collection could not keep its elements alive.

## Options considered

### A. Precise tracing with runtime type descriptors

Every record/array type would get a runtime descriptor listing which fields
are references; the collector would use the descriptor to find interior
pointers, and the stack map would carry per-root metadata. This is the
direction the architecture spec's memory model points at ("keep managed
references distinguishable from raw pointers in HIR/MIR and, where necessary,
in runtime metadata").

*Advantages*: exact reclamation, no false retention, enables moving collectors
later.

*Disadvantages*: it is a whole subsystem — a runtime type registry, a
descriptor emitted for every record and array type, descriptor-aware marking,
and per-root metadata in the frame. The LLVM GC documentation makes the same
observation: the type system is "not suitable for conveying such information
through the compiler", so the binary interface has to be designed and
versioned separately. The spec itself lists type descriptors as open work
(D010), i.e. a phase of its own.

### B. Conservative scanning of the whole machine stack (Boehm-style)

Scan every word of the stack for values that look like heap pointers.

*Advantages*: no compiler support needed at all.

*Disadvantages*: needs stack-bounds discovery and frame walking; the LLVM GC
documentation warns that "some aggressive compiler transformations can break
conservative garbage collectors"; every integer that happens to equal a heap
address is retained forever. It also throws away information the compiler
already has — Nexum knows exactly which slots hold references.

### C. Mostly-precise tracing (chosen)

The compiler knows, for every local, whether it holds a reference and how
large it is. So roots are described **precisely**; the only imprecision is
*inside* an aggregate, whose words are scanned conservatively.

*Advantages*:

* Extends the mechanism that already exists. The root frame is a shadow stack
  in the sense of the LLVM GC documentation's "shadow-stack" strategy: a
  linked list of frames, pushed in the prologue and popped before every
  return. Only the *description* of a root changes, from "one pointer" to
  "(address, bytes to scan)".
* No type-descriptor subsystem is required.
* It is **sound**: every live interior reference is an aligned pointer inside a
  scanned range, so a live object is never collected.
* The imprecision is bounded and documentable: an integer that happens to
  equal the address of a live object keeps that one object alive for one
  extra cycle. That is a delayed free, never a crash and never a wrong
  program result.

*Disadvantages*: an aggregate is scanned word-by-word, so a numeric field may
be mistaken for a pointer (false retention only). Records and arrays that
contain no references are *not* scanned at all, because the compiler knows
their field types — so the common case stays precise.

The LLVM GC documentation notes that a conservative collector cannot move
objects. Nexum's collector was already non-moving, so this choice costs
nothing that was not already given up.

### Why stack aggregates must be scanned too

The instruction to follow up on this decision was explicit: "scanning marked
heap objects alone is not sufficient if live references can exist in
unscanned stack aggregates." Both halves are therefore covered:

* **Stack side**: every local whose type contains a reference is registered
  in the root frame with the number of bytes to scan. A `Str` local scans 8
  bytes; a record or array containing references scans its whole size; a
  numeric local scans 0 bytes and is not registered at all.
* **Heap side**: every heap object carries a `scan_bytes` field. Strings are
  leaves (`scan_bytes = 0`), so their *contents* are never mistaken for
  pointers. A collection's element buffer carries the number of bytes that
  hold references.

Marking is a fixpoint: marking an object can reveal further objects, so the
collector alternates between tracing the heap and sweeping until no new
object is marked. "New" is the operative word: the mark phase reports
whether it marked an object *for the first time*, and the trace repeats its
pass over the heap while that is still happening. An object that is only
reachable through another object (a `List` element buffer, for example) is
discovered after its referrer, so a trace that stopped after one pass would
leave it - and everything it points to - unmarked and reclaimable.

## Dynamic collection design

`List` is a new built-in dynamic collection, deliberately separate from the
existing fixed-size arrays (D005 is unchanged).

* Representation: a pointer to a heap object; the object holds the element
  count, the capacity, the element size, and a pointer to a separate element
  buffer.
* The element buffer is a *separate* heap object so that growing the list
  does not move the list object itself — the list pointer held by user code
  and registered as a root stays valid across growth.
* Growth doubles the capacity and allocates a new buffer, copying the
  elements and then updating the list's buffer pointer. No allocation happens
  between allocating the new buffer and publishing it, so the new buffer is
  never reachable-but-unrooted at a collection point.
* Element type: fixed by the first `list_push` (like Rust's `Vec::new()`
  inference). A later push of an incompatible type is a compile error.
* Element copy: the compiler materialises the element into a stack temporary
  and passes its address, so the runtime stays monomorphic and can handle
  records as well as scalars.
* Bounds checks on `list_get`/`list_set` are runtime checks that panic with
  `index out of bounds`, matching array semantics.

## Temporary limitations

* The element type is fixed by the first push; there is no widening.
* Element scanning inside a collection is conservative: a numeric field of a
  record element may be mistaken for a pointer.
* A list's element type cannot be established without a push, so a list
  cannot be read before it is written to.
* `List` cannot be nested in a fixed-size array yet, because array element
  types are fixed at compile time and `List` carries a runtime element type.
* The list element type is inferred per binding in the block where the push
  happens; a list declared in an outer block and first pushed in an inner
  block does not carry its element type back out.
* There is no `List` of `List` special case; nested lists work, but the inner
  list is traced through the outer element buffer.
* The old T039 restriction (managed `Str` in records and arrays) is
  withdrawn: the collector traces references inside aggregates.
