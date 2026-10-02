# ARGONE — task breakdown

Argone is the compiler-architecture modernization stage defined in
`Nexum_Argone_Unified_AOT_Prompt.md`. It is a **hard stage gate**: Stage 4
through Stage 8 do not resume until Argone is 100% complete.

This file is the executable breakdown. `docs/ARGONE-STATUS.md` is the
checklist that records completion, and `tools/argone-gate.ps1` is what
enforces it. Nothing here starts until Task 0 is resolved.

---

## The rule that shapes this plan

> **A stage is never partially implemented.**

Three consequences, applied literally:

1. **No "mostly done" items.** A task is either complete against its gate
   or it has not started. There is no in-progress state that lets the gate
   open. If MLIR turns out to be unobtainable, the honest outcome is a
   *documented renegotiation with the user*, not a quietly narrowed Argone
   that reports success for the parts that were easy.
2. **Prerequisites become part of the stage.** The prompt says so
   explicitly (§Argone Hard Stage Gate). Task 0 is a prerequisite that
   Argone discovered, so it is an Argone task with its own gate.
3. **The gate is machine-checked.** `tools/argone-gate.ps1` fails when any
   required item is incomplete. CI runs it. A human cannot wave it through
   by asserting completion in a commit message.

---

## Current state, measured

Grounded in the tree at `e3e4a10`, not estimated.

| Fact | Value |
| --- | --- |
| Rust source | 17,290 lines across 9 crates |
| Runtime (`runtime.ll`) | 2,956 lines |
| Tests | 318, all passing |
| External crate dependencies | **zero** — std-only |
| Examples | 13, with a 3-way differential (interpreter / native / `NX_NOUNBOX`) |
| Benchmarks | 5, each with a Rust reference, correctness-gated |
| Dependency DAG | acyclic: `lexer → ast → parser → {types, ir, mem} → {interp, codegen} → driver` |

The AST-walking duplication Argone exists to remove, measured:

| Crate | `Stmt` match arms | `Expr` match arms |
| --- | --- | --- |
| nx-types | 15 | 37 |
| nx-ir | 23 | 13 |
| nx-mem | 21 | 18 |
| nx-interp | 14 | 28 |
| nx-codegen | 27 | 37 |
| **total** | **100** | **133** |

233 arms re-deriving the same facts. That is the concrete target.

---

## TASK 0 — Toolchain feasibility (gate: blocks D onward)

**Must be resolved before any architecture is committed.** Once HIR exists
the design is committed to a lowering strategy; if MLIR is unobtainable at
that point, Argone cannot finish and work is stranded.

### What was measured on this host

| Check | Result |
| --- | --- |
| clang | 23.1.2, `C:\Program Files\LLVM` |
| MLIR tools (`mlir-opt`, `mlir-translate`, `mlir-tblgen`) | **absent** |
| `opt`, `llc`, `llvm-config` | **absent** |
| `LLVM-C.lib` (C API import library) | present, 300 KB |
| `crates.io` API (metadata) | reachable, HTTP 200 |
| `static.crates.io` (crate download CDN) | **unreachable** |
| Cached crates that bind LLVM/MLIR | **none** (11 crates cached, none relevant) |
| `cl.exe` / MSVC C++ toolchain | not on PATH, no install path resolved |
| Disk / cores / RAM | 760 GB free, 16 cores, 15.7 GB RAM |

The LLVM Windows installer ships clang, lldb, lld and the binutils
equivalents, but **not MLIR**. So `mlir-opt` is not merely missing from
`PATH`; it does not exist on this machine, and the official installer does
not provide it.

### Consequence

The MLIR stages (G, H, I, J, K) require *both* an MLIR toolchain *and* a
way to reach it from Rust. Both are currently unavailable, and the crate
CDN being blocked means `melior` / `inkwell` / `mlir-sys` cannot be
fetched.

This is not an argument against Argone. It is a prerequisite that must be
satisfied, and satisfying it is Task 0's job.

### Options, to be evaluated and decided in writing

| Option | Needs | Viability here |
| --- | --- | --- |
| Prebuilt LLVM+MLIR release (official, or from a distro) | network access to a release host | blocked — same CDN class as `static.crates.io` |
| Build LLVM+MLIR from source | LLVM source tree + MSVC C++ toolchain | source not present; no C++ toolchain resolved |
| Rust bindings (`melior` / `mlir-sys`) building MLIR internally | crate download + C++ toolchain + hours of build | crate CDN blocked |
| **Nix / vcpkg / conda / pip LLVM distribution** | network to that host | depends on host reachability — **must be tested** |
| Hand-rolled FFI over a vendored prebuilt MLIR DLL | an MLIR DLL + headers | no MLIR DLL present |

