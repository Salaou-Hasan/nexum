# What the toolchain may depend on

**Question.** What may the Nexum toolchain depend on: which Rust crates,
which external programs, which versions of them? May it use LLVM's libraries
directly instead of shelling out to `clang`? And what may a *generated Nexum
program* depend on? The point is that the toolchain's dependencies become a
deliberate choice rather than an accident of what happens to be installed on
the developer's machine.

**Summary.** The recommendation is to split the programme's one dependency
rule into two rules - one about the language, one about the toolchain - and to
make the toolchain's dependency list an explicit, checked allow-list. The
toolchain's dependencies today are small and entirely deliberate:
`nexum-toolchain` has **no third-party Rust crates at all**, and it calls
exactly one external program, `clang`, in exactly two places. The language's
rule is a different rule, and it is already enforced without any compiler:
the freestanding gate reads the emitted IR's `declare` lines. LLVM **can** be
used as a library - `llvm-sys` is MIT and links the Apache-2.0-with-exceptions
LLVM C API - but it cannot be used with the LLVM the installer tells users to
install (the official Windows package has no `llvm-config`, and `llvm-sys`
needs one), it turns every release target into a cross-compiled LLVM build,
it grows the compiler from 1.7 MB to tens of megabytes, and it puts a C/C++
toolchain into the build of the compiler. So: keep LLVM external for now, but
stop depending on an *arbitrary* system clang - the emitted IR uses opaque
pointers, so clang must be 15 or newer, and nothing checks that today. The
installer must not ship clang, and the release pipeline must not either.

## Recommendation

1. **Split the rule in two, and write both into `specs/AGENT-BRIEF.md`.**
   The current rule 7 says "No new dependencies. The toolchain is Rust with
   the crates it already has; the language's programs must not need the C
   library." The first clause is about the *compiler*, the second about the
   *language*, and they are not the same kind of rule. Rule 7a must bind the
   generated program (no C library, enforced by the freestanding gate). Rule
   7b must bind the compiler (no *unreviewed* dependencies, enforced by an
   allow-list). Reason: a rule that says "no new dependencies" to the
   compiler forbids, by accident, the one thing the roadmap already says the
   compiler may do - use LLVM ("the compiler and its tooling may still be
   written in Rust and use LLVM - that is the *toolchain's* business",
   `docs/native-roadmap.md:13-14`).

2. **The toolchain's dependency list becomes a file in the repository, and a
   test checks that the code and the file agree.** Today the list is:
   eleven workspace-local `nexum-*` crates, plus `clap` and `anyhow` in the
   driver, plus one external program (`clang`). `nexum-toolchain` itself has
   zero third-party crates, and its two declared workspace dependencies
   (`nexum-target`, `nexum-diagnostics`) are not used anywhere in its source
   and can be deleted. Reason: today the list exists only in
   `Cargo.lock`; nobody reads it, and a new crate enters the compiler
   silently through an editor's autocomplete.

3. **The compiler may use the C library, the C++ standard library and
   LLVM's C API. The "no C library" rule applies to generated programs and
   to nothing else.** This is the distinction the programme depends on, and
   it is worth stating plainly: a Nexum *program* is freestanding; the
   *compiler* is an ordinary Rust program that may use whatever makes it
   work, including things written in C and C++. Reason: the compiler runs on
   the developer's machine, where a C library is always present; the program
   runs on the target, where it may not be. Conflating the two would either
   ban the compiler from using `std` (absurd) or let a C dependency leak
   into a generated program (the thing D012 exists to prevent).

4. **Do not link LLVM into `nx` in this phase; record it as the sanctioned
   path for the phases that need it.** `llvm-sys` is usable in principle
   (section "Can LLVM be a library"), but: the official LLVM Windows
   package the installer points at has no `llvm-config`, which is what
   `llvm-sys` needs to find LLVM; the release matrix has five targets, two
   of them cross-compiled, so it would mean building LLVM five times; the
   `nx` binary goes from 1.7 MB to tens of megabytes - the whole LLVM C API
   is a 70 MB shared library on this machine - and the build of the compiler
   gains a C/C++ toolchain; and the brief's rule 7 forbids it as the rule
   stands. Reason: the price is high and the benefit lands in phase 11g,
   not now. What must happen instead is item 7.

5. **Split the one clang invocation into two, and name them.** Today
   `run_clang_with` asks clang to do everything: parse the IR, optimise it,
   generate code, assemble it, and link it (`crates/nexum-toolchain/src/lib.rs:178-208`).
   The *link* half needs a linker, not clang, and D016 already says the
   program is to be linked by the Nexum toolchain. The *compile* half
   (IR to object file) needs LLVM, and cannot go away without either LLVM
   as a library or our own backend. Reason: this is the line the native
   linker work (phase 11g, `specs/research/native-linker.md`) has to cross,
   and the codebase cannot show where that line is while one command does
   five jobs. Splitting it also lets the *linker* be discovered and reported
   separately, which it is not today (`linker_kind` is the constant string
   `"system (clang default)"`, `lib.rs:17`).

