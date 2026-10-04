# Nexum (nx)

Python-simple syntax. Native compiler. LLVM backend.

```powershell
nx run examples\hello.nx      # build if stale, then run native
nx check examples\hello.nx    # static type check
nx build examples\hello.nx    # native exe via LLVM (needs clang)
nx build examples\hello.nx --emit-ir   # print the generated LLVM IR
nx dump-ir examples\hello.nx  # effect summaries (reads/writes/prints)
nx update                     # self-update exe + extension
nx setup --apply              # wire .nx icons into your VS Code theme
```

Write it once, the compiler ships the machine:

```nexum
fn collatz(n):
    steps = 0
    while n != 1:
        if n % 2 == 0:
            n = n / 2
        else:
            n = 3 * n + 1
        steps = steps + 1
    return steps

print(collatz(27))     # 111
```

`collatz` is pure, so it is memoized automatically, and `n` is a statically
known `Int`, so it is unboxed into a bare register and the arithmetic is raw.
Neither is written in the source: both are consequences of what the code
means.

## One execution model

Nexum compiles ahead-of-time. Every program takes one path:

```
source -> lexer -> parser -> type check -> effect summary + memory plan
       -> LLVM IR -> clang -O2 -> native executable
```

`nx <file.nx>` and `nx run <file.nx>` both end at that executable, and
with no `-o` the executable is written next to its `.nx` source file.
There is no second way to execute NX: the tree-walking interpreter was deleted
rather than kept alongside the compiler, so every rule in `docs/grammar.md`
has exactly one implementation behind it. There is also no concurrency --
see `Still open` in `CHANGELOG.md` for why `parallel:` was removed rather than
fixed.

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
- **Correct or loud, never wrapped.** Integer overflow traps rather than
  wrapping, `**` saturates instead of silently returning a wrong power, and
  a string is addressed by character rather than by byte. `3 * n + 1` either
  produces the right answer or stops the program; it never quietly does not.
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
- `compiler/nx-ir/` — effect summaries (reads/writes/prints per function)
- `compiler/nx-mem/` — memory planning (Unique/Shared per local)
- `compiler/nx-codegen/` — LLVM IR backend (`nx build`)
- `compiler/nx-driver/` — `nx` CLI
- `compiler/nx-e2e/` — compile-and-run tests against expected values
- `editors/vscode-nexum/` — VS Code extension
- `wix/` — Windows MSI installer
- `tools/verify.ps1` — every example, two ways, diffed
- `tools/argone-gate.ps1` — decides whether the ARGONE stage is complete
- `bench/run.ps1` — Nexum vs Rust, correctness-gated
- `bench/compiler/` — where the compiler's own time goes
- `examples/` — `.nx` samples

## Types and behavior

`type` is state, `impl` is behavior. No inheritance: a type's fields are
its own, and behavior is attached to it explicitly.

```
type Point:
    x: Float
    y: Float

impl Point:
    fn area(self):
        return self.x * self.y

    # `mut self` writes its result back into the receiver:
    # p.moved(1, 2) means p = moved(p, 1, 2)
    fn moved(mut self, dx, dy):
        self.x = self.x + dx
        self.y = self.y + dy
        return self

    fn origin():                 # no receiver: an associated function
        return Point(0.0, 0.0)

p = Point(3.0, 4.0)
p.area()                        # 12.0
p.moved(1.0, -2.0)              # p is now Point(4, 2)
Point.origin()                  # Point(0, 0)
```

Containers have value semantics: they copy on bind, so no assignment or
argument passing aliases. A slice copies its elements too, so a list of lists
does not write through to its parent.

Strings are shared and never mutated in place, and they are addressed by
character rather than by byte:

```nexum
s = "日本語"
print(len(s))      # 3 characters, not 9 bytes
print(s[1])        # 本
print(s[::2])      # 日語
```

Write non-ASCII text with escapes so the source stays ASCII -- a raw
multi-byte character is decoded differently by a diff, by a terminal with the
wrong code page, and by a patch applied as bytes, and none of them reports an
error when they disagree:

```nexum
s = "\u65e5\u672c\u8a9e"      # the same three characters
```

`"\uXXXX"`, `"\UXXXXXXXX"` and `"\u{...}"` are all accepted, spelled the way
Python spells them.

## What is not done yet

Stated plainly, because a README that only lists strengths is not a
specification. `CHANGELOG.md` carries the full list with detail.

- Container binds deep-copy, so container-heavy code runs 3x to 22x slower
  than Rust on the measured workloads. Copy-on-write would close most of it
  and needs reference counting.
- `len` on a string is `O(n)` and `s[i]` is `O(i)`, because a character count
  is not cached beside the byte length.
- Memory is malloc-ed and lives for the process lifetime. There is no
  collector and no arena yet, so a program that churns grows without bound.
- No concurrency. `parallel:` was removed rather than repaired; the reasoning
  is in `CHANGELOG.md`.

## Architecture stage

`docs/grammar.md` is the language specification. `docs/numeric-audit.md`
records the numeric representation audit.

**ARGONE is in progress and is a hard gate.** Stage 4 through Stage 8 stay
locked until it completes.

| Document | What it is |
| --- | --- |
| `docs/ARGONE.md` | The task breakdown, one gate per task |
| `docs/ARGONE-STATUS.md` | The completion checklist — this file *is* the gate |
| `tools/argone-gate.ps1` | Decides whether the stage is complete |
| `tools/argone-gate-tests.ps1` | Tries to fool the gate, and fails if it can |

```powershell
pwsh -File tools\argone-gate.ps1            # where the stage stands
pwsh -File tools\argone-gate.ps1 -Enforce   # exit 1 unless complete
pwsh -File tools\argone-gate-tests.ps1      # the gate's own tests
```

A task is `not started` or `complete`. There is no third state, so a stage
cannot be *mostly* done and have the gate open anyway.
