# HIR — design

Task D of ARGONE: a Nexum-owned high-level IR between the checker and
whatever consumes typed code. This document is the design; the build
turn implements it. Nothing here executes — there is deliberately no
code in this stage, so nothing can be mistaken for a working pipeline.

## 0. What D must satisfy

The gate (`docs/ARGONE-STATUS.md`, task D), one bullet per section:

| Gate item | Design section |
|---|---|
| All examples lower; `nx dump-hir` stable enough to diff in tests | §9 (dump format + stability contract), §8 (lowering covers every construct) |
| Name-free: no source identifier survives outside diagnostics | §5 (slots, IDs, diag sidecar) + the strip test |
| Every node typed; a test proves none is untyped | §4 (total `ty` field), §7 verifier rules V4–V6 |
| Verifier rejects malformed HIR, one negative test per rule | §7 (rules V1–V12, each with accept/reject sketches) |
| Integer overflow lives in HIR, proven by a test that would fail if the backend recomputed it | §4 (`ArithRule`), §10 (backend contract), §8 (lowering calls the checker's matrix) |

Architecture constraints (`docs/architecture/audit.md` §6) that bind this
design: every semantic rule gets one owner; representation stays out of
HIR (unboxing reasoning and the box/box-boundary distinction never
appear here); verification is the deliverable, not the afterthought.

The corpus is 14 examples (13 named in ARGONE plus
`examples/boundaries.nx`). "All 13 examples" in the gate text means the
full corpus.

## 1. Position in the pipeline

```
source -> lexer -> parser -> AST -> checker -> HIR -> verifier
                                                  |
                        (later stages: MIR -> backend trait -> LLVM)
```

HIR is built from a **checked** program only, exactly like codegen
today: lowering takes the AST plus the checker's published tables
(`FnInfo` per function, plus the new query API in §8) and never
type-checks anything itself. A lowering failure is an internal error,
never a user diagnostic — user errors are all reported by `nx check`
before lowering runs.

HIR is additive. The AST-to-text backend keeps working untouched until
task M retires it; HIR grows beside it behind `nx dump-hir`. There is
no flag day in this design.

## 2. Core model

A HIR program is a set of modules. Everything that was a *name* is now
either a **slot** (a binding: parameters and locals of one function) or
an **ID** (a declared entity: modules, types, methods, functions,
module-level variables). Nothing else changes shape: HIR keeps
structured statements (`if`/`while`/`for`), because control-flow
graphs are MIR's job, not HIR's.

```text
Program  { modules: [Module], entry: ModuleId }
Module   { id, globals: [Global], types: [Type], methods: [Method],
           funcs: [FuncId], top: FuncId }
Function { id, params: [(Slot, Ty)], ret: Ty, body: Block }
Block    = [Stmt]
```

ID assignment is deterministic: modules, types, methods, functions and
globals are numbered in **sorted name order** (the entry module keeps a
stable position by the same rule, not by privilege). Slots are numbered
per function: parameters `0..n` in declaration order, then every other
binding in source order of first binding. Slot *order* affects only
dump readability, never semantics — but it is specified anyway, so two
lowerings of one program are identical by construction.

`__main__` and `<top>` survive as ordinary names in diagnostics, not as
special cases in the model: the entry module is `entry: ModuleId`, and
module-level code is a synthetic function like any other.

## 3. IDs and slots

| Reference | Representation | Notes |
|---|---|---|
| Local binding | `Slot(u32)` | params first, then first-bind order (§2) |
| Module variable | `GlobalId(u32)` | per-module table; discovered as today |
| Module | `ModuleId(u32)` | sorted order; imports resolved in lowering |
| Record type | `TypeId(u32)` | per-program table (see §6) |
| Method | `MethodId(u32)` | per-program table → `{type_id, func_id, receiver}` |
| Function | `FuncId(u32)` | per-program table; methods included |
| Field | `FieldIdx(usize)` | constant offset into the type's layout |
| `self` | `Slot` | the receiver is slot 0 of a method's params |

Field access is always by constant index. The checker already proves
which field of which layout is meant (and rejects unknown fields), so
lowering records the index and no later stage ever resolves a field
name again. The same holds for method calls: `CallMethod` carries a
`MethodId`, never a name.

What is deliberately NOT an ID: literals (`Int(i64)`, `Float(f64)`,
`Bool`, `Str(String)` — strings are data, not identifiers), operators
(they are node kinds, §4), and builtin selections (`Len | Push |
Input`).

## 4. Every node typed, every rule decided

Every expression node carries `ty: Ty` — total, always present,
`Unknown` allowed where the checker proved dynamic (sugar on unresolved
bases, heterogeneous positions). "Every node typed" means the field is
never missing, and §7 turns that from a structural truism into checked
consistency rules (operand types match the recorded rule, field indices
are in range, arities line up, conditions are `Bool`).

Beside the type, each node carries the *semantic rule it decided* —
the choice some stage used to re-derive. The complete set, with the
single owner each defers to:

| Node | Rule field | Values | Decided from |
|---|---|---|---|
| Int arithmetic (`+ - * // %`, binary and unary) | `ArithRule` | `Trap` | R1: always trap on `Int` operands. The backend emits the checked intrinsic; it never chooses |
| `**` on `Int`s | `PowRule` | `Saturate` (+ negative-exponent rejection, already a checker error) | R2 |
| Float arithmetic | `ArithRule` | `Float` | operator matrix |
| Mixed `Int`/`Float` | `ArithRule` | `PromoteFloat` | `arith_result` in `nx-types` (see §8: lowering *calls* the matrix, it does not copy it) |
| `==` / `!=` | `EqRule` | `Numeric \| StrEq \| StructuralRecord \| IdentityNone` | operand types |
| Ordering `< <= > >=` | `CmpRule` | `Numeric \| StrOrder` | operand types |
| `in` / `not in` | `MemberRule` | `ListEq \| StrSub \| DictKey` | RHS type (this is what makes `"" in s` true by construction) |
| Index `a[i]` | `IndexRule` | `ListInt \| StrChar \| DictKey \| Dynamic` | base type; `StrChar` is the R3 decision |
| Slice `a[f:t:s]` | `SliceRule` | `ListCopy \| StrChars` | base type; bounds are `Int`, step positivity stays a runtime check |
| Comprehension | `IterRule` + elem rule | `List \| StrChars \| Dynamic` | iterable type; comprehension and `for` share the rule so both spellings agree |
| `for` iterable | `IterRule` | `CountedRange \| List \| StrChars \| DictKeys \| Dynamic` | header shape + type; direction stays dynamic (backend emits the up/down select as today) |
| Range value `a..b` | `RangeRule` | `AscendingOrEmpty` | value context always; the descending form exists only as a `for` header |
| String construction / concat | `StrRule` | `Chars` | marker that every string op counts characters; a future byte-string would be a new rule, not a reinterpretation |
| Assignment / bind | `CopyRule` | `CopyScalar \| ShareStr \| DeepClone` | value semantics (§4.2): containers deep-copy, strings share, scalars copy |
| `del` | `DelRule` | `Unbind \| ListRemove \| DictRemove \| RecordBlank` | target shape + container type |
| Construction `T(..)` | — (type_id + arity) | exact arity, checked | declaration |
| Field read/write | — (`FieldIdx`) | constant offset | declaration |
| Method call | `MethodId` + writeback flag | receiver included; `mut self` writes back iff the receiver has storage (decided in lowering via the shared storage predicate) | resolution order (§4.4 of the grammar) |
| Builtin call | `Len \| Push \| Input` | arity checked | ambient surface |
| Ternary | — (join type) | `then`/`else` join | checker |
| `and` / `or` | — | short-circuit | structure (MIR builds the diamond) |

`ArithRule::Trap` is the headline case for the gate: today the
backend chooses between `add i64` and the overflow intrinsic by
re-deriving what the checker knows. Under this design the lowering
records `Trap`, and the backend matches on the rule. The gate's proof
test constructs HIR with `Trap` on an addition and asserts the backend
emits the checked intrinsic without consulting operand types — i.e.,
the test fails if the backend recomputes the decision.

Where the rule comes from matters as much as where it lives: lowering
derives rules by **calling the checker's published decision functions**
(`arith_result` and the new query API in §8), never by re-implementing
their matrices. One owner per rule, one caller of each owner.

Representation is absent on purpose. There is no box/unbox distinction
anywhere in this section: `ty: Ty` is a *meaning*, and whether a value
lives in a register or a `%NxVal` remains the backend's private
decision, exactly as `ty_dispatch`/`NV { raw, ty }` work today.

## 5. Name-freedom and diagnostics

No binding, type, method, module, field, or global is referenced by a
source string anywhere in §2–§4. The single exception is diagnostics:
every node carries

```text
span: (line, col)        // where it came from
diag: Option<DiagInfo>   // original names, for errors and dump-hir only
```

`DiagInfo` holds the inert spellings (variable name, type name, method
name, field name, module name). It is never consulted for any decision
— resolution output is IDs and slots before `diag` is even attached.

Three tests pin this (the second and third are the gate's "name-free"
proof):

1. **Structural walk**: lower the full corpus; assert no `String`
   payload exists outside `DiagInfo` (literals are `i64`/`f64`/`bool`;
   `Str` literals are data and are explicitly exempted).
2. **Diag-independence**: strip every `DiagInfo` and re-run the
   verifier — same accept/reject on the corpus. Renaming nothing,
   proving names decide nothing.
3. **Dump stability** (§9): dump twice, byte-identical; dump with and
   without diag names where applicable.

## 6. Types, methods, modules in HIR

- **Types**: per-program table. Each entry keeps the field layout as
  `(FieldIdx, Ty)` pairs plus the diag name. Recursive and mutually
  recursive layouts work because entries reference `TypeId`, resolved
  after all declarations are indexed (same two-phase shape as
  `validate_records` today).
- **Methods**: per-program table `{type_id, func_id, receiver}`.
  Bodies live once, in the function table. The orphan rule
  (impl in the type's module) is a checker error and never reaches HIR.
- **Associated functions** are plain functions; a call through a type
  name lowers to `CallFn` directly (there is no receiver to pass).
- **Modules**: imports are fully resolved in lowering — the
  `ModuleId` on every reference *is* the module-path decision, so no
  later stage reads `NX_PATH` or re-searches directories. This retires
  the last re-derivation the `shape::resolve_module_file` migration
  left in place (callers still compose search dirs; after HIR they
  consume IDs).
- **Type imports and aliases** are canonicalised in lowering
  (`U(...)` becomes `Construct` on the canonical `TypeId`).

## 7. The verifier

The verifier is a separate pass over plain-data HIR: every rule below
is checkable on hand-built HIR, which is what makes the negative tests
possible. (If malformed HIR were unrepresentable in Rust, a negative
test could not exist — so HIR nodes are plain data and the verifier is
the discipline, mirroring the exhaustive-matchers-must-not-centralise
rule: the compiler, not the type system, enforces this layer.)

| Rule | Accepts | Rejects (negative test) |
|---|---|---|
| V1 def-before-use | every `Slot` read is a parameter or bound earlier in textual order | read of a never-bound slot |
| V2 references resolve | every ID is in range of its table | `TypeId(99)` with two types |
| V3 typed arithmetic | `Trap` node has two `Int` operands; `Float`/`PromoteFloat` match theirs | `Trap` with a `Float` operand |
| V4 conditions are `Bool` | `If`/`While`/assert/ternary conditions typed `Bool` | `If` on an `Int` condition |
| V5 field indices in range | `FieldIdx < field count` of the base record type | index past the end |
| V6 method match | call receiver type is the method's declared type; arity matches | receiver of another type; wrong arity |
| V7 construct arity | arg count equals field count | partial constructor |
| V8 return match | each `Return` matches the function `ret` | `Int` return from `-> Str` |
| V9 loop discipline | `Break`/`Continue` inside a loop | either outside any loop |
| V10 index/slice types | index and bounds are `Int` | `Str` index |
| V11 range bounds | both bounds `Int` | `Float` bound |
| V12 name-freedom | no `String` outside `DiagInfo` (§5 test 1) | smuggled name field |

Lowering runs the verifier on its own output before returning it: a
lowering bug fails fast at the lowering site, not three stages later.
(The verifier checks structure and consistency, never semantics — it
cannot tell a wrong-but-well-formed program from a right one. Oracle
duty stays with the expected-value tests.)

## 8. Lowering

Inputs: the checked AST, `FnInfo` per function (`infer_program_for`,
per module — the `__main__` fix this design depends on), plus a small
new query API on `nx-types` (to be added in the build turn; listed here
so the surface is designed once):

- `arith_rule(op, l: Ty, r: Ty) -> ArithRule` — thin wrapper over the
  existing `arith_result` matrix (or `arith_result` itself made `pub`);
  the single owner, called — not copied — by lowering.
- `method_query(module, type, method) -> MethodInfo` — the checker's
  resolution (including alias canonicalisation), not a re-walk.
- `field_info(type, field) -> (FieldIdx, Ty)` — index plus declared
  type in one lookup.

Phases, in order:

1. **Index.** Walk all modules: assign `ModuleId`s (sorted),
   `TypeId`s, `MethodId`s/`FuncId`s, `GlobalId`s. No bodies yet.
2. **Slots.** Per function: params `0..n`, then first-bind source
   order. `self` is slot 0 of methods (absent for associated fns).
3. **Bodies.** Lower statements and expressions, attaching `ty` from
   `FnInfo` locals and deciding every §4 rule via the query API.
   Imports, type declarations and impl blocks produce no nodes —
   only table entries and resolved references.
4. **Verify.** Run §7 over the result; failure is an internal error
   naming the rule and the span.

Lowering never reports user diagnostics (unchecked programs never
arrive) and never consults source names for decisions (only for
`diag`). `nx dump-hir` prints §9 output for one entry file.

## 9. `dump-hir` and stability

`nx dump-hir <file.nx>` prints the verified HIR as line-based
S-expressions, one module after another in ID order, functions in ID
order within a module:

```text
(module __main__
  (type Point (field x Float) (field y Float))
  (fn collatz (param n Int) -> Int
    ...))
```

Stability contract (what "stable enough to diff in tests" means):

- all tables iterate in ID (i.e. sorted-name) order — no `HashMap`
  order leaks anywhere;
- no absolute paths (module names, never file paths), no timestamps,
  no counters except slot/ID numbering;
- spans print as `line:col` (deterministic input, deterministic spans);
- diag names render for readability; §5 test 3 covers independence.

The corpus test lowers all 14 examples, dumps each twice, and asserts
byte-identical output — plus against a checked-in snapshot for at
least one example, so a representation drift in the format itself
fails loudly. (Snapshot *content* is reviewed by a human when it
changes; the test only enforces stability.)

## 10. What the backend may and must not do

When task G adapts the backend to consume HIR (the trait boundary),
this contract becomes the trait documentation. Stated now so the
lowering and the verifier already enforce the backend's half:

- MAY: match on rule fields (`ArithRule::Trap` selects the checked
  intrinsic unconditionally); use types for representation choices;
  use IDs/slots/indices for layout, offsets, symbols, and calls.
- MUST NOT: match on `diag` names for any decision; call
  `arith_result`, method tables, field tables, or module resolution;
  distinguish `NX_NOUNBOX` builds by dispatch (representation only —
  the existing `ty_dispatch` discipline survives verbatim).
- The `Trap` proof test (§4) is the template: for each rule, a test
  that constructs the HIR decision directly and asserts the backend
  honors it without re-deriving the inputs.

## 11. Extension points (present, empty)

- **Ownership / `own` (Stage 6):** `CopyRule` gains no variant today,
  but every bind site already carries the field a move rule would
  occupy; `own self` receivers already parse and lower like `self`.
  A move discipline attaches at `CopyRule` + slot-liveness, nowhere else.
- **Closures:** no AST node, no HIR node. If they arrive, captures
  become slots in a lowered anonymous `Function` — the function table
  already supports nameless entries by ID.
- **Capabilities / `dyn` (Stages 4–5):** method calls carry a resolved
  `MethodId`; dynamic dispatch would widen that field to a
  statically-unknown receiver plus a runtime lookup. The field's
  position in the node is the extension point.
- **Effect summaries:** `nx-ir` consumes AST today. Migrating it to
  HIR (richer, already-resolved input) belongs to task F, not here —
  noted so the migration has a named home.

## 12. Migration and rollback

- New crate `nx-hir` (`nx-ast`, `nx-types` as deps; direction
  preserves the DAG). New CLI verb `nx dump-hir`. New test files for
  lowering, verifier positives/negatives, dump stability, and the
  `Trap` proof test.
- The AST text backend is untouched: same inputs, same outputs, same
  tests. `verify.ps1`, CI, and benchmarks keep running against it.
- Worst case is deletion: remove the crate, the verb, and the tests.
  Nothing else references HIR until task G wires the trait.

## 13. Open questions (deferred explicitly, not decided here)

1. Whether `AssignOp` desugars to explicit temp slots in HIR or stays
   a node until MIR ( leaning: stays; §4 documents the order, MIR
   makes temps explicit — but the desugar-early alternative is
   legitimate and must be decided in the build turn, not drifted into).
2. Whether effect summaries migrate to HIR in F or stay on AST (leaning:
   migrate; HIR is strictly richer input for the same analysis).
3. Snapshot format details for dump-hir (§9 gives the contract; exact
   S-expr shapes get fixed when the first snapshot is reviewed).
4. Interprocedural detail in verifier V1 (nested functions are
   separate `Function`s; V1 checks per-function — cross-function slot
   leaks are impossible by construction since slots never cross
   function boundaries; noted here so nobody "fixes" it).
