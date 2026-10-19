# The complete checklist of remaining C dependencies

**Question.** Every place where Nexum - the language, its runtime, or the
programs it builds - still depends on the C library, so that the programme
can drive that number to zero and keep it there.

**Summary.** The recommendation is to reach **zero external symbols on every
target**, and to prove it with a gate that counts *referenced* symbols, not
merely *declared* ones. Today a POSIX program declares six C symbols
(`printf`, `fputs`, `fputc`, `exit`, `malloc`, `free`) but only **four** of
them are actually referenced by the linked object (`printf`, `exit`, `malloc`,
`free`); `fputs` and `fputc` are dead declarations on the POSIX path. A
Windows program declares and references **thirteen** (`printf`, `fputs`,
`fputc`, `__acrt_iob_func`, `exit`, `malloc`, `free`, `fopen`, `fclose`,
`fseek`, `ftell`, `fread`, `fwrite`), plus `_fltused`, which clang emits for
the CRT rather than Nexum. Every one of them is the *runtime's* debt, not the
program's: a Nexum program cannot name a C symbol at all, because the
language has no FFI - there is no `extern`, no `unsafe`, no pointer type, and
an unknown callee is the error T006. The replacement for each symbol is
already half-built: the system call layer (`nexum_sys_*`) exists and is
verified on Linux and macOS on x86-64 and AArch64, and three of its eight
wrappers (`mmap`, `munmap`, `exit`) are emitted but never called. What is
missing is the allocator, the output formatter, and - on Windows - the
`ntdll` stubs. The toolchain's own dependencies (`clang` for verification and
linking, Rust and LLVM for the compiler itself) are the *compiler's*
business and are explicitly out of scope for the language's zero, by D012 and
D016.

---

## Recommendation

### R1. The acceptance criterion, stated as a number per target

Count a symbol when the **linked object file** carries an undefined
reference to it, i.e. when `llvm-nm --undefined-only` lists it. A
`declare` line with no call site is not a dependency and must not be
counted.

| Target | Declared today | Referenced today | Target count | Phase that closes it |
| ------ | --------------- | ----------------- | ------------ | -------------------- |
| `linux-x86_64` | 6 | **4** (`printf`, `exit`, `malloc`, `free`) | **0** | 11a.2 |
| `linux-aarch64` | 6 | **4** (same) | **0** | 11a.2 |
| `macos-x86_64` | 6 | **4** (same) | **0** | 11a.2 |
| `macos-aarch64` | 6 | **4** (same) | **0** | 11a.2 |
| `windows-x86_64` | 13 | **13** (+ `_fltused` from clang) | **0** | 11a.2 (POSIX parts) + 11a.2 Windows stubs |

The POSIX declared list is `printf`, `fputs`, `fputc`, `exit`, `malloc`,
`free`; the Windows list is the six above plus `__acrt_iob_func`, `fopen`,
`fclose`, `fseek`, `ftell`, `fread`, `fwrite`.

For reference, the list before the syscall-layer slice of phase 11a (the
commit `e3e16c7`) was **seventeen**: the four stdio entries
(`printf`, `fputs`, `fputc`, `__acrt_iob_func`), the process entry
(`exit`), the two heap entries (`malloc`, `free`), the three string
entries (`strlen`, `memcpy`, `strcmp`), the raw POSIX `write`, and the six
file entries (`fopen`, `fclose`, `fseek`, `ftell`, `fread`, `fwrite`).
`docs/native-roadmap.md:27-40` still describes that state and is stale.

### R2. The symbol-by-symbol checklist

This table is the acceptance criterion. Every row is a symbol a program can
still reach; the "End state" column is what the linked object must look
like when the row is closed.

