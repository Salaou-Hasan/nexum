# How the compiler comes to be written in Nexum

**Question.** Phase 11j of the native roadmap says "the toolchain self-hosts:
the compiler is written in Nexum" (`docs/native-roadmap.md:78`). What has
already been proven about that, what is still missing from the language for it
to happen, in what order the work should be done so that every step is
checkable, and how large is it really?

**Summary.** The recommendation is to replace the Rust compiler **stage by
stage, each stage behind a differential gate against the Rust stage it
replaces**, with the Rust compiler staying the reference implementation until
the last stage; the "self-hosted mode" in the driver is the *final* step of
that sequence, not an alternative to it. The good news is that the language is
**already expressive enough for the whole compiler** - the missing "maps,
sets, a string builder, an interning table, a sort" are not language
features, they are library code that can be written today over `List` and
records - and the bad news is that the roadmap has four gaps this plan needs
and does not own: a **nameable `List` type** (so a record can hold a map), an
**append-mode file write** (so the code generator never has to build a
gigantic string), an **exact float representation** (so the IR comparison can
be byte-exact), and the **performance** of 11b-11f (without which the
bootstrap is quadratic). The remaining work is roughly **7,000-9,000 lines of
Nexum** - a little more than the 7,752 lines of Rust it replaces - so the
right first step is therefore not the code generator but a **500-line
collection library and the sema's data model**, behind a typed-HIR gate.

## Recommendation

1. **Replace the Rust compiler stage by stage; do not add a "self-hosted
   mode" to the driver first.** The existing gates *are* the piece-by-piece
   replacement, done safely: the Rust implementation stays in the tree as the
   reference, the self-hosted implementation is written beside it, and the two
   are compared on every fixture. A `nx --self-hosted` flag is the *last*
   stage of that sequence (11j.5), because until the sema, the MIR and the
   code generator exist in Nexum there is nothing for the flag to run.
   *Reason:* the differential gate is the only mechanism that has ever
   proven anything about the self-hosted code, and it is already in the test
   suite (`crates/nexum-driver/tests/backend.rs:142` and `:196`).

2. **Give every stage one textual contract, in the style of
   `nexum-parser/src/normalize.rs`.** The parser gate works because both
   parsers can print the same normalised tree. The sema needs the same
   thing (a normalised typed HIR), the MIR needs the same thing (a
   normalised MIR), and the code generator needs the same thing (the program
   half of the LLVM IR, with the runtime slice excluded).
   *Reason:* a byte-for-byte comparison of a Rust data structure against a
   Nexum data structure is not possible; a normalised text form is the only
   thing that is, and it has already been shown to work.

3. **Stage 11j.1 - start with the data model, not with the sema.** Write
   `examples/selfhost/nxlib/`: the **fat node** (one uniform record for the
   whole compiler), the **arena** (a `List` of fat nodes plus a `List` of
   `Int` as the side table), the **linear map** (a `List` of
   `Entry { key: Str, val: Int }`), and the **text helpers**
   (`int_to_str`, `str_to_int`, byte-wise string compare). This is
   500-700 lines of Nexum, it needs **no language change at all**, and it
   settles the two questions every later stage depends on: how a recursive
   type is represented (arena + indices, per D009) and how a lookup table is
   represented (one insertion-ordered `List`).
   *Reason:* the sema is the first stage that *needs* a map, and the map is
   the first thing that needs a decision. Deciding it in isolation is much
   cheaper than discovering it inside 2,000 lines of type checker.

4. **The sema gate is two comparisons, not one.** `selfhosted_sema_diagnostics_match`
   (the sequence of severity, code and span must be identical) and
   `selfhosted_sema_hir_matches` (a new Rust-side `nexum_sema::normalize`
   dump). The HIR dump alone would not catch a missing diagnostic; the
   diagnostics alone would not catch a wrong type.
   *Reason:* the parser gate compares one artefact; the sema produces two
   kinds of observable, and the diagnostic one is the one users see.

5. **The MIR gate compares MIR, not IR.** A new Rust-side `nexum_mir::dump`
   prints the blocks, the instructions, the terminators and the value types;
   the self-hosted lowering prints the same. Comparing the IR text here
   would test the code generator before the code generator exists.
   *Reason:* one artefact per stage keeps each gate's failure message
   unambiguous.

6. **The code generator gate compares the program half of the IR, and
   handles floats explicitly.** `nexum_codegen::emit` prepends the runtime
   slice (`crates/nexum-codegen/src/lib.rs:119`), which is 679 lines of Rust
   that emit the same text every time and would drown the comparison. The
   gate must compare only the program part, and it must compare float
   constants in a form both sides can produce exactly (see feature **F3**
   below; the Rust side formats floats with Rust's `Display`, which is a
   shortest-round-trip algorithm that cannot be reproduced in Nexum without
   writing Ryu).
   *Reason:* a gate that compares 95% identical text and fails on the 5% that
   matters is worse than no gate.

7. **The bootstrap is a three-stage fixpoint, and its first step is nearly
   free.** Stage 0: the Rust compiler builds the self-hosted compiler
   (`nxc0`). Stage 1: `nxc0` compiles the self-hosted compiler's own source
   (`nxc1`); the gate is that `nxc1` and `nxc0` produce identical output on
   the whole corpus. Stage 2: `nxc1` compiles the source again (`nxc2`);
   the gate is `nxc2` and `nxc1` are identical. **The first bootstrap step
   is a rehearsal that reuses the two gates that already exist:** extend the
   lexer and parser corpus to include the self-hosted compiler's own source
   (`examples/selfhost/*.nx`, 1,135 lines - a hundred times the current
   fixtures) and require the self-hosted lexer and parser to agree with the
   Rust ones on it. That rehearsal is the first real test of whether the
   language can host the compiler, and it costs a corpus change.
   *Reason:* the classic bootstrap failure is a compiler that cannot compile
   its own source; the cheapest way to find that out is to point the
   existing gates at the compiler's own source.

