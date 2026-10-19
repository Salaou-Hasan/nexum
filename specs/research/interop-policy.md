# Interoperability without a C binding

**Question.** Nexum deliberately has no C binding: no `extern`, no foreign
function type, no way to name or call a symbol that lives outside the program.
The question this document answers is what to do about that — whether the
programme should grow an FFI, and if so what kind, when, and under what
bounds. It is not academic: it decides whether the language can be used for
anything that touches the outside world (a database, a display, a device, an
existing library), and the answer has to be a policy, not a deferral.

**Summary.** The recommendation is that the language grows **no binary FFI**,
ever, as a matter of policy: interoperability in Nexum is done by reaching the
machine directly (inline assembly, pointers and `unsafe`, the runtime layer
built on them) or by speaking a **binary protocol** to an external process or
device over a channel the program owns. There is exactly **one** bounded
exception, and it is not an FFI: an opt-in **device link** — a small, fixed
set of entry points in a *Nexum-defined* ABI (not the C ABI) that the user
links explicitly, off by default, never required by the standard library, and
reachable only from `unsafe` code — reserved initially for the GPU path. The
source-level route (compiling a `.c` file with the system compiler and linking
it) is rejected outright: it puts the C library and the system compiler back
on a program that phase 11a exists to free. The hard consequence to accept
now is that protocol-level interop needs a **transport the runtime does not
have yet** (sockets, descriptor passing, `ioctl`), and that gap is not in the
phase list.

## Recommendation

The programme should adopt this house rule, and record it as a numbered
decision (**D017**, provisional) in `docs/language-reference/decisions.md`:

> **D017 — The interoperability currency of Nexum is the machine and the byte
> protocol, not the C ABI.** The language has no `extern`, no foreign function
> type and no C header import, and it will not grow them. A Nexum program
> reaches the outside world in exactly three ways: (1) by reaching the machine
> itself — inline assembly (D013, D014), pointers, `unsafe` regions and
> `volatile` (phase 11d) — and the runtime layer built on them; (2) by
> speaking a binary protocol to an external process or device over a channel
> the program owns; (3) by linking one opt-in native component that exposes a
> **Nexum-defined** ABI rather than the C ABI, reachable only from `unsafe`
> code, off by default and never required by the standard library. The C ABI
> is not part of the language, and the language's semantics never depend on it.

The individual decisions, with the reason for each:

1. **The language does not grow a binary FFI.** Not `extern "C"`, not a
   `foreign fn`, not a C header parser, not a C-ABI shim in the standard
   library.
   *Reason:* an FFI makes the platform's C ABI part of the language's
   contract — its calling convention, its struct layout, its varargs, its
   `errno`. Once the contract is the C ABI, the C library comes with it, and
   phase 11a (the freestanding runtime) becomes a project that is undone by
   every program that uses the FFI. The promise the programme is selling is
   "the language's semantics are ours"; a C ABI in the language makes that
   promise false for exactly the programs that need the outside world most.

