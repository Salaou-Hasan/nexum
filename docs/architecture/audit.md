# ARGONE consolidated architecture report

> **Status: a point-in-time investigation.** Everything below was true when it
> was written, against a tree that still had a tree-walking interpreter. That
> interpreter has since been deleted, so several findings below can no longer
> be reproduced against it and some have been fixed outright. What changed:
>
> - the interpreter is gone; `nx` compiles ahead-of-time and that is the only
>   execution model
> - `parallel:` was removed rather than repaired -- it needed a static proof of
>   race-freedom and the proof had holes
> - all nine correctness bugs this report led to are fixed, plus the eleven
>   divergences below
> - strings are addressed by character, not byte
> - integer overflow traps rather than wrapping
>
> The findings are kept because they are why those changes were made. Where a
> finding is now historical this header says so; the body is otherwise as it
> was written.


Stage 3 synthesis of the 10-agent parallel investigation, reconciled against
the tree by the lead architect.

Everything marked **VERIFIED** was executed on this host and the output
recorded. Everything marked **code-proven** was established by reading both
implementations and the two provably compute different results.
Everything marked **unverified** is explicitly not confirmed.

---

## 1. The finding that reframes the stage

**Eleven reachable programs behave differently on the interpreter and the
native backend.** All eleven are VERIFIED by execution.

| # | Program | Interpreter | Native | Root cause |
| --- | --- | --- | --- | --- |
| 1 | `9223372036854775807 + 1` | error `integer overflow` | `-9223372036854775808` | `add i64` wraps, `runtime.ll:613` |
| 2 | `9223372036854775807 * 2` | error `integer overflow` | `-2` | `mul i64` wraps, `runtime.ll:686` |
| 3 | `2 ** 100` | `9223372036854775807` (saturates) | `0` | `nx_ipow` unguarded, `runtime.ll:1595` |
| 4 | `2 ** -1` | error, names the fix | `1` | `nx_ipow` has no negative-exponent check |
| 5 | `"" in "hello"` | `true` | `false` | `nx_strcontains:1848` rejects empty needle |
| 6 | `"hello" in "hello"` | `true` | `false` | `icmp sgt` requires strictly longer, `:1852` |
| 7 | `len("héllo")` | `5` | `6` | chars vs bytes, `runtime.ll:1077` |
| 8 | `"héllo"[1]` | `é` | replacement char | byte indexing, `runtime.ll:1133` |
| 9 | `"héllo"[0:2]` | `hé` | `h` + invalid byte | byte slicing, `runtime.ll:2299` |
| 10 | `1.0e15` | `1000000000000000.0` | `1000000000000000` | `%.17g` vs Rust shortest round-trip |
| 11 | `del d["z"]` (missing) | error `key z not found` | silently no-ops | `nx_dictdel:2197` returns unchanged |

Plus, code-proven but not executed here:

- `LOOP_LIMIT` (10M) and `CALL_LIMIT` (500) existed only in the interpreter. The team has since ruled that neither was ever a language semantic, and both died with the crate (grammar.md R4).
  `parallel:` task output order is spawn order on the interpreter and
  cursor-claimed on native.
- Compound assignment evaluates the RHS before the target on native
  (`codegen:2489`) and after on the interpreter (`interp:271`).
- `del name` unbinds on the interpreter and stores `none` on native.

**Why the 3-way differential is green.** No file in `examples/` or `bench/`
reaches these cases. `bench/listsum.nx` peaks around 4e9, `bench/fib.nx`
stops at 34, `examples/syntax.nx` tops out at `2**10`. The harness is
sound; the corpus is empty at exactly the boundary where the compiler is
wrong.

**Root cause, and it is architectural.** `docs/grammar.md:12` lists, as the
authority for "Runtime meaning", `compiler/nx-codegen/src/runtime.ll`. The
language specification points at the native backend. The interpreter is a
cross-check on a backend, not the other way round. Every one of the eleven
divergences exists because a rule was written once in Rust and once in LLVM
IR with no owner, and the two drifted.

