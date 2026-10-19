# Linking a Nexum program without clang

**Question.** How can a Nexum program be linked without `clang`, so that the
last toolchain dependency in the build path goes away (phase 11g)?

**Summary.** Phase 11g should be: **split the build into a compile step
(clang, unchanged) and a link step that the Nexum toolchain drives itself**,
implemented as a small, unit-testable link-line builder over the platform's
system linker, with Linux x86-64 first. Clang is only a driver; the linker is
behind it, and the link line is where the ABI of the program is decided
(D016) - the entry point, the C runtime objects, the C library, the dynamic
loader. Taking it over is what makes a libc-free program possible, and it
fixes a live bug: today `nx build --target linux-aarch64` on an x86-64 host
silently produces an x86-64 Windows binary (measured below). Going further is
not worth it yet: linking LLVM's lld as a library is forbidden by rule 7 and
the only crate that does it pins LLVM 14, and writing our own ELF writer is a
multi-thousand-line project that removes the *system linker* from the build but
not clang - and it only becomes tractable once the runtime is freestanding
(11a.2), so the binary can be static and loader-free. The build cannot stop
depending on LLVM before 11j in any case: the IR-to-machine-code half of the
step is a compiler backend, not a linker, and no linker can replace it.

## Recommendation

1. **Split `run_clang_with` into `compile_to_object` and `link_object`, and
   write the link step ourselves.** Reason: the link line is a decision about
   the program's ABI, and today that decision is clang's, made by flags we
   pass through a driver we do not control. D016
   (`docs/language-reference/decisions.md:233-242`) says the program is
   linked by the Nexum toolchain; that means the link *line*, not that we must
   write a linker from scratch.
2. **Make the link line a pure function** - `fn link_plan(target, mode,
   paths) -> Vec<String>` - with no I/O in it, and unit-test it per platform.
   Reason: it is the only part of this work that can be tested without
   building a program, and it is where the platform knowledge lives. A
   golden test per platform (Linux, macOS, Windows) pins the exact argv.
3. **Pass the target to the compile step and refuse to link a foreign object
   with a host linker.** Reason: this is a real, reproducible bug today
   (measured below), and it is the cheapest part of 11g. The toolchain must
   never produce a binary whose machine type is not the one it was asked for.
4. **Delete the separate `verify_ir` step from the build path; compile once.**
   Reason: `verify_ir` compiles the IR to an object and throws it away, and
   then `run_clang` compiles the same IR again. The compile step *is* the
   verification (same parser, same verifier, same backend). Keeping the
   LLVM-level check in the test suite, where clang is already available, is
   enough. This halves the build's LLVM work and removes one clang
   invocation.
5. **Add a cheap in-process structural check of the emitted IR** (balanced
   blocks, every function has a terminator, every callee is declared or
   defined, no `nsw`, the target triple is present). Reason: it needs no
   external tool, it runs on every build, and it catches the class of bug
   that matters - a codegen regression - without LLVM. It does **not**
   replace LLVM's verifier; it complements it.
6. **Do not adopt lld as a Rust library in 11g.** Reason: rule 7 (no new
   dependencies) forbids it, and independently it is a bad trade - the only
   crate (`lld_rs`) pins LLVM 14, was last published in January 2023, and
   would replace "clang on the PATH" with "LLVM 14 development files on the
   machine", which is strictly heavier. It also does not let us drop clang:
   the object file still has to come from somewhere.
7. **Do not write our own object writer / linker in 11g.** Reason: it is a
   2-3 week project for the static case and a second one for the dynamic
   case, it removes the *system linker* from the build but not clang, and it
   is only tractable once 11a.2 makes the binary libc-free and static.
   Revisit it after 11a.2, as part of 11j.
8. **Treat the freestanding configuration as a mode of our linker, not as a
   change of phase.** Reason: the moment the runtime stops declaring
   `printf`/`malloc`/`exit` (11a.2), "no C library, our own `_start`, static,
   no dynamic loader" becomes a one-line change in *our* code rather than a
   flag we hope clang passes through correctly. That is the real deliverable
   of 11g.

The files this touches, the work it implies and the tests that would prove it
are in "The design" below.

## What the code looks like today

### The build path

The whole build is two calls into `clang`:

* `crates/nexum-toolchain/src/lib.rs:178-208` - `run_clang_with` runs
  `clang -Wno-override-module -O<n> [-flto=thin -fuse-ld=lld]
  [--sysroot=..] [-fprofile-generate | -fprofile-use=..] <file.ll> -o <out>`.
  One process does all three jobs: it parses the IR, runs the codegen, runs
  the assembler, and runs the linker.
