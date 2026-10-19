# Where a new primitive type has to thread through the compiler

**Question.** Every place in the Nexum compiler where a new primitive type
(`f32`) has to be threaded through: every enum variant, struct field and
match arm that lists the existing primitive types, where the type
representation lives and how it maps to LLVM, where the parser and the
grammar distinguish number kinds, how `print` and the other builtins dispatch
on the argument type, how the collector treats a float, where the normalised
AST form lives, and which fixtures and tests would need a new case.

**Summary.** The recommendation is to land the f32 work as five ordered
slices (the ordering and the parallelism are tabulated at the end of the
Recommendation): **(A)** fix the float comparison bug that already exists in
the `f64` path — it is a separate, self-contained change and it must come
first; **(B)** add `Ty::F32` to the semantic analyser *together with* the
codegen's LLVM mapping, size and alignment, because the mapping's catch-all
would silently compile an unmapped type as `i64`; **(C)** add the MIR
constant, the type-aware constant folder and the `print` path (which needs
an explicit `fpext`); **(D)** add the literal syntax, if the orchestrator
wants one — it is the only slice that needs the lexer, the parser, the
normalised form, the self-hosted lexer and the self-hosted parser all at
once, and the recommendation is to defer it by introducing a `f32(x)`
conversion builtin instead, which needs none of them; **(E)** the fixtures,
the tests and the documentation. The type touches **4 Rust source files in 4
crates**, **4 documentation or editor artefacts** and the fixture
directories; `nexum-source` and `nexum-runtime` need **no change at all**
(the runtime is type-agnostic, and the only contract that touches a number
is the element size and scan size the compiler hands to `nexum_list_push`).
Four pre-existing bugs in the `f64` path were found on the way and are
documented in §13; the most important is that float ordering comparisons and
`!=` currently emit **invalid LLVM IR** (`icmp slt double %a, %b`), which no
fixture covers.

---

## Recommendation

1. **Fix the float comparison bug first, on its own, before any f32 work.**
   `crates/nexum-codegen/src/lib.rs:636-662` has no float case for `Ne`,
   `Lt`, `Gt`, `Le`, `Ge`; they emit `icmp` on the operand type, which is
   invalid IR for a float. Verified with clang: `error: icmp requires
   integer operands`. This is a bug in the existing `f64` path, it is
   invisible to the test suite (no fixture compares two floats), and the f32
   comparison path inherits it. It is a self-contained change in one file and
   it can be done in parallel with everything else.
   *Reason:* it is a prerequisite for the f32 comparison path being correct,
   and it is cheap enough that it should not wait for the f32 design to be
   settled.

2. **Decide the surface syntax before writing code.** The recommendation is
   to make `F32` a **type name** (`let x: F32 = ...`) and to introduce the
   value through a **conversion builtin** `f32(x)`, and to defer the literal
   suffix (`1.5f32`) to a later slice. With a conversion builtin in place, a
   literal suffix is pure sugar and costs nothing extra later.
   *Reason:* the suffix needs the lexer, the Rust parser AST, the normalised
   form, the self-hosted lexer and the self-hosted parser changed together;
   the conversion builtin needs **none** of them, because `f32(1.5)` is
   already a `Call` node in the AST (`crates/nexum-parser/src/lib.rs:1103-1121`)
   and already appears in the normalised form. The conversion builtin also
   gives the language its first explicit numeric conversion, which the tower
   needs anyway (D015 says the arithmetic semantics are spelled out per
   operation).

3. **Land `Ty::F32` and the codegen mapping in one merge.** `Ty` lives in
   `crates/nexum-sema/src/lib.rs:15-32` and the LLVM mapping in
   `crates/nexum-codegen/src/lib.rs:8-20`. The mapping's catch-all arm
   (`_ => "i64"`, line 18) means a `Ty::F32` that reaches codegen without a
   mapping arm is **silently** compiled as `i64`.
   *Reason:* a half-landed type is worse than no type; the two changes are
   ten lines each and belong in one commit.

4. **Land the MIR constant and the `print` path with the type.**
   `Const` (`crates/nexum-mir/src/lib.rs:15-21`) carries no type tag, so an
   f32 constant needs either a new `Const::F32(f32)` variant or a
   type-aware folder. `print` needs an `fpext float to double` before the
   `printf` call, because a raw `float` passed to a variadic function is
   read as a `double` by `printf` and prints garbage (verified: `1.5`
   prints as `5.28427e-315`).
   *Reason:* both are correctness requirements of the type, not polish.

5. **Land the literal syntax last, as its own slice.** If the orchestrator
   chooses the suffix, the slice touches the Rust lexer
   (`crates/nexum-lexer/src/lib.rs:303-329`), the self-hosted lexer
   (`examples/selfhost/lexer_lib.nx:254-282`), the Rust parser AST and
   `parse_primary` (`crates/nexum-parser/src/lib.rs:48-76`, `1077-1089`), the
   normalised form (`crates/nexum-parser/src/normalize.rs:69-70`), the
   self-hosted parser (`examples/selfhost/parser.nx:251-270`), and the
   formatter (`crates/nexum-driver/src/main.rs:587-596`).
   *Reason:* it is the only slice with a cross-crate contract (the
   differential lexer and parser tests compare the Rust and the self-hosted
   implementations token by token and line by line), so it deserves its own
   reviewable unit of work.

6. **Get the size and the alignment right, or `List[F32]` corrupts memory.**
   `llvm_size` (`crates/nexum-codegen/src/lib.rs:55-68`) and `llvm_align`
   (`:36-53`) both default to `8` for an unknown type. The element size is
   what the compiler hands to `nexum_list_push` as the **stride** of the
   buffer (`:779`), and the runtime uses it both as the byte offset
   (`off = len * esize`) and as the `memcpy` length
   (`crates/nexum-runtime/src/lib.rs:224`). An f32 element with a size of 8
   would give a buffer with twice the stride and a `memcpy` that reads past
   the element. The same two functions drive `record_size` (`:70-81`), which
   must agree with LLVM's own struct layout or the collector scans the wrong
   number of bytes.
   *Reason:* this is the one place where a missing line is a memory-safety
   bug rather than a wrong answer.

7. **Replace the "is it `Float`?" tests with a predicate.** Six places ask
   `ty == Ty::Float` to decide whether to emit a floating-point instruction
   (`crates/nexum-codegen/src/lib.rs:525`, `534`, `541`, `548`, `624`,
   `681`) and five more hardcode the word `double` in the emitted text
   (`:482`, `:555`, `:627`, `:682`, `:1215`). They should ask
   `ty.is_float()` and use `llvm_ty(ty)`.
   *Reason:* the f64 special case is the pattern the task asks about; it is
   spread over eleven lines in five different shapes.

8. **Fix the arithmetic result-type rule at the same time.**
   `binop_type` (`crates/nexum-sema/src/lib.rs:1965-2054`) decides the
   result type with `if *l == Ty::Float || *r == Ty::Float { Ty::Float }`
   (lines 1984-1988 and 2005-2014). With `Ty::F32` added and the rule
   unchanged, `Int + F32` is typed **`Int`** (line 1987) and `Float + F32` is
   typed **`Float`** (line 1985) — both wrong. The rule must become: both
   operands numeric, both the same type, otherwise the diagnostic.
   *Reason:* the existing rule only works because there is exactly one float
   type.

