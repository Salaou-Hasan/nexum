# Nexum Language Grammar (Draft)

Status: **draft** — describes the syntax accepted by the bootstrap compiler
(`nx`) as of the Phase 4 milestone (managed `Str` strings on a precise
non-moving mark-sweep heap, `concat`/`collect`/`heap_used`, string equality).
Constructs arrive phase by phase (see §15 of the architecture spec); this
document is updated with each phase and is not yet a frozen language
specification.

## 1. Source text

| Property        | Rule                                                        |
| --------------- | ----------------------------------------------------------- |
| Encoding        | UTF-8                                                       |
| Line endings    | `\n` and `\r\n` both accepted; `\r` is ignored              |
| Indentation     | Spaces only; indentation defines block structure (off-side rule) |
| Comments        | `#` to end of line                                          |
| Identifiers     | `[A-Za-z_][A-Za-z0-9_]*`                                    |
| Keywords        | `fn type let mut if else while for in return true false and or not` |

Newlines are significant statement separators. A block is a sequence of
statements at a deeper indentation level, opened by `:` at the end of the
construct header line.

## 2. Lexical grammar

```
newline     = "\n"
comment     = "#" {any-char-except-newline}
identifier  = letter {letter | digit | "_"}
int-lit     = digit {digit}
float-lit   = digit {digit} "." digit {digit}
str-lit     = '"' {escape | any-char} '"'
escape      = "\n" | "\t" | "\"" | "\\"
```

Punctuation and operators:

```
( ) [ ] , : ; . -> .. + - * / % += -= *= /= %= == != < > <= >= = and or not
```

## 3. Syntactic grammar

```
program       = { statement } EOF

statement     = fn-decl
              | type-decl
              | let-stmt
              | assignment
              | if-stmt
              | while-stmt
              | for-stmt
              | return-stmt
              | expr-stmt

fn-decl       = "fn" identifier "(" [ params ] ")" [ "->" type-expr ] ":"
                    block
params        = param { "," param }
param         = [ "mut" ] identifier [ ":" type-expr ]

type-decl     = "type" identifier ":" INDENT { field-decl } DEDENT
field-decl    = identifier ":" type-expr

type-expr     = identifier          (Int | Float | Bool | Str | Unit | record name)
              | "[" type-expr ";" int-lit "]"   (fixed-size array)

let-stmt      = "let" [ "mut" ] identifier [ ":" type-expr ] "=" expr
assignment    = lvalue ( "=" | "+=" | "-=" | "*=" | "/=" | "%=" ) expr
lvalue        = identifier { "." identifier | "[" expr "]" }

if-stmt       = "if" expr ":" block
                    [ "else" ":" block ]
while-stmt    = "while" expr ":" block
for-stmt      = "for" identifier "in" ( expr ".." expr | expr ) ":" block
                  (range form: lo..hi; otherwise for-each over an array)
return-stmt   = "return" [ expr ]

expr-stmt     = expr

expr          = or-expr
or-expr       = and-expr { "or" and-expr }
and-expr      = cmp-expr { "and" cmp-expr }
cmp-expr      = add-expr { ( "==" | "!=" | "<" | ">" | "<=" | ">=" ) add-expr }
add-expr      = mul-expr { ( "+" | "-" ) mul-expr }
mul-expr      = unary-expr { ( "*" | "/" | "%" ) unary-expr }
unary-expr    = [ "-" | "not" ] postfix
postfix       = primary { "." identifier | "[" expr "]" }
primary       = int-lit
              | float-lit
              | str-lit
              | "true" | "false"
              | identifier
              | identifier "(" [ args ] ")"
              | "[" args "]"            (array literal)
              | "(" expr ")"
args          = expr { "," expr }

block         = INDENT { statement } DEDENT
```

Operator precedence, loosest to tightest:
`or` < `and` < comparisons < `+ -` < `* / %` < unary `- not` < field
access `.` < call.

## 4. Static semantics (current)

* **Types** — `Int` (i64), `Float` (f64), `Bool` (i1 widened to i64 at ABI boundaries), `Str` (constant string), `Unit`, records (by value), fixed-size arrays (by value), and `Never` (the type of `panic`).
* **Function signatures** — parameter and return types may be annotated. Unannotated
  types are inferred by fixpoint: parameter types are refined from call sites and
  return types from `return` values, re-analyzing until the signature table stops
  changing. A function that never yields a value is `Unit`.
* **Parameters** — a plain parameter is an immutable copy (assigning to it is
  T010). A `mut` parameter is a **by-reference** parameter: the callee borrows
  the caller's binding, assignments inside the callee are visible to the caller,
  and the argument must be a mutable variable (T037).
