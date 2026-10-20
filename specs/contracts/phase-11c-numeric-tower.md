# The contract for wave B: the numeric tower (11c)

Status: **ready to dispatch**. This follows
`specs/research/numeric-tower.md` and `specs/research/f32-touchpoints.md`,
and it settles the two questions those documents left open.

## What the tower is

Ten types, and a width is a type:

| Type | Width | Notes |
| ---- | ----- | ----- |
| `F32`, `F64` | 32 and 64 bits | IEEE 754 binary |
| `I8`, `I16`, `I32`, `I64` | 8, 16, 32, 64 bits | two's complement, wrapping |
| `U8`, `U16`, `U32`, `U64` | 8, 16, 32, 64 bits | wrapping |

`Int` is `I64` and `Float` is `F64`, both kept as the names the language
has always used. A width is a distinct type because a width that is not a
type cannot be a field of a packed record, and phase 11d needs that.

**There are no implicit conversions.** A conversion is explicit, through a
builtin: `i32(x)`, `u8(x)`, `f64(x)`, `f32(x)`, and so on. Each one
has a `to_` form that traps - `to_i32_checked` - for the conversions
that can lose information.

**Integers wrap.** Two's complement, like Rust, with no trap. The
saturating and checked families are separate builtins.

**Division truncates toward zero** and the remainder follows the
dividend, which is what LLVM's `sdiv` and `srem` do, so it costs
nothing.

**One rounding mode**, round to nearest, and it is not programmable.
IEEE floats never trap: a NaN propagates, an infinity is an infinity, and
the sign of a zero is kept.

**Comparisons follow IEEE**: `==` is false and `!=` is true when a NaN is
involved, and the orderings are false.

## The two questions the research left open

**The literal syntax: a suffix, or a conversion?** The numeric tower
research proposed `1.0f32` and `42i8`. The f32 research found the
problem: the normalised form and the self-hosted parser disagree in
principle about a literal's parsed value against its raw source text, so
a suffix changes what the differential gate compares, and the gate
cannot tell the two parsers apart any more.

**So there is no suffix.** A float literal is an `F64` and an integer
literal is an `I64`, and a narrower or wider type is reached with a
conversion builtin: `f32(1.0)`, `i8(42)`, `u32(x)`. The suffix can
come later, if it comes at all, and it would be a separate wave with the
normalised form settled first.

**The `print` of a float.** The output formatting is C's `%g` today
(D019), and the research on the number formatting settled that the
shortest round-trip form is a *different* format from `%g`. So the tower
does not change what `print` does: a `Float` prints as `%g`, and an
`F32` prints as `%g` of the value widened to `F64` - which is the same
as printing the `F32` exactly, because the widening is exact. A later
wave can add a `repr` function that prints the shortest form that reads
back as the same value; the research already named it.

## The order of the work

The slices are in this order, and each one is independently verifiable:

1. **The comparison defect.** Already fixed, but it is the first item
   because the rest stands on it: the code generator tested the *result*
   type where it needed the *operand* type, so the float case never
   fired and every float comparison emitted an integer comparison on
   doubles. There is a fixture for it.
2. **The type and the code generator's mapping together.** The type enum
   in the sema, and the four tables in the code generator - `llvm_ty`,
   `llvm_align`, `llvm_size`, `has_refs` - plus the scan length. These
   are in different crates, so two agents can work on them at once, and
   the wave does not compile until both have landed.
3. **The MIR constant.** A constant of the new types, so a program can
   write one.
4. **The conversions.** The `i32(x)` family, in the sema.
5. **The fixtures and the documentation.**

The literal syntax is not in this wave, and neither is the vector type
that needs the widths.

## The interfaces, pinned

The two agents of the wave work on different crates and never wait for
each other.

### The type, in `crates/nexum-sema/src/lib.rs`

```rust
pub enum Ty {
    // ... the existing variants
    F32, F64,
    I8, I16, I32, I64,
    U8, U16, U32, U64,
}
```

`Int` and `Float` stay as they are - they are `Ty::I64` and `Ty::F64`
under other names - so nothing that matches on them changes.

### The code generator's tables, in `crates/nexum-codegen/src/lib.rs`

| Type | LLVM | Align | Size | `has_refs` | Scan |
| ---- | ---- | ----- | ---- | ---------- | ---- |
| `F32` | `float` | 4 | 4 | no | - |
| `F64` | `double` | 8 | 8 | no | - |
| `I8`, `U8` | `i8` | 1 | 1 | no | - |
| `I16`, `U16` | `i16` | 2 | 2 | no | - |
| `I32`, `U32` | `i32` | 4 | 4 | no | - |
| `I64`, `U64` | `i64` | 8 | 8 | no | - |

A numeric value holds no reference, so it is never scanned - and that is
the rule for any type added later as well. A missing arm in `llvm_size` is
a memory-safety bug and not a wrong answer: the size is the stride of a
list of that type and the length of a copy of it.

### The conversions, in `crates/nexum-sema/src/lib.rs`

The builtins are named after the type they produce: `i8`, `i16`, `i32`,
`u8`, `u16`, `u32`, `f32`, `f64`, and `i64` for the signed family. Each
takes one argument of a numeric type. The checked forms are
`to_i8_checked` and so on, and they return a value that says whether the
conversion fitted - the language has no `Option` yet, so the checked form
is a panic on failure in this wave, and a result type in a later one.

The existing `Int` and `Float` names are the `i64` and `f64` conversions
with the old behaviour: an integer literal that does not fit `i64` is a
compile-time error rather than a silent zero, and the most negative `i64`
can be written.

## Who owns what

| Agent | Owns |
| ----- | ---- |
| the types | `crates/nexum-sema/src/lib.rs`, `crates/nexum-mir/src/lib.rs` |
| the code generator | `crates/nexum-codegen/src/lib.rs` |

The tests: each agent writes its own fixtures, and neither writes a file
the other owns. The fixtures go under
`crates/nexum-driver/tests/fixtures/`, in the `run` directory for a
program that runs and the `fail` directory for a program that must not
compile.

## What each agent has to show

* The whole suite green, on Windows, Linux and macOS.
* Clippy clean - no warnings at all.
* A program that prints each of the ten types, and the values are right.
* A program that adds two `I32`s and wraps, and one that divides a
  negative number, and the answers are the ones the contract says.
* A program that converts a value that does not fit, and it is a compile
  time error rather than a wrong number.
* An honest report: what was verified, and what was not.

## The rules

* No implicit conversions, and no widening by accident.
* The runtime is freestanding: no C library, and no 128-bit division.
* No new dependency.
* Do not weaken a test to make a change pass.
* Do not commit, push or merge. Open a pull request and let the
  orchestrator merge it.
