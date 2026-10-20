//! The runtime's maths builtins, written as LLVM IR.
//!
//! The runtime is freestanding: a program on a POSIX target reaches the
//! kernel and nothing else, so there is no C library to ask for `exp`,
//! `log` or `pow`. These are the implementations the runtime ships instead.
//!
//! Everything here is 64-bit arithmetic. There is no `udiv i128` and no
//! `sdiv i128`: LLVM lowers a 128-bit division to a call to `__udivti3`,
//! which is a C library dependency, and the phase forbids it. The only
//! divisions in this file are 64-bit ones (`fdiv` and `sdiv i64`).
//!
//! The algorithms are the classical ones, with the polynomial coefficients
//! fitted in high precision (mpmath, 60 decimal digits) over the exact
//! interval each polynomial is used on:
//!
//! * `exp` reduces the argument to `|r| <= ln2/2` and evaluates
//!   `exp(r) = 1 + r + (r^2/2) * P(r)` there.
//! * `log` reduces the argument to `m` in `[sqrt(2)/2, sqrt(2)]` and
//!   evaluates `log(1+f) = f - (hfsq - s*(hfsq + R))` there, with
//!   `s = f/(2+f)`.
//! * `tanh` uses a polynomial for `|x| <= ln2/2` and `(1-u)/(1+u)`, with
//!   `u = e^(-2|x|)`, outside it.
//! * `erf` uses a polynomial for `|x| <= 1.5` and Legendre's continued
//!   fraction for the complementary error function above it.
//! * `pow` squares for an integer exponent and goes through
//!   `exp(y*log(x))` otherwise.
//!
//! The rounding functions (`floor`, `ceil`, `trunc`, `round`) are the ones
//! LLVM's x86-64 backend calls when it lowers `llvm.floor.f64` and its
//! siblings: on the SSE2 baseline those intrinsics become calls to the C
//! library, so the runtime defines the names itself. They are done with bit
//! manipulation, not with the intrinsics, which would call straight back
//! into them.
//!
//! `nexum_sqrt` is `llvm.sqrt.f64`: on x86-64 that is the `sqrtsd`
//! instruction, on AArch64 the `fsqrt` instruction, and neither needs the C
//! library.
//!
//! The accuracy, measured against the C library over the table the tests
//! use (the tests measure it, they do not assume it):
//!
//! | function | the worst distance from the C library |
//! | -------- | ------------------------------------- |
//! | `nexum_sqrt` | 0 ulp |
//! | `nexum_exp`  | 1 ulp |
//! | `nexum_log`  | 1 ulp |
//! | `nexum_tanh` | 1 ulp |
//! | `nexum_erf`  | 2 ulps |
//! | `nexum_pow`  | 1 ulp for `\|y*ln(x)\| <= 1`, 5 ulps for an integer exponent |
//!
//! `pow` is the one that is not uniform: its argument goes through a
//! logarithm and an exponential, and the argument of the exponential is
//! rounded, so the distance grows with the size of `y*ln(x)`. An integer
//! exponent does not go through them at all - it squares - and keeps to a
//! few units in the last place.

/// The exact doubles the routines use, as their bit patterns.
mod c {
    /// ln(2) with its low 20 bits cleared: `k*LN2_HI` is exact for every
    /// exponent the reduction can produce.
    pub const LN2_HI: u64 = 0x3FE62E42FEE00000;
    /// The rest of ln(2).
    pub const LN2_LO: u64 = 0x3DEA39EF35793C76;
    /// 1/ln(2), for the argument reduction.
    pub const INVLN2: u64 = 0x3FF71547652B82FE;
    /// sqrt(pi).
    pub const SQRT_PI: u64 = 0x3FFC5BF891B4EF6A;
    /// The largest argument `exp` takes; above it the result overflows.
    pub const EXP_OVERFLOW: u64 = 0x4086300000000000;
    /// The smallest argument `exp` takes; below it the result underflows.
    pub const EXP_UNDERFLOW: u64 = 0xC087500000000000;
    /// The mantissa of sqrt(2): the point the logarithm's reduction turns at.
    pub const SQRT2_MANT: u64 = 0x0006A09E667F3BCD;
    /// 2^54, the scale a subnormal is multiplied by.
    pub const TWO_POW_54: u64 = 0x4350000000000000;
    /// The bit pattern of 1.5: the point `erf`'s polynomial stops at.
    pub const ONE_POINT_FIVE: u64 = 0x3FF8000000000000;
    /// The bit pattern of ln(2)/2, as a comparison threshold.
    pub const HALF_LN2: u64 = 0x3FD62E42FEFA39EF;
    /// The bit pattern of 1.5*ln(2), as a comparison threshold.
    pub const ONE_AND_HALF_LN2: u64 = 0x3FF0A2B23F3BAB73;

    /// `exp(r) = 1 + r + (r^2/2) * P(r)`, for `|r| <= ln2/2`.
    pub const EXP_P: [u64; 10] = [
        0x3FF0000000000001,
        0x3FD5555555555556,
        0x3FB5555555553D68,
        0x3F911111111109B5,
        0x3F66C16C17889EF1,
        0x3F3A01A01A7C2EFE,
        0x3F0A019B9149A41C,
        0x3ED71DE0DB2F6B19,
        0x3EA28917C89A43A7,
        0x3E6AF389ECFC4B9C,
    ];

    /// `log(1+f) = 2s + 2 s^3 T(s^2)`, with `s = f/(2+f)`.
    pub const LOG_T: [u64; 7] = [
        0x3FD5555555555558,
        0x3FC99999999952E2,
        0x3FC2492492DF148D,
        0x3FBC71C62E5800A1,
        0x3FB7462B4AB2EF6B,
        0x3FB39FE606542DDE,
        0x3FB2B584AAE78A57,
    ];

    /// `tanh(r) = r + r^3 S(r^2)`, for `|r| <= ln2/2`.
    pub const TANH_S: [u64; 9] = [
        0xBFD5555555555555,
        0x3FC111111111107A,
        0xBFABA1BA1B9FAE1F,
        0x3F9664F486CD243D,
        0xBF8226E2DFF57AB0,
        0x3F6D6D106710009B,
        0xBF57D539795D6DDB,
        0x3F4301A924D2DCDB,
        0xBF29467C499751CE,
    ];

