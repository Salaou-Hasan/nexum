# Compiler performance measurement harness

This directory measures **compiler** cost: how long `nx` takes to turn a
`.nx` file into a binary, how much memory it needs while doing it, and how
big the output is. It says nothing about how fast the *generated programs*
run — that is `bench/run.ps1`, which is a different measurement and is not
touched by anything here.

Everything in this directory is self-contained. It creates no files outside
`bench/compiler/`.

```
gen.ps1        deterministic generator of synthetic .nx programs
programs/      the committed corpus + manifest.json (sha256 per file)
measure.ps1    the measurement harness
results/       raw CSV, summary CSV, derived CSV, context JSON, report
README.md      this file
```

---

## 1. Quick start

```powershell
cd C:\nexum\bench\compiler

# one-time: build the compiler under test (the PATH `nx` is v0.4.2 and STALE)
cargo build --release --offline -p nx-driver      # run from C:\nexum

# regenerate the corpus and prove it is deterministic
.\gen.ps1
.\gen.ps1 -Verify

# measure (this is the full run; see "How long does it take" below)
.\measure.ps1 -Runs 7
```

Useful subsets:

```powershell
.\measure.ps1 -Runs 3 -Only 'funcs-*'      # one family
.\measure.ps1 -Runs 3 -Only 'funcs-2000'   # one program
.\measure.ps1 -Runs 3 -SkipO0              # drop the -O0 cross-check
.\measure.ps1 -Runs 3 -NoVerifyRun         # skip the correctness gate
```

`measure.ps1` writes into `results/` and a scratch `results\work-<tag>\`
holding the `.ll` files, objects and exes it produced. Those intermediates
are large (the largest `.ll` is ~13 MB) and are **not** meant to be
committed; `results\` is scratch output.

---

## 2. The program corpus, and why it is not "N lines of code"

Compile time does not scale with line count. It scales with the number of
*distinct things the backend has to do something about*: symbols to declare,
SSA values to number, loops and calls to lower, methods to dispatch. A corpus
that only grows line count measures nothing.

`gen.ps1` therefore offers three families, each with **one** tunable axis and
everything else pinned:

| family  | axis                          | what it stresses |
|---------|-------------------------------|------------------|
| `funcs` | N independent functions       | symbol table, per-function declaration emission, effect summaries, clang's function count. Depth-2, breadth-N call graph; every function is called exactly once so nothing is dead code. |
| `stmts` | S statements in **one** function | per-function cost: one straight-line SSA chain, one huge basic block, LLVM `mem2reg` pressure. The axis most likely to go superlinear. |
| `flow`  | N methods + N `while` loops  | receiver lowering (`self`, `mut self`), record layout, loop and call lowering. |

Committed ladder (fixed seed `20260902`):

```
funcs-50    17,018 B     stmts-20     1,843 B     flow-30     18,513 B
funcs-200   66,983 B     stmts-100    4,462 B     flow-120    73,395 B
funcs-800  267,517 B     stmts-400   14,298 B     flow-480   299,950 B
funcs-2000 671,657 B     stmts-1500  50,375 B
```

Total corpus **1.42 MB**, largest single file **656 KB**. That is the cap
this directory imposes on the repository: the interesting superlinearity
shows up well below a megabyte of source, and past that point the repo cost
is not repaid. Larger sizes are still available for probing without
committing them:

```powershell
.\gen.ps1 -Family funcs -Sizes 'funcs=6400' -OutDir $env:TEMP\nxprobe
```

### Determinism

There is no randomness anywhere. The generator uses one fixed-seed 31-bit
LCG (`Get-Rand`) to pick operators and operands, so the corpus is
byte-identical on every run and every machine. `-Verify` regenerates the
whole set into a scratch directory and compares SHA256 of every file against
the committed copy, exiting non-zero on any mismatch. Run it after any edit
to `gen.ps1`; `manifest.json` carries the hashes so drift is detectable even
without re-running the generator.

Files are written UTF-8 **without** BOM. PowerShell 5.1's `Set-Content
-Encoding UTF8` would add one and the lexer would see a stray codepoint at
offset 0.

### Why the arithmetic looks the way it does

Every generated value is masked with `& 65535`, so all values stay in
`[0, 65535]`. The programs cannot overflow i64, cannot divide by zero, and
cannot run long: the driver loops are pinned at 3-4 iterations. This keeps
the *generated program* semantically boring, which is the point — we are
measuring the compiler, not the program — while still exercising the
unboxed `Int` path.

---

## 3. What is measured

Seven modes, run per program:

| mode | command | what it isolates |
|------|---------|------------------|
| `check` | `nx check <f>` | frontend + type check only (`nx_types::check_source`) |
| `dump-ir` | `nx dump-ir <f>` | effect/summary analysis only (`nx_ir::analyze`), no type check |
| `emit-ir` | `nx build <f> --emit-ir` | check + infer + mem plan + effect analysis + codegen + IR serialisation. **No clang.** |
| `build` | `nx build <f> -o out.exe` | the whole pipeline |
| `clangc-O2` | `clang -O2 -c <ll> -o out.obj` | LLVM on our IR, **no link** |
| `clang-O2` | `clang -O2 <ll> -o out.exe` | LLVM + native link |
| `clang-O0` | `clang -O0 <ll> -o out.exe` | unoptimised; the O0→O2 gap is optimiser cost |

`emit-ir` is the load-bearing measurement. `build_ir()` in
`compiler/nx-driver/src/main.rs` is shared by `build --emit-ir` and
`build_exe`, so `emit-ir` runs exactly the same Nexum-side work that `build`
does; the only thing `build` adds is the `.ll` write, a `clang --version`
probe, and clang itself.

`emit-ir`'s stdout is captured to a **real file**, not to `NUL`. That is the
same string `build` writes to `%TEMP%\nxbuild-<pid>.ll` before handing it to
clang, so the number is the faithful "Nexum's own work" cost rather than a
pipe cost.

Three bounds fall out of the three cheapest modes:

* `dump-ir` − floor ≈ lex + parse + effect analysis
* `check` − floor ≈ lex + parse + type check
* `emit-ir` − floor ≈ all of the above, twice, plus codegen

The lexer, parser and type checker cannot be separated from each other
through the CLI surface — see "What could not be measured".

### The clang/Nexum split

`build` time decomposes as

```
build = emit_ir + clangc + link_and_driver
            ^         ^           ^
            |         |           native link + the `clang --version` probe +
            |         |           the .ll write + the up_to_date() check
            |         LLVM parse + optimise + object emission, on our IR
            Nexum's own work: check + infer + mem plan +
            effect analysis + codegen + IR serialisation
