# The allocator that replaces `malloc` in the runtime

**Question.** What allocator does the Nexum runtime need, and what should it
be, so that a generated program depends on nothing but the kernel? The
collector (`nexum_alloc`, `nexum_alloc_scanned`, the sweep) is the allocator's
only client today; the design has to say exactly what the collector asks for,
how freed memory is recycled, how a free list can live inside the collector's
own object header without corrupting it, how a chunk is mapped through the
system call layer that now exists, what the Windows build does, and which
runtime functions change.

**Summary.** The recommendation is a **size-class allocator over `mmap`ed
chunks with a bump pointer and one free list per size class**: 255 exact
16-byte classes for blocks up to 4096 bytes, four doubling classes up to
65536, and a dedicated `mmap` for anything larger. Nothing is ever split or
merged, so the allocator needs no loop, no coalescing and no tree — it is
about two hundred lines of LLVM IR text. The free list reuses the collector's
`+16` word, and that is safe under four invariants that the sweep already
satisfies. Two things have to be fixed before the allocator can work: the
existing `nexum_sys_mmap` wrapper puts the file descriptor and the offset in
the wrong argument registers, and the page size is 16 KiB — not 4 KiB — on
Apple Silicon. The collector's object layout does not change at all, and
the exact set of runtime functions that change is five, in two files, plus
one new file.

## Recommendation

1. **The allocator is a size-class allocator over `mmap`ed chunks.** A bump
   pointer carves blocks out of the current chunk; a block that dies goes on
   the free list of its size class; a request takes the head of its class's
   list, or is carved from the bump. This is the only one of the candidates
   in "The design" that is O(1) per allocation, recycles freed memory, and
   needs no loop to implement.

2. **The collector's object header *is* the allocator's block header.** The
   allocator writes exactly one word into a block it manages: the free-list
   link at `+16`, the slot the collector uses for the next object in the
   global list. The two uses never overlap, because a block is either a live
   object or a free block, never both. This costs zero bytes per object and
   leaves the documented object layout (`docs/runtime-abi.md` §2) untouched.
   The four invariants that make it safe are listed in "The `+16` slot".

3. **The class granularity is 16 bytes and the fine range runs from 32 to
   4096.** This is not arbitrary: the collector always asks for
   `32 + round16(payload)`, so every request is a multiple of 16 offset by
   32, and a 16-byte-granular class fits such a request *exactly*. In the
   fine range the allocator therefore has **zero internal fragmentation** —
   a 64-byte request gets a 64-byte block, not a 128-byte one. Above 4096
   the classes double to 65536, and above that a block gets its own mapping
   and its own size, so the waste there is only the page rounding.

4. **Nothing is ever split and nothing is ever merged.** A block's class is
   derived from the size the collector records in its header, so a block's
   class must never change; splitting a block would change it. Coalescing is
   impossible by construction (a class holds blocks of exactly one size) and
   is not needed. This is the rule that makes the allocator small enough to
   write in IR text.

5. **The allocator's interface is two functions and one shared helper:**
   `nexum_heap_alloc(total) -> ptr`, `nexum_heap_free(ptr, total)`, and
   `nexum_size_class(total) -> class`. The collector reconstructs `total`
   from the payload size it stored in the header, so the allocator never has
   to read the collector's fields.

6. **The `nexum_sys_mmap` wrapper must be fixed before any of this works.**
   Today it passes `-1` in the *flags* register and `0` in the *descriptor*
   register — the descriptor is stdin and the flags are all ones. See "The
   system call layer" for the exact change.

7. **The page size is a per-target constant, and it is 16384 on macOS/AArch64.**
   macOS on Apple Silicon has 16 KiB pages, and its `munmap` rejects a length
   that is not a multiple of the page size. Using 4096 there leaks every
   large block.

8. **Freed memory is recycled in the process; it is not handed back to the
   kernel** — with one exception: a block larger than the biggest class
   (65536) has its own mapping, and that mapping is unmapped when the block
   dies, so the kernel gets it back immediately. Chunks are never unmapped
   during the program's life.

9. **The failure mode is the one that exists today.** A failed mapping
   returns null; the collector turns null into `nexum_oom()`, which prints
   `out of memory` on stderr and exits with status 1. (Recommended extra: a
   collection and one retry before giving up, since a collection can free
   enough to make the allocation fit.)

10. **The Windows build uses `VirtualAlloc`/`VirtualFree` for the raw
    blocks**, which is the same allocator with a different raw-block layer.
    `VirtualAlloc` is a kernel32 export, not the C library, so the Windows
    program stops depending on the CRT's heap even though it still uses the
    CRT for stdio. If the orchestrator would rather not touch the Windows
    build in this slice, the fallback is to keep `malloc`/`free` there and
    disable the free lists — the collector's code is identical either way.

11. **The exact set of runtime functions that change** is in "The set of
    functions that change": `runtime_ir`, `nexum_alloc_scanned` and
    `nexum_sweep` in `lib.rs`, and `wrappers` and `Syscalls::for_os` in
    `syscalls.rs`. One new file is added, and the collector's object layout,
    its accounting and its algorithms do not change at all.

## What the code looks like today

The facts this recommendation depends on, with the places they are written
down.

