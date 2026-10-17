# Nexum Runtime ABI (Bootstrap)

Status: **Phase 4 milestone**. This document specifies the runtime ABI the
compiler targets today. It is the "Runtime ABI note" required by Phase 4 of
the architecture spec: native value representations, managed-heap object
layout, root frames, safepoints, and the staged arena / cleanup model.

## 1. Native (unmanaged) values

These are values the compiler passes and stores by value. None of them
participate in collection.

| Nexum type | LLVM type | Representation                              |
| ---------- | --------- | ------------------------------------------- |
| `Int`      | `i64`     | two's-complement (wrapping arithmetic)      |
| `Float`    | `double`  | IEEE 754                                    |
| `Bool`     | `i1`      | 0 / 1                                       |
| `Str`      | `ptr`     | pointer to a NUL-terminated byte buffer     |
| `Unit`     | `void`    | —                                           |
| records    | `%name`   | LLVM struct, field order = declaration order |
| arrays     | `[N x T]` | inline, value type                          |

`Str` is the **only** managed type in the bootstrap. A `Str` value is a
pointer to a NUL-terminated UTF-8 buffer. Two representations share this
type:

* **static literals** — pointers into module-private global constants. They
  are immortal and never participate in collection.
* **heap strings** — pointers into blocks allocated by `nexum_alloc`
  (produced by `concat`). They are collected.

The compiler distinguishes the two only through the managed-heap range check
performed by the collector; no type-level tag is required because the heap
is a single, contiguous-by-blocks region managed by `nexum_alloc`.

## 2. Managed heap

* **Allocator**: `nexum_alloc(size)` rounds the requested payload to 16 bytes,
  adds a 32-byte header, and obtains memory from `malloc`. It links the new
  block at the head of a global singly-linked list (`@nexum_obj_head`).
* **Object block layout** (all offsets in bytes):
  * `+0`  `size` — payload size rounded up to 16 (i64)
  * `+8`  `mark` — 0 = unmarked, 1 = marked (i64)
  * `+16` `next` — next object in the global list (ptr)
  * `+24` reserved (i64)
  * `+32` **payload** — the bytes the program sees, 16-byte aligned, always
    NUL-terminated for strings
* **Collector**: precise, non-moving, single-threaded mark-sweep. A
  collection runs only inside `nexum_alloc` (when the allocation watermark
  exceeds `@nexum_threshold`, currently 64 KiB) or when a program calls the
  `collect()` builtin.
* **Mark phase**: for every root slot of every registered root frame, read
  the slot's current contents and, if non-null, find the object whose payload
  pointer matches and set its mark bit.
* **Sweep phase**: walk the global list; unmarked blocks are unlinked, their
  size subtracted from `@nexum_live`, and `free`d; marked blocks have their
  mark bit reset to 0.
* **Statistics**: `heap_used()` returns `@nexum_live` (sum of rounded payload
  sizes of live heap objects).

## 3. Roots and safepoints

* **Root frame**: a compiler-generated struct on a function's stack:
  * `+0`  `prev` (ptr) — previous frame
  * `+8`  `slots` (ptr) — pointer to an array of root slots
  * `+16` `count` (i64) — number of slots
* A **root slot** is an alloca of type `ptr` (a local `Str` variable or a
  compiler-managed temporary). The frame stores the *addresses* of these
  slots, so the collector reads their (possibly changing) contents at
  collection time.
* **Registration**: a function with at least one root slot allocates the frame
  and its slot array in its entry block, zeroes every slot, and calls
  `nexum_roots_push(frame)`; it calls `nexum_roots_pop()` before every
  `return`. Functions with no `Str` values pay nothing.
* **Temporary roots**: a call whose result type is `Str` spills the result
  into a dedicated root slot immediately after the call, before any other
  allocation can run. This is what keeps nested constructions such as
  `concat(build(n - 1), concat("y", "z"))` sound.
* **Safepoint**: every allocation (`nexum_alloc`) and the `collect()` builtin
  are safepoints. Because collection only runs there and all live `Str`
  values reside in registered slots at those points, the root set is always
  precise. No stack maps or separate safepoint polling are needed in the
  bootstrap.
* **Non-moving**: the collector never moves objects, so values held in
  registers across a collection stay valid as long as the object is reachable
  from a root slot.

## 4. Arenas (staged)

Arenas (bulk-lifetime regions) are **specified but not yet implemented** in
the bootstrap. The intended model:

* an arena is a bump-allocated region with a single release point;
* references into an arena must not escape the region (enforced statically or
  via explicit scoped APIs);
* release is LIFO with respect to nested scopes and runs on early returns and
  error paths.

The implementation is deferred until a scoped language surface exists; the
decision is recorded as D009 in `decisions.md`.

## 5. Deterministic cleanup (staged)

Compiler-supported deterministic cleanup (RAII-style, running on scope exit,
early returns and panics) is specified but not yet implemented. The intended
order is LIFO within a scope. Recorded as D009.

## 6. Current restrictions

* `Str` values may not be stored in record fields or array elements yet: the
  collector has no type descriptors, so it cannot find pointers inside
  aggregates. This is enforced statically (T039). Strings are fully usable in
  locals, parameters and returns.
* Only `Str` is managed; records and arrays are value types and never
  allocate on the heap.
* Single-threaded only.
","path">
C:\Nexum\nexum\docs\runtime-abi.md