8. **The self-hosted compiler's own source must be fully annotated and must
   obey the arena discipline.** Two hazards are silent failures today: the
   type-inference fixpoint is capped at 65 iterations
   (`crates/nexum-sema/src/lib.rs:323`) and anything still unresolved is
   silently defaulted to `Int`/`Unit`
   (`crates/nexum-sema/src/lib.rs:408-417`); and a list's element type is
   fixed by its first `list_push` and can only be refined through a plain
   named binding (`crates/nexum-sema/src/lib.rs:1403-1407`).
   *Reason:* a 10,000-line program with a deep call graph is exactly the
   shape that trips both.

9. **Four features are needed by this plan and are not in the roadmap**
   (**F1** a nameable `List` type; **F2** an append-mode file write;
   **F3** an exact float representation for the IR gate; **F4** the
   performance of 11b-11f). They are listed in full in *The design* below.
   The standard library already names a `collections` module
   (`docs/standard-library.md:51`) but no phase owns it, and
   `docs/self-hosting.md:41` lists "Hash maps in std" as the prerequisite of
   stage 10d without saying when it lands.
   *Reason:* phases 11b-11f can now be checked against a real requirement
   rather than a guess, and the four items above are the ones that the
   requirement exposes.

10. **Do not re-plan the phases; re-scope 11j.** 11j is currently one line
    in the phase table. It is really six stages with four new gates, and it
    depends on features that no phase currently produces. The orchestrator
    should decide whether 11j stays a single phase with an internal plan
    (this document) or becomes 11j.1-11j.6 in the table, and should place
    **F1** and **F2** before it.

## What the code looks like today

### What is already proven

The front end of the compiler exists twice: once in Rust, once in Nexum, and
the two are compared on every test run.

* **The lexer.** `examples/selfhost/lexer_lib.nx` (315 lines) tokenises a
  file into a `List` of `Token` records
  (`examples/selfhost/lexer_lib.nx:11-14`) and
  `examples/selfhost/lexer.nx` drives it over a file. The gate
  `selfhosted_lexer_matches_rust_lexer`
  (`crates/nexum-driver/tests/backend.rs:142-194`) compiles the self-hosted
  lexer, runs it over every example program, and compares the token stream
  with `nexum_lexer::lex` token by token.
* **The parser.** `examples/selfhost/parser.nx` (801 lines) parses into a
  **flat AST arena**: a `List` of `Node` records whose children are integer
  indices, plus a side table for the variable-length child lists
  (`examples/selfhost/parser.nx:9-17`, `:21-26`). The gate
  `selfhosted_parser_matches_rust_parser`
  (`crates/nexum-driver/tests/backend.rs:196-249`) compares it with
  `nexum_parser::normalize::normalize` over the parse fixtures.
* **Both gates pass.** Verified in this worktree:

  ```
  $ cargo test -p nexum-driver --test backend selfhosted
  running 2 tests
  test selfhosted_lexer_matches_rust_lexer ... ok
  test selfhosted_parser_matches_rust_parser ... ok
  test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 6 filtered out
  ```

What this proves is exactly what it says: **the language can express a real
compiler front end** - a byte-oriented scanner with an indent stack, a
recursive-descent parser with precedence climbing, a tagged-union AST stored
as an arena, a normalised text form, and file I/O.

What it does **not** prove, and the estimate below depends on noticing this:

* The lexer gate covers **9 example programs** and the parser gate covers
  **35 parse fixtures** - `nx_files` is not recursive
  (`crates/nexum-driver/tests/common/mod.rs:89-97`), so
  `examples/selfhost/` is not in the corpus at all. The self-hosted source
  is 1,135 lines; the largest fixture is 1,241 bytes.
* The self-hosted parser does **not** parse path assignments
  (`a.b[i] = v`, `examples/selfhost/parser.nx:16-17`), so two of the nine
  examples are outside its reach. `docs/self-hosting.md:71-75` says the two
  parsers "agree on every example program except the two that use path
  assignments" - that is a statement about the two implementations, not
  about the gate, which never runs on the examples. **The gate is narrower
  than the documentation implies**, and the programme should not treat the
  documentation as the evidence.
* The corpus is small enough that the front end has never been exercised on
  a program the size of the compiler it is supposed to host. That is the
  gap the bootstrap rehearsal (stage 11j.6) closes.

### The language as it stands

The self-hosted code is written in a deliberately narrow dialect, and the
narrowness is the specification of what the rest of the compiler will need.

| Feature | Status | Evidence |
| ------- | ------ | -------- |
| Records, arrays, `while`/`for`/`if`, recursion | present | used throughout `examples/selfhost/` |
| `Str`: `len`, `s[i]`, `slice`, `concat`, `==` | present | `docs/standard-library.md:9-20` |
| `List`: `list_new/push/len/get/set` | present | `docs/standard-library.md:24-35` |
| `read_file`, `write_file` | present | `docs/standard-library.md:19-20` |
| Records may hold `Str` | present | `Token` in `lexer_lib.nx:11-14` |
| **Records may hold `List`** | **absent** | `List` is not a nameable type: `error[T024]: unknown type 'List' for field 'names'` |
| **Lists of lists** | **absent** | `error[T049]: list element type mismatch: expected Int, found List[Int]` |
| **String ordering** | **absent** | `error[T008]: ordering comparisons require numerics, found Str` |
| **`print` of a record** | **absent** | `error[T028]: print does not support record values` |
| **`int` to text, text to `int`** | **absent** | no such builtin exists |
| **Function values / closures** | **absent** | no function types in `docs/language-reference/grammar.md:112` |
| **`argv`, process spawn, directory listing** | **absent** | not in the builtin list |
| **`stderr` write, `exit(code)`** | **absent** | only `panic(msg)`, which writes to stderr and exits 1 (`docs/language-reference/decisions.md:41-48`) |

(All four "absent" rows were confirmed by compiling small programs with
`nx check`; the exact diagnostics are quoted.)

### What the remaining stages need from the Rust side

