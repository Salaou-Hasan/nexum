# The number formatting that replaces `printf`

**Question.** What exactly must a Nexum program print for an `Int`, a `Float`,
a `Bool` and a `Str`, and how is that produced once the C library is gone:
which format the language defines for floats, which algorithm implements it
without libm, what the runtime entry points look like, and what the code
generator emits?

**Summary.** The recommendation is to keep `print`'s output byte-for-byte
identical to what the C library produces today - in particular `Float` is
exactly C's `%g` with the default precision of 6 - so that no program's
output changes and no fixture changes; and to implement it with an *exact*
decimal conversion built on a small base-2^32 bignum, which turns out to be
**cheaper** to write in LLVM IR text than the shortest-round-trip conversion
the brief asks about (no tables, no 128-bit division, no libcalls, about 250
lines of IR). The runtime exposes four helpers named the way
`docs/language-reference/grammar.md` §6 already names them
(`nexum_print_int`, `nexum_print_float`, `nexum_print_bool`,
`nexum_print_str`); they format into a stack buffer and write to stdout
through the system call layer, allocating nothing, so the print path can land
before the allocator does. The code generator drops the format-string globals
and the varargs call and emits a direct call to the matching helper. The one
thing the language should decide for itself is the spelling of a NaN,
because the C library's spelling is platform-dependent.

## Recommendation

1. **The output contract does not change.** `Int` prints as its decimal
   digits with a leading `-` when negative; `Bool` prints as `0` or `1`;
   `Str` prints its bytes verbatim; `Float` prints exactly what C's `%g`
   prints today, followed by a newline. No fixture changes, no program
   output changes, and `docs/language-reference/grammar.md:305` stays true
   ("`5.0` prints as `5`").

2. **`Float` is `%g` exactly, not the shortest round-trip.** The two are
   different formats: they disagree on 99.96% of random doubles (measured).
   The shortest form is *not* sufficient to reproduce `%g` (the counterexample
   is in §3 of the design), and an exact six-significant-digit conversion *is*
   sufficient - a reference implementation of the specification below matched
   the C library on 3,998,627 values with zero mismatches.

3. **The exact conversion is an exact decimal expansion, computed with a
   small bignum, not a table-driven approximation.** The double's exact
   decimal expansion is obtained with one big-integer multiply (by a power of
   five) and repeated division by 10^9, in base 2^32 limbs. That uses only
   `mul i64` and `udiv i64`, which every target has natively. The alternative
   - the shortest-round-trip family (Grisu / Ryu / Schubfach / Dragon4) -
   needs either a bignum anyway (Dragon4) or a 21 KB table of split powers of
   five plus a hand-written 128÷64 division (Ryu), because `udiv i128` lowers
   to a `__udivti3` libcall that a freestanding program cannot have. **The
   exact `%g` is the cheaper of the two to implement here.**

4. **Four runtime entry points, named as the language reference already names
   them** (`grammar.md:263-264`): `nexum_print_int(i64)`,
   `nexum_print_float(double)`, `nexum_print_bool(i64)`,
   `nexum_print_str(ptr)`. Each formats into a stack buffer and writes it to
   stdout (file descriptor 1) through the system call layer. **The print path
   allocates nothing**: it is not a collector safepoint, and a program that
   prints in a loop does not grow the heap.

5. **The formatting core is separate from the writing**, so the future `str`
   module can reuse it. `nexum_fmt_int_to(ptr buf, i64 v) -> i64` (and the
   float and bool siblings) write into a caller-provided buffer, NUL-
   terminate, and return the length. `nexum_fmt_int(i64 v) -> ptr` (and the
   siblings) wrap them and return a NUL-terminated buffer taken from
   `nexum_alloc` - that is the only entry point that depends on the
   allocator, and it is the one the parallel allocator work has to satisfy.

6. **The code generator emits a direct call**, not a varargs call:
   `print(x)` becomes `call void @nexum_print_int(i64 %v)` (or the float,
   bool or str variant). The `fmt_N` format-string globals and the
   `get_fmt_global` helper disappear from the module, and the runtime's
   `declare i32 @printf(ptr, ...)` goes away.

7. **The language fixes the NaN spelling itself.** The C library's is
   platform-dependent: glibc and the BSD/macOS libc print `nan` and `-nan`,
   while the UCRT on Windows prints `nan`, `-nan`, `nan(ind)` and
   `nan(snan)` depending on the payload (measured). The language should
   print `nan` for every NaN and `-nan` for a NaN with the sign bit set -
   the glibc/macOS spelling, and the one the POSIX targets will see. This is
   a deliberate deviation from the Windows C library; no fixture prints a
   NaN, so nothing changes in the test suite.

8. **A lossless float representation is a separate, later piece of work.**
   `%g` with six digits does not round-trip: 99.95% of random doubles do not
   survive a `print` / `parse` cycle. When the standard library grows its
   `str` module, the shortest round-trip representation should be added as
   its own function (`repr`), not by changing `print`. §"Open questions"
   has the decision.

## What the code looks like today

### The `print` builtin in the code generator

`crates/nexum-codegen/src/lib.rs:716-755` is the whole of it, abridged:

```rust
if callee == "print" {
    let argty = args.first().and_then(|a| vt.get(&a.0).cloned()).unwrap_or(Ty::Int);
    let fmt_content = match argty {
        Ty::Int => "%lld\n",
        Ty::Float => "%g\n",
        Ty::Bool => "%d\n",
        Ty::Str => "%s\n",
        _ => "%lld\n",          // a List: a ptr passed to a %lld conversion
    };
    let gname = get_fmt_global(fmt_content, globals, gcounter, strmap);
    ...
    buf.push_str(&format!(
        "  call i32 (ptr, ...) @printf(ptr @{}, {} {})\n", gname, argtystr, argreg));
    if let Some(d) = dest { ... }   // the dummy Unit result
}
```

* The format strings are the four literals at `lib.rs:723-729`.
* `get_fmt_global` (`lib.rs:211-226`) interns them as `@fmt_N` globals, which
  are emitted at `lib.rs:158-167` as NUL-terminated byte arrays.
* `Bool` is `zext`ed from `i1` to `i64` (`lib.rs:740-746`) because
  `printf`'s variadic arguments are promoted.
* The `_` arm is reached for a `List`: `llvm_ty` maps `Ty::List` to `ptr`
  (`lib.rs:14`), so the call passes a **pointer to a `%lld` conversion** - a
  varargs type mismatch. On the 64-bit targets it happens to print the
  address as a decimal number, but it is formally undefined behaviour. The
  type checker rejects records and arrays for `print`
  (`crates/nexum-sema/src/lib.rs:1606-1631`, diagnostic T028) but not
  `List`, so that arm is reachable today.

### The runtime side

* `crates/nexum-runtime/src/lib.rs:49` declares `printf`; `:50-51` declare
  `fputs` and `fputc`, which the Windows panic path uses.
* `crates/nexum-runtime/src/strings.rs:11-27` is the runtime's own
  `nexum_strlen`, `nexum_memcpy` and `nexum_strcmp` - the pattern this work
  should follow: the runtime writes its own routines instead of borrowing the
  C library's.
* `crates/nexum-runtime/src/syscalls.rs:237-292` is the system call layer.
  The write wrapper is `nexum_sys_write(fd, buf, count)`, all three arguments
  `i64`; the existing file routines call it that way
  (`strings.rs:41,45`).
* The panic path (`crates/nexum-runtime/src/lib.rs:116-126`) already shows
  the two output paths: on POSIX `nexum_sys_write(2, ...)` to stderr, on
  Windows `fputs`/`fputc` to `__acrt_iob_func(2)`.
* The allocator entry point that exists today is `nexum_alloc(i64) ->
  ptr` (`crates/nexum-runtime/src/lib.rs:150`), which currently calls
  `malloc`; it returns the *payload* pointer (header + 32) and is the seam
  the parallel allocator work replaces.

### The language reference

* `docs/language-reference/grammar.md:263-264` already specifies the shape
  this work should take: "A `print(x)` call lowers to a call into the
  runtime print helpers (`nexum_print_int`, `nexum_print_float`,
  `nexum_print_bool`, `nexum_print_str`)." **The code does not do this yet;
  the document describes the intent.**
* `docs/language-reference/grammar.md:305-306` records the open item:
  "`Float` printing currently uses C `%g` (`5.0` prints as `5`); a runtime
  formatter lands with the runtime ABI in Phase 4."
* `docs/language-reference/decisions.md:72` (D004) records the same open
  item: "Open: exact Float printing (`%g` today) and explicit conversion
  intrinsics are pinned with the runtime ABI in Phase 4."
* `docs/runtime-abi.md:26` lists `printf`, `fputs`, `fputc` as the C
  library pieces that phase 11a.2 removes.

### The fixtures that must not change

`crates/nexum-driver/tests/backend.rs:94-138` compiles every
`tests/fixtures/run/*.nx`, runs it, and compares stdout with the sibling
`.out` file byte for byte (after a CRLF normalisation). The float-producing
prints in that directory are:

| Fixture | Source | Value | Current output |
| ------- | ------ | ----- | -------------- |
| `arith.nx:5` | `print(2.5 * 2.0)` | 5.0 | `arith.out:4` = `5` |
| `int_wrap.nx:15` | `print(1.0 / 0.0)` | +inf | `int_wrap.out:7` = `inf` |
| `foreach.nx:17` | `print(s)` | 14.0 | `foreach.out:4` = `14` |
| `particles.nx:16-19` | four `print`s | 11.0, 22.0, 13.0, 24.0 | `particles.out:1-4` |
| `records.nx:10,12,14` | three `print`s | 25.0, 52.0, 4.0 | `records.out:1-3` |

Every one of those values is an exact integer (or +inf). That fact is what
makes the choice in §Recommendation 2 free: **the `%g` format and the
shortest-round-trip format produce the same text for every value the
fixtures print**, so neither choice changes a fixture.

`examples/demo.nx:16` prints `2.5 * 2.0` and is affected the same way; it
has no golden file.

## The design

### 1. The output contract

Every `print(x)` writes the formatted value and then one `'\n'` (0x0A).
Nothing else: no width, no padding, no separators, no locale.

