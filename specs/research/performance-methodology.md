# How the programme measures speed

**Question.** How will the native roadmap know whether Nexum is fast enough? What
benchmark suite exists today, what should a benchmark suite for this language
contain, how must it be run for the numbers to mean anything, which costs must
it measure so that the claims of phases 11c-11i are measured rather than
asserted, what is the comparison baseline, what should the tooling be, and where
does it live so that the agents implementing 11c-11i do not collide on files?

**Summary.** There is a benchmark today, but it is one program (`fib30.nx`), one
PowerShell script, and two numbers, and it does not survive contact with
reality: measured on the authoring machine, process start-up (about 10 ms) plus
run-to-run noise (up to 8x) swamps the workload, so the recorded "53.97 ms" is
mostly start-up, and the only optimisation comparison in the repository
(`benchmarks/phase8_report.md`) has 2 runs per configuration and reports
`-O3+LTO` as twice as slow as `-O2`, which is noise, not a result. The
recommendation is to build a real suite in a new tree, `benchmarks/suite/`, with
a hand-written Rust harness at `crates/nexum-driver/src/bin/nx-bench.rs` (no new
crates, no new dependencies, no collision with the files 11c-11i will touch),
where each case is a `.nx` file carrying a machine-readable `# BENCH` header
(iteration count, expected checksum, an optional IR assertion that the case is
still measuring what it claims) and the harness times each case as a whole
process, 15 times, takes the median, subtracts a paired baseline case, and
records the result as ns/op in a committed `baseline.json` keyed to a machine
profile. The suite must measure four costs the programme's own claims rest on -
syscall overhead, call overhead, allocation cost, and the amortised cost of a
collected allocation - and it must compare against the same algorithm compiled
in C with the same clang and the same flags, because "Nexum is 87% of C" is the
only absolute claim a systems language can make. One primitive is missing and
must be scheduled: a monotonic clock, without which every number is a whole
process and the cheap operations (an integer add, a call) cannot be separated
from loop overhead.

## Recommendation

1. **Put the suite in `benchmarks/suite/` and the harness in
   `crates/nexum-driver/src/bin/nx-bench.rs`.** Cargo auto-discovers
   `src/bin/*.rs` as an extra binary even when the crate already declares an
   explicit `[[bin]]` (verified experimentally on this toolchain), so this needs
   **no change to any `Cargo.toml`**, no new workspace member, no new
   dependency - which is what the agent brief's rule 7 requires - and it sits in
   a file no other agent in the programme will create. The alternative, a
   separate `crates/nexum-bench` crate, is one line in the root `Cargo.toml`
   plus one `Cargo.toml`; it is cleaner but it merges badly with the ten other
   branches in flight. The benchmark *programs* belong under `benchmarks/suite/`
   rather than in the Rust crate because they are language programs, and because
   the existing `benchmarks/*.ps1` scripts glob only the top level of
   `benchmarks/` and so will not pick them up.

2. **Each case is a `.nx` file with a machine-readable header.** One line, the
   first line of the file:

   ```
   # BENCH unit=call ops=20000000 expect=400000000000000 ir_check=call i64 @noinline
   ```

   `unit` is the label for the report; `ops` is the number of iterations of the
   operation under test and **must equal the iteration count written in the
   source**; `expect` is the exact value the program must print on its last
   line, which makes every case a correctness test as well as a measurement;
   `ir_check` is an optional substring the emitted LLVM IR must contain, which
   is how a case proves it is still measuring the thing it claims to measure
   (this is not optional in spirit - see risk 1). A case with no `ops` is a
   *probe*: it prints a value and the harness records it as a metric (bytes
   allocated, live heap, parameters touched) instead of a time.

3. **The harness compiles the cases itself, not through the `nx` binary.** It
   calls `nexum_source` -> `nexum_parser` -> `nexum_sema` -> `nexum_mir` ->
   `nexum_codegen` -> `nexum_toolchain::run_clang_with` directly, so that it
   can set the **MIR optimisation level and the clang optimisation level
   independently** - which is exactly what today's scripts cannot do, and
   whose confounding produced the meaningless `-O3`/`-O3+LTO` rows. The
   harness's own build profile is irrelevant: only the flags of the *measured
   program* matter, and the harness records them in the report and in the
   baseline file.

4. **Reject the criterion-based harness.** Criterion (and every other
   `[[bench]]` harness) times *Rust functions*. The thing being measured here
   is a *Nexum program*, so a Rust harness can only time the process spawn -
   which is the noise, not the signal. It is also forbidden by the agent
   brief's "no new dependencies" rule, and no `criterion` is present in
   `Cargo.lock` today. The correct harness is a hand-written one, and it is
   small: compile, run 15 times, take the median, subtract, divide.

