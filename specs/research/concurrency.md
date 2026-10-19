# The concurrency model without the C library

**Question.** How does a Nexum program reach the cores when the program links
no C library: how is a thread created without `pthread_create`, what does a
thread's stack and thread-local storage look like, what is the Windows story,
what happens to the collector when two threads exist, what do the atomics mean,
does the language detect data races, is there a scheduler, and what does the
surface syntax look like?

**Summary.** A thread is a raw `clone` on Linux (56 on x86-64, 220 on
AArch64) with the child's stack from `mmap` and the child's thread pointer
handed to the kernel through `CLONE_SETTLS`; `pthread_create` is out because
it is C library code. Windows has no answer until phase 11a.2 generates the
ntdll stubs and macOS has none at all without the Mach thread API, so both
are documented limitations in 11e.1 and the compiler rejects `spawn` there.
The collector is the hard part: in 11e.1 it becomes *thread-local* — one
heap, one root list and one watermark per thread — which needs no
stop-the-world, no collector lock, and no change to the compiler's root
protocol, at the price of a rule the type checker can enforce (no managed
value crosses a thread boundary). The shared heap with a stop-the-world
collector is 11e.2 and needs eleven specific changes to today's runtime; they
are listed below so they are not discovered during implementation. The
atomics are pure compiler work — LLVM's atomic instructions with C11
orderings, defaulting to sequentially consistent — and are the one slice that
can be built, tested and landed on its own. There is no race detector and no
user-visible scheduler; the surface syntax is `spawn f(args)`, `join(t)`,
`atomic_new`, `atomic_load`, `atomic_store`, `atomic_fetch_add`,
`atomic_compare_exchange`, and one ordering-suffixed variant of each.

## Recommendation

1. **Thread creation is `clone`, not `pthread_create`.** `pthread_create` is
   glibc code; it allocates the stack, sets up the TLS and the robust list,
   and it does all of that in ways a freestanding program cannot reach. The
   kernel's `clone` gives us the three things we actually need — a fresh
   stack, a fresh thread pointer, and a thread id — and nothing else.
   *Reason:* D012 (the runtime is freestanding) and D013 (the operating
   system is reached through inline assembly, never through a C binding) leave
   no room for a library call that happens to be convenient.

2. **The syscall numbers go in the per-system table next to the existing
   ones** (`crates/nexum-runtime/src/syscalls.rs:58`), and the `clone`
   wrapper is added to `wrappers` (`crates/nexum-runtime/src/syscalls.rs:237`)
   in the same style: the number is an immediate in the assembly text, the
   arguments are pinned to registers, the stack is aligned for the call.
   The numbers, verified against the kernel sources:

   | Call | Linux x86-64 | Linux AArch64 | Used for |
   | ---- | ------------ | ------------- | -------- |
   | `clone` | 56 | 220 | thread creation |
   | `futex` | 202 | 98 | join, parking, mutexes |
   | `exit` | 60 | 93 | ending *one* thread |
   | `exit_group` | 231 | 94 | **not** used: it would kill the process |
   | `mmap` | 9 | 222 | thread stacks |
   | `munmap` | 11 | 215 | releasing a stack |
   | `sched_yield` | 24 | 124 | `yield()` |
   | `gettid` | 186 | 178 | thread id, diagnostics |
   | `sched_getaffinity` | 204 | 123 | `cpu_count()` |
   | `prlimit64` | 302 | 261 | reading `RLIMIT_STACK` for the default stack size |

   *Reason:* the existing layer already keeps the numbers per system
   (`syscalls.rs:58`) and the roadmap commits to that discipline (D013). The
   numbers were read out of `arch/x86/entry/syscalls/syscall_64.tbl` and
   `include/uapi/asm-generic/unistd.h` in the Linux tree, not from memory.

3. **`clone`'s argument order is per architecture, not per system, and the
   two supported architectures differ.** This is the single most dangerous
   detail in this document:

   | Architecture | `clone` argument order | Registers |
   | ------------ | ---------------------- | --------- |
   | x86-64 | `flags, newsp, parent_tid, child_tid, tls` | `rdi, rsi, rdx, r10, r8` |
   | AArch64 | `flags, newsp, parent_tid, tls, child_tid` | `x0, x1, x2, x3, x4` |

   The AArch64 order is the "backwards" one: `arch/arm64/Kconfig` selects
   `CLONE_BACKWARDS` (line 111) and `arch/x86/Kconfig` selects it only for
   `X86_32` (line 16), so x86-64 uses the normal order and AArch64 does not.
   The prototype in `include/linux/syscalls.h` and the expansion in
   `kernel/fork.c` (`SYSCALL_DEFINE5(clone, ...)`, line 2891) follow the
   selection. *Reason:* getting this wrong is silent and fatal — the kernel
   would take the child-tid pointer as the thread pointer and the child would
   crash on its first thread-local access, or worse, would appear to work.

4. **The flags are `CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND |
   CLONE_THREAD | CLONE_SYSVSEM | CLONE_SETTLS | CLONE_PARENT_SETTID |
   CLONE_CHILD_CLEARTID`** (`0x3D0F00`). `CLONE_THREAD` makes it a thread
   rather than a process; `CLONE_SETTLS` gives the child its own thread
   pointer; `CLONE_PARENT_SETTID` makes the kernel write the child's tid
   into the parent's `parent_tid` word at creation; `CLONE_CHILD_CLEARTID`
   makes the kernel clear that same word when the thread dies and issue a
   futex wake on it, which *is* the join mechanism. The `parent_tid` and
   `child_tid` arguments therefore point at the same word — the thread
   record's tid word — exactly as glibc passes `&pd->tid` twice.
   `CLONE_CHILD_SETTID` is not used: it makes the child write its own tid
   into the `child_tid` word at exit, which the kernel's clearing already
   covers.
   *Reason:* this is the set glibc uses for a pthread, minus the parts only
   the C library needs, and it gives the join for free.

5. **The stack is allocated by the parent with `mmap` and released by the
   child.** Default size 8 MiB (the Linux default), overridable by the
   program; `prlimit64(RLIMIT_STACK)` can supply the process default. One
   `PROT_NONE` guard page below the stack catches overflow. The child
   cannot unmap the stack it is running on, so it switches to a separate
   one-page *exit stack* first and unmaps the thread stack from there. The
   parent never touches the child's stack.
   *Reason:* the child releasing its own stack works even if the parent never
   joins, and it keeps the thread record free of ownership questions.