9. **Add the documentation in the same merge as the code.** Four files
   outside the compiler: `docs/language-reference/grammar.md:66` and `:112`,
   `docs/runtime-abi.md:79-88`, `docs/standard-library.md:11`, and the
   editor's syntax file `editors/vscode/syntaxes/nexum.tmLanguage.json:38`.
   A new numbered decision goes into
   `docs/language-reference/decisions.md` next to D015 (line 222).
   *Reason:* the brief's house rules require the grammar change to match the
   decision change.

### Which parts are independent

| Slice | Files | Depends on | Can run in parallel with |
| ----- | ----- | ---------- | ------------------------ |
| **A** — float comparison fix | `nexum-codegen/src/lib.rs` only | nothing | B, C, D, E |
| **B** — `Ty::F32` in sema | `nexum-sema/src/lib.rs` | nothing | A, C, D |
| **C** — codegen mapping for `Ty::F32` | `nexum-codegen/src/lib.rs` | the *name* `Ty::F32` agreed in advance | A, B, D, E |
| **D** — MIR constant + `print` | `nexum-mir/src/lib.rs`, `nexum-codegen/src/lib.rs` | B, C | A, E |
| **E** — literal syntax | `nexum-lexer`, `nexum-parser` (+ `normalize.rs`), `nexum-driver`, `examples/selfhost/*` | the syntax decision (open question 1) | A, B, C, D |
| **F** — fixtures and tests | `crates/nexum-driver/tests/**` | B, C, D, E | — |
| **G** — docs | `docs/**`, `editors/**` | B, E | A–F |

Two contention points to plan for:

* **`crates/nexum-sema/src/lib.rs` is the contended file.** Slice B and slice
  E both touch it (E needs to type the new literal). Give them separate
  branches and merge B first.
* **`crates/nexum-codegen/src/lib.rs` is the contended file.** Slice A, C and
  D all touch it. Merge A first (it is a bug fix on its own), then C and D
  together.

---

## What the code looks like today

### 0. The shape of the pipeline

```
source text
  -> nexum-lexer   TokenKind::Int / TokenKind::Float          (the number kinds are decided here)
  -> nexum-parser  ExprKind::Int(i64) / ExprKind::Float(f64)   (the AST)
     -> nexum-parser::normalize::normalize                     (the differential-test form)
  -> nexum-sema    Ty::Int / Ty::Float / ... , HirExpr::Float(f64, Ty)
  -> nexum-mir     Const::Int(i64) / Const::Float(f64), Instr::*, Ty in every instruction
  -> nexum-codegen llvm_ty(Ty) -> "i64" / "double" / ... , the LLVM IR text
     -> nexum-runtime runtime_ir(...)                          (the collector, the heap, the list)
```

`nexum-source` holds only `FileId`, `Span`, `SourceFile`, `SourceMap`
(`crates/nexum-source/src/lib.rs`) and has **nothing** type-related. It needs
no change.

### 1. `nexum-lexer` — where the number kinds are decided

| Place | What it is |
| ----- | ---------- |
| `crates/nexum-lexer/src/lib.rs:10` | `TokenKind::Float` — the variant. |
| `crates/nexum-lexer/src/lib.rs:72` | `TokenKind::Float => "Float"` — the **stable token name**, used by `nx lex` and compared against the self-hosted lexer in `crates/nexum-driver/tests/backend.rs:143-194`. A new token kind must be added here and to the self-hosted lexer. |
| `crates/nexum-lexer/src/lib.rs:303-329` | The number scanner. `is_float` is set when a `.` is followed by a digit (`:310-318`), and the kind is chosen at `:323-327`. |

Two facts about the number scanner that matter for an f32 literal:

* **There is no exponent.** The scanner accepts digits and at most one `.`;
  it never accepts `e`/`E`. `1e5` lexes as `Int "1"` followed by
  `Ident "e5"`, which is a parse error, not a float. The self-hosted lexer
  has the same limitation (`examples/selfhost/lexer_lib.nx:254-282`).
* **A suffix would have to be lexed.** `1.5f32` currently lexes as
  `Float "1.5"` followed by `Ident "f32"`. The lexer would have to consume
  the suffix, in the Rust lexer and in the self-hosted lexer, or the
  differential lexer test (`backend.rs:143`) fails.

### 2. `nexum-parser` — the AST and the type expressions

| Place | What it is |
| ----- | ---------- |
| `crates/nexum-parser/src/lib.rs:48-76` | `pub enum ExprKind`. `Int(i64)` at `:49`, `Float(f64)` at `:50`. **A new literal kind needs a variant here** (or the existing `Float` variant has to become typed). |
| `crates/nexum-parser/src/lib.rs:1077-1089` | `parse_primary`. `TokenKind::Int` → `ExprKind::Int` (`:1080-1084`); `TokenKind::Float` → `ExprKind::Float(t.parse().unwrap_or(0.0))` (`:1085-1089`). Note `unwrap_or(0.0)`: a float literal that does not parse becomes `0.0` **silently**. |
| `crates/nexum-parser/src/lib.rs:13-16` | `pub enum TypeExpr { Named(String), Array(Box<TypeExpr>, usize) }`. A type annotation is just a **name**; the name is resolved to a `Ty` in sema. **So a new primitive type needs no parser change for the annotation** — only sema's `tty`. |
| `crates/nexum-parser/src/lib.rs:648-680` | `parse_type_expr`. It accepts any `Ident` as a type name (`:649-652`) and does not know what the primitives are. |
| `crates/nexum-parser/src/lib.rs:1103-1121` | `parse_primary`'s `Ident` branch: an identifier followed by `(` is already a `Call { callee, args }`. This is why a `f32(x)` conversion builtin needs no parser change at all. |

### 3. `nexum-sema` — the type representation

`Ty` is the single type representation of the whole compiler
(`crates/nexum-sema/src/lib.rs:15-32`):

```rust
pub enum Ty {
    Int,            // :16
    Float,          // :17   <- the f64 special case
    Bool,           // :18
    Str,            // :19
    Unit,           // :20
    Record(String), // :22
    Array(Box<Ty>, usize), // :24
    List(Box<Ty>),  // :27
    Never,          // :29
    Unknown,        // :30
    Error,          // :31
}
```

It derives `PartialEq, Eq` (`:14`), so the variants are compared by value
everywhere.

Every place in sema that lists the primitive types:

| Place | What it is | What an `f32` needs |
| ----- | ---------- | --------------------- |
| `nexum-sema/src/lib.rs:34-49` | `Ty::name()` — the display name used in every diagnostic. `Ty::Float => "Float"` at `:38`. | A new arm, e.g. `Ty::F32 => "F32"`. |
| `nexum-sema/src/lib.rs:50-52` | `Ty::is_numeric()` — `matches!(self, Ty::Int \| Ty::Float)`. **This is the gate for arithmetic and for unary minus.** | Add `Ty::F32`. |
| `nexum-sema/src/lib.rs:54-69` | `Ty::compatible()` — arrays and lists recurse, everything else falls through to `self == other` (`:67`). | **No change needed**: `F32` and `Float` are incompatible by default, which is what we want. |
| `nexum-sema/src/lib.rs:536-555` | `tty()` — the **type-name table**. `"Int" => Ty::Int` (`:540`), `"Float" => Ty::Float` (`:541`), `"Bool"` (`:542`), `"Str"` (`:543`), `"Unit"` (`:544`); anything else is a record name or `Ty::Unknown` (`:545-551`). | A `"F32" => Ty::F32` arm. **This is the only place the language's type names are spelled.** |
| `nexum-sema/src/lib.rs:123-180` | `HirExpr`. `Int(i64, Ty)` at `:125`, **`Float(f64, Ty)` at `:126`**, `Bool(bool, Ty)` at `:127`, `Str(String, Ty)` at `:128`. | The HIR literal **already carries a `Ty`** — this is the hook that lets a float literal be typed as f32 without a new HIR variant (but the *value* is an f64 and would have to be rounded). |
| `nexum-sema/src/lib.rs:182-201` | `HirExpr::ty()` — the `HirExpr::Float(_, t)` arm at `:186`. | Only if a new HIR variant is added. |
| `nexum-sema/src/lib.rs:903-908` | `analyze_expr`'s literal typing: `ExprKind::Float(v) => HirExpr::Float(*v, Ty::Float)` at `:906`. | The typing of a float literal. |
| `nexum-sema/src/lib.rs:1965-2054` | `binop_type` — the arithmetic result type. See below. | **Must be rewritten**; see the trap below. |
| `nexum-sema/src/lib.rs:923-936` | Unary minus: `if !inner.ty().is_numeric() && ...` → the `T017` diagnostic. | Follows `is_numeric()`. |
| `nexum-sema/src/lib.rs:383-417` | The ABI defaults: an unannotated parameter becomes `Ty::Int` (`:389`, `:414`) and an unannotated return becomes `Ty::Int` or `Ty::Unit` (`:399`). | No change, but note that the default is `Int`, not `Float`. |
| `nexum-sema/src/lib.rs:1596-1637` | The `print` builtin (see §4). | See §4. |
| `nexum-sema/src/lib.rs:1359-1450` | `list_push` / `list_set`: the element type is fixed by the first push (`:1399-1419`), and a mismatch is `T049` (`:1420-1432`). | This is where a `List[F32]` element type comes from. |

#### The arithmetic result-type trap

`binop_type` decides the type of `a + b` like this:

```rust
// nexum-sema/src/lib.rs:1984-1988
if *l == Ty::Float || *r == Ty::Float {
    Ty::Float
} else {
    Ty::Int
}
```

and the same shape again at `:2005-2014` for the deferred case. The rule only
works because there is exactly one float type. With `Ty::F32` added and the
rule left alone:

* `Int + F32` → the operands are not compatible, so `T008` is reported, but
  the **result type is `Ty::Int`** (`:1987`) — wrong;
* `Float + F32` → the result type is **`Ty::Float`** (`:1985`) — also wrong,
  the value would silently be widened to f64.

The rule has to become "both operands numeric **and** the same type",
instead of "if either is `Float`, the result is `Float`".

### 4. How `print` and the other builtins dispatch on the argument type

There are **two** dispatch points, and they are in different crates.

**In sema** (`crates/nexum-sema/src/lib.rs:1596-1637`): `print` is checked
with a *whitelist by exclusion*, not a per-type table. It rejects
`Ty::Record` (`:1608-1616`, `T028`) and `Ty::Array` (`:1617-1628`, `T028`)
and accepts **everything else** — `Int`, `Float`, `Bool`, `Str`, `Unit`.
So an f32 argument is accepted with **no sema change at all**.

Every other builtin dispatches on the argument type in the same
"is it this type?" style, and none of them needs to know about f32:

| Builtin | Place | Dispatch |
| ------- | ----- | -------- |
| `panic` | `nexum-sema/src/lib.rs:1168-1183` | one `Str` argument |
| `str_char_at` | `:1184-1219` | `Str` + `Int` |
| `slice` | `:1220-1259` | `Str` + two `Int` |
| `read_file` | `:1260-1285` | one `Str` |
| `write_file` | `:1286-1315` | two `Str` |
| `list_new` | `:1316-1331` | no arguments |
| `list_len` | `:1332-1358` | one `List` |
| `list_push` / `list_set` | `:1359-1450` | a `List` and a value of the list's element type |
| `list_get` | `:1451-1501` | a `List` and an `Int` |
| `concat` | `:1502-1528` | two `Str` |
| `collect` | `:1529-1544` | no arguments |
| `heap_used` | `:1545-1560` | no arguments |
| `len` | `:1561-1594` | an `Array` or a `Str` |

**In codegen** (`crates/nexum-codegen/src/lib.rs:716-755`): `print` is the
one builtin with a real per-type dispatch, because it picks a `printf`
format string:

```rust
// nexum-codegen/src/lib.rs:723-729
let fmt_content = match argty {
    Ty::Int   => "%lld\n",
    Ty::Float => "%g\n",
    Ty::Bool  => "%d\n",
    Ty::Str   => "%s\n",
    _         => "%lld\n",
};
```

`Bool` is widened to `i64` before the call (`:740-743`); every other type is
passed with its own LLVM type (`:745`). The call itself is at `:747-750`:

```rust
buf.push_str(&format!(
    "  call i32 (ptr, ...) @printf(ptr @{}, {} {})\n",
    gname, argtystr, argreg
));
```

**An f32 needs an `fpext float to double` before that call.** `printf` is
variadic and reads a `double` for `%g`, but a `float` passed raw into a
variadic call arrives in the low half of the SSE register. Verified
empirically:

```
raw float to printf:   5.28427e-315     <- garbage
with fpext:            1.5              <- correct
```

The `float` case also needs its own format-string arm; `%g` works for both
once the value has been widened (it prints 6 significant digits, which is
already the documented behaviour — `docs/language-reference/grammar.md:305`).

The `list_push` / `list_set` path (`:756-801`) also depends on the type:
`list_elem_ty` (`:102-107`, `:763`) gets the element type out of the list's
`Ty::List(..)`, and `llvm_size` / `scan_bytes` of that element type are
handed to the runtime (`:779-780`).

### 5. `nexum-mir` — the constant and the folder

| Place | What it is |
| ----- | ---------- |
| `crates/nexum-mir/src/lib.rs:15-21` | `pub enum Const { Int(i64), Float(f64), Bool(bool), Str(String) }`. **The constant carries no type tag** — the type comes from the instruction that holds it (`Instr::Const { dest, c, ty }`, `:47-52`). |
| `crates/nexum-mir/src/lib.rs:289-306` | The implicit zero value for a function that falls off the end: `Ty::Float => Const::Float(0.0)` (`:296`), `Ty::Bool => Const::Bool(false)` (`:297`), `_ => Const::Int(0)` (`:298`). |
| `crates/nexum-mir/src/lib.rs:527-565` | `lower_expr`: `HE::Float(v, ty) => Const::Float(*v)` (`:539-547`). The HIR's `ty` is **dropped** — the constant does not record it. |
| `crates/nexum-mir/src/lib.rs:867-911` | `fold_binop(op, l, r, _ty: Ty)` — **the type parameter is named `_ty` and ignored**. The float arms (`:895-904`) fold in f64. |
| `crates/nexum-mir/src/lib.rs:913-921` | `fold_unop`: `(Neg, Const::Float(v)) => Some(Const::Float(-v))` (`:917`). |

