# The GPU path without CUDA and without a C binding

**Question.** How does a Nexum program use a GPU, given that the language may
not use a C ABI, may not use CUDA or OpenCL, and may not borrow the C
library? This document works out what the options really are, what each
costs, and which one the programme should do.

**Summary.** The recommendation is the CPU path first, and a Linux/AMD
driver-native GPU path scoped to one vendor, with the GPU work split into a
cheap toolchain-side spike and an expensive runtime path. The central
correction is that the question's premise about Vulkan is wrong: the Vulkan
API is a C API implemented by a shared library, not a binary protocol, and
the only thing that consumes SPIR-V is that same C-ABI library - so
"Vulkan spoken directly" does not exist, and a driver-native path has to
supply the *hardware* instruction set, not SPIR-V. The binary protocols that
do exist are the Linux kernel's DRM ioctls (public, stable UAPI) and the
vendor's own execution ioctls (public for AMD and Intel, closed for
NVIDIA). Metal on Apple silicon is not reachable at all without an
Objective-C ABI. The honest cost of a first working kernel is one phase on
one vendor, not one sprint, and the CPU path - with the f32 tower, the math
builtins and the vector support already planned for 11c, 11e and 11f - is
good enough to land 11i without the GPU.

## Recommendation

1. **Split 11h into 11h.0 (a spike) and 11h.1 (the path).** 11h.0 is a
   toolchain-side proof that the *design* works: a Nexum `kernel` function
   compiled by the Nexum compiler into a device code object, dispatched by
   a small Rust tool that speaks the kernel's ioctl protocol directly, and
   checked against the CPU result and measured. The tool is a *tool*, like
   the compiler, so it is outside the "the language's programs link no C
   library" rule; and because it uses raw syscalls rather than a Vulkan
   library, it also demonstrates that the protocol can be spoken without a
   C ABI at all. It costs days, not months, and it settles the language
   surface, the f32 semantics, the memory model, the synchronisation model
   and the performance. Everything in 11h.1 depends on 11h.0 having been
   done.

2. **Do the CPU path first, and do it properly.** 11c (f32, integer widths,
   our own math builtins), 11e (threads, atomics) and 11f (vectors, strides)
   are what make 11i possible at all, and they are also what makes a
   fallback device possible later. Reason: 11i is the phase that matters, and
   it can be landed on the CPU; the GPU multiplies throughput, it does not
   add capability. A framework that is correct on the CPU and 10x too slow
   is worth infinitely more than a framework that is half-written and waits
   for a driver.