5. **Measure with iteration counts and a paired baseline, not with a clock** -
   because the language has no clock. The harness times the whole process 15
   times and takes the **median** (the median of 15 runs is far more stable
   than the current scripts' min-of-7 or 2-run samples), then subtracts the
   median of a paired baseline case (by default `micro/loop_empty.nx`, which
   runs the same loop without the operation under test), then divides by
   `ops`. The subtraction is what removes the constant overheads - loop
   bookkeeping, bounds checks, process start-up. It has a noise floor of about
   1 ms per pair, so **a case's `ops` must be chosen so the expected cost of
   the measured operation is at least 100 ms**; the harness warns when a case
   finishes in under 100 ms or over 30 s.

6. **Schedule a monotonic clock as a dependency.** A `now_ns()` builtin backed
   by `clock_gettime(CLOCK_MONOTONIC)` (Linux syscall 228) and
   `mach_absolute_time` (Darwin) is the single missing primitive that turns
   "ns per process, minus a baseline" into "ns per operation, measured inside
   the program". It belongs in the phase 11a runtime (the syscall layer at
   `crates/nexum-runtime/src/syscalls.rs` already has read, write, open, close,
   exit and lseek, but no clock), and it is small: one syscall number per
   platform, one wrapper, one builtin, one ABI entry. Until it lands the suite
   is still useful, but the cheap operations stay approximate and must be
   labelled as such.

7. **The suite must measure four costs, and they are the ones the programme's
   own claims rest on.** Syscall overhead, call overhead, allocation cost, and
   the amortised cost of a collected allocation. These are the four numbers
   that "Nexum owns the machine" is actually made of, and none of them can be
   asserted without a measurement. The suite also needs the ordinary
   microbenchmarks a systems language is judged on - integer and float
   arithmetic, memory access, string handling, list operations - and three
   whole-program benchmarks (matrix multiply, a parser, a sort) plus, once 11i
   lands, one training step of the model framework.

8. **The comparison baseline is the same algorithm in C, compiled with the
   same clang and the same flags.** Not another language: a cross-language
   comparison that does not hold the backend constant measures the backend. The
   C twin for each whole-program case lives in `benchmarks/suite/reference/`
   and is compiled with `clang -O3` on the same target, with no `-ffast-math`,
   no manual unrolling and no `restrict`, so that the difference is the Nexum
   front end and runtime rather than the optimiser. The second baseline is
   **the previous version of Nexum**: that is what `baseline.json` is for, and
   every phase's pull request should re-run the suite and show the delta. The
   third reference is the **C language's own numbers for the same operations**
   (call overhead, malloc, a raw syscall), which is how the four runtime
   claims are turned into "Nexum's call costs X ns, C's costs Y ns".

9. **Regression rule: median-based, with a noise-aware threshold.** A case is
   a regression when `new_median > baseline_median * (1 + max(2%, 3 * noise))`
   and an improvement when it is below `baseline_median * (1 - max(2%, 3 *
   noise))`, where `noise` is the relative interquartile range
   `(p75 - p25) / median` of the same 15 samples. The 2% floor is below the
   noise of a quiet machine; the `3 * noise` term protects a noisy one. The
   verdict uses the median (robust) and the min is reported alongside it.

10. **The machine is part of the baseline.** The harness records a machine
    profile (CPU model, core count, OS and version, power plan or governor,
    whether the machine is on AC, the clang and rustc versions, and the
    measured flags) in `baseline.json` and **refuses to compare a run against a
    baseline recorded on a different profile** unless forced. A benchmark
    result without a machine is not a result.

11. **CI runs the suite in smoke mode only.** The GitHub-hosted runners
    (`.github/workflows/ci.yml` runs on `ubuntu-latest`, `macos-latest`,
    `windows-latest`) are shared and throttled and cannot produce a usable
    timing; asserting performance there would produce flaky failures and
    train the programme to ignore the suite. CI runs `nx-bench smoke`, which
    compiles every case, runs it once, and checks the checksum - so the suite
    is also a correctness gate on the optimiser, which is a second, cheaper
    win. The timing run is a separate job on a known machine.

12. **The first sitting writes 8 files.** The harness
    (`crates/nexum-driver/src/bin/nx-bench.rs`, about 350 lines), the case
    directory with 4 micro cases and 3 whole-program cases plus their C
    twins, `benchmarks/suite/README.md`, and `benchmarks/suite/baseline.json`
    produced by the first `--record` run. The rest of the cases listed in the
    design below are the follow-on, one per phase.

## What the code looks like today

### The optimisation levels that exist

There are **two independent** optimisation knobs, and they are on different
commands. This is the single most important fact for the benchmark harness,
because the existing scripts confound them.

**MIR level - the global `--opt-level` flag.**

* `crates/nexum-driver/src/main.rs:16-18` - `--opt-level` is a global `u8`
  flag, default `0`, documented as "0 = none, 1 = const-fold + DCE + inlining".
