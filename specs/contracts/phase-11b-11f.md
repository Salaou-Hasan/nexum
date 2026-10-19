# The contract for the language phases, 11b to 11f

Status: **planned**. The research for each phase is in `specs/research/`;
this document turns it into work that can be done in parallel without two
agents touching the same file.

## The contention, and what it means

Three files are touched by almost every phase: the sema's type
representation and builtin dispatch, the code generator's type mapping and
its handling of the builtins, and the MIR. The self-hosting gate is part
of every feature too: a change to the grammar has to be made in the Rust
lexer, the Rust parser, the normalised form, and the self-hosted lexer and
parser, or the self-hosted compiler silently falls behind.

So the phases are organised in **waves**, and within a wave the agents
work on **different crates** with the interfaces between them pinned by
this document. The waves are merged in order, by the orchestrator.

| Wave | Phase | Agents | Crates |
| ---- | ----- | ------ | ------ |
| A | 11b, inline assembly | 3 | parser, sema, codegen |
| B | 11c, the numeric tower | 3 | sema, codegen, MIR |
| C | 11d, memory and layout | 3 | parser, sema, codegen |
| D | 11e, concurrency | 3 | sema, codegen, runtime |
| E | 11f, vectors | 2 | sema, codegen |

Within a wave the agents never touch the same file. Between waves the
orchestrator merges, and the conflicts are resolved in the merge.

## The grammar budget is small

The research settled this, and it is the most useful fact in the
programme: the whole of 11b to 11f needs about **two reserved words**
(`unsafe`, `spawn`), **one new token** (`&`) and about ten new AST nodes.
Everything else is a named type - the type grammar is an identifier, so a
new type name is free - or a builtin, which the parser makes an ordinary
call node and the sema decides what the name means.

The cost of a grammar change is high because it is made four times: the
Rust lexer, the Rust parser, the normalised form, and the self-hosted
lexer and parser. The cost model from the research: a new type name is 0
lines, a reserved word is about 8 lines plus a source break, a new
statement form about 70, and a new literal form (a numeric suffix) about
90.

**The rule for every phase: prefer a builtin over a grammar change.** A
builtin needs no lexer change, no parser change and no change to the
self-hosted parser, so it is a fraction of the work and it keeps the
self-hosted compiler in step for free.

## The interfaces between the agents of a wave

These are pinned so that the agents of a wave do not have to agree
mid-flight. The orchestrator writes them down; the agents implement
against them.

### The parser and the sema

The parser produces a call node for a builtin, with the arguments it was
given. The sema recognises the name, checks the argument types, and
lowers it - either to an existing MIR instruction or to a new one.

**The parser agent does not need to know what a builtin means.** That is
the rule that makes the split work.

**The sema agent needs the parser's node shape**, which is already there:
a call node with a name and a list of arguments. If the feature needs a
new *syntax* rather than a new builtin, the parser agent adds it and the
sema agent is told the node kind and its fields.

### The sema and the code generator

The sema lowers a builtin to a MIR instruction with a type; the code
generator lowers the MIR instruction to LLVM. The interface is the MIR
instruction set.

**A new MIR instruction is a six-file edit** - the instruction's
definition, the sema's lowering, the MIR's five optimizer functions that
match on instructions exhaustively, and the code generator. So an agent
that needs a new MIR instruction owns all of that, or says in its report
that it does and describes the instruction it needs.

The type mapping is the other interface: `llvm_ty` in the code generator
maps the sema's types to LLVM's. A new primitive type needs a new arm
there, and the collector's `has_refs`, `scan_bytes`, `llvm_size` and
`llvm_align` need the matching arm.

**The code generator agent owns `llvm_ty` and the four tables around it.**
The sema agent owns the type enum and the builtin dispatch. Neither of
them touches the other's file, and the interface is the type enum, which
is already shared through `nexum-mir`.

### The type representation

`nexum_sema::Ty` is imported by the MIR and the code generator, so a new
type is not a sema-local change. The rule for a wave: **the sema agent
adds the type variant, and the code generator agent adds the arms in the
same wave** - they are in different crates, so there is no conflict, but
the wave does not compile until both have landed. The orchestrator
merges the sema agent's pull request first and holds the code generator's
until the wave is complete.

## The self-hosting gate is part of the feature

The gate runs the self-hosted lexer over every file in `examples/` and the
self-hosted parser over the parse fixtures. A feature that changes the
grammar has to add a parse fixture **in the same commit**, or the
self-hosted parser falls behind while the suite stays green.

The normalised form (`crates/nexum-parser/src/normalize.rs`) is the
contract between the Rust parser and the self-hosted one. Any new syntax
has to appear in it and in its counterpart in `examples/selfhost/parser.nx`.

**The rule for every phase: if the grammar changes, the normalised form
changes with it, in the same pull request.**

## The rules that apply to every phase

