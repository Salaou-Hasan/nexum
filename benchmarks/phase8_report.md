# Phase 8 optimization comparison

Generated: 2026-10-17 12:47  
nx: target\debug\nx.exe  

| benchmark | level | runs | min (ms) | median (ms) |
| --------- | ----- | ---- | -------- | ----------- |
| fib30 | -O0 | 2 | 21.47 | 24.25 |
| fib30 | -O2 | 2 | 14.84 | 16.36 |
| fib30 | -O3 | 2 | 17.02 | 17.9 |
| fib30 | -O3+LTO | 2 | 28.08 | 30.84 |

Notes:
- MIR-level passes (const-fold, DCE, inlining) are on for -O3/-O3+LTO via --opt 1.
- -O0/-O2 use --opt 0 (no MIR passes; clang optimizes).
- `nx` is a debug build.