### The collector

* `crates/nexum-runtime/src/lib.rs:147` — `nexum_alloc_scanned(payload,
  scan_bytes)`. It rounds the payload up to 16 (`%sz0 = add i64 %payload, 15;
  %sz = and i64 %sz0, -16`), adds the 32-byte header (`%total = add i64 %sz,
  32`), calls `nexum_gc_maybe()`, calls `@malloc(%total)`, and on null calls
  `nexum_oom()`. It then writes the header: `+0` = the rounded payload size,
  `+8` = 0 (the mark), `+16` = the current head of the global object list,
  `+24` = the scan length; it links the object in by storing the old head
  into `+16` and the new object into `@nexum_obj_head`; it adds `%sz` to
  `@nexum_live` and `@nexum_since`; and it returns the **payload** pointer,
  `p + 32`.
* `crates/nexum-runtime/src/lib.rs:150` — `nexum_alloc(payload)` is
  `nexum_alloc_scanned(payload, 0)`.
* `crates/nexum-runtime/src/lib.rs:186` — `nexum_sweep()`. It walks the
  global object list with a `prev` pointer. A marked object is kept: its
  mark is reset to 0 and it becomes the new `prev`. An unmarked object is
  dropped: its size is read from `+0` and subtracted from `@nexum_live`, its
  `+16` is read to get the next object, it is unlinked (either the head is
  replaced, or the predecessor's `+16` is overwritten), and only then
  `free(cur)` is called.
* `crates/nexum-runtime/src/lib.rs:162` — `nexum_mark_ptr(pay)`. It walks the
  same global object list and compares `o + 32` with the candidate pointer.
  It reads `+8` (the mark) and `+16` (the next object) of every object it
  visits.
* `crates/nexum-runtime/src/lib.rs:180` — `nexum_gc_trace()`. It walks the
  same list to a fixpoint, reading `+8`, `+24` and `+16` of every object.
* `crates/nexum-runtime/src/lib.rs:104` — `@nexum_obj_head` is the head of
  the global object list.
* `crates/nexum-runtime/src/lib.rs:106-108` — `@nexum_live` (the live
  payload bytes), `@nexum_since` (the bytes allocated since the last
  collection) and `@nexum_threshold` (65536).
* `crates/nexum-runtime/src/lib.rs:130` — `nexum_oom()` panics with
  `out of memory`.
* `crates/nexum-runtime/src/lib.rs:235` — `nexum_heap_used()` returns
  `@nexum_live`.
* `crates/nexum-runtime/src/lib.rs:53-54` — the two declarations to remove:
  `declare ptr @malloc(i64)` and `declare void @free(ptr)`.

### The object layout

`docs/runtime-abi.md` §2 (lines 103-139) pins it: `+0` the payload size
rounded up to 16, `+8` the mark, `+16` the next object in the global list,
`+24` the number of payload bytes to scan, `+32` the payload, 16-byte
aligned.

Two things follow that the design leans on:

* the payload is at a **fixed** offset of 32 from the block pointer, and
* every field the collector reads is at a **fixed** offset, so the allocator
  can be given a block pointer and know where the link word is.

### The system call layer

`crates/nexum-runtime/src/syscalls.rs`:

* lines 57-91 — the numbers per system. `mmap` is 9 on Linux and 197 on
  Darwin, `munmap` is 11 and 198.
* lines 110-151 — the register layout. On x86-64 the arguments go in
  `rdi, rsi, rdx, r10, r8, r9` and the result comes back in `rax`; on
  AArch64 the arguments are staged in `x9`-`x14`, moved into `x0`-`x5`, and
  the result comes back in `x0`.
* lines 236-244 — the `push` helper. It emits
  `define internal i64 @nexum_sys_{name}(i64 %a0, i64 %a1, i64 %a2, i64 %a3)`:
  **the parameter list is hardcoded to four.**
* lines 278-288 — the `mmap` wrapper. Its argument list is
  `arg(0,"%a0"), arg(1,"%a1"), arg(2,"%a2"), const_arg(3,-1), const_arg(4,0)`,
  with the comment "the file descriptor is -1 (the mapping is anonymous) and
  the offset is 0, so both are constants".

**That wrapper is wrong, and the allocator cannot work until it is fixed.**
The six arguments of `mmap` are, in order: address, length, protection,
flags, descriptor, offset. On x86-64 they land in `rdi, rsi, rdx, r10, r8,
r9`. The wrapper puts the constant `-1` at index 3 (the **flags** register)
and the constant `0` at index 4 (the **descriptor** register). The emitted
assembly is:

```
%r = call i64 asm sideeffect "...", "={rax},{rdi},{rsi},{rdx},{r10},{r8},~{rcx},~{r11},~{r12},~{memory}"(i64 %a0, i64 %a1, i64 %a2, i64 -1, i64 0)
```

so `r10` (flags) is all ones and `r8` (the descriptor) is 0 — stdin. The
kernel rejects the call with `EINVAL` in the common case; `MAP_FIXED`
(`0x10`) is among the bits set in the flags word, so a call that did get
through would map over address 0. The same is true of the AArch64 wrapper.

The fix is small: the constants move to index 4 (the descriptor) and index 5
(the offset), and the flags become the fourth *variable* argument, which
fits the existing four-parameter signature.

### What does not exist

* There is **no `nexum_alloc_raw`** anywhere in the tree. The collector has
  exactly two entry points, `nexum_alloc` and `nexum_alloc_scanned`. If a
  raw allocation is added later (phase 11d arenas, or the standard library),
  it has to be built on top of whatever this design decides; see "Open
  questions", question 6.
* There is **no `mmap` use anywhere yet**; the wrapper exists but nothing
  calls it. This is the first consumer.
* The Windows build has no system call layer at all
  (`crates/nexum-runtime/src/lib.rs:60-74`): it declares `fopen`, `fclose`,
  `fseek`, `ftell`, `fread`, `fwrite` and `__acrt_iob_func` and uses them.
* `docs/runtime-abi.md` §0 (lines 19-32) lists `malloc`/`free` as the C
  library pieces that phase 11a.2 removes, and `docs/native-roadmap.md`
  lines 100-103 record that Windows follows later because it has no direct
  system call instruction.

## The design

### 1. What the collector needs from an allocator

Exactly five things, and nothing else.

**The sizes it asks for.** `total = 32 + round16(payload)`. The collector
computes both halves itself today and passes the sum to `malloc`
(`crates/nexum-runtime/src/lib.rs:147`). The consequences:

* `total` is always a multiple of 16 and always at least 32;
* `total` is unbounded above — `nexum_read_file` asks for the size of a file
  plus one (`crates/nexum-runtime/src/strings.rs:40`), which can be
  anything;
* the *payload* is `total - 32` bytes, and the collector only ever uses
  `round16(payload)` of it.

**The alignment.** 16 bytes. The payload sits at `+32`, so a 16-byte
aligned block gives a 16-byte aligned payload, which is what
`docs/runtime-abi.md` §2 promises. Every block the allocator returns must be
16-byte aligned; that is also what makes the header's four words naturally
aligned.

**Whether it frees objects, and when.** It frees them, but only in bulk and
only in one place: the sweep (`crates/nexum-runtime/src/lib.rs:186`). An
object is freed when it is *unmarked* at the end of a collection. The
collector never frees an object individually, and it never frees a live
object. The sweep's unlink order matters and is already correct: it reads
`+16`, unlinks the object, and only then frees it.

**Whether the freed memory can be reused.** It **must** be reused. The
collector exists to bound a program's footprint, and a program that allocates
in a loop (the self-hosted compiler in phase 11j is the case that matters)
would grow without bound under an allocator that never hands memory back.
Reuse is not optional; the design below recycles every freed block.

**What the allocator must know about a block when it is freed.** Nothing, if
the collector passes the size along: the sweep already reads the payload
size out of `+0` to update `@nexum_live`, so it can pass `total = 32 + that`
to the free function. The allocator therefore does not need a size field of
its own, and does not need to read any word of the collector's header.

### 2. The block layout

The block the allocator hands out is exactly the block the collector
describes. There is no prologue, no trailer, and no per-block metadata:

```
+0   size        (i64)  the collector's payload size, rounded up to 16
+8   mark        (i64)  the collector's mark bit
+16  next        (ptr)  the collector's next-object link  /  the free-list link
+24  scan_bytes  (i64)  the collector's scan length
+32  payload           the bytes the program sees
```

The allocator's *only* use of the block is the word at `+16`, and it uses it
only while the block is free. The block's size is always known to the
allocator from the `total` argument of `nexum_heap_alloc` and
`nexum_heap_free`.

This is the whole coupling between the collector and the allocator: **the
free-list link lives at offset 16 of the block, and the block's size is
passed in rather than stored.** If phase 11d later needs an allocator that
serves callers with no collector header at all, the change is to give the
allocator its own 16-byte prologue and move the link to offset −8; the size
classes, the chunk, the bump pointer and every algorithm stay exactly as
they are. That migration is spelled out in "Open questions", question 6.

### 3. The size classes

A class is an **exact block size**. A request of `total` bytes is served by
the class whose size is the smallest one that is at least `total`.

| Class range | Sizes | Count |
| ------------ | ----- | ----- |
| fine | 32, 48, 64, …, 4096 (16-byte steps) | 255 (c = 0…254) |
| coarse | 8192, 16384, 32768, 65536 | 4 (c = 255…258) |
| large | the request's own size, its own mapping | 1 (c = 259) |

The class index is a **pure function of the size**, computed, never
searched:

```
nexum_size_class(total):
    if total <= 4096:  return (total - 32) >> 4        ; 0 … 254
    if total <= 8192:  return 255
    if total <= 16384: return 256
    if total <= 32768: return 257
    if total <= 65536: return 258
    return 259                                        ; the large class
```

and the size of a class:

```
nexum_class_size(c):
    if c <= 254:  return 32 + (c << 4)                 ; 32 … 4096
    return 8192 << (c - 255)                           ; 8192 … 65536
```

**Why 16-byte granularity.** The collector asks for `32 + round16(payload)`,
so every request is of the form `32 + 16k`. For `total <= 4096` the class
index is `(total - 32) / 16` and the class *size* is `32 + 16c`, which is
exactly `total`. **The fine classes fit every request exactly, with zero
internal fragmentation.** With 64-byte granularity a 64-byte request would
land in a 128-byte block and waste half of it; that is the mistake this
choice avoids.

**Why the fine range stops at 4096.** Beyond it the classes double, so the
waste is at most 50% of the block. The ranges that matter in practice — the
element buffer of a `List`, whose capacity doubles
(`crates/nexum-runtime/src/lib.rs:224`) — land on the class boundary
exactly below 4096 and waste 25-33% above it. Extending the fine range to
65536 would remove that waste at the price of 3840 more free-list heads;
the trade is a tuning decision, and the fine range is a single constant in
the generator.

**Why there is a large class at all.** A block over 65536 gets its own
mapping, sized to the request (rounded up to the page). That keeps the
allocator free of a first-fit search over large blocks — a loop, external
fragmentation, and a subtle bit of arithmetic — at the price of one `mmap`
and one `munmap` per large lifetime. `glibc` does the same thing above its
`mmap` threshold.

### 4. The chunk and the bump pointer

A chunk is one `mmap` of **1 MiB** (1048576 bytes), page aligned, with a
32-byte header:

```
chunk + 0   chunk_size (i64)   the mapped length
chunk + 8   chunk_next (ptr)   the next chunk in the chunk list
chunk + 32  the first block
```

The chunk's usable area is `chunk_size - 32` bytes, and the blocks are
carved from it in order by the bump pointer. Because the chunk base is page
aligned and every class size is a multiple of 16, **every block is 16-byte
aligned**, and so is every payload.

The runtime's state is four globals and one array:

```
@nexum_free_lists = internal global [260 x ptr] zeroinitializer, align 8
@nexum_chunk_list = internal global ptr null, align 8   ; every chunk, for release
@nexum_chunk_cur  = internal global ptr null, align 8   ; the chunk being carved
@nexum_chunk_bump = internal global ptr null, align 8   ; the next free byte in it
@nexum_chunk_end  = internal global ptr null, align 8   ; one past its last byte
```

**The bump never goes back.** When the current chunk cannot fit the request,
a new chunk is mapped and the old chunk's remaining tail is stranded. The
tail is at most one chunk (1 MiB) per chunk, and it is *address space*, not
resident memory: an untouched page of an anonymous mapping is never faulted
in. The free list absorbs most of the pressure before a new chunk is needed,
because a request is served from its class's free list first.

### 5. The algorithms

**Allocation.**

```
nexum_heap_alloc(total):
    c = nexum_size_class(total)
    if c == 259:                                   ; the large class
        len = page_round_up(total)
        p = mmap(NULL, len, PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS, -1, 0)
        return p < 0 ? null : p
    head = free_lists[c]
    if head != null:                               ; recycle
        free_lists[c] = head->+16
        return head
    sz = nexum_class_size(c)
    if bump != null and bump + sz <= chunk_end:
        p = bump
        bump = bump + sz
        return p
    map a new chunk (1 MiB); if that failed: return null
    p = bump; bump = bump + sz; return p
```

Two observations:

* the recycled block's size is exactly `nexum_class_size(c)`, because every
  block on a class's list was carved for that class; the allocator does not
  have to read the block to know how big it is;
* the new chunk is 1 MiB and the largest small class is 65536, so a new
  chunk always fits the request: the "carve" after mapping needs no second
  fit test, and there is **no loop**.

**Freeing.**

```
nexum_heap_free(p, total):
    c = nexum_size_class(total)
    if c == 259:
        munmap(p, page_round_up(total))
        return
    free_lists[c]->+16 = p
    free_lists[c] = p
```

**The class function is called twice with the same argument** — once at
allocation, once at free — and both calls see the same `total`, because the
collector stores the payload size in the header at allocation and never
changes it. That is the whole consistency argument; there is no other place
where a block's size lives.

### 6. The `+16` slot

The collector's `+16` word holds the next object in the global list while
the object is live. The allocator wants to use it as the free-list link
while the block is free. The two uses are compatible because **a block is in
exactly one list at a time**: the collector's object list if it is a live
object, the allocator's free list if it is dead. The four invariants:

1. **The collector unlinks the object before it frees it.** The sweep
   (`crates/nexum-runtime/src/lib.rs:186`) reads `+16` into a register,
   writes the predecessor's `+16` (or the head global), and only then calls
   the free function. When the allocator writes the free link into `+16`,
   the slot is dead. *This is the invariant a future change could break;
   it is called out again in "Risks".*
2. **The collector only ever walks the object list.** `nexum_mark_ptr`,
   `nexum_gc_trace` and `nexum_sweep` all start at `@nexum_obj_head` and
   follow `+16`. A free block is never in that list, so its `+16` — which
   points at another *free* block — is never read as an object link.
3. **The collector overwrites `+16` when it links a new object.** In
   `nexum_alloc_scanned`, `store ptr %head, ptr %np` writes the object-list
   head into `+16` before the object is published, so a recycled block's
   stale free link is gone before the object becomes reachable.
4. **The allocator pops by reading `+16` and never writes it.** The popped
   block's `+16` still holds the old free-list successor until the
   collector overwrites it; the allocator does not rely on it.

A trace, with the object list `A → B → C → null` and `B` unmarked:

```
sweep: cur = A   marked  -> keep: A->+8 = 0; prev = A; cur = A->+16 = B
sweep: cur = B   unmarked-> drop: sz = B->+0; live -= sz
                           dnxt = B->+16 = C          ; read first
                           A->+16 = C                 ; unlink B
                           free(B, 32 + sz)           ; now B->+16 = the free list
sweep: cur = C   marked  -> keep: C->+8 = 0; prev = C; cur = C->+16 = null
```

`B` is not in the object list any more, so the free link in its `+16` is
harmless; and `A->+16` was rewritten before `B` was freed, so the list is
`A → C → null` and intact.

### 7. The system call layer

**The `mmap` wrapper has to be fixed.** The argument list becomes:

```rust
&[
    arg(0, "%a0"),      // the address (0: the kernel chooses)
    arg(1, "%a1"),      // the length
    arg(2, "%a2"),      // the protection
    arg(3, "%a3"),      // the flags
    const_arg(4, -1),   // the descriptor: -1, an anonymous mapping
    const_arg(5, 0),    // the offset
]
```

The emitted constraint string becomes `={rax},{rdi},{rsi},{rdx},{r10},{r8},{r9}`
on x86-64 and `={x0},{x9},{x10},{x11},{x12},{x13},{x14}` on AArch64, and the
operands become `(i64 %a0, i64 %a1, i64 %a2, i64 %a3, i64 -1, i64 0)`. The
four-parameter signature of the generated function is already right: `a0`
to `a3` are the four variable arguments.

**The flags and the protection are per system**, so they belong in the
`Syscalls` struct next to the numbers:

| | Linux | Darwin |
| - | - | - |
| `PROT_READ \| PROT_WRITE` | 0x03 | 0x03 |
| `MAP_PRIVATE \| MAP_ANONYMOUS` | 0x02 \| 0x20 = 0x22 | 0x0002 \| 0x1000 = 0x1002 |

(Darwin spells the anonymous flag `MAP_ANON`; the value is the same
`0x1000`.)

**The page size is a per-target constant** and it goes into the generator
next to the syscall numbers:

| Target | Page size |
| ------ | --------- |
| linux-x86_64, linux-aarch64, macos-x86_64, windows-x86_64 | 4096 |
| macos-aarch64 | **16384** |

macOS on Apple Silicon has 16 KiB pages, and macOS's `munmap` fails with
`EINVAL` when the length is not a multiple of the page size. Linux rounds
the length up in both `mmap` and `munmap`, so on Linux the constant only
affects how exactly the mapping lines up; on macOS it decides whether the
unmap works at all. The same constant is used for the mapping length and
for the unmap length, so the two always agree.

**The `munmap` wrapper is fine as it stands** (`syscalls.rs:289`): two
arguments, `rdi` and `rsi`. It has two unused parameters (`a2`, `a3`),
which is harmless.

### 8. Windows

The Windows build has no system call layer (`crates/nexum-runtime/src/lib.rs:60-74`),
and Windows has no direct system call instruction, so the raw-block layer
has to be something else. Three options:

* **A. `VirtualAlloc`/`VirtualFree` (recommended).** They are kernel32
  exports, not the C library, so the Windows program stops depending on the
  CRT's heap; the CRT is still linked for stdio and `exit` until the output
  formatting is replaced. The allocator's algorithms, classes and free
  lists are identical to the POSIX ones — only the two raw-block calls
  differ:

  ```
  VirtualAlloc(NULL, len, MEM_COMMIT|MEM_RESERVE /* 0x3000 */, PAGE_READWRITE /* 0x04 */)
  VirtualFree(p, 0, MEM_RELEASE /* 0x8000 */)
  ```

  `VirtualAlloc` returns null on failure, which is the same convention as
  the POSIX path. The Windows *allocation granularity* is 64 KiB, so a
  large block's reservation is rounded up to that; the page size is 4096.
* **B. Keep `malloc`/`free` on Windows.** The smallest possible change:
  the POSIX build gets the new allocator, the Windows build keeps the CRT
  heap. The collector's code is identical on both platforms — the Windows
  `nexum_heap_alloc` is just `malloc` and `nexum_heap_free` is `free`, and
  the free lists are simply not used there. The cost is that the two
  platforms behave differently (Windows does not recycle, and it has no
  class rounding), and the program still declares `malloc` and `free`.
* **C. Wait for the ntdll stubs.** The roadmap puts the Windows system call
  stubs in 11a.2 as a separate piece of work
  (`docs/native-roadmap.md` lines 100-103). Waiting means the allocator
  lands on POSIX first and Windows follows.

The recommendation is **A**, with **B** as the fallback if the orchestrator
wants this slice to touch POSIX only. When the ntdll stubs land, `VirtualAlloc`
is replaced by `NtAllocateVirtualMemory` and nothing else in the allocator
changes. The design deliberately keeps the raw-block layer behind two
functions so that this swap is a two-line change.

### 9. The growth policy and the failure mode

**Growth.** One chunk at a time, 1 MiB each, mapped on demand. The chunk
size must be at least the largest class plus the header (65536 + 32) and a
multiple of the page size; 1 MiB satisfies both on every target. A chunk is
never unmapped during the program's life, so a program that allocates and
frees in a loop keeps a steady footprint.

**The failure mode is the existing one.** A failed `mmap` returns a negative
value; the allocator returns null; `nexum_alloc_scanned` sees the null and
calls `nexum_oom()`, which prints `out of memory` on stderr and exits with
status 1 (`crates/nexum-runtime/src/lib.rs:130`). The same happens when
`VirtualAlloc` returns null on Windows.

*Recommended addition:* before declaring OOM, run a collection and retry
once. A collection frees the garbage, which may be enough for the
allocation to fit in the existing chunk. The sequence becomes

```
p = nexum_heap_alloc(total)
if p == null:  nexum_collect();  p = nexum_heap_alloc(total)
if p == null:  nexum_oom()
```

This is a strict improvement and it does not change the OOM message or the
exit status.

**Overflow.** `total` is `32 + round16(payload)`. A payload near 2^63 would
overflow the addition and produce a negative total, which would land in the
large class and ask for a negative length. The allocator should reject a
total that is not in `[32, 2^47]` by returning null, which becomes the OOM
path.

### 10. The collector's own bookkeeping

The collector's bookkeeping is the global object list (`@nexum_obj_head`,
`crates/nexum-runtime/src/lib.rs:104`) plus the root list. The interaction
with the allocator is:

* **Three lists, three different link words.** The object list is threaded
  through the blocks' `+16`; the allocator's per-class free lists are
  threaded through the same `+16`; the chunk list is threaded through the
  *chunk headers* (`chunk + 8`), which are not blocks and never collide.
* **The collector never walks the free lists and the allocator never walks
  the object list.** Each list is only ever traversed from its own head.
* **The collector's accounting is unchanged.** `@nexum_live` still counts
  the live *payload* bytes, because the sweep still subtracts the payload
  size it reads from `+0`. `heap_used()` keeps its meaning. It does
  under-report the real memory: the class rounding, the chunk headers and
  the stranded tails are not counted. That was already true of `malloc`,
  and it is not worth changing here.
* **The collection watermark is unchanged.** `@nexum_since` still accumulates
  payload sizes and is still compared against `@nexum_threshold` (65536).
* **The mark bit and the scan length are untouched.** The allocator never
  writes `+8` or `+24`; it only writes `+16`, and only on a dead block.
* **The conservative scanner is unaffected.** It scans payloads, never
  headers, and a recycled block's payload is written by the new occupant
  before the object is published.

### 11. Writing it in LLVM IR text

The runtime is emitted as IR text (`crates/nexum-runtime/src/lib.rs:46`),
so the design has to be writable in that form. This one is, and unusually
so:

* **The allocator contains no loop at all.** Allocation is a handful of
  branches and one array index; freeing is an array index and two stores.
  The class function is a comparison chain and a shift. The only loop in the
  neighbourhood is the one in `nexum_memcpy`, which is already there.
* **The class index is computed, not searched.** This is the single
  decision that makes the design IR-friendly. A design that *searches* for
  the smallest non-empty class needs a loop over 260 entries on every
  allocation, or an unrolled binary search — a chain of 8 comparisons
  written out by hand, which is exactly the kind of text that is painful to
  review and easy to get wrong.
* **Nothing is split and nothing is merged.** Coalescing adjacent free
  blocks is the part of a real allocator that is hardest to express in IR
  text: it needs address-ordered free lists per bin, neighbour lookups, and
  careful pointer arithmetic, and a mistake there corrupts the heap
  silently. The exact-size classes make it unnecessary: two blocks of the
  same class are interchangeable, so the free list is a plain stack.
* **A tree or a slab allocator would be worse.** A size-ordered tree needs
  sift-up and sift-down loops; a slab allocator needs per-slab block counts
  and bitmaps. Both are writable but neither buys anything for this
  workload.

The IR sketch the implementer is aiming at, with the shape of each function
(these are the algorithms, not the final text):

```
define internal i64 @nexum_size_class(i64 %total)      ; 5 branches, one shift
define internal i64 @nexum_class_size(i64 %c)          ; 2 branches, one shift
define internal i64 @nexum_page_round_up(i64 %n)       ; add, and
define internal void @nexum_heap_new_chunk(i64 %min)   ; one mmap of 1 MiB, a few stores
define internal ptr @nexum_heap_alloc(i64 %total)      ; the algorithm above
define internal void @nexum_heap_free(ptr %p, i64 %total)
```

