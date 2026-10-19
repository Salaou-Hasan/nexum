# The test surface, and where a new feature's tests belong

**Question.** What does the Nexum test suite prove today, what does it not,
and where exactly should the tests for phases 11a to 11i live so that the
agents implementing them do not collide on a test file?

**Summary.** The recommendation comes first: **freeze the four existing test
files after 11a, give every phase from 11b on its own new integration test
file, and let a new fixture be the test.** The suite is small and well
shaped - 36 tests in eight files, in four fixture kinds, plus four
differential gates - but it has four holes that matter to the programme: the
IR "verification" gate does **not** catch a call whose arguments do not match
the callee's signature (measured below), the seven backend tests that need
clang **silently pass** when clang is missing, the system call layer is only
*executed* on the runners that happen to be POSIX, and the collector's
out-of-memory path, the large-input path and the Windows-only paths have no
test at all. Two of those four are the ones that let the bugs of the last
slice through - the IR gate's blind spot, and the fact that only a POSIX
runner ever executes a system call - and both are closed by one new gate: a
per-target assertion on the *shape* of the emitted system call call sites,
which I recommend as the first thing 11a does.

## Recommendation

Fourteen numbered items. Items 1-4 are the enabling work; 5-13 are the
placement plan, one per phase; item 14 is the two harness fixes that make
every other gate's green mean something.

1. **Every phase gets its own integration test file, and only its own.**
   The four existing test files
   (`crates/nexum-driver/tests/backend.rs`, `fixtures.rs`, `freestanding.rs`,
   `common/mod.rs`) are **frozen after 11a**. From 11b on, a new gate goes
   in a new file named after the phase. The exact names are in the table
   below. *Reason*: the programme runs several agents at once
   (`specs/PROGRAMME.md`), and the whole class of "two agents edited
   `backend.rs`" merge conflicts is removed by naming one file per phase.

2. **A new fixture is a test; a new test file is only for a new *kind* of
   gate.** The three fixture directories are auto-discovered
   (`crates/nexum-driver/tests/common/mod.rs:89` `nx_files` and `:99`
   `fixtures_dir`, used at `fixtures.rs:16-17`, `:42-43`, `backend.rs:47`,
   `:96-97`, `:222-223`), so a new
   `.nx` file in `pass/`, `fail/`, `parse/` or `run/` is picked up with no
   change to any test. *Reason*: the fixture directories are the language's
   specification (`docs/self-hosting.md:6-7`); a phase that adds a feature
   should add fixtures, not edit the harness.

3. **Make the opt-level gate discover the run fixtures instead of listing
   them.** `backend.rs:254-269` hard-codes 14 file names and
   `backend.rs:271-273` silently `continue`s when one is missing, so a
   renamed fixture drops out of the gate without a word. Replace the array
   with `nx_files(&fixtures_dir("run"))`. *Owner: the 11a agent* - it is the
   last change to `backend.rs` in the programme. *Reason*: this is what
   makes item 1 work; after it, no phase from 11b on has to touch
   `backend.rs` to get its fixtures into the differential gate.

4. **Add an optional `.err` sidecar to the run fixtures.**
   `run_outputs_match_fixtures` (`backend.rs:93`) compares stdout and the
   exit code but never stderr, so no test in the repository can pin a panic
   message. Make it compare `<stem>.err` when that file exists. *Owner: the
   11a agent*, same single edit to `backend.rs`. *Reason*: the panic text
   is the observable behaviour of every runtime error, and 11a-11d all add
   error paths. With the sidecar, a phase pins a message by dropping a file
   next to its fixture.

5. **11a - the freestanding runtime. Three slices, three new files.**
   - 11a.1 (the system call layer, already landed) ->
     **`crates/nexum-driver/tests/syscall_layer.rs`** (new). The gate is a
     per-target assertion on the *shape* of the emitted call sites, and it
     is the test that would have caught `73783d8` and `c600dcf`. The
     runtime crate is 11a's own, so the unit tests in
     `crates/nexum-runtime/src/syscalls.rs` and a new
     `#[cfg(test)] mod tests` in `crates/nexum-runtime/src/strings.rs` are
     also 11a's to change.
   - 11a.2 (the allocator over `mmap`, and the output formatting that
     replaces `printf`/`fputs`/`fputc`) ->
     **`crates/nexum-driver/tests/allocator.rs`** and
     **`crates/nexum-driver/tests/output.rs`** (new).
   - 11a.2 (the Windows system call stubs) ->
     **`crates/nexum-driver/tests/windows_syscalls.rs`** (new).
   *Reason*: the three slices have three different owners and three
   different failure modes; one file would collide.

6. **11b - inline assembly -> `crates/nexum-driver/tests/asm.rs`** (new),
   with fixtures `parse/asm_*.nx` (so the self-hosted parser gate covers
   the new syntax), `pass/asm_*.nx`, `run/asm_*.nx` + `.out`, and
   `fail/asm_*.nx` with `# EXPECT: Txxx`. The gate is per-target: an
   assembly block for a foreign instruction set must assemble for the
   target it claims, and that is the same check
   `ir_verifies_for_all_targets` already does for the runtime.

7. **11c - the numeric tower -> `crates/nexum-driver/tests/numeric.rs`**
   (new), with fixtures `run/f32_*.nx`, `run/math_*.nx`,
   `run/int_widths_*.nx`. The gate is **exact bits, not tolerance**: the
   math builtins are ours and not libm's, so their expected values have to
   be a table in the test, not a comparison against the C library. A
   tolerance test would pass with a libm-backed implementation, which is
   exactly what 11c must not have.

8. **11d - memory and layout -> `crates/nexum-driver/tests/memory.rs`**
   (new), with fixtures `run/arena_*.nx`, `run/volatile_*.nx`,
   `run/packed_*.nx`, and `fail/unsafe_*.nx`. The arena fixture is also
   the first test of D009, which has been specified since the bootstrap and
   never exercised.

9. **11e - concurrency -> `crates/nexum-driver/tests/concurrency.rs`**
   (new), with fixtures `run/threads_*.nx`, `run/atomics_*.nx`,
   `run/channels_*.nx`. The gate must be *deterministic*: a thread test
   that asserts on timing is a flaky test. The collector is single-threaded
   today (`docs/runtime-abi.md:118`); 11e must either keep it that way and
   say so in a test, or make it safe and say that instead. One of the two
   has to be pinned.

10. **11f - SIMD -> `crates/nexum-driver/tests/simd.rs`** (new), with
    fixtures `run/simd_*.nx`. The gate is the *behaviour* (a vectorised sum
    equals a scalar sum, bit for bit) plus a loose shape check on the
    emitted IR (`<4 x float>`, `<8 x float>`). Do **not** assert on LLVM
    intrinsic names: that pins the test to the LLVM version.

11. **11g - the native linker -> `crates/nexum-driver/tests/linker.rs`**
    (new), with fixtures `run/linked_*.nx`. The gate is a *differential*
    one: the same program linked by the Nexum linker and by `clang` must
    produce the same stdout, the same exit code and the same external
    symbol list. The clang-linked build is the oracle, so this gate needs
    no new reference values.

12. **11h - the GPU path -> `crates/nexum-driver/tests/gpu.rs`** (new).
    Split it in two: the shader front end (the SPIR-V/Metal emission) is
    pure computation and runs **everywhere**; the dispatch and the
    GPU-versus-CPU comparison needs a device and must be *skipped*, not
    failed, on a machine without one. The skip convention already in the
    tree is `let Some(x) = ... else { return }` (`backend.rs:46`) - but see
    item 14: that convention must not be used for a gate that is supposed
    to prove something.

