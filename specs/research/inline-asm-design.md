# Inline assembly as a first-class Nexum feature (phase 11b)

**Question.** What should an inline-assembly block look like in Nexum, what is
its operand model, what does the language guarantee about it, how does it lower
to LLVM IR, what happens when a program containing it is compiled for another
target, and how does it interact with the collector, the root frames and the
stack-alignment rule the system call layer has to honour? The deliverable is a
design concrete enough that a parser agent, a sema agent and a codegen agent
can implement it in parallel without guessing.

**Summary.** Inline assembly in Nexum is a **statement**, not an expression:
`unsafe asm "<template>" in (...) out (...) clobber (...)`. The template is a
string literal whose operands are referred to **by name** (`$a`), never by
number, and the compiler rewrites the names into LLVM's positional form; a
literal dollar is written `$$`. The operand model has exactly **two kinds** -
a general register the register allocator chooses (`reg`), and a **pinned
physical register** (`rdi`) - and **no memory operands, no immediate operands
and no labels** in this phase; a constant is written in the assembly text,
exactly as the runtime's system call layer already does, because LLVM's
substitution of an immediate is not portable between the two instruction sets
(verified below). Every block is marked **`unsafe`**, which is phase 11d's
keyword introduced early in its minimal form, and the compiler guarantees
only what it can: the declared clobbers are dead afterwards, the declared
operands are where the declaration says, arbitrary memory is treated as
clobbered, the stack is 16-byte aligned at the block, and the collector cannot
run inside it. The block lowers to a single LLVM `call ... asm sideeffect`
with the outputs first in the constraint string and the inputs as the call
arguments. The assembly text is target-specific; a block may carry a `target`
guard, and a guard that does not include the compilation target is a
**compile-time error** (T059) rather than a silent omission, because Nexum has
no conditional compilation yet.

## Recommendation

1. **The surface syntax is a statement with a named template and a named
   operand list.** `unsafe asm "template" in (label: class) value out (label:
   class) destination clobber (reg, ...)`. The template is a *string literal*,
   because Nexum has no brace tokens and the lexer already accepts a
   multi-line string. Operands are referred to by **name** (`$label`), not by
   number, and the compiler rewrites the names to LLVM's `$N` form. A literal
   dollar in the template is written `$$`. Reason: the runtime layer's own
   documentation records that "the numbering of the operands in an assembly
   block counts the output as operand zero, and a mistake there is silent and
   fatal" (`crates/nexum-runtime/src/syscalls.rs:12-15`). A name-based
   reference removes that failure mode, and the compiler can then *check* it
   (T051, T052).

2. **It is a statement, not an expression.** The outputs of a block name the
   variables they are written into; the values are stored, not returned.
   Reason: Nexum has no tuples and no destructuring, so an expression form
   could only ever have one output, and `rdtsc`/`cpuid` need two and four.
   The statement form handles any number of outputs with one rule, and it
   matches the language's existing mutability model (a `let mut` binding
   lives in a stack slot, which is where the register is stored).

3. **Two operand kinds: `reg` (the register allocator chooses) and a pinned
   physical register.** The default is `reg`; a program that wants a specific
   register says so. Reason: the system call layer and every privileged
   instruction *need* pinned registers, and ordinary code should not have to
   care. LLVM allocates for `reg` and is forced for a pinned name, so the
   language exposes exactly the choice, and the choice is in the program
   rather than in the compiler.

4. **No memory operands, no immediate operands, no labels in phase 11b.**
   Memory operands need pointers (phase 11d); labels need branch targets the
   language does not have. Immediates are excluded for a measured reason:
   LLVM's substitution of an immediate operand produces `$5` on x86-64 but a
   bare `7` on AArch64 (which is not a valid AArch64 immediate - it needs
   `#7`), and the `n` constraint means "register or 32-bit immediate" on
   x86-64 but "immediate only" on AArch64. A constant is therefore written in
   the assembly text, where the dialect's own syntax applies - which is
   exactly what the runtime layer does today, and what
   `docs/runtime-abi.md:46-49` already calls out as one of the three
   details of the system call layer that are silent when they are wrong.

5. **The block is marked `unsafe`.** `unsafe asm ...` is required; a block
   without the marker is T050. Reason: the language guarantees only the
   clobbers and the operand placement; the block can still corrupt the stack,
   the frame pointer, the collector's invariants and the machine state the
   language does not model. Marking it is the honest thing to do, and it
   gives phase 11d a landing site instead of a breaking change. `unsafe` is
   introduced now in its minimal form - it is only allowed in front of `asm` -
   and phase 11d widens it to unsafe blocks and unsafe functions.

6. **The compiler guarantees exactly five things** (the "contract" in
   *The safety story* below): the block runs once, in order, and is never
   removed or moved; the declared clobbers hold nothing live afterwards; the
   declared operands are where the declaration says; arbitrary memory is
   treated as clobbered; the stack is 16-byte aligned at the block. Everything
   else is the programmer's promise.

7. **The collector cannot run inside a block, and the root frame stays live
   across it.** Collection only happens inside `nexum_alloc` and `collect()`
   (`crates/nexum-runtime/src/lib.rs:147-156`); a block is not an allocation
   site, and the kernel does not allocate on our heap. The compiler therefore
   needs no temporary-root machinery for a block, *because* an output of a
   managed type (`Str`, `List`) is rejected (T054): a pointer the collector
   has never seen cannot be described by a root set the compiler builds.

8. **The lowering is one LLVM `call ... asm sideeffect`.** The constraint
   string is `outputs, inputs, clobbers, ~{memory}`; the call arguments are
   the inputs, in declaration order; the template's `$N` numbering is outputs
   first, then inputs (verified: `movq $0, $1` with `"={rax},{rdi}"` emits
   `movq %rax, %rdi`). `~{memory}` is **always** emitted, so a block that
   touches a device register cannot be reordered against a memory access by
   accident. Newlines in the template are written `\0A` in the LLVM string,
   because LLVM's inline-assembly template does not interpret `\n` (verified).

9. **The stack alignment is the compiler's job, not the user's.** A function
   that contains a block is given a stack frame by the compiler, so the block
   is entered with a 16-byte aligned stack on the targets whose ABI requires
   it. This is measured, not assumed: a function *with* a frame sees
   `%rsp & 15 == 0` at the block and a function *without* one sees `8` - which
   is precisely why the runtime's system call wrappers align the stack by
   hand (`crates/nexum-runtime/src/syscalls.rs:171-180`). A block that changes
   the stack pointer itself declares `alignstack`, and LLVM then keeps the
   stack aligned around it.

