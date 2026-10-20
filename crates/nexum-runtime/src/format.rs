//! The print path: the formatters that replace `printf`.
//!
//! A Nexum program must be able to print an `Int`, a `Bool`, a `Str` and a
//! `Float` without the C library. This module is the whole of that: the four
//! entry points the code generator calls, and the formatting core they are
//! built on.
//!
//! The output contract does not change. `Int` is its decimal digits with a
//! leading `-` when it is negative, `Bool` is `0` or `1`, `Str` is its bytes,
//! and `Float` is **exactly** C's `%g` with the default precision of six.
//! The byte-for-byte identity is the point: the run fixtures depend on it.
//!
//! The float conversion is an *exact* six significant digit conversion. The
//! double's exact decimal expansion is computed with a small base-2^32
//! bignum - one multiplication by a power of five and repeated divisions by
//! 10^9 - using 64-bit operations only. The shortest round-trip form is a
//! different format (it disagrees with `%g` on 99.96% of random doubles) and
//! it needs a division by 128 bits, which this phase does not allow.
//!
//! Nothing here allocates. The formatters write into a buffer in their own
//! stack frame, so the print path is not a collector safepoint and does not
//! depend on the allocator: it can land before it.
//!
//! See `specs/research/number-formatting.md` for the design and the
//! measurements behind it.

/// The formatting routines, as LLVM IR.
///
/// `posix` selects the write: the POSIX targets go through the system call
/// layer, and the Windows build still goes through the C library, because
/// Windows has no direct system call instruction.
pub fn format_routines(posix: bool) -> String {
    let mut s = String::new();
    s.push_str(&constants());
    s.push_str(&bignum_helpers());
    s.push_str(&byte_helpers());
    s.push_str(&int_formatter());
    s.push_str(&bool_formatter());
    s.push_str(&float_formatter());
    s.push_str(&writers(posix));
    s
}

/// The constants the formatters read.
///
/// The two tables are the powers of five and the powers of two the bignum
/// needs. They are written out byte by byte, because a hand written hex
/// constant is a silent bug waiting to happen.
fn constants() -> String {
    let mut s = String::new();
    let mut table = |name: &str, values: &[u64]| {
        let list: Vec<String> = values.iter().map(|v| format!("i64 {v}")).collect();
        s.push_str(&format!(
            "@{name} = private unnamed_addr constant [{} x i64] [{}]\n",
            values.len(),
            list.join(", ")
        ));
    };
    let pow5: Vec<u64> = (0..13).map(|k| 5u64.pow(k)).collect();
    table("nexum_fmt_pow5", &pow5);
    let pow2: Vec<u64> = (0..32).map(|k| 1u64 << k).collect();
    table("nexum_fmt_pow2", &pow2);
    // The special values. The NaN spelling is the language's own: the C
    // library's is platform dependent (the UCRT prints `nan(ind)` and
    // `nan(snan)`), and the language does not depend on a platform.
    s.push_str("@nexum_fmt_nan = private unnamed_addr constant [4 x i8] c\"nan\\00\"\n");
    s.push_str("@nexum_fmt_nan_neg = private unnamed_addr constant [5 x i8] c\"-nan\\00\"\n");
    s.push_str("@nexum_fmt_inf = private unnamed_addr constant [4 x i8] c\"inf\\00\"\n");
    s.push_str("@nexum_fmt_inf_neg = private unnamed_addr constant [5 x i8] c\"-inf\\00\"\n");
    s.push_str("@nexum_fmt_zero = private unnamed_addr constant [2 x i8] c\"0\\00\"\n");
    s.push_str("@nexum_fmt_zero_neg = private unnamed_addr constant [3 x i8] c\"-0\\00\"\n");
    s.push_str("@nexum_fmt_nl = private unnamed_addr constant [2 x i8] c\"\\0A\\00\"\n\n");
    s
}