The MIR verifier (`crates/nexum-mir/src/lib.rs:772-785`) only checks that
every block has a terminator. It is type-agnostic and needs no change.

Two consequences:

* If the constant keeps no type tag, an f32 constant is an f64 value that
  has to be rounded (`fptrunc`) at the point of use, and the folder has to
  round too — otherwise `0.1f32 + 0.2f32` folded in f64 and truncated once
  is not the same as folded in f32. Adding `Const::F32(f32)` makes the
  constant self-describing and lets the folder round at every step.
* `fold_binop`'s ignored `_ty` is a landmine for the whole tower: the
  moment there are two float types, the folder has to know which one it is
  folding.

### 6. `nexum-codegen` — the LLVM mapping and the hardcoded `double`

`llvm_ty` is the whole type → LLVM mapping
(`crates/nexum-codegen/src/lib.rs:8-20`):

```rust
fn llvm_ty(t: &Ty) -> String {
    match t {
        Ty::Int      => "i64".to_string(),     // :10
        Ty::Float    => "double".to_string(),  // :11  <- the f64 special case
        Ty::Bool     => "i1".to_string(),      // :12
        Ty::Str      => "ptr".to_string(),     // :13
        Ty::List(_)  => "ptr".to_string(),     // :14
        Ty::Unit     => "void".to_string(),    // :15
        Ty::Record(n) => format!("%{}", n),    // :16
        Ty::Array(elem, n) => format!("[{} x {}]", n, llvm_ty(elem)), // :17
        _            => "i64".to_string(),     // :18  <- the silent trap
    }
}
```

The `_ => "i64"` catch-all means a `Ty::F32` that is not listed here is
**silently compiled as `i64`**: no error, no warning, wrong arithmetic.

The three layout functions:

| Function | Place | The float case |
| -------- | ----- | -------------- |
| `has_refs` | `:23-34` | falls into `_ => false` (`:32`) — **a float holds no references and is not scanned.** |
| `llvm_align` | `:36-53` | falls into `_ => 8` (`:51`). An f32's natural alignment is **4**. |
| `llvm_size` | `:55-68` | `Ty::Int \| Ty::Float \| Ty::Str \| Ty::List(_) => 8` (`:57`). An f32's size is **4**. |
| `record_size` | `:70-81` | The layout, computed from `llvm_align` and `llvm_size`. It must agree with LLVM's own struct layout. |
| `scan_bytes` | `:85-99` | `0` when `has_refs` is false (`:86-88`). |

The places that hardcode the word `double` in the emitted IR:

| Place | What it emits | What it needs |
| ----- | ------------- | ------------- |
| `:461` | `Instr::Const { dest, c, ty: _ }` — **the instruction's type is ignored**. | The `ty` has to be used. |
| `:466-482` | `Const::Float(v)` → `fadd double 0.0, {v}` (`:482`), with infinity/NaN as 64-bit hex bit patterns (`:467-474`). | `fadd float 0.0, {v}` for an f32 constant. See the note on hex patterns below. |
| `:525`, `:534`, `:541`, `:548` | `if *ty == Ty::Float` → `fadd` / `fsub` / `fmul` / `fdiv`\|`frem`, otherwise the integer op. | A predicate that covers both float types. |
| `:555` | `fdiv` / `frem` emit `double` **literally**, not `t` (`llvm_ty(ty)`). | `t`. |
| `:615-635` | `Eq`: `fcmp oeq` for `Ty::Float` (`:624-628`), with `double` hardcoded at `:627`. | `t`. |
| `:636-650` | `Ne`: **no float case** — `icmp ne {oty_str}` (`:646`). | `fcmp une`, and the `Ty::Float` special case does not exist here at all. |
| `:651-662` | `Lt` / `Gt` / `Le` / `Ge`: **no float case** — `icmp slt` / `sgt` / `sle` / `sge` (`:652-661`). | `fcmp olt` / `ogt` / `ole` / `oge`. |
| `:667-690` | `UnOp::Neg`: `fneg double` (`:682`) if `Ty::Float` (`:681`). | `fneg {t}`. |
| `:1202-1220` | `main`'s return: `Ty::Float => fptosi double {r} to i32` (`:1214-1217`). | `fptosi float`. |

#### What LLVM accepts for a `float` constant

Verified against clang 19 (`clang -x ir`):

| Emitted literal | Result |
| ---------------- | ------ |
| `fadd float 0.0, 1.5` | valid → `1.500000e+00` |
| `fadd float 0.0, 0x7FF0000000000000` | valid → `+inf` (a 64-bit hex pattern is read as a **double** and converted) |
| `fadd float 0.0, 0x7FF00000` | **error**: `floating point constant invalid for type` (a 32-bit hex pattern is rejected) |
| `fadd float 0.0, 1.0e40` | **error**: `floating-point constant overflowed type` (an out-of-range decimal is a hard error, not an infinity) |

So the existing infinity/NaN handling at `:467-474` (which uses the 64-bit
double patterns `0x7FF0000000000000`, `0xFFF0000000000000`,
`0x7FF8000000000000`) **works unchanged for f32**; what must not happen is a
32-bit hex pattern, and a decimal literal must never be out of the f32
range. Rust's `{}` for an `f32` prints the shortest decimal that round-trips
as an f32, and `format!("{}", 1.0f32)` is `"1"` — the existing "append `.0`
if there is no `.`/`e`/`E`" rule at `:477-479` covers that.

### 7. `nexum-runtime` — type-agnostic, with one contract that matters

The runtime emits LLVM IR text and contains **no float, double, `Float` or
`Ty` at all** (verified by grep over `crates/nexum-runtime/src/`). The
collector is entirely byte-oriented:

| Place | What it is |
| ----- | ---------- |
| `crates/nexum-runtime/src/lib.rs:29-32` | The documented contract: *"A `Str` local scans 8 bytes; a record or array that contains references scans its whole size; a numeric local scans 0 bytes and is not registered at all."* |
| `crates/nexum-runtime/src/lib.rs:168` | `@nexum_scan_range(ptr %base, i64 %bytes)` — scans `bytes/8` 8-byte words. |
| `crates/nexum-runtime/src/lib.rs:174` | `@nexum_collect()` — walks the root frames, calls `nexum_scan_range` for each entry with a non-zero scan size. |
| `crates/nexum-runtime/src/lib.rs:180` | `@nexum_gc_trace()` — traces the heap. |
| `crates/nexum-runtime/src/lib.rs:224` | `@nexum_list_push(ptr %list, ptr %elem, i64 %elem_size, i64 %elem_scan)` — **`elem_size` is the buffer stride** (`off = len * esize`, `memcpy(dst, elem, esize)`) and **`elem_scan` is the number of bytes the collector must scan in the buffer**. |
| `crates/nexum-runtime/src/lib.rs:230` | `@nexum_list_set` — uses the list's stored element size. |

**The runtime needs no change for f32.** The only thing the compiler must
get right is the two numbers it hands to `nexum_list_push`: the element size
(4 for an f32) and the scan size (0 for an f32).

