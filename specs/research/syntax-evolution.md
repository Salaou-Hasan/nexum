# How the syntax grows for the systems work (phases 11b-11f)

**Question.** The systems work (phases 11b-11f) needs inline assembly, a
numeric tower, pointers and layout control, threads, and vectors. The
language today is small and closed: sixteen reserved words, no braces, an
off-side rule, and a hand-written recursive-descent parser that is mirrored
by a self-hosted parser in Nexum. How should the surface syntax grow for
those five phases *without* the language turning into a different language,
and which of the five need a grammar change (and therefore a change to the
self-hosted parser) and which can be built on the grammar that exists?

**Summary.** Only two of the five phases need new syntax at all. The
recommendation is: **phase 11f (vectors) needs no grammar change whatsoever**
(a vector type is a *named* type and a vector literal is a *call to a type
name*, which is exactly how records are constructed today); **phase 11c (the
numeric tower) needs no grammar change for the tower itself** — the type
names are free and the conversions are builtins — and the literal *suffixes*
are optional sugar that can be deferred to a separate slice; **phase 11e
(concurrency) needs exactly one new reserved word**, `spawn`, added to the
slot the language already has for prefix operators; **phase 11d (pointers and
layout) needs the most**, roughly one new word (`unsafe`), one new type form
(`*T`), two new prefix operators (`*p` and `&p`) and two *contextual* words
(`packed`, `align`); and **phase 11b (inline assembly) should ship as a
builtin** — `asm(template, constraints, operands...)` — which needs no
grammar change at all, because LLVM's own inline assembly has exactly that
shape. The total budget for the whole programme is therefore about **two new
reserved words** (`unsafe`, `spawn`), **one new token** (`&`) and **about ten
new AST nodes**, and the lexer is the single
serialisation point: the *suffix* work in 11c is the only change that is
expensive in the lexer, and it should be bundled with the `&` token from
11d rather than done twice. Everything else can be built on the grammar that
exists, which means 11b, 11c and 11f can proceed **in parallel** with 11d
and 11e, and the differential gate only has to be re-exercised twice.

## Recommendation

1. **Adopt the house style as a written test, and apply it to every new
   form.** A new *statement* is a lowercase reserved keyword, then operands
   in the same slots the neighbours use, then `:` and an indented body — the
   shape of `if`/`while`/`for`. A new *expression* is either a builtin called
   like a function (`print`, `concat`, `list_push`) or a call to a *type
   name* in positional form (`Point(1, 2)`). A new *type* is a capitalised
   name, which the grammar already accepts for free. A new *annotation* is a
   contextual word, not a reserved word. A new *operator* is one
   punctuation character, added to the existing prefix-operator slot. **The
   grammar grows by slots, not by new shapes.** (Reason: the language has
   exactly one shape for a scoped body — keyword, `colon`, indent — and
   reusing it is what keeps the language recognisable.)

2. **Phase 11b, inline assembly: ship as a builtin; no grammar change.**
   The surface is `asm(template, constraints, operands...)`, an expression,
   whose result is the value of the first output operand. The template is a
   string with `$0`-style placeholders; the constraints are a string in the
   GCC/LLVM constraint syntax (`"=r,r"`, `"=a,=S"`, `~{memory}`); the operands
   are ordinary Nexum expressions. This maps one-to-one onto LLVM's `asm`
   instruction, so the implementation is a codegen change, not a front-end
   change. A `let r = asm(...)` reads exactly like `let x = concat(a, b)`,
   which is the house style. **Do not take `asm` as a reserved word in 11b.**
   (Reason: an asm block has operands and a result and no scope and no
   binding, which is the definition of a builtin; and taking the word costs
   a source-compatibility break for nothing.)

