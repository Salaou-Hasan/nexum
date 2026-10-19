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
| The allocator over `mmap` (replaces `malloc`/`free`) | in research |
| The output formatting (replaces `printf`/`fputs`/`fputc`) | in research |
| The Windows system call stubs | in research |

A program built for Linux declares six external symbols today:
`printf`, `fputs`, `fputc`, `exit`, `malloc`, `free`. It used to declare
seventeen. The gate `crates/nexum-driver/tests/freestanding.rs` pins the
list, so it can only get shorter.

## The research in flight

Each of these was a pull request against `main`, written by one agent, in
`specs/research/`. All twenty-two are merged.

| Document | Question |
| -------- | -------- |
| `f32-touchpoints.md` | where a new primitive type has to thread through the compiler |
| `numeric-tower.md` | the numeric types and the exact semantics of every operation |
| `allocator-design.md` | the allocator that replaces `malloc`, and the collector's needs |
| `number-formatting.md` | the formatting that replaces `printf`, matching C's output |
| `math-implementations.md` | the maths builtins, implemented without libm |
| `inline-asm-design.md` | inline assembly as a first-class language feature |
| `memory-layout.md` | pointers, explicit layout, volatile, unsafe, arenas |
| `record-layout.md` | the record layout rules and how to control them |
| `concurrency.md` | threads without the C library |
| `atomics-model.md` | the atomic types, the orderings, the memory model |
| `simd-vectors.md` | vectors and SIMD |
| `native-linker.md` | linking a program without clang |
| `codegen-backend-options.md` | textual IR against LLVM as a library |
| `toolchain-dependencies.md` | what the toolchain may depend on |
| `gpu-path.md` | the GPU path without CUDA or a C binding |
| `windows-syscalls.md` | reaching the Windows kernel without the C library |
| `ml-framework.md` | the model framework and what it needs from each phase |
| `ffi-audit.md` | the complete checklist of remaining C dependencies |
| `error-handling-model.md` | what replaces the panic-only model |
| `syntax-evolution.md` | how the syntax grows for the systems work |
| `standard-library-plan.md` | the standard library, written in Nexum |
| `self-hosting-plan.md` | how the compiler comes to be written in Nexum |
| `compiler-pipeline.md` | the compiler stage by stage, and the extension points |
| `test-surface.md` | the test surface, and where a new feature's tests belong |
| `performance-methodology.md` | how the programme measures speed |
| `interop-policy.md` | interoperability without a C binding |
| `wasm-target.md` | WebAssembly as a target |

## The implementation in flight

Phase 11a.2, one pull request each, disjoint files:

| Agent | Owns |
| ----- | ---- |
| the allocator | `crates/nexum-runtime/src/heap.rs`, `tests/allocator.rs` |
| the output | `crates/nexum-runtime/src/format.rs`, the codegen's `print` block, `tests/output.rs` |
| the maths | `crates/nexum-runtime/src/math.rs`, `tests/math.rs` |
| the bytes | `crates/nexum-runtime/src/bytes.rs`, the sema's builtin dispatch |
| the layout | `crates/nexum-target/src/lib.rs` and its tests |
| the benchmarks | `crates/nexum-driver/src/bin/nx-bench.rs`, `benchmarks/suite/` |
| the syscall tests | `crates/nexum-driver/tests/syscall_layer.rs` |

The contract is `specs/contracts/phase-11a2.md`; the contract for the
language phases is `specs/contracts/phase-11b-11f.md`.

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
**six live defects**, each confirmed by running the compiler rather than
by reading it:

| Defect | Found by |
| ------- | -------- |
| comparing two floats produced invalid LLVM | four agents independently |
| an integer literal that does not fit `i64` silently became `0` | the numeric tower |
| the most negative `i64` could not be written | the numeric tower |
| the link step ignored `--target`, so a cross build produced a host binary | two agents |
| the `mmap` wrapper put the descriptor in the flags register | three agents |
| the write path passed a pointer where the wrapper wanted an integer | the test surface |

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
`crates/nexum-runtime/src/syscalls.rs` and the gate in
`crates/nexum-driver/tests/freestanding.rs` exist because of that.