* **Variables** — `let` introduces an immutable binding; `let mut` introduces a
  mutable binding. Assigning to a non-`mut` binding is an error (T010).
  Assigning to an undefined binding is an error (T011).
* **Scoping** — a function body is a scope; `if`/`while`/`for` bodies are nested
  scopes. Bindings introduced in a nested block (including a `for` loop
  variable) are visible only inside it; `let` may shadow an outer binding
  inside a nested scope without affecting the outer binding.
* **Conditions** — `if` and `while` conditions must be `Bool` (T001, T002).
* **Arithmetic** — operands must be numeric and of the same numeric type
  (T017/T008). There is no implicit `Int` → `Float` conversion yet. `Int`
  arithmetic is two's-complement **wrapping**; integer division by zero calls
  `panic("division by zero")` (exit 1); `MIN / -1` wraps to `MIN` and
  `MIN % -1` is `0`. `Float` follows IEEE 754 (division by `0.0` yields
  `inf`/`nan` and never traps).
* **Compound assignment** — `x op= e` is exactly `x = x op e` with the target
  path evaluated once: the current value is read, `e` is evaluated, the binary
  operation is applied with the same operand rules as `x op e` (T008/T017), and
  the result is written back with the same mutability (T010) and type (T012)
  rules as plain assignment. For paths with indices (`xs[i] += e`) the index
  expressions are evaluated exactly once and shared by the read and the write.
* **Comparisons** — operands must be the same type; ordering comparisons
  require numerics. `==`/`!=` on `Str` compare byte-wise contents. String
  ordering is not implemented.
* **Logic** — `and`, `or`, `not` require `Bool`.
* **For-each** — `for v in xs:` requires `xs` to be an array (T038). The array
  expression is evaluated once; `v` is bound (immutably) to each element in
  order, by value. Mutating `xs` inside the body does not affect the
  iteration.
* **Managed strings** — `Str` is the only managed type. `concat(a, b)` builds a
  heap string; `collect()` runs the collector; `heap_used()` returns live
  payload bytes. Managed `Str` values may not be stored in record fields or
  array elements yet (T039); they are fully usable in locals, parameters and
  returns. See `docs/runtime-abi.md`.
* **Calls** — arity (T005) and argument types (T007) are checked against the
  callee signature. `print(x)` accepts exactly one argument of any type except
  records (T013, T028).
* **Records** — `type Name:` declares a record with one or more named, typed
  fields (T027 enforces non-empty; T023 rejects duplicate fields; T024 rejects
  fields of unknown type; T022 rejects duplicate type names). Construction is
  positional: `Name(e0, e1, ...)`, checked for arity and field types
  (T005, T007). Field access `e.field` requires a record base (T020) and a
  declared field (T019). Records are **value types** — `let q = p` copies.
* **Field assignment** — `a.b.c = v` requires `a` to be a `let mut` binding
  (T010) and rewrites the aggregate value stored in `a`; the innermost value
  must match the field type (T025). No aliasing is introduced by the copy.
* **Arrays** — `[T; N]` is a fixed-size array of `N ≥ 1` values of type `T`
  (zero-length arrays are rejected: T029, T030; element type must be a value
  type: T036). Literals `[e0, e1, ...]` are non-empty and all elements must have
  the same type (T029, T035). `xs[i]` requires an array (T031) and an `Int`
  index (T032); a constant index outside `[0, N)` is a compile-time error
  (T033). A dynamic index is checked at run time: out-of-range indices call
  `panic("index out of bounds")` and terminate with exit code 1. Arrays are
  **value types** — `let ys = xs` copies, and assignment (including
  `xs[i] = v`) rewrites the whole aggregate value.
* **`len(xs)`** — takes exactly one array argument and yields its length as
  `Int` (T034); the result is a compile-time constant.
* **`panic(msg)`** — takes exactly one `Str` (T026); writes the message plus a
  newline to stderr and terminates the process with exit code 1. Its type is
  `Never`, which is compatible with every type; statements after a `panic` in
  the same block are unreachable and are not emitted.
* **`main`** — `fn main():` is the entry point. A falling-off-the-end `main`
  returns 0.

## 5. Diagnostic codes (front end)

