# Nexum (nx)

Python-simple syntax. Native compiler. LLVM backend.

```powershell
nx examples\hello.nx          # run (tree-walk interpreter)
nx run examples\hello.nx      # build if stale, then run native
nx check examples\hello.nx    # static type check
nx build examples\hello.nx    # native exe via LLVM (needs clang)
nx build --emit-ir            # print the generated LLVM IR
nx dump-ir examples\hello.nx  # effect summaries (reads/writes/prints)
nx update                     # self-update exe + extension
nx setup --apply              # wire .nx icons into your VS Code theme
```

Prototype with the interpreter, ship with the compiler:

```
parallel:          # tasks run on threads when conflict-free
    a = fib(20)    # (deterministic: same output every run)
    b = fib(20)
```

## What the compiler does for you

You write ordinary code; the compiler finds the expensive parts.

- **Automatic memoization.** `nx-ir` proves a function is pure (no
  shared reads or writes, no printing, no heap). Pure functions are
  cached automatically — no annotation, no source change. `fib(90)` is
  instant instead of ~2.8e18 calls.
- **Unboxing.** A value whose type is statically known lives in a bare
  register (`i64`/`double`/`i1`) and gets raw LLVM arithmetic. Boxing is
  re-inserted only where a dynamic boundary needs it: call arguments,
  returns, list elements, module globals.
- **Type inference from use.** NX has no annotations and no overloading,
  so `n - 1` proves `n` is `Int` and `xs[i]` proves `i` is `Int`. That is
  what lets the backend skip the box.
- **Deterministic parallelism.** Conflict-free tasks run on a thread
  pool sized to the batch, on every platform; conflicting ones serialize
  in program order. Output is byte-identical on every run, and CI checks
  that by running the parallel example repeatedly and diffing.
- **Memory planning.** `nx-mem` classifies each local Unique or Shared
  and releases buffers automatically. You never write a smart pointer or
  a lifetime.

```
$ nx build examples\unbox.nx && nx run examples\unbox.nx     # unboxed
NX_NOUNBOX=1 nx build examples\unbox.nx                      # all boxed
```

Both produce identical output; the unboxed build is ~6x faster on
numeric loops.

### Environment variables

| Variable | Effect |
| --- | --- |
| `NX_NOMEMO=1` | Disable automatic memoization |
| `NX_NOUNBOX=1` | Disable unboxing; emit the all-boxed code |
| `NX_CFLAGS=...` | Extra flags passed to `clang` (e.g. sanitizers) |
| `NX_PATH=...` | Extra module search directories |

Changing `NX_NOMEMO` or `NX_NOUNBOX` invalidates the build cache, so a
flag flip rebuilds instead of silently reusing a stale binary.

## Layout

- `compiler/nx-lexer/` — chars -> tokens (indent-sensitive)
- `compiler/nx-ast/` — AST
- `compiler/nx-parser/` — recursive descent
- `compiler/nx-types/` — static checker (`nx check`)
- `compiler/nx-interp/` — tree-walk interpreter (`nx file.nx`)
- `compiler/nx-codegen/` — LLVM IR backend (`nx build`)
- `compiler/nx-driver/` — `nx` CLI
- `editors/vscode-nexum/` — VS Code extension
- `wix/` — Windows MSI installer
- `examples/` — `.nx` samples