3. **Define "no C binding" before designing anything, and record it as a
   decision.** The programme means two different things by it, and they lead
   to different answers. Recommend the **binary-level** reading: *the
   program links no C library; it reaches the GPU through the kernel's
   ioctl protocol.* Under the source-level reading ("no `extern "C"` in
   Nexum source"), every option opens up - Metal included - and 11h becomes
   a week of work with `ash` or an Objective-C shim. This decision (call it
   D017) is the fork in the road and the orchestrator has to make it.

4. **Scope 11h.1 to Linux + AMD + one GPU family, and only under the
   binary-level reading.** Concretely: the DRM UAPI in
   `include/uapi/drm/drm.h` and `include/uapi/drm/amdgpu_drm.h`, an
   AMDGCN-ISA shader blob produced at build time, and the kernel's own
   execution ioctls. AMD is the only vendor where all three of the
   following are true: the execution interface is public UAPI, the
   instruction set is publicly documented, and the toolchain's existing
   LLVM can already produce the instruction set. Estimate: **8-12
   engineer-weeks** for one vendor, one GPU family and one kernel shape.

5. **Do not build a SPIR-V front end in 11h.** SPIR-V is the wrong target
   for a driver-native path, because the only consumer of SPIR-V is the
   C-ABI driver we are trying not to call. A Nexum-to-SPIR-V generator is a
   5,000-15,000 line project that buys nothing until 11h.1 exists, and
   hand-written SPIR-V is a 1,500-2,500 line assembler that buys the same
   nothing. If the programme wants a portable shader story later, the
   portable thing is not SPIR-V - it is "one Nexum kernel, compiled per
   vendor by the toolchain".

6. **Make the shader a build-time artefact, not a runtime one.** The
   compiler already emits LLVM IR (`crates/nexum-codegen/src/lib.rs`) and
   already shells out to `clang` (`crates/nexum-toolchain/src/lib.rs:178-208`).
   A `kernel` function is compiled a second time, for the GPU target, by
   the same `clang`, with LLVM's AMDGPU backend. The result is a code
   object embedded in the program as a global. There is no runtime shader
   compiler, no shader language, and no SPIR-V anywhere in the picture. The
   rules explicitly allow this: "The compiler and its tooling may still be
   written in Rust and use LLVM" (`docs/native-roadmap.md:13-14`).

7. **macOS gets no GPU path in 11h.** Metal's only public compute interface
   is an Objective-C one, reached through `MTLCreateSystemDefaultDevice`
   and Objective-C message sends, which means `libobjc` and the dynamic
   linker. There is no Metal binary protocol to speak. Under the
   binary-level reading, macOS is out; under the source-level reading it is
   a shim. The honest substitute for Apple silicon is the CPU path, which
   is a good one: 128-bit NEON (four `f32` lanes per instruction, several
   such units per core), unified memory, and no copy between the
   "accelerator" and the "host" because there is only one memory.

8. **NVIDIA is out.** Its kernel module is open, its user-space driver,
   its Vulkan ICD and its shader compiler are closed binaries behind an
   undocumented protocol. Reimplementing that protocol is a multi-year
   reverse-engineering project (the ZLUDA scale), and it is not what phase
   11h is for. One narrow exception is worth recording: NVIDIA accepts
   **PTX**, a documented text virtual ISA, and JITs it in the driver - so
   NVIDIA is the one vendor where a driver-native path would *not* need a
   shader compiler. It does not help, because the protocol underneath is
   the closed one.

9. **Windows is out for now, and the reason is not the GPU.** Windows has
   no direct system call instruction and the runtime is still on the C
   library (`docs/runtime-abi.md:28-32`). Its GPU interface, `D3DKMT`, is
   however a *public binary protocol* over `dxgkrnl`, so the same shape
   of work is possible later. It is an 11a.2 problem, not an 11h problem.

10. **Before 11i starts, the GPU path must have proven seven things.**
    They are listed in "What the GPU path needs to have been proven" below.
    If the seventh (throughput) fails, the honest conclusion is that 11h
    does not pay for itself, and the programme should say so rather than
    carry the phase.

## What the code looks like today

Only the facts the recommendation depends on.

### The rules the GPU path has to satisfy

* `docs/native-roadmap.md:10-14` - "A Nexum program depends on **nothing
  but the kernel**. No libc, no C ABI, no runtime borrowed from elsewhere."
  The compiler and its tooling may still be Rust and LLVM; that is the
  toolchain's business.
* `docs/native-roadmap.md:107-109` - "**No GPU driver in the compiler.** The
  GPU path is a library the user links, talking the driver's binary
  protocol. That is how 'no C FFI' and 'use the GPU' coexist." This is the
  sentence that sets the task, and it is the one that the option analysis
  below qualifies.
* `docs/native-roadmap.md:118-120` - "A driver-native GPU path is a large
  project on its own: the ioctl protocol, the shader compiler front end, the
  memory model." The roadmap already knows this.
* `docs/language-reference/decisions.md:178` (D012) - the runtime is
  freestanding, no libc. `:198` (D013) - the OS is reached through inline
  assembly, "never through a C binding". `:210` (D014) - inline assembly is
  a first-class language feature, "so that it can reach what the language
  does not model". `:222` (D015) - the numeric tower is f32/f64. `:233`
  (D016) - the program is linked by the Nexum toolchain.
* `docs/runtime-abi.md:11-32` - what still comes from the C library:
  `malloc`/`free`, `printf`/`fputs`/`fputc`, `exit`, and the Windows file
  routines. All of that is 11a.2, and none of it is the GPU's problem.

### The parts of the repository the design leans on

* **The runtime is LLVM IR text, not a compiled library.**
  `crates/nexum-runtime/src/lib.rs:1-5` says so, and
  `crates/nexum-codegen/src/lib.rs:119` splices `runtime_ir(&target.os,
  &target.arch)` into the module that becomes the program. The runtime is
  emitted as `internal` definitions into the user's program, so it costs
  the program nothing and links nothing. A GPU library built the same way -
  a Rust crate that emits IR text, spliced in by the compiler - inherits
  exactly the "no C library" property, and it is the obvious home for the
  driver protocol.
* **The system call layer is the model for the ioctl layer.**
  `crates/nexum-runtime/src/syscalls.rs:237-292` emits one wrapper per
  call; each takes its arguments as `i64` and returns the raw result.
  `:168-231` builds the assembly: the number is an immediate (`:186-189`),
  the arguments are pinned to registers by the constraint string
  (`:211-218`), the stack is aligned for the instruction (`:174-181`). An
  `ioctl` wrapper is the same shape with one extra argument: the request
  number is a constant, and the third argument is a pointer to a struct the
  runtime lays out itself.
* **The number table is per system and is pinned by a test.**
  `crates/nexum-runtime/src/syscalls.rs:56-91` holds it and
  `:300-317` (`the_numbers_are_per_system`) asserts the values. The table
  has no `ioctl` yet; adding it follows the same pattern.
* **The toolchain already shells out to `clang`.**
  `crates/nexum-toolchain/src/lib.rs:178-208` builds the program, and
  `:221-248` (`verify_ir_for`) compiles the IR *for a target triple*, which
  is exactly the hook a GPU-target compile needs.
* **The target model has no GPU capability.**
  `crates/nexum-target/src/lib.rs:4-9` (`TargetSpec`) has a triple, a
  pointer width, an OS and an architecture, and `:41-72` lists five
  targets. A GPU path needs a per-target answer to "which driver, which
  instruction set", and that does not fit in the current struct.
* **The language has no pointers, no f32, no atomics and no vectors yet.**
  `docs/language-reference/grammar.md:48-63` gives the statement forms
  (`fn`, `type`, `let`, assignments, control flow); `:19` gives the keyword
  list; `:17` says `#` starts a comment, so there is no attribute syntax
  to hang a kernel marker on. `docs/language-reference/decisions.md:41`
  (D003) says errors are `panic` plus static diagnostics - there is no `?`
  operator to design around. Phase 11d (pointers), 11c (f32), 11e
  (atomics) and 11f (vectors) all land before 11h
  (`docs/native-roadmap.md:69-77`).

### The kernel interfaces that exist - and the one that does not

This is the part of the research that changes the answer, so it is worth
being exact. Verified against the Linux kernel's public UAPI headers
(`include/uapi/drm/`) and the Vulkan loader's source.

| Interface | What it actually is | Can a freestanding program speak it? |
| --------- | ------------------- | ------------------------------------ |
| Vulkan API | A **C API** in a shared library (`libvulkan.so.1`), dispatched through the loader | No |
| Vulkan loader | A C library that reads JSON manifests, `dlopen`s ICDs, resolves `vk_icdGetInstanceProcAddr` | No |
| SPIR-V | A binary module of 32-bit words; a *shader* input format | Yes - but only a C-ABI driver reads it |
| DRM core ioctls | `ioctl(2)` with a request number and a struct we lay out | **Yes** |
| AMD / Intel execution ioctls | `ioctl(2)`, per-vendor, public UAPI on both | **Yes** |
| AMDGCN ISA, Intel GT ISA | Binary machine code the kernel driver executes | **Yes** |
| PTX | A text virtual ISA; the NVIDIA *user-space* driver JITs it | Yes, but the driver that JITs it is closed |
| Metal | Objective-C objects and message sends inside a framework | No |
| NVIDIA user space | Closed binary protocol | No |

**There are no `DRM_IOCTL_VULKAN_*` ioctls in the Linux kernel.** The
kernel's public DRM header (`include/uapi/drm/drm.h`) defines the core
interface - `DRM_IOCTL_VERSION`, `DRM_IOCTL_GEM_CLOSE/FLINK/OPEN`,
`DRM_IOCTL_PRIME_HANDLE_TO_FD`, `DRM_IOCTL_PRIME_FD_TO_HANDLE`, and the
whole `DRM_IOCTL_SYNCOBJ_*` family - and no Vulkan-specific entry, and
there is no separate Vulkan UAPI header anywhere in the tree. Whatever
the DRM Vulkan proposal was, it is not in the kernel. That matters: the
memory and synchronisation primitives a driver-native path needs are
*generic DRM*, not Vulkan-specific, and they are stable.

**The generic DRM execution interface is not public UAPI either.**
`include/drm/drm_exec.h` is a kernel-internal header, so each driver still
has its own execution ioctls. For AMD that is `amdgpu_drm.h`; for Intel it
is `xe_drm.h`. Both are public.

**The Vulkan loader is a C library and it is not needed.**
`Vulkan-Loader/loader/loader.c:3553-3783` reads the ICD manifests from
`XDG_CONFIG_HOME`, `XDG_CONFIG_DIRS` and `SYSCONFDIR` (in practice
`/etc/vulkan/icd.d` and `/usr/share/vulkan/icd.d`), with `VK_DRIVER_FILES`
and `VK_ICD_FILENAMES` overriding; `:2172-2205` resolves
`vk_icdGetInstanceProcAddr` and `vk_icdNegotiateLoaderICDInterfaceVersion`
out of the `dlopen`ed ICD. Every step of that is a C ABI. A program that
does not use Vulkan does not need any of it: the GPU is a device node and a
set of ioctls.

### What the kernel actually offers, per vendor

**AMD (`include/uapi/drm/amdgpu_drm.h`).** This is the richest public
interface:

* `DRM_IOCTL_AMDGPU_GEM_CREATE` (sub-number `0x00`),
  `DRM_IOCTL_AMDGPU_GEM_MMAP` (`0x01`), `DRM_IOCTL_AMDGPU_CTX` (`0x02`,
  which allocates the GPU context with `AMDGPU_CTX_OP_ALLOC_CTX`),
  `DRM_IOCTL_AMDGPU_BO_LIST` (`0x03`), `DRM_IOCTL_AMDGPU_CS` (`0x04`) - the
  command-stream submission, `DRM_IOCTL_AMDGPU_INFO` (`0x05`),
  `DRM_IOCTL_AMDGPU_GEM_WAIT_IDLE` (`0x07`),
  `DRM_IOCTL_AMDGPU_GEM_VA` (`0x08`), `DRM_IOCTL_AMDGPU_WAIT_CS` (`0x09`),
  `DRM_IOCTL_AMDGPU_GEM_USERPTR` (`0x11`), `DRM_IOCTL_AMDGPU_VM` (`0x13`),
  `DRM_IOCTL_AMDGPU_SCHED` (`0x15`), `DRM_IOCTL_AMDGPU_USERQ` (`0x16`),
  `DRM_IOCTL_AMDGPU_USERQ_SIGNAL` (`0x17`), `DRM_IOCTL_AMDGPU_USERQ_WAIT`
  (`0x18`).
* The GEM memory kinds are public: `AMDGPU_GEM_DOMAIN_CPU` (system memory,
  not GPU accessible), `AMDGPU_GEM_DOMAIN_GTT` (system memory mapped into
  the GPU address space), `AMDGPU_GEM_DOMAIN_VRAM` (local video memory),
  `AMDGPU_GEM_DOMAIN_GWS` (global wave synchronisation across compute
  units) and `AMDGPU_GEM_DOMAIN_DOORBELL`.
* `drm_amdgpu_cs_chunk_ib` is the submission unit: a GPU virtual address
  (`va_start`), a length in bytes (`ib_bytes`), a hardware IP type
  (`AMDGPU_HW_IP_COMPUTE` is `1`) and an IP instance. The bytes at
  `va_start` are the command stream, and for compute they are the
  hardware instruction stream.
* `DRM_IOCTL_AMDGPU_USERQ` is the modern queue-creation interface
  (`AMDGPU_USERQ_OP_CREATE`/`_FREE`), with `DRM_IOCTL_AMDGPU_USERQ_SIGNAL`
  and `DRM_IOCTL_AMDGPU_USERQ_WAIT` for synchronisation.

**Intel (`include/uapi/drm/xe_drm.h`).** Also entirely public:
`DRM_IOCTL_XE_DEVICE_QUERY` (`0x00`), `DRM_IOCTL_XE_GEM_CREATE` (`0x01`),
`DRM_IOCTL_XE_GEM_MMAP_OFFSET` (`0x02`), `DRM_IOCTL_XE_VM_CREATE` (`0x03`),
`DRM_IOCTL_XE_VM_DESTROY` (`0x04`), `DRM_IOCTL_XE_VM_BIND` (`0x05`),
`DRM_IOCTL_XE_EXEC_QUEUE_CREATE` (`0x06`), `DRM_IOCTL_XE_EXEC_QUEUE_DESTROY`
(`0x07`), `DRM_IOCTL_XE_EXEC_QUEUE_GET_PROPERTY` (`0x08`),
`DRM_IOCTL_XE_EXEC` (`0x09`), `DRM_IOCTL_XE_WAIT_USER_FENCE` (`0x0a`),
`DRM_IOCTL_XE_OBSERVATION` (`0x0b`), `DRM_IOCTL_XE_MADVISE` (`0x0c`).
`DRM_XE_EXEC` takes an exec-queue id, an array of `drm_xe_sync` objects, and
a batch-buffer address; the batch buffer holds the hardware instruction
stream. `DRM_XE_WAIT_USER_FENCE` waits on a *value in memory* with a
comparison operator (`EQ`, `NEQ`, `GT`, `GTE`, `LT`, `LTE`) and a mask -
which is a GPU-visible atomic, written by the GPU and read by the CPU.

**Generic synchronisation.** `DRM_IOCTL_SYNCOBJ_CREATE`, `_DESTROY`,
`_HANDLE_TO_FD`, `_FD_TO_HANDLE`, `_WAIT`, `_RESET`, `_SIGNAL`,
`_TIMELINE_WAIT`, `_TIMELINE_SIGNAL`, `_QUERY`, `_TRANSFER` and
`_EVENTFD` are all in the public `drm.h`. The timeline syncobj is a 64-bit
counter in a kernel object; the eventfd variant lets a completion wake a
file descriptor the program can `poll`.

**Apple.** `include/uapi/drm/asahi_drm.h` exists in the kernel - the Asahi
driver for Apple silicon's AGX GPU has a public UAPI. That is a *Linux*
interface for Apple hardware, it is graphics-only, and its instruction set
is reverse-engineered. It is not a macOS answer.

### The system calls the path needs

The existing table is in `docs/runtime-abi.md:56-72` and
`crates/nexum-runtime/src/syscalls.rs:56-91`. The GPU path adds:

| Call | Linux x86-64 | Linux aarch64 | Darwin | Note |
| ---- | ------------ | ------------- | ------ | ---- |
| `ioctl` | 16 | 29 | 54 | the whole driver interface |
| `openat` | 257 | 56 | (`open` = 5) | the device node |
| `getdents64` | 217 | 61 | (`getdirentries` = 196) | enumerate `/dev/dri` |
| `mmap` | 9 | 222 | 197 | already in the table |
| `poll` | 7 | 7 | 230 | wait on a fence fd |
| `futex` | 202 | 98 | - | wake a CPU-side waiter |
| `eventfd2` | 290 | 19 | - | a completion as a readable fd |

Verified: the Linux numbers from `arch/x86/entry/syscalls/syscall_64.tbl`
and `include/uapi/asm-generic/unistd.h`; the Darwin numbers from XNU's
`bsd/kern/syscalls.master` (line 108: `54 AUE_IOCTL ALL { int ioctl(int
fd, u_long com, caddr_t data) NO_SYSCALL_STUB; }`, line 196
`AUE_GETDIRENTRIES`, line 230 `AUE_POLL`). Darwin's numbers are
per operating system, not per architecture - the existing table already
relies on that (`crates/nexum-runtime/src/syscalls.rs:73-88` gives one set
for both architectures). The Linux numbers *are* per architecture, so the
`Syscalls` struct will need per-architecture entries, which is a small
change to `crates/nexum-runtime/src/syscalls.rs:37-54`. All of these
should be pinned by a test in the style of `the_numbers_are_per_system`
(`:300-317`). **I could not run these numbers on a Linux or macOS machine -
this host is Windows - so they are verified against the kernel and XNU
sources, not executed.**

One macOS caveat: there is no `futex` on Darwin. The equivalent is
`__ulock_wait`/`__ulock_wake`, which are libsyscall wrappers rather than
plain system calls, so 11e's atomic wait will need a per-OS story
anyway - another reason that the CPU-side wait is worth building before
the GPU path, not after.

The `ioctl` request number itself is not a number we choose: it is
`_IOWR('d', nr, type)`, where `nr` is `DRM_COMMAND_BASE + sub-number`
(`DRM_COMMAND_BASE` is `0x40`), and the encoding is
`dir(2) | size(14) | type(8) | nr(8)` shifted as
`dir<<30 | size<<16 | 'd'<<8 | nr`, with `dir = 3` for read-write. The
`size` field is `sizeof` of the C struct, which the runtime must
reproduce byte for byte, padding included. **This is the single most
error-prone part of the whole path and it is where the first tests have to
go.**

## The design

### 1. The correction: three interfaces, not one

The question describes the Vulkan driver interface as "a binary protocol,
so it can be spoken directly". It is not. There are three different
interfaces stacked on top of each other, and they have different answers:

```
    your program
        |
        |  (1) Vulkan API  -- a C ABI, in a shared library.  NOT a protocol.
        v
    the ICD (libvulkan_radeon.so, libvulkan_intel.so, libGLX_nvidia.so)
        |  (2) SPIR-V in, hardware ISA out.  The compiler lives HERE.
        v
    the kernel driver
        |  (3) ioctl(2) with a request number and a struct.  A protocol.
        v
    the hardware
```

* Layer (1) cannot be spoken by a program that may not use a C ABI. Full
  stop. The Vulkan API is `vkCreateDevice`, `vkAllocateMemory`,
  `vkMapMemory`, `vkCreateShaderModule`, `vkQueueSubmit`,
  `vkWaitForFences` - a C function table, reached through the loader's
  dispatch. There is no wire format. Even the "direct to ICD" loader mode
  still `dlopen`s the ICD and calls its C entry points.
* Layer (2) is where the shader compiler lives. Mesa's `radv`, Intel's
  `libigc`, NVIDIA's `libnvidia-glvkspirv` all contain an LLVM-based
  SPIR-V-to-hardware compiler. **No kernel driver in existence accepts
  SPIR-V.** A kernel driver accepts the *hardware* instruction set.
* Layer (3) is the part that is a real binary protocol, and it is public
  and stable for the parts that are generic (DRM GEM, PRIME, syncobj) and
  per-vendor for the parts that are not (the execution ioctls).

So the driver-native path is not "speak Vulkan directly". It is "skip
layers (1) and (2), and implement the small part of (3) that the hardware
needs". That is a real, bounded project - and it is *smaller* than a Vulkan
implementation, because a compute-only path needs a device, some memory, one
queue, one dispatch and one fence, not a full graphics API.

It also means the shader story changes. Section 5 works it through.

### 2. Option (a): Vulkan compute, spoken directly

**As stated, this option does not exist.** Three ways to read it, and what
each one is worth:

**(a1) Skip the loader, use the ICD's C API directly.** You still need a C
ABI. Worthless under the binary-level reading. Under the source-level
reading it is a day of work: `dlopen` the ICD, resolve
`vk_icdGetInstanceProcAddr`, call it. This is what every "loaderless
Vulkan" project does.

**(a2) Implement the Vulkan *client* semantics on the kernel UAPI.** That
is, write a mini-Vulkan: device enumeration, memory allocation, queue
creation, command buffer encoding, submission, fences - all on top of
`ioctl(2)`. This is real work but it is bounded, and it is what section 8
describes for AMD. The problem is not the Vulkan part; it is that this
mini-Vulkan has to be given *something* to execute, and the only thing it
can execute is the hardware instruction set, which the ICD would have
compiled from SPIR-V. So (a2) degenerates into option (c).

**(a3) Write a Nexum-to-SPIR-V front end and hand SPIR-V to the ICD.**
Rejected: the hand-off is a C ABI call.

**What Vulkan is good for**: it is the only *portable* GPU interface there
is. One shader, many vendors. If the programme ever wants a GPU path that
covers every vendor with one implementation, Vulkan is it, and the price is
a C ABI in the program. That is a coherent position - it is just not the
one `docs/native-roadmap.md:10-12` describes.

**A note on SPIR-V's size, for the record.** SPIR-V is a binary module: a
five-word header (magic `0x07230203`, version, generator, id bound,
schema) followed by instructions, each a word count, an opcode and typed
operands. Writing an *assembler* for a restricted compute subset is maybe
1,500-2,500 lines; writing a *generator* from a source language is
5,000-15,000 lines. Both are estimates, not measurements. Neither is
worth doing now, because both produce a file that only a C-ABI library
can consume.

### 3. Option (b): Metal on Apple silicon

**Metal has the same shape as Vulkan, and the shape does not help.**

* The public compute interface is Objective-C: `MTLDevice`,
  `MTLCommandQueue`, `MTLCommandBuffer`, `MTLComputePipelineState`,
  `MTLResource`. You get a device from `MTLCreateSystemDefaultDevice()` -
  a C function returning an Objective-C object - and then you send
  Objective-C messages.
* To send an Objective-C message from a program that may not use a C ABI,
  you need the Objective-C runtime: `objc_msgSend`, class lookup, the
  selector machinery. `libobjc` is a C library and is part of libSystem.
  Resolving `objc_msgSend` without the dynamic linker means parsing the
  dyld shared cache by hand. Both are the same kind of work as "reimplement
  the C library", which is what the whole programme is trying to avoid.
* **There is no Metal compute binary protocol.** The command buffer is an
  Objective-C object graph, not a byte stream. (The one binary format
  Metal has is the *shader*: the Metal Shading Language is compiled by
  `xcrun metal` into AIR, an undocumented Apple intermediate
  representation, and the runtime loads it through
  `newLibraryWithData:`. That is a build-time artefact handed to a C-ABI
  call - the same shape as SPIR-V, and no better.)
* **Discovery** on macOS is the IOKit registry, whose API
  (`IOServiceMatching`, `IOServiceGetMatchingService`) is a C framework.
  There is no `/dev` node for the GPU and no `/proc` equivalent. So the
  question's own framing is right: IOKit is a C library.

**Conclusion.** Under the binary-level reading of "no C binding", macOS
cannot have a GPU path. Not "is hard" - cannot, because the only public
interface is an object-oriented C ABI with no wire format underneath.

The good news is that the *substitute* is strong. Apple silicon's CPU
cores have 128-bit NEON (four f32 lanes), the memory is unified, and
there is no copy between "host" and "accelerator" because there is only
one memory. A well-written f32 BLAS on an M-series core is a serious
piece of number-crunching. The GPU's advantage on Apple silicon is
mostly large-batch throughput and the matrix units, which for a language's
first ML framework is a "later" problem.

**One more thing worth recording.** If the programme ever relaxes to the
source-level reading, macOS becomes the *easy* GPU platform: a thin
Objective-C shim, MSL compiled by `xcrun metal` at build time, and a
`MTLCommandBuffer` encode-and-commit. That is a week of work, and it is
the only platform where a GPU path is cheap. This is worth knowing before
the programme commits to the strict reading, because the strict reading
costs it the entire Apple user base.

### 4. Option (c): the vendor's own protocol, per platform

This is the only option that satisfies the binary-level reading. What it
costs per platform:

| Platform | Interface | Public? | Shader input | Verdict |
| -------- | --------- | ------- | ------------ | ------- |
| Linux + AMD | `amdgpu_drm.h` | **Yes**, stable UAPI | AMDGCN ISA (documented) | **The one to do** |
| Linux + Intel | `xe_drm.h` | **Yes**, stable UAPI | Intel GT ISA (largely undocumented) | Later, and expensive |
| Linux + NVIDIA | proprietary | No | PTX (documented) | No |
| macOS | none | - | AIR (undocumented) | No |
| Windows | `D3DKMT` | Yes (WDK headers) | DXIL (documented) | Later, after 11a.2 |

Notes on each:

* **AMD** is the sweet spot for three independent reasons, and all three
  have to hold. The execution interface is public UAPI. The instruction
  set (GCN, RDNA 1/2/3) is published by AMD. And the toolchain's existing
  LLVM has an AMDGPU backend that produces exactly that instruction set.
  The AMD path is therefore the *only* one where a first kernel is
  reachable without writing a compiler.
* **Intel** has the interface but not the instruction set. The Xe driver
  takes a *batch buffer* (`drm_xe_exec.address`, `num_batch_buffer`) and
  the public header says nothing about what is inside it beyond that it
  is the thing the engine executes. The contents are a **GT ISA** blob in
  a container format the header does not describe. Intel's IGC (the
  compiler that produces it) is open source, but the *format* of its
  output is effectively undocumented, and the ISA itself is only
  partially described. So on Intel the work includes either
  reverse-engineering the ISA or reimplementing IGC's output convention.
  That is a research project, not a phase.