| Type | LLVM type | Output |
| ---- | --------- | ------ |
| `Int` | `i64` | the decimal digits of the value, `-` in front when negative, `0` for zero. No padding, no `+`, no grouping. |
| `Bool` | `i1` (widened to `i64` at the call) | `0` or `1`. |
| `Str` | `ptr` | the bytes of the NUL-terminated buffer, verbatim. A `Str` cannot contain an interior NUL (`docs/runtime-abi.md` §8), so the byte count is the string length. |
| `Float` | `double` | exactly C's `%g` with the default precision of 6 (the specification is in §2). |

The buffer sizes that follow from this:

| Formatter | Longest output | Buffer needed (with the NUL) |
| --------- | -------------- | ---------------------------- |
| `Int` | `-9223372036854775808` = 20 characters | 21 |
| `Bool` | `0` = 1 character | 2 |
| `Str` | (the string itself; no copy) | 0 |
| `Float` | `-1.23457e+308` = 13 characters | 14 |

Use 24, 2, 0 and 32 bytes respectively, so that the sizes are not tight.
(The float *conversion* needs a much larger scratch area - about 1.2 KB -
which is why it lives in its own function with its own frame; see §4.)

### 2. What `%g` means, exactly

This is the specification the runtime has to implement. It is the C11 rule
(7.21.6.1) with the default precision of 6, and it was validated against the
C library (see §"Verification").

Given a finite double `v`:

**Special values first.**

* `v` is infinite: `inf`, or `-inf` when the sign bit is set.
* `v` is a NaN: `nan`, or `-nan` when the sign bit is set. (The language
  fixes this; see Recommendation 7. The C library does not agree with itself
  across platforms.)
* `v` is zero: `0`, or `-0` when the sign bit is set. **The negative zero
  keeps its sign.**

**Otherwise: round to six significant digits, then lay the digits out.**

Let the exact value of `v` be `m · 2^e`, with `m` an integer (the significand
with the implicit bit, or the fraction for a subnormal) and `e` an integer.

1. **The exact decimal expansion.** Let `s` be the number of digits after the
   decimal point and `D` the digit string of the significand:
   * if `e >= 0`: `D` is the digits of `m · 2^e` and `s = 0`;
   * if `e < 0`: `D` is the digits of `m · 5^(-e)` and `s = -e`.

   `D` has at most 767 digits (the worst case is the smallest subnormal),
   and `m · 5^(-e)` needs at most 2547 bits (80 limbs of 32 bits).

2. **The decimal exponent.** `intdigits = len(D) - s` is the number of digits
   before the decimal point, and `X = intdigits - 1` is the exponent of the
   leading digit. (`intdigits` may be zero or negative: `0.1` has
   `intdigits = 0` and `X = -1`.)

3. **Round to six significant digits.** The six digits are `D[0..5]`, zero
   padded on the right if `D` is shorter. The seventh digit is `D[6]` and
   "the rest" is `D[7..]`:
   * `D[6] > '5'`: round the sixth digit up;
   * `D[6] < '5'`: leave it;
   * `D[6] == '5'` and the rest is not all zeros: round up;
   * `D[6] == '5'` and the rest **is** all zeros: this is an exact tie -
     round up if the sixth digit is odd, leave it if the sixth digit is even
     (round half to even, which is what the C library does under the
     default rounding mode).

   If the round-up carries (`999999` becomes `1000000`), the six digits
   become `100000` and `X` increases by one.

4. **Choose the layout.** The test is on `X` *after* the rounding:
   * **`-4 <= X <= 5`: fixed notation.** The number of decimals is
     `5 - X`.
     * `X >= 0`: the integer part is the first `X + 1` of the six digits and
       the fractional part is the remaining `5 - X` of them.
     * `X < 0`: the integer part is `0` and the fractional part is `-X - 1`
       zeros followed by the six digits.
     * The trailing zeros of the **fractional part only** are removed; if it
       becomes empty, the `.` goes with it. The integer part keeps its
       zeros, so `100000` stays `100000`.
   * **otherwise: exponential notation.** The first digit, then a `.` and the
     remaining five digits with their trailing zeros removed (the `.` is
     omitted when all five are zeros), then `e`, then the sign (`+` unless
     `X` is negative), then the absolute value of `X` in **at least two
     digits**: `1e+06`, `1e-05`, `1.23457e+308`.

5. **The sign.** A `-` in front when the sign bit is set.

Worked examples, all measured against the C library:

| Value | `%g` | Why |
| ----- | ---- | --- |
| `5.0` | `5` | `X = 0`, fixed, `5.00000` → strip → `5` |
| `0.1` | `0.1` | `X = -1`, fixed, `0.100000` → strip → `0.1` |
| `1.0/3.0` | `0.333333` | six digits, `X = -1` |
| `1e-4` | `0.0001` | `X = -4`, still fixed |
| `1e-5` | `1e-05` | `X = -5`, exponential |
| `1e5` | `100000` | `X = 5`, fixed |
| `1e6` | `1e+06` | `X = 6`, exponential |
| `1234567.0` | `1.23457e+06` | `X = 6`, six digits |
| `99999.5` | `99999.5` | six digits exactly, `X = 4` |
| `999999.5` | `1e+06` | exact tie, sixth digit 9 is odd → up → carry → `X = 6` |
| `123456.5` | `123456` | exact tie, sixth digit 6 is even → down |
| `1000000.5` | `1e+06` | exact tie, sixth digit 0 is even → down |
| `1.0000001` | `1` | rounds to `1.00000` → strip → `1` |
| `9.999995` | `10` | tie, up, no carry: `10.0000` → strip → `10` |
| `-0.0` | `-0` | the sign is kept |
| `1.0/0.0` | `inf` | |
| `5e-324` | `4.94066e-324` | the smallest subnormal |

