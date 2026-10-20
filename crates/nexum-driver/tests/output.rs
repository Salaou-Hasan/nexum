//! The print path: the runtime's formatters against the C library.
//!
//! The requirement is byte-for-byte identical output to what the C library
//! produces today, because the run fixtures depend on it. `Int` is its
//! decimal digits, `Bool` is `0` or `1`, `Str` is its bytes, and `Float` is
//! exactly C's `%g` with the default precision of six.
//!
//! The float format is checked against the C library over millions of
//! values: the edge cases, random bit patterns, and random "nice" decimals.
//! The comparison is a differential one - the runtime's own output, from a
//! program the runtime formats, against the C library's `printf("%g")`.
//! Both programs are built by the same toolchain, so they use the same C
//! library.
//!
//! The values are handed to the programs in a file, so that one run covers
//! millions of them.

mod common;

use common::compile_file;
use std::path::{Path, PathBuf};
use std::process::Command;

/// How many random bit patterns the sweep uses.
const RANDOM_BITS: usize = 3_000_000;
/// How many random "nice" decimals the sweep uses.
const NICE_DECIMALS: usize = 1_000_000;

/// The toolchain, if it is installed.
fn toolchain() -> Option<nexum_toolchain::Toolchain> {
    nexum_toolchain::Toolchain::discover().ok()
}

/// A private work directory.
fn work_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join("nexum_tests").join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The bytes of a path, as an LLVM string constant.
fn llvm_path(path: &Path) -> String {
    let mut s = String::new();
    for b in path.to_string_lossy().as_bytes() {
        s.push_str(&format!("\\{b:02X}"));
    }
    s
}

/// The module the test drives: the runtime, plus a driver that formats the
/// values in a file and writes them to stdout.
///
/// The program the driver is compiled with has no `main`, so the module is
/// the runtime and the driver provides the entry point.
fn module_with_driver(name: &str, driver: &str) -> String {
    let work = work_dir("output");
    // The name is unique per test: the tests run in parallel, and they
    // share the work directory.
    let helper = work.join(format!("helper_{name}.nx"));
    std::fs::write(&helper, "fn helper():\n    print(0)\n").unwrap();
    let p = compile_file(&helper);
    assert!(p.diagnostics.is_empty(), "the helper must compile");
    let mir = p.mir.expect("mir");
    let mut ir = nexum_codegen::emit(&mir, &nexum_target::TargetSpec::host());
    ir.push('\n');
    ir.push_str(driver);
    ir
}

/// The driver that formats the doubles in a file and writes them out.
fn float_driver(path: &Path, count: usize) -> String {
    driver(path, count, "double", "nexum_fmt_float_to")
}

/// The driver that formats the integers in a file and writes them out.
fn int_driver(path: &Path, count: usize) -> String {
    driver(path, count, "i64", "nexum_fmt_int_to")
}

/// The driver, for a file of values of one type.
///
/// The output goes into a buffer on the stack, which is flushed when it is
/// full: the driver must not allocate, because an allocation would collect
/// the file's buffer, which nothing roots.
fn driver(path: &Path, count: usize, ty: &str, fmt: &str) -> String {
    format!(
        r#"@nexum_test_path = private unnamed_addr constant [{n} x i8] c"{path}\00"

define i32 @main() {{
entry:
  %data = call ptr @nexum_read_file(ptr @nexum_test_path)
  %out = alloca i8, i64 65536, align 8
  %tmp = alloca i8, i64 32, align 8
  %pos = alloca i64, align 8
  %i = alloca i64, align 8
  store i64 0, ptr %pos
  store i64 0, ptr %i
  br label %loop
loop:
  %iv = load i64, ptr %i
  %done = icmp uge i64 %iv, {count}
  br i1 %done, label %flush, label %body
body:
  %off = mul i64 %iv, 8
  %p = getelementptr i8, ptr %data, i64 %off
  %v = load {ty}, ptr %p
  %len = call i64 @{fmt}(ptr %tmp, {ty} %v)
  %posv = load i64, ptr %pos
  %need = add i64 %posv, %len
  %need2 = add i64 %need, 1
  %full = icmp ugt i64 %need2, 65536
  br i1 %full, label %doflush, label %docopy
doflush:
  %nf = load i64, ptr %pos
  call void @nexum_write_stdout(ptr %out, i64 %nf)
  store i64 0, ptr %pos
  br label %docopy
docopy:
  %posv2 = load i64, ptr %pos
  %dst = getelementptr i8, ptr %out, i64 %posv2
  call ptr @nexum_memcpy(ptr %dst, ptr %tmp, i64 %len)
  %pos2 = add i64 %posv2, %len
  %nlp = getelementptr i8, ptr %out, i64 %pos2
  store i8 10, ptr %nlp
  %pos3 = add i64 %pos2, 1
  store i64 %pos3, ptr %pos
  %i2 = add i64 %iv, 1
  store i64 %i2, ptr %i
  br label %loop
flush:
  %nl = load i64, ptr %pos
  call void @nexum_write_stdout(ptr %out, i64 %nl)
  ret i32 0
}}
"#,
        n = path.to_string_lossy().len() + 1,
        path = llvm_path(path),
        count = count,
        ty = ty,
        fmt = fmt,
    )
}