* `crates/nexum-driver/src/main.rs:1070` - `compile_to_mir` calls
  `optimize_level(&mut mir, opt_level)`.
* `crates/nexum-mir/src/lib.rs:1383-1393` - the whole of it:

  ```rust
  pub fn optimize_level(m: &mut MirModule, level: u8) {
      if level == 0 { return; }
      const_fold(m);
      dce(m);
      inline_small(m);
      const_fold(m);
      dce(m);
  }
  ```

  There is no level 2, 3 or 4. Anything above 0 runs the same five passes.
* `crates/nexum-mir/src/lib.rs:1377-1379` - `optimize()` is level 1, and that
  is what `crates/nexum-driver/tests/common/mod.rs:55` uses in
  `compile_file`, so the correctness tests run the optimiser while the
  benchmarks do not.
* `crates/nexum-mir/src/lib.rs:1100-1135` - the inliner's rules, which matter
  for any call-overhead measurement: a function is inlinable only if it is a
  single block, has no `mut` parameter, ends in `Return(Some(_))`, has at most
  8 instructions, and contains no `Alloca`, `Load`, `Store`, `Panic` or `Call`.
  `inline_small` then loops to a fixed point (`loop { if !try_inline_one(m) {
  break } }`), so it inlines chains: a callee that calls another callee becomes
  inlinable once the inner call has been inlined into it. **A call-overhead
  benchmark that does not defeat this measures an inlined loop.**

**Backend level - the clang flags on `nx build`.**

* `crates/nexum-driver/src/main.rs:38-55` - `nx build` takes `--opt <0-3>`
  (default `2`), `--lto`, `--sysroot`, `--pgo-generate`, `--pgo-use` and
  `--release`.
* `crates/nexum-driver/src/main.rs:183-189` - `--release` means clang `-O3`
  plus thin LTO; `--lto` alone adds `-flto=thin -fuse-ld=lld`.
* `crates/nexum-toolchain/src/lib.rs:178-208` - `run_clang_with` turns the
  options into `-O{min(opt,3)}`, `-flto=thin -fuse-ld=lld`, `--sysroot=`,
  `-fprofile-generate`, `-fprofile-use`.
* `crates/nexum-driver/src/main.rs:994-1005` - the default `BUILD_OPTS` is
  `opt: 2`, so **`nx run` links with clang `-O2`**.

So the effective default build is **MIR level 0 + clang `-O2`**: the default
build runs no MIR-level optimisation at all. And `nx build --opt 3` changes
only the backend level; to get MIR level 1 as well you must write
`nx --opt-level 1 build file.nx --opt 3`.

### The benchmark suite that exists

* `benchmarks/` at the repository root holds eight files: `fib30.nx`,
  `run.ps1`, `phase8_compare.ps1`, `compiler_perf.ps1`, `BASELINE.md`,
  `compiler_perf.md`, `phase8_report.md`, `results.csv`.
* `benchmarks/run.ps1:19-50` - the harness. It globs `benchmarks/*.nx`,
  compiles each with `nx emit-ir` (which does **not** pass `--opt-level`, so
  the MIR passes are off), links with `clang -Wno-override-module -O2`, runs
  the executable **once as a warm-up**, then runs it 7 more times timing the
  **whole process** with `System.Diagnostics.Stopwatch`, and reports the min
  and the median. It appends a row to `benchmarks/results.csv`.
* `benchmarks/BASELINE.md:14-31` - the only baseline in the repository:
  `fib30`, 7 runs, min 53.97 ms, median 59.77 ms, on "Windows x86_64, clang
  23.1.3, nx debug build, clang -O2, no LTO/PGO". Its own caveat is the
  finding this document is about: "Timer resolution and process startup
  dominate for small benchmarks; keep workloads >= ~10 ms".
* `benchmarks/phase8_compare.ps1:12-17` - the optimisation matrix:

  | level | MIR `--opt-level` | clang |
  | ----- | ----------------- | ----- |
  | `-O0` | 0 | `-O0` |
  | `-O2` | 0 | `-O2` |
  | `-O3` | 1 | `-O3` |
  | `-O3+LTO` | 1 | `-O3` + thin LTO |

  The MIR level and the clang level move together, so the table cannot say
  which one produced a difference.
* `benchmarks/phase8_report.md:8-11` - the results, **2 runs per
  configuration**: `-O0` 21.47/24.25, `-O2` 14.84/16.36, `-O3` 17.02/17.90,
  `-O3+LTO` 28.08/30.84. `-O3` slower than `-O2`, and LTO twice as slow as
  everything else. With 2 samples and a ~10 ms process-start-up floor on a
  workload of this size, none of these four numbers means anything.