| # | Symbol | Targets | Where it is used today | What replaces it | Phase | End state |
| - | ------ | -------- | ----------------------- | ---------------- | ----- | --------- |
| 1 | `printf` | all | `crates/nexum-codegen/src/lib.rs:748` - the `print` builtin, with the format strings chosen at `:723-729` (`%lld`, `%g`, `%d`, `%s`) | runtime helpers `nexum_print_int` / `nexum_print_float` / `nexum_print_bool` / `nexum_print_str`, already named in `docs/language-reference/grammar.md:263-264` but never implemented; they write through `nexum_sys_write` (POSIX) or a Windows handle write | 11a.2 | no `printf`; `print` is ours, and the float format is specified by Nexum, not by libc's `%g` |
| 2 | `malloc` | all | `crates/nexum-runtime/src/lib.rs:147` - `@nexum_alloc_scanned`, which every managed allocation goes through | our own allocator over `nexum_sys_mmap`, emitted already at `crates/nexum-runtime/src/syscalls.rs:278-288` and never called | 11a.2 | no `malloc`; the collector owns the heap |
| 3 | `free` | all | `crates/nexum-runtime/src/lib.rs:186` - `@nexum_sweep`, which reclaims every dead block | our own allocator's free list (and `nexum_sys_munmap`, emitted at `crates/nexum-runtime/src/syscalls.rs:289`, never called) | 11a.2 | no `free` |
| 4 | `exit` | all | `crates/nexum-runtime/src/lib.rs:124` (POSIX `@nexum_panic`) and `:118` (Windows `@nexum_panic`) | POSIX: `nexum_sys_exit`, emitted at `crates/nexum-runtime/src/syscalls.rs:290`, never called - but Linux `exit` (60) is the *thread* exit; the process exit needs `exit_group` (231), which is not in the table at `crates/nexum-runtime/src/syscalls.rs:60-72`. Windows: an `NtTerminateProcess` stub | 11a.2 | no `exit`; the panic path ends in a system call |
| 5 | `fputs` | Windows (declared on POSIX too) | `crates/nexum-runtime/src/lib.rs:118` - the Windows `@nexum_panic` writes the message with it | a write to the Windows error handle | 11a.2 | no `fputs`; and the dead POSIX declaration at `crates/nexum-runtime/src/lib.rs:50` is deleted |
| 6 | `fputc` | Windows (declared on POSIX too) | `crates/nexum-runtime/src/lib.rs:118` - the newline in the Windows `@nexum_panic` | the same handle write | 11a.2 | no `fputc`; and the dead POSIX declaration at `crates/nexum-runtime/src/lib.rs:51` is deleted |
| 7 | `__acrt_iob_func` | Windows | `crates/nexum-runtime/src/lib.rs:118` - it is how the runtime gets the `stderr` `FILE*` | an `ntdll` stub for the standard error handle | 11a.2 | no `__acrt_iob_func` |
| 8 | `fopen` | Windows | `crates/nexum-runtime/src/strings.rs:48` (and `:51`) - `@nexum_read_file` / `@nexum_write_file` | an `NtCreateFile` stub | 11a.2 | no `fopen` |
| 9 | `fclose` | Windows | `crates/nexum-runtime/src/strings.rs:48` and `:51` | an `NtClose` stub | 11a.2 | no `fclose` |
| 10 | `fseek` | Windows | `crates/nexum-runtime/src/strings.rs:48` - seeks to the end to size the file | `NtSetInformationFile` with `FilePositionInformation` | 11a.2 | no `fseek` |
| 11 | `ftell` | Windows | `crates/nexum-runtime/src/strings.rs:48` - the file size | `NtQueryInformationFile` with `FileStandardInformation` | 11a.2 | no `ftell` |
| 12 | `fread` | Windows | `crates/nexum-runtime/src/strings.rs:48` | an `NtReadFile` stub | 11a.2 | no `fread` |
| 13 | `fwrite` | Windows | `crates/nexum-runtime/src/strings.rs:51` | an `NtWriteFile` stub | 11a.2 | no `fwrite` |
| 14 | `_fltused` | Windows | not ours: clang emits it for the MSVC CRT when the module uses floating point | nothing to replace - it disappears when the program no longer links the MSVC CRT, i.e. when it has its own entry point | 11a.2 / 11g | not referenced |
| 15 | the CRT startup (`mainCRTStartup` / `__main` / `_start`) | all | implicit: the program exports `main` (`crates/nexum-codegen/src/lib.rs:237-238`, `:1202-1228`) and the C runtime turns its return value into the process exit code | our own entry point: `_start` on POSIX (call `main`, then `exit_group` / Darwin `exit`), and a CRT-free entry point on Windows | 11a.2 (entry) / 11g (linker) | the program is linked without a C runtime at all |

### R3. The numbered decisions behind the table

1. **The count is a linked-object count, not a declaration count.** A
   `declare` with no call site costs nothing at link time and is not a
   dependency. Counting declarations made the POSIX number look like six
   when it was four, and it would let a symbol creep back in as a
   declaration nobody noticed.
2. **`printf` is the runtime's debt, not the program's.** A program reaches
   `printf` only through the `print` builtin, and `print` is a language
   surface that already has a specified replacement in the language
   reference (`docs/language-reference/grammar.md:263-264`). The programme
   should replace `printf` with the four `nexum_print_*` helpers the
   documentation already names, and the naming should be made real rather
   than the doc being corrected.
3. **`malloc` is the runtime's debt, not the program's.** A program
   allocates only through `concat`, `slice`, `list_push`, `read_file` and
   the compiler's own temporaries; all of them route through
   `nexum_alloc` -> `nexum_alloc_scanned` -> `malloc`. Replacing `malloc`
   is one allocator, in one place.
4. **The float format is a language decision, not a libc detail.** Today
   `print(3.14159265358979)` prints `3.14159`, because `%g` is six
   significant digits; `print(1.0/0.0)` prints `inf`
   (`crates/nexum-driver/tests/fixtures/run/int_wrap.out`). The
   replacement formatter must either reproduce `%g` exactly or the language
   reference must specify a different format and the fixtures must be
   updated. This is the one place where dropping libc changes observable
   program output.
5. **Windows is a separate closing job, not a port of the POSIX one.**
   Windows has no direct system call instruction; the kernel interface
   lives behind `ntdll` and the stubs have to be *generated*
   (`docs/native-roadmap.md:98-103`, `docs/language-reference/decisions.md:190-194`).
   The Windows program's real needs are: a standard-handle write (for
   `print` and the panic message), a process exit, a heap, and open /
   read / write / size / close on a file. That is the list in rows 5-13.
6. **The toolchain's dependencies stay.** `clang` for verification
   (`crates/nexum-toolchain/src/lib.rs:214-248`) and for linking
   (`:174-208`), and Rust and LLVM for the compiler itself, are the
   *compiler's* business by D012 and D016, and phase 11g replaces the link
   step. They are **not** part of the language's zero and must not be
   counted in it.