### 8. `nexum-driver` — the formatter and the CLI

| Place | What it is |
| ----- | ---------- |
| `crates/nexum-driver/src/main.rs:587-596` | `fmt_expr` — the `nx fmt` printer. `ExprKind::Float(v)` prints `{}` and appends `.0` if the text has no `.`, `e` or `E` (`:590-596`). A new literal kind needs an arm here. |
| `crates/nexum-driver/src/main.rs:549-559` | `fmt_type` — prints a `TypeExpr`; a `Named` type is printed by name, so **no change** for a new type name. |
| `crates/nexum-driver/src/main.rs:1105-1119` | `parse_file` → `nexum_parser::normalize::normalize`. |
| `crates/nexum-driver/src/main.rs:947-990` | `nx explain` — the diagnostic table. Type names appear only in prose; a new diagnostic code would be added here. |

The driver does **not** mention `Ty` anywhere (verified by grep) — it goes
through the sema and the MIR without inspecting types.

### 9. The collector's treatment of `Float`, and of an f32

The collector's view of a type is decided entirely in codegen, in three
functions:

1. **`has_refs`** (`crates/nexum-codegen/src/lib.rs:23-34`) asks "can this
   value hold a pointer the collector must trace?". `Ty::Str` and
   `Ty::List(_)` can; `Ty::Array` and `Ty::Record` recurse into their
   elements and fields; **everything else, including `Ty::Float`, is
   `false`** (`:32`).
2. **`scan_bytes`** (`:85-99`) returns `0` for a type with no references.
3. The **root frame** is built from the slots and temporaries whose type
   `has_refs` accepts (`:361-388`), and each entry records the address and
   the number of bytes to scan. A numeric local is **not registered at all**
   — it is not a root, and it is not scanned.

**An f32 would behave exactly like `Float`, with no change to any of this**,
because `has_refs` is a whitelist of the reference-holding types and
everything else falls into `_ => false`. The one thing that *must* change is
`llvm_size`: a `List[F32]` would get `elem_size = 8` from the `_ => 8`
catch-all while LLVM stores a 4-byte `float`, so the buffer stride and the
`memcpy` length would both be wrong (see Recommendation 6).

The roadmap already states this property:
`docs/native-roadmap.md:114-117` — *"`Float` buffers are not scanned at all
(they hold no references), which is what makes them cheap."*

### 10. The normalised AST form and the differential self-hosting tests

`crates/nexum-parser/src/normalize.rs` is the contract between the Rust
parser and the self-hosted parser.

* The header comment (`normalize.rs:1-10`) states the contract: the
  self-hosted compiler (Phase 10) parses the same source with its own
  parser, and the normalised form is the line-oriented tree the two are
  compared through.
* The literal arms are `normalize.rs:69-70`:

  ```rust
  ExprKind::Int(v)   => out.push_str(&format!("{}int {}\n", pad, v)),
  ExprKind::Float(v) => out.push_str(&format!("{}float {:?}\n", pad, v)),
  ```

  `{:?}` is Rust's `Debug` for `f64`, which always prints a decimal point
  (`2.5` → `2.5`, `2.0` → `2.0`).
* The type expressions are printed as `named {name}` (`:56-58`), so a new
  type **name** needs no change here at all. Only a new *literal kind*
  needs a new line.

The self-hosted side:

* `examples/selfhost/parser.nx:251-270` — `parse_primary`: `if k == "Float"`
  → `add_node(nodes, "float", t, 0, 0, 0)` (`:257-260`).
* `examples/selfhost/parser.nx:763-764` — the default branch of
  `print_node` prints the node's kind and its text, which is how `int`,
  `float` and `bool` are printed.
* `examples/selfhost/lexer_lib.nx:254-282` — the self-hosted number lexer.

The differential tests that compare the two:

| Test | Place | What it compares |
| ---- | ----- | ---------------- |
| `selfhosted_lexer_matches_rust_lexer` | `crates/nexum-driver/tests/backend.rs:143-194` | the token **kind names** and the token text, for every `examples/*.nx` |
| `selfhosted_parser_matches_rust_parser` | `crates/nexum-driver/tests/backend.rs:196-249` | the **normalised form**, for every `tests/fixtures/parse/*.nx` |

**A new literal kind has to be added to both sides at once**, and the two
have to produce byte-identical text. This is where the design has to be
careful:

* The Rust side prints the **parsed value** (`{:?}` of an f64); the
  self-hosted side prints the **raw source text** (`text_at(tokens, pos)`).
  They agree today only because the fixtures use a canonical spelling:
  `tests/fixtures/parse/literal_float.nx` contains exactly `2.5`. A source
  of `2.50` would make the Rust side print `2.5` and the self-hosted side
  print `2.50`, and the differential test would fail. That fragility exists
  today for f64.
* For an f32 the two sides would have to agree on the **rounding** as well:
  the Rust side would print the `f32` value, the self-hosted side would
  have to parse and round it — which the self-hosted language cannot do
  today, because it has no f32 and no float-to-string conversion.

**This is a strong argument for the conversion-builtin route** (Recommendation
2): `f32(1.5)` is a `Call` node, it is already in the normalised form
(`call f32` + the argument), and **neither the normalised form nor the
self-hosted parser changes at all**.

### 11. The fixtures and tests that would need a new case

| Artefact | Place | What it would need |
| -------- | ----- | ----------------- |
| `compile_pass_fixtures` | `crates/nexum-driver/tests/fixtures.rs:14-38` | every `tests/fixtures/pass/*.nx` must compile with zero error diagnostics |
| `compile_fail_fixtures` | `crates/nexum-driver/tests/fixtures.rs:40-94` | every `tests/fixtures/fail/*.nx` must produce the codes listed in its `# EXPECT: Txxx` comments |
| `codegen_emits_expected_fragments` | `crates/nexum-driver/tests/backend.rs:77-91` | asserts `ir.contains("fadd double")` (`:85`) — an f32 case would add `ir.contains("fadd float")` |
| `ir_verifies_for_pass_fixtures` | `crates/nexum-driver/tests/backend.rs:45-50` | the IR of every pass fixture must verify with clang |
| `ir_verifies_for_examples` | `crates/nexum-driver/tests/backend.rs:52-58` | the IR of every `examples/*.nx` must verify |
| `ir_verifies_for_all_targets` | `crates/nexum-driver/tests/backend.rs:60-74` | the same IR must verify for every target triple |
| `run_outputs_match_fixtures` | `crates/nexum-driver/tests/backend.rs:94-138` | every `tests/fixtures/run/*.nx` must produce exactly the bytes of the matching `.out` (and the `.exit` code) |
| `differential_opt_levels_agree` | `crates/nexum-driver/tests/backend.rs:251-316` | a **fixed list** of programs (`:254-269`) — an f32 program would be added to the list |
| the lexer unit tests | `crates/nexum-lexer/src/lib.rs:440-537` | a test for the new token kind, if any |
| the parser unit tests | `crates/nexum-parser/src/lib.rs:1163-1296` | a test for the new literal, if any |

The fixtures that mention floats today:

| Fixture | Content | Note |
| ------- | ------- | ---- |
| `tests/fixtures/parse/literal_float.nx` | `2.5` | the differential-parser fixture for a float literal |
| `tests/fixtures/parse/literals.nx` | `true` | |
| `tests/fixtures/pass/numbers.nx` | `let f = 1.5` / `let g = f * 2.0` / `print(g)` | the float arithmetic fixture |
| `tests/fixtures/run/arith.nx` | `print(2.5 * 2.0)` → `5` | the run fixture; the `.out` shows `%g` printing `5` for `5.0` |

**There is no fixture anywhere that compares two floats.** `arith.nx`
compares `1 < 2` (integers) and `examples/particles.nx` has no comparison at
all. That is why the `icmp`-on-float bug (§13.1) has survived.

New fixtures that would be needed:

* `tests/fixtures/pass/` — a program that declares an f32 value, does
  arithmetic on it, puts it in an array and in a list, and prints it.
* `tests/fixtures/run/` — the same, with a `.out` file. **The `.out` file
  pins the printed form**, so it pins the `%g` behaviour and the rounding.
* `tests/fixtures/parse/` — only if the literal syntax changes.
* `tests/fixtures/fail/` — a fixture with `# EXPECT: T008` for a mixed
  `Float`/`F32` operand pair, and one for `Int`/`F32`.

### 12. The documentation and the editor

| Artefact | Place | What it says today |
| -------- | ----- | ------------------- |
| `docs/language-reference/grammar.md:66` | the `type-expr` production | `type-expr = identifier (Int \| Float \| Bool \| Str \| Unit \| record name)` |
| `docs/language-reference/grammar.md:112` | the type list | *"`Int` (i64), `Float` (f64), `Bool` (i1 …), `Str` …, `Unit`, records …, arrays …, `Never`"* |
| `docs/language-reference/grammar.md:129-134` | the arithmetic rules | *"operands must be numeric and of the same numeric type (T017/T008). There is no implicit `Int` → `Float` conversion yet."* |
| `docs/language-reference/grammar.md:300-306` | the known limits | *"No implicit numeric conversions"*, and the `%g` note |
| `docs/runtime-abi.md:79-88` | the type → LLVM table | `| `Float` | `double` | IEEE 754 |` at `:82` |
| `docs/standard-library.md:11` | the `print` row | *"`T` may be Int, Float, Bool or Str."* |
| `docs/language-reference/decisions.md:222-231` | **D015** | *"The numeric tower is f32 and f64"* — status: phase 11c |
| `docs/native-roadmap.md:47-49` | the audit | *"`Float` is `f64` only. There is no `f32` …"* |
| `editors/vscode/syntaxes/nexum.tmLanguage.json:38` | the builtin type names | `"match": "\\b(Int\|Float\|Bool\|Str\|Unit\|Never)\\b"` |

### 13. The bugs found on the way (pre-existing, in the `f64` path)

These are not f32 work; they are defects in the existing `Float` path that
the f32 implementer will meet immediately. Two of them were reproduced
end-to-end in this worktree.

**13.1 — Float ordering comparisons and `!=` emit invalid LLVM IR.**
`crates/nexum-codegen/src/lib.rs:636-650` (`Ne`) and `:651-662`
(`Lt`/`Gt`/`Le`/`Ge`) have **no float case**: they emit
`icmp ne {oty_str}` / `icmp slt {oty_str}` with the operand's LLVM type.
For a `double` operand that is `icmp slt double %a, %b`, which LLVM
rejects. `Eq` is the only comparison that was given a float case
(`:624-628`, `fcmp oeq`).

Reproduced: a program with

```
let a = 1.5
let b = 2.5
if a < b: ...
if a != b: ...
```

emits

```
%v6  = icmp slt double %v4, %v5
%v10 = icmp ne double %v8, %v9
```

and `clang -x ir` rejects the module:

```
error: icmp requires integer operands
  626 |   %v6 = icmp slt double %v4, %v5
```

**No fixture covers it.** `tests/fixtures/run/arith.nx` compares `1 < 2`
(integers) and `examples/particles.nx` has no comparison at all, so
`run_outputs_match_fixtures` and `ir_verifies_for_examples` never see it.

**13.2 — `print` of a float needs an explicit widening.** Not a defect in
the `f64` path (a `double` is what `%g` wants), but the shape of the trap:
the codegen calls `printf` with the value's own LLVM type
(`crates/nexum-codegen/src/lib.rs:747-750`), and `printf` is variadic and
reads a `double` for `%g`. A raw `float` arrives in the low half of the SSE
register. Reproduced with a hand-written module:

```
call i32 (ptr, ...) @printf(ptr @fmt, float %x)      ->  5.28427e-315
call i32 (ptr, ...) @printf(ptr @fmt, double %d)      ->  1.5
   (with %d = fpext float %x to double)
```

**13.3 — the constant's hex bit patterns are 64-bit, and only 64-bit hex
patterns work for an `f32`.** `crates/nexum-codegen/src/lib.rs:466-481`
emits infinity and NaN as `0x7FF0000000000000`, `0xFFF0000000000000` and
`0x7FF8000000000000`. Checked against clang:

| Literal for a `float` | clang |
| ---------------------- | ----- |
| `0x7FF0000000000000` | accepted, becomes `+inf` |
| `0x7FF00000` | **rejected**: `floating point constant invalid for type` |
| `1.0e40` | **rejected**: `floating-point constant overflowed type` |
| `3.4028235e38` | accepted (f32::MAX) |

So the existing 64-bit patterns keep working for an f32, a 32-bit pattern
must never be used, and a decimal literal must never be out of the f32
range.

**13.4 — the normalised form and the self-hosted parser disagree in
principle.** `crates/nexum-parser/src/normalize.rs:70` prints the *parsed
value* (`{:?}` of an `f64`) while `examples/selfhost/parser.nx:257-260`
prints the *raw source text*. They agree today only because the fixtures
are spelled canonically (`tests/fixtures/parse/literal_float.nx` is exactly
`2.5`). A source of `2.50` would make the two differ, and the differential
test would fail. This matters for f32, where the self-hosted side would also
have to round.

**13.5 — `parse_primary` swallows an unparsable float literal.**
`crates/nexum-parser/src/lib.rs:1088` uses `t.parse().unwrap_or(0.0)`: a
float literal that does not parse becomes `0.0` with no diagnostic. The
lexer only ever produces a well-formed decimal, so this is latent today.

---

## The design

The design below is the one the recommendation points at. It is written so
that an implementer does not have to guess.

### The syntax

**Type name.** `F32`. `Float` keeps its meaning (f64) — renaming it to `F64`
would break every existing Nexum program, every fixture and the standard
library, and buys only symmetry. The Rust variant is `Ty::F32`.

**How a value becomes an f32.** A conversion builtin `f32(x)`:

* `x` may be `Float` (f64) or `Int`; the result is `F32`.
* `f32(x)` is **not** a new syntax: `parse_primary` already turns
  `Ident (` into a `Call` (`crates/nexum-parser/src/lib.rs:1103-1121`), so
  the lexer, the parser AST, the normalised form, the self-hosted lexer and
  the self-hosted parser are all untouched.
* The builtin is implemented in sema next to the other builtins (around
  `crates/nexum-sema/src/lib.rs:1359-1450`), with a new diagnostic code
  (the next free one is `T050`; the table in
  `crates/nexum-driver/src/main.rs:949-986` has to be extended).

