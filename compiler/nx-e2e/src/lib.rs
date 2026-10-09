//! End-to-end harness: compile a Nexum program to a native executable and
//! run it, asserting on what it prints.
//!
//! Why this exists: the tree-walking interpreter was removed, and before it
//! went, it was the only part of the compiler that ever *executed* a Nexum
//! program. `nx-codegen`'s own tests assert on emitted LLVM IR text and never
//! run anything; nothing else invoked clang. Deleting the interpreter without
//! this crate would have left the project with zero execution coverage.
//!
//! Expected-value tests are a stronger oracle than a differential against a
//! second engine: a differential can only tell you two implementations
//! differ, never which one is right, and it reports "ok" when both are wrong.
//! These tests assert what the answer *is*.
//!
//! ASCII-only source, deliberately: several tests embed language snippets.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// What a compiled program did.
#[derive(Debug, Clone)]
pub struct Outcome {
    /// Process exit code.
    pub code: i32,
    /// Everything the program wrote to stdout and stderr, newline-normalised.
    pub out: String,
}

impl Outcome {
    /// stdout split into lines, with trailing empties removed.
    pub fn lines(&self) -> Vec<String> {
        let t = self.out.trim_end_matches('\n');
        if t.is_empty() {
            Vec::new()
        } else {
            t.split('\n').map(|s| s.to_string()).collect()
        }
    }
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A scratch directory that removes itself on drop.
///
/// Unique per call, so `cargo test`'s thread pool cannot collide.
pub struct Scratch {
    pub dir: PathBuf,
}

impl Scratch {
    pub fn new(tag: &str) -> Scratch {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("nxe2e-{tag}-{nanos}-{n}"));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch { dir }
    }

    pub fn write(&self, name: &str, src: &str) -> PathBuf {
        let p = self.dir.join(name);
        std::fs::write(&p, src).expect("write source");
        p
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn normalize(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Type-check only. Returns every diagnostic, joined.
pub fn check(src: &str) -> Result<(), String> {
    let s = Scratch::new("check");
    let file = s.write("main.nx", src);
    match nx_types::check_source(src, &s.dir) {
        Ok(()) => {
            let _ = file;
            Ok(())
        }
        Err(es) => Err(es
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ")),
    }
}

/// Compile to a native executable and run it.
///
/// Compilation is ahead-of-time: type check, emit LLVM IR, hand it to clang.
/// `nounbox` selects the all-boxed build, which must produce identical
/// output to the default build.
pub fn run_in(src: &str, nounbox: bool) -> Outcome {
    let s = Scratch::new(if nounbox { "run-boxed" } else { "run" });
    run_in_dir(src, nounbox, &s.dir)
}

pub fn run(src: &str) -> Outcome {
    run_in(src, false)
}

/// As `run_in`, but the caller owns the scratch directory. Use this for
/// multi-file programs: write the entry and its modules into `dir` first.
pub fn run_in_dir(src: &str, nounbox: bool, dir: &Path) -> Outcome {
    run_in_dir_with_input(src, nounbox, dir, None)
}

/// Compile and run, feeding `input` on stdin when present. `None` inherits
/// stdin exactly like `run_in_dir`; `Some` (even empty) pipes it, so EOF is
/// deterministic rather than whatever the test runner was started with.
pub fn run_with_input(src: &str, input: &str) -> Outcome {
    let s = Scratch::new("run-input");
    run_in_dir_with_input(src, false, &s.dir, Some(input))
}

/// As `run_with_input`, with the representation selected.
pub fn run_in_with_input(src: &str, nounbox: bool, input: &str) -> Outcome {
    let s = Scratch::new(if nounbox {
        "run-boxed-input"
    } else {
        "run-input"
    });
    run_in_dir_with_input(src, nounbox, &s.dir, Some(input))
}

/// As `run_in`, but the caller owns the scratch directory. Use this for
/// multi-file programs: write the entry and its modules into `dir` first.
pub fn run_in_dir_with_input(src: &str, nounbox: bool, dir: &Path, input: Option<&str>) -> Outcome {
    let _ = std::fs::create_dir_all(dir);
    let file = dir.join("main.nx");
    std::fs::write(&file, src).expect("write entry");

    let base = dir.to_path_buf();
    if let Err(es) = nx_types::check_source(src, &base) {
        return Outcome {
            code: 101,
            out: normalize(
                &es.iter()
                    .map(|e| e.to_string())
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        };
    }
    let ir = match nx_codegen::compile_opts(src, &base, !nounbox) {
        Ok(ir) => ir,
        Err(e) => {
            return Outcome {
                code: 102,
                out: normalize(&e.to_string()),
            }
        }
    };

    let ll = dir.join("prog.ll");
    std::fs::write(&ll, &ir).expect("write ir");

    let out = dir.join(if cfg!(windows) { "prog.exe" } else { "prog" });
    let _ = std::fs::remove_file(&out);

    let mut cmd = Command::new("clang");
    cmd.arg("-O2").arg(&ll).arg("-o").arg(&out);
    if cfg!(unix) {
        cmd.arg("-lm");
    }
    let clang = match cmd.output() {
        Ok(o) => o,
        Err(e) => {
            return Outcome {
                code: 103,
                out: format!("cannot run clang: {e}"),
            }
        }
    };
    if !clang.status.success() {
        return Outcome {
            code: 104,
            out: normalize(&String::from_utf8_lossy(&clang.stderr)),
        };
    }

    match input {
        Some(text) => run_piped(&out, text),
        None => match Command::new(&out).output() {
            Ok(o) => {
                let mut text = String::from_utf8_lossy(&o.stdout).to_string();
                let err = String::from_utf8_lossy(&o.stderr).to_string();
                if !err.trim().is_empty() {
                    if !text.is_empty() && !text.ends_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&err);
                }
                Outcome {
                    code: o.status.code().unwrap_or(-1),
                    out: normalize(&text),
                }
            }
            Err(e) => Outcome {
                code: 105,
                out: format!("cannot run built program: {e}"),
            },
        },
    }
}

fn run_piped(out: &Path, input: &str) -> Outcome {
    use std::io::Write as _;
    use std::process::Stdio;
    let mut child = match Command::new(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return Outcome {
                code: 105,
                out: format!("cannot run built program: {e}"),
            }
        }
    };
    // Write, then close stdin so the child sees EOF after the input.
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(input.as_bytes()).is_err() {
            return Outcome {
                code: 105,
                out: "cannot write to program stdin".to_string(),
            };
        }
    }
    match child.wait_with_output() {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).to_string();
            let err = String::from_utf8_lossy(&o.stderr).to_string();
            if !err.trim().is_empty() {
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(&err);
            }
            Outcome {
                code: o.status.code().unwrap_or(-1),
                out: normalize(&text),
            }
        }
        Err(e) => Outcome {
            code: 105,
            out: format!("cannot run built program: {e}"),
        },
    }
}