3. **Phase 11c, the numeric tower: no grammar change for the tower; defer
   the suffixes.** The type names `F32`, `F64`, `I8`..`I64`, `U8`..`U64` are
   *named* types and the grammar already accepts any identifier as a type, so
   they cost nothing in the front end; the conversions are `to_f32(x)`,
   `to_i32(x)` builtins and cost nothing either. The literal suffixes
   (`42i8`, `1.0f32`) are *optional sugar* and are the only part of 11c that
   touches the lexer, because today `1.0f32` lexes as `Float("1.0")`
   followed by `Ident("f32")` (verified with `nx lex`). **Recommend
   deferring the suffixes, hex/octal/binary literals and exponent notation
   to a separate slice (11c.2) and bundling them with the one new token
   11d needs.** (Reason: the tower's semantics — the type of a literal comes
   from its context — do not require the suffix spelling; and the suffix
   change is the single most expensive lexer change in the programme,
   because it rewrites the number scanner in *both* lexers and forces the
   AST to carry a literal's text rather than its value.)

4. **Phase 11d, memory and layout: the largest grammar change, and the one
   that gates 11b's ergonomics.** Four pieces, in increasing order of cost:
   - the pointer type `*T` (one new case in the type grammar; the `*`
     character already has a token);
   - the dereference `*p` and the address-of `&p` (two new prefix
     operators; `*` already has a token, `&` needs one new token in the two
     lexers);
   - the `unsafe` region, spelled `unsafe:` plus an indented body — the
     same shape as `if` and `while`, one new reserved word;
   - the layout annotations `packed` and `align`, spelled as **contextual
     words** on the `type` declaration and on the field line, so they cost
     no lexer change and break no existing program.
   **The model to adopt: creating a pointer is safe, dereferencing is
   unsafe.** Pointer creation is an integer cast; the dangerous operation is
   `*p`, and it must sit inside an `unsafe` region. That model keeps the
   `unsafe` region a *statement* (so it introduces no value that has to be
   threaded out) and lets the region use bindings from enclosing scopes.
   (Reason: it is the only split that works without a place-value type, and
   it matches the "unsafe region" wording in the roadmap.)

5. **Phase 11e, concurrency: one new reserved word, `spawn`, in the prefix
   slot.** `spawn` is a prefix operator beside `-` and `not`, so
   `let h = spawn compute(n)` is `unary-expr = [ "-" | "not" | "spawn" ]
   postfix` with one more alternative — the smallest possible grammar change
   for a control-flow-shaped feature. `join(h)`, `channel_new()`,
   `channel_send(ch, v)`, `channel_recv(ch)`, `atomic_new(0)`,
   `atomic_load(a)`, `atomic_store(a, v)`, `atomic_add(a, v)`,
   `atomic_cas(a, e, n)` and `yield_cpu()` are all **builtins** and need no
   grammar. The `Atomic` and `Channel` types are named builtin types, like
   `List` is today. (Reason: a spawn is not an ordinary call — it creates a
   thread — and making that visible in the syntax is worth one word; the
   builtin alternative `spawn(f, args...)` would have to be variadic or
   would need closures, which the language does not have.)

6. **Phase 11f, SIMD and vectors: no grammar change at all.** The vector
   type is a named type (`F32x4`, `I32x8`, `F64x2`); the vector literal is
   the *positional construction of a type*, `F32x4(1.0, 2.0, 3.0, 4.0)`,
   which is exactly the record-construction shape the language already has
   (`Point(1, 2)`). The parser already produces a call node for it; only the
   name resolution has to learn that the name denotes a type rather than a
   function. Strides are a builtin (`stride(x)`), and the intrinsics
   (`dot(a, b)`, `fma(a, b, c)`, `hadd(a, b)`, `abs4(a)`) are builtins.
   (Reason: the language's aggregate-construction rule *is* "call a type
   name with the values in order"; inventing a new literal shape for
   vectors would be the one place in 11b-11f that genuinely made the
   language a different language.)

7. **Spend the reserved-word budget deliberately: two words, no more.**
   Recommend taking `unsafe` (11d) and `spawn` (11e) as reserved words, and
   *not* taking `asm`, `join`, `packed`, `align`, `volatile`, `ptr`, `vec`,
   `atomic`, `channel` — those are builtins or contextual words. The cost of
   a reserved word is small in the toolchain (about three lines in each of
   the two lexers, plus a dispatch arm in each parser) but it is a **source-
   compatibility break**: `unsafe`, `spawn` and `asm` all parse as ordinary
   identifiers today (verified), so every program that uses one of them
   stops compiling. Contextual words avoid that break at zero cost, and
   they should be the default for annotations.

8. **Serialise the lexer, parallelise everything else.** Every syntax
   change has to be made twice — once in Rust, once in Nexum — because the
   normalised AST of the Rust parser is compared against the normalised AST
   of the self-hosted parser in the differential gate. The *lexer* has the
   same double cost, and the two lexer changes in this programme (`&` for
   11d, and the numeric suffixes for 11c) should be **one PR**, not two.
   Everything else — the type name `*T`, the deref operator, the `unsafe`
   statement, the `spawn` prefix, the vector constructor, and all the
   builtins — can be done in parallel with each other, because none of them
   changes the token stream.

9. **Keep the alternatives open, and use them by default.** Two of the five
   phases (11b and 11f) are better served by existing forms than by new
   ones, and the *majority* of the surface of 11d and 11e is builtins rather
   than syntax. The rule to apply: **a builtin is the right answer whenever
   the feature has operands and a result and introduces no scope, no binding
   and no literal that the parser has to recognise.** Only `unsafe` (a
   scope) and `spawn` (a control-flow position) clear that bar, and only
   `unsafe` needs a new scope form at all.

## What the code looks like today

### The parser is hand-written, and the grammar document is not its input

`crates/nexum-parser/src/lib.rs` is a hand-written recursive-descent parser:
a `Parser` struct over a token slice (`lib.rs:273-279`), one function per
grammar production (`parse_stmt` at `lib.rs:336`, `parse_fn` at `lib.rs:508`,
`parse_type_decl` at `lib.rs:571`, `parse_let` at `lib.rs:682`, `parse_if` at
`lib.rs:718`, `parse_while` at `lib.rs:745`, `parse_for` at `lib.rs:759`,
`parse_return` at `lib.rs:787`, `parse_import` at `lib.rs:804`,
`parse_block` at `lib.rs:830`, the expression chain at `lib.rs:872-1160`).
`docs/language-reference/grammar.md` §3 is a *description* of that parser,
maintained by hand; there is no parser generator and no grammar file that
produces code.

### The current syntactic shape

**Blocks are off-side, and the colon is decoration.** The lexer measures the
indentation of each line and emits `Indent`/`Dedent` markers
(`crates/nexum-lexer/src/lib.rs:207-231`); the parser opens a body by
expecting an `Indent` token (`parser lib.rs:830-862`). The `:` that ends a
header line is *consumed but not required* — every header does
`if self.kind() == TokenKind::Colon { self.advance(); }` (`parser lib.rs:555`,
`580`, `721`, `727`, `748`, `776`). Verified: `fn f() print(1)` parses, and
its body is the single statement `print(1)`.

**Statements** (`grammar.md:48-56`, `parser lib.rs:336-506`): `fn`, `type`,
`let`, assignment (plain and path), `if`/`else`, `while`, `for`, `return`,
`import`, and an expression statement. A statement that begins with an
identifier is disambiguated from an expression statement by a **lookahead
scan** for the path-assignment shape (`parser lib.rs:347-412`): the parser
walks ahead over `.ident` and `[...]` segments looking for a top-level `=`
or `op=`. A statement that begins with a *keyword* is dispatched on the
token kind alone (`parser lib.rs:339-346`). This asymmetry matters: a new
keyword form is trivial to add, a new identifier-led form is not.

**Expressions** (`grammar.md:85-101`, `parser lib.rs:872-1160`): a
precedence ladder `or < and < comparison < +- < */% < unary < postfix <
call`. The unary level accepts `-` and `not` only (`parser lib.rs:1000-1027`).
The postfix level accepts `.field` and `[index]` only (`parser
lib.rs:1029-1076`). The primary level accepts an integer literal, a float
literal, `true`, `false`, a string, an identifier, an identifier followed by
an argument list, a parenthesised expression, and a bracketed list
(`parser lib.rs:1077-1160`).

**A call is always to a bare name.** `ExprKind::Call` holds
`callee: String` (`parser lib.rs:54-58`) — not an expression. So there is no
method call and no qualified call: `xs.len(1)` is a parse error today
(verified: `error[P001]: expected RParen, found LParen`). This is why
`join(h)`, `to_f32(x)` and `F32x4(1.0, 2.0)` are all expressible and
`xs.join(h)` is not.

**Literals.** Integers and floats only: `int-lit = digit {digit}`,
`float-lit = digit {digit} "." digit {digit}` (`grammar.md:31-32`), scanned
by `crates/nexum-lexer/src/lib.rs:304-329`. The scan stops at the first
character that is not a digit or a `.` followed by a digit. There is no
hexadecimal, no exponent, no suffix, no binary literal. Verified with
`nx lex`: `1.0f32` produces `Float "1.0"` then `Ident "f32"`, and `42i8`
produces `Int "42"` then `Ident "i8"`.

**Types** are `type-expr = identifier | "[" type-expr ";" int-lit "]"`
(`grammar.md:66-67`), parsed at `parser lib.rs:648-680`, and represented as
`TypeExpr::Named(String) | TypeExpr::Array(Box<TypeExpr>, usize)`
(`parser lib.rs:12-16`). **Any new primitive type name is therefore free** —
`Int`, `Float`, `Bool`, `Str`, `Unit` are resolved by name in
`crates/nexum-sema/src/lib.rs:540-544`, and `List` is a builtin type in the
same way (`sema lib.rs:15-32`). Verified: `let x: Ptr = 1` parses today and
normalises to `named Ptr`.

**Record declarations** are `type Name:` followed by an indented list of
`field: Type` lines (`grammar.md:63-64`, `parser lib.rs:571-646`). The field
parser reads the name, the type, and then **silently discards the rest of
the line** (`parser lib.rs:630-635`) — which is why a trailing annotation on
a field line is cheap to add later. Construction is positional
(`Name(e0, e1)`), field access is `e.field`, field assignment rewrites the
whole aggregate, and records are value types (`grammar.md:166-176`).
There is no `{ field = value }` literal and no method syntax
(`grammar.md:287-291`).

**`import`** is `import "path.nx"`, top level only, with the path relative
to the importing file, each file loaded once, and one shared namespace with
no qualified names and no per-module visibility (D002 in
`docs/language-reference/decisions.md:25-39`; parser `lib.rs:804-828` and
the loader at `lib.rs:196-271`).

**Builtins are called like functions.** `print(x)`, `panic(msg)`,
`len(x)`, `concat(a, b)`, `slice(s, lo, hi)`, `str_char_at(s, i)`,
`collect()`, `heap_used()`, `read_file(p)`, `write_file(p, c)`,
`list_new()`, `list_push(l, v)`, `list_len(l)`, `list_get(l, i)`,
`list_set(l, i, v)` (`docs/standard-library.md:9-34`). They are resolved by
name in the semantic pass (`sema lib.rs:1184`, `1316`, `1502`, `1545`,
`1596`) and lowered directly by the code generator; they have no source
definition and their names cannot be shadowed. **A builtin costs the front end
nothing**: the parser produces an ordinary call node and the semantic pass
decides what the name means.

**The keyword set** is sixteen words: `fn type let mut if else while for in
return true false and or not import` (`grammar.md:19`,
`crates/nexum-lexer/src/lib.rs:13-28`, `342-359`). The editor has a fourth
copy of the list at `editors/vscode/syntaxes/nexum.tmLanguage.json:26` —
which has already drifted, since `import` is missing from it.

### The grammar's cost: every syntax change is made twice

The self-hosted compiler parses the same source with its own parser and
prints it in the same normalised form, so the two can be compared. The
normalised form is the contract: a line-oriented tree, one node per line,
two spaces of indent per level (`crates/nexum-parser/src/normalize.rs:1-11`).

The two gates are:

* `selfhosted_lexer_matches_rust_lexer`
  (`crates/nexum-driver/tests/backend.rs:143-194`) — the Rust lexer's token
  stream (`kind.name()` + text) against the self-hosted lexer's, for every
  `.nx` file in `examples/`. `TokenKind::name()`
  (`crates/nexum-lexer/src/lib.rs:65-123`) is the stable spelling of the
  token kinds, so **a new token kind needs a `name()` arm**, and the
  self-hosted lexer's `keyword_kind()`, `two_char_kind()` and
  `single_char_kind()` (`examples/selfhost/lexer_lib.nx:37-131`) must grow
  the same vocabulary.
* `selfhosted_parser_matches_rust_parser`
  (`backend.rs:199-249`) — the Rust parser's normalised output against the
  self-hosted parser's, for every fixture in
  `crates/nexum-driver/tests/fixtures/parse/`. The self-hosted parser
  (`examples/selfhost/parser.nx`) is a flat arena of `Node` records with
  three integer slots (`kind`, `text`, `a`, `b`, `c`) plus a side table for
  variable-length child lists; every node kind it knows has a `print_node`
  arm (`parser.nx:638-764`) that must match the Rust normaliser
  (`normalize.rs:66-245`) line for line.

So a syntax change is made **four** times, not twice:

| # | Place | What it costs |
| - | ----- | ------------- |
| 1 | Rust lexer (`crates/nexum-lexer/src/lib.rs`) | a `TokenKind` variant, a `name()` arm, a scan rule |
| 2 | Rust parser (`crates/nexum-parser/src/lib.rs`) | an AST variant, a parse function, a dispatch arm |
| 3 | Rust normaliser (`crates/nexum-parser/src/normalize.rs`) | a printing arm |
| 4 | Self-hosted lexer + parser (`examples/selfhost/*.nx`) | the same vocabulary, the same node kind, the same printing arm |

plus a fifth place that is easy to forget: the **formatter** in the driver
(`crates/nexum-driver/src/main.rs:522-840`, one arm per `StmtKind` and per
`ExprKind`), and a sixth: the **grammar document** and the **decision
record** (`docs/language-reference/grammar.md` §3-§4 and a new numbered entry
in `docs/language-reference/decisions.md`, per `specs/AGENT-BRIEF.md:129-131`).

Two facts make the cost higher than it first looks:

* **The self-hosted parser is already behind.** It does not yet parse path
  assignments (`parser.nx:17`, and the two examples that use them are the
  known exceptions in `docs/self-hosting.md:70-74`). Every grammar change
  lands on a parser that has not finished the current one.
* **The token stream is a byte-exact contract.** The lexer gate compares
  kinds *and* text. A change to the number scanner, for example, cannot be
  "close enough".

**The cost model this yields** (the numbers are the real line counts from
the files above):

| Kind of change | Lines in the Rust side | Lines in the Nexum side | Other | Total |
| -------------- | ---------------------- | ----------------------- | ----- | ----- |
| A new reserved word | 3 (variant, `name()`, keyword arm) | 3 (`keyword_kind` arm) | editor list; source break | ~8 + a break |
| A new single-character token | 3 | 3 | editor list | ~8 |
| A new prefix operator (`*p`, `&p`) | ~10 (AST + parse + normalise) | ~10 | formatter | ~25 |
| A new statement form | ~30 | ~30 | formatter, grammar doc, decision | ~70 |
| A new literal form (numeric suffix) | ~40 (number scanner + AST + normalise) | ~40 | grammar doc, decision | ~90 |
| A new *type name* (`F32`, `F32x4`, `Atomic`) | 0 | 0 | sema only | ~0 |
| A new *builtin* (`join`, `to_f32`, `stride`) | 0 | 0 | sema + codegen | ~0 in the front end |

**This table is the budget.** The whole 11b-11f programme fits in roughly
**two reserved words** (`unsafe`, `spawn`), **one new token** (`&`), and
**about ten new AST nodes** — everything else is type names and builtins.
The one item that does not fit the pattern is the numeric suffix, at roughly
three times the cost of a statement form, for optional sugar. That is the
single most important scheduling fact in this document.

## The design

### The house style, stated as a test

A new form is acceptable if it reads like the forms already there. Concretely:

1. **Keyword-led forms.** Every existing statement that begins with a word
   begins with a *reserved keyword*, and the keyword names the operation in
   one lowercase English word: `fn`, `type`, `let`, `mut`, `if`, `else`,
   `while`, `for`, `in`, `return`, `import`. A new statement follows: `asm`,
   `unsafe`, `spawn`. Not `Asm`, not `ASM`, not `unsafe_block`.
2. **The body shape is fixed.** `keyword <operands> :` then an indented body,
   exactly as `if cond:`, `while cond:`, `for x in xs:`, `type Name:` do.
   The `unsafe:` region is this shape and nothing else.
3. **Operands go in the usual slots.** The operand of a prefix form is an
   expression (`not x`, `-x`); the operands of a call are a comma-separated
   expression list (`print(x)`, `list_push(l, v)`); the operand of a
   `for` is introduced by `in`. A new form borrows one of these slots
   rather than inventing a parameter syntax.
4. **Types are capitalised nouns; values and calls are lowercase.** `Int`,
   `Float`, `Str`, `Bool`, `Unit`, and (by convention) user record names
   `Point`, `Particle`; builtins and functions are lowercase
   `snake_case`. So a new type is `F32x4` and a new builtin is `to_f32`.
5. **Builtins are lowercase and called like functions**, with the argument
   order *receiver first* (`list_push(l, v)`, `list_set(l, i, v)`).
6. **Operators are symbolic; words are reserved for word-operators.** `and`,
   `or`, `not` are words because they are boolean operations; `+ - * / %`
   are symbols. A new symbolic operator (`*p`, `&p`) is a symbol; a new
   word-operator (`spawn`) is a word.
7. **No braces, ever.** A block is indentation. The only bracket characters
   in the language are `()`, `[]`; `{}` does not even lex (`L003`,
   verified).

Read next to the existing forms, the new ones look like this:

```
# 11d — the pointer model
let p = addr_of(x)          # a builtin, or &x
*p = 1                      # a prefix operator, like -x and not x
unsafe:                     # a keyword, a colon, an indented body
    *p = 1

# 11e — concurrency
let h = spawn compute(n)    # a prefix operator, like -x and not x
let r = join(h)             # a builtin, like list_len(l)

# 11f — vectors
let v = F32x4(1.0, 2.0, 3.0, 4.0)   # a call to a type name, like Point(1, 2)
let d = dot(a, b)                    # a builtin, like concat(a, b)

# 11c — the tower
let x: F32 = 1.0            # a type name; free
let y = to_f32(1.0)         # a builtin; free
```

### Phase 11b — inline assembly

**Surface (recommended): a builtin, an expression.**

```
let r = asm("mov $0, $1", "=r,r", x, y)
```

* `template` — a string. `$0`, `$1`, ... are placeholders for the operands,
  substituted with the concrete registers the constraint string selects. The
  spelling matches LLVM's inline assembly exactly, so the lowering is a
  direct translation.
* `constraints` — a string in the GCC/LLVM constraint syntax. One
  constraint per operand (`r` general register, `a`/`b`/`c`/`d`/`S`/`D`
  specific registers, `m` memory, `i` immediate, `=r` output, `+r`
  read-write). Clobbers appear in the same string as `~{memory}` or
  `~{dirflag}`. The constraint string is *parsed and validated at compile
  time* by the front end; a malformed constraint is a compile-time
  diagnostic, not a runtime failure.
* `operands` — ordinary Nexum expressions, one per non-clobber constraint.
  Their types are checked against the constraint's class: an integer
  constraint takes an `Int` (or a fixed-width integer in 11c), a vector
  constraint takes a vector.
* **Result** — the value of the first output (`=`) operand; `Unit` when
  there is none. The type of the result follows the constraint's width.

**Why this is the right surface.** The feature has operands and a result
and introduces no scope, no binding and no literal the parser must
recognise — which is exactly the definition of a builtin in this language
(`docs/standard-library.md:57-61`). It lowers to LLVM's `asm` instruction
with essentially no design work, which is what the phase is for: the
runtime's system-call layer already needs exactly this shape
(`docs/native-roadmap.md:124-130`).

**What is deferred, and why.** The ergonomics of *naming a register* and
*tying an operand to a place in memory* need pointers (11d). A device
register is at an address; a buffer is at an address. So:

* 11b.1 (now): the template form above. Register-constrained, value
  operands. Enough for the runtime's syscall layer, where the values are
  integers and the result comes back in a register.
* 11b.2 (after 11d): the same builtin, but the constraint string may also
  name a *pointer* operand, so a device register can be read and written.

**The optional upgrade, and its cost.** If the constraint-string surface
proves too error-prone for ordinary users, the upgrade is a keyword
statement with a structured operand list:

```
asm "mov $0, $1" in rbx = y, out rax
```

That costs one reserved word (`asm`), one statement form, one AST node, one
normaliser arm, one self-hosted parse function and one print arm — roughly
the "~70 lines" row of the cost model, plus a source-compatibility break.
**The recommendation is not to pay it in 11b.** Revisit when 11d has
pointers and the register vocabulary has settled; the builtin stays as the
low-level escape hatch either way.

### Phase 11c — the numeric tower

**The tower itself: no grammar change.**

* The types `F32`, `F64`, `I8`, `I16`, `I32`, `I64`, `U8`, `U16`, `U32`,
  `U64` are *named types*. `type-expr = identifier` (`grammar.md:66`) and
  the semantic pass resolves a name to a type (`sema lib.rs:540-544`). A new
  primitive type is therefore a semantic change only.
* The conversions are builtins: `to_f32(x)`, `to_f64(x)`, `to_i8(x)`,
  `to_i32(x)`, `to_u16(x)`, and so on. A builtin costs the front end
  nothing. This also matches the sibling numeric-tower research on branch
  `research/numeric-tower`, which recommends exactly this: explicit `to_*`
  builtins, no implicit conversion.
* The literal-typing rule — an unsuffixed literal takes its type from its
  context — is a *semantic* change: the literal's type becomes a type
  variable that the lattice resolves. `ExprKind::Int(i64)` already carries
  the value, so the AST does not have to change for unsuffixed literals.

**The suffixes: optional, and deferred.**

```
let a = 42i8        # today: Int "42" then Ident "i8"
let b = 1.0f32      # today: Float "1.0" then Ident "f32"
```

(Verified with `nx lex`; the current lexer stops the number at the first
character that is not a digit or a `.` followed by a digit —
`crates/nexum-lexer/src/lib.rs:304-329`.)

The cost of supporting them is the **number scanner in both lexers**
(`crates/nexum-lexer/src/lib.rs:304-329` and
`examples/selfhost/lexer_lib.nx:254-282`, about thirty lines each), plus:

* a rule for what happens to `42abc` — the maximal alphanumeric run after
  the digits is either a known suffix or an error, and that rule has to be
  implemented identically in the two lexers, because the lexer gate
  compares the token streams byte for byte;
* the AST has to keep the literal's *text* rather than its value, because
  `0xFFFFFFFFFFFFFFFF` does not fit in the `i64` that `ExprKind::Int`
  holds today (`parser lib.rs:49`); that is an AST change, a normaliser
  change and a self-hosted parser change;
* hex/octal/binary and exponent notation belong in the same change, or the
  number scanner has to be rewritten twice.

**Recommendation:** do the tower in 11c with no grammar change; put the
suffixes, the bases and the exponents in a single **11c.2** slice, and
schedule that slice together with 11d's `&` token so the lexer is touched
once. If the orchestrator decides the suffixes are needed in 11c proper,
then 11c.2 moves up and 11d's `&` moves down to meet it — but nothing else
changes, because nothing else touches the lexer.

### Phase 11d — memory and layout

**The pointer type: `*T`.**

```
let p: *Int = addr_of(x)
```

One new case in `parse_type_expr` (`parser lib.rs:648-680`): a `Star`
token followed by a type expression. The `*` character already has a token
(`TokenKind::Star`, `crates/nexum-lexer/src/lib.rs:43`), so **no lexer
change**. The AST gains `TypeExpr::Ptr(Box<TypeExpr>)`, the normaliser
gains an arm, the self-hosted parser's `parse_type` (`parser.nx:354-369`)
gains a case.

*Fallback if the budget is tight:* a nominal `Ptr` type with no pointee
costs **literally nothing** — verified, `let x: Ptr = 1` already parses and
normalises to `named Ptr` — at the price of losing the pointee type. The
recommendation is `*T`; `Ptr` is the escape hatch if the parser work has to
be minimised.

**Dereference and address-of: `*p` and `&p`.**

```
*p = 1            # dereference and store
let v = *p        # dereference and load
let q = &x        # address-of
```

* `*p` is a **prefix operator**, added to the slot the language already has:
  `unary-expr = [ "-" | "not" ] postfix` (`grammar.md:91`,
  `parser lib.rs:1000-1027`) becomes `[ "-" | "not" | "*" ] postfix`. There
  is no ambiguity with multiplication, because a prefix `*` has no left
  operand — `a * b` is consumed by the `*` level (`parser lib.rs:977-999`).
  **No lexer change.**
* `&p` needs a **new `Amp` token** — `&` does not lex today (verified:
  `error[L003]: unexpected character '&'`). That is three lines in each
  lexer (`crates/nexum-lexer/src/lib.rs`: a `TokenKind` variant, a
  `name()` arm, a `single_char_kind` arm; `examples/selfhost/lexer_lib.nx`:
  a `single_char_kind` arm) plus the editor's operator list. Cheap, and the
  only lexer change 11d forces.

*Cheaper alternative:* `addr_of(p)` as a builtin, at zero front-end cost.
The house style has no address-of yet (D001 records that there is no
`&mut`-style borrow syntax), so there is no style to break — but `&p` is
what a reader expects in a systems language, and the price is six lines.
**Recommendation: `&p`.**

**The unsafe region: `unsafe:` plus an indented body.**

```
unsafe:
    *p = 1
    v = *q
```

This is the shape of `if` and `while` (keyword, `colon`, indented body),
so it is one new reserved word, one new statement form, one AST node
(`StmtKind::Unsafe { body }`), one normaliser arm, one self-hosted parse
function and one print arm, one formatter arm.

**The model, which is the part that actually needs deciding:** *creating* a
pointer is safe; *dereferencing* is unsafe. Pointer creation is an integer
cast (`addr_of`); the dangerous operation is `*p`, and it must be inside an
`unsafe` region. Two consequences:

* The region is a **statement**, not an expression, so it introduces no
  value that has to be threaded out of it. It is a *marker on a region of
  code*, and the bindings inside it are scoped to it (D006), but the
  operations inside it can read and write the enclosing bindings — so
  `*p = 1` inside the region does write the outer `p`.
* The rule is checkable in the semantic pass: a `Deref` expression outside
  an `unsafe` region is a diagnostic. It needs a *region depth* counter in
  the checker, not a type.

*Alternative considered:* making `unsafe` an expression that wraps a single
operation (`let p = unsafe ptr_from(addr)`). It needs a second form, and
the "region" wording in the roadmap is lost. **Recommendation: the
statement form only.**

**Layout annotations: contextual words.**

```
type Header packed:
    magic: U32
    length: U32 align 4
```

`packed` and `align` are **contextual words**: the parser recognises them
in one position, and everywhere else they remain ordinary identifiers. That
means:

* **no lexer change at all** — the grammar's `identifier` already covers
  them, and the self-hosted lexer needs no new vocabulary;
* **no source-compatibility break** — `let packed = 1` still works;
* the Rust parser checks for an `Ident` whose text is `packed` after the
  type name (`parser lib.rs:571-582`) and for an `Ident` whose text is
  `align` followed by an integer after a field's type
  (`parser lib.rs:609-635`, where the rest of the line is currently
  discarded, so the change is local);
* the self-hosted parser must mirror it in `parse_type_decl`
  (`parser.nx:536-574`), and the normaliser must print it — which is a
  change to the *normalised form*, and therefore a change to the
  differential contract on both sides at once.

**Why not a reserved word for `packed`?** Because it is an *annotation*,
and the language has no attribute mechanism; a reserved word would be a
whole new syntactic category (a word that is only a keyword in one place)
for one feature. Contextual words are the cheapest way to have annotations
at all, and they cost the toolchain nothing.

**`volatile`: builtins first.**

```
let v = volatile_load(p)
volatile_store(p, 0x1234)
```

Zero grammar. A `volatile` *annotation* on a pointer type
(`type Reg: volatile *U32`) is a later refinement, and when it comes it
should be a contextual word like `packed`.

**Arenas: builtins first.** D009 and `docs/runtime-abi.md` §6 specify a
bump-allocated region with a single release point and LIFO release on
early returns. The surface `arena_new()`, `arena_alloc(a, n)`,
`arena_release(a)` is builtins — zero grammar — and it makes the release
point *explicit*, which is what the specified model says. The alternative
is an `arena:` block that releases at the dedent, which would be a new
keyword and a new statement form and would have to define the interaction
with `return` (the runtime ABI says release is LIFO and runs on early
returns, so a block form has to run releases on the way out of the
function, which is a code-generation change, not a syntax change).
**Recommendation: builtins now; the block form is a later refinement, if
at all.**

### Phase 11e — concurrency

**`spawn` is a prefix operator.**

```
let h = spawn compute(n)
let r = join(h)
```

Adding `spawn` to the unary level (`unary-expr = [ "-" | "not" | "spawn" ]
postfix`) makes the operand a full postfix expression, so `compute(n)`
binds as the call before `spawn` applies to it. The grammar change is one
alternative in one production (`parser lib.rs:1000-1027`), one AST variant
(`ExprKind::Spawn(Box<Expr>)`), one normaliser arm, one case in the
self-hosted `parse_unary` (`parser.nx:217-227`), and one formatter arm. One
reserved word: `spawn`.

**Why a keyword and not a builtin.** A builtin would have to be
`spawn(compute, 1, 2)` — variadic, with the callee passed as a *name*,
because `ExprKind::Call` holds a name rather than an expression
(`parser lib.rs:54-58`). Or it would need a closure, and the language has
no closures. The keyword form reads better, marks the spawn visibly in the
source (which matters for a concurrency feature), and leaves room for
`spawn` to grow options later without changing a call convention.

*The fallback if the keyword is refused:* `spawn(f, args...)` as a variadic
builtin — zero grammar — at the price of an awkward call and a name that
cannot also be a user function.

**Everything else in 11e is a builtin.**

```
let ch = channel_new()               # Channel: a named builtin type
channel_send(ch, 42)
let v = channel_recv(ch)

let a = atomic_new(0)                # Atomic: a named builtin type
let old = atomic_load(a)
atomic_store(a, 1)
atomic_add(a, 1)
let ok = atomic_cas(a, 1, 2)

yield_cpu()
```

`Atomic` and `Channel` are named builtin types in the same way `List` is
today (`sema lib.rs:15-32`), so they need no grammar. `join(h)` is a
builtin too: the thread handle is a named builtin type, and `join` is the
one operation on it.

**One reserved word for the whole phase.**

### Phase 11f — SIMD and vectors

**No grammar change. None.**

* **The vector type** is a named type: `F32x4`, `F32x8`, `I32x4`, `F64x2`,
  `I8x16`. Capitalised, like every other type.
* **The vector literal** is the positional construction of a type:

  ```
  let v = F32x4(1.0, 2.0, 3.0, 4.0)
  ```

  This is *exactly* the record-construction shape the language already has
  (`Point(1, 2)`, `Particle(1.0, 2.0)` in `examples/particles.nx:14`). The
  parser already produces `ExprKind::Call { callee: "F32x4", args }` for it
  (`parser lib.rs:1103-1122`); only the name resolution has to learn that
  the name denotes a *type* rather than a function, and the semantic pass
  has to check the arity (four) and the element type (`F32`).
* **Strides** are a builtin: `stride(x)`, `stride_of(p)`. The alternative —
  a stride in the array *type*, `[T; N; S]` — would be a change to the type
  grammar for something a builtin expresses perfectly well.
* **The intrinsics** are builtins: `dot(a, b)`, `fma(a, b, c)`,
  `hadd(a, b)`, `abs4(a)`, `min4(a, b)`, `max4(a, b)`, `fmadd256(a, b)`.
  They are ordinary calls; the code generator maps them to the LLVM
  intrinsics or to inline assembly.

**Why this is the right answer and not a new literal.** A new literal
shape — say `<<1.0, 2.0, 3.0, 4.0>>` or `f32x4[1.0, 2.0, 3.0, 4.0]` —
would be the one place in 11b-11f that makes the language a different
language: a second aggregate syntax, a second set of tokens, and a
normalised form that has to grow a new node for no gain. Using the
construction-of-a-type rule keeps the language's aggregate story in one
place: *a value of an aggregate type is written by naming the type and
listing the elements*.

### The reserved words: what each feature needs, and what taking one costs

**The current sixteen** are `fn type let mut if else while for in return
true false and or not import` (`crates/nexum-lexer/src/lib.rs:13-28`).

**What the five phases need:**

| Word | Needed for | Recommend? | Why |
| ---- | ---------- | ---------- | --- |
| `unsafe` | 11d, the unsafe region | **yes, reserved** | it introduces a scope, and a scope needs a keyword |
| `spawn` | 11e, the thread spawn | **yes, reserved** | it is a control-flow position, and the prefix-operator slot already exists |
| `asm` | 11b, only if the statement form is chosen | **no, not in 11b** | the builtin form needs no scope, no binding and no new literal |
| `packed` | 11d, layout | **no, contextual** | an annotation, and contextual words are free |
| `align` | 11d, layout | **no, contextual** | an annotation |
| `volatile` | 11d | **no, builtin or contextual** | a property of an access, not a form |
| `ptr` | 11d | **no** | the type is `*T` |
| `join` | 11e | **no, builtin** | a call with a result |
| `atomic`, `channel` | 11e | **no, builtin prefixes** | `atomic_add(a, 1)`, `channel_send(ch, v)` |
| `vec` | 11f | **no** | the type is `F32x4` and the literal is `F32x4(...)` |

**The cost of taking a word, exactly:**

1. **In the toolchain, about eight lines.** The Rust lexer needs a
   `TokenKind` variant, an arm in `TokenKind::name()`
   (`crates/nexum-lexer/src/lib.rs:65-123`) and an arm in the keyword match
   (`lib.rs:342-359`); the self-hosted lexer needs an arm in
   `keyword_kind()` (`examples/selfhost/lexer_lib.nx:37-70`). The editor's
   keyword list needs a fourth edit
   (`editors/vscode/syntaxes/nexum.tmLanguage.json:26`).
2. **In the parsers, one dispatch arm each** — the Rust
   `parse_stmt` (`parser lib.rs:339-346`) or `parse_unary`
   (`parser lib.rs:1000-1027`), and the self-hosted `parse_stmt`
   (`parser.nx:600-627`) or `parse_unary` (`parser.nx:217-227`).
3. **In the programs that already exist, a break.** A reserved word cannot
   be an identifier. Verified: `let unsafe = 1`, `let spawn = 2` and
   `let asm = 3` all parse today, and all three would stop parsing. The
   repository's own examples and fixtures have to be grepped and renamed.
   This is mechanical, but it is a *breaking change to the language*, and it
   is the only cost that is not local to the toolchain.
4. **In the documents,** a new numbered decision in
   `docs/language-reference/decisions.md` and the matching change to
   `docs/language-reference/grammar.md` §3 (`specs/AGENT-BRIEF.md:129-131`).

**Contextual words cost none of item 3** and none of item 1 — they are
identifiers in the grammar, and the parser checks their spelling in one
position. The only price is that the *meaning* of `packed` depends on
where it appears, which is a documentation burden, not a toolchain one.

**The words that are already spoken for** and cannot be reused:
`fn`, `type`, `let`, `mut`, `if`, `else`, `while`, `for`, `in`, `return`,
`true`, `false`, `and`, `or`, `not`, `import`. Note that `in` is already
taken, which is why the concurrency design does not use `in`/`out` for
assembly operands, and that `not` is taken, which is why the boolean
operations could not be spelled `!` without a new token.

### The alternatives to extending the grammar

There are four ways to add a feature without touching the grammar, and the
roadmap's five phases use all of them.

**1. A builtin call.** The feature becomes a name resolved by the semantic
pass and lowered by the code generator. The front-end cost is *zero*: the
parser produces an ordinary call node (`parser lib.rs:1103-1122`) and the
semantic pass decides what the name means (`sema lib.rs:1184`, `1316`,
`1502`, `1545`, `1596`). This is the right answer whenever the feature has
**operands and a result, and introduces no scope, no binding, and no literal
the parser must recognise**. In this programme it covers: `join`,
`channel_*`, `atomic_*`, `yield_cpu`, `to_f32` and the rest of the
conversions, `stride`, the vector intrinsics, `volatile_load` /
`volatile_store`, `arena_new` / `arena_alloc` / `arena_release`, `addr_of`
(if the `&` token is not taken), and — recommended — the whole of 11b's
inline assembly.

**2. A call to a type name.** The feature becomes a *type* whose values are
written by naming the type and listing the elements. This works because
`Ident "(" args ")"` is already a call (`parser lib.rs:1103-1122`) and
because records already use exactly this shape. The front-end cost is
*zero*; the semantic pass resolves the name to a type and checks the arity
and the element types. This is how **11f's vector literals** are built, and
it is the model the language already uses for every aggregate.

**3. A record with special fields.** A convention rather than a syntax: a
record type with a field named `stride` or `packed` and the *semantics*
attach to the field's name. The language has no attribute mechanism, so
this is invisible to the type system — the field is an ordinary field, and
only the code generator and the semantic pass know what the name means.
**This works only when the special meaning is a value, not a modifier.** A
`stride: Int` field is a value and the convention is honest. "`packed`"
is a *modifier* on the layout, and a modifier cannot be a field, because a
field is data and `packed` changes what the compiler does with the
*record*. So the roadmap's layout annotations are the one case where a
record-with-special-fields cannot substitute for syntax, and the cheap
substitute is a contextual word, not a field.

**4. A contextual word.** A word that is an ordinary identifier in the
grammar and is recognised by the parser in one position. Cost: no lexer
change, no reserved word, no source break, no change to the token stream.
The price is that the grammar document has to say "in this position, this
identifier means X", and the normaliser has to print it, which is a change
to the differential contract on both sides. **This is the right answer for
annotations** — `packed`, `align`, and (later) `volatile`.

**When each is better, summarised:**

| The feature… | Use | Because |
| ------------ | --- | ------- |
| has operands and a result, no scope, no binding | a **builtin** | zero front-end cost; the cost moves to sema and codegen, which is where it belongs |
| is a value of an aggregate type | a **type name + positional construction** | the language already has one aggregate syntax |
| is an annotation on a declaration | a **contextual word** | no lexer change, no source break |
| introduces a *scope* (`unsafe`) | a **keyword statement** | a scope needs a keyword and a body; there is no other way to say it |
| occupies a *control-flow position* (`spawn`) | a **keyword prefix** | the prefix-operator slot already exists and it costs one alternative |
| must be recognised as a *literal* by the parser | a **new literal form** | the most expensive thing in the language; avoid unless the value cannot be written any other way |

The last row is the one to be careful about. A vector literal *can* be
written as a call to a type name, so 11f does not need a literal form. A
numeric suffix *cannot* be written any other way (the suffix is part of the
literal's spelling), which is exactly why it is the one item in this
programme that is worth its cost — and why it should be a deliberate,
separately-scheduled decision rather than a side effect of the tower.

## Risks

* **The self-hosted parser is behind, and every grammar change lands on
  it.** It does not yet parse path assignments
  (`examples/selfhost/parser.nx:17`; the two known exceptions are recorded
  in `docs/self-hosting.md:70-74`). A syntax change made while it is behind
  is a syntax change made against a moving target. *Mitigation:* land the
  path-assignment gap first, or accept that the differential parser gate
  will be red for the duration of the change.

* **The token stream is a byte-exact contract, and the number scanner is
  the risky part.** The lexer gate compares kinds and text
  (`backend.rs:173-179`). A suffix rule that differs in one lexer by one
  character is a red gate, not a warning. *Mitigation:* the suffixes, the
  bases and the exponents must be one change, in both lexers, in one PR;
  and they should be scheduled together with 11d's `&` so the lexer is
  touched once.

* **Taking a reserved word breaks existing programs.** `unsafe`, `spawn`
  and `asm` all parse as identifiers today (verified). The repository's own
  examples and fixtures have to be found and renamed — a `grep` and a
  mechanical edit, but it is a *language* break, and it is the part of the
  cost that is not local. *Mitigation:* take the two words the features
  genuinely need (`unsafe`, `spawn`), and use contextual words for
  annotations.

* **The layout annotations change the normalised form, and the normalised
  form is the differential contract.** `type Header packed:` has to
  normalise identically in Rust (`normalize.rs:182-188`) and in Nexum
  (`parser.nx:753-756`). If the two disagree on the spelling of an
  annotation, the gate is red. *Mitigation:* decide the normalised
  spelling of every new node in the same change that adds it, and add a
  parse fixture for it under
  `crates/nexum-driver/tests/fixtures/parse/` — the gate only runs over
  those fixtures (`backend.rs:222-223`).

* **The `unsafe` model may be the wrong split.** If the orchestrator decides
  that *creating* a pointer must also be unsafe — which is the Rust model,
  and is defensible — then the region has to allow the creation *and* the
  dereference, and the pointer must escape the region, which turns the
  region into something that has to produce a value. That is the
  "expression form" alternative, and it costs a second form. *Mitigation:*
  decide this before the syntax is written, not after; the recommendation
  here is the split that needs only one form.

* **The assembly surface may be judged too low-level.** The constraint
  string is a GCC dialect; a reader who has not written inline assembly
  will find it opaque. *Mitigation:* the surface is a *builtin*, so the
  compiler can grow a friendlier wrapper as a **library function in Nexum**
  (once there is a module system with namespaces) without any grammar work
  at all. That is the strongest argument for the builtin: the ergonomics
  can be fixed later in the standard library, whereas a grammar form cannot
  be changed later without a migration.

* **The vector-type-as-constructor decision constrains 11f.** Once
  `F32x4(1.0, 2.0, 3.0, 4.0)` is the vector literal, a vector type cannot
  also be a record with the same name, and the semantics of "call a name
  that denotes a type" have to be settled (does `F32x4(1.0)` with the wrong
  arity give a type error or a name error?). *Mitigation:* settle the
  name-resolution rule once, generally: **a call to a name that denotes a
  type is a construction; a call to a name that denotes a function is a
  call; a name cannot be both.** Records already rely on this.

* **Coordination with the sibling research.** The numeric-tower research on
  the parallel branch `research/numeric-tower` already recommends the
  suffix spellings `42i8`, `42u32`, `1.0f32`, `1.0f64` and hex/octal/binary
  literals, and the allocator-design research on `research/allocator-design`
  covers the arena model. This document recommends *deferring* the suffixes
  to 11c.2, which is a disagreement that the orchestrator has to resolve
  explicitly; if the suffix decision is taken the other way, the cost
  table in this document says exactly what it buys.

## Open questions

1. **Do the numeric suffixes land in 11c or in 11c.2?** *Recommendation:*
   11c.2, bundled with hex/octal/binary and exponent notation, and
   scheduled in the same PR as 11d's `&` token. The tower itself needs
   neither, and the suffix scanner is the most expensive lexer change in
   the programme. If the orchestrator wants the suffixes in 11c proper,
   the cost is ~90 lines across the two lexers plus the AST change, and
   11d's `&` moves to meet it.

2. **Is `spawn` a keyword or a builtin?** *Recommendation:* a keyword, in
   the prefix-operator slot, with `spawn(f, args...)` as the variadic
   builtin fallback. The keyword is one word and one production; the
   builtin is zero words but an awkward call.

3. **Is inline assembly a builtin or a keyword statement in 11b?**
   *Recommendation:* a builtin (`asm(template, constraints, operands...)`),
   and a keyword statement only after 11d has pointers and the register
   vocabulary has settled. The builtin lowers to LLVM's `asm` almost
   directly, and it leaves the ergonomics to be fixed later in the
   standard library rather than in the grammar.

4. **Is the pointer type `*T` or a nominal `Ptr`?** *Recommendation:*
   `*T`. `Ptr` is free (it already parses) and is the fallback if the
   parser budget is exhausted; but a typed pointer is what makes the
   `unsafe` rule checkable by a human reading the code.

5. **Is address-of `&p` or `addr_of(p)`?** *Recommendation:* `&p`. The
   cost is one token in each lexer; `addr_of` is the free fallback.

6. **Does creating a pointer require an `unsafe` region, or only
   dereferencing it?** *Recommendation:* only dereferencing. It keeps the
   `unsafe` region a statement (one form, no value to thread out) and it
   matches the fact that the language already has a "place" concept
   (`mut` bindings, `mut` parameters, D001).

7. **Are the layout annotations contextual words or reserved words?**
   *Recommendation:* contextual words (`packed`, `align`). Zero lexer cost,
   zero source break, and the same mechanism will serve `volatile` and
   `repr`-style annotations later.

8. **Do arenas get a block form?** *Recommendation:* no, not in 11d. The
   builtins `arena_new` / `arena_alloc` / `arena_release` match the
   specified model (a single explicit release point, LIFO) and cost no
   grammar. A block form would need the keyword, the form, and a rule for
   releasing on early returns.

9. **Should the vector type be a named type with a constructor call, or a
   distinct literal?** *Recommendation:* a named type with a constructor
   call (`F32x4(1.0, 2.0, 3.0, 4.0)`). It reuses the record-construction
   rule and needs no grammar. This also settles the general rule that a
   call to a name denoting a type is a construction.

10. **Do we add the `spawn`/`unsafe` words to the editor's keyword list in
    the same change?** *Recommendation:* yes. The editor's list
    (`editors/vscode/syntaxes/nexum.tmLanguage.json:26`) has already
    drifted from the language (it does not list `import`), which is a
    sign that the list needs to be derived from the lexer's keyword table
    rather than maintained by hand. Deriving it is a small task and it
    removes a whole class of drift.

11. **Should the normaliser's spelling of each new node be decided in the
    same change that adds it?** *Recommendation:* yes, and it should be
    written down in the grammar document, because the normalised form is
    the contract between the two parsers and the one thing a reader of the
    grammar cannot derive from the grammar.

## Changes needed elsewhere

* `docs/language-reference/grammar.md` §3 (syntactic grammar) and §4
  (static semantics) — for every accepted form: the `*T` type, the `*p`
  and `&p` operators, the `unsafe` statement, the `spawn` prefix, and the
  vector type and its construction rule.
* `docs/language-reference/decisions.md` — a new numbered decision (D017
  onwards) for each of: the pointer model and the unsafe rule, the
  `spawn`/`join` model, the vector type and its construction rule, and the
  numeric tower's literal-typing rule.
* `docs/standard-library.md` — the new builtins: `to_f32` and the other
  conversions, `join`, `channel_*`, `atomic_*`, `yield_cpu`, `stride`,
  `dot` / `fma` / `hadd` / `abs4`, `volatile_load` / `volatile_store`,
  `arena_*`, and `asm`.
* `editors/vscode/syntaxes/nexum.tmLanguage.json` — the new keywords
  (`unsafe`, `spawn`), the new operators (`*` as a prefix, `&`), the new
  type names (`F32`, `F32x4`, `Int`, ...) and the numeric-suffix pattern.
* `crates/nexum-driver/tests/fixtures/parse/` — one fixture per new
  syntax form, since the differential parser gate only runs over that
  directory (`backend.rs:222-223`).
