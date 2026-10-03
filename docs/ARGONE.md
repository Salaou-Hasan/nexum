# ARGONE — task breakdown

Argone moves the compiler to Nexum-owned HIR and MIR/SSA, with strong IR
verification, reusable analyses, Nexum-level optimization, and a structured
direct LLVM AOT backend.

**MLIR is not a prerequisite, not a dependency, and not a completion
criterion.** It may be added later if a concrete requirement justifies it.
JIT, ORC and LLJIT are out of scope permanently. The backend boundary is a
trait so a second backend stays possible without redesigning HIR or MIR.

`docs/ARGONE-STATUS.md` is the checklist and *is* the gate.
`tools/argone-gate.ps1` decides. `tools/argone-gate-tests.ps1` tries to
fool it.

**Why this is ordered the way it is:** the audit found eleven reachable
programs where the interpreter and the native backend disagree, and found
that `docs/grammar.md:12` names `runtime.ll` as the authority for runtime
meaning. The specification points at a backend. That is why the eleven
exist. Task 0 fixes the symptom; every later task exists so it cannot recur.

---

## The rule

A stage is never partially implemented. A task is `not started` or
`complete`, with no third state, and `complete` requires every gate item
checked **and** an evidence line. `tools/argone-gate.ps1` enforces this and
has ten adversarial tests proving it rejects scaffolding, a partial status,
unchecked boxes, and a build that merely compiles.

---

## Baseline, measured

9 crates, 17,290 lines, **zero external dependencies**, 318 `#[test]`
attributes over 317 functions, **zero integration tests**, **no test
invokes clang**, 13 examples with a two-way differential, 5
Rust-referenced benchmarks. Full detail in `docs/architecture/audit.md`.

Duplication, measured (my earlier 100/133 figure was wrong):

| | Stmt arms | Expr arms |
| --- | --- | --- |
| total across 5 walkers | 146 | 118 |
| of which exhaustive language interpreters | was 105 (3x18, 3x17); now 70 | — |
| of which partial re-derivations | 159 combined | |

The remaining exhaustive walkers are compile-time-enforced sync and stay. The partial re-derivations are the target.

---

## TASK 0 — Semantic correctness (blocks D onward)

The audit ran eleven programs whose semantics are wrong. All are reachable
from valid source. Fix them, and make each impossible to reintroduce
unnoticed.

1. integer `+ - * /` overflow — the native path wraps silently, with no diagnostic. `@.msg.overflow` is declared in `runtime.ll` and never raised.
2. integer `**` saturation and negative exponent
3. `"" in s` and `s in s`
4. non-ASCII `len`, index, slice, iteration — characters vs bytes
5. float printing at `|x| >= 1e15` and `< 1e-12`
6. `del d[missing_key]` silently no-ops
7. `del name` unbinds vs stores `none`
8. compound-assignment evaluation order is reversed
9. ~~`LOOP_LIMIT` / `CALL_LIMIT`~~ — resolved: the interpreter is gone, and neither limit was ever a language semantic
10. ~~`parallel:` task output order~~ - resolved by removal: the feature needed a static proof of race-freedom that had holes, so it went rather than the claim
11. `-9223372036854775808` is a parse error; `i64::MIN` is unrepresentable

### Gate

- [ ] Each rule has exactly one written owner, named in `docs/grammar.md`
- [ ] `grammar.md` no longer names a backend as the authority for meaning
- [ ] Every one of the eleven has one written owner, and a test that fails if a second implementation re-derives it
- [ ] `examples/boundaries.nx` covers them permanently as a differential fixture
- [ ] `tools/verify.ps1` is trustworthy: builds from the tree, ordered byte
      diff, no line-dropping, exit codes checked, `examples/modules/` included
- [ ] CI compiles all 13 examples natively, including records, methods, syntax
- [ ] Short-circuit `and`/`or` has a test on both sides (currently zero coverage)

**Why first.** Building HIR/MIR on top of a backend that silently wraps
integers produces a silent wrong answer. The specification is not yet an
oracle for the runtime.

---

## A — Full architecture audit

**Done.** `docs/architecture/audit.md`: 11 verified divergences, crate
structure, corrected duplication measurement, LLVM integration decision with
evidence, harness defect list, risks.

### Gate
- [x] Covers structure, duplication, semantics ownership, toolchain, harness
- [x] Every claim cites file:symbol or records the command that produced it
- [x] Corrections to prior documentation recorded explicitly