6. **The thread-local storage the child needs is the child's thread pointer,
   and the kernel sets it — but the runtime has to give the kernel a block
   with the right layout.** `CLONE_SETTLS` makes the kernel store the `tls`
   argument as the child's thread pointer: on AArch64
   `p->thread.uw.tp_value = tls` (`arch/arm64/kernel/process.c:486`), on
   x86-64 the child's FS base. The kernel does not interpret the pointer, so
   the runtime must hand it a block laid out exactly like the parent's, with
   the runtime's per-thread state at the same offset.
   **This is phase 11a's work, not 11e's.** 11a owns the freestanding
   startup and therefore owns the TLS layout. 11e must consume an interface
   from 11a — call it `nexum_tls_child_block()` — and must not open-code the
   layout itself. *Reason:* the ELF TLS layout (where the thread pointer sits
   relative to the block, how the block is copied for a child) is exactly the
   kind of detail that is silently wrong if two people implement it.

7. **The fallback if 11a's TLS work slips: the per-thread state travels as a
   hidden first parameter.** Every Nexum function would take the per-thread
   control block as an implicit first argument. *Reason:* it needs no TLS
   machinery at all and works identically on every target, but it changes
   every function signature and every call site, so it is a fallback, not a
   choice.

8. **The thread's entry point is a runtime trampoline, and the arguments are
   unpacked by a compiler-generated thunk.** The parent writes two words at
   the top of the child's stack: the entry pointer and the argument pointer.
   The trampoline pops them and calls the entry. The entry is *not* the
   user's function: a Nexum function takes typed parameters, and there is no
   way to unpack them generically. So `spawn f(a, b)` lowers to
   `nexum_spawn(ptr @f.__spawn_thunk, ptr %argbuf)`, and the compiler emits
   the thunk, which knows the types and loads the arguments out of the
   buffer. The buffer is a native (non-collector) allocation the parent makes
   and the child frees.
   *Reason:* the trampoline stays uniform across every function the program
   might spawn, and the type knowledge stays in the compiler where it
   belongs.

9. **`join` waits on the tid word the kernel already maintains.** The thread
   record holds one four-byte `tid` word. `clone` is given that word as both
   the `parent_tid` and the `child_tid` argument, so the kernel writes the
   child's tid into it at creation and clears it — with a futex wake — when
   the thread dies. The join is then: read the word; while it is not zero,
   `futex(FUTEX_WAIT_PRIVATE, &word, tid, timeout)`; the wait returns
   `EAGAIN` immediately if the word already changed, so the loop is the
   whole protocol. `FUTEX_WAIT`'s timeout is *absolute*
   (`CLOCK_MONOTONIC`), not relative; a relative timeout would return
   immediately. `join` returns `Unit` in 11e.1 — a thread that returns a
   managed value would have to publish it into the collector, which is the
   11e.2 problem. A second `join` on the same handle is a runtime error, not
   a use-after-free.
   *Reason:* the futex is the only blocking primitive that needs no C
   library, `FUTEX_*_PRIVATE` avoids the shared-hash path entirely, and
   reusing the kernel's own tid word means the runtime keeps no second
   piece of join state.

10. **The collector in 11e.1 becomes thread-local; the shared heap is
    11e.2.** Each thread gets its own object list, its own root list, its
    own watermark and its own collector. Nothing is shared, so there is no
    collector lock, no stop-the-world, no change to the mark/sweep
    algorithm, and **no change at all to the compiler's root protocol** —
    `nexum_roots_push` and `nexum_roots_pop` keep their signatures
    (`crates/nexum-runtime/src/lib.rs:136` and `:139`) and simply read and
    write the current thread's head instead of a global. The price is a rule
    the type checker can enforce exactly: **a managed value never crosses a
    thread boundary.** `spawn` accepts only shareable arguments (`Int`,
    `Float`, `Bool`, records and arrays of those, and `AtomicInt`), and the
    spawned function must return `Unit`.
    *Reason:* of the three options analysed below, this is the only one that
    is implementable without rewriting the collector, and it is sound
    precisely because the type rule is checkable. It also matches the
    architecture note that "concurrency follow[s] ownership/data
    partitioning where possible".

11. **The atomics are LLVM's atomic instructions, with C11 orderings, and
    sequentially consistent is the default.** The five orderings the
    language exposes are relaxed, acquire, release, acq_rel and seq_cst
    (LLVM spells relaxed as `monotonic`). The operations are load, store,
    exchange, compare-exchange, fetch-add and fetch-sub, plus fences. The
    mapping is direct:

    | Nexum | LLVM IR |
    | ------ | -------- |
    | `atomic_load_relaxed(a)` | `%v = load atomic i64, ptr %a monotonic, align 8` |
    | `atomic_load_acquire(a)` | `%v = load atomic i64, ptr %a acquire, align 8` |
    | `atomic_load_seq_cst(a)` | `%v = load atomic i64, ptr %a seq_cst, align 8` |
    | `atomic_store_release(a, v)` | `store atomic i64 %v, ptr %a release, align 8` |
    | `atomic_exchange_acq_rel(a, v)` | `%o = atomicrmw xchg ptr %a, i64 %v, align 8 acq_rel` |
    | `atomic_compare_exchange_seq_cst(a, e, d)` | `%r = cmpxchg ptr %a, i64 %e, i64 %d, align 8 seq_cst seq_cst` |
    | `atomic_fetch_add_seq_cst(a, v)` | `%o = atomicrmw add ptr %a, i64 %v, align 8 seq_cst` |
    | `atomic_fetch_sub_relaxed(a, v)` | `%o = atomicrmw sub ptr %a, i64 %v, align 8 monotonic` |
    | `atomic_fence_seq_cst()` | `fence seq_cst` |

    `cmpxchg` returns `{i64, i1}`: the flag says whether the exchange
    happened, and the value is what the cell held. The language's
    `expected` argument must be updated with the actual value on failure, so
    the compiler stores the returned value back into the expected slot when
    the exchange fails. *Reason:* the architecture note asks for "formally
    defined Relaxed, Acquire, Release, AcqRel and SequentiallyConsistent
    semantics" and for ordering to be "a language contract, not a
    backend-specific convenience"; LLVM's orderings *are* that contract, and
    naming them one-for-one avoids inventing a third vocabulary.

12. **An `AtomicInt` is a pointer to a cell in a shared, non-collector
    arena.** The arena is bump-allocated with an atomic `fetch_add` and its
    cells are never freed, so it needs no lock and no collector. The cell's
    contents are an `i64` and hold no references, so even if it were
    collector-managed nothing would be traced. Because the arena is outside
    every collector heap, an `AtomicInt` can be handed to another thread —
    which is the whole point of it. *Reason:* under the thread-local
    collector of recommendation 10, a collector-managed atomic could not
    cross a thread boundary at all, and the semantics would have to change
    in 11e.2. The arena keeps one set of semantics across both phases.