* **NVIDIA** is the awkward one. The open kernel modules (R560 and later)
  give you the kernel side. The user-space driver, the Vulkan ICD and the
  shader compiler are closed binaries, and the protocol between the
  user-space driver and the kernel module is undocumented. ZLUDA exists
  precisely because this protocol is hard, and ZLUDA is a multi-year
  effort by a specialist. The one bright spot: NVIDIA's driver JITs
  **PTX**, a documented text virtual ISA, so a driver-native NVIDIA path
  would not need a shader compiler. That does not help, because the
  protocol underneath is the closed one.
* **Windows** is genuinely a binary protocol - `D3DKMT` over `dxgkrnl` -
  and DXIL is documented - but the runtime is still on the C library
  (`docs/runtime-abi.md:28-32`) and Windows has no system call
  instruction, so the whole platform is 11a.2's problem first.

### 5. The shader story

**What a compute shader is in each model.**

* **Vulkan / SPIR-V.** A compute shader is a SPIR-V module with one
  `OpEntryPoint` in the `GLCompute` execution model, a `LocalSize`
  execution-mode decoration, and bindings for its inputs. It runs as a
  *grid of workgroups*; each workgroup is a group of invocations that may
  share scratch memory. The unit of work is a thread, and the builtins
  are `GlobalInvocationID`, `LocalInvocationID`, `WorkgroupID`,
  `NumWorkgroups`.