**Why a builtin and not a literal suffix.** The suffix `1.5f32` needs the
lexer to consume the suffix (Rust and self-hosted), the parser AST to carry
it, the normalised form to print it, the self-hosted parser to build it, and
the formatter to print it — and, worst of all, the self-hosted parser would
have to *round* an f64 text to an f32 to produce the same normalised line,
which it cannot do today (§10). A builtin has none of those problems and
can be converted into sugar later: once `f32(x)` exists, `1.5f32` can be
defined as `f32(1.5)` and the desugaring happens in the parser, where the
rounding is done once by the Rust side — and the normalised form would then
need a decision about whether the sugar is visible.

**What the language would not have.** No implicit conversion: `F32` and
`Float` are different types and mixing them in an arithmetic expression is
`T008`, exactly as `Int` and `Float` are today.

### The type representation

```rust
// crates/nexum-sema/src/lib.rs:15-32
pub enum Ty {
    Int,
    Float,     // f64
    F32,       // <- new
    Bool,
    Str,
    Unit,
    Record(String),
    Array(Box<Ty>, usize),
    List(Box<Ty>),
    Never,
    Unknown,
    Error,
}
```

and the four arms that go with it:

```rust
// crates/nexum-sema/src/lib.rs:34-49
Ty::F32 => "F32".to_string(),

// crates/nexum-sema/src/lib.rs:50-52
pub fn is_numeric(&self) -> bool {
    matches!(self, Ty::Int | Ty::Float | Ty::F32)
}

// crates/nexum-sema/src/lib.rs:50-52 (new helper, used by codegen)
pub fn is_float(&self) -> bool {
    matches!(self, Ty::Float | Ty::F32)
}

// crates/nexum-sema/src/lib.rs:536-555
"F32" => Ty::F32,
```

`Ty::compatible` needs **no change**: its fall-through is `self == other`
(`:67`), so `F32` and `Float` are already incompatible.

### The lowering

```
source            f32(1.5)
parser            Call { callee: "f32", args: [Float(1.5)] }        (unchanged AST)
sema              HirExpr::Call { callee: "f32", args: [Float(1.5, Float)], ty: F32 }
                    (the builtin arm does the conversion: f64 -> f32,
                     emitting a HirExpr::Float(v as f32, Ty::F32) or a
                     dedicated HirExpr::F32(f32, Ty))
mir               Instr::Const { dest, c: Const::F32(v), ty: F32 }
codegen           %v0 = fadd float 0.0, 1.5
```

The conversion itself has to happen somewhere with an explicit rounding.
The cleanest place is **sema**, in the `f32` builtin arm: the argument is
analysed as an f64 and the result is a rounded f32. That keeps the rounding
in one place, visible in the HIR, and lets the MIR and the codegen stay
type-driven.

If the HIR is to carry the rounded value, the HIR needs either a new
variant (`HirExpr::F32(f32, Ty)`) or the existing `HirExpr::Float(f64, Ty)`
with the value pre-rounded. The first is honest — the HIR variant list is
at `crates/nexum-sema/src/lib.rs:123-180` and adding a variant there is a
four-line change plus the `ty()` arm at `:182-201`. The second is cheaper
but leaves an f64 in the HIR that means "an f32", which the MIR then has to
truncate.

### The layout

```rust
// crates/nexum-codegen/src/lib.rs:8-20
Ty::F32 => "float".to_string(),

// crates/nexum-codegen/src/lib.rs:36-53
Ty::F32 => 4,          // llvm_align

// crates/nexum-codegen/src/lib.rs:55-68
Ty::F32 => 4,          // llvm_size
```

With those two numbers, `record_size` (`:70-81`) agrees with LLVM's own
struct layout for the interesting cases:

| Record | LLVM layout | `record_size` with `F32` = 4/4 |
| ------ | ------------ | -------------------------------- |
| `{ f: F32 }` | `{float}` → size 4, align 4 | 4 |
| `{ f: F32, i: Int }` | `{float, i64}` → size 16, align 8 | 16 |
| `{ f: F32, s: Str }` | `{float, ptr}` → size 16, align 8 | 16 |

A `List[F32]` gets `elem_size = 4` and `elem_scan = 0` — the buffer is
dense, and the collector never scans it.

### The collector

**No change.** `has_refs` (`crates/nexum-codegen/src/lib.rs:23-34`) already
returns `false` for every type that is not `Str`, `List`, `Array` or
`Record`, and `scan_bytes` (`:85-99`) returns `0` for those. An f32 is a
numeric value like `Float` and holds no reference, so it is not registered
as a root and the collector never looks at it.

### `print`

```rust
// crates/nexum-codegen/src/lib.rs:723-729
Ty::F32 => "%g\n",
```

and, before the `printf` call, an explicit widening:

```
%v = fpext float %arg to double
call i32 (ptr, ...) @printf(ptr @fmt, double %v)
```

`%g` prints six significant digits, which is the documented behaviour for
`Float` today (`docs/language-reference/grammar.md:305`); a runtime
formatter lands later and will replace both.

---

## Risks

1. **A missing `llvm_size` arm is a memory-safety bug, not a wrong answer.**
   If `Ty::F32` is added to `Ty` and to `llvm_ty` but not to `llvm_size`,
   a `List[F32]` gets `elem_size = 8` while LLVM stores 4-byte elements.
   The runtime then uses an 8-byte stride and an 8-byte `memcpy` on a
   4-byte element: the buffer is twice as large as it should be, the
   `memcpy` reads four bytes past the element, and `list_get` loads a
   `float` from a misaligned address. Nothing in the test suite would catch
   it unless a fixture puts an f32 in a list.
   *Mitigation:* Recommendation 6; a run fixture that puts f32 values in a
   list.

2. **A missing `llvm_ty` arm is silent.** `llvm_ty`'s catch-all is
   `_ => "i64"` (`:18`). A `Ty::F32` that is not listed is compiled as an
   integer, and the program produces wrong numbers rather than an error.
   *Mitigation:* the type and its mapping land in the same merge; and the
   catch-all should arguably become an `unreachable!()` or a compile-time
   exhaustiveness check (the `Ty` enum is `#[derive]`-able, so a
   `match` without a wildcard would force every new variant to be handled
   in every one of these functions).

3. **The float comparison bug is already live in the f64 path.** A program
   that compares two floats today produces IR that clang rejects (§13.1). If
   the f32 work is merged before that is fixed, the f32 comparison path will
   inherit it and the first f32 fixture that compares two values will fail
   to verify.
   *Mitigation:* Recommendation 1; fix it first, on its own.

4. **`print` of an f32 without an `fpext` prints garbage**, not an error.
   The value is passed in the low half of an SSE register and `printf`
   reads 64 bits. The output would look like `5.28427e-315` and a run
   fixture with a `.out` file would catch it — but only if the fixture
   exists.
   *Mitigation:* Recommendation 4; a run fixture that prints an f32.

