# Self-hosting (Phase 10, optional)

Status: **in progress**. The architecture spec lists self-hosting as an
*optional* phase that should only begin once the language features it
depends on are stable. The bootstrap compiler remains the reference
implementation; the conformance fixtures
(`crates/nexum-driver/tests/fixtures/`) are the specification.

## Blockers, and where they stand

1. **Module system / imports** (D002 is still "one file = one module"). The
   compiler is split across ~10 crates; without imports it cannot be
   expressed. **Done**: `import "path.nx"` exists, with a shared namespace,
   canonical-path de-duplication and cycle detection.
2. **Standard library I/O** - file reading, directory traversal, process
   spawning. The bootstrap has no `io` module. **Partly done**:
   `read_file` / `write_file` exist; directory traversal and process
   spawning do not, and stage 10f needs both.
3. **String indexing and slicing** - the lexer needs `src[i]` / substrings.
   **Done**: `s[i]` yields one byte and `slice(s, lo, hi)` takes a byte
   range. Note that strings are byte strings, so a slice is a byte range.
4. **Dynamic collections** - token streams, AST nodes, symbol tables need
   growable vectors. **Done**: `List` is a monomorphic growable collection
   (`list_new`, `list_push`, `list_len`, `list_get`, `list_set`).
5. **Generics / traits** - AST nodes are a tagged union family; Nexum has
   no generics or sum types beyond records. **Open**: a recursive AST
   cannot be a record (a record cannot contain itself), so the AST uses a
   flat node arena with integer indices, which is the data-oriented design
   the spec prefers anyway.
6. **Error recovery / diagnostics** - the self-hosted compiler must emit
   the same structured diagnostics; needs span tracking and a diagnostic
   sink. **Open**.

## Staged plan

| Stage | Deliverable | Prerequisite |
| ----- | ----------- | ------------ |
| 10a | Port the lexer to Nexum (single file, string input). **Done** - see `examples/selfhost/lexer.nx`. | String indexing + a small token type. (Done) |
| 10b | Lexer with a token stream, validated against the Rust lexer. **Done** - the self-hosted lexer builds a `List` of tokens and the differential gate compares it with `nx lex` token by token. | Growable lists, file I/O. (Done) |
| 10c | Port the parser (recursive descent) onto a flat AST arena. **Partly done** - functions, `let`, assignment, expression statements, `return`, `if`/`else`, `while`, `for`, record declarations, `import` and type annotations are parsed and validated against the Rust parser; path assignments (`a.b[i] = v`) and import resolution are still to come. | `List` (Done); node arena needs index-based records. (Done) |
| 10d | Port name resolution + type checking. | Hash maps in std. |
| 10e | Port MIR lowering + optimization passes. | - |
| 10f | Port codegen (LLVM IR text emission). | String building (already have concat). |
| 10g | Port the driver (file I/O, process). | std::io + std::process. |
| 10h | Build the self-hosted compiler and run the conformance + differential suite against it. | All of the above. |

## Exit gate

> Self-hosted compiler builds reproducibly and passes the same
> conformance/differential suite.

This gate is only meaningful once stages 10c-10g are complete. Until then,
the Rust bootstrap compiler remains the reference implementation.

## What is demonstrated today

* The language can express recursive functions, arrays, records, control
  flow, managed strings and a mostly-precise GC (Phases 1-4), so the *shape*
  of a compiler is expressible.
* **Stage 10a/10b are demonstrated**: `examples/selfhost/lexer.nx` is a
  working self-hosted lexer for a subset of Nexum. It reads a source file,
  tokenizes it into a `List` of tokens and prints the token stream in the
  same format as `nx lex`, so the two can be compared token by token.
* The matching differential test
  (`selfhosted_lexer_matches_rust_lexer`) compares the two token streams for
  every example program and is part of the normal test suite.
* **Stage 10c is partly demonstrated**: `examples/selfhost/parser.nx` parses
  the source file into a flat AST arena (a `List` of `Node` records whose
  children are indices, plus a side table for variable-length child lists)
  and prints it in the same normalized form as `nx parse`. The differential
  test `selfhosted_parser_matches_rust_parser` compares the two parsers over
  the fixtures in `crates/nexum-driver/tests/fixtures/parse/`, and the two
  agree on every example program except the two that use path assignments
  (`examples/particles.nx`, `examples/segments.nx`).

Four language-level lessons came out of writing them:

* `and` and `or` do **not** short-circuit, so `i < n and src[i] != "\n"`
  reads out of bounds. Code that indexes has to test the bound first.
* Strings are byte strings and `s[i]` yields one byte, so the lexer is
  byte-oriented; sources are expected to be ASCII.
* A record cannot contain itself, so a tree of records is not expressible.
  The AST is an arena of flat records with integer child indices, which is
  the data-oriented shape the spec prefers anyway. A list's element type is
  fixed by its first `list_push`, so the arena is seeded with a placeholder
  node to fix the type before the parser fills it in.
* A list cannot be popped, so a stack in a list has to be managed by
  hand: the lexer's indent stack overwrites the value of a level that is
  re-entered and only grows the list when a new level is reached.

## Collector defect found by the self-hosted lexer, and fixed

The self-hosted lexer was the first program to build a `List` of records
whose fields are heap strings and then allocate heavily while reading the
list back. It exposed a defect in the collector: the printed text of the
tokens came out as garbage, and a large enough program crashed with an
access violation.

**Cause.** `nexum_mark_ptr` reported "this object was **already** marked"
where the trace loop expected "this object was **newly** marked". The
fixpoint that propagates marks through the heap therefore stopped after a
single pass. An object that is reachable only *through* another object - a
`List` element buffer, for instance - is discovered after its referrer, so
it was never traced: the buffer and the strings it held were reclaimed
while still in use. Small programs did not show the defect because they
never reached the collection watermark.

**Fix.** `nexum_mark_ptr` now reports "newly marked", so the trace repeats
its pass over the heap until a pass marks nothing new. The collector's
contract is documented in `docs/runtime-abi.md`.

**Regression coverage.** Two independent gates fail without the fix:

* `selfhosted_lexer_matches_rust_lexer` runs the self-hosted lexer and the
  Rust lexer over every example program and requires the token streams to
  be identical.
* The `list_records_gc` run fixture builds a list of records with heap
  string fields, collects repeatedly, and reads every row back.