### Deliverable

`docs/architecture/toolchain.md` recording: the chosen route, exact
versions, where they came from, how CI reproduces them, and — for the
rejected options — the specific reason each was rejected.

### Gate

- [ ] At least one MLIR toolchain is invocable (`mlir-opt --version` succeeds)
- [ ] A Rust crate in the workspace can construct and lower MLIR
- [ ] The route is reproducible from a clean checkout, documented
- [ ] The decision is recorded, including rejected alternatives

**If no option is viable, Argone stops here and the scope is renegotiated
with the user before any HIR work begins.** That is a legitimate outcome
under the prompt's own rules. Proceeding to build HIR/MIR anyway and
reporting partial Argone is not.

---

## A — Full architecture audit

Covers prompt §33 items 1–24.

### Deliverable
`docs/architecture/audit.md`, assembled from material that partly exists
already (`docs/grammar.md`, `docs/numeric-audit.md`) and extended with:
module dependency graph, data flow, runtime ABI, unboxing mechanism,
memory model, effect/parallel model, test and benchmark coverage,
architectural weaknesses, migration risks, proposed HIR/MIR/dialect,
lowering pipeline, LLVM integration strategy, files that must change, files
that must not, and rollback strategy.

### Gate
- [ ] All 24 items of §33 present, none as a placeholder
- [ ] Every claim cites a file and function, as `docs/grammar.md` does
- [ ] Reviewed against the tree, not against memory

---

## B — Baseline tests and benchmarks

Prompt §23 and §Argone Benchmarking Gate.

### Current gap
Present: integer and float arithmetic, loops, calls, recursion, lists,
allocation (via `matmul`, `mandel`, `dot`, `listsum`, `fib`).
**Missing: string operations, dict/hash operations, sorting, I/O, startup
time, binary size, compile time, peak memory.**

### Deliverable
- New benchmarks for the missing categories, each with a Rust reference
- `bench/baseline.json` — machine-readable: compile time, binary size,
  runtime, peak memory, startup, per workload
- `tools/bench-baseline.ps1` to regenerate and diff it

### Gate
- [ ] Every category in §23 has a benchmark or a documented reason it does not apply
- [ ] Baseline JSON is committed and regenerable
- [ ] The harness measures compile time and binary size, not only runtime
- [ ] A recorded run on an idle machine; noise characterized, not assumed away

---

## C — Shared semantic structures

Prompt §Argone rule 7: semantic facts move into structures consumed by
passes, instead of being rediscovered by five AST walkers.

### Design
A new `nx-semantics` crate owning facts that are currently derived
independently per crate:

- resolved symbols and binding identity (not names — slots)
- resolved type of every binding and expression
- effect summaries per binding and per function
- allocation class per binding
- module/import resolution

### Deliverable
`nx-semantics`, consumed by nx-types, nx-ir, nx-mem, nx-interp,
nx-codegen.

### Gate
- [ ] AST match arms for *resolved* facts drop measurably from the 100/133 baseline, per crate, with the numbers recorded
- [ ] No crate re-derives a fact another crate already resolved
- [ ] Interpreter, native, and `NX_NOUNBOX` still agree on all 13 examples
- [ ] All 318 existing tests pass unchanged

Deliberately *not* in scope: rewriting the AST, or changing semantics. This
task moves facts; it does not change what they are.

---

## D — HIR

Prompt §7, §Argone HIR Requirements.

### Design
`nx-hir`: name-free, typed, resolved. Bindings are slots, not names. Every
node carries its resolved type. Effects and allocation class are attached,
not recomputed. Spans retained only where diagnostics need them.

Covers only constructs the language has today — functions, calls,
variables, constants, conditionals, loops, lists, indexing, returns,
modules, parallel blocks, records, methods. Ownership/move/borrow and
closures get principled extension points, not speculative structures
(prompt: "Do not implement speculative language features solely for Argone").

### Deliverable
`nx-hir` crate + `nx dump-hir`.

