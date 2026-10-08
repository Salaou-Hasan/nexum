# Changelog

## v0.4.4

### Fixed: a short-circuit operand containing arithmetic produced IR clang rejected
`xs[i - 1] > xs[i] and xs[i] > xs[i]` is not straight-line code. Each
checked subtraction opens its own overflow diamond, so the right-hand
operand's value ends up in the last of those blocks -- not in the block
the merge opened for it. The merge's `phi` named the block the operand
*started* in, which was never a predecessor of the merge at all, and
clang rejected the whole module:

    error: invalid LLVM IR input: PHI node entries do not match predecessors!

Nothing in the suite covered the shape, so four benchmark workloads
(`sortint`, `sortstr`, `strscan`, `textstat`) had silently stopped
building, and any program combining `and`/`or` with arithmetic, a
subscript, or a nested `and`/`or` was rejected the same way. The
`a if c else b` conditional had the identical defect in both arms.

The emitter now tracks the block it is writing into and closes it when
an operand turns out not to have stayed straight-line, so the `phi`
names the block that actually holds the value. Pinned by
`every_phi_names_a_real_predecessor`, which checks the invariant
directly rather than one program's shape, and by an end-to-end test
that compiles and runs the shape.

### Editor: indentation tracks Python's system instead of hand-rolled rules
`language-configuration.json` now mirrors VS Code's built-in Python
config rule-for-rule: block-opener `onEnter` rule, `elif`/`else`
dedent, off-side folding, `#region` markers. It cannot literally be
Python's file -- Python's patterns don't know `fn`, `type` or `impl`,
and its string-prefix pairs don't exist in NX -- so the keywords, the
comment-tolerant trailing colon, and the blank-line rule stay NX's
own; everything else tracks upstream. No behavior changes beyond the
`#region` folding markers.

## v0.4.3

### Error highlighter: one root cause, not sixteen squiggles
A file importing itself (`import test` inside `test.nx`) produced 16
diagnostics -- the file was checked once per nesting level with
different bindings, so contradictory pairs ("undefined variable 'do'"
*and* "module 'test' has no member 'double'") and exact duplicates
piled onto one root cause. The checker now treats a module that fails
to load (circular, missing, unparseable, or checked with errors) as a
failed load: the load error stands alone, every requested name binds
`Unknown` silently, repeat imports short-circuit without re-running
the submodule check, and a poisoned receiver stays silent at method
calls while a merely-dynamic one still errors by the Stage 4 rule.
The same program now reports exactly the two root errors. Pinned by
three checker tests (exact error lists, plus guards that healthy
modules and dynamic receivers still error); the extension additionally
collapses identical diagnostics client-side.

### CI: two red checks fixed, both harness-level
- `test (windows)`: `one_example_still_matches_its_snapshot` failed
  only on Windows CI because the committed snapshot checks out CRLF
  there (core.autocrlf) while the dump always emits `\n`; every LF
  checkout stayed green, which is why it never reproduced locally.
  Reproduced here by converting both files to CRLF, then fixed by
  comparing line-ending-insensitively in `dump_stability.rs` (the
  lexer already normalizes endings, so spans are unaffected).
- `native (ubuntu)` memcheck: valgrind never ran -- the installed
  valgrind rejects `--errors-for-leaks=no` ("Unknown option") and
  died before touching any example, which read as rc=1 on the first
  example. Replaced with `--leak-check=no`, which expresses the same
  documented intent (NX never frees by design) on every valgrind.
  Whether Task 0's last box closes now depends on the next green run.

### Editor: Enter on a blank line keeps that line's indent
Pressing Enter on a blank line below a block used to land back inside
the block: with no `onEnterRules` entry matching, VS Code falls back
to a backward walk that skips blank lines, finds the nearest code
line, and inherits *its* indent. A second `onEnterRules` entry now
matches whitespace-only lines with `indentAction: none`, which keeps
the blank line's own indent and bypasses the walk entirely -- outside
a block you stay at column 0, inside a block you stay at block level,
and block openers still indent as before. Proven with
`tools/enter-indent-sim.ps1`, which mirrors VS Code's Enter pipeline
and fails 3 of 5 cases on the old config.

## v0.4.2

### Editor: Python-like indentation and error squiggles
- `editors/vscode-nexum/language-configuration.json`: the indent
  pattern now matches block openers only (`fn type impl if elif else
  for while`), so `# comment:` lines and value lines ending in a colon
  no longer mint phantom indent levels and guide columns; `folding:
  { offSide: true }` declares blank lines belong to the block above,
  so guides and folding stop at scope boundaries. Highlighting and
  auto-indent for real blocks are unchanged.
- New dependency-free `extension.js`: `nx check` diagnostics as editor
  squiggles on save, on tab switch, and via **Nexum: Check current
  file** (`nexum.executablePath`, `nexum.checkOnSave` settings).
  Parser proven against real compiler output; `node --check` runs in CI.

### input() takes numbers, int()/float() convert
- `input()` prompts may be `Str`, `Int` or `Float` (printed the way
  `print` prints them); the answer is still always `Str`.
- New `int(x)` / `float(x)` builtins (direct and sugar spellings):
  identity/widening for the numeric side, truncation toward zero for
  `Float`-to-`Int`, runtime parsing for `Str` (integer/decimal syntax,
  blanks allowed, hex rejected for floats since NX has none anywhere).
  Bad strings are runtime errors naming the value; wrong types and
  arities are type errors. Grammar 4.3 and the error table own the
  semantics; parsing lives once in `runtime.ll` (`@nx_to_int`,
  `@nx_to_float`).

### ARGONE D: fifth box proven, task complete
- `arith_plan` in `nx-hir` (rule plus operator in, emission out; no
  operand types in the signature, so recomputation inside the table is
  unrepresentable), `emit_arith` in `nx-codegen` (the only consulted
  table on the scalar path; the remaining type-to-rule derivation is
  one marked site awaiting task G), `arith_plan` table tests (7), and
  `trap_rule_drives_checked_emission` in `nx-codegen` (lowered `Trap`
  counts versus emitted checked intrinsics per operator, plus a float
  control; verified red under a sabotaged plan).
- `BuiltinOp::ToInt/ToFloat` in the HIR model with lowering, verifier
  and dump coverage for the new builtins.

## Unreleased