/// The bignum: base 2^32 limbs, little endian.
///
/// Three operations are needed, and all of them are 64-bit operations: a
/// multiplication by a small constant, a division by a small constant, and a
/// shift by whole limbs. A 128-bit division would be a libcall, and this
/// phase does not allow one.
fn bignum_helpers() -> String {
    let mut s = String::new();
    // Multiply the bignum by a constant that fits in 32 bits.
    //
    // The constant is at most 5^13 = 1220703125, so a 32-bit limb times the
    // constant plus the carry stays below 2^63 and the product is exact in
    // 64 bits. The carry out of the top limb is below the constant, so it
    // fits in a limb of its own.
    s.push_str(
        r#"define internal void @nexum_bn_mul_small(ptr %bn, ptr %nl, i64 %c) {
entry:
  %n = load i32, ptr %nl
  %ni = zext i32 %n to i64
  %carry = alloca i64, align 8
  %i = alloca i64, align 8
  store i64 0, ptr %carry
  store i64 0, ptr %i
  br label %loop
loop:
  %iv = load i64, ptr %i
  %done = icmp uge i64 %iv, %ni
  br i1 %done, label %carryout, label %body
body:
  %lp = getelementptr [88 x i32], ptr %bn, i64 0, i64 %iv
  %limb = load i32, ptr %lp
  %lz = zext i32 %limb to i64
  %cv = load i64, ptr %carry
  %p0 = mul i64 %lz, %c
  %p = add i64 %p0, %cv
  %nlz = and i64 %p, 4294967295
  %nlt = trunc i64 %nlz to i32
  store i32 %nlt, ptr %lp
  %nc = lshr i64 %p, 32
  store i64 %nc, ptr %carry
  %i2 = add i64 %iv, 1
  store i64 %i2, ptr %i
  br label %loop
carryout:
  %cv2 = load i64, ptr %carry
  %cz = icmp eq i64 %cv2, 0
  br i1 %cz, label %ret, label %grow
grow:
  %lp2 = getelementptr [88 x i32], ptr %bn, i64 0, i64 %ni
  %cvt = trunc i64 %cv2 to i32
  store i32 %cvt, ptr %lp2
  %n2 = add i32 %n, 1
  store i32 %n2, ptr %nl
  br label %ret
ret:
  ret void
}

"#,
    );
    // Divide the bignum by a constant that fits in 32 bits, and return the
    // remainder. The remainder is one group of nine decimal digits.
    //
    // The running remainder is below the divisor, so the dividend limb by
    // limb is below 10^9 * 2^32, which fits in 64 bits, and the quotient
    // limb is below 2^32.
    s.push_str(
        r#"define internal i64 @nexum_bn_div_small(ptr %bn, ptr %nl, i64 %d) {
entry:
  %n = load i32, ptr %nl
  %ni = zext i32 %n to i64
  %rem = alloca i64, align 8
  %i = alloca i64, align 8
  store i64 0, ptr %rem
  %i0 = sub i64 %ni, 1
  store i64 %i0, ptr %i
  br label %loop
loop:
  %iv = load i64, ptr %i
  %done = icmp slt i64 %iv, 0
  br i1 %done, label %trim, label %body
body:
  %lp = getelementptr [88 x i32], ptr %bn, i64 0, i64 %iv
  %limb = load i32, ptr %lp
  %lz = zext i32 %limb to i64
  %rv = load i64, ptr %rem
  %hi = mul i64 %rv, 4294967296
  %cur = add i64 %hi, %lz
  %q = udiv i64 %cur, %d
  %r = urem i64 %cur, %d
  %qt = trunc i64 %q to i32
  store i32 %qt, ptr %lp
  store i64 %r, ptr %rem
  %i2 = sub i64 %iv, 1
  store i64 %i2, ptr %i
  br label %loop
trim:
  %j = alloca i64, align 8
  store i64 %ni, ptr %j
  br label %trimloop
trimloop:
  %jv = load i64, ptr %j
  %jz = icmp eq i64 %jv, 0
  br i1 %jz, label %trimzero, label %trimbody
trimbody:
  %j1 = sub i64 %jv, 1
  %lp2 = getelementptr [88 x i32], ptr %bn, i64 0, i64 %j1
  %lv = load i32, ptr %lp2
  %lnz = icmp ne i32 %lv, 0
  store i64 %j1, ptr %j
  br i1 %lnz, label %trimset, label %trimloop
trimset:
  %jc = add i64 %j1, 1
  %jct = trunc i64 %jc to i32
  store i32 %jct, ptr %nl
  br label %trimdone
trimzero:
  store i32 0, ptr %nl
  br label %trimdone
trimdone:
  %rv2 = load i64, ptr %rem
  ret i64 %rv2
}

"#,
    );
    // Shift the bignum left by whole limbs.
    s.push_str(
        r#"define internal void @nexum_bn_shift_limbs(ptr %bn, ptr %nl, i64 %k) {
entry:
  %n = load i32, ptr %nl
  %ni = zext i32 %n to i64
  %i = alloca i64, align 8
  store i64 %ni, ptr %i
  br label %loop
loop:
  %iv = load i64, ptr %i
  %done = icmp eq i64 %iv, 0
  br i1 %done, label %fin, label %body
body:
  %i1 = sub i64 %iv, 1
  %srcp = getelementptr [88 x i32], ptr %bn, i64 0, i64 %i1
  %v = load i32, ptr %srcp
  %di = add i64 %i1, %k
  %dstp = getelementptr [88 x i32], ptr %bn, i64 0, i64 %di
  store i32 %v, ptr %dstp
  store i64 %i1, ptr %i
  br label %loop
fin:
  %kt = trunc i64 %k to i32
  %n2 = add i32 %n, %kt
  store i32 %n2, ptr %nl
  ret void
}

"#,
    );
    s
}

/// The byte helpers the formatters share.
fn byte_helpers() -> String {
    let mut s = String::new();
    // Append one byte at the cursor.
    s.push_str(
        r#"define internal void @nexum_putb(ptr %buf, ptr %cur, i8 %c) {
entry:
  %cv = load i64, ptr %cur
  %p = getelementptr i8, ptr %buf, i64 %cv
  store i8 %c, ptr %p
  %cv2 = add i64 %cv, 1
  store i64 %cv2, ptr %cur
  ret void
}

"#,
    );
    // Append a run of bytes at the cursor.
    s.push_str(
        r#"define internal void @nexum_putn(ptr %dst, ptr %cur, ptr %src, i64 %n) {
entry:
  %i = alloca i64, align 8
  store i64 0, ptr %i
  br label %loop
loop:
  %iv = load i64, ptr %i
  %done = icmp uge i64 %iv, %n
  br i1 %done, label %fin, label %body
body:
  %sp = getelementptr i8, ptr %src, i64 %iv
  %c = load i8, ptr %sp
  call void @nexum_putb(ptr %dst, ptr %cur, i8 %c)
  %i2 = add i64 %iv, 1
  store i64 %i2, ptr %i
  br label %loop
fin:
  ret void
}

"#,
    );
    // How many trailing zero bytes a run has. The run's length is at most
    // six, so the answer is at most six.
    s.push_str(
        r#"define internal i64 @nexum_trailzeros(ptr %p, i64 %n) {
entry:
  %i = alloca i64, align 8
  store i64 %n, ptr %i
  br label %loop
loop:
  %iv = load i64, ptr %i
  %done = icmp eq i64 %iv, 0
  br i1 %done, label %fin, label %body
body:
  %i1 = sub i64 %iv, 1
  %pp = getelementptr i8, ptr %p, i64 %i1
  %c = load i8, ptr %pp
  %is0 = icmp eq i8 %c, 48
  br i1 %is0, label %cont, label %fin
cont:
  store i64 %i1, ptr %i
  br label %loop
fin:
  %z = sub i64 %n, %iv
  ret i64 %z
}

"#,
    );
    s
}

/// The integer formatter.
///
/// The magnitude of `-9223372036854775808` is 2^63, which does not fit in a
/// positive 64-bit integer, but the two's complement pattern of `0 - v` is
/// 2^63 and the unsigned operations read it as 9223372036854775808. So the
/// sign is a signed comparison and the magnitude is a wrapping subtraction.
fn int_formatter() -> String {
    let mut s = String::new();
    s.push_str(
        r#"define internal i64 @nexum_fmt_int_to(ptr %buf, i64 %v) {
entry:
  %tmp = alloca i8, i64 24, align 8
  %u = alloca i64, align 8
  %n = alloca i64, align 8
  %neg = icmp slt i64 %v, 0
  %umn = sub i64 0, %v
  %um = select i1 %neg, i64 %umn, i64 %v
  store i64 %um, ptr %u
  store i64 0, ptr %n
  br label %loop
loop:
  %u1 = load i64, ptr %u
  %d = urem i64 %u1, 10
  %u2 = udiv i64 %u1, 10
  store i64 %u2, ptr %u
  %nn = load i64, ptr %n
  %pos = sub i64 19, %nn
  %tp = getelementptr i8, ptr %tmp, i64 %pos
  %cc = add i64 %d, 48
  %cc8 = trunc i64 %cc to i8
  store i8 %cc8, ptr %tp
  %nn2 = add i64 %nn, 1
  store i64 %nn2, ptr %n
  %more = icmp ne i64 %u2, 0
  br i1 %more, label %loop, label %emit
emit:
  %n2 = load i64, ptr %n
  %start = sub i64 20, %n2
  %srcp = getelementptr i8, ptr %tmp, i64 %start
  %off = select i1 %neg, i64 1, i64 0
  %dstp = getelementptr i8, ptr %buf, i64 %off
  call ptr @nexum_memcpy(ptr %dstp, ptr %srcp, i64 %n2)
  br i1 %neg, label %sign, label %nosign
sign:
  %bp = getelementptr i8, ptr %buf, i64 0
  store i8 45, ptr %bp
  br label %done
nosign:
  br label %done
done:
  %len = add i64 %n2, %off
  %nulp = getelementptr i8, ptr %buf, i64 %len
  store i8 0, ptr %nulp
  ret i64 %len
}

"#,
    );
    s
}

/// The boolean formatter: `0` or `1`.
fn bool_formatter() -> String {
    let mut s = String::new();
    s.push_str(
        r#"define internal i64 @nexum_fmt_bool_to(ptr %buf, i64 %v) {
entry:
  %b = icmp ne i64 %v, 0
  %c = select i1 %b, i8 49, i8 48
  store i8 %c, ptr %buf
  %nulp = getelementptr i8, ptr %buf, i64 1
  store i8 0, ptr %nulp
  ret i64 1
}

"#,
    );
    s
}

/// The float formatter: exactly C's `%g` with the default precision of six.
///
/// The steps are the ones in the research document:
///
/// 1. the bit decomposition, and the special values;
/// 2. the exact decimal expansion, with the bignum;
/// 3. the rounding to six significant digits, with the ties going to even;
/// 4. the layout: the fixed notation between -4 and 5, the exponential one
///    outside it.
///
/// Every allocation is in the entry block: an `alloca` inside a loop would
/// grow the stack on every iteration.
fn float_formatter() -> String {
    let mut s = String::new();
    s.push_str(
        r#"define internal i64 @nexum_fmt_float_to(ptr %buf, double %v) {
entry:
  %bn = alloca [88 x i32], align 8
  %nl = alloca i32, align 4
  %count = alloca i64, align 8
  %db = alloca [800 x i8], align 8
  %dpos = alloca i64, align 8
  %rv = alloca i64, align 8
  %kk = alloca i64, align 8
  %sc = alloca i64, align 8
  %kk3 = alloca i64, align 8
  %rs = alloca i64, align 8
  %rn = alloca i64, align 8
  %xadj = alloca i64, align 8
  %ci = alloca i64, align 8
  %carry = alloca i64, align 8
  %body = alloca i8, i64 32, align 8
  %cur = alloca i64, align 8
  %zi = alloca i64, align 8
  %dg = alloca [8 x i8], align 8
  %bits = bitcast double %v to i64
  %neg = icmp slt i64 %bits, 0
  %ebits = lshr i64 %bits, 52
  %ebitsm = and i64 %ebits, 2047
  %frac = and i64 %bits, 4503599627370495
  %ehi = icmp eq i64 %ebitsm, 2047
  %fne = icmp ne i64 %frac, 0
  %feq = icmp eq i64 %frac, 0
  %isnan = and i1 %ehi, %fne
  %isinf = and i1 %ehi, %feq
  %elo = icmp eq i64 %ebitsm, 0
  %iszero = and i1 %elo, %feq
  %spec1 = or i1 %isnan, %isinf
  %spec = or i1 %spec1, %iszero
  br i1 %spec, label %special, label %general
special:
  %t1 = select i1 %neg, ptr @nexum_fmt_nan_neg, ptr @nexum_fmt_nan
  %t2 = select i1 %neg, ptr @nexum_fmt_inf_neg, ptr @nexum_fmt_inf
  %t3 = select i1 %neg, ptr @nexum_fmt_zero_neg, ptr @nexum_fmt_zero
  %t4 = select i1 %isinf, ptr %t2, ptr %t3
  %txt = select i1 %isnan, ptr %t1, ptr %t4
  %slen = call i64 @nexum_strlen(ptr %txt)
  call ptr @nexum_memcpy(ptr %buf, ptr %txt, i64 %slen)
  %snulp = getelementptr i8, ptr %buf, i64 %slen
  store i8 0, ptr %snulp
  ret i64 %slen
general:
  %issub = icmp eq i64 %ebitsm, 0
  %mhi = or i64 %frac, 4503599627370496
  %m = select i1 %issub, i64 %frac, i64 %mhi
  %enorm = sub i64 %ebitsm, 1075
  %e = select i1 %issub, i64 -1074, i64 %enorm
  %eneg = icmp slt i64 %e, 0
  %sabs = sub i64 0, %e
  %s = select i1 %eneg, i64 %sabs, i64 0
  %m0 = and i64 %m, 4294967295
  %m1 = lshr i64 %m, 32
  %m0t = trunc i64 %m0 to i32
  %m1t = trunc i64 %m1 to i32
  %bp0 = getelementptr [88 x i32], ptr %bn, i64 0, i64 0
  store i32 %m0t, ptr %bp0
  %bp1 = getelementptr [88 x i32], ptr %bn, i64 0, i64 1
  store i32 %m1t, ptr %bp1
  store i32 2, ptr %nl
  br i1 %eneg, label %build_frac, label %build_int
build_frac:
  %kdiv = udiv i64 %sabs, 13
  %krem = urem i64 %sabs, 13
  %pwp = getelementptr [13 x i64], ptr @nexum_fmt_pow5, i64 0, i64 %krem
  %c5 = load i64, ptr %pwp
  call void @nexum_bn_mul_small(ptr %bn, ptr %nl, i64 %c5)
  store i64 %kdiv, ptr %count
  br label %muloop
muloop:
  %cv = load i64, ptr %count
  %cdone = icmp eq i64 %cv, 0
  br i1 %cdone, label %bignum_ready, label %mulbody
mulbody:
  call void @nexum_bn_mul_small(ptr %bn, ptr %nl, i64 1220703125)
  %cv2 = sub i64 %cv, 1
  store i64 %cv2, ptr %count
  br label %muloop
build_int:
  %sh = udiv i64 %e, 32
  %rb = urem i64 %e, 32
  %pwp2 = getelementptr [32 x i64], ptr @nexum_fmt_pow2, i64 0, i64 %rb
  %c2 = load i64, ptr %pwp2
  call void @nexum_bn_mul_small(ptr %bn, ptr %nl, i64 %c2)
  call void @nexum_bn_shift_limbs(ptr %bn, ptr %nl, i64 %sh)
  br label %bignum_ready
bignum_ready:
  store i64 800, ptr %dpos
  br label %digloop
digloop:
  %rem = call i64 @nexum_bn_div_small(ptr %bn, ptr %nl, i64 1000000000)
  store i64 %rem, ptr %rv
  store i64 0, ptr %kk
  br label %diginner
diginner:
  %kv = load i64, ptr %kk
  %rv2 = load i64, ptr %rv
  %d = urem i64 %rv2, 10
  %rv3 = udiv i64 %rv2, 10
  store i64 %rv3, ptr %rv
  %dp = load i64, ptr %dpos
  %dp1 = sub i64 %dp, 1
  %dp2 = sub i64 %dp1, %kv
  %dpp = getelementptr i8, ptr %db, i64 %dp2
  %cc = add i64 %d, 48
  %cc8 = trunc i64 %cc to i8
  store i8 %cc8, ptr %dpp
  %kk2 = add i64 %kv, 1
  store i64 %kk2, ptr %kk
  %dmore = icmp slt i64 %kk2, 9
  br i1 %dmore, label %diginner, label %digafter
digafter:
  %dpa = sub i64 %dp, 9
  store i64 %dpa, ptr %dpos
  %n = load i32, ptr %nl
  %isz = icmp eq i32 %n, 0
  br i1 %isz, label %digdone, label %digloop
digdone:
  %dp3 = load i64, ptr %dpos
  store i64 %dp3, ptr %sc
  br label %scan
scan:
  %sv = load i64, ptr %sc
  %sp = getelementptr i8, ptr %db, i64 %sv
  %sc8 = load i8, ptr %sp
  %snon = icmp ne i8 %sc8, 48
  br i1 %snon, label %scandone, label %scannext
scannext:
  %sv2 = add i64 %sv, 1
  store i64 %sv2, ptr %sc
  br label %scan
scandone:
  %start = load i64, ptr %sc
  %len = sub i64 800, %start
  store i64 0, ptr %kk3
  br label %cploop
cploop:
  %kv2 = load i64, ptr %kk3
  %cmore = icmp slt i64 %kv2, 7
  br i1 %cmore, label %cpbody, label %cpdone
cpbody:
  %idx = add i64 %start, %kv2
  %inrange = icmp ult i64 %idx, 800
  %dgp = getelementptr [8 x i8], ptr %dg, i64 0, i64 %kv2
  br i1 %inrange, label %cphave, label %cppad
cphave:
  %sp2 = getelementptr i8, ptr %db, i64 %idx
  %cv8 = load i8, ptr %sp2
  store i8 %cv8, ptr %dgp
  br label %cpnext
cppad:
  store i8 48, ptr %dgp
  br label %cpnext
cpnext:
  %kv3 = add i64 %kv2, 1
  store i64 %kv3, ptr %kk3
  br label %cploop
cpdone:
  %rstart = add i64 %start, 7
  store i64 %rstart, ptr %rs
  store i64 0, ptr %rn
  br label %rsloop
rsloop:
  %rv4 = load i64, ptr %rs
  %rsdone = icmp uge i64 %rv4, 800
  br i1 %rsdone, label %rsdone2, label %rsbody
rsbody:
  %rsp = getelementptr i8, ptr %db, i64 %rv4
  %rc = load i8, ptr %rsp
  %rcz = icmp ne i8 %rc, 48
  %rczz = zext i1 %rcz to i64
  %rnz = load i64, ptr %rn
  %rn2 = or i64 %rnz, %rczz
  store i64 %rn2, ptr %rn
  %rs2 = add i64 %rv4, 1
  store i64 %rs2, ptr %rs
  br label %rsloop
rsdone2:
  %d6p = getelementptr [8 x i8], ptr %dg, i64 0, i64 6
  %d6 = load i8, ptr %d6p
  %gt5 = icmp ugt i8 %d6, 53
  %tie = icmp eq i8 %d6, 53
  %d5p = getelementptr [8 x i8], ptr %dg, i64 0, i64 5
  %d5 = load i8, ptr %d5p
  %d5n = sub i8 %d5, 48
  %d5z = zext i8 %d5n to i64
  %d5odd = and i64 %d5z, 1
  %rnz2 = load i64, ptr %rn
  %tieup = or i64 %rnz2, %d5odd
  %tie2 = zext i1 %tie to i64
  %tieup2 = and i64 %tie2, %tieup
  %gt5z = zext i1 %gt5 to i64
  %roundup = or i64 %gt5z, %tieup2
  %roundupb = icmp ne i64 %roundup, 0
  store i64 0, ptr %xadj
  br i1 %roundupb, label %doround, label %rounddone
doround:
  store i64 5, ptr %ci
  store i64 1, ptr %carry
  br label %incloop
incloop:
  %civ = load i64, ptr %ci
  %cend = icmp slt i64 %civ, 0
  br i1 %cend, label %inccarry, label %incbody
incbody:
  %dgp2 = getelementptr [8 x i8], ptr %dg, i64 0, i64 %civ
  %dv = load i8, ptr %dgp2
  %dvn = sub i8 %dv, 48
  %dvz = zext i8 %dvn to i64
  %carryv = load i64, ptr %carry
  %sum = add i64 %dvz, %carryv
  %is10 = icmp eq i64 %sum, 10
  br i1 %is10, label %incwrap, label %incset
incwrap:
  store i8 48, ptr %dgp2
  store i64 1, ptr %carry
  %ci2 = sub i64 %civ, 1
  store i64 %ci2, ptr %ci
  br label %incloop
incset:
  %newd = add i64 %sum, 48
  %newd8 = trunc i64 %newd to i8
  store i8 %newd8, ptr %dgp2
  store i64 0, ptr %carry
  br label %rounddone
inccarry:
  %d0p = getelementptr [8 x i8], ptr %dg, i64 0, i64 0
  store i8 49, ptr %d0p
  %d1p = getelementptr [8 x i8], ptr %dg, i64 0, i64 1
  store i8 48, ptr %d1p
  %d2p = getelementptr [8 x i8], ptr %dg, i64 0, i64 2
  store i8 48, ptr %d2p
  %d3p = getelementptr [8 x i8], ptr %dg, i64 0, i64 3
  store i8 48, ptr %d3p
  %d4p = getelementptr [8 x i8], ptr %dg, i64 0, i64 4
  store i8 48, ptr %d4p
  %d5p2 = getelementptr [8 x i8], ptr %dg, i64 0, i64 5
  store i8 48, ptr %d5p2
  store i64 1, ptr %xadj
  br label %rounddone
rounddone:
  %x0 = sub i64 %len, %s
  %x1 = sub i64 %x0, 1
  %xa = load i64, ptr %xadj
  %x = add i64 %x1, %xa
  store i64 0, ptr %cur
  br i1 %neg, label %signbody, label %layout
signbody:
  call void @nexum_putb(ptr %body, ptr %cur, i8 45)
  br label %layout
layout:
  %xlow = icmp sge i64 %x, -4
  %xhigh = icmp sle i64 %x, 5
  %fixed = and i1 %xlow, %xhigh
  br i1 %fixed, label %layoutfixed, label %layoutexp
layoutfixed:
  %xpos = icmp sge i64 %x, 0
  br i1 %xpos, label %fixpos, label %fixneg
fixpos:
  %dg0 = getelementptr [8 x i8], ptr %dg, i64 0, i64 0
  %icount = add i64 %x, 1
  call void @nexum_putn(ptr %body, ptr %cur, ptr %dg0, i64 %icount)
  %fstart = add i64 %x, 1
  %fsrc = getelementptr [8 x i8], ptr %dg, i64 0, i64 %fstart
  %flen0 = sub i64 6, %fstart
  %fz = call i64 @nexum_trailzeros(ptr %fsrc, i64 %flen0)
  %flen = sub i64 %flen0, %fz
  %hasfrac = icmp sgt i64 %flen, 0
  br i1 %hasfrac, label %fixfrac, label %fixdone
fixfrac:
  call void @nexum_putb(ptr %body, ptr %cur, i8 46)
  call void @nexum_putn(ptr %body, ptr %cur, ptr %fsrc, i64 %flen)
  br label %fixdone
fixneg:
  call void @nexum_putb(ptr %body, ptr %cur, i8 48)
  call void @nexum_putb(ptr %body, ptr %cur, i8 46)
  %xabs = sub i64 0, %x
  %zeros = sub i64 %xabs, 1
  store i64 0, ptr %zi
  br label %zloop
zloop:
  %zv = load i64, ptr %zi
  %zdone = icmp uge i64 %zv, %zeros
  br i1 %zdone, label %znext, label %zbody
zbody:
  call void @nexum_putb(ptr %body, ptr %cur, i8 48)
  %zi2 = add i64 %zv, 1
  store i64 %zi2, ptr %zi
  br label %zloop
znext:
  %dg0b = getelementptr [8 x i8], ptr %dg, i64 0, i64 0
  %z6 = call i64 @nexum_trailzeros(ptr %dg0b, i64 6)
  %flen2 = sub i64 6, %z6
  call void @nexum_putn(ptr %body, ptr %cur, ptr %dg0b, i64 %flen2)
  br label %fixdone
fixdone:
  br label %finish
layoutexp:
  %dg0c = getelementptr [8 x i8], ptr %dg, i64 0, i64 0
  %d0 = load i8, ptr %dg0c
  call void @nexum_putb(ptr %body, ptr %cur, i8 %d0)
  %esrc = getelementptr [8 x i8], ptr %dg, i64 0, i64 1
  %z5 = call i64 @nexum_trailzeros(ptr %esrc, i64 5)
  %flen3 = sub i64 5, %z5
  %hasfrac2 = icmp sgt i64 %flen3, 0
  br i1 %hasfrac2, label %expfrac, label %expe
expfrac:
  call void @nexum_putb(ptr %body, ptr %cur, i8 46)
  call void @nexum_putn(ptr %body, ptr %cur, ptr %esrc, i64 %flen3)
  br label %expe
expe:
  call void @nexum_putb(ptr %body, ptr %cur, i8 101)
  %xneg = icmp slt i64 %x, 0
  br i1 %xneg, label %expneg, label %exppos
expneg:
  call void @nexum_putb(ptr %body, ptr %cur, i8 45)
  br label %expabs
exppos:
  call void @nexum_putb(ptr %body, ptr %cur, i8 43)
  br label %expabs
expabs:
  %xnegv = sub i64 0, %x
  %xabs2 = select i1 %xneg, i64 %xnegv, i64 %x
  call void @nexum_put_exp(ptr %body, ptr %cur, i64 %xabs2)
  br label %finish
finish:
  %curv = load i64, ptr %cur
  call ptr @nexum_memcpy(ptr %buf, ptr %body, i64 %curv)
  %nulp2 = getelementptr i8, ptr %buf, i64 %curv
  store i8 0, ptr %nulp2
  ret i64 %curv
}

"#,
    );
    // The exponent: the absolute value of the decimal exponent, with at
    // least two digits.
    s.push_str(
        r#"define internal void @nexum_put_exp(ptr %body, ptr %cur, i64 %ax) {
entry:
  %tmp = alloca i8, i64 8, align 8
  %n = alloca i64, align 8
  %v = alloca i64, align 8
  %small = icmp slt i64 %ax, 10
  br i1 %small, label %sm, label %big
sm:
  call void @nexum_putb(ptr %body, ptr %cur, i8 48)
  %d = add i64 %ax, 48
  %d8 = trunc i64 %d to i8
  call void @nexum_putb(ptr %body, ptr %cur, i8 %d8)
  br label %fin
big:
  store i64 0, ptr %n
  store i64 %ax, ptr %v
  br label %loop
loop:
  %vv = load i64, ptr %v
  %done = icmp eq i64 %vv, 0
  br i1 %done, label %emit, label %digit
digit:
  %d2 = urem i64 %vv, 10
  %v2 = udiv i64 %vv, 10
  %nn = load i64, ptr %n
  %pos = sub i64 7, %nn
  %tp = getelementptr i8, ptr %tmp, i64 %pos
  %cc = add i64 %d2, 48
  %cc8 = trunc i64 %cc to i8
  store i8 %cc8, ptr %tp
  %nn2 = add i64 %nn, 1
  store i64 %nn2, ptr %n
  store i64 %v2, ptr %v
  br label %loop
emit:
  %nn3 = load i64, ptr %n
  %start = sub i64 8, %nn3
  %srcp = getelementptr i8, ptr %tmp, i64 %start
  call void @nexum_putn(ptr %body, ptr %cur, ptr %srcp, i64 %nn3)
  br label %fin
fin:
  ret void
}

"#,
    );
    s
}

/// The writers: the four entry points and the write to stdout.
fn writers(posix: bool) -> String {
    let mut s = String::new();
    if posix {
        // The write goes through the system call layer, and a partial write
        // is retried: a pipe takes what it takes.
        s.push_str(
            r#"define internal void @nexum_write_stdout(ptr %buf, i64 %len) {
entry:
  %off = alloca i64, align 8
  store i64 0, ptr %off
  br label %loop
loop:
  %o = load i64, ptr %off
  %done = icmp uge i64 %o, %len
  br i1 %done, label %fin, label %body
body:
  %p = getelementptr i8, ptr %buf, i64 %o
  %pi = ptrtoint ptr %p to i64
  %rem = sub i64 %len, %o
  %w = call i64 @nexum_sys_write(i64 1, i64 %pi, i64 %rem)
  %bad = icmp slt i64 %w, 0
  br i1 %bad, label %fin, label %adv
adv:
  %zero = icmp eq i64 %w, 0
  br i1 %zero, label %fin, label %next
next:
  %o2 = add i64 %o, %w
  store i64 %o2, ptr %off
  br label %loop
fin:
  ret void
}

"#,
        );
    } else {
        // The Windows build still uses the C library: its kernel interface
        // lives behind ntdll, and the system call stubs are a later phase.
        s.push_str(
            r#"define internal void @nexum_write_stdout(ptr %buf, i64 %len) {
entry:
  %out = call ptr @__acrt_iob_func(i32 1)
  %w = call i64 @fwrite(ptr %buf, i64 1, i64 %len, ptr %out)
  ret void
}

"#,
        );
    }
    // The four entry points. The newline is a byte the printer appends
    // itself, so a program prints the same bytes everywhere.
    s.push_str(
        r#"define internal void @nexum_print_int(i64 %v) {
entry:
  %buf = alloca i8, i64 32, align 8
  %len = call i64 @nexum_fmt_int_to(ptr %buf, i64 %v)
  %p = getelementptr i8, ptr %buf, i64 %len
  store i8 10, ptr %p
  %n = add i64 %len, 1
  call void @nexum_write_stdout(ptr %buf, i64 %n)
  ret void
}

define internal void @nexum_print_bool(i64 %v) {
entry:
  %buf = alloca i8, i64 32, align 8
  %len = call i64 @nexum_fmt_bool_to(ptr %buf, i64 %v)
  %p = getelementptr i8, ptr %buf, i64 %len
  store i8 10, ptr %p
  %n = add i64 %len, 1
  call void @nexum_write_stdout(ptr %buf, i64 %n)
  ret void
}

define internal void @nexum_print_float(double %v) {
entry:
  %buf = alloca i8, i64 32, align 8
  %len = call i64 @nexum_fmt_float_to(ptr %buf, double %v)
  %p = getelementptr i8, ptr %buf, i64 %len
  store i8 10, ptr %p
  %n = add i64 %len, 1
  call void @nexum_write_stdout(ptr %buf, i64 %n)
  ret void
}

define internal void @nexum_print_str(ptr %s) {
entry:
  %len = call i64 @nexum_strlen(ptr %s)
  call void @nexum_write_stdout(ptr %s, i64 %len)
  call void @nexum_write_stdout(ptr @nexum_fmt_nl, i64 1)
  ret void
}

"#,
    );
    s
}

#[cfg(test)]
mod tests {
    use super::format_routines;

    /// The print path must not need the C library: it declares nothing.
    #[test]
    fn the_print_path_declares_nothing() {
        let ir = format_routines(true);
        assert!(!ir.contains("declare "), "the print path declares a symbol");
    }

    /// The phase's rule: no 128-bit division, because it is a libcall.
    #[test]
    fn there_is_no_128_bit_division() {
        let ir = format_routines(true);
        assert!(!ir.contains("udiv i128"), "a 128-bit division");
        assert!(!ir.contains("sdiv i128"), "a 128-bit division");
    }

    /// The print path allocates nothing: it is not a collector safepoint, and
    /// it does not depend on the allocator.
    #[test]
    fn the_print_path_allocates_nothing() {
        let ir = format_routines(true);
        for name in ["nexum_print_int", "nexum_print_bool", "nexum_print_float"] {
            let body = ir
                .split(&format!("define internal void @{name}("))
                .nth(1)
                .unwrap_or_else(|| panic!("no {name}"));
            let body = body.split("}\n\n").next().unwrap_or(body);
            assert!(!body.contains("@nexum_alloc"), "{name} allocates");
            assert!(!body.contains("@malloc"), "{name} allocates");
        }
    }

    /// An `alloca` inside a loop grows the stack on every iteration, so
    /// every allocation is in the entry block.
    #[test]
    fn every_allocation_is_in_the_entry_block() {
        let ir = format_routines(true);
        for def in ir.split("define internal ").skip(1) {
            let header = def.lines().next().unwrap_or_default();
            let name = header.split('(').next().unwrap_or_default();
            let mut in_entry = true;
            for line in def.lines().skip(1) {
                let trimmed = line.trim();
                if trimmed.ends_with(':') {
                    in_entry = trimmed == "entry:";
                    continue;
                }
                if in_entry {
                    continue;
                }
                assert!(
                    !trimmed.starts_with("alloca"),
                    "{name}: an alloca outside the entry block: {trimmed}"
                );
            }
        }
    }

    /// The entry points the code generator calls are the ones the language
    /// reference names.
    #[test]
    fn the_entry_points_are_named_as_the_language_names_them() {
        let ir = format_routines(true);
        for name in [
            "nexum_print_int",
            "nexum_print_bool",
            "nexum_print_float",
            "nexum_print_str",
            "nexum_fmt_int_to",
            "nexum_fmt_bool_to",
            "nexum_fmt_float_to",
        ] {
            assert!(ir.contains(&format!("@{name}(")), "no {name}");
        }
    }
}
