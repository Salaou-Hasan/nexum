# The contract for phase 11a.2

Status: **active**. This is the contract the implementation agents work
from. It follows the research in `specs/research/`, which each agent should
read for the reasoning behind its part.

The goal of phase 11a.2: **a Nexum program on Linux or macOS depends on
the kernel and on nothing else.** At the start of the phase a program
declares six external symbols; at the end it declares none.

## Who owns what

Every agent owns its own files. Nothing else is touched. If an agent needs
a change outside its files, it writes the request in its report under
"Changes needed elsewhere" and the orchestrator makes it.

| Agent | Owns |
| ----- | ---- |
| the allocator | `crates/nexum-runtime/src/heap.rs`, `crates/nexum-driver/tests/allocator.rs` |
| the output | `crates/nexum-runtime/src/format.rs`, the `print` block in `crates/nexum-codegen/src/lib.rs`, `crates/nexum-driver/tests/output.rs` |
| the maths | `crates/nexum-runtime/src/math.rs`, `crates/nexum-driver/tests/math.rs` |
| the bytes | `crates/nexum-runtime/src/bytes.rs`, the builtin dispatch in `crates/nexum-sema/src/lib.rs` |
| the layout | `crates/nexum-target/src/lib.rs` and its tests |
| the benchmarks | `crates/nexum-driver/src/bin/nx-bench.rs`, `benchmarks/suite/` |
| the syscall tests | `crates/nexum-driver/tests/syscall_layer.rs` |

Two agents touch the compiler: **the output** touches the code generator's
`print` block, and **the bytes** touches the sema's builtin dispatch. No
other agent may touch those two files in this phase. If an agent needs
something there that is not in its own scope, it asks.

The module declarations for `heap`, `format`, `math` and `bytes` already
exist in `crates/nexum-runtime/src/lib.rs`, and the files are empty stubs.
An agent fills its own file in.

## The house rules for this phase

* The runtime is freestanding: no C library, on the POSIX targets
  (`docs/runtime-abi.md` §0). The Windows build keeps the C library.
* **No `udiv i128`, no `sdiv i128`.** LLVM lowers a 128-bit division to a
  `__udivti3` call, which is a C library dependency. Everything has to be
  built from 64-bit operations.
* **No new external symbols.** The phase's acceptance test is that the
  generated IR declares no C symbol at all for the POSIX targets.
* The runtime is LLVM IR text. Anything the runtime needs must be writable
  in that form, and the agents should say so in their report if a design
  turns out not to be.
* The collector's object layout does not change
  (`docs/runtime-abi.md` §2): a 32-byte header, the size at `+0`, the mark
  at `+8`, the next-object pointer at `+16`, the scan length at `+24`, and
  the payload at `+32`.

## The allocator

Read `specs/research/allocator-design.md` first. The design is settled:
a size-class allocator over `mmap`ed chunks.

The interface the collector uses:

```
nexum_heap_alloc(total) -> ptr      ; a block of `total` bytes, 16-byte aligned
nexum_heap_free(ptr, total)         ; the block goes back to its class
nexum_size_class(total) -> class    ; the class of a size, computed not searched
```

The rules that make it small enough to write in IR text:

* **The class is computed, not searched.** 16-byte granularity from 32 to
  4096, then four doubling classes to 65536, then a class of its own for
  anything larger. Nothing is split and nothing is merged, so there is no
  coalescing code.
* **The free list lives in the collector's `+16` word.** A block is either
  a live object or a free block, never both. The sweep unlinks before it
  frees, so the word is free exactly when it is needed.
* **A block larger than the biggest class has its own mapping**, which is
  unmapped when the block dies.
* **A failed mapping returns null**, and the collector turns it into
  `nexum_oom()`.

Two things the allocator must get right that are easy to get wrong:

* **The mapping arguments are in the order the system call expects**:
  address, length, protection, flags, descriptor, offset. The `mmap`
  wrapper takes six arguments now, and the flags go in the fourth register
  and the descriptor in the fifth.
* **The page size is 16384 on macOS/AArch64** and 4096 everywhere else.
  `munmap` refuses a length that is not a multiple of the page size, so the
  wrong constant leaks every large block. The page size is already a
  function in `crates/nexum-runtime/src/syscalls.rs`.

The agent must also provide a **raw allocation entry point** - a block of
an exact size that the collector does not manage - because the arenas of
phase 11d need it, and because a raw block is the only thing a program can
hand to the kernel.

The tests must cover: an allocation and a free, a reallocation of the same
size coming from the free list, a size in each class, a size above the
largest class, and the alignment of every returned block.

## The output

Read `specs/research/number-formatting.md` first. The requirement is
**byte-for-byte identical output** to what the C library produces today,
because the output fixtures depend on it.

The runtime entry points, which write to stdout through the system call
layer and allocate nothing:

```
nexum_print_int(v)        ; decimal, with a leading minus
nexum_print_bool(v)       ; 0 or 1
nexum_print_str(s)        ; the bytes
nexum_print_float(v)      ; exactly C's %g, precision 6
```

and a family that formats into a buffer the caller owns:

```
nexum_fmt_int(v, buf) -> len
nexum_fmt_float(v, buf) -> len
nexum_fmt_bool(v, buf) -> len
```

The float format is C's `%g`: six significant digits, the exponential form
when the exponent is below -4 or not below the precision, the trailing
zeros removed, and the exponent written with at least two digits. The
special values are `inf`, `-inf` and `nan` - the C library's spelling of
NaN is platform dependent, so the language fixes it.

The conversion is an **exact** six significant digit conversion, done with
a small base-2^32 bignum: one multiplication by a power of five and
repeated division by 10^9, in 64-bit operations. The shortest round-trip
form is **not** what `%g` prints, and it needs a division by 128 bits,
which the phase's rules forbid.

The code generator's `print` builtin changes from building a `printf`
format string to calling the runtime's entry point. The `@fmt_N` globals,
`get_fmt_global` and `declare i32 @printf` go away.

The tests must compare the runtime's output against the C library's for a
large set of values - the research already contains a reference
implementation and the numbers it was checked against - and the run
fixtures must not change.

## The maths

Read `specs/research/math-implementations.md` first.

The functions the model framework needs, in the runtime:

```
nexum_sqrt(x) -> f64
nexum_exp(x) -> f64
nexum_log(x) -> f64
nexum_tanh(x) -> f64
nexum_pow(x, y) -> f64
nexum_erf(x) -> f64
```

plus the rounding functions LLVM lowers to a libm call: `floor`, `ceil`,
`trunc` and `round`.

Each has an accuracy contract, in ULPs, and the tests must measure it
rather than assert a value that was copied from the research. The
algorithms, the argument reductions and the polynomial coefficients are in
the research document; the agent implements them and checks the accuracy
against the C library while it is on a machine that has one.

`exp` and `log` are the ones that matter: a training run's loss curve
depends on them.

## The bytes

The standard library needs a byte's numeric value, and it does not have
one: `ord` and `chr` are the primitives every other collection is built
on (the map's hash, the sort's comparison, the interning table).

The runtime gets `crates/nexum-runtime/src/bytes.rs` with the two
operations, and the sema gets the two builtins. The builtins' names follow
the house style for the standard library, which prefixes everything
(`byte_value`, `byte_from_value`), because imports flatten into one
namespace.

## The layout

`crates/nexum-target/src/lib.rs` has hand-written data layouts, and they
have drifted from clang's canonical ones for four of the five targets. It
is inert today, but `i128:128` and `-Fn32` become live when phase 11c adds
`f32` and the integer widths. The agent fixes the layouts to match
clang's, and writes the test that compares them.

## The benchmarks

Read `specs/research/performance-methodology.md` first. The harness is a
new binary, `crates/nexum-driver/src/bin/nx-bench.rs`, and the cases live
in `benchmarks/suite/`. Cargo finds the binary without a `Cargo.toml`
change.

The cases are `.nx` files with a header comment:

```
# BENCH unit=ms ops=1000000 expect=3628800 ir_check=call main
```

`expect` makes every case a correctness test as well as a measurement, so
a benchmark that measures the wrong thing fails. `ir_check` stops the
optimiser from deleting the call being measured.

The first cases are the four runtime claims the programme makes: the
system call overhead, the call overhead, the allocation cost, and the cost
of a collected allocation. Each needs a C twin with the same algorithm.

## The syscall tests

`crates/nexum-driver/tests/syscall_layer.rs` covers the layer on its own:
the numbers per system, the immediates, the argument order, the stack
alignment, and the file routines - read, write, and a round trip. The
layer's failure modes are silent, so the tests have to be about the shape
of the generated IR, not only about the behaviour.

## How the phase is judged

* The generated IR for a POSIX target declares **no** external symbol.
* The freestanding gate (`crates/nexum-driver/tests/freestanding.rs`) is
  updated to the new expectation - the number may only go down.
* Every run fixture still produces the same output.
* The whole suite is green on Linux, macOS and Windows, and clippy is
  clean.
* Each agent's own tests pass, and the report says what was verified.