7. **The gate gets a second, stronger assertion.** `crates/nexum-driver/tests/freestanding.rs`
   (added on `main` at `2a193e3`) is the gate today. It counts
   declarations, it is the right place for the acceptance criterion, and it
   needs one more test that links an object and counts the undefined
   symbols in it.
8. **The documentation that states the number is part of the debt.**
   `docs/native-roadmap.md:27-40` (seventeen) and `docs/runtime-abi.md:21-28`
   (the phase table) state the count. They must be updated in the same
   commit that changes the count, or the next agent will read a stale
   number.

---

## What the code looks like today

### The runtime's declarations, per target

`crates/nexum-runtime/src/lib.rs:46-100` builds the runtime IR. Six
declarations are emitted unconditionally, at `:49-54`:

```
declare i32 @printf(ptr, ...)      // lib.rs:49
declare i32 @fputs(ptr, ptr)       // lib.rs:50
declare i32 @fputc(i32, ptr)       // lib.rs:51
declare void @exit(i32)            // lib.rs:52
declare ptr @malloc(i64)           // lib.rs:53
declare void @free(ptr)            // lib.rs:54
```

On Windows, seven more are added at `:63` and `:67-72`:
`__acrt_iob_func`, `fopen`, `fclose`, `fseek`, `ftell`, `fread`, `fwrite`.
On POSIX, none are added: the system call layer
(`crates/nexum-runtime/src/syscalls.rs:237-292`) and the POSIX file
routines (`crates/nexum-runtime/src/strings.rs:39-45`) are used instead.

Measured with `nx --target <t> emit-ir examples/hello.nx`:

```
linux-x86_64    exit, fputc, fputs, free, malloc, printf      (6)
linux-aarch64   exit, fputc, fputs, free, malloc, printf      (6)
macos-x86_64    exit, fputc, fputs, free, malloc, printf      (6)
macos-aarch64   exit, fputc, fputs, free, malloc, printf      (6)
windows-x86_64  __acrt_iob_func, exit, fclose, fopen, fputc,
                fputs, fread, free, fseek, ftell, fwrite,
                malloc, printf                                (13)
```

### Which of them are now unused

`fputs` and `fputc` are declared on every platform but **called only on the
Windows path**, at `crates/nexum-runtime/src/lib.rs:118`:

```
%err = call ptr @__acrt_iob_func(i32 2)
%f   = call i32 @fputs(ptr %msg, ptr %err)
%n   = call i32 @fputc(i32 10, ptr %err)
```

The POSIX panic (`crates/nexum-runtime/src/lib.rs:122-124`) writes through
`nexum_sys_write` instead. LLVM drops the two unused declarations, so the
POSIX object does not carry them.

Verified by compiling the generated IR to an object and listing the
undefined symbols:

```
$ clang --target=x86_64-unknown-linux-gnu -c hello_linux.ll -o hello_linux.obj
$ llvm-nm --undefined-only hello_linux.obj
         U exit
         U free
         U malloc
         U printf

$ clang --target=x86_64-apple-darwin -c hello_mac.ll -o hello_mac.obj
$ llvm-nm --undefined-only hello_mac.obj
         U _exit
         U _free
         U _malloc
         U _printf

$ clang --target=aarch64-unknown-linux-gnu -c hello_laa.ll -o hello_laa.obj
$ llvm-nm --undefined-only hello_laa.obj
         U exit
         U free
         U malloc
         U printf

$ clang --target=x86_64-pc-windows-msvc -c hello_win.ll -o hello_win.obj
$ llvm-nm --undefined-only hello_win.obj
         U __acrt_iob_func
         U _fltused        <- emitted by clang for the MSVC CRT, not by Nexum
         U exit
         U fclose
         U fopen
         U fputc
         U fputs
         U fread
         U free
         U fseek
         U ftell
         U fwrite
         U malloc
         U printf
```

`_fltused` is emitted by clang, not by Nexum: compiling
`examples/fact.nx` (no floating point) on the Windows target produces the
same thirteen symbols **without** `_fltused`.

(The leading underscore in the macOS list is the Mach-O convention: `_exit`
is the symbol `exit` on that platform. It is the same four dependencies, not
four different ones.)

### Every call the runtime makes into the C library

| Call site | C symbol | Notes |
| --------- | -------- | ----- |
| `crates/nexum-runtime/src/lib.rs:147` (`@nexum_alloc_scanned`) | `malloc` | the only allocation path; every `concat`, `slice`, `list_push`, `read_file` and compiler temporary lands here |
| `crates/nexum-runtime/src/lib.rs:186` (`@nexum_sweep`) | `free` | the only reclamation path |
| `crates/nexum-runtime/src/lib.rs:124` (POSIX `@nexum_panic`) | `exit` | the panic path |
| `crates/nexum-runtime/src/lib.rs:118` (Windows `@nexum_panic`) | `__acrt_iob_func`, `fputs`, `fputc`, `exit` | the panic path |
| `crates/nexum-runtime/src/strings.rs:48` (Windows `@nexum_read_file`) | `fopen`, `fseek`, `ftell`, `fread`, `fclose` | the file path |
| `crates/nexum-runtime/src/strings.rs:51` (Windows `@nexum_write_file`) | `fopen`, `fwrite`, `fclose` | the file path |

There are no others: the only LLVM `declare` statements in the whole
compiler are the thirteen in `crates/nexum-runtime/src/lib.rs:49-72`, and
the only C symbol the *code generator* itself names is `printf`
(`crates/nexum-codegen/src/lib.rs:748`). Every other C call in the emitted
program comes from the runtime's own IR.