### Gate
- [ ] Every construct in the 13 examples lowers to HIR
- [ ] HIR is name-free: no source identifier survives into it except in diagnostics
- [ ] Every HIR node has a resolved type; a checker test proves no node is untyped
- [ ] A verifier rejects malformed HIR (dangling slot, type mismatch, unbound callee)
- [ ] `nx dump-hir` is stable enough to diff in tests
- [ ] The interpreter still runs from HIR or the AST, and 13/13 agree

---

## E — MIR / SSA

Prompt §8, §9, §Argone MIR Requirements.

### Design
`nx-mir`: functions, basic blocks, explicit terminators, SSA values,
explicit alloc/load/store, typed operations. Not name-based.

### Deliverable
`nx-mir` crate + `nx dump-mir`.

### Gate
- [ ] All 13 examples build MIR with no name-based reasoning anywhere in the builder
- [ ] Every value has exactly one defining instruction (verifier-enforced)
- [ ] Definitions dominate uses (verifier-enforced)
- [ ] Every block is terminated and reachable from entry (verifier-enforced)
- [ ] `nx dump-mir` round-trips stably
- [ ] Verifier has negative tests: every rule has a case that must be rejected

The verifier is part of E, not deferred — an SSA form without a dominance
checker is a shape, not a discipline.

---

## F — MIR verification and core analyses

Prompt §16 (MIR optimization layer), §Argone Unboxing Migration.

### Scope
- Value-based representation analysis: prove `v17 : i64`, `v18 : boxed`,
  `v19 : double` — replacing name-based unboxing reasoning
- SCCP / constant propagation
- DCE
- CSE
- Copy propagation
- CFG simplification
- Boxed-boundary elimination: a value must stay unboxed across its region
  without box→unbox→box churn

### Gate
- [ ] Each pass has a correctness test proving input and output are semantically equivalent
- [ ] Each pass is measured on the §23 benchmark set, before and after
- [ ] Representation analysis is value-keyed, and a test proves a value can stay unboxed across a region
- [ ] All passes together leave the 13 examples agreeing on all three paths
- [ ] No pass is claimed to improve anything without a benchmark number

---

## G — Nexum MLIR dialect

**Blocked on Task 0.** Prompt §10, §Argone MLIR Requirements.

Real dialect ops for real Nexum semantics (`nx.function`, `nx.call`,
`nx.box`, `nx.unbox`, `nx.list_push`, `nx.parallel`, …), with verifiers,
types and invariants. Standard MLIR ops wherever they already say the right
thing — the prompt forbids inventing a custom op for a concept a standard op
expresses.

### Gate
- [ ] Dialect compiles and round-trips through MLIR's parser and printer
- [ ] Every op has a verifier, and each verifier has a rejection test
- [ ] Ops correspond to actual Nexum semantics, not to the current LLVM text
- [ ] The dialect is not a thin textual wrapper around LLVM

---

## H — MIR → Nexum MLIR lowering

### Gate
- [ ] All 13 examples lower without loss
- [ ] Lowering is verified by MLIR's own verifier at every step
- [ ] Round-trip: MIR → MLIR → dumped, compared structurally

---

## I — Nexum MLIR → standard MLIR

### Gate
- [ ] Every Nexum-specific op has a documented lowering to standard dialects, or a documented reason it survives
- [ ] No Nexum op survives that a standard op expresses correctly
- [ ] Standard-dialect verification passes

---

## J — Standard MLIR → LLVM dialect

### Gate
- [ ] Full path to LLVM dialect with no information loss
- [ ] LLVM dialect verifies
- [ ] The `NxVal` ABI boundary is explicit in the IR, not implicit in a convention

---

## K — LLVM IR and AOT object generation

Prompt §15, §Argone AOT Lowering Pipeline. AOT only — no JIT, no runtime
compilation, no second native path.

### Gate
- [ ] Object/executable generation works with no textual `.ll` assembly step
- [ ] `nx emit-llvm` still works, as a diagnostic export
- [ ] Nothing in the tree constitutes a dynamic native compilation subsystem
- [ ] Arm64 and RISC-V targets are configured, or a documented blocker is recorded

---

## L — Native execution and differential verification

Prompt §21, §Argone Testing Gate.

### Deliverable
Extend `tools/verify.ps1` to run the corpus through the MLIR pipeline as a
fourth path, against the interpreter and the legacy backend.

### Gate
- [ ] All 13 examples produce identical output across interpreter / legacy native / MLIR native / `NX_NOUNBOX`
- [ ] The differential is a CI gate, not a local script
- [ ] The interpreter is documented as the semantic oracle