---

## B — Baselines

Measured before any architecture change, and separating compiler cost from
generated-program cost.

- Compiler: `check` time, full build time, split into Nexum codegen vs clang
  vs link; peak working set; output size; scaling on synthetic programs.
- Generated programs: the existing 5 benchmarks plus coverage for string
  operations, dict operations, control flow, allocation churn, and one
  application-shaped program.
- `bench/baseline.json`, regenerable, with machine, toolchain, build mode,
  warm-up policy, sample count and variability recorded.

### Gate
- [ ] Every category covered, or explicitly not applicable with a reason
- [ ] clang-vs-Nexum codegen split measured, not estimated
- [ ] `baseline.json` committed and regenerable from one command
- [ ] Noise characterised on an idle machine

---

## C — Shared semantic structures

Target: the 159 partial re-derivations. Placement is a `shape` module inside
`nx-ast`, not a new crate — `nx-ast` is already the universal ancestor, so
this adds zero dependency edges and cannot invert the DAG.

First eight, in payoff order: receiver-has-storage (kills a byte-identical
pair and a dead third copy), `method_key` / `split_method_key`, builtin
set and arity, module path resolution, `substatements`, assigned-name set
with a `recurse_into_for_body` flag, read-name set with a comprehension
shadow set, import list.

Two deliberate non-actions, both verified:

- **Do not centralise the three exhaustive walkers.** `check_stmt`,
  `exec_stmt`, `emit_stmt` are exhaustive with no wildcard, so a new `Stmt`
  variant is a compile error in three crates at once, naming every site.
  Centralising trades a free build-time invariant for a runtime table and
  leaves the 90% of each arm that matters still per-crate.
- **Do not centralise the dispatch *predicate*.** `nx-types` asks the static
  type, `nx-codegen` the representation type —
  which is `Unknown` whenever `NX_NOUNBOX` is set. Sharing the predicate
  would make `NX_NOUNBOX` a different language. Share the resolution *order*
  and the *messages*; share nothing else.

### Gate
- [ ] Partial re-derivation arms measurably reduced from 159
- [ ] The dead `Checker::writable_receiver` is deleted, not wired up
- [ ] `NX_NOUNBOX` still produces identical output on all 13 examples
- [ ] Module path resolution is one function; the intra-crate duplicate goes
- [ ] The `infer_program` `"__main__"` hardcoding is fixed with a fixture that
      has a method call inside an imported module

---

## D — HIR

Nexum-owned, name-free, typed, resolved. Bindings are slots. Every node
carries its resolved type and the *semantic rule it decided* — operator
matrix, string element kind, equality relation, copy discipline — so the
backend never re-derives them.

Scoped to constructs the language has today. Ownership, closures and `dyn`
get extension points, not speculative structures.

### Gate
- [ ] All 13 examples lower; `nx dump-hir` stable enough to diff in tests
- [ ] Name-free: no source identifier survives outside diagnostics
- [ ] Every node typed; a test proves none is untyped
- [ ] Verifier rejects malformed HIR, one negative test per rule
- [ ] A rule the backend used to decide for itself now lives in HIR — at
      least integer overflow, proven by a test that would fail if the
      backend recomputed it

---

## E — MIR / SSA

Basic blocks, explicit terminators, SSA values, explicit alloc/load/store.
No name-based reasoning anywhere in the builder.

The verifier ships with E and has a negative test per rule: single
assignment, dominance, termination, reachability. An SSA form without a
dominance checker is a shape, not a discipline. Use real iterative dataflow
dominance, not linear order — the short-circuit `and`/`or` diamond already
emits a `phi i1` with two sibling-block predecessors, which is exactly what
a naive check rejects.

### Gate
- [ ] All 13 examples build MIR; `nx dump-mir` round-trips stably
- [ ] Grep gate proves no name-based reasoning in the builder
- [ ] Every invariant has an accepting and a rejecting test
- [ ] Reachable-block reads rejected, not repaired

---

## F — Verification and analyses

Value-keyed representation analysis (replacing name-keyed unboxing
reasoning), SCCP, constant propagation, DCE, CSE, copy propagation, CFG
simplification, boxed-boundary elimination.

Analysis invalidation is explicit: what a pass may reuse, and what it must
recompute after a transformation that could change it.

### Gate
- [ ] Representation analysis is value-keyed, with a test proving a value
      stays unboxed across a region