```

reported two independent ways so the subtraction can be checked:

* **subtraction** — `clang_est = med(build) - med(emit-ir)`
* **direct** — `clang_direct = med(clang-O2)`, measured by invoking clang
  yourself on exactly the `.ll` that `nx` produced

If those two agree, the split is trustworthy. If they disagree, the
subtraction is not, and the report should say so.

### Phase attribution, from the source

These are properties of the current implementation that the numbers below
have to be read against. From `compiler/nx-codegen/src/lib.rs`,
`compile_opts()` (called by `compile_entry`):

* **Type inference runs twice per build.** `build_ir` calls
  `nx_types::check_source`, then `compile_entry` calls
  `nx_types::infer_program` again for the same program (line ~1169).
* **The whole AST is deep-cloned at least twice more.** `analyze_map(loader
  .programs.clone())` (line ~1191) clones every module's `Program`; then
  `emit_module`/`declare_module_fns` each do `g.programs.get(module).cloned()`
  (lines ~1207, ~1210). For a single-file program that is three full AST
  copies. This is linear in program size but with a very large constant, and
  it is the most likely explanation for the compiler's memory profile.

---

## 4. Methodology

### Clock

`System.Diagnostics.Stopwatch`, started immediately before
`Process.Start` and stopped the instant `WaitForExit` reports the child gone.

### Peak memory — the Win32 trap

`Process.PeakWorkingSet64` is sampled **in a 1 ms poll loop while the child is
still alive**. Reading it after `WaitForExit()` returns 0 on a handle
PowerShell did not create; that was verified directly here (`nx check` on a
one-line file read `0` after exit and `3.0 MB` when sampled during the run).
The sampler is a C# helper compiled with `Add-Type` at harness startup
(`NxProc.Run`), because `Start-Process -PassThru` returns a `Process` whose
handle PowerShell does not retain — `.Handle` must be touched first or
`PeakWorkingSet64`/`ExitCode` throw — and because `Start-Process` costs
~7 ms of PowerShell overhead per launch, which is the same order as the
fastest thing measured here.

A consequence worth stating plainly: **for processes that live for less than
a few milliseconds there is no peak-memory sample.** `peak_sampled` in the
summary CSV records whether any sample was taken.

**`nx` spawns clang as a child process, so `nx`'s peak working set does not
include clang.** Measured directly: `nx build` peaks at almost exactly the
same value as `nx build --emit-ir` does, on every program. The pipeline peak
is therefore reported as two columns and their sum, never as one number:
`peak_nx_mb` (our process) and `peak_llvm_mb` (clang `-O2 -c`), plus
`peak_pipeline_mb`.

Use `peak_llvm_mb` — from `clang -O2 -c` — rather than the peak of
`clang -O2 <ll> -o exe`, when you want to know how LLVM's memory scales with
IR size. With a link in the command, the linker is a *child* of clang and its
memory is outside clang's working set, so the `clang-O2` peak is both
smaller and not the quantity of interest.

### Three Windows traps this harness had to be written around

Recorded because each one produced a confidently wrong number before it was
found, and each would do the same to the next person.

**1. `$null` from PowerShell is `""` in C#.** Binding PowerShell's `$null`
to a C# `string` parameter yields `String.Empty`, not `null`. A drain helper
that tested `stdoutPath == null` therefore took the *file* branch and called
`new FileStream("")`, which throws. The exception was raised **inside the
drain thread's `catch {}`**, so the thread died silently, and the child then
blocked forever writing to a full stdout pipe (Windows anonymous pipes carry
4 KB, not 64 KB). Symptom: `nx dump-ir` appeared to hang for the full
120 s timeout, 19 times out of 20 — while standalone it takes **0.05 s**. The
real tell was in the child's own stderr, which Rust prints to the harness
console: `failed printing to stdout: The pipe is being closed. (os error
232)`. Fixed by testing `string.IsNullOrEmpty`, opening the sink *before*
`Process.Start`, and reporting a drain failure instead of swallowing it.

**2. `PeakWorkingSet64` is zero after exit.** Covered above. It is the
difference between a memory column that means something and one that reads
`0`.

**3. Each clang mode must have its own `-o` path.** All of `nx build`,
`clang -O2` and `clang -O0` writing to `prog.exe` means whichever ran last
defines the recorded "output binary size". `prog.exe`,
`clang-O2.exe` and `clang-O0.exe` are now distinct.

### Warm-up

One untimed pass over every mode per program before sample 1, so the `nx`
binary, the source file, the work directory and the `.ll` are all resident.
The warm-up line is printed so a failure in it is visible.

### Sample count and ordering

Default `-Runs 7` timed samples per (program, mode), plus 1 warm-up.

Modes are **interleaved inside a repeat**, not run in blocks: all modes of
program P run, then all modes again, and so on. A background-load episode on
a shared box therefore perturbs all modes of one repeat equally instead of
poisoning one whole mode for the whole run.

### The floor, and load rejection

This machine has a **process-launch floor of roughly 17-25 ms** (`nx
--version` does essentially no work and still costs that; `clang --version`
costs 30-40 ms). The harness measures this floor at the start of every run
and records it in `compiler-<tag>-context.json`. Nothing below it is
resolvable: `check` on a small program is *at* the floor and its value is
not a measurement of the type checker.

This box is shared with other concurrent work and the load is episodic and
severe. It was observed inflating a single mode by 3-6x: `nx dump-ir` on
`funcs-800` measures **0.098 s** standalone and **29.9 s** inside a loaded
repeat. Two mitigations, both explicit:

1. Every repeat carries its own floor probe (`nx --version`, 3 samples,
   minimum).
2. A repeat whose floor exceeds **1.5x** the quietest floor observed anywhere
   in the session is dropped from the statistics. The number kept and the
   number dropped are both in `*-summary.csv` (`n`, `n_dropped`); the dropped
   rows are still in `*-raw.csv`. Nothing is silently discarded.

`floor_ms` values observed, and how many readings survived the filter, are
printed in the run's `*-report.txt`.

### Statistics

Median is the headline; `min`, `max`, `sd` and `cv%` are all reported, and
every raw sample is in `*-raw.csv`. Median because a background process can
steal a timeslice; min because it is the most robust estimator of the true
cost under contention. Where they disagree sharply, the run is suspect.

### Forced rebuild

`up_to_date()` in the driver short-circuits on a warm exe and prints
"up to date" instantly. Before **every** timed `build` the harness deletes
both the output `.exe` and its `.nxstamp` sibling. Without that the mode
would measure a `stat()` call.

### Correctness gate

Timings are worthless if the corpus does not compile to something real, so
before timing each program the harness requires:

1. `nx build` succeeds and the exe produces non-empty output;
2. a second, independent build from scratch produces byte-identical output
   (codegen determinism);
3. for programs up to `-VerifyMaxBytes` (default 150,000 B), a second build
   with unboxing disabled produces byte-identical output, so a representation
   change is not also a semantics change.

If any check fails the harness aborts and discards all timings.

Requirement 3 used to compare against the tree-walking interpreter, which is
what made it a real oracle rather than a self-consistency check. That
interpreter has since been deleted, so the check is now unboxed against
boxed. It is weaker, and honestly so: it cannot catch a bug both
representations share -- and a bug of exactly that shape did exist. A
function reading a global only inside a dict literal was memoized and
returned a stale value, and both builds agreed on the wrong answer. That is
why `compiler/nx-e2e` asserts expected values rather than agreement.

---

## 5. Output files

| file | contents |
|------|----------|
| `compiler-<tag>-raw.csv` | one row per (program, mode, repeat): `seconds`, `peak`, `polls`, `rep_floor_ms`, `exit`. Every sample, including dropped ones. |
| `compiler-<tag>-summary.csv` | one row per (program, mode): `n`, `n_dropped`, `med_s`, `min_s`, `max_s`, `mean_s`, `sd_s`, `cv_pct`, `peak_mb`, and the sorted sample list |
| `compiler-<tag>-derived.csv` | the phase split per program: check, emit-ir, codegen, clang-c, link/driver, build, `.ll` and `.exe` bytes, peaks, percentages, floor-subtracted columns |
| `compiler-<tag>-context.json` | machine, tool versions, floor measurements, load factor, per-repeat floors |
| `compiler-<tag>-report.txt` | the human-readable tables |

`<tag>` defaults to a timestamp; pass `-Tag` to make it stable.

`n_error` in the summary CSV counts runs that timed out or exited non-zero.
Those are **excluded** from every statistic: a single 120 s timeout among
seven 0.05 s runs would otherwise become the median. They remain in the raw
CSV, and any (program, mode) pair with no valid run left is listed in the
report under `ERRORED` and omitted from the derived table rather than
reported as a number.

---

## 6. Reproducing a number

Compare `context.json` first. If the floors are in a different band from
yours, your machine is under different load and the absolute seconds are not
comparable — only the ratios are. `programs/manifest.json` must match
(`gen.ps1 -Verify`) or you are not measuring the same input.

The compiler under test is pinned by path, not by `PATH`: `-NxExe` defaults
to `C:\nexum\target\release\nx.exe`. **The `nx` on `PATH` is v0.4.2 and is
one release behind the tree** — using it would silently measure
different code.

### How long a full run takes

`-Runs 7` over the 11 committed programs, 7 modes, plus the correctness gate
and the floor probes, is roughly 10-20 minutes on this box — the spread is
the box, not the harness. `-Runs 3 -SkipO0` is about 40% of that. The
correctness gate builds every program twice, which is why a `-Runs 1` run is
not instant.

---

## 7. What this harness does NOT measure

* **Lexer / parser / type-checker individually.** The CLI exposes `--lex` and
  `--parse`, but both *print* their result — `--parse` dumps the whole AST
  with `{:#?}`, which costs far more than parsing does. There is no
  "parse only, print nothing" mode, and adding one would mean changing the
  driver, which is out of scope here. `check` and `dump-ir` give upper and
  lower bounds on the frontend as a group instead.
* **Per-module cost.** Every generated program is a single module. The
  loader, `analyze_map` and `nx_mem::plan` all iterate `HashMap<String,
  Program>`, so multi-module scaling is untested.
* **Peak memory *including* the linker.** `nx` spawns `clang`, which spawns
  the native linker. `nx`'s and clang's peaks are reported separately; the
  linker's are not captured at all. `peak_pipeline_mb` is `nx + LLVM`, which
  is a lower bound on the true whole-pipeline peak.
* **Anything about the generated program's speed.** That is `bench/run.ps1`.
* **In-process compilation.** Every number is a whole-process measurement and
  therefore includes the ~17-25 ms process-launch floor. Nothing here
  measures a library API, because there is no library API — the driver is a
  binary and the crates expose `check_source` / `compile_entry` directly.
* **Incremental / warm builds.** `up_to_date()` is deliberately defeated
  before every timed build, so the "nothing changed" path is never measured.
* **The `nx` on `PATH`.** See above; it is a release stale.