---

## M — Optimization migration

Prompt §16. Layered: Nexum-level → MIR → MLIR → LLVM. Do not duplicate
what LLVM already does well.

### Gate
- [ ] Each optimization lives in exactly one layer, documented
- [ ] No LLVM optimization is reimplemented
- [ ] Every migrated optimization is benchmarked, with before and after numbers

---

## N — Default backend migration

Prompt §30. `--backend=legacy|mlir`, then default flips.

### Gate
- [ ] `--backend` flag exists and both backends build the full corpus
- [ ] Default is switched to the MLIR pipeline
- [ ] Differential still green after the switch
- [ ] Benchmarks after the switch are not worse than baseline beyond stated noise

---

## O — Legacy backend retirement

Prompt §Argone Legacy Backend Migration. Deprecated, then removed. "Do not
leave two permanent compiler architectures."

### Gate
- [ ] Legacy backend marked deprecated with a stated removal condition
- [ ] Removed
- [ ] Nothing references it: grep-clean
- [ ] No functionality was lost with it; `nx emit-llvm` still supported
- [ ] Whole corpus still builds and passes on the surviving path

---

## P — Final verification, benchmarking, documentation, gate

Prompt §36 completion criteria, §35 final report.

### Deliverable
`docs/architecture/argone-report.md` with all 20 required items.

### Gate
- [ ] Every box in `docs/ARGONE-STATUS.md` is `[x]` with evidence
- [ ] `tools/argone-gate.ps1` exits 0
- [ ] The full test suite, differential, and benchmark set pass
- [ ] Before/after benchmark tables exist for every §23 category
- [ ] Rollback path documented and *tested*
- [ ] The final report's completion checklist is entirely checked

---

## Ordering and parallelism

```
Task 0 (toolchain) ── gate ──> D (HIR) ──> E (MIR) ──> F (analyses)
     │                            ^
     └──> A, B, C run in parallel with Task 0
                                  |
                                  v
                     G ──> H ──> I ──> J ──> K ──> L ──> M ──> N ──> O ──> P
```

- **A, B, C are independent of Task 0** and of each other. They can be
  worked in any order, in parallel.
- **D onward is strictly sequential** and gated on Task 0.
- A, B, C deliver real value on their own. That is *not* a reason to start
  them and then stop: they are prerequisites for the gate, and the gate
  does not open until P.

---

## What "never partially implemented" means concretely

`tools/argone-gate.ps1` checks, and CI runs:

1. Every task T0, A–P has a status of either `complete` or `not started`.
   No third state.
2. `complete` requires its gate items to be `[x]` **and** an evidence line.
3. Argone is complete only when **all 17 tasks** are complete.
4. If any task is incomplete, the script exits non-zero and names it.
5. The Stage 4–8 roadmap is blocked while the script fails. Resuming it is
   a deliberate, documented action, not a default.

The script cannot be satisfied by scaffolding, a placeholder IR, a demo
program that reaches MLIR, or a compiler that merely builds — the prompt
names all four as invalid, and each is a specific check in the script.

---

## Risks

| Risk | Mitigation |
| --- | --- |
| MLIR unobtainable (Task 0 fails) | Stop and renegotiate scope before HIR is built. This is the single largest risk and it is resolved first, on purpose. |
| MLIR obtained but a Rust binding needs a C++ toolchain we lack | Prefer prebuilt MLIR + a thin C ABI shim over a source build. §26 forbids rewriting the compiler in C++ but permits a bridge. |
| HIR/MIR become a second AST with extra steps | Task C's measurable arm-count reduction is the check. If arms do not fall, the abstraction is not earning its place. |
| Migration drags correctness down | The interpreter oracle and the 3-way differential run at every task, not only at the end. |
| Optimizations regress performance | §16 forbids unmeasured claims; Task B exists so "before" is a number. |
| Scope creep into speculative language features | Prompt forbids it; the HIR scope list is closed to what the language has today. |

---

## Rollback

Every task is a single commit range with a stated revert point. The legacy
textual backend is retained in full through Task N, so the worst case at any
point is `git revert` to the last green commit and `--backend=legacy`.

The one task that is not cleanly revertible is O, legacy removal. It is
therefore last, it is gated on the MLIR path being the proven default, and
the removal commit is preceded by a deprecation commit giving a stated
condition.
