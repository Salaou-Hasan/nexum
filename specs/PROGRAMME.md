# The programme board

Status: **active**. The native roadmap is `docs/native-roadmap.md`. This
file is where the programme is *run*: what is in flight, what was decided,
and what is next. The agents' own rules are in `specs/AGENT-BRIEF.md`.

## How the programme runs

```
research  ->  contract  ->  implementation  ->  integration  ->  release
   (agents)   (the lead)     (agents)          (the lead)      (the lead)
```

Research agents each own one question and one file, work in their own
worktree, and open a pull request. The lead reviews each pull request
against the brief, merges it, and distils the findings into a contract.
Implementation agents then work from the contract, each on a disjoint set
of files, each with its own pull request. The lead integrates, runs the
gates, and cuts a release.

The rule that keeps the agents from colliding: **one question, one file,
one branch, one pull request**. Nothing is written to `main` except by the
lead, and only after review.

## Phase 11a - the freestanding runtime

The goal: a Nexum program depends on the kernel and on nothing else.

| Piece | State |
| ----- | ----- |
| The system call layer (Linux, macOS; x86-64, AArch64) | **done** |
| The runtime's own string routines | **done** |
| The runtime's own file routines (POSIX) | **done** |
| The cross-target verification assembles for the target | **done** |
| The freestanding gate (the external symbol list) | **done** |
| The allocator over `mmap` (replaces `malloc`/`free`) | **done** |
| The output formatting (replaces `printf`/`fputs`/`fputc`) | **done** |
| The maths builtins (replaces libm) | **done** |
| The Windows system call stubs | in research |

## The research: 25 documents, all merged

Each is a pull request against `main`, written by one agent, in
`specs/research/`. The decisions they settled are in
`docs/language-reference/decisions.md` (D012 to D020) and in the
contracts.

The documents cover: the numeric tower, the f32 touchpoints, the
allocator's design, the number formatting, the maths implementations, the
inline assembly design, the memory and layout model, the record layout,
concurrency, the memory model and atomics, vectors and SIMD, the native
linker, the code generator's backend, the toolchain's dependencies, the
GPU path, the Windows system calls, the model framework, the C dependency
audit, the error model, the syntax's growth, the standard library, the
self-hosting plan, the compiler pipeline, the test surface, the
performance methodology, interoperability, and WebAssembly.

## The implementation: seven agents, seven pull requests, all merged

Phase 11a.2, one pull request each, disjoint files.

| Agent | Owns | State |
| ----- | ---- | ----- |
| the allocator | `crates/nexum-runtime/src/heap.rs`, `tests/allocator.rs` | **merged** |
| the output | `crates/nexum-runtime/src/format.rs`, the codegen's `print` block, `tests/output.rs` | **merged** |
| the maths | `crates/nexum-runtime/src/math.rs`, `tests/math.rs` | **merged** |
| the bytes | `crates/nexum-runtime/src/bytes.rs`, the sema's builtin dispatch | **merged** |
| the layout | `crates/nexum-target/src/lib.rs` and its tests | **merged** |
| the benchmarks | `crates/nexum-driver/src/bin/nx-bench.rs`, `benchmarks/suite/` | **merged** |
| the syscall tests | `crates/nexum-driver/tests/syscall_layer.rs` | **merged** |

The contract is `specs/contracts/phase-11a2.md`; the contract for the
language phases is `specs/contracts/phase-11b-11f.md`.

### Wave A: inline assembly

Four agents, four pull requests, merged together on a branch so that
`main` never went red in the middle of the wave:

| Agent | Owns | State |
| ----- | ---- | ----- |
| the Rust front end | `crates/nexum-parser/src/lib.rs`, `normalize.rs` | **merged** |
| the self-hosted front end | `examples/selfhost/parser.nx` | **merged** |
| the sema | `crates/nexum-sema/src/lib.rs`, `crates/nexum-mir/src/lib.rs` | **merged** |
| the code generator | `crates/nexum-codegen/src/lib.rs` | **merged** |

**The wave found a hole in its own contract**, which is the most useful
thing that happened in it. The contract said the template's operands are
named, but the pinned MIR instruction carries no operand names, so a
program written that way reached LLVM unchanged and **crashed the
backend** instead of producing a diagnostic. The code generator's agent
found it, said plainly that it was not its file to fix, and set out the
two ways out. D021 settles it: the template is LLVM's own, and the
semantic pass checks the operand range and the constraint count, so a
mistake is a diagnostic.

**The two front ends designed different shapes** - the parser made
`asm(...)` a statement, the sema expected an ordinary call - because the
contract settled the semantics and not the boundary between the two
crates. That is what an integration agent is for, and it is the lesson
for the next contract: a contract has to settle the shape, not only the
meaning.

