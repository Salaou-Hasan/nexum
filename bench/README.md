# Nexum vs Rust

Same algorithms, both compiled to native code, both at `-O2`.
Rust is built with `rustc -O` on a single crate (no dependencies, no
`#[inline(never)]` tricks) so the comparison is compiler against
compiler, not configuration against configuration.

Every benchmark is a pair of source files that must print the same
thing. `run.ps1` builds both, checks the outputs match, then times
each over several runs and reports the median.

## The four things being compared

| Benchmark | What it stresses |
| --- | --- |
| `fib` | call overhead and recursion; NX memoizes, Rust does not |
| `mandel` | float math in a tight loop; NX unboxes, Rust is native |
| `nbody` | struct-of-arrays float math, the classic floating-point workload |
| `matrix` | nested-loop float multiply-add |

## Reading the results

- **Correctness is the gate.** A benchmark only counts if both
  programs print identical output. `run.ps1` fails loudly otherwise.
- **NX's automatic memoization** is the whole point of the `fib` row
  and is not something Rust does for you. That row measures a language
  feature, not raw code quality. `NX_NOMEMO=1` shows the same program
  without it.
- **NX's unboxing** is a real optimization but a partial one. Function
  boundaries stay boxed in v0, so a call-heavy loop still pays for the
  ABI. Rust monomorphizes and inlines, so it has no such boundary.
- Numbers come from one machine and one run. Treat them as indicative,
  not as a verdict.

## Reproducing

```powershell
powershell -File bench\run.ps1
```

Flags: `-Runs <n>` for timing repetitions (default 5), `-SkipRust` to
run only the NX side, `-NoUnbox` to add an all-boxed NX column.
