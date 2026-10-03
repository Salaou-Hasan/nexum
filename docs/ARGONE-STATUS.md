# ARGONE stage status

The completion checklist for the Argone stage. This file **is** the gate.
`tools/argone-gate.ps1` reads it; `tools/argone-gate-tests.ps1` tries to
fool it. CI runs the self-tests.

ASCII-only on purpose: this file is parsed by a script and printed into a
console table.

## How to read this file

Each task is `not started` or `complete`. There is no third state, and that
is the point. The gate rejects `in progress`, `partial`, `mostly`,
`pending` and `blocked` as statuses, because a stage that admits a third
state is a stage that gets left halfway.

`complete` requires every gate item checked **and** an evidence line naming
a commit, file or measurement. A bare `[x]` with no evidence is incomplete,
which is what makes scaffolding insufficient.

`Blocked by:` is informational and does not affect the gate.

MLIR is not a prerequisite and is not a completion criterion anywhere in
this file. Task definitions: `docs/ARGONE.md`.

---

## Task 0 - Semantic correctness

Status: not started

- [ ] Each of the 11 rules has exactly one written owner, named in docs/grammar.md
- [ ] docs/grammar.md no longer names a backend as the authority for runtime meaning
- [ ] All 11 have one written owner, and a test that fails if a second implementation re-derives them
- [ ] The runtime has no known memory-safety defect; ASan must cover hand-written runtime IR, not just Rust code
- [ ] No native test asserts a known-bad behaviour
- [ ] examples/boundaries.nx covers them as a permanent differential fixture
- [ ] verify.ps1 builds from the tree, ordered byte diff, no line-dropping, exit codes checked
- [ ] verify.ps1 includes examples/modules/
- [ ] CI natively compiles all 13 examples, including records, methods, syntax
- [ ] Short-circuit and/or is tested on both the short-circuiting and non-short-circuiting branches
      Evidence:

## A - Full architecture audit

Status: not started

- [ ] Covers structure, duplication, semantics ownership, toolchain, harness
- [ ] Every claim cites file:symbol or records the command that produced it
- [ ] Corrections to prior documentation recorded explicitly
      Evidence:

## B - Baselines

Status: not started

- [ ] Every benchmark category covered, or explicitly not applicable with a reason
- [ ] clang-vs-Nexum codegen split measured, not estimated
- [ ] bench/baseline.json committed and regenerable from one command
- [ ] Noise characterised on an idle machine
      Evidence:

## C - Shared semantic structures

Status: not started

- [ ] Partial re-derivation arms measurably reduced from the 159 measured with the interpreter present
- [ ] Dead Checker::writable_receiver deleted, not wired up
- [ ] NX_NOUNBOX still produces identical output on all 13 examples
- [ ] Module path resolution is one function; intra-crate duplicate removed
- [ ] infer_program "__main__" hardcoding fixed, with a fixture having a method call in an imported module
      Evidence:

## D - HIR

Status: not started

- [ ] All 13 examples lower; nx dump-hir stable enough to diff in tests
- [ ] Name-free: no source identifier survives outside diagnostics
- [ ] Every node typed; a test proves none is untyped
- [ ] Verifier rejects malformed HIR, one negative test per rule
- [ ] At least integer overflow now lives in HIR, proven by a test that fails if the backend recomputes it
      Evidence:

## E - MIR / SSA

Status: not started

- [ ] All 13 examples build MIR; nx dump-mir round-trips stably
- [ ] Grep gate proves no name-based reasoning in the builder
- [ ] Single assignment, dominance, termination, reachability each have an
      accepting and a rejecting test
- [ ] Dominance uses real iterative dataflow, not linear order
      Evidence:

## F - Verification and analyses

Status: not started

- [ ] Representation analysis is value-keyed, proven by a test that a value
      stays unboxed across a region
- [ ] SCCP, constant propagation, DCE, CSE, copy propagation, CFG simplify present
- [ ] Each pass has an equivalence test and a before/after benchmark
- [ ] Analysis invalidation rules written down, not implied
- [ ] 13 examples still produce identical output with and without NX_NOUNBOX after every pass
      Evidence:

## G - LLVM integration boundary

Status: not started

- [ ] Backend trait defined; legacy backend implements it or is adapted
- [ ] Backend cannot reach the AST
- [ ] --backend=legacy|new exists and both build the full corpus
- [ ] --emit-llvm still works as a diagnostic export
      Evidence:

## H - Structured LLVM generation, feasibility

Status: not started

- [ ] A Rust crate builds a module and function via LLVM-C.dll with zero new dependencies
- [ ] It emits IR text for inspection and an object file for linking
- [ ] Output matches the legacy text path for one example
- [ ] A second example exercising records and methods: also matches
- [ ] LLVM version and version-compatibility policy recorded
      Evidence:

## I - Structured LLVM generation, full

Status: not started

- [ ] All 13 examples compile and run through the structured path
- [ ] Every Task 0 rule enforced by construction, not convention
- [ ] No textual .ll assembly step in the production path
- [ ] No JIT, ORC or LLJIT anywhere; the C API does object generation only
      Evidence:

## J - Execution and differential verification

Status: not started

- [ ] 13 examples identical across legacy native / new native / NX_NOUNBOX
- [ ] The differential is a CI gate, not a local script
- [ ] verify.ps1 cannot pass against a stale binary
- [ ] A self-test proves the differential can go red, so it cannot rot always-green
      Evidence:

## K - Nexum-level optimization

Status: not started

- [ ] Ownership table: which layer owns which optimization
- [ ] No reimplementation of an LLVM pass
- [ ] Every migrated optimization benchmarked, before and after
- [ ] Trade-offs documented, including compile-time cost
      Evidence:

## L - Default backend switch

Status: not started

- [ ] Default is the new pipeline; legacy reachable via flag
- [ ] Differential green after the switch
- [ ] Benchmarks within stated noise of baseline
- [ ] The Task 0 divergences re-checked on the new default
      Evidence:

## M - Legacy backend retirement

Status: not started

- [ ] Deprecated with a stated removal condition
- [ ] Removed; grep-clean
- [ ] Nothing lost; --emit-llvm still supported
- [ ] Whole corpus builds and passes on the surviving path
      Evidence:

## N - Final gate

Status: not started

- [ ] Every item above is [x] with evidence
- [ ] tools/argone-gate.ps1 exits 0
- [ ] Full suite, differential and benchmarks pass
- [ ] Report distinguishes complete / unvalidated / blocked / deferred
      Evidence:

---

## Roadmap lock

While this file is not fully checked, the language roadmap stays LOCKED.
Resuming it is a deliberate, documented action, not a default.