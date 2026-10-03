# The Nexum grammar

Derived from the implementation, not from intent. Every rule below names
the file and function that enforces it where there is one, so a claim here
can be checked against the code. Where there is no single enforcing
function -- a runtime semantic with one implementation, for instance --
the rule is stated normatively and this document is the authority.

| Layer | Source |
| --- | --- |
| Tokens, literals, layout | `compiler/nx-lexer/src/lib.rs` |
| Statements, expressions, precedence | `compiler/nx-parser/src/lib.rs` |
| Types, methods, ambient builtins | `compiler/nx-types/src/lib.rs` |
| Operator result types | `fn arith_result`, `compiler/nx-types/src/lib.rs` |
| Runtime meaning (arithmetic, strings, limits) | this document: sections 3.1.1, 4.2 |

Runtime meaning is owned by this specification, not by a backend. There
is one execution model (ahead-of-time compilation to a native
executable), so a rule stated here has exactly one implementation to
keep honest; `compiler/nx-codegen/src/runtime.ll` is an implementation of
this document, not an authority over it. The one exception is the
operator result matrix in `arith_result`, which is the checker-side owner
of the type each operator produces, and which section 3.1.1 states in
prose as well.

Nexum is indentation-sensitive like Python and statically checked with no
type annotations. `Int` is 64-bit signed, `Float` is an IEEE double.

---

## 1. Lexical grammar

### 1.1 Keywords

28 reserved words, matched whole-word (`selfish` and `mutable` are ordinary
identifiers):

```text
if elif else while for in fn return break continue
import from as
true false None del assert type impl self mut own
and or not
```

`None` is capitalised because it is a *value*, not a control word.

`self`, `mut` and `own` are full keywords so a receiver can be spelled.
`mut` and `own` are reserved ahead of Stage 6, so binding modifiers will
need no lexer change.

### 1.2 Identifiers

```text
identifier ::= ident_start ident_continue*
ident_start ::= [A-Za-z_]
ident_continue ::= [A-Za-z0-9_]
```

Case-sensitive. Not a keyword. `print` is *not* a keyword — it is an
ordinary identifier that the parser special-cases when it is immediately
followed by `(` (`parse_simple_stmt`), so `x = print` is legal.

### 1.3 Integer literals

```text
integer ::= decimal_int | hex_int | oct_int | bin_int

decimal_int ::= digit ( digit | "_" )*
hex_int     ::= ("0x" | "0X") hex_digit ( hex_digit | "_" )*
oct_int     ::= ("0o" | "0O") oct_digit ( oct_digit | "_" )*
bin_int     ::= ("0b" | "0B") ("0" | "1" | "_")*

digit     ::= [0-9]
hex_digit ::= [0-9a-fA-F]
oct_digit ::= [0-7]
```

Underscores are digit separators anywhere in the run and are stripped, so
`1_000_000` is `1000000`. Radix literals are decoded to decimal at lexing
time, so every consumer downstream sees a plain digit string. Overflow is
not detected here: the digits pass through and the parser reports the
range error when it fails to parse them as `i64`.

### 1.4 Float literals

```text
float ::= decimal_int "." decimal_int exponent?
        | decimal_int exponent

exponent ::= ("e" | "E") ("+" | "-")? digit ( digit | "_" )*
```

A `.` only begins a fraction when a digit follows it. That is what keeps
`1..n` a range rather than a malformed float, and `1.foo` a field access
rather than a float followed by a name. An `e` only begins an exponent
when a digit (optionally after a sign) follows, so a trailing identifier
starting with `e` is not swallowed.

There are no other float spellings: no leading-dot floats (`.5` is an
error), no `inf`/`nan` literals, no thousands separators.

### 1.5 String literals

```text
string ::= '"' char* '"'
char    ::= any character except '"' and '\'
         | escape

escape ::= "\n" | "\t" | "\r" | "\"" | "\\"
```