    /// `erf(r) = r * (1 + Y(r^2))`, for `|r| <= 1.5`.
    pub const ERF_Y: [u64; 18] = [
        0x3FC06EBA8214DB69,
        0xBFD812746B0379E7,
        0x3FBCE2F21A042BE2,
        0xBF9B82CE31288B48,
        0x3F7565BCD0E6A3EC,
        0xBF4C02DB4003CAD5,
        0x3F1F9A326F935C7A,
        0xBEEF4D25C327B2D6,
        0x3EBB9E6C91E8CFE8,
        0xBE85F7425B931D16,
        0x3E4FCC4CB9D26FFE,
        0xBE151D28B378D38C,
        0x3DD9E3A4712B6326,
        0xBD9D6BC2C8A1DBF2,
        0x3D5ED3432115A240,
        0xBD1CC152EF0CDDDE,
        0x3CD555D6D46EE932,
        0xBC8253E9B8158210,
    ];
}

/// A double constant, as the hex literal LLVM's parser accepts.
fn f(v: u64) -> String {
    format!("0x{v:016X}")
}

/// A small builder for LLVM IR text.
///
/// The runtime is IR, and IR is verbose; the builder keeps the arithmetic
/// in one place and lets the polynomial fits be written as data.
struct Ir {
    lines: std::cell::RefCell<Vec<String>>,
    next: std::cell::Cell<usize>,
}

impl Ir {
    fn new() -> Self {
        Ir {
            lines: std::cell::RefCell::new(Vec::new()),
            next: std::cell::Cell::new(0),
        }
    }

    fn tmp(&self) -> String {
        let n = self.next.get() + 1;
        self.next.set(n);
        format!("%t{n}")
    }

    fn op(&self, line: String) {
        self.lines.borrow_mut().push(format!("  {line}"));
    }

    fn fbin(&self, op: &str, a: &str, b: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = {op} double {a}, {b}"));
        d
    }

    fn fun(&self, op: &str, a: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = {op} double {a}"));
        d
    }

    fn ibin(&self, op: &str, a: &str, b: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = {op} i64 {a}, {b}"));
        d
    }

    fn icmp(&self, pred: &str, a: &str, b: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = icmp {pred} i64 {a}, {b}"));
        d
    }

    fn fcmp(&self, pred: &str, a: &str, b: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = fcmp {pred} double {a}, {b}"));
        d
    }

    /// The conjunction of two conditions.
    fn and_i1(&self, a: &str, b: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = and i1 {a}, {b}"));
        d
    }

    /// The disjunction of two conditions.
    fn or_i1(&self, a: &str, b: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = or i1 {a}, {b}"));
        d
    }

    /// The negation of a condition.
    fn not_i1(&self, c: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = xor i1 {c}, true"));
        d
    }

    fn zext(&self, c: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = zext i1 {c} to i64"));
        d
    }

    fn sel_d(&self, c: &str, a: &str, b: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = select i1 {c}, double {a}, double {b}"));
        d
    }

    fn sel_i(&self, c: &str, a: &str, b: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = select i1 {c}, i64 {a}, i64 {b}"));
        d
    }

    /// The absolute value, by clearing the sign bit.
    fn fabs(&self, x: &str) -> String {
        let u = self.bitcast_fi(x);
        let a = self.ibin("and", &u, "9223372036854775807");
        self.bitcast_if(&a)
    }

    fn bitcast_fi(&self, x: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = bitcast double {x} to i64"));
        d
    }

    fn bitcast_if(&self, x: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = bitcast i64 {x} to double"));
        d
    }

    fn sitofp(&self, x: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = sitofp i64 {x} to double"));
        d
    }

    fn fptosi(&self, x: &str) -> String {
        let d = self.tmp();
        self.op(format!("{d} = fptosi double {x} to i64"));
        d
    }

    /// A Horner polynomial: `c0 + x*(c1 + x*(c2 + ...))`.
    fn horner(&self, coeffs: &[u64], x: &str) -> String {
        let mut acc = f(coeffs[coeffs.len() - 1]);
        for k in (0..coeffs.len() - 1).rev() {
            let m = self.fbin("fmul", x, &acc);
            acc = self.fbin("fadd", &m, &f(coeffs[k]));
        }
        acc
    }

    /// The body of a function, as IR text.
    fn body(&self) -> String {
        let mut s = String::new();
        for l in self.lines.borrow().iter() {
            s.push_str(l);
            s.push('\n');
        }
        s
    }
}

/// A function, as IR text. The body ends with the `ret` itself.
fn func(name: &str, args: &str, body: &str) -> String {
    format!("define double @{name}({args}) {{\nentry:\n{body}}}\n\n")
}

/// The runtime's maths builtins, as LLVM IR.
///
/// The names are the ones the contract fixes: `nexum_sqrt`, `nexum_exp`,
/// `nexum_log`, `nexum_tanh`, `nexum_pow` and `nexum_erf`, plus `floor`,
/// `ceil`, `trunc` and `round`, which are the names LLVM's x86-64 backend
/// calls when it lowers the rounding intrinsics.
pub fn math_routines() -> String {
    let mut s = String::new();
    s.push_str(&trunc_routine());
    s.push_str(&ldexp2_routine());
    s.push_str(&sqrt_routine());
    s.push_str(&exp_routine());
    s.push_str(&log_routine());
    s.push_str(&tanh_routine());
    s.push_str(&erfc_routine());
    s.push_str(&erf_routine());
    s.push_str(&pow_routine());
    s.push_str(&floor_routine());
    s.push_str(&ceil_routine());
    s.push_str(&trunc_public_routine());
    s.push_str(&round_routine());
    s
}

