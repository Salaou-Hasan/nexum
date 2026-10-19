# Vectors and SIMD in the language

**Question.** How should phase 11f express data-parallel work in Nexum: what the
vector *type* is, how it is written, what operations it has, how each one maps
onto an LLVM vector instruction, how it is laid out in memory, how it interacts
with the collector, which intrinsics the language needs, how much of it is
target-dependent, whether it competes with LLVM's auto-vectoriser, and what has
to be built in which order.

**Summary.** The recommendation is a single new type constructor, `Vec<T, N>`: a
fixed-width vector of a *numeric scalar* (`T` is `Float`/`Int` today, plus
`f32` and the fixed-width integers once 11c lands), written `Vec<Float, 8>`, with
the lane count `N` part of the type and a power of two. Element-wise arithmetic
and comparisons are the ordinary operators; a comparison yields a *mask*, which
is the same constructor with `Bool` as the element type (`Vec<Bool, 8>`), because
LLVM's comparison result is `<N x i1>` and nothing else. The mapping onto LLVM
is one-to-one: `Vec<Float, 8>` is `<8 x double>`, `+` is `fadd`, `<` is
`fcmp olt`, a shuffle is `shufflevector`, a horizontal add is a halving
`shufflevector`+`fadd` tree the code generator emits *inline*. The runtime needs
to provide almost nothing: because the runtime is emitted as fixed LLVM IR text
it cannot be parameterised over the lane count, so a horizontal reduction is
emitted into the user's function rather than called, and the one helper that is
needed (`print` of a vector) takes a pointer and a count. The element type can
never be a managed reference, so a vector is never scanned by the collector. The
work is mostly independent of 11c: the type, the layout and every `f64`/`i64`
operation can be built now, and only the `f32`/`i32` element types wait for the
numeric tower. The one thing phase 11f must *not* do is fight the auto-vectoriser
— and the thing that currently fights it is the value semantics of arrays, not
the absence of explicit vectors.

## Recommendation

1. **One type constructor: `Vec<T, N>`.** `T` is a numeric scalar type
   (`Float` = f64, `Int` = i64 today; `f32` and the fixed-width integers after
   11c) or `Bool`. `N` is a compile-time integer literal, a power of two,
   `1 <= N <= 64`. The lane count is part of the type: `Vec<Float, 4>` and
   `Vec<Float, 8>` are different types and cannot be mixed.
   *Reason:* one constructor covers every element type the numeric tower will
   add, so the language does not need a name per (element type × width)
   combination — `F32x4`, `F32x8`, `F64x4`, … would be 35 names today and more
   later. Because the width is in the type, mixing widths is a compile-time
   error rather than a silent truncation, and the compiler can pick the exact
   LLVM type without a target table.

2. **Masks are `Vec<Bool, N>` — the same constructor, not a second one.** An
   element-wise comparison of two `Vec<T, N>` produces `Vec<Bool, N>`, and that
   is the only way to make one. `Vec<Bool, N>` accepts only `and`, `or`, `not`,
   `vec_select`, `vec_any` and `vec_all`; arithmetic on it is a diagnostic.
   *Reason:* LLVM's comparison result *is* `<N x i1>`; a second constructor
   (`Mask<T, N>`) would have an element type that is not the LLVM element type,
   and every function that today assumes `llvm_ty(elem)` is the element type
   would need a parallel path. One constructor keeps `llvm_ty` uniform:
   `Ty::Vector(elem, n) => "<{n} x {llvm_ty(elem)}>"` works for `<8 x double>`
   and `<8 x i1>` alike.

3. **The syntax is `Vec<Float, 8>`, and a vector is constructed by calling the
   type: `Vec<Float, 8>(1.0)`, `Vec<Float, 8>()`, `Vec<Float, 4>(1.0, 2.0, 3.0,
   4.0)`.** Zero arguments is the zero vector, one argument is a broadcast
   (splat), and `N` arguments are the lanes. The name `Vec` is reserved in the
   type namespace, like `Int`, `Float`, `Str` and `List`.
   *Reason:* the element type and the lane count have to be written *somewhere*
   the checker can see without inference, and the type position is the only
   place that already carries both. It also matches the language's existing
   "call a type name to build a value" style (`Vec2(1.0, 2.0)` builds a
   record). The alternative — free functions with type arguments, `vec_splat<F64,
   8>(x)` — cannot be parsed: `<` is `TokenKind::Lt`, the comparison operator,
   and in `parse_primary` the token after an identifier is genuinely ambiguous
   (`a < b` versus `f<T>(x)`), so a type-argument call syntax would need
   speculative backtracking and would change the meaning of existing code such
   as `a < b > (c)`.

4. **The operations are the ordinary operators plus a `vec_`-prefixed intrinsic
   set.** Element-wise `+ - *` (and `/` on float vectors) on two vectors of the
   same type; `== != < > <= >=` producing a mask; `and`/`or`/`not` on masks.
   The intrinsics: `vec_extract`, `vec_insert`, `vec_shuffle`, `vec_shuffle2`,
   `vec_select`, `vec_any`, `vec_all`, `vec_reduce_add`, `vec_reduce_min`,
   `vec_reduce_max`, `vec_fma`, `vec_from_array`, `vec_to_array`, and — after
   11d — `vec_load`, `vec_load_aligned`, `vec_store`, `vec_store_aligned`,
   `vec_gather`, `vec_scatter`. The broadcast is not an intrinsic: it is the
   vector literal with one argument, `Vec<Float, 8>(x)`.
   *Reason:* the element-wise operations are exactly what the operators already
   mean, so there is no reason to make the user call a function for `a + b`.
   Everything the operators *cannot* express (a shuffle, a horizontal add, a
   broadcast, an extract) needs a name, and `vec_` matches the existing
   `list_new`, `list_push`, `str_char_at` convention.

5. **The LLVM mapping is one instruction per operation**, and it is pinned in
   the table in "The design" below. `Vec<Float, 8>` is `<8 x double>`;
   `Vec<Int, 8>` is `<8 x i64>`; a mask is `<8 x i1>`; `+` is `fadd`/`add`;
   `<` is `fcmp olt`/`icmp slt`; `vec_select` is `select`; `vec_shuffle` is
   `shufflevector`; `vec_extract`/`vec_insert` are `extractelement`/
   `insertelement`.
   *Reason:* the whole point of the phase is that the programmer can see the
   machine. A one-to-one mapping means the emitted IR is readable and the
   performance is predictable, and it means the code generator has no policy to
   invent.

6. **The layout is: a vector occupies exactly `N × sizeof(T)` bytes with no
   padding, and its alignment is the target's ABI alignment for that vector
   type — the next power of two ≥ the size, capped at the target's maximum
   vector alignment (64 bytes on x86-64; 16 on AArch64, where a wide vector is
   split into 128-bit chunks).** Measured on
   x86-64: `<2 x double>` → 16 bytes / align 16; `<4 x i32>` → 16 / 16;
   `<8 x float>` → 32 / 32; `<8 x double>` → 64 / 64; `<16 x float>` → 64 / 64;
   `<8 x i1>` → 8 / 8 (one byte per lane, not one bit).
   The *language-level* guarantee is only the size and the element alignment;
   the stronger alignment is a target property that a program may rely on only
   through `vec_load_aligned`.
   *Reason:* the size is a hard property of the LLVM data layout for vector
   types and never changes. The alignment is not: it depends on the target, so
   the language must not promise it in the type — but the code generator must
   always emit an explicit `align` on a vector load/store/alloca so that the
   emitted IR does not depend on the datalayout string either.

