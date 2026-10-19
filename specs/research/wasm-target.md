# WebAssembly as a Nexum target

**Question.** Is WebAssembly a target worth having for Nexum, given that the
programme is about owning the machine - and if so, what would it take?

**Summary.** The recommendation is *yes, later*: a wasm target is worth
having, but it is a **destination for the language, not a machine to own**, and
it should not be built now. It fits the programme in exactly one way - it is
the only target where no C library exists at all, so "no C FFI" stops being a
discipline and becomes a fact - and it forces three pieces of work the
programme needs anyway (a fixed-arena allocator, a pointer-width-independent
runtime ABI, and a data-layout-driven layout model). Those three belong to
phase 11a.2 and 11d. The target itself belongs in a new phase after 11f. The
good news is that the toolchain does not need a new code generator: the
existing textual-LLVM-IR pipeline compiles for wasm32 unchanged, and I built
and ran Nexum programs that way (16 of the 21 `run` fixtures produce their
expected output under Node after one class of 32-bit ABI offsets is fixed).
The bad news is that WebAssembly cannot do anything the programme exists for:
no devices, no processes, no files, no signals, a 32-bit address space, and a
fixed 64 KiB shadow stack.

## Recommendation

1. **Do not add a wasm target to `nexum-target` in this phase.** No triple, no
   runtime slice, no driver path, no tests. The reason is not that wasm is
   hard - it is that the wasm target is a *destination* and every phase from
   11a to 11j except 11g is closer to the thing the programme exists for
   (owning the machine, the model framework, the self-hosted compiler).
   Building the target early would also freeze the runtime ABI in a
   64-bit-only shape, which is exactly the shape a wasm target cannot use.

2. **Do the three prerequisites inside the phases that need them anyway.**
   These are the only things the wasm target forces, and all three are worth
   having on POSIX:

   a. **The allocator must support a fixed arena** (phase 11a.2). On wasm the
      managed heap *is* a fixed region of the linear memory: it cannot ask
      the OS for more, so it must be a bump region with a release point. That
      is the same mechanism as the arenas of phase 11d (D009), so it should
      be built once, as a first-class arena, not twice.

   b. **The runtime ABI must be laid out by type, not by hand-written byte
      offsets** (phase 11a, and it is a bug fix, not a wasm feature). The
      root frame, the root entry and the object header are addressed with
      `getelementptr i8, ptr %f, i64 8` and `i64 16`
      (`crates/nexum-runtime/src/lib.rs:174`), which assumes an 8-byte
      pointer. On a 32-bit target the root frame `{ptr, ptr, i64}` is 16
      bytes, so the entry count at `+16` lands *outside* the allocation. The
      failure is silent: I measured five of the twenty-one `run` fixtures
      trapping inside the collector on wasm32, and with typed GEPs in place
      sixteen of twenty-one pass.

   c. **The code generator must stop hand-computing record sizes with
      8-byte pointers** (phase 11d). `llvm_size` and `llvm_align`
      (`crates/nexum-codegen/src/lib.rs:36-68`) return 8 for a `Str`; on
      wasm32 the LLVM type `{ptr, ptr}` is 8 bytes, not 16, so the element
      stride the collector is told about is wrong. Today `list_push` is told
      the element is 16 bytes for a `{Str, Str}` record while the type LLVM
      built is 8 bytes wide. The generator should ask the data layout, not
      assume.

3. **Build the target after 11d at the earliest, and after 11f as the
   recommended point.** A new phase 11k, *not* a part of 11g: 11g is the
   native linker, and it does not transfer - wasm needs `wasm-ld`, so the
   wasm build keeps an external linker and the "no C library" claim stays a
   claim about the *program*, not about the *toolchain*. 11d is the earliest
   point because it delivers the pointer model and the arenas; 11f is the
   recommended point because 11e (threads) and 11f (SIMD) have direct wasm
   proposals - the threads proposal and `simd128` - and building the target
   before them would mean building them twice.