### 3. The algorithms, and why the shortest form is not the answer

**The shortest round-trip conversion is not sufficient to reproduce `%g`.**
The counterexample is `1.234565`:

* the double nearest to `1.234565` is `1.2345649999999999`, slightly *below*
  the decimal;
* its shortest round-trip representation is `1.234565` (six digits, it
  round-trips);
* `%g` of that double is `1.23456`, because the true value is below the
  six-digit midpoint and rounds down.

So the shortest form and `%g` genuinely disagree, and no amount of care in a
shortest-round-trip implementation will make them agree. The other direction
holds: an exact six-significant-digit rounding, followed by the layout rules
of §2, reproduces `%g` exactly. That is what the reference implementation
below does, and it matched the C library on every value it was given.

The families, and what each costs **in LLVM IR text**, which is what the
runtime is written in:

| Algorithm | Output | What it needs | Verdict |
| --------- | ------ | ------------- | ------- |
| **Exact decimal expansion (this design)** | `%g` | one bignum multiply by a power of five, repeated division by 10^9, base 2^32 limbs. Uses `mul i64` and `udiv i64` only. | **Chosen.** No tables, no libcalls, about 250 lines of IR, exact. |
| Dragon4 (Steele/White, Flajolet) | shortest or fixed precision | an arbitrary-precision bignum; the fixed-precision variant is the same machinery | Viable; the bignum is the same one as above, so the exact variant is strictly less work |
| Ryu `d2s` | shortest | the split powers of five (`DOUBLE_POW5_SPLIT`, `DOUBLE_POW5_INV_SPLIT`, about 21 KB of tables) **and** a 128÷64 division, because `udiv i128` lowers to a `__udivti3` libcall | More work, and it produces a different format |
| Ryu `d2fixed` | correctly rounded fixed precision | the same tables plus a 192-bit slow path | The exact `%g`, done the hard way |
| Grisu2 | shortest, correct about 99.9% of the time | cached powers of ten; needs a fallback to be exact | Not exact on its own |
| Schubfach | shortest | a table and careful interval reasoning | More work, different format |

The two facts that decide this, both measured on this machine with the
toolchain's own clang 23.1.3:

* `mul i128` is expanded **inline** by the LLVM backend on x86-64 (the
  generated assembly is four instructions, no call), and `shl i128` likewise.
  But `udiv i128` becomes `callq __udivti3` - a libcall from compiler-rt,
  which a freestanding program must not have. So a Ryu-style implementation
  needs a hand-written 128÷64 division, and the exact-`%g` implementation
  needs none.
* A shortest-round-trip implementation needs the split powers of five in
  every program: about 21 KB of module text, in a language whose selling
  point is that its programs are small.

**Performance.** The exact conversion is dominated by the bignum, and the
bignum is small for ordinary values: a double like `3.14` has `e = -51`, so
`5^51` is four 32-bit limbs, the product is six limbs, and the whole
expansion is about 51 digits - roughly a hundred limb operations, on the
order of a hundred nanoseconds. The worst case (a subnormal) is about 80
limbs and 767 digits, a few thousand operations. If the cost ever matters,
a fast path can be added in front (a double-precision estimate plus an
uncertainty test, falling back to the bignum when the estimate lands within
an ulp of a six-digit midpoint); the exact path stays as the arbiter.

**A lossless representation is still worth having, later.** `%g` does not
round-trip: 99.95% of random doubles do not survive `print` followed by
`parse`. The shortest-round-trip form always does. That belongs in the `str`
module as a separate function (§"Open questions"), not in `print`.

### 4. The conversion, step by step

`nexum_fmt_float_to(ptr %buf, double %v) -> i64` - writes the text of `%v`
into `%buf` (at least 32 bytes), NUL-terminates it, and returns the number
of characters written, not counting the NUL.

**a. The bit decomposition.**

```
bits = bitcast double %v to i64
sign = bits < 0                      ; signed compare: the sign bit
ebits = (bits >> 52) & 0x7FF
frac  = bits & 0x000FFFFFFFFFFFFF
```

* `ebits == 0x7FF` and `frac != 0`: a NaN → `nan` / `-nan`.
* `ebits == 0x7FF` and `frac == 0`: an infinity → `inf` / `-inf`.
* `ebits == 0` and `frac == 0`: a zero → `0` / `-0`.
* `ebits == 0` (a subnormal): `m = frac`, `e = -1074`.
* otherwise: `m = frac | (1 << 52)`, `e = ebits - 1075`.

**b. The exact expansion, with a bignum.**