* `benchmarks/compiler_perf.md` - compile-time numbers, also 2 runs per cell.
* There is **no `[[bench]]` target anywhere** and no `criterion` in
  `Cargo.lock`. `crates/nexum-driver/tests/` contains `backend.rs`,
  `fixtures.rs` and `common/mod.rs`; none of them measures time.
  `crates/nexum-driver/tests/backend.rs:251-...` (`differential_opt_levels_agree`)
  compiles each example at MIR level 0 and 1 and compares the **program
  output** - a correctness gate on the optimiser, which is valuable and is not
  a performance measurement.
* `crates/nexum-driver/tests/backend.rs:94-120` (`run_outputs_match_fixtures`)
  is the existing "expected output" mechanism: compile `tests/fixtures/run/*.nx`
  and compare stdout against the sibling `.out` file. The benchmark suite
  should reuse that shape (`expect=` in the header, rather than a `.out`
  file, so that the checksum and the timing metadata live together).
* `crates/nexum-driver/src/main.rs:858-897` - `nx test <dir>` runs every
  `.nx` in a directory and checks only the **exit status**. No output
  comparison, no timing.

### The runtime facts the cost model depends on

* `crates/nexum-runtime/src/lib.rs:143-150` - `nexum_alloc_scanned`: the
  payload is rounded up to 16 bytes and the **header is 32 bytes** - size at
  offset 0, mark bit at 8, next-object pointer at 16, scan-bytes at 24, payload
  at 32. It calls `nexum_gc_maybe()` **before every allocation** and then
  allocates through `malloc`.
* `crates/nexum-runtime/src/lib.rs:53-54` - `declare ptr @malloc(i64)` and
  `declare void @free(ptr)`: the collector still goes through libc.
* `crates/nexum-runtime/src/lib.rs:108` - `@nexum_threshold = 65536`: a
  collection is triggered every 64 KiB of allocation, and **the threshold is
  not settable from the language**.
* `crates/nexum-runtime/src/lib.rs:155` - `nexum_gc_maybe` compares
  `nexum_since > nexum_threshold`.
* `crates/nexum-runtime/src/lib.rs:168` - `nexum_scan_range` scans
  conservatively, eight bytes at a time, treating every word as a candidate
  pointer.
* `crates/nexum-runtime/src/lib.rs:174` - `nexum_collect` marks from every
  registered frame's root entries, traces the heap to a fixed point, then
  sweeps.
* `crates/nexum-runtime/src/lib.rs:191` - `nexum_concat`: two `strlen`s, one
  allocation of `len+1` bytes, two `memcpy`s. `nexum_slice` (line 201) is one
  allocation and one `memcpy`. Both are therefore **one managed allocation per
  call**, which makes them the natural allocation-cost probe.
* `crates/nexum-runtime/src/lib.rs:224` - `nexum_list_push` copies the
  element into a flat, doubling buffer; the element is *not* boxed.
* `crates/nexum-runtime/src/lib.rs:235` - `nexum_heap_used` returns
  `nexum_live`, the **payload** bytes of live objects (the 32-byte header is
  not counted).
* `crates/nexum-codegen/src/lib.rs:771-774` - `print` is libc `printf`.
* `crates/nexum-runtime/src/syscalls.rs:39-54` - the syscall layer has read,
  write, open, close, exit and lseek, on Linux and macOS, x86-64 and AArch64.
  **There is no clock syscall and no builtin exposes `write`/`read` to a
  program.**
* `docs/standard-library.md:9-38` - the complete builtin list: `print`,
  `panic`, `len`, `concat`, `slice`, `str_char_at`, `collect`, `heap_used`,
  `read_file`, `write_file`, and the five `list_*` builtins. **There is no
  clock, no timer and no environment access.**

### What the language can and cannot express today

Verified by compiling and running programs on this machine (Windows x86_64,
clang 23.1.3, `nx` built from `main` at `316f6c6`):

* **Works:** `let mut a: [Float; 4] = [1.0, 2.0, 3.0, 4.0]` (an explicit type
  annotation with an array literal); the nested array type `[[Float; 2]; 2]`
  and `m[1][0]`; a `mut` parameter mutating a caller's array or record in
  place; `list_new` / `list_push` / `list_get` / `list_set` / `list_len`;
  `concat` / `slice` / `str_char_at` / `len`; `collect()`; `heap_used()`.
* **Does not work:** `[0.0; 4096]` repeat syntax - `[T; N]` is a *type*
  (`docs/language-reference/grammar.md:67`), not a literal. **Array sizes in
  a benchmark are therefore bounded by how many elements a human is willing
  to type**, which is the real reason a 64x64 matrix benchmark cannot be
  written today. The matrix benchmark in the first sitting uses a flat
  `[Float; 64]` written out as 64 literals, or a `List` of `Float` used as a
  flat buffer.
* **Consequence for the plan:** the suite's ability to express realistic
  workloads is bounded by the language, and the benchmark suite is the place
  where that shows up first. Filling an array, taking a slice of an array, and
  expressing `f32` are all 11d/11c features; until they land, the whole-program
  benchmarks are small. This is worth saying out loud rather than quietly
  benchmarking a 16x16 matrix and calling it a matrix benchmark.

