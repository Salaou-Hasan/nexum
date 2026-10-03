# Coverage audit workloads

Twelve benchmark pairs that fill the gaps `bench/` had. Same convention as
`bench/run.ps1`: each workload is a pair of sources that must print
identical output, correctness is the gate, and the timings of a disagreeing
pair are discarded.

```powershell
powershell -File bench\workloads\run-workloads.ps1
```

Flags match the parent runner: `-Runs <n>` timing repetitions (default 5),
`-SkipRust` for the NX side only, `-Only <a,b>` to restrict the set.

## Why these twelve

The five benchmarks in `bench/` are `fib`, `mandel`, `matmul`, `listsum`
and `dot`. Between them they contain no string, no dict, no record, no
`type`, no `impl`, no method, no `for`-over-a-list, no
module and no data structure other than a flat list of numbers. What they
do cover well is float arithmetic in a tight loop and indexed reads from a
flat list, and they cover that carefully.

The gaps, and the workload that closes each:

| Gap in `bench/` | Workload |
| --- | --- |
| string construction, concatenation, slicing | `strbuild` |
| string indexing, `in`, `<`, iteration, slicing | `strscan` |
| dicts: build, update, read, miss, delete, iterate | `dictops` |
| sorting | `sortint`, `sortstr` |
| the integer operator group and strided writes | `sieve` |
| mixed control flow: backtracking, DP, data-dependent exit | `flowctl` |
| recursion depth, call breadth, copy-on-bind | `recursion` |
| allocate/free pressure on lists and dicts | `churn` |
| records, methods, `mut self`, value semantics | `records` |
| an application-shaped program | `textstat` |

## Results

Intel Core 7 240H, 16 logical cores, Windows, clang 23.1.2, rustc 1.95.
Median of 5 runs via `bench/workloads/run-workloads.ps1`. NX is built with
`nx build` (LLVM IR handed to clang -O2); Rust with `rustc -O` on a single
dependency-free crate. All twelve pairs print identical output.

| Workload | NX | Rust | rust/nx | NX peak RSS | Rust peak RSS |
| --- | --- | --- | --- | --- | --- |
| `churn` | 0.361s | 0.054s | 6.73x | 461 MB | 3.8 MB |
| `dictops` | 0.492s | 0.023s | 21.65x | 5.1 MB | 2.2 MB |
| `flowctl` | 0.425s | 0.032s | 13.36x | 5.7 MB | 2.8 MB |
| `records` | 0.343s | 0.015s | 22.30x | 379 MB | 3.2 MB |
| `recursion` | 0.371s | 0.036s | 10.31x | 594 MB | 2.9 MB |
| `sieve` | 0.328s | 0.081s | 4.03x | 157 MB | 42 MB |
| `sortint` | 0.456s | 0.128s | 3.57x | 53 MB | 19 MB |
| `sortstr` | 0.547s | 0.137s | 4.00x | 11 MB | 6.9 MB |
| `strbuild` | 0.336s | 0.018s | 18.98x | 773 MB | 2.2 MB |
| `strscan` | 0.357s | 0.026s | 13.84x | 198 MB | 4.1 MB |
| `textstat` | 0.291s | 0.031s | 9.51x | 723 MB | 2.4 MB |


**Read the small Rust numbers with care.** Process launch plus exit in this
harness costs 0.007s to 0.015s per run, measured with a one-line program
on each toolchain over four trials. Rust-side times at or below ~0.02s
(`dictops`, `records`, `strbuild`) are inside that noise, so their ratios
are upper bounds rather than measurements.

## Reading the columns honestly

**Peak RSS is the more interesting half of this table.** NX has no garbage
collector and no allocator to speak of: `runtime.ll` frees container
storage on scope exit but never frees a string, so every string the program
concatenates is live until the process exits. `strbuild` (773 MB),
`textstat` (723 MB) and `recursion` (594 MB) are almost entirely strings
that will never be reclaimed, against a few megabytes for the Rust
reference. That is not a constant factor to tune away; it is the shape of
building a string in NX today, because `s + t` is the only way to grow one.

**The numeric gaps are smaller than the container gaps.** `sortint` at
3.57x is the best NX does here, and it is the workload where almost every
operation is an integer comparison plus two indexed accesses -- the same
path `listsum` and `dot` already exercise. The 13x to 22x rows are all
dominated by something that is not arithmetic: a dict scan (`dictops`), a
container clone (`records`, `recursion`), a quadratic string build
(`strbuild`, `textstat`), or a per-character allocation (`strscan`).