4. **When it is built, the shape is this.**

   * **Triples.** `wasm32-unknown-unknown` first: freestanding, no imports, no
     C library, no OS - it is the triple that matches the programme's own
     claim. Then `wasm32-wasip1`, which supplies the host calls for files,
     stdout and exit. Not `wasm32-unknown-emscripten`: that is the C library
     by another name.
   * **`TargetSpec`.** `pointer_width: 32`. wasm is neither one of the
     known operating systems nor one of the known architectures, so it needs
     a *new axis*, not a new `os` value: `runtime_ir` branches on
     `os.contains("windows")` and then on `os.contains("macos")`
     (`crates/nexum-runtime/src/lib.rs:46-100`), and an unknown OS would
     silently fall into the POSIX branch and emit x86-64 system calls.
   * **Codegen.** No change to the generator itself. The triple and the data
     layout go into `TargetSpec` (the wasm32 layout is
     `e-m:e-p:32:32-p10:8:8-p20:8:8-i64:64-i128:128-n32:64-S128-ni:1:10:20`),
     and the layout model becomes data-layout-driven (item 2c).
   * **Runtime.** A new slice: a bump/arena allocator over the linear memory,
     the same mark-sweep collector over it, a host write for the output, a
     host call or a trap for panic and exit, and the ABI's structures
     addressed by type.
   * **Driver.** A wasm build path: `-nostdlib`, a `.wasm` output, and no
     "run" - a wasm module is a library the host calls, not a process. The
     cross-target verification test (`crates/nexum-driver/tests/backend.rs:60-74`)
     picks up the new triple for free, because it already passes
     `--target=<triple>` to clang.
   * **Tests.** The existing `run` fixtures, built for wasm and run under a
     host (Node, wasmtime, a browser), plus a `wasm` fixture directory and a
     test that the collector still reclaims inside a fixed arena.

5. **What it buys, stated plainly.** A place to run the language where no C
   library exists at all - the only place where "no C FFI" is a fact rather
   than a rule - and a portable deployment target (browser, edge runtime,
   plugin host). It is also a good *test* of the freestanding work: if the
   runtime slice for wasm has to be written without a libc, then 11a is
   validated in a second, independent configuration.

6. **What it does not buy, stated just as plainly.** Any of the capability
   the programme exists for. There is no system call, no process, no file, no
   device and no signal; the address space is 32 bits; the shadow stack is a
   fixed region; threads need a proposal *and* a host that supports shared
   memories. It also does not remove a boundary - it *changes* the boundary.
   The C FFI the programme is removing is replaced by a host ABI (imports and
   exports), which is a cleaner boundary, but it is a new surface that has to
   be designed rather than inherited.

7. **Priority, if the orchestrator wants an order.** The wasm target sits
   *below* 11e (concurrency), 11f (SIMD), 11h (the GPU path) and 11i (the ML
   framework), and it is not on the critical path for 11j (self-hosting). If
   the budget for a second destination exists, the browser/edge/plugin-host
   story is a better use of it than anything else in the queue - but it is
   the *last* thing in the queue, not the next.

## What the code looks like today

### The target list

`crates/nexum-target/src/lib.rs` holds a flat `TargetSpec`
(`triple`, `pointer_width`, `os`, `arch`) and a table of five targets
(`crates/nexum-target/src/lib.rs:75-83`):

| name | triple | `os` | `arch` |
| ---- | ------ | ---- | ------ |
| `windows-x86_64` | `x86_64-pc-windows-msvc` | `windows` | `x86_64` |
| `linux-x86_64` | `x86_64-unknown-linux-gnu` | `linux` | `x86_64` |
| `linux-aarch64` | `aarch64-unknown-linux-gnu` | `linux` | `aarch64` |
| `macos-x86_64` | `x86_64-apple-darwin` | `macos` | `x86_64` |
| `macos-aarch64` | `aarch64-apple-darwin` | `macos` | `aarch64` |

Three things about it matter for wasm:

* `pointer_width` is `64` in every single arm of the code
  (`crates/nexum-target/src/lib.rs:19-67`) and is not read anywhere else in
  the tree. It is the natural place to put `32`, but putting it there
  changes nothing by itself: the sizes that matter are computed elsewhere.
* `data_layout_str()` (`crates/nexum-target/src/lib.rs:85-100`) has a
  catch-all fallback for unknown `(arch, os)` pairs, so a wasm target with
  the wrong entry would silently get the x86-64 layout - 64-bit pointers on a
  32-bit machine.
* `os` and `arch` are the only two axes the runtime is selected on.

### The code generator

`nexum_codegen::emit` (`crates/nexum-codegen/src/lib.rs:109-175`) writes
textual LLVM IR: the module header is the triple and the data layout, then
the runtime slice, then the record types, the string globals and the
functions. Everything the generator emits is target-agnostic: opaque
pointers, `i64` integers, `double` floats, LLVM structs and fixed arrays,
`alloca`, `getelementptr`, `insertvalue`/`extractvalue`. There is no x86
anywhere in it.

The target is used in exactly two places: the module header, and
`nexum_runtime::runtime_ir(&target.os, &target.arch)`
(`crates/nexum-codegen/src/lib.rs:119`).

The *layout* is not target-agnostic, though. `llvm_align`, `llvm_size` and
`scan_bytes` (`crates/nexum-codegen/src/lib.rs:36-99`) hand-compute the size
of a record from its fields, with `Str` and `List` counted as 8 bytes
(line 57) and 8 bytes of scan width (line 90). The computed size is what
the collector is told to scan and what `list_push` is told the element
stride is (`crates/nexum-codegen/src/lib.rs:779-780`). These numbers assume
8-byte pointers.

### The runtime slice

`nexum_runtime::runtime_ir(os, arch)`
(`crates/nexum-runtime/src/lib.rs:46-100`) picks one of two things:

* `os.contains("windows")` -> the C library: `printf`, `fputs`, `fputc`,
  `exit`, `malloc`, `free`, `fopen`, `fclose`, `fseek`, `ftell`, `fread`,
  `fwrite`, `__acrt_iob_func`.
* otherwise -> the POSIX system call layer: the inline-assembly wrappers from
  `crates/nexum-runtime/src/syscalls.rs`, plus the file routines built on
  them. The system is chosen by `os.contains("macos")`, the instruction set
  by `arch.contains("aarch64")`.

There is **no third case**. An OS the runtime does not know is a POSIX
system, which is the wrong default for a machine with no system calls at
all.

Everything else in the runtime - the collector, the object layout, the root
frames, the string and list helpers - is written once and shared by all
targets. That is the good news: the collector is not a per-target thing.
The bad news is how it addresses its own structures. The object block is
laid out with hand-written byte offsets (`+0` size, `+8` mark, `+16` next,
`+24` scan bytes, `+32` payload; `crates/nexum-runtime/src/lib.rs:147`), and
so is the root frame (`+0` previous, `+8` entries, `+16` count) and the root
entry (`+0` address, `+8` scan bytes) - see
`docs/runtime-abi.md:103-172` and the code at
`crates/nexum-runtime/src/lib.rs:174`. Those offsets are correct on a 64-bit
target and wrong on a 32-bit one, in a way that no compiler will report.

### The driver and the toolchain

`build_file` (`crates/nexum-driver/src/main.rs:1128-1151`) writes the IR to
a temporary `.ll`, verifies it with `verify_ir_for(.., Some(&target.triple))`
and then hands it to clang to produce an executable. `run_file`
(`crates/nexum-driver/src/main.rs:1153-1158`) executes the result.

`verify_ir_for` (`crates/nexum-toolchain/src/lib.rs:221-248`) passes
`--target=<triple>` to clang, so the cross-target check works for a new
triple with no change. `run_clang_with`
(`crates/nexum-toolchain/src/lib.rs:178-208`) does *not* pass a target: it
relies on the `target triple` line inside the module, which the generator
writes. So the build follows the module.

