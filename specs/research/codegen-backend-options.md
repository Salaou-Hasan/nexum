# The code generator's backend: textual IR, or LLVM as a library

**Question.** Should the code generator keep emitting LLVM IR as text and let
`clang` lower, assemble and link it, or should it use LLVM's libraries, build
the module in memory and run the passes itself? Phase 11g (the native linker)
depends on the answer.

**Summary.** Keep the textual IR, and take the link step away from `clang`; do
not move the code generator to LLVM as a library. The Rust bootstrap compiler
is explicitly temporary (`docs/native-roadmap.md:104-106`) and phase 11j
replaces it with a compiler written in Nexum, which cannot call LLVM's C++
API - so a Rust-only LLVM binding would be an investment in the part of the
system that is scheduled to be thrown away, and the textual IR path is the
only one that survives the self-hosting. LLVM-as-a-library is also the wrong
shape of answer to the problem it appears to solve: both of its real benefits
(the datalayout and the backend diagnostics) have cheaper direct fixes. Phase
11g does not need the codegen decision at all - a linker consumes object
files, and `clang -c` produces object files just as well as an in-process
LLVM does - so 11g should be scoped to the link step and must not be blocked
on this question. The honest accounting of "replace `clang` with the system
linker" is in [What "replace clang with the system linker" buys and
costs](#what-replace-clang-with-the-system-linker-buys-and-costs): it removes
`clang`'s *link-driver* role and makes the link step ours, and it does **not**
remove the LLVM dependency.

## Recommendation

1. **Keep the textual IR. Do not move the code generator to LLVM as a
   library.**
   The reason is the self-hosting plan. LLVM's library interface is C++; the
   C API is a thin subset of it. A compiler written in Nexum cannot call
   either: the language has no C FFI, no `extern`, no pointer type, and D012 /
   D013 deliberately keep C out of the language. A self-hosted Nexum compiler
   therefore has exactly three options - emit textual IR and shell out to a
   tool, write its own machine-code backend, or first add a C FFI to the
   language. The first is what `docs/self-hosting.md:43` (stage 10f, "Port
   codegen (LLVM IR text emission)") already plans, and the second is a
   multi-year project the programme is not doing. So the textual IR is not a
   bootstrap convenience that 11j will replace; it is the backend the
   programme ends up with. A Rust-only LLVM binding would have to be built,
   paid for, and then discarded at 11j.

2. **Do not build the hybrid.** "Use LLVM's libraries when they are available,
   fall back to `clang` otherwise" costs a second backend, a second LLVM
   version, and a build output that depends on which one the machine happens
   to have. It does not reduce the dependency, it doubles it, and it makes the
   binaries non-reproducible across machines. It is a sensible *migration
   step* towards the library, and the library is not where this programme is
   going, so it is a migration step to nowhere. The complexity is detailed
   under [The hybrid](#the-hybrid).

3. **Do not let 11g wait for this decision.** 11g is "the program is linked by
   us, not by `clang`" (`docs/native-roadmap.md:75`, D016 at
   `docs/language-reference/decisions.md:233-242`). A linker consumes object
   files. `clang -c foo.ll -o foo.o` produces an object file today, and the
   resulting object file is the same kind of artefact an in-process LLVM would
   produce. So the native linker can be built and shipped against today's
   codegen, with no change to `nexum-codegen` at all. 11g's completeness
   (no external toolchain *at all*) would depend on the codegen question; its
   scope as written does not.

4. **Take the link step away from `clang` now, in the cheap way, and then in
   11g's way.**
   *Cheap, immediately:* stop letting `clang` drive the link. Compile to an
   object file with `clang -c`, then invoke the system linker ourselves with
   an explicit command line. This is small, it satisfies D016 in its literal
   wording, and it is the step that makes the link line inspectable instead of
   hidden inside `clang`'s driver.
   *In 11g:* replace the system linker with our own, which is the real
   deliverable and the one that makes the freestanding goal reachable.
   See [What "replace clang with the system linker" buys and
   costs](#what-replace-clang-with-the-system-linker-buys-and-costs) for what
   each half buys and costs.

5. **Fix the datalayout drift now, with a test, rather than by adopting a
   dependency.** The hand-written datalayout strings in
   `crates/nexum-target/src/lib.rs:85-100` already differ from the ones
   `clang` computes for the same targets, for four of the five supported
   targets, and nothing reports it. Under LLVM-as-a-library the target machine
   would set the datalayout and the drift would be impossible. Under the
   textual approach the fix is five strings and one test that compares them
   against `clang -S -emit-llvm`. That is cheaper than the dependency and it
   keeps the check in the repository instead of in a library's behaviour. The
   measured drift is in [What the code looks like today](#what-the-code-looks-like-today).

6. **Record the decision.** This is a toolchain decision of the same family
   as D016 and should be a numbered entry in
   `docs/language-reference/decisions.md` (D017), so that it is not reopened
   casually and so that phase 11g inherits a written position rather than an
   inherited assumption. The orchestrator owns that edit; this document only
   recommends it.

## What the code looks like today

### The code generator produces IR text, and nothing else

`nexum_codegen::emit` has the signature `pub fn emit(mir: &MirModule, target:
&TargetSpec) -> String` (`crates/nexum-codegen/src/lib.rs:109`). The crate's
first line says so: `//! Backend: MIR -> LLVM IR text (bootstrap).`
(`crates/nexum-codegen/src/lib.rs:1`). There is no LLVM type in the crate's
dependency list at all (`crates/nexum-codegen/Cargo.toml:6-12`); the crate
depends on `nexum-mir`, `nexum-sema`, `nexum-target`, `nexum-toolchain`,
`nexum-diagnostics` and `nexum-runtime` and on nothing else.

What the emitted module contains:

* the module header: `; ModuleID = 'nexum'` (line 111), `target triple = "..."`
  (line 112) and `target datalayout = "..."` (lines 113-116), where the
  datalayout comes from `TargetSpec::data_layout_str()`
  (`crates/nexum-target/src/lib.rs:85-100`) - a **hand-maintained string per
  (architecture, operating system)**;
* the whole runtime, spliced in as text: `out.push_str(&nexum_runtime::runtime_ir(&target.os, &target.arch));`
  (line 119). The runtime is itself a string builder
  (`crates/nexum-runtime/src/lib.rs:46`), and it includes the inline-assembly
  system call wrappers, which are AT&T assembly text inside `asm sideeffect`
  strings (`crates/nexum-runtime/src/syscalls.rs:207-208` and `226-230`);
* the record types: `%Name = type { ... }` (lines 122-125);
* the string literals, as module-private globals (lines 159-167), escaped
  byte by byte (`llvm_escape`, lines 177-193);
* the functions themselves, one at a time (`emit_fn`, line 228): `define
  {ret} @{name}({params})` (line 263), the memory plan that hoists every
  `alloca` into the prologue and registers the collector's root frame
  (lines 281-351, 435-457), the division-by-zero and `MIN / -1` handling
  (lines 566-612), `insertvalue` / `extractvalue` for records and arrays
  (lines 888-916, 976-1006), and the bounds checks (lines 1007-1057).

The types are LLVM's own: `i64`, `double`, `i1`, `ptr`, `[N x T]`, `%record`
(`llvm_ty`, lines 8-20). The pointers are **opaque** - `ptr` is used
everywhere, with no pointee type - so the emitted IR is LLVM 15+ syntax and
will not parse on LLVM 14.

### The toolchain hands the text to clang and lets clang do everything else

`crates/nexum-toolchain/src/lib.rs` is 274 lines and contains two subprocess
helpers.

`Toolchain::discover()` (line 13) finds `clang` on the `PATH` and nothing
else; the linker's identity is recorded as the string `"system (clang
default)"` (line 17). If `clang` is missing, the toolchain refuses to work
and says how to install it (`missing_clang_message`, lines 133-141).

`run_clang_with` (line 178) is the whole build step, and it is one
invocation:

```
clang -Wno-override-module -O{min(3)} [-flto=thin -fuse-ld=lld]
      [--sysroot=...] [-fprofile-generate | -fprofile-use=...]
      <file.ll> -o <output>
```

(lines 185-201). In one process, `clang` parses the IR, verifies it, runs its
optimisation pipeline, lowers it to the target's machine code, assembles it,
and links it into an executable.

`verify_ir_for` (line 221) is the "verification", and it is not a
verification:

```
clang -Wno-override-module [--target=<triple>] -x ir -c <file.ll> -o <tmp>.obj
```

(lines 226-240). It compiles the module to a throwaway object file and then
deletes it (line 241). The doc comment says so plainly (lines 210-213): "the
strongest check available without standalone `opt`/`llvm-as` binaries". The
`--target=` is what makes the cross-target check work: without it, the host
assembler would be handed assembly for a foreign instruction set
(lines 218-224). The failure path is text: `Err(format!("LLVM IR
verification failed:\n{}", stderr.trim()))` (line 246) - `clang`'s stderr,
re-wrapped as an opaque `String`.

### The driver wires it together

`build_file` (`crates/nexum-driver/src/main.rs:1128`) and `build_to` (line
1160) both do the same five things: compile to MIR, `emit` the IR, write it
to `<temp>/nexum_out.ll` (line 1139/1165), `verify_ir_for` it with the
target's triple (lines 1141/1167), and `run_clang_with` it (lines
1142/1168).

Two things follow from that shape:

* **`nx check` does not check the IR at all.** `check_file` (line 1121) runs
  the front end and the MIR pipeline and stops. The IR is verified only as a
  side effect of *building*.
* **The IR text is a user-visible artefact.** `nx emit-ir` prints it (lines
  195-199), and `nx doctor` / `nx explain-toolchain` report on `clang` (lines
  905-924, 161-163).

### The consequences that follow from this design

* **The verification is `clang`'s.** Not the IR verifier alone - the whole
  backend, plus the target's assembler, because `verify_ir_for` compiles to
  an object file. It is a *strong* check (stronger than a verifier alone),
  but it is `clang`'s check, it is `clang`'s version, and its output is
  `clang`'s text.
* **The optimisation is `clang`'s pipeline.** `-O0`..`-O3` are `clang`'s
  flags (toolchain line 187), LTO is `-flto=thin -fuse-ld=lld` (lines
  189-190), PGO is `-fprofile-generate` / `-fprofile-use` (lines 196-199).
  Nexum's own optimisation is MIR-level and runs *before* codegen: const
  folding, DCE and small-function inlining
  (`crates/nexum-mir/src/lib.rs:1376-1393`).
* **The target support is `clang`'s.** The triple is a string in the IR
  (codegen line 112) and the datalayout is a hand-written string
  (`crates/nexum-target/src/lib.rs:85-100`). The set of targets Nexum can
  build for is bounded by "a `clang` exists that can target it" plus "the
  runtime has a hand-written assembly wrapper for it"
  (`crates/nexum-runtime/src/syscalls.rs`). Five targets are listed
  (`crates/nexum-target/src/lib.rs:75-83`).
* **The toolchain has an external, user-installed dependency.** `nx setup`
  offers to install it (`crates/nexum-driver/src/setup.rs:103-131`,
  `265-274`), the installers check for it
  (`docs/installation.md:56-70`, `packaging/windows/check-llvm.cmd`), `nx
  doctor` reports it (driver lines 905-924) and CI installs it on all three
  platforms (`.github/workflows/ci.yml:19-24`).
* **The toolchain itself is small and dependency-free.** Eleven crates, two
  external Rust dependencies in total (`clap` and `anyhow`, both in the
  driver), and no C or C++ anywhere. The `nx` binary is 2.9 MiB
  (`target/debug/nx.exe`, 3,058,688 bytes).

### One measured fact that matters for the decision: the datalayout has drifted

`nexum-target`'s hand-written datalayout strings do not match the ones
`clang` computes for the same targets. Emitting `examples/hello.nx` for each
of the five targets and asking `clang -S -emit-llvm` to re-print the module
gives:

| Target | Nexum's datalayout | `clang`'s datalayout |
| ------ | ------------------ | -------------------- |
| `windows-x86_64` | `e-m:w-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128` | *identical* |
| `linux-x86_64` | `e-m:e-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128` | `...-i64:64-i128:128-f80:128-...` |
| `macos-x86_64` | `e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-f80:128-n8:16:32:64-S128` | `...-i64:64-i128:128-f80:128-...` |
| `linux-aarch64` | `e-m:e-i8:8:32-i16:16:32-i64:64-i128:128-n32:64-S128` | `e-m:e-p270:32:32-p271:32:32-p272:64:64-i8:8:32-...-n32:64-S128-Fn32` |
| `macos-aarch64` | `e-m:o-i64:64-i128:128-n32:64-S128` | `e-m:o-i64:64-i128:128-n32:64-S128-Fn32` |

What is missing: the `i128:128` alignment on the two x86-64 Unix targets,
the pointer-alignment spec (`p270/p271/p272`) and the `-Fn32` native-float
spec on AArch64, and `-Fn32` on AArch64 Darwin.

This is currently **inert**: with the present type set (`i64`, `double`,
`i1`, `ptr`, arrays, records) the resulting AArch64 object file is
byte-for-byte identical to the one produced from `clang`'s own datalayout
(9,952 bytes, the same SHA-256), and `clang -c` accepts both without a
warning. But it will stop being inert: `i128:128` matters as soon as the
numeric tower adds 128-bit integers, and `-Fn32` matters as soon as `f32`
exists (phase 11c, D015) because `-Fn32` is what tells LLVM that the native
float width is 32 bits. A wrong datalayout is a silent miscompile, and
under the textual approach nothing in the build reports it. That is the
concrete cost of owning the target description by hand - and it is the cost
that the LLVM-library route would have eliminated for free.

## The design

### Option A - the textual IR, as it is today

```
.nx source -> MIR -> .ll (text) -> [clang: parse, verify, -O, codegen, assemble, link] -> exe
```

The property that matters: the artefact between the compiler and the backend
is a **document**. It can be read, diffed, grepped, printed by
`nx emit-ir`, fed to `opt`, `llc`, `llvm-dis`, `llvm-link`, or a debugger, and
compared against another compiler's output.

### Option B - LLVM as a library

```
.nx source -> MIR -> LLVMModule (in memory) -> [LLVM passes] -> LLVMModule
                                                        -> [TargetMachine] -> .o
.nx source -> .o -> [our linker] -> exe
```

What changes, concretely:

* **The emitter is rewritten.** `emit` returns a `String`; the library
  version builds `LLVMValueRef`s. Every one of the ~1,260 lines of
  `nexum-codegen` and every one of the ~240 lines of the runtime emitter
  changes shape. The runtime is the harder half: it is currently written as
  long literal IR strings (`crates/nexum-runtime/src/lib.rs:146-168` and
  elsewhere), and the C API cannot take a string - every instruction would
  have to be constructed by hand, or the runtime text would have to be parsed
  into a module at run time.
* **The passes are run in process.** `-O2` becomes "run the default `<O2>`
  pipeline"; LTO becomes the ThinLTO backend; PGO becomes instrumentation
  passes. All of that is real functionality that the library route delivers
  better than shelling out.
* **The target is set in process.** `TargetRegistry` plus
  `LLVMCreateTargetMachine` selects the backend and sets the datalayout and
  the triple on the module, so the hand-written strings in `nexum-target`
  disappear - and so does the drift measured above.
* **The diagnostics become structured.** `LLVMGetDiagInfo` gives severity,
  line and column, which could be turned into Nexum's own `Diagnostic` type
  with spans instead of `clang`'s stderr wrapped in a `String`
  (toolchain line 246).
* **`clang` disappears from the toolchain.** The compiler produces the
  object file itself. The *linker* is still needed - and that is 11g.

What it costs:

* **Build time.** `llvm-sys` either binds to a prebuilt LLVM (so every
  developer and every CI job needs the LLVM *development* package, not just
  the `clang` binary) or builds LLVM from source, which is hours on a
  many-core machine and tens of gigabytes of disk. The toolchain's build
  today is a plain `cargo build` of eleven pure-Rust crates with two
  external dependencies; making it depend on a C++ toolchain and a
  multi-gigabyte build is a large regression in kind, not in degree.
* **Binary size.** `nx` is 2.9 MiB today. The LLVM C API shared library on
  this machine is 69.9 MB (`LLVM-C.dll`), `libclang.dll` is 84 MB and
  `liblldb.dll` is 107.2 MB. A static link of the same libraries is larger
  still, and it has to be shipped in the MSI and the `.deb`. The released
  artefact goes from "one 3 MB file you drop on the `PATH`"
  (`docs/installation.md:139-155`) to a multi-hundred-megabyte install with a
  shared-library dependency, or to a static blob that has to be attributed.
* **API churn.** LLVM's C++ API changes between releases, and the pass
  manager was replaced wholesale in the LLVM 13-16 window. A safe wrapper
  (`inkwell`) is pinned to specific LLVM versions, so "use whatever LLVM the
  user has" - the model the installers are built on
  (`docs/installation.md:56-70`) - is replaced by a version matrix. The
  textual approach has a much smaller version surface: the emitted subset is
  small, conservative and stable, and the *only* version-sensitive thing is
  whether the parser accepts it.
* **Licensing.** LLVM is Apache-2.0 with the LLVM exception, so it is
  compatible with this project's Apache-2.0 licence
  (`Cargo.toml`, `workspace.package.license`) - the licence is not a
  blocker. The cost is attribution and distribution: shipping LLVM inside
  the released binaries obliges the project to carry the LLVM licence notice
  with them, and the packaging (`packaging/windows/Package.wxs`,
  `packaging/linux/`) has to account for it.
* **It contradicts the programme's own dependency rule.** The agent brief
  (`specs/AGENT-BRIEF.md`, rule 7) says "No new dependencies. The toolchain
  is Rust with the crates it already has". `llvm-sys` is a new dependency
  and it is the heaviest possible one; taking it is a programme-level
  decision, not a toolchain detail.

### Option C - the hybrid

Keep emitting textual IR; use LLVM's libraries for the verification and the
optimisation when they are available; fall back to `clang` otherwise.

The complexity it adds, honestly:

* **Two backends to keep correct.** The in-process path (parse, verify, run
  the pipeline) and the out-of-process path (`clang`) must agree about what
  "valid IR" and "optimised" mean. They will not always: LLVM 20's verifier
  accepts things LLVM 18's rejects, so the guarantee becomes "accepted by
  whichever LLVM this machine has".
* **The dependency is not removed, it is doubled.** The toolchain now needs
  the LLVM development package *and* a working `clang`, and `nx doctor` has
  to check both, and `nx setup` has to install both. The "optional" nature
  of the library is only optional at *run* time; at *build* time it is
  mandatory, so the build becomes heavier while the behaviour becomes less
  predictable.
* **The output stops being a function of the source.** The in-process
  pipeline and `clang -O2` do not produce the same object file: `clang`'s
  driver adds target defaults, its own LTO configuration and its integrated
  assembler. So the same program built on two machines would produce two
  different binaries, depending on which path the machine took. Today the
  binary already depends on the user's clang version, so this is not a
  catastrophe, but it makes the property *unpredictable* rather than
  *uniform*.
* **The test matrix doubles, and the mode is a property of the machine.**
  Every backend test would have to run in both modes, and the suite would
  need an explicit way to force the mode; otherwise the tests silently
  exercise whichever one the developer happens to have.

The hybrid is only worth building as a *migration step* towards the
library. Since the library is not where this programme is going, it is a
migration step to nowhere.

### What "replace clang with the system linker" buys and costs

The answer to the phase 11g question is: **yes, keep the textual IR, and
replace `clang` with the system linker - but understand precisely which half
of `clang` that replaces.** `clang` currently plays two roles: it is the
*compiler* (IR to object code) and it is the *link driver* (driving `ld` or
`lld` with the right flags). Taking the second role away is cheap and is
what D016 asks for. Taking the first role away is the codegen question, and
this document says: do not.

The pipeline becomes two steps:

```
.nx source -> MIR -> .ll (text) -> [clang -c / llc: parse, verify, -O, codegen, assemble] -> .o
                                                            -> [ld / lld / link.exe: our command line] -> exe
```

**What it buys.**

* **D016 is satisfied in its literal wording**: the program is linked by the
  Nexum toolchain, not by `clang`
  (`docs/language-reference/decisions.md:233-242`). Today the toolchain
  records the linker's identity as `"system (clang default)"`
  (toolchain line 17), which is the opposite of that.
* **The link line becomes ours, and therefore inspectable.** Today the
  command that produces the binary is a property of `clang`'s driver: the
  CRT selection, the default libraries, the `--as-needed` behaviour, the
  subsystem, the dynamic linker path, the `/DEFAULTLIB` directives - none of
  it is in the repository, and none of it is printed. `nx build --verbose`
  can print the whole thing.
* **The flags the toolchain already has become meaningful.** `--target`
  (driver line 21) currently changes only the IR and the verification, never
  the link; `--sysroot` (toolchain line 193) is passed to `clang` and never
  reaches a linker we do not control. With our own link line, both apply to
  the step they are named for.
* **11g becomes a smaller and better-defined step.** The native linker
  consumes object files, and object files are what the pipeline already
  produces in the middle. The linker is the component that carries the ABI
  and the binary layout - the thing D016's rationale is actually about - and
  it is the thing that must be ours for the freestanding goal to be
  reachable.
* **The freestanding link line becomes expressible.** A program with its own
  `_start`, no `crt1.o`, no `-lc`, no libc at all, is a *different link
  line*, not a different compiler. Owning the link step is the only way to
  have two of them.

**What it costs.**

* **Platform link knowledge, which is a project of its own.** On POSIX:
  `crt1.o` / `crti.o` / `crtn.o`, `-lc`, `-lm`, `--dynamic-linker`,
  `-pie` versus `-no-pie`, `--eh-frame-hdr`, `-z` flags, the `DT_INIT` and
  `DT_INIT_ARRAY` ordering, the `--build-id` and the `.note` sections. On
  Windows: `link.exe` or `lld-link`, `kernel32.lib`, `ucrt.lib`,
  `/ENTRY:mainCRTStartup`, `/SUBSYSTEM:CONSOLE`, `/MACHINE:X64`, the
  `.pdata` / `.xdata` unwind sections, the import library, the PDB path -
  and the `/DEFAULTLIB:` directives that `clang` puts in the object's
  `.drectve` section, which our linker would have to honour. All of this is
  knowledge `clang` currently carries for us.
* **The C library is still in the way.** Until phase 11a.2 the runtime
  declares `printf`, `fputs`, `fputc`, `exit`, `malloc` and `free`
  (`crates/nexum-runtime/src/lib.rs:49-54`). So the link line we write today
  is a *hosted* link line, and the freestanding link line is a second mode
  that arrives later. Our linker has to support hosted linking first and
  freestanding linking second - it is not a replacement, it is an addition.
* **We lose `clang`'s linker diagnostics** and have to write our own:
  undefined symbol, duplicate symbol, relocation overflow, section
  mismatch, TLS, init-array ordering. `clang`'s messages are good; ours
  would start empty.
* **"The system linker" is not one program.** On POSIX it is `ld.bfd`,
  `ld.gold` or `ld.lld`, with different flags and different defaults; on
  Windows it is `link.exe` (which needs the MSVC environment) or `lld-link`.
  The toolchain would have to pick one per platform and know its flags. The
  good news is that the toolchain already assumes `lld` is present - it
  passes `-fuse-ld=lld` for LTO (toolchain line 190) - and `lld` ships in
  the same package as `clang` on the platforms the toolchain supports, and
  `clang --print-prog-name=ld.lld` reports its path, so the toolchain does
  not have to search the `PATH` for it.
* **The tests change shape.** `run_clang` (toolchain line 174) currently
  *links*; the backend tests use it to produce an executable
  (`crates/nexum-driver/tests/backend.rs:114-119`, `159-164`, `213-218`,
  `292-297`). Splitting it into "compile to object" and "link" touches every
  one of those call sites, and the link step acquires its own test surface.

**What it explicitly does not buy.** It does not remove the LLVM
dependency: the compiler still needs an LLVM installation, because the
object file still comes from `clang`. It does not make the toolchain
self-contained. It does not make binaries reproducible across `clang`
versions (the object file still comes from the user's `clang`; only the link
becomes deterministic given the same object). It does not let Nexum target
an architecture the user's `clang` cannot target. And it does not reduce the
number of external programs from one to zero - it replaces one external
program with two.

That last point is the honest headline: **"replace `clang` with the system
linker" removes `clang`'s *link-driver* role, not `clang` itself.** The
LLVM dependency survives 11g as it stands. It is 11g's *scope*, and the
codegen question, that together determine whether the dependency can go -
and this document's answer to the second half is "not by adopting LLVM as a
library".

### The decision criteria: what would have to be true to move to the library

For the programme to adopt LLVM-as-a-library, all of the following would
have to hold:

1. **The self-hosting plan is abandoned or reversed.** A Nexum-written
   compiler cannot call LLVM's C++ API, and the C API is a subset of it.
   Either 11j is dropped, or Nexum gains a C FFI *and* the self-hosted
   compiler is written in Nexum *and* that FFI is allowed to reach a C++
   library. That is three conditions, the last two of which are language
   changes of their own (D009/11d territory: pointer types, `unsafe`,
   target-specific C calling conventions).
2. **The toolchain gives up "whatever LLVM the user has".** The library
   route needs a *pinned* LLVM with development packages, and either ships
   it or requires the user to install a specific one. `nx setup`, the
   installers and `nx doctor` are all built around "install the LLVM
   toolchain" (`crates/nexum-driver/src/setup.rs:103-131`;
   `packaging/windows/check-llvm.cmd`; `docs/installation.md:56-70`); they
   would have to be rebuilt around a pinned version.
3. **The build must be reproducible and self-contained across machines.**
   Today, two machines with different `clang` versions produce different
   binaries from the same source. If that becomes a requirement, the answer
   is a pinned toolchain, not necessarily an in-process one.
4. **The install footprint stops mattering.** A 3 MB compiler that a user
   drops on the `PATH` is a real asset of the project; a 100 MB install is
   a real cost, and it is paid by every user to serve a goal that only the
   self-hosted compiler could actually use.
5. **The backend diagnostics become a product requirement rather than a
   convenience.** This is the weakest of the five, and the least likely to
   be decisive on its own: a malformed IR is nearly always a *compiler*
   bug, not a user error, and a compiler bug wants a stack trace rather than
   a span-annotated diagnostic.

Conditions 1 and 2 are the ones that matter, and neither is close. Note
what they have in common: **they are all about the compiler that will
replace the bootstrap, not the bootstrap itself.** The Rust bootstrap is
explicitly temporary (`docs/native-roadmap.md:104-106`: "The compiler stays
in Rust for now... the point of 11a-11i is that the *language* becomes
capable, not that the compiler moves"). Investing in a Rust-only LLVM
binding is investing in the temporary part.

And there is a third end state that the library route is not: a compiler
that needs no external tools at all. That end state requires *our own*
machine-code backend, not LLVM's. LLVM-as-a-library is a transitional option
that only makes sense if the compiler stays in Rust forever - which the
programme has already decided it will not.

### What this means for phase 11g

* **11g is not blocked by this decision and must not wait for it.** A linker
  consumes object files; `clang -c` produces object files today. 11g can be
  built and shipped against the current codegen, with no change to
  `nexum-codegen`.
* **11g's scope as written ("the program is linked by us, not by `clang`",
  D016) is exactly the link step.** That is the scope this document
  recommends, and it is achievable without touching the codegen.
* **The cheap half should land first**: `clang -c` for the object file, our
  own command line for the link. It is a small change, it satisfies D016,
  and it makes the eventual native linker a replacement for something we
  already own rather than a replacement for `clang`'s driver.
* **11g's *completeness* - no external toolchain at all - is explicitly out
  of scope for 11g under this recommendation.** That remains open, and it
  remains open in the direction of *our own backend*, not LLVM-as-a-library.
  If the orchestrator wants the completeness, the honest statement is that
  it is a phase 11g-plus project that ends in our own codegen, not a
  dependency swap.

### What this means for phase 11j

* **The textual IR path has to survive the self-hosting, and it will.** The
  self-hosting plan already assumes it: stage 10f is "Port codegen (LLVM IR
  text emission)" (`docs/self-hosting.md:43`). The self-hosted compiler
  builds the module as text and hands it to a tool, exactly as the Rust one
  does.
