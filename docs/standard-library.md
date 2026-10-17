# Nexum Standard Library (Phase 9)

The bootstrap standard library is the set of **builtins** available in every
program without imports. There is no `import` syntax yet (see D002); builtins
are resolved by name.

## Core builtins

| Name        | Signature                  | Description                                              |
| ----------- | -------------------------- | -------------------------------------------------------- |
| `print(x)`  | `print(x: T) -> Unit`      | Print `x` followed by a newline. `T` may be Int, Float, Bool or Str. |
| `panic(msg)`| `panic(msg: Str) -> Never` | Write `msg` + newline to stderr and exit with code 1.     |
| `len(x)`    | `len(x) -> Int`            | Array length (compile-time constant) or string byte length. |
| `concat(a,b)`| `concat(a: Str, b: Str) -> Str` | Concatenate two strings on the managed heap.         |
| `collect()` | `collect() -> Unit`        | Force a garbage collection.                              |
| `heap_used()`| `heap_used() -> Int`      | Live payload bytes on the managed heap.                  |
| `slice(s,lo,hi)` | `slice(s: Str, lo: Int, hi: Int) -> Str` | Substring `s[lo..hi]`. |
| `str_char_at(s,i)` | `str_char_at(s: Str, i: Int) -> Str` | Single character at index `i`. |

## Planned (post-Phase 9)

* `io` — file and stream I/O.
* `math` — trigonometry, rounding, constants.
* `collections` — dynamic vectors, maps, sets (on the managed heap).
* `str` — split, trim, format, parse.

These will be delivered as a real standard library crate once the module
system (Phase 10+) lands. Until then, builtins remain the only "stdlib".

## Calling convention

Builtins are not user functions: they have no source definition and are
lowered directly by the code generator. Their names are reserved and may not
be shadowed by user functions.