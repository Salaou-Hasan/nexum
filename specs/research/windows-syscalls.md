# Reaching the Windows kernel without the C library

**Question.** How does the Windows half of the runtime stop using the C
library (phase 11a.2), so that a Nexum program on Windows depends on the
kernel and nothing else?

**Summary.** The recommendation is to generate our own system call stubs:
the runtime reads the syscall numbers out of `ntdll.dll`'s own stub bytes
at start-up and executes the `syscall` instruction itself, with the
arguments placed the way the Windows x64 kernel expects them. That is not
a C binding and not a C library - `ntdll.dll` is the operating system's
own native interface, and the transition is our own inline assembly, so it
satisfies D012 and D013 as they are written. The parts that are genuinely
hard are not the transition (that is verified working) but the argument
placement, which the compiler actively works against, and the fact that
the native API takes structures where the POSIX calls take integers. The
C library therefore stays on Windows for the duration of 11a.2; the first
step towards removing it is the system call layer plus the allocator, and
both can be built without touching the file routines.

## Recommendation

1. **Take route (b): our own stubs, generated at start-up from `ntdll`'s
   own bytes - but without a code generator.** The stub is a fixed block of
   inline assembly; the syscall number is a global that a resolver fills
   in at start-up. That removes the need to write a system call stub
   generator (a piece of self-modifying code and its own set of hazards)
   while keeping the transition inside the runtime, where D013 wants it.
   Reason: the syscall numbers are not documented and differ between
   Windows builds, so they must be read from the running system's own
   `ntdll` rather than compiled in.

2. **Resolve `ntdll` by walking the loader list from the PEB and parsing
   its export directory - about a hundred and fifty lines of IR, and not
   a PE loader.** A PE loader means loading a module from disk (sections,
   relocations, imports, TLS); we do not need any of that, because
   `ntdll.dll` is already mapped in every process and we only need two
   things from it: the address of the `Nt*` entry points and the syscall
   number in each entry point's bytes. Reason: the alternative - having
   the linker put `Nt*` in the import table - makes the native linker
   (phase 11g) responsible for emitting import descriptors, and it still
   leaves the runtime unable to read the syscall numbers out of the stub.

3. **Accept the dependency on `ntdll.dll`, and say so in the documents.**
   `ntdll.dll` is the operating system's native interface, not a C
   library: it implements no C semantics, it exposes the kernel's own
   entry points. The C library is `ucrt` (stdio, `malloc`, the CRT
   startup); removing it is what "no C library" means, and that is
   reachable. Removing `ntdll` is not reachable at all, because the
   syscall numbers live nowhere else. Reason: the honest reading of D012
   ("a program depends on the kernel and on nothing else") is that
   `ntdll` *is* the kernel's user-mode half on Windows - there is no
   other way in - and D013's rule against "a C binding" is about not
   binding to a C library's API, which we are not doing.

4. **The stub shape: the Microsoft x64 calling convention, and the stub
   must not touch the stack pointer across the transition.** The first
   four arguments go in `r10`, `rdx`, `r8`, `r9` (the kernel's order, not
   the C order: `r10` rather than `rcx`), the syscall number in `rax`,
   and the arguments from the fifth on are read by the kernel from the
   user stack, in the slots the calling convention reserves for them.
   Reason: the kernel reads them from the stack at the transition, so any
   stack the stub creates moves the place the kernel looks in. This is
   the one part of the design that is not yet pinned down by measurement
   (see *Risks*), and it must be settled with a measurement harness
   before anything is built on it.

5. **Change the wrapper's shape on Windows: the wrapper takes the
   arguments, not integers.** On POSIX the wrapper is
   `nexum_sys_read(i64 fd, i64 buf, i64 len)` - everything is an integer
   and the pointers are cast. The Windows calls take structures: a
   Unicode path, an access mask, an `OBJECT_ATTRIBUTES`, an
   `IO_STATUS_BLOCK`, a region size and a protection type. The runtime
   therefore has to build those structures, and the wrapper has to take
   pointers to them. Reason: this is the real difference in difficulty
   between the two platforms - not the instruction, but the argument
   shape.