13. **11i - the ML framework -> `crates/nexum-driver/tests/ml.rs`** (new),
    with fixtures `run/ml_*.nx`. Two gates: the autograd gradient against
    an *analytic* value (exact, no tolerance), and the optimizer's
    convergence (tolerance allowed - it is the one place a tolerance is
    honest). The `f32` claim is checked at the ABI level: the tensor
    payload must be `[N x float]`, not `double`.

14. **Two fixes to the harness itself, both owned by the 11a agent, both
    done before any later phase starts.**
    - **A missing toolchain must not look like a pass.** Seven of the eight
      backend tests begin `let Some(tc) = toolchain() else { return };`
      (`backend.rs:46`, `:54`, `:62`, `:95`, `:144`, `:200`, `:253`). On a
      machine without clang the backend suite reports *8 passed* and proves
      nothing. Make it panic, or at least print a loud "skipped: no clang"
      that the CI log cannot miss. The CI installs clang
      (`.github/workflows/ci.yml:19-24`), so CI is safe today; a
      developer's machine is not.
    - **Give each test its own directory.** Every backend test writes into
      the single shared `%TEMP%/nexum_tests/` (`backend.rs:16-20`). Two
      different tests write the *same file name*: `ir_verifies_for_pass_fixtures`
      writes `arrays.ll` from `pass/arrays.nx` and `run_outputs_match_fixtures`
      writes `arrays.ll` from `run/arrays.nx`; the same for `records.ll`.
      The Rust harness runs the tests of one binary in parallel threads, so
      the two writes race. A flaky "IR verification failed" on `arrays.ll`
      or `records.ll` is this collision, not a defect. The helper for this
      already exists - `work_dir` (`backend.rs:24-28`), used by the two
      self-hosted tests - and should be used by all of them.

## What the code looks like today

### The inventory

36 tests, in eight files, in five crates.

| File | Kind | Tests | Line of the module |
| ---- | ---- | ----- | ------------------ |
| `crates/nexum-driver/tests/backend.rs` | integration | 8 | - |
| `crates/nexum-driver/tests/fixtures.rs` | integration | 2 | - |
| `crates/nexum-driver/tests/freestanding.rs` | integration | 4 | - |
| `crates/nexum-lexer/src/lib.rs` | unit | 7 | `:439` |
| `crates/nexum-parser/src/lib.rs` | unit | 7 | `:1163` |
| `crates/nexum-runtime/src/syscalls.rs` | unit | 5 | `:294` |
| `crates/nexum-toolchain/src/lib.rs` | unit | 2 | `:250` |
| `crates/nexum-driver/src/main.rs` | unit | 1 | `:1178` |

`crates/nexum-sema` (2054 lines), `crates/nexum-mir` (1393),
`crates/nexum-codegen` (1238), `crates/nexum-parser/src/normalize.rs` (254),
`crates/nexum-diagnostics` (75), `crates/nexum-source` (78) and
`crates/nexum-target` (105) have **no unit tests at all**. Everything they
do is covered only through the driver's integration tests, or not at all.

Verified on this machine (Windows, clang 23.1.3, rustc 1.95.0) against
`main` at `41547e6`: `cargo test --workspace` -> 36 passed, 0 failed, in
six test binaries.

### The backend suite, test by test

All eight live in `crates/nexum-driver/tests/backend.rs`. They share five
helpers: `toolchain()` (`:12`), `tmp_dir()` (`:16`), `work_dir()` (`:24`),
`ir_verifies()` (`:30`), and the `common` module
(`crates/nexum-driver/tests/common/mod.rs`).

1. **`ir_verifies_for_pass_fixtures`** (`:44`). For every `.nx` in
   `tests/fixtures/pass/`, compiles front end -> MIR -> MIR verification,
   emits LLVM IR for the **host** target, writes it to
   `%TEMP%/nexum_tests/<stem>.ll` and runs
   `nexum_toolchain::verify_ir` (which is `clang -Wno-override-module -x ir
   -c`, `crates/nexum-toolchain/src/lib.rs:214-248`). *Proves*: the six
   `pass/` fixtures produce a module the LLVM front end accepts. *Needs*:
   clang. *Does not prove*: anything about the fixtures' runtime behaviour -
   there is no `.out` for a `pass/` fixture.

2. **`ir_verifies_for_examples`** (`:52`). The same for the nine programs in
   `examples/` (the three in `examples/selfhost/` are in a subdirectory and
   are not read by `nx_files`, which is not recursive). *Proves*: the
   example programs, which are bigger and more varied than the `pass/`
   fixtures, produce a valid module.

3. **`ir_verifies_for_all_targets`** (`:60`). Emits `examples/hello.nx` for
   each of the five targets in `nexum_target::TargetSpec::list()`
   (`crates/nexum-target/src/lib.rs:75-83`: `windows-x86_64`,
   `linux-x86_64`, `linux-aarch64`, `macos-x86_64`, `macos-aarch64`) and
   verifies each with `verify_ir_for(clang, ll, Some(target.triple))`, i.e.
   `clang --target=<triple> -x ir -c`. *Proves*: the module is valid **for
   the target it claims**, not for the host. That matters because the
   runtime's system call wrappers are inline assembly: a Linux AArch64
   `svc #0` cannot be checked by an x86-64 assembler. The fix that made
   this cross-target is in `e3e16c7`. *Also proves*, incidentally, that the
   syscall wrappers assemble for all four (os, arch) pairs, because the
   runtime IR is in every module (`crates/nexum-codegen/src/lib.rs:119`).
   *Needs*: clang, and a clang that can target all five triples. CI runs
   this test on all three runners (`.github/workflows/ci.yml:29-31`).

4. **`codegen_emits_expected_fragments`** (`:76`). Compiles
   `pass/numbers.nx` and asserts the IR contains `define i32 @main(`, `fadd
   double`, `mul i64`, and **does not** contain `nsw`. *Proves*: the
   codegen's shape on four points - the entry signature, `Float` as
   `double`, `Int` as `i64`, and wrapping (not trapping) integer
   arithmetic. *Needs*: nothing but the compiler. This is the cheapest test
   in the suite and the only one that pins the arithmetic semantics.

5. **`run_outputs_match_fixtures`** (`:93`). For every `.nx` in
   `fixtures/run/`: compile, assert zero error diagnostics, emit IR, verify
   it, link it with `run_clang` (`crates/nexum-toolchain/src/lib.rs:174`),
   run the executable, and compare the **stdout** with `<stem>.out` and the
   **exit code** with `<stem>.exit` (default 0). *Proves*: the observable
   behaviour of 21 programs, including the three that trap
   (`divzero`, `index_oob`, `panic`) and the one that returns a non-zero
   code (`exit_code`, exit 3). *Needs*: clang, a linker, and the ability to
   run a native binary - and, for `io_roundtrip`, a writable working
   directory. *Does not prove*: anything about stderr. There is no `.err`
   fixture and no stderr comparison anywhere in the repository.