* **The sema** (`crates/nexum-sema/src/lib.rs`, 2,054 lines) holds three
  hash maps: the function signature table
  (`crates/nexum-sema/src/lib.rs:449`), the record table (`:450`) and the
  per-scope variable table (`crates/nexum-sema/src/lib.rs:467`). It also
  runs a **monotone fixpoint** over the signature table, capped at 65
  iterations (`:323`), and defaults whatever is still unresolved to `Int` or
  `Unit` (`:408-417`).
* **The MIR** (`crates/nexum-mir/src/lib.rs`, 1,393 lines) is the
  self-referential structure the task asks about: `Ty` is recursive
  (`Ty::Array(Box<Ty>, usize)`, `:15-32`), `Instr` is a tagged union with
  variable-length payloads (`Vec<Value>`, `:47-135`), and `MirFn` holds
  `Vec<BasicBlock>` (`:156-168`). It uses `HashMap<String, Value>` for the
  environment (`:207`) and `HashSet<Value>` for the DCE liveness set
  (`:929`, `:953`).
* **The code generator** (`crates/nexum-codegen/src/lib.rs`, 1,281 lines)
  builds the IR as one `String`, prepends the runtime slice (`:119`),
  assigns string globals in first-use order (`:195-209`), escapes strings
  byte by byte (`:177-193`) and formats float constants with Rust's
  `Display` (`:466-482`).
* **The runtime slice** (`crates/nexum-runtime/src/lib.rs`, 679 lines) is
  generated as IR text by Rust and is a *part of every module the compiler
  emits*. A self-hosted code generator has to produce it too, or the
  bootstrap stops at the code generator.
* **The driver** (`crates/nexum-driver/src/main.rs`, 1,526 lines) does file
  I/O, import resolution, and hands the `.ll` to `clang`.

### The two facts that decide the strategy

1. **The Rust compiler is deterministic.** Every hash map in the compiler is
   used for lookup only; the ordered structures are `Vec`s. The DCE
   liveness set is a `HashSet` that is only ever queried for membership
   (`crates/nexum-mir/src/lib.rs:979`), the inliner's remap table is
   lookup-only (`:1230`), record definitions keep declaration order
   (`crates/nexum-sema/src/lib.rs:424-428`) and string globals get their
   names in first-use order (`crates/nexum-codegen/src/lib.rs:195-209`).
   **A bootstrap fixed point is therefore reachable - but only if the
   self-hosted compiler iterates its own tables in insertion order too.**
   A hash-ordered iteration would produce a different, equally valid
   module, and the gate would fail forever.
2. **The backend already supports what the sema needs.** `llvm_ty` maps
   `Ty::List(_)` to `ptr` (`crates/nexum-codegen/src/lib.rs:14`),
   `has_refs` says a `List` holds a reference (`:25`), and `scan_bytes`
   gives it 8 scan bytes (`:90`). **A record with a `List` field needs no
   backend change at all** - only the front end has to learn that `List` is
   a nameable type. This makes feature **F1** a front-end-only change.

## The design

### The stage map

| Stage | Deliverable | Gate | New artefact |
| ----- | ----------- | ---- | ------------ |
| 10b (done) | self-hosted lexer | `selfhosted_lexer_matches_rust_lexer` | - |
| 10c (done) | self-hosted parser | `selfhosted_parser_matches_rust_parser` | - |
| **11j.1** | `nxlib`: fat node, arena, linear map, text helpers | `selfhosted_collections_match` | fixture corpus |
| **11j.2** | self-hosted sema | `selfhosted_sema_diagnostics_match` + `selfhosted_sema_hir_matches` | `nexum_sema::normalize` |
| **11j.3** | self-hosted MIR lowering + passes | `selfhosted_mir_matches_rust_mir` | `nexum_mir::dump` |
| **11j.4** | self-hosted code generator | `selfhosted_codegen_matches_rust_codegen` | `nexum_codegen::emit_program` |
| **11j.5** | self-hosted driver + `nx --self-hosted` | `selfhosted_build_matches_rust_build` | driver mode |
| **11j.6** | the bootstrap fixpoint | `selfhosted_compiler_is_a_fixed_point` | - |

### Stage 11j.1, specified

**Why this stage and not the sema.** The sema is 2,054 lines of Rust and it
needs three things the rest of the compiler also needs: a recursive type
representation, a lookup table, and integer-to-text conversion. Isolating
those three into a 500-700 line library means the sema stage is a
*transcription* rather than a *design*, and the design decisions are made
once, in a file small enough to review in one sitting.

**The fat node.** One record shape for the whole compiler:

```
type Node:
    kind: Str    # the discriminant: "ty-int", "scope", "hir-let", ...
    text: Str    # the payload that is text: a name, a message
    a: Int       # payload 1
    b: Int       # payload 2
    c: Int       # payload 3
    d: Int       # payload 4
```

This is `examples/selfhost/parser.nx:21-26` generalised. It works because
`List` is monomorphic (D011) and the language has no sum types: one arena,
one node shape, one `kind` tag, everything variable-length in a side table.
The alternative - a separate arena per node shape - costs one copy of the
three-line arena helper per shape and gives up the ability to put a node of
kind A inside a node of kind B without a side table. **One fat node is the
right call**, and the parser has already proven the pattern.

**The arena.** Two `List`s per compiler instance:

```
let nodes = list_new()   # List of Node
let extra = list_new()   # List of Int - the side table
```

`arena_push(nodes, extra, kind, text, a, b, c, d) -> Int` returns the node's
index. `arena_child(extra, start, count)` walks a variable-length child
list, exactly as `parser.nx:279-307` does for call arguments.

Two rules the arena imposes, and the compiler must obey them:

* the arena is **append-only**; a node is never mutated after it is pushed,
  so an index is stable for the compiler's lifetime;
* the arena's element type is fixed by its first push, so the arena must be
  **seeded with a placeholder node** before the first real one - the same
  trick `parser.nx:770-775` uses.

**The linear map.** One map type for the whole compiler:

```
type Entry:
    key: Str
    val: Int

# A map is a List of Entry. Lookup is a linear scan.
```