10. **Target dependence is an assertion, not a filter.** A block may carry
    `target "linux-x86_64", "macos-x86_64"`, naming the targets its text is
    valid for. If the compilation target is not in the list, the compiler
    reports **T059** and stops. Without a guard, the text is compiled for the
    current target and the assembler decides. Reason: Nexum has no
    conditional compilation, so a "skip the block on the other target"
    behaviour would be a silent change of meaning, and per-target *variants*
    need a general conditional-compilation construct that phase 11b should
    not own. The orchestrator should decide whether to add that construct
    (see *Open questions*).

11. **Three crates change, and their interfaces are pinned below.** The
    parser owns the surface syntax and the AST; the sema owns every
    target-dependent decision (it needs the compilation target, so
    `nexum_sema::analyze` gains an `analyze_for(am, target)` entry point and
    `nexum-sema` gains a dependency on `nexum-target`); the MIR and the
    codegen own the lowering and the emission. The sema produces the
    *rewritten* template, the per-operand constraints and the whole
    constraint string; the codegen produces the LLVM call, the stores and the
    frame anchor.

## What the code looks like today

Facts from the repository, with the ones the recommendation depends on.

### The mechanism already works: the system call layer is inline assembly

* `crates/nexum-runtime/src/syscalls.rs:7-21` states the two rules the layer
  is built on: *"The arguments are pinned to the registers the instruction
  reads them from, with the constraint string, so the assembly never has to
  move them and never has to refer to an operand by number. The numbering of
  the operands in an assembly block counts the output as operand zero, and a
  mistake there is silent and fatal; naming the registers avoids the
  question."* and *"The system call instruction needs a 16-byte aligned
  stack."*
* `crates/nexum-runtime/src/syscalls.rs:110-150` - the per-(os, arch) layout:
  the argument registers, the result register, the number register, the
  instruction and the clobbered registers. x86-64 Linux and macOS use
  `syscall` with the arguments in `rdi, rsi, rdx, r10, r8, r9`; AArch64 Linux
  uses `svc #0` with the number in `x8` and Darwin `svc #0x80` with the number
  in `x16`.
* `crates/nexum-runtime/src/syscalls.rs:168-231` - the body generator. It
  builds the text, the constraint string and the operand list. Three details
  matter for the language design:
  * the constraint string is `={result},{arg},...~{clobber},...~{memory}` -
    the output first, then the inputs, then the clobbers, then the memory
    clobber (`:211-218`);
  * the text is a single line and the newlines are escaped: *"The text of an
    assembly block is one line: the newlines are escapes"* (`:207-208`), i.e.
    `\n` becomes `\0A`;
  * a literal dollar is written twice: *"A literal dollar is written twice in
    an assembly block: a single one starts an operand reference"* (`:175-177`,
    `:182-184`), i.e. `$$`.
* `crates/nexum-runtime/src/syscalls.rs:334-339` - the test that pins the
  stack alignment (`and $$-16, %rsp` / `mov %r12, %rsp`).
* `crates/nexum-runtime/src/syscalls.rs:344-350` - the test that pins the
  constraint shape (`"={rax},{rdi},{rsi},{rdx}"`).
* `crates/nexum-runtime/src/syscalls.rs:352-368` - the test that pins the
  `$$` escape, expressed as "a dollar that is not doubled is an operand
  reference".
* `docs/runtime-abi.md:34-52` restates the same three silent-failure
  details, and `docs/runtime-abi.md:54-72` gives the per-system numbers and
  the per-architecture conventions.