6. **The verification step is not the LLVM verifier, and the code says it
   is. Fix the name or fix the comment.** `verify_ir_for`'s doc comment
   claims the call "runs the LLVM IR parser, verifier, and backend"
   (`lib.rs:210-213`). Measured on clang 23.1.3: it runs the parser and the
   backend, and it rejects four of five malformed modules I tried - but it
   accepted a call whose argument list does not match the callee's declared
   signature, with exit code 0 and an object file produced. clang passes
   `-disable-llvm-verifier` to `-cc1` in this path (`clang -###` shows it),
   so the check that runs is clang's own module check, not the verifier pass.
   Reason: a comment that overstates what the check does is worse than no
   check, because the next agent will rely on it. Either rename the function
   to what it does (`clang_accepts_the_module`) or make it actually run the
   verifier - and the second option needs `opt`, which the official Windows
   LLVM package does not ship (measured).

7. **The toolchain must not depend on an arbitrary system clang. Pin a
   minimum version and check it.** The emitted IR uses opaque pointers
   (`declare i32 @printf(ptr, ...)`, measured with `nx emit-ir`), which LLVM
   15 introduced and LLVM 17 made the only form. `clang_install_hint` tells
   a Linux user to run `sudo apt install clang` (`lib.rs:120-128`), and on
   Ubuntu 22.04 that is clang 14, which cannot parse the IR Nexum emits.
   `nx doctor` prints the clang version but never compares it
   (`crates/nexum-driver/src/main.rs:906-907`). The house rule should be:
   the toolchain states the minimum version of every external program it
   needs, refuses to build with an older one, and says so in `nx doctor`.
   Reason: a dependency that is present but the wrong version is worse than
   one that is missing, because the failure is a parse error in generated
   IR rather than a missing-tool message.