## The design

### Directory layout

Everything new is under two directories that no other agent in the programme
will touch.

```
benchmarks/suite/                       # the suite: programs, references, baseline
  README.md                             # how to run it, how to read the report
  baseline.json                         # the accepted baseline (committed)
  cases/
    micro/                              # one operation at a time
      loop_empty.nx                     # the paired baseline for the micro group
      arith_int.nx
      arith_float.nx
      call_value.nx
      call_mut.nx
      array_read.nx
      list_push.nx
      string_concat.nx
      gc_pause.nx
    program/                            # whole programs
      matmul.nx
      parser.nx
      sort.nx
    ml/                                 # phase 11i
      train_step.nx
  reference/                            # the C twins, same algorithm
    matmul.c
    parser.c
    sort.c
  results/                              # raw run JSON (gitignored)

crates/nexum-driver/src/bin/nx-bench.rs # the harness
```

`benchmarks/suite/results/` is the only new path that should be gitignored;
`baseline.json` is committed, because it is the artefact a pull request
compares against. The existing `benchmarks/*.ps1`, `benchmarks/*.md`,
`benchmarks/fib30.nx` and `benchmarks/results.csv` are left exactly as they
are - `fib30.nx` is still the historical Phase 2 baseline, and the Phase 8
report is an artefact of a past phase, not something to rewrite.

### The case format

One header line, the first line of the file, after any comment block:

```
# BENCH unit=<label> ops=<int> expect=<value> [ir_check=<substring>] [base=<path>] [note=<text>]
```

* `unit` - the label in the report. Use one unit per kind of thing:
  `int-add`, `float-add`, `call`, `call-mut`, `array-read`, `list-push`,
  `byte-alloc`, `ns-per-syscall`, `ns-per-gc`, `flop`, `ns-per-byte`.
* `ops` - the number of iterations of the operation under test, **and it must
  equal the iteration count in the source**. The harness does not rewrite the
  source; the equality is checked indirectly, because `expect` is only correct
  for exactly that many iterations, so a mismatch fails the correctness gate.
* `expect` - the exact value the program prints on its last line. The harness
  runs the program once with stdout piped, parses the last non-empty line and
  compares: integers exactly, floats within 1e-9 relative. **Every case is
  therefore also a regression test on the optimiser**, which is what makes the
  suite safe to put in CI even though CI cannot time anything.
* `ir_check` - an optional substring the emitted LLVM IR must contain. This is
  how a case proves it is still measuring what it claims: the call-overhead
  case asserts `call i64 @noinline` is still in the IR at MIR level 1, which
  is the only thing standing between the measurement and a fully inlined loop.
* `base` - an optional path to a paired baseline case. When present, the
  harness subtracts that case's median from this case's median before
  dividing by `ops`. Both cases must declare the same `ops`.
* A case with **no `ops`** is a *probe*: the harness records the printed value
  as a metric (`unit` says what it is) instead of a time. `alloc_bytes.nx` and
  `heap_live.nx` are probes.

A case prints **exactly one number**, on the last line. Nothing else may go
to stdout; the timed runs redirect stdout to null anyway, and the correctness
run parses the last line.

### The harness

`crates/nexum-driver/src/bin/nx-bench.rs`, about 350 lines, no new
dependencies. Commands:

```
nx-bench list                    # every case with its header, parsed
nx-bench smoke                   # compile every case, run once, check the checksum
nx-bench run [pattern]           # time the cases, print the table
nx-bench run --baseline          # time and compare against baseline.json
nx-bench record                  # time and write baseline.json
nx-bench doctor                  # the machine profile and the toolchain versions
```

The procedure for one timed case, in order:

1. **Compile.** Front end to MIR to LLVM IR to an executable, using
   `nexum_mir::optimize_level(&mut mir, mir_opt)` and
   `nexum_toolchain::run_clang_with(&clang, ll, exe, &BuildOpts { opt: clang_opt,
   lto, pgo_use, .. })`. Defaults: `mir_opt = 1`, `clang_opt = 3`, no LTO, no
   PGO. **MIR level and clang level are separate knobs and are recorded
   separately** - this is the fix for the confounding in
   `benchmarks/phase8_compare.ps1`. The flags in force are printed with
   every result and stored in the baseline.
2. **Check.** Run once with stdout piped; parse the last line; compare with
   `expect`; check `ir_check` against the emitted IR. A failure aborts the
   case with a diagnostic - it never produces a number.
3. **Warm up.** Run once more, discarding the time. This warms the page cache
   for the executable and lets the CPU settle.
4. **Measure.** Run 15 times. Each run is `std::process::Command` with stdout
   redirected to null, timed with `std::time::Instant` (monotonic,
   nanosecond resolution). **One process per sample** - the language cannot
   time itself yet, so the process boundary is the measurement boundary.