/// Compile a module and run it, and return its output.
fn run_module(ir: &str, name: &str) -> String {
    let tc = toolchain().expect("clang");
    let work = work_dir("output");
    let ll = work.join(format!("{name}.ll"));
    let exe = work.join(format!("{name}.exe"));
    std::fs::write(&ll, ir).expect("write ir");
    nexum_toolchain::verify_ir(&tc.clang, ll.to_str().unwrap())
        .unwrap_or_else(|e| panic!("verify ir: {e}"));
    nexum_toolchain::run_clang(&tc.clang, ll.to_str().unwrap(), exe.to_str().unwrap())
        .unwrap_or_else(|e| panic!("link: {e}"));
    let out = Command::new(&exe).output().expect("run");
    assert!(out.status.success(), "the driver failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// The C library's own answer: a program that prints the values in a file
/// with `printf("%g")` (or `printf("%lld")`).
///
/// It is built by the same toolchain as the runtime's driver, so the two
/// use the same C library - which is the point of the comparison.
fn c_library_output(values: &Path, format: &str, name: &str) -> String {
    let tc = toolchain().expect("clang");
    let work = work_dir("output");
    let source = work.join(format!("{name}.c"));
    let exe = work.join(format!("{name}.exe"));
    std::fs::write(
        &source,
        format!(
            r#"#define _CRT_SECURE_NO_WARNINGS
#include <stdio.h>
#include <stdint.h>
#include <string.h>

int main(int argc, char **argv) {{
    if (argc < 2) return 2;
    FILE *f = fopen(argv[1], "rb");
    if (!f) return 3;
    uint64_t bits;
    while (fread(&bits, sizeof bits, 1, f) == 1) {{
        double d;
        memcpy(&d, &bits, sizeof d);
        printf("{format}\n", d);
    }}
    fclose(f);
    return 0;
}}
"#
        ),
    )
    .expect("write the reference");
    nexum_toolchain::run_clang(&tc.clang, source.to_str().unwrap(), exe.to_str().unwrap())
        .unwrap_or_else(|e| panic!("the reference did not build: {e}"));
    let out = Command::new(&exe)
        .arg(values)
        .output()
        .expect("run the reference");
    assert!(out.status.success(), "the reference failed: {out:?}");
    String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n")
}

/// The C library's NaN spelling is platform dependent: the UCRT prints
/// `nan(ind)` and `nan(snan)`, and the glibc and the BSD libc print `nan`.
/// The language fixes the spelling, so the payload is dropped before the
/// two are compared.
fn without_payload(s: &str) -> &str {
    match s.find('(') {
        Some(pos) => &s[..pos],
        None => s,
    }
}

/// Compare two outputs, line by line, and say where they differ.
fn assert_same_lines(runtime: &str, library: &str, what: &str) {
    let mut a = runtime.lines();
    let mut b = library.lines();
    let mut count = 0usize;
    loop {
        match (a.next(), b.next()) {
            (None, None) => break,
            (x, y) => {
                let (x, y) = (
                    x.unwrap_or("<end of output>"),
                    y.unwrap_or("<end of output>"),
                );
                assert_eq!(
                    without_payload(x),
                    without_payload(y),
                    "{what}: the runtime printed {x:?}, the C library {y:?} (line {count})"
                );
                count += 1;
            }
        }
    }
}

/// A deterministic generator, so that a failure is reproducible.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// The values the float sweep uses.
///
/// The edge cases come first, then the random bit patterns (which cover the
/// whole range, subnormals and the extremes included), and then the random
/// "nice" decimals (which have a short exact expansion, so the rounding and
/// the tie breaking are exercised).
fn float_values() -> Vec<f64> {
    let mut v = edge_cases();
    let mut rng = Rng::new(0x9E37_79B9_7F4A_7C15);
    for _ in 0..RANDOM_BITS {
        v.push(f64::from_bits(rng.next_u64()));
    }
    for _ in 0..NICE_DECIMALS {
        let n = rng.next_u64() % 1_000_000_000_000_000;
        let k = (rng.next_u64() % 16) as i32;
        v.push(n as f64 / 10f64.powi(k));
    }
    v
}

/// The values the int sweep uses.
fn int_values() -> Vec<i64> {
    let mut v = vec![
        0,
        1,
        -1,
        9,
        10,
        -10,
        99,
        100,
        i64::MAX,
        i64::MIN,
        i64::MAX - 1,
        i64::MIN + 1,
    ];
    let mut rng = Rng::new(0x1234_5678_9ABC_DEF0);
    for _ in 0..100_000 {
        v.push(rng.next_u64() as i64);
    }
    for e in 0..19 {
        let p = 10i64.pow(e);
        v.push(p);
        v.push(-p);
        v.push(p - 1);
        v.push(-(p - 1));
    }
    v
}

/// The values that are interesting: the ones the research document lists,
/// the extremes, the special bit patterns, and the exact ties.
fn edge_cases() -> Vec<f64> {
    let mut v = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.1,
        -0.1,
        1.0 / 3.0,
        2.0 / 3.0,
        1e-4,
        1e-5,
        1e5,
        1e6,
        99999.5,
        999999.5,
        123456.5,
        1000000.5,
        1234567.0,
        1.0000001,
        9.999995,
        1.234565,
        5.0,
        2.5,
        1e-300,
        1e300,
        1e15,
        1e16,
        1e17,
        f64::MAX,
        f64::MIN,
        f64::MIN_POSITIVE,
        f64::EPSILON,
        5e-324,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
    ];
    // The bit patterns that the C library spells in its own way.
    v.push(f64::from_bits(0x7ff8_0000_0000_0000)); // a quiet NaN
    v.push(f64::from_bits(0xfff8_0000_0000_0000)); // a negative quiet NaN
    v.push(f64::from_bits(0x7ff0_0000_0000_0001)); // a signalling NaN
    v.push(f64::from_bits(0xfff0_0000_0000_0001)); // a negative signalling NaN
    v.push(f64::from_bits(0x0000_0000_0000_0001)); // the smallest subnormal
    v.push(f64::from_bits(0x000f_ffff_ffff_ffff)); // the largest subnormal
    v.push(f64::from_bits(0x0010_0000_0000_0000)); // the smallest normal
    v.push(f64::from_bits(0x7fef_ffff_ffff_ffff)); // the largest finite
    // The exact ties: a seven digit value that ends in five. The sixth
    // digit decides the rounding, and it is even or odd in both cases.
    for m in 0..1000u32 {
        let a = (m * 1000 + 100_000) as f64 + 0.5;
        let b = (m * 1000 + 999_999) as f64 + 0.5;
        v.push(a);
        v.push(-a);
        v.push(b);
        v.push(-b);
    }
    // The values just below and above a rounding boundary.
    for k in 0..1000u32 {
        v.push(9.999995 + k as f64 * 1e-6);
        v.push(0.9999995 + k as f64 * 1e-7);
    }
    v
}

/// The float formatter produces exactly what the C library's `%g` produces.
///
/// The values are the edge cases, two million random bit patterns, and a
/// million random "nice" decimals.
#[test]
fn the_float_formatter_matches_the_c_library() {
    let Some(_tc) = toolchain() else { return };
    let values = float_values();
    let work = work_dir("output");
    let data = work.join("floats.bin");
    let mut bytes = Vec::with_capacity(values.len() * 8);
    for v in &values {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&data, &bytes).expect("write the values");

    let ir = module_with_driver("float", &float_driver(&data, values.len()));
    let out = run_module(&ir, "float_sweep");
    let library = c_library_output(&data, "%g", "float_reference");

    assert_same_lines(&out, &library, "a float");
    assert_eq!(out.lines().count(), values.len());
    eprintln!(
        "checked {} float values against the C library",
        values.len()
    );
}

/// The integer formatter produces exactly what the C library's `%lld`
/// produces.
#[test]
fn the_int_formatter_matches_the_c_library() {
    let Some(_tc) = toolchain() else { return };
    let values = int_values();
    let work = work_dir("output");
    let data = work.join("ints.bin");
    let mut bytes = Vec::with_capacity(values.len() * 8);
    for v in &values {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&data, &bytes).expect("write the values");

    let ir = module_with_driver("int", &int_driver(&data, values.len()));
    let out = run_module(&ir, "int_sweep");
    let library = c_library_output(&data, "%lld", "int_reference");

    assert_same_lines(&out, &library, "an integer");
    assert_eq!(out.lines().count(), values.len());
    eprintln!(
        "checked {} integer values against the C library",
        values.len()
    );
}

/// The values the research document lists, with the text the C library
/// produces for them. The table is the specification, read out loud.
#[test]
fn the_worked_examples_are_right() {
    let cases: &[(f64, &str)] = &[
        (5.0, "5"),
        (0.1, "0.1"),
        (1.0 / 3.0, "0.333333"),
        (1e-4, "0.0001"),
        (1e-5, "1e-05"),
        (1e5, "100000"),
        (1e6, "1e+06"),
        (1234567.0, "1.23457e+06"),
        (99999.5, "99999.5"),
        (999999.5, "1e+06"),
        (123456.5, "123456"),
        (1000000.5, "1e+06"),
        (1.0000001, "1"),
        (9.999995, "10"),
        (-0.0, "-0"),
        (f64::INFINITY, "inf"),
        (f64::NEG_INFINITY, "-inf"),
        (5e-324, "4.94066e-324"),
        (f64::MAX, "1.79769e+308"),
    ];
    let Some(_tc) = toolchain() else { return };
    let work = work_dir("output");
    let data = work.join("cases.bin");
    let mut bytes = Vec::new();
    for (v, _) in cases {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&data, &bytes).expect("write the values");

    let ir = module_with_driver("cases", &float_driver(&data, cases.len()));
    let out = run_module(&ir, "float_cases");
    let library = c_library_output(&data, "%g", "cases_reference");

    let mut lines = out.lines();
    for (v, expected) in cases {
        let line = lines.next().expect("the output ended early");
        assert_eq!(line, *expected, "{v:e}");
    }
    assert!(lines.next().is_none(), "the runtime printed too much");
    // The table and the C library have to agree as well.
    assert_same_lines(&out, &library, "a worked example");
}

/// The NaN spelling is the language's own, not the C library's.
///
/// The C library's spelling is platform dependent - the UCRT prints
/// `nan(ind)` and `nan(snan)` - so the language prints `nan` for every NaN
/// and `-nan` for a NaN with the sign bit set.
#[test]
fn the_nan_spelling_is_the_languages_own() {
    let Some(_tc) = toolchain() else { return };
    let cases: &[(f64, &str)] = &[
        (f64::from_bits(0x7ff8_0000_0000_0000), "nan"),
        (f64::from_bits(0xfff8_0000_0000_0000), "-nan"),
        (f64::from_bits(0x7ff0_0000_0000_0001), "nan"),
        (f64::from_bits(0xfff0_0000_0000_0001), "-nan"),
    ];
    let work = work_dir("output");
    let data = work.join("nans.bin");
    let mut bytes = Vec::new();
    for (v, _) in cases {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&data, &bytes).expect("write the values");

    let ir = module_with_driver("nans", &float_driver(&data, cases.len()));
    let out = run_module(&ir, "float_nans");

    let mut lines = out.lines();
    for (v, expected) in cases {
        let line = lines.next().expect("the output ended early");
        assert_eq!(line, *expected, "{v:e}");
    }
    assert!(lines.next().is_none());
}

/// The `print` builtin prints what the C library prints.
///
/// This is the whole path: the code generator's lowering, the runtime's
/// formatters, and the write to stdout.
#[test]
fn the_print_builtin_prints_what_the_c_library_prints() {
    let Some(tc) = toolchain() else { return };
    let work = work_dir("output");
    let source = work.join("print.nx");
    let program = r#"fn main():
    print(0)
    print(1)
    print(-1)
    print(42)
    print(-42)
    print(9223372036854775807)
    print(0 - 9223372036854775807 - 1)
    print(true)
    print(false)
    print("")
    print("hello world")
    print(0.0)
    print(1.0)
    print(-1.0)
    print(0.1)
    print(1.0 / 3.0)
    print(2.5 * 2.0)
    print(1000000.0)
    print(0.00001)
    print(999999.5)
    print(123456.5)
    print(1.0 / 0.0)
"#;
    std::fs::write(&source, program).expect("write the program");

    let p = compile_file(&source);
    let errs = common::error_lines(&p.diagnostics);
    assert!(errs.is_empty(), "the program must compile: {errs:?}");
    let mir = p.mir.expect("mir");
    let ir = nexum_codegen::emit(&mir, &nexum_target::TargetSpec::host());
    let ll = work.join("print.ll");
    let exe = work.join("print.exe");
    std::fs::write(&ll, &ir).expect("write ir");
    nexum_toolchain::verify_ir(&tc.clang, ll.to_str().unwrap()).expect("verify ir");
    nexum_toolchain::run_clang(&tc.clang, ll.to_str().unwrap(), exe.to_str().unwrap())
        .expect("link");
    let out = Command::new(&exe).output().expect("run");
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");

    let expected: &[&str] = &[
        "0",
        "1",
        "-1",
        "42",
        "-42",
        "9223372036854775807",
        "-9223372036854775808",
        "1",
        "0",
        "",
        "hello world",
        "0",
        "1",
        "-1",
        "0.1",
        "0.333333",
        "5",
        "1e+06",
        "1e-05",
        "1e+06",
        "123456",
        "inf",
    ];
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), expected.len(), "the output:\n{stdout}");
    for (got, want) in lines.iter().zip(expected) {
        assert_eq!(got, want);
    }

    // The float lines are what the C library prints for the same values.
    // The values are the ones the program prints, so the table above is
    // checked against the C library and not only against itself.
    let floats: &[f64] = &[
        0.0,
        1.0,
        -1.0,
        0.1,
        1.0 / 3.0,
        2.5 * 2.0,
        1000000.0,
        0.00001,
        999999.5,
        123456.5,
        f64::INFINITY,
    ];
    let data = work.join("print_floats.bin");
    let mut bytes = Vec::new();
    for v in floats {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::write(&data, &bytes).expect("write the values");
    let library = c_library_output(&data, "%g", "print_reference");
    let expected_floats = &[
        "0", "1", "-1", "0.1", "0.333333", "5", "1e+06", "1e-05", "1e+06", "123456", "inf",
    ];
    let library_lines: Vec<&str> = library.lines().collect();
    assert_eq!(library_lines.len(), expected_floats.len());
    for (got, want) in library_lines.iter().zip(expected_floats) {
        assert_eq!(*got, *want, "the C library and the table disagree");
    }
}

