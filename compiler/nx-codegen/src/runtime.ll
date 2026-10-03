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
; Element counts are 32 bits. Both `n` and `cap` count *elements*, not
; bytes -- the byte size is computed at the allocation site -- and an
; element is at least 16 bytes, so reaching 2^31 of them would need 32 GiB
; of live data behind a runtime with no garbage collector. That is not a
; reachable state, so 32 bits is not a semantic limit, and it lets the two
; counters share one 8-byte slot instead of two.
;
; Every load widens back to i64 with `zext` and every store narrows with
; `trunc`, so all arithmetic outside the header stays 64-bit and no index
; or length is ever computed in 32 bits. `nx_listpush` and `nx_dictput`
; check before storing, so a count can never silently wrap.
;
; The header allocation stays `malloc(24)` on purpose: the saving is in
; the struct's own size, and shrinking the malloc too would be a separate
; change with its own justification.
%NxList = type { ptr, i32, i32 }
; Dict header mirrors a list; entries are (key, value) pairs.
%NxDict = type { ptr, i32, i32 }
%NxDictEntry = type { %NxVal, %NxVal }
%NxRec = type { ptr, i64, ptr }  ; fields, nfields, type descriptor
%NxDesc = type { ptr, i64, i64, ptr }  ; type-name bytes, type-name len, nfields, field-name table
%NxRecName = type { i64, ptr } ; name length, name bytes

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
; R1 (docs/grammar.md 3.1.1): integer overflow traps. The with.overflow
; family returns the wrapped value AND the overflow flag from a single
; operation, so checking costs nothing an add did not already cost, and
; LLVM deletes both the flag and the branch whenever it can prove the
; range -- which it can for most loop counters and index arithmetic.
declare { i64, i1 } @llvm.sadd.with.overflow.i64(i64, i64)
declare { i64, i1 } @llvm.ssub.with.overflow.i64(i64, i64)
declare { i64, i1 } @llvm.smul.with.overflow.i64(i64, i64)

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
@.fmt.lparen = private constant [2 x i8] c"(\00"
@.fmt.rparen = private constant [2 x i8] c")\00"
@.fmt.brace_l = private constant [2 x i8] c"{\00"
@.fmt.brace_r = private constant [2 x i8] c"}\00"
@.fmt.colonsp = private constant [3 x i8] c": \00"
@.fmt.comma = private constant [3 x i8] c", \00"
@.fmt.fnopen = private constant [5 x i8] c"<fn \00"
@.fmt.close = private constant [2 x i8] c">\00"
@.msg.divzero = private constant [17 x i8] c"division by zero\00"
@.msg.overflow = private constant [17 x i8] c"integer overflow\00"
; Reached only by a container whose element count would pass 2^31, which
; needs tens of gigabytes of live data behind a runtime with no collector.
; It exists so a 32-bit header field can never wrap silently.
@.msg.toomany = private constant [36 x i8] c"list or dict exceeded 2^31 elements\00"
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
    i64 7, label %dict
    i64 8, label %rec
  ]
; A string is *not* freed here.
;
; `nx_str` does not copy: it stores the caller's pointer, so a string
; literal's payload points straight into read-only static memory
; (`@.nxstr.N`). Calling `free` on that address corrupts the heap -- it
; was reachable from any function whose Unique local held a string, and it
; aborted with STATUS_HEAP_CORRUPTION.
;
; Freeing is consistent with the rest of the model rather than a patch over
; one crash. Strings are shared and never mutated in place, so sharing one
; is observationally identical to copying it -- which is why the backend's
; `needs_clone` never copies a string. A shared object with many owners
; cannot be freed by any one of them. Only container storage, which is
; owned and unaliased, is released here.
;
; `nx_strcat` and `nx_slice` do allocate, so those leak. That is the
; documented model for this release: malloc'd memory lives for the process
; lifetime and the arena story arrives with the memory planner.
str:
  ret void
list:
  %hp = extractvalue %NxVal %v, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxList, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  %lp = getelementptr %NxList, ptr %h, i64 0, i32 1
  %len32 = load i32, ptr %lp
  %len = zext i32 %len32 to i64
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
dict:
  ; A dict frees each entry's storage the same way a list frees its
  ; elements: entries hold boxes, and only the entry array and header
  ; themselves are heap blocks owned here.
  %dhp = extractvalue %NxVal %v, 1
  %dh = inttoptr i64 %dhp to ptr
  %ddp = getelementptr %NxDict, ptr %dh, i64 0, i32 0
  %ddata = load ptr, ptr %ddp
  %dlp = getelementptr %NxDict, ptr %dh, i64 0, i32 1
  %dlen32 = load i32, ptr %dlp
  %dlen = zext i32 %dlen32 to i64
  br label %dcond
dcond:
  %di = phi i64 [0, %dict], [%di2, %dbody]
  %dfin = icmp eq i64 %di, %dlen
  br i1 %dfin, label %dout, label %dbody
dbody:
  %dep = getelementptr %NxDictEntry, ptr %ddata, i64 %di
  %dkp = getelementptr %NxDictEntry, ptr %dep, i64 0, i32 0
  %dvp = getelementptr %NxDictEntry, ptr %dep, i64 0, i32 1
  %dk = load %NxVal, ptr %dkp
  %dv = load %NxVal, ptr %dvp
  call void @nx_free_val(%NxVal %dk)
  call void @nx_free_val(%NxVal %dv)
  %di2 = add i64 %di, 1
  br label %dcond
dout:
  call void @free(ptr %ddata)
  call void @free(ptr %dh)
  ret void
rec:
  ; Fields are boxes owned by the record; the descriptor is a static
  ; global and is never freed.
  %rhp = extractvalue %NxVal %v, 1
  %rh = inttoptr i64 %rhp to ptr
  %rfp = getelementptr %NxRec, ptr %rh, i64 0, i32 0
  %rfields = load ptr, ptr %rfp
  %rnp = getelementptr %NxRec, ptr %rh, i64 0, i32 1
  %rn = load i64, ptr %rnp
  br label %rcond
rcond:
  %rri = phi i64 [0, %rec], [%rri2, %rbody]
  %rfin = icmp eq i64 %rri, %rn
  br i1 %rfin, label %rout, label %rbody
rbody:
  %rep = getelementptr %NxVal, ptr %rfields, i64 %rri
  %re = load %NxVal, ptr %rep
  call void @nx_free_val(%NxVal %re)
  %rri2 = add i64 %rri, 1
  br label %rcond
rout:
  call void @free(ptr %rfields)
  call void @free(ptr %rh)
  ret void
done:
  ret void
}

declare void @free(ptr)

