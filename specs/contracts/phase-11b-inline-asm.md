# The contract for wave A: inline assembly (11b)

Status: **ready to dispatch**. This is the contract the four agents of
wave A work from. It follows the research in
`specs/research/inline-asm-design.md` and
`specs/research/syntax-evolution.md`, and it settles the one thing those
two disagree about.

## The decision the orchestrator made

The two research documents disagree on the shape of the feature.

* The inline-assembly research settled a **statement**: `unsafe asm
  "<template>" in (label: rdi) value out (label: rax) dest clobber (rcx, r11)`.
* The syntax research found that the whole of 11b to 11f needs about two
  reserved words, one token and ten AST nodes, and that a **builtin** needs
  no grammar change at all - and therefore no change to the self-hosted
  parser.

The syntax research wins, because a grammar change is made four times: the
Rust lexer, the Rust parser, the normalised form, and the self-hosted
lexer and parser. The statement form costs about seventy lines in each of
them; the builtin form costs none.

**So `asm` is a builtin.** It is a call like any other, and the sema
decides what the name means.

The feature is still complete: the operands are named, the clobbers are
declared, the target guard is there, and the output binds to a variable.
The shape is a *call* rather than a *statement*, so the two pieces that a
statement would have given for free - the destination of an output, and
the marker that says the block is unsafe - are carried in the arguments.

## The surface syntax

```
let mut out = 0
asm(out, "mov $value, %rax", "=r,r", 42)
```

The arguments, in order:

| argument | type | meaning |
| -------- | ---- | ------- |
| 0 | a mutable variable of an integer type | where the result of the block goes |
| 1 | `Str` | the assembly template |
| 2 | `Str` | the constraint string, LLVM's own |
| 3.. | values | the inputs, in the order the constraint string names them |

The template's operands are **named**, never numbered: `$name` refers to
the operand called `name`, and the compiler rewrites the names to the
positional form LLVM wants. A literal dollar is written twice.

The constraint string is the user's, and it must name the operands in
LLVM's order: the outputs first, then the inputs, then the clobbers. The
result of the block is the value of the first output, and the destination
is the variable in argument zero. So the number of constraints before the
first `~{` is one more than the number of inputs: the destination is the
first output.

The target guard is **the last argument when it is a string literal**,
and the inputs are the integers before it. The inputs are integers, so
there is no other way to read the argument list.

**The safety marker.** The builtin is only accepted inside an `unsafe`
region, and phase 11d introduces the region. Until then, the builtin is
accepted everywhere and the *diagnostic* says that the feature is
unrestricted. The agents do not add a keyword: the syntax research showed
that a reserved word costs a source break, and the phase that needs it -
11d - is the phase that introduces it.

**The target guard.** A fifth argument, a `Str`, names the target the
block is for. When the program is compiled for another target, the
compiler reports an error. The guard is an assertion, not a filter: the
feature is not silently dropped.

## The interfaces, pinned

The four agents work on different files and never wait for each other.
These are the shapes they agree on.

### The AST, in `crates/nexum-parser/src/lib.rs`

```rust
StmtKind::Asm {
    dest: Expr,          // the variable the result goes to
    template: String,    // the assembly text
    constraints: String, // the constraint string
    inputs: Vec<Expr>,   // the values, in order
    target: Option<String>,
}
```

### The normalised form, in `crates/nexum-parser/src/normalize.rs`

The self-hosted parser prints the same lines, so the form is part of the
contract between the two parsers:

```
asm
  asm-dest <the destination expression>
  asm-template <the template>
  asm-constraints <the constraints>
  asm-input <the input expression>
  asm-target <the target>          (only when there is one)
```

### The MIR, in `crates/nexum-mir/src/lib.rs`

```rust
Instr::Asm {
    dest: Value,          // where the result goes
    template: String,
    constraints: String,
    inputs: Vec<Value>,
    ty: Ty,
}
```

The target guard is a **compile-time** check, so it does not reach the
MIR: the sema reports the diagnostic and lowers the block to a panic, the
way the other builtins report a type error.

### The sema, in `crates/nexum-sema/src/lib.rs`

The builtin is recognised in the builtin dispatch, beside `read_file` and
`write_file`, and it checks:

* argument zero is a variable of an integer type - the destination;
* argument one and two are `Str`;
* the remaining arguments are integers - the inputs;
* the argument count matches the number of operands the constraint string
  names, which the sema counts by counting the constraints before the
  first `~{`;
* the target guard, when there is one, names the target the program is
  being compiled for.

A failure is a diagnostic with its own code, `T051`. The existing codes
run to `T050`, which the byte primitives took.

### The code generator, in `crates/nexum-codegen/src/lib.rs`

The lowering is one LLVM instruction:

```llvm
%r = call i64 asm sideeffect "<template>", "<constraints>"(<operands>)
```

with the operands pinned to the registers the constraint string names,
the template's `$name` references rewritten to the positional form, and
the result stored to the destination.

The facts the research measured, which the implementer needs:

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

## Who owns what

| Agent | Owns |
| ----- | ---- |
| the Rust front end | `crates/nexum-parser/src/lib.rs`, `crates/nexum-parser/src/normalize.rs` |
| the self-hosted front end | `examples/selfhost/parser.nx` |
| the sema | `crates/nexum-sema/src/lib.rs`, `crates/nexum-mir/src/lib.rs` |
| the code generator | `crates/nexum-codegen/src/lib.rs` |

The tests: the Rust front end's agent adds a parse fixture; the sema's
agent adds the diagnostic fixtures; the code generator's agent adds the
end-to-end test. Each test file is owned by the agent that writes it, and
no two agents write the same file.

The self-hosted parser's agent must not touch the Rust parser, and the
Rust parser's agent must not touch the self-hosted parser: the normalised
form above is what makes them agree, and it is written down here so that
neither has to ask.

## What each agent has to show

* The whole suite green, on Windows, Linux and macOS.
* Clippy clean - no warnings at all.
* The self-hosted parser and the Rust parser produce the same normalised
  form for the new fixture, which the differential gate checks.
* A program that uses `asm` produces the right value, and the test says
  what the right value is.
* An honest report: what was verified, and what was not.

## The rules

* The smallest thing that works. No keyword, no region, no immediate
  operands, no floating-point operands.
* The runtime is freestanding: the assembly must not call the C library.
* No new dependency.
* Do not weaken a test to make a change pass. If a test fails because of
  your change, fix the change.
* Do not commit, push or merge. Open a pull request and let the
  orchestrator merge it.