/// A list is printed as its address, as a number.
///
/// The type checker does not reject a list for `print` yet, and the old
/// varargs call passed the list's pointer to a `%lld` conversion. The call
/// is well typed now, and the observable behaviour is the same.
#[test]
fn a_list_is_printed_as_its_address() {
    let Some(tc) = toolchain() else { return };
    let work = work_dir("output");
    let source = work.join("list.nx");
    std::fs::write(
        &source,
        "fn main():\n    let xs = list_new()\n    list_push(xs, 1)\n    print(xs)\n",
    )
    .expect("write the program");
    let p = compile_file(&source);
    let errs = common::error_lines(&p.diagnostics);
    assert!(errs.is_empty(), "the program must compile: {errs:?}");
    let mir = p.mir.expect("mir");
    let ir = nexum_codegen::emit(&mir, &nexum_target::TargetSpec::host());
    let ll = work.join("list.ll");
    let exe = work.join("list.exe");
    std::fs::write(&ll, &ir).expect("write ir");
    nexum_toolchain::verify_ir(&tc.clang, ll.to_str().unwrap()).expect("verify ir");
    nexum_toolchain::run_clang(&tc.clang, ll.to_str().unwrap(), exe.to_str().unwrap())
        .expect("link");
    let out = Command::new(&exe).output().expect("run");
    let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
    let line = stdout.trim();
    assert!(
        line.parse::<u64>().is_ok(),
        "a list prints as a number, not {line:?}"
    );
}