The bignum is base 2^32, little-endian, in a stack array of 88 limbs (352
bytes - the worst case is 80). Two operations are needed, and both are
straightforward:

*Multiply by a small constant.* `N = m · 5^k` is built by multiplying the
bignum by `5^13 = 1220703125` (which fits in 32 bits) `k / 13` times and by
`5^(k mod 13)` once. The chunk size is chosen so that
`limb · 5^13 + carry` stays below 2^63: the product of a 32-bit limb and
`5^13` is at most 5.24 · 10^18, and `carry < 5^13`, so the sum fits in an
`i64`. Each step is `zext` the limb to `i64`, `mul i64` by the constant,
split the result into the new limb and the carry.

*Divide by a small constant.* To turn `N` into decimal digits, divide by
10^9 repeatedly and keep the remainders. In base 2^32 the running remainder
is below 10^9, so `rem · 2^32 + limb` is below 2^63 and the quotient limb
comes from a single `udiv i64` by 10^9 (the quotient is at most `2^32 - 1`,
because `rem · 2^32 + limb <= 10^9 · 2^32 - 1`). The remainders are the
decimal groups, least significant first; the last group is the most
significant and is not padded with zeros.

**c. From the digits to the text.**

With `D` the digit string (most significant first) and `s` the number of
fractional digits, `intdigits = len(D) - s` and `X = intdigits - 1`. Then
§2's steps 3 to 5 apply: take `D[0..5]`, round, lay out.

**d. The special values** are written directly; note that `-0.0` is `"-0"`
and that a NaN never has a fractional part.

### 5. The integer formatting

`nexum_fmt_int_to(ptr %buf, i64 %v) -> i64`.

The contract: the decimal digits of `%v`, `-` in front when negative, no
padding, no separators; the NUL after the last character; the return value
is the character count.

The algorithm, with the two details that are easy to get wrong:

* **Use unsigned division.** `udiv i64` and `urem i64` treat the bit pattern
  as unsigned, which is what the magnitude needs. Signed division would
  break on the negative values.
* **`INT64_MIN` needs no special case.** The magnitude of `-9223372036854775808`
  is 2^63, which does not fit in a positive `i64`. But the two's-complement
  pattern of `0 - INT64_MIN` *is* 2^63, and the unsigned operations read it
  as 9223372036854775808. So: `neg = (%v < 0)` (a signed comparison), and
  `u = 0 - %v` (a wrapping subtraction); the digits of `u` are produced with
  `urem`/`udiv` by 10.

Writing the digits backwards from the end of the buffer and then copying them
to the front is the simplest shape, and `nexum_memcpy` already exists for
the copy. The longest output is `-9223372036854775808`: 20 characters, so
the buffer needs 21 bytes with the NUL.

`Bool` is the same routine with a value of 0 or 1; the codegen already
widens `i1` to `i64`, so `nexum_print_bool` can simply be
`nexum_fmt_int_to` with the value, and the output is `0` or `1`.

`Str` needs no formatting at all: the bytes are already in a NUL-terminated
buffer, so the printer writes `nexum_strlen(s)` bytes of it.

### 6. Where the bytes go, and the allocator

Two layers, with different answers to "where do the bytes live":

**The print path - no allocation.** The four `nexum_print_*` helpers format
into a buffer in their own stack frame and write it to stdout. Nothing is
allocated, so:

* the print path is not a collector safepoint and does not grow the heap;
* a program that prints in a loop does not trigger collections;
* **the print path does not depend on the allocator at all**, so it can land
  before the allocator work does.

**The format path - allocation.** `nexum_fmt_int(i64) -> ptr` and its
siblings return a NUL-terminated buffer, for the future `str` module
(`format`, `repr`, `parse`). This is the only part that needs the allocator,
and its requirement on the allocator is exactly the one the parallel work
has to meet:

> a `ptr @nexum_alloc(i64 %bytes)` that returns a pointer to at least
> `%bytes` writable bytes, and whose result is a *managed* object with
> `scan_bytes = 0` (a string leaf, exactly as `nexum_concat` allocates it
> today at `crates/nexum-runtime/src/lib.rs:191`).

The buffer is written by `nexum_fmt_*_to` and then NUL-terminated by the
wrapper, so the core does not know where the memory came from.

**The write itself.** On the POSIX targets, `nexum_sys_write(1, buf, len)` -
the same wrapper the file routines use (`strings.rs:45`), with the file
descriptor 1 for stdout. On Windows, the C library is still in use until the
system call stubs land (phase 11a.2), so the Windows path uses
`__acrt_iob_func(1)` and `fwrite`, the same shape the Windows panic path
uses today (`crates/nexum-runtime/src/lib.rs:116-119`).

Two things the writer has to get right:

* **A partial write must be retried.** `write` may write fewer bytes than
  asked for, especially on a pipe. The loop is: while bytes remain, call
  `nexum_sys_write`, advance the pointer by what was written, and stop on a
  negative result (an error) or a zero return. The existing
  `nexum_write_file` does not loop (`strings.rs:44-45`); that is a latent
  bug in a different file, and the new writer should not copy it.