### What I measured

Everything below was measured on this machine with LLVM/clang 23.1.3,
Node 24.13.1, and the compiler in this repository
(`cargo build -p nexum-driver`, then `nx emit-ir` on the examples and the
`run` fixtures). The experiments live outside the repository, in
`C:\Users\Dell\AppData\Local\Temp\opencode\wasm-lab`; **no repository file
was changed**.

**The existing pipeline compiles for wasm32.** The emitted IR for
`examples/strings.nx` - the whole runtime, the collector, the root frames,
the records, the strings - compiled with
`clang --target=wasm32-unknown-unknown` and linked into a `.wasm` module
without a single change to the generator. The textual-IR approach is not the
problem here: LLVM IR is target-independent, and the module carries its own
triple and data layout, so the existing pipeline *can* emit IR for wasm32
and let LLVM's WebAssembly backend lower it. **No new code generator and no
new backend are needed.**

**The POSIX system call layer cannot.** The same module, built for Linux
(the POSIX runtime slice, with the inline-assembly wrappers), fails to
compile for wasm32 with eight copies of:

```
error: could not allocate output register for constraint '{rax}'
```

The wrappers pin their arguments to x86 registers
(`crates/nexum-runtime/src/syscalls.rs:110-149`), and the WebAssembly backend
has no such registers. This is not a defect - a wasm module has no system
calls to make - but it means the wasm runtime is a *different slice*, not a
different selection of the existing one. A wasm target needs its own
runtime, not a `SysOs::Wasm` entry.

**The data layout has to change.** clang's wasm32 data layout is
`e-m:e-p:32:32-p10:8:8-p20:8:8-i64:64-i128:128-n32:64-S128-ni:1:10:20`
(`p:32:32` - 32-bit pointers). The generator writes the layout from
`TargetSpec::data_layout_str()`, so a wasm entry is mandatory; without it the
module would claim 64-bit pointers on a 32-bit machine.

**128-bit arithmetic becomes a library call.** `mul i128` lowers to a call to
`__multi3` and `udiv i128` to `__udivti3`; both are undefined symbols in the
object file. A freestanding wasm runtime has to provide them. This is not
hypothetical: phase 11c adds fixed-width integers and 11f adds vectors, and
this LLVM installation ships **no wasm32 runtime libraries at all**
(`lib/clang/23/lib` contains only `windows` and `x86_64-pc-windows-msvc`),
so even the default `clang --target=wasm32-unknown-unknown x.ll -o x.wasm`
fails looking for `crt1.o`, `-lc` and `libclang_rt.builtins.a`. A wasm build
must be `-nostdlib` and the runtime must supply what it needs.

