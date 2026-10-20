//! The maths builtins, measured against the C library.
//!
//! The runtime has no C library to ask for `exp`, `log` or `pow`, so it
//! carries its own. These tests compile the runtime's maths - the LLVM IR
//! text in `crates/nexum-runtime/src/math.rs` - into a program, run it, and
//! compare every result with the C library's.
//!
//! The comparison is against the C library and not against a number copied
//! out of a document: the harness calls the platform's `exp`, `log`,
//! `tanh`, `pow`, `erf` and `sqrt` next to the runtime's own, and the test
//! measures the distance between the two answers in units in the last place.
//!
//! The accuracy contracts the tests hold the implementation to:
//!
//! | function | contract |
//! | -------- | -------- |
//! | `nexum_sqrt` | 1 ulp |
//! | `nexum_exp`  | 1 ulp |
//! | `nexum_log`  | 1 ulp |
//! | `nexum_tanh` | 2 ulps |
//! | `nexum_erf`  | 2 ulps |
//! | `nexum_pow`  | 2 ulps for `\|y*ln(x)\| <= 1`, 8 ulps for an integer exponent |
//! | `floor`, `ceil`, `trunc`, `round` | exact |

// The runtime's maths module is loaded by path rather than depended on:
// the driver crate does not depend on the runtime crate, and the module is
// the thing under test.
#[path = "../../nexum-runtime/src/math.rs"]
mod math;

use std::path::PathBuf;
use std::process::Command;

/// The C library's functions, next to the runtime's own.
const C_FUNCTIONS: [(&str, &str); 5] = [
    ("nexum_sqrt", "sqrt"),
    ("nexum_exp", "exp"),
    ("nexum_log", "log"),
    ("nexum_tanh", "tanh"),
    ("nexum_erf", "erf"),
];

/// The runtime's rounding functions. The C library's are not called in the
/// harness, because the runtime defines the same names; the comparison for
/// these is with Rust's own, which are the C library's.
const ROUNDING: [&str; 4] = ["floor", "ceil", "trunc", "round"];

/// A double, as the hex literal LLVM's parser takes.
fn hex(v: f64) -> String {
    format!("0x{:016X}", v.to_bits())
}

/// The distance between two doubles, in units in the last place.
fn ulps_between(a: f64, b: f64) -> i64 {
    if a == b {
        return 0;
    }
    if a.is_nan() || b.is_nan() {
        return i64::MAX;
    }
    fn ordinal(x: f64) -> i64 {
        let b = x.to_bits() as i64;
        if b < 0 { i64::MIN - b } else { b }
    }
    let d = ordinal(a) as i128 - ordinal(b) as i128;
    if d > i64::MAX as i128 {
        i64::MAX
    } else if d < i64::MIN as i128 {
        i64::MIN
    } else {
        d as i64
    }
}

/// The work directory for the compiled harness.
fn work_dir() -> PathBuf {
    let d = std::env::temp_dir().join("nexum_math_tests");
    std::fs::create_dir_all(&d).expect("work dir");
    d
}

/// The input table: the arguments the functions are measured on.
///
/// The values are spread over the range each function is used in, and the
/// interesting ones - zero, the extremes, the small ones - are in there too.
fn inputs() -> Vec<f64> {
    let mut v: Vec<f64> = Vec::new();
    // A deterministic spread over the range, without a random number
    // generator: the test has to fail the same way twice.
    let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        // splitmix64
        seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let mut uniform = |lo: f64, hi: f64| {
        let r = (next() >> 11) as f64 / (1u64 << 53) as f64;
        lo + (hi - lo) * r
    };
    for _ in 0..120 {
        v.push(uniform(-700.0, 700.0));
    }
    for _ in 0..80 {
        v.push(uniform(-20.0, 20.0));
    }
    for _ in 0..80 {
        v.push(uniform(-1.0, 1.0));
    }
    for _ in 0..40 {
        v.push(uniform(-1e-5, 1e-5));
    }
    // The edges.
    v.extend([
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        -0.5,
        2.0,
        10.0,
        1e-8,
        -1e-8,
        1e-300,
        -1e-300,
        1e300,
        700.0,
        -700.0,
        709.0,
        -745.0,
        f64::MIN_POSITIVE,
        5e-324,
    ]);
    v
}

