# The numeric tower and the semantics of its operations

**Question.** What is the set of numeric types Nexum will have after phase 11c,
and what exactly does every operation on them do - the representation, the
overflow rule, the division-by-zero rule, the rounding mode, the NaN and
infinity rules, signed zero, the literal typing rules, the conversion rules,
the comparison rules, the integer division and remainder rules for negative
values, what the runtime has to implement because LLVM cannot, and how the
whole thing sits with the collector and the ABI?

**Summary.** The recommendation is a tower of **ten numeric types** -
`F32`, `F64`, `I8`, `I16`, `I32`, `I64`, `U8`, `U16`, `U32`, `U64` - in which
**every width is a distinct type**, `Int` stays as the name of `I64` and
`Float` stays as the name of `F64`, and **there are no implicit conversions at
all**: every conversion is an explicit `to_*` builtin, and every unsuffixed
literal takes its type from the context it appears in. Integer arithmetic
**wraps** by default (with explicit `*_sat` and `*_checked` families), integer
division **truncates toward zero** and the remainder **takes the sign of the
dividend** - the rule LLVM's `sdiv`/`srem` already have, so it costs nothing -
while float arithmetic is **plain IEEE 754 binary32/binary64 in one rounding
mode (round-to-nearest, ties-to-even) that is not programmable**, never traps,
propagates NaN, and preserves signed zero. Only a small, explicit set of
operations has no LLVM counterpart and must be written by us: the elementary
functions (`exp`, `log`, `pow`, the trigonometric family), the rounding
functions (`floor`, `ceil`, `trunc`, `round`), `frexp`/`ldexp`, and the
decimal formatter - everything else lowers to an LLVM instruction, a compiler
intrinsic, or a few instructions of compiler-generated IR. Numerics hold no
references, so the collector does not scan them, and an aggregate of pure
numerics is not scanned at all, which is what makes the `f32` path of phase 11i
cheap. The decision the orchestrator actually has to make is the **size of the
tower**: the recommendation is the full uniform lattice, because the rules in
this document are about the *lattice* and not about the members, and a partial
tower re-opens every rule in it later.

## Recommendation

The numbered decisions, then the decision-ready tables. Every "**Contested**"
marker is a place where two reasonable languages disagree; each one carries a
recommendation and the reason.

1. **The tower is ten types: `F32`, `F64`, `I8`, `I16`, `I32`, `I64`, `U8`,
   `U16`, `U32`, `U64`.** `Int` is a synonym of `I64` and `Float` is a
   synonym of `F64`; both names stay valid forever, so every existing program,
   fixture and document keeps its meaning. `Bool`, `Str`, `Unit` and `Never`
   are not part of the tower.
   *Reason.* The rules below (ranges, conversions, literal typing, the
   overflow families, the ABI) are stated over the whole lattice. Stating them
   once over a closed lattice is smaller than stating them over an irregular
   subset and then re-opening them when the subset grows.
   *This is the size of the tower, not the schedule.* Which members land in
   11c and which arrive with the layout work in 11d is a separate question
   (open question 1); the rules are the same either way.