**The machine is one linear memory, and the compiler gets a map of it.** A
freestanding wasm module built from the Nexum IR exports
`memory`, `__stack_pointer`, `__stack_low`, `__stack_high`, `__data_end`,
`__heap_base`, `__global_base` and `__wasm_first_page_end`. The default
layout is `--stack-first`: the shadow stack occupies `[0, 65536)` and the
heap starts just above the static data. `alloca` is lowered to the shadow
stack - `global.get 0` (the `__stack_pointer` global) minus the frame size -
so the *stack* is a fixed 64 KiB region with no guard page and no growth.
There is no `mmap` and no `munmap`: the only way to get more memory is
`memory.grow`, which returns the old size in pages, or **-1 on failure**
(measured), and which can be capped at link time with `--max-memory` (by
default wasm-ld emits a memory with no maximum, so the 4 GiB wasm32 limit and
the host's own limit apply).

**There are no system calls.** The module cannot read a file, write to a
terminal or exit. `--allow-undefined` turns the undefined symbols into
imports from a host module; that host module is the whole operating system
interface, and it is a *new* interface, not the C one.

**Threads are a proposal, and LLVM will silently drop them.** With the
default feature set, `atomicrmw add` is lowered to a plain load/add/store -
no error, no warning, just a racy program. With `+atomics` it becomes
`i32.atomic.rmw.add`, and the module then needs `--shared-memory` at link
time and a host that supports shared memories.

**Floating point is IEEE 754 by default.** Measured on wasm32:
`1.0/0.0` gives `+inf` (`0x7ff0000000000000`) and `0.0 + 0.0` gives the
canonical NaN (`0x7ff8000000000000`). The "strict" subset - no infinities,
canonical NaN only - is a separate proposal and is not the default; the wasm
*deterministic profile* is what mandates it. So the restriction the question
asks about is real, but it is opt-in: a Nexum target on the default wasm
configuration keeps the same `Float` semantics as on x86-64. If the
programme ever wants bit-identical results across targets, the deterministic
profile is the thing to adopt, and it is a decision about the *language*,
not about the target.

**The 32-bit assumption breaks the collector, silently.** With the runtime
ABI exactly as it is today, five of the twenty-one `run` fixtures trap with
`memory access out of bounds` inside `nexum_scan_range`; the rest run. The
cause is the root frame: `{ptr, ptr, i64}` is 24 bytes on a 64-bit target
and **16 bytes** on wasm32, so the hardcoded `getelementptr i8, ptr %f, i64
16` for the entry count writes *past* the frame, and the count the collector
reads back is garbage. Changing those six GEPs to typed ones
(`getelementptr %nexum.root_frame, ptr %f, i64 0, i32 2`) fixes it:

| build | result |
| ----- | ------ |
| offsets as they are today | 11 of 21 fixtures pass, 5 trap inside the collector |
| offsets expressed as types | **16 of 21 fixtures pass with the expected output** |

The five that still differ are not wasm problems: three need an `exit(1)`
path (the panic path traps instead of exiting), one needs a filesystem
(WASI, or a host file API), and one differs only in float formatting
("Infinity" against the C library's "inf").

The same class of assumption is in the code generator: the emitted IR says
`call void @nexum_list_push(ptr %v2, ptr %vle7, i64 16, i64 16)` for a
`{Str, Str}` record - 16 bytes, because the generator counts a `Str` as 8
bytes - while the LLVM type it built, `%Token = type { ptr, ptr }`, is 8
bytes wide on wasm32. The element stride and the collector's scan width are
both wrong, and nothing says so.

## The design

### Is it the same programme?

No, and this is the most important thing in the document. "Owning the
machine" means: reaching the kernel, mapping memory, driving a device, using
the cores. A wasm module can do none of those things. It has no system
calls; its memory is a byte array the host lends it; it has one thread
unless the threads proposal is enabled and the host cooperates; it cannot
open a file or take a signal; and it is not even a process - it is a library
that a host instantiates and calls. On every axis the programme cares about,
wasm is the *opposite* of owning the machine: it is the machine reduced to
a register file, a call stack and a byte array, with every capability
held by someone else.

So wasm is a **separate destination**: a browser, an edge runtime, a plugin
host, a sandbox for untrusted code, a portable deployment of a Nexum
library. That is a legitimate destination for a language. It is not a step
toward the goal, and it should not be sold as one.

What it *is* good for, in the programme's own terms, is as a **test of the
freestanding claim**. D012 says the runtime does not link the C library. On
the POSIX targets that is a discipline the runtime maintains by hand (it has
its own `strlen`, `memcpy`, `strcmp`, its own file routines, its own system
call layer). On wasm there is no libc to discipline anyone: the runtime
either provides the functionality itself or the program does not run. A wasm
runtime slice is therefore an independent check that the language's core -
types, records, strings, lists, the collector - really is independent of the
platform. That is worth something. It is not worth a phase.

### The linear memory and the collector

The managed heap on wasm is a region of the linear memory. There are three
ways to arrange that, and only one of them is the right one:

* **Bump allocation with no reuse.** The simplest, and what my experiment
  did (the host's `malloc` import was a bump allocator over
  `__heap_base`). It works, and it is what the 11a.2 allocator would do with
  a free list added later - but a program that allocates in a loop exhausts
  the memory, because there is no `free` to call and no `munmap` to give
  pages back.
* **A free list inside the region.** The collector's sweep returns an
  object to a free list instead of to `free`
  (`crates/nexum-runtime/src/lib.rs:186` is the `call void @free(ptr %cur)`
  that has to become something else). This is the shape the 11a.2 allocator
  should have anyway, and it is the shape that makes the wasm target work
  with no special case.
* **Fixed arenas.** The heap is divided into arenas; each arena is a bump
  region with a single release point, and the collector only runs inside
  arenas. This is exactly the arena model of D009
  (`docs/language-reference/decisions.md:112-126`), and it is the model that
  maps most cleanly onto a linear memory that cannot be given back.

The answer to "a linear memory with no system calls means the runtime cannot
grow its heap the way it does now - what happens" is therefore: the heap
grows by growing the *linear memory* (`memory.grow`, in 64 KiB pages, which
can fail and returns -1), and the heap inside it grows by bumping. The
runtime must check the `memory.grow` result and must have a policy for
running out (the current one is `nexum_oom` -> panic, which is fine).

The collector itself does not change: it is a non-moving mark-sweep over a
linked list of objects, and the list, the mark bit, the scan widths and the
root frames are all portable. What changes is (a) where the objects come
from, (b) where they go when they are swept, and (c) the widths, because
the widths are computed from the pointer size.

### The absence of a pointer type

WebAssembly has addresses but no pointer type: the language has no
`pointer`; the module has `i32` values that happen to be addresses, and
LLVM's wasm backend has already moved on to the *next* pointer model (the
one with provenance, from the GC and stack-switching proposals). Nexum has
no pointer type either - that is phase 11d.

So the wasm target is a *forcing function* for 11d rather than an obstacle
to it. A pointer in Nexum has to have a width, and on wasm it is 32 bits.
The moment the language has a pointer type, the runtime ABI's fixed byte
offsets have to be expressed in terms of that width, and the code
generator's hand-computed record sizes have to come from the data layout.
Both of those are on the critical path for wasm and both are also just
correctness work for the existing targets (the 8-byte assumption is
currently invisible, but it is an assumption).

The practical consequence for the wasm target is that a `Str` is a 4-byte
pointer to a NUL-terminated buffer, records shrink, and the collector's
scan widths shrink with them. Nothing in the *language* changes.

### Threads

The base WebAssembly specification has no threads: no shared memory, no
atomics, no `memory.atomic.wait`. The threads proposal adds shared
memories, the atomic instructions and wait/notify, and it is in the
WebAssembly 2.0 specification. LLVM exposes it as the `+atomics` target
feature, and the linker needs `--shared-memory`.

The trap is that the default silently degrades. Measured: with the default
feature set, `atomicrmw add` becomes a plain load/add/store - the program
compiles, links and runs, and it is racy. A wasm target therefore has to
*pin* its feature set and refuse to compile concurrency constructs when the
threads proposal is off, rather than let LLVM decide. This is a good
argument for the 11e work (threads, atomics, a scheduler) being designed
against an explicit capability set rather than against POSIX.

### Floating point

On the default wasm configuration, `Float` behaves exactly as it does on
x86-64: `f64` is IEEE 754, infinities exist, NaN exists, and the canonical
NaN is what the backend produces. The "strict" subset - where an operation
that would produce an infinity produces the largest finite value instead,
and every NaN is the canonical one - is a separate proposal (and the
deterministic profile of the specification adopts it). Adopting it would be
a *language* decision, with the same consequences as deciding that Nexum's
`Float` never produces a NaN: the tests, the compiler's constant folding and
the runtime's math builtins all have to agree. It is not a wasm problem,
and wasm is not a reason to decide it.

### The toolchain

This is the part that turns out to be cheap, and it is the part worth being
concrete about.

* **The code generator does not change.** It already emits target-neutral
  textual LLVM IR with the target's triple and data layout in the module
  header. LLVM's WebAssembly backend lowers that IR. The only codegen work
  is the layout model (item 2c) and whatever the language's pointer type
  needs in 11d.
* **The target list gains two entries** in `crates/nexum-target/src/lib.rs`:
  `wasm32-unknown-unknown` and `wasm32-wasip1`, with `pointer_width: 32`,
  the wasm32 data layout, and a new axis so the runtime can tell that this
  target has no operating system.
* **The runtime gains a slice.** The selection in `runtime_ir` becomes a
  three-way choice: the C library (Windows, for now), the POSIX system call
  layer, and the wasm slice. The wasm slice needs: an arena allocator over
  the linear memory, the collector's structures addressed by type, a host
  write for `print`, a host call or a trap for `panic` and `exit`, and no
  file routines unless the target is `wasm32-wasip1`.
* **The driver gains a build path.** The current path ends in an executable
  and `run` executes it. A wasm build ends in a `.wasm` module, is linked
  with `-nostdlib` (and `--no-entry` for a library module, or `--entry=` for
  a WASI command), and cannot be "run" by the driver at all - it needs a
  host. The `nx build --target wasm32-unknown-unknown` command should
  therefore produce the module and print how to run it, and the cross-target
  verification test picks the triple up for free.
* **The verifier is unaffected.** `verify_ir_for` already passes
  `--target=<triple>`, so the module is checked by the wasm backend, not by
  the host assembler.

### The collector and the arenas: the same mechanism?

Yes, and that is the strongest argument for doing the arena work early.

The wasm managed heap has to be a *fixed region*: it cannot ask for memory
from an operating system, so it has to own a region of the linear memory
and manage it itself. D009's arena is exactly that: "a bump-allocated
region with a single release point" (`docs/language-reference/decisions.md:120-122`,
and `docs/runtime-abi.md:205-217`). The 11a.2 allocator and the 11d arenas are
the same mechanism seen from two ends: 11a.2 needs an allocator that can
serve a *region* it was given (for a wasm heap, and for device buffers and
parse trees on POSIX), and 11d needs the language surface that hands out
those regions.