/// The code generator calls the runtime, and the format strings are gone.
#[test]
fn the_print_lowering_calls_the_runtime() {
    let work = work_dir("output");
    let source = work.join("lowering.nx");
    std::fs::write(
        &source,
        "fn main():\n    print(1)\n    print(true)\n    print(1.5)\n    print(\"s\")\n",
    )
    .expect("write the program");
    let p = compile_file(&source);
    let mir = p.mir.expect("mir");
    let ir = nexum_codegen::emit(&mir, &nexum_target::TargetSpec::host());

    assert!(ir.contains("call void @nexum_print_int(i64"), "{ir}");
    assert!(ir.contains("call void @nexum_print_bool(i64"), "{ir}");
    assert!(ir.contains("call void @nexum_print_float(double"), "{ir}");
    assert!(ir.contains("call void @nexum_print_str(ptr"), "{ir}");
    // The runtime's formatters are in the module exactly once.
    for name in [
        "nexum_print_int",
        "nexum_print_bool",
        "nexum_print_float",
        "nexum_print_str",
    ] {
        let count = ir
            .matches(&format!("define internal void @{name}("))
            .count();
        assert_eq!(count, 1, "{name} is defined {count} times");
    }
    // No format string, and no varargs call. The runtime still *declares*
    // `printf` - the declaration is the runtime's, and it goes away with the
    // rest of the C library - but nothing calls it.
    assert!(!ir.contains("call i32 (ptr, ...) @printf"), "{ir}");
    assert!(!ir.contains("call i32 (ptr, ...)"), "{ir}");
    assert!(!ir.contains("@fmt_"), "{ir}");
}