7. **A vector holds no reference, and the element type cannot be one.** The
   allowed element types are the unmanaged scalars: a numeric type, `Bool`, and
   (after 11d) a raw `Ptr<T>` — a raw pointer is an unmanaged address, not a
   managed reference. `Str`, `List`, records and arrays are rejected at the
   point the vector type is formed, with a diagnostic. Because the element type
   is statically known to be unmanaged, `has_refs(Ty::Vector(..))` is `false`
   unconditionally, `scan_bytes` is 0, and a vector local is never registered
   as a root.
   *Reason:* the collector's question "does this type hold a reference?" has to
   have a compile-time answer. The restriction is in the *type*, not in a
   runtime check, so the answer is always "no" and the vector is a plain value
   type — passed and returned by value, copied on assignment, like an array.

8. **The language needs an explicit fused multiply-add, and it does *not* need
   a runtime helper for the horizontal reductions.** `vec_fma(a, b, c)` maps onto
   the LLVM `fma` instruction. A plain `a * b + c` maps onto `fmul` + `fadd`
   and is *not* contracted, because the language emits no fast-math flags and
   must not: an unfused multiply-add and a fused one round differently, and
   silently contracting would change the meaning of every float expression in
   the language. The horizontal reductions (`vec_reduce_add`, `vec_reduce_min`,
   `vec_reduce_max`) are emitted by the code generator as an inline halving
   tree of `shufflevector` + `fadd`/`fmin`/`fmax`, and need no runtime entry
   point at all.
   *Reason:* the runtime is emitted as fixed LLVM IR text
   (`crates/nexum-runtime/src/lib.rs:46`), so a runtime helper cannot be
   parameterised over the lane count: `nexum_hadd_f64_8` would have to exist
   once per width, and the width set is open. A helper whose signature does not
   mention the width (pointer + count) can only do the *memory* side of a
   reduction, not the arithmetic. Emitting the tree inline is both simpler and
   faster.

9. **The runtime provides exactly one thing for vectors: a width-independent
   printer.** `nexum_print_f64_n(ptr %elems, i64 %n)` and
   `nexum_print_i64_n(ptr %elems, i64 %n)`. The code generator spills the
   vector to a stack slot and passes the slot address and the lane count.
   *Reason:* the same "the IR text is fixed" constraint. A helper per width
   would be 7 helpers per element type; a pointer-and-count helper is one
   helper per element type, forever. (The alternative — expanding `print(v)`
   into a per-lane print loop in the code generator — also works and is the
   fallback if the orchestrator prefers not to grow the runtime.)

10. **The language does not fix a width per target, and never changes a
    width.** `Vec<Float, 4>` is four lanes on every target; on a target whose
    vector registers are 512 bits wide it is still four lanes. The target's
    register width (SSE2 128 bits = 2×f64/4×f32, AVX2 256 = 4×f64/8×f32,
    AVX-512 512 = 8×f64/16×f32, NEON 128 = 2×f64/4×f32) is a *performance*
    fact, not a semantic one.
    *Reason:* a language whose results depend on the machine is not a language
    with reproducible programs, and phase 11i (the ML framework) needs
    bit-identical results across the machines it runs on. A program that wants
    the widest vector must ask for it by name.