* **Metal / MSL.** The same shape, expressed in a C++-like language:
  `[[kernel]] void k(...)` with `[[thread_position_in_grid]]`,
  `[[thread_position_in_threadgroup]]`, `[[threadgroups_per_grid]]`.
  Compiled to AIR, then to the GPU's instruction set by Apple's compiler.
* **AMDGCN ISA.** No source language at all: the kernel is a machine-code
  blob. A compute dispatch is a grid of wavefronts; the "workgroup size"
  is the wavefront size (32 or 64); the grid is described by a *kernel
  descriptor* (a 64-byte block, pointed at by the `s_kernel_desc` system
  register, whose first two 32-bit fields carry the grid size, the
  workgroup size, the shared-memory size and the register counts), and a
  *dispatch packet* in the command stream that sets up the compute queue
  and launches it. The hardware also has the "global wave sync" memory
  (`AMDGPU_GEM_DOMAIN_GWS`) for inter-CU barriers, and the LDS (local data
  share) for workgroup-private memory. **The byte-exact layout of the
  kernel descriptor and the dispatch packet is from AMD's ISA
  documentation, not from the kernel headers, and I did not verify it
  here** - it is one of the things the 11h.0 spike exists to pin down.
* **Intel GT ISA.** The same idea, different encoding, undocumented.
* **PTX.** A virtual ISA: a text form, register-typed, with a documented
  `%tid`/`%ctaid` addressing model. The *driver* JITs it. This is the one
  model in which the shader is portable text rather than a blob.