5. **Statistics.** From the 15 samples: `min`, `p25`, `median`, `p75`, `max`,
   and `noise = (p75 - p25) / median`.
6. **Subtract.** If `base` is given, subtract the base case's median. The
   case's `ops` must equal the base's.
7. **Divide.** `ns_per_op = (median_case - median_base) * 1e6 / ops`.
8. **Warn.** If the case's total elapsed time is under 100 ms, the harness
   warns that the case is too short to trust and that `ops` should be raised.
   Over 30 s, it warns that the case is unnecessarily long.

The report is a table (`case`, `unit`, `ops`, `ns/op`, `min`, `p25`, `median`,
`p75`, `max`, `noise`) written to stdout as text and to
`benchmarks/suite/results/<timestamp>.json` as JSON, and a
`benchmarks/suite/RESULTS.md` is written for the humans.

### The statistics and the verdict

* 15 samples per case, median as the statistic. The median of 15 is used
  because the failure mode on a real machine is a positive tail - a
  scheduler interrupt, a background build, a thermal excursion - which moves
  the median very little and the mean a lot. `min` is reported too, because
  on a quiet machine the min is the cleanest estimator, and the two
  together tell the reader whether the machine was quiet.
* `noise` is the relative interquartile range of the same 15 samples. It is
  the harness's own estimate of how much this machine can be trusted today.
* Verdict against the baseline:

  ```
  threshold = max(0.02, 3 * noise)
  regression  if new_median > base_median * (1 + threshold)
  improvement if new_median < base_median * (1 - threshold)
  otherwise   no change
  ```

  The 2% floor is a floor because a 2% change is below what a quiet machine
  can resolve over 15 runs; the `3 * noise` term is what protects a machine
  that is noisier than usual on the day. A case with a missing baseline, or
  with a baseline recorded on a different machine profile, reports
  `no baseline` rather than a verdict.
* Every number the suite prints is labelled with the machine profile and the
  flags. A number without those two is not a result and must not be quoted.

### The baseline file

`benchmarks/suite/baseline.json`, committed:

```json
{
  "schema": 1,
  "recorded_at": "2026-10-19T14:03:11Z",
  "machine": {
    "profile": "sha256-of-cpu-cores-os-powerplan",
    "cpu": "AMD Ryzen 7 5800X, 8 cores",
    "cores": 8,
    "os": "windows 11 26100",
    "power": "ac",
    "clang": "23.1.3",
    "rustc": "1.95.0"
  },
  "flags": { "mir_opt": 1, "clang_opt": 3, "lto": false, "pgo": null },
  "cases": {
    "micro/call_value": {
      "ns_per_op": 3.11, "ops": 20000000, "runs": 15,
      "samples_ms": [184.1, 185.0, 184.6, ...],
      "noise": 0.021
    }
  }
}
```

The raw samples are kept so that the statistics can be recomputed later
without re-running, and so that a reader can see the shape of the
distribution rather than just a number.

### The machine profile and the discipline

* The profile is a hash of: CPU model string, physical core count, OS and
  version, power plan (Windows) or governor (Linux), and whether the machine
  is running on AC. A baseline recorded under a different profile is refused.
* **Pin to a core** for the measurement (`taskset -c 2` on Linux, `start
  /affinity` on Windows) and record that it was pinned. On a multi-core box
  an unpinned run migrates between cores with different cache and turbo
  behaviour.
* **Do not run the suite while anything else is running**, and not on
  battery. Both are discipline, not tooling, and the profile is what makes
  the absence of them visible.
* Turbining is a real source of drift: the first seconds of a run are often
  faster than the rest. The warm-up run exists for this reason as much as
  for the page cache.
* The suite is single-threaded until 11e. When the concurrency phase lands,
  the profile must record the thread count and the pinning set, and the
  scaling cases must be a separate group.

### The four runtime claims

This is the part the programme actually needs. Each of the four is a claim
that the roadmap makes in prose; each needs a number.

| Claim | The case | What is measured | Compared against | Needs |
| ----- | -------- | ---------------- | --------------- | ----- |
| Syscall overhead | `micro/syscall_write.nx` | ns per `write(2)` of one byte | the same loop in C using the raw syscall; the difference is Nexum's wrapper cost | a builtin that exposes the runtime's `write`; meaningful only after 11a |
| Call overhead | `micro/call_value.nx`, `micro/call_mut.nx` | ns per call, by value and by reference | the same loop in C; and by-value vs by-reference, because `mut` parameters pass an address | an `ir_check` that the call survived the optimiser |
| Allocation cost | `micro/string_concat.nx`, `micro/list_push.nx`, probe `micro/alloc_bytes.nx` | ns per managed allocation; bytes per allocation; bytes per object including the 32-byte header | the same allocation pattern in C with `malloc` | nothing; `concat` and `slice` already allocate exactly once per call |
| Cost of a collected allocation | `micro/gc_pause.nx`, `micro/gc_amortized.nx` | ns per collection as a function of live heap; ns per allocated byte with the collector on | the same allocation pattern in C with `malloc` and `free`: the difference is the amortised collector cost | nothing today, provided the pause case keeps the allocated data **live** and the per-iteration allocation stays under the 64 KiB watermark |