So the work is not "an allocator for wasm" and "the arenas of 11d" - it is
one allocator with a fixed-region mode, and the wasm target is simply the
first user that *requires* it. On POSIX it is an optimisation and a
convenience; on wasm it is the only way to allocate at all.

### The honest comparison

| | what it buys | what it costs |
| --- | --- | --- |
| **wasm target** | a destination where no C library exists; a portable deployment target (browser, edge, plugin host); an independent check on the freestanding runtime; a forcing function for the pointer model, the arena allocator and the ABI | a second runtime slice; a pointer-width-independent ABI (a documented ABI change and a version bump); a data-layout-driven layout model in the generator; a third axis in the target model; a build path that produces a module, not a program, and needs a host; a new *host* ABI in place of the C one; 32-bit pointers everywhere; a fixed 64 KiB shadow stack; threads only with a proposal and a cooperating host |
| **the POSIX story (11e-11f, 11h-11i)** | the capability the programme exists for: the cores, the GPU, the model framework, the self-hosted compiler | the same ABI and allocator work, but only once, and on the machine the programme is about |

The comparison is not close. The wasm target is interesting; it is not
important. The three preparatory items (2a-2c) are worth doing on their own
merits, and they are what the wasm target would have paid for.

## Risks

* **The ABI change is the real risk, not the wasm work.** Making the
  runtime's structures pointer-width-independent touches
  `docs/runtime-abi.md` and the runtime IR, and it is a *semantic* change to
  a documented ABI (the bootstrap ABI is `v0` and is allowed to change
  without notice, per `docs/runtime-abi.md:235-240` - the time to do it is
  before `v1` is declared, not after). If the ABI is frozen first, a wasm
  target is not merely expensive, it is impossible without a second ABI.
