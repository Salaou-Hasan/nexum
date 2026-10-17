# Compiler performance report (Phase 5)

Generated: 2026-10-17 12:26  
nx: target\debug\nx.exe  

| example | opt | runs | min (ms) | median (ms) |
| ------- | --- | ---- | -------- | ----------- |
| demo | 0 | 2 | 26.65 | 38.16 |
| demo | 1 | 2 | 17 | 26.15 |
| fact | 0 | 2 | 15.65 | 25 |
| fact | 1 | 2 | 15.78 | 23.12 |
| hello | 0 | 2 | 21.53 | 22.4 |
| hello | 1 | 2 | 16.45 | 22.8 |
| loop | 0 | 2 | 18.69 | 26.1 |
| loop | 1 | 2 | 18.34 | 23.26 |
| particles | 0 | 2 | 20.03 | 25.28 |
| particles | 1 | 2 | 22.4 | 29.02 |
| segments | 0 | 2 | 20.15 | 25.32 |
| segments | 1 | 2 | 24.98 | 43.74 |
| strings | 0 | 2 | 27.23 | 29.34 |
| strings | 1 | 2 | 21.94 | 32.2 |

Notes:
- Compile time is front-end + MIR + codegen + IR text emission (no clang).
- Opt level 1 runs const-fold, DCE and small-function inlining.
- `nx` is a debug build; release numbers will differ.