* `docs/language-reference/decisions.md:198-220` - D013 ("the operating
  system is reached through inline assembly") and D014 ("inline assembly is a
  first-class language feature, not a runtime-only trick"; *"the language
  surface is phase 11b. The runtime already uses the mechanism it will be
  built on."*).

### The front end today

* `crates/nexum-lexer/src/lib.rs:7-63` - the token kinds. There is no
  `unsafe`, no `asm`, no `{` and no `}`. `in` exists (`KwIn`), `out`,
  `clobber`, `alignstack` and `target` do not.
* `crates/nexum-lexer/src/lib.rs:260-302` - string literals. The scanner
  consumes characters until the closing quote **including newlines**, so a
  multi-line string is already a single token. *Verified:* `nx lex` on a
  source file with a two-line string literal emits one `Str` token whose text
  contains the newline.
* `crates/nexum-lexer/src/lib.rs:342-360` - the keyword table, and
  `:66-123` - `TokenKind::name()`, the stable token names the self-hosted
  lexer is compared against. A keyword is a real token, so `asm` and
  `unsafe` need entries in both.
* `crates/nexum-parser/src/lib.rs:41-76` - the AST expression kinds; `:95-153`
  - the statement kinds; `:336-506` - the statement dispatch (a keyword arms
  the dispatch, a bare identifier arms the path-assignment lookahead).
* `crates/nexum-parser/src/lib.rs:1206-1221` - a call is `identifier "(" args
  ")"`; a bare identifier is a variable. There is no other way to introduce a
  call, so `asm` must be a keyword to be a statement.
* `crates/nexum-sema/src/lib.rs:14-70` - the type lattice. `Int` is `i64`,
  `Float` is `double`, `Str` and `List` are the managed types, `Bool` is
  `i1`, records and arrays are aggregates.
* `crates/nexum-sema/src/lib.rs:91-121` - the HIR statement kinds; `:123-180`
  - the HIR expression kinds. Every HIR node carries its type.
* `crates/nexum-sema/src/lib.rs:208` - `pub fn analyze(am: &AModule) ->
  SemaResult`. The sema has **no** target today, and the driver calls it at
  `crates/nexum-driver/src/main.rs:1056` and in the test harness at
  `crates/nexum-driver/tests/common/mod.rs:29`.
* `crates/nexum-mir/src/lib.rs:46-135` - the MIR instructions; `:329-517` -
  the statement lowering; `:566-578` - a variable read is a `Load` from the
  binding's slot.
* `crates/nexum-codegen/src/lib.rs:280-350` - the memory plan, a pre-scan of
  the function that decides the stack slots; `:399-458` - the prologue, where
  every `alloca` is hoisted and the root frame is built and pushed.
* `crates/nexum-codegen/src/lib.rs:1192-1195` - `nexum_roots_pop()` before
  every return.
* `crates/nexum-target/src/lib.rs:3-9` - `TargetSpec { triple, pointer_width,
  os, arch }`; `:38-73` - the five target profiles: `windows-x86_64`,
  `linux-x86_64`, `linux-aarch64`, `macos-x86_64`, `macos-aarch64`. That is
  exactly the (os, arch) axis the system call layer varies over
  (`crates/nexum-runtime/src/syscalls.rs:110-150`).
* `crates/nexum-toolchain/src/lib.rs:221-248` - `verify_ir_for` assembles
  the module **for the target the module claims**, which is what makes a
  target-specific assembly block checkable.
* `crates/nexum-diagnostics/src/lib.rs:6-11` - `Severity` has `Error`,
  `Warning` and `Note`; `:24-42` - only an `error` constructor exists.

### What the measurements say (clang 23.1.3, LLVM IR, run and inspected)

These were measured on this machine, not recalled:

| Question | Answer |
| --- | --- |
| How is an operand numbered in the template? | Outputs first, then inputs. `movq $0, $1` with `"={rax},{rdi}"` emits `movq %rax, %rdi`; `movq $1, $0` emits `movq %rdi, %rax`. |
| Do `~{reg}` clobbers consume call arguments? | No. `"={rax},{rdi},{rsi},{rdx},~{rcx},~{r11},~{r12},~{memory}"` with three arguments is accepted - this is the runtime layer's exact shape. |
| Is a constraint string of only clobbers legal? | Yes: `call void asm sideeffect "nop", "~{memory}"()` is accepted. |
| How is a literal `$` written? | `$$`. `movq $$7, %rax` emits `movq $7, %rax`. |
| Does the LLVM template interpret `\n`? | **No.** `mov x16, #1\nsvc #0x80` reaches the assembler with a literal backslash-n. The newline must be `\0A` - which is what `crates/nexum-runtime/src/syscalls.rs:208` already does. |
| How is a register operand substituted? | With the dialect's sigil: `%rdi` on x86-64 AT&T, `x1` on AArch64. |
| How is an *immediate* operand substituted? | `$5` on x86-64 (valid), but a bare `7` on AArch64 (invalid - AArch64 needs `#7`). |
| What does the `n` constraint mean? | On x86-64 "register or 32-bit immediate" (a 2^40 value was materialised into a register); on AArch64 "immediate only" (`constraint 'n' expects an integer constant expression`). |
| How do several outputs work? | The call returns a literal struct: `call {i64, i64} asm sideeffect "rdtsc", "={rax},={rdx}"()`, then `extractvalue`. |
| What if an output is never used? | LLVM errors with *"could not allocate output register for constraint 'r'"*. Every declared output must be stored. |
| Is the stack aligned at a block? | A function **with** a frame: `%rsp & 15 == 0`. A function **without** one: `8`. The difference is exactly what the runtime layer compensates for by hand. |
| What is the x86-64 inline-assembly dialect? | AT&T, on every x86-64 target including Windows. |
| Does `alignstack` exist? | Yes: `call void asm sideeffect alignstack "..." "..."()`. LLVM then keeps the stack aligned around the block by adding a push/pop pair. |
| Is a block duplicated by the optimiser? | No. (`sideeffect` prevents removal; a loop keeps one copy in the body.) |

## The design

### 1. The surface syntax

```
asm-stmt      = "unsafe" "asm" str-lit { asm-opt }

asm-opt       = "in" asm-operand
               | "out" asm-operand
               | "clobber" "(" ident { "," ident } ")"
               | "alignstack"
               | "target" str-lit { "," str-lit }

asm-operand   = "(" ident [ ":" asm-class ] ")" expr

asm-class     = "reg" | register-name
```

* `asm` and `unsafe` are **keywords** (new tokens `KwAsm`, `KwUnsafe`).
  `out`, `clobber`, `alignstack` and `target` are **keywords** as well
  (`KwOut`, `KwClobber`, `KwAlignStack`, `KwTarget`). `in` is the existing
  `KwIn`.
* `reg` and the register names are **contextual**: they are plain identifiers,
  and the parser distinguishes them by their text. `reg` is the general
  class; anything else is a physical register name, validated by the sema
  against the target's register table.
* The class may be omitted; the default is `reg`.
* The template is a **string literal**, and it comes first, so there is no
  parsing ambiguity between the template and an operand's value expression
  (a `Str` is a valid expression, and the parser must be able to tell them
  apart).
* The template may span several source lines: the lexer already returns a
  multi-line string as one token. The price is that the options then sit on
  the template's last line. A block form (`unsafe asm:` with an indented
  list of options) would fix that and is the natural follow-up; it is listed
  under *Open questions*.

The three candidates the task asks to compare:

**(a) A template string plus a constraint string (LLVM-shaped).**
`asm("mov %rdi, %rax", "=r,r")`. This is the easiest to lower - the
constraint string is already the answer - but the user has to know LLVM's
constraint language, the operand numbering is positional, and a mistake is
silent. It also leaks LLVM's *template* dialect (`$0`, `$$`) into the
language, which the runtime layer already finds hazardous
(`crates/nexum-runtime/src/syscalls.rs:12-15`). **Rejected as the user-facing
form**, but it is what the design lowers to, and the lowering is where the
LLVM knowledge belongs.

**(b) A structured form, one operand per line with a register class.**
`asm { in a: reg = x; out r: rax }`. This has the advantage of not requiring
the user to know LLVM, and the disadvantage that real assembly is not
structured: an instruction sequence is free text, and a form that wants one
instruction per line cannot express it. It is also a much larger grammar
change (the language has no braces). **Rejected**, but its one good idea -
naming a register *class* rather than a constraint letter - is kept.

**(c) A register-allocator-free form, the user names the registers directly.**
`asm "mov %rdi, %rax"`. This is what the runtime layer does today, and it is
exactly what a system call needs. But taken alone it forces every program to
know the ABI, gives the register allocator nothing to do, and makes the
compiler unable to check anything. **Adopted as one of the two operand
kinds**, not as the whole form: the program says `in (a: rdi) fd` and the
compiler emits the constraint `{rdi}`.

The recommendation is therefore **(a) lowered, (b) informed, (c) available**:
the user writes names and classes, the compiler writes constraints.

A worked example - the system call layer's `write`, in Nexum:

```
fn sys_write(fd: Int, buf: Int, len: Int) -> Int:
    let mut r = 0
    unsafe asm "movq $$1, %rax\nsyscall" in (a: rdi) fd in (b: rsi) buf in (c: rdx) len out (res: rax) r clobber (rcx, r11)
    return r
```

and the same block with a multi-line template:

```
    unsafe asm "movq $$1, %rax
syscall" in (a: rdi) fd in (b: rsi) buf in (c: rdx) len out (res: rax) r clobber (rcx, r11)
```

### 2. The operand model

| Kind | Surface | LLVM constraint | Value | Phase 11b |
| --- | --- | --- | --- | --- |
| General register | `in (a: reg) x` | `r` | the value of `x` | yes |
| Pinned register | `in (a: rdi) x` | `{rdi}` | the value of `x` | yes |
| Immediate | `in (a: imm) 5` | `n` | a constant | **no** - see below |
| Memory | `in (a: mem) p` | `m` | the address of `p` | **no** - needs pointers (11d) |
| Label | `label:` | - | a branch target | **no** - the language has no labels |

* **Values in, values out.** An input is an arbitrary expression, evaluated
  once, before the block. An output is a *destination*: a whole `let mut`
  binding, into which the register's value is stored. The destination must be
  a whole binding - a field or an element is rejected, the same rule T037
  applies to a `mut` argument.
* **Registers.** Two kinds, as above. A pinned register is the way to reach
  a specific machine location: a system call's argument registers, a
  privileged instruction's operand register, a device register read through a
  `mov`.
* **Immediates are not operands in this phase.** A constant is written in the
  assembly text, where the dialect's own syntax applies: `movq $$1, %rax` on
  x86-64, `mov x16, #1` on AArch64. The reason is measured, not stylistic
  (see the table above): LLVM substitutes an immediate operand as `$5` on
  x86-64 but as a bare `7` on AArch64, where it is not a valid immediate; and
  the `n` constraint means different things on the two targets. The runtime
  layer already made this choice - the system call number is a constant in
  the text, not an operand
  (`crates/nexum-runtime/src/syscalls.rs:182-187`).
* **Memory operands are deferred** to phase 11d, which introduces pointers.
  Until then there is no address to give. Note that a block still *clobbers*
  all memory (see below), so a device register can be read through the text
  even before pointers exist.
* **Labels are deferred**: the language has no branch targets, and LLVM's
  label operands need them.
* **Types.** An operand's type is the type of its value. In phase 11b only
  `Int` (i64) is accepted, so the operand is an integer register. `Float` is
  deferred: on AArch64 LLVM prints the substituted name of a floating-point
  operand as a *vector* register (`v0`) even when the constraint pins `d0`,
  and `fmov v0.2d, v0.2d` does not assemble, so the floating-point story
  needs the numeric tower (11c) and the SIMD work (11f) to settle. A `Bool`
  is rejected as well (an `i1` in a register is not a useful thing to hand
  to assembly); a program writes `if b { 1 } else { 0 }`.
* **Output count.** Any number. The LLVM call returns a literal struct for
  more than one output and the codegen `extractvalue`s each field.

### 3. The safety story

**What the language guarantees** (the contract of `unsafe asm`):