`nexum_heap_alloc` in full, to show that there is no back-edge:

```
define internal ptr @nexum_heap_alloc(i64 %total) {
entry:
  %c = call i64 @nexum_size_class(i64 %total)
  %islarge = icmp eq i64 %c, 259
  br i1 %islarge, label %large, label %small
small:
  %slot = getelementptr [260 x ptr], ptr @nexum_free_lists, i64 0, i64 %c
  %head = load ptr, ptr %slot
  %empty = icmp eq ptr %head, null
  br i1 %empty, label %bump, label %pop
pop:
  %np = getelementptr i8, ptr %head, i64 16
  %nxt = load ptr, ptr %np
  store ptr %nxt, ptr %slot
  ret ptr %head
bump:
  %sz = call i64 @nexum_class_size(i64 %c)
  %bump = load ptr, ptr @nexum_chunk_bump
  %none = icmp eq ptr %bump, null
  br i1 %none, label %chunk, label %fits
fits:
  %end = load ptr, ptr @nexum_chunk_end
  %p = getelementptr i8, ptr %bump, i64 0
  %np2 = getelementptr i8, ptr %bump, i64 %sz
  %room = icmp ule ptr %np2, %end
  br i1 %room, label %carve, label %chunk
carve:
  store ptr %np2, ptr @nexum_chunk_bump
  ret ptr %p
chunk:
  call void @nexum_heap_new_chunk(i64 %sz)
  %b3 = load ptr, ptr @nexum_chunk_bump
  %still = icmp eq ptr %b3, null
  br i1 %still, label %fail, label %carve2
carve2:
  %b4 = load ptr, ptr @nexum_chunk_bump
  %p3 = getelementptr i8, ptr %b4, i64 0
  %np3 = getelementptr i8, ptr %b4, i64 %sz
  store ptr %np3, ptr @nexum_chunk_bump
  ret ptr %p3
large:
  %len = call i64 @nexum_page_round_up(i64 %total)
  %r = call i64 @nexum_sys_mmap(i64 0, i64 %len, i64 3, i64 34)
  %bad = icmp slt i64 %r, 0
  br i1 %bad, label %fail, label %ok
ok:
  %p2 = inttoptr i64 %r to ptr
  ret ptr %p2
fail:
  ret ptr null
}
```

