# The standard library, written in Nexum

**Question.** What belongs in the runtime and what belongs in a standard library
written in Nexum; how such a library is distributed, found and imported; which
modules it needs first and what is in them; and how the map, the sorting and the
interning problems are solved without language features that do not exist yet.

**Summary.** The runtime keeps the machine: the allocator, the collector, the
root frames, the system calls, the panic, the container primitives it has to
trace, and the byte-level primitives the code generator itself calls. The
standard library gets everything else - strings, collections, maths, files,
formatting - written in Nexum and compiled together with the program that uses
it. Distribution is a **library search path** added to the existing `import`
resolver: no grammar change, one new resolution rule, and a reserved `std/`
prefix so a user file cannot shadow the library. The first module is `str`,
specified below down to its function signatures. The map is an **open-addressed
hash table with linear probing in a single flat list**, whose value type is
fixed to `Int` so that one implementation serves every use (values live behind
handles); it needs a byte-to-integer conversion that the language does not have
yet. Sorting is a comparison sort in the library, one instantiation per
comparable element type, because the type inference is monomorphic - a library
function has exactly one instantiation per program, which is the sharpest
constraint on the whole design. Interning is the map plus a parallel list of
canonical strings, threaded explicitly, because the language has no globals.
The library is tested by the fixture and differential machinery that already
exists; its own unit tests are ordinary `nx` programs. Twelve things are
blocked on language features that do not exist, and the two that matter most
are the missing byte-to-integer conversion and the monomorphic inference.

---

## Recommendation

1. **The boundary is three-way, not two-way.** The **runtime** owns the
   machine and the memory model: the allocator, the collector, the root frames,
   the safepoints, the system calls, the panic, the container primitives the
   collector has to trace (`List`), the byte-level string primitives the code
   generator itself calls, and the output routines that write bytes to a
   descriptor. The **standard library** owns the algorithms and the policy:
   splitting, joining, trimming, searching, building, sorting, hashing, maps,
   sets, number formatting, parsing, path handling, file *policy* (lines,
   encodings), and the composed maths. The **grey zone** - a primitive the
   algorithms cannot be written without - goes into the runtime as a builtin.
   Today that grey zone is exactly two kinds of thing: a byte's numeric value
   (`ord` and `chr`), and the correctly-rounded decimal conversion of a
   `Float`. Everything else the library needs, it can write itself.

   *Why:* the runtime exists to make the program own the machine; the library
   exists so that the language has a library rather than a runtime with
   everything bolted into it. A rule that puts "the things the collector and
   the program model need" in the runtime and "everything a program would
   want" in the library is the right rule, but it needs one more clause, because
   a library written in a language without bit access cannot implement a hash
   function, and a library written in a language without `Float`->`Int`
   conversion cannot format a float.

2. **The standard library is source, not an artifact.** It is a set of `.nx`
   files that are compiled together with the program that imports them, by the
   same compiler, in the same pass. There is no prebuilt library, no `.a`, no
   separate compilation, no ABI to keep stable between the library and the
   compiler. That is what "written in Nexum" has to mean if the toolchain is
   to self-host (phase 11j): the compiler must be able to compile its own
   library.

   *Cost, stated honestly:* the whole imported library is in the program. The
   MIR's dead-code elimination is per function (`crates/nexum-mir/src/lib.rs:926`),
   not per module, so an imported but unused library function is still compiled
   in. The mitigation is to keep the modules small and to let a program import
   only the modules it needs.

3. **Distribution is a library search path in the import resolver.** Today
   `import "path.nx"` is resolved as `base.join(rel)`, relative to the
   importing file (`crates/nexum-parser/src/lib.rs:251`). The resolver gains a
   list of *library roots* and tries each of them. The first root is
   `$NEXUM_STD` (a path list); the rest are derived from where the `nx`
   executable lives.

   *Why here and not in the language:* the import syntax already accepts a
   path-shaped string, and the resolver is where the decision belongs. **No
   grammar change is needed** for the standard library to exist.

4. **`std/` is a reserved path prefix.** An import whose path begins with
   `std/` is resolved *only* against the library roots, never relative to the
   importing file, so a program cannot accidentally shadow the library with a
   local `std/str.nx`. Every other path keeps today's behaviour (relative to
   the importing file first, then the library roots).

5. **Every library name is prefixed with its module** - `str_split`, not
   `split`; `text_format_int`, not `format_int`; `map_new`, not `new`. The
   import mechanism flattens every imported file into **one namespace**
   (`docs/language-reference/decisions.md:37-39`), and a duplicate name across
   two files in one program is a hard error (T014 - verified, see *What the
   code looks like today*). With a flat namespace there is no way to write
   `std.str.split`, so the prefix is the only way to keep a library and a user
   program in the same namespace. The qualified form is the right long-term
   shape and is listed as a language change, but the prefix is what makes the
   library shippable now.

6. **The first module is `str`**, specified in full under *The design*. It is
   first because every other module needs it, because the self-hosted lexer
   needs it (stage 10b of `docs/self-hosting.md`), and because it is the module
   that can be written today with nothing but the builtins that already exist.

7. **The map is an open-addressed hash table with linear probing, in a single
   flat `List`, with the value type fixed to `Int`.** The map is `Str -> Int`;
   anything else is stored behind the integer handle in a list the caller owns.
   This is the "arena of nodes with indices" shape the language already
   prefers for the self-hosted AST (`docs/language-reference/decisions.md:112-126`,
   `docs/self-hosting.md:84-86`). It needs no list of lists, no `List` inside a
   record, and no insert-at-index operation - only `list_get` and `list_set`.
   *Verified working in Nexum today*, with the hash function faked, because
   the real one needs `ord`.

8. **Sorting is a comparison sort written in the library** (quicksort or
   mergesort over `list_get`/`list_set`), **one instantiation per comparable
   element type**, because the inference is monomorphic. A `sort` that works
   for `Int` and one that works for `Float` are two functions. A single
   *generic* sort over a list of `(rank, handle)` pairs is provided as the
   escape hatch, so a caller with any element type can still sort by
   computing integer ranks.

9. **Interning is the map plus a parallel list of canonical strings**, with
   the table threaded explicitly through the caller. The language has no
   globals: a top-level `let` parses but is dropped by the front end
   (verified - T004 on use). A library with hidden state does not exist in
   Nexum yet, so the standard library is **stateless**: every function takes
   its state as a parameter.

10. **Two new runtime primitives are the price of admission: `ord(s: Str) ->
    Int` and `chr(n: Int) -> Str`.** They are the byte-level equivalents of
    `len` and `slice`, they belong beside them in the runtime, and without
    them the library cannot hash a string, cannot order two strings, cannot
    read a digit's value, and cannot sort strings. Every one of those is
    currently blocked.

11. **One small language change unlocks the container story: make `List` and
    `List[T]` usable type annotations.** Today `List` is not a valid type
    expression at all: a record field of type `List` is a hard T024 (verified).
    A record that holds a list is the natural shape for a map handle, a
    reader and a builder. The change is in the grammar, the parser's type
    expression and the semantic analyser's type resolution; the code generator
    already lowers `Ty::List` to a pointer with an 8-byte scan (`crates/nexum-codegen/src/lib.rs:15`,
    `:25`, `:90`), so the back end needs nothing.

12. **The library is tested by the machinery that already exists.** Its unit
    tests are ordinary `.nx` programs under `lib/nexum/std/tests/`, run by
    `nx test`, which executes each file and checks the exit status - no
    harness change at all. Its behaviour is pinned by fixture files with
    expected stdout, in the same shape as `crates/nexum-driver/tests/fixtures/run/`.
    Where a reference exists, the library is tested **differentially** against
    it - the same trick as `selfhosted_lexer_matches_rust_lexer`
    (`crates/nexum-driver/tests/backend.rs:143-194`): the Rust implementation
    of integer formatting, say, against the Nexum one, byte for byte.

13. **The bootstrap order for the self-hosting plan (phase 11j) is: `str`,
    then `text`, then `collections` (map, set, sort), then `fs` and `os`, and
    `math` whenever 11c lands.** The compiler needs `str`, `text`,
    `collections` and `fs`; it never needs `math`. The dependency the
    compiler imposes is `collections`, because stage 10d (name resolution and
    type checking) needs hash maps (`docs/self-hosting.md:41`).