Only those five escapes exist. Any other backslash sequence is a lex
error. There are no raw strings, no triple-quoted strings, no string
interpolation, and no multi-line string literals — a string ends at the
first unescaped `"` on its line.

### 1.6 Comments

```text
comment ::= "#" any-character-except-newline
```

`#` runs to end of line. A `#` inside a string is a literal character.

Note that `//` is **floor division, not a comment**. It is easy to reach
for `//` out of C or Rust habit; `#` is the NX comment marker.

### 1.7 Layout

Indentation is significant and **spaces only**. A tab in the indentation
prefix is a lex error, not a tab stop — mixing them silently is the class
of bug this rule exists to prevent.

```text
block_open  ::= ":" newline indent_token
block_close ::= dedent_token
```

The lexer keeps a stack of indent widths starting at 0. A line indented
wider than the top pushes `Indent`; narrower pops one `Dedent` per level.
If the resulting width matches no previous level, that is an "inconsistent
indentation" error. Blank and comment-only lines are skipped entirely and
produce no `Indent`/`Dedent`, so a trailing blank line before a dedent is
harmless.

A block may not be empty: `if x:` with nothing indented under it is a
parse error.

### 1.8 Operators and punctuation

```text
( ) [ ] { } , : . .. ;

+  -=  *  /=  //  //=  %  %=   **  **=
&  &=  |  |=  ^  ^=  ~  <<  <<=  >>  >>=
=  ==  !=  <  <=  >  >=  !
```

`//` is matched before `/` and `**` before `*` by the lexer, which emits
distinct tokens, so the parser never has to back up.

---

## 2. Statement grammar

```text
program ::= statement*

statement ::= type_decl
            | impl_block
            | fn_decl
            | if_stmt
            | while_stmt
            | for_stmt

            | import_stmt
            | from_import_stmt
            | return_stmt
            | break_stmt
            | continue_stmt
            | del_stmt
            | assert_stmt
            | print_stmt
            | assign_stmt
            | compound_assign_stmt
            | expr_stmt
```

### 2.1 Declarations

```text
type_decl ::= "type" ident ":" newline indent field+ dedent
field     ::= ident ( ":" ident )?
```

The field type is optional. `x` alone declares an unresolved field, which
is what lets a field's type be pinned later by how it is actually used
rather than written out in advance. A field name may not repeat.

`type` and `impl` are module-level only. A declaration inside a function
body is a type error, not a parse error, because it would be a second and
incompatible layout for the same name — exactly what a static type system
cannot represent.

### 2.2 Functions and methods

```text
fn_decl   ::= "fn" ident "(" params? ")" ":" block
impl_block ::= "impl" ident ":" newline indent method+ dedent
method    ::= "fn" ident "(" receiver? params? ")" ":" block

receiver ::= "self"          -- read-only, the default shape
           | "mut" "self"    -- result is written back into the receiver
           | "own" "self"    -- reserved for Stage 6

params ::= ident ( "," ident )*
```

`self` may appear **only** as the first parameter, and a receiver consumes
that first position — `fn m(self, k)` takes one argument, not two. An
`impl` block may contain only `fn` definitions; anything else is an error
rather than a silently ignored line. A duplicate method name in one `impl`
is a parse error.

Semantics of the receiver kinds, enforced by the checker:

- **`self`** — the method gets a copy. Writing through it (`self.x = 1`) is
  a **compile error**, because under value semantics the write would be
  discarded. Saying so beats letting it look meaningful.
- **`mut self`** — the method may write through `self`, and **must return
  the record**: its result *is* the new receiver, so `p.moved(1, 2)` means
  `p = moved(p, 1, 2)`. Returning anything else is a type error.
- **`own self`** — parses today, means nothing yet.

A `mut self` call on a receiver with no storage (a call result, a literal)
still evaluates; there is simply nowhere to write. That is what makes
`q.moved(1, 1).moved(2, 2)` read as one expression.

