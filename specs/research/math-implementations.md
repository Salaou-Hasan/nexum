# The maths builtins, implemented without libm

**Question.** What does the Nexum runtime have to provide so that a Nexum
program can do the arithmetic a model framework needs, when the program cannot
link the C library? For each function: which algorithm, which accuracy, what
it costs, and how hard it is to write in LLVM IR text (which is what the
runtime is made of).

**Summary.** The recommendation is to implement six functions in the runtime
IR now — `sqrt`, `exp`, `log`, `expm1`, `log1p`, `tanh` — and to leave the
rest to the standard library or to later phases. `sqrt` is free: LLVM's
`llvm.sqrt.f64` is one instruction (`sqrtsd` / `fsqrt`) and is correctly
rounded (0 ULP measured). The other five are the FDLIBM algorithms
(Johnson's `exp`/`log` minimax polynomials, the `expm1`/`log1p` corrections,
`tanh` on top of `expm1`); measured against a 60-digit reference they land at
**≤ 1 ULP for `exp`, `log`, `expm1`, `log1p` and ≤ 2 ULP for `tanh`**, and
they cost 8–17 ns per call on this host, within a factor of two of the
platform C library. The important discovery is that **LLVM's own math
intrinsics do not remove the libm dependency**: `llvm.exp.f64`,
`llvm.log.f64`, `llvm.pow.f64`, `llvm.sin.f64` and `llvm.cos.f64` are lowered
to *calls to the libm symbols* (`exp@PLT`, `log@PLT`, …), and
`llvm.fma.f64` — which the FMA-based rewrites of the polynomials want — is
also lowered to a `fma@PLT` call on the x86-64 baseline target the toolchain
currently uses, because that target has no FMA instruction. So the builtins
have to be written out. A prototype of the five was written in IR text and
measured: 831 lines of IR, 112–172 x86-64 instructions per function, and six
bugs — five of them in the bit-level plumbing and the branch conditions, one
of them a missing factor in a polynomial. None of them was in the
mathematics.

## Recommendation

Implement, in the runtime IR, in this order.

| # | Function | Algorithm | Accuracy (measured) | Difficulty in IR text | Cost |
| - | -------- | --------- | ------------------- | --------------------- | ---- |
| 1 | `sqrt`   | `llvm.sqrt.f64` — one `sqrtsd` / `fsqrt` | **0 ULP** (correctly rounded) | trivial: 3 instructions | 4 ns |
| 2 | `exp`    | FDLIBM `__ieee754_exp`: 2-term Cody–Waite reduction to \|r\| ≤ ½ln2, degree-11 minimax `P1..P5`, scale by 2^k | **≤ 1 ULP** | medium: the reduction and the 2^k scaling need bit handling | 8.5 ns |
| 3 | `log`    | FDLIBM `__ieee754_log`: normalise the mantissa into [√2/2, √2), atanh-form minimax `Lg1..Lg7` in `s = f/(2+f)` | **≤ 1 ULP** | medium: exponent extraction, mantissa normalisation, two summation orders | 9.5 ns |
| 4 | `expm1`  | FDLIBM `s_expm1`: the `exp` reduction plus the rational minimax `Q1..Q5` and the `k ≤ -2 / k > 56` special cases | **≤ 1 ULP** | hard: 5 branches on `k`, the `1 − 2^-k` construction, the `k ≥ 20` branch | 11 ns |
| 5 | `log1p`  | FDLIBM `s_log1p`: the `log` normalisation plus the correction term `c = (1+x − u)/u` and `Lp1..Lp7` | **≤ 1 ULP** | hard: the `1+x` rounding correction and the `k == 0` validity condition | 14 ns |
| 6 | `tanh`   | FDLIBM `s_tanh`: `1 − 2/(expm1(2\|x\|)+2)` for \|x\| ≥ 1, `−expm1(−2\|x\|)/(expm1(−2\|x\|)+2)` below | **≤ 2 ULP** | easy once `expm1` exists: one division | 17 ns |

Then, in a later phase, and only if the framework asks for them:

| Function | Algorithm | Accuracy | Difficulty | Note |
| -------- | --------- | -------- | ---------- | ---- |
| `erf`   | FDLIBM `s_erf`: two rational approximations (`pp/qq`, `pa/qa`) plus two asymptotic series (`ra/sa`, `rb/sb`) | ≤ 1 ULP | hard: 40+ constants, four regimes | needed only for exact GELU; the tanh approximation of GELU is 1e-4 accurate and needs nothing |
| `erfc`  | `1 − erf` below 1, the `rb/sb` asymptotic series above | ≤ 1 ULP (absolute) | hard | rarely needed |
| `pow`   | FDLIBM `__ieee754_pow`: a double-double logarithm, an exponential, and ~40 special cases | ≤ 1 ULP, **not** correctly rounded | very hard: ~150 lines | for a learning-rate schedule use `exp(step * log(decay))` instead, which is exact to ~1 ULP for the values that matter |
| `log2`  | `log(x) · (1/ln2)`, with the constant split `ivln2hi + ivln2lo` | ≤ 2 ULP | trivial | `log2` is a multiply away from `log` |
| `log10` | `log(x) · (1/ln10)`, with `ivln10hi + ivln10lo` | ≤ 2 ULP | trivial | same |
| `sinh`  | FDLIBM `e_sinh`: `exp` plus the `2^-28` and 22 thresholds | ≤ 1 ULP | easy on top of `exp` | not needed by a model framework |
| `cosh`  | FDLIBM `e_cosh` | ≤ 1 ULP | easy on top of `exp` | not needed |
| `gamma`, `lgamma` | FDLIBM `e_lgamma_r`: the Stirling series with the reflection formula | ≤ 1–2 ULP | very hard: 40+ constants, the sign of the result | not needed by a model framework |

