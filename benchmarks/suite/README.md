# The Nexum benchmark suite

The programme that owns the machine has to be able to say how fast it is.
This directory is how: every case is a Nexum program with a machine-readable
header, every case is compiled and checksummed before it is timed, and every
number is reported next to the machine it was measured on and the flags the
measured program was built with.

The harness is `nx-bench`, in `crates/nexum-driver/src/bin/nx-bench.rs`. It
is a binary of the driver crate, so it needs no new crate, no new
dependency and no change to any `Cargo.toml`.

```powershell
# from the repository root
cargo run --release --bin nx-bench -- list          # the cases and their headers
cargo run --release --bin nx-bench -- smoke         # compile every case, check the checksum
cargo run --release --bin nx-bench -- run            # time the cases, print the table
cargo run --release --bin nx-bench -- run --baseline # ... and compare against baseline.json
cargo run --release --bin nx-bench -- record         # ... and write baseline.json
cargo run --release --bin nx-bench -- doctor         # the machine profile and the toolchain
```

`smoke` is the mode CI should run: it compiles every case, runs it once and
checks the checksum, so the suite is also a correctness gate on the
optimiser. The shared runners cannot time anything, so the timing run is a
separate job on a known machine.

## The layout

```
cases/micro/*.nx     the cases, one operation at a time
reference/*.c        the C twins: the same algorithm, the same clang, the same flags
baseline.json        the accepted baseline, committed
results/             the raw runs, not committed
```

## The case format

The first comment line of a case carries the header:

```
# BENCH unit=ns-per-alloc ops=4000000 expect=4000001 base=cases/micro/alloc_concat_base.nx note="..."
```

| field       | meaning                                                                   |
| ----------- | ------------------------------------------------------------------------- |
| `unit`      | the label of the thing being measured, shown in the report                 |
| `ops`       | the number of iterations of the operation under test                       |
| `expect`    | the value the program must print on its last line                          |
| `ir_check`  | a substring the emitted LLVM IR must contain                               |
| `asm_check` | a function name the compiled assembly must still call                      |
| `base`      | the paired baseline, relative to this directory                            |
| `status`    | `blocked`: the case is compiled and checksummed, but not timed             |
| `mir_opt`   | the MIR level the case is only valid at                                   |
| `clang_opt` | the clang level the case is only valid at                                  |
| `note`      | a human-readable explanation, quoted if it contains spaces                 |

`expect` is what makes a benchmark a correctness test as well as a
measurement: a case that computes the wrong number fails instead of
reporting a time. `ops` must equal the iteration count in the source - the
harness never rewrites the source, and the equality is caught by the
checksum.

`ir_check` and `asm_check` are the two halves of "this case is still
measuring what it claims to measure". The IR check catches the MIR inliner
eating a call; the assembly check catches clang eating it, which it does at
`-O1` and above for every user function in the language today.

## The statistics

* **Fifteen timed runs per case**, and the **median** as the statistic. The
  failure mode on a real machine is a positive tail - a scheduler interrupt,
  a background build, a thermal excursion - which moves the median very
  little and the mean a lot.
* One **warm-up run** before the samples, and it is thrown away. The first
  seconds of a run are often faster than the rest, and that is exactly the
  sample that would be wrong.
* A **paired baseline**: the same program with the operation under test
  removed. The harness subtracts the baseline's median from the case's
  median, which is what removes the process start-up (about 10 ms on the
  authoring machine) and the loop bookkeeping from the result. Process
  start-up is the same size as a small workload on a noisy machine, so a
  case that is not subtracted is not a measurement.
* The result is divided by `ops`, and the report also shows `min`, `p25`,
  `p75`, `max` and the **noise**: the relative interquartile range
  `(p75 - p25) / median` of the same fifteen samples. The noise is the
  machine's own estimate of how much it can be trusted today.
* A case that finishes in under 100 ms is too short for the difference to
  mean anything, and the harness says so. Over 30 s and it says that too.
* Against the recorded baseline, a case is a **regression** when
  `now > base * (1 + max(2%, 3 * noise))` and an **improvement** when it is
  below `base * (1 - max(2%, 3 * noise))`. The 2% floor is below what a
  quiet machine can resolve over fifteen runs; the `3 * noise` term
  protects a noisy one.
* The **machine profile** is part of the baseline. A baseline recorded
  under a different profile is refused: a number without the machine it was
  measured on is not a result. The profile is a hash of the CPU model, the
  core count, the OS and version and the power source; `baseline.json`
  records the parts as well, so a reader can see why two runs differ.

## The C twins

Every measured case has a twin in `reference/`: the same algorithm, the same
clang and the same flags, written in C. The twin is run in two modes,
`case` and `base`, and the difference between them is the cost of the
operation in C - measured the same way the Nexum side is measured, so the
ratio compares the same thing on both sides.

The twins are not decoration. "Nexum is 87% of C" is the only absolute claim
a systems language can make, and a comparison against another language
would measure the other language's backend instead. The Nexum programs are
compiled by clang too, so the C twin is what isolates the Nexum front end
and the Nexum runtime from the optimiser.

The report prints the twin that was used and the flags it was built with,
so a reader never has to guess what a ratio was against.

## The four runtime claims

The suite exists to measure the four costs the native roadmap's own claims
rest on.

| claim                     | case                  | status                                    |
| ------------------------- | --------------------- | ----------------------------------------- |
| syscall overhead          | `micro/syscall_write` | not yet measurable                        |
| call overhead             | `micro/call_value`    | not yet measurable                        |
| allocation cost           | `micro/alloc_concat`  | measured                                  |
| cost of a collected allocation | `micro/gc_pause`  | measured                                  |

The two that are not measurable are not skipped quietly: they are compiled,
checksummed, and reported with the reason.

* **Syscall overhead.** The runtime has a write wrapper, but no builtin
  exposes it, so a program cannot issue a bare `write(2)`. The closest it
  can get is `write_file`, which opens a file, writes one byte and closes
  it - three syscalls, not one. The case and its C twin are in place; the
  claim becomes measurable when phase 11a exposes the runtime's write.
* **Call overhead.** A call overhead is the difference between making a call
  and not making one, and that difference only exists if the call survives
  the optimiser. Nexum cannot say "do not inline this function", and clang
  inlines every user function call at `-O1` and above. A callee that is
  deliberately expensive enough for clang's inliner to refuse it can be
  written - the case has one, and `asm_check` proves the call survived - but
  the paired baseline is then optimised differently from the case, because
  the caller cannot see into the callee. The two differences disagreed by an
  order of magnitude, and neither was the cost of a call. The suite
  therefore reports the claim as not yet measurable rather than publishing a
  number that means something else. The case, the baseline and the C twin
  are in place for the day the compiler can keep a call alive.

## The discipline

* **Pin the measurement.** `nx-bench --pin 2` pins the run to one core, so
  it does not migrate between cores with different cache and turbo
  behaviour. The pinning is recorded in the machine profile.
* **Do not run the suite while anything else is running**, and not on
  battery. Both are discipline, not tooling, and the profile is what makes
  the absence of them visible.
* **One machine.** A baseline is only comparable on the machine that
  produced it. `baseline.json` is the artefact a pull request compares
  against; contributors on other machines run the suite in smoke mode.
* **The flags are recorded.** The MIR level and the clang level are
  separate knobs and they are recorded separately - the measured program's
  flags, not the harness's.