* `crates/nexum-toolchain/src/lib.rs:221-248` - `verify_ir_for` runs
  `clang [-Wno-override-module] [--target=<triple>] -x ir -c <file.ll> -o
  <file.ll>.verify.obj` and deletes the object. It is the second compile of
  the same IR.
* `crates/nexum-driver/src/main.rs:1128-1151` (`build_file`) and
  `:1160-1176` (`build_to`) call `verify_ir_for(... Some(&target.triple))`
  and then `run_clang_with(...)`. The IR is written to the fixed path
  `std::env::temp_dir().join("nexum_out.ll")`, so two builds in parallel
  clobber each other's IR and object.
* `crates/nexum-toolchain/src/lib.rs:12-23` - `Toolchain::discover` finds
  `clang` and nothing else; `linker_kind` is the string
  `"system (clang default)"`. The toolchain does not know which linker it
  used, and cannot choose one.
* `crates/nexum-driver/src/main.rs:899-926` (`doctor`) and
  `crates/nexum-driver/src/setup.rs:265-279` check for `clang` only.

### What the code generator produces

* `crates/nexum-codegen/src/lib.rs:109-175` - `emit` produces **one** LLVM IR
  text module: `target triple` (`:112`), `target datalayout` (`:113-116`),
  the runtime IR spliced in as `internal` definitions (`:119`), the record
  types, `private unnamed_addr constant` string globals (`:162-167`), and the
  functions.
* `crates/nexum-codegen/src/lib.rs:237-241` and `:1202-1229` - `main` is
  `define i32 @main()` - zero arguments, C return type. There is no `_start`,
  no `mainCRTStartup`, and no other entry point anywhere in the output.
* `crates/nexum-runtime/src/lib.rs:49-54` - the runtime declares six
  external symbols on POSIX: `printf`, `fputs`, `fputc`, `exit`, `malloc`,
  `free`. On Windows it also declares `__acrt_iob_func`, `fopen`, `fclose`,
  `fseek`, `ftell`, `fread`, `fwrite` (`:60-74`).
* `crates/nexum-target/src/lib.rs:39-73` - five targets are known:
  `windows-x86_64`, `linux-x86_64`, `linux-aarch64`, `macos-x86_64`,
  `macos-aarch64`.

So the program that reaches the linker today is: one object file, six
undefined libc symbols, an entry point named `main`, and nothing else.

### What clang actually does (measured on this machine)

`clang -###` prints the two processes clang runs. For the three platforms
(LLVM 23.1.3):

**Linux (ELF)** - `clang --target=x86_64-unknown-linux-gnu`:

```
clang -cc1 -triple x86_64-unknown-linux-gnu -emit-obj ... -o out.o -x ir file.ll
ld.lld --hash-style=gnu --eh-frame-hdr -m elf_x86_64 -pie \
       -dynamic-linker /lib64/ld-linux-x86-64.so.2 -o out \
       Scrt1.o crti.o crtbeginS.o out.o \
       -lgcc --as-needed -lgcc_s --no-as-needed -lc \
       -lgcc --as-needed -lgcc_s --no-as-needed crtendS.o crtn.o
```

**macOS (Mach-O)** - `clang --target=x86_64-apple-darwin`:

```
clang -cc1 -triple x86_64-apple-macosx10.4.0 -emit-obj ... -o out.o -x ir file.ll
ld64.lld -demangle -dynamic -arch x86_64 -platform_version macos 10.4.0 10.4.0 \
          -mllvm -enable-linkonceodr-outlining -o out -lcrt1.o out.o -lSystem -lgcc_s.1
```

**Windows (PE/COFF)** - `clang` on the host triple:

```
clang -cc1 -triple x86_64-pc-windows-msvc19.44.35222 -emit-obj ... -o out.o -x ir file.ll
lld-link -out:out.exe -defaultlib:libcmt -defaultlib:oldnames \
         -libpath:<VC Tools MSVC lib x64> -libpath:<Windows Kits ucrt x64> \
         -libpath:<Windows Kits um x64> -nologo out.o
```

That is the whole job, spelled out: the first line is the code generator (not
a linker), the second line is the link.

### The object the linker receives

`llvm-nm`/`llvm-objdump` on the object clang produces from the current
`hello.nx` IR, for `linux-x86_64` at `-O2`:

```
RELOCATION RECORDS FOR [.text]:
0000000000000001 R_X86_64_PLT32   strlen-0x4
0000000000000144 R_X86_64_PC32    .Lstr_0-0x4
0000000000000149 R_X86_64_PLT32   puts-0x4
000000000000015c R_X86_64_PLT32   printf-0x4
...
Sections: .strtab .text .rela.text .rodata.cst8 .rodata.str1.1
          .note.GNU-stack .llvm_addrsig .symtab
Undefined: printf, puts, strlen
```

Three things in there matter for this phase:

* the relocation types the link step has to resolve - `R_X86_64_PLT32` for
  calls, `R_X86_64_PC32` for addresses of data;
* `.note.GNU-stack` (the executable-stack marker) and `.llvm_addrsig` (an
  LLVM-specific section a linker must know about);
* **the optimizer introduces libc calls of its own.** `nexum_strlen` was
  rewritten by LLVM into a tail call to the C library's `strlen` (the
  relocation at offset 1 is inside `nexum_strlen`), and `printf("...\n")`
  became `puts`. `-ffreestanding`, `-fno-builtin` and a `nobuiltin` attribute
  on the definition did **not** prevent it (measured; the rewrite comes from
  LLVM's library-call recognition, not from the driver's builtin flags). A
  libc-free build therefore needs the C-named routines (`memcpy`, `memset`,
  `memmove`, `memcmp`, and `strlen` in practice) supplied by the runtime, or
  the recognition defeated.

### The bug this phase has to fix

On this x86-64 Windows host:

```
> nx build --target linux-aarch64 examples/hello.nx -o hello.exe
built: C:\...\hello.exe
```

and the file is a **PE, machine 0x8664 (x86-64)**, containing "Hello, Nexum!".
The module said `target triple = "aarch64-unknown-linux-gnu"`. Two things
happened:

1. `run_clang_with` does not pass `--target`, so clang used the *host*
   triple. The mismatch would have been reported as
   `warning: overriding the module target triple with x86_64-pc-windows-msvc`
   - and `-Wno-override-module` (`nexum-toolchain/src/lib.rs:185`) suppresses
   exactly that warning.
2. At `-O2` the unused `internal` aarch64 syscall wrappers were dead-code
   eliminated before the backend saw them, so the aarch64 inline assembly
   (`asm sideeffect ... "={x0},..."`) never reached the x86-64 backend and no
   error was raised. At `-O0` the same build fails with
   `error: could not allocate output register for constraint '{x0}'`.

So the toolchain can emit IR for five targets, can verify it for five targets
(`crates/nexum-driver/tests/backend.rs:61-74`), and can link for exactly one
- and it does not know which one it linked for. A toolchain that owns the
link step cannot make that mistake.

## The design

### The build step has two halves, and only one of them is a linker

```
  Nexum source
      |  (front end, MIR)
      v
  LLVM IR text  ------------------->  LLVM's code generator  --->  .o
  (nexum-codegen)                       (clang -cc1, or LLVM's        (ELF/COFF/Mach-O
                                         libraries, or 11j's own)       relocatable)
                                                                         |
                                                                         v
                                                            the linker (ld / lld-link /
                                                            ld64)  --->  executable
```

The left half is a **compiler backend** (instruction selection, register
allocation, calling conventions, the inline assembler). The right half is
**the linker**, and it is the right half that D016 is about.

The consequence for the phase: **the build cannot stop depending on LLVM
before 11j**, because the left half has no substitute that is not a compiler
backend. What 11g can do is take the right half away from clang's driver and
put it in our code. Everything below assumes that.

### What the link step actually has to do

Six jobs, in the order a linker does them.

**1. The object file format.** The input is a relocatable object: ELF
`ET_REL` on Linux, Mach-O `MH_OBJECT` on macOS, COFF (`.obj`) on Windows. The
output is an executable: ELF `ET_EXEC` or `ET_DYN` (PIE), Mach-O
`MH_EXECUTE`, PE32+ `.exe`. The toolchain does not have to *write* an object
file to do this (clang writes it), but it does have to know the format, because
the format determines the flags: `-m elf_x86_64` vs `-arch arm64` vs
`/subsystem:console`.

**2. The entry point.** The kernel does not call `main`.

* Linux: `Scrt1.o` provides `_start`, which sets up the stack and calls
  `__libc_start_main(main, argc, argv, ...)`. A libc-free program replaces
  all of that: it provides its own `_start` (in the runtime, in inline
  assembly, the way D013 already does for the system calls), which reads
  `argc`/`argv`/`envp` off the stack, aligns it, calls `main`, and exits with
  `exit_group` (syscall 231 on Linux).
