# Numeric representation audit

Scope: the whole NX compiler and the native runtime it emits. Every numeric
width was either narrowed, widened, deliberately kept, or classified as a
deferred opportunity, with a measurement or an explicit reason in each case.

Host: Intel Core 7 240H, 64-bit Windows, MSVC target.

---

## 1. Numeric-type inventory

The interesting numbers are not the ones in the language surface (`Int`,
`Float`) but the ones hidden in layouts, generated code and bookkeeping.
`Int` and `Float` are the source types; everything else is an internal
representation with its own justification.

### 1a. Runtime value representation (`runtime.ll`)

| Type | Definition | Size | Role | Verdict |
| --- | --- | --- | --- | --- |
| `%NxVal` | `{ i64 tag, i64 payload, i64 extra }` | 24 | Every NX value | KEEP — see §4.1 |
| `%NxVal.payload` | `i64` | 8 | `Int` bits, or `Float` as a bitcast double, or a heap pointer | KEEP |
| `%NxVal.tag` | `i64` | 8 | Discriminant, values 0–8 | KEEP — see §4.2 |
| `%NxVal.extra` | `i64` | 8 | Cached length / field count | KEEP — see §4.3 |
| `%NxList` | `{ ptr, i32 n, i32 cap }` | 16 | List header | **NARROWED** from `{ ptr, i64, i64 }` |
| `%NxDict` | `{ ptr, i32 n, i32 cap }` | 16 | Dict header | **NARROWED** from `{ ptr, i64, i64 }` |
| `%NxList.n` / `.cap` | `i32` | 4 | Element count, capacity | **NARROWED**, widened on load with `zext` |
| `%NxDictEntry` | `{ %NxVal, %NxVal }` | 48 | One key/value pair | KEEP — follows `%NxVal` |
| `%NxRec` | `{ ptr, i64 nfields, ptr desc }` | 24 | Record header | KEEP — see §4.4 |
| `%NxRec.nfields` | `i64` | 8 | Arity | KEEP — padding trap |
| `%NxDesc` | `{ ptr, i64, i64, ptr }` | 32 | Type descriptor | KEEP — padding trap |
| `%NxRecName` | `{ i64 len, ptr bytes }` | 16 | Name in a descriptor | KEEP |
| `%NxPool` | `{ i64 total, i64 done, ptr fn }` | 24 | One parallel batch | SPECIALIZE (deferred) — see §6.2 |
| `%NxMemoSlot` | `{ i64, i64, [8 x %NxVal], %NxVal, i1 }` | 240 | One memo cache entry | KEEP — see §4.5 |
| `@nx_memo_table` | `[4096 x %NxMemoSlot]` | 960 KiB | Static BSS in every binary | BENCHMARK_FIRST — see §6.3 |
| `@nx_memo_count`, `@nx_memo_lock` | `i64` | — | Entries, spinlock | KEEP — atomic, see §4.6 |

### 1b. Compiler AST (`nx-ast`)

AST nodes are the highest-cardinality structure in the compiler: a
400-function program is tens of thousands of them, and every one carries a
position.

| Type | Before | After | Verdict |
| --- | --- | --- | --- |
| `Span` | `{ usize, usize }` = 16 | `{ u32, u32 }` = 8 | **NARROWED** |
| `Expr` | 64 | 56 | follows `Span` |
| `Stmt` | 192 | 168 | follows `Span` |
| `Target` | 32 | 32 | unchanged, carries no position |
| `Field` | 48 | 48 | two `String`s, unchanged |
| `Expr::Int` payload | `i64` | `i64` | KEEP — this *is* the language's `Int` |
| `Expr::Float` payload | `f64` | `f64` | KEEP — narrowing to `f32` changes every result |

### 1c. Diagnostics and error types (five crates)