### ARGONE D: HIR lowering, verifier, dump-hir (4 of 5 boxes)
New crate `nx-hir`: a name-free, fully typed HIR lowered from the
checked AST plus checker tables. Bindings are slots (params first,
then first-bind order; `self` is slot 0); declared entities are IDs
in sorted-name order. Every expression carries `ty: HTy` (mandatory,
so untyped nodes are unrepresentable) and every operator node carries
its decided rule -- integer arithmetic records `ArithRule::Trap`,
power records `PowRule::Saturate`, string concat, membership, index,
slice, iteration, copy and delete rules likewise. Rules come from
calling the checker's `arith_result`, never a second matrix.
`String` survives only as `Str` literals, the interned runtime-name
table, and the inert `diag` sidecar (verified by grep plus the
verifier's strip test: erasing every diagnostic name changes no
verdict). Lowering verifies its own output (phase 4); a lowering bug
fails at the producing span, and `nx dump-hir` prints the verified
HIR as line S-expressions with a snapshot test on
`examples/control.nx`.
Tests: 48 in `nx-hir` (28 lowering decisions incl. trapping integer
arithmetic, mixed-float promotion, eager from-import snapshots,
`mut self` write-back only where the receiver has storage; 16
verifier rules with one rejecting case each; corpus-wide lowering of
all 42 shipped programs; dump-twice-identical plus snapshot). Full
workspace suite green, `tools/verify.ps1` 14/14, gate self-tests
10/10 (including a CRLF-tolerance fix to two Evidence regexes in
`tools/argone-gate-tests.ps1`, matching the `\r?\n` handling the
neighboring tests already had).
Open half of the fifth box: overflow lives in HIR and lowering is
proven to record it, but the proof that the *backend* honors the rule
without re-deriving it needs task G, since the backend still consumes
the AST.

### ARGONE D: HIR design recorded
`docs/architecture/hir.md` specifies the high-level IR before any code
exists: slots and ID tables, the per-node decided-rule table (integer
overflow lives in HIR as `ArithRule::Trap`), twelve verifier rules with
accept/reject sketches, the dump-hir stability contract, lowering
phases, the backend may/must-not contract, and empty extension points
for ownership, closures and `dyn`. Design only, no crate yet -- the
build turn implements it against this document.

### ARGONE Task 0: the remaining divergences are closed
- i64::MIN is spellable: a unary minus in front of exactly 2^63 folds
  to MIN in every radix spelling (`-9223372036854775808`,
  `-0x8000000000000000`). The same change closes a silent-misparse hole:
  radix overflow passed bare digits downstream, so `0x8000000000000000`
  read as decimal 8e15; the prefix now goes back on and the parser
  reports the range error.