* macOS: the kernel always starts `dyld`, and `dyld` calls `main`. macOS has
  no static executables - the dynamic loader is not optional - so "no C
  library" on macOS means "no `libSystem`", not "no dynamic loader". The
  program is still a `dyld` client.
* Windows: the PE entry point is `mainCRTStartup` (from the UCRT) or one of
  ours. Windows also has no direct system call instruction, so the program
  needs `ntdll` stubs (11a.2) before it can be CRT-free.

**3. The runtime libraries.** This is where the freestanding work lands.
Today the program needs the C library (six symbols on POSIX). The link line
decides that: `-lc` and the CRT objects in the line above are what pull it
in. A freestanding link line has `-nostdlib` and no CRT objects, and the
program provides its own `_start` and its own `memcpy`/`memset`/... .

**4. The symbol table.** The linker resolves symbols between the input
objects and the libraries. It writes `.symtab`/`.strtab` into the output
(and `.dynsym`/`.dynstr` plus `.hash`/`.gnu.hash` in a dynamic build). The
kernel does not read `.symtab`; it is there for debuggers and `nm`. In a
static freestanding build, `.symtab` is the only symbol table, and it is
optional.

**5. The relocations.** The relocation types the current object uses are
`R_X86_64_PLT32` (a call to a function) and `R_X86_64_PC32` (the address of
a data item). A full x86-64 program also needs `R_X86_64_64`,
`R_X86_64_32S`, `R_X86_64_GOTPCREL*` (in PIC code), `R_X86_64_TPOFF64` (TLS);
AArch64 needs `R_AARCH64_ABS64`, `R_AARCH64_CALL26`, `R_AARCH64_JUMP26`, and
the paired `R_AARCH64_ADR_PREL_PG_HI21` + `R_AARCH64_ADD_ABS_LO12_NC`. In a
static build, every one of them is resolved at link time and none of them
survives into the output.

**6. The sections.** A dynamic Linux executable has roughly twenty:
`.interp`, `.note.ABI-tag`, `.note.gnu.build-id`, `.hash`, `.gnu.hash`,
`.dynsym`, `.dynstr`, `.gnu.version`, `.rela.dyn`, `.rela.plt`, `.init`,
`.fini`, `.init_array`, `.fini_array`, `.plt`, `.got`, `.got.plt`, `.text`,
`.rodata`, `.eh_frame_hdr`, `.eh_frame`, `.data.rel.ro`, `.data`, `.bss`,
`.dynamic`, `.comment`, `.symtab`, `.strtab`, `.shstrtab`, plus
`PT_LOAD`/`PT_INTERP`/`PT_DYNAMIC`/`PT_GNU_RELRO`/`PT_GNU_STACK` program
headers. A static freestanding Linux executable has four sections and two
segments. Measured, by linking the current object with
`ld.lld -nostdlib -static -e main --no-dynamic-linker`:

```
Program headers: PT_PHDR, PT_LOAD (R E), PT_LOAD (RW), PT_GNU_STACK
Sections:        .rodata .text .comment .symtab .shstrtab .strtab
```

No `.interp`, no `.dynamic`, no `.dynsym`, no `.plt`, no `.got`, no
`.rela.dyn`. **That is the difference between "writing a linker" and "writing
a linker": the dynamic case is where the real work is, and the freestanding
static case is what makes it possible.**

### Option A - our own link line over the system linker (recommended)

**What it is.** The toolchain builds the linker's argument vector itself and
runs the linker. Concretely:

```
crates/nexum-toolchain/src/link/mod.rs     new: the plan builder and the modes
crates/nexum-toolchain/src/link/linux.rs   new: the ELF link line
crates/nexum-toolchain/src/link/macos.rs   new: the Mach-O link line
crates/nexum-toolchain/src/link/windows.rs new: the COFF link line
crates/nexum-toolchain/src/lib.rs          changed: compile_to_object + link_object
```

The core data structure is a pure function, so it can be unit-tested without
building anything:

```rust
pub enum LinkMode {
    /// Hosted: the C library and the platform's startup objects. What the
    /// toolchain does today, minus clang's driver.
    Hosted,
    /// Freestanding: no C library, our own `_start`, static, no loader.
    Freestanding,
}

pub struct LinkPlan {
    pub linker: String,        // "ld.lld", "ld64.lld", "lld-link", "ld", ...
    pub argv: Vec<String>,     // the full command line
    pub entry: Option<String>, // "-e" / "/entry:"
}

pub fn link_plan(target: &TargetSpec, mode: LinkMode, paths: &ToolPaths)
    -> Result<LinkPlan, String>;
```