14. **Twelve things in the standard library are blocked on language features
    that do not exist yet.** They are listed under *The design* with the
    smallest change that would unblock each. The two that gate everything
    else are the missing byte-to-integer conversion (item 10) and the
    monomorphic inference (item 8's caveat).

---

## What the code looks like today

### The "standard library" today is the builtin set

`docs/standard-library.md:3-5` says so outright: "The bootstrap standard
library is the set of **builtins** available in every program without imports.
There is no `import` of library modules yet (see D002); builtins are resolved
by name." The file lists twenty builtins in two tables
(`docs/standard-library.md:9-35`), states the current module rule
(`:43-45`), and lists what is planned but absent: `io`, `math`, `collections`,
`str` (`:47-55`).

Builtins are not functions: they have no source definition, they are lowered
directly by the code generator, and their names are reserved
(`docs/standard-library.md:57-61`). The lowering is a name substitution in
`crates/nexum-codegen/src/lib.rs:836-847` (`concat` -> `nexum_concat`,
`list_new` -> `nexum_list_new`, and so on), and the type checking of each one
is hand-written in the semantic analyser
(`crates/nexum-sema/src/lib.rs:1260-1500`). There is no mechanism by which a
*user-written* function could be given a reserved name or a special lowering.

### What the runtime actually holds

`crates/nexum-runtime/src/lib.rs:1-42` is the inventory, and it is a short
one: the panic primitive, the managed heap with the mark-sweep collector, the
root-frame registration, the string helpers, the file I/O helpers, and the
growable `List`. In the emitted IR:

| Piece | Where | Note |
| ----- | ----- | ---- |
| the libc declarations still in use | `lib.rs:49-54` | `printf`, `fputs`, `fputc`, `exit`, `malloc`, `free` |
| the panic | `lib.rs:115-126` | message + newline to stderr, `exit(1)` |
| the out-of-memory handler | `lib.rs:129-131` | |
| the root frames | `lib.rs:136-139` | push/pop a shadow stack |
| the allocator | `lib.rs:146-151` | `nexum_alloc`, `nexum_alloc_scanned`; still `malloc` underneath |
| the watermark and the collector | `lib.rs:154-187` | mark, scan, trace to a fixpoint, sweep |
| `concat`, `str_char_at`, `slice` | `lib.rs:190-202` | they allocate on the managed heap |
| the file routines | `lib.rs:204-207` + `strings.rs:33-55` | whole-file read and write |
| the growable `List` | `lib.rs:208-231` | a heap object plus a separate element buffer |
| `strlen`, `memcpy`, `strcmp` | `strings.rs:11-27` | ours, on every platform |

`docs/runtime-abi.md:11-32` is the freestanding statement and the table of what
still comes from the C library: `malloc`/`free` and `printf`/`fputs`/`fputc`
and `exit` are phase 11a.2; the file routines on Windows are phase 11a.2
because Windows has no direct system call instruction.

### The import mechanism

It exists and it is small. `crates/nexum-parser/src/lib.rs:188-202` is
`load_module`: it loads a source file and everything it imports into one
`SourceMap` and returns one flattened module. `load_file` (`:204-271`)
canonicalises each path so that `a.nx` and `./a.nx` are the same file and so
that a cycle terminates (`:213-216`), reads and lexes and parses the file, and
then walks the top-level statements; an `Import` statement resolves its
target as `base.join(rel)` where `base` is the importing file's directory
(`:247-252`), and a missing file is T047 (`:254-262`). Everything else is
appended to one flat statement list (`:268`).

The rules that follow from this, and that the standard library has to live
with:

* **One namespace.** Every imported declaration shares the importing file's
  namespace (`docs/language-reference/decisions.md:37-39`). There is no
  per-module visibility, no `pub`, no qualified name.
* **Loaded once.** Diamond imports and import cycles are safe by
  construction, not by a check.
* **Top level only.** `import` inside a function is P015
  (`crates/nexum-parser/src/lib.rs:806-814`).
* **Relative to the importing file.** There is no search path, and no way to
  name a file that is not next to the importer.

The one program in the repository that uses it is the self-hosted lexer:
`examples/selfhost/lexer.nx:8` is `import "lexer_lib.nx"`.

### What the language can and cannot express today

These are the facts the standard library has to be built out of. Every one of
them was checked against the compiler in this checkout, not read off a
document.

| Fact | Where | Consequence for the library |
| ---- | ----- | --------------------------- |
| `Str` and `List` are the only managed types | `decisions.md:128-150` | the library's data structures are built out of those two |
| `List`'s element type is fixed by its first `list_push` | `decisions.md:152-166`, `sema:1399-1419` | a list is monomorphic |
| `List` is **not** a valid type annotation | `grammar.md:66-68`, `sema:537-555` | a record cannot hold a list (T024) |
| an unknown type in a *parameter* annotation is silently ignored | `sema:288-302` (no T024 for parameters; T024 is only for record fields, `sema:247-254`) | `fn f(xs: List)` compiles and means "untyped" |
| the inference is **monomorphic per function** | `sema:314-403` (a fixpoint over one signature table) | one library function = one instantiation per program |
| a function is not a value | `sema:912-914` | no `map(f, xs)`, no callbacks, no "sort by key" |
| `Str` has no ordering | `grammar.md:139-143` | strings cannot be sorted or binary-searched |
| there is no byte-to-integer conversion | no `ord`/`chr` anywhere in the builtins | no hashing, no digit values, no char classification by value |
| for-each needs an array | `grammar.md:145-148`, `sema:793-811` | a list is iterated with an index loop |
| `and`/`or` do not short-circuit | `docs/self-hosting.md:78-79` | every index is guarded by an explicit test |
| there are no globals | `sema:418-432` (`HirModule` has only records and functions) | library state is threaded explicitly |
| no `Option`, no `Result` | `decisions.md:41-57` | "not found" is a sentinel or a default argument |
| no tuples, no variadics | `grammar.md:43-104` | a format API is a builder, not `format(fmt, ...) |
| binary I/O is impossible | `runtime-abi.md:232-233` (a NUL truncates) | the file API is text-only |
| `print` uses C `%g` for floats | `codegen:716-755`, `grammar.md:305-306` | float formatting is a runtime job (11a.2) |

#### The claims in this document that were checked against the compiler

Every claim above that says "verified" was checked by compiling and running a
small program with the bootstrap compiler in this checkout
(`target/debug/nx.exe`, the Windows host build). The programs were written to
a scratch directory outside the repository; **no repository file was changed**.
The checks, and what they produced:

| Claim | Check | Result |
| ----- | ----- | ------ |
| the seeding trick fixes a list's element type and the type escapes through the inferred return type | a function that returns `list_new()` after `list_push(out, "")`, then `list_get` on the result at the call site | **works** - the caller sees `List[Str]`, `list_get` returns the element |
| a library function can be monomorphic per program | `fn rev(xs: List) -> List` called with a `List[Int]` and then a `List[Str]` | **`error[T007]: argument 1 of `rev` expects List[Int], found List[Str]`** |
| `List` is not a valid field type | `type Holder: items: List` | **`error[T024]: unknown type `List` for field `items``** |
| an unknown *parameter* type is silently ignored | `fn f(xs: Bogus) -> Int` | **compiles cleanly** - no diagnostic at all |
| you cannot push into a list obtained from `list_get` | `list_push(list_get(outer, 0), 1)` | **`error[T049]: cannot infer the element type of this list`** |
| a list of lists *is* usable through a local binding | `let inner = list_get(outer, 0)` then `list_push(inner, 1)`, twice | **works** |
| for-each needs an array | `for x in l:` over a `List` | **`error[T038]: for-each requires an array, found List[Int]`** |
| `Str` has no ordering | `if "a" < "b":` | **`error[T008]: ordering comparisons require numerics, found Str`** |
| a function is not a value | `let f = print` | **`error[T004]: undefined variable `print``** |
| there are no globals | a top-level `let mut counter = 0`, then `print(counter)` | **`error[T004]: undefined variable `counter``** - the top-level `let` parses and is then dropped |
| the import namespace is flat | two files defining the same function, one importing the other | **`error[T014]: duplicate definition of function `same_name``** |
| an import path may have a directory component | `import "sub/lib.nx"` | **works** |
| the builder pattern works | `builder_new()` returning a seeded list, `builder_add(mut b, s)` pushing, `builder_get(b)` reading | **works** - the output is the concatenation |
| **the open-addressed hash table works in Nexum today** | the exact `MapSlot{key, value, state}` table and the exact `map_hash` shape from *The design*, in one `List[MapSlot]`, with a header slot for the count, linear probing, `list_get`/`list_set` only (the byte value faked, since `ord` does not exist - which makes every key hash to the same bucket, so the probe chain is exercised hard) | **works** - insert, replace, hit, miss, the `O(1)` count and the capacity all behave correctly |
| **a comparison sort works in Nexum today** | a bubble sort over a `List[Int]` using `list_get`/`list_set` | **works** - the list came out sorted |
| `len` on a `Str` is the byte length, and `s[i]` is one byte | `len(concat("ab","cd"))` and `s[1]` | **works** - `4` and `b` |

The two that matter most for the recommendation are the last two: the map and
the sort are **not** blocked on the language. They are blocked on one missing
primitive, `ord`, and everything else follows from that.

### What the self-hosted lexer already shows the library is needed for

`examples/selfhost/lexer_lib.nx` is the only substantial Nexum program in the
repository, and it is a catalogue of what the library would remove:

* `char_in` (`:16-23`) scans a string of candidate characters byte by byte for
  every input byte - a linear search per character, with no byte values.
* `keyword_kind` (`:37-70`) and `two_char_kind` (`:72-96`) are hand-written
  chains of `if` comparisons - **a map, written as a chain, because there is no
  map**.
* The string-literal scanner (`:219-252`) builds the token text with
  `text = concat(text, ch)` inside the loop - quadratic in the length of the
  literal, one heap allocation per byte.
* The indent stack (`:179-193`) is managed by hand because a list cannot be
  popped.

And `docs/self-hosting.md:76-89` records the four language-level lessons the
exercise produced, all of which are constraints on the library.

### The test machinery

| Piece | Where | What it does |
| ----- | ----- | ------------ |
| `nx test <dir>` | `driver/src/main.rs:858-897` | runs each `.nx` in the directory and checks the exit status |
| run fixtures | `tests/backend.rs:94-138` | compiles, links, runs, and compares stdout with a `.out` file; also checks the exit code against a `.exit` file |
| pass / fail fixtures | `tests/fixtures.rs:14-93` | clean compile, or the expected diagnostic codes with a source span |
| the lexer differential | `tests/backend.rs:143-194` | the self-hosted lexer must produce the Rust lexer's token stream exactly |
| the parser differential | `tests/backend.rs:199-249` | the self-hosted parser must produce the Rust parser's normalised form exactly |
| the optimiser differential | `tests/backend.rs:251-316` | opt level 0 and 1 must agree on the output and the exit code |
| the fixture scanner | `tests/common/mod.rs:89-96` | **not recursive** - one directory, `.nx` files only |
| CI | `.github/workflows/ci.yml` | `cargo test --workspace`; `nx test` is *not* run |

---

## The design

### 1. The boundary, in detail

#### What is in the runtime, and why each thing is there

| Runtime piece | Why it cannot be in the library |
| -------------- | -------------------------------- |
| the allocator | the collector's backing store; the library cannot free |
| the collector, the mark/scan/trace/sweep loop | it *is* the memory model |
| the root frames, the safepoints | the compiler and the runtime agree on where a reference lives |
| the system calls | the kernel interface; phase 11a makes them inline assembly (D013) |
| the panic and the OOM handler | the failure mode of the program model (D003) |
| `List` (`nexum_list_*`) | the element buffer is a separate heap object so growth does not move the handle, and the collector must know the buffer's scan size (`runtime-abi.md:190-203`); the compiler must materialise an element into a stack temporary before the copy (`codegen:756-835`) |
| `concat`, `slice`, `str_char_at` | they allocate on the managed heap, so they must use `nexum_alloc` |
| `strlen`, `memcpy`, `strcmp` | the code generator calls them for struct copies and string handling |
| the file routines | they call the allocator and the system calls |
| the output routines (11a.2) | they write bytes to a descriptor |
| the numeric tower's float primitives and transcendentals (11c) | they need the bits of a `Float`, and the language has no way to look at them |
| **`ord` and `chr` (new)** | a byte's numeric value; the language can read a byte only as a one-byte `Str`, which it cannot compare or convert |

#### What is in the library

| Module | Contents |
| ------ | -------- |
| `str` | splitting, joining, trimming, searching, building, classification, comparison |
| `text` | number formatting (integer, float, hex, binary), padding, a format builder |
| `collections` | the map, the set, sorting, searching, reversing, slicing |
| `math` | the composed maths: degrees and radians, `lerp`, `clamp`, `approx_eq`, `relu`, `softmax`, reductions |
| `fs` | whole-file reading and writing, line splitting, path manipulation, buffered *in-memory* readers and writers |
| `os` | arguments, environment, process spawning |
| `test` | the assertion helpers the library's own tests use |

#### The rule for the grey zone

> A primitive goes into the runtime when it cannot be written in Nexum without
> another primitive that has to know the representation.

Applied today, that rule puts exactly two kinds of thing in the runtime that a
naive reading would put in the library: the byte-value pair (`ord` and `chr`),
and the float formatter (correctly-rounded decimal conversion of an `f64`).

Applied to the file routines, it splits them in two: the *whole-file read and
write* stay in the runtime (they need the allocator and the system calls), and
the *policy* - reading a file into lines, joining paths, trimming a trailing
newline - is library code.

### 2. The import mechanism's requirements

The resolver changes from one rule to a small ordered set of rules.

#### The resolution algorithm

```
resolve(importing_file, rel):
    if rel starts with "std/":
        # a reserved prefix: library roots only, never the importing file
        for root in library_roots():
            cand = root / rel
            if cand exists: return cand
        fail("cannot import `rel`: not found in the standard library")
    else:
        cand = importing_file.dir / rel          # today's behaviour, first
        if cand exists: return cand
        for root in library_roots():
            cand = root / rel
            if cand exists: return cand
        fail("cannot import `rel`: file not found")
```

The library roots, in order:

1. **`$NEXUM_STD`** - a path list separated by `;` on Windows and `:` on
   POSIX. The escape hatch for a developer working on the library, and the
   hook CI uses to test an unreleased library.
2. **`<directory of the `nx` executable>/std`** - the portable layout, and
   the Windows layout, where the compiler and its library travel together.
3. **`<directory of the `nx` executable>/../share/nexum/std`** - the Debian
   layout, which today puts the extension in `/usr/share/nexum/`
   (`packaging/README.md:38-50`) and would put the library beside it.
4. **the workspace checkout's `lib/nexum`** - found by walking up from the
   executable's directory looking for `Cargo.toml` next to `crates/nexum-driver`.
   This is what lets the bootstrap compiler build against the checked-out
   library with no install step.

Every root is tried in order, and the first hit wins. The roots are *not*
searched for a non-`std/` path until the importing file's own directory has
failed, so today's behaviour is preserved exactly for existing programs.

#### What is required of the language, and what is not

* **No grammar change.** `import "std/str.nx"` is already a string literal in
  a top-level statement (`grammar.md:80-81`). The resolver decides where it
  comes from.
* **No change to the loader's de-duplication.** Canonicalisation and
  "load once" (`parser:213-216`) already give the library the same property:
  if two modules import `std/str.nx`, it is compiled once.
* **Path separators.** `std/str.nx` uses `/` on every platform. `Path::join`
  accepts `/` on Windows, and the resolver should normalise anyway.
* **Diagnostics must name the roots.** Today the message is "cannot import
  `x`: file not found" (`parser:254-262`). The new message must list the roots
  that were searched, or a broken install is undiagnosable.
* **`nx doctor` must print the resolved library root.** It already prints the
  toolchain (`driver/src/main.rs:899-926`); the library root belongs there.

#### The consequences of the flat namespace

The namespace is flat, so:

* Every library name is prefixed (`str_split`, `text_int`, `map_new`,
  `set_new`, `fs_read`, `math_lerp`).
* A user program that defines `str_split` and imports the library gets T014
  (verified). This is the price of the flat namespace, and it is why the
  prefix convention is not optional.
* The library cannot have two functions with the same name in different
  modules. `str_split` and `fs_split` are fine; `split` in both is not.

#### What `nx new` scaffolds

Today `new_project` (`driver/src/main.rs:215-249`) writes `nexum.toml`,
`src/main.nx`, `tests/smoke.nx` and `README.md`. The scaffold for a project
that uses the library:

```
myproject/
  nexum.toml
  src/main.nx
  tests/smoke.nx
  README.md
```

`src/main.nx`:

```
import "std/str.nx"

fn main():
    let name = "world"
    print(concat("hello, ", name))
    print(str_len(name))
```

`tests/smoke.nx`:

```
import "std/str.nx"

fn main():
    let parts = str_split("a,b,c", ",")
    if list_len(parts) != 3:
        panic("expected three parts")
    if str_join(parts, ",") != "a,b,c":
        panic("split and join do not round trip")
    print("ok")
```

The scaffold proves two things at once: that the library is *found* (the
import resolves) and that it *works* (the round trip holds). `nx new` should
also print the resolved library root, so that a broken install is obvious at
scaffold time rather than at first compile.

`nexum.toml` does not need a dependency section for the standard library - it
is not a dependency, it is part of the toolchain. It may record the library
version it was written against (`std = "0.1"`), and the resolver should warn
when the toolchain's library version differs. That is a nicety, not a
requirement.

### 3. The module layout on disk

In the repository:

```
lib/nexum/
  std/
    str.nx           # the first module
    text.nx          # formatting
    collections.nx   # map, set, sort   (split into map.nx/set.nx/sort.nx if it grows)
    math.nx
    fs.nx
    os.nx
    test.nx
  std/
    tests/
      str_test.nx
      text_test.nx
      collections_test.nx
      math_test.nx
      fs_test.nx
```

Installed:

| Platform | Location | Change needed |
| -------- | -------- | ------------- |
| Debian | `/usr/share/nexum/std/` | a new directory in the `.deb` (`packaging/linux/`) |
| Windows (MSI) | `%ProgramFiles%\Nexum\std\` | a new component in `packaging/windows/Package.wxs` |
| macOS (manual install) | `~/.local/share/nexum/std/` | `crates/nexum-driver/src/setup.rs:166-179` currently copies only the binary |

The import graph is small and acyclic: `text`, `collections` and `fs` may
import `str`; `math` and `os` import nothing.

### 4. The first module: `str`

#### The signature list

Every function operates on **bytes**. A "character" is one byte. The names are
prefixed because the namespace is flat.

```
# --- classification (ASCII) -------------------------------------------
fn str_is_space(c: Str) -> Bool
fn str_is_digit(c: Str) -> Bool
fn str_is_alpha(c: Str) -> Bool
fn str_is_alnum(c: Str) -> Bool
fn str_is_upper(c: Str) -> Bool
fn str_is_lower(c: Str) -> Bool
fn str_is_hex(c: Str) -> Bool
fn str_is_punct(c: Str) -> Bool

# --- length and slices -------------------------------------------------
fn str_len(s: Str) -> Int
fn str_slice(s: Str, lo: Int, hi: Int) -> Str
fn str_at(s: Str, i: Int) -> Str
fn str_tail(s: Str, lo: Int) -> Str

# --- searching ---------------------------------------------------------
fn str_index_of(s: Str, needle: Str) -> Int
fn str_index_of_from(s: Str, needle: Str, from: Int) -> Int
fn str_last_index_of(s: Str, needle: Str) -> Int
fn str_contains(s: Str, needle: Str) -> Bool
fn str_starts_with(s: Str, prefix: Str) -> Bool
fn str_ends_with(s: Str, suffix: Str) -> Bool
fn str_count(s: Str, needle: Str) -> Int

# --- splitting and joining --------------------------------------------
fn str_split(s: Str, sep: Str) -> List        # -> List[Str]
fn str_split_lines(s: Str) -> List            # -> List[Str]
fn str_split_whitespace(s: Str) -> List       # -> List[Str]
fn str_join(parts: List, sep: Str) -> Str     # parts: List[Str]

# --- trimming and padding ---------------------------------------------
fn str_trim(s: Str) -> Str
fn str_trim_start(s: Str) -> Str
fn str_trim_end(s: Str) -> Str
fn str_trim_chars(s: Str, cut: Str) -> Str
fn str_pad_left(s: Str, width: Int, fill: Str) -> Str
fn str_pad_right(s: Str, width: Int, fill: Str) -> Str
fn str_repeat(s: Str, n: Int) -> Str

# --- replacing ---------------------------------------------------------
fn str_replace(s: Str, from: Str, to: Str) -> Str
fn str_replace_all(s: Str, from: Str, to: Str) -> Str

# --- ordering (needs `ord`; see the blocked list) ---------------------
fn str_cmp(a: Str, b: Str) -> Int       # <0, 0, >0, byte-wise
fn str_rank(s: Str, alphabet: Str) -> Int  # the position in a sorted alphabet

# --- building ----------------------------------------------------------
fn str_builder_new() -> List                  # -> List[Str]
fn str_builder_push(mut b, s: Str) -> Unit
fn str_builder_len(b) -> Int
fn str_builder_to_string(b) -> Str
```

#### The semantics, edge case by edge case

| Function | Rule |
| -------- | ---- |
| `str_is_*` | The argument must be exactly one byte; a longer string is a `panic("not a character")`. A byte above 127 is not ASCII: it is not a digit, not a letter, not a space. |
| `str_len` | The byte length. Identical to the `len` builtin; provided so that the library reads consistently and so that `str_len` can be changed to a character count later without touching the callers. |
| `str_slice(s, lo, hi)` | The byte range `[lo, hi)`. Out-of-range bounds panic with `index out of bounds`, exactly like the builtin `slice`. |
| `str_at(s, i)` | The single byte at `i`, as a one-byte string. Identical to the `str_char_at` builtin. |
| `str_tail(s, lo)` | `s[lo..len)`. |
| `str_index_of(s, needle)` | The byte offset of the first occurrence, or `-1`. An empty `needle` returns `0`. |
| `str_index_of_from(s, needle, from)` | As above, but the search starts at `from`. A negative `from` is `0`. |
| `str_last_index_of(s, needle)` | The offset of the last occurrence, or `-1`. |
| `str_contains(s, needle)` | Whether `needle` occurs at all. |
| `str_starts_with` / `str_ends_with` | Byte-wise prefix and suffix tests. An empty prefix or suffix is true. |
| `str_count(s, needle)` | The number of non-overlapping occurrences. An empty `needle` yields `0`. |
| `str_split(s, sep)` | Splits on `sep`. **Consecutive separators produce empty pieces** - the split is not lossy, and `str_join(str_split(s, sep), sep) == s` always holds. An empty `sep` panics with `panic("empty separator")`. An empty `s` gives a list holding one empty string. |
| `str_split_lines(s)` | Splits on `"\n"`. A `"\r"` before the `"\n"` is dropped. A trailing newline does **not** produce a final empty line. An empty `s` gives an empty list. |
| `str_split_whitespace(s)` | Runs of ASCII whitespace are one separator; leading and trailing whitespace is dropped; the result is never empty (an empty input gives an empty list). |
| `str_join(parts, sep)` | The concatenation of the parts with `sep` between them. The empty list gives `""`. |
| `str_trim(s)` | Strips ASCII whitespace from both ends. |
| `str_trim_start` / `str_trim_end` | One end only. |
| `str_trim_chars(s, cut)` | Strips any byte in `cut` from both ends. An empty `cut` returns `s`. |
| `str_pad_left(s, width, fill)` | Pads on the left with `fill` until the width is reached. A string already at least `width` long is returned unchanged. An empty `fill` panics with `panic("empty pad")`. `fill` is truncated to its first byte if it is longer. |
| `str_pad_right(s, width, fill)` | The mirror image. |
| `str_repeat(s, n)` | `s` repeated `n` times. `n <= 0` gives `""`. |
| `str_replace(s, from, to)` | Replaces the **first** occurrence. An empty `from` panics. |
| `str_replace_all(s, from, to)` | Replaces every occurrence, left to right, non-overlapping. An empty `from` panics. |
| `str_builder_new()` | A fresh builder. |
| `str_builder_push(b, s)` | Appends a chunk. `b` is borrowed, so the caller's builder grows. The caller must pass a mutable binding. |
| `str_builder_len(b)` | The number of chunks. |
| `str_builder_to_string(b)` | The chunks joined. |

#### The three implementation details that decide whether this is possible at all

**(a) The element type has to be established at creation.** A `List`'s element
type is fixed by its first `list_push`, and a list whose element type was
never established cannot be read (T049). So a builder cannot be "an empty
list" - the functions that read it back would hit T049.

The fix is to **seed** the list and let the inferred return type carry the
element type out of the function:

```
fn str_builder_new() -> List:
    let b = list_new()
    list_push(b, "")        # seeds the element type as Str
    return b
```

The semantic analyser refines the binding `b` to `List[Str]` at the push
(`sema:1399-1419`) and the `return` then reads the refined type
(`sema:864-888`), so the function's inferred return type is `List[Str]` and
the caller sees it. **Verified**: this compiles and runs today.

This is the same trick the self-hosted parser uses to fix the type of its
node arena (`docs/self-hosting.md:85-86`), and it is the template for every
list-returning library function.

**(b) The builder is a list of chunks, and the join is a fold.** The builder is
a `List[Str]` of chunks; `str_builder_to_string` folds `concat` over them.
That is `O(total bytes x number of chunks)` time and one allocation per chunk.
A pairwise merge would make it `O(total log chunks)`. The fold is the simpler
first version; the difference only shows for large builders, and the real fix
is a runtime-side growable byte buffer (see the risks).

**(c) The `List` annotation on `str_join` is documentation only - until the
language change lands.** `fn str_join(parts: List, sep: Str)` compiles today
because an unknown type in a *parameter* annotation is silently ignored
(`sema:288-302`); only a *field* annotation is a hard error. The parameter's
type is then refined from the call sites. It works, and it documents the
intent, but it does not constrain anything - and it does not even complain
when it is wrong. **Verified**: `fn f(xs: Bogus)` compiles cleanly.

### 5. The map problem

The language has `List` and no map. The compiler itself needs one (stage 10d
needs "Hash maps in std", `docs/self-hosting.md:41`), and the intern table
needs one.

#### The three candidates, and what each costs

| | **(a) a list of pairs, linear search** | **(b) a sorted list, binary search** | **(c) a hash table built out of lists** |
| --- | --- | --- | --- |
| lookup | `O(n)` string comparisons | `O(log n)` comparisons | `O(1)` expected |
| insert | `O(n)` (the scan) + `O(1)` (the push) | `O(n)` - a list has no insert-at-index, so a shift is a rebuild | `O(1)` expected, plus `O(n)` on growth |
| memory | one list of pairs | one list of pairs | one list of slots, ~2x the entries |
| code | ~20 lines | ~60 lines (the shift or the rebuild) | ~120 lines (the probe, the grow, the rehash) |
| expressible today? | **yes** | only with `ord` (to compare the keys) | only with `ord` (to hash the keys) |
| good for | a few hundred entries; the compiler's *keyword* tables; a small scope | a read-mostly table that is never written | everything the compiler does at scale |

The honest summary:

* **(a) is the smallest honest map.** It is fully expressible in Nexum today -
  the key comparison is `==` on `Str`, which exists. It is `O(n)` per
  operation, which is fine for the tens of entries a keyword table has and
  hopeless for the tens of thousands a compiler's symbol table has.
* **(b) is dominated.** Binary search wins on lookup but loses on insert, and
  a list cannot be inserted into at an index, so every insert is a rebuild
  that allocates. A compiler inserts far more often than it looks up. Reject
  as the primary structure; keep the *idea* of a sorted list for the one case
  that wants it - deterministic iteration order.
* **(c) is the answer**, and the question is how to build it out of the
  collections that exist.

#### How the hash table is built out of lists

The naive shape - a list of buckets, each bucket a list - **does not work**:
a `List` of `List`s is awkward to mutate (pushing into a list obtained from
`list_get` is T049 - verified) and a record cannot hold a `List` at all
(`List` is not a valid field type - verified, T024).

The shape that **does** work is **open addressing with linear probing, in a
single flat list**:

```
type MapSlot:
    key: Str
    value: Int
    state: Int        # 0 = empty, 1 = occupied, 2 = tombstone, 3 = header
```

The map is one `List[MapSlot]`, and **slot 0 is a header**: `header.value` is
the number of occupied entries. The data slots are `1 .. len-1`, so:

* `map_len(t)` is `list_get(t, 0).value` - `O(1)`, which matters, because
  the intern table needs the next dense id on every insertion and a scan would
  make interning quadratic again.
* `map_capacity(t)` is `list_len(t) - 1`.
* a lookup computes `h = hash(key) % capacity`, reads slot `1 + h`, and walks
  forward until it finds the key or an empty slot.
* an insert finds an empty slot (or the key itself) and **writes** it with
  `list_set`, bumping the header's count when it is a new entry.
* growth builds a new, larger list and rehashes into it.

This needs only `list_get`, `list_set`, `list_len` and `list_push` - no
insert-at-index, no list of lists, no `List` inside a record, and no
`list_get` result being pushed into. **Verified working in Nexum today**,
with the hash faked as `len(key)` because the real hash needs `ord`.

The cost of open addressing is that a lookup can degrade to `O(n)` when the
table is nearly full, so the table must grow at a load factor of about 0.7,
and growth is `O(n)`. A chained hash table would avoid the clustering but
needs the chain heads in a second list, which means the map handle is two
lists - which means every map operation takes two parameters, or the language
grows `List[T]` annotations so that a record can hold both.

#### The value type: `Int`, and why

The map's value type is fixed to `Int`. Everything else is stored behind the
handle:

```
let m = map_new(64)
map_put(m, "alpha", 7)      # 7 is a handle into a list the caller owns
let payload = list_get(my_values, map_get(m, "alpha"))
```

This is not a limitation worked around; it is the shape the language already
prefers. A record cannot contain itself, so the self-hosted AST is "an arena of
nodes with indices" (`docs/language-reference/decisions.md:112-126`,
`docs/self-hosting.md:84-86`). The map is the same idea: a string-to-handle
table plus a flat array of payloads. It has three advantages:

* **One implementation.** The monomorphic inference
  (`sema:314-403`) means a library function has exactly one instantiation per
  program, so a `Map[Str, Str]` and a `Map[Str, Int]` would be two copies of
  the same code. With the value type fixed to `Int`, there is one.
* **The payload is a flat array**, which is the layout the memory traffic
  wants, and which phase 11f's SIMD work can address directly.
* **The map composes with the intern table**, which is also `Str -> Int`.

The price is that the caller does the indirection. That is the same trade the
arena pattern makes everywhere else in this language.

#### The hash function, and what it is blocked on

The hash needs the numeric value of a byte. The language has no way to get it:
`s[i]` yields a **one-byte `Str`**, not a number, and `Str` has no ordering
(`grammar.md:139-143`) and no conversion to `Int`. There is no `ord` builtin.

So the hash is blocked on one primitive. Two ways to unblock it:

* **Add `ord(s: Str) -> Int` and `chr(n: Int) -> Str` to the runtime.** They
  are byte-level primitives, exactly like `len` and `slice`, and they belong
  beside them in `crates/nexum-runtime/src/lib.rs:190-202`. With them, the
  library writes its own hash:

  ```
  fn map_hash(s: Str) -> Int:
      let n = len(s)
      let mut h = 2166136261
      let mut i = 0
      while i < n:
          let b = ord(slice(s, i, i + 1))
          h = (h * 1000003 + b) % 1000000007
          i = i + 1
      return h
  ```

  The modulus keeps the accumulator in range, and the multiplier is small
  enough that `h * 1000003` stays inside an `Int` (the language's integer
  arithmetic is wrapping, so an overflow would not trap - but it would make
  the hash arbitrary). The bitwise `&` that a textbook FNV-1a would use does
  not exist until the numeric tower.

* **Add `str_hash(s: Str) -> Int` to the runtime** as a fast path. One call,
  no per-byte allocation. It is strictly less general than `ord`, but it is
  enough for the map and it is cheap.

**Recommendation: `ord`/`chr` first, `str_hash` as an optimisation later.**
`ord` is the more generally useful primitive - it also unblocks string
ordering, digit parsing, and character classification by value - and the
library should not have to ask the runtime for a hash when it can compute one.

The per-byte `slice` in the hash above allocates a one-byte heap string per
byte, which is why a runtime `str_hash` is worth having later.

#### The map API

```
fn map_new(capacity: Int) -> List            # -> List[MapSlot]
fn map_get(m, key: Str) -> Int               # -1 when absent
fn map_get_or(m, key: Str, default: Int) -> Int
fn map_has(m, key: Str) -> Bool
fn map_put(m, key: Str, value: Int) -> Unit  # insert or replace
fn map_remove(m, key: Str) -> Bool
fn map_len(m) -> Int                         # O(1): the header slot
fn map_capacity(m) -> Int
fn map_clear(m) -> Unit
```

`map_get` returning `-1` for "absent" is forced by the absence of `Option`
(`decisions.md:41-57`); `map_get_or` is the version that does not need the
caller to think about the sentinel. `map_put` mutates the map in place; the
map is a `List`, so it is a reference, and the caller keeps the handle.

### 6. The sorting problem

#### What is sortable today

| Element type | Sortable? | Why |
| ------------ | --------- | --- |
| `Int` | **yes** | `<` works on numerics (`grammar.md:129-134`) |
| `Float` | **yes** | the same, with IEEE semantics |
| `Bool` | no | no ordering on `Bool`; not worth it |
| `Str` | **no** | `Str` has no ordering (`grammar.md:143`); needs `ord` |
| records | no | the comparison is per field, so it is per record type |

The comparison sort itself is easy: quicksort or mergesort over `list_get` and
`list_set`, with a temporary for the swap. **Verified working in Nexum today**
- a bubble sort over a `List[Int]` sorts correctly.

#### The real constraint: one instantiation per program

Because the signature table is a single monomorphic map refined to a fixpoint
(`sema:314-403`), a library function's parameter type is fixed by its first
call site and every later call must agree. **Verified**: a function
`fn rev(xs: List) -> List` called with a `List[Int]` and then with a
`List[Str]` is a hard T007 ("argument 1 of `rev` expects List[Int], found
List[Str]").

So there is no one `sort`. There is `sort_int` and `sort_float`, and a third
and fourth for whatever else a program needs. Shipping `sort_int` and
`sort_float` as separate copies of the same algorithm is ugly but it is the
truth of the language today.

#### The escape hatch: sorting by rank

A single sort that works for **any** element type is possible if the sort
never looks at the elements. Sort a list of pairs instead:

```
type SortPair:
    rank: Int
    handle: Int
```

`sort_pairs(pairs: List) -> List` sorts by `rank`, and the caller computes
the rank. For `Str` keys the rank comes from `str_rank(key, alphabet)` - a
library function that gives a key's position in a sorted alphabet - which is
`O(n)` per key unless the alphabet is sorted once, so it is a real cost, but
it is a *choice* rather than a language limitation, and it lets one sort
implementation serve every element type.

The same trick serves the compiler's own needs: the symbol table is a
`Str -> Int` map, and the integer handles are already sortable.

#### What sorting by key would need

"sort by a key function" is blocked on functions not being values
(`sema:912-914`). The rank sort is the workaround; first-class functions are
the fix.

### 7. The interning problem

The compiler needs to intern strings: every distinct string gets one dense
integer, and the same string always gets the same integer.

#### What the language forces

* **Strings are byte strings** (`runtime-abi.md:99-101`), so interning is
  byte-exact. Two strings that differ in one byte are different entries. That
  is correct for a compiler (source is ASCII in practice) and must be stated.
* **A `Str` is a pointer** (`runtime-abi.md:84`), so two equal strings are
  two different heap objects. The language has no pointer type, so a program
  cannot compare pointers, and therefore cannot detect "these two strings are
  the same object". Interning must therefore be *by content*, with the
  canonical string kept in a parallel structure.
* **There are no globals** (`sema:418-432`; verified: a top-level `let mut`
  parses and is then dropped - using it is T004). The intern table cannot be
  a hidden global. It is a value the caller owns and threads through.
* **A NUL byte cannot be represented** - strings are NUL-terminated
  (`runtime-abi.md:84`, `:99-101`), and `read_file` truncates at a NUL
  (`runtime-abi.md:232-233`). A source file with a NUL in it cannot be interned
  faithfully. This is a real limitation to record, not to solve now.

#### The design

```
fn intern_new() -> List                 # the table: a List[MapSlot], seeded
fn intern(table, s: Str) -> Int         # the dense id, 0..n-1
fn intern_count(table) -> Int
```

Two structures, not one: the *table* maps a string to an id, and a *parallel
list of canonical strings* maps an id back to a string. The parallel list is
`List[Str]` and is indexed by the id. It belongs to the caller - the library
has no globals - so the canonical-string list is a `List[Str]` the caller
pushes to, and reading an id back is `list_get(strings, id)`. That function
does not need to be in the library at all.

`intern_new` seeds its table with one empty slot, exactly as the builder
seeds its list, so that the element type is fixed at `MapSlot` and the
inferred return type is `List[MapSlot]`.

Interning is therefore:

```
let table = intern_new()
let strings = list_new()          # List[Str], indexed by id

let id = intern(table, "fn")      # 0
list_push(strings, "fn")          # the canonical copy
```

The intern function, given the map above:

```
fn intern(table, s: Str) -> Int:
    let existing = map_get(table, s)
    if existing >= 0:
        return existing
    let id = map_len(table)       # the next dense id
    map_put(table, s, id)
    return id
```

`map_get` returning `-1` for "absent" is what makes this work without
`Option`. A `map_get_or` variant does not help here, because "absent" and "id
zero" have to be told apart - and `-1` does that.

#### The cost, and what it buys

| Structure | Intern a new string | Intern a known string | Total for n distinct strings |
| --------- | -------------------- | --------------------- | ------------------------------ |
| list of pairs, linear search | `O(n)` | `O(n)` | `O(n^2)` string comparisons |
| open-addressed hash table | `O(1)` expected | `O(1)` expected | `O(n)` |

For a compiler with 50 000 distinct identifiers, the linear version is about
1.25 billion string comparisons - minutes, not milliseconds. The hashed
version is 50 000 hashes. **Interning is the reason the hash table is not
optional.**

And the hash table is blocked on `ord`. So the chain is:

> `ord` (a runtime primitive) -> the hash function (library) -> the map
> (library) -> interning (library) -> stage 10d of the self-hosting plan
> (name resolution and type checking).

### 8. The other modules

#### `text` - the formatting of phase 11a.2

Phase 11a.2 replaces `printf` with our own output routines. The split:

* The **runtime** owns "write these bytes to this descriptor". That is what
  replaces `printf`, `fputs` and `fputc` (`lib.rs:49-51`), and it needs the
  system calls.
* The **library** owns "turn this value into bytes".

The library's `text` module:

```
fn text_int(v: Int) -> Str            # decimal, with a leading '-'
fn text_uint(v: Int) -> Str           # decimal, never negative
fn text_parse_int(s: Str) -> Int      # decimal; panics on a bad digit
fn text_hex(v: Int) -> Str            # lower case, no prefix
fn text_bin(v: Int) -> Str
fn text_float(v: Float) -> Str        # see below
fn text_bool(v: Bool) -> Str
fn text_pad_left(s: Str, width: Int, fill: Str) -> Str
fn text_pad_right(s: Str, width: Int, fill: Str) -> Str

fn text_builder_new() -> List         # List[Str]
fn text_add_str(mut b, s: Str) -> Unit
fn text_add_int(mut b, v: Int) -> Unit
fn text_add_float(mut b, v: Float) -> Unit
fn text_add_bool(mut b, v: Bool) -> Unit
fn text_to_string(b) -> Str
```

**`text_int` is library code and can be written today**: repeated division by
ten, with the digit taken from the literal table `"0123456789"` and sliced
out with `slice`. No `ord` needed - the digit's *value* is the index, not a
byte value. The negative sign is prepended for a negative input.
`text_parse_int` is its inverse, and it is also library code: the digit's
value is found by comparing the byte against the ten characters of the table
(`ord` would make it a single comparison, but ten is fine at this size).

**`text_float` is not library code today.** It needs a `Float` -> `Int`
conversion (the numeric tower, 11c) or the runtime's own formatter. The
runtime has to format floats for `print` anyway (it currently uses C `%g`,
`codegen:723-729`), so the recommendation is: the **runtime** exposes
`float_to_str(v: Float) -> Str` as a builtin in 11a.2 (it needs the float's
bits and correctly-rounded decimal conversion), and the library's `text_float`
is a thin wrapper plus the library's own policy (padding, precision).

**There is no `format(fmt, ...)` function**, because the language has neither
variadics nor tuples. The builder is the general form: `text_add_str`,
`text_add_int` and so on all append to the same `List[Str]`. It is more
verbose than a format string, and it is what the language can express.

#### `collections`

```
# the map (see above)
fn map_new(capacity: Int) -> List
fn map_get(m, key: Str) -> Int
fn map_get_or(m, key: Str, default: Int) -> Int
fn map_has(m, key: Str) -> Bool
fn map_put(m, key: Str, value: Int) -> Unit
fn map_remove(m, key: Str) -> Bool
fn map_len(m) -> Int
fn map_capacity(m) -> Int
fn map_clear(m) -> Unit

# the set: a map whose value is a constant
fn set_new(capacity: Int) -> List
fn set_has(s, key: Str) -> Bool
fn set_add(s, key: Str) -> Unit
fn set_remove(s, key: Str) -> Bool
fn set_len(s) -> Int

# sorting
fn sort_int(xs: List) -> List
fn sort_float(xs: List) -> List
fn sort_pairs(pairs: List) -> List       # by rank; works for any element type

# searching and reshaping
fn list_reverse(xs: List) -> List
fn list_contains_int(xs: List, v: Int) -> Bool
fn list_index_of_int(xs: List, v: Int) -> Int
fn list_slice(xs: List, lo: Int, hi: Int) -> List
fn list_append(a: List, b: List) -> List
```

Note the suffixes on `list_contains_int` and `list_index_of_int`: those two
compare an element against a value, so the element type has to be pinned, and
the monomorphic inference pins it to `Int` for the whole program. The
unsuffixed `list_reverse`, `list_slice` and `list_append` never look at an
element's value, so their element type simply flows from the parameter to the
result - one instantiation per program, but an instantiation that does not
have to be spelled out in the name.

Note `list_reverse(xs: List) -> List`: the element type flows from the
parameter to the result by seeding the result list with the first element
(`list_push(out, list_get(xs, 0))`). That is how a *generic-looking* function
is written in a monomorphic language: it works for one element type per
program, and the caller gets the element type back.

#### `math`

The **math builtins are phase 11c and belong to the runtime**, not the
library: `sqrt`, `exp`, `log`, `sin`, `cos`, `tan`, `asin`, `acos`, `atan`,
`atan2`, `sinh`, `cosh`, `tanh`, `erf`, `floor`, `ceil`, `round`, `trunc`,
`abs`, `min`, `max`, plus `pow` and `hypot`. They cannot be written in Nexum:
they need the bits of a `Float` and a `Float` -> `Int` conversion, and the
argument reduction that `sin` and `exp` need to be accurate is not expressible
without them. They are also part of the numeric tower (D015), which is a
language feature, not a library.

The **library's** `math` module is the composition layer, and it is where the
model framework's needs are met:

```
fn math_deg(rad: Float) -> Float
fn math_rad(deg: Float) -> Float
fn math_lerp(a: Float, b: Float, t: Float) -> Float
fn math_clamp(v: Float, lo: Float, hi: Float) -> Float
fn math_sign(v: Float) -> Float
fn math_approx_eq(a: Float, b: Float, eps: Float) -> Bool
fn math_relu(v: Float) -> Float
fn math_gelu(v: Float) -> Float        # needs erf, a builtin
fn math_softmax(xs: List) -> List      # List[Float]; subtract the max first
fn math_sum(xs: List) -> Float
fn math_mean(xs: List) -> Float
fn math_max(xs: List) -> Float
fn math_min(xs: List) -> Float
```

Two notes:

* `math_softmax` must subtract the maximum before exponentiating, or it
  overflows. The reduction over a `List[Float]` is a library function - the
  element type is `Float` and the type is fixed program-wide, which is fine
  because the model framework only ever has one `List[Float]`.
* **A random number source is blocked.** A deterministic PRNG needs bitwise
  operators (`^`, `>>`, `<<`), which do not exist until 11c. A polynomial
  generator with `%` and `*` works today but is a poor PRNG. The honest
  answer: the library's PRNG lands with 11c, or the runtime exposes
  `rand_u64() -> Int` as a source of entropy.

#### `fs` - files, and the buffered forms

The runtime owns whole-file reading and writing (`lib.rs:204-207`,
`strings.rs:33-55`). The library owns the policy:

```
fn fs_read(path: Str) -> Str
fn fs_write(path: Str, content: Str) -> Unit
fn fs_read_lines(path: Str) -> List        # List[Str]
fn fs_write_lines(path: Str, lines: List) -> Unit
fn fs_exists(path: Str) -> Bool
fn fs_size(path: Str) -> Int
fn fs_join(base: Str, rel: Str) -> Str
fn fs_dirname(path: Str) -> Str
fn fs_basename(path: Str) -> Str
fn fs_extension(path: Str) -> Str
fn fs_strip_extension(path: Str) -> Str
```

**The buffered forms.** A reader that streams from a file needs a file
handle, a position and a buffer. The runtime has none: `read_file` and
`write_file` are whole-file operations, and there is no `file_open`,
`file_read`, `file_write`, `file_seek` or `file_close`. So:

* A **buffered reader over memory** is expressible today: a record
  `{text: Str, pos: Int}` is a value type, `Str` is a valid field type, and
  `reader_next_line(mut r)` advances `r.pos`. This is a genuine buffered
  reader over an in-memory buffer - it is what `fs_read_lines` uses, and it is
  what the compiler's lexer wants (read the file once, then serve it in
  lines).
* A **streaming reader over a file** is blocked on the runtime's file-handle
  builtins. This is also where `append` mode, `fs_size`, `fs_exists`,
  modification time and directory traversal live - the runtime has no `stat`
  and no directory enumeration today, so those three functions in the list
  above are the ones that cannot land until the runtime grows them.

The distinction is worth stating plainly: "buffered" means *not re-reading the
file for every line*, and that is achievable today. It does not mean *reading
the file in chunks from the kernel*, which is not.

### 9. How the standard library is tested

The library is written in Nexum, so it is tested by the same four layers the
rest of the language uses.

#### Layer 1 - the library's own tests, as `nx` programs

```
lib/nexum/std/tests/str_test.nx
lib/nexum/std/tests/text_test.nx
...
```

Each is an ordinary program: it runs assertions and `panic`s on the first
failure. `nx test lib/nexum/std/tests` runs each file and checks the exit
status (`driver/src/main.rs:858-897`) - **no harness change is needed at
all**. The same machinery already runs a project's `tests/` directory
(`docs/package-format.md:3-14`).

The catch: `nx test` is **not** in CI. CI runs `cargo test --workspace`
(`.github/workflows/ci.yml`). So the library's tests need a CI step, or a
Rust integration test that shells out to `nx test`. The latter is better: it
keeps one command (`cargo test --workspace`) as the gate, and it is a new file
(`crates/nexum-driver/tests/stdlib.rs`), which is a change outside this
document's file.

#### Layer 2 - fixture tests with expected output

The existing shape: a `.nx` program, a `.out` file with the expected stdout,
and optionally a `.exit` file (`crates/nexum-driver/tests/backend.rs:94-138`).
The library's fixtures go in a new directory, because the fixture scanner is
**not recursive** (`tests/common/mod.rs:89-96`) - so `tests/fixtures/run/`
cannot hold a `std/` subdirectory without a change to the scanner.

Two options:

* Put the library's fixtures in the existing flat `tests/fixtures/run/`
  directory, with an import of a relative path back to `lib/nexum/std/`. The
  relative path from `crates/nexum-driver/tests/fixtures/run/` to the library
  is `../../../../../lib/nexum/std/str.nx` - five levels up. It works, it is
  ugly, and it breaks if the fixture directory moves.
* Add a `tests/fixtures/std/` directory and a new test function that scans it.
  This needs a one-line change to `nx_files` (or a new recursive scanner) and
  a new test function. **Cleaner; recommended.**

#### Layer 3 - differential tests against a reference

This is the strongest layer, and the repository already has the pattern: the
self-hosted lexer must produce the Rust lexer's token stream, byte for byte
(`tests/backend.rs:143-194`). The same shape applies to the library:

| Library function | Reference | Compared |
| ---------------- | --------- | -------- |
| `text_int` | Rust's `format!("{}", v)` | the exact string, for a table of edge values (0, -1, `i64::MIN`, `i64::MAX`) |
| `text_hex`, `text_bin` | Rust's `format!("{:x}", v)` | the exact string |
| `str_split`, `str_join` | Rust's `split` / `join` | the list contents and the round trip |
| `text_float` | Rust's `format!("{}", v)` | the exact string, once the runtime formatter exists |
| `str_replace_all` | Rust's `replace` | the exact string |

The differential test compiles the Nexum program, runs it, and compares its
stdout with the reference's output. The comparison is per byte, so a
difference in the last character of an integer is caught.

#### Layer 4 - property tests, in the library's own test programs

Round trips and invariants that need no reference:

* `str_join(str_split(s, sep), sep) == s` for a table of inputs and
  separators.
* `str_replace_all(str_replace_all(s, a, b), b, a) == s` when `a` and `b` do
  not overlap.
* `text_int(text_parse_int(s)) == s` for every decimal string, and
  `text_parse_int(text_int(v)) == v` for every `v` in a table.
* `sort_int` produces a non-decreasing sequence and is a permutation of its
  input.
* `map_put` then `map_get` returns what was put; `map_get` of an absent key
  returns `-1`.
* `intern` is idempotent: `intern(t, s) == intern(t, s)`, and two different
  strings get different ids.

#### What the library must be careful about in its own tests

The library inherits the language's sharp edges, and its tests are where they
get pinned down:

* `and` and `or` do not short-circuit, so every index is guarded by an
  explicit test (`docs/self-hosting.md:78-79`).
* A list's element type is fixed by its first push, so a test that builds a
  list of strings and a list of numbers needs two lists.
* A list cannot be popped, so a stack in a list is managed by hand
  (`docs/self-hosting.md:87-89`).
* The collector is a real collector: a test that builds a large structure and
  reads it back after a collection is a genuine test
  (`tests/fixtures/run/list_records_gc.nx`).

### 10. The bootstrap order

Which modules the compiler itself needs, and which only programs need.

| Module | The compiler needs it? | Why | Needed by |
| ------ | ---------------------- | --- | --------- |
| `str` | **yes** | the lexer scans, slices, classifies and compares; the parser splits and joins | stage 10b/10c (already prototyped) |
| `text` | **yes** | the code generator emits LLVM IR as text; it formats integers, floats and identifiers into it | stage 10f (`docs/self-hosting.md:43`) |
| `collections` | **yes** | the symbol table, the scope stack, the intern table | stage 10d (`docs/self-hosting.md:41`) |
| `fs` | **yes** | the driver reads source files and writes output | stage 10g (`docs/self-hosting.md:44`) |
| `os` | **yes** | the driver spawns the linker and reads the environment | stage 10g |
| `math` | **no** | the compiler never needs trigonometry | programs; the model framework (11i) |

So the recommended order, with the gate on each:

| Step | Module | Gate |
| ---- | ------ | ---- |
| **S0** | `str` - split, join, trim, search, build, classify | **nothing** - it can be written today |
| **S1** | `text` - `text_int`, `text_hex`, the builder | nothing for the integer half; the float half waits for 11a.2's `float_to_str` |
| **S2** | `ord` and `chr` (runtime primitives) | a small runtime change, in `crates/nexum-runtime/src/lib.rs` and the sema's builtin table |
| **S3** | `collections` - the map, the set, the sorts | S2, plus `List[T]` annotations if the map handle is to be one value |
| **S4** | `fs` and `os` | runtime file-handle builtins for the streaming half; the in-memory half is ready now |
| **S5** | `math` | the numeric tower (11c) |

S0 and S1 are unblocked and can start immediately. S2 is the first thing that
needs a change outside the library. S3 is the first thing that needs a change
to the language.

The dependency that matters for the self-hosting plan: **stage 10d cannot
start until S3 lands, and S3 needs S2.** That is the critical path from the
standard library into phase 11j.

### 11. What is blocked, and on what

| # | Blocked | The smallest change that unblocks it | Who owns it |
| - | ------- | ---------------------------------------- | ----------- |
| B1 | hashing, string ordering, digit values, character classification by value | **`ord(s: Str) -> Int` and `chr(n: Int) -> Str` as runtime builtins** | the runtime (11a) |
| B2 | a record that holds a list: a map handle as one value, a reader as one value | **`List` and `List[T]` as type annotations** (grammar, parser, sema; the code generator already lowers `Ty::List` correctly) | the language |
| B3 | one `sort` for every element type; one `map` for every value type | **generics**, or the handle/arena convention, or per-type specialisations | the language |
| B4 | `map(f, xs)`, `filter`, "sort by key" | **first-class functions** | the language |
| B5 | "key not found" as a value | **`Option`/`Result`**, or the `-1` sentinel convention | the language |
| B6 | a global intern table, a global PRNG, a global arena | **module-level state**, or the explicit-threading convention | the language |
| B7 | `format(fmt, ...)`; a map handle of two lists | **tuples and variadics**, or the builder API | the language |
| B8 | streaming readers and writers, append mode, directory traversal, file metadata | **runtime file-handle builtins** (`file_open`, `file_read`, `file_write`, `file_seek`, `file_close`, `dir_*`) | the runtime (11a) |
| B9 | `text_float` in the library; a library PRNG | **the numeric tower** (11c): `Float`->`Int` conversions and the bitwise operators | the language (11c) |
| B10 | a library whose names cannot collide with the user's | **qualified names** (`std.str.split`), or the prefix convention | the language |
| B11 | sorting strings, binary search over string keys | **B1** (`ord`), after which the library can compare bytes itself | the library, after B1 |
| B12 | a tree of lists; a bucket list of lists | **answered** by the flat open-addressed layout - the map needs no bucket list at all | - |

Two of these are worth pulling out of the table, because they are not really
"blocked" - they are **decided**:

* **B5 and B6 are answered by convention, not by a language change.** "Not
  found" is `-1`, or a `get_or` with a default; state is threaded
  explicitly. These are the conventions the first version of the library
  should use, and they are the same conventions the self-hosted parser
  already uses for its flat AST arena.
* **B3 is answered by the handle convention** for the map. For the sort it is
  answered by shipping `sort_int` and `sort_float` and a rank sort, at the
  cost of a duplicated algorithm.

---

## Risks

1. **The monomorphic inference makes the library's surface awkward, and the
   cost is not visible until the second instantiation is needed.** The first
   `sort_int` is fine. The moment a program wants to sort a `List[Float]`
   too, there is a second copy of the algorithm, and the third element type
   makes three. The mitigations (specialise, or the rank sort) are known, but
   the library's API will look odd to a reader who does not know why. *The
   recommendation would be wrong if generics land before the library does* -
   then the right answer is one generic `sort`, and the specialisations are
   wasted work.

2. **The library is compiled into every program that imports any of it.**
   The MIR's dead-code elimination is per function, not per module
   (`crates/nexum-mir/src/lib.rs:926`), so an unused `str_replace_all` is
   still in the binary. A library that grows to a few thousand lines makes
   every program that imports one of its modules a few thousand lines larger.
   *Mitigation:* small modules, import only what is used, and add
   module-level dead-code elimination to the MIR if the library grows past
   that.

3. **The builder's join is quadratic in the number of chunks.** `str_join`
   over a builder of `k` chunks is `O(total x k)`. The self-hosted lexer's
   string-literal scanner is the case that matters, and it appends **one
   byte at a time** - so a 1000-byte literal would be 1000 chunks and 500 000
   bytes of copying. *Mitigation:* the builder should coalesce adjacent
   single-byte chunks, or the runtime should grow a byte buffer. This is the
   most likely performance bug in the first version of the library, and it
   should be a fixture from the start.

4. **The map degrades under clustering.** Open addressing with linear probing
   degrades to `O(n)` as the table fills, and the growth policy has to be
   right. A map with a bad growth policy is a map that gets slow exactly when
   it is used most. *Mitigation:* grow at a load factor of about 0.7 and test
   it at that size.

5. **`ord` may be seen as a language feature rather than a runtime
   primitive.** If it is pushed into the language as a first-class conversion
   (`Int` to `Byte`, a `Byte` type), the library's shape changes: the byte
   type would be a real value type, and `s[i]` would return a byte rather
   than a one-byte string. That is a *better* language, but it is a much
   bigger change, and the standard library cannot wait for it. *The
   recommendation would be wrong if the numeric tower introduces a byte type
   before 11a lands* - then `ord` is redundant and the library should be
   written against the byte type instead.

6. **The flat namespace makes the library a permanent part of the global
   name space.** Once `str_split` is in the compiler's own programs, it is in
   every program's name space, and it can never be renamed without breaking
   every program that uses it. *Mitigation:* the prefix convention, plus the
   rule that a library name never changes once shipped. The alternative -
   qualified names - is the right long-term answer but is a language change
   with no landing date.

7. **The library's tests could become a second, weaker test suite.** The
   fixture and differential machinery is the real gate; `nx test` under
   `lib/nexum/std/tests/` is convenient but it is *not* in CI today. If the
   library's tests live only there, they will rot. *Mitigation:* the CI step
   or the Rust integration test, and the library's behaviour pinned by
   fixtures with expected output, exactly like the rest of the language.

8. **Writing the library in Nexum will expose more language gaps than the
   plan anticipates.** Every module in this plan is a statement about what the
   language can express, and the language is young. The most likely surprise
   is something structural - a record that must hold a list, a function that
   must return two values, a comparison that must be on a non-numeric type.
   *The plan assumes each of these is a small change. If one of them turns
   out to be a phase, the module that needs it slips, and the bootstrap order
   slips with it.*

---

## Open questions

Each of these needs an answer from the orchestrator. The recommendation for
each is given.

1. **Do `ord` and `chr` land in phase 11a, or do they wait for the numeric
   tower?**
   *Recommendation:* **11a**. They are byte-level primitives that belong beside
   `len` and `slice` in the runtime, they unblock the map and therefore stage
   10d of the self-hosting plan, and they do not depend on the numeric tower.
   If the numeric tower introduces a real byte type, they become redundant -
   which is an argument for landing them *before* the tower, not after.

2. **Does the standard library get qualified names, or the prefix convention?**
   *Recommendation:* **the prefix convention now, qualified names as a
   language change later.** The prefix works with the flat namespace that
   exists today and costs nothing. Qualified names (`std.str.split`) are the
   right shape and should be recorded as a language decision, but they are not
   on the critical path and the library should not wait for them.

3. **Is `List[T]` added as a type annotation?**
   *Recommendation:* **yes, and early** - it is a small change (grammar,
   parser, sema; the code generator already lowers `Ty::List` to a pointer
   with an 8-byte scan) and it is what makes a map handle, a reader and a
   builder a single value rather than two or three parameters. It also removes
   the trap that an unknown parameter type is silently ignored, which will
   otherwise bite every library author.

4. **Does the library get a `Map` with a general value type, or only
   `Str -> Int` handles?**
   *Recommendation:* **only `Str -> Int` handles in the first version.** The
   monomorphic inference makes a general map two implementations, and the
   handle convention is the shape the language already prefers for its AST
   arena. If generics land, a general `Map` replaces it.

5. **Is the standard library's version recorded in `nexum.toml`, and is a
   mismatch an error?**
   *Recommendation:* **record it and warn; do not error.** The library ships
   with the toolchain, so a mismatch is a developer situation, not a user's.
   A hard error would break every project on every toolchain upgrade for no
   benefit.

6. **Where does the library live on disk when it is installed, and who owns
   the packaging change?**
   *Recommendation:* `/usr/share/nexum/std/` on Debian (beside the extension
   that is already there), `%ProgramFiles%\Nexum\std\` on Windows, and
   `~/.local/share/nexum/std/` for the macOS manual install - which today
   copies only the binary (`crates/nexum-driver/src/setup.rs:166-179`) and so
   needs a change. The packaging change is a separate piece of work and
   should be scheduled with it.

7. **Does the library's test suite run in CI, and through which door?**
   *Recommendation:* **a new Rust integration test** (`crates/nexum-driver/tests/stdlib.rs`)
   that runs the library's `nx` test programs and the differential fixtures,
   so that `cargo test --workspace` stays the single gate. `nx test` alone is
   not enough, because CI does not run it today.

8. **Is a `Str` builder a runtime primitive or a library type?**
   *Recommendation:* **a library type for now**, with the runtime-side growable
   byte buffer recorded as the fix for the quadratic join. A runtime
   `str_builder` would be a container primitive like `List`, which is
   defensible, but it adds a second container to the collector's contract for
   a problem the library can solve well enough.

---

## Changes needed elsewhere

This document changes no code. The following changes are implied by the
recommendation, and are listed here so that the orchestrator can schedule them
against the phases that own them.

| Where | Change | Phase | Blocking? |
| ----- | ------ | ----- | --------- |
| `crates/nexum-parser/src/lib.rs:247-262` | the import resolver: a library-root search, the reserved `std/` prefix, and a diagnostic that names the roots searched | 11a | **yes** - nothing ships without it |
| `crates/nexum-runtime/src/lib.rs`, `crates/nexum-sema/src/lib.rs` | the `ord` and `chr` builtins | 11a | **yes** - the map, the sort of strings, and interning all wait on it |
| `crates/nexum-driver/src/main.rs:899-926` | `nx doctor` prints the resolved library root | 11a | no |
| `crates/nexum-driver/src/main.rs:215-249` | `nx new` scaffolds a project that imports the library, and prints the resolved root | 11a | no |
| `crates/nexum-sema/src/lib.rs:537-555`, `crates/nexum-parser` | `List` and `List[T]` as type annotations; and a diagnostic for an unknown *parameter* type, which is silently ignored today | 11a/11b | **yes** for the map handle; no for the first module |
| `crates/nexum-driver/tests/common/mod.rs:89-96`, `tests/backend.rs` | a fixture directory for the library, and a differential test file | 11a | **yes** - the library's tests are the gate |
| `.github/workflows/ci.yml` | nothing, if the tests are a Rust integration test | 11a | no |
| `crates/nexum-driver/src/setup.rs:166-179`, `packaging/` | install the library beside the compiler | 11a | no - the `$NEXUM_STD` root covers the development period |
| `docs/standard-library.md` | rewritten: it describes the builtin set as the standard library, which stops being true | 11a | no |
| `docs/language-reference/decisions.md` | new decisions: the runtime/library boundary, the library search path, the reserved `std/` prefix, the prefix convention | 11a | no |
| `docs/language-reference/grammar.md` | the `List[T]` annotation, if it is adopted | 11a/11b | no |