define void @nx_print_val(%NxVal %v) {
entry:
  ; Scratch for the float branch, hoisted here so it is allocated once
  ; rather than per switch arm.
  %buf = alloca [64 x i8]
  %tag = extractvalue %NxVal %v, 0
  switch i64 %tag, label %dflt [
    i64 0, label %none
    i64 1, label %int
    i64 2, label %float
    i64 3, label %bool
    i64 4, label %str
    i64 5, label %list
    i64 6, label %func
    i64 7, label %dict
    i64 8, label %rec
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
; Dicts print in insertion order, the same order iteration yields, so a
; program's output stays stable across runs.
dict:
  call i32 (ptr, ...) @printf(ptr @.fmt.brace_l)
  %dhp = extractvalue %NxVal %v, 1
  %dn = extractvalue %NxVal %v, 2
  %dh = inttoptr i64 %dhp to ptr
  %dhp2 = getelementptr %NxDict, ptr %dh, i64 0, i32 0
  %ddata = load ptr, ptr %dhp2
  br label %dcond
dcond:
  %di = phi i64 [0, %dict], [%di2, %dval]
  %ddone = icmp eq i64 %di, %dn
  br i1 %ddone, label %dend, label %dbody
dbody:
  %dsp = icmp ne i64 %di, 0
  br i1 %dsp, label %dcomma, label %dval
dcomma:
  call i32 (ptr, ...) @printf(ptr @.fmt.comma)
  br label %dval
dval:
  %dep = getelementptr %NxDictEntry, ptr %ddata, i64 %di
  %dkp = getelementptr %NxDictEntry, ptr %dep, i64 0, i32 0
  %dvp = getelementptr %NxDictEntry, ptr %dep, i64 0, i32 1
  %dk = load %NxVal, ptr %dkp
  %dv = load %NxVal, ptr %dvp
  call void @nx_print_val(%NxVal %dk)
  call i32 (ptr, ...) @printf(ptr @.fmt.colonsp)
  call void @nx_print_val(%NxVal %dv)
  %di2 = add i64 %di, 1
  br label %dcond
dend:
  call i32 (ptr, ...) @printf(ptr @.fmt.brace_r)
  ret void
; Records print in constructor form -- `Point(1, 2)` -- so the output
; reads back as the expression that would build the value. The type name
; comes from the descriptor, which the backend emits per declaration.
rec:
  %rd = call ptr @nx_rec_desc(%NxVal %v)
  %rtp = getelementptr %NxDesc, ptr %rd, i64 0, i32 0
  %rt = load ptr, ptr %rtp
  %rlp = getelementptr %NxDesc, ptr %rd, i64 0, i32 1
  %rl = load i64, ptr %rlp
  %rl32 = trunc i64 %rl to i32
  call i32 (ptr, ...) @printf(ptr @.fmt.ss, i32 %rl32, ptr %rt)
  call i32 (ptr, ...) @printf(ptr @.fmt.lparen)
  %rn = call i64 @nx_rec_nfields(%NxVal %v)
  br label %rcond
rcond:
  %ri = phi i64 [0, %rec], [%ri2, %rval]
  %rdone = icmp eq i64 %ri, %rn
  br i1 %rdone, label %rend, label %rbody
rbody:
  %rsp = icmp ne i64 %ri, 0
  br i1 %rsp, label %rcomma, label %rval
rcomma:
  call i32 (ptr, ...) @printf(ptr @.fmt.comma)
  br label %rval
rval:
  %re = call %NxVal @nx_rec_get(%NxVal %v, i64 %ri)
  call void @nx_print_val(%NxVal %re)
  %ri2 = add i64 %ri, 1
  br label %rcond
rend:
  call i32 (ptr, ...) @printf(ptr @.fmt.rparen)
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
    ; R1: a wrapped Int is not an answer, it is a wrong number the
    ; program then goes on to reason about. Trap instead.
    %o = call { i64, i1 } @llvm.sadd.with.overflow.i64(i64 %la, i64 %ra)
    %s = extractvalue { i64, i1 } %o, 0
    %of = extractvalue { i64, i1 } %o, 1
    br i1 %of, label %iovf, label %iok
  iovf:
    call void @nx_panic(ptr @.msg.overflow)
    unreachable
  iok:
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
    ; R1: a wrapped Int is not an answer, it is a wrong number the
    ; program then goes on to reason about. Trap instead.
    %o = call { i64, i1 } @llvm.ssub.with.overflow.i64(i64 %la, i64 %ra)
    %s = extractvalue { i64, i1 } %o, 0
    %of = extractvalue { i64, i1 } %o, 1
    br i1 %of, label %iovf, label %iok
  iovf:
    call void @nx_panic(ptr @.msg.overflow)
    unreachable
  iok:
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
    ; R1: a wrapped Int is not an answer, it is a wrong number the
    ; program then goes on to reason about. Trap instead.
    %o = call { i64, i1 } @llvm.smul.with.overflow.i64(i64 %la, i64 %ra)
    %s = extractvalue { i64, i1 } %o, 0
    %of = extractvalue { i64, i1 } %o, 1
    br i1 %of, label %iovf, label %iok
  iovf:
    call void @nx_panic(ptr @.msg.overflow)
    unreachable
  iok:
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

; --- unboxed division: same zero-check panic as the boxed path ---
define i64 @nx_div_i64(i64 %l, i64 %r) {
entry:
  %z = icmp eq i64 %r, 0
  br i1 %z, label %dz, label %chk2
dz:
  call void @nx_panic(ptr @.msg.divzero)
  unreachable
chk2:
  ; sdiv i64 INT64_MIN, -1 is POISON in LLVM, not a wrapped value: the
  ; quotient simply does not fit in i64. Left unchecked this is undefined
  ; behaviour, so it traps like every other arithmetic overflow.
  %ismin = icmp eq i64 %l, -9223372036854775808
  %isneg1 = icmp eq i64 %r, -1
  %bad = and i1 %ismin, %isneg1
  br i1 %bad, label %ovf, label %ok
ovf:
  call void @nx_panic(ptr @.msg.overflow)
  unreachable
ok:
  %s = sdiv i64 %l, %r
  ret i64 %s
}

define double @nx_fdiv(double %l, double %r) {
entry:
  %z = fcmp oeq double %r, 0.0
  br i1 %z, label %dz, label %ok
dz:
  call void @nx_panic(ptr @.msg.divzero)
  unreachable
ok:
  %s = fdiv double %l, %r
  ret double %s
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
  ; IEEE compare, not bit equality: NaN != NaN and 0.0 == -0.0 must hold
  ; so that the unboxed and boxed paths agree.
  %fa = extractvalue %NxVal %l, 1
  %fb = extractvalue %NxVal %r, 1
  %d1 = bitcast i64 %fa to double
  %d2 = bitcast i64 %fb to double
  %e2 = fcmp oeq double %d1, %d2
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
  br i1 %five, label %lists, label %c5
lists:
  %e5 = call i1 @nx_listeq(%NxVal %l, %NxVal %r)
  ret i1 %e5
c5:
  %seven = icmp eq i64 %lt, 7
  br i1 %seven, label %dicts, label %c6
dicts:
  %edict = call i1 @nx_dicteq(%NxVal %l, %NxVal %r)
  ret i1 %edict
c6:
  %eight = icmp eq i64 %lt, 8
  br i1 %eight, label %recs, label %rest
recs:
  %erec = call i1 @nx_receq(%NxVal %l, %NxVal %r)
  ret i1 %erec
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
  ; 16, not 24: sizeof(%NxList) after the counters became i32. The malloc
  ; and the struct have to move together -- narrowing the type while leaving
  ; the allocation alone would save nothing at all, and the wasted 8 bytes
  ; would sit between the pointer and the counters as tail padding.
  %hsize = add i64 0, 16
  %h = call ptr @malloc(i64 16)
  %bytes = mul i64 %cap2, 24
  %data = call ptr @malloc(i64 %bytes)
  %dp = getelementptr %NxList, ptr %h, i64 0, i32 0
  store ptr %data, ptr %dp
  %lp = getelementptr %NxList, ptr %h, i64 0, i32 1
  store i32 0, ptr %lp
  %cp = getelementptr %NxList, ptr %h, i64 0, i32 2
  %cap232 = trunc i64 %cap2 to i32
  store i32 %cap232, ptr %cp
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
  %len32 = load i32, ptr %lp
  %len = zext i32 %len32 to i64
  %cp = getelementptr %NxList, ptr %h, i64 0, i32 2
  %cap32 = load i32, ptr %cp
  %cap = zext i32 %cap32 to i64
  %full = icmp eq i64 %len, %cap
  br i1 %full, label %grow, label %store
grow:
  %ncap = mul i64 %cap, 2
  %dp = getelementptr %NxList, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  %nb = mul i64 %ncap, 24
  ; A list cannot reach 2^31 elements: each is at least 16 bytes of live
  ; data behind a runtime with no collector, so the count has nowhere to
  ; wrap. The check is here anyway, because a header field that could
  ; silently truncate is the kind of thing that only shows up as corruption
  ; much later.
  %ncapfits = icmp ule i64 %ncap, 2147483647
  br i1 %ncapfits, label %realloc, label %toobig
toobig:
  call void @nx_panic(ptr @.msg.toomany)
  br label %realloc
realloc:
  %nd = call ptr @realloc(ptr %data, i64 %nb)
  store ptr %nd, ptr %dp
  %ncap32 = trunc i64 %ncap to i32
  store i32 %ncap32, ptr %cp
  br label %store
store:
  %dp2 = getelementptr %NxList, ptr %h, i64 0, i32 0
  %data2 = load ptr, ptr %dp2
  %ep = getelementptr %NxVal, ptr %data2, i64 %len
  store %NxVal %v, ptr %ep
  %len2 = add i64 %len, 1
  %len232 = trunc i64 %len2 to i32
  store i32 %len232, ptr %lp
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
  br i1 %isstr, label %str, label %c2
str:
  %n2 = extractvalue %NxVal %v, 2
  %r2 = call %NxVal @nx_int(i64 %n2)
  ret %NxVal %r2
c2:
  %isdict = icmp eq i64 %t, 7
  br i1 %isdict, label %dict, label %bad
dict:
  ; A dict mirrors its length in the same field a list uses.
  %n3 = extractvalue %NxVal %v, 2
  %r3 = call %NxVal @nx_int(i64 %n3)
  ret %NxVal %r3
bad:
  call void @nx_panic(ptr @.msg.type)
  unreachable
}

define %NxVal @nx_index(%NxVal %b, %NxVal %ix) {
entry:
  %it = extractvalue %NxVal %b, 0
  ; A dict is keyed by value, so it is resolved before the index is
  %isd = icmp eq i64 %it, 7
  br i1 %isd, label %dict, label %pidx
dict:
  %dv = call %NxVal @nx_dictget(%NxVal %b, %NxVal %ix)
  ret %NxVal %dv
pidx:
  ; A positional index has to be an Int; a string key would read the wrong
  ; field of the value, so it fails rather than misbehaving quietly.
  %vt = extractvalue %NxVal %ix, 0
  %vi = icmp eq i64 %vt, 1
  br i1 %vi, label %posi, label %bad
posi:
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
  ; Hoisted into the entry block. An alloca inside a branch would be a
  ; fresh allocation on every path that reaches it.
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

; --- memoization: purity-proven function cache (fixed 4096 slots) ---
; Key: (fnid, args). Only scalar args (Int/Float/Bool/Str) participate;
; anything else misses. Eviction clears the table when full. Locking is
; a cmpxchg spinlock so the IR stays platform-neutral (pthreads-free).
%NxMemoSlot = type { i64, i64, [8 x %NxVal], %NxVal, i1 }

@nx_memo_table = global [4096 x %NxMemoSlot] zeroinitializer
@nx_memo_count = global i64 0
@nx_memo_lock = global i64 0

define void @nx_spin_lock() {
entry:
  br label %spin
spin:
  %r = cmpxchg ptr @nx_memo_lock, i64 0, i64 1 acquire monotonic
  %ok = extractvalue { i64, i1 } %r, 1
  br i1 %ok, label %held, label %spin
held:
  ret void
}

define void @nx_spin_unlock() {
entry:
  store atomic i64 0, ptr @nx_memo_lock release, align 8
  ret void
}

; All args scalar (tags 1..4) and at most 8 of them?
define i1 @nx_memo_args_ok(ptr %args, i64 %nargs) {
entry:
  %many = icmp sgt i64 %nargs, 8
  br i1 %many, label %no, label %loop
loop:
  %i = phi i64 [0, %entry], [%i2, %next]
  %done = icmp eq i64 %i, %nargs
  br i1 %done, label %yes, label %chk
chk:
  %ep = getelementptr %NxVal, ptr %args, i64 %i
  %e = load %NxVal, ptr %ep
  %t = extractvalue %NxVal %e, 0
  %lo = icmp sge i64 %t, 1
  %hi = icmp sle i64 %t, 4
  %ok = and i1 %lo, %hi
  br i1 %ok, label %next, label %no
next:
  %i2 = add i64 %i, 1
  br label %loop
yes:
  ret i1 true
no:
  ret i1 false
}

define i64 @nx_memo_mixstr(i64 %h, i64 %ptr, i64 %len) {
entry:
  %h1 = xor i64 %h, %len
  %h2 = mul i64 %h1, 1099511628211
  %big = icmp sge i64 %len, 8
  br i1 %big, label %wide, label %bytes
wide:
  %pp = inttoptr i64 %ptr to ptr
  %w1 = load i64, ptr %pp
  %h3 = xor i64 %h2, %w1
  %h4 = mul i64 %h3, 1099511628211
  %last = sub i64 %len, 8
  %lp = getelementptr i8, ptr %pp, i64 %last
  %w2 = load i64, ptr %lp
  %h5 = xor i64 %h4, %w2
  %h6 = mul i64 %h5, 1099511628211
  ret i64 %h6
bytes:
  br label %bloop
bloop:
  %i = phi i64 [0, %bytes], [%i2, %bread]
  %hh = phi i64 [%h2, %bytes], [%hh2, %bread]
  %fin = icmp eq i64 %i, %len
  br i1 %fin, label %bout, label %bread
bread:
  %pp2 = inttoptr i64 %ptr to ptr
  %cp = getelementptr i8, ptr %pp2, i64 %i
  %c = load i8, ptr %cp
  %ce = zext i8 %c to i64
  %hx = xor i64 %hh, %ce
  %hh2 = mul i64 %hx, 1099511628211
  %i2 = add i64 %i, 1
  br label %bloop
bout:
  ret i64 %hh
}

define i64 @nx_memo_hash(i64 %fnid, ptr %args, i64 %nargs) {
entry:
  br label %loop
loop:
  %i = phi i64 [0, %entry], [%i2, %nextv]
  %h = phi i64 [14695981039346656037, %entry], [%h3, %nextv]
  %done = icmp eq i64 %i, %nargs
  br i1 %done, label %out, label %mix
mix:
  %ep = getelementptr %NxVal, ptr %args, i64 %i
  %e = load %NxVal, ptr %ep
  %t = extractvalue %NxVal %e, 0
  %a = extractvalue %NxVal %e, 1
  %b = extractvalue %NxVal %e, 2
  %h0 = xor i64 %h, %t
  %h1 = mul i64 %h0, 1099511628211
  %h2 = xor i64 %h1, %a
  %hm = mul i64 %h2, 1099511628211
  %isstr = icmp eq i64 %t, 4
  br i1 %isstr, label %str, label %plain
str:
  %hs = call i64 @nx_memo_mixstr(i64 %hm, i64 %a, i64 %b)
  br label %nextv
plain:
  %hp = xor i64 %hm, %b
  %h3p = mul i64 %hp, 1099511628211
  br label %nextv
nextv:
  %h3 = phi i64 [%hs, %str], [%h3p, %plain]
  %i2 = add i64 %i, 1
  br label %loop
out:
  %hf = xor i64 %h, %fnid
  %hmf = mul i64 %hf, 1099511628211
  %hn = xor i64 %hmf, %nargs
  %hnm = mul i64 %hn, 1099511628211
  ret i64 %hnm
}

define i1 @nx_memo_keyeq(ptr %slot, i64 %fnid, ptr %args, i64 %nargs) {
entry:
  %fp = getelementptr %NxMemoSlot, ptr %slot, i64 0, i32 0
  %f = load i64, ptr %fp
  %fe = icmp eq i64 %f, %fnid
  br i1 %fe, label %c1, label %no
c1:
  %np = getelementptr %NxMemoSlot, ptr %slot, i64 0, i32 1
  %n = load i64, ptr %np
  %ne = icmp eq i64 %n, %nargs
  br i1 %ne, label %loop, label %no
loop:
  %i = phi i64 [0, %c1], [%i2, %next]
  %done = icmp eq i64 %i, %nargs
  br i1 %done, label %yes, label %cmp
cmp:
  %kp = getelementptr %NxMemoSlot, ptr %slot, i64 0, i32 2, i64 %i
  %k = load %NxVal, ptr %kp
  %ep = getelementptr %NxVal, ptr %args, i64 %i
  %e = load %NxVal, ptr %ep
  %kt = extractvalue %NxVal %k, 0
  %et = extractvalue %NxVal %e, 0
  %te = icmp eq i64 %kt, %et
  br i1 %te, label %c2, label %no
c2:
  %isstr = icmp eq i64 %kt, 4
  br i1 %isstr, label %str, label %c3
str:
  %eq = call i1 @nx_streq(%NxVal %k, %NxVal %e)
  br i1 %eq, label %next, label %no
c3:
  %ka = extractvalue %NxVal %k, 1
  %ea = extractvalue %NxVal %e, 1
  %ae = icmp eq i64 %ka, %ea
  br i1 %ae, label %c4, label %no
c4:
  %kb = extractvalue %NxVal %k, 2
  %eb = extractvalue %NxVal %e, 2
  %be = icmp eq i64 %kb, %eb
  br i1 %be, label %next, label %no
next:
  %i2 = add i64 %i, 1
  br label %loop
yes:
  ret i1 true
no:
  ret i1 false
}

define i1 @nx_memo_get(i64 %fnid, ptr %args, i64 %nargs, ptr %out) {
entry:
  %ok = call i1 @nx_memo_args_ok(ptr %args, i64 %nargs)
  br i1 %ok, label %lock, label %miss
lock:
  call void @nx_spin_lock()
  %h = call i64 @nx_memo_hash(i64 %fnid, ptr %args, i64 %nargs)
  %idx0 = urem i64 %h, 4096
  br label %probe
probe:
  %idx = phi i64 [%idx0, %lock], [%idx2, %next]
  %n = phi i64 [0, %lock], [%n2, %next]
  %full = icmp eq i64 %n, 4096
  br i1 %full, label %missrel, label %chk
chk:
  %slot = getelementptr [4096 x %NxMemoSlot], ptr @nx_memo_table, i64 0, i64 %idx
  %up = getelementptr %NxMemoSlot, ptr %slot, i64 0, i32 4
  %used = load i1, ptr %up
  br i1 %used, label %cmp, label %missrel
cmp:
  %eq = call i1 @nx_memo_keyeq(ptr %slot, i64 %fnid, ptr %args, i64 %nargs)
  br i1 %eq, label %hit, label %next
next:
  %t = add i64 %idx, 1
  %idx2 = urem i64 %t, 4096
  %n2 = add i64 %n, 1
  br label %probe
hit:
  %vp = getelementptr %NxMemoSlot, ptr %slot, i64 0, i32 3
  %v = load %NxVal, ptr %vp
  store %NxVal %v, ptr %out
  call void @nx_spin_unlock()
  ret i1 true
missrel:
  call void @nx_spin_unlock()
  br label %miss
miss:
  ret i1 false
}

define void @nx_memo_clear() {
entry:
  br label %loop
loop:
  %i = phi i64 [0, %entry], [%i2, %clr]
  %done = icmp eq i64 %i, 4096
  br i1 %done, label %out, label %clr
clr:
  %slot = getelementptr [4096 x %NxMemoSlot], ptr @nx_memo_table, i64 0, i64 %i
  %up = getelementptr %NxMemoSlot, ptr %slot, i64 0, i32 4
  store i1 false, ptr %up
  %i2 = add i64 %i, 1
  br label %loop
out:
  store i64 0, ptr @nx_memo_count
  ret void
}

define void @nx_memo_put(i64 %fnid, ptr %args, i64 %nargs, %NxVal %val) {
entry:
  %ok = call i1 @nx_memo_args_ok(ptr %args, i64 %nargs)
  br i1 %ok, label %lock, label %out
lock:
  call void @nx_spin_lock()
  %cnt = load i64, ptr @nx_memo_count
  %full = icmp sge i64 %cnt, 4096
  br i1 %full, label %clr, label %go
clr:
  call void @nx_memo_clear()
  br label %go
go:
  %h = call i64 @nx_memo_hash(i64 %fnid, ptr %args, i64 %nargs)
  %idx0 = urem i64 %h, 4096
  br label %probe
probe:
  %idx = phi i64 [%idx0, %go], [%idx2, %next]
  %n = phi i64 [0, %go], [%n2, %next]
  %spin = icmp eq i64 %n, 4096
  br i1 %spin, label %rel, label %chk
chk:
  %slot = getelementptr [4096 x %NxMemoSlot], ptr @nx_memo_table, i64 0, i64 %idx
  %up = getelementptr %NxMemoSlot, ptr %slot, i64 0, i32 4
  %used = load i1, ptr %up
  br i1 %used, label %cmp, label %ins
cmp:
  %eq = call i1 @nx_memo_keyeq(ptr %slot, i64 %fnid, ptr %args, i64 %nargs)
  br i1 %eq, label %rel, label %next
next:
  %t = add i64 %idx, 1
  %idx2 = urem i64 %t, 4096
  %n2 = add i64 %n, 1
  br label %probe
ins:
  %fp = getelementptr %NxMemoSlot, ptr %slot, i64 0, i32 0
  store i64 %fnid, ptr %fp
  %np = getelementptr %NxMemoSlot, ptr %slot, i64 0, i32 1
  store i64 %nargs, ptr %np
  br label %copy
copy:
  %ci = phi i64 [0, %ins], [%ci2, %ccopy]
  %cdone = icmp eq i64 %ci, %nargs
  br i1 %cdone, label %store, label %ccopy
ccopy:
  %kp = getelementptr %NxMemoSlot, ptr %slot, i64 0, i32 2, i64 %ci
  %ep = getelementptr %NxVal, ptr %args, i64 %ci
  %e = load %NxVal, ptr %ep
  store %NxVal %e, ptr %kp
  %ci2 = add i64 %ci, 1
  br label %copy
store:
  %vp = getelementptr %NxMemoSlot, ptr %slot, i64 0, i32 3
  store %NxVal %val, ptr %vp
  store i1 true, ptr %up
  %c2 = load i64, ptr @nx_memo_count
  %c3 = add i64 %c2, 1
  store i64 %c3, ptr @nx_memo_count
  br label %rel
rel:
  call void @nx_spin_unlock()
  br label %out
out:
  ret void
}

; =====================================================================

; =====================================================================
; Stage 1: integral operators, dicts, membership, slicing.
;
; Tag 7 Dict is new: a = header ptr, b = len. The header reuses the
; { data, len, cap } shape of a list, but data points at %NxDictEntry
; (key, value) pairs instead of bare values.
;
; Dicts are deliberately an insertion-ordered vector rather than a hash
; table. Iteration order is part of the language's determinism contract, and
; a hash map would make it depend on hashing. Linear lookup
; is the right trade at the sizes a general-purpose program holds; the
; record/type work can revisit this if measurements justify it.
; =====================================================================

@.msg.modzero = private constant [15 x i8] c"modulo by zero\00"
@.msg.shift = private constant [28 x i8] c"shift distance out of range\00"
@.msg.assert = private constant [17 x i8] c"assertion failed\00"
@.msg.nokey = private constant [14 x i8] c"key not found\00"
@.msg.step = private constant [28 x i8] c"slice step must be positive\00"
@.msg.nofield = private constant [23 x i8] c"type has no such field\00"
@.msg.negexp = private constant [71 x i8] c"negative exponent on Int; use a Float exponent for a fractional result\00"
; --- integral operators -------------------------------------------
; `%` and `//` both need floor division, so it is factored out once.
; `sdiv` truncates toward zero, which is wrong for negative operands;
; the explicit correction is what makes -7 // 3 equal -3 and -7 % 3 equal
; 2, matching the interpreter exactly. Reusing sdiv's quotient would
; make the two disagree on half of all negative inputs.

define i64 @nx_floordiv_i64(i64 %l, i64 %r) {
entry:
  %z = icmp eq i64 %r, 0
  br i1 %z, label %dz, label %chk2
dz:
  call void @nx_panic(ptr @.msg.divzero)
  unreachable
chk2:
  ; sdiv/srem of INT64_MIN by -1 is POISON in LLVM: the quotient does not
  ; fit in i64. Unchecked, this is undefined behaviour.
  %ismin = icmp eq i64 %l, -9223372036854775808
  %isneg1 = icmp eq i64 %r, -1
  %bad = and i1 %ismin, %isneg1
  br i1 %bad, label %ovf, label %go
ovf:
  call void @nx_panic(ptr @.msg.overflow)
  unreachable
go:
  %q = sdiv i64 %l, %r
  %rem = srem i64 %l, %r
  %remz = icmp eq i64 %rem, 0
  br i1 %remz, label %done, label %fix
fix:
  ; Truncation rounded the wrong way only when the operands had
  ; opposite signs and the division was inexact.
  %lpos = icmp sgt i64 %l, 0
  %rpos = icmp sgt i64 %r, 0
  %opp = xor i1 %lpos, %rpos
  br i1 %opp, label %down, label %done
down:
  %q2 = sub i64 %q, 1
  ret i64 %q2
done:
  ret i64 %q
}

define i64 @nx_mod_i64(i64 %l, i64 %r) {
entry:
  %z = icmp eq i64 %r, 0
  br i1 %z, label %mz, label %chk2
mz:
  call void @nx_panic(ptr @.msg.modzero)
  unreachable
chk2:
  ; sdiv/srem of INT64_MIN by -1 is POISON in LLVM: the quotient does not
  ; fit in i64. Unchecked, this is undefined behaviour.
  %ismin = icmp eq i64 %l, -9223372036854775808
  %isneg1 = icmp eq i64 %r, -1
  %bad = and i1 %ismin, %isneg1
  br i1 %bad, label %ovf, label %go
ovf:
  call void @nx_panic(ptr @.msg.overflow)
  unreachable
go:
  %rem = srem i64 %l, %r
  %rz = icmp eq i64 %rem, 0
  br i1 %rz, label %done, label %fix
fix:
  %lpos = icmp sgt i64 %l, 0
  %rpos = icmp sgt i64 %r, 0
  %opp = xor i1 %lpos, %rpos
  br i1 %opp, label %adj, label %done
adj:
  ; Take the divisor's sign, so the remainder follows the divisor.
  %rneg = icmp slt i64 %r, 0
  %neg = icmp slt i64 %rem, 0
  %wrong = xor i1 %rneg, %neg
  br i1 %wrong, label %shift, label %done
shift:
  %s = add i64 %rem, %r
  ret i64 %s
done:
  ret i64 %rem
}

define double @nx_fpow(double %l, double %r) {
entry:
  %p = call double @llvm.pow.f64(double %l, double %r)
  ret double %p
}

define i64 @nx_ipow(i64 %base, i64 %e) {
entry:
  ; R2: an integer power either fits or saturates; it never wraps. The
  ; boxed path in nx_pow has always enforced that, but the UNBOXED path --
  ; which is the default, since NX_NOUNBOX is normally unset -- called this
  ; function directly and inherited none of it. So 2 ** -1 returned 1 and
  ; 2 ** 100 returned 0.
  %neg = icmp slt i64 %e, 0
  br i1 %neg, label %negexp, label %chkbig
chkbig:
  %big = icmp sgt i64 %e, 62
  br i1 %big, label %sat, label %loop
sat:
  ; 0, 1 and -1 do not saturate: their powers are always exact.
  %iszero = icmp eq i64 %base, 0
  br i1 %iszero, label %zres, label %chkunit
zres:
  ret i64 0
chkunit:
  %mone = icmp eq i64 %base, -1
  br i1 %mone, label %negpar, label %chkone
negpar:
  %par = and i64 %e, 1
  %peven = icmp eq i64 %par, 0
  %sg = select i1 %peven, i64 1, i64 -1
  ret i64 %sg
chkone:
  %one = icmp eq i64 %base, 1
  br i1 %one, label %ores, label %satsign
ores:
  ret i64 1
satsign:
  %aneg = icmp slt i64 %base, 0
  %satv = select i1 %aneg, i64 -9223372036854775808, i64 9223372036854775807
  ret i64 %satv
loop:
  ; Repeated multiplication, not repeated squaring. The exponent is now
  ; known to be at most 62, so the simple loop is correct and fast enough.
  %i = phi i64 [0, %chkbig], [%i2, %body]
  %acc = phi i64 [1, %chkbig], [%acc2, %body]
  %done = icmp sge i64 %i, %e
  br i1 %done, label %exit, label %body
body:
  %m = mul i64 %acc, %base
  %acc2 = add i64 %m, 0
  %i2 = add i64 %i, 1
  br label %loop
exit:
  ret i64 %acc
negexp:
  call void @nx_panic(ptr @.msg.negexp)
  unreachable
}

define %NxVal @nx_mod(%NxVal %l, %NxVal %r) {
entry:
  %lt = extractvalue %NxVal %l, 0
  %rt = extractvalue %NxVal %r, 0
  %both = icmp eq i64 %lt, 1
  %b2 = icmp eq i64 %rt, 1
  %ok = and i1 %both, %b2
  br i1 %ok, label %ints, label %bad
ints:
  %a = extractvalue %NxVal %l, 1
  %b = extractvalue %NxVal %r, 1
  %m = call i64 @nx_mod_i64(i64 %a, i64 %b)
  %res = call %NxVal @nx_int(i64 %m)
  ret %NxVal %res
bad:
  call void @nx_panic(ptr @.msg.type)
  unreachable
}

define %NxVal @nx_floordiv(%NxVal %l, %NxVal %r) {
entry:
  %lt = extractvalue %NxVal %l, 0
  %rt = extractvalue %NxVal %r, 0
  %both = icmp eq i64 %lt, 1
  %b2 = icmp eq i64 %rt, 1
  %ok = and i1 %both, %b2
  br i1 %ok, label %ints, label %bad
ints:
  %a = extractvalue %NxVal %l, 1
  %b = extractvalue %NxVal %r, 1
  %m = call i64 @nx_floordiv_i64(i64 %a, i64 %b)
  %res = call %NxVal @nx_int(i64 %m)
  ret %NxVal %res
bad:
  call void @nx_panic(ptr @.msg.type)
  unreachable
}

define %NxVal @nx_pow(%NxVal %l, %NxVal %r) {
entry:
  %lt = extractvalue %NxVal %l, 0
  %rt = extractvalue %NxVal %r, 0
  %li = icmp eq i64 %lt, 1
  %ri = icmp eq i64 %rt, 1
  %both = and i1 %li, %ri
  br i1 %both, label %ints, label %checkt
ints:
  %a = extractvalue %NxVal %l, 1
  %b = extractvalue %NxVal %r, 1
  %neg = icmp slt i64 %b, 0
  br i1 %neg, label %negexp, label %chkbig
chkbig:
  ; Past 62 the result cannot fit, so saturate rather than wrap. A
  ; saturating answer keeps a runaway loop from silently becoming a
  ; wrong number. The bases whose power is always exactly representable
  ; are answered exactly instead of being clamped.
  %big = icmp sgt i64 %b, 62
  br i1 %big, label %sat, label %small
sat:
  %iszero = icmp eq i64 %a, 0
  br i1 %iszero, label %zres, label %chkunit
zres:
  %rz = call %NxVal @nx_int(i64 0)
  ret %NxVal %rz
chkunit:
  ; (-1)**k is exact for any k, so parity decides the sign.
  %mone = icmp eq i64 %a, -1
  br i1 %mone, label %negparity, label %chkone
negparity:
  %par = and i64 %b, 1
  %pe = icmp eq i64 %par, 0
  %sgn = select i1 %pe, i64 1, i64 -1
  %rp = call %NxVal @nx_int(i64 %sgn)
  ret %NxVal %rp
chkone:
  %one = icmp eq i64 %a, 1
  br i1 %one, label %ores, label %satsign
ores:
  %ro = call %NxVal @nx_int(i64 1)
  ret %NxVal %ro
satsign:
  %aneg = icmp slt i64 %a, 0
  %satv = select i1 %aneg, i64 -9223372036854775808, i64 9223372036854775807
  %r2 = call %NxVal @nx_int(i64 %satv)
  ret %NxVal %r2
small:
  %p = call i64 @nx_ipow(i64 %a, i64 %b)
  %r3 = call %NxVal @nx_int(i64 %p)
  ret %NxVal %r3
negexp:
  call void @nx_panic(ptr @.msg.negexp)
  unreachable
checkt:
  %lf = icmp eq i64 %lt, 2
  %rf = icmp eq i64 %rt, 2
  %anyf = or i1 %lf, %rf
  br i1 %anyf, label %floats, label %bad
floats:
  ; nx_tonum converts per side rather than bitcasting: a mixed
  ; `2 ** 0.5` has an Int on the left, and reading its payload as float
  ; bits would yield a denormal instead of 2.0.
  %av = call double @nx_tonum(%NxVal %l)
  %bv = call double @nx_tonum(%NxVal %r)
  %pf = call double @nx_fpow(double %av, double %bv)
  %r4 = call %NxVal @nx_float(double %pf)
  ret %NxVal %r4
bad:
  call void @nx_panic(ptr @.msg.type)
  unreachable
}

define %NxVal @nx_bitand(%NxVal %l, %NxVal %r) {
entry:
  %a = extractvalue %NxVal %l, 1
  %b = extractvalue %NxVal %r, 1
  %v = and i64 %a, %b
  %res = call %NxVal @nx_int(i64 %v)
  ret %NxVal %res
}

define %NxVal @nx_bitor(%NxVal %l, %NxVal %r) {
entry:
  %a = extractvalue %NxVal %l, 1
  %b = extractvalue %NxVal %r, 1
  %v = or i64 %a, %b
  %res = call %NxVal @nx_int(i64 %v)
  ret %NxVal %res
}

define %NxVal @nx_bitxor(%NxVal %l, %NxVal %r) {
entry:
  %a = extractvalue %NxVal %l, 1
  %b = extractvalue %NxVal %r, 1
  %v = xor i64 %a, %b
  %res = call %NxVal @nx_int(i64 %v)
  ret %NxVal %res
}

define %NxVal @nx_shl(%NxVal %l, %NxVal %r) {
entry:
  %a = extractvalue %NxVal %l, 1
  %b = extractvalue %NxVal %r, 1
  %lo = icmp slt i64 %b, 0
  %hi = icmp sgt i64 %b, 63
  %bad = or i1 %lo, %hi
  br i1 %bad, label %oob, label %ok
ok:
  %v = shl i64 %a, %b
  %res = call %NxVal @nx_int(i64 %v)
  ret %NxVal %res
oob:
  call void @nx_panic(ptr @.msg.shift)
  unreachable
}

define %NxVal @nx_shr(%NxVal %l, %NxVal %r) {
entry:
  %a = extractvalue %NxVal %l, 1
  %b = extractvalue %NxVal %r, 1
  %lo = icmp slt i64 %b, 0
  %hi = icmp sgt i64 %b, 63
  %bad = or i1 %lo, %hi
  br i1 %bad, label %oob, label %ok
ok:
  %v = ashr i64 %a, %b
  %res = call %NxVal @nx_int(i64 %v)
  ret %NxVal %res
oob:
  call void @nx_panic(ptr @.msg.shift)
  unreachable
}

define %NxVal @nx_bitnot(%NxVal %v) {
entry:
  %a = extractvalue %NxVal %v, 1
  %n = xor i64 %a, -1
  %res = call %NxVal @nx_int(i64 %n)
  ret %NxVal %res
}

; --- membership ----------------------------------------------------

define %NxVal @nx_in(%NxVal %needle, %NxVal %hay) {
entry:
  %ht = extractvalue %NxVal %hay, 0
  %isl = icmp eq i64 %ht, 5
  br i1 %isl, label %list, label %c
list:
  %n = extractvalue %NxVal %hay, 2
  br label %scan
scan:
  %i = phi i64 [0, %list], [%i2, %adv]
  %done = icmp sge i64 %i, %n
  br i1 %done, label %no, label %chk
chk:
  %e = call %NxVal @nx_listget(%NxVal %hay, i64 %i)
  %same = call i1 @nx_eqb(%NxVal %e, %NxVal %needle)
  br i1 %same, label %yes, label %adv
adv:
  %i2 = add i64 %i, 1
  br label %scan
c:
  %iss = icmp eq i64 %ht, 4
  br i1 %iss, label %str, label %d
str:
  %found = call %NxVal @nx_strcontains(%NxVal %hay, %NxVal %needle)
  ret %NxVal %found
d:
  %isd = icmp eq i64 %ht, 7
  br i1 %isd, label %dict, label %bad
dict:
  %found2 = call %NxVal @nx_dictcontains(%NxVal %hay, %NxVal %needle)
  ret %NxVal %found2
yes:
  %r = call %NxVal @nx_bool(i1 true)
  ret %NxVal %r
no:
  %r2 = call %NxVal @nx_bool(i1 false)
  ret %NxVal %r2
bad:
  call void @nx_panic(ptr @.msg.type)
  unreachable
}

; Substring search. Byte-wise, matching how strings are stored.
define %NxVal @nx_strcontains(%NxVal %hay, %NxVal %needle) {
entry:
  %t = extractvalue %NxVal %needle, 0
  %isstr = icmp eq i64 %t, 4
  br i1 %isstr, label %prep, label %nope
prep:
  %hn = extractvalue %NxVal %hay, 2
  %nn = extractvalue %NxVal %needle, 2
  ; An empty needle is present in any string, so `"" in s` is true.
  %empty = icmp eq i64 %nn, 0
  br i1 %empty, label %nope, label %chkl
chkl:
  %fits = icmp sgt i64 %hn, %nn
  br i1 %fits, label %init, label %nope
init:
  %hp = extractvalue %NxVal %hay, 1
  %np = extractvalue %NxVal %needle, 1
  %hs = inttoptr i64 %hp to ptr
  %ns = inttoptr i64 %np to ptr
  %last = sub i64 %hn, %nn
  br label %outer
outer:
  %i = phi i64 [0, %init], [%i2, %adv]
  %over = icmp sgt i64 %i, %last
  br i1 %over, label %nope, label %inner
inner:
  %j = phi i64 [0, %outer], [%j2, %icont]
  %jdone = icmp sge i64 %j, %nn
  br i1 %jdone, label %match, label %ichk
ichk:
  %hp1 = getelementptr i8, ptr %hs, i64 %i
  %off = getelementptr i8, ptr %hp1, i64 %j
  %c1 = load i8, ptr %off
  %np1 = getelementptr i8, ptr %ns, i64 %j
  %c2 = load i8, ptr %np1
  %eq = icmp eq i8 %c1, %c2
  br i1 %eq, label %icont, label %adv
icont:
  %j2 = add i64 %j, 1
  br label %inner
adv:
  %i2 = add i64 %i, 1
  br label %outer
match:
  %r = call %NxVal @nx_bool(i1 true)
  ret %NxVal %r
nope:
  %r2 = call %NxVal @nx_bool(i1 false)
  ret %NxVal %r2
}

define %NxVal @nx_dictcontains(%NxVal %d, %NxVal %k) {
entry:
  %n = extractvalue %NxVal %d, 2
  %hp = extractvalue %NxVal %d, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxDict, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  br label %scan
scan:
  %i = phi i64 [0, %entry], [%i2, %adv]
  %done = icmp sge i64 %i, %n
  br i1 %done, label %no, label %chk
chk:
  %ep = getelementptr %NxDictEntry, ptr %data, i64 %i
  %kv = getelementptr %NxDictEntry, ptr %ep, i64 0, i32 0
  %key = load %NxVal, ptr %kv
  %same = call i1 @nx_eqb(%NxVal %key, %NxVal %k)
  br i1 %same, label %yes, label %adv
adv:
  %i2 = add i64 %i, 1
  br label %scan
yes:
  %r = call %NxVal @nx_bool(i1 true)
  ret %NxVal %r
no:
  %r2 = call %NxVal @nx_bool(i1 false)
  ret %NxVal %r2
}

; --- dicts --------------------------------------------------------

define %NxVal @nx_new_dict(i64 %cap) {
entry:
  %c0 = icmp eq i64 %cap, 0
  %cap2 = select i1 %c0, i64 4, i64 %cap
  ; 16, matching sizeof(%NxDict) after the counters became i32. See
  ; nx_new_list: the allocation and the struct have to move together.
  %h = call ptr @malloc(i64 16)
  %bytes = mul i64 %cap2, 48
  %data = call ptr @malloc(i64 %bytes)
  %dp = getelementptr %NxDict, ptr %h, i64 0, i32 0
  store ptr %data, ptr %dp
  %lp = getelementptr %NxDict, ptr %h, i64 0, i32 1
  store i32 0, ptr %lp
  %cp = getelementptr %NxDict, ptr %h, i64 0, i32 2
  %dcap232 = trunc i64 %cap2 to i32
  store i32 %dcap232, ptr %cp
  %hi = ptrtoint ptr %h to i64
  %r0 = insertvalue %NxVal zeroinitializer, i64 7, 0
  %r1 = insertvalue %NxVal %r0, i64 %hi, 1
  %r2 = insertvalue %NxVal %r1, i64 0, 2
  ret %NxVal %r2
}

; Linear probe. Returns true and writes the value to %out on a hit.
define i1 @nx_dictfind(%NxVal %d, %NxVal %k, ptr %out) {
entry:
  %n = extractvalue %NxVal %d, 2
  %hp = extractvalue %NxVal %d, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxDict, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  br label %scan
scan:
  %i = phi i64 [0, %entry], [%i2, %adv]
  %done = icmp sge i64 %i, %n
  br i1 %done, label %miss, label %chk
chk:
  %ep = getelementptr %NxDictEntry, ptr %data, i64 %i
  %kv = getelementptr %NxDictEntry, ptr %ep, i64 0, i32 0
  %key = load %NxVal, ptr %kv
  %same = call i1 @nx_eqb(%NxVal %key, %NxVal %k)
  br i1 %same, label %hit, label %adv
hit:
  %vv = getelementptr %NxDictEntry, ptr %ep, i64 0, i32 1
  %val = load %NxVal, ptr %vv
  store %NxVal %val, ptr %out
  ret i1 true
adv:
  %i2 = add i64 %i, 1
  br label %scan
miss:
  ret i1 false
}

define i1 @nx_dictfindidx(%NxVal %d, %NxVal %k, ptr %outidx) {
entry:
  %n = extractvalue %NxVal %d, 2
  %hp = extractvalue %NxVal %d, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxDict, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  br label %scan
scan:
  %i = phi i64 [0, %entry], [%i2, %adv]
  %done = icmp sge i64 %i, %n
  br i1 %done, label %miss, label %chk
chk:
  %ep = getelementptr %NxDictEntry, ptr %data, i64 %i
  %kv = getelementptr %NxDictEntry, ptr %ep, i64 0, i32 0
  %key = load %NxVal, ptr %kv
  %same = call i1 @nx_eqb(%NxVal %key, %NxVal %k)
  br i1 %same, label %hit, label %adv
hit:
  store i64 %i, ptr %outidx
  ret i1 true
adv:
  %i2 = add i64 %i, 1
  br label %scan
miss:
  ret i1 false
}

; `vp` points at the dict value and is updated in place. An existing key
; is overwritten where it sits, so its position in iteration order does
; not move -- that stability is what makes repeated assignment
; reproducible run to run.
define void @nx_dictset(ptr %vp, %NxVal %k, %NxVal %v) {
entry:
  %idx = alloca i64
  %dv = load %NxVal, ptr %vp
  %hit = call i1 @nx_dictfindidx(%NxVal %dv, %NxVal %k, ptr %idx)
  br i1 %hit, label %overwrite, label %append
overwrite:
  %i = load i64, ptr %idx
  %hp = extractvalue %NxVal %dv, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxDict, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  %ep = getelementptr %NxDictEntry, ptr %data, i64 %i
  %vv = getelementptr %NxDictEntry, ptr %ep, i64 0, i32 1
  store %NxVal %v, ptr %vv
  ret void
append:
  %hp2 = extractvalue %NxVal %dv, 1
  %h2 = inttoptr i64 %hp2 to ptr
  %lp = getelementptr %NxDict, ptr %h2, i64 0, i32 1
  %len32 = load i32, ptr %lp
  %len = zext i32 %len32 to i64
  %cp = getelementptr %NxDict, ptr %h2, i64 0, i32 2
  %cap32 = load i32, ptr %cp
  %cap = zext i32 %cap32 to i64
  %full = icmp eq i64 %len, %cap
  br i1 %full, label %grow, label %put
grow:
  %ncap = mul i64 %cap, 2
  %dp2 = getelementptr %NxDict, ptr %h2, i64 0, i32 0
  %dold = load ptr, ptr %dp2
  %nb = mul i64 %ncap, 48
  ; Same saturation guard as the list: a dict entry is 48 bytes, so 2^31 of
  ; them is 96 GiB of live data in a runtime with no collector.
  %ncapfits = icmp ule i64 %ncap, 2147483647
  br i1 %ncapfits, label %realloc, label %toobig
toobig:
  call void @nx_panic(ptr @.msg.toomany)
  br label %realloc
realloc:
  %nd = call ptr @realloc(ptr %dold, i64 %nb)
  store ptr %nd, ptr %dp2
  %ncap32 = trunc i64 %ncap to i32
  store i32 %ncap32, ptr %cp
  br label %put
put:
  %dp3 = getelementptr %NxDict, ptr %h2, i64 0, i32 0
  %data3 = load ptr, ptr %dp3
  %ep3 = getelementptr %NxDictEntry, ptr %data3, i64 %len
  %kv3 = getelementptr %NxDictEntry, ptr %ep3, i64 0, i32 0
  store %NxVal %k, ptr %kv3
  %vv3 = getelementptr %NxDictEntry, ptr %ep3, i64 0, i32 1
  store %NxVal %v, ptr %vv3
  %len2 = add i64 %len, 1
  %len232 = trunc i64 %len2 to i32
  store i32 %len232, ptr %lp
  ; The mirrored length keeps the tag-7 `b` field in step with the
  ; header, which is what iteration and len() read.
  %r1 = insertvalue %NxVal zeroinitializer, i64 7, 0
  %r2 = insertvalue %NxVal %r1, i64 %hp2, 1
  %r3 = insertvalue %NxVal %r2, i64 %len2, 2
  store %NxVal %r3, ptr %vp
  ret void
}

define %NxVal @nx_dictget(%NxVal %d, %NxVal %k) {
entry:
  %out = alloca %NxVal
  %found = call i1 @nx_dictfind(%NxVal %d, %NxVal %k, ptr %out)
  br i1 %found, label %yes, label %no
yes:
  %v = load %NxVal, ptr %out
  ret %NxVal %v
no:
  call void @nx_panic(ptr @.msg.nokey)
  unreachable
}

define %NxVal @nx_dictgetor(%NxVal %d, %NxVal %k, %NxVal %dflt) {
entry:
  %out = alloca %NxVal
  %found = call i1 @nx_dictfind(%NxVal %d, %NxVal %k, ptr %out)
  br i1 %found, label %yes, label %no
yes:
  %v = load %NxVal, ptr %out
  ret %NxVal %v
no:
  ret %NxVal %dflt
}

define i64 @nx_dictlen(%NxVal %d) {
entry:
  %n = extractvalue %NxVal %d, 2
  ret i64 %n
}

define %NxVal @nx_dictkeys(%NxVal %d) {
entry:
  %slot = alloca %NxVal
  %n = extractvalue %NxVal %d, 2
  %out = call %NxVal @nx_new_list(i64 %n)
  store %NxVal %out, ptr %slot
  %hp = extractvalue %NxVal %d, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxDict, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  br label %scan
scan:
  %i = phi i64 [0, %entry], [%i2, %body]
  %done = icmp sge i64 %i, %n
  br i1 %done, label %exit, label %body
body:
  %ep = getelementptr %NxDictEntry, ptr %data, i64 %i
  %kv = getelementptr %NxDictEntry, ptr %ep, i64 0, i32 0
  %key = load %NxVal, ptr %kv
  call void @nx_listpush(ptr %slot, %NxVal %key)
  %i2 = add i64 %i, 1
  br label %scan
exit:
  %r = load %NxVal, ptr %slot
  ret %NxVal %r
}

define %NxVal @nx_dictvals(%NxVal %d) {
entry:
  %slot = alloca %NxVal
  %n = extractvalue %NxVal %d, 2
  %out = call %NxVal @nx_new_list(i64 %n)
  store %NxVal %out, ptr %slot
  %hp = extractvalue %NxVal %d, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxDict, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  br label %scan
scan:
  %i = phi i64 [0, %entry], [%i2, %body]
  %done = icmp sge i64 %i, %n
  br i1 %done, label %exit, label %body
body:
  %ep = getelementptr %NxDictEntry, ptr %data, i64 %i
  %vv = getelementptr %NxDictEntry, ptr %ep, i64 0, i32 1
  %val = load %NxVal, ptr %vv
  call void @nx_listpush(ptr %slot, %NxVal %val)
  %i2 = add i64 %i, 1
  br label %scan
exit:
  %r = load %NxVal, ptr %slot
  ret %NxVal %r
}

; Each item is a two-element list [key, value], which is what the
; interpreter produces too, so `for pair in d.items()` behaves the same
; on both paths.
define %NxVal @nx_dictitems(%NxVal %d) {
entry:
  %slot = alloca %NxVal
  %pslot = alloca %NxVal
  %n = extractvalue %NxVal %d, 2
  %out = call %NxVal @nx_new_list(i64 %n)
  store %NxVal %out, ptr %slot
  %hp = extractvalue %NxVal %d, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxDict, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  br label %scan
scan:
  %i = phi i64 [0, %entry], [%i2, %body]
  %done = icmp sge i64 %i, %n
  br i1 %done, label %exit, label %body
body:
  %ep = getelementptr %NxDictEntry, ptr %data, i64 %i
  %kv = getelementptr %NxDictEntry, ptr %ep, i64 0, i32 0
  %vv = getelementptr %NxDictEntry, ptr %ep, i64 0, i32 1
  %key = load %NxVal, ptr %kv
  %val = load %NxVal, ptr %vv
  %pair = call %NxVal @nx_new_list(i64 2)
  store %NxVal %pair, ptr %pslot
  call void @nx_listpush(ptr %pslot, %NxVal %key)
  call void @nx_listpush(ptr %pslot, %NxVal %val)
  %pdone = load %NxVal, ptr %pslot
  call void @nx_listpush(ptr %slot, %NxVal %pdone)
  %i2 = add i64 %i, 1
  br label %scan
exit:
  %r = load %NxVal, ptr %slot
  ret %NxVal %r
}

; Removes a key by shifting the tail down, so order stays stable.
define %NxVal @nx_dictdel(%NxVal %d, %NxVal %k) {
entry:
  %idx = alloca i64
  %found = call i1 @nx_dictfindidx(%NxVal %d, %NxVal %k, ptr %idx)
  br i1 %found, label %rm, label %ret
ret:
  ret %NxVal %d
rm:
  %i = load i64, ptr %idx
  %n = extractvalue %NxVal %d, 2
  %hp = extractvalue %NxVal %d, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxDict, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  %last = sub i64 %n, 1
  br label %scan
scan:
  %j = phi i64 [%i, %rm], [%j2, %body]
  %done = icmp sge i64 %j, %last
  br i1 %done, label %shrink, label %body
body:
  %src = add i64 %j, 1
  %se = getelementptr %NxDictEntry, ptr %data, i64 %src
  %sk = getelementptr %NxDictEntry, ptr %se, i64 0, i32 0
  %sv = getelementptr %NxDictEntry, ptr %se, i64 0, i32 1
  %skv = load %NxVal, ptr %sk
  %svv = load %NxVal, ptr %sv
  %de = getelementptr %NxDictEntry, ptr %data, i64 %j
  %dk = getelementptr %NxDictEntry, ptr %de, i64 0, i32 0
  store %NxVal %skv, ptr %dk
  %dv = getelementptr %NxDictEntry, ptr %de, i64 0, i32 1
  store %NxVal %svv, ptr %dv
  %j2 = add i64 %j, 1
  br label %scan
shrink:
  %lp = getelementptr %NxDict, ptr %h, i64 0, i32 1
  %n2 = sub i64 %n, 1
  %n232 = trunc i64 %n2 to i32
  store i32 %n232, ptr %lp
  %r1 = insertvalue %NxVal zeroinitializer, i64 7, 0
  %r2 = insertvalue %NxVal %r1, i64 %hp, 1
  %r3 = insertvalue %NxVal %r2, i64 %n2, 2
  ret %NxVal %r3
}

; --- slicing ------------------------------------------------------
; Copies rather than aliases. A view would make later mutation of either
; side surprising, and copying keeps the one-owner value model intact.

define %NxVal @nx_slice(%NxVal %b, i64 %from, i64 %to, i64 %step) {
entry:
  ; Hoisted into the entry block: an alloca inside the clamp block would
  ; allocate afresh on every call that reached it.
  %slot = alloca %NxVal
  %badstep = icmp sle i64 %step, 0
  br i1 %badstep, label %bads, label %checktag
bads:
  call void @nx_panic(ptr @.msg.step)
  unreachable
checktag:
  %t = extractvalue %NxVal %b, 0
  %isl = icmp eq i64 %t, 5
  br i1 %isl, label %clamp, label %strpath
clamp:
  %n = extractvalue %NxVal %b, 2
  %hp = extractvalue %NxVal %b, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxList, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  ; from: negative counts from the end, then clamped into [0, n].
  %fneg = icmp slt i64 %from, 0
  %fadj = add i64 %from, %n
  %f0 = select i1 %fneg, i64 %fadj, i64 %from
  %fhi = icmp sgt i64 %f0, %n
  %f1 = select i1 %fhi, i64 %n, i64 %f0
  %flow = icmp slt i64 %f1, 0
  %f2 = select i1 %flow, i64 0, i64 %f1
  %tneg = icmp slt i64 %to, 0
  %tadj = add i64 %to, %n
  %t0 = select i1 %tneg, i64 %tadj, i64 %to
  %thi = icmp sgt i64 %t0, %n
  %t1 = select i1 %thi, i64 %n, i64 %t0
  %tlow = icmp slt i64 %t1, 0
  %t2 = select i1 %tlow, i64 0, i64 %t1
  %cap = sub i64 %t2, %f2
  %cnt = sdiv i64 %cap, %step
  %out = call %NxVal @nx_new_list(i64 %cnt)
  store %NxVal %out, ptr %slot
  br label %scan
scan:
  %i = phi i64 [%f2, %clamp], [%i2, %body]
  %done = icmp sge i64 %i, %t2
  br i1 %done, label %exit, label %body
body:
  %ep = getelementptr %NxVal, ptr %data, i64 %i
  %v = load %NxVal, ptr %ep
  ; Deep-copy container elements. A slice yields a new list, so a list of
  ; lists whose elements were shared would let `ys = xs[0:2]; ys[0][0] = 9`
  ; write through into `xs`. A plain bind already deep-copies, so the
  ; shallow version made one expression mean two different things depending
  ; only on whether a slice appeared.
  ;
  ; nx_clone returns Int, Float, Bool, Str and Func unchanged and deep-copies
  ; List, Dict and Record, so one call covers both cases. The tag test is
  ; left inside it rather than duplicated here: a scalar slice pays a call
  ; it does not need, but a branch here would need its own blocks, and the
  ; extra predecessor would have to be threaded through the `scan` phi.
  %cv = call %NxVal @nx_clone(%NxVal %v)
  call void @nx_listpush(ptr %slot, %NxVal %cv)
  %i2 = add i64 %i, %step
  br label %scan
exit:
  %r = load %NxVal, ptr %slot
  ret %NxVal %r
strpath:
  %iss = icmp eq i64 %t, 4
  br i1 %iss, label %str, label %bad
str:
  %n2 = extractvalue %NxVal %b, 2
  %sp = extractvalue %NxVal %b, 1
  %s = inttoptr i64 %sp to ptr
  %fneg2 = icmp slt i64 %from, 0
  %fadj2 = add i64 %from, %n2
  %f02 = select i1 %fneg2, i64 %fadj2, i64 %from
  %fhi2 = icmp sgt i64 %f02, %n2
  %f12 = select i1 %fhi2, i64 %n2, i64 %f02
  %flow2 = icmp slt i64 %f12, 0
  %f22 = select i1 %flow2, i64 0, i64 %f12
  %tneg2 = icmp slt i64 %to, 0
  %tadj2 = add i64 %to, %n2
  %t02 = select i1 %tneg2, i64 %tadj2, i64 %to
  %thi2 = icmp sgt i64 %t02, %n2
  %t12 = select i1 %thi2, i64 %n2, i64 %t02
  %tlow2 = icmp slt i64 %t12, 0
  %t22 = select i1 %tlow2, i64 0, i64 %t12
  %cap2 = sub i64 %t22, %f22
  ; The element count is ceil(cap/step). floor(cap/step) agrees with it when
  ; cap divides evenly and is one short otherwise: over 6 bytes with step 2
  ; both give 3, but over 5 bytes the answer is 3 and floor says 2. That
  ; shortfall was a heap buffer overflow.
  %sp1 = add i64 %cap2, %step
  %sp2 = sub i64 %sp1, 1
  %cnt2 = sdiv i64 %sp2, %step
  %buf = call ptr @malloc(i64 %cnt2)
  br label %sscan
sscan:
  %si = phi i64 [%f22, %str], [%si2, %sbody]
  %so = phi i64 [0, %str], [%so2, %sbody]
  %sdone = icmp sge i64 %si, %t22
  br i1 %sdone, label %sout, label %sbody
sbody:
  %srcp = getelementptr i8, ptr %s, i64 %si
  %c = load i8, ptr %srcp
  ; The output offset advances by ONE per element. Mirroring the source
  ; offset (si - f22) advanced it by step instead, so a step of 2 wrote at
  ; 0, 2, 4... leaving every odd byte uninitialised and running off the end
  ; of a buffer sized for the packed result. That is why slicing a list
  ; worked (it reserves then grows) and slicing a string did not.
  %dstp = getelementptr i8, ptr %buf, i64 %so
  store i8 %c, ptr %dstp
  %si2 = add i64 %si, %step
  %so2 = add i64 %so, 1
  br label %sscan
sout:
  %sv = call %NxVal @nx_str(ptr %buf, i64 %cnt2)
  ret %NxVal %sv
bad:
  call void @nx_panic(ptr @.msg.type)
  unreachable
}

; --- list removal --------------------------------------------------

define %NxVal @nx_listdel(%NxVal %l, i64 %i) {
entry:
  %slot = alloca %NxVal
  %n = extractvalue %NxVal %l, 2
  %neg = icmp slt i64 %i, 0
  %adj = add i64 %i, %n
  %pos = select i1 %neg, i64 %adj, i64 %i
  %oob1 = icmp slt i64 %pos, 0
  %oob2 = icmp sge i64 %pos, %n
  %oob = or i1 %oob1, %oob2
  br i1 %oob, label %bad, label %go
bad:
  call void @nx_panic_idx(i64 %i, i64 %n)
  unreachable
go:
  %hp = extractvalue %NxVal %l, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxList, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  %out = call %NxVal @nx_new_list(i64 %n)
  store %NxVal %out, ptr %slot
  br label %scan
scan:
  %j = phi i64 [0, %go], [%j2, %adv]
  %done = icmp sge i64 %j, %n
  br i1 %done, label %exit, label %body
body:
  %skip = icmp eq i64 %j, %pos
  br i1 %skip, label %adv, label %keep
keep:
  %ep = getelementptr %NxVal, ptr %data, i64 %j
  %v = load %NxVal, ptr %ep
  call void @nx_listpush(ptr %slot, %NxVal %v)
  br label %adv
adv:
  %j2 = add i64 %j, 1
  br label %scan
exit:
  %r = load %NxVal, ptr %slot
  ret %NxVal %r
}

; --- assertion ----------------------------------------------------

define void @nx_assert_fail() {
entry:
  call void @nx_panic(ptr @.msg.assert)
  unreachable
}

define void @nx_assert_fail_msg(%NxVal %msg) {
entry:
  call void @nx_print_val(%NxVal %msg)
  call i32 (ptr, ...) @printf(ptr @.fmt.colonsp)
  call void @nx_panic(ptr @.msg.assert)
  unreachable
}

; --- in-place element write ---------------------------------------
; Updates the element in the list's own storage rather than building a
; replacement, so an alias of the list sees the change. A grow never
; happens here, which is why the element pointer stays valid.

define void @nx_listset(%NxVal %l, i64 %i, %NxVal %v) {
entry:
  %n = extractvalue %NxVal %l, 2
  %neg = icmp slt i64 %i, 0
  %adj = add i64 %i, %n
  %pos = select i1 %neg, i64 %adj, i64 %i
  %oob1 = icmp slt i64 %pos, 0
  %oob2 = icmp sge i64 %pos, %n
  %oob = or i1 %oob1, %oob2
  br i1 %oob, label %bad, label %go
bad:
  call void @nx_panic_idx(i64 %i, i64 %n)
  unreachable
go:
  %hp = extractvalue %NxVal %l, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxList, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  %ep = getelementptr %NxVal, ptr %data, i64 %pos
  store %NxVal %v, ptr %ep
  ret void
}

define void @nx_dictset_at(%NxVal %l, i64 %i, %NxVal %v) {
entry:
  call void @nx_listset(%NxVal %l, i64 %i, %NxVal %v)
  ret void
}

; Key at a positional index, so `for k in d` walks the dict in insertion
; order without first materialising the whole key list.
define %NxVal @nx_dictkeyat(%NxVal %d, i64 %i) {
entry:
  %n = extractvalue %NxVal %d, 2
  %oob1 = icmp slt i64 %i, 0
  %oob2 = icmp sge i64 %i, %n
  %oob = or i1 %oob1, %oob2
  br i1 %oob, label %bad, label %go
bad:
  call void @nx_panic_idx(i64 %i, i64 %n)
  unreachable
go:
  %hp = extractvalue %NxVal %d, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxDict, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  %ep = getelementptr %NxDictEntry, ptr %data, i64 %i
  %kv = getelementptr %NxDictEntry, ptr %ep, i64 0, i32 0
  %key = load %NxVal, ptr %kv
  ret %NxVal %key
}
; --- records -------------------------------------------------------
; Tag 8 Record: a = header ptr, b = type index. The header holds the
; field array plus a pointer to a static type descriptor, which is what
; lets the dynamic path resolve a field name.
;
; Fields are boxed %NxVal. Unboxing scalar fields is a separate pass: the
; layout would have to become type-directed, and the value model here is
; deliberately uniform so the dynamic path stays simple.
;
; Type descriptors are emitted by the backend as globals; the runtime only
; reads them. %NxDesc = { i64 nfields, ptr names } and names points at
; { i64 len, ptr bytes } entries, in declaration order.

; Emitted by the backend, one per declared type.

define %NxVal @nx_new_record(i64 %nfields, ptr %desc) {
entry:
  %bytes = mul i64 %nfields, 24
  %fields = call ptr @malloc(i64 %bytes)
  br label %fill
fill:
  ; Zero the fields first so a partially built record is never read: None
  ; is tag 0, which is what every consumer already handles.
  %i = phi i64 [0, %entry], [%i2, %body]
  %done = icmp sge i64 %i, %nfields
  br i1 %done, label %build, label %body
body:
  %ep = getelementptr %NxVal, ptr %fields, i64 %i
  %zero = insertvalue %NxVal zeroinitializer, i64 0, 0
  store %NxVal %zero, ptr %ep
  %i2 = add i64 %i, 1
  br label %fill
build:
  %h = call ptr @malloc(i64 24)
  %hp0 = getelementptr %NxRec, ptr %h, i64 0, i32 0
  store ptr %fields, ptr %hp0
  %hp1 = getelementptr %NxRec, ptr %h, i64 0, i32 1
  store i64 %nfields, ptr %hp1
  %hp2 = getelementptr %NxRec, ptr %h, i64 0, i32 2
  store ptr %desc, ptr %hp2
  %hi = ptrtoint ptr %h to i64
  %r0 = insertvalue %NxVal zeroinitializer, i64 8, 0
  %r1 = insertvalue %NxVal %r0, i64 %hi, 1
  %r2 = insertvalue %NxVal %r1, i64 %nfields, 2
  ret %NxVal %r2
}

define i1 @nx_is_record(%NxVal %v) {
entry:
  %t = extractvalue %NxVal %v, 0
  %r = icmp eq i64 %t, 8
  ret i1 %r
}

define i64 @nx_rec_nfields(%NxVal %v) {
entry:
  %n = extractvalue %NxVal %v, 2
  ret i64 %n
}

define ptr @nx_rec_desc(%NxVal %v) {
entry:
  %hp = extractvalue %NxVal %v, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxRec, ptr %h, i64 0, i32 2
  %d = load ptr, ptr %dp
  ret ptr %d
}

define ptr @nx_rec_fields(%NxVal %v) {
entry:
  %hp = extractvalue %NxVal %v, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxRec, ptr %h, i64 0, i32 0
  %f = load ptr, ptr %dp
  ret ptr %f
}

; Field at a constant offset. The caller has already checked the index is
; within the record, so there is no bounds test here.
define %NxVal @nx_rec_get(%NxVal %v, i64 %i) {
entry:
  %f = call ptr @nx_rec_fields(%NxVal %v)
  %ep = getelementptr %NxVal, ptr %f, i64 %i
  %e = load %NxVal, ptr %ep
  ret %NxVal %e
}

define void @nx_rec_set(%NxVal %v, i64 %i, %NxVal %val) {
entry:
  %f = call ptr @nx_rec_fields(%NxVal %v)
  %ep = getelementptr %NxVal, ptr %f, i64 %i
  store %NxVal %val, ptr %ep
  ret void
}

; Field by name, for the dynamic path where the type is not known. An
; unknown name is an error rather than a silent None.
define %NxVal @nx_rec_getn(%NxVal %v, ptr %name, i64 %nlen) {
entry:
  %n = call i64 @nx_rec_nfields(%NxVal %v)
  %d = call ptr @nx_rec_desc(%NxVal %v)
  %dp = getelementptr %NxDesc, ptr %d, i64 0, i32 3
  %names = load ptr, ptr %dp
  br label %scan
scan:
  %i = phi i64 [0, %entry], [%i2, %adv]
  %done = icmp sge i64 %i, %n
  br i1 %done, label %bad, label %chk
chk:
  %np = getelementptr %NxRecName, ptr %names, i64 %i
  %lp = getelementptr %NxRecName, ptr %np, i64 0, i32 0
  %ll = load i64, ptr %lp
  %same = icmp eq i64 %ll, %nlen
  br i1 %same, label %cmp, label %adv
cmp:
  %bp = getelementptr %NxRecName, ptr %np, i64 0, i32 1
  %bb = load ptr, ptr %bp
  %c1 = call i32 @memcmp(ptr %name, ptr %bb, i64 %nlen)
  %eq = icmp eq i32 %c1, 0
  br i1 %eq, label %hit, label %adv
hit:
  %r = call %NxVal @nx_rec_get(%NxVal %v, i64 %i)
  ret %NxVal %r
adv:
  %i2 = add i64 %i, 1
  br label %scan
bad:
  call void @nx_panic(ptr @.msg.nofield)
  unreachable
}

define i1 @nx_rec_hasn(%NxVal %v, ptr %name, i64 %nlen) {
entry:
  %n = call i64 @nx_rec_nfields(%NxVal %v)
  %d = call ptr @nx_rec_desc(%NxVal %v)
  %dp = getelementptr %NxDesc, ptr %d, i64 0, i32 3
  %names = load ptr, ptr %dp
  br label %scan
scan:
  %i = phi i64 [0, %entry], [%i2, %adv]
  %done = icmp sge i64 %i, %n
  br i1 %done, label %no, label %chk
chk:
  %np = getelementptr %NxRecName, ptr %names, i64 %i
  %lp = getelementptr %NxRecName, ptr %np, i64 0, i32 0
  %ll = load i64, ptr %lp
  %same = icmp eq i64 %ll, %nlen
  br i1 %same, label %cmp, label %adv
cmp:
  %bp = getelementptr %NxRecName, ptr %np, i64 0, i32 1
  %bb = load ptr, ptr %bp
  %c1 = call i32 @memcmp(ptr %name, ptr %bb, i64 %nlen)
  %eq = icmp eq i32 %c1, 0
  br i1 %eq, label %yes, label %adv
adv:
  %i2 = add i64 %i, 1
  br label %scan
yes:
  ret i1 true
no:
  ret i1 false
}

; Field write by name, for the dynamic path. Mirrors `nx_rec_getn`: an
; unknown name is an error rather than a silent extension, because a
; record's arity is fixed by its declaration.
define void @nx_rec_setn(%NxVal %v, ptr %name, i64 %nlen, %NxVal %val) {
entry:
  %n = call i64 @nx_rec_nfields(%NxVal %v)
  %d = call ptr @nx_rec_desc(%NxVal %v)
  %dp = getelementptr %NxDesc, ptr %d, i64 0, i32 3
  %names = load ptr, ptr %dp
  br label %scan
scan:
  %i = phi i64 [0, %entry], [%i2, %adv]
  %done = icmp sge i64 %i, %n
  br i1 %done, label %bad, label %chk
chk:
  %np = getelementptr %NxRecName, ptr %names, i64 %i
  %lp = getelementptr %NxRecName, ptr %np, i64 0, i32 0
  %ll = load i64, ptr %lp
  %same = icmp eq i64 %ll, %nlen
  br i1 %same, label %cmp, label %adv
cmp:
  %bp = getelementptr %NxRecName, ptr %np, i64 0, i32 1
  %bb = load ptr, ptr %bp
  %c1 = call i32 @memcmp(ptr %name, ptr %bb, i64 %nlen)
  %eq = icmp eq i32 %c1, 0
  br i1 %eq, label %hit, label %adv
hit:
  call void @nx_rec_set(%NxVal %v, i64 %i, %NxVal %val)
  ret void
adv:
  %i2 = add i64 %i, 1
  br label %scan
bad:
  call void @nx_panic(ptr @.msg.nofield)
  unreachable
}

; Structural record equality: same type name, then field by field. The
; name comparison is by bytes rather than descriptor identity, so two
; structurally identical declarations agree even across modules -- the
; same rule the interpreter follows.
define i1 @nx_receq(%NxVal %l, %NxVal %r) {
entry:
  %dl = call ptr @nx_rec_desc(%NxVal %l)
  %dr = call ptr @nx_rec_desc(%NxVal %r)
  %nl = extractvalue %NxVal %l, 2
  %nr = extractvalue %NxVal %r, 2
  %samen = icmp eq i64 %nl, %nr
  br i1 %samen, label %cmpname, label %no
cmpname:
  %tp = getelementptr %NxDesc, ptr %dl, i64 0, i32 0
  %tn = load ptr, ptr %tp
  %lp = getelementptr %NxDesc, ptr %dl, i64 0, i32 1
  %ll = load i64, ptr %lp
  %up = getelementptr %NxDesc, ptr %dr, i64 0, i32 0
  %un = load ptr, ptr %up
  %vp = getelementptr %NxDesc, ptr %dr, i64 0, i32 1
  %vl = load i64, ptr %vp
  %samel = icmp eq i64 %ll, %vl
  br i1 %samel, label %cmpbytes, label %no
cmpbytes:
  %c1 = call i32 @memcmp(ptr %tn, ptr %un, i64 %ll)
  %eq = icmp eq i32 %c1, 0
  br i1 %eq, label %scan, label %no
scan:
  %i = phi i64 [0, %cmpbytes], [%i2, %adv]
  %done = icmp sge i64 %i, %nl
  br i1 %done, label %yes, label %chk
chk:
  %el = call %NxVal @nx_rec_get(%NxVal %l, i64 %i)
  %er = call %NxVal @nx_rec_get(%NxVal %r, i64 %i)
  %same = call i1 @nx_eqb(%NxVal %el, %NxVal %er)
  br i1 %same, label %adv, label %no
adv:
  %i2 = add i64 %i, 1
  br label %scan
yes:
  ret i1 true
no:
  ret i1 false
}

; Structural dict equality: same size, and every key of the left dict
; present with an equal value on the right. Order-insensitive, matching
; the interpreter: `{a: 1, b: 2}` equals `{b: 2, a: 1}`.
define i1 @nx_dicteq(%NxVal %l, %NxVal %r) {
entry:
  ; Hoisted into the entry block: an alloca in the scan loop would
  ; allocate afresh on every iteration.
  %out = alloca %NxVal
  %nl = extractvalue %NxVal %l, 2
  %nr = extractvalue %NxVal %r, 2
  %samen = icmp eq i64 %nl, %nr
  br i1 %samen, label %scan, label %no
scan:
  %i = phi i64 [0, %entry], [%i2, %adv]
  %done = icmp sge i64 %i, %nl
  br i1 %done, label %yes, label %chk
chk:
  %kl = call %NxVal @nx_dictkeyat(%NxVal %l, i64 %i)
  %found = call i1 @nx_dictfind(%NxVal %r, %NxVal %kl, ptr %out)
  br i1 %found, label %cmpv, label %no
cmpv:
  %vl = call %NxVal @nx_dictget(%NxVal %l, %NxVal %kl)
  %vr = load %NxVal, ptr %out
  %same = call i1 @nx_eqb(%NxVal %vl, %NxVal %vr)
  br i1 %same, label %adv, label %no
adv:
  %i2 = add i64 %i, 1
  br label %scan
yes:
  ret i1 true
no:
  ret i1 false
}
; --- value semantics -------------------------------------------------
; NX copies containers on bind: `ys = xs` leaves `ys` independent, and a
; function argument never aliases the caller's value. The interpreter has
; always behaved this way (every bind clones); the native path matches it
; through `nx_clone`, called at every store of a non-scalar.
;
; Strings are shared, not copied: nothing mutates a string in place, so
; sharing is observably identical to copying. Only List, Dict and Record
; (tags 5, 7, 8) duplicate storage.

define %NxVal @nx_clone(%NxVal %v) {
entry:
  %t = extractvalue %NxVal %v, 0
  %isl = icmp eq i64 %t, 5
  br i1 %isl, label %list, label %c1
list:
  %lc = call %NxVal @nx_listclone(%NxVal %v)
  ret %NxVal %lc
c1:
  %isd = icmp eq i64 %t, 7
  br i1 %isd, label %dict, label %c2
dict:
  %dc = call %NxVal @nx_dictclone(%NxVal %v)
  ret %NxVal %dc
c2:
  %isr = icmp eq i64 %t, 8
  br i1 %isr, label %rec, label %same
rec:
  %rc = call %NxVal @nx_recclone(%NxVal %v)
  ret %NxVal %rc
same:
  ret %NxVal %v
}

define %NxVal @nx_listclone(%NxVal %l) {
entry:
  %slot = alloca %NxVal
  %n = extractvalue %NxVal %l, 2
  %out = call %NxVal @nx_new_list(i64 %n)
  store %NxVal %out, ptr %slot
  %hp = extractvalue %NxVal %l, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxList, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  br label %scan
scan:
  %i = phi i64 [0, %entry], [%i2, %body]
  %done = icmp sge i64 %i, %n
  br i1 %done, label %exit, label %body
body:
  %ep = getelementptr %NxVal, ptr %data, i64 %i
  %e = load %NxVal, ptr %ep
  %ec = call %NxVal @nx_clone(%NxVal %e)
  call void @nx_listpush(ptr %slot, %NxVal %ec)
  %i2 = add i64 %i, 1
  br label %scan
exit:
  %r = load %NxVal, ptr %slot
  ret %NxVal %r
}

define %NxVal @nx_dictclone(%NxVal %d) {
entry:
  %slot = alloca %NxVal
  %n = extractvalue %NxVal %d, 2
  %out = call %NxVal @nx_new_dict(i64 %n)
  store %NxVal %out, ptr %slot
  %hp = extractvalue %NxVal %d, 1
  %h = inttoptr i64 %hp to ptr
  %dp = getelementptr %NxDict, ptr %h, i64 0, i32 0
  %data = load ptr, ptr %dp
  br label %scan
scan:
  %i = phi i64 [0, %entry], [%i2, %body]
  %done = icmp sge i64 %i, %n
  br i1 %done, label %exit, label %body
body:
  %ep = getelementptr %NxDictEntry, ptr %data, i64 %i
  %kv = getelementptr %NxDictEntry, ptr %ep, i64 0, i32 0
  %vv = getelementptr %NxDictEntry, ptr %ep, i64 0, i32 1
  %key = load %NxVal, ptr %kv
  %val = load %NxVal, ptr %vv
  %kc = call %NxVal @nx_clone(%NxVal %key)
  %vc = call %NxVal @nx_clone(%NxVal %val)
  call void @nx_dictset(ptr %slot, %NxVal %kc, %NxVal %vc)
  %i2 = add i64 %i, 1
  br label %scan
exit:
  %r = load %NxVal, ptr %slot
  ret %NxVal %r
}

define %NxVal @nx_recclone(%NxVal %v) {
entry:
  %slot = alloca %NxVal
  %n = call i64 @nx_rec_nfields(%NxVal %v)
  %d = call ptr @nx_rec_desc(%NxVal %v)
  %out = call %NxVal @nx_new_record(i64 %n, ptr %d)
  store %NxVal %out, ptr %slot
  br label %scan
scan:
  %i = phi i64 [0, %entry], [%i2, %body]
  %done = icmp sge i64 %i, %n
  br i1 %done, label %exit, label %body
body:
  %cur = load %NxVal, ptr %slot
  %e = call %NxVal @nx_rec_get(%NxVal %v, i64 %i)
  %ec = call %NxVal @nx_clone(%NxVal %e)
  ; `nx_rec_set` writes through the header, so the slot only needs
  ; reloading at the end; the header itself never moves.
  call void @nx_rec_set(%NxVal %cur, i64 %i, %NxVal %ec)
  %i2 = add i64 %i, 1
  br label %scan
exit:
  %r = load %NxVal, ptr %slot
  ret %NxVal %r
}

; --- dynamic container dispatch --------------------------------------
; When the static type is Unknown, the tag decides. Each of these is the
; dynamic counterpart of a statically-dispatched operation above; a tag
; that makes no sense for the operation panics rather than corrupting
; memory, which is what makes an unresolved type safe to carry.

; `a[k] = v` for an unresolved base: a list takes an Int position, a
; dict takes any scalar key. Anything else is a deferred type error.
; Returns the (possibly length-updated) container, so the caller can
; write it back -- a dict may have grown, which moves its mirrored
; length the same way `nx_dictset` maintains it.
define %NxVal @nx_storeindex(%NxVal %b, %NxVal %k, %NxVal %v) {
entry:
  ; Hoisted: this alloca serves the dict arm only, but placing it here
  ; keeps the one-alloca-per-function invariant the hoisting test checks.
  %slot = alloca %NxVal
  %t = extractvalue %NxVal %b, 0
  %isl = icmp eq i64 %t, 5
  br i1 %isl, label %list, label %c
list:
  %kt = extractvalue %NxVal %k, 0
  %ki = icmp eq i64 %kt, 1
  br i1 %ki, label %pos, label %bad
pos:
  %i = extractvalue %NxVal %k, 1
  call void @nx_listset(%NxVal %b, i64 %i, %NxVal %v)
  ret %NxVal %b
c:
  %isd = icmp eq i64 %t, 7
  br i1 %isd, label %dict, label %bad
dict:
  store %NxVal %b, ptr %slot
  call void @nx_dictset(ptr %slot, %NxVal %k, %NxVal %v)
  %upd = load %NxVal, ptr %slot
  ret %NxVal %upd
bad:
  call void @nx_panic(ptr @.msg.type)
  unreachable
}

; `del a[k]` for an unresolved base.
define %NxVal @nx_delindex(%NxVal %b, %NxVal %k) {
entry:
  %t = extractvalue %NxVal %b, 0
  %isl = icmp eq i64 %t, 5
  br i1 %isl, label %list, label %c
list:
  %kt = extractvalue %NxVal %k, 0
  %ki = icmp eq i64 %kt, 1
  br i1 %ki, label %pos, label %bad
pos:
  %i = extractvalue %NxVal %k, 1
  %r = call %NxVal @nx_listdel(%NxVal %b, i64 %i)
  ret %NxVal %r
c:
  %isd = icmp eq i64 %t, 7
  br i1 %isd, label %dict, label %bad
dict:
  %r2 = call %NxVal @nx_dictdel(%NxVal %b, %NxVal %k)
  ret %NxVal %r2
bad:
  call void @nx_panic(ptr @.msg.type)
  unreachable
}

; Element `i` of an unresolved iterable: a list yields its element, a
; string its character, and a dict its `i`-th key in insertion order --
; which is what makes `for k in d` work when `d` is unresolved.
define %NxVal @nx_each(%NxVal %v, i64 %i) {
entry:
  %t = extractvalue %NxVal %v, 0
  %isd = icmp eq i64 %t, 7
  br i1 %isd, label %dict, label %rest
dict:
  %k = call %NxVal @nx_dictkeyat(%NxVal %v, i64 %i)
  ret %NxVal %k
rest:
  %ib = call %NxVal @nx_int(i64 %i)
  %e = call %NxVal @nx_index(%NxVal %v, %NxVal %ib)
  ret %NxVal %e
}

; `push(x, v)` for an unresolved `x`. Only a list can be pushed to; a
; dict takes `d[k] = v` instead, so anything else panics.
define void @nx_pushdyn(ptr %vp, %NxVal %v) {
entry:
  %lv = load %NxVal, ptr %vp
  %t = extractvalue %NxVal %lv, 0
  %isl = icmp eq i64 %t, 5
  br i1 %isl, label %ok, label %bad
ok:
  call void @nx_listpush(ptr %vp, %NxVal %v)
  ret void
bad:
  call void @nx_panic(ptr @.msg.type)
  unreachable
}