`parwork` was removed along with the feature it measured. `parallel:` asked the
scheduler to prove race-freedom statically; the proof had holes, and a program
could print the wrong answer whenever it lost a race. Correctness was worth
more than the one row where NX beat Rust, so the feature went rather than the
row.

## Language limitations these workloads had to work around

Each of these is a real constraint, not a benchmark artefact, and each one
changed how the source is written. They are the reason several workloads
are shaped the way they are rather than the way they would ideally be.

- **No string builder.** `s + t` allocates a new string and the old one is
  never freed, so growing a string is quadratic in the bytes copied. Rust
  uses `push_str`, which is amortised O(1); `strbuild` therefore measures a
  missing stdlib feature more than it measures codegen.
- **No `sort`, no `split`, no `str()`.** The minimal stdlib is Stage 3 work
  and is not present (`grammar.md` section 4.3). `sortint` and `sortstr`
  are hand-written quicksorts; `textstat` hand-rolls its tokenizer and its
  integer-to-string conversion.
- **Argument passing copies containers.** A recursive traversal that passes
  its container as an argument clones the whole container per node. For a
  524288-element table that is about 2.7e11 element copies, and the process
  dies before finishing it. `recursion` reads its table from a module-level
  binding instead, and `sortint` is iterative with an explicit stack for
  the same reason.
- **`push` needs a list variable.** `push(d[k], v)` is refused, so a list
  stored in a dict cannot be grown in place.
- **A missing dict key is a runtime error.** There is no `get`-with-default
  on the surface, so every probe is guarded by `k in d`.
- **`for` binds a fresh copy.** A `mut self` method called on a `for` loop
  variable writes back into the loop variable and leaves the collection
  untouched, silently. `records` measures the working spelling (indexed)
  and the non-working one (loop variable) side by side.
- **Recursion has a hard ceiling.** The interpreter refuses at 500 frames
  (`CALL_LIMIT` in `nx-interp`); the native backend survives about 7000 and
  then takes a real stack overflow with no diagnostic. `recursion` is capped
  at depth 350 so it stays inside both.
- **No file I/O.** Every workload generates its own data, which keeps them
  deterministic and keeps I/O out of the timings.

## Defects found while writing these

Reproduced on the native backend; each is a program the interpreter gets
right.

- **Stepped string slices read uninitialised memory and write out of
  bounds.** In `runtime.ll`'s `nx_slice`, the string path computes the
  destination offset as `source_index - from` instead of the packed output
  offset, so `s[::2]` on `"abcdef"` returns `"a"` natively and `"ace"` under
  the interpreter. With `step > 1` the write positions run past the end of
  the `malloc(cnt)` buffer. `strscan` deliberately uses only unit-step
  slices.
- **A recursive function with a list argument can crash the native
  backend.** With the default flag settings the process dies with an access
  violation; `NX_NOUNBOX=1` or `NX_NOMEMO=1` avoids it, and so does making
  the recursive function impure so the memoization pass skips it. Minimal
  reproducer is a four-line n-queens guard. `flowctl` uses three bitmasks
  instead of a column list, and `recursion` reads its table from a global.
- **`i64::MIN / -1` returns garbage natively.** `nx_index` is fine, but
  integer division overflow is unchecked, so the value that comes back is
  whatever the hardware divide left behind. No workload divides by -1.
- **Float printing trims trailing zeros from the integer part.** `1e14`
  prints as `1` and `9.007199254740992e15` prints with a spurious `.0`
  suffix (`fmt_float` in `nx-interp`, `nx_fmt_float` in `runtime.ll`).
  Every float result in `sieve`-style workloads is therefore printed as an
  exact integer, which both toolchains format identically.
- **The interpreter rejects a chained assignment into a dict.**
  `m["k"][0] = v` is refused with "index must be Int, found k" while the
  native backend accepts it and does the right thing. `d[0]["x"] = v` and
  `g[0][1] = v` work in both.
- **Integer overflow is checked by the interpreter and ignored natively.**
  The interpreter raises "integer overflow"; the native backend wraps like
  `rustc -O`. No workload overflows.

## What was not covered

- `bench/README.md` claims NX has no `%` operator and no indexed
  assignment. Both are wrong: `%` follows Python's sign convention and
  `a[i] = v` works. The two existing benchmarks work around limitations
  that do not exist. This directory's workloads use both freely.
- No workload uses `modules`, `del` on a name, `assert`, the ternary, or
  comprehension over a dict, because none of them is a performance
  question. They are in `examples/syntax.nx`.