/// The arguments `pow` is measured on.
fn pow_inputs() -> Vec<(f64, f64)> {
    let mut v: Vec<(f64, f64)> = Vec::new();
    let mut seed: u64 = 0xD1B5_4A32_D192_ED03;
    let mut next = move || {
        seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let mut uniform = |lo: f64, hi: f64| {
        let r = (next() >> 11) as f64 / (1u64 << 53) as f64;
        lo + (hi - lo) * r
    };
    for _ in 0..60 {
        let x = uniform(0.01, 100.0);
        let y = uniform(-2.0, 2.0);
        v.push((x, y));
    }
    // The arguments whose product with the logarithm stays small.
    for _ in 0..60 {
        let x = uniform(0.5, 2.0);
        let y = uniform(-1.0, 1.0);
        v.push((x, y));
    }
    // Integer exponents.
    for y in [-8.0, -5.0, -3.0, -2.0, -1.0, 1.0, 2.0, 3.0, 5.0, 8.0] {
        for x in [0.5, 1.5, 2.0, 3.0, 10.0, 1e-3] {
            v.push((x, y));
        }
    }
    v
}

/// The harness: the runtime's maths, the C library's functions, and a
/// program that runs both over the table and writes the answers down.
fn harness_ir(xs: &[f64], pows: &[(f64, f64)]) -> String {
    let mut s = String::new();
    s.push_str("target triple = \"x86_64-pc-windows-msvc\"\n\n");
    for (_, c) in C_FUNCTIONS {
        s.push_str(&format!("declare double @{c}(double)\n"));
    }
    s.push_str("declare double @pow(double, double)\n");
    s.push_str("declare ptr @fopen(ptr, ptr)\n");
    s.push_str("declare i64 @fwrite(ptr, i64, i64, ptr)\n");
    s.push_str("declare i32 @fclose(ptr)\n");
    s.push_str("@nx_mode = private unnamed_addr constant [3 x i8] c\"wb\\00\"\n\n");

    // The table of arguments.
    s.push_str(&format!(
        "@nx_xs = internal constant [{} x double] [",
        xs.len()
    ));
    for (i, x) in xs.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&format!("double {}", hex(*x)));
    }
    s.push_str("]\n");
    s.push_str(&format!(
        "@nx_pxs = internal constant [{} x double] [",
        pows.len()
    ));
    for (i, (x, _)) in pows.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&format!("double {}", hex(*x)));
    }
    s.push_str("]\n");
    s.push_str(&format!(
        "@nx_ys = internal constant [{} x double] [",
        pows.len()
    ));
    for (i, (_, y)) in pows.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&format!("double {}", hex(*y)));
    }
    s.push_str("]\n");

    // The tables of functions.
    s.push_str("@nx_fns = internal constant [5 x ptr] [");
    for (i, (ours, _)) in C_FUNCTIONS.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&format!("ptr @{ours}"));
    }
    s.push_str("]\n");
    s.push_str("@nx_cfns = internal constant [5 x ptr] [");
    for (i, (_, theirs)) in C_FUNCTIONS.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&format!("ptr @{theirs}"));
    }
    s.push_str("]\n");
    s.push_str("@nx_rfns = internal constant [4 x ptr] [");
    for (i, name) in ROUNDING.iter().enumerate() {
        if i > 0 {
            s.push_str(", ");
        }
        s.push_str(&format!("ptr @nexum_{name}"));
    }
    s.push_str("]\n\n");

    let n = xs.len();
    let m = pows.len();
    let mut p: Vec<String> = Vec::new();
    p.push("define i32 @main(i32 %argc, ptr %argv) {".into());
    p.push("entry:".into());
    p.push("  %pathp = getelementptr i8, ptr %argv, i64 8".into());
    p.push("  %path = load ptr, ptr %pathp".into());
    p.push("  %file = call ptr @fopen(ptr %path, ptr @nx_mode)".into());
    p.push("  %fs = alloca i64".into());
    p.push("  %is = alloca i64".into());
    p.push("  %buf = alloca [2 x double], align 16".into());
    p.push("  %one = alloca [1 x double], align 16".into());
    p.push("  store i64 0, ptr %fs".into());
    // The single-argument functions: the runtime's and the C library's.
    p.push("  br label %floop".into());
    p.push("floop:".into());
    p.push("  %f = load i64, ptr %fs".into());
    p.push("  %fdone = icmp sge i64 %f, 5".into());
    p.push("  br i1 %fdone, label %flend, label %fbody".into());
    p.push("fbody:".into());
    p.push("  %fp1 = getelementptr ptr, ptr @nx_fns, i64 %f".into());
    p.push("  %fpv1 = load ptr, ptr %fp1".into());
    p.push("  %cp = getelementptr ptr, ptr @nx_cfns, i64 %f".into());
    p.push("  %cpv = load ptr, ptr %cp".into());
    p.push("  store i64 0, ptr %is".into());
    p.push("  br label %iloop".into());
    p.push("iloop:".into());
    p.push("  %i1 = load i64, ptr %is".into());
    p.push(format!("  %idone = icmp sge i64 %i1, {n}"));
    p.push("  br i1 %idone, label %iend, label %ibody".into());
    p.push("ibody:".into());
    p.push("  %xp1 = getelementptr double, ptr @nx_xs, i64 %i1".into());
    p.push("  %x1 = load double, ptr %xp1".into());
    p.push("  %v1 = call double %fpv1(double %x1)".into());
    p.push("  %cv1 = call double %cpv(double %x1)".into());
    p.push("  %b01 = getelementptr [2 x double], ptr %buf, i64 0, i64 0".into());
    p.push("  store double %v1, ptr %b01".into());
    p.push("  %b11 = getelementptr [2 x double], ptr %buf, i64 0, i64 1".into());
    p.push("  store double %cv1, ptr %b11".into());
    p.push("  %w1 = call i64 @fwrite(ptr %buf, i64 16, i64 1, ptr %file)".into());
    p.push("  %j1 = add i64 %i1, 1".into());
    p.push("  store i64 %j1, ptr %is".into());
    p.push("  br label %iloop".into());
    p.push("iend:".into());
    p.push("  %f2 = add i64 %f, 1".into());
    p.push("  store i64 %f2, ptr %fs".into());
    p.push("  br label %floop".into());
    p.push("flend:".into());
    // pow.
    p.push("  store i64 0, ptr %is".into());
    p.push("  br label %ploop".into());
    p.push("ploop:".into());
    p.push("  %i2 = load i64, ptr %is".into());
    p.push(format!("  %pdone = icmp sge i64 %i2, {m}"));
    p.push("  br i1 %pdone, label %pend, label %pbody".into());
    p.push("pbody:".into());
    p.push("  %xp2 = getelementptr double, ptr @nx_pxs, i64 %i2".into());
    p.push("  %x2 = load double, ptr %xp2".into());
    p.push("  %yp = getelementptr double, ptr @nx_ys, i64 %i2".into());
    p.push("  %y2 = load double, ptr %yp".into());
    p.push("  %v2 = call double @nexum_pow(double %x2, double %y2)".into());
    p.push("  %cv2 = call double @pow(double %x2, double %y2)".into());
    p.push("  %b02 = getelementptr [2 x double], ptr %buf, i64 0, i64 0".into());
    p.push("  store double %v2, ptr %b02".into());
    p.push("  %b12 = getelementptr [2 x double], ptr %buf, i64 0, i64 1".into());
    p.push("  store double %cv2, ptr %b12".into());
    p.push("  %w2 = call i64 @fwrite(ptr %buf, i64 16, i64 1, ptr %file)".into());
    p.push("  %j2 = add i64 %i2, 1".into());
    p.push("  store i64 %j2, ptr %is".into());
    p.push("  br label %ploop".into());
    p.push("pend:".into());
    // The rounding functions. The C library's are not called here: the
    // runtime has its own names for them, so that it never defines a
    // symbol the C library also defines.
    p.push("  store i64 0, ptr %fs".into());
    p.push("  br label %rloop".into());
    p.push("rloop:".into());
    p.push("  %rf = load i64, ptr %fs".into());
    p.push("  %rdone = icmp sge i64 %rf, 4".into());
    p.push("  br i1 %rdone, label %rend, label %rbody".into());
    p.push("rbody:".into());
    p.push("  %fp4 = getelementptr ptr, ptr @nx_rfns, i64 %rf".into());
    p.push("  %fpv4 = load ptr, ptr %fp4".into());
    p.push("  store i64 0, ptr %is".into());
    p.push("  br label %rloop2".into());
    p.push("rloop2:".into());
    p.push("  %i3 = load i64, ptr %is".into());
    p.push(format!("  %idone3 = icmp sge i64 %i3, {n}"));
    p.push("  br i1 %idone3, label %rend2, label %rbody2".into());
    p.push("rbody2:".into());
    p.push("  %xp3 = getelementptr double, ptr @nx_xs, i64 %i3".into());
    p.push("  %x3 = load double, ptr %xp3".into());
    p.push("  %v3 = call double %fpv4(double %x3)".into());
    p.push("  %b03 = getelementptr [1 x double], ptr %one, i64 0, i64 0".into());
    p.push("  store double %v3, ptr %b03".into());
    p.push("  %w3 = call i64 @fwrite(ptr %one, i64 8, i64 1, ptr %file)".into());
    p.push("  %j3 = add i64 %i3, 1".into());
    p.push("  store i64 %j3, ptr %is".into());
    p.push("  br label %rloop2".into());
    p.push("rend2:".into());
    p.push("  %f3 = add i64 %rf, 1".into());
    p.push("  store i64 %f3, ptr %fs".into());
    p.push("  br label %rloop".into());
    p.push("rend:".into());
    p.push("  %cl = call i32 @fclose(ptr %file)".into());
    p.push("  ret i32 0".into());
    p.push("}".into());
    s.push_str(&p.join("\n"));
    s.push_str("\n\n");

    // The runtime's own maths, at the end so it can call nothing of the
    // harness.
    s.push_str(&math::math_routines());
    s
}