6. **`selfhosted_lexer_matches_rust_lexer`** (`:142`). Compiles
   `examples/selfhost/lexer.nx` once, then for **every program in
   `examples/`**: copies it to
   `%TEMP%/nexum_tests/lexer/nexum_selfhost_input.nx`, runs the self-hosted
   lexer, and compares its output with the token stream the Rust lexer
   produces (`kind.name()`, a tab, the text, a newline -
   `backend.rs:171-179`). *Proves*: the self-hosted lexer and the Rust
   lexer agree, on every example program. *Needs*: clang and a POSIX-ish
   file read (on Windows the self-hosted lexer uses the `fopen` path, so
   the file read is the C library's there).

7. **`selfhosted_parser_matches_rust_parser`** (`:198`). Compiles
   `examples/selfhost/parser.nx` once, then for **every `.nx` in
   `fixtures/parse/`**: runs the self-hosted parser and compares its output
   with `nexum_parser::normalize::normalize(&module)`
   (`crates/nexum-parser/src/normalize.rs:248`), the line-oriented tree form.
   *Proves*: the self-hosted parser builds the same tree as the Rust
   parser, on 35 fixtures.

8. **`differential_opt_levels_agree`** (`:251`). For a hard-coded list of
   14 programs (5 examples + 9 run fixtures), compiles at MIR opt level 0
   and at level 1, links and runs both, and compares
   `"<exit code>\n<stdout>"`. *Proves*: the MIR optimiser - const fold,
   DCE, inlining (`crates/nexum-mir/src/lib.rs:1383-1391`) - does not
   change the observable behaviour. Note that this compares the *MIR*
   optimiser, not clang's: `run_clang` always passes `-O0`
   (`crates/nexum-toolchain/src/lib.rs:186`).

### The freestanding gate

`crates/nexum-driver/tests/freestanding.rs` was added in `2a193e3` and
tidied in `41e2e59`, after this task was written; the placement plan below
accounts for it.

- `external_symbols(target)` (`:19`) emits `examples/hello.nx` for a named
  target and collects the `declare` lines.
- `posix_programs_need_only_the_remaining_libc` (`:40`) pins the POSIX
  external symbol list to exactly `exit, fputc, fputs, free, malloc,
  printf` - six symbols, down from seventeen.
- `windows_still_needs_the_c_library` (`:59`) pins the Windows list to
  thirteen symbols, including the six file routines and
  `__acrt_iob_func`.
- `posix_programs_have_the_system_call_layer` (`:85`) asserts that the
  wrappers `read, write, open, close, lseek, exit` are present and that
  the IR contains `asm sideeffect`.
- `the_string_routines_are_ours` (`:108`) asserts `nexum_strlen`,
  `nexum_memcpy` and `nexum_strcmp` are defined and that the C library's
  `strlen`, `memcpy` and `strcmp` are **not** declared.

*Proves*: the shape of the freestanding claim - the list of external
symbols, per target. *Does not prove*: that the calls *into* the syscall
layer are correct. It checks that the wrappers are present, not that they
are called correctly.

### The fixture kinds

Four directories under `crates/nexum-driver/tests/fixtures/`. The
conformance fixtures are the language's specification
(`docs/self-hosting.md:6-7`).

| Directory | Files | What it means | Who reads it |
| --------- | ----- | -------------- | ------------ |
| `pass/` | 6 `.nx` | must lex, parse, type-check, lower to MIR and pass MIR verification with **zero** error diagnostics | `compile_pass_fixtures` (`fixtures.rs:14`) and `ir_verifies_for_pass_fixtures` (`backend.rs:44`) |
| `fail/` | 29 `.nx` | must produce **at least one** error diagnostic; every `# EXPECT: Txxx` in the file must be among the codes produced; every diagnostic produced must carry a source span | `compile_fail_fixtures` (`fixtures.rs:40`) |
| `parse/` | 35 `.nx` | must parse without errors; the *normalised* form is the contract | `selfhosted_parser_matches_rust_parser` (`backend.rs:198`) |
| `run/` | 21 `.nx` + 21 `.out` + 4 `.exit` | the program's stdout and exit code | `run_outputs_match_fixtures` (`backend.rs:93`); a hard-coded subset of 9 in the opt-level gate |

**The file format.** A fixture is a `.nx` source file; the sidecars are
`.out` (the exact expected stdout, compared after CRLF -> LF
normalisation, `backend.rs:103` and `:121`) and `.exit` (the expected
process exit code, an integer, trimmed, `backend.rs:123-130`; **absent
means 0**). `.gitattributes` forces LF on `*.nx`, `*.out` and `*.exit`, so
the checkout is already normalised; the in-code normalisation is belt and
braces. There is no `.err` sidecar - that is item 4 of the recommendation.

**The naming convention.** `snake_case`, and the name says what is under
test rather than what is expected: `index_oob_const.nx`, `mut_borrow.nx`,
`list_push_mismatch.nx` in `fail/`; `gc_reclaim.nx`, `int_wrap.nx`,
`io_roundtrip.nx` in `run/`; `fn_while.nx`, `arith_precedence.nx`,
`unary_not.nx` in `parse/`. The `fail/` fixtures carry the expectation in
the file itself, in a comment: `# EXPECT: T037` (`fixtures.rs:51-56`
scans every line, takes the text after the first `#`, and strips the
`EXPECT:` prefix). The codes are the semantic codes of `nexum-sema` -
T001 bad if condition, T005 arity, T010 immutable assignment, T033
constant index out of bounds, T037 `mut` borrow, T049 list element type -
plus `L0xx` for the lexer and `M001` for the MIR.

**The check is "contains", not "equals".** `fixtures.rs:75-83` requires
each expected code to be *among* the produced codes; extra codes are
allowed. That is deliberate (one file can trip several checks) but it means
a `fail/` fixture cannot pin the *exact* diagnostic set.

### The unit tests inside the crates, and their convention

22 unit tests, in five crates. The convention is consistent and worth
copying:

- A `#[cfg(test)] mod tests { use super::*; ... }` at the **bottom** of the
  module file, never in a separate file.
- A private helper at the top of the module, named for what it does:
  `lex_str` (`crates/nexum-lexer/src/lib.rs:445`), `kinds` (`:451`),
  `parse_str` (`crates/nexum-parser/src/lib.rs:1170`), `first_fn_body`
  (`:1182`), `has_operand_reference`
  (`crates/nexum-runtime/src/syscalls.rs:354`).
- **Every test carries a doc comment that states the failure it pins**, in
  the present tense and with the consequence:
  "The two systems number their calls differently, and a wrong number is
  silent: the call reaches the kernel as a different one."
  (`crates/nexum-runtime/src/syscalls.rs:298-299`).
- **The name is a full sentence**: `the_numbers_are_per_system`,
  `the_number_is_an_immediate`, `the_stack_is_aligned_for_the_call`,
  `the_arguments_are_pinned_to_their_registers`,
  `aarch64_stages_its_arguments`,
  `token_spans_are_monotonic`, `mul_binds_tighter_than_add`.
- **The assertion carries the artefact**, so a failure prints the evidence:
  `assert!(ir.contains("movq $$0, %rax"), "{ir}")`
  (`crates/nexum-runtime/src/syscalls.rs:326`).
- **A unit test pins a shape that is silent when it is wrong, and it must
  run everywhere.** The `syscalls.rs` tests build the IR text and compare
  strings: no clang, no target, no platform. That is the whole point, and
  the commit message of `316f6c6` says so: "These tests run on every
  platform, not only where the layer is exercised."
  The corollary - an **integration** test pins a *behaviour* - is the rule
  the rest of this document follows.

### The differential gates, and what a change would break

Four gates compare two implementations or two configurations of one.

**Gate A - the self-hosted lexer against the Rust lexer** (`backend.rs:142`).
Reference: `nexum_lexer::lex`. Subject: `examples/selfhost/lexer.nx`,
compiled by the Rust compiler and run on the host. Input set: the nine
programs in `examples/`.
*A runtime change that breaks it*: the collector. The self-hosted lexer was
the first program to build a `List` of records whose fields are heap
strings and to allocate heavily while reading the list back, and it is
what exposed the `nexum_mark_ptr` defect - the mark phase reported
"already marked" where the trace loop needed "newly marked", so the
fixpoint stopped after one pass and the list's element buffer was reclaimed
while still in use (`docs/self-hosting.md:91-118`). The same class of
change - the root registration, the `List` growth path
(`crates/nexum-runtime/src/lib.rs:224`), the `Str` copy in `concat`, the
string routines - breaks this gate. *A codegen change that breaks it*: the
layout of the `Token` record, the calling of `list_get` (which returns a
pointer *into* the element buffer, `crates/nexum-runtime/src/lib.rs:227`),
the `print` of a `Str`, the `read_file` path the lexer reads its input
through. *Its limits*: it only runs on the host target; it only proves
agreement with the Rust lexer, never correctness; the two programs that use
path assignments are covered here.

**Gate B - the self-hosted parser against the Rust parser** (`backend.rs:198`).
Reference: `nexum_parser::normalize::normalize` over the Rust parse tree.
Subject: `examples/selfhost/parser.nx`. Input set: the 35 files in
`fixtures/parse/`.
*A runtime change that breaks it*: the same collector pressure - the parser
builds a flat AST arena of `Node` records in a `List` and grows a side
table, so a collector or `List`-growth regression shows up here. *A codegen
change that breaks it*: the record layout, the `List` element buffer, the
string routines. *Its limits, and they are real*: the self-hosted parser
does **not** implement path assignments (`a.b[i] = v`,
`docs/self-hosting.md:40`, `examples/selfhost/parser.nx:19`), and the two
programs that use them (`examples/particles.nx`, `examples/segments.nx`)
live in `examples/`, which this gate does not read. So the coverage is
**asymmetric**: the lexer gate reads `examples/`, the parser gate reads
`fixtures/parse/`, and the self-hosted parser is never run over the
example programs. Adding the example programs to the parser gate is the
one free coverage win available in this suite - it is already compiling the
parser, so it is a one-line change to the input set (and it would fail
today, on the two path-assignment programs, which is exactly why it has
not been done).

**Gate C - the optimisation levels against each other** (`backend.rs:251`).
Reference: opt level 0. Subject: opt level 1. Input set: 14 hard-coded
programs.
*A runtime change that breaks it*: only if the optimiser's output is
observable through the runtime - a constant-folded `Str` comparison, a
folded list index, an inlined `mut` parameter whose aliasing the inliner
got wrong. The `mut` parameter is the sharp edge: it is passed by
reference (`crates/nexum-driver/tests/fixtures/run/arrays.nx`,
`compound.nx`), and inlining a function that mutates its caller's binding
is exactly the kind of change that makes the two levels disagree. *A
codegen change that breaks it*: the same, through the MIR the optimiser
works on. *Its limits*: it is a **self-consistency** gate - a defect
present at both levels is invisible to it. The `if !f.exists() { continue }`
at `backend.rs:271-273` silently drops a renamed file, and the file list
is manual, so a new run fixture does not enter the gate by itself. This is
the gate that item 3 of the recommendation fixes.

**Gate D - the IR against every target** (`backend.rs:60`).
Reference: the host assembler. Subject: the cross assembler for the
target's triple. Input set: `examples/hello.nx` only.
*A runtime change that breaks it*: any change to the inline assembly text
or the constraint strings, for any of the four (os, arch) pairs, and any
change to the runtime IR that is invalid for a target (a data-layout
mismatch, an unsupported type). *A codegen change that breaks it*: the
same. *Its limits*: **it does not catch a call whose arguments do not match
the callee's signature**, and that is a measured fact, not a guess -
see "The IR gate is weaker than its name" below.

### The IR gate is weaker than its name

`crates/nexum-toolchain/src/lib.rs:210-213` says `verify_ir` "Compiles the
IR to a throwaway object file, which runs the LLVM IR parser, verifier, and
backend (the strongest check available without standalone `opt`/`llvm-as`
binaries)". The first two clauses are true. The third one is not, in one
specific and important way.

Measured on this machine (clang 23.1.3, `clang -Wno-override-module
--target=x86_64-unknown-linux-gnu -x ir -c`):

| Module | Result |
| ------ | ------ |
| `store i32 1, i32 %x` (wrong operand type) | rejected, exit 1 |
| `br label %entry` from `entry` (entry has a predecessor) | rejected, exit 1 |
| a use that is not dominated by its definition | rejected, exit 1 |
| **`call i64 @f(ptr %p, i64 1)` where `f` is `define internal i64 @f(i64, i64)`** | **accepted, exit 0, no output** |

So a call whose argument *types* and *count* do not match the callee's
declared signature is not reported.

This is not hypothetical. `crates/nexum-runtime/src/strings.rs:44` still
has the defect that `73783d8` fixed on the read side:

```
%fd = call i64 @nexum_sys_open(ptr %path, i64 577, i64 420)
```

against

```
define internal i64 @nexum_sys_open(i64 %a0, i64 %a1, i64 %a2, i64 %a3)
```

(`crates/nexum-runtime/src/syscalls.rs:241`). Measured: `nx --target
linux-x86_64 emit-ir examples/hello.nx` then `clang --target
=x86_64-unknown-linux-gnu -x ir -c` -> exit 0, no diagnostic. The
machine code is *right* only by accident of the x86-64 calling convention -
the pointer arrives in `rdi`, which is where the `i64` parameter is read
from - and the fourth parameter is simply never read by `open`. The
`read_file` side was fixed (`crates/nexum-runtime/src/strings.rs:39-41`
casts the path and the buffer with `ptrtoint`); the `open` call site in
`write_file` was not.

The consequence for the programme: the "IR verifying for every target"
gate - the one that runs on every runner and checks the module for every
target - protects against a module that will not parse, and against a
module the assembler rejects, and against nothing else. The class of
defect the programme's own history calls dangerous (a value in the wrong
register, a pointer where an integer belongs) is exactly the class it does
not catch. The register case is caught today only because a *text* unit
test asserts on the constraint string
(`crates/nexum-runtime/src/syscalls.rs:345`) - which is the right kind of
test, and the right place for it, but which only covers one of the four
(os, arch) pairs.