**What the language would look like.** Two decisions: how a kernel is
declared, and how the host dispatches it.

The declaration has to be a new top-level form, because the current
grammar has no attributes (`#` is a comment,
`docs/language-reference/grammar.md:17`) and no other marker. A `kernel`
keyword parallel to `fn` is the least surprising:

```
kernel saxpy(n: Int, alpha: F32, x: GpuMem(F32), y: GpuMem(F32)) :
    let lane = gpu_local_id_x()
    let gid  = gpu_group_id_x() * 64 + lane
    if gid < n :
        y[gid] = alpha * x[gid] + y[gid]
```

The builtins are the GPU's own: `gpu_local_id_x/y/z`,
`gpu_group_id_x/y/z`, `gpu_num_groups_x/y/z`, and - later - a
workgroup-local scratch buffer. The parameter types are device pointers,
which need 11d's pointer type and a "this points at device memory"
distinction; `GpuMem(F32)` is a proposal, not a decision.

The dispatch side is an ordinary library API, and it should look like
every other Nexum API: a set of records, methods, and `panic` on error
(D003 says there is no `?`).

```
let dev  = gpu.open_device(0)              # panics if there is none
let buf  = dev.alloc(F32, 4096, gpu.Memory.Device)
let host = buf.map_host()                  # a host-visible view
let q    = dev.queue(0)
q.dispatch(saxpy, gpu.Grid(1024, 1, 1), gpu.Workgroup(64, 1, 1), [a, b, c])
q.wait()
```

**Should the first version reuse the host language?** Three options, and
the recommendation is the third:

1. *Hand-written SPIR-V.* ~1,500-2,500 lines of assembler, produces a file
   only a C-ABI driver can consume. **Rejected**: wrong target, and the
   consumer does not exist under the strict reading.
2. *A Nexum-to-SPIR-V front end.* 5,000-15,000 lines, plus a new
   intermediate, plus the structured-control-flow rules Vulkan imposes.
   **Rejected**: enormous, and it produces a file only a C-ABI driver can
   consume. Its only virtue is portability, and portability is a
   second-phase concern.
3. *A Nexum kernel compiled by the Nexum compiler to the hardware
   instruction set.* **Recommended.** The kernel is a Nexum function with a
   `kernel` marker; the compiler emits it a second time with the GPU
   calling convention and the grid metadata; the toolchain's existing
   `clang` (`crates/nexum-toolchain/src/lib.rs:178-208`) compiles it for
   the GPU target; the code object is embedded in the program as a global.
   The language surface is one keyword and a handful of builtins. The
   compiler-side cost is the smallest of the three by an order of
   magnitude, and the rules explicitly permit the toolchain to use LLVM
   (`docs/native-roadmap.md:13-14`).

The cost of option 3 is that it is **per vendor**. AMD gets an AMDGPU
backend; Intel would need a GT-ISA backend; NVIDIA would need a PTX
backend (which, helpfully, LLVM has). In practice one Nexum kernel
becomes several blobs, and the toolchain picks the right one. That is
exactly how everyone else's multi-vendor stack works, and it is a much
smaller problem than a portable shader *language*.

**The build step, concretely.** The compiler already knows the target
(`crates/nexum-target/src/lib.rs`) and already compiles IR for a triple
(`crates/nexum-toolchain/src/lib.rs:221-248`). The GPU compile is the same
call with a different triple:

```
clang -target amdgcn-amd-amdgcn -mcpu=gfx1200 -O3 -nogpulib -x ir kernel.ll -c -o kernel.o
```

(`-nogpulib` keeps the compile from wanting the ROCm device libraries;
the exact triple and flags for a given GPU family are a detail the spike
settles.) The code object is then extracted from the resulting ELF (the
`.text` section plus the kernel descriptor) and emitted as a `private
constant` global in the program's IR. The runtime uploads it to the device
as an ordinary buffer. **Risk to note:** the installed `clang` must
include the AMDGPU target. LLVM release builds do; some distribution
builds do not. The toolchain should detect this and report it (`nx
doctor` already reports on `clang`,
`crates/nexum-driver/src/main.rs:905-913`).

### 6. The memory story

There are three ways for the CPU and the GPU to share data, and they have
very different costs. The choice is not "pinned vs coherent" - it is
"device memory + copies", "system memory the GPU maps", or "host memory
the GPU pins".

**(i) Device memory and copies.** The GPU has its own memory; the CPU
allocates a staging buffer in system memory, copies in, dispatches, copies
out. On AMD, `AMDGPU_GEM_DOMAIN_VRAM` for the device buffer and
`AMDGPU_GEM_DOMAIN_CPU` for the staging buffer, with a `memcpy` over PCIe
in between (DMA-BUF, or a driver-mediated copy).

*Cost*: two transfers per operation. At 12-16 GB/s over PCIe 4.0 x16, a
100 MB tensor costs about 7 ms each way. For anything that is not
transfer-dominated, this destroys the point of the GPU.

*(ii) System memory the GPU maps.* On AMD, `AMDGPU_GEM_DOMAIN_GTT` is
"GPU accessible system memory, mapped into the GPU address space" - the
buffer is ordinary system RAM that the GPU reads over PCIe. There is no
copy at all, but every access crosses PCIe, so the GPU runs at memory-
bandwidth-over-PCIe speed, not at HBM/GDDR speed. Useful for small
kernels, and for the first kernel of all.

*(iii) Host memory the GPU pins.* The buffer is allocated by the
program's own allocator, pinned (`mlock`/`MAP_LOCKED`), and its physical
pages are mapped into the GPU's address space - on AMD, `GTT` again; on
Intel, `DRM_XE_VM_BIND_OP_MAP_USERPTR` with a `pat_index` that selects
caching and coherency (`xe_drm.h:1080-1110` documents exactly this: for
coherency the `pat_index` must be at least 1-way coherent when the GEM's
`cpu_caching` is `DRM_XE_GEM_CPU_CACHING_WB`).

**The coherence problem, stated plainly.** On a discrete GPU, the CPU's
view of device memory and the GPU's view of it are *not* coherent by
default. The CPU writes to a mapped VRAM buffer over PCIe; the GPU's L2
may hold stale lines. The reverse is worse: the GPU writes, the CPU reads
a stale line out of its own cache. So a shared buffer needs one of:

* a *coherent* mapping (1-way or 2-way coherent `pat_index`, and on AMD a
  flush of the CPU cache lines), which costs performance;
* an explicit *flush/invalidate* pair around each dispatch, which is what
  `AMDGPU_IB_FLAG_EMIT_MEM_SYNC` is for;
* or *avoid sharing entirely* and use copies.

For a first version, the sane answer is: **share for the input, copy for
the output, or use coherent mappings and measure.** A framework that
silently produces stale results because of a cache-coherency bug is the
worst possible outcome, so the first version should use the *slowest*
correct option (coherent mapping, or explicit flush) and optimise later.