`LexError`, `ParseError`, `CheckError`, `RuntimeError`, `CodegenError` and
the interpreter's internal call frames all carried `line: usize, col:
usize`. These are 16 bytes each, on objects that exist only to report a
problem.

| Field | Before | After | Verdict |
| --- | --- | --- | --- |
| `line` | `usize` | `nx_lexer::LineNo` = `u32` | **NARROWED** |
| `col` | `usize` | `nx_lexer::ColNo` = `u32` | **NARROWED** |

### 1d. Indices, counters and offsets (deliberately untouched)

| Location | Field | Type | Verdict |
| --- | --- | --- | --- |
| `Lexer.chars` | `pos` | `usize` | DO_NOT_CHANGE — indexes a `Vec`, compared to `len()` |
| `Parser` | `pos` | `usize` | DO_NOT_CHANGE — indexes a `Vec` |
| `Gen` | `tmp`, `label`, `strc` | `u64` | DO_NOT_CHANGE — see §4.7 |
| `Gen` | `AllocFrame.at` | `usize` | DO_NOT_CHANGE — byte offset into a `String`, used for slicing |
| `nx-ir` | `batches: Vec<Vec<usize>>` | `usize` | DO_NOT_CHANGE — task indices |
| `nx-types` | `arity`, `builtin_arity` | `usize` | DO_NOT_CHANGE — one field per function |

---

## 2. Before → after

### 2.1 Source positions: `usize` → `u32`

One type, `LineNo`/`ColNo`, now defined in `nx-lexer` (the crate where
positions are born) and re-exported from `nx-ast`, used by every downstream
crate. Every position field in the workspace uses it.

Rationale: a position is bounded by the file it describes. `u32` addresses
4 billion lines and 4 billion columns; an editor exhausts address space long
before that. Nothing does arithmetic on a position — they are read,
compared and printed — so there was no arithmetic to break.

Measured, x86-64:

```text
node      before   after   change
Span        16       8      -50.0%
Expr        64      56      -12.5%
Stmt       192     168      -12.5%
Target      32      32        0.0%
```

`Expr` and `Stmt` moved much less than `Span` did, because their widest
variants are set by pointers and lengths rather than by the position.
`Expr` is pinned by the comprehension variant (`Box` + `String` + two
`Box`es + `Option<Box>`), which no position narrowing can shrink.

End-to-end, `nx check` peak working set on a generated 117 KiB /
400-function program:

```text
before   12.4 MiB
after    11.3 MiB
change    -1.1 MiB  (-8.9%)
```

On the small examples the change is invisible — they are all process
baseline, with the CLI's own image at 3–4 MiB against a few hundred AST
nodes. The win scales with program size, which is why it needed the
generated program to see.

### 2.2 Container headers: `i64` → `i32`

`%NxList` and `%NxDict` are `{ ptr, n, cap }`. Both counters are element
counts, not byte sizes — the byte size is computed at the allocation site.
An element is at least 16 bytes, so reaching 2^31 of them needs 32 GiB of
live data behind a runtime with no garbage collector. That is not a
reachable state, so 32 bits is not a semantic limit.

Both counters now occupy one 8-byte slot instead of two:

```text
header       before   after   change
%NxList         24      16      -33.3%
%NxDict         24      16      -33.3%
```

**The allocation had to move with the struct.** The first attempt narrowed
the type and left `malloc(24)` alone, which saves nothing — the 8 bytes
simply become tail padding. `nx_new_list` and `nx_new_dict` now allocate
16. This is pinned by a test, because it is exactly the kind of half-change
that looks finished and is not.

Measured against the CRT rather than modelled, with 400,000 live
allocations:

```text
400,000 x malloc(24): total _msize = 9.16 MiB
400,000 x malloc(16): total _msize = 6.10 MiB
saving: 3.06 MiB, i.e. exactly 8 bytes per block
```

The UCRT does not round these into a shared size class, so the saving is
real rather than absorbed by the allocator.

Every load widens with `zext` and every store narrows with `trunc`, so no
index or length is ever *computed* in 32 bits. `nx_listpush` and
`nx_dictset` check for saturation before narrowing, so a count cannot
silently wrap:

```llvm
%ncapfits = icmp ule i64 %ncap, 2147483647
br i1 %ncapfits, label %realloc, label %toobig
toobig:
  call void @nx_panic(ptr @.msg.toomany)