/// The results of one run.
struct Results {
    /// For each of the five single-argument functions, the pairs: the
    /// runtime's answer and the C library's.
    single: Vec<Vec<(f64, f64)>>,
    /// The pairs for `pow`.
    powers: Vec<(f64, f64)>,
    /// For each of the four rounding functions, the runtime's answers.
    rounding: Vec<Vec<f64>>,
}

/// Compile the harness and run it.
fn run_harness(tag: &str, xs: &[f64], pows: &[(f64, f64)]) -> Option<Results> {
    let tc = nexum_toolchain::Toolchain::discover().ok()?;
    let work = work_dir();
    let ll = work.join(format!("math_harness_{tag}.ll"));
    let exe = work.join(format!("math_harness_{tag}.exe"));
    let out = work.join(format!("math_results_{tag}.bin"));
    std::fs::write(&ll, harness_ir(xs, pows)).expect("write ir");
    nexum_toolchain::verify_ir(&tc.clang, ll.to_str().unwrap())
        .unwrap_or_else(|e| panic!("the maths IR does not verify:\n{e}"));
    nexum_toolchain::run_clang(&tc.clang, ll.to_str().unwrap(), exe.to_str().unwrap())
        .unwrap_or_else(|e| panic!("the maths harness does not link:\n{e}"));
    let _ = std::fs::remove_file(&out);
    let run = Command::new(&exe)
        .arg(out.to_str().unwrap())
        .output()
        .expect("run the maths harness");
    assert!(
        run.status.success(),
        "the maths harness failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let bytes = std::fs::read(&out).expect("the harness wrote no results");

    let n = xs.len();
    let m = pows.len();
    let want = (5 * n + m) * 16 + 4 * n * 8;
    assert_eq!(
        bytes.len(),
        want,
        "the harness wrote {} bytes, and {} were expected",
        bytes.len(),
        want
    );

    let mut at = 0usize;
    let mut pairs: Vec<(f64, f64)> = Vec::new();
    for _ in 0..(5 * n + m) {
        let a = f64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        let b = f64::from_le_bytes(bytes[at + 8..at + 16].try_into().unwrap());
        pairs.push((a, b));
        at += 16;
    }
    let mut single = Vec::new();
    let mut rest = pairs;
    for _ in 0..5 {
        let part: Vec<(f64, f64)> = rest.drain(..n).collect();
        single.push(part);
    }
    let powers = rest;
    let mut rounding = Vec::new();
    for _ in 0..4 {
        let mut part = Vec::with_capacity(n);
        for _ in 0..n {
            part.push(f64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()));
            at += 8;
        }
        rounding.push(part);
    }
    Some(Results {
        single,
        powers,
        rounding,
    })
}