### What is not covered

**The Windows-only behaviour.** The Windows path is the C library
(`crates/nexum-runtime/src/lib.rs:60-74`): `__acrt_iob_func` for the error
stream, and `fopen`/`fclose`/`fseek`/`ftell`/`fread`/`fwrite` for the
files. It *is* executed on the Windows CI runner, by `run_outputs_match_fixtures`
(the `io_roundtrip` fixture goes through `fopen`) and by the two
self-hosted gates (they read their input through `read_file`). But:

* `freestanding.rs:59` pins the *symbol list* for Windows and nothing
  else; no test asserts that the Windows file path is the C library path in
  the *code*, as opposed to the symbol list.
* The panic path on Windows writes to the error stream through
  `__acrt_iob_func(2)` (`crates/nexum-runtime/src/lib.rs:117-118`). No test
  reads any error stream, on any platform.
* There is no Windows-specific fixture. `fixtures/run/` has no file whose
  behaviour differs on Windows.

**The POSIX-only behaviour.** The system call layer is *emitted* for the
POSIX targets and *executed* only on the runners that are POSIX. The
Windows runner never executes a single `nexum_sys_*` call. Specifically:

* `nexum_sys_mmap`, `nexum_sys_munmap` and `nexum_sys_exit`
  (`crates/nexum-runtime/src/syscalls.rs:278-290`) are emitted and called
  by nothing. They are dead code today, prepared for 11a.2's allocator.