8. **The installer must not ship clang, and the release pipeline must not
   either.** Today the installer asks the *user's package manager* to
   install LLVM (`crates/nexum-driver/src/setup.rs:103-133`), the Windows
   MSI writes a note file when clang is missing
   (`packaging/windows/check-llvm.cmd`), and the Debian package declares no
   `Depends:` on clang at all (`packaging/linux/control.in`). That is the
   right shape: Nexum does not redistribute a 1.5 GB toolchain it does not
   control. If the toolchain ever links LLVM (item 4's long-term path), the
   licence notices must ship with the binary - see item 10.

9. **`nx build` does not have to work without an LLVM toolchain, and today
   it does not.** `Toolchain::discover` fails with
   `missing_clang_message()` (`lib.rs:13-19`, `133-141`), and `nx doctor`
   says "it cannot build programs until clang is installed"
   (`main.rs:920-925`). But the *programme* should decide this explicitly
   rather than by accident, and the recommendation is: **no.** The IR is
   LLVM IR; something has to implement LLVM's semantics, and that something
   has to be present. The question is only *whose* LLVM - the system's, or
   ours - and the programme should not answer that by "whatever `clang` is
   first on the PATH". The corollary: the commands that do not need a
   compiler (`nx check`, `nx emit-ir`, `nx parse`, `nx lex`, `nx analyze`)
   must keep working without one, and they do (measured).

10. **Two gates are missing today, and both are cheap.** First, the
    dependency allow-list (item 2). Second, the **clang-version gate**: the
    backend test suite skips seven tests when clang is missing
    (`crates/nexum-driver/tests/backend.rs:46,54,62,95,144,200,253` - all
    of the form `let Some(tc) = toolchain() else { return };`), so on a
    machine without clang the suite is green without having tested
    anything. Measured: with `C:\Program Files\LLVM\bin` removed from `PATH`, `cargo test --test backend` reports "8 passed; 0 failed" in **0.01 s**, against 17.44 s with clang present. The language-level gate does not have this problem, because
    `crates/nexum-driver/tests/freestanding.rs` reads the emitted IR and
    needs no compiler at all. Reason: a skipped test and a passing test
    look the same in CI, and the difference decides whether a release is
    sound.

11. **The repository has no licence file, and that becomes a real problem
    the moment LLVM is embedded.** `Cargo.toml:21` declares
    `license = "Apache-2.0"` and `README.md:174-176` says the same, but
    there is no `LICENSE` file, no `NOTICE`, and no third-party notices
    anywhere in the repository (measured). Apache-2.0 section 4(a) requires
    that a copy of the licence goes with the redistributed work, and the
    LLVM Exceptions require the LLVM notice to travel with the embedded
    parts. The `llvm-sys` crate is MIT, which requires its notice too. So
    the release pipeline needs a `THIRD-PARTY-NOTICES` step whether or not
    the toolchain links LLVM - and it needs it *before* it does.

12. **Record the decision as a numbered entry.** The dependency policy is
    a language-and-toolchain decision of the same kind as D012-D016, and it
    belongs in `docs/language-reference/decisions.md` as **D017**, with the
    house-rule text from the "The design" section below.

## What the code looks like today

### The dependency set, and what each dependency is for

Measured with `cargo tree -p nexum-toolchain` and `cargo tree -p nexum-driver`
in the worktree.

| Dependency | Kind | Used for |
| --------- | ---- | -------- |
| `nexum-toolchain` | workspace crate | Toolchain discovery and the two clang calls. Its only source file is `crates/nexum-toolchain/src/lib.rs` (274 lines). |
| `nexum-target` | workspace crate | Declared by `nexum-toolchain/Cargo.toml:7`. **Not used**: `grep nexum_target crates/nexum-toolchain/src/lib.rs` finds nothing. |
| `nexum-diagnostics` | workspace crate | Declared by `nexum-toolchain/Cargo.toml:8`. **Not used**: same grep. |
| `nexum-source` | workspace crate | The source map, behind `nexum-diagnostics`. The only thing `nexum-toolchain` transitively pulls in. |
| `clap` | third-party crate | The command line (`crates/nexum-driver/Cargo.toml`). Brings 20 crates with it (anstream, anstyle, clap_builder, clap_derive, heck, proc-macro2, quote, strsim, syn, unicode-ident, utf8parse, windows-sys, windows-link, ...). |
| `anyhow` | third-party crate | Error handling in the driver. |
| `clang` | external program | The only external program the toolchain crate calls: `lib.rs:184` and `lib.rs:227`. |

So the toolchain crate's *effective* dependency set is **the Rust standard
library and nothing else**. The whole workspace's `Cargo.lock` holds 33
packages: 11 `nexum-*` crates and 22 third-party ones, of which only two are
direct dependencies.

The driver's other external programs, none of which are in the build path:
`curl` (`main.rs:257`, `main.rs:359`; `setup.rs:49`, `setup.rs:72`) for
`nx update` and for downloading releases, `msiexec.exe` (`main.rs:436`,
`setup.rs:141`) for the Windows installer, `cmd.exe` plus `code`
(`main.rs:448`, `main.rs:497`, `main.rs:505`) for the editor extension, and
the built program itself (`main.rs:878`, `main.rs:1156`) when a test runs it.

### How clang is found

`Toolchain::discover` (`lib.rs:12-23`) looks for `clang` on `PATH` with
`find_exe` (`lib.rs:143-158`), which tries `<name>.exe` and then `<name>` in
each `PATH` entry. There is no version check, no alternative name, and no
configurable path. `clang_installed` (`lib.rs:27-29`) is the same lookup
without the error, and is what `nx doctor` and `nx setup` use.
`missing_clang_message` (`lib.rs:133-141`) is the user-facing text, and
`clang_install_hint` (`lib.rs:120-128`) is `winget install LLVM.LLVM`,
`brew install llvm`, or `sudo apt install clang` depending on the platform.

### The two calls

**The verification call** (`verify_ir_for`, `lib.rs:221-248`):

```
clang -Wno-override-module --target=<triple> -x ir -c <file.ll> -o <file.ll>.verify.obj
```

It produces a throwaway object file next to the IR, deletes it, and reports
the compiler's stderr on failure. The `--target` is what makes it a
cross-target check: the roadmap says "The cross-target verification now
assembles for the target the module claims, not for the host: a module that
contains assembly for a foreign instruction set cannot be checked by the host
assembler" (`docs/native-roadmap.md:143-145`). The object file is why the
check is target-aware at all - the *assembler* is what rejects foreign
assembly.

**The build call** (`run_clang_with`, `lib.rs:178-208`):

```
clang -Wno-override-module -O<n> [-flto=thin -fuse-ld=lld] [--sysroot=…]
      [-fprofile-generate] [-fprofile-use=…] <file.ll> -o <out>
```

One command for five jobs: parse, optimise, generate code, assemble, link.
Measured with `clang -###` on this machine (LLVM 23.1.3,
x86_64-pc-windows-msvc): it runs `clang -cc1 -triple … -emit-obj … -x ir
<file.ll>` and then `lld-link -out:out.exe … <file>.o`. So the *linker* it
uses on Windows is `lld-link` from the same LLVM installation; on Linux and
macOS it is the platform's `ld` via the C toolchain, which is a second,
undocumented external dependency.

**The two calls are not the same call, and today they disagree about the
target.** `verify_ir_for` is given `--target=<triple>`
(`main.rs:1141`, `main.rs:1167`); `run_clang_with` is not given a target at
all. Measured consequence, on this x86_64 Windows host:

```
$ nx --target linux-aarch64 build examples/hello.nx -o hello_aarch64.exe
built: …\hello_aarch64.exe
$ llvm-readobj --file-headers hello_aarch64.exe
  Format: COFF-x86-64
  Machine: IMAGE_FILE_MACHINE_AMD64 (0x8664)
```

The build reported success and produced an **x86-64** executable for an
AArch64 target. The verification step checked the module for
`aarch64-unknown-linux-gnu`; the build step compiled and linked it for the
host. This is the failure mode the programme's own lesson warns about -
"anything that only fails on one platform needs a test that runs everywhere" -
in a form that does not fail at all. It is a finding, not a fix: this
document changes no code.

### What the verification call actually checks

Measured on clang 23.1.3, x86_64-pc-windows-msvc, with hand-written
malformed modules. "verify" is `clang -Wno-override-module -x ir -c`; "link"
is `clang <file.ll> -o <file>.exe`.

| Malformed module | verify path | link path |
| ---------------- | ----------- | --------- |
| Unparseable IR (unclosed function) | rejected, exit 1 | rejected, exit 1, same message |
| `ret void` in an `i32` function | rejected: "value doesn't match function result type 'i32'" | rejected, same |
| Entry block with a predecessor | rejected: "invalid LLVM IR input: Entry block to function must not have predecessors!" | rejected, same |
| Dominance violation | rejected: "invalid LLVM IR input: Instruction does not dominate all uses!" | rejected, same |
| `call i32 @f(i32 1)` where `@f` is `declare i32 @f()` | **accepted, exit 0**, object produced | accepted by the compiler; rejected by the *linker*: "undefined symbol: f" |

So the two paths reject the same IR errors. The link path additionally
catches what only a linker can catch (undefined symbols), and the verify path
additionally knows the *target* and does not write the output program.

`clang -###` shows `-disable-llvm-verifier` in the `-cc1` arguments in both
paths, and for a plain C file as well. Running `-cc1` directly, without that
flag, gives the same accept/reject decisions. The practical conclusion is
that the check that runs is clang's own module check, not the LLVM verifier
pass; it catches most things, but not everything, and the doc comment's claim
that it runs "the LLVM IR parser, verifier, and backend" is not accurate.

**I could not run LLVM's own verifier on this machine**, because the LLVM
installation the installer points at does not have it. The LLVM 23.1.3
Windows package (`C:\Program Files\LLVM`) contains `clang`, `clang++`,
`lld`, `lld-link`, `ld64.lld`, `wasm-ld`, and the `llvm-*` tools
(`llvm-ar`, `llvm-nm`, `llvm-objdump`, `llvm-cov`, `llvm-profdata`,
`llvm-ranlib`, `llvm-strip`, ...), and it does **not** contain `opt`,
`llc`, `llvm-as` or `llvm-config` (measured by listing the directory and
searching the whole tree). That is a direct answer to one of the
alternatives: the "check it with `llc` or `opt`" option is not available on
the platform where most Nexum users are, with the LLVM the installer
installs.

### The cost of the two calls

Measured on this machine, five iterations each, on a small module:

| Step | Total | Per call |
| ---- | ----- | -------- |
| verify (`clang -x ir -c`, produces an object) | 454 ms | ~91 ms |
| link (`clang file.ll -o out.exe`) | 1198 ms | ~240 ms |
| `clang -x ir -S -o NUL` (no assembler, no object) | 304 ms | ~61 ms |

The verify step adds roughly 90 ms to every build and runs the LLVM backend
a second time over the same module. That is worth remembering when item 5
splits the command: after the split, "compile to object" and "link" are
separate, and the verification can be folded into the compile rather than
being a third pass.

### What the machine without clang looks like

Measured, with `C:\Program Files\LLVM\bin` removed from `PATH`:

```
$ nx doctor
needed to build and run Nexum programs:
  clang: NOT FOUND
    Nexum builds programs with the LLVM toolchain. Install it with:
      winget install LLVM.LLVM
…
Nexum is installed, but the LLVM toolchain is missing: it cannot
build programs until clang is installed.

$ nx build examples/hello.nx
clang not found on PATH.
Nexum builds programs with the LLVM toolchain, so clang has to be installed.
  install it with: winget install LLVM.LLVM
  then run `nx doctor` to check that everything is in place.
```

So `nx build` and `nx run` fail with a clear message, and `nx check`,
`nx emit-ir`, `nx parse`, `nx lex` and `nx analyze` still work: they never
reach the toolchain crate. The message is good. The two things that are
missing are the *version* check (item 7) and the fact that the backend test
suite goes quietly green without testing anything (item 10).

### The release pipeline

`nx.exe` is 1.69 MB today. The release workflow
(`.github/workflows/release.yml`) builds five targets: `x86_64-pc-windows-msvc`,
`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`x86_64-apple-darwin` and `aarch64-apple-darwin`. It installs clang on the
Linux and macOS runners (`release.yml:34-39`) because the build needs it for
the tests, not for the binary.

The installers do not ship clang:

* `install.ps1` / `install.sh` download the `nx` binary and hand over to
  `nx setup`.
* `nx setup` asks whether to install LLVM and then calls
  `winget install LLVM.LLVM`, `brew install llvm` or `sudo apt install -y
  clang` (`setup.rs:103-133`).
* The Windows MSI ships `nx.exe`, the icon, the VS Code extension and
  `check-llvm.cmd`, which writes `llvm-required.txt` next to the install if
  clang is missing and exits 0 (`packaging/windows/check-llvm.cmd`).
* The Debian package ships `/usr/bin/nx` plus the icon and the extension, and
  `packaging/linux/control.in` has **no `Depends:` line at all** - it does
  not even say that clang is recommended.

## Can LLVM be a library?

Yes, in principle. `llvm-sys` is the crate: MIT licensed, ~6.0 million
downloads, repository `gitlab.com/taricorp/llvm-sys.rs`, latest version
231.0.1 (published 2026-10-09) (crates.io, measured). Its version number is
the LLVM version times ten: 231 is LLVM 23.1.x, 221 is LLVM 22.1.x. It
exposes LLVM's C API, including `ir_reader`, `core`, `target`, `linker`,
`lto`, `transforms` and `bit_reader`/`bit_writer`.

What it would pull in, from the crate's own README and the crates.io
metadata (measured):

* **An installed LLVM with `llvm-config`.** The README is explicit: "There
  must be a compatible version of LLVM available. By default `llvm-sys` will
  look for `llvm-config` on `PATH` … Binary distributions of LLVM (including
  the official release packages) generally **do not** include a copy of
  `llvm-config`, making them unsuited to be used for building programs with
  `llvm-sys`." The exceptions it lists are the apt.llvm.org Debian/Ubuntu
  packages, Fedora's `llvm-devel`, and Arch's `llvm`. **This is the blocker
  for the installer as it stands**: the official Windows package that
  `winget install LLVM.LLVM` gives has no `llvm-config` (measured - the whole
  `C:\Program Files\LLVM` tree has none), and neither has the brew or the
  apt package in its default form.
* **A C++ library built from source**, when no suitable binary package
  exists. The README's "Compiling LLVM" section is a CMake configure and
  build of the LLVM project. That is the "large C++ library, built from
  source, with a long build time" in the question: an LLVM build is tens of
  minutes to hours, and it has to happen once per release target. The
  release matrix has five targets, two of them cross-compiled from an
  x86-64 runner, so this is five LLVM builds per release, and the README
  says about cross-compilation: "Will theoretically work, but hasn't been
  tested."
* **A C compiler at build time.** The `disable-alltargets-init` feature
  exists because "these functions need to be compiled from C", and turning
  it off "can allow the build to proceed if a working C compiler is
  unavailable". Turning it off is not an option for Nexum: the toolchain
  must initialise five targets, not just the host's.
* **A matching Rust toolchain on Windows**: "You must use a version of Rust
  that uses the same compiler as you build LLVM with, either MSVC or
  MinGW."
* **Five more Rust crates**: `libc` (normal), `clang-sys` and `tempfile`
  (optional), and `anyhow`, `cc`, `lazy_static`, `regex-lite` and `semver`
  (build dependencies). It declares `links = "llvm-<major>"`, so it can only
  be present once in a dependency graph.

What it would cost the programme:

* **Licence.** LLVM is "Apache License v2.0 with LLVM Exceptions"
  (`llvm.org/LICENSE.txt`, read). The exception says that "if, as a result of
  your compiling your source code, portions of this Software are embedded
  into an Object form of such source code, you may redistribute such
  embedded portions in such Object form without complying with the conditions
  of Sections 4(a), 4(b) and 4(d) of the License". So embedding LLVM in `nx`
  is permitted, and does not infect `nx` with anything worse than Apache-2.0.
  But the notices have to travel: the release must carry the Apache-2.0
  text and the LLVM Exceptions, and the MIT notice of `llvm-sys` itself.
  Today the repository has **no `LICENSE` file and no `NOTICE` file at all**
  (measured), so this is work that has to be done before, not after.
  Nothing in this conflicts with Nexum's own Apache-2.0 licence.
* **Size.** `LLVM-C.dll` is 69.9 MB on this machine; a static link of the
  parts Nexum needs would put the `nx` binary in the same order of
  magnitude, against 1.69 MB today.
* **The rule.** `specs/AGENT-BRIEF.md` rule 7 says "No new dependencies. The
  toolchain is Rust with the crates it already has". Read strictly, that
  forbids `llvm-sys` today. This document's recommendation is to keep that
  rule in force for this phase and to amend it (item 1) so that adding LLVM
  later is a recorded decision rather than a violation.
* **The contradiction with the roadmap.** `docs/native-roadmap.md:62-63`
  already says "The verifier also calls `clang`; that can be replaced by
  linking the toolchain against LLVM's own libraries later." So the
  programme has already named this as the eventual answer. The question
  this document answers is *what it costs and when*.

## What clang as a driver actually does for the two calls

Splitting the work clang does between the two calls:

| Job | Needs | Replaceable by the system linker? |
| --- | ----- | ---------------------------------- |
| Parse the IR | the LLVM IR parser | **No.** Only LLVM can parse LLVM IR. A pure-Rust parser (`llvm-ir`) would be a new dependency and would not verify anything. |
| Verify the module | the LLVM verifier | **No.** There is no second implementation. |
| Optimise | LLVM's passes | **No**, and it is not wanted: the IR is emitted already-optimised by `nexum-codegen`. |
| Generate machine code | LLVM's backend | **No.** |
| Assemble | the assembler (clang's integrated one, or a platform `as`) | **Partly.** The assembler could be a separate program, but clang's integrated assembler is why cross-target assembly works without a cross-assembler installed. |
| Link | a linker | **Yes.** This is the whole of D016. `lld` is already a separate program on Windows and macOS, and `ld` on Linux; the toolchain can call it directly, or phase 11g writes its own. |

So the answer to the question is: **the link can be taken away from clang
immediately, by splitting the command and calling the linker directly; the
compile cannot.** The compile is the part that needs LLVM, and LLVM is
either a program on the machine or a library in the binary.

The three alternatives the question names, and what they are worth:

* **Check the IR with `llc` or `opt`.** This needs an LLVM installation that
  ships those tools. The official Windows package does not (measured), so
  this would add a second external dependency rather than remove one, and it
  would still need clang for the compile half. On a machine that has
  `apt.llvm.org`'s packages it would work; on the standard Windows install
  it does not exist. It also buys little: `opt -passes=verify` runs the
  verifier, and `llc` runs the backend, but the current verify call already
  runs the backend and catches the IR errors a wrong module would produce.
* **Check the IR with the host's clang in a target-appropriate way.** This
  is what the toolchain does today (`--target=<triple>`, `lib.rs:229-231`),
  and it is the right shape: the check is done by the same LLVM that will
  compile the module, for the target the module claims. It costs a second
  pass over the module (~91 ms measured) and requires clang to be present.
* **Do not verify at all, and let the link step fail.** Measured: the link
  path rejects the same four malformed modules as the verify path, and
  catches undefined symbols as well. So for the *host* target this loses
  almost nothing. It loses three things: the target-correct check (item 5 -
  the link step does not honour `--target` today, so dropping the verify
  call would make cross-target builds silently unchecked), the "no output
  file is written when the module is broken" property, and any check that
  wants to happen without producing a program (an `nx build --emit-object`,
  for instance). It is the cheapest option and it is defensible *after* the
  link step is fixed to pass `--target`; it is not defensible before.

## The design

The house rule, written to go into `specs/AGENT-BRIEF.md` as the
replacement for rule 7:

> **7. Dependencies.**
>
> **7a. The language.** A program built by Nexum depends on the kernel and
> on nothing else: no C library, no C ABI, no runtime borrowed from
> elsewhere. No choice the toolchain makes may change this. It is enforced by
> `crates/nexum-driver/tests/freestanding.rs`, which reads the emitted IR
> and needs no compiler to run.
>
> **7b. The toolchain.** The compiler is a Rust program. It may use any
> library, in any language, that its licence and its distribution allow -
> including the C library, the C++ standard library and LLVM's C API. What
> binds it is not "no dependencies" but "no unreviewed dependencies".
> Every dependency the toolchain has - Rust crate, native library, external
> program - is listed in `crates/nexum-toolchain/DEPENDENCIES.md`, with what
> it is for, and the version it is pinned to. A dependency that is not in
> that file is a defect, and a test fails when the file and the code
> disagree.
>
> **7c. External programs.** A program the toolchain calls must be found on
> `PATH`, must meet a stated minimum version, and must be checked before it
> is used. A program that is present but too old must produce a clear
> message, not a confusing error from generated IR.
>
> **7d. Distribution.** A dependency that is redistributed with the compiler
> ships its licence notices. A dependency that the user's own package
> manager installs is not redistributed, and its notices are not ours to
> ship. The compiler never bundles a toolchain it does not control.

### The dependency table

`crates/nexum-toolchain/DEPENDENCIES.md`, in the shape it should take:

| Dependency | Kind | Version | Why | Enforced by |
| ---------- | ---- | ------- | --- | ------------ |
| `nexum-toolchain`'s workspace crates | Rust, workspace | with the workspace | target descriptions and diagnostics | `Cargo.lock` allow-list test |
| `clap` | Rust, third party | `4` | the command line | `Cargo.lock` allow-list test |
| `anyhow` | Rust, third party | `1` | error handling in the driver | `Cargo.lock` allow-list test |
| `clang` | external program | **minimum 15**, ideally pinned | compiles the IR and links the program | version gate in `Toolchain::discover` and in `nx doctor` |
| the platform linker | external program | whatever the platform has | links the program, once D016 lands | `nx doctor` reports which one |
| (future) `llvm-sys` | Rust, third party | pinned to one LLVM version | the verifier, the backend and the assembler, in the binary | a decision entry, plus `LICENSE.LLVM` in the release |

The allow-list test reads `Cargo.lock` from the workspace root and asserts
that the set of packages outside `nexum-*` is exactly the list above. It
needs no network and no `cargo` at test time, so it runs everywhere - which
matters, because the point of the list is to be checked on every platform.

For the external programs, the list lives in the code as well as in the
file, so that a test can compare them:

```rust
// crates/nexum-toolchain/src/lib.rs
/// The external programs the toolchain calls. The list is checked against
/// crates/nexum-toolchain/DEPENDENCIES.md by a test.
pub const EXTERNAL_PROGRAMS: &[(&str, &str)] = &[
    ("clang", "compiles the emitted IR and links the program"),
];
```

and the toolchain calls nothing that is not in it. Today that is one entry.

### The version gate

The minimum version is a property of the IR the compiler emits, not a
preference. The emitted IR uses opaque pointers (`ptr`), which LLVM 15
introduced and LLVM 17 made the only supported form (LLVM's own
`docs/OpaquePointers.rst`). So:

* `clang_installed()` becomes `clang_version()` and returns the parsed
  version.
* `Toolchain::discover` compares it with `MINIMUM_CLANG = (15, 0, 0)` and
  fails with a message that says which version is needed and which one was
  found.
* `nx doctor` prints the version it found and says "clang 15 or newer is
  needed" when it is older, next to the existing "NOT FOUND" branch.
* `clang_install_hint` stops recommending `sudo apt install clang` without
  saying which version that gives; on Ubuntu 22.04 it gives clang 14, which
  cannot parse the IR.

### What the release pipeline does with this

* **Nothing changes today.** The installer asks the user's package manager
  for LLVM; the MSI and the deb ship the compiler, the icon and the
  extension; the binaries stay at 1.7 MB.
* **The deb gains a `Recommends: clang (>= 15)`.** `packaging/linux/control.in`
  currently has no `Depends:` line at all, so the package does not even
  hint that a toolchain is needed. `Recommends` is right, not `Depends`: the
  package installs fine without a compiler, it just cannot build programs.
* **The release pipeline gains a version check.** The CI installs clang on
  the Linux and macOS runners (`ci.yml:19-24`, `release.yml:34-39`); it
  should install a *known* version and assert it, because the tests are
  currently run against whatever the runner image happens to ship.
* **The release pipeline gains a third-party notices step** when, and only
  when, the toolchain links something. The repository needs a `LICENSE`
  file before that, because `Cargo.toml` already claims Apache-2.0 and the
  README says so, and the binary has to carry the text either way.

### The end state, for the phases that need it

If the programme later decides to carry LLVM inside `nx` (item 4's long-term
path), the shape is:

```
Nexum source
  -> MIR
  -> LLVM IR (textual, as today)
  -> LLVM's own verifier and backend, in the binary   [replaces the verify call]
  -> an object file
  -> the platform linker, or the toolchain's own       [D016, phase 11g]
  -> a program that needs no C library
```

At that point the external dependencies are a linker and, on the targets where
the toolchain does not yet emit machine code itself, an assembler. The
generated program's dependencies are unchanged: the kernel.

## Risks

* **The rule as written is ambiguous and has already been read both ways.**
  "No new dependencies. The toolchain is Rust with the crates it already
  has; the language's programs must not need the C library." One reading
  forbids LLVM in the compiler forever; the other forbids nothing at all. If
  the programme does not split it, the next agent will pick the reading
  that lets them do what they want. That is what item 1 is for.
* **`llvm-sys` is a bigger commitment than "add a crate".** It needs an LLVM
  with `llvm-config`, and the LLVM the installer gives users does not have
  one (measured). So the decision to link LLVM implies the decision to
  *build* LLVM, five times, for five targets - or to vendor prebuilt LLVM
  binaries and pin them. That is a release-pipeline project, not a
  dependency edit.
* **The verifier is not the verifier.** If a future agent trusts
  `verify_ir_for` to have run LLVM's verifier (the comment says so), they
  will write IR that passes and is wrong. The measured case is a call whose
  argument list does not match the callee: accepted, exit 0, and a program
  that misbehaves at run time.
* **Dropping the verify call to save 90 ms would lose the target check**
  until the link step is fixed to honour `--target`. Today the cross-target
  build produces the wrong architecture and says "built:" - so removing the
  only target-aware check would make that silent in a second way.
* **The version problem is already live.** A Linux user who follows the
  documented install (`sudo apt install clang` on Ubuntu 22.04) gets clang
  14 and cannot build anything. The failure will look like a compiler bug,
  in generated IR, not like a missing tool.
* **The test suite goes green when clang is missing.** Seven tests in
  `crates/nexum-driver/tests/backend.rs` return early. On a machine without
  clang - including a CI runner that failed to install it - the suite
  passes without testing the backend. The language-level gate does not have
  this problem.
* **A statically linked LLVM makes the compiler harder to build and to
  ship.** Fat binaries, five cross-compiled LLVM builds, a C++ toolchain
  in the build, and the Rust/compiler-family agreement on Windows. Every one
  of these is a way for a release to fail on a Friday.
* **The licence obligations are currently unmet in a way that matters only
  later.** No `LICENSE`, no `NOTICE`, no third-party notices, while
  `Cargo.toml` says Apache-2.0. Harmless while nothing is embedded;
  not harmless the moment LLVM is.

## Open questions

1. **Does the programme want `nx build` to work on a machine with no LLVM
   at all?** Recommendation: **no.** The IR is LLVM IR, and something has to
   implement LLVM's semantics. The decision is *whose* LLVM, not *whether*.
   The programme should say explicitly that the toolchain needs an LLVM and
   that the toolchain's job is to carry or to require a *known* one, rather
   than to take whatever is on the PATH. This needs the lead's decision,
   because it decides whether the long-term path is "carry LLVM" (fat
   binaries, five LLVM builds) or "require a pinned LLVM" (a small compiler,
   a stricter installation contract, and `nx doctor` refusing to build
   against the wrong version).
2. **Which LLVM version is the reference?** The emitted IR needs LLVM 15 or
   newer for opaque pointers. The reference should be one version, pinned,
   that CI installs and that the toolchain checks for. Recommendation: pick
   the version the release pipeline builds against and require *at least*
   that, with a stated minimum.
3. **Is the verifier step kept, renamed, or replaced?** Recommendation:
   rename it to what it does (`clang_accepts_the_module`, or
   `check_module_for_target`) and fix the comment now; decide later whether
   to make it a real verifier run, which needs `opt` or LLVM as a library.
   The name is a small change and the misleading comment is the risk.
4. **Where does the allow-list live, and who checks it?** Recommendation:
   `crates/nexum-toolchain/DEPENDENCIES.md`, checked by a test in
   `crates/nexum-driver/tests/` that reads `Cargo.lock`. It needs no
   network and no `cargo`, so it runs on every platform, which is the point.
5. **Should the `nx` binary ever contain LLVM?** This is the big one, and
   it belongs to the lead together with the native-linker work
   (`specs/research/native-linker.md`) and the codegen-backend work
   (`specs/research/codegen-backend-options.md`), because the three
   decisions interlock: what the toolchain emits, what links it, and what
   implements LLVM. Recommendation for now: no, and record the preconditions
   - a pinned LLVM with `llvm-config`, a third-party notices step, and a
   release pipeline that can build or fetch LLVM per target.
6. **Should `nx doctor` fail the build, or only report?** Recommendation:
   `nx doctor` reports and `nx build` refuses. A doctor that only warns
   leaves the developer with a build that fails later for a different
   reason.
7. **Should the Debian package depend on clang?** Recommendation:
   `Recommends: clang (>= 15)`, not `Depends:`. The compiler installs
   without a toolchain and reports that it cannot build programs yet.
8. **Should the two unused dependencies in `nexum-toolchain` be deleted?**
   `nexum-target` and `nexum-diagnostics` are declared
   (`crates/nexum-toolchain/Cargo.toml:7-8`) and used nowhere. Deleting
   them makes the toolchain crate's dependency set empty, which is the
   clearest possible statement of the policy. Recommendation: delete them
   as part of adopting rule 7b.
9. **Who owns the cross-target build defect?** The link step not passing
   `--target` is a code change in `run_clang_with`, outside this document's
   scope. It should be raised as its own task: it is exactly the class of
   silent failure the programme's first slice was about.

## Changes needed elsewhere

* `specs/AGENT-BRIEF.md`: replace rule 7 with rules 7a-7d (the text above).
* `docs/language-reference/decisions.md`: add **D017** - the dependency
  policy: the language's programs are freestanding, the toolchain's
  dependencies are listed and reviewed, and the toolchain may use LLVM as a
  library when the programme decides to.
* `crates/nexum-toolchain/DEPENDENCIES.md`: new file, the allow-list.
* `crates/nexum-driver/tests/dependencies.rs`: new test, the allow-list
  gate.
* `crates/nexum-toolchain/src/lib.rs`: the `EXTERNAL_PROGRAMS` constant, the
  version gate in `Toolchain::discover`, the two unused dependencies
  deleted, and the `verify_ir_for` doc comment corrected.
* `crates/nexum-driver/src/main.rs`: `nx doctor` reports the clang version
  it found against the minimum it needs.
* `packaging/linux/control.in`: `Recommends: clang (>= 15)`.
* `.github/workflows/ci.yml` and `release.yml`: install a *pinned* clang, and
  assert the version, instead of taking the runner image's.
* `LICENSE` and `THIRD-PARTY-NOTICES`: new files in the repository root, and
  a step in the release pipeline that puts them next to the binary.