The accuracy in this second table is the *published* accuracy of the
FDLIBM original, not a measurement: only the six functions above were
implemented and measured (section 7).

**Ordered list of what to implement first, with the reason for each step.**

1. **`sqrt`** — it is one LLVM instruction and it is exactly rounded, so there
   is nothing to implement and nothing to get wrong. It is also the first
   function a framework asks for (RMSNorm, Adam's second moment).
2. **`exp`** — softmax, the exponential learning-rate schedule, and every
   positive-parameter activation need it. It is the hardest of the five to
   *transcribe*, because the argument reduction has to be exact.
3. **`log`** — log-softmax, cross-entropy, and the natural log of a
   likelihood. It shares the "read the exponent out of the bit pattern" idea
   with `exp`, so the bit helpers are already there.
4. **`expm1` and `log1p`** — not "nice to have". `exp(x) − 1` and `log(1+x)`
   lose their *relative* accuracy for small arguments, and the numerically
   stable losses (Poisson NLL, `log(1 − p)`, the ELBO) are exactly the places
   where a framework hits that. They are the two functions that are *not*
   reachable from `exp` and `log` by a subtraction.
5. **`tanh`** — one division on top of `expm1`, and the activation that
   appears in every recurrent and every transformer block.
6. **`erf`** — only when the framework wants the exact GELU. The tanh
   approximation of GELU differs from the exact one by at most 1e-4, which is
   far below the noise of a training run, so this can wait for the phase that
   builds the framework.
7. **`pow`** — only if something genuinely needs a general power. A
   learning-rate schedule of the form `base · decay^step` is computed as
   `exp(step · log(decay))`, and for any `decay` that is not within 1e-8 of 1
   that is accurate to about 1 ULP.
8. **`log2`, `log10`, `sinh`, `cosh`** — the standard library's business, not
   the runtime's (see "Runtime or standard library" below).

**The one thing not to do:** do not reach for `llvm.exp.f64` and
`llvm.log.f64` and call the dependency removed. They are libm calls in
disguise (measured, see below).

## What the code looks like today

* The runtime is emitted as LLVM IR text by
  `crates/nexum-runtime/src/lib.rs:50` (`runtime_ir(os, arch)`), which
  concatenates a few string constants: the declarations of the C-library
  symbols the program still uses (`crates/nexum-runtime/src/lib.rs:53`:
  `printf`, `fputs`, `fputc`, `exit`, `malloc`, `free`), the string routines
  (`crates/nexum-runtime/src/strings.rs:11`), the file routines
  (`crates/nexum-runtime/src/strings.rs:33`) and the system call layer
  (`crates/nexum-runtime/src/syscalls.rs`).
* The house style for the runtime is **one long string literal per function**,
  with the IR written flat: `crates/nexum-runtime/src/strings.rs:15` is the
  whole of `nexum_strlen` on one line. There is no IR builder, no assembler
  and no escape hatch: the only way to add a routine is to write IR text.
* The runtime is freestanding on the POSIX targets: `docs/runtime-abi.md:11`
  (§0) and `docs/language-reference/decisions.md:178` (D012). The Windows
  build is still on the C library.
* D015 (`docs/language-reference/decisions.md:222`) already decides the
  question this document answers: "the math builtins implemented by us rather
  than taken from `libm`", and says why — "it needs the math functions to be
  ours so that the results do not depend on the C library's version".
* The roadmap names the five functions the language is missing:
  `docs/native-roadmap.md:60` — "no `sqrt`, `exp`, `log`, `tanh`, `erf` — no
  math at all beyond `+ - * / %`". Phase 11a owns "our own … math and I/O
  routines" (`docs/native-roadmap.md:81`) and phase 11c owns "the math
  builtins (implemented by us, not libm)" (`docs/native-roadmap.md:83`).
* The standard library doc already reserves a `math` module for
  "trigonometry, rounding, constants" (`docs/standard-library.md:50`).
* The target table (`crates/nexum-target/src/lib.rs:47` and `:53`) gives
  plain triples — `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu` —
  with **no `-march`/`-mcpu`**, so x86-64 is the SSE2 baseline. This matters
  for FMA (see below).
* The link step still calls `clang`
  (`crates/nexum-toolchain/src/lib.rs:177`); the IR is handed to it as a
  file (`crates/nexum-toolchain/src/lib.rs:229` verifies a module by
  compiling it).

## The design

### 1. What LLVM already gives us, and what it does not

Measured by compiling IR to x86-64 assembly with clang 23.1.3
(`clang -x ir -S -O2 --target=x86_64-unknown-linux-gnu`):

| Intrinsic | Lowered to | Consequence |
| --------- | ---------- | ----------- |
| `llvm.sqrt.f64` | `sqrtsd %xmm0, %xmm0` | **one instruction**, IEEE-754 correctly rounded |
| `llvm.fabs.f64` | `andpd` | one instruction |
| `llvm.fma.f64` | `jmp fma@PLT` (baseline) / `vfmadd213sd` (with `-mfma`) / `fmadd` (AArch64) | **a libm call on the current x86-64 target** |
| `llvm.exp.f64` | `jmp exp@PLT` | a libm call |
| `llvm.log.f64` | `jmp log@PLT` | a libm call |
| `llvm.pow.f64` | `jmp pow@PLT` | a libm call |
| `llvm.sin.f64` | `jmp sin@PLT` | a libm call |
| `llvm.cos.f64` | `jmp cos@PLT` | a libm call |
| `llvm.exp2.f64` | `jmp exp2@PLT` | a libm call |
| `llvm.log2.f64` | `jmp log2@PLT` | a libm call |
| `llvm.log10.f64` | `jmp log10@PLT` | a libm call |

Two consequences.

* **Using the LLVM math intrinsics does not remove the libc dependency.**
  LLVM has no inline expansions for them; the backend emits a call to the
  libm symbol. A program built with `llvm.exp.f64` still declares `exp`.
* **FMA is not available on the x86-64 baseline the toolchain targets.**
  `llvm.fma.f64` becomes a `fma@PLT` call, which is a libm call. The
  FMA-based rewrites of the polynomials (which are the obvious way to get
  the accuracy down to 0.5 ULP) are therefore *not* available to us on
  x86-64 without changing the target, and AArch64 has FMA for free
  (`fmadd`).

So the baseline algorithms have to be the ones that were designed **without**
FMA. That is exactly what FDLIBM's `__kernel_exp` and `__kernel_log` are:
they were written in 1993 for machines with no fused multiply-add, and they
still reach < 1 ULP. That is the recommendation: use the FDLIBM
formulations, not an FMA rewrite of them.

(`llvm.pow.f64` is a special case: `LibCallSimplifier` turns `pow(x, 2.0)`
into `x*x` and `pow(x, 3.0)` into `x*x*x`, but a general exponent stays a
call.)

### 2. The algorithms, and the polynomials

All the coefficients below are FDLIBM's (Sun Microsystems, 1993), as carried
by openlibm (`src/e_exp.c`, `src/e_log.c`, `src/s_expm1.c`,
`src/s_log1p.c`, `src/s_tanh.c`, `src/s_erf.c`, `src/e_lgamma_r.c`,
`src/e_pow.c`). The hexadecimal forms are the ones the source comments say
are the intended values; the decimal forms are what can be typed. **Both
are given so that the implementer does not have to trust a decimal→binary
conversion.** The licence question (the FDLIBM notice must be preserved) is
an open question below.

#### 2.1 `exp` — the argument reduction

`exp(x) = 2^k · exp(r)` with `x = k·ln2 + r` and `|r| ≤ ½ln2 ≈ 0.34658`.

The reduction has to be *exact enough*: `k` reaches 1024, so `k·ln2` must be
computed without losing the low bits of `r`. This is what the Cody–Waite
split does — `ln2` is written as the sum of two doubles whose sum is the
correctly rounded `ln2`, and the high part has 20 trailing zero bits, so
`k·ln2_hi` is **exact** for every `|k| < 2^32`:

```
ln2_hi = 6.93147180369123816490e-01   (0x3FE62E42FEE00000)   20 trailing zeros
ln2_lo = 1.90821492927058770002e-10   (0x3DEA39EF35793C76)
invln2 = 1.44269504088896338700e+00   (0x3FF71547652B82FE)    1/ln2
```

`k` is `round(x · invln2)` (fdlibm uses a truncation of `x/ln2 ± ½`, which
is the same thing with a different tie rule). Then

```
hi = x − k·ln2_hi          (exact)
lo = k·ln2_lo
r  = hi − lo
```

The polynomial is evaluated on `r` and the result is scaled by `2^k`, built
by putting `(1023 + k)` into the exponent field of the bit pattern
(`(1023+k) << 52`).

#### 2.2 `exp` — the polynomial

`exp(r)` is computed through the *R-form*: with `z = r²`,

```
R(z) = 2 + P1·z + P2·z² + P3·z³ + P4·z⁴ + P5·z⁵
```

approximates `r·(e^r+1)/(e^r−1)` to better than 2⁻⁵⁹ on `[0, 0.34658]`, and

```
exp(r) = 1 + r + r·R1(r)/(2 − R1(r)),   R1(r) = r − (P1·r³ + P2·r⁵ + … + P5·r¹¹)
```

The code in fdlibm computes `c = r − (P1·r² + P2·r⁴ + … )` (a rescaled
form) and then `1 − ((r·c)/(c−2) − r)`.

```
P1 =  1.66666666666666019037e-01   (0x3FC555555555553E)   = 1/6
P2 = -2.77777777770155933842e-03   (0xBF66C16C16BEBD93)   = -1/360
P3 =  6.61375632143793436117e-05   (0x3F11566AAF25DE2C)   = 1/15120
P4 = -1.65339022054652515390e-06   (0xBEBBBD41C5D26BF1)   = -1/604800
P5 =  4.13813679705723846039e-08   (0x3E66376972BEA4D0)   = 1/23950080
```

Reference range: `|r| ≤ 0.5·ln2 = 0.34657359027997264`. Degree 11, five
coefficients, error bound 2⁻⁵⁹.

#### 2.3 `log` — the argument reduction

`log(x) = k·ln2 + log(m)` with `x = m·2^k` and `m ∈ [√2/2, √2)`.

`k` is the binary exponent (`E − 1023`) and `m` is built by rewriting the
high word of `x`:

```
i    = (mantissa + 0x95f64) & 0x100000      ; 0x95f64 is the rounding threshold
m    = mantissa | (i ^ 0x3ff00000)          ; rewrite the exponent to 1023 or 1022
k   += (i >> 20)                            ; i == 0x100000 means "divide by two"
```

The two high words `0x3ff00000` and `0x3fe00000` are the ones that make
`m` land in `[√2/2, √2)`; the `0x95f64` threshold is the one that decides
which side of 1 the mantissa is on. The low 32 bits of `x` are preserved —
that is what `SET_HIGH_WORD` does and what a bitcast-plus-mask has to
reproduce.

#### 2.4 `log` — the polynomial

With `f = m − 1` and `s = f/(2+f)`, `log(m) = 2s + s·R(s²)`, and `R` is
approximated by a degree-14 minimax on `|s| ≤ 0.1716`:

```
Lg1 = 6.666666666666735130e-01   (0x3FE5555555555593)
Lg2 = 3.999999999940941908e-01   (0x3FD999999997FA04)
Lg3 = 2.857142874366239149e-01   (0x3FD2492494229359)
Lg4 = 2.222219843214978396e-01   (0x3FCC71C51D8E78AF)
Lg5 = 1.818357216161805012e-01   (0x3FC7466496CB03DE)
Lg6 = 1.531383769920937332e-01   (0x3FC39A09D078C69F)
Lg7 = 1.479819860511658591e-01   (0x3FC2F112DF3E5244)
```

Evaluation (the order matters, and it is the order in which the "missing
`w`" bug below hides):

```
z = s·s ;  w = z·z
t1 = w·(Lg2 + w·(Lg4 + w·Lg6))
t2 = z·(Lg1 + w·(Lg3 + w·(Lg5 + w·Lg7)))
R  = t1 + t2
```

and then, in the two summation orders fdlibm uses to avoid cancellation:

```
k == 0,  "positive" form:   log(m) = f − (hfsq − s·(hfsq + R))
k == 0,  "negative" form:   log(m) = f − s·(f − R)
k != 0,  "positive" form:   log(m) = k·ln2_hi − ((hfsq − (s·(hfsq+R) + k·ln2_lo)) − f)
k != 0,  "negative" form:   log(m) = k·ln2_hi − ((s·(f−R) − k·ln2_lo) − f)
```

The branch between the two forms is `i = mantissa − 0x6147a; j = 0x6b851 −
mantissa; if (i | j) > 0` use the positive form.

Reference range: `m ∈ [√2/2, √2)`, i.e. `f ∈ [−0.2929, 0.4142]`,
`s ∈ [−0.1716, 0.1716]`.

#### 2.5 `expm1`

Same reduction as `exp`, but the polynomial is a *rational* approximation of
`R1(r²) = 1 − r²/60 + r⁴/2520 − …`, scaled so that `z = r²/2`:

```
Q1 = -3.33333333333331316428e-02   (0xBFA11111111110F4)   = 2·(-1/60)
Q2 =  1.58730158725481460165e-03   (0x3F5A01A019FE5585)   = 4·(1/2520)
Q3 = -7.93650757867487942473e-05   (0xBF14CE199EAADBB7)   = 8·(-1/100800)
Q4 =  4.00821782732936239552e-06   (0x3ED0CFCA86E65239)   = 16·(1/3991680)
Q5 = -2.01099218183624371326e-07   (0xBE8AFDB76E09C32D)   = 32·(-1/159252300)
```

evaluated as `r1 = 1 + hxs·(Q1 + hxs·(Q2 + hxs·(Q3 + hxs·(Q4 + hxs·Q5))))`
with `hxs = r²/2`, then

```
t = 3 − r1·(r/2) ;  e = hxs·((r1 − t)/(6 − r·t))
```

and the correction term `c = (hi − r) − lo` added back, because the
reduction rounds `r`.

`expm1` has four more branches that `exp` does not have, and they are the
reason it is the hardest of the five to write:

* `k == 0` → `r − (r·e − hxs)`;
* `k == −1` → `0.5·(r − e) − 0.5`;
* `k == 1` → `−2·(e − (r + 0.5))` if `r < −0.25`, else `1 + 2·(r − e)`;
* `k ≤ −2` or `k > 56` → `2^k·(1 − (e − r)) − 1`;
* `1 ≤ k < 20` → `y = (1 − 2^-k) − (e − r)`, `y·2^k` (the `1 − 2^-k` is
  built by rewriting the high word: `0x3ff00000 − (0x200000 >> k)`);
* `k ≥ 20` → `y = r − (e + 2^-k)`, `y + 1`, `y·2^k` (here `2^-k` is built
  from the high word `(0x3ff − k) << 20`).

#### 2.6 `log1p`

The `log` reduction, applied to `1 + x` — but `1 + x` is *rounded*, so a
correction term is needed:

```
u = 1.0 + x                  (rounded)
c = (k > 0) ? 1.0 − (u − x) : x − (u − 1.0)      ; the lost part
c /= u
```

and the same `Lp1..Lp7` coefficients as `Lg1..Lg7` are used, with
`f = u − 1` and `s = f/(2+f)`.

There is one subtlety that is easy to get wrong and that the prototype got
wrong first: the `k == 0` path (where `f = x` directly) is only valid when
`1 + x ∈ [√2/2, √2+)`. fdlibm spells that condition as
`hx > 0 || hx <= (int32)0xbfd2bec4`; without it, `log1p` is evaluated
outside the range its polynomial was fitted for and loses about seven
digits.

#### 2.7 `tanh`

```
|x| < 2^-28 :  tanh(x) = x
|x| ≥ 22    :  tanh(x) = 1
|x| ≥ 1     :  tanh(x) = 1 − 2/(expm1(2|x|) + 2)
|x| < 1     :  tanh(x) = −expm1(−2|x|)/(expm1(−2|x|) + 2)
```

with the sign of `x` applied at the end. It is `expm1` plus one division,
which is why it inherits `expm1`'s 2 ULP.

#### 2.8 `erf` — for later

`erf` is two rational approximations and two asymptotic series, and it needs
40+ constants. For reference, the shape of it is:

```
erx = 8.45062911510467529297e-01  (0x3FEB0AC160000000)
efx = 1.28379167095512586316e-01  (0x3FC06EBA8214DB69)
efx8= 1.02703333676410069053e+00  (0x3FF06EBA8214DB69)
pp0..pp4, qq1..qq5   (the |x| < 0.84375 rational approximation)
pa0..pa6, qa1..qa6   (the 0.84375 <= |x| < 1.25 rational approximation)
ra0..ra7, sa1..sa8   (the 1.25 <= |x| < 2.857 asymptotic series)
rb0..rb6, sb1..sb7   (the |x| >= 2.857 asymptotic series, also used by erfc)
```

The full tables are in the source file; the point of listing them here is
that `erf` is a *different kind of job* from `exp` and `log`: it is four
regimes and a lot of constants, and it is worth it only if the framework
needs the exact GELU.

### 3. The special cases

Measured on the prototype (the bit patterns are the actual outputs):

| Input | `sqrt` | `exp` | `log` | `expm1` | `log1p` | `tanh` |
| ----- | ------ | ----- | ----- | ------- | ------- | ------ |
| `+0.0`  | `+0` | `1` | `−inf` | `+0` | `+0` | `+0` |
| `−0.0`  | `−0` | `1` | `−inf` | `−0` | `−0` | `−0` |
| `+inf`  | `+inf` | `+inf` | `+inf` | `+inf` | `+inf` | `1` |
| `−inf`  | NaN | `0` | `+inf` | `−1` | NaN | `−1` |
| NaN     | NaN | NaN | NaN | NaN | NaN | NaN |
| `−1.0`  | NaN | `e⁻¹` | NaN | `e⁻¹−1` | `−inf` | `−tanh(1)` |
| `x < 0` | NaN | `e^x` | NaN | `e^x−1` | NaN (x < −1) | `tanh(x)` |
| `x > 709.782712893384` | | `+inf` | | `+inf` | | |
| `x < −745.133219101941` | | `0` | | `−1` | | |
| subnormal `x` | `sqrt(x)` | `1` | **finite** (`−708.4 … −744.4`) | `x` | `x` | `x` |

Two of these deserve a comment.

* **`log` of a subnormal must be finite.** `log(2.2e-308)` is about
  −708.4, not −inf. fdlibm scales a subnormal up by 2⁵⁴ and corrects the
  exponent by −54. Getting this wrong is easy: the "is the high word
  zero?" test is also the "is the argument zero?" test.
* **`exp` between −745.13 and −708.4 is a subnormal**, not zero. fdlibm's
  underflow threshold is −745.13; the region between that and the point
  where the result stops being normal is computed by the ordinary path
  (the scaling is done in two steps, `2^(k+1000)·2^-1000`, so the
  intermediate stays normal and only the last multiply underflows).
  Getting *this* wrong is also easy: the obvious reading of "if |x| is
  large, special-case it" turns the whole subnormal range into zero.

### 4. What accuracy a training run actually needs

**A few ULP is enough, and one ULP is what the algorithm gives anyway.**

The argument, in numbers:

* A double has a relative resolution of 2⁻⁵² ≈ 2.2e-16. A 1-ULP error in
  `exp` is a relative error of 2.2e-16 in the result.
* A softmax over N logits turns that into an absolute error of about
  N·2⁻⁵² in each probability and about the same in the log-probability:
  for N = 1000, 2e-13.
* A loss curve changes by ~1e-3 per step early in training and by ~1e-6
  near convergence. 2e-13 is **seven to nine orders of magnitude** below
  that. A 4-ULP implementation (8.8e-16) is still six to nine orders
  below.
* A cross-entropy loss of order 1 has an absolute error of ~1e-13 from a
  1-ULP `log`; the gradient of the loss inherits the same relative error.

So the tolerance that matters is: **`exp` and `log` within 4 ULP, and
`expm1`/`log1p` with a good *relative* accuracy for small arguments** —
which is the whole point of those two functions. The FDLIBM algorithms
deliver ≤ 1 ULP (and ≤ 2 ULP for `tanh`) at no extra cost, so there is no
reason to trade accuracy down for speed here.

What actually breaks a training run is *not* ULP error but:

1. **Overflow to infinity.** `exp(800)` is `+inf`; the loss becomes `inf`
   and then `NaN` after one gradient step. This is why the framework must
   subtract the maximum before exponentiating, and why `exp` has to return
   `+inf` rather than something finite.
2. **`log(0)` = `−inf`.** The same argument, from the other side.
3. **Losing relative accuracy on small arguments.** `exp(1e-12) − 1`
   computed with `exp` and a subtraction is 0 (the true value is 1e-12);
   `expm1(1e-12)` is exact to 1 ULP. Likewise `log1p(1e-12)` versus
   `log(1 + 1e-12)`, where the addition rounds to 1 and the logarithm
   returns 0. Any loss written as `log(1 − p)` or `log(sigmoid(x))` needs
   `log1p`.
4. **Reproducibility.** Two machines, two C libraries, two results. D015
   wants the results to be identical everywhere; that is an argument for
   owning the implementation and for pinning the exact coefficients in a
   test.

### 5. Runtime IR, or the standard library?

**The split I recommend: the six hot functions in the runtime IR, the long
tail in the standard library.**

The argument for the runtime:

* The runtime IR is the only place where the bit-level plumbing can live.
  `exp` and `log` need to read the exponent out of a double's bit pattern
  and to build `2^k` by writing an exponent field. The Nexum language has
  no `f32`/`f64` bit operations, no `i32`, and no casts between them
  (D015, `docs/language-reference/decisions.md:222`, is still staged), so a
  standard-library `exp` written in Nexum could not do the reduction at all
  without new language features.
* These functions are called in the inner loop of every layer, so they have
  to be inlinable and free of call overhead. The runtime is linked into
  every program as `internal` definitions, and the compiler can see them.
* Every program needs them, not just the framework.

The argument for the standard library:

* `log2`, `log10`, `sinh`, `cosh`, `erf`, `erfc`, `gamma`, `lgamma` need
  **no bit manipulation at all** — they are built out of `exp`, `log` and
  ordinary arithmetic. A standard library written in Nexum can express
  them.
* They are rare. Putting 40 constants of `erf` and 40 of `lgamma` into
  every program's image is a cost that most programs should not pay.
* The standard library is where "convenience" belongs: the `math` module is
  already reserved in `docs/standard-library.md:50`.

The trade-off to be aware of: **a standard-library implementation is
slower**, because the call is not inlined and the interpreter overhead
dominates a 20-instruction polynomial. For `erf` in a GELU that is
irrelevant (it is called once per element and the whole point of GELU is
that it is 1e-4 accurate anyway). For `exp` it would be fatal.

There is one more consideration, and it is the reason I would not put `erf`
in the runtime even though the roadmap names it: **the runtime IR is written
by hand, and hand-written IR is where the bugs were.** All six of the bugs
in the prototype were in the bit-level plumbing of the runtime-style code,
not in the mathematics. `erf` is four regimes and 40 constants; the
probability of getting it right by hand on the first try is not high, and
the cost of a wrong `erf` in a GELU is a silently different model. The
tanh-approximation of GELU is 1e-4 accurate and needs nothing at all.

### 6. What the IR text costs

The prototype — `sqrt`, `exp`, `log`, `expm1`, `log1p`, `tanh` in LLVM IR
text, no FMA — is **831 lines** of IR and **112–172 x86-64 instructions**
per function (measured with `llvm-objdump`):

| Function | x86-64 instructions | ns/call (this host) | the C library, same host |
| -------- | ------------------- | ------------------- | ------------------------ |
| `sqrt`   | 3 | 4.0 | — |
| `exp`    | 112 | 8.5 | 6.0 |
| `log`    | 144 | 9.5 | 6.5 |
| `expm1`  | 169 | 11.0 | 8.5 |
| `log1p`  | 172 | 14.0 | — |
| `tanh`   | 61 (+ `expm1`) | 17.0 | 7.0 |

(The timings are rough: `clock()` on Windows has a 1 ms granularity, over
2·10⁶ calls; the host is x86-64 Windows with the MSVC C library, whose
`exp`/`log` are Intel's optimised routines. The order of magnitude is what
matters: our routines are within a factor of two of a tuned libm.)

Things that are specific to writing this in IR text, and that the
implementer needs to know:

* **LLVM IR has no parentheses.** Every expression that C would parenthesise
  needs its own temporary, and there is no operator precedence to lean on.
  A degree-11 polynomial is 11 lines, not 1.
* **Unnamed values are function-scoped.** Two `%t` in the same function are
  an error, even in different blocks, so the names have to be unique
  across the whole function.
* **The bit helpers are the dangerous part.** The prototype needed four of
  them — read the high word, read the low word, build a double from two
  words, replace the high word — and **five of the six bugs were in those
  four functions or in the branch conditions that use them**:
  1. `nexum_sethi` was missing a `shl i64 %hi64, 32`, so it wrote the
     new high word into the *low* half of the double. The result was a
     denormal, and `log` was off by 8e-4 in the mantissa part.
  2. `nexum_mk` had the same missing shift.
  3. `nexum_pow2` built `2^k` as `(0x3ff00000 + k) << 20` instead of
     `(1023 + k) << 52`; the shift overflowed the 32-bit register and `2^k`
     came out as a denormal.
  4. The `log` polynomial was missing one `w` factor: `Lg1 + (Lg3 + w·…)`
     instead of `Lg1 + w·(Lg3 + w·…)`. The error was 8e-4 — eight digits
     — and it looked like a rounding error.
  5. The `expm1` branch condition was `k < −2` where fdlibm has `k ≤ −2`.
  6. The `log1p` `1+x` branch was selected with an unsigned comparison
     where fdlibm uses a signed one, and the `k == 0` path was taken
     outside the range its polynomial was fitted for.

  None of these are mathematics. They are transcription errors, and the
  only reason they were found is that the result was compared against a
  60-digit reference over tens of thousands of points. **The bit helpers
  should be written once, tested once, and then trusted** — and the test
  should be a bit-pattern comparison, not a numerical one.
* **No FMA means the polynomial has to be written the FDLIBM way**, with
  the minimax coefficients, and the summation orders that avoid
  cancellation. That is what the coefficients above are for.

### 7. How this was verified

The prototype was written in LLVM IR text, compiled with clang 23.1.3
(`-x ir -c -O2`), and linked with a C driver that reads double bit patterns
from standard input and writes the results back as bit patterns. The
reference is `mpmath` at 60 decimal digits, rounded to the nearest double.

The ULP error is `|got − reference| / ulp(reference)`, with the reference
being the correctly rounded double of the true value.

| Function | Points | Range | Max ULP |
| -------- | ------ | ----- | ------- |
| `sqrt`  | 4 000 | 1e-16 … 1e16 | **0** |
| `exp`   | 6 000 | −745 … 709 (including the subnormal range) | **1** |
| `exp`   | 4 000 | −1e-3 … 1e-3 | **1** |
| `exp`   | 20 000 | random in −700 … 700 | **1** |
| `log`   | 20 000 | 1e-300 … 1e300 (logarithmic) | **1** |
| `log`   | 8 000 | 0.5 … 2 | **1** |
| `log`   | 20 000 | random in 1e-5 … 1e5 | **1** |
| `expm1` | 8 000 | −40 … 40 | **1** |
| `expm1` | 4 000 | −1e-5 … 1e-5 | **0** |
| `expm1` | 20 000 | random in −30 … 30 | **1** |
| `log1p` | 8 000 | −0.9 … 1e6 | **1** |
| `log1p` | 4 000 | −1e-6 … 1e-6 | **0** |
| `log1p` | 20 000 | random in −0.99 … 1e3 | **1** |
| `tanh`  | 8 000 | −25 … 25 | **2** |
| `tanh`  | 20 000 | random in −20 … 20 | **2** |

The special cases were checked separately (the table in section 3 is the
measured output, not the intended one).

**What was not verified.** The prototype is not in the repository — this
deliverable is a research document and the task was explicitly "do not
change any code". The IR text lives outside the tree and is not part of
this pull request. `erf`, `erfc`, `pow`, `gamma`, `lgamma`, `sinh`, `cosh`,
`log2` and `log10` were **not** implemented or measured; for those, the
table gives the algorithm and the published accuracy of the FDLIBM
original, not a measurement. The `f32` versions were not considered at all:
every coefficient above would have to be re-derived for single precision,
and that is a separate piece of work.

## Risks

* **The transcription risk is the real risk.** Six bugs, five of them in
  bit plumbing, and one of them an eight-digit error that looked like
  rounding. The mitigation is the same in every case: a bit-pattern test
  for the helpers, and a comparison against a high-precision reference over
  a wide range for the functions. This is a test-infrastructure
  requirement, and it should be raised before the implementation starts.
* **The subnormal range is a separate code path** in `exp` and `log`, and
  it is the part that is most likely to be left out or got wrong. A
  framework that does not use subnormals will not notice; one that does
  (or a test that does) will.
* **A 1-ULP difference from the platform's `libm` is visible.** A test
  that compares against the C library will fail in the last bit. The
  tests have to compare against a pinned reference value or against a
  correctly rounded reference, not against `libm`.
* **`exp(1.0)` is 1 ULP high in this implementation** (2.7182818284590455
  against the correctly rounded 2.718281828459045). fdlibm special-cases
  `x == 1.0` to return the closer value; the prototype does not. It is
  worth deciding whether to keep the special case (it costs a compare and
  a branch, and it makes `exp(1.0)` the "nice" value) or to drop it (and
  then `exp(1.0)` is 1 ULP high like every other argument).
* **FMA.** If the target later gains `-march=x86-64-v3` (or the AArch64
  path is used), the polynomials can be rewritten with FMA for a lower
  error and fewer instructions. That is an optimisation for later, and the
  FDLIBM baseline must work without it.
* **The `f32` versions are a separate job.** Phase 11c needs `f32` as
  well. The single-precision polynomials are different coefficients with
  different reference ranges; they cannot be derived from the double ones
  by truncation. Budget for them separately, and consider generating both
  from one source of truth.
* **The licence of the coefficients.** The minimax coefficients are
  FDLIBM's. The FDLIBM notice permits use and modification provided the
  notice is preserved, and openlibm (which carries them) is ISC-licensed.
  Whether "the coefficients" or "the file" is the thing being copied is a
  question for whoever owns the licence review, and the answer decides
  whether the notice has to travel with the runtime.

## Open questions

1. **Does the framework need the exact GELU?** If yes, `erf` becomes a
   phase-11i item and it should be written *once*, carefully, with the
   asymptotic series (which are the part that is easy to get wrong) checked
   against a high-precision reference over the whole range. If no — and the
   tanh approximation is 1e-4 accurate, which is below any training
   signal — `erf` can be dropped entirely. **Recommendation: ask the
   framework's owner; assume "no" for now.**
2. **Do we want `pow` at all?** The FDLIBM `pow` is the largest single
   function in the set and it is not correctly rounded. A learning-rate
   schedule can be written as `exp(step·log(decay))`. **Recommendation:
   do not implement `pow` until something needs it; if something does,
   implement the FDLIBM one and document that it is not correctly
   rounded.**
3. **Should the standard library own `log2`/`log10`/`sinh`/`cosh`?** They
   are one multiply and one `exp` away from the runtime's `log`/`exp`, and
   they need no bit manipulation. **Recommendation: yes, the standard
   library — but only after the module system exists (there is no `import`
   of library modules yet, `docs/standard-library.md:4`), so in practice
   they are runtime IR functions that are simply not called by anything.**
4. **Should `exp(1.0)` be special-cased?** **Recommendation: no. Keep the
   general path; a special case for one argument is a wart, and the
   1-ULP difference is not worth a branch.**
5. **What is the reference for the tests?** Pinning a table of values in
   the test suite is cheap and deterministic, but it only pins the values
   that were thought of. A generated table — every function against a
   60-digit reference over a wide range, with the *bit patterns* pinned —
   catches the whole class of bugs above. **Recommendation: generate the
   table, pin the bit patterns, and regenerate it deliberately when an
   algorithm changes.**
6. **Who owns the FDLIBM notice?** **Recommendation: keep the FDLIBM
   copyright notice in the file that carries the coefficients, whatever
   that file turns out to be, and treat the question of whether the
   coefficients alone are a copyrightable derivative as one for the
   licence review rather than for the implementer.**
7. **Does the runtime need the `f32` variants at the same time?** Phase
   11c introduces `f32`. Every function above would be implemented twice,
   with different coefficients, and the argument reduction for `f32` is
   different (a 16-bit exponent, a 24-bit mantissa, and a reduction that
   has to be right to 2⁻²⁴ rather than 2⁻⁵³). **Recommendation: do the
   `f64` set first, in one go, and treat `f32` as a separate slice with
   its own coefficients and its own reference table.**
8. **Should the runtime expose `frexp`/`ldexp`/`scalbn` as well?** Every
   one of these functions needs them internally, and a program that wants
   to do its own numerics will want them too. **Recommendation: implement
   them as internal helpers now, and decide later whether to make them
   part of the public surface.**