5. **The const folder would compute f32 constants in f64.**
   `fold_binop` ignores its type parameter (`crates/nexum-mir/src/lib.rs:867`),
   so `0.1f32 + 0.2f32` folded in f64 and truncated once is a different
   number from the same expression folded in f32. The
   `differential_opt_levels_agree` test
   (`crates/nexum-driver/tests/backend.rs:251-316`) compares opt 0 against
   opt 1, so a folded f32 constant that disagrees with the unfolded
   computation would fail that test — which is good, but only if an f32
   program is in the list at `:254-269`.
   *Mitigation:* give `Const` a type tag, make the folder type-aware, and
   add an f32 program to the differential list.

6. **The normalised form is a two-sided contract.** If the literal syntax
   changes, the Rust normalised form and the self-hosted parser must
   produce byte-identical text for every parse fixture, and the two
   currently disagree in principle (the Rust side prints the parsed value,
   the self-hosted side prints the raw source text). Rounding an f64 to an
   f32 in the self-hosted parser is not expressible in today's language.
   *Mitigation:* this is the main reason the recommendation defers the
   literal suffix, and the reason the conversion builtin is recommended
   instead.

7. **The differential lexer test constrains the lexer.** Any new token kind
   or any change to the number scanner has to be made in
   `examples/selfhost/lexer_lib.nx` as well, or
   `selfhosted_lexer_matches_rust_lexer`
   (`crates/nexum-driver/tests/backend.rs:143-194`) fails. The self-hosted
   lexer is written in Nexum and has no f32.

8. **The editor's type list is easy to forget.**
   `editors/vscode/syntaxes/nexum.tmLanguage.json:38` hardcodes
   `Int|Float|Bool|Str|Unit|Never`. A new type name that is not added there
   is not highlighted. This is not a test failure; it is a shipped artefact
   that will not be noticed by CI.

9. **`Ty` is matched in eleven places across three crates** (`llvm_ty`,
   `llvm_align`, `llvm_size`, `has_refs`, `scan_bytes`, `Ty::name`,
   `is_numeric`, `tty`, `binop_type`, `fold_binop`, `main`'s return
   conversion, plus the `Ty::Float` tests in the binop emitter). Missing one
   is easy, because most of them have a catch-all that does the wrong thing
   quietly. *Mitigation:* the recommendation to replace the catch-alls with
   exhaustive matches, or with a single `is_float()` predicate.

---

## Open questions

1. **What is the literal syntax for an f32?** *Recommendation:* a
   conversion builtin `f32(x)` first, and the literal suffix `1.5f32` later
   as sugar, if at all. The suffix costs the lexer, both parsers, the
   normalised form and the formatter, and it forces the self-hosted parser
   to round a decimal — which it cannot do today. The builtin costs one arm
   in sema. *The orchestrator has to decide, because it changes the size of
   the work by roughly a factor of three.*

2. **Is `F32` the right name, and does `Float` become `F64`?**
   *Recommendation:* add `F32` and leave `Float` as f64. Renaming `Float`
   to `F64` is a breaking change to every existing program, every fixture
   and the standard library, and buys only symmetry. The editor's type list
   and the grammar table have to be updated either way.

3. **Is `f32` a conversion, a cast, or a constructor?** *Recommendation:*
   call it a **conversion** and make it explicit that it rounds to nearest
   (IEEE 754 round-to-nearest-even), and that `f32` of an `Int` is an exact
   conversion for every `Int` in range. The name `f32` reads like a cast;
   if the orchestrator prefers the cast reading, the grammar should say
   that `f32(x)` rounds rather than truncates.

4. **What is `f32` of a value that is out of range?** Every `i64` is exactly
   representable as an f32 only up to 2^24; beyond that the conversion
   rounds. *Recommendation:* define it as the IEEE 754 round-to-nearest-even
   conversion of the exact value, with no error and no trap — the same
   "never traps" rule that `Float` follows today
   (`docs/language-reference/grammar.md:133-134`).

5. **Does `print` of an f32 print the f32 value or the widened f64?**
   *Recommendation:* widen to f64 and print with `%g`, exactly as the f64
   path does today, and let the runtime formatter (a later phase) fix the
   precision. The alternative — a dedicated `%g`-with-float-semantics
   format — does not exist in C's `printf` and would need the runtime
   formatter first.

6. **Do the fixed-width integers (also phase 11c) land in the same slice?**
   *Recommendation:* **no.** The f32 slice should be a complete, reviewable
   unit on its own: one new type, one conversion, one layout, one test
   fixture set. The integer widths touch the same files (the `Ty` enum, the
   `llvm_ty`/`llvm_size`/`llvm_align` tables, the MIR's `Const`, the
   saturating/wrapping arithmetic rules in `binop_type` and the codegen's
   overflow checks at `crates/nexum-codegen/src/lib.rs:558-613`) but they
   are a much larger change, and mixing them makes the review of the f32
   slice harder for no gain.

7. **Should `f64` be renamed in the type system to `F64` as part of this
   work, so that the tower is symmetric?** *Recommendation:* no, for the
   reason in question 2. But the *decision record* should note that `Float`
   is f64 permanently, so that the naming asymmetry is a decision rather
   than an accident.

8. **Should the arithmetic rule keep the "one operand decides the result"
   shape?** *Recommendation:* no. With two float types the rule has to
   become "both operands must have the same type" (`T008` otherwise), and
   the result type is that type. That is the rule the grammar already
   describes in prose (`docs/language-reference/grammar.md:129-130`) — the
   implementation only matches it today because there is one float type.

9. **Does the normalised form need to distinguish the two float types?**
   With the conversion builtin, the normalised form contains `call f32` and
   nothing else changes. With a literal suffix, the normalised form would
   need a new line for the f32 literal, and the self-hosted parser would
   have to produce it — see the risk in §10.
   *Recommendation:* the builtin route keeps the normalised form
   unchanged, which is one fewer thing to break.

---

## What was verified, and what was not

**Verified in this worktree** (`C:\Nexum\work\f32-touchpoints`, branch
`research/f32-touchpoints`, at `main` = `316f6c6`):

* `cargo check --workspace` → exit code 0.
* `cargo test --workspace` → exit code 0; 32 tests pass
  (1 driver unit + 8 backend + 2 fixtures + 7 lexer + 7 parser + 5 runtime
  + 2 toolchain).
* The float comparison bug was **reproduced end to end**: a program with
  `if a < b` and `if a != b` on two `Float`s emits
  `%v6 = icmp slt double %v4, %v5` and
  `%v10 = icmp ne double %v8, %v9`, and `clang -x ir` rejects the module
  with `error: icmp requires integer operands`.
* The `print` bug was **reproduced end to end**: a hand-written module that
  passes a raw `float` to `printf` with `%g` prints `5.28427e-315`; the
  same module with `fpext float to double` first prints `1.5`.
* The LLVM float-constant rules were **checked against clang**: a 64-bit hex
  bit pattern is accepted for a `float` and converted (`+inf`), a 32-bit hex
  pattern is rejected (`floating point constant invalid for type`), and an
  out-of-range decimal is rejected (`floating-point constant overflowed
  type`).

**Not verified here:** the behaviour of the language *with* an f32 — that is
the implementer's job. The design above has not been compiled, and the
fixture expectations (in particular the printed form of an f32 under `%g`)
have not been pinned by running a program, because there is no f32 yet.

**The scratch files used for the two reproductions were written outside the
repository**, in the session's temporary directory, and deleted afterwards.
No file in the repository was modified.