/// The print path needs no C library: the POSIX targets reach the kernel
/// through the system call layer.
#[test]
fn the_print_path_needs_no_c_library() {
    let work = work_dir("output");
    let source = work.join("noclib.nx");
    std::fs::write(&source, "fn main():\n    print(1.5)\n").expect("write the program");
    let p = compile_file(&source);
    let mir = p.mir.expect("mir");

    for target in [
        "linux-x86_64",
        "linux-aarch64",
        "macos-x86_64",
        "macos-aarch64",
    ] {
        let spec = nexum_target::TargetSpec::by_name(target).unwrap();
        let ir = nexum_codegen::emit(&mir, &spec);
        // The print path calls nothing but the runtime.
        for name in [
            "nexum_print_int",
            "nexum_print_bool",
            "nexum_print_float",
            "nexum_print_str",
        ] {
            let body = ir
                .split(&format!("define internal void @{name}("))
                .nth(1)
                .unwrap_or_else(|| panic!("{target} has no {name}"));
            let body = body.split("}\n\n").next().unwrap_or(body);
            for call in ["printf", "fwrite", "fputs", "fputc", "malloc", "free"] {
                assert!(
                    !body.contains(&format!("@{call}(")),
                    "{target}: {name} calls {call}"
                );
            }
            // The print path allocates nothing: it is not a collector
            // safepoint, and it does not depend on the allocator.
            assert!(!body.contains("@nexum_alloc"), "{target}: {name} allocates");
        }
        // The POSIX targets write to stdout through the system call layer.
        assert!(
            ir.contains("call i64 @nexum_sys_write(i64 1"),
            "{target} does not write to stdout through the system call layer"
        );
    }
}