* The `open` flags are asserted nowhere: Linux's `open_write` is
  `1 | 64 | 512` and Darwin's is `1 | 512 | 1024`
  (`crates/nexum-runtime/src/syscalls.rs:70` and `:86`); the `open_read`
  flag and the `open_mode` are asserted nowhere.
* `mmap` and `munmap` numbers (Linux 9 and 11, Darwin 197 and 198) are
  asserted nowhere; `the_numbers_are_per_system` covers only read, write,
  open, close, lseek and exit (`crates/nexum-runtime/src/syscalls.rs:301-317`).
* The whole `nexum_read_file`/`nexum_write_file` pair is exercised by one
  fixture (`io_roundtrip.nx`) which is only *run* on POSIX.

**The collector's edge cases.** `gc_reclaim.nx`, `gc_stress.nx`,
`list_gc.nx` and `list_records_gc.nx` cover the common path well - and
`list_records_gc.nx` was the fixture that pinned the mark-phase defect
(`997d02e`, `docs/self-hosting.md:112-118`). What is missing:

* **The out-of-memory path.** `nexum_oom` (`crates/nexum-runtime/src/lib.rs:130`)
  prints "out of memory" and exits 1. It is reachable only if `malloc`
  returns null, and nothing in the test suite can make it do so. There is
  no fault injection and no test.
* **The watermark.** `@nexum_threshold` is 65536
  (`crates/nexum-runtime/src/lib.rs:108`) and a collection is triggered
  inside an allocation when the watermark is exceeded
  (`crates/nexum-runtime/src/lib.rs:154-156`). Every GC fixture calls
  `collect()` explicitly, so **no fixture proves that an allocation can
  trigger a collection**. That path is the one the runtime's soundness
  argument depends on.
* **A `Str` inside an array.** The collector scans the whole payload
  conservatively and strings are leaves with `scan_bytes = 0`
  (`docs/runtime-abi.md:138-139`). The fixtures cover a record with `Str`
  fields *inside a list* (`list_records_gc.nx`) and a `Str` in a local
  (`gc_stress.nx`); there is no fixture with a `Str` field inside an
  **array**, which is a value type that the collector scans wholesale.
* **`heap_used()` is only ever compared with `<` or `>`**
  (`gc_reclaim.nx`, `gc_stress.nx`, `strings.nx`). Nothing pins the exact
  accounting, so a leak of a few bytes would not be seen.
* **The root-registration rules** (`docs/runtime-abi.md:150-154`): "a
  numeric local is not registered at all" is never tested.

**The large-input path.** No fixture allocates more than a few kilobytes.
`list_records_gc.nx` builds 64 rows of 3-4 byte slices; `gc_stress.nx`
builds a 10-character string; the list fixture grows to 23 elements, i.e.
**two** reallocations of the element buffer (the initial capacity is eight
elements, `crates/nexum-runtime/src/lib.rs:224`), against a growth path
that doubles (`crates/nexum-runtime/src/lib.rs:224`). There is no fixture
that crosses the 64 KiB watermark by allocation alone, none that grows a
list through many doublings, and none that handles a multi-megabyte
buffer. The `int_wrap.nx` fixture has the largest literal in the suite.

**Other gaps, for completeness.**

* `nexum_mir::verify` (`crates/nexum-mir/src/lib.rs:772-785`) only checks
  that every basic block has a terminator (M001), and it is **never
  exercised negatively**: the `fail/` fixtures stop at the front end
  (`fixtures.rs:63` uses `run_frontend`, not `compile_file`), so M001 has
  no test.
* The optimiser (`crates/nexum-mir/src/lib.rs:1377-1391`) has no unit test;
  its only coverage is the self-consistency gate C.
* The `nx` CLI has one unit test (version comparison). `nx test`
  (`crates/nexum-driver/src/main.rs:858`) has no test, and the `nx lex` and
  `nx parse` output formats are only checked indirectly, through the
  differential gates.
* `nexum_parser::normalize` (254 lines) has no test of its own; it is
  compared against the self-hosted parser, which is agreement, not
  correctness.

### How a CI failure is diagnosed from the logs

`.github/workflows/ci.yml` runs a three-OS matrix
(`ubuntu-latest`, `macos-latest`, `windows-latest`) with `fail-fast: false`
(`:12`), so all three report. The steps are: `cargo build --workspace`
(`:26`), `cargo test --workspace` (`:28`), and a separate
`cargo test --test backend ir_verifies_for_all_targets` (`:31`). A red
*Build* step is a compile error; a red *Test* step is a test panic; a red
third step is the cross-target verification, and it is distinguishable
because it is its own step.

By test name:

* `posix_programs_need_only_the_remaining_libc` /
  `windows_still_needs_the_c_library` -> the message is
  `"<target> declares [..]"`, i.e. the exact list. The fix is in the
  runtime, not in the test. This is the gate that will change on every
  11a.2 step, by design (`freestanding.rs:10-12`).
* `run_outputs_match_fixtures` -> `assert_eq!` prints the expected and the
  actual stdout, and the two exit codes. The `.ll` and the `.exe` are left
  in `%TEMP%/nexum_tests/` (on Linux, `/tmp/nexum_tests/`), so the failing
  program can be re-run by hand: `./arrays.exe`.
* `ir_verifies_for_*` -> the panic embeds clang's stderr
  (`crates/nexum-toolchain/src/lib.rs:245-246`), which names the `.ll`
  line and the LLVM diagnostic. The `.ll` is in the same directory and can
  be re-checked by hand: `clang -x ir -c <file>.ll -o /dev/null`.
* `selfhosted_lexer_matches_rust_lexer` /
  `selfhosted_parser_matches_rust_parser` -> `assert_eq!(actual, expected)`
  prints the two streams, so the diff is at the top of the log. The failing
  input is the **last** file copied into
  `%TEMP%/nexum_tests/lexer/nexum_selfhost_input.nx` (or `.../parser/...`),
  and the compiled self-hosted program is next to it - so the failure
  reproduces by hand: copy a file to `nexum_selfhost_input.nx` in that
  directory and run `selfhost_lexer.exe`.
* `differential_opt_levels_agree` -> the panic prints
  `--- opt0 ---` and `--- opt1 ---` with the two result blobs, and the
  `.result` files are in the temp directory.
* `compile_fail_fixtures` -> prints the expected code, the codes actually
  produced, and the file.
* `compile_pass_fixtures` -> prints the file and the error lines.

Three weaknesses in the diagnosis, all of which the recommendation
addresses:

1. **A missing toolchain is indistinguishable from a pass.** Seven of the
   eight backend tests do `let Some(tc) = toolchain() else { return };`
   (`backend.rs:46`, `:54`, `:62`, `:95`, `:144`, `:200`, `:253`). On a
   machine without clang, the backend suite reports *8 passed* and proves
   nothing. CI installs clang (`:19-24`), so the CI log is safe; a
   developer's `cargo test` is not.