Two subtleties that will otherwise produce wrong numbers:

* **The call case must defeat the MIR inliner.** `inline_small`
  (`crates/nexum-mir/src/lib.rs:1100-1135`) refuses a function with a `mut`
  parameter, with a call in it, with more than 8 instructions, or that is not
  a single block, and it runs to a fixed point, so a callee that calls
  another callee becomes inlinable once the inner call has been inlined into
  it. The case must therefore be written so the callee cannot be inlined at
  all, and the `ir_check` must assert that it was not.
* **The GC pause case must keep its data reachable.** A collection of
  unreachable objects costs almost nothing; a collection of a live heap costs
  the mark phase. The case must hold the allocated data in a live binding
  across the `collect()` call, and must stay under the 64 KiB watermark
  (`crates/nexum-runtime/src/lib.rs:108`) so that the explicit `collect()` is
  the only collection in the measured window.

The syscall row is the one that is **not measurable today**, and the suite
should say so rather than skip it silently: the runtime's `write` wrapper is
not exposed to a program, and until 11a the program reaches the kernel through
libc's `write` anyway (`docs/native-roadmap.md:136-140`). The suite records
the case as `not yet measurable` until the builtin exists, and the programme
does not get to claim a syscall overhead until it does.

### The whole-program benchmarks and their C twins

Three, in `benchmarks/suite/program/`, each with a twin in
`benchmarks/suite/reference/`:

* **`matmul.nx`** - a flat `Float` matrix multiply. Today it must use a
  `List` of `Float` (a flat, doubling, unscanned buffer) or a hand-written
  `[Float; 64]` literal, because there is no array fill. The C twin uses a
  plain `double` array with the same loop structure and no unrolling. Report
  ns per multiply-add and the ratio to C.
* **`parser.nx`** - a byte-level parser over a fixed input: split a buffer on
  a delimiter, parse integers, accumulate. It exercises `str_char_at`,
  `slice`, `concat` and the managed heap, which is where a systems language
  usually loses to C. The C twin parses the same bytes with a pointer walk.
* **`sort.nx`** - a sort of a `List` of `Int` (insertion or heap sort, using
  `list_get`/`list_set`). The C twin sorts a `malloc`'d array with the same
  algorithm. This is the case that shows whether the managed container costs
  anything.