**The allocation story in a freestanding runtime.** `nexum_alloc` goes
through `malloc` today (`crates/nexum-runtime/src/lib.rs:147`) and 11a.2
replaces it with the runtime's own allocator. The GPU buffers should be
**outside** the managed heap entirely: they are raw device memory with no
references in them, they are not scanned, and their lifetime is explicit.
They should be a distinct type, allocated and released explicitly - which
means 11d's `unsafe` regions and explicit arenas (D009) have to land
first, exactly as `docs/native-roadmap.md:72` says.

### 7. The synchronisation story

**What each layer offers.**

* *Fences.* A GPU-side fence is a driver object. Under the AMD path it is
  `DRM_IOCTL_AMDGPU_USERQ_WAIT` (wait for a queue to drain) or
  `DRM_IOCTL_AMDGPU_GEM_WAIT_IDLE` (wait for a buffer to be idle), or
  `DRM_IOCTL_AMDGPU_WAIT_CS` (wait for a specific command stream).
* *Generic DRM sync objects.* `DRM_IOCTL_SYNCOBJ_CREATE`, `_WAIT`,
  `_SIGNAL`, `_RESET`, `_TIMELINE_WAIT`, `_TIMELINE_SIGNAL`, `_QUERY`,
  `_EVENTFD`. These are public, generic, and work across drivers. The
  timeline syncobj is a 64-bit counter the GPU bumps and the CPU reads.
* *User fences (Intel).* `DRM_XE_WAIT_USER_FENCE` waits on a **value in
  memory**, with a mask and a comparison operator. This is exactly a
  GPU-visible atomic counter.
* *Semaphores.* Vulkan's timeline semaphores are a 64-bit counter in
  memory that a queue bumps; under the driver-native path the same role
  is played by a DRM timeline syncobj (AMD, Intel) or by the "global wave
  sync" domain (AMD, for inter-compute-unit barriers inside one dispatch).

**How they map onto the language's atomics (11e).** This is the good
news: the two models line up.

* A *GPU-visible atomic* is exactly a memory location the GPU writes
  without the CPU's help. 11e's atomic store/load/fetch-add, applied to
  device memory, is the right primitive. Intel's user fence is the
  canonical case: the kernel writes a value to an address, the CPU waits
  until the value satisfies a predicate.
* A *CPU-visible completion* is a kernel object, not a memory location.
  The GPU cannot write to ordinary process memory, so the completion has
  to arrive through the kernel: a DRM syncobj (a kernel object), an
  eventfd, or a sync file. The bridge into 11e's world is one hop:
  a thread (or the dispatcher) waits on the kernel object - by `poll`ing
  an eventfd, or by `DRM_IOCTL_SYNCOBJ_WAIT` - and then does an ordinary
  atomic store to a program-visible flag and a `futex` wake, which is what
  11e's `atomic_wait` would be built on.
* So 11e should grow two things the GPU path needs: an **atomic that can
  live in device memory** (same instruction, different address space), and
  an **atomic wait** that can be woken by a futex. Both are natural
  extensions of what 11e is already building, and both are needed by the
  CPU-only path too (a thread pool with a work-stealing queue needs
  exactly a futex-backed wait).

**Ordering rules the implementation must honour.** The subtle bugs in
GPU code are nearly always ordering bugs. The rules:

1. A dispatch is not complete until the fence says so; reading the output
   buffer before the fence is a race.
2. A buffer written by the GPU and read by the CPU needs a flush or a
   coherent mapping *and* the fence. The fence alone is not enough if the
   CPU's cache holds a stale line.
3. Two dispatches on the same queue are ordered; two on different queues
   are not, unless a semaphore says so.
4. The host must not touch a buffer that the GPU is reading.

### 8. The recommended path, end to end (Linux + AMD)

This is the concrete sequence for 11h.1. It is written so an implementer
does not have to guess.

**Step 1 - the syscall layer.** Add `ioctl` (and `openat`, `getdents64`,
`poll`) to `Syscalls` in `crates/nexum-runtime/src/syscalls.rs:37-91`,
with the Linux numbers per architecture and the Darwin number shared.
Emit an `nexum_sys_ioctl(fd, request, arg)` wrapper in the style of
`:237-292`: three `i64` arguments, the request number an immediate, the
third argument a pointer. Pin the numbers with a test in the style of
`:300-317`.

**Step 2 - the request-number table.** One table of
`(name, sub-number, struct size, direction)`, and a function that computes
`_IOWR('d', 0x40 + sub, size)`. The struct sizes must be derived by hand
from the C headers, and **the first tests must assert the computed
request numbers against the values the C headers produce** - otherwise a
padding mistake is a silent, fatal, "invalid argument" at run time.

**Step 3 - device discovery.** Enumerate `/dev/dri` with `getdents64` on
a directory fd, or read `/sys/class/drm/`. Open `renderD*` (not `card*`:
the render node is the one with compute). `DRM_IOCTL_VERSION` returns the
driver name (`amdgpu`, `i915`, `xe`, `nouveau`). `DRM_IOCTL_AMDGPU_INFO`
returns the hardware IP versions (`AMDGPU_INFO_HW_IP_INFO`), the VRAM size
(`AMDGPU_INFO_MEMORY`) and the GTT size (`AMDGPU_INFO_VRAM_GTT`) - enough
to pick an instruction set family, though the marketing name is not in the
current UAPI. That is the whole discovery step: no loader, no manifest,
no `dlopen`.

**Step 4 - memory.** `DRM_IOCTL_AMDGPU_GEM_CREATE` with
`gem_domain = AMDGPU_GEM_DOMAIN_VRAM` for device memory, `GTT` for
GPU-accessible system memory. `DRM_IOCTL_AMDGPU_GEM_MMAP` for a host
mapping. `DRM_IOCTL_AMDGPU_VM` to create a GPU address space and
`DRM_IOCTL_AMDGPU_GEM_VA` to bind a buffer into it. The
`drm_amdgpu_gem_mmap` struct is the one place where a CPU pointer to
device memory appears, and it is where the coherence rules from section 6
are decided.

**Step 5 - the queue.** `DRM_IOCTL_AMDGPU_USERQ` with
`AMDGPU_USERQ_OP_CREATE`, `DRM_AMDGPU_USERQ_CREATE_FLAGS_QUEUE_PRIORITY_*`,
and the MQD struct for the compute IP (`drm_amdgpu_userq_mqd_compute_gfx11`
in the current headers). The returned queue id is what the dispatch
refers to. A GPU context is allocated separately with
`DRM_IOCTL_AMDGPU_CTX` and `AMDGPU_CTX_OP_ALLOC_CTX`, and its id is the
`ctx_id` the submission carries.

**Step 6 - the shader.** At build time: compile the kernel to AMDGCN ISA,
embed the code object. At run time: create a GEM buffer, copy the code
object into it, bind it into the VM. The code object is a dispatch-ready
blob: the instruction stream plus the 64-byte kernel descriptor with the
grid size, the workgroup size, the shared-memory size and the register
counts, plus the dispatch packet in the command stream. **This is the part
I could not verify from the kernel headers** - the kernel descriptor and
dispatch-packet layout is in the RDNA ISA manual and in Mesa's `radv`
kernel setup, not in the UAPI. It is a known, bounded unknown, and it is
exactly what the 11h.0 spike exists to pin down.

**Step 7 - the dispatch.** `DRM_IOCTL_AMDGPU_CS` with a
`drm_amdgpu_cs_chunk_ib` (the indirect buffer: `va_start`, `ib_bytes`,
`ip_type = AMDGPU_HW_IP_COMPUTE`, `ip_instance`) and a
`drm_amdgpu_bo_list` chunk listing the buffers the kernel reads and writes.
The chunk framing is `drm_amdgpu_cs_chunk` (`chunk_id`, `length_dw` in
dwords, `chunk_data`); the driver also understands `dep`, `fence`, `sem`
and `syncobj` chunks, which is how a submission can wait on a previous
one.

**Step 8 - the fence.** `DRM_IOCTL_AMDGPU_USERQ_WAIT` to wait for the
queue, or `DRM_IOCTL_AMDGPU_GEM_WAIT_IDLE` to wait for the output buffer,
or a DRM syncobj for the general case.