An `impl` must live in the same module as its `type`. An orphan impl is
refused.

### 2.3 Control flow

```text
if_stmt       ::= "if" expr ":" block elif* else?
elif          ::= "elif" expr ":" block
else          ::= "else" ":" block

while_stmt    ::= "while" expr ":" block
for_stmt      ::= "for" ident "in" expr ":" block


return_stmt   ::= "return" expr_list?
break_stmt    ::= "break"
continue_stmt ::= "continue"
```

`elif` is a keyword, not `else if`.

`for x in expr:` — when the iterable is written as a range literal it
stays a range in the AST and the backend emits a counted loop instead of
materialising a list. Anywhere else a range is an ordinary list-valued
expression. Only one loop variable: there is no `for a, b in pairs`.

A block may not be empty.

`parallel:` was removed. It ran conflict-free tasks on a thread pool while
serialising conflicting ones, and it promised output in task order whatever the
scheduler did. That promise was enforced by a static approximation of
race-freedom, and the approximation had holes: a name bound inside a `parallel:`
block is a module global in the compiled program, but the dependency analysis
did not know that, so dependent tasks were emitted into one concurrent batch.
A program could then print the wrong answer whenever it lost the race -- and it
usually passed its own test, because the race is usually won. Correctness was
worth more than the throughput, so the feature is gone rather than the claim.
There is no `spawn`, no thread pool and no atomic in the language. Real
concurrency is a Stage 6 question, once ownership exists to make sharing
explicit rather than inferred.

Neither loop form counts iterations against a limit and call depth is not
tracked, so there is no iteration budget and no recursion limit to
configure -- rule R4 in section 3.1.1.

### 2.4 Modules

```text
import_stmt   ::= "import" ident ( "as" ident )?
from_import_stmt ::= "from" ident "import" import_name ( "," import_name )*
import_name   ::= ident ( "as" ident )?
```

`import m` binds the module name; `m.f(...)` calls into it and `m.T(...)`
constructs a type it declares. `from m import f, T as U` binds individual
names. An alias for a type is canonicalised, so a value built through `U`
compares equal to one built through `T`.

Module resolution looks in the entry file's directory and then each
`NX_PATH` entry, for `<name>.nx`.

### 2.5 Assignment

```text
assign_stmt          ::= target_list "=" expr_list
target_list          ::= target ( "," target )*
target               ::= ident
                      | postfix "[" expr "]"
                      | postfix "." ident

compound_assign_stmt ::= target binop "=" expr
```

Compound forms: `+= -= *= /= //= %= **= &= |= ^= <<= >>=`. All twelve
binary operators have one. `<<=` and `>>=` are there; `%=` follows
Python's sign convention (`-7 % 3 == 2`).

A target is a name, an element, or a field — and postfix suffixes chain
greedily, so `a[i][j] = v` and `a[i].x = v` both work. A call result is
not a target.

Several targets against several values assign positionally. Several
targets against **one** value destructure a tuple or multiple return:
`a, b = f()`. Several targets against one non-tuple value is a type error.

Assignment is a statement. There is no walrus operator and no assignment
expression.

### 2.6 Other statements

```text
print_stmt ::= "print" "(" expr ( "," expr )* ")"
del_stmt   ::= "del" target ( "," target )*
assert_stmt ::= "assert" expr ( "," expr )?
expr_stmt  ::= expr
```

`del` on a name unbinds it; on an element or field it removes that one
entry. On a record it **blanks the field to `None`** rather than removing
it, because a record's arity is fixed. Strings are immutable, so `del s[0]`
is a type error.

`assert` takes an optional message expression.

Any bare expression is a legal statement; the value is discarded.

---

## 3. Expression grammar

### 3.1 Precedence, tightest binding first