### What the phase achieved

A Linux program declared seventeen external symbols when the programme
started. It declares **none** now: the heap is our allocator over the
system calls, the string routines are ours, the file routines go through
the kernel, the output is formatted by the runtime and written with a
system call, and the panic exits through the kernel.

| Piece | Before | Now |
| ----- | ------ | --- |
| the system call layer | none | Linux and macOS, x86-64 and AArch64 |
| the string routines | libc | ours, on every platform |
| the file routines | libc | the system calls, on the POSIX targets |
| the heap | `malloc` | ours, on every platform |
| the output | `printf` | ours, on the POSIX targets |
| the float format | the C library's `%g` | ours, matched against it over four million values |
| the maths | libm | ours: `exp`, `log`, `tanh`, `erf`, `pow`, `sqrt` and the four rounding functions, in 64-bit operations with no `i128` and no integer division |

The Windows build is still on the C library for its output and its file
routines. Its heap is ours now, though: the allocator uses `VirtualAlloc`,
which is the kernel and not the C library.

## The one open item

**The allocator fails on macOS.** Linux and Windows pass. The symptom is
a crash in the first allocation, with no output.

What is known, from measurement rather than argument:

* The system call layer itself works on macOS: the file routines read and
  write, and a probe that maps memory directly succeeds on Linux.
* The mapping arguments are in the right order and the page size is
  right - 16384 on macOS on Apple Silicon, 4096 everywhere else.
* **Darwin's `munmap` is 73**, not the 198 that sits next to `mmap` in the
  BSD master table; 198 is an unused slot. The table had it wrong, so the
  allocator's unmap called a system call that does not exist.
* Darwin reports an error by setting the carry flag and returning the
  error number *positive*, where Linux returns it negative. The runtime
  makes the result negative in the assembly so that one convention holds
  everywhere - but that only works if the flag is what the kernel sets.

The probe (`crates/nexum-driver/tests/probe.rs`) prints what the kernel
returns for a mapping, for an open of a file that is not there, and for a
write, and then walks the allocator's own sequence. It writes unbuffered,
so a crash does not take the lines that say where it happened with it.

Two rounds of guessing were wrong before the probe existed, which is the
lesson the research had already written down and the orchestrator had not
applied to its own debugging.

## The decisions this programme has taken so far

* **D012** - the runtime is freestanding: no libc.
* **D013** - the operating system is reached through inline assembly.
* **D014** - inline assembly is a first-class language feature.
* **D015** - the numeric tower is f32 and f64.
* **D016** - the program is linked by the Nexum toolchain, not by clang.
* **D017** - the language grows no binary FFI.
* **D018** - the numeric tower has distinct widths and no implicit conversions.
* **D019** - printing a float is C's `%g`.
* **D020** - the code generator keeps emitting textual LLVM IR.

## What the research found, and what it cost

The research was supposed to design the later phases. Instead it found
**nine live defects**, each confirmed by running the compiler rather than
by reading it:

| Defect | Found by |
| ------- | -------- |
| comparing two floats produced invalid LLVM | four agents independently |
| an integer literal that does not fit `i64` silently became `0` | the numeric tower |
| the most negative `i64` could not be written | the numeric tower |
| the link step ignored `--target`, so a cross build produced a host binary | two agents |
| the `mmap` wrapper put the file descriptor in the flags register | three agents |
| the write path passed a pointer where an integer was wanted | the test surface |
| the data layouts had drifted from clang's on all five targets | the backend |
| Darwin's system call numbers were wrong in two places | the syscall tests |
| Darwin's `munmap` was 198, an unused slot | the XNU master table |

And one fact about the gates themselves: **the LLVM verifier does not
catch a call with mismatched argument types**, because clang passes
`-disable-llvm-verifier` to the compiler proper. Every "the IR verifies"
gate in the project was therefore weaker than it looked, and a unit test
now checks the runtime's own calls.

## The lessons from the first slice

The system call layer took four fixes, and every one of them was silent:
the module verified, the build succeeded, and the program died at the
first call.

1. LLVM counts an assembly block's output as operand zero, so the
   arguments were numbered from the wrong place.
2. `movq 1, %rax` reads memory; the immediate needs the sigil.
3. Darwin numbers its system calls differently from Linux, and the
   difference is invisible until a call reaches the kernel as another one.
4. The system call instruction wants a 16-byte aligned stack.

The lesson for the programme: **anything that only fails on one platform
needs a test that runs everywhere.** The unit tests in
`crates/nexum-runtime/src/syscalls.rs`, the gate in
`crates/nexum-driver/tests/freestanding.rs` and the probe in
`crates/nexum-driver/tests/probe.rs` exist because of that.