**Step 9 - the library surface.** A Nexum module (`gpu.nexum`) over the
runtime, with the `kernel` declaration form, the grid builtins, and the
device/queue/buffer/dispatch types.

**What the 11h.0 spike looks like.** The same nine steps, with one
difference: steps 4-8 are done in Rust, inside the toolchain. Step 2 (the
kernel) is *not* done in Rust - the spike takes a real Nexum `kernel`
function, the Nexum compiler emits it as LLVM IR with the GPU calling
convention, `clang` compiles that IR to AMDGCN ISA, and the Rust tool
uploads and dispatches the resulting code object. So the spike validates
the *language* surface and the *shader* path for real, and the protocol
work it does is the same protocol work 11h.1 has to do - the difference is
that in 11h.1 it is written in the runtime's IR-emitting style instead of
in Rust.

Success criteria: a `f32` `axpy` and a `f32` reduction run on the GPU and
match the CPU to within `f32` tolerance; a buffer round-trips correctly;
the fence ordering holds over 1,000 iterations; the GPU kernel is
measurably faster than the CPU kernel on a large enough problem.

**The spike needs an AMD GPU.** If nobody on the programme has one, the
spike is blocked on hardware, and that is a procurement question the
orchestrator has to answer before 11h.0 can start - not a detail that
surfaces in week three.

### 9. The CPU-first alternative

This is the option that makes 11i possible now, so it deserves the same
rigour as the GPU path.

**What it needs.**

1. **The f32 tower (11c, D015).** `f32` in the language, with the
   arithmetic semantics spelled out. The risk is not the arithmetic - LLVM
   has `float` and x86-64 has SSE2, so `f32` arithmetic is native and is
   *not* silently promoted to `f64` (that is an x87 problem, and we are
   64-bit). The risk is in the *library*: every `Float` operation today
   is `f64`, and the conversion points are where a framework silently
   loses precision.
2. **The math builtins, ours, not libm (11c).** `sqrt` is free and exact
   (`sqrtss` / `fsqrt`). `exp`, `log`, `tanh`, `erf`, `pow`, `sin`, `cos`
   are not: each needs a range reduction plus a polynomial, in `f32` and
   in `f64`. The performance target matters more than the accuracy
   target. A careful `f32` `exp` is about 1-2 ns scalar; a vectorised one
   (8 lanes of AVX2) is about 0.3-0.5 ns per element. A naive
   implementation at 20-50 ns will make a transformer unrunnable, because
   the activation functions are applied to every element of every layer.
   **This is the single biggest CPU-side risk in 11i** and it should be
   treated as a first-class deliverable of 11c, with a benchmark per
   function, not as a tail of the phase.
3. **The vector support (11f).** Eight `f32` lanes on AVX2, sixteen on
   AVX-512, four on NEON. With a *layout* story: a `f32` tensor is a
   contiguous run of `f32`, and a vector load is an aligned load, so
   vectorising a loop is a matter of the compiler knowing the alignment.
4. **The threads (11e).** A thread pool for the outer loop of a matrix
   multiply, with 11e's atomics as the synchronisation primitive.

**What a competitive CPU implementation looks like.** A `f32` GEMM written
as: pack A and B into panels, then a register-tiled micro-kernel
(8x8 output tile, 8-wide FMA, 4 accumulators per row) walking the panels.
That reaches roughly 70-90% of the core's FMA peak. On a 3 GHz core with
two 256-bit FMA units, that is about 90-100 GFLOP/s per core; with eight
cores, 600-800 GFLOP/s. For comparison, an RTX 4090 is about 82 TFLOP/s
`f32` and a 7900 XTX about 61 TFLOP/s: the CPU is 1-2% of a discrete
GPU. An Apple M4 Max CPU is about 5 TFLOP/s against about 34 TFLOP/s for
its GPU - a ratio of about 1:7, and with unified memory on top. *These
peak numbers are vendor-published, and I did not measure any of them
here; the point of the paragraph is the order of magnitude, not the
digits.*

**Where the CPU is genuinely competitive, and where it is not.**

* *Small models, small batches, interactive inference.* For a 1B-parameter
  model in `f32`, a token costs 4 GB of weight traffic. At 50 GB/s of
  desktop memory bandwidth that is 80 ms per token on the CPU, against
  about 4 ms on a 4090. Twenty times slower, and still interactive. For a
  7B model it is 28 GB, 560 ms on the CPU against 28 ms on the GPU -
  twenty times slower and no longer comfortable.
* *Training.* A small model trains on a CPU in hours rather than minutes.
  That is a completely different research velocity, and it is the reason
  the GPU matters.
* *The framework's own development.* Every correctness property of an
  autodiff engine - gradients, shapes, dtypes, optimisers, checkpointing -
  is testable on the CPU, and testing it on the CPU is 20x cheaper.

**So the honest summary of the CPU path: it is enough to land 11i, and it
is not enough to make 11i mean "full scale" in the training sense.** That
is the argument for doing the GPU work, and the argument for doing it in
the right order: the framework first (so there is something to accelerate),
the GPU second.

### 10. What the GPU path needs to have been proven

The recommendation is "the CPU path first, the GPU after", and that is only
honest if the GPU path has a *gate* rather than a date. The gate is seven
properties, all of which the 11h.0 spike can establish and all of which
the CPU path does not depend on. If they are met by the time 11i starts,
11h.1 is ordinary engineering. If they are not, 11h is a research project
and the programme should say so instead of carrying it.

1. **A device is found and identified without a C library.** The
   enumeration, the open, and the driver-name handshake work, and the
   program's dependency list shows no C library.
2. **A buffer round-trips.** Host writes, device reads, device writes,
   host reads - the values come back exactly, and the test says which
   coherency option was used.
3. **One kernel runs and is numerically correct.** An `f32` `axpy` and an
   `f32` reduction, compared against the CPU implementation to within
   `f32` tolerance, not to within "roughly right".
4. **The build-time shader path works and is reproducible.** A `kernel`
   function in a Nexum file becomes a device code object, on a machine
   with no GPU present, with the same bytes every time. A build that only
   works on the developer's laptop is not a build path.
5. **The synchronisation is correct under repetition.** The same kernel
   run a thousand times gives the same answer every time, with the fence
   respected. This is the test that catches the coherency and ordering
   bugs that are otherwise silent.
6. **The language surface is the one the framework wants.** The kernel
   declaration, the grid builtins and the device-pointer types are
   something 11i can actually build a tensor library on - not a
   one-off that the framework has to be bent around.
7. **The throughput is worth it.** On a problem large enough to matter,
   the GPU kernel is at least 3-5x the CPU kernel. If it is not, the
   entire driver path buys nothing that a better CPU kernel does not, and
   the honest conclusion is that the GPU is not what 11i needs.

Property 7 is the one that decides the phase. Properties 1-6 can all be
met and the phase can still be a mistake.

### 11. The cost table

Estimates in engineer-weeks, for a first working kernel, by a developer
who knows the platform. They are estimates, not measurements.

| # | Option | Work to a first working kernel | Notes |
| - | ------ | ------------------------------ | ----- |
| 1 | **CPU path** (11c + 11e + 11f as planned) | already budgeted; the *math builtins* are the risk | Do it first, regardless |
| 2 | **11h.0 spike** (a Rust tool, raw ioctls, real Nexum kernel) | **1-2 weeks** | Validates the design *and* the protocol; a tool, so outside the no-C rule |
| 3 | **Linux + AMD, driver-native** (11h.1) | **8-12 weeks** | One vendor, one GPU family, one kernel shape |
| 4 | Linux + Intel (xe UAPI + GT ISA) | 3-6 months | The instruction set is the blocker |
| 5 | macOS + Metal, source-level reading | 1-2 weeks | Only if "no C" is relaxed |
| 6 | macOS + Metal, binary-level reading | **impossible** | No protocol under the ObjC API |
| 7 | NVIDIA, any reading | 6-18 months | Reverse engineering the closed user space |
| 8 | Vulkan spoken "directly" (a2) | 12-20 weeks, and it still needs (4) | A mini-Vulkan, on top of a vendor path |
| 9 | Nexum-to-SPIR-V front end | 15-30 weeks | And it produces something nothing can consume |

