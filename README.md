# Nexum (nx)

Python-simple syntax. Native compiler. LLVM backend.

```powershell
nx examples\hello.nx          # run (tree-walk interpreter)
nx check examples\hello.nx    # static type check
nx build examples\hello.nx    # native exe via LLVM (needs clang)
nx update                     # self-update exe + extension
nx setup --apply              # wire .nx icons into your VS Code theme
```

Prototype with the interpreter, ship with the compiler:

```
parallel:          # tasks run on threads when conflict-free
    a = fib(20)    # (deterministic: same output every run)
    b = fib(20)
```

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