The C twins are compiled with **the same clang and the same flags** as the
Nexum case (`clang -O3`, host target, no `-ffast-math`, no manual unrolling,
no `restrict`), so the only differences are the Nexum front end and the Nexum
runtime. The report prints the ratio, `nexum_ns / c_ns`, and the suite's
acceptance target is stated as a number the programme decides (the
recommendation is >= 80% of C on the whole-program group, with the micro
group held to a tighter bar, but the threshold is the orchestrator's call).

### The model framework's training loop

`cases/ml/train_step.nx`, once phase 11i lands: one training step of a small
model (a two-layer MLP over a fixed input), reporting ns per training step and,
as a probe, the parameter count and the bytes touched. The harness needs no
new capability for it: a timed case with `expect=` is enough, and if the
framework reports several metrics the case can print the one being measured and
the others can be separate probe cases.

### Changes needed elsewhere

These are for the orchestrator; the implementer of this plan should not make
them.

1. **A monotonic clock builtin** (`now_ns() -> Int`), backed by
   `clock_gettime(CLOCK_MONOTONIC)` on Linux and `mach_absolute_time` on
   Darwin, added to the syscall layer in
   `crates/nexum-runtime/src/syscalls.rs` and exposed through
   `crates/nexum-runtime/src/lib.rs` and the codegen's builtin table. This is
   a phase 11a item; until it lands, the suite reports process-level
   differences and the cheap microbenchmarks stay approximate.
2. **A builtin that exposes the runtime's `write`/`read`**, so the
   syscall-overhead case can be written. The wrappers already exist; nothing
   exposes them to a program.
3. **A way to set the collector's threshold** from the language, or a
   documented statement that the 64 KiB watermark is fixed. It determines
   whether the amortised collection cost can be isolated or only measured
   against C's `malloc`.
4. **Array fill and array slice** in 11d, and **`f32`** in 11c. The suite can
   measure a 16x16 matrix multiply today; it cannot measure a 512x512 one,
   and that gap is a language gap, not a benchmarking gap.
5. **A CI job** that runs `nx-bench smoke` on every pull request, so the
   suite's cases are compiled and their checksums verified on all three
   platforms. The timing job is separate and does not gate merges.

## Risks

1. **The MIR inliner silently invalidates a microbenchmark.** This is the
   most likely way for the suite to produce confident nonsense. A call
   benchmark whose callee gets inlined measures an empty loop and will
   report a beautiful, wrong number. Mitigation: the `ir_check` field, plus
   the rule that the benchmark's own README states which optimisation
   levels each case is valid at. If a case is only valid at MIR level 0, the
   header says so and the harness refuses to run it at any other level.
2. **Whole-process timing is a weak instrument.** Every number in the suite
   is a process time minus a process time. On the authoring machine the
   start-up floor is about 10 ms and the run-to-run spread reached 8x on a
   20 ms workload. Mitigation: long workloads (>= 100 ms of measured
   operation), the median of 15 samples, the paired baseline, and the
   explicit statement in the report that the resolution of the cheap cases is
   the difference of two 10 ms-scale measurements. The clock builtin removes
   this risk entirely and should not be deferred long.
3. **The suite rots.** A benchmark suite that is not run is a liability: it
   will fail for unrelated reasons and people will start ignoring it.
   Mitigation: CI runs the smoke mode on every pull request, so a case that
   stops compiling or stops producing the right checksum is caught
   immediately, long before anyone relies on its timing.
4. **Benchmark-driven optimisation.** Once the suite exists, someone will
   start optimising the cases rather than the language. Mitigation: the cases
   are deliberately small and explicit about what they measure, the reference
   C twins keep the comparison honest, and the acceptance criterion is a
   ratio to C rather than a raw number.
5. **The suite measures the backend, not the language.** Nexum's programs are
   compiled by clang, so a "Nexum is fast" claim can be entirely a clang
   claim. Mitigation: the C twin is compiled by the same clang with the same
   flags, so the ratio isolates the front end and the runtime - which is the
   part the programme owns. And the four runtime claims are measured
   separately, because those are the parts the roadmap is changing.
6. **The plan is too big for one sitting.** The risk is real: the harness
   plus 7 cases plus 3 C twins plus the baseline is a lot. Mitigation: the
   first sitting is the harness plus **4** micro cases and **1** whole-program
   case with its twin; the rest of the list in the design section is the
   follow-on, one group per phase, added when the phase lands. The
   directory layout and the header format are the durable part; the case
   list is not.

## Open questions

1. **What is the acceptance threshold?** The recommendation is that the
   whole-program group must reach >= 80% of C and that the four runtime
   claims must each be stated as an absolute number in the release notes. The
   orchestrator must decide the numbers; the suite can only report ratios and
   deltas. Recommendation: adopt 80% of C as the whole-program bar and
   require that no runtime claim is made without a measured number attached.
2. **Is a clock builtin in scope for 11a, or is it a separate change?** It is
   ~50 lines across the syscall layer, the runtime IR and the codegen's
   builtin table, and it is the difference between approximate and exact
   microbenchmarks. Recommendation: add it to 11a, because the syscall layer
   is being rewritten there anyway and a clock is the one syscall the
   benchmark suite cannot do without.
3. **Should the harness be a separate crate after all?** The recommendation is
   `crates/nexum-driver/src/bin/nx-bench.rs` because it needs no
   `Cargo.toml` change and no new dependency. The orchestrator may prefer
   `crates/nexum-bench` for tidiness, at the cost of one line in the root
   `Cargo.toml` and one extra `Cargo.toml` - which will conflict with other
   branches. Recommendation: keep it in the driver for now; move it later if
   the driver crate grows.
4. **What happens to `benchmarks/run.ps1` and `benchmarks/fib30.nx`?** They
   are the Phase 2 baseline and are referenced by `benchmarks/BASELINE.md`.
   Recommendation: leave them untouched as historical artefacts, and let
   `benchmarks/suite/` be the suite from now on. The Phase 8 report should
   stay as it is, with a note that its 2-run numbers are not a measurement -
   the recommendation is that the suite supersedes it rather than that anyone
   edits it.
5. **Is one machine enough?** A baseline is only comparable on the machine
   that produced it, and the programme will have contributors on several
   machines. Recommendation: keep one reference machine and one reference
   profile; contributors on other machines run the suite in smoke mode, and
   the timing run is done by one person or by a scheduled job. The alternative
   - a baseline per machine - multiplies the maintenance and the noise for
   no benefit.
6. **Should the suite gate merges?** Recommendation: **no**, not on timing.
   CI gates on the smoke mode (the cases compile and produce the right
   checksums). Timing regressions are reported by the harness and reviewed by
   a human, because a 3% regression on a shared CI runner is not a signal
   and a merge gate that cries wolf gets disabled. Once the reference machine
   runs the suite on every pull request with a stable profile, a timing gate
   becomes worth having.
