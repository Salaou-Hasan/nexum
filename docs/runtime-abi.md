# Nexum Runtime ABI (Bootstrap)

Status: **Phase 4/10 milestone**. This document specifies the runtime ABI the
compiler targets today. It is the "Runtime ABI note" required by Phase 4 of
the architecture spec: native value representations, managed-heap object
layouts, root frames, safepoints, and the staged arena / cleanup model.

The tracing model is decided in
`docs/language-reference/decisions-gc.md`; this document pins the layouts.

## 0. The runtime is freestanding

A program built by Nexum depends on the kernel and on nothing else. The
runtime does not link the C library on the POSIX targets: the string
routines, the file routines and the system call layer are part of the
runtime, and the system call layer is inline assembly in the generated IR
(D012, D013 in `docs/language-reference/decisions.md`).

The parts that still come from the C library, and when they go:

| Piece | Today | Phase |
| ----- | ----- | ----- |
| `strlen`, `memcpy`, `strcmp` | the runtime's own | done |
| file reading and writing | the system calls | done (POSIX) |
| `malloc`, `free` | the C library | 11a.2 |
| `printf`, `fputs`, `fputc` | the C library | 11a.2 |
| `exit` | the C library | 11a.2 |
| the file routines on Windows | the C library | 11a.2 (Windows has no direct system call instruction) |

The Windows build needs its own system call stubs: its kernel interface
lives behind `ntdll`, so the stubs have to be generated rather than
assembled.

### The system call layer

The layer lives in `crates/nexum-runtime/src/syscalls.rs` and covers Linux
and macOS on x86-64 and AArch64. The wrappers are inline assembly in the
generated IR, and three details of it are worth writing down, because each
of them is silent when it is wrong:

* **The arguments are pinned to their registers.** The constraint string
  names the register each argument goes in, so the assembly body never
  refers to an operand by number. The numbering of the operands in an
  assembly block counts the output as operand zero, and a mistake there
  moves the wrong value into the wrong register without any diagnostic.
* **The system call number is an immediate.** In AT&T syntax `movq 1, %rax`
  reads the 64-bit value at the absolute address 1; an immediate needs the
  sigil, and a literal dollar is written twice in an assembly block because
  a single one starts an operand reference.
* **The stack is aligned for the call.** The system call instruction wants
  a 16-byte aligned stack, and a function without a frame is entered with
  the stack at 8 modulo 16. The wrapper aligns it and restores it.

The numbers themselves are per system, and they do not match:

| Call | Linux | Darwin |
| ---- | ----- | ------ |
| `read` | 0 | 3 |
| `write` | 1 | 4 |
| `open` | 2 | 5 |
| `close` | 3 | 6 |
| `lseek` | 8 | 199 |
| `mmap` | 9 | 197 |
| `munmap` | 11 | 198 |
| `exit` | 60 | 1 |

The conventions differ as well: on x86-64 the number goes in `rax` and the
arguments in `rdi`, `rsi`, `rdx`, `r10`, `r8`, `r9`; on AArch64 the number
goes in `x8` (Linux) or `x16` (Darwin) and the arguments in `x0` to `x5`,
and the result comes back in `x0` - the same register the first argument
went in, which is why the arguments are staged in `x9` to `x14` and moved
into place. The instruction is `svc #0` on Linux and `svc #0x80` on Darwin.

## 1. Native (unmanaged) values

These are values the compiler passes and stores by value. None of them
participate in collection.

| Nexum type | LLVM type | Representation                              |
| ---------- | --------- | ------------------------------------------- |
| `Int`      | `i64`     | two's-complement (wrapping arithmetic)      |
| `Float`    | `double`  | IEEE 754                                    |
| `Bool`     | `i1`      | 0 / 1                                       |
| `Str`      | `ptr`     | pointer to a NUL-terminated byte buffer     |
| `Unit`     | `void`    | -                                           |
| records    | `%name`   | LLVM struct, field order = declaration order |
| arrays     | `[N x T]` | inline, value type                          |
| `List`     | `ptr`     | pointer to a heap object (see section 5)     |

`Str` is the only *managed* type in the bootstrap. A `Str` value is a
pointer to a NUL-terminated UTF-8 byte buffer. Two representations share
this type:

* **static literals** - pointers into module-private global constants. They
  are immortal and never participate in collection.
* **heap strings** - pointers into blocks allocated by `nexum_alloc`
  (produced by `concat`). They are collected.

Strings are *byte* strings: `s[i]` yields one byte and `slice(s, lo, hi)`
yields the byte range `[lo, hi)`. Both may split a multi-byte UTF-8
character; lengths are byte counts.

## 2. Managed heap

* **Allocator**: `nexum_alloc(size)` rounds the requested payload to 16
  bytes, adds a 32-byte header, and obtains memory from `malloc`. It links
  the new block at the head of a global singly-linked list
  (`@nexum_obj_head`). `nexum_alloc_scanned(size, scan_bytes)` additionally
  records how many payload bytes hold references the collector must trace.
  **Both return the payload pointer (header + 32), not the header.**
* **Object block layout** (all offsets in bytes):
  * `+0`  `size` - payload size rounded up to 16 (i64)
  * `+8`  `mark` - 0 = unmarked, 1 = marked (i64)
  * `+16` `next` - next object in the global list (ptr)
  * `+24` `scan_bytes` - payload bytes to scan for interior references (i64)
  * `+32` **payload** - the bytes the program sees, 16-byte aligned, always
    NUL-terminated for strings