/// The measured distance, in ulps, between the runtime's answers and the C
/// library's.
struct Measured {
    worst: i64,
    worst_at: f64,
    count: usize,
}

fn measure(name: &str, pairs: &[(f64, f64)]) -> Measured {
    let mut m = Measured {
        worst: 0,
        worst_at: 0.0,
        count: 0,
    };
    for (i, (v, cv)) in pairs.iter().enumerate() {
        // The C library is the reference. Where it has no number the
        // runtime must have none either: a NaN against a NaN, and an
        // infinity against an infinity of the same sign.
        if !cv.is_finite() {
            assert!(
                !v.is_finite(),
                "{name}: the runtime gave {v} where the C library gave {cv}"
            );
            if cv.is_nan() {
                assert!(v.is_nan(), "{name}: the runtime gave {v}, not a NaN");
            } else {
                assert_eq!(
                    v.is_sign_negative(),
                    cv.is_sign_negative(),
                    "{name}: the runtime gave {v} where the C library gave {cv}"
                );
            }
            continue;
        }
        assert!(
            v.is_finite(),
            "{name}: the runtime gave {v} where the C library gave {cv}"
        );
        let d = ulps_between(*v, *cv);
        m.count += 1;
        if d > m.worst {
            m.worst = d;
            m.worst_at = pairs[i].0;
        }
    }
    m
}