```

The check sits on the growth path, not the store path, so a push costs one
32-bit store instead of one 64-bit store — the same instruction count,
writing half the bytes.

---

## 3. Intentionally `i64`

| Where | Why 64 bits is right |
| --- | --- |
| `Expr::Int`, `Value::Int`, `%NxVal.payload` | This is the language's `Int`. It is the one width the language actually promises, and narrowing it would change what programs can compute. |
| `Expr::Float`, `%NxVal.payload` for a float | An IEEE double, bit for bit. `f32` would halve a node and change every result. |
| `%NxVal.tag` | A dedicated discriminant, not packed into a payload. See §4.2. |
| `%NxVal.extra` | Caches the length so `len()` is a load, not a pointer chase. See §4.3. |
| All arithmetic in the runtime | Lengths, indices, hash mixing and byte sizes are computed in `i64` regardless of the width they are *stored* in. |
| `@nx_memo_lock` | An atomic under a `cmpxchg` spinlock. Narrowing it buys 8 bytes of BSS and costs a compare-and-swap on a contended word — a bad trade in the one place contention is the point. |
| `nx_memo` hash | FNV-1a is defined over 64-bit words; a 32-bit hash would collide more and is the cache's whole quality metric. |

---

## 4. Cases considered and rejected

### 4.1 `%NxVal` 24 → 16 bytes — the big one, deliberately deferred

The largest remaining opportunity. `extra` is, for every aggregate, a
*duplicate* of a field in the header the value already points at: list
length is `%NxList.n`, dict length is `%NxDict.n`, record arity is
`%NxRec.nfields`, string length is in the string header. Deleting the field
would take `%NxVal` from 24 to 16 bytes — 33% off every value in the
system.

```text
1,000,000 list elements   22.9 MiB -> 15.3 MiB
%NxDictEntry               48 B  -> 32 B
%NxMemoSlot               240 B  -> 184 B
```

**Not implemented.** It is 53 read/write sites in `runtime.ll` plus one in
the backend, in the core value representation of a language with no garbage
collector and no other bounds metadata. That is an architectural change
wanting its own stage and a sanitizer sweep, not an audit finding to apply
in passing. Recorded as the top entry in §6.1.

### 4.2 `%NxVal.tag` → `i8` or `i16`

Eight distinct tags need one byte. But `tag` is the *first* field of a
struct that is passed by value through every call, return, list store and
dict store in the runtime. Narrowing it moves it into padding, and every
`extractvalue %NxVal %v, 0` becomes a sign/zero extension instead of a
move. Measured with the size probe: `{ i8, i64, i64 }` is 24 bytes, the
same as today, plus an extension on every tag check. Strictly worse.

### 4.3 `%NxVal.extra` → `i32`

`{ i64, i64, i32 }` is 20 bytes, padded to 24. No change. This is the
padding trap the audit brief warns about, and it is the reason the
container headers *did* shrink: `{ ptr, i32, i32 }` puts the two narrow
fields in one slot, while `{ i64, i64, i32 }` cannot.

### 4.4 `%NxRec.nfields` and `%NxDesc` → `i32`

`{ ptr, i64, ptr }` is 24 bytes; `{ ptr, ptr, i32 }` is also 24 — the `i32`
lands in tail padding. `{ ptr, i64, i64, ptr }` is 32; reordered as
`{ ptr, ptr, i32, i32 }` it would be 24, a genuine 8 bytes. Not taken:
there is one descriptor per *type*, so a program with a hundred types saves
800 bytes, against the churn of reordering a struct the backend indexes by
field number.

### 4.5 `%NxMemoSlot` — the `used` flag

`{ ..., i1 used }` puts a 1-byte flag in the struct's tail padding. Dropping
it gives 232 from 240 — a 3% saving on a table that is mostly its eight
`%NxVal` argument slots. Rejected as not worth the change.

### 4.6 Narrowing the memo hash

Rejected: see §3.

### 4.7 `Gen.tmp`, `Gen.label`, `Gen.strc`

`u64` → `u32` would be safe — 4 billion registers is unreachable — and
worthless. They are three fields in one struct, not per-node data. There is
no memory to save and the increment cost is identical.

### 4.8 Boxing `Expr::Comprehension`'s `var`

Would take `Expr` from 56 to 48 bytes. Rejected: it trades a heap
allocation on every comprehension for 8 bytes on a node that is itself one
short-lived allocation. Fewer, denser allocations wins, and "smallest field"
is not the goal. Pinned by a test so the decision survives.

### 4.9 `f64` → `f32` anywhere

No. It changes results, which is a language semantics change, not an
internal optimization.

---

## 5. Source types versus internal representations

The layers stay distinct, which is what makes the audit tractable:

```text
NX source type            Int, Float, Str, Bool, None, list, dict, record
  -> type system           Ty::Int / Ty::Float / ... (nx-types)
  -> AST                  Expr::Int(i64, Span)         64-bit payload
  -> optimizer             NV::raw(Ty::Int, reg)        bare i64, no box
  -> runtime               %NxVal { i64, i64, i64 }     tagged, boxed
  -> machine               mov / add / imul             raw i64