/// Assert the program prints exactly these lines and exits 0.
#[track_caller]
pub fn assert_output(src: &str, expected: &[&str]) {
    let o = run(src);
    assert_eq!(
        o.lines(),
        expected.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "\n  program output: {:?}\n  source:\n{}",
        o.out,
        indent(src)
    );
    assert_eq!(o.code, 0, "expected exit 0, got {}", o.code);
}

/// Assert the program prints exactly this single line.
#[track_caller]
pub fn assert_one(src: &str, expected: &str) {
    assert_output(src, &[expected]);
}

/// Assert the program, fed `input` on stdin, prints exactly these lines
/// and exits 0. Interactive programs cannot use `assert_output`: the
/// harness stdin is whatever the test runner was started with.
#[track_caller]
pub fn assert_output_with_input(src: &str, input: &str, expected: &[&str]) {
    let o = run_with_input(src, input);
    assert_eq!(
        o.lines(),
        expected.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        "\n  program output: {:?}\n  source:\n{}",
        o.out,
        indent(src)
    );
    assert_eq!(o.code, 0, "expected exit 0, got {}", o.code);
}

/// Assert the program FAILS at compile time with a diagnostic containing
/// `needle`. The native pipeline type-checks before emitting code, so most
/// "runtime errors" in a checked language are really compile-time
/// rejections. Getting this distinction wrong would silently delete coverage.
#[track_caller]
pub fn assert_rejected(src: &str, needle: &str) {
    match check(src) {
        Ok(()) => panic!(
            "expected a type error containing {:?}, but the program was accepted\n  source:\n{}",
            needle,
            indent(src)
        ),
        Err(msg) => assert!(
            msg.contains(needle),
            "diagnostic did not contain {:?}\n  got: {}\n  source:\n{}",
            needle,
            msg,
            indent(src)
        ),
    }
}

/// Assert the program compiles, then fails at runtime with output containing
/// `needle`. Used only for errors the type checker cannot see.
#[track_caller]
pub fn assert_runtime_error(src: &str, needle: &str) {
    let o = run(src);
    assert_ne!(
        o.code,
        0,
        "expected a runtime failure, but it exited 0 with {:?}\n  source:\n{}",
        o.out,
        indent(src)
    );
    assert!(
        o.out.contains(needle),
        "runtime output did not contain {:?}\n  got: {:?}\n  source:\n{}",
        needle,
        o.out,
        indent(src)
    );
}

fn indent(s: &str) -> String {
    s.lines()
        .map(|l| format!("    {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}