6. **The heap comes from `NtAllocateVirtualMemory` and
   `NtFreeVirtualMemory`, and the pages have to be committed and
   released.** No `VirtualAlloc`, no `malloc`: the allocator grows with
   `MEM_COMMIT | MEM_RESERVE` and `PAGE_READWRITE` and releases with
   `MEM_RELEASE`, which requires the base address that the allocation
   returned. Reason: without the C library there is no `malloc`, and
   `NtAllocateVirtualMemory` is the only way to get pages.

7. **The order of the work, and what does not wait for the allocator.**
   The resolver (find `ntdll`, find the exports, read the syscall
   numbers) and the stub templates can be built first and are independent
   of everything else: they need no memory of their own. Then the
   `Nt*` wrappers, which need no heap either (the arguments are the
   caller's structures). Then the allocator, which needs the wrappers.
   Then the file routines, which need the allocator (they allocate the
   buffer they read into). Then the output formatting, which needs the
   allocator and the console handles. The `printf` replacement and the
   process exit come last. Reason: the resolver and the wrappers are the
   part that is independent of the allocator, and they are also the part
   that is independent of the argument-layout question - they can be
   written and reviewed while that question is being measured.

8. **C scope the first slice tightly: the file routines and the process
   exit, not the console.** `printf` replacement needs the console
   handles, and on Windows those come from the process parameters (the
   PEB), not from the C library's `stdout` - another structure to build,
   another argument to get right. Reason: the file path is the part the
   POSIX runtime already has, and the console is the part that is new
   work for the Windows build; keeping them apart keeps the first slice
   small enough to land.

## What the code looks like today

* `crates/nexum-runtime/src/syscalls.rs:23` - the operating systems the
  layer covers: `Linux` and `Macos`. There is no `Windows`, and the
  numbers table (`syscalls.rs:37`, filled in by `Syscalls::for_os` at
  `syscalls.rs:56`) has no Windows column.
* `crates/nexum-runtime/src/syscalls.rs:110` - `layout()` gives the
  register sets and the instruction per system and architecture. The
  Windows entry does not exist: there is no instruction to name.
* `crates/nexum-runtime/src/syscalls.rs:163` - `call_body()` builds the
  inline assembly: the number is an immediate in the register the
  convention names, the arguments are pinned to their registers by the
  constraint string, and the stack is aligned for the call
  (`syscalls.rs:174`). On Windows the alignment dance would be replaced
  by the opposite requirement: the stack must *not* move.
* `crates/nexum-runtime/src/syscalls.rs:233` - `wrappers()` emits one
  function per call, all of them `i64` in and `i64` out
  (`syscalls.rs:241`). Every Windows call would need a different shape.
* `crates/nexum-runtime/src/lib.rs:60` - the Windows branch of
  `runtime_ir()`: it declares `fopen`, `fclose`, `fseek`, `ftell`,
  `fread`, `fwrite` and `__acrt_iob_func` (`lib.rs:63-73`) and uses
  `strings::file_routines(false, 0, 0)` (`lib.rs:99`). The comment at
  `lib.rs:65` already says what this document is about: "Windows has no
  direct system call instruction: its kernel interface lives behind
  ntdll. The file routines still go through the C library until the
  system call stubs exist (phase 11a.2)."
* `crates/nexum-runtime/src/lib.rs:49` - the C library declarations that
  the whole runtime still leans on: `printf`, `fputs`, `fputc`, `exit`,
  `malloc`, `free`. The collector allocates through `malloc`
  (`lib.rs:147`) and frees through `free` (`lib.rs:186`).
* `crates/nexum-runtime/src/strings.rs:11` - the string routines are
  already ours on every platform, including Windows. That part of D012 is
  done.
* `crates/nexum-runtime/src/strings.rs:33` - `file_routines()` takes a
  `posix` flag; the Windows half (`strings.rs:46`) is the C library
  version.
* `docs/runtime-abi.md:19` - the table of what still comes from the C
  library and when it goes: `malloc`/`free`, `printf`/`fputs`/`fputc`,
  `exit` and "the file routines on Windows" are all marked 11a.2, and
  `runtime-abi.md:30` says the Windows build "needs its own system call
  stubs: its kernel interface lives behind `ntdll`, so the stubs have to
  be generated rather than assembled".
* `docs/language-reference/decisions.md:178` (D012) and `:198` (D013) -
  the rules this document has to satisfy.
* `docs/native-roadmap.md:98` - the roadmap's own statement: "Windows
  has no direct system call instruction, and its kernel interface lives
  behind `ntdll`. Reaching it without a C binding means generating our
  own system call stubs - a real piece of work."
* `crates/nexum-target/src/lib.rs:41` - the Windows target is
  `x86_64-pc-windows-msvc`, x86-64 only, and `crates/nexum-target/src/lib.rs:87`
  gives it the COFF data layout.
* `crates/nexum-toolchain/src/lib.rs:221` - `verify_ir_for()` compiles
  the module for the *target* rather than the host, because a module with
  foreign assembly cannot be checked by the host assembler. The Windows
  assembly will be checked the same way.

## The design

### Why Windows is different

On Linux and macOS the kernel entry is an instruction: `syscall` on
x86-64, `svc #0` on AArch64. The number goes in a register, the arguments
go in registers, the result comes back in a register, and that is the
whole interface. There is no library between the program and the kernel,
which is why the POSIX runtime can be freestanding.

On Windows there is no such instruction in user mode. The kernel's
interface is exposed by the operating system as `ntdll.dll`, and the
entry points are the `Nt*` and `Zw*` functions. `ntdll` is not a C
library - it is the native interface to the kernel, and it is loaded into
every process by the loader - but it *is* a module with an export table
and a C-shaped calling convention, so reaching the kernel "without a C
binding" needs one extra step: the program has to make the transition
itself.

### The three routes

**(a) Read `ntdll`'s export table at run time and call the `Nt*` entry
points directly.** The runtime finds `ntdll` (from the PEB's loader
list), parses its export directory, and calls the entry point as an
ordinary function call. Cost: one export-table parser, one indirect call
per system call. This *is* a C ABI - the Microsoft x64 calling
convention - but it is the ABI of the kernel's own native interface, not
of a C library: no C semantics are involved and no C library is linked.
Under a strict reading of D013 ("reached through inline assembly, never
through a C binding") this route is a C binding and is not acceptable;
under the reading D012 actually cares about ("the program depends on the
kernel and on nothing else") it is acceptable, because `ntdll` is the
kernel's own module.

**(b) Generate the system call stubs by reading the instruction bytes
out of `ntdll`.** The stub in `ntdll` is not a function; it is four
instructions that make the transition. Read those bytes, take the
syscall number out of them, and emit a stub of our own that executes
`syscall` with the number we read. This is what Linux and macOS would
require if they had no instruction: the program would have to carry the
transition itself. Cost: the export parser (shared with (a)) plus the
byte-level extraction. This is the route D013 asks for, and it is the
recommendation.

**(c) Keep the C library on Windows and say so.** Cost: nothing, and the
program keeps the seventeen external symbols it has now. This is where
the Windows build starts and where it should stay until (b) works.

The honest reading of the rule is this. D012's rule is about *the C
library*: the ISO C and POSIX semantics implemented by `ucrt`. `ntdll`
implements none of those. D013's rule is about *how the operating system
is reached*: through inline assembly in the generated IR, so that the
register allocation, the calling convention and the error convention are
all visible in the runtime. Route (b) satisfies both: the program links
no C library, and the transition is our own assembly. Route (a)
satisfies the first and breaks the second in form, even though its
substance - the kernel's own ABI - is arguably the same thing. Route (c)
satisfies neither, and is where we are.

### What the stub looks like

The `ntdll` stub for an `Nt*` function is twenty-one bytes and has been
the same shape for years. Read out of this machine's
`C:\Windows\System32\ntdll.dll` (version 10.0.26100.9549):

```
4c 8b d1                  mov  r10, rcx          ; the first argument moves out of rcx
b8 55 00 00 00            mov  eax, 55h         ; the syscall number
f6 04 25 08 03 fe 7f 01   test byte ptr [0x7FFE0308], 1
75 03                     jne  <the 32-bit path>
0f 05                     syscall
c3                        ret
cd 2e                     int  2eh             ; the 32-bit path, for a 32-bit process
c3                        ret
```

Three things follow from that, and all three are verified:

* The kernel's argument order is `r10`, `rdx`, `r8`, `r9` - not the C
  order. `rcx` is destroyed by the `syscall` instruction, which is why
  the first argument is moved to `r10` first. The result comes back in
  `rax`.
* The syscall number is at a fixed place in the stub: the four-byte
  immediate after `b8`. That is where the resolver reads it from. The
  numbers differ between Windows builds, so they must be read, not
  compiled in.
* The `test` of `SharedUserData + 0x308` selects the 32-bit transition
  for a process running under WOW64. We are a 64-bit process, so our
  stub can execute `syscall` unconditionally.

The transition itself is verified working. A stub written as inline
assembly in the generated IR, with the number loaded from a global and
nothing else - no C library in the path - ended the process with the
requested exit code through `NtTerminateProcess`, and allocated a page
through `NtAllocateVirtualMemory` and wrote to it and read it back.

So the stub is, in the shape the existing layer already uses:

```
define internal i64 @nexum_sys_ntclose(ptr %args) noinline {
entry:
  %r = call i64 asm sideeffect
      "movq %rcx, %r10\0Amovl nexum_ssn_ntclose(%rip), %eax\0Asyscall",
      "={rax},{rcx},~{r10},~{r11},~{memory}"(ptr %args)
  ret i64 %r
}
```

Note what is *not* in it, compared with `call_body()`
(`crates/nexum-runtime/src/syscalls.rs:163`): the stack alignment. On
Windows the stack must not move across the transition at all, so the
`mov %rsp, %r12; and $-16, %rsp` dance has no counterpart - and its
absence is the point.

### The Windows calling convention, and the arguments from the fifth on

The Microsoft x64 convention: the first four integer arguments go in
`rcx`, `rdx`, `r8`, `r9`; the caller always reserves 32 bytes of shadow
space for them; arguments five and up are passed on the stack, at
`[rsp + 0x20]` and up at the call.

That is visible in LLVM's own lowering: a function with eleven integer
arguments reads the seventh and the eleventh at `88(%rsp)` and
`40(%rsp)` respectively, counted from its own entry, which is the
caller's `rsp` plus eight. It is also visible in Windows' own code:
`KernelBase!VirtualAlloc` sets up its call to `NtAllocateVirtualMemory`
with `AllocationType` at `[rsp+0x20]` and `Protect` at `[rsp+0x28]`.

The kernel reads the fifth and later arguments from the user stack, in
the slots the convention reserves for them - that is forced, because
`ntdll`'s own stubs are ordinary functions called by ordinary callers,
so the arguments are wherever the convention puts them. Our own stub
must therefore leave the stack alone across the transition, and the
caller must put the arguments where the convention says.

**The compiler is the obstacle here, not the kernel.** Three separate
optimisations get in the way, all of them observed in the disassembly:

* *Dead argument elimination.* The compiler drops call arguments the
  callee does not use. A wrapper whose assembly mentions only the first
  four arguments is a function with four parameters as far as the
  optimiser is concerned, and the caller stops passing the rest.
* *Function specialisation.* When the arguments are constants at the
  call site, the compiler clones the wrapper with the constants folded
  in, and the clone has fewer parameters. The wrapper then never sees
  the stack arguments, and the caller never puts them there.
* *Call slot reuse.* The outgoing-argument area of the caller is
  scratch space. Once the wrapper's stack arguments are dead, the
  compiler reuses those slots for other values - in the experiment, for
  the initialisation of the `UNICODE_STRING` - and the kernel then reads
  a path where an access mask should be.

The consequence for the design: the arguments must reach the kernel
through a route the optimiser cannot see as dead. Two ways are open: the
wrapper reads them from the caller's outgoing-argument area itself (the
caller must reserve it, which the calling convention does when the
wrapper's signature has that many arguments), or the runtime writes the
argument block itself and the wrapper copies it into place. The second
is under our control and the first is not; the measurement harness below
has to decide between them.

### The calls, and the structures they need

These are the native calls the runtime needs, with their arguments as
the Windows kernel expects them. The `Nt*` prototypes are the ones the
Windows Driver Kit documents; note the ones that do not match what one
would guess.

| What the runtime needs | The native call | Arguments |
| --- | --- | --- |
| open a file | `NtCreateFile` | `PHANDLE FileHandle`, `ACCESS_MASK DesiredAccess`, `POBJECT_ATTRIBUTES ObjectAttributes`, `PIO_STATUS_BLOCK IoStatusBlock`, `PLARGE_INTEGER AllocationSize`, `ULONG FileAttributes`, `ULONG ShareAccess`, `ULONG CreateDisposition`, `ULONG CreateOptions`, `PVOID EaBuffer`, `ULONG EaLength` (11; seven of them on the stack) |
| read a file | `NtReadFile` | `HANDLE FileHandle`, `PVOID IoBuffer`, `ULONG Length`, `PLARGE_INTEGER ByteOffset`, `PULONG Key`, `PIO_STATUS_BLOCK IoStatusBlock`, `PVOID ApcContext`, `PVOID ApcRoutine` (8) |
| write a file | `NtWriteFile` | the same shape as `NtReadFile` (8) |
| the size of a file | `NtQueryInformationFile` | `HANDLE FileHandle`, `PIO_STATUS_BLOCK IoStatusBlock`, `PVOID FileInformation`, `ULONG Length`, `FILE_INFORMATION_CLASS FileInformationClass` (5) |
| close | `NtClose` | `HANDLE Handle` (1) |
| map memory | `NtAllocateVirtualMemory` | `HANDLE ProcessHandle`, `PVOID *BaseAddress`, `ULONG_PTR ZeroBits`, `PSIZE_T RegionSize`, `ULONG AllocationType`, `ULONG Protect` (6) |
| unmap memory | `NtFreeVirtualMemory` | `HANDLE ProcessHandle`, `PVOID *BaseAddress`, `PSIZE_T RegionSize`, `ULONG FreeType` (4) |
| exit | `NtTerminateProcess` | `HANDLE ProcessHandle`, `NTSTATUS ExitStatus` (2) |

The structures, and their sizes on x86-64:

* `UNICODE_STRING` (16 bytes): `USHORT Length`, `USHORT MaximumLength`,
  `PWSTR Buffer`. The length is in **bytes**, and the buffer is UTF-16.
  Nexum's strings are byte strings, so the runtime needs a UTF-8 to
  UTF-16 conversion for paths - an ASCII fast path is enough for now.
* `OBJECT_ATTRIBUTES` (48 bytes): `ULONG Length`, `HANDLE RootDirectory`,
  `PUNICODE_STRING ObjectName`, `ULONG Attributes`, `PVOID
  SecurityDescriptor`, `PVOID SecurityQualityOfService`. `Length` is
  48, `RootDirectory` is null, `Attributes` is `OBJ_CASE_INSENSITIVE`
  (0x40) for a path.
* `IO_STATUS_BLOCK` (16 bytes): the status and the information. The
  runtime mostly wants the information word - the number of bytes read.
* `FILE_STANDARD_INFORMATION` (32 bytes): `LARGE_INTEGER AllocationSize`,
  `LARGE_INTEGER EndOfFile`, `LARGE_INTEGER NumberOfLinks`, `BOOLEAN
  DeletePending`, `BOOLEAN Directory`. `EndOfFile` at offset 8 is the
  size of the file; the class is `FileStandardInformation` (1).
* `LARGE_INTEGER` (8 bytes): a signed 64-bit number.

And the constants:

* Access: `FILE_READ_DATA` 0x1, `FILE_WRITE_DATA` 0x2,
  `FILE_APPEND_DATA` 0x4, `FILE_READ_EA` 0x8, `FILE_READ_ATTRIBUTES`
  0x80, `FILE_WRITE_ATTRIBUTES` 0x100, `DELETE` 0x10000, `SYNCHRONIZE`
  0x100000, `GENERIC_READ` 0x80000000, `GENERIC_WRITE` 0x40000000. The
  `FILE_GENERIC_*` masks (`FILE_GENERIC_READ` 0x120089,
  `FILE_GENERIC_WRITE` 0x120116) are the convenient ones, because they
  include `SYNCHRONIZE`, which a handle opened for synchronous I/O
  needs.
* Share: `FILE_SHARE_READ` 0x1, `FILE_SHARE_WRITE` 0x2,
  `FILE_SHARE_DELETE` 0x4.
* Disposition: `FILE_SUPERSEDE` 0, `FILE_OPEN` 1, `FILE_CREATE` 2,
  `FILE_OPEN_IF` 3, `FILE_OVERWRITE` 4, `FILE_OVERWRITE_IF` 5.
* Options: `FILE_DIRECTORY_FILE` 0x1, `FILE_WRITE_THROUGH` 0x2,
  `FILE_SEQUENTIAL_ONLY` 0x4, `FILE_SYNCHRONOUS_IO_ALERT` 0x10,
  `FILE_SYNCHRONOUS_IO_NONALERT` 0x20, `FILE_NON_DIRECTORY_FILE` 0x40.
* Attributes: `FILE_ATTRIBUTE_NORMAL` 0x80.
* Memory: `MEM_COMMIT` 0x1000, `MEM_RESERVE` 0x2000, `MEM_RELEASE`
  0x8000; `PAGE_READWRITE` 0x4, `PAGE_EXECUTE_READWRITE` 0x40.

**The difference in difficulty between POSIX and Windows is the argument
shape, not the instruction.** The POSIX wrappers
(`crates/nexum-runtime/src/syscalls.rs:233`) take three or four `i64`s
and the runtime casts its pointers to integers on the way in
(`crates/nexum-runtime/src/strings.rs:39`). The Windows wrappers take
pointers to structures that the runtime has to build - a Unicode string,
an object attributes block, an I/O status block - and the values inside
them are typed (an access mask, a protection type, a region size). So
either the wrapper's signature changes to take those structures, or the
runtime builds them and passes pointers; the first is what the rest of
this document assumes, because a wrapper that takes the structures can
be handed the caller's own structures without a copy.

### The allocation

Without the C library there is no `malloc`, so the heap is
`NtAllocateVirtualMemory`: reserve and commit in one go
(`MEM_COMMIT | MEM_RESERVE`, `0x3000`) with `PAGE_READWRITE` (4), and
release with `MEM_RELEASE` (0x8000), which has to be given the base
address the allocation returned and a zero size. The runtime's allocator
grows in regions, keeps a free list over the regions it has, and returns
memory to the kernel only when a whole region is free. Two details
matter: `MEM_RELEASE` only works on the address the allocation returned
(a sub-region cannot be released), and the size of a region is a
multiple of the 4 KiB page.

The allocator is the first thing that needs the system call layer, and
the file routines are the first thing that needs the allocator: reading a
whole file means allocating a buffer for it. So the allocator is the
gate.

### The order of the work

1. **The resolver** - find `ntdll` from the PEB's loader list, parse its
   export directory, read the syscall number out of each `Nt*` stub's
   bytes, and store it in a global. Needs no memory of its own: the
   loader list is walked in place, the export table is read in place,
   and the numbers go into static globals. Independent of the allocator,
   and independent of the argument-layout question.
2. **The stub templates** - one inline assembly block per call, generated
   by the same kind of table `Syscalls::for_os` already holds
   (`crates/nexum-runtime/src/syscalls.rs:56`), extended with the
   Windows entry: the register set (`r10`, `rdx`, `r8`, `r9`), the
   number in `rax`, and no stack adjustment.
3. **The argument layout harness** - a small program that calls
   `NtAllocateVirtualMemory` through the new stub in several shapes and
   reports which one the kernel accepts. This is the gate for
   everything below, and it is where the compiler's interference has to
   be measured away.
4. **The allocator** - regions over `NtAllocateVirtualMemory` and
   `NtFreeVirtualMemory`, replacing `malloc` and `free`
   (`crates/nexum-runtime/src/lib.rs:147`, `:186`).
5. **The file routines** - `nexum_read_file` and `nexum_write_file`
   (`crates/nexum-runtime/src/strings.rs:33`) over `NtCreateFile`,
   `NtReadFile`, `NtWriteFile`, `NtQueryInformationFile` and `NtClose`,
   replacing the Windows branch (`crates/nexum-runtime/src/strings.rs:46`).
6. **The console** - `printf`, `fputs` and `fputc` over
   `NtWriteFile` on the standard handles, which come from the process
   parameters in the PEB rather than from the C library's `stdout`.
7. **The exit and the entry point** - `NtTerminateProcess` instead of
   `exit` (`crates/nexum-runtime/src/lib.rs:124`), and a PE entry point
   of our own instead of the C runtime's `mainCRTStartup`: once `ucrt`
   is gone, the program has to start itself.

Steps 1 and 2 are independent of the allocator and of everything else;
they can be written and reviewed now. Step 3 depends on nothing but the
harness itself. Step 4 is the first thing that needs a working system
call layer.

### What the experiments found that is not obvious

These are all measured on this machine (Windows 11, build 26100) with
clang 23.1.3 targeting `x86_64-pc-windows-msvc`, and they are the
traps an implementer will hit first:

* **`NtAllocateVirtualMemory` takes six arguments, not five.** The third
  is `ZeroBits`, and the order is `ProcessHandle`, `BaseAddress`,
  `ZeroBits`, `RegionSize`, `AllocationType`, `Protect`. Calling it with
  the five-argument shape gives `STATUS_ACCESS_VIOLATION` (0xC0000005),
  because the kernel reads `AllocationType` where `RegionSize` should be.
  The same call with the six-argument shape succeeds and allocates a real
  page.
* **The native API wants an NT path, not a DOS path.** `"C:\file.txt"`
  gives `STATUS_OBJECT_NAME_INVALID` (0xC000003B); `"\\??\\C:\file.txt"`
  is accepted. A relative path is not accepted either without a
  `RootDirectory`.
* **The share mode is not optional.** A file opened with a share mode
  that does not include `FILE_SHARE_DELETE` cannot be opened again
  while the first handle is open: `STATUS_ACCESS_DENIED` (0xC0000022).
* **The inline assembly on the Windows target is AT&T syntax.** Intel
  syntax is rejected by the assembler ("unknown use of instruction
  mnemonic without a size suffix"), so the existing style in
  `syscalls.rs` carries over unchanged. A literal `$` still has to be
  doubled (`$$`), and the operand list still has to carry explicit types
  (`i64 %a0`), or the module does not verify.
* **`movl $imm, %r64` does not assemble** ("invalid operand for
  instruction"). Use `movq`.
* **The `syscall` instruction assembles for the Windows target** and the
  result comes back in `rax`; `rcx` and `r11` are destroyed by the
  transition, so the constraint string has to say so.

### How much of "no C library" is reachable

Everything except the dependency on `ntdll` itself.

* **No PE loader is needed.** We need two things from `ntdll`: the
  address of its exports and the syscall number in each stub. Both come
  from reading structures that are already mapped: the loader list in the
  PEB, and the export directory of the module. That is a hundred and
  fifty lines of IR, not a loader.
* **No system call stub generator is needed.** The stub is a fixed block
  of inline assembly; the syscall number is a global that the resolver
  fills in. Nothing is generated at run time and no page has to be made
  executable.
* **What cannot be removed is `ntdll`.** The syscall numbers are not
  documented and are not derivable; they live in `ntdll`'s stub bytes.
  A program that wanted to drop `ntdll` would have to carry a table of
  numbers per Windows build, which is worse than reading them.
* **What is removed is `ucrt`**: `printf`, `fputs`, `fputc`, `exit`,
  `malloc`, `free`, `fopen`, `fclose`, `fseek`, `ftell`, `fread`,
  `fwrite` and `__acrt_iob_func` - thirteen of the seventeen symbols
  listed in `docs/native-roadmap.md:29`, plus the C runtime's
  `mainCRTStartup`, which the program replaces with an entry point of
  its own. The remaining four (`printf` and the stdio helpers on the
  console, `exit`) go in the last two steps of the order above.

So the honest answer to the question is: **the C library stays on Windows
until 11a.2 is finished, and it can be finished** - the only things the
program will still depend on are the kernel and `ntdll.dll`, which is
the kernel's own module.

### The first step

The first step towards removing the C library is **the resolver and the
stub templates**, not the allocator and not the file routines. They are
the part that is independent of everything else, and they are the part
that settles the question the whole slice depends on: what the kernel's
interface looks like from the runtime, and whether the runtime can make
the transition itself. The resolver needs no memory of its own, so it
can be built and tested before the allocator exists; the harness in
step 3 needs no heap either.

## Risks

* **The argument layout is not settled.** Two programs whose system call
  stubs are byte-for-byte identical gave different results: the same
  `NtAllocateVirtualMemory` call succeeded in one and returned
  `STATUS_INVALID_HANDLE` in the other, and the only difference was the
  size of the caller's stack frame. The measurements that are consistent
  are: the kernel reads the fifth and later arguments from the user
  stack, at the slot the calling convention reserves for them, and the
  stub must not move the stack pointer across the transition. The
  measurement that is *not* consistent is the exact address of that slot
  relative to the stack pointer at the transition: 0x28 worked in one
  program and did not work in another with the same stub. **This must be
  measured again, carefully, before anything is built on it** - and the
  measurement harness is the first thing to build. If it cannot be made
  consistent, the fallback is route (a): call the `Nt*` exports directly
  and give up the "inline assembly" form of D013 on Windows only.
* **The optimiser is hostile to the design.** Dead argument elimination,
  function specialisation and call-slot reuse all remove the arguments
  from under the kernel. Any wrapper shape that relies on the compiler to
  place the stack arguments is fragile, and the fragility is silent: the
  program builds, and the call returns a plausible error code.
* **The native API is not the documented API.** `NtAllocateVirtualMemory`
  has a `ZeroBits` argument that a reader of the C prototype would not
  expect, and the native file calls want NT paths. Every signature in
  this document has to be checked against the running system, not
  against the header.
* **The `Nt*` interface is not documented by Microsoft.** It is stable in
  practice - the stub layout and the call numbers have been stable across
  the Windows versions anyone uses - but it is not a contract. A Windows
  build that renumbers its services would break the runtime, and the
  failure would be "the syscall returned an odd status", not a link
  error.
* **The 32-bit path is out of scope.** The `Nt*` stubs select the
  `int 2eh` transition for a WOW64 process, and a 64-bit program cannot
  use it. Nexum's Windows target is x86-64 only
  (`crates/nexum-target/src/lib.rs:41`), so this is only a problem if
  that changes.
* **The console is more work than the files.** The standard handles
  come from the process parameters in the PEB, and the PEB's layout is
  another set of structures to get right - and the `printf` replacement
  is the largest single piece of the runtime that is still on the C
  library.

## Open questions

1. **Which route, for the record?** The recommendation is route (b), our
   own stubs, with the numbers read from `ntdll` at start-up. The
   orchestrator has to accept that the program will still depend on
   `ntdll.dll` - and that D012 and D013 should be amended to say so on
   Windows, because the current wording ("depends on the kernel and on
   nothing else", "never through a C binding") reads as a prohibition on
   `ntdll` that cannot be honoured. Recommendation: amend D012 with a
   Windows clause, "the kernel's native interface on Windows is
   `ntdll.dll`; the program depends on it and on nothing else", and leave
   D013 alone, since route (b) satisfies it as written.
2. **Does the orchestrator want the argument layout settled before the
   allocator is written?** Recommendation: yes - the allocator is the
   first consumer of the system call layer and the first thing that
   will show the layout is wrong, so the harness comes first and the
   allocator follows it.
3. **Should the resolver live in the runtime, or in the toolchain?**
   Recommendation: in the runtime. The numbers differ per Windows build,
   so they cannot be resolved at link time; and the runtime already
   generates its own code (`crates/nexum-runtime/src/lib.rs:46`), so the
   resolver and the stub templates belong there.
4. **How much of the console work belongs in 11a.2?** Recommendation: the
   file routines and the exit first, the console last. A program that
   can read and write files but cannot print is still a program; a
   program that can print but cannot allocate is not.
5. **Should `docs/runtime-abi.md` gain a Windows section now?** The
   document already says the Windows build needs generated stubs
   (`docs/runtime-abi.md:30`); recommendation: yes, once the layout is
   measured, record the register set, the slot the kernel reads, and the
   structures, next to the POSIX table at `docs/runtime-abi.md:54`.