```

`Int` is 64 bits at every layer because the language says so. `Float` is
`f64` at every layer for the same reason. A source position exists only in
the AST and diagnostic layers, which is why it could be 32 bits without
touching the language.

---

## 6. Remaining opportunities

### 6.1 Delete `%NxVal.extra` (highest value)

24 → 16 bytes on every value, every dict entry, every memo slot argument
and every call argument array. 53 sites in `runtime.ll`, one in the
backend. Blocked on: a dedicated stage with an ASan/UBSan sweep of the
whole runtime, since a mistake here is silent memory corruption rather
than a wrong answer. The two-path differential (native, then native with
`NX_NOUNBOX=1`) would catch functional breakage but not an out-of-bounds
read.

### 6.2 `%NxPool` → `{ i32 total, i32 done, ptr fn }`

24 → 16 bytes. Genuine win, trivially safe (two counters that share a
slot). Deferred because there is exactly one pool alive at a time, so it
saves 8 bytes per program.

### 6.3 The memo table is 960 KiB of static BSS in every binary

4096 × 240 bytes, unconditionally, even for `hello world`. It is
zero-initialised and therefore lazily faulted, so the resident cost is
whatever is touched — but the image is bigger than it needs to be, and a
`[8 x %NxVal]` argument window is generous. Halving the window to 4 saves
40% of the table at the cost of memoizing only functions of arity ≤ 4.
Needs a benchmark across real workloads to decide, since memoization is
currently worth several orders of magnitude on `fib`.

### 6.4 Constant-range analysis for unboxing

`nx-types` already unboxes to `i64`/`double`/`i1` on a statically known
type. Constant-range propagation could additionally prove that a value
fits in `i32` and unbox to `i32`, halving the register pressure and the
slot width on stack-heavy numeric code. Worth real investigation, and the
place the next performance stage should look.

### 6.5 Flat scalar arrays

Already on the roadmap as part of the performance stage: a `List(Int)`
currently stores 24 bytes per element, of which 8 are payload and 16 are
tag and bookkeeping. A dedicated representation for homogeneous scalar
lists is a much larger memory win than anything in this audit — 24 bytes
to 8 for the common case — but it is a new container kind with its own
clone, equality, iteration and unboxing paths, not a numeric tweak.

---

## 7. Validation

| Gate | Result |
| --- | --- |
| `cargo test --workspace` | 307 pass, 0 fail |
| `tools/verify.ps1` — 13 examples × interpreter / native / `NX_NOUNBOX` | all agree |
| `bench/run.ps1` — 5 benchmarks vs Rust | all MATCH |
| LLVM type checking of every touched field | clean — a missed `load`/`store` width is a compile error, not a silent bug |
| Structure-size assertions | new tests in `nx-ast` and `nx-codegen` pin every width above |

### Benchmark A/B, same session

Both arms rebuilt from scratch and measured with the same harness
(median of 5):

```text
bench      before    after    delta
fib         0.007s   0.007s     --
mandel      1.143s   1.183s   +3.5%
matmul      0.117s   0.123s   +5.1%
listsum     0.086s   0.080s   -7.0%
dot         0.074s   0.073s   -1.4%
```

Mixed signs across five benchmarks, two of them faster. That is noise, not
a regression — and the hot path inspection agrees: the store path gained no
instructions, and the only added branch is on container growth. The
per-benchmark run-to-run spread observed across sessions (matmul
0.107–0.123 s) is wider than any of these deltas.

### What could not be measured

The container-header saving was **not** visible in NX program peak working
set. On an identical binary, three consecutive runs of the same program
measured 76–97 MiB, a ±12% spread, against a 3.06 MiB expected saving.
The saving is therefore reported from `_msize` (exact, allocator-reported)
and from the struct layout, not from resident memory. A meaningful
resident-memory measurement needs a quieter host or a deterministic
allocation counter, and is listed as evidence still needed.