13. **The language does not detect data races.** The programmer is
    responsible, exactly as in C and in Rust's `unsafe` blocks. The
    difference from C is that a race is *not* undefined behaviour here: the
    language keeps its memory-safety guarantee (no dangling pointers, no
    overruns), and a racy access to a native cell yields an unspecified
    value rather than a crash. The **safe subset** is:
    * no sharing at all — each thread owns its data (the recommended style);
    * locations accessed only through atomic operations;
    * message passing (channels, 11e.3) for anything else;
    * read-only sharing of a native buffer that nobody writes.
    The unsafe part is concurrent *non-atomic* access to shared native
    memory. Once phase 11d lands `unsafe` blocks, that is what they should
    mark — not the atomics themselves. *Reason:* a race detector needs a
    happens-before model, an instrumentation pass and a runtime; the
    sanitizers live in compiler-rt, which is a C library, and a program that
    links it is no longer a program that depends on nothing but the kernel
    (D012). The toolchain may later offer `--tsan` as a *toolchain* feature;
    the language must not require it.

14. **There is no user-visible scheduler in 11e.1.** `spawn` creates an OS
    thread, `join` blocks, and the operating system's scheduler decides which
    thread runs on which core. A program parallelises by spawning N threads
    (N from `cpu_count()`, i.e. `sched_getaffinity`) and giving each a slice
    of the work. The roadmap's "a scheduler" is better served by a
    work-stealing pool written in Nexum in the standard library — the
    roadmap's own principle is that "the standard library is written in
    Nexum" — built on the same primitives (a queue, a futex, atomics).
    *Reason:* a user-level scheduler is a library, not a language feature,
    and building it in Nexum is what proves the primitives are enough.

15. **The surface syntax.**

    ```nexum
    # a thread
    fn worker(n: Int):
        # ... work ...
        return

    let t = spawn worker(7)
    join(t)

    # an atomic
    let mut counter = atomic_new(0)
    atomic_store(counter, 1)              # seq_cst
    let v = atomic_load(counter)          # seq_cst
    let old = atomic_fetch_add(counter, 1)
    let ok = atomic_compare_exchange(counter, 1, 2)
    atomic_store_release(counter, 3)
    let r = atomic_load_relaxed(counter)
    atomic_fence_seq_cst()
    ```

    `spawn` is not an ordinary call: it names a function and its arguments
    directly (`spawn worker(7)`), because the callee has to be known at
    compile time for the thunk to be emitted. `join` and the `atomic_*`
    family are builtins in the same style as `list_push` and `concat`. Two
    new opaque types: `Thread` and `AtomicInt`, both `ptr` in the ABI, both
    holding no references (so `has_refs` in
    `crates/nexum-codegen/src/lib.rs:23` returns false for them and the
    collector never scans them). Each atomic operation has one plain name
    (seq_cst) and one name per explicit ordering
    (`_relaxed`, `_acquire`, `_release`, `_acq_rel`, `_seq_cst`).
    *Reason:* the language has no overloading and no value-level enum
    constants, so a name per ordering fits the existing builtin style
    (`str_char_at`, `list_get`) better than an ordering argument.

16. **What can be built independently.** This matters for the plan, because
    the pieces have very different dependencies:

    | Piece | Depends on | Can start now |
    | ----- | ---------- | ------------- |
    | Atomics: syntax, type checking, lowering to LLVM atomic instructions | nothing | **yes — this is the first slice** |
    | `Thread` / `AtomicInt` types, `spawn` / `join` surface syntax | nothing (stubs suffice) | yes |
    | `clone` / `futex` / `exit` syscall wrappers and their per-system numbers | nothing | yes |
    | Fixing `nexum_sys_mmap` (see below) | nothing | yes |
    | The thread stack allocator (`mmap`, guard page, exit stack) | the fixed `mmap` wrapper | yes |
    | The per-thread control block and the "current thread" accessor | **11a's freestanding startup (TLS)** | no |
    | The trampoline and the compiler-generated thunk | the PCB | no |
    | The thread-local collector (recommendation 10) | the PCB | no |
    | The shared-heap stop-the-world collector (11e.2) | the above plus the compiler changes in the change list | no |
    | Channels, the work-stealing pool | the above | no |
    | Sharing a numeric buffer between threads | **11d's pointer type** | no |

    The last row is the uncomfortable one: today a `Float` array is a value
    type lowered to an LLVM array (`crates/nexum-codegen/src/lib.rs:17`), so
    it lives in an `alloca` on the stack. A million-element `Float` buffer is
    an eight-megabyte stack object, and there is no way to hand a buffer to
    another thread. **11e without 11d can only parallelise scalar work.**
    For the workload the roadmap names — training and inference — 11d is a
    hard prerequisite for 11e to be useful, and that ordering should be
    decided by the orchestrator rather than discovered later.

17. **What the collector's design has to change.** This is the hard part and
    it must not be discovered during implementation. The full list is in
    "The collector under concurrency" below; the summary is: the root list
    becomes per-thread, a thread registry appears, a stop-the-world protocol
    appears, the compiler has to insert poll points, the window between a
    call returning and its temporary root being spilled has to be closed, and
    the object-list mutation has to be excluded from the collector's sweep.

18. **Windows and macOS are documented limitations in 11e.1.** The compiler
    rejects `spawn` on those targets with a clear diagnostic, and the
    language reference says so. The Windows answer exists — `ntdll`'s
    `RtlCreateUserThread`, with `NtWaitForSingleObject` for the join — but it
    is a call into a DLL, not a system call, and D013 says the operating
    system is reached through inline assembly, never through a C binding. The
    Windows backend of `nexum_spawn` is therefore written behind the same
    interface and filled in when 11a.2 generates the ntdll stubs.
    *Reason:* using `CreateThread` from kernel32 would work today (the
    Windows build still links the UCRT) but would violate D013 and would
    have to be thrown away in 11a.2.

## What the code looks like today

### The system call layer

`crates/nexum-runtime/src/syscalls.rs` is the model for reaching the kernel,
and it is already shaped for what 11e needs:

* The numbers live in a per-system table, `Syscalls::for_os`
  (`syscalls.rs:58`), because Linux and Darwin number their calls
  differently. 11e adds `clone`, `futex`, `sched_yield`, `gettid` and
  `sched_getaffinity` to it.
* The register layout lives in a per-(system, architecture) table,
  `layout` (`syscalls.rs:110`): the argument registers, the result register,
  the register the number goes in, the instruction, and the clobbers. This
  is where the `clone` argument-order difference between x86-64 and AArch64
  belongs — the table is already per architecture, so the difference has a
  home.
* The wrapper generator, `call_body` (`syscalls.rs:168`), pins each argument
  to a register through the constraint string, writes the call number as an
  immediate, aligns the stack to 16 bytes for the call, and restores it
  afterwards. 11e adds two wrappers in exactly this style.
* `wrappers` (`syscalls.rs:237`) emits every wrapper with the same four
  parameters, `i64 %a0` to `i64 %a3`. **`mmap` needs six arguments and
  `clone` needs five**, so the generator has to grow a per-call parameter
  count. This is not optional: `mmap` is wrong today.
* The runtime only reaches the POSIX system calls on the non-Windows path
  (`crates/nexum-runtime/src/lib.rs:75`); the Windows path still goes
  through the C library for files.