11. **Explicit vectors and the auto-vectoriser coexist; the phase must not
    interfere with the auto-vectoriser, and the thing that currently does
    interfere is the value semantics of arrays, not the absence of explicit
    vectors.** Verified on the current compiler: a Nexum loop that reads
    array elements into a scalar accumulator *is* auto-vectorised by
    `clang -O3` ("vectorized loop (vectorization width: 16, interleaved count:
    2)"), and a loop that writes an element of a *by-value* array binding is
    *not* ("value that could not be identified as reduction is used outside
    the loop"), because `xs[i] = v` is a functional update of the whole
    binding, so the code generator reloads and re-stores the entire array on
    every iteration. The same loop written over a `mut` array parameter — a
    borrow, so the write goes straight to the caller's storage — *is*
    vectorised.
    *Reason:* the auto-vectoriser is a backend pass over scalar loops; an
    explicit vector loop is not a scalar loop and is simply skipped, so there
    is no double-vectorisation and no conflict. The conflict that does exist is
    the opposite one: the compiler must not make the scalar path
    un-vectorisable, or the explicit vector type becomes the *only* way to get
    performance, which is a much worse language.

12. **The order of work, and the dependency on 11c.** 11f.0 (the type, the
    parser, the layout) → 11f.1 (the value operations) → 11f.2 (the memory
    operations, **needs 11d**) → 11f.3 (the `f32`/`i32` element types,
    **needs 11c**) → 11f.4 (the target story: `--target-cpu`, and the
    diagnostic for a width the target cannot hold in one register). 11f.0,
    11f.1 and 11f.4 are **independent of the numeric tower**: they use `Float`
    (f64) and `Int` (i64), which exist today. Only 11f.3 waits for 11c, and it
    waits only for the *names* 11c chooses — the design does not change if the
    f32 type ends up called `F32` rather than `Float32`.
    11f.2 additionally needs one piece of sema machinery that 11d will also
    want: an *expected type* threaded into expression analysis, so that a
    type-directed builtin such as `vec_load` can learn `T` and `N` from the
    context.

## What the code looks like today

Facts from the repository, with the line references the recommendation depends
on. The worktree is `C:\Nexum\work\simd-vectors` at commit `316f6c6`.

### The type system

* `Ty` is a flat enum with no type parameters
  (`crates/nexum-sema/src/lib.rs:14-32`): `Int`, `Float`, `Bool`, `Str`,
  `Unit`, `Record(String)`, `Array(Box<Ty>, usize)`, `List(Box<Ty>)`, `Never`,
  `Unknown`, `Error`. `Float` is f64; there is no f32 and no integer width
  (D015 in `docs/language-reference/decisions.md:222-231`).
* `Ty::name`, `Ty::is_numeric` and `Ty::compatible` are at
  `crates/nexum-sema/src/lib.rs:34-70`. `compatible` already has a structural
  case for arrays and lists — `(Ty::Array(a, n), Ty::Array(b, m)) => n == m &&
  a.compatible(b)` — which is the shape a vector needs.
* The parser's type expressions are `TypeExpr::Named(String)` and
  `TypeExpr::Array(Box<TypeExpr>, usize)`
  (`crates/nexum-parser/src/lib.rs:12-16`); `parse_type_expr`
  (`crates/nexum-parser/src/lib.rs:648-680`) handles an identifier or
  `[T; N]`. There is no generic-argument syntax anywhere.
* The lexer already has every token a vector type needs: `TokenKind::Lt` and
  `TokenKind::Gt` (`crates/nexum-lexer/src/lib.rs:53-54`), `Comma` (`:34`),
  `Int` (`:11`). **No lexer change is required.**
* Type resolution is `tty` (`crates/nexum-sema/src/lib.rs:537-555`): it maps the
  builtin names to `Ty` and falls back to the record table. Type *validation*
  is `ty_check` (`:557-590`), which today rejects zero-length arrays (T030) and
  non-value element types (T036).
* Binary operator typing is `binop_type` (`:1965-2054`). It requires both
  operands to be numeric and of the same type, requires `Bool` operands for
  `and`/`or`, and returns `Ty::Bool` for every comparison. The highest
  diagnostic code in use is `T049`; the highest parser code is `P015`.
* The sema runs the function bodies in a fixpoint loop
  (`crates/nexum-sema/src/lib.rs:323-388`, up to 65 passes) that refines
  `Unknown` parameter and return types from the call sites. **The bodies are
  re-analysed on every pass**, so a mechanism that depends on a type becoming
  known later (an "expected type" taken from the signature table) fits the
  existing design.

### The lowering

* `llvm_ty` (`crates/nexum-codegen/src/lib.rs:8-20`) maps `Ty` to an LLVM type
  string: `Int` → `i64`, `Float` → `double`, `Bool` → `i1`, `Str`/`List` →
  `ptr`, `Unit` → `void`, `Record(n)` → `%n`, `Array(e, n)` → `[n x llvm_ty(e)]`.
  A vector is a one-line addition: `Ty::Vector(e, n) => format!("<{} x {}>", n,
  llvm_ty(e))`.
* `has_refs` (`:22-34`) is the collector's question. It answers `true` for
  `Str`, `List` and any record/array that transitively contains one, and
  `false` for everything else. A vector must answer `false` *unconditionally*,
  which is only sound if the element type is statically restricted.
* `llvm_align` (`:36-53`), `llvm_size` (`:55-68`), `record_size` (`:70-81`) and
  `scan_bytes` (`:83-99`) are the layout functions. `record_size` already lays
  a field out with `off.div_ceil(a) * a` using the *field's* alignment, so a
  record with a vector field is positioned correctly once `llvm_align` knows
  the vector's alignment.
* `Instr::BinOp` is emitted at `crates/nexum-codegen/src/lib.rs:501-666`. The
  float cases are selected by `*ty == Ty::Float` and the integer cases fall
  through to `add`/`sub`/`mul`. The integer division case (`:547-614`) is
  written for `i64` specifically: it emits a zero-divisor test, a call to
  `nexum_panic`, and a `MIN / -1` guard, all with a hard-coded `i64`.
* The comparison cases are at `:615-662`.
* `print` is emitted at `:716-755` as a C `printf` call with a format global
  and a type-specific format string (`%lld`, `%g`, `%d`, `%s`).
* The builtin-to-runtime-symbol table is at `:834-880` (`concat` → `nexum_concat`,
  `list_new` → `nexum_list_new`, …). A vector builtin that needs no runtime
  entry point simply does not appear here.
* The MIR instruction set is `Instr` (`crates/nexum-mir/src/lib.rs:46-135`).
  Every instruction carries a `Ty` for its result, and the three verifiers
  walk the instruction list in four places (`:996`, `:1034`, `:1237`, `:1274`),
  so a new instruction has to be added to each.
* `TargetSpec` (`crates/nexum-target/src/lib.rs:4-9`) carries the triple, the
  pointer width, the OS and the architecture; `data_layout_str` (`:85-100`)
  gives the layout string. **The layout strings do not contain a vector
  alignment (`v64:…`) component**, so the ABI alignment of a vector type comes
  from LLVM's default (the next power of two ≥ the size, capped at the
  target's maximum vector alignment).

### Two facts that shape the design

* **Ordering comparisons on `Float` emit invalid IR.** `MBin::Lt`, `Gt`, `Le`
  and `Ge` (`crates/nexum-codegen/src/lib.rs:651-662`) always emit `icmp`,
  including on `double`; `MBin::Ne` (`:636-649`) does the same. Only `MBin::Eq`
  (`:615-635`) special-cases `Float` and emits `fcmp oeq`. Reproduced on the
  current tree:

  ```
  > nx build cmporder.nx
  LLVM IR verification failed:
  nexum_out.ll:627:18: error: icmp requires integer operands
    627 |   %v6 = icmp slt double %v4, %v5
  ```

  ```
  > nx build neq.nx
  LLVM IR verification failed:
  nexum_out.ll:627:17: error: icmp requires integer operands
    627 |   %v6 = icmp ne double %v4, %v5
  ```

  A vector comparison must use `fcmp oeq/one/olt/ogt/ole/oge`, and it must not
  inherit this. The scalar path needs the same fix independently of 11f; it is
  raised as open question 9 below.

* **The value semantics of arrays defeat the loop vectoriser.** `xs[i] = v` on
  a by-value binding is a functional update of the whole binding
  (`docs/language-reference/grammar.md:276-280`), so the code generator
  spills the aggregate, writes one element and reloads the whole array — on
  every iteration (`crates/nexum-codegen/src/lib.rs:1058-1118`). Measured on
  the current tree with `clang -O3 -Rpass=loop-vectorize`:

  | program | result |
  | ------- | ------ |
  | `while i < 8: s = s + xs[i]` (read only) | `vectorized loop (vectorization width: 16, interleaved count: 2)` |
  | `while i < 8: xs[i] = xs[i] * k` on a by-value `xs` | `loop not vectorized: value that could not be identified as reduction is used outside the loop` |
  | the same loop on a `mut xs: [Float; 8]` parameter | `vectorized loop (vectorization width: 16, interleaved count: 2)` |

  This is a language-design fact, not a codegen bug, and it is the one place
  where the phase must not make things worse.

## The design

### The type

```
Vec<T, N>          a fixed-width vector of N elements of type T
```

* `T` ∈ { `Float` (f64), `Int` (i64) } today; after 11c also `f32` and the
  fixed-width integer types. `Bool` is allowed as the element type **only** to
  form a mask.
* `N` is an integer literal, a power of two, `1 <= N <= 64`.
* `Vec<Bool, N>` is the mask type. It is produced only by a comparison. It
  accepts `and`, `or`, `not`, `vec_select`, `vec_any`, `vec_all` and nothing
  else.
* After 11d, `Ptr<T>` is also an allowed element type (a raw pointer is an
  unmanaged address), which is what makes `vec_gather` expressible.

The internal representation is one new variant:

```rust
// crates/nexum-sema/src/lib.rs
pub enum Ty {
    // ... existing variants ...
    /// Fixed-width vector: element type + lane count. The element type is
    /// always an unmanaged scalar, so a vector never holds a reference.
    Vector(Box<Ty>, usize),
}
```

`Ty::name()` → `format!("Vec<{}, {}>", elem.name(), n)`.
`Ty::compatible()` → `(Ty::Vector(a, n), Ty::Vector(b, m)) => n == m &&
a.compatible(b)`; a vector is never compatible with a scalar.

### The syntax

```
type-expr     = identifier          (Int | Float | Bool | Str | Unit | record name)
              | "[" type-expr ";" int-lit "]"   (fixed-size array)
              | "Vec" "<" type-expr "," int-lit ">"   (fixed-width vector)
```

```
Vec<Float, 8>          the type
Vec<Float, 8>()        the zero vector
Vec<Float, 8>(1.0)     a broadcast: eight lanes, all 1.0
Vec<Float, 4>(1.0, 2.0, 3.0, 4.0)   four lanes
```

The argument count rule is *0, 1, or N*; anything else is a diagnostic. For
`N == 1` the "one argument" and "N arguments" readings coincide.

Construction is a call to a type name, exactly like a record constructor
(`Vec2(1.0, 2.0)`), so the sema gets a new `HirExpr` case rather than a new
callable:

```rust
// crates/nexum-sema/src/lib.rs
HirExpr::VectorLit { elem: Ty, len: usize, args: Vec<HirExpr> }
```

and the parser gets a matching `ExprKind::VectorLit { elem: TypeExpr, len:
usize, args: Vec<Expr> }`.

### The operations and their LLVM mapping

With `T` = f64, `N` = 8, so the LLVM type is `<8 x double>`; the same table
applies to every element type and lane count.

| Nexum | LLVM IR | note |
| ----- | ------- | ---- |
| `Vec<Float, 8>` | `<8 x double>` | |
| `Vec<Int, 8>` | `<8 x i64>` | |
| `Vec<Bool, 8>` | `<8 x i1>` | the mask; one byte per lane in memory |
| `a + b` | `%r = fadd <8 x double> %a, %b` | `add` for integers |
| `a - b` | `%r = fsub <8 x double> %a, %b` | `sub` for integers |
| `a * b` | `%r = fmul <8 x double> %a, %b` | `mul` for integers |
| `a / b` (float) | `%r = fdiv <8 x double> %a, %b` | `frem` for `%` |
| `a == b` (float) | `%r = fcmp oeq <8 x double> %a, %b` | *ordered*: false if a NaN is involved |
| `a != b` (float) | `%r = fcmp one <8 x double> %a, %b` | |
| `a < b` (float) | `%r = fcmp olt <8 x double> %a, %b` | |
| `a <= b` (float) | `%r = fcmp ole <8 x double> %a, %b` | |
| `a > b` (float) | `%r = fcmp ogt <8 x double> %a, %b` | |
| `a >= b` (float) | `%r = fcmp oge <8 x double> %a, %b` | |
| `a == b` (integer) | `%r = icmp eq <8 x i64> %a, %b` | |
| `a < b` (integer) | `%r = icmp slt <8 x i64> %a, %b` | `slt`/`sgt`/`sle`/`sge` |
| `m1 and m2` | `%r = and <8 x i1> %m1, %m2` | |
| `m1 or m2` | `%r = or <8 x i1> %m1, %m2` | |
| `not m` | `%r = xor <8 x i1> %m, %all_true` | `%all_true` built by `insertelement`+`shufflevector` |
| `vec_select(m, a, b)` | `%r = select <8 x i1> %m, <8 x double> %a, <8 x double> %b` | |
| `Vec<Float, 8>(x)` | `%t = insertelement <8 x double> poison, double %x, i32 0`<br>`%r = shufflevector <8 x double> %t, <8 x double> poison, <8 x i32> zeroinitializer` | the broadcast |
| `vec_extract(v, 3)` | `%r = extractelement <8 x double> %v, i32 3` | the index must be a literal |
| `vec_insert(v, 3, x)` | `%r = insertelement <8 x double> %v, double %x, i32 3` | the index must be a literal |
| `vec_shuffle(v, [0,1,…])` | `%r = shufflevector <8 x double> %v, <8 x double> poison, <8 x i32> <0,1,…>` | indices in `[0, N)` |
| `vec_shuffle2(a, b, [..])` | `%r = shufflevector <8 x double> %a, <8 x double> %b, <8 x i32> <..>` | indices in `[0, 2N)` |
| `vec_any(m)` | the `or` tree below, then `extractelement` | |
| `vec_all(m)` | the `and` tree below, then `extractelement` | |
| `vec_reduce_add(v)` | the halving tree below | |
| `vec_reduce_min(v)` | the halving tree with `fcmp olt` + `select` | no `fmin` instruction exists |
| `vec_reduce_max(v)` | the halving tree with `fcmp ogt` + `select` | `smin`/`smax` exist for integers (LLVM 12+) |
| `vec_fma(a, b, c)` | `%r = fma <8 x double> %a, %b, %c` | a true fused multiply-add, one rounding |
| `vec_from_array(a)` | spill the `[8 x double]` and `load <8 x double>` | |
| `vec_to_array(v)` | spill the `<8 x double>` and `load [8 x double]` | |
| `vec_load(p)` | `%r = load <8 x double>, ptr %p, align 8` | the *element* alignment: always safe |
| `vec_load_aligned(p)` | `%r = load <8 x double>, ptr %p, align 64` | the target's vector alignment |
| `vec_store(v, p)` | `store <8 x double> %v, ptr %p, align 8` | |
| `vec_store_aligned(v, p)` | `store <8 x double> %v, ptr %p, align 64` | |
| `vec_gather(ptrs, mask)` | `%r = call <8 x double> @llvm.masked.gather.v8double.p0(<8 x ptr> %ptrs, i32 8, <8 x i1> %mask, <8 x double> %passthru)` | the intrinsic's exact name depends on the LLVM version (see the risk on the pinned toolchain) |

Notes on the mapping:

* The float comparison predicates are the **ordered** ones (`oeq`, `one`, `olt`,
  `ogt`, `ole`, `oge`), matching the scalar `Float` equality, which already
  uses `fcmp oeq` (`crates/nexum-codegen/src/lib.rs:624-628`). A NaN in either
  operand makes every ordered predicate false.
* `vec_reduce_add` is specified as a **tree** sum, not a left-to-right sum.
  The two differ in the last bits, and the tree order is fixed by the mask
  sequence below so that the result is reproducible. The language makes no
  promise today about the order of float addition, so this pins a rule that
  was previously unspecified; it is recorded as such in the decision entry.
* `vec_fma` uses the LLVM `fma` *instruction*, not the `llvm.fma` intrinsic
  call. The instruction is available from LLVM 12; on a target without an FMA
  unit the backend expands it to `fmul` + `fadd` (correct, but not fused).
* `vec_shuffle`'s and `vec_shuffle2`'s index arrays are **compile-time
  constants**: `[Int; N]` literals whose elements must be integer literals.
  A dynamic index is a diagnostic, because LLVM's `shufflevector` mask is
  always a constant.
* `vec_extract`'s and `vec_insert`'s indices must be integer literals for the
  same reason (`extractelement`/`insertelement` want a constant index to stay
  in a register).

### The reduction trees

The horizontal add of eight doubles, emitted inline by the code generator:

```llvm
  %v0 = fmul <8 x double> %a, %b          ; the caller's a * b
  %v1 = shufflevector <8 x double> %v0, <8 x double> poison, <8 x i32> <i32 4, i32 5, i32 6, i32 7, i32 0, i32 1, i32 2, i32 3>
  %v2 = fadd <8 x double> %v0, %v1
  %v3 = shufflevector <8 x double> %v2, <8 x double> poison, <8 x i32> <i32 2, i32 3, i32 0, i32 1, i32 4, i32 5, i32 6, i32 7>
  %v4 = fadd <8 x double> %v2, %v3
  %v5 = shufflevector <8 x double> %v4, <8 x double> poison, <8 x i32> <i32 1, i32 0, i32 2, i32 3, i32 4, i32 5, i32 6, i32 7>
  %v6 = fadd <8 x double> %v4, %v5
  %v7 = extractelement <8 x double> %v6, i32 0
```

For a general power-of-two `N` the tree is `log2(N)` steps. The mask for a
halving step from `m` lanes to `m/2` is `[m/2, …, m-1, 0, …, m/2-1]` for the
first `m` entries and the identity `[m, …, N-1]` for the rest, and the
addition is `add %v, %shuffled` — the first `m/2` lanes end up with the sum of
lane *i* and lane *i + m/2*.

`vec_any` is the same shape with `or` on `<N x i1>` and `vec_all` with `and`.
The tree is used rather than `bitcast <N x i1> to iN` because `i2` and `i4` do
not exist in LLVM, so the bitcast only works for `N` ∈ {8, 16, 32, 64}; the
tree works for every power of two and the backend recognises it (x86: `pmovmskb`
+ `test`; AArch64: `addv`/`cnt`).

### The layout

* **Size**: exactly `N × sizeof(T)`, with no padding. `<8 x double>` is 64
  bytes, `<8 x float>` is 32, `<4 x i32>` is 16, `<8 x i1>` is 8 (one byte per
  lane — a mask is *not* bit-packed in memory).
* **Alignment**: the target's ABI alignment for the vector type. Measured on
  x86-64 with clang 23:

  | LLVM type | size | alignment |
  | --------- | ---- | --------- |
  | `<2 x double>` | 16 | 16 |
  | `<4 x i32>` | 16 | 16 |
  | `<8 x float>` | 32 | 32 |
  | `<8 x double>` | 64 | 64 |
  | `<16 x float>` | 64 | 64 |
  | `<8 x i1>` | 8 | 8 |

  The rule is *the next power of two ≥ the size, capped at the target's maximum
  vector alignment*: 64 bytes on x86-64, and 16 bytes on AArch64 — there the
  backend splits a wide vector into 128-bit chunks (a `<16 x float>` is stored
  as four `str q` instructions), so the strong x86-64 alignment is not
  portable and the language must not promise it.
* **What the language promises**: the size, and that the alignment requirement
  is *at least* the element type's alignment. The stronger alignment is a
  target property; a program may rely on it only through `vec_load_aligned`.
* **What the code generator must do**: emit an explicit `align` on every
  vector `load`, `store` and `alloca`, so the emitted IR does not depend on the
  datalayout string (which has no `v64:…` component today,
  `crates/nexum-target/src/lib.rs:85-100`). A vector's alignment is then
  `min(next_pow2(N × elem_size), target.max_vector_alignment)`.
* **In a record**: `record_size` (`crates/nexum-codegen/src/lib.rs:70-81`)
  already positions each field at a multiple of the field's alignment, so a
  record with a `Vec<Float, 8>` field is 64-byte aligned and the field sits at
  offset 0 (or 64). A vector is a value type: `let y = x` copies it, a
  parameter is passed by value, and it can be returned by value.

### The collector

* The element type is restricted to **unmanaged** types: a numeric scalar,
  `Bool`, and (after 11d) a raw `Ptr<T>`. `Str`, `List`, records and arrays are
  rejected when the vector type is formed:

  ```
  T051: vector element type must be an unmanaged value type, found Str
  ```

  The rule is a property of the *type*, checked statically, so the collector's
  question always has a compile-time answer.
* `has_refs(Ty::Vector(..))` is therefore `false` unconditionally
  (`crates/nexum-codegen/src/lib.rs:22-34`), `scan_bytes` is 0 (`:83-99`), and
  a vector local is **never registered as a root** — the same treatment a
  numeric local gets today.
* A record that contains only vectors has no references either, so it is not a
  root.
* A vector is a plain value type, like an array: passed and returned by value,
  copied on assignment. There is no vector on the managed heap and nothing for
  the collector to do.

### The intrinsics

Free functions, `vec_`-prefixed, matching the existing `list_*` / `str_*`
convention.

| intrinsic | signature | needs |
| --------- | --------- | ----- |
| `vec_extract` | `(Vec<T, N>, literal) -> T` | |
| `vec_insert` | `(Vec<T, N>, literal, T) -> Vec<T, N>` | |
| `vec_shuffle` | `(Vec<T, N>, [Int; N]) -> Vec<T, N>` | |
| `vec_shuffle2` | `(Vec<T, N>, Vec<T, N>, [Int; N]) -> Vec<T, N>` | |
| `vec_select` | `(Vec<Bool, N>, Vec<T, N>, Vec<T, N>) -> Vec<T, N>` | |
| `vec_any` | `(Vec<Bool, N>) -> Bool` | |
| `vec_all` | `(Vec<Bool, N>) -> Bool` | |
| `vec_reduce_add` | `(Vec<T, N>) -> T` | |
| `vec_reduce_min` | `(Vec<T, N>) -> T` | |
| `vec_reduce_max` | `(Vec<T, N>) -> T` | |
| `vec_fma` | `(Vec<T, N>, Vec<T, N>, Vec<T, N>) -> Vec<T, N>` | |
| `vec_from_array` | `([T; N]) -> Vec<T, N>` | |
| `vec_to_array` | `(Vec<T, N>) -> [T; N]` | |
| `vec_load` | `(Ptr<T>) -> Vec<T, N>` | 11d (the type comes from the expected type) |
| `vec_load_aligned` | `(Ptr<T>) -> Vec<T, N>` | 11d |
| `vec_store` | `(Vec<T, N>, Ptr<T>)` | 11d |
| `vec_store_aligned` | `(Vec<T, N>, Ptr<T>)` | 11d |
| `vec_gather` | `(Vec<Ptr<T>, N>, Vec<Bool, N>) -> Vec<T, N>` | 11d |
| `vec_scatter` | `(Vec<T, N>, Vec<Ptr<T>, N>, Vec<Bool, N>)` | 11d |

The broadcast is spelled by the literal form `Vec<T, N>(x)`; a separate
`vec_splat` is not needed, and adding one would create a second way to say the
same thing.

`vec_load` is the one intrinsic whose type cannot be derived from its
arguments: the lane count and the element type come from the **expected type**
of the call. That means the sema needs an expected type threaded into
expression analysis (`analyze_expr(ctx, scope, e, expected: Option<&Ty>)`),
available at a `let` with an annotation, at a call argument whose parameter
type is known, at a `return` whose function return type is known, and at an
assignment whose target type is known. The sema's fixpoint
(`crates/nexum-sema/src/lib.rs:323-388`) re-analyses the bodies, so a type that
becomes known in a later pass reaches the call. Without an expected type,
`vec_load` is a diagnostic ("cannot infer the vector type of `vec_load`").

### The runtime

The runtime is emitted as fixed LLVM IR text
(`crates/nexum-runtime/src/lib.rs:46-239`, `crates/nexum-runtime/src/strings.rs`),
so a runtime entry point's signature cannot mention a lane count that is not
statically known to the runtime module. Consequences:

* **The reductions need no runtime helper.** `vec_reduce_add` and friends are
  emitted inline into the user's function as the shuffle tree above.
* **`vec_any` / `vec_all` need no runtime helper** — same reason.
* **The shuffles need no runtime helper** — they are one `shufflevector`.
* **The loads and stores need no runtime helper** — they are one `load`/`store`.
* **The gather needs no runtime helper** — it is an `llvm.masked.gather` call.
* **One helper is needed: printing a vector.** `nexum_print_f64_n(ptr %elems,
  i64 %n)` and `nexum_print_i64_n(ptr %elems, i64 %n)`, in the runtime module,
  with a pointer and a count so that one helper serves every lane count. The
  code generator spills the vector to a stack slot and passes the slot address
  and the lane count. (The alternative is to expand `print(v)` in the code
  generator into a per-lane print loop; the helper is simpler and keeps the
  formatting in one place. Either is acceptable; the helper is what this
  document recommends.)

So the runtime's contribution to 11f is two functions and no more.

### Target dependence

| target | register width | f64 lanes | f32 lanes |
| ------ | -------------- | --------- | --------- |
| SSE2 (x86-64 baseline) | 128 bits | 2 | 4 |
| AVX2 | 256 bits | 4 | 8 |
| AVX-512 | 512 bits | 8 | 16 |
| NEON (AArch64) | 128 bits | 2 | 4 |

* **The language fixes no width.** The program chooses the width, in the type.
  `Vec<Float, 4>` is four lanes everywhere; on AVX-512 it still is four lanes
  and the compiler will not widen it.
* **The compiler never changes a width.** The emitted LLVM type is exactly
  `<N x T>`, whatever the target.
* **The target's width is a performance fact.** On a 128-bit target a
  `Vec<Float, 8>` is two register operations; on a 512-bit target it is one.
* **A program that wants the target's natural width has to ask.** There is no
  `vec_native_width()` in v1; when one is added it must be a *value*, never a
  change to a type's meaning. (A compile-time constant chosen by the target
  would make the program non-reproducible across targets, which is exactly what
  11i cannot tolerate.)