* **The newline is a byte, not a line discipline.** The printer appends
  `0x0A` itself. The current `printf` on Windows writes `CRLF` because the
  C runtime is in text mode; the fixture harness normalises `CRLF` to `LF`
  (`crates/nexum-driver/tests/backend.rs:103,121`), and the `.out` files are
  stored with LF (`.gitattributes`), so writing a bare `0x0A` is correct and
  is one byte less.

### 7. The runtime entry points

```llvm
; Print to stdout: the formatted value and a newline.
define internal void @nexum_print_int(i64 %v);
define internal void @nexum_print_float(double %v);
define internal void @nexum_print_bool(i64 %v);
define internal void @nexum_print_str(ptr %s);

; The formatting core: write into the caller's buffer, NUL-terminate,
; return the length (not counting the NUL).
define internal i64 @nexum_fmt_int_to(ptr %buf, i64 %v);
define internal i64 @nexum_fmt_float_to(ptr %buf, double %v);
define internal i64 @nexum_fmt_bool_to(ptr %buf, i64 %v);

; The same text as a managed string, for the standard library.
define internal ptr @nexum_fmt_int(i64 %v);
define internal ptr @nexum_fmt_float(double %v);
define internal ptr @nexum_fmt_bool(i64 %v);
```

`internal` is right: the code generator's calls are in the same module, the
way it already calls `nexum_panic` (`crates/nexum-codegen/src/lib.rs:574`
and three other places). (The string routines are the exception, for a
historical reason.)

The `*_to` core writes into a buffer of at least 32 bytes for a float and 24
for an integer; the `nexum_print_*` wrappers own such a buffer in their own
frame and pass it down.

### 8. The code generator change

`crates/nexum-codegen/src/lib.rs:716-755` becomes:

```rust
if callee == "print" {
    let argty = args.first().and_then(|a| vt.get(&a.0).cloned()).unwrap_or(Ty::Int);
    let argreg = args.first()
        .map(|a| valreg.get(&a.0).cloned().unwrap_or_else(|| format!("%v{}", a.0)))
        .unwrap_or_default();
    let arg0 = args.first().map(|a| a.0).unwrap_or(0);
    match argty {
        Ty::Float => buf.push_str(&format!("  call void @nexum_print_float(double {})\n", argreg)),
        Ty::Bool => {
            let zr = format!("%v{}_z", arg0);
            buf.push_str(&format!("  {} = zext i1 {} to i64\n", zr, argreg));
            buf.push_str(&format!("  call void @nexum_print_bool(i64 {})\n", zr));
        }
        Ty::Str => buf.push_str(&format!("  call void @nexum_print_str(ptr {})\n", argreg)),
        Ty::List(_) => {
            // Today this reaches the `_` arm of the format match, which
            // passes a pointer to a `%lld` conversion: a varargs type
            // mismatch. Keep the observable behaviour (the address, as a
            // number) but make it a well typed call. T028 should reject
            // `List` outright.
            let p = format!("%v{}_i", arg0);
            buf.push_str(&format!("  {} = ptrtoint ptr {} to i64\n", p, argreg));
            buf.push_str(&format!("  call void @nexum_print_int(i64 {})\n", p));
        }
        _ => buf.push_str(&format!("  call void @nexum_print_int(i64 {})\n", argreg)),
    }
    if let Some(d) = dest {
        let r = format!("%v{}", d.0);
        valreg.insert(d.0, r.clone());
        buf.push_str(&format!("  {} = add i64 0, 0\n", r)); // dummy unit
    }
}
```

The `List` arm is spelled out rather than folded into the default because
the default arm passes an `i64`, and a `List` is a `ptr`: without the
`ptrtoint` the emitted call would not verify.

The emitted IR for the four cases:

```llvm
  call void @nexum_print_int(i64 %v3)
  %v4 = zext i1 %v5 to i64
  call void @nexum_print_bool(i64 %v4)
  call void @nexum_print_float(double %v6)
  call void @nexum_print_str(ptr %v7)
```

The `Bool` widening stays (the runtime takes an `i64`, and the widening is
already there today); the `Float` argument needs no conversion, because the
`Float` is an `f64` in the language and an `f64` in LLVM.

Two things disappear:

* `get_fmt_global` (`lib.rs:211-226`) and the `@fmt_N` globals
  (`lib.rs:158-167`): no format strings are needed any more, so the module
  loses one global per distinct type and the interning helper becomes dead
  code.
* `declare i32 @printf(ptr, ...)` in the runtime (`lib.rs:49`). The
  `fputs`/`fputc` declarations stay until the Windows path is done.

**One hole to close.** The `_` arm currently passes a `List`'s pointer to a
`%lld` conversion - a varargs type mismatch, which on the 64-bit targets
prints the address and is formally undefined behaviour. The type checker
rejects records and arrays for `print` (T028,
`crates/nexum-sema/src/lib.rs:1606-1631`) but not `List`, so
`print(some_list)` reaches it. The recommendation is to extend T028 to
`List` (and `Unit`), so the match above is exhaustive over
{Int, Float, Bool, Str} and the `List` arm can go away. That is a language
change and belongs in a decision (§"Open questions"); until it is made, the
`List` arm above keeps the current observable behaviour - the address, as a
decimal number - but makes it a well typed call.

### 9. What the fixtures would have to change to