This is the concrete justification for the whole stage: **semantics need an
owner.** A backend that re-derives what `+` means cannot be trusted to agree
with anything.

### 1a. A bug the differential is structurally blind to

Sub-agent reports proposed that `nx_ir::expr` has a `_ => {}` arm that
silently drops `Dict`, `Slice`, `IfExpr` and `Comprehension`, so purity is
under-approximated and a memoized function can return a stale value.

**Confirmed by execution.** A function reading a global *only inside a dict
literal* is memoized:

```nexum
g = 5
fn f(k):
    d = {"a": g}
    return d["a"]
fn g2():
    g = 10
    return 0
print(f(0))   # 5
g2()
print(f(0))   # 5   <- should be 10
```

`--emit-ir` shows `nx_memo_put` / `nx_memo_get` for both `fnid 1` and
`fnid 2`, so `f` is cached despite reading `g`, and the second call returns
the stale `5`.

The interpreter prints the same `5 | 5`. **Both engines are wrong and they
agree**, because both call the same `nx_ir::memoizable`. The three-way
differential therefore reports `ok` on a program that is incorrect. This is a
different failure class from the eleven above: those are divergences, this
is a shared blind spot, and no amount of interpreter/native comparison will
ever catch it. Only a property-based test — "a memoized function's result is
independent of state it does not declare reading" — would.

The same report proposed that `memoizable` ignores allocation, so a function
returning a fresh container caches a pointer the caller later frees.
**Not reproduced.** `--emit-ir` shows `mk()` in `return [1,2,3]` is *not*
memoized, and an AddressSanitizer build (via `NX_CFLAGS=-fsanitize=address`)
of a five-iteration cache-and-free loop reports no error. Recorded as
unreproduced rather than fixed.

### 1b. Test-count corrections

Measured by enumeration: **318 `#[test]` attributes over 317 distinct
functions.** `nx-interp:3388` and `:3395` stack two `#[test]` attributes on
one function. `CHANGELOG.md` states 322 in one place and 318 in another;
`docs/numeric-audit.md` states 307. All three are wrong. Zero integration
tests exist — there is no `tests/` directory anywhere, and no test anywhere
invokes `clang`.

---

## 2. Repository structure, as discovered

Nine crates, 17,290 lines, **zero external dependencies** (every manifest
line is `path = "../nx-*"`). Dependency DAG is acyclic:

```
lexer -> ast -> parser -> { types, ir, mem } -> { interp, codegen } -> driver
```

Crate roles, corrected against reality:

| Crate | Lines | Role | Accurate? |
| --- | --- | --- | --- |
| nx-lexer | 1066 | tokens, indentation | yes |
| nx-ast | 529 | `Stmt`/`Expr`/`Target` | yes |
| nx-parser | 1830 | recursive descent, precedence | yes |
| nx-types | 2796 | checker, inference, scope | yes |
| nx-ir | 888 | **side-effect summariser, not an IR** | name is misleading |
| nx-mem | 641 | allocation plan (`Stack`/`Unique`/`Shared`) | yes |
| nx-interp | 3575 | tree-walking interpreter | yes |
| nx-codegen | 4866 | textual LLVM IR + `runtime.ll` | yes |
| nx-driver | 1099 | CLI | yes |

`nx-ir` computes `Summary { prints, heap, opaque, reads, writes }` and a
`partition()` for `parallel:`. It produces **no IR and has no verifier**, so
the "reject malformed IR" requirement has zero infrastructure to extend.
Three bespoke text greps in `nx-codegen` are the entire current defence:
`assert_top_level_defines`, `allocas_are_hoisted_to_the_entry_block`,
`unboxed_slots_are_never_touched_as_boxes`.

---

## 3. Duplication, measured (Agent 8 refuted my earlier figure)

My `docs/ARGONE.md` claimed 100 `Stmt` / 133 `Expr` match arms. **That was
wrong.** Measured by distinct arms:

| Crate | Stmt arms | Expr arms |
| --- | --- | --- |
| nx-types | 23 | 32 |
| nx-ir | 33 | 8 |
| nx-mem | 25 | 17 |
| nx-interp | 23 | 24 |
| nx-codegen | 42 | 37 |
| **total** | **146** | **118** |

But the total is the wrong target. Of 264 arms:

- **105 are exhaustive language interpreters** — `check_stmt` / `exec_stmt` /
  `emit_stmt` have exactly 18 arms each; `check_expr_inner` / `eval_expr` /
  `emit_expr` exactly 17 each, with no wildcard. Adding a `Stmt` variant is a
  **compile error in three crates at once**, naming every site. This is the
  cheapest possible sync mechanism and it is free. It must not be centralised.
- **159 are partial re-derivations**, and this is where the real work is.

Duplication worth acting on, with the discriminating detail:

| Fact | Sites | Verdict |
| --- | --- | --- |
| builtin set/arity | 5 tables; `nx-types:210` is a `pub` gate that 4 sites bypass | centralise, one function |
| `Type.method` key | 4 `format!` sites + 1 `rsplit('.')` reverse-parse | centralise, make total |
| module path resolve | 4 copies, 2 of them byte-identical, 1 duplicated *within* nx-codegen | centralise |
| import list | 2 byte-identical + 1 strict superset that recurses into `fn`/`parallel` | **decide first**, it is a behaviour change |
| `substatements` | the `for elifs / if let else` sequence, hand-rolled 6 times | centralise |
| assigned-name set | 3 copies; `nx-ir:246` ≡ `nx-mem:263` arm-for-arm | centralise with a flag |
| read-name set | 2 copies; `nx-mem` lacks the comprehension shadow set | centralise |
| receiver-has-storage | 2 byte-identical + 1 **dead** copy in nx-types | centralise, delete dead |

Two dead functions found as a side effect: `Checker::writable_receiver`
(`nx-types:582`, `#[allow(dead_code)]`, zero callers) and four unused
`BinOp` predicates.

**A live bug, code-proven:** `nx_types::infer_program` hardcodes
`module_name: "__main__"` (`nx-types:2016`) but is called once per loaded
module (`nx-codegen:1167`). Every non-main module's inference is filed under
`("__main__", name)` while codegen keys on the real module, so a method call
on a local inside an imported module misses `ty_dispatch` and falls through
to "only modules, types and builtins support attribute calls".
`examples/modules/utils.nx` defines no methods, so the differential cannot
see it. Not executed; needs a fixture.

---

## 4. LLVM-only integration decision

### Corrected environment facts

My previous report claimed two blockers. **Both were wrong**, and I verified
the corrections:

| Claim I made | Reality | How verified |
| --- | --- | --- |
| `static.crates.io` unreachable | **reachable** | `cargo fetch` in a scratch crate downloaded `unicode-width v0.2.2` and resolved `getopts`, exit 0. My earlier probe fetched the CDN *root*, which returns 403 because it serves no index. |
| no C++ toolchain | **MSVC 14.44.35207 present** | `C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Tools\MSVC\14.44.35207\bin\Hostx64\x64\cl.exe` |
| no LLVM C API | **`LLVM-C.dll` 71 MB, `LLVM-C.lib` 293 KB** | both present under `C:\Program Files\LLVM` |
| no MLIR | **confirmed absent** | 93 entries enumerated in `LLVM\bin`; no `mlir-opt`/`mlir-translate`/`mlir-tblgen`, and no `opt`, `llc` or `llvm-config` |

There are no `llvm-c` headers under `LLVM\include`, which does not matter:
Rust FFI needs only `extern "C"` declarations, not headers.

### Feasibility proven, not argued

A zero-dependency probe crate (`$env:TEMP\opencode\llvmffi-probe`, empty
`[dependencies]`, builds `--offline`) was written and run. Reproduced by the
lead architect, not merely accepted from the sub-agent:

| step | result |
| --- | --- |
| per-target init via `LLVM-C.lib` | OK |
| build module, function, `LLVMBuild*`, `LLVMPositionBuilderAtEnd` | OK |
| **`LLVMVerifyModule`** | **`rc=0`** — a real in-process verifier exists; Nexum has none today |
| **`LLVMParseIRInContext` on the real 89,640-byte `runtime.ll`** | **`rc=0`, verifies `rc=0`** |
| **`LLVMLinkModules2`** merging parsed runtime + fresh module | **`rc=0`**, combined module verifies `rc=0` |
| `LLVMTargetMachineEmitToFile` | 23,696-byte COFF object, **no textual IR anywhere** |
| link with unmodified `clang`, run | exits **42** |

The existing 2,842-line textual prelude can therefore be absorbed
**structurally and unchanged** via `LLVMParseIRInContext`. That converts the
largest block of hand-written IR from an obstacle into a migration ramp.

Three ABI traps, measured, each a loud failure rather than a silent wrong
answer:

1. `LLVMInitializeAllTargetInfos`/`AllTargets` no longer exist in LLVM 23.
   Calling `LLVMGetTargetFromTriple` without per-target init access-violates
   (`0xC0000005`). Must call `LLVMInitializeX86TargetInfo/Target/TargetMC/
   AsmPrinter/AsmParser` first.
2. `LLVMDisposeDiagnosticInfos` and `LLVMWriteTargetToFile` are no longer
   exported. Linking fails with `LNK2019` naming the symbol, which is a good
   failure mode.
3. `LLVMTargetMachineEmitToFile`'s file-type argument: **`0` is
   `LLVMAssemblyFile`, not object.** Passing 0 silently produced a
   78,964-byte textual MASM file that `llvm-nm` rejected. `1` is object.

One behavioural change to get right: `runtime.ll` has **no `target triple`
and no `datalayout`**, so today clang applies host defaults implicitly. A
structured module must set both explicitly or it silently inherits whatever
the host defaults are.

### Why not inkwell, measured

`inkwell 0.10.0` (newest, published 2026-08-06) has no `llvm23-1` feature;
its feature list tops out at `llvm22-1`. Installed LLVM is **23.1.2**. It is
blocked by a hard version fact, not a preference. `llvm-sys 231.0.0`
downloads but its default build fails machine-wide because `llvm-config`
does not exist anywhere on this host; it builds only with
`no-llvm-linking`. `llvm-ir 0.11.3` is version-gated below 23 as well.

So the zero-dependency route is not merely preferable on taste — it is the
only one that works against LLVM 23 here.

### Incidental measurement

The status quo leaks its intermediate: `nxbuild-<PID>.ll` is never removed.
**1,071 files, 113 MB** in TEMP, largest 13.1 MB. A structured backend makes
that file an optional debug artifact rather than a mandatory on-disk step.

### Decision

**Structured LLVM IR generation through the LLVM C API, linked directly,
with no new crate dependency.**

Why this and not inkwell:

1. It needs **zero new dependencies**, preserving the property that makes the
   workspace build `--offline` and auditable.
2. `LLVM-C.lib` is already installed, so the route is available today.
3. Inkwell would work but drags in `llvm-sys` plus a `cc` build step and
   pins us to a matching LLVM major version through feature flags. That is
   a real maintenance cost for a project whose whole value proposition is a
   small, legible, dependency-free compiler.

The cost is honest: ~40 `extern "C"` declarations plus struct/enum mirrors,
maintained by hand against LLVM's stable C API. In exchange, HIR/MIR own the
semantics and the backend owns only instruction selection.

Rejected: textual `.ll` as the *permanent* architecture (prohibited, and
correctly so — it is why the eleven divergences were possible); building
MLIR from source (hours, and no MLIR is needed); a future MLIR backend stays
possible precisely because the backend boundary will be a trait, not a
codegen call site.

---

