# Nexum Standard Library (Phase 9/10)

The bootstrap standard library is the set of **builtins** available in every
program without imports. There is no `import` of library modules yet (see
D002); builtins are resolved by name.

## Core builtins

| Name             | Signature                                  | Description                                    |
| ---------------- | ------------------------------------------ | ---------------------------------------------- |
| `print(x)`       | `print(x: T) -> Unit`                      | Print `x` plus a newline. `T` may be Int, Float, Bool or Str. |
| `panic(msg)`     | `panic(msg: Str) -> Never`                | Write `msg` plus a newline to stderr, exit 1.  |
| `len(x)`         | `len(x) -> Int`                            | Array length (a compile-time constant) or string byte length. |
| `concat(a, b)`   | `concat(a: Str, b: Str) -> Str`            | Concatenate two strings on the managed heap.   |
| `slice(s, lo, hi)` | `slice(s: Str, lo: Int, hi: Int) -> Str` | Substring `s[lo..hi]` (byte range).            |
| `str_char_at(s, i)` | `str_char_at(s: Str, i: Int) -> Str`    | The single byte at index `i`.                 |
| `collect()`      | `collect() -> Unit`                        | Force a garbage collection.                    |
| `heap_used()`    | `heap_used() -> Int`                       | Live payload bytes on the managed heap.        |
| `read_file(p)`   | `read_file(path: Str) -> Str`              | Read a whole file as a string.                 |
| `write_file(p, c)` | `write_file(path: Str, content: Str) -> Unit` | Write a string to a file (truncates).      |

## List builtins

| Name                | Signature                                       | Description                        |
| ------------------- | ----------------------------------------------- | ---------------------------------- |
| `list_new()`        | `list_new() -> List`                            | An empty growable list.            |
| `list_push(l, v)`   | `list_push(l: List, v: T) -> Unit`              | Append `v`.                       |
| `list_len(l)`       | `list_len(l: List) -> Int`                      | Number of elements.                |
| `list_get(l, i)`    | `list_get(l: List, i: Int) -> T`                | The element at index `i`.          |
| `list_set(l, i, v)` | `list_set(l: List, i: Int, v: T) -> Unit`       | Replace the element at index `i`.  |

The element type of a list is fixed by the first `list_push`. Pushing a
different type is a compile error (T049), as is reading a list whose element
type was never established (T049). Indexes are checked at run time and
panic with `index out of bounds`.

A list is a *reference*: two names can refer to the same list, and mutating
through one is visible through the other.

## Modules

`import "path.nx"` at the top level of a file loads another module into the
same program. The path is relative to the importing file. A file is loaded
once, so diamond imports and import cycles are safe. Imported declarations
share one namespace with the importing file.

## Planned (post-Phase 10)

* `io` - file and stream I/O beyond whole-file text reads.
* `math` - trigonometry, rounding, constants.
* `collections` - maps, sets, deques on the managed heap.
* `str` - split, trim, format, parse.

These will be delivered as a real standard library once the module system
grows beyond whole-program loading.

## Calling convention

Builtins are not user functions: they have no source definition and are
lowered directly by the code generator. Their names are reserved and may not
be shadowed by user functions.