/// The run fixtures still produce the same output.
///
/// The backend test does this too, but the print path is what this change
/// touches, so the fixtures are checked here as well.
#[test]
fn the_run_fixtures_still_print_what_they_printed() {
    let Some(tc) = toolchain() else { return };
    let dir = common::fixtures_dir("run");
    let files = common::nx_files(&dir);
    assert!(!files.is_empty(), "no run fixtures in {}", dir.display());
    let work = work_dir("output");
    for f in files {
        let expected = std::fs::read_to_string(f.with_extension("out"))
            .unwrap_or_else(|e| panic!("missing {}: {e}", f.display()))
            .replace("\r\n", "\n");
        let p = compile_file(&f);
        let errs = common::error_lines(&p.diagnostics);
        assert!(errs.is_empty(), "{}: {:?}", f.display(), errs);
        let mir = p.mir.expect("mir");
        let ir = nexum_codegen::emit(&mir, &nexum_target::TargetSpec::host());
        let stem = f.file_stem().unwrap().to_string_lossy().to_string();
        let ll = work.join(format!("{stem}.ll"));
        let exe = work.join(format!("{stem}.exe"));
        std::fs::write(&ll, ir).expect("write ir");
        nexum_toolchain::verify_ir(&tc.clang, ll.to_str().unwrap())
            .unwrap_or_else(|e| panic!("{}: verify: {e}", f.display()));
        nexum_toolchain::run_clang(&tc.clang, ll.to_str().unwrap(), exe.to_str().unwrap())
            .unwrap_or_else(|e| panic!("{}: link: {e}", f.display()));
        let out = Command::new(&exe).output().expect("run exe");
        let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
        assert_eq!(stdout, expected, "{}: stdout mismatch", f.display());
    }
}