### The code generator's emissions

* **`printf` (still live).** `crates/nexum-codegen/src/lib.rs:716-755` is
  the whole of the `print` builtin. The format string is chosen from the
  argument's type at `:723-729` - `%lld\n` for `Int`, `%g\n` for `Float`,
  `%d\n` for `Bool` (with an explicit `zext`, `:742`), `%s\n` for `Str` -
  interned as a private global by `get_fmt_global` (`:211-226`), and the
  call is emitted at `:747-750` as
  `call i32 (ptr, ...) @printf(ptr @fmt_N, ...)`.
* **`strcmp` (replaced).** `crates/nexum-codegen/src/lib.rs:620` and `:640`
  used to be `@strcmp`; they are now `@nexum_strcmp`, defined at
  `crates/nexum-runtime/src/strings.rs:24`. The change was made in commit
  `e3e16c7`.
* **`memcpy` (replaced).** No `@memcpy` remains anywhere in the compiler.
  It was used in the runtime's `nexum_concat`, `nexum_slice`,
  `nexum_list_push` and `nexum_list_set`, and all four now call
  `nexum_memcpy` (`crates/nexum-runtime/src/lib.rs:191`, `:201`, `:224`,
  `:230`), defined at `crates/nexum-runtime/src/strings.rs:19`.
* **`strlen` (replaced).** No `@strlen` remains. It was used in
  `nexum_panic`, `nexum_concat`, `nexum_str_char_at`, `nexum_slice` and the
  Windows `nexum_write_file`; all now call `nexum_strlen`
  (`crates/nexum-runtime/src/lib.rs:122`, `:191`, `:196`, `:201`, and
  `crates/nexum-runtime/src/strings.rs:44`), defined at
  `crates/nexum-runtime/src/strings.rs:15`.
* **`malloc`, `free`, `exit`, `fopen`, `fread`, `fwrite`, `write`,
  `fclose`, `fseek`, `ftell` in the codegen:** none. The code generator
  never names a C symbol except `printf`; everything else goes through a
  `nexum_*` runtime entry point, mapped at
  `crates/nexum-codegen/src/lib.rs:836-847`.
* **Dead runtime code.** `nexum_sys_mmap`, `nexum_sys_munmap` and
  `nexum_sys_exit` are emitted on every POSIX target
  (`crates/nexum-runtime/src/syscalls.rs:278-290`) and are called by
  nothing. They are the three wrappers the allocator and the exit path
  are going to need, already written and already paid for.

### The toolchain's own dependencies - and the line between the two

These are real, and they are **not** part of the language's zero.

| Dependency | Where | What it is |
| ---------- | ----- | --------- |
| `clang` for IR verification | `crates/nexum-toolchain/src/lib.rs:214-248` (`verify_ir`, `verify_ir_for`), called from `crates/nexum-driver/src/main.rs:1141` and `:1167` | the compiler's check that its own output is well formed. It parses, verifies and assembles the module for the *target*, not the host (`:229-231`). |
| `clang` for linking | `crates/nexum-toolchain/src/lib.rs:174-208` (`run_clang_with`), called from `crates/nexum-driver/src/main.rs:1142` and `:1168` | the step that produces the binary. D016 (`docs/language-reference/decisions.md:233-242`) moves this to phase 11g. |
| `clang` discovery | `crates/nexum-toolchain/src/lib.rs:12-19`, `:27-29`, `:133-141` | `nx doctor` reports it (`crates/nexum-driver/src/main.rs:899-926`). |
| Rust and LLVM for the compiler itself | `Cargo.toml:1-30`, `crates/nexum-driver/src/main.rs:916-918` | the compiler is a Rust program; it links the platform C library through Rust's own standard library. |
| The Rust crate set | `Cargo.toml:24-30` (`anyhow`, `thiserror`, `clap`, `serde`, `toml`, `tracing`) | the compiler's own dependencies. |
| CI installs `clang` | `.github/workflows/ci.yml:19-24` | the toolchain's dependency, on every platform. |

The distinction the programme must keep: **the *program* must stop needing
the C library; the *compiler* may keep needing `clang`.** D012 says it
explicitly - "the toolchain may still use Rust and LLVM. That is the
compiler's business, not the language's"
(`docs/language-reference/decisions.md:195-196`) - and
`docs/native-roadmap.md:58-63` says the same thing. The native linker
(phase 11g) is the last toolchain dependency in the build path, and it is a
*separate* phase from the runtime work (11a).

There is one toolchain dependency that *does* reach the program, and it is
easy to miss: **the CRT startup code.** The program exports `main`
(`crates/nexum-codegen/src/lib.rs:237-238`, `:1202-1228`), and the C
runtime's startup routine is what turns that return value into the process
exit code. So even a program whose panic path never calls `exit` still runs
C library code on the way out. This is row 15 of the table, and it is the
reason "the program links no C library" is not the same as "the program
names no C symbol".

### The C library features the language exposes without knowing it

Two features of the C library reach a Nexum program without the program
ever naming them. Both are the *runtime's*, and both are the programme's
debt.