| Level | Operators | Associativity | Notes |
| --- | --- | --- | --- |
| 1 | `f(x)`, `a[i]`, `a[i:j:k]`, `a.b` | left | postfix chain |
| 2 | `**` | **right** | base is postfix, exponent may be signed || 3 | `+x`, `-x`, `~x` | right | `+x` is a no-op but legal |
| 4 | `*` `/` `//` `%` | left | |
| 5 | `+` `-` | left | |
| 6 | `<<` `>>` | left | |
| 7 | `&` | left | |
| 8 | `^` | left | |
| 9 | `\|` | left | looser than `&` and `^` |
| 10 | `==` `!=` `<` `<=` `>` `>=` `in` `not in` | left | **see below** |
| 11 | `not` | right | binds *looser* than comparison |
| 12 | `and` | left | |
| 13 | `or` | left | |
| 14 | `a if c else b` | right | ternary |
| 15 | `a..b` | n/a | range |
| — | `=` and the compound forms | n/a | statement level, not an operator |

Three of these are deliberate choices rather than accidents:

**`**` binds tighter than prefix unary on its left.** So `-2 ** 2` is
`-(2 ** 2)` = `-4`. The base is a postfix expression and the exponent is a
full unary expression, which is why the *parse* of `2 ** -1` is legal.
Getting this the other way round sends unary and power into a parse cycle.