* **A silent 32-bit corruption class.** The layout model in the generator
  and the offsets in the runtime are both 64-bit assumptions that produce
  wrong answers rather than errors. Every 32-bit target will find them, and
  they are the kind of bug that shows up as a corrupted heap three phases
  later.
* **The wasm target becomes a maintenance sink.** A wasm runtime is a
  second implementation of the collector's environment, and it will drift.
  The threads and SIMD proposals will add a third and fourth configuration
  (shared memory, `simd128`) to keep working.
* **The host ABI is a new public surface.** Whatever the wasm runtime
  imports and exports becomes part of Nexum's interface to the world, and it
  has to be designed, versioned and documented - the programme is removing a
  boundary, not multiplying them. This is the risk that makes the wasm
  target more than a compiler feature.
* **The recommendation is wrong if the programme's goal changes.** If Nexum
  is ever aimed at *deployment* rather than at *the machine* - a language
  that runs in a browser, in an edge function, in a plugin sandbox - the
  priority inverts, and the wasm target moves to the front. Nothing in the
  current roadmap says that, and `docs/native-roadmap.md:5-21` says the
  opposite.
* **The wasm target is wrong if the browser is not a destination.** A wasm
  module in a browser is a module with no filesystem, no clock it can trust,
  no threads (without the proposal) and a 32-bit address space; a Nexum
  program that needs a file cannot go there. If the only intended
  destination is "a plugin host for a desktop application", the cost is not
  repaid by the benefit.