2. **The temp directory is shared, and two tests write the same file
   name.** `ir_verifies_for_pass_fixtures` writes `arrays.ll` (from
   `pass/arrays.nx`) and `run_outputs_match_fixtures` writes `arrays.ll`
   (from `run/arrays.nx`); the same for `records.ll`. The Rust harness
   runs the tests of one binary in parallel threads, so the two writes
   race. A flaky "IR verification failed" on `arrays.ll` or `records.ll`
   is this collision, not a defect. The helper that fixes it already
   exists - `work_dir` (`backend.rs:24-28`).
3. **No test reads the error stream**, so a change to a panic message is
   invisible in the log, and the fixtures cannot record what a program
   printed to stderr.

## The design

### The taxonomy a phase's tests should use

Four kinds, in increasing order of cost and decreasing order of
portability:

| Kind | Where | Proves | Runs on |
| ---- | ----- | ------ | ------- |
| **unit** | `#[cfg(test)] mod tests` at the bottom of the module file | a *shape* that is silent when it is wrong | everywhere, no toolchain |
| **fixture** | `tests/fixtures/{pass,fail,parse,run}/` | a *behaviour* of the language or the runtime | the host, needs clang |
| **differential** | a test that compares two implementations or two configurations | *agreement* | the host, needs clang |
| **cross-target** | a test that emits for every `TargetSpec::list()` target and verifies with `--target=` | that the module is valid *for that target* | every CI runner, needs clang |

The rule the existing tests already follow, and that the placement plan
keeps: **a unit test pins a shape; a fixture pins a behaviour; a
differential gate pins agreement; a cross-target gate pins the target
independence of the shape.** The programme's own lesson
(`specs/PROGRAMME.md`, "The lessons from the first slice") is the
extension of it: **anything that only fails on one platform needs a test
that runs everywhere.** A cross-target IR *shape* assertion is that test.

### The per-target IR shape gate (the one that matters most)

The gate that would have caught the two silent bugs of the first slice is
not a behaviour test and not a differential test - it is a **shape
assertion over the emitted IR, for every target, on every runner**. The
concretely missing assertions are:

1. **Every call site of a `nexum_sys_*` wrapper passes `i64` arguments, and
   the right number of them.** The wrapper is declared
   `@nexum_sys_open(i64 %a0, i64 %a1, i64 %a2, i64 %a3)`
   (`crates/nexum-runtime/src/syscalls.rs:241`) and `open` uses three of
   the four. A call site that passes a `ptr`, or that passes the wrong
   count, is a bug that `verify_ir` does not catch and that only fails on
   the platform that executes it. This is the assertion that would have
   caught `73783d8` - and it would catch the `write_file` defect that is
   still there at `crates/nexum-runtime/src/strings.rs:44`.
2. **The number is an immediate, in every target.** `movq $$N, %rax` on
   x86-64, `mov x8, #N` (Linux) or `mov x16, #N` (Darwin) on AArch64.
   `the_number_is_an_immediate` (`crates/nexum-runtime/src/syscalls.rs:323`)
   pins this for Linux x86-64 only; the other three (os, arch) pairs are
   not covered.
3. **The stack is aligned, in every target.** `the_stack_is_aligned_for_the_call`
   (`:334`) covers Linux x86-64 only.
4. **The arguments are pinned, in every target.**
   `the_arguments_are_pinned_to_their_registers` (`:345`) covers Linux
   x86-64 only; `aarch64_stages_its_arguments` (`:373`) covers the
   AArch64 layouts but asserts only the constraint prefix.
5. **The Darwin AArch64 service call is `svc #0x80` and the Linux one is
   `svc #0`.** Asserted in `aarch64_stages_its_arguments` (`:378`, `:383`)
   for the AArch64 pairs only.

The shape gate belongs in `crates/nexum-driver/tests/syscall_layer.rs`
because it is per-target and the *runtime* is what changes; the four
assertions that are pure text belong in the unit tests in
`crates/nexum-runtime/src/syscalls.rs`, where the crate already has them.

### The layout of a new phase's tests

```
crates/nexum-driver/tests/
  backend.rs          frozen after 11a
  fixtures.rs         frozen after 11a
  freestanding.rs     frozen after 11a
  common/mod.rs       frozen after 11a (owner: the 11a agent)
  syscall_layer.rs    11a.1
  allocator.rs        11a.2
  output.rs           11a.2
  windows_syscalls.rs 11a.2
  asm.rs              11b
  numeric.rs          11c
  memory.rs           11d
  concurrency.rs      11e
  simd.rs             11f
  linker.rs           11g
  gpu.rs              11h
  ml.rs               11i
  fixtures/
    pass/   fail/   parse/   run/     shared; disjoint file names
```

A phase that needs a new shared helper and finds that `common/mod.rs` does
not have it defines a **private** copy inside its own test file and notes
it in its report; the lead folds it into `common/mod.rs` at integration
time. That keeps the phases unblocked and keeps the file ownership
disjoint.

### The placement plan, phase by phase

**11a - the freestanding runtime.** Three slices.

*11a.1, the system call layer (landed).*
New: `crates/nexum-driver/tests/syscall_layer.rs`.
Gate: for every target in `TargetSpec::list()`, emit `examples/hello.nx`
and assert the *shape* of the system call layer: the wrappers are present,
the number is an immediate in the right register, the instruction is the
right one (`syscall` / `svc #0` / `svc #0x80`), the stack is aligned, the
arguments are pinned, and **no call site of a `nexum_sys_*` wrapper passes
a `ptr`**.
Changed: `crates/nexum-runtime/src/syscalls.rs` (extend the existing
`mod tests` to the other three (os, arch) pairs and to `mmap`/`munmap`),
`crates/nexum-runtime/src/strings.rs` (add a `#[cfg(test)] mod tests` that
asserts the file routines cast their pointers, and that the Windows variant
uses the C library).
Fixtures: none needed; the shape gate is over the IR, not over programs.

*11a.2, the allocator over `mmap`.*
New: `crates/nexum-driver/tests/allocator.rs`.
Gate: the external symbol list for the POSIX targets loses `malloc` and
`free` (the `freestanding.rs` list changes with it - that is the point of
that test); the allocator's IR uses `nexum_sys_mmap`/`nexum_sys_munmap`
with the right arguments; the collector's `free` becomes the allocator's
`munmap`.
Fixtures: `run/alloc_basic.nx` (+ `.out`), `run/alloc_oom.nx` (+ `.out`,
`.exit`, and `.err` once item 4 lands) - the out-of-memory path needs a
fault-injection hook, and the cheapest one is a runtime counter that the
allocator consults (`NEXUM_FAIL_ALLOC_AFTER`-style, but a compile-time
counter in the IR, not an environment variable: the runtime is freestanding
and has no environment).
Unit: in `crates/nexum-runtime/src/allocator.rs` (a new module, owned by
this slice) - the rounding to 16 bytes, the 32-byte header, the payload
pointer, the growth of the arena.

*11a.2, the output formatting.*
New: `crates/nexum-driver/tests/output.rs`.
Gate: the external symbol list loses `printf`, `fputs` and `fputc`; the
`print` of every type produces the *exact* bytes C's `printf` produces -
so the expected values are a table, not a comparison against the C library
(the whole point of the phase is that the C library is gone).
Fixtures: `run/format_int.nx`, `run/format_float.nx`,
`run/format_str.nx` (+ `.out` each). The float formatting is where this
gets expensive: `1.0`, `0.1`, `1e300`, `-0.0` and the shortest-round-trip
cases are the ones that decide the implementation.
Unit: the formatting is pure, so it is a **unit** test, not a fixture -
in the new module, table-driven, one row per case.