`Map` is *not* a record, because a record cannot hold a `List` until
feature **F1** lands. A map is therefore a bare `List` handle, and every
map function takes that handle:

```
fn map_find(entries: List, key: Str) -> Int   # -1 if absent
fn map_put(entries: List, key: Str, val: Int)  # appends or replaces
fn map_len(entries: List) -> Int
```

Why a *linear* map and not a hash map:

* it is a single handle, so it can be passed as a parameter and returned
  without any new language feature;
* its iteration order is insertion order, which is exactly what the Rust
  compiler's output order requires (fact 1 above), so the map cannot
  accidentally break the bootstrap;
* the sema's scope tables hold a few hundred entries; a linear scan is
  acceptable at that size.

Why *one* map type is enough: the compiler's data is arena-indexed. A
symbol table maps a name to an `Int` index; a type table maps a name to an
`Int` type id; an interning table maps a string to an `Int` string id. Every
value the compiler stores is an index into an arena, so `Str -> Int` covers
all of them, and **no generics are needed** - which matters, because the
language has none and D011 leaves "a generalisation to other element types"
open.

The upgrade path is explicit: once **F1** lands, a map becomes a record
holding two lists (the hash slots and the insertion order), which is a
drop-in replacement for the linear map because the API is the same.

**The text helpers.** `int_to_str`, `str_to_int`, and a byte-wise string
compare. All three are ordinary Nexum code:

* `int_to_str` is repeated division by 10 plus a ten-element table of digit
  characters. It must handle `0`, negative values and `i64::MIN`
  (whose absolute value overflows - the standard trick is to work on the
  negative side, because `MIN / -1` wraps to `MIN` per D004).
* `str_to_int` is the inverse.
* A byte-wise string compare uses `str_char_at` and `==` on one-byte
  strings. **No numeric byte value is needed for a compare** - only for
  hashing and for LLVM escaping (see **F4**).

**The gate.** `selfhosted_collections_match` runs a Nexum program over a
fixed corpus of `(key, value)` pairs and dumps the resulting table in
insertion order; the Rust side does the same with a `Vec<(String, i64)>`
and a linear scan, and the two dumps must be identical. The same test
round-trips `int_to_str`/`str_to_int` over the edge values
(`0, 1, -1, 9, 10, 99, 100, i64::MAX, i64::MIN`) and over a pseudo-random
corpus. **The corpus is a new fixture file, and the Rust side of the
comparison is about 80 lines.**

### Stage 11j.2, the sema

**The sema's data model in Nexum.**

* `Ty` becomes fat nodes: `kind` is `"int" | "float" | "bool" | "str" |
  "unit" | "never" | "unknown" | "error" | "record" | "array" | "list"`,
  `text` holds the record name, `a` holds the element node's index and `b`
  the array length. This is `Ty::Array(Box<Ty>, usize)`
  (`crates/nexum-sema/src/lib.rs:15-32`) turned inside out, and it is the
  same transformation the parser already performs.
* A `Scope` becomes a fat node whose entries are the indices of the
  bindings it holds, kept in a side table of `Int` (the scope's `a` and `b`
  fields are the start and the count). With **F1** it becomes instead a
  record holding a `List` of entries directly, which is cheaper.
* The function signature table is a map from name to a fat node index; the
  record table is a map from name to a fat node index.
* The **fixpoint** is a loop over the signature table, exactly as
  `crates/nexum-sema/src/lib.rs:323-402` does, with the same convergence
  test (`ctx.functions == pass_table`) and the same 65-iteration cap. The
  self-hosted compiler's own source must be fully annotated so the cap is
  never reached (recommendation 8).

**Diagnostics.** A diagnostic is a fat node: severity, code, message
(`text`), and the span (file id, start, end) in the integer fields. The
*renderer* is the same shape as `nexum_diagnostics::render`
(`crates/nexum-diagnostics/src/lib.rs:44-75`): it needs the source text, the
line and column (a scan for newlines - `s[i]` and `len` are enough), and it
needs to build the `--> file:line:col` line, which needs **int to text**.

**The typed-HIR dump.** A new Rust module `nexum_sema::normalize`, in the
style of `nexum_parser::normalize`. It prints, one node per line, two spaces
of indent per level:

```
fn add
  param a
  param-type
    named Int
  return-type
    named Int
  let
    name total
    let-type
      named Int
    binop add
      var a
      int 1
```

The self-hosted sema prints the same shape. The dump must be **total**: every
HIR node kind appears, and the type of every expression appears next to it.

**The diagnostics gate.** The Rust side prints each diagnostic as
`severity<Tcode> file:start:end` and the self-hosted side prints the same
line. The *message text* is compared too, but as a **separate assertion**,
so that a wording change is visibly a wording change and not a structural
mismatch. The message is built with `concat` and `int_to_str`; it must match
the Rust `format!` strings word for word, which is brittle in exactly the
same way the parser's normalised form is brittle - and that brittleness is
the point: it is what makes the gate a contract.

**The corpus.** The sema gate must run over more than the happy path. The
29 files in `crates/nexum-driver/tests/fixtures/fail/` each expect a
specific diagnostic code, and they are the natural corpus: the self-hosted
sema must produce **the same set of diagnostic codes in the same order** for
each of them. That is a strong gate and it costs nothing extra, because the
fixtures already exist.

### Stage 11j.3, the MIR

The MIR is the stage where the language's rules bite hardest, and the
answer is the arena the parser already demonstrates.

* `Instr` is a tagged union with `Vec<Value>` payloads
  (`crates/nexum-mir/src/lib.rs:47-135`). In Nexum it is a fat node whose
  `kind` is the instruction, whose integer fields are the operands
  (`Value` is a `usize` newtype, `:10`) and whose variable-length payload
  (call arguments, record fields, array elements) lives in the side table
  as a `(start, count)` range, exactly as `parser.nx:299-307` does for call
  arguments.
* `BasicBlock` is a fat node: `a` is the terminator's discriminant, `b`
  and `c` are the branch targets, `d` is the start of the instruction list
  in the side table.