* A builtin is preferred over a grammar change.
* A new primitive type needs an arm in `llvm_ty`, `has_refs`,
  `scan_bytes`, `llvm_size` and `llvm_align`, and a case in the `print`
  dispatch. A missing `llvm_size` arm is a memory-safety bug, not a wrong
  answer: it is the stride and the copy length for a list of that type.
* A `print` of a float that is narrower than a `double` needs an explicit
  conversion; a raw `float` passed to a variadic function prints garbage.
* The collector never scans a numeric value, and a type that can hold a
  reference is scanned - so a new aggregate type has to answer the
  question correctly.
* The runtime is freestanding on the POSIX targets: no C library, and no
  128-bit division.
* The tests: a new fixture for a new behaviour, in
  `crates/nexum-driver/tests/fixtures/run/` for something that runs, and a
  unit test for something that is about the shape of the generated code.
  Each phase gets its own test file under `crates/nexum-driver/tests/`,
  named after the phase, so that no two phases collide.
* The `nx` CLI: a new feature that a user can reach gets a command or a
  flag, and the `nx doctor` output is updated if the toolchain's
  requirements changed.

## The waves, in order

### Wave A - inline assembly (11b)

The design is in `specs/research/inline-asm-design.md`: `asm` is a
statement, the template is a string literal whose operands are **named**
(`$a`) and never numbered, the operands are either a register the
compiler allocates or a pinned physical register, `unsafe` is required, and
the target guard is an assertion.

Three agents: the parser (the statement and its operands), the sema (the
operand types and the `unsafe` marker), the code generator (the
constraint string and the lowering).

The facts the research measured, which the implementers need:

* The operand numbering in the **call** form is outputs first, then
  inputs. A mistake there is silent.
* `~{reg}` clobbers do not consume call arguments.
* The LLVM inline assembly template does not interpret `\n`; it must be
  `\0A`.
* An immediate operand is not portable: LLVM substitutes it as `$5` on
  x86-64 but as a bare number on AArch64, which is invalid there. So
  immediates are not operands in this phase; a constant goes in the text.
* A function **with** a frame sees the stack 16-byte aligned; a frameless
  one does not. The compiler has to force a frame, which is what the
  runtime's system call layer does by hand.
* A floating-point operand is deferred: AArch64 prints a pinned `d0` as
  `v0`, so the float operands wait for 11c and 11f.

### Wave B - the numeric tower (11c)

The design is in `specs/research/numeric-tower.md` and D018: ten types,
distinct widths, explicit conversions, wrapping integers, truncating
division, one rounding mode.

The slices, in order: the comparison defect (done - the codegen tested
the result type instead of the operand type, so the float case never
fired), then the type and its code generator mapping together, then the
MIR constant and `print`, then the literal syntax last and only if it is
wanted.

The research recommends the **conversion-builtin route over a literal
suffix**, because the normalised form and the self-hosted parser disagree
in principle about a literal's parsed value against its raw source text.
That decision is the orchestrator's to confirm before the wave starts.

### Wave C - memory and layout (11d)

The design is in `specs/research/memory-layout.md` and
`specs/research/record-layout.md`: one pointer type with an optional
pointee, a pointer is never a root, `unsafe` is a block and a function,
`volatile` is an access-site qualifier, the arena is a lexical scope over
a chunked bump allocator, and `defer` is the cleanup construct.

The arena needs a raw allocation with an explicit size, which phase 11a.2
provides.

### Wave D - concurrency (11e)

The design is in `specs/research/concurrency.md` and
`specs/research/atomics-model.md`: `clone` on Linux, the thread-local
collector, the atomic types as handles onto a heap cell, and the orderings
with `seq_cst` as the default because a default must not be a
correctness trap.

The atomics are the one slice that can land on its own: the LLVM
instructions map directly, and the runtime is not involved. The collector's
thread-local form is the bigger piece, and it changes the compiler's
root-frame handling.

The research found three things the implementer needs: a `cmpxchg` takes
**two** orderings, a success and a failure; a `load atomic` and a
`store atomic` must carry an explicit non-zero `align` or the module does
not parse; and an `i1` atomic is rejected, so the boolean atomic is an
`i8`.

### Wave E - vectors (11f)

The design is in `specs/research/simd-vectors.md`: one constructor,
`Vec<T, N>`, with the width in the type, the ordinary operators for the
element-wise work, and `vec_`-prefixed builtins for the rest. The
constructor is a **call to the type**, because type arguments on a free
function are not parseable - `<` is the comparison operator.

The layout is exactly `N` times the element size, and the alignment is
the target's. A vector is never scanned: the element type cannot be a
managed reference.

## The order the programme runs them in

The GPU path is not a wave: the research showed that Vulkan is a C API
with a loader behind it, so the route the roadmap described is not
reachable under the programme's own rules. The route that is reachable is
the kernel's own ioctls, and it needs a per-vendor driver, so it comes
after the model framework exists and has something to run.

The model framework (11i) needs the numeric tower, the maths builtins, the
allocator's `noalias` pointers, and the vector support. It is the reason
for the programme, and it is the phase that proves the rest.