*11a.2, the Windows system call stubs.*
New: `crates/nexum-driver/tests/windows_syscalls.rs`.
Gate: the emitted IR for `windows-x86_64` contains the stub sequence, and
the external symbol list loses the six file routines and
`__acrt_iob_func`. The stubs are *generated* (Windows has no system call
instruction; the interface lives behind `ntdll`), so the gate is over the
IR text.
Fixtures: `run/win_io.nx` (+ `.out`) - the same round trip as
`io_roundtrip.nx`, so the two paths can be compared.
Note: the *execution* of the Windows stubs can only happen on a Windows
runner. The CI matrix has one, so the fixture runs there. On the POSIX
runners the fixture must not be run at all - and the mechanism for "do not
run this here" does not exist yet. Item 14's first half (a missing
toolchain must not look like a pass) is the same problem: **a gate that
cannot run must say so loudly**, not return quietly. Recommend a `skip`
that prints `SKIPPED: <reason>` so the log shows a gap instead of a pass.

**11b - inline assembly.**
New: `crates/nexum-driver/tests/asm.rs`.
Gate: an assembly block for each target assembles **for that target**
(`verify_ir_for` with the target's triple - the same mechanism as
`ir_verifies_for_all_targets`, but with the program's own assembly in it,
not `hello.nx`'s); the constraint string names the registers; a
mis-numbered operand is caught.
Fixtures: `parse/asm_*.nx` (so the self-hosted parser gate covers the new
syntax - the parser is the language's contract and the new syntax is part
of it), `pass/asm_*.nx`, `run/asm_*.nx` (+ `.out`), `fail/asm_*.nx`
(+ `# EXPECT: Txxx` for a bad constraint or a bad register).
Unit: the lowering of an assembly block to MIR, in a **new** module
`crates/nexum-mir/src/asm.rs` (this slice's own file), not in
`crates/nexum-mir/src/lib.rs` - that file is shared.

**11c - the numeric tower.**
New: `crates/nexum-driver/tests/numeric.rs`.
Gate: `f32` is a distinct type from `f64` in the IR (`float` vs `double`);
the integer widths are `i8`/`i16`/`i32`/`i64`; the saturating and
wrapping operations differ from the plain ones; the math builtins produce
**the exact bit pattern in a table** - not a comparison against libm, and
not a tolerance. A tolerance test would pass with a libm-backed
implementation, which is exactly what the phase forbids.
Fixtures: `run/f32_arith.nx`, `run/f32_compare.nx`,
`run/int_widths.nx`, `run/math_sqrt.nx`, `run/math_exp.nx`,
`run/math_trig.nx` (+ `.out` each); `fail/f32_type_mismatch.nx`.
Unit: the semantics of each operation, in a new module in `nexum-mir` or
`nexum-sema` - the same arithmetic, expressed once, checked against the
table the fixture also uses. **The table must be shared** between the unit
test and the fixture, or the two will drift; put it in the crate, not in
the test.

**11d - memory and layout.**
New: `crates/nexum-driver/tests/memory.rs`.
Gate: a pointer round-trips; a `volatile` read is not elided (the
observable proof is that a write through a volatile pointer is seen by a
later read - that needs a *behaviour* fixture, not an IR check); a packed
record has the declared layout (check the IR's struct type, and check the
behaviour); an arena releases at its release point (D009, specified since
the bootstrap, never tested).
Fixtures: `run/arena_basic.nx`, `run/arena_early_return.nx`,
`run/volatile_rw.nx`, `run/packed_record.nx` (+ `.out` each);
`fail/unsafe_borrow.nx`, `fail/pointer_type.nx` (+ `# EXPECT:`).
Unit: the record layout rules, in a new module in `nexum-codegen` or
`nexum-sema`.

**11e - concurrency.**
New: `crates/nexum-driver/tests/concurrency.rs`.
Gate: threads run and join; an atomic counter reaches the right total; a
channel delivers every message. The tests must be **deterministic** - a
test that asserts on timing is a flaky test, and a flaky test in a shared
CI matrix is worse than no test. Concretely: no sleeps, no timing
assertions, and a bounded join with a timeout so a deadlock fails rather
than hangs.
Fixtures: `run/threads_basic.nx`, `run/atomics_counter.nx`,
`run/channels_order.nx` (+ `.out`).
One decision the orchestrator has to make and pin in a test: **the
collector is single-threaded** (`docs/runtime-abi.md:118`). Either 11e
keeps it that way and a test says so, or 11e makes it safe and a test says
that. Leaving it unstated is the failure mode.

**11f - SIMD.**
New: `crates/nexum-driver/tests/simd.rs`.
Gate: the *behaviour* - a vectorised sum equals the scalar sum, bit for
bit, over a vector long enough to be interesting; a stride is honoured; a
layout is explicit. Plus a loose shape check on the IR (`<4 x float>`,
`<8 x float>`, `<16 x i8>`). Do **not** assert on LLVM intrinsic names
(`llvm.x86.avx2.*`): that pins the test to an LLVM version and to a target,
and it is the kind of assertion that fails on a runner for a reason that
has nothing to do with the feature.
Fixtures: `run/simd_sum.nx`, `run/simd_stride.nx`, `run/simd_layout.nx`
(+ `.out`).
The SIMD tests are the ones most likely to need a per-target split: a
program using `<16 x i8>` may not be legal on a target without it.

**11g - the native linker.**
New: `crates/nexum-driver/tests/linker.rs`.
Gate: a **differential** one, and the best-shaped gate in the plan: the
same program, linked by the Nexum linker and by `clang`, must produce the
same stdout, the same exit code and the same external symbol list. The
clang-linked build is the oracle, so no new reference values are needed.
Plus: a link failure is a link failure (a missing symbol is reported, not
silently dropped), and the cross-target link works (a Linux AArch64 program
links for `aarch64-unknown-linux-gnu`).
Fixtures: `run/linked_hello.nx` (+ `.out`).

**11h - the GPU path.**
New: `crates/nexum-driver/tests/gpu.rs`.
Split in two, because the two halves have different availability:
* the **shader front end** - the SPIR-V or Metal emission - is pure
  computation and runs **everywhere**, including the CI runners, which have
  no GPU;
* the **dispatch and the comparison against the CPU** needs a device. It
  must be *skipped with a message*, not failed, on a machine without one.
The skip must print `SKIPPED: no GPU` so the log shows a gap, not a pass.
Fixtures: `run/gpu_add.nx` (+ `.out`), which is also the CPU reference for
the comparison.

**11i - the ML framework.**
New: `crates/nexum-driver/tests/ml.rs`.
Gate: the autograd gradient against an **analytic** value (exact, no
tolerance: `d/dx (x^3)` at 2 is 12); a layer composition; the optimizer's
convergence (a tolerance is honest here, and only here); and the `f32`
claim at the ABI level - the tensor payload is `[N x float]`, not
`double`, which is the check that keeps 11c's work from being thrown away
by 11i.
Fixtures: `run/ml_grad.nx`, `run/ml_layers.nx`, `run/ml_optimize.nx`
(+ `.out`).

### What the gaps cost, and which to close first

Ordered by what they would have caught.

1. **The per-target IR shape gate** (11a.1). It would have caught
   `73783d8` (the pointer where an integer belongs) and it catches
   `c600dcf` (the operand numbered from zero) at the *shape* level, where
   the existing unit test only covers Linux x86-64. It is the only gate
   that looks at the *call sites* of the system call layer, and it looks at
   them on every runner, for every target. **Close it first.**
2. **The other three (os, arch) pairs in the unit tests.** `3bbb563` (the
   immediate) and `1d261bd` (the Darwin numbers) are now covered by
   `the_number_is_an_immediate` and `the_numbers_are_per_system`, but only
   for Linux x86-64 and the number table. The stack alignment, the
   register pinning, the `svc #0` vs `svc #0x80` and the AArch64 staging
   are covered for *one* pair each. A mistake in the Darwin AArch64 layout
   is still silent on every runner. **Close it second** - it is the same
   file and the same hour of work.
3. **The missing toolchain must not look like a pass** (item 14). Seven of
   the eight backend gates are vacuous without clang. This is not a
   historical bug, it is a hole in the gate suite *today*, on any
   developer machine. **Close it third** - it is a two-line change and it
   is what makes every other gate's green meaningful.
4. **The out-of-memory path and the large-input path.** Both are
   untested, and both are the paths where the collector's soundness
   argument is load-bearing. The OOM path needs a fault-injection hook
   (a counter in the allocator, not an environment variable: the runtime
   is freestanding). **Close it fourth**, with 11a.2, because the
   allocator is what fails.
5. **The stderr sidecar** (item 4). Without it, no test in the repository
   can pin a panic message, and 11a-11d all add error paths. **Close it
   with 11a**, because it is one edit to `backend.rs` and the same edit
   then serves every later phase.
6. **The Windows-only behaviour.** The CI matrix has a Windows runner, so
   the Windows path is executed; what is missing is a *Windows-specific
   fixture* and a check on the error stream. Close it with 11a.2's Windows
   stubs, when there is finally something Windows-specific to test.
7. **The collector's edge cases** (a `Str` in an array, the exact
   `heap_used()` accounting, the allocation-triggered collection). These
   are 11a.2's collector work, and each one is a `run/` fixture. The
   allocation-triggered collection is the most important: no fixture
   proves it today.

## Risks

* **The measurement behind the IR gate is version-specific.** The claim
  that clang's `-x ir -c` does not report a call-signature mismatch was
  measured with clang 23.1.3 on this machine. A different clang, or the
  `opt` binary if it is ever installed, may report it. If it does, the
  shape gate becomes belt-and-braces rather than the only defence - which
  is a good outcome, not a reason to skip it. The recommendation does not
  depend on the measurement being true forever: the shape gate is a better
  test than a verifier, because it says *what* must be true rather than
  *that nothing is wrong*.
* **The `write_file` defect may be benign on the current targets.**
  `call i64 @nexum_sys_open(ptr %path, ...)` produces the right machine
  code on x86-64 and on AArch64, because the pointer arrives in the
  register the `i64` parameter is read from. It becomes a real bug the
  moment the wrapper's signature changes (11a.2 will change it, to take a
  length, or to take the path differently), or on a target with a
  different convention. Fixing it is a one-line change in
  `crates/nexum-runtime/src/strings.rs:44` and belongs with 11a.1, but
  **it is not this document's change to make** - it is a code change, and
  this task is research.