/// The runtime's `exp` against the C library's.
#[test]
fn exp_is_within_one_ulp_of_the_c_library() {
    let xs = inputs();
    let Some(r) = run_harness("exp", &xs, &pow_inputs()) else {
        return;
    };
    let m = measure("exp", &r.single[1]);
    println!(
        "exp: the worst distance from the C library is {} ulp, at {}",
        m.worst, m.worst_at
    );
    assert!(
        m.count > 100,
        "the measurement is too small to mean anything"
    );
    assert!(
        m.worst <= 1,
        "exp is {} ulps from the C library at {}",
        m.worst,
        m.worst_at
    );
}

/// The runtime's `log` against the C library's.
#[test]
fn log_is_within_one_ulp_of_the_c_library() {
    let xs = inputs();
    let Some(r) = run_harness("log", &xs, &pow_inputs()) else {
        return;
    };
    let m = measure("log", &r.single[2]);
    println!(
        "log: the worst distance from the C library is {} ulp, at {}",
        m.worst, m.worst_at
    );
    assert!(
        m.count > 100,
        "the measurement is too small to mean anything"
    );
    assert!(
        m.worst <= 1,
        "log is {} ulps from the C library at {}",
        m.worst,
        m.worst_at
    );
}

/// The runtime's `sqrt` against the C library's.
#[test]
fn sqrt_is_within_one_ulp_of_the_c_library() {
    let xs = inputs();
    let Some(r) = run_harness("sqrt", &xs, &pow_inputs()) else {
        return;
    };
    let m = measure("sqrt", &r.single[0]);
    println!(
        "sqrt: the worst distance from the C library is {} ulp, at {}",
        m.worst, m.worst_at
    );
    assert!(
        m.count > 100,
        "the measurement is too small to mean anything"
    );
    assert!(
        m.worst <= 1,
        "sqrt is {} ulps from the C library at {}",
        m.worst,
        m.worst_at
    );
}