**`printf`, through the `print` builtin.** A program writes `print(x)`.
The builtin is the language surface (`docs/standard-library.md:11`,
`crates/nexum-sema/src/lib.rs:1596-1637` for the type rules), and the
implementation behind it is `printf` with a C format string
(`crates/nexum-codegen/src/lib.rs:723-750`). The program has no idea. The
audit must say so plainly: **`printf` is the runtime's dependency, not the
program's.** But the *observable* behaviour is currently libc's: `print` of a
`Float` is `%g`, which is six significant digits, so `print(3.14159265358979)`
prints `3.14159` and `print(1.0/0.0)` prints `inf`
(`crates/nexum-driver/tests/fixtures/run/int_wrap.out`). Dropping libc
without deciding the float format would silently change what programs
print.

**`malloc`, through the collector.** A program allocates by `concat`,
`slice`, `list_push`, `read_file`, or by a compiler-generated temporary.
None of those is `malloc`; they are all `nexum_alloc`
(`crates/nexum-runtime/src/lib.rs:150`), which is
`nexum_alloc_scanned` (`:147`), which calls `malloc` at `:147`. The
reclamation path is the same in reverse: `nexum_sweep` calls `free` at
`:186`. So **`malloc` and `free` are the runtime's dependency, not the
program's**, and one allocator replaces both.

This is the important consequence for the programme's framing: the language
does not "depend on the C library" in the sense of having a C-facing
surface. It depends on it in exactly one place - the runtime - and the
runtime's C surface is 4 symbols on POSIX and 13 on Windows.

### The parts of the language that cannot be expressed without a C library today

**The absence of a way to call into C is a feature, not a gap.** The
language has no FFI:

* The keyword list is `fn type let mut if else while for in return true
  false and or not import` (`crates/nexum-lexer/src/lib.rs:342-359`). There
  is no `extern`, no `unsafe`, no `asm`.
* There is no pointer type, no `volatile`, no inline assembly - the language
  reaches the machine through none of these
  (`docs/native-roadmap.md:44-47`).
* An unknown callee is the error T006, "undefined function"
  (`crates/nexum-sema/src/lib.rs:1716`). A program **cannot** name a
  symbol it does not define.
* No `.nx` file in the repository names a C symbol (checked across
  `examples/` and `crates/nexum-driver/tests/fixtures/`).

So the language as it stands cannot express anything that *requires* a C
library - which is the point. The question is what would need one, and the
answer is: nothing in the language itself, and exactly one thing in the
programme.

* **The GPU path (phase 11h).** `docs/native-roadmap.md:107-109` decides
  that the GPU path is "a library the user links, talking the driver's
  binary protocol", and that this is how "no C FFI" and "use the GPU"
  coexist. But a *library the user links* still has to be *called*, and
  today a Nexum program has no way to call anything it did not define.
  Either the driver-facing code lives in the runtime/toolchain (a Nexum
  program reaches it through a builtin), or the language gains a narrow,
  explicit declaration of an external symbol - which is the moment the
  language stops being closed. **This is the single place where the
  programme has to make a decision, and it is not a decision this audit can
  make.** Note that the answer does not have to be "C FFI": a native
  library could be described with Nexum's own types and called through a
  declared signature, without any C ABI in the language.
* **The math builtins (phase 11c).** D015
  (`docs/language-reference/decisions.md:222-231`) already decides that the
  math functions are "implemented by us rather than taken from `libm`".
  Not a C dependency, but the same discipline: the numbers must not depend
  on the C library's version.
* **The float formatter.** See above: ours, in 11a.2.

Everything else in the roadmap - inline assembly (11b), the numeric tower
(11c), memory and layout (11d), concurrency (11e), SIMD (11f), the native
linker (11g) - is reachable without a C library, because the language has
no way to ask for one.

### The tests that would notice a regression

**There is now a gate, and it is the right shape.**
`crates/nexum-driver/tests/freestanding.rs`, added on `main` at commit
`2a193e3` ("test(runtime): the freestanding gate"), is the acceptance test
for this programme. It has four tests:

| Test | Line | What it pins |
| ---- | ---- | ------------ |
| `posix_programs_need_only_the_remaining_libc` | `:42-56` | the four POSIX targets declare exactly `["exit", "fputc", "fputs", "free", "malloc", "printf"]` |
| `windows_still_needs_the_c_library` | `:61-82` | the Windows target declares exactly the thirteen |
| `posix_programs_have_the_system_call_layer` | `:87-105` | the wrappers `@nexum_sys_read/write/open/close/lseek/exit` exist and the IR contains `asm sideeffect` |
| `the_string_routines_are_ours` | `:110-135` | `nexum_strlen/memcpy/strcmp` are present and `declare i64 @strlen`, `declare ptr @memcpy`, `declare i32 @strcmp` are absent |

The helper it uses is `external_symbols` (`:19-37`), which scans the emitted
IR for `declare ` lines.

**The gaps, and what to add.**

1. **It counts declarations, not references.** `external_symbols` reads
   `declare` lines out of the IR text (`:26-33`). A declaration with no call
   site is counted (which is why `fputs` and `fputc` are in the POSIX list
   even though the POSIX object does not carry them), and a symbol reached
   by a route the scan does not see is not counted. **Add** a test that
   compiles the IR to an object for each target and asserts the undefined
   symbol set - the same measurement made above with `clang -c` and
   `llvm-nm --undefined-only`. That test is the one that matches the
   acceptance criterion in R1, and it is the one that would notice a symbol
   creeping back in through an LLVM intrinsic (an `llvm.sqrt.f64` that
   lowers to a `sqrt` call, say).