## Open questions

1. **Does the programme want a deployment target at all, or only the
   machine?** *Recommendation:* only the machine, for now. `docs/native-roadmap.md`
   and D012-D016 are all about capability, not reach. Revisit this question
   if the ML framework (11i) needs to run in a browser or an edge function -
   that is the point at which the wasm target becomes a requirement rather
   than a possibility.

2. **Should the arena work be pulled forward into 11a.2?** *Recommendation:*
   yes, as the "fixed region" mode of the allocator, not as the language
   surface of D009. 11a.2 needs it (the wasm heap, and POSIX device
   buffers), 11d needs it (the language surface), and doing the mechanism
   once in 11a.2 stops the wasm target from having to invent its own.

3. **Is the runtime ABI change worth doing before `v1` is declared?** *Recommendation:*
   yes, and it should be done as part of 11a.2 rather than as a wasm
   project. The change is small - typed GEPs instead of byte offsets - and it
   removes a class of silent corruption that a 32-bit target would otherwise
   expose.

4. **Which wasm target first: `wasm32-unknown-unknown` or `wasm32-wasip1`?**
   *Recommendation:* `wasm32-unknown-unknown` first. It is the freestanding
   one, it matches the programme's own claim, and it is the one that has no
   dependencies: no WASI, no libc, no host beyond "call this function and
   give me a memory". `wasm32-wasip1` is then a host shim on top of it, not
   a second runtime.

5. **Does the collector need to be aware of the arena boundaries?** *If the
   heap is a fixed region, yes: the sweep must return memory to the arena's
   free list, and the collector must not walk past the end of the region.*
   *Recommendation:* design the collector so that the free operation is a
   parameter of the allocator rather than a hard-coded `free` call, which is
   a small change to `crates/nexum-runtime/src/lib.rs:186` and makes the
   arena model of D009 implementable without touching the collector.

6. **Who runs the wasm modules in the tests?** *Recommendation:* a host in
   the test harness, not in the driver. The driver produces the module; the
   test suite runs it under a host it chooses (Node, wasmtime) and compares
   the output with the same `.out` fixtures the native tests use. The
   existing `run` fixtures are already the right corpus - in my experiment,
   16 of 21 of them passed unchanged.

7. **Is the `f32`/NaN question a wasm question?** *Recommendation:* no. The
   strict-float subset is a *language* decision with the same consequences
   on every target; the wasm deterministic profile is one reason to consider
   it, not a requirement of the target. Decide it in 11c, where the numeric
   tower is defined, and not in a wasm phase.
