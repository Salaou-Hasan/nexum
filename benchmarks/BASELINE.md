# Benchmark baseline

## How to run

```
cargo build -p nexum-driver
powershell -ExecutionPolicy Bypass -File benchmarks\run.ps1 -Nx target\debug\nx.exe
```

Each benchmark is compiled by `nx emit-ir` to LLVM IR, linked with
`clang -O2`, warmed up once, then timed over N runs (default 7). Results are
appended to `benchmarks/results.csv`.

## Baseline (Phase 2, 2026-10-17)

| benchmark | runs | min (ms) | median (ms) |
| --------- | ---- | -------- | ----------- |
| fib30     | 7    | 53.97    | 59.77       |

Environment: Windows x86_64, clang 23.1.3 (LLVM), `nx` debug build,
clang `-O2`, no LTO/PGO yet.

Caveats:

* The `nx` compiler itself is a debug build; compile-time numbers later will
  depend on `--release`.
* Optimization currently relies on clang/LLVM (`-O2`) **and** MIR-level passes
  (const-fold, DCE, small-function inlining) at `--opt-level 1`. See
  `benchmarks/compiler_perf.md` for compiler-side timings.
* Timer resolution and process startup dominate for small benchmarks; keep
  workloads ≥ ~10 ms.

## Phase 8 baseline (optimization maturity)

See `benchmarks/phase8_report.md` for the full comparison. MIR-level passes (const-fold, DCE, inlining) land via `--opt-level 1`. PGO/LTO available via `nx build --release` / `--lto` / `--pgo-generate` / `--pgo-use`. Compile-time numbers in `benchmarks/compiler_perf.md`.