* **Therefore the textual IR is the programme's backend, not the
  bootstrap's.** A move to LLVM-as-a-library would put the *programme's*
  backend in Rust, in a compiler that 11j replaces, and would have to be
  undone. That is the whole argument, and it is why the recommendation is
  "keep", not "keep for now".
* **The differential gate gets stronger, not weaker.** The 11j exit gate is
  "the self-hosted compiler builds reproducibly and passes the same
  conformance/differential suite" (`docs/self-hosting.md:47-53`). With a
  textual backend on both sides, the comparison is *IR text against IR
  text* - exact, diffable, reviewable. Under a library backend, the Rust
  side would produce an object file and the comparison would be
  object-file-against-object-file, which is neither exact nor reviewable
  and would test the linker as much as the compiler.
* **The two existing self-hosting gates are already written against
  textual output** (`crates/nexum-driver/tests/backend.rs:140-194` and
  `196-249` compare token streams and normalized parse output). A move to
  the library would not break them - they compare the front end's output,
  not the backend's - but it would establish that the two compilers have
  *different* backends, which is the opposite of what 11j is for.

### The testing implications of each option

**The textual IR, as it is today.** The backend suite is
`crates/nexum-driver/tests/backend.rs`, eight tests:

| Test | Needs a toolchain? | What it checks |
| ---- | ------------------ | -------------- |
| `ir_verifies_for_pass_fixtures` (line 44) | yes | the IR of the pass fixtures compiles |
| `ir_verifies_for_examples` (line 52) | yes | the IR of every example compiles |
| `ir_verifies_for_all_targets` (line 60) | yes | the IR compiles *for each target*, via `--target=` |
| `codegen_emits_expected_fragments` (line 76) | **no** | the IR contains `define i32 @main(`, `fadd double`, `mul i64`, and no `nsw` |
| `run_outputs_match_fixtures` (line 93) | yes | 21 run fixtures: stdout and exit code |
| `selfhosted_lexer_matches_rust_lexer` (line 140) | yes | the self-hosted lexer against the Rust one |
| `selfhosted_parser_matches_rust_parser` (line 196) | yes | the self-hosted parser against the Rust one |
| `differential_opt_levels_agree` (line 251) | yes | the same program at MIR opt 0 and opt 1 behaves the same |