2. **Widths are distinct types, not literal decorations.** The literal
   `42u8` has the type `U8`, and `U8` is a first-class type: it can be a
   record field, an array element, a parameter type and a `List` element
   type. It is not "an `Int` that happens to be narrow".
   *Reason.* A width that is not a type cannot be a record field, cannot be
   an array element, cannot be a syscall argument with an 8-bit slot, and
   cannot be a `List` element type. Phase 11d ("memory and layout: pointers,
   packed records") cannot be expressed without width-exact types. The
   "one `Int` with widths as literals" option is C's `int` problem: the width
   would be a property of the *literal* and not of the *value*.
   **Contested.** A smaller tower (`F32 F64 I32 I64`, or
   `F32 F64 I32 I64 U8`) is cheaper to implement. **Recommend the full
   lattice**, because the implementation cost per member is a table lookup
   once the rules are fixed, and 11d will need every member.
3. **Integer arithmetic wraps by default.** `+`, `-`, `*`, unary `-`, `<<` on
   integers wrap in two's complement. No overflow flag, no trap, no
   promotion. `MIN / -1` is `MIN`; `MIN % -1` is `0`; `-MIN` is `MIN`.
   *Reason.* D004 already decided this for `Int` and it is what the code
   generates today. A language that owns the machine must be able to compute
   a hash, a checksum or a pointer offset without a trap in the middle.
4. **The overflow behaviour comes in three named families, all on integers:**
   `add_wrap`/`sub_wrap`/`mul_wrap` (identical to the operators, available for
   explicitness), `add_sat`/`sub_sat`/`mul_sat` (clamp to the type's bounds),
   and `add_checked`/`sub_checked`/`mul_checked` (panic
   `"integer overflow"`). Floats have no overflow families: float overflow is
   IEEE, and yields an infinity.
   *Reason.* The roadmap asks for "saturating/wrapping arithmetic"; D004
   leaves "checked (trapping) arithmetic" open and says it "can be added
   later as an opt-in profile without changing these rules". Pinning all three
   now means 11c adds two families and the third is only an implementation.
5. **Integer division truncates toward zero; the remainder takes the sign of
   the dividend.** `-7 / 3 == -2`, `-7 % 3 == -1`, and
   `a == (a / b) * b + (a % b)` holds exactly for every pair that does not
   trap.
   *Reason.* This is exactly LLVM's `sdiv`/`srem`/`udiv`/`urem`, so the
   operation lowers to one instruction with no fix-up branch. The floored
   rule (Python) costs a correction on every division. This is the
   *contested* choice: C, C++, Rust, Go, Java and Zig all truncate; Python,
   Lua and Haskell floor. **Recommend truncation**, because it is what the
   hardware does and because the alternative is a branch on every division in
   a language whose whole point is speed.
6. **Division by zero traps for integers, never for floats.** Integer `/`
   and `%` by zero call `panic("division by zero")` and exit 1, as today. A
   **constant** zero divisor is a compile-time error. Float `/` and `%` by
   zero follow IEEE: `x / 0.0` is `Â±inf` with the sign of `x`, `0.0 / 0.0` is
   NaN, `x % 0.0` is NaN.
   *Reason.* The integer rule is already implemented and specified (D004).
   Making a *constant* zero divisor an error removes a class of bug that
   today survives to run time.
7. **There is exactly one rounding mode: round-to-nearest, ties-to-even. It
   is not programmable.** Every float operation, every conversion to a float
   and every float literal uses it. The language has no way to select
   another mode, and there is no `set_rounding_mode`.
   *Reason.* A single mode is the smallest thing that is complete: dynamic
   rounding modes need the LLVM *constrained* floating-point intrinsics, a
   per-thread or per-block mode variable, and a documented interaction with
   the collector's safepoints. Nothing in the roadmap needs them.
8. **Float arithmetic is plain IEEE 754 and never traps.** Overflow to an
   infinity is not an error. NaN propagates through every arithmetic
   operation. Signed zero is preserved and observable: `-0.0` is a distinct
   value from `+0.0`, `1.0 / +0.0` is `+inf` and `1.0 / -0.0` is `-inf`.
   Subnormals are supported (gradual underflow) and flush-to-zero is never
   enabled implicitly.
   *Reason.* This is the current behaviour, verified below. It is also the
   behaviour an ML framework expects, and the "no FTZ" rule keeps results
   reproducible.
9. **Comparisons follow IEEE.** `==` is false if either operand is NaN;
   `!=` is true if either operand is NaN; `<`, `>`, `<=`, `>=` are false if
   either operand is NaN. Integer comparisons are signed for the signed types
   and unsigned for the unsigned types. Operands of different numeric types
   cannot be compared at all - there is no implicit conversion to make the
   comparison work.
   **Contested.** Some languages make `==` on NaN true; Rust's
   `f32::partial_cmp` returns `None` when either side is NaN, although its
   `==` operator is IEEE. **Recommend IEEE**, matching the `fcmp oeq` the
   code already emits for `==`.
10. **Literals take their type from the context, and only within their
    family.** An unsuffixed integer literal takes the context's integer type
    if the value fits in it, otherwise `I64`; an unsuffixed float literal
    takes the context's float type (`F32` or `F64`) if there is one,
    otherwise `F64`. A suffix (`42i8`, `42u32`, `1.0f32`, `1.0f64`) pins the
    type. An integer literal that does not fit in `I64`, or a literal with a
    suffix that does not fit in the suffixed type, is a **compile-time
    error**; a float literal that overflows the target precision is also a
    compile-time error.
    *Reason.* This is Rust's model, and it is the only model that gives
    `let x: I32 = 5` without giving `let x: I64 = some_i32`. The compiler
    must keep the literal's *text* until the type is resolved and parse it
    at the target precision; parsing to `f64` and converting afterwards
    double-rounds.
11. **There are no implicit numeric conversions.** Every conversion is an
    explicit builtin `to_i8`, `to_i16`, `to_i32`, `to_i64`, `to_u8`,
    `to_u16`, `to_u32`, `to_u64`, `to_f32`, `to_f64`, with Rust's `as`
    semantics, plus a `to_*_checked` twin of each that panics instead of
    losing information.
    **Contested.** C, C++, Java and Kotlin widen implicitly; Rust, Go, Swift
    and Zig do not. **Recommend no implicit conversion**, because D004
    already rejected it ("implicit widening hides type errors") and because
    with ten numeric types an implicit common-type lattice is a large part of
    the type checker and a large source of "why is this `I64`?".
    The one exception is *literal typing* (decision 10), which is not a
    conversion: a literal has no type until the context gives it one.
12. **Bitwise and shift operators are added for integers** (`&`, `|`, `^`,
    `~`, `<<`, `>>`) and are **not defined on floats**. `<<` shifts in
    zeros; `>>` is arithmetic (sign-filling) on the signed types and logical
    (zero-filling) on the unsigned types. A shift amount `>=` the width of
    the type is a **runtime panic** `"shift amount out of range"`; a constant
    shift amount `>=` the width is a compile-time error.
    *Reason.* A systems language cannot do device work without masks and
    shifts. LLVM's `shl`/`lshr`/`ashr` are *poison* for an out-of-range shift
    amount, so the guard has to be somewhere; a panic is cheaper to reason
    about than C's "undefined".
    **Contested.** Rust panics in debug builds and masks the shift amount in
    release builds; C masks and says "undefined". **Recommend the panic**,
    because the compiler always optimises, so a masked rule would behave
    differently from what the source says.
13. **The runtime implements only what LLVM cannot.** The elementary
    functions, the rounding functions, `frexp`/`ldexp`/`ilogb`/`modf`, and
    the decimal formatter are ours; `sqrt`, `fabs`, `copysign`, the
    bit-count intrinsics, the conversions, the saturating conversions and
    all the arithmetic are LLVM's. Saturating multiply has no LLVM
    intrinsic but is expressible in LLVM IR, so it is compiler-generated, not
    runtime code. `fma` is **not** provided: the x86-64 baseline has no FMA
    instruction, and a "may or may not be fused" contract is a weaker
    promise than the language can make anywhere else.
14. **Numerics are not scanned by the collector, and an aggregate of pure
    numerics is not scanned at all.** A `F32` local registers 0 scan bytes
    and is not a root; a record or array whose fields are all numeric is not
    spilled, not scanned and not registered; a `List[F32]`'s element buffer
    carries 0 scan bytes. This is the existing rule and the new widths do
    not change it - but they make it *matter*, because a `f32` tensor is the
    case where "not scanned" is the whole point.
15. **The ABI gains the widths.** `F32` is `float` (4 bytes, align 4), the
    integer widths are `i8`/`i16`/`i32`/`i64` with their natural sizes and
    alignments, `Bool` stays `i1`, and `Int`/`Float` keep `i64`/`double`.
    Integer arguments are widened to 64 bits at the syscall boundary -
    zero-extended for the unsigned types, sign-extended for the signed ones -
    exactly as `Bool` is today.

### The type set (decision-ready)

| Type | Representation | LLVM type | Size / align | Range | Default? |
| ---- | -------------- | ---------- | ------------ | ----- | -------- |
| `F32` | IEEE 754 binary32 | `float` | 4 / 4 | Â±3.4028235e38; min subnormal 1.4012985e-45 | no |
| `F64` | IEEE 754 binary64 | `double` | 8 / 8 | Â±1.7976931348623157e308; min subnormal 4.9406565e-324 | **yes** (also spelled `Float`) |
| `I8` | two's complement | `i8` | 1 / 1 | -128 â€¦ 127 | no |
| `I16` | two's complement | `i16` | 2 / 2 | -32768 â€¦ 32767 | no |
| `I32` | two's complement | `i32` | 4 / 4 | -2147483648 â€¦ 2147483647 | no |
| `I64` | two's complement | `i64` | 8 / 8 | -9223372036854775808 â€¦ 9223372036854775807 | **yes** (also spelled `Int`) |
| `U8` | two's complement | `i8` | 1 / 1 | 0 â€¦ 255 | no |
| `U16` | two's complement | `i16` | 2 / 2 | 0 â€¦ 65535 | no |
| `U32` | two's complement | `i32` | 4 / 4 | 0 â€¦ 4294967295 | no |
| `U64` | two's complement | `i64` | 8 / 8 | 0 â€¦ 18446744073709551615 | no |

`Bool` is `i1` and is **not** numeric; `to_i32(true)` is `1` and
`to_i64(false)` is `0`, but `true + true` is an error.

### The operations, per kind of type

| Operation | Integers (every width, signed and unsigned) | Floats (`F32`, `F64`) |
| --------- | ----------------------------------------- | --------------------- |
| `a + b` | wrapping, two's complement | IEEE, RNE; no trap; overflow to `Â±inf` |
| `a - b` | wrapping, two's complement | IEEE, RNE; no trap |
| `a * b` | wrapping, two's complement | IEEE, RNE; no trap |
| `a / b` | **truncating toward zero**; `b == 0` panics `"division by zero"`; `MIN / -1` is `MIN` | IEEE; `a / Â±0.0` is `Â±inf` by the sign of `a`, `0.0 / 0.0` is NaN; never traps |
| `a % b` | sign of the **dividend**; `b == 0` panics; `MIN % -1` is `0`; `a == (a/b)*b + (a%b)` holds exactly | IEEE remainder (`frem`), exact, sign of the dividend; `a % 0.0` and `inf % x` are NaN |
| `-a` | wrapping negate: `-MIN` is `MIN` | IEEE `fneg`: `-0.0` is `+0.0`, `NaN` is `NaN` |
| `a == b` | bitwise equality | IEEE **ordered** equal: **false** if either is NaN |
| `a != b` | bitwise inequality | IEEE **unordered** not-equal: **true** if either is NaN |
| `a < b`, `a > b`, `a <= b`, `a >= b` | signed compare (`slt`/`sgt`/`sle`/`sge`) for `I*`, unsigned compare (`ult`/`ugt`/`ule`/`uge`) for `U*` | IEEE ordered compare: **false** if either is NaN |
| `a & b`, `a \| b`, `a ^ b` | bitwise, on the two's-complement bit pattern | **not defined** |
| `~a` | bitwise not; `~a == -a - 1` | **not defined** |
| `a << s` | shift left, zero-filled; `s >= width` panics `"shift amount out of range"` | **not defined** |
| `a >> s` | signed: arithmetic, sign-filling; unsigned: logical, zero-filling; `s >= width` panics | **not defined** |
| `abs(a)` | wrapping (`abs(MIN)` is `MIN`); `abs_sat`/`abs_checked` also exist | clear the sign bit (`abs(-0.0)` is `+0.0`, `abs(NaN)` is NaN) |
| `min(a, b)`, `max(a, b)` | signed min/max for `I*`, unsigned for `U*` | NaN-propagating (see *Contested* below) |

**Contested - `min`/`max` on floats.** C's `fmin` returns the non-NaN operand;
IEEE 754-2008 `minimum` returns NaN if either operand is NaN; Rust's
`f32::min` returns the non-NaN operand. **Recommend NaN-propagation**: it
matches the way NaN propagates through every other operation in the tower,
and it avoids the trap where `min(NaN, 1.0)` quietly yields `1.0`. Signed
zero is respected: `min(-0.0, +0.0)` is `-0.0`.

### The overflow families

| Form | On overflow | Available for |
| ---- | ----------- | ------------- |
| `a + b`, `a - b`, `a * b` | wraps (two's complement) | all integer types |
| `add_wrap(a, b)`, `sub_wrap`, `mul_wrap` | wraps (same as the operator) | all integer types |
| `add_sat(a, b)`, `sub_sat`, `mul_sat` | clamps to the type's min / max | all integer types |
| `add_checked(a, b)`, `sub_checked`, `mul_checked` | `panic("integer overflow")`, exit 1 | all integer types |

`add_checked` **traps** rather than returning an `Option`, because the
language has no `Option` or `Result` (D003). If a sum type arrives later, the
family may be renamed (`add_try`) and a trapping name adopted; the *rule* -
overflow is a panic, not a value - is what this document fixes.

### The conversions

`to_X(v)` converts `v` to type `X`. `to_X_checked(v)` has the same source
type and the same target type, and panics instead of losing information.

| Source | Target | `to_X` rule |
| ------ | ------ | ----------- |
| integer | integer | **modular**: the low *n* bits of the two's-complement value (`to_u8(-1)` is `255`, `to_i8(200)` is `-56`) |
| integer | float | round to nearest, ties to even; may lose precision; **never** overflows to an infinity (the largest `U64` is far below `F32`'s maximum) |
| float | integer | **truncate toward zero**, then **saturate** to the target's bounds, and `NaN` becomes `0` (`to_i32(1e30)` is `2147483647`, `to_i32(NaN)` is `0`) |
| float | float | round to nearest, ties to even; `F64 â†’ F32` may overflow to `Â±inf` |
| `Bool` | integer | `false` is `0`, `true` is `1` |
| anything | `Bool` | **not allowed** |

The `to_X_checked` twin panics (`"integer overflow"`, `"float out of range"`,
`"not representable"`) when the value cannot be represented in the target:
an integer that does not fit, a float outside the target's range, a NaN
targeted at an integer, or an integer that is not *exactly* representable as
a float.

**Contested - integer â†’ integer out of range.** Rust's `as` truncates
modularly and its `try_into` refuses; Go and Java's narrowing conversion
truncate modularly; Kotlin's `toByte()` refuses; Zig's `@intCast` traps and
its `@truncate` truncates modularly; Swift traps. **Recommend modular for
`to_X` and a trap for `to_X_checked`**, which is Rust's `as` / `try_into`
pair with the `Option` replaced by a panic. Modular is the right default
because it is what the value's bits already are, and because the checked
twin is there for the cases where it is not.

### The literal typing rules

| Literal | Spelling | Type | Extra rule |
| ------- | -------- | ---- | ---------- |
| integer, unsuffixed | `123` | the context's integer type if the value fits in it, otherwise `I64` | must fit in `I64` at all, else a compile-time error |
| integer, suffixed | `123i8` `123i16` `123i32` `123i64` `123u8` `123u16` `123u32` `123u64` | exactly that type | must fit in that type, else a compile-time error |
| float, unsuffixed | `1.0` `1.` `1e9` `1.5e-3` | the context's float type (`F32` or `F64`) if the context is a float, otherwise `F64` | must not overflow the target precision, else a compile-time error |
| float, suffixed | `1.0f32` `1.0f64` | exactly that type | must not overflow that type, else a compile-time error |

Rules that follow from the table:

* The two families do not mix: an integer literal in a float context is an
  error, and a float literal in an integer context is an error. Write
  `1.0` where a float is wanted, `to_f64(1)` where the conversion is meant.
  **Contested.** C allows the mix; Rust rejects it. **Recommend the strict
  rule** - it is one rule, and it keeps "no implicit conversion" true.
* A literal is converted **once**, directly at the target precision. `0.1` in
  an `F32` context is the nearest `f32` to `0.1`, not
  `to_f32(0.1f64)`. The two differ for some decimals, and the single
  conversion is the one that round-trips.
* Hexadecimal, octal and binary integer literals (`0xFF`, `0o17`, `0b1010`)
  and `_` digit separators are recommended additions to the lexer; they are
  cheap and a systems language needs them for masks. They change no rule in
  this table.
* An integer literal may not be negative - `-9223372036854775808` is unary
  minus applied to `9223372036854775808`, which does not fit in `I64`. The
  lexer must therefore fold the sign into the literal when the digits are
  exactly `9223372036854775808`, so that the most negative `I64` is
  expressible. **Today it is not** (verified below).

## What the code looks like today

Facts from the repository, with the commands that produced them.

* **`Int` is `i64` and `Float` is `f64`.** The type lattice is
  `crates/nexum-sema/src/lib.rs:15-32` (`Ty::Int`, `Ty::Float`, `Ty::Bool`,
  `Ty::Str`, `Ty::Unit`, `Ty::Record`, `Ty::Array`, `Ty::List`, `Ty::Never`,
  `Ty::Unknown`, `Ty::Error`); the names are resolved at
  `crates/nexum-sema/src/lib.rs:539-544`; the lowering is
  `crates/nexum-codegen/src/lib.rs:8-20` (`Ty::Int => "i64"`,
  `Ty::Float => "double"`); the ABI table says the same at
  `docs/runtime-abi.md:79-88`.
* **Literal values are `i64` and `f64` in the front end.**
  `crates/nexum-sema/src/lib.rs:125-126` (`HirExpr::Int(i64, Ty)`,
  `HirExpr::Float(f64, Ty)`), `crates/nexum-mir/src/lib.rs:16-18`
  (`Const::Int(i64)`, `Const::Float(f64)`). The lexer keeps the literal's
  *text* (`crates/nexum-lexer/src/lib.rs:303-330`), but the parser parses it
  eagerly (`crates/nexum-parser/src/lib.rs:1080-1089`).
* **There is no explicit conversion of any kind today.** The builtin handling
  starts at `crates/nexum-sema/src/lib.rs:1168` (`panic`) and ends with
  `print` at `crates/nexum-sema/src/lib.rs:1596` (`str_char_at`, `slice`,
  `read_file`, `write_file`, `list_*`, `concat`, `collect`, `heap_used`,
  `len`, `print`). D004 already anticipates them: "or later, an
  explicit conversion function".
* **Mixed `Int`/`Float` operands are already an error** (T008/T017,
  `crates/nexum-sema/src/lib.rs:1965-2016`), and the binary operator rules
  live in `crates/nexum-sema/src/lib.rs:1965-2054`.
* **Integer arithmetic already wraps**, with the `nsw`/`nuw` flags correctly
  omitted (`crates/nexum-codegen/src/lib.rs:523-541`).
* **Integer division is already fully defined**: a zero divisor calls
  `nexum_panic`, and `MIN / -1` is special-cased to `MIN` with `MIN % -1`
  forced to `0` (`crates/nexum-codegen/src/lib.rs:547-613`). The same rule is
  in the constant folder (`crates/nexum-mir/src/lib.rs:870-886`).
* **The collector does not scan numerics.** `has_refs` returns false for every
  scalar (`crates/nexum-codegen/src/lib.rs:22-34`), so `scan_bytes` is 0
  (`crates/nexum-codegen/src/lib.rs:83-99`) and the local is not registered
  (`crates/nexum-codegen/src/lib.rs:361-387`). The rule is written down at
  `docs/runtime-abi.md:150-154`, `docs/language-reference/decisions-gc.md:88-97`
  and `crates/nexum-runtime/src/lib.rs:29-32`.
* **The ABI table and the roadmap already anticipate the tower.**
  `docs/language-reference/decisions.md:222-231` (D015: "the tower grows to
  `f32` and `f64`"), `docs/native-roadmap.md:71` (phase 11c), and
  `docs/native-roadmap.md:114-118` ("`Float` buffers are not scanned at
  all").

### Three defects the tower has to fix, verified on this checkout

The bootstrap compiler at `316f6c6` was built and run against small programs
in `%TEMP%\opencode\fc`. The outputs below are the real ones.

**1. Every float comparison produces invalid LLVM IR.** The `Eq` case tests
the *result* type instead of the *operand* type
(`crates/nexum-codegen/src/lib.rs:624`), so the `fcmp` branch is never taken
and an `icmp` on a `double` is emitted; the ordering cases do the same
unconditionally (`crates/nexum-codegen/src/lib.rs:651-662`).

```
> nx build fc.nx        # fn main(): let a = 1.5; let b = 2.5; if a < b: print(1)
LLVM IR verification failed:
C:\Users\Dell\AppData\Local\Temp\nexum_out.ll:626:18: error: icmp requires integer operands
  626 |   %v6 = icmp slt double %v4, %v5
```

`==` fails the same way (`icmp eq double` - also verified by running it), and
`!=`, `>`, `<=` and `>=` take the same code path
(`crates/nexum-codegen/src/lib.rs:636-662`). **All six float comparisons are
broken today.** The fix is to branch on the operand type, and to emit
`fcmp oeq`/`une`/`olt`/`ogt`/`ole`/`oge` for the float case - which is what
decision 9 specifies.

**2. An integer literal that does not fit in `i64` silently becomes `0`.**
The parser does `t.parse().unwrap_or(0)` (`crates/nexum-parser/src/lib.rs:1083`).

```
> nx emit-ir lit.nx     # fn main(): let a = 99999999999999999999; print(a)
  %v0 = add i64 0, 0
```

No diagnostic. Decision 10 makes this a compile-time error.

**3. The most negative `i64` cannot be written.** The magnitude
`9223372036854775808` does not fit in `i64`, so by defect 2 it parses as `0`,
and the unary minus then yields `0`:

```
> nx run neg.nx         # let a = 0 - 7; print(a / 3); print(a % 3)
                        # let m = 0 - 9223372036854775808; print(m)
-2
-1
0
```

The first two outputs confirm that today's integer division truncates toward
zero and that the remainder takes the sign of the dividend - the rule this
document recommends. The third confirms defect 2. Decision 10 requires the
lexer to fold a leading `-` into the literal when the digits are exactly
`9223372036854775808`, so that `I64`'s minimum is expressible.

Two more current behaviours, verified, that the tower keeps or changes:

```
> nx run dz.nx          # 1 / 0
division by zero       (exit 1)

> nx run fz.nx          # 1.0 / 0.0, 0.0 / 0.0, 0.0 - 1.0, 0.0 - 0.0
inf
nan
-1
0

> nx run nz2.nx         # 0.0 * -1.0, 1.0 / 3.0
-0
0.333333              <-- %g is lossy: 1/3 does not round-trip
```

So: the integer division-by-zero panic and the IEEE float behaviour are
already right, signed zero is already preserved, and the float printer is
already both libc-dependent and lossy.

### The implementation surface for phase 11c

Not a plan - a list of the places the tower touches, so that the orchestrator
can see the size of the change.

| File | What changes |
| ---- | ------------ |
| `crates/nexum-sema/src/lib.rs:15-32` | the `Ty` lattice grows from 2 numeric variants to 10; `Int`/`Float` become spellings of `I64`/`F64` |
| `crates/nexum-sema/src/lib.rs:125-126` | `HirExpr::Int`/`Float` must carry the literal's *text* (or a width) until the context resolves it |
| `crates/nexum-sema/src/lib.rs:539-544` | the type-name resolver learns the ten names plus the `Int`/`Float` synonyms |
| `crates/nexum-sema/src/lib.rs:1965-2054` | `binop_type` learns the per-type operator sets, the "same type or error" rule, and the literal type variable |
| `crates/nexum-sema/src/lib.rs:384-399` | unannotated parameters still default to `Int` |
| `crates/nexum-mir/src/lib.rs:16-18` | `Const` must hold a value of the literal's width, not just `i64`/`f64` |
| `crates/nexum-mir/src/lib.rs:870-917` | the constant folder becomes width-aware and must reproduce the `MIN / -1` rule per width |
| `crates/nexum-codegen/src/lib.rs:8-20` | `llvm_ty` maps the ten types |
| `crates/nexum-codegen/src/lib.rs:36-68` | `llvm_size`/`llvm_align` become width-aware - or, better, are replaced by LLVM's data layout |
| `crates/nexum-codegen/src/lib.rs:501-666` | the operator lowering becomes per-type; the float comparison bug is fixed here |
| `crates/nexum-codegen/src/lib.rs:667-690` | the unary lowering becomes per-type |
| `crates/nexum-codegen/src/lib.rs:723-726` | the `print` format selection becomes per-type, and stops using C `printf` |
| `crates/nexum-lexer/src/lib.rs:303-330` | literal suffixes, hex/octal/binary, exponent notation, `1.` |
| `crates/nexum-parser/src/lib.rs:1080-1089` | literals keep their text; an out-of-range literal is diagnosed |
| `crates/nexum-runtime/src/lib.rs` | the float primitives: `floor`, `ceil`, `trunc`, `round`, `rint`, `frexp`, `ldexp`, `ilogb`, `modf`, the bit casts, and the decimal formatter |
| `docs/runtime-abi.md:74-88` | the ABI table gains the widths and the widening rule |
| `docs/language-reference/grammar.md` | the literal grammar, the operator list, the type list, the static-semantics section |
| `docs/language-reference/decisions.md` | a new decision entry (D017 onwards) recording this document |

New diagnostics will be needed; the free codes today are `T050` and up:
"integer literal out of range", "float literal out of range", "shift amount
out of range", "operand types are not the same numeric type".

## The design

### The syntax

The grammar changes phase 11c needs are small, and they are all in the lexer
and in the grammar document (`docs/language-reference/grammar.md` Â§2 and Â§3):

* **Type names.** The type expression becomes a list of names rather than a
  single identifier: `Int` (= `I64`), `I8`, `I16`, `I32`, `I64`, `U8`,
  `U16`, `U32`, `U64`, `Float` (= `F64`), `F32`, `F64`, `Bool`, `Str`,
  `Unit`, plus the record names. Today `type-expr = identifier` (Â§3) and
  the names are resolved by a string match
  (`crates/nexum-sema/src/lib.rs:539-544`); the resolver becomes a table
  lookup.
* **Integer literals.** `int-lit = digit {digit}` gains `0x`/`0o`/`0b`
  prefixes, `_` separators and a width suffix:
  `int-lit = [ "0x" | "0o" | "0b" ] (digit | "_")+ [ "i8" | "i16" | "i32" |
  "i64" | "u8" | "u16" | "u32" | "u64" ]`. The suffix is absorbed only when
  it matches one of those spellings exactly *and* the next character is not
  an identifier character, so `1i8` is a literal but `1i8x` is the literal
  `1` followed by the identifier `i8x`.
* **Float literals.** `float-lit` gains an exponent and the trailing-dot
  form; a literal is a float when it has a `.` or an exponent. The current
  rule requires a fractional digit after the point
  (`crates/nexum-lexer/src/lib.rs:310-317`), so `1.` and `1e9` do not lex
  today. The suffix `f32` / `f64` is absorbed the same way.
* **Operators.** `&`, `|`, `^`, `~`, `<<`, `>>` are added to the operator
  list (Â§2) with C's precedence: `<<`/`>>` bind tighter than `&`, `^` and
  `|`, which bind tighter than `+`/`-`.
* **No other syntax changes.** There is no `as` cast, no new operator for a
  conversion and no new keyword: the conversions and the overflow families
  are builtins, resolved by name like `print` and `list_push`.

### The lowering

* The type lattice becomes one numeric shape rather than a dozen variants -
  a `Ty::Num { kind: Signed | Unsigned | Float, width: 8 | 16 | 32 | 64 }`
  keeps the operator lowering at one match arm per operation instead of one
  per type. `Int` and `Float` become `Ty::Num { Signed, 64 }` and
  `Ty::Num { Float, 64 }`.
* The operator lowering becomes a table keyed on (operation, kind). The
  integer path is the one that exists today, generalised over the width;
  the `MIN / -1` fix-up (`crates/nexum-codegen/src/lib.rs:577-612`) is
  emitted per width and the zero-divisor check stays.
* The float comparison bug is fixed here: the branch tests the *operand*
  type, and the float case emits `fcmp oeq`/`une`/`olt`/`ogt`/`ole`/`oge`.
* The literal path changes shape: the parser keeps the literal's text, the
  type checker resolves the literal's type from the context, and the
  constant folder parses the text at the resolved width. `Const`
  (`crates/nexum-mir/src/lib.rs:16-18`) becomes width-aware.
* The layout comes from LLVM's data layout rather than from the
  hand-written `llvm_size`/`llvm_align`/`record_size`
  (`crates/nexum-codegen/src/lib.rs:36-81`), because the same numbers are
  used for the collector's scan range and for the element stride of a
  `List`, and a second implementation of the layout is a second source of
  truth.

### What the runtime must provide

| Operation | LLVM | Where it must live |
| --------- | ----- | ------------------- |
| `+ - *` (integer) | `add`/`sub`/`mul` without `nsw`/`nuw` | compiler IR |
| `+ - * / %` (float) | `fadd`/`fsub`/`fmul`/`fdiv`/`frem` | compiler IR |
| `/ %` (integer) | `sdiv`/`srem`/`udiv`/`urem`; `MIN / -1` is poison in LLVM and needs the explicit fix-up the code already emits | compiler IR + the existing `nexum_panic` |
| `& \| ^ ~ << >>` | `and`/`or`/`xor`, `shl`/`lshr`/`ashr`; the out-of-range shift is poison and needs a guard | compiler IR |
| comparisons | `icmp` / `fcmp` | compiler IR |
| conversions | `zext`/`sext`/`trunc`, `sitofp`/`uitofp`, `fptosi`/`fptoui`, `fptrunc`/`fpext`, `bitcast` | compiler IR |
| `sqrt` | `llvm.sqrt.f32/f64` â†’ `sqrtss`/`sqrtsd` (SSE2 baseline) and `fsqrt` (AArch64): a hardware instruction | compiler IR |
| `abs`, `copysign` | `llvm.fabs`, `llvm.copysign` | compiler IR |
| `abs` (integer) | `llvm.abs` | compiler IR |
| bit count, bit scan | `llvm.ctpop`/`ctlz`/`cttz` | compiler IR |
| byte swap, bit reverse | `llvm.bswap`, `llvm.bitreverse` | compiler IR |
| rotate | `llvm.fshl`/`llvm.fshr` | compiler IR |
| `min`/`max` (float) | `llvm.minnum`/`llvm.maxnum` exist but their NaN and signed-zero rules are not the ones this document specifies, so the language defines its own | compiler IR (a compare and a select) |
| **saturating `+`/`-`** | `llvm.sadd.sat`/`usub.sat` (LLVM 8+) | compiler IR |
| **saturating `*`** | **no intrinsic** - expressible with `mul` + `umulh` + compare + select | compiler IR (about 8 instructions) |
| **saturating float â†’ int** | `llvm.fptosi.sat`/`fptoui.sat` (LLVM 12+) - the NaN result is `0` | compiler IR |
| **checked `+ - *`** | `llvm.sadd.with.overflow` and friends give the overflow flag; the panic path is ours | compiler IR + `nexum_panic` |
| **`floor`, `ceil`, `trunc`, `round`, `rint`** | LLVM has the intrinsics, **but on the x86-64 baseline (SSE2, no SSE4.1) they lower to `libm` calls** - which a freestanding build cannot make | **runtime** (about 20 instructions each, done with bit manipulation) |
| **`frexp`, `ldexp`, `ilogb`, `modf`, `remquo`** | none | **runtime** |
| **`exp`, `exp2`, `expm1`, `log`, `log2`, `log10`, `log1p`, `pow`, `sin`, `cos`, `tan`, `asin`, `acos`, `atan`, `atan2`, `sinh`, `cosh`, `tanh`, `asinh`, `acosh`, `atanh`, `cbrt`, `hypot`, `erf`, `erfc`, `tgamma`, `lgamma`** | none | **the Nexum standard library**, in `f32` and `f64` |
| **`fma`** | `llvm.fmuladd` is *not* a fused multiply-add; the x86-64 baseline has no FMA instruction | **not provided** |
| **float â†’ decimal** | none | **runtime** (shortest round-trip decimal, e.g. a Ryu/Grisu implementation) |
| **decimal â†’ float** | none | **runtime** (correctly-rounded decimal parser) - but only if the standard library grows a `parse_float`; the *compiler* may keep using Rust's `str::parse`, because the compiler is allowed the Rust standard library and only the generated program is freestanding |

The elementary functions are the only large piece of work in this table, and
they are better written in Nexum than in LLVM IR text: the runtime crate emits
its module as hand-written LLVM IR (`crates/nexum-runtime/src/lib.rs:46`),
and hand-writing an `exp` in that form is not reasonable. The runtime only
has to supply the bit-level primitives the library needs -
`f32_bits`, `f64_bits`, `bits_f32`, `bits_f64` (pure `bitcast`s) and the
rounding functions - and the library does the rest in Nexum, which is what the
roadmap wants ("the standard library is written in Nexum").

**The accuracy contract for the elementary functions.** The recommendation is
**at most 1 ULP of error** in both precisions, not correctly-rounded results.
Correctly-rounded `exp`, `log` and `pow` are a research-grade problem; the
fdlibm algorithms everyone uses are within 1 ULP and are implementable in a
few hundred lines each. The orchestrator has to accept "not correctly
rounded" in the specification, or the phase gets much larger.

### The collector

| Situation | What the collector does |
| --------- | ----------------------- |
| a local of type `F32`, `I32`, `U64`, ... | 0 scan bytes; **not registered as a root at all** |
| a record whose fields are all numeric | 0 scan bytes; not spilled, not registered |
| an array of numerics, e.g. `[F32; 4096]` | 0 scan bytes; not spilled, not registered |
| a `List[F32]` | the element buffer carries `scan_bytes = 0`; the buffer is not traced |
| a record with **any** reference field | the whole aggregate is scanned conservatively; a numeric field inside it may be mistaken for a pointer |

The last row is the pre-existing "mostly precise" model: the roots are
described precisely, the *contents* of an aggregate are scanned
conservatively. Adding `F32` does not change it and does not need to. What
changes is the *cost*: a `f32` tensor is the case where "not scanned" is the
whole point, and this is what makes phase 11i's tensors cheap.

One consequence of the tower has to be handled in the implementation: the
scan size of an aggregate is computed by a hand-written function
(`crates/nexum-codegen/src/lib.rs:70-81`) that currently assumes every scalar
is 8 bytes (`crates/nexum-codegen/src/lib.rs:55-68`). With `F32` and the
narrow integers, that function is wrong: a record with an `I8` field is 1
byte, not 8. The right fix is to stop recomputing the layout and ask LLVM
for it, so that the layout the collector scans and the layout the ABI uses
cannot drift apart.

### The ABI

| Nexum type | LLVM type | SysV / Windows passing |
| ---------- | ---------- | ----------------------- |
| `F32` | `float` | xmm register |
| `F64` | `double` | xmm register |
| `I8`/`U8` | `i8` | widened to 32 bits in the register |
| `I16`/`U16` | `i16` | widened to 32 bits in the register |
| `I32`/`U32` | `i32` | 32-bit register |
| `I64`/`U64` | `i64` | 64-bit register |
| `Int` | `i64` | 64-bit register |
| `Float` | `double` | xmm register |
| `Bool` | `i1` | widened to 64 bits at the ABI boundary (today's rule) |

Rules that follow:

* **Integer arguments are widened to 64 bits at the syscall boundary**,
  zero-extended for the unsigned types and sign-extended for the signed ones.
  The kernel reads the whole register, so a `U32` count must be zero-extended
  and a negative `I32` must be sign-extended. This is the same treatment
  `Bool` already gets.
* **Record and array layout becomes width-exact.** An `I8` field occupies one
  byte with alignment 1, an `F32` field four bytes with alignment 4. The
  layout must come from LLVM's data layout rather than from the hand-written
  size function, or the collector's scan range and the ABI's layout will
  disagree.
* **Printing.** `print` today lowers to C `printf` with `%lld` / `%g`
  (`crates/nexum-codegen/src/lib.rs:723-726`). With the tower and with the
  freestanding runtime, `print` needs our own formatter for every type: the
  ten numeric types, `Bool`, `Str`, and the float rules - the shortest decimal
  that round-trips, a trailing `.0` so that `1.0` does not print as `1`,
  `-0` distinct from `0`, and `inf` / `-inf` / `nan`. The current `%g` is
  lossy (verified below) and depends on the C library.
* **The range form of `for`** (`for i in a..b`) keeps `Int` bounds, and the
  array index keeps `Int`, so that phase 11c does not churn the existing
  corpus. Changing them to `U64` is a later, separate decision.

## Risks

* **The tower is too big for the compiler that exists.** Ten numeric types
  mean ten branches in every operator, ten conversion targets, ten print
  formats and a width-aware constant folder. The rules are regular, so the
  work is mechanical, but it is not small. The mitigation is to land the
  *rules* (this document) and the *front end* first, with `F32` and the
  widths behind one `Ty::Num { kind, width }` shape rather than ten
  variants.
* **Literal typing is the hardest part of the front end.** Today a literal
  has a concrete type the moment the parser sees it. Context-sensitive
  literals need a type variable in the lattice and a unification step, and
  the fixpoint inference (`crates/nexum-sema/src/lib.rs:384-399`) has to
  cope with it. If this is got wrong, the compiler either rejects
  `let x: I32 = 5` or silently widens.
* **The "one rounding mode" rule is a promise the compiler has to keep.**
  LLVM will contract `a * b + c` into an FMA if the fast-math flags allow
  it, and on some targets it will evaluate in a wider format. The compiler
  must not set `contract` or `fast` on floating-point operations, and must
  not enable excess precision. On x86-64 (SSE2) and AArch64 this is
  automatic, but it is a promise that has to be tested, not assumed.
* **The elementary functions are a large, imprecise chunk of work.** Writing
  correctly-rounded `exp`/`log`/`pow` is a research project; writing 1-ULP
  versions is a few thousand lines. If the orchestrator wants "correctly
  rounded" in the specification, phase 11c gets much bigger.
* **`fma` is the operation that will be asked for.** A model framework wants
  it, and the x86-64 baseline does not have it. Providing it with a "may be
  fused" contract introduces the one place in the language where the same
  source can produce two different results. The recommendation is to not
  provide it and to say so in the decision record, so that the request is
  answered rather than deferred.
* **The ABI must not drift from the collector.** The hand-written layout
  function already assumes 8-byte scalars
  (`crates/nexum-codegen/src/lib.rs:55-68`), and it feeds both the scan size
  and the element stride of a `List`
  (`crates/nexum-codegen/src/lib.rs:775-781`). If it is not made
  width-aware in the same change that introduces the narrow types, the
  collector scans more bytes than the value occupies and a `List[F32]` gets a
  16-byte-per-element buffer instead of an 8-byte one. The right fix is to
  take the layout from LLVM's data layout instead of recomputing it.
* **Adding unsigned integers doubles the conversion surface.** Every rule
  that mentions "signed" now has an unsigned counterpart, and the mixed case
  (`I32 + U32`) is an error rather than a promotion. That is deliberate, but
  it will produce diagnostics that programmers find surprising until they
  read the decision record.
* **Two decisions in this document are reversible only at a high cost.** The
  literal typing rule and the "no implicit conversion" rule are felt
  everywhere. Both are cheap to change now and expensive to change after the
  standard library and the ML framework are written against them.

## Open questions

Each of these has a recommendation; the orchestrator has to confirm it.

1. **How many members does the tower have in 11c?** The rules are stated for
   the whole lattice either way. *Recommendation:* land `F32`, `F64`, `I32`,
   `I64`, `U8`, `U32`, `U64` in 11c and `I8`, `I16`, `U16` with the layout
   work in 11d, because the narrow widths are only useful once packed
   records and byte buffers exist. If the orchestrator prefers one step,
   land all ten.
2. **Is `Int` a synonym of `I64`, or does it stay the primary name?** Both
   spellings must work either way. *Recommendation:* the canonical set in the
   decision record is `I8 I16 I32 I64 U8 U16 U32 U64 F32 F64`, and `Int` and
   `Float` are accepted synonyms of `I64` and `F64` that are never
   withdrawn - so the existing grammar, fixtures and documentation stay
   true, and the naming is uniform across the tower.
3. **Do the integer widths mix implicitly with each other?** The
   recommendation is no: `I32 + I64` is an error, and `to_i64(x)` is the
   way. *Recommendation:* no implicit widening (decision 11).
4. **Does an integer literal get an `F64` context?** The recommendation is
   no - `let x: F64 = 5` is an error, as in Rust. *Recommendation:* keep the
   strict rule; it is the only way "no implicit conversion" stays literally
   true.
5. **Is the shift-amount panic worth the branch?** *Recommendation:*
   yes; the alternative (masking) is what Rust does in release builds and it
   means the same source means different things in different builds.
6. **Is `min`/`max` NaN-propagating?** *Recommendation:* yes, matching
   IEEE 754-2008 `minimum`/`maximum` and the rest of the tower's NaN
   behaviour.
7. **What is the accuracy contract for the elementary functions?** *Recommendation:*
   "at most 1 ULP" in both precisions, stated as such in the decision record
   and the standard library documentation. Correctly-rounded is not
   affordable.
8. **Where do the elementary functions live - the runtime crate, the
   standard library, or a Rust `no_std` static library?** *Recommendation:*
   the Nexum standard library, with the runtime supplying the bit-level
   float primitives. This is what the roadmap wants and it keeps the
   hand-written LLVM IR runtime small.
9. **Is `i128` in scope?** LLVM has `i128`, but 128-bit division has no
   portable code generation - it needs a compiler-rt helper that a
   freestanding build cannot borrow. *Recommendation:* no `i128` in 11c. If
   it is wanted, its division and remainder have to be written by us, and
   that is a decision for a later phase.
10. **Are the `for` range bounds and the array index still `Int`?** The
    recommendation is yes, so that 11c does not churn the corpus; moving
    them to `U64` is a separate decision.
11. **Is `print` of a float required to round-trip?** The recommendation is
    yes - the shortest decimal that parses back to the same value - because
    a lossy print is what makes float debugging miserable, and the current
    `%g` is already lossy (verified above). The cost is a Ryu-class
    formatter in the runtime.
12. **Does `to_*_checked` trap or return an error value?** The
    recommendation is that it traps, because the language has no `Option` or
    `Result` (D003). If a sum type arrives, this is revisited - but the
    *rule* fixed here is that overflow and out-of-range are panics, not
    values.