/// The runtime's `tanh` against the C library's.
#[test]
fn tanh_is_within_two_ulps_of_the_c_library() {
    let xs = inputs();
    let Some(r) = run_harness("tanh", &xs, &pow_inputs()) else {
        return;
    };
    let m = measure("tanh", &r.single[3]);
    println!(
        "tanh: the worst distance from the C library is {} ulp, at {}",
        m.worst, m.worst_at
    );
    assert!(
        m.count > 100,
        "the measurement is too small to mean anything"
    );
    assert!(
        m.worst <= 2,
        "tanh is {} ulps from the C library at {}",
        m.worst,
        m.worst_at
    );
}

/// The runtime's `erf` against the C library's.
#[test]
fn erf_is_within_two_ulps_of_the_c_library() {
    let xs = inputs();
    let Some(r) = run_harness("erf", &xs, &pow_inputs()) else {
        return;
    };
    let m = measure("erf", &r.single[4]);
    println!(
        "erf: the worst distance from the C library is {} ulp, at {}",
        m.worst, m.worst_at
    );
    assert!(
        m.count > 100,
        "the measurement is too small to mean anything"
    );
    assert!(
        m.worst <= 2,
        "erf is {} ulps from the C library at {}",
        m.worst,
        m.worst_at
    );
}

/// The runtime's `pow` against the C library's.
///
/// The exponent is a double, and the argument of the exponential it goes
/// through is rounded, so the distance grows with the size of the exponent
/// times the logarithm of the base. The contract is therefore for the
/// arguments a model framework uses: a small product, or an integer
/// exponent, which is computed by squaring.
#[test]
fn pow_is_accurate_for_the_arguments_a_framework_uses() {
    let xs = inputs();
    let pows = pow_inputs();
    let Some(r) = run_harness("pow", &xs, &pows) else {
        return;
    };
    let mut worst_small = 0i64;
    let mut worst_small_at = 0.0;
    let mut worst_int = 0i64;
    let mut worst_int_at = (0.0, 0.0);
    let mut worst_all = 0i64;
    let mut worst_all_at = (0.0, 0.0);
    for ((v, cv), (x, y)) in r.powers.iter().zip(pows.iter()) {
        if !cv.is_finite() {
            assert!(!v.is_finite(), "pow({x}, {y}) gave {v}, the C library {cv}");
            continue;
        }
        assert!(v.is_finite(), "pow({x}, {y}) gave {v}, the C library {cv}");
        let d = ulps_between(*v, *cv);
        if d > worst_all {
            worst_all = d;
            worst_all_at = (*x, *y);
        }
        if y.fract() == 0.0 && d > worst_int {
            worst_int = d;
            worst_int_at = (*x, *y);
        } else if y.fract() != 0.0 && (y * x.ln()).abs() <= 1.0 && d > worst_small {
            worst_small = d;
            worst_small_at = *x;
        }
    }
    println!(
        "pow: the worst distance is {} ulp over every argument, at {:?}",
        worst_all, worst_all_at
    );
    println!(
        "pow: {} ulp for |y*ln(x)| <= 1, at {}",
        worst_small, worst_small_at
    );
    println!(
        "pow: {} ulp for an integer exponent, at {:?}",
        worst_int, worst_int_at
    );
    assert!(
        worst_small <= 2,
        "pow is {} ulps from the C library at {}",
        worst_small,
        worst_small_at
    );
    assert!(
        worst_int <= 8,
        "pow is {} ulps from the C library at {:?}",
        worst_int,
        worst_int_at
    );
}