1. **The block runs once, in order.** It is emitted as a `sideeffect` call,
   so the optimiser may not remove it, and it is not moved across another
   side-effecting instruction. It is not duplicated either: measured, a block
   in a loop stays one copy. (A block that *does* have to be run twice -
   because the program calls something twice - is the programmer's business,
   not the compiler's.)
2. **The declared clobbers are dead afterwards.** Every register in
   `clobber (...)` is passed to LLVM as `~{reg}`, so nothing live is kept in
   it across the block.
3. **The declared operands are where the declaration says.** A pinned
   operand is in that physical register; a `reg` operand is in a register of
   the general class; the compiler checks the register's class against the
   operand's type.
4. **Arbitrary memory is clobbered.** `~{memory}` is always emitted, so the
   compiler never moves a load or a store across the block and never assumes
   memory is unchanged. A block that reads or writes a device register
   therefore behaves like a `volatile` access without the programmer having
   to remember to say so.
5. **The stack is 16-byte aligned at the block** on the targets whose ABI
   requires it, because the compiler gives every function that contains a
   block a stack frame. Measured: with a frame the block sees
   `%rsp & 15 == 0`; without one it sees `8`.

**What the language cannot guarantee** (the programmer's promise):

1. **The block does not corrupt the stack, the frame pointer or the return
   address.** The compiler cannot read the text, so it rejects the obvious
   registers (`rsp`, `rbp`, `sp`, `x29`, `x30`) as clobbers (T058) but
   cannot stop the text from writing through them. A block that changes the
   stack pointer declares `alignstack`, and LLVM then keeps the stack
   aligned around it.
2. **The block does not break the collector's invariants.** The root frame
   lives on the stack (`crates/nexum-codegen/src/lib.rs:433-457`); a block
   that overwrites the stack destroys the root set. A block that writes into
   the managed heap can corrupt an object the collector will later scan.
3. **The block does not produce a managed reference.** Enforced by the
   compiler: an output of type `Str` or `List` is T054. A pointer the
   collector has never seen cannot be described by a root set the compiler
   builds, and this is the one thing the collector's model cannot absorb.
4. **The block does not leave the machine in a state the language does not
   model.** The language has no model of privileged mode, of MMIO, of
   interrupt state, or of a CPU feature that may not exist. That is the
   point of the feature; it is also why it is `unsafe`.

**Why `unsafe` and not something else.** The alternatives were: no marker at
all (then phase 11d has to add `unsafe` to every existing assembly program -
a breaking change - and in the meantime the most dangerous construct in the
language is unmarked), a compiler flag or a manifest opt-in (invisible at the
call site, not inherited by an imported module, and not part of the language
specification), and a separate keyword (then phase 11d has two markers to
merge). `unsafe` in front of `asm` is the smallest thing that is honest, and
it is the same keyword phase 11d will widen. In phase 11b `unsafe` is
accepted **only** in front of `asm`; anywhere else it is a parse error, so
phase 11d can widen it without a grammar ambiguity.

### 4. The lowering to LLVM

**The register allocation model.** The language exposes the choice and the
program makes it:

* `reg` - LLVM allocates. The constraint is `r`; the template's `$label` is
  substituted with whatever register LLVM picked.
* a pinned name - LLVM is forced. The constraint is `{rdi}`; the template's
  `$label` is substituted with `%rdi`.

The runtime layer pins every register by hand because a system call has no
choice; ordinary code should not have to. The default is therefore `reg`, and
a program that needs a specific register says so. This is the whole answer to
"should the language expose the pinning or let LLVM allocate": **both, with
the allocation as the default.**

**The exact IR.** For the `sys_write` example above:

```
%v5 = call i64 asm sideeffect "movq $$1, %rax\0Asyscall", "={rax},{rdi},{rsi},{rdx},~{rcx},~{r11},~{memory}"(i64 %v3, i64 %v4, i64 %v6)
store i64 %v5, ptr %v7
```

and for two outputs:

```
%v5 = call {i64, i64} asm sideeffect "rdtsc", "={rax},={rdx},~{memory}"()
%v6 = extractvalue {i64, i64} %v5, 0
store i64 %v6, ptr %v8
%v9 = extractvalue {i64, i64} %v5, 1
store i64 %v9, ptr %v10
```

The rules:

* The constraint string is **outputs, then inputs, then clobbers, then
  `~{memory}`** - the same shape the runtime layer uses
  (`crates/nexum-runtime/src/syscalls.rs:211-218`).
* The call's **arguments are the inputs**, in declaration order. The outputs
  are not arguments; their values come back in the call's result.
* The template's `$N` numbering is **outputs first, then inputs**
  (measured). The compiler rewrites `$label` into `$N` accordingly, and the
  numbering is fixed by the declaration order: the outputs in the order they
  were declared take `0..n-1`, the inputs in the order they were declared
  take `n..`.
* A literal dollar in the template is `$$`, and the compiler passes `$$`
  through unchanged: both conventions mean "a literal dollar", so the rewrite
  is the identity for it.
* A `$` that is not followed by an identifier and not doubled is **T060**.
  A `$` followed by a digit is the case the runtime layer warns about
  (an operand number silently means something else); the compiler rejects it
  with a message that says to use `$$`.
* Newlines in the template become `\0A` in the LLVM string, because LLVM's
  inline-assembly template does not interpret `\n` (measured). The codegen
  does this, exactly as `crates/nexum-runtime/src/syscalls.rs:207-208` does.
* `sideeffect` is always present. There is no "pure" form in phase 11b, and
  therefore no `volatile` keyword to spell: **every block is volatile**. The
  word `volatile` is reserved for phase 11d's memory-access qualifier
  (`docs/native-roadmap.md:72`), which is a different thing, and spelling it
  here would invite the confusion. If the orchestrator prefers an explicit
  marker, `volatile` can be accepted as a no-op without changing anything
  else - see *Open questions*.
* `alignstack` is emitted when the block declares it.

**The frame anchor.** A function that contains a block gets a 16-byte
`alloca` in its prologue, so LLVM cannot elide the frame. This is what makes
the 16-byte alignment guarantee true, and it is measured: without a frame
the block sees `%rsp & 15 == 8`, with one it sees `0`. The codegen's existing
pre-scan (`crates/nexum-codegen/src/lib.rs:280-350`) is where the decision
belongs: it already walks the instructions of a function to plan the stack
slots, and it can set a flag when it sees an `Instr::Asm`.

### 5. Target dependence

The assembly text is in the target's dialect (AT&T for x86-64, the AArch64
syntax for AArch64) and is valid for one instruction set.

* **Without a guard**, the block is compiled for whatever target is being
  built, and the assembler decides whether the text is valid. The toolchain
  already assembles for the target the module claims
  (`crates/nexum-toolchain/src/lib.rs:221-248`), so the failure is at the
  right target - but the message is the assembler's. The compiler should
  therefore **prefix the assembler's error with the source location and the
  target triple**, so the user learns which target rejected the text.
* **With a guard**, `target "linux-x86_64", "macos-x86_64"` names the
  targets the text is valid for. If the compilation target is not in the
  list, the compiler reports **T059** - "this assembly block is written for
  `linux-x86_64`, but the program is being compiled for `macos-aarch64`" -
  and stops. The guard is an **assertion**, not a filter.
* **Per-target variants are deferred.** A program that must run on two
  targets with different assembly needs a conditional-compilation
  construct, which Nexum does not have. When it lands it should be a
  general `when target ...` form, and the guard should then become a filter
  (the block is omitted) rather than an assertion. Until then, an assertion
  is the honest behaviour: it never changes the meaning of a program
  silently. See *Open questions*.

The target profiles are the five names in
`crates/nexum-target/src/lib.rs:75-83` - `windows-x86_64`, `linux-x86_64`,
`linux-aarch64`, `macos-x86_64`, `macos-aarch64` - which are exactly the
(os, arch) pairs the system call layer varies over.

### 6. The collector and the root frames

Facts:

* The root frame is a compiler-generated struct on the function's stack; it
  is built in the entry block and pushed there
  (`crates/nexum-codegen/src/lib.rs:433-457`), and popped before every
  return (`crates/nexum-codegen/src/lib.rs:1192-1195`).
* A collection runs **only** inside an allocation (`nexum_alloc` calls
  `nexum_gc_maybe`, `crates/nexum-runtime/src/lib.rs:147-156`) or in the
  `collect()` builtin. Those are the safepoints
  (`docs/runtime-abi.md:167-169`).
* Every reference-holding local lives in a stack slot, and every aggregate
  that can hold a reference is spilled into a registered slot
  (`crates/nexum-codegen/src/lib.rs:280-389`).

Consequences for a block:

1. **The root frame is live across the block.** It is pushed in the entry
   block and popped at the return, and a block in the middle of the function
   does not disturb that. The frame is on the stack, so the guarantee is
   conditional on the block not corrupting the stack - which is the
   programmer's promise, and the reason `rsp` and `rbp` are rejected as
   clobbers.
2. **The collector cannot run inside the block.** A block is not an
   allocation site, and the kernel does not allocate on our heap. So the
   root set is stable across the block, and the compiler needs **no
   temporary-root machinery** for it.
3. **A block cannot produce a managed reference** (T054). This is what
   makes point 2 sound: if a block could produce a `Str`, the pointer would
   be in a register with no root describing it, and the next allocation
   would free it.
4. **A managed value may be *passed in* to a block.** A `Str` is a pointer
   to a payload; the value lives in a rooted slot, the block reads the
   pointer, and the object stays alive because the slot is still a root.
   (In phase 11b an operand's type is `Int` only, so in practice the
   pointer would be passed as an `Int`; the rule is recorded here because
   phase 11d will want it.)
5. **The block is not a safepoint**, so it does not have to spill anything
   around itself. If phase 11d ever allows a managed output, the codegen
   must then spill the result into a rooted temporary immediately, exactly
   as it does for a call's result
   (`crates/nexum-codegen/src/lib.rs:317-326`, `:868-882`).

### 7. The stack alignment rule

The system call layer has to align the stack by hand because *"A function
without a frame is entered with the stack at 8 modulo 16, so the wrapper
aligns it itself and restores it afterwards"*
(`crates/nexum-runtime/src/syscalls.rs:16-18`, `:171-180`, `:202-205`).

A user block in a Nexum function has the same problem: the function may have
no frame, and the kernel's `syscall` wants a 16-byte aligned stack. The
compiler's rule:

* **Every function that contains a block gets a stack frame.** The codegen
  emits a 16-byte `alloca` in the prologue, so LLVM cannot elide the frame,
  and the block is entered with the stack 16-byte aligned on the targets
  whose ABI requires it. Measured: `%rsp & 15 == 0` with a frame, `8`
  without.
* **A block that changes the stack pointer declares `alignstack`.** LLVM's
  `alignstack` flag tells it that the block's stack effect is unbalanced; it
  then keeps the stack aligned around the block (it adds a push/pop pair,
  measured). A block that aligns the stack and restores it - the system call
  layer's own shape - is balanced and needs no flag.
* **The frame pointer and the stack pointer are not clobbers** (T058). On
  x86-64 that is `rsp` and `rbp`; on AArch64 it is `sp`, `x29` (the frame
  pointer) and `x30` (the link register).

### 8. The grammar and the AST

`docs/language-reference/grammar.md` changes (the orchestrator owns the
file; the content is specified here):

* §1 Keywords: add `asm`, `unsafe`, `out`, `clobber`, `alignstack`,
  `target`. `in` already exists.
* §3 Syntactic grammar: add the `asm-stmt` production above, and
  `asm-stmt` to the `statement` alternation.
* §5 Diagnostic codes: add the parser codes P016-P023 and the semantic
  codes T050-T062 (listed below).
* The grammar document's "Accepted-but-unspecified" section (§7) is where
  the deferred items go: memory operands, immediate operands, labels,
  `Float` operands and per-target variants.

The AST (`crates/nexum-parser/src/lib.rs`):

```rust
/// The kind of location an inline-assembly operand lives in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsmOperandKind {
    /// A general register the register allocator chooses.
    Reg,
    /// A physical register, pinned: the value is in this register.
    Register(String),
}

/// One operand of an inline-assembly block.
#[derive(Debug, Clone)]
pub struct AsmOperand {
    /// The name the template refers to the operand by (`$name`).
    pub name: String,
    /// Where the operand lives.
    pub kind: AsmOperandKind,
    /// The operand's value: an expression for an input, the destination
    /// variable for an output.
    pub value: Expr,
    pub span: Span,
}

/// An inline-assembly block.
#[derive(Debug, Clone)]
pub struct AsmBlock {
    /// The template, as written.
    pub template: String,
    /// The inputs, in declaration order.
    pub inputs: Vec<AsmOperand>,
    /// The outputs, in declaration order.
    pub outputs: Vec<AsmOperand>,
    /// The registers the block destroys, by name.
    pub clobbers: Vec<String>,
    /// The targets the block is written for; empty means "any target".
    pub targets: Vec<String>,
    /// The block moves the stack pointer without restoring it.
    pub align_stack: bool,
    /// The block is marked `unsafe`.
    pub unsafe_marked: bool,
    pub span: Span,
}
```

and one new statement kind:

```rust
pub enum StmtKind {
    // ... the existing kinds ...
    Asm(AsmBlock),
}
```

The parser's `parse_asm` builds one of these from the token stream and
attaches the span of the whole statement.

### 9. The sema

`crates/nexum-sema/src/lib.rs` gains:

```rust
pub enum HirStmt {
    // ... the existing kinds ...
    Asm {
        /// The template, already rewritten to LLVM's positional form
        /// (`$0`, `$1`, ...; `$$` stays `$$`).
        template: String,
        /// The outputs, in declaration order. `constraint` is the operand's
        /// LLVM constraint (`={rax}`, `=r`).
        outputs: Vec<HirAsmOperand>,
        /// The inputs, in declaration order.
        inputs: Vec<HirAsmOperand>,
        /// The clobbers, as LLVM constraints (`~{rcx}`).
        clobbers: Vec<String>,
        /// The whole constraint string, already assembled: the outputs, then
        /// the inputs, then the clobbers, then `~{memory}`.
        constraints: String,
        /// The block moves the stack pointer without restoring it.
        align_stack: bool,
    },
}

pub struct HirAsmOperand {
    /// The name the template refers to the operand by.
    pub name: String,
    /// The operand's LLVM constraint (`={rax}`, `=r`, `{rdi}`).
    pub llvm_constraint: String,
    /// The operand's type.
    pub ty: Ty,
    /// For an input: the value. For an output: the destination variable's
    /// name (the MIR resolves the slot).
    pub value: HirExpr,
}
```

The constraint syntax is LLVM's, but it is built here rather than in the
codegen, because the sema is the only stage that has both the target's
register table and the resolved operand kinds, and because the *numbering* of
the template's operands (outputs first, then inputs) is fixed here - the
template rewrite depends on it. The codegen treats the constraint string as
an opaque string.

The sema's checks:

| Code | Condition |
| --- | --- |
| T050 | the block is not marked `unsafe` |
| T051 | the template refers to `$name` and no operand is called `name` |
| T052 | an operand of class `reg` is never referred to in the template |
| T053 | an operand's type is not `Int` |
| T054 | an output's type is a managed type (`Str`, `List`) |
| T055 | an output's destination is not a whole `let mut` binding |
| T056 | a register name is not a register of the compilation target |
| T057 | a pinned register's class does not match the operand's type |
| T058 | a clobber is a reserved register (`rsp`, `rbp`, `sp`, `x29`, `x30`) |
| T059 | the target guard does not include the compilation target |
| T060 | the template contains a `$` that introduces no operand |
| T061 | two operands have the same name |
| T062 | the template is empty |

Notes on the harder ones:

* **T052** applies only to a `reg` operand. A *pinned* operand need not be
  referred to: the system call style is `in (a: rdi) fd` with the template
  `"syscall"`, where the instruction reads the register implicitly. A `reg`
  operand that is never referred to is always a mistake, because the
  register the allocator picks is unknown to the text.
* **T055** mirrors T037: a `mut` argument must be a mutable variable, so an
  output's destination must be a whole `let mut` binding, and it must be
  mutable.
* **T056/T057/T058** need the target's register table, so the sema needs the
  target. `nexum-target` gains a module `asm` with the per-architecture
  table:

  ```rust
  /// The class of a register: integer or floating point.
  pub enum RegClass { Int, Float }

  /// One register of a target.
  pub struct RegInfo {
      pub name: &'static str,
      pub class: RegClass,
      /// The register cannot be clobbered (stack pointer, frame pointer,
      /// link register).
      pub reserved: bool,
  }

  /// The registers of an architecture, by name.
  pub fn registers(arch: &str) -> &'static [RegInfo];

  /// The LLVM constraint letter for a class, per architecture.
  pub fn class_letter(arch: &str, class: RegClass) -> &'static str;
  ```

  The x86-64 table is the sixteen general registers
  (`rax rbx rcx rdx rsi rdi rbp rsp r8 r9 r10 r11 r12 r13 r14 r15`) with
  `rsp` and `rbp` reserved; the AArch64 table is `x0`-`x30` with `x29` and
  `x30` reserved. `class_letter` returns `r` for `Int` on both. (The
  floating-point class is in the table for phase 11c, but no phase-11b
  operand may use it.)

**The sema needs the target.** `nexum-sema` gains a dependency on
`nexum-target`, and the entry point becomes:

```rust
pub fn analyze(am: &AModule) -> SemaResult;                     // unchanged
pub fn analyze_for(am: &AModule, target: &TargetSpec) -> SemaResult;
```

`analyze` stays as `analyze_for(am, &TargetSpec::host())`, so the existing
callers (`crates/nexum-driver/src/main.rs:1056`,
`crates/nexum-driver/tests/common/mod.rs:29`) keep working; the driver
should call `analyze_for` with the real target, so a cross-target build
checks the guard. The sema agent also owns the driver change.

### 10. The MIR and the codegen

`crates/nexum-mir/src/lib.rs` gains one instruction:

```rust
Asm {
    /// The template, already rewritten to LLVM's positional form.
    template: String,
    /// The LLVM constraint string.
    constraints: String,
    /// The input values, in the order the constraint string lists them.
    inputs: Vec<Value>,
    /// The output destinations: the slot to store the result into, and the
    /// type of the value.
    outputs: Vec<(Value, Ty)>,
    /// The block moves the stack pointer without restoring it.
    align_stack: bool,
}
```

The lowering of `HirStmt::Asm`:

1. For each input, `lower_expr` the value (a variable read becomes a `Load`
   from the binding's slot, as everywhere else).
2. For each output, resolve the destination's slot from the environment
   (`ctx.env[name]`) and take the type from the HIR.
3. Emit the instruction with the template and the constraint string the sema
   produced.

`crates/nexum-codegen/src/lib.rs` gains the emission:

* **The constraint string** is the one the sema assembled; the codegen emits
  it verbatim.
* **The template** gets its newlines replaced: `\n` becomes `\0A` (LLVM's
  inline-assembly template does not interpret `\n`).
* **The call**:

  ```
  %vN = call <ty> asm sideeffect "<template>", "<constraints>"(<inputs>)
  ```
  where `<ty>` is `void` with no output, the output's type with one output,
  and `{T0, T1, ...}` with several. `alignstack` goes between `sideeffect`
  and the template when the block declares it.
* **The outputs**: with one output, `store <ty> %vN, ptr <slot>`; with
  several, `extractvalue` each field and store it. Every output must be
  stored - LLVM rejects an output with no user.
* **The frame anchor**: the pre-scan sets a flag when the function contains
  an `Instr::Asm`, and the prologue then emits a 16-byte `alloca`.

The codegen needs the target only for the frame anchor's alignment
assumption; the constraint string and the operand numbering are already
settled by the sema.

### 11. The three changes, side by side

The three agents can work in parallel; the table is the contract.

| | **Parser agent** | **Sema agent** | **Codegen agent** |
| --- | --- | --- | --- |
| Crates | `nexum-lexer`, `nexum-parser` | `nexum-target` (new `asm` module), `nexum-sema`, `nexum-driver` (the `analyze_for` call, the `nx explain` entries) | `nexum-mir`, `nexum-codegen` |
| New tokens | `KwAsm`, `KwUnsafe`, `KwOut`, `KwClobber`, `KwAlignStack`, `KwTarget` | - | - |
| New AST | `AsmBlock`, `AsmOperand`, `AsmOperandKind`, `StmtKind::Asm` | - | - |
| New HIR | - | `HirStmt::Asm`, `HirAsmOperand` | - |
| New MIR | - | - | `Instr::Asm` |
| New target data | - | `RegInfo`, `RegClass`, `registers()`, `class_letter()` | - |
| Entry point | `parse` | `analyze_for(am, target)` | `emit(mir, target)` |
| Produces | the template text, the operand lists, the clobbers, the target list | the **rewritten** template, the per-operand **constraint**, the **constraint string**, the resolved types | the LLVM call, the stores, the frame anchor |
| Diagnostics | P016-P023 | T050-T062 | none (the codegen has no diagnostic channel) |

The three interfaces that must not drift:

* **Parser -> sema**: the AST above. The sema reads `AsmBlock` and never
  looks at tokens.
* **Sema -> MIR**: the HIR above. The template the sema produces is already
  in LLVM's positional form, so the MIR never has to know the operand
  numbering.
* **MIR -> codegen**: the MIR instruction above. The codegen never has to
  know the operand names.

### 12. The diagnostics

Parser codes:

| Code | Meaning |
| --- | --- |
| P016 | expected an assembly template string after `asm` |
| P017 | expected an operand name |
| P018 | expected `:` and an operand class |
| P019 | expected `)` after an assembly operand |
| P020 | expected a register name in the clobber list |
| P021 | expected `)` after the clobber list |
| P022 | expected a target name after `target` |
| P023 | unexpected token in an assembly block |

Semantic codes: T050-T062, as in the table above. The grammar document's
diagnostic table (§5) and `nx explain`
(`crates/nexum-driver/src/main.rs:947-990`) both need the new entries; the
`nx explain` change belongs to the sema agent, since it is the same list.

### 13. The tests

* **Parser**: unit tests in `crates/nexum-parser/src/lib.rs` for the
  statement form, the operand forms, the clobber list, the target list and
  each parse error.
* **Sema**: a `pass` fixture and one `fail` fixture per semantic code, in
  `crates/nexum-driver/tests/fixtures/{pass,fail}/`, each with an
  `# EXPECT: Txxx` comment (the harness at
  `crates/nexum-driver/tests/fixtures.rs:41-104` checks the codes and that
  every diagnostic carries a span).
* **Codegen**: a test that the emitted IR contains the expected constraint
  string and the `\0A` escaping, in the style of the runtime's own tests
  (`crates/nexum-runtime/src/syscalls.rs:294-384`).
* **End to end**: a `run` fixture
  (`crates/nexum-driver/tests/fixtures/run/`) that does something the
  machine can be asked to do - the cheapest is reading the timestamp
  counter, or a `cpuid` leaf - and prints a value that is stable enough to
  compare. A fixture that checks the *stack alignment* is the important one:
  a block that computes `%rsp & 15` and prints it must print `0`.
* **Cross-target**: the `target` guard is checked by compiling the same
  program for two targets and expecting T059 for the one that does not
  match. `verify_ir_for` already assembles for the target the module claims
  (`crates/nexum-toolchain/src/lib.rs:221-248`), so a block whose text is
  for a foreign instruction set fails at the right target.

## Risks

* **The `$` convention is the sharpest edge.** The template uses `$name` for
  an operand and `$$` for a literal dollar, which is the Rust convention and
  the runtime layer's convention. A user who writes `$0` gets T060, which is
  the point. But a user who writes `movq $1, %rax` meaning "the immediate 1"
  gets T060 as well, with a message that says to write `$$1`. The message
  must be explicit, because the failure the runtime layer documents is
  exactly this one.
* **A template that spans several lines puts the options on its last line.**
  That reads badly. The indented block form fixes it and is a parser-only
  change, but it is a second grammar, so phase 11b should ship the one-line
  form and add the block form when a real program wants it.
* **The `unsafe` keyword is introduced before phase 11d has designed it.**
  If phase 11d wants `unsafe` to mean something else - an unsafe *block*,
  an unsafe *function*, a trait - the marker in front of `asm` still fits,
  but the orchestrator should decide now that `unsafe` is a keyword and that
  11b's use is its narrowest form. If the orchestrator prefers to wait, the
  alternative is a compiler flag, which is worse.
* **Forcing a stack frame in every function with a block costs a frame.** It
  is a few bytes and one `sub`; the price is worth the alignment guarantee.
  It is also a change in the generated code for functions that today have
  none, which the benchmark suite may notice.
* **`~{memory}` on every block is conservative.** A block that only does
  arithmetic cannot have its memory accesses reordered across it. That is
  the price of not having to remember to declare a memory clobber, and it is
  the same choice the runtime layer makes.
* **The "no immediate operands" rule will be questioned.** A user will want
  `in (n: imm) 5`. The answer is measured: LLVM's immediate substitution is
  not portable between x86-64 and AArch64, and the `n` constraint means
  different things on the two. If phase 11c or 11f wants immediates back,
  they should be added as a *target-specific* class with a documented
  difference, not as a portable one.
* **The `Float` operand is deferred**, so a program cannot move a double
  through a register directly in phase 11b. It can still reach the
  floating-point registers through the text (`fmov x0, d0` and back), and
  phase 11c will settle the types.
* **The recommendation is wrong if the target's inline-assembly dialect is
  not the one assumed.** The measurements were taken with clang 23.1.3 on
  x86-64 and AArch64 targets, and the runtime layer already depends on the
  same behaviour (AT&T on x86-64, the AArch64 syntax on AArch64). If the
  toolchain ever moves to `inteldialect`, the template dialect changes but
  none of the structure does.
* **The recommendation is wrong if the sema cannot be given the target.**
  Every target-dependent check (T056, T057, T058, T059) needs it. If the
  orchestrator prefers to keep the sema target-independent, those four
  checks have to move to the codegen, and the codegen has to grow a
  diagnostic channel - a much larger change than a parameter.

## Open questions

1. **Should the target guard be an assertion (T059) or a filter (the block is
   omitted on the other targets)?**
   *Recommendation:* an assertion in phase 11b. A filter changes the meaning
   of a program silently, and Nexum has no conditional compilation for the
   user to fall back on. When a general `when target ...` construct lands,
   the guard should become a filter, and T059 should go away.

2. **Should a `volatile` keyword be spelled on a block?**
   *Recommendation:* no. Every block is `sideeffect`, so there is nothing to
   turn off, and the word `volatile` is reserved for phase 11d's
   memory-access qualifier, which is a different concept. If the
   orchestrator wants the explicitness, accept `volatile` as a no-op
   keyword - it costs one token and one parse rule, and it keeps the
   surface stable when a `pure` form is added later.

3. **Should the block form (`unsafe asm:` with an indented list of options)
   ship in phase 11b?**
   *Recommendation:* no, ship the one-line form, and add the block form as a
   parser-only follow-up. The block form is the right long-term shape - the
   language is off-side based and a multi-instruction body is the common
   case - but it is a second grammar and the one-line form is enough to
   unblock the runtime.

4. **Should `unsafe` be introduced now, or should the block be gated by a
   compiler flag until phase 11d?**
   *Recommendation:* introduce `unsafe` now, in its narrowest form. A flag
   is invisible at the call site, is not part of the language, and would have
   to be replaced by `unsafe` later anyway - which would then be a breaking
   change for every program that used the flag.

5. **Should the `unsafe` marker be required, or should a "safe" subset
   exist (a block that only names registers it declares)?**
   *Recommendation:* required, and no safe subset in phase 11b. The
   compiler cannot check the text, so a "safe" subset would be a promise it
   cannot keep. A future `safe asm` could be defined as a block that
   declares every register it touches and touches no memory - but that
   requires the compiler to parse the assembly, which is a much larger
   project (and is what a real assembler-integrated checker would do).

6. **Should the runtime's system call layer be rewritten in Nexum once this
   lands?**
   *Recommendation:* yes, as a separate task, and it is the proof that the
   feature is worth having. `crates/nexum-runtime/src/syscalls.rs` currently
   emits LLVM IR by hand; with inline assembly in the language, the same
   wrappers are a Nexum source file, and the hand-written IR - with its
   three silent-failure details - goes away. It also removes the
   hand-rolled stack alignment from the runtime, because the compiler's
   frame anchor does it.

7. **Should the numeric tower's `f32`/`f64` (phase 11c) extend the operand
   types, and should the SIMD work (11f) add vector operands?**
   *Recommendation:* yes, in that order, and each with its own measured
   constraint letters. The AArch64 floating-point naming (LLVM prints `v0`
   for a pinned `d0`) has to be settled before a `Float` operand can be
   offered there; the vector class is a different question again.

8. **Should a block be allowed inside an expression once the language has
   tuples?**
   *Recommendation:* not in phase 11b, and probably not later either: a
   block with a side effect reads better as a statement, and the statement
   form already covers several outputs. If it is added later, it should be
   sugar for a single output written into a fresh variable.
