# The native roadmap (Phase 11 and beyond)

Status: **active**. This is the plan that follows Phase 10 (self-hosting).

## The goal

Nexum stops being a general-purpose language that borrows the C library and
becomes one that owns the machine:

* A Nexum program depends on **nothing but the kernel**. No libc, no C ABI,
  no runtime borrowed from elsewhere.
* The standard library is **written in Nexum**.
* The compiler and its tooling may still be written in Rust and use LLVM -
  that is the *toolchain's* business. The *language* stops depending on C.
* Everything else rides on that: the systems work, the LLM framework, the
  self-hosted compiler.

The point is not ideological. It is that a language which cannot reach the
OS, cannot own its allocator, cannot express `f32`, and cannot touch a
device register cannot be used for the work Nexum is now aimed at. Those are
the missing pieces, and this is the order they have to arrive in.

## The audit: what is missing today

### The runtime still leans on libc

When the programme started, the generated program's runtime declared
seventeen external symbols:

| Area | Symbols |
| ---- | ------- |
| stdio | `printf`, `fputs`, `fputc`, `__acrt_iob_func` |
| process | `exit` |
| heap | `malloc`, `free` |
| strings | `strlen`, `memcpy`, `strcmp` |
| raw POSIX | `write` |
| files | `fopen`, `fclose`, `fseek`, `ftell`, `fread`, `fwrite` |

The system call layer, the runtime's own string routines, the runtime's
own file routines, the allocator and the output formatting have taken all
of them away. **A program built for Linux or macOS now declares no
external symbol at all.** The gate
`crates/nexum-driver/tests/freestanding.rs` pins that, so the list cannot
grow back.

Windows is still on the C library for its output and its file routines:
its kernel interface lives behind `ntdll`, so the system call stubs have
to be generated rather than assembled, and that is phase 11a.2.

So every program Nexum builds today is a C program with a Nexum front end.
The collector allocates through `malloc`, prints through `printf`, and
reaches the filesystem through `fopen`.

### The language cannot reach the machine

* **No OS access.** There is no `extern`, no syscall, no inline assembly, no
  volatile, no pointer type. A Nexum program cannot read a device register,
  cannot map memory, cannot trap a signal.
* **No numeric tower.** `Float` is `f64` only. There is no `f32`, no
  integer widths, no saturating or wrapping arithmetic, no `sqrt`, `exp`,
  `log`, `tanh`, `erf` - no math at all beyond `+ - * / %`.
* **No memory control.** The collector owns the heap; a program cannot
  allocate a fixed arena, map a region, or free deterministically (D009
  defers arenas).
* **No concurrency.** No threads, no atomics, no channels.
* **No SIMD.** LLVM can autovectorise a loop, but the language cannot
  express a vector, a stride, or a layout.
* **No unsafe.** No way to say "this pointer is mine, trust me".

### The toolchain

* The link step calls `clang`. The *program* is what must stop needing it;
  using `lld` (or our own linker) inside the Rust toolchain is fine.
* The verifier also calls `clang`; that can be replaced by linking the
  toolchain against LLVM's own libraries later.

## The phases

| Phase | What lands | Why it is here |
| ----- | ----------- | --------------- |
| **11a** | Freestanding runtime: a syscall layer, our own allocator, our own string, math and I/O routines. The runtime stops linking libc. | The keystone. Everything else needs a runtime that does not borrow the C one. |
| **11b** | Inline assembly in the language. | "Full control over every single thing in the hardware" needs a first-class primitive for it. |
| **11c** | The numeric tower: `f32`, integer widths, saturating/wrapping arithmetic, the math builtins (implemented by us, not libm). | ML and DSP need `f32`; the math builtins are needed by any non-trivial model. |
| **11d** | Memory and layout: pointers, packed records, `unsafe` regions, `volatile`, explicit arenas and deterministic cleanup (finishing D009). | Device programming, the GPU path, and the allocator all need it. |
| **11e** | Concurrency: threads, atomics, a scheduler, channels. | Training and inference both want the cores. |
| **11f** | SIMD and explicit layout: vectors, strides, intrinsics. | The difference between "works" and "fast". |
| **11g** | The native linker: the program is linked by us, not by `clang`. | The last toolchain dependency in the build path. |
| **11h** | The GPU path: a driver-native compute path - the kernel's own ioctls, not Vulkan. | The difference between a toy and a real model. |
| **11i** | The ML framework, now at full scale: `f32` tensors, autograd, layers, optimizers, BLAS-shaped kernels, data pipelines. | The thing this whole plan exists for. |
| **11j** | The toolchain self-hosts: the compiler is written in Nexum. | The proof that the language is complete. |