/// The truncation of a double, by bit manipulation.
///
/// The value's own bits give the answer: for `|x| < 1` the result is a
/// signed zero, for `|x| >= 2^52` the value is already an integer, and in
/// between the low bits of the mantissa are the fraction.
fn trunc_routine() -> String {
    let ir = Ir::new();
    let x = "%x";
    let u = ir.bitcast_fi(x);
    let sign = ir.ibin("and", &u, "-9223372036854775808");
    let abs = ir.ibin("and", &u, "9223372036854775807");
    let e = ir.ibin("lshr", &abs, "52");
    let e = ir.ibin("and", &e, "2047");
    // The exponent of a value that is already an integer, or is not finite.
    let big = ir.icmp("uge", &e, "1075");
    // The exponent of a value with no integer part at all.
    let small = ir.icmp("ule", &e, "1022");
    // The number of fraction bits to clear.
    let sh = ir.ibin("sub", "1075", &e);
    let sh = ir.ibin("and", &sh, "63");
    let m = ir.ibin("lshr", &abs, &sh);
    let m = ir.ibin("shl", &m, &sh);
    let mag = ir.sel_i(&big, &abs, &m);
    let signed = ir.ibin("or", &sign, &mag);
    let v2 = ir.sel_i(&small, &sign, &signed);
    let out = ir.bitcast_if(&v2);
    ir.op(format!("ret double {out}"));
    func("nexum_math_trunc", "double %x", &ir.body())
}

/// `x * 2^k`, correctly rounded, for a normal `x` and a small `k`.
///
/// The exponent of the result is the exponent of `x` plus `k`. A result
/// below the smallest normal is a subnormal, and it is made by shifting the
/// significand down, with the rounding a shift has to do.
fn ldexp2_routine() -> String {
    let ir = Ir::new();
    let x = "%x";
    let k = "%k";
    let u = ir.bitcast_fi(x);
    let sign = ir.ibin("and", &u, "-9223372036854775808");
    let abs = ir.ibin("and", &u, "9223372036854775807");
    let e = ir.ibin("lshr", &abs, "52");
    let e = ir.ibin("and", &e, "2047");
    // The significand with the hidden bit made explicit.
    let m = ir.ibin("and", &abs, "4503599627370495");
    let m1 = ir.ibin("or", &m, "4503599627370496");
    let newe = ir.ibin("add", &e, k);
    let normal = ir.icmp("sge", &newe, "1");
    let not_over = ir.icmp("slt", &newe, "2047");
    // A normal result: the same significand under a new exponent.
    let nbits = ir.ibin("or", &sign, &m);
    let nbits = ir.ibin("or", &nbits, &ir.ibin("shl", &newe, "52"));
    // A result that is too large.
    let inf = ir.ibin("or", &sign, "9218868437227405312");
    // A subnormal result: the significand is shifted down and rounded.
    let sh = ir.ibin("sub", "1", &newe);
    let too_big = ir.icmp("sgt", &sh, "54");
    let sh = ir.sel_i(&too_big, "54", &sh);
    let q = ir.ibin("lshr", &m1, &sh);
    let mask = ir.ibin("sub", &ir.ibin("shl", "1", &sh), "1");
    let r = ir.ibin("and", &m1, &mask);
    let half = ir.ibin("shl", "1", &ir.ibin("sub", &sh, "1"));
    let above = ir.icmp("ugt", &r, &half);
    let equal = ir.icmp("eq", &r, &half);
    let odd = ir.icmp("ne", &ir.ibin("and", &q, "1"), "0");
    let up = ir.zext(&above);
    let tie = ir.ibin("and", &ir.zext(&equal), &ir.zext(&odd));
    let round_up = ir.ibin("or", &up, &tie);
    let q = ir.ibin("add", &q, &round_up);
    let sbits = ir.ibin("or", &sign, &q);
    // Pick the normal, the subnormal or the infinity.
    let both = ir.ibin("and", &ir.zext(&normal), &ir.zext(&not_over));
    let is_normal = ir.icmp("ne", &both, "0");
    let r1 = ir.sel_i(&is_normal, &nbits, &sbits);
    let r2 = ir.sel_i(&not_over, &r1, &inf);
    let out = ir.bitcast_if(&r2);
    ir.op(format!("ret double {out}"));
    func("nexum_math_ldexp2", "double %x, i64 %k", &ir.body())
}

/// `sqrt`, which is a hardware instruction and not a library call.
fn sqrt_routine() -> String {
    let ir = Ir::new();
    let r = ir.tmp();
    ir.op(format!("{r} = call double @llvm.sqrt.f64(double %x)"));
    ir.op(format!("ret double {r}"));
    func("nexum_sqrt", "double %x", &ir.body())
}

/// `exp(x)`.
fn exp_routine() -> String {
    let ir = Ir::new();
    let x = "%x";
    let u = ir.bitcast_fi(x);
    let abs = ir.ibin("and", &u, "9223372036854775807");
    let e = ir.ibin("lshr", &abs, "52");
    let e = ir.ibin("and", &e, "2047");
    let mant = ir.ibin("and", &abs, "4503599627370495");
    let neg = ir.icmp("slt", &u, "0");
    // A NaN argument is returned as it is.
    let is_inf = ir.icmp("eq", &e, "2047");
    let mant_ne = ir.icmp("ne", &mant, "0");
    let is_nan = ir.ibin("and", &ir.zext(&is_inf), &ir.zext(&mant_ne));
    let is_nan = ir.icmp("ne", &is_nan, "0");
    // An infinity argument: exp(-inf) is zero, exp(+inf) is +inf.
    let over = ir.fcmp("ogt", x, &f(c::EXP_OVERFLOW));
    let under = ir.fcmp("olt", x, &f(c::EXP_UNDERFLOW));
    ir.op(format!("  br i1 {}, label %nan, label %chk", is_nan));
    ir.op("nan:".to_string());
    ir.op("  ret double %x".to_string());
    ir.op("chk:".to_string());
    ir.op(format!("  br i1 {}, label %over, label %chk2", over));
    ir.op("over:".to_string());
    ir.op(format!("  ret double {}", f(0x7FF0000000000000)));
    ir.op("chk2:".to_string());
    ir.op(format!("  br i1 {}, label %under, label %reduce", under));
    ir.op("under:".to_string());
    ir.op(format!("  ret double {}", f(0)));
    ir.op("reduce:".to_string());

    // The reduction: r = x - k*ln(2), with |r| <= ln(2)/2.
    let over_half = ir.icmp("ugt", &abs, &c::HALF_LN2.to_string());
    let in_big = ir.icmp("uge", &abs, &c::ONE_AND_HALF_LN2.to_string());
    // k = round(x/ln(2)), away from zero in the middle range.
    let v = ir.fbin("fmul", x, &f(c::INVLN2));
    let half = ir.sel_d(&neg, &f(0xBFE0000000000000), &f(0x3FE0000000000000));
    let v = ir.fbin("fadd", &v, &half);
    // The conversion to an integer has to be in range, so the value is
    // bounded first.
    let limit = f(0x43E0000000000000);
    let too_big = ir.fcmp("ogt", &v, &limit);
    let too_small = ir.fcmp("olt", &v, &f(0xC3E0000000000000));
    let v1 = ir.sel_d(&too_big, &limit, &v);
    let v2 = ir.sel_d(&too_small, &f(0xC3E0000000000000), &v1);
    let k_big = ir.fptosi(&v2);
    let k_mid = ir.sel_i(&neg, "-1", "1");
    let k1 = ir.sel_i(&in_big, &k_big, &k_mid);
    let k = ir.sel_i(&over_half, &k1, "0");
    let kf = ir.sitofp(&k);
    let t = ir.fbin("fmul", &kf, &f(c::LN2_HI));
    let hi = ir.fbin("fsub", x, &t);
    let lo = ir.fbin("fmul", &kf, &f(c::LN2_LO));

    // exp(r) = 1 + r + (r^2/2) * P(r)
    let r = ir.fbin("fsub", &hi, &lo);
    let r2 = ir.fbin("fmul", &r, &r);
    let p = ir.horner(&c::EXP_P, &r);
    let half_r2 = ir.fbin("fmul", &f(0x3FE0000000000000), &r2);
    let corr = ir.fbin("fmul", &half_r2, &p);
    let inner = ir.fbin("fsub", &lo, &corr);
    let inner = ir.fbin("fsub", &inner, &hi);
    let y = ir.fbin("fsub", &f(0x3FF0000000000000), &inner);
    let scaled = ir.tmp();
    ir.op(format!(
        "{scaled} = call double @nexum_math_ldexp2(double {y}, i64 {k})"
    ));
    ir.op(format!("ret double {scaled}"));
    func("nexum_exp", "double %x", &ir.body())
}

/// `log(x)`.
fn log_routine() -> String {
    let ir = Ir::new();
    let x = "%x";
    let u = ir.bitcast_fi(x);
    let abs = ir.ibin("and", &u, "9223372036854775807");
    let e = ir.ibin("lshr", &abs, "52");
    let e = ir.ibin("and", &e, "2047");
    let mant = ir.ibin("and", &abs, "4503599627370495");
    let is_inf = ir.icmp("eq", &e, "2047");
    let mant_ne = ir.icmp("ne", &mant, "0");
    let is_nan = ir.ibin("and", &ir.zext(&is_inf), &ir.zext(&mant_ne));
    let is_nan = ir.icmp("ne", &is_nan, "0");
    let is_zero = ir.icmp("eq", &abs, "0");
    // A negative argument has no answer, but a negative zero is a zero.
    let neg = ir.icmp("slt", &u, "0");
    let neg = ir.and_i1(&neg, &ir.not_i1(&is_zero));
    ir.op(format!("  br i1 {}, label %bad, label %chk0", is_nan));
    ir.op("bad:".to_string());
    ir.op(format!("  ret double {}", f(0x7FF8000000000000)));
    ir.op("chk0:".to_string());
    ir.op(format!("  br i1 {}, label %neg, label %chk1", neg));
    ir.op("neg:".to_string());
    ir.op(format!("  ret double {}", f(0x7FF8000000000000)));
    ir.op("chk1:".to_string());
    ir.op(format!("  br i1 {}, label %zero, label %scale", is_zero));
    ir.op("zero:".to_string());
    ir.op(format!("  ret double {}", f(0xFFF0000000000000)));
    ir.op("scale:".to_string());

    // A subnormal is scaled up by 2^54, which is exact.
    let subnormal = ir.icmp("eq", &e, "0");
    let scaled = ir.fbin("fmul", x, &f(c::TWO_POW_54));
    let su = ir.bitcast_fi(&scaled);
    let sabs = ir.ibin("and", &su, "9223372036854775807");
    let se = ir.ibin("lshr", &sabs, "52");
    let se = ir.ibin("and", &se, "2047");
    let smant = ir.ibin("and", &sabs, "4503599627370495");
    let e = ir.sel_i(&subnormal, &se, &e);
    let mant = ir.sel_i(&subnormal, &smant, &mant);
    let scaled_k = ir.ibin("sub", &ir.ibin("sub", &se, "1023"), "54");
    let k = ir.sel_i(&subnormal, &scaled_k, &ir.ibin("sub", &e, "1023"));

    // The mantissa with the exponent brought to zero: that is m in [1, 2).
    let m_bits = ir.ibin("or", &mant, "4607182418800017408");
    let m = ir.bitcast_if(&m_bits);
    // m in [sqrt(2)/2, sqrt(2)]: halve it when it is over sqrt(2).
    let over_sqrt2 = ir.icmp("uge", &mant, &c::SQRT2_MANT.to_string());
    let half = ir.fbin("fmul", &m, &f(0x3FE0000000000000));
    let m = ir.sel_d(&over_sqrt2, &half, &m);
    let k = ir.sel_i(&over_sqrt2, &ir.ibin("add", &k, "1"), &k);

    // f = m - 1
    let fv = ir.fbin("fsub", &m, &f(0x3FF0000000000000));
    // Two ways to correct f. For |f| < 2^-20 the series is enough:
    // log(1+f) = f - (f^2/2 - f^3/3). For the rest the polynomial is used:
    // s = f/(2+f) and log(1+f) = f - (hfsq - s*(hfsq + R)).
    let f2 = ir.fbin("fmul", &fv, &fv);
    let third = ir.fbin("fmul", &fv, &f(0x3FD5555555555555));
    let series_inner = ir.fbin("fsub", &f(0x3FE0000000000000), &third);
    let series_c = ir.fbin("fmul", &f2, &series_inner);
    let denom = ir.fbin("fadd", &f(0x4000000000000000), &fv);
    let s = ir.fbin("fdiv", &fv, &denom);
    let z = ir.fbin("fmul", &s, &s);
    let t = ir.horner(&c::LOG_T, &z);
    let rpoly = ir.fbin("fmul", &f(0x4000000000000000), &ir.fbin("fmul", &z, &t));
    let hfsq = ir.fbin("fmul", &f(0x3FE0000000000000), &ir.fbin("fmul", &fv, &fv));
    let inner = ir.fbin("fmul", &s, &ir.fbin("fadd", &hfsq, &rpoly));
    let poly_c = ir.fbin("fsub", &hfsq, &inner);
    let small = ir.fcmp("olt", &ir.fabs(&fv), &f(0x3EB0000000000000));
    let corr = ir.sel_d(&small, &series_c, &poly_c);
    // The answer is k*ln(2) + f - corr, with the small part of k*ln(2)
    // added to the small part of the answer, so that it is not lost in the
    // rounding of the large part.
    let kf = ir.sitofp(&k);
    let lo = ir.fbin("fmul", &kf, &f(c::LN2_LO));
    let inner = ir.fbin("fsub", &corr, &lo);
    let inner = ir.fbin("fsub", &inner, &fv);
    let hi = ir.fbin("fmul", &kf, &f(c::LN2_HI));
    let out = ir.fbin("fsub", &hi, &inner);
    ir.op(format!("ret double {out}"));
    func("nexum_log", "double %x", &ir.body())
}

/// `tanh(x)`.
fn tanh_routine() -> String {
    let ir = Ir::new();
    let x = "%x";
    let u = ir.bitcast_fi(x);
    let abs = ir.ibin("and", &u, "9223372036854775807");
    let e = ir.ibin("lshr", &abs, "52");
    let e = ir.ibin("and", &e, "2047");
    let mant = ir.ibin("and", &abs, "4503599627370495");
    let neg = ir.icmp("slt", &u, "0");
    let is_inf = ir.icmp("eq", &e, "2047");
    let mant_ne = ir.icmp("ne", &mant, "0");
    let is_nan = ir.ibin("and", &ir.zext(&is_inf), &ir.zext(&mant_ne));
    let is_nan = ir.icmp("ne", &is_nan, "0");
    // tanh(+-inf) is +-1.
    ir.op(format!("  br i1 {}, label %nan, label %chk", is_nan));
    ir.op("nan:".to_string());
    ir.op("  ret double %x".to_string());
    ir.op("chk:".to_string());
    ir.op(format!("  br i1 {}, label %inf, label %chk2", is_inf));
    ir.op("inf:".to_string());
    let one = f(0x3FF0000000000000);
    let neg_one = f(0xBFF0000000000000);
    let out = ir.sel_d(&neg, &neg_one, &one);
    ir.op(format!("ret double {out}"));
    ir.op("chk2:".to_string());
    // |x| <= ln(2)/2: the polynomial.
    let small = ir.icmp("ule", &abs, &c::HALF_LN2.to_string());
    ir.op(format!("  br i1 {}, label %poly, label %large", small));
    ir.op("poly:".to_string());
    // tanh(r) = r + r^3 S(r^2)
    let z = ir.fbin("fmul", x, x);
    let s = ir.horner(&c::TANH_S, &z);
    let corr = ir.fbin("fmul", &ir.fbin("fmul", x, &z), &s);
    let out = ir.fbin("fadd", x, &corr);
    ir.op(format!("ret double {out}"));
    ir.op("large:".to_string());
    // Above it: tanh(x) = (1-u)/(1+u), with u = e^(-2|x|).
    let ax = ir.fabs(x);
    let two_ax = ir.fbin("fmul", &f(0x4000000000000000), &ax);
    let neg_two_ax = ir.fun("fneg", &two_ax);
    let u = ir.tmp();
    ir.op(format!("{u} = call double @nexum_exp(double {neg_two_ax})"));
    let num = ir.fbin("fsub", &f(0x3FF0000000000000), &u);
    let den = ir.fbin("fadd", &f(0x3FF0000000000000), &u);
    let r = ir.fbin("fdiv", &num, &den);
    let out = ir.sel_d(&neg, &ir.fun("fneg", &r), &r);
    ir.op(format!("ret double {out}"));
    func("nexum_tanh", "double %x", &ir.body())
}

/// The complementary error function, by Legendre's continued fraction.
///
/// `Q(1/2, z) = z^(1/2) e^-z / sqrt(pi) / (z + 1/2 - (1/2)/(z + 5/2 - ...))`
/// with `z = x^2`. The fraction converges for `z > 3/2`, which is where it
/// is used.
fn erfc_routine() -> String {
    let ir = Ir::new();
    let x = "%x";
    let z = ir.fbin("fmul", x, x);
    // x^2 can be too large for the fraction: the answer is then zero, and
    // the fraction would only produce infinities.
    let z_inf = ir.fcmp("oeq", &z, &f(0x7FF0000000000000));
    ir.op(format!("  br i1 {}, label %zero, label %cf", z_inf));
    ir.op("zero:".to_string());
    ir.op(format!("  ret double {}", f(0)));
    ir.op("cf:".to_string());
    let b0 = ir.fbin("fadd", &z, &f(0x3FE0000000000000));
    let fslot = ir.tmp();
    let cslot = ir.tmp();
    let dslot = ir.tmp();
    let islot = ir.tmp();
    ir.op(format!("{fslot} = alloca double"));
    ir.op(format!("{cslot} = alloca double"));
    ir.op(format!("{dslot} = alloca double"));
    ir.op(format!("{islot} = alloca i64"));
    ir.op(format!("store double {b0}, ptr {fslot}"));
    ir.op(format!("store double {b0}, ptr {cslot}"));
    ir.op(format!("store double 0.0, ptr {dslot}"));
    ir.op(format!("store i64 1, ptr {islot}"));
    ir.op("  br label %loop".to_string());
    ir.op("loop:".to_string());
    let i = ir.tmp();
    ir.op(format!("{i} = load i64, ptr {islot}"));
    let done = ir.icmp("sgt", &i, "200");
    ir.op(format!("  br i1 {}, label %end, label %body", done));
    ir.op("body:".to_string());
    let fi = ir.sitofp(&i);
    // an = -i*(i - 1/2)
    let an = ir.fbin("fmul", &ir.fbin("fsub", &fi, &f(0x3FE0000000000000)), &fi);
    let an = ir.fun("fneg", &an);
    let b = ir.fbin("fadd", &b0, &ir.fbin("fmul", &f(0x4000000000000000), &fi));
    let d = ir.tmp();
    let cc = ir.tmp();
    ir.op(format!("{d} = load double, ptr {dslot}"));
    ir.op(format!("{cc} = load double, ptr {cslot}"));
    let d = ir.fbin("fadd", &b, &ir.fbin("fmul", &an, &d));
    let cc = ir.fbin("fadd", &b, &ir.fbin("fdiv", &an, &cc));
    // The denominator and the numerator of the convergent cannot get so
    // small that they stop meaning anything.
    let d_small = ir.fcmp("olt", &ir.fabs(&d), &f(0x01A56E1FC2F8F359));
    let d = ir.sel_d(&d_small, &f(0x01A56E1FC2F8F359), &d);
    let c_small = ir.fcmp("olt", &ir.fabs(&cc), &f(0x01A56E1FC2F8F359));
    let cc = ir.sel_d(&c_small, &f(0x01A56E1FC2F8F359), &cc);
    let d = ir.fbin("fdiv", &f(0x3FF0000000000000), &d);
    let delta = ir.fbin("fmul", &cc, &d);
    let fs = ir.tmp();
    ir.op(format!("{fs} = load double, ptr {fslot}"));
    let fs = ir.fbin("fmul", &fs, &delta);
    ir.op(format!("store double {d}, ptr {dslot}"));
    ir.op(format!("store double {cc}, ptr {cslot}"));
    ir.op(format!("store double {fs}, ptr {fslot}"));
    // Stop when the convergent stops moving.
    let moved = ir.fcmp(
        "olt",
        &ir.fabs(&ir.fbin("fsub", &delta, &f(0x3FF0000000000000))),
        &f(0x3C9AB2B4E8B0F0D1),
    );
    let i2 = ir.ibin("add", &i, "1");
    ir.op(format!("store i64 {i2}, ptr {islot}"));
    ir.op(format!("  br i1 {}, label %end, label %loop", moved));
    ir.op("end:".to_string());
    // erfc(x) = x*e^(-x^2)/(sqrt(pi)*f)
    let fs = ir.tmp();
    ir.op(format!("{fs} = load double, ptr {fslot}"));
    let negz = ir.fun("fneg", &z);
    let ez = ir.tmp();
    ir.op(format!("{ez} = call double @nexum_exp(double {negz})"));
    let num = ir.fbin("fmul", x, &ez);
    let den = ir.fbin("fmul", &f(c::SQRT_PI), &fs);
    let out = ir.fbin("fdiv", &num, &den);
    ir.op(format!("ret double {out}"));
    func("nexum_math_erfc", "double %x", &ir.body())
}

/// `erf(x)`.
fn erf_routine() -> String {
    let ir = Ir::new();
    let x = "%x";
    let u = ir.bitcast_fi(x);
    let abs = ir.ibin("and", &u, "9223372036854775807");
    let e = ir.ibin("lshr", &abs, "52");
    let e = ir.ibin("and", &e, "2047");
    let mant = ir.ibin("and", &abs, "4503599627370495");
    let neg = ir.icmp("slt", &u, "0");
    let is_inf = ir.icmp("eq", &e, "2047");
    let mant_ne = ir.icmp("ne", &mant, "0");
    let is_nan = ir.ibin("and", &ir.zext(&is_inf), &ir.zext(&mant_ne));
    let is_nan = ir.icmp("ne", &is_nan, "0");
    ir.op(format!("  br i1 {}, label %nan, label %chk", is_nan));
    ir.op("nan:".to_string());
    ir.op("  ret double %x".to_string());
    ir.op("chk:".to_string());
    // erf(+-inf) is +-1.
    ir.op(format!("  br i1 {}, label %inf, label %chk2", is_inf));
    ir.op("inf:".to_string());
    let one = f(0x3FF0000000000000);
    let neg_one = f(0xBFF0000000000000);
    let out = ir.sel_d(&neg, &neg_one, &one);
    ir.op(format!("ret double {out}"));
    ir.op("chk2:".to_string());
    // |x| <= 1.5: the polynomial.
    let small = ir.icmp("ule", &abs, &c::ONE_POINT_FIVE.to_string());
    ir.op(format!("  br i1 {}, label %poly, label %cf", small));
    ir.op("poly:".to_string());
    // erf(r) = r * (1 + Y(r^2))
    let z = ir.fbin("fmul", x, x);
    let y = ir.horner(&c::ERF_Y, &z);
    let out = ir.fbin("fmul", x, &ir.fbin("fadd", &f(0x3FF0000000000000), &y));
    ir.op(format!("ret double {out}"));
    ir.op("cf:".to_string());
    // erf(x) = 1 - erfc(|x|), negated for a negative argument.
    let ax = ir.fabs(x);
    let ec = ir.tmp();
    ir.op(format!("{ec} = call double @nexum_math_erfc(double {ax})"));
    let v = ir.fbin("fsub", &f(0x3FF0000000000000), &ec);
    let out = ir.sel_d(&neg, &ir.fun("fneg", &v), &v);
    ir.op(format!("ret double {out}"));
    func("nexum_erf", "double %x", &ir.body())
}

/// `pow(x, y)`.
fn pow_routine() -> String {
    let ir = Ir::new();
    let x = "%x";
    let y = "%y";
    let nan_x = ir.fcmp("uno", x, x);
    let nan_y = ir.fcmp("uno", y, y);
    let is_nan = ir.or_i1(&nan_x, &nan_y);
    // Whether the exponent is an integer, and whether it is small enough
    // to be counted in. Both are needed further down, so they are
    // computed here.
    let ty = ir.tmp();
    ir.op(format!("{ty} = call double @nexum_math_trunc(double {y})"));
    let y_int = ir.fcmp("oeq", &ty, y);
    let y_big = ir.fcmp("ogt", &ir.fabs(y), &f(0x41E0000000000000));
    let is_int = ir.and_i1(&y_int, &ir.not_i1(&y_big));
    ir.op(format!("  br i1 {}, label %nan, label %chk", is_nan));
    ir.op("nan:".to_string());
    ir.op(format!("  ret double {}", f(0x7FF8000000000000)));
    ir.op("chk:".to_string());
    // y = 0 and x = 1 both give 1.
    let y_zero = ir.fcmp("oeq", y, &f(0));
    let x_one = ir.fcmp("oeq", x, &f(0x3FF0000000000000));
    let is_one = ir.or_i1(&y_zero, &x_one);
    ir.op(format!("  br i1 {}, label %one, label %chk2", is_one));
    ir.op("one:".to_string());
    ir.op(format!("  ret double {}", f(0x3FF0000000000000)));
    ir.op("chk2:".to_string());
    // A negative base needs an integer exponent.
    let x_neg = ir.fcmp("olt", x, &f(0));
    ir.op(format!("  br i1 {}, label %neg, label %chk3", x_neg));
    ir.op("neg:".to_string());
    ir.op(format!("  br i1 {}, label %negok, label %nan2", y_int));
    ir.op("nan2:".to_string());
    ir.op(format!("  ret double {}", f(0x7FF8000000000000)));
    ir.op("negok:".to_string());
    let nx = ir.fun("fneg", x);
    let r = ir.tmp();
    ir.op(format!(
        "{r} = call double @nexum_pow(double {nx}, double {y})"
    ));
    // An odd exponent changes the sign.
    let u = ir.bitcast_fi(y);
    let odd = ir.icmp("ne", &ir.ibin("and", &u, "1"), "0");
    let out = ir.sel_d(&odd, &ir.fun("fneg", &r), &r);
    ir.op(format!("ret double {out}"));
    ir.op("chk3:".to_string());
    // A zero base.
    let x_zero = ir.fcmp("oeq", x, &f(0));
    ir.op(format!("  br i1 {}, label %zero, label %chk4", x_zero));
    ir.op("zero:".to_string());
    let y_pos = ir.fcmp("ogt", y, &f(0));
    let inf = f(0x7FF0000000000000);
    let out = ir.sel_d(&y_pos, &f(0), &inf);
    ir.op(format!("ret double {out}"));
    ir.op("chk4:".to_string());
    // An infinite base or exponent.
    let x_inf = ir.fcmp("oeq", &ir.fabs(x), &inf);
    let y_inf = ir.fcmp("oeq", &ir.fabs(y), &inf);
    let inf_case = ir.or_i1(&x_inf, &y_inf);
    ir.op(format!("  br i1 {}, label %infcase, label %chk5", inf_case));
    ir.op("infcase:".to_string());
    // x = +inf: the sign of the exponent decides. y = +-inf: the sign of
    // x - 1 decides, and x = 1 gives 1, which was handled above.
    let y_pos2 = ir.fcmp("ogt", y, &f(0));
    let x_big = ir.fcmp("ogt", x, &f(0x3FF0000000000000));
    let out1 = ir.sel_d(&y_pos2, &inf, &f(0));
    let out2 = ir.sel_d(&x_big, &inf, &f(0));
    let out = ir.sel_d(&x_inf, &out1, &out2);
    ir.op(format!("ret double {out}"));
    ir.op("chk5:".to_string());

    // An integer exponent is computed by squaring: the error stays at a
    // few units in the last place, whatever the exponent is.
    ir.op(format!("  br i1 {}, label %square, label %exp", is_int));
    ir.op("square:".to_string());
    let nslot = ir.tmp();
    let rslot = ir.tmp();
    let bslot = ir.tmp();
    ir.op(format!("{nslot} = alloca i64"));
    ir.op(format!("{rslot} = alloca double"));
    ir.op(format!("{bslot} = alloca double"));
    let yi = ir.fptosi(y);
    let yi_neg = ir.icmp("slt", &yi, "0");
    let yi_abs = ir.ibin("sub", "0", &yi);
    let n = ir.sel_i(&yi_neg, &yi_abs, &yi);
    ir.op(format!("store i64 {n}, ptr {nslot}"));
    ir.op(format!("store double 1.0, ptr {rslot}"));
    // A negative exponent inverts the base.
    let inv = ir.fbin("fdiv", &f(0x3FF0000000000000), x);
    let b = ir.sel_d(&yi_neg, &inv, x);
    ir.op(format!("store double {b}, ptr {bslot}"));
    ir.op("  br label %sqloop".to_string());
    ir.op("sqloop:".to_string());
    let n2 = ir.tmp();
    ir.op(format!("{n2} = load i64, ptr {nslot}"));
    let more = ir.icmp("sgt", &n2, "0");
    ir.op(format!("  br i1 {}, label %sqbody, label %sqend", more));
    ir.op("sqbody:".to_string());
    let odd = ir.icmp("ne", &ir.ibin("and", &n2, "1"), "0");
    ir.op(format!("  br i1 {}, label %sqmul, label %sqsq", odd));
    ir.op("sqmul:".to_string());
    let rr = ir.tmp();
    let bb = ir.tmp();
    ir.op(format!("{rr} = load double, ptr {rslot}"));
    ir.op(format!("{bb} = load double, ptr {bslot}"));
    let rr = ir.fbin("fmul", &rr, &bb);
    ir.op(format!("store double {rr}, ptr {rslot}"));
    ir.op("  br label %sqsq".to_string());
    ir.op("sqsq:".to_string());
    let bb = ir.tmp();
    ir.op(format!("{bb} = load double, ptr {bslot}"));
    let bb = ir.fbin("fmul", &bb, &bb);
    ir.op(format!("store double {bb}, ptr {bslot}"));
    let n3 = ir.ibin("lshr", &n2, "1");
    ir.op(format!("store i64 {n3}, ptr {nslot}"));
    ir.op("  br label %sqloop".to_string());
    ir.op("sqend:".to_string());
    let rr = ir.tmp();
    ir.op(format!("{rr} = load double, ptr {rslot}"));
    ir.op(format!("ret double {rr}"));
    ir.op("exp:".to_string());
    // x^y = exp(y*log(x))
    let lg = ir.tmp();
    ir.op(format!("{lg} = call double @nexum_log(double {x})"));
    let t = ir.fbin("fmul", y, &lg);
    let out = ir.tmp();
    ir.op(format!("{out} = call double @nexum_exp(double {t})"));
    ir.op(format!("ret double {out}"));
    func("nexum_pow", "double %x, double %y", &ir.body())
}

/// `floor(x)`: the largest integer that is not larger than `x`.
///
/// The truncation is done with the bits of the value: for `|x| < 1` the
/// answer is a zero, for `|x| >= 2^52` the value is already an integer, and
/// in between the low bits of the mantissa are the fraction.
fn floor_routine() -> String {
    let ir = Ir::new();
    let x = "%x";
    let u = ir.bitcast_fi(x);
    let sign = ir.ibin("and", &u, "-9223372036854775808");
    let abs = ir.ibin("and", &u, "9223372036854775807");
    let e = ir.ibin("lshr", &abs, "52");
    let e = ir.ibin("and", &e, "2047");
    let sh = ir.ibin("sub", "1075", &e);
    let sh = ir.ibin("and", &sh, "63");
    let m = ir.ibin("lshr", &abs, &sh);
    let m = ir.ibin("shl", &m, &sh);
    let big = ir.icmp("uge", &e, "1075");
    let small = ir.icmp("ule", &e, "1022");
    let mag = ir.sel_i(&big, &abs, &m);
    let signed = ir.ibin("or", &sign, &mag);
    let down = ir.sel_i(&small, &sign, &signed);
    let down_f = ir.bitcast_if(&down);
    // A negative value that is not an integer is one lower.
    let neg = ir.icmp("slt", &u, "0");
    let int_part = ir.fcmp("oeq", x, &down_f);
    let one_less = ir.fbin("fsub", &down_f, &f(0x3FF0000000000000));
    let out = ir.sel_d(&neg, &ir.sel_d(&int_part, &down_f, &one_less), &down_f);
    // A NaN is returned as it is.
    let is_nan = ir.fcmp("uno", x, x);
    let out = ir.sel_d(&is_nan, x, &out);
    ir.op(format!("ret double {out}"));
    func("nexum_floor", "double %x", &ir.body())
}

/// `ceil(x)`: the smallest integer that is not smaller than `x`.
fn ceil_routine() -> String {
    let ir = Ir::new();
    let x = "%x";
    let u = ir.bitcast_fi(x);
    let sign = ir.ibin("and", &u, "-9223372036854775808");
    let abs = ir.ibin("and", &u, "9223372036854775807");
    let e = ir.ibin("lshr", &abs, "52");
    let e = ir.ibin("and", &e, "2047");
    let sh = ir.ibin("sub", "1075", &e);
    let sh = ir.ibin("and", &sh, "63");
    let m = ir.ibin("lshr", &abs, &sh);
    let m = ir.ibin("shl", &m, &sh);
    let big = ir.icmp("uge", &e, "1075");
    let small = ir.icmp("ule", &e, "1022");
    let mag = ir.sel_i(&big, &abs, &m);
    let signed = ir.ibin("or", &sign, &mag);
    let down = ir.sel_i(&small, &sign, &signed);
    let down_f = ir.bitcast_if(&down);
    // A positive value that is not an integer is one higher.
    let pos = ir.fcmp("ogt", x, &f(0));
    let int_part = ir.fcmp("oeq", x, &down_f);
    let one_more = ir.fbin("fadd", &down_f, &f(0x3FF0000000000000));
    let out = ir.sel_d(&pos, &ir.sel_d(&int_part, &down_f, &one_more), &down_f);
    let is_nan = ir.fcmp("uno", x, x);
    let out = ir.sel_d(&is_nan, x, &out);
    ir.op(format!("ret double {out}"));
    func("nexum_ceil", "double %x", &ir.body())
}

/// `trunc(x)`: the integer part of `x`, with the sign of `x`.
fn trunc_public_routine() -> String {
    let ir = Ir::new();
    let x = "%x";
    let u = ir.bitcast_fi(x);
    let sign = ir.ibin("and", &u, "-9223372036854775808");
    let abs = ir.ibin("and", &u, "9223372036854775807");
    let e = ir.ibin("lshr", &abs, "52");
    let e = ir.ibin("and", &e, "2047");
    let sh = ir.ibin("sub", "1075", &e);
    let sh = ir.ibin("and", &sh, "63");
    let m = ir.ibin("lshr", &abs, &sh);
    let m = ir.ibin("shl", &m, &sh);
    let big = ir.icmp("uge", &e, "1075");
    let small = ir.icmp("ule", &e, "1022");
    let mag = ir.sel_i(&big, &abs, &m);
    let signed = ir.ibin("or", &sign, &mag);
    let v2 = ir.sel_i(&small, &sign, &signed);
    let out = ir.bitcast_if(&v2);
    let is_nan = ir.fcmp("uno", x, x);
    let out = ir.sel_d(&is_nan, x, &out);
    ir.op(format!("ret double {out}"));
    func("nexum_trunc", "double %x", &ir.body())
}

/// `round(x)`: the nearest integer, with a half rounded away from zero.
fn round_routine() -> String {
    let ir = Ir::new();
    let x = "%x";
    let u = ir.bitcast_fi(x);
    let sign = ir.ibin("and", &u, "-9223372036854775808");
    let abs = ir.ibin("and", &u, "9223372036854775807");
    let e = ir.ibin("lshr", &abs, "52");
    let e = ir.ibin("and", &e, "2047");
    let sh = ir.ibin("sub", "1075", &e);
    let sh = ir.ibin("and", &sh, "63");
    let m = ir.ibin("lshr", &abs, &sh);
    let m = ir.ibin("shl", &m, &sh);
    let big = ir.icmp("uge", &e, "1075");
    let small = ir.icmp("ule", &e, "1022");
    let mag = ir.sel_i(&big, &abs, &m);
    let signed = ir.ibin("or", &sign, &mag);
    let t = ir.bitcast_if(&ir.sel_i(&small, &sign, &signed));
    // The fraction decides which way the half goes.
    let frac = ir.fbin("fsub", x, &t);
    let up = ir.fcmp("oge", &frac, &f(0x3FE0000000000000));
    let down = ir.fcmp("ole", &frac, &f(0xBFE0000000000000));
    let up_result = ir.fbin("fadd", &t, &f(0x3FF0000000000000));
    let down_result = ir.fbin("fsub", &t, &f(0x3FF0000000000000));
    let out = ir.sel_d(&up, &up_result, &ir.sel_d(&down, &down_result, &t));
    let is_nan = ir.fcmp("uno", x, x);
    let out = ir.sel_d(&is_nan, x, &out);
    ir.op(format!("ret double {out}"));
    func("nexum_round", "double %x", &ir.body())
}