## 5. Corrected test and harness state

- **318 `#[test]` attributes, 317 distinct functions.** `nx-interp:3388`
  and `:3395` stack two `#[test]` attributes on one function.
  `CHANGELOG.md` says 322 in one place and 318 in another.
- **Zero integration tests.** No `tests/` directory exists anywhere. No
  test invokes `clang`. `Command::new` appears only in `nx-driver/main.rs`,
  never inside `#[cfg(test)]`.
- `tools/verify.ps1` defects, all confirmed by reading:
  - `:7` uses `%USERPROFILE%\.cargo\bin\nx.exe` — a **stale installed binary**,
    not the tree. It can report all-green against a broken checkout.
  - `:19` drops blank lines and any line starting with whitespace + `+`,
    so blank-line divergence is invisible.
  - `:67-68` uses `Compare-Object`, a set comparison with a bounded sync
    window, while the header and the changelog claim "byte-identical".
  - No `$LASTEXITCODE` check on any of the three runs. A program that fails
    identically on all three compares equal and reports `ok`.
  - `:9` globs non-recursively, so `examples/modules/` never runs.
- **CI omits the three richest examples.** `ci.yml:59` and `:132` list 11;
  `records.nx`, `methods.nx` and `syntax.nx` are never natively compiled.
  Those three are the only coverage of records, `mut self`, `del`,
  comprehensions, slices and `None`.
- **The `argone-gate` CI job I added is broken on Linux.** `argone-gate.ps1:28`
  and `argone-gate-tests.ps1:13` build paths with `\` separators, which
  PowerShell on Unix treats as literal filename characters. The job exits 2
  on `ubuntu-latest`. My error, and it needs fixing.

---

## 6. Architecture

Unchanged from the directive, with the semantics-ownership requirement made
concrete:

```
source -> lexer -> parser -> AST -> checker -> HIR -> MIR/SSA
       -> IR verification -> Nexum passes -> Backend trait -> LLVM C API -> object -> link
```

Three requirements that follow directly from section 1:

**Every semantic rule gets one owner.** HIR nodes carry the resolved rule —
not a re-derivation. The operator matrix (result type, overflow policy,
`**` saturation, shift range, zero divisor), string element semantics,
equality relation per type, dict ordering guarantee, value/copy discipline.
The backend selects an instruction; it never decides what `+` means.

**Representation stays out of HIR.** `Gen::ty_dispatch` exists precisely so
method resolution is identical with `NX_NOUNBOX` set, and its comment states
the invariant: unboxing is a representation choice, not a typing one. The
`NV { raw, ty }` split — physical form and static type as separate fields —
is the right abstraction and must survive the migration.

**Verification is the deliverable, not a side effect.** SSA single-assignment,
dominance, termination and reachability, each with a rejecting test. The
three existing `nx-codegen` greps are the template: they are already catching
real LLVM-invalid IR that no other check sees.

---

## 7. Risks

| Risk | Severity | Response |
| --- | --- | --- |
| Divergences multiplied by adding backends | critical | fix the eleven first; they are the evidence that this risk is real |
| HIR/MIR becomes a second AST | high | gate on arm-count reduction; if arms do not fall, it is not earning its place |
| Centralising the exhaustive walkers | medium | do not. They are compile-time-enforced sync |
| Hand-written LLVM FFI drifts from LLVM | medium | C API is stable; declarations are mechanical |
| Gate enforces nothing | medium | already true of CI; fix the path bug and decide enforcement explicitly |

---

## 8. What I got wrong earlier, on the record

1. I made MLIR a hard gate on the architecture. It was not, and two of the
   three blockers I cited were misdiagnoses — a 403 on the CDN root read as
   "blocked", and a C++ toolchain I never looked for properly.
2. I quoted an AST-arm count I had not measured.
3. I added a CI job that fails on Linux and never noticed.

The first is corrected by the directive. The second and third are corrected
here. The common cause is stating a measured-sounding number without
running the measurement.