- Unary negation traps on overflow on both paths (`-MIN` is `0 - MIN`).
- `del d[missing]` is a runtime error ("key not found"), matching
  `d[missing]` reads; `del` on a name unbinds (later uses are statically
  undefined, so the runtime's rebind-to-`None` is unobservable).
- Compound assignment evaluates the target before the value, each once
  (`xs[idx()] += val()` runs index, then value).
- Comprehensions over strings yield characters, like `for c in s`
  (the comprehension had its own byte loop).
- `and`/`or` short-circuit is tested on both branches (was zero coverage).
- `examples/boundaries.nx` pins the decided rules as a permanent
  differential fixture (14 examples in `verify.ps1` and CI).
- `docs/grammar.md` owns each rule outright: R1 names unary minus, new
  R6 states float printing (measured edges: fixed `1e-12..1e16`), and
  sections 1.3/2.5/2.6 state the MIN spelling, evaluation order and
  deletion errors. A stale "stepped string slices are broken" test note
  is replaced with the values (the overflow was fixed earlier).
### Test counts
235 unit tests plus 186 native execution tests. `cargo test --workspace`
runs everything; `tools/verify.ps1` reports 14/14.

## v0.4.1

### `nx run` no longer reports a built executable as missing
`nx run main.nx` (and `nx build main.nx --run`) built `main.exe` and then
failed with `nx: cannot run main.exe: program not found` -- reproduced
exactly. A bare exe name does not resolve through the current directory
on Windows, so the run path now spawns by absolute path. Absolute `-o`
paths pass through unchanged. Pinned by `exe_spawns_by_absolute_path`.

### `input()`: read a line from stdin
The third ambient builtin, alongside `len` and `push`. `input()` reads a
line; `input(prompt)` prints the prompt verbatim (no newline, flushed)
first. The answer is always a `Str` with the newline -- and a carriage
return before it -- stripped. EOF with no characters is a runtime error
("unexpected end of input"): there are no exceptions to catch it with,
and an empty string would read as a value. A function calling `input()`
is never memoized. Taking this is a deliberate roadmap resumption: the
ARGONE lock stays in force for everything else, and this entry is the
record that the exception was explicit rather than drift.
### Test counts
232 unit tests plus 179 native execution tests. `cargo test --workspace`
runs everything; `tools/verify.ps1` reports 13/13.

## v0.4.0

The first release since v0.3.0. The work below was labelled v0.4.0 through
v0.4.4 in draft and none of those labels was ever published, so it all
ships as one release. Sections are newest first; the draft labels are
kept as headings so the cross-references between entries still resolve.

### The interpreter is gone. Nexum compiles ahead-of-time, and that is all.
`compiler/nx-interp` is deleted -- 3,575 lines and 96 tests -- along with every
execution path that used it. There is now exactly one execution model:
    source -> lexer -> parser -> type check -> LLVM IR -> clang -O2 -> native executable
**CLI**
- `nx <file.nx>` builds a native executable and runs it; it used to interpret
- `nx --run <file.nx>` **removed**. It only ever meant "interpret".
- `nx run <file.nx> [-o <out>]` accepts `-o`
- `nx build` creates the `-o` target directory if it does not exist, so
  `nx build a.nx -o build\release\a.exe` no longer fails inside the linker
  with a message that never mentions nx
- without `-o`, the executable lands beside its `.nx` source
This is a breaking change. Anything scripting `nx <file>` for its speed now
pays a compile, and `--run` must become `run`.
### Replacing the oracle
Deleting the interpreter removes the only part of the compiler that ever
*executed* a Nexum program: `nx-codegen` asserts on emitted IR text and never
runs anything, and nothing else invoked clang. So `compiler/nx-e2e` is a new
crate with a compile-and-run harness, and 165 tests now execute real programs
against expected values.
Expected-value tests are a stronger oracle than a differential against a
second engine. A differential can only tell you two implementations differ,
never which is right -- and it reports "ok" when both are wrong. That is not
hypothetical: the old three-way harness reported agreement on programs where
the interpreter and the backend both returned a stale memoized value.
Converting the suite immediately corrected three of my own expectations.
Comparing a `P` to a `Q` is rejected rather than answered `false`;
destructuring a non-tuple is a runtime rejection the checker does not catch;
and my compound-assignment arithmetic was wrong. None of those could have
been found by diffing two engines.
### A heap buffer overflow in string slicing
`print("abcdef"[::2])` returned heap garbage, and a 10-character string
sliced with a step wrote past the end of its buffer. Two defects, both in
`nx_slice`'s string path:
1. the element count was `floor(cap/step)` where it must be `ceil`, so the
   shortfall only appeared when `cap` did not divide evenly -- which is why
   `"abcdef"[::2]` looked plausible and `"abcde"[::2]` did not
2. the destination offset advanced by `step` instead of 1, leaving every odd
   byte uninitialised *and* running off the end. The comment above it read
   "packed from zero, not mirrored from the source offset" and then mirrored
   the source offset.
The list path was unaffected because it allocates then pushes, which is why
slicing a list always looked right. Five regression tests pin it.
### ASan does not currently cover the hand-written runtime
The binaries import `clang_rt.asan_dynamic-x86_64.dll`, and ASan works on
this machine -- a control C program trips it. Yet it reports **nothing** for
the overflow above. So a green ASan run is not evidence that the runtime is
memory-safe, which is the opposite of what the CI step implies.
Measured, and now precisely explained: when clang is handed a `.ll` file,
`-fsanitize=address` links the ASan runtime but emits **zero** load/store
checks into the output. A minimal `.ll` with an unfoldable heap OOB
(`volatile` store+load at a runtime index into `malloc(3)`) exits silently
at both `-O0` and `-O2`, with and without `datalayout`/`target triple`,
while the identical C program trips `heap-buffer-overflow ... WRITE of size
1`. Symbol check confirms it: the `.ll` binary contains `__asan_init` and
no `__asan_report_*` at all; the C binary has them. The only live ASan
machinery in an NX binary is the libc interceptors -- which is why a
garbage string trips at `printf` time (`%.*s` argument validation) while
the OOB writes that produced it stay invisible. Reintroducing the old
`nx_slice` sizing bug reproduces exactly that shape: silent writes, abort
only at print.
Two consequences. First, the CI `asan check` step can only catch
interceptor-visible libc misuse, never a heap/stack/global OOB inside NX
code; real coverage of the hand-written IR needs something that works on
uninstrumented binaries (Valgrind on Linux) or an instrumenting middle
step this toolchain does not ship (`opt` is absent here). Second, on
Windows an ASan binary does not start at all unless the clang runtime dir
(`...\LLVM\lib\clang\23\lib\windows`) is on `PATH` -- without it the exit
is `0xC0000135`, not a sanitizer report. The Task 0 gate item ("ASan must
cover hand-written runtime IR, not just Rust code") stays open, and now
says what would close it.
### tools/verify.ps1
The differential is now two-way (native, then native with `NX_NOUNBOX=1`).
Four defects fixed: it ran a stale copy of `nx` from `$CARGO_HOME\bin`, so it
could report all-green against a binary from many commits ago; it dropped
blank lines from both sides before comparing; it used `Compare-Object`, a set
comparison, while claiming byte-identical output; and it checked no exit code,
so a program failing identically on both paths compared equal. The corpus is
now an explicit 13-entry list including `examples/modules/`, which a
non-recursive glob had skipped entirely -- module imports had no CI coverage at
all.
The dangerous version of this edit is worth recording: deleting `$interp`
while leaving `Compare-Object` pointed at it would have printed `ok 13/13`
while comparing nothing.
### nx-ast / nx-codegen / nx-types: dangling comments
Fourteen source comments referred to the deleted interpreter. One was actively
harmful. `nx-codegen`'s `Pow` arm carried:
    // which panics or saturates exactly as the interpreter does.
It does not. That path calls `@nx_ipow` directly, bypassing every check in
`@nx_pow`, which is why `2 ** 100` returned 0 and `2 ** -1` returned 1. A
comment asserting a guarantee the code does not make is how a bug survives
review.
### docs/grammar.md: the specification owns runtime meaning
The authority table read:
    | Runtime meaning | compiler/nx-codegen/src/runtime.ll |
The *language specification named a backend as the authority for meaning*.
That is the root cause of every divergence recorded in v0.4.4: each rule was
written once in Rust and once in LLVM IR with no owner, and the two drifted.
It now names this document, with `arith_result` as the single checker-side
owner of operator result types.
Five runtime rules are now written down as testable rules R1-R5: integer
overflow traps; `**` saturates and rejects a negative exponent; a string is a
sequence of characters; there is no loop or recursion limit; execution is
single-threaded and in program order.
**R1, R2 and R3 were specified first and are enforced below.** The boxed path
had them first; the unboxed path -- the default, since `NX_NOUNBOX` is normally
unset -- did not, so the guarantees did not hold for ordinary programs. The
`Integer arithmetic`, `Slicing a list of lists` and `Strings are addressed by
character` sections record the enforcement; the specification led the runtime
by design, and those are Argone Task 0.
### `parallel:` is removed. There is no concurrency in Nexum.
Not deprecated -- gone, with every trace of it:
| Removed | |
|---|---|
| `parallel:` keyword | lexer, AST variant, parser rule, type-checker scope tracking |
| `parallel:` tests | 6 in `nx-e2e`, 5 in `nx-codegen`, 4 in `nx-types`, 1 in `nx-parser`, 3 in `nx-ir` |
| scheduler | `nx_ir::{task_summaries, partition, conflicts}`, `stmt_summary` |
| codegen | `emit_parallel`, `emit_threaded_batch`, `run_pool`, `collect_outer_reads`, `collect_outer_reads_expr`, `collect_expr_reads` |
| runtime | `nx_pool_claim`, `nx_pool_worker`, `%NxPool` from `runtime.ll` |
| platform shims | `runtime_threads_win.ll`, `runtime_threads_unix.ll`, the `THREADS` include |
| corpus | `examples/parallel.nx`, `bench/workloads/parwork.{nx,rs}` |
**Why.** `parallel:` asked the scheduler to prove race-freedom statically and
promised output in task order whatever the scheduler did. The proof had holes,
and I found one while fixing an unrelated bug.
A name bound inside a top-level `parallel:` block is a module global in the
emitted code -- `nx-codegen`'s `collect_module_globals` says so explicitly and
declares it. But `nx-ir`'s `top_assigned`, which feeds the *dependency* analysis,
only recursed into `if` and `while`. So `is_shared("t")` was false for a name
that was in fact shared, the scheduler saw no conflict, and `t = 0`, the loop
accumulating into `t`, and `x = t` were emitted as three concurrent tasks.
Two analyses of the same fact, disagreeing. The program compiled, ran, and
printed the right answer whenever the race was won:
    n = 100
    x = 0
    y = 0
    parallel:
        t = 0
        for i in 0..n:
            t = t + i
        x = t
        ...
300 consecutive runs after the fix: 300x `4950/9900`. Before it, one run in
roughly six printed `0`, and `parallel_tasks_do_not_share_loop_variables` -- a
test whose own comment calls it a data race detector -- passed about four times
in five.
An automatic-parallelism feature is only as good as its race-freedom proof, and
a sound one needs ownership and alias analysis, which is Stage 6. Until that
exists, the honest options are a correct-but-unusable feature or no feature.
This was the second one.
`nx_spin_lock`/`nx_spin_unlock` stay: memoization uses them.
R5 in `docs/grammar.md` survives in rewritten form -- execution is
single-threaded and in program order -- because a single-threaded model is the
one guarantee a compiler can make without a proof obligation.
The cost is real: `parwork` was the only benchmark where NX beat Rust (0.59x).
Correctness outranks that row.
### Integer arithmetic has a defined answer or none at all
R1 and R2 were written down and enforced only on the boxed path. Since
`NX_NOUNBOX` is normally unset, the unboxed path is the default, so the
guarantees did not hold for ordinary programs.
    x = -9223372036854775807 - 1
    x / -1     -> garbage    (now: integer overflow)
    x // -1    -> garbage    (now: integer overflow)
    x % -1     -> 0          (now: integer overflow)
    2 ** -1    -> 1          (now: traps)
    2 ** 100   -> 0          (now: 9223372036854775807)
`sdiv i64 INT64_MIN, -1` is *poison* in LLVM, not a wrapped value: the quotient
does not exist, so leaving it unchecked was undefined behaviour rather than a
wrong answer. `nx_div_i64`, `nx_floordiv_i64` and `nx_mod_i64` now guard it.
`nx_ipow` inherited none of `nx_pow`'s R2 checks and now mirrors them exactly,
including answering 0, 1 and -1 by parity instead of clamping them.
`+`, `-` and `*` on two `Int`s now trap on overflow, on both paths, via
`llvm.{sadd,ssub,smul}.with.overflow.i64`. That is one operation returning the
value and the flag, LLVM lowers it to the same instruction plus overflow flags,
and it disappears entirely wherever LLVM can prove the range -- most loop
counters and index arithmetic. Python sign conventions are untouched:
`-7 // 2 == -4`, `-7 % 3 == 2`.
Two codegen tests asserted the literal text `add i64` / `mul i64`. They were
guarding "the operands never get boxed", which the intrinsic preserves; the
assertions now name the intrinsic and keep the two `!@nx_add` / `!@nx_mul`
checks that state the real invariant.
### Slicing a list of lists no longer writes through to the parent
    xs = [[1, 2], [3, 4], [5, 6]]
    ys = xs[0:2]
    ys[0][0] = 99
    print(xs)      # was [[99, 2], [3, 4], [5, 6]]
`nx_slice`'s list path pushed each element as it found it, so a list of lists
shared its inner lists with the parent. A plain bind, a literal and a function
argument all deep-copied, so `ys = xs` and `ys = xs[0:2]` meant different
things and only one of them was what `docs/grammar.md` says.
It now clones each element through the existing `nx_clone`, which returns Int,
Float, Bool, Str and Func unchanged and deep-copies List, Dict and Record. One
call per element, no new blocks in the loop body.
Worth recording how this one went. The first attempt branched on the element's
tag inside the loop body to avoid the call for scalars. That needed new basic
blocks, which meant the `scan` loop's `phi` had to name the new back-edge, and
LLVM rejected the result with `expected instruction opcode` pointing at a
label. Three wrong diagnoses followed: a duplicate SSA name (`%c` was already
taken by the string path in the same function -- that one was real), then a
suspicion that `push:`/`adv:` were reserved words, then a bisect whose string
replacements never matched because the pattern had LF and the file had CRLF,
so every "variant" was really the original file with only the `phi` changed.
Reading the whole function first, and keeping the change to three lines, is
what worked. Four failures to one correct edit.
`slice_copies_rather_than_aliases` passed throughout, because it only reaches
top-level `Int` elements -- which a shallow slice already handled. The nested
case is now `slice_copies_nested_containers_too`.
Also verified: the `1e14 -> 1` float-printing bug in my notes does not
reproduce. `print(1e14)` is `100000000000000`, and `1e-4 .. 1e15` all print in
the documented fixed notation with `%.17g` taking over outside it.
### `continue` in a `for` loop no longer hangs
    t = 0
    for i in 0..5:
        if i == 2:
            continue
        t = t + i
    print(t)      # hung forever; now 8
`continue` branched to the loop's *condition*, which is right for `while` and
wrong for `for`: a `for` owns its induction variable, so re-testing the
condition re-tests the same index and the loop never advances. It never
advanced at all, so it did not terminate.
Both `for` arms now branch to a latch that runs the increment and then goes
back to the condition, and the normal fall-through goes through the same latch
so there is one increment site rather than two.
`continue_skips_the_rest_of_the_body_in_a_while_loop` incremented by hand
*before* its `continue`, which is what the rule requires in a `while` loop --
and it meant the suite had no `for`-loop `continue` test at all, which is how
this survived. Covered now for the range form, the descending form, list
iteration and string iteration, plus innermost-loop targeting and shadow
restore on the `continue` path.
### `xs[1:3] = 9` is rejected instead of silently doing nothing
    xs = [1, 2, 3, 4, 5]
    xs[1:3] = 9
    print(xs)      # printed [1, 2, 3, 4, 5], as though it had worked
`target_from_expr` had a catch-all that produced `Target::Name("")` for
anything that was not a name, element or field, so a slice target became an
assignment to a variable with an empty name. Its own comment claimed the case
was "unreachable through the grammar", which is what made it safe to leave
alone: a slice *is* an `Expr`, and the parser reaches this function having just
seen `=`.
`target_from_expr` now returns a `Result` and names the offending shape:
    cannot assign to a slice; a target is a name, an element, or a field
    cannot assign to a call; a target is a name, an element, or a field
This is a rejection rather than an implementation. `docs/grammar.md` 2.5 says
"A target is a name, an element, or a field", and Python and Rust reject slice
assignment too. Slice assignment would be a language addition with real
semantics -- length changes, element-type unification -- and it belongs in the
spec before it belongs in the backend.
Still not supported, and now visible rather than silent: `xs[0], xs[1] = 7, 8`.
The grammar has `target_list ::= target ("," target)*` but the parser only
accepts a single target before the comma.
### Strings are addressed by character, and there is a way to spell them in ASCII
R3, the last of the correctness bugs:
    print(len("héllo"))        was 6, now 5
    print("héllo"[1])          was one byte of é, now é
    print("日本語"[1])          was one byte, now 本
    for c in "日本語": ...     was nine broken bytes, now three characters
Three helpers in the runtime find character boundaries. A UTF-8 character
starts wherever a byte is not `10xxxxxx`, and its width comes from the lead
byte, so none of them decodes a code point:
- `nx_utf8_count` counts non-continuation bytes
- `nx_utf8_offset` walks characters to a byte offset
- `nx_utf8_charlen` reads the width from the lead byte
`nx_len`, `nx_index` and `nx_slice` route through them, and iteration follows
`nx_index` for free. `nx_slice` now sizes its buffer in bytes from the span
between the two bounds, because sizing it in characters -- which is what it
did -- is up to 4x too small for anything non-ASCII.
The cost is honest: `len` is O(n) and `s[i]` is O(i), where a byte-addressed
string is O(1) for both. Caching a character count next to the byte length is
the fix, and it wants a string header the way a list already has one.
### `\uXXXX`, `\UXXXXXXXX`, `\u{...}`
The escapes exist because of the cost above being unmeasurable otherwise. A
literal non-ASCII glyph in a source file is UTF-8, and a diff, a terminal with
the wrong code page, and a patch applied as bytes all decode it differently --
and none of them reports an error when they disagree, they report mojibake.
`\u65e5` cannot do that, because its six bytes say what they are.
All three spellings are Python's. A surrogate or an out-of-range value is a lex
error rather than a string holding something no UTF-8 encoder will accept.
I hit this while writing the R3 tests: raw glyphs in the test sources came back
through the console as CJK. The right response was to make the test sources
ASCII, not to strip every non-ASCII byte from the repository -- an em-dash in
a comment is not the problem and was never implicated. Every source file under
`compiler/` and `tools/` is ASCII apart from eleven pre-existing em-dashes,
which stay.
### Test counts
222 unit tests plus 173 native execution tests. `cargo test --workspace` runs
everything; `tools/verify.ps1` reports 13/13.
### Still open

Correctness. The nine bugs from the audit are closed, and so are the four
that were left after it: a method call through a field now resolves through
the field's declared type (with `mut self` writing back through it),
`xs[0], xs[1] = 7, 8` assigns positionally, `"" in s` is true, and a needle
as long as the haystack matches. What is left, measured rather than
assumed:

- a method call on a parameter the checker cannot resolve. NX has no type
  annotations (grammar.md 4.1), so a parameter takes its type from how the
  body uses it, and a body that only forwards `p.get()` resolves nothing.
  Annotating the parameter would be a language addition, not a fix.

Performance, stated rather than hidden:

- a bind deep-copies a container and argument passing deep-copies again, so
  container-heavy code runs 3x to 22x slower than the Rust equivalent measured
  in `bench/`. Copy-on-write would close most of that and needs reference
  counting, which is Stage 6.
- `len` on a string is O(n) and `s[i]` is O(i), because a character count is
  not cached beside the byte length. Caching it wants a string header the way
  a list already has one.
- malloc-ed memory lives for the process lifetime. There is no collector and
  no arena yet, so a program that churns grows without bound.
### Draft v0.4.4

#### ARGONE revised: MLIR removed as a prerequisite, and the audit found something worse

A ten-agent parallel investigation, reconciled against the tree. Full
report in `docs/architecture/audit.md`; plan in `docs/ARGONE.md`; gate in
`docs/ARGONE-STATUS.md`.

**MLIR is no longer a prerequisite, dependency, or completion criterion.**
Tasks G-K previously described a Nexum MLIR dialect and an MLIR lowering
pipeline. They are replaced by a backend boundary trait, structured LLVM
generation through the LLVM C API, and pipeline integration. MLIR may be
added later if a concrete requirement justifies it. JIT, ORC and LLJIT are
permanently out of scope.

#### Corrections to v0.4.3, on the record

Three claims in the previous entry were wrong.

- **"`static.crates.io` is unreachable."** It is reachable. A scratch crate
  resolves and downloads (`cargo fetch`, exit 0, fetched `unicode-width
  v0.2.2`). The earlier probe fetched the CDN *root*, which returns 403
  because it serves no directory index, and I read that as blocked.
- **"No C++ toolchain."** MSVC 14.44.35207 is installed at
  `C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\`.
  It needs `vcvars64.bat` to enter the environment, not installation.
- **"No LLVM C API."** `LLVM-C.dll` (71 MB) and `LLVM-C.lib` (293 KB) are both
  present under `C:\Program Files\LLVM`. There are no `llvm-c` headers,
  which does not matter for Rust FFI.

Only one of the three blockers was real: MLIR tools genuinely do not exist
on this machine. That is no longer a blocker.

The AST-arm duplication figure (100 `Stmt` / 133 `Expr`) was also wrong.
Measured: 146 and 118 — and the total was the wrong target anyway.

#### Eleven verified interpreter/native divergences

Every one executed on this host.

| Program | Interpreter | Native |
| --- | --- | --- |
| `9223372036854775807 + 1` | error `integer overflow` | `-9223372036854775808` |
| `9223372036854775807 * 2` | error | `-2` |
| `2 ** 100` | saturates to `i64::MAX` | `0` |
| `2 ** -1` | error, names the fix | `1` |
| `"" in "hello"` | `true` | `false` |
| `"hello" in "hello"` | `true` | `false` |
| `len("héllo")` | `5` | `6` |
| `"héllo"[1]` | `é` | invalid byte |
| `"héllo"[0:2]` | `hé` | `h` + invalid byte |
| `1.0e15` | `1000000000000000.0` | `1000000000000000` |
| `del d["z"]` (missing) | error | silently no-ops |

The three-way differential is green because no example or benchmark reaches
these cases: `listsum` peaks near 4e9, `fib` stops at 34, `syntax` tops out
at `2**10`. The harness is sound; the corpus is empty exactly where the
compiler is wrong.

**Root cause is architectural.** `docs/grammar.md:12` lists
`compiler/nx-codegen/src/runtime.ll` as the authority for "Runtime meaning".
The specification points at a backend. The interpreter is a cross-check on a
backend, not the reverse. Every divergence exists because a rule was written
once in Rust and once in LLVM IR with no owner.

This is now **Task 0**, gating every representation change: building a typed
HIR while the backend silently wraps integers produces two wrong
implementations instead of one.

#### A bug the differential cannot see by construction

`nx_ir::expr` has a `_ => {}` arm that silently drops `Dict`, `Slice`,
`IfExpr` and `Comprehension`, so a function reading a global only inside a
dict literal is memoized. Confirmed by execution and by `--emit-ir` showing
`nx_memo_put`/`nx_memo_get` for the function:

```nexum
g = 5
fn f(k):
    d = {"a": g}
    return d["a"]
print(f(0))   # 5
g2()          # writes g = 10
print(f(0))   # 5 -- stale; should be 10
```

Both engines produce the stale `5` because both call the same
`memoizable`. Interpreter/native comparison is structurally incapable of
catching this; only a property-based test would.

A related claim — that memoization caches a freed container pointer — was
**not reproduced**: the function is not memoized in that shape, and an
AddressSanitizer build reports no error.

#### Harness defects found

`tools/verify.ps1`, the oracle:

- runs `%USERPROFILE%\.cargo\bin\nx.exe`, a **stale installed binary**, so it
  can report all-green against a broken checkout
- drops blank lines and any line starting with whitespace plus `+`
- uses `Compare-Object`, a set comparison, while the header and changelog
  claim "byte-identical"
- checks no exit code on any of its three runs, so a program failing
  identically on all three compares equal and reports `ok`
- globs non-recursively, so `examples/modules/` never runs

CI natively compiles 11 of 13 examples, omitting `records.nx`, `methods.nx`
and `syntax.nx` — the only coverage of records, `mut self`, `del`,
comprehensions, slices and `None`.

#### Fixes in this entry

- `tools/argone-gate.ps1` and `tools/argone-gate-tests.ps1` built paths with
  literal `\`, which PowerShell on Unix treats as a filename character. The
  `argone-gate` CI job I added in v0.4.3 therefore exited 2 on every
  `ubuntu-latest` run. Both now build paths with `Join-Path`, and the
  self-tests invoke `pwsh` on PowerShell Core rather than `powershell` by
  name.
- `docs/ARGONE.md`, `docs/ARGONE-STATUS.md` and the gate's expected-task list
  rewritten from 17 MLIR-shaped tasks to 15: Task 0 (semantic correctness)
  plus A-N. The two-state rule is unchanged. All ten adversarial gate tests
  pass.

No compiler behaviour changed. 318 tests pass; the 13 examples still agree
across interpreter / native / `NX_NOUNBOX`.


### Draft v0.4.3

#### ARGONE: the architecture stage is planned, gated, and started

`Nexum_Argone_Unified_AOT_Prompt.md` defines ARGONE as a hard stage gate:
the compiler moves from an AST-heavy, directly-to-textual-LLVM design to a
layered pipeline with its own HIR and MIR/SSA, a real MLIR dialect, and an
AOT backend. Stage 4 through Stage 8 do not resume until it is 100%
complete.

- `docs/ARGONE.md` — the breakdown: 17 tasks (Task 0 plus A-P, the prompt's
  mandated order), each with deliverables and a checkable gate. A, B and C
  are independent of the toolchain and can run in parallel; D onward is
  strictly sequential.
- `docs/ARGONE-STATUS.md` — the completion checklist. This file *is* the
  gate.
- `tools/argone-gate.ps1` — decides whether the stage is complete. Rejects
  `in progress`, `partial`, `mostly`, `pending` and `blocked` as statuses,
  because a stage that admits a third state is a stage that gets left
  halfway. A task may claim `complete` only with every gate item checked
  *and* an evidence line, which is what makes scaffolding insufficient.
- `tools/argone-gate-tests.ps1` — ten adversarial tests that try to fool the
  gate the four ways the prompt names as invalid grounds for declaring
  success: scaffolding, a partial status, unchecked boxes, and a build that
  merely compiles. It also proves the gate *opens* on a genuinely complete
  stage, so the mechanism cannot rot into something that always fails.
- CI gains an `argone-gate` job that runs the gate's self-tests and prints
  the stage state.

#### Task 0: a prerequisite Argone discovered

Measuring this host before committing to an architecture:

| Check | Result |
| --- | --- |
| clang | 23.1.2, installed |
| MLIR tools (`mlir-opt`, `mlir-translate`, `mlir-tblgen`) | **absent** |
| `opt`, `llc`, `llvm-config` | **absent** |
| `static.crates.io` (crate download CDN) | **unreachable** |
| cached crates binding LLVM/MLIR | none |
| C++ toolchain (`cl.exe`) | not on PATH |

The LLVM Windows installer ships clang, lldb and lld but not MLIR, so the
MLIR stages have no toolchain here, and the blocked crate CDN means
`melior`/`inkwell`/`mlir-sys` cannot be fetched either. Nexum has **zero**
external crate dependencies and keeps it that way unless the toolchain route
forces otherwise.

So obtaining an MLIR toolchain is Task 0, an Argone task with its own gate,
resolved *before* HIR begins. Once HIR exists the lowering strategy is
committed; discovering MLIR is unobtainable at that point would strand the
work. If no route is viable, the correct outcome is a documented
renegotiation with the user — not a quietly narrowed Argone that reports
success for the easy parts.

#### Measured duplication Argone targets

Five crates walk the AST independently, re-deriving the same facts:

| Crate | `Stmt` arms | `Expr` arms |
| --- | --- | --- |
| nx-types | 15 | 37 |
| nx-ir | 23 | 13 |
| nx-mem | 21 | 18 |
| nx-interp | 14 | 28 |
| nx-codegen | 27 | 37 |
| **total** | **100** | **133** |

Task C's gate is that this number falls measurably. If it does not, the new
abstraction is not earning its place.

Baseline for comparison: 17,290 Rust lines across 9 crates, 318 tests,
2,956-line runtime, zero external dependencies, 13 examples with a 3-way
differential, 5 Rust-referenced benchmarks.


### Draft v0.4.2

#### The grammar, written down
`docs/grammar.md` documents the whole language, derived from the lexer,
parser and checker rather than from intent, with every rule naming the
function that enforces it. Lexical grammar, the full statement and
expression grammar, a precedence table, the ambient surface, the static
rules, and a complete program whose output is verified by running it three
ways.

Writing it turned up **five real bugs**, four of them pre-existing.

#### Fixed: `free()` on a read-only string constant
`nx_str` does not copy -- it stores the caller's pointer, so a string
literal's payload points straight into read-only static memory. But
`nx_free_val` called `free` on it. Any function whose Unique local held a
string aborted with `STATUS_HEAP_CORRUPTION`:

```
fn f():
    s = "hi"
f()
```

Three lines. Every `fn` with a non-escaping string local hit it.

The fix is consistency rather than a patch: strings are shared, never
mutated in place, and `needs_clone` never copies one -- so a shared object
with many owners cannot be freed by any of them. `nx_free_val` no longer
frees strings. `nx_strcat` and `nx_slice` do allocate, so those now leak,
which is the documented model for this release.

#### Fixed: a loop variable was a module global
A `for` variable took the `in_init` path and became a module global, so it
leaked into every later statement and two loops collided on the name.
Inside a `parallel:` task that collision is a data race, which would break
the determinism contract outright. Loop variables are now always local
slots.

#### Fixed: a nested loop destroyed the outer loop's variable
`for i in 0..3:` containing `for i in 0..2:` left the outer `i` holding the
inner loop's last value. Observable, in both the interpreter and the
backend:

```
for i in 0..3:
    for i in 0..2:
        print("in", i)
    print("out", i)     # printed 1 every time, should be 0, 1, 2
```

A loop variable is now scoped to its loop and the previous binding is
restored on every exit -- normal, `break`, `continue`, `return` and error
-- in both the frame path (inside a function) and the globals path (module
level), which are two different binding paths.

#### Fixed: a trailing `parallel:` block emitted invalid LLVM
```
x = 0
parallel:
    a = 1
    b = 2
```

`clang` rejected it with "expected instruction opcode". The globals pass
only scanned top-level statements, so a name first assigned inside a task
was discovered *during* emission and its declaration landed between an
`entry:` label and the next instruction. The pass now walks task bodies.

#### Fixed: a `mut self` write-back could be silently dropped
`store_name` tested `in_init` before checking for a local slot, so a
write-back to a loop variable went to a module global while the next read
used the local slot. The update vanished: the code compiled, ran, and
printed the old value. A name with a local slot is a local, full stop.

Fixing that exposed a latent one: `cur_fn` was never set to `<top>` while
emitting the module body, so every lookup keyed on the current function --
unboxing, method dispatch on a local, the memory plan -- missed. Top-level
code was never unboxed at all, and a method call on a top-level local could
not resolve. Both now work.

#### Known gaps, not fixed here
- `del p` after a `mut self` write-back on a record fails to compile
  ("only modules, types and builtins support attribute calls"). Plain
  `del p`, `del xs[0]` and `del d["a"]` are all fine.
- `del d["a"]` on a single-key dict makes the checker report `d` as
  undefined afterwards.

#### Verification
322 tests pass. All 13 examples agree across interpreter / native /
`NX_NOUNBOX`. All 5 benchmarks match their Rust reference output. The
grammar's example program runs identically on all three paths and its
documented output is machine-checked.


### Draft v0.4.1

#### Numeric representation audit
Full audit in `docs/numeric-audit.md`. Every numeric width in the compiler
and the emitted runtime is classified with a measurement or a reason.

- **Source positions are `u32`.** `LineNo`/`ColNo` live in `nx-lexer`,
  where positions are born, and every crate uses them. `Span` 16 -> 8
  bytes, `Expr` 64 -> 56, `Stmt` 192 -> 168. A position is bounded by the
  file it describes and is never used in arithmetic, so there was nothing
  to lose; `nx check` peak working set on a 400-function program drops
  12.4 -> 11.3 MiB (-8.9%).
- **Container headers are 16 bytes.** `%NxList` and `%NxDict` count
  elements, not bytes, and 2^31 elements needs 32 GiB behind a runtime
  with no collector, so both counters are `i32`. 24 -> 16 bytes per
  header, measured against the CRT at 8 bytes per block.
- **The header `malloc` moved with the struct.** Narrowing the type while
  leaving `malloc(24)` saves nothing and leaves 8 bytes of tail padding.
  The first attempt did exactly that and measured as no change at all.
  A test now pins struct and allocation together.
- **Counts cannot wrap.** `nx_listpush` and `nx_dictset` check for
  saturation before narrowing, on the growth path, so a push costs a 32-bit
  store instead of a 64-bit one -- same instruction count, half the bytes.
- Rejected, with measurements: narrowing `%NxVal.tag` (lands in padding and
  adds an extension to every tag check), narrowing `%NxVal.extra` (also
  padding; and it is the reason `len()` is a load rather than a pointer
  chase), narrowing `%NxRec.nfields` and `%NxDesc` (padding, and there is
  one descriptor per *type*), dropping the memo slot's `used` flag (3% of
  a table that is mostly its argument slots), narrowing the memo lock and
  hash (a contended atomic and FNV-1a's definition), `f64` -> `f32`
  anywhere (changes results), and boxing `Expr::Comprehension`'s variable
  (trades an allocation per comprehension for 8 bytes).
- New structure-size tests in `nx-ast` and `nx-codegen` pin every width
  above, so a widening is a test failure rather than a quiet regression.

#### Not done, and why
Deleting `%NxVal.extra` would take every NX value from 24 to 16 bytes --
33% off every list element, dict entry, memo slot and call argument array.
It is 53 sites in `runtime.ll` in the core value representation of a
language with no garbage collector, so it wants its own stage and a
sanitizer sweep rather than an audit applied in passing. Also deferred:
the 960 KiB memo table in every binary, constant-range narrowing below
`i64`, and flat scalar array storage. See `docs/numeric-audit.md` §6.

#### Verification
307 tests pass. All 13 examples agree across interpreter / native /
`NX_NOUNBOX`. All 5 benchmarks match their Rust reference output. A
same-session A/B of the benchmark suite shows mixed signs across five
benchmarks (two faster), i.e. noise rather than a regression.


### Draft v0.4.0

#### Stage 3: `impl` blocks and methods
- `impl T:` attaches functions to a type declared in the same module.
  `fn name(self)` is a method, called `v.name(...)`; `fn name(mut self)`
  additionally writes its result back into the receiver, so
  `p.moved(1.0, 1.0)` means `p = moved(p, 1.0, 1.0)`; `fn name()` is an
  associated function, called `T.name(...)`.
- Writing through a read-only `self` is a compile error. `self` is a copy
  under value semantics, so such a write would be silently discarded --
  the compiler says so instead of letting it look meaningful.
- A `mut self` method must return the record: its result *is* the new
  receiver, so anything else would clobber it with the wrong type.
- A `mut self` call on a receiver with no storage still evaluates, it just
  has nowhere to write. That is what makes `q.moved(1, 1).moved(2, 2)`
  read as one expression.
- Orphan impls are refused: an `impl` must live with its `type`. Two
  modules holding incompatible layouts for one name is exactly what a
  static type system cannot represent.
- Resolution order for `base.attr(...)` is module, associated function,
  impl method, then builtin sugar. Methods therefore win over the sugar,
  so a type may define its own `push`. An unresolved base still takes
  sugar, which is what keeps `x.push(1)` working on dynamic values.
  Dynamic dispatch on an unresolved receiver is Stage 4.
- Methods are their own analysis scope under `Type.method` keys in
  `nx-ir` and `nx-mem`, so they never collide with same-named plain
  functions in the effect or memory plans.
- A method is never memoized. Purity analysis reasons about a function's
  own body, but a `mut self` method's contract extends past it: the call
  writes back at the call site, and a cache hit would skip that write.
- Method dispatch types come from the checker, not from the unboxing
  decision. `NX_NOUNBOX=1` keeps working method for method, so the opt-out
  stays a debug switch instead of becoming a second language.

#### Fixed
- A method receiver was emitted twice in the backend -- once to learn its
  static type, once to build the argument list. For a `mut self` receiver
  that ran the write-back twice, so `q.moved(1, 1).moved(1, 1)` advanced
  `q` three times instead of once. Receivers are now evaluated once.
- `nx_memo_put` was emitted from a second lookup of the memo id, so a
  function could write a cache entry it never read, under an id belonging
  to another function. The prologue's decision is now the only one.
- Checking a function body consumed the enclosing scope, so every
  statement after the first `fn` in a module saw an empty module and
  reported its variables undefined. The locals are now returned instead
  of taken.
- `T.m(...)` asked the expression checker about `T`, which is a type and
  not a binding, so it was reported undefined. Associated-function calls
  are now resolved before the base is evaluated as a value.
- `self` was a keyword the expression parser did not accept, so
  `self.x = 1` -- the whole reason `mut self` exists -- did not parse.

#### Tooling
- `tools/verify.ps1` runs every example three ways -- interpreter, native,
  and native with `NX_NOUNBOX=1` -- and diffs the output. All 13 examples
  must agree; a disagreement is a build failure.
- All five benchmarks still match their Rust reference output.


## v0.3.0

### Purity-directed automatic memoization
- `nx-ir` proves a function Pure; the compiler caches it automatically,
  with no annotation and no source change. Scalar arguments only, so
  list mutation stays visible. 4096-entry bounded cache, cmpxchg
  spinlock in the runtime (no pthreads, so it links on Windows too).
- Opt out with `NX_NOMEMO=1`. Interpreter `fib(30)`: 80.17s -> 0.09s;
  native `fib(90)`: 0.19s.
- Parallel worker threads get a 64 MiB stack: memoized recursion reaches
  a depth that overflowed the 2 MiB default.

### Unboxing optimizer
- Values with a statically known scalar type live in bare `i64`/`double`
  /`i1` registers and stack slots, with raw LLVM arithmetic. Boxing is
  re-inserted only at dynamic boundaries: call arguments, returns, list
  elements, module globals, `print`. Function boundaries stay boxed.
- `nx-types` now infers parameter types from the body. Uses with exactly
  one answer pin the type (`xs[i]` is `Int`, `if b` is `Bool`,
  `x < 1.5` is `Float`); a parameter used only in arithmetic is numeric
  and defaults to `Int`, matching how a numeric local is fixed by its
  first binding.
- Fixed a soundness hole the unboxing exposed: an unresolved expression
  assigned to a known-typed variable now widens that variable to
  unresolved, instead of silently keeping the narrow type.
- `nx_eq` on floats now uses IEEE comparison rather than bit equality, so
  `NaN != NaN` and `0.0 == -0.0` hold and the boxed and unboxed paths
  agree with the interpreter.
- Opt out with `NX_NOUNBOX=1`. ~6.3x faster on numeric loops, identical
  output.

### Task pool for `parallel:`
- A batch gets a pool sized to the batch instead of one thread per task.
  Workers claim task indices from a shared cursor, so a thread that
  finishes early picks up the next task instead of idling, and uneven
  tasks still balance. The calling thread works too, which saves a
  thread and guarantees the batch drains even if a spawn fails.
- The pool is per-block, not global: no process-wide mutable state,
  nothing to shut down, and no question about a pool outliving the code
  that queued work into it.
- Only the two OS bindings differ between platforms, so they moved to
  `runtime_threads_win.ll` / `runtime_threads_unix.ll`. The pool logic
  is shared and therefore tested identically everywhere.
- 8 independent tasks: 0.190s sequential -> 0.033s, a 6x speedup on 16
  logical cores (4.42x with thread-per-task), output byte-identical.
- CI now runs the parallel example 8 times and diffs, since threads make
  output order a real risk.

### Windows threaded `parallel:`
- `parallel:` batches now run on real threads on Windows instead of
  lowering sequentially, so the block means the same thing on every
  platform. 8 independent tasks: 0.193s sequential -> 0.044s, a 4.42x
  speedup on 16 logical cores, with byte-identical output.
- The HANDLE comes from `CreateThread`'s return value. Its last argument
  is `lpThreadId`, a DWORD id, and waiting on that silently failed
  every time -- which is why the first Windows build produced zeros.
- `runtime.ll` declares both platforms' threading APIs and stays
  platform-neutral: an unused declaration emits no symbol reference.
- `collect_outer_reads` and the batch emitter are no longer unix-only.
- CI asserts the generated IR actually contains spawn calls. A batch
  that quietly fell back to inline execution would still pass the
  differential test while doing no parallel work at all.

### Tooling
- `nx build --emit-ir` prints the generated LLVM IR.
- Build stamp invalidates the cache when `NX_NOMEMO` or `NX_CFLAGS`
  change, so a flag flip rebuilds instead of reusing a stale binary.
- CI now checks that unboxed and `NX_NOUNBOX` builds produce identical
  output.

## v0.2.0
- Deterministic `parallel:` blocks (threaded interpreter, pthread codegen)
- Effects analysis (`nx-ir`, `nx dump-ir`)
- `nx setup` per-theme icon plus-one; icon coexistence policy
- `nx build` incremental + `--run`; `NX_CFLAGS` passthrough

## v0.1.0
- Memory planner (`nx-mem`): Unique locals freed at scope exit, Shared arena
- `NX_CFLAGS` passthrough; ASan differential in CI (Linux)

## v0.0.8
- LLVM backend: `nx build` native exes, `nx build --run`
- Interpreter and binaries verified byte-identical in CI

## v0.0.7
- Modules: `import` / `as` / `from…import`, `mod.member`, circular detection
- `nx check` static type checker (advisory)

## v0.0.6
- `nx update` finds the `code` CLI outside PATH

## v0.0.5
- `nx update` self-elevates via UAC on Windows (no more Access denied)

## v0.0.4
- VS Code: Nexum file icon theme (blue N on `.nx` files)
- Extension publisher `HasanSalaou`

## v0.0.3
- Windows MSI installer (PATH + bundled VS Code extension)
- `nx update` also updates the VS Code extension

## v0.0.2
- `nx update` self-updater (`nx update --version <ver>` to pin)
- `nx --version` / `nx --license` (MIT embedded in exe)
- Raw `nx` binaries per release (no more zips)

## v0.0.1
- Initial interpreter: lexer, parser, tree-walk `nx file.nx`
- Indent blocks, `if/elif/else`, `while`, `for i in a..b` + `for x in list`
- `fn` + `return` + recursion, scopes
- Lists, indexing incl. negative, `len()`, `push()`
- `break` / `continue`, `+= -= *= /=`
- `Int` / `Float` (15-sig display) / `Bool` / `Str`
- VS Code extension: highlighting + `:` auto-indent