/// The rounding functions are exact.
///
/// They are compared with Rust's own, which are the C library's.
#[test]
fn the_rounding_functions_are_exact() {
    let mut xs = inputs();
    xs.extend([
        0.5,
        -0.5,
        1.5,
        -1.5,
        2.5,
        -2.5,
        0.49999999999999994,
        -0.49999999999999994,
        4503599627370495.5,
        9007199254740993.0,
    ]);
    let Some(r) = run_harness("round", &xs, &pow_inputs()) else {
        return;
    };
    for (i, name) in ROUNDING.iter().enumerate() {
        for (j, v) in r.rounding[i].iter().enumerate() {
            let want = match *name {
                "floor" => xs[j].floor(),
                "ceil" => xs[j].ceil(),
                "trunc" => xs[j].trunc(),
                _ => xs[j].round(),
            };
            assert_eq!(
                *v, want,
                "{name}({}) gave {v} and the C library gave {want}",
                xs[j]
            );
        }
    }
}

/// The IR is the shape the phase requires.
#[test]
fn the_maths_ir_has_no_c_library_dependency() {
    let ir = math::math_routines();
    // A 128-bit division is a call to `__udivti3`, which is a C library
    // dependency.
    assert!(
        !ir.contains("udiv i128") && !ir.contains("sdiv i128"),
        "the maths IR divides 128 bits wide"
    );
    // The maths must not declare anything: a program that uses it should
    // still need nothing but the kernel.
    for line in ir.lines() {
        assert!(
            !line.starts_with("declare "),
            "the maths IR declares something: {line}"
        );
    }
    // And it must not call the C library either.
    for name in [
        "exp",
        "log",
        "tanh",
        "pow",
        "erf",
        "sqrt",
        "fmod",
        "ldexp",
        "scalbn",
        "frexp",
        "__udivti3",
        "__divti3",
    ] {
        assert!(
            !ir.contains(&format!("@{name}(")),
            "the maths IR calls the C library's {name}"
        );
    }
    // The rounding functions are the ones the C library would be asked for,
    // so the runtime has to define them itself rather than call them.
    for name in ["floor", "ceil", "trunc", "round"] {
        assert!(
            !ir.contains(&format!("call double @{name}(")),
            "the maths IR calls the C library's {name}"
        );
        assert!(
            ir.contains(&format!("define double @nexum_{name}(")),
            "the maths IR does not define nexum_{name}"
        );
    }
}

/// The names are the ones the contract fixes, and they are visible.
#[test]
fn the_maths_functions_are_external_and_named_right() {
    let ir = math::math_routines();
    for name in [
        "nexum_sqrt",
        "nexum_exp",
        "nexum_log",
        "nexum_tanh",
        "nexum_pow",
        "nexum_erf",
        "nexum_floor",
        "nexum_ceil",
        "nexum_trunc",
        "nexum_round",
    ] {
        assert!(
            ir.contains(&format!("define double @{name}(")),
            "the maths IR does not define {name}"
        );
        assert!(
            !ir.contains(&format!("define internal double @{name}(")),
            "{name} is internal, and the code generator cannot call it"
        );
    }
}

/// The harness wrote what it was asked to write.
#[test]
fn the_harness_writes_its_results() {
    let xs = inputs();
    let pows = pow_inputs();
    let Some(r) = run_harness("results", &xs, &pows) else {
        return;
    };
    let total: usize = r.single.iter().map(|s| s.len()).sum::<usize>() + r.powers.len();
    assert_eq!(
        total,
        C_FUNCTIONS.len() * xs.len() + pows.len(),
        "the harness wrote the wrong number of results"
    );
}