* **Collector**: non-moving, single-threaded mark-sweep. A collection runs
  only inside an allocation (when the allocation watermark exceeds
  `@nexum_threshold`, currently 64 KiB) or when a program calls the
  `collect()` builtin.
* **Mark phase**: for every root entry of every registered frame, scan the
  described byte range for candidate pointers and mark the objects they
  address. Then trace the heap to a fixpoint, because a marked object can
  reference further objects. The trace repeats its pass over the heap until a
  pass marks nothing new; `nexum_mark_ptr` therefore reports **"this object
  was newly marked"** (not "this object was already marked"), which is what
  drives the fixpoint. The distinction matters: an object that is only
  reachable *through* another object (a `List` element buffer, for example)
  is discovered after its referrer, so a single pass would leave it - and
  everything it points to - unmarked and reclaimable.
* **Sweep phase**: walk the global list; unmarked blocks are unlinked, their
  size subtracted from `@nexum_live`, and `free`d; marked blocks have their
  mark bit reset to 0.
* **Statistics**: `heap_used()` returns `@nexum_live` (sum of rounded payload
  sizes of live heap objects).

Strings are **leaves** (`scan_bytes = 0`): their contents are never mistaken
for pointers.

## 3. Roots and safepoints

* **Root frame**: a compiler-generated struct on a function's stack:
  * `+0`  `prev` (ptr) - previous frame
  * `+8`  `entries` (ptr) - pointer to an array of root entries
  * `+16` `count` (i64) - number of entries
* **Root entry**: a pair describing one root:
  * `+0`  `addr` (ptr) - address of the root slot
  * `+8`  `scan_bytes` (i64) - bytes to scan; 0 means "not a root"
* The compiler decides what to register: a local or temporary whose type can
  hold a reference is registered with the number of bytes the collector must
  scan - 8 for a `Str` or `List`, the whole size for a record or array that
  contains references, 0 for everything else (a numeric local is not
  registered at all).
* An aggregate value that can hold references is **spilled into a rooted
  slot** right after it is produced, because while it only lives in a
  register a collection triggered by a later allocation could free the strings
  it points to.
* **Registration**: a function with at least one root allocates the frame
  and its entry array in its entry block, zeroes every root slot, and calls
  `nexum_roots_push(frame)`; it calls `nexum_roots_pop()` before every
  `return`.
* **Temporary roots**: a call whose result holds references spills the
  result into a dedicated root slot immediately after the call, before any
  other allocation can run. This is what keeps nested constructions such as
  `concat(build(n - 1), concat("y", "z"))` sound.
* **Safepoint**: every allocation and the `collect()` builtin are safepoints.
  Because collection only runs there and all live references reside in
  registered slots at those points, the root set is always complete.
* **Non-moving**: the collector never moves objects, so values held in
  registers across a collection stay valid as long as the object is reachable
  from a root.

## 4. Soundness argument

A reference is never reachable only from a register:

* a heap-allocating expression's result is spilled into a root slot
  immediately after the call;
* a reference copied out of an aggregate is still reachable through the
  aggregate's own root slot, and aggregates are not overwritten between the
  copy and its use;
* literals are immortal.

Therefore every live interior reference is inside a scanned range, so the
collector can never free a reachable object. The converse is not exact: a
non-pointer word that happens to equal a live heap address keeps that object
alive one cycle longer. That is a delayed free, never a wrong result.

## 5. Lists

* A list is a heap object whose payload is
  `[0]` element buffer (ptr), `[8]` length (i64), `[16]` capacity in bytes
  (i64), `[24]` element size (i64), `[32]` element scan size (i64). Only the
  buffer pointer is a reference, so the list object scans 8 bytes.
* The element buffer is a **separate** heap object, so growing the list
  never moves the list object and the handle held by user code stays valid.
  The buffer's `scan_bytes` is the number of bytes that hold references.
* Growth doubles the capacity, allocates a new buffer, copies the elements
  and then publishes it. No allocation happens between allocating the new
  buffer and publishing it, so the new buffer is never reachable-but-unrooted
  at a collection point.
* The list handle is the payload pointer of the list object.

## 6. Arenas (staged)

Arenas (bulk-lifetime regions) are **specified but not yet implemented**. The
intended model:

* an arena is a bump-allocated region with a single release point;
* references into an arena must not escape the region (enforced statically or
  via explicit scoped APIs);
* release is LIFO with respect to nested scopes and runs on early returns and
  error paths.

The implementation is deferred until a scoped language surface exists; the
decision is recorded as D009 in `decisions.md`.

## 7. Deterministic cleanup (staged)

Compiler-supported deterministic cleanup (RAII-style, running on scope exit,
early returns and panics) is specified but not yet implemented. The intended
order is LIFO within a scope. Recorded as D009.

## 8. Current restrictions

* `Str` values may be stored in record fields and array elements: the
  collector traces references inside aggregates.
* Only `Str` and `List` are managed; records and arrays are value types and
  never allocate on the heap.
* Single-threaded only.
* Binary I/O is not supported: `read_file` reads bytes but strings are
  NUL-terminated, so a NUL byte truncates the value.

## 9. Stability policy (Phase 9)

The runtime ABI is versioned. Breaking changes require a major version bump
and a migration note. The bootstrap ABI (this document) is `v0`; it is
allowed to change without notice until a `v1` is declared. Programs must be
recompiled against the matching runtime.