`LinkMode::Hosted` produces, for Linux x86-64, the line measured above
minus the parts that are clang's business. `LinkMode::Freestanding` produces
the line measured above: `-nostdlib -static -e _start --no-dynamic-linker`,
plus `-fno-stack-protector` at the *compile* step (Ubuntu's clang defaults to
`-fstack-protector-strong`, which emits calls to `__stack_chk_fail` - a libc
symbol that a libc-free program cannot have).

**Where the paths come from.** The CRT objects (`Scrt1.o`, `crti.o`,
`crtn.o`, `crtbeginS.o`, `crtendS.o`) live in the C library's development
package and their location is distribution-specific. Two ways to get them:
ask the compiler driver (`clang -print-file-name=Scrt1.o`, which is how `rustc`
asks `cc` for the same information), or find them ourselves. Asking clang is
the pragmatic choice and keeps clang where it already is - as the code
generator - while our code owns the line. In `Freestanding` mode there are no
CRT objects to find: the runtime provides `_start`, so the problem disappears
rather than being solved.

**Which linker.** In order: `ld.lld` (ELF, and multi-architecture:
`-m aarch64linux`), `ld64.lld` (Mach-O: `-arch arm64`), `lld-link`
(COFF), then the platform's own `ld`/`link.exe`. Preferring lld matters for
cross-compilation: GNU `ld` is single-target, lld is not. LLVM's own
installers ship lld next to clang (`ld.lld`, `ld64.lld`, `lld-link`,
`wasm-ld` are all in `C:\Program Files\LLVM\bin` on this machine), so using it
does not add an installation requirement for a user who already has clang -
though a `apt install clang` on Debian does *not* pull in `lld`, so `nx
doctor` has to report it.

**What it costs.**

* ~400-600 lines of Rust for the three platforms, plus the per-platform
  knowledge (the CRT object names, the dynamic loader path, the subsystem
  flag, the search paths).
* We take over clang's driver logic: the CRT objects, `-lc`, the interpreter
  path, `--eh-frame-hdr`, the hash style, the PIE default, the `-z relro`
  and `-z now` defaults, the MSVC/UCRT search paths. Each of those is one
  line in our plan and one more thing to keep up to date when a platform
  changes.
* `-flto=thin` stops working: the object produced by `clang -c -flto=thin`
  is **LLVM bitcode**, not machine code (measured: the file starts with
  `42 43 C0 DE`, the bitcode magic, and `ld.lld` rejects it - `error:
  tiny_thin.exe.lto.tiny_thin.o: not an ELF file`). Thin LTO needs an
  LTO-aware linker, which means either keeping the link inside clang or
  requiring lld specifically. The same applies to `-fprofile-generate` at
  link time, which pulls in the compiler-rt profile runtime.
* It does **not** remove clang from the build. The code generator is still
  clang.

**What it buys.**

* The ABI decision moves into our code (D016). When 11a.2 lands, "no C
  library, our own `_start`, static" is a change in `link_plan`, not a flag
  we hope clang forwards.
* The target is explicit end to end, and the foreign-target bug cannot
  happen.
* The link line becomes testable: a pure function with golden tests.

### Option B - lld as a Rust library

The facts, from the crates.io API:

| | |
| --- | --- |
| Crate | `lld_rs` (the `lld` crate was renamed) |
| Version | 140.0.0, published 2023-01-04 - the last of three versions |
| Licence | MIT OR Apache-2.0 (compatible with the project's Apache-2.0) |
| Normal dependencies | `llvm-sys ^140.0.3`, `libc ^0.2.138` |
| Build dependencies | `cc`, `lazy_static`, `semver` |
| What it does | binds LLVM 14's `lld` and drives it in-process |

**It is ruled out by rule 7** ("No new dependencies"), and it would fail the
cost test anyway:

* `llvm-sys 140` binds **LLVM 14** (released 2021). `llvm-sys` links against
  an LLVM installation found through `llvm-config`, so a user would need LLVM
  *14* development files, while the `clang` they already have is LLVM 17, 18,
  19, 20 or 23. `llvm-sys 140` will not build against any of those. The
  crate has not been updated since January 2023.
* It replaces "a `clang` binary on the PATH" with "an LLVM 14 development
  installation", which is a strictly heavier requirement: headers, static
  libraries, `llvm-config`, and a version pin that will not track the user's
  clang.
* It makes the `nx` binary enormous (LLVM's libraries are linked in).
* And it does not solve the problem on its own: lld links *objects*, and the
  object still has to come from somewhere. Getting it without the `clang`
  binary means `llvm-sys` anyway - the same dependency, with the same version
  pin.

There is a variant worth naming for the record: **the `object` crate**
(0.40.0, MIT OR Apache-2.0) reads and writes ELF, PE and Mach-O. It is the
obvious shortcut for option C, and it is also a new dependency, so option C
under rule 7 means hand-rolling the format.

### Option C - our own object writer and linker

**What it means.** Read the relocatable object the code generator produced
(sections, symbols, relocations), lay out the segments, apply the
relocations, and write the executable: the ELF header, the program headers,
the section headers, `.symtab`/`.strtab`/`.shstrtab`.

**The cost, honestly split in two.**

* *The static freestanding case* - one object, no libraries, no dynamic
  loader, four sections, two program headers, and a handful of relocation
  types (`R_X86_64_64`, `R_X86_64_PC32`, `R_X86_64_PLT32`, and the AArch64
  equivalents). This is a real project: 1500-2500 lines of careful Rust, a
  week of bugs around section alignment, symbol scoping (local vs global vs
  weak), `SHN_COMMON`, and the `.note.GNU-stack` merge. It is *doable*.
* *The dynamic case* - the one the program needs today - is a second project
  of comparable size: `.dynsym`/`.dynstr`, `.hash` and `.gnu.hash`, the
  `.rela.dyn`/`.rela.plt` sections, the PLT and the GOT, the `DT_*` dynamic
  array, copy relocations, `DT_NEEDED`, the `PT_INTERP` segment. This is
  where the bugs live, and none of it is needed for a libc-free program.

**What it removes and what it does not.** It removes the *system linker*
(`ld`, `lld-link`, `link.exe`) from the build. It does **not** remove clang:
the object still comes from the code generator. So on its own, option C
cannot deliver "no clang in the build" either.

**When it is worth it.** When the binary is static and libc-free, option C
becomes small, and it is a prerequisite for 11j (a self-hosted compiler that
emits its own objects needs a linker that consumes them). So the right time
is *after* 11a.2, not in 11g.

**The trap.** The task framing - "writing our own ELF writer, which means
writing an object file, a relocation model and a symbol table from scratch" -
understates where the difficulty is. Writing the ELF container is the easy
20%. The hard 80% is the code generator: the IR-to-machine-code backend that
produces the object in the first place. Option C does not touch that, and
nothing in the linker can substitute for it.

### Verification: what replaces `verify_ir`

`verify_ir` is not a linker question, and it is not really a clang question
either. It is an **LLVM** question.

What it does today (`nexum-toolchain/src/lib.rs:221-248`): `clang -x ir -c`
runs the IR parser, the IR verifier, the target's code generator (which is
where "could not allocate output register for constraint '{x0}'" comes from),
and the assembler. It is a full compile to a throwaway object.

The options:

1. **Keep `clang -x ir -c`.** Correct, and free. It stays the LLVM-level
   check.
2. **Replace it with `opt -passes=verify`.** Worse: it is still an LLVM
   binary, and it checks less (no backend, no assembler).
3. **Link the toolchain against LLVM's libraries (`llvm-sys`) and call
   `LLVMVerifyModule`.** This is the real replacement - but it is a new
   dependency, which rule 7 forbids, and it needs an LLVM development
   install at build time. This is a decision for the phase that adopts LLVM's
   libraries (11j, or whenever the compiler stops shelling out), not for 11g.
4. **Write our own IR checker.** Cheap and genuinely useful, but it is not
   the LLVM verifier: it can check the shape of the module our codegen
   produces (balanced blocks, every function has a terminator, every callee
   is declared or defined, the target triple and datalayout are present, no
   `nsw` on wrapping arithmetic), not LLVM's type and dominance rules.

**The recommendation:** option 4 for the build path, option 1 for the test
suite. Concretely:

* **In the build path, delete the separate verify step.** Once the build is
  split, the compile step is the verification: it runs the same parser, the
  same verifier and the same backend that `verify_ir` ran, and it produces
  an object we keep. Compiling the IR twice was pure waste. The error
  messages a user sees for a bad module are the same ones, because they come
  from the same place.
* **In the test suite, keep `verify_ir_for`.** `crates/nexum-driver/tests/backend.rs:44-74`
  verifies the IR for every fixture and for every target. That is a test, and
  a test may use clang - CI already installs it
  (`.github/workflows/ci.yml:19-24`).
* **Add the in-process structural check**, and run it on every build. It
  catches the class of failure that matters (a codegen regression) with no
  external tool and no LLVM.

### Cross-compilation

Today: the toolchain emits IR for five targets and verifies it for five
targets, and links for one - the host - without knowing it. The foreign
target build is not an error; it is a *different* binary (measured above).

With the split, cross-compilation becomes a question with an answer:

* **Compile**: `clang --target=<triple> -c` works for all five targets
  today; the LLVM install the user already has has every backend. Nothing
  changes.
* **Link**: the linker must be able to *produce* that target's format. lld
  can do all three (`ld.lld -m aarch64linux` for ELF, `ld64.lld -arch arm64`
  for Mach-O, `lld-link` for COFF x86-64 and arm64). GNU `ld` and Apple's
  `ld` are single-target, so a cross build needs a cross toolchain (a
  `aarch64-linux-gnu-gcc` installation, for instance) or lld.
* **The toolchain must know which linker it is talking to**, and it must
  refuse to produce a binary for the wrong machine. Two ways: pass the
  target to the linker explicitly (`-m elf_aarch64`, `-arch arm64`) and
  verify the result, or check the object's own machine field before linking.
  The first is what lld wants; the second is a cheap safety net that works
  with any linker.

The honest position: **after 11g, a cross build is possible for the targets
whose linker we have.** With lld present, that is all five. Without it, the
toolchain should say so plainly ("target linux-aarch64 needs a cross linker;
found only ld, which is x86-64-only") rather than silently building for the
host.

### "The last toolchain dependency": the program, or the build?

These are two different goals, and the roadmap mixes them. Sorting them out
is most of the answer to this phase.

* **The program does not need clang, and never did.** A built program is a
  finished executable; it needs the kernel and (today) the C library. What
  the program must stop needing is **the C library** - that is D012, and it
  is phase **11a.2**, not 11g. The six symbols in
  `crates/nexum-runtime/src/lib.rs:49-54` are the whole list.
* **The build needs clang for two different things.** The code generator
  (IR to machine code) and the link driver. Only the second is a linker.
  Taking the link driver away is what 11g is for. Taking the code generator
  away needs LLVM's libraries (forbidden by rule 7) or a compiler backend of
  our own (11j).

So the precise statement of what 11g delivers is:

> After 11g, the Nexum toolchain links its own programs: the entry point,
> the runtime libraries, the dynamic loader and the binary layout are decided
> by Nexum's code, not by clang's driver. Clang remains in the build as the
> code generator, and the program remains dependent on the C library until
> 11a.2 - at which point the switch to "no C library, our own `_start`,
> static, no loader" is a change in our link plan rather than a change in
> what we ask clang for.

The roadmap's own wording
(`docs/native-roadmap.md:60-63`) is close to this: "The link step calls
clang. The *program* is what must stop needing it; using lld (or our own
linker) inside the Rust toolchain is fine." The sentence mixes the program
and the build; the phase title ("the last toolchain dependency in the build
path") is the build. The link step is the last part of the build path we can
take without new dependencies, and it is worth taking.

## Risks

**R1 - The link line drifts from the platform.** We would own the CRT
object names, the interpreter path, the PIE and RELRO defaults, the MSVC and
UCRT search paths. A distribution that moves `Scrt1.o` breaks the build in a
way clang's driver would have handled. *Mitigation*: keep the plan in one
place, have `nx doctor` report which linker and which CRT objects were
found, and keep a "fall back to clang" mode behind a flag for one release.

**R2 - Thin LTO and PGO break.** `clang -c -flto=thin` produces bitcode, not
an object (measured: `42 43 C0 DE`, and `ld.lld` rejects it). `clang --lto`
at link time is what makes LTO work today. Our link step therefore has to
either require lld (which is LTO-aware) or refuse `--lto`. PGO at link time
pulls in the compiler-rt profile runtime, which our line would have to add
explicitly. *Mitigation*: implement `--lto` and `--pgo-generate` only for
the lld path first, and say so in `nx build --help`.

**R3 - The foreign-target bug is fixed, and something new appears.** Today
`nx build --target linux-aarch64` on x86-64 silently produces an x86-64
binary. After the fix it must fail with a clear message. That is a
behaviour change users may have (unknowingly) relied on. *Mitigation*: the
failure message names the target, the host, and the linker that was found.

**R4 - The freestanding link fails on symbols we did not expect.** The
optimizer introduces libc calls of its own: `nexum_strlen` became a call to
`strlen`, `printf("...\n")` became `puts` (measured), and the backend will
introduce `memcpy`/`memset` for aggregate copies. `-ffreestanding` and
`-fno-builtin` do not stop the first two (measured). *Mitigation*: the
runtime must export the C-named routines (`memcpy`, `memset`, `memmove`,
`memcmp`, and `strlen` if the recognition is not defeated), and the
freestanding mode should be tested by linking with `-nostdlib` and asserting
that the object has no undefined symbols - which is a test we can write
today, without a linker of our own.

**R5 - The stack protector.** Ubuntu's clang defaults to
`-fstack-protector-strong`, which emits calls to `__stack_chk_fail`. A
libc-free program cannot satisfy that. *Mitigation*: the freestanding
compile step passes `-fno-stack-protector`; the hosted step does not.

**R6 - Parallel builds collide.** The IR is written to the fixed path
`std::env::temp_dir().join("nexum_out.ll")`
(`crates/nexum-driver/src/main.rs:1139` and `:1165`), and the verify object
to `<ll>.verify.obj`. Two concurrent builds overwrite each other. Splitting
the build adds an object file, which makes this worse. *Mitigation*: use a
unique temporary directory per build (`std::env::temp_dir().join(format!(
"nexum-build-{}", process::id()))`, or a per-build counter).

**R7 - Option C is much bigger than it looks.** A hand-written ELF writer
that reads objects and writes executables is 1500-2500 lines before the
dynamic case, and the dynamic case is a second project. *Mitigation*: do not
start it in 11g; if it is started, do the static case only, and only after
11a.2.

**R8 - The verification gets weaker.** If the build path drops `verify_ir`
and relies on the compile step, a module that parses but is malformed in a
way the backend does not catch may slip through. *Mitigation*: the in-process
structural check covers exactly that class; and the test suite keeps the
LLVM-level verification.

**R9 - What would make this recommendation wrong.** If LLVM's libraries were
allowed as a dependency, then `llvm-sys` + `lld_rs` would collapse the two
halves into one process and the link step would be genuinely ours with
clang gone from the build path. Rule 7 is what rules that out. If rule 7 is
relaxed for the toolchain, option B becomes worth re-reading - but the LLVM
14 pin still makes `lld_rs` unattractive, and a *current* `llvm-sys` (231.x,
LLVM 21+) plus a hand-written lld binding would be the way, not `lld_rs`.

## Open questions

1. **Does rule 7 ("no new dependencies") apply to the toolchain's build-time
   dependencies, or only to the runtime dependencies of a Nexum program?**
   The brief reads "The toolchain is Rust with the crates it already has; the
   language's programs must not need the C library", which suggests both
   halves are binding. *Recommendation*: treat it as binding, and revisit it
   explicitly in the phase that adopts LLVM's libraries (11j). This decision
   is the one that separates option A from option B.
2. **Is phase 11g allowed to land before 11a.2?** *Recommendation*: yes, but
   split it. Land the split (`compile_to_object` + `link_object`), the
   target fix and the link-plan tests as 11g.1 - they are useful on their
   own and they fix a live bug. Land the freestanding mode (`-nostdlib`,
   our `_start`, static, no loader) as 11g.2, after 11a.2 has removed the six
   libc symbols, because before that the freestanding link cannot succeed
   and the mode would ship untested.
3. **Which is the first platform?** *Recommendation*: Linux x86-64, and only
   Linux x86-64. It is the platform where the runtime is closest to
   freestanding, where a static binary needs no loader, and where the
   relocatable object and the executable are the same format. macOS cannot
   have a static executable, so its freestanding mode is "no libSystem but
   still dyld"; Windows cannot be CRT-free until the `ntdll` stubs exist
   (11a.2). Linux first, the others after.
4. **Do we write our own linker later, and when?** *Recommendation*: yes,
   but after 11a.2, and as part of 11j rather than as a separate phase. The
   trigger is the self-hosted compiler: once Nexum's own backend emits
   objects, the linker that consumes them is ours too. Do not start it
   before then - it removes the system linker from the build but not clang,
   which is not the goal of 11g.
5. **Who owns the "no libc" switch: the link plan or the runtime?** Today
   the runtime decides (it declares the six symbols), so the linker has to
   discover it. *Recommendation*: the runtime states its requirements
   explicitly - a `LinkRequirements` value in the runtime crate saying "I
   need libc" or "I am freestanding" - and the link plan reads it. That
   keeps the linker from having to guess, and it makes the 11a.2 flip a
   one-line change in one place.
6. **Does the toolchain still need clang as a *process*, or only as
   libraries?** This is the question the whole phase turns on, and it is
   not answered by 11g. *Recommendation*: keep clang as a process for now;
   revisit when the compiler stops emitting LLVM IR (11j), at which point
   the code generator and the verifier both move into the process and the
   linker is the last thing standing between the toolchain and the kernel.