2. **Interop is the machine or the protocol.** Two sanctioned routes, both of
   them things the language owns or is about to own:
   * the **machine**: inline assembly (phase 11b), pointers, `unsafe`,
     `volatile` and packed records (phase 11d), and the runtime layer built on
     them — the system call layer is already the template
     (`crates/nexum-runtime/src/syscalls.rs`);
   * the **protocol**: the program speaks a binary protocol — its own or a
     published one — to an external process or device, over a byte channel it
   owns.
   *Reason:* both routes keep the calling convention, the data layout and the
   error convention inside Nexum. The machine route is what the programme has
   already chosen for the operating system (D013: "The operating system is
   reached through inline assembly, never through a C binding"). The protocol
   route is the generalisation of the same decision to everything that is not
   the kernel.

3. **One bounded exception: the device link.** An explicitly linked native
   component exposing a **Nexum-defined** ABI — not the C ABI — with a small,
   enumerated surface (device, queue, buffer, submit, wait). The bounds:
   * **opt-in per build and off by default** (a build flag / manifest field;
     the default build never links one);
   * **never required by the standard library** — the standard library must
     build and run with no device linked at all;
   * **reachable only from `unsafe` code**, and only through phase 11d's
     pointer and `unsafe` machinery;
   * **a versioned ABI of its own** (a new `docs/device-abi.md`, versioned
     separately from the runtime ABI), with a closed set of entry points;
   * **never a general escape hatch**: adding an entry point is a breaking
     change to the device ABI, not a routine extension.
   *Reason:* there is one place where a foreign interface is genuinely needed
   and no protocol-level route exists — Metal on Apple silicon. Without a
   bounded exception the programme's own GPU phase (11h) has no honest answer
   on that platform. The exception is bounded by making it *ours*: we own the
   convention, the surface and the versioning, so the C ABI never becomes a
   language feature. This mirrors the rule the roadmap already applies to the
   toolchain ("the compiler and its tooling may still be written in Rust and
   use LLVM — that is the toolchain's business. The *language* stops
   depending on C", `docs/native-roadmap.md:13-14`); the device link applies
   the same line to one binary component.

4. **Source-level C interop is rejected as a language feature.** The
   toolchain will not compile a `.c` file and link it into a Nexum program.
   *Reason:* it re-introduces the C library *and* the system compiler as build
   dependencies of the program, contradicting D012 (freestanding runtime) and
   D016 (the program is linked by the Nexum toolchain, not by `clang`), and it
   makes the C ABI the interoperability currency all over again. A user who
   needs a C library builds a separate binary and talks to it over a
   protocol — which is recommendation 2 with the C library kept on the far
   side of the boundary.

5. **Callbacks: the language gets no function-pointer type for the sake of
   interop.** Two sanctioned shapes, and both work in a language without
   closures:
   * **no callback at all** — the foreign side answers with *data* over the
     channel. Completion, readiness and progress are messages, not code
     pointers. This is the protocol route's natural shape, and it is why
     protocol-level interop fits a language that has no closures;
   * **the address of a named top-level function**, restricted to the device
     link, called **synchronously on the calling thread**, while the calling
     frame is registered. No closures, no capturing environment, no
     first-class function values — the compiler already knows the symbol name
     of a top-level function, so the only new thing needed is a type to hold
     its address.
   *Reason:* the language has no closures and no function-pointer type
   (`docs/language-reference/grammar.md:85-103`, and `Instr::Call` names its
   callee directly — `crates/nexum-mir/src/lib.rs:66-71`). A general
   function-pointer type would be the first step towards a C ABI, because a
   function pointer is only meaningful together with a calling convention.

6. **The collector rules for any foreign boundary**, stated now so that
   whatever implements it later cannot get them wrong:
   * **the calling frame stays live across the foreign call.** This already
     holds: roots are shadow-stack frames pushed in the prologue and popped
     before every return, and the collector walks the whole frame chain
     (`docs/runtime-abi.md:141-172`,
     `crates/nexum-runtime/src/lib.rs:135-140`, `:174`), so a foreign call
     that re-enters managed code on the same thread sees the caller's roots.
   * **a foreign pointer is an unmanaged value and is never scanned.** The
     mark phase only follows words that match the payload address of a live
     object (`nexum_mark_ptr`,
     `crates/nexum-runtime/src/lib.rs:162`), so a foreign pointer is ignored
     — but the rule is stated anyway: the boundary type is never registered as
     a root and its contents are never scanned.
   * **no managed reference escapes into foreign storage.** A `Str` or `List`
     handed to foreign code that keeps it past the call would dangle the
     moment the collector runs, and the language cannot express "keep this
     alive until the foreign side is done" — arenas are still staged (D009).
   * **no cross-thread callbacks.** The collector is single-threaded and
     non-moving (`docs/runtime-abi.md:118-121`); a callback from another
     thread would race the collector. Cross-thread calls become admissible
     only after 11e, and only once the collector is made concurrent.

7. **Ordering: no interop work before 11d, and none before the transport
   exists.** An FFI is currently *inexpressible*, not merely unimplemented:
   the language has no value that can hold a foreign address — `Int` is `i64`,
   `Str` and `List` are managed pointers, and there is no pointer type and no
   `unsafe` (`docs/native-roadmap.md:44-46`; 11d adds them). Phase 11d is
   therefore the prerequisite for every route that involves a pointer.
   Protocol-level interop additionally needs a transport the runtime does not
   have (see "The design" below).

8. **Make "not callable from C" structural, not merely documented.** Today a
   user function is emitted with external linkage and the platform's default
   calling convention (`define i64 @helper(%Point %arg_p)`), and three
   runtime helpers are global symbols (`nexum_strlen`, `nexum_memcpy`,
   `nexum_strcmp` — deliberately not `internal`,
   `crates/nexum-runtime/src/strings.rs:9-10`). Nothing sanctions this, but
   it makes the "no ABI" claim a matter of documentation rather than
   structure. Recommendation: emit the language's functions as `internal`
   (or under a reserved prefix) when the native linker lands (11g), so the
   absence of an ABI is a property of the binary.

9. **Write it down.** The rule belongs in `decisions.md` as D017, and the
   "what this means for the grammar" note belongs in
   `docs/language-reference/grammar.md` §7 (accepted-but-unspecified), which
   today still lists the module system and error handling as open. No grammar
   change is needed yet, because there is no syntax to change.

## What the code looks like today

Facts from the repository, with the checks that produced them.

### There is no interop surface in the language

* The keyword list is `fn type let mut if else while for in return true false
  and or not import` — there is no `extern`, no `unsafe`, no `asm`
  (`docs/language-reference/grammar.md:19`;
  `crates/nexum-lexer/src/lib.rs:343-359`).
* The type grammar is `identifier` (Int, Float, Bool, Str, Unit, record) or a
  fixed-size array (`docs/language-reference/grammar.md:66-67`). There is no
  pointer type and no function type, so there is nothing that could name a
  foreign address or a foreign function.
* Calls are direct and by name: `Instr::Call { dest, callee: String, args, ty }`
  (`crates/nexum-mir/src/lib.rs:66-71`). The MIR has no indirect call, so
  there is no call-through-a-pointer anywhere in the compiler.
* The roadmap lists the gap explicitly: "No OS access. There is no `extern`,
  no syscall, no inline assembly, no volatile, no pointer type."
  (`docs/native-roadmap.md:44-46`).

### The generated program depends on the C library, and only on it

* The runtime declares the C library's symbols directly:
  `printf`, `fputs`, `fputc`, `exit`, `malloc`, `free`
  (`crates/nexum-runtime/src/lib.rs:49-54`); on Windows it adds
  `__acrt_iob_func`, `fopen`, `fclose`, `fseek`, `ftell`, `fread`, `fwrite`
  (`:63-72`).
* Verified on this checkout (Linux target, a one-line `print` program; the
  commands are shown in POSIX form, they were run on Windows through
  PowerShell):

  ```
  $ nx --target linux-x86_64 emit-ir hello.nx | grep '^declare'
  declare i32 @printf(ptr, ...)
  declare i32 @fputs(ptr, ptr)
  declare i32 @fputc(i32, ptr)
  declare void @exit(i32)
  declare ptr @malloc(i64)
  declare void @free(ptr)

  $ clang --target=x86_64-unknown-linux-gnu -c hello_linux.ll -o hello_linux.o
  $ llvm-nm --undefined-only hello_linux.o
                   U exit
                   U free
                   U malloc
                   U printf
  $ llvm-nm --defined-only --extern-only hello_linux.o
  0000000000000bd0 T main
  0000000000000040 T nexum_memcpy
  00000000000000a0 T nexum_strcmp
  0000000000000000 T nexum_strlen
  ```

  Six symbols are declared; only four survive into the object, because the
  POSIX `panic` path writes through the syscall layer rather than through
  `fputs`/`fputc` (`crates/nexum-runtime/src/lib.rs:121-125`) and the
  unused declarations are dropped.
* The link step is `clang`, invoked with no `-nostdlib`, no `-static` and no
  `-nodefaultlibs` (`crates/nexum-toolchain/src/lib.rs:178-208`), so the
  program links the platform C runtime dynamically today. D016 (the native
  linker, phase 11g) is what removes that dependency at link time; 11a.2 is
  what removes the six symbols above.
* Everything else is already ours: the string routines
  (`crates/nexum-runtime/src/strings.rs`), the file routines and the system
  call layer (`crates/nexum-runtime/src/syscalls.rs`).

### The program cannot meaningfully be called from C

* The entry point is the language's own `main`, which returns `i32`
  (`crates/nexum-codegen/src/lib.rs:237-242`, `:1202-1228`); the IR shows
  `define i32 @main()`. Every runtime helper except the three string routines
  is emitted `internal`, so it is not reachable from outside the module.
* The three string routines are deliberately *not* internal
  (`crates/nexum-runtime/src/strings.rs:9-10`), so `nexum_strlen`,
  `nexum_memcpy` and `nexum_strcmp` are global symbols in the object file —
  visible in the `llvm-nm` output above. Nothing documents them and their
  signatures are not an ABI; this is a symbol leak, not an interface.
* A user function is emitted with external linkage and the platform's default
  calling convention, e.g. `define i64 @helper(%Point %arg_p)` with
  `%Point = type { i64, i64 }`. So a C file *could* call a Nexum function —
  but everything it would rely on is unspecified: the record layout is only
  "LLVM struct, field order = declaration order"
  (`docs/runtime-abi.md:86`), a `Str` is a pointer to a NUL-terminated buffer
  that the collector expects to own (`docs/runtime-abi.md:84-97`), a `mut`
  parameter is a pointer into the caller's storage
  (`docs/language-reference/grammar.md:281-283`), and the runtime's globals
  (`@nexum_obj_head`, `@nexum_roots_head`) would have to be initialised by
  hand. There is no `extern`-style annotation, no name-mangling scheme and
  no way to name a foreign function, so this is an accident of the codegen,
  not a supported entry point. Recommendation 8 makes it structural.

### The system call layer is the template for a foreign interface without C

* The wrappers are inline assembly in the generated IR: the number is an
  immediate, the arguments are pinned to the registers the instruction reads
  them from, and the result comes back in the register the convention uses
  (`crates/nexum-runtime/src/syscalls.rs:1-21`, `:168-231`).
* They are `internal` and take `i64` arguments, because they are the
  runtime's own, not the program's.
* The syscall table today is `read`, `write`, `open`, `close`, `mmap`,
  `munmap`, `exit`, `lseek` (`crates/nexum-runtime/src/syscalls.rs:61-68`).
  **There is no socket, no pipe, no `ioctl`, no `poll`, no descriptor
  passing.** Protocol-level interop has no transport yet.

### The collector, and what a foreign call does to it

* Non-moving mark-sweep, single-threaded. A collection runs only inside an
  allocation or at the `collect()` builtin
  (`docs/runtime-abi.md:118-121`, `:167-169`).
* Roots are shadow-stack frames: `{prev, entries, count}`, pushed in the
  prologue, popped before every return, chained through `@nexum_roots_head`
  (`docs/runtime-abi.md:141-166`;
  `crates/nexum-runtime/src/lib.rs:135-140`, `:174`). Because the chain is a
  linked list of *all* live frames rather than a single "current frame"
  register, a nested entry into managed code — which is what a callback is —
  sees every outer frame's roots.
* `Str` and `List` are the only managed types (D010,
  `docs/language-reference/decisions.md:128-150`); strings are leaves and
  are not scanned.
* The mark phase walks the object list and compares each candidate word
  against the payload address of a live object
  (`nexum_mark_ptr`, `crates/nexum-runtime/src/lib.rs:162`; `nexum_scan_range`
  at `:168`). A pointer that is not a live payload is ignored, not followed.

## The design

### Why this matters: what a real user needs from the outside world

| Need | Shape it has | Fits which route |
| ---- | ------------ | ---------------- |
| A database | Postgres and MySQL speak a documented binary wire protocol; SQLite is an in-process C library | Protocol (Postgres/MySQL); for SQLite, either a re-implementation or a helper process |
| A display | X11 and Wayland are line protocols over a socket | Protocol, and it is a good fit |
| A system service | D-Bus, journald, systemd's socket protocol: bytes over a socket | Protocol |
| Compression, images, crypto, FFT | In-process C libraries (zstd, libpng, OpenSSL, FFTW) | Re-implementation in Nexum, or a helper process, or the device link |
| A GPU | A device with a driver; on Linux, DRM ioctls and a shared-memory ring; on Apple silicon, only the Metal C API | Machine (Linux) or device link (Apple silicon) |

The row that hurts is the fourth one. The programme's stated position is that
the matrix kernels and the math are ours to write
(`docs/native-roadmap.md:113-114`, D015), and that is defensible for a
standard library. It is a much heavier position for a *user* who wants
zstd. That is the real cost of the "no FFI" rule, and it is why
recommendations 2, 3 and 4 are about giving the user a *route*, not about
pretending the need is not there.

### Option (a): no interop at all — the language reaches the machine itself

*What it is:* everything the language needs, it implements. The system call
layer is the template: `nexum_sys_read` and friends are written as inline
assembly in the generated IR and are part of the runtime
(`crates/nexum-runtime/src/syscalls.rs`); the allocator, the string and file
routines and the math builtins follow in 11a.2 and 11c.

*ABI:* there is none, because there is no foreign side. The runtime ABI is
internal to the program and versioned as such
(`docs/runtime-abi.md:235-239`).

*Who owns the calling convention:* the compiler. Every call is intra-module,
so the convention is an implementation detail and can change.

*Callbacks:* not applicable.

*Collector:* unaffected — nothing outside the program can call back or hold a
reference.

*Cost:* every external capability is re-implemented, in Nexum, by the project.
That is a large recurring cost, paid by the standard-library authors first and
by users afterwards.

*Verdict:* this is the policy for the **language**, because it is the only
option that keeps the promise the programme is selling. It is not sufficient
as the whole answer for third-party libraries.

### Option (b): source-level interop — compile a `.c` file and link it

*What it is:* the build finds a `.c` file, runs the system compiler on it, and
links the object into the program. The Nexum source declares the foreign
function by name and describes its types by hand.

*ABI:* the C ABI, by definition.

*Who owns the calling convention:* **the C compiler, not Nexum.** This is the
decisive structural consequence. The moment a `.c` file is in the build, the
platform's C ABI is part of the language's contract, which forces two
things the language does not currently promise: (i) Nexum's record layout must
match C's struct layout including padding — today it is only "LLVM struct,
field order = declaration order" (`docs/runtime-abi.md:86`) — and (ii) the C
library becomes a dependency again, because a `.c` file that calls `printf`
needs libc, and libc's `printf` needs the allocator the programme is in the
middle of replacing.

*Callbacks:* a C function may take a function pointer. Nexum has no
function-pointer type and no closures, so the most it could pass is the
address of a named top-level function — which needs a new value form (11d) —
and the C side would be free to call it on any thread, which the
single-threaded collector cannot survive.

*Collector:* all four rules in recommendation 6 apply, and the fourth (no
cross-thread callbacks) is the one C libraries break most often.

*Cost:* the freestanding goal is abandoned for every program that uses it:
the program links the C library and the system compiler becomes a build
dependency, contradicting D012 and D016.

*Verdict:* **rejected.** A user who needs a C library should build a separate
binary and talk to it over a protocol — the same answer as option (d), with
the C library kept on the far side of the boundary where it does not touch the
language.

### Option (c): a binary FFI

*What it is:* `extern "C" fn ...` (or a Nexum-flavoured spelling), an
ABI description, and a way to name a foreign symbol and pass addresses
across the boundary.

*ABI:* a published calling convention for the boundary. There are two
choices and they are not equivalent:

* *the platform C ABI* — the platform's registers, its struct layout, its
  varargs. This is what makes existing libraries reachable at all. The cost
  is that C now owns the convention.
* *a Nexum-defined ABI* — our own layout and our own convention. The cost is
  that no existing C library is reachable without a shim per library. This is
  the shape of the device link in recommendation 3.

*Who owns the calling convention:* whichever of the two is chosen. The
language-level FFI is the first choice; the device link is the second.

*Callbacks:* a function-pointer type is required. Since the language has no
closures, the honest form is "the address of a named top-level function",
which the compiler can already lower (the symbol name is known) once there
is a type to hold it. The harder half is not the type but the discipline: the
callback may only run synchronously, on the calling thread, while the calling
frame is registered (recommendation 6).

*Collector:* the four rules apply. The two that actually bite:
the **root frame stays live across the call** — already true today, because
the roots are a frame chain and the collector walks all of it — and **the
foreign pointer is never followed**, which is already true today, because the
mark phase only follows words that match a live payload address. The rules
have to be stated anyway, because the moment the boundary type is
"unmanaged bytes in a slot", the discipline has to be enforced by the
compiler rather than by luck.

*Freestanding effect:* any FFI whose target is a C library **re-links the C
library**. The cost is not the symbols; it is that the program's own
behaviour — allocation, strings, error reporting — can then be routed through
C, and "the language's semantics are ours" stops being checkable.

*Verdict:* the C-ABI variant is **not adopted as a language feature**. Its
Nexum-ABI variant is adopted, bounded, as the device link (recommendation 3).
The difference between the two is ownership: in the device link, Nexum owns
the convention, the surface and the versioning, so the C ABI never becomes a
language feature and the exception cannot grow into a general FFI.

### Option (d): protocol-level interop

*What it is:* the program speaks a binary protocol to an external process or
device. The encoding is either a published one (the Postgres wire protocol,
X11, Wayland, D-Bus, the DRM ioctls) or one we design (a service we ship).
The transport is a byte channel the program owns: a pipe, a Unix socket, a
shared-memory ring, a file descriptor passed over `SCM_RIGHTS`.

*ABI:* a **message** ABI, not a calling convention. Fields have explicit
widths and endianness; there is no register convention, no struct layout to
agree on, no varargs. The "calling convention" is the request/response
ordering and the error convention of the protocol.

*Who owns it:* the protocol's owner — the protocol's author for a published
protocol, us for one we design. This is the key difference from the C ABI:
the interoperability contract is a *document*, not an ABI that a compiler has
to agree with.

*Callbacks:* **there are none, by construction** — and that is the strongest
argument for this route in a language without closures. The foreign side
never calls into the program; it answers. Where an event must be reported (a
job finished, a socket readable), the program polls or blocks on the channel,
and the readiness notification is *data*, not a code pointer. No
function-pointer type is needed anywhere in the language.

*Cost:* data movement. Every byte crossing the boundary is copied unless
shared memory is used, and the protocol has to be designed, documented and
versioned. For a database or a display server that is the normal shape; for a
GPU it is the wrong shape (below).

*Prerequisite the roadmap does not have:* the runtime has no socket, pipe,
`ioctl` or descriptor-passing syscall
(`crates/nexum-runtime/src/syscalls.rs:61-68`), and no way to wait on
several channels. A protocol route needs at least `socket`/`connect`/`bind`,
`send`/`recv`, `sendmsg`/`recvmsg` with `SCM_RIGHTS` for descriptor passing,
and `poll`/`epoll`. **This gap has to be closed or option (d) stays
theoretical** — see "Open questions".

### The collector, spelled out

For any boundary — a device link, a protocol channel, a helper process:

* **The calling frame stays live across the foreign call.** Already true:
  `nexum_roots_push` runs in the prologue and `nexum_roots_pop` before every
  return (`docs/runtime-abi.md:159-162`), and the collector walks the frame
  chain from `@nexum_roots_head`
  (`crates/nexum-runtime/src/lib.rs:174`). A foreign call that re-enters
  managed code on the same thread therefore sees the caller's roots, and a
  collection triggered inside that re-entry cannot free anything the caller
  still holds.
* **A foreign call may allocate.** Only by re-entering managed code (the
  collector's own trigger points are allocation and `collect()`), and then the
  ordinary rules apply. The foreign side's allocations are invisible to the
  collector — they are not in the object list, so the collector will never
  free them, and it will never follow them either.
* **A foreign call may call back.** Permitted only synchronously, on the
  calling thread, and only while the calling frame is registered. An
  asynchronous callback — the foreign side keeps the pointer and calls later —
  is forbidden: the objects it would touch may already have been collected,
  and the callback would arrive after the frame was popped, which is a
  use-after-free by construction.
* **A foreign call may hold references.** Only unmanaged ones. A managed
  reference (`Str`, `List`) must not be handed to foreign storage: the
  collector cannot see that the foreign side still holds it, and the language
  cannot yet express "keep this alive until the foreign side is done"
  (arenas are staged, D009). The boundary passes bytes and unmanaged
  addresses; nothing else.
* **A foreign pointer is never followed by the collector.** The mark phase
  only follows candidate words that match the payload address of a live
  object (`nexum_mark_ptr`,
  `crates/nexum-runtime/src/lib.rs:162`), so a foreign pointer is ignored.
  The rule is nonetheless normative: the boundary type is unmanaged, is
  never registered as a root, and its contents are never scanned. A pointer
  to foreign memory must not be stored in a slot the compiler registered as a
  reference; with a managed heap that is `malloc`-backed today
  (`crates/nexum-runtime/src/lib.rs:147`) the addresses are disjoint, so a
  false match cannot happen — but that is a property of the allocator, not a
  guarantee the language should rely on.
* **Ownership across the boundary is single-sided.** A buffer is allocated on
  one side and freed on that side. A foreign library's `free` must never see
  a managed pointer, or the collector's free list is corrupted.

### The freestanding goal

* The measurable form of "freestanding": a program has no undefined symbol
  other than the ones the Nexum runtime defines; its allocator, strings, file
  routines, math and I/O are ours.
* Today the program is not there yet: it declares six C symbols on Linux
  (verified above) and links the platform C runtime through `clang`
  (`crates/nexum-toolchain/src/lib.rs:178-208`). 11a.2 deletes the six; 11g
  deletes the link-time dependency on the C runtime.
* Any C-ABI FFI puts the C library back on every program that uses it. That
  is the whole of 11a undone for those programs, and it is why
  recommendation 1 is a policy rather than a default.
* The device link does not put the C ABI into the language, but it does put a
  native component on the link line, and that component will normally use
  libc. So a device-linked program has libc mapped in the process, and the
  honest form of the rule is: **the language's semantics never depend on C; a
  program that opts into a device link is not a freestanding program, and the
  toolchain should be able to say so** — for example by refusing to build it
  under the freestanding profile.

### The GPU path, which is the forcing case

`docs/native-roadmap.md:107-109` already states the shape: "No GPU driver in
the compiler. The GPU path is a library the user links, talking the driver's
binary protocol. That is how 'no C FFI' and 'use the GPU' coexist."

That sentence is right in direction and glosses over one thing worth being
precise about: **the driver speaks a protocol, but the loader speaks C.**
On Linux the loader is `libvulkan.so`, whose interface is the C API; the
*driver* underneath it is reached through DRM — `open("/dev/dri/renderD128")`,
`ioctl` for context and memory management, and a shared-memory ring for
command submission and completion. Every one of those is a syscall or an
`mmap`ed region, i.e. exactly what the existing system call layer reaches
(`crates/nexum-runtime/src/syscalls.rs`), plus `ioctl`, which the syscall
table does not have yet (`:61-68`).

On Apple silicon there is no equivalent: Metal is reachable only through the
Objective-C/C framework API (`MTLDevice`, `MTLCommandQueue`, …). There is no
public wire protocol for Metal. **This is the one place where the "no binary
FFI" position has no protocol-level answer**, and it is why recommendation 3
exists.

Three ways to pay, with their honest costs:

1. **A DRM client written in Nexum** (Linux). The runtime implements the DRM
   ioctls and the command ring. Fully freestanding; no C ABI anywhere; the
   cost is a large piece of work (context creation, DMA-BUF import and
   export, sync-file based synchronisation) and it is Linux-only.
2. **A separate GPU service process** that owns the C API, with the model
   program talking a byte protocol over a Unix socket or a shared-memory
   ring. The program stays freestanding and the service is an ordinary
   program that may use C. The cost is an extra process, an extra copy per
   transfer unless the ring is shared memory, and a protocol to design — for
   training workloads, at tensor sizes, the copy is prohibitive.
3. **A device link**: the program links a native component exposing a
   *Nexum-defined* ABI (device, queue, buffer, submit, wait) that calls the
   loader's C API internally. The cost is one component per platform, and the
   program stops being freestanding; the benefit is that the language stays
   free of the C ABI and the surface stays tiny enough to be enumerable.

The recommendation for 11h is **shape 3 now, with shape 1 as the direction of
travel on Linux**, and an explicit decision on Apple silicon: either a
Metal-backed device-link component, or no GPU support on that platform. What
must *not* happen is the obvious shortcut — giving the language an `extern`
so the program can call `MTLCreateSystemDefaultDevice()`. That would trade
the programme's central promise for one platform.

## Risks

* **The exception stretches.** The device link starts as a handful of GPU
  entry points and the pull to add "just one more" (a socket wrapper, a
  zstd binding, a font rasteriser) is strong. If the device ABI grows an
  entry point per library, it has become an FFI with a different name.
  *What would have to be true for the recommendation to be wrong:* that the
  user base needs in-process C libraries badly enough that protocol
  interop's copying cost dominates. Mitigation: the device ABI is closed and
  versioned, adding an entry point is a breaking change, and it is never
  required by the standard library.
* **Protocol interop is too slow where it matters.** Shared-memory rings
  make it acceptable for a database and a display; they do not make it
  acceptable for GPU tensor traffic. If the device link is not built, the GPU
  path is either Linux-only or not done. This is the strongest argument for
  building the device link rather than relying on the protocol route.
* **The transport may never arrive.** Option (d) is the policy answer for
  almost everything, and it is currently unimplementable: no socket, no
  pipe, no `ioctl`, no descriptor passing, no way to wait on several
  channels. If the roadmap does not add them, the rule is unimplementable and
  the programme will drift back towards an FFI by accident.
* **"No C ABI" is read as "no C libraries".** The rule is about the
  *language*. A user may still link a third-party binary; the packaging rule
  is the toolchain's business (11g), not the language's. If this is not
  stated, the standard-library authors will read the rule as "you may not
  use anything that is not written in Nexum", which is a different and much
  more expensive rule.
* **The accidental ABI is mistaken for a supported one.** The emitted
  functions have external linkage and the platform's default calling
  convention, so a C file can call a Nexum function today. Anyone who does
  that is relying on the record layout, the `Str` representation and the
  runtime's globals, none of which are published. Recommendation 8 removes
  the accident.
* **A callback crosses the boundary asynchronously anyway.** The most likely
  way this design breaks is not a language feature but a library that hands
  back a completion pointer. The collector rules are the defence, and they
  only work if the boundary type is enforced by the compiler.

## Open questions

For the orchestrator, with a recommendation for each.

1. **Does "no binary FFI" bind the language or the binary?** The rule says
   the *language* never names a foreign symbol. A user may still link a
   third-party binary, provided the language cannot call it by name.
   *Recommendation:* the language rule only. The packaging and link policy is
   11g's business, and the toolchain should be able to mark a binary that
   carries a foreign dependency.

2. **Where does the transport go?** Protocol interop needs sockets, pipes,
   descriptor passing and a way to wait on several channels.
   *Recommendation:* add it to the roadmap explicitly — a new **11a.3
   "channels"**: the socket/pipe/`ioctl`/`poll` system calls plus a small
   runtime layer for connecting, framing and waiting. Without it,
   recommendation 2 is a rule with no implementation and the programme will
   drift. If the phase list cannot grow, put it at the front of 11d.

3. **Where does the device ABI live?** *Recommendation:* a separate document,
   `docs/device-abi.md`, versioned independently of the runtime ABI, written
   when 11h starts and not before. It should enumerate the entry points
   exhaustively — a closed list, not a pattern.

4. **May a managed reference cross a device-link boundary?** *Recommendation:
   no.* The boundary passes unmanaged bytes and unmanaged addresses only. The
   collector rules forbid a managed reference escaping into foreign storage,
   and arenas (D009) are the eventual answer to "keep it alive until the
   device is done".

5. **Apple silicon: device link, or no GPU?** The no-FFI rule has no
   protocol-level answer for Metal.
   *Recommendation:* a Metal-backed device-link component, and say plainly in
   the 11h plan that this is a bounded exception to the rule rather than a
   route the language supports.

6. **Should the language's functions stop being externally visible?**
   *Recommendation:* yes, with 11g — emit them `internal` or under a reserved
   prefix. It costs almost nothing and makes "not callable from C" a property
   of the binary rather than of a document.

7. **What exactly is the experimental gate for the device link?** The
   manifest is currently a line scan for `entry`
   (`crates/nexum-driver/src/main.rs:1007-1028`) and the CLI has only global
   `--target` and `--opt-level` (`:15-25`).
   *Recommendation:* a build-time flag — `nx build --device <name>` and a
   `[device]` manifest field — that is off by default, that the standard
   library never sets, and that the freestanding profile rejects outright.

8. **Does the "experimental, off by default" framing apply to the device link
   only, or is there a language-level FFI experiment too?** The task framing
   assumed an experimental language FFI might be wanted.
   *Recommendation:* **no language-level FFI experiment.** An experimental
   `extern` would immediately become the thing every user asks for, and it
   would make the C ABI the interoperability currency of the language before
   the programme has decided whether that is acceptable. The device link is
   the experiment: it is opt-in, off by default, never required by the
   standard library, and small enough to be withdrawn. If an FFI experiment
   is wanted later, the device link is where it would live.

## Changes needed elsewhere

Recorded here rather than acted on, since this task writes one file.

* `docs/language-reference/decisions.md` — add **D017** (provisional) with
  the house rule text quoted above.
* `docs/language-reference/grammar.md` §7 — add a line to
  "accepted-but-unspecified": no `extern`, no foreign function type, no C
  header import, and the interop routes that are allowed instead.
* `docs/native-roadmap.md` — the "Honest limits" section should say plainly
  that protocol-level interop needs a transport (11a.3) that does not exist
  yet, and that the GPU path has a bounded exception on Apple silicon.
* `docs/runtime-abi.md` §0 — the freestanding table should gain a row for
  the device link: "an explicitly linked device component may bring its own
  dependencies; a device-linked program is not a freestanding program".
* The roadmap phase list needs the transport item from Open question 2.