**`nexum_sys_mmap` is broken and 11e needs it.** The emitted function has
four parameters, but the assembly block has five operands and ten
constraints:

```llvm
define internal i64 @nexum_sys_mmap(i64 %a0, i64 %a1, i64 %a2, i64 %a3) {
  %r = call i64 asm sideeffect "...", "={rax},{rdi},{rsi},{rdx},{r10},{r8},~{rcx},~{r11},~{r12},~{memory}"(i64 %a0, i64 %a1, i64 %a2, i64 -1, i64 0)
```

The comment at `syscalls.rs:277` says "the file descriptor is -1 and the
offset is 0", but `-1` is placed in argument 3 and `0` in argument 4. On
x86-64 `mmap` reads `rdi=addr, rsi=len, rdx=prot, r10=flags, r8=fd, r9=offset`,
so the wrapper would pass **flags = -1 and fd = 0** (stdin) and leave the
offset in `r9` undefined. It also puts the constants in the wrong slots
entirely: fd is argument 4 and offset is argument 5. The wrapper is unused
today, which is why nothing fails — and the LLVM verifier does **not**
catch it: a cross-target build of `examples/hello.nx` with
`nx build --target linux-x86_64` succeeds with this malformed function in
the module. 11e needs `mmap` for the thread stacks, so this has to be
fixed first, and the fix is independent of everything else in 11e.

### The collector

The collector is a non-moving mark-sweep collector with a shadow stack of
root frames. It is entirely global today, and every one of its globals is a
concurrency hazard:

* `@nexum_obj_head` (`crates/nexum-runtime/src/lib.rs:104`) — the global list
  of every object. `nexum_alloc_scanned` (`lib.rs:147`) links the new object
  at the head with a load and two stores, unsynchronised. Two threads
  allocating at once both read the old head, and the second store wins: the
  first thread's object is **lost from the list**. The chain itself stays
  acyclic — `next` is written once, at allocation — so the immediate
  consequence is a leak rather than a crash: the lost object is never swept,
  it is never even visible to the mark phase, and `@nexum_live` drifts away
  from the truth.
* `@nexum_roots_head` (`lib.rs:105`) — **one** root list for the whole
  process. `nexum_roots_push` (`lib.rs:136`) pushes at the head and
  `nexum_roots_pop` (`lib.rs:139`) pops the head and restores the frame's
  `prev` pointer. With two threads, thread B's pop removes thread A's
  frame and restores *A's* `prev` — the chain is corrupted permanently, the
  collector walks frames that are no longer live, and a live object is
  freed. This is the specific question the collector raises: **two threads
  collecting at the same time is not a race that a lock around the
  collection fixes** — see the analysis below.
* `@nexum_live`, `@nexum_since`, `@nexum_threshold` (`lib.rs:106`, `:107`,
  `:108`) — plain `i64` counters updated with unsynchronised
  read-modify-write in the allocator. Lost updates mean the watermark is
  wrong: either the collector never runs or it runs constantly.
* `nexum_gc_maybe` (`lib.rs:155`) — the watermark check inside every
  allocation.
* `nexum_collect` (`lib.rs:174`) — walks the global root list, then
  `nexum_gc_trace` (`lib.rs:180`) walks the global object list to a
  fixpoint, then `nexum_sweep` (`lib.rs:186`) frees the unmarked ones.
* `malloc` and `free` (`lib.rs:53`, `:54`) — the C library's allocator.
  glibc's is thread-safe, but the runtime's own allocator (phase 11a.2) will
  have to be.

The compiler side is friendlier than it looks:

* The prologue pushes the root frame (`crates/nexum-codegen/src/lib.rs:456`)
  and every return pops it (`codegen/src/lib.rs:1194`). **The signatures of
  `nexum_roots_push` and `nexum_roots_pop` do not have to change** for the
  collector to become per-thread: the runtime can keep the same interface
  and store the head in the per-thread control block.
* A call whose result holds references is spilled into a rooted slot
  immediately after the call (`codegen/src/lib.rs:317`, `:407`). That is the
  temporary-root rule from `docs/runtime-abi.md` §3, and it is what makes
  the soundness argument in §4 work.
* The soundness argument itself: "every live reference is inside a scanned
  range". It holds at a safepoint. The one exception is the window between
  a call returning and the spill that follows it, and that window is the
  collector's worst problem under concurrency — see below.

### The language surface

* There are no globals: only `fn` declarations at the top level are
  supported (`crates/nexum-sema/src/lib.rs:274`). A spawned function can
  therefore only reach its data through its arguments.
* There are no closures and no function-pointer type, so `spawn` has to name
  the function directly.
* There is no pointer type yet (that is phase 11d), so a thread cannot be
  handed a buffer.
* The type enum (`crates/nexum-sema/src/lib.rs:15`) has `Int`, `Float`,
  `Bool`, `Str`, `Unit`, `Record`, `Array`, `List`, `Never`. `Thread` and
  `AtomicInt` are two new opaque types, both `ptr` in the ABI.
* `has_refs` (`codegen/src/lib.rs:23`) decides whether the collector scans a
  type. `Thread` and `AtomicInt` hold no references, so they are not
  registered as roots — which is what lets a `Thread` handle cross between
  threads without the collector seeing it.
* `docs/runtime-abi.md` §8 lists "Single-threaded only" as a current
  restriction, and `docs/language-reference/decisions.md` D010 says the
  collector is "non-moving, single-threaded". Both need a follow-up entry.

## The design

### Thread creation, on Linux

```
nexum_spawn(entry: ptr, args: ptr) -> ptr        ; returns the thread handle
```

1. The parent allocates the argument buffer (native memory, not collector
   memory) and fills it with the arguments in their ABI layout.
2. The parent allocates the thread stack with
   `mmap(NULL, size, PROT_READ|PROT_WRITE, MAP_PRIVATE|MAP_ANONYMOUS, -1, 0)`
   and puts one `PROT_NONE` guard page below it with `mprotect`.
3. The parent writes the entry pointer and the argument pointer at the top of
   the stack, sixteen-byte aligned so that the trampoline's stack satisfies
   the ABI at the call.
4. The parent builds the child's TLS block (via 11a's
   `nexum_tls_child_block()`) and calls
   `clone(0x3D0F00, child_stack, &rec.tid, &rec.tid, child_tls)` — the
   `parent_tid` and the `child_tid` arguments are the *same* word, the
   thread record's tid word. The return value is the child's tid in the
   parent and 0 in the child.
5. The parent returns the handle. The tid word has already been filled in
   by the kernel by the time `clone` returns.

The child, on return from the syscall, has `rax == 0` and the new stack
pointer. It must not return from `clone`, so the trampoline takes over
immediately.

### The trampoline and the calling convention

The trampoline is per architecture and is written in inline assembly, in the
style of the system call layer (D013, D014):

* x86-64: the child's `rsp` points at the entry word.
  `mov (%rsp), %rax` (entry), `mov 8(%rsp), %rdi` (argument),
  `add $16, %rsp`, `call *%rax`.