* `MirFn` is a fat node: the block range in the side table, the parameter
  list in the side table, and the value-type table as a side table of
  `(value index, type index)` pairs.
* `Ty` inside the MIR is the same fat node as in the sema - **one type
  representation shared by the sema and the MIR**, which is what the Rust
  code does too (`nexum_sema::Ty` is used directly by `nexum_mir`).
* The environment (`HashMap<String, Value>`,
  `crates/nexum-mir/src/lib.rs:207`) is a map from a name to a value index.
* The DCE liveness set (`HashSet<Value>`, `:929`, `:953`) is a set of
  integers; a `List` of `Int` with a linear scan is adequate, and better
  still is a **bitmap** over the dense value range, since the values in one
  function are dense (`Value(0..n)`).

**The gate.** A new Rust module `nexum_mir::dump` prints the MIR: for each
function, its parameters, its blocks in order, each block's instructions in
order (with their operands resolved to value indices and their types
resolved to type names), and its terminator. The self-hosted lowering
prints the same. The gate compares the two dumps.

**The const folder is a trap.** `nexum_mir::const_fold`
(`crates/nexum-mir/src/lib.rs:792`) folds constant binops, including
floats. A folded float constant has no source text, and its shortest
round-trip decimal cannot be reproduced in Nexum (see **F3**). The
recommendation is to **stop folding float arithmetic in both compilers** -
it is a small behaviour change, it removes a class of divergence, and it
means every float constant in the IR comes from a literal.

### Stage 11j.4, the code generator

**The program half of the IR.** `nexum_codegen::emit`
(`crates/nexum-codegen/src/lib.rs:109`) emits the runtime slice first
(`:119`) and then the program. The gate must compare only the program, so
`emit` should be split into `emit_runtime(target) -> String` and
`emit_program(mir, target) -> String`, with `emit` the concatenation of the
two. That is a small, safe change to the Rust compiler that makes the gate
meaningful.

**The runtime slice.** The self-hosted code generator has to produce the
runtime slice too, or the bootstrap stops here. Two options:

* **Port `nexum_runtime::runtime_ir` to Nexum.** It is 679 lines of Rust
  that is almost entirely string literals of LLVM IR, so the port is
  mechanical but it is 700-900 lines of Nexum and it is the single largest
  mechanical chunk of the project.
* **Wait for 11b.** Phase 11b puts inline assembly in the language, and
  D013/D014 say the operating system is reached through inline assembly in
  the generated IR. Once the syscall layer is expressed *in the language*,
  it is ordinary Nexum source that the compiler compiles, and the runtime
  slice shrinks to the collector, the heap, the list helpers and `panic` -
  a few hundred lines that are stable.

  **This is a real dependency of 11j.4 on 11b, and it is the strongest
  argument for the two phases being sequenced as the roadmap has them.** If
  11b is delayed, 11j.4 must carry the port of `runtime_ir` as well.

**The string building.** `nexum_concat` allocates a new buffer and copies
both arguments (`crates/nexum-runtime/src/lib.rs:191`), so building a
module by repeated concatenation is quadratic. Three ways out:

* **Append-mode file write (**F2**). The code generator writes chunks to
  the `.ll` file as it produces them and never materialises the module as a
  `Str`. This is the clean answer, it also serves the real driver in 11j.5,
  and it is about twenty lines of runtime.
* **A chunked builder.** A `List` of `Str` chunks, joined at the end.
  Expressible today, but the join is still quadratic in the number of
  chunks times the total length, and it still needs the whole module in
  memory.
* **An arena (11d).** The right long-term answer, and it is what 11d is
  for, but it is the last phase before 11j and 11j.4 cannot wait for it if
  11j.4 is to be reached on schedule.

**The target description.** `TargetSpec`
(`crates/nexum-target/src/lib.rs:3-10`) is a record; `by_name` and
`data_layout_str` are matches over the arch and the OS
(`:38-100`). All of it is expressible. The one wrinkle is `TargetSpec::host()`
(`:12-37`), which reads the host at run time in Rust; in a self-hosted
compiler the host is a **compile-time constant** of the compiler binary, so
the self-hosted code generator takes the target as a parameter (or as a
constant chosen when the compiler was built).

**The float problem.** The Rust code generator formats a float constant with
Rust's `Display` and then appends `.0` if the result has no `.` or `e`
(`crates/nexum-codegen/src/lib.rs:466-482`). Rust's `Display` for `f64` is a
shortest-round-trip decimal that **never uses exponent notation** - so
`1e300` prints as a 301-digit number - and it is not reproducible in Nexum
without implementing Ryu. Three ways to make the gate exact:

1. **Change the Rust code generator to emit the hex form**
   (`double 0x3FB999999999999A`). LLVM accepts it, it is exact, and it
   needs a `float_to_bits` on the Nexum side (a runtime builtin: `bitcast
   double %f to i64`).
2. **Keep the literal's source text.** The MIR's `Const::Float` would carry
   the text as well as the value; the code generator prints the text; the
   Rust side of the gate parses the text back to a `f64` and re-emits the
   hex. This is exact, needs no `float_to_bits` on the Nexum side, and it
   requires the const folder not to fold floats (see stage 11j.3).
3. **Replace every float constant with a placeholder on both sides.** The
   weakest of the three, and it loses the check that the *value* is right -
   though the value is already checked by the lexer gate, which compares
   the literal's text.

**Recommendation: option 2**, with option 1 as the fallback if changing the
MIR's `Const` is judged too invasive. Option 3 is the minimum that lets the
gate be written today.

### Stage 11j.5, the driver

The driver is the stage where the language's gaps stop being academic.

* **Input.** The two existing self-hosted programs read a **fixed file
  name** (`nexum_selfhost_input.nx`,
  `examples/selfhost/lexer.nx:11`) and the test harness copies the fixture
  there (`crates/nexum-driver/tests/backend.rs:181`). The self-hosted
  compiler should keep that convention through the differential stages; the
  corpus is copied in, the output is written to a fixed name, and the Rust
  side reads it. `argv` is then not needed for any gate.