Options 3 and 8 are the same work with different amounts of extra
ceremony: option 8 is option 3 plus a Vulkan-shaped API that the hardware
does not need.

## Risks

* **The premise of 11h is wrong in a way that has been believed for a
  while.** `docs/native-roadmap.md:76` says "SPIR-V/Vulkan on the common
  case, Metal on Apple silicon". Neither of those is reachable under the
  programme's own rules, for the reasons in sections 2 and 3. If the
  programme has been *counting on* a Vulkan path, that expectation is
  wrong, and the sooner it is corrected the cheaper it is. **This is the
  single most important finding in this document.**
* **"No C binding" is ambiguous, and the ambiguity is worth a factor of
  ten.** Under the source-level reading, macOS and every vendor open up in
  a fortnight. Under the binary-level reading, macOS closes and only AMD
  and Intel remain, at a cost of 8-12 weeks each. **The recommendation is
  wrong if the orchestrator picks the source-level reading** - and it would
  be *right for a different reason*, because the answer would then be
  "call `vkCreateShaderModule` and be done". This is why it is
  recommendation 3 and not a footnote.
* **The instruction set is the long pole, not the ioctls.** The ioctl
  protocol is public, stable and mechanical: it is a weekend of struct
  layouts and a request-number table. The instruction set is where the
  unknowns are - the kernel descriptor, the dispatch packet, the wavefront
  size, the LDS, the global wave sync. These are documented for AMD's
  RDNA and reverse-engineered for Intel's Xe. If the first target turns
  out to be Intel, the estimate triples.
* **The build-time shader path is an assumption about the toolchain.** It
  assumes the installed `clang` has the AMDGPU target and that LLVM's
  AMDGPU backend emits a code object with the layout the driver wants. The
  first is checkable in a minute (`clang --print-targets`); the second is
  what the 11h.0 spike is for. **A "loaderless" path that needs a
  toolchain-side shader compiler is not loaderless** - it has moved the
  dependency from the program to the build, which is legitimate under the
  rules but should be stated plainly rather than discovered later.
* **Coherency bugs are silent and late.** They show up as a model that
  trains to a slightly wrong loss and nobody knows why. The mitigation is
  to start with the slowest correct option (coherent mapping or explicit
  flush) and to write a test that runs the same kernel a thousand times
  and checks the result, not just once.
* **The framework and the driver are on the critical path together.**
  11i needs a tensor type, a device abstraction and a synchronisation
  model. If the GPU path lands first, the framework is designed around
  one vendor's quirks. If the framework lands first on the CPU, the
  device abstraction is designed against a real need. **The CPU-first
  order is not just cheaper; it produces a better abstraction.**
* **The CPU path's real risk is the math builtins, not the vectors.** A
  framework with a slow `exp` or `tanh` is unusable, and 11c is where
  that is decided. If 11c's math builtins are written as an afterthought,
  11i is blocked and no GPU work helps.
* **The Windows and macOS gaps will be noticed.** A programme whose GPU
  path works on Linux and not on the user's Mac is a programme that
  excludes a lot of people. The mitigations are (i) the source-level
  reading, which reopens macOS cheaply, and (ii) the CPU path, which is
  genuinely good on Apple silicon. Both are better than pretending the
  strict reading has no cost.
* **The spike needs hardware, and hardware procurement is slow.** 11h.0
  cannot start without an AMD GPU on somebody's desk. If the programme has
  none, 11h.0 is blocked on a purchase order, and the whole phase slides.
  This is worth checking before 11h is scheduled rather than during it.
* **This document's own numbers are estimates.** The ioctl interfaces, the
  struct layouts, the request-number encoding and the system call numbers
  are verified against the kernel's and XNU's sources. The *costs* - the
  engineer-week figures, the line counts for a SPIR-V assembler, the
  performance targets - are estimates by someone who has not built any of
  them. They are the right order of magnitude and they are the basis of a
  ranking, not of a schedule. The 11h.0 spike is what turns them into
  numbers.

## Open questions

1. **What does "no C binding" mean: source-level or binary-level?**
   *Recommendation*: binary-level - "the program links no C library; it
   reaches the GPU through the kernel's ioctl protocol" - because it is
   what `docs/native-roadmap.md:10-12` says and because it is the only
   reading under which the GPU path is interesting. Record it as D017.
   *If the answer is source-level*: 11h collapses to "call the Vulkan
   loader, or a thin Metal shim", the whole path is a fortnight, and
   recommendation 4 (Linux/AMD only) is wrong.
2. **Is a toolchain-side spike allowed?** 11h.0's tool is a *tool*, like
   the compiler, so `docs/native-roadmap.md:107-109` ("no GPU driver in
   the compiler") applies to the shipped product, not to a development
   tool. *Recommendation*: yes, and it needs no exception at all - the
   spike can use raw syscalls in Rust and avoid the C ABI completely,
   which is also what proves the protocol is speakable without one. The
   thing that does need a decision is the *end state*: the shipped
   runtime path must not grow a dependency on the spike's Rust crate.
3. **Which vendor is first?** *Recommendation*: AMD on Linux, RDNA
   (gfx10/gfx11/gfx12), because it is the only vendor where the interface
   is public, the instruction set is documented, and the toolchain's LLVM
   already targets it. Intel second, NVIDIA not at all. The orchestrator
   should also consider whether the *first* vendor should be the one the
   programme's own users have - if most of them are on Apple silicon, the
   answer is "no GPU path until the reading is relaxed", and that is a
   legitimate outcome.
4. **Does the GPU library have to be written in Nexum?** The runtime is
   emitted as LLVM IR text from a Rust crate
   (`crates/nexum-runtime/src/lib.rs:1-5`), so a GPU library built the
   same way is Nexum's *output* rather than Nexum's *source*, and it
   would be very hard to write the ioctl protocol in a language that
   cannot yet express pointers (11d) or f32 (11c).
   *Recommendation*: write the GPU library in Rust, emitting IR text,
   exactly like the runtime, and keep the *user-facing* API in Nexum. If
   the programme insists the library be written in Nexum, then 11d has to
   land first and the shape has to be a Nexum module - which is a
   materially different and larger piece of work.
5. **What is the target's name for the `f32` type, and for a device
   pointer?** `F32` or `Float32`? `GpuMem(F32)`, `DevicePtr(F32)`, or
   something in 11d's pointer model? *Recommendation*: 11d owns the
   pointer model; the GPU path should be written against whatever 11d
   lands, and this document's syntax is illustrative, not a proposal to
   be entered in the grammar.
6. **Is the `kernel` declaration the right form?** The alternative is a
   `fn` with a marker (`fn kernel saxpy(...)`), which keeps the grammar's
   top-level forms to two. *Recommendation*: a separate `kernel` form,
   because the kernel is compiled differently, has different parameter
   rules, and may not be called from the host - which is exactly the kind
   of difference a separate form expresses better than a flag.
7. **Should 11h be a phase at all, or a spike that might end the
   programme's GPU work?** *Recommendation*: keep 11h, but let the 11h.0
   spike be allowed to conclude "the GPU path is not worth it". The
   programme has already written down that a driver-native GPU path is
   "a large project on its own" (`docs/native-roadmap.md:118-120`); the
   spike is where that sentence is tested.
8. **What would make 11i "full scale"?** The phase table says 11i is the
   ML framework "now at full scale"
   (`docs/native-roadmap.md:77`). If "full scale" means a framework that
   *runs* real models, the CPU path is enough. If it means training a
   frontier model, the CPU path is not enough and 11h cannot be
   optional. *Recommendation*: the orchestrator should write down which,
   because it decides whether 11h is on the critical path.
