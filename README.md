# Nexum (nx)

Python-simple syntax. Native compiler. LLVM backend (planned).

## Step 1 — lexer only

```powershell
cargo test
cargo run -p nx-driver -- --lex examples\hello.nx
```

## Layout

- `compiler/nx-lexer/` — chars -> tokens
- `compiler/nx-driver/` — `nx` CLI
- `examples/` — `.nx` samples