* AArch64: `ldr x0, [sp]`, `ldr x1, [sp, #8]`, `add sp, sp, #16`,
  `blr x0`.

The trampoline then:

1. stores the child's PCB into the thread-local slot;
2. initialises the child's root-list head, object-list head and watermark;
3. calls the entry (the compiler-generated thunk) with the argument pointer;
4. on return, lets the kernel clear the tid word (which it does on the
   child's exit, with the futex wake that wakes the joiner), so the runtime
   writes no join state of its own;
5. switches to the exit stack, unmaps the thread stack, and calls the
   `exit` syscall — **`exit`, not `exit_group`**: `exit_group` ends every
   thread in the process, which would take the whole program down.

A panic inside a thread is the exception: `nexum_panic`
(`crates/nexum-runtime/src/lib.rs:121`) calls `exit(1)`, which in a thread
would end only that thread and let the program carry on with a dead worker.
A panic means the program is broken, so the runtime must end the whole
process there: the per-thread control block records that this is a thread,
and the panic path calls `exit_group` instead of `exit`.

The user's function keeps the ordinary Nexum calling convention; the thunk
absorbs the difference:

```llvm
define internal void @worker.__spawn_thunk(ptr %args) {
entry:
  %n = load i64, ptr %args              ; the first argument, in its ABI layout
  call void @worker(i64 %n)
  ret void
}
```

The argument buffer is freed by the child after the thunk has read it (or by
the trampoline, right after the thunk returns — the trampoline is the better
place, because the thunk may have kept a pointer into it).

### The thread stack

* Allocated by the parent with `mmap`. Default 8 MiB, overridable.
* One `PROT_NONE` page below the stack as a guard.
* Released by the child, after it has switched to a separate one-page exit
  stack. The parent never frees it.
* The thread *record* (the tid word, the handle, the per-thread control
  block) is allocated by the parent, is not on the child's stack, and is
  freed by the parent in `join`.

### Thread-local storage

The runtime needs exactly one thread-local in 11e: a pointer to the
per-thread control block. It is declared in the runtime IR as

```llvm
@nexum_tls_pcb = thread_local global ptr null
```

and read through a helper `nexum_current_pcb()`. A direct load from a
`thread_local` global is lowered by the backend to the ELF TLS access for the
target, so the runtime never hard-codes `mov %fs:offset`; the *layout* is
the linker's business.

There is one condition on that: the backend can only use the local-exec or
initial-exec model, which needs the program to be linked statically (or at
least not to be a shared object that uses dynamic TLS). The general-dynamic
model would call `__tls_get_addr`, which is C library code and is exactly
what D012 forbids. A freestanding program is statically linked, so this is
the normal case — but it is a constraint on the link step, and 11g (the
native linker) has to honour it.

For a `clone` child, the runtime must hand the kernel a block with the same
layout, and the thread pointer the kernel stores is the child's copy. That
means the runtime needs the parent's thread pointer and the extent of the
TLS block — and that is 11a's business, because 11a owns the freestanding
startup and therefore the TLS layout. 11e's contract with 11a is one
function:

```
nexum_tls_child_block() -> ptr    ; the child's thread pointer, or null
```

11a also has to answer the question the layout forces: on x86-64 the thread
pointer is read with `arch_prctl(ARCH_GET_FS, &v)` (158) and on AArch64 it is
read with `mrs x0, TPIDR_EL0`, which is readable from EL0. The runtime
copies the parent's block into the child's, so the child's thread-local
variables start with the parent's values — which is what glibc does, and what
a program expects.

### The join primitive

```
nexum_join(thread: ptr) -> void
```

The thread record holds one four-byte `tid` word, and that word is the join
mechanism. `clone` is handed the same address as both the `parent_tid` and
the `child_tid` argument, so:

* at creation the kernel writes the child's tid into it
  (`put_user(nr, args->parent_tid)`, `kernel/fork.c:2796`);
* at exit the kernel clears it and issues a futex wake on it
  (`p->clear_child_tid = args->child_tid`, `kernel/fork.c:2174`).

The join is therefore:

```
tid = load(rec.tid)                       ; non-zero while the thread lives
while tid != 0:
    futex(FUTEX_WAIT_PRIVATE, &rec.tid, tid, absolute_deadline)
    tid = load(rec.tid)
```

`FUTEX_WAIT` compares the word with the expected value and returns
`EAGAIN` at once if they differ, so the loop needs no extra logic. The
timeout is an *absolute* `CLOCK_MONOTONIC` deadline, not a relative
interval; a relative timeout would return immediately. The wait must use a
bounded timeout rather than block forever: the wait loop is also the
natural place to poll the collector's stop request, and a bounded wait is
what lets a thread blocked in `join` still take part in a stop-the-world
collection.

The runtime keeps no second piece of join state. If 11e.3 ever needs a
result value, it goes in a second word and the child writes it *before* it
exits, so the tid word's clearing is still the last thing that happens.

`join` returns `Unit`. A thread that wants to send a value back uses a
channel (11e.3); a thread that returns a managed value would have to publish
it into the collector, which is 11e.2 work.

### The collector under concurrency

The question is what happens when two threads exist and the collector is
exactly what it is today: one global object list, one global root list, one
global watermark. Three options were considered.

#### Option (i): a global lock around the collection

*What it gives.* One collector at a time; if every allocation takes the same
lock, the object list is not mutated while the collector walks it.

*What it does not give.* It does not stop the other threads, and stopping
them is the actual requirement. Two specific failures:

1. *The collector reads a frame that is being popped.* The root frames live
   on each thread's stack (`docs/runtime-abi.md` §3). Thread B returns from a
   function, pops its frame (`codegen/src/lib.rs:1194`), and the frame's
   memory is immediately reused by the next call. The collector, walking B's
   root list, reads slots that no longer hold roots. A live object is then
   unmarked, freed, and the program reads freed memory.
2. *The collector misses an object that is in flight.* A call that returns a
   reference has its result spilled into a rooted slot immediately after the
   call (`codegen/src/lib.rs:317`). Between the call returning and the
   spill, the object is in a register and in no root. If the collector runs
   in that window, it frees the object and the thread then spills a dangling
   pointer into its root slot.

A lock around the collection fixes neither. **Verdict: necessary but not
sufficient.** The lock is still needed — it serialises the collectors and
protects the object list — but it cannot be the whole answer, and a design
that believes it is will produce a use-after-free that reproduces once every
few thousand collections.

#### Option (ii): per-thread allocation with a shared collect

*What it gives.* Each thread allocates from its own free list or arena, so
the allocation fast path has no global contention; one collector walks all
the heaps.

*What it does not give.* The collector still needs every thread's roots at
one instant, so it still needs the stop-the-world of option (i) — and it now
has to sweep several heaps. The mark phase has to walk each thread's object
list while that thread is parked, and the object lists are per-thread, so
each one needs its own exclusion.

**Verdict: strictly more work than option (i) for exactly the same safety.**
It is worth doing only when allocation contention is measured to matter, and
it is the natural shape if the collector later becomes generational. It
should not be the first step.

#### Option (iii): the collector is thread-local

*What it gives.* Each thread has its own heap, its own object list, its own
root list, its own watermark, its own collector. Nothing is shared, so
there is no collector lock, no stop-the-world, no poll in the compiler, and
no change to the mark/sweep algorithm. The change to the runtime is
mechanical: the five globals at `lib.rs:104`–`lib.rs:108` move into the
per-thread control block, and `nexum_roots_push`/`nexum_roots_pop`
(`lib.rs:136`, `:139`) read and write the current thread's head instead of a
global. **The compiler does not change at all** — the root frame protocol,
the temporary-root spill and the safepoint rule all stay as they are.

*What it costs.* A managed object created in one thread cannot be used in
another: the owner's collector does not know that the other thread holds a
reference, and will free it.

*How the cost is contained.* By a type rule the checker can enforce exactly:
**a managed value never crosses a thread boundary.** `spawn` accepts only
shareable arguments — `Int`, `Float`, `Bool`, records and arrays of those,
and `AtomicInt` — and the spawned function must return `Unit`. The predicate
this needs already exists in the back end — `has_refs`
(`codegen/src/lib.rs:23`) answers "does this type hold a reference" — and
the checker keeps the same record table (`crates/nexum-sema/src/lib.rs:450`),
so it needs the same predicate in front of it. The spawn's argument types
are known statically, so the rule is checkable, and the model is sound
rather than "undefined behaviour if you are careless".

**Verdict: this is the 11e.1 recommendation.** It is the only one of the
three that can be implemented without rewriting the collector, and it is
sound precisely because the restriction is a static rule. Its cost is
concentrated where it hurts least: the workload the roadmap names (training
and inference) moves `Float` data, not `Str` and `List`.

#### The end state: the shared heap (11e.2)

A shared managed heap needs a stop-the-world collector, and that is a
substantial change to both the runtime and the compiler. The changes, in
full:

1. **`@nexum_roots_head` becomes per-thread.** `nexum_roots_push` and
   `nexum_roots_pop` (`lib.rs:136`, `:139`) use the current thread's head.
   *No compiler change.* The root frame layout is unchanged.
2. **A thread registry.** A global list of thread records, each with its
   root head, its state and its park/resume words. The collector walks it.
3. **A stop-the-world protocol.** A global "stop requested" word. Every
   thread polls it at a safepoint; the collector requests the stop, waits
   for every other live thread to be parked, collects, then releases the
   stop and wakes them.
4. **Poll points in the compiler.** The poll must be placed where the root
   set is complete: at function entry and at loop back edges. It must
   **not** be placed between a call and the temporary-root spill that
   follows it (`codegen/src/lib.rs:317`), because at that point the object
   is not yet rooted. A thread that is stopped in that window would have
   its object freed underneath it.
5. **The in-flight window has to be closed.** The cheapest way is a single
   extra slot in the per-thread control block: the runtime stores the object
   it is about to return into `pcb.inflight` before returning, the compiler
   clears it after the spill. The collector treats `pcb.inflight` as a root.
   This costs one store per allocation and changes no ABI. The alternative
   — the allocating runtime functions take the caller's root slot and
   publish the result into it under a global collector lock — closes the
   window without the extra slot but serialises every allocation and is an
   ABI change to the runtime and to the compiler's call lowering. **The
   in-flight slot is the recommendation.**
6. **The object-list mutation must be excluded from the sweep.** A thread
   preempted inside the allocator's critical section would otherwise link a
   new object into the list while the collector sweeps it. The fix is an
   allocation lock: the collector takes it for the whole collection, and
   the poll points are placed *outside* the allocator's critical section so
   that a parked thread is never inside it. The collector's wait for the
   lock is a wait for a *running* thread to finish its critical section, not
   for a parked thread, so there is no deadlock.
7. **The counters become per-thread or atomic.** `@nexum_live`,
   `@nexum_since` and `@nexum_threshold` (`lib.rs:106`–`:108`).
8. **The allocator becomes thread-safe.** Phase 11a.2's allocator must be:
   a per-thread arena with a lock-free slow path, or a mutex-protected
   global free list. `malloc`/`free` (`lib.rs:53`) are thread-safe today;
   the replacement is not, by default.
9. **The `collect()` builtin and every allocation are poll points** — they
   already are safepoints (`docs/runtime-abi.md` §3); the poll has to be
   added at both.
10. **Blocking calls must be safepoint-aware.** A thread blocked in a
    system call cannot park, so the collector cannot finish. The runtime's
    own blocking calls (the futex wait in `join`, and anything else that
    waits) must use a bounded wait and re-poll. **File I/O is a documented
    limitation**: a thread blocked in `read` holds up every collection until
    the read completes.
11. **The mark phase's cost is visible.** `nexum_mark_ptr` (`lib.rs:162`)
    walks the whole object list for every candidate pointer, so a
    collection is quadratic in the number of objects and candidates. Under a
    stop-the-world that pause is now visible to every thread. This is worth
    measuring before 11e.2, not after.

None of these eleven is a detail; each of them is a decision that has to be
made before the code is written.

### The atomics

**Operations.** Load, store, exchange (swap), compare-exchange, fetch-add,
fetch-sub. Fetch-and/or/xor and fetch-min/max are available in LLVM and can
be added when someone needs them; LLVM's `atomicrmw umin`/`umax` are
recent, so the target baseline should be checked before offering them.

**Widths.** `AtomicInt` is `i64` in 11e. When the numeric tower lands
(phase 11c) the widths multiply; the clean answer is one atomic type per
integer width, which is a lot of type names, or a single atomic type whose
width is fixed at `i64` until a generic atomic arrives. **Recommendation:
one `AtomicInt` at `i64` in 11e, revisited in 11c.**

**Alignment.** LLVM requires an atomic access to be naturally aligned and of
a power-of-two size. The arena hands out 8-byte-aligned cells, so this is
free.

**Memory orderings.** Relaxed, acquire, release, acq_rel, seq_cst. The
default is seq_cst, as in C11 and Rust. LLVM's spelling of relaxed is
`monotonic`. The compiler must not weaken an ordering: a `release` store
stays a release store, and the `acquire` side of a load stays an acquire.
This is the "language contract, not a backend-specific convenience" the
architecture note asks for.

**The mapping.** The table in recommendation 11 is the whole mapping; there
is no runtime involvement at all. An atomic operation is a compiler intrinsic
in the same way `print` is a builtin today.

**Where the cell lives.** A shared arena, bump-allocated with an atomic
`fetch_add`, cells never freed. It is outside every collector heap, so it
holds no references, needs no rooting, and can be handed to another thread.

### The data race story

The language does not detect races, and it says so in the language reference
and in a decision record. The reasoning:

* A race detector needs a happens-before model, an instrumentation pass over
  every memory access, and a runtime that records shadow state. The
  sanitizers live in compiler-rt, which is a C library; a program that links
  it no longer "depends on nothing but the kernel" (D012). The toolchain may
  offer it as an optional feature — the toolchain's business — but the
  language must not require it, and 11e must not assume it is there.
* Precise detection is impossible anyway in a language with conservative
  scanning inside aggregates (`docs/language-reference/decisions-gc.md`): a
  detector would have to know which words are references.

What the language *does* guarantee:

* **Memory safety.** No dangling pointers, no out-of-bounds access, no
  double free. A data race cannot produce a crash through the collector.
* **Atomicity.** The atomic operations are atomic, whatever the orderings.
* **A defined-but-unspecified outcome for a racy non-atomic access.** The
  value read is one of the values written. It is not undefined behaviour in
  the C sense, because the language defines it.

The safe subset, in the order a programmer should reach for it:

1. **Partition the data.** Each thread owns its slice. No shared writes, no
   races, and the best locality. This is the default the architecture note
   asks for ("concurrency follow ownership/data partitioning where
   possible").
2. **Atomics for the shared scalars.** Flags, counters, work-stealing
   indices. The rule that makes it sound: *a location that is accessed
   atomically is never accessed non-atomically*.
3. **Channels for everything else.** A channel operation is a
   synchronisation point (release on the send, acquire on the receive), so
   the data it carries is safely published. Channels are 11e.3.
4. **Read-only sharing.** Several threads reading a buffer nobody writes.

The unsafe part is concurrent non-atomic access to shared native memory —
two threads writing the same cell, or one writing while another reads. When
phase 11d lands `unsafe` blocks, that is what they should mark. The atomics
themselves are not unsafe.

### The scheduler

There is none in 11e.1, and that is a decision rather than an omission.

`spawn` creates an OS thread. `join` blocks. The operating system's
scheduler decides which thread runs where, and the program decides how many
threads to create. That is enough for the workload the roadmap names: N
threads, each taking a slice of the work, joined at the end.

The roadmap lists "a scheduler" in 11e. The right home for it is the
standard library, written in Nexum, as the roadmap itself says. A
work-stealing pool needs a queue, a futex and atomics — all of which 11e.1
provides — and writing it in Nexum is what proves the primitives are
sufficient. The runtime should provide only what a pool cannot: `spawn`,
`join`, atomics, `cpu_count()`, and `yield()`.

### The surface syntax and the lowering

```
spawn_stmt = "spawn" identifier "(" [ args ] ")"
```

`spawn` is a statement-level form, not a general expression: the callee
must be known at compile time so the thunk can be emitted. `join` and the
`atomic_*` family are ordinary builtins.

Lowering of `spawn worker(7)`:

1. The compiler resolves `worker` to its LLVM symbol and works out the
   argument layout from the signature (`sigs` in
   `crates/nexum-codegen/src/lib.rs:140` already carries the per-parameter
   modes).
2. It emits the thunk `@worker.__spawn_thunk(ptr %args)`, which loads the
   arguments out of the buffer and calls `worker`.
3. It allocates the argument buffer (native memory), fills it, and calls
   `nexum_spawn(ptr @worker.__spawn_thunk, ptr %buf)`.
4. `nexum_spawn` returns the thread handle, which the compiler types as
   `Thread`.

Lowering of `atomic_fetch_add(counter, 1)`:

1. The compiler resolves `counter` to the cell pointer and emits
   `%old = atomicrmw add ptr %cell, i64 1, align 8 seq_cst`.
2. The result is the previous value.

Lowering of `atomic_compare_exchange(counter, expected, desired)`:

1. `%r = cmpxchg ptr %cell, i64 %expected, i64 %desired, align 8 seq_cst seq_cst`
2. The success flag is `%r.1`; on failure the compiler stores `%r.0` back
   into the `expected` slot, because the language's contract is that
   `expected` is updated with the value that was found.

The two new types are opaque and hold no references, so `has_refs`
(`codegen/src/lib.rs:23`) returns false for them and they are never
registered as roots — which is what allows a `Thread` handle and an
`AtomicInt` to be shared without the collector's involvement.

### The runtime entry points

| Name | Signature | Purpose |
| ---- | --------- | ------- |
| `nexum_spawn` | `(ptr entry, ptr args) -> ptr` | create a thread, return the handle |
| `nexum_join` | `(ptr thread) -> void` | wait for a thread |
| `nexum_current_pcb` | `() -> ptr` | the current thread's control block |
| `nexum_tls_child_block` | `() -> ptr` | **11a's**: the child's thread pointer |
| `nexum_cpu_count` | `() -> i64` | the number of usable cores |
| `nexum_yield` | `() -> void` | `sched_yield` |
| `nexum_thread_trampoline` | `() -> void` | the per-architecture child entry |
| `nexum_sys_clone` | the system call layer | `clone` |
| `nexum_sys_futex` | the system call layer | `futex` |
| `nexum_sys_exit_thread` | the system call layer | `exit`, not `exit_group` |

## Risks

* **The TLS layout is the biggest unknown, and it is not in 11e's scope.**
  If 11a does not deliver a usable thread-local, 11e cannot have per-thread
  collector state at all, and the fallback (recommendation 7) is a large
  change to every signature in the language. **This is the dependency the
  orchestrator has to look at first.**
* **The thread-local access model constrains the link step.** The backend
  can only use local-exec or initial-exec TLS, which needs a statically
  linked program; the general-dynamic model would call `__tls_get_addr`,
  which is C library code. If a Nexum program is ever linked as a shared
  object or against a dynamic loader, the thread-local access stops
  working. 11g (the native linker) has to keep the program static.
* **The `clone` argument order is per architecture.** Getting it wrong on
  AArch64 is silent: the kernel takes the child-tid pointer as the thread
  pointer, and the child dies on its first thread-local access, or appears
  to work until it does not. The fix is to put the order in the per-
  architecture table next to the register layout, and to have a test that
  fails loudly if the two architectures disagree.
* **`exit_group` instead of `exit`** ends the whole process. It is one
  number in a table.
* **A malformed `mmap` wrapper is not caught by the verifier.** The
  existing `nexum_sys_mmap` passes flags and fd in the wrong registers and
  the verifier accepts it, because it is unused. 11e must use `mmap` for
  the thread stacks, so the wrapper has to be fixed first — and the general
  lesson is that an unused wrapper in the runtime is not tested by anything.
* **The child cannot unmap its own stack** without first switching away
  from it. Forgetting the exit stack is an immediate crash in the child
  after it has done all its work, which is the most confusing possible
  failure.
* **The `futex` timeout is absolute.** A relative timeout returns
  immediately, and the `join` becomes a spin loop that burns a core.
* **The collector's mark phase is already quadratic**
  (`nexum_mark_ptr` walks the whole object list per candidate pointer). Under
  a stop-the-world that pause becomes visible to every thread. It should be
  measured before 11e.2, not after.
* **There is no race detector.** A user will eventually file a bug that is a
  data race. The language reference has to say this in the same sentence as
  the atomics, not in a footnote.
* **11e without 11d can only parallelise scalars.** A `Float` array is
  today a stack value (`codegen/src/lib.rs:17`), so there is no way to share
  a numeric buffer between threads, and a large one does not fit on the
  stack. For the workload the roadmap names, 11d is a prerequisite.
* **The trampoline is per architecture and is assembly.** The stack
  alignment and the argument order have to be right on the first try; there
  is no compiler to check them.
* **macOS has no answer.** Darwin's `clone` is a different, undocumented
  ABI, and the realistic freestanding path is the Mach thread API. If macOS
  matters for 11e, that is a research item of its own.

## Open questions

Each of these has a recommendation, so the orchestrator can take them as
decisions rather than as questions.

1. **Does 11e wait for 11d (pointers) and 11a's TLS work, or does it land
   the atomics and the syntax first?**
   *Recommendation:* land the atomics and the surface syntax first — they
   depend on nothing and are the part of 11e that is genuinely
   self-contained — then the threads once 11a's TLS interface exists. 11e
   without 11d is still worth doing for the atomics and for the thread
   plumbing, but the orchestrator should not promise "training uses the
   cores" until 11d has landed.
2. **Is the 11e.1 collector model the thread-local one (recommendation 10),
   or is the shared heap worth doing in 11e?**
   *Recommendation:* the thread-local one. The shared heap needs the eleven
   changes listed above, including poll insertion in the compiler, and it
   would turn a phase about threads into a phase about the collector.
3. **Does `join` return the thread's value, or `Unit`?**
   *Recommendation:* `Unit` in 11e.1. Returning a managed value means the
   child publishes into the collector, which is the 11e.2 problem; returning
   a native value is possible but has nowhere to put it without a
   first-class "result box" in the surface. Channels solve this in 11e.3.
4. **Is the atomic type `AtomicInt` at `i64` only, or does it follow the
   numeric tower?**
   *Recommendation:* `AtomicInt` at `i64` in 11e; revisit in 11c when the
   integer widths exist. One type name now, the right set later.
5. **Do the ordering variants get separate names, or one function with an
   ordering argument?**
   *Recommendation:* separate names, following the builtin style the
   language already has (`atomic_load`, `atomic_load_relaxed`,
   `atomic_load_acquire`, `atomic_load_seq_cst`). An ordering argument would
   need an ordering value type, which the language does not have.
6. **Is the macOS limitation acceptable?**
   *Recommendation:* yes for 11e.1, provided it is stated in the language
   reference and the diagnostic says why. The alternative — the Mach thread
   API — is a research item that should not hold up the phase.
7. **Is the "no managed value crosses a thread boundary" rule enforced in
   the type checker, or documented?**
   *Recommendation:* enforced. It is a static check on the spawn's argument
   types, the back end already has the `has_refs` predicate it needs, and an
   unenforced rule is an unsoundness. It also has to be a decision record
   (D017 or later), because it is a visible restriction on the language.
8. **Does the phase include channels, as the roadmap's phase line says
   ("threads, atomics, a scheduler, channels")?**
   *Recommendation:* no. Channels need a queue with a lock and a condition
   variable, and under the thread-local collector a channel can only carry
   native values or copies. The scheduler is a standard-library library in
   Nexum, built on channels. Recommend 11e.1 = threads, join, atomics,
   `cpu_count`, `yield`; 11e.2 = the shared collector; 11e.3 = channels;
   11e.4 = the pool in the standard library.
9. **Is `spawn` allowed to name any function, or only functions with the
   right shape?**
   *Recommendation:* any function whose parameters are all shareable and
   whose return type is `Unit`. The compiler emits the thunk, so the
   function's own shape does not matter.
10. **Should the thread stack size be a program-level setting?**
    *Recommendation:* a builtin `thread_stack_size(bytes)` that applies to
    threads created after the call, with 8 MiB as the default and
    `prlimit64(RLIMIT_STACK)` as the fallback. 8 MiB is the Linux default
    and a thread that needs more is a thread that should be using an arena
    (phase 11d, D009).

## Changes needed elsewhere

* `crates/nexum-runtime/src/syscalls.rs`: add `clone`, `futex`,
  `sched_yield`, `gettid`, `sched_getaffinity` to `Syscalls::for_os`
  (`syscalls.rs:58`); add the per-architecture `clone` argument order to
  `layout` (`syscalls.rs:110`) or a table beside it; grow the wrapper
  generator (`syscalls.rs:237`) so a call can have more than four
  arguments; **fix `nexum_sys_mmap`**, which currently passes the file
  descriptor and the offset in the wrong registers (`syscalls.rs:277`).
* `crates/nexum-runtime/src/lib.rs`: the thread entry points
  (`nexum_spawn`, `nexum_join`, `nexum_current_pcb`, `nexum_cpu_count`,
  `nexum_yield`, the trampoline); the per-thread control block; and, for
  11e.1, the collector's globals (`lib.rs:104`–`lib.rs:108`) moving into
  the control block and `nexum_roots_push`/`nexum_roots_pop`
  (`lib.rs:136`, `:139`) using it.
* `crates/nexum-sema/src/lib.rs`: the `Thread` and `AtomicInt` types
  (`lib.rs:15`); the "shareable" check on `spawn`'s arguments; the
  builtin arities for `join` and the `atomic_*` family.
* `crates/nexum-codegen/src/lib.rs`: the `spawn` lowering and the
  compiler-generated thunk; the atomic lowerings; `llvm_ty` (`lib.rs:8`) and
  `has_refs` (`lib.rs:23`) learning the two new opaque types.
* `crates/nexum-parser/src/lib.rs`: the `spawn` statement form and the new
  keywords, if any.
* `docs/language-reference/grammar.md`: the `spawn` form, the new builtins,
  the two new types.
* `docs/language-reference/decisions.md`: a decision record for the
  thread-local collector and the "no managed value crosses a thread
  boundary" rule, and an update to D010 ("single-threaded").
* `docs/runtime-abi.md`: §0 (the C library table — `pthread_create` is not
  on the list because it is never used), §2 (the collector's thread-local
  state), §3 (roots and safepoints in a thread), §8 (the
  "single-threaded only" restriction is withdrawn or narrowed).
* `docs/native-roadmap.md`: the phase 11e line, if the slicing in open
  question 8 is adopted.
* Phase 11a: the freestanding startup must provide
  `nexum_tls_child_block()` and a valid thread-local for the initial
  thread. **11e depends on this.**
* Phase 11d: a pointer type, or some other way to share a numeric buffer
  between threads, before 11e is useful for the workload the roadmap names.