The property that is easy to miss: **one of the eight runs with no toolchain
at all** (`codegen_emits_expected_fragments`, line 76, which never calls
`toolchain()`), and the other seven *skip cleanly* when there is no `clang`
(`let Some(tc) = toolchain() else { return };`, lines 46, 54, 62, 95). A
machine with no LLVM installation can still run the codegen invariants. That
is a direct consequence of the IR being a document the compiler produces on
its own.

The other thing worth noticing is what the "verification" test really is. It
is not "the IR passes a verifier" - it is "the IR compiles to an object file
for this target", which includes the target's assembler. That is a stronger
check than a verifier alone, and it is the check that makes the cross-target
case work at all (lines 60-74). Under the library route the equivalent check
would be a true verifier (parse + verify in process), which is hermetic and
fast - and *weaker* in one respect, because it would no longer prove that the
target's assembler accepts the module.

**LLVM as a library.** The suite would need LLVM everywhere:

* The "no toolchain" property is lost for the whole suite. There is no such
  thing as a backend test that runs without the backend.
* `codegen_emits_expected_fragments` would have to be re-expressed. The
  module has to be printed back to text to assert on it - `print_to_string`
  - which re-creates the textual representation the library route was
  supposed to replace. The assertion then becomes "the text *LLVM* prints is
  what I expect", which is more fragile than "the text *I* emit is what I
  expect", because LLVM's printer changes between releases, and it loses the
  ability to assert on the input rather than the output. The test suite's
  power comes from the IR being text, and a library backend would have to
  re-derive that text to keep it.