* **Freezing `backend.rs` is a real constraint on the programme.** If a
  later phase needs a *new kind* of gate over the fixtures - a
  performance gate, a memory-leak gate - it will need to edit
  `backend.rs` or add a new file that reads the fixtures. That is fine:
  the freeze is on the *existing* files, and a new kind of gate gets a new
  file. The one thing that must not happen is two phases editing
  `backend.rs` at once.
* **The per-target IR shape gate is a string-matching test.** It will
  break when the runtime's IR text is reflowed, even if nothing is wrong.
  That is the same trade-off the existing `syscalls.rs` unit tests make
  (`assert!(ir.contains("movq $$0, %rax"), "{ir}")`), and it is the right
  trade for a layer whose failure mode is silent. The mitigation is that
  the assertion messages print the whole IR, so a reflow failure is
  obvious in the log.
* **The GPU and concurrency gates are the two most likely to be flaky.**
  Both are warned about above. A flaky test in a shared CI matrix
  poisons the whole signal, so a phase that cannot make its gate
  deterministic should not add it yet.
* **The concurrency decision (collector single-threaded or not) is not a
  test question, it is a design question.** If 11e does not decide it, the
  test will have to encode whatever the code happens to do, and then the
  test is a snapshot, not a contract.

## Open questions

Each with a recommendation.

1. **Does the self-hosted parser get run over `examples/`?** It is
   compiled already, and the two programs it would fail on are the two
   with path assignments, which it does not implement. *Recommendation*:
   yes - add `examples/` to the parser gate's input set
   (`backend.rs:222`), and add a `parse/` fixture with a path assignment
   so the self-hosted parser has something to be compared against once it
   implements them. Until then, the two path-assignment examples have to be
   excluded from the gate - and the exclusion should be *listed in the
   test*, with a comment saying why, rather than being an accident of
   which directory the gate happens to read. That is the difference between
   a known gap and an unknown one.
2. **Does the run fixture set get a `.err` sidecar, or a separate
   `fail_runtime/` kind?** *Recommendation*: the sidecar. A separate
   directory would mean a second loop over the fixtures and a second
   auto-discovery rule; the sidecar is one `if` in the existing loop and
   it keeps one fixture in one place.
3. **Should the opt-level gate run over the `run/` fixtures, or over
   everything?** *Recommendation*: over the `run/` fixtures plus the
   examples, i.e. exactly the union it has today but discovered rather
   than listed. Running it over the `pass/` fixtures too would add the
   compile-only programs, which is cheap and worth it.
4. **Is the collector single-threaded after 11e?** *Recommendation*: the
   orchestrator decides, and the decision goes in
   `docs/language-reference/decisions-gc.md` next to the tracing model.
   A test that pins the current behaviour without a decision behind it
   will be a snapshot.
5. **What is the fault-injection mechanism for the out-of-memory path?**
   *Recommendation*: a counter inside the allocator, in the IR, that a
   test-only fixture can arm through a builtin. Not an environment
   variable - the runtime is freestanding and has no environment - and not
   a compile-time flag, because the flag would have to be per-program and
   the allocator is in the runtime.
6. **Does the shape gate live in the integration test or in the crate?**
   *Recommendation*: both, in the same way the existing tests already
   split it. The pure text assertions (the immediate, the alignment, the
   constraint string) belong in the unit tests of
   `crates/nexum-runtime/src/syscalls.rs`; the *call site* assertions
   (no `ptr` at a `nexum_sys_*` call site) belong in the integration test,
   because they are about the module as a whole, for a target, and they
   need the codegen.
7. **Who owns `crates/nexum-driver/tests/common/mod.rs` from 11b on?**
   *Recommendation*: nobody. It is frozen after 11a. A phase that needs a
   new shared helper defines it privately in its own file and says so in
   its report; the lead folds it in at integration time. The alternative -
   a shared `harness/mod.rs` with a second owner - buys a second file to
   coordinate and is not worth it.
8. **Should the `fail/` fixture check become "equals" instead of
   "contains"?** *Recommendation*: no, not now. "Contains" lets one file
   trip several checks, which is the right default for a growing language.
   When a phase needs the exact diagnostic set, it should assert that in
   its own test file against its own fixture, where the expectation is
   explicit.
