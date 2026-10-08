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

Status: complete

- [x] Each of the 11 rules has exactly one written owner, named in docs/grammar.md
- [x] docs/grammar.md no longer names a backend as the authority for runtime meaning
- [x] All 11 have one written owner, and a test that fails if a second implementation re-derives them
- [x] The runtime has no known memory-safety defect; ASan must cover hand-written runtime IR, not just Rust code
      Evidence: CI run 37785049102, job `native (ubuntu-latest)` at commit 152c9b2, step `memcheck every example (linux only)` -- `valgrind --tool=memcheck --error-exitcode=42` over all 14 examples with `NX_CFLAGS="-O1 -g"`, printing `memcheck clean` for each: boundaries, hello, flex, control, funcs, lists, search, records, methods, syntax, modules/main, fib, unbox, comments. Valgrind Memcheck instruments the generated machine code, so unlike the ASan step it does see inside the hand-written runtime IR, which is what this item asks for; the ASan step's own comment (.github/workflows/ci.yml, `asan check (linux, leaks off by design)`) records why it cannot, and its scope is unchanged. Leaks stay out of scope by `--leak-check=no`.
- [x] No native test asserts a known-bad behaviour
- [x] examples/boundaries.nx covers them as a permanent differential fixture
- [x] verify.ps1 builds from the tree, ordered byte diff, no line-dropping, exit codes checked
- [x] verify.ps1 includes examples/modules/
- [x] CI natively compiles all 14 examples, including records, methods, syntax
- [x] Short-circuit and/or is tested on both the short-circuiting and non-short-circuiting branches
      Evidence: docs/grammar.md 3.1.1 (R1-R6), 1.3 (MIN spelling), 2.5 (compound order), 2.6 (deletion errors); authority table names the document, runtime.ll is an implementation (grammar.md:9-24); compiler/nx-e2e expected-value tests plus examples/boundaries.nx (single execution model, so expected values are the anti-rederivation guard); stale-behavior notes replaced with values (containers_strings.rs stepped-slice test); tools/verify.ps1 builds nx from the tree, compares byte-exact with exit codes, corpus lists modules/main.nx; .github/workflows/ci.yml EXAMPLES (14, incl. boundaries.nx); and_or_short_circuit_on_both_branches in language_core.rs. Memory-safety item still open: ASan proven incapable of covering .ll inputs (CHANGELOG), Valgrind memcheck step added but awaiting its first green Linux CI run.

## A - Full architecture audit

Status: complete

- [x] Covers structure, duplication, semantics ownership, toolchain, harness
- [x] Every claim cites file:symbol or records the command that produced it
- [x] Corrections to prior documentation recorded explicitly
      Evidence: docs/architecture/audit.md sections 1-8 plus section 9 (corrections since the investigation: e2e/clang counts, nine-crate roster, nx_ir::expr hole closure, arm-count update, post-stage divergence fixes), each with file:line or the enumerating command.

## B - Baselines

Status: not started

- [ ] Every benchmark category covered, or explicitly not applicable with a reason
- [ ] clang-vs-Nexum codegen split measured, not estimated
- [ ] bench/baseline.json committed and regenerable from one command
- [ ] Noise characterised on an idle machine
      Evidence:

## C - Shared semantic structures

Status: complete

- [x] Partial re-derivation arms measurably reduced from the 159 measured with the interpreter present
- [x] Dead Checker::writable_receiver deleted, not wired up
- [x] NX_NOUNBOX still produces identical output on all 14 examples
- [x] Module path resolution is one function; intra-crate duplicate removed
- [x] infer_program "__main__" hardcoding fixed, with a fixture having a method call in an imported module
      Evidence: nx-ast::shape owns resolve_module_file (5 call sites), child_bodies, assigned_names (3 sites), imported_modules (3 loaders, now recursive), method_key/split_method_key (5 build sites, 1 parse), BUILTINS table (nx-types delegates, nx-mem uses the gate); hand-rolled control-flow recursion 29 to 3 measured by enumerating elif/else/While/For arms across nx-ir/nx-mem/nx-codegen/nx-types (remaining 3 need conditions and stay); writable_receiver deleted (nx-types); verify.ps1 14/14 incl. NX_NOUNBOX; infer_program_for with a_method_call_inside_an_imported_module_resolves e2e fixture (methods.rs) plus infer_program_for_keys_by_module unit test. Read-name sets and receiver predicates deliberately not shared (different soundness directions; documented in shape.rs).

## D - HIR

Status: complete

- [x] All 13 examples lower; nx dump-hir stable enough to diff in tests
- [x] Name-free: no source identifier survives outside diagnostics
- [x] Every node typed; a test proves none is untyped
- [x] Verifier rejects malformed HIR, one negative test per rule
- [x] At least integer overflow now lives in HIR, proven by a test that fails if the backend recomputes it
      Evidence: new crate compiler/nx-hir (model/lower/verify/dump): slots params-first then first-bind order with self as slot 0, IDs in sorted-name order, ty: HTy mandatory on every expression, decided rules on every operator node (Trap/Saturate/Concat/Member/Index/Slice/Iter/Copy/Del) derived by calling nx-types::arith_result; String only in Str literals, HProgram::strings, DiagInfo (grep plus strip test); lowering verifies its own output before returning; nx dump-hir in nx-driver with snapshot compiler/nx-hir/tests/snapshots/control.hir.txt. Tests: 63 in nx-hir (29 lowering incl ToInt/ToFloat nodes, 16 verifier negatives V1-V12, 7 arith_plan table, corpus-wide lower of all 42 shipped programs, dump-twice-identical plus snapshot); workspace suite green, verify.ps1 14/14, gate self-tests 10/10. Fifth box: arith_plan in nx-hir (rule plus operator in, emission out, no operand types in the signature, so recomputation inside the table is unrepresentable), emit_arith in nx-codegen (the only consulted table on the scalar path; the remaining type-to-rule derivation is one marked site for task G), and trap_rule_drives_checked_emission in nx-codegen (lowered Trap counts versus emitted checked intrinsics per operator, plus a float control; verified red under a sabotaged plan).

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