* **Imports.** `import "path.nx"` resolves relative to the importing file
  and de-duplicates by canonical path. Path manipulation - join, directory,
  normalise `..` - is string work and is expressible with `slice` and
  `concat`; the de-duplication needs a set of paths, i.e. the map from
  11j.1 keyed by the canonical path.
* **Output.** Write the `.ll` with the append-mode write (**F2**).
* **Diagnostics to stderr, and the exit code.** `panic(msg)` writes the
  message to stderr and exits with 1 (`docs/language-reference/decisions.md:41-48`),
  so the error path already works: build the whole diagnostic text and
  `panic` with it. A proper `eprint` and `exit(code)` would be better -
  the error path should be able to report several diagnostics and exit with
  a code that distinguishes "compilation failed" from "internal error" - but
  they are conveniences, not blockers.
* **Linking.** The self-hosted compiler cannot spawn `clang` (no process
  support). The split is therefore: **the self-hosted compiler produces the
  `.ll`; the Rust toolchain links it.** `nx --self-hosted` runs the
  self-hosted compiler to produce the `.ll` and then links it with the
  existing toolchain code (`crates/nexum-driver/src/main.rs:1128-1151`).
  This keeps the bootstrap free of FFI and of process control, and it is
  the same split the roadmap already accepts for the *toolchain*: "the
  compiler may still be written in Rust and use LLVM - that is the
  *toolchain's* business" (`docs/native-roadmap.md:13-15`).
* **The gate.** `nx build --self-hosted` on the run fixtures
  (`crates/nexum-driver/tests/fixtures/run/`) must produce the same
  executable behaviour - same stdout, same exit code - as `nx build`. The
  existing `run_outputs_match_fixtures` gate
  (`crates/nexum-driver/tests/backend.rs:93-138`) is the template.

### Stage 11j.6, the bootstrap

**The three stages.**

```
stage 0:   nx (Rust)  --compiles-->  nxc0        (the self-hosted compiler)
stage 1:   nxc0       --compiles-->  nxc1        (itself)
           gate: nxc1 and nxc0 produce identical output on the corpus
stage 2:   nxc1       --compiles-->  nxc2
           gate: nxc2 and nxc1 produce identical output on the corpus
```

The fixpoint is reached when `nxc_{k+1}` and `nxc_k` agree. Comparing
*output* rather than *binaries* is the right choice: the bootstrap's
property is that the compiler is a function on programs, and the function
must be stable, not that the machine code is identical.

**The first bootstrap step is a rehearsal, and it is nearly free.** The
first thing to try is not "compile the whole compiler" but "point the two
gates that already exist at the compiler's own source":

1. extend the lexer gate's corpus to include `examples/selfhost/*.nx`
   (1,135 lines - a hundred times the current fixtures);
2. extend the parser gate's corpus likewise;
3. require both to agree with the Rust implementations.

This costs a corpus change and it tests the thing the bootstrap needs: can
the self-hosted front end read the language the compiler is written in? It
will immediately expose the gaps - the self-hosted parser does not parse
path assignments, so the compiler's own source must not use them; the
compiler's own source must not use a `List` in a record field until
**F1** lands; it must not use exponent literals (the lexer has no exponent
form, `crates/nexum-lexer/src/lib.rs:303-330`); and it must not print a
record.

**What the bootstrap requires of the language, beyond the gates:**

* **Determinism.** Already discussed: the self-hosted compiler's map must
  iterate in insertion order. The linear map of 11j.1 does this by
  construction.
* **Full annotations.** The 65-iteration fixpoint cap
  (`crates/nexum-sema/src/lib.rs:323`) and the silent `Int`/`Unit` default
  (`:408-417`) are the two ways a large program can be compiled *wrongly*
  rather than rejected. The self-hosted compiler's own source must annotate
  every parameter and return type, so the fixpoint has nothing to infer.
* **The list discipline.** A list's element type is fixed by its first
  `list_push`, and the refinement only works when the list is a plain named
  binding in the current scope
  (`crates/nexum-sema/src/lib.rs:1403-1407`); a list of lists is rejected
  outright. The compiler's arenas satisfy this: one list, one node shape,
  one element type, and the side table is a list of `Int`.
* **Performance.** `benchmarks/compiler_perf.md` measures 16-38 ms (minimum)
  and 22-44 ms (median) for a 400-byte program in a *debug* build. The
  self-hosted compiler's source will be 8,000-10,000 lines. The stage-1
  compiler must be built at `-O2` (`nx build --release`), and the stage-2
  compiler must compile 10,000 lines of Nexum in a time a human will
  tolerate. **This is the risk that 11b-11f exist to remove**: a quadratic
  `concat`, a linear-scan map and a byte-at-a-time hash all multiply.

### The feature checklist

Every language or standard-library feature the later stages need, and where
it stands. This is the list that phases 11b-11f can be checked against.

