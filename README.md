# Nexum — Language Design & Implementation Blueprint (Bootstrap)

Nexum is a statically typed, ahead-of-time compiled native language with a clean,
data-oriented surface. This repository is the **bootstrap compiler** implementing Phases 0–9 of the
design blueprint (Phase 10, optional self-hosting, is planned).

## Status

Working vertical slice: a `.nx` source file compiles to a native executable on
Windows (x86_64) using LLVM/clang. Supported today:

- Primitives: `Int`, `Float`, `Bool`, `Str`
- Functions with parameters and return types; unannotated signatures are
  inferred (fixpoint over call sites and `return` statements)
- `mut` parameters: by-reference (the callee mutates the caller's binding);
  plain parameters are immutable copies
- `let` / `let mut` and assignment with mutability checking, including
  compound assignment (`x += e`, `xs[i] *= 2`) with the target evaluated once
- Records: `type Vec2:` declarations, positional construction
  (`Vec2(1.0, 2.0)`), field access `p.x`, and nested field assignment
  `a.b.c = v` (functional update of the whole binding)
- Fixed-size arrays: `[1, 2, 3]`, indexing `xs[i]` (compile-time checks for
  constant indices, runtime bounds check otherwise), `len(xs)`, and arrays of
  records (`ps[i].x = v`)
- `panic(msg)`: message to stderr, exit code 1, type `Never`
- `if` / `else`
- `while`, `for x in a..b`, and `for v in xs:` (for-each over an array)
- Managed strings: `concat(a, b)`, `collect()`, `heap_used()`, string
  equality (`==`/`!=`); precise non-moving mark-sweep GC; see
  `docs/runtime-abi.md`
- Arithmetic, comparisons, `and`/`or`/`not`
- Function calls (including recursion)
- `print(...)` builtin
- Indentation-based blocks (`#` comments)
- Diagnostics with stable codes and source spans
- LLVM IR verification before linking (`nx build` runs the IR parser +
  verifier + backend via clang)

See `docs/language-reference/grammar.md` for the grammar draft,
`docs/language-reference/decisions.md` for the decision record,
`docs/runtime-abi.md` for the runtime ABI,
`docs/package-format.md` for the package format,
`docs/standard-library.md` for the standard library,
`docs/compatibility.md` for compatibility and migration rules, and
`docs/self-hosting.md` for the self-hosting roadmap.

## Build

```sh
cargo build
```

Requires Rust (stable) and clang on PATH.

## Usage

```sh
# Type-check
nx check examples/hello.nx

# Build an executable
nx build examples/hello.nx -o hello.exe

# Compile, run, and show output
nx run examples/hello.nx

# Print the generated LLVM IR
nx emit-ir examples/hello.nx

# Show discovered toolchain
nx explain-toolchain
```

## Testing

```sh
cargo test --workspace
```

Includes:

- lexer/parser unit tests
- compile-pass fixtures (`crates/nexum-driver/tests/fixtures/pass/`)
- compile-fail fixtures with expected diagnostic codes
  (`crates/nexum-driver/tests/fixtures/fail/`)
- backend gate: every pass fixture and example is emitted to IR, verified by
  clang, and run fixtures are executed and compared against expected stdout and
  exit codes (`crates/nexum-driver/tests/backend.rs`)

Backend tests skip gracefully when clang is not installed.

## Benchmarks

```sh
powershell -ExecutionPolicy Bypass -File benchmarks\run.ps1 -Nx target\debug\nx.exe
```

## Layout

```
crates/
  nexum-source/        source files, spans, source map
  nexum-diagnostics/   diagnostic model + rendering
  nexum-lexer/         indentation-aware lexer
  nexum-parser/        recursive descent + Pratt parser, spanned AST
  nexum-sema/          name resolution, type checking + inference, HIR
  nexum-mir/           MIR (CFG/SSA values), lowering, verifier, optimization hook
  nexum-target/        target descriptors
  nexum-toolchain/     clang/linker discovery, IR verification
  nexum-codegen/       MIR -> LLVM IR text
  nexum-runtime/       runtime support (placeholder)
  nexum-driver/        `nx` CLI
examples/              sample programs
benchmarks/            benchmark programs, harness, baseline
docs/                  language reference, architecture notes
```

## License

Apache-2.0
