# Self-hosting (Phase 10, optional)

Status: **planned / not yet started**. The architecture spec lists self-hosting
as an *optional* phase that should only begin once the language features it
depends on are stable.

## Why not now

The bootstrap compiler is written in Rust. Porting it to Nexum requires
language features that do not exist yet:

1. **Module system / imports** (D002 is still "one file = one module"). The
   compiler is split across ~10 crates; without imports it cannot be expressed.
2. **Standard library I/O** — file reading, directory traversal, process
   spawning. The bootstrap has no `io` module.
3. **String indexing and slicing** — the lexer needs `src[i]` / substrings;
   `Str` only supports `concat`, `len`, equality today.
4. **Dynamic collections** — token streams, AST nodes, symbol tables need
   growable vectors; only fixed-size arrays exist.
5. **Generics / traits** — AST nodes are a tagged union family; Nexum has
   no generics or sum types beyond records.
6. **Error recovery / diagnostics** — the self-hosted compiler must emit the
   same structured diagnostics; needs span tracking and a diagnostic sink.

## Staged plan

| Stage | Deliverable | Prerequisite |
| ----- | ----------- | ------------ |
| 10a | Port the lexer to Nexum (single file, string input). | String indexing + a small token type. |
| 10b | Port the parser (recursive descent). | Dynamic arrays or arena-allocated nodes. |
| 10c | Port name resolution + type checking. | Hash maps in std. |
| 10d | Port MIR lowering + optimization passes. | — |
| 10e | Port codegen (LLVM IR text emission). | String building (already have concat). |
| 10f | Port the driver (file I/O, process). | std::io + std::process. |
| 10g | Build the self-hosted compiler and run the conformance + differential suite against it. | All of the above. |

## Exit gate

> Self-hosted compiler builds reproducibly and passes the same
> conformance/differential suite.

This gate is only meaningful once stages 10a–10f are complete. Until then,
the Rust bootstrap compiler remains the reference implementation and the
conformance fixtures (`crates/nexum-driver/tests/fixtures/`) are the
specification.

## What is demonstrated today

* The language can express recursive functions, arrays, records, control flow,
  managed strings and a precise GC (Phases 1–4), so the *shape* of a
  compiler is expressible.
* `examples/selfhost_lexer.nx` is reserved for a future lexer port; it is
  not yet checked in because the required string primitives are missing.

## Recommendation

Do not start self-hosting until the module system and `std::io` land. The
higher-leverage work is Phase 11+ (ecosystem, tooling, performance), which
benefits both the Rust and the future self-hosted compiler.