2. **The Windows list has no `_fltused` entry.** If the new test counts
   referenced symbols, the Windows expectation must include `_fltused` for
   a program that uses floating point, or the test must use a fixture with
   no floats. `examples/fact.nx` is such a fixture today.
3. **The runtime crate has no test over the IR it emits.**
   `crates/nexum-runtime/src/lib.rs` has no `#[cfg(test)]` module at all;
   the only runtime tests are the five in
   `crates/nexum-runtime/src/syscalls.rs:294-385`, and they cover the
   system call numbers, the immediates, the stack alignment, the register
   pinning and the AArch64 staging - none of them touches the external
   symbol set. **Add** unit tests there over `runtime_ir(os, arch)`
   asserting the exact declared set per platform family, so the runtime
   crate alone catches a symbol creeping back in, without going through the
   driver.
4. **The documentation states the number.**
   `docs/native-roadmap.md:27-40` says "seventeen external symbols" (stale
   - it was seventeen before `e3e16c7`, it is six declared today) and
   `:137-140` says "six external symbols - `printf`, `fputs`, `fputc`,
   `exit`, `malloc` and `free`" (six *declared*, four *referenced*).
   `docs/runtime-abi.md:21-28` has the phase table with `malloc`/`free`,
   `printf`/`fputs`/`fputc`, `exit` and the Windows file routines all
   marked "11a.2". These three places must move with the code, or the next
   agent reads a stale number.
5. **What is already covered, and should stay covered.** The behaviour of
   the paths that are about to be rewritten is pinned by the run fixtures,
   and these are the tests that will fail if a replacement is wrong:
   * `crates/nexum-driver/tests/fixtures/run/panic.nx` / `panic.out` /
     `panic.exit` - the panic path (message to stderr, exit code 1). This
     is the test that exercises `exit` and, on POSIX, `nexum_sys_write`.
   * `crates/nexum-driver/tests/fixtures/run/io_roundtrip.nx` / `.out` -
     the file path, which on POSIX already goes through the system calls
     and on Windows goes through `fopen`/`fread`/`fwrite`/`fclose`.
   * `crates/nexum-driver/tests/fixtures/run/int_wrap.nx` / `.out` - the
     `inf` case, i.e. the float format.
   * `crates/nexum-driver/tests/fixtures/run/gc_reclaim.nx`, `gc_stress.nx`,
     `list_gc.nx`, `list_records_gc.nx` - the allocator and the collector,
     which is where a broken `malloc` replacement shows up.
   * `crates/nexum-driver/tests/backend.rs:93-138`
     (`run_outputs_match_fixtures`) runs every run fixture and compares
     stdout and the exit code, and `:60-74`
     (`ir_verifies_for_all_targets`) verifies the IR for all five targets.

### The Windows half

Windows is still on the C library, entirely. The reason is recorded:
"Windows has no direct system call instruction, and its kernel interface
lives behind `ntdll`. Reaching it without a C binding means generating our
own system call stubs - a real piece of work"
(`docs/native-roadmap.md:98-103`, and
`docs/language-reference/decisions.md:190-194`). The POSIX targets go
freestanding first; Windows follows in 11a.2.

**What the Windows program actually needs.** Stripping the thirteen symbols
down to what they are *for*, the Windows program needs five capabilities:

| Capability | Symbols it needs today | What it needs instead |
| ---------- | ---------------------- | --------------------- |
| Write to the standard output | `printf` | a write to the standard output handle |
| Write to the standard error | `__acrt_iob_func`, `fputs`, `fputc` | a write to the standard error handle |
| Terminate the process | `exit` | `NtTerminateProcess` |
| Heap | `malloc`, `free` | `NtAllocateVirtualMemory` / `NtFreeVirtualMemory` behind our own allocator - the same allocator the POSIX targets get |
| Files | `fopen`, `fclose`, `fseek`, `ftell`, `fread`, `fwrite` | `NtCreateFile`, `NtClose`, `NtSetInformationFile`, `NtQueryInformationFile`, `NtReadFile`, `NtWriteFile` |

Plus the entry point (row 15): a Windows program linked against the MSVC
CRT runs `mainCRTStartup`, and the CRT's floating-point initialisation is
what `_fltused` refers to. A program with its own entry point does not need
either.

**The order matters.** The allocator is shared: if 11a.2 writes the
allocator against a `mmap`-like interface, Windows should get the same
allocator over `NtAllocateVirtualMemory`, rather than a second allocator.
The output formatter is shared too. So the Windows stubs are the *only*
Windows-specific work in 11a.2 - the allocator and the formatter are
written once.

**The risk.** `ntdll` stubs are syscall wrappers, not system calls: they
are calls into another library. Is that still "no C library"? The answer
the programme has already written down is yes, in spirit and in fact:
`ntdll` is the kernel's own interface, it is not a C library, and a program
that calls only `ntdll` depends on the kernel and nothing else - which is
exactly the D012 criterion. But the *stubs* have to be generated rather
than declared, because `ntdll`'s exports are not declared in a module the
way `printf` is. `docs/runtime-abi.md:30-32` says so: "The Windows build
needs its own system call stubs: its kernel interface lives behind `ntdll`,
so the stubs have to be generated rather than assembled."

---

## The design

### The shape of the end state

A Nexum program's linked object has **no undefined symbols** other than
the ones the kernel interface provides (on POSIX: none at all, since the
system call instruction is inline assembly; on Windows: the `ntdll` stubs,
which resolve inside `ntdll` itself). The program has its own entry point.
The C library is not on the link line.