| # | Feature | Needed by | Exists? | Action |
| - | ------- | --------- | ------- | ------ |
| **F1** | **`List` as a nameable type** (`List` or `List[T]` in a type annotation), so a record can hold a map | 11j.2 (sema), 11j.4 | **No** - `error[T024]: unknown type 'List' for field 'names'` | Front-end only: a `TypeExpr::List` variant, `tty` handling, the type-annotation check. **The backend already supports it** (`llvm_ty(Ty::List(_)) == "ptr"`, `has_refs` true, `scan_bytes` 8). A grammar change and a decision entry are required. |
| **F2** | **Append-mode file write** (`write_file_append`, or open/write/close) | 11j.4, 11j.5 | **No** - `write_file` truncates | ~20 lines of runtime IR plus a builtin. Without it the code generator needs a quadratic chunked builder. |
| **F3** | **An exact float representation for the IR gate** (`float_to_bits`, or float literals keep their source text and the const folder stops folding floats) | 11j.4 | **No** | Either a runtime builtin (`bitcast double %f to i64`) or a small change to `Const::Float` plus a change to `const_fold`. Rust's `Display` is not reproducible in Nexum. |
| **F4** | **A numeric byte value from a string** (`byte_at(s, i) -> Int`) | 11j.1 (hashing), 11j.4 (LLVM escaping) | **No** - `str_char_at` returns a one-byte `Str` | Workaround exists (a 256-entry lookup table) but it is 256 comparisons per byte. A runtime builtin is a few lines. |
| F5 | Integer to text and text to integer | 11j.2, 11j.4 | No, but **expressible today** in Nexum | Library code, not a language feature. Must handle `i64::MIN`. |
| F6 | String ordering (`str_cmp`) | optional | **No** - `error[T008]` | **Not needed** by this design: the linear map iterates in insertion order, and diagnostics are ordered by source position (integers). |
| F7 | Maps, sets, an interning table, a sort | 11j.1 onward | **No** in the language; **expressible today** as a library over `List` | `docs/standard-library.md:51` names a `collections` module as planned, but **no phase owns it**, and `docs/self-hosting.md:41` lists "Hash maps in std" as a prerequisite without a date. This plan needs it in 11j.1. |
| F8 | Function values / closures | would simplify 11j.1 | **No** | **Not needed** by this design: one fat node, one map type, one arena. Would be needed by a more ergonomic design. |
| F9 | `argv`, process spawn, directory listing | 11j.5 (a complete `nx` replacement) | **No** | **Not needed for any gate**: the fixed-filename convention already in use. Needed only for a drop-in `nx`. Linking stays in the Rust toolchain, so process spawn is not needed either. |
| F10 | `eprint`, `exit(code)` | 11j.2, 11j.5 | **No** - only `panic` | `panic` covers the error path (stderr + exit 1). A proper pair is a convenience. |
| F11 | A nameable `Bool`-carrying `Option`/`Result` | would simplify the sema | **No** | **Not needed**: the sema's "no value" is the absence of a node, and "error" is a `kind` tag. |
| F12 | Performance: 11b (inline asm), 11c (numeric tower), 11d (memory and layout, arenas), 11f (SIMD) | 11j.4, 11j.6 | 11b-11f are on the roadmap; **11b is a genuine dependency of 11j.4** (it is what lets the runtime slice stop being injected IR) | The stages are correct as the roadmap has them. The programme should know that 11j.4 waits on 11b, and that a quadratic `concat` and a linear map make the bootstrap slow until 11d lands. |

**The four the roadmap does not have: F1, F2, F3, F4** - plus the
scheduling of F7, which is named in the standard-library plan but owned by
no phase.

### The estimate

