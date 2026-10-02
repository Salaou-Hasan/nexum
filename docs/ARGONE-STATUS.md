# ARGONE stage status

The completion checklist for the Argone stage. Mirrors the completion
criteria in `Nexum_Argone_Unified_AOT_Prompt.md` section 36, organized by
task.

**This file is the gate.** `tools/argone-gate.ps1` reads it and exits
non-zero if Argone is not complete. CI runs that script.

This file is deliberately ASCII-only. It is parsed by a script and printed
into a console table, and a BOM-less UTF-8 character renders as something
else in both places.

## How to read this file

Each task is `not started` or `complete`. There is no third state, and that
is the point. The gate script rejects `in progress`, `partial`, `mostly`,
`pending` and `blocked` as statuses, because a stage that admits a third
state is a stage that gets left halfway.

A task becomes `complete` when every gate item under it is `[x]` and the
task carries an Evidence line naming a commit, a file, or a measurement. A
bare `[x]` with no evidence is treated as incomplete, which is what makes
"scaffolding exists" insufficient.

`Blocked by:` is informational. It records a dependency; it is not a
status and does not affect the gate.

Task definitions and gates: `docs/ARGONE.md`.

---

## Task 0 - Toolchain feasibility

Status: not started

- [ ] An MLIR toolchain is invocable (`mlir-opt --version` succeeds)
- [ ] A workspace crate can construct and lower MLIR from Rust
- [ ] The route is reproducible from a clean checkout
- [ ] The decision is documented, including rejected alternatives
      Evidence: docs/architecture/toolchain.md

## A - Full architecture audit

Status: not started

- [ ] All 24 items of section 33 present, none a placeholder
- [ ] Every claim cites a file and function
- [ ] Reviewed against the tree rather than from memory
      Evidence: docs/architecture/audit.md

## B - Baseline tests and benchmarks

Status: not started

- [ ] Every section 23 benchmark category covered or explicitly not applicable
- [ ] bench/baseline.json committed and regenerable
- [ ] Compile time, binary size, runtime, memory, and startup all measured
- [ ] A recorded run on an idle machine, with noise characterized
      Evidence: bench/baseline.json, tools/bench-baseline.ps1

## C - Shared semantic structures

Status: not started

- [ ] nx-semantics crate exists and is consumed by all five AST walkers
- [ ] AST match arms for resolved facts measurably reduced from 100 Stmt / 133 Expr
- [ ] No crate re-derives a fact another crate already resolved
- [ ] 13/13 examples still agree across interpreter / native / NX_NOUNBOX
- [ ] All pre-existing tests pass unchanged
      Evidence: compiler/nx-semantics/, arm-count table in the audit

## D - HIR

Status: not started

- [ ] nx-hir is a real compiler layer, not an AST alias
- [ ] Every construct in the 13 examples lowers to HIR
- [ ] HIR is name-free; identifiers survive only in diagnostics
- [ ] Every HIR node has a resolved type
- [ ] A verifier rejects malformed HIR, with negative tests
- [ ] `nx dump-hir` is stable enough to diff in tests
      Evidence: compiler/nx-hir/

## E - MIR / SSA

Status: not started

- [ ] nx-mir has basic blocks, terminators, SSA values, explicit alloc/load/store
- [ ] No name-based reasoning anywhere in the MIR builder
- [ ] Single-assignment verified, with negative tests
- [ ] Dominance verified, with negative tests
- [ ] Every block terminated and reachable, with negative tests
- [ ] `nx dump-mir` round-trips stably
      Evidence: compiler/nx-mir/

## F - MIR verification and core analyses

Status: not started

- [ ] Representation analysis is value-keyed, not name-keyed
- [ ] A value stays unboxed across a region without box/unbox churn, proven by test
- [ ] SCCP, constant propagation, DCE, CSE, copy propagation, CFG simplification present
- [ ] Each pass has an equivalence test
- [ ] Each pass is benchmarked before and after
- [ ] 13/13 examples agree on all three paths after all passes
      Evidence: benchmark tables in the audit

## G - Nexum MLIR dialect

Status: not started

Blocked by: Task 0

- [ ] Dialect compiles and round-trips through MLIR parse and print
- [ ] Every op has a verifier, each with a rejection test
- [ ] Ops reflect actual Nexum semantics, not the current LLVM text
- [ ] Not a thin textual wrapper around LLVM
      Evidence: compiler/nx-mlir/

## H - MIR to Nexum MLIR lowering

Status: not started

- [ ] All 13 examples lower without loss
- [ ] MLIR verifier passes at every step
- [ ] Round-trip is structurally comparable
      Evidence: lowering tests

## I - Nexum MLIR to standard MLIR

Status: not started

- [ ] Every Nexum op lowered, or documented as intentionally surviving
- [ ] No Nexum op survives that a standard op expresses correctly
- [ ] Standard-dialect verification passes
      Evidence: lowering tests

## J - Standard MLIR to LLVM dialect

Status: not started

- [ ] Full path to LLVM dialect with no information loss
- [ ] LLVM dialect verifies
- [ ] The NxVal ABI boundary is explicit in IR, not implicit in convention
      Evidence: lowering tests

## K - LLVM IR and AOT object generation

Status: not started

- [ ] Object or executable generation with no textual .ll assembly step
- [ ] `nx emit-llvm` still works as a diagnostic export
- [ ] No dynamic native compilation subsystem anywhere in the tree
- [ ] Arm64 and RISC-V configured, or a documented blocker
      Evidence: build and object inspection tests

## L - Native execution and differential verification

Status: not started

- [ ] 13 examples identical across interpreter / legacy native / MLIR native / NX_NOUNBOX
- [ ] The differential is a CI gate, not a local script
- [ ] The interpreter is documented as the semantic oracle
      Evidence: tools/verify.ps1 extension, CI job

## M - Optimization migration

Status: not started

- [ ] Each optimization lives in exactly one layer, documented
- [ ] No LLVM optimization reimplemented
- [ ] Every migrated optimization benchmarked before and after
      Evidence: optimization ownership table

## N - Default backend migration

Status: not started

- [ ] `--backend=legacy|mlir` exists; both build the full corpus
- [ ] Default switched to the MLIR pipeline
- [ ] Differential green after the switch
- [ ] Benchmarks after the switch within stated noise of baseline
      Evidence: compiler/nx-driver/

## O - Legacy backend retirement

Status: not started

- [ ] Legacy backend deprecated with a stated removal condition
- [ ] Legacy backend removed
- [ ] Nothing references it (grep-clean)
- [ ] No functionality lost; `nx emit-llvm` still supported
- [ ] Whole corpus builds and passes on the surviving path
      Evidence: removal commit and CI

## P - Final verification, benchmarking, documentation, gate

Status: not started

- [ ] Every item above is [x] with evidence
- [ ] tools/argone-gate.ps1 exits 0
- [ ] Full test suite, differential, and benchmarks pass
- [ ] Before and after tables for every section 23 category
- [ ] Rollback path documented and tested
- [ ] Final report's 20-item checklist entirely checked
      Evidence: docs/architecture/argone-report.md

---

## Roadmap lock

While this file is not fully checked, Stage 4 through Stage 8 are LOCKED.

Resuming the roadmap is a deliberate, documented action that requires
updating this file and the changelog. It is not a default, and it is not
implied by a task or two being done.