* The verification test becomes hermetic, fast and structured. That is a real
  gain - and it is the gain that has the cheapest direct substitute: the
  emitted IR is produced by the compiler from MIR, so a malformed module is
  a compiler bug, and a compiler bug wants a panic with a backtrace, not a
  span.
* A version matrix appears: "the IR we emit is accepted by LLVM N, N+1,
  N+2". Today there is no matrix, because the emitted subset is small,
  conservative and stable, and the only version-sensitive thing is whether
  the parser accepts it.
* The behavioural gates are unaffected. `differential_opt_levels_agree` (line
  251) compares the *program's* output at two MIR optimisation levels, so it
  survives the change unchanged - and it remains the gate that actually
  matters, because it is the only one that tests semantics rather than
  syntax.

**The hybrid.** Every test above runs in both modes. The mode is a property
of the machine, so the suite needs an explicit override and a report of which
mode ran. The behavioural gates must hold in both modes, and a new
cross-mode gate ("does the library path and the `clang` path produce the
same program?") has to be written - and it will fail on any difference in
`clang`'s default pipeline configuration, which is exactly the kind of
difference that is tedious to chase and uninteresting to fix.

## Risks

* **What would have to be true for this recommendation to be wrong.** If the
  programme decides that the self-hosting is not happening - if 11j is
  dropped in favour of a permanent Rust compiler - then the argument in
  recommendation 1 collapses and the library route becomes worth
  evaluating on its own merits. The recommendation would then be wrong, and
  the mistake would have been made in the *roadmap*, not in the backend.
  The same is true if the programme decides that the toolchain must have no
  external dependency for a reason other than the self-hosting (an
  air-gapped build, a WASM-hosted compiler): in that case LLVM-as-a-library
  is still the wrong answer, and the right one is our own backend.
* **The link step is a real project, and 11g could be underestimated.** The
  cost list above is the honest one: platform link knowledge, the libc
  coupling, our own diagnostics, and the "the system linker is not one
  program" problem. If 11g is scoped to "POSIX only, ELF only, hosted
  linking only", it is a manageable project. If it is scoped to "all three
  host platforms, all five targets, freestanding, with our own diagnostics",
  it is a large one, and it should be split.
* **The datalayout drift is a live defect risk, and it is independent of
  this decision.** The measured drift above is inert today and will not stay
  inert when `f32` and the integer widths land in 11c. It should be fixed
  and pinned by a test regardless of which backend route is chosen, and it
  should be fixed *before* 11c, not after.
* **The textual IR pins the compiler to an LLVM 15+ syntax.** Opaque pointers
  (`ptr` with no pointee type) are used throughout
  (`crates/nexum-codegen/src/lib.rs:13`, `256`, `493`), so the emitted IR
  does not parse on LLVM 14. That is a deliberate, acceptable choice - it is
  the modern syntax - but it means the toolchain's minimum LLVM version is a
  fact that should be written down rather than discovered by a user.
* **The self-hosting argument could be over-read.** 11j is a large project
  and may be far away. The Rust compiler is what everyone uses in the
  meantime, and "the Rust compiler is temporary" does not make its users'
  experience unimportant. The honest reading of the recommendation is not
  "the Rust compiler does not matter" but "the Rust compiler's *backend* must
  not be the thing the programme is built on, because the backend is the part
  that 11j has to reproduce".

## Open questions

1. **Does 11g cover the system linker, or only our own linker?**
   *Recommendation:* split it. 11g.1 is "our link command line" (`clang -c`
   plus an explicit linker invocation); 11g.2 is "our linker". The first is
   small, immediately useful, and makes the second a replacement for
   something we already own.

2. **Which system linker?**
   *Recommendation:* `lld` (`ld.lld` on POSIX, `lld-link` on Windows),
   located through `clang --print-prog-name=ld.lld` rather than by searching
   the `PATH`, because the toolchain already assumes `lld` is present
   (`-fuse-ld=lld`, toolchain line 190) and `lld` ships alongside `clang` on
   the supported platforms. `link.exe` is the fallback on Windows, and
   `ld.bfd` is the fallback on POSIX if `lld` is missing.

3. **Do we support the hosted link line at all, or only the freestanding
   one?**
   *Recommendation:* both, and hosted first, because the runtime still needs
   `malloc`, `free`, `printf`, `fputs`, `fputc` and `exit`
   (`crates/nexum-runtime/src/lib.rs:49-54`) until 11a.2. The freestanding
   link line is 11a.2's business, not 11g's.

4. **Is the datalayout fix in scope for 11g, or its own task?**
   *Recommendation:* its own small task, before 11c, and it is a bug fix
   rather than a design change: replace the five strings in
   `crates/nexum-target/src/lib.rs:85-100` with the values `clang` computes
   (`clang -S -emit-llvm` on a trivial module for each target), and add a
   test that compares them, so the drift cannot come back.

5. **Do we record the decision as D017?**
   *Recommendation:* yes. It is the same class of decision as D016, it is the
   kind of thing that gets re-litigated every time somebody notices that
   `clang` is a big dependency, and 11g inherits a written position better
   than it inherits an assumption. The orchestrator owns the edit to
   `docs/language-reference/decisions.md`.

6. **Does the roadmap need a line about the LLVM dependency's end state?**
   *Recommendation:* yes, one line. Today `docs/native-roadmap.md:58-63`
   says the verifier "can be replaced by linking the toolchain against
   LLVM's own libraries later", which is now known to be the wrong
   destination: it is not a *later*, it is a *never*, for the reasons above.
   The line should say that the toolchain's LLVM dependency ends with the
   self-hosted compiler, and that the intermediate step is the linker.

7. **Should the self-hosting plan's stage 10f be made explicit about the
   tool it shells out to?**
   *Recommendation:* yes, one sentence in `docs/self-hosting.md`. Stage 10f
   ("Port codegen (LLVM IR text emission)") should say that the ported
   compiler writes the same IR text and invokes the same tool, and that the
   IR text - not the object file - is the differential comparison. That
   pins down the strongest form of the 11j gate while it is still cheap to
   change.

## Changes needed elsewhere

Nothing in this document was implemented; this is a research deliverable.
The recommendations that would need edits elsewhere, if the orchestrator
accepts them:

* `docs/native-roadmap.md:58-63` - the "The toolchain" section says the
  verifier "can be replaced by linking the toolchain against LLVM's own
  libraries later". That sentence should be replaced with the position
  settled here.
* `docs/language-reference/decisions.md` - a new D017 recording the
  decision, in the same style as D016 (lines 233-242).
* `crates/nexum-target/src/lib.rs:85-100` - the five datalayout strings, and
  a new test that pins them against `clang`'s own values.
* `docs/self-hosting.md:43` - stage 10f should state that the self-hosted
  compiler emits the same IR text and shells out to the same tool.
* `crates/nexum-toolchain/src/lib.rs:174-208` - when 11g.1 lands, `run_clang`
  splits into "compile to object" and "link", and `Toolchain::discover`
  (line 13) grows a linker to find.
* `crates/nexum-driver/tests/backend.rs` - the `run_clang` call sites (lines
  114, 159, 213, 292) change shape when the link step becomes ours.
