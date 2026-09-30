; Nexum runtime prelude, linked (textually) into every compiled program.
; NX owns semantics; LLVM owns codegen. Memory in v0 is malloc'd and lives
; for the process lifetime (freed by the OS at exit) — the arena story
; arrives with the memory planner.
;
; Value model: %NxVal = { i64 tag, i64 a, i64 b }
;   tag 0 None              {0,0,0}
;   tag 1 Int               a = i64
;   tag 2 Float             a = f64 bits
;   tag 3 Bool              a = 0/1
;   tag 4 Str               a = ptr, b = len
;   tag 5 List              a = header ptr, b = len (mirror)
;   tag 6 Func              a = fn ptr, b = name ptr (NUL-terminated)
; List header: { ptr data (NxVal elements), i64 len, i64 cap }

%NxVal = type { i64, i64, i64 }
%NxList = type { ptr, i64, i64 }

declare i32 @printf(ptr, ...)
declare void @exit(i32)
declare ptr @malloc(i64)
declare ptr @realloc(ptr, i64)
declare i32 @memcmp(ptr, ptr, i64)
declare i32 @snprintf(ptr, i64, ptr, ...)
declare double @llvm.fabs.f64(double)
declare double @llvm.round.f64(double)
declare double @llvm.floor.f64(double)
declare double @llvm.log10.f64(double)
declare double @llvm.pow.f64(double, double)

@.panic.tag = private constant [5 x i8] c"panic"
@.fmt.ld = private constant [5 x i8] c"%lld\00"
@.fmt.sp = private constant [2 x i8] c" \00"
@.fmt.nl = private constant [2 x i8] c"\0A\00"
@.fmt.ss = private constant [5 x i8] c"%.*s\00"
@.fmt.true = private constant [5 x i8] c"true\00"
@.fmt.false = private constant [6 x i8] c"false\00"
@.fmt.none = private constant [5 x i8] c"none\00"
@.fmt.lb = private constant [2 x i8] c"[\00"
@.fmt.rb = private constant [2 x i8] c"]\00"
@.fmt.comma = private constant [3 x i8] c", \00"
@.fmt.fnopen = private constant [5 x i8] c"<fn \00"
@.fmt.close = private constant [2 x i8] c">\00"
@.msg.divzero = private constant [17 x i8] c"division by zero\00"
@.msg.overflow = private constant [17 x i8] c"integer overflow\00"
@.msg.oob = private constant [35 x i8] c"index %lld out of range (len %lld)\00"
@.msg.type = private constant [14 x i8] c"type mismatch\00"
@.msg.notcall = private constant [13 x i8] c"not callable\00"
@.msg.noattr = private constant [38 x i8] c"only modules support attribute access\00"
@.msg.inf = private constant [4 x i8] c"inf\00"
@.msg.ninf = private constant [5 x i8] c"-inf\00"
@.msg.nan = private constant [4 x i8] c"NaN\00"
@.msg.zero = private constant [2 x i8] c"0\00"
@.msg.fdec = private constant [7 x i8] c"%.*f%s\00"
@.msg.sci = private constant [6 x i8] c"%.17g\00"
@.msg.empty = private constant [1 x i8] c"\00"

define void @nx_panic(ptr %msg) {
entry:
  call i32 (ptr, ...) @printf(ptr %msg)
  call i32 (ptr, ...) @printf(ptr @.fmt.nl)
  call void @exit(i32 1)
  unreachable
}

; --- float formatting: mirrors the interpreter's 15-sig-digit trim ---
define i64 @nx_fmt_float(double %v, ptr %buf) {
entry:
  %isnan = fcmp uno double %v, 0.0
  br i1 %isnan, label %nan, label %chk
nan:
  call i32 (ptr, ...) @printf(ptr @.msg.nan)
  ret i64 3
chk:
  %av = call double @llvm.fabs.f64(double %v)
  %isinf = fcmp oeq double %av, 0x7FF0000000000000
  br i1 %isinf, label %inf, label %chk0
inf:
  %neg = fcmp olt double %v, 0.0
  br i1 %neg, label %ninf, label %pinf
ninf:
  call i32 (ptr, ...) @printf(ptr @.msg.ninf)
  ret i64 4
pinf:
  call i32 (ptr, ...) @printf(ptr @.msg.inf)
  ret i64 3
chk0:
  %iszero = fcmp oeq double %v, 0.0
  br i1 %iszero, label %zero, label %range
zero:
  call i32 (ptr, ...) @printf(ptr @.msg.zero)
  ret i64 1
range:
  %big = fcmp oge double %av, 1.0e15
  %tiny = fcmp olt double %av, 1.0e-12
  %sci = or i1 %big, %tiny
  br i1 %sci, label %fallback, label %sig
fallback:
  ; Extreme magnitudes: %.17g shortest-ish (matches interpreter closely enough).
  %nfb = call i32 (ptr, ...) @printf(ptr @.msg.sci, double %v)
  ret i64 0
sig:
  %expf = call double @llvm.log10.f64(double %av)
  %expfl = call double @llvm.floor.f64(double %expf)
  %exp = fptosi double %expfl to i32
  %sh = sub i32 14, %exp
  %shf = sitofp i32 %sh to double
  %scale = call double @llvm.pow.f64(double 1.0e1, double %shf)
  %mul = fmul double %v, %scale
  %rnd = call double @llvm.round.f64(double %mul)
  %r = fdiv double %rnd, %scale
  %rzero = fcmp oeq double %r, 0.0
  br i1 %rzero, label %zero, label %fmt
fmt:
  %dec = sub i32 14, %exp
  %dec0 = icmp slt i32 %dec, 0
  %dec1 = select i1 %dec0, i32 0, i32 %dec
  %decbig = icmp sgt i32 %dec1, 15
  %decimals = select i1 %decbig, i32 15, i32 %dec1
  call i32 (ptr, i64, ptr, ...) @snprintf(ptr %buf, i64 64, ptr @.msg.fdec, i32 %decimals, double %r, ptr @.msg.empty)
  ; trim trailing zeros then a trailing dot, in place
  %len0 = call i64 @nx_strlen(ptr %buf)
  br label %trim
trim:
  %len = phi i64 [%len0, %fmt], [%len2, %trimnext]
  %last = sub i64 %len, 1
  %chp = getelementptr i8, ptr %buf, i64 %last
  %ch = load i8, ptr %chp
  %is0 = icmp eq i8 %ch, 48
  %hasdot = call i1 @nx_hasdot(ptr %buf, i64 %len)
  %dotlast = icmp eq i8 %ch, 46
  br i1 %is0, label %dodot, label %ckdot
dodot:
  br i1 %hasdot, label %chop0, label %done
chop0:
  store i8 0, ptr %chp
  br label %trimnext
ckdot:
  br i1 %dotlast, label %chopdot, label %done
chopdot:
  store i8 0, ptr %chp
  br label %done
trimnext:
  %len2 = sub i64 %len, 1
  br label %trim
done:
  %fin = call i64 @nx_strlen(ptr %buf)
  call i32 (ptr, ...) @printf(ptr @.fmt.ss, i32 2147483647, ptr %buf)
  ret i64 %fin
}

define i64 @nx_strlen(ptr %s) {
entry:
  br label %loop
loop:
  %i = phi i64 [0, %entry], [%i2, %next]
  %p = getelementptr i8, ptr %s, i64 %i
  %c = load i8, ptr %p
  %end = icmp eq i8 %c, 0
  br i1 %end, label %out, label %next
next:
  %i2 = add i64 %i, 1
  br label %loop
out:
  ret i64 %i
}

define i1 @nx_hasdot(ptr %s, i64 %n) {
entry:
  br label %loop
loop:
  %i = phi i64 [0, %entry], [%i2, %next]
  %done = icmp eq i64 %i, %n
  br i1 %done, label %out0, label %chk
chk:
  %p = getelementptr i8, ptr %s, i64 %i
  %c = load i8, ptr %p
  %is = icmp eq i8 %c, 46
  br i1 %is, label %out1, label %next
next:
  %i2 = add i64 %i, 1
  br label %loop
out0:
  ret i1 false
out1:
  ret i1 true
}

; --- value constructors ---
define %NxVal @nx_int(i64 %v) {
entry:
  %r0 = insertvalue %NxVal zeroinitializer, i64 1, 0
  %r1 = insertvalue %NxVal %r0, i64 %v, 1
  ret %NxVal %r1
}
define %NxVal @nx_float(double %v) {
entry:
  %b = bitcast double %v to i64
  %r0 = insertvalue %NxVal zeroinitializer, i64 2, 0
  %r1 = insertvalue %NxVal %r0, i64 %b, 1
  ret %NxVal %r1
}
define %NxVal @nx_bool(i1 %v) {
entry:
  %e = zext i1 %v to i64
  %r0 = insertvalue %NxVal zeroinitializer, i64 3, 0
  %r1 = insertvalue %NxVal %r0, i64 %e, 1
  ret %NxVal %r1
}
define %NxVal @nx_str(ptr %p, i64 %n) {
entry:
  %pi = ptrtoint ptr %p to i64
  %r0 = insertvalue %NxVal zeroinitializer, i64 4, 0
  %r1 = insertvalue %NxVal %r0, i64 %pi, 1
  %r2 = insertvalue %NxVal %r1, i64 %n, 2
  ret %NxVal %r2
}
define %NxVal @nx_none() {
entry:
  ret %NxVal zeroinitializer
}

; --- printing ---
define void @nx_free_val(%NxVal %v) {
entry:
  %t = extractvalue %NxVal %v, 0
  switch i64 %t, label %done [
    i64 4, label %str
    i64 5, label %list
  ]
str:
  %p = extractvalue %NxVal %v, 1
  %pp = inttoptr i64 %p to ptr
  call void @free(ptr %pp)
  ret void
list:
  %hp = extractvalue %NxVal %v, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxList, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  %lp = getelementptr %NxList, ptr %h, i64 0, i32 1
  %len = load i64, ptr %lp
  br label %lcond
lcond:
  %i = phi i64 [0, %list], [%i2, %lbody]
  %fin = icmp eq i64 %i, %len
  br i1 %fin, label %lout, label %lbody
lbody:
  %ep = getelementptr %NxVal, ptr %data, i64 %i
  %e = load %NxVal, ptr %ep
  call void @nx_free_val(%NxVal %e)
  %i2 = add i64 %i, 1
  br label %lcond
lout:
  call void @free(ptr %data)
  call void @free(ptr %h)
  ret void
done:
  ret void
}

declare void @free(ptr)

; Threading for `parallel:` (unix; Windows lowers sequentially).
; pthread_t travels as i64 (slots zeroed first so 32-bit ids extend cleanly).
declare i32 @pthread_create(ptr, ptr, ptr, ptr)
declare i32 @pthread_join(i64, ptr)

define void @nx_print_val(%NxVal %v) {
entry:
  %tag = extractvalue %NxVal %v, 0
  switch i64 %tag, label %dflt [
    i64 0, label %none
    i64 1, label %int
    i64 2, label %float
    i64 3, label %bool
    i64 4, label %str
    i64 5, label %list
    i64 6, label %func
  ]
none:
  call i32 (ptr, ...) @printf(ptr @.fmt.none)
  ret void
int:
  %iv = extractvalue %NxVal %v, 1
  call i32 (ptr, ...) @printf(ptr @.fmt.ld, i64 %iv)
  ret void
float:
  %fb = extractvalue %NxVal %v, 1
  %fv = bitcast i64 %fb to double
  %buf = alloca [64 x i8]
  %bp = getelementptr [64 x i8], ptr %buf, i64 0, i64 0
  call i64 @nx_fmt_float(double %fv, ptr %bp)
  ret void
bool:
  %bv = extractvalue %NxVal %v, 1
  %bt = icmp ne i64 %bv, 0
  br i1 %bt, label %bt1, label %bt0
bt1:
  call i32 (ptr, ...) @printf(ptr @.fmt.true)
  ret void
bt0:
  call i32 (ptr, ...) @printf(ptr @.fmt.false)
  ret void
str:
  %sp = extractvalue %NxVal %v, 1
  %sl = extractvalue %NxVal %v, 2
  %spp = inttoptr i64 %sp to ptr
  %sl32 = trunc i64 %sl to i32
  call i32 (ptr, ...) @printf(ptr @.fmt.ss, i32 %sl32, ptr %spp)
  ret void
list:
  call i32 (ptr, ...) @printf(ptr @.fmt.lb)
  %lp = extractvalue %NxVal %v, 1
  %ll = extractvalue %NxVal %v, 2
  %lh = inttoptr i64 %lp to ptr
  %lhp = getelementptr %NxList, ptr %lh, i64 0, i32 0
  %ldata = load ptr, ptr %lhp
  br label %lcond
lcond:
  %li = phi i64 [0, %list], [%li2, %lval]
  %ldone = icmp eq i64 %li, %ll
  br i1 %ldone, label %lend, label %lbody
lbody:
  %lsp = icmp ne i64 %li, 0
  br i1 %lsp, label %lcomma, label %lval
lcomma:
  call i32 (ptr, ...) @printf(ptr @.fmt.comma)
  br label %lval
lval:
  %ldp = getelementptr %NxVal, ptr %ldata, i64 %li
  %le = load %NxVal, ptr %ldp
  call void @nx_print_val(%NxVal %le)
  %li2 = add i64 %li, 1
  br label %lcond
lend:
  call i32 (ptr, ...) @printf(ptr @.fmt.rb)
  ret void
func:
  %np = extractvalue %NxVal %v, 2
  %npp = inttoptr i64 %np to ptr
  call i32 (ptr, ...) @printf(ptr @.fmt.fnopen)
  call i32 (ptr, ...) @printf(ptr @.fmt.ss, i32 2147483647, ptr %npp)
  call i32 (ptr, ...) @printf(ptr @.fmt.close)
  ret void
dflt:
  call void @nx_panic(ptr @.msg.type)
  unreachable
}

define void @nx_print(ptr %arr, i64 %n) {
entry:
  br label %cond
cond:
  %i = phi i64 [0, %entry], [%i2, %val]
  %done = icmp eq i64 %i, %n
  br i1 %done, label %end, label %body
body:
  %sp = icmp ne i64 %i, 0
  br i1 %sp, label %comma, label %val
comma:
  call i32 (ptr, ...) @printf(ptr @.fmt.sp)
  br label %val
val:
  %ep = getelementptr %NxVal, ptr %arr, i64 %i
  %e = load %NxVal, ptr %ep
  call void @nx_print_val(%NxVal %e)
  %i2 = add i64 %i, 1
  br label %cond
end:
  call i32 (ptr, ...) @printf(ptr @.fmt.nl)
  ret void
}

; --- arithmetic (mirrors the interpreter matrix) ---
define %NxVal @nx_add(%NxVal %l, %NxVal %r) {
entry:
  %lt = extractvalue %NxVal %l, 0
  %rt = extractvalue %NxVal %r, 0
  %li = icmp eq i64 %lt, 1
  %ri = icmp eq i64 %rt, 1
  %ii = and i1 %li, %ri
  br i1 %ii, label %ints, label %c1
ints:
  %la = extractvalue %NxVal %l, 1
  %ra = extractvalue %NxVal %r, 1
  %s = add i64 %la, %ra
  %v = call %NxVal @nx_int(i64 %s)
  ret %NxVal %v
c1:
  %ls = icmp eq i64 %lt, 4
  %rs = icmp eq i64 %rt, 4
  %ss = and i1 %ls, %rs
  br i1 %ss, label %strs, label %nums
strs:
  %v2 = call %NxVal @nx_strcat(%NxVal %l, %NxVal %r)
  ret %NxVal %v2
nums:
  %lf = call double @nx_tonum(%NxVal %l)
  %rf = call double @nx_tonum(%NxVal %r)
  %s2 = fadd double %lf, %rf
  %v3 = call %NxVal @nx_float(double %s2)
  ret %NxVal %v3
}

define double @nx_tonum(%NxVal %v) {
entry:
  %t = extractvalue %NxVal %v, 0
  %ti = icmp eq i64 %t, 1
  br i1 %ti, label %isint, label %tonumchk
isint:
  %a = extractvalue %NxVal %v, 1
  %f = sitofp i64 %a to double
  ret double %f
tonumchk:
  %tf = icmp eq i64 %t, 2
  br i1 %tf, label %isflt, label %bad
isflt:
  %b = extractvalue %NxVal %v, 1
  %f2 = bitcast i64 %b to double
  ret double %f2
bad:
  call void @nx_panic(ptr @.msg.type)
  unreachable
}

define %NxVal @nx_sub(%NxVal %l, %NxVal %r) {
entry:
  %lt = extractvalue %NxVal %l, 0
  %rt = extractvalue %NxVal %r, 0
  %li = icmp eq i64 %lt, 1
  %ri = icmp eq i64 %rt, 1
  %ii = and i1 %li, %ri
  br i1 %ii, label %ints, label %nums
ints:
  %la = extractvalue %NxVal %l, 1
  %ra = extractvalue %NxVal %r, 1
  %s = sub i64 %la, %ra
  %v = call %NxVal @nx_int(i64 %s)
  ret %NxVal %v
nums:
  %lf = call double @nx_tonum(%NxVal %l)
  %rf = call double @nx_tonum(%NxVal %r)
  %s2 = fsub double %lf, %rf
  %v3 = call %NxVal @nx_float(double %s2)
  ret %NxVal %v3
}

define %NxVal @nx_mul(%NxVal %l, %NxVal %r) {
entry:
  %lt = extractvalue %NxVal %l, 0
  %rt = extractvalue %NxVal %r, 0
  %li = icmp eq i64 %lt, 1
  %ri = icmp eq i64 %rt, 1
  %ii = and i1 %li, %ri
  br i1 %ii, label %ints, label %nums
ints:
  %la = extractvalue %NxVal %l, 1
  %ra = extractvalue %NxVal %r, 1
  %s = mul i64 %la, %ra
  %v = call %NxVal @nx_int(i64 %s)
  ret %NxVal %v
nums:
  %lf = call double @nx_tonum(%NxVal %l)
  %rf = call double @nx_tonum(%NxVal %r)
  %s2 = fmul double %lf, %rf
  %v3 = call %NxVal @nx_float(double %s2)
  ret %NxVal %v3
}

define %NxVal @nx_div(%NxVal %l, %NxVal %r) {
entry:
  %lt = extractvalue %NxVal %l, 0
  %rt = extractvalue %NxVal %r, 0
  %li = icmp eq i64 %lt, 1
  %ri = icmp eq i64 %rt, 1
  %ii = and i1 %li, %ri
  br i1 %ii, label %ints, label %nums
ints:
  %la = extractvalue %NxVal %l, 1
  %ra = extractvalue %NxVal %r, 1
  %z = icmp eq i64 %ra, 0
  br i1 %z, label %dz, label %ok
dz:
  call void @nx_panic(ptr @.msg.divzero)
  unreachable
ok:
  %s = sdiv i64 %la, %ra
  %v = call %NxVal @nx_int(i64 %s)
  ret %NxVal %v
nums:
  %lf = call double @nx_tonum(%NxVal %l)
  %rf = call double @nx_tonum(%NxVal %r)
  %z2 = fcmp oeq double %rf, 0.0
  br i1 %z2, label %dz2, label %ok2
dz2:
  call void @nx_panic(ptr @.msg.divzero)
  unreachable
ok2:
  %s2 = fdiv double %lf, %rf
  %v3 = call %NxVal @nx_float(double %s2)
  ret %NxVal %v3
}

define %NxVal @nx_strcat(%NxVal %l, %NxVal %r) {
entry:
  %lp = extractvalue %NxVal %l, 1
  %ll = extractvalue %NxVal %l, 2
  %rp = extractvalue %NxVal %r, 1
  %rl = extractvalue %NxVal %r, 2
  %n = add i64 %ll, %rl
  %buf = call ptr @malloc(i64 %n)
  %lpp = inttoptr i64 %lp to ptr
  %rpp = inttoptr i64 %rp to ptr
  call void @nx_memcpy(ptr %buf, ptr %lpp, i64 %ll)
  %dst = getelementptr i8, ptr %buf, i64 %ll
  call void @nx_memcpy(ptr %dst, ptr %rpp, i64 %rl)
  %v = call %NxVal @nx_str(ptr %buf, i64 %n)
  ret %NxVal %v
}

define void @nx_memcpy(ptr %d, ptr %s, i64 %n) {
entry:
  br label %cond
cond:
  %i = phi i64 [0, %entry], [%i2, %body]
  %done = icmp eq i64 %i, %n
  br i1 %done, label %out, label %body
body:
  %sp = getelementptr i8, ptr %s, i64 %i
  %c = load i8, ptr %sp
  %dp = getelementptr i8, ptr %d, i64 %i
  store i8 %c, ptr %dp
  %i2 = add i64 %i, 1
  br label %cond
out:
  ret void
}

; --- comparisons -> Bool values ---
define %NxVal @nx_eq(%NxVal %l, %NxVal %r) {
entry:
  %b = call i1 @nx_eqb(%NxVal %l, %NxVal %r)
  %v = call %NxVal @nx_bool(i1 %b)
  ret %NxVal %v
}
define i1 @nx_eqb(%NxVal %l, %NxVal %r) {
entry:
  %lt = extractvalue %NxVal %l, 0
  %rt = extractvalue %NxVal %r, 0
  %same = icmp eq i64 %lt, %rt
  br i1 %same, label %samet, label %mixed
samet:
  %one = icmp eq i64 %lt, 1
  br i1 %one, label %ints, label %c1
ints:
  %la = extractvalue %NxVal %l, 1
  %ra = extractvalue %NxVal %r, 1
  %e = icmp eq i64 %la, %ra
  ret i1 %e
c1:
  %two = icmp eq i64 %lt, 2
  br i1 %two, label %floats, label %c2
floats:
  %fa = extractvalue %NxVal %l, 1
  %fb = extractvalue %NxVal %r, 1
  %e2 = icmp eq i64 %fa, %fb
  ret i1 %e2
c2:
  %three = icmp eq i64 %lt, 3
  br i1 %three, label %bools, label %c3
bools:
  %ba = extractvalue %NxVal %l, 1
  %bb = extractvalue %NxVal %r, 1
  %e3 = icmp eq i64 %ba, %bb
  ret i1 %e3
c3:
  %four = icmp eq i64 %lt, 4
  br i1 %four, label %strs, label %c4
strs:
  %e4 = call i1 @nx_streq(%NxVal %l, %NxVal %r)
  ret i1 %e4
c4:
  %five = icmp eq i64 %lt, 5
  br i1 %five, label %lists, label %rest
lists:
  %e5 = call i1 @nx_listeq(%NxVal %l, %NxVal %r)
  ret i1 %e5
rest:
  ret i1 true
mixed:
  %li = icmp eq i64 %lt, 1
  %rf = icmp eq i64 %rt, 2
  %m1 = and i1 %li, %rf
  br i1 %m1, label %if, label %m2
if:
  %la2 = extractvalue %NxVal %l, 1
  %lf = sitofp i64 %la2 to double
  %rbb = extractvalue %NxVal %r, 1
  %rf2 = bitcast i64 %rbb to double
  %e6 = fcmp oeq double %lf, %rf2
  ret i1 %e6
m2:
  %lf3 = icmp eq i64 %lt, 2
  %ri = icmp eq i64 %rt, 1
  %m3 = and i1 %lf3, %ri
  br i1 %m3, label %fi, label %no
fi:
  %fbb = extractvalue %NxVal %l, 1
  %ff = bitcast i64 %fbb to double
  %ra2 = extractvalue %NxVal %r, 1
  %rif = sitofp i64 %ra2 to double
  %e7 = fcmp oeq double %ff, %rif
  ret i1 %e7
no:
  ret i1 false
}

define i1 @nx_streq(%NxVal %l, %NxVal %r) {
entry:
  %ll = extractvalue %NxVal %l, 2
  %rl = extractvalue %NxVal %r, 2
  %e = icmp eq i64 %ll, %rl
  br i1 %e, label %cmp, label %no
cmp:
  %lp = extractvalue %NxVal %l, 1
  %rp = extractvalue %NxVal %r, 1
  %lpp = inttoptr i64 %lp to ptr
  %rpp = inttoptr i64 %rp to ptr
  %c = call i32 @memcmp(ptr %lpp, ptr %rpp, i64 %ll)
  %e2 = icmp eq i32 %c, 0
  ret i1 %e2
no:
  ret i1 false
}

define i1 @nx_listeq(%NxVal %l, %NxVal %r) {
entry:
  %ll = extractvalue %NxVal %l, 2
  %rl = extractvalue %NxVal %r, 2
  %e = icmp eq i64 %ll, %rl
  br i1 %e, label %loop, label %no
loop:
  %i = phi i64 [0, %entry], [%i2, %next]
  %done = icmp eq i64 %i, %ll
  br i1 %done, label %yes, label %chk
chk:
  %a = call %NxVal @nx_listget(%NxVal %l, i64 %i)
  %b = call %NxVal @nx_listget(%NxVal %r, i64 %i)
  %eq = call i1 @nx_eqb(%NxVal %a, %NxVal %b)
  br i1 %eq, label %next, label %no
next:
  %i2 = add i64 %i, 1
  br label %loop
yes:
  ret i1 true
no:
  ret i1 false
}

define i32 @nx_cmp(%NxVal %l, %NxVal %r) {
entry:
  %lt = extractvalue %NxVal %l, 0
  %rt = extractvalue %NxVal %r, 0
  %li = icmp eq i64 %lt, 1
  %ri = icmp eq i64 %rt, 1
  %ii = and i1 %li, %ri
  br i1 %ii, label %ints, label %c1
ints:
  %la = extractvalue %NxVal %l, 1
  %ra = extractvalue %NxVal %r, 1
  %lt2 = icmp slt i64 %la, %ra
  %gt = icmp sgt i64 %la, %ra
  %r1 = select i1 %lt2, i32 -1, i32 0
  %r2 = select i1 %gt, i32 1, i32 %r1
  ret i32 %r2
c1:
  %ls = icmp eq i64 %lt, 4
  %rs = icmp eq i64 %rt, 4
  %ss = and i1 %ls, %rs
  br i1 %ss, label %strs, label %nums
strs:
  %lp = extractvalue %NxVal %l, 1
  %ll = extractvalue %NxVal %l, 2
  %rp = extractvalue %NxVal %r, 1
  %rl = extractvalue %NxVal %r, 2
  %lpp = inttoptr i64 %lp to ptr
  %rpp = inttoptr i64 %rp to ptr
  %minl = icmp ult i64 %ll, %rl
  %mn = select i1 %minl, i64 %ll, i64 %rl
  %c = call i32 @memcmp(ptr %lpp, ptr %rpp, i64 %mn)
  %nz = icmp ne i32 %c, 0
  br i1 %nz, label %mcret, label %lenord
mcret:
  %neg = icmp slt i32 %c, 0
  %cr = select i1 %neg, i32 -1, i32 1
  ret i32 %cr
lenord:
  %llt = icmp ult i64 %ll, %rl
  %lgt = icmp ugt i64 %ll, %rl
  %lr1 = select i1 %llt, i32 -1, i32 0
  %lr2 = select i1 %lgt, i32 1, i32 %lr1
  ret i32 %lr2
nums:
  %lf = call double @nx_tonum(%NxVal %l)
  %rf = call double @nx_tonum(%NxVal %r)
  %flt = fcmp olt double %lf, %rf
  %fgt = fcmp ogt double %lf, %rf
  %fr1 = select i1 %flt, i32 -1, i32 0
  %fr2 = select i1 %fgt, i32 1, i32 %fr1
  ret i32 %fr2
}

; --- lists ---
define %NxVal @nx_new_list(i64 %cap) {
entry:
  %c0 = icmp eq i64 %cap, 0
  %cap2 = select i1 %c0, i64 4, i64 %cap
  %hsize = add i64 0, 24
  %h = call ptr @malloc(i64 24)
  %bytes = mul i64 %cap2, 24
  %data = call ptr @malloc(i64 %bytes)
  %dp = getelementptr %NxList, ptr %h, i64 0, i32 0
  store ptr %data, ptr %dp
  %lp = getelementptr %NxList, ptr %h, i64 0, i32 1
  store i64 0, ptr %lp
  %cp = getelementptr %NxList, ptr %h, i64 0, i32 2
  store i64 %cap2, ptr %cp
  %hi = ptrtoint ptr %h to i64
  %r0 = insertvalue %NxVal zeroinitializer, i64 5, 0
  %r1 = insertvalue %NxVal %r0, i64 %hi, 1
  %r2 = insertvalue %NxVal %r1, i64 0, 2
  ret %NxVal %r2
}

define void @nx_listpush(ptr %vp, %NxVal %v) {
entry:
  %lv = load %NxVal, ptr %vp
  %hp = extractvalue %NxVal %lv, 1
  %h = inttoptr i64 %hp to ptr
  %lp = getelementptr %NxList, ptr %h, i64 0, i32 1
  %len = load i64, ptr %lp
  %cp = getelementptr %NxList, ptr %h, i64 0, i32 2
  %cap = load i64, ptr %cp
  %full = icmp eq i64 %len, %cap
  br i1 %full, label %grow, label %store
grow:
  %ncap = mul i64 %cap, 2
  %dp = getelementptr %NxList, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  %nb = mul i64 %ncap, 24
  %nd = call ptr @realloc(ptr %data, i64 %nb)
  store ptr %nd, ptr %dp
  store i64 %ncap, ptr %cp
  br label %store
store:
  %dp2 = getelementptr %NxList, ptr %h, i64 0, i32 0
  %data2 = load ptr, ptr %dp2
  %ep = getelementptr %NxVal, ptr %data2, i64 %len
  store %NxVal %v, ptr %ep
  %len2 = add i64 %len, 1
  store i64 %len2, ptr %lp
  ; mirror len into the value payload
  %nv0 = insertvalue %NxVal %lv, i64 %len2, 2
  store %NxVal %nv0, ptr %vp
  ret void
}

define %NxVal @nx_listget(%NxVal %l, i64 %i) {
entry:
  %hp = extractvalue %NxVal %l, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxList, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  %ep = getelementptr %NxVal, ptr %data, i64 %i
  %e = load %NxVal, ptr %ep
  ret %NxVal %e
}

define %NxVal @nx_len(%NxVal %v) {
entry:
  %t = extractvalue %NxVal %v, 0
  %islist = icmp eq i64 %t, 5
  br i1 %islist, label %list, label %c
list:
  %n = extractvalue %NxVal %v, 2
  %r = call %NxVal @nx_int(i64 %n)
  ret %NxVal %r
c:
  %isstr = icmp eq i64 %t, 4
  br i1 %isstr, label %str, label %bad
str:
  %n2 = extractvalue %NxVal %v, 2
  %r2 = call %NxVal @nx_int(i64 %n2)
  ret %NxVal %r2
bad:
  call void @nx_panic(ptr @.msg.type)
  unreachable
}

define %NxVal @nx_index(%NxVal %b, %NxVal %ix) {
entry:
  %it = extractvalue %NxVal %b, 0
  %i = extractvalue %NxVal %ix, 1
  %islist = icmp eq i64 %it, 5
  br i1 %islist, label %list, label %c
list:
  %n = extractvalue %NxVal %b, 2
  %neg = icmp slt i64 %i, 0
  %adj = add i64 %i, %n
  %pos = select i1 %neg, i64 %adj, i64 %i
  %oob1 = icmp slt i64 %pos, 0
  %oob2 = icmp sge i64 %pos, %n
  %oob = or i1 %oob1, %oob2
  br i1 %oob, label %badidxl, label %ok
ok:
  %e = call %NxVal @nx_listget(%NxVal %b, i64 %pos)
  ret %NxVal %e
badidxl:
  call void @nx_panic_idx(i64 %i, i64 %n)
  unreachable
c:
  %isstr = icmp eq i64 %it, 4
  br i1 %isstr, label %str, label %bad
str:
  %n2 = extractvalue %NxVal %b, 2
  %neg2 = icmp slt i64 %i, 0
  %adj2 = add i64 %i, %n2
  %pos2 = select i1 %neg2, i64 %adj2, i64 %i
  %ob1 = icmp slt i64 %pos2, 0
  %ob2 = icmp sge i64 %pos2, %n2
  %ob = or i1 %ob1, %ob2
  br i1 %ob, label %badidxs, label %ok2
ok2:
  %sp = extractvalue %NxVal %b, 1
  %spp = inttoptr i64 %sp to ptr
  %cp = getelementptr i8, ptr %spp, i64 %pos2
  %buf = call ptr @malloc(i64 1)
  %ch = load i8, ptr %cp
  store i8 %ch, ptr %buf
  %v = call %NxVal @nx_str(ptr %buf, i64 1)
  ret %NxVal %v
badidxs:
  call void @nx_panic_idx(i64 %i, i64 %n2)
  unreachable
bad:
  call void @nx_panic(ptr @.msg.type)
  unreachable
}

define void @nx_panic_idx(i64 %i, i64 %n) {
entry:
  %buf = alloca [64 x i8]
  %bp = getelementptr [64 x i8], ptr %buf, i64 0, i64 0
  call i32 (ptr, i64, ptr, ...) @snprintf(ptr %bp, i64 64, ptr @.msg.oob, i64 %i, i64 %n)
  call void @nx_panic(ptr %bp)
  unreachable
}

define %NxVal @nx_neg(%NxVal %v) {
entry:
  %t = extractvalue %NxVal %v, 0
  %ti = icmp eq i64 %t, 1
  br i1 %ti, label %i, label %c
i:
  %a = extractvalue %NxVal %v, 1
  %n = sub i64 0, %a
  %r = call %NxVal @nx_int(i64 %n)
  ret %NxVal %r
c:
  %tf = icmp eq i64 %t, 2
  br i1 %tf, label %f, label %bad
f:
  %b = extractvalue %NxVal %v, 1
  %fb = bitcast i64 %b to double
  %n2 = fneg double %fb
  %r2 = call %NxVal @nx_float(double %n2)
  ret %NxVal %r2
bad:
  call void @nx_panic(ptr @.msg.type)
  unreachable
}

define %NxVal @nx_not(%NxVal %v) {
entry:
  %a = extractvalue %NxVal %v, 1
  %z = icmp eq i64 %a, 0
  %r = call %NxVal @nx_bool(i1 %z)
  ret %NxVal %r
}