(`%sz` is `nexum_class_size(%c)`, computed in the `bump` block, which
dominates every use of it. The `chunk` path does not re-test the fit: a
1 MiB chunk always fits a request of at most 65536 bytes, and the `still`
check catches the failed mapping so the function cannot loop.)

### 12. The set of functions that change

| Where | What | Change |
| ----- | ---- | ------ |
| `crates/nexum-runtime/src/syscalls.rs:278-288` | the `mmap` wrapper | the constants move to index 4 and 5; the flags become the fourth variable argument |
| `crates/nexum-runtime/src/syscalls.rs:236-244` | the `push` helper | the parameter count must follow the number of variable arguments (four today, which is enough) |
| `crates/nexum-runtime/src/syscalls.rs:57-91` | `Syscalls::for_os` | add the `mmap` protection and flags constants, per system |
| `crates/nexum-runtime/src/syscalls.rs` | a new constant | the page size per target (4096, except 16384 on macOS/AArch64) |
| `crates/nexum-runtime/src/lib.rs:53-54` | the declarations | remove `declare ptr @malloc(i64)` and `declare void @free(ptr)` |
| `crates/nexum-runtime/src/lib.rs:147` | `nexum_alloc_scanned` | `@malloc(%total)` becomes `@nexum_heap_alloc(%total)`; the null check stays; optionally add the collect-and-retry |
| `crates/nexum-runtime/src/lib.rs:186` | `nexum_sweep` | `@free(%cur)` becomes `@nexum_heap_free(%cur, %total)`, where `%total` is `32 +` the payload size already read from `+0` |
| a new file `crates/nexum-runtime/src/alloc.rs` | the allocator | the globals, `nexum_size_class`, `nexum_class_size`, `nexum_page_round_up`, `nexum_heap_new_chunk`, `nexum_heap_alloc`, `nexum_heap_free` |
| `crates/nexum-runtime/src/lib.rs:46` | `runtime_ir` | call the allocator generator, and pick the page size from the target |