Note the difference between parsing and meaning: `2 ** -1` parses and type
checks, but at runtime an `Int` base with a negative exponent is an error
("negative exponent on Int; use a Float exponent for a fractional
result"). `2.0 ** -1` is `0.5`. The grammar admits the exponent; the
runtime decides whether the arithmetic exists.

**`not` binds looser than comparison.** So `not a in b` is `not (a in b)`
and `not a == b` is `not (a == b)`. At the unary level it would instead
read as `(not a) == b`, which is almost never what was meant. This is also
why `not in` can exist as a single operator rather than `not (a in b)`.

**Comparisons chain left, they do not compare pairwise.** `a < b < c`
parses as `(a < b) < c`, not Python's chained form. The left operand of
the second comparison is a Bool, so `1 < 2 < 3` is rejected statically
with "cannot order Bool and Int" rather than silently doing something
surprising — but it is worth knowing that the parse is left-associative.

#### 3.1.1 Fixed runtime rules

Five rules that were previously ambiguous and are now decided. Each one
is stated so that a single test can check it, and each is owned by this
document: an implementation that disagrees with one of them is a bug, not
an alternative reading of the language. They live here because this is
where the operator rules live; the string rule is also summarised in
section 4.2.

**R1. Integer overflow traps. It never wraps.**
`+`, `-`, `*`, `//` and `%` on two `Int`s compute exactly in `i64`. If
the exact result lies outside `-9223372036854775808 .. 9223372036854775807`,
the program stops with the runtime error "integer overflow". There is no
wrapping, no saturating, and no `Int` value that stands for "too big":
`9223372036854775807 + 1` is an error, not `-9223372036854775808`. `**`
is the one arithmetic operator that does not follow this rule, and R2
says why. Division or modulus by zero remains a runtime error, unchanged.

**R2. `Int ** Int` saturates; a negative exponent is rejected.**
- A negative exponent on an `Int` base is a runtime error: "negative
  exponent on Int; use a Float exponent for a fractional result". So
  `2 ** -1` is an error while `2.0 ** -1` is `0.5`. This is the rule
  referred to above: the grammar admits the exponent, the runtime
  decides whether the arithmetic exists.
- If the exact result is not representable, the answer is the extreme
  `Int` in the direction of the base: `9223372036854775807` for a
  positive base, `-9223372036854775808` for a negative one. It does not
  wrap.
- The two bases whose power is always exactly representable are answered
  exactly instead of being clamped: a base of `0` gives `0` for any
  positive exponent, and a base of `-1` gives `1` or `-1` by the parity of
  the exponent. `0 ** 0` is `1`.
- Because saturation is defined here, `**` cannot trap on magnitude. A
  program that needs to know whether a power overflowed has to compute the
  bound itself; there is no builtin that reports it.

**R3. A string is a sequence of characters, not of bytes.**
Every operation that counts, indexes, slices or walks a `Str` uses the
same unit, and no operation can split a character in half:
- `len(s)` is a count of characters. A five-character "hello" whose `e`
  carries an acute accent has length 5, whatever the source encoding of
  the literal was.
- `s[i]` is the one-character string at character index `i`, negative
  indices counting from the end exactly as for a list (section 3.3).
- A slice takes characters: `s[a:b]` starts at character `a`, and the
  step counts characters.
- `for c in s` yields one-character strings, one per character.
- `x in s` tests membership in the character sequence. Matching never
  starts or ends inside a character, so no substring test can match a
  fragment of a multi-byte character.

A character-counted string and a byte-counted string are different
languages, which is why this is a rule and not an implementation detail.

**R4. There is no loop-iteration limit and no recursion-depth limit.**
Neither `while` nor `for` counts iterations against any constant, and
call depth is not tracked. No program is refused for looping or
recursing deeply. What eventually stops a runaway recursion is the
native stack, which is a property of the machine, not a rule of the
language: there is no diagnostic to catch and no limit to configure.

**R5. Execution is single-threaded and in program order.**
There is no concurrency in the language, so nothing can be reordered or
observed out of order. This was previously the `parallel:` ordering rule; it
survives the removal of `parallel:` because the guarantee is still worth
stating, and a single-threaded model is the one thing a compiler can promise
without a proof obligation. It is also what makes R4's "the native stack is
what stops runaway recursion" an honest answer rather than a gap.

### 3.2 Primary expressions

```text
primary ::= integer
          | float
          | string
          | "true" | "false" | "None"
          | ident
          | "self"                     -- only inside a method body
          | "(" expr ")"
          | list_literal
          | dict_literal
          | comprehension

list_literal ::= "[" ( expr ( "," expr )* ","? )? "]"
dict_literal ::= "{" ( dict_entry ( "," dict_entry )* ","? )? "}"
dict_entry   ::= expr ":" expr

comprehension ::= "[" expr "for" ident "in" iterable ( "if" expr )? "]"
iterable       ::= or_expr | or_expr ".." or_expr
```

A comprehension is a list literal whose first element is followed by
`for` instead of a comma. It has **at most one** `if`, which is all that is
needed to be useful. The iterable is parsed at the `or` level specifically
so a following `if` binds to the comprehension rather than becoming a
ternary on the iterable.

`None` is the unit value. It is distinct from an empty list and from a
missing key, which is what makes it usable for optionals.

### 3.3 Postfix expressions

```text
postfix ::= primary ( "(" args? ")" | "[" expr "]" | "[" slice? "]" | "." ident )*

args  ::= expr ( "," expr )* ","?
slice ::= [expr] ":" [expr] [ ":" [expr] ]
```

Call arguments and dict entries may span lines. The `?` forms in `slice`
are all optional, so `a[:]`, `a[1:]`, `a[:3]`, `a[::2]` and `a[1:8:2]`
all parse through the one form.

Slice bounds are ordinary expressions and may be negative: `xs[-1:]` on
`[1, 2, 3]` is `[3]`. Bounds differ from the step — a step must be
positive, and a zero or negative one is a **runtime** error ("slice step
must be positive, found 0"). The checker does not catch it, because it
cannot: the step is usually only known at runtime.

An `Attr` on a record is a field read (`p.x`). An `Attr` that is the callee
of a call is method resolution — see §4.3.

### 3.4 Ranges

```text
range ::= if_expr ".." if_expr
```

A range is an ordinary expression producing a list of Ints, not
`for`-only syntax. That is what makes it composable:
`xs[i + 1 .. n + 1]`, `[i for i in 0..5]`, `len(0..n)`. Bounds must be
Ints.

Ranges are half-open and ascending: `0..5` is `[0, 1, 2, 3, 4]`.

---

## 4. Types, methods and the ambient surface

### 4.1 Types

NX has no annotations and no user-declared type expressions at use sites.
Types are *inferred*, and a `type` declaration creates a record layout.

```text
type Point:
    x: Float
    y: Float
```

`Point(1.0, 2.0)` constructs, with fields in declaration order and exact
arity. A partial constructor would need a notion of an unset field that
the value model does not have, so arity is exact.

Names cannot collide across kinds: a variable, a function, a parameter, a
loop variable, an import alias and a type may not share a name.

### 4.2 Value semantics

Containers **copy on bind**. Neither assignment nor argument passing
aliases, so mutating through one binding never affects another:

```
q = p
q.x = 0.0      # p is unchanged
```

This is why `self` is a copy and why writing through a read-only `self` is
refused rather than permitted and ignored.

Strings are shared but never mutated in place. A `Str` is a sequence of
characters rather than of bytes, and `len`, indexing, slicing, iteration
and `in` all count the same characters -- rule R3 in section 3.1.1.

### 4.3 Ambient builtins

Two, and they need no import:

```text
len(x)          -- list, string or dict length  -> Int
push(xs, v)     -- append to a list            -> None
```

Both also have method-call sugar, resolved *after* methods so a type may
override them:

```text
xs.push(3)      -- exactly len-free sugar for push(xs, 3)
xs.len()        -- sugar for len(xs)
```

`push` requires a list **variable** as its first argument. Pushing into a
temporary would drop the result, so `push([1], 2)` is a type error. The
first `push` into an empty list pins the element type, and a later
mismatch is an error.

The minimal stdlib — string, conversion, and the fuller list and dict
surfaces — is Stage 3 work in progress, not yet present. Do not write
against it.

### 4.4 Method resolution

For `base.attr(...)`, in order:

1. **module** — `m.f(...)`, a function of module `m`
2. **associated function** — `T.f(...)`, an `fn` with no receiver
3. **impl method** — `v.f(...)`, where `v` is a statically known record
4. **builtin sugar** — `xs.push(1)`

An unresolved base takes sugar, which is what keeps `x.push(1)` working on
dynamic values. Calling a method on a receiver whose type is not statically
known is a **type error** in Stage 3; dynamic dispatch arrives in Stage 4.

An associated function takes no receiver, so a value of the type is as good
a base as the type itself — `p.zero()` and `P.zero()` both work.

---

## 5. Static rules summary

Errors the checker reports, collected so the surface is legible:

| Rule | Kind |
| --- | --- |
| Undefined variable, undefined function | type error |
| Calling a function with the wrong arity | type error |
| Arithmetically incompatible operands | type error |
| Assigning a value of the wrong type to a field | type error |
| Indexing with a non-Int | type error |
| Deleting from a string, or from a non-container | type error |
| `type` or `impl` inside a function | type error |
| `impl` for a type from another module | type error |
| `mut self` method not returning the record | type error |
| Writing through a read-only `self` | type error |
| Calling a `mut self` method through a read-only `self` | type error |
| Several targets against one non-tuple value | type error |
| A name shared between a type and a variable/function/param | type error |
| `break`/`continue` outside a loop, `return` outside a function | type error |

| Empty block, inconsistent indentation, tab indentation | parse error |
| Duplicate field, duplicate method, duplicate field name in a method | parse error |
| String escape other than `\n \t \r \" \\` | lex error |
| Negative exponent on an `Int` base; zero or negative slice step | **runtime** error |
| `Int` overflow in `+`, `-`, `*`, `//`, `%`; `//` or `%` by zero | **runtime** error (section 3.1.1, R1) |

---

## 6. A complete program

Everything in one file, exercising every statement form.

```
# comments run to end of line; // is floor division, not a comment

type Point:
    x: Float
    y: Float

type Counter:
    n: Int

impl Point:
    fn area(self):
        return self.x * self.y          # read-only self: a copy

    fn moved(mut self, dx, dy):
        self.x = self.x + dx            # mut self: may write
        self.y = self.y + dy
        return self                     # ... and must return the record

    fn origin():
        return Point(0.0, 0.0)          # no receiver: associated function

impl Counter:
    fn zero():
        return Counter(0)

    fn bumped(mut self):
        self.n = self.n + 1
        return self

fn classify(n):
    if n < 0:
        return "negative"
    elif n == 0:
        return "zero"
    else:
        return "positive"

fn main():
    p = Point(3.0, 4.0)
    print(p.area())                     # 12.0

    p.moved(1.0, -2.0)                  # write-back: p is now Point(4, 2)
    print(p)
    print(Point.origin())

    q = Point(0.0, 0.0)
    print(q.moved(1.0, 1.0).moved(1.0, 1.0).area())   # chains as one expression

    # arithmetic, in full
    print(7 // 3, 7 % 3, -7 % 3, 2 ** 10, 1 << 4, 255 & 15, 240 | 15, 255 ^ 15, ~0)
    print(-2 ** 2)                      # -(2 ** 2) = -4
    print(2.0 ** -1)                    # 0.5 -- an Int base would fail

    # literals
    print(1_000_000, 0xFF, 0o17, 0b1010, 1e3, 2.5e-3)
    print(not 1 == 2, 3 in [1, 2, 3], "a" not in ["b"])

    # containers
    xs = [1, 2, 3,]
    xs.push(4)
    print(xs, xs.len(), xs[1], xs[1:3], xs[::2], xs[-1:])
    d = {"a": 1, "b": 2,}
    print(d["a"], d)
    print([i * i for i in 0..5])
    print([i for i in 0..10 if i % 2 == 0])
    print(0 if p.x > 3 else 1)          # ternary

    # loops
    t = 0
    for i in 0..5:
        t += i
    while t > 0:
        t -= 3

    c = Counter.zero()
    c.bumped()
    print(c.n, classify(-1), classify(0), classify(5))

    assert t == -2, "unexpected"

    del xs
    print(t)

main()
```

Expected output:

```text
12
Point(4, 2)
Point(0, 0)
4
2 1 2 1024 16 15 255 240 -1
-4
0.5
1000000 255 15 10 1000 0.0025
true true true
[1, 2, 3, 4] 4 2 [2, 3] [1, 3] [4]
1 {a: 1, b: 2}
[0, 1, 4, 9, 16]
[0, 2, 4, 6, 8]
0
1 negative zero positive
-2
```

The block above is the published expectation, and with no second
execution path left to diff it against, the only machine check behind it
is `tools/verify.ps1`: it builds every example natively twice, once by
default and once with `NX_NOUNBOX=1`, and requires the two runs to be
byte-identical, which proves the unboxing pass is a representation change
and not a language change. Nothing compares a run against this block, so
editing the block is a claim about the language that has to be re-run by
hand to confirm.

Two notes on the example:

- Every method call resolves statically. `p.scaled_here(2.0)` would be a
  type error ("type 'Point' has no method 'scaled_here'") rather than a
  runtime lookup failure — there is no fallback to "call something and
  hope" in Stage 3.
- `xs[-1:]` uses a negative lower *bound* in a slice, which is fine. A
  negative *step* is not — see §3.3.

---

## 7. Not in the grammar yet

Named explicitly so nothing here reads as a promise:

| Feature | Stage |
| --- | --- |
| Minimal stdlib — string, conversion, list and dict functions | Stage 3, in progress |
| Dynamic dispatch on a receiver of unknown type | Stage 4 |
| Capabilities (traits), `dyn`, generic parameters | Stage 4, 5 |
| Fully inferred borrowing and lifetimes; `own` binding ownership | Stage 6 |
| Full stdlib in NX | Stage 7 |
| `match`, classes, inheritance, closures, generators, modules-as-values, exceptions, `while/else`, `try`, decorators, string multi-line literals | not planned |