Phases 11a and 11b are the ones that change the character of the language;
the rest build on them.

### What 11a looks like now

A program built for Linux or macOS **declares no external symbol at all**.
It used to declare seventeen. What replaced them:

| Was | Is now |
| --- | ------- |
| `malloc`, `free` | a size-class allocator over `mmap`, with the free list in the collector's own next-object word |
| `printf`, `fputs`, `fputc` | the runtime's own formatters, with the float format being C's `%g` byte for byte |
| `exit` | the system call |
| `strlen`, `memcpy`, `strcmp` | the runtime's own string routines, on every platform |
| `fopen`, `fread`, `fwrite` | the system calls, through the runtime's file routines |
| libm's `exp`, `log`, `tanh`, `erf`, `pow`, `sqrt` and the rounding functions | the runtime's own, in 64-bit operations with no `i128` and no integer division |

Windows is still on the C library for its output and its file routines,
because its kernel interface lives behind `ntdll` and the system call
stubs have to be generated rather than assembled. Its heap is ours now:
`VirtualAlloc` is the kernel and not the C library.

The gate `crates/nexum-driver/tests/freestanding.rs` pins the list, so it
can only get shorter.

### Where the rest of the phases are

| Phase | State |
| ----- | ----- |
| **11b** inline assembly | in flight |
| **11c** the numeric tower | contracted, waiting for 11b |
| **11d** memory and layout | researched |
| **11e** concurrency | researched |
| **11f** vectors | researched |
| **11g** the native linker | researched |
| **11h** the GPU path | researched |
| **11i** the model framework | researched |
| **11j** self-hosting | researched |

## The decisions this implies

These are recorded in `docs/language-reference/decisions.md`:

* **D012** - The runtime is freestanding: no libc.
* **D013** - The operating system is reached through inline assembly, never
  through a C binding.
* **D014** - Inline assembly is a first-class language feature, not a
  runtime-only trick.
* **D015** - The numeric tower is `f32`/`f64` plus fixed-width integers,
  with the arithmetic semantics spelled out per operation.
* **D016** - The generated program is linked by the Nexum toolchain, not by
  `clang`.

## What is already decided by this

* **Windows.** Windows has no direct system call instruction, and its
  kernel interface lives behind `ntdll`. Reaching it without a C binding
  means generating our own system call stubs - a real piece of work. The
  POSIX targets (Linux, macOS) go freestanding first; Windows follows in
  11a.2. Until then the Windows build keeps the UCRT path.
* **The compiler stays in Rust for now.** Self-hosting it (11j) is a
  separate, much larger project; the point of 11a-11i is that the *language*
  becomes capable, not that the compiler moves.
* **No GPU driver in the compiler.** The GPU path is a library the user
  links, talking the driver's binary protocol. That is how "no C FFI" and
  "use the GPU" coexist.

## Honest limits

* Without BLAS, the matrix kernels are ours to write. That is fine for the
  sizes a systems language is good at, and it is why 11f exists.
* The collector's tracing is proportional to what it scans. Large buffers
  with references are expensive; `Float` buffers are not scanned at all
  (they hold no references), which is what makes them cheap.
* A driver-native GPU path is a large project on its own: the ioctl
  protocol, the shader compiler front end, the memory model.

## Where the work is now

Phase 11a is under way. The first slice is done:

* **The system call layer** (`crates/nexum-runtime/src/syscalls.rs`): the
  wrappers are inline assembly in the generated IR, for Linux and macOS on
  x86-64 and AArch64. The arguments are pinned to the registers the
  instruction reads them from, the system call number is a constant in the
  assembly text, and the result comes back in the register the ABI uses.
* **Our own string routines** (`nexum_strlen`, `nexum_memcpy`,
  `nexum_strcmp`), on every platform: a program should not need the C
  library for a byte copy.
* **Our own file routines** on the POSIX targets: `read_file` and
  `write_file` go through the system calls, and the size of a file comes
  from seeking to the end of it.

A program built for Linux now declares six external symbols - `printf`,
`fputs`, `fputc`, `exit`, `malloc` and `free` - where it used to declare
four. The next slice replaces the allocator and the output formatting,
which leaves the C library behind entirely.

The layer is verified on all three platforms: a program built for Linux
opens, reads and writes a file through it, and the same code runs on macOS.

The cross-target verification now assembles for the target the module
claims, not for the host: a module that contains assembly for a foreign
instruction set cannot be checked by the host assembler.