* **Scalable vectors (ARM SVE) are out of scope.** LLVM's `<vscale x 4 x
  float>` has no fixed size, and the size guarantee above would be lost.
* 11f.4 adds `--target-cpu` to the driver, so that the target's feature set
  (AVX2, AVX-512, NEON) reaches the LLVM backend, and optionally a warning
  when a vector is wider than the target's register.

### The interaction with auto-vectorisation

* **They do not conflict.** LLVM's loop vectoriser runs in the backend over
  *scalar* loops; an explicit vector loop is not a scalar loop, so it is
  skipped. There is no double-vectorisation and no interaction with the SLP
  vectoriser beyond it vectorising the straight-line scalar code next to
  explicit vector code, which is harmless.
* **The auto-vectoriser works today for the common patterns.** Verified with
  `clang -O3 -Rpass=loop-vectorize` on the IR the current compiler emits:

  | program | result |
  | ------- | ------ |
  | read-only accumulation over an array | `vectorized loop (vectorization width: 16, interleaved count: 2)` |
  | a dynamic index into an array | `vectorized loop (vectorization width: 16, interleaved count: 2)` |
  | a write to an element of a by-value array binding | `loop not vectorized: value that could not be identified as reduction is used outside the loop` |
  | the same write through a `mut` array parameter | `vectorized loop (vectorization width: 16, interleaved count: 2)` |

* **The per-element bounds check is not *always* the blocker.** The check that
  the code generator emits (`crates/nexum-codegen/src/lib.rs:1040-1049`) branches
  to a call to `nexum_panic`, which is a side effect the vectoriser cannot
  speculate. In the constant-length case the check is provably redundant inside
  the loop and folds away, and the loop vectorises; when it does not fold, it
  can block. A hand-written control isolates the mechanism: the same array-sum
  loop, with and without the check-and-panic branch, vectorises without it
  (`vectorized loop (vectorization width: 2, interleaved count: 2)`) and does
  not with it. The practical consequence for 11f is that the bounds check
  should be hoisted out of the loop where the compiler can prove it, not left
  as a per-element branch — and that a phase that adds explicit vectors must
  not make the check worse.
* **What *is* the blocker is the value semantics of arrays.** `xs[i] = v` on a
  by-value binding reloads and re-stores the whole array on every iteration,
  and the vectoriser cannot reason about that. This is a language fact (D005:
  arrays are value types) and it is fixed in 11d's layout work, not in 11f.
* **The rule for 11f: an explicit vector is an additional tool, not the only
  one.** The phase must not make the scalar path slower or un-vectorisable,
  and the compiler's diagnostics should not push a program towards explicit
  vectors where the auto-vectoriser already does the job.

### The order of work

| step | what | depends on |
| ---- | ---- | ----------- |
| **11f.0** | the type: `Ty::Vector`, `TypeExpr::Vector`, the parser rule, `llvm_ty`, `llvm_align`, `llvm_size`, `has_refs`, `scan_bytes`, the record layout, the diagnostics | nothing |
| **11f.1** | the value operations: the element-wise arithmetic, the comparisons and the masks, `and`/`or`/`not` on masks, `vec_select`, `vec_any`, `vec_all`, `vec_extract`, `vec_insert`, `vec_shuffle`, `vec_shuffle2`, `vec_reduce_add`/`min`/`max`, `vec_fma`, `vec_from_array`, `vec_to_array`, the vector literal | 11f.0 |
| **11f.1.1** | integer vector `/` and `%` (deferred: see the risks) | 11f.1 |
| **11f.2** | the memory operations: `vec_load`, `vec_load_aligned`, `vec_store`, `vec_store_aligned`, `vec_gather`, `vec_scatter`; the expected-type mechanism in the sema | **11d** (pointers) |
| **11f.3** | the other element types: `f32`, `i8`, `i16`, `i32` | **11c** (the numeric tower) |
| **11f.4** | the target story: `--target-cpu`, the feature set, the diagnostic for a width the target cannot hold in one register | 11f.0 |

**The dependency on 11c is small and late.** 11f.0, 11f.1 and 11f.4 use
`Float` (f64) and `Int` (i64), which exist today; they do not need a single
new numeric type. 11f.3 adds the other element types and needs only the *names*
11c chooses — the design is unaffected by whether the f32 type ends up called
`F32`, `Float32` or `f32`. The numeric tower must add `Ty::F32` (or its
equivalent) to `Ty` and `float` to `llvm_ty`; the vector work then inherits it
through `Ty::Vector`.

**The dependency on 11d is real but confined to 11f.2.** Everything in 11f.0
and 11f.1 is a value-to-value operation and needs no memory model. The
pointer-based loads and stores cannot be written before 11d gives the language
a pointer type; the array conversions (`vec_from_array`, `vec_to_array`) are
the pointer-free stand-in and land in 11f.1.

### The per-crate changes, with the interfaces pinned

**`crates/nexum-lexer`** — no change. `Lt`, `Gt`, `Comma` and `Int` already
exist (`crates/nexum-lexer/src/lib.rs:11`, `:34`, `:53-54`).

**`crates/nexum-parser`**

```rust
// crates/nexum-parser/src/lib.rs
pub enum TypeExpr {
    Named(String),
    Array(Box<TypeExpr>, usize),
    /// Fixed-width vector: element type + lane count.
    Vector(Box<TypeExpr>, usize),
}