### The allocator (rows 2 and 3)

The collector's contract does not change: `nexum_alloc_scanned` returns the
payload pointer (header + 32), the object header layout is fixed
(`docs/runtime-abi.md:111-117`), and the collector is still a non-moving
mark-sweep (`docs/runtime-abi.md:118-136`). What changes is where the
memory comes from.

* **Backing store.** `nexum_sys_mmap` (`crates/nexum-runtime/src/syscalls.rs:278-288`)
  already exists and is already correct on Linux and macOS: an anonymous
  mapping, `fd = -1`, offset `0`. The Windows counterpart is
  `NtAllocateVirtualMemory` with `MEM_COMMIT`.
* **Free list.** A non-moving collector does not need the kernel to free
  memory: a swept block goes on a free list and is handed out again.
  `munmap` is then only needed for a whole-arena release, which the staged
  arena model (D009, `docs/runtime-abi.md:205-217`) will want later.
* **The watermark.** `@nexum_threshold` is 65536 bytes
  (`crates/nexum-runtime/src/lib.rs:108`). With our own allocator the
  watermark should be a *count of bytes since the last collection*, which
  it already is; what changes is that the arena grows in chunks
  (`nexum_sys_mmap` with a fixed chunk size), so a collection happens
  between chunks, not inside one.
* **The consequence for `heap_used()`.** Unchanged: it reports
  `@nexum_live`, the sum of the live payload sizes
  (`crates/nexum-runtime/src/lib.rs:234-236`). Freed-and-reused bytes are
  not live, so the number keeps meaning what it means today.

### The output formatter (row 1)

Four runtime entry points, named as `docs/language-reference/grammar.md:263-264`
already names them:

* `nexum_print_int(i64)` - the decimal digits, plus the sign for negatives.
  `printf`'s `%lld` and a hand-written formatter agree on the digits; the
  only difference is the `INT64_MIN` case, which must not overflow.
* `nexum_print_bool(i1)` - `true` prints `1`, `false` prints `0`, matching
  today's `%d` (`crates/nexum-codegen/src/lib.rs:742`) and the fixtures.
* `nexum_print_str(ptr)` - the bytes up to the NUL, written as one
  `nexum_sys_write` on POSIX.
* `nexum_print_float(double)` - **the decision**: reproduce `%g` exactly,
  or specify a Nexum format.

Then the codegen changes from one `printf` call
(`crates/nexum-codegen/src/lib.rs:747-750`) to a call to the matching
`nexum_print_*`, and the format-string globals and `get_fmt_global`
(`:211-226`) become dead.

**The float format, spelled out.** `%g` is: the shorter of `%e` and `%f`
with the trailing zeros removed, six significant digits by default, and
the exponent form when the exponent is below -4 or at least the precision.
Concretely, on the reference libc: `3.14159265358979` -> `3.14159`,
`0.1` -> `0.1`, `1.0/0.0` -> `inf`, `100000000.0` -> `1e+08`,
`1.0e20` -> `1e+20`, `0.000001` -> `1e-06`. Reproducing that is possible
but it is a *lossy* format, and the roadmap's own direction (D015, the
numeric tower) points the other way. The recommendation is to specify a
Nexum float format - shortest round-trip, the format that prints back to
the same double - and to state it in the language reference, then update
`crates/nexum-driver/tests/fixtures/run/int_wrap.out`. The only fixture
that depends on the float format today is the `inf` line, and `inf` is
format-independent.

### The exit path (rows 4 and 15)

Two separate things, and they must not be confused:

* **The panic path.** `nexum_panic` (`crates/nexum-runtime/src/lib.rs:116-126`)
  writes the message and the newline, then terminates. On POSIX it becomes
  `nexum_sys_exit`, which already exists
  (`crates/nexum-runtime/src/syscalls.rs:290`) - but on Linux the number
  in the table (60) is `exit`, which terminates the *thread*; the process
  needs `exit_group` (231). The number table
  (`crates/nexum-runtime/src/syscalls.rs:60-72` and `:73-88`, and
  `docs/runtime-abi.md:56-65`) needs a second number, and the two systems
  differ: Linux has `exit_group`, Darwin's `exit` (1) is already
  process-wide.
* **The normal path.** `main` returns an `i32`
  (`crates/nexum-codegen/src/lib.rs:1202-1228`), and today the C runtime
  turns it into the process exit code. A freestanding program has an
  entry point that calls `main` and then exits with its value. That entry
  point is also the piece that lets the program link with no C runtime at
  all (`-nostdlib` and friends), which is what removes `_fltused` on
  Windows and the CRT startup on POSIX.

### The Windows stubs (rows 5-13)

Generated, not declared. Four things have to be got right for each call:

* **How to reach `ntdll`.** The program's PE import table can name
  `ntdll.dll` and the `Nt*` entry points directly: the Windows loader
  resolves them and fills the import address table. That is the OS loader,
  not a C library, and it needs no `GetProcAddress` and no `kernel32`.
* **The calling convention.** Windows x64: the first four arguments in
  `rcx`, `rdx`, `r8`, `r9`; the result in `rax`; and the 32-byte shadow
  space the caller reserves.