| Code | Meaning                                          |
| ---- | ------------------------------------------------ |
| L001 | inconsistent indentation                         |
| L002 | unterminated string                              |
| L003 | unexpected character                             |
| P00x | syntax errors (unexpected token, missing token)  |
| T001 | `if` condition is not `Bool`                     |
| T002 | `while` condition is not `Bool`                  |
| T003 | return type mismatch                            |
| T004 | undefined variable                              |
| T005 | wrong number of call arguments                  |
| T006 | undefined function                              |
| T007 | call argument type mismatch                     |
| T008 | invalid binary operands                         |
| T009 | `let` annotation mismatch                       |
| T010 | assignment to immutable binding                 |
| T011 | assignment to undefined binding                 |
| T012 | assignment type mismatch                        |
| T013 | `print` arity error                             |
| T014 | duplicate function definition                   |
| T015 | `for` range start is not `Int`                  |
| T016 | `for` range end is not `Int`                    |
| T017 | unary `-` on non-numeric value                  |
| T018 | `not` on non-`Bool` value                       |
| T019 | unknown record field                            |
| T020 | field access/assignment on a non-record value   |
| T021 | *(reserved)*                                    |
| T022 | duplicate type declaration                      |
| T023 | duplicate field in a record                     |
| T024 | record field of unknown type                    |
| T025 | field assignment type mismatch                  |
| T026 | `panic` arity/type error                        |
| T027 | record declares no fields                       |
| T028 | `print` of a record value                       |
| T029 | empty array literal                             |
| T030 | zero-length array type                          |
| T031 | indexing a non-array value                      |
| T032 | array index is not `Int`                        |
| T033 | constant index out of bounds                    |
| T034 | `len` arity/type error                          |
| T035 | array element type mismatch                     |
| T036 | invalid array element type (`Unit`/`Never`)    |
| T037 | `mut` argument is not a mutable variable        |
| T038 | for-each over a non-array value                 |
| T039 | managed `Str` stored in a record or array       |
| T040 | `concat` arity/type error                      |
| T041 | `collect` arity error                          |
| T042 | `heap_used` arity error                        |

Parser codes: P001 unexpected token, P002 expected function name, P003 expected
variable name, P004 unexpected statement/token, P006 expected field name after
`.`, P007 expected type name, P008 expected field name, P009 expected field
type, P010 expected `]` after index, P011 expected `=`, `.field` or `[index]` in
an assignment target, P012 expected `;` and length in an array type, P013
expected a numeric array length, P014 expected `]` to close an array literal.

## 6. Compiler conventions relevant to semantics

* A `print(x)` call lowers to a call into the runtime print helpers
  (`nexum_print_int`, `nexum_print_float`, `nexum_print_bool`, `nexum_print_str`).
* Mutable variables and parameters live in stack slots; immutable temporaries
  are SSA values.
* `for i in a..b` is defined as: evaluate `a` and `b` once, loop `i = a` while
  `i < b`, `i += 1`.
* `for v in xs:` is defined as: evaluate `xs` once into a temporary, then
  iterate an index `0..len` reading elements by value. It desugars to the
  equivalent `while` loop before lowering.
* Records lower to LLVM struct types (`%Name = type { ... }`), passed and
  returned by value; field reads/writes lower to `extractvalue`/`insertvalue`.
  `a.b.c = v` is a functional update of the whole binding: every enclosing
  record value is rebuilt, then stored back into `a`.
* Arrays lower to LLVM array types (`[N x T]`), passed and returned by value.
  Literals build an `insertvalue` chain. A dynamic index address is computed by
  spilling the aggregate to a temporary slot and using `getelementptr`; the
  optimizer removes the round trip. Dynamic indices are preceded by an unsigned
  bounds check (`icmp ult`) that calls `nexum_panic` on failure.
* A `mut` parameter lowers to a pointer to the caller's storage: the callee
  loads and stores through the pointer, and call sites (checked by T037) pass
  the address of the argument binding.
* `panic` lowers to the bootstrap runtime helper `nexum_panic` (message to
  stderr, `exit(1)`).

## 7. Accepted-but-unspecified (to be resolved in later phases)

* Records: no methods, no generics, no `{ field = value }` literal syntax
  (construction is positional), and field order is the declaration order for
  ABI purposes.
* Arrays are fixed-size only: there is no slice/view type, no growable list, no
  array-of-array annotation sugar beyond `[[T; N]; M]`, and no array slicing.
* `mut` parameters borrow a whole binding; there is no way to borrow a field or
  element (`f(a.b)` is rejected). Aliasing of two `mut` parameters bound to the
  same caller binding is not diagnosed yet.
* Modules: a source file is a module; there is no import syntax yet (see
  `decisions.md`). Error handling: `panic` plus compile-time diagnostics only;
  structured error values and `Result`-like types are a later-phase decision.
* No implicit numeric conversions; overflow behavior undefined until the
  runtime ABI lands.
* Falling off the end of a value-returning function yields a **zero value**
  of the return type (`0`, `0.0`, `false`); this is bootstrap behavior and
  will become a compile-time error or explicit rule later.
* `Float` printing currently uses C `%g` (`5.0` prints as `5`); a runtime
  formatter lands with the runtime ABI in Phase 4.
* Messages emitted by `panic` currently pass through C `fputs`; a structured
  failure API (stack traces, source locations, error codes) lands with the
  runtime in Phase 4.