pub enum ExprKind {
    // ...
    /// A vector literal: `Vec<Float, 4>(1.0, 2.0, 3.0, 4.0)`.
    VectorLit { elem: TypeExpr, len: usize, args: Vec<Expr> },
}
```

* `parse_type_expr` (`crates/nexum-parser/src/lib.rs:648-680`): after an
  identifier, if the text is `Vec` and the next token is `Lt`, parse
  `< type-expr , int-lit >`. New diagnostics: `P016` "expected `<` after
  `Vec`", `P017` "expected `,` in a vector type", `P018` "expected a lane
  count in a vector type", `P019` "expected `>` to close a vector type".
* `parse_primary`: when the identifier is `Vec` and the next token is `Lt`,
  parse the vector type expression and then the argument list, and produce
  `ExprKind::VectorLit`.
* `normalize.rs`: the `type_expr` and `expr` printers gain the two new
  variants — the normalised form is the parser-level contract with the
  self-hosted parser (`crates/nexum-parser/src/normalize.rs:54-64`, `:66-105`).

**`crates/nexum-sema`**

```rust
// crates/nexum-sema/src/lib.rs
pub enum Ty {
    // ...
    Vector(Box<Ty>, usize),
}
```

* `Ty::name` → `format!("Vec<{}, {}>", elem.name(), n)`.
* `Ty::compatible` → vectors are compatible iff the lane counts match and the
  element types are compatible.
* `fty_name` (`:509-514`) and `tty` (`:537-555`) gain the vector case.
* `ty_check` (`:557-590`) gains the vector case, with the new diagnostics:
  * `T050` — the lane count must be a power of two between 1 and 64.
  * `T051` — the element type must be an unmanaged value type (a numeric
    scalar, `Bool`, or — after 11d — `Ptr<T>`); `Str`, `List`, records and
    arrays are rejected.
* `binop_type` (`:1965-2054`) gains the vector cases:
  * arithmetic: two vectors of the same type → that vector type; a scalar and
    a vector mixed → `T052` "cannot mix a scalar and a vector in an arithmetic
    operation" (there is no implicit broadcast — D004 forbids implicit
    conversions).
  * comparisons: two vectors of the same type → `Ty::Vector(Bool, n)`.
  * `and`/`or`: `Bool`, or two masks of the same width; `not`: `Bool` or a
    mask.
  * arithmetic on a mask → `T053` "a mask supports only `and`, `or` and `not`".
* `analyze_expr` (`:903-...`) gains the `ExprKind::VectorLit` case: the
  argument count must be 0, 1 or `N` (`T054`), the element type must be the
  declared one, and the result type is `Ty::Vector(elem, n)`.
* The builtins `vec_extract`, `vec_insert`, `vec_shuffle`, `vec_shuffle2`,
  `vec_select`, `vec_any`, `vec_all`, `vec_reduce_add`, `vec_reduce_min`,
  `vec_reduce_max`, `vec_fma`, `vec_from_array`, `vec_to_array` are checked in
  `analyze_expr` alongside the existing builtins (`panic` at `:1168`,
  `str_char_at` at `:1184`, `concat` at `:1502`, `len` at `:1561`): arity and
  argument types are checked with one diagnostic code per intrinsic
  (`T055`…`T067`), and the *literal* requirement for the shuffle and extract
  indices is checked here (`T056` "the index of `vec_extract` must be a
  literal").
* `HirExpr` gains `VectorLit { elem: Ty, len: usize, args: Vec<HirExpr> }`.
* For 11f.2: `analyze_expr` grows an `expected: Option<&Ty>` parameter, and
  `vec_load`/`vec_load_aligned` read `T` and `N` from it (`T068` "cannot infer
  the vector type of `vec_load`" when it is absent).

**`crates/nexum-mir`**

```rust
// crates/nexum-mir/src/lib.rs
pub enum Instr {
    // ...
    /// A vector value built from its lanes.
    VectorLit { dest: Value, args: Vec<Value>, ty: Ty },
    /// A broadcast of one scalar into every lane.
    Splat { dest: Value, value: Value, ty: Ty },
    /// `extractelement`; the index is a compile-time constant.
    ExtractElem { dest: Value, vec: Value, index: u32, ty: Ty },
    /// `insertelement`; the index is a compile-time constant.
    InsertElem { dest: Value, vec: Value, index: u32, value: Value, ty: Ty },
    /// `shufflevector`; the mask is a compile-time constant.
    Shuffle { dest: Value, a: Value, b: Value, mask: Vec<u32>, ty: Ty },
}
```

The constant indices and masks are fields of the instruction, not SSA values,
because LLVM's `extractelement`, `insertelement` and `shufflevector` want
constants. The three verifiers that walk the instruction list (at
`crates/nexum-mir/src/lib.rs:996`, `:1034`, `:1237` and `:1274`) must learn the
five new cases.

**`crates/nexum-codegen`**

* `llvm_ty` (`:8-20`): `Ty::Vector(elem, n) => format!("<{} x {}>", n,
  llvm_ty(elem))`.
* `has_refs` (`:22-34`): `Ty::Vector(..) => false`.
* `llvm_align` (`:36-53`): `Ty::Vector(elem, n) => vec_align(elem, n)`, where
  `vec_align` is `min(next_pow2(n × elem_size), target.max_vector_alignment)`.
  The function needs the target, so it grows a parameter (or the target's
  alignment is threaded through `records`).
* `llvm_size` (`:55-68`): `Ty::Vector(elem, n) => n × llvm_size(elem)`.
* `scan_bytes` (`:83-99`): `Ty::Vector(..) => 0`.
* `Instr::BinOp` (`:501-666`): the vector cases. The float cases are selected
  by the *element* type, not by `ty == Ty::Float`; the integer division case
  (`:547-614`) is rewritten so that it is not hard-coded to `i64`, and the
  zero-divisor panic is not used for a vector (see the risks).
* The new instructions get emitters, and the reduction emitters produce the
  shuffle trees inline.
* `print` (`:716-755`): a vector argument spills the vector to a slot and calls
  `nexum_print_f64_n`/`nexum_print_i64_n` with the slot address and the lane
  count.
* `emit` (`:109-175`) gains the target's maximum vector alignment.

**`crates/nexum-target`**

```rust
// crates/nexum-target/src/lib.rs
impl TargetSpec {
    /// The largest alignment a vector value may have on this target:
    /// 64 bytes on x86-64, 16 bytes on AArch64.
    pub fn max_vector_alignment(&self) -> u64 { /* by arch */ }
}
```

**`crates/nexum-runtime`**

* Two new entry points in the runtime module, with a width-independent
  signature:

  ```llvm
  define internal void @nexum_print_f64_n(ptr %elems, i64 %n) { ... }
  define internal void @nexum_print_i64_n(ptr %elems, i64 %n) { ... }
  ```

  Nothing else. In particular there is no helper for the reductions, the
  shuffles, the masks or the loads and stores.

**`crates/nexum-driver`** — no change for 11f.0/11f.1. 11f.2 needs the
pointer support from 11d; 11f.4 adds `--target-cpu`.

**`docs/`**

* `docs/language-reference/decisions.md`: **D017** — "the vector type is a
  fixed-width vector of an unmanaged scalar".
* `docs/language-reference/grammar.md`: the `type-expr` rule, the vector
  literal in the expression grammar, the builtin list, the diagnostic table
  (T050…T068, P016…P019) and the lowering notes.
* `docs/runtime-abi.md`: the native value table (§1) gains the vector row and
  the layout rules.

## Risks

* **The recommendation is wrong if the lane count must be target-dependent.**
  If the orchestrator decides that a vector's width should follow the target
  (the "portable SIMD" style), then the type `Vec<Float, 4>` would have a
  different meaning on different machines, every float expression would become
  machine-dependent, and phase 11i would be unable to reproduce a result
  across machines. The recommendation would then have to be inverted, and the
  type would have to become a *shape* rather than a type — which is a much
  larger change, because the sema, the collector and the ABI all key on the
  type. This is the single decision that most changes the work.
* **The recommendation is wrong if the runtime is going to grow a native
  intrinsic library.** If the runtime is later given hand-written
  target-specific kernels (`nexum_hadd_f64_8` and friends), the "emit the
  reduction inline" recommendation becomes unnecessary — but not wrong: the
  inline tree is what LLVM lowers to `haddps`/`addv` anyway, and a hand-written
  kernel would be *worse* on a target whose instruction set differs. The
  recommendation would only change for the reductions that a target has a
  dedicated instruction for and LLVM does not already use.
* **A tree sum is not a sequential sum.** `vec_reduce_add` sums in a fixed
  tree order, so its result can differ in the last bits from
  `((a0+a1)+a2)+…`. The language has never specified the order of float
  addition, so this is a new rule rather than a change, but a program that
  compares against a hand-computed sequential sum may be surprised. If the
  orchestrator wants the sequential order instead, the reduction cannot be a
  shuffle tree; it would have to be a chain of `extractelement`s and scalar
  adds, which is much slower and defeats the purpose. The recommendation is
  the tree, with the order documented.
* **Integer vector division is the awkward operation.** The scalar `Int`
  division panics on a zero divisor and wraps on `MIN / -1`
  (`crates/nexum-codegen/src/lib.rs:547-614`). A vector cannot panic per
  lane, so either the whole vector operation panics (a branch, which hurts the
  vectorised code) or the semantics diverge from the scalar ones. The
  recommendation defers integer vector `/` and `%` to 11f.1.1, where the rule
  is pinned: **the operation panics if *any* lane's divisor is zero, and
  `MIN / -1` wraps to `MIN` in every lane** — the same rule as the scalar
  path, applied to the whole vector. The implementation computes a zero
  divisor mask, tests it with `vec_any`, branches to the panic, and guards the
  division with a `select` so that no lane is ever a poison. If the
  orchestrator prefers a different rule (a defined result, no panic), that is
  a decision to take before 11f.1.1, not after.
* **The power-of-two restriction on `N` may be too strict.** `Vec<Float, 3>`
  is a legal LLVM type and a program might want it. The restriction exists so
  that the reduction trees, the shuffle masks and the alignment story are
  uniform. Relaxing it later is possible but would make the reduction code
  handle a ragged tail, which is why it is better to decide now.
* **`Vec<Bool, N>` occupies one byte per lane.** A mask of 64 lanes is 64
  bytes, not 8. A program that expects a bit-packed mask will be wrong. The
  alternative — a bit-packed mask type — cannot feed LLVM's `select` without
  extra work, so the byte-per-lane layout is the right trade, but it must be
  documented.
* **The toolchain's LLVM version is not pinned.** Two details depend on it:
  the exact name of the gather intrinsic (`llvm.masked.gather.v8double.p0` in
  LLVM 14 and later, `llvm.masked.gather.v8double.v8p0double` before that), and
  whether the `fma` *instruction* (LLVM 12+) is available. The implementer
  must check both against the pinned LLVM rather than against this document.
* **The scalar `Float` comparison bug will be discovered by anyone who writes
  a vector comparison test next to a scalar one.** The vector path must use
  `fcmp`; the scalar path (`crates/nexum-codegen/src/lib.rs:636-662`) must be
  fixed separately, and the two must not be "fixed" by a shared helper that
  keeps the old behaviour.
* **The `expected` type machinery is a change to the sema's shape.** It is a
  parameter added to `analyze_expr` and threaded through every call site that
  knows a type. It is a bounded change, but it touches the hottest function in
  the compiler, and 11d will want it too. It should be done once, in 11f.2
  (or in 11d, whichever comes first), not twice.

## Open questions

1. **Should the lane count be part of the type, or should the type be a
   "shape" that the target fills in?** *Recommendation: part of the type
   (recommendation 1).* The shape approach makes every float result
   machine-dependent and blocks 11i. Deciding this first changes everything
   else.
2. **Is `Vec` the right name?** *Recommendation: yes.* It is the conventional
   name, it is not a keyword today, and it is distinct from the runtime's
   `List` (which is the growable collection). The alternative is `Vector`, or a
   `simd_` prefix. The name `Vec` is reserved in the type namespace.
3. **Should the syntax be `Vec<Float, 8>` (angle brackets) or `Vec[Float, 8]`
   (square brackets)?** *Recommendation: angle brackets.* Square brackets are
   already the array type's brackets (`[Float; 8]`) and the index bracket, and
   `Vec[Float, 8]` reads like an index into a `Vec`; `Vec<Float, 8>` also
   nests cleanly (`Vec<Vec<Float, 4>, 2>`) and matches the "type parameters"
   convention readers expect. The cost: `Vec` becomes the one type in the
   language whose name is followed by `<`, and the parser must recognise it
   in exactly two positions (a type expression and a literal).
4. **Should the mask be `Vec<Bool, N>` or a separate `Mask<T, N>`?**
   *Recommendation: `Vec<Bool, N>`* (recommendation 2). A separate constructor
   would have an element type that is not the LLVM element type, which breaks
   the uniformity of `llvm_ty` and adds a second layout path.
5. **Should the vector literal take 0, 1 or N arguments?** *Recommendation:
   yes* — 0 is the zero vector, 1 is a broadcast, N are the lanes. The
   alternative is to require exactly N and spell the broadcast as a separate
   intrinsic, which needs the expected type and is therefore more machinery
   for no benefit.
6. **Should the lane count be restricted to powers of two?** *Recommendation:
   yes for v1* (recommendation 1). If the orchestrator wants `Vec<Float, 3>`,
   the reduction trees need a ragged tail and the alignment story stops being
   "the size is a power of two".
7. **What should `print` do for a vector?** *Recommendation: print the lanes
   in order, separated by spaces, through a width-independent runtime helper
   (recommendation 9).* The alternative — a per-lane print loop in the code
   generator — is equally acceptable and needs no runtime change.
8. **Should the semantic of a vector comparison be "ordered" (false when a NaN
   is involved) or "unordered"?** *Recommendation: ordered* (`fcmp olt` and
   friends), matching the scalar `Float` equality, which already uses
   `fcmp oeq`. Unordered predicates would need a separate spelling
   (`vec_cmp_unordered`) and would surprise anyone reading `a < b`.
9. **Should the phase fix the scalar `Float` comparison bug, or file it
   separately?** *Recommendation: file it separately, and fix it in the same
   week.* The bug is in `crates/nexum-codegen/src/lib.rs:636-662` and makes
   `a != b`, `a < b`, `a > b`, `a <= b` and `a >= b` on `Float` produce IR
   that LLVM rejects. It is not a vector problem, but the vector work will
   produce tests that trip over it immediately.
10. **Should 11f wait for 11c and 11d, or start now?** *Recommendation: start
    now.* 11f.0, 11f.1 and 11f.4 need neither 11c nor 11d; 11f.2 needs 11d and
    11f.3 needs 11c. Starting now means the vector type exists and is
    exercised (and the numeric tower lands into a type that already has a
    vector form).
11. **Who fixes the value semantics of arrays that defeat the vectoriser?**
    *Recommendation: 11d, not 11f.* The functional update of a by-value array
    is a language rule (D005), and the fix is a layout/lowering change in the
    11d work. 11f should only make sure it does not make the problem worse,
    and should record the measurement in the decision entry so the orchestrator
    can see it is a real, measured effect and not a guess.
12. **Should the target's native width be queryable?** *Recommendation: not in
    v1.* When it is added it must be a *value* (`vec_native_lanes() -> Int`) and
    never a change to a type's meaning. A compile-time constant that depends
    on the target would make a program's results machine-dependent.
13. **Should there be a `vec_mask`-only spelling of `and`/`or`?** *Recommendation:
    no.* The ordinary `and`/`or` operators already do not short-circuit (a
    house rule), which is exactly the right semantics for a bitwise mask
    operation, and adding a second spelling would fragment the language.