For the recommendation as stated: **nothing**. The five float-producing
prints in `tests/fixtures/run/` print exact integers (5, 14, 11, 22, 13, 24,
25, 52, 4) and one infinity, and for every one of those values the `%g`
format and the shortest-round-trip format produce the same text.

For the record, here is what the alternatives would change, so the
orchestrator can see the whole picture:

* **Shortest round-trip instead of `%g`:** the run fixtures would still not
  change (the same reason), but the *format* would change for 99.96% of
  other values: `1.0/3.0` would print `0.3333333333333333` instead of
  `0.333333`, and `1234567.0` would print `1.234567e+06` instead of
  `1.23457e+06`. The layout rule would also have to be re-specified, because
  `%g`'s "fixed if `-4 <= X < 6`" is tied to its precision of 6; the natural
  replacement is to keep `-4 <= X <= 5` as constants, which gives the same
  layout decisions for every value.
* **A fixed 17-digit representation** (always enough to round-trip a
  double): the run fixtures would not change either, but the output would be
  much noisier (`14.00000000000000000`).
* **A lossy fixed 6-digit format with a different tie rule** (round half
  away from zero instead of half to even): the run fixtures would not
  change, but `123456.5` would print `123457` instead of `123456`, and
  `999999.5` would print `1000000` instead of `1e+06`. The exact tie cases
  are common in the range `[10^5, 2^53)`, so this is not a corner case.

## Verification

What was run, and what it showed.

**1. The `%g` specification was implemented from the standard and checked
against the C library.** A reference implementation (round the exact value
to six significant digits with ties-to-even, then apply the layout rules of
§2) was compiled with the toolchain's own clang
(`C:\Program Files\LLVM\bin\clang.exe`, clang 23.1.3) and compared with
`snprintf("%g")` over the edge cases, 3,000,000 random bit patterns and
1,000,000 random "nice" decimals:

```
tested=3998627 mismatches=0
```

**2. The C library's actual behaviour was measured** for the values in §2,
including the special ones:

```
0.0        -> [0]          -0.0        -> [-0]
1.0        -> [1]          0.1         -> [0.1]
1/3        -> [0.333333]   1e-4        -> [0.0001]
1e-5       -> [1e-05]      1e5         -> [100000]
1e6        -> [1e+06]      99999.5     -> [99999.5]
999999.5   -> [1e+06]      1000000.5   -> [1e+06]
123456.5   -> [123456]     1234567.0   -> [1.23457e+06]
1.0000001  -> [1]          9.999995    -> [10]
INFINITY   -> [inf]        -INFINITY   -> [-inf]
NAN        -> [nan]        -NAN        -> [-nan(ind)]
DBL_MAX    -> [1.79769e+308]  5e-324  -> [4.94066e-324]
```

**3. The NaN spelling is platform-dependent** (UCRT on this machine):
`0x7ff8000000000001` → `nan`, `0xfff8000000000001` → `-nan`,
`0xfff8000000000000` → `-nan(ind)`, `0x7ff0000000000001` → `nan(snan)`.
glibc and the BSD/macOS libc print `nan` and `-nan` for all of these. This
is why Recommendation 7 has the language fix the spelling.

**4. The shortest form is not sufficient** (the counterexample in §3):

```
1.2345649999999999 -> %g=[1.23456]   shortest=[1.234565]
0.1234565           -> %g=[0.123456]  shortest=[0.1234565]
999999.5            -> %g=[1e+06]     shortest=[999999.5]
```

**5. The two formats are different, and `%g` does not round-trip.** Over
200,000 random doubles: `%g` and the shortest form differ on 199,913 of
them; `%g` does not round-trip on 199,895 of them; the shortest form
round-trips on all of them.

**6. Every float the run fixtures print is an exact integer or an
infinity**, so the format choice does not touch the fixtures: 5.0, 14.0,
11.0, 22.0, 13.0, 24.0, 25.0, 52.0, 4.0 and +inf.

**7. The fixture suite passes today**, so the baseline is known:

```
$ cargo test -p nexum-driver --test backend run_outputs_match_fixtures
running 1 test
test run_outputs_match_fixtures ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out
```

**8. The LLVM facts the design rests on** (the toolchain's clang,
`x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc`):

```
mul i128   -> expanded inline (mulq / imulq / addq), no call
shl i128   -> expanded inline (shldq / shlq), no call
udiv i128  -> callq __udivti3@PLT        ; a compiler-rt libcall
```

So a Ryu-style implementation would have to hand-write its 128÷64 division
and would carry about 21 KB of tables, while the exact conversion in §4
needs neither.

**Not verified.** The bignum conversion of §4 was specified and its
arithmetic was checked by hand, but it has not been implemented and run -
this is a research deliverable and no code was changed. The sizes quoted
for the limbs and the digit buffer follow from the worst case (the smallest
subnormal has a 767-digit exact expansion and needs 2547 bits, so 80 limbs
of 32 bits and 88 allocated limbs). The exact `%g` was validated through a
reference implementation in C, not through an IR implementation; an
implementation in the runtime still has to be checked against the C library
over the same value set before it can be trusted.

## Risks