**Functions that do not change:** `nexum_alloc`, `nexum_gc_maybe`,
`nexum_collect`, `nexum_gc_trace`, `nexum_mark_ptr`, `nexum_scan_range`,
`nexum_roots_push`, `nexum_roots_pop`, `nexum_panic`, `nexum_oom`,
`nexum_concat`, `nexum_slice`, `nexum_str_char_at`, `nexum_list_new`,
`nexum_list_push`, `nexum_list_get`, `nexum_list_set`, `nexum_heap_used`,
and the string and file routines. The collector's object layout, its
accounting and its algorithms are all unchanged.

**The compiler side changes nothing.** The runtime's entry points keep their
names and their signatures, so `crates/nexum-codegen/src/lib.rs:119` (which
splices the runtime into the module) and the builtins map at
`crates/nexum-codegen/src/lib.rs:838-839` are untouched.

**The documentation changes:** `docs/runtime-abi.md` §2 gains a paragraph
about the allocator and the shared `+16` slot, and §0's table moves
`malloc`/`free` from "the C library" to "the runtime's own".

## Risks

**The `+16` invariant is the one that can bite.** If a future change frees
an object *before* unlinking it from the object list, the free link would
overwrite the object's next pointer, and the sweep would then walk into the
free list and treat free blocks as live objects: the mark bit of a free
block is whatever it was, so a block could be resurrected (leaked) or, worse,
its `+0` would be read as a payload size and subtracted from `@nexum_live`
until the accounting went negative. The sweep's current order is correct;
this is a note for whoever touches it next, and it deserves a comment in the
code and a test that pins the order.