**What has to be written in Nexum rather than in Rust.** (Line counts are
`Get-Content .Count` over the crate's `src`, blanks included.)

| Component | Rust today | Self-hosted, planned | Status |
| --------- | ---------- | -------------------- | ------ |
| lexer | 537 | 334 | done |
| parser (parse + AST + normalised form) | 1,550 | 801 | done |
| sema | 2,054 | 2,300-3,000 | to do |
| MIR (lower + passes + verify) | 1,393 | 1,400-1,900 | to do |
| code generator | 1,281 | 1,400-1,900 | to do |
| runtime slice | 679 | 700-900, or ~0 after 11b | to do |
| source map + diagnostics | 153 | 200-300 | to do |
| target description | 105 | 150-200 | to do |
| driver (the compile path only) | ~120 | 500-800 | to do |
| **total** | **7,872** | **~8,000-10,100** | **1,135 done** |

**Remaining: roughly 7,000-9,000 lines of Nexum**, and the whole
self-hosted compiler will be 8,000-10,000 lines of Nexum.

The one measured data point is the ported front end: **1,135 lines of Nexum
against 2,087 lines of Rust, a ratio of 0.54.** The ported front end is
unusually favourable to Nexum - a recursive-descent parser is mostly control
flow, which Nexum expresses well, and the Rust side carries the source map,
the loader and the diagnostics plumbing that the self-hosted version handles
differently. The rest of the compiler is less favourable:

* the **sema** is a type-directed traversal with a `match` over twenty
  statement kinds and `Option` plumbing; in Nexum a `match` becomes a chain
  of `if`s with early returns, which is *longer* than the Rust, and the
  `Option`s become arena indices with `-1` for "none", which is *shorter*.
  The two roughly cancel, so the ratio is near 1.1-1.5;
* the **code generator** is string building, and a `concat` chain is much
  longer than a `format!`; the ratio is near 1.1-1.5;
* the **MIR** is mechanical; the ratio is near 1.0-1.4;
* the **driver** has to do path handling and argument parsing by hand, where
  Rust has `std::path` and `clap`; the ratio is near 4.

So the *effort* per line of Nexum is roughly 1.8 lines of Rust (the inverse
of the front end's 0.54), and 7,000-9,000 lines of Nexum is **12,000-16,000
lines of Rust-equivalent effort**. Against a target of 7,872 lines of Rust,
**the self-hosting programme is roughly one and a half to twice the size of
the compiler it replaces.**

That is the honest number, and it is why the staged plan matters: at this
size the only way to keep the work verifiable is to make every stage a gate,
and the only way to make it *finishable* is to keep the Rust compiler as a
permanent fallback rather than betting the toolchain on the port.

**The other half of the cost is in Rust, not Nexum:**

* four new gates (about 400 lines of Rust across four test files);
* two new dump modules, `nexum_sema::normalize` and `nexum_mir::dump`
  (about 500 lines of Rust, and they are the *specification* the Nexum code
  is written against - so they should be written first);
* the **F1** front-end change (about 150 lines of Rust: a `TypeExpr` variant,
  `tty`, the annotation check, the grammar, the decision entry, the
  fixtures);
* the **F2**, **F3** and **F4** runtime builtins (about 100 lines of
  runtime IR);
* the `emit`/`emit_program` split and the `const_fold` change (about 50
  lines).

**Total programme: ~7,000-9,000 lines of Nexum and ~1,200 lines of Rust.**
In calendar terms, at the pace the front end was ported (1,135 lines of
Nexum), the Nexum work alone is a multi-agent effort measured in months,
not weeks. **The orchestrator should decide whether 11j is a phase of the
same size as 11a or whether it is a programme of its own**, because the
roadmap's phase table does not distinguish the two.

## Risks

1. **The self-hosted compiler cannot compile its own source.** The two
   silent-failure modes are the 65-iteration inference cap
   (`crates/nexum-sema/src/lib.rs:323`) and the `Int`/`Unit` default
   (`:408-417`): a deep call graph compiles *wrongly* rather than being
   rejected. *Mitigation:* the compiler's own source is fully annotated;
   the fixpoint has nothing to infer. *Would falsify the recommendation if:*
   a fully annotated 8,000-line program still fails to converge - then the
   inference machinery itself has to be fixed first.
2. **The output drifts from the Rust compiler's because of iteration
   order.** The Rust compiler's output order is declaration order and
   first-use order. A hash-ordered map in the self-hosted compiler would
   produce a different but equally valid module, and every downstream gate
   would fail for a reason that has nothing to do with correctness.
   *Mitigation:* the map iterates in insertion order, by construction, and
   the gate's failure message says so.
3. **The bootstrap is too slow to be usable.** Every `concat` copies the
   whole string; every map lookup is a linear scan; every hash is a byte
   loop. Compiling 8,000 lines of Nexum with a quadratic string builder is
   the difference between minutes and hours. *Mitigation:* the chunked
   builder and the append-mode write (**F2**), then 11d's arenas.
4. **The gates are too strict and become a chore.** The normalised forms
   are textual contracts; every wording change in a diagnostic message
   breaks a gate. *Mitigation:* the diagnostics gate compares codes and
   spans first and the message separately, so a wording change is a
   one-line diff rather than a red gate. *This risk is accepted*: the
   brittleness is what makes the gate a contract.
5. **The corpus is too small.** The gates today cover 9 example programs
   and 35 parse fixtures. The self-hosted compiler will be validated
   against a much larger program, and the first thing that will break is
   probably the parser (path assignments) or the sema (a diagnostic
   ordering). *Mitigation:* the corpus grows with the stages, and the
   bootstrap rehearsal (stage 11j.6) points the existing gates at the
   compiler's own source first, which is a hundred times bigger than
   anything tested so far.
6. **The runtime slice is the wrong thing to port.** Porting
   `nexum_runtime::runtime_ir` to Nexum is 700-900 lines of mechanical
   work that 11b would make unnecessary. *Mitigation:* do 11b first. *This
   is the one place where the phase order in the roadmap is load-bearing
   and not merely convenient.*
7. **The estimate is optimistic.** The one measured ratio (0.54) comes from
   a component with an unusually favourable shape - a recursive-descent
   parser is mostly `if`/`return`, and the language expresses that well.
   The sema is a different shape: it is full of `match` over type tags and
   `Option` plumbing, both of which the language lacks, and the fat-node
   discipline may not absorb all of it. The code generator is worse still,
   because a `concat` chain is longer than a `format!`. *The honest range
   is 7,000-9,000 lines of Nexum, and the pessimistic end is roughly
   12,000 - and that pessimistic end is what the *effort* estimate should
   be planned against.*

## Open questions

1. **Does 11j stay one phase, or become 11j.1-11j.6 in the phase table?**
   *Recommendation:* give it six lines in the table, with the gates named.
   A single line hides the fact that the phase is a programme, and the
   gates are the deliverable, not the code.
2. **Where do F1 and F2 land?** F1 (a nameable `List` type) is a
   front-end change with a grammar change and a decision entry; F2 (an
   append-mode file write) is a runtime change. Both are small. *Recommendation:*
   put **F1** in 11d (it is a memory-and-layout concern: "can a record hold
   a managed type?" is exactly 11d's question) and **F2** in 11a (it is a
   file-routine concern, and 11a is already rewriting the file routines).
   F3 and F4 are small enough to ride along with whichever phase touches
   the runtime.
3. **Should the self-hosted compiler replace the Rust one, or sit beside
   it?** *Recommendation:* beside it, indefinitely, with the Rust one as the
   reference and the release compiler. The self-hosted compiler's value is
   that it proves the language; it is not obviously a better compiler (it
   will be slower, and the Rust one has LLVM in it). The exit gate in
   `docs/self-hosting.md:49-53` says "passes the same
   conformance/differential suite", not "replaces the Rust compiler" - and
   that is the right target.
4. **Does the MIR need to be the same MIR?** The Rust pipeline is
   AST -> HIR -> MIR -> IR, with three representations. A self-hosted
   compiler could go AST -> IR directly, which would be much less code -
   but then the MIR stage of the plan disappears and the "compare the
   normalised MIR" gate has no counterpart. *Recommendation:* keep the
   three-stage pipeline. The MIR is what makes the optimiser testable, and
   the optimiser is where a self-hosted compiler's value would be largest.
5. **Should the sema dump compare types by name or by structure?**
   *Recommendation:* by name - `named Int`, `named Point` - with the
   *record* definitions dumped separately at the top of the dump, so that
   a change in a record's fields is visible as a change in the header
   rather than as a diff in every use.
6. **What is the corpus for the sema and the MIR gates?** *Recommendation:*
   the 29 `fail/` fixtures (for the diagnostics), the 35 `parse/` fixtures
   and the 9 examples (for the HIR and the MIR), and then the run
   fixtures. A fourth corpus - the self-hosted compiler's own source - is
   what the bootstrap rehearsal uses, and it should be added to the sema
   and the MIR gates as soon as they exist.
7. **Is the 65-iteration inference cap going to be enough for the
   self-hosted compiler?** This is not answerable until the compiler
   exists, and it is the single most likely cause of the first bootstrap
   failure. *Recommendation:* find out early - write a 500-line Nexum
   program with a deliberately deep call graph and see whether it
   converges. That is a two-day experiment and it de-risks the whole
   programme.