* **The system call number.** The `ntdll` stubs encode a number that moves
  between Windows releases, so a stub must not hard-code it. The robust
  shapes are: resolve the export by name (letting the PE loader do it, or
  walking `ntdll`'s export table) and call the `ntdll` export, which is a
  call into `ntdll` rather than a system call; or read the number out of
  the bytes of the resolved stub.
* **The argument packing**, including the `UNICODE_STRING` structures the
  object-manager calls take.

There is a separate research brief in flight for the Windows system calls
(`work/windows-syscalls`); this audit should not duplicate it, and should
point at it instead. What this audit owns is the *list* of capabilities the
Windows program needs (rows 5-13), which is the acceptance criterion that
brief has to satisfy.

### The gate

Extend `crates/nexum-driver/tests/freestanding.rs` with the
reference-count test, and add the per-platform assertions to the runtime
crate. The acceptance criterion is a number that only goes down:

```
POSIX, referenced:   4 today  -> 0
Windows, referenced: 13 today -> 0
```

---

## Risks

1. **The float format changes observable output.** Replacing `%g` with a
   shortest-round-trip formatter changes what `print` writes for floats.
   Only one fixture line depends on it today (`inf`), but a user-visible
   behaviour changes silently. *Mitigation:* decide the format explicitly,
   state it in `docs/language-reference/grammar.md` and
   `docs/standard-library.md:11`, and update the fixture in the same
   commit.
2. **The collector's soundness depends on the allocator being correct.**
   The allocator is the only place where a bug turns into a use-after-free
   rather than a wrong answer. The GC fixtures
   (`gc_reclaim.nx`, `gc_stress.nx`, `list_gc.nx`, `list_records_gc.nx`)
   and the self-hosted lexer differential test
   (`crates/nexum-driver/tests/backend.rs:143-194`) are the safety net, and
   they must pass before the allocator lands.
3. **The Windows stubs are the largest piece of work in this table** and
   they are the least like the POSIX work: they are calls into another
   library, and the system call numbers move between Windows releases. A
   stub with a wrong number fails in a way that looks like a bug in the
   runtime, not in the stub.
4. **The entry point is easy to forget.** Replacing `exit` in the panic
   path and nothing else leaves the *normal* exit path going through the
   CRT. A program would then link no C symbols and still depend on the C
   runtime. The gate must check the entry point, not only the symbol set.
5. **`_fltused` is not ours.** It appears in the Windows object because
   clang emits it for the CRT. It is not a call the runtime makes, and a
   future gate that counts it as a dependency would be wrong. The gate
   should either exclude it explicitly or use a float-free fixture.
6. **The declaration/reference gap could hide a regression.** As long as
   the gate only counts `declare` lines, a symbol that arrives through an
   LLVM intrinsic (which lowers to a libm or libc call without a `declare`
   line in our text) is invisible. This is not hypothetical: the numeric
   tower (11c) will introduce `llvm.sqrt.f64` and friends, and on some
   targets those lower to `sqrt` calls.
7. **Linux's `exit` is not `exit_group`.** Using `nexum_sys_exit` as it
   stands would terminate the thread and leave the process in a strange
   state. This is the kind of mistake that passes a "does it exit?" test
   and fails in production.
8. **The count is a moving target in the wrong direction.** Every new
   runtime feature that wants a C library function - a float formatter, a
   math routine, a hash - is an opportunity to add a symbol back. The
   gate only stops that if it is run on every change, which is why it is a
   test and not a document.

---

## Open questions

1. **What is the float format?** *Recommendation:* shortest round-trip
   (print the shortest decimal string that reads back as the same
   `double`), stated in the language reference, with `inf`/`nan` spelled
   out. It is what Rust, Go and JavaScript do, it is lossless, and it
   replaces a format that loses digits. The cost is a changed fixture.
2. **Does the gate count declarations or references?**
   *Recommendation:* references, for the acceptance criterion (R1), with
   the declaration list kept as a secondary assertion so that a dead
   declaration does not accumulate either. Two numbers, both visible.
3. **Who owns the Windows stubs?** There is already a
   `research/windows-syscalls` brief in flight. *Recommendation:* that
   brief owns the stub *mechanism* (the calling convention, the number
   discovery, the string packing); this audit owns only the *list* of
   capabilities the Windows program needs (rows 5-13), which is what the
   other brief must satisfy.
4. **Does the language stay closed?** The GPU path (11h) needs a program
   to reach a native library. *Recommendation:* the language stays closed;
   the driver-facing code is reached through a builtin, the way `print` and
   `read_file` are. If the orchestrator prefers a declared external
   symbol, that is a language change with a numbered decision (D0xx) and
   a grammar change, and it should be taken as its own phase, not as a
   side effect of the runtime work.
5. **Is the allocator shared between POSIX and Windows?**
   *Recommendation:* yes - one allocator, two backing stores
   (`mmap`/`NtAllocateVirtualMemory`). Two allocators would mean two
   collector audits.
6. **Does the entry point land in 11a.2 or 11g?**
   *Recommendation:* the *program* stops depending on the CRT startup in
   11a.2, because that is what makes "no C runtime on the link line"
   true; 11g then replaces `clang` as the linker, which is the toolchain's
   half of the same change.
7. **What is the exact acceptance artifact?**
   *Recommendation:* a single test in
   `crates/nexum-driver/tests/freestanding.rs` that, for each of the five
   targets, compiles the IR to an object and asserts the undefined symbol
   set is empty, with the four-symbol and thirteen-symbol lists kept as
   the "before" values in the commit that closes them. The programme is
   done when that test passes for all five targets.