- [ ] Each pass has an equivalence test and a before/after benchmark
- [ ] Invalidation rules written down, not implied
- [ ] 13 examples still produce identical output with and without `NX_NOUNBOX` after every pass

---

## G — LLVM integration boundary

Replace the direct-to-text backend with a trait the new path implements,
while the legacy text path keeps working.

Backend trait consumes the designated Nexum IR, never the AST, and never
re-derives a semantic fact. Decide and document the split between HIR-level
semantics and instruction selection.

### Gate
- [ ] Backend trait defined; legacy backend implements it or is adapted
- [ ] Backend cannot reach the AST
- [ ] `--backend=legacy|new` exists and both build the full corpus
- [ ] `--emit-llvm` still works as a diagnostic export

---

## H — Structured LLVM generation, feasibility

Prove the LLVM C API route before building on it. Smallest experiment first.

### Gate
- [ ] A Rust crate constructs a module and function via `LLVM-C.dll` with
      zero new dependencies
- [ ] It emits IR text for inspection and an object file for linking
- [ ] Output matches what the legacy text path produces for one example
- [ ] A second example, exercising records and methods, also matches
- [ ] Recorded LLVM version, and the version-compatibility policy

If this fails, the documented outcome is a fallback to the text path with an
explicit contract and a migration plan — not a silent substitution.

---

## I — Structured LLVM generation, full

The designated IR consumed through the structured API. Integer widths and
signedness, floats, conversions, control flow, calls, locals, parameters,
aggregates, runtime calls, external symbols.

Document and test: overflow behaviour, signed vs unsigned, truncation,
float behaviour, UB constraints, pointer representation, calling convention,
linker requirements. **Never emit an operation whose assumptions contradict
Nexum's semantics** — the eleven divergences are exactly that failure.

### Gate
- [ ] All 13 examples compile and run through the structured path
- [ ] Every rule from Task 0 is enforced by construction, not by convention
- [ ] No textual `.ll` assembly step in the production path
- [ ] No JIT, ORC or LLJIT anywhere; `nx-C API` does object generation and
      nothing dynamic

---

## J — Execution and differential verification

There is no second execution path to differential against any more. Correctness rests on the expected-value tests in `compiler/nx-e2e`, so Task 0 is what makes them trustworthy.

### Gate
- [ ] 13 examples identical across legacy native / new native / `NX_NOUNBOX`
- [ ] The differential is a CI gate, not a local script
- [ ] `verify.ps1` builds from the tree and cannot pass against a stale binary
- [ ] A self-test proves the differential can go red, so it cannot rot to always-green

---

## K — Nexum-level optimization

Exploits Nexum's own semantic knowledge; backend-oriented work stays with
LLVM. No LLVM optimization reimplemented. Each optimization lives in exactly
one layer, documented.

### Gate
- [ ] Ownership table: which layer owns which optimization
- [ ] No reimplementation of an LLVM pass
- [ ] Every migrated optimization benchmarked, before and after
- [ ] Trade-offs documented, including optimisation cost at compile time

---

## L — Default backend switch

### Gate
- [ ] Default is the new pipeline; legacy reachable via flag
- [ ] Differential green after the switch
- [ ] Benchmarks within stated noise of baseline
- [ ] The three Task 0 divergences re-checked on the new default

---

## M — Legacy backend retirement

Last, gated on the new path being the proven default, preceded by a
deprecation commit with a stated removal condition. This is the only task
that is not cleanly revertible.

### Gate
- [ ] Deprecated with a stated removal condition
- [ ] Removed; grep-clean
- [ ] Nothing lost; `--emit-llvm` still supported
- [ ] Whole corpus builds and passes on the surviving path

---

## N — Final gate

### Gate
- [ ] Every item above `[x]` with evidence
- [ ] `tools/argone-gate.ps1` exits 0
- [ ] Full suite, differential and benchmarks pass
- [ ] Report distinguishes complete / unvalidated / blocked / deferred

---

## Order

```
Task 0 (semantic correctness) ──> A ──> B ──> C ──> D ──> E ──> F
                                                              │
                              G ──> H ──> I ──> J ──> K ──> L ──> M ──> N
```

Task 0 gates D onward: building a typed HIR while the backend still wraps
integers produces two wrong implementations. A, B and C are independent of
the backend and run in parallel with D's design.

## Rollback

Every task is a commit range with a revert point. The legacy backend survives
until M, so the worst case at any point is `git revert` to the last green
commit and `--backend=legacy`.