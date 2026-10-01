# Nexum vs Rust

Same algorithms, both compiled to native code, both at `-O2`. Rust is
built with `rustc -O` on a single dependency-free crate, so this is
compiler against compiler rather than configuration against
configuration.

Every benchmark is a pair of sources that must print the same thing.
`run.ps1` gates on that: a benchmark whose outputs disagree is reported
as a failure and its timings are discarded, because a fast wrong answer
is worth nothing.

## Results

Intel Core 7 240H, 16 logical cores, Windows, clang 23.1.2 and
rustc 1.95. Median of 7 runs.

| Benchmark | NX | Rust | rust/nx | NX all-boxed |
| --- | --- | --- | --- | --- |
| `fib` | 0.011s | 0.028s | **0.40x** | 0.018s |
| `mandel` | 3.275s | 0.626s | 5.23x | 9.223s |
| `matmul` | 0.232s | 0.023s | 10.01x | 0.458s |
| `listsum` | 0.142s | 0.022s | 6.31x | 0.159s |
| `dot` | 0.112s | 0.030s | 3.78x | 0.112s |

`rust/nx` above 1.00 means Rust was faster. All five print identical
output on both toolchains.

## What each row measures

| Benchmark | Shape |
| --- | --- |
| `fib` | recursion and call overhead; NX memoizes, Rust does not |
| `mandel` | float math in a tight loop, one call per point |
| `matmul` | 260x260 dense float multiply, building the result list |
| `listsum` | build a list with `push`, then walk it twice |
| `dot` | read-only: two list reads and a multiply-add per element |

## Reading the results honestly

**NX wins `fib` and it is not a codegen win.** Automatic memoization is
a language feature Rust does not have. The all-boxed NX build ties Rust
almost exactly (0.018s vs 0.028s), which says the two backends produce
comparable code for this shape. What NX adds is the compiler noticing
the function is pure and caching it.

**Unboxing is worth 1.1x to 2.8x** on the numeric loops (`mandel` 9.223
to 3.275 is the clearest). It is real, and it is partial: function
boundaries stay boxed, and it does nothing about memory layout.

**Rust wins the numeric loops by 4x to 10x, and the reason is the value
representation, not the arithmetic.** Both toolchains emit the same
scalar `mulsd`/`addsd` and neither vectorizes the reduction. The cost is
per element:

- An NX list element is a 24-byte tagged `%NxVal`; a Rust `Vec<f64>`
  element is 8 bytes.
- An NX read goes through `nx_index`, which checks the tag, adjusts a
  negative index, and range-checks twice, then extracts the payload and
  bitcasts it. A Rust read is one bounds compare and a load.
- `matmul` is worse than `dot` (10x vs 3.8x) because NX has no indexed
  assignment, so the result list is built with `push`, and each `push`
  is a length/capacity check, a possible `realloc`, and a 24-byte
  store. Rust pre-allocates and writes by index.

So the gap is the dynamic value representation and the indexing path,
which unboxing does not address. The fix is to specialize a list of
proven scalars into a flat array, the way Rust's `Vec<f64>` already is.

**`listsum` and `dot` overlap on purpose.** Both index a list in a
loop; `dot` adds a multiply. They land close together, which says the
indexing path, not the arithmetic, is the cost.

## Language differences that shaped the benchmarks

These are real gaps in NX, not benchmark artifacts, and each one forced
a benchmark to be written a particular way:

- **No indexed assignment.** `a[i] = v` does not parse. Every list here
  is built with `push` and then read-only.
- **No `%` operator.** The fills use `x - (x / m) * m`, which is exact
  for the non-negative values involved.
- **No structs or tuples.** n-body would need a contrived
  parallel-list layout, so it was dropped rather than benchmarked
  through a workaround that measures the workaround.
- **Comments are `#`, not `//`.**

## Reproducing

```powershell
powershell -File bench\run.ps1
```

Flags: `-Runs <n>` timing repetitions (default 5), `-SkipRust` for the
NX side only, `-NoUnbox` to add the all-boxed column.

To see memoization's contribution separately, rebuild one benchmark
with `NX_NOMEMO=1`.