* **The float conversion is the risky part.** Every other formatter is
  trivial. A bug in the bignum shows up as a wrong digit, and the wrong
  digit is silent. The mitigation is the same one used here: a reference
  implementation, compared against the C library over millions of values
  (random bit patterns, subnormals, the extremes, and every exact tie).
* **The exact ties are the interesting cases.** An exact tie at six digits
  is possible whenever the value is a half-integer in `[10^5, 10^6)` (and
  beyond, while the midpoint still fits in 53 bits), and the C library
  rounds those to even. A conversion that rounds ties away from zero, or
  that cannot detect them, will differ from `%g` on values that are not
  exotic at all - `999999.5` and `123456.5` are ordinary numbers.
* **The stack cost of the float path is about 1.2 KB** (352 bytes of bignum
  plus an 800-byte digit buffer). That is fine for a print, but it should be
  checked against the runtime's stack budget, and the buffer should not be
  allocated in the *caller's* frame.
* **A partial write to stdout would truncate the output.** The writer has
  to loop. The existing `nexum_write_file` does not, and the new code should
  not copy that.
* **The Windows path is still the C library**, so the Windows output keeps
  its `CRLF` line endings until the Windows system call stubs land
  (phase 11a.2). The fixture harness normalises them, so the tests do not
  notice; a program that compares its own output byte for byte would.
* **Six digits are lossy.** `print` cannot be used to write a float that is
  read back. This is today's behaviour and it is preserved, but it is a
  wart, and the standard library will need a lossless form.
* **The `List` hole.** Until T028 covers `List`, `print` on a list passes a
  pointer to a `%lld` conversion - a varargs type mismatch that prints the
  address on the 64-bit targets. The design above keeps the current
  behaviour, but it should not be left that way.

## Open questions

1. **Keep `%g`, or define a lossless float format now?** *Recommendation:
   keep `%g` for `print` - it is what the language does today, it keeps
   every program and every fixture unchanged, and it is the cheaper of the
   two to implement. Add the shortest round-trip form later, as a separate
   function in the `str` module (`repr`), and give it its own entry point
   (`nexum_repr_float`). The two can coexist: `print` uses `%g`, `repr` uses
   the shortest form.* This also keeps phase 11c (the `f32` tower) simple:
   `f32` needs nine significant digits to round-trip, so it cannot share
   the six-digit rule, and a separate `repr` is where that belongs.

2. **What is a NaN, in the language's own words?** *Recommendation: `nan`
   for every NaN and `-nan` when the sign bit is set - the glibc and macOS
   spelling, and the one the POSIX targets produce today. It costs nothing
   on the POSIX targets and deviates from the UCRT's `nan(ind)` on Windows,
   which no program should depend on.* If the orchestrator prefers strict
   bug-compatibility with the host C library, the alternative is to keep the
   platform's spelling - but then the language's output is platform
   dependent, which is worse.

3. **Does `print` accept a `List`?** *Recommendation: no. Extend T028 to
   `List` (and `Unit`) so that `print` accepts exactly `Int`, `Float`,
   `Bool` and `Str`, and drop the `List` arm from the code generator. This
   is a language change, so it needs its own decision entry; the
   alternatives are to print the list's contents (a different feature) or to
   keep the status quo.*

4. **Who owns the stdout buffer?** *Recommendation: no buffer for now - one
   `write` per print, exactly as the panic path does today. A program that
   prints in a loop will make one system call per line, which is a
   performance question the standard library should answer (a `stdout`
   handle with a buffer), not the print formatter.*

5. **Does the float conversion want a fast path now?** *Recommendation: no.
   The exact conversion is a few hundred limb operations for an ordinary
   value, and a print is not a hot loop in a bootstrap language. The fast
   path (a double-precision estimate plus an uncertainty test, with the
   bignum as the arbiter) is a later optimisation, and the exact path stays
   as the definition of the format.*

6. **Who verifies the implementation?** *Recommendation: the implementer
   ports the reference implementation used here into a test that compares
   the runtime's output with the C library's over the same value set, as
   long as the C library is available - the Windows build still has it. The
   test then becomes a table of expected strings, so it keeps working once
   the C library is gone.*

## Changes needed elsewhere

* `crates/nexum-codegen/src/lib.rs:716-755` - the `print` lowering (this
  document, §8); `get_fmt_global` (`:211-226`) and the `@fmt_N` globals
  (`:158-167`) become dead.
* `crates/nexum-runtime/src/lib.rs:49` - `declare i32 @printf(ptr, ...)`
  goes away.
* `crates/nexum-runtime/src/strings.rs` (or a new module beside it) - the
  formatters and the print helpers.
* `crates/nexum-sema/src/lib.rs:1606-1631` - T028 extended to `List`, if
  Open question 3 is decided that way.
* `docs/language-reference/grammar.md:263-264` - the text is already
  correct; `:305-306` and `docs/language-reference/decisions.md:72` (D004)
  can be closed, and a new decision entry should record the float format,
  the NaN spelling and the `repr` function.
* `docs/runtime-abi.md:26` - the row for `printf`, `fputs` and `fputc` moves
  from "the C library" to "done" for the POSIX targets, and the new runtime
  entry points are listed with the ABI.