**The size passed to `free` must be the size passed to `alloc`.** The class
is derived from the size at both ends. If the collector ever changed what it
stores in `+0` (say, to the *block* size instead of the payload size), a
block would land on the wrong class's list, and could later be handed out
with a capacity smaller than the payload it holds — a buffer overflow, not a
crash. The interface makes this hard to get wrong (the size is passed
explicitly, and the collector derives it from the header in exactly one
place), but it is a coupling and it should be documented at both ends.

**The `mmap` wrapper is broken today.** If the fix is not applied, the
allocator maps with the wrong flags and the wrong descriptor. The kernel
rejects the call with `EINVAL` in the common case, which the allocator would
read as "out of memory" — a confusing failure rather than a crash. But
`MAP_FIXED` is among the bits in the flags word, so a call that did get
through would map over address 0. This is a prerequisite, not a nicety.

**The page size on macOS/AArch64.** Getting it wrong (4096 instead of 16384)
makes every `munmap` of a large block fail with `EINVAL`. The failure is
silent — the allocator does not check the return value — and the effect is
that large blocks leak instead of being returned. The constant must be
per-target and it must be the same constant in the mapping length and the
unmap length.

**The stranded chunk tails.** Each chunk strands up to 1 MiB of address
space. This is not resident memory, but on a 32-bit target (not supported
today) a program that churns through many chunks could exhaust the address
space. A smaller chunk (64 KiB) reduces the waste sixteenfold at the cost
of sixteen times as many `mmap` calls. The chunk size is one constant.

**The large-class policy.** A program that repeatedly allocates and frees a
buffer over 64 KiB pays an `mmap` and a `munmap` per cycle, and re-faults
the pages. `glibc` has the same behaviour above its threshold, and it is
the price of not writing a first-fit search over large blocks. If a future
profile shows this matters, the fix is a large-block free list with a
first-fit walk, which is a loop and would need its own design.

**The class granularity is a guess.** 16 bytes in the fine range is exact
for the collector's sizes, but the fine range's end (4096) and the doubling
above it are tuning decisions made without a profile of a real Nexum
program. The self-hosted compiler (phase 11j) is the workload that will
exercise this, and it is the right time to revisit the numbers — the
constants are all in one place.

**`heap_used()` under-reports.** It counts live payload bytes, not the
memory the process holds. That was already true with `malloc` and it does
not get worse in a way that matters, but the gap grows now that the
allocator has class rounding and chunk overhead of its own.

## Open questions

1. **The chunk size.** 1 MiB is recommended: it is big enough that the
   `mmap` count stays low (one per megabyte of allocation) and small enough
   that the stranded tail stays under a megabyte. 64 KiB or 256 KiB are
   reasonable alternatives; it is one constant in the generator.
   *Recommendation: 1 MiB.*

2. **The threshold of the large class.** 65536 (the biggest small class) is
   recommended, so the rule is simply "anything the classes cannot hold".
   128 KiB, like `glibc`'s `mmap` threshold, would move more allocations
   into the chunk path at the price of a coarser class ladder.
   *Recommendation: 65536, because the rule stays trivial.*

3. **Windows.** `VirtualAlloc`/`VirtualFree` (recommended) or keep
   `malloc`/`free` on Windows for this slice. The collector's code is the
   same either way; the difference is whether the Windows program stops
   declaring `malloc` and `free`.
   *Recommendation: `VirtualAlloc`. It is kernel32, not the C library, and
   it makes the two platforms behave identically, which is worth more than
   the small extra surface.*

4. **The collect-and-retry before OOM.** It turns "out of memory" into "the
   collection freed enough" in the common case where the program is
   garbage-heavy, at the cost of one collection in the genuinely hopeless
   case. *Recommendation: yes, add it.*

5. **What `heap_used()` counts.** It counts live payload bytes today and
   would keep counting them. It could count the allocator's block bytes
   instead, which would be closer to the truth but would change a
   user-visible builtin.
   *Recommendation: keep the payload bytes, and note the discrepancy in
   the ABI document.*

6. **Whether the allocator stays coupled to the collector's header.** The
   recommendation keeps them coupled: the free-list link at `+16`, the size
   passed in rather than stored, and no prologue. The price is that the
   allocator cannot serve a caller that has no collector header — which
   phase 11d (arenas, explicit memory) and the standard library will want.
   The migration is mechanical: give the allocator a 16-byte prologue
   (the size at −16, the link at −8), move the payload to +32 of the
   prologue, and change the two call sites. The classes, the chunk, the bump
   and every algorithm are unaffected.
   *Recommendation: keep the coupling now, and pay the prologue in 11d when
   a second kind of caller actually exists. The alternative — paying the
   16 bytes per object from day one, on objects as small as 48 bytes — buys
   a future that phase 11d has not asked for yet.*

7. **Whether a raw allocation entry point is added in this slice.** There is
   no `nexum_alloc_raw` in the tree today. The recommendation does not add
   one: the allocator's contract is "the caller passes the same total to
   free that it passed to alloc", which is a contract a raw caller can
   honour, but a raw caller also needs a place to put its own size if it
   wants the allocator to find it, and that place is the collector's `+0`
   today. If the orchestrator wants `nexum_alloc_raw` in this slice, it
   should decide question 6 first.
   *Recommendation: do not add it now; add it with the prologue in 11d.*
