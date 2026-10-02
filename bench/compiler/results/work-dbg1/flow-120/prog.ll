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

; --- task pool for `parallel:` -------------------------------------
;
; A batch gets a fixed pool sized to the batch, created before any task
; runs and joined before the enclosing code continues. Tasks are claimed
; from a shared cursor with a cmpxchg loop, so a thread that finishes
; early picks up the next task instead of idling. Which thread runs
; which task is not fixed, and does not need to be: the effects analysis
; has already proven conflicting tasks are serialized into separate
; batches, and a batch's tasks write disjoint state.
;
; %NxPool = { i64 cursor (atomic), i64 count, [n x ptr] tasks }
; The cursor is a cmpxchg spin rather than an atomicrmw add so the pool
; needs no target-specific atomic support.

%NxPool = type { i64, i64, ptr }

; Claim the next task index, or -1 when the batch is exhausted.
define i64 @nx_pool_claim(ptr %pool) {
entry:
  %cur = getelementptr %NxPool, ptr %pool, i64 0, i32 0
  %cntp = getelementptr %NxPool, ptr %pool, i64 0, i32 1
  %n = load i64, ptr %cntp
  br label %try
try:
  %old = atomicrmw add ptr %cur, i64 1 seq_cst
  %mine = icmp ult i64 %old, %n
  br i1 %mine, label %got, label %out
got:
  ret i64 %old
out:
  ret i64 -1
}

; Worker body: claim and run until the batch is drained.
define ptr @nx_pool_worker(ptr %p) {
entry:
  %pool = bitcast ptr %p to ptr
  br label %loop
loop:
  %i = call i64 @nx_pool_claim(ptr %pool)
  %more = icmp ne i64 %i, -1
  br i1 %more, label %run, label %done
run:
  %taskp = getelementptr %NxPool, ptr %pool, i64 0, i32 2
  %arr = load ptr, ptr %taskp
  %slot = getelementptr ptr, ptr %arr, i64 %i
  %f = load ptr, ptr %slot
  %r = call ptr %f(ptr null)
  br label %loop
done:
  ret ptr null
}

; Start one worker. Platform-specific: defined in runtime_threads_win.ll or
; runtime_threads_unix.ll, which codegen includes for the target. Declared
; here only so the pool reads as one unit.
; declare ptr @nx_thread_start(ptr, ptr)
; declare void @nx_thread_join(ptr)

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

; --- unboxed division: same zero-check panic as the boxed path ---
define i64 @nx_div_i64(i64 %l, i64 %r) {
entry:
  %z = icmp eq i64 %r, 0
  br i1 %z, label %dz, label %ok
dz:
  call void @nx_panic(ptr @.msg.divzero)
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
; table. Iteration order is part of the determinism contract `parallel:`
; rests on, and a hash map would make it depend on hashing. Linear lookup
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
  br i1 %z, label %dz, label %go
dz:
  call void @nx_panic(ptr @.msg.divzero)
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
  br i1 %z, label %mz, label %go
mz:
  call void @nx_panic(ptr @.msg.modzero)
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
  ; Repeated multiplication by the base, not repeated squaring: the
  ; exponent is already known to be at most 62 by the caller, so the
  ; simple loop is both correct and fast enough.
  br label %loop
loop:
  %i = phi i64 [0, %entry], [%i2, %body]
  %acc = phi i64 [1, %entry], [%acc2, %body]
  %done = icmp sge i64 %i, %e
  br i1 %done, label %exit, label %body
body:
  %m = mul i64 %acc, %base
  %acc2 = add i64 %m, 0
  %i2 = add i64 %i, 1
  br label %loop
exit:
  ret i64 %acc
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
  call void @nx_listpush(ptr %slot, %NxVal %v)
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
  %cnt2 = sdiv i64 %cap2, %step
  %buf = call ptr @malloc(i64 %cnt2)
  br label %sscan
sscan:
  %si = phi i64 [%f22, %str], [%si2, %sbody]
  %sdone = icmp sge i64 %si, %t22
  br i1 %sdone, label %sout, label %sbody
sbody:
  %srcp = getelementptr i8, ptr %s, i64 %si
  %c = load i8, ptr %srcp
  ; The destination is packed from zero, not mirrored from the source
  ; offset -- otherwise a slice starting past zero would write past the
  ; end of the buffer.
  %rel = sub i64 %si, %f22
  %dstp = getelementptr i8, ptr %buf, i64 %rel
  store i8 %c, ptr %dstp
  %si2 = add i64 %si, %step
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
; Windows thread shims for `parallel:`.
;
; Included instead of runtime_threads_unix.ll on MSVC targets. The pool
; logic itself lives in the main prelude; only these two OS bindings are
; platform-specific, which keeps the task-claiming code identical
; everywhere and therefore equally well tested.

declare ptr @CreateThread(ptr, i64, ptr, ptr, i32, ptr)
declare i32 @WaitForSingleObject(ptr, i32)
declare i32 @CloseHandle(ptr)

; The HANDLE is CreateThread's return value. Its last argument is
; lpThreadId, a DWORD id, and waiting on that fails every time.
define ptr @nx_thread_start(ptr %fn, ptr %arg) {
entry:
  %tid = alloca i32
  store i32 0, ptr %tid
  %h = call ptr @CreateThread(ptr null, i64 0, ptr %fn, ptr %arg, i32 0, ptr %tid)
  ret ptr %h
}

; INFINITE (-1): wait however long the task takes.
define void @nx_thread_join(ptr %h) {
entry:
  %w = call i32 @WaitForSingleObject(ptr %h, i32 -1)
  call i32 @CloseHandle(ptr %h)
  ret void
}

@.rec.tname.nx__d_4____main____Cell = private constant [4 x i8] c"Cell"
@.rec.fname.nx__d_4____main____Cell.0 = private constant [1 x i8] c"v"
@.rec.fname.nx__d_4____main____Cell.1 = private constant [1 x i8] c"w"
@.rec.names.nx__d_4____main____Cell = private constant [2 x %NxRecName] [%NxRecName { i64 1, ptr @.rec.fname.nx__d_4____main____Cell.0 }, %NxRecName { i64 1, ptr @.rec.fname.nx__d_4____main____Cell.1 }]
@nx__d_4____main____Cell = private constant %NxDesc { ptr @.rec.tname.nx__d_4____main____Cell, i64 4, i64 2, ptr @.rec.names.nx__d_4____main____Cell }
@nx__g___main____c = global %NxVal zeroinitializer
@nx__g___main____total = global %NxVal zeroinitializer
@nx__g___main____i0 = global %NxVal zeroinitializer
@nx__g___main____acc0 = global %NxVal zeroinitializer
@nx__g___main____i1 = global %NxVal zeroinitializer
@nx__g___main____acc1 = global %NxVal zeroinitializer
@nx__g___main____i2 = global %NxVal zeroinitializer
@nx__g___main____acc2 = global %NxVal zeroinitializer
@nx__g___main____i3 = global %NxVal zeroinitializer
@nx__g___main____acc3 = global %NxVal zeroinitializer
@nx__g___main____i4 = global %NxVal zeroinitializer
@nx__g___main____acc4 = global %NxVal zeroinitializer
@nx__g___main____i5 = global %NxVal zeroinitializer
@nx__g___main____acc5 = global %NxVal zeroinitializer
@nx__g___main____i6 = global %NxVal zeroinitializer
@nx__g___main____acc6 = global %NxVal zeroinitializer
@nx__g___main____i7 = global %NxVal zeroinitializer
@nx__g___main____acc7 = global %NxVal zeroinitializer
@nx__g___main____i8 = global %NxVal zeroinitializer
@nx__g___main____acc8 = global %NxVal zeroinitializer
@nx__g___main____i9 = global %NxVal zeroinitializer
@nx__g___main____acc9 = global %NxVal zeroinitializer
@nx__g___main____i10 = global %NxVal zeroinitializer
@nx__g___main____acc10 = global %NxVal zeroinitializer
@nx__g___main____i11 = global %NxVal zeroinitializer
@nx__g___main____acc11 = global %NxVal zeroinitializer
@nx__g___main____i12 = global %NxVal zeroinitializer
@nx__g___main____acc12 = global %NxVal zeroinitializer
@nx__g___main____i13 = global %NxVal zeroinitializer
@nx__g___main____acc13 = global %NxVal zeroinitializer
@nx__g___main____i14 = global %NxVal zeroinitializer
@nx__g___main____acc14 = global %NxVal zeroinitializer
@nx__g___main____i15 = global %NxVal zeroinitializer
@nx__g___main____acc15 = global %NxVal zeroinitializer
@nx__g___main____i16 = global %NxVal zeroinitializer
@nx__g___main____acc16 = global %NxVal zeroinitializer
@nx__g___main____i17 = global %NxVal zeroinitializer
@nx__g___main____acc17 = global %NxVal zeroinitializer
@nx__g___main____i18 = global %NxVal zeroinitializer
@nx__g___main____acc18 = global %NxVal zeroinitializer
@nx__g___main____i19 = global %NxVal zeroinitializer
@nx__g___main____acc19 = global %NxVal zeroinitializer
@nx__g___main____i20 = global %NxVal zeroinitializer
@nx__g___main____acc20 = global %NxVal zeroinitializer
@nx__g___main____i21 = global %NxVal zeroinitializer
@nx__g___main____acc21 = global %NxVal zeroinitializer
@nx__g___main____i22 = global %NxVal zeroinitializer
@nx__g___main____acc22 = global %NxVal zeroinitializer
@nx__g___main____i23 = global %NxVal zeroinitializer
@nx__g___main____acc23 = global %NxVal zeroinitializer
@nx__g___main____i24 = global %NxVal zeroinitializer
@nx__g___main____acc24 = global %NxVal zeroinitializer
@nx__g___main____i25 = global %NxVal zeroinitializer
@nx__g___main____acc25 = global %NxVal zeroinitializer
@nx__g___main____i26 = global %NxVal zeroinitializer
@nx__g___main____acc26 = global %NxVal zeroinitializer
@nx__g___main____i27 = global %NxVal zeroinitializer
@nx__g___main____acc27 = global %NxVal zeroinitializer
@nx__g___main____i28 = global %NxVal zeroinitializer
@nx__g___main____acc28 = global %NxVal zeroinitializer
@nx__g___main____i29 = global %NxVal zeroinitializer
@nx__g___main____acc29 = global %NxVal zeroinitializer
@nx__g___main____i30 = global %NxVal zeroinitializer
@nx__g___main____acc30 = global %NxVal zeroinitializer
@nx__g___main____i31 = global %NxVal zeroinitializer
@nx__g___main____acc31 = global %NxVal zeroinitializer
@nx__g___main____i32 = global %NxVal zeroinitializer
@nx__g___main____acc32 = global %NxVal zeroinitializer
@nx__g___main____i33 = global %NxVal zeroinitializer
@nx__g___main____acc33 = global %NxVal zeroinitializer
@nx__g___main____i34 = global %NxVal zeroinitializer
@nx__g___main____acc34 = global %NxVal zeroinitializer
@nx__g___main____i35 = global %NxVal zeroinitializer
@nx__g___main____acc35 = global %NxVal zeroinitializer
@nx__g___main____i36 = global %NxVal zeroinitializer
@nx__g___main____acc36 = global %NxVal zeroinitializer
@nx__g___main____i37 = global %NxVal zeroinitializer
@nx__g___main____acc37 = global %NxVal zeroinitializer
@nx__g___main____i38 = global %NxVal zeroinitializer
@nx__g___main____acc38 = global %NxVal zeroinitializer
@nx__g___main____i39 = global %NxVal zeroinitializer
@nx__g___main____acc39 = global %NxVal zeroinitializer
@nx__g___main____i40 = global %NxVal zeroinitializer
@nx__g___main____acc40 = global %NxVal zeroinitializer
@nx__g___main____i41 = global %NxVal zeroinitializer
@nx__g___main____acc41 = global %NxVal zeroinitializer
@nx__g___main____i42 = global %NxVal zeroinitializer
@nx__g___main____acc42 = global %NxVal zeroinitializer
@nx__g___main____i43 = global %NxVal zeroinitializer
@nx__g___main____acc43 = global %NxVal zeroinitializer
@nx__g___main____i44 = global %NxVal zeroinitializer
@nx__g___main____acc44 = global %NxVal zeroinitializer
@nx__g___main____i45 = global %NxVal zeroinitializer
@nx__g___main____acc45 = global %NxVal zeroinitializer
@nx__g___main____i46 = global %NxVal zeroinitializer
@nx__g___main____acc46 = global %NxVal zeroinitializer
@nx__g___main____i47 = global %NxVal zeroinitializer
@nx__g___main____acc47 = global %NxVal zeroinitializer
@nx__g___main____i48 = global %NxVal zeroinitializer
@nx__g___main____acc48 = global %NxVal zeroinitializer
@nx__g___main____i49 = global %NxVal zeroinitializer
@nx__g___main____acc49 = global %NxVal zeroinitializer
@nx__g___main____i50 = global %NxVal zeroinitializer
@nx__g___main____acc50 = global %NxVal zeroinitializer
@nx__g___main____i51 = global %NxVal zeroinitializer
@nx__g___main____acc51 = global %NxVal zeroinitializer
@nx__g___main____i52 = global %NxVal zeroinitializer
@nx__g___main____acc52 = global %NxVal zeroinitializer
@nx__g___main____i53 = global %NxVal zeroinitializer
@nx__g___main____acc53 = global %NxVal zeroinitializer
@nx__g___main____i54 = global %NxVal zeroinitializer
@nx__g___main____acc54 = global %NxVal zeroinitializer
@nx__g___main____i55 = global %NxVal zeroinitializer
@nx__g___main____acc55 = global %NxVal zeroinitializer
@nx__g___main____i56 = global %NxVal zeroinitializer
@nx__g___main____acc56 = global %NxVal zeroinitializer
@nx__g___main____i57 = global %NxVal zeroinitializer
@nx__g___main____acc57 = global %NxVal zeroinitializer
@nx__g___main____i58 = global %NxVal zeroinitializer
@nx__g___main____acc58 = global %NxVal zeroinitializer
@nx__g___main____i59 = global %NxVal zeroinitializer
@nx__g___main____acc59 = global %NxVal zeroinitializer
@nx__g___main____i60 = global %NxVal zeroinitializer
@nx__g___main____acc60 = global %NxVal zeroinitializer
@nx__g___main____i61 = global %NxVal zeroinitializer
@nx__g___main____acc61 = global %NxVal zeroinitializer
@nx__g___main____i62 = global %NxVal zeroinitializer
@nx__g___main____acc62 = global %NxVal zeroinitializer
@nx__g___main____i63 = global %NxVal zeroinitializer
@nx__g___main____acc63 = global %NxVal zeroinitializer
@nx__g___main____i64 = global %NxVal zeroinitializer
@nx__g___main____acc64 = global %NxVal zeroinitializer
@nx__g___main____i65 = global %NxVal zeroinitializer
@nx__g___main____acc65 = global %NxVal zeroinitializer
@nx__g___main____i66 = global %NxVal zeroinitializer
@nx__g___main____acc66 = global %NxVal zeroinitializer
@nx__g___main____i67 = global %NxVal zeroinitializer
@nx__g___main____acc67 = global %NxVal zeroinitializer
@nx__g___main____i68 = global %NxVal zeroinitializer
@nx__g___main____acc68 = global %NxVal zeroinitializer
@nx__g___main____i69 = global %NxVal zeroinitializer
@nx__g___main____acc69 = global %NxVal zeroinitializer
@nx__g___main____i70 = global %NxVal zeroinitializer
@nx__g___main____acc70 = global %NxVal zeroinitializer
@nx__g___main____i71 = global %NxVal zeroinitializer
@nx__g___main____acc71 = global %NxVal zeroinitializer
@nx__g___main____i72 = global %NxVal zeroinitializer
@nx__g___main____acc72 = global %NxVal zeroinitializer
@nx__g___main____i73 = global %NxVal zeroinitializer
@nx__g___main____acc73 = global %NxVal zeroinitializer
@nx__g___main____i74 = global %NxVal zeroinitializer
@nx__g___main____acc74 = global %NxVal zeroinitializer
@nx__g___main____i75 = global %NxVal zeroinitializer
@nx__g___main____acc75 = global %NxVal zeroinitializer
@nx__g___main____i76 = global %NxVal zeroinitializer
@nx__g___main____acc76 = global %NxVal zeroinitializer
@nx__g___main____i77 = global %NxVal zeroinitializer
@nx__g___main____acc77 = global %NxVal zeroinitializer
@nx__g___main____i78 = global %NxVal zeroinitializer
@nx__g___main____acc78 = global %NxVal zeroinitializer
@nx__g___main____i79 = global %NxVal zeroinitializer
@nx__g___main____acc79 = global %NxVal zeroinitializer
@nx__g___main____i80 = global %NxVal zeroinitializer
@nx__g___main____acc80 = global %NxVal zeroinitializer
@nx__g___main____i81 = global %NxVal zeroinitializer
@nx__g___main____acc81 = global %NxVal zeroinitializer
@nx__g___main____i82 = global %NxVal zeroinitializer
@nx__g___main____acc82 = global %NxVal zeroinitializer
@nx__g___main____i83 = global %NxVal zeroinitializer
@nx__g___main____acc83 = global %NxVal zeroinitializer
@nx__g___main____i84 = global %NxVal zeroinitializer
@nx__g___main____acc84 = global %NxVal zeroinitializer
@nx__g___main____i85 = global %NxVal zeroinitializer
@nx__g___main____acc85 = global %NxVal zeroinitializer
@nx__g___main____i86 = global %NxVal zeroinitializer
@nx__g___main____acc86 = global %NxVal zeroinitializer
@nx__g___main____i87 = global %NxVal zeroinitializer
@nx__g___main____acc87 = global %NxVal zeroinitializer
@nx__g___main____i88 = global %NxVal zeroinitializer
@nx__g___main____acc88 = global %NxVal zeroinitializer
@nx__g___main____i89 = global %NxVal zeroinitializer
@nx__g___main____acc89 = global %NxVal zeroinitializer
@nx__g___main____i90 = global %NxVal zeroinitializer
@nx__g___main____acc90 = global %NxVal zeroinitializer
@nx__g___main____i91 = global %NxVal zeroinitializer
@nx__g___main____acc91 = global %NxVal zeroinitializer
@nx__g___main____i92 = global %NxVal zeroinitializer
@nx__g___main____acc92 = global %NxVal zeroinitializer
@nx__g___main____i93 = global %NxVal zeroinitializer
@nx__g___main____acc93 = global %NxVal zeroinitializer
@nx__g___main____i94 = global %NxVal zeroinitializer
@nx__g___main____acc94 = global %NxVal zeroinitializer
@nx__g___main____i95 = global %NxVal zeroinitializer
@nx__g___main____acc95 = global %NxVal zeroinitializer
@nx__g___main____i96 = global %NxVal zeroinitializer
@nx__g___main____acc96 = global %NxVal zeroinitializer
@nx__g___main____i97 = global %NxVal zeroinitializer
@nx__g___main____acc97 = global %NxVal zeroinitializer
@nx__g___main____i98 = global %NxVal zeroinitializer
@nx__g___main____acc98 = global %NxVal zeroinitializer
@nx__g___main____i99 = global %NxVal zeroinitializer
@nx__g___main____acc99 = global %NxVal zeroinitializer
@nx__g___main____i100 = global %NxVal zeroinitializer
@nx__g___main____acc100 = global %NxVal zeroinitializer
@nx__g___main____i101 = global %NxVal zeroinitializer
@nx__g___main____acc101 = global %NxVal zeroinitializer
@nx__g___main____i102 = global %NxVal zeroinitializer
@nx__g___main____acc102 = global %NxVal zeroinitializer
@nx__g___main____i103 = global %NxVal zeroinitializer
@nx__g___main____acc103 = global %NxVal zeroinitializer
@nx__g___main____i104 = global %NxVal zeroinitializer
@nx__g___main____acc104 = global %NxVal zeroinitializer
@nx__g___main____i105 = global %NxVal zeroinitializer
@nx__g___main____acc105 = global %NxVal zeroinitializer
@nx__g___main____i106 = global %NxVal zeroinitializer
@nx__g___main____acc106 = global %NxVal zeroinitializer
@nx__g___main____i107 = global %NxVal zeroinitializer
@nx__g___main____acc107 = global %NxVal zeroinitializer
@nx__g___main____i108 = global %NxVal zeroinitializer
@nx__g___main____acc108 = global %NxVal zeroinitializer
@nx__g___main____i109 = global %NxVal zeroinitializer
@nx__g___main____acc109 = global %NxVal zeroinitializer
@nx__g___main____i110 = global %NxVal zeroinitializer
@nx__g___main____acc110 = global %NxVal zeroinitializer
@nx__g___main____i111 = global %NxVal zeroinitializer
@nx__g___main____acc111 = global %NxVal zeroinitializer
@nx__g___main____i112 = global %NxVal zeroinitializer
@nx__g___main____acc112 = global %NxVal zeroinitializer
@nx__g___main____i113 = global %NxVal zeroinitializer
@nx__g___main____acc113 = global %NxVal zeroinitializer
@nx__g___main____i114 = global %NxVal zeroinitializer
@nx__g___main____acc114 = global %NxVal zeroinitializer
@nx__g___main____i115 = global %NxVal zeroinitializer
@nx__g___main____acc115 = global %NxVal zeroinitializer
@nx__g___main____i116 = global %NxVal zeroinitializer
@nx__g___main____acc116 = global %NxVal zeroinitializer
@nx__g___main____i117 = global %NxVal zeroinitializer
@nx__g___main____acc117 = global %NxVal zeroinitializer
@nx__g___main____i118 = global %NxVal zeroinitializer
@nx__g___main____acc118 = global %NxVal zeroinitializer
@nx__g___main____i119 = global %NxVal zeroinitializer
@nx__g___main____acc119 = global %NxVal zeroinitializer
@nx__done___main__ = global i1 false
define %NxVal @nx__m_2____main____Cell__m0(%NxVal* %args, i64 %nargs) {
entry:
  %t1 = alloca %NxVal
  %t5 = alloca i64
  %t20 = alloca i64
  store %NxVal zeroinitializer, ptr %t1
  %t2 = getelementptr %NxVal, ptr %args, i64 0
  %t3 = load %NxVal, ptr %t2
  %t4 = call %NxVal @nx_clone(%NxVal %t3)
  store %NxVal %t4, ptr %t1
  %t6 = getelementptr %NxVal, ptr %args, i64 1
  %t7 = load %NxVal, ptr %t6
  %t8 = extractvalue %NxVal %t7, 1
  store i64 %t8, ptr %t5
  %t9 = load %NxVal, ptr %t1
  %t10 = call %NxVal @nx_rec_get(%NxVal %t9, i64 0)
  %t11 = load %NxVal, ptr %t1
  %t12 = call %NxVal @nx_rec_get(%NxVal %t11, i64 1)
  %t13 = call %NxVal @nx_add(%NxVal %t10, %NxVal %t12)
  %t14 = load i64, ptr %t5
  %t15 = call %NxVal @nx_int(i64 %t14)
  %t16 = call %NxVal @nx_add(%NxVal %t13, %NxVal %t15)
  %t17 = add i64 65535, 0
  %t18 = call %NxVal @nx_int(i64 %t17)
  %t19 = call %NxVal @nx_bitand(%NxVal %t16, %NxVal %t18)
  %t21 = extractvalue %NxVal %t19, 1
  store i64 %t21, ptr %t20
  %t22 = load i64, ptr %t20
  %t23 = add i64 70, 0
  %t24 = and i64 %t22, %t23
  %t25 = add i64 40, 0
  %t26 = and i64 %t24, %t25
  %t27 = add i64 65535, 0
  %t28 = and i64 %t26, %t27
  store i64 %t28, ptr %t20
  %t29 = load i64, ptr %t20
  %t30 = add i64 16, 0
  %t31 = or i64 %t29, %t30
  %t32 = add i64 67, 0
  %t33 = or i64 %t31, %t32
  %t34 = add i64 65535, 0
  %t35 = and i64 %t33, %t34
  store i64 %t35, ptr %t20
  %t36 = load i64, ptr %t20
  %t37 = add i64 56, 0
  %t38 = or i64 %t36, %t37
  %t39 = add i64 44, 0
  %t40 = or i64 %t38, %t39
  %t41 = add i64 65535, 0
  %t42 = and i64 %t40, %t41
  store i64 %t42, ptr %t20
  %t43 = load i64, ptr %t20
  %t44 = add i64 94, 0
  %t45 = mul i64 %t43, %t44
  %t46 = add i64 65, 0
  %t47 = mul i64 %t45, %t46
  %t48 = add i64 65535, 0
  %t49 = and i64 %t47, %t48
  store i64 %t49, ptr %t20
  %t50 = load i64, ptr %t20
  %t51 = add i64 12, 0
  %t52 = xor i64 %t50, %t51
  %t53 = add i64 5, 0
  %t54 = xor i64 %t52, %t53
  %t55 = add i64 65535, 0
  %t56 = and i64 %t54, %t55
  store i64 %t56, ptr %t20
  %t57 = load i64, ptr %t20
  %t58 = call %NxVal @nx_int(i64 %t57)
  ret %NxVal %t58
}
define %NxVal @nx__m_2____main____Cell__m1(%NxVal* %args, i64 %nargs) {
entry:
  %t59 = alloca %NxVal
  %t63 = alloca i64
  %t78 = alloca i64
  store %NxVal zeroinitializer, ptr %t59
  %t60 = getelementptr %NxVal, ptr %args, i64 0
  %t61 = load %NxVal, ptr %t60
  %t62 = call %NxVal @nx_clone(%NxVal %t61)
  store %NxVal %t62, ptr %t59
  %t64 = getelementptr %NxVal, ptr %args, i64 1
  %t65 = load %NxVal, ptr %t64
  %t66 = extractvalue %NxVal %t65, 1
  store i64 %t66, ptr %t63
  %t67 = load %NxVal, ptr %t59
  %t68 = call %NxVal @nx_rec_get(%NxVal %t67, i64 0)
  %t69 = load %NxVal, ptr %t59
  %t70 = call %NxVal @nx_rec_get(%NxVal %t69, i64 1)
  %t71 = call %NxVal @nx_add(%NxVal %t68, %NxVal %t70)
  %t72 = load i64, ptr %t63
  %t73 = call %NxVal @nx_int(i64 %t72)
  %t74 = call %NxVal @nx_add(%NxVal %t71, %NxVal %t73)
  %t75 = add i64 65535, 0
  %t76 = call %NxVal @nx_int(i64 %t75)
  %t77 = call %NxVal @nx_bitand(%NxVal %t74, %NxVal %t76)
  %t79 = extractvalue %NxVal %t77, 1
  store i64 %t79, ptr %t78
  %t80 = load i64, ptr %t78
  %t81 = add i64 84, 0
  %t82 = mul i64 %t80, %t81
  %t83 = add i64 28, 0
  %t84 = mul i64 %t82, %t83
  %t85 = add i64 65535, 0
  %t86 = and i64 %t84, %t85
  store i64 %t86, ptr %t78
  %t87 = load i64, ptr %t78
  %t88 = add i64 3, 0
  %t89 = add i64 %t87, %t88
  %t90 = add i64 48, 0
  %t91 = add i64 %t89, %t90
  %t92 = add i64 65535, 0
  %t93 = and i64 %t91, %t92
  store i64 %t93, ptr %t78
  %t94 = load i64, ptr %t78
  %t95 = add i64 51, 0
  %t96 = add i64 %t94, %t95
  %t97 = add i64 40, 0
  %t98 = add i64 %t96, %t97
  %t99 = add i64 65535, 0
  %t100 = and i64 %t98, %t99
  store i64 %t100, ptr %t78
  %t101 = load i64, ptr %t78
  %t102 = add i64 2, 0
  %t103 = and i64 %t101, %t102
  %t104 = add i64 14, 0
  %t105 = and i64 %t103, %t104
  %t106 = add i64 65535, 0
  %t107 = and i64 %t105, %t106
  store i64 %t107, ptr %t78
  %t108 = load i64, ptr %t78
  %t109 = add i64 3, 0
  %t110 = or i64 %t108, %t109
  %t111 = add i64 72, 0
  %t112 = or i64 %t110, %t111
  %t113 = add i64 65535, 0
  %t114 = and i64 %t112, %t113
  store i64 %t114, ptr %t78
  %t115 = load i64, ptr %t78
  %t116 = call %NxVal @nx_int(i64 %t115)
  ret %NxVal %t116
}
define %NxVal @nx__m_3____main____Cell__m10(%NxVal* %args, i64 %nargs) {
entry:
  %t117 = alloca %NxVal
  %t121 = alloca i64
  %t136 = alloca i64
  store %NxVal zeroinitializer, ptr %t117
  %t118 = getelementptr %NxVal, ptr %args, i64 0
  %t119 = load %NxVal, ptr %t118
  %t120 = call %NxVal @nx_clone(%NxVal %t119)
  store %NxVal %t120, ptr %t117
  %t122 = getelementptr %NxVal, ptr %args, i64 1
  %t123 = load %NxVal, ptr %t122
  %t124 = extractvalue %NxVal %t123, 1
  store i64 %t124, ptr %t121
  %t125 = load %NxVal, ptr %t117
  %t126 = call %NxVal @nx_rec_get(%NxVal %t125, i64 0)
  %t127 = load %NxVal, ptr %t117
  %t128 = call %NxVal @nx_rec_get(%NxVal %t127, i64 1)
  %t129 = call %NxVal @nx_add(%NxVal %t126, %NxVal %t128)
  %t130 = load i64, ptr %t121
  %t131 = call %NxVal @nx_int(i64 %t130)
  %t132 = call %NxVal @nx_add(%NxVal %t129, %NxVal %t131)
  %t133 = add i64 65535, 0
  %t134 = call %NxVal @nx_int(i64 %t133)
  %t135 = call %NxVal @nx_bitand(%NxVal %t132, %NxVal %t134)
  %t137 = extractvalue %NxVal %t135, 1
  store i64 %t137, ptr %t136
  %t138 = load i64, ptr %t136
  %t139 = add i64 76, 0
  %t140 = call i64 @nx_mod_i64(i64 %t138, i64 %t139)
  %t141 = add i64 4, 0
  %t142 = call i64 @nx_mod_i64(i64 %t140, i64 %t141)
  %t143 = add i64 65535, 0
  %t144 = and i64 %t142, %t143
  store i64 %t144, ptr %t136
  %t145 = load i64, ptr %t136
  %t146 = add i64 71, 0
  %t147 = call i64 @nx_mod_i64(i64 %t145, i64 %t146)
  %t148 = add i64 74, 0
  %t149 = call i64 @nx_mod_i64(i64 %t147, i64 %t148)
  %t150 = add i64 65535, 0
  %t151 = and i64 %t149, %t150
  store i64 %t151, ptr %t136
  %t152 = load i64, ptr %t136
  %t153 = add i64 8, 0
  %t154 = xor i64 %t152, %t153
  %t155 = add i64 84, 0
  %t156 = xor i64 %t154, %t155
  %t157 = add i64 65535, 0
  %t158 = and i64 %t156, %t157
  store i64 %t158, ptr %t136
  %t159 = load i64, ptr %t136
  %t160 = add i64 53, 0
  %t161 = call i64 @nx_mod_i64(i64 %t159, i64 %t160)
  %t162 = add i64 42, 0
  %t163 = call i64 @nx_mod_i64(i64 %t161, i64 %t162)
  %t164 = add i64 65535, 0
  %t165 = and i64 %t163, %t164
  store i64 %t165, ptr %t136
  %t166 = load i64, ptr %t136
  %t167 = add i64 2, 0
  %t168 = call i64 @nx_mod_i64(i64 %t166, i64 %t167)
  %t169 = add i64 53, 0
  %t170 = call i64 @nx_mod_i64(i64 %t168, i64 %t169)
  %t171 = add i64 65535, 0
  %t172 = and i64 %t170, %t171
  store i64 %t172, ptr %t136
  %t173 = load i64, ptr %t136
  %t174 = call %NxVal @nx_int(i64 %t173)
  ret %NxVal %t174
}
define %NxVal @nx__m_4____main____Cell__m100(%NxVal* %args, i64 %nargs) {
entry:
  %t175 = alloca %NxVal
  %t179 = alloca i64
  %t194 = alloca i64
  store %NxVal zeroinitializer, ptr %t175
  %t176 = getelementptr %NxVal, ptr %args, i64 0
  %t177 = load %NxVal, ptr %t176
  %t178 = call %NxVal @nx_clone(%NxVal %t177)
  store %NxVal %t178, ptr %t175
  %t180 = getelementptr %NxVal, ptr %args, i64 1
  %t181 = load %NxVal, ptr %t180
  %t182 = extractvalue %NxVal %t181, 1
  store i64 %t182, ptr %t179
  %t183 = load %NxVal, ptr %t175
  %t184 = call %NxVal @nx_rec_get(%NxVal %t183, i64 0)
  %t185 = load %NxVal, ptr %t175
  %t186 = call %NxVal @nx_rec_get(%NxVal %t185, i64 1)
  %t187 = call %NxVal @nx_add(%NxVal %t184, %NxVal %t186)
  %t188 = load i64, ptr %t179
  %t189 = call %NxVal @nx_int(i64 %t188)
  %t190 = call %NxVal @nx_add(%NxVal %t187, %NxVal %t189)
  %t191 = add i64 65535, 0
  %t192 = call %NxVal @nx_int(i64 %t191)
  %t193 = call %NxVal @nx_bitand(%NxVal %t190, %NxVal %t192)
  %t195 = extractvalue %NxVal %t193, 1
  store i64 %t195, ptr %t194
  %t196 = load i64, ptr %t194
  %t197 = add i64 7, 0
  %t198 = mul i64 %t196, %t197
  %t199 = add i64 85, 0
  %t200 = mul i64 %t198, %t199
  %t201 = add i64 65535, 0
  %t202 = and i64 %t200, %t201
  store i64 %t202, ptr %t194
  %t203 = load i64, ptr %t194
  %t204 = add i64 47, 0
  %t205 = call i64 @nx_mod_i64(i64 %t203, i64 %t204)
  %t206 = add i64 38, 0
  %t207 = call i64 @nx_mod_i64(i64 %t205, i64 %t206)
  %t208 = add i64 65535, 0
  %t209 = and i64 %t207, %t208
  store i64 %t209, ptr %t194
  %t210 = load i64, ptr %t194
  %t211 = add i64 86, 0
  %t212 = sub i64 %t210, %t211
  %t213 = add i64 18, 0
  %t214 = sub i64 %t212, %t213
  %t215 = add i64 65535, 0
  %t216 = and i64 %t214, %t215
  store i64 %t216, ptr %t194
  %t217 = load i64, ptr %t194
  %t218 = add i64 59, 0
  %t219 = xor i64 %t217, %t218
  %t220 = add i64 14, 0
  %t221 = xor i64 %t219, %t220
  %t222 = add i64 65535, 0
  %t223 = and i64 %t221, %t222
  store i64 %t223, ptr %t194
  %t224 = load i64, ptr %t194
  %t225 = add i64 8, 0
  %t226 = mul i64 %t224, %t225
  %t227 = add i64 60, 0
  %t228 = mul i64 %t226, %t227
  %t229 = add i64 65535, 0
  %t230 = and i64 %t228, %t229
  store i64 %t230, ptr %t194
  %t231 = load i64, ptr %t194
  %t232 = call %NxVal @nx_int(i64 %t231)
  ret %NxVal %t232
}
define %NxVal @nx__m_4____main____Cell__m101(%NxVal* %args, i64 %nargs) {
entry:
  %t233 = alloca %NxVal
  %t237 = alloca i64
  %t252 = alloca i64
  store %NxVal zeroinitializer, ptr %t233
  %t234 = getelementptr %NxVal, ptr %args, i64 0
  %t235 = load %NxVal, ptr %t234
  %t236 = call %NxVal @nx_clone(%NxVal %t235)
  store %NxVal %t236, ptr %t233
  %t238 = getelementptr %NxVal, ptr %args, i64 1
  %t239 = load %NxVal, ptr %t238
  %t240 = extractvalue %NxVal %t239, 1
  store i64 %t240, ptr %t237
  %t241 = load %NxVal, ptr %t233
  %t242 = call %NxVal @nx_rec_get(%NxVal %t241, i64 0)
  %t243 = load %NxVal, ptr %t233
  %t244 = call %NxVal @nx_rec_get(%NxVal %t243, i64 1)
  %t245 = call %NxVal @nx_add(%NxVal %t242, %NxVal %t244)
  %t246 = load i64, ptr %t237
  %t247 = call %NxVal @nx_int(i64 %t246)
  %t248 = call %NxVal @nx_add(%NxVal %t245, %NxVal %t247)
  %t249 = add i64 65535, 0
  %t250 = call %NxVal @nx_int(i64 %t249)
  %t251 = call %NxVal @nx_bitand(%NxVal %t248, %NxVal %t250)
  %t253 = extractvalue %NxVal %t251, 1
  store i64 %t253, ptr %t252
  %t254 = load i64, ptr %t252
  %t255 = add i64 64, 0
  %t256 = add i64 %t254, %t255
  %t257 = add i64 38, 0
  %t258 = add i64 %t256, %t257
  %t259 = add i64 65535, 0
  %t260 = and i64 %t258, %t259
  store i64 %t260, ptr %t252
  %t261 = load i64, ptr %t252
  %t262 = add i64 2, 0
  %t263 = sub i64 %t261, %t262
  %t264 = add i64 39, 0
  %t265 = sub i64 %t263, %t264
  %t266 = add i64 65535, 0
  %t267 = and i64 %t265, %t266
  store i64 %t267, ptr %t252
  %t268 = load i64, ptr %t252
  %t269 = add i64 42, 0
  %t270 = sub i64 %t268, %t269
  %t271 = add i64 53, 0
  %t272 = sub i64 %t270, %t271
  %t273 = add i64 65535, 0
  %t274 = and i64 %t272, %t273
  store i64 %t274, ptr %t252
  %t275 = load i64, ptr %t252
  %t276 = add i64 58, 0
  %t277 = mul i64 %t275, %t276
  %t278 = add i64 14, 0
  %t279 = mul i64 %t277, %t278
  %t280 = add i64 65535, 0
  %t281 = and i64 %t279, %t280
  store i64 %t281, ptr %t252
  %t282 = load i64, ptr %t252
  %t283 = add i64 74, 0
  %t284 = or i64 %t282, %t283
  %t285 = add i64 53, 0
  %t286 = or i64 %t284, %t285
  %t287 = add i64 65535, 0
  %t288 = and i64 %t286, %t287
  store i64 %t288, ptr %t252
  %t289 = load i64, ptr %t252
  %t290 = call %NxVal @nx_int(i64 %t289)
  ret %NxVal %t290
}
define %NxVal @nx__m_4____main____Cell__m102(%NxVal* %args, i64 %nargs) {
entry:
  %t291 = alloca %NxVal
  %t295 = alloca i64
  %t310 = alloca i64
  store %NxVal zeroinitializer, ptr %t291
  %t292 = getelementptr %NxVal, ptr %args, i64 0
  %t293 = load %NxVal, ptr %t292
  %t294 = call %NxVal @nx_clone(%NxVal %t293)
  store %NxVal %t294, ptr %t291
  %t296 = getelementptr %NxVal, ptr %args, i64 1
  %t297 = load %NxVal, ptr %t296
  %t298 = extractvalue %NxVal %t297, 1
  store i64 %t298, ptr %t295
  %t299 = load %NxVal, ptr %t291
  %t300 = call %NxVal @nx_rec_get(%NxVal %t299, i64 0)
  %t301 = load %NxVal, ptr %t291
  %t302 = call %NxVal @nx_rec_get(%NxVal %t301, i64 1)
  %t303 = call %NxVal @nx_add(%NxVal %t300, %NxVal %t302)
  %t304 = load i64, ptr %t295
  %t305 = call %NxVal @nx_int(i64 %t304)
  %t306 = call %NxVal @nx_add(%NxVal %t303, %NxVal %t305)
  %t307 = add i64 65535, 0
  %t308 = call %NxVal @nx_int(i64 %t307)
  %t309 = call %NxVal @nx_bitand(%NxVal %t306, %NxVal %t308)
  %t311 = extractvalue %NxVal %t309, 1
  store i64 %t311, ptr %t310
  %t312 = load i64, ptr %t310
  %t313 = add i64 22, 0
  %t314 = mul i64 %t312, %t313
  %t315 = add i64 76, 0
  %t316 = mul i64 %t314, %t315
  %t317 = add i64 65535, 0
  %t318 = and i64 %t316, %t317
  store i64 %t318, ptr %t310
  %t319 = load i64, ptr %t310
  %t320 = add i64 2, 0
  %t321 = call i64 @nx_mod_i64(i64 %t319, i64 %t320)
  %t322 = add i64 67, 0
  %t323 = call i64 @nx_mod_i64(i64 %t321, i64 %t322)
  %t324 = add i64 65535, 0
  %t325 = and i64 %t323, %t324
  store i64 %t325, ptr %t310
  %t326 = load i64, ptr %t310
  %t327 = add i64 3, 0
  %t328 = add i64 %t326, %t327
  %t329 = add i64 22, 0
  %t330 = add i64 %t328, %t329
  %t331 = add i64 65535, 0
  %t332 = and i64 %t330, %t331
  store i64 %t332, ptr %t310
  %t333 = load i64, ptr %t310
  %t334 = add i64 41, 0
  %t335 = add i64 %t333, %t334
  %t336 = add i64 76, 0
  %t337 = add i64 %t335, %t336
  %t338 = add i64 65535, 0
  %t339 = and i64 %t337, %t338
  store i64 %t339, ptr %t310
  %t340 = load i64, ptr %t310
  %t341 = add i64 19, 0
  %t342 = call i64 @nx_mod_i64(i64 %t340, i64 %t341)
  %t343 = add i64 1, 0
  %t344 = call i64 @nx_mod_i64(i64 %t342, i64 %t343)
  %t345 = add i64 65535, 0
  %t346 = and i64 %t344, %t345
  store i64 %t346, ptr %t310
  %t347 = load i64, ptr %t310
  %t348 = call %NxVal @nx_int(i64 %t347)
  ret %NxVal %t348
}
define %NxVal @nx__m_4____main____Cell__m103(%NxVal* %args, i64 %nargs) {
entry:
  %t349 = alloca %NxVal
  %t353 = alloca i64
  %t368 = alloca i64
  store %NxVal zeroinitializer, ptr %t349
  %t350 = getelementptr %NxVal, ptr %args, i64 0
  %t351 = load %NxVal, ptr %t350
  %t352 = call %NxVal @nx_clone(%NxVal %t351)
  store %NxVal %t352, ptr %t349
  %t354 = getelementptr %NxVal, ptr %args, i64 1
  %t355 = load %NxVal, ptr %t354
  %t356 = extractvalue %NxVal %t355, 1
  store i64 %t356, ptr %t353
  %t357 = load %NxVal, ptr %t349
  %t358 = call %NxVal @nx_rec_get(%NxVal %t357, i64 0)
  %t359 = load %NxVal, ptr %t349
  %t360 = call %NxVal @nx_rec_get(%NxVal %t359, i64 1)
  %t361 = call %NxVal @nx_add(%NxVal %t358, %NxVal %t360)
  %t362 = load i64, ptr %t353
  %t363 = call %NxVal @nx_int(i64 %t362)
  %t364 = call %NxVal @nx_add(%NxVal %t361, %NxVal %t363)
  %t365 = add i64 65535, 0
  %t366 = call %NxVal @nx_int(i64 %t365)
  %t367 = call %NxVal @nx_bitand(%NxVal %t364, %NxVal %t366)
  %t369 = extractvalue %NxVal %t367, 1
  store i64 %t369, ptr %t368
  %t370 = load i64, ptr %t368
  %t371 = add i64 27, 0
  %t372 = call i64 @nx_mod_i64(i64 %t370, i64 %t371)
  %t373 = add i64 14, 0
  %t374 = call i64 @nx_mod_i64(i64 %t372, i64 %t373)
  %t375 = add i64 65535, 0
  %t376 = and i64 %t374, %t375
  store i64 %t376, ptr %t368
  %t377 = load i64, ptr %t368
  %t378 = add i64 68, 0
  %t379 = call i64 @nx_mod_i64(i64 %t377, i64 %t378)
  %t380 = add i64 34, 0
  %t381 = call i64 @nx_mod_i64(i64 %t379, i64 %t380)
  %t382 = add i64 65535, 0
  %t383 = and i64 %t381, %t382
  store i64 %t383, ptr %t368
  %t384 = load i64, ptr %t368
  %t385 = add i64 37, 0
  %t386 = and i64 %t384, %t385
  %t387 = add i64 12, 0
  %t388 = and i64 %t386, %t387
  %t389 = add i64 65535, 0
  %t390 = and i64 %t388, %t389
  store i64 %t390, ptr %t368
  %t391 = load i64, ptr %t368
  %t392 = add i64 55, 0
  %t393 = add i64 %t391, %t392
  %t394 = add i64 32, 0
  %t395 = add i64 %t393, %t394
  %t396 = add i64 65535, 0
  %t397 = and i64 %t395, %t396
  store i64 %t397, ptr %t368
  %t398 = load i64, ptr %t368
  %t399 = add i64 63, 0
  %t400 = add i64 %t398, %t399
  %t401 = add i64 34, 0
  %t402 = add i64 %t400, %t401
  %t403 = add i64 65535, 0
  %t404 = and i64 %t402, %t403
  store i64 %t404, ptr %t368
  %t405 = load i64, ptr %t368
  %t406 = call %NxVal @nx_int(i64 %t405)
  ret %NxVal %t406
}
define %NxVal @nx__m_4____main____Cell__m104(%NxVal* %args, i64 %nargs) {
entry:
  %t407 = alloca %NxVal
  %t411 = alloca i64
  %t426 = alloca i64
  store %NxVal zeroinitializer, ptr %t407
  %t408 = getelementptr %NxVal, ptr %args, i64 0
  %t409 = load %NxVal, ptr %t408
  %t410 = call %NxVal @nx_clone(%NxVal %t409)
  store %NxVal %t410, ptr %t407
  %t412 = getelementptr %NxVal, ptr %args, i64 1
  %t413 = load %NxVal, ptr %t412
  %t414 = extractvalue %NxVal %t413, 1
  store i64 %t414, ptr %t411
  %t415 = load %NxVal, ptr %t407
  %t416 = call %NxVal @nx_rec_get(%NxVal %t415, i64 0)
  %t417 = load %NxVal, ptr %t407
  %t418 = call %NxVal @nx_rec_get(%NxVal %t417, i64 1)
  %t419 = call %NxVal @nx_add(%NxVal %t416, %NxVal %t418)
  %t420 = load i64, ptr %t411
  %t421 = call %NxVal @nx_int(i64 %t420)
  %t422 = call %NxVal @nx_add(%NxVal %t419, %NxVal %t421)
  %t423 = add i64 65535, 0
  %t424 = call %NxVal @nx_int(i64 %t423)
  %t425 = call %NxVal @nx_bitand(%NxVal %t422, %NxVal %t424)
  %t427 = extractvalue %NxVal %t425, 1
  store i64 %t427, ptr %t426
  %t428 = load i64, ptr %t426
  %t429 = add i64 67, 0
  %t430 = call i64 @nx_mod_i64(i64 %t428, i64 %t429)
  %t431 = add i64 87, 0
  %t432 = call i64 @nx_mod_i64(i64 %t430, i64 %t431)
  %t433 = add i64 65535, 0
  %t434 = and i64 %t432, %t433
  store i64 %t434, ptr %t426
  %t435 = load i64, ptr %t426
  %t436 = add i64 56, 0
  %t437 = add i64 %t435, %t436
  %t438 = add i64 54, 0
  %t439 = add i64 %t437, %t438
  %t440 = add i64 65535, 0
  %t441 = and i64 %t439, %t440
  store i64 %t441, ptr %t426
  %t442 = load i64, ptr %t426
  %t443 = add i64 73, 0
  %t444 = or i64 %t442, %t443
  %t445 = add i64 37, 0
  %t446 = or i64 %t444, %t445
  %t447 = add i64 65535, 0
  %t448 = and i64 %t446, %t447
  store i64 %t448, ptr %t426
  %t449 = load i64, ptr %t426
  %t450 = add i64 88, 0
  %t451 = call i64 @nx_mod_i64(i64 %t449, i64 %t450)
  %t452 = add i64 46, 0
  %t453 = call i64 @nx_mod_i64(i64 %t451, i64 %t452)
  %t454 = add i64 65535, 0
  %t455 = and i64 %t453, %t454
  store i64 %t455, ptr %t426
  %t456 = load i64, ptr %t426
  %t457 = add i64 78, 0
  %t458 = xor i64 %t456, %t457
  %t459 = add i64 18, 0
  %t460 = xor i64 %t458, %t459
  %t461 = add i64 65535, 0
  %t462 = and i64 %t460, %t461
  store i64 %t462, ptr %t426
  %t463 = load i64, ptr %t426
  %t464 = call %NxVal @nx_int(i64 %t463)
  ret %NxVal %t464
}
define %NxVal @nx__m_4____main____Cell__m105(%NxVal* %args, i64 %nargs) {
entry:
  %t465 = alloca %NxVal
  %t469 = alloca i64
  %t484 = alloca i64
  store %NxVal zeroinitializer, ptr %t465
  %t466 = getelementptr %NxVal, ptr %args, i64 0
  %t467 = load %NxVal, ptr %t466
  %t468 = call %NxVal @nx_clone(%NxVal %t467)
  store %NxVal %t468, ptr %t465
  %t470 = getelementptr %NxVal, ptr %args, i64 1
  %t471 = load %NxVal, ptr %t470
  %t472 = extractvalue %NxVal %t471, 1
  store i64 %t472, ptr %t469
  %t473 = load %NxVal, ptr %t465
  %t474 = call %NxVal @nx_rec_get(%NxVal %t473, i64 0)
  %t475 = load %NxVal, ptr %t465
  %t476 = call %NxVal @nx_rec_get(%NxVal %t475, i64 1)
  %t477 = call %NxVal @nx_add(%NxVal %t474, %NxVal %t476)
  %t478 = load i64, ptr %t469
  %t479 = call %NxVal @nx_int(i64 %t478)
  %t480 = call %NxVal @nx_add(%NxVal %t477, %NxVal %t479)
  %t481 = add i64 65535, 0
  %t482 = call %NxVal @nx_int(i64 %t481)
  %t483 = call %NxVal @nx_bitand(%NxVal %t480, %NxVal %t482)
  %t485 = extractvalue %NxVal %t483, 1
  store i64 %t485, ptr %t484
  %t486 = load i64, ptr %t484
  %t487 = add i64 38, 0
  %t488 = call i64 @nx_mod_i64(i64 %t486, i64 %t487)
  %t489 = add i64 71, 0
  %t490 = call i64 @nx_mod_i64(i64 %t488, i64 %t489)
  %t491 = add i64 65535, 0
  %t492 = and i64 %t490, %t491
  store i64 %t492, ptr %t484
  %t493 = load i64, ptr %t484
  %t494 = add i64 5, 0
  %t495 = and i64 %t493, %t494
  %t496 = add i64 21, 0
  %t497 = and i64 %t495, %t496
  %t498 = add i64 65535, 0
  %t499 = and i64 %t497, %t498
  store i64 %t499, ptr %t484
  %t500 = load i64, ptr %t484
  %t501 = add i64 91, 0
  %t502 = or i64 %t500, %t501
  %t503 = add i64 50, 0
  %t504 = or i64 %t502, %t503
  %t505 = add i64 65535, 0
  %t506 = and i64 %t504, %t505
  store i64 %t506, ptr %t484
  %t507 = load i64, ptr %t484
  %t508 = add i64 30, 0
  %t509 = call i64 @nx_mod_i64(i64 %t507, i64 %t508)
  %t510 = add i64 4, 0
  %t511 = call i64 @nx_mod_i64(i64 %t509, i64 %t510)
  %t512 = add i64 65535, 0
  %t513 = and i64 %t511, %t512
  store i64 %t513, ptr %t484
  %t514 = load i64, ptr %t484
  %t515 = add i64 44, 0
  %t516 = add i64 %t514, %t515
  %t517 = add i64 50, 0
  %t518 = add i64 %t516, %t517
  %t519 = add i64 65535, 0
  %t520 = and i64 %t518, %t519
  store i64 %t520, ptr %t484
  %t521 = load i64, ptr %t484
  %t522 = call %NxVal @nx_int(i64 %t521)
  ret %NxVal %t522
}
define %NxVal @nx__m_4____main____Cell__m106(%NxVal* %args, i64 %nargs) {
entry:
  %t523 = alloca %NxVal
  %t527 = alloca i64
  %t542 = alloca i64
  store %NxVal zeroinitializer, ptr %t523
  %t524 = getelementptr %NxVal, ptr %args, i64 0
  %t525 = load %NxVal, ptr %t524
  %t526 = call %NxVal @nx_clone(%NxVal %t525)
  store %NxVal %t526, ptr %t523
  %t528 = getelementptr %NxVal, ptr %args, i64 1
  %t529 = load %NxVal, ptr %t528
  %t530 = extractvalue %NxVal %t529, 1
  store i64 %t530, ptr %t527
  %t531 = load %NxVal, ptr %t523
  %t532 = call %NxVal @nx_rec_get(%NxVal %t531, i64 0)
  %t533 = load %NxVal, ptr %t523
  %t534 = call %NxVal @nx_rec_get(%NxVal %t533, i64 1)
  %t535 = call %NxVal @nx_add(%NxVal %t532, %NxVal %t534)
  %t536 = load i64, ptr %t527
  %t537 = call %NxVal @nx_int(i64 %t536)
  %t538 = call %NxVal @nx_add(%NxVal %t535, %NxVal %t537)
  %t539 = add i64 65535, 0
  %t540 = call %NxVal @nx_int(i64 %t539)
  %t541 = call %NxVal @nx_bitand(%NxVal %t538, %NxVal %t540)
  %t543 = extractvalue %NxVal %t541, 1
  store i64 %t543, ptr %t542
  %t544 = load i64, ptr %t542
  %t545 = add i64 22, 0
  %t546 = sub i64 %t544, %t545
  %t547 = add i64 77, 0
  %t548 = sub i64 %t546, %t547
  %t549 = add i64 65535, 0
  %t550 = and i64 %t548, %t549
  store i64 %t550, ptr %t542
  %t551 = load i64, ptr %t542
  %t552 = add i64 62, 0
  %t553 = xor i64 %t551, %t552
  %t554 = add i64 2, 0
  %t555 = xor i64 %t553, %t554
  %t556 = add i64 65535, 0
  %t557 = and i64 %t555, %t556
  store i64 %t557, ptr %t542
  %t558 = load i64, ptr %t542
  %t559 = add i64 37, 0
  %t560 = add i64 %t558, %t559
  %t561 = add i64 18, 0
  %t562 = add i64 %t560, %t561
  %t563 = add i64 65535, 0
  %t564 = and i64 %t562, %t563
  store i64 %t564, ptr %t542
  %t565 = load i64, ptr %t542
  %t566 = add i64 36, 0
  %t567 = add i64 %t565, %t566
  %t568 = add i64 88, 0
  %t569 = add i64 %t567, %t568
  %t570 = add i64 65535, 0
  %t571 = and i64 %t569, %t570
  store i64 %t571, ptr %t542
  %t572 = load i64, ptr %t542
  %t573 = add i64 3, 0
  %t574 = and i64 %t572, %t573
  %t575 = add i64 20, 0
  %t576 = and i64 %t574, %t575
  %t577 = add i64 65535, 0
  %t578 = and i64 %t576, %t577
  store i64 %t578, ptr %t542
  %t579 = load i64, ptr %t542
  %t580 = call %NxVal @nx_int(i64 %t579)
  ret %NxVal %t580
}
define %NxVal @nx__m_4____main____Cell__m107(%NxVal* %args, i64 %nargs) {
entry:
  %t581 = alloca %NxVal
  %t585 = alloca i64
  %t600 = alloca i64
  store %NxVal zeroinitializer, ptr %t581
  %t582 = getelementptr %NxVal, ptr %args, i64 0
  %t583 = load %NxVal, ptr %t582
  %t584 = call %NxVal @nx_clone(%NxVal %t583)
  store %NxVal %t584, ptr %t581
  %t586 = getelementptr %NxVal, ptr %args, i64 1
  %t587 = load %NxVal, ptr %t586
  %t588 = extractvalue %NxVal %t587, 1
  store i64 %t588, ptr %t585
  %t589 = load %NxVal, ptr %t581
  %t590 = call %NxVal @nx_rec_get(%NxVal %t589, i64 0)
  %t591 = load %NxVal, ptr %t581
  %t592 = call %NxVal @nx_rec_get(%NxVal %t591, i64 1)
  %t593 = call %NxVal @nx_add(%NxVal %t590, %NxVal %t592)
  %t594 = load i64, ptr %t585
  %t595 = call %NxVal @nx_int(i64 %t594)
  %t596 = call %NxVal @nx_add(%NxVal %t593, %NxVal %t595)
  %t597 = add i64 65535, 0
  %t598 = call %NxVal @nx_int(i64 %t597)
  %t599 = call %NxVal @nx_bitand(%NxVal %t596, %NxVal %t598)
  %t601 = extractvalue %NxVal %t599, 1
  store i64 %t601, ptr %t600
  %t602 = load i64, ptr %t600
  %t603 = add i64 22, 0
  %t604 = and i64 %t602, %t603
  %t605 = add i64 36, 0
  %t606 = and i64 %t604, %t605
  %t607 = add i64 65535, 0
  %t608 = and i64 %t606, %t607
  store i64 %t608, ptr %t600
  %t609 = load i64, ptr %t600
  %t610 = add i64 25, 0
  %t611 = add i64 %t609, %t610
  %t612 = add i64 55, 0
  %t613 = add i64 %t611, %t612
  %t614 = add i64 65535, 0
  %t615 = and i64 %t613, %t614
  store i64 %t615, ptr %t600
  %t616 = load i64, ptr %t600
  %t617 = add i64 55, 0
  %t618 = and i64 %t616, %t617
  %t619 = add i64 2, 0
  %t620 = and i64 %t618, %t619
  %t621 = add i64 65535, 0
  %t622 = and i64 %t620, %t621
  store i64 %t622, ptr %t600
  %t623 = load i64, ptr %t600
  %t624 = add i64 18, 0
  %t625 = mul i64 %t623, %t624
  %t626 = add i64 50, 0
  %t627 = mul i64 %t625, %t626
  %t628 = add i64 65535, 0
  %t629 = and i64 %t627, %t628
  store i64 %t629, ptr %t600
  %t630 = load i64, ptr %t600
  %t631 = add i64 13, 0
  %t632 = call i64 @nx_mod_i64(i64 %t630, i64 %t631)
  %t633 = add i64 29, 0
  %t634 = call i64 @nx_mod_i64(i64 %t632, i64 %t633)
  %t635 = add i64 65535, 0
  %t636 = and i64 %t634, %t635
  store i64 %t636, ptr %t600
  %t637 = load i64, ptr %t600
  %t638 = call %NxVal @nx_int(i64 %t637)
  ret %NxVal %t638
}
define %NxVal @nx__m_4____main____Cell__m108(%NxVal* %args, i64 %nargs) {
entry:
  %t639 = alloca %NxVal
  %t643 = alloca i64
  %t658 = alloca i64
  store %NxVal zeroinitializer, ptr %t639
  %t640 = getelementptr %NxVal, ptr %args, i64 0
  %t641 = load %NxVal, ptr %t640
  %t642 = call %NxVal @nx_clone(%NxVal %t641)
  store %NxVal %t642, ptr %t639
  %t644 = getelementptr %NxVal, ptr %args, i64 1
  %t645 = load %NxVal, ptr %t644
  %t646 = extractvalue %NxVal %t645, 1
  store i64 %t646, ptr %t643
  %t647 = load %NxVal, ptr %t639
  %t648 = call %NxVal @nx_rec_get(%NxVal %t647, i64 0)
  %t649 = load %NxVal, ptr %t639
  %t650 = call %NxVal @nx_rec_get(%NxVal %t649, i64 1)
  %t651 = call %NxVal @nx_add(%NxVal %t648, %NxVal %t650)
  %t652 = load i64, ptr %t643
  %t653 = call %NxVal @nx_int(i64 %t652)
  %t654 = call %NxVal @nx_add(%NxVal %t651, %NxVal %t653)
  %t655 = add i64 65535, 0
  %t656 = call %NxVal @nx_int(i64 %t655)
  %t657 = call %NxVal @nx_bitand(%NxVal %t654, %NxVal %t656)
  %t659 = extractvalue %NxVal %t657, 1
  store i64 %t659, ptr %t658
  %t660 = load i64, ptr %t658
  %t661 = add i64 67, 0
  %t662 = or i64 %t660, %t661
  %t663 = add i64 11, 0
  %t664 = or i64 %t662, %t663
  %t665 = add i64 65535, 0
  %t666 = and i64 %t664, %t665
  store i64 %t666, ptr %t658
  %t667 = load i64, ptr %t658
  %t668 = add i64 22, 0
  %t669 = or i64 %t667, %t668
  %t670 = add i64 65, 0
  %t671 = or i64 %t669, %t670
  %t672 = add i64 65535, 0
  %t673 = and i64 %t671, %t672
  store i64 %t673, ptr %t658
  %t674 = load i64, ptr %t658
  %t675 = add i64 40, 0
  %t676 = sub i64 %t674, %t675
  %t677 = add i64 42, 0
  %t678 = sub i64 %t676, %t677
  %t679 = add i64 65535, 0
  %t680 = and i64 %t678, %t679
  store i64 %t680, ptr %t658
  %t681 = load i64, ptr %t658
  %t682 = add i64 22, 0
  %t683 = mul i64 %t681, %t682
  %t684 = add i64 69, 0
  %t685 = mul i64 %t683, %t684
  %t686 = add i64 65535, 0
  %t687 = and i64 %t685, %t686
  store i64 %t687, ptr %t658
  %t688 = load i64, ptr %t658
  %t689 = add i64 14, 0
  %t690 = sub i64 %t688, %t689
  %t691 = add i64 20, 0
  %t692 = sub i64 %t690, %t691
  %t693 = add i64 65535, 0
  %t694 = and i64 %t692, %t693
  store i64 %t694, ptr %t658
  %t695 = load i64, ptr %t658
  %t696 = call %NxVal @nx_int(i64 %t695)
  ret %NxVal %t696
}
define %NxVal @nx__m_4____main____Cell__m109(%NxVal* %args, i64 %nargs) {
entry:
  %t697 = alloca %NxVal
  %t701 = alloca i64
  %t716 = alloca i64
  store %NxVal zeroinitializer, ptr %t697
  %t698 = getelementptr %NxVal, ptr %args, i64 0
  %t699 = load %NxVal, ptr %t698
  %t700 = call %NxVal @nx_clone(%NxVal %t699)
  store %NxVal %t700, ptr %t697
  %t702 = getelementptr %NxVal, ptr %args, i64 1
  %t703 = load %NxVal, ptr %t702
  %t704 = extractvalue %NxVal %t703, 1
  store i64 %t704, ptr %t701
  %t705 = load %NxVal, ptr %t697
  %t706 = call %NxVal @nx_rec_get(%NxVal %t705, i64 0)
  %t707 = load %NxVal, ptr %t697
  %t708 = call %NxVal @nx_rec_get(%NxVal %t707, i64 1)
  %t709 = call %NxVal @nx_add(%NxVal %t706, %NxVal %t708)
  %t710 = load i64, ptr %t701
  %t711 = call %NxVal @nx_int(i64 %t710)
  %t712 = call %NxVal @nx_add(%NxVal %t709, %NxVal %t711)
  %t713 = add i64 65535, 0
  %t714 = call %NxVal @nx_int(i64 %t713)
  %t715 = call %NxVal @nx_bitand(%NxVal %t712, %NxVal %t714)
  %t717 = extractvalue %NxVal %t715, 1
  store i64 %t717, ptr %t716
  %t718 = load i64, ptr %t716
  %t719 = add i64 57, 0
  %t720 = mul i64 %t718, %t719
  %t721 = add i64 27, 0
  %t722 = mul i64 %t720, %t721
  %t723 = add i64 65535, 0
  %t724 = and i64 %t722, %t723
  store i64 %t724, ptr %t716
  %t725 = load i64, ptr %t716
  %t726 = add i64 71, 0
  %t727 = and i64 %t725, %t726
  %t728 = add i64 84, 0
  %t729 = and i64 %t727, %t728
  %t730 = add i64 65535, 0
  %t731 = and i64 %t729, %t730
  store i64 %t731, ptr %t716
  %t732 = load i64, ptr %t716
  %t733 = add i64 90, 0
  %t734 = mul i64 %t732, %t733
  %t735 = add i64 74, 0
  %t736 = mul i64 %t734, %t735
  %t737 = add i64 65535, 0
  %t738 = and i64 %t736, %t737
  store i64 %t738, ptr %t716
  %t739 = load i64, ptr %t716
  %t740 = add i64 71, 0
  %t741 = sub i64 %t739, %t740
  %t742 = add i64 27, 0
  %t743 = sub i64 %t741, %t742
  %t744 = add i64 65535, 0
  %t745 = and i64 %t743, %t744
  store i64 %t745, ptr %t716
  %t746 = load i64, ptr %t716
  %t747 = add i64 85, 0
  %t748 = add i64 %t746, %t747
  %t749 = add i64 57, 0
  %t750 = add i64 %t748, %t749
  %t751 = add i64 65535, 0
  %t752 = and i64 %t750, %t751
  store i64 %t752, ptr %t716
  %t753 = load i64, ptr %t716
  %t754 = call %NxVal @nx_int(i64 %t753)
  ret %NxVal %t754
}
define %NxVal @nx__m_3____main____Cell__m11(%NxVal* %args, i64 %nargs) {
entry:
  %t755 = alloca %NxVal
  %t759 = alloca i64
  %t774 = alloca i64
  store %NxVal zeroinitializer, ptr %t755
  %t756 = getelementptr %NxVal, ptr %args, i64 0
  %t757 = load %NxVal, ptr %t756
  %t758 = call %NxVal @nx_clone(%NxVal %t757)
  store %NxVal %t758, ptr %t755
  %t760 = getelementptr %NxVal, ptr %args, i64 1
  %t761 = load %NxVal, ptr %t760
  %t762 = extractvalue %NxVal %t761, 1
  store i64 %t762, ptr %t759
  %t763 = load %NxVal, ptr %t755
  %t764 = call %NxVal @nx_rec_get(%NxVal %t763, i64 0)
  %t765 = load %NxVal, ptr %t755
  %t766 = call %NxVal @nx_rec_get(%NxVal %t765, i64 1)
  %t767 = call %NxVal @nx_add(%NxVal %t764, %NxVal %t766)
  %t768 = load i64, ptr %t759
  %t769 = call %NxVal @nx_int(i64 %t768)
  %t770 = call %NxVal @nx_add(%NxVal %t767, %NxVal %t769)
  %t771 = add i64 65535, 0
  %t772 = call %NxVal @nx_int(i64 %t771)
  %t773 = call %NxVal @nx_bitand(%NxVal %t770, %NxVal %t772)
  %t775 = extractvalue %NxVal %t773, 1
  store i64 %t775, ptr %t774
  %t776 = load i64, ptr %t774
  %t777 = add i64 80, 0
  %t778 = add i64 %t776, %t777
  %t779 = add i64 75, 0
  %t780 = add i64 %t778, %t779
  %t781 = add i64 65535, 0
  %t782 = and i64 %t780, %t781
  store i64 %t782, ptr %t774
  %t783 = load i64, ptr %t774
  %t784 = add i64 10, 0
  %t785 = xor i64 %t783, %t784
  %t786 = add i64 28, 0
  %t787 = xor i64 %t785, %t786
  %t788 = add i64 65535, 0
  %t789 = and i64 %t787, %t788
  store i64 %t789, ptr %t774
  %t790 = load i64, ptr %t774
  %t791 = add i64 53, 0
  %t792 = add i64 %t790, %t791
  %t793 = add i64 10, 0
  %t794 = add i64 %t792, %t793
  %t795 = add i64 65535, 0
  %t796 = and i64 %t794, %t795
  store i64 %t796, ptr %t774
  %t797 = load i64, ptr %t774
  %t798 = add i64 59, 0
  %t799 = call i64 @nx_mod_i64(i64 %t797, i64 %t798)
  %t800 = add i64 3, 0
  %t801 = call i64 @nx_mod_i64(i64 %t799, i64 %t800)
  %t802 = add i64 65535, 0
  %t803 = and i64 %t801, %t802
  store i64 %t803, ptr %t774
  %t804 = load i64, ptr %t774
  %t805 = add i64 1, 0
  %t806 = add i64 %t804, %t805
  %t807 = add i64 19, 0
  %t808 = add i64 %t806, %t807
  %t809 = add i64 65535, 0
  %t810 = and i64 %t808, %t809
  store i64 %t810, ptr %t774
  %t811 = load i64, ptr %t774
  %t812 = call %NxVal @nx_int(i64 %t811)
  ret %NxVal %t812
}
define %NxVal @nx__m_4____main____Cell__m110(%NxVal* %args, i64 %nargs) {
entry:
  %t813 = alloca %NxVal
  %t817 = alloca i64
  %t832 = alloca i64
  store %NxVal zeroinitializer, ptr %t813
  %t814 = getelementptr %NxVal, ptr %args, i64 0
  %t815 = load %NxVal, ptr %t814
  %t816 = call %NxVal @nx_clone(%NxVal %t815)
  store %NxVal %t816, ptr %t813
  %t818 = getelementptr %NxVal, ptr %args, i64 1
  %t819 = load %NxVal, ptr %t818
  %t820 = extractvalue %NxVal %t819, 1
  store i64 %t820, ptr %t817
  %t821 = load %NxVal, ptr %t813
  %t822 = call %NxVal @nx_rec_get(%NxVal %t821, i64 0)
  %t823 = load %NxVal, ptr %t813
  %t824 = call %NxVal @nx_rec_get(%NxVal %t823, i64 1)
  %t825 = call %NxVal @nx_add(%NxVal %t822, %NxVal %t824)
  %t826 = load i64, ptr %t817
  %t827 = call %NxVal @nx_int(i64 %t826)
  %t828 = call %NxVal @nx_add(%NxVal %t825, %NxVal %t827)
  %t829 = add i64 65535, 0
  %t830 = call %NxVal @nx_int(i64 %t829)
  %t831 = call %NxVal @nx_bitand(%NxVal %t828, %NxVal %t830)
  %t833 = extractvalue %NxVal %t831, 1
  store i64 %t833, ptr %t832
  %t834 = load i64, ptr %t832
  %t835 = add i64 66, 0
  %t836 = add i64 %t834, %t835
  %t837 = add i64 50, 0
  %t838 = add i64 %t836, %t837
  %t839 = add i64 65535, 0
  %t840 = and i64 %t838, %t839
  store i64 %t840, ptr %t832
  %t841 = load i64, ptr %t832
  %t842 = add i64 58, 0
  %t843 = call i64 @nx_mod_i64(i64 %t841, i64 %t842)
  %t844 = add i64 27, 0
  %t845 = call i64 @nx_mod_i64(i64 %t843, i64 %t844)
  %t846 = add i64 65535, 0
  %t847 = and i64 %t845, %t846
  store i64 %t847, ptr %t832
  %t848 = load i64, ptr %t832
  %t849 = add i64 7, 0
  %t850 = or i64 %t848, %t849
  %t851 = add i64 27, 0
  %t852 = or i64 %t850, %t851
  %t853 = add i64 65535, 0
  %t854 = and i64 %t852, %t853
  store i64 %t854, ptr %t832
  %t855 = load i64, ptr %t832
  %t856 = add i64 32, 0
  %t857 = xor i64 %t855, %t856
  %t858 = add i64 31, 0
  %t859 = xor i64 %t857, %t858
  %t860 = add i64 65535, 0
  %t861 = and i64 %t859, %t860
  store i64 %t861, ptr %t832
  %t862 = load i64, ptr %t832
  %t863 = add i64 51, 0
  %t864 = call i64 @nx_mod_i64(i64 %t862, i64 %t863)
  %t865 = add i64 74, 0
  %t866 = call i64 @nx_mod_i64(i64 %t864, i64 %t865)
  %t867 = add i64 65535, 0
  %t868 = and i64 %t866, %t867
  store i64 %t868, ptr %t832
  %t869 = load i64, ptr %t832
  %t870 = call %NxVal @nx_int(i64 %t869)
  ret %NxVal %t870
}
define %NxVal @nx__m_4____main____Cell__m111(%NxVal* %args, i64 %nargs) {
entry:
  %t871 = alloca %NxVal
  %t875 = alloca i64
  %t890 = alloca i64
  store %NxVal zeroinitializer, ptr %t871
  %t872 = getelementptr %NxVal, ptr %args, i64 0
  %t873 = load %NxVal, ptr %t872
  %t874 = call %NxVal @nx_clone(%NxVal %t873)
  store %NxVal %t874, ptr %t871
  %t876 = getelementptr %NxVal, ptr %args, i64 1
  %t877 = load %NxVal, ptr %t876
  %t878 = extractvalue %NxVal %t877, 1
  store i64 %t878, ptr %t875
  %t879 = load %NxVal, ptr %t871
  %t880 = call %NxVal @nx_rec_get(%NxVal %t879, i64 0)
  %t881 = load %NxVal, ptr %t871
  %t882 = call %NxVal @nx_rec_get(%NxVal %t881, i64 1)
  %t883 = call %NxVal @nx_add(%NxVal %t880, %NxVal %t882)
  %t884 = load i64, ptr %t875
  %t885 = call %NxVal @nx_int(i64 %t884)
  %t886 = call %NxVal @nx_add(%NxVal %t883, %NxVal %t885)
  %t887 = add i64 65535, 0
  %t888 = call %NxVal @nx_int(i64 %t887)
  %t889 = call %NxVal @nx_bitand(%NxVal %t886, %NxVal %t888)
  %t891 = extractvalue %NxVal %t889, 1
  store i64 %t891, ptr %t890
  %t892 = load i64, ptr %t890
  %t893 = add i64 85, 0
  %t894 = add i64 %t892, %t893
  %t895 = add i64 62, 0
  %t896 = add i64 %t894, %t895
  %t897 = add i64 65535, 0
  %t898 = and i64 %t896, %t897
  store i64 %t898, ptr %t890
  %t899 = load i64, ptr %t890
  %t900 = add i64 32, 0
  %t901 = call i64 @nx_mod_i64(i64 %t899, i64 %t900)
  %t902 = add i64 25, 0
  %t903 = call i64 @nx_mod_i64(i64 %t901, i64 %t902)
  %t904 = add i64 65535, 0
  %t905 = and i64 %t903, %t904
  store i64 %t905, ptr %t890
  %t906 = load i64, ptr %t890
  %t907 = add i64 91, 0
  %t908 = sub i64 %t906, %t907
  %t909 = add i64 37, 0
  %t910 = sub i64 %t908, %t909
  %t911 = add i64 65535, 0
  %t912 = and i64 %t910, %t911
  store i64 %t912, ptr %t890
  %t913 = load i64, ptr %t890
  %t914 = add i64 63, 0
  %t915 = mul i64 %t913, %t914
  %t916 = add i64 11, 0
  %t917 = mul i64 %t915, %t916
  %t918 = add i64 65535, 0
  %t919 = and i64 %t917, %t918
  store i64 %t919, ptr %t890
  %t920 = load i64, ptr %t890
  %t921 = add i64 39, 0
  %t922 = xor i64 %t920, %t921
  %t923 = add i64 84, 0
  %t924 = xor i64 %t922, %t923
  %t925 = add i64 65535, 0
  %t926 = and i64 %t924, %t925
  store i64 %t926, ptr %t890
  %t927 = load i64, ptr %t890
  %t928 = call %NxVal @nx_int(i64 %t927)
  ret %NxVal %t928
}
define %NxVal @nx__m_4____main____Cell__m112(%NxVal* %args, i64 %nargs) {
entry:
  %t929 = alloca %NxVal
  %t933 = alloca i64
  %t948 = alloca i64
  store %NxVal zeroinitializer, ptr %t929
  %t930 = getelementptr %NxVal, ptr %args, i64 0
  %t931 = load %NxVal, ptr %t930
  %t932 = call %NxVal @nx_clone(%NxVal %t931)
  store %NxVal %t932, ptr %t929
  %t934 = getelementptr %NxVal, ptr %args, i64 1
  %t935 = load %NxVal, ptr %t934
  %t936 = extractvalue %NxVal %t935, 1
  store i64 %t936, ptr %t933
  %t937 = load %NxVal, ptr %t929
  %t938 = call %NxVal @nx_rec_get(%NxVal %t937, i64 0)
  %t939 = load %NxVal, ptr %t929
  %t940 = call %NxVal @nx_rec_get(%NxVal %t939, i64 1)
  %t941 = call %NxVal @nx_add(%NxVal %t938, %NxVal %t940)
  %t942 = load i64, ptr %t933
  %t943 = call %NxVal @nx_int(i64 %t942)
  %t944 = call %NxVal @nx_add(%NxVal %t941, %NxVal %t943)
  %t945 = add i64 65535, 0
  %t946 = call %NxVal @nx_int(i64 %t945)
  %t947 = call %NxVal @nx_bitand(%NxVal %t944, %NxVal %t946)
  %t949 = extractvalue %NxVal %t947, 1
  store i64 %t949, ptr %t948
  %t950 = load i64, ptr %t948
  %t951 = add i64 61, 0
  %t952 = mul i64 %t950, %t951
  %t953 = add i64 76, 0
  %t954 = mul i64 %t952, %t953
  %t955 = add i64 65535, 0
  %t956 = and i64 %t954, %t955
  store i64 %t956, ptr %t948
  %t957 = load i64, ptr %t948
  %t958 = add i64 54, 0
  %t959 = call i64 @nx_mod_i64(i64 %t957, i64 %t958)
  %t960 = add i64 78, 0
  %t961 = call i64 @nx_mod_i64(i64 %t959, i64 %t960)
  %t962 = add i64 65535, 0
  %t963 = and i64 %t961, %t962
  store i64 %t963, ptr %t948
  %t964 = load i64, ptr %t948
  %t965 = add i64 21, 0
  %t966 = and i64 %t964, %t965
  %t967 = add i64 14, 0
  %t968 = and i64 %t966, %t967
  %t969 = add i64 65535, 0
  %t970 = and i64 %t968, %t969
  store i64 %t970, ptr %t948
  %t971 = load i64, ptr %t948
  %t972 = add i64 13, 0
  %t973 = call i64 @nx_mod_i64(i64 %t971, i64 %t972)
  %t974 = add i64 57, 0
  %t975 = call i64 @nx_mod_i64(i64 %t973, i64 %t974)
  %t976 = add i64 65535, 0
  %t977 = and i64 %t975, %t976
  store i64 %t977, ptr %t948
  %t978 = load i64, ptr %t948
  %t979 = add i64 40, 0
  %t980 = or i64 %t978, %t979
  %t981 = add i64 63, 0
  %t982 = or i64 %t980, %t981
  %t983 = add i64 65535, 0
  %t984 = and i64 %t982, %t983
  store i64 %t984, ptr %t948
  %t985 = load i64, ptr %t948
  %t986 = call %NxVal @nx_int(i64 %t985)
  ret %NxVal %t986
}
define %NxVal @nx__m_4____main____Cell__m113(%NxVal* %args, i64 %nargs) {
entry:
  %t987 = alloca %NxVal
  %t991 = alloca i64
  %t1006 = alloca i64
  store %NxVal zeroinitializer, ptr %t987
  %t988 = getelementptr %NxVal, ptr %args, i64 0
  %t989 = load %NxVal, ptr %t988
  %t990 = call %NxVal @nx_clone(%NxVal %t989)
  store %NxVal %t990, ptr %t987
  %t992 = getelementptr %NxVal, ptr %args, i64 1
  %t993 = load %NxVal, ptr %t992
  %t994 = extractvalue %NxVal %t993, 1
  store i64 %t994, ptr %t991
  %t995 = load %NxVal, ptr %t987
  %t996 = call %NxVal @nx_rec_get(%NxVal %t995, i64 0)
  %t997 = load %NxVal, ptr %t987
  %t998 = call %NxVal @nx_rec_get(%NxVal %t997, i64 1)
  %t999 = call %NxVal @nx_add(%NxVal %t996, %NxVal %t998)
  %t1000 = load i64, ptr %t991
  %t1001 = call %NxVal @nx_int(i64 %t1000)
  %t1002 = call %NxVal @nx_add(%NxVal %t999, %NxVal %t1001)
  %t1003 = add i64 65535, 0
  %t1004 = call %NxVal @nx_int(i64 %t1003)
  %t1005 = call %NxVal @nx_bitand(%NxVal %t1002, %NxVal %t1004)
  %t1007 = extractvalue %NxVal %t1005, 1
  store i64 %t1007, ptr %t1006
  %t1008 = load i64, ptr %t1006
  %t1009 = add i64 27, 0
  %t1010 = xor i64 %t1008, %t1009
  %t1011 = add i64 33, 0
  %t1012 = xor i64 %t1010, %t1011
  %t1013 = add i64 65535, 0
  %t1014 = and i64 %t1012, %t1013
  store i64 %t1014, ptr %t1006
  %t1015 = load i64, ptr %t1006
  %t1016 = add i64 33, 0
  %t1017 = call i64 @nx_mod_i64(i64 %t1015, i64 %t1016)
  %t1018 = add i64 41, 0
  %t1019 = call i64 @nx_mod_i64(i64 %t1017, i64 %t1018)
  %t1020 = add i64 65535, 0
  %t1021 = and i64 %t1019, %t1020
  store i64 %t1021, ptr %t1006
  %t1022 = load i64, ptr %t1006
  %t1023 = add i64 41, 0
  %t1024 = mul i64 %t1022, %t1023
  %t1025 = add i64 47, 0
  %t1026 = mul i64 %t1024, %t1025
  %t1027 = add i64 65535, 0
  %t1028 = and i64 %t1026, %t1027
  store i64 %t1028, ptr %t1006
  %t1029 = load i64, ptr %t1006
  %t1030 = add i64 95, 0
  %t1031 = xor i64 %t1029, %t1030
  %t1032 = add i64 51, 0
  %t1033 = xor i64 %t1031, %t1032
  %t1034 = add i64 65535, 0
  %t1035 = and i64 %t1033, %t1034
  store i64 %t1035, ptr %t1006
  %t1036 = load i64, ptr %t1006
  %t1037 = add i64 33, 0
  %t1038 = xor i64 %t1036, %t1037
  %t1039 = add i64 40, 0
  %t1040 = xor i64 %t1038, %t1039
  %t1041 = add i64 65535, 0
  %t1042 = and i64 %t1040, %t1041
  store i64 %t1042, ptr %t1006
  %t1043 = load i64, ptr %t1006
  %t1044 = call %NxVal @nx_int(i64 %t1043)
  ret %NxVal %t1044
}
define %NxVal @nx__m_4____main____Cell__m114(%NxVal* %args, i64 %nargs) {
entry:
  %t1045 = alloca %NxVal
  %t1049 = alloca i64
  %t1064 = alloca i64
  store %NxVal zeroinitializer, ptr %t1045
  %t1046 = getelementptr %NxVal, ptr %args, i64 0
  %t1047 = load %NxVal, ptr %t1046
  %t1048 = call %NxVal @nx_clone(%NxVal %t1047)
  store %NxVal %t1048, ptr %t1045
  %t1050 = getelementptr %NxVal, ptr %args, i64 1
  %t1051 = load %NxVal, ptr %t1050
  %t1052 = extractvalue %NxVal %t1051, 1
  store i64 %t1052, ptr %t1049
  %t1053 = load %NxVal, ptr %t1045
  %t1054 = call %NxVal @nx_rec_get(%NxVal %t1053, i64 0)
  %t1055 = load %NxVal, ptr %t1045
  %t1056 = call %NxVal @nx_rec_get(%NxVal %t1055, i64 1)
  %t1057 = call %NxVal @nx_add(%NxVal %t1054, %NxVal %t1056)
  %t1058 = load i64, ptr %t1049
  %t1059 = call %NxVal @nx_int(i64 %t1058)
  %t1060 = call %NxVal @nx_add(%NxVal %t1057, %NxVal %t1059)
  %t1061 = add i64 65535, 0
  %t1062 = call %NxVal @nx_int(i64 %t1061)
  %t1063 = call %NxVal @nx_bitand(%NxVal %t1060, %NxVal %t1062)
  %t1065 = extractvalue %NxVal %t1063, 1
  store i64 %t1065, ptr %t1064
  %t1066 = load i64, ptr %t1064
  %t1067 = add i64 79, 0
  %t1068 = mul i64 %t1066, %t1067
  %t1069 = add i64 62, 0
  %t1070 = mul i64 %t1068, %t1069
  %t1071 = add i64 65535, 0
  %t1072 = and i64 %t1070, %t1071
  store i64 %t1072, ptr %t1064
  %t1073 = load i64, ptr %t1064
  %t1074 = add i64 45, 0
  %t1075 = sub i64 %t1073, %t1074
  %t1076 = add i64 43, 0
  %t1077 = sub i64 %t1075, %t1076
  %t1078 = add i64 65535, 0
  %t1079 = and i64 %t1077, %t1078
  store i64 %t1079, ptr %t1064
  %t1080 = load i64, ptr %t1064
  %t1081 = add i64 6, 0
  %t1082 = sub i64 %t1080, %t1081
  %t1083 = add i64 45, 0
  %t1084 = sub i64 %t1082, %t1083
  %t1085 = add i64 65535, 0
  %t1086 = and i64 %t1084, %t1085
  store i64 %t1086, ptr %t1064
  %t1087 = load i64, ptr %t1064
  %t1088 = add i64 54, 0
  %t1089 = mul i64 %t1087, %t1088
  %t1090 = add i64 35, 0
  %t1091 = mul i64 %t1089, %t1090
  %t1092 = add i64 65535, 0
  %t1093 = and i64 %t1091, %t1092
  store i64 %t1093, ptr %t1064
  %t1094 = load i64, ptr %t1064
  %t1095 = add i64 8, 0
  %t1096 = sub i64 %t1094, %t1095
  %t1097 = add i64 44, 0
  %t1098 = sub i64 %t1096, %t1097
  %t1099 = add i64 65535, 0
  %t1100 = and i64 %t1098, %t1099
  store i64 %t1100, ptr %t1064
  %t1101 = load i64, ptr %t1064
  %t1102 = call %NxVal @nx_int(i64 %t1101)
  ret %NxVal %t1102
}
define %NxVal @nx__m_4____main____Cell__m115(%NxVal* %args, i64 %nargs) {
entry:
  %t1103 = alloca %NxVal
  %t1107 = alloca i64
  %t1122 = alloca i64
  store %NxVal zeroinitializer, ptr %t1103
  %t1104 = getelementptr %NxVal, ptr %args, i64 0
  %t1105 = load %NxVal, ptr %t1104
  %t1106 = call %NxVal @nx_clone(%NxVal %t1105)
  store %NxVal %t1106, ptr %t1103
  %t1108 = getelementptr %NxVal, ptr %args, i64 1
  %t1109 = load %NxVal, ptr %t1108
  %t1110 = extractvalue %NxVal %t1109, 1
  store i64 %t1110, ptr %t1107
  %t1111 = load %NxVal, ptr %t1103
  %t1112 = call %NxVal @nx_rec_get(%NxVal %t1111, i64 0)
  %t1113 = load %NxVal, ptr %t1103
  %t1114 = call %NxVal @nx_rec_get(%NxVal %t1113, i64 1)
  %t1115 = call %NxVal @nx_add(%NxVal %t1112, %NxVal %t1114)
  %t1116 = load i64, ptr %t1107
  %t1117 = call %NxVal @nx_int(i64 %t1116)
  %t1118 = call %NxVal @nx_add(%NxVal %t1115, %NxVal %t1117)
  %t1119 = add i64 65535, 0
  %t1120 = call %NxVal @nx_int(i64 %t1119)
  %t1121 = call %NxVal @nx_bitand(%NxVal %t1118, %NxVal %t1120)
  %t1123 = extractvalue %NxVal %t1121, 1
  store i64 %t1123, ptr %t1122
  %t1124 = load i64, ptr %t1122
  %t1125 = add i64 53, 0
  %t1126 = and i64 %t1124, %t1125
  %t1127 = add i64 71, 0
  %t1128 = and i64 %t1126, %t1127
  %t1129 = add i64 65535, 0
  %t1130 = and i64 %t1128, %t1129
  store i64 %t1130, ptr %t1122
  %t1131 = load i64, ptr %t1122
  %t1132 = add i64 21, 0
  %t1133 = call i64 @nx_mod_i64(i64 %t1131, i64 %t1132)
  %t1134 = add i64 7, 0
  %t1135 = call i64 @nx_mod_i64(i64 %t1133, i64 %t1134)
  %t1136 = add i64 65535, 0
  %t1137 = and i64 %t1135, %t1136
  store i64 %t1137, ptr %t1122
  %t1138 = load i64, ptr %t1122
  %t1139 = add i64 28, 0
  %t1140 = or i64 %t1138, %t1139
  %t1141 = add i64 8, 0
  %t1142 = or i64 %t1140, %t1141
  %t1143 = add i64 65535, 0
  %t1144 = and i64 %t1142, %t1143
  store i64 %t1144, ptr %t1122
  %t1145 = load i64, ptr %t1122
  %t1146 = add i64 50, 0
  %t1147 = or i64 %t1145, %t1146
  %t1148 = add i64 23, 0
  %t1149 = or i64 %t1147, %t1148
  %t1150 = add i64 65535, 0
  %t1151 = and i64 %t1149, %t1150
  store i64 %t1151, ptr %t1122
  %t1152 = load i64, ptr %t1122
  %t1153 = add i64 3, 0
  %t1154 = add i64 %t1152, %t1153
  %t1155 = add i64 50, 0
  %t1156 = add i64 %t1154, %t1155
  %t1157 = add i64 65535, 0
  %t1158 = and i64 %t1156, %t1157
  store i64 %t1158, ptr %t1122
  %t1159 = load i64, ptr %t1122
  %t1160 = call %NxVal @nx_int(i64 %t1159)
  ret %NxVal %t1160
}
define %NxVal @nx__m_4____main____Cell__m116(%NxVal* %args, i64 %nargs) {
entry:
  %t1161 = alloca %NxVal
  %t1165 = alloca i64
  %t1180 = alloca i64
  store %NxVal zeroinitializer, ptr %t1161
  %t1162 = getelementptr %NxVal, ptr %args, i64 0
  %t1163 = load %NxVal, ptr %t1162
  %t1164 = call %NxVal @nx_clone(%NxVal %t1163)
  store %NxVal %t1164, ptr %t1161
  %t1166 = getelementptr %NxVal, ptr %args, i64 1
  %t1167 = load %NxVal, ptr %t1166
  %t1168 = extractvalue %NxVal %t1167, 1
  store i64 %t1168, ptr %t1165
  %t1169 = load %NxVal, ptr %t1161
  %t1170 = call %NxVal @nx_rec_get(%NxVal %t1169, i64 0)
  %t1171 = load %NxVal, ptr %t1161
  %t1172 = call %NxVal @nx_rec_get(%NxVal %t1171, i64 1)
  %t1173 = call %NxVal @nx_add(%NxVal %t1170, %NxVal %t1172)
  %t1174 = load i64, ptr %t1165
  %t1175 = call %NxVal @nx_int(i64 %t1174)
  %t1176 = call %NxVal @nx_add(%NxVal %t1173, %NxVal %t1175)
  %t1177 = add i64 65535, 0
  %t1178 = call %NxVal @nx_int(i64 %t1177)
  %t1179 = call %NxVal @nx_bitand(%NxVal %t1176, %NxVal %t1178)
  %t1181 = extractvalue %NxVal %t1179, 1
  store i64 %t1181, ptr %t1180
  %t1182 = load i64, ptr %t1180
  %t1183 = add i64 12, 0
  %t1184 = sub i64 %t1182, %t1183
  %t1185 = add i64 25, 0
  %t1186 = sub i64 %t1184, %t1185
  %t1187 = add i64 65535, 0
  %t1188 = and i64 %t1186, %t1187
  store i64 %t1188, ptr %t1180
  %t1189 = load i64, ptr %t1180
  %t1190 = add i64 67, 0
  %t1191 = call i64 @nx_mod_i64(i64 %t1189, i64 %t1190)
  %t1192 = add i64 85, 0
  %t1193 = call i64 @nx_mod_i64(i64 %t1191, i64 %t1192)
  %t1194 = add i64 65535, 0
  %t1195 = and i64 %t1193, %t1194
  store i64 %t1195, ptr %t1180
  %t1196 = load i64, ptr %t1180
  %t1197 = add i64 79, 0
  %t1198 = call i64 @nx_mod_i64(i64 %t1196, i64 %t1197)
  %t1199 = add i64 4, 0
  %t1200 = call i64 @nx_mod_i64(i64 %t1198, i64 %t1199)
  %t1201 = add i64 65535, 0
  %t1202 = and i64 %t1200, %t1201
  store i64 %t1202, ptr %t1180
  %t1203 = load i64, ptr %t1180
  %t1204 = add i64 2, 0
  %t1205 = mul i64 %t1203, %t1204
  %t1206 = add i64 42, 0
  %t1207 = mul i64 %t1205, %t1206
  %t1208 = add i64 65535, 0
  %t1209 = and i64 %t1207, %t1208
  store i64 %t1209, ptr %t1180
  %t1210 = load i64, ptr %t1180
  %t1211 = add i64 38, 0
  %t1212 = or i64 %t1210, %t1211
  %t1213 = add i64 43, 0
  %t1214 = or i64 %t1212, %t1213
  %t1215 = add i64 65535, 0
  %t1216 = and i64 %t1214, %t1215
  store i64 %t1216, ptr %t1180
  %t1217 = load i64, ptr %t1180
  %t1218 = call %NxVal @nx_int(i64 %t1217)
  ret %NxVal %t1218
}
define %NxVal @nx__m_4____main____Cell__m117(%NxVal* %args, i64 %nargs) {
entry:
  %t1219 = alloca %NxVal
  %t1223 = alloca i64
  %t1238 = alloca i64
  store %NxVal zeroinitializer, ptr %t1219
  %t1220 = getelementptr %NxVal, ptr %args, i64 0
  %t1221 = load %NxVal, ptr %t1220
  %t1222 = call %NxVal @nx_clone(%NxVal %t1221)
  store %NxVal %t1222, ptr %t1219
  %t1224 = getelementptr %NxVal, ptr %args, i64 1
  %t1225 = load %NxVal, ptr %t1224
  %t1226 = extractvalue %NxVal %t1225, 1
  store i64 %t1226, ptr %t1223
  %t1227 = load %NxVal, ptr %t1219
  %t1228 = call %NxVal @nx_rec_get(%NxVal %t1227, i64 0)
  %t1229 = load %NxVal, ptr %t1219
  %t1230 = call %NxVal @nx_rec_get(%NxVal %t1229, i64 1)
  %t1231 = call %NxVal @nx_add(%NxVal %t1228, %NxVal %t1230)
  %t1232 = load i64, ptr %t1223
  %t1233 = call %NxVal @nx_int(i64 %t1232)
  %t1234 = call %NxVal @nx_add(%NxVal %t1231, %NxVal %t1233)
  %t1235 = add i64 65535, 0
  %t1236 = call %NxVal @nx_int(i64 %t1235)
  %t1237 = call %NxVal @nx_bitand(%NxVal %t1234, %NxVal %t1236)
  %t1239 = extractvalue %NxVal %t1237, 1
  store i64 %t1239, ptr %t1238
  %t1240 = load i64, ptr %t1238
  %t1241 = add i64 18, 0
  %t1242 = sub i64 %t1240, %t1241
  %t1243 = add i64 86, 0
  %t1244 = sub i64 %t1242, %t1243
  %t1245 = add i64 65535, 0
  %t1246 = and i64 %t1244, %t1245
  store i64 %t1246, ptr %t1238
  %t1247 = load i64, ptr %t1238
  %t1248 = add i64 93, 0
  %t1249 = or i64 %t1247, %t1248
  %t1250 = add i64 79, 0
  %t1251 = or i64 %t1249, %t1250
  %t1252 = add i64 65535, 0
  %t1253 = and i64 %t1251, %t1252
  store i64 %t1253, ptr %t1238
  %t1254 = load i64, ptr %t1238
  %t1255 = add i64 64, 0
  %t1256 = add i64 %t1254, %t1255
  %t1257 = add i64 48, 0
  %t1258 = add i64 %t1256, %t1257
  %t1259 = add i64 65535, 0
  %t1260 = and i64 %t1258, %t1259
  store i64 %t1260, ptr %t1238
  %t1261 = load i64, ptr %t1238
  %t1262 = add i64 12, 0
  %t1263 = mul i64 %t1261, %t1262
  %t1264 = add i64 77, 0
  %t1265 = mul i64 %t1263, %t1264
  %t1266 = add i64 65535, 0
  %t1267 = and i64 %t1265, %t1266
  store i64 %t1267, ptr %t1238
  %t1268 = load i64, ptr %t1238
  %t1269 = add i64 25, 0
  %t1270 = mul i64 %t1268, %t1269
  %t1271 = add i64 50, 0
  %t1272 = mul i64 %t1270, %t1271
  %t1273 = add i64 65535, 0
  %t1274 = and i64 %t1272, %t1273
  store i64 %t1274, ptr %t1238
  %t1275 = load i64, ptr %t1238
  %t1276 = call %NxVal @nx_int(i64 %t1275)
  ret %NxVal %t1276
}
define %NxVal @nx__m_4____main____Cell__m118(%NxVal* %args, i64 %nargs) {
entry:
  %t1277 = alloca %NxVal
  %t1281 = alloca i64
  %t1296 = alloca i64
  store %NxVal zeroinitializer, ptr %t1277
  %t1278 = getelementptr %NxVal, ptr %args, i64 0
  %t1279 = load %NxVal, ptr %t1278
  %t1280 = call %NxVal @nx_clone(%NxVal %t1279)
  store %NxVal %t1280, ptr %t1277
  %t1282 = getelementptr %NxVal, ptr %args, i64 1
  %t1283 = load %NxVal, ptr %t1282
  %t1284 = extractvalue %NxVal %t1283, 1
  store i64 %t1284, ptr %t1281
  %t1285 = load %NxVal, ptr %t1277
  %t1286 = call %NxVal @nx_rec_get(%NxVal %t1285, i64 0)
  %t1287 = load %NxVal, ptr %t1277
  %t1288 = call %NxVal @nx_rec_get(%NxVal %t1287, i64 1)
  %t1289 = call %NxVal @nx_add(%NxVal %t1286, %NxVal %t1288)
  %t1290 = load i64, ptr %t1281
  %t1291 = call %NxVal @nx_int(i64 %t1290)
  %t1292 = call %NxVal @nx_add(%NxVal %t1289, %NxVal %t1291)
  %t1293 = add i64 65535, 0
  %t1294 = call %NxVal @nx_int(i64 %t1293)
  %t1295 = call %NxVal @nx_bitand(%NxVal %t1292, %NxVal %t1294)
  %t1297 = extractvalue %NxVal %t1295, 1
  store i64 %t1297, ptr %t1296
  %t1298 = load i64, ptr %t1296
  %t1299 = add i64 3, 0
  %t1300 = xor i64 %t1298, %t1299
  %t1301 = add i64 25, 0
  %t1302 = xor i64 %t1300, %t1301
  %t1303 = add i64 65535, 0
  %t1304 = and i64 %t1302, %t1303
  store i64 %t1304, ptr %t1296
  %t1305 = load i64, ptr %t1296
  %t1306 = add i64 11, 0
  %t1307 = add i64 %t1305, %t1306
  %t1308 = add i64 77, 0
  %t1309 = add i64 %t1307, %t1308
  %t1310 = add i64 65535, 0
  %t1311 = and i64 %t1309, %t1310
  store i64 %t1311, ptr %t1296
  %t1312 = load i64, ptr %t1296
  %t1313 = add i64 18, 0
  %t1314 = call i64 @nx_mod_i64(i64 %t1312, i64 %t1313)
  %t1315 = add i64 6, 0
  %t1316 = call i64 @nx_mod_i64(i64 %t1314, i64 %t1315)
  %t1317 = add i64 65535, 0
  %t1318 = and i64 %t1316, %t1317
  store i64 %t1318, ptr %t1296
  %t1319 = load i64, ptr %t1296
  %t1320 = add i64 19, 0
  %t1321 = or i64 %t1319, %t1320
  %t1322 = add i64 40, 0
  %t1323 = or i64 %t1321, %t1322
  %t1324 = add i64 65535, 0
  %t1325 = and i64 %t1323, %t1324
  store i64 %t1325, ptr %t1296
  %t1326 = load i64, ptr %t1296
  %t1327 = add i64 86, 0
  %t1328 = and i64 %t1326, %t1327
  %t1329 = add i64 54, 0
  %t1330 = and i64 %t1328, %t1329
  %t1331 = add i64 65535, 0
  %t1332 = and i64 %t1330, %t1331
  store i64 %t1332, ptr %t1296
  %t1333 = load i64, ptr %t1296
  %t1334 = call %NxVal @nx_int(i64 %t1333)
  ret %NxVal %t1334
}
define %NxVal @nx__m_4____main____Cell__m119(%NxVal* %args, i64 %nargs) {
entry:
  %t1335 = alloca %NxVal
  %t1339 = alloca i64
  %t1354 = alloca i64
  store %NxVal zeroinitializer, ptr %t1335
  %t1336 = getelementptr %NxVal, ptr %args, i64 0
  %t1337 = load %NxVal, ptr %t1336
  %t1338 = call %NxVal @nx_clone(%NxVal %t1337)
  store %NxVal %t1338, ptr %t1335
  %t1340 = getelementptr %NxVal, ptr %args, i64 1
  %t1341 = load %NxVal, ptr %t1340
  %t1342 = extractvalue %NxVal %t1341, 1
  store i64 %t1342, ptr %t1339
  %t1343 = load %NxVal, ptr %t1335
  %t1344 = call %NxVal @nx_rec_get(%NxVal %t1343, i64 0)
  %t1345 = load %NxVal, ptr %t1335
  %t1346 = call %NxVal @nx_rec_get(%NxVal %t1345, i64 1)
  %t1347 = call %NxVal @nx_add(%NxVal %t1344, %NxVal %t1346)
  %t1348 = load i64, ptr %t1339
  %t1349 = call %NxVal @nx_int(i64 %t1348)
  %t1350 = call %NxVal @nx_add(%NxVal %t1347, %NxVal %t1349)
  %t1351 = add i64 65535, 0
  %t1352 = call %NxVal @nx_int(i64 %t1351)
  %t1353 = call %NxVal @nx_bitand(%NxVal %t1350, %NxVal %t1352)
  %t1355 = extractvalue %NxVal %t1353, 1
  store i64 %t1355, ptr %t1354
  %t1356 = load i64, ptr %t1354
  %t1357 = add i64 45, 0
  %t1358 = call i64 @nx_mod_i64(i64 %t1356, i64 %t1357)
  %t1359 = add i64 48, 0
  %t1360 = call i64 @nx_mod_i64(i64 %t1358, i64 %t1359)
  %t1361 = add i64 65535, 0
  %t1362 = and i64 %t1360, %t1361
  store i64 %t1362, ptr %t1354
  %t1363 = load i64, ptr %t1354
  %t1364 = add i64 72, 0
  %t1365 = add i64 %t1363, %t1364
  %t1366 = add i64 39, 0
  %t1367 = add i64 %t1365, %t1366
  %t1368 = add i64 65535, 0
  %t1369 = and i64 %t1367, %t1368
  store i64 %t1369, ptr %t1354
  %t1370 = load i64, ptr %t1354
  %t1371 = add i64 31, 0
  %t1372 = add i64 %t1370, %t1371
  %t1373 = add i64 28, 0
  %t1374 = add i64 %t1372, %t1373
  %t1375 = add i64 65535, 0
  %t1376 = and i64 %t1374, %t1375
  store i64 %t1376, ptr %t1354
  %t1377 = load i64, ptr %t1354
  %t1378 = add i64 45, 0
  %t1379 = and i64 %t1377, %t1378
  %t1380 = add i64 29, 0
  %t1381 = and i64 %t1379, %t1380
  %t1382 = add i64 65535, 0
  %t1383 = and i64 %t1381, %t1382
  store i64 %t1383, ptr %t1354
  %t1384 = load i64, ptr %t1354
  %t1385 = add i64 74, 0
  %t1386 = sub i64 %t1384, %t1385
  %t1387 = add i64 32, 0
  %t1388 = sub i64 %t1386, %t1387
  %t1389 = add i64 65535, 0
  %t1390 = and i64 %t1388, %t1389
  store i64 %t1390, ptr %t1354
  %t1391 = load i64, ptr %t1354
  %t1392 = call %NxVal @nx_int(i64 %t1391)
  ret %NxVal %t1392
}
define %NxVal @nx__m_3____main____Cell__m12(%NxVal* %args, i64 %nargs) {
entry:
  %t1393 = alloca %NxVal
  %t1397 = alloca i64
  %t1412 = alloca i64
  store %NxVal zeroinitializer, ptr %t1393
  %t1394 = getelementptr %NxVal, ptr %args, i64 0
  %t1395 = load %NxVal, ptr %t1394
  %t1396 = call %NxVal @nx_clone(%NxVal %t1395)
  store %NxVal %t1396, ptr %t1393
  %t1398 = getelementptr %NxVal, ptr %args, i64 1
  %t1399 = load %NxVal, ptr %t1398
  %t1400 = extractvalue %NxVal %t1399, 1
  store i64 %t1400, ptr %t1397
  %t1401 = load %NxVal, ptr %t1393
  %t1402 = call %NxVal @nx_rec_get(%NxVal %t1401, i64 0)
  %t1403 = load %NxVal, ptr %t1393
  %t1404 = call %NxVal @nx_rec_get(%NxVal %t1403, i64 1)
  %t1405 = call %NxVal @nx_add(%NxVal %t1402, %NxVal %t1404)
  %t1406 = load i64, ptr %t1397
  %t1407 = call %NxVal @nx_int(i64 %t1406)
  %t1408 = call %NxVal @nx_add(%NxVal %t1405, %NxVal %t1407)
  %t1409 = add i64 65535, 0
  %t1410 = call %NxVal @nx_int(i64 %t1409)
  %t1411 = call %NxVal @nx_bitand(%NxVal %t1408, %NxVal %t1410)
  %t1413 = extractvalue %NxVal %t1411, 1
  store i64 %t1413, ptr %t1412
  %t1414 = load i64, ptr %t1412
  %t1415 = add i64 47, 0
  %t1416 = call i64 @nx_mod_i64(i64 %t1414, i64 %t1415)
  %t1417 = add i64 87, 0
  %t1418 = call i64 @nx_mod_i64(i64 %t1416, i64 %t1417)
  %t1419 = add i64 65535, 0
  %t1420 = and i64 %t1418, %t1419
  store i64 %t1420, ptr %t1412
  %t1421 = load i64, ptr %t1412
  %t1422 = add i64 80, 0
  %t1423 = call i64 @nx_mod_i64(i64 %t1421, i64 %t1422)
  %t1424 = add i64 28, 0
  %t1425 = call i64 @nx_mod_i64(i64 %t1423, i64 %t1424)
  %t1426 = add i64 65535, 0
  %t1427 = and i64 %t1425, %t1426
  store i64 %t1427, ptr %t1412
  %t1428 = load i64, ptr %t1412
  %t1429 = add i64 83, 0
  %t1430 = call i64 @nx_mod_i64(i64 %t1428, i64 %t1429)
  %t1431 = add i64 23, 0
  %t1432 = call i64 @nx_mod_i64(i64 %t1430, i64 %t1431)
  %t1433 = add i64 65535, 0
  %t1434 = and i64 %t1432, %t1433
  store i64 %t1434, ptr %t1412
  %t1435 = load i64, ptr %t1412
  %t1436 = add i64 88, 0
  %t1437 = or i64 %t1435, %t1436
  %t1438 = add i64 52, 0
  %t1439 = or i64 %t1437, %t1438
  %t1440 = add i64 65535, 0
  %t1441 = and i64 %t1439, %t1440
  store i64 %t1441, ptr %t1412
  %t1442 = load i64, ptr %t1412
  %t1443 = add i64 63, 0
  %t1444 = mul i64 %t1442, %t1443
  %t1445 = add i64 37, 0
  %t1446 = mul i64 %t1444, %t1445
  %t1447 = add i64 65535, 0
  %t1448 = and i64 %t1446, %t1447
  store i64 %t1448, ptr %t1412
  %t1449 = load i64, ptr %t1412
  %t1450 = call %NxVal @nx_int(i64 %t1449)
  ret %NxVal %t1450
}
define %NxVal @nx__m_3____main____Cell__m13(%NxVal* %args, i64 %nargs) {
entry:
  %t1451 = alloca %NxVal
  %t1455 = alloca i64
  %t1470 = alloca i64
  store %NxVal zeroinitializer, ptr %t1451
  %t1452 = getelementptr %NxVal, ptr %args, i64 0
  %t1453 = load %NxVal, ptr %t1452
  %t1454 = call %NxVal @nx_clone(%NxVal %t1453)
  store %NxVal %t1454, ptr %t1451
  %t1456 = getelementptr %NxVal, ptr %args, i64 1
  %t1457 = load %NxVal, ptr %t1456
  %t1458 = extractvalue %NxVal %t1457, 1
  store i64 %t1458, ptr %t1455
  %t1459 = load %NxVal, ptr %t1451
  %t1460 = call %NxVal @nx_rec_get(%NxVal %t1459, i64 0)
  %t1461 = load %NxVal, ptr %t1451
  %t1462 = call %NxVal @nx_rec_get(%NxVal %t1461, i64 1)
  %t1463 = call %NxVal @nx_add(%NxVal %t1460, %NxVal %t1462)
  %t1464 = load i64, ptr %t1455
  %t1465 = call %NxVal @nx_int(i64 %t1464)
  %t1466 = call %NxVal @nx_add(%NxVal %t1463, %NxVal %t1465)
  %t1467 = add i64 65535, 0
  %t1468 = call %NxVal @nx_int(i64 %t1467)
  %t1469 = call %NxVal @nx_bitand(%NxVal %t1466, %NxVal %t1468)
  %t1471 = extractvalue %NxVal %t1469, 1
  store i64 %t1471, ptr %t1470
  %t1472 = load i64, ptr %t1470
  %t1473 = add i64 3, 0
  %t1474 = mul i64 %t1472, %t1473
  %t1475 = add i64 31, 0
  %t1476 = mul i64 %t1474, %t1475
  %t1477 = add i64 65535, 0
  %t1478 = and i64 %t1476, %t1477
  store i64 %t1478, ptr %t1470
  %t1479 = load i64, ptr %t1470
  %t1480 = add i64 13, 0
  %t1481 = xor i64 %t1479, %t1480
  %t1482 = add i64 11, 0
  %t1483 = xor i64 %t1481, %t1482
  %t1484 = add i64 65535, 0
  %t1485 = and i64 %t1483, %t1484
  store i64 %t1485, ptr %t1470
  %t1486 = load i64, ptr %t1470
  %t1487 = add i64 39, 0
  %t1488 = sub i64 %t1486, %t1487
  %t1489 = add i64 69, 0
  %t1490 = sub i64 %t1488, %t1489
  %t1491 = add i64 65535, 0
  %t1492 = and i64 %t1490, %t1491
  store i64 %t1492, ptr %t1470
  %t1493 = load i64, ptr %t1470
  %t1494 = add i64 30, 0
  %t1495 = add i64 %t1493, %t1494
  %t1496 = add i64 42, 0
  %t1497 = add i64 %t1495, %t1496
  %t1498 = add i64 65535, 0
  %t1499 = and i64 %t1497, %t1498
  store i64 %t1499, ptr %t1470
  %t1500 = load i64, ptr %t1470
  %t1501 = add i64 12, 0
  %t1502 = call i64 @nx_mod_i64(i64 %t1500, i64 %t1501)
  %t1503 = add i64 88, 0
  %t1504 = call i64 @nx_mod_i64(i64 %t1502, i64 %t1503)
  %t1505 = add i64 65535, 0
  %t1506 = and i64 %t1504, %t1505
  store i64 %t1506, ptr %t1470
  %t1507 = load i64, ptr %t1470
  %t1508 = call %NxVal @nx_int(i64 %t1507)
  ret %NxVal %t1508
}
define %NxVal @nx__m_3____main____Cell__m14(%NxVal* %args, i64 %nargs) {
entry:
  %t1509 = alloca %NxVal
  %t1513 = alloca i64
  %t1528 = alloca i64
  store %NxVal zeroinitializer, ptr %t1509
  %t1510 = getelementptr %NxVal, ptr %args, i64 0
  %t1511 = load %NxVal, ptr %t1510
  %t1512 = call %NxVal @nx_clone(%NxVal %t1511)
  store %NxVal %t1512, ptr %t1509
  %t1514 = getelementptr %NxVal, ptr %args, i64 1
  %t1515 = load %NxVal, ptr %t1514
  %t1516 = extractvalue %NxVal %t1515, 1
  store i64 %t1516, ptr %t1513
  %t1517 = load %NxVal, ptr %t1509
  %t1518 = call %NxVal @nx_rec_get(%NxVal %t1517, i64 0)
  %t1519 = load %NxVal, ptr %t1509
  %t1520 = call %NxVal @nx_rec_get(%NxVal %t1519, i64 1)
  %t1521 = call %NxVal @nx_add(%NxVal %t1518, %NxVal %t1520)
  %t1522 = load i64, ptr %t1513
  %t1523 = call %NxVal @nx_int(i64 %t1522)
  %t1524 = call %NxVal @nx_add(%NxVal %t1521, %NxVal %t1523)
  %t1525 = add i64 65535, 0
  %t1526 = call %NxVal @nx_int(i64 %t1525)
  %t1527 = call %NxVal @nx_bitand(%NxVal %t1524, %NxVal %t1526)
  %t1529 = extractvalue %NxVal %t1527, 1
  store i64 %t1529, ptr %t1528
  %t1530 = load i64, ptr %t1528
  %t1531 = add i64 37, 0
  %t1532 = add i64 %t1530, %t1531
  %t1533 = add i64 71, 0
  %t1534 = add i64 %t1532, %t1533
  %t1535 = add i64 65535, 0
  %t1536 = and i64 %t1534, %t1535
  store i64 %t1536, ptr %t1528
  %t1537 = load i64, ptr %t1528
  %t1538 = add i64 44, 0
  %t1539 = mul i64 %t1537, %t1538
  %t1540 = add i64 78, 0
  %t1541 = mul i64 %t1539, %t1540
  %t1542 = add i64 65535, 0
  %t1543 = and i64 %t1541, %t1542
  store i64 %t1543, ptr %t1528
  %t1544 = load i64, ptr %t1528
  %t1545 = add i64 40, 0
  %t1546 = call i64 @nx_mod_i64(i64 %t1544, i64 %t1545)
  %t1547 = add i64 71, 0
  %t1548 = call i64 @nx_mod_i64(i64 %t1546, i64 %t1547)
  %t1549 = add i64 65535, 0
  %t1550 = and i64 %t1548, %t1549
  store i64 %t1550, ptr %t1528
  %t1551 = load i64, ptr %t1528
  %t1552 = add i64 46, 0
  %t1553 = mul i64 %t1551, %t1552
  %t1554 = add i64 7, 0
  %t1555 = mul i64 %t1553, %t1554
  %t1556 = add i64 65535, 0
  %t1557 = and i64 %t1555, %t1556
  store i64 %t1557, ptr %t1528
  %t1558 = load i64, ptr %t1528
  %t1559 = add i64 67, 0
  %t1560 = add i64 %t1558, %t1559
  %t1561 = add i64 51, 0
  %t1562 = add i64 %t1560, %t1561
  %t1563 = add i64 65535, 0
  %t1564 = and i64 %t1562, %t1563
  store i64 %t1564, ptr %t1528
  %t1565 = load i64, ptr %t1528
  %t1566 = call %NxVal @nx_int(i64 %t1565)
  ret %NxVal %t1566
}
define %NxVal @nx__m_3____main____Cell__m15(%NxVal* %args, i64 %nargs) {
entry:
  %t1567 = alloca %NxVal
  %t1571 = alloca i64
  %t1586 = alloca i64
  store %NxVal zeroinitializer, ptr %t1567
  %t1568 = getelementptr %NxVal, ptr %args, i64 0
  %t1569 = load %NxVal, ptr %t1568
  %t1570 = call %NxVal @nx_clone(%NxVal %t1569)
  store %NxVal %t1570, ptr %t1567
  %t1572 = getelementptr %NxVal, ptr %args, i64 1
  %t1573 = load %NxVal, ptr %t1572
  %t1574 = extractvalue %NxVal %t1573, 1
  store i64 %t1574, ptr %t1571
  %t1575 = load %NxVal, ptr %t1567
  %t1576 = call %NxVal @nx_rec_get(%NxVal %t1575, i64 0)
  %t1577 = load %NxVal, ptr %t1567
  %t1578 = call %NxVal @nx_rec_get(%NxVal %t1577, i64 1)
  %t1579 = call %NxVal @nx_add(%NxVal %t1576, %NxVal %t1578)
  %t1580 = load i64, ptr %t1571
  %t1581 = call %NxVal @nx_int(i64 %t1580)
  %t1582 = call %NxVal @nx_add(%NxVal %t1579, %NxVal %t1581)
  %t1583 = add i64 65535, 0
  %t1584 = call %NxVal @nx_int(i64 %t1583)
  %t1585 = call %NxVal @nx_bitand(%NxVal %t1582, %NxVal %t1584)
  %t1587 = extractvalue %NxVal %t1585, 1
  store i64 %t1587, ptr %t1586
  %t1588 = load i64, ptr %t1586
  %t1589 = add i64 3, 0
  %t1590 = sub i64 %t1588, %t1589
  %t1591 = add i64 55, 0
  %t1592 = sub i64 %t1590, %t1591
  %t1593 = add i64 65535, 0
  %t1594 = and i64 %t1592, %t1593
  store i64 %t1594, ptr %t1586
  %t1595 = load i64, ptr %t1586
  %t1596 = add i64 82, 0
  %t1597 = and i64 %t1595, %t1596
  %t1598 = add i64 62, 0
  %t1599 = and i64 %t1597, %t1598
  %t1600 = add i64 65535, 0
  %t1601 = and i64 %t1599, %t1600
  store i64 %t1601, ptr %t1586
  %t1602 = load i64, ptr %t1586
  %t1603 = add i64 78, 0
  %t1604 = mul i64 %t1602, %t1603
  %t1605 = add i64 89, 0
  %t1606 = mul i64 %t1604, %t1605
  %t1607 = add i64 65535, 0
  %t1608 = and i64 %t1606, %t1607
  store i64 %t1608, ptr %t1586
  %t1609 = load i64, ptr %t1586
  %t1610 = add i64 22, 0
  %t1611 = add i64 %t1609, %t1610
  %t1612 = add i64 78, 0
  %t1613 = add i64 %t1611, %t1612
  %t1614 = add i64 65535, 0
  %t1615 = and i64 %t1613, %t1614
  store i64 %t1615, ptr %t1586
  %t1616 = load i64, ptr %t1586
  %t1617 = add i64 70, 0
  %t1618 = xor i64 %t1616, %t1617
  %t1619 = add i64 58, 0
  %t1620 = xor i64 %t1618, %t1619
  %t1621 = add i64 65535, 0
  %t1622 = and i64 %t1620, %t1621
  store i64 %t1622, ptr %t1586
  %t1623 = load i64, ptr %t1586
  %t1624 = call %NxVal @nx_int(i64 %t1623)
  ret %NxVal %t1624
}
define %NxVal @nx__m_3____main____Cell__m16(%NxVal* %args, i64 %nargs) {
entry:
  %t1625 = alloca %NxVal
  %t1629 = alloca i64
  %t1644 = alloca i64
  store %NxVal zeroinitializer, ptr %t1625
  %t1626 = getelementptr %NxVal, ptr %args, i64 0
  %t1627 = load %NxVal, ptr %t1626
  %t1628 = call %NxVal @nx_clone(%NxVal %t1627)
  store %NxVal %t1628, ptr %t1625
  %t1630 = getelementptr %NxVal, ptr %args, i64 1
  %t1631 = load %NxVal, ptr %t1630
  %t1632 = extractvalue %NxVal %t1631, 1
  store i64 %t1632, ptr %t1629
  %t1633 = load %NxVal, ptr %t1625
  %t1634 = call %NxVal @nx_rec_get(%NxVal %t1633, i64 0)
  %t1635 = load %NxVal, ptr %t1625
  %t1636 = call %NxVal @nx_rec_get(%NxVal %t1635, i64 1)
  %t1637 = call %NxVal @nx_add(%NxVal %t1634, %NxVal %t1636)
  %t1638 = load i64, ptr %t1629
  %t1639 = call %NxVal @nx_int(i64 %t1638)
  %t1640 = call %NxVal @nx_add(%NxVal %t1637, %NxVal %t1639)
  %t1641 = add i64 65535, 0
  %t1642 = call %NxVal @nx_int(i64 %t1641)
  %t1643 = call %NxVal @nx_bitand(%NxVal %t1640, %NxVal %t1642)
  %t1645 = extractvalue %NxVal %t1643, 1
  store i64 %t1645, ptr %t1644
  %t1646 = load i64, ptr %t1644
  %t1647 = add i64 76, 0
  %t1648 = sub i64 %t1646, %t1647
  %t1649 = add i64 31, 0
  %t1650 = sub i64 %t1648, %t1649
  %t1651 = add i64 65535, 0
  %t1652 = and i64 %t1650, %t1651
  store i64 %t1652, ptr %t1644
  %t1653 = load i64, ptr %t1644
  %t1654 = add i64 50, 0
  %t1655 = xor i64 %t1653, %t1654
  %t1656 = add i64 82, 0
  %t1657 = xor i64 %t1655, %t1656
  %t1658 = add i64 65535, 0
  %t1659 = and i64 %t1657, %t1658
  store i64 %t1659, ptr %t1644
  %t1660 = load i64, ptr %t1644
  %t1661 = add i64 54, 0
  %t1662 = xor i64 %t1660, %t1661
  %t1663 = add i64 16, 0
  %t1664 = xor i64 %t1662, %t1663
  %t1665 = add i64 65535, 0
  %t1666 = and i64 %t1664, %t1665
  store i64 %t1666, ptr %t1644
  %t1667 = load i64, ptr %t1644
  %t1668 = add i64 21, 0
  %t1669 = call i64 @nx_mod_i64(i64 %t1667, i64 %t1668)
  %t1670 = add i64 71, 0
  %t1671 = call i64 @nx_mod_i64(i64 %t1669, i64 %t1670)
  %t1672 = add i64 65535, 0
  %t1673 = and i64 %t1671, %t1672
  store i64 %t1673, ptr %t1644
  %t1674 = load i64, ptr %t1644
  %t1675 = add i64 89, 0
  %t1676 = and i64 %t1674, %t1675
  %t1677 = add i64 54, 0
  %t1678 = and i64 %t1676, %t1677
  %t1679 = add i64 65535, 0
  %t1680 = and i64 %t1678, %t1679
  store i64 %t1680, ptr %t1644
  %t1681 = load i64, ptr %t1644
  %t1682 = call %NxVal @nx_int(i64 %t1681)
  ret %NxVal %t1682
}
define %NxVal @nx__m_3____main____Cell__m17(%NxVal* %args, i64 %nargs) {
entry:
  %t1683 = alloca %NxVal
  %t1687 = alloca i64
  %t1702 = alloca i64
  store %NxVal zeroinitializer, ptr %t1683
  %t1684 = getelementptr %NxVal, ptr %args, i64 0
  %t1685 = load %NxVal, ptr %t1684
  %t1686 = call %NxVal @nx_clone(%NxVal %t1685)
  store %NxVal %t1686, ptr %t1683
  %t1688 = getelementptr %NxVal, ptr %args, i64 1
  %t1689 = load %NxVal, ptr %t1688
  %t1690 = extractvalue %NxVal %t1689, 1
  store i64 %t1690, ptr %t1687
  %t1691 = load %NxVal, ptr %t1683
  %t1692 = call %NxVal @nx_rec_get(%NxVal %t1691, i64 0)
  %t1693 = load %NxVal, ptr %t1683
  %t1694 = call %NxVal @nx_rec_get(%NxVal %t1693, i64 1)
  %t1695 = call %NxVal @nx_add(%NxVal %t1692, %NxVal %t1694)
  %t1696 = load i64, ptr %t1687
  %t1697 = call %NxVal @nx_int(i64 %t1696)
  %t1698 = call %NxVal @nx_add(%NxVal %t1695, %NxVal %t1697)
  %t1699 = add i64 65535, 0
  %t1700 = call %NxVal @nx_int(i64 %t1699)
  %t1701 = call %NxVal @nx_bitand(%NxVal %t1698, %NxVal %t1700)
  %t1703 = extractvalue %NxVal %t1701, 1
  store i64 %t1703, ptr %t1702
  %t1704 = load i64, ptr %t1702
  %t1705 = add i64 8, 0
  %t1706 = call i64 @nx_mod_i64(i64 %t1704, i64 %t1705)
  %t1707 = add i64 3, 0
  %t1708 = call i64 @nx_mod_i64(i64 %t1706, i64 %t1707)
  %t1709 = add i64 65535, 0
  %t1710 = and i64 %t1708, %t1709
  store i64 %t1710, ptr %t1702
  %t1711 = load i64, ptr %t1702
  %t1712 = add i64 9, 0
  %t1713 = call i64 @nx_mod_i64(i64 %t1711, i64 %t1712)
  %t1714 = add i64 56, 0
  %t1715 = call i64 @nx_mod_i64(i64 %t1713, i64 %t1714)
  %t1716 = add i64 65535, 0
  %t1717 = and i64 %t1715, %t1716
  store i64 %t1717, ptr %t1702
  %t1718 = load i64, ptr %t1702
  %t1719 = add i64 52, 0
  %t1720 = and i64 %t1718, %t1719
  %t1721 = add i64 40, 0
  %t1722 = and i64 %t1720, %t1721
  %t1723 = add i64 65535, 0
  %t1724 = and i64 %t1722, %t1723
  store i64 %t1724, ptr %t1702
  %t1725 = load i64, ptr %t1702
  %t1726 = add i64 11, 0
  %t1727 = sub i64 %t1725, %t1726
  %t1728 = add i64 66, 0
  %t1729 = sub i64 %t1727, %t1728
  %t1730 = add i64 65535, 0
  %t1731 = and i64 %t1729, %t1730
  store i64 %t1731, ptr %t1702
  %t1732 = load i64, ptr %t1702
  %t1733 = add i64 48, 0
  %t1734 = xor i64 %t1732, %t1733
  %t1735 = add i64 36, 0
  %t1736 = xor i64 %t1734, %t1735
  %t1737 = add i64 65535, 0
  %t1738 = and i64 %t1736, %t1737
  store i64 %t1738, ptr %t1702
  %t1739 = load i64, ptr %t1702
  %t1740 = call %NxVal @nx_int(i64 %t1739)
  ret %NxVal %t1740
}
define %NxVal @nx__m_3____main____Cell__m18(%NxVal* %args, i64 %nargs) {
entry:
  %t1741 = alloca %NxVal
  %t1745 = alloca i64
  %t1760 = alloca i64
  store %NxVal zeroinitializer, ptr %t1741
  %t1742 = getelementptr %NxVal, ptr %args, i64 0
  %t1743 = load %NxVal, ptr %t1742
  %t1744 = call %NxVal @nx_clone(%NxVal %t1743)
  store %NxVal %t1744, ptr %t1741
  %t1746 = getelementptr %NxVal, ptr %args, i64 1
  %t1747 = load %NxVal, ptr %t1746
  %t1748 = extractvalue %NxVal %t1747, 1
  store i64 %t1748, ptr %t1745
  %t1749 = load %NxVal, ptr %t1741
  %t1750 = call %NxVal @nx_rec_get(%NxVal %t1749, i64 0)
  %t1751 = load %NxVal, ptr %t1741
  %t1752 = call %NxVal @nx_rec_get(%NxVal %t1751, i64 1)
  %t1753 = call %NxVal @nx_add(%NxVal %t1750, %NxVal %t1752)
  %t1754 = load i64, ptr %t1745
  %t1755 = call %NxVal @nx_int(i64 %t1754)
  %t1756 = call %NxVal @nx_add(%NxVal %t1753, %NxVal %t1755)
  %t1757 = add i64 65535, 0
  %t1758 = call %NxVal @nx_int(i64 %t1757)
  %t1759 = call %NxVal @nx_bitand(%NxVal %t1756, %NxVal %t1758)
  %t1761 = extractvalue %NxVal %t1759, 1
  store i64 %t1761, ptr %t1760
  %t1762 = load i64, ptr %t1760
  %t1763 = add i64 18, 0
  %t1764 = or i64 %t1762, %t1763
  %t1765 = add i64 73, 0
  %t1766 = or i64 %t1764, %t1765
  %t1767 = add i64 65535, 0
  %t1768 = and i64 %t1766, %t1767
  store i64 %t1768, ptr %t1760
  %t1769 = load i64, ptr %t1760
  %t1770 = add i64 3, 0
  %t1771 = call i64 @nx_mod_i64(i64 %t1769, i64 %t1770)
  %t1772 = add i64 48, 0
  %t1773 = call i64 @nx_mod_i64(i64 %t1771, i64 %t1772)
  %t1774 = add i64 65535, 0
  %t1775 = and i64 %t1773, %t1774
  store i64 %t1775, ptr %t1760
  %t1776 = load i64, ptr %t1760
  %t1777 = add i64 20, 0
  %t1778 = mul i64 %t1776, %t1777
  %t1779 = add i64 2, 0
  %t1780 = mul i64 %t1778, %t1779
  %t1781 = add i64 65535, 0
  %t1782 = and i64 %t1780, %t1781
  store i64 %t1782, ptr %t1760
  %t1783 = load i64, ptr %t1760
  %t1784 = add i64 81, 0
  %t1785 = add i64 %t1783, %t1784
  %t1786 = add i64 63, 0
  %t1787 = add i64 %t1785, %t1786
  %t1788 = add i64 65535, 0
  %t1789 = and i64 %t1787, %t1788
  store i64 %t1789, ptr %t1760
  %t1790 = load i64, ptr %t1760
  %t1791 = add i64 14, 0
  %t1792 = mul i64 %t1790, %t1791
  %t1793 = add i64 62, 0
  %t1794 = mul i64 %t1792, %t1793
  %t1795 = add i64 65535, 0
  %t1796 = and i64 %t1794, %t1795
  store i64 %t1796, ptr %t1760
  %t1797 = load i64, ptr %t1760
  %t1798 = call %NxVal @nx_int(i64 %t1797)
  ret %NxVal %t1798
}
define %NxVal @nx__m_3____main____Cell__m19(%NxVal* %args, i64 %nargs) {
entry:
  %t1799 = alloca %NxVal
  %t1803 = alloca i64
  %t1818 = alloca i64
  store %NxVal zeroinitializer, ptr %t1799
  %t1800 = getelementptr %NxVal, ptr %args, i64 0
  %t1801 = load %NxVal, ptr %t1800
  %t1802 = call %NxVal @nx_clone(%NxVal %t1801)
  store %NxVal %t1802, ptr %t1799
  %t1804 = getelementptr %NxVal, ptr %args, i64 1
  %t1805 = load %NxVal, ptr %t1804
  %t1806 = extractvalue %NxVal %t1805, 1
  store i64 %t1806, ptr %t1803
  %t1807 = load %NxVal, ptr %t1799
  %t1808 = call %NxVal @nx_rec_get(%NxVal %t1807, i64 0)
  %t1809 = load %NxVal, ptr %t1799
  %t1810 = call %NxVal @nx_rec_get(%NxVal %t1809, i64 1)
  %t1811 = call %NxVal @nx_add(%NxVal %t1808, %NxVal %t1810)
  %t1812 = load i64, ptr %t1803
  %t1813 = call %NxVal @nx_int(i64 %t1812)
  %t1814 = call %NxVal @nx_add(%NxVal %t1811, %NxVal %t1813)
  %t1815 = add i64 65535, 0
  %t1816 = call %NxVal @nx_int(i64 %t1815)
  %t1817 = call %NxVal @nx_bitand(%NxVal %t1814, %NxVal %t1816)
  %t1819 = extractvalue %NxVal %t1817, 1
  store i64 %t1819, ptr %t1818
  %t1820 = load i64, ptr %t1818
  %t1821 = add i64 36, 0
  %t1822 = sub i64 %t1820, %t1821
  %t1823 = add i64 45, 0
  %t1824 = sub i64 %t1822, %t1823
  %t1825 = add i64 65535, 0
  %t1826 = and i64 %t1824, %t1825
  store i64 %t1826, ptr %t1818
  %t1827 = load i64, ptr %t1818
  %t1828 = add i64 69, 0
  %t1829 = call i64 @nx_mod_i64(i64 %t1827, i64 %t1828)
  %t1830 = add i64 60, 0
  %t1831 = call i64 @nx_mod_i64(i64 %t1829, i64 %t1830)
  %t1832 = add i64 65535, 0
  %t1833 = and i64 %t1831, %t1832
  store i64 %t1833, ptr %t1818
  %t1834 = load i64, ptr %t1818
  %t1835 = add i64 55, 0
  %t1836 = xor i64 %t1834, %t1835
  %t1837 = add i64 39, 0
  %t1838 = xor i64 %t1836, %t1837
  %t1839 = add i64 65535, 0
  %t1840 = and i64 %t1838, %t1839
  store i64 %t1840, ptr %t1818
  %t1841 = load i64, ptr %t1818
  %t1842 = add i64 17, 0
  %t1843 = add i64 %t1841, %t1842
  %t1844 = add i64 75, 0
  %t1845 = add i64 %t1843, %t1844
  %t1846 = add i64 65535, 0
  %t1847 = and i64 %t1845, %t1846
  store i64 %t1847, ptr %t1818
  %t1848 = load i64, ptr %t1818
  %t1849 = add i64 38, 0
  %t1850 = add i64 %t1848, %t1849
  %t1851 = add i64 77, 0
  %t1852 = add i64 %t1850, %t1851
  %t1853 = add i64 65535, 0
  %t1854 = and i64 %t1852, %t1853
  store i64 %t1854, ptr %t1818
  %t1855 = load i64, ptr %t1818
  %t1856 = call %NxVal @nx_int(i64 %t1855)
  ret %NxVal %t1856
}
define %NxVal @nx__m_2____main____Cell__m2(%NxVal* %args, i64 %nargs) {
entry:
  %t1857 = alloca %NxVal
  %t1861 = alloca i64
  %t1876 = alloca i64
  store %NxVal zeroinitializer, ptr %t1857
  %t1858 = getelementptr %NxVal, ptr %args, i64 0
  %t1859 = load %NxVal, ptr %t1858
  %t1860 = call %NxVal @nx_clone(%NxVal %t1859)
  store %NxVal %t1860, ptr %t1857
  %t1862 = getelementptr %NxVal, ptr %args, i64 1
  %t1863 = load %NxVal, ptr %t1862
  %t1864 = extractvalue %NxVal %t1863, 1
  store i64 %t1864, ptr %t1861
  %t1865 = load %NxVal, ptr %t1857
  %t1866 = call %NxVal @nx_rec_get(%NxVal %t1865, i64 0)
  %t1867 = load %NxVal, ptr %t1857
  %t1868 = call %NxVal @nx_rec_get(%NxVal %t1867, i64 1)
  %t1869 = call %NxVal @nx_add(%NxVal %t1866, %NxVal %t1868)
  %t1870 = load i64, ptr %t1861
  %t1871 = call %NxVal @nx_int(i64 %t1870)
  %t1872 = call %NxVal @nx_add(%NxVal %t1869, %NxVal %t1871)
  %t1873 = add i64 65535, 0
  %t1874 = call %NxVal @nx_int(i64 %t1873)
  %t1875 = call %NxVal @nx_bitand(%NxVal %t1872, %NxVal %t1874)
  %t1877 = extractvalue %NxVal %t1875, 1
  store i64 %t1877, ptr %t1876
  %t1878 = load i64, ptr %t1876
  %t1879 = add i64 88, 0
  %t1880 = sub i64 %t1878, %t1879
  %t1881 = add i64 76, 0
  %t1882 = sub i64 %t1880, %t1881
  %t1883 = add i64 65535, 0
  %t1884 = and i64 %t1882, %t1883
  store i64 %t1884, ptr %t1876
  %t1885 = load i64, ptr %t1876
  %t1886 = add i64 46, 0
  %t1887 = or i64 %t1885, %t1886
  %t1888 = add i64 64, 0
  %t1889 = or i64 %t1887, %t1888
  %t1890 = add i64 65535, 0
  %t1891 = and i64 %t1889, %t1890
  store i64 %t1891, ptr %t1876
  %t1892 = load i64, ptr %t1876
  %t1893 = add i64 10, 0
  %t1894 = xor i64 %t1892, %t1893
  %t1895 = add i64 47, 0
  %t1896 = xor i64 %t1894, %t1895
  %t1897 = add i64 65535, 0
  %t1898 = and i64 %t1896, %t1897
  store i64 %t1898, ptr %t1876
  %t1899 = load i64, ptr %t1876
  %t1900 = add i64 37, 0
  %t1901 = and i64 %t1899, %t1900
  %t1902 = add i64 15, 0
  %t1903 = and i64 %t1901, %t1902
  %t1904 = add i64 65535, 0
  %t1905 = and i64 %t1903, %t1904
  store i64 %t1905, ptr %t1876
  %t1906 = load i64, ptr %t1876
  %t1907 = add i64 22, 0
  %t1908 = sub i64 %t1906, %t1907
  %t1909 = add i64 85, 0
  %t1910 = sub i64 %t1908, %t1909
  %t1911 = add i64 65535, 0
  %t1912 = and i64 %t1910, %t1911
  store i64 %t1912, ptr %t1876
  %t1913 = load i64, ptr %t1876
  %t1914 = call %NxVal @nx_int(i64 %t1913)
  ret %NxVal %t1914
}
define %NxVal @nx__m_3____main____Cell__m20(%NxVal* %args, i64 %nargs) {
entry:
  %t1915 = alloca %NxVal
  %t1919 = alloca i64
  %t1934 = alloca i64
  store %NxVal zeroinitializer, ptr %t1915
  %t1916 = getelementptr %NxVal, ptr %args, i64 0
  %t1917 = load %NxVal, ptr %t1916
  %t1918 = call %NxVal @nx_clone(%NxVal %t1917)
  store %NxVal %t1918, ptr %t1915
  %t1920 = getelementptr %NxVal, ptr %args, i64 1
  %t1921 = load %NxVal, ptr %t1920
  %t1922 = extractvalue %NxVal %t1921, 1
  store i64 %t1922, ptr %t1919
  %t1923 = load %NxVal, ptr %t1915
  %t1924 = call %NxVal @nx_rec_get(%NxVal %t1923, i64 0)
  %t1925 = load %NxVal, ptr %t1915
  %t1926 = call %NxVal @nx_rec_get(%NxVal %t1925, i64 1)
  %t1927 = call %NxVal @nx_add(%NxVal %t1924, %NxVal %t1926)
  %t1928 = load i64, ptr %t1919
  %t1929 = call %NxVal @nx_int(i64 %t1928)
  %t1930 = call %NxVal @nx_add(%NxVal %t1927, %NxVal %t1929)
  %t1931 = add i64 65535, 0
  %t1932 = call %NxVal @nx_int(i64 %t1931)
  %t1933 = call %NxVal @nx_bitand(%NxVal %t1930, %NxVal %t1932)
  %t1935 = extractvalue %NxVal %t1933, 1
  store i64 %t1935, ptr %t1934
  %t1936 = load i64, ptr %t1934
  %t1937 = add i64 58, 0
  %t1938 = sub i64 %t1936, %t1937
  %t1939 = add i64 2, 0
  %t1940 = sub i64 %t1938, %t1939
  %t1941 = add i64 65535, 0
  %t1942 = and i64 %t1940, %t1941
  store i64 %t1942, ptr %t1934
  %t1943 = load i64, ptr %t1934
  %t1944 = add i64 27, 0
  %t1945 = call i64 @nx_mod_i64(i64 %t1943, i64 %t1944)
  %t1946 = add i64 73, 0
  %t1947 = call i64 @nx_mod_i64(i64 %t1945, i64 %t1946)
  %t1948 = add i64 65535, 0
  %t1949 = and i64 %t1947, %t1948
  store i64 %t1949, ptr %t1934
  %t1950 = load i64, ptr %t1934
  %t1951 = add i64 51, 0
  %t1952 = add i64 %t1950, %t1951
  %t1953 = add i64 40, 0
  %t1954 = add i64 %t1952, %t1953
  %t1955 = add i64 65535, 0
  %t1956 = and i64 %t1954, %t1955
  store i64 %t1956, ptr %t1934
  %t1957 = load i64, ptr %t1934
  %t1958 = add i64 29, 0
  %t1959 = sub i64 %t1957, %t1958
  %t1960 = add i64 61, 0
  %t1961 = sub i64 %t1959, %t1960
  %t1962 = add i64 65535, 0
  %t1963 = and i64 %t1961, %t1962
  store i64 %t1963, ptr %t1934
  %t1964 = load i64, ptr %t1934
  %t1965 = add i64 65, 0
  %t1966 = or i64 %t1964, %t1965
  %t1967 = add i64 34, 0
  %t1968 = or i64 %t1966, %t1967
  %t1969 = add i64 65535, 0
  %t1970 = and i64 %t1968, %t1969
  store i64 %t1970, ptr %t1934
  %t1971 = load i64, ptr %t1934
  %t1972 = call %NxVal @nx_int(i64 %t1971)
  ret %NxVal %t1972
}
define %NxVal @nx__m_3____main____Cell__m21(%NxVal* %args, i64 %nargs) {
entry:
  %t1973 = alloca %NxVal
  %t1977 = alloca i64
  %t1992 = alloca i64
  store %NxVal zeroinitializer, ptr %t1973
  %t1974 = getelementptr %NxVal, ptr %args, i64 0
  %t1975 = load %NxVal, ptr %t1974
  %t1976 = call %NxVal @nx_clone(%NxVal %t1975)
  store %NxVal %t1976, ptr %t1973
  %t1978 = getelementptr %NxVal, ptr %args, i64 1
  %t1979 = load %NxVal, ptr %t1978
  %t1980 = extractvalue %NxVal %t1979, 1
  store i64 %t1980, ptr %t1977
  %t1981 = load %NxVal, ptr %t1973
  %t1982 = call %NxVal @nx_rec_get(%NxVal %t1981, i64 0)
  %t1983 = load %NxVal, ptr %t1973
  %t1984 = call %NxVal @nx_rec_get(%NxVal %t1983, i64 1)
  %t1985 = call %NxVal @nx_add(%NxVal %t1982, %NxVal %t1984)
  %t1986 = load i64, ptr %t1977
  %t1987 = call %NxVal @nx_int(i64 %t1986)
  %t1988 = call %NxVal @nx_add(%NxVal %t1985, %NxVal %t1987)
  %t1989 = add i64 65535, 0
  %t1990 = call %NxVal @nx_int(i64 %t1989)
  %t1991 = call %NxVal @nx_bitand(%NxVal %t1988, %NxVal %t1990)
  %t1993 = extractvalue %NxVal %t1991, 1
  store i64 %t1993, ptr %t1992
  %t1994 = load i64, ptr %t1992
  %t1995 = add i64 6, 0
  %t1996 = or i64 %t1994, %t1995
  %t1997 = add i64 10, 0
  %t1998 = or i64 %t1996, %t1997
  %t1999 = add i64 65535, 0
  %t2000 = and i64 %t1998, %t1999
  store i64 %t2000, ptr %t1992
  %t2001 = load i64, ptr %t1992
  %t2002 = add i64 83, 0
  %t2003 = and i64 %t2001, %t2002
  %t2004 = add i64 11, 0
  %t2005 = and i64 %t2003, %t2004
  %t2006 = add i64 65535, 0
  %t2007 = and i64 %t2005, %t2006
  store i64 %t2007, ptr %t1992
  %t2008 = load i64, ptr %t1992
  %t2009 = add i64 51, 0
  %t2010 = sub i64 %t2008, %t2009
  %t2011 = add i64 29, 0
  %t2012 = sub i64 %t2010, %t2011
  %t2013 = add i64 65535, 0
  %t2014 = and i64 %t2012, %t2013
  store i64 %t2014, ptr %t1992
  %t2015 = load i64, ptr %t1992
  %t2016 = add i64 83, 0
  %t2017 = add i64 %t2015, %t2016
  %t2018 = add i64 52, 0
  %t2019 = add i64 %t2017, %t2018
  %t2020 = add i64 65535, 0
  %t2021 = and i64 %t2019, %t2020
  store i64 %t2021, ptr %t1992
  %t2022 = load i64, ptr %t1992
  %t2023 = add i64 14, 0
  %t2024 = mul i64 %t2022, %t2023
  %t2025 = add i64 50, 0
  %t2026 = mul i64 %t2024, %t2025
  %t2027 = add i64 65535, 0
  %t2028 = and i64 %t2026, %t2027
  store i64 %t2028, ptr %t1992
  %t2029 = load i64, ptr %t1992
  %t2030 = call %NxVal @nx_int(i64 %t2029)
  ret %NxVal %t2030
}
define %NxVal @nx__m_3____main____Cell__m22(%NxVal* %args, i64 %nargs) {
entry:
  %t2031 = alloca %NxVal
  %t2035 = alloca i64
  %t2050 = alloca i64
  store %NxVal zeroinitializer, ptr %t2031
  %t2032 = getelementptr %NxVal, ptr %args, i64 0
  %t2033 = load %NxVal, ptr %t2032
  %t2034 = call %NxVal @nx_clone(%NxVal %t2033)
  store %NxVal %t2034, ptr %t2031
  %t2036 = getelementptr %NxVal, ptr %args, i64 1
  %t2037 = load %NxVal, ptr %t2036
  %t2038 = extractvalue %NxVal %t2037, 1
  store i64 %t2038, ptr %t2035
  %t2039 = load %NxVal, ptr %t2031
  %t2040 = call %NxVal @nx_rec_get(%NxVal %t2039, i64 0)
  %t2041 = load %NxVal, ptr %t2031
  %t2042 = call %NxVal @nx_rec_get(%NxVal %t2041, i64 1)
  %t2043 = call %NxVal @nx_add(%NxVal %t2040, %NxVal %t2042)
  %t2044 = load i64, ptr %t2035
  %t2045 = call %NxVal @nx_int(i64 %t2044)
  %t2046 = call %NxVal @nx_add(%NxVal %t2043, %NxVal %t2045)
  %t2047 = add i64 65535, 0
  %t2048 = call %NxVal @nx_int(i64 %t2047)
  %t2049 = call %NxVal @nx_bitand(%NxVal %t2046, %NxVal %t2048)
  %t2051 = extractvalue %NxVal %t2049, 1
  store i64 %t2051, ptr %t2050
  %t2052 = load i64, ptr %t2050
  %t2053 = add i64 7, 0
  %t2054 = and i64 %t2052, %t2053
  %t2055 = add i64 39, 0
  %t2056 = and i64 %t2054, %t2055
  %t2057 = add i64 65535, 0
  %t2058 = and i64 %t2056, %t2057
  store i64 %t2058, ptr %t2050
  %t2059 = load i64, ptr %t2050
  %t2060 = add i64 47, 0
  %t2061 = mul i64 %t2059, %t2060
  %t2062 = add i64 11, 0
  %t2063 = mul i64 %t2061, %t2062
  %t2064 = add i64 65535, 0
  %t2065 = and i64 %t2063, %t2064
  store i64 %t2065, ptr %t2050
  %t2066 = load i64, ptr %t2050
  %t2067 = add i64 29, 0
  %t2068 = call i64 @nx_mod_i64(i64 %t2066, i64 %t2067)
  %t2069 = add i64 41, 0
  %t2070 = call i64 @nx_mod_i64(i64 %t2068, i64 %t2069)
  %t2071 = add i64 65535, 0
  %t2072 = and i64 %t2070, %t2071
  store i64 %t2072, ptr %t2050
  %t2073 = load i64, ptr %t2050
  %t2074 = add i64 81, 0
  %t2075 = mul i64 %t2073, %t2074
  %t2076 = add i64 51, 0
  %t2077 = mul i64 %t2075, %t2076
  %t2078 = add i64 65535, 0
  %t2079 = and i64 %t2077, %t2078
  store i64 %t2079, ptr %t2050
  %t2080 = load i64, ptr %t2050
  %t2081 = add i64 15, 0
  %t2082 = or i64 %t2080, %t2081
  %t2083 = add i64 79, 0
  %t2084 = or i64 %t2082, %t2083
  %t2085 = add i64 65535, 0
  %t2086 = and i64 %t2084, %t2085
  store i64 %t2086, ptr %t2050
  %t2087 = load i64, ptr %t2050
  %t2088 = call %NxVal @nx_int(i64 %t2087)
  ret %NxVal %t2088
}
define %NxVal @nx__m_3____main____Cell__m23(%NxVal* %args, i64 %nargs) {
entry:
  %t2089 = alloca %NxVal
  %t2093 = alloca i64
  %t2108 = alloca i64
  store %NxVal zeroinitializer, ptr %t2089
  %t2090 = getelementptr %NxVal, ptr %args, i64 0
  %t2091 = load %NxVal, ptr %t2090
  %t2092 = call %NxVal @nx_clone(%NxVal %t2091)
  store %NxVal %t2092, ptr %t2089
  %t2094 = getelementptr %NxVal, ptr %args, i64 1
  %t2095 = load %NxVal, ptr %t2094
  %t2096 = extractvalue %NxVal %t2095, 1
  store i64 %t2096, ptr %t2093
  %t2097 = load %NxVal, ptr %t2089
  %t2098 = call %NxVal @nx_rec_get(%NxVal %t2097, i64 0)
  %t2099 = load %NxVal, ptr %t2089
  %t2100 = call %NxVal @nx_rec_get(%NxVal %t2099, i64 1)
  %t2101 = call %NxVal @nx_add(%NxVal %t2098, %NxVal %t2100)
  %t2102 = load i64, ptr %t2093
  %t2103 = call %NxVal @nx_int(i64 %t2102)
  %t2104 = call %NxVal @nx_add(%NxVal %t2101, %NxVal %t2103)
  %t2105 = add i64 65535, 0
  %t2106 = call %NxVal @nx_int(i64 %t2105)
  %t2107 = call %NxVal @nx_bitand(%NxVal %t2104, %NxVal %t2106)
  %t2109 = extractvalue %NxVal %t2107, 1
  store i64 %t2109, ptr %t2108
  %t2110 = load i64, ptr %t2108
  %t2111 = add i64 83, 0
  %t2112 = sub i64 %t2110, %t2111
  %t2113 = add i64 15, 0
  %t2114 = sub i64 %t2112, %t2113
  %t2115 = add i64 65535, 0
  %t2116 = and i64 %t2114, %t2115
  store i64 %t2116, ptr %t2108
  %t2117 = load i64, ptr %t2108
  %t2118 = add i64 33, 0
  %t2119 = or i64 %t2117, %t2118
  %t2120 = add i64 45, 0
  %t2121 = or i64 %t2119, %t2120
  %t2122 = add i64 65535, 0
  %t2123 = and i64 %t2121, %t2122
  store i64 %t2123, ptr %t2108
  %t2124 = load i64, ptr %t2108
  %t2125 = add i64 25, 0
  %t2126 = or i64 %t2124, %t2125
  %t2127 = add i64 23, 0
  %t2128 = or i64 %t2126, %t2127
  %t2129 = add i64 65535, 0
  %t2130 = and i64 %t2128, %t2129
  store i64 %t2130, ptr %t2108
  %t2131 = load i64, ptr %t2108
  %t2132 = add i64 23, 0
  %t2133 = call i64 @nx_mod_i64(i64 %t2131, i64 %t2132)
  %t2134 = add i64 2, 0
  %t2135 = call i64 @nx_mod_i64(i64 %t2133, i64 %t2134)
  %t2136 = add i64 65535, 0
  %t2137 = and i64 %t2135, %t2136
  store i64 %t2137, ptr %t2108
  %t2138 = load i64, ptr %t2108
  %t2139 = add i64 56, 0
  %t2140 = call i64 @nx_mod_i64(i64 %t2138, i64 %t2139)
  %t2141 = add i64 78, 0
  %t2142 = call i64 @nx_mod_i64(i64 %t2140, i64 %t2141)
  %t2143 = add i64 65535, 0
  %t2144 = and i64 %t2142, %t2143
  store i64 %t2144, ptr %t2108
  %t2145 = load i64, ptr %t2108
  %t2146 = call %NxVal @nx_int(i64 %t2145)
  ret %NxVal %t2146
}
define %NxVal @nx__m_3____main____Cell__m24(%NxVal* %args, i64 %nargs) {
entry:
  %t2147 = alloca %NxVal
  %t2151 = alloca i64
  %t2166 = alloca i64
  store %NxVal zeroinitializer, ptr %t2147
  %t2148 = getelementptr %NxVal, ptr %args, i64 0
  %t2149 = load %NxVal, ptr %t2148
  %t2150 = call %NxVal @nx_clone(%NxVal %t2149)
  store %NxVal %t2150, ptr %t2147
  %t2152 = getelementptr %NxVal, ptr %args, i64 1
  %t2153 = load %NxVal, ptr %t2152
  %t2154 = extractvalue %NxVal %t2153, 1
  store i64 %t2154, ptr %t2151
  %t2155 = load %NxVal, ptr %t2147
  %t2156 = call %NxVal @nx_rec_get(%NxVal %t2155, i64 0)
  %t2157 = load %NxVal, ptr %t2147
  %t2158 = call %NxVal @nx_rec_get(%NxVal %t2157, i64 1)
  %t2159 = call %NxVal @nx_add(%NxVal %t2156, %NxVal %t2158)
  %t2160 = load i64, ptr %t2151
  %t2161 = call %NxVal @nx_int(i64 %t2160)
  %t2162 = call %NxVal @nx_add(%NxVal %t2159, %NxVal %t2161)
  %t2163 = add i64 65535, 0
  %t2164 = call %NxVal @nx_int(i64 %t2163)
  %t2165 = call %NxVal @nx_bitand(%NxVal %t2162, %NxVal %t2164)
  %t2167 = extractvalue %NxVal %t2165, 1
  store i64 %t2167, ptr %t2166
  %t2168 = load i64, ptr %t2166
  %t2169 = add i64 75, 0
  %t2170 = xor i64 %t2168, %t2169
  %t2171 = add i64 70, 0
  %t2172 = xor i64 %t2170, %t2171
  %t2173 = add i64 65535, 0
  %t2174 = and i64 %t2172, %t2173
  store i64 %t2174, ptr %t2166
  %t2175 = load i64, ptr %t2166
  %t2176 = add i64 63, 0
  %t2177 = or i64 %t2175, %t2176
  %t2178 = add i64 30, 0
  %t2179 = or i64 %t2177, %t2178
  %t2180 = add i64 65535, 0
  %t2181 = and i64 %t2179, %t2180
  store i64 %t2181, ptr %t2166
  %t2182 = load i64, ptr %t2166
  %t2183 = add i64 51, 0
  %t2184 = mul i64 %t2182, %t2183
  %t2185 = add i64 30, 0
  %t2186 = mul i64 %t2184, %t2185
  %t2187 = add i64 65535, 0
  %t2188 = and i64 %t2186, %t2187
  store i64 %t2188, ptr %t2166
  %t2189 = load i64, ptr %t2166
  %t2190 = add i64 87, 0
  %t2191 = and i64 %t2189, %t2190
  %t2192 = add i64 48, 0
  %t2193 = and i64 %t2191, %t2192
  %t2194 = add i64 65535, 0
  %t2195 = and i64 %t2193, %t2194
  store i64 %t2195, ptr %t2166
  %t2196 = load i64, ptr %t2166
  %t2197 = add i64 57, 0
  %t2198 = add i64 %t2196, %t2197
  %t2199 = add i64 44, 0
  %t2200 = add i64 %t2198, %t2199
  %t2201 = add i64 65535, 0
  %t2202 = and i64 %t2200, %t2201
  store i64 %t2202, ptr %t2166
  %t2203 = load i64, ptr %t2166
  %t2204 = call %NxVal @nx_int(i64 %t2203)
  ret %NxVal %t2204
}
define %NxVal @nx__m_3____main____Cell__m25(%NxVal* %args, i64 %nargs) {
entry:
  %t2205 = alloca %NxVal
  %t2209 = alloca i64
  %t2224 = alloca i64
  store %NxVal zeroinitializer, ptr %t2205
  %t2206 = getelementptr %NxVal, ptr %args, i64 0
  %t2207 = load %NxVal, ptr %t2206
  %t2208 = call %NxVal @nx_clone(%NxVal %t2207)
  store %NxVal %t2208, ptr %t2205
  %t2210 = getelementptr %NxVal, ptr %args, i64 1
  %t2211 = load %NxVal, ptr %t2210
  %t2212 = extractvalue %NxVal %t2211, 1
  store i64 %t2212, ptr %t2209
  %t2213 = load %NxVal, ptr %t2205
  %t2214 = call %NxVal @nx_rec_get(%NxVal %t2213, i64 0)
  %t2215 = load %NxVal, ptr %t2205
  %t2216 = call %NxVal @nx_rec_get(%NxVal %t2215, i64 1)
  %t2217 = call %NxVal @nx_add(%NxVal %t2214, %NxVal %t2216)
  %t2218 = load i64, ptr %t2209
  %t2219 = call %NxVal @nx_int(i64 %t2218)
  %t2220 = call %NxVal @nx_add(%NxVal %t2217, %NxVal %t2219)
  %t2221 = add i64 65535, 0
  %t2222 = call %NxVal @nx_int(i64 %t2221)
  %t2223 = call %NxVal @nx_bitand(%NxVal %t2220, %NxVal %t2222)
  %t2225 = extractvalue %NxVal %t2223, 1
  store i64 %t2225, ptr %t2224
  %t2226 = load i64, ptr %t2224
  %t2227 = add i64 54, 0
  %t2228 = or i64 %t2226, %t2227
  %t2229 = add i64 15, 0
  %t2230 = or i64 %t2228, %t2229
  %t2231 = add i64 65535, 0
  %t2232 = and i64 %t2230, %t2231
  store i64 %t2232, ptr %t2224
  %t2233 = load i64, ptr %t2224
  %t2234 = add i64 65, 0
  %t2235 = add i64 %t2233, %t2234
  %t2236 = add i64 44, 0
  %t2237 = add i64 %t2235, %t2236
  %t2238 = add i64 65535, 0
  %t2239 = and i64 %t2237, %t2238
  store i64 %t2239, ptr %t2224
  %t2240 = load i64, ptr %t2224
  %t2241 = add i64 48, 0
  %t2242 = add i64 %t2240, %t2241
  %t2243 = add i64 62, 0
  %t2244 = add i64 %t2242, %t2243
  %t2245 = add i64 65535, 0
  %t2246 = and i64 %t2244, %t2245
  store i64 %t2246, ptr %t2224
  %t2247 = load i64, ptr %t2224
  %t2248 = add i64 95, 0
  %t2249 = add i64 %t2247, %t2248
  %t2250 = add i64 82, 0
  %t2251 = add i64 %t2249, %t2250
  %t2252 = add i64 65535, 0
  %t2253 = and i64 %t2251, %t2252
  store i64 %t2253, ptr %t2224
  %t2254 = load i64, ptr %t2224
  %t2255 = add i64 33, 0
  %t2256 = mul i64 %t2254, %t2255
  %t2257 = add i64 78, 0
  %t2258 = mul i64 %t2256, %t2257
  %t2259 = add i64 65535, 0
  %t2260 = and i64 %t2258, %t2259
  store i64 %t2260, ptr %t2224
  %t2261 = load i64, ptr %t2224
  %t2262 = call %NxVal @nx_int(i64 %t2261)
  ret %NxVal %t2262
}
define %NxVal @nx__m_3____main____Cell__m26(%NxVal* %args, i64 %nargs) {
entry:
  %t2263 = alloca %NxVal
  %t2267 = alloca i64
  %t2282 = alloca i64
  store %NxVal zeroinitializer, ptr %t2263
  %t2264 = getelementptr %NxVal, ptr %args, i64 0
  %t2265 = load %NxVal, ptr %t2264
  %t2266 = call %NxVal @nx_clone(%NxVal %t2265)
  store %NxVal %t2266, ptr %t2263
  %t2268 = getelementptr %NxVal, ptr %args, i64 1
  %t2269 = load %NxVal, ptr %t2268
  %t2270 = extractvalue %NxVal %t2269, 1
  store i64 %t2270, ptr %t2267
  %t2271 = load %NxVal, ptr %t2263
  %t2272 = call %NxVal @nx_rec_get(%NxVal %t2271, i64 0)
  %t2273 = load %NxVal, ptr %t2263
  %t2274 = call %NxVal @nx_rec_get(%NxVal %t2273, i64 1)
  %t2275 = call %NxVal @nx_add(%NxVal %t2272, %NxVal %t2274)
  %t2276 = load i64, ptr %t2267
  %t2277 = call %NxVal @nx_int(i64 %t2276)
  %t2278 = call %NxVal @nx_add(%NxVal %t2275, %NxVal %t2277)
  %t2279 = add i64 65535, 0
  %t2280 = call %NxVal @nx_int(i64 %t2279)
  %t2281 = call %NxVal @nx_bitand(%NxVal %t2278, %NxVal %t2280)
  %t2283 = extractvalue %NxVal %t2281, 1
  store i64 %t2283, ptr %t2282
  %t2284 = load i64, ptr %t2282
  %t2285 = add i64 72, 0
  %t2286 = add i64 %t2284, %t2285
  %t2287 = add i64 82, 0
  %t2288 = add i64 %t2286, %t2287
  %t2289 = add i64 65535, 0
  %t2290 = and i64 %t2288, %t2289
  store i64 %t2290, ptr %t2282
  %t2291 = load i64, ptr %t2282
  %t2292 = add i64 50, 0
  %t2293 = add i64 %t2291, %t2292
  %t2294 = add i64 7, 0
  %t2295 = add i64 %t2293, %t2294
  %t2296 = add i64 65535, 0
  %t2297 = and i64 %t2295, %t2296
  store i64 %t2297, ptr %t2282
  %t2298 = load i64, ptr %t2282
  %t2299 = add i64 90, 0
  %t2300 = sub i64 %t2298, %t2299
  %t2301 = add i64 57, 0
  %t2302 = sub i64 %t2300, %t2301
  %t2303 = add i64 65535, 0
  %t2304 = and i64 %t2302, %t2303
  store i64 %t2304, ptr %t2282
  %t2305 = load i64, ptr %t2282
  %t2306 = add i64 56, 0
  %t2307 = and i64 %t2305, %t2306
  %t2308 = add i64 58, 0
  %t2309 = and i64 %t2307, %t2308
  %t2310 = add i64 65535, 0
  %t2311 = and i64 %t2309, %t2310
  store i64 %t2311, ptr %t2282
  %t2312 = load i64, ptr %t2282
  %t2313 = add i64 2, 0
  %t2314 = mul i64 %t2312, %t2313
  %t2315 = add i64 13, 0
  %t2316 = mul i64 %t2314, %t2315
  %t2317 = add i64 65535, 0
  %t2318 = and i64 %t2316, %t2317
  store i64 %t2318, ptr %t2282
  %t2319 = load i64, ptr %t2282
  %t2320 = call %NxVal @nx_int(i64 %t2319)
  ret %NxVal %t2320
}
define %NxVal @nx__m_3____main____Cell__m27(%NxVal* %args, i64 %nargs) {
entry:
  %t2321 = alloca %NxVal
  %t2325 = alloca i64
  %t2340 = alloca i64
  store %NxVal zeroinitializer, ptr %t2321
  %t2322 = getelementptr %NxVal, ptr %args, i64 0
  %t2323 = load %NxVal, ptr %t2322
  %t2324 = call %NxVal @nx_clone(%NxVal %t2323)
  store %NxVal %t2324, ptr %t2321
  %t2326 = getelementptr %NxVal, ptr %args, i64 1
  %t2327 = load %NxVal, ptr %t2326
  %t2328 = extractvalue %NxVal %t2327, 1
  store i64 %t2328, ptr %t2325
  %t2329 = load %NxVal, ptr %t2321
  %t2330 = call %NxVal @nx_rec_get(%NxVal %t2329, i64 0)
  %t2331 = load %NxVal, ptr %t2321
  %t2332 = call %NxVal @nx_rec_get(%NxVal %t2331, i64 1)
  %t2333 = call %NxVal @nx_add(%NxVal %t2330, %NxVal %t2332)
  %t2334 = load i64, ptr %t2325
  %t2335 = call %NxVal @nx_int(i64 %t2334)
  %t2336 = call %NxVal @nx_add(%NxVal %t2333, %NxVal %t2335)
  %t2337 = add i64 65535, 0
  %t2338 = call %NxVal @nx_int(i64 %t2337)
  %t2339 = call %NxVal @nx_bitand(%NxVal %t2336, %NxVal %t2338)
  %t2341 = extractvalue %NxVal %t2339, 1
  store i64 %t2341, ptr %t2340
  %t2342 = load i64, ptr %t2340
  %t2343 = add i64 19, 0
  %t2344 = sub i64 %t2342, %t2343
  %t2345 = add i64 37, 0
  %t2346 = sub i64 %t2344, %t2345
  %t2347 = add i64 65535, 0
  %t2348 = and i64 %t2346, %t2347
  store i64 %t2348, ptr %t2340
  %t2349 = load i64, ptr %t2340
  %t2350 = add i64 97, 0
  %t2351 = xor i64 %t2349, %t2350
  %t2352 = add i64 46, 0
  %t2353 = xor i64 %t2351, %t2352
  %t2354 = add i64 65535, 0
  %t2355 = and i64 %t2353, %t2354
  store i64 %t2355, ptr %t2340
  %t2356 = load i64, ptr %t2340
  %t2357 = add i64 6, 0
  %t2358 = add i64 %t2356, %t2357
  %t2359 = add i64 13, 0
  %t2360 = add i64 %t2358, %t2359
  %t2361 = add i64 65535, 0
  %t2362 = and i64 %t2360, %t2361
  store i64 %t2362, ptr %t2340
  %t2363 = load i64, ptr %t2340
  %t2364 = add i64 16, 0
  %t2365 = xor i64 %t2363, %t2364
  %t2366 = add i64 52, 0
  %t2367 = xor i64 %t2365, %t2366
  %t2368 = add i64 65535, 0
  %t2369 = and i64 %t2367, %t2368
  store i64 %t2369, ptr %t2340
  %t2370 = load i64, ptr %t2340
  %t2371 = add i64 18, 0
  %t2372 = sub i64 %t2370, %t2371
  %t2373 = add i64 65, 0
  %t2374 = sub i64 %t2372, %t2373
  %t2375 = add i64 65535, 0
  %t2376 = and i64 %t2374, %t2375
  store i64 %t2376, ptr %t2340
  %t2377 = load i64, ptr %t2340
  %t2378 = call %NxVal @nx_int(i64 %t2377)
  ret %NxVal %t2378
}
define %NxVal @nx__m_3____main____Cell__m28(%NxVal* %args, i64 %nargs) {
entry:
  %t2379 = alloca %NxVal
  %t2383 = alloca i64
  %t2398 = alloca i64
  store %NxVal zeroinitializer, ptr %t2379
  %t2380 = getelementptr %NxVal, ptr %args, i64 0
  %t2381 = load %NxVal, ptr %t2380
  %t2382 = call %NxVal @nx_clone(%NxVal %t2381)
  store %NxVal %t2382, ptr %t2379
  %t2384 = getelementptr %NxVal, ptr %args, i64 1
  %t2385 = load %NxVal, ptr %t2384
  %t2386 = extractvalue %NxVal %t2385, 1
  store i64 %t2386, ptr %t2383
  %t2387 = load %NxVal, ptr %t2379
  %t2388 = call %NxVal @nx_rec_get(%NxVal %t2387, i64 0)
  %t2389 = load %NxVal, ptr %t2379
  %t2390 = call %NxVal @nx_rec_get(%NxVal %t2389, i64 1)
  %t2391 = call %NxVal @nx_add(%NxVal %t2388, %NxVal %t2390)
  %t2392 = load i64, ptr %t2383
  %t2393 = call %NxVal @nx_int(i64 %t2392)
  %t2394 = call %NxVal @nx_add(%NxVal %t2391, %NxVal %t2393)
  %t2395 = add i64 65535, 0
  %t2396 = call %NxVal @nx_int(i64 %t2395)
  %t2397 = call %NxVal @nx_bitand(%NxVal %t2394, %NxVal %t2396)
  %t2399 = extractvalue %NxVal %t2397, 1
  store i64 %t2399, ptr %t2398
  %t2400 = load i64, ptr %t2398
  %t2401 = add i64 81, 0
  %t2402 = sub i64 %t2400, %t2401
  %t2403 = add i64 5, 0
  %t2404 = sub i64 %t2402, %t2403
  %t2405 = add i64 65535, 0
  %t2406 = and i64 %t2404, %t2405
  store i64 %t2406, ptr %t2398
  %t2407 = load i64, ptr %t2398
  %t2408 = add i64 80, 0
  %t2409 = xor i64 %t2407, %t2408
  %t2410 = add i64 45, 0
  %t2411 = xor i64 %t2409, %t2410
  %t2412 = add i64 65535, 0
  %t2413 = and i64 %t2411, %t2412
  store i64 %t2413, ptr %t2398
  %t2414 = load i64, ptr %t2398
  %t2415 = add i64 85, 0
  %t2416 = xor i64 %t2414, %t2415
  %t2417 = add i64 69, 0
  %t2418 = xor i64 %t2416, %t2417
  %t2419 = add i64 65535, 0
  %t2420 = and i64 %t2418, %t2419
  store i64 %t2420, ptr %t2398
  %t2421 = load i64, ptr %t2398
  %t2422 = add i64 88, 0
  %t2423 = call i64 @nx_mod_i64(i64 %t2421, i64 %t2422)
  %t2424 = add i64 20, 0
  %t2425 = call i64 @nx_mod_i64(i64 %t2423, i64 %t2424)
  %t2426 = add i64 65535, 0
  %t2427 = and i64 %t2425, %t2426
  store i64 %t2427, ptr %t2398
  %t2428 = load i64, ptr %t2398
  %t2429 = add i64 81, 0
  %t2430 = sub i64 %t2428, %t2429
  %t2431 = add i64 15, 0
  %t2432 = sub i64 %t2430, %t2431
  %t2433 = add i64 65535, 0
  %t2434 = and i64 %t2432, %t2433
  store i64 %t2434, ptr %t2398
  %t2435 = load i64, ptr %t2398
  %t2436 = call %NxVal @nx_int(i64 %t2435)
  ret %NxVal %t2436
}
define %NxVal @nx__m_3____main____Cell__m29(%NxVal* %args, i64 %nargs) {
entry:
  %t2437 = alloca %NxVal
  %t2441 = alloca i64
  %t2456 = alloca i64
  store %NxVal zeroinitializer, ptr %t2437
  %t2438 = getelementptr %NxVal, ptr %args, i64 0
  %t2439 = load %NxVal, ptr %t2438
  %t2440 = call %NxVal @nx_clone(%NxVal %t2439)
  store %NxVal %t2440, ptr %t2437
  %t2442 = getelementptr %NxVal, ptr %args, i64 1
  %t2443 = load %NxVal, ptr %t2442
  %t2444 = extractvalue %NxVal %t2443, 1
  store i64 %t2444, ptr %t2441
  %t2445 = load %NxVal, ptr %t2437
  %t2446 = call %NxVal @nx_rec_get(%NxVal %t2445, i64 0)
  %t2447 = load %NxVal, ptr %t2437
  %t2448 = call %NxVal @nx_rec_get(%NxVal %t2447, i64 1)
  %t2449 = call %NxVal @nx_add(%NxVal %t2446, %NxVal %t2448)
  %t2450 = load i64, ptr %t2441
  %t2451 = call %NxVal @nx_int(i64 %t2450)
  %t2452 = call %NxVal @nx_add(%NxVal %t2449, %NxVal %t2451)
  %t2453 = add i64 65535, 0
  %t2454 = call %NxVal @nx_int(i64 %t2453)
  %t2455 = call %NxVal @nx_bitand(%NxVal %t2452, %NxVal %t2454)
  %t2457 = extractvalue %NxVal %t2455, 1
  store i64 %t2457, ptr %t2456
  %t2458 = load i64, ptr %t2456
  %t2459 = add i64 36, 0
  %t2460 = and i64 %t2458, %t2459
  %t2461 = add i64 66, 0
  %t2462 = and i64 %t2460, %t2461
  %t2463 = add i64 65535, 0
  %t2464 = and i64 %t2462, %t2463
  store i64 %t2464, ptr %t2456
  %t2465 = load i64, ptr %t2456
  %t2466 = add i64 84, 0
  %t2467 = and i64 %t2465, %t2466
  %t2468 = add i64 25, 0
  %t2469 = and i64 %t2467, %t2468
  %t2470 = add i64 65535, 0
  %t2471 = and i64 %t2469, %t2470
  store i64 %t2471, ptr %t2456
  %t2472 = load i64, ptr %t2456
  %t2473 = add i64 11, 0
  %t2474 = sub i64 %t2472, %t2473
  %t2475 = add i64 66, 0
  %t2476 = sub i64 %t2474, %t2475
  %t2477 = add i64 65535, 0
  %t2478 = and i64 %t2476, %t2477
  store i64 %t2478, ptr %t2456
  %t2479 = load i64, ptr %t2456
  %t2480 = add i64 3, 0
  %t2481 = add i64 %t2479, %t2480
  %t2482 = add i64 61, 0
  %t2483 = add i64 %t2481, %t2482
  %t2484 = add i64 65535, 0
  %t2485 = and i64 %t2483, %t2484
  store i64 %t2485, ptr %t2456
  %t2486 = load i64, ptr %t2456
  %t2487 = add i64 61, 0
  %t2488 = mul i64 %t2486, %t2487
  %t2489 = add i64 48, 0
  %t2490 = mul i64 %t2488, %t2489
  %t2491 = add i64 65535, 0
  %t2492 = and i64 %t2490, %t2491
  store i64 %t2492, ptr %t2456
  %t2493 = load i64, ptr %t2456
  %t2494 = call %NxVal @nx_int(i64 %t2493)
  ret %NxVal %t2494
}
define %NxVal @nx__m_2____main____Cell__m3(%NxVal* %args, i64 %nargs) {
entry:
  %t2495 = alloca %NxVal
  %t2499 = alloca i64
  %t2514 = alloca i64
  store %NxVal zeroinitializer, ptr %t2495
  %t2496 = getelementptr %NxVal, ptr %args, i64 0
  %t2497 = load %NxVal, ptr %t2496
  %t2498 = call %NxVal @nx_clone(%NxVal %t2497)
  store %NxVal %t2498, ptr %t2495
  %t2500 = getelementptr %NxVal, ptr %args, i64 1
  %t2501 = load %NxVal, ptr %t2500
  %t2502 = extractvalue %NxVal %t2501, 1
  store i64 %t2502, ptr %t2499
  %t2503 = load %NxVal, ptr %t2495
  %t2504 = call %NxVal @nx_rec_get(%NxVal %t2503, i64 0)
  %t2505 = load %NxVal, ptr %t2495
  %t2506 = call %NxVal @nx_rec_get(%NxVal %t2505, i64 1)
  %t2507 = call %NxVal @nx_add(%NxVal %t2504, %NxVal %t2506)
  %t2508 = load i64, ptr %t2499
  %t2509 = call %NxVal @nx_int(i64 %t2508)
  %t2510 = call %NxVal @nx_add(%NxVal %t2507, %NxVal %t2509)
  %t2511 = add i64 65535, 0
  %t2512 = call %NxVal @nx_int(i64 %t2511)
  %t2513 = call %NxVal @nx_bitand(%NxVal %t2510, %NxVal %t2512)
  %t2515 = extractvalue %NxVal %t2513, 1
  store i64 %t2515, ptr %t2514
  %t2516 = load i64, ptr %t2514
  %t2517 = add i64 22, 0
  %t2518 = sub i64 %t2516, %t2517
  %t2519 = add i64 49, 0
  %t2520 = sub i64 %t2518, %t2519
  %t2521 = add i64 65535, 0
  %t2522 = and i64 %t2520, %t2521
  store i64 %t2522, ptr %t2514
  %t2523 = load i64, ptr %t2514
  %t2524 = add i64 59, 0
  %t2525 = call i64 @nx_mod_i64(i64 %t2523, i64 %t2524)
  %t2526 = add i64 26, 0
  %t2527 = call i64 @nx_mod_i64(i64 %t2525, i64 %t2526)
  %t2528 = add i64 65535, 0
  %t2529 = and i64 %t2527, %t2528
  store i64 %t2529, ptr %t2514
  %t2530 = load i64, ptr %t2514
  %t2531 = add i64 30, 0
  %t2532 = call i64 @nx_mod_i64(i64 %t2530, i64 %t2531)
  %t2533 = add i64 21, 0
  %t2534 = call i64 @nx_mod_i64(i64 %t2532, i64 %t2533)
  %t2535 = add i64 65535, 0
  %t2536 = and i64 %t2534, %t2535
  store i64 %t2536, ptr %t2514
  %t2537 = load i64, ptr %t2514
  %t2538 = add i64 8, 0
  %t2539 = xor i64 %t2537, %t2538
  %t2540 = add i64 37, 0
  %t2541 = xor i64 %t2539, %t2540
  %t2542 = add i64 65535, 0
  %t2543 = and i64 %t2541, %t2542
  store i64 %t2543, ptr %t2514
  %t2544 = load i64, ptr %t2514
  %t2545 = add i64 42, 0
  %t2546 = call i64 @nx_mod_i64(i64 %t2544, i64 %t2545)
  %t2547 = add i64 69, 0
  %t2548 = call i64 @nx_mod_i64(i64 %t2546, i64 %t2547)
  %t2549 = add i64 65535, 0
  %t2550 = and i64 %t2548, %t2549
  store i64 %t2550, ptr %t2514
  %t2551 = load i64, ptr %t2514
  %t2552 = call %NxVal @nx_int(i64 %t2551)
  ret %NxVal %t2552
}
define %NxVal @nx__m_3____main____Cell__m30(%NxVal* %args, i64 %nargs) {
entry:
  %t2553 = alloca %NxVal
  %t2557 = alloca i64
  %t2572 = alloca i64
  store %NxVal zeroinitializer, ptr %t2553
  %t2554 = getelementptr %NxVal, ptr %args, i64 0
  %t2555 = load %NxVal, ptr %t2554
  %t2556 = call %NxVal @nx_clone(%NxVal %t2555)
  store %NxVal %t2556, ptr %t2553
  %t2558 = getelementptr %NxVal, ptr %args, i64 1
  %t2559 = load %NxVal, ptr %t2558
  %t2560 = extractvalue %NxVal %t2559, 1
  store i64 %t2560, ptr %t2557
  %t2561 = load %NxVal, ptr %t2553
  %t2562 = call %NxVal @nx_rec_get(%NxVal %t2561, i64 0)
  %t2563 = load %NxVal, ptr %t2553
  %t2564 = call %NxVal @nx_rec_get(%NxVal %t2563, i64 1)
  %t2565 = call %NxVal @nx_add(%NxVal %t2562, %NxVal %t2564)
  %t2566 = load i64, ptr %t2557
  %t2567 = call %NxVal @nx_int(i64 %t2566)
  %t2568 = call %NxVal @nx_add(%NxVal %t2565, %NxVal %t2567)
  %t2569 = add i64 65535, 0
  %t2570 = call %NxVal @nx_int(i64 %t2569)
  %t2571 = call %NxVal @nx_bitand(%NxVal %t2568, %NxVal %t2570)
  %t2573 = extractvalue %NxVal %t2571, 1
  store i64 %t2573, ptr %t2572
  %t2574 = load i64, ptr %t2572
  %t2575 = add i64 45, 0
  %t2576 = mul i64 %t2574, %t2575
  %t2577 = add i64 37, 0
  %t2578 = mul i64 %t2576, %t2577
  %t2579 = add i64 65535, 0
  %t2580 = and i64 %t2578, %t2579
  store i64 %t2580, ptr %t2572
  %t2581 = load i64, ptr %t2572
  %t2582 = add i64 21, 0
  %t2583 = and i64 %t2581, %t2582
  %t2584 = add i64 41, 0
  %t2585 = and i64 %t2583, %t2584
  %t2586 = add i64 65535, 0
  %t2587 = and i64 %t2585, %t2586
  store i64 %t2587, ptr %t2572
  %t2588 = load i64, ptr %t2572
  %t2589 = add i64 28, 0
  %t2590 = and i64 %t2588, %t2589
  %t2591 = add i64 39, 0
  %t2592 = and i64 %t2590, %t2591
  %t2593 = add i64 65535, 0
  %t2594 = and i64 %t2592, %t2593
  store i64 %t2594, ptr %t2572
  %t2595 = load i64, ptr %t2572
  %t2596 = add i64 75, 0
  %t2597 = mul i64 %t2595, %t2596
  %t2598 = add i64 8, 0
  %t2599 = mul i64 %t2597, %t2598
  %t2600 = add i64 65535, 0
  %t2601 = and i64 %t2599, %t2600
  store i64 %t2601, ptr %t2572
  %t2602 = load i64, ptr %t2572
  %t2603 = add i64 38, 0
  %t2604 = and i64 %t2602, %t2603
  %t2605 = add i64 87, 0
  %t2606 = and i64 %t2604, %t2605
  %t2607 = add i64 65535, 0
  %t2608 = and i64 %t2606, %t2607
  store i64 %t2608, ptr %t2572
  %t2609 = load i64, ptr %t2572
  %t2610 = call %NxVal @nx_int(i64 %t2609)
  ret %NxVal %t2610
}
define %NxVal @nx__m_3____main____Cell__m31(%NxVal* %args, i64 %nargs) {
entry:
  %t2611 = alloca %NxVal
  %t2615 = alloca i64
  %t2630 = alloca i64
  store %NxVal zeroinitializer, ptr %t2611
  %t2612 = getelementptr %NxVal, ptr %args, i64 0
  %t2613 = load %NxVal, ptr %t2612
  %t2614 = call %NxVal @nx_clone(%NxVal %t2613)
  store %NxVal %t2614, ptr %t2611
  %t2616 = getelementptr %NxVal, ptr %args, i64 1
  %t2617 = load %NxVal, ptr %t2616
  %t2618 = extractvalue %NxVal %t2617, 1
  store i64 %t2618, ptr %t2615
  %t2619 = load %NxVal, ptr %t2611
  %t2620 = call %NxVal @nx_rec_get(%NxVal %t2619, i64 0)
  %t2621 = load %NxVal, ptr %t2611
  %t2622 = call %NxVal @nx_rec_get(%NxVal %t2621, i64 1)
  %t2623 = call %NxVal @nx_add(%NxVal %t2620, %NxVal %t2622)
  %t2624 = load i64, ptr %t2615
  %t2625 = call %NxVal @nx_int(i64 %t2624)
  %t2626 = call %NxVal @nx_add(%NxVal %t2623, %NxVal %t2625)
  %t2627 = add i64 65535, 0
  %t2628 = call %NxVal @nx_int(i64 %t2627)
  %t2629 = call %NxVal @nx_bitand(%NxVal %t2626, %NxVal %t2628)
  %t2631 = extractvalue %NxVal %t2629, 1
  store i64 %t2631, ptr %t2630
  %t2632 = load i64, ptr %t2630
  %t2633 = add i64 37, 0
  %t2634 = call i64 @nx_mod_i64(i64 %t2632, i64 %t2633)
  %t2635 = add i64 9, 0
  %t2636 = call i64 @nx_mod_i64(i64 %t2634, i64 %t2635)
  %t2637 = add i64 65535, 0
  %t2638 = and i64 %t2636, %t2637
  store i64 %t2638, ptr %t2630
  %t2639 = load i64, ptr %t2630
  %t2640 = add i64 16, 0
  %t2641 = xor i64 %t2639, %t2640
  %t2642 = add i64 52, 0
  %t2643 = xor i64 %t2641, %t2642
  %t2644 = add i64 65535, 0
  %t2645 = and i64 %t2643, %t2644
  store i64 %t2645, ptr %t2630
  %t2646 = load i64, ptr %t2630
  %t2647 = add i64 89, 0
  %t2648 = mul i64 %t2646, %t2647
  %t2649 = add i64 49, 0
  %t2650 = mul i64 %t2648, %t2649
  %t2651 = add i64 65535, 0
  %t2652 = and i64 %t2650, %t2651
  store i64 %t2652, ptr %t2630
  %t2653 = load i64, ptr %t2630
  %t2654 = add i64 44, 0
  %t2655 = add i64 %t2653, %t2654
  %t2656 = add i64 24, 0
  %t2657 = add i64 %t2655, %t2656
  %t2658 = add i64 65535, 0
  %t2659 = and i64 %t2657, %t2658
  store i64 %t2659, ptr %t2630
  %t2660 = load i64, ptr %t2630
  %t2661 = add i64 66, 0
  %t2662 = or i64 %t2660, %t2661
  %t2663 = add i64 36, 0
  %t2664 = or i64 %t2662, %t2663
  %t2665 = add i64 65535, 0
  %t2666 = and i64 %t2664, %t2665
  store i64 %t2666, ptr %t2630
  %t2667 = load i64, ptr %t2630
  %t2668 = call %NxVal @nx_int(i64 %t2667)
  ret %NxVal %t2668
}
define %NxVal @nx__m_3____main____Cell__m32(%NxVal* %args, i64 %nargs) {
entry:
  %t2669 = alloca %NxVal
  %t2673 = alloca i64
  %t2688 = alloca i64
  store %NxVal zeroinitializer, ptr %t2669
  %t2670 = getelementptr %NxVal, ptr %args, i64 0
  %t2671 = load %NxVal, ptr %t2670
  %t2672 = call %NxVal @nx_clone(%NxVal %t2671)
  store %NxVal %t2672, ptr %t2669
  %t2674 = getelementptr %NxVal, ptr %args, i64 1
  %t2675 = load %NxVal, ptr %t2674
  %t2676 = extractvalue %NxVal %t2675, 1
  store i64 %t2676, ptr %t2673
  %t2677 = load %NxVal, ptr %t2669
  %t2678 = call %NxVal @nx_rec_get(%NxVal %t2677, i64 0)
  %t2679 = load %NxVal, ptr %t2669
  %t2680 = call %NxVal @nx_rec_get(%NxVal %t2679, i64 1)
  %t2681 = call %NxVal @nx_add(%NxVal %t2678, %NxVal %t2680)
  %t2682 = load i64, ptr %t2673
  %t2683 = call %NxVal @nx_int(i64 %t2682)
  %t2684 = call %NxVal @nx_add(%NxVal %t2681, %NxVal %t2683)
  %t2685 = add i64 65535, 0
  %t2686 = call %NxVal @nx_int(i64 %t2685)
  %t2687 = call %NxVal @nx_bitand(%NxVal %t2684, %NxVal %t2686)
  %t2689 = extractvalue %NxVal %t2687, 1
  store i64 %t2689, ptr %t2688
  %t2690 = load i64, ptr %t2688
  %t2691 = add i64 57, 0
  %t2692 = and i64 %t2690, %t2691
  %t2693 = add i64 2, 0
  %t2694 = and i64 %t2692, %t2693
  %t2695 = add i64 65535, 0
  %t2696 = and i64 %t2694, %t2695
  store i64 %t2696, ptr %t2688
  %t2697 = load i64, ptr %t2688
  %t2698 = add i64 26, 0
  %t2699 = call i64 @nx_mod_i64(i64 %t2697, i64 %t2698)
  %t2700 = add i64 88, 0
  %t2701 = call i64 @nx_mod_i64(i64 %t2699, i64 %t2700)
  %t2702 = add i64 65535, 0
  %t2703 = and i64 %t2701, %t2702
  store i64 %t2703, ptr %t2688
  %t2704 = load i64, ptr %t2688
  %t2705 = add i64 15, 0
  %t2706 = add i64 %t2704, %t2705
  %t2707 = add i64 53, 0
  %t2708 = add i64 %t2706, %t2707
  %t2709 = add i64 65535, 0
  %t2710 = and i64 %t2708, %t2709
  store i64 %t2710, ptr %t2688
  %t2711 = load i64, ptr %t2688
  %t2712 = add i64 95, 0
  %t2713 = call i64 @nx_mod_i64(i64 %t2711, i64 %t2712)
  %t2714 = add i64 82, 0
  %t2715 = call i64 @nx_mod_i64(i64 %t2713, i64 %t2714)
  %t2716 = add i64 65535, 0
  %t2717 = and i64 %t2715, %t2716
  store i64 %t2717, ptr %t2688
  %t2718 = load i64, ptr %t2688
  %t2719 = add i64 77, 0
  %t2720 = call i64 @nx_mod_i64(i64 %t2718, i64 %t2719)
  %t2721 = add i64 17, 0
  %t2722 = call i64 @nx_mod_i64(i64 %t2720, i64 %t2721)
  %t2723 = add i64 65535, 0
  %t2724 = and i64 %t2722, %t2723
  store i64 %t2724, ptr %t2688
  %t2725 = load i64, ptr %t2688
  %t2726 = call %NxVal @nx_int(i64 %t2725)
  ret %NxVal %t2726
}
define %NxVal @nx__m_3____main____Cell__m33(%NxVal* %args, i64 %nargs) {
entry:
  %t2727 = alloca %NxVal
  %t2731 = alloca i64
  %t2746 = alloca i64
  store %NxVal zeroinitializer, ptr %t2727
  %t2728 = getelementptr %NxVal, ptr %args, i64 0
  %t2729 = load %NxVal, ptr %t2728
  %t2730 = call %NxVal @nx_clone(%NxVal %t2729)
  store %NxVal %t2730, ptr %t2727
  %t2732 = getelementptr %NxVal, ptr %args, i64 1
  %t2733 = load %NxVal, ptr %t2732
  %t2734 = extractvalue %NxVal %t2733, 1
  store i64 %t2734, ptr %t2731
  %t2735 = load %NxVal, ptr %t2727
  %t2736 = call %NxVal @nx_rec_get(%NxVal %t2735, i64 0)
  %t2737 = load %NxVal, ptr %t2727
  %t2738 = call %NxVal @nx_rec_get(%NxVal %t2737, i64 1)
  %t2739 = call %NxVal @nx_add(%NxVal %t2736, %NxVal %t2738)
  %t2740 = load i64, ptr %t2731
  %t2741 = call %NxVal @nx_int(i64 %t2740)
  %t2742 = call %NxVal @nx_add(%NxVal %t2739, %NxVal %t2741)
  %t2743 = add i64 65535, 0
  %t2744 = call %NxVal @nx_int(i64 %t2743)
  %t2745 = call %NxVal @nx_bitand(%NxVal %t2742, %NxVal %t2744)
  %t2747 = extractvalue %NxVal %t2745, 1
  store i64 %t2747, ptr %t2746
  %t2748 = load i64, ptr %t2746
  %t2749 = add i64 89, 0
  %t2750 = and i64 %t2748, %t2749
  %t2751 = add i64 26, 0
  %t2752 = and i64 %t2750, %t2751
  %t2753 = add i64 65535, 0
  %t2754 = and i64 %t2752, %t2753
  store i64 %t2754, ptr %t2746
  %t2755 = load i64, ptr %t2746
  %t2756 = add i64 3, 0
  %t2757 = or i64 %t2755, %t2756
  %t2758 = add i64 89, 0
  %t2759 = or i64 %t2757, %t2758
  %t2760 = add i64 65535, 0
  %t2761 = and i64 %t2759, %t2760
  store i64 %t2761, ptr %t2746
  %t2762 = load i64, ptr %t2746
  %t2763 = add i64 30, 0
  %t2764 = and i64 %t2762, %t2763
  %t2765 = add i64 58, 0
  %t2766 = and i64 %t2764, %t2765
  %t2767 = add i64 65535, 0
  %t2768 = and i64 %t2766, %t2767
  store i64 %t2768, ptr %t2746
  %t2769 = load i64, ptr %t2746
  %t2770 = add i64 8, 0
  %t2771 = sub i64 %t2769, %t2770
  %t2772 = add i64 38, 0
  %t2773 = sub i64 %t2771, %t2772
  %t2774 = add i64 65535, 0
  %t2775 = and i64 %t2773, %t2774
  store i64 %t2775, ptr %t2746
  %t2776 = load i64, ptr %t2746
  %t2777 = add i64 20, 0
  %t2778 = mul i64 %t2776, %t2777
  %t2779 = add i64 20, 0
  %t2780 = mul i64 %t2778, %t2779
  %t2781 = add i64 65535, 0
  %t2782 = and i64 %t2780, %t2781
  store i64 %t2782, ptr %t2746
  %t2783 = load i64, ptr %t2746
  %t2784 = call %NxVal @nx_int(i64 %t2783)
  ret %NxVal %t2784
}
define %NxVal @nx__m_3____main____Cell__m34(%NxVal* %args, i64 %nargs) {
entry:
  %t2785 = alloca %NxVal
  %t2789 = alloca i64
  %t2804 = alloca i64
  store %NxVal zeroinitializer, ptr %t2785
  %t2786 = getelementptr %NxVal, ptr %args, i64 0
  %t2787 = load %NxVal, ptr %t2786
  %t2788 = call %NxVal @nx_clone(%NxVal %t2787)
  store %NxVal %t2788, ptr %t2785
  %t2790 = getelementptr %NxVal, ptr %args, i64 1
  %t2791 = load %NxVal, ptr %t2790
  %t2792 = extractvalue %NxVal %t2791, 1
  store i64 %t2792, ptr %t2789
  %t2793 = load %NxVal, ptr %t2785
  %t2794 = call %NxVal @nx_rec_get(%NxVal %t2793, i64 0)
  %t2795 = load %NxVal, ptr %t2785
  %t2796 = call %NxVal @nx_rec_get(%NxVal %t2795, i64 1)
  %t2797 = call %NxVal @nx_add(%NxVal %t2794, %NxVal %t2796)
  %t2798 = load i64, ptr %t2789
  %t2799 = call %NxVal @nx_int(i64 %t2798)
  %t2800 = call %NxVal @nx_add(%NxVal %t2797, %NxVal %t2799)
  %t2801 = add i64 65535, 0
  %t2802 = call %NxVal @nx_int(i64 %t2801)
  %t2803 = call %NxVal @nx_bitand(%NxVal %t2800, %NxVal %t2802)
  %t2805 = extractvalue %NxVal %t2803, 1
  store i64 %t2805, ptr %t2804
  %t2806 = load i64, ptr %t2804
  %t2807 = add i64 11, 0
  %t2808 = sub i64 %t2806, %t2807
  %t2809 = add i64 36, 0
  %t2810 = sub i64 %t2808, %t2809
  %t2811 = add i64 65535, 0
  %t2812 = and i64 %t2810, %t2811
  store i64 %t2812, ptr %t2804
  %t2813 = load i64, ptr %t2804
  %t2814 = add i64 26, 0
  %t2815 = add i64 %t2813, %t2814
  %t2816 = add i64 31, 0
  %t2817 = add i64 %t2815, %t2816
  %t2818 = add i64 65535, 0
  %t2819 = and i64 %t2817, %t2818
  store i64 %t2819, ptr %t2804
  %t2820 = load i64, ptr %t2804
  %t2821 = add i64 88, 0
  %t2822 = or i64 %t2820, %t2821
  %t2823 = add i64 7, 0
  %t2824 = or i64 %t2822, %t2823
  %t2825 = add i64 65535, 0
  %t2826 = and i64 %t2824, %t2825
  store i64 %t2826, ptr %t2804
  %t2827 = load i64, ptr %t2804
  %t2828 = add i64 32, 0
  %t2829 = mul i64 %t2827, %t2828
  %t2830 = add i64 6, 0
  %t2831 = mul i64 %t2829, %t2830
  %t2832 = add i64 65535, 0
  %t2833 = and i64 %t2831, %t2832
  store i64 %t2833, ptr %t2804
  %t2834 = load i64, ptr %t2804
  %t2835 = add i64 55, 0
  %t2836 = sub i64 %t2834, %t2835
  %t2837 = add i64 3, 0
  %t2838 = sub i64 %t2836, %t2837
  %t2839 = add i64 65535, 0
  %t2840 = and i64 %t2838, %t2839
  store i64 %t2840, ptr %t2804
  %t2841 = load i64, ptr %t2804
  %t2842 = call %NxVal @nx_int(i64 %t2841)
  ret %NxVal %t2842
}
define %NxVal @nx__m_3____main____Cell__m35(%NxVal* %args, i64 %nargs) {
entry:
  %t2843 = alloca %NxVal
  %t2847 = alloca i64
  %t2862 = alloca i64
  store %NxVal zeroinitializer, ptr %t2843
  %t2844 = getelementptr %NxVal, ptr %args, i64 0
  %t2845 = load %NxVal, ptr %t2844
  %t2846 = call %NxVal @nx_clone(%NxVal %t2845)
  store %NxVal %t2846, ptr %t2843
  %t2848 = getelementptr %NxVal, ptr %args, i64 1
  %t2849 = load %NxVal, ptr %t2848
  %t2850 = extractvalue %NxVal %t2849, 1
  store i64 %t2850, ptr %t2847
  %t2851 = load %NxVal, ptr %t2843
  %t2852 = call %NxVal @nx_rec_get(%NxVal %t2851, i64 0)
  %t2853 = load %NxVal, ptr %t2843
  %t2854 = call %NxVal @nx_rec_get(%NxVal %t2853, i64 1)
  %t2855 = call %NxVal @nx_add(%NxVal %t2852, %NxVal %t2854)
  %t2856 = load i64, ptr %t2847
  %t2857 = call %NxVal @nx_int(i64 %t2856)
  %t2858 = call %NxVal @nx_add(%NxVal %t2855, %NxVal %t2857)
  %t2859 = add i64 65535, 0
  %t2860 = call %NxVal @nx_int(i64 %t2859)
  %t2861 = call %NxVal @nx_bitand(%NxVal %t2858, %NxVal %t2860)
  %t2863 = extractvalue %NxVal %t2861, 1
  store i64 %t2863, ptr %t2862
  %t2864 = load i64, ptr %t2862
  %t2865 = add i64 48, 0
  %t2866 = or i64 %t2864, %t2865
  %t2867 = add i64 30, 0
  %t2868 = or i64 %t2866, %t2867
  %t2869 = add i64 65535, 0
  %t2870 = and i64 %t2868, %t2869
  store i64 %t2870, ptr %t2862
  %t2871 = load i64, ptr %t2862
  %t2872 = add i64 24, 0
  %t2873 = xor i64 %t2871, %t2872
  %t2874 = add i64 56, 0
  %t2875 = xor i64 %t2873, %t2874
  %t2876 = add i64 65535, 0
  %t2877 = and i64 %t2875, %t2876
  store i64 %t2877, ptr %t2862
  %t2878 = load i64, ptr %t2862
  %t2879 = add i64 22, 0
  %t2880 = or i64 %t2878, %t2879
  %t2881 = add i64 64, 0
  %t2882 = or i64 %t2880, %t2881
  %t2883 = add i64 65535, 0
  %t2884 = and i64 %t2882, %t2883
  store i64 %t2884, ptr %t2862
  %t2885 = load i64, ptr %t2862
  %t2886 = add i64 52, 0
  %t2887 = call i64 @nx_mod_i64(i64 %t2885, i64 %t2886)
  %t2888 = add i64 55, 0
  %t2889 = call i64 @nx_mod_i64(i64 %t2887, i64 %t2888)
  %t2890 = add i64 65535, 0
  %t2891 = and i64 %t2889, %t2890
  store i64 %t2891, ptr %t2862
  %t2892 = load i64, ptr %t2862
  %t2893 = add i64 40, 0
  %t2894 = xor i64 %t2892, %t2893
  %t2895 = add i64 57, 0
  %t2896 = xor i64 %t2894, %t2895
  %t2897 = add i64 65535, 0
  %t2898 = and i64 %t2896, %t2897
  store i64 %t2898, ptr %t2862
  %t2899 = load i64, ptr %t2862
  %t2900 = call %NxVal @nx_int(i64 %t2899)
  ret %NxVal %t2900
}
define %NxVal @nx__m_3____main____Cell__m36(%NxVal* %args, i64 %nargs) {
entry:
  %t2901 = alloca %NxVal
  %t2905 = alloca i64
  %t2920 = alloca i64
  store %NxVal zeroinitializer, ptr %t2901
  %t2902 = getelementptr %NxVal, ptr %args, i64 0
  %t2903 = load %NxVal, ptr %t2902
  %t2904 = call %NxVal @nx_clone(%NxVal %t2903)
  store %NxVal %t2904, ptr %t2901
  %t2906 = getelementptr %NxVal, ptr %args, i64 1
  %t2907 = load %NxVal, ptr %t2906
  %t2908 = extractvalue %NxVal %t2907, 1
  store i64 %t2908, ptr %t2905
  %t2909 = load %NxVal, ptr %t2901
  %t2910 = call %NxVal @nx_rec_get(%NxVal %t2909, i64 0)
  %t2911 = load %NxVal, ptr %t2901
  %t2912 = call %NxVal @nx_rec_get(%NxVal %t2911, i64 1)
  %t2913 = call %NxVal @nx_add(%NxVal %t2910, %NxVal %t2912)
  %t2914 = load i64, ptr %t2905
  %t2915 = call %NxVal @nx_int(i64 %t2914)
  %t2916 = call %NxVal @nx_add(%NxVal %t2913, %NxVal %t2915)
  %t2917 = add i64 65535, 0
  %t2918 = call %NxVal @nx_int(i64 %t2917)
  %t2919 = call %NxVal @nx_bitand(%NxVal %t2916, %NxVal %t2918)
  %t2921 = extractvalue %NxVal %t2919, 1
  store i64 %t2921, ptr %t2920
  %t2922 = load i64, ptr %t2920
  %t2923 = add i64 81, 0
  %t2924 = and i64 %t2922, %t2923
  %t2925 = add i64 71, 0
  %t2926 = and i64 %t2924, %t2925
  %t2927 = add i64 65535, 0
  %t2928 = and i64 %t2926, %t2927
  store i64 %t2928, ptr %t2920
  %t2929 = load i64, ptr %t2920
  %t2930 = add i64 38, 0
  %t2931 = add i64 %t2929, %t2930
  %t2932 = add i64 6, 0
  %t2933 = add i64 %t2931, %t2932
  %t2934 = add i64 65535, 0
  %t2935 = and i64 %t2933, %t2934
  store i64 %t2935, ptr %t2920
  %t2936 = load i64, ptr %t2920
  %t2937 = add i64 8, 0
  %t2938 = or i64 %t2936, %t2937
  %t2939 = add i64 35, 0
  %t2940 = or i64 %t2938, %t2939
  %t2941 = add i64 65535, 0
  %t2942 = and i64 %t2940, %t2941
  store i64 %t2942, ptr %t2920
  %t2943 = load i64, ptr %t2920
  %t2944 = add i64 47, 0
  %t2945 = sub i64 %t2943, %t2944
  %t2946 = add i64 85, 0
  %t2947 = sub i64 %t2945, %t2946
  %t2948 = add i64 65535, 0
  %t2949 = and i64 %t2947, %t2948
  store i64 %t2949, ptr %t2920
  %t2950 = load i64, ptr %t2920
  %t2951 = add i64 31, 0
  %t2952 = call i64 @nx_mod_i64(i64 %t2950, i64 %t2951)
  %t2953 = add i64 15, 0
  %t2954 = call i64 @nx_mod_i64(i64 %t2952, i64 %t2953)
  %t2955 = add i64 65535, 0
  %t2956 = and i64 %t2954, %t2955
  store i64 %t2956, ptr %t2920
  %t2957 = load i64, ptr %t2920
  %t2958 = call %NxVal @nx_int(i64 %t2957)
  ret %NxVal %t2958
}
define %NxVal @nx__m_3____main____Cell__m37(%NxVal* %args, i64 %nargs) {
entry:
  %t2959 = alloca %NxVal
  %t2963 = alloca i64
  %t2978 = alloca i64
  store %NxVal zeroinitializer, ptr %t2959
  %t2960 = getelementptr %NxVal, ptr %args, i64 0
  %t2961 = load %NxVal, ptr %t2960
  %t2962 = call %NxVal @nx_clone(%NxVal %t2961)
  store %NxVal %t2962, ptr %t2959
  %t2964 = getelementptr %NxVal, ptr %args, i64 1
  %t2965 = load %NxVal, ptr %t2964
  %t2966 = extractvalue %NxVal %t2965, 1
  store i64 %t2966, ptr %t2963
  %t2967 = load %NxVal, ptr %t2959
  %t2968 = call %NxVal @nx_rec_get(%NxVal %t2967, i64 0)
  %t2969 = load %NxVal, ptr %t2959
  %t2970 = call %NxVal @nx_rec_get(%NxVal %t2969, i64 1)
  %t2971 = call %NxVal @nx_add(%NxVal %t2968, %NxVal %t2970)
  %t2972 = load i64, ptr %t2963
  %t2973 = call %NxVal @nx_int(i64 %t2972)
  %t2974 = call %NxVal @nx_add(%NxVal %t2971, %NxVal %t2973)
  %t2975 = add i64 65535, 0
  %t2976 = call %NxVal @nx_int(i64 %t2975)
  %t2977 = call %NxVal @nx_bitand(%NxVal %t2974, %NxVal %t2976)
  %t2979 = extractvalue %NxVal %t2977, 1
  store i64 %t2979, ptr %t2978
  %t2980 = load i64, ptr %t2978
  %t2981 = add i64 4, 0
  %t2982 = mul i64 %t2980, %t2981
  %t2983 = add i64 55, 0
  %t2984 = mul i64 %t2982, %t2983
  %t2985 = add i64 65535, 0
  %t2986 = and i64 %t2984, %t2985
  store i64 %t2986, ptr %t2978
  %t2987 = load i64, ptr %t2978
  %t2988 = add i64 53, 0
  %t2989 = or i64 %t2987, %t2988
  %t2990 = add i64 19, 0
  %t2991 = or i64 %t2989, %t2990
  %t2992 = add i64 65535, 0
  %t2993 = and i64 %t2991, %t2992
  store i64 %t2993, ptr %t2978
  %t2994 = load i64, ptr %t2978
  %t2995 = add i64 71, 0
  %t2996 = xor i64 %t2994, %t2995
  %t2997 = add i64 35, 0
  %t2998 = xor i64 %t2996, %t2997
  %t2999 = add i64 65535, 0
  %t3000 = and i64 %t2998, %t2999
  store i64 %t3000, ptr %t2978
  %t3001 = load i64, ptr %t2978
  %t3002 = add i64 64, 0
  %t3003 = add i64 %t3001, %t3002
  %t3004 = add i64 4, 0
  %t3005 = add i64 %t3003, %t3004
  %t3006 = add i64 65535, 0
  %t3007 = and i64 %t3005, %t3006
  store i64 %t3007, ptr %t2978
  %t3008 = load i64, ptr %t2978
  %t3009 = add i64 41, 0
  %t3010 = or i64 %t3008, %t3009
  %t3011 = add i64 39, 0
  %t3012 = or i64 %t3010, %t3011
  %t3013 = add i64 65535, 0
  %t3014 = and i64 %t3012, %t3013
  store i64 %t3014, ptr %t2978
  %t3015 = load i64, ptr %t2978
  %t3016 = call %NxVal @nx_int(i64 %t3015)
  ret %NxVal %t3016
}
define %NxVal @nx__m_3____main____Cell__m38(%NxVal* %args, i64 %nargs) {
entry:
  %t3017 = alloca %NxVal
  %t3021 = alloca i64
  %t3036 = alloca i64
  store %NxVal zeroinitializer, ptr %t3017
  %t3018 = getelementptr %NxVal, ptr %args, i64 0
  %t3019 = load %NxVal, ptr %t3018
  %t3020 = call %NxVal @nx_clone(%NxVal %t3019)
  store %NxVal %t3020, ptr %t3017
  %t3022 = getelementptr %NxVal, ptr %args, i64 1
  %t3023 = load %NxVal, ptr %t3022
  %t3024 = extractvalue %NxVal %t3023, 1
  store i64 %t3024, ptr %t3021
  %t3025 = load %NxVal, ptr %t3017
  %t3026 = call %NxVal @nx_rec_get(%NxVal %t3025, i64 0)
  %t3027 = load %NxVal, ptr %t3017
  %t3028 = call %NxVal @nx_rec_get(%NxVal %t3027, i64 1)
  %t3029 = call %NxVal @nx_add(%NxVal %t3026, %NxVal %t3028)
  %t3030 = load i64, ptr %t3021
  %t3031 = call %NxVal @nx_int(i64 %t3030)
  %t3032 = call %NxVal @nx_add(%NxVal %t3029, %NxVal %t3031)
  %t3033 = add i64 65535, 0
  %t3034 = call %NxVal @nx_int(i64 %t3033)
  %t3035 = call %NxVal @nx_bitand(%NxVal %t3032, %NxVal %t3034)
  %t3037 = extractvalue %NxVal %t3035, 1
  store i64 %t3037, ptr %t3036
  %t3038 = load i64, ptr %t3036
  %t3039 = add i64 30, 0
  %t3040 = and i64 %t3038, %t3039
  %t3041 = add i64 63, 0
  %t3042 = and i64 %t3040, %t3041
  %t3043 = add i64 65535, 0
  %t3044 = and i64 %t3042, %t3043
  store i64 %t3044, ptr %t3036
  %t3045 = load i64, ptr %t3036
  %t3046 = add i64 57, 0
  %t3047 = or i64 %t3045, %t3046
  %t3048 = add i64 19, 0
  %t3049 = or i64 %t3047, %t3048
  %t3050 = add i64 65535, 0
  %t3051 = and i64 %t3049, %t3050
  store i64 %t3051, ptr %t3036
  %t3052 = load i64, ptr %t3036
  %t3053 = add i64 9, 0
  %t3054 = add i64 %t3052, %t3053
  %t3055 = add i64 31, 0
  %t3056 = add i64 %t3054, %t3055
  %t3057 = add i64 65535, 0
  %t3058 = and i64 %t3056, %t3057
  store i64 %t3058, ptr %t3036
  %t3059 = load i64, ptr %t3036
  %t3060 = add i64 14, 0
  %t3061 = add i64 %t3059, %t3060
  %t3062 = add i64 87, 0
  %t3063 = add i64 %t3061, %t3062
  %t3064 = add i64 65535, 0
  %t3065 = and i64 %t3063, %t3064
  store i64 %t3065, ptr %t3036
  %t3066 = load i64, ptr %t3036
  %t3067 = add i64 75, 0
  %t3068 = add i64 %t3066, %t3067
  %t3069 = add i64 2, 0
  %t3070 = add i64 %t3068, %t3069
  %t3071 = add i64 65535, 0
  %t3072 = and i64 %t3070, %t3071
  store i64 %t3072, ptr %t3036
  %t3073 = load i64, ptr %t3036
  %t3074 = call %NxVal @nx_int(i64 %t3073)
  ret %NxVal %t3074
}
define %NxVal @nx__m_3____main____Cell__m39(%NxVal* %args, i64 %nargs) {
entry:
  %t3075 = alloca %NxVal
  %t3079 = alloca i64
  %t3094 = alloca i64
  store %NxVal zeroinitializer, ptr %t3075
  %t3076 = getelementptr %NxVal, ptr %args, i64 0
  %t3077 = load %NxVal, ptr %t3076
  %t3078 = call %NxVal @nx_clone(%NxVal %t3077)
  store %NxVal %t3078, ptr %t3075
  %t3080 = getelementptr %NxVal, ptr %args, i64 1
  %t3081 = load %NxVal, ptr %t3080
  %t3082 = extractvalue %NxVal %t3081, 1
  store i64 %t3082, ptr %t3079
  %t3083 = load %NxVal, ptr %t3075
  %t3084 = call %NxVal @nx_rec_get(%NxVal %t3083, i64 0)
  %t3085 = load %NxVal, ptr %t3075
  %t3086 = call %NxVal @nx_rec_get(%NxVal %t3085, i64 1)
  %t3087 = call %NxVal @nx_add(%NxVal %t3084, %NxVal %t3086)
  %t3088 = load i64, ptr %t3079
  %t3089 = call %NxVal @nx_int(i64 %t3088)
  %t3090 = call %NxVal @nx_add(%NxVal %t3087, %NxVal %t3089)
  %t3091 = add i64 65535, 0
  %t3092 = call %NxVal @nx_int(i64 %t3091)
  %t3093 = call %NxVal @nx_bitand(%NxVal %t3090, %NxVal %t3092)
  %t3095 = extractvalue %NxVal %t3093, 1
  store i64 %t3095, ptr %t3094
  %t3096 = load i64, ptr %t3094
  %t3097 = add i64 93, 0
  %t3098 = add i64 %t3096, %t3097
  %t3099 = add i64 10, 0
  %t3100 = add i64 %t3098, %t3099
  %t3101 = add i64 65535, 0
  %t3102 = and i64 %t3100, %t3101
  store i64 %t3102, ptr %t3094
  %t3103 = load i64, ptr %t3094
  %t3104 = add i64 86, 0
  %t3105 = sub i64 %t3103, %t3104
  %t3106 = add i64 4, 0
  %t3107 = sub i64 %t3105, %t3106
  %t3108 = add i64 65535, 0
  %t3109 = and i64 %t3107, %t3108
  store i64 %t3109, ptr %t3094
  %t3110 = load i64, ptr %t3094
  %t3111 = add i64 26, 0
  %t3112 = and i64 %t3110, %t3111
  %t3113 = add i64 29, 0
  %t3114 = and i64 %t3112, %t3113
  %t3115 = add i64 65535, 0
  %t3116 = and i64 %t3114, %t3115
  store i64 %t3116, ptr %t3094
  %t3117 = load i64, ptr %t3094
  %t3118 = add i64 96, 0
  %t3119 = mul i64 %t3117, %t3118
  %t3120 = add i64 16, 0
  %t3121 = mul i64 %t3119, %t3120
  %t3122 = add i64 65535, 0
  %t3123 = and i64 %t3121, %t3122
  store i64 %t3123, ptr %t3094
  %t3124 = load i64, ptr %t3094
  %t3125 = add i64 97, 0
  %t3126 = add i64 %t3124, %t3125
  %t3127 = add i64 25, 0
  %t3128 = add i64 %t3126, %t3127
  %t3129 = add i64 65535, 0
  %t3130 = and i64 %t3128, %t3129
  store i64 %t3130, ptr %t3094
  %t3131 = load i64, ptr %t3094
  %t3132 = call %NxVal @nx_int(i64 %t3131)
  ret %NxVal %t3132
}
define %NxVal @nx__m_2____main____Cell__m4(%NxVal* %args, i64 %nargs) {
entry:
  %t3133 = alloca %NxVal
  %t3137 = alloca i64
  %t3152 = alloca i64
  store %NxVal zeroinitializer, ptr %t3133
  %t3134 = getelementptr %NxVal, ptr %args, i64 0
  %t3135 = load %NxVal, ptr %t3134
  %t3136 = call %NxVal @nx_clone(%NxVal %t3135)
  store %NxVal %t3136, ptr %t3133
  %t3138 = getelementptr %NxVal, ptr %args, i64 1
  %t3139 = load %NxVal, ptr %t3138
  %t3140 = extractvalue %NxVal %t3139, 1
  store i64 %t3140, ptr %t3137
  %t3141 = load %NxVal, ptr %t3133
  %t3142 = call %NxVal @nx_rec_get(%NxVal %t3141, i64 0)
  %t3143 = load %NxVal, ptr %t3133
  %t3144 = call %NxVal @nx_rec_get(%NxVal %t3143, i64 1)
  %t3145 = call %NxVal @nx_add(%NxVal %t3142, %NxVal %t3144)
  %t3146 = load i64, ptr %t3137
  %t3147 = call %NxVal @nx_int(i64 %t3146)
  %t3148 = call %NxVal @nx_add(%NxVal %t3145, %NxVal %t3147)
  %t3149 = add i64 65535, 0
  %t3150 = call %NxVal @nx_int(i64 %t3149)
  %t3151 = call %NxVal @nx_bitand(%NxVal %t3148, %NxVal %t3150)
  %t3153 = extractvalue %NxVal %t3151, 1
  store i64 %t3153, ptr %t3152
  %t3154 = load i64, ptr %t3152
  %t3155 = add i64 32, 0
  %t3156 = add i64 %t3154, %t3155
  %t3157 = add i64 6, 0
  %t3158 = add i64 %t3156, %t3157
  %t3159 = add i64 65535, 0
  %t3160 = and i64 %t3158, %t3159
  store i64 %t3160, ptr %t3152
  %t3161 = load i64, ptr %t3152
  %t3162 = add i64 19, 0
  %t3163 = xor i64 %t3161, %t3162
  %t3164 = add i64 4, 0
  %t3165 = xor i64 %t3163, %t3164
  %t3166 = add i64 65535, 0
  %t3167 = and i64 %t3165, %t3166
  store i64 %t3167, ptr %t3152
  %t3168 = load i64, ptr %t3152
  %t3169 = add i64 31, 0
  %t3170 = sub i64 %t3168, %t3169
  %t3171 = add i64 41, 0
  %t3172 = sub i64 %t3170, %t3171
  %t3173 = add i64 65535, 0
  %t3174 = and i64 %t3172, %t3173
  store i64 %t3174, ptr %t3152
  %t3175 = load i64, ptr %t3152
  %t3176 = add i64 37, 0
  %t3177 = sub i64 %t3175, %t3176
  %t3178 = add i64 81, 0
  %t3179 = sub i64 %t3177, %t3178
  %t3180 = add i64 65535, 0
  %t3181 = and i64 %t3179, %t3180
  store i64 %t3181, ptr %t3152
  %t3182 = load i64, ptr %t3152
  %t3183 = add i64 91, 0
  %t3184 = or i64 %t3182, %t3183
  %t3185 = add i64 15, 0
  %t3186 = or i64 %t3184, %t3185
  %t3187 = add i64 65535, 0
  %t3188 = and i64 %t3186, %t3187
  store i64 %t3188, ptr %t3152
  %t3189 = load i64, ptr %t3152
  %t3190 = call %NxVal @nx_int(i64 %t3189)
  ret %NxVal %t3190
}
define %NxVal @nx__m_3____main____Cell__m40(%NxVal* %args, i64 %nargs) {
entry:
  %t3191 = alloca %NxVal
  %t3195 = alloca i64
  %t3210 = alloca i64
  store %NxVal zeroinitializer, ptr %t3191
  %t3192 = getelementptr %NxVal, ptr %args, i64 0
  %t3193 = load %NxVal, ptr %t3192
  %t3194 = call %NxVal @nx_clone(%NxVal %t3193)
  store %NxVal %t3194, ptr %t3191
  %t3196 = getelementptr %NxVal, ptr %args, i64 1
  %t3197 = load %NxVal, ptr %t3196
  %t3198 = extractvalue %NxVal %t3197, 1
  store i64 %t3198, ptr %t3195
  %t3199 = load %NxVal, ptr %t3191
  %t3200 = call %NxVal @nx_rec_get(%NxVal %t3199, i64 0)
  %t3201 = load %NxVal, ptr %t3191
  %t3202 = call %NxVal @nx_rec_get(%NxVal %t3201, i64 1)
  %t3203 = call %NxVal @nx_add(%NxVal %t3200, %NxVal %t3202)
  %t3204 = load i64, ptr %t3195
  %t3205 = call %NxVal @nx_int(i64 %t3204)
  %t3206 = call %NxVal @nx_add(%NxVal %t3203, %NxVal %t3205)
  %t3207 = add i64 65535, 0
  %t3208 = call %NxVal @nx_int(i64 %t3207)
  %t3209 = call %NxVal @nx_bitand(%NxVal %t3206, %NxVal %t3208)
  %t3211 = extractvalue %NxVal %t3209, 1
  store i64 %t3211, ptr %t3210
  %t3212 = load i64, ptr %t3210
  %t3213 = add i64 50, 0
  %t3214 = and i64 %t3212, %t3213
  %t3215 = add i64 78, 0
  %t3216 = and i64 %t3214, %t3215
  %t3217 = add i64 65535, 0
  %t3218 = and i64 %t3216, %t3217
  store i64 %t3218, ptr %t3210
  %t3219 = load i64, ptr %t3210
  %t3220 = add i64 6, 0
  %t3221 = call i64 @nx_mod_i64(i64 %t3219, i64 %t3220)
  %t3222 = add i64 23, 0
  %t3223 = call i64 @nx_mod_i64(i64 %t3221, i64 %t3222)
  %t3224 = add i64 65535, 0
  %t3225 = and i64 %t3223, %t3224
  store i64 %t3225, ptr %t3210
  %t3226 = load i64, ptr %t3210
  %t3227 = add i64 27, 0
  %t3228 = mul i64 %t3226, %t3227
  %t3229 = add i64 4, 0
  %t3230 = mul i64 %t3228, %t3229
  %t3231 = add i64 65535, 0
  %t3232 = and i64 %t3230, %t3231
  store i64 %t3232, ptr %t3210
  %t3233 = load i64, ptr %t3210
  %t3234 = add i64 3, 0
  %t3235 = mul i64 %t3233, %t3234
  %t3236 = add i64 6, 0
  %t3237 = mul i64 %t3235, %t3236
  %t3238 = add i64 65535, 0
  %t3239 = and i64 %t3237, %t3238
  store i64 %t3239, ptr %t3210
  %t3240 = load i64, ptr %t3210
  %t3241 = add i64 35, 0
  %t3242 = add i64 %t3240, %t3241
  %t3243 = add i64 18, 0
  %t3244 = add i64 %t3242, %t3243
  %t3245 = add i64 65535, 0
  %t3246 = and i64 %t3244, %t3245
  store i64 %t3246, ptr %t3210
  %t3247 = load i64, ptr %t3210
  %t3248 = call %NxVal @nx_int(i64 %t3247)
  ret %NxVal %t3248
}
define %NxVal @nx__m_3____main____Cell__m41(%NxVal* %args, i64 %nargs) {
entry:
  %t3249 = alloca %NxVal
  %t3253 = alloca i64
  %t3268 = alloca i64
  store %NxVal zeroinitializer, ptr %t3249
  %t3250 = getelementptr %NxVal, ptr %args, i64 0
  %t3251 = load %NxVal, ptr %t3250
  %t3252 = call %NxVal @nx_clone(%NxVal %t3251)
  store %NxVal %t3252, ptr %t3249
  %t3254 = getelementptr %NxVal, ptr %args, i64 1
  %t3255 = load %NxVal, ptr %t3254
  %t3256 = extractvalue %NxVal %t3255, 1
  store i64 %t3256, ptr %t3253
  %t3257 = load %NxVal, ptr %t3249
  %t3258 = call %NxVal @nx_rec_get(%NxVal %t3257, i64 0)
  %t3259 = load %NxVal, ptr %t3249
  %t3260 = call %NxVal @nx_rec_get(%NxVal %t3259, i64 1)
  %t3261 = call %NxVal @nx_add(%NxVal %t3258, %NxVal %t3260)
  %t3262 = load i64, ptr %t3253
  %t3263 = call %NxVal @nx_int(i64 %t3262)
  %t3264 = call %NxVal @nx_add(%NxVal %t3261, %NxVal %t3263)
  %t3265 = add i64 65535, 0
  %t3266 = call %NxVal @nx_int(i64 %t3265)
  %t3267 = call %NxVal @nx_bitand(%NxVal %t3264, %NxVal %t3266)
  %t3269 = extractvalue %NxVal %t3267, 1
  store i64 %t3269, ptr %t3268
  %t3270 = load i64, ptr %t3268
  %t3271 = add i64 20, 0
  %t3272 = call i64 @nx_mod_i64(i64 %t3270, i64 %t3271)
  %t3273 = add i64 24, 0
  %t3274 = call i64 @nx_mod_i64(i64 %t3272, i64 %t3273)
  %t3275 = add i64 65535, 0
  %t3276 = and i64 %t3274, %t3275
  store i64 %t3276, ptr %t3268
  %t3277 = load i64, ptr %t3268
  %t3278 = add i64 32, 0
  %t3279 = and i64 %t3277, %t3278
  %t3280 = add i64 36, 0
  %t3281 = and i64 %t3279, %t3280
  %t3282 = add i64 65535, 0
  %t3283 = and i64 %t3281, %t3282
  store i64 %t3283, ptr %t3268
  %t3284 = load i64, ptr %t3268
  %t3285 = add i64 2, 0
  %t3286 = call i64 @nx_mod_i64(i64 %t3284, i64 %t3285)
  %t3287 = add i64 14, 0
  %t3288 = call i64 @nx_mod_i64(i64 %t3286, i64 %t3287)
  %t3289 = add i64 65535, 0
  %t3290 = and i64 %t3288, %t3289
  store i64 %t3290, ptr %t3268
  %t3291 = load i64, ptr %t3268
  %t3292 = add i64 78, 0
  %t3293 = xor i64 %t3291, %t3292
  %t3294 = add i64 42, 0
  %t3295 = xor i64 %t3293, %t3294
  %t3296 = add i64 65535, 0
  %t3297 = and i64 %t3295, %t3296
  store i64 %t3297, ptr %t3268
  %t3298 = load i64, ptr %t3268
  %t3299 = add i64 86, 0
  %t3300 = call i64 @nx_mod_i64(i64 %t3298, i64 %t3299)
  %t3301 = add i64 51, 0
  %t3302 = call i64 @nx_mod_i64(i64 %t3300, i64 %t3301)
  %t3303 = add i64 65535, 0
  %t3304 = and i64 %t3302, %t3303
  store i64 %t3304, ptr %t3268
  %t3305 = load i64, ptr %t3268
  %t3306 = call %NxVal @nx_int(i64 %t3305)
  ret %NxVal %t3306
}
define %NxVal @nx__m_3____main____Cell__m42(%NxVal* %args, i64 %nargs) {
entry:
  %t3307 = alloca %NxVal
  %t3311 = alloca i64
  %t3326 = alloca i64
  store %NxVal zeroinitializer, ptr %t3307
  %t3308 = getelementptr %NxVal, ptr %args, i64 0
  %t3309 = load %NxVal, ptr %t3308
  %t3310 = call %NxVal @nx_clone(%NxVal %t3309)
  store %NxVal %t3310, ptr %t3307
  %t3312 = getelementptr %NxVal, ptr %args, i64 1
  %t3313 = load %NxVal, ptr %t3312
  %t3314 = extractvalue %NxVal %t3313, 1
  store i64 %t3314, ptr %t3311
  %t3315 = load %NxVal, ptr %t3307
  %t3316 = call %NxVal @nx_rec_get(%NxVal %t3315, i64 0)
  %t3317 = load %NxVal, ptr %t3307
  %t3318 = call %NxVal @nx_rec_get(%NxVal %t3317, i64 1)
  %t3319 = call %NxVal @nx_add(%NxVal %t3316, %NxVal %t3318)
  %t3320 = load i64, ptr %t3311
  %t3321 = call %NxVal @nx_int(i64 %t3320)
  %t3322 = call %NxVal @nx_add(%NxVal %t3319, %NxVal %t3321)
  %t3323 = add i64 65535, 0
  %t3324 = call %NxVal @nx_int(i64 %t3323)
  %t3325 = call %NxVal @nx_bitand(%NxVal %t3322, %NxVal %t3324)
  %t3327 = extractvalue %NxVal %t3325, 1
  store i64 %t3327, ptr %t3326
  %t3328 = load i64, ptr %t3326
  %t3329 = add i64 5, 0
  %t3330 = sub i64 %t3328, %t3329
  %t3331 = add i64 8, 0
  %t3332 = sub i64 %t3330, %t3331
  %t3333 = add i64 65535, 0
  %t3334 = and i64 %t3332, %t3333
  store i64 %t3334, ptr %t3326
  %t3335 = load i64, ptr %t3326
  %t3336 = add i64 19, 0
  %t3337 = add i64 %t3335, %t3336
  %t3338 = add i64 26, 0
  %t3339 = add i64 %t3337, %t3338
  %t3340 = add i64 65535, 0
  %t3341 = and i64 %t3339, %t3340
  store i64 %t3341, ptr %t3326
  %t3342 = load i64, ptr %t3326
  %t3343 = add i64 75, 0
  %t3344 = and i64 %t3342, %t3343
  %t3345 = add i64 3, 0
  %t3346 = and i64 %t3344, %t3345
  %t3347 = add i64 65535, 0
  %t3348 = and i64 %t3346, %t3347
  store i64 %t3348, ptr %t3326
  %t3349 = load i64, ptr %t3326
  %t3350 = add i64 79, 0
  %t3351 = xor i64 %t3349, %t3350
  %t3352 = add i64 71, 0
  %t3353 = xor i64 %t3351, %t3352
  %t3354 = add i64 65535, 0
  %t3355 = and i64 %t3353, %t3354
  store i64 %t3355, ptr %t3326
  %t3356 = load i64, ptr %t3326
  %t3357 = add i64 65, 0
  %t3358 = xor i64 %t3356, %t3357
  %t3359 = add i64 65, 0
  %t3360 = xor i64 %t3358, %t3359
  %t3361 = add i64 65535, 0
  %t3362 = and i64 %t3360, %t3361
  store i64 %t3362, ptr %t3326
  %t3363 = load i64, ptr %t3326
  %t3364 = call %NxVal @nx_int(i64 %t3363)
  ret %NxVal %t3364
}
define %NxVal @nx__m_3____main____Cell__m43(%NxVal* %args, i64 %nargs) {
entry:
  %t3365 = alloca %NxVal
  %t3369 = alloca i64
  %t3384 = alloca i64
  store %NxVal zeroinitializer, ptr %t3365
  %t3366 = getelementptr %NxVal, ptr %args, i64 0
  %t3367 = load %NxVal, ptr %t3366
  %t3368 = call %NxVal @nx_clone(%NxVal %t3367)
  store %NxVal %t3368, ptr %t3365
  %t3370 = getelementptr %NxVal, ptr %args, i64 1
  %t3371 = load %NxVal, ptr %t3370
  %t3372 = extractvalue %NxVal %t3371, 1
  store i64 %t3372, ptr %t3369
  %t3373 = load %NxVal, ptr %t3365
  %t3374 = call %NxVal @nx_rec_get(%NxVal %t3373, i64 0)
  %t3375 = load %NxVal, ptr %t3365
  %t3376 = call %NxVal @nx_rec_get(%NxVal %t3375, i64 1)
  %t3377 = call %NxVal @nx_add(%NxVal %t3374, %NxVal %t3376)
  %t3378 = load i64, ptr %t3369
  %t3379 = call %NxVal @nx_int(i64 %t3378)
  %t3380 = call %NxVal @nx_add(%NxVal %t3377, %NxVal %t3379)
  %t3381 = add i64 65535, 0
  %t3382 = call %NxVal @nx_int(i64 %t3381)
  %t3383 = call %NxVal @nx_bitand(%NxVal %t3380, %NxVal %t3382)
  %t3385 = extractvalue %NxVal %t3383, 1
  store i64 %t3385, ptr %t3384
  %t3386 = load i64, ptr %t3384
  %t3387 = add i64 60, 0
  %t3388 = xor i64 %t3386, %t3387
  %t3389 = add i64 33, 0
  %t3390 = xor i64 %t3388, %t3389
  %t3391 = add i64 65535, 0
  %t3392 = and i64 %t3390, %t3391
  store i64 %t3392, ptr %t3384
  %t3393 = load i64, ptr %t3384
  %t3394 = add i64 38, 0
  %t3395 = mul i64 %t3393, %t3394
  %t3396 = add i64 3, 0
  %t3397 = mul i64 %t3395, %t3396
  %t3398 = add i64 65535, 0
  %t3399 = and i64 %t3397, %t3398
  store i64 %t3399, ptr %t3384
  %t3400 = load i64, ptr %t3384
  %t3401 = add i64 90, 0
  %t3402 = and i64 %t3400, %t3401
  %t3403 = add i64 65, 0
  %t3404 = and i64 %t3402, %t3403
  %t3405 = add i64 65535, 0
  %t3406 = and i64 %t3404, %t3405
  store i64 %t3406, ptr %t3384
  %t3407 = load i64, ptr %t3384
  %t3408 = add i64 82, 0
  %t3409 = mul i64 %t3407, %t3408
  %t3410 = add i64 50, 0
  %t3411 = mul i64 %t3409, %t3410
  %t3412 = add i64 65535, 0
  %t3413 = and i64 %t3411, %t3412
  store i64 %t3413, ptr %t3384
  %t3414 = load i64, ptr %t3384
  %t3415 = add i64 16, 0
  %t3416 = call i64 @nx_mod_i64(i64 %t3414, i64 %t3415)
  %t3417 = add i64 83, 0
  %t3418 = call i64 @nx_mod_i64(i64 %t3416, i64 %t3417)
  %t3419 = add i64 65535, 0
  %t3420 = and i64 %t3418, %t3419
  store i64 %t3420, ptr %t3384
  %t3421 = load i64, ptr %t3384
  %t3422 = call %NxVal @nx_int(i64 %t3421)
  ret %NxVal %t3422
}
define %NxVal @nx__m_3____main____Cell__m44(%NxVal* %args, i64 %nargs) {
entry:
  %t3423 = alloca %NxVal
  %t3427 = alloca i64
  %t3442 = alloca i64
  store %NxVal zeroinitializer, ptr %t3423
  %t3424 = getelementptr %NxVal, ptr %args, i64 0
  %t3425 = load %NxVal, ptr %t3424
  %t3426 = call %NxVal @nx_clone(%NxVal %t3425)
  store %NxVal %t3426, ptr %t3423
  %t3428 = getelementptr %NxVal, ptr %args, i64 1
  %t3429 = load %NxVal, ptr %t3428
  %t3430 = extractvalue %NxVal %t3429, 1
  store i64 %t3430, ptr %t3427
  %t3431 = load %NxVal, ptr %t3423
  %t3432 = call %NxVal @nx_rec_get(%NxVal %t3431, i64 0)
  %t3433 = load %NxVal, ptr %t3423
  %t3434 = call %NxVal @nx_rec_get(%NxVal %t3433, i64 1)
  %t3435 = call %NxVal @nx_add(%NxVal %t3432, %NxVal %t3434)
  %t3436 = load i64, ptr %t3427
  %t3437 = call %NxVal @nx_int(i64 %t3436)
  %t3438 = call %NxVal @nx_add(%NxVal %t3435, %NxVal %t3437)
  %t3439 = add i64 65535, 0
  %t3440 = call %NxVal @nx_int(i64 %t3439)
  %t3441 = call %NxVal @nx_bitand(%NxVal %t3438, %NxVal %t3440)
  %t3443 = extractvalue %NxVal %t3441, 1
  store i64 %t3443, ptr %t3442
  %t3444 = load i64, ptr %t3442
  %t3445 = add i64 89, 0
  %t3446 = add i64 %t3444, %t3445
  %t3447 = add i64 20, 0
  %t3448 = add i64 %t3446, %t3447
  %t3449 = add i64 65535, 0
  %t3450 = and i64 %t3448, %t3449
  store i64 %t3450, ptr %t3442
  %t3451 = load i64, ptr %t3442
  %t3452 = add i64 67, 0
  %t3453 = mul i64 %t3451, %t3452
  %t3454 = add i64 27, 0
  %t3455 = mul i64 %t3453, %t3454
  %t3456 = add i64 65535, 0
  %t3457 = and i64 %t3455, %t3456
  store i64 %t3457, ptr %t3442
  %t3458 = load i64, ptr %t3442
  %t3459 = add i64 81, 0
  %t3460 = mul i64 %t3458, %t3459
  %t3461 = add i64 41, 0
  %t3462 = mul i64 %t3460, %t3461
  %t3463 = add i64 65535, 0
  %t3464 = and i64 %t3462, %t3463
  store i64 %t3464, ptr %t3442
  %t3465 = load i64, ptr %t3442
  %t3466 = add i64 54, 0
  %t3467 = sub i64 %t3465, %t3466
  %t3468 = add i64 84, 0
  %t3469 = sub i64 %t3467, %t3468
  %t3470 = add i64 65535, 0
  %t3471 = and i64 %t3469, %t3470
  store i64 %t3471, ptr %t3442
  %t3472 = load i64, ptr %t3442
  %t3473 = add i64 94, 0
  %t3474 = add i64 %t3472, %t3473
  %t3475 = add i64 79, 0
  %t3476 = add i64 %t3474, %t3475
  %t3477 = add i64 65535, 0
  %t3478 = and i64 %t3476, %t3477
  store i64 %t3478, ptr %t3442
  %t3479 = load i64, ptr %t3442
  %t3480 = call %NxVal @nx_int(i64 %t3479)
  ret %NxVal %t3480
}
define %NxVal @nx__m_3____main____Cell__m45(%NxVal* %args, i64 %nargs) {
entry:
  %t3481 = alloca %NxVal
  %t3485 = alloca i64
  %t3500 = alloca i64
  store %NxVal zeroinitializer, ptr %t3481
  %t3482 = getelementptr %NxVal, ptr %args, i64 0
  %t3483 = load %NxVal, ptr %t3482
  %t3484 = call %NxVal @nx_clone(%NxVal %t3483)
  store %NxVal %t3484, ptr %t3481
  %t3486 = getelementptr %NxVal, ptr %args, i64 1
  %t3487 = load %NxVal, ptr %t3486
  %t3488 = extractvalue %NxVal %t3487, 1
  store i64 %t3488, ptr %t3485
  %t3489 = load %NxVal, ptr %t3481
  %t3490 = call %NxVal @nx_rec_get(%NxVal %t3489, i64 0)
  %t3491 = load %NxVal, ptr %t3481
  %t3492 = call %NxVal @nx_rec_get(%NxVal %t3491, i64 1)
  %t3493 = call %NxVal @nx_add(%NxVal %t3490, %NxVal %t3492)
  %t3494 = load i64, ptr %t3485
  %t3495 = call %NxVal @nx_int(i64 %t3494)
  %t3496 = call %NxVal @nx_add(%NxVal %t3493, %NxVal %t3495)
  %t3497 = add i64 65535, 0
  %t3498 = call %NxVal @nx_int(i64 %t3497)
  %t3499 = call %NxVal @nx_bitand(%NxVal %t3496, %NxVal %t3498)
  %t3501 = extractvalue %NxVal %t3499, 1
  store i64 %t3501, ptr %t3500
  %t3502 = load i64, ptr %t3500
  %t3503 = add i64 62, 0
  %t3504 = sub i64 %t3502, %t3503
  %t3505 = add i64 72, 0
  %t3506 = sub i64 %t3504, %t3505
  %t3507 = add i64 65535, 0
  %t3508 = and i64 %t3506, %t3507
  store i64 %t3508, ptr %t3500
  %t3509 = load i64, ptr %t3500
  %t3510 = add i64 2, 0
  %t3511 = call i64 @nx_mod_i64(i64 %t3509, i64 %t3510)
  %t3512 = add i64 46, 0
  %t3513 = call i64 @nx_mod_i64(i64 %t3511, i64 %t3512)
  %t3514 = add i64 65535, 0
  %t3515 = and i64 %t3513, %t3514
  store i64 %t3515, ptr %t3500
  %t3516 = load i64, ptr %t3500
  %t3517 = add i64 59, 0
  %t3518 = mul i64 %t3516, %t3517
  %t3519 = add i64 68, 0
  %t3520 = mul i64 %t3518, %t3519
  %t3521 = add i64 65535, 0
  %t3522 = and i64 %t3520, %t3521
  store i64 %t3522, ptr %t3500
  %t3523 = load i64, ptr %t3500
  %t3524 = add i64 55, 0
  %t3525 = or i64 %t3523, %t3524
  %t3526 = add i64 18, 0
  %t3527 = or i64 %t3525, %t3526
  %t3528 = add i64 65535, 0
  %t3529 = and i64 %t3527, %t3528
  store i64 %t3529, ptr %t3500
  %t3530 = load i64, ptr %t3500
  %t3531 = add i64 69, 0
  %t3532 = or i64 %t3530, %t3531
  %t3533 = add i64 12, 0
  %t3534 = or i64 %t3532, %t3533
  %t3535 = add i64 65535, 0
  %t3536 = and i64 %t3534, %t3535
  store i64 %t3536, ptr %t3500
  %t3537 = load i64, ptr %t3500
  %t3538 = call %NxVal @nx_int(i64 %t3537)
  ret %NxVal %t3538
}
define %NxVal @nx__m_3____main____Cell__m46(%NxVal* %args, i64 %nargs) {
entry:
  %t3539 = alloca %NxVal
  %t3543 = alloca i64
  %t3558 = alloca i64
  store %NxVal zeroinitializer, ptr %t3539
  %t3540 = getelementptr %NxVal, ptr %args, i64 0
  %t3541 = load %NxVal, ptr %t3540
  %t3542 = call %NxVal @nx_clone(%NxVal %t3541)
  store %NxVal %t3542, ptr %t3539
  %t3544 = getelementptr %NxVal, ptr %args, i64 1
  %t3545 = load %NxVal, ptr %t3544
  %t3546 = extractvalue %NxVal %t3545, 1
  store i64 %t3546, ptr %t3543
  %t3547 = load %NxVal, ptr %t3539
  %t3548 = call %NxVal @nx_rec_get(%NxVal %t3547, i64 0)
  %t3549 = load %NxVal, ptr %t3539
  %t3550 = call %NxVal @nx_rec_get(%NxVal %t3549, i64 1)
  %t3551 = call %NxVal @nx_add(%NxVal %t3548, %NxVal %t3550)
  %t3552 = load i64, ptr %t3543
  %t3553 = call %NxVal @nx_int(i64 %t3552)
  %t3554 = call %NxVal @nx_add(%NxVal %t3551, %NxVal %t3553)
  %t3555 = add i64 65535, 0
  %t3556 = call %NxVal @nx_int(i64 %t3555)
  %t3557 = call %NxVal @nx_bitand(%NxVal %t3554, %NxVal %t3556)
  %t3559 = extractvalue %NxVal %t3557, 1
  store i64 %t3559, ptr %t3558
  %t3560 = load i64, ptr %t3558
  %t3561 = add i64 15, 0
  %t3562 = and i64 %t3560, %t3561
  %t3563 = add i64 86, 0
  %t3564 = and i64 %t3562, %t3563
  %t3565 = add i64 65535, 0
  %t3566 = and i64 %t3564, %t3565
  store i64 %t3566, ptr %t3558
  %t3567 = load i64, ptr %t3558
  %t3568 = add i64 88, 0
  %t3569 = and i64 %t3567, %t3568
  %t3570 = add i64 43, 0
  %t3571 = and i64 %t3569, %t3570
  %t3572 = add i64 65535, 0
  %t3573 = and i64 %t3571, %t3572
  store i64 %t3573, ptr %t3558
  %t3574 = load i64, ptr %t3558
  %t3575 = add i64 31, 0
  %t3576 = sub i64 %t3574, %t3575
  %t3577 = add i64 20, 0
  %t3578 = sub i64 %t3576, %t3577
  %t3579 = add i64 65535, 0
  %t3580 = and i64 %t3578, %t3579
  store i64 %t3580, ptr %t3558
  %t3581 = load i64, ptr %t3558
  %t3582 = add i64 53, 0
  %t3583 = mul i64 %t3581, %t3582
  %t3584 = add i64 83, 0
  %t3585 = mul i64 %t3583, %t3584
  %t3586 = add i64 65535, 0
  %t3587 = and i64 %t3585, %t3586
  store i64 %t3587, ptr %t3558
  %t3588 = load i64, ptr %t3558
  %t3589 = add i64 75, 0
  %t3590 = mul i64 %t3588, %t3589
  %t3591 = add i64 9, 0
  %t3592 = mul i64 %t3590, %t3591
  %t3593 = add i64 65535, 0
  %t3594 = and i64 %t3592, %t3593
  store i64 %t3594, ptr %t3558
  %t3595 = load i64, ptr %t3558
  %t3596 = call %NxVal @nx_int(i64 %t3595)
  ret %NxVal %t3596
}
define %NxVal @nx__m_3____main____Cell__m47(%NxVal* %args, i64 %nargs) {
entry:
  %t3597 = alloca %NxVal
  %t3601 = alloca i64
  %t3616 = alloca i64
  store %NxVal zeroinitializer, ptr %t3597
  %t3598 = getelementptr %NxVal, ptr %args, i64 0
  %t3599 = load %NxVal, ptr %t3598
  %t3600 = call %NxVal @nx_clone(%NxVal %t3599)
  store %NxVal %t3600, ptr %t3597
  %t3602 = getelementptr %NxVal, ptr %args, i64 1
  %t3603 = load %NxVal, ptr %t3602
  %t3604 = extractvalue %NxVal %t3603, 1
  store i64 %t3604, ptr %t3601
  %t3605 = load %NxVal, ptr %t3597
  %t3606 = call %NxVal @nx_rec_get(%NxVal %t3605, i64 0)
  %t3607 = load %NxVal, ptr %t3597
  %t3608 = call %NxVal @nx_rec_get(%NxVal %t3607, i64 1)
  %t3609 = call %NxVal @nx_add(%NxVal %t3606, %NxVal %t3608)
  %t3610 = load i64, ptr %t3601
  %t3611 = call %NxVal @nx_int(i64 %t3610)
  %t3612 = call %NxVal @nx_add(%NxVal %t3609, %NxVal %t3611)
  %t3613 = add i64 65535, 0
  %t3614 = call %NxVal @nx_int(i64 %t3613)
  %t3615 = call %NxVal @nx_bitand(%NxVal %t3612, %NxVal %t3614)
  %t3617 = extractvalue %NxVal %t3615, 1
  store i64 %t3617, ptr %t3616
  %t3618 = load i64, ptr %t3616
  %t3619 = add i64 65, 0
  %t3620 = call i64 @nx_mod_i64(i64 %t3618, i64 %t3619)
  %t3621 = add i64 3, 0
  %t3622 = call i64 @nx_mod_i64(i64 %t3620, i64 %t3621)
  %t3623 = add i64 65535, 0
  %t3624 = and i64 %t3622, %t3623
  store i64 %t3624, ptr %t3616
  %t3625 = load i64, ptr %t3616
  %t3626 = add i64 64, 0
  %t3627 = and i64 %t3625, %t3626
  %t3628 = add i64 49, 0
  %t3629 = and i64 %t3627, %t3628
  %t3630 = add i64 65535, 0
  %t3631 = and i64 %t3629, %t3630
  store i64 %t3631, ptr %t3616
  %t3632 = load i64, ptr %t3616
  %t3633 = add i64 15, 0
  %t3634 = sub i64 %t3632, %t3633
  %t3635 = add i64 88, 0
  %t3636 = sub i64 %t3634, %t3635
  %t3637 = add i64 65535, 0
  %t3638 = and i64 %t3636, %t3637
  store i64 %t3638, ptr %t3616
  %t3639 = load i64, ptr %t3616
  %t3640 = add i64 18, 0
  %t3641 = xor i64 %t3639, %t3640
  %t3642 = add i64 12, 0
  %t3643 = xor i64 %t3641, %t3642
  %t3644 = add i64 65535, 0
  %t3645 = and i64 %t3643, %t3644
  store i64 %t3645, ptr %t3616
  %t3646 = load i64, ptr %t3616
  %t3647 = add i64 44, 0
  %t3648 = mul i64 %t3646, %t3647
  %t3649 = add i64 63, 0
  %t3650 = mul i64 %t3648, %t3649
  %t3651 = add i64 65535, 0
  %t3652 = and i64 %t3650, %t3651
  store i64 %t3652, ptr %t3616
  %t3653 = load i64, ptr %t3616
  %t3654 = call %NxVal @nx_int(i64 %t3653)
  ret %NxVal %t3654
}
define %NxVal @nx__m_3____main____Cell__m48(%NxVal* %args, i64 %nargs) {
entry:
  %t3655 = alloca %NxVal
  %t3659 = alloca i64
  %t3674 = alloca i64
  store %NxVal zeroinitializer, ptr %t3655
  %t3656 = getelementptr %NxVal, ptr %args, i64 0
  %t3657 = load %NxVal, ptr %t3656
  %t3658 = call %NxVal @nx_clone(%NxVal %t3657)
  store %NxVal %t3658, ptr %t3655
  %t3660 = getelementptr %NxVal, ptr %args, i64 1
  %t3661 = load %NxVal, ptr %t3660
  %t3662 = extractvalue %NxVal %t3661, 1
  store i64 %t3662, ptr %t3659
  %t3663 = load %NxVal, ptr %t3655
  %t3664 = call %NxVal @nx_rec_get(%NxVal %t3663, i64 0)
  %t3665 = load %NxVal, ptr %t3655
  %t3666 = call %NxVal @nx_rec_get(%NxVal %t3665, i64 1)
  %t3667 = call %NxVal @nx_add(%NxVal %t3664, %NxVal %t3666)
  %t3668 = load i64, ptr %t3659
  %t3669 = call %NxVal @nx_int(i64 %t3668)
  %t3670 = call %NxVal @nx_add(%NxVal %t3667, %NxVal %t3669)
  %t3671 = add i64 65535, 0
  %t3672 = call %NxVal @nx_int(i64 %t3671)
  %t3673 = call %NxVal @nx_bitand(%NxVal %t3670, %NxVal %t3672)
  %t3675 = extractvalue %NxVal %t3673, 1
  store i64 %t3675, ptr %t3674
  %t3676 = load i64, ptr %t3674
  %t3677 = add i64 67, 0
  %t3678 = sub i64 %t3676, %t3677
  %t3679 = add i64 84, 0
  %t3680 = sub i64 %t3678, %t3679
  %t3681 = add i64 65535, 0
  %t3682 = and i64 %t3680, %t3681
  store i64 %t3682, ptr %t3674
  %t3683 = load i64, ptr %t3674
  %t3684 = add i64 18, 0
  %t3685 = call i64 @nx_mod_i64(i64 %t3683, i64 %t3684)
  %t3686 = add i64 17, 0
  %t3687 = call i64 @nx_mod_i64(i64 %t3685, i64 %t3686)
  %t3688 = add i64 65535, 0
  %t3689 = and i64 %t3687, %t3688
  store i64 %t3689, ptr %t3674
  %t3690 = load i64, ptr %t3674
  %t3691 = add i64 83, 0
  %t3692 = xor i64 %t3690, %t3691
  %t3693 = add i64 6, 0
  %t3694 = xor i64 %t3692, %t3693
  %t3695 = add i64 65535, 0
  %t3696 = and i64 %t3694, %t3695
  store i64 %t3696, ptr %t3674
  %t3697 = load i64, ptr %t3674
  %t3698 = add i64 84, 0
  %t3699 = sub i64 %t3697, %t3698
  %t3700 = add i64 40, 0
  %t3701 = sub i64 %t3699, %t3700
  %t3702 = add i64 65535, 0
  %t3703 = and i64 %t3701, %t3702
  store i64 %t3703, ptr %t3674
  %t3704 = load i64, ptr %t3674
  %t3705 = add i64 54, 0
  %t3706 = and i64 %t3704, %t3705
  %t3707 = add i64 10, 0
  %t3708 = and i64 %t3706, %t3707
  %t3709 = add i64 65535, 0
  %t3710 = and i64 %t3708, %t3709
  store i64 %t3710, ptr %t3674
  %t3711 = load i64, ptr %t3674
  %t3712 = call %NxVal @nx_int(i64 %t3711)
  ret %NxVal %t3712
}
define %NxVal @nx__m_3____main____Cell__m49(%NxVal* %args, i64 %nargs) {
entry:
  %t3713 = alloca %NxVal
  %t3717 = alloca i64
  %t3732 = alloca i64
  store %NxVal zeroinitializer, ptr %t3713
  %t3714 = getelementptr %NxVal, ptr %args, i64 0
  %t3715 = load %NxVal, ptr %t3714
  %t3716 = call %NxVal @nx_clone(%NxVal %t3715)
  store %NxVal %t3716, ptr %t3713
  %t3718 = getelementptr %NxVal, ptr %args, i64 1
  %t3719 = load %NxVal, ptr %t3718
  %t3720 = extractvalue %NxVal %t3719, 1
  store i64 %t3720, ptr %t3717
  %t3721 = load %NxVal, ptr %t3713
  %t3722 = call %NxVal @nx_rec_get(%NxVal %t3721, i64 0)
  %t3723 = load %NxVal, ptr %t3713
  %t3724 = call %NxVal @nx_rec_get(%NxVal %t3723, i64 1)
  %t3725 = call %NxVal @nx_add(%NxVal %t3722, %NxVal %t3724)
  %t3726 = load i64, ptr %t3717
  %t3727 = call %NxVal @nx_int(i64 %t3726)
  %t3728 = call %NxVal @nx_add(%NxVal %t3725, %NxVal %t3727)
  %t3729 = add i64 65535, 0
  %t3730 = call %NxVal @nx_int(i64 %t3729)
  %t3731 = call %NxVal @nx_bitand(%NxVal %t3728, %NxVal %t3730)
  %t3733 = extractvalue %NxVal %t3731, 1
  store i64 %t3733, ptr %t3732
  %t3734 = load i64, ptr %t3732
  %t3735 = add i64 30, 0
  %t3736 = add i64 %t3734, %t3735
  %t3737 = add i64 52, 0
  %t3738 = add i64 %t3736, %t3737
  %t3739 = add i64 65535, 0
  %t3740 = and i64 %t3738, %t3739
  store i64 %t3740, ptr %t3732
  %t3741 = load i64, ptr %t3732
  %t3742 = add i64 62, 0
  %t3743 = sub i64 %t3741, %t3742
  %t3744 = add i64 12, 0
  %t3745 = sub i64 %t3743, %t3744
  %t3746 = add i64 65535, 0
  %t3747 = and i64 %t3745, %t3746
  store i64 %t3747, ptr %t3732
  %t3748 = load i64, ptr %t3732
  %t3749 = add i64 58, 0
  %t3750 = and i64 %t3748, %t3749
  %t3751 = add i64 7, 0
  %t3752 = and i64 %t3750, %t3751
  %t3753 = add i64 65535, 0
  %t3754 = and i64 %t3752, %t3753
  store i64 %t3754, ptr %t3732
  %t3755 = load i64, ptr %t3732
  %t3756 = add i64 97, 0
  %t3757 = sub i64 %t3755, %t3756
  %t3758 = add i64 64, 0
  %t3759 = sub i64 %t3757, %t3758
  %t3760 = add i64 65535, 0
  %t3761 = and i64 %t3759, %t3760
  store i64 %t3761, ptr %t3732
  %t3762 = load i64, ptr %t3732
  %t3763 = add i64 79, 0
  %t3764 = or i64 %t3762, %t3763
  %t3765 = add i64 85, 0
  %t3766 = or i64 %t3764, %t3765
  %t3767 = add i64 65535, 0
  %t3768 = and i64 %t3766, %t3767
  store i64 %t3768, ptr %t3732
  %t3769 = load i64, ptr %t3732
  %t3770 = call %NxVal @nx_int(i64 %t3769)
  ret %NxVal %t3770
}
define %NxVal @nx__m_2____main____Cell__m5(%NxVal* %args, i64 %nargs) {
entry:
  %t3771 = alloca %NxVal
  %t3775 = alloca i64
  %t3790 = alloca i64
  store %NxVal zeroinitializer, ptr %t3771
  %t3772 = getelementptr %NxVal, ptr %args, i64 0
  %t3773 = load %NxVal, ptr %t3772
  %t3774 = call %NxVal @nx_clone(%NxVal %t3773)
  store %NxVal %t3774, ptr %t3771
  %t3776 = getelementptr %NxVal, ptr %args, i64 1
  %t3777 = load %NxVal, ptr %t3776
  %t3778 = extractvalue %NxVal %t3777, 1
  store i64 %t3778, ptr %t3775
  %t3779 = load %NxVal, ptr %t3771
  %t3780 = call %NxVal @nx_rec_get(%NxVal %t3779, i64 0)
  %t3781 = load %NxVal, ptr %t3771
  %t3782 = call %NxVal @nx_rec_get(%NxVal %t3781, i64 1)
  %t3783 = call %NxVal @nx_add(%NxVal %t3780, %NxVal %t3782)
  %t3784 = load i64, ptr %t3775
  %t3785 = call %NxVal @nx_int(i64 %t3784)
  %t3786 = call %NxVal @nx_add(%NxVal %t3783, %NxVal %t3785)
  %t3787 = add i64 65535, 0
  %t3788 = call %NxVal @nx_int(i64 %t3787)
  %t3789 = call %NxVal @nx_bitand(%NxVal %t3786, %NxVal %t3788)
  %t3791 = extractvalue %NxVal %t3789, 1
  store i64 %t3791, ptr %t3790
  %t3792 = load i64, ptr %t3790
  %t3793 = add i64 37, 0
  %t3794 = sub i64 %t3792, %t3793
  %t3795 = add i64 9, 0
  %t3796 = sub i64 %t3794, %t3795
  %t3797 = add i64 65535, 0
  %t3798 = and i64 %t3796, %t3797
  store i64 %t3798, ptr %t3790
  %t3799 = load i64, ptr %t3790
  %t3800 = add i64 58, 0
  %t3801 = and i64 %t3799, %t3800
  %t3802 = add i64 58, 0
  %t3803 = and i64 %t3801, %t3802
  %t3804 = add i64 65535, 0
  %t3805 = and i64 %t3803, %t3804
  store i64 %t3805, ptr %t3790
  %t3806 = load i64, ptr %t3790
  %t3807 = add i64 6, 0
  %t3808 = or i64 %t3806, %t3807
  %t3809 = add i64 87, 0
  %t3810 = or i64 %t3808, %t3809
  %t3811 = add i64 65535, 0
  %t3812 = and i64 %t3810, %t3811
  store i64 %t3812, ptr %t3790
  %t3813 = load i64, ptr %t3790
  %t3814 = add i64 6, 0
  %t3815 = call i64 @nx_mod_i64(i64 %t3813, i64 %t3814)
  %t3816 = add i64 63, 0
  %t3817 = call i64 @nx_mod_i64(i64 %t3815, i64 %t3816)
  %t3818 = add i64 65535, 0
  %t3819 = and i64 %t3817, %t3818
  store i64 %t3819, ptr %t3790
  %t3820 = load i64, ptr %t3790
  %t3821 = add i64 73, 0
  %t3822 = and i64 %t3820, %t3821
  %t3823 = add i64 89, 0
  %t3824 = and i64 %t3822, %t3823
  %t3825 = add i64 65535, 0
  %t3826 = and i64 %t3824, %t3825
  store i64 %t3826, ptr %t3790
  %t3827 = load i64, ptr %t3790
  %t3828 = call %NxVal @nx_int(i64 %t3827)
  ret %NxVal %t3828
}
define %NxVal @nx__m_3____main____Cell__m50(%NxVal* %args, i64 %nargs) {
entry:
  %t3829 = alloca %NxVal
  %t3833 = alloca i64
  %t3848 = alloca i64
  store %NxVal zeroinitializer, ptr %t3829
  %t3830 = getelementptr %NxVal, ptr %args, i64 0
  %t3831 = load %NxVal, ptr %t3830
  %t3832 = call %NxVal @nx_clone(%NxVal %t3831)
  store %NxVal %t3832, ptr %t3829
  %t3834 = getelementptr %NxVal, ptr %args, i64 1
  %t3835 = load %NxVal, ptr %t3834
  %t3836 = extractvalue %NxVal %t3835, 1
  store i64 %t3836, ptr %t3833
  %t3837 = load %NxVal, ptr %t3829
  %t3838 = call %NxVal @nx_rec_get(%NxVal %t3837, i64 0)
  %t3839 = load %NxVal, ptr %t3829
  %t3840 = call %NxVal @nx_rec_get(%NxVal %t3839, i64 1)
  %t3841 = call %NxVal @nx_add(%NxVal %t3838, %NxVal %t3840)
  %t3842 = load i64, ptr %t3833
  %t3843 = call %NxVal @nx_int(i64 %t3842)
  %t3844 = call %NxVal @nx_add(%NxVal %t3841, %NxVal %t3843)
  %t3845 = add i64 65535, 0
  %t3846 = call %NxVal @nx_int(i64 %t3845)
  %t3847 = call %NxVal @nx_bitand(%NxVal %t3844, %NxVal %t3846)
  %t3849 = extractvalue %NxVal %t3847, 1
  store i64 %t3849, ptr %t3848
  %t3850 = load i64, ptr %t3848
  %t3851 = add i64 16, 0
  %t3852 = add i64 %t3850, %t3851
  %t3853 = add i64 53, 0
  %t3854 = add i64 %t3852, %t3853
  %t3855 = add i64 65535, 0
  %t3856 = and i64 %t3854, %t3855
  store i64 %t3856, ptr %t3848
  %t3857 = load i64, ptr %t3848
  %t3858 = add i64 41, 0
  %t3859 = add i64 %t3857, %t3858
  %t3860 = add i64 51, 0
  %t3861 = add i64 %t3859, %t3860
  %t3862 = add i64 65535, 0
  %t3863 = and i64 %t3861, %t3862
  store i64 %t3863, ptr %t3848
  %t3864 = load i64, ptr %t3848
  %t3865 = add i64 49, 0
  %t3866 = call i64 @nx_mod_i64(i64 %t3864, i64 %t3865)
  %t3867 = add i64 3, 0
  %t3868 = call i64 @nx_mod_i64(i64 %t3866, i64 %t3867)
  %t3869 = add i64 65535, 0
  %t3870 = and i64 %t3868, %t3869
  store i64 %t3870, ptr %t3848
  %t3871 = load i64, ptr %t3848
  %t3872 = add i64 95, 0
  %t3873 = xor i64 %t3871, %t3872
  %t3874 = add i64 37, 0
  %t3875 = xor i64 %t3873, %t3874
  %t3876 = add i64 65535, 0
  %t3877 = and i64 %t3875, %t3876
  store i64 %t3877, ptr %t3848
  %t3878 = load i64, ptr %t3848
  %t3879 = add i64 43, 0
  %t3880 = or i64 %t3878, %t3879
  %t3881 = add i64 31, 0
  %t3882 = or i64 %t3880, %t3881
  %t3883 = add i64 65535, 0
  %t3884 = and i64 %t3882, %t3883
  store i64 %t3884, ptr %t3848
  %t3885 = load i64, ptr %t3848
  %t3886 = call %NxVal @nx_int(i64 %t3885)
  ret %NxVal %t3886
}
define %NxVal @nx__m_3____main____Cell__m51(%NxVal* %args, i64 %nargs) {
entry:
  %t3887 = alloca %NxVal
  %t3891 = alloca i64
  %t3906 = alloca i64
  store %NxVal zeroinitializer, ptr %t3887
  %t3888 = getelementptr %NxVal, ptr %args, i64 0
  %t3889 = load %NxVal, ptr %t3888
  %t3890 = call %NxVal @nx_clone(%NxVal %t3889)
  store %NxVal %t3890, ptr %t3887
  %t3892 = getelementptr %NxVal, ptr %args, i64 1
  %t3893 = load %NxVal, ptr %t3892
  %t3894 = extractvalue %NxVal %t3893, 1
  store i64 %t3894, ptr %t3891
  %t3895 = load %NxVal, ptr %t3887
  %t3896 = call %NxVal @nx_rec_get(%NxVal %t3895, i64 0)
  %t3897 = load %NxVal, ptr %t3887
  %t3898 = call %NxVal @nx_rec_get(%NxVal %t3897, i64 1)
  %t3899 = call %NxVal @nx_add(%NxVal %t3896, %NxVal %t3898)
  %t3900 = load i64, ptr %t3891
  %t3901 = call %NxVal @nx_int(i64 %t3900)
  %t3902 = call %NxVal @nx_add(%NxVal %t3899, %NxVal %t3901)
  %t3903 = add i64 65535, 0
  %t3904 = call %NxVal @nx_int(i64 %t3903)
  %t3905 = call %NxVal @nx_bitand(%NxVal %t3902, %NxVal %t3904)
  %t3907 = extractvalue %NxVal %t3905, 1
  store i64 %t3907, ptr %t3906
  %t3908 = load i64, ptr %t3906
  %t3909 = add i64 70, 0
  %t3910 = and i64 %t3908, %t3909
  %t3911 = add i64 68, 0
  %t3912 = and i64 %t3910, %t3911
  %t3913 = add i64 65535, 0
  %t3914 = and i64 %t3912, %t3913
  store i64 %t3914, ptr %t3906
  %t3915 = load i64, ptr %t3906
  %t3916 = add i64 12, 0
  %t3917 = mul i64 %t3915, %t3916
  %t3918 = add i64 13, 0
  %t3919 = mul i64 %t3917, %t3918
  %t3920 = add i64 65535, 0
  %t3921 = and i64 %t3919, %t3920
  store i64 %t3921, ptr %t3906
  %t3922 = load i64, ptr %t3906
  %t3923 = add i64 73, 0
  %t3924 = or i64 %t3922, %t3923
  %t3925 = add i64 26, 0
  %t3926 = or i64 %t3924, %t3925
  %t3927 = add i64 65535, 0
  %t3928 = and i64 %t3926, %t3927
  store i64 %t3928, ptr %t3906
  %t3929 = load i64, ptr %t3906
  %t3930 = add i64 34, 0
  %t3931 = add i64 %t3929, %t3930
  %t3932 = add i64 40, 0
  %t3933 = add i64 %t3931, %t3932
  %t3934 = add i64 65535, 0
  %t3935 = and i64 %t3933, %t3934
  store i64 %t3935, ptr %t3906
  %t3936 = load i64, ptr %t3906
  %t3937 = add i64 10, 0
  %t3938 = or i64 %t3936, %t3937
  %t3939 = add i64 27, 0
  %t3940 = or i64 %t3938, %t3939
  %t3941 = add i64 65535, 0
  %t3942 = and i64 %t3940, %t3941
  store i64 %t3942, ptr %t3906
  %t3943 = load i64, ptr %t3906
  %t3944 = call %NxVal @nx_int(i64 %t3943)
  ret %NxVal %t3944
}
define %NxVal @nx__m_3____main____Cell__m52(%NxVal* %args, i64 %nargs) {
entry:
  %t3945 = alloca %NxVal
  %t3949 = alloca i64
  %t3964 = alloca i64
  store %NxVal zeroinitializer, ptr %t3945
  %t3946 = getelementptr %NxVal, ptr %args, i64 0
  %t3947 = load %NxVal, ptr %t3946
  %t3948 = call %NxVal @nx_clone(%NxVal %t3947)
  store %NxVal %t3948, ptr %t3945
  %t3950 = getelementptr %NxVal, ptr %args, i64 1
  %t3951 = load %NxVal, ptr %t3950
  %t3952 = extractvalue %NxVal %t3951, 1
  store i64 %t3952, ptr %t3949
  %t3953 = load %NxVal, ptr %t3945
  %t3954 = call %NxVal @nx_rec_get(%NxVal %t3953, i64 0)
  %t3955 = load %NxVal, ptr %t3945
  %t3956 = call %NxVal @nx_rec_get(%NxVal %t3955, i64 1)
  %t3957 = call %NxVal @nx_add(%NxVal %t3954, %NxVal %t3956)
  %t3958 = load i64, ptr %t3949
  %t3959 = call %NxVal @nx_int(i64 %t3958)
  %t3960 = call %NxVal @nx_add(%NxVal %t3957, %NxVal %t3959)
  %t3961 = add i64 65535, 0
  %t3962 = call %NxVal @nx_int(i64 %t3961)
  %t3963 = call %NxVal @nx_bitand(%NxVal %t3960, %NxVal %t3962)
  %t3965 = extractvalue %NxVal %t3963, 1
  store i64 %t3965, ptr %t3964
  %t3966 = load i64, ptr %t3964
  %t3967 = add i64 84, 0
  %t3968 = sub i64 %t3966, %t3967
  %t3969 = add i64 15, 0
  %t3970 = sub i64 %t3968, %t3969
  %t3971 = add i64 65535, 0
  %t3972 = and i64 %t3970, %t3971
  store i64 %t3972, ptr %t3964
  %t3973 = load i64, ptr %t3964
  %t3974 = add i64 45, 0
  %t3975 = add i64 %t3973, %t3974
  %t3976 = add i64 84, 0
  %t3977 = add i64 %t3975, %t3976
  %t3978 = add i64 65535, 0
  %t3979 = and i64 %t3977, %t3978
  store i64 %t3979, ptr %t3964
  %t3980 = load i64, ptr %t3964
  %t3981 = add i64 55, 0
  %t3982 = xor i64 %t3980, %t3981
  %t3983 = add i64 52, 0
  %t3984 = xor i64 %t3982, %t3983
  %t3985 = add i64 65535, 0
  %t3986 = and i64 %t3984, %t3985
  store i64 %t3986, ptr %t3964
  %t3987 = load i64, ptr %t3964
  %t3988 = add i64 85, 0
  %t3989 = call i64 @nx_mod_i64(i64 %t3987, i64 %t3988)
  %t3990 = add i64 65, 0
  %t3991 = call i64 @nx_mod_i64(i64 %t3989, i64 %t3990)
  %t3992 = add i64 65535, 0
  %t3993 = and i64 %t3991, %t3992
  store i64 %t3993, ptr %t3964
  %t3994 = load i64, ptr %t3964
  %t3995 = add i64 90, 0
  %t3996 = sub i64 %t3994, %t3995
  %t3997 = add i64 33, 0
  %t3998 = sub i64 %t3996, %t3997
  %t3999 = add i64 65535, 0
  %t4000 = and i64 %t3998, %t3999
  store i64 %t4000, ptr %t3964
  %t4001 = load i64, ptr %t3964
  %t4002 = call %NxVal @nx_int(i64 %t4001)
  ret %NxVal %t4002
}
define %NxVal @nx__m_3____main____Cell__m53(%NxVal* %args, i64 %nargs) {
entry:
  %t4003 = alloca %NxVal
  %t4007 = alloca i64
  %t4022 = alloca i64
  store %NxVal zeroinitializer, ptr %t4003
  %t4004 = getelementptr %NxVal, ptr %args, i64 0
  %t4005 = load %NxVal, ptr %t4004
  %t4006 = call %NxVal @nx_clone(%NxVal %t4005)
  store %NxVal %t4006, ptr %t4003
  %t4008 = getelementptr %NxVal, ptr %args, i64 1
  %t4009 = load %NxVal, ptr %t4008
  %t4010 = extractvalue %NxVal %t4009, 1
  store i64 %t4010, ptr %t4007
  %t4011 = load %NxVal, ptr %t4003
  %t4012 = call %NxVal @nx_rec_get(%NxVal %t4011, i64 0)
  %t4013 = load %NxVal, ptr %t4003
  %t4014 = call %NxVal @nx_rec_get(%NxVal %t4013, i64 1)
  %t4015 = call %NxVal @nx_add(%NxVal %t4012, %NxVal %t4014)
  %t4016 = load i64, ptr %t4007
  %t4017 = call %NxVal @nx_int(i64 %t4016)
  %t4018 = call %NxVal @nx_add(%NxVal %t4015, %NxVal %t4017)
  %t4019 = add i64 65535, 0
  %t4020 = call %NxVal @nx_int(i64 %t4019)
  %t4021 = call %NxVal @nx_bitand(%NxVal %t4018, %NxVal %t4020)
  %t4023 = extractvalue %NxVal %t4021, 1
  store i64 %t4023, ptr %t4022
  %t4024 = load i64, ptr %t4022
  %t4025 = add i64 46, 0
  %t4026 = or i64 %t4024, %t4025
  %t4027 = add i64 65, 0
  %t4028 = or i64 %t4026, %t4027
  %t4029 = add i64 65535, 0
  %t4030 = and i64 %t4028, %t4029
  store i64 %t4030, ptr %t4022
  %t4031 = load i64, ptr %t4022
  %t4032 = add i64 73, 0
  %t4033 = or i64 %t4031, %t4032
  %t4034 = add i64 16, 0
  %t4035 = or i64 %t4033, %t4034
  %t4036 = add i64 65535, 0
  %t4037 = and i64 %t4035, %t4036
  store i64 %t4037, ptr %t4022
  %t4038 = load i64, ptr %t4022
  %t4039 = add i64 41, 0
  %t4040 = call i64 @nx_mod_i64(i64 %t4038, i64 %t4039)
  %t4041 = add i64 65, 0
  %t4042 = call i64 @nx_mod_i64(i64 %t4040, i64 %t4041)
  %t4043 = add i64 65535, 0
  %t4044 = and i64 %t4042, %t4043
  store i64 %t4044, ptr %t4022
  %t4045 = load i64, ptr %t4022
  %t4046 = add i64 91, 0
  %t4047 = mul i64 %t4045, %t4046
  %t4048 = add i64 69, 0
  %t4049 = mul i64 %t4047, %t4048
  %t4050 = add i64 65535, 0
  %t4051 = and i64 %t4049, %t4050
  store i64 %t4051, ptr %t4022
  %t4052 = load i64, ptr %t4022
  %t4053 = add i64 93, 0
  %t4054 = and i64 %t4052, %t4053
  %t4055 = add i64 82, 0
  %t4056 = and i64 %t4054, %t4055
  %t4057 = add i64 65535, 0
  %t4058 = and i64 %t4056, %t4057
  store i64 %t4058, ptr %t4022
  %t4059 = load i64, ptr %t4022
  %t4060 = call %NxVal @nx_int(i64 %t4059)
  ret %NxVal %t4060
}
define %NxVal @nx__m_3____main____Cell__m54(%NxVal* %args, i64 %nargs) {
entry:
  %t4061 = alloca %NxVal
  %t4065 = alloca i64
  %t4080 = alloca i64
  store %NxVal zeroinitializer, ptr %t4061
  %t4062 = getelementptr %NxVal, ptr %args, i64 0
  %t4063 = load %NxVal, ptr %t4062
  %t4064 = call %NxVal @nx_clone(%NxVal %t4063)
  store %NxVal %t4064, ptr %t4061
  %t4066 = getelementptr %NxVal, ptr %args, i64 1
  %t4067 = load %NxVal, ptr %t4066
  %t4068 = extractvalue %NxVal %t4067, 1
  store i64 %t4068, ptr %t4065
  %t4069 = load %NxVal, ptr %t4061
  %t4070 = call %NxVal @nx_rec_get(%NxVal %t4069, i64 0)
  %t4071 = load %NxVal, ptr %t4061
  %t4072 = call %NxVal @nx_rec_get(%NxVal %t4071, i64 1)
  %t4073 = call %NxVal @nx_add(%NxVal %t4070, %NxVal %t4072)
  %t4074 = load i64, ptr %t4065
  %t4075 = call %NxVal @nx_int(i64 %t4074)
  %t4076 = call %NxVal @nx_add(%NxVal %t4073, %NxVal %t4075)
  %t4077 = add i64 65535, 0
  %t4078 = call %NxVal @nx_int(i64 %t4077)
  %t4079 = call %NxVal @nx_bitand(%NxVal %t4076, %NxVal %t4078)
  %t4081 = extractvalue %NxVal %t4079, 1
  store i64 %t4081, ptr %t4080
  %t4082 = load i64, ptr %t4080
  %t4083 = add i64 43, 0
  %t4084 = mul i64 %t4082, %t4083
  %t4085 = add i64 48, 0
  %t4086 = mul i64 %t4084, %t4085
  %t4087 = add i64 65535, 0
  %t4088 = and i64 %t4086, %t4087
  store i64 %t4088, ptr %t4080
  %t4089 = load i64, ptr %t4080
  %t4090 = add i64 50, 0
  %t4091 = mul i64 %t4089, %t4090
  %t4092 = add i64 28, 0
  %t4093 = mul i64 %t4091, %t4092
  %t4094 = add i64 65535, 0
  %t4095 = and i64 %t4093, %t4094
  store i64 %t4095, ptr %t4080
  %t4096 = load i64, ptr %t4080
  %t4097 = add i64 50, 0
  %t4098 = sub i64 %t4096, %t4097
  %t4099 = add i64 79, 0
  %t4100 = sub i64 %t4098, %t4099
  %t4101 = add i64 65535, 0
  %t4102 = and i64 %t4100, %t4101
  store i64 %t4102, ptr %t4080
  %t4103 = load i64, ptr %t4080
  %t4104 = add i64 29, 0
  %t4105 = mul i64 %t4103, %t4104
  %t4106 = add i64 28, 0
  %t4107 = mul i64 %t4105, %t4106
  %t4108 = add i64 65535, 0
  %t4109 = and i64 %t4107, %t4108
  store i64 %t4109, ptr %t4080
  %t4110 = load i64, ptr %t4080
  %t4111 = add i64 43, 0
  %t4112 = sub i64 %t4110, %t4111
  %t4113 = add i64 83, 0
  %t4114 = sub i64 %t4112, %t4113
  %t4115 = add i64 65535, 0
  %t4116 = and i64 %t4114, %t4115
  store i64 %t4116, ptr %t4080
  %t4117 = load i64, ptr %t4080
  %t4118 = call %NxVal @nx_int(i64 %t4117)
  ret %NxVal %t4118
}
define %NxVal @nx__m_3____main____Cell__m55(%NxVal* %args, i64 %nargs) {
entry:
  %t4119 = alloca %NxVal
  %t4123 = alloca i64
  %t4138 = alloca i64
  store %NxVal zeroinitializer, ptr %t4119
  %t4120 = getelementptr %NxVal, ptr %args, i64 0
  %t4121 = load %NxVal, ptr %t4120
  %t4122 = call %NxVal @nx_clone(%NxVal %t4121)
  store %NxVal %t4122, ptr %t4119
  %t4124 = getelementptr %NxVal, ptr %args, i64 1
  %t4125 = load %NxVal, ptr %t4124
  %t4126 = extractvalue %NxVal %t4125, 1
  store i64 %t4126, ptr %t4123
  %t4127 = load %NxVal, ptr %t4119
  %t4128 = call %NxVal @nx_rec_get(%NxVal %t4127, i64 0)
  %t4129 = load %NxVal, ptr %t4119
  %t4130 = call %NxVal @nx_rec_get(%NxVal %t4129, i64 1)
  %t4131 = call %NxVal @nx_add(%NxVal %t4128, %NxVal %t4130)
  %t4132 = load i64, ptr %t4123
  %t4133 = call %NxVal @nx_int(i64 %t4132)
  %t4134 = call %NxVal @nx_add(%NxVal %t4131, %NxVal %t4133)
  %t4135 = add i64 65535, 0
  %t4136 = call %NxVal @nx_int(i64 %t4135)
  %t4137 = call %NxVal @nx_bitand(%NxVal %t4134, %NxVal %t4136)
  %t4139 = extractvalue %NxVal %t4137, 1
  store i64 %t4139, ptr %t4138
  %t4140 = load i64, ptr %t4138
  %t4141 = add i64 29, 0
  %t4142 = xor i64 %t4140, %t4141
  %t4143 = add i64 12, 0
  %t4144 = xor i64 %t4142, %t4143
  %t4145 = add i64 65535, 0
  %t4146 = and i64 %t4144, %t4145
  store i64 %t4146, ptr %t4138
  %t4147 = load i64, ptr %t4138
  %t4148 = add i64 51, 0
  %t4149 = or i64 %t4147, %t4148
  %t4150 = add i64 32, 0
  %t4151 = or i64 %t4149, %t4150
  %t4152 = add i64 65535, 0
  %t4153 = and i64 %t4151, %t4152
  store i64 %t4153, ptr %t4138
  %t4154 = load i64, ptr %t4138
  %t4155 = add i64 84, 0
  %t4156 = xor i64 %t4154, %t4155
  %t4157 = add i64 4, 0
  %t4158 = xor i64 %t4156, %t4157
  %t4159 = add i64 65535, 0
  %t4160 = and i64 %t4158, %t4159
  store i64 %t4160, ptr %t4138
  %t4161 = load i64, ptr %t4138
  %t4162 = add i64 17, 0
  %t4163 = xor i64 %t4161, %t4162
  %t4164 = add i64 41, 0
  %t4165 = xor i64 %t4163, %t4164
  %t4166 = add i64 65535, 0
  %t4167 = and i64 %t4165, %t4166
  store i64 %t4167, ptr %t4138
  %t4168 = load i64, ptr %t4138
  %t4169 = add i64 64, 0
  %t4170 = or i64 %t4168, %t4169
  %t4171 = add i64 66, 0
  %t4172 = or i64 %t4170, %t4171
  %t4173 = add i64 65535, 0
  %t4174 = and i64 %t4172, %t4173
  store i64 %t4174, ptr %t4138
  %t4175 = load i64, ptr %t4138
  %t4176 = call %NxVal @nx_int(i64 %t4175)
  ret %NxVal %t4176
}
define %NxVal @nx__m_3____main____Cell__m56(%NxVal* %args, i64 %nargs) {
entry:
  %t4177 = alloca %NxVal
  %t4181 = alloca i64
  %t4196 = alloca i64
  store %NxVal zeroinitializer, ptr %t4177
  %t4178 = getelementptr %NxVal, ptr %args, i64 0
  %t4179 = load %NxVal, ptr %t4178
  %t4180 = call %NxVal @nx_clone(%NxVal %t4179)
  store %NxVal %t4180, ptr %t4177
  %t4182 = getelementptr %NxVal, ptr %args, i64 1
  %t4183 = load %NxVal, ptr %t4182
  %t4184 = extractvalue %NxVal %t4183, 1
  store i64 %t4184, ptr %t4181
  %t4185 = load %NxVal, ptr %t4177
  %t4186 = call %NxVal @nx_rec_get(%NxVal %t4185, i64 0)
  %t4187 = load %NxVal, ptr %t4177
  %t4188 = call %NxVal @nx_rec_get(%NxVal %t4187, i64 1)
  %t4189 = call %NxVal @nx_add(%NxVal %t4186, %NxVal %t4188)
  %t4190 = load i64, ptr %t4181
  %t4191 = call %NxVal @nx_int(i64 %t4190)
  %t4192 = call %NxVal @nx_add(%NxVal %t4189, %NxVal %t4191)
  %t4193 = add i64 65535, 0
  %t4194 = call %NxVal @nx_int(i64 %t4193)
  %t4195 = call %NxVal @nx_bitand(%NxVal %t4192, %NxVal %t4194)
  %t4197 = extractvalue %NxVal %t4195, 1
  store i64 %t4197, ptr %t4196
  %t4198 = load i64, ptr %t4196
  %t4199 = add i64 53, 0
  %t4200 = or i64 %t4198, %t4199
  %t4201 = add i64 33, 0
  %t4202 = or i64 %t4200, %t4201
  %t4203 = add i64 65535, 0
  %t4204 = and i64 %t4202, %t4203
  store i64 %t4204, ptr %t4196
  %t4205 = load i64, ptr %t4196
  %t4206 = add i64 89, 0
  %t4207 = call i64 @nx_mod_i64(i64 %t4205, i64 %t4206)
  %t4208 = add i64 70, 0
  %t4209 = call i64 @nx_mod_i64(i64 %t4207, i64 %t4208)
  %t4210 = add i64 65535, 0
  %t4211 = and i64 %t4209, %t4210
  store i64 %t4211, ptr %t4196
  %t4212 = load i64, ptr %t4196
  %t4213 = add i64 44, 0
  %t4214 = call i64 @nx_mod_i64(i64 %t4212, i64 %t4213)
  %t4215 = add i64 1, 0
  %t4216 = call i64 @nx_mod_i64(i64 %t4214, i64 %t4215)
  %t4217 = add i64 65535, 0
  %t4218 = and i64 %t4216, %t4217
  store i64 %t4218, ptr %t4196
  %t4219 = load i64, ptr %t4196
  %t4220 = add i64 17, 0
  %t4221 = xor i64 %t4219, %t4220
  %t4222 = add i64 35, 0
  %t4223 = xor i64 %t4221, %t4222
  %t4224 = add i64 65535, 0
  %t4225 = and i64 %t4223, %t4224
  store i64 %t4225, ptr %t4196
  %t4226 = load i64, ptr %t4196
  %t4227 = add i64 62, 0
  %t4228 = and i64 %t4226, %t4227
  %t4229 = add i64 77, 0
  %t4230 = and i64 %t4228, %t4229
  %t4231 = add i64 65535, 0
  %t4232 = and i64 %t4230, %t4231
  store i64 %t4232, ptr %t4196
  %t4233 = load i64, ptr %t4196
  %t4234 = call %NxVal @nx_int(i64 %t4233)
  ret %NxVal %t4234
}
define %NxVal @nx__m_3____main____Cell__m57(%NxVal* %args, i64 %nargs) {
entry:
  %t4235 = alloca %NxVal
  %t4239 = alloca i64
  %t4254 = alloca i64
  store %NxVal zeroinitializer, ptr %t4235
  %t4236 = getelementptr %NxVal, ptr %args, i64 0
  %t4237 = load %NxVal, ptr %t4236
  %t4238 = call %NxVal @nx_clone(%NxVal %t4237)
  store %NxVal %t4238, ptr %t4235
  %t4240 = getelementptr %NxVal, ptr %args, i64 1
  %t4241 = load %NxVal, ptr %t4240
  %t4242 = extractvalue %NxVal %t4241, 1
  store i64 %t4242, ptr %t4239
  %t4243 = load %NxVal, ptr %t4235
  %t4244 = call %NxVal @nx_rec_get(%NxVal %t4243, i64 0)
  %t4245 = load %NxVal, ptr %t4235
  %t4246 = call %NxVal @nx_rec_get(%NxVal %t4245, i64 1)
  %t4247 = call %NxVal @nx_add(%NxVal %t4244, %NxVal %t4246)
  %t4248 = load i64, ptr %t4239
  %t4249 = call %NxVal @nx_int(i64 %t4248)
  %t4250 = call %NxVal @nx_add(%NxVal %t4247, %NxVal %t4249)
  %t4251 = add i64 65535, 0
  %t4252 = call %NxVal @nx_int(i64 %t4251)
  %t4253 = call %NxVal @nx_bitand(%NxVal %t4250, %NxVal %t4252)
  %t4255 = extractvalue %NxVal %t4253, 1
  store i64 %t4255, ptr %t4254
  %t4256 = load i64, ptr %t4254
  %t4257 = add i64 11, 0
  %t4258 = xor i64 %t4256, %t4257
  %t4259 = add i64 76, 0
  %t4260 = xor i64 %t4258, %t4259
  %t4261 = add i64 65535, 0
  %t4262 = and i64 %t4260, %t4261
  store i64 %t4262, ptr %t4254
  %t4263 = load i64, ptr %t4254
  %t4264 = add i64 82, 0
  %t4265 = sub i64 %t4263, %t4264
  %t4266 = add i64 21, 0
  %t4267 = sub i64 %t4265, %t4266
  %t4268 = add i64 65535, 0
  %t4269 = and i64 %t4267, %t4268
  store i64 %t4269, ptr %t4254
  %t4270 = load i64, ptr %t4254
  %t4271 = add i64 88, 0
  %t4272 = sub i64 %t4270, %t4271
  %t4273 = add i64 17, 0
  %t4274 = sub i64 %t4272, %t4273
  %t4275 = add i64 65535, 0
  %t4276 = and i64 %t4274, %t4275
  store i64 %t4276, ptr %t4254
  %t4277 = load i64, ptr %t4254
  %t4278 = add i64 83, 0
  %t4279 = add i64 %t4277, %t4278
  %t4280 = add i64 18, 0
  %t4281 = add i64 %t4279, %t4280
  %t4282 = add i64 65535, 0
  %t4283 = and i64 %t4281, %t4282
  store i64 %t4283, ptr %t4254
  %t4284 = load i64, ptr %t4254
  %t4285 = add i64 35, 0
  %t4286 = call i64 @nx_mod_i64(i64 %t4284, i64 %t4285)
  %t4287 = add i64 3, 0
  %t4288 = call i64 @nx_mod_i64(i64 %t4286, i64 %t4287)
  %t4289 = add i64 65535, 0
  %t4290 = and i64 %t4288, %t4289
  store i64 %t4290, ptr %t4254
  %t4291 = load i64, ptr %t4254
  %t4292 = call %NxVal @nx_int(i64 %t4291)
  ret %NxVal %t4292
}
define %NxVal @nx__m_3____main____Cell__m58(%NxVal* %args, i64 %nargs) {
entry:
  %t4293 = alloca %NxVal
  %t4297 = alloca i64
  %t4312 = alloca i64
  store %NxVal zeroinitializer, ptr %t4293
  %t4294 = getelementptr %NxVal, ptr %args, i64 0
  %t4295 = load %NxVal, ptr %t4294
  %t4296 = call %NxVal @nx_clone(%NxVal %t4295)
  store %NxVal %t4296, ptr %t4293
  %t4298 = getelementptr %NxVal, ptr %args, i64 1
  %t4299 = load %NxVal, ptr %t4298
  %t4300 = extractvalue %NxVal %t4299, 1
  store i64 %t4300, ptr %t4297
  %t4301 = load %NxVal, ptr %t4293
  %t4302 = call %NxVal @nx_rec_get(%NxVal %t4301, i64 0)
  %t4303 = load %NxVal, ptr %t4293
  %t4304 = call %NxVal @nx_rec_get(%NxVal %t4303, i64 1)
  %t4305 = call %NxVal @nx_add(%NxVal %t4302, %NxVal %t4304)
  %t4306 = load i64, ptr %t4297
  %t4307 = call %NxVal @nx_int(i64 %t4306)
  %t4308 = call %NxVal @nx_add(%NxVal %t4305, %NxVal %t4307)
  %t4309 = add i64 65535, 0
  %t4310 = call %NxVal @nx_int(i64 %t4309)
  %t4311 = call %NxVal @nx_bitand(%NxVal %t4308, %NxVal %t4310)
  %t4313 = extractvalue %NxVal %t4311, 1
  store i64 %t4313, ptr %t4312
  %t4314 = load i64, ptr %t4312
  %t4315 = add i64 27, 0
  %t4316 = xor i64 %t4314, %t4315
  %t4317 = add i64 17, 0
  %t4318 = xor i64 %t4316, %t4317
  %t4319 = add i64 65535, 0
  %t4320 = and i64 %t4318, %t4319
  store i64 %t4320, ptr %t4312
  %t4321 = load i64, ptr %t4312
  %t4322 = add i64 6, 0
  %t4323 = add i64 %t4321, %t4322
  %t4324 = add i64 34, 0
  %t4325 = add i64 %t4323, %t4324
  %t4326 = add i64 65535, 0
  %t4327 = and i64 %t4325, %t4326
  store i64 %t4327, ptr %t4312
  %t4328 = load i64, ptr %t4312
  %t4329 = add i64 95, 0
  %t4330 = mul i64 %t4328, %t4329
  %t4331 = add i64 4, 0
  %t4332 = mul i64 %t4330, %t4331
  %t4333 = add i64 65535, 0
  %t4334 = and i64 %t4332, %t4333
  store i64 %t4334, ptr %t4312
  %t4335 = load i64, ptr %t4312
  %t4336 = add i64 31, 0
  %t4337 = xor i64 %t4335, %t4336
  %t4338 = add i64 69, 0
  %t4339 = xor i64 %t4337, %t4338
  %t4340 = add i64 65535, 0
  %t4341 = and i64 %t4339, %t4340
  store i64 %t4341, ptr %t4312
  %t4342 = load i64, ptr %t4312
  %t4343 = add i64 55, 0
  %t4344 = and i64 %t4342, %t4343
  %t4345 = add i64 40, 0
  %t4346 = and i64 %t4344, %t4345
  %t4347 = add i64 65535, 0
  %t4348 = and i64 %t4346, %t4347
  store i64 %t4348, ptr %t4312
  %t4349 = load i64, ptr %t4312
  %t4350 = call %NxVal @nx_int(i64 %t4349)
  ret %NxVal %t4350
}
define %NxVal @nx__m_3____main____Cell__m59(%NxVal* %args, i64 %nargs) {
entry:
  %t4351 = alloca %NxVal
  %t4355 = alloca i64
  %t4370 = alloca i64
  store %NxVal zeroinitializer, ptr %t4351
  %t4352 = getelementptr %NxVal, ptr %args, i64 0
  %t4353 = load %NxVal, ptr %t4352
  %t4354 = call %NxVal @nx_clone(%NxVal %t4353)
  store %NxVal %t4354, ptr %t4351
  %t4356 = getelementptr %NxVal, ptr %args, i64 1
  %t4357 = load %NxVal, ptr %t4356
  %t4358 = extractvalue %NxVal %t4357, 1
  store i64 %t4358, ptr %t4355
  %t4359 = load %NxVal, ptr %t4351
  %t4360 = call %NxVal @nx_rec_get(%NxVal %t4359, i64 0)
  %t4361 = load %NxVal, ptr %t4351
  %t4362 = call %NxVal @nx_rec_get(%NxVal %t4361, i64 1)
  %t4363 = call %NxVal @nx_add(%NxVal %t4360, %NxVal %t4362)
  %t4364 = load i64, ptr %t4355
  %t4365 = call %NxVal @nx_int(i64 %t4364)
  %t4366 = call %NxVal @nx_add(%NxVal %t4363, %NxVal %t4365)
  %t4367 = add i64 65535, 0
  %t4368 = call %NxVal @nx_int(i64 %t4367)
  %t4369 = call %NxVal @nx_bitand(%NxVal %t4366, %NxVal %t4368)
  %t4371 = extractvalue %NxVal %t4369, 1
  store i64 %t4371, ptr %t4370
  %t4372 = load i64, ptr %t4370
  %t4373 = add i64 46, 0
  %t4374 = call i64 @nx_mod_i64(i64 %t4372, i64 %t4373)
  %t4375 = add i64 78, 0
  %t4376 = call i64 @nx_mod_i64(i64 %t4374, i64 %t4375)
  %t4377 = add i64 65535, 0
  %t4378 = and i64 %t4376, %t4377
  store i64 %t4378, ptr %t4370
  %t4379 = load i64, ptr %t4370
  %t4380 = add i64 69, 0
  %t4381 = mul i64 %t4379, %t4380
  %t4382 = add i64 75, 0
  %t4383 = mul i64 %t4381, %t4382
  %t4384 = add i64 65535, 0
  %t4385 = and i64 %t4383, %t4384
  store i64 %t4385, ptr %t4370
  %t4386 = load i64, ptr %t4370
  %t4387 = add i64 96, 0
  %t4388 = mul i64 %t4386, %t4387
  %t4389 = add i64 87, 0
  %t4390 = mul i64 %t4388, %t4389
  %t4391 = add i64 65535, 0
  %t4392 = and i64 %t4390, %t4391
  store i64 %t4392, ptr %t4370
  %t4393 = load i64, ptr %t4370
  %t4394 = add i64 90, 0
  %t4395 = sub i64 %t4393, %t4394
  %t4396 = add i64 85, 0
  %t4397 = sub i64 %t4395, %t4396
  %t4398 = add i64 65535, 0
  %t4399 = and i64 %t4397, %t4398
  store i64 %t4399, ptr %t4370
  %t4400 = load i64, ptr %t4370
  %t4401 = add i64 94, 0
  %t4402 = and i64 %t4400, %t4401
  %t4403 = add i64 83, 0
  %t4404 = and i64 %t4402, %t4403
  %t4405 = add i64 65535, 0
  %t4406 = and i64 %t4404, %t4405
  store i64 %t4406, ptr %t4370
  %t4407 = load i64, ptr %t4370
  %t4408 = call %NxVal @nx_int(i64 %t4407)
  ret %NxVal %t4408
}
define %NxVal @nx__m_2____main____Cell__m6(%NxVal* %args, i64 %nargs) {
entry:
  %t4409 = alloca %NxVal
  %t4413 = alloca i64
  %t4428 = alloca i64
  store %NxVal zeroinitializer, ptr %t4409
  %t4410 = getelementptr %NxVal, ptr %args, i64 0
  %t4411 = load %NxVal, ptr %t4410
  %t4412 = call %NxVal @nx_clone(%NxVal %t4411)
  store %NxVal %t4412, ptr %t4409
  %t4414 = getelementptr %NxVal, ptr %args, i64 1
  %t4415 = load %NxVal, ptr %t4414
  %t4416 = extractvalue %NxVal %t4415, 1
  store i64 %t4416, ptr %t4413
  %t4417 = load %NxVal, ptr %t4409
  %t4418 = call %NxVal @nx_rec_get(%NxVal %t4417, i64 0)
  %t4419 = load %NxVal, ptr %t4409
  %t4420 = call %NxVal @nx_rec_get(%NxVal %t4419, i64 1)
  %t4421 = call %NxVal @nx_add(%NxVal %t4418, %NxVal %t4420)
  %t4422 = load i64, ptr %t4413
  %t4423 = call %NxVal @nx_int(i64 %t4422)
  %t4424 = call %NxVal @nx_add(%NxVal %t4421, %NxVal %t4423)
  %t4425 = add i64 65535, 0
  %t4426 = call %NxVal @nx_int(i64 %t4425)
  %t4427 = call %NxVal @nx_bitand(%NxVal %t4424, %NxVal %t4426)
  %t4429 = extractvalue %NxVal %t4427, 1
  store i64 %t4429, ptr %t4428
  %t4430 = load i64, ptr %t4428
  %t4431 = add i64 56, 0
  %t4432 = xor i64 %t4430, %t4431
  %t4433 = add i64 59, 0
  %t4434 = xor i64 %t4432, %t4433
  %t4435 = add i64 65535, 0
  %t4436 = and i64 %t4434, %t4435
  store i64 %t4436, ptr %t4428
  %t4437 = load i64, ptr %t4428
  %t4438 = add i64 45, 0
  %t4439 = sub i64 %t4437, %t4438
  %t4440 = add i64 4, 0
  %t4441 = sub i64 %t4439, %t4440
  %t4442 = add i64 65535, 0
  %t4443 = and i64 %t4441, %t4442
  store i64 %t4443, ptr %t4428
  %t4444 = load i64, ptr %t4428
  %t4445 = add i64 8, 0
  %t4446 = sub i64 %t4444, %t4445
  %t4447 = add i64 83, 0
  %t4448 = sub i64 %t4446, %t4447
  %t4449 = add i64 65535, 0
  %t4450 = and i64 %t4448, %t4449
  store i64 %t4450, ptr %t4428
  %t4451 = load i64, ptr %t4428
  %t4452 = add i64 61, 0
  %t4453 = call i64 @nx_mod_i64(i64 %t4451, i64 %t4452)
  %t4454 = add i64 2, 0
  %t4455 = call i64 @nx_mod_i64(i64 %t4453, i64 %t4454)
  %t4456 = add i64 65535, 0
  %t4457 = and i64 %t4455, %t4456
  store i64 %t4457, ptr %t4428
  %t4458 = load i64, ptr %t4428
  %t4459 = add i64 34, 0
  %t4460 = mul i64 %t4458, %t4459
  %t4461 = add i64 60, 0
  %t4462 = mul i64 %t4460, %t4461
  %t4463 = add i64 65535, 0
  %t4464 = and i64 %t4462, %t4463
  store i64 %t4464, ptr %t4428
  %t4465 = load i64, ptr %t4428
  %t4466 = call %NxVal @nx_int(i64 %t4465)
  ret %NxVal %t4466
}
define %NxVal @nx__m_3____main____Cell__m60(%NxVal* %args, i64 %nargs) {
entry:
  %t4467 = alloca %NxVal
  %t4471 = alloca i64
  %t4486 = alloca i64
  store %NxVal zeroinitializer, ptr %t4467
  %t4468 = getelementptr %NxVal, ptr %args, i64 0
  %t4469 = load %NxVal, ptr %t4468
  %t4470 = call %NxVal @nx_clone(%NxVal %t4469)
  store %NxVal %t4470, ptr %t4467
  %t4472 = getelementptr %NxVal, ptr %args, i64 1
  %t4473 = load %NxVal, ptr %t4472
  %t4474 = extractvalue %NxVal %t4473, 1
  store i64 %t4474, ptr %t4471
  %t4475 = load %NxVal, ptr %t4467
  %t4476 = call %NxVal @nx_rec_get(%NxVal %t4475, i64 0)
  %t4477 = load %NxVal, ptr %t4467
  %t4478 = call %NxVal @nx_rec_get(%NxVal %t4477, i64 1)
  %t4479 = call %NxVal @nx_add(%NxVal %t4476, %NxVal %t4478)
  %t4480 = load i64, ptr %t4471
  %t4481 = call %NxVal @nx_int(i64 %t4480)
  %t4482 = call %NxVal @nx_add(%NxVal %t4479, %NxVal %t4481)
  %t4483 = add i64 65535, 0
  %t4484 = call %NxVal @nx_int(i64 %t4483)
  %t4485 = call %NxVal @nx_bitand(%NxVal %t4482, %NxVal %t4484)
  %t4487 = extractvalue %NxVal %t4485, 1
  store i64 %t4487, ptr %t4486
  %t4488 = load i64, ptr %t4486
  %t4489 = add i64 1, 0
  %t4490 = sub i64 %t4488, %t4489
  %t4491 = add i64 10, 0
  %t4492 = sub i64 %t4490, %t4491
  %t4493 = add i64 65535, 0
  %t4494 = and i64 %t4492, %t4493
  store i64 %t4494, ptr %t4486
  %t4495 = load i64, ptr %t4486
  %t4496 = add i64 97, 0
  %t4497 = mul i64 %t4495, %t4496
  %t4498 = add i64 42, 0
  %t4499 = mul i64 %t4497, %t4498
  %t4500 = add i64 65535, 0
  %t4501 = and i64 %t4499, %t4500
  store i64 %t4501, ptr %t4486
  %t4502 = load i64, ptr %t4486
  %t4503 = add i64 68, 0
  %t4504 = add i64 %t4502, %t4503
  %t4505 = add i64 86, 0
  %t4506 = add i64 %t4504, %t4505
  %t4507 = add i64 65535, 0
  %t4508 = and i64 %t4506, %t4507
  store i64 %t4508, ptr %t4486
  %t4509 = load i64, ptr %t4486
  %t4510 = add i64 17, 0
  %t4511 = call i64 @nx_mod_i64(i64 %t4509, i64 %t4510)
  %t4512 = add i64 54, 0
  %t4513 = call i64 @nx_mod_i64(i64 %t4511, i64 %t4512)
  %t4514 = add i64 65535, 0
  %t4515 = and i64 %t4513, %t4514
  store i64 %t4515, ptr %t4486
  %t4516 = load i64, ptr %t4486
  %t4517 = add i64 79, 0
  %t4518 = xor i64 %t4516, %t4517
  %t4519 = add i64 53, 0
  %t4520 = xor i64 %t4518, %t4519
  %t4521 = add i64 65535, 0
  %t4522 = and i64 %t4520, %t4521
  store i64 %t4522, ptr %t4486
  %t4523 = load i64, ptr %t4486
  %t4524 = call %NxVal @nx_int(i64 %t4523)
  ret %NxVal %t4524
}
define %NxVal @nx__m_3____main____Cell__m61(%NxVal* %args, i64 %nargs) {
entry:
  %t4525 = alloca %NxVal
  %t4529 = alloca i64
  %t4544 = alloca i64
  store %NxVal zeroinitializer, ptr %t4525
  %t4526 = getelementptr %NxVal, ptr %args, i64 0
  %t4527 = load %NxVal, ptr %t4526
  %t4528 = call %NxVal @nx_clone(%NxVal %t4527)
  store %NxVal %t4528, ptr %t4525
  %t4530 = getelementptr %NxVal, ptr %args, i64 1
  %t4531 = load %NxVal, ptr %t4530
  %t4532 = extractvalue %NxVal %t4531, 1
  store i64 %t4532, ptr %t4529
  %t4533 = load %NxVal, ptr %t4525
  %t4534 = call %NxVal @nx_rec_get(%NxVal %t4533, i64 0)
  %t4535 = load %NxVal, ptr %t4525
  %t4536 = call %NxVal @nx_rec_get(%NxVal %t4535, i64 1)
  %t4537 = call %NxVal @nx_add(%NxVal %t4534, %NxVal %t4536)
  %t4538 = load i64, ptr %t4529
  %t4539 = call %NxVal @nx_int(i64 %t4538)
  %t4540 = call %NxVal @nx_add(%NxVal %t4537, %NxVal %t4539)
  %t4541 = add i64 65535, 0
  %t4542 = call %NxVal @nx_int(i64 %t4541)
  %t4543 = call %NxVal @nx_bitand(%NxVal %t4540, %NxVal %t4542)
  %t4545 = extractvalue %NxVal %t4543, 1
  store i64 %t4545, ptr %t4544
  %t4546 = load i64, ptr %t4544
  %t4547 = add i64 78, 0
  %t4548 = and i64 %t4546, %t4547
  %t4549 = add i64 40, 0
  %t4550 = and i64 %t4548, %t4549
  %t4551 = add i64 65535, 0
  %t4552 = and i64 %t4550, %t4551
  store i64 %t4552, ptr %t4544
  %t4553 = load i64, ptr %t4544
  %t4554 = add i64 79, 0
  %t4555 = xor i64 %t4553, %t4554
  %t4556 = add i64 8, 0
  %t4557 = xor i64 %t4555, %t4556
  %t4558 = add i64 65535, 0
  %t4559 = and i64 %t4557, %t4558
  store i64 %t4559, ptr %t4544
  %t4560 = load i64, ptr %t4544
  %t4561 = add i64 87, 0
  %t4562 = call i64 @nx_mod_i64(i64 %t4560, i64 %t4561)
  %t4563 = add i64 67, 0
  %t4564 = call i64 @nx_mod_i64(i64 %t4562, i64 %t4563)
  %t4565 = add i64 65535, 0
  %t4566 = and i64 %t4564, %t4565
  store i64 %t4566, ptr %t4544
  %t4567 = load i64, ptr %t4544
  %t4568 = add i64 22, 0
  %t4569 = call i64 @nx_mod_i64(i64 %t4567, i64 %t4568)
  %t4570 = add i64 58, 0
  %t4571 = call i64 @nx_mod_i64(i64 %t4569, i64 %t4570)
  %t4572 = add i64 65535, 0
  %t4573 = and i64 %t4571, %t4572
  store i64 %t4573, ptr %t4544
  %t4574 = load i64, ptr %t4544
  %t4575 = add i64 58, 0
  %t4576 = add i64 %t4574, %t4575
  %t4577 = add i64 52, 0
  %t4578 = add i64 %t4576, %t4577
  %t4579 = add i64 65535, 0
  %t4580 = and i64 %t4578, %t4579
  store i64 %t4580, ptr %t4544
  %t4581 = load i64, ptr %t4544
  %t4582 = call %NxVal @nx_int(i64 %t4581)
  ret %NxVal %t4582
}
define %NxVal @nx__m_3____main____Cell__m62(%NxVal* %args, i64 %nargs) {
entry:
  %t4583 = alloca %NxVal
  %t4587 = alloca i64
  %t4602 = alloca i64
  store %NxVal zeroinitializer, ptr %t4583
  %t4584 = getelementptr %NxVal, ptr %args, i64 0
  %t4585 = load %NxVal, ptr %t4584
  %t4586 = call %NxVal @nx_clone(%NxVal %t4585)
  store %NxVal %t4586, ptr %t4583
  %t4588 = getelementptr %NxVal, ptr %args, i64 1
  %t4589 = load %NxVal, ptr %t4588
  %t4590 = extractvalue %NxVal %t4589, 1
  store i64 %t4590, ptr %t4587
  %t4591 = load %NxVal, ptr %t4583
  %t4592 = call %NxVal @nx_rec_get(%NxVal %t4591, i64 0)
  %t4593 = load %NxVal, ptr %t4583
  %t4594 = call %NxVal @nx_rec_get(%NxVal %t4593, i64 1)
  %t4595 = call %NxVal @nx_add(%NxVal %t4592, %NxVal %t4594)
  %t4596 = load i64, ptr %t4587
  %t4597 = call %NxVal @nx_int(i64 %t4596)
  %t4598 = call %NxVal @nx_add(%NxVal %t4595, %NxVal %t4597)
  %t4599 = add i64 65535, 0
  %t4600 = call %NxVal @nx_int(i64 %t4599)
  %t4601 = call %NxVal @nx_bitand(%NxVal %t4598, %NxVal %t4600)
  %t4603 = extractvalue %NxVal %t4601, 1
  store i64 %t4603, ptr %t4602
  %t4604 = load i64, ptr %t4602
  %t4605 = add i64 26, 0
  %t4606 = or i64 %t4604, %t4605
  %t4607 = add i64 52, 0
  %t4608 = or i64 %t4606, %t4607
  %t4609 = add i64 65535, 0
  %t4610 = and i64 %t4608, %t4609
  store i64 %t4610, ptr %t4602
  %t4611 = load i64, ptr %t4602
  %t4612 = add i64 72, 0
  %t4613 = add i64 %t4611, %t4612
  %t4614 = add i64 24, 0
  %t4615 = add i64 %t4613, %t4614
  %t4616 = add i64 65535, 0
  %t4617 = and i64 %t4615, %t4616
  store i64 %t4617, ptr %t4602
  %t4618 = load i64, ptr %t4602
  %t4619 = add i64 83, 0
  %t4620 = sub i64 %t4618, %t4619
  %t4621 = add i64 66, 0
  %t4622 = sub i64 %t4620, %t4621
  %t4623 = add i64 65535, 0
  %t4624 = and i64 %t4622, %t4623
  store i64 %t4624, ptr %t4602
  %t4625 = load i64, ptr %t4602
  %t4626 = add i64 78, 0
  %t4627 = and i64 %t4625, %t4626
  %t4628 = add i64 33, 0
  %t4629 = and i64 %t4627, %t4628
  %t4630 = add i64 65535, 0
  %t4631 = and i64 %t4629, %t4630
  store i64 %t4631, ptr %t4602
  %t4632 = load i64, ptr %t4602
  %t4633 = add i64 17, 0
  %t4634 = call i64 @nx_mod_i64(i64 %t4632, i64 %t4633)
  %t4635 = add i64 43, 0
  %t4636 = call i64 @nx_mod_i64(i64 %t4634, i64 %t4635)
  %t4637 = add i64 65535, 0
  %t4638 = and i64 %t4636, %t4637
  store i64 %t4638, ptr %t4602
  %t4639 = load i64, ptr %t4602
  %t4640 = call %NxVal @nx_int(i64 %t4639)
  ret %NxVal %t4640
}
define %NxVal @nx__m_3____main____Cell__m63(%NxVal* %args, i64 %nargs) {
entry:
  %t4641 = alloca %NxVal
  %t4645 = alloca i64
  %t4660 = alloca i64
  store %NxVal zeroinitializer, ptr %t4641
  %t4642 = getelementptr %NxVal, ptr %args, i64 0
  %t4643 = load %NxVal, ptr %t4642
  %t4644 = call %NxVal @nx_clone(%NxVal %t4643)
  store %NxVal %t4644, ptr %t4641
  %t4646 = getelementptr %NxVal, ptr %args, i64 1
  %t4647 = load %NxVal, ptr %t4646
  %t4648 = extractvalue %NxVal %t4647, 1
  store i64 %t4648, ptr %t4645
  %t4649 = load %NxVal, ptr %t4641
  %t4650 = call %NxVal @nx_rec_get(%NxVal %t4649, i64 0)
  %t4651 = load %NxVal, ptr %t4641
  %t4652 = call %NxVal @nx_rec_get(%NxVal %t4651, i64 1)
  %t4653 = call %NxVal @nx_add(%NxVal %t4650, %NxVal %t4652)
  %t4654 = load i64, ptr %t4645
  %t4655 = call %NxVal @nx_int(i64 %t4654)
  %t4656 = call %NxVal @nx_add(%NxVal %t4653, %NxVal %t4655)
  %t4657 = add i64 65535, 0
  %t4658 = call %NxVal @nx_int(i64 %t4657)
  %t4659 = call %NxVal @nx_bitand(%NxVal %t4656, %NxVal %t4658)
  %t4661 = extractvalue %NxVal %t4659, 1
  store i64 %t4661, ptr %t4660
  %t4662 = load i64, ptr %t4660
  %t4663 = add i64 62, 0
  %t4664 = mul i64 %t4662, %t4663
  %t4665 = add i64 76, 0
  %t4666 = mul i64 %t4664, %t4665
  %t4667 = add i64 65535, 0
  %t4668 = and i64 %t4666, %t4667
  store i64 %t4668, ptr %t4660
  %t4669 = load i64, ptr %t4660
  %t4670 = add i64 21, 0
  %t4671 = mul i64 %t4669, %t4670
  %t4672 = add i64 69, 0
  %t4673 = mul i64 %t4671, %t4672
  %t4674 = add i64 65535, 0
  %t4675 = and i64 %t4673, %t4674
  store i64 %t4675, ptr %t4660
  %t4676 = load i64, ptr %t4660
  %t4677 = add i64 53, 0
  %t4678 = sub i64 %t4676, %t4677
  %t4679 = add i64 43, 0
  %t4680 = sub i64 %t4678, %t4679
  %t4681 = add i64 65535, 0
  %t4682 = and i64 %t4680, %t4681
  store i64 %t4682, ptr %t4660
  %t4683 = load i64, ptr %t4660
  %t4684 = add i64 3, 0
  %t4685 = xor i64 %t4683, %t4684
  %t4686 = add i64 87, 0
  %t4687 = xor i64 %t4685, %t4686
  %t4688 = add i64 65535, 0
  %t4689 = and i64 %t4687, %t4688
  store i64 %t4689, ptr %t4660
  %t4690 = load i64, ptr %t4660
  %t4691 = add i64 17, 0
  %t4692 = or i64 %t4690, %t4691
  %t4693 = add i64 87, 0
  %t4694 = or i64 %t4692, %t4693
  %t4695 = add i64 65535, 0
  %t4696 = and i64 %t4694, %t4695
  store i64 %t4696, ptr %t4660
  %t4697 = load i64, ptr %t4660
  %t4698 = call %NxVal @nx_int(i64 %t4697)
  ret %NxVal %t4698
}
define %NxVal @nx__m_3____main____Cell__m64(%NxVal* %args, i64 %nargs) {
entry:
  %t4699 = alloca %NxVal
  %t4703 = alloca i64
  %t4718 = alloca i64
  store %NxVal zeroinitializer, ptr %t4699
  %t4700 = getelementptr %NxVal, ptr %args, i64 0
  %t4701 = load %NxVal, ptr %t4700
  %t4702 = call %NxVal @nx_clone(%NxVal %t4701)
  store %NxVal %t4702, ptr %t4699
  %t4704 = getelementptr %NxVal, ptr %args, i64 1
  %t4705 = load %NxVal, ptr %t4704
  %t4706 = extractvalue %NxVal %t4705, 1
  store i64 %t4706, ptr %t4703
  %t4707 = load %NxVal, ptr %t4699
  %t4708 = call %NxVal @nx_rec_get(%NxVal %t4707, i64 0)
  %t4709 = load %NxVal, ptr %t4699
  %t4710 = call %NxVal @nx_rec_get(%NxVal %t4709, i64 1)
  %t4711 = call %NxVal @nx_add(%NxVal %t4708, %NxVal %t4710)
  %t4712 = load i64, ptr %t4703
  %t4713 = call %NxVal @nx_int(i64 %t4712)
  %t4714 = call %NxVal @nx_add(%NxVal %t4711, %NxVal %t4713)
  %t4715 = add i64 65535, 0
  %t4716 = call %NxVal @nx_int(i64 %t4715)
  %t4717 = call %NxVal @nx_bitand(%NxVal %t4714, %NxVal %t4716)
  %t4719 = extractvalue %NxVal %t4717, 1
  store i64 %t4719, ptr %t4718
  %t4720 = load i64, ptr %t4718
  %t4721 = add i64 41, 0
  %t4722 = add i64 %t4720, %t4721
  %t4723 = add i64 62, 0
  %t4724 = add i64 %t4722, %t4723
  %t4725 = add i64 65535, 0
  %t4726 = and i64 %t4724, %t4725
  store i64 %t4726, ptr %t4718
  %t4727 = load i64, ptr %t4718
  %t4728 = add i64 20, 0
  %t4729 = call i64 @nx_mod_i64(i64 %t4727, i64 %t4728)
  %t4730 = add i64 43, 0
  %t4731 = call i64 @nx_mod_i64(i64 %t4729, i64 %t4730)
  %t4732 = add i64 65535, 0
  %t4733 = and i64 %t4731, %t4732
  store i64 %t4733, ptr %t4718
  %t4734 = load i64, ptr %t4718
  %t4735 = add i64 82, 0
  %t4736 = sub i64 %t4734, %t4735
  %t4737 = add i64 24, 0
  %t4738 = sub i64 %t4736, %t4737
  %t4739 = add i64 65535, 0
  %t4740 = and i64 %t4738, %t4739
  store i64 %t4740, ptr %t4718
  %t4741 = load i64, ptr %t4718
  %t4742 = add i64 9, 0
  %t4743 = and i64 %t4741, %t4742
  %t4744 = add i64 5, 0
  %t4745 = and i64 %t4743, %t4744
  %t4746 = add i64 65535, 0
  %t4747 = and i64 %t4745, %t4746
  store i64 %t4747, ptr %t4718
  %t4748 = load i64, ptr %t4718
  %t4749 = add i64 28, 0
  %t4750 = xor i64 %t4748, %t4749
  %t4751 = add i64 42, 0
  %t4752 = xor i64 %t4750, %t4751
  %t4753 = add i64 65535, 0
  %t4754 = and i64 %t4752, %t4753
  store i64 %t4754, ptr %t4718
  %t4755 = load i64, ptr %t4718
  %t4756 = call %NxVal @nx_int(i64 %t4755)
  ret %NxVal %t4756
}
define %NxVal @nx__m_3____main____Cell__m65(%NxVal* %args, i64 %nargs) {
entry:
  %t4757 = alloca %NxVal
  %t4761 = alloca i64
  %t4776 = alloca i64
  store %NxVal zeroinitializer, ptr %t4757
  %t4758 = getelementptr %NxVal, ptr %args, i64 0
  %t4759 = load %NxVal, ptr %t4758
  %t4760 = call %NxVal @nx_clone(%NxVal %t4759)
  store %NxVal %t4760, ptr %t4757
  %t4762 = getelementptr %NxVal, ptr %args, i64 1
  %t4763 = load %NxVal, ptr %t4762
  %t4764 = extractvalue %NxVal %t4763, 1
  store i64 %t4764, ptr %t4761
  %t4765 = load %NxVal, ptr %t4757
  %t4766 = call %NxVal @nx_rec_get(%NxVal %t4765, i64 0)
  %t4767 = load %NxVal, ptr %t4757
  %t4768 = call %NxVal @nx_rec_get(%NxVal %t4767, i64 1)
  %t4769 = call %NxVal @nx_add(%NxVal %t4766, %NxVal %t4768)
  %t4770 = load i64, ptr %t4761
  %t4771 = call %NxVal @nx_int(i64 %t4770)
  %t4772 = call %NxVal @nx_add(%NxVal %t4769, %NxVal %t4771)
  %t4773 = add i64 65535, 0
  %t4774 = call %NxVal @nx_int(i64 %t4773)
  %t4775 = call %NxVal @nx_bitand(%NxVal %t4772, %NxVal %t4774)
  %t4777 = extractvalue %NxVal %t4775, 1
  store i64 %t4777, ptr %t4776
  %t4778 = load i64, ptr %t4776
  %t4779 = add i64 69, 0
  %t4780 = or i64 %t4778, %t4779
  %t4781 = add i64 41, 0
  %t4782 = or i64 %t4780, %t4781
  %t4783 = add i64 65535, 0
  %t4784 = and i64 %t4782, %t4783
  store i64 %t4784, ptr %t4776
  %t4785 = load i64, ptr %t4776
  %t4786 = add i64 77, 0
  %t4787 = and i64 %t4785, %t4786
  %t4788 = add i64 87, 0
  %t4789 = and i64 %t4787, %t4788
  %t4790 = add i64 65535, 0
  %t4791 = and i64 %t4789, %t4790
  store i64 %t4791, ptr %t4776
  %t4792 = load i64, ptr %t4776
  %t4793 = add i64 5, 0
  %t4794 = mul i64 %t4792, %t4793
  %t4795 = add i64 2, 0
  %t4796 = mul i64 %t4794, %t4795
  %t4797 = add i64 65535, 0
  %t4798 = and i64 %t4796, %t4797
  store i64 %t4798, ptr %t4776
  %t4799 = load i64, ptr %t4776
  %t4800 = add i64 45, 0
  %t4801 = and i64 %t4799, %t4800
  %t4802 = add i64 54, 0
  %t4803 = and i64 %t4801, %t4802
  %t4804 = add i64 65535, 0
  %t4805 = and i64 %t4803, %t4804
  store i64 %t4805, ptr %t4776
  %t4806 = load i64, ptr %t4776
  %t4807 = add i64 57, 0
  %t4808 = or i64 %t4806, %t4807
  %t4809 = add i64 74, 0
  %t4810 = or i64 %t4808, %t4809
  %t4811 = add i64 65535, 0
  %t4812 = and i64 %t4810, %t4811
  store i64 %t4812, ptr %t4776
  %t4813 = load i64, ptr %t4776
  %t4814 = call %NxVal @nx_int(i64 %t4813)
  ret %NxVal %t4814
}
define %NxVal @nx__m_3____main____Cell__m66(%NxVal* %args, i64 %nargs) {
entry:
  %t4815 = alloca %NxVal
  %t4819 = alloca i64
  %t4834 = alloca i64
  store %NxVal zeroinitializer, ptr %t4815
  %t4816 = getelementptr %NxVal, ptr %args, i64 0
  %t4817 = load %NxVal, ptr %t4816
  %t4818 = call %NxVal @nx_clone(%NxVal %t4817)
  store %NxVal %t4818, ptr %t4815
  %t4820 = getelementptr %NxVal, ptr %args, i64 1
  %t4821 = load %NxVal, ptr %t4820
  %t4822 = extractvalue %NxVal %t4821, 1
  store i64 %t4822, ptr %t4819
  %t4823 = load %NxVal, ptr %t4815
  %t4824 = call %NxVal @nx_rec_get(%NxVal %t4823, i64 0)
  %t4825 = load %NxVal, ptr %t4815
  %t4826 = call %NxVal @nx_rec_get(%NxVal %t4825, i64 1)
  %t4827 = call %NxVal @nx_add(%NxVal %t4824, %NxVal %t4826)
  %t4828 = load i64, ptr %t4819
  %t4829 = call %NxVal @nx_int(i64 %t4828)
  %t4830 = call %NxVal @nx_add(%NxVal %t4827, %NxVal %t4829)
  %t4831 = add i64 65535, 0
  %t4832 = call %NxVal @nx_int(i64 %t4831)
  %t4833 = call %NxVal @nx_bitand(%NxVal %t4830, %NxVal %t4832)
  %t4835 = extractvalue %NxVal %t4833, 1
  store i64 %t4835, ptr %t4834
  %t4836 = load i64, ptr %t4834
  %t4837 = add i64 5, 0
  %t4838 = and i64 %t4836, %t4837
  %t4839 = add i64 17, 0
  %t4840 = and i64 %t4838, %t4839
  %t4841 = add i64 65535, 0
  %t4842 = and i64 %t4840, %t4841
  store i64 %t4842, ptr %t4834
  %t4843 = load i64, ptr %t4834
  %t4844 = add i64 46, 0
  %t4845 = and i64 %t4843, %t4844
  %t4846 = add i64 65, 0
  %t4847 = and i64 %t4845, %t4846
  %t4848 = add i64 65535, 0
  %t4849 = and i64 %t4847, %t4848
  store i64 %t4849, ptr %t4834
  %t4850 = load i64, ptr %t4834
  %t4851 = add i64 81, 0
  %t4852 = call i64 @nx_mod_i64(i64 %t4850, i64 %t4851)
  %t4853 = add i64 31, 0
  %t4854 = call i64 @nx_mod_i64(i64 %t4852, i64 %t4853)
  %t4855 = add i64 65535, 0
  %t4856 = and i64 %t4854, %t4855
  store i64 %t4856, ptr %t4834
  %t4857 = load i64, ptr %t4834
  %t4858 = add i64 79, 0
  %t4859 = and i64 %t4857, %t4858
  %t4860 = add i64 87, 0
  %t4861 = and i64 %t4859, %t4860
  %t4862 = add i64 65535, 0
  %t4863 = and i64 %t4861, %t4862
  store i64 %t4863, ptr %t4834
  %t4864 = load i64, ptr %t4834
  %t4865 = add i64 35, 0
  %t4866 = sub i64 %t4864, %t4865
  %t4867 = add i64 81, 0
  %t4868 = sub i64 %t4866, %t4867
  %t4869 = add i64 65535, 0
  %t4870 = and i64 %t4868, %t4869
  store i64 %t4870, ptr %t4834
  %t4871 = load i64, ptr %t4834
  %t4872 = call %NxVal @nx_int(i64 %t4871)
  ret %NxVal %t4872
}
define %NxVal @nx__m_3____main____Cell__m67(%NxVal* %args, i64 %nargs) {
entry:
  %t4873 = alloca %NxVal
  %t4877 = alloca i64
  %t4892 = alloca i64
  store %NxVal zeroinitializer, ptr %t4873
  %t4874 = getelementptr %NxVal, ptr %args, i64 0
  %t4875 = load %NxVal, ptr %t4874
  %t4876 = call %NxVal @nx_clone(%NxVal %t4875)
  store %NxVal %t4876, ptr %t4873
  %t4878 = getelementptr %NxVal, ptr %args, i64 1
  %t4879 = load %NxVal, ptr %t4878
  %t4880 = extractvalue %NxVal %t4879, 1
  store i64 %t4880, ptr %t4877
  %t4881 = load %NxVal, ptr %t4873
  %t4882 = call %NxVal @nx_rec_get(%NxVal %t4881, i64 0)
  %t4883 = load %NxVal, ptr %t4873
  %t4884 = call %NxVal @nx_rec_get(%NxVal %t4883, i64 1)
  %t4885 = call %NxVal @nx_add(%NxVal %t4882, %NxVal %t4884)
  %t4886 = load i64, ptr %t4877
  %t4887 = call %NxVal @nx_int(i64 %t4886)
  %t4888 = call %NxVal @nx_add(%NxVal %t4885, %NxVal %t4887)
  %t4889 = add i64 65535, 0
  %t4890 = call %NxVal @nx_int(i64 %t4889)
  %t4891 = call %NxVal @nx_bitand(%NxVal %t4888, %NxVal %t4890)
  %t4893 = extractvalue %NxVal %t4891, 1
  store i64 %t4893, ptr %t4892
  %t4894 = load i64, ptr %t4892
  %t4895 = add i64 12, 0
  %t4896 = and i64 %t4894, %t4895
  %t4897 = add i64 50, 0
  %t4898 = and i64 %t4896, %t4897
  %t4899 = add i64 65535, 0
  %t4900 = and i64 %t4898, %t4899
  store i64 %t4900, ptr %t4892
  %t4901 = load i64, ptr %t4892
  %t4902 = add i64 52, 0
  %t4903 = sub i64 %t4901, %t4902
  %t4904 = add i64 6, 0
  %t4905 = sub i64 %t4903, %t4904
  %t4906 = add i64 65535, 0
  %t4907 = and i64 %t4905, %t4906
  store i64 %t4907, ptr %t4892
  %t4908 = load i64, ptr %t4892
  %t4909 = add i64 59, 0
  %t4910 = call i64 @nx_mod_i64(i64 %t4908, i64 %t4909)
  %t4911 = add i64 79, 0
  %t4912 = call i64 @nx_mod_i64(i64 %t4910, i64 %t4911)
  %t4913 = add i64 65535, 0
  %t4914 = and i64 %t4912, %t4913
  store i64 %t4914, ptr %t4892
  %t4915 = load i64, ptr %t4892
  %t4916 = add i64 55, 0
  %t4917 = mul i64 %t4915, %t4916
  %t4918 = add i64 8, 0
  %t4919 = mul i64 %t4917, %t4918
  %t4920 = add i64 65535, 0
  %t4921 = and i64 %t4919, %t4920
  store i64 %t4921, ptr %t4892
  %t4922 = load i64, ptr %t4892
  %t4923 = add i64 49, 0
  %t4924 = sub i64 %t4922, %t4923
  %t4925 = add i64 85, 0
  %t4926 = sub i64 %t4924, %t4925
  %t4927 = add i64 65535, 0
  %t4928 = and i64 %t4926, %t4927
  store i64 %t4928, ptr %t4892
  %t4929 = load i64, ptr %t4892
  %t4930 = call %NxVal @nx_int(i64 %t4929)
  ret %NxVal %t4930
}
define %NxVal @nx__m_3____main____Cell__m68(%NxVal* %args, i64 %nargs) {
entry:
  %t4931 = alloca %NxVal
  %t4935 = alloca i64
  %t4950 = alloca i64
  store %NxVal zeroinitializer, ptr %t4931
  %t4932 = getelementptr %NxVal, ptr %args, i64 0
  %t4933 = load %NxVal, ptr %t4932
  %t4934 = call %NxVal @nx_clone(%NxVal %t4933)
  store %NxVal %t4934, ptr %t4931
  %t4936 = getelementptr %NxVal, ptr %args, i64 1
  %t4937 = load %NxVal, ptr %t4936
  %t4938 = extractvalue %NxVal %t4937, 1
  store i64 %t4938, ptr %t4935
  %t4939 = load %NxVal, ptr %t4931
  %t4940 = call %NxVal @nx_rec_get(%NxVal %t4939, i64 0)
  %t4941 = load %NxVal, ptr %t4931
  %t4942 = call %NxVal @nx_rec_get(%NxVal %t4941, i64 1)
  %t4943 = call %NxVal @nx_add(%NxVal %t4940, %NxVal %t4942)
  %t4944 = load i64, ptr %t4935
  %t4945 = call %NxVal @nx_int(i64 %t4944)
  %t4946 = call %NxVal @nx_add(%NxVal %t4943, %NxVal %t4945)
  %t4947 = add i64 65535, 0
  %t4948 = call %NxVal @nx_int(i64 %t4947)
  %t4949 = call %NxVal @nx_bitand(%NxVal %t4946, %NxVal %t4948)
  %t4951 = extractvalue %NxVal %t4949, 1
  store i64 %t4951, ptr %t4950
  %t4952 = load i64, ptr %t4950
  %t4953 = add i64 84, 0
  %t4954 = mul i64 %t4952, %t4953
  %t4955 = add i64 37, 0
  %t4956 = mul i64 %t4954, %t4955
  %t4957 = add i64 65535, 0
  %t4958 = and i64 %t4956, %t4957
  store i64 %t4958, ptr %t4950
  %t4959 = load i64, ptr %t4950
  %t4960 = add i64 24, 0
  %t4961 = xor i64 %t4959, %t4960
  %t4962 = add i64 42, 0
  %t4963 = xor i64 %t4961, %t4962
  %t4964 = add i64 65535, 0
  %t4965 = and i64 %t4963, %t4964
  store i64 %t4965, ptr %t4950
  %t4966 = load i64, ptr %t4950
  %t4967 = add i64 20, 0
  %t4968 = xor i64 %t4966, %t4967
  %t4969 = add i64 44, 0
  %t4970 = xor i64 %t4968, %t4969
  %t4971 = add i64 65535, 0
  %t4972 = and i64 %t4970, %t4971
  store i64 %t4972, ptr %t4950
  %t4973 = load i64, ptr %t4950
  %t4974 = add i64 51, 0
  %t4975 = or i64 %t4973, %t4974
  %t4976 = add i64 2, 0
  %t4977 = or i64 %t4975, %t4976
  %t4978 = add i64 65535, 0
  %t4979 = and i64 %t4977, %t4978
  store i64 %t4979, ptr %t4950
  %t4980 = load i64, ptr %t4950
  %t4981 = add i64 66, 0
  %t4982 = and i64 %t4980, %t4981
  %t4983 = add i64 16, 0
  %t4984 = and i64 %t4982, %t4983
  %t4985 = add i64 65535, 0
  %t4986 = and i64 %t4984, %t4985
  store i64 %t4986, ptr %t4950
  %t4987 = load i64, ptr %t4950
  %t4988 = call %NxVal @nx_int(i64 %t4987)
  ret %NxVal %t4988
}
define %NxVal @nx__m_3____main____Cell__m69(%NxVal* %args, i64 %nargs) {
entry:
  %t4989 = alloca %NxVal
  %t4993 = alloca i64
  %t5008 = alloca i64
  store %NxVal zeroinitializer, ptr %t4989
  %t4990 = getelementptr %NxVal, ptr %args, i64 0
  %t4991 = load %NxVal, ptr %t4990
  %t4992 = call %NxVal @nx_clone(%NxVal %t4991)
  store %NxVal %t4992, ptr %t4989
  %t4994 = getelementptr %NxVal, ptr %args, i64 1
  %t4995 = load %NxVal, ptr %t4994
  %t4996 = extractvalue %NxVal %t4995, 1
  store i64 %t4996, ptr %t4993
  %t4997 = load %NxVal, ptr %t4989
  %t4998 = call %NxVal @nx_rec_get(%NxVal %t4997, i64 0)
  %t4999 = load %NxVal, ptr %t4989
  %t5000 = call %NxVal @nx_rec_get(%NxVal %t4999, i64 1)
  %t5001 = call %NxVal @nx_add(%NxVal %t4998, %NxVal %t5000)
  %t5002 = load i64, ptr %t4993
  %t5003 = call %NxVal @nx_int(i64 %t5002)
  %t5004 = call %NxVal @nx_add(%NxVal %t5001, %NxVal %t5003)
  %t5005 = add i64 65535, 0
  %t5006 = call %NxVal @nx_int(i64 %t5005)
  %t5007 = call %NxVal @nx_bitand(%NxVal %t5004, %NxVal %t5006)
  %t5009 = extractvalue %NxVal %t5007, 1
  store i64 %t5009, ptr %t5008
  %t5010 = load i64, ptr %t5008
  %t5011 = add i64 91, 0
  %t5012 = call i64 @nx_mod_i64(i64 %t5010, i64 %t5011)
  %t5013 = add i64 72, 0
  %t5014 = call i64 @nx_mod_i64(i64 %t5012, i64 %t5013)
  %t5015 = add i64 65535, 0
  %t5016 = and i64 %t5014, %t5015
  store i64 %t5016, ptr %t5008
  %t5017 = load i64, ptr %t5008
  %t5018 = add i64 84, 0
  %t5019 = add i64 %t5017, %t5018
  %t5020 = add i64 70, 0
  %t5021 = add i64 %t5019, %t5020
  %t5022 = add i64 65535, 0
  %t5023 = and i64 %t5021, %t5022
  store i64 %t5023, ptr %t5008
  %t5024 = load i64, ptr %t5008
  %t5025 = add i64 94, 0
  %t5026 = and i64 %t5024, %t5025
  %t5027 = add i64 65, 0
  %t5028 = and i64 %t5026, %t5027
  %t5029 = add i64 65535, 0
  %t5030 = and i64 %t5028, %t5029
  store i64 %t5030, ptr %t5008
  %t5031 = load i64, ptr %t5008
  %t5032 = add i64 29, 0
  %t5033 = or i64 %t5031, %t5032
  %t5034 = add i64 28, 0
  %t5035 = or i64 %t5033, %t5034
  %t5036 = add i64 65535, 0
  %t5037 = and i64 %t5035, %t5036
  store i64 %t5037, ptr %t5008
  %t5038 = load i64, ptr %t5008
  %t5039 = add i64 83, 0
  %t5040 = mul i64 %t5038, %t5039
  %t5041 = add i64 2, 0
  %t5042 = mul i64 %t5040, %t5041
  %t5043 = add i64 65535, 0
  %t5044 = and i64 %t5042, %t5043
  store i64 %t5044, ptr %t5008
  %t5045 = load i64, ptr %t5008
  %t5046 = call %NxVal @nx_int(i64 %t5045)
  ret %NxVal %t5046
}
define %NxVal @nx__m_2____main____Cell__m7(%NxVal* %args, i64 %nargs) {
entry:
  %t5047 = alloca %NxVal
  %t5051 = alloca i64
  %t5066 = alloca i64
  store %NxVal zeroinitializer, ptr %t5047
  %t5048 = getelementptr %NxVal, ptr %args, i64 0
  %t5049 = load %NxVal, ptr %t5048
  %t5050 = call %NxVal @nx_clone(%NxVal %t5049)
  store %NxVal %t5050, ptr %t5047
  %t5052 = getelementptr %NxVal, ptr %args, i64 1
  %t5053 = load %NxVal, ptr %t5052
  %t5054 = extractvalue %NxVal %t5053, 1
  store i64 %t5054, ptr %t5051
  %t5055 = load %NxVal, ptr %t5047
  %t5056 = call %NxVal @nx_rec_get(%NxVal %t5055, i64 0)
  %t5057 = load %NxVal, ptr %t5047
  %t5058 = call %NxVal @nx_rec_get(%NxVal %t5057, i64 1)
  %t5059 = call %NxVal @nx_add(%NxVal %t5056, %NxVal %t5058)
  %t5060 = load i64, ptr %t5051
  %t5061 = call %NxVal @nx_int(i64 %t5060)
  %t5062 = call %NxVal @nx_add(%NxVal %t5059, %NxVal %t5061)
  %t5063 = add i64 65535, 0
  %t5064 = call %NxVal @nx_int(i64 %t5063)
  %t5065 = call %NxVal @nx_bitand(%NxVal %t5062, %NxVal %t5064)
  %t5067 = extractvalue %NxVal %t5065, 1
  store i64 %t5067, ptr %t5066
  %t5068 = load i64, ptr %t5066
  %t5069 = add i64 33, 0
  %t5070 = and i64 %t5068, %t5069
  %t5071 = add i64 60, 0
  %t5072 = and i64 %t5070, %t5071
  %t5073 = add i64 65535, 0
  %t5074 = and i64 %t5072, %t5073
  store i64 %t5074, ptr %t5066
  %t5075 = load i64, ptr %t5066
  %t5076 = add i64 20, 0
  %t5077 = or i64 %t5075, %t5076
  %t5078 = add i64 65, 0
  %t5079 = or i64 %t5077, %t5078
  %t5080 = add i64 65535, 0
  %t5081 = and i64 %t5079, %t5080
  store i64 %t5081, ptr %t5066
  %t5082 = load i64, ptr %t5066
  %t5083 = add i64 36, 0
  %t5084 = and i64 %t5082, %t5083
  %t5085 = add i64 18, 0
  %t5086 = and i64 %t5084, %t5085
  %t5087 = add i64 65535, 0
  %t5088 = and i64 %t5086, %t5087
  store i64 %t5088, ptr %t5066
  %t5089 = load i64, ptr %t5066
  %t5090 = add i64 23, 0
  %t5091 = call i64 @nx_mod_i64(i64 %t5089, i64 %t5090)
  %t5092 = add i64 15, 0
  %t5093 = call i64 @nx_mod_i64(i64 %t5091, i64 %t5092)
  %t5094 = add i64 65535, 0
  %t5095 = and i64 %t5093, %t5094
  store i64 %t5095, ptr %t5066
  %t5096 = load i64, ptr %t5066
  %t5097 = add i64 73, 0
  %t5098 = call i64 @nx_mod_i64(i64 %t5096, i64 %t5097)
  %t5099 = add i64 69, 0
  %t5100 = call i64 @nx_mod_i64(i64 %t5098, i64 %t5099)
  %t5101 = add i64 65535, 0
  %t5102 = and i64 %t5100, %t5101
  store i64 %t5102, ptr %t5066
  %t5103 = load i64, ptr %t5066
  %t5104 = call %NxVal @nx_int(i64 %t5103)
  ret %NxVal %t5104
}
define %NxVal @nx__m_3____main____Cell__m70(%NxVal* %args, i64 %nargs) {
entry:
  %t5105 = alloca %NxVal
  %t5109 = alloca i64
  %t5124 = alloca i64
  store %NxVal zeroinitializer, ptr %t5105
  %t5106 = getelementptr %NxVal, ptr %args, i64 0
  %t5107 = load %NxVal, ptr %t5106
  %t5108 = call %NxVal @nx_clone(%NxVal %t5107)
  store %NxVal %t5108, ptr %t5105
  %t5110 = getelementptr %NxVal, ptr %args, i64 1
  %t5111 = load %NxVal, ptr %t5110
  %t5112 = extractvalue %NxVal %t5111, 1
  store i64 %t5112, ptr %t5109
  %t5113 = load %NxVal, ptr %t5105
  %t5114 = call %NxVal @nx_rec_get(%NxVal %t5113, i64 0)
  %t5115 = load %NxVal, ptr %t5105
  %t5116 = call %NxVal @nx_rec_get(%NxVal %t5115, i64 1)
  %t5117 = call %NxVal @nx_add(%NxVal %t5114, %NxVal %t5116)
  %t5118 = load i64, ptr %t5109
  %t5119 = call %NxVal @nx_int(i64 %t5118)
  %t5120 = call %NxVal @nx_add(%NxVal %t5117, %NxVal %t5119)
  %t5121 = add i64 65535, 0
  %t5122 = call %NxVal @nx_int(i64 %t5121)
  %t5123 = call %NxVal @nx_bitand(%NxVal %t5120, %NxVal %t5122)
  %t5125 = extractvalue %NxVal %t5123, 1
  store i64 %t5125, ptr %t5124
  %t5126 = load i64, ptr %t5124
  %t5127 = add i64 30, 0
  %t5128 = or i64 %t5126, %t5127
  %t5129 = add i64 54, 0
  %t5130 = or i64 %t5128, %t5129
  %t5131 = add i64 65535, 0
  %t5132 = and i64 %t5130, %t5131
  store i64 %t5132, ptr %t5124
  %t5133 = load i64, ptr %t5124
  %t5134 = add i64 35, 0
  %t5135 = call i64 @nx_mod_i64(i64 %t5133, i64 %t5134)
  %t5136 = add i64 23, 0
  %t5137 = call i64 @nx_mod_i64(i64 %t5135, i64 %t5136)
  %t5138 = add i64 65535, 0
  %t5139 = and i64 %t5137, %t5138
  store i64 %t5139, ptr %t5124
  %t5140 = load i64, ptr %t5124
  %t5141 = add i64 47, 0
  %t5142 = or i64 %t5140, %t5141
  %t5143 = add i64 78, 0
  %t5144 = or i64 %t5142, %t5143
  %t5145 = add i64 65535, 0
  %t5146 = and i64 %t5144, %t5145
  store i64 %t5146, ptr %t5124
  %t5147 = load i64, ptr %t5124
  %t5148 = add i64 35, 0
  %t5149 = xor i64 %t5147, %t5148
  %t5150 = add i64 75, 0
  %t5151 = xor i64 %t5149, %t5150
  %t5152 = add i64 65535, 0
  %t5153 = and i64 %t5151, %t5152
  store i64 %t5153, ptr %t5124
  %t5154 = load i64, ptr %t5124
  %t5155 = add i64 15, 0
  %t5156 = xor i64 %t5154, %t5155
  %t5157 = add i64 18, 0
  %t5158 = xor i64 %t5156, %t5157
  %t5159 = add i64 65535, 0
  %t5160 = and i64 %t5158, %t5159
  store i64 %t5160, ptr %t5124
  %t5161 = load i64, ptr %t5124
  %t5162 = call %NxVal @nx_int(i64 %t5161)
  ret %NxVal %t5162
}
define %NxVal @nx__m_3____main____Cell__m71(%NxVal* %args, i64 %nargs) {
entry:
  %t5163 = alloca %NxVal
  %t5167 = alloca i64
  %t5182 = alloca i64
  store %NxVal zeroinitializer, ptr %t5163
  %t5164 = getelementptr %NxVal, ptr %args, i64 0
  %t5165 = load %NxVal, ptr %t5164
  %t5166 = call %NxVal @nx_clone(%NxVal %t5165)
  store %NxVal %t5166, ptr %t5163
  %t5168 = getelementptr %NxVal, ptr %args, i64 1
  %t5169 = load %NxVal, ptr %t5168
  %t5170 = extractvalue %NxVal %t5169, 1
  store i64 %t5170, ptr %t5167
  %t5171 = load %NxVal, ptr %t5163
  %t5172 = call %NxVal @nx_rec_get(%NxVal %t5171, i64 0)
  %t5173 = load %NxVal, ptr %t5163
  %t5174 = call %NxVal @nx_rec_get(%NxVal %t5173, i64 1)
  %t5175 = call %NxVal @nx_add(%NxVal %t5172, %NxVal %t5174)
  %t5176 = load i64, ptr %t5167
  %t5177 = call %NxVal @nx_int(i64 %t5176)
  %t5178 = call %NxVal @nx_add(%NxVal %t5175, %NxVal %t5177)
  %t5179 = add i64 65535, 0
  %t5180 = call %NxVal @nx_int(i64 %t5179)
  %t5181 = call %NxVal @nx_bitand(%NxVal %t5178, %NxVal %t5180)
  %t5183 = extractvalue %NxVal %t5181, 1
  store i64 %t5183, ptr %t5182
  %t5184 = load i64, ptr %t5182
  %t5185 = add i64 80, 0
  %t5186 = call i64 @nx_mod_i64(i64 %t5184, i64 %t5185)
  %t5187 = add i64 58, 0
  %t5188 = call i64 @nx_mod_i64(i64 %t5186, i64 %t5187)
  %t5189 = add i64 65535, 0
  %t5190 = and i64 %t5188, %t5189
  store i64 %t5190, ptr %t5182
  %t5191 = load i64, ptr %t5182
  %t5192 = add i64 32, 0
  %t5193 = add i64 %t5191, %t5192
  %t5194 = add i64 61, 0
  %t5195 = add i64 %t5193, %t5194
  %t5196 = add i64 65535, 0
  %t5197 = and i64 %t5195, %t5196
  store i64 %t5197, ptr %t5182
  %t5198 = load i64, ptr %t5182
  %t5199 = add i64 16, 0
  %t5200 = sub i64 %t5198, %t5199
  %t5201 = add i64 59, 0
  %t5202 = sub i64 %t5200, %t5201
  %t5203 = add i64 65535, 0
  %t5204 = and i64 %t5202, %t5203
  store i64 %t5204, ptr %t5182
  %t5205 = load i64, ptr %t5182
  %t5206 = add i64 18, 0
  %t5207 = sub i64 %t5205, %t5206
  %t5208 = add i64 55, 0
  %t5209 = sub i64 %t5207, %t5208
  %t5210 = add i64 65535, 0
  %t5211 = and i64 %t5209, %t5210
  store i64 %t5211, ptr %t5182
  %t5212 = load i64, ptr %t5182
  %t5213 = add i64 62, 0
  %t5214 = add i64 %t5212, %t5213
  %t5215 = add i64 57, 0
  %t5216 = add i64 %t5214, %t5215
  %t5217 = add i64 65535, 0
  %t5218 = and i64 %t5216, %t5217
  store i64 %t5218, ptr %t5182
  %t5219 = load i64, ptr %t5182
  %t5220 = call %NxVal @nx_int(i64 %t5219)
  ret %NxVal %t5220
}
define %NxVal @nx__m_3____main____Cell__m72(%NxVal* %args, i64 %nargs) {
entry:
  %t5221 = alloca %NxVal
  %t5225 = alloca i64
  %t5240 = alloca i64
  store %NxVal zeroinitializer, ptr %t5221
  %t5222 = getelementptr %NxVal, ptr %args, i64 0
  %t5223 = load %NxVal, ptr %t5222
  %t5224 = call %NxVal @nx_clone(%NxVal %t5223)
  store %NxVal %t5224, ptr %t5221
  %t5226 = getelementptr %NxVal, ptr %args, i64 1
  %t5227 = load %NxVal, ptr %t5226
  %t5228 = extractvalue %NxVal %t5227, 1
  store i64 %t5228, ptr %t5225
  %t5229 = load %NxVal, ptr %t5221
  %t5230 = call %NxVal @nx_rec_get(%NxVal %t5229, i64 0)
  %t5231 = load %NxVal, ptr %t5221
  %t5232 = call %NxVal @nx_rec_get(%NxVal %t5231, i64 1)
  %t5233 = call %NxVal @nx_add(%NxVal %t5230, %NxVal %t5232)
  %t5234 = load i64, ptr %t5225
  %t5235 = call %NxVal @nx_int(i64 %t5234)
  %t5236 = call %NxVal @nx_add(%NxVal %t5233, %NxVal %t5235)
  %t5237 = add i64 65535, 0
  %t5238 = call %NxVal @nx_int(i64 %t5237)
  %t5239 = call %NxVal @nx_bitand(%NxVal %t5236, %NxVal %t5238)
  %t5241 = extractvalue %NxVal %t5239, 1
  store i64 %t5241, ptr %t5240
  %t5242 = load i64, ptr %t5240
  %t5243 = add i64 85, 0
  %t5244 = or i64 %t5242, %t5243
  %t5245 = add i64 27, 0
  %t5246 = or i64 %t5244, %t5245
  %t5247 = add i64 65535, 0
  %t5248 = and i64 %t5246, %t5247
  store i64 %t5248, ptr %t5240
  %t5249 = load i64, ptr %t5240
  %t5250 = add i64 16, 0
  %t5251 = add i64 %t5249, %t5250
  %t5252 = add i64 17, 0
  %t5253 = add i64 %t5251, %t5252
  %t5254 = add i64 65535, 0
  %t5255 = and i64 %t5253, %t5254
  store i64 %t5255, ptr %t5240
  %t5256 = load i64, ptr %t5240
  %t5257 = add i64 89, 0
  %t5258 = mul i64 %t5256, %t5257
  %t5259 = add i64 61, 0
  %t5260 = mul i64 %t5258, %t5259
  %t5261 = add i64 65535, 0
  %t5262 = and i64 %t5260, %t5261
  store i64 %t5262, ptr %t5240
  %t5263 = load i64, ptr %t5240
  %t5264 = add i64 12, 0
  %t5265 = mul i64 %t5263, %t5264
  %t5266 = add i64 51, 0
  %t5267 = mul i64 %t5265, %t5266
  %t5268 = add i64 65535, 0
  %t5269 = and i64 %t5267, %t5268
  store i64 %t5269, ptr %t5240
  %t5270 = load i64, ptr %t5240
  %t5271 = add i64 56, 0
  %t5272 = mul i64 %t5270, %t5271
  %t5273 = add i64 76, 0
  %t5274 = mul i64 %t5272, %t5273
  %t5275 = add i64 65535, 0
  %t5276 = and i64 %t5274, %t5275
  store i64 %t5276, ptr %t5240
  %t5277 = load i64, ptr %t5240
  %t5278 = call %NxVal @nx_int(i64 %t5277)
  ret %NxVal %t5278
}
define %NxVal @nx__m_3____main____Cell__m73(%NxVal* %args, i64 %nargs) {
entry:
  %t5279 = alloca %NxVal
  %t5283 = alloca i64
  %t5298 = alloca i64
  store %NxVal zeroinitializer, ptr %t5279
  %t5280 = getelementptr %NxVal, ptr %args, i64 0
  %t5281 = load %NxVal, ptr %t5280
  %t5282 = call %NxVal @nx_clone(%NxVal %t5281)
  store %NxVal %t5282, ptr %t5279
  %t5284 = getelementptr %NxVal, ptr %args, i64 1
  %t5285 = load %NxVal, ptr %t5284
  %t5286 = extractvalue %NxVal %t5285, 1
  store i64 %t5286, ptr %t5283
  %t5287 = load %NxVal, ptr %t5279
  %t5288 = call %NxVal @nx_rec_get(%NxVal %t5287, i64 0)
  %t5289 = load %NxVal, ptr %t5279
  %t5290 = call %NxVal @nx_rec_get(%NxVal %t5289, i64 1)
  %t5291 = call %NxVal @nx_add(%NxVal %t5288, %NxVal %t5290)
  %t5292 = load i64, ptr %t5283
  %t5293 = call %NxVal @nx_int(i64 %t5292)
  %t5294 = call %NxVal @nx_add(%NxVal %t5291, %NxVal %t5293)
  %t5295 = add i64 65535, 0
  %t5296 = call %NxVal @nx_int(i64 %t5295)
  %t5297 = call %NxVal @nx_bitand(%NxVal %t5294, %NxVal %t5296)
  %t5299 = extractvalue %NxVal %t5297, 1
  store i64 %t5299, ptr %t5298
  %t5300 = load i64, ptr %t5298
  %t5301 = add i64 49, 0
  %t5302 = xor i64 %t5300, %t5301
  %t5303 = add i64 49, 0
  %t5304 = xor i64 %t5302, %t5303
  %t5305 = add i64 65535, 0
  %t5306 = and i64 %t5304, %t5305
  store i64 %t5306, ptr %t5298
  %t5307 = load i64, ptr %t5298
  %t5308 = add i64 12, 0
  %t5309 = or i64 %t5307, %t5308
  %t5310 = add i64 75, 0
  %t5311 = or i64 %t5309, %t5310
  %t5312 = add i64 65535, 0
  %t5313 = and i64 %t5311, %t5312
  store i64 %t5313, ptr %t5298
  %t5314 = load i64, ptr %t5298
  %t5315 = add i64 26, 0
  %t5316 = or i64 %t5314, %t5315
  %t5317 = add i64 36, 0
  %t5318 = or i64 %t5316, %t5317
  %t5319 = add i64 65535, 0
  %t5320 = and i64 %t5318, %t5319
  store i64 %t5320, ptr %t5298
  %t5321 = load i64, ptr %t5298
  %t5322 = add i64 6, 0
  %t5323 = add i64 %t5321, %t5322
  %t5324 = add i64 67, 0
  %t5325 = add i64 %t5323, %t5324
  %t5326 = add i64 65535, 0
  %t5327 = and i64 %t5325, %t5326
  store i64 %t5327, ptr %t5298
  %t5328 = load i64, ptr %t5298
  %t5329 = add i64 45, 0
  %t5330 = call i64 @nx_mod_i64(i64 %t5328, i64 %t5329)
  %t5331 = add i64 73, 0
  %t5332 = call i64 @nx_mod_i64(i64 %t5330, i64 %t5331)
  %t5333 = add i64 65535, 0
  %t5334 = and i64 %t5332, %t5333
  store i64 %t5334, ptr %t5298
  %t5335 = load i64, ptr %t5298
  %t5336 = call %NxVal @nx_int(i64 %t5335)
  ret %NxVal %t5336
}
define %NxVal @nx__m_3____main____Cell__m74(%NxVal* %args, i64 %nargs) {
entry:
  %t5337 = alloca %NxVal
  %t5341 = alloca i64
  %t5356 = alloca i64
  store %NxVal zeroinitializer, ptr %t5337
  %t5338 = getelementptr %NxVal, ptr %args, i64 0
  %t5339 = load %NxVal, ptr %t5338
  %t5340 = call %NxVal @nx_clone(%NxVal %t5339)
  store %NxVal %t5340, ptr %t5337
  %t5342 = getelementptr %NxVal, ptr %args, i64 1
  %t5343 = load %NxVal, ptr %t5342
  %t5344 = extractvalue %NxVal %t5343, 1
  store i64 %t5344, ptr %t5341
  %t5345 = load %NxVal, ptr %t5337
  %t5346 = call %NxVal @nx_rec_get(%NxVal %t5345, i64 0)
  %t5347 = load %NxVal, ptr %t5337
  %t5348 = call %NxVal @nx_rec_get(%NxVal %t5347, i64 1)
  %t5349 = call %NxVal @nx_add(%NxVal %t5346, %NxVal %t5348)
  %t5350 = load i64, ptr %t5341
  %t5351 = call %NxVal @nx_int(i64 %t5350)
  %t5352 = call %NxVal @nx_add(%NxVal %t5349, %NxVal %t5351)
  %t5353 = add i64 65535, 0
  %t5354 = call %NxVal @nx_int(i64 %t5353)
  %t5355 = call %NxVal @nx_bitand(%NxVal %t5352, %NxVal %t5354)
  %t5357 = extractvalue %NxVal %t5355, 1
  store i64 %t5357, ptr %t5356
  %t5358 = load i64, ptr %t5356
  %t5359 = add i64 79, 0
  %t5360 = call i64 @nx_mod_i64(i64 %t5358, i64 %t5359)
  %t5361 = add i64 44, 0
  %t5362 = call i64 @nx_mod_i64(i64 %t5360, i64 %t5361)
  %t5363 = add i64 65535, 0
  %t5364 = and i64 %t5362, %t5363
  store i64 %t5364, ptr %t5356
  %t5365 = load i64, ptr %t5356
  %t5366 = add i64 93, 0
  %t5367 = sub i64 %t5365, %t5366
  %t5368 = add i64 65, 0
  %t5369 = sub i64 %t5367, %t5368
  %t5370 = add i64 65535, 0
  %t5371 = and i64 %t5369, %t5370
  store i64 %t5371, ptr %t5356
  %t5372 = load i64, ptr %t5356
  %t5373 = add i64 81, 0
  %t5374 = mul i64 %t5372, %t5373
  %t5375 = add i64 57, 0
  %t5376 = mul i64 %t5374, %t5375
  %t5377 = add i64 65535, 0
  %t5378 = and i64 %t5376, %t5377
  store i64 %t5378, ptr %t5356
  %t5379 = load i64, ptr %t5356
  %t5380 = add i64 61, 0
  %t5381 = add i64 %t5379, %t5380
  %t5382 = add i64 67, 0
  %t5383 = add i64 %t5381, %t5382
  %t5384 = add i64 65535, 0
  %t5385 = and i64 %t5383, %t5384
  store i64 %t5385, ptr %t5356
  %t5386 = load i64, ptr %t5356
  %t5387 = add i64 7, 0
  %t5388 = add i64 %t5386, %t5387
  %t5389 = add i64 15, 0
  %t5390 = add i64 %t5388, %t5389
  %t5391 = add i64 65535, 0
  %t5392 = and i64 %t5390, %t5391
  store i64 %t5392, ptr %t5356
  %t5393 = load i64, ptr %t5356
  %t5394 = call %NxVal @nx_int(i64 %t5393)
  ret %NxVal %t5394
}
define %NxVal @nx__m_3____main____Cell__m75(%NxVal* %args, i64 %nargs) {
entry:
  %t5395 = alloca %NxVal
  %t5399 = alloca i64
  %t5414 = alloca i64
  store %NxVal zeroinitializer, ptr %t5395
  %t5396 = getelementptr %NxVal, ptr %args, i64 0
  %t5397 = load %NxVal, ptr %t5396
  %t5398 = call %NxVal @nx_clone(%NxVal %t5397)
  store %NxVal %t5398, ptr %t5395
  %t5400 = getelementptr %NxVal, ptr %args, i64 1
  %t5401 = load %NxVal, ptr %t5400
  %t5402 = extractvalue %NxVal %t5401, 1
  store i64 %t5402, ptr %t5399
  %t5403 = load %NxVal, ptr %t5395
  %t5404 = call %NxVal @nx_rec_get(%NxVal %t5403, i64 0)
  %t5405 = load %NxVal, ptr %t5395
  %t5406 = call %NxVal @nx_rec_get(%NxVal %t5405, i64 1)
  %t5407 = call %NxVal @nx_add(%NxVal %t5404, %NxVal %t5406)
  %t5408 = load i64, ptr %t5399
  %t5409 = call %NxVal @nx_int(i64 %t5408)
  %t5410 = call %NxVal @nx_add(%NxVal %t5407, %NxVal %t5409)
  %t5411 = add i64 65535, 0
  %t5412 = call %NxVal @nx_int(i64 %t5411)
  %t5413 = call %NxVal @nx_bitand(%NxVal %t5410, %NxVal %t5412)
  %t5415 = extractvalue %NxVal %t5413, 1
  store i64 %t5415, ptr %t5414
  %t5416 = load i64, ptr %t5414
  %t5417 = add i64 44, 0
  %t5418 = mul i64 %t5416, %t5417
  %t5419 = add i64 47, 0
  %t5420 = mul i64 %t5418, %t5419
  %t5421 = add i64 65535, 0
  %t5422 = and i64 %t5420, %t5421
  store i64 %t5422, ptr %t5414
  %t5423 = load i64, ptr %t5414
  %t5424 = add i64 17, 0
  %t5425 = xor i64 %t5423, %t5424
  %t5426 = add i64 50, 0
  %t5427 = xor i64 %t5425, %t5426
  %t5428 = add i64 65535, 0
  %t5429 = and i64 %t5427, %t5428
  store i64 %t5429, ptr %t5414
  %t5430 = load i64, ptr %t5414
  %t5431 = add i64 10, 0
  %t5432 = call i64 @nx_mod_i64(i64 %t5430, i64 %t5431)
  %t5433 = add i64 81, 0
  %t5434 = call i64 @nx_mod_i64(i64 %t5432, i64 %t5433)
  %t5435 = add i64 65535, 0
  %t5436 = and i64 %t5434, %t5435
  store i64 %t5436, ptr %t5414
  %t5437 = load i64, ptr %t5414
  %t5438 = add i64 9, 0
  %t5439 = mul i64 %t5437, %t5438
  %t5440 = add i64 68, 0
  %t5441 = mul i64 %t5439, %t5440
  %t5442 = add i64 65535, 0
  %t5443 = and i64 %t5441, %t5442
  store i64 %t5443, ptr %t5414
  %t5444 = load i64, ptr %t5414
  %t5445 = add i64 30, 0
  %t5446 = xor i64 %t5444, %t5445
  %t5447 = add i64 9, 0
  %t5448 = xor i64 %t5446, %t5447
  %t5449 = add i64 65535, 0
  %t5450 = and i64 %t5448, %t5449
  store i64 %t5450, ptr %t5414
  %t5451 = load i64, ptr %t5414
  %t5452 = call %NxVal @nx_int(i64 %t5451)
  ret %NxVal %t5452
}
define %NxVal @nx__m_3____main____Cell__m76(%NxVal* %args, i64 %nargs) {
entry:
  %t5453 = alloca %NxVal
  %t5457 = alloca i64
  %t5472 = alloca i64
  store %NxVal zeroinitializer, ptr %t5453
  %t5454 = getelementptr %NxVal, ptr %args, i64 0
  %t5455 = load %NxVal, ptr %t5454
  %t5456 = call %NxVal @nx_clone(%NxVal %t5455)
  store %NxVal %t5456, ptr %t5453
  %t5458 = getelementptr %NxVal, ptr %args, i64 1
  %t5459 = load %NxVal, ptr %t5458
  %t5460 = extractvalue %NxVal %t5459, 1
  store i64 %t5460, ptr %t5457
  %t5461 = load %NxVal, ptr %t5453
  %t5462 = call %NxVal @nx_rec_get(%NxVal %t5461, i64 0)
  %t5463 = load %NxVal, ptr %t5453
  %t5464 = call %NxVal @nx_rec_get(%NxVal %t5463, i64 1)
  %t5465 = call %NxVal @nx_add(%NxVal %t5462, %NxVal %t5464)
  %t5466 = load i64, ptr %t5457
  %t5467 = call %NxVal @nx_int(i64 %t5466)
  %t5468 = call %NxVal @nx_add(%NxVal %t5465, %NxVal %t5467)
  %t5469 = add i64 65535, 0
  %t5470 = call %NxVal @nx_int(i64 %t5469)
  %t5471 = call %NxVal @nx_bitand(%NxVal %t5468, %NxVal %t5470)
  %t5473 = extractvalue %NxVal %t5471, 1
  store i64 %t5473, ptr %t5472
  %t5474 = load i64, ptr %t5472
  %t5475 = add i64 12, 0
  %t5476 = sub i64 %t5474, %t5475
  %t5477 = add i64 56, 0
  %t5478 = sub i64 %t5476, %t5477
  %t5479 = add i64 65535, 0
  %t5480 = and i64 %t5478, %t5479
  store i64 %t5480, ptr %t5472
  %t5481 = load i64, ptr %t5472
  %t5482 = add i64 19, 0
  %t5483 = xor i64 %t5481, %t5482
  %t5484 = add i64 87, 0
  %t5485 = xor i64 %t5483, %t5484
  %t5486 = add i64 65535, 0
  %t5487 = and i64 %t5485, %t5486
  store i64 %t5487, ptr %t5472
  %t5488 = load i64, ptr %t5472
  %t5489 = add i64 26, 0
  %t5490 = sub i64 %t5488, %t5489
  %t5491 = add i64 10, 0
  %t5492 = sub i64 %t5490, %t5491
  %t5493 = add i64 65535, 0
  %t5494 = and i64 %t5492, %t5493
  store i64 %t5494, ptr %t5472
  %t5495 = load i64, ptr %t5472
  %t5496 = add i64 48, 0
  %t5497 = or i64 %t5495, %t5496
  %t5498 = add i64 41, 0
  %t5499 = or i64 %t5497, %t5498
  %t5500 = add i64 65535, 0
  %t5501 = and i64 %t5499, %t5500
  store i64 %t5501, ptr %t5472
  %t5502 = load i64, ptr %t5472
  %t5503 = add i64 61, 0
  %t5504 = call i64 @nx_mod_i64(i64 %t5502, i64 %t5503)
  %t5505 = add i64 42, 0
  %t5506 = call i64 @nx_mod_i64(i64 %t5504, i64 %t5505)
  %t5507 = add i64 65535, 0
  %t5508 = and i64 %t5506, %t5507
  store i64 %t5508, ptr %t5472
  %t5509 = load i64, ptr %t5472
  %t5510 = call %NxVal @nx_int(i64 %t5509)
  ret %NxVal %t5510
}
define %NxVal @nx__m_3____main____Cell__m77(%NxVal* %args, i64 %nargs) {
entry:
  %t5511 = alloca %NxVal
  %t5515 = alloca i64
  %t5530 = alloca i64
  store %NxVal zeroinitializer, ptr %t5511
  %t5512 = getelementptr %NxVal, ptr %args, i64 0
  %t5513 = load %NxVal, ptr %t5512
  %t5514 = call %NxVal @nx_clone(%NxVal %t5513)
  store %NxVal %t5514, ptr %t5511
  %t5516 = getelementptr %NxVal, ptr %args, i64 1
  %t5517 = load %NxVal, ptr %t5516
  %t5518 = extractvalue %NxVal %t5517, 1
  store i64 %t5518, ptr %t5515
  %t5519 = load %NxVal, ptr %t5511
  %t5520 = call %NxVal @nx_rec_get(%NxVal %t5519, i64 0)
  %t5521 = load %NxVal, ptr %t5511
  %t5522 = call %NxVal @nx_rec_get(%NxVal %t5521, i64 1)
  %t5523 = call %NxVal @nx_add(%NxVal %t5520, %NxVal %t5522)
  %t5524 = load i64, ptr %t5515
  %t5525 = call %NxVal @nx_int(i64 %t5524)
  %t5526 = call %NxVal @nx_add(%NxVal %t5523, %NxVal %t5525)
  %t5527 = add i64 65535, 0
  %t5528 = call %NxVal @nx_int(i64 %t5527)
  %t5529 = call %NxVal @nx_bitand(%NxVal %t5526, %NxVal %t5528)
  %t5531 = extractvalue %NxVal %t5529, 1
  store i64 %t5531, ptr %t5530
  %t5532 = load i64, ptr %t5530
  %t5533 = add i64 78, 0
  %t5534 = add i64 %t5532, %t5533
  %t5535 = add i64 19, 0
  %t5536 = add i64 %t5534, %t5535
  %t5537 = add i64 65535, 0
  %t5538 = and i64 %t5536, %t5537
  store i64 %t5538, ptr %t5530
  %t5539 = load i64, ptr %t5530
  %t5540 = add i64 83, 0
  %t5541 = and i64 %t5539, %t5540
  %t5542 = add i64 53, 0
  %t5543 = and i64 %t5541, %t5542
  %t5544 = add i64 65535, 0
  %t5545 = and i64 %t5543, %t5544
  store i64 %t5545, ptr %t5530
  %t5546 = load i64, ptr %t5530
  %t5547 = add i64 31, 0
  %t5548 = mul i64 %t5546, %t5547
  %t5549 = add i64 73, 0
  %t5550 = mul i64 %t5548, %t5549
  %t5551 = add i64 65535, 0
  %t5552 = and i64 %t5550, %t5551
  store i64 %t5552, ptr %t5530
  %t5553 = load i64, ptr %t5530
  %t5554 = add i64 81, 0
  %t5555 = or i64 %t5553, %t5554
  %t5556 = add i64 41, 0
  %t5557 = or i64 %t5555, %t5556
  %t5558 = add i64 65535, 0
  %t5559 = and i64 %t5557, %t5558
  store i64 %t5559, ptr %t5530
  %t5560 = load i64, ptr %t5530
  %t5561 = add i64 29, 0
  %t5562 = add i64 %t5560, %t5561
  %t5563 = add i64 76, 0
  %t5564 = add i64 %t5562, %t5563
  %t5565 = add i64 65535, 0
  %t5566 = and i64 %t5564, %t5565
  store i64 %t5566, ptr %t5530
  %t5567 = load i64, ptr %t5530
  %t5568 = call %NxVal @nx_int(i64 %t5567)
  ret %NxVal %t5568
}
define %NxVal @nx__m_3____main____Cell__m78(%NxVal* %args, i64 %nargs) {
entry:
  %t5569 = alloca %NxVal
  %t5573 = alloca i64
  %t5588 = alloca i64
  store %NxVal zeroinitializer, ptr %t5569
  %t5570 = getelementptr %NxVal, ptr %args, i64 0
  %t5571 = load %NxVal, ptr %t5570
  %t5572 = call %NxVal @nx_clone(%NxVal %t5571)
  store %NxVal %t5572, ptr %t5569
  %t5574 = getelementptr %NxVal, ptr %args, i64 1
  %t5575 = load %NxVal, ptr %t5574
  %t5576 = extractvalue %NxVal %t5575, 1
  store i64 %t5576, ptr %t5573
  %t5577 = load %NxVal, ptr %t5569
  %t5578 = call %NxVal @nx_rec_get(%NxVal %t5577, i64 0)
  %t5579 = load %NxVal, ptr %t5569
  %t5580 = call %NxVal @nx_rec_get(%NxVal %t5579, i64 1)
  %t5581 = call %NxVal @nx_add(%NxVal %t5578, %NxVal %t5580)
  %t5582 = load i64, ptr %t5573
  %t5583 = call %NxVal @nx_int(i64 %t5582)
  %t5584 = call %NxVal @nx_add(%NxVal %t5581, %NxVal %t5583)
  %t5585 = add i64 65535, 0
  %t5586 = call %NxVal @nx_int(i64 %t5585)
  %t5587 = call %NxVal @nx_bitand(%NxVal %t5584, %NxVal %t5586)
  %t5589 = extractvalue %NxVal %t5587, 1
  store i64 %t5589, ptr %t5588
  %t5590 = load i64, ptr %t5588
  %t5591 = add i64 94, 0
  %t5592 = xor i64 %t5590, %t5591
  %t5593 = add i64 87, 0
  %t5594 = xor i64 %t5592, %t5593
  %t5595 = add i64 65535, 0
  %t5596 = and i64 %t5594, %t5595
  store i64 %t5596, ptr %t5588
  %t5597 = load i64, ptr %t5588
  %t5598 = add i64 51, 0
  %t5599 = or i64 %t5597, %t5598
  %t5600 = add i64 37, 0
  %t5601 = or i64 %t5599, %t5600
  %t5602 = add i64 65535, 0
  %t5603 = and i64 %t5601, %t5602
  store i64 %t5603, ptr %t5588
  %t5604 = load i64, ptr %t5588
  %t5605 = add i64 56, 0
  %t5606 = or i64 %t5604, %t5605
  %t5607 = add i64 9, 0
  %t5608 = or i64 %t5606, %t5607
  %t5609 = add i64 65535, 0
  %t5610 = and i64 %t5608, %t5609
  store i64 %t5610, ptr %t5588
  %t5611 = load i64, ptr %t5588
  %t5612 = add i64 41, 0
  %t5613 = add i64 %t5611, %t5612
  %t5614 = add i64 74, 0
  %t5615 = add i64 %t5613, %t5614
  %t5616 = add i64 65535, 0
  %t5617 = and i64 %t5615, %t5616
  store i64 %t5617, ptr %t5588
  %t5618 = load i64, ptr %t5588
  %t5619 = add i64 38, 0
  %t5620 = sub i64 %t5618, %t5619
  %t5621 = add i64 23, 0
  %t5622 = sub i64 %t5620, %t5621
  %t5623 = add i64 65535, 0
  %t5624 = and i64 %t5622, %t5623
  store i64 %t5624, ptr %t5588
  %t5625 = load i64, ptr %t5588
  %t5626 = call %NxVal @nx_int(i64 %t5625)
  ret %NxVal %t5626
}
define %NxVal @nx__m_3____main____Cell__m79(%NxVal* %args, i64 %nargs) {
entry:
  %t5627 = alloca %NxVal
  %t5631 = alloca i64
  %t5646 = alloca i64
  store %NxVal zeroinitializer, ptr %t5627
  %t5628 = getelementptr %NxVal, ptr %args, i64 0
  %t5629 = load %NxVal, ptr %t5628
  %t5630 = call %NxVal @nx_clone(%NxVal %t5629)
  store %NxVal %t5630, ptr %t5627
  %t5632 = getelementptr %NxVal, ptr %args, i64 1
  %t5633 = load %NxVal, ptr %t5632
  %t5634 = extractvalue %NxVal %t5633, 1
  store i64 %t5634, ptr %t5631
  %t5635 = load %NxVal, ptr %t5627
  %t5636 = call %NxVal @nx_rec_get(%NxVal %t5635, i64 0)
  %t5637 = load %NxVal, ptr %t5627
  %t5638 = call %NxVal @nx_rec_get(%NxVal %t5637, i64 1)
  %t5639 = call %NxVal @nx_add(%NxVal %t5636, %NxVal %t5638)
  %t5640 = load i64, ptr %t5631
  %t5641 = call %NxVal @nx_int(i64 %t5640)
  %t5642 = call %NxVal @nx_add(%NxVal %t5639, %NxVal %t5641)
  %t5643 = add i64 65535, 0
  %t5644 = call %NxVal @nx_int(i64 %t5643)
  %t5645 = call %NxVal @nx_bitand(%NxVal %t5642, %NxVal %t5644)
  %t5647 = extractvalue %NxVal %t5645, 1
  store i64 %t5647, ptr %t5646
  %t5648 = load i64, ptr %t5646
  %t5649 = add i64 5, 0
  %t5650 = add i64 %t5648, %t5649
  %t5651 = add i64 13, 0
  %t5652 = add i64 %t5650, %t5651
  %t5653 = add i64 65535, 0
  %t5654 = and i64 %t5652, %t5653
  store i64 %t5654, ptr %t5646
  %t5655 = load i64, ptr %t5646
  %t5656 = add i64 5, 0
  %t5657 = call i64 @nx_mod_i64(i64 %t5655, i64 %t5656)
  %t5658 = add i64 48, 0
  %t5659 = call i64 @nx_mod_i64(i64 %t5657, i64 %t5658)
  %t5660 = add i64 65535, 0
  %t5661 = and i64 %t5659, %t5660
  store i64 %t5661, ptr %t5646
  %t5662 = load i64, ptr %t5646
  %t5663 = add i64 77, 0
  %t5664 = mul i64 %t5662, %t5663
  %t5665 = add i64 8, 0
  %t5666 = mul i64 %t5664, %t5665
  %t5667 = add i64 65535, 0
  %t5668 = and i64 %t5666, %t5667
  store i64 %t5668, ptr %t5646
  %t5669 = load i64, ptr %t5646
  %t5670 = add i64 94, 0
  %t5671 = sub i64 %t5669, %t5670
  %t5672 = add i64 28, 0
  %t5673 = sub i64 %t5671, %t5672
  %t5674 = add i64 65535, 0
  %t5675 = and i64 %t5673, %t5674
  store i64 %t5675, ptr %t5646
  %t5676 = load i64, ptr %t5646
  %t5677 = add i64 28, 0
  %t5678 = and i64 %t5676, %t5677
  %t5679 = add i64 68, 0
  %t5680 = and i64 %t5678, %t5679
  %t5681 = add i64 65535, 0
  %t5682 = and i64 %t5680, %t5681
  store i64 %t5682, ptr %t5646
  %t5683 = load i64, ptr %t5646
  %t5684 = call %NxVal @nx_int(i64 %t5683)
  ret %NxVal %t5684
}
define %NxVal @nx__m_2____main____Cell__m8(%NxVal* %args, i64 %nargs) {
entry:
  %t5685 = alloca %NxVal
  %t5689 = alloca i64
  %t5704 = alloca i64
  store %NxVal zeroinitializer, ptr %t5685
  %t5686 = getelementptr %NxVal, ptr %args, i64 0
  %t5687 = load %NxVal, ptr %t5686
  %t5688 = call %NxVal @nx_clone(%NxVal %t5687)
  store %NxVal %t5688, ptr %t5685
  %t5690 = getelementptr %NxVal, ptr %args, i64 1
  %t5691 = load %NxVal, ptr %t5690
  %t5692 = extractvalue %NxVal %t5691, 1
  store i64 %t5692, ptr %t5689
  %t5693 = load %NxVal, ptr %t5685
  %t5694 = call %NxVal @nx_rec_get(%NxVal %t5693, i64 0)
  %t5695 = load %NxVal, ptr %t5685
  %t5696 = call %NxVal @nx_rec_get(%NxVal %t5695, i64 1)
  %t5697 = call %NxVal @nx_add(%NxVal %t5694, %NxVal %t5696)
  %t5698 = load i64, ptr %t5689
  %t5699 = call %NxVal @nx_int(i64 %t5698)
  %t5700 = call %NxVal @nx_add(%NxVal %t5697, %NxVal %t5699)
  %t5701 = add i64 65535, 0
  %t5702 = call %NxVal @nx_int(i64 %t5701)
  %t5703 = call %NxVal @nx_bitand(%NxVal %t5700, %NxVal %t5702)
  %t5705 = extractvalue %NxVal %t5703, 1
  store i64 %t5705, ptr %t5704
  %t5706 = load i64, ptr %t5704
  %t5707 = add i64 45, 0
  %t5708 = xor i64 %t5706, %t5707
  %t5709 = add i64 8, 0
  %t5710 = xor i64 %t5708, %t5709
  %t5711 = add i64 65535, 0
  %t5712 = and i64 %t5710, %t5711
  store i64 %t5712, ptr %t5704
  %t5713 = load i64, ptr %t5704
  %t5714 = add i64 94, 0
  %t5715 = xor i64 %t5713, %t5714
  %t5716 = add i64 70, 0
  %t5717 = xor i64 %t5715, %t5716
  %t5718 = add i64 65535, 0
  %t5719 = and i64 %t5717, %t5718
  store i64 %t5719, ptr %t5704
  %t5720 = load i64, ptr %t5704
  %t5721 = add i64 66, 0
  %t5722 = or i64 %t5720, %t5721
  %t5723 = add i64 52, 0
  %t5724 = or i64 %t5722, %t5723
  %t5725 = add i64 65535, 0
  %t5726 = and i64 %t5724, %t5725
  store i64 %t5726, ptr %t5704
  %t5727 = load i64, ptr %t5704
  %t5728 = add i64 44, 0
  %t5729 = xor i64 %t5727, %t5728
  %t5730 = add i64 35, 0
  %t5731 = xor i64 %t5729, %t5730
  %t5732 = add i64 65535, 0
  %t5733 = and i64 %t5731, %t5732
  store i64 %t5733, ptr %t5704
  %t5734 = load i64, ptr %t5704
  %t5735 = add i64 83, 0
  %t5736 = xor i64 %t5734, %t5735
  %t5737 = add i64 87, 0
  %t5738 = xor i64 %t5736, %t5737
  %t5739 = add i64 65535, 0
  %t5740 = and i64 %t5738, %t5739
  store i64 %t5740, ptr %t5704
  %t5741 = load i64, ptr %t5704
  %t5742 = call %NxVal @nx_int(i64 %t5741)
  ret %NxVal %t5742
}
define %NxVal @nx__m_3____main____Cell__m80(%NxVal* %args, i64 %nargs) {
entry:
  %t5743 = alloca %NxVal
  %t5747 = alloca i64
  %t5762 = alloca i64
  store %NxVal zeroinitializer, ptr %t5743
  %t5744 = getelementptr %NxVal, ptr %args, i64 0
  %t5745 = load %NxVal, ptr %t5744
  %t5746 = call %NxVal @nx_clone(%NxVal %t5745)
  store %NxVal %t5746, ptr %t5743
  %t5748 = getelementptr %NxVal, ptr %args, i64 1
  %t5749 = load %NxVal, ptr %t5748
  %t5750 = extractvalue %NxVal %t5749, 1
  store i64 %t5750, ptr %t5747
  %t5751 = load %NxVal, ptr %t5743
  %t5752 = call %NxVal @nx_rec_get(%NxVal %t5751, i64 0)
  %t5753 = load %NxVal, ptr %t5743
  %t5754 = call %NxVal @nx_rec_get(%NxVal %t5753, i64 1)
  %t5755 = call %NxVal @nx_add(%NxVal %t5752, %NxVal %t5754)
  %t5756 = load i64, ptr %t5747
  %t5757 = call %NxVal @nx_int(i64 %t5756)
  %t5758 = call %NxVal @nx_add(%NxVal %t5755, %NxVal %t5757)
  %t5759 = add i64 65535, 0
  %t5760 = call %NxVal @nx_int(i64 %t5759)
  %t5761 = call %NxVal @nx_bitand(%NxVal %t5758, %NxVal %t5760)
  %t5763 = extractvalue %NxVal %t5761, 1
  store i64 %t5763, ptr %t5762
  %t5764 = load i64, ptr %t5762
  %t5765 = add i64 66, 0
  %t5766 = xor i64 %t5764, %t5765
  %t5767 = add i64 63, 0
  %t5768 = xor i64 %t5766, %t5767
  %t5769 = add i64 65535, 0
  %t5770 = and i64 %t5768, %t5769
  store i64 %t5770, ptr %t5762
  %t5771 = load i64, ptr %t5762
  %t5772 = add i64 3, 0
  %t5773 = call i64 @nx_mod_i64(i64 %t5771, i64 %t5772)
  %t5774 = add i64 17, 0
  %t5775 = call i64 @nx_mod_i64(i64 %t5773, i64 %t5774)
  %t5776 = add i64 65535, 0
  %t5777 = and i64 %t5775, %t5776
  store i64 %t5777, ptr %t5762
  %t5778 = load i64, ptr %t5762
  %t5779 = add i64 72, 0
  %t5780 = or i64 %t5778, %t5779
  %t5781 = add i64 81, 0
  %t5782 = or i64 %t5780, %t5781
  %t5783 = add i64 65535, 0
  %t5784 = and i64 %t5782, %t5783
  store i64 %t5784, ptr %t5762
  %t5785 = load i64, ptr %t5762
  %t5786 = add i64 11, 0
  %t5787 = add i64 %t5785, %t5786
  %t5788 = add i64 37, 0
  %t5789 = add i64 %t5787, %t5788
  %t5790 = add i64 65535, 0
  %t5791 = and i64 %t5789, %t5790
  store i64 %t5791, ptr %t5762
  %t5792 = load i64, ptr %t5762
  %t5793 = add i64 17, 0
  %t5794 = and i64 %t5792, %t5793
  %t5795 = add i64 81, 0
  %t5796 = and i64 %t5794, %t5795
  %t5797 = add i64 65535, 0
  %t5798 = and i64 %t5796, %t5797
  store i64 %t5798, ptr %t5762
  %t5799 = load i64, ptr %t5762
  %t5800 = call %NxVal @nx_int(i64 %t5799)
  ret %NxVal %t5800
}
define %NxVal @nx__m_3____main____Cell__m81(%NxVal* %args, i64 %nargs) {
entry:
  %t5801 = alloca %NxVal
  %t5805 = alloca i64
  %t5820 = alloca i64
  store %NxVal zeroinitializer, ptr %t5801
  %t5802 = getelementptr %NxVal, ptr %args, i64 0
  %t5803 = load %NxVal, ptr %t5802
  %t5804 = call %NxVal @nx_clone(%NxVal %t5803)
  store %NxVal %t5804, ptr %t5801
  %t5806 = getelementptr %NxVal, ptr %args, i64 1
  %t5807 = load %NxVal, ptr %t5806
  %t5808 = extractvalue %NxVal %t5807, 1
  store i64 %t5808, ptr %t5805
  %t5809 = load %NxVal, ptr %t5801
  %t5810 = call %NxVal @nx_rec_get(%NxVal %t5809, i64 0)
  %t5811 = load %NxVal, ptr %t5801
  %t5812 = call %NxVal @nx_rec_get(%NxVal %t5811, i64 1)
  %t5813 = call %NxVal @nx_add(%NxVal %t5810, %NxVal %t5812)
  %t5814 = load i64, ptr %t5805
  %t5815 = call %NxVal @nx_int(i64 %t5814)
  %t5816 = call %NxVal @nx_add(%NxVal %t5813, %NxVal %t5815)
  %t5817 = add i64 65535, 0
  %t5818 = call %NxVal @nx_int(i64 %t5817)
  %t5819 = call %NxVal @nx_bitand(%NxVal %t5816, %NxVal %t5818)
  %t5821 = extractvalue %NxVal %t5819, 1
  store i64 %t5821, ptr %t5820
  %t5822 = load i64, ptr %t5820
  %t5823 = add i64 66, 0
  %t5824 = xor i64 %t5822, %t5823
  %t5825 = add i64 26, 0
  %t5826 = xor i64 %t5824, %t5825
  %t5827 = add i64 65535, 0
  %t5828 = and i64 %t5826, %t5827
  store i64 %t5828, ptr %t5820
  %t5829 = load i64, ptr %t5820
  %t5830 = add i64 24, 0
  %t5831 = xor i64 %t5829, %t5830
  %t5832 = add i64 62, 0
  %t5833 = xor i64 %t5831, %t5832
  %t5834 = add i64 65535, 0
  %t5835 = and i64 %t5833, %t5834
  store i64 %t5835, ptr %t5820
  %t5836 = load i64, ptr %t5820
  %t5837 = add i64 64, 0
  %t5838 = sub i64 %t5836, %t5837
  %t5839 = add i64 65, 0
  %t5840 = sub i64 %t5838, %t5839
  %t5841 = add i64 65535, 0
  %t5842 = and i64 %t5840, %t5841
  store i64 %t5842, ptr %t5820
  %t5843 = load i64, ptr %t5820
  %t5844 = add i64 43, 0
  %t5845 = and i64 %t5843, %t5844
  %t5846 = add i64 27, 0
  %t5847 = and i64 %t5845, %t5846
  %t5848 = add i64 65535, 0
  %t5849 = and i64 %t5847, %t5848
  store i64 %t5849, ptr %t5820
  %t5850 = load i64, ptr %t5820
  %t5851 = add i64 44, 0
  %t5852 = sub i64 %t5850, %t5851
  %t5853 = add i64 31, 0
  %t5854 = sub i64 %t5852, %t5853
  %t5855 = add i64 65535, 0
  %t5856 = and i64 %t5854, %t5855
  store i64 %t5856, ptr %t5820
  %t5857 = load i64, ptr %t5820
  %t5858 = call %NxVal @nx_int(i64 %t5857)
  ret %NxVal %t5858
}
define %NxVal @nx__m_3____main____Cell__m82(%NxVal* %args, i64 %nargs) {
entry:
  %t5859 = alloca %NxVal
  %t5863 = alloca i64
  %t5878 = alloca i64
  store %NxVal zeroinitializer, ptr %t5859
  %t5860 = getelementptr %NxVal, ptr %args, i64 0
  %t5861 = load %NxVal, ptr %t5860
  %t5862 = call %NxVal @nx_clone(%NxVal %t5861)
  store %NxVal %t5862, ptr %t5859
  %t5864 = getelementptr %NxVal, ptr %args, i64 1
  %t5865 = load %NxVal, ptr %t5864
  %t5866 = extractvalue %NxVal %t5865, 1
  store i64 %t5866, ptr %t5863
  %t5867 = load %NxVal, ptr %t5859
  %t5868 = call %NxVal @nx_rec_get(%NxVal %t5867, i64 0)
  %t5869 = load %NxVal, ptr %t5859
  %t5870 = call %NxVal @nx_rec_get(%NxVal %t5869, i64 1)
  %t5871 = call %NxVal @nx_add(%NxVal %t5868, %NxVal %t5870)
  %t5872 = load i64, ptr %t5863
  %t5873 = call %NxVal @nx_int(i64 %t5872)
  %t5874 = call %NxVal @nx_add(%NxVal %t5871, %NxVal %t5873)
  %t5875 = add i64 65535, 0
  %t5876 = call %NxVal @nx_int(i64 %t5875)
  %t5877 = call %NxVal @nx_bitand(%NxVal %t5874, %NxVal %t5876)
  %t5879 = extractvalue %NxVal %t5877, 1
  store i64 %t5879, ptr %t5878
  %t5880 = load i64, ptr %t5878
  %t5881 = add i64 96, 0
  %t5882 = sub i64 %t5880, %t5881
  %t5883 = add i64 31, 0
  %t5884 = sub i64 %t5882, %t5883
  %t5885 = add i64 65535, 0
  %t5886 = and i64 %t5884, %t5885
  store i64 %t5886, ptr %t5878
  %t5887 = load i64, ptr %t5878
  %t5888 = add i64 79, 0
  %t5889 = add i64 %t5887, %t5888
  %t5890 = add i64 27, 0
  %t5891 = add i64 %t5889, %t5890
  %t5892 = add i64 65535, 0
  %t5893 = and i64 %t5891, %t5892
  store i64 %t5893, ptr %t5878
  %t5894 = load i64, ptr %t5878
  %t5895 = add i64 58, 0
  %t5896 = mul i64 %t5894, %t5895
  %t5897 = add i64 38, 0
  %t5898 = mul i64 %t5896, %t5897
  %t5899 = add i64 65535, 0
  %t5900 = and i64 %t5898, %t5899
  store i64 %t5900, ptr %t5878
  %t5901 = load i64, ptr %t5878
  %t5902 = add i64 84, 0
  %t5903 = sub i64 %t5901, %t5902
  %t5904 = add i64 62, 0
  %t5905 = sub i64 %t5903, %t5904
  %t5906 = add i64 65535, 0
  %t5907 = and i64 %t5905, %t5906
  store i64 %t5907, ptr %t5878
  %t5908 = load i64, ptr %t5878
  %t5909 = add i64 29, 0
  %t5910 = call i64 @nx_mod_i64(i64 %t5908, i64 %t5909)
  %t5911 = add i64 52, 0
  %t5912 = call i64 @nx_mod_i64(i64 %t5910, i64 %t5911)
  %t5913 = add i64 65535, 0
  %t5914 = and i64 %t5912, %t5913
  store i64 %t5914, ptr %t5878
  %t5915 = load i64, ptr %t5878
  %t5916 = call %NxVal @nx_int(i64 %t5915)
  ret %NxVal %t5916
}
define %NxVal @nx__m_3____main____Cell__m83(%NxVal* %args, i64 %nargs) {
entry:
  %t5917 = alloca %NxVal
  %t5921 = alloca i64
  %t5936 = alloca i64
  store %NxVal zeroinitializer, ptr %t5917
  %t5918 = getelementptr %NxVal, ptr %args, i64 0
  %t5919 = load %NxVal, ptr %t5918
  %t5920 = call %NxVal @nx_clone(%NxVal %t5919)
  store %NxVal %t5920, ptr %t5917
  %t5922 = getelementptr %NxVal, ptr %args, i64 1
  %t5923 = load %NxVal, ptr %t5922
  %t5924 = extractvalue %NxVal %t5923, 1
  store i64 %t5924, ptr %t5921
  %t5925 = load %NxVal, ptr %t5917
  %t5926 = call %NxVal @nx_rec_get(%NxVal %t5925, i64 0)
  %t5927 = load %NxVal, ptr %t5917
  %t5928 = call %NxVal @nx_rec_get(%NxVal %t5927, i64 1)
  %t5929 = call %NxVal @nx_add(%NxVal %t5926, %NxVal %t5928)
  %t5930 = load i64, ptr %t5921
  %t5931 = call %NxVal @nx_int(i64 %t5930)
  %t5932 = call %NxVal @nx_add(%NxVal %t5929, %NxVal %t5931)
  %t5933 = add i64 65535, 0
  %t5934 = call %NxVal @nx_int(i64 %t5933)
  %t5935 = call %NxVal @nx_bitand(%NxVal %t5932, %NxVal %t5934)
  %t5937 = extractvalue %NxVal %t5935, 1
  store i64 %t5937, ptr %t5936
  %t5938 = load i64, ptr %t5936
  %t5939 = add i64 33, 0
  %t5940 = call i64 @nx_mod_i64(i64 %t5938, i64 %t5939)
  %t5941 = add i64 27, 0
  %t5942 = call i64 @nx_mod_i64(i64 %t5940, i64 %t5941)
  %t5943 = add i64 65535, 0
  %t5944 = and i64 %t5942, %t5943
  store i64 %t5944, ptr %t5936
  %t5945 = load i64, ptr %t5936
  %t5946 = add i64 84, 0
  %t5947 = add i64 %t5945, %t5946
  %t5948 = add i64 73, 0
  %t5949 = add i64 %t5947, %t5948
  %t5950 = add i64 65535, 0
  %t5951 = and i64 %t5949, %t5950
  store i64 %t5951, ptr %t5936
  %t5952 = load i64, ptr %t5936
  %t5953 = add i64 81, 0
  %t5954 = mul i64 %t5952, %t5953
  %t5955 = add i64 37, 0
  %t5956 = mul i64 %t5954, %t5955
  %t5957 = add i64 65535, 0
  %t5958 = and i64 %t5956, %t5957
  store i64 %t5958, ptr %t5936
  %t5959 = load i64, ptr %t5936
  %t5960 = add i64 63, 0
  %t5961 = call i64 @nx_mod_i64(i64 %t5959, i64 %t5960)
  %t5962 = add i64 10, 0
  %t5963 = call i64 @nx_mod_i64(i64 %t5961, i64 %t5962)
  %t5964 = add i64 65535, 0
  %t5965 = and i64 %t5963, %t5964
  store i64 %t5965, ptr %t5936
  %t5966 = load i64, ptr %t5936
  %t5967 = add i64 65, 0
  %t5968 = or i64 %t5966, %t5967
  %t5969 = add i64 48, 0
  %t5970 = or i64 %t5968, %t5969
  %t5971 = add i64 65535, 0
  %t5972 = and i64 %t5970, %t5971
  store i64 %t5972, ptr %t5936
  %t5973 = load i64, ptr %t5936
  %t5974 = call %NxVal @nx_int(i64 %t5973)
  ret %NxVal %t5974
}
define %NxVal @nx__m_3____main____Cell__m84(%NxVal* %args, i64 %nargs) {
entry:
  %t5975 = alloca %NxVal
  %t5979 = alloca i64
  %t5994 = alloca i64
  store %NxVal zeroinitializer, ptr %t5975
  %t5976 = getelementptr %NxVal, ptr %args, i64 0
  %t5977 = load %NxVal, ptr %t5976
  %t5978 = call %NxVal @nx_clone(%NxVal %t5977)
  store %NxVal %t5978, ptr %t5975
  %t5980 = getelementptr %NxVal, ptr %args, i64 1
  %t5981 = load %NxVal, ptr %t5980
  %t5982 = extractvalue %NxVal %t5981, 1
  store i64 %t5982, ptr %t5979
  %t5983 = load %NxVal, ptr %t5975
  %t5984 = call %NxVal @nx_rec_get(%NxVal %t5983, i64 0)
  %t5985 = load %NxVal, ptr %t5975
  %t5986 = call %NxVal @nx_rec_get(%NxVal %t5985, i64 1)
  %t5987 = call %NxVal @nx_add(%NxVal %t5984, %NxVal %t5986)
  %t5988 = load i64, ptr %t5979
  %t5989 = call %NxVal @nx_int(i64 %t5988)
  %t5990 = call %NxVal @nx_add(%NxVal %t5987, %NxVal %t5989)
  %t5991 = add i64 65535, 0
  %t5992 = call %NxVal @nx_int(i64 %t5991)
  %t5993 = call %NxVal @nx_bitand(%NxVal %t5990, %NxVal %t5992)
  %t5995 = extractvalue %NxVal %t5993, 1
  store i64 %t5995, ptr %t5994
  %t5996 = load i64, ptr %t5994
  %t5997 = add i64 65, 0
  %t5998 = or i64 %t5996, %t5997
  %t5999 = add i64 12, 0
  %t6000 = or i64 %t5998, %t5999
  %t6001 = add i64 65535, 0
  %t6002 = and i64 %t6000, %t6001
  store i64 %t6002, ptr %t5994
  %t6003 = load i64, ptr %t5994
  %t6004 = add i64 37, 0
  %t6005 = mul i64 %t6003, %t6004
  %t6006 = add i64 26, 0
  %t6007 = mul i64 %t6005, %t6006
  %t6008 = add i64 65535, 0
  %t6009 = and i64 %t6007, %t6008
  store i64 %t6009, ptr %t5994
  %t6010 = load i64, ptr %t5994
  %t6011 = add i64 66, 0
  %t6012 = and i64 %t6010, %t6011
  %t6013 = add i64 55, 0
  %t6014 = and i64 %t6012, %t6013
  %t6015 = add i64 65535, 0
  %t6016 = and i64 %t6014, %t6015
  store i64 %t6016, ptr %t5994
  %t6017 = load i64, ptr %t5994
  %t6018 = add i64 73, 0
  %t6019 = xor i64 %t6017, %t6018
  %t6020 = add i64 40, 0
  %t6021 = xor i64 %t6019, %t6020
  %t6022 = add i64 65535, 0
  %t6023 = and i64 %t6021, %t6022
  store i64 %t6023, ptr %t5994
  %t6024 = load i64, ptr %t5994
  %t6025 = add i64 61, 0
  %t6026 = or i64 %t6024, %t6025
  %t6027 = add i64 60, 0
  %t6028 = or i64 %t6026, %t6027
  %t6029 = add i64 65535, 0
  %t6030 = and i64 %t6028, %t6029
  store i64 %t6030, ptr %t5994
  %t6031 = load i64, ptr %t5994
  %t6032 = call %NxVal @nx_int(i64 %t6031)
  ret %NxVal %t6032
}
define %NxVal @nx__m_3____main____Cell__m85(%NxVal* %args, i64 %nargs) {
entry:
  %t6033 = alloca %NxVal
  %t6037 = alloca i64
  %t6052 = alloca i64
  store %NxVal zeroinitializer, ptr %t6033
  %t6034 = getelementptr %NxVal, ptr %args, i64 0
  %t6035 = load %NxVal, ptr %t6034
  %t6036 = call %NxVal @nx_clone(%NxVal %t6035)
  store %NxVal %t6036, ptr %t6033
  %t6038 = getelementptr %NxVal, ptr %args, i64 1
  %t6039 = load %NxVal, ptr %t6038
  %t6040 = extractvalue %NxVal %t6039, 1
  store i64 %t6040, ptr %t6037
  %t6041 = load %NxVal, ptr %t6033
  %t6042 = call %NxVal @nx_rec_get(%NxVal %t6041, i64 0)
  %t6043 = load %NxVal, ptr %t6033
  %t6044 = call %NxVal @nx_rec_get(%NxVal %t6043, i64 1)
  %t6045 = call %NxVal @nx_add(%NxVal %t6042, %NxVal %t6044)
  %t6046 = load i64, ptr %t6037
  %t6047 = call %NxVal @nx_int(i64 %t6046)
  %t6048 = call %NxVal @nx_add(%NxVal %t6045, %NxVal %t6047)
  %t6049 = add i64 65535, 0
  %t6050 = call %NxVal @nx_int(i64 %t6049)
  %t6051 = call %NxVal @nx_bitand(%NxVal %t6048, %NxVal %t6050)
  %t6053 = extractvalue %NxVal %t6051, 1
  store i64 %t6053, ptr %t6052
  %t6054 = load i64, ptr %t6052
  %t6055 = add i64 94, 0
  %t6056 = sub i64 %t6054, %t6055
  %t6057 = add i64 26, 0
  %t6058 = sub i64 %t6056, %t6057
  %t6059 = add i64 65535, 0
  %t6060 = and i64 %t6058, %t6059
  store i64 %t6060, ptr %t6052
  %t6061 = load i64, ptr %t6052
  %t6062 = add i64 33, 0
  %t6063 = or i64 %t6061, %t6062
  %t6064 = add i64 82, 0
  %t6065 = or i64 %t6063, %t6064
  %t6066 = add i64 65535, 0
  %t6067 = and i64 %t6065, %t6066
  store i64 %t6067, ptr %t6052
  %t6068 = load i64, ptr %t6052
  %t6069 = add i64 37, 0
  %t6070 = or i64 %t6068, %t6069
  %t6071 = add i64 47, 0
  %t6072 = or i64 %t6070, %t6071
  %t6073 = add i64 65535, 0
  %t6074 = and i64 %t6072, %t6073
  store i64 %t6074, ptr %t6052
  %t6075 = load i64, ptr %t6052
  %t6076 = add i64 74, 0
  %t6077 = add i64 %t6075, %t6076
  %t6078 = add i64 79, 0
  %t6079 = add i64 %t6077, %t6078
  %t6080 = add i64 65535, 0
  %t6081 = and i64 %t6079, %t6080
  store i64 %t6081, ptr %t6052
  %t6082 = load i64, ptr %t6052
  %t6083 = add i64 3, 0
  %t6084 = add i64 %t6082, %t6083
  %t6085 = add i64 31, 0
  %t6086 = add i64 %t6084, %t6085
  %t6087 = add i64 65535, 0
  %t6088 = and i64 %t6086, %t6087
  store i64 %t6088, ptr %t6052
  %t6089 = load i64, ptr %t6052
  %t6090 = call %NxVal @nx_int(i64 %t6089)
  ret %NxVal %t6090
}
define %NxVal @nx__m_3____main____Cell__m86(%NxVal* %args, i64 %nargs) {
entry:
  %t6091 = alloca %NxVal
  %t6095 = alloca i64
  %t6110 = alloca i64
  store %NxVal zeroinitializer, ptr %t6091
  %t6092 = getelementptr %NxVal, ptr %args, i64 0
  %t6093 = load %NxVal, ptr %t6092
  %t6094 = call %NxVal @nx_clone(%NxVal %t6093)
  store %NxVal %t6094, ptr %t6091
  %t6096 = getelementptr %NxVal, ptr %args, i64 1
  %t6097 = load %NxVal, ptr %t6096
  %t6098 = extractvalue %NxVal %t6097, 1
  store i64 %t6098, ptr %t6095
  %t6099 = load %NxVal, ptr %t6091
  %t6100 = call %NxVal @nx_rec_get(%NxVal %t6099, i64 0)
  %t6101 = load %NxVal, ptr %t6091
  %t6102 = call %NxVal @nx_rec_get(%NxVal %t6101, i64 1)
  %t6103 = call %NxVal @nx_add(%NxVal %t6100, %NxVal %t6102)
  %t6104 = load i64, ptr %t6095
  %t6105 = call %NxVal @nx_int(i64 %t6104)
  %t6106 = call %NxVal @nx_add(%NxVal %t6103, %NxVal %t6105)
  %t6107 = add i64 65535, 0
  %t6108 = call %NxVal @nx_int(i64 %t6107)
  %t6109 = call %NxVal @nx_bitand(%NxVal %t6106, %NxVal %t6108)
  %t6111 = extractvalue %NxVal %t6109, 1
  store i64 %t6111, ptr %t6110
  %t6112 = load i64, ptr %t6110
  %t6113 = add i64 69, 0
  %t6114 = and i64 %t6112, %t6113
  %t6115 = add i64 86, 0
  %t6116 = and i64 %t6114, %t6115
  %t6117 = add i64 65535, 0
  %t6118 = and i64 %t6116, %t6117
  store i64 %t6118, ptr %t6110
  %t6119 = load i64, ptr %t6110
  %t6120 = add i64 6, 0
  %t6121 = sub i64 %t6119, %t6120
  %t6122 = add i64 44, 0
  %t6123 = sub i64 %t6121, %t6122
  %t6124 = add i64 65535, 0
  %t6125 = and i64 %t6123, %t6124
  store i64 %t6125, ptr %t6110
  %t6126 = load i64, ptr %t6110
  %t6127 = add i64 11, 0
  %t6128 = add i64 %t6126, %t6127
  %t6129 = add i64 50, 0
  %t6130 = add i64 %t6128, %t6129
  %t6131 = add i64 65535, 0
  %t6132 = and i64 %t6130, %t6131
  store i64 %t6132, ptr %t6110
  %t6133 = load i64, ptr %t6110
  %t6134 = add i64 75, 0
  %t6135 = xor i64 %t6133, %t6134
  %t6136 = add i64 66, 0
  %t6137 = xor i64 %t6135, %t6136
  %t6138 = add i64 65535, 0
  %t6139 = and i64 %t6137, %t6138
  store i64 %t6139, ptr %t6110
  %t6140 = load i64, ptr %t6110
  %t6141 = add i64 5, 0
  %t6142 = call i64 @nx_mod_i64(i64 %t6140, i64 %t6141)
  %t6143 = add i64 6, 0
  %t6144 = call i64 @nx_mod_i64(i64 %t6142, i64 %t6143)
  %t6145 = add i64 65535, 0
  %t6146 = and i64 %t6144, %t6145
  store i64 %t6146, ptr %t6110
  %t6147 = load i64, ptr %t6110
  %t6148 = call %NxVal @nx_int(i64 %t6147)
  ret %NxVal %t6148
}
define %NxVal @nx__m_3____main____Cell__m87(%NxVal* %args, i64 %nargs) {
entry:
  %t6149 = alloca %NxVal
  %t6153 = alloca i64
  %t6168 = alloca i64
  store %NxVal zeroinitializer, ptr %t6149
  %t6150 = getelementptr %NxVal, ptr %args, i64 0
  %t6151 = load %NxVal, ptr %t6150
  %t6152 = call %NxVal @nx_clone(%NxVal %t6151)
  store %NxVal %t6152, ptr %t6149
  %t6154 = getelementptr %NxVal, ptr %args, i64 1
  %t6155 = load %NxVal, ptr %t6154
  %t6156 = extractvalue %NxVal %t6155, 1
  store i64 %t6156, ptr %t6153
  %t6157 = load %NxVal, ptr %t6149
  %t6158 = call %NxVal @nx_rec_get(%NxVal %t6157, i64 0)
  %t6159 = load %NxVal, ptr %t6149
  %t6160 = call %NxVal @nx_rec_get(%NxVal %t6159, i64 1)
  %t6161 = call %NxVal @nx_add(%NxVal %t6158, %NxVal %t6160)
  %t6162 = load i64, ptr %t6153
  %t6163 = call %NxVal @nx_int(i64 %t6162)
  %t6164 = call %NxVal @nx_add(%NxVal %t6161, %NxVal %t6163)
  %t6165 = add i64 65535, 0
  %t6166 = call %NxVal @nx_int(i64 %t6165)
  %t6167 = call %NxVal @nx_bitand(%NxVal %t6164, %NxVal %t6166)
  %t6169 = extractvalue %NxVal %t6167, 1
  store i64 %t6169, ptr %t6168
  %t6170 = load i64, ptr %t6168
  %t6171 = add i64 74, 0
  %t6172 = add i64 %t6170, %t6171
  %t6173 = add i64 52, 0
  %t6174 = add i64 %t6172, %t6173
  %t6175 = add i64 65535, 0
  %t6176 = and i64 %t6174, %t6175
  store i64 %t6176, ptr %t6168
  %t6177 = load i64, ptr %t6168
  %t6178 = add i64 60, 0
  %t6179 = add i64 %t6177, %t6178
  %t6180 = add i64 29, 0
  %t6181 = add i64 %t6179, %t6180
  %t6182 = add i64 65535, 0
  %t6183 = and i64 %t6181, %t6182
  store i64 %t6183, ptr %t6168
  %t6184 = load i64, ptr %t6168
  %t6185 = add i64 94, 0
  %t6186 = or i64 %t6184, %t6185
  %t6187 = add i64 71, 0
  %t6188 = or i64 %t6186, %t6187
  %t6189 = add i64 65535, 0
  %t6190 = and i64 %t6188, %t6189
  store i64 %t6190, ptr %t6168
  %t6191 = load i64, ptr %t6168
  %t6192 = add i64 41, 0
  %t6193 = and i64 %t6191, %t6192
  %t6194 = add i64 22, 0
  %t6195 = and i64 %t6193, %t6194
  %t6196 = add i64 65535, 0
  %t6197 = and i64 %t6195, %t6196
  store i64 %t6197, ptr %t6168
  %t6198 = load i64, ptr %t6168
  %t6199 = add i64 54, 0
  %t6200 = sub i64 %t6198, %t6199
  %t6201 = add i64 74, 0
  %t6202 = sub i64 %t6200, %t6201
  %t6203 = add i64 65535, 0
  %t6204 = and i64 %t6202, %t6203
  store i64 %t6204, ptr %t6168
  %t6205 = load i64, ptr %t6168
  %t6206 = call %NxVal @nx_int(i64 %t6205)
  ret %NxVal %t6206
}
define %NxVal @nx__m_3____main____Cell__m88(%NxVal* %args, i64 %nargs) {
entry:
  %t6207 = alloca %NxVal
  %t6211 = alloca i64
  %t6226 = alloca i64
  store %NxVal zeroinitializer, ptr %t6207
  %t6208 = getelementptr %NxVal, ptr %args, i64 0
  %t6209 = load %NxVal, ptr %t6208
  %t6210 = call %NxVal @nx_clone(%NxVal %t6209)
  store %NxVal %t6210, ptr %t6207
  %t6212 = getelementptr %NxVal, ptr %args, i64 1
  %t6213 = load %NxVal, ptr %t6212
  %t6214 = extractvalue %NxVal %t6213, 1
  store i64 %t6214, ptr %t6211
  %t6215 = load %NxVal, ptr %t6207
  %t6216 = call %NxVal @nx_rec_get(%NxVal %t6215, i64 0)
  %t6217 = load %NxVal, ptr %t6207
  %t6218 = call %NxVal @nx_rec_get(%NxVal %t6217, i64 1)
  %t6219 = call %NxVal @nx_add(%NxVal %t6216, %NxVal %t6218)
  %t6220 = load i64, ptr %t6211
  %t6221 = call %NxVal @nx_int(i64 %t6220)
  %t6222 = call %NxVal @nx_add(%NxVal %t6219, %NxVal %t6221)
  %t6223 = add i64 65535, 0
  %t6224 = call %NxVal @nx_int(i64 %t6223)
  %t6225 = call %NxVal @nx_bitand(%NxVal %t6222, %NxVal %t6224)
  %t6227 = extractvalue %NxVal %t6225, 1
  store i64 %t6227, ptr %t6226
  %t6228 = load i64, ptr %t6226
  %t6229 = add i64 97, 0
  %t6230 = xor i64 %t6228, %t6229
  %t6231 = add i64 39, 0
  %t6232 = xor i64 %t6230, %t6231
  %t6233 = add i64 65535, 0
  %t6234 = and i64 %t6232, %t6233
  store i64 %t6234, ptr %t6226
  %t6235 = load i64, ptr %t6226
  %t6236 = add i64 51, 0
  %t6237 = and i64 %t6235, %t6236
  %t6238 = add i64 51, 0
  %t6239 = and i64 %t6237, %t6238
  %t6240 = add i64 65535, 0
  %t6241 = and i64 %t6239, %t6240
  store i64 %t6241, ptr %t6226
  %t6242 = load i64, ptr %t6226
  %t6243 = add i64 20, 0
  %t6244 = and i64 %t6242, %t6243
  %t6245 = add i64 39, 0
  %t6246 = and i64 %t6244, %t6245
  %t6247 = add i64 65535, 0
  %t6248 = and i64 %t6246, %t6247
  store i64 %t6248, ptr %t6226
  %t6249 = load i64, ptr %t6226
  %t6250 = add i64 59, 0
  %t6251 = sub i64 %t6249, %t6250
  %t6252 = add i64 6, 0
  %t6253 = sub i64 %t6251, %t6252
  %t6254 = add i64 65535, 0
  %t6255 = and i64 %t6253, %t6254
  store i64 %t6255, ptr %t6226
  %t6256 = load i64, ptr %t6226
  %t6257 = add i64 79, 0
  %t6258 = and i64 %t6256, %t6257
  %t6259 = add i64 87, 0
  %t6260 = and i64 %t6258, %t6259
  %t6261 = add i64 65535, 0
  %t6262 = and i64 %t6260, %t6261
  store i64 %t6262, ptr %t6226
  %t6263 = load i64, ptr %t6226
  %t6264 = call %NxVal @nx_int(i64 %t6263)
  ret %NxVal %t6264
}
define %NxVal @nx__m_3____main____Cell__m89(%NxVal* %args, i64 %nargs) {
entry:
  %t6265 = alloca %NxVal
  %t6269 = alloca i64
  %t6284 = alloca i64
  store %NxVal zeroinitializer, ptr %t6265
  %t6266 = getelementptr %NxVal, ptr %args, i64 0
  %t6267 = load %NxVal, ptr %t6266
  %t6268 = call %NxVal @nx_clone(%NxVal %t6267)
  store %NxVal %t6268, ptr %t6265
  %t6270 = getelementptr %NxVal, ptr %args, i64 1
  %t6271 = load %NxVal, ptr %t6270
  %t6272 = extractvalue %NxVal %t6271, 1
  store i64 %t6272, ptr %t6269
  %t6273 = load %NxVal, ptr %t6265
  %t6274 = call %NxVal @nx_rec_get(%NxVal %t6273, i64 0)
  %t6275 = load %NxVal, ptr %t6265
  %t6276 = call %NxVal @nx_rec_get(%NxVal %t6275, i64 1)
  %t6277 = call %NxVal @nx_add(%NxVal %t6274, %NxVal %t6276)
  %t6278 = load i64, ptr %t6269
  %t6279 = call %NxVal @nx_int(i64 %t6278)
  %t6280 = call %NxVal @nx_add(%NxVal %t6277, %NxVal %t6279)
  %t6281 = add i64 65535, 0
  %t6282 = call %NxVal @nx_int(i64 %t6281)
  %t6283 = call %NxVal @nx_bitand(%NxVal %t6280, %NxVal %t6282)
  %t6285 = extractvalue %NxVal %t6283, 1
  store i64 %t6285, ptr %t6284
  %t6286 = load i64, ptr %t6284
  %t6287 = add i64 73, 0
  %t6288 = xor i64 %t6286, %t6287
  %t6289 = add i64 23, 0
  %t6290 = xor i64 %t6288, %t6289
  %t6291 = add i64 65535, 0
  %t6292 = and i64 %t6290, %t6291
  store i64 %t6292, ptr %t6284
  %t6293 = load i64, ptr %t6284
  %t6294 = add i64 10, 0
  %t6295 = or i64 %t6293, %t6294
  %t6296 = add i64 20, 0
  %t6297 = or i64 %t6295, %t6296
  %t6298 = add i64 65535, 0
  %t6299 = and i64 %t6297, %t6298
  store i64 %t6299, ptr %t6284
  %t6300 = load i64, ptr %t6284
  %t6301 = add i64 90, 0
  %t6302 = or i64 %t6300, %t6301
  %t6303 = add i64 75, 0
  %t6304 = or i64 %t6302, %t6303
  %t6305 = add i64 65535, 0
  %t6306 = and i64 %t6304, %t6305
  store i64 %t6306, ptr %t6284
  %t6307 = load i64, ptr %t6284
  %t6308 = add i64 60, 0
  %t6309 = mul i64 %t6307, %t6308
  %t6310 = add i64 74, 0
  %t6311 = mul i64 %t6309, %t6310
  %t6312 = add i64 65535, 0
  %t6313 = and i64 %t6311, %t6312
  store i64 %t6313, ptr %t6284
  %t6314 = load i64, ptr %t6284
  %t6315 = add i64 59, 0
  %t6316 = and i64 %t6314, %t6315
  %t6317 = add i64 11, 0
  %t6318 = and i64 %t6316, %t6317
  %t6319 = add i64 65535, 0
  %t6320 = and i64 %t6318, %t6319
  store i64 %t6320, ptr %t6284
  %t6321 = load i64, ptr %t6284
  %t6322 = call %NxVal @nx_int(i64 %t6321)
  ret %NxVal %t6322
}
define %NxVal @nx__m_2____main____Cell__m9(%NxVal* %args, i64 %nargs) {
entry:
  %t6323 = alloca %NxVal
  %t6327 = alloca i64
  %t6342 = alloca i64
  store %NxVal zeroinitializer, ptr %t6323
  %t6324 = getelementptr %NxVal, ptr %args, i64 0
  %t6325 = load %NxVal, ptr %t6324
  %t6326 = call %NxVal @nx_clone(%NxVal %t6325)
  store %NxVal %t6326, ptr %t6323
  %t6328 = getelementptr %NxVal, ptr %args, i64 1
  %t6329 = load %NxVal, ptr %t6328
  %t6330 = extractvalue %NxVal %t6329, 1
  store i64 %t6330, ptr %t6327
  %t6331 = load %NxVal, ptr %t6323
  %t6332 = call %NxVal @nx_rec_get(%NxVal %t6331, i64 0)
  %t6333 = load %NxVal, ptr %t6323
  %t6334 = call %NxVal @nx_rec_get(%NxVal %t6333, i64 1)
  %t6335 = call %NxVal @nx_add(%NxVal %t6332, %NxVal %t6334)
  %t6336 = load i64, ptr %t6327
  %t6337 = call %NxVal @nx_int(i64 %t6336)
  %t6338 = call %NxVal @nx_add(%NxVal %t6335, %NxVal %t6337)
  %t6339 = add i64 65535, 0
  %t6340 = call %NxVal @nx_int(i64 %t6339)
  %t6341 = call %NxVal @nx_bitand(%NxVal %t6338, %NxVal %t6340)
  %t6343 = extractvalue %NxVal %t6341, 1
  store i64 %t6343, ptr %t6342
  %t6344 = load i64, ptr %t6342
  %t6345 = add i64 93, 0
  %t6346 = and i64 %t6344, %t6345
  %t6347 = add i64 37, 0
  %t6348 = and i64 %t6346, %t6347
  %t6349 = add i64 65535, 0
  %t6350 = and i64 %t6348, %t6349
  store i64 %t6350, ptr %t6342
  %t6351 = load i64, ptr %t6342
  %t6352 = add i64 21, 0
  %t6353 = and i64 %t6351, %t6352
  %t6354 = add i64 3, 0
  %t6355 = and i64 %t6353, %t6354
  %t6356 = add i64 65535, 0
  %t6357 = and i64 %t6355, %t6356
  store i64 %t6357, ptr %t6342
  %t6358 = load i64, ptr %t6342
  %t6359 = add i64 79, 0
  %t6360 = sub i64 %t6358, %t6359
  %t6361 = add i64 64, 0
  %t6362 = sub i64 %t6360, %t6361
  %t6363 = add i64 65535, 0
  %t6364 = and i64 %t6362, %t6363
  store i64 %t6364, ptr %t6342
  %t6365 = load i64, ptr %t6342
  %t6366 = add i64 38, 0
  %t6367 = xor i64 %t6365, %t6366
  %t6368 = add i64 24, 0
  %t6369 = xor i64 %t6367, %t6368
  %t6370 = add i64 65535, 0
  %t6371 = and i64 %t6369, %t6370
  store i64 %t6371, ptr %t6342
  %t6372 = load i64, ptr %t6342
  %t6373 = add i64 13, 0
  %t6374 = xor i64 %t6372, %t6373
  %t6375 = add i64 32, 0
  %t6376 = xor i64 %t6374, %t6375
  %t6377 = add i64 65535, 0
  %t6378 = and i64 %t6376, %t6377
  store i64 %t6378, ptr %t6342
  %t6379 = load i64, ptr %t6342
  %t6380 = call %NxVal @nx_int(i64 %t6379)
  ret %NxVal %t6380
}
define %NxVal @nx__m_3____main____Cell__m90(%NxVal* %args, i64 %nargs) {
entry:
  %t6381 = alloca %NxVal
  %t6385 = alloca i64
  %t6400 = alloca i64
  store %NxVal zeroinitializer, ptr %t6381
  %t6382 = getelementptr %NxVal, ptr %args, i64 0
  %t6383 = load %NxVal, ptr %t6382
  %t6384 = call %NxVal @nx_clone(%NxVal %t6383)
  store %NxVal %t6384, ptr %t6381
  %t6386 = getelementptr %NxVal, ptr %args, i64 1
  %t6387 = load %NxVal, ptr %t6386
  %t6388 = extractvalue %NxVal %t6387, 1
  store i64 %t6388, ptr %t6385
  %t6389 = load %NxVal, ptr %t6381
  %t6390 = call %NxVal @nx_rec_get(%NxVal %t6389, i64 0)
  %t6391 = load %NxVal, ptr %t6381
  %t6392 = call %NxVal @nx_rec_get(%NxVal %t6391, i64 1)
  %t6393 = call %NxVal @nx_add(%NxVal %t6390, %NxVal %t6392)
  %t6394 = load i64, ptr %t6385
  %t6395 = call %NxVal @nx_int(i64 %t6394)
  %t6396 = call %NxVal @nx_add(%NxVal %t6393, %NxVal %t6395)
  %t6397 = add i64 65535, 0
  %t6398 = call %NxVal @nx_int(i64 %t6397)
  %t6399 = call %NxVal @nx_bitand(%NxVal %t6396, %NxVal %t6398)
  %t6401 = extractvalue %NxVal %t6399, 1
  store i64 %t6401, ptr %t6400
  %t6402 = load i64, ptr %t6400
  %t6403 = add i64 26, 0
  %t6404 = sub i64 %t6402, %t6403
  %t6405 = add i64 44, 0
  %t6406 = sub i64 %t6404, %t6405
  %t6407 = add i64 65535, 0
  %t6408 = and i64 %t6406, %t6407
  store i64 %t6408, ptr %t6400
  %t6409 = load i64, ptr %t6400
  %t6410 = add i64 41, 0
  %t6411 = xor i64 %t6409, %t6410
  %t6412 = add i64 4, 0
  %t6413 = xor i64 %t6411, %t6412
  %t6414 = add i64 65535, 0
  %t6415 = and i64 %t6413, %t6414
  store i64 %t6415, ptr %t6400
  %t6416 = load i64, ptr %t6400
  %t6417 = add i64 40, 0
  %t6418 = add i64 %t6416, %t6417
  %t6419 = add i64 53, 0
  %t6420 = add i64 %t6418, %t6419
  %t6421 = add i64 65535, 0
  %t6422 = and i64 %t6420, %t6421
  store i64 %t6422, ptr %t6400
  %t6423 = load i64, ptr %t6400
  %t6424 = add i64 74, 0
  %t6425 = and i64 %t6423, %t6424
  %t6426 = add i64 9, 0
  %t6427 = and i64 %t6425, %t6426
  %t6428 = add i64 65535, 0
  %t6429 = and i64 %t6427, %t6428
  store i64 %t6429, ptr %t6400
  %t6430 = load i64, ptr %t6400
  %t6431 = add i64 13, 0
  %t6432 = and i64 %t6430, %t6431
  %t6433 = add i64 20, 0
  %t6434 = and i64 %t6432, %t6433
  %t6435 = add i64 65535, 0
  %t6436 = and i64 %t6434, %t6435
  store i64 %t6436, ptr %t6400
  %t6437 = load i64, ptr %t6400
  %t6438 = call %NxVal @nx_int(i64 %t6437)
  ret %NxVal %t6438
}
define %NxVal @nx__m_3____main____Cell__m91(%NxVal* %args, i64 %nargs) {
entry:
  %t6439 = alloca %NxVal
  %t6443 = alloca i64
  %t6458 = alloca i64
  store %NxVal zeroinitializer, ptr %t6439
  %t6440 = getelementptr %NxVal, ptr %args, i64 0
  %t6441 = load %NxVal, ptr %t6440
  %t6442 = call %NxVal @nx_clone(%NxVal %t6441)
  store %NxVal %t6442, ptr %t6439
  %t6444 = getelementptr %NxVal, ptr %args, i64 1
  %t6445 = load %NxVal, ptr %t6444
  %t6446 = extractvalue %NxVal %t6445, 1
  store i64 %t6446, ptr %t6443
  %t6447 = load %NxVal, ptr %t6439
  %t6448 = call %NxVal @nx_rec_get(%NxVal %t6447, i64 0)
  %t6449 = load %NxVal, ptr %t6439
  %t6450 = call %NxVal @nx_rec_get(%NxVal %t6449, i64 1)
  %t6451 = call %NxVal @nx_add(%NxVal %t6448, %NxVal %t6450)
  %t6452 = load i64, ptr %t6443
  %t6453 = call %NxVal @nx_int(i64 %t6452)
  %t6454 = call %NxVal @nx_add(%NxVal %t6451, %NxVal %t6453)
  %t6455 = add i64 65535, 0
  %t6456 = call %NxVal @nx_int(i64 %t6455)
  %t6457 = call %NxVal @nx_bitand(%NxVal %t6454, %NxVal %t6456)
  %t6459 = extractvalue %NxVal %t6457, 1
  store i64 %t6459, ptr %t6458
  %t6460 = load i64, ptr %t6458
  %t6461 = add i64 54, 0
  %t6462 = and i64 %t6460, %t6461
  %t6463 = add i64 11, 0
  %t6464 = and i64 %t6462, %t6463
  %t6465 = add i64 65535, 0
  %t6466 = and i64 %t6464, %t6465
  store i64 %t6466, ptr %t6458
  %t6467 = load i64, ptr %t6458
  %t6468 = add i64 97, 0
  %t6469 = sub i64 %t6467, %t6468
  %t6470 = add i64 33, 0
  %t6471 = sub i64 %t6469, %t6470
  %t6472 = add i64 65535, 0
  %t6473 = and i64 %t6471, %t6472
  store i64 %t6473, ptr %t6458
  %t6474 = load i64, ptr %t6458
  %t6475 = add i64 73, 0
  %t6476 = or i64 %t6474, %t6475
  %t6477 = add i64 87, 0
  %t6478 = or i64 %t6476, %t6477
  %t6479 = add i64 65535, 0
  %t6480 = and i64 %t6478, %t6479
  store i64 %t6480, ptr %t6458
  %t6481 = load i64, ptr %t6458
  %t6482 = add i64 70, 0
  %t6483 = xor i64 %t6481, %t6482
  %t6484 = add i64 54, 0
  %t6485 = xor i64 %t6483, %t6484
  %t6486 = add i64 65535, 0
  %t6487 = and i64 %t6485, %t6486
  store i64 %t6487, ptr %t6458
  %t6488 = load i64, ptr %t6458
  %t6489 = add i64 72, 0
  %t6490 = sub i64 %t6488, %t6489
  %t6491 = add i64 5, 0
  %t6492 = sub i64 %t6490, %t6491
  %t6493 = add i64 65535, 0
  %t6494 = and i64 %t6492, %t6493
  store i64 %t6494, ptr %t6458
  %t6495 = load i64, ptr %t6458
  %t6496 = call %NxVal @nx_int(i64 %t6495)
  ret %NxVal %t6496
}
define %NxVal @nx__m_3____main____Cell__m92(%NxVal* %args, i64 %nargs) {
entry:
  %t6497 = alloca %NxVal
  %t6501 = alloca i64
  %t6516 = alloca i64
  store %NxVal zeroinitializer, ptr %t6497
  %t6498 = getelementptr %NxVal, ptr %args, i64 0
  %t6499 = load %NxVal, ptr %t6498
  %t6500 = call %NxVal @nx_clone(%NxVal %t6499)
  store %NxVal %t6500, ptr %t6497
  %t6502 = getelementptr %NxVal, ptr %args, i64 1
  %t6503 = load %NxVal, ptr %t6502
  %t6504 = extractvalue %NxVal %t6503, 1
  store i64 %t6504, ptr %t6501
  %t6505 = load %NxVal, ptr %t6497
  %t6506 = call %NxVal @nx_rec_get(%NxVal %t6505, i64 0)
  %t6507 = load %NxVal, ptr %t6497
  %t6508 = call %NxVal @nx_rec_get(%NxVal %t6507, i64 1)
  %t6509 = call %NxVal @nx_add(%NxVal %t6506, %NxVal %t6508)
  %t6510 = load i64, ptr %t6501
  %t6511 = call %NxVal @nx_int(i64 %t6510)
  %t6512 = call %NxVal @nx_add(%NxVal %t6509, %NxVal %t6511)
  %t6513 = add i64 65535, 0
  %t6514 = call %NxVal @nx_int(i64 %t6513)
  %t6515 = call %NxVal @nx_bitand(%NxVal %t6512, %NxVal %t6514)
  %t6517 = extractvalue %NxVal %t6515, 1
  store i64 %t6517, ptr %t6516
  %t6518 = load i64, ptr %t6516
  %t6519 = add i64 36, 0
  %t6520 = sub i64 %t6518, %t6519
  %t6521 = add i64 84, 0
  %t6522 = sub i64 %t6520, %t6521
  %t6523 = add i64 65535, 0
  %t6524 = and i64 %t6522, %t6523
  store i64 %t6524, ptr %t6516
  %t6525 = load i64, ptr %t6516
  %t6526 = add i64 14, 0
  %t6527 = xor i64 %t6525, %t6526
  %t6528 = add i64 75, 0
  %t6529 = xor i64 %t6527, %t6528
  %t6530 = add i64 65535, 0
  %t6531 = and i64 %t6529, %t6530
  store i64 %t6531, ptr %t6516
  %t6532 = load i64, ptr %t6516
  %t6533 = add i64 40, 0
  %t6534 = sub i64 %t6532, %t6533
  %t6535 = add i64 45, 0
  %t6536 = sub i64 %t6534, %t6535
  %t6537 = add i64 65535, 0
  %t6538 = and i64 %t6536, %t6537
  store i64 %t6538, ptr %t6516
  %t6539 = load i64, ptr %t6516
  %t6540 = add i64 23, 0
  %t6541 = mul i64 %t6539, %t6540
  %t6542 = add i64 70, 0
  %t6543 = mul i64 %t6541, %t6542
  %t6544 = add i64 65535, 0
  %t6545 = and i64 %t6543, %t6544
  store i64 %t6545, ptr %t6516
  %t6546 = load i64, ptr %t6516
  %t6547 = add i64 97, 0
  %t6548 = sub i64 %t6546, %t6547
  %t6549 = add i64 43, 0
  %t6550 = sub i64 %t6548, %t6549
  %t6551 = add i64 65535, 0
  %t6552 = and i64 %t6550, %t6551
  store i64 %t6552, ptr %t6516
  %t6553 = load i64, ptr %t6516
  %t6554 = call %NxVal @nx_int(i64 %t6553)
  ret %NxVal %t6554
}
define %NxVal @nx__m_3____main____Cell__m93(%NxVal* %args, i64 %nargs) {
entry:
  %t6555 = alloca %NxVal
  %t6559 = alloca i64
  %t6574 = alloca i64
  store %NxVal zeroinitializer, ptr %t6555
  %t6556 = getelementptr %NxVal, ptr %args, i64 0
  %t6557 = load %NxVal, ptr %t6556
  %t6558 = call %NxVal @nx_clone(%NxVal %t6557)
  store %NxVal %t6558, ptr %t6555
  %t6560 = getelementptr %NxVal, ptr %args, i64 1
  %t6561 = load %NxVal, ptr %t6560
  %t6562 = extractvalue %NxVal %t6561, 1
  store i64 %t6562, ptr %t6559
  %t6563 = load %NxVal, ptr %t6555
  %t6564 = call %NxVal @nx_rec_get(%NxVal %t6563, i64 0)
  %t6565 = load %NxVal, ptr %t6555
  %t6566 = call %NxVal @nx_rec_get(%NxVal %t6565, i64 1)
  %t6567 = call %NxVal @nx_add(%NxVal %t6564, %NxVal %t6566)
  %t6568 = load i64, ptr %t6559
  %t6569 = call %NxVal @nx_int(i64 %t6568)
  %t6570 = call %NxVal @nx_add(%NxVal %t6567, %NxVal %t6569)
  %t6571 = add i64 65535, 0
  %t6572 = call %NxVal @nx_int(i64 %t6571)
  %t6573 = call %NxVal @nx_bitand(%NxVal %t6570, %NxVal %t6572)
  %t6575 = extractvalue %NxVal %t6573, 1
  store i64 %t6575, ptr %t6574
  %t6576 = load i64, ptr %t6574
  %t6577 = add i64 6, 0
  %t6578 = or i64 %t6576, %t6577
  %t6579 = add i64 9, 0
  %t6580 = or i64 %t6578, %t6579
  %t6581 = add i64 65535, 0
  %t6582 = and i64 %t6580, %t6581
  store i64 %t6582, ptr %t6574
  %t6583 = load i64, ptr %t6574
  %t6584 = add i64 2, 0
  %t6585 = add i64 %t6583, %t6584
  %t6586 = add i64 64, 0
  %t6587 = add i64 %t6585, %t6586
  %t6588 = add i64 65535, 0
  %t6589 = and i64 %t6587, %t6588
  store i64 %t6589, ptr %t6574
  %t6590 = load i64, ptr %t6574
  %t6591 = add i64 21, 0
  %t6592 = sub i64 %t6590, %t6591
  %t6593 = add i64 18, 0
  %t6594 = sub i64 %t6592, %t6593
  %t6595 = add i64 65535, 0
  %t6596 = and i64 %t6594, %t6595
  store i64 %t6596, ptr %t6574
  %t6597 = load i64, ptr %t6574
  %t6598 = add i64 54, 0
  %t6599 = mul i64 %t6597, %t6598
  %t6600 = add i64 53, 0
  %t6601 = mul i64 %t6599, %t6600
  %t6602 = add i64 65535, 0
  %t6603 = and i64 %t6601, %t6602
  store i64 %t6603, ptr %t6574
  %t6604 = load i64, ptr %t6574
  %t6605 = add i64 14, 0
  %t6606 = xor i64 %t6604, %t6605
  %t6607 = add i64 64, 0
  %t6608 = xor i64 %t6606, %t6607
  %t6609 = add i64 65535, 0
  %t6610 = and i64 %t6608, %t6609
  store i64 %t6610, ptr %t6574
  %t6611 = load i64, ptr %t6574
  %t6612 = call %NxVal @nx_int(i64 %t6611)
  ret %NxVal %t6612
}
define %NxVal @nx__m_3____main____Cell__m94(%NxVal* %args, i64 %nargs) {
entry:
  %t6613 = alloca %NxVal
  %t6617 = alloca i64
  %t6632 = alloca i64
  store %NxVal zeroinitializer, ptr %t6613
  %t6614 = getelementptr %NxVal, ptr %args, i64 0
  %t6615 = load %NxVal, ptr %t6614
  %t6616 = call %NxVal @nx_clone(%NxVal %t6615)
  store %NxVal %t6616, ptr %t6613
  %t6618 = getelementptr %NxVal, ptr %args, i64 1
  %t6619 = load %NxVal, ptr %t6618
  %t6620 = extractvalue %NxVal %t6619, 1
  store i64 %t6620, ptr %t6617
  %t6621 = load %NxVal, ptr %t6613
  %t6622 = call %NxVal @nx_rec_get(%NxVal %t6621, i64 0)
  %t6623 = load %NxVal, ptr %t6613
  %t6624 = call %NxVal @nx_rec_get(%NxVal %t6623, i64 1)
  %t6625 = call %NxVal @nx_add(%NxVal %t6622, %NxVal %t6624)
  %t6626 = load i64, ptr %t6617
  %t6627 = call %NxVal @nx_int(i64 %t6626)
  %t6628 = call %NxVal @nx_add(%NxVal %t6625, %NxVal %t6627)
  %t6629 = add i64 65535, 0
  %t6630 = call %NxVal @nx_int(i64 %t6629)
  %t6631 = call %NxVal @nx_bitand(%NxVal %t6628, %NxVal %t6630)
  %t6633 = extractvalue %NxVal %t6631, 1
  store i64 %t6633, ptr %t6632
  %t6634 = load i64, ptr %t6632
  %t6635 = add i64 4, 0
  %t6636 = add i64 %t6634, %t6635
  %t6637 = add i64 87, 0
  %t6638 = add i64 %t6636, %t6637
  %t6639 = add i64 65535, 0
  %t6640 = and i64 %t6638, %t6639
  store i64 %t6640, ptr %t6632
  %t6641 = load i64, ptr %t6632
  %t6642 = add i64 25, 0
  %t6643 = call i64 @nx_mod_i64(i64 %t6641, i64 %t6642)
  %t6644 = add i64 32, 0
  %t6645 = call i64 @nx_mod_i64(i64 %t6643, i64 %t6644)
  %t6646 = add i64 65535, 0
  %t6647 = and i64 %t6645, %t6646
  store i64 %t6647, ptr %t6632
  %t6648 = load i64, ptr %t6632
  %t6649 = add i64 25, 0
  %t6650 = mul i64 %t6648, %t6649
  %t6651 = add i64 46, 0
  %t6652 = mul i64 %t6650, %t6651
  %t6653 = add i64 65535, 0
  %t6654 = and i64 %t6652, %t6653
  store i64 %t6654, ptr %t6632
  %t6655 = load i64, ptr %t6632
  %t6656 = add i64 17, 0
  %t6657 = and i64 %t6655, %t6656
  %t6658 = add i64 81, 0
  %t6659 = and i64 %t6657, %t6658
  %t6660 = add i64 65535, 0
  %t6661 = and i64 %t6659, %t6660
  store i64 %t6661, ptr %t6632
  %t6662 = load i64, ptr %t6632
  %t6663 = add i64 45, 0
  %t6664 = add i64 %t6662, %t6663
  %t6665 = add i64 10, 0
  %t6666 = add i64 %t6664, %t6665
  %t6667 = add i64 65535, 0
  %t6668 = and i64 %t6666, %t6667
  store i64 %t6668, ptr %t6632
  %t6669 = load i64, ptr %t6632
  %t6670 = call %NxVal @nx_int(i64 %t6669)
  ret %NxVal %t6670
}
define %NxVal @nx__m_3____main____Cell__m95(%NxVal* %args, i64 %nargs) {
entry:
  %t6671 = alloca %NxVal
  %t6675 = alloca i64
  %t6690 = alloca i64
  store %NxVal zeroinitializer, ptr %t6671
  %t6672 = getelementptr %NxVal, ptr %args, i64 0
  %t6673 = load %NxVal, ptr %t6672
  %t6674 = call %NxVal @nx_clone(%NxVal %t6673)
  store %NxVal %t6674, ptr %t6671
  %t6676 = getelementptr %NxVal, ptr %args, i64 1
  %t6677 = load %NxVal, ptr %t6676
  %t6678 = extractvalue %NxVal %t6677, 1
  store i64 %t6678, ptr %t6675
  %t6679 = load %NxVal, ptr %t6671
  %t6680 = call %NxVal @nx_rec_get(%NxVal %t6679, i64 0)
  %t6681 = load %NxVal, ptr %t6671
  %t6682 = call %NxVal @nx_rec_get(%NxVal %t6681, i64 1)
  %t6683 = call %NxVal @nx_add(%NxVal %t6680, %NxVal %t6682)
  %t6684 = load i64, ptr %t6675
  %t6685 = call %NxVal @nx_int(i64 %t6684)
  %t6686 = call %NxVal @nx_add(%NxVal %t6683, %NxVal %t6685)
  %t6687 = add i64 65535, 0
  %t6688 = call %NxVal @nx_int(i64 %t6687)
  %t6689 = call %NxVal @nx_bitand(%NxVal %t6686, %NxVal %t6688)
  %t6691 = extractvalue %NxVal %t6689, 1
  store i64 %t6691, ptr %t6690
  %t6692 = load i64, ptr %t6690
  %t6693 = add i64 75, 0
  %t6694 = mul i64 %t6692, %t6693
  %t6695 = add i64 34, 0
  %t6696 = mul i64 %t6694, %t6695
  %t6697 = add i64 65535, 0
  %t6698 = and i64 %t6696, %t6697
  store i64 %t6698, ptr %t6690
  %t6699 = load i64, ptr %t6690
  %t6700 = add i64 97, 0
  %t6701 = sub i64 %t6699, %t6700
  %t6702 = add i64 7, 0
  %t6703 = sub i64 %t6701, %t6702
  %t6704 = add i64 65535, 0
  %t6705 = and i64 %t6703, %t6704
  store i64 %t6705, ptr %t6690
  %t6706 = load i64, ptr %t6690
  %t6707 = add i64 33, 0
  %t6708 = xor i64 %t6706, %t6707
  %t6709 = add i64 56, 0
  %t6710 = xor i64 %t6708, %t6709
  %t6711 = add i64 65535, 0
  %t6712 = and i64 %t6710, %t6711
  store i64 %t6712, ptr %t6690
  %t6713 = load i64, ptr %t6690
  %t6714 = add i64 63, 0
  %t6715 = and i64 %t6713, %t6714
  %t6716 = add i64 18, 0
  %t6717 = and i64 %t6715, %t6716
  %t6718 = add i64 65535, 0
  %t6719 = and i64 %t6717, %t6718
  store i64 %t6719, ptr %t6690
  %t6720 = load i64, ptr %t6690
  %t6721 = add i64 96, 0
  %t6722 = or i64 %t6720, %t6721
  %t6723 = add i64 39, 0
  %t6724 = or i64 %t6722, %t6723
  %t6725 = add i64 65535, 0
  %t6726 = and i64 %t6724, %t6725
  store i64 %t6726, ptr %t6690
  %t6727 = load i64, ptr %t6690
  %t6728 = call %NxVal @nx_int(i64 %t6727)
  ret %NxVal %t6728
}
define %NxVal @nx__m_3____main____Cell__m96(%NxVal* %args, i64 %nargs) {
entry:
  %t6729 = alloca %NxVal
  %t6733 = alloca i64
  %t6748 = alloca i64
  store %NxVal zeroinitializer, ptr %t6729
  %t6730 = getelementptr %NxVal, ptr %args, i64 0
  %t6731 = load %NxVal, ptr %t6730
  %t6732 = call %NxVal @nx_clone(%NxVal %t6731)
  store %NxVal %t6732, ptr %t6729
  %t6734 = getelementptr %NxVal, ptr %args, i64 1
  %t6735 = load %NxVal, ptr %t6734
  %t6736 = extractvalue %NxVal %t6735, 1
  store i64 %t6736, ptr %t6733
  %t6737 = load %NxVal, ptr %t6729
  %t6738 = call %NxVal @nx_rec_get(%NxVal %t6737, i64 0)
  %t6739 = load %NxVal, ptr %t6729
  %t6740 = call %NxVal @nx_rec_get(%NxVal %t6739, i64 1)
  %t6741 = call %NxVal @nx_add(%NxVal %t6738, %NxVal %t6740)
  %t6742 = load i64, ptr %t6733
  %t6743 = call %NxVal @nx_int(i64 %t6742)
  %t6744 = call %NxVal @nx_add(%NxVal %t6741, %NxVal %t6743)
  %t6745 = add i64 65535, 0
  %t6746 = call %NxVal @nx_int(i64 %t6745)
  %t6747 = call %NxVal @nx_bitand(%NxVal %t6744, %NxVal %t6746)
  %t6749 = extractvalue %NxVal %t6747, 1
  store i64 %t6749, ptr %t6748
  %t6750 = load i64, ptr %t6748
  %t6751 = add i64 24, 0
  %t6752 = or i64 %t6750, %t6751
  %t6753 = add i64 86, 0
  %t6754 = or i64 %t6752, %t6753
  %t6755 = add i64 65535, 0
  %t6756 = and i64 %t6754, %t6755
  store i64 %t6756, ptr %t6748
  %t6757 = load i64, ptr %t6748
  %t6758 = add i64 66, 0
  %t6759 = and i64 %t6757, %t6758
  %t6760 = add i64 75, 0
  %t6761 = and i64 %t6759, %t6760
  %t6762 = add i64 65535, 0
  %t6763 = and i64 %t6761, %t6762
  store i64 %t6763, ptr %t6748
  %t6764 = load i64, ptr %t6748
  %t6765 = add i64 49, 0
  %t6766 = sub i64 %t6764, %t6765
  %t6767 = add i64 45, 0
  %t6768 = sub i64 %t6766, %t6767
  %t6769 = add i64 65535, 0
  %t6770 = and i64 %t6768, %t6769
  store i64 %t6770, ptr %t6748
  %t6771 = load i64, ptr %t6748
  %t6772 = add i64 11, 0
  %t6773 = sub i64 %t6771, %t6772
  %t6774 = add i64 84, 0
  %t6775 = sub i64 %t6773, %t6774
  %t6776 = add i64 65535, 0
  %t6777 = and i64 %t6775, %t6776
  store i64 %t6777, ptr %t6748
  %t6778 = load i64, ptr %t6748
  %t6779 = add i64 28, 0
  %t6780 = call i64 @nx_mod_i64(i64 %t6778, i64 %t6779)
  %t6781 = add i64 30, 0
  %t6782 = call i64 @nx_mod_i64(i64 %t6780, i64 %t6781)
  %t6783 = add i64 65535, 0
  %t6784 = and i64 %t6782, %t6783
  store i64 %t6784, ptr %t6748
  %t6785 = load i64, ptr %t6748
  %t6786 = call %NxVal @nx_int(i64 %t6785)
  ret %NxVal %t6786
}
define %NxVal @nx__m_3____main____Cell__m97(%NxVal* %args, i64 %nargs) {
entry:
  %t6787 = alloca %NxVal
  %t6791 = alloca i64
  %t6806 = alloca i64
  store %NxVal zeroinitializer, ptr %t6787
  %t6788 = getelementptr %NxVal, ptr %args, i64 0
  %t6789 = load %NxVal, ptr %t6788
  %t6790 = call %NxVal @nx_clone(%NxVal %t6789)
  store %NxVal %t6790, ptr %t6787
  %t6792 = getelementptr %NxVal, ptr %args, i64 1
  %t6793 = load %NxVal, ptr %t6792
  %t6794 = extractvalue %NxVal %t6793, 1
  store i64 %t6794, ptr %t6791
  %t6795 = load %NxVal, ptr %t6787
  %t6796 = call %NxVal @nx_rec_get(%NxVal %t6795, i64 0)
  %t6797 = load %NxVal, ptr %t6787
  %t6798 = call %NxVal @nx_rec_get(%NxVal %t6797, i64 1)
  %t6799 = call %NxVal @nx_add(%NxVal %t6796, %NxVal %t6798)
  %t6800 = load i64, ptr %t6791
  %t6801 = call %NxVal @nx_int(i64 %t6800)
  %t6802 = call %NxVal @nx_add(%NxVal %t6799, %NxVal %t6801)
  %t6803 = add i64 65535, 0
  %t6804 = call %NxVal @nx_int(i64 %t6803)
  %t6805 = call %NxVal @nx_bitand(%NxVal %t6802, %NxVal %t6804)
  %t6807 = extractvalue %NxVal %t6805, 1
  store i64 %t6807, ptr %t6806
  %t6808 = load i64, ptr %t6806
  %t6809 = add i64 87, 0
  %t6810 = call i64 @nx_mod_i64(i64 %t6808, i64 %t6809)
  %t6811 = add i64 12, 0
  %t6812 = call i64 @nx_mod_i64(i64 %t6810, i64 %t6811)
  %t6813 = add i64 65535, 0
  %t6814 = and i64 %t6812, %t6813
  store i64 %t6814, ptr %t6806
  %t6815 = load i64, ptr %t6806
  %t6816 = add i64 93, 0
  %t6817 = xor i64 %t6815, %t6816
  %t6818 = add i64 49, 0
  %t6819 = xor i64 %t6817, %t6818
  %t6820 = add i64 65535, 0
  %t6821 = and i64 %t6819, %t6820
  store i64 %t6821, ptr %t6806
  %t6822 = load i64, ptr %t6806
  %t6823 = add i64 92, 0
  %t6824 = or i64 %t6822, %t6823
  %t6825 = add i64 8, 0
  %t6826 = or i64 %t6824, %t6825
  %t6827 = add i64 65535, 0
  %t6828 = and i64 %t6826, %t6827
  store i64 %t6828, ptr %t6806
  %t6829 = load i64, ptr %t6806
  %t6830 = add i64 56, 0
  %t6831 = xor i64 %t6829, %t6830
  %t6832 = add i64 89, 0
  %t6833 = xor i64 %t6831, %t6832
  %t6834 = add i64 65535, 0
  %t6835 = and i64 %t6833, %t6834
  store i64 %t6835, ptr %t6806
  %t6836 = load i64, ptr %t6806
  %t6837 = add i64 54, 0
  %t6838 = call i64 @nx_mod_i64(i64 %t6836, i64 %t6837)
  %t6839 = add i64 8, 0
  %t6840 = call i64 @nx_mod_i64(i64 %t6838, i64 %t6839)
  %t6841 = add i64 65535, 0
  %t6842 = and i64 %t6840, %t6841
  store i64 %t6842, ptr %t6806
  %t6843 = load i64, ptr %t6806
  %t6844 = call %NxVal @nx_int(i64 %t6843)
  ret %NxVal %t6844
}
define %NxVal @nx__m_3____main____Cell__m98(%NxVal* %args, i64 %nargs) {
entry:
  %t6845 = alloca %NxVal
  %t6849 = alloca i64
  %t6864 = alloca i64
  store %NxVal zeroinitializer, ptr %t6845
  %t6846 = getelementptr %NxVal, ptr %args, i64 0
  %t6847 = load %NxVal, ptr %t6846
  %t6848 = call %NxVal @nx_clone(%NxVal %t6847)
  store %NxVal %t6848, ptr %t6845
  %t6850 = getelementptr %NxVal, ptr %args, i64 1
  %t6851 = load %NxVal, ptr %t6850
  %t6852 = extractvalue %NxVal %t6851, 1
  store i64 %t6852, ptr %t6849
  %t6853 = load %NxVal, ptr %t6845
  %t6854 = call %NxVal @nx_rec_get(%NxVal %t6853, i64 0)
  %t6855 = load %NxVal, ptr %t6845
  %t6856 = call %NxVal @nx_rec_get(%NxVal %t6855, i64 1)
  %t6857 = call %NxVal @nx_add(%NxVal %t6854, %NxVal %t6856)
  %t6858 = load i64, ptr %t6849
  %t6859 = call %NxVal @nx_int(i64 %t6858)
  %t6860 = call %NxVal @nx_add(%NxVal %t6857, %NxVal %t6859)
  %t6861 = add i64 65535, 0
  %t6862 = call %NxVal @nx_int(i64 %t6861)
  %t6863 = call %NxVal @nx_bitand(%NxVal %t6860, %NxVal %t6862)
  %t6865 = extractvalue %NxVal %t6863, 1
  store i64 %t6865, ptr %t6864
  %t6866 = load i64, ptr %t6864
  %t6867 = add i64 1, 0
  %t6868 = call i64 @nx_mod_i64(i64 %t6866, i64 %t6867)
  %t6869 = add i64 36, 0
  %t6870 = call i64 @nx_mod_i64(i64 %t6868, i64 %t6869)
  %t6871 = add i64 65535, 0
  %t6872 = and i64 %t6870, %t6871
  store i64 %t6872, ptr %t6864
  %t6873 = load i64, ptr %t6864
  %t6874 = add i64 21, 0
  %t6875 = call i64 @nx_mod_i64(i64 %t6873, i64 %t6874)
  %t6876 = add i64 35, 0
  %t6877 = call i64 @nx_mod_i64(i64 %t6875, i64 %t6876)
  %t6878 = add i64 65535, 0
  %t6879 = and i64 %t6877, %t6878
  store i64 %t6879, ptr %t6864
  %t6880 = load i64, ptr %t6864
  %t6881 = add i64 69, 0
  %t6882 = xor i64 %t6880, %t6881
  %t6883 = add i64 29, 0
  %t6884 = xor i64 %t6882, %t6883
  %t6885 = add i64 65535, 0
  %t6886 = and i64 %t6884, %t6885
  store i64 %t6886, ptr %t6864
  %t6887 = load i64, ptr %t6864
  %t6888 = add i64 69, 0
  %t6889 = add i64 %t6887, %t6888
  %t6890 = add i64 82, 0
  %t6891 = add i64 %t6889, %t6890
  %t6892 = add i64 65535, 0
  %t6893 = and i64 %t6891, %t6892
  store i64 %t6893, ptr %t6864
  %t6894 = load i64, ptr %t6864
  %t6895 = add i64 45, 0
  %t6896 = or i64 %t6894, %t6895
  %t6897 = add i64 58, 0
  %t6898 = or i64 %t6896, %t6897
  %t6899 = add i64 65535, 0
  %t6900 = and i64 %t6898, %t6899
  store i64 %t6900, ptr %t6864
  %t6901 = load i64, ptr %t6864
  %t6902 = call %NxVal @nx_int(i64 %t6901)
  ret %NxVal %t6902
}
define %NxVal @nx__m_3____main____Cell__m99(%NxVal* %args, i64 %nargs) {
entry:
  %t6903 = alloca %NxVal
  %t6907 = alloca i64
  %t6922 = alloca i64
  store %NxVal zeroinitializer, ptr %t6903
  %t6904 = getelementptr %NxVal, ptr %args, i64 0
  %t6905 = load %NxVal, ptr %t6904
  %t6906 = call %NxVal @nx_clone(%NxVal %t6905)
  store %NxVal %t6906, ptr %t6903
  %t6908 = getelementptr %NxVal, ptr %args, i64 1
  %t6909 = load %NxVal, ptr %t6908
  %t6910 = extractvalue %NxVal %t6909, 1
  store i64 %t6910, ptr %t6907
  %t6911 = load %NxVal, ptr %t6903
  %t6912 = call %NxVal @nx_rec_get(%NxVal %t6911, i64 0)
  %t6913 = load %NxVal, ptr %t6903
  %t6914 = call %NxVal @nx_rec_get(%NxVal %t6913, i64 1)
  %t6915 = call %NxVal @nx_add(%NxVal %t6912, %NxVal %t6914)
  %t6916 = load i64, ptr %t6907
  %t6917 = call %NxVal @nx_int(i64 %t6916)
  %t6918 = call %NxVal @nx_add(%NxVal %t6915, %NxVal %t6917)
  %t6919 = add i64 65535, 0
  %t6920 = call %NxVal @nx_int(i64 %t6919)
  %t6921 = call %NxVal @nx_bitand(%NxVal %t6918, %NxVal %t6920)
  %t6923 = extractvalue %NxVal %t6921, 1
  store i64 %t6923, ptr %t6922
  %t6924 = load i64, ptr %t6922
  %t6925 = add i64 65, 0
  %t6926 = add i64 %t6924, %t6925
  %t6927 = add i64 20, 0
  %t6928 = add i64 %t6926, %t6927
  %t6929 = add i64 65535, 0
  %t6930 = and i64 %t6928, %t6929
  store i64 %t6930, ptr %t6922
  %t6931 = load i64, ptr %t6922
  %t6932 = add i64 39, 0
  %t6933 = sub i64 %t6931, %t6932
  %t6934 = add i64 19, 0
  %t6935 = sub i64 %t6933, %t6934
  %t6936 = add i64 65535, 0
  %t6937 = and i64 %t6935, %t6936
  store i64 %t6937, ptr %t6922
  %t6938 = load i64, ptr %t6922
  %t6939 = add i64 10, 0
  %t6940 = sub i64 %t6938, %t6939
  %t6941 = add i64 56, 0
  %t6942 = sub i64 %t6940, %t6941
  %t6943 = add i64 65535, 0
  %t6944 = and i64 %t6942, %t6943
  store i64 %t6944, ptr %t6922
  %t6945 = load i64, ptr %t6922
  %t6946 = add i64 44, 0
  %t6947 = or i64 %t6945, %t6946
  %t6948 = add i64 61, 0
  %t6949 = or i64 %t6947, %t6948
  %t6950 = add i64 65535, 0
  %t6951 = and i64 %t6949, %t6950
  store i64 %t6951, ptr %t6922
  %t6952 = load i64, ptr %t6922
  %t6953 = add i64 25, 0
  %t6954 = add i64 %t6952, %t6953
  %t6955 = add i64 44, 0
  %t6956 = add i64 %t6954, %t6955
  %t6957 = add i64 65535, 0
  %t6958 = and i64 %t6956, %t6957
  store i64 %t6958, ptr %t6922
  %t6959 = load i64, ptr %t6922
  %t6960 = call %NxVal @nx_int(i64 %t6959)
  ret %NxVal %t6960
}
define void @nx__init___main__() {
entry:
  %t6983 = alloca [2 x %NxVal]
  %t7070 = alloca [2 x %NxVal]
  %t7157 = alloca [2 x %NxVal]
  %t7244 = alloca [2 x %NxVal]
  %t7331 = alloca [2 x %NxVal]
  %t7418 = alloca [2 x %NxVal]
  %t7505 = alloca [2 x %NxVal]
  %t7592 = alloca [2 x %NxVal]
  %t7679 = alloca [2 x %NxVal]
  %t7766 = alloca [2 x %NxVal]
  %t7853 = alloca [2 x %NxVal]
  %t7940 = alloca [2 x %NxVal]
  %t8027 = alloca [2 x %NxVal]
  %t8114 = alloca [2 x %NxVal]
  %t8201 = alloca [2 x %NxVal]
  %t8288 = alloca [2 x %NxVal]
  %t8375 = alloca [2 x %NxVal]
  %t8462 = alloca [2 x %NxVal]
  %t8549 = alloca [2 x %NxVal]
  %t8636 = alloca [2 x %NxVal]
  %t8723 = alloca [2 x %NxVal]
  %t8810 = alloca [2 x %NxVal]
  %t8897 = alloca [2 x %NxVal]
  %t8984 = alloca [2 x %NxVal]
  %t9071 = alloca [2 x %NxVal]
  %t9158 = alloca [2 x %NxVal]
  %t9245 = alloca [2 x %NxVal]
  %t9332 = alloca [2 x %NxVal]
  %t9419 = alloca [2 x %NxVal]
  %t9506 = alloca [2 x %NxVal]
  %t9593 = alloca [2 x %NxVal]
  %t9680 = alloca [2 x %NxVal]
  %t9767 = alloca [2 x %NxVal]
  %t9854 = alloca [2 x %NxVal]
  %t9941 = alloca [2 x %NxVal]
  %t10028 = alloca [2 x %NxVal]
  %t10115 = alloca [2 x %NxVal]
  %t10202 = alloca [2 x %NxVal]
  %t10289 = alloca [2 x %NxVal]
  %t10376 = alloca [2 x %NxVal]
  %t10463 = alloca [2 x %NxVal]
  %t10550 = alloca [2 x %NxVal]
  %t10637 = alloca [2 x %NxVal]
  %t10724 = alloca [2 x %NxVal]
  %t10811 = alloca [2 x %NxVal]
  %t10898 = alloca [2 x %NxVal]
  %t10985 = alloca [2 x %NxVal]
  %t11072 = alloca [2 x %NxVal]
  %t11159 = alloca [2 x %NxVal]
  %t11246 = alloca [2 x %NxVal]
  %t11333 = alloca [2 x %NxVal]
  %t11420 = alloca [2 x %NxVal]
  %t11507 = alloca [2 x %NxVal]
  %t11594 = alloca [2 x %NxVal]
  %t11681 = alloca [2 x %NxVal]
  %t11768 = alloca [2 x %NxVal]
  %t11855 = alloca [2 x %NxVal]
  %t11942 = alloca [2 x %NxVal]
  %t12029 = alloca [2 x %NxVal]
  %t12116 = alloca [2 x %NxVal]
  %t12203 = alloca [2 x %NxVal]
  %t12290 = alloca [2 x %NxVal]
  %t12377 = alloca [2 x %NxVal]
  %t12464 = alloca [2 x %NxVal]
  %t12551 = alloca [2 x %NxVal]
  %t12638 = alloca [2 x %NxVal]
  %t12725 = alloca [2 x %NxVal]
  %t12812 = alloca [2 x %NxVal]
  %t12899 = alloca [2 x %NxVal]
  %t12986 = alloca [2 x %NxVal]
  %t13073 = alloca [2 x %NxVal]
  %t13160 = alloca [2 x %NxVal]
  %t13247 = alloca [2 x %NxVal]
  %t13334 = alloca [2 x %NxVal]
  %t13421 = alloca [2 x %NxVal]
  %t13508 = alloca [2 x %NxVal]
  %t13595 = alloca [2 x %NxVal]
  %t13682 = alloca [2 x %NxVal]
  %t13769 = alloca [2 x %NxVal]
  %t13856 = alloca [2 x %NxVal]
  %t13943 = alloca [2 x %NxVal]
  %t14030 = alloca [2 x %NxVal]
  %t14117 = alloca [2 x %NxVal]
  %t14204 = alloca [2 x %NxVal]
  %t14291 = alloca [2 x %NxVal]
  %t14378 = alloca [2 x %NxVal]
  %t14465 = alloca [2 x %NxVal]
  %t14552 = alloca [2 x %NxVal]
  %t14639 = alloca [2 x %NxVal]
  %t14726 = alloca [2 x %NxVal]
  %t14813 = alloca [2 x %NxVal]
  %t14900 = alloca [2 x %NxVal]
  %t14987 = alloca [2 x %NxVal]
  %t15074 = alloca [2 x %NxVal]
  %t15161 = alloca [2 x %NxVal]
  %t15248 = alloca [2 x %NxVal]
  %t15335 = alloca [2 x %NxVal]
  %t15422 = alloca [2 x %NxVal]
  %t15509 = alloca [2 x %NxVal]
  %t15596 = alloca [2 x %NxVal]
  %t15683 = alloca [2 x %NxVal]
  %t15770 = alloca [2 x %NxVal]
  %t15857 = alloca [2 x %NxVal]
  %t15944 = alloca [2 x %NxVal]
  %t16031 = alloca [2 x %NxVal]
  %t16118 = alloca [2 x %NxVal]
  %t16205 = alloca [2 x %NxVal]
  %t16292 = alloca [2 x %NxVal]
  %t16379 = alloca [2 x %NxVal]
  %t16466 = alloca [2 x %NxVal]
  %t16553 = alloca [2 x %NxVal]
  %t16640 = alloca [2 x %NxVal]
  %t16727 = alloca [2 x %NxVal]
  %t16814 = alloca [2 x %NxVal]
  %t16901 = alloca [2 x %NxVal]
  %t16988 = alloca [2 x %NxVal]
  %t17075 = alloca [2 x %NxVal]
  %t17162 = alloca [2 x %NxVal]
  %t17249 = alloca [2 x %NxVal]
  %t17336 = alloca [2 x %NxVal]
  %t17409 = alloca [1 x %NxVal]
  %t17413 = alloca [2 x %NxVal]
  %t17416 = alloca [2 x %NxVal]
  %t17427 = alloca [2 x %NxVal]
  %t6961 = load i1, ptr @nx__done___main__
  br i1 %t6961, label %initskip2, label %initrun1
initrun1:
  store i1 true, ptr @nx__done___main__
  %t6962 = call %NxVal @nx_new_record(i64 2, ptr @nx__d_4____main____Cell)
  %t6963 = add i64 7, 0
  %t6964 = call %NxVal @nx_int(i64 %t6963)
  call void @nx_rec_set(%NxVal %t6962, i64 0, %NxVal %t6964)
  %t6965 = add i64 11, 0
  %t6966 = call %NxVal @nx_int(i64 %t6965)
  call void @nx_rec_set(%NxVal %t6962, i64 1, %NxVal %t6966)
  store %NxVal %t6962, ptr @nx__g___main____c
  %t6967 = add i64 0, 0
  %t6968 = call %NxVal @nx_int(i64 %t6967)
  store %NxVal %t6968, ptr @nx__g___main____total
  %t6969 = add i64 0, 0
  %t6970 = call %NxVal @nx_int(i64 %t6969)
  store %NxVal %t6970, ptr @nx__g___main____i0
  %t6971 = add i64 0, 0
  %t6972 = call %NxVal @nx_int(i64 %t6971)
  store %NxVal %t6972, ptr @nx__g___main____acc0
  br label %wcond3
wcond3:
  %t6973 = load %NxVal, ptr @nx__g___main____i0
  %t6974 = add i64 3, 0
  %t6975 = extractvalue %NxVal %t6973, 1
  %t6976 = icmp slt i64 %t6975, %t6974
  br i1 %t6976, label %wbody4, label %wend5
wbody4:
  %t6977 = load %NxVal, ptr @nx__g___main____acc0
  %t6978 = load %NxVal, ptr @nx__g___main____c
  %t6979 = load %NxVal, ptr @nx__g___main____i0
  %t6980 = add i64 0, 0
  %t6981 = extractvalue %NxVal %t6979, 1
  %t6982 = add i64 %t6981, %t6980
  %t6984 = getelementptr [2 x %NxVal], ptr %t6983, i64 0, i64 0
  store %NxVal %t6978, ptr %t6984
  %t6985 = call %NxVal @nx_int(i64 %t6982)
  %t6986 = getelementptr [2 x %NxVal], ptr %t6983, i64 0, i64 1
  store %NxVal %t6985, ptr %t6986
  %t6987 = getelementptr [2 x %NxVal], ptr %t6983, i64 0, i64 0
  %t6988 = call %NxVal @nx__m_2____main____Cell__m0(ptr %t6987, i64 2)
  %t6989 = extractvalue %NxVal %t6988, 1
  %t6990 = extractvalue %NxVal %t6977, 1
  %t6991 = add i64 %t6990, %t6989
  %t6992 = add i64 65535, 0
  %t6993 = and i64 %t6991, %t6992
  %t6994 = call %NxVal @nx_int(i64 %t6993)
  store %NxVal %t6994, ptr @nx__g___main____acc0
  %t6995 = load %NxVal, ptr @nx__g___main____acc0
  %t6996 = add i64 84, 0
  %t6997 = extractvalue %NxVal %t6995, 1
  %t6998 = xor i64 %t6997, %t6996
  %t6999 = add i64 34, 0
  %t7000 = load %NxVal, ptr @nx__g___main____i0
  %t7001 = extractvalue %NxVal %t7000, 1
  %t7002 = add i64 %t6999, %t7001
  %t7003 = xor i64 %t6998, %t7002
  %t7004 = add i64 65535, 0
  %t7005 = and i64 %t7003, %t7004
  %t7006 = call %NxVal @nx_int(i64 %t7005)
  store %NxVal %t7006, ptr @nx__g___main____acc0
  %t7007 = load %NxVal, ptr @nx__g___main____acc0
  %t7008 = add i64 31, 0
  %t7009 = extractvalue %NxVal %t7007, 1
  %t7010 = sub i64 %t7009, %t7008
  %t7011 = add i64 19, 0
  %t7012 = sub i64 %t7010, %t7011
  %t7013 = load %NxVal, ptr @nx__g___main____i0
  %t7014 = extractvalue %NxVal %t7013, 1
  %t7015 = add i64 %t7012, %t7014
  %t7016 = add i64 65535, 0
  %t7017 = and i64 %t7015, %t7016
  %t7018 = call %NxVal @nx_int(i64 %t7017)
  store %NxVal %t7018, ptr @nx__g___main____acc0
  %t7019 = load %NxVal, ptr @nx__g___main____acc0
  %t7020 = add i64 30, 0
  %t7021 = extractvalue %NxVal %t7019, 1
  %t7022 = xor i64 %t7021, %t7020
  %t7023 = add i64 12, 0
  %t7024 = load %NxVal, ptr @nx__g___main____i0
  %t7025 = extractvalue %NxVal %t7024, 1
  %t7026 = add i64 %t7023, %t7025
  %t7027 = xor i64 %t7022, %t7026
  %t7028 = add i64 65535, 0
  %t7029 = and i64 %t7027, %t7028
  %t7030 = call %NxVal @nx_int(i64 %t7029)
  store %NxVal %t7030, ptr @nx__g___main____acc0
  %t7031 = load %NxVal, ptr @nx__g___main____acc0
  %t7032 = add i64 92, 0
  %t7033 = extractvalue %NxVal %t7031, 1
  %t7034 = or i64 %t7033, %t7032
  %t7035 = add i64 34, 0
  %t7036 = load %NxVal, ptr @nx__g___main____i0
  %t7037 = extractvalue %NxVal %t7036, 1
  %t7038 = add i64 %t7035, %t7037
  %t7039 = or i64 %t7034, %t7038
  %t7040 = add i64 65535, 0
  %t7041 = and i64 %t7039, %t7040
  %t7042 = call %NxVal @nx_int(i64 %t7041)
  store %NxVal %t7042, ptr @nx__g___main____acc0
  %t7043 = load %NxVal, ptr @nx__g___main____i0
  %t7044 = add i64 1, 0
  %t7045 = extractvalue %NxVal %t7043, 1
  %t7046 = add i64 %t7045, %t7044
  %t7047 = call %NxVal @nx_int(i64 %t7046)
  store %NxVal %t7047, ptr @nx__g___main____i0
  br label %wcond3
wend5:
  %t7048 = load %NxVal, ptr @nx__g___main____total
  %t7049 = load %NxVal, ptr @nx__g___main____acc0
  %t7050 = extractvalue %NxVal %t7048, 1
  %t7051 = extractvalue %NxVal %t7049, 1
  %t7052 = add i64 %t7050, %t7051
  %t7053 = add i64 65535, 0
  %t7054 = and i64 %t7052, %t7053
  %t7055 = call %NxVal @nx_int(i64 %t7054)
  store %NxVal %t7055, ptr @nx__g___main____total
  %t7056 = add i64 0, 0
  %t7057 = call %NxVal @nx_int(i64 %t7056)
  store %NxVal %t7057, ptr @nx__g___main____i1
  %t7058 = add i64 0, 0
  %t7059 = call %NxVal @nx_int(i64 %t7058)
  store %NxVal %t7059, ptr @nx__g___main____acc1
  br label %wcond6
wcond6:
  %t7060 = load %NxVal, ptr @nx__g___main____i1
  %t7061 = add i64 3, 0
  %t7062 = extractvalue %NxVal %t7060, 1
  %t7063 = icmp slt i64 %t7062, %t7061
  br i1 %t7063, label %wbody7, label %wend8
wbody7:
  %t7064 = load %NxVal, ptr @nx__g___main____acc1
  %t7065 = load %NxVal, ptr @nx__g___main____c
  %t7066 = load %NxVal, ptr @nx__g___main____i1
  %t7067 = add i64 1, 0
  %t7068 = extractvalue %NxVal %t7066, 1
  %t7069 = add i64 %t7068, %t7067
  %t7071 = getelementptr [2 x %NxVal], ptr %t7070, i64 0, i64 0
  store %NxVal %t7065, ptr %t7071
  %t7072 = call %NxVal @nx_int(i64 %t7069)
  %t7073 = getelementptr [2 x %NxVal], ptr %t7070, i64 0, i64 1
  store %NxVal %t7072, ptr %t7073
  %t7074 = getelementptr [2 x %NxVal], ptr %t7070, i64 0, i64 0
  %t7075 = call %NxVal @nx__m_2____main____Cell__m1(ptr %t7074, i64 2)
  %t7076 = extractvalue %NxVal %t7075, 1
  %t7077 = extractvalue %NxVal %t7064, 1
  %t7078 = add i64 %t7077, %t7076
  %t7079 = add i64 65535, 0
  %t7080 = and i64 %t7078, %t7079
  %t7081 = call %NxVal @nx_int(i64 %t7080)
  store %NxVal %t7081, ptr @nx__g___main____acc1
  %t7082 = load %NxVal, ptr @nx__g___main____acc1
  %t7083 = add i64 13, 0
  %t7084 = extractvalue %NxVal %t7082, 1
  %t7085 = add i64 %t7084, %t7083
  %t7086 = add i64 35, 0
  %t7087 = add i64 %t7085, %t7086
  %t7088 = load %NxVal, ptr @nx__g___main____i1
  %t7089 = extractvalue %NxVal %t7088, 1
  %t7090 = add i64 %t7087, %t7089
  %t7091 = add i64 65535, 0
  %t7092 = and i64 %t7090, %t7091
  %t7093 = call %NxVal @nx_int(i64 %t7092)
  store %NxVal %t7093, ptr @nx__g___main____acc1
  %t7094 = load %NxVal, ptr @nx__g___main____acc1
  %t7095 = add i64 20, 0
  %t7096 = extractvalue %NxVal %t7094, 1
  %t7097 = sub i64 %t7096, %t7095
  %t7098 = add i64 71, 0
  %t7099 = sub i64 %t7097, %t7098
  %t7100 = load %NxVal, ptr @nx__g___main____i1
  %t7101 = extractvalue %NxVal %t7100, 1
  %t7102 = add i64 %t7099, %t7101
  %t7103 = add i64 65535, 0
  %t7104 = and i64 %t7102, %t7103
  %t7105 = call %NxVal @nx_int(i64 %t7104)
  store %NxVal %t7105, ptr @nx__g___main____acc1
  %t7106 = load %NxVal, ptr @nx__g___main____acc1
  %t7107 = add i64 40, 0
  %t7108 = extractvalue %NxVal %t7106, 1
  %t7109 = sub i64 %t7108, %t7107
  %t7110 = add i64 34, 0
  %t7111 = sub i64 %t7109, %t7110
  %t7112 = load %NxVal, ptr @nx__g___main____i1
  %t7113 = extractvalue %NxVal %t7112, 1
  %t7114 = add i64 %t7111, %t7113
  %t7115 = add i64 65535, 0
  %t7116 = and i64 %t7114, %t7115
  %t7117 = call %NxVal @nx_int(i64 %t7116)
  store %NxVal %t7117, ptr @nx__g___main____acc1
  %t7118 = load %NxVal, ptr @nx__g___main____acc1
  %t7119 = add i64 18, 0
  %t7120 = extractvalue %NxVal %t7118, 1
  %t7121 = mul i64 %t7120, %t7119
  %t7122 = add i64 38, 0
  %t7123 = mul i64 %t7121, %t7122
  %t7124 = load %NxVal, ptr @nx__g___main____i1
  %t7125 = extractvalue %NxVal %t7124, 1
  %t7126 = add i64 %t7123, %t7125
  %t7127 = add i64 65535, 0
  %t7128 = and i64 %t7126, %t7127
  %t7129 = call %NxVal @nx_int(i64 %t7128)
  store %NxVal %t7129, ptr @nx__g___main____acc1
  %t7130 = load %NxVal, ptr @nx__g___main____i1
  %t7131 = add i64 1, 0
  %t7132 = extractvalue %NxVal %t7130, 1
  %t7133 = add i64 %t7132, %t7131
  %t7134 = call %NxVal @nx_int(i64 %t7133)
  store %NxVal %t7134, ptr @nx__g___main____i1
  br label %wcond6
wend8:
  %t7135 = load %NxVal, ptr @nx__g___main____total
  %t7136 = load %NxVal, ptr @nx__g___main____acc1
  %t7137 = extractvalue %NxVal %t7135, 1
  %t7138 = extractvalue %NxVal %t7136, 1
  %t7139 = add i64 %t7137, %t7138
  %t7140 = add i64 65535, 0
  %t7141 = and i64 %t7139, %t7140
  %t7142 = call %NxVal @nx_int(i64 %t7141)
  store %NxVal %t7142, ptr @nx__g___main____total
  %t7143 = add i64 0, 0
  %t7144 = call %NxVal @nx_int(i64 %t7143)
  store %NxVal %t7144, ptr @nx__g___main____i2
  %t7145 = add i64 0, 0
  %t7146 = call %NxVal @nx_int(i64 %t7145)
  store %NxVal %t7146, ptr @nx__g___main____acc2
  br label %wcond9
wcond9:
  %t7147 = load %NxVal, ptr @nx__g___main____i2
  %t7148 = add i64 3, 0
  %t7149 = extractvalue %NxVal %t7147, 1
  %t7150 = icmp slt i64 %t7149, %t7148
  br i1 %t7150, label %wbody10, label %wend11
wbody10:
  %t7151 = load %NxVal, ptr @nx__g___main____acc2
  %t7152 = load %NxVal, ptr @nx__g___main____c
  %t7153 = load %NxVal, ptr @nx__g___main____i2
  %t7154 = add i64 2, 0
  %t7155 = extractvalue %NxVal %t7153, 1
  %t7156 = add i64 %t7155, %t7154
  %t7158 = getelementptr [2 x %NxVal], ptr %t7157, i64 0, i64 0
  store %NxVal %t7152, ptr %t7158
  %t7159 = call %NxVal @nx_int(i64 %t7156)
  %t7160 = getelementptr [2 x %NxVal], ptr %t7157, i64 0, i64 1
  store %NxVal %t7159, ptr %t7160
  %t7161 = getelementptr [2 x %NxVal], ptr %t7157, i64 0, i64 0
  %t7162 = call %NxVal @nx__m_2____main____Cell__m2(ptr %t7161, i64 2)
  %t7163 = extractvalue %NxVal %t7162, 1
  %t7164 = extractvalue %NxVal %t7151, 1
  %t7165 = add i64 %t7164, %t7163
  %t7166 = add i64 65535, 0
  %t7167 = and i64 %t7165, %t7166
  %t7168 = call %NxVal @nx_int(i64 %t7167)
  store %NxVal %t7168, ptr @nx__g___main____acc2
  %t7169 = load %NxVal, ptr @nx__g___main____acc2
  %t7170 = add i64 28, 0
  %t7171 = extractvalue %NxVal %t7169, 1
  %t7172 = and i64 %t7171, %t7170
  %t7173 = add i64 47, 0
  %t7174 = load %NxVal, ptr @nx__g___main____i2
  %t7175 = extractvalue %NxVal %t7174, 1
  %t7176 = add i64 %t7173, %t7175
  %t7177 = and i64 %t7172, %t7176
  %t7178 = add i64 65535, 0
  %t7179 = and i64 %t7177, %t7178
  %t7180 = call %NxVal @nx_int(i64 %t7179)
  store %NxVal %t7180, ptr @nx__g___main____acc2
  %t7181 = load %NxVal, ptr @nx__g___main____acc2
  %t7182 = add i64 95, 0
  %t7183 = extractvalue %NxVal %t7181, 1
  %t7184 = and i64 %t7183, %t7182
  %t7185 = add i64 85, 0
  %t7186 = load %NxVal, ptr @nx__g___main____i2
  %t7187 = extractvalue %NxVal %t7186, 1
  %t7188 = add i64 %t7185, %t7187
  %t7189 = and i64 %t7184, %t7188
  %t7190 = add i64 65535, 0
  %t7191 = and i64 %t7189, %t7190
  %t7192 = call %NxVal @nx_int(i64 %t7191)
  store %NxVal %t7192, ptr @nx__g___main____acc2
  %t7193 = load %NxVal, ptr @nx__g___main____acc2
  %t7194 = add i64 4, 0
  %t7195 = extractvalue %NxVal %t7193, 1
  %t7196 = add i64 %t7195, %t7194
  %t7197 = add i64 88, 0
  %t7198 = add i64 %t7196, %t7197
  %t7199 = load %NxVal, ptr @nx__g___main____i2
  %t7200 = extractvalue %NxVal %t7199, 1
  %t7201 = add i64 %t7198, %t7200
  %t7202 = add i64 65535, 0
  %t7203 = and i64 %t7201, %t7202
  %t7204 = call %NxVal @nx_int(i64 %t7203)
  store %NxVal %t7204, ptr @nx__g___main____acc2
  %t7205 = load %NxVal, ptr @nx__g___main____acc2
  %t7206 = add i64 10, 0
  %t7207 = extractvalue %NxVal %t7205, 1
  %t7208 = call i64 @nx_mod_i64(i64 %t7207, i64 %t7206)
  %t7209 = add i64 11, 0
  %t7210 = call i64 @nx_mod_i64(i64 %t7208, i64 %t7209)
  %t7211 = load %NxVal, ptr @nx__g___main____i2
  %t7212 = extractvalue %NxVal %t7211, 1
  %t7213 = add i64 %t7210, %t7212
  %t7214 = add i64 65535, 0
  %t7215 = and i64 %t7213, %t7214
  %t7216 = call %NxVal @nx_int(i64 %t7215)
  store %NxVal %t7216, ptr @nx__g___main____acc2
  %t7217 = load %NxVal, ptr @nx__g___main____i2
  %t7218 = add i64 1, 0
  %t7219 = extractvalue %NxVal %t7217, 1
  %t7220 = add i64 %t7219, %t7218
  %t7221 = call %NxVal @nx_int(i64 %t7220)
  store %NxVal %t7221, ptr @nx__g___main____i2
  br label %wcond9
wend11:
  %t7222 = load %NxVal, ptr @nx__g___main____total
  %t7223 = load %NxVal, ptr @nx__g___main____acc2
  %t7224 = extractvalue %NxVal %t7222, 1
  %t7225 = extractvalue %NxVal %t7223, 1
  %t7226 = add i64 %t7224, %t7225
  %t7227 = add i64 65535, 0
  %t7228 = and i64 %t7226, %t7227
  %t7229 = call %NxVal @nx_int(i64 %t7228)
  store %NxVal %t7229, ptr @nx__g___main____total
  %t7230 = add i64 0, 0
  %t7231 = call %NxVal @nx_int(i64 %t7230)
  store %NxVal %t7231, ptr @nx__g___main____i3
  %t7232 = add i64 0, 0
  %t7233 = call %NxVal @nx_int(i64 %t7232)
  store %NxVal %t7233, ptr @nx__g___main____acc3
  br label %wcond12
wcond12:
  %t7234 = load %NxVal, ptr @nx__g___main____i3
  %t7235 = add i64 3, 0
  %t7236 = extractvalue %NxVal %t7234, 1
  %t7237 = icmp slt i64 %t7236, %t7235
  br i1 %t7237, label %wbody13, label %wend14
wbody13:
  %t7238 = load %NxVal, ptr @nx__g___main____acc3
  %t7239 = load %NxVal, ptr @nx__g___main____c
  %t7240 = load %NxVal, ptr @nx__g___main____i3
  %t7241 = add i64 3, 0
  %t7242 = extractvalue %NxVal %t7240, 1
  %t7243 = add i64 %t7242, %t7241
  %t7245 = getelementptr [2 x %NxVal], ptr %t7244, i64 0, i64 0
  store %NxVal %t7239, ptr %t7245
  %t7246 = call %NxVal @nx_int(i64 %t7243)
  %t7247 = getelementptr [2 x %NxVal], ptr %t7244, i64 0, i64 1
  store %NxVal %t7246, ptr %t7247
  %t7248 = getelementptr [2 x %NxVal], ptr %t7244, i64 0, i64 0
  %t7249 = call %NxVal @nx__m_2____main____Cell__m3(ptr %t7248, i64 2)
  %t7250 = extractvalue %NxVal %t7249, 1
  %t7251 = extractvalue %NxVal %t7238, 1
  %t7252 = add i64 %t7251, %t7250
  %t7253 = add i64 65535, 0
  %t7254 = and i64 %t7252, %t7253
  %t7255 = call %NxVal @nx_int(i64 %t7254)
  store %NxVal %t7255, ptr @nx__g___main____acc3
  %t7256 = load %NxVal, ptr @nx__g___main____acc3
  %t7257 = add i64 78, 0
  %t7258 = extractvalue %NxVal %t7256, 1
  %t7259 = and i64 %t7258, %t7257
  %t7260 = add i64 89, 0
  %t7261 = load %NxVal, ptr @nx__g___main____i3
  %t7262 = extractvalue %NxVal %t7261, 1
  %t7263 = add i64 %t7260, %t7262
  %t7264 = and i64 %t7259, %t7263
  %t7265 = add i64 65535, 0
  %t7266 = and i64 %t7264, %t7265
  %t7267 = call %NxVal @nx_int(i64 %t7266)
  store %NxVal %t7267, ptr @nx__g___main____acc3
  %t7268 = load %NxVal, ptr @nx__g___main____acc3
  %t7269 = add i64 71, 0
  %t7270 = extractvalue %NxVal %t7268, 1
  %t7271 = or i64 %t7270, %t7269
  %t7272 = add i64 23, 0
  %t7273 = load %NxVal, ptr @nx__g___main____i3
  %t7274 = extractvalue %NxVal %t7273, 1
  %t7275 = add i64 %t7272, %t7274
  %t7276 = or i64 %t7271, %t7275
  %t7277 = add i64 65535, 0
  %t7278 = and i64 %t7276, %t7277
  %t7279 = call %NxVal @nx_int(i64 %t7278)
  store %NxVal %t7279, ptr @nx__g___main____acc3
  %t7280 = load %NxVal, ptr @nx__g___main____acc3
  %t7281 = add i64 75, 0
  %t7282 = extractvalue %NxVal %t7280, 1
  %t7283 = call i64 @nx_mod_i64(i64 %t7282, i64 %t7281)
  %t7284 = add i64 71, 0
  %t7285 = call i64 @nx_mod_i64(i64 %t7283, i64 %t7284)
  %t7286 = load %NxVal, ptr @nx__g___main____i3
  %t7287 = extractvalue %NxVal %t7286, 1
  %t7288 = add i64 %t7285, %t7287
  %t7289 = add i64 65535, 0
  %t7290 = and i64 %t7288, %t7289
  %t7291 = call %NxVal @nx_int(i64 %t7290)
  store %NxVal %t7291, ptr @nx__g___main____acc3
  %t7292 = load %NxVal, ptr @nx__g___main____acc3
  %t7293 = add i64 81, 0
  %t7294 = extractvalue %NxVal %t7292, 1
  %t7295 = xor i64 %t7294, %t7293
  %t7296 = add i64 76, 0
  %t7297 = load %NxVal, ptr @nx__g___main____i3
  %t7298 = extractvalue %NxVal %t7297, 1
  %t7299 = add i64 %t7296, %t7298
  %t7300 = xor i64 %t7295, %t7299
  %t7301 = add i64 65535, 0
  %t7302 = and i64 %t7300, %t7301
  %t7303 = call %NxVal @nx_int(i64 %t7302)
  store %NxVal %t7303, ptr @nx__g___main____acc3
  %t7304 = load %NxVal, ptr @nx__g___main____i3
  %t7305 = add i64 1, 0
  %t7306 = extractvalue %NxVal %t7304, 1
  %t7307 = add i64 %t7306, %t7305
  %t7308 = call %NxVal @nx_int(i64 %t7307)
  store %NxVal %t7308, ptr @nx__g___main____i3
  br label %wcond12
wend14:
  %t7309 = load %NxVal, ptr @nx__g___main____total
  %t7310 = load %NxVal, ptr @nx__g___main____acc3
  %t7311 = extractvalue %NxVal %t7309, 1
  %t7312 = extractvalue %NxVal %t7310, 1
  %t7313 = add i64 %t7311, %t7312
  %t7314 = add i64 65535, 0
  %t7315 = and i64 %t7313, %t7314
  %t7316 = call %NxVal @nx_int(i64 %t7315)
  store %NxVal %t7316, ptr @nx__g___main____total
  %t7317 = add i64 0, 0
  %t7318 = call %NxVal @nx_int(i64 %t7317)
  store %NxVal %t7318, ptr @nx__g___main____i4
  %t7319 = add i64 0, 0
  %t7320 = call %NxVal @nx_int(i64 %t7319)
  store %NxVal %t7320, ptr @nx__g___main____acc4
  br label %wcond15
wcond15:
  %t7321 = load %NxVal, ptr @nx__g___main____i4
  %t7322 = add i64 3, 0
  %t7323 = extractvalue %NxVal %t7321, 1
  %t7324 = icmp slt i64 %t7323, %t7322
  br i1 %t7324, label %wbody16, label %wend17
wbody16:
  %t7325 = load %NxVal, ptr @nx__g___main____acc4
  %t7326 = load %NxVal, ptr @nx__g___main____c
  %t7327 = load %NxVal, ptr @nx__g___main____i4
  %t7328 = add i64 4, 0
  %t7329 = extractvalue %NxVal %t7327, 1
  %t7330 = add i64 %t7329, %t7328
  %t7332 = getelementptr [2 x %NxVal], ptr %t7331, i64 0, i64 0
  store %NxVal %t7326, ptr %t7332
  %t7333 = call %NxVal @nx_int(i64 %t7330)
  %t7334 = getelementptr [2 x %NxVal], ptr %t7331, i64 0, i64 1
  store %NxVal %t7333, ptr %t7334
  %t7335 = getelementptr [2 x %NxVal], ptr %t7331, i64 0, i64 0
  %t7336 = call %NxVal @nx__m_2____main____Cell__m4(ptr %t7335, i64 2)
  %t7337 = extractvalue %NxVal %t7336, 1
  %t7338 = extractvalue %NxVal %t7325, 1
  %t7339 = add i64 %t7338, %t7337
  %t7340 = add i64 65535, 0
  %t7341 = and i64 %t7339, %t7340
  %t7342 = call %NxVal @nx_int(i64 %t7341)
  store %NxVal %t7342, ptr @nx__g___main____acc4
  %t7343 = load %NxVal, ptr @nx__g___main____acc4
  %t7344 = add i64 20, 0
  %t7345 = extractvalue %NxVal %t7343, 1
  %t7346 = xor i64 %t7345, %t7344
  %t7347 = add i64 56, 0
  %t7348 = load %NxVal, ptr @nx__g___main____i4
  %t7349 = extractvalue %NxVal %t7348, 1
  %t7350 = add i64 %t7347, %t7349
  %t7351 = xor i64 %t7346, %t7350
  %t7352 = add i64 65535, 0
  %t7353 = and i64 %t7351, %t7352
  %t7354 = call %NxVal @nx_int(i64 %t7353)
  store %NxVal %t7354, ptr @nx__g___main____acc4
  %t7355 = load %NxVal, ptr @nx__g___main____acc4
  %t7356 = add i64 40, 0
  %t7357 = extractvalue %NxVal %t7355, 1
  %t7358 = or i64 %t7357, %t7356
  %t7359 = add i64 78, 0
  %t7360 = load %NxVal, ptr @nx__g___main____i4
  %t7361 = extractvalue %NxVal %t7360, 1
  %t7362 = add i64 %t7359, %t7361
  %t7363 = or i64 %t7358, %t7362
  %t7364 = add i64 65535, 0
  %t7365 = and i64 %t7363, %t7364
  %t7366 = call %NxVal @nx_int(i64 %t7365)
  store %NxVal %t7366, ptr @nx__g___main____acc4
  %t7367 = load %NxVal, ptr @nx__g___main____acc4
  %t7368 = add i64 22, 0
  %t7369 = extractvalue %NxVal %t7367, 1
  %t7370 = xor i64 %t7369, %t7368
  %t7371 = add i64 80, 0
  %t7372 = load %NxVal, ptr @nx__g___main____i4
  %t7373 = extractvalue %NxVal %t7372, 1
  %t7374 = add i64 %t7371, %t7373
  %t7375 = xor i64 %t7370, %t7374
  %t7376 = add i64 65535, 0
  %t7377 = and i64 %t7375, %t7376
  %t7378 = call %NxVal @nx_int(i64 %t7377)
  store %NxVal %t7378, ptr @nx__g___main____acc4
  %t7379 = load %NxVal, ptr @nx__g___main____acc4
  %t7380 = add i64 31, 0
  %t7381 = extractvalue %NxVal %t7379, 1
  %t7382 = mul i64 %t7381, %t7380
  %t7383 = add i64 88, 0
  %t7384 = mul i64 %t7382, %t7383
  %t7385 = load %NxVal, ptr @nx__g___main____i4
  %t7386 = extractvalue %NxVal %t7385, 1
  %t7387 = add i64 %t7384, %t7386
  %t7388 = add i64 65535, 0
  %t7389 = and i64 %t7387, %t7388
  %t7390 = call %NxVal @nx_int(i64 %t7389)
  store %NxVal %t7390, ptr @nx__g___main____acc4
  %t7391 = load %NxVal, ptr @nx__g___main____i4
  %t7392 = add i64 1, 0
  %t7393 = extractvalue %NxVal %t7391, 1
  %t7394 = add i64 %t7393, %t7392
  %t7395 = call %NxVal @nx_int(i64 %t7394)
  store %NxVal %t7395, ptr @nx__g___main____i4
  br label %wcond15
wend17:
  %t7396 = load %NxVal, ptr @nx__g___main____total
  %t7397 = load %NxVal, ptr @nx__g___main____acc4
  %t7398 = extractvalue %NxVal %t7396, 1
  %t7399 = extractvalue %NxVal %t7397, 1
  %t7400 = add i64 %t7398, %t7399
  %t7401 = add i64 65535, 0
  %t7402 = and i64 %t7400, %t7401
  %t7403 = call %NxVal @nx_int(i64 %t7402)
  store %NxVal %t7403, ptr @nx__g___main____total
  %t7404 = add i64 0, 0
  %t7405 = call %NxVal @nx_int(i64 %t7404)
  store %NxVal %t7405, ptr @nx__g___main____i5
  %t7406 = add i64 0, 0
  %t7407 = call %NxVal @nx_int(i64 %t7406)
  store %NxVal %t7407, ptr @nx__g___main____acc5
  br label %wcond18
wcond18:
  %t7408 = load %NxVal, ptr @nx__g___main____i5
  %t7409 = add i64 3, 0
  %t7410 = extractvalue %NxVal %t7408, 1
  %t7411 = icmp slt i64 %t7410, %t7409
  br i1 %t7411, label %wbody19, label %wend20
wbody19:
  %t7412 = load %NxVal, ptr @nx__g___main____acc5
  %t7413 = load %NxVal, ptr @nx__g___main____c
  %t7414 = load %NxVal, ptr @nx__g___main____i5
  %t7415 = add i64 5, 0
  %t7416 = extractvalue %NxVal %t7414, 1
  %t7417 = add i64 %t7416, %t7415
  %t7419 = getelementptr [2 x %NxVal], ptr %t7418, i64 0, i64 0
  store %NxVal %t7413, ptr %t7419
  %t7420 = call %NxVal @nx_int(i64 %t7417)
  %t7421 = getelementptr [2 x %NxVal], ptr %t7418, i64 0, i64 1
  store %NxVal %t7420, ptr %t7421
  %t7422 = getelementptr [2 x %NxVal], ptr %t7418, i64 0, i64 0
  %t7423 = call %NxVal @nx__m_2____main____Cell__m5(ptr %t7422, i64 2)
  %t7424 = extractvalue %NxVal %t7423, 1
  %t7425 = extractvalue %NxVal %t7412, 1
  %t7426 = add i64 %t7425, %t7424
  %t7427 = add i64 65535, 0
  %t7428 = and i64 %t7426, %t7427
  %t7429 = call %NxVal @nx_int(i64 %t7428)
  store %NxVal %t7429, ptr @nx__g___main____acc5
  %t7430 = load %NxVal, ptr @nx__g___main____acc5
  %t7431 = add i64 22, 0
  %t7432 = extractvalue %NxVal %t7430, 1
  %t7433 = and i64 %t7432, %t7431
  %t7434 = add i64 75, 0
  %t7435 = load %NxVal, ptr @nx__g___main____i5
  %t7436 = extractvalue %NxVal %t7435, 1
  %t7437 = add i64 %t7434, %t7436
  %t7438 = and i64 %t7433, %t7437
  %t7439 = add i64 65535, 0
  %t7440 = and i64 %t7438, %t7439
  %t7441 = call %NxVal @nx_int(i64 %t7440)
  store %NxVal %t7441, ptr @nx__g___main____acc5
  %t7442 = load %NxVal, ptr @nx__g___main____acc5
  %t7443 = add i64 33, 0
  %t7444 = extractvalue %NxVal %t7442, 1
  %t7445 = and i64 %t7444, %t7443
  %t7446 = add i64 75, 0
  %t7447 = load %NxVal, ptr @nx__g___main____i5
  %t7448 = extractvalue %NxVal %t7447, 1
  %t7449 = add i64 %t7446, %t7448
  %t7450 = and i64 %t7445, %t7449
  %t7451 = add i64 65535, 0
  %t7452 = and i64 %t7450, %t7451
  %t7453 = call %NxVal @nx_int(i64 %t7452)
  store %NxVal %t7453, ptr @nx__g___main____acc5
  %t7454 = load %NxVal, ptr @nx__g___main____acc5
  %t7455 = add i64 75, 0
  %t7456 = extractvalue %NxVal %t7454, 1
  %t7457 = sub i64 %t7456, %t7455
  %t7458 = add i64 60, 0
  %t7459 = sub i64 %t7457, %t7458
  %t7460 = load %NxVal, ptr @nx__g___main____i5
  %t7461 = extractvalue %NxVal %t7460, 1
  %t7462 = add i64 %t7459, %t7461
  %t7463 = add i64 65535, 0
  %t7464 = and i64 %t7462, %t7463
  %t7465 = call %NxVal @nx_int(i64 %t7464)
  store %NxVal %t7465, ptr @nx__g___main____acc5
  %t7466 = load %NxVal, ptr @nx__g___main____acc5
  %t7467 = add i64 84, 0
  %t7468 = extractvalue %NxVal %t7466, 1
  %t7469 = sub i64 %t7468, %t7467
  %t7470 = add i64 6, 0
  %t7471 = sub i64 %t7469, %t7470
  %t7472 = load %NxVal, ptr @nx__g___main____i5
  %t7473 = extractvalue %NxVal %t7472, 1
  %t7474 = add i64 %t7471, %t7473
  %t7475 = add i64 65535, 0
  %t7476 = and i64 %t7474, %t7475
  %t7477 = call %NxVal @nx_int(i64 %t7476)
  store %NxVal %t7477, ptr @nx__g___main____acc5
  %t7478 = load %NxVal, ptr @nx__g___main____i5
  %t7479 = add i64 1, 0
  %t7480 = extractvalue %NxVal %t7478, 1
  %t7481 = add i64 %t7480, %t7479
  %t7482 = call %NxVal @nx_int(i64 %t7481)
  store %NxVal %t7482, ptr @nx__g___main____i5
  br label %wcond18
wend20:
  %t7483 = load %NxVal, ptr @nx__g___main____total
  %t7484 = load %NxVal, ptr @nx__g___main____acc5
  %t7485 = extractvalue %NxVal %t7483, 1
  %t7486 = extractvalue %NxVal %t7484, 1
  %t7487 = add i64 %t7485, %t7486
  %t7488 = add i64 65535, 0
  %t7489 = and i64 %t7487, %t7488
  %t7490 = call %NxVal @nx_int(i64 %t7489)
  store %NxVal %t7490, ptr @nx__g___main____total
  %t7491 = add i64 0, 0
  %t7492 = call %NxVal @nx_int(i64 %t7491)
  store %NxVal %t7492, ptr @nx__g___main____i6
  %t7493 = add i64 0, 0
  %t7494 = call %NxVal @nx_int(i64 %t7493)
  store %NxVal %t7494, ptr @nx__g___main____acc6
  br label %wcond21
wcond21:
  %t7495 = load %NxVal, ptr @nx__g___main____i6
  %t7496 = add i64 3, 0
  %t7497 = extractvalue %NxVal %t7495, 1
  %t7498 = icmp slt i64 %t7497, %t7496
  br i1 %t7498, label %wbody22, label %wend23
wbody22:
  %t7499 = load %NxVal, ptr @nx__g___main____acc6
  %t7500 = load %NxVal, ptr @nx__g___main____c
  %t7501 = load %NxVal, ptr @nx__g___main____i6
  %t7502 = add i64 6, 0
  %t7503 = extractvalue %NxVal %t7501, 1
  %t7504 = add i64 %t7503, %t7502
  %t7506 = getelementptr [2 x %NxVal], ptr %t7505, i64 0, i64 0
  store %NxVal %t7500, ptr %t7506
  %t7507 = call %NxVal @nx_int(i64 %t7504)
  %t7508 = getelementptr [2 x %NxVal], ptr %t7505, i64 0, i64 1
  store %NxVal %t7507, ptr %t7508
  %t7509 = getelementptr [2 x %NxVal], ptr %t7505, i64 0, i64 0
  %t7510 = call %NxVal @nx__m_2____main____Cell__m6(ptr %t7509, i64 2)
  %t7511 = extractvalue %NxVal %t7510, 1
  %t7512 = extractvalue %NxVal %t7499, 1
  %t7513 = add i64 %t7512, %t7511
  %t7514 = add i64 65535, 0
  %t7515 = and i64 %t7513, %t7514
  %t7516 = call %NxVal @nx_int(i64 %t7515)
  store %NxVal %t7516, ptr @nx__g___main____acc6
  %t7517 = load %NxVal, ptr @nx__g___main____acc6
  %t7518 = add i64 23, 0
  %t7519 = extractvalue %NxVal %t7517, 1
  %t7520 = or i64 %t7519, %t7518
  %t7521 = add i64 58, 0
  %t7522 = load %NxVal, ptr @nx__g___main____i6
  %t7523 = extractvalue %NxVal %t7522, 1
  %t7524 = add i64 %t7521, %t7523
  %t7525 = or i64 %t7520, %t7524
  %t7526 = add i64 65535, 0
  %t7527 = and i64 %t7525, %t7526
  %t7528 = call %NxVal @nx_int(i64 %t7527)
  store %NxVal %t7528, ptr @nx__g___main____acc6
  %t7529 = load %NxVal, ptr @nx__g___main____acc6
  %t7530 = add i64 97, 0
  %t7531 = extractvalue %NxVal %t7529, 1
  %t7532 = call i64 @nx_mod_i64(i64 %t7531, i64 %t7530)
  %t7533 = add i64 78, 0
  %t7534 = call i64 @nx_mod_i64(i64 %t7532, i64 %t7533)
  %t7535 = load %NxVal, ptr @nx__g___main____i6
  %t7536 = extractvalue %NxVal %t7535, 1
  %t7537 = add i64 %t7534, %t7536
  %t7538 = add i64 65535, 0
  %t7539 = and i64 %t7537, %t7538
  %t7540 = call %NxVal @nx_int(i64 %t7539)
  store %NxVal %t7540, ptr @nx__g___main____acc6
  %t7541 = load %NxVal, ptr @nx__g___main____acc6
  %t7542 = add i64 72, 0
  %t7543 = extractvalue %NxVal %t7541, 1
  %t7544 = add i64 %t7543, %t7542
  %t7545 = add i64 26, 0
  %t7546 = add i64 %t7544, %t7545
  %t7547 = load %NxVal, ptr @nx__g___main____i6
  %t7548 = extractvalue %NxVal %t7547, 1
  %t7549 = add i64 %t7546, %t7548
  %t7550 = add i64 65535, 0
  %t7551 = and i64 %t7549, %t7550
  %t7552 = call %NxVal @nx_int(i64 %t7551)
  store %NxVal %t7552, ptr @nx__g___main____acc6
  %t7553 = load %NxVal, ptr @nx__g___main____acc6
  %t7554 = add i64 33, 0
  %t7555 = extractvalue %NxVal %t7553, 1
  %t7556 = call i64 @nx_mod_i64(i64 %t7555, i64 %t7554)
  %t7557 = add i64 18, 0
  %t7558 = call i64 @nx_mod_i64(i64 %t7556, i64 %t7557)
  %t7559 = load %NxVal, ptr @nx__g___main____i6
  %t7560 = extractvalue %NxVal %t7559, 1
  %t7561 = add i64 %t7558, %t7560
  %t7562 = add i64 65535, 0
  %t7563 = and i64 %t7561, %t7562
  %t7564 = call %NxVal @nx_int(i64 %t7563)
  store %NxVal %t7564, ptr @nx__g___main____acc6
  %t7565 = load %NxVal, ptr @nx__g___main____i6
  %t7566 = add i64 1, 0
  %t7567 = extractvalue %NxVal %t7565, 1
  %t7568 = add i64 %t7567, %t7566
  %t7569 = call %NxVal @nx_int(i64 %t7568)
  store %NxVal %t7569, ptr @nx__g___main____i6
  br label %wcond21
wend23:
  %t7570 = load %NxVal, ptr @nx__g___main____total
  %t7571 = load %NxVal, ptr @nx__g___main____acc6
  %t7572 = extractvalue %NxVal %t7570, 1
  %t7573 = extractvalue %NxVal %t7571, 1
  %t7574 = add i64 %t7572, %t7573
  %t7575 = add i64 65535, 0
  %t7576 = and i64 %t7574, %t7575
  %t7577 = call %NxVal @nx_int(i64 %t7576)
  store %NxVal %t7577, ptr @nx__g___main____total
  %t7578 = add i64 0, 0
  %t7579 = call %NxVal @nx_int(i64 %t7578)
  store %NxVal %t7579, ptr @nx__g___main____i7
  %t7580 = add i64 0, 0
  %t7581 = call %NxVal @nx_int(i64 %t7580)
  store %NxVal %t7581, ptr @nx__g___main____acc7
  br label %wcond24
wcond24:
  %t7582 = load %NxVal, ptr @nx__g___main____i7
  %t7583 = add i64 3, 0
  %t7584 = extractvalue %NxVal %t7582, 1
  %t7585 = icmp slt i64 %t7584, %t7583
  br i1 %t7585, label %wbody25, label %wend26
wbody25:
  %t7586 = load %NxVal, ptr @nx__g___main____acc7
  %t7587 = load %NxVal, ptr @nx__g___main____c
  %t7588 = load %NxVal, ptr @nx__g___main____i7
  %t7589 = add i64 7, 0
  %t7590 = extractvalue %NxVal %t7588, 1
  %t7591 = add i64 %t7590, %t7589
  %t7593 = getelementptr [2 x %NxVal], ptr %t7592, i64 0, i64 0
  store %NxVal %t7587, ptr %t7593
  %t7594 = call %NxVal @nx_int(i64 %t7591)
  %t7595 = getelementptr [2 x %NxVal], ptr %t7592, i64 0, i64 1
  store %NxVal %t7594, ptr %t7595
  %t7596 = getelementptr [2 x %NxVal], ptr %t7592, i64 0, i64 0
  %t7597 = call %NxVal @nx__m_2____main____Cell__m7(ptr %t7596, i64 2)
  %t7598 = extractvalue %NxVal %t7597, 1
  %t7599 = extractvalue %NxVal %t7586, 1
  %t7600 = add i64 %t7599, %t7598
  %t7601 = add i64 65535, 0
  %t7602 = and i64 %t7600, %t7601
  %t7603 = call %NxVal @nx_int(i64 %t7602)
  store %NxVal %t7603, ptr @nx__g___main____acc7
  %t7604 = load %NxVal, ptr @nx__g___main____acc7
  %t7605 = add i64 13, 0
  %t7606 = extractvalue %NxVal %t7604, 1
  %t7607 = and i64 %t7606, %t7605
  %t7608 = add i64 76, 0
  %t7609 = load %NxVal, ptr @nx__g___main____i7
  %t7610 = extractvalue %NxVal %t7609, 1
  %t7611 = add i64 %t7608, %t7610
  %t7612 = and i64 %t7607, %t7611
  %t7613 = add i64 65535, 0
  %t7614 = and i64 %t7612, %t7613
  %t7615 = call %NxVal @nx_int(i64 %t7614)
  store %NxVal %t7615, ptr @nx__g___main____acc7
  %t7616 = load %NxVal, ptr @nx__g___main____acc7
  %t7617 = add i64 19, 0
  %t7618 = extractvalue %NxVal %t7616, 1
  %t7619 = add i64 %t7618, %t7617
  %t7620 = add i64 32, 0
  %t7621 = add i64 %t7619, %t7620
  %t7622 = load %NxVal, ptr @nx__g___main____i7
  %t7623 = extractvalue %NxVal %t7622, 1
  %t7624 = add i64 %t7621, %t7623
  %t7625 = add i64 65535, 0
  %t7626 = and i64 %t7624, %t7625
  %t7627 = call %NxVal @nx_int(i64 %t7626)
  store %NxVal %t7627, ptr @nx__g___main____acc7
  %t7628 = load %NxVal, ptr @nx__g___main____acc7
  %t7629 = add i64 85, 0
  %t7630 = extractvalue %NxVal %t7628, 1
  %t7631 = mul i64 %t7630, %t7629
  %t7632 = add i64 32, 0
  %t7633 = mul i64 %t7631, %t7632
  %t7634 = load %NxVal, ptr @nx__g___main____i7
  %t7635 = extractvalue %NxVal %t7634, 1
  %t7636 = add i64 %t7633, %t7635
  %t7637 = add i64 65535, 0
  %t7638 = and i64 %t7636, %t7637
  %t7639 = call %NxVal @nx_int(i64 %t7638)
  store %NxVal %t7639, ptr @nx__g___main____acc7
  %t7640 = load %NxVal, ptr @nx__g___main____acc7
  %t7641 = add i64 5, 0
  %t7642 = extractvalue %NxVal %t7640, 1
  %t7643 = call i64 @nx_mod_i64(i64 %t7642, i64 %t7641)
  %t7644 = add i64 84, 0
  %t7645 = call i64 @nx_mod_i64(i64 %t7643, i64 %t7644)
  %t7646 = load %NxVal, ptr @nx__g___main____i7
  %t7647 = extractvalue %NxVal %t7646, 1
  %t7648 = add i64 %t7645, %t7647
  %t7649 = add i64 65535, 0
  %t7650 = and i64 %t7648, %t7649
  %t7651 = call %NxVal @nx_int(i64 %t7650)
  store %NxVal %t7651, ptr @nx__g___main____acc7
  %t7652 = load %NxVal, ptr @nx__g___main____i7
  %t7653 = add i64 1, 0
  %t7654 = extractvalue %NxVal %t7652, 1
  %t7655 = add i64 %t7654, %t7653
  %t7656 = call %NxVal @nx_int(i64 %t7655)
  store %NxVal %t7656, ptr @nx__g___main____i7
  br label %wcond24
wend26:
  %t7657 = load %NxVal, ptr @nx__g___main____total
  %t7658 = load %NxVal, ptr @nx__g___main____acc7
  %t7659 = extractvalue %NxVal %t7657, 1
  %t7660 = extractvalue %NxVal %t7658, 1
  %t7661 = add i64 %t7659, %t7660
  %t7662 = add i64 65535, 0
  %t7663 = and i64 %t7661, %t7662
  %t7664 = call %NxVal @nx_int(i64 %t7663)
  store %NxVal %t7664, ptr @nx__g___main____total
  %t7665 = add i64 0, 0
  %t7666 = call %NxVal @nx_int(i64 %t7665)
  store %NxVal %t7666, ptr @nx__g___main____i8
  %t7667 = add i64 0, 0
  %t7668 = call %NxVal @nx_int(i64 %t7667)
  store %NxVal %t7668, ptr @nx__g___main____acc8
  br label %wcond27
wcond27:
  %t7669 = load %NxVal, ptr @nx__g___main____i8
  %t7670 = add i64 3, 0
  %t7671 = extractvalue %NxVal %t7669, 1
  %t7672 = icmp slt i64 %t7671, %t7670
  br i1 %t7672, label %wbody28, label %wend29
wbody28:
  %t7673 = load %NxVal, ptr @nx__g___main____acc8
  %t7674 = load %NxVal, ptr @nx__g___main____c
  %t7675 = load %NxVal, ptr @nx__g___main____i8
  %t7676 = add i64 8, 0
  %t7677 = extractvalue %NxVal %t7675, 1
  %t7678 = add i64 %t7677, %t7676
  %t7680 = getelementptr [2 x %NxVal], ptr %t7679, i64 0, i64 0
  store %NxVal %t7674, ptr %t7680
  %t7681 = call %NxVal @nx_int(i64 %t7678)
  %t7682 = getelementptr [2 x %NxVal], ptr %t7679, i64 0, i64 1
  store %NxVal %t7681, ptr %t7682
  %t7683 = getelementptr [2 x %NxVal], ptr %t7679, i64 0, i64 0
  %t7684 = call %NxVal @nx__m_2____main____Cell__m8(ptr %t7683, i64 2)
  %t7685 = extractvalue %NxVal %t7684, 1
  %t7686 = extractvalue %NxVal %t7673, 1
  %t7687 = add i64 %t7686, %t7685
  %t7688 = add i64 65535, 0
  %t7689 = and i64 %t7687, %t7688
  %t7690 = call %NxVal @nx_int(i64 %t7689)
  store %NxVal %t7690, ptr @nx__g___main____acc8
  %t7691 = load %NxVal, ptr @nx__g___main____acc8
  %t7692 = add i64 93, 0
  %t7693 = extractvalue %NxVal %t7691, 1
  %t7694 = xor i64 %t7693, %t7692
  %t7695 = add i64 37, 0
  %t7696 = load %NxVal, ptr @nx__g___main____i8
  %t7697 = extractvalue %NxVal %t7696, 1
  %t7698 = add i64 %t7695, %t7697
  %t7699 = xor i64 %t7694, %t7698
  %t7700 = add i64 65535, 0
  %t7701 = and i64 %t7699, %t7700
  %t7702 = call %NxVal @nx_int(i64 %t7701)
  store %NxVal %t7702, ptr @nx__g___main____acc8
  %t7703 = load %NxVal, ptr @nx__g___main____acc8
  %t7704 = add i64 16, 0
  %t7705 = extractvalue %NxVal %t7703, 1
  %t7706 = sub i64 %t7705, %t7704
  %t7707 = add i64 63, 0
  %t7708 = sub i64 %t7706, %t7707
  %t7709 = load %NxVal, ptr @nx__g___main____i8
  %t7710 = extractvalue %NxVal %t7709, 1
  %t7711 = add i64 %t7708, %t7710
  %t7712 = add i64 65535, 0
  %t7713 = and i64 %t7711, %t7712
  %t7714 = call %NxVal @nx_int(i64 %t7713)
  store %NxVal %t7714, ptr @nx__g___main____acc8
  %t7715 = load %NxVal, ptr @nx__g___main____acc8
  %t7716 = add i64 60, 0
  %t7717 = extractvalue %NxVal %t7715, 1
  %t7718 = or i64 %t7717, %t7716
  %t7719 = add i64 17, 0
  %t7720 = load %NxVal, ptr @nx__g___main____i8
  %t7721 = extractvalue %NxVal %t7720, 1
  %t7722 = add i64 %t7719, %t7721
  %t7723 = or i64 %t7718, %t7722
  %t7724 = add i64 65535, 0
  %t7725 = and i64 %t7723, %t7724
  %t7726 = call %NxVal @nx_int(i64 %t7725)
  store %NxVal %t7726, ptr @nx__g___main____acc8
  %t7727 = load %NxVal, ptr @nx__g___main____acc8
  %t7728 = add i64 60, 0
  %t7729 = extractvalue %NxVal %t7727, 1
  %t7730 = call i64 @nx_mod_i64(i64 %t7729, i64 %t7728)
  %t7731 = add i64 49, 0
  %t7732 = call i64 @nx_mod_i64(i64 %t7730, i64 %t7731)
  %t7733 = load %NxVal, ptr @nx__g___main____i8
  %t7734 = extractvalue %NxVal %t7733, 1
  %t7735 = add i64 %t7732, %t7734
  %t7736 = add i64 65535, 0
  %t7737 = and i64 %t7735, %t7736
  %t7738 = call %NxVal @nx_int(i64 %t7737)
  store %NxVal %t7738, ptr @nx__g___main____acc8
  %t7739 = load %NxVal, ptr @nx__g___main____i8
  %t7740 = add i64 1, 0
  %t7741 = extractvalue %NxVal %t7739, 1
  %t7742 = add i64 %t7741, %t7740
  %t7743 = call %NxVal @nx_int(i64 %t7742)
  store %NxVal %t7743, ptr @nx__g___main____i8
  br label %wcond27
wend29:
  %t7744 = load %NxVal, ptr @nx__g___main____total
  %t7745 = load %NxVal, ptr @nx__g___main____acc8
  %t7746 = extractvalue %NxVal %t7744, 1
  %t7747 = extractvalue %NxVal %t7745, 1
  %t7748 = add i64 %t7746, %t7747
  %t7749 = add i64 65535, 0
  %t7750 = and i64 %t7748, %t7749
  %t7751 = call %NxVal @nx_int(i64 %t7750)
  store %NxVal %t7751, ptr @nx__g___main____total
  %t7752 = add i64 0, 0
  %t7753 = call %NxVal @nx_int(i64 %t7752)
  store %NxVal %t7753, ptr @nx__g___main____i9
  %t7754 = add i64 0, 0
  %t7755 = call %NxVal @nx_int(i64 %t7754)
  store %NxVal %t7755, ptr @nx__g___main____acc9
  br label %wcond30
wcond30:
  %t7756 = load %NxVal, ptr @nx__g___main____i9
  %t7757 = add i64 3, 0
  %t7758 = extractvalue %NxVal %t7756, 1
  %t7759 = icmp slt i64 %t7758, %t7757
  br i1 %t7759, label %wbody31, label %wend32
wbody31:
  %t7760 = load %NxVal, ptr @nx__g___main____acc9
  %t7761 = load %NxVal, ptr @nx__g___main____c
  %t7762 = load %NxVal, ptr @nx__g___main____i9
  %t7763 = add i64 9, 0
  %t7764 = extractvalue %NxVal %t7762, 1
  %t7765 = add i64 %t7764, %t7763
  %t7767 = getelementptr [2 x %NxVal], ptr %t7766, i64 0, i64 0
  store %NxVal %t7761, ptr %t7767
  %t7768 = call %NxVal @nx_int(i64 %t7765)
  %t7769 = getelementptr [2 x %NxVal], ptr %t7766, i64 0, i64 1
  store %NxVal %t7768, ptr %t7769
  %t7770 = getelementptr [2 x %NxVal], ptr %t7766, i64 0, i64 0
  %t7771 = call %NxVal @nx__m_2____main____Cell__m9(ptr %t7770, i64 2)
  %t7772 = extractvalue %NxVal %t7771, 1
  %t7773 = extractvalue %NxVal %t7760, 1
  %t7774 = add i64 %t7773, %t7772
  %t7775 = add i64 65535, 0
  %t7776 = and i64 %t7774, %t7775
  %t7777 = call %NxVal @nx_int(i64 %t7776)
  store %NxVal %t7777, ptr @nx__g___main____acc9
  %t7778 = load %NxVal, ptr @nx__g___main____acc9
  %t7779 = add i64 52, 0
  %t7780 = extractvalue %NxVal %t7778, 1
  %t7781 = sub i64 %t7780, %t7779
  %t7782 = add i64 38, 0
  %t7783 = sub i64 %t7781, %t7782
  %t7784 = load %NxVal, ptr @nx__g___main____i9
  %t7785 = extractvalue %NxVal %t7784, 1
  %t7786 = add i64 %t7783, %t7785
  %t7787 = add i64 65535, 0
  %t7788 = and i64 %t7786, %t7787
  %t7789 = call %NxVal @nx_int(i64 %t7788)
  store %NxVal %t7789, ptr @nx__g___main____acc9
  %t7790 = load %NxVal, ptr @nx__g___main____acc9
  %t7791 = add i64 68, 0
  %t7792 = extractvalue %NxVal %t7790, 1
  %t7793 = call i64 @nx_mod_i64(i64 %t7792, i64 %t7791)
  %t7794 = add i64 86, 0
  %t7795 = call i64 @nx_mod_i64(i64 %t7793, i64 %t7794)
  %t7796 = load %NxVal, ptr @nx__g___main____i9
  %t7797 = extractvalue %NxVal %t7796, 1
  %t7798 = add i64 %t7795, %t7797
  %t7799 = add i64 65535, 0
  %t7800 = and i64 %t7798, %t7799
  %t7801 = call %NxVal @nx_int(i64 %t7800)
  store %NxVal %t7801, ptr @nx__g___main____acc9
  %t7802 = load %NxVal, ptr @nx__g___main____acc9
  %t7803 = add i64 36, 0
  %t7804 = extractvalue %NxVal %t7802, 1
  %t7805 = add i64 %t7804, %t7803
  %t7806 = add i64 59, 0
  %t7807 = add i64 %t7805, %t7806
  %t7808 = load %NxVal, ptr @nx__g___main____i9
  %t7809 = extractvalue %NxVal %t7808, 1
  %t7810 = add i64 %t7807, %t7809
  %t7811 = add i64 65535, 0
  %t7812 = and i64 %t7810, %t7811
  %t7813 = call %NxVal @nx_int(i64 %t7812)
  store %NxVal %t7813, ptr @nx__g___main____acc9
  %t7814 = load %NxVal, ptr @nx__g___main____acc9
  %t7815 = add i64 53, 0
  %t7816 = extractvalue %NxVal %t7814, 1
  %t7817 = and i64 %t7816, %t7815
  %t7818 = add i64 52, 0
  %t7819 = load %NxVal, ptr @nx__g___main____i9
  %t7820 = extractvalue %NxVal %t7819, 1
  %t7821 = add i64 %t7818, %t7820
  %t7822 = and i64 %t7817, %t7821
  %t7823 = add i64 65535, 0
  %t7824 = and i64 %t7822, %t7823
  %t7825 = call %NxVal @nx_int(i64 %t7824)
  store %NxVal %t7825, ptr @nx__g___main____acc9
  %t7826 = load %NxVal, ptr @nx__g___main____i9
  %t7827 = add i64 1, 0
  %t7828 = extractvalue %NxVal %t7826, 1
  %t7829 = add i64 %t7828, %t7827
  %t7830 = call %NxVal @nx_int(i64 %t7829)
  store %NxVal %t7830, ptr @nx__g___main____i9
  br label %wcond30
wend32:
  %t7831 = load %NxVal, ptr @nx__g___main____total
  %t7832 = load %NxVal, ptr @nx__g___main____acc9
  %t7833 = extractvalue %NxVal %t7831, 1
  %t7834 = extractvalue %NxVal %t7832, 1
  %t7835 = add i64 %t7833, %t7834
  %t7836 = add i64 65535, 0
  %t7837 = and i64 %t7835, %t7836
  %t7838 = call %NxVal @nx_int(i64 %t7837)
  store %NxVal %t7838, ptr @nx__g___main____total
  %t7839 = add i64 0, 0
  %t7840 = call %NxVal @nx_int(i64 %t7839)
  store %NxVal %t7840, ptr @nx__g___main____i10
  %t7841 = add i64 0, 0
  %t7842 = call %NxVal @nx_int(i64 %t7841)
  store %NxVal %t7842, ptr @nx__g___main____acc10
  br label %wcond33
wcond33:
  %t7843 = load %NxVal, ptr @nx__g___main____i10
  %t7844 = add i64 3, 0
  %t7845 = extractvalue %NxVal %t7843, 1
  %t7846 = icmp slt i64 %t7845, %t7844
  br i1 %t7846, label %wbody34, label %wend35
wbody34:
  %t7847 = load %NxVal, ptr @nx__g___main____acc10
  %t7848 = load %NxVal, ptr @nx__g___main____c
  %t7849 = load %NxVal, ptr @nx__g___main____i10
  %t7850 = add i64 10, 0
  %t7851 = extractvalue %NxVal %t7849, 1
  %t7852 = add i64 %t7851, %t7850
  %t7854 = getelementptr [2 x %NxVal], ptr %t7853, i64 0, i64 0
  store %NxVal %t7848, ptr %t7854
  %t7855 = call %NxVal @nx_int(i64 %t7852)
  %t7856 = getelementptr [2 x %NxVal], ptr %t7853, i64 0, i64 1
  store %NxVal %t7855, ptr %t7856
  %t7857 = getelementptr [2 x %NxVal], ptr %t7853, i64 0, i64 0
  %t7858 = call %NxVal @nx__m_3____main____Cell__m10(ptr %t7857, i64 2)
  %t7859 = extractvalue %NxVal %t7858, 1
  %t7860 = extractvalue %NxVal %t7847, 1
  %t7861 = add i64 %t7860, %t7859
  %t7862 = add i64 65535, 0
  %t7863 = and i64 %t7861, %t7862
  %t7864 = call %NxVal @nx_int(i64 %t7863)
  store %NxVal %t7864, ptr @nx__g___main____acc10
  %t7865 = load %NxVal, ptr @nx__g___main____acc10
  %t7866 = add i64 79, 0
  %t7867 = extractvalue %NxVal %t7865, 1
  %t7868 = or i64 %t7867, %t7866
  %t7869 = add i64 71, 0
  %t7870 = load %NxVal, ptr @nx__g___main____i10
  %t7871 = extractvalue %NxVal %t7870, 1
  %t7872 = add i64 %t7869, %t7871
  %t7873 = or i64 %t7868, %t7872
  %t7874 = add i64 65535, 0
  %t7875 = and i64 %t7873, %t7874
  %t7876 = call %NxVal @nx_int(i64 %t7875)
  store %NxVal %t7876, ptr @nx__g___main____acc10
  %t7877 = load %NxVal, ptr @nx__g___main____acc10
  %t7878 = add i64 1, 0
  %t7879 = extractvalue %NxVal %t7877, 1
  %t7880 = and i64 %t7879, %t7878
  %t7881 = add i64 40, 0
  %t7882 = load %NxVal, ptr @nx__g___main____i10
  %t7883 = extractvalue %NxVal %t7882, 1
  %t7884 = add i64 %t7881, %t7883
  %t7885 = and i64 %t7880, %t7884
  %t7886 = add i64 65535, 0
  %t7887 = and i64 %t7885, %t7886
  %t7888 = call %NxVal @nx_int(i64 %t7887)
  store %NxVal %t7888, ptr @nx__g___main____acc10
  %t7889 = load %NxVal, ptr @nx__g___main____acc10
  %t7890 = add i64 32, 0
  %t7891 = extractvalue %NxVal %t7889, 1
  %t7892 = call i64 @nx_mod_i64(i64 %t7891, i64 %t7890)
  %t7893 = add i64 25, 0
  %t7894 = call i64 @nx_mod_i64(i64 %t7892, i64 %t7893)
  %t7895 = load %NxVal, ptr @nx__g___main____i10
  %t7896 = extractvalue %NxVal %t7895, 1
  %t7897 = add i64 %t7894, %t7896
  %t7898 = add i64 65535, 0
  %t7899 = and i64 %t7897, %t7898
  %t7900 = call %NxVal @nx_int(i64 %t7899)
  store %NxVal %t7900, ptr @nx__g___main____acc10
  %t7901 = load %NxVal, ptr @nx__g___main____acc10
  %t7902 = add i64 7, 0
  %t7903 = extractvalue %NxVal %t7901, 1
  %t7904 = sub i64 %t7903, %t7902
  %t7905 = add i64 63, 0
  %t7906 = sub i64 %t7904, %t7905
  %t7907 = load %NxVal, ptr @nx__g___main____i10
  %t7908 = extractvalue %NxVal %t7907, 1
  %t7909 = add i64 %t7906, %t7908
  %t7910 = add i64 65535, 0
  %t7911 = and i64 %t7909, %t7910
  %t7912 = call %NxVal @nx_int(i64 %t7911)
  store %NxVal %t7912, ptr @nx__g___main____acc10
  %t7913 = load %NxVal, ptr @nx__g___main____i10
  %t7914 = add i64 1, 0
  %t7915 = extractvalue %NxVal %t7913, 1
  %t7916 = add i64 %t7915, %t7914
  %t7917 = call %NxVal @nx_int(i64 %t7916)
  store %NxVal %t7917, ptr @nx__g___main____i10
  br label %wcond33
wend35:
  %t7918 = load %NxVal, ptr @nx__g___main____total
  %t7919 = load %NxVal, ptr @nx__g___main____acc10
  %t7920 = extractvalue %NxVal %t7918, 1
  %t7921 = extractvalue %NxVal %t7919, 1
  %t7922 = add i64 %t7920, %t7921
  %t7923 = add i64 65535, 0
  %t7924 = and i64 %t7922, %t7923
  %t7925 = call %NxVal @nx_int(i64 %t7924)
  store %NxVal %t7925, ptr @nx__g___main____total
  %t7926 = add i64 0, 0
  %t7927 = call %NxVal @nx_int(i64 %t7926)
  store %NxVal %t7927, ptr @nx__g___main____i11
  %t7928 = add i64 0, 0
  %t7929 = call %NxVal @nx_int(i64 %t7928)
  store %NxVal %t7929, ptr @nx__g___main____acc11
  br label %wcond36
wcond36:
  %t7930 = load %NxVal, ptr @nx__g___main____i11
  %t7931 = add i64 3, 0
  %t7932 = extractvalue %NxVal %t7930, 1
  %t7933 = icmp slt i64 %t7932, %t7931
  br i1 %t7933, label %wbody37, label %wend38
wbody37:
  %t7934 = load %NxVal, ptr @nx__g___main____acc11
  %t7935 = load %NxVal, ptr @nx__g___main____c
  %t7936 = load %NxVal, ptr @nx__g___main____i11
  %t7937 = add i64 11, 0
  %t7938 = extractvalue %NxVal %t7936, 1
  %t7939 = add i64 %t7938, %t7937
  %t7941 = getelementptr [2 x %NxVal], ptr %t7940, i64 0, i64 0
  store %NxVal %t7935, ptr %t7941
  %t7942 = call %NxVal @nx_int(i64 %t7939)
  %t7943 = getelementptr [2 x %NxVal], ptr %t7940, i64 0, i64 1
  store %NxVal %t7942, ptr %t7943
  %t7944 = getelementptr [2 x %NxVal], ptr %t7940, i64 0, i64 0
  %t7945 = call %NxVal @nx__m_3____main____Cell__m11(ptr %t7944, i64 2)
  %t7946 = extractvalue %NxVal %t7945, 1
  %t7947 = extractvalue %NxVal %t7934, 1
  %t7948 = add i64 %t7947, %t7946
  %t7949 = add i64 65535, 0
  %t7950 = and i64 %t7948, %t7949
  %t7951 = call %NxVal @nx_int(i64 %t7950)
  store %NxVal %t7951, ptr @nx__g___main____acc11
  %t7952 = load %NxVal, ptr @nx__g___main____acc11
  %t7953 = add i64 84, 0
  %t7954 = extractvalue %NxVal %t7952, 1
  %t7955 = or i64 %t7954, %t7953
  %t7956 = add i64 67, 0
  %t7957 = load %NxVal, ptr @nx__g___main____i11
  %t7958 = extractvalue %NxVal %t7957, 1
  %t7959 = add i64 %t7956, %t7958
  %t7960 = or i64 %t7955, %t7959
  %t7961 = add i64 65535, 0
  %t7962 = and i64 %t7960, %t7961
  %t7963 = call %NxVal @nx_int(i64 %t7962)
  store %NxVal %t7963, ptr @nx__g___main____acc11
  %t7964 = load %NxVal, ptr @nx__g___main____acc11
  %t7965 = add i64 47, 0
  %t7966 = extractvalue %NxVal %t7964, 1
  %t7967 = or i64 %t7966, %t7965
  %t7968 = add i64 46, 0
  %t7969 = load %NxVal, ptr @nx__g___main____i11
  %t7970 = extractvalue %NxVal %t7969, 1
  %t7971 = add i64 %t7968, %t7970
  %t7972 = or i64 %t7967, %t7971
  %t7973 = add i64 65535, 0
  %t7974 = and i64 %t7972, %t7973
  %t7975 = call %NxVal @nx_int(i64 %t7974)
  store %NxVal %t7975, ptr @nx__g___main____acc11
  %t7976 = load %NxVal, ptr @nx__g___main____acc11
  %t7977 = add i64 16, 0
  %t7978 = extractvalue %NxVal %t7976, 1
  %t7979 = add i64 %t7978, %t7977
  %t7980 = add i64 80, 0
  %t7981 = add i64 %t7979, %t7980
  %t7982 = load %NxVal, ptr @nx__g___main____i11
  %t7983 = extractvalue %NxVal %t7982, 1
  %t7984 = add i64 %t7981, %t7983
  %t7985 = add i64 65535, 0
  %t7986 = and i64 %t7984, %t7985
  %t7987 = call %NxVal @nx_int(i64 %t7986)
  store %NxVal %t7987, ptr @nx__g___main____acc11
  %t7988 = load %NxVal, ptr @nx__g___main____acc11
  %t7989 = add i64 67, 0
  %t7990 = extractvalue %NxVal %t7988, 1
  %t7991 = and i64 %t7990, %t7989
  %t7992 = add i64 58, 0
  %t7993 = load %NxVal, ptr @nx__g___main____i11
  %t7994 = extractvalue %NxVal %t7993, 1
  %t7995 = add i64 %t7992, %t7994
  %t7996 = and i64 %t7991, %t7995
  %t7997 = add i64 65535, 0
  %t7998 = and i64 %t7996, %t7997
  %t7999 = call %NxVal @nx_int(i64 %t7998)
  store %NxVal %t7999, ptr @nx__g___main____acc11
  %t8000 = load %NxVal, ptr @nx__g___main____i11
  %t8001 = add i64 1, 0
  %t8002 = extractvalue %NxVal %t8000, 1
  %t8003 = add i64 %t8002, %t8001
  %t8004 = call %NxVal @nx_int(i64 %t8003)
  store %NxVal %t8004, ptr @nx__g___main____i11
  br label %wcond36
wend38:
  %t8005 = load %NxVal, ptr @nx__g___main____total
  %t8006 = load %NxVal, ptr @nx__g___main____acc11
  %t8007 = extractvalue %NxVal %t8005, 1
  %t8008 = extractvalue %NxVal %t8006, 1
  %t8009 = add i64 %t8007, %t8008
  %t8010 = add i64 65535, 0
  %t8011 = and i64 %t8009, %t8010
  %t8012 = call %NxVal @nx_int(i64 %t8011)
  store %NxVal %t8012, ptr @nx__g___main____total
  %t8013 = add i64 0, 0
  %t8014 = call %NxVal @nx_int(i64 %t8013)
  store %NxVal %t8014, ptr @nx__g___main____i12
  %t8015 = add i64 0, 0
  %t8016 = call %NxVal @nx_int(i64 %t8015)
  store %NxVal %t8016, ptr @nx__g___main____acc12
  br label %wcond39
wcond39:
  %t8017 = load %NxVal, ptr @nx__g___main____i12
  %t8018 = add i64 3, 0
  %t8019 = extractvalue %NxVal %t8017, 1
  %t8020 = icmp slt i64 %t8019, %t8018
  br i1 %t8020, label %wbody40, label %wend41
wbody40:
  %t8021 = load %NxVal, ptr @nx__g___main____acc12
  %t8022 = load %NxVal, ptr @nx__g___main____c
  %t8023 = load %NxVal, ptr @nx__g___main____i12
  %t8024 = add i64 12, 0
  %t8025 = extractvalue %NxVal %t8023, 1
  %t8026 = add i64 %t8025, %t8024
  %t8028 = getelementptr [2 x %NxVal], ptr %t8027, i64 0, i64 0
  store %NxVal %t8022, ptr %t8028
  %t8029 = call %NxVal @nx_int(i64 %t8026)
  %t8030 = getelementptr [2 x %NxVal], ptr %t8027, i64 0, i64 1
  store %NxVal %t8029, ptr %t8030
  %t8031 = getelementptr [2 x %NxVal], ptr %t8027, i64 0, i64 0
  %t8032 = call %NxVal @nx__m_3____main____Cell__m12(ptr %t8031, i64 2)
  %t8033 = extractvalue %NxVal %t8032, 1
  %t8034 = extractvalue %NxVal %t8021, 1
  %t8035 = add i64 %t8034, %t8033
  %t8036 = add i64 65535, 0
  %t8037 = and i64 %t8035, %t8036
  %t8038 = call %NxVal @nx_int(i64 %t8037)
  store %NxVal %t8038, ptr @nx__g___main____acc12
  %t8039 = load %NxVal, ptr @nx__g___main____acc12
  %t8040 = add i64 4, 0
  %t8041 = extractvalue %NxVal %t8039, 1
  %t8042 = or i64 %t8041, %t8040
  %t8043 = add i64 36, 0
  %t8044 = load %NxVal, ptr @nx__g___main____i12
  %t8045 = extractvalue %NxVal %t8044, 1
  %t8046 = add i64 %t8043, %t8045
  %t8047 = or i64 %t8042, %t8046
  %t8048 = add i64 65535, 0
  %t8049 = and i64 %t8047, %t8048
  %t8050 = call %NxVal @nx_int(i64 %t8049)
  store %NxVal %t8050, ptr @nx__g___main____acc12
  %t8051 = load %NxVal, ptr @nx__g___main____acc12
  %t8052 = add i64 9, 0
  %t8053 = extractvalue %NxVal %t8051, 1
  %t8054 = xor i64 %t8053, %t8052
  %t8055 = add i64 31, 0
  %t8056 = load %NxVal, ptr @nx__g___main____i12
  %t8057 = extractvalue %NxVal %t8056, 1
  %t8058 = add i64 %t8055, %t8057
  %t8059 = xor i64 %t8054, %t8058
  %t8060 = add i64 65535, 0
  %t8061 = and i64 %t8059, %t8060
  %t8062 = call %NxVal @nx_int(i64 %t8061)
  store %NxVal %t8062, ptr @nx__g___main____acc12
  %t8063 = load %NxVal, ptr @nx__g___main____acc12
  %t8064 = add i64 54, 0
  %t8065 = extractvalue %NxVal %t8063, 1
  %t8066 = add i64 %t8065, %t8064
  %t8067 = add i64 20, 0
  %t8068 = add i64 %t8066, %t8067
  %t8069 = load %NxVal, ptr @nx__g___main____i12
  %t8070 = extractvalue %NxVal %t8069, 1
  %t8071 = add i64 %t8068, %t8070
  %t8072 = add i64 65535, 0
  %t8073 = and i64 %t8071, %t8072
  %t8074 = call %NxVal @nx_int(i64 %t8073)
  store %NxVal %t8074, ptr @nx__g___main____acc12
  %t8075 = load %NxVal, ptr @nx__g___main____acc12
  %t8076 = add i64 35, 0
  %t8077 = extractvalue %NxVal %t8075, 1
  %t8078 = call i64 @nx_mod_i64(i64 %t8077, i64 %t8076)
  %t8079 = add i64 20, 0
  %t8080 = call i64 @nx_mod_i64(i64 %t8078, i64 %t8079)
  %t8081 = load %NxVal, ptr @nx__g___main____i12
  %t8082 = extractvalue %NxVal %t8081, 1
  %t8083 = add i64 %t8080, %t8082
  %t8084 = add i64 65535, 0
  %t8085 = and i64 %t8083, %t8084
  %t8086 = call %NxVal @nx_int(i64 %t8085)
  store %NxVal %t8086, ptr @nx__g___main____acc12
  %t8087 = load %NxVal, ptr @nx__g___main____i12
  %t8088 = add i64 1, 0
  %t8089 = extractvalue %NxVal %t8087, 1
  %t8090 = add i64 %t8089, %t8088
  %t8091 = call %NxVal @nx_int(i64 %t8090)
  store %NxVal %t8091, ptr @nx__g___main____i12
  br label %wcond39
wend41:
  %t8092 = load %NxVal, ptr @nx__g___main____total
  %t8093 = load %NxVal, ptr @nx__g___main____acc12
  %t8094 = extractvalue %NxVal %t8092, 1
  %t8095 = extractvalue %NxVal %t8093, 1
  %t8096 = add i64 %t8094, %t8095
  %t8097 = add i64 65535, 0
  %t8098 = and i64 %t8096, %t8097
  %t8099 = call %NxVal @nx_int(i64 %t8098)
  store %NxVal %t8099, ptr @nx__g___main____total
  %t8100 = add i64 0, 0
  %t8101 = call %NxVal @nx_int(i64 %t8100)
  store %NxVal %t8101, ptr @nx__g___main____i13
  %t8102 = add i64 0, 0
  %t8103 = call %NxVal @nx_int(i64 %t8102)
  store %NxVal %t8103, ptr @nx__g___main____acc13
  br label %wcond42
wcond42:
  %t8104 = load %NxVal, ptr @nx__g___main____i13
  %t8105 = add i64 3, 0
  %t8106 = extractvalue %NxVal %t8104, 1
  %t8107 = icmp slt i64 %t8106, %t8105
  br i1 %t8107, label %wbody43, label %wend44
wbody43:
  %t8108 = load %NxVal, ptr @nx__g___main____acc13
  %t8109 = load %NxVal, ptr @nx__g___main____c
  %t8110 = load %NxVal, ptr @nx__g___main____i13
  %t8111 = add i64 13, 0
  %t8112 = extractvalue %NxVal %t8110, 1
  %t8113 = add i64 %t8112, %t8111
  %t8115 = getelementptr [2 x %NxVal], ptr %t8114, i64 0, i64 0
  store %NxVal %t8109, ptr %t8115
  %t8116 = call %NxVal @nx_int(i64 %t8113)
  %t8117 = getelementptr [2 x %NxVal], ptr %t8114, i64 0, i64 1
  store %NxVal %t8116, ptr %t8117
  %t8118 = getelementptr [2 x %NxVal], ptr %t8114, i64 0, i64 0
  %t8119 = call %NxVal @nx__m_3____main____Cell__m13(ptr %t8118, i64 2)
  %t8120 = extractvalue %NxVal %t8119, 1
  %t8121 = extractvalue %NxVal %t8108, 1
  %t8122 = add i64 %t8121, %t8120
  %t8123 = add i64 65535, 0
  %t8124 = and i64 %t8122, %t8123
  %t8125 = call %NxVal @nx_int(i64 %t8124)
  store %NxVal %t8125, ptr @nx__g___main____acc13
  %t8126 = load %NxVal, ptr @nx__g___main____acc13
  %t8127 = add i64 81, 0
  %t8128 = extractvalue %NxVal %t8126, 1
  %t8129 = or i64 %t8128, %t8127
  %t8130 = add i64 53, 0
  %t8131 = load %NxVal, ptr @nx__g___main____i13
  %t8132 = extractvalue %NxVal %t8131, 1
  %t8133 = add i64 %t8130, %t8132
  %t8134 = or i64 %t8129, %t8133
  %t8135 = add i64 65535, 0
  %t8136 = and i64 %t8134, %t8135
  %t8137 = call %NxVal @nx_int(i64 %t8136)
  store %NxVal %t8137, ptr @nx__g___main____acc13
  %t8138 = load %NxVal, ptr @nx__g___main____acc13
  %t8139 = add i64 26, 0
  %t8140 = extractvalue %NxVal %t8138, 1
  %t8141 = sub i64 %t8140, %t8139
  %t8142 = add i64 64, 0
  %t8143 = sub i64 %t8141, %t8142
  %t8144 = load %NxVal, ptr @nx__g___main____i13
  %t8145 = extractvalue %NxVal %t8144, 1
  %t8146 = add i64 %t8143, %t8145
  %t8147 = add i64 65535, 0
  %t8148 = and i64 %t8146, %t8147
  %t8149 = call %NxVal @nx_int(i64 %t8148)
  store %NxVal %t8149, ptr @nx__g___main____acc13
  %t8150 = load %NxVal, ptr @nx__g___main____acc13
  %t8151 = add i64 23, 0
  %t8152 = extractvalue %NxVal %t8150, 1
  %t8153 = xor i64 %t8152, %t8151
  %t8154 = add i64 33, 0
  %t8155 = load %NxVal, ptr @nx__g___main____i13
  %t8156 = extractvalue %NxVal %t8155, 1
  %t8157 = add i64 %t8154, %t8156
  %t8158 = xor i64 %t8153, %t8157
  %t8159 = add i64 65535, 0
  %t8160 = and i64 %t8158, %t8159
  %t8161 = call %NxVal @nx_int(i64 %t8160)
  store %NxVal %t8161, ptr @nx__g___main____acc13
  %t8162 = load %NxVal, ptr @nx__g___main____acc13
  %t8163 = add i64 69, 0
  %t8164 = extractvalue %NxVal %t8162, 1
  %t8165 = mul i64 %t8164, %t8163
  %t8166 = add i64 58, 0
  %t8167 = mul i64 %t8165, %t8166
  %t8168 = load %NxVal, ptr @nx__g___main____i13
  %t8169 = extractvalue %NxVal %t8168, 1
  %t8170 = add i64 %t8167, %t8169
  %t8171 = add i64 65535, 0
  %t8172 = and i64 %t8170, %t8171
  %t8173 = call %NxVal @nx_int(i64 %t8172)
  store %NxVal %t8173, ptr @nx__g___main____acc13
  %t8174 = load %NxVal, ptr @nx__g___main____i13
  %t8175 = add i64 1, 0
  %t8176 = extractvalue %NxVal %t8174, 1
  %t8177 = add i64 %t8176, %t8175
  %t8178 = call %NxVal @nx_int(i64 %t8177)
  store %NxVal %t8178, ptr @nx__g___main____i13
  br label %wcond42
wend44:
  %t8179 = load %NxVal, ptr @nx__g___main____total
  %t8180 = load %NxVal, ptr @nx__g___main____acc13
  %t8181 = extractvalue %NxVal %t8179, 1
  %t8182 = extractvalue %NxVal %t8180, 1
  %t8183 = add i64 %t8181, %t8182
  %t8184 = add i64 65535, 0
  %t8185 = and i64 %t8183, %t8184
  %t8186 = call %NxVal @nx_int(i64 %t8185)
  store %NxVal %t8186, ptr @nx__g___main____total
  %t8187 = add i64 0, 0
  %t8188 = call %NxVal @nx_int(i64 %t8187)
  store %NxVal %t8188, ptr @nx__g___main____i14
  %t8189 = add i64 0, 0
  %t8190 = call %NxVal @nx_int(i64 %t8189)
  store %NxVal %t8190, ptr @nx__g___main____acc14
  br label %wcond45
wcond45:
  %t8191 = load %NxVal, ptr @nx__g___main____i14
  %t8192 = add i64 3, 0
  %t8193 = extractvalue %NxVal %t8191, 1
  %t8194 = icmp slt i64 %t8193, %t8192
  br i1 %t8194, label %wbody46, label %wend47
wbody46:
  %t8195 = load %NxVal, ptr @nx__g___main____acc14
  %t8196 = load %NxVal, ptr @nx__g___main____c
  %t8197 = load %NxVal, ptr @nx__g___main____i14
  %t8198 = add i64 14, 0
  %t8199 = extractvalue %NxVal %t8197, 1
  %t8200 = add i64 %t8199, %t8198
  %t8202 = getelementptr [2 x %NxVal], ptr %t8201, i64 0, i64 0
  store %NxVal %t8196, ptr %t8202
  %t8203 = call %NxVal @nx_int(i64 %t8200)
  %t8204 = getelementptr [2 x %NxVal], ptr %t8201, i64 0, i64 1
  store %NxVal %t8203, ptr %t8204
  %t8205 = getelementptr [2 x %NxVal], ptr %t8201, i64 0, i64 0
  %t8206 = call %NxVal @nx__m_3____main____Cell__m14(ptr %t8205, i64 2)
  %t8207 = extractvalue %NxVal %t8206, 1
  %t8208 = extractvalue %NxVal %t8195, 1
  %t8209 = add i64 %t8208, %t8207
  %t8210 = add i64 65535, 0
  %t8211 = and i64 %t8209, %t8210
  %t8212 = call %NxVal @nx_int(i64 %t8211)
  store %NxVal %t8212, ptr @nx__g___main____acc14
  %t8213 = load %NxVal, ptr @nx__g___main____acc14
  %t8214 = add i64 91, 0
  %t8215 = extractvalue %NxVal %t8213, 1
  %t8216 = add i64 %t8215, %t8214
  %t8217 = add i64 18, 0
  %t8218 = add i64 %t8216, %t8217
  %t8219 = load %NxVal, ptr @nx__g___main____i14
  %t8220 = extractvalue %NxVal %t8219, 1
  %t8221 = add i64 %t8218, %t8220
  %t8222 = add i64 65535, 0
  %t8223 = and i64 %t8221, %t8222
  %t8224 = call %NxVal @nx_int(i64 %t8223)
  store %NxVal %t8224, ptr @nx__g___main____acc14
  %t8225 = load %NxVal, ptr @nx__g___main____acc14
  %t8226 = add i64 49, 0
  %t8227 = extractvalue %NxVal %t8225, 1
  %t8228 = xor i64 %t8227, %t8226
  %t8229 = add i64 59, 0
  %t8230 = load %NxVal, ptr @nx__g___main____i14
  %t8231 = extractvalue %NxVal %t8230, 1
  %t8232 = add i64 %t8229, %t8231
  %t8233 = xor i64 %t8228, %t8232
  %t8234 = add i64 65535, 0
  %t8235 = and i64 %t8233, %t8234
  %t8236 = call %NxVal @nx_int(i64 %t8235)
  store %NxVal %t8236, ptr @nx__g___main____acc14
  %t8237 = load %NxVal, ptr @nx__g___main____acc14
  %t8238 = add i64 31, 0
  %t8239 = extractvalue %NxVal %t8237, 1
  %t8240 = mul i64 %t8239, %t8238
  %t8241 = add i64 30, 0
  %t8242 = mul i64 %t8240, %t8241
  %t8243 = load %NxVal, ptr @nx__g___main____i14
  %t8244 = extractvalue %NxVal %t8243, 1
  %t8245 = add i64 %t8242, %t8244
  %t8246 = add i64 65535, 0
  %t8247 = and i64 %t8245, %t8246
  %t8248 = call %NxVal @nx_int(i64 %t8247)
  store %NxVal %t8248, ptr @nx__g___main____acc14
  %t8249 = load %NxVal, ptr @nx__g___main____acc14
  %t8250 = add i64 48, 0
  %t8251 = extractvalue %NxVal %t8249, 1
  %t8252 = or i64 %t8251, %t8250
  %t8253 = add i64 39, 0
  %t8254 = load %NxVal, ptr @nx__g___main____i14
  %t8255 = extractvalue %NxVal %t8254, 1
  %t8256 = add i64 %t8253, %t8255
  %t8257 = or i64 %t8252, %t8256
  %t8258 = add i64 65535, 0
  %t8259 = and i64 %t8257, %t8258
  %t8260 = call %NxVal @nx_int(i64 %t8259)
  store %NxVal %t8260, ptr @nx__g___main____acc14
  %t8261 = load %NxVal, ptr @nx__g___main____i14
  %t8262 = add i64 1, 0
  %t8263 = extractvalue %NxVal %t8261, 1
  %t8264 = add i64 %t8263, %t8262
  %t8265 = call %NxVal @nx_int(i64 %t8264)
  store %NxVal %t8265, ptr @nx__g___main____i14
  br label %wcond45
wend47:
  %t8266 = load %NxVal, ptr @nx__g___main____total
  %t8267 = load %NxVal, ptr @nx__g___main____acc14
  %t8268 = extractvalue %NxVal %t8266, 1
  %t8269 = extractvalue %NxVal %t8267, 1
  %t8270 = add i64 %t8268, %t8269
  %t8271 = add i64 65535, 0
  %t8272 = and i64 %t8270, %t8271
  %t8273 = call %NxVal @nx_int(i64 %t8272)
  store %NxVal %t8273, ptr @nx__g___main____total
  %t8274 = add i64 0, 0
  %t8275 = call %NxVal @nx_int(i64 %t8274)
  store %NxVal %t8275, ptr @nx__g___main____i15
  %t8276 = add i64 0, 0
  %t8277 = call %NxVal @nx_int(i64 %t8276)
  store %NxVal %t8277, ptr @nx__g___main____acc15
  br label %wcond48
wcond48:
  %t8278 = load %NxVal, ptr @nx__g___main____i15
  %t8279 = add i64 3, 0
  %t8280 = extractvalue %NxVal %t8278, 1
  %t8281 = icmp slt i64 %t8280, %t8279
  br i1 %t8281, label %wbody49, label %wend50
wbody49:
  %t8282 = load %NxVal, ptr @nx__g___main____acc15
  %t8283 = load %NxVal, ptr @nx__g___main____c
  %t8284 = load %NxVal, ptr @nx__g___main____i15
  %t8285 = add i64 15, 0
  %t8286 = extractvalue %NxVal %t8284, 1
  %t8287 = add i64 %t8286, %t8285
  %t8289 = getelementptr [2 x %NxVal], ptr %t8288, i64 0, i64 0
  store %NxVal %t8283, ptr %t8289
  %t8290 = call %NxVal @nx_int(i64 %t8287)
  %t8291 = getelementptr [2 x %NxVal], ptr %t8288, i64 0, i64 1
  store %NxVal %t8290, ptr %t8291
  %t8292 = getelementptr [2 x %NxVal], ptr %t8288, i64 0, i64 0
  %t8293 = call %NxVal @nx__m_3____main____Cell__m15(ptr %t8292, i64 2)
  %t8294 = extractvalue %NxVal %t8293, 1
  %t8295 = extractvalue %NxVal %t8282, 1
  %t8296 = add i64 %t8295, %t8294
  %t8297 = add i64 65535, 0
  %t8298 = and i64 %t8296, %t8297
  %t8299 = call %NxVal @nx_int(i64 %t8298)
  store %NxVal %t8299, ptr @nx__g___main____acc15
  %t8300 = load %NxVal, ptr @nx__g___main____acc15
  %t8301 = add i64 1, 0
  %t8302 = extractvalue %NxVal %t8300, 1
  %t8303 = add i64 %t8302, %t8301
  %t8304 = add i64 3, 0
  %t8305 = add i64 %t8303, %t8304
  %t8306 = load %NxVal, ptr @nx__g___main____i15
  %t8307 = extractvalue %NxVal %t8306, 1
  %t8308 = add i64 %t8305, %t8307
  %t8309 = add i64 65535, 0
  %t8310 = and i64 %t8308, %t8309
  %t8311 = call %NxVal @nx_int(i64 %t8310)
  store %NxVal %t8311, ptr @nx__g___main____acc15
  %t8312 = load %NxVal, ptr @nx__g___main____acc15
  %t8313 = add i64 21, 0
  %t8314 = extractvalue %NxVal %t8312, 1
  %t8315 = sub i64 %t8314, %t8313
  %t8316 = add i64 77, 0
  %t8317 = sub i64 %t8315, %t8316
  %t8318 = load %NxVal, ptr @nx__g___main____i15
  %t8319 = extractvalue %NxVal %t8318, 1
  %t8320 = add i64 %t8317, %t8319
  %t8321 = add i64 65535, 0
  %t8322 = and i64 %t8320, %t8321
  %t8323 = call %NxVal @nx_int(i64 %t8322)
  store %NxVal %t8323, ptr @nx__g___main____acc15
  %t8324 = load %NxVal, ptr @nx__g___main____acc15
  %t8325 = add i64 31, 0
  %t8326 = extractvalue %NxVal %t8324, 1
  %t8327 = and i64 %t8326, %t8325
  %t8328 = add i64 13, 0
  %t8329 = load %NxVal, ptr @nx__g___main____i15
  %t8330 = extractvalue %NxVal %t8329, 1
  %t8331 = add i64 %t8328, %t8330
  %t8332 = and i64 %t8327, %t8331
  %t8333 = add i64 65535, 0
  %t8334 = and i64 %t8332, %t8333
  %t8335 = call %NxVal @nx_int(i64 %t8334)
  store %NxVal %t8335, ptr @nx__g___main____acc15
  %t8336 = load %NxVal, ptr @nx__g___main____acc15
  %t8337 = add i64 29, 0
  %t8338 = extractvalue %NxVal %t8336, 1
  %t8339 = mul i64 %t8338, %t8337
  %t8340 = add i64 64, 0
  %t8341 = mul i64 %t8339, %t8340
  %t8342 = load %NxVal, ptr @nx__g___main____i15
  %t8343 = extractvalue %NxVal %t8342, 1
  %t8344 = add i64 %t8341, %t8343
  %t8345 = add i64 65535, 0
  %t8346 = and i64 %t8344, %t8345
  %t8347 = call %NxVal @nx_int(i64 %t8346)
  store %NxVal %t8347, ptr @nx__g___main____acc15
  %t8348 = load %NxVal, ptr @nx__g___main____i15
  %t8349 = add i64 1, 0
  %t8350 = extractvalue %NxVal %t8348, 1
  %t8351 = add i64 %t8350, %t8349
  %t8352 = call %NxVal @nx_int(i64 %t8351)
  store %NxVal %t8352, ptr @nx__g___main____i15
  br label %wcond48
wend50:
  %t8353 = load %NxVal, ptr @nx__g___main____total
  %t8354 = load %NxVal, ptr @nx__g___main____acc15
  %t8355 = extractvalue %NxVal %t8353, 1
  %t8356 = extractvalue %NxVal %t8354, 1
  %t8357 = add i64 %t8355, %t8356
  %t8358 = add i64 65535, 0
  %t8359 = and i64 %t8357, %t8358
  %t8360 = call %NxVal @nx_int(i64 %t8359)
  store %NxVal %t8360, ptr @nx__g___main____total
  %t8361 = add i64 0, 0
  %t8362 = call %NxVal @nx_int(i64 %t8361)
  store %NxVal %t8362, ptr @nx__g___main____i16
  %t8363 = add i64 0, 0
  %t8364 = call %NxVal @nx_int(i64 %t8363)
  store %NxVal %t8364, ptr @nx__g___main____acc16
  br label %wcond51
wcond51:
  %t8365 = load %NxVal, ptr @nx__g___main____i16
  %t8366 = add i64 3, 0
  %t8367 = extractvalue %NxVal %t8365, 1
  %t8368 = icmp slt i64 %t8367, %t8366
  br i1 %t8368, label %wbody52, label %wend53
wbody52:
  %t8369 = load %NxVal, ptr @nx__g___main____acc16
  %t8370 = load %NxVal, ptr @nx__g___main____c
  %t8371 = load %NxVal, ptr @nx__g___main____i16
  %t8372 = add i64 16, 0
  %t8373 = extractvalue %NxVal %t8371, 1
  %t8374 = add i64 %t8373, %t8372
  %t8376 = getelementptr [2 x %NxVal], ptr %t8375, i64 0, i64 0
  store %NxVal %t8370, ptr %t8376
  %t8377 = call %NxVal @nx_int(i64 %t8374)
  %t8378 = getelementptr [2 x %NxVal], ptr %t8375, i64 0, i64 1
  store %NxVal %t8377, ptr %t8378
  %t8379 = getelementptr [2 x %NxVal], ptr %t8375, i64 0, i64 0
  %t8380 = call %NxVal @nx__m_3____main____Cell__m16(ptr %t8379, i64 2)
  %t8381 = extractvalue %NxVal %t8380, 1
  %t8382 = extractvalue %NxVal %t8369, 1
  %t8383 = add i64 %t8382, %t8381
  %t8384 = add i64 65535, 0
  %t8385 = and i64 %t8383, %t8384
  %t8386 = call %NxVal @nx_int(i64 %t8385)
  store %NxVal %t8386, ptr @nx__g___main____acc16
  %t8387 = load %NxVal, ptr @nx__g___main____acc16
  %t8388 = add i64 71, 0
  %t8389 = extractvalue %NxVal %t8387, 1
  %t8390 = mul i64 %t8389, %t8388
  %t8391 = add i64 54, 0
  %t8392 = mul i64 %t8390, %t8391
  %t8393 = load %NxVal, ptr @nx__g___main____i16
  %t8394 = extractvalue %NxVal %t8393, 1
  %t8395 = add i64 %t8392, %t8394
  %t8396 = add i64 65535, 0
  %t8397 = and i64 %t8395, %t8396
  %t8398 = call %NxVal @nx_int(i64 %t8397)
  store %NxVal %t8398, ptr @nx__g___main____acc16
  %t8399 = load %NxVal, ptr @nx__g___main____acc16
  %t8400 = add i64 87, 0
  %t8401 = extractvalue %NxVal %t8399, 1
  %t8402 = mul i64 %t8401, %t8400
  %t8403 = add i64 3, 0
  %t8404 = mul i64 %t8402, %t8403
  %t8405 = load %NxVal, ptr @nx__g___main____i16
  %t8406 = extractvalue %NxVal %t8405, 1
  %t8407 = add i64 %t8404, %t8406
  %t8408 = add i64 65535, 0
  %t8409 = and i64 %t8407, %t8408
  %t8410 = call %NxVal @nx_int(i64 %t8409)
  store %NxVal %t8410, ptr @nx__g___main____acc16
  %t8411 = load %NxVal, ptr @nx__g___main____acc16
  %t8412 = add i64 24, 0
  %t8413 = extractvalue %NxVal %t8411, 1
  %t8414 = call i64 @nx_mod_i64(i64 %t8413, i64 %t8412)
  %t8415 = add i64 24, 0
  %t8416 = call i64 @nx_mod_i64(i64 %t8414, i64 %t8415)
  %t8417 = load %NxVal, ptr @nx__g___main____i16
  %t8418 = extractvalue %NxVal %t8417, 1
  %t8419 = add i64 %t8416, %t8418
  %t8420 = add i64 65535, 0
  %t8421 = and i64 %t8419, %t8420
  %t8422 = call %NxVal @nx_int(i64 %t8421)
  store %NxVal %t8422, ptr @nx__g___main____acc16
  %t8423 = load %NxVal, ptr @nx__g___main____acc16
  %t8424 = add i64 48, 0
  %t8425 = extractvalue %NxVal %t8423, 1
  %t8426 = sub i64 %t8425, %t8424
  %t8427 = add i64 79, 0
  %t8428 = sub i64 %t8426, %t8427
  %t8429 = load %NxVal, ptr @nx__g___main____i16
  %t8430 = extractvalue %NxVal %t8429, 1
  %t8431 = add i64 %t8428, %t8430
  %t8432 = add i64 65535, 0
  %t8433 = and i64 %t8431, %t8432
  %t8434 = call %NxVal @nx_int(i64 %t8433)
  store %NxVal %t8434, ptr @nx__g___main____acc16
  %t8435 = load %NxVal, ptr @nx__g___main____i16
  %t8436 = add i64 1, 0
  %t8437 = extractvalue %NxVal %t8435, 1
  %t8438 = add i64 %t8437, %t8436
  %t8439 = call %NxVal @nx_int(i64 %t8438)
  store %NxVal %t8439, ptr @nx__g___main____i16
  br label %wcond51
wend53:
  %t8440 = load %NxVal, ptr @nx__g___main____total
  %t8441 = load %NxVal, ptr @nx__g___main____acc16
  %t8442 = extractvalue %NxVal %t8440, 1
  %t8443 = extractvalue %NxVal %t8441, 1
  %t8444 = add i64 %t8442, %t8443
  %t8445 = add i64 65535, 0
  %t8446 = and i64 %t8444, %t8445
  %t8447 = call %NxVal @nx_int(i64 %t8446)
  store %NxVal %t8447, ptr @nx__g___main____total
  %t8448 = add i64 0, 0
  %t8449 = call %NxVal @nx_int(i64 %t8448)
  store %NxVal %t8449, ptr @nx__g___main____i17
  %t8450 = add i64 0, 0
  %t8451 = call %NxVal @nx_int(i64 %t8450)
  store %NxVal %t8451, ptr @nx__g___main____acc17
  br label %wcond54
wcond54:
  %t8452 = load %NxVal, ptr @nx__g___main____i17
  %t8453 = add i64 3, 0
  %t8454 = extractvalue %NxVal %t8452, 1
  %t8455 = icmp slt i64 %t8454, %t8453
  br i1 %t8455, label %wbody55, label %wend56
wbody55:
  %t8456 = load %NxVal, ptr @nx__g___main____acc17
  %t8457 = load %NxVal, ptr @nx__g___main____c
  %t8458 = load %NxVal, ptr @nx__g___main____i17
  %t8459 = add i64 17, 0
  %t8460 = extractvalue %NxVal %t8458, 1
  %t8461 = add i64 %t8460, %t8459
  %t8463 = getelementptr [2 x %NxVal], ptr %t8462, i64 0, i64 0
  store %NxVal %t8457, ptr %t8463
  %t8464 = call %NxVal @nx_int(i64 %t8461)
  %t8465 = getelementptr [2 x %NxVal], ptr %t8462, i64 0, i64 1
  store %NxVal %t8464, ptr %t8465
  %t8466 = getelementptr [2 x %NxVal], ptr %t8462, i64 0, i64 0
  %t8467 = call %NxVal @nx__m_3____main____Cell__m17(ptr %t8466, i64 2)
  %t8468 = extractvalue %NxVal %t8467, 1
  %t8469 = extractvalue %NxVal %t8456, 1
  %t8470 = add i64 %t8469, %t8468
  %t8471 = add i64 65535, 0
  %t8472 = and i64 %t8470, %t8471
  %t8473 = call %NxVal @nx_int(i64 %t8472)
  store %NxVal %t8473, ptr @nx__g___main____acc17
  %t8474 = load %NxVal, ptr @nx__g___main____acc17
  %t8475 = add i64 52, 0
  %t8476 = extractvalue %NxVal %t8474, 1
  %t8477 = add i64 %t8476, %t8475
  %t8478 = add i64 70, 0
  %t8479 = add i64 %t8477, %t8478
  %t8480 = load %NxVal, ptr @nx__g___main____i17
  %t8481 = extractvalue %NxVal %t8480, 1
  %t8482 = add i64 %t8479, %t8481
  %t8483 = add i64 65535, 0
  %t8484 = and i64 %t8482, %t8483
  %t8485 = call %NxVal @nx_int(i64 %t8484)
  store %NxVal %t8485, ptr @nx__g___main____acc17
  %t8486 = load %NxVal, ptr @nx__g___main____acc17
  %t8487 = add i64 70, 0
  %t8488 = extractvalue %NxVal %t8486, 1
  %t8489 = sub i64 %t8488, %t8487
  %t8490 = add i64 64, 0
  %t8491 = sub i64 %t8489, %t8490
  %t8492 = load %NxVal, ptr @nx__g___main____i17
  %t8493 = extractvalue %NxVal %t8492, 1
  %t8494 = add i64 %t8491, %t8493
  %t8495 = add i64 65535, 0
  %t8496 = and i64 %t8494, %t8495
  %t8497 = call %NxVal @nx_int(i64 %t8496)
  store %NxVal %t8497, ptr @nx__g___main____acc17
  %t8498 = load %NxVal, ptr @nx__g___main____acc17
  %t8499 = add i64 22, 0
  %t8500 = extractvalue %NxVal %t8498, 1
  %t8501 = or i64 %t8500, %t8499
  %t8502 = add i64 84, 0
  %t8503 = load %NxVal, ptr @nx__g___main____i17
  %t8504 = extractvalue %NxVal %t8503, 1
  %t8505 = add i64 %t8502, %t8504
  %t8506 = or i64 %t8501, %t8505
  %t8507 = add i64 65535, 0
  %t8508 = and i64 %t8506, %t8507
  %t8509 = call %NxVal @nx_int(i64 %t8508)
  store %NxVal %t8509, ptr @nx__g___main____acc17
  %t8510 = load %NxVal, ptr @nx__g___main____acc17
  %t8511 = add i64 27, 0
  %t8512 = extractvalue %NxVal %t8510, 1
  %t8513 = call i64 @nx_mod_i64(i64 %t8512, i64 %t8511)
  %t8514 = add i64 47, 0
  %t8515 = call i64 @nx_mod_i64(i64 %t8513, i64 %t8514)
  %t8516 = load %NxVal, ptr @nx__g___main____i17
  %t8517 = extractvalue %NxVal %t8516, 1
  %t8518 = add i64 %t8515, %t8517
  %t8519 = add i64 65535, 0
  %t8520 = and i64 %t8518, %t8519
  %t8521 = call %NxVal @nx_int(i64 %t8520)
  store %NxVal %t8521, ptr @nx__g___main____acc17
  %t8522 = load %NxVal, ptr @nx__g___main____i17
  %t8523 = add i64 1, 0
  %t8524 = extractvalue %NxVal %t8522, 1
  %t8525 = add i64 %t8524, %t8523
  %t8526 = call %NxVal @nx_int(i64 %t8525)
  store %NxVal %t8526, ptr @nx__g___main____i17
  br label %wcond54
wend56:
  %t8527 = load %NxVal, ptr @nx__g___main____total
  %t8528 = load %NxVal, ptr @nx__g___main____acc17
  %t8529 = extractvalue %NxVal %t8527, 1
  %t8530 = extractvalue %NxVal %t8528, 1
  %t8531 = add i64 %t8529, %t8530
  %t8532 = add i64 65535, 0
  %t8533 = and i64 %t8531, %t8532
  %t8534 = call %NxVal @nx_int(i64 %t8533)
  store %NxVal %t8534, ptr @nx__g___main____total
  %t8535 = add i64 0, 0
  %t8536 = call %NxVal @nx_int(i64 %t8535)
  store %NxVal %t8536, ptr @nx__g___main____i18
  %t8537 = add i64 0, 0
  %t8538 = call %NxVal @nx_int(i64 %t8537)
  store %NxVal %t8538, ptr @nx__g___main____acc18
  br label %wcond57
wcond57:
  %t8539 = load %NxVal, ptr @nx__g___main____i18
  %t8540 = add i64 3, 0
  %t8541 = extractvalue %NxVal %t8539, 1
  %t8542 = icmp slt i64 %t8541, %t8540
  br i1 %t8542, label %wbody58, label %wend59
wbody58:
  %t8543 = load %NxVal, ptr @nx__g___main____acc18
  %t8544 = load %NxVal, ptr @nx__g___main____c
  %t8545 = load %NxVal, ptr @nx__g___main____i18
  %t8546 = add i64 18, 0
  %t8547 = extractvalue %NxVal %t8545, 1
  %t8548 = add i64 %t8547, %t8546
  %t8550 = getelementptr [2 x %NxVal], ptr %t8549, i64 0, i64 0
  store %NxVal %t8544, ptr %t8550
  %t8551 = call %NxVal @nx_int(i64 %t8548)
  %t8552 = getelementptr [2 x %NxVal], ptr %t8549, i64 0, i64 1
  store %NxVal %t8551, ptr %t8552
  %t8553 = getelementptr [2 x %NxVal], ptr %t8549, i64 0, i64 0
  %t8554 = call %NxVal @nx__m_3____main____Cell__m18(ptr %t8553, i64 2)
  %t8555 = extractvalue %NxVal %t8554, 1
  %t8556 = extractvalue %NxVal %t8543, 1
  %t8557 = add i64 %t8556, %t8555
  %t8558 = add i64 65535, 0
  %t8559 = and i64 %t8557, %t8558
  %t8560 = call %NxVal @nx_int(i64 %t8559)
  store %NxVal %t8560, ptr @nx__g___main____acc18
  %t8561 = load %NxVal, ptr @nx__g___main____acc18
  %t8562 = add i64 18, 0
  %t8563 = extractvalue %NxVal %t8561, 1
  %t8564 = sub i64 %t8563, %t8562
  %t8565 = add i64 68, 0
  %t8566 = sub i64 %t8564, %t8565
  %t8567 = load %NxVal, ptr @nx__g___main____i18
  %t8568 = extractvalue %NxVal %t8567, 1
  %t8569 = add i64 %t8566, %t8568
  %t8570 = add i64 65535, 0
  %t8571 = and i64 %t8569, %t8570
  %t8572 = call %NxVal @nx_int(i64 %t8571)
  store %NxVal %t8572, ptr @nx__g___main____acc18
  %t8573 = load %NxVal, ptr @nx__g___main____acc18
  %t8574 = add i64 5, 0
  %t8575 = extractvalue %NxVal %t8573, 1
  %t8576 = add i64 %t8575, %t8574
  %t8577 = add i64 85, 0
  %t8578 = add i64 %t8576, %t8577
  %t8579 = load %NxVal, ptr @nx__g___main____i18
  %t8580 = extractvalue %NxVal %t8579, 1
  %t8581 = add i64 %t8578, %t8580
  %t8582 = add i64 65535, 0
  %t8583 = and i64 %t8581, %t8582
  %t8584 = call %NxVal @nx_int(i64 %t8583)
  store %NxVal %t8584, ptr @nx__g___main____acc18
  %t8585 = load %NxVal, ptr @nx__g___main____acc18
  %t8586 = add i64 92, 0
  %t8587 = extractvalue %NxVal %t8585, 1
  %t8588 = sub i64 %t8587, %t8586
  %t8589 = add i64 22, 0
  %t8590 = sub i64 %t8588, %t8589
  %t8591 = load %NxVal, ptr @nx__g___main____i18
  %t8592 = extractvalue %NxVal %t8591, 1
  %t8593 = add i64 %t8590, %t8592
  %t8594 = add i64 65535, 0
  %t8595 = and i64 %t8593, %t8594
  %t8596 = call %NxVal @nx_int(i64 %t8595)
  store %NxVal %t8596, ptr @nx__g___main____acc18
  %t8597 = load %NxVal, ptr @nx__g___main____acc18
  %t8598 = add i64 40, 0
  %t8599 = extractvalue %NxVal %t8597, 1
  %t8600 = sub i64 %t8599, %t8598
  %t8601 = add i64 68, 0
  %t8602 = sub i64 %t8600, %t8601
  %t8603 = load %NxVal, ptr @nx__g___main____i18
  %t8604 = extractvalue %NxVal %t8603, 1
  %t8605 = add i64 %t8602, %t8604
  %t8606 = add i64 65535, 0
  %t8607 = and i64 %t8605, %t8606
  %t8608 = call %NxVal @nx_int(i64 %t8607)
  store %NxVal %t8608, ptr @nx__g___main____acc18
  %t8609 = load %NxVal, ptr @nx__g___main____i18
  %t8610 = add i64 1, 0
  %t8611 = extractvalue %NxVal %t8609, 1
  %t8612 = add i64 %t8611, %t8610
  %t8613 = call %NxVal @nx_int(i64 %t8612)
  store %NxVal %t8613, ptr @nx__g___main____i18
  br label %wcond57
wend59:
  %t8614 = load %NxVal, ptr @nx__g___main____total
  %t8615 = load %NxVal, ptr @nx__g___main____acc18
  %t8616 = extractvalue %NxVal %t8614, 1
  %t8617 = extractvalue %NxVal %t8615, 1
  %t8618 = add i64 %t8616, %t8617
  %t8619 = add i64 65535, 0
  %t8620 = and i64 %t8618, %t8619
  %t8621 = call %NxVal @nx_int(i64 %t8620)
  store %NxVal %t8621, ptr @nx__g___main____total
  %t8622 = add i64 0, 0
  %t8623 = call %NxVal @nx_int(i64 %t8622)
  store %NxVal %t8623, ptr @nx__g___main____i19
  %t8624 = add i64 0, 0
  %t8625 = call %NxVal @nx_int(i64 %t8624)
  store %NxVal %t8625, ptr @nx__g___main____acc19
  br label %wcond60
wcond60:
  %t8626 = load %NxVal, ptr @nx__g___main____i19
  %t8627 = add i64 3, 0
  %t8628 = extractvalue %NxVal %t8626, 1
  %t8629 = icmp slt i64 %t8628, %t8627
  br i1 %t8629, label %wbody61, label %wend62
wbody61:
  %t8630 = load %NxVal, ptr @nx__g___main____acc19
  %t8631 = load %NxVal, ptr @nx__g___main____c
  %t8632 = load %NxVal, ptr @nx__g___main____i19
  %t8633 = add i64 19, 0
  %t8634 = extractvalue %NxVal %t8632, 1
  %t8635 = add i64 %t8634, %t8633
  %t8637 = getelementptr [2 x %NxVal], ptr %t8636, i64 0, i64 0
  store %NxVal %t8631, ptr %t8637
  %t8638 = call %NxVal @nx_int(i64 %t8635)
  %t8639 = getelementptr [2 x %NxVal], ptr %t8636, i64 0, i64 1
  store %NxVal %t8638, ptr %t8639
  %t8640 = getelementptr [2 x %NxVal], ptr %t8636, i64 0, i64 0
  %t8641 = call %NxVal @nx__m_3____main____Cell__m19(ptr %t8640, i64 2)
  %t8642 = extractvalue %NxVal %t8641, 1
  %t8643 = extractvalue %NxVal %t8630, 1
  %t8644 = add i64 %t8643, %t8642
  %t8645 = add i64 65535, 0
  %t8646 = and i64 %t8644, %t8645
  %t8647 = call %NxVal @nx_int(i64 %t8646)
  store %NxVal %t8647, ptr @nx__g___main____acc19
  %t8648 = load %NxVal, ptr @nx__g___main____acc19
  %t8649 = add i64 6, 0
  %t8650 = extractvalue %NxVal %t8648, 1
  %t8651 = sub i64 %t8650, %t8649
  %t8652 = add i64 37, 0
  %t8653 = sub i64 %t8651, %t8652
  %t8654 = load %NxVal, ptr @nx__g___main____i19
  %t8655 = extractvalue %NxVal %t8654, 1
  %t8656 = add i64 %t8653, %t8655
  %t8657 = add i64 65535, 0
  %t8658 = and i64 %t8656, %t8657
  %t8659 = call %NxVal @nx_int(i64 %t8658)
  store %NxVal %t8659, ptr @nx__g___main____acc19
  %t8660 = load %NxVal, ptr @nx__g___main____acc19
  %t8661 = add i64 10, 0
  %t8662 = extractvalue %NxVal %t8660, 1
  %t8663 = mul i64 %t8662, %t8661
  %t8664 = add i64 43, 0
  %t8665 = mul i64 %t8663, %t8664
  %t8666 = load %NxVal, ptr @nx__g___main____i19
  %t8667 = extractvalue %NxVal %t8666, 1
  %t8668 = add i64 %t8665, %t8667
  %t8669 = add i64 65535, 0
  %t8670 = and i64 %t8668, %t8669
  %t8671 = call %NxVal @nx_int(i64 %t8670)
  store %NxVal %t8671, ptr @nx__g___main____acc19
  %t8672 = load %NxVal, ptr @nx__g___main____acc19
  %t8673 = add i64 39, 0
  %t8674 = extractvalue %NxVal %t8672, 1
  %t8675 = call i64 @nx_mod_i64(i64 %t8674, i64 %t8673)
  %t8676 = add i64 38, 0
  %t8677 = call i64 @nx_mod_i64(i64 %t8675, i64 %t8676)
  %t8678 = load %NxVal, ptr @nx__g___main____i19
  %t8679 = extractvalue %NxVal %t8678, 1
  %t8680 = add i64 %t8677, %t8679
  %t8681 = add i64 65535, 0
  %t8682 = and i64 %t8680, %t8681
  %t8683 = call %NxVal @nx_int(i64 %t8682)
  store %NxVal %t8683, ptr @nx__g___main____acc19
  %t8684 = load %NxVal, ptr @nx__g___main____acc19
  %t8685 = add i64 1, 0
  %t8686 = extractvalue %NxVal %t8684, 1
  %t8687 = call i64 @nx_mod_i64(i64 %t8686, i64 %t8685)
  %t8688 = add i64 41, 0
  %t8689 = call i64 @nx_mod_i64(i64 %t8687, i64 %t8688)
  %t8690 = load %NxVal, ptr @nx__g___main____i19
  %t8691 = extractvalue %NxVal %t8690, 1
  %t8692 = add i64 %t8689, %t8691
  %t8693 = add i64 65535, 0
  %t8694 = and i64 %t8692, %t8693
  %t8695 = call %NxVal @nx_int(i64 %t8694)
  store %NxVal %t8695, ptr @nx__g___main____acc19
  %t8696 = load %NxVal, ptr @nx__g___main____i19
  %t8697 = add i64 1, 0
  %t8698 = extractvalue %NxVal %t8696, 1
  %t8699 = add i64 %t8698, %t8697
  %t8700 = call %NxVal @nx_int(i64 %t8699)
  store %NxVal %t8700, ptr @nx__g___main____i19
  br label %wcond60
wend62:
  %t8701 = load %NxVal, ptr @nx__g___main____total
  %t8702 = load %NxVal, ptr @nx__g___main____acc19
  %t8703 = extractvalue %NxVal %t8701, 1
  %t8704 = extractvalue %NxVal %t8702, 1
  %t8705 = add i64 %t8703, %t8704
  %t8706 = add i64 65535, 0
  %t8707 = and i64 %t8705, %t8706
  %t8708 = call %NxVal @nx_int(i64 %t8707)
  store %NxVal %t8708, ptr @nx__g___main____total
  %t8709 = add i64 0, 0
  %t8710 = call %NxVal @nx_int(i64 %t8709)
  store %NxVal %t8710, ptr @nx__g___main____i20
  %t8711 = add i64 0, 0
  %t8712 = call %NxVal @nx_int(i64 %t8711)
  store %NxVal %t8712, ptr @nx__g___main____acc20
  br label %wcond63
wcond63:
  %t8713 = load %NxVal, ptr @nx__g___main____i20
  %t8714 = add i64 3, 0
  %t8715 = extractvalue %NxVal %t8713, 1
  %t8716 = icmp slt i64 %t8715, %t8714
  br i1 %t8716, label %wbody64, label %wend65
wbody64:
  %t8717 = load %NxVal, ptr @nx__g___main____acc20
  %t8718 = load %NxVal, ptr @nx__g___main____c
  %t8719 = load %NxVal, ptr @nx__g___main____i20
  %t8720 = add i64 20, 0
  %t8721 = extractvalue %NxVal %t8719, 1
  %t8722 = add i64 %t8721, %t8720
  %t8724 = getelementptr [2 x %NxVal], ptr %t8723, i64 0, i64 0
  store %NxVal %t8718, ptr %t8724
  %t8725 = call %NxVal @nx_int(i64 %t8722)
  %t8726 = getelementptr [2 x %NxVal], ptr %t8723, i64 0, i64 1
  store %NxVal %t8725, ptr %t8726
  %t8727 = getelementptr [2 x %NxVal], ptr %t8723, i64 0, i64 0
  %t8728 = call %NxVal @nx__m_3____main____Cell__m20(ptr %t8727, i64 2)
  %t8729 = extractvalue %NxVal %t8728, 1
  %t8730 = extractvalue %NxVal %t8717, 1
  %t8731 = add i64 %t8730, %t8729
  %t8732 = add i64 65535, 0
  %t8733 = and i64 %t8731, %t8732
  %t8734 = call %NxVal @nx_int(i64 %t8733)
  store %NxVal %t8734, ptr @nx__g___main____acc20
  %t8735 = load %NxVal, ptr @nx__g___main____acc20
  %t8736 = add i64 79, 0
  %t8737 = extractvalue %NxVal %t8735, 1
  %t8738 = and i64 %t8737, %t8736
  %t8739 = add i64 4, 0
  %t8740 = load %NxVal, ptr @nx__g___main____i20
  %t8741 = extractvalue %NxVal %t8740, 1
  %t8742 = add i64 %t8739, %t8741
  %t8743 = and i64 %t8738, %t8742
  %t8744 = add i64 65535, 0
  %t8745 = and i64 %t8743, %t8744
  %t8746 = call %NxVal @nx_int(i64 %t8745)
  store %NxVal %t8746, ptr @nx__g___main____acc20
  %t8747 = load %NxVal, ptr @nx__g___main____acc20
  %t8748 = add i64 69, 0
  %t8749 = extractvalue %NxVal %t8747, 1
  %t8750 = or i64 %t8749, %t8748
  %t8751 = add i64 37, 0
  %t8752 = load %NxVal, ptr @nx__g___main____i20
  %t8753 = extractvalue %NxVal %t8752, 1
  %t8754 = add i64 %t8751, %t8753
  %t8755 = or i64 %t8750, %t8754
  %t8756 = add i64 65535, 0
  %t8757 = and i64 %t8755, %t8756
  %t8758 = call %NxVal @nx_int(i64 %t8757)
  store %NxVal %t8758, ptr @nx__g___main____acc20
  %t8759 = load %NxVal, ptr @nx__g___main____acc20
  %t8760 = add i64 68, 0
  %t8761 = extractvalue %NxVal %t8759, 1
  %t8762 = mul i64 %t8761, %t8760
  %t8763 = add i64 27, 0
  %t8764 = mul i64 %t8762, %t8763
  %t8765 = load %NxVal, ptr @nx__g___main____i20
  %t8766 = extractvalue %NxVal %t8765, 1
  %t8767 = add i64 %t8764, %t8766
  %t8768 = add i64 65535, 0
  %t8769 = and i64 %t8767, %t8768
  %t8770 = call %NxVal @nx_int(i64 %t8769)
  store %NxVal %t8770, ptr @nx__g___main____acc20
  %t8771 = load %NxVal, ptr @nx__g___main____acc20
  %t8772 = add i64 76, 0
  %t8773 = extractvalue %NxVal %t8771, 1
  %t8774 = call i64 @nx_mod_i64(i64 %t8773, i64 %t8772)
  %t8775 = add i64 73, 0
  %t8776 = call i64 @nx_mod_i64(i64 %t8774, i64 %t8775)
  %t8777 = load %NxVal, ptr @nx__g___main____i20
  %t8778 = extractvalue %NxVal %t8777, 1
  %t8779 = add i64 %t8776, %t8778
  %t8780 = add i64 65535, 0
  %t8781 = and i64 %t8779, %t8780
  %t8782 = call %NxVal @nx_int(i64 %t8781)
  store %NxVal %t8782, ptr @nx__g___main____acc20
  %t8783 = load %NxVal, ptr @nx__g___main____i20
  %t8784 = add i64 1, 0
  %t8785 = extractvalue %NxVal %t8783, 1
  %t8786 = add i64 %t8785, %t8784
  %t8787 = call %NxVal @nx_int(i64 %t8786)
  store %NxVal %t8787, ptr @nx__g___main____i20
  br label %wcond63
wend65:
  %t8788 = load %NxVal, ptr @nx__g___main____total
  %t8789 = load %NxVal, ptr @nx__g___main____acc20
  %t8790 = extractvalue %NxVal %t8788, 1
  %t8791 = extractvalue %NxVal %t8789, 1
  %t8792 = add i64 %t8790, %t8791
  %t8793 = add i64 65535, 0
  %t8794 = and i64 %t8792, %t8793
  %t8795 = call %NxVal @nx_int(i64 %t8794)
  store %NxVal %t8795, ptr @nx__g___main____total
  %t8796 = add i64 0, 0
  %t8797 = call %NxVal @nx_int(i64 %t8796)
  store %NxVal %t8797, ptr @nx__g___main____i21
  %t8798 = add i64 0, 0
  %t8799 = call %NxVal @nx_int(i64 %t8798)
  store %NxVal %t8799, ptr @nx__g___main____acc21
  br label %wcond66
wcond66:
  %t8800 = load %NxVal, ptr @nx__g___main____i21
  %t8801 = add i64 3, 0
  %t8802 = extractvalue %NxVal %t8800, 1
  %t8803 = icmp slt i64 %t8802, %t8801
  br i1 %t8803, label %wbody67, label %wend68
wbody67:
  %t8804 = load %NxVal, ptr @nx__g___main____acc21
  %t8805 = load %NxVal, ptr @nx__g___main____c
  %t8806 = load %NxVal, ptr @nx__g___main____i21
  %t8807 = add i64 21, 0
  %t8808 = extractvalue %NxVal %t8806, 1
  %t8809 = add i64 %t8808, %t8807
  %t8811 = getelementptr [2 x %NxVal], ptr %t8810, i64 0, i64 0
  store %NxVal %t8805, ptr %t8811
  %t8812 = call %NxVal @nx_int(i64 %t8809)
  %t8813 = getelementptr [2 x %NxVal], ptr %t8810, i64 0, i64 1
  store %NxVal %t8812, ptr %t8813
  %t8814 = getelementptr [2 x %NxVal], ptr %t8810, i64 0, i64 0
  %t8815 = call %NxVal @nx__m_3____main____Cell__m21(ptr %t8814, i64 2)
  %t8816 = extractvalue %NxVal %t8815, 1
  %t8817 = extractvalue %NxVal %t8804, 1
  %t8818 = add i64 %t8817, %t8816
  %t8819 = add i64 65535, 0
  %t8820 = and i64 %t8818, %t8819
  %t8821 = call %NxVal @nx_int(i64 %t8820)
  store %NxVal %t8821, ptr @nx__g___main____acc21
  %t8822 = load %NxVal, ptr @nx__g___main____acc21
  %t8823 = add i64 79, 0
  %t8824 = extractvalue %NxVal %t8822, 1
  %t8825 = or i64 %t8824, %t8823
  %t8826 = add i64 11, 0
  %t8827 = load %NxVal, ptr @nx__g___main____i21
  %t8828 = extractvalue %NxVal %t8827, 1
  %t8829 = add i64 %t8826, %t8828
  %t8830 = or i64 %t8825, %t8829
  %t8831 = add i64 65535, 0
  %t8832 = and i64 %t8830, %t8831
  %t8833 = call %NxVal @nx_int(i64 %t8832)
  store %NxVal %t8833, ptr @nx__g___main____acc21
  %t8834 = load %NxVal, ptr @nx__g___main____acc21
  %t8835 = add i64 27, 0
  %t8836 = extractvalue %NxVal %t8834, 1
  %t8837 = add i64 %t8836, %t8835
  %t8838 = add i64 33, 0
  %t8839 = add i64 %t8837, %t8838
  %t8840 = load %NxVal, ptr @nx__g___main____i21
  %t8841 = extractvalue %NxVal %t8840, 1
  %t8842 = add i64 %t8839, %t8841
  %t8843 = add i64 65535, 0
  %t8844 = and i64 %t8842, %t8843
  %t8845 = call %NxVal @nx_int(i64 %t8844)
  store %NxVal %t8845, ptr @nx__g___main____acc21
  %t8846 = load %NxVal, ptr @nx__g___main____acc21
  %t8847 = add i64 10, 0
  %t8848 = extractvalue %NxVal %t8846, 1
  %t8849 = and i64 %t8848, %t8847
  %t8850 = add i64 47, 0
  %t8851 = load %NxVal, ptr @nx__g___main____i21
  %t8852 = extractvalue %NxVal %t8851, 1
  %t8853 = add i64 %t8850, %t8852
  %t8854 = and i64 %t8849, %t8853
  %t8855 = add i64 65535, 0
  %t8856 = and i64 %t8854, %t8855
  %t8857 = call %NxVal @nx_int(i64 %t8856)
  store %NxVal %t8857, ptr @nx__g___main____acc21
  %t8858 = load %NxVal, ptr @nx__g___main____acc21
  %t8859 = add i64 87, 0
  %t8860 = extractvalue %NxVal %t8858, 1
  %t8861 = call i64 @nx_mod_i64(i64 %t8860, i64 %t8859)
  %t8862 = add i64 86, 0
  %t8863 = call i64 @nx_mod_i64(i64 %t8861, i64 %t8862)
  %t8864 = load %NxVal, ptr @nx__g___main____i21
  %t8865 = extractvalue %NxVal %t8864, 1
  %t8866 = add i64 %t8863, %t8865
  %t8867 = add i64 65535, 0
  %t8868 = and i64 %t8866, %t8867
  %t8869 = call %NxVal @nx_int(i64 %t8868)
  store %NxVal %t8869, ptr @nx__g___main____acc21
  %t8870 = load %NxVal, ptr @nx__g___main____i21
  %t8871 = add i64 1, 0
  %t8872 = extractvalue %NxVal %t8870, 1
  %t8873 = add i64 %t8872, %t8871
  %t8874 = call %NxVal @nx_int(i64 %t8873)
  store %NxVal %t8874, ptr @nx__g___main____i21
  br label %wcond66
wend68:
  %t8875 = load %NxVal, ptr @nx__g___main____total
  %t8876 = load %NxVal, ptr @nx__g___main____acc21
  %t8877 = extractvalue %NxVal %t8875, 1
  %t8878 = extractvalue %NxVal %t8876, 1
  %t8879 = add i64 %t8877, %t8878
  %t8880 = add i64 65535, 0
  %t8881 = and i64 %t8879, %t8880
  %t8882 = call %NxVal @nx_int(i64 %t8881)
  store %NxVal %t8882, ptr @nx__g___main____total
  %t8883 = add i64 0, 0
  %t8884 = call %NxVal @nx_int(i64 %t8883)
  store %NxVal %t8884, ptr @nx__g___main____i22
  %t8885 = add i64 0, 0
  %t8886 = call %NxVal @nx_int(i64 %t8885)
  store %NxVal %t8886, ptr @nx__g___main____acc22
  br label %wcond69
wcond69:
  %t8887 = load %NxVal, ptr @nx__g___main____i22
  %t8888 = add i64 3, 0
  %t8889 = extractvalue %NxVal %t8887, 1
  %t8890 = icmp slt i64 %t8889, %t8888
  br i1 %t8890, label %wbody70, label %wend71
wbody70:
  %t8891 = load %NxVal, ptr @nx__g___main____acc22
  %t8892 = load %NxVal, ptr @nx__g___main____c
  %t8893 = load %NxVal, ptr @nx__g___main____i22
  %t8894 = add i64 22, 0
  %t8895 = extractvalue %NxVal %t8893, 1
  %t8896 = add i64 %t8895, %t8894
  %t8898 = getelementptr [2 x %NxVal], ptr %t8897, i64 0, i64 0
  store %NxVal %t8892, ptr %t8898
  %t8899 = call %NxVal @nx_int(i64 %t8896)
  %t8900 = getelementptr [2 x %NxVal], ptr %t8897, i64 0, i64 1
  store %NxVal %t8899, ptr %t8900
  %t8901 = getelementptr [2 x %NxVal], ptr %t8897, i64 0, i64 0
  %t8902 = call %NxVal @nx__m_3____main____Cell__m22(ptr %t8901, i64 2)
  %t8903 = extractvalue %NxVal %t8902, 1
  %t8904 = extractvalue %NxVal %t8891, 1
  %t8905 = add i64 %t8904, %t8903
  %t8906 = add i64 65535, 0
  %t8907 = and i64 %t8905, %t8906
  %t8908 = call %NxVal @nx_int(i64 %t8907)
  store %NxVal %t8908, ptr @nx__g___main____acc22
  %t8909 = load %NxVal, ptr @nx__g___main____acc22
  %t8910 = add i64 87, 0
  %t8911 = extractvalue %NxVal %t8909, 1
  %t8912 = xor i64 %t8911, %t8910
  %t8913 = add i64 71, 0
  %t8914 = load %NxVal, ptr @nx__g___main____i22
  %t8915 = extractvalue %NxVal %t8914, 1
  %t8916 = add i64 %t8913, %t8915
  %t8917 = xor i64 %t8912, %t8916
  %t8918 = add i64 65535, 0
  %t8919 = and i64 %t8917, %t8918
  %t8920 = call %NxVal @nx_int(i64 %t8919)
  store %NxVal %t8920, ptr @nx__g___main____acc22
  %t8921 = load %NxVal, ptr @nx__g___main____acc22
  %t8922 = add i64 5, 0
  %t8923 = extractvalue %NxVal %t8921, 1
  %t8924 = sub i64 %t8923, %t8922
  %t8925 = add i64 2, 0
  %t8926 = sub i64 %t8924, %t8925
  %t8927 = load %NxVal, ptr @nx__g___main____i22
  %t8928 = extractvalue %NxVal %t8927, 1
  %t8929 = add i64 %t8926, %t8928
  %t8930 = add i64 65535, 0
  %t8931 = and i64 %t8929, %t8930
  %t8932 = call %NxVal @nx_int(i64 %t8931)
  store %NxVal %t8932, ptr @nx__g___main____acc22
  %t8933 = load %NxVal, ptr @nx__g___main____acc22
  %t8934 = add i64 70, 0
  %t8935 = extractvalue %NxVal %t8933, 1
  %t8936 = call i64 @nx_mod_i64(i64 %t8935, i64 %t8934)
  %t8937 = add i64 82, 0
  %t8938 = call i64 @nx_mod_i64(i64 %t8936, i64 %t8937)
  %t8939 = load %NxVal, ptr @nx__g___main____i22
  %t8940 = extractvalue %NxVal %t8939, 1
  %t8941 = add i64 %t8938, %t8940
  %t8942 = add i64 65535, 0
  %t8943 = and i64 %t8941, %t8942
  %t8944 = call %NxVal @nx_int(i64 %t8943)
  store %NxVal %t8944, ptr @nx__g___main____acc22
  %t8945 = load %NxVal, ptr @nx__g___main____acc22
  %t8946 = add i64 57, 0
  %t8947 = extractvalue %NxVal %t8945, 1
  %t8948 = add i64 %t8947, %t8946
  %t8949 = add i64 34, 0
  %t8950 = add i64 %t8948, %t8949
  %t8951 = load %NxVal, ptr @nx__g___main____i22
  %t8952 = extractvalue %NxVal %t8951, 1
  %t8953 = add i64 %t8950, %t8952
  %t8954 = add i64 65535, 0
  %t8955 = and i64 %t8953, %t8954
  %t8956 = call %NxVal @nx_int(i64 %t8955)
  store %NxVal %t8956, ptr @nx__g___main____acc22
  %t8957 = load %NxVal, ptr @nx__g___main____i22
  %t8958 = add i64 1, 0
  %t8959 = extractvalue %NxVal %t8957, 1
  %t8960 = add i64 %t8959, %t8958
  %t8961 = call %NxVal @nx_int(i64 %t8960)
  store %NxVal %t8961, ptr @nx__g___main____i22
  br label %wcond69
wend71:
  %t8962 = load %NxVal, ptr @nx__g___main____total
  %t8963 = load %NxVal, ptr @nx__g___main____acc22
  %t8964 = extractvalue %NxVal %t8962, 1
  %t8965 = extractvalue %NxVal %t8963, 1
  %t8966 = add i64 %t8964, %t8965
  %t8967 = add i64 65535, 0
  %t8968 = and i64 %t8966, %t8967
  %t8969 = call %NxVal @nx_int(i64 %t8968)
  store %NxVal %t8969, ptr @nx__g___main____total
  %t8970 = add i64 0, 0
  %t8971 = call %NxVal @nx_int(i64 %t8970)
  store %NxVal %t8971, ptr @nx__g___main____i23
  %t8972 = add i64 0, 0
  %t8973 = call %NxVal @nx_int(i64 %t8972)
  store %NxVal %t8973, ptr @nx__g___main____acc23
  br label %wcond72
wcond72:
  %t8974 = load %NxVal, ptr @nx__g___main____i23
  %t8975 = add i64 3, 0
  %t8976 = extractvalue %NxVal %t8974, 1
  %t8977 = icmp slt i64 %t8976, %t8975
  br i1 %t8977, label %wbody73, label %wend74
wbody73:
  %t8978 = load %NxVal, ptr @nx__g___main____acc23
  %t8979 = load %NxVal, ptr @nx__g___main____c
  %t8980 = load %NxVal, ptr @nx__g___main____i23
  %t8981 = add i64 23, 0
  %t8982 = extractvalue %NxVal %t8980, 1
  %t8983 = add i64 %t8982, %t8981
  %t8985 = getelementptr [2 x %NxVal], ptr %t8984, i64 0, i64 0
  store %NxVal %t8979, ptr %t8985
  %t8986 = call %NxVal @nx_int(i64 %t8983)
  %t8987 = getelementptr [2 x %NxVal], ptr %t8984, i64 0, i64 1
  store %NxVal %t8986, ptr %t8987
  %t8988 = getelementptr [2 x %NxVal], ptr %t8984, i64 0, i64 0
  %t8989 = call %NxVal @nx__m_3____main____Cell__m23(ptr %t8988, i64 2)
  %t8990 = extractvalue %NxVal %t8989, 1
  %t8991 = extractvalue %NxVal %t8978, 1
  %t8992 = add i64 %t8991, %t8990
  %t8993 = add i64 65535, 0
  %t8994 = and i64 %t8992, %t8993
  %t8995 = call %NxVal @nx_int(i64 %t8994)
  store %NxVal %t8995, ptr @nx__g___main____acc23
  %t8996 = load %NxVal, ptr @nx__g___main____acc23
  %t8997 = add i64 46, 0
  %t8998 = extractvalue %NxVal %t8996, 1
  %t8999 = sub i64 %t8998, %t8997
  %t9000 = add i64 55, 0
  %t9001 = sub i64 %t8999, %t9000
  %t9002 = load %NxVal, ptr @nx__g___main____i23
  %t9003 = extractvalue %NxVal %t9002, 1
  %t9004 = add i64 %t9001, %t9003
  %t9005 = add i64 65535, 0
  %t9006 = and i64 %t9004, %t9005
  %t9007 = call %NxVal @nx_int(i64 %t9006)
  store %NxVal %t9007, ptr @nx__g___main____acc23
  %t9008 = load %NxVal, ptr @nx__g___main____acc23
  %t9009 = add i64 80, 0
  %t9010 = extractvalue %NxVal %t9008, 1
  %t9011 = sub i64 %t9010, %t9009
  %t9012 = add i64 5, 0
  %t9013 = sub i64 %t9011, %t9012
  %t9014 = load %NxVal, ptr @nx__g___main____i23
  %t9015 = extractvalue %NxVal %t9014, 1
  %t9016 = add i64 %t9013, %t9015
  %t9017 = add i64 65535, 0
  %t9018 = and i64 %t9016, %t9017
  %t9019 = call %NxVal @nx_int(i64 %t9018)
  store %NxVal %t9019, ptr @nx__g___main____acc23
  %t9020 = load %NxVal, ptr @nx__g___main____acc23
  %t9021 = add i64 35, 0
  %t9022 = extractvalue %NxVal %t9020, 1
  %t9023 = call i64 @nx_mod_i64(i64 %t9022, i64 %t9021)
  %t9024 = add i64 29, 0
  %t9025 = call i64 @nx_mod_i64(i64 %t9023, i64 %t9024)
  %t9026 = load %NxVal, ptr @nx__g___main____i23
  %t9027 = extractvalue %NxVal %t9026, 1
  %t9028 = add i64 %t9025, %t9027
  %t9029 = add i64 65535, 0
  %t9030 = and i64 %t9028, %t9029
  %t9031 = call %NxVal @nx_int(i64 %t9030)
  store %NxVal %t9031, ptr @nx__g___main____acc23
  %t9032 = load %NxVal, ptr @nx__g___main____acc23
  %t9033 = add i64 29, 0
  %t9034 = extractvalue %NxVal %t9032, 1
  %t9035 = or i64 %t9034, %t9033
  %t9036 = add i64 34, 0
  %t9037 = load %NxVal, ptr @nx__g___main____i23
  %t9038 = extractvalue %NxVal %t9037, 1
  %t9039 = add i64 %t9036, %t9038
  %t9040 = or i64 %t9035, %t9039
  %t9041 = add i64 65535, 0
  %t9042 = and i64 %t9040, %t9041
  %t9043 = call %NxVal @nx_int(i64 %t9042)
  store %NxVal %t9043, ptr @nx__g___main____acc23
  %t9044 = load %NxVal, ptr @nx__g___main____i23
  %t9045 = add i64 1, 0
  %t9046 = extractvalue %NxVal %t9044, 1
  %t9047 = add i64 %t9046, %t9045
  %t9048 = call %NxVal @nx_int(i64 %t9047)
  store %NxVal %t9048, ptr @nx__g___main____i23
  br label %wcond72
wend74:
  %t9049 = load %NxVal, ptr @nx__g___main____total
  %t9050 = load %NxVal, ptr @nx__g___main____acc23
  %t9051 = extractvalue %NxVal %t9049, 1
  %t9052 = extractvalue %NxVal %t9050, 1
  %t9053 = add i64 %t9051, %t9052
  %t9054 = add i64 65535, 0
  %t9055 = and i64 %t9053, %t9054
  %t9056 = call %NxVal @nx_int(i64 %t9055)
  store %NxVal %t9056, ptr @nx__g___main____total
  %t9057 = add i64 0, 0
  %t9058 = call %NxVal @nx_int(i64 %t9057)
  store %NxVal %t9058, ptr @nx__g___main____i24
  %t9059 = add i64 0, 0
  %t9060 = call %NxVal @nx_int(i64 %t9059)
  store %NxVal %t9060, ptr @nx__g___main____acc24
  br label %wcond75
wcond75:
  %t9061 = load %NxVal, ptr @nx__g___main____i24
  %t9062 = add i64 3, 0
  %t9063 = extractvalue %NxVal %t9061, 1
  %t9064 = icmp slt i64 %t9063, %t9062
  br i1 %t9064, label %wbody76, label %wend77
wbody76:
  %t9065 = load %NxVal, ptr @nx__g___main____acc24
  %t9066 = load %NxVal, ptr @nx__g___main____c
  %t9067 = load %NxVal, ptr @nx__g___main____i24
  %t9068 = add i64 24, 0
  %t9069 = extractvalue %NxVal %t9067, 1
  %t9070 = add i64 %t9069, %t9068
  %t9072 = getelementptr [2 x %NxVal], ptr %t9071, i64 0, i64 0
  store %NxVal %t9066, ptr %t9072
  %t9073 = call %NxVal @nx_int(i64 %t9070)
  %t9074 = getelementptr [2 x %NxVal], ptr %t9071, i64 0, i64 1
  store %NxVal %t9073, ptr %t9074
  %t9075 = getelementptr [2 x %NxVal], ptr %t9071, i64 0, i64 0
  %t9076 = call %NxVal @nx__m_3____main____Cell__m24(ptr %t9075, i64 2)
  %t9077 = extractvalue %NxVal %t9076, 1
  %t9078 = extractvalue %NxVal %t9065, 1
  %t9079 = add i64 %t9078, %t9077
  %t9080 = add i64 65535, 0
  %t9081 = and i64 %t9079, %t9080
  %t9082 = call %NxVal @nx_int(i64 %t9081)
  store %NxVal %t9082, ptr @nx__g___main____acc24
  %t9083 = load %NxVal, ptr @nx__g___main____acc24
  %t9084 = add i64 56, 0
  %t9085 = extractvalue %NxVal %t9083, 1
  %t9086 = add i64 %t9085, %t9084
  %t9087 = add i64 43, 0
  %t9088 = add i64 %t9086, %t9087
  %t9089 = load %NxVal, ptr @nx__g___main____i24
  %t9090 = extractvalue %NxVal %t9089, 1
  %t9091 = add i64 %t9088, %t9090
  %t9092 = add i64 65535, 0
  %t9093 = and i64 %t9091, %t9092
  %t9094 = call %NxVal @nx_int(i64 %t9093)
  store %NxVal %t9094, ptr @nx__g___main____acc24
  %t9095 = load %NxVal, ptr @nx__g___main____acc24
  %t9096 = add i64 91, 0
  %t9097 = extractvalue %NxVal %t9095, 1
  %t9098 = sub i64 %t9097, %t9096
  %t9099 = add i64 18, 0
  %t9100 = sub i64 %t9098, %t9099
  %t9101 = load %NxVal, ptr @nx__g___main____i24
  %t9102 = extractvalue %NxVal %t9101, 1
  %t9103 = add i64 %t9100, %t9102
  %t9104 = add i64 65535, 0
  %t9105 = and i64 %t9103, %t9104
  %t9106 = call %NxVal @nx_int(i64 %t9105)
  store %NxVal %t9106, ptr @nx__g___main____acc24
  %t9107 = load %NxVal, ptr @nx__g___main____acc24
  %t9108 = add i64 70, 0
  %t9109 = extractvalue %NxVal %t9107, 1
  %t9110 = call i64 @nx_mod_i64(i64 %t9109, i64 %t9108)
  %t9111 = add i64 76, 0
  %t9112 = call i64 @nx_mod_i64(i64 %t9110, i64 %t9111)
  %t9113 = load %NxVal, ptr @nx__g___main____i24
  %t9114 = extractvalue %NxVal %t9113, 1
  %t9115 = add i64 %t9112, %t9114
  %t9116 = add i64 65535, 0
  %t9117 = and i64 %t9115, %t9116
  %t9118 = call %NxVal @nx_int(i64 %t9117)
  store %NxVal %t9118, ptr @nx__g___main____acc24
  %t9119 = load %NxVal, ptr @nx__g___main____acc24
  %t9120 = add i64 56, 0
  %t9121 = extractvalue %NxVal %t9119, 1
  %t9122 = xor i64 %t9121, %t9120
  %t9123 = add i64 51, 0
  %t9124 = load %NxVal, ptr @nx__g___main____i24
  %t9125 = extractvalue %NxVal %t9124, 1
  %t9126 = add i64 %t9123, %t9125
  %t9127 = xor i64 %t9122, %t9126
  %t9128 = add i64 65535, 0
  %t9129 = and i64 %t9127, %t9128
  %t9130 = call %NxVal @nx_int(i64 %t9129)
  store %NxVal %t9130, ptr @nx__g___main____acc24
  %t9131 = load %NxVal, ptr @nx__g___main____i24
  %t9132 = add i64 1, 0
  %t9133 = extractvalue %NxVal %t9131, 1
  %t9134 = add i64 %t9133, %t9132
  %t9135 = call %NxVal @nx_int(i64 %t9134)
  store %NxVal %t9135, ptr @nx__g___main____i24
  br label %wcond75
wend77:
  %t9136 = load %NxVal, ptr @nx__g___main____total
  %t9137 = load %NxVal, ptr @nx__g___main____acc24
  %t9138 = extractvalue %NxVal %t9136, 1
  %t9139 = extractvalue %NxVal %t9137, 1
  %t9140 = add i64 %t9138, %t9139
  %t9141 = add i64 65535, 0
  %t9142 = and i64 %t9140, %t9141
  %t9143 = call %NxVal @nx_int(i64 %t9142)
  store %NxVal %t9143, ptr @nx__g___main____total
  %t9144 = add i64 0, 0
  %t9145 = call %NxVal @nx_int(i64 %t9144)
  store %NxVal %t9145, ptr @nx__g___main____i25
  %t9146 = add i64 0, 0
  %t9147 = call %NxVal @nx_int(i64 %t9146)
  store %NxVal %t9147, ptr @nx__g___main____acc25
  br label %wcond78
wcond78:
  %t9148 = load %NxVal, ptr @nx__g___main____i25
  %t9149 = add i64 3, 0
  %t9150 = extractvalue %NxVal %t9148, 1
  %t9151 = icmp slt i64 %t9150, %t9149
  br i1 %t9151, label %wbody79, label %wend80
wbody79:
  %t9152 = load %NxVal, ptr @nx__g___main____acc25
  %t9153 = load %NxVal, ptr @nx__g___main____c
  %t9154 = load %NxVal, ptr @nx__g___main____i25
  %t9155 = add i64 25, 0
  %t9156 = extractvalue %NxVal %t9154, 1
  %t9157 = add i64 %t9156, %t9155
  %t9159 = getelementptr [2 x %NxVal], ptr %t9158, i64 0, i64 0
  store %NxVal %t9153, ptr %t9159
  %t9160 = call %NxVal @nx_int(i64 %t9157)
  %t9161 = getelementptr [2 x %NxVal], ptr %t9158, i64 0, i64 1
  store %NxVal %t9160, ptr %t9161
  %t9162 = getelementptr [2 x %NxVal], ptr %t9158, i64 0, i64 0
  %t9163 = call %NxVal @nx__m_3____main____Cell__m25(ptr %t9162, i64 2)
  %t9164 = extractvalue %NxVal %t9163, 1
  %t9165 = extractvalue %NxVal %t9152, 1
  %t9166 = add i64 %t9165, %t9164
  %t9167 = add i64 65535, 0
  %t9168 = and i64 %t9166, %t9167
  %t9169 = call %NxVal @nx_int(i64 %t9168)
  store %NxVal %t9169, ptr @nx__g___main____acc25
  %t9170 = load %NxVal, ptr @nx__g___main____acc25
  %t9171 = add i64 59, 0
  %t9172 = extractvalue %NxVal %t9170, 1
  %t9173 = add i64 %t9172, %t9171
  %t9174 = add i64 21, 0
  %t9175 = add i64 %t9173, %t9174
  %t9176 = load %NxVal, ptr @nx__g___main____i25
  %t9177 = extractvalue %NxVal %t9176, 1
  %t9178 = add i64 %t9175, %t9177
  %t9179 = add i64 65535, 0
  %t9180 = and i64 %t9178, %t9179
  %t9181 = call %NxVal @nx_int(i64 %t9180)
  store %NxVal %t9181, ptr @nx__g___main____acc25
  %t9182 = load %NxVal, ptr @nx__g___main____acc25
  %t9183 = add i64 2, 0
  %t9184 = extractvalue %NxVal %t9182, 1
  %t9185 = add i64 %t9184, %t9183
  %t9186 = add i64 60, 0
  %t9187 = add i64 %t9185, %t9186
  %t9188 = load %NxVal, ptr @nx__g___main____i25
  %t9189 = extractvalue %NxVal %t9188, 1
  %t9190 = add i64 %t9187, %t9189
  %t9191 = add i64 65535, 0
  %t9192 = and i64 %t9190, %t9191
  %t9193 = call %NxVal @nx_int(i64 %t9192)
  store %NxVal %t9193, ptr @nx__g___main____acc25
  %t9194 = load %NxVal, ptr @nx__g___main____acc25
  %t9195 = add i64 5, 0
  %t9196 = extractvalue %NxVal %t9194, 1
  %t9197 = call i64 @nx_mod_i64(i64 %t9196, i64 %t9195)
  %t9198 = add i64 80, 0
  %t9199 = call i64 @nx_mod_i64(i64 %t9197, i64 %t9198)
  %t9200 = load %NxVal, ptr @nx__g___main____i25
  %t9201 = extractvalue %NxVal %t9200, 1
  %t9202 = add i64 %t9199, %t9201
  %t9203 = add i64 65535, 0
  %t9204 = and i64 %t9202, %t9203
  %t9205 = call %NxVal @nx_int(i64 %t9204)
  store %NxVal %t9205, ptr @nx__g___main____acc25
  %t9206 = load %NxVal, ptr @nx__g___main____acc25
  %t9207 = add i64 27, 0
  %t9208 = extractvalue %NxVal %t9206, 1
  %t9209 = mul i64 %t9208, %t9207
  %t9210 = add i64 70, 0
  %t9211 = mul i64 %t9209, %t9210
  %t9212 = load %NxVal, ptr @nx__g___main____i25
  %t9213 = extractvalue %NxVal %t9212, 1
  %t9214 = add i64 %t9211, %t9213
  %t9215 = add i64 65535, 0
  %t9216 = and i64 %t9214, %t9215
  %t9217 = call %NxVal @nx_int(i64 %t9216)
  store %NxVal %t9217, ptr @nx__g___main____acc25
  %t9218 = load %NxVal, ptr @nx__g___main____i25
  %t9219 = add i64 1, 0
  %t9220 = extractvalue %NxVal %t9218, 1
  %t9221 = add i64 %t9220, %t9219
  %t9222 = call %NxVal @nx_int(i64 %t9221)
  store %NxVal %t9222, ptr @nx__g___main____i25
  br label %wcond78
wend80:
  %t9223 = load %NxVal, ptr @nx__g___main____total
  %t9224 = load %NxVal, ptr @nx__g___main____acc25
  %t9225 = extractvalue %NxVal %t9223, 1
  %t9226 = extractvalue %NxVal %t9224, 1
  %t9227 = add i64 %t9225, %t9226
  %t9228 = add i64 65535, 0
  %t9229 = and i64 %t9227, %t9228
  %t9230 = call %NxVal @nx_int(i64 %t9229)
  store %NxVal %t9230, ptr @nx__g___main____total
  %t9231 = add i64 0, 0
  %t9232 = call %NxVal @nx_int(i64 %t9231)
  store %NxVal %t9232, ptr @nx__g___main____i26
  %t9233 = add i64 0, 0
  %t9234 = call %NxVal @nx_int(i64 %t9233)
  store %NxVal %t9234, ptr @nx__g___main____acc26
  br label %wcond81
wcond81:
  %t9235 = load %NxVal, ptr @nx__g___main____i26
  %t9236 = add i64 3, 0
  %t9237 = extractvalue %NxVal %t9235, 1
  %t9238 = icmp slt i64 %t9237, %t9236
  br i1 %t9238, label %wbody82, label %wend83
wbody82:
  %t9239 = load %NxVal, ptr @nx__g___main____acc26
  %t9240 = load %NxVal, ptr @nx__g___main____c
  %t9241 = load %NxVal, ptr @nx__g___main____i26
  %t9242 = add i64 26, 0
  %t9243 = extractvalue %NxVal %t9241, 1
  %t9244 = add i64 %t9243, %t9242
  %t9246 = getelementptr [2 x %NxVal], ptr %t9245, i64 0, i64 0
  store %NxVal %t9240, ptr %t9246
  %t9247 = call %NxVal @nx_int(i64 %t9244)
  %t9248 = getelementptr [2 x %NxVal], ptr %t9245, i64 0, i64 1
  store %NxVal %t9247, ptr %t9248
  %t9249 = getelementptr [2 x %NxVal], ptr %t9245, i64 0, i64 0
  %t9250 = call %NxVal @nx__m_3____main____Cell__m26(ptr %t9249, i64 2)
  %t9251 = extractvalue %NxVal %t9250, 1
  %t9252 = extractvalue %NxVal %t9239, 1
  %t9253 = add i64 %t9252, %t9251
  %t9254 = add i64 65535, 0
  %t9255 = and i64 %t9253, %t9254
  %t9256 = call %NxVal @nx_int(i64 %t9255)
  store %NxVal %t9256, ptr @nx__g___main____acc26
  %t9257 = load %NxVal, ptr @nx__g___main____acc26
  %t9258 = add i64 68, 0
  %t9259 = extractvalue %NxVal %t9257, 1
  %t9260 = and i64 %t9259, %t9258
  %t9261 = add i64 70, 0
  %t9262 = load %NxVal, ptr @nx__g___main____i26
  %t9263 = extractvalue %NxVal %t9262, 1
  %t9264 = add i64 %t9261, %t9263
  %t9265 = and i64 %t9260, %t9264
  %t9266 = add i64 65535, 0
  %t9267 = and i64 %t9265, %t9266
  %t9268 = call %NxVal @nx_int(i64 %t9267)
  store %NxVal %t9268, ptr @nx__g___main____acc26
  %t9269 = load %NxVal, ptr @nx__g___main____acc26
  %t9270 = add i64 62, 0
  %t9271 = extractvalue %NxVal %t9269, 1
  %t9272 = or i64 %t9271, %t9270
  %t9273 = add i64 55, 0
  %t9274 = load %NxVal, ptr @nx__g___main____i26
  %t9275 = extractvalue %NxVal %t9274, 1
  %t9276 = add i64 %t9273, %t9275
  %t9277 = or i64 %t9272, %t9276
  %t9278 = add i64 65535, 0
  %t9279 = and i64 %t9277, %t9278
  %t9280 = call %NxVal @nx_int(i64 %t9279)
  store %NxVal %t9280, ptr @nx__g___main____acc26
  %t9281 = load %NxVal, ptr @nx__g___main____acc26
  %t9282 = add i64 1, 0
  %t9283 = extractvalue %NxVal %t9281, 1
  %t9284 = sub i64 %t9283, %t9282
  %t9285 = add i64 15, 0
  %t9286 = sub i64 %t9284, %t9285
  %t9287 = load %NxVal, ptr @nx__g___main____i26
  %t9288 = extractvalue %NxVal %t9287, 1
  %t9289 = add i64 %t9286, %t9288
  %t9290 = add i64 65535, 0
  %t9291 = and i64 %t9289, %t9290
  %t9292 = call %NxVal @nx_int(i64 %t9291)
  store %NxVal %t9292, ptr @nx__g___main____acc26
  %t9293 = load %NxVal, ptr @nx__g___main____acc26
  %t9294 = add i64 4, 0
  %t9295 = extractvalue %NxVal %t9293, 1
  %t9296 = call i64 @nx_mod_i64(i64 %t9295, i64 %t9294)
  %t9297 = add i64 4, 0
  %t9298 = call i64 @nx_mod_i64(i64 %t9296, i64 %t9297)
  %t9299 = load %NxVal, ptr @nx__g___main____i26
  %t9300 = extractvalue %NxVal %t9299, 1
  %t9301 = add i64 %t9298, %t9300
  %t9302 = add i64 65535, 0
  %t9303 = and i64 %t9301, %t9302
  %t9304 = call %NxVal @nx_int(i64 %t9303)
  store %NxVal %t9304, ptr @nx__g___main____acc26
  %t9305 = load %NxVal, ptr @nx__g___main____i26
  %t9306 = add i64 1, 0
  %t9307 = extractvalue %NxVal %t9305, 1
  %t9308 = add i64 %t9307, %t9306
  %t9309 = call %NxVal @nx_int(i64 %t9308)
  store %NxVal %t9309, ptr @nx__g___main____i26
  br label %wcond81
wend83:
  %t9310 = load %NxVal, ptr @nx__g___main____total
  %t9311 = load %NxVal, ptr @nx__g___main____acc26
  %t9312 = extractvalue %NxVal %t9310, 1
  %t9313 = extractvalue %NxVal %t9311, 1
  %t9314 = add i64 %t9312, %t9313
  %t9315 = add i64 65535, 0
  %t9316 = and i64 %t9314, %t9315
  %t9317 = call %NxVal @nx_int(i64 %t9316)
  store %NxVal %t9317, ptr @nx__g___main____total
  %t9318 = add i64 0, 0
  %t9319 = call %NxVal @nx_int(i64 %t9318)
  store %NxVal %t9319, ptr @nx__g___main____i27
  %t9320 = add i64 0, 0
  %t9321 = call %NxVal @nx_int(i64 %t9320)
  store %NxVal %t9321, ptr @nx__g___main____acc27
  br label %wcond84
wcond84:
  %t9322 = load %NxVal, ptr @nx__g___main____i27
  %t9323 = add i64 3, 0
  %t9324 = extractvalue %NxVal %t9322, 1
  %t9325 = icmp slt i64 %t9324, %t9323
  br i1 %t9325, label %wbody85, label %wend86
wbody85:
  %t9326 = load %NxVal, ptr @nx__g___main____acc27
  %t9327 = load %NxVal, ptr @nx__g___main____c
  %t9328 = load %NxVal, ptr @nx__g___main____i27
  %t9329 = add i64 27, 0
  %t9330 = extractvalue %NxVal %t9328, 1
  %t9331 = add i64 %t9330, %t9329
  %t9333 = getelementptr [2 x %NxVal], ptr %t9332, i64 0, i64 0
  store %NxVal %t9327, ptr %t9333
  %t9334 = call %NxVal @nx_int(i64 %t9331)
  %t9335 = getelementptr [2 x %NxVal], ptr %t9332, i64 0, i64 1
  store %NxVal %t9334, ptr %t9335
  %t9336 = getelementptr [2 x %NxVal], ptr %t9332, i64 0, i64 0
  %t9337 = call %NxVal @nx__m_3____main____Cell__m27(ptr %t9336, i64 2)
  %t9338 = extractvalue %NxVal %t9337, 1
  %t9339 = extractvalue %NxVal %t9326, 1
  %t9340 = add i64 %t9339, %t9338
  %t9341 = add i64 65535, 0
  %t9342 = and i64 %t9340, %t9341
  %t9343 = call %NxVal @nx_int(i64 %t9342)
  store %NxVal %t9343, ptr @nx__g___main____acc27
  %t9344 = load %NxVal, ptr @nx__g___main____acc27
  %t9345 = add i64 32, 0
  %t9346 = extractvalue %NxVal %t9344, 1
  %t9347 = mul i64 %t9346, %t9345
  %t9348 = add i64 29, 0
  %t9349 = mul i64 %t9347, %t9348
  %t9350 = load %NxVal, ptr @nx__g___main____i27
  %t9351 = extractvalue %NxVal %t9350, 1
  %t9352 = add i64 %t9349, %t9351
  %t9353 = add i64 65535, 0
  %t9354 = and i64 %t9352, %t9353
  %t9355 = call %NxVal @nx_int(i64 %t9354)
  store %NxVal %t9355, ptr @nx__g___main____acc27
  %t9356 = load %NxVal, ptr @nx__g___main____acc27
  %t9357 = add i64 30, 0
  %t9358 = extractvalue %NxVal %t9356, 1
  %t9359 = call i64 @nx_mod_i64(i64 %t9358, i64 %t9357)
  %t9360 = add i64 69, 0
  %t9361 = call i64 @nx_mod_i64(i64 %t9359, i64 %t9360)
  %t9362 = load %NxVal, ptr @nx__g___main____i27
  %t9363 = extractvalue %NxVal %t9362, 1
  %t9364 = add i64 %t9361, %t9363
  %t9365 = add i64 65535, 0
  %t9366 = and i64 %t9364, %t9365
  %t9367 = call %NxVal @nx_int(i64 %t9366)
  store %NxVal %t9367, ptr @nx__g___main____acc27
  %t9368 = load %NxVal, ptr @nx__g___main____acc27
  %t9369 = add i64 65, 0
  %t9370 = extractvalue %NxVal %t9368, 1
  %t9371 = or i64 %t9370, %t9369
  %t9372 = add i64 48, 0
  %t9373 = load %NxVal, ptr @nx__g___main____i27
  %t9374 = extractvalue %NxVal %t9373, 1
  %t9375 = add i64 %t9372, %t9374
  %t9376 = or i64 %t9371, %t9375
  %t9377 = add i64 65535, 0
  %t9378 = and i64 %t9376, %t9377
  %t9379 = call %NxVal @nx_int(i64 %t9378)
  store %NxVal %t9379, ptr @nx__g___main____acc27
  %t9380 = load %NxVal, ptr @nx__g___main____acc27
  %t9381 = add i64 39, 0
  %t9382 = extractvalue %NxVal %t9380, 1
  %t9383 = mul i64 %t9382, %t9381
  %t9384 = add i64 12, 0
  %t9385 = mul i64 %t9383, %t9384
  %t9386 = load %NxVal, ptr @nx__g___main____i27
  %t9387 = extractvalue %NxVal %t9386, 1
  %t9388 = add i64 %t9385, %t9387
  %t9389 = add i64 65535, 0
  %t9390 = and i64 %t9388, %t9389
  %t9391 = call %NxVal @nx_int(i64 %t9390)
  store %NxVal %t9391, ptr @nx__g___main____acc27
  %t9392 = load %NxVal, ptr @nx__g___main____i27
  %t9393 = add i64 1, 0
  %t9394 = extractvalue %NxVal %t9392, 1
  %t9395 = add i64 %t9394, %t9393
  %t9396 = call %NxVal @nx_int(i64 %t9395)
  store %NxVal %t9396, ptr @nx__g___main____i27
  br label %wcond84
wend86:
  %t9397 = load %NxVal, ptr @nx__g___main____total
  %t9398 = load %NxVal, ptr @nx__g___main____acc27
  %t9399 = extractvalue %NxVal %t9397, 1
  %t9400 = extractvalue %NxVal %t9398, 1
  %t9401 = add i64 %t9399, %t9400
  %t9402 = add i64 65535, 0
  %t9403 = and i64 %t9401, %t9402
  %t9404 = call %NxVal @nx_int(i64 %t9403)
  store %NxVal %t9404, ptr @nx__g___main____total
  %t9405 = add i64 0, 0
  %t9406 = call %NxVal @nx_int(i64 %t9405)
  store %NxVal %t9406, ptr @nx__g___main____i28
  %t9407 = add i64 0, 0
  %t9408 = call %NxVal @nx_int(i64 %t9407)
  store %NxVal %t9408, ptr @nx__g___main____acc28
  br label %wcond87
wcond87:
  %t9409 = load %NxVal, ptr @nx__g___main____i28
  %t9410 = add i64 3, 0
  %t9411 = extractvalue %NxVal %t9409, 1
  %t9412 = icmp slt i64 %t9411, %t9410
  br i1 %t9412, label %wbody88, label %wend89
wbody88:
  %t9413 = load %NxVal, ptr @nx__g___main____acc28
  %t9414 = load %NxVal, ptr @nx__g___main____c
  %t9415 = load %NxVal, ptr @nx__g___main____i28
  %t9416 = add i64 28, 0
  %t9417 = extractvalue %NxVal %t9415, 1
  %t9418 = add i64 %t9417, %t9416
  %t9420 = getelementptr [2 x %NxVal], ptr %t9419, i64 0, i64 0
  store %NxVal %t9414, ptr %t9420
  %t9421 = call %NxVal @nx_int(i64 %t9418)
  %t9422 = getelementptr [2 x %NxVal], ptr %t9419, i64 0, i64 1
  store %NxVal %t9421, ptr %t9422
  %t9423 = getelementptr [2 x %NxVal], ptr %t9419, i64 0, i64 0
  %t9424 = call %NxVal @nx__m_3____main____Cell__m28(ptr %t9423, i64 2)
  %t9425 = extractvalue %NxVal %t9424, 1
  %t9426 = extractvalue %NxVal %t9413, 1
  %t9427 = add i64 %t9426, %t9425
  %t9428 = add i64 65535, 0
  %t9429 = and i64 %t9427, %t9428
  %t9430 = call %NxVal @nx_int(i64 %t9429)
  store %NxVal %t9430, ptr @nx__g___main____acc28
  %t9431 = load %NxVal, ptr @nx__g___main____acc28
  %t9432 = add i64 85, 0
  %t9433 = extractvalue %NxVal %t9431, 1
  %t9434 = sub i64 %t9433, %t9432
  %t9435 = add i64 27, 0
  %t9436 = sub i64 %t9434, %t9435
  %t9437 = load %NxVal, ptr @nx__g___main____i28
  %t9438 = extractvalue %NxVal %t9437, 1
  %t9439 = add i64 %t9436, %t9438
  %t9440 = add i64 65535, 0
  %t9441 = and i64 %t9439, %t9440
  %t9442 = call %NxVal @nx_int(i64 %t9441)
  store %NxVal %t9442, ptr @nx__g___main____acc28
  %t9443 = load %NxVal, ptr @nx__g___main____acc28
  %t9444 = add i64 8, 0
  %t9445 = extractvalue %NxVal %t9443, 1
  %t9446 = and i64 %t9445, %t9444
  %t9447 = add i64 87, 0
  %t9448 = load %NxVal, ptr @nx__g___main____i28
  %t9449 = extractvalue %NxVal %t9448, 1
  %t9450 = add i64 %t9447, %t9449
  %t9451 = and i64 %t9446, %t9450
  %t9452 = add i64 65535, 0
  %t9453 = and i64 %t9451, %t9452
  %t9454 = call %NxVal @nx_int(i64 %t9453)
  store %NxVal %t9454, ptr @nx__g___main____acc28
  %t9455 = load %NxVal, ptr @nx__g___main____acc28
  %t9456 = add i64 96, 0
  %t9457 = extractvalue %NxVal %t9455, 1
  %t9458 = or i64 %t9457, %t9456
  %t9459 = add i64 56, 0
  %t9460 = load %NxVal, ptr @nx__g___main____i28
  %t9461 = extractvalue %NxVal %t9460, 1
  %t9462 = add i64 %t9459, %t9461
  %t9463 = or i64 %t9458, %t9462
  %t9464 = add i64 65535, 0
  %t9465 = and i64 %t9463, %t9464
  %t9466 = call %NxVal @nx_int(i64 %t9465)
  store %NxVal %t9466, ptr @nx__g___main____acc28
  %t9467 = load %NxVal, ptr @nx__g___main____acc28
  %t9468 = add i64 10, 0
  %t9469 = extractvalue %NxVal %t9467, 1
  %t9470 = or i64 %t9469, %t9468
  %t9471 = add i64 56, 0
  %t9472 = load %NxVal, ptr @nx__g___main____i28
  %t9473 = extractvalue %NxVal %t9472, 1
  %t9474 = add i64 %t9471, %t9473
  %t9475 = or i64 %t9470, %t9474
  %t9476 = add i64 65535, 0
  %t9477 = and i64 %t9475, %t9476
  %t9478 = call %NxVal @nx_int(i64 %t9477)
  store %NxVal %t9478, ptr @nx__g___main____acc28
  %t9479 = load %NxVal, ptr @nx__g___main____i28
  %t9480 = add i64 1, 0
  %t9481 = extractvalue %NxVal %t9479, 1
  %t9482 = add i64 %t9481, %t9480
  %t9483 = call %NxVal @nx_int(i64 %t9482)
  store %NxVal %t9483, ptr @nx__g___main____i28
  br label %wcond87
wend89:
  %t9484 = load %NxVal, ptr @nx__g___main____total
  %t9485 = load %NxVal, ptr @nx__g___main____acc28
  %t9486 = extractvalue %NxVal %t9484, 1
  %t9487 = extractvalue %NxVal %t9485, 1
  %t9488 = add i64 %t9486, %t9487
  %t9489 = add i64 65535, 0
  %t9490 = and i64 %t9488, %t9489
  %t9491 = call %NxVal @nx_int(i64 %t9490)
  store %NxVal %t9491, ptr @nx__g___main____total
  %t9492 = add i64 0, 0
  %t9493 = call %NxVal @nx_int(i64 %t9492)
  store %NxVal %t9493, ptr @nx__g___main____i29
  %t9494 = add i64 0, 0
  %t9495 = call %NxVal @nx_int(i64 %t9494)
  store %NxVal %t9495, ptr @nx__g___main____acc29
  br label %wcond90
wcond90:
  %t9496 = load %NxVal, ptr @nx__g___main____i29
  %t9497 = add i64 3, 0
  %t9498 = extractvalue %NxVal %t9496, 1
  %t9499 = icmp slt i64 %t9498, %t9497
  br i1 %t9499, label %wbody91, label %wend92
wbody91:
  %t9500 = load %NxVal, ptr @nx__g___main____acc29
  %t9501 = load %NxVal, ptr @nx__g___main____c
  %t9502 = load %NxVal, ptr @nx__g___main____i29
  %t9503 = add i64 29, 0
  %t9504 = extractvalue %NxVal %t9502, 1
  %t9505 = add i64 %t9504, %t9503
  %t9507 = getelementptr [2 x %NxVal], ptr %t9506, i64 0, i64 0
  store %NxVal %t9501, ptr %t9507
  %t9508 = call %NxVal @nx_int(i64 %t9505)
  %t9509 = getelementptr [2 x %NxVal], ptr %t9506, i64 0, i64 1
  store %NxVal %t9508, ptr %t9509
  %t9510 = getelementptr [2 x %NxVal], ptr %t9506, i64 0, i64 0
  %t9511 = call %NxVal @nx__m_3____main____Cell__m29(ptr %t9510, i64 2)
  %t9512 = extractvalue %NxVal %t9511, 1
  %t9513 = extractvalue %NxVal %t9500, 1
  %t9514 = add i64 %t9513, %t9512
  %t9515 = add i64 65535, 0
  %t9516 = and i64 %t9514, %t9515
  %t9517 = call %NxVal @nx_int(i64 %t9516)
  store %NxVal %t9517, ptr @nx__g___main____acc29
  %t9518 = load %NxVal, ptr @nx__g___main____acc29
  %t9519 = add i64 20, 0
  %t9520 = extractvalue %NxVal %t9518, 1
  %t9521 = or i64 %t9520, %t9519
  %t9522 = add i64 86, 0
  %t9523 = load %NxVal, ptr @nx__g___main____i29
  %t9524 = extractvalue %NxVal %t9523, 1
  %t9525 = add i64 %t9522, %t9524
  %t9526 = or i64 %t9521, %t9525
  %t9527 = add i64 65535, 0
  %t9528 = and i64 %t9526, %t9527
  %t9529 = call %NxVal @nx_int(i64 %t9528)
  store %NxVal %t9529, ptr @nx__g___main____acc29
  %t9530 = load %NxVal, ptr @nx__g___main____acc29
  %t9531 = add i64 17, 0
  %t9532 = extractvalue %NxVal %t9530, 1
  %t9533 = mul i64 %t9532, %t9531
  %t9534 = add i64 76, 0
  %t9535 = mul i64 %t9533, %t9534
  %t9536 = load %NxVal, ptr @nx__g___main____i29
  %t9537 = extractvalue %NxVal %t9536, 1
  %t9538 = add i64 %t9535, %t9537
  %t9539 = add i64 65535, 0
  %t9540 = and i64 %t9538, %t9539
  %t9541 = call %NxVal @nx_int(i64 %t9540)
  store %NxVal %t9541, ptr @nx__g___main____acc29
  %t9542 = load %NxVal, ptr @nx__g___main____acc29
  %t9543 = add i64 30, 0
  %t9544 = extractvalue %NxVal %t9542, 1
  %t9545 = xor i64 %t9544, %t9543
  %t9546 = add i64 27, 0
  %t9547 = load %NxVal, ptr @nx__g___main____i29
  %t9548 = extractvalue %NxVal %t9547, 1
  %t9549 = add i64 %t9546, %t9548
  %t9550 = xor i64 %t9545, %t9549
  %t9551 = add i64 65535, 0
  %t9552 = and i64 %t9550, %t9551
  %t9553 = call %NxVal @nx_int(i64 %t9552)
  store %NxVal %t9553, ptr @nx__g___main____acc29
  %t9554 = load %NxVal, ptr @nx__g___main____acc29
  %t9555 = add i64 44, 0
  %t9556 = extractvalue %NxVal %t9554, 1
  %t9557 = sub i64 %t9556, %t9555
  %t9558 = add i64 66, 0
  %t9559 = sub i64 %t9557, %t9558
  %t9560 = load %NxVal, ptr @nx__g___main____i29
  %t9561 = extractvalue %NxVal %t9560, 1
  %t9562 = add i64 %t9559, %t9561
  %t9563 = add i64 65535, 0
  %t9564 = and i64 %t9562, %t9563
  %t9565 = call %NxVal @nx_int(i64 %t9564)
  store %NxVal %t9565, ptr @nx__g___main____acc29
  %t9566 = load %NxVal, ptr @nx__g___main____i29
  %t9567 = add i64 1, 0
  %t9568 = extractvalue %NxVal %t9566, 1
  %t9569 = add i64 %t9568, %t9567
  %t9570 = call %NxVal @nx_int(i64 %t9569)
  store %NxVal %t9570, ptr @nx__g___main____i29
  br label %wcond90
wend92:
  %t9571 = load %NxVal, ptr @nx__g___main____total
  %t9572 = load %NxVal, ptr @nx__g___main____acc29
  %t9573 = extractvalue %NxVal %t9571, 1
  %t9574 = extractvalue %NxVal %t9572, 1
  %t9575 = add i64 %t9573, %t9574
  %t9576 = add i64 65535, 0
  %t9577 = and i64 %t9575, %t9576
  %t9578 = call %NxVal @nx_int(i64 %t9577)
  store %NxVal %t9578, ptr @nx__g___main____total
  %t9579 = add i64 0, 0
  %t9580 = call %NxVal @nx_int(i64 %t9579)
  store %NxVal %t9580, ptr @nx__g___main____i30
  %t9581 = add i64 0, 0
  %t9582 = call %NxVal @nx_int(i64 %t9581)
  store %NxVal %t9582, ptr @nx__g___main____acc30
  br label %wcond93
wcond93:
  %t9583 = load %NxVal, ptr @nx__g___main____i30
  %t9584 = add i64 3, 0
  %t9585 = extractvalue %NxVal %t9583, 1
  %t9586 = icmp slt i64 %t9585, %t9584
  br i1 %t9586, label %wbody94, label %wend95
wbody94:
  %t9587 = load %NxVal, ptr @nx__g___main____acc30
  %t9588 = load %NxVal, ptr @nx__g___main____c
  %t9589 = load %NxVal, ptr @nx__g___main____i30
  %t9590 = add i64 30, 0
  %t9591 = extractvalue %NxVal %t9589, 1
  %t9592 = add i64 %t9591, %t9590
  %t9594 = getelementptr [2 x %NxVal], ptr %t9593, i64 0, i64 0
  store %NxVal %t9588, ptr %t9594
  %t9595 = call %NxVal @nx_int(i64 %t9592)
  %t9596 = getelementptr [2 x %NxVal], ptr %t9593, i64 0, i64 1
  store %NxVal %t9595, ptr %t9596
  %t9597 = getelementptr [2 x %NxVal], ptr %t9593, i64 0, i64 0
  %t9598 = call %NxVal @nx__m_3____main____Cell__m30(ptr %t9597, i64 2)
  %t9599 = extractvalue %NxVal %t9598, 1
  %t9600 = extractvalue %NxVal %t9587, 1
  %t9601 = add i64 %t9600, %t9599
  %t9602 = add i64 65535, 0
  %t9603 = and i64 %t9601, %t9602
  %t9604 = call %NxVal @nx_int(i64 %t9603)
  store %NxVal %t9604, ptr @nx__g___main____acc30
  %t9605 = load %NxVal, ptr @nx__g___main____acc30
  %t9606 = add i64 12, 0
  %t9607 = extractvalue %NxVal %t9605, 1
  %t9608 = and i64 %t9607, %t9606
  %t9609 = add i64 11, 0
  %t9610 = load %NxVal, ptr @nx__g___main____i30
  %t9611 = extractvalue %NxVal %t9610, 1
  %t9612 = add i64 %t9609, %t9611
  %t9613 = and i64 %t9608, %t9612
  %t9614 = add i64 65535, 0
  %t9615 = and i64 %t9613, %t9614
  %t9616 = call %NxVal @nx_int(i64 %t9615)
  store %NxVal %t9616, ptr @nx__g___main____acc30
  %t9617 = load %NxVal, ptr @nx__g___main____acc30
  %t9618 = add i64 38, 0
  %t9619 = extractvalue %NxVal %t9617, 1
  %t9620 = or i64 %t9619, %t9618
  %t9621 = add i64 9, 0
  %t9622 = load %NxVal, ptr @nx__g___main____i30
  %t9623 = extractvalue %NxVal %t9622, 1
  %t9624 = add i64 %t9621, %t9623
  %t9625 = or i64 %t9620, %t9624
  %t9626 = add i64 65535, 0
  %t9627 = and i64 %t9625, %t9626
  %t9628 = call %NxVal @nx_int(i64 %t9627)
  store %NxVal %t9628, ptr @nx__g___main____acc30
  %t9629 = load %NxVal, ptr @nx__g___main____acc30
  %t9630 = add i64 57, 0
  %t9631 = extractvalue %NxVal %t9629, 1
  %t9632 = xor i64 %t9631, %t9630
  %t9633 = add i64 30, 0
  %t9634 = load %NxVal, ptr @nx__g___main____i30
  %t9635 = extractvalue %NxVal %t9634, 1
  %t9636 = add i64 %t9633, %t9635
  %t9637 = xor i64 %t9632, %t9636
  %t9638 = add i64 65535, 0
  %t9639 = and i64 %t9637, %t9638
  %t9640 = call %NxVal @nx_int(i64 %t9639)
  store %NxVal %t9640, ptr @nx__g___main____acc30
  %t9641 = load %NxVal, ptr @nx__g___main____acc30
  %t9642 = add i64 31, 0
  %t9643 = extractvalue %NxVal %t9641, 1
  %t9644 = or i64 %t9643, %t9642
  %t9645 = add i64 38, 0
  %t9646 = load %NxVal, ptr @nx__g___main____i30
  %t9647 = extractvalue %NxVal %t9646, 1
  %t9648 = add i64 %t9645, %t9647
  %t9649 = or i64 %t9644, %t9648
  %t9650 = add i64 65535, 0
  %t9651 = and i64 %t9649, %t9650
  %t9652 = call %NxVal @nx_int(i64 %t9651)
  store %NxVal %t9652, ptr @nx__g___main____acc30
  %t9653 = load %NxVal, ptr @nx__g___main____i30
  %t9654 = add i64 1, 0
  %t9655 = extractvalue %NxVal %t9653, 1
  %t9656 = add i64 %t9655, %t9654
  %t9657 = call %NxVal @nx_int(i64 %t9656)
  store %NxVal %t9657, ptr @nx__g___main____i30
  br label %wcond93
wend95:
  %t9658 = load %NxVal, ptr @nx__g___main____total
  %t9659 = load %NxVal, ptr @nx__g___main____acc30
  %t9660 = extractvalue %NxVal %t9658, 1
  %t9661 = extractvalue %NxVal %t9659, 1
  %t9662 = add i64 %t9660, %t9661
  %t9663 = add i64 65535, 0
  %t9664 = and i64 %t9662, %t9663
  %t9665 = call %NxVal @nx_int(i64 %t9664)
  store %NxVal %t9665, ptr @nx__g___main____total
  %t9666 = add i64 0, 0
  %t9667 = call %NxVal @nx_int(i64 %t9666)
  store %NxVal %t9667, ptr @nx__g___main____i31
  %t9668 = add i64 0, 0
  %t9669 = call %NxVal @nx_int(i64 %t9668)
  store %NxVal %t9669, ptr @nx__g___main____acc31
  br label %wcond96
wcond96:
  %t9670 = load %NxVal, ptr @nx__g___main____i31
  %t9671 = add i64 3, 0
  %t9672 = extractvalue %NxVal %t9670, 1
  %t9673 = icmp slt i64 %t9672, %t9671
  br i1 %t9673, label %wbody97, label %wend98
wbody97:
  %t9674 = load %NxVal, ptr @nx__g___main____acc31
  %t9675 = load %NxVal, ptr @nx__g___main____c
  %t9676 = load %NxVal, ptr @nx__g___main____i31
  %t9677 = add i64 31, 0
  %t9678 = extractvalue %NxVal %t9676, 1
  %t9679 = add i64 %t9678, %t9677
  %t9681 = getelementptr [2 x %NxVal], ptr %t9680, i64 0, i64 0
  store %NxVal %t9675, ptr %t9681
  %t9682 = call %NxVal @nx_int(i64 %t9679)
  %t9683 = getelementptr [2 x %NxVal], ptr %t9680, i64 0, i64 1
  store %NxVal %t9682, ptr %t9683
  %t9684 = getelementptr [2 x %NxVal], ptr %t9680, i64 0, i64 0
  %t9685 = call %NxVal @nx__m_3____main____Cell__m31(ptr %t9684, i64 2)
  %t9686 = extractvalue %NxVal %t9685, 1
  %t9687 = extractvalue %NxVal %t9674, 1
  %t9688 = add i64 %t9687, %t9686
  %t9689 = add i64 65535, 0
  %t9690 = and i64 %t9688, %t9689
  %t9691 = call %NxVal @nx_int(i64 %t9690)
  store %NxVal %t9691, ptr @nx__g___main____acc31
  %t9692 = load %NxVal, ptr @nx__g___main____acc31
  %t9693 = add i64 77, 0
  %t9694 = extractvalue %NxVal %t9692, 1
  %t9695 = xor i64 %t9694, %t9693
  %t9696 = add i64 68, 0
  %t9697 = load %NxVal, ptr @nx__g___main____i31
  %t9698 = extractvalue %NxVal %t9697, 1
  %t9699 = add i64 %t9696, %t9698
  %t9700 = xor i64 %t9695, %t9699
  %t9701 = add i64 65535, 0
  %t9702 = and i64 %t9700, %t9701
  %t9703 = call %NxVal @nx_int(i64 %t9702)
  store %NxVal %t9703, ptr @nx__g___main____acc31
  %t9704 = load %NxVal, ptr @nx__g___main____acc31
  %t9705 = add i64 4, 0
  %t9706 = extractvalue %NxVal %t9704, 1
  %t9707 = and i64 %t9706, %t9705
  %t9708 = add i64 82, 0
  %t9709 = load %NxVal, ptr @nx__g___main____i31
  %t9710 = extractvalue %NxVal %t9709, 1
  %t9711 = add i64 %t9708, %t9710
  %t9712 = and i64 %t9707, %t9711
  %t9713 = add i64 65535, 0
  %t9714 = and i64 %t9712, %t9713
  %t9715 = call %NxVal @nx_int(i64 %t9714)
  store %NxVal %t9715, ptr @nx__g___main____acc31
  %t9716 = load %NxVal, ptr @nx__g___main____acc31
  %t9717 = add i64 32, 0
  %t9718 = extractvalue %NxVal %t9716, 1
  %t9719 = sub i64 %t9718, %t9717
  %t9720 = add i64 53, 0
  %t9721 = sub i64 %t9719, %t9720
  %t9722 = load %NxVal, ptr @nx__g___main____i31
  %t9723 = extractvalue %NxVal %t9722, 1
  %t9724 = add i64 %t9721, %t9723
  %t9725 = add i64 65535, 0
  %t9726 = and i64 %t9724, %t9725
  %t9727 = call %NxVal @nx_int(i64 %t9726)
  store %NxVal %t9727, ptr @nx__g___main____acc31
  %t9728 = load %NxVal, ptr @nx__g___main____acc31
  %t9729 = add i64 4, 0
  %t9730 = extractvalue %NxVal %t9728, 1
  %t9731 = sub i64 %t9730, %t9729
  %t9732 = add i64 89, 0
  %t9733 = sub i64 %t9731, %t9732
  %t9734 = load %NxVal, ptr @nx__g___main____i31
  %t9735 = extractvalue %NxVal %t9734, 1
  %t9736 = add i64 %t9733, %t9735
  %t9737 = add i64 65535, 0
  %t9738 = and i64 %t9736, %t9737
  %t9739 = call %NxVal @nx_int(i64 %t9738)
  store %NxVal %t9739, ptr @nx__g___main____acc31
  %t9740 = load %NxVal, ptr @nx__g___main____i31
  %t9741 = add i64 1, 0
  %t9742 = extractvalue %NxVal %t9740, 1
  %t9743 = add i64 %t9742, %t9741
  %t9744 = call %NxVal @nx_int(i64 %t9743)
  store %NxVal %t9744, ptr @nx__g___main____i31
  br label %wcond96
wend98:
  %t9745 = load %NxVal, ptr @nx__g___main____total
  %t9746 = load %NxVal, ptr @nx__g___main____acc31
  %t9747 = extractvalue %NxVal %t9745, 1
  %t9748 = extractvalue %NxVal %t9746, 1
  %t9749 = add i64 %t9747, %t9748
  %t9750 = add i64 65535, 0
  %t9751 = and i64 %t9749, %t9750
  %t9752 = call %NxVal @nx_int(i64 %t9751)
  store %NxVal %t9752, ptr @nx__g___main____total
  %t9753 = add i64 0, 0
  %t9754 = call %NxVal @nx_int(i64 %t9753)
  store %NxVal %t9754, ptr @nx__g___main____i32
  %t9755 = add i64 0, 0
  %t9756 = call %NxVal @nx_int(i64 %t9755)
  store %NxVal %t9756, ptr @nx__g___main____acc32
  br label %wcond99
wcond99:
  %t9757 = load %NxVal, ptr @nx__g___main____i32
  %t9758 = add i64 3, 0
  %t9759 = extractvalue %NxVal %t9757, 1
  %t9760 = icmp slt i64 %t9759, %t9758
  br i1 %t9760, label %wbody100, label %wend101
wbody100:
  %t9761 = load %NxVal, ptr @nx__g___main____acc32
  %t9762 = load %NxVal, ptr @nx__g___main____c
  %t9763 = load %NxVal, ptr @nx__g___main____i32
  %t9764 = add i64 32, 0
  %t9765 = extractvalue %NxVal %t9763, 1
  %t9766 = add i64 %t9765, %t9764
  %t9768 = getelementptr [2 x %NxVal], ptr %t9767, i64 0, i64 0
  store %NxVal %t9762, ptr %t9768
  %t9769 = call %NxVal @nx_int(i64 %t9766)
  %t9770 = getelementptr [2 x %NxVal], ptr %t9767, i64 0, i64 1
  store %NxVal %t9769, ptr %t9770
  %t9771 = getelementptr [2 x %NxVal], ptr %t9767, i64 0, i64 0
  %t9772 = call %NxVal @nx__m_3____main____Cell__m32(ptr %t9771, i64 2)
  %t9773 = extractvalue %NxVal %t9772, 1
  %t9774 = extractvalue %NxVal %t9761, 1
  %t9775 = add i64 %t9774, %t9773
  %t9776 = add i64 65535, 0
  %t9777 = and i64 %t9775, %t9776
  %t9778 = call %NxVal @nx_int(i64 %t9777)
  store %NxVal %t9778, ptr @nx__g___main____acc32
  %t9779 = load %NxVal, ptr @nx__g___main____acc32
  %t9780 = add i64 27, 0
  %t9781 = extractvalue %NxVal %t9779, 1
  %t9782 = sub i64 %t9781, %t9780
  %t9783 = add i64 8, 0
  %t9784 = sub i64 %t9782, %t9783
  %t9785 = load %NxVal, ptr @nx__g___main____i32
  %t9786 = extractvalue %NxVal %t9785, 1
  %t9787 = add i64 %t9784, %t9786
  %t9788 = add i64 65535, 0
  %t9789 = and i64 %t9787, %t9788
  %t9790 = call %NxVal @nx_int(i64 %t9789)
  store %NxVal %t9790, ptr @nx__g___main____acc32
  %t9791 = load %NxVal, ptr @nx__g___main____acc32
  %t9792 = add i64 50, 0
  %t9793 = extractvalue %NxVal %t9791, 1
  %t9794 = or i64 %t9793, %t9792
  %t9795 = add i64 88, 0
  %t9796 = load %NxVal, ptr @nx__g___main____i32
  %t9797 = extractvalue %NxVal %t9796, 1
  %t9798 = add i64 %t9795, %t9797
  %t9799 = or i64 %t9794, %t9798
  %t9800 = add i64 65535, 0
  %t9801 = and i64 %t9799, %t9800
  %t9802 = call %NxVal @nx_int(i64 %t9801)
  store %NxVal %t9802, ptr @nx__g___main____acc32
  %t9803 = load %NxVal, ptr @nx__g___main____acc32
  %t9804 = add i64 61, 0
  %t9805 = extractvalue %NxVal %t9803, 1
  %t9806 = and i64 %t9805, %t9804
  %t9807 = add i64 38, 0
  %t9808 = load %NxVal, ptr @nx__g___main____i32
  %t9809 = extractvalue %NxVal %t9808, 1
  %t9810 = add i64 %t9807, %t9809
  %t9811 = and i64 %t9806, %t9810
  %t9812 = add i64 65535, 0
  %t9813 = and i64 %t9811, %t9812
  %t9814 = call %NxVal @nx_int(i64 %t9813)
  store %NxVal %t9814, ptr @nx__g___main____acc32
  %t9815 = load %NxVal, ptr @nx__g___main____acc32
  %t9816 = add i64 32, 0
  %t9817 = extractvalue %NxVal %t9815, 1
  %t9818 = xor i64 %t9817, %t9816
  %t9819 = add i64 14, 0
  %t9820 = load %NxVal, ptr @nx__g___main____i32
  %t9821 = extractvalue %NxVal %t9820, 1
  %t9822 = add i64 %t9819, %t9821
  %t9823 = xor i64 %t9818, %t9822
  %t9824 = add i64 65535, 0
  %t9825 = and i64 %t9823, %t9824
  %t9826 = call %NxVal @nx_int(i64 %t9825)
  store %NxVal %t9826, ptr @nx__g___main____acc32
  %t9827 = load %NxVal, ptr @nx__g___main____i32
  %t9828 = add i64 1, 0
  %t9829 = extractvalue %NxVal %t9827, 1
  %t9830 = add i64 %t9829, %t9828
  %t9831 = call %NxVal @nx_int(i64 %t9830)
  store %NxVal %t9831, ptr @nx__g___main____i32
  br label %wcond99
wend101:
  %t9832 = load %NxVal, ptr @nx__g___main____total
  %t9833 = load %NxVal, ptr @nx__g___main____acc32
  %t9834 = extractvalue %NxVal %t9832, 1
  %t9835 = extractvalue %NxVal %t9833, 1
  %t9836 = add i64 %t9834, %t9835
  %t9837 = add i64 65535, 0
  %t9838 = and i64 %t9836, %t9837
  %t9839 = call %NxVal @nx_int(i64 %t9838)
  store %NxVal %t9839, ptr @nx__g___main____total
  %t9840 = add i64 0, 0
  %t9841 = call %NxVal @nx_int(i64 %t9840)
  store %NxVal %t9841, ptr @nx__g___main____i33
  %t9842 = add i64 0, 0
  %t9843 = call %NxVal @nx_int(i64 %t9842)
  store %NxVal %t9843, ptr @nx__g___main____acc33
  br label %wcond102
wcond102:
  %t9844 = load %NxVal, ptr @nx__g___main____i33
  %t9845 = add i64 3, 0
  %t9846 = extractvalue %NxVal %t9844, 1
  %t9847 = icmp slt i64 %t9846, %t9845
  br i1 %t9847, label %wbody103, label %wend104
wbody103:
  %t9848 = load %NxVal, ptr @nx__g___main____acc33
  %t9849 = load %NxVal, ptr @nx__g___main____c
  %t9850 = load %NxVal, ptr @nx__g___main____i33
  %t9851 = add i64 33, 0
  %t9852 = extractvalue %NxVal %t9850, 1
  %t9853 = add i64 %t9852, %t9851
  %t9855 = getelementptr [2 x %NxVal], ptr %t9854, i64 0, i64 0
  store %NxVal %t9849, ptr %t9855
  %t9856 = call %NxVal @nx_int(i64 %t9853)
  %t9857 = getelementptr [2 x %NxVal], ptr %t9854, i64 0, i64 1
  store %NxVal %t9856, ptr %t9857
  %t9858 = getelementptr [2 x %NxVal], ptr %t9854, i64 0, i64 0
  %t9859 = call %NxVal @nx__m_3____main____Cell__m33(ptr %t9858, i64 2)
  %t9860 = extractvalue %NxVal %t9859, 1
  %t9861 = extractvalue %NxVal %t9848, 1
  %t9862 = add i64 %t9861, %t9860
  %t9863 = add i64 65535, 0
  %t9864 = and i64 %t9862, %t9863
  %t9865 = call %NxVal @nx_int(i64 %t9864)
  store %NxVal %t9865, ptr @nx__g___main____acc33
  %t9866 = load %NxVal, ptr @nx__g___main____acc33
  %t9867 = add i64 83, 0
  %t9868 = extractvalue %NxVal %t9866, 1
  %t9869 = and i64 %t9868, %t9867
  %t9870 = add i64 10, 0
  %t9871 = load %NxVal, ptr @nx__g___main____i33
  %t9872 = extractvalue %NxVal %t9871, 1
  %t9873 = add i64 %t9870, %t9872
  %t9874 = and i64 %t9869, %t9873
  %t9875 = add i64 65535, 0
  %t9876 = and i64 %t9874, %t9875
  %t9877 = call %NxVal @nx_int(i64 %t9876)
  store %NxVal %t9877, ptr @nx__g___main____acc33
  %t9878 = load %NxVal, ptr @nx__g___main____acc33
  %t9879 = add i64 94, 0
  %t9880 = extractvalue %NxVal %t9878, 1
  %t9881 = sub i64 %t9880, %t9879
  %t9882 = add i64 24, 0
  %t9883 = sub i64 %t9881, %t9882
  %t9884 = load %NxVal, ptr @nx__g___main____i33
  %t9885 = extractvalue %NxVal %t9884, 1
  %t9886 = add i64 %t9883, %t9885
  %t9887 = add i64 65535, 0
  %t9888 = and i64 %t9886, %t9887
  %t9889 = call %NxVal @nx_int(i64 %t9888)
  store %NxVal %t9889, ptr @nx__g___main____acc33
  %t9890 = load %NxVal, ptr @nx__g___main____acc33
  %t9891 = add i64 18, 0
  %t9892 = extractvalue %NxVal %t9890, 1
  %t9893 = add i64 %t9892, %t9891
  %t9894 = add i64 17, 0
  %t9895 = add i64 %t9893, %t9894
  %t9896 = load %NxVal, ptr @nx__g___main____i33
  %t9897 = extractvalue %NxVal %t9896, 1
  %t9898 = add i64 %t9895, %t9897
  %t9899 = add i64 65535, 0
  %t9900 = and i64 %t9898, %t9899
  %t9901 = call %NxVal @nx_int(i64 %t9900)
  store %NxVal %t9901, ptr @nx__g___main____acc33
  %t9902 = load %NxVal, ptr @nx__g___main____acc33
  %t9903 = add i64 15, 0
  %t9904 = extractvalue %NxVal %t9902, 1
  %t9905 = and i64 %t9904, %t9903
  %t9906 = add i64 19, 0
  %t9907 = load %NxVal, ptr @nx__g___main____i33
  %t9908 = extractvalue %NxVal %t9907, 1
  %t9909 = add i64 %t9906, %t9908
  %t9910 = and i64 %t9905, %t9909
  %t9911 = add i64 65535, 0
  %t9912 = and i64 %t9910, %t9911
  %t9913 = call %NxVal @nx_int(i64 %t9912)
  store %NxVal %t9913, ptr @nx__g___main____acc33
  %t9914 = load %NxVal, ptr @nx__g___main____i33
  %t9915 = add i64 1, 0
  %t9916 = extractvalue %NxVal %t9914, 1
  %t9917 = add i64 %t9916, %t9915
  %t9918 = call %NxVal @nx_int(i64 %t9917)
  store %NxVal %t9918, ptr @nx__g___main____i33
  br label %wcond102
wend104:
  %t9919 = load %NxVal, ptr @nx__g___main____total
  %t9920 = load %NxVal, ptr @nx__g___main____acc33
  %t9921 = extractvalue %NxVal %t9919, 1
  %t9922 = extractvalue %NxVal %t9920, 1
  %t9923 = add i64 %t9921, %t9922
  %t9924 = add i64 65535, 0
  %t9925 = and i64 %t9923, %t9924
  %t9926 = call %NxVal @nx_int(i64 %t9925)
  store %NxVal %t9926, ptr @nx__g___main____total
  %t9927 = add i64 0, 0
  %t9928 = call %NxVal @nx_int(i64 %t9927)
  store %NxVal %t9928, ptr @nx__g___main____i34
  %t9929 = add i64 0, 0
  %t9930 = call %NxVal @nx_int(i64 %t9929)
  store %NxVal %t9930, ptr @nx__g___main____acc34
  br label %wcond105
wcond105:
  %t9931 = load %NxVal, ptr @nx__g___main____i34
  %t9932 = add i64 3, 0
  %t9933 = extractvalue %NxVal %t9931, 1
  %t9934 = icmp slt i64 %t9933, %t9932
  br i1 %t9934, label %wbody106, label %wend107
wbody106:
  %t9935 = load %NxVal, ptr @nx__g___main____acc34
  %t9936 = load %NxVal, ptr @nx__g___main____c
  %t9937 = load %NxVal, ptr @nx__g___main____i34
  %t9938 = add i64 34, 0
  %t9939 = extractvalue %NxVal %t9937, 1
  %t9940 = add i64 %t9939, %t9938
  %t9942 = getelementptr [2 x %NxVal], ptr %t9941, i64 0, i64 0
  store %NxVal %t9936, ptr %t9942
  %t9943 = call %NxVal @nx_int(i64 %t9940)
  %t9944 = getelementptr [2 x %NxVal], ptr %t9941, i64 0, i64 1
  store %NxVal %t9943, ptr %t9944
  %t9945 = getelementptr [2 x %NxVal], ptr %t9941, i64 0, i64 0
  %t9946 = call %NxVal @nx__m_3____main____Cell__m34(ptr %t9945, i64 2)
  %t9947 = extractvalue %NxVal %t9946, 1
  %t9948 = extractvalue %NxVal %t9935, 1
  %t9949 = add i64 %t9948, %t9947
  %t9950 = add i64 65535, 0
  %t9951 = and i64 %t9949, %t9950
  %t9952 = call %NxVal @nx_int(i64 %t9951)
  store %NxVal %t9952, ptr @nx__g___main____acc34
  %t9953 = load %NxVal, ptr @nx__g___main____acc34
  %t9954 = add i64 57, 0
  %t9955 = extractvalue %NxVal %t9953, 1
  %t9956 = xor i64 %t9955, %t9954
  %t9957 = add i64 77, 0
  %t9958 = load %NxVal, ptr @nx__g___main____i34
  %t9959 = extractvalue %NxVal %t9958, 1
  %t9960 = add i64 %t9957, %t9959
  %t9961 = xor i64 %t9956, %t9960
  %t9962 = add i64 65535, 0
  %t9963 = and i64 %t9961, %t9962
  %t9964 = call %NxVal @nx_int(i64 %t9963)
  store %NxVal %t9964, ptr @nx__g___main____acc34
  %t9965 = load %NxVal, ptr @nx__g___main____acc34
  %t9966 = add i64 68, 0
  %t9967 = extractvalue %NxVal %t9965, 1
  %t9968 = or i64 %t9967, %t9966
  %t9969 = add i64 39, 0
  %t9970 = load %NxVal, ptr @nx__g___main____i34
  %t9971 = extractvalue %NxVal %t9970, 1
  %t9972 = add i64 %t9969, %t9971
  %t9973 = or i64 %t9968, %t9972
  %t9974 = add i64 65535, 0
  %t9975 = and i64 %t9973, %t9974
  %t9976 = call %NxVal @nx_int(i64 %t9975)
  store %NxVal %t9976, ptr @nx__g___main____acc34
  %t9977 = load %NxVal, ptr @nx__g___main____acc34
  %t9978 = add i64 18, 0
  %t9979 = extractvalue %NxVal %t9977, 1
  %t9980 = mul i64 %t9979, %t9978
  %t9981 = add i64 25, 0
  %t9982 = mul i64 %t9980, %t9981
  %t9983 = load %NxVal, ptr @nx__g___main____i34
  %t9984 = extractvalue %NxVal %t9983, 1
  %t9985 = add i64 %t9982, %t9984
  %t9986 = add i64 65535, 0
  %t9987 = and i64 %t9985, %t9986
  %t9988 = call %NxVal @nx_int(i64 %t9987)
  store %NxVal %t9988, ptr @nx__g___main____acc34
  %t9989 = load %NxVal, ptr @nx__g___main____acc34
  %t9990 = add i64 36, 0
  %t9991 = extractvalue %NxVal %t9989, 1
  %t9992 = xor i64 %t9991, %t9990
  %t9993 = add i64 60, 0
  %t9994 = load %NxVal, ptr @nx__g___main____i34
  %t9995 = extractvalue %NxVal %t9994, 1
  %t9996 = add i64 %t9993, %t9995
  %t9997 = xor i64 %t9992, %t9996
  %t9998 = add i64 65535, 0
  %t9999 = and i64 %t9997, %t9998
  %t10000 = call %NxVal @nx_int(i64 %t9999)
  store %NxVal %t10000, ptr @nx__g___main____acc34
  %t10001 = load %NxVal, ptr @nx__g___main____i34
  %t10002 = add i64 1, 0
  %t10003 = extractvalue %NxVal %t10001, 1
  %t10004 = add i64 %t10003, %t10002
  %t10005 = call %NxVal @nx_int(i64 %t10004)
  store %NxVal %t10005, ptr @nx__g___main____i34
  br label %wcond105
wend107:
  %t10006 = load %NxVal, ptr @nx__g___main____total
  %t10007 = load %NxVal, ptr @nx__g___main____acc34
  %t10008 = extractvalue %NxVal %t10006, 1
  %t10009 = extractvalue %NxVal %t10007, 1
  %t10010 = add i64 %t10008, %t10009
  %t10011 = add i64 65535, 0
  %t10012 = and i64 %t10010, %t10011
  %t10013 = call %NxVal @nx_int(i64 %t10012)
  store %NxVal %t10013, ptr @nx__g___main____total
  %t10014 = add i64 0, 0
  %t10015 = call %NxVal @nx_int(i64 %t10014)
  store %NxVal %t10015, ptr @nx__g___main____i35
  %t10016 = add i64 0, 0
  %t10017 = call %NxVal @nx_int(i64 %t10016)
  store %NxVal %t10017, ptr @nx__g___main____acc35
  br label %wcond108
wcond108:
  %t10018 = load %NxVal, ptr @nx__g___main____i35
  %t10019 = add i64 3, 0
  %t10020 = extractvalue %NxVal %t10018, 1
  %t10021 = icmp slt i64 %t10020, %t10019
  br i1 %t10021, label %wbody109, label %wend110
wbody109:
  %t10022 = load %NxVal, ptr @nx__g___main____acc35
  %t10023 = load %NxVal, ptr @nx__g___main____c
  %t10024 = load %NxVal, ptr @nx__g___main____i35
  %t10025 = add i64 35, 0
  %t10026 = extractvalue %NxVal %t10024, 1
  %t10027 = add i64 %t10026, %t10025
  %t10029 = getelementptr [2 x %NxVal], ptr %t10028, i64 0, i64 0
  store %NxVal %t10023, ptr %t10029
  %t10030 = call %NxVal @nx_int(i64 %t10027)
  %t10031 = getelementptr [2 x %NxVal], ptr %t10028, i64 0, i64 1
  store %NxVal %t10030, ptr %t10031
  %t10032 = getelementptr [2 x %NxVal], ptr %t10028, i64 0, i64 0
  %t10033 = call %NxVal @nx__m_3____main____Cell__m35(ptr %t10032, i64 2)
  %t10034 = extractvalue %NxVal %t10033, 1
  %t10035 = extractvalue %NxVal %t10022, 1
  %t10036 = add i64 %t10035, %t10034
  %t10037 = add i64 65535, 0
  %t10038 = and i64 %t10036, %t10037
  %t10039 = call %NxVal @nx_int(i64 %t10038)
  store %NxVal %t10039, ptr @nx__g___main____acc35
  %t10040 = load %NxVal, ptr @nx__g___main____acc35
  %t10041 = add i64 27, 0
  %t10042 = extractvalue %NxVal %t10040, 1
  %t10043 = xor i64 %t10042, %t10041
  %t10044 = add i64 44, 0
  %t10045 = load %NxVal, ptr @nx__g___main____i35
  %t10046 = extractvalue %NxVal %t10045, 1
  %t10047 = add i64 %t10044, %t10046
  %t10048 = xor i64 %t10043, %t10047
  %t10049 = add i64 65535, 0
  %t10050 = and i64 %t10048, %t10049
  %t10051 = call %NxVal @nx_int(i64 %t10050)
  store %NxVal %t10051, ptr @nx__g___main____acc35
  %t10052 = load %NxVal, ptr @nx__g___main____acc35
  %t10053 = add i64 41, 0
  %t10054 = extractvalue %NxVal %t10052, 1
  %t10055 = call i64 @nx_mod_i64(i64 %t10054, i64 %t10053)
  %t10056 = add i64 68, 0
  %t10057 = call i64 @nx_mod_i64(i64 %t10055, i64 %t10056)
  %t10058 = load %NxVal, ptr @nx__g___main____i35
  %t10059 = extractvalue %NxVal %t10058, 1
  %t10060 = add i64 %t10057, %t10059
  %t10061 = add i64 65535, 0
  %t10062 = and i64 %t10060, %t10061
  %t10063 = call %NxVal @nx_int(i64 %t10062)
  store %NxVal %t10063, ptr @nx__g___main____acc35
  %t10064 = load %NxVal, ptr @nx__g___main____acc35
  %t10065 = add i64 94, 0
  %t10066 = extractvalue %NxVal %t10064, 1
  %t10067 = call i64 @nx_mod_i64(i64 %t10066, i64 %t10065)
  %t10068 = add i64 47, 0
  %t10069 = call i64 @nx_mod_i64(i64 %t10067, i64 %t10068)
  %t10070 = load %NxVal, ptr @nx__g___main____i35
  %t10071 = extractvalue %NxVal %t10070, 1
  %t10072 = add i64 %t10069, %t10071
  %t10073 = add i64 65535, 0
  %t10074 = and i64 %t10072, %t10073
  %t10075 = call %NxVal @nx_int(i64 %t10074)
  store %NxVal %t10075, ptr @nx__g___main____acc35
  %t10076 = load %NxVal, ptr @nx__g___main____acc35
  %t10077 = add i64 73, 0
  %t10078 = extractvalue %NxVal %t10076, 1
  %t10079 = and i64 %t10078, %t10077
  %t10080 = add i64 77, 0
  %t10081 = load %NxVal, ptr @nx__g___main____i35
  %t10082 = extractvalue %NxVal %t10081, 1
  %t10083 = add i64 %t10080, %t10082
  %t10084 = and i64 %t10079, %t10083
  %t10085 = add i64 65535, 0
  %t10086 = and i64 %t10084, %t10085
  %t10087 = call %NxVal @nx_int(i64 %t10086)
  store %NxVal %t10087, ptr @nx__g___main____acc35
  %t10088 = load %NxVal, ptr @nx__g___main____i35
  %t10089 = add i64 1, 0
  %t10090 = extractvalue %NxVal %t10088, 1
  %t10091 = add i64 %t10090, %t10089
  %t10092 = call %NxVal @nx_int(i64 %t10091)
  store %NxVal %t10092, ptr @nx__g___main____i35
  br label %wcond108
wend110:
  %t10093 = load %NxVal, ptr @nx__g___main____total
  %t10094 = load %NxVal, ptr @nx__g___main____acc35
  %t10095 = extractvalue %NxVal %t10093, 1
  %t10096 = extractvalue %NxVal %t10094, 1
  %t10097 = add i64 %t10095, %t10096
  %t10098 = add i64 65535, 0
  %t10099 = and i64 %t10097, %t10098
  %t10100 = call %NxVal @nx_int(i64 %t10099)
  store %NxVal %t10100, ptr @nx__g___main____total
  %t10101 = add i64 0, 0
  %t10102 = call %NxVal @nx_int(i64 %t10101)
  store %NxVal %t10102, ptr @nx__g___main____i36
  %t10103 = add i64 0, 0
  %t10104 = call %NxVal @nx_int(i64 %t10103)
  store %NxVal %t10104, ptr @nx__g___main____acc36
  br label %wcond111
wcond111:
  %t10105 = load %NxVal, ptr @nx__g___main____i36
  %t10106 = add i64 3, 0
  %t10107 = extractvalue %NxVal %t10105, 1
  %t10108 = icmp slt i64 %t10107, %t10106
  br i1 %t10108, label %wbody112, label %wend113
wbody112:
  %t10109 = load %NxVal, ptr @nx__g___main____acc36
  %t10110 = load %NxVal, ptr @nx__g___main____c
  %t10111 = load %NxVal, ptr @nx__g___main____i36
  %t10112 = add i64 36, 0
  %t10113 = extractvalue %NxVal %t10111, 1
  %t10114 = add i64 %t10113, %t10112
  %t10116 = getelementptr [2 x %NxVal], ptr %t10115, i64 0, i64 0
  store %NxVal %t10110, ptr %t10116
  %t10117 = call %NxVal @nx_int(i64 %t10114)
  %t10118 = getelementptr [2 x %NxVal], ptr %t10115, i64 0, i64 1
  store %NxVal %t10117, ptr %t10118
  %t10119 = getelementptr [2 x %NxVal], ptr %t10115, i64 0, i64 0
  %t10120 = call %NxVal @nx__m_3____main____Cell__m36(ptr %t10119, i64 2)
  %t10121 = extractvalue %NxVal %t10120, 1
  %t10122 = extractvalue %NxVal %t10109, 1
  %t10123 = add i64 %t10122, %t10121
  %t10124 = add i64 65535, 0
  %t10125 = and i64 %t10123, %t10124
  %t10126 = call %NxVal @nx_int(i64 %t10125)
  store %NxVal %t10126, ptr @nx__g___main____acc36
  %t10127 = load %NxVal, ptr @nx__g___main____acc36
  %t10128 = add i64 97, 0
  %t10129 = extractvalue %NxVal %t10127, 1
  %t10130 = and i64 %t10129, %t10128
  %t10131 = add i64 49, 0
  %t10132 = load %NxVal, ptr @nx__g___main____i36
  %t10133 = extractvalue %NxVal %t10132, 1
  %t10134 = add i64 %t10131, %t10133
  %t10135 = and i64 %t10130, %t10134
  %t10136 = add i64 65535, 0
  %t10137 = and i64 %t10135, %t10136
  %t10138 = call %NxVal @nx_int(i64 %t10137)
  store %NxVal %t10138, ptr @nx__g___main____acc36
  %t10139 = load %NxVal, ptr @nx__g___main____acc36
  %t10140 = add i64 36, 0
  %t10141 = extractvalue %NxVal %t10139, 1
  %t10142 = add i64 %t10141, %t10140
  %t10143 = add i64 67, 0
  %t10144 = add i64 %t10142, %t10143
  %t10145 = load %NxVal, ptr @nx__g___main____i36
  %t10146 = extractvalue %NxVal %t10145, 1
  %t10147 = add i64 %t10144, %t10146
  %t10148 = add i64 65535, 0
  %t10149 = and i64 %t10147, %t10148
  %t10150 = call %NxVal @nx_int(i64 %t10149)
  store %NxVal %t10150, ptr @nx__g___main____acc36
  %t10151 = load %NxVal, ptr @nx__g___main____acc36
  %t10152 = add i64 75, 0
  %t10153 = extractvalue %NxVal %t10151, 1
  %t10154 = and i64 %t10153, %t10152
  %t10155 = add i64 68, 0
  %t10156 = load %NxVal, ptr @nx__g___main____i36
  %t10157 = extractvalue %NxVal %t10156, 1
  %t10158 = add i64 %t10155, %t10157
  %t10159 = and i64 %t10154, %t10158
  %t10160 = add i64 65535, 0
  %t10161 = and i64 %t10159, %t10160
  %t10162 = call %NxVal @nx_int(i64 %t10161)
  store %NxVal %t10162, ptr @nx__g___main____acc36
  %t10163 = load %NxVal, ptr @nx__g___main____acc36
  %t10164 = add i64 25, 0
  %t10165 = extractvalue %NxVal %t10163, 1
  %t10166 = add i64 %t10165, %t10164
  %t10167 = add i64 70, 0
  %t10168 = add i64 %t10166, %t10167
  %t10169 = load %NxVal, ptr @nx__g___main____i36
  %t10170 = extractvalue %NxVal %t10169, 1
  %t10171 = add i64 %t10168, %t10170
  %t10172 = add i64 65535, 0
  %t10173 = and i64 %t10171, %t10172
  %t10174 = call %NxVal @nx_int(i64 %t10173)
  store %NxVal %t10174, ptr @nx__g___main____acc36
  %t10175 = load %NxVal, ptr @nx__g___main____i36
  %t10176 = add i64 1, 0
  %t10177 = extractvalue %NxVal %t10175, 1
  %t10178 = add i64 %t10177, %t10176
  %t10179 = call %NxVal @nx_int(i64 %t10178)
  store %NxVal %t10179, ptr @nx__g___main____i36
  br label %wcond111
wend113:
  %t10180 = load %NxVal, ptr @nx__g___main____total
  %t10181 = load %NxVal, ptr @nx__g___main____acc36
  %t10182 = extractvalue %NxVal %t10180, 1
  %t10183 = extractvalue %NxVal %t10181, 1
  %t10184 = add i64 %t10182, %t10183
  %t10185 = add i64 65535, 0
  %t10186 = and i64 %t10184, %t10185
  %t10187 = call %NxVal @nx_int(i64 %t10186)
  store %NxVal %t10187, ptr @nx__g___main____total
  %t10188 = add i64 0, 0
  %t10189 = call %NxVal @nx_int(i64 %t10188)
  store %NxVal %t10189, ptr @nx__g___main____i37
  %t10190 = add i64 0, 0
  %t10191 = call %NxVal @nx_int(i64 %t10190)
  store %NxVal %t10191, ptr @nx__g___main____acc37
  br label %wcond114
wcond114:
  %t10192 = load %NxVal, ptr @nx__g___main____i37
  %t10193 = add i64 3, 0
  %t10194 = extractvalue %NxVal %t10192, 1
  %t10195 = icmp slt i64 %t10194, %t10193
  br i1 %t10195, label %wbody115, label %wend116
wbody115:
  %t10196 = load %NxVal, ptr @nx__g___main____acc37
  %t10197 = load %NxVal, ptr @nx__g___main____c
  %t10198 = load %NxVal, ptr @nx__g___main____i37
  %t10199 = add i64 37, 0
  %t10200 = extractvalue %NxVal %t10198, 1
  %t10201 = add i64 %t10200, %t10199
  %t10203 = getelementptr [2 x %NxVal], ptr %t10202, i64 0, i64 0
  store %NxVal %t10197, ptr %t10203
  %t10204 = call %NxVal @nx_int(i64 %t10201)
  %t10205 = getelementptr [2 x %NxVal], ptr %t10202, i64 0, i64 1
  store %NxVal %t10204, ptr %t10205
  %t10206 = getelementptr [2 x %NxVal], ptr %t10202, i64 0, i64 0
  %t10207 = call %NxVal @nx__m_3____main____Cell__m37(ptr %t10206, i64 2)
  %t10208 = extractvalue %NxVal %t10207, 1
  %t10209 = extractvalue %NxVal %t10196, 1
  %t10210 = add i64 %t10209, %t10208
  %t10211 = add i64 65535, 0
  %t10212 = and i64 %t10210, %t10211
  %t10213 = call %NxVal @nx_int(i64 %t10212)
  store %NxVal %t10213, ptr @nx__g___main____acc37
  %t10214 = load %NxVal, ptr @nx__g___main____acc37
  %t10215 = add i64 71, 0
  %t10216 = extractvalue %NxVal %t10214, 1
  %t10217 = and i64 %t10216, %t10215
  %t10218 = add i64 19, 0
  %t10219 = load %NxVal, ptr @nx__g___main____i37
  %t10220 = extractvalue %NxVal %t10219, 1
  %t10221 = add i64 %t10218, %t10220
  %t10222 = and i64 %t10217, %t10221
  %t10223 = add i64 65535, 0
  %t10224 = and i64 %t10222, %t10223
  %t10225 = call %NxVal @nx_int(i64 %t10224)
  store %NxVal %t10225, ptr @nx__g___main____acc37
  %t10226 = load %NxVal, ptr @nx__g___main____acc37
  %t10227 = add i64 39, 0
  %t10228 = extractvalue %NxVal %t10226, 1
  %t10229 = and i64 %t10228, %t10227
  %t10230 = add i64 3, 0
  %t10231 = load %NxVal, ptr @nx__g___main____i37
  %t10232 = extractvalue %NxVal %t10231, 1
  %t10233 = add i64 %t10230, %t10232
  %t10234 = and i64 %t10229, %t10233
  %t10235 = add i64 65535, 0
  %t10236 = and i64 %t10234, %t10235
  %t10237 = call %NxVal @nx_int(i64 %t10236)
  store %NxVal %t10237, ptr @nx__g___main____acc37
  %t10238 = load %NxVal, ptr @nx__g___main____acc37
  %t10239 = add i64 61, 0
  %t10240 = extractvalue %NxVal %t10238, 1
  %t10241 = call i64 @nx_mod_i64(i64 %t10240, i64 %t10239)
  %t10242 = add i64 85, 0
  %t10243 = call i64 @nx_mod_i64(i64 %t10241, i64 %t10242)
  %t10244 = load %NxVal, ptr @nx__g___main____i37
  %t10245 = extractvalue %NxVal %t10244, 1
  %t10246 = add i64 %t10243, %t10245
  %t10247 = add i64 65535, 0
  %t10248 = and i64 %t10246, %t10247
  %t10249 = call %NxVal @nx_int(i64 %t10248)
  store %NxVal %t10249, ptr @nx__g___main____acc37
  %t10250 = load %NxVal, ptr @nx__g___main____acc37
  %t10251 = add i64 10, 0
  %t10252 = extractvalue %NxVal %t10250, 1
  %t10253 = add i64 %t10252, %t10251
  %t10254 = add i64 88, 0
  %t10255 = add i64 %t10253, %t10254
  %t10256 = load %NxVal, ptr @nx__g___main____i37
  %t10257 = extractvalue %NxVal %t10256, 1
  %t10258 = add i64 %t10255, %t10257
  %t10259 = add i64 65535, 0
  %t10260 = and i64 %t10258, %t10259
  %t10261 = call %NxVal @nx_int(i64 %t10260)
  store %NxVal %t10261, ptr @nx__g___main____acc37
  %t10262 = load %NxVal, ptr @nx__g___main____i37
  %t10263 = add i64 1, 0
  %t10264 = extractvalue %NxVal %t10262, 1
  %t10265 = add i64 %t10264, %t10263
  %t10266 = call %NxVal @nx_int(i64 %t10265)
  store %NxVal %t10266, ptr @nx__g___main____i37
  br label %wcond114
wend116:
  %t10267 = load %NxVal, ptr @nx__g___main____total
  %t10268 = load %NxVal, ptr @nx__g___main____acc37
  %t10269 = extractvalue %NxVal %t10267, 1
  %t10270 = extractvalue %NxVal %t10268, 1
  %t10271 = add i64 %t10269, %t10270
  %t10272 = add i64 65535, 0
  %t10273 = and i64 %t10271, %t10272
  %t10274 = call %NxVal @nx_int(i64 %t10273)
  store %NxVal %t10274, ptr @nx__g___main____total
  %t10275 = add i64 0, 0
  %t10276 = call %NxVal @nx_int(i64 %t10275)
  store %NxVal %t10276, ptr @nx__g___main____i38
  %t10277 = add i64 0, 0
  %t10278 = call %NxVal @nx_int(i64 %t10277)
  store %NxVal %t10278, ptr @nx__g___main____acc38
  br label %wcond117
wcond117:
  %t10279 = load %NxVal, ptr @nx__g___main____i38
  %t10280 = add i64 3, 0
  %t10281 = extractvalue %NxVal %t10279, 1
  %t10282 = icmp slt i64 %t10281, %t10280
  br i1 %t10282, label %wbody118, label %wend119
wbody118:
  %t10283 = load %NxVal, ptr @nx__g___main____acc38
  %t10284 = load %NxVal, ptr @nx__g___main____c
  %t10285 = load %NxVal, ptr @nx__g___main____i38
  %t10286 = add i64 38, 0
  %t10287 = extractvalue %NxVal %t10285, 1
  %t10288 = add i64 %t10287, %t10286
  %t10290 = getelementptr [2 x %NxVal], ptr %t10289, i64 0, i64 0
  store %NxVal %t10284, ptr %t10290
  %t10291 = call %NxVal @nx_int(i64 %t10288)
  %t10292 = getelementptr [2 x %NxVal], ptr %t10289, i64 0, i64 1
  store %NxVal %t10291, ptr %t10292
  %t10293 = getelementptr [2 x %NxVal], ptr %t10289, i64 0, i64 0
  %t10294 = call %NxVal @nx__m_3____main____Cell__m38(ptr %t10293, i64 2)
  %t10295 = extractvalue %NxVal %t10294, 1
  %t10296 = extractvalue %NxVal %t10283, 1
  %t10297 = add i64 %t10296, %t10295
  %t10298 = add i64 65535, 0
  %t10299 = and i64 %t10297, %t10298
  %t10300 = call %NxVal @nx_int(i64 %t10299)
  store %NxVal %t10300, ptr @nx__g___main____acc38
  %t10301 = load %NxVal, ptr @nx__g___main____acc38
  %t10302 = add i64 54, 0
  %t10303 = extractvalue %NxVal %t10301, 1
  %t10304 = or i64 %t10303, %t10302
  %t10305 = add i64 41, 0
  %t10306 = load %NxVal, ptr @nx__g___main____i38
  %t10307 = extractvalue %NxVal %t10306, 1
  %t10308 = add i64 %t10305, %t10307
  %t10309 = or i64 %t10304, %t10308
  %t10310 = add i64 65535, 0
  %t10311 = and i64 %t10309, %t10310
  %t10312 = call %NxVal @nx_int(i64 %t10311)
  store %NxVal %t10312, ptr @nx__g___main____acc38
  %t10313 = load %NxVal, ptr @nx__g___main____acc38
  %t10314 = add i64 57, 0
  %t10315 = extractvalue %NxVal %t10313, 1
  %t10316 = or i64 %t10315, %t10314
  %t10317 = add i64 67, 0
  %t10318 = load %NxVal, ptr @nx__g___main____i38
  %t10319 = extractvalue %NxVal %t10318, 1
  %t10320 = add i64 %t10317, %t10319
  %t10321 = or i64 %t10316, %t10320
  %t10322 = add i64 65535, 0
  %t10323 = and i64 %t10321, %t10322
  %t10324 = call %NxVal @nx_int(i64 %t10323)
  store %NxVal %t10324, ptr @nx__g___main____acc38
  %t10325 = load %NxVal, ptr @nx__g___main____acc38
  %t10326 = add i64 9, 0
  %t10327 = extractvalue %NxVal %t10325, 1
  %t10328 = sub i64 %t10327, %t10326
  %t10329 = add i64 78, 0
  %t10330 = sub i64 %t10328, %t10329
  %t10331 = load %NxVal, ptr @nx__g___main____i38
  %t10332 = extractvalue %NxVal %t10331, 1
  %t10333 = add i64 %t10330, %t10332
  %t10334 = add i64 65535, 0
  %t10335 = and i64 %t10333, %t10334
  %t10336 = call %NxVal @nx_int(i64 %t10335)
  store %NxVal %t10336, ptr @nx__g___main____acc38
  %t10337 = load %NxVal, ptr @nx__g___main____acc38
  %t10338 = add i64 81, 0
  %t10339 = extractvalue %NxVal %t10337, 1
  %t10340 = mul i64 %t10339, %t10338
  %t10341 = add i64 30, 0
  %t10342 = mul i64 %t10340, %t10341
  %t10343 = load %NxVal, ptr @nx__g___main____i38
  %t10344 = extractvalue %NxVal %t10343, 1
  %t10345 = add i64 %t10342, %t10344
  %t10346 = add i64 65535, 0
  %t10347 = and i64 %t10345, %t10346
  %t10348 = call %NxVal @nx_int(i64 %t10347)
  store %NxVal %t10348, ptr @nx__g___main____acc38
  %t10349 = load %NxVal, ptr @nx__g___main____i38
  %t10350 = add i64 1, 0
  %t10351 = extractvalue %NxVal %t10349, 1
  %t10352 = add i64 %t10351, %t10350
  %t10353 = call %NxVal @nx_int(i64 %t10352)
  store %NxVal %t10353, ptr @nx__g___main____i38
  br label %wcond117
wend119:
  %t10354 = load %NxVal, ptr @nx__g___main____total
  %t10355 = load %NxVal, ptr @nx__g___main____acc38
  %t10356 = extractvalue %NxVal %t10354, 1
  %t10357 = extractvalue %NxVal %t10355, 1
  %t10358 = add i64 %t10356, %t10357
  %t10359 = add i64 65535, 0
  %t10360 = and i64 %t10358, %t10359
  %t10361 = call %NxVal @nx_int(i64 %t10360)
  store %NxVal %t10361, ptr @nx__g___main____total
  %t10362 = add i64 0, 0
  %t10363 = call %NxVal @nx_int(i64 %t10362)
  store %NxVal %t10363, ptr @nx__g___main____i39
  %t10364 = add i64 0, 0
  %t10365 = call %NxVal @nx_int(i64 %t10364)
  store %NxVal %t10365, ptr @nx__g___main____acc39
  br label %wcond120
wcond120:
  %t10366 = load %NxVal, ptr @nx__g___main____i39
  %t10367 = add i64 3, 0
  %t10368 = extractvalue %NxVal %t10366, 1
  %t10369 = icmp slt i64 %t10368, %t10367
  br i1 %t10369, label %wbody121, label %wend122
wbody121:
  %t10370 = load %NxVal, ptr @nx__g___main____acc39
  %t10371 = load %NxVal, ptr @nx__g___main____c
  %t10372 = load %NxVal, ptr @nx__g___main____i39
  %t10373 = add i64 39, 0
  %t10374 = extractvalue %NxVal %t10372, 1
  %t10375 = add i64 %t10374, %t10373
  %t10377 = getelementptr [2 x %NxVal], ptr %t10376, i64 0, i64 0
  store %NxVal %t10371, ptr %t10377
  %t10378 = call %NxVal @nx_int(i64 %t10375)
  %t10379 = getelementptr [2 x %NxVal], ptr %t10376, i64 0, i64 1
  store %NxVal %t10378, ptr %t10379
  %t10380 = getelementptr [2 x %NxVal], ptr %t10376, i64 0, i64 0
  %t10381 = call %NxVal @nx__m_3____main____Cell__m39(ptr %t10380, i64 2)
  %t10382 = extractvalue %NxVal %t10381, 1
  %t10383 = extractvalue %NxVal %t10370, 1
  %t10384 = add i64 %t10383, %t10382
  %t10385 = add i64 65535, 0
  %t10386 = and i64 %t10384, %t10385
  %t10387 = call %NxVal @nx_int(i64 %t10386)
  store %NxVal %t10387, ptr @nx__g___main____acc39
  %t10388 = load %NxVal, ptr @nx__g___main____acc39
  %t10389 = add i64 90, 0
  %t10390 = extractvalue %NxVal %t10388, 1
  %t10391 = and i64 %t10390, %t10389
  %t10392 = add i64 20, 0
  %t10393 = load %NxVal, ptr @nx__g___main____i39
  %t10394 = extractvalue %NxVal %t10393, 1
  %t10395 = add i64 %t10392, %t10394
  %t10396 = and i64 %t10391, %t10395
  %t10397 = add i64 65535, 0
  %t10398 = and i64 %t10396, %t10397
  %t10399 = call %NxVal @nx_int(i64 %t10398)
  store %NxVal %t10399, ptr @nx__g___main____acc39
  %t10400 = load %NxVal, ptr @nx__g___main____acc39
  %t10401 = add i64 51, 0
  %t10402 = extractvalue %NxVal %t10400, 1
  %t10403 = add i64 %t10402, %t10401
  %t10404 = add i64 21, 0
  %t10405 = add i64 %t10403, %t10404
  %t10406 = load %NxVal, ptr @nx__g___main____i39
  %t10407 = extractvalue %NxVal %t10406, 1
  %t10408 = add i64 %t10405, %t10407
  %t10409 = add i64 65535, 0
  %t10410 = and i64 %t10408, %t10409
  %t10411 = call %NxVal @nx_int(i64 %t10410)
  store %NxVal %t10411, ptr @nx__g___main____acc39
  %t10412 = load %NxVal, ptr @nx__g___main____acc39
  %t10413 = add i64 37, 0
  %t10414 = extractvalue %NxVal %t10412, 1
  %t10415 = and i64 %t10414, %t10413
  %t10416 = add i64 64, 0
  %t10417 = load %NxVal, ptr @nx__g___main____i39
  %t10418 = extractvalue %NxVal %t10417, 1
  %t10419 = add i64 %t10416, %t10418
  %t10420 = and i64 %t10415, %t10419
  %t10421 = add i64 65535, 0
  %t10422 = and i64 %t10420, %t10421
  %t10423 = call %NxVal @nx_int(i64 %t10422)
  store %NxVal %t10423, ptr @nx__g___main____acc39
  %t10424 = load %NxVal, ptr @nx__g___main____acc39
  %t10425 = add i64 23, 0
  %t10426 = extractvalue %NxVal %t10424, 1
  %t10427 = sub i64 %t10426, %t10425
  %t10428 = add i64 77, 0
  %t10429 = sub i64 %t10427, %t10428
  %t10430 = load %NxVal, ptr @nx__g___main____i39
  %t10431 = extractvalue %NxVal %t10430, 1
  %t10432 = add i64 %t10429, %t10431
  %t10433 = add i64 65535, 0
  %t10434 = and i64 %t10432, %t10433
  %t10435 = call %NxVal @nx_int(i64 %t10434)
  store %NxVal %t10435, ptr @nx__g___main____acc39
  %t10436 = load %NxVal, ptr @nx__g___main____i39
  %t10437 = add i64 1, 0
  %t10438 = extractvalue %NxVal %t10436, 1
  %t10439 = add i64 %t10438, %t10437
  %t10440 = call %NxVal @nx_int(i64 %t10439)
  store %NxVal %t10440, ptr @nx__g___main____i39
  br label %wcond120
wend122:
  %t10441 = load %NxVal, ptr @nx__g___main____total
  %t10442 = load %NxVal, ptr @nx__g___main____acc39
  %t10443 = extractvalue %NxVal %t10441, 1
  %t10444 = extractvalue %NxVal %t10442, 1
  %t10445 = add i64 %t10443, %t10444
  %t10446 = add i64 65535, 0
  %t10447 = and i64 %t10445, %t10446
  %t10448 = call %NxVal @nx_int(i64 %t10447)
  store %NxVal %t10448, ptr @nx__g___main____total
  %t10449 = add i64 0, 0
  %t10450 = call %NxVal @nx_int(i64 %t10449)
  store %NxVal %t10450, ptr @nx__g___main____i40
  %t10451 = add i64 0, 0
  %t10452 = call %NxVal @nx_int(i64 %t10451)
  store %NxVal %t10452, ptr @nx__g___main____acc40
  br label %wcond123
wcond123:
  %t10453 = load %NxVal, ptr @nx__g___main____i40
  %t10454 = add i64 3, 0
  %t10455 = extractvalue %NxVal %t10453, 1
  %t10456 = icmp slt i64 %t10455, %t10454
  br i1 %t10456, label %wbody124, label %wend125
wbody124:
  %t10457 = load %NxVal, ptr @nx__g___main____acc40
  %t10458 = load %NxVal, ptr @nx__g___main____c
  %t10459 = load %NxVal, ptr @nx__g___main____i40
  %t10460 = add i64 40, 0
  %t10461 = extractvalue %NxVal %t10459, 1
  %t10462 = add i64 %t10461, %t10460
  %t10464 = getelementptr [2 x %NxVal], ptr %t10463, i64 0, i64 0
  store %NxVal %t10458, ptr %t10464
  %t10465 = call %NxVal @nx_int(i64 %t10462)
  %t10466 = getelementptr [2 x %NxVal], ptr %t10463, i64 0, i64 1
  store %NxVal %t10465, ptr %t10466
  %t10467 = getelementptr [2 x %NxVal], ptr %t10463, i64 0, i64 0
  %t10468 = call %NxVal @nx__m_3____main____Cell__m40(ptr %t10467, i64 2)
  %t10469 = extractvalue %NxVal %t10468, 1
  %t10470 = extractvalue %NxVal %t10457, 1
  %t10471 = add i64 %t10470, %t10469
  %t10472 = add i64 65535, 0
  %t10473 = and i64 %t10471, %t10472
  %t10474 = call %NxVal @nx_int(i64 %t10473)
  store %NxVal %t10474, ptr @nx__g___main____acc40
  %t10475 = load %NxVal, ptr @nx__g___main____acc40
  %t10476 = add i64 8, 0
  %t10477 = extractvalue %NxVal %t10475, 1
  %t10478 = xor i64 %t10477, %t10476
  %t10479 = add i64 7, 0
  %t10480 = load %NxVal, ptr @nx__g___main____i40
  %t10481 = extractvalue %NxVal %t10480, 1
  %t10482 = add i64 %t10479, %t10481
  %t10483 = xor i64 %t10478, %t10482
  %t10484 = add i64 65535, 0
  %t10485 = and i64 %t10483, %t10484
  %t10486 = call %NxVal @nx_int(i64 %t10485)
  store %NxVal %t10486, ptr @nx__g___main____acc40
  %t10487 = load %NxVal, ptr @nx__g___main____acc40
  %t10488 = add i64 74, 0
  %t10489 = extractvalue %NxVal %t10487, 1
  %t10490 = xor i64 %t10489, %t10488
  %t10491 = add i64 87, 0
  %t10492 = load %NxVal, ptr @nx__g___main____i40
  %t10493 = extractvalue %NxVal %t10492, 1
  %t10494 = add i64 %t10491, %t10493
  %t10495 = xor i64 %t10490, %t10494
  %t10496 = add i64 65535, 0
  %t10497 = and i64 %t10495, %t10496
  %t10498 = call %NxVal @nx_int(i64 %t10497)
  store %NxVal %t10498, ptr @nx__g___main____acc40
  %t10499 = load %NxVal, ptr @nx__g___main____acc40
  %t10500 = add i64 11, 0
  %t10501 = extractvalue %NxVal %t10499, 1
  %t10502 = xor i64 %t10501, %t10500
  %t10503 = add i64 55, 0
  %t10504 = load %NxVal, ptr @nx__g___main____i40
  %t10505 = extractvalue %NxVal %t10504, 1
  %t10506 = add i64 %t10503, %t10505
  %t10507 = xor i64 %t10502, %t10506
  %t10508 = add i64 65535, 0
  %t10509 = and i64 %t10507, %t10508
  %t10510 = call %NxVal @nx_int(i64 %t10509)
  store %NxVal %t10510, ptr @nx__g___main____acc40
  %t10511 = load %NxVal, ptr @nx__g___main____acc40
  %t10512 = add i64 72, 0
  %t10513 = extractvalue %NxVal %t10511, 1
  %t10514 = xor i64 %t10513, %t10512
  %t10515 = add i64 37, 0
  %t10516 = load %NxVal, ptr @nx__g___main____i40
  %t10517 = extractvalue %NxVal %t10516, 1
  %t10518 = add i64 %t10515, %t10517
  %t10519 = xor i64 %t10514, %t10518
  %t10520 = add i64 65535, 0
  %t10521 = and i64 %t10519, %t10520
  %t10522 = call %NxVal @nx_int(i64 %t10521)
  store %NxVal %t10522, ptr @nx__g___main____acc40
  %t10523 = load %NxVal, ptr @nx__g___main____i40
  %t10524 = add i64 1, 0
  %t10525 = extractvalue %NxVal %t10523, 1
  %t10526 = add i64 %t10525, %t10524
  %t10527 = call %NxVal @nx_int(i64 %t10526)
  store %NxVal %t10527, ptr @nx__g___main____i40
  br label %wcond123
wend125:
  %t10528 = load %NxVal, ptr @nx__g___main____total
  %t10529 = load %NxVal, ptr @nx__g___main____acc40
  %t10530 = extractvalue %NxVal %t10528, 1
  %t10531 = extractvalue %NxVal %t10529, 1
  %t10532 = add i64 %t10530, %t10531
  %t10533 = add i64 65535, 0
  %t10534 = and i64 %t10532, %t10533
  %t10535 = call %NxVal @nx_int(i64 %t10534)
  store %NxVal %t10535, ptr @nx__g___main____total
  %t10536 = add i64 0, 0
  %t10537 = call %NxVal @nx_int(i64 %t10536)
  store %NxVal %t10537, ptr @nx__g___main____i41
  %t10538 = add i64 0, 0
  %t10539 = call %NxVal @nx_int(i64 %t10538)
  store %NxVal %t10539, ptr @nx__g___main____acc41
  br label %wcond126
wcond126:
  %t10540 = load %NxVal, ptr @nx__g___main____i41
  %t10541 = add i64 3, 0
  %t10542 = extractvalue %NxVal %t10540, 1
  %t10543 = icmp slt i64 %t10542, %t10541
  br i1 %t10543, label %wbody127, label %wend128
wbody127:
  %t10544 = load %NxVal, ptr @nx__g___main____acc41
  %t10545 = load %NxVal, ptr @nx__g___main____c
  %t10546 = load %NxVal, ptr @nx__g___main____i41
  %t10547 = add i64 41, 0
  %t10548 = extractvalue %NxVal %t10546, 1
  %t10549 = add i64 %t10548, %t10547
  %t10551 = getelementptr [2 x %NxVal], ptr %t10550, i64 0, i64 0
  store %NxVal %t10545, ptr %t10551
  %t10552 = call %NxVal @nx_int(i64 %t10549)
  %t10553 = getelementptr [2 x %NxVal], ptr %t10550, i64 0, i64 1
  store %NxVal %t10552, ptr %t10553
  %t10554 = getelementptr [2 x %NxVal], ptr %t10550, i64 0, i64 0
  %t10555 = call %NxVal @nx__m_3____main____Cell__m41(ptr %t10554, i64 2)
  %t10556 = extractvalue %NxVal %t10555, 1
  %t10557 = extractvalue %NxVal %t10544, 1
  %t10558 = add i64 %t10557, %t10556
  %t10559 = add i64 65535, 0
  %t10560 = and i64 %t10558, %t10559
  %t10561 = call %NxVal @nx_int(i64 %t10560)
  store %NxVal %t10561, ptr @nx__g___main____acc41
  %t10562 = load %NxVal, ptr @nx__g___main____acc41
  %t10563 = add i64 9, 0
  %t10564 = extractvalue %NxVal %t10562, 1
  %t10565 = xor i64 %t10564, %t10563
  %t10566 = add i64 26, 0
  %t10567 = load %NxVal, ptr @nx__g___main____i41
  %t10568 = extractvalue %NxVal %t10567, 1
  %t10569 = add i64 %t10566, %t10568
  %t10570 = xor i64 %t10565, %t10569
  %t10571 = add i64 65535, 0
  %t10572 = and i64 %t10570, %t10571
  %t10573 = call %NxVal @nx_int(i64 %t10572)
  store %NxVal %t10573, ptr @nx__g___main____acc41
  %t10574 = load %NxVal, ptr @nx__g___main____acc41
  %t10575 = add i64 55, 0
  %t10576 = extractvalue %NxVal %t10574, 1
  %t10577 = add i64 %t10576, %t10575
  %t10578 = add i64 72, 0
  %t10579 = add i64 %t10577, %t10578
  %t10580 = load %NxVal, ptr @nx__g___main____i41
  %t10581 = extractvalue %NxVal %t10580, 1
  %t10582 = add i64 %t10579, %t10581
  %t10583 = add i64 65535, 0
  %t10584 = and i64 %t10582, %t10583
  %t10585 = call %NxVal @nx_int(i64 %t10584)
  store %NxVal %t10585, ptr @nx__g___main____acc41
  %t10586 = load %NxVal, ptr @nx__g___main____acc41
  %t10587 = add i64 29, 0
  %t10588 = extractvalue %NxVal %t10586, 1
  %t10589 = or i64 %t10588, %t10587
  %t10590 = add i64 60, 0
  %t10591 = load %NxVal, ptr @nx__g___main____i41
  %t10592 = extractvalue %NxVal %t10591, 1
  %t10593 = add i64 %t10590, %t10592
  %t10594 = or i64 %t10589, %t10593
  %t10595 = add i64 65535, 0
  %t10596 = and i64 %t10594, %t10595
  %t10597 = call %NxVal @nx_int(i64 %t10596)
  store %NxVal %t10597, ptr @nx__g___main____acc41
  %t10598 = load %NxVal, ptr @nx__g___main____acc41
  %t10599 = add i64 76, 0
  %t10600 = extractvalue %NxVal %t10598, 1
  %t10601 = or i64 %t10600, %t10599
  %t10602 = add i64 88, 0
  %t10603 = load %NxVal, ptr @nx__g___main____i41
  %t10604 = extractvalue %NxVal %t10603, 1
  %t10605 = add i64 %t10602, %t10604
  %t10606 = or i64 %t10601, %t10605
  %t10607 = add i64 65535, 0
  %t10608 = and i64 %t10606, %t10607
  %t10609 = call %NxVal @nx_int(i64 %t10608)
  store %NxVal %t10609, ptr @nx__g___main____acc41
  %t10610 = load %NxVal, ptr @nx__g___main____i41
  %t10611 = add i64 1, 0
  %t10612 = extractvalue %NxVal %t10610, 1
  %t10613 = add i64 %t10612, %t10611
  %t10614 = call %NxVal @nx_int(i64 %t10613)
  store %NxVal %t10614, ptr @nx__g___main____i41
  br label %wcond126
wend128:
  %t10615 = load %NxVal, ptr @nx__g___main____total
  %t10616 = load %NxVal, ptr @nx__g___main____acc41
  %t10617 = extractvalue %NxVal %t10615, 1
  %t10618 = extractvalue %NxVal %t10616, 1
  %t10619 = add i64 %t10617, %t10618
  %t10620 = add i64 65535, 0
  %t10621 = and i64 %t10619, %t10620
  %t10622 = call %NxVal @nx_int(i64 %t10621)
  store %NxVal %t10622, ptr @nx__g___main____total
  %t10623 = add i64 0, 0
  %t10624 = call %NxVal @nx_int(i64 %t10623)
  store %NxVal %t10624, ptr @nx__g___main____i42
  %t10625 = add i64 0, 0
  %t10626 = call %NxVal @nx_int(i64 %t10625)
  store %NxVal %t10626, ptr @nx__g___main____acc42
  br label %wcond129
wcond129:
  %t10627 = load %NxVal, ptr @nx__g___main____i42
  %t10628 = add i64 3, 0
  %t10629 = extractvalue %NxVal %t10627, 1
  %t10630 = icmp slt i64 %t10629, %t10628
  br i1 %t10630, label %wbody130, label %wend131
wbody130:
  %t10631 = load %NxVal, ptr @nx__g___main____acc42
  %t10632 = load %NxVal, ptr @nx__g___main____c
  %t10633 = load %NxVal, ptr @nx__g___main____i42
  %t10634 = add i64 42, 0
  %t10635 = extractvalue %NxVal %t10633, 1
  %t10636 = add i64 %t10635, %t10634
  %t10638 = getelementptr [2 x %NxVal], ptr %t10637, i64 0, i64 0
  store %NxVal %t10632, ptr %t10638
  %t10639 = call %NxVal @nx_int(i64 %t10636)
  %t10640 = getelementptr [2 x %NxVal], ptr %t10637, i64 0, i64 1
  store %NxVal %t10639, ptr %t10640
  %t10641 = getelementptr [2 x %NxVal], ptr %t10637, i64 0, i64 0
  %t10642 = call %NxVal @nx__m_3____main____Cell__m42(ptr %t10641, i64 2)
  %t10643 = extractvalue %NxVal %t10642, 1
  %t10644 = extractvalue %NxVal %t10631, 1
  %t10645 = add i64 %t10644, %t10643
  %t10646 = add i64 65535, 0
  %t10647 = and i64 %t10645, %t10646
  %t10648 = call %NxVal @nx_int(i64 %t10647)
  store %NxVal %t10648, ptr @nx__g___main____acc42
  %t10649 = load %NxVal, ptr @nx__g___main____acc42
  %t10650 = add i64 1, 0
  %t10651 = extractvalue %NxVal %t10649, 1
  %t10652 = and i64 %t10651, %t10650
  %t10653 = add i64 44, 0
  %t10654 = load %NxVal, ptr @nx__g___main____i42
  %t10655 = extractvalue %NxVal %t10654, 1
  %t10656 = add i64 %t10653, %t10655
  %t10657 = and i64 %t10652, %t10656
  %t10658 = add i64 65535, 0
  %t10659 = and i64 %t10657, %t10658
  %t10660 = call %NxVal @nx_int(i64 %t10659)
  store %NxVal %t10660, ptr @nx__g___main____acc42
  %t10661 = load %NxVal, ptr @nx__g___main____acc42
  %t10662 = add i64 96, 0
  %t10663 = extractvalue %NxVal %t10661, 1
  %t10664 = and i64 %t10663, %t10662
  %t10665 = add i64 19, 0
  %t10666 = load %NxVal, ptr @nx__g___main____i42
  %t10667 = extractvalue %NxVal %t10666, 1
  %t10668 = add i64 %t10665, %t10667
  %t10669 = and i64 %t10664, %t10668
  %t10670 = add i64 65535, 0
  %t10671 = and i64 %t10669, %t10670
  %t10672 = call %NxVal @nx_int(i64 %t10671)
  store %NxVal %t10672, ptr @nx__g___main____acc42
  %t10673 = load %NxVal, ptr @nx__g___main____acc42
  %t10674 = add i64 66, 0
  %t10675 = extractvalue %NxVal %t10673, 1
  %t10676 = xor i64 %t10675, %t10674
  %t10677 = add i64 86, 0
  %t10678 = load %NxVal, ptr @nx__g___main____i42
  %t10679 = extractvalue %NxVal %t10678, 1
  %t10680 = add i64 %t10677, %t10679
  %t10681 = xor i64 %t10676, %t10680
  %t10682 = add i64 65535, 0
  %t10683 = and i64 %t10681, %t10682
  %t10684 = call %NxVal @nx_int(i64 %t10683)
  store %NxVal %t10684, ptr @nx__g___main____acc42
  %t10685 = load %NxVal, ptr @nx__g___main____acc42
  %t10686 = add i64 79, 0
  %t10687 = extractvalue %NxVal %t10685, 1
  %t10688 = sub i64 %t10687, %t10686
  %t10689 = add i64 2, 0
  %t10690 = sub i64 %t10688, %t10689
  %t10691 = load %NxVal, ptr @nx__g___main____i42
  %t10692 = extractvalue %NxVal %t10691, 1
  %t10693 = add i64 %t10690, %t10692
  %t10694 = add i64 65535, 0
  %t10695 = and i64 %t10693, %t10694
  %t10696 = call %NxVal @nx_int(i64 %t10695)
  store %NxVal %t10696, ptr @nx__g___main____acc42
  %t10697 = load %NxVal, ptr @nx__g___main____i42
  %t10698 = add i64 1, 0
  %t10699 = extractvalue %NxVal %t10697, 1
  %t10700 = add i64 %t10699, %t10698
  %t10701 = call %NxVal @nx_int(i64 %t10700)
  store %NxVal %t10701, ptr @nx__g___main____i42
  br label %wcond129
wend131:
  %t10702 = load %NxVal, ptr @nx__g___main____total
  %t10703 = load %NxVal, ptr @nx__g___main____acc42
  %t10704 = extractvalue %NxVal %t10702, 1
  %t10705 = extractvalue %NxVal %t10703, 1
  %t10706 = add i64 %t10704, %t10705
  %t10707 = add i64 65535, 0
  %t10708 = and i64 %t10706, %t10707
  %t10709 = call %NxVal @nx_int(i64 %t10708)
  store %NxVal %t10709, ptr @nx__g___main____total
  %t10710 = add i64 0, 0
  %t10711 = call %NxVal @nx_int(i64 %t10710)
  store %NxVal %t10711, ptr @nx__g___main____i43
  %t10712 = add i64 0, 0
  %t10713 = call %NxVal @nx_int(i64 %t10712)
  store %NxVal %t10713, ptr @nx__g___main____acc43
  br label %wcond132
wcond132:
  %t10714 = load %NxVal, ptr @nx__g___main____i43
  %t10715 = add i64 3, 0
  %t10716 = extractvalue %NxVal %t10714, 1
  %t10717 = icmp slt i64 %t10716, %t10715
  br i1 %t10717, label %wbody133, label %wend134
wbody133:
  %t10718 = load %NxVal, ptr @nx__g___main____acc43
  %t10719 = load %NxVal, ptr @nx__g___main____c
  %t10720 = load %NxVal, ptr @nx__g___main____i43
  %t10721 = add i64 43, 0
  %t10722 = extractvalue %NxVal %t10720, 1
  %t10723 = add i64 %t10722, %t10721
  %t10725 = getelementptr [2 x %NxVal], ptr %t10724, i64 0, i64 0
  store %NxVal %t10719, ptr %t10725
  %t10726 = call %NxVal @nx_int(i64 %t10723)
  %t10727 = getelementptr [2 x %NxVal], ptr %t10724, i64 0, i64 1
  store %NxVal %t10726, ptr %t10727
  %t10728 = getelementptr [2 x %NxVal], ptr %t10724, i64 0, i64 0
  %t10729 = call %NxVal @nx__m_3____main____Cell__m43(ptr %t10728, i64 2)
  %t10730 = extractvalue %NxVal %t10729, 1
  %t10731 = extractvalue %NxVal %t10718, 1
  %t10732 = add i64 %t10731, %t10730
  %t10733 = add i64 65535, 0
  %t10734 = and i64 %t10732, %t10733
  %t10735 = call %NxVal @nx_int(i64 %t10734)
  store %NxVal %t10735, ptr @nx__g___main____acc43
  %t10736 = load %NxVal, ptr @nx__g___main____acc43
  %t10737 = add i64 19, 0
  %t10738 = extractvalue %NxVal %t10736, 1
  %t10739 = and i64 %t10738, %t10737
  %t10740 = add i64 39, 0
  %t10741 = load %NxVal, ptr @nx__g___main____i43
  %t10742 = extractvalue %NxVal %t10741, 1
  %t10743 = add i64 %t10740, %t10742
  %t10744 = and i64 %t10739, %t10743
  %t10745 = add i64 65535, 0
  %t10746 = and i64 %t10744, %t10745
  %t10747 = call %NxVal @nx_int(i64 %t10746)
  store %NxVal %t10747, ptr @nx__g___main____acc43
  %t10748 = load %NxVal, ptr @nx__g___main____acc43
  %t10749 = add i64 41, 0
  %t10750 = extractvalue %NxVal %t10748, 1
  %t10751 = add i64 %t10750, %t10749
  %t10752 = add i64 61, 0
  %t10753 = add i64 %t10751, %t10752
  %t10754 = load %NxVal, ptr @nx__g___main____i43
  %t10755 = extractvalue %NxVal %t10754, 1
  %t10756 = add i64 %t10753, %t10755
  %t10757 = add i64 65535, 0
  %t10758 = and i64 %t10756, %t10757
  %t10759 = call %NxVal @nx_int(i64 %t10758)
  store %NxVal %t10759, ptr @nx__g___main____acc43
  %t10760 = load %NxVal, ptr @nx__g___main____acc43
  %t10761 = add i64 58, 0
  %t10762 = extractvalue %NxVal %t10760, 1
  %t10763 = call i64 @nx_mod_i64(i64 %t10762, i64 %t10761)
  %t10764 = add i64 56, 0
  %t10765 = call i64 @nx_mod_i64(i64 %t10763, i64 %t10764)
  %t10766 = load %NxVal, ptr @nx__g___main____i43
  %t10767 = extractvalue %NxVal %t10766, 1
  %t10768 = add i64 %t10765, %t10767
  %t10769 = add i64 65535, 0
  %t10770 = and i64 %t10768, %t10769
  %t10771 = call %NxVal @nx_int(i64 %t10770)
  store %NxVal %t10771, ptr @nx__g___main____acc43
  %t10772 = load %NxVal, ptr @nx__g___main____acc43
  %t10773 = add i64 15, 0
  %t10774 = extractvalue %NxVal %t10772, 1
  %t10775 = sub i64 %t10774, %t10773
  %t10776 = add i64 14, 0
  %t10777 = sub i64 %t10775, %t10776
  %t10778 = load %NxVal, ptr @nx__g___main____i43
  %t10779 = extractvalue %NxVal %t10778, 1
  %t10780 = add i64 %t10777, %t10779
  %t10781 = add i64 65535, 0
  %t10782 = and i64 %t10780, %t10781
  %t10783 = call %NxVal @nx_int(i64 %t10782)
  store %NxVal %t10783, ptr @nx__g___main____acc43
  %t10784 = load %NxVal, ptr @nx__g___main____i43
  %t10785 = add i64 1, 0
  %t10786 = extractvalue %NxVal %t10784, 1
  %t10787 = add i64 %t10786, %t10785
  %t10788 = call %NxVal @nx_int(i64 %t10787)
  store %NxVal %t10788, ptr @nx__g___main____i43
  br label %wcond132
wend134:
  %t10789 = load %NxVal, ptr @nx__g___main____total
  %t10790 = load %NxVal, ptr @nx__g___main____acc43
  %t10791 = extractvalue %NxVal %t10789, 1
  %t10792 = extractvalue %NxVal %t10790, 1
  %t10793 = add i64 %t10791, %t10792
  %t10794 = add i64 65535, 0
  %t10795 = and i64 %t10793, %t10794
  %t10796 = call %NxVal @nx_int(i64 %t10795)
  store %NxVal %t10796, ptr @nx__g___main____total
  %t10797 = add i64 0, 0
  %t10798 = call %NxVal @nx_int(i64 %t10797)
  store %NxVal %t10798, ptr @nx__g___main____i44
  %t10799 = add i64 0, 0
  %t10800 = call %NxVal @nx_int(i64 %t10799)
  store %NxVal %t10800, ptr @nx__g___main____acc44
  br label %wcond135
wcond135:
  %t10801 = load %NxVal, ptr @nx__g___main____i44
  %t10802 = add i64 3, 0
  %t10803 = extractvalue %NxVal %t10801, 1
  %t10804 = icmp slt i64 %t10803, %t10802
  br i1 %t10804, label %wbody136, label %wend137
wbody136:
  %t10805 = load %NxVal, ptr @nx__g___main____acc44
  %t10806 = load %NxVal, ptr @nx__g___main____c
  %t10807 = load %NxVal, ptr @nx__g___main____i44
  %t10808 = add i64 44, 0
  %t10809 = extractvalue %NxVal %t10807, 1
  %t10810 = add i64 %t10809, %t10808
  %t10812 = getelementptr [2 x %NxVal], ptr %t10811, i64 0, i64 0
  store %NxVal %t10806, ptr %t10812
  %t10813 = call %NxVal @nx_int(i64 %t10810)
  %t10814 = getelementptr [2 x %NxVal], ptr %t10811, i64 0, i64 1
  store %NxVal %t10813, ptr %t10814
  %t10815 = getelementptr [2 x %NxVal], ptr %t10811, i64 0, i64 0
  %t10816 = call %NxVal @nx__m_3____main____Cell__m44(ptr %t10815, i64 2)
  %t10817 = extractvalue %NxVal %t10816, 1
  %t10818 = extractvalue %NxVal %t10805, 1
  %t10819 = add i64 %t10818, %t10817
  %t10820 = add i64 65535, 0
  %t10821 = and i64 %t10819, %t10820
  %t10822 = call %NxVal @nx_int(i64 %t10821)
  store %NxVal %t10822, ptr @nx__g___main____acc44
  %t10823 = load %NxVal, ptr @nx__g___main____acc44
  %t10824 = add i64 36, 0
  %t10825 = extractvalue %NxVal %t10823, 1
  %t10826 = call i64 @nx_mod_i64(i64 %t10825, i64 %t10824)
  %t10827 = add i64 62, 0
  %t10828 = call i64 @nx_mod_i64(i64 %t10826, i64 %t10827)
  %t10829 = load %NxVal, ptr @nx__g___main____i44
  %t10830 = extractvalue %NxVal %t10829, 1
  %t10831 = add i64 %t10828, %t10830
  %t10832 = add i64 65535, 0
  %t10833 = and i64 %t10831, %t10832
  %t10834 = call %NxVal @nx_int(i64 %t10833)
  store %NxVal %t10834, ptr @nx__g___main____acc44
  %t10835 = load %NxVal, ptr @nx__g___main____acc44
  %t10836 = add i64 63, 0
  %t10837 = extractvalue %NxVal %t10835, 1
  %t10838 = and i64 %t10837, %t10836
  %t10839 = add i64 19, 0
  %t10840 = load %NxVal, ptr @nx__g___main____i44
  %t10841 = extractvalue %NxVal %t10840, 1
  %t10842 = add i64 %t10839, %t10841
  %t10843 = and i64 %t10838, %t10842
  %t10844 = add i64 65535, 0
  %t10845 = and i64 %t10843, %t10844
  %t10846 = call %NxVal @nx_int(i64 %t10845)
  store %NxVal %t10846, ptr @nx__g___main____acc44
  %t10847 = load %NxVal, ptr @nx__g___main____acc44
  %t10848 = add i64 46, 0
  %t10849 = extractvalue %NxVal %t10847, 1
  %t10850 = and i64 %t10849, %t10848
  %t10851 = add i64 27, 0
  %t10852 = load %NxVal, ptr @nx__g___main____i44
  %t10853 = extractvalue %NxVal %t10852, 1
  %t10854 = add i64 %t10851, %t10853
  %t10855 = and i64 %t10850, %t10854
  %t10856 = add i64 65535, 0
  %t10857 = and i64 %t10855, %t10856
  %t10858 = call %NxVal @nx_int(i64 %t10857)
  store %NxVal %t10858, ptr @nx__g___main____acc44
  %t10859 = load %NxVal, ptr @nx__g___main____acc44
  %t10860 = add i64 72, 0
  %t10861 = extractvalue %NxVal %t10859, 1
  %t10862 = xor i64 %t10861, %t10860
  %t10863 = add i64 72, 0
  %t10864 = load %NxVal, ptr @nx__g___main____i44
  %t10865 = extractvalue %NxVal %t10864, 1
  %t10866 = add i64 %t10863, %t10865
  %t10867 = xor i64 %t10862, %t10866
  %t10868 = add i64 65535, 0
  %t10869 = and i64 %t10867, %t10868
  %t10870 = call %NxVal @nx_int(i64 %t10869)
  store %NxVal %t10870, ptr @nx__g___main____acc44
  %t10871 = load %NxVal, ptr @nx__g___main____i44
  %t10872 = add i64 1, 0
  %t10873 = extractvalue %NxVal %t10871, 1
  %t10874 = add i64 %t10873, %t10872
  %t10875 = call %NxVal @nx_int(i64 %t10874)
  store %NxVal %t10875, ptr @nx__g___main____i44
  br label %wcond135
wend137:
  %t10876 = load %NxVal, ptr @nx__g___main____total
  %t10877 = load %NxVal, ptr @nx__g___main____acc44
  %t10878 = extractvalue %NxVal %t10876, 1
  %t10879 = extractvalue %NxVal %t10877, 1
  %t10880 = add i64 %t10878, %t10879
  %t10881 = add i64 65535, 0
  %t10882 = and i64 %t10880, %t10881
  %t10883 = call %NxVal @nx_int(i64 %t10882)
  store %NxVal %t10883, ptr @nx__g___main____total
  %t10884 = add i64 0, 0
  %t10885 = call %NxVal @nx_int(i64 %t10884)
  store %NxVal %t10885, ptr @nx__g___main____i45
  %t10886 = add i64 0, 0
  %t10887 = call %NxVal @nx_int(i64 %t10886)
  store %NxVal %t10887, ptr @nx__g___main____acc45
  br label %wcond138
wcond138:
  %t10888 = load %NxVal, ptr @nx__g___main____i45
  %t10889 = add i64 3, 0
  %t10890 = extractvalue %NxVal %t10888, 1
  %t10891 = icmp slt i64 %t10890, %t10889
  br i1 %t10891, label %wbody139, label %wend140
wbody139:
  %t10892 = load %NxVal, ptr @nx__g___main____acc45
  %t10893 = load %NxVal, ptr @nx__g___main____c
  %t10894 = load %NxVal, ptr @nx__g___main____i45
  %t10895 = add i64 45, 0
  %t10896 = extractvalue %NxVal %t10894, 1
  %t10897 = add i64 %t10896, %t10895
  %t10899 = getelementptr [2 x %NxVal], ptr %t10898, i64 0, i64 0
  store %NxVal %t10893, ptr %t10899
  %t10900 = call %NxVal @nx_int(i64 %t10897)
  %t10901 = getelementptr [2 x %NxVal], ptr %t10898, i64 0, i64 1
  store %NxVal %t10900, ptr %t10901
  %t10902 = getelementptr [2 x %NxVal], ptr %t10898, i64 0, i64 0
  %t10903 = call %NxVal @nx__m_3____main____Cell__m45(ptr %t10902, i64 2)
  %t10904 = extractvalue %NxVal %t10903, 1
  %t10905 = extractvalue %NxVal %t10892, 1
  %t10906 = add i64 %t10905, %t10904
  %t10907 = add i64 65535, 0
  %t10908 = and i64 %t10906, %t10907
  %t10909 = call %NxVal @nx_int(i64 %t10908)
  store %NxVal %t10909, ptr @nx__g___main____acc45
  %t10910 = load %NxVal, ptr @nx__g___main____acc45
  %t10911 = add i64 74, 0
  %t10912 = extractvalue %NxVal %t10910, 1
  %t10913 = or i64 %t10912, %t10911
  %t10914 = add i64 51, 0
  %t10915 = load %NxVal, ptr @nx__g___main____i45
  %t10916 = extractvalue %NxVal %t10915, 1
  %t10917 = add i64 %t10914, %t10916
  %t10918 = or i64 %t10913, %t10917
  %t10919 = add i64 65535, 0
  %t10920 = and i64 %t10918, %t10919
  %t10921 = call %NxVal @nx_int(i64 %t10920)
  store %NxVal %t10921, ptr @nx__g___main____acc45
  %t10922 = load %NxVal, ptr @nx__g___main____acc45
  %t10923 = add i64 96, 0
  %t10924 = extractvalue %NxVal %t10922, 1
  %t10925 = mul i64 %t10924, %t10923
  %t10926 = add i64 51, 0
  %t10927 = mul i64 %t10925, %t10926
  %t10928 = load %NxVal, ptr @nx__g___main____i45
  %t10929 = extractvalue %NxVal %t10928, 1
  %t10930 = add i64 %t10927, %t10929
  %t10931 = add i64 65535, 0
  %t10932 = and i64 %t10930, %t10931
  %t10933 = call %NxVal @nx_int(i64 %t10932)
  store %NxVal %t10933, ptr @nx__g___main____acc45
  %t10934 = load %NxVal, ptr @nx__g___main____acc45
  %t10935 = add i64 11, 0
  %t10936 = extractvalue %NxVal %t10934, 1
  %t10937 = call i64 @nx_mod_i64(i64 %t10936, i64 %t10935)
  %t10938 = add i64 74, 0
  %t10939 = call i64 @nx_mod_i64(i64 %t10937, i64 %t10938)
  %t10940 = load %NxVal, ptr @nx__g___main____i45
  %t10941 = extractvalue %NxVal %t10940, 1
  %t10942 = add i64 %t10939, %t10941
  %t10943 = add i64 65535, 0
  %t10944 = and i64 %t10942, %t10943
  %t10945 = call %NxVal @nx_int(i64 %t10944)
  store %NxVal %t10945, ptr @nx__g___main____acc45
  %t10946 = load %NxVal, ptr @nx__g___main____acc45
  %t10947 = add i64 30, 0
  %t10948 = extractvalue %NxVal %t10946, 1
  %t10949 = sub i64 %t10948, %t10947
  %t10950 = add i64 2, 0
  %t10951 = sub i64 %t10949, %t10950
  %t10952 = load %NxVal, ptr @nx__g___main____i45
  %t10953 = extractvalue %NxVal %t10952, 1
  %t10954 = add i64 %t10951, %t10953
  %t10955 = add i64 65535, 0
  %t10956 = and i64 %t10954, %t10955
  %t10957 = call %NxVal @nx_int(i64 %t10956)
  store %NxVal %t10957, ptr @nx__g___main____acc45
  %t10958 = load %NxVal, ptr @nx__g___main____i45
  %t10959 = add i64 1, 0
  %t10960 = extractvalue %NxVal %t10958, 1
  %t10961 = add i64 %t10960, %t10959
  %t10962 = call %NxVal @nx_int(i64 %t10961)
  store %NxVal %t10962, ptr @nx__g___main____i45
  br label %wcond138
wend140:
  %t10963 = load %NxVal, ptr @nx__g___main____total
  %t10964 = load %NxVal, ptr @nx__g___main____acc45
  %t10965 = extractvalue %NxVal %t10963, 1
  %t10966 = extractvalue %NxVal %t10964, 1
  %t10967 = add i64 %t10965, %t10966
  %t10968 = add i64 65535, 0
  %t10969 = and i64 %t10967, %t10968
  %t10970 = call %NxVal @nx_int(i64 %t10969)
  store %NxVal %t10970, ptr @nx__g___main____total
  %t10971 = add i64 0, 0
  %t10972 = call %NxVal @nx_int(i64 %t10971)
  store %NxVal %t10972, ptr @nx__g___main____i46
  %t10973 = add i64 0, 0
  %t10974 = call %NxVal @nx_int(i64 %t10973)
  store %NxVal %t10974, ptr @nx__g___main____acc46
  br label %wcond141
wcond141:
  %t10975 = load %NxVal, ptr @nx__g___main____i46
  %t10976 = add i64 3, 0
  %t10977 = extractvalue %NxVal %t10975, 1
  %t10978 = icmp slt i64 %t10977, %t10976
  br i1 %t10978, label %wbody142, label %wend143
wbody142:
  %t10979 = load %NxVal, ptr @nx__g___main____acc46
  %t10980 = load %NxVal, ptr @nx__g___main____c
  %t10981 = load %NxVal, ptr @nx__g___main____i46
  %t10982 = add i64 46, 0
  %t10983 = extractvalue %NxVal %t10981, 1
  %t10984 = add i64 %t10983, %t10982
  %t10986 = getelementptr [2 x %NxVal], ptr %t10985, i64 0, i64 0
  store %NxVal %t10980, ptr %t10986
  %t10987 = call %NxVal @nx_int(i64 %t10984)
  %t10988 = getelementptr [2 x %NxVal], ptr %t10985, i64 0, i64 1
  store %NxVal %t10987, ptr %t10988
  %t10989 = getelementptr [2 x %NxVal], ptr %t10985, i64 0, i64 0
  %t10990 = call %NxVal @nx__m_3____main____Cell__m46(ptr %t10989, i64 2)
  %t10991 = extractvalue %NxVal %t10990, 1
  %t10992 = extractvalue %NxVal %t10979, 1
  %t10993 = add i64 %t10992, %t10991
  %t10994 = add i64 65535, 0
  %t10995 = and i64 %t10993, %t10994
  %t10996 = call %NxVal @nx_int(i64 %t10995)
  store %NxVal %t10996, ptr @nx__g___main____acc46
  %t10997 = load %NxVal, ptr @nx__g___main____acc46
  %t10998 = add i64 54, 0
  %t10999 = extractvalue %NxVal %t10997, 1
  %t11000 = and i64 %t10999, %t10998
  %t11001 = add i64 49, 0
  %t11002 = load %NxVal, ptr @nx__g___main____i46
  %t11003 = extractvalue %NxVal %t11002, 1
  %t11004 = add i64 %t11001, %t11003
  %t11005 = and i64 %t11000, %t11004
  %t11006 = add i64 65535, 0
  %t11007 = and i64 %t11005, %t11006
  %t11008 = call %NxVal @nx_int(i64 %t11007)
  store %NxVal %t11008, ptr @nx__g___main____acc46
  %t11009 = load %NxVal, ptr @nx__g___main____acc46
  %t11010 = add i64 8, 0
  %t11011 = extractvalue %NxVal %t11009, 1
  %t11012 = mul i64 %t11011, %t11010
  %t11013 = add i64 61, 0
  %t11014 = mul i64 %t11012, %t11013
  %t11015 = load %NxVal, ptr @nx__g___main____i46
  %t11016 = extractvalue %NxVal %t11015, 1
  %t11017 = add i64 %t11014, %t11016
  %t11018 = add i64 65535, 0
  %t11019 = and i64 %t11017, %t11018
  %t11020 = call %NxVal @nx_int(i64 %t11019)
  store %NxVal %t11020, ptr @nx__g___main____acc46
  %t11021 = load %NxVal, ptr @nx__g___main____acc46
  %t11022 = add i64 48, 0
  %t11023 = extractvalue %NxVal %t11021, 1
  %t11024 = xor i64 %t11023, %t11022
  %t11025 = add i64 85, 0
  %t11026 = load %NxVal, ptr @nx__g___main____i46
  %t11027 = extractvalue %NxVal %t11026, 1
  %t11028 = add i64 %t11025, %t11027
  %t11029 = xor i64 %t11024, %t11028
  %t11030 = add i64 65535, 0
  %t11031 = and i64 %t11029, %t11030
  %t11032 = call %NxVal @nx_int(i64 %t11031)
  store %NxVal %t11032, ptr @nx__g___main____acc46
  %t11033 = load %NxVal, ptr @nx__g___main____acc46
  %t11034 = add i64 74, 0
  %t11035 = extractvalue %NxVal %t11033, 1
  %t11036 = call i64 @nx_mod_i64(i64 %t11035, i64 %t11034)
  %t11037 = add i64 15, 0
  %t11038 = call i64 @nx_mod_i64(i64 %t11036, i64 %t11037)
  %t11039 = load %NxVal, ptr @nx__g___main____i46
  %t11040 = extractvalue %NxVal %t11039, 1
  %t11041 = add i64 %t11038, %t11040
  %t11042 = add i64 65535, 0
  %t11043 = and i64 %t11041, %t11042
  %t11044 = call %NxVal @nx_int(i64 %t11043)
  store %NxVal %t11044, ptr @nx__g___main____acc46
  %t11045 = load %NxVal, ptr @nx__g___main____i46
  %t11046 = add i64 1, 0
  %t11047 = extractvalue %NxVal %t11045, 1
  %t11048 = add i64 %t11047, %t11046
  %t11049 = call %NxVal @nx_int(i64 %t11048)
  store %NxVal %t11049, ptr @nx__g___main____i46
  br label %wcond141
wend143:
  %t11050 = load %NxVal, ptr @nx__g___main____total
  %t11051 = load %NxVal, ptr @nx__g___main____acc46
  %t11052 = extractvalue %NxVal %t11050, 1
  %t11053 = extractvalue %NxVal %t11051, 1
  %t11054 = add i64 %t11052, %t11053
  %t11055 = add i64 65535, 0
  %t11056 = and i64 %t11054, %t11055
  %t11057 = call %NxVal @nx_int(i64 %t11056)
  store %NxVal %t11057, ptr @nx__g___main____total
  %t11058 = add i64 0, 0
  %t11059 = call %NxVal @nx_int(i64 %t11058)
  store %NxVal %t11059, ptr @nx__g___main____i47
  %t11060 = add i64 0, 0
  %t11061 = call %NxVal @nx_int(i64 %t11060)
  store %NxVal %t11061, ptr @nx__g___main____acc47
  br label %wcond144
wcond144:
  %t11062 = load %NxVal, ptr @nx__g___main____i47
  %t11063 = add i64 3, 0
  %t11064 = extractvalue %NxVal %t11062, 1
  %t11065 = icmp slt i64 %t11064, %t11063
  br i1 %t11065, label %wbody145, label %wend146
wbody145:
  %t11066 = load %NxVal, ptr @nx__g___main____acc47
  %t11067 = load %NxVal, ptr @nx__g___main____c
  %t11068 = load %NxVal, ptr @nx__g___main____i47
  %t11069 = add i64 47, 0
  %t11070 = extractvalue %NxVal %t11068, 1
  %t11071 = add i64 %t11070, %t11069
  %t11073 = getelementptr [2 x %NxVal], ptr %t11072, i64 0, i64 0
  store %NxVal %t11067, ptr %t11073
  %t11074 = call %NxVal @nx_int(i64 %t11071)
  %t11075 = getelementptr [2 x %NxVal], ptr %t11072, i64 0, i64 1
  store %NxVal %t11074, ptr %t11075
  %t11076 = getelementptr [2 x %NxVal], ptr %t11072, i64 0, i64 0
  %t11077 = call %NxVal @nx__m_3____main____Cell__m47(ptr %t11076, i64 2)
  %t11078 = extractvalue %NxVal %t11077, 1
  %t11079 = extractvalue %NxVal %t11066, 1
  %t11080 = add i64 %t11079, %t11078
  %t11081 = add i64 65535, 0
  %t11082 = and i64 %t11080, %t11081
  %t11083 = call %NxVal @nx_int(i64 %t11082)
  store %NxVal %t11083, ptr @nx__g___main____acc47
  %t11084 = load %NxVal, ptr @nx__g___main____acc47
  %t11085 = add i64 45, 0
  %t11086 = extractvalue %NxVal %t11084, 1
  %t11087 = sub i64 %t11086, %t11085
  %t11088 = add i64 28, 0
  %t11089 = sub i64 %t11087, %t11088
  %t11090 = load %NxVal, ptr @nx__g___main____i47
  %t11091 = extractvalue %NxVal %t11090, 1
  %t11092 = add i64 %t11089, %t11091
  %t11093 = add i64 65535, 0
  %t11094 = and i64 %t11092, %t11093
  %t11095 = call %NxVal @nx_int(i64 %t11094)
  store %NxVal %t11095, ptr @nx__g___main____acc47
  %t11096 = load %NxVal, ptr @nx__g___main____acc47
  %t11097 = add i64 2, 0
  %t11098 = extractvalue %NxVal %t11096, 1
  %t11099 = call i64 @nx_mod_i64(i64 %t11098, i64 %t11097)
  %t11100 = add i64 56, 0
  %t11101 = call i64 @nx_mod_i64(i64 %t11099, i64 %t11100)
  %t11102 = load %NxVal, ptr @nx__g___main____i47
  %t11103 = extractvalue %NxVal %t11102, 1
  %t11104 = add i64 %t11101, %t11103
  %t11105 = add i64 65535, 0
  %t11106 = and i64 %t11104, %t11105
  %t11107 = call %NxVal @nx_int(i64 %t11106)
  store %NxVal %t11107, ptr @nx__g___main____acc47
  %t11108 = load %NxVal, ptr @nx__g___main____acc47
  %t11109 = add i64 44, 0
  %t11110 = extractvalue %NxVal %t11108, 1
  %t11111 = and i64 %t11110, %t11109
  %t11112 = add i64 48, 0
  %t11113 = load %NxVal, ptr @nx__g___main____i47
  %t11114 = extractvalue %NxVal %t11113, 1
  %t11115 = add i64 %t11112, %t11114
  %t11116 = and i64 %t11111, %t11115
  %t11117 = add i64 65535, 0
  %t11118 = and i64 %t11116, %t11117
  %t11119 = call %NxVal @nx_int(i64 %t11118)
  store %NxVal %t11119, ptr @nx__g___main____acc47
  %t11120 = load %NxVal, ptr @nx__g___main____acc47
  %t11121 = add i64 38, 0
  %t11122 = extractvalue %NxVal %t11120, 1
  %t11123 = add i64 %t11122, %t11121
  %t11124 = add i64 58, 0
  %t11125 = add i64 %t11123, %t11124
  %t11126 = load %NxVal, ptr @nx__g___main____i47
  %t11127 = extractvalue %NxVal %t11126, 1
  %t11128 = add i64 %t11125, %t11127
  %t11129 = add i64 65535, 0
  %t11130 = and i64 %t11128, %t11129
  %t11131 = call %NxVal @nx_int(i64 %t11130)
  store %NxVal %t11131, ptr @nx__g___main____acc47
  %t11132 = load %NxVal, ptr @nx__g___main____i47
  %t11133 = add i64 1, 0
  %t11134 = extractvalue %NxVal %t11132, 1
  %t11135 = add i64 %t11134, %t11133
  %t11136 = call %NxVal @nx_int(i64 %t11135)
  store %NxVal %t11136, ptr @nx__g___main____i47
  br label %wcond144
wend146:
  %t11137 = load %NxVal, ptr @nx__g___main____total
  %t11138 = load %NxVal, ptr @nx__g___main____acc47
  %t11139 = extractvalue %NxVal %t11137, 1
  %t11140 = extractvalue %NxVal %t11138, 1
  %t11141 = add i64 %t11139, %t11140
  %t11142 = add i64 65535, 0
  %t11143 = and i64 %t11141, %t11142
  %t11144 = call %NxVal @nx_int(i64 %t11143)
  store %NxVal %t11144, ptr @nx__g___main____total
  %t11145 = add i64 0, 0
  %t11146 = call %NxVal @nx_int(i64 %t11145)
  store %NxVal %t11146, ptr @nx__g___main____i48
  %t11147 = add i64 0, 0
  %t11148 = call %NxVal @nx_int(i64 %t11147)
  store %NxVal %t11148, ptr @nx__g___main____acc48
  br label %wcond147
wcond147:
  %t11149 = load %NxVal, ptr @nx__g___main____i48
  %t11150 = add i64 3, 0
  %t11151 = extractvalue %NxVal %t11149, 1
  %t11152 = icmp slt i64 %t11151, %t11150
  br i1 %t11152, label %wbody148, label %wend149
wbody148:
  %t11153 = load %NxVal, ptr @nx__g___main____acc48
  %t11154 = load %NxVal, ptr @nx__g___main____c
  %t11155 = load %NxVal, ptr @nx__g___main____i48
  %t11156 = add i64 48, 0
  %t11157 = extractvalue %NxVal %t11155, 1
  %t11158 = add i64 %t11157, %t11156
  %t11160 = getelementptr [2 x %NxVal], ptr %t11159, i64 0, i64 0
  store %NxVal %t11154, ptr %t11160
  %t11161 = call %NxVal @nx_int(i64 %t11158)
  %t11162 = getelementptr [2 x %NxVal], ptr %t11159, i64 0, i64 1
  store %NxVal %t11161, ptr %t11162
  %t11163 = getelementptr [2 x %NxVal], ptr %t11159, i64 0, i64 0
  %t11164 = call %NxVal @nx__m_3____main____Cell__m48(ptr %t11163, i64 2)
  %t11165 = extractvalue %NxVal %t11164, 1
  %t11166 = extractvalue %NxVal %t11153, 1
  %t11167 = add i64 %t11166, %t11165
  %t11168 = add i64 65535, 0
  %t11169 = and i64 %t11167, %t11168
  %t11170 = call %NxVal @nx_int(i64 %t11169)
  store %NxVal %t11170, ptr @nx__g___main____acc48
  %t11171 = load %NxVal, ptr @nx__g___main____acc48
  %t11172 = add i64 62, 0
  %t11173 = extractvalue %NxVal %t11171, 1
  %t11174 = mul i64 %t11173, %t11172
  %t11175 = add i64 58, 0
  %t11176 = mul i64 %t11174, %t11175
  %t11177 = load %NxVal, ptr @nx__g___main____i48
  %t11178 = extractvalue %NxVal %t11177, 1
  %t11179 = add i64 %t11176, %t11178
  %t11180 = add i64 65535, 0
  %t11181 = and i64 %t11179, %t11180
  %t11182 = call %NxVal @nx_int(i64 %t11181)
  store %NxVal %t11182, ptr @nx__g___main____acc48
  %t11183 = load %NxVal, ptr @nx__g___main____acc48
  %t11184 = add i64 62, 0
  %t11185 = extractvalue %NxVal %t11183, 1
  %t11186 = and i64 %t11185, %t11184
  %t11187 = add i64 54, 0
  %t11188 = load %NxVal, ptr @nx__g___main____i48
  %t11189 = extractvalue %NxVal %t11188, 1
  %t11190 = add i64 %t11187, %t11189
  %t11191 = and i64 %t11186, %t11190
  %t11192 = add i64 65535, 0
  %t11193 = and i64 %t11191, %t11192
  %t11194 = call %NxVal @nx_int(i64 %t11193)
  store %NxVal %t11194, ptr @nx__g___main____acc48
  %t11195 = load %NxVal, ptr @nx__g___main____acc48
  %t11196 = add i64 35, 0
  %t11197 = extractvalue %NxVal %t11195, 1
  %t11198 = add i64 %t11197, %t11196
  %t11199 = add i64 6, 0
  %t11200 = add i64 %t11198, %t11199
  %t11201 = load %NxVal, ptr @nx__g___main____i48
  %t11202 = extractvalue %NxVal %t11201, 1
  %t11203 = add i64 %t11200, %t11202
  %t11204 = add i64 65535, 0
  %t11205 = and i64 %t11203, %t11204
  %t11206 = call %NxVal @nx_int(i64 %t11205)
  store %NxVal %t11206, ptr @nx__g___main____acc48
  %t11207 = load %NxVal, ptr @nx__g___main____acc48
  %t11208 = add i64 72, 0
  %t11209 = extractvalue %NxVal %t11207, 1
  %t11210 = xor i64 %t11209, %t11208
  %t11211 = add i64 43, 0
  %t11212 = load %NxVal, ptr @nx__g___main____i48
  %t11213 = extractvalue %NxVal %t11212, 1
  %t11214 = add i64 %t11211, %t11213
  %t11215 = xor i64 %t11210, %t11214
  %t11216 = add i64 65535, 0
  %t11217 = and i64 %t11215, %t11216
  %t11218 = call %NxVal @nx_int(i64 %t11217)
  store %NxVal %t11218, ptr @nx__g___main____acc48
  %t11219 = load %NxVal, ptr @nx__g___main____i48
  %t11220 = add i64 1, 0
  %t11221 = extractvalue %NxVal %t11219, 1
  %t11222 = add i64 %t11221, %t11220
  %t11223 = call %NxVal @nx_int(i64 %t11222)
  store %NxVal %t11223, ptr @nx__g___main____i48
  br label %wcond147
wend149:
  %t11224 = load %NxVal, ptr @nx__g___main____total
  %t11225 = load %NxVal, ptr @nx__g___main____acc48
  %t11226 = extractvalue %NxVal %t11224, 1
  %t11227 = extractvalue %NxVal %t11225, 1
  %t11228 = add i64 %t11226, %t11227
  %t11229 = add i64 65535, 0
  %t11230 = and i64 %t11228, %t11229
  %t11231 = call %NxVal @nx_int(i64 %t11230)
  store %NxVal %t11231, ptr @nx__g___main____total
  %t11232 = add i64 0, 0
  %t11233 = call %NxVal @nx_int(i64 %t11232)
  store %NxVal %t11233, ptr @nx__g___main____i49
  %t11234 = add i64 0, 0
  %t11235 = call %NxVal @nx_int(i64 %t11234)
  store %NxVal %t11235, ptr @nx__g___main____acc49
  br label %wcond150
wcond150:
  %t11236 = load %NxVal, ptr @nx__g___main____i49
  %t11237 = add i64 3, 0
  %t11238 = extractvalue %NxVal %t11236, 1
  %t11239 = icmp slt i64 %t11238, %t11237
  br i1 %t11239, label %wbody151, label %wend152
wbody151:
  %t11240 = load %NxVal, ptr @nx__g___main____acc49
  %t11241 = load %NxVal, ptr @nx__g___main____c
  %t11242 = load %NxVal, ptr @nx__g___main____i49
  %t11243 = add i64 49, 0
  %t11244 = extractvalue %NxVal %t11242, 1
  %t11245 = add i64 %t11244, %t11243
  %t11247 = getelementptr [2 x %NxVal], ptr %t11246, i64 0, i64 0
  store %NxVal %t11241, ptr %t11247
  %t11248 = call %NxVal @nx_int(i64 %t11245)
  %t11249 = getelementptr [2 x %NxVal], ptr %t11246, i64 0, i64 1
  store %NxVal %t11248, ptr %t11249
  %t11250 = getelementptr [2 x %NxVal], ptr %t11246, i64 0, i64 0
  %t11251 = call %NxVal @nx__m_3____main____Cell__m49(ptr %t11250, i64 2)
  %t11252 = extractvalue %NxVal %t11251, 1
  %t11253 = extractvalue %NxVal %t11240, 1
  %t11254 = add i64 %t11253, %t11252
  %t11255 = add i64 65535, 0
  %t11256 = and i64 %t11254, %t11255
  %t11257 = call %NxVal @nx_int(i64 %t11256)
  store %NxVal %t11257, ptr @nx__g___main____acc49
  %t11258 = load %NxVal, ptr @nx__g___main____acc49
  %t11259 = add i64 76, 0
  %t11260 = extractvalue %NxVal %t11258, 1
  %t11261 = add i64 %t11260, %t11259
  %t11262 = add i64 22, 0
  %t11263 = add i64 %t11261, %t11262
  %t11264 = load %NxVal, ptr @nx__g___main____i49
  %t11265 = extractvalue %NxVal %t11264, 1
  %t11266 = add i64 %t11263, %t11265
  %t11267 = add i64 65535, 0
  %t11268 = and i64 %t11266, %t11267
  %t11269 = call %NxVal @nx_int(i64 %t11268)
  store %NxVal %t11269, ptr @nx__g___main____acc49
  %t11270 = load %NxVal, ptr @nx__g___main____acc49
  %t11271 = add i64 39, 0
  %t11272 = extractvalue %NxVal %t11270, 1
  %t11273 = xor i64 %t11272, %t11271
  %t11274 = add i64 30, 0
  %t11275 = load %NxVal, ptr @nx__g___main____i49
  %t11276 = extractvalue %NxVal %t11275, 1
  %t11277 = add i64 %t11274, %t11276
  %t11278 = xor i64 %t11273, %t11277
  %t11279 = add i64 65535, 0
  %t11280 = and i64 %t11278, %t11279
  %t11281 = call %NxVal @nx_int(i64 %t11280)
  store %NxVal %t11281, ptr @nx__g___main____acc49
  %t11282 = load %NxVal, ptr @nx__g___main____acc49
  %t11283 = add i64 33, 0
  %t11284 = extractvalue %NxVal %t11282, 1
  %t11285 = and i64 %t11284, %t11283
  %t11286 = add i64 12, 0
  %t11287 = load %NxVal, ptr @nx__g___main____i49
  %t11288 = extractvalue %NxVal %t11287, 1
  %t11289 = add i64 %t11286, %t11288
  %t11290 = and i64 %t11285, %t11289
  %t11291 = add i64 65535, 0
  %t11292 = and i64 %t11290, %t11291
  %t11293 = call %NxVal @nx_int(i64 %t11292)
  store %NxVal %t11293, ptr @nx__g___main____acc49
  %t11294 = load %NxVal, ptr @nx__g___main____acc49
  %t11295 = add i64 23, 0
  %t11296 = extractvalue %NxVal %t11294, 1
  %t11297 = sub i64 %t11296, %t11295
  %t11298 = add i64 51, 0
  %t11299 = sub i64 %t11297, %t11298
  %t11300 = load %NxVal, ptr @nx__g___main____i49
  %t11301 = extractvalue %NxVal %t11300, 1
  %t11302 = add i64 %t11299, %t11301
  %t11303 = add i64 65535, 0
  %t11304 = and i64 %t11302, %t11303
  %t11305 = call %NxVal @nx_int(i64 %t11304)
  store %NxVal %t11305, ptr @nx__g___main____acc49
  %t11306 = load %NxVal, ptr @nx__g___main____i49
  %t11307 = add i64 1, 0
  %t11308 = extractvalue %NxVal %t11306, 1
  %t11309 = add i64 %t11308, %t11307
  %t11310 = call %NxVal @nx_int(i64 %t11309)
  store %NxVal %t11310, ptr @nx__g___main____i49
  br label %wcond150
wend152:
  %t11311 = load %NxVal, ptr @nx__g___main____total
  %t11312 = load %NxVal, ptr @nx__g___main____acc49
  %t11313 = extractvalue %NxVal %t11311, 1
  %t11314 = extractvalue %NxVal %t11312, 1
  %t11315 = add i64 %t11313, %t11314
  %t11316 = add i64 65535, 0
  %t11317 = and i64 %t11315, %t11316
  %t11318 = call %NxVal @nx_int(i64 %t11317)
  store %NxVal %t11318, ptr @nx__g___main____total
  %t11319 = add i64 0, 0
  %t11320 = call %NxVal @nx_int(i64 %t11319)
  store %NxVal %t11320, ptr @nx__g___main____i50
  %t11321 = add i64 0, 0
  %t11322 = call %NxVal @nx_int(i64 %t11321)
  store %NxVal %t11322, ptr @nx__g___main____acc50
  br label %wcond153
wcond153:
  %t11323 = load %NxVal, ptr @nx__g___main____i50
  %t11324 = add i64 3, 0
  %t11325 = extractvalue %NxVal %t11323, 1
  %t11326 = icmp slt i64 %t11325, %t11324
  br i1 %t11326, label %wbody154, label %wend155
wbody154:
  %t11327 = load %NxVal, ptr @nx__g___main____acc50
  %t11328 = load %NxVal, ptr @nx__g___main____c
  %t11329 = load %NxVal, ptr @nx__g___main____i50
  %t11330 = add i64 50, 0
  %t11331 = extractvalue %NxVal %t11329, 1
  %t11332 = add i64 %t11331, %t11330
  %t11334 = getelementptr [2 x %NxVal], ptr %t11333, i64 0, i64 0
  store %NxVal %t11328, ptr %t11334
  %t11335 = call %NxVal @nx_int(i64 %t11332)
  %t11336 = getelementptr [2 x %NxVal], ptr %t11333, i64 0, i64 1
  store %NxVal %t11335, ptr %t11336
  %t11337 = getelementptr [2 x %NxVal], ptr %t11333, i64 0, i64 0
  %t11338 = call %NxVal @nx__m_3____main____Cell__m50(ptr %t11337, i64 2)
  %t11339 = extractvalue %NxVal %t11338, 1
  %t11340 = extractvalue %NxVal %t11327, 1
  %t11341 = add i64 %t11340, %t11339
  %t11342 = add i64 65535, 0
  %t11343 = and i64 %t11341, %t11342
  %t11344 = call %NxVal @nx_int(i64 %t11343)
  store %NxVal %t11344, ptr @nx__g___main____acc50
  %t11345 = load %NxVal, ptr @nx__g___main____acc50
  %t11346 = add i64 1, 0
  %t11347 = extractvalue %NxVal %t11345, 1
  %t11348 = add i64 %t11347, %t11346
  %t11349 = add i64 68, 0
  %t11350 = add i64 %t11348, %t11349
  %t11351 = load %NxVal, ptr @nx__g___main____i50
  %t11352 = extractvalue %NxVal %t11351, 1
  %t11353 = add i64 %t11350, %t11352
  %t11354 = add i64 65535, 0
  %t11355 = and i64 %t11353, %t11354
  %t11356 = call %NxVal @nx_int(i64 %t11355)
  store %NxVal %t11356, ptr @nx__g___main____acc50
  %t11357 = load %NxVal, ptr @nx__g___main____acc50
  %t11358 = add i64 12, 0
  %t11359 = extractvalue %NxVal %t11357, 1
  %t11360 = sub i64 %t11359, %t11358
  %t11361 = add i64 50, 0
  %t11362 = sub i64 %t11360, %t11361
  %t11363 = load %NxVal, ptr @nx__g___main____i50
  %t11364 = extractvalue %NxVal %t11363, 1
  %t11365 = add i64 %t11362, %t11364
  %t11366 = add i64 65535, 0
  %t11367 = and i64 %t11365, %t11366
  %t11368 = call %NxVal @nx_int(i64 %t11367)
  store %NxVal %t11368, ptr @nx__g___main____acc50
  %t11369 = load %NxVal, ptr @nx__g___main____acc50
  %t11370 = add i64 74, 0
  %t11371 = extractvalue %NxVal %t11369, 1
  %t11372 = xor i64 %t11371, %t11370
  %t11373 = add i64 58, 0
  %t11374 = load %NxVal, ptr @nx__g___main____i50
  %t11375 = extractvalue %NxVal %t11374, 1
  %t11376 = add i64 %t11373, %t11375
  %t11377 = xor i64 %t11372, %t11376
  %t11378 = add i64 65535, 0
  %t11379 = and i64 %t11377, %t11378
  %t11380 = call %NxVal @nx_int(i64 %t11379)
  store %NxVal %t11380, ptr @nx__g___main____acc50
  %t11381 = load %NxVal, ptr @nx__g___main____acc50
  %t11382 = add i64 24, 0
  %t11383 = extractvalue %NxVal %t11381, 1
  %t11384 = and i64 %t11383, %t11382
  %t11385 = add i64 71, 0
  %t11386 = load %NxVal, ptr @nx__g___main____i50
  %t11387 = extractvalue %NxVal %t11386, 1
  %t11388 = add i64 %t11385, %t11387
  %t11389 = and i64 %t11384, %t11388
  %t11390 = add i64 65535, 0
  %t11391 = and i64 %t11389, %t11390
  %t11392 = call %NxVal @nx_int(i64 %t11391)
  store %NxVal %t11392, ptr @nx__g___main____acc50
  %t11393 = load %NxVal, ptr @nx__g___main____i50
  %t11394 = add i64 1, 0
  %t11395 = extractvalue %NxVal %t11393, 1
  %t11396 = add i64 %t11395, %t11394
  %t11397 = call %NxVal @nx_int(i64 %t11396)
  store %NxVal %t11397, ptr @nx__g___main____i50
  br label %wcond153
wend155:
  %t11398 = load %NxVal, ptr @nx__g___main____total
  %t11399 = load %NxVal, ptr @nx__g___main____acc50
  %t11400 = extractvalue %NxVal %t11398, 1
  %t11401 = extractvalue %NxVal %t11399, 1
  %t11402 = add i64 %t11400, %t11401
  %t11403 = add i64 65535, 0
  %t11404 = and i64 %t11402, %t11403
  %t11405 = call %NxVal @nx_int(i64 %t11404)
  store %NxVal %t11405, ptr @nx__g___main____total
  %t11406 = add i64 0, 0
  %t11407 = call %NxVal @nx_int(i64 %t11406)
  store %NxVal %t11407, ptr @nx__g___main____i51
  %t11408 = add i64 0, 0
  %t11409 = call %NxVal @nx_int(i64 %t11408)
  store %NxVal %t11409, ptr @nx__g___main____acc51
  br label %wcond156
wcond156:
  %t11410 = load %NxVal, ptr @nx__g___main____i51
  %t11411 = add i64 3, 0
  %t11412 = extractvalue %NxVal %t11410, 1
  %t11413 = icmp slt i64 %t11412, %t11411
  br i1 %t11413, label %wbody157, label %wend158
wbody157:
  %t11414 = load %NxVal, ptr @nx__g___main____acc51
  %t11415 = load %NxVal, ptr @nx__g___main____c
  %t11416 = load %NxVal, ptr @nx__g___main____i51
  %t11417 = add i64 51, 0
  %t11418 = extractvalue %NxVal %t11416, 1
  %t11419 = add i64 %t11418, %t11417
  %t11421 = getelementptr [2 x %NxVal], ptr %t11420, i64 0, i64 0
  store %NxVal %t11415, ptr %t11421
  %t11422 = call %NxVal @nx_int(i64 %t11419)
  %t11423 = getelementptr [2 x %NxVal], ptr %t11420, i64 0, i64 1
  store %NxVal %t11422, ptr %t11423
  %t11424 = getelementptr [2 x %NxVal], ptr %t11420, i64 0, i64 0
  %t11425 = call %NxVal @nx__m_3____main____Cell__m51(ptr %t11424, i64 2)
  %t11426 = extractvalue %NxVal %t11425, 1
  %t11427 = extractvalue %NxVal %t11414, 1
  %t11428 = add i64 %t11427, %t11426
  %t11429 = add i64 65535, 0
  %t11430 = and i64 %t11428, %t11429
  %t11431 = call %NxVal @nx_int(i64 %t11430)
  store %NxVal %t11431, ptr @nx__g___main____acc51
  %t11432 = load %NxVal, ptr @nx__g___main____acc51
  %t11433 = add i64 2, 0
  %t11434 = extractvalue %NxVal %t11432, 1
  %t11435 = and i64 %t11434, %t11433
  %t11436 = add i64 24, 0
  %t11437 = load %NxVal, ptr @nx__g___main____i51
  %t11438 = extractvalue %NxVal %t11437, 1
  %t11439 = add i64 %t11436, %t11438
  %t11440 = and i64 %t11435, %t11439
  %t11441 = add i64 65535, 0
  %t11442 = and i64 %t11440, %t11441
  %t11443 = call %NxVal @nx_int(i64 %t11442)
  store %NxVal %t11443, ptr @nx__g___main____acc51
  %t11444 = load %NxVal, ptr @nx__g___main____acc51
  %t11445 = add i64 82, 0
  %t11446 = extractvalue %NxVal %t11444, 1
  %t11447 = call i64 @nx_mod_i64(i64 %t11446, i64 %t11445)
  %t11448 = add i64 51, 0
  %t11449 = call i64 @nx_mod_i64(i64 %t11447, i64 %t11448)
  %t11450 = load %NxVal, ptr @nx__g___main____i51
  %t11451 = extractvalue %NxVal %t11450, 1
  %t11452 = add i64 %t11449, %t11451
  %t11453 = add i64 65535, 0
  %t11454 = and i64 %t11452, %t11453
  %t11455 = call %NxVal @nx_int(i64 %t11454)
  store %NxVal %t11455, ptr @nx__g___main____acc51
  %t11456 = load %NxVal, ptr @nx__g___main____acc51
  %t11457 = add i64 14, 0
  %t11458 = extractvalue %NxVal %t11456, 1
  %t11459 = add i64 %t11458, %t11457
  %t11460 = add i64 22, 0
  %t11461 = add i64 %t11459, %t11460
  %t11462 = load %NxVal, ptr @nx__g___main____i51
  %t11463 = extractvalue %NxVal %t11462, 1
  %t11464 = add i64 %t11461, %t11463
  %t11465 = add i64 65535, 0
  %t11466 = and i64 %t11464, %t11465
  %t11467 = call %NxVal @nx_int(i64 %t11466)
  store %NxVal %t11467, ptr @nx__g___main____acc51
  %t11468 = load %NxVal, ptr @nx__g___main____acc51
  %t11469 = add i64 91, 0
  %t11470 = extractvalue %NxVal %t11468, 1
  %t11471 = add i64 %t11470, %t11469
  %t11472 = add i64 72, 0
  %t11473 = add i64 %t11471, %t11472
  %t11474 = load %NxVal, ptr @nx__g___main____i51
  %t11475 = extractvalue %NxVal %t11474, 1
  %t11476 = add i64 %t11473, %t11475
  %t11477 = add i64 65535, 0
  %t11478 = and i64 %t11476, %t11477
  %t11479 = call %NxVal @nx_int(i64 %t11478)
  store %NxVal %t11479, ptr @nx__g___main____acc51
  %t11480 = load %NxVal, ptr @nx__g___main____i51
  %t11481 = add i64 1, 0
  %t11482 = extractvalue %NxVal %t11480, 1
  %t11483 = add i64 %t11482, %t11481
  %t11484 = call %NxVal @nx_int(i64 %t11483)
  store %NxVal %t11484, ptr @nx__g___main____i51
  br label %wcond156
wend158:
  %t11485 = load %NxVal, ptr @nx__g___main____total
  %t11486 = load %NxVal, ptr @nx__g___main____acc51
  %t11487 = extractvalue %NxVal %t11485, 1
  %t11488 = extractvalue %NxVal %t11486, 1
  %t11489 = add i64 %t11487, %t11488
  %t11490 = add i64 65535, 0
  %t11491 = and i64 %t11489, %t11490
  %t11492 = call %NxVal @nx_int(i64 %t11491)
  store %NxVal %t11492, ptr @nx__g___main____total
  %t11493 = add i64 0, 0
  %t11494 = call %NxVal @nx_int(i64 %t11493)
  store %NxVal %t11494, ptr @nx__g___main____i52
  %t11495 = add i64 0, 0
  %t11496 = call %NxVal @nx_int(i64 %t11495)
  store %NxVal %t11496, ptr @nx__g___main____acc52
  br label %wcond159
wcond159:
  %t11497 = load %NxVal, ptr @nx__g___main____i52
  %t11498 = add i64 3, 0
  %t11499 = extractvalue %NxVal %t11497, 1
  %t11500 = icmp slt i64 %t11499, %t11498
  br i1 %t11500, label %wbody160, label %wend161
wbody160:
  %t11501 = load %NxVal, ptr @nx__g___main____acc52
  %t11502 = load %NxVal, ptr @nx__g___main____c
  %t11503 = load %NxVal, ptr @nx__g___main____i52
  %t11504 = add i64 52, 0
  %t11505 = extractvalue %NxVal %t11503, 1
  %t11506 = add i64 %t11505, %t11504
  %t11508 = getelementptr [2 x %NxVal], ptr %t11507, i64 0, i64 0
  store %NxVal %t11502, ptr %t11508
  %t11509 = call %NxVal @nx_int(i64 %t11506)
  %t11510 = getelementptr [2 x %NxVal], ptr %t11507, i64 0, i64 1
  store %NxVal %t11509, ptr %t11510
  %t11511 = getelementptr [2 x %NxVal], ptr %t11507, i64 0, i64 0
  %t11512 = call %NxVal @nx__m_3____main____Cell__m52(ptr %t11511, i64 2)
  %t11513 = extractvalue %NxVal %t11512, 1
  %t11514 = extractvalue %NxVal %t11501, 1
  %t11515 = add i64 %t11514, %t11513
  %t11516 = add i64 65535, 0
  %t11517 = and i64 %t11515, %t11516
  %t11518 = call %NxVal @nx_int(i64 %t11517)
  store %NxVal %t11518, ptr @nx__g___main____acc52
  %t11519 = load %NxVal, ptr @nx__g___main____acc52
  %t11520 = add i64 10, 0
  %t11521 = extractvalue %NxVal %t11519, 1
  %t11522 = mul i64 %t11521, %t11520
  %t11523 = add i64 21, 0
  %t11524 = mul i64 %t11522, %t11523
  %t11525 = load %NxVal, ptr @nx__g___main____i52
  %t11526 = extractvalue %NxVal %t11525, 1
  %t11527 = add i64 %t11524, %t11526
  %t11528 = add i64 65535, 0
  %t11529 = and i64 %t11527, %t11528
  %t11530 = call %NxVal @nx_int(i64 %t11529)
  store %NxVal %t11530, ptr @nx__g___main____acc52
  %t11531 = load %NxVal, ptr @nx__g___main____acc52
  %t11532 = add i64 29, 0
  %t11533 = extractvalue %NxVal %t11531, 1
  %t11534 = sub i64 %t11533, %t11532
  %t11535 = add i64 58, 0
  %t11536 = sub i64 %t11534, %t11535
  %t11537 = load %NxVal, ptr @nx__g___main____i52
  %t11538 = extractvalue %NxVal %t11537, 1
  %t11539 = add i64 %t11536, %t11538
  %t11540 = add i64 65535, 0
  %t11541 = and i64 %t11539, %t11540
  %t11542 = call %NxVal @nx_int(i64 %t11541)
  store %NxVal %t11542, ptr @nx__g___main____acc52
  %t11543 = load %NxVal, ptr @nx__g___main____acc52
  %t11544 = add i64 4, 0
  %t11545 = extractvalue %NxVal %t11543, 1
  %t11546 = mul i64 %t11545, %t11544
  %t11547 = add i64 83, 0
  %t11548 = mul i64 %t11546, %t11547
  %t11549 = load %NxVal, ptr @nx__g___main____i52
  %t11550 = extractvalue %NxVal %t11549, 1
  %t11551 = add i64 %t11548, %t11550
  %t11552 = add i64 65535, 0
  %t11553 = and i64 %t11551, %t11552
  %t11554 = call %NxVal @nx_int(i64 %t11553)
  store %NxVal %t11554, ptr @nx__g___main____acc52
  %t11555 = load %NxVal, ptr @nx__g___main____acc52
  %t11556 = add i64 47, 0
  %t11557 = extractvalue %NxVal %t11555, 1
  %t11558 = and i64 %t11557, %t11556
  %t11559 = add i64 39, 0
  %t11560 = load %NxVal, ptr @nx__g___main____i52
  %t11561 = extractvalue %NxVal %t11560, 1
  %t11562 = add i64 %t11559, %t11561
  %t11563 = and i64 %t11558, %t11562
  %t11564 = add i64 65535, 0
  %t11565 = and i64 %t11563, %t11564
  %t11566 = call %NxVal @nx_int(i64 %t11565)
  store %NxVal %t11566, ptr @nx__g___main____acc52
  %t11567 = load %NxVal, ptr @nx__g___main____i52
  %t11568 = add i64 1, 0
  %t11569 = extractvalue %NxVal %t11567, 1
  %t11570 = add i64 %t11569, %t11568
  %t11571 = call %NxVal @nx_int(i64 %t11570)
  store %NxVal %t11571, ptr @nx__g___main____i52
  br label %wcond159
wend161:
  %t11572 = load %NxVal, ptr @nx__g___main____total
  %t11573 = load %NxVal, ptr @nx__g___main____acc52
  %t11574 = extractvalue %NxVal %t11572, 1
  %t11575 = extractvalue %NxVal %t11573, 1
  %t11576 = add i64 %t11574, %t11575
  %t11577 = add i64 65535, 0
  %t11578 = and i64 %t11576, %t11577
  %t11579 = call %NxVal @nx_int(i64 %t11578)
  store %NxVal %t11579, ptr @nx__g___main____total
  %t11580 = add i64 0, 0
  %t11581 = call %NxVal @nx_int(i64 %t11580)
  store %NxVal %t11581, ptr @nx__g___main____i53
  %t11582 = add i64 0, 0
  %t11583 = call %NxVal @nx_int(i64 %t11582)
  store %NxVal %t11583, ptr @nx__g___main____acc53
  br label %wcond162
wcond162:
  %t11584 = load %NxVal, ptr @nx__g___main____i53
  %t11585 = add i64 3, 0
  %t11586 = extractvalue %NxVal %t11584, 1
  %t11587 = icmp slt i64 %t11586, %t11585
  br i1 %t11587, label %wbody163, label %wend164
wbody163:
  %t11588 = load %NxVal, ptr @nx__g___main____acc53
  %t11589 = load %NxVal, ptr @nx__g___main____c
  %t11590 = load %NxVal, ptr @nx__g___main____i53
  %t11591 = add i64 53, 0
  %t11592 = extractvalue %NxVal %t11590, 1
  %t11593 = add i64 %t11592, %t11591
  %t11595 = getelementptr [2 x %NxVal], ptr %t11594, i64 0, i64 0
  store %NxVal %t11589, ptr %t11595
  %t11596 = call %NxVal @nx_int(i64 %t11593)
  %t11597 = getelementptr [2 x %NxVal], ptr %t11594, i64 0, i64 1
  store %NxVal %t11596, ptr %t11597
  %t11598 = getelementptr [2 x %NxVal], ptr %t11594, i64 0, i64 0
  %t11599 = call %NxVal @nx__m_3____main____Cell__m53(ptr %t11598, i64 2)
  %t11600 = extractvalue %NxVal %t11599, 1
  %t11601 = extractvalue %NxVal %t11588, 1
  %t11602 = add i64 %t11601, %t11600
  %t11603 = add i64 65535, 0
  %t11604 = and i64 %t11602, %t11603
  %t11605 = call %NxVal @nx_int(i64 %t11604)
  store %NxVal %t11605, ptr @nx__g___main____acc53
  %t11606 = load %NxVal, ptr @nx__g___main____acc53
  %t11607 = add i64 85, 0
  %t11608 = extractvalue %NxVal %t11606, 1
  %t11609 = add i64 %t11608, %t11607
  %t11610 = add i64 22, 0
  %t11611 = add i64 %t11609, %t11610
  %t11612 = load %NxVal, ptr @nx__g___main____i53
  %t11613 = extractvalue %NxVal %t11612, 1
  %t11614 = add i64 %t11611, %t11613
  %t11615 = add i64 65535, 0
  %t11616 = and i64 %t11614, %t11615
  %t11617 = call %NxVal @nx_int(i64 %t11616)
  store %NxVal %t11617, ptr @nx__g___main____acc53
  %t11618 = load %NxVal, ptr @nx__g___main____acc53
  %t11619 = add i64 56, 0
  %t11620 = extractvalue %NxVal %t11618, 1
  %t11621 = or i64 %t11620, %t11619
  %t11622 = add i64 85, 0
  %t11623 = load %NxVal, ptr @nx__g___main____i53
  %t11624 = extractvalue %NxVal %t11623, 1
  %t11625 = add i64 %t11622, %t11624
  %t11626 = or i64 %t11621, %t11625
  %t11627 = add i64 65535, 0
  %t11628 = and i64 %t11626, %t11627
  %t11629 = call %NxVal @nx_int(i64 %t11628)
  store %NxVal %t11629, ptr @nx__g___main____acc53
  %t11630 = load %NxVal, ptr @nx__g___main____acc53
  %t11631 = add i64 12, 0
  %t11632 = extractvalue %NxVal %t11630, 1
  %t11633 = xor i64 %t11632, %t11631
  %t11634 = add i64 22, 0
  %t11635 = load %NxVal, ptr @nx__g___main____i53
  %t11636 = extractvalue %NxVal %t11635, 1
  %t11637 = add i64 %t11634, %t11636
  %t11638 = xor i64 %t11633, %t11637
  %t11639 = add i64 65535, 0
  %t11640 = and i64 %t11638, %t11639
  %t11641 = call %NxVal @nx_int(i64 %t11640)
  store %NxVal %t11641, ptr @nx__g___main____acc53
  %t11642 = load %NxVal, ptr @nx__g___main____acc53
  %t11643 = add i64 78, 0
  %t11644 = extractvalue %NxVal %t11642, 1
  %t11645 = call i64 @nx_mod_i64(i64 %t11644, i64 %t11643)
  %t11646 = add i64 45, 0
  %t11647 = call i64 @nx_mod_i64(i64 %t11645, i64 %t11646)
  %t11648 = load %NxVal, ptr @nx__g___main____i53
  %t11649 = extractvalue %NxVal %t11648, 1
  %t11650 = add i64 %t11647, %t11649
  %t11651 = add i64 65535, 0
  %t11652 = and i64 %t11650, %t11651
  %t11653 = call %NxVal @nx_int(i64 %t11652)
  store %NxVal %t11653, ptr @nx__g___main____acc53
  %t11654 = load %NxVal, ptr @nx__g___main____i53
  %t11655 = add i64 1, 0
  %t11656 = extractvalue %NxVal %t11654, 1
  %t11657 = add i64 %t11656, %t11655
  %t11658 = call %NxVal @nx_int(i64 %t11657)
  store %NxVal %t11658, ptr @nx__g___main____i53
  br label %wcond162
wend164:
  %t11659 = load %NxVal, ptr @nx__g___main____total
  %t11660 = load %NxVal, ptr @nx__g___main____acc53
  %t11661 = extractvalue %NxVal %t11659, 1
  %t11662 = extractvalue %NxVal %t11660, 1
  %t11663 = add i64 %t11661, %t11662
  %t11664 = add i64 65535, 0
  %t11665 = and i64 %t11663, %t11664
  %t11666 = call %NxVal @nx_int(i64 %t11665)
  store %NxVal %t11666, ptr @nx__g___main____total
  %t11667 = add i64 0, 0
  %t11668 = call %NxVal @nx_int(i64 %t11667)
  store %NxVal %t11668, ptr @nx__g___main____i54
  %t11669 = add i64 0, 0
  %t11670 = call %NxVal @nx_int(i64 %t11669)
  store %NxVal %t11670, ptr @nx__g___main____acc54
  br label %wcond165
wcond165:
  %t11671 = load %NxVal, ptr @nx__g___main____i54
  %t11672 = add i64 3, 0
  %t11673 = extractvalue %NxVal %t11671, 1
  %t11674 = icmp slt i64 %t11673, %t11672
  br i1 %t11674, label %wbody166, label %wend167
wbody166:
  %t11675 = load %NxVal, ptr @nx__g___main____acc54
  %t11676 = load %NxVal, ptr @nx__g___main____c
  %t11677 = load %NxVal, ptr @nx__g___main____i54
  %t11678 = add i64 54, 0
  %t11679 = extractvalue %NxVal %t11677, 1
  %t11680 = add i64 %t11679, %t11678
  %t11682 = getelementptr [2 x %NxVal], ptr %t11681, i64 0, i64 0
  store %NxVal %t11676, ptr %t11682
  %t11683 = call %NxVal @nx_int(i64 %t11680)
  %t11684 = getelementptr [2 x %NxVal], ptr %t11681, i64 0, i64 1
  store %NxVal %t11683, ptr %t11684
  %t11685 = getelementptr [2 x %NxVal], ptr %t11681, i64 0, i64 0
  %t11686 = call %NxVal @nx__m_3____main____Cell__m54(ptr %t11685, i64 2)
  %t11687 = extractvalue %NxVal %t11686, 1
  %t11688 = extractvalue %NxVal %t11675, 1
  %t11689 = add i64 %t11688, %t11687
  %t11690 = add i64 65535, 0
  %t11691 = and i64 %t11689, %t11690
  %t11692 = call %NxVal @nx_int(i64 %t11691)
  store %NxVal %t11692, ptr @nx__g___main____acc54
  %t11693 = load %NxVal, ptr @nx__g___main____acc54
  %t11694 = add i64 39, 0
  %t11695 = extractvalue %NxVal %t11693, 1
  %t11696 = xor i64 %t11695, %t11694
  %t11697 = add i64 68, 0
  %t11698 = load %NxVal, ptr @nx__g___main____i54
  %t11699 = extractvalue %NxVal %t11698, 1
  %t11700 = add i64 %t11697, %t11699
  %t11701 = xor i64 %t11696, %t11700
  %t11702 = add i64 65535, 0
  %t11703 = and i64 %t11701, %t11702
  %t11704 = call %NxVal @nx_int(i64 %t11703)
  store %NxVal %t11704, ptr @nx__g___main____acc54
  %t11705 = load %NxVal, ptr @nx__g___main____acc54
  %t11706 = add i64 19, 0
  %t11707 = extractvalue %NxVal %t11705, 1
  %t11708 = xor i64 %t11707, %t11706
  %t11709 = add i64 39, 0
  %t11710 = load %NxVal, ptr @nx__g___main____i54
  %t11711 = extractvalue %NxVal %t11710, 1
  %t11712 = add i64 %t11709, %t11711
  %t11713 = xor i64 %t11708, %t11712
  %t11714 = add i64 65535, 0
  %t11715 = and i64 %t11713, %t11714
  %t11716 = call %NxVal @nx_int(i64 %t11715)
  store %NxVal %t11716, ptr @nx__g___main____acc54
  %t11717 = load %NxVal, ptr @nx__g___main____acc54
  %t11718 = add i64 53, 0
  %t11719 = extractvalue %NxVal %t11717, 1
  %t11720 = call i64 @nx_mod_i64(i64 %t11719, i64 %t11718)
  %t11721 = add i64 88, 0
  %t11722 = call i64 @nx_mod_i64(i64 %t11720, i64 %t11721)
  %t11723 = load %NxVal, ptr @nx__g___main____i54
  %t11724 = extractvalue %NxVal %t11723, 1
  %t11725 = add i64 %t11722, %t11724
  %t11726 = add i64 65535, 0
  %t11727 = and i64 %t11725, %t11726
  %t11728 = call %NxVal @nx_int(i64 %t11727)
  store %NxVal %t11728, ptr @nx__g___main____acc54
  %t11729 = load %NxVal, ptr @nx__g___main____acc54
  %t11730 = add i64 73, 0
  %t11731 = extractvalue %NxVal %t11729, 1
  %t11732 = and i64 %t11731, %t11730
  %t11733 = add i64 24, 0
  %t11734 = load %NxVal, ptr @nx__g___main____i54
  %t11735 = extractvalue %NxVal %t11734, 1
  %t11736 = add i64 %t11733, %t11735
  %t11737 = and i64 %t11732, %t11736
  %t11738 = add i64 65535, 0
  %t11739 = and i64 %t11737, %t11738
  %t11740 = call %NxVal @nx_int(i64 %t11739)
  store %NxVal %t11740, ptr @nx__g___main____acc54
  %t11741 = load %NxVal, ptr @nx__g___main____i54
  %t11742 = add i64 1, 0
  %t11743 = extractvalue %NxVal %t11741, 1
  %t11744 = add i64 %t11743, %t11742
  %t11745 = call %NxVal @nx_int(i64 %t11744)
  store %NxVal %t11745, ptr @nx__g___main____i54
  br label %wcond165
wend167:
  %t11746 = load %NxVal, ptr @nx__g___main____total
  %t11747 = load %NxVal, ptr @nx__g___main____acc54
  %t11748 = extractvalue %NxVal %t11746, 1
  %t11749 = extractvalue %NxVal %t11747, 1
  %t11750 = add i64 %t11748, %t11749
  %t11751 = add i64 65535, 0
  %t11752 = and i64 %t11750, %t11751
  %t11753 = call %NxVal @nx_int(i64 %t11752)
  store %NxVal %t11753, ptr @nx__g___main____total
  %t11754 = add i64 0, 0
  %t11755 = call %NxVal @nx_int(i64 %t11754)
  store %NxVal %t11755, ptr @nx__g___main____i55
  %t11756 = add i64 0, 0
  %t11757 = call %NxVal @nx_int(i64 %t11756)
  store %NxVal %t11757, ptr @nx__g___main____acc55
  br label %wcond168
wcond168:
  %t11758 = load %NxVal, ptr @nx__g___main____i55
  %t11759 = add i64 3, 0
  %t11760 = extractvalue %NxVal %t11758, 1
  %t11761 = icmp slt i64 %t11760, %t11759
  br i1 %t11761, label %wbody169, label %wend170
wbody169:
  %t11762 = load %NxVal, ptr @nx__g___main____acc55
  %t11763 = load %NxVal, ptr @nx__g___main____c
  %t11764 = load %NxVal, ptr @nx__g___main____i55
  %t11765 = add i64 55, 0
  %t11766 = extractvalue %NxVal %t11764, 1
  %t11767 = add i64 %t11766, %t11765
  %t11769 = getelementptr [2 x %NxVal], ptr %t11768, i64 0, i64 0
  store %NxVal %t11763, ptr %t11769
  %t11770 = call %NxVal @nx_int(i64 %t11767)
  %t11771 = getelementptr [2 x %NxVal], ptr %t11768, i64 0, i64 1
  store %NxVal %t11770, ptr %t11771
  %t11772 = getelementptr [2 x %NxVal], ptr %t11768, i64 0, i64 0
  %t11773 = call %NxVal @nx__m_3____main____Cell__m55(ptr %t11772, i64 2)
  %t11774 = extractvalue %NxVal %t11773, 1
  %t11775 = extractvalue %NxVal %t11762, 1
  %t11776 = add i64 %t11775, %t11774
  %t11777 = add i64 65535, 0
  %t11778 = and i64 %t11776, %t11777
  %t11779 = call %NxVal @nx_int(i64 %t11778)
  store %NxVal %t11779, ptr @nx__g___main____acc55
  %t11780 = load %NxVal, ptr @nx__g___main____acc55
  %t11781 = add i64 20, 0
  %t11782 = extractvalue %NxVal %t11780, 1
  %t11783 = and i64 %t11782, %t11781
  %t11784 = add i64 80, 0
  %t11785 = load %NxVal, ptr @nx__g___main____i55
  %t11786 = extractvalue %NxVal %t11785, 1
  %t11787 = add i64 %t11784, %t11786
  %t11788 = and i64 %t11783, %t11787
  %t11789 = add i64 65535, 0
  %t11790 = and i64 %t11788, %t11789
  %t11791 = call %NxVal @nx_int(i64 %t11790)
  store %NxVal %t11791, ptr @nx__g___main____acc55
  %t11792 = load %NxVal, ptr @nx__g___main____acc55
  %t11793 = add i64 86, 0
  %t11794 = extractvalue %NxVal %t11792, 1
  %t11795 = xor i64 %t11794, %t11793
  %t11796 = add i64 50, 0
  %t11797 = load %NxVal, ptr @nx__g___main____i55
  %t11798 = extractvalue %NxVal %t11797, 1
  %t11799 = add i64 %t11796, %t11798
  %t11800 = xor i64 %t11795, %t11799
  %t11801 = add i64 65535, 0
  %t11802 = and i64 %t11800, %t11801
  %t11803 = call %NxVal @nx_int(i64 %t11802)
  store %NxVal %t11803, ptr @nx__g___main____acc55
  %t11804 = load %NxVal, ptr @nx__g___main____acc55
  %t11805 = add i64 77, 0
  %t11806 = extractvalue %NxVal %t11804, 1
  %t11807 = mul i64 %t11806, %t11805
  %t11808 = add i64 84, 0
  %t11809 = mul i64 %t11807, %t11808
  %t11810 = load %NxVal, ptr @nx__g___main____i55
  %t11811 = extractvalue %NxVal %t11810, 1
  %t11812 = add i64 %t11809, %t11811
  %t11813 = add i64 65535, 0
  %t11814 = and i64 %t11812, %t11813
  %t11815 = call %NxVal @nx_int(i64 %t11814)
  store %NxVal %t11815, ptr @nx__g___main____acc55
  %t11816 = load %NxVal, ptr @nx__g___main____acc55
  %t11817 = add i64 18, 0
  %t11818 = extractvalue %NxVal %t11816, 1
  %t11819 = and i64 %t11818, %t11817
  %t11820 = add i64 31, 0
  %t11821 = load %NxVal, ptr @nx__g___main____i55
  %t11822 = extractvalue %NxVal %t11821, 1
  %t11823 = add i64 %t11820, %t11822
  %t11824 = and i64 %t11819, %t11823
  %t11825 = add i64 65535, 0
  %t11826 = and i64 %t11824, %t11825
  %t11827 = call %NxVal @nx_int(i64 %t11826)
  store %NxVal %t11827, ptr @nx__g___main____acc55
  %t11828 = load %NxVal, ptr @nx__g___main____i55
  %t11829 = add i64 1, 0
  %t11830 = extractvalue %NxVal %t11828, 1
  %t11831 = add i64 %t11830, %t11829
  %t11832 = call %NxVal @nx_int(i64 %t11831)
  store %NxVal %t11832, ptr @nx__g___main____i55
  br label %wcond168
wend170:
  %t11833 = load %NxVal, ptr @nx__g___main____total
  %t11834 = load %NxVal, ptr @nx__g___main____acc55
  %t11835 = extractvalue %NxVal %t11833, 1
  %t11836 = extractvalue %NxVal %t11834, 1
  %t11837 = add i64 %t11835, %t11836
  %t11838 = add i64 65535, 0
  %t11839 = and i64 %t11837, %t11838
  %t11840 = call %NxVal @nx_int(i64 %t11839)
  store %NxVal %t11840, ptr @nx__g___main____total
  %t11841 = add i64 0, 0
  %t11842 = call %NxVal @nx_int(i64 %t11841)
  store %NxVal %t11842, ptr @nx__g___main____i56
  %t11843 = add i64 0, 0
  %t11844 = call %NxVal @nx_int(i64 %t11843)
  store %NxVal %t11844, ptr @nx__g___main____acc56
  br label %wcond171
wcond171:
  %t11845 = load %NxVal, ptr @nx__g___main____i56
  %t11846 = add i64 3, 0
  %t11847 = extractvalue %NxVal %t11845, 1
  %t11848 = icmp slt i64 %t11847, %t11846
  br i1 %t11848, label %wbody172, label %wend173
wbody172:
  %t11849 = load %NxVal, ptr @nx__g___main____acc56
  %t11850 = load %NxVal, ptr @nx__g___main____c
  %t11851 = load %NxVal, ptr @nx__g___main____i56
  %t11852 = add i64 56, 0
  %t11853 = extractvalue %NxVal %t11851, 1
  %t11854 = add i64 %t11853, %t11852
  %t11856 = getelementptr [2 x %NxVal], ptr %t11855, i64 0, i64 0
  store %NxVal %t11850, ptr %t11856
  %t11857 = call %NxVal @nx_int(i64 %t11854)
  %t11858 = getelementptr [2 x %NxVal], ptr %t11855, i64 0, i64 1
  store %NxVal %t11857, ptr %t11858
  %t11859 = getelementptr [2 x %NxVal], ptr %t11855, i64 0, i64 0
  %t11860 = call %NxVal @nx__m_3____main____Cell__m56(ptr %t11859, i64 2)
  %t11861 = extractvalue %NxVal %t11860, 1
  %t11862 = extractvalue %NxVal %t11849, 1
  %t11863 = add i64 %t11862, %t11861
  %t11864 = add i64 65535, 0
  %t11865 = and i64 %t11863, %t11864
  %t11866 = call %NxVal @nx_int(i64 %t11865)
  store %NxVal %t11866, ptr @nx__g___main____acc56
  %t11867 = load %NxVal, ptr @nx__g___main____acc56
  %t11868 = add i64 51, 0
  %t11869 = extractvalue %NxVal %t11867, 1
  %t11870 = or i64 %t11869, %t11868
  %t11871 = add i64 47, 0
  %t11872 = load %NxVal, ptr @nx__g___main____i56
  %t11873 = extractvalue %NxVal %t11872, 1
  %t11874 = add i64 %t11871, %t11873
  %t11875 = or i64 %t11870, %t11874
  %t11876 = add i64 65535, 0
  %t11877 = and i64 %t11875, %t11876
  %t11878 = call %NxVal @nx_int(i64 %t11877)
  store %NxVal %t11878, ptr @nx__g___main____acc56
  %t11879 = load %NxVal, ptr @nx__g___main____acc56
  %t11880 = add i64 45, 0
  %t11881 = extractvalue %NxVal %t11879, 1
  %t11882 = call i64 @nx_mod_i64(i64 %t11881, i64 %t11880)
  %t11883 = add i64 5, 0
  %t11884 = call i64 @nx_mod_i64(i64 %t11882, i64 %t11883)
  %t11885 = load %NxVal, ptr @nx__g___main____i56
  %t11886 = extractvalue %NxVal %t11885, 1
  %t11887 = add i64 %t11884, %t11886
  %t11888 = add i64 65535, 0
  %t11889 = and i64 %t11887, %t11888
  %t11890 = call %NxVal @nx_int(i64 %t11889)
  store %NxVal %t11890, ptr @nx__g___main____acc56
  %t11891 = load %NxVal, ptr @nx__g___main____acc56
  %t11892 = add i64 9, 0
  %t11893 = extractvalue %NxVal %t11891, 1
  %t11894 = or i64 %t11893, %t11892
  %t11895 = add i64 41, 0
  %t11896 = load %NxVal, ptr @nx__g___main____i56
  %t11897 = extractvalue %NxVal %t11896, 1
  %t11898 = add i64 %t11895, %t11897
  %t11899 = or i64 %t11894, %t11898
  %t11900 = add i64 65535, 0
  %t11901 = and i64 %t11899, %t11900
  %t11902 = call %NxVal @nx_int(i64 %t11901)
  store %NxVal %t11902, ptr @nx__g___main____acc56
  %t11903 = load %NxVal, ptr @nx__g___main____acc56
  %t11904 = add i64 76, 0
  %t11905 = extractvalue %NxVal %t11903, 1
  %t11906 = xor i64 %t11905, %t11904
  %t11907 = add i64 74, 0
  %t11908 = load %NxVal, ptr @nx__g___main____i56
  %t11909 = extractvalue %NxVal %t11908, 1
  %t11910 = add i64 %t11907, %t11909
  %t11911 = xor i64 %t11906, %t11910
  %t11912 = add i64 65535, 0
  %t11913 = and i64 %t11911, %t11912
  %t11914 = call %NxVal @nx_int(i64 %t11913)
  store %NxVal %t11914, ptr @nx__g___main____acc56
  %t11915 = load %NxVal, ptr @nx__g___main____i56
  %t11916 = add i64 1, 0
  %t11917 = extractvalue %NxVal %t11915, 1
  %t11918 = add i64 %t11917, %t11916
  %t11919 = call %NxVal @nx_int(i64 %t11918)
  store %NxVal %t11919, ptr @nx__g___main____i56
  br label %wcond171
wend173:
  %t11920 = load %NxVal, ptr @nx__g___main____total
  %t11921 = load %NxVal, ptr @nx__g___main____acc56
  %t11922 = extractvalue %NxVal %t11920, 1
  %t11923 = extractvalue %NxVal %t11921, 1
  %t11924 = add i64 %t11922, %t11923
  %t11925 = add i64 65535, 0
  %t11926 = and i64 %t11924, %t11925
  %t11927 = call %NxVal @nx_int(i64 %t11926)
  store %NxVal %t11927, ptr @nx__g___main____total
  %t11928 = add i64 0, 0
  %t11929 = call %NxVal @nx_int(i64 %t11928)
  store %NxVal %t11929, ptr @nx__g___main____i57
  %t11930 = add i64 0, 0
  %t11931 = call %NxVal @nx_int(i64 %t11930)
  store %NxVal %t11931, ptr @nx__g___main____acc57
  br label %wcond174
wcond174:
  %t11932 = load %NxVal, ptr @nx__g___main____i57
  %t11933 = add i64 3, 0
  %t11934 = extractvalue %NxVal %t11932, 1
  %t11935 = icmp slt i64 %t11934, %t11933
  br i1 %t11935, label %wbody175, label %wend176
wbody175:
  %t11936 = load %NxVal, ptr @nx__g___main____acc57
  %t11937 = load %NxVal, ptr @nx__g___main____c
  %t11938 = load %NxVal, ptr @nx__g___main____i57
  %t11939 = add i64 57, 0
  %t11940 = extractvalue %NxVal %t11938, 1
  %t11941 = add i64 %t11940, %t11939
  %t11943 = getelementptr [2 x %NxVal], ptr %t11942, i64 0, i64 0
  store %NxVal %t11937, ptr %t11943
  %t11944 = call %NxVal @nx_int(i64 %t11941)
  %t11945 = getelementptr [2 x %NxVal], ptr %t11942, i64 0, i64 1
  store %NxVal %t11944, ptr %t11945
  %t11946 = getelementptr [2 x %NxVal], ptr %t11942, i64 0, i64 0
  %t11947 = call %NxVal @nx__m_3____main____Cell__m57(ptr %t11946, i64 2)
  %t11948 = extractvalue %NxVal %t11947, 1
  %t11949 = extractvalue %NxVal %t11936, 1
  %t11950 = add i64 %t11949, %t11948
  %t11951 = add i64 65535, 0
  %t11952 = and i64 %t11950, %t11951
  %t11953 = call %NxVal @nx_int(i64 %t11952)
  store %NxVal %t11953, ptr @nx__g___main____acc57
  %t11954 = load %NxVal, ptr @nx__g___main____acc57
  %t11955 = add i64 7, 0
  %t11956 = extractvalue %NxVal %t11954, 1
  %t11957 = sub i64 %t11956, %t11955
  %t11958 = add i64 87, 0
  %t11959 = sub i64 %t11957, %t11958
  %t11960 = load %NxVal, ptr @nx__g___main____i57
  %t11961 = extractvalue %NxVal %t11960, 1
  %t11962 = add i64 %t11959, %t11961
  %t11963 = add i64 65535, 0
  %t11964 = and i64 %t11962, %t11963
  %t11965 = call %NxVal @nx_int(i64 %t11964)
  store %NxVal %t11965, ptr @nx__g___main____acc57
  %t11966 = load %NxVal, ptr @nx__g___main____acc57
  %t11967 = add i64 16, 0
  %t11968 = extractvalue %NxVal %t11966, 1
  %t11969 = call i64 @nx_mod_i64(i64 %t11968, i64 %t11967)
  %t11970 = add i64 67, 0
  %t11971 = call i64 @nx_mod_i64(i64 %t11969, i64 %t11970)
  %t11972 = load %NxVal, ptr @nx__g___main____i57
  %t11973 = extractvalue %NxVal %t11972, 1
  %t11974 = add i64 %t11971, %t11973
  %t11975 = add i64 65535, 0
  %t11976 = and i64 %t11974, %t11975
  %t11977 = call %NxVal @nx_int(i64 %t11976)
  store %NxVal %t11977, ptr @nx__g___main____acc57
  %t11978 = load %NxVal, ptr @nx__g___main____acc57
  %t11979 = add i64 55, 0
  %t11980 = extractvalue %NxVal %t11978, 1
  %t11981 = and i64 %t11980, %t11979
  %t11982 = add i64 45, 0
  %t11983 = load %NxVal, ptr @nx__g___main____i57
  %t11984 = extractvalue %NxVal %t11983, 1
  %t11985 = add i64 %t11982, %t11984
  %t11986 = and i64 %t11981, %t11985
  %t11987 = add i64 65535, 0
  %t11988 = and i64 %t11986, %t11987
  %t11989 = call %NxVal @nx_int(i64 %t11988)
  store %NxVal %t11989, ptr @nx__g___main____acc57
  %t11990 = load %NxVal, ptr @nx__g___main____acc57
  %t11991 = add i64 81, 0
  %t11992 = extractvalue %NxVal %t11990, 1
  %t11993 = or i64 %t11992, %t11991
  %t11994 = add i64 71, 0
  %t11995 = load %NxVal, ptr @nx__g___main____i57
  %t11996 = extractvalue %NxVal %t11995, 1
  %t11997 = add i64 %t11994, %t11996
  %t11998 = or i64 %t11993, %t11997
  %t11999 = add i64 65535, 0
  %t12000 = and i64 %t11998, %t11999
  %t12001 = call %NxVal @nx_int(i64 %t12000)
  store %NxVal %t12001, ptr @nx__g___main____acc57
  %t12002 = load %NxVal, ptr @nx__g___main____i57
  %t12003 = add i64 1, 0
  %t12004 = extractvalue %NxVal %t12002, 1
  %t12005 = add i64 %t12004, %t12003
  %t12006 = call %NxVal @nx_int(i64 %t12005)
  store %NxVal %t12006, ptr @nx__g___main____i57
  br label %wcond174
wend176:
  %t12007 = load %NxVal, ptr @nx__g___main____total
  %t12008 = load %NxVal, ptr @nx__g___main____acc57
  %t12009 = extractvalue %NxVal %t12007, 1
  %t12010 = extractvalue %NxVal %t12008, 1
  %t12011 = add i64 %t12009, %t12010
  %t12012 = add i64 65535, 0
  %t12013 = and i64 %t12011, %t12012
  %t12014 = call %NxVal @nx_int(i64 %t12013)
  store %NxVal %t12014, ptr @nx__g___main____total
  %t12015 = add i64 0, 0
  %t12016 = call %NxVal @nx_int(i64 %t12015)
  store %NxVal %t12016, ptr @nx__g___main____i58
  %t12017 = add i64 0, 0
  %t12018 = call %NxVal @nx_int(i64 %t12017)
  store %NxVal %t12018, ptr @nx__g___main____acc58
  br label %wcond177
wcond177:
  %t12019 = load %NxVal, ptr @nx__g___main____i58
  %t12020 = add i64 3, 0
  %t12021 = extractvalue %NxVal %t12019, 1
  %t12022 = icmp slt i64 %t12021, %t12020
  br i1 %t12022, label %wbody178, label %wend179
wbody178:
  %t12023 = load %NxVal, ptr @nx__g___main____acc58
  %t12024 = load %NxVal, ptr @nx__g___main____c
  %t12025 = load %NxVal, ptr @nx__g___main____i58
  %t12026 = add i64 58, 0
  %t12027 = extractvalue %NxVal %t12025, 1
  %t12028 = add i64 %t12027, %t12026
  %t12030 = getelementptr [2 x %NxVal], ptr %t12029, i64 0, i64 0
  store %NxVal %t12024, ptr %t12030
  %t12031 = call %NxVal @nx_int(i64 %t12028)
  %t12032 = getelementptr [2 x %NxVal], ptr %t12029, i64 0, i64 1
  store %NxVal %t12031, ptr %t12032
  %t12033 = getelementptr [2 x %NxVal], ptr %t12029, i64 0, i64 0
  %t12034 = call %NxVal @nx__m_3____main____Cell__m58(ptr %t12033, i64 2)
  %t12035 = extractvalue %NxVal %t12034, 1
  %t12036 = extractvalue %NxVal %t12023, 1
  %t12037 = add i64 %t12036, %t12035
  %t12038 = add i64 65535, 0
  %t12039 = and i64 %t12037, %t12038
  %t12040 = call %NxVal @nx_int(i64 %t12039)
  store %NxVal %t12040, ptr @nx__g___main____acc58
  %t12041 = load %NxVal, ptr @nx__g___main____acc58
  %t12042 = add i64 58, 0
  %t12043 = extractvalue %NxVal %t12041, 1
  %t12044 = xor i64 %t12043, %t12042
  %t12045 = add i64 27, 0
  %t12046 = load %NxVal, ptr @nx__g___main____i58
  %t12047 = extractvalue %NxVal %t12046, 1
  %t12048 = add i64 %t12045, %t12047
  %t12049 = xor i64 %t12044, %t12048
  %t12050 = add i64 65535, 0
  %t12051 = and i64 %t12049, %t12050
  %t12052 = call %NxVal @nx_int(i64 %t12051)
  store %NxVal %t12052, ptr @nx__g___main____acc58
  %t12053 = load %NxVal, ptr @nx__g___main____acc58
  %t12054 = add i64 58, 0
  %t12055 = extractvalue %NxVal %t12053, 1
  %t12056 = add i64 %t12055, %t12054
  %t12057 = add i64 80, 0
  %t12058 = add i64 %t12056, %t12057
  %t12059 = load %NxVal, ptr @nx__g___main____i58
  %t12060 = extractvalue %NxVal %t12059, 1
  %t12061 = add i64 %t12058, %t12060
  %t12062 = add i64 65535, 0
  %t12063 = and i64 %t12061, %t12062
  %t12064 = call %NxVal @nx_int(i64 %t12063)
  store %NxVal %t12064, ptr @nx__g___main____acc58
  %t12065 = load %NxVal, ptr @nx__g___main____acc58
  %t12066 = add i64 70, 0
  %t12067 = extractvalue %NxVal %t12065, 1
  %t12068 = mul i64 %t12067, %t12066
  %t12069 = add i64 70, 0
  %t12070 = mul i64 %t12068, %t12069
  %t12071 = load %NxVal, ptr @nx__g___main____i58
  %t12072 = extractvalue %NxVal %t12071, 1
  %t12073 = add i64 %t12070, %t12072
  %t12074 = add i64 65535, 0
  %t12075 = and i64 %t12073, %t12074
  %t12076 = call %NxVal @nx_int(i64 %t12075)
  store %NxVal %t12076, ptr @nx__g___main____acc58
  %t12077 = load %NxVal, ptr @nx__g___main____acc58
  %t12078 = add i64 3, 0
  %t12079 = extractvalue %NxVal %t12077, 1
  %t12080 = call i64 @nx_mod_i64(i64 %t12079, i64 %t12078)
  %t12081 = add i64 2, 0
  %t12082 = call i64 @nx_mod_i64(i64 %t12080, i64 %t12081)
  %t12083 = load %NxVal, ptr @nx__g___main____i58
  %t12084 = extractvalue %NxVal %t12083, 1
  %t12085 = add i64 %t12082, %t12084
  %t12086 = add i64 65535, 0
  %t12087 = and i64 %t12085, %t12086
  %t12088 = call %NxVal @nx_int(i64 %t12087)
  store %NxVal %t12088, ptr @nx__g___main____acc58
  %t12089 = load %NxVal, ptr @nx__g___main____i58
  %t12090 = add i64 1, 0
  %t12091 = extractvalue %NxVal %t12089, 1
  %t12092 = add i64 %t12091, %t12090
  %t12093 = call %NxVal @nx_int(i64 %t12092)
  store %NxVal %t12093, ptr @nx__g___main____i58
  br label %wcond177
wend179:
  %t12094 = load %NxVal, ptr @nx__g___main____total
  %t12095 = load %NxVal, ptr @nx__g___main____acc58
  %t12096 = extractvalue %NxVal %t12094, 1
  %t12097 = extractvalue %NxVal %t12095, 1
  %t12098 = add i64 %t12096, %t12097
  %t12099 = add i64 65535, 0
  %t12100 = and i64 %t12098, %t12099
  %t12101 = call %NxVal @nx_int(i64 %t12100)
  store %NxVal %t12101, ptr @nx__g___main____total
  %t12102 = add i64 0, 0
  %t12103 = call %NxVal @nx_int(i64 %t12102)
  store %NxVal %t12103, ptr @nx__g___main____i59
  %t12104 = add i64 0, 0
  %t12105 = call %NxVal @nx_int(i64 %t12104)
  store %NxVal %t12105, ptr @nx__g___main____acc59
  br label %wcond180
wcond180:
  %t12106 = load %NxVal, ptr @nx__g___main____i59
  %t12107 = add i64 3, 0
  %t12108 = extractvalue %NxVal %t12106, 1
  %t12109 = icmp slt i64 %t12108, %t12107
  br i1 %t12109, label %wbody181, label %wend182
wbody181:
  %t12110 = load %NxVal, ptr @nx__g___main____acc59
  %t12111 = load %NxVal, ptr @nx__g___main____c
  %t12112 = load %NxVal, ptr @nx__g___main____i59
  %t12113 = add i64 59, 0
  %t12114 = extractvalue %NxVal %t12112, 1
  %t12115 = add i64 %t12114, %t12113
  %t12117 = getelementptr [2 x %NxVal], ptr %t12116, i64 0, i64 0
  store %NxVal %t12111, ptr %t12117
  %t12118 = call %NxVal @nx_int(i64 %t12115)
  %t12119 = getelementptr [2 x %NxVal], ptr %t12116, i64 0, i64 1
  store %NxVal %t12118, ptr %t12119
  %t12120 = getelementptr [2 x %NxVal], ptr %t12116, i64 0, i64 0
  %t12121 = call %NxVal @nx__m_3____main____Cell__m59(ptr %t12120, i64 2)
  %t12122 = extractvalue %NxVal %t12121, 1
  %t12123 = extractvalue %NxVal %t12110, 1
  %t12124 = add i64 %t12123, %t12122
  %t12125 = add i64 65535, 0
  %t12126 = and i64 %t12124, %t12125
  %t12127 = call %NxVal @nx_int(i64 %t12126)
  store %NxVal %t12127, ptr @nx__g___main____acc59
  %t12128 = load %NxVal, ptr @nx__g___main____acc59
  %t12129 = add i64 86, 0
  %t12130 = extractvalue %NxVal %t12128, 1
  %t12131 = mul i64 %t12130, %t12129
  %t12132 = add i64 55, 0
  %t12133 = mul i64 %t12131, %t12132
  %t12134 = load %NxVal, ptr @nx__g___main____i59
  %t12135 = extractvalue %NxVal %t12134, 1
  %t12136 = add i64 %t12133, %t12135
  %t12137 = add i64 65535, 0
  %t12138 = and i64 %t12136, %t12137
  %t12139 = call %NxVal @nx_int(i64 %t12138)
  store %NxVal %t12139, ptr @nx__g___main____acc59
  %t12140 = load %NxVal, ptr @nx__g___main____acc59
  %t12141 = add i64 85, 0
  %t12142 = extractvalue %NxVal %t12140, 1
  %t12143 = call i64 @nx_mod_i64(i64 %t12142, i64 %t12141)
  %t12144 = add i64 20, 0
  %t12145 = call i64 @nx_mod_i64(i64 %t12143, i64 %t12144)
  %t12146 = load %NxVal, ptr @nx__g___main____i59
  %t12147 = extractvalue %NxVal %t12146, 1
  %t12148 = add i64 %t12145, %t12147
  %t12149 = add i64 65535, 0
  %t12150 = and i64 %t12148, %t12149
  %t12151 = call %NxVal @nx_int(i64 %t12150)
  store %NxVal %t12151, ptr @nx__g___main____acc59
  %t12152 = load %NxVal, ptr @nx__g___main____acc59
  %t12153 = add i64 97, 0
  %t12154 = extractvalue %NxVal %t12152, 1
  %t12155 = mul i64 %t12154, %t12153
  %t12156 = add i64 72, 0
  %t12157 = mul i64 %t12155, %t12156
  %t12158 = load %NxVal, ptr @nx__g___main____i59
  %t12159 = extractvalue %NxVal %t12158, 1
  %t12160 = add i64 %t12157, %t12159
  %t12161 = add i64 65535, 0
  %t12162 = and i64 %t12160, %t12161
  %t12163 = call %NxVal @nx_int(i64 %t12162)
  store %NxVal %t12163, ptr @nx__g___main____acc59
  %t12164 = load %NxVal, ptr @nx__g___main____acc59
  %t12165 = add i64 18, 0
  %t12166 = extractvalue %NxVal %t12164, 1
  %t12167 = mul i64 %t12166, %t12165
  %t12168 = add i64 87, 0
  %t12169 = mul i64 %t12167, %t12168
  %t12170 = load %NxVal, ptr @nx__g___main____i59
  %t12171 = extractvalue %NxVal %t12170, 1
  %t12172 = add i64 %t12169, %t12171
  %t12173 = add i64 65535, 0
  %t12174 = and i64 %t12172, %t12173
  %t12175 = call %NxVal @nx_int(i64 %t12174)
  store %NxVal %t12175, ptr @nx__g___main____acc59
  %t12176 = load %NxVal, ptr @nx__g___main____i59
  %t12177 = add i64 1, 0
  %t12178 = extractvalue %NxVal %t12176, 1
  %t12179 = add i64 %t12178, %t12177
  %t12180 = call %NxVal @nx_int(i64 %t12179)
  store %NxVal %t12180, ptr @nx__g___main____i59
  br label %wcond180
wend182:
  %t12181 = load %NxVal, ptr @nx__g___main____total
  %t12182 = load %NxVal, ptr @nx__g___main____acc59
  %t12183 = extractvalue %NxVal %t12181, 1
  %t12184 = extractvalue %NxVal %t12182, 1
  %t12185 = add i64 %t12183, %t12184
  %t12186 = add i64 65535, 0
  %t12187 = and i64 %t12185, %t12186
  %t12188 = call %NxVal @nx_int(i64 %t12187)
  store %NxVal %t12188, ptr @nx__g___main____total
  %t12189 = add i64 0, 0
  %t12190 = call %NxVal @nx_int(i64 %t12189)
  store %NxVal %t12190, ptr @nx__g___main____i60
  %t12191 = add i64 0, 0
  %t12192 = call %NxVal @nx_int(i64 %t12191)
  store %NxVal %t12192, ptr @nx__g___main____acc60
  br label %wcond183
wcond183:
  %t12193 = load %NxVal, ptr @nx__g___main____i60
  %t12194 = add i64 3, 0
  %t12195 = extractvalue %NxVal %t12193, 1
  %t12196 = icmp slt i64 %t12195, %t12194
  br i1 %t12196, label %wbody184, label %wend185
wbody184:
  %t12197 = load %NxVal, ptr @nx__g___main____acc60
  %t12198 = load %NxVal, ptr @nx__g___main____c
  %t12199 = load %NxVal, ptr @nx__g___main____i60
  %t12200 = add i64 60, 0
  %t12201 = extractvalue %NxVal %t12199, 1
  %t12202 = add i64 %t12201, %t12200
  %t12204 = getelementptr [2 x %NxVal], ptr %t12203, i64 0, i64 0
  store %NxVal %t12198, ptr %t12204
  %t12205 = call %NxVal @nx_int(i64 %t12202)
  %t12206 = getelementptr [2 x %NxVal], ptr %t12203, i64 0, i64 1
  store %NxVal %t12205, ptr %t12206
  %t12207 = getelementptr [2 x %NxVal], ptr %t12203, i64 0, i64 0
  %t12208 = call %NxVal @nx__m_3____main____Cell__m60(ptr %t12207, i64 2)
  %t12209 = extractvalue %NxVal %t12208, 1
  %t12210 = extractvalue %NxVal %t12197, 1
  %t12211 = add i64 %t12210, %t12209
  %t12212 = add i64 65535, 0
  %t12213 = and i64 %t12211, %t12212
  %t12214 = call %NxVal @nx_int(i64 %t12213)
  store %NxVal %t12214, ptr @nx__g___main____acc60
  %t12215 = load %NxVal, ptr @nx__g___main____acc60
  %t12216 = add i64 72, 0
  %t12217 = extractvalue %NxVal %t12215, 1
  %t12218 = or i64 %t12217, %t12216
  %t12219 = add i64 15, 0
  %t12220 = load %NxVal, ptr @nx__g___main____i60
  %t12221 = extractvalue %NxVal %t12220, 1
  %t12222 = add i64 %t12219, %t12221
  %t12223 = or i64 %t12218, %t12222
  %t12224 = add i64 65535, 0
  %t12225 = and i64 %t12223, %t12224
  %t12226 = call %NxVal @nx_int(i64 %t12225)
  store %NxVal %t12226, ptr @nx__g___main____acc60
  %t12227 = load %NxVal, ptr @nx__g___main____acc60
  %t12228 = add i64 56, 0
  %t12229 = extractvalue %NxVal %t12227, 1
  %t12230 = sub i64 %t12229, %t12228
  %t12231 = add i64 23, 0
  %t12232 = sub i64 %t12230, %t12231
  %t12233 = load %NxVal, ptr @nx__g___main____i60
  %t12234 = extractvalue %NxVal %t12233, 1
  %t12235 = add i64 %t12232, %t12234
  %t12236 = add i64 65535, 0
  %t12237 = and i64 %t12235, %t12236
  %t12238 = call %NxVal @nx_int(i64 %t12237)
  store %NxVal %t12238, ptr @nx__g___main____acc60
  %t12239 = load %NxVal, ptr @nx__g___main____acc60
  %t12240 = add i64 95, 0
  %t12241 = extractvalue %NxVal %t12239, 1
  %t12242 = mul i64 %t12241, %t12240
  %t12243 = add i64 38, 0
  %t12244 = mul i64 %t12242, %t12243
  %t12245 = load %NxVal, ptr @nx__g___main____i60
  %t12246 = extractvalue %NxVal %t12245, 1
  %t12247 = add i64 %t12244, %t12246
  %t12248 = add i64 65535, 0
  %t12249 = and i64 %t12247, %t12248
  %t12250 = call %NxVal @nx_int(i64 %t12249)
  store %NxVal %t12250, ptr @nx__g___main____acc60
  %t12251 = load %NxVal, ptr @nx__g___main____acc60
  %t12252 = add i64 87, 0
  %t12253 = extractvalue %NxVal %t12251, 1
  %t12254 = call i64 @nx_mod_i64(i64 %t12253, i64 %t12252)
  %t12255 = add i64 71, 0
  %t12256 = call i64 @nx_mod_i64(i64 %t12254, i64 %t12255)
  %t12257 = load %NxVal, ptr @nx__g___main____i60
  %t12258 = extractvalue %NxVal %t12257, 1
  %t12259 = add i64 %t12256, %t12258
  %t12260 = add i64 65535, 0
  %t12261 = and i64 %t12259, %t12260
  %t12262 = call %NxVal @nx_int(i64 %t12261)
  store %NxVal %t12262, ptr @nx__g___main____acc60
  %t12263 = load %NxVal, ptr @nx__g___main____i60
  %t12264 = add i64 1, 0
  %t12265 = extractvalue %NxVal %t12263, 1
  %t12266 = add i64 %t12265, %t12264
  %t12267 = call %NxVal @nx_int(i64 %t12266)
  store %NxVal %t12267, ptr @nx__g___main____i60
  br label %wcond183
wend185:
  %t12268 = load %NxVal, ptr @nx__g___main____total
  %t12269 = load %NxVal, ptr @nx__g___main____acc60
  %t12270 = extractvalue %NxVal %t12268, 1
  %t12271 = extractvalue %NxVal %t12269, 1
  %t12272 = add i64 %t12270, %t12271
  %t12273 = add i64 65535, 0
  %t12274 = and i64 %t12272, %t12273
  %t12275 = call %NxVal @nx_int(i64 %t12274)
  store %NxVal %t12275, ptr @nx__g___main____total
  %t12276 = add i64 0, 0
  %t12277 = call %NxVal @nx_int(i64 %t12276)
  store %NxVal %t12277, ptr @nx__g___main____i61
  %t12278 = add i64 0, 0
  %t12279 = call %NxVal @nx_int(i64 %t12278)
  store %NxVal %t12279, ptr @nx__g___main____acc61
  br label %wcond186
wcond186:
  %t12280 = load %NxVal, ptr @nx__g___main____i61
  %t12281 = add i64 3, 0
  %t12282 = extractvalue %NxVal %t12280, 1
  %t12283 = icmp slt i64 %t12282, %t12281
  br i1 %t12283, label %wbody187, label %wend188
wbody187:
  %t12284 = load %NxVal, ptr @nx__g___main____acc61
  %t12285 = load %NxVal, ptr @nx__g___main____c
  %t12286 = load %NxVal, ptr @nx__g___main____i61
  %t12287 = add i64 61, 0
  %t12288 = extractvalue %NxVal %t12286, 1
  %t12289 = add i64 %t12288, %t12287
  %t12291 = getelementptr [2 x %NxVal], ptr %t12290, i64 0, i64 0
  store %NxVal %t12285, ptr %t12291
  %t12292 = call %NxVal @nx_int(i64 %t12289)
  %t12293 = getelementptr [2 x %NxVal], ptr %t12290, i64 0, i64 1
  store %NxVal %t12292, ptr %t12293
  %t12294 = getelementptr [2 x %NxVal], ptr %t12290, i64 0, i64 0
  %t12295 = call %NxVal @nx__m_3____main____Cell__m61(ptr %t12294, i64 2)
  %t12296 = extractvalue %NxVal %t12295, 1
  %t12297 = extractvalue %NxVal %t12284, 1
  %t12298 = add i64 %t12297, %t12296
  %t12299 = add i64 65535, 0
  %t12300 = and i64 %t12298, %t12299
  %t12301 = call %NxVal @nx_int(i64 %t12300)
  store %NxVal %t12301, ptr @nx__g___main____acc61
  %t12302 = load %NxVal, ptr @nx__g___main____acc61
  %t12303 = add i64 27, 0
  %t12304 = extractvalue %NxVal %t12302, 1
  %t12305 = add i64 %t12304, %t12303
  %t12306 = add i64 3, 0
  %t12307 = add i64 %t12305, %t12306
  %t12308 = load %NxVal, ptr @nx__g___main____i61
  %t12309 = extractvalue %NxVal %t12308, 1
  %t12310 = add i64 %t12307, %t12309
  %t12311 = add i64 65535, 0
  %t12312 = and i64 %t12310, %t12311
  %t12313 = call %NxVal @nx_int(i64 %t12312)
  store %NxVal %t12313, ptr @nx__g___main____acc61
  %t12314 = load %NxVal, ptr @nx__g___main____acc61
  %t12315 = add i64 92, 0
  %t12316 = extractvalue %NxVal %t12314, 1
  %t12317 = call i64 @nx_mod_i64(i64 %t12316, i64 %t12315)
  %t12318 = add i64 41, 0
  %t12319 = call i64 @nx_mod_i64(i64 %t12317, i64 %t12318)
  %t12320 = load %NxVal, ptr @nx__g___main____i61
  %t12321 = extractvalue %NxVal %t12320, 1
  %t12322 = add i64 %t12319, %t12321
  %t12323 = add i64 65535, 0
  %t12324 = and i64 %t12322, %t12323
  %t12325 = call %NxVal @nx_int(i64 %t12324)
  store %NxVal %t12325, ptr @nx__g___main____acc61
  %t12326 = load %NxVal, ptr @nx__g___main____acc61
  %t12327 = add i64 29, 0
  %t12328 = extractvalue %NxVal %t12326, 1
  %t12329 = call i64 @nx_mod_i64(i64 %t12328, i64 %t12327)
  %t12330 = add i64 63, 0
  %t12331 = call i64 @nx_mod_i64(i64 %t12329, i64 %t12330)
  %t12332 = load %NxVal, ptr @nx__g___main____i61
  %t12333 = extractvalue %NxVal %t12332, 1
  %t12334 = add i64 %t12331, %t12333
  %t12335 = add i64 65535, 0
  %t12336 = and i64 %t12334, %t12335
  %t12337 = call %NxVal @nx_int(i64 %t12336)
  store %NxVal %t12337, ptr @nx__g___main____acc61
  %t12338 = load %NxVal, ptr @nx__g___main____acc61
  %t12339 = add i64 1, 0
  %t12340 = extractvalue %NxVal %t12338, 1
  %t12341 = mul i64 %t12340, %t12339
  %t12342 = add i64 32, 0
  %t12343 = mul i64 %t12341, %t12342
  %t12344 = load %NxVal, ptr @nx__g___main____i61
  %t12345 = extractvalue %NxVal %t12344, 1
  %t12346 = add i64 %t12343, %t12345
  %t12347 = add i64 65535, 0
  %t12348 = and i64 %t12346, %t12347
  %t12349 = call %NxVal @nx_int(i64 %t12348)
  store %NxVal %t12349, ptr @nx__g___main____acc61
  %t12350 = load %NxVal, ptr @nx__g___main____i61
  %t12351 = add i64 1, 0
  %t12352 = extractvalue %NxVal %t12350, 1
  %t12353 = add i64 %t12352, %t12351
  %t12354 = call %NxVal @nx_int(i64 %t12353)
  store %NxVal %t12354, ptr @nx__g___main____i61
  br label %wcond186
wend188:
  %t12355 = load %NxVal, ptr @nx__g___main____total
  %t12356 = load %NxVal, ptr @nx__g___main____acc61
  %t12357 = extractvalue %NxVal %t12355, 1
  %t12358 = extractvalue %NxVal %t12356, 1
  %t12359 = add i64 %t12357, %t12358
  %t12360 = add i64 65535, 0
  %t12361 = and i64 %t12359, %t12360
  %t12362 = call %NxVal @nx_int(i64 %t12361)
  store %NxVal %t12362, ptr @nx__g___main____total
  %t12363 = add i64 0, 0
  %t12364 = call %NxVal @nx_int(i64 %t12363)
  store %NxVal %t12364, ptr @nx__g___main____i62
  %t12365 = add i64 0, 0
  %t12366 = call %NxVal @nx_int(i64 %t12365)
  store %NxVal %t12366, ptr @nx__g___main____acc62
  br label %wcond189
wcond189:
  %t12367 = load %NxVal, ptr @nx__g___main____i62
  %t12368 = add i64 3, 0
  %t12369 = extractvalue %NxVal %t12367, 1
  %t12370 = icmp slt i64 %t12369, %t12368
  br i1 %t12370, label %wbody190, label %wend191
wbody190:
  %t12371 = load %NxVal, ptr @nx__g___main____acc62
  %t12372 = load %NxVal, ptr @nx__g___main____c
  %t12373 = load %NxVal, ptr @nx__g___main____i62
  %t12374 = add i64 62, 0
  %t12375 = extractvalue %NxVal %t12373, 1
  %t12376 = add i64 %t12375, %t12374
  %t12378 = getelementptr [2 x %NxVal], ptr %t12377, i64 0, i64 0
  store %NxVal %t12372, ptr %t12378
  %t12379 = call %NxVal @nx_int(i64 %t12376)
  %t12380 = getelementptr [2 x %NxVal], ptr %t12377, i64 0, i64 1
  store %NxVal %t12379, ptr %t12380
  %t12381 = getelementptr [2 x %NxVal], ptr %t12377, i64 0, i64 0
  %t12382 = call %NxVal @nx__m_3____main____Cell__m62(ptr %t12381, i64 2)
  %t12383 = extractvalue %NxVal %t12382, 1
  %t12384 = extractvalue %NxVal %t12371, 1
  %t12385 = add i64 %t12384, %t12383
  %t12386 = add i64 65535, 0
  %t12387 = and i64 %t12385, %t12386
  %t12388 = call %NxVal @nx_int(i64 %t12387)
  store %NxVal %t12388, ptr @nx__g___main____acc62
  %t12389 = load %NxVal, ptr @nx__g___main____acc62
  %t12390 = add i64 29, 0
  %t12391 = extractvalue %NxVal %t12389, 1
  %t12392 = or i64 %t12391, %t12390
  %t12393 = add i64 6, 0
  %t12394 = load %NxVal, ptr @nx__g___main____i62
  %t12395 = extractvalue %NxVal %t12394, 1
  %t12396 = add i64 %t12393, %t12395
  %t12397 = or i64 %t12392, %t12396
  %t12398 = add i64 65535, 0
  %t12399 = and i64 %t12397, %t12398
  %t12400 = call %NxVal @nx_int(i64 %t12399)
  store %NxVal %t12400, ptr @nx__g___main____acc62
  %t12401 = load %NxVal, ptr @nx__g___main____acc62
  %t12402 = add i64 57, 0
  %t12403 = extractvalue %NxVal %t12401, 1
  %t12404 = or i64 %t12403, %t12402
  %t12405 = add i64 28, 0
  %t12406 = load %NxVal, ptr @nx__g___main____i62
  %t12407 = extractvalue %NxVal %t12406, 1
  %t12408 = add i64 %t12405, %t12407
  %t12409 = or i64 %t12404, %t12408
  %t12410 = add i64 65535, 0
  %t12411 = and i64 %t12409, %t12410
  %t12412 = call %NxVal @nx_int(i64 %t12411)
  store %NxVal %t12412, ptr @nx__g___main____acc62
  %t12413 = load %NxVal, ptr @nx__g___main____acc62
  %t12414 = add i64 8, 0
  %t12415 = extractvalue %NxVal %t12413, 1
  %t12416 = and i64 %t12415, %t12414
  %t12417 = add i64 44, 0
  %t12418 = load %NxVal, ptr @nx__g___main____i62
  %t12419 = extractvalue %NxVal %t12418, 1
  %t12420 = add i64 %t12417, %t12419
  %t12421 = and i64 %t12416, %t12420
  %t12422 = add i64 65535, 0
  %t12423 = and i64 %t12421, %t12422
  %t12424 = call %NxVal @nx_int(i64 %t12423)
  store %NxVal %t12424, ptr @nx__g___main____acc62
  %t12425 = load %NxVal, ptr @nx__g___main____acc62
  %t12426 = add i64 88, 0
  %t12427 = extractvalue %NxVal %t12425, 1
  %t12428 = call i64 @nx_mod_i64(i64 %t12427, i64 %t12426)
  %t12429 = add i64 29, 0
  %t12430 = call i64 @nx_mod_i64(i64 %t12428, i64 %t12429)
  %t12431 = load %NxVal, ptr @nx__g___main____i62
  %t12432 = extractvalue %NxVal %t12431, 1
  %t12433 = add i64 %t12430, %t12432
  %t12434 = add i64 65535, 0
  %t12435 = and i64 %t12433, %t12434
  %t12436 = call %NxVal @nx_int(i64 %t12435)
  store %NxVal %t12436, ptr @nx__g___main____acc62
  %t12437 = load %NxVal, ptr @nx__g___main____i62
  %t12438 = add i64 1, 0
  %t12439 = extractvalue %NxVal %t12437, 1
  %t12440 = add i64 %t12439, %t12438
  %t12441 = call %NxVal @nx_int(i64 %t12440)
  store %NxVal %t12441, ptr @nx__g___main____i62
  br label %wcond189
wend191:
  %t12442 = load %NxVal, ptr @nx__g___main____total
  %t12443 = load %NxVal, ptr @nx__g___main____acc62
  %t12444 = extractvalue %NxVal %t12442, 1
  %t12445 = extractvalue %NxVal %t12443, 1
  %t12446 = add i64 %t12444, %t12445
  %t12447 = add i64 65535, 0
  %t12448 = and i64 %t12446, %t12447
  %t12449 = call %NxVal @nx_int(i64 %t12448)
  store %NxVal %t12449, ptr @nx__g___main____total
  %t12450 = add i64 0, 0
  %t12451 = call %NxVal @nx_int(i64 %t12450)
  store %NxVal %t12451, ptr @nx__g___main____i63
  %t12452 = add i64 0, 0
  %t12453 = call %NxVal @nx_int(i64 %t12452)
  store %NxVal %t12453, ptr @nx__g___main____acc63
  br label %wcond192
wcond192:
  %t12454 = load %NxVal, ptr @nx__g___main____i63
  %t12455 = add i64 3, 0
  %t12456 = extractvalue %NxVal %t12454, 1
  %t12457 = icmp slt i64 %t12456, %t12455
  br i1 %t12457, label %wbody193, label %wend194
wbody193:
  %t12458 = load %NxVal, ptr @nx__g___main____acc63
  %t12459 = load %NxVal, ptr @nx__g___main____c
  %t12460 = load %NxVal, ptr @nx__g___main____i63
  %t12461 = add i64 63, 0
  %t12462 = extractvalue %NxVal %t12460, 1
  %t12463 = add i64 %t12462, %t12461
  %t12465 = getelementptr [2 x %NxVal], ptr %t12464, i64 0, i64 0
  store %NxVal %t12459, ptr %t12465
  %t12466 = call %NxVal @nx_int(i64 %t12463)
  %t12467 = getelementptr [2 x %NxVal], ptr %t12464, i64 0, i64 1
  store %NxVal %t12466, ptr %t12467
  %t12468 = getelementptr [2 x %NxVal], ptr %t12464, i64 0, i64 0
  %t12469 = call %NxVal @nx__m_3____main____Cell__m63(ptr %t12468, i64 2)
  %t12470 = extractvalue %NxVal %t12469, 1
  %t12471 = extractvalue %NxVal %t12458, 1
  %t12472 = add i64 %t12471, %t12470
  %t12473 = add i64 65535, 0
  %t12474 = and i64 %t12472, %t12473
  %t12475 = call %NxVal @nx_int(i64 %t12474)
  store %NxVal %t12475, ptr @nx__g___main____acc63
  %t12476 = load %NxVal, ptr @nx__g___main____acc63
  %t12477 = add i64 59, 0
  %t12478 = extractvalue %NxVal %t12476, 1
  %t12479 = call i64 @nx_mod_i64(i64 %t12478, i64 %t12477)
  %t12480 = add i64 37, 0
  %t12481 = call i64 @nx_mod_i64(i64 %t12479, i64 %t12480)
  %t12482 = load %NxVal, ptr @nx__g___main____i63
  %t12483 = extractvalue %NxVal %t12482, 1
  %t12484 = add i64 %t12481, %t12483
  %t12485 = add i64 65535, 0
  %t12486 = and i64 %t12484, %t12485
  %t12487 = call %NxVal @nx_int(i64 %t12486)
  store %NxVal %t12487, ptr @nx__g___main____acc63
  %t12488 = load %NxVal, ptr @nx__g___main____acc63
  %t12489 = add i64 60, 0
  %t12490 = extractvalue %NxVal %t12488, 1
  %t12491 = xor i64 %t12490, %t12489
  %t12492 = add i64 24, 0
  %t12493 = load %NxVal, ptr @nx__g___main____i63
  %t12494 = extractvalue %NxVal %t12493, 1
  %t12495 = add i64 %t12492, %t12494
  %t12496 = xor i64 %t12491, %t12495
  %t12497 = add i64 65535, 0
  %t12498 = and i64 %t12496, %t12497
  %t12499 = call %NxVal @nx_int(i64 %t12498)
  store %NxVal %t12499, ptr @nx__g___main____acc63
  %t12500 = load %NxVal, ptr @nx__g___main____acc63
  %t12501 = add i64 42, 0
  %t12502 = extractvalue %NxVal %t12500, 1
  %t12503 = call i64 @nx_mod_i64(i64 %t12502, i64 %t12501)
  %t12504 = add i64 79, 0
  %t12505 = call i64 @nx_mod_i64(i64 %t12503, i64 %t12504)
  %t12506 = load %NxVal, ptr @nx__g___main____i63
  %t12507 = extractvalue %NxVal %t12506, 1
  %t12508 = add i64 %t12505, %t12507
  %t12509 = add i64 65535, 0
  %t12510 = and i64 %t12508, %t12509
  %t12511 = call %NxVal @nx_int(i64 %t12510)
  store %NxVal %t12511, ptr @nx__g___main____acc63
  %t12512 = load %NxVal, ptr @nx__g___main____acc63
  %t12513 = add i64 53, 0
  %t12514 = extractvalue %NxVal %t12512, 1
  %t12515 = add i64 %t12514, %t12513
  %t12516 = add i64 19, 0
  %t12517 = add i64 %t12515, %t12516
  %t12518 = load %NxVal, ptr @nx__g___main____i63
  %t12519 = extractvalue %NxVal %t12518, 1
  %t12520 = add i64 %t12517, %t12519
  %t12521 = add i64 65535, 0
  %t12522 = and i64 %t12520, %t12521
  %t12523 = call %NxVal @nx_int(i64 %t12522)
  store %NxVal %t12523, ptr @nx__g___main____acc63
  %t12524 = load %NxVal, ptr @nx__g___main____i63
  %t12525 = add i64 1, 0
  %t12526 = extractvalue %NxVal %t12524, 1
  %t12527 = add i64 %t12526, %t12525
  %t12528 = call %NxVal @nx_int(i64 %t12527)
  store %NxVal %t12528, ptr @nx__g___main____i63
  br label %wcond192
wend194:
  %t12529 = load %NxVal, ptr @nx__g___main____total
  %t12530 = load %NxVal, ptr @nx__g___main____acc63
  %t12531 = extractvalue %NxVal %t12529, 1
  %t12532 = extractvalue %NxVal %t12530, 1
  %t12533 = add i64 %t12531, %t12532
  %t12534 = add i64 65535, 0
  %t12535 = and i64 %t12533, %t12534
  %t12536 = call %NxVal @nx_int(i64 %t12535)
  store %NxVal %t12536, ptr @nx__g___main____total
  %t12537 = add i64 0, 0
  %t12538 = call %NxVal @nx_int(i64 %t12537)
  store %NxVal %t12538, ptr @nx__g___main____i64
  %t12539 = add i64 0, 0
  %t12540 = call %NxVal @nx_int(i64 %t12539)
  store %NxVal %t12540, ptr @nx__g___main____acc64
  br label %wcond195
wcond195:
  %t12541 = load %NxVal, ptr @nx__g___main____i64
  %t12542 = add i64 3, 0
  %t12543 = extractvalue %NxVal %t12541, 1
  %t12544 = icmp slt i64 %t12543, %t12542
  br i1 %t12544, label %wbody196, label %wend197
wbody196:
  %t12545 = load %NxVal, ptr @nx__g___main____acc64
  %t12546 = load %NxVal, ptr @nx__g___main____c
  %t12547 = load %NxVal, ptr @nx__g___main____i64
  %t12548 = add i64 64, 0
  %t12549 = extractvalue %NxVal %t12547, 1
  %t12550 = add i64 %t12549, %t12548
  %t12552 = getelementptr [2 x %NxVal], ptr %t12551, i64 0, i64 0
  store %NxVal %t12546, ptr %t12552
  %t12553 = call %NxVal @nx_int(i64 %t12550)
  %t12554 = getelementptr [2 x %NxVal], ptr %t12551, i64 0, i64 1
  store %NxVal %t12553, ptr %t12554
  %t12555 = getelementptr [2 x %NxVal], ptr %t12551, i64 0, i64 0
  %t12556 = call %NxVal @nx__m_3____main____Cell__m64(ptr %t12555, i64 2)
  %t12557 = extractvalue %NxVal %t12556, 1
  %t12558 = extractvalue %NxVal %t12545, 1
  %t12559 = add i64 %t12558, %t12557
  %t12560 = add i64 65535, 0
  %t12561 = and i64 %t12559, %t12560
  %t12562 = call %NxVal @nx_int(i64 %t12561)
  store %NxVal %t12562, ptr @nx__g___main____acc64
  %t12563 = load %NxVal, ptr @nx__g___main____acc64
  %t12564 = add i64 23, 0
  %t12565 = extractvalue %NxVal %t12563, 1
  %t12566 = call i64 @nx_mod_i64(i64 %t12565, i64 %t12564)
  %t12567 = add i64 4, 0
  %t12568 = call i64 @nx_mod_i64(i64 %t12566, i64 %t12567)
  %t12569 = load %NxVal, ptr @nx__g___main____i64
  %t12570 = extractvalue %NxVal %t12569, 1
  %t12571 = add i64 %t12568, %t12570
  %t12572 = add i64 65535, 0
  %t12573 = and i64 %t12571, %t12572
  %t12574 = call %NxVal @nx_int(i64 %t12573)
  store %NxVal %t12574, ptr @nx__g___main____acc64
  %t12575 = load %NxVal, ptr @nx__g___main____acc64
  %t12576 = add i64 44, 0
  %t12577 = extractvalue %NxVal %t12575, 1
  %t12578 = add i64 %t12577, %t12576
  %t12579 = add i64 35, 0
  %t12580 = add i64 %t12578, %t12579
  %t12581 = load %NxVal, ptr @nx__g___main____i64
  %t12582 = extractvalue %NxVal %t12581, 1
  %t12583 = add i64 %t12580, %t12582
  %t12584 = add i64 65535, 0
  %t12585 = and i64 %t12583, %t12584
  %t12586 = call %NxVal @nx_int(i64 %t12585)
  store %NxVal %t12586, ptr @nx__g___main____acc64
  %t12587 = load %NxVal, ptr @nx__g___main____acc64
  %t12588 = add i64 9, 0
  %t12589 = extractvalue %NxVal %t12587, 1
  %t12590 = sub i64 %t12589, %t12588
  %t12591 = add i64 54, 0
  %t12592 = sub i64 %t12590, %t12591
  %t12593 = load %NxVal, ptr @nx__g___main____i64
  %t12594 = extractvalue %NxVal %t12593, 1
  %t12595 = add i64 %t12592, %t12594
  %t12596 = add i64 65535, 0
  %t12597 = and i64 %t12595, %t12596
  %t12598 = call %NxVal @nx_int(i64 %t12597)
  store %NxVal %t12598, ptr @nx__g___main____acc64
  %t12599 = load %NxVal, ptr @nx__g___main____acc64
  %t12600 = add i64 76, 0
  %t12601 = extractvalue %NxVal %t12599, 1
  %t12602 = call i64 @nx_mod_i64(i64 %t12601, i64 %t12600)
  %t12603 = add i64 18, 0
  %t12604 = call i64 @nx_mod_i64(i64 %t12602, i64 %t12603)
  %t12605 = load %NxVal, ptr @nx__g___main____i64
  %t12606 = extractvalue %NxVal %t12605, 1
  %t12607 = add i64 %t12604, %t12606
  %t12608 = add i64 65535, 0
  %t12609 = and i64 %t12607, %t12608
  %t12610 = call %NxVal @nx_int(i64 %t12609)
  store %NxVal %t12610, ptr @nx__g___main____acc64
  %t12611 = load %NxVal, ptr @nx__g___main____i64
  %t12612 = add i64 1, 0
  %t12613 = extractvalue %NxVal %t12611, 1
  %t12614 = add i64 %t12613, %t12612
  %t12615 = call %NxVal @nx_int(i64 %t12614)
  store %NxVal %t12615, ptr @nx__g___main____i64
  br label %wcond195
wend197:
  %t12616 = load %NxVal, ptr @nx__g___main____total
  %t12617 = load %NxVal, ptr @nx__g___main____acc64
  %t12618 = extractvalue %NxVal %t12616, 1
  %t12619 = extractvalue %NxVal %t12617, 1
  %t12620 = add i64 %t12618, %t12619
  %t12621 = add i64 65535, 0
  %t12622 = and i64 %t12620, %t12621
  %t12623 = call %NxVal @nx_int(i64 %t12622)
  store %NxVal %t12623, ptr @nx__g___main____total
  %t12624 = add i64 0, 0
  %t12625 = call %NxVal @nx_int(i64 %t12624)
  store %NxVal %t12625, ptr @nx__g___main____i65
  %t12626 = add i64 0, 0
  %t12627 = call %NxVal @nx_int(i64 %t12626)
  store %NxVal %t12627, ptr @nx__g___main____acc65
  br label %wcond198
wcond198:
  %t12628 = load %NxVal, ptr @nx__g___main____i65
  %t12629 = add i64 3, 0
  %t12630 = extractvalue %NxVal %t12628, 1
  %t12631 = icmp slt i64 %t12630, %t12629
  br i1 %t12631, label %wbody199, label %wend200
wbody199:
  %t12632 = load %NxVal, ptr @nx__g___main____acc65
  %t12633 = load %NxVal, ptr @nx__g___main____c
  %t12634 = load %NxVal, ptr @nx__g___main____i65
  %t12635 = add i64 65, 0
  %t12636 = extractvalue %NxVal %t12634, 1
  %t12637 = add i64 %t12636, %t12635
  %t12639 = getelementptr [2 x %NxVal], ptr %t12638, i64 0, i64 0
  store %NxVal %t12633, ptr %t12639
  %t12640 = call %NxVal @nx_int(i64 %t12637)
  %t12641 = getelementptr [2 x %NxVal], ptr %t12638, i64 0, i64 1
  store %NxVal %t12640, ptr %t12641
  %t12642 = getelementptr [2 x %NxVal], ptr %t12638, i64 0, i64 0
  %t12643 = call %NxVal @nx__m_3____main____Cell__m65(ptr %t12642, i64 2)
  %t12644 = extractvalue %NxVal %t12643, 1
  %t12645 = extractvalue %NxVal %t12632, 1
  %t12646 = add i64 %t12645, %t12644
  %t12647 = add i64 65535, 0
  %t12648 = and i64 %t12646, %t12647
  %t12649 = call %NxVal @nx_int(i64 %t12648)
  store %NxVal %t12649, ptr @nx__g___main____acc65
  %t12650 = load %NxVal, ptr @nx__g___main____acc65
  %t12651 = add i64 75, 0
  %t12652 = extractvalue %NxVal %t12650, 1
  %t12653 = add i64 %t12652, %t12651
  %t12654 = add i64 4, 0
  %t12655 = add i64 %t12653, %t12654
  %t12656 = load %NxVal, ptr @nx__g___main____i65
  %t12657 = extractvalue %NxVal %t12656, 1
  %t12658 = add i64 %t12655, %t12657
  %t12659 = add i64 65535, 0
  %t12660 = and i64 %t12658, %t12659
  %t12661 = call %NxVal @nx_int(i64 %t12660)
  store %NxVal %t12661, ptr @nx__g___main____acc65
  %t12662 = load %NxVal, ptr @nx__g___main____acc65
  %t12663 = add i64 19, 0
  %t12664 = extractvalue %NxVal %t12662, 1
  %t12665 = mul i64 %t12664, %t12663
  %t12666 = add i64 68, 0
  %t12667 = mul i64 %t12665, %t12666
  %t12668 = load %NxVal, ptr @nx__g___main____i65
  %t12669 = extractvalue %NxVal %t12668, 1
  %t12670 = add i64 %t12667, %t12669
  %t12671 = add i64 65535, 0
  %t12672 = and i64 %t12670, %t12671
  %t12673 = call %NxVal @nx_int(i64 %t12672)
  store %NxVal %t12673, ptr @nx__g___main____acc65
  %t12674 = load %NxVal, ptr @nx__g___main____acc65
  %t12675 = add i64 2, 0
  %t12676 = extractvalue %NxVal %t12674, 1
  %t12677 = call i64 @nx_mod_i64(i64 %t12676, i64 %t12675)
  %t12678 = add i64 68, 0
  %t12679 = call i64 @nx_mod_i64(i64 %t12677, i64 %t12678)
  %t12680 = load %NxVal, ptr @nx__g___main____i65
  %t12681 = extractvalue %NxVal %t12680, 1
  %t12682 = add i64 %t12679, %t12681
  %t12683 = add i64 65535, 0
  %t12684 = and i64 %t12682, %t12683
  %t12685 = call %NxVal @nx_int(i64 %t12684)
  store %NxVal %t12685, ptr @nx__g___main____acc65
  %t12686 = load %NxVal, ptr @nx__g___main____acc65
  %t12687 = add i64 63, 0
  %t12688 = extractvalue %NxVal %t12686, 1
  %t12689 = and i64 %t12688, %t12687
  %t12690 = add i64 66, 0
  %t12691 = load %NxVal, ptr @nx__g___main____i65
  %t12692 = extractvalue %NxVal %t12691, 1
  %t12693 = add i64 %t12690, %t12692
  %t12694 = and i64 %t12689, %t12693
  %t12695 = add i64 65535, 0
  %t12696 = and i64 %t12694, %t12695
  %t12697 = call %NxVal @nx_int(i64 %t12696)
  store %NxVal %t12697, ptr @nx__g___main____acc65
  %t12698 = load %NxVal, ptr @nx__g___main____i65
  %t12699 = add i64 1, 0
  %t12700 = extractvalue %NxVal %t12698, 1
  %t12701 = add i64 %t12700, %t12699
  %t12702 = call %NxVal @nx_int(i64 %t12701)
  store %NxVal %t12702, ptr @nx__g___main____i65
  br label %wcond198
wend200:
  %t12703 = load %NxVal, ptr @nx__g___main____total
  %t12704 = load %NxVal, ptr @nx__g___main____acc65
  %t12705 = extractvalue %NxVal %t12703, 1
  %t12706 = extractvalue %NxVal %t12704, 1
  %t12707 = add i64 %t12705, %t12706
  %t12708 = add i64 65535, 0
  %t12709 = and i64 %t12707, %t12708
  %t12710 = call %NxVal @nx_int(i64 %t12709)
  store %NxVal %t12710, ptr @nx__g___main____total
  %t12711 = add i64 0, 0
  %t12712 = call %NxVal @nx_int(i64 %t12711)
  store %NxVal %t12712, ptr @nx__g___main____i66
  %t12713 = add i64 0, 0
  %t12714 = call %NxVal @nx_int(i64 %t12713)
  store %NxVal %t12714, ptr @nx__g___main____acc66
  br label %wcond201
wcond201:
  %t12715 = load %NxVal, ptr @nx__g___main____i66
  %t12716 = add i64 3, 0
  %t12717 = extractvalue %NxVal %t12715, 1
  %t12718 = icmp slt i64 %t12717, %t12716
  br i1 %t12718, label %wbody202, label %wend203
wbody202:
  %t12719 = load %NxVal, ptr @nx__g___main____acc66
  %t12720 = load %NxVal, ptr @nx__g___main____c
  %t12721 = load %NxVal, ptr @nx__g___main____i66
  %t12722 = add i64 66, 0
  %t12723 = extractvalue %NxVal %t12721, 1
  %t12724 = add i64 %t12723, %t12722
  %t12726 = getelementptr [2 x %NxVal], ptr %t12725, i64 0, i64 0
  store %NxVal %t12720, ptr %t12726
  %t12727 = call %NxVal @nx_int(i64 %t12724)
  %t12728 = getelementptr [2 x %NxVal], ptr %t12725, i64 0, i64 1
  store %NxVal %t12727, ptr %t12728
  %t12729 = getelementptr [2 x %NxVal], ptr %t12725, i64 0, i64 0
  %t12730 = call %NxVal @nx__m_3____main____Cell__m66(ptr %t12729, i64 2)
  %t12731 = extractvalue %NxVal %t12730, 1
  %t12732 = extractvalue %NxVal %t12719, 1
  %t12733 = add i64 %t12732, %t12731
  %t12734 = add i64 65535, 0
  %t12735 = and i64 %t12733, %t12734
  %t12736 = call %NxVal @nx_int(i64 %t12735)
  store %NxVal %t12736, ptr @nx__g___main____acc66
  %t12737 = load %NxVal, ptr @nx__g___main____acc66
  %t12738 = add i64 66, 0
  %t12739 = extractvalue %NxVal %t12737, 1
  %t12740 = or i64 %t12739, %t12738
  %t12741 = add i64 31, 0
  %t12742 = load %NxVal, ptr @nx__g___main____i66
  %t12743 = extractvalue %NxVal %t12742, 1
  %t12744 = add i64 %t12741, %t12743
  %t12745 = or i64 %t12740, %t12744
  %t12746 = add i64 65535, 0
  %t12747 = and i64 %t12745, %t12746
  %t12748 = call %NxVal @nx_int(i64 %t12747)
  store %NxVal %t12748, ptr @nx__g___main____acc66
  %t12749 = load %NxVal, ptr @nx__g___main____acc66
  %t12750 = add i64 23, 0
  %t12751 = extractvalue %NxVal %t12749, 1
  %t12752 = call i64 @nx_mod_i64(i64 %t12751, i64 %t12750)
  %t12753 = add i64 43, 0
  %t12754 = call i64 @nx_mod_i64(i64 %t12752, i64 %t12753)
  %t12755 = load %NxVal, ptr @nx__g___main____i66
  %t12756 = extractvalue %NxVal %t12755, 1
  %t12757 = add i64 %t12754, %t12756
  %t12758 = add i64 65535, 0
  %t12759 = and i64 %t12757, %t12758
  %t12760 = call %NxVal @nx_int(i64 %t12759)
  store %NxVal %t12760, ptr @nx__g___main____acc66
  %t12761 = load %NxVal, ptr @nx__g___main____acc66
  %t12762 = add i64 88, 0
  %t12763 = extractvalue %NxVal %t12761, 1
  %t12764 = or i64 %t12763, %t12762
  %t12765 = add i64 84, 0
  %t12766 = load %NxVal, ptr @nx__g___main____i66
  %t12767 = extractvalue %NxVal %t12766, 1
  %t12768 = add i64 %t12765, %t12767
  %t12769 = or i64 %t12764, %t12768
  %t12770 = add i64 65535, 0
  %t12771 = and i64 %t12769, %t12770
  %t12772 = call %NxVal @nx_int(i64 %t12771)
  store %NxVal %t12772, ptr @nx__g___main____acc66
  %t12773 = load %NxVal, ptr @nx__g___main____acc66
  %t12774 = add i64 4, 0
  %t12775 = extractvalue %NxVal %t12773, 1
  %t12776 = add i64 %t12775, %t12774
  %t12777 = add i64 3, 0
  %t12778 = add i64 %t12776, %t12777
  %t12779 = load %NxVal, ptr @nx__g___main____i66
  %t12780 = extractvalue %NxVal %t12779, 1
  %t12781 = add i64 %t12778, %t12780
  %t12782 = add i64 65535, 0
  %t12783 = and i64 %t12781, %t12782
  %t12784 = call %NxVal @nx_int(i64 %t12783)
  store %NxVal %t12784, ptr @nx__g___main____acc66
  %t12785 = load %NxVal, ptr @nx__g___main____i66
  %t12786 = add i64 1, 0
  %t12787 = extractvalue %NxVal %t12785, 1
  %t12788 = add i64 %t12787, %t12786
  %t12789 = call %NxVal @nx_int(i64 %t12788)
  store %NxVal %t12789, ptr @nx__g___main____i66
  br label %wcond201
wend203:
  %t12790 = load %NxVal, ptr @nx__g___main____total
  %t12791 = load %NxVal, ptr @nx__g___main____acc66
  %t12792 = extractvalue %NxVal %t12790, 1
  %t12793 = extractvalue %NxVal %t12791, 1
  %t12794 = add i64 %t12792, %t12793
  %t12795 = add i64 65535, 0
  %t12796 = and i64 %t12794, %t12795
  %t12797 = call %NxVal @nx_int(i64 %t12796)
  store %NxVal %t12797, ptr @nx__g___main____total
  %t12798 = add i64 0, 0
  %t12799 = call %NxVal @nx_int(i64 %t12798)
  store %NxVal %t12799, ptr @nx__g___main____i67
  %t12800 = add i64 0, 0
  %t12801 = call %NxVal @nx_int(i64 %t12800)
  store %NxVal %t12801, ptr @nx__g___main____acc67
  br label %wcond204
wcond204:
  %t12802 = load %NxVal, ptr @nx__g___main____i67
  %t12803 = add i64 3, 0
  %t12804 = extractvalue %NxVal %t12802, 1
  %t12805 = icmp slt i64 %t12804, %t12803
  br i1 %t12805, label %wbody205, label %wend206
wbody205:
  %t12806 = load %NxVal, ptr @nx__g___main____acc67
  %t12807 = load %NxVal, ptr @nx__g___main____c
  %t12808 = load %NxVal, ptr @nx__g___main____i67
  %t12809 = add i64 67, 0
  %t12810 = extractvalue %NxVal %t12808, 1
  %t12811 = add i64 %t12810, %t12809
  %t12813 = getelementptr [2 x %NxVal], ptr %t12812, i64 0, i64 0
  store %NxVal %t12807, ptr %t12813
  %t12814 = call %NxVal @nx_int(i64 %t12811)
  %t12815 = getelementptr [2 x %NxVal], ptr %t12812, i64 0, i64 1
  store %NxVal %t12814, ptr %t12815
  %t12816 = getelementptr [2 x %NxVal], ptr %t12812, i64 0, i64 0
  %t12817 = call %NxVal @nx__m_3____main____Cell__m67(ptr %t12816, i64 2)
  %t12818 = extractvalue %NxVal %t12817, 1
  %t12819 = extractvalue %NxVal %t12806, 1
  %t12820 = add i64 %t12819, %t12818
  %t12821 = add i64 65535, 0
  %t12822 = and i64 %t12820, %t12821
  %t12823 = call %NxVal @nx_int(i64 %t12822)
  store %NxVal %t12823, ptr @nx__g___main____acc67
  %t12824 = load %NxVal, ptr @nx__g___main____acc67
  %t12825 = add i64 8, 0
  %t12826 = extractvalue %NxVal %t12824, 1
  %t12827 = or i64 %t12826, %t12825
  %t12828 = add i64 61, 0
  %t12829 = load %NxVal, ptr @nx__g___main____i67
  %t12830 = extractvalue %NxVal %t12829, 1
  %t12831 = add i64 %t12828, %t12830
  %t12832 = or i64 %t12827, %t12831
  %t12833 = add i64 65535, 0
  %t12834 = and i64 %t12832, %t12833
  %t12835 = call %NxVal @nx_int(i64 %t12834)
  store %NxVal %t12835, ptr @nx__g___main____acc67
  %t12836 = load %NxVal, ptr @nx__g___main____acc67
  %t12837 = add i64 28, 0
  %t12838 = extractvalue %NxVal %t12836, 1
  %t12839 = or i64 %t12838, %t12837
  %t12840 = add i64 32, 0
  %t12841 = load %NxVal, ptr @nx__g___main____i67
  %t12842 = extractvalue %NxVal %t12841, 1
  %t12843 = add i64 %t12840, %t12842
  %t12844 = or i64 %t12839, %t12843
  %t12845 = add i64 65535, 0
  %t12846 = and i64 %t12844, %t12845
  %t12847 = call %NxVal @nx_int(i64 %t12846)
  store %NxVal %t12847, ptr @nx__g___main____acc67
  %t12848 = load %NxVal, ptr @nx__g___main____acc67
  %t12849 = add i64 34, 0
  %t12850 = extractvalue %NxVal %t12848, 1
  %t12851 = sub i64 %t12850, %t12849
  %t12852 = add i64 67, 0
  %t12853 = sub i64 %t12851, %t12852
  %t12854 = load %NxVal, ptr @nx__g___main____i67
  %t12855 = extractvalue %NxVal %t12854, 1
  %t12856 = add i64 %t12853, %t12855
  %t12857 = add i64 65535, 0
  %t12858 = and i64 %t12856, %t12857
  %t12859 = call %NxVal @nx_int(i64 %t12858)
  store %NxVal %t12859, ptr @nx__g___main____acc67
  %t12860 = load %NxVal, ptr @nx__g___main____acc67
  %t12861 = add i64 19, 0
  %t12862 = extractvalue %NxVal %t12860, 1
  %t12863 = and i64 %t12862, %t12861
  %t12864 = add i64 9, 0
  %t12865 = load %NxVal, ptr @nx__g___main____i67
  %t12866 = extractvalue %NxVal %t12865, 1
  %t12867 = add i64 %t12864, %t12866
  %t12868 = and i64 %t12863, %t12867
  %t12869 = add i64 65535, 0
  %t12870 = and i64 %t12868, %t12869
  %t12871 = call %NxVal @nx_int(i64 %t12870)
  store %NxVal %t12871, ptr @nx__g___main____acc67
  %t12872 = load %NxVal, ptr @nx__g___main____i67
  %t12873 = add i64 1, 0
  %t12874 = extractvalue %NxVal %t12872, 1
  %t12875 = add i64 %t12874, %t12873
  %t12876 = call %NxVal @nx_int(i64 %t12875)
  store %NxVal %t12876, ptr @nx__g___main____i67
  br label %wcond204
wend206:
  %t12877 = load %NxVal, ptr @nx__g___main____total
  %t12878 = load %NxVal, ptr @nx__g___main____acc67
  %t12879 = extractvalue %NxVal %t12877, 1
  %t12880 = extractvalue %NxVal %t12878, 1
  %t12881 = add i64 %t12879, %t12880
  %t12882 = add i64 65535, 0
  %t12883 = and i64 %t12881, %t12882
  %t12884 = call %NxVal @nx_int(i64 %t12883)
  store %NxVal %t12884, ptr @nx__g___main____total
  %t12885 = add i64 0, 0
  %t12886 = call %NxVal @nx_int(i64 %t12885)
  store %NxVal %t12886, ptr @nx__g___main____i68
  %t12887 = add i64 0, 0
  %t12888 = call %NxVal @nx_int(i64 %t12887)
  store %NxVal %t12888, ptr @nx__g___main____acc68
  br label %wcond207
wcond207:
  %t12889 = load %NxVal, ptr @nx__g___main____i68
  %t12890 = add i64 3, 0
  %t12891 = extractvalue %NxVal %t12889, 1
  %t12892 = icmp slt i64 %t12891, %t12890
  br i1 %t12892, label %wbody208, label %wend209
wbody208:
  %t12893 = load %NxVal, ptr @nx__g___main____acc68
  %t12894 = load %NxVal, ptr @nx__g___main____c
  %t12895 = load %NxVal, ptr @nx__g___main____i68
  %t12896 = add i64 68, 0
  %t12897 = extractvalue %NxVal %t12895, 1
  %t12898 = add i64 %t12897, %t12896
  %t12900 = getelementptr [2 x %NxVal], ptr %t12899, i64 0, i64 0
  store %NxVal %t12894, ptr %t12900
  %t12901 = call %NxVal @nx_int(i64 %t12898)
  %t12902 = getelementptr [2 x %NxVal], ptr %t12899, i64 0, i64 1
  store %NxVal %t12901, ptr %t12902
  %t12903 = getelementptr [2 x %NxVal], ptr %t12899, i64 0, i64 0
  %t12904 = call %NxVal @nx__m_3____main____Cell__m68(ptr %t12903, i64 2)
  %t12905 = extractvalue %NxVal %t12904, 1
  %t12906 = extractvalue %NxVal %t12893, 1
  %t12907 = add i64 %t12906, %t12905
  %t12908 = add i64 65535, 0
  %t12909 = and i64 %t12907, %t12908
  %t12910 = call %NxVal @nx_int(i64 %t12909)
  store %NxVal %t12910, ptr @nx__g___main____acc68
  %t12911 = load %NxVal, ptr @nx__g___main____acc68
  %t12912 = add i64 71, 0
  %t12913 = extractvalue %NxVal %t12911, 1
  %t12914 = xor i64 %t12913, %t12912
  %t12915 = add i64 9, 0
  %t12916 = load %NxVal, ptr @nx__g___main____i68
  %t12917 = extractvalue %NxVal %t12916, 1
  %t12918 = add i64 %t12915, %t12917
  %t12919 = xor i64 %t12914, %t12918
  %t12920 = add i64 65535, 0
  %t12921 = and i64 %t12919, %t12920
  %t12922 = call %NxVal @nx_int(i64 %t12921)
  store %NxVal %t12922, ptr @nx__g___main____acc68
  %t12923 = load %NxVal, ptr @nx__g___main____acc68
  %t12924 = add i64 12, 0
  %t12925 = extractvalue %NxVal %t12923, 1
  %t12926 = call i64 @nx_mod_i64(i64 %t12925, i64 %t12924)
  %t12927 = add i64 84, 0
  %t12928 = call i64 @nx_mod_i64(i64 %t12926, i64 %t12927)
  %t12929 = load %NxVal, ptr @nx__g___main____i68
  %t12930 = extractvalue %NxVal %t12929, 1
  %t12931 = add i64 %t12928, %t12930
  %t12932 = add i64 65535, 0
  %t12933 = and i64 %t12931, %t12932
  %t12934 = call %NxVal @nx_int(i64 %t12933)
  store %NxVal %t12934, ptr @nx__g___main____acc68
  %t12935 = load %NxVal, ptr @nx__g___main____acc68
  %t12936 = add i64 81, 0
  %t12937 = extractvalue %NxVal %t12935, 1
  %t12938 = and i64 %t12937, %t12936
  %t12939 = add i64 51, 0
  %t12940 = load %NxVal, ptr @nx__g___main____i68
  %t12941 = extractvalue %NxVal %t12940, 1
  %t12942 = add i64 %t12939, %t12941
  %t12943 = and i64 %t12938, %t12942
  %t12944 = add i64 65535, 0
  %t12945 = and i64 %t12943, %t12944
  %t12946 = call %NxVal @nx_int(i64 %t12945)
  store %NxVal %t12946, ptr @nx__g___main____acc68
  %t12947 = load %NxVal, ptr @nx__g___main____acc68
  %t12948 = add i64 89, 0
  %t12949 = extractvalue %NxVal %t12947, 1
  %t12950 = and i64 %t12949, %t12948
  %t12951 = add i64 27, 0
  %t12952 = load %NxVal, ptr @nx__g___main____i68
  %t12953 = extractvalue %NxVal %t12952, 1
  %t12954 = add i64 %t12951, %t12953
  %t12955 = and i64 %t12950, %t12954
  %t12956 = add i64 65535, 0
  %t12957 = and i64 %t12955, %t12956
  %t12958 = call %NxVal @nx_int(i64 %t12957)
  store %NxVal %t12958, ptr @nx__g___main____acc68
  %t12959 = load %NxVal, ptr @nx__g___main____i68
  %t12960 = add i64 1, 0
  %t12961 = extractvalue %NxVal %t12959, 1
  %t12962 = add i64 %t12961, %t12960
  %t12963 = call %NxVal @nx_int(i64 %t12962)
  store %NxVal %t12963, ptr @nx__g___main____i68
  br label %wcond207
wend209:
  %t12964 = load %NxVal, ptr @nx__g___main____total
  %t12965 = load %NxVal, ptr @nx__g___main____acc68
  %t12966 = extractvalue %NxVal %t12964, 1
  %t12967 = extractvalue %NxVal %t12965, 1
  %t12968 = add i64 %t12966, %t12967
  %t12969 = add i64 65535, 0
  %t12970 = and i64 %t12968, %t12969
  %t12971 = call %NxVal @nx_int(i64 %t12970)
  store %NxVal %t12971, ptr @nx__g___main____total
  %t12972 = add i64 0, 0
  %t12973 = call %NxVal @nx_int(i64 %t12972)
  store %NxVal %t12973, ptr @nx__g___main____i69
  %t12974 = add i64 0, 0
  %t12975 = call %NxVal @nx_int(i64 %t12974)
  store %NxVal %t12975, ptr @nx__g___main____acc69
  br label %wcond210
wcond210:
  %t12976 = load %NxVal, ptr @nx__g___main____i69
  %t12977 = add i64 3, 0
  %t12978 = extractvalue %NxVal %t12976, 1
  %t12979 = icmp slt i64 %t12978, %t12977
  br i1 %t12979, label %wbody211, label %wend212
wbody211:
  %t12980 = load %NxVal, ptr @nx__g___main____acc69
  %t12981 = load %NxVal, ptr @nx__g___main____c
  %t12982 = load %NxVal, ptr @nx__g___main____i69
  %t12983 = add i64 69, 0
  %t12984 = extractvalue %NxVal %t12982, 1
  %t12985 = add i64 %t12984, %t12983
  %t12987 = getelementptr [2 x %NxVal], ptr %t12986, i64 0, i64 0
  store %NxVal %t12981, ptr %t12987
  %t12988 = call %NxVal @nx_int(i64 %t12985)
  %t12989 = getelementptr [2 x %NxVal], ptr %t12986, i64 0, i64 1
  store %NxVal %t12988, ptr %t12989
  %t12990 = getelementptr [2 x %NxVal], ptr %t12986, i64 0, i64 0
  %t12991 = call %NxVal @nx__m_3____main____Cell__m69(ptr %t12990, i64 2)
  %t12992 = extractvalue %NxVal %t12991, 1
  %t12993 = extractvalue %NxVal %t12980, 1
  %t12994 = add i64 %t12993, %t12992
  %t12995 = add i64 65535, 0
  %t12996 = and i64 %t12994, %t12995
  %t12997 = call %NxVal @nx_int(i64 %t12996)
  store %NxVal %t12997, ptr @nx__g___main____acc69
  %t12998 = load %NxVal, ptr @nx__g___main____acc69
  %t12999 = add i64 85, 0
  %t13000 = extractvalue %NxVal %t12998, 1
  %t13001 = and i64 %t13000, %t12999
  %t13002 = add i64 28, 0
  %t13003 = load %NxVal, ptr @nx__g___main____i69
  %t13004 = extractvalue %NxVal %t13003, 1
  %t13005 = add i64 %t13002, %t13004
  %t13006 = and i64 %t13001, %t13005
  %t13007 = add i64 65535, 0
  %t13008 = and i64 %t13006, %t13007
  %t13009 = call %NxVal @nx_int(i64 %t13008)
  store %NxVal %t13009, ptr @nx__g___main____acc69
  %t13010 = load %NxVal, ptr @nx__g___main____acc69
  %t13011 = add i64 93, 0
  %t13012 = extractvalue %NxVal %t13010, 1
  %t13013 = add i64 %t13012, %t13011
  %t13014 = add i64 19, 0
  %t13015 = add i64 %t13013, %t13014
  %t13016 = load %NxVal, ptr @nx__g___main____i69
  %t13017 = extractvalue %NxVal %t13016, 1
  %t13018 = add i64 %t13015, %t13017
  %t13019 = add i64 65535, 0
  %t13020 = and i64 %t13018, %t13019
  %t13021 = call %NxVal @nx_int(i64 %t13020)
  store %NxVal %t13021, ptr @nx__g___main____acc69
  %t13022 = load %NxVal, ptr @nx__g___main____acc69
  %t13023 = add i64 57, 0
  %t13024 = extractvalue %NxVal %t13022, 1
  %t13025 = add i64 %t13024, %t13023
  %t13026 = add i64 58, 0
  %t13027 = add i64 %t13025, %t13026
  %t13028 = load %NxVal, ptr @nx__g___main____i69
  %t13029 = extractvalue %NxVal %t13028, 1
  %t13030 = add i64 %t13027, %t13029
  %t13031 = add i64 65535, 0
  %t13032 = and i64 %t13030, %t13031
  %t13033 = call %NxVal @nx_int(i64 %t13032)
  store %NxVal %t13033, ptr @nx__g___main____acc69
  %t13034 = load %NxVal, ptr @nx__g___main____acc69
  %t13035 = add i64 9, 0
  %t13036 = extractvalue %NxVal %t13034, 1
  %t13037 = and i64 %t13036, %t13035
  %t13038 = add i64 79, 0
  %t13039 = load %NxVal, ptr @nx__g___main____i69
  %t13040 = extractvalue %NxVal %t13039, 1
  %t13041 = add i64 %t13038, %t13040
  %t13042 = and i64 %t13037, %t13041
  %t13043 = add i64 65535, 0
  %t13044 = and i64 %t13042, %t13043
  %t13045 = call %NxVal @nx_int(i64 %t13044)
  store %NxVal %t13045, ptr @nx__g___main____acc69
  %t13046 = load %NxVal, ptr @nx__g___main____i69
  %t13047 = add i64 1, 0
  %t13048 = extractvalue %NxVal %t13046, 1
  %t13049 = add i64 %t13048, %t13047
  %t13050 = call %NxVal @nx_int(i64 %t13049)
  store %NxVal %t13050, ptr @nx__g___main____i69
  br label %wcond210
wend212:
  %t13051 = load %NxVal, ptr @nx__g___main____total
  %t13052 = load %NxVal, ptr @nx__g___main____acc69
  %t13053 = extractvalue %NxVal %t13051, 1
  %t13054 = extractvalue %NxVal %t13052, 1
  %t13055 = add i64 %t13053, %t13054
  %t13056 = add i64 65535, 0
  %t13057 = and i64 %t13055, %t13056
  %t13058 = call %NxVal @nx_int(i64 %t13057)
  store %NxVal %t13058, ptr @nx__g___main____total
  %t13059 = add i64 0, 0
  %t13060 = call %NxVal @nx_int(i64 %t13059)
  store %NxVal %t13060, ptr @nx__g___main____i70
  %t13061 = add i64 0, 0
  %t13062 = call %NxVal @nx_int(i64 %t13061)
  store %NxVal %t13062, ptr @nx__g___main____acc70
  br label %wcond213
wcond213:
  %t13063 = load %NxVal, ptr @nx__g___main____i70
  %t13064 = add i64 3, 0
  %t13065 = extractvalue %NxVal %t13063, 1
  %t13066 = icmp slt i64 %t13065, %t13064
  br i1 %t13066, label %wbody214, label %wend215
wbody214:
  %t13067 = load %NxVal, ptr @nx__g___main____acc70
  %t13068 = load %NxVal, ptr @nx__g___main____c
  %t13069 = load %NxVal, ptr @nx__g___main____i70
  %t13070 = add i64 70, 0
  %t13071 = extractvalue %NxVal %t13069, 1
  %t13072 = add i64 %t13071, %t13070
  %t13074 = getelementptr [2 x %NxVal], ptr %t13073, i64 0, i64 0
  store %NxVal %t13068, ptr %t13074
  %t13075 = call %NxVal @nx_int(i64 %t13072)
  %t13076 = getelementptr [2 x %NxVal], ptr %t13073, i64 0, i64 1
  store %NxVal %t13075, ptr %t13076
  %t13077 = getelementptr [2 x %NxVal], ptr %t13073, i64 0, i64 0
  %t13078 = call %NxVal @nx__m_3____main____Cell__m70(ptr %t13077, i64 2)
  %t13079 = extractvalue %NxVal %t13078, 1
  %t13080 = extractvalue %NxVal %t13067, 1
  %t13081 = add i64 %t13080, %t13079
  %t13082 = add i64 65535, 0
  %t13083 = and i64 %t13081, %t13082
  %t13084 = call %NxVal @nx_int(i64 %t13083)
  store %NxVal %t13084, ptr @nx__g___main____acc70
  %t13085 = load %NxVal, ptr @nx__g___main____acc70
  %t13086 = add i64 37, 0
  %t13087 = extractvalue %NxVal %t13085, 1
  %t13088 = xor i64 %t13087, %t13086
  %t13089 = add i64 82, 0
  %t13090 = load %NxVal, ptr @nx__g___main____i70
  %t13091 = extractvalue %NxVal %t13090, 1
  %t13092 = add i64 %t13089, %t13091
  %t13093 = xor i64 %t13088, %t13092
  %t13094 = add i64 65535, 0
  %t13095 = and i64 %t13093, %t13094
  %t13096 = call %NxVal @nx_int(i64 %t13095)
  store %NxVal %t13096, ptr @nx__g___main____acc70
  %t13097 = load %NxVal, ptr @nx__g___main____acc70
  %t13098 = add i64 25, 0
  %t13099 = extractvalue %NxVal %t13097, 1
  %t13100 = xor i64 %t13099, %t13098
  %t13101 = add i64 42, 0
  %t13102 = load %NxVal, ptr @nx__g___main____i70
  %t13103 = extractvalue %NxVal %t13102, 1
  %t13104 = add i64 %t13101, %t13103
  %t13105 = xor i64 %t13100, %t13104
  %t13106 = add i64 65535, 0
  %t13107 = and i64 %t13105, %t13106
  %t13108 = call %NxVal @nx_int(i64 %t13107)
  store %NxVal %t13108, ptr @nx__g___main____acc70
  %t13109 = load %NxVal, ptr @nx__g___main____acc70
  %t13110 = add i64 47, 0
  %t13111 = extractvalue %NxVal %t13109, 1
  %t13112 = xor i64 %t13111, %t13110
  %t13113 = add i64 6, 0
  %t13114 = load %NxVal, ptr @nx__g___main____i70
  %t13115 = extractvalue %NxVal %t13114, 1
  %t13116 = add i64 %t13113, %t13115
  %t13117 = xor i64 %t13112, %t13116
  %t13118 = add i64 65535, 0
  %t13119 = and i64 %t13117, %t13118
  %t13120 = call %NxVal @nx_int(i64 %t13119)
  store %NxVal %t13120, ptr @nx__g___main____acc70
  %t13121 = load %NxVal, ptr @nx__g___main____acc70
  %t13122 = add i64 30, 0
  %t13123 = extractvalue %NxVal %t13121, 1
  %t13124 = call i64 @nx_mod_i64(i64 %t13123, i64 %t13122)
  %t13125 = add i64 30, 0
  %t13126 = call i64 @nx_mod_i64(i64 %t13124, i64 %t13125)
  %t13127 = load %NxVal, ptr @nx__g___main____i70
  %t13128 = extractvalue %NxVal %t13127, 1
  %t13129 = add i64 %t13126, %t13128
  %t13130 = add i64 65535, 0
  %t13131 = and i64 %t13129, %t13130
  %t13132 = call %NxVal @nx_int(i64 %t13131)
  store %NxVal %t13132, ptr @nx__g___main____acc70
  %t13133 = load %NxVal, ptr @nx__g___main____i70
  %t13134 = add i64 1, 0
  %t13135 = extractvalue %NxVal %t13133, 1
  %t13136 = add i64 %t13135, %t13134
  %t13137 = call %NxVal @nx_int(i64 %t13136)
  store %NxVal %t13137, ptr @nx__g___main____i70
  br label %wcond213
wend215:
  %t13138 = load %NxVal, ptr @nx__g___main____total
  %t13139 = load %NxVal, ptr @nx__g___main____acc70
  %t13140 = extractvalue %NxVal %t13138, 1
  %t13141 = extractvalue %NxVal %t13139, 1
  %t13142 = add i64 %t13140, %t13141
  %t13143 = add i64 65535, 0
  %t13144 = and i64 %t13142, %t13143
  %t13145 = call %NxVal @nx_int(i64 %t13144)
  store %NxVal %t13145, ptr @nx__g___main____total
  %t13146 = add i64 0, 0
  %t13147 = call %NxVal @nx_int(i64 %t13146)
  store %NxVal %t13147, ptr @nx__g___main____i71
  %t13148 = add i64 0, 0
  %t13149 = call %NxVal @nx_int(i64 %t13148)
  store %NxVal %t13149, ptr @nx__g___main____acc71
  br label %wcond216
wcond216:
  %t13150 = load %NxVal, ptr @nx__g___main____i71
  %t13151 = add i64 3, 0
  %t13152 = extractvalue %NxVal %t13150, 1
  %t13153 = icmp slt i64 %t13152, %t13151
  br i1 %t13153, label %wbody217, label %wend218
wbody217:
  %t13154 = load %NxVal, ptr @nx__g___main____acc71
  %t13155 = load %NxVal, ptr @nx__g___main____c
  %t13156 = load %NxVal, ptr @nx__g___main____i71
  %t13157 = add i64 71, 0
  %t13158 = extractvalue %NxVal %t13156, 1
  %t13159 = add i64 %t13158, %t13157
  %t13161 = getelementptr [2 x %NxVal], ptr %t13160, i64 0, i64 0
  store %NxVal %t13155, ptr %t13161
  %t13162 = call %NxVal @nx_int(i64 %t13159)
  %t13163 = getelementptr [2 x %NxVal], ptr %t13160, i64 0, i64 1
  store %NxVal %t13162, ptr %t13163
  %t13164 = getelementptr [2 x %NxVal], ptr %t13160, i64 0, i64 0
  %t13165 = call %NxVal @nx__m_3____main____Cell__m71(ptr %t13164, i64 2)
  %t13166 = extractvalue %NxVal %t13165, 1
  %t13167 = extractvalue %NxVal %t13154, 1
  %t13168 = add i64 %t13167, %t13166
  %t13169 = add i64 65535, 0
  %t13170 = and i64 %t13168, %t13169
  %t13171 = call %NxVal @nx_int(i64 %t13170)
  store %NxVal %t13171, ptr @nx__g___main____acc71
  %t13172 = load %NxVal, ptr @nx__g___main____acc71
  %t13173 = add i64 30, 0
  %t13174 = extractvalue %NxVal %t13172, 1
  %t13175 = xor i64 %t13174, %t13173
  %t13176 = add i64 79, 0
  %t13177 = load %NxVal, ptr @nx__g___main____i71
  %t13178 = extractvalue %NxVal %t13177, 1
  %t13179 = add i64 %t13176, %t13178
  %t13180 = xor i64 %t13175, %t13179
  %t13181 = add i64 65535, 0
  %t13182 = and i64 %t13180, %t13181
  %t13183 = call %NxVal @nx_int(i64 %t13182)
  store %NxVal %t13183, ptr @nx__g___main____acc71
  %t13184 = load %NxVal, ptr @nx__g___main____acc71
  %t13185 = add i64 27, 0
  %t13186 = extractvalue %NxVal %t13184, 1
  %t13187 = or i64 %t13186, %t13185
  %t13188 = add i64 72, 0
  %t13189 = load %NxVal, ptr @nx__g___main____i71
  %t13190 = extractvalue %NxVal %t13189, 1
  %t13191 = add i64 %t13188, %t13190
  %t13192 = or i64 %t13187, %t13191
  %t13193 = add i64 65535, 0
  %t13194 = and i64 %t13192, %t13193
  %t13195 = call %NxVal @nx_int(i64 %t13194)
  store %NxVal %t13195, ptr @nx__g___main____acc71
  %t13196 = load %NxVal, ptr @nx__g___main____acc71
  %t13197 = add i64 68, 0
  %t13198 = extractvalue %NxVal %t13196, 1
  %t13199 = sub i64 %t13198, %t13197
  %t13200 = add i64 34, 0
  %t13201 = sub i64 %t13199, %t13200
  %t13202 = load %NxVal, ptr @nx__g___main____i71
  %t13203 = extractvalue %NxVal %t13202, 1
  %t13204 = add i64 %t13201, %t13203
  %t13205 = add i64 65535, 0
  %t13206 = and i64 %t13204, %t13205
  %t13207 = call %NxVal @nx_int(i64 %t13206)
  store %NxVal %t13207, ptr @nx__g___main____acc71
  %t13208 = load %NxVal, ptr @nx__g___main____acc71
  %t13209 = add i64 17, 0
  %t13210 = extractvalue %NxVal %t13208, 1
  %t13211 = mul i64 %t13210, %t13209
  %t13212 = add i64 78, 0
  %t13213 = mul i64 %t13211, %t13212
  %t13214 = load %NxVal, ptr @nx__g___main____i71
  %t13215 = extractvalue %NxVal %t13214, 1
  %t13216 = add i64 %t13213, %t13215
  %t13217 = add i64 65535, 0
  %t13218 = and i64 %t13216, %t13217
  %t13219 = call %NxVal @nx_int(i64 %t13218)
  store %NxVal %t13219, ptr @nx__g___main____acc71
  %t13220 = load %NxVal, ptr @nx__g___main____i71
  %t13221 = add i64 1, 0
  %t13222 = extractvalue %NxVal %t13220, 1
  %t13223 = add i64 %t13222, %t13221
  %t13224 = call %NxVal @nx_int(i64 %t13223)
  store %NxVal %t13224, ptr @nx__g___main____i71
  br label %wcond216
wend218:
  %t13225 = load %NxVal, ptr @nx__g___main____total
  %t13226 = load %NxVal, ptr @nx__g___main____acc71
  %t13227 = extractvalue %NxVal %t13225, 1
  %t13228 = extractvalue %NxVal %t13226, 1
  %t13229 = add i64 %t13227, %t13228
  %t13230 = add i64 65535, 0
  %t13231 = and i64 %t13229, %t13230
  %t13232 = call %NxVal @nx_int(i64 %t13231)
  store %NxVal %t13232, ptr @nx__g___main____total
  %t13233 = add i64 0, 0
  %t13234 = call %NxVal @nx_int(i64 %t13233)
  store %NxVal %t13234, ptr @nx__g___main____i72
  %t13235 = add i64 0, 0
  %t13236 = call %NxVal @nx_int(i64 %t13235)
  store %NxVal %t13236, ptr @nx__g___main____acc72
  br label %wcond219
wcond219:
  %t13237 = load %NxVal, ptr @nx__g___main____i72
  %t13238 = add i64 3, 0
  %t13239 = extractvalue %NxVal %t13237, 1
  %t13240 = icmp slt i64 %t13239, %t13238
  br i1 %t13240, label %wbody220, label %wend221
wbody220:
  %t13241 = load %NxVal, ptr @nx__g___main____acc72
  %t13242 = load %NxVal, ptr @nx__g___main____c
  %t13243 = load %NxVal, ptr @nx__g___main____i72
  %t13244 = add i64 72, 0
  %t13245 = extractvalue %NxVal %t13243, 1
  %t13246 = add i64 %t13245, %t13244
  %t13248 = getelementptr [2 x %NxVal], ptr %t13247, i64 0, i64 0
  store %NxVal %t13242, ptr %t13248
  %t13249 = call %NxVal @nx_int(i64 %t13246)
  %t13250 = getelementptr [2 x %NxVal], ptr %t13247, i64 0, i64 1
  store %NxVal %t13249, ptr %t13250
  %t13251 = getelementptr [2 x %NxVal], ptr %t13247, i64 0, i64 0
  %t13252 = call %NxVal @nx__m_3____main____Cell__m72(ptr %t13251, i64 2)
  %t13253 = extractvalue %NxVal %t13252, 1
  %t13254 = extractvalue %NxVal %t13241, 1
  %t13255 = add i64 %t13254, %t13253
  %t13256 = add i64 65535, 0
  %t13257 = and i64 %t13255, %t13256
  %t13258 = call %NxVal @nx_int(i64 %t13257)
  store %NxVal %t13258, ptr @nx__g___main____acc72
  %t13259 = load %NxVal, ptr @nx__g___main____acc72
  %t13260 = add i64 18, 0
  %t13261 = extractvalue %NxVal %t13259, 1
  %t13262 = or i64 %t13261, %t13260
  %t13263 = add i64 8, 0
  %t13264 = load %NxVal, ptr @nx__g___main____i72
  %t13265 = extractvalue %NxVal %t13264, 1
  %t13266 = add i64 %t13263, %t13265
  %t13267 = or i64 %t13262, %t13266
  %t13268 = add i64 65535, 0
  %t13269 = and i64 %t13267, %t13268
  %t13270 = call %NxVal @nx_int(i64 %t13269)
  store %NxVal %t13270, ptr @nx__g___main____acc72
  %t13271 = load %NxVal, ptr @nx__g___main____acc72
  %t13272 = add i64 62, 0
  %t13273 = extractvalue %NxVal %t13271, 1
  %t13274 = xor i64 %t13273, %t13272
  %t13275 = add i64 41, 0
  %t13276 = load %NxVal, ptr @nx__g___main____i72
  %t13277 = extractvalue %NxVal %t13276, 1
  %t13278 = add i64 %t13275, %t13277
  %t13279 = xor i64 %t13274, %t13278
  %t13280 = add i64 65535, 0
  %t13281 = and i64 %t13279, %t13280
  %t13282 = call %NxVal @nx_int(i64 %t13281)
  store %NxVal %t13282, ptr @nx__g___main____acc72
  %t13283 = load %NxVal, ptr @nx__g___main____acc72
  %t13284 = add i64 17, 0
  %t13285 = extractvalue %NxVal %t13283, 1
  %t13286 = mul i64 %t13285, %t13284
  %t13287 = add i64 30, 0
  %t13288 = mul i64 %t13286, %t13287
  %t13289 = load %NxVal, ptr @nx__g___main____i72
  %t13290 = extractvalue %NxVal %t13289, 1
  %t13291 = add i64 %t13288, %t13290
  %t13292 = add i64 65535, 0
  %t13293 = and i64 %t13291, %t13292
  %t13294 = call %NxVal @nx_int(i64 %t13293)
  store %NxVal %t13294, ptr @nx__g___main____acc72
  %t13295 = load %NxVal, ptr @nx__g___main____acc72
  %t13296 = add i64 13, 0
  %t13297 = extractvalue %NxVal %t13295, 1
  %t13298 = call i64 @nx_mod_i64(i64 %t13297, i64 %t13296)
  %t13299 = add i64 59, 0
  %t13300 = call i64 @nx_mod_i64(i64 %t13298, i64 %t13299)
  %t13301 = load %NxVal, ptr @nx__g___main____i72
  %t13302 = extractvalue %NxVal %t13301, 1
  %t13303 = add i64 %t13300, %t13302
  %t13304 = add i64 65535, 0
  %t13305 = and i64 %t13303, %t13304
  %t13306 = call %NxVal @nx_int(i64 %t13305)
  store %NxVal %t13306, ptr @nx__g___main____acc72
  %t13307 = load %NxVal, ptr @nx__g___main____i72
  %t13308 = add i64 1, 0
  %t13309 = extractvalue %NxVal %t13307, 1
  %t13310 = add i64 %t13309, %t13308
  %t13311 = call %NxVal @nx_int(i64 %t13310)
  store %NxVal %t13311, ptr @nx__g___main____i72
  br label %wcond219
wend221:
  %t13312 = load %NxVal, ptr @nx__g___main____total
  %t13313 = load %NxVal, ptr @nx__g___main____acc72
  %t13314 = extractvalue %NxVal %t13312, 1
  %t13315 = extractvalue %NxVal %t13313, 1
  %t13316 = add i64 %t13314, %t13315
  %t13317 = add i64 65535, 0
  %t13318 = and i64 %t13316, %t13317
  %t13319 = call %NxVal @nx_int(i64 %t13318)
  store %NxVal %t13319, ptr @nx__g___main____total
  %t13320 = add i64 0, 0
  %t13321 = call %NxVal @nx_int(i64 %t13320)
  store %NxVal %t13321, ptr @nx__g___main____i73
  %t13322 = add i64 0, 0
  %t13323 = call %NxVal @nx_int(i64 %t13322)
  store %NxVal %t13323, ptr @nx__g___main____acc73
  br label %wcond222
wcond222:
  %t13324 = load %NxVal, ptr @nx__g___main____i73
  %t13325 = add i64 3, 0
  %t13326 = extractvalue %NxVal %t13324, 1
  %t13327 = icmp slt i64 %t13326, %t13325
  br i1 %t13327, label %wbody223, label %wend224
wbody223:
  %t13328 = load %NxVal, ptr @nx__g___main____acc73
  %t13329 = load %NxVal, ptr @nx__g___main____c
  %t13330 = load %NxVal, ptr @nx__g___main____i73
  %t13331 = add i64 73, 0
  %t13332 = extractvalue %NxVal %t13330, 1
  %t13333 = add i64 %t13332, %t13331
  %t13335 = getelementptr [2 x %NxVal], ptr %t13334, i64 0, i64 0
  store %NxVal %t13329, ptr %t13335
  %t13336 = call %NxVal @nx_int(i64 %t13333)
  %t13337 = getelementptr [2 x %NxVal], ptr %t13334, i64 0, i64 1
  store %NxVal %t13336, ptr %t13337
  %t13338 = getelementptr [2 x %NxVal], ptr %t13334, i64 0, i64 0
  %t13339 = call %NxVal @nx__m_3____main____Cell__m73(ptr %t13338, i64 2)
  %t13340 = extractvalue %NxVal %t13339, 1
  %t13341 = extractvalue %NxVal %t13328, 1
  %t13342 = add i64 %t13341, %t13340
  %t13343 = add i64 65535, 0
  %t13344 = and i64 %t13342, %t13343
  %t13345 = call %NxVal @nx_int(i64 %t13344)
  store %NxVal %t13345, ptr @nx__g___main____acc73
  %t13346 = load %NxVal, ptr @nx__g___main____acc73
  %t13347 = add i64 37, 0
  %t13348 = extractvalue %NxVal %t13346, 1
  %t13349 = mul i64 %t13348, %t13347
  %t13350 = add i64 54, 0
  %t13351 = mul i64 %t13349, %t13350
  %t13352 = load %NxVal, ptr @nx__g___main____i73
  %t13353 = extractvalue %NxVal %t13352, 1
  %t13354 = add i64 %t13351, %t13353
  %t13355 = add i64 65535, 0
  %t13356 = and i64 %t13354, %t13355
  %t13357 = call %NxVal @nx_int(i64 %t13356)
  store %NxVal %t13357, ptr @nx__g___main____acc73
  %t13358 = load %NxVal, ptr @nx__g___main____acc73
  %t13359 = add i64 86, 0
  %t13360 = extractvalue %NxVal %t13358, 1
  %t13361 = call i64 @nx_mod_i64(i64 %t13360, i64 %t13359)
  %t13362 = add i64 75, 0
  %t13363 = call i64 @nx_mod_i64(i64 %t13361, i64 %t13362)
  %t13364 = load %NxVal, ptr @nx__g___main____i73
  %t13365 = extractvalue %NxVal %t13364, 1
  %t13366 = add i64 %t13363, %t13365
  %t13367 = add i64 65535, 0
  %t13368 = and i64 %t13366, %t13367
  %t13369 = call %NxVal @nx_int(i64 %t13368)
  store %NxVal %t13369, ptr @nx__g___main____acc73
  %t13370 = load %NxVal, ptr @nx__g___main____acc73
  %t13371 = add i64 5, 0
  %t13372 = extractvalue %NxVal %t13370, 1
  %t13373 = mul i64 %t13372, %t13371
  %t13374 = add i64 85, 0
  %t13375 = mul i64 %t13373, %t13374
  %t13376 = load %NxVal, ptr @nx__g___main____i73
  %t13377 = extractvalue %NxVal %t13376, 1
  %t13378 = add i64 %t13375, %t13377
  %t13379 = add i64 65535, 0
  %t13380 = and i64 %t13378, %t13379
  %t13381 = call %NxVal @nx_int(i64 %t13380)
  store %NxVal %t13381, ptr @nx__g___main____acc73
  %t13382 = load %NxVal, ptr @nx__g___main____acc73
  %t13383 = add i64 94, 0
  %t13384 = extractvalue %NxVal %t13382, 1
  %t13385 = or i64 %t13384, %t13383
  %t13386 = add i64 75, 0
  %t13387 = load %NxVal, ptr @nx__g___main____i73
  %t13388 = extractvalue %NxVal %t13387, 1
  %t13389 = add i64 %t13386, %t13388
  %t13390 = or i64 %t13385, %t13389
  %t13391 = add i64 65535, 0
  %t13392 = and i64 %t13390, %t13391
  %t13393 = call %NxVal @nx_int(i64 %t13392)
  store %NxVal %t13393, ptr @nx__g___main____acc73
  %t13394 = load %NxVal, ptr @nx__g___main____i73
  %t13395 = add i64 1, 0
  %t13396 = extractvalue %NxVal %t13394, 1
  %t13397 = add i64 %t13396, %t13395
  %t13398 = call %NxVal @nx_int(i64 %t13397)
  store %NxVal %t13398, ptr @nx__g___main____i73
  br label %wcond222
wend224:
  %t13399 = load %NxVal, ptr @nx__g___main____total
  %t13400 = load %NxVal, ptr @nx__g___main____acc73
  %t13401 = extractvalue %NxVal %t13399, 1
  %t13402 = extractvalue %NxVal %t13400, 1
  %t13403 = add i64 %t13401, %t13402
  %t13404 = add i64 65535, 0
  %t13405 = and i64 %t13403, %t13404
  %t13406 = call %NxVal @nx_int(i64 %t13405)
  store %NxVal %t13406, ptr @nx__g___main____total
  %t13407 = add i64 0, 0
  %t13408 = call %NxVal @nx_int(i64 %t13407)
  store %NxVal %t13408, ptr @nx__g___main____i74
  %t13409 = add i64 0, 0
  %t13410 = call %NxVal @nx_int(i64 %t13409)
  store %NxVal %t13410, ptr @nx__g___main____acc74
  br label %wcond225
wcond225:
  %t13411 = load %NxVal, ptr @nx__g___main____i74
  %t13412 = add i64 3, 0
  %t13413 = extractvalue %NxVal %t13411, 1
  %t13414 = icmp slt i64 %t13413, %t13412
  br i1 %t13414, label %wbody226, label %wend227
wbody226:
  %t13415 = load %NxVal, ptr @nx__g___main____acc74
  %t13416 = load %NxVal, ptr @nx__g___main____c
  %t13417 = load %NxVal, ptr @nx__g___main____i74
  %t13418 = add i64 74, 0
  %t13419 = extractvalue %NxVal %t13417, 1
  %t13420 = add i64 %t13419, %t13418
  %t13422 = getelementptr [2 x %NxVal], ptr %t13421, i64 0, i64 0
  store %NxVal %t13416, ptr %t13422
  %t13423 = call %NxVal @nx_int(i64 %t13420)
  %t13424 = getelementptr [2 x %NxVal], ptr %t13421, i64 0, i64 1
  store %NxVal %t13423, ptr %t13424
  %t13425 = getelementptr [2 x %NxVal], ptr %t13421, i64 0, i64 0
  %t13426 = call %NxVal @nx__m_3____main____Cell__m74(ptr %t13425, i64 2)
  %t13427 = extractvalue %NxVal %t13426, 1
  %t13428 = extractvalue %NxVal %t13415, 1
  %t13429 = add i64 %t13428, %t13427
  %t13430 = add i64 65535, 0
  %t13431 = and i64 %t13429, %t13430
  %t13432 = call %NxVal @nx_int(i64 %t13431)
  store %NxVal %t13432, ptr @nx__g___main____acc74
  %t13433 = load %NxVal, ptr @nx__g___main____acc74
  %t13434 = add i64 10, 0
  %t13435 = extractvalue %NxVal %t13433, 1
  %t13436 = or i64 %t13435, %t13434
  %t13437 = add i64 59, 0
  %t13438 = load %NxVal, ptr @nx__g___main____i74
  %t13439 = extractvalue %NxVal %t13438, 1
  %t13440 = add i64 %t13437, %t13439
  %t13441 = or i64 %t13436, %t13440
  %t13442 = add i64 65535, 0
  %t13443 = and i64 %t13441, %t13442
  %t13444 = call %NxVal @nx_int(i64 %t13443)
  store %NxVal %t13444, ptr @nx__g___main____acc74
  %t13445 = load %NxVal, ptr @nx__g___main____acc74
  %t13446 = add i64 74, 0
  %t13447 = extractvalue %NxVal %t13445, 1
  %t13448 = call i64 @nx_mod_i64(i64 %t13447, i64 %t13446)
  %t13449 = add i64 32, 0
  %t13450 = call i64 @nx_mod_i64(i64 %t13448, i64 %t13449)
  %t13451 = load %NxVal, ptr @nx__g___main____i74
  %t13452 = extractvalue %NxVal %t13451, 1
  %t13453 = add i64 %t13450, %t13452
  %t13454 = add i64 65535, 0
  %t13455 = and i64 %t13453, %t13454
  %t13456 = call %NxVal @nx_int(i64 %t13455)
  store %NxVal %t13456, ptr @nx__g___main____acc74
  %t13457 = load %NxVal, ptr @nx__g___main____acc74
  %t13458 = add i64 70, 0
  %t13459 = extractvalue %NxVal %t13457, 1
  %t13460 = add i64 %t13459, %t13458
  %t13461 = add i64 62, 0
  %t13462 = add i64 %t13460, %t13461
  %t13463 = load %NxVal, ptr @nx__g___main____i74
  %t13464 = extractvalue %NxVal %t13463, 1
  %t13465 = add i64 %t13462, %t13464
  %t13466 = add i64 65535, 0
  %t13467 = and i64 %t13465, %t13466
  %t13468 = call %NxVal @nx_int(i64 %t13467)
  store %NxVal %t13468, ptr @nx__g___main____acc74
  %t13469 = load %NxVal, ptr @nx__g___main____acc74
  %t13470 = add i64 57, 0
  %t13471 = extractvalue %NxVal %t13469, 1
  %t13472 = or i64 %t13471, %t13470
  %t13473 = add i64 44, 0
  %t13474 = load %NxVal, ptr @nx__g___main____i74
  %t13475 = extractvalue %NxVal %t13474, 1
  %t13476 = add i64 %t13473, %t13475
  %t13477 = or i64 %t13472, %t13476
  %t13478 = add i64 65535, 0
  %t13479 = and i64 %t13477, %t13478
  %t13480 = call %NxVal @nx_int(i64 %t13479)
  store %NxVal %t13480, ptr @nx__g___main____acc74
  %t13481 = load %NxVal, ptr @nx__g___main____i74
  %t13482 = add i64 1, 0
  %t13483 = extractvalue %NxVal %t13481, 1
  %t13484 = add i64 %t13483, %t13482
  %t13485 = call %NxVal @nx_int(i64 %t13484)
  store %NxVal %t13485, ptr @nx__g___main____i74
  br label %wcond225
wend227:
  %t13486 = load %NxVal, ptr @nx__g___main____total
  %t13487 = load %NxVal, ptr @nx__g___main____acc74
  %t13488 = extractvalue %NxVal %t13486, 1
  %t13489 = extractvalue %NxVal %t13487, 1
  %t13490 = add i64 %t13488, %t13489
  %t13491 = add i64 65535, 0
  %t13492 = and i64 %t13490, %t13491
  %t13493 = call %NxVal @nx_int(i64 %t13492)
  store %NxVal %t13493, ptr @nx__g___main____total
  %t13494 = add i64 0, 0
  %t13495 = call %NxVal @nx_int(i64 %t13494)
  store %NxVal %t13495, ptr @nx__g___main____i75
  %t13496 = add i64 0, 0
  %t13497 = call %NxVal @nx_int(i64 %t13496)
  store %NxVal %t13497, ptr @nx__g___main____acc75
  br label %wcond228
wcond228:
  %t13498 = load %NxVal, ptr @nx__g___main____i75
  %t13499 = add i64 3, 0
  %t13500 = extractvalue %NxVal %t13498, 1
  %t13501 = icmp slt i64 %t13500, %t13499
  br i1 %t13501, label %wbody229, label %wend230
wbody229:
  %t13502 = load %NxVal, ptr @nx__g___main____acc75
  %t13503 = load %NxVal, ptr @nx__g___main____c
  %t13504 = load %NxVal, ptr @nx__g___main____i75
  %t13505 = add i64 75, 0
  %t13506 = extractvalue %NxVal %t13504, 1
  %t13507 = add i64 %t13506, %t13505
  %t13509 = getelementptr [2 x %NxVal], ptr %t13508, i64 0, i64 0
  store %NxVal %t13503, ptr %t13509
  %t13510 = call %NxVal @nx_int(i64 %t13507)
  %t13511 = getelementptr [2 x %NxVal], ptr %t13508, i64 0, i64 1
  store %NxVal %t13510, ptr %t13511
  %t13512 = getelementptr [2 x %NxVal], ptr %t13508, i64 0, i64 0
  %t13513 = call %NxVal @nx__m_3____main____Cell__m75(ptr %t13512, i64 2)
  %t13514 = extractvalue %NxVal %t13513, 1
  %t13515 = extractvalue %NxVal %t13502, 1
  %t13516 = add i64 %t13515, %t13514
  %t13517 = add i64 65535, 0
  %t13518 = and i64 %t13516, %t13517
  %t13519 = call %NxVal @nx_int(i64 %t13518)
  store %NxVal %t13519, ptr @nx__g___main____acc75
  %t13520 = load %NxVal, ptr @nx__g___main____acc75
  %t13521 = add i64 57, 0
  %t13522 = extractvalue %NxVal %t13520, 1
  %t13523 = call i64 @nx_mod_i64(i64 %t13522, i64 %t13521)
  %t13524 = add i64 27, 0
  %t13525 = call i64 @nx_mod_i64(i64 %t13523, i64 %t13524)
  %t13526 = load %NxVal, ptr @nx__g___main____i75
  %t13527 = extractvalue %NxVal %t13526, 1
  %t13528 = add i64 %t13525, %t13527
  %t13529 = add i64 65535, 0
  %t13530 = and i64 %t13528, %t13529
  %t13531 = call %NxVal @nx_int(i64 %t13530)
  store %NxVal %t13531, ptr @nx__g___main____acc75
  %t13532 = load %NxVal, ptr @nx__g___main____acc75
  %t13533 = add i64 93, 0
  %t13534 = extractvalue %NxVal %t13532, 1
  %t13535 = xor i64 %t13534, %t13533
  %t13536 = add i64 33, 0
  %t13537 = load %NxVal, ptr @nx__g___main____i75
  %t13538 = extractvalue %NxVal %t13537, 1
  %t13539 = add i64 %t13536, %t13538
  %t13540 = xor i64 %t13535, %t13539
  %t13541 = add i64 65535, 0
  %t13542 = and i64 %t13540, %t13541
  %t13543 = call %NxVal @nx_int(i64 %t13542)
  store %NxVal %t13543, ptr @nx__g___main____acc75
  %t13544 = load %NxVal, ptr @nx__g___main____acc75
  %t13545 = add i64 65, 0
  %t13546 = extractvalue %NxVal %t13544, 1
  %t13547 = add i64 %t13546, %t13545
  %t13548 = add i64 62, 0
  %t13549 = add i64 %t13547, %t13548
  %t13550 = load %NxVal, ptr @nx__g___main____i75
  %t13551 = extractvalue %NxVal %t13550, 1
  %t13552 = add i64 %t13549, %t13551
  %t13553 = add i64 65535, 0
  %t13554 = and i64 %t13552, %t13553
  %t13555 = call %NxVal @nx_int(i64 %t13554)
  store %NxVal %t13555, ptr @nx__g___main____acc75
  %t13556 = load %NxVal, ptr @nx__g___main____acc75
  %t13557 = add i64 97, 0
  %t13558 = extractvalue %NxVal %t13556, 1
  %t13559 = xor i64 %t13558, %t13557
  %t13560 = add i64 78, 0
  %t13561 = load %NxVal, ptr @nx__g___main____i75
  %t13562 = extractvalue %NxVal %t13561, 1
  %t13563 = add i64 %t13560, %t13562
  %t13564 = xor i64 %t13559, %t13563
  %t13565 = add i64 65535, 0
  %t13566 = and i64 %t13564, %t13565
  %t13567 = call %NxVal @nx_int(i64 %t13566)
  store %NxVal %t13567, ptr @nx__g___main____acc75
  %t13568 = load %NxVal, ptr @nx__g___main____i75
  %t13569 = add i64 1, 0
  %t13570 = extractvalue %NxVal %t13568, 1
  %t13571 = add i64 %t13570, %t13569
  %t13572 = call %NxVal @nx_int(i64 %t13571)
  store %NxVal %t13572, ptr @nx__g___main____i75
  br label %wcond228
wend230:
  %t13573 = load %NxVal, ptr @nx__g___main____total
  %t13574 = load %NxVal, ptr @nx__g___main____acc75
  %t13575 = extractvalue %NxVal %t13573, 1
  %t13576 = extractvalue %NxVal %t13574, 1
  %t13577 = add i64 %t13575, %t13576
  %t13578 = add i64 65535, 0
  %t13579 = and i64 %t13577, %t13578
  %t13580 = call %NxVal @nx_int(i64 %t13579)
  store %NxVal %t13580, ptr @nx__g___main____total
  %t13581 = add i64 0, 0
  %t13582 = call %NxVal @nx_int(i64 %t13581)
  store %NxVal %t13582, ptr @nx__g___main____i76
  %t13583 = add i64 0, 0
  %t13584 = call %NxVal @nx_int(i64 %t13583)
  store %NxVal %t13584, ptr @nx__g___main____acc76
  br label %wcond231
wcond231:
  %t13585 = load %NxVal, ptr @nx__g___main____i76
  %t13586 = add i64 3, 0
  %t13587 = extractvalue %NxVal %t13585, 1
  %t13588 = icmp slt i64 %t13587, %t13586
  br i1 %t13588, label %wbody232, label %wend233
wbody232:
  %t13589 = load %NxVal, ptr @nx__g___main____acc76
  %t13590 = load %NxVal, ptr @nx__g___main____c
  %t13591 = load %NxVal, ptr @nx__g___main____i76
  %t13592 = add i64 76, 0
  %t13593 = extractvalue %NxVal %t13591, 1
  %t13594 = add i64 %t13593, %t13592
  %t13596 = getelementptr [2 x %NxVal], ptr %t13595, i64 0, i64 0
  store %NxVal %t13590, ptr %t13596
  %t13597 = call %NxVal @nx_int(i64 %t13594)
  %t13598 = getelementptr [2 x %NxVal], ptr %t13595, i64 0, i64 1
  store %NxVal %t13597, ptr %t13598
  %t13599 = getelementptr [2 x %NxVal], ptr %t13595, i64 0, i64 0
  %t13600 = call %NxVal @nx__m_3____main____Cell__m76(ptr %t13599, i64 2)
  %t13601 = extractvalue %NxVal %t13600, 1
  %t13602 = extractvalue %NxVal %t13589, 1
  %t13603 = add i64 %t13602, %t13601
  %t13604 = add i64 65535, 0
  %t13605 = and i64 %t13603, %t13604
  %t13606 = call %NxVal @nx_int(i64 %t13605)
  store %NxVal %t13606, ptr @nx__g___main____acc76
  %t13607 = load %NxVal, ptr @nx__g___main____acc76
  %t13608 = add i64 44, 0
  %t13609 = extractvalue %NxVal %t13607, 1
  %t13610 = mul i64 %t13609, %t13608
  %t13611 = add i64 19, 0
  %t13612 = mul i64 %t13610, %t13611
  %t13613 = load %NxVal, ptr @nx__g___main____i76
  %t13614 = extractvalue %NxVal %t13613, 1
  %t13615 = add i64 %t13612, %t13614
  %t13616 = add i64 65535, 0
  %t13617 = and i64 %t13615, %t13616
  %t13618 = call %NxVal @nx_int(i64 %t13617)
  store %NxVal %t13618, ptr @nx__g___main____acc76
  %t13619 = load %NxVal, ptr @nx__g___main____acc76
  %t13620 = add i64 38, 0
  %t13621 = extractvalue %NxVal %t13619, 1
  %t13622 = add i64 %t13621, %t13620
  %t13623 = add i64 40, 0
  %t13624 = add i64 %t13622, %t13623
  %t13625 = load %NxVal, ptr @nx__g___main____i76
  %t13626 = extractvalue %NxVal %t13625, 1
  %t13627 = add i64 %t13624, %t13626
  %t13628 = add i64 65535, 0
  %t13629 = and i64 %t13627, %t13628
  %t13630 = call %NxVal @nx_int(i64 %t13629)
  store %NxVal %t13630, ptr @nx__g___main____acc76
  %t13631 = load %NxVal, ptr @nx__g___main____acc76
  %t13632 = add i64 70, 0
  %t13633 = extractvalue %NxVal %t13631, 1
  %t13634 = call i64 @nx_mod_i64(i64 %t13633, i64 %t13632)
  %t13635 = add i64 20, 0
  %t13636 = call i64 @nx_mod_i64(i64 %t13634, i64 %t13635)
  %t13637 = load %NxVal, ptr @nx__g___main____i76
  %t13638 = extractvalue %NxVal %t13637, 1
  %t13639 = add i64 %t13636, %t13638
  %t13640 = add i64 65535, 0
  %t13641 = and i64 %t13639, %t13640
  %t13642 = call %NxVal @nx_int(i64 %t13641)
  store %NxVal %t13642, ptr @nx__g___main____acc76
  %t13643 = load %NxVal, ptr @nx__g___main____acc76
  %t13644 = add i64 90, 0
  %t13645 = extractvalue %NxVal %t13643, 1
  %t13646 = add i64 %t13645, %t13644
  %t13647 = add i64 62, 0
  %t13648 = add i64 %t13646, %t13647
  %t13649 = load %NxVal, ptr @nx__g___main____i76
  %t13650 = extractvalue %NxVal %t13649, 1
  %t13651 = add i64 %t13648, %t13650
  %t13652 = add i64 65535, 0
  %t13653 = and i64 %t13651, %t13652
  %t13654 = call %NxVal @nx_int(i64 %t13653)
  store %NxVal %t13654, ptr @nx__g___main____acc76
  %t13655 = load %NxVal, ptr @nx__g___main____i76
  %t13656 = add i64 1, 0
  %t13657 = extractvalue %NxVal %t13655, 1
  %t13658 = add i64 %t13657, %t13656
  %t13659 = call %NxVal @nx_int(i64 %t13658)
  store %NxVal %t13659, ptr @nx__g___main____i76
  br label %wcond231
wend233:
  %t13660 = load %NxVal, ptr @nx__g___main____total
  %t13661 = load %NxVal, ptr @nx__g___main____acc76
  %t13662 = extractvalue %NxVal %t13660, 1
  %t13663 = extractvalue %NxVal %t13661, 1
  %t13664 = add i64 %t13662, %t13663
  %t13665 = add i64 65535, 0
  %t13666 = and i64 %t13664, %t13665
  %t13667 = call %NxVal @nx_int(i64 %t13666)
  store %NxVal %t13667, ptr @nx__g___main____total
  %t13668 = add i64 0, 0
  %t13669 = call %NxVal @nx_int(i64 %t13668)
  store %NxVal %t13669, ptr @nx__g___main____i77
  %t13670 = add i64 0, 0
  %t13671 = call %NxVal @nx_int(i64 %t13670)
  store %NxVal %t13671, ptr @nx__g___main____acc77
  br label %wcond234
wcond234:
  %t13672 = load %NxVal, ptr @nx__g___main____i77
  %t13673 = add i64 3, 0
  %t13674 = extractvalue %NxVal %t13672, 1
  %t13675 = icmp slt i64 %t13674, %t13673
  br i1 %t13675, label %wbody235, label %wend236
wbody235:
  %t13676 = load %NxVal, ptr @nx__g___main____acc77
  %t13677 = load %NxVal, ptr @nx__g___main____c
  %t13678 = load %NxVal, ptr @nx__g___main____i77
  %t13679 = add i64 77, 0
  %t13680 = extractvalue %NxVal %t13678, 1
  %t13681 = add i64 %t13680, %t13679
  %t13683 = getelementptr [2 x %NxVal], ptr %t13682, i64 0, i64 0
  store %NxVal %t13677, ptr %t13683
  %t13684 = call %NxVal @nx_int(i64 %t13681)
  %t13685 = getelementptr [2 x %NxVal], ptr %t13682, i64 0, i64 1
  store %NxVal %t13684, ptr %t13685
  %t13686 = getelementptr [2 x %NxVal], ptr %t13682, i64 0, i64 0
  %t13687 = call %NxVal @nx__m_3____main____Cell__m77(ptr %t13686, i64 2)
  %t13688 = extractvalue %NxVal %t13687, 1
  %t13689 = extractvalue %NxVal %t13676, 1
  %t13690 = add i64 %t13689, %t13688
  %t13691 = add i64 65535, 0
  %t13692 = and i64 %t13690, %t13691
  %t13693 = call %NxVal @nx_int(i64 %t13692)
  store %NxVal %t13693, ptr @nx__g___main____acc77
  %t13694 = load %NxVal, ptr @nx__g___main____acc77
  %t13695 = add i64 72, 0
  %t13696 = extractvalue %NxVal %t13694, 1
  %t13697 = or i64 %t13696, %t13695
  %t13698 = add i64 86, 0
  %t13699 = load %NxVal, ptr @nx__g___main____i77
  %t13700 = extractvalue %NxVal %t13699, 1
  %t13701 = add i64 %t13698, %t13700
  %t13702 = or i64 %t13697, %t13701
  %t13703 = add i64 65535, 0
  %t13704 = and i64 %t13702, %t13703
  %t13705 = call %NxVal @nx_int(i64 %t13704)
  store %NxVal %t13705, ptr @nx__g___main____acc77
  %t13706 = load %NxVal, ptr @nx__g___main____acc77
  %t13707 = add i64 55, 0
  %t13708 = extractvalue %NxVal %t13706, 1
  %t13709 = call i64 @nx_mod_i64(i64 %t13708, i64 %t13707)
  %t13710 = add i64 9, 0
  %t13711 = call i64 @nx_mod_i64(i64 %t13709, i64 %t13710)
  %t13712 = load %NxVal, ptr @nx__g___main____i77
  %t13713 = extractvalue %NxVal %t13712, 1
  %t13714 = add i64 %t13711, %t13713
  %t13715 = add i64 65535, 0
  %t13716 = and i64 %t13714, %t13715
  %t13717 = call %NxVal @nx_int(i64 %t13716)
  store %NxVal %t13717, ptr @nx__g___main____acc77
  %t13718 = load %NxVal, ptr @nx__g___main____acc77
  %t13719 = add i64 23, 0
  %t13720 = extractvalue %NxVal %t13718, 1
  %t13721 = or i64 %t13720, %t13719
  %t13722 = add i64 61, 0
  %t13723 = load %NxVal, ptr @nx__g___main____i77
  %t13724 = extractvalue %NxVal %t13723, 1
  %t13725 = add i64 %t13722, %t13724
  %t13726 = or i64 %t13721, %t13725
  %t13727 = add i64 65535, 0
  %t13728 = and i64 %t13726, %t13727
  %t13729 = call %NxVal @nx_int(i64 %t13728)
  store %NxVal %t13729, ptr @nx__g___main____acc77
  %t13730 = load %NxVal, ptr @nx__g___main____acc77
  %t13731 = add i64 96, 0
  %t13732 = extractvalue %NxVal %t13730, 1
  %t13733 = mul i64 %t13732, %t13731
  %t13734 = add i64 57, 0
  %t13735 = mul i64 %t13733, %t13734
  %t13736 = load %NxVal, ptr @nx__g___main____i77
  %t13737 = extractvalue %NxVal %t13736, 1
  %t13738 = add i64 %t13735, %t13737
  %t13739 = add i64 65535, 0
  %t13740 = and i64 %t13738, %t13739
  %t13741 = call %NxVal @nx_int(i64 %t13740)
  store %NxVal %t13741, ptr @nx__g___main____acc77
  %t13742 = load %NxVal, ptr @nx__g___main____i77
  %t13743 = add i64 1, 0
  %t13744 = extractvalue %NxVal %t13742, 1
  %t13745 = add i64 %t13744, %t13743
  %t13746 = call %NxVal @nx_int(i64 %t13745)
  store %NxVal %t13746, ptr @nx__g___main____i77
  br label %wcond234
wend236:
  %t13747 = load %NxVal, ptr @nx__g___main____total
  %t13748 = load %NxVal, ptr @nx__g___main____acc77
  %t13749 = extractvalue %NxVal %t13747, 1
  %t13750 = extractvalue %NxVal %t13748, 1
  %t13751 = add i64 %t13749, %t13750
  %t13752 = add i64 65535, 0
  %t13753 = and i64 %t13751, %t13752
  %t13754 = call %NxVal @nx_int(i64 %t13753)
  store %NxVal %t13754, ptr @nx__g___main____total
  %t13755 = add i64 0, 0
  %t13756 = call %NxVal @nx_int(i64 %t13755)
  store %NxVal %t13756, ptr @nx__g___main____i78
  %t13757 = add i64 0, 0
  %t13758 = call %NxVal @nx_int(i64 %t13757)
  store %NxVal %t13758, ptr @nx__g___main____acc78
  br label %wcond237
wcond237:
  %t13759 = load %NxVal, ptr @nx__g___main____i78
  %t13760 = add i64 3, 0
  %t13761 = extractvalue %NxVal %t13759, 1
  %t13762 = icmp slt i64 %t13761, %t13760
  br i1 %t13762, label %wbody238, label %wend239
wbody238:
  %t13763 = load %NxVal, ptr @nx__g___main____acc78
  %t13764 = load %NxVal, ptr @nx__g___main____c
  %t13765 = load %NxVal, ptr @nx__g___main____i78
  %t13766 = add i64 78, 0
  %t13767 = extractvalue %NxVal %t13765, 1
  %t13768 = add i64 %t13767, %t13766
  %t13770 = getelementptr [2 x %NxVal], ptr %t13769, i64 0, i64 0
  store %NxVal %t13764, ptr %t13770
  %t13771 = call %NxVal @nx_int(i64 %t13768)
  %t13772 = getelementptr [2 x %NxVal], ptr %t13769, i64 0, i64 1
  store %NxVal %t13771, ptr %t13772
  %t13773 = getelementptr [2 x %NxVal], ptr %t13769, i64 0, i64 0
  %t13774 = call %NxVal @nx__m_3____main____Cell__m78(ptr %t13773, i64 2)
  %t13775 = extractvalue %NxVal %t13774, 1
  %t13776 = extractvalue %NxVal %t13763, 1
  %t13777 = add i64 %t13776, %t13775
  %t13778 = add i64 65535, 0
  %t13779 = and i64 %t13777, %t13778
  %t13780 = call %NxVal @nx_int(i64 %t13779)
  store %NxVal %t13780, ptr @nx__g___main____acc78
  %t13781 = load %NxVal, ptr @nx__g___main____acc78
  %t13782 = add i64 66, 0
  %t13783 = extractvalue %NxVal %t13781, 1
  %t13784 = xor i64 %t13783, %t13782
  %t13785 = add i64 35, 0
  %t13786 = load %NxVal, ptr @nx__g___main____i78
  %t13787 = extractvalue %NxVal %t13786, 1
  %t13788 = add i64 %t13785, %t13787
  %t13789 = xor i64 %t13784, %t13788
  %t13790 = add i64 65535, 0
  %t13791 = and i64 %t13789, %t13790
  %t13792 = call %NxVal @nx_int(i64 %t13791)
  store %NxVal %t13792, ptr @nx__g___main____acc78
  %t13793 = load %NxVal, ptr @nx__g___main____acc78
  %t13794 = add i64 60, 0
  %t13795 = extractvalue %NxVal %t13793, 1
  %t13796 = or i64 %t13795, %t13794
  %t13797 = add i64 79, 0
  %t13798 = load %NxVal, ptr @nx__g___main____i78
  %t13799 = extractvalue %NxVal %t13798, 1
  %t13800 = add i64 %t13797, %t13799
  %t13801 = or i64 %t13796, %t13800
  %t13802 = add i64 65535, 0
  %t13803 = and i64 %t13801, %t13802
  %t13804 = call %NxVal @nx_int(i64 %t13803)
  store %NxVal %t13804, ptr @nx__g___main____acc78
  %t13805 = load %NxVal, ptr @nx__g___main____acc78
  %t13806 = add i64 65, 0
  %t13807 = extractvalue %NxVal %t13805, 1
  %t13808 = and i64 %t13807, %t13806
  %t13809 = add i64 27, 0
  %t13810 = load %NxVal, ptr @nx__g___main____i78
  %t13811 = extractvalue %NxVal %t13810, 1
  %t13812 = add i64 %t13809, %t13811
  %t13813 = and i64 %t13808, %t13812
  %t13814 = add i64 65535, 0
  %t13815 = and i64 %t13813, %t13814
  %t13816 = call %NxVal @nx_int(i64 %t13815)
  store %NxVal %t13816, ptr @nx__g___main____acc78
  %t13817 = load %NxVal, ptr @nx__g___main____acc78
  %t13818 = add i64 6, 0
  %t13819 = extractvalue %NxVal %t13817, 1
  %t13820 = or i64 %t13819, %t13818
  %t13821 = add i64 54, 0
  %t13822 = load %NxVal, ptr @nx__g___main____i78
  %t13823 = extractvalue %NxVal %t13822, 1
  %t13824 = add i64 %t13821, %t13823
  %t13825 = or i64 %t13820, %t13824
  %t13826 = add i64 65535, 0
  %t13827 = and i64 %t13825, %t13826
  %t13828 = call %NxVal @nx_int(i64 %t13827)
  store %NxVal %t13828, ptr @nx__g___main____acc78
  %t13829 = load %NxVal, ptr @nx__g___main____i78
  %t13830 = add i64 1, 0
  %t13831 = extractvalue %NxVal %t13829, 1
  %t13832 = add i64 %t13831, %t13830
  %t13833 = call %NxVal @nx_int(i64 %t13832)
  store %NxVal %t13833, ptr @nx__g___main____i78
  br label %wcond237
wend239:
  %t13834 = load %NxVal, ptr @nx__g___main____total
  %t13835 = load %NxVal, ptr @nx__g___main____acc78
  %t13836 = extractvalue %NxVal %t13834, 1
  %t13837 = extractvalue %NxVal %t13835, 1
  %t13838 = add i64 %t13836, %t13837
  %t13839 = add i64 65535, 0
  %t13840 = and i64 %t13838, %t13839
  %t13841 = call %NxVal @nx_int(i64 %t13840)
  store %NxVal %t13841, ptr @nx__g___main____total
  %t13842 = add i64 0, 0
  %t13843 = call %NxVal @nx_int(i64 %t13842)
  store %NxVal %t13843, ptr @nx__g___main____i79
  %t13844 = add i64 0, 0
  %t13845 = call %NxVal @nx_int(i64 %t13844)
  store %NxVal %t13845, ptr @nx__g___main____acc79
  br label %wcond240
wcond240:
  %t13846 = load %NxVal, ptr @nx__g___main____i79
  %t13847 = add i64 3, 0
  %t13848 = extractvalue %NxVal %t13846, 1
  %t13849 = icmp slt i64 %t13848, %t13847
  br i1 %t13849, label %wbody241, label %wend242
wbody241:
  %t13850 = load %NxVal, ptr @nx__g___main____acc79
  %t13851 = load %NxVal, ptr @nx__g___main____c
  %t13852 = load %NxVal, ptr @nx__g___main____i79
  %t13853 = add i64 79, 0
  %t13854 = extractvalue %NxVal %t13852, 1
  %t13855 = add i64 %t13854, %t13853
  %t13857 = getelementptr [2 x %NxVal], ptr %t13856, i64 0, i64 0
  store %NxVal %t13851, ptr %t13857
  %t13858 = call %NxVal @nx_int(i64 %t13855)
  %t13859 = getelementptr [2 x %NxVal], ptr %t13856, i64 0, i64 1
  store %NxVal %t13858, ptr %t13859
  %t13860 = getelementptr [2 x %NxVal], ptr %t13856, i64 0, i64 0
  %t13861 = call %NxVal @nx__m_3____main____Cell__m79(ptr %t13860, i64 2)
  %t13862 = extractvalue %NxVal %t13861, 1
  %t13863 = extractvalue %NxVal %t13850, 1
  %t13864 = add i64 %t13863, %t13862
  %t13865 = add i64 65535, 0
  %t13866 = and i64 %t13864, %t13865
  %t13867 = call %NxVal @nx_int(i64 %t13866)
  store %NxVal %t13867, ptr @nx__g___main____acc79
  %t13868 = load %NxVal, ptr @nx__g___main____acc79
  %t13869 = add i64 10, 0
  %t13870 = extractvalue %NxVal %t13868, 1
  %t13871 = or i64 %t13870, %t13869
  %t13872 = add i64 73, 0
  %t13873 = load %NxVal, ptr @nx__g___main____i79
  %t13874 = extractvalue %NxVal %t13873, 1
  %t13875 = add i64 %t13872, %t13874
  %t13876 = or i64 %t13871, %t13875
  %t13877 = add i64 65535, 0
  %t13878 = and i64 %t13876, %t13877
  %t13879 = call %NxVal @nx_int(i64 %t13878)
  store %NxVal %t13879, ptr @nx__g___main____acc79
  %t13880 = load %NxVal, ptr @nx__g___main____acc79
  %t13881 = add i64 54, 0
  %t13882 = extractvalue %NxVal %t13880, 1
  %t13883 = or i64 %t13882, %t13881
  %t13884 = add i64 3, 0
  %t13885 = load %NxVal, ptr @nx__g___main____i79
  %t13886 = extractvalue %NxVal %t13885, 1
  %t13887 = add i64 %t13884, %t13886
  %t13888 = or i64 %t13883, %t13887
  %t13889 = add i64 65535, 0
  %t13890 = and i64 %t13888, %t13889
  %t13891 = call %NxVal @nx_int(i64 %t13890)
  store %NxVal %t13891, ptr @nx__g___main____acc79
  %t13892 = load %NxVal, ptr @nx__g___main____acc79
  %t13893 = add i64 23, 0
  %t13894 = extractvalue %NxVal %t13892, 1
  %t13895 = or i64 %t13894, %t13893
  %t13896 = add i64 45, 0
  %t13897 = load %NxVal, ptr @nx__g___main____i79
  %t13898 = extractvalue %NxVal %t13897, 1
  %t13899 = add i64 %t13896, %t13898
  %t13900 = or i64 %t13895, %t13899
  %t13901 = add i64 65535, 0
  %t13902 = and i64 %t13900, %t13901
  %t13903 = call %NxVal @nx_int(i64 %t13902)
  store %NxVal %t13903, ptr @nx__g___main____acc79
  %t13904 = load %NxVal, ptr @nx__g___main____acc79
  %t13905 = add i64 92, 0
  %t13906 = extractvalue %NxVal %t13904, 1
  %t13907 = mul i64 %t13906, %t13905
  %t13908 = add i64 16, 0
  %t13909 = mul i64 %t13907, %t13908
  %t13910 = load %NxVal, ptr @nx__g___main____i79
  %t13911 = extractvalue %NxVal %t13910, 1
  %t13912 = add i64 %t13909, %t13911
  %t13913 = add i64 65535, 0
  %t13914 = and i64 %t13912, %t13913
  %t13915 = call %NxVal @nx_int(i64 %t13914)
  store %NxVal %t13915, ptr @nx__g___main____acc79
  %t13916 = load %NxVal, ptr @nx__g___main____i79
  %t13917 = add i64 1, 0
  %t13918 = extractvalue %NxVal %t13916, 1
  %t13919 = add i64 %t13918, %t13917
  %t13920 = call %NxVal @nx_int(i64 %t13919)
  store %NxVal %t13920, ptr @nx__g___main____i79
  br label %wcond240
wend242:
  %t13921 = load %NxVal, ptr @nx__g___main____total
  %t13922 = load %NxVal, ptr @nx__g___main____acc79
  %t13923 = extractvalue %NxVal %t13921, 1
  %t13924 = extractvalue %NxVal %t13922, 1
  %t13925 = add i64 %t13923, %t13924
  %t13926 = add i64 65535, 0
  %t13927 = and i64 %t13925, %t13926
  %t13928 = call %NxVal @nx_int(i64 %t13927)
  store %NxVal %t13928, ptr @nx__g___main____total
  %t13929 = add i64 0, 0
  %t13930 = call %NxVal @nx_int(i64 %t13929)
  store %NxVal %t13930, ptr @nx__g___main____i80
  %t13931 = add i64 0, 0
  %t13932 = call %NxVal @nx_int(i64 %t13931)
  store %NxVal %t13932, ptr @nx__g___main____acc80
  br label %wcond243
wcond243:
  %t13933 = load %NxVal, ptr @nx__g___main____i80
  %t13934 = add i64 3, 0
  %t13935 = extractvalue %NxVal %t13933, 1
  %t13936 = icmp slt i64 %t13935, %t13934
  br i1 %t13936, label %wbody244, label %wend245
wbody244:
  %t13937 = load %NxVal, ptr @nx__g___main____acc80
  %t13938 = load %NxVal, ptr @nx__g___main____c
  %t13939 = load %NxVal, ptr @nx__g___main____i80
  %t13940 = add i64 80, 0
  %t13941 = extractvalue %NxVal %t13939, 1
  %t13942 = add i64 %t13941, %t13940
  %t13944 = getelementptr [2 x %NxVal], ptr %t13943, i64 0, i64 0
  store %NxVal %t13938, ptr %t13944
  %t13945 = call %NxVal @nx_int(i64 %t13942)
  %t13946 = getelementptr [2 x %NxVal], ptr %t13943, i64 0, i64 1
  store %NxVal %t13945, ptr %t13946
  %t13947 = getelementptr [2 x %NxVal], ptr %t13943, i64 0, i64 0
  %t13948 = call %NxVal @nx__m_3____main____Cell__m80(ptr %t13947, i64 2)
  %t13949 = extractvalue %NxVal %t13948, 1
  %t13950 = extractvalue %NxVal %t13937, 1
  %t13951 = add i64 %t13950, %t13949
  %t13952 = add i64 65535, 0
  %t13953 = and i64 %t13951, %t13952
  %t13954 = call %NxVal @nx_int(i64 %t13953)
  store %NxVal %t13954, ptr @nx__g___main____acc80
  %t13955 = load %NxVal, ptr @nx__g___main____acc80
  %t13956 = add i64 70, 0
  %t13957 = extractvalue %NxVal %t13955, 1
  %t13958 = xor i64 %t13957, %t13956
  %t13959 = add i64 6, 0
  %t13960 = load %NxVal, ptr @nx__g___main____i80
  %t13961 = extractvalue %NxVal %t13960, 1
  %t13962 = add i64 %t13959, %t13961
  %t13963 = xor i64 %t13958, %t13962
  %t13964 = add i64 65535, 0
  %t13965 = and i64 %t13963, %t13964
  %t13966 = call %NxVal @nx_int(i64 %t13965)
  store %NxVal %t13966, ptr @nx__g___main____acc80
  %t13967 = load %NxVal, ptr @nx__g___main____acc80
  %t13968 = add i64 90, 0
  %t13969 = extractvalue %NxVal %t13967, 1
  %t13970 = call i64 @nx_mod_i64(i64 %t13969, i64 %t13968)
  %t13971 = add i64 82, 0
  %t13972 = call i64 @nx_mod_i64(i64 %t13970, i64 %t13971)
  %t13973 = load %NxVal, ptr @nx__g___main____i80
  %t13974 = extractvalue %NxVal %t13973, 1
  %t13975 = add i64 %t13972, %t13974
  %t13976 = add i64 65535, 0
  %t13977 = and i64 %t13975, %t13976
  %t13978 = call %NxVal @nx_int(i64 %t13977)
  store %NxVal %t13978, ptr @nx__g___main____acc80
  %t13979 = load %NxVal, ptr @nx__g___main____acc80
  %t13980 = add i64 71, 0
  %t13981 = extractvalue %NxVal %t13979, 1
  %t13982 = or i64 %t13981, %t13980
  %t13983 = add i64 47, 0
  %t13984 = load %NxVal, ptr @nx__g___main____i80
  %t13985 = extractvalue %NxVal %t13984, 1
  %t13986 = add i64 %t13983, %t13985
  %t13987 = or i64 %t13982, %t13986
  %t13988 = add i64 65535, 0
  %t13989 = and i64 %t13987, %t13988
  %t13990 = call %NxVal @nx_int(i64 %t13989)
  store %NxVal %t13990, ptr @nx__g___main____acc80
  %t13991 = load %NxVal, ptr @nx__g___main____acc80
  %t13992 = add i64 23, 0
  %t13993 = extractvalue %NxVal %t13991, 1
  %t13994 = and i64 %t13993, %t13992
  %t13995 = add i64 52, 0
  %t13996 = load %NxVal, ptr @nx__g___main____i80
  %t13997 = extractvalue %NxVal %t13996, 1
  %t13998 = add i64 %t13995, %t13997
  %t13999 = and i64 %t13994, %t13998
  %t14000 = add i64 65535, 0
  %t14001 = and i64 %t13999, %t14000
  %t14002 = call %NxVal @nx_int(i64 %t14001)
  store %NxVal %t14002, ptr @nx__g___main____acc80
  %t14003 = load %NxVal, ptr @nx__g___main____i80
  %t14004 = add i64 1, 0
  %t14005 = extractvalue %NxVal %t14003, 1
  %t14006 = add i64 %t14005, %t14004
  %t14007 = call %NxVal @nx_int(i64 %t14006)
  store %NxVal %t14007, ptr @nx__g___main____i80
  br label %wcond243
wend245:
  %t14008 = load %NxVal, ptr @nx__g___main____total
  %t14009 = load %NxVal, ptr @nx__g___main____acc80
  %t14010 = extractvalue %NxVal %t14008, 1
  %t14011 = extractvalue %NxVal %t14009, 1
  %t14012 = add i64 %t14010, %t14011
  %t14013 = add i64 65535, 0
  %t14014 = and i64 %t14012, %t14013
  %t14015 = call %NxVal @nx_int(i64 %t14014)
  store %NxVal %t14015, ptr @nx__g___main____total
  %t14016 = add i64 0, 0
  %t14017 = call %NxVal @nx_int(i64 %t14016)
  store %NxVal %t14017, ptr @nx__g___main____i81
  %t14018 = add i64 0, 0
  %t14019 = call %NxVal @nx_int(i64 %t14018)
  store %NxVal %t14019, ptr @nx__g___main____acc81
  br label %wcond246
wcond246:
  %t14020 = load %NxVal, ptr @nx__g___main____i81
  %t14021 = add i64 3, 0
  %t14022 = extractvalue %NxVal %t14020, 1
  %t14023 = icmp slt i64 %t14022, %t14021
  br i1 %t14023, label %wbody247, label %wend248
wbody247:
  %t14024 = load %NxVal, ptr @nx__g___main____acc81
  %t14025 = load %NxVal, ptr @nx__g___main____c
  %t14026 = load %NxVal, ptr @nx__g___main____i81
  %t14027 = add i64 81, 0
  %t14028 = extractvalue %NxVal %t14026, 1
  %t14029 = add i64 %t14028, %t14027
  %t14031 = getelementptr [2 x %NxVal], ptr %t14030, i64 0, i64 0
  store %NxVal %t14025, ptr %t14031
  %t14032 = call %NxVal @nx_int(i64 %t14029)
  %t14033 = getelementptr [2 x %NxVal], ptr %t14030, i64 0, i64 1
  store %NxVal %t14032, ptr %t14033
  %t14034 = getelementptr [2 x %NxVal], ptr %t14030, i64 0, i64 0
  %t14035 = call %NxVal @nx__m_3____main____Cell__m81(ptr %t14034, i64 2)
  %t14036 = extractvalue %NxVal %t14035, 1
  %t14037 = extractvalue %NxVal %t14024, 1
  %t14038 = add i64 %t14037, %t14036
  %t14039 = add i64 65535, 0
  %t14040 = and i64 %t14038, %t14039
  %t14041 = call %NxVal @nx_int(i64 %t14040)
  store %NxVal %t14041, ptr @nx__g___main____acc81
  %t14042 = load %NxVal, ptr @nx__g___main____acc81
  %t14043 = add i64 4, 0
  %t14044 = extractvalue %NxVal %t14042, 1
  %t14045 = call i64 @nx_mod_i64(i64 %t14044, i64 %t14043)
  %t14046 = add i64 32, 0
  %t14047 = call i64 @nx_mod_i64(i64 %t14045, i64 %t14046)
  %t14048 = load %NxVal, ptr @nx__g___main____i81
  %t14049 = extractvalue %NxVal %t14048, 1
  %t14050 = add i64 %t14047, %t14049
  %t14051 = add i64 65535, 0
  %t14052 = and i64 %t14050, %t14051
  %t14053 = call %NxVal @nx_int(i64 %t14052)
  store %NxVal %t14053, ptr @nx__g___main____acc81
  %t14054 = load %NxVal, ptr @nx__g___main____acc81
  %t14055 = add i64 77, 0
  %t14056 = extractvalue %NxVal %t14054, 1
  %t14057 = xor i64 %t14056, %t14055
  %t14058 = add i64 33, 0
  %t14059 = load %NxVal, ptr @nx__g___main____i81
  %t14060 = extractvalue %NxVal %t14059, 1
  %t14061 = add i64 %t14058, %t14060
  %t14062 = xor i64 %t14057, %t14061
  %t14063 = add i64 65535, 0
  %t14064 = and i64 %t14062, %t14063
  %t14065 = call %NxVal @nx_int(i64 %t14064)
  store %NxVal %t14065, ptr @nx__g___main____acc81
  %t14066 = load %NxVal, ptr @nx__g___main____acc81
  %t14067 = add i64 29, 0
  %t14068 = extractvalue %NxVal %t14066, 1
  %t14069 = sub i64 %t14068, %t14067
  %t14070 = add i64 10, 0
  %t14071 = sub i64 %t14069, %t14070
  %t14072 = load %NxVal, ptr @nx__g___main____i81
  %t14073 = extractvalue %NxVal %t14072, 1
  %t14074 = add i64 %t14071, %t14073
  %t14075 = add i64 65535, 0
  %t14076 = and i64 %t14074, %t14075
  %t14077 = call %NxVal @nx_int(i64 %t14076)
  store %NxVal %t14077, ptr @nx__g___main____acc81
  %t14078 = load %NxVal, ptr @nx__g___main____acc81
  %t14079 = add i64 30, 0
  %t14080 = extractvalue %NxVal %t14078, 1
  %t14081 = call i64 @nx_mod_i64(i64 %t14080, i64 %t14079)
  %t14082 = add i64 76, 0
  %t14083 = call i64 @nx_mod_i64(i64 %t14081, i64 %t14082)
  %t14084 = load %NxVal, ptr @nx__g___main____i81
  %t14085 = extractvalue %NxVal %t14084, 1
  %t14086 = add i64 %t14083, %t14085
  %t14087 = add i64 65535, 0
  %t14088 = and i64 %t14086, %t14087
  %t14089 = call %NxVal @nx_int(i64 %t14088)
  store %NxVal %t14089, ptr @nx__g___main____acc81
  %t14090 = load %NxVal, ptr @nx__g___main____i81
  %t14091 = add i64 1, 0
  %t14092 = extractvalue %NxVal %t14090, 1
  %t14093 = add i64 %t14092, %t14091
  %t14094 = call %NxVal @nx_int(i64 %t14093)
  store %NxVal %t14094, ptr @nx__g___main____i81
  br label %wcond246
wend248:
  %t14095 = load %NxVal, ptr @nx__g___main____total
  %t14096 = load %NxVal, ptr @nx__g___main____acc81
  %t14097 = extractvalue %NxVal %t14095, 1
  %t14098 = extractvalue %NxVal %t14096, 1
  %t14099 = add i64 %t14097, %t14098
  %t14100 = add i64 65535, 0
  %t14101 = and i64 %t14099, %t14100
  %t14102 = call %NxVal @nx_int(i64 %t14101)
  store %NxVal %t14102, ptr @nx__g___main____total
  %t14103 = add i64 0, 0
  %t14104 = call %NxVal @nx_int(i64 %t14103)
  store %NxVal %t14104, ptr @nx__g___main____i82
  %t14105 = add i64 0, 0
  %t14106 = call %NxVal @nx_int(i64 %t14105)
  store %NxVal %t14106, ptr @nx__g___main____acc82
  br label %wcond249
wcond249:
  %t14107 = load %NxVal, ptr @nx__g___main____i82
  %t14108 = add i64 3, 0
  %t14109 = extractvalue %NxVal %t14107, 1
  %t14110 = icmp slt i64 %t14109, %t14108
  br i1 %t14110, label %wbody250, label %wend251
wbody250:
  %t14111 = load %NxVal, ptr @nx__g___main____acc82
  %t14112 = load %NxVal, ptr @nx__g___main____c
  %t14113 = load %NxVal, ptr @nx__g___main____i82
  %t14114 = add i64 82, 0
  %t14115 = extractvalue %NxVal %t14113, 1
  %t14116 = add i64 %t14115, %t14114
  %t14118 = getelementptr [2 x %NxVal], ptr %t14117, i64 0, i64 0
  store %NxVal %t14112, ptr %t14118
  %t14119 = call %NxVal @nx_int(i64 %t14116)
  %t14120 = getelementptr [2 x %NxVal], ptr %t14117, i64 0, i64 1
  store %NxVal %t14119, ptr %t14120
  %t14121 = getelementptr [2 x %NxVal], ptr %t14117, i64 0, i64 0
  %t14122 = call %NxVal @nx__m_3____main____Cell__m82(ptr %t14121, i64 2)
  %t14123 = extractvalue %NxVal %t14122, 1
  %t14124 = extractvalue %NxVal %t14111, 1
  %t14125 = add i64 %t14124, %t14123
  %t14126 = add i64 65535, 0
  %t14127 = and i64 %t14125, %t14126
  %t14128 = call %NxVal @nx_int(i64 %t14127)
  store %NxVal %t14128, ptr @nx__g___main____acc82
  %t14129 = load %NxVal, ptr @nx__g___main____acc82
  %t14130 = add i64 11, 0
  %t14131 = extractvalue %NxVal %t14129, 1
  %t14132 = xor i64 %t14131, %t14130
  %t14133 = add i64 51, 0
  %t14134 = load %NxVal, ptr @nx__g___main____i82
  %t14135 = extractvalue %NxVal %t14134, 1
  %t14136 = add i64 %t14133, %t14135
  %t14137 = xor i64 %t14132, %t14136
  %t14138 = add i64 65535, 0
  %t14139 = and i64 %t14137, %t14138
  %t14140 = call %NxVal @nx_int(i64 %t14139)
  store %NxVal %t14140, ptr @nx__g___main____acc82
  %t14141 = load %NxVal, ptr @nx__g___main____acc82
  %t14142 = add i64 71, 0
  %t14143 = extractvalue %NxVal %t14141, 1
  %t14144 = or i64 %t14143, %t14142
  %t14145 = add i64 32, 0
  %t14146 = load %NxVal, ptr @nx__g___main____i82
  %t14147 = extractvalue %NxVal %t14146, 1
  %t14148 = add i64 %t14145, %t14147
  %t14149 = or i64 %t14144, %t14148
  %t14150 = add i64 65535, 0
  %t14151 = and i64 %t14149, %t14150
  %t14152 = call %NxVal @nx_int(i64 %t14151)
  store %NxVal %t14152, ptr @nx__g___main____acc82
  %t14153 = load %NxVal, ptr @nx__g___main____acc82
  %t14154 = add i64 59, 0
  %t14155 = extractvalue %NxVal %t14153, 1
  %t14156 = and i64 %t14155, %t14154
  %t14157 = add i64 64, 0
  %t14158 = load %NxVal, ptr @nx__g___main____i82
  %t14159 = extractvalue %NxVal %t14158, 1
  %t14160 = add i64 %t14157, %t14159
  %t14161 = and i64 %t14156, %t14160
  %t14162 = add i64 65535, 0
  %t14163 = and i64 %t14161, %t14162
  %t14164 = call %NxVal @nx_int(i64 %t14163)
  store %NxVal %t14164, ptr @nx__g___main____acc82
  %t14165 = load %NxVal, ptr @nx__g___main____acc82
  %t14166 = add i64 2, 0
  %t14167 = extractvalue %NxVal %t14165, 1
  %t14168 = xor i64 %t14167, %t14166
  %t14169 = add i64 33, 0
  %t14170 = load %NxVal, ptr @nx__g___main____i82
  %t14171 = extractvalue %NxVal %t14170, 1
  %t14172 = add i64 %t14169, %t14171
  %t14173 = xor i64 %t14168, %t14172
  %t14174 = add i64 65535, 0
  %t14175 = and i64 %t14173, %t14174
  %t14176 = call %NxVal @nx_int(i64 %t14175)
  store %NxVal %t14176, ptr @nx__g___main____acc82
  %t14177 = load %NxVal, ptr @nx__g___main____i82
  %t14178 = add i64 1, 0
  %t14179 = extractvalue %NxVal %t14177, 1
  %t14180 = add i64 %t14179, %t14178
  %t14181 = call %NxVal @nx_int(i64 %t14180)
  store %NxVal %t14181, ptr @nx__g___main____i82
  br label %wcond249
wend251:
  %t14182 = load %NxVal, ptr @nx__g___main____total
  %t14183 = load %NxVal, ptr @nx__g___main____acc82
  %t14184 = extractvalue %NxVal %t14182, 1
  %t14185 = extractvalue %NxVal %t14183, 1
  %t14186 = add i64 %t14184, %t14185
  %t14187 = add i64 65535, 0
  %t14188 = and i64 %t14186, %t14187
  %t14189 = call %NxVal @nx_int(i64 %t14188)
  store %NxVal %t14189, ptr @nx__g___main____total
  %t14190 = add i64 0, 0
  %t14191 = call %NxVal @nx_int(i64 %t14190)
  store %NxVal %t14191, ptr @nx__g___main____i83
  %t14192 = add i64 0, 0
  %t14193 = call %NxVal @nx_int(i64 %t14192)
  store %NxVal %t14193, ptr @nx__g___main____acc83
  br label %wcond252
wcond252:
  %t14194 = load %NxVal, ptr @nx__g___main____i83
  %t14195 = add i64 3, 0
  %t14196 = extractvalue %NxVal %t14194, 1
  %t14197 = icmp slt i64 %t14196, %t14195
  br i1 %t14197, label %wbody253, label %wend254
wbody253:
  %t14198 = load %NxVal, ptr @nx__g___main____acc83
  %t14199 = load %NxVal, ptr @nx__g___main____c
  %t14200 = load %NxVal, ptr @nx__g___main____i83
  %t14201 = add i64 83, 0
  %t14202 = extractvalue %NxVal %t14200, 1
  %t14203 = add i64 %t14202, %t14201
  %t14205 = getelementptr [2 x %NxVal], ptr %t14204, i64 0, i64 0
  store %NxVal %t14199, ptr %t14205
  %t14206 = call %NxVal @nx_int(i64 %t14203)
  %t14207 = getelementptr [2 x %NxVal], ptr %t14204, i64 0, i64 1
  store %NxVal %t14206, ptr %t14207
  %t14208 = getelementptr [2 x %NxVal], ptr %t14204, i64 0, i64 0
  %t14209 = call %NxVal @nx__m_3____main____Cell__m83(ptr %t14208, i64 2)
  %t14210 = extractvalue %NxVal %t14209, 1
  %t14211 = extractvalue %NxVal %t14198, 1
  %t14212 = add i64 %t14211, %t14210
  %t14213 = add i64 65535, 0
  %t14214 = and i64 %t14212, %t14213
  %t14215 = call %NxVal @nx_int(i64 %t14214)
  store %NxVal %t14215, ptr @nx__g___main____acc83
  %t14216 = load %NxVal, ptr @nx__g___main____acc83
  %t14217 = add i64 50, 0
  %t14218 = extractvalue %NxVal %t14216, 1
  %t14219 = add i64 %t14218, %t14217
  %t14220 = add i64 67, 0
  %t14221 = add i64 %t14219, %t14220
  %t14222 = load %NxVal, ptr @nx__g___main____i83
  %t14223 = extractvalue %NxVal %t14222, 1
  %t14224 = add i64 %t14221, %t14223
  %t14225 = add i64 65535, 0
  %t14226 = and i64 %t14224, %t14225
  %t14227 = call %NxVal @nx_int(i64 %t14226)
  store %NxVal %t14227, ptr @nx__g___main____acc83
  %t14228 = load %NxVal, ptr @nx__g___main____acc83
  %t14229 = add i64 39, 0
  %t14230 = extractvalue %NxVal %t14228, 1
  %t14231 = or i64 %t14230, %t14229
  %t14232 = add i64 53, 0
  %t14233 = load %NxVal, ptr @nx__g___main____i83
  %t14234 = extractvalue %NxVal %t14233, 1
  %t14235 = add i64 %t14232, %t14234
  %t14236 = or i64 %t14231, %t14235
  %t14237 = add i64 65535, 0
  %t14238 = and i64 %t14236, %t14237
  %t14239 = call %NxVal @nx_int(i64 %t14238)
  store %NxVal %t14239, ptr @nx__g___main____acc83
  %t14240 = load %NxVal, ptr @nx__g___main____acc83
  %t14241 = add i64 73, 0
  %t14242 = extractvalue %NxVal %t14240, 1
  %t14243 = add i64 %t14242, %t14241
  %t14244 = add i64 17, 0
  %t14245 = add i64 %t14243, %t14244
  %t14246 = load %NxVal, ptr @nx__g___main____i83
  %t14247 = extractvalue %NxVal %t14246, 1
  %t14248 = add i64 %t14245, %t14247
  %t14249 = add i64 65535, 0
  %t14250 = and i64 %t14248, %t14249
  %t14251 = call %NxVal @nx_int(i64 %t14250)
  store %NxVal %t14251, ptr @nx__g___main____acc83
  %t14252 = load %NxVal, ptr @nx__g___main____acc83
  %t14253 = add i64 37, 0
  %t14254 = extractvalue %NxVal %t14252, 1
  %t14255 = xor i64 %t14254, %t14253
  %t14256 = add i64 37, 0
  %t14257 = load %NxVal, ptr @nx__g___main____i83
  %t14258 = extractvalue %NxVal %t14257, 1
  %t14259 = add i64 %t14256, %t14258
  %t14260 = xor i64 %t14255, %t14259
  %t14261 = add i64 65535, 0
  %t14262 = and i64 %t14260, %t14261
  %t14263 = call %NxVal @nx_int(i64 %t14262)
  store %NxVal %t14263, ptr @nx__g___main____acc83
  %t14264 = load %NxVal, ptr @nx__g___main____i83
  %t14265 = add i64 1, 0
  %t14266 = extractvalue %NxVal %t14264, 1
  %t14267 = add i64 %t14266, %t14265
  %t14268 = call %NxVal @nx_int(i64 %t14267)
  store %NxVal %t14268, ptr @nx__g___main____i83
  br label %wcond252
wend254:
  %t14269 = load %NxVal, ptr @nx__g___main____total
  %t14270 = load %NxVal, ptr @nx__g___main____acc83
  %t14271 = extractvalue %NxVal %t14269, 1
  %t14272 = extractvalue %NxVal %t14270, 1
  %t14273 = add i64 %t14271, %t14272
  %t14274 = add i64 65535, 0
  %t14275 = and i64 %t14273, %t14274
  %t14276 = call %NxVal @nx_int(i64 %t14275)
  store %NxVal %t14276, ptr @nx__g___main____total
  %t14277 = add i64 0, 0
  %t14278 = call %NxVal @nx_int(i64 %t14277)
  store %NxVal %t14278, ptr @nx__g___main____i84
  %t14279 = add i64 0, 0
  %t14280 = call %NxVal @nx_int(i64 %t14279)
  store %NxVal %t14280, ptr @nx__g___main____acc84
  br label %wcond255
wcond255:
  %t14281 = load %NxVal, ptr @nx__g___main____i84
  %t14282 = add i64 3, 0
  %t14283 = extractvalue %NxVal %t14281, 1
  %t14284 = icmp slt i64 %t14283, %t14282
  br i1 %t14284, label %wbody256, label %wend257
wbody256:
  %t14285 = load %NxVal, ptr @nx__g___main____acc84
  %t14286 = load %NxVal, ptr @nx__g___main____c
  %t14287 = load %NxVal, ptr @nx__g___main____i84
  %t14288 = add i64 84, 0
  %t14289 = extractvalue %NxVal %t14287, 1
  %t14290 = add i64 %t14289, %t14288
  %t14292 = getelementptr [2 x %NxVal], ptr %t14291, i64 0, i64 0
  store %NxVal %t14286, ptr %t14292
  %t14293 = call %NxVal @nx_int(i64 %t14290)
  %t14294 = getelementptr [2 x %NxVal], ptr %t14291, i64 0, i64 1
  store %NxVal %t14293, ptr %t14294
  %t14295 = getelementptr [2 x %NxVal], ptr %t14291, i64 0, i64 0
  %t14296 = call %NxVal @nx__m_3____main____Cell__m84(ptr %t14295, i64 2)
  %t14297 = extractvalue %NxVal %t14296, 1
  %t14298 = extractvalue %NxVal %t14285, 1
  %t14299 = add i64 %t14298, %t14297
  %t14300 = add i64 65535, 0
  %t14301 = and i64 %t14299, %t14300
  %t14302 = call %NxVal @nx_int(i64 %t14301)
  store %NxVal %t14302, ptr @nx__g___main____acc84
  %t14303 = load %NxVal, ptr @nx__g___main____acc84
  %t14304 = add i64 20, 0
  %t14305 = extractvalue %NxVal %t14303, 1
  %t14306 = and i64 %t14305, %t14304
  %t14307 = add i64 21, 0
  %t14308 = load %NxVal, ptr @nx__g___main____i84
  %t14309 = extractvalue %NxVal %t14308, 1
  %t14310 = add i64 %t14307, %t14309
  %t14311 = and i64 %t14306, %t14310
  %t14312 = add i64 65535, 0
  %t14313 = and i64 %t14311, %t14312
  %t14314 = call %NxVal @nx_int(i64 %t14313)
  store %NxVal %t14314, ptr @nx__g___main____acc84
  %t14315 = load %NxVal, ptr @nx__g___main____acc84
  %t14316 = add i64 72, 0
  %t14317 = extractvalue %NxVal %t14315, 1
  %t14318 = call i64 @nx_mod_i64(i64 %t14317, i64 %t14316)
  %t14319 = add i64 31, 0
  %t14320 = call i64 @nx_mod_i64(i64 %t14318, i64 %t14319)
  %t14321 = load %NxVal, ptr @nx__g___main____i84
  %t14322 = extractvalue %NxVal %t14321, 1
  %t14323 = add i64 %t14320, %t14322
  %t14324 = add i64 65535, 0
  %t14325 = and i64 %t14323, %t14324
  %t14326 = call %NxVal @nx_int(i64 %t14325)
  store %NxVal %t14326, ptr @nx__g___main____acc84
  %t14327 = load %NxVal, ptr @nx__g___main____acc84
  %t14328 = add i64 25, 0
  %t14329 = extractvalue %NxVal %t14327, 1
  %t14330 = and i64 %t14329, %t14328
  %t14331 = add i64 28, 0
  %t14332 = load %NxVal, ptr @nx__g___main____i84
  %t14333 = extractvalue %NxVal %t14332, 1
  %t14334 = add i64 %t14331, %t14333
  %t14335 = and i64 %t14330, %t14334
  %t14336 = add i64 65535, 0
  %t14337 = and i64 %t14335, %t14336
  %t14338 = call %NxVal @nx_int(i64 %t14337)
  store %NxVal %t14338, ptr @nx__g___main____acc84
  %t14339 = load %NxVal, ptr @nx__g___main____acc84
  %t14340 = add i64 29, 0
  %t14341 = extractvalue %NxVal %t14339, 1
  %t14342 = and i64 %t14341, %t14340
  %t14343 = add i64 87, 0
  %t14344 = load %NxVal, ptr @nx__g___main____i84
  %t14345 = extractvalue %NxVal %t14344, 1
  %t14346 = add i64 %t14343, %t14345
  %t14347 = and i64 %t14342, %t14346
  %t14348 = add i64 65535, 0
  %t14349 = and i64 %t14347, %t14348
  %t14350 = call %NxVal @nx_int(i64 %t14349)
  store %NxVal %t14350, ptr @nx__g___main____acc84
  %t14351 = load %NxVal, ptr @nx__g___main____i84
  %t14352 = add i64 1, 0
  %t14353 = extractvalue %NxVal %t14351, 1
  %t14354 = add i64 %t14353, %t14352
  %t14355 = call %NxVal @nx_int(i64 %t14354)
  store %NxVal %t14355, ptr @nx__g___main____i84
  br label %wcond255
wend257:
  %t14356 = load %NxVal, ptr @nx__g___main____total
  %t14357 = load %NxVal, ptr @nx__g___main____acc84
  %t14358 = extractvalue %NxVal %t14356, 1
  %t14359 = extractvalue %NxVal %t14357, 1
  %t14360 = add i64 %t14358, %t14359
  %t14361 = add i64 65535, 0
  %t14362 = and i64 %t14360, %t14361
  %t14363 = call %NxVal @nx_int(i64 %t14362)
  store %NxVal %t14363, ptr @nx__g___main____total
  %t14364 = add i64 0, 0
  %t14365 = call %NxVal @nx_int(i64 %t14364)
  store %NxVal %t14365, ptr @nx__g___main____i85
  %t14366 = add i64 0, 0
  %t14367 = call %NxVal @nx_int(i64 %t14366)
  store %NxVal %t14367, ptr @nx__g___main____acc85
  br label %wcond258
wcond258:
  %t14368 = load %NxVal, ptr @nx__g___main____i85
  %t14369 = add i64 3, 0
  %t14370 = extractvalue %NxVal %t14368, 1
  %t14371 = icmp slt i64 %t14370, %t14369
  br i1 %t14371, label %wbody259, label %wend260
wbody259:
  %t14372 = load %NxVal, ptr @nx__g___main____acc85
  %t14373 = load %NxVal, ptr @nx__g___main____c
  %t14374 = load %NxVal, ptr @nx__g___main____i85
  %t14375 = add i64 85, 0
  %t14376 = extractvalue %NxVal %t14374, 1
  %t14377 = add i64 %t14376, %t14375
  %t14379 = getelementptr [2 x %NxVal], ptr %t14378, i64 0, i64 0
  store %NxVal %t14373, ptr %t14379
  %t14380 = call %NxVal @nx_int(i64 %t14377)
  %t14381 = getelementptr [2 x %NxVal], ptr %t14378, i64 0, i64 1
  store %NxVal %t14380, ptr %t14381
  %t14382 = getelementptr [2 x %NxVal], ptr %t14378, i64 0, i64 0
  %t14383 = call %NxVal @nx__m_3____main____Cell__m85(ptr %t14382, i64 2)
  %t14384 = extractvalue %NxVal %t14383, 1
  %t14385 = extractvalue %NxVal %t14372, 1
  %t14386 = add i64 %t14385, %t14384
  %t14387 = add i64 65535, 0
  %t14388 = and i64 %t14386, %t14387
  %t14389 = call %NxVal @nx_int(i64 %t14388)
  store %NxVal %t14389, ptr @nx__g___main____acc85
  %t14390 = load %NxVal, ptr @nx__g___main____acc85
  %t14391 = add i64 12, 0
  %t14392 = extractvalue %NxVal %t14390, 1
  %t14393 = sub i64 %t14392, %t14391
  %t14394 = add i64 45, 0
  %t14395 = sub i64 %t14393, %t14394
  %t14396 = load %NxVal, ptr @nx__g___main____i85
  %t14397 = extractvalue %NxVal %t14396, 1
  %t14398 = add i64 %t14395, %t14397
  %t14399 = add i64 65535, 0
  %t14400 = and i64 %t14398, %t14399
  %t14401 = call %NxVal @nx_int(i64 %t14400)
  store %NxVal %t14401, ptr @nx__g___main____acc85
  %t14402 = load %NxVal, ptr @nx__g___main____acc85
  %t14403 = add i64 38, 0
  %t14404 = extractvalue %NxVal %t14402, 1
  %t14405 = or i64 %t14404, %t14403
  %t14406 = add i64 77, 0
  %t14407 = load %NxVal, ptr @nx__g___main____i85
  %t14408 = extractvalue %NxVal %t14407, 1
  %t14409 = add i64 %t14406, %t14408
  %t14410 = or i64 %t14405, %t14409
  %t14411 = add i64 65535, 0
  %t14412 = and i64 %t14410, %t14411
  %t14413 = call %NxVal @nx_int(i64 %t14412)
  store %NxVal %t14413, ptr @nx__g___main____acc85
  %t14414 = load %NxVal, ptr @nx__g___main____acc85
  %t14415 = add i64 29, 0
  %t14416 = extractvalue %NxVal %t14414, 1
  %t14417 = add i64 %t14416, %t14415
  %t14418 = add i64 37, 0
  %t14419 = add i64 %t14417, %t14418
  %t14420 = load %NxVal, ptr @nx__g___main____i85
  %t14421 = extractvalue %NxVal %t14420, 1
  %t14422 = add i64 %t14419, %t14421
  %t14423 = add i64 65535, 0
  %t14424 = and i64 %t14422, %t14423
  %t14425 = call %NxVal @nx_int(i64 %t14424)
  store %NxVal %t14425, ptr @nx__g___main____acc85
  %t14426 = load %NxVal, ptr @nx__g___main____acc85
  %t14427 = add i64 31, 0
  %t14428 = extractvalue %NxVal %t14426, 1
  %t14429 = sub i64 %t14428, %t14427
  %t14430 = add i64 45, 0
  %t14431 = sub i64 %t14429, %t14430
  %t14432 = load %NxVal, ptr @nx__g___main____i85
  %t14433 = extractvalue %NxVal %t14432, 1
  %t14434 = add i64 %t14431, %t14433
  %t14435 = add i64 65535, 0
  %t14436 = and i64 %t14434, %t14435
  %t14437 = call %NxVal @nx_int(i64 %t14436)
  store %NxVal %t14437, ptr @nx__g___main____acc85
  %t14438 = load %NxVal, ptr @nx__g___main____i85
  %t14439 = add i64 1, 0
  %t14440 = extractvalue %NxVal %t14438, 1
  %t14441 = add i64 %t14440, %t14439
  %t14442 = call %NxVal @nx_int(i64 %t14441)
  store %NxVal %t14442, ptr @nx__g___main____i85
  br label %wcond258
wend260:
  %t14443 = load %NxVal, ptr @nx__g___main____total
  %t14444 = load %NxVal, ptr @nx__g___main____acc85
  %t14445 = extractvalue %NxVal %t14443, 1
  %t14446 = extractvalue %NxVal %t14444, 1
  %t14447 = add i64 %t14445, %t14446
  %t14448 = add i64 65535, 0
  %t14449 = and i64 %t14447, %t14448
  %t14450 = call %NxVal @nx_int(i64 %t14449)
  store %NxVal %t14450, ptr @nx__g___main____total
  %t14451 = add i64 0, 0
  %t14452 = call %NxVal @nx_int(i64 %t14451)
  store %NxVal %t14452, ptr @nx__g___main____i86
  %t14453 = add i64 0, 0
  %t14454 = call %NxVal @nx_int(i64 %t14453)
  store %NxVal %t14454, ptr @nx__g___main____acc86
  br label %wcond261
wcond261:
  %t14455 = load %NxVal, ptr @nx__g___main____i86
  %t14456 = add i64 3, 0
  %t14457 = extractvalue %NxVal %t14455, 1
  %t14458 = icmp slt i64 %t14457, %t14456
  br i1 %t14458, label %wbody262, label %wend263
wbody262:
  %t14459 = load %NxVal, ptr @nx__g___main____acc86
  %t14460 = load %NxVal, ptr @nx__g___main____c
  %t14461 = load %NxVal, ptr @nx__g___main____i86
  %t14462 = add i64 86, 0
  %t14463 = extractvalue %NxVal %t14461, 1
  %t14464 = add i64 %t14463, %t14462
  %t14466 = getelementptr [2 x %NxVal], ptr %t14465, i64 0, i64 0
  store %NxVal %t14460, ptr %t14466
  %t14467 = call %NxVal @nx_int(i64 %t14464)
  %t14468 = getelementptr [2 x %NxVal], ptr %t14465, i64 0, i64 1
  store %NxVal %t14467, ptr %t14468
  %t14469 = getelementptr [2 x %NxVal], ptr %t14465, i64 0, i64 0
  %t14470 = call %NxVal @nx__m_3____main____Cell__m86(ptr %t14469, i64 2)
  %t14471 = extractvalue %NxVal %t14470, 1
  %t14472 = extractvalue %NxVal %t14459, 1
  %t14473 = add i64 %t14472, %t14471
  %t14474 = add i64 65535, 0
  %t14475 = and i64 %t14473, %t14474
  %t14476 = call %NxVal @nx_int(i64 %t14475)
  store %NxVal %t14476, ptr @nx__g___main____acc86
  %t14477 = load %NxVal, ptr @nx__g___main____acc86
  %t14478 = add i64 65, 0
  %t14479 = extractvalue %NxVal %t14477, 1
  %t14480 = call i64 @nx_mod_i64(i64 %t14479, i64 %t14478)
  %t14481 = add i64 9, 0
  %t14482 = call i64 @nx_mod_i64(i64 %t14480, i64 %t14481)
  %t14483 = load %NxVal, ptr @nx__g___main____i86
  %t14484 = extractvalue %NxVal %t14483, 1
  %t14485 = add i64 %t14482, %t14484
  %t14486 = add i64 65535, 0
  %t14487 = and i64 %t14485, %t14486
  %t14488 = call %NxVal @nx_int(i64 %t14487)
  store %NxVal %t14488, ptr @nx__g___main____acc86
  %t14489 = load %NxVal, ptr @nx__g___main____acc86
  %t14490 = add i64 37, 0
  %t14491 = extractvalue %NxVal %t14489, 1
  %t14492 = and i64 %t14491, %t14490
  %t14493 = add i64 26, 0
  %t14494 = load %NxVal, ptr @nx__g___main____i86
  %t14495 = extractvalue %NxVal %t14494, 1
  %t14496 = add i64 %t14493, %t14495
  %t14497 = and i64 %t14492, %t14496
  %t14498 = add i64 65535, 0
  %t14499 = and i64 %t14497, %t14498
  %t14500 = call %NxVal @nx_int(i64 %t14499)
  store %NxVal %t14500, ptr @nx__g___main____acc86
  %t14501 = load %NxVal, ptr @nx__g___main____acc86
  %t14502 = add i64 10, 0
  %t14503 = extractvalue %NxVal %t14501, 1
  %t14504 = mul i64 %t14503, %t14502
  %t14505 = add i64 82, 0
  %t14506 = mul i64 %t14504, %t14505
  %t14507 = load %NxVal, ptr @nx__g___main____i86
  %t14508 = extractvalue %NxVal %t14507, 1
  %t14509 = add i64 %t14506, %t14508
  %t14510 = add i64 65535, 0
  %t14511 = and i64 %t14509, %t14510
  %t14512 = call %NxVal @nx_int(i64 %t14511)
  store %NxVal %t14512, ptr @nx__g___main____acc86
  %t14513 = load %NxVal, ptr @nx__g___main____acc86
  %t14514 = add i64 20, 0
  %t14515 = extractvalue %NxVal %t14513, 1
  %t14516 = call i64 @nx_mod_i64(i64 %t14515, i64 %t14514)
  %t14517 = add i64 68, 0
  %t14518 = call i64 @nx_mod_i64(i64 %t14516, i64 %t14517)
  %t14519 = load %NxVal, ptr @nx__g___main____i86
  %t14520 = extractvalue %NxVal %t14519, 1
  %t14521 = add i64 %t14518, %t14520
  %t14522 = add i64 65535, 0
  %t14523 = and i64 %t14521, %t14522
  %t14524 = call %NxVal @nx_int(i64 %t14523)
  store %NxVal %t14524, ptr @nx__g___main____acc86
  %t14525 = load %NxVal, ptr @nx__g___main____i86
  %t14526 = add i64 1, 0
  %t14527 = extractvalue %NxVal %t14525, 1
  %t14528 = add i64 %t14527, %t14526
  %t14529 = call %NxVal @nx_int(i64 %t14528)
  store %NxVal %t14529, ptr @nx__g___main____i86
  br label %wcond261
wend263:
  %t14530 = load %NxVal, ptr @nx__g___main____total
  %t14531 = load %NxVal, ptr @nx__g___main____acc86
  %t14532 = extractvalue %NxVal %t14530, 1
  %t14533 = extractvalue %NxVal %t14531, 1
  %t14534 = add i64 %t14532, %t14533
  %t14535 = add i64 65535, 0
  %t14536 = and i64 %t14534, %t14535
  %t14537 = call %NxVal @nx_int(i64 %t14536)
  store %NxVal %t14537, ptr @nx__g___main____total
  %t14538 = add i64 0, 0
  %t14539 = call %NxVal @nx_int(i64 %t14538)
  store %NxVal %t14539, ptr @nx__g___main____i87
  %t14540 = add i64 0, 0
  %t14541 = call %NxVal @nx_int(i64 %t14540)
  store %NxVal %t14541, ptr @nx__g___main____acc87
  br label %wcond264
wcond264:
  %t14542 = load %NxVal, ptr @nx__g___main____i87
  %t14543 = add i64 3, 0
  %t14544 = extractvalue %NxVal %t14542, 1
  %t14545 = icmp slt i64 %t14544, %t14543
  br i1 %t14545, label %wbody265, label %wend266
wbody265:
  %t14546 = load %NxVal, ptr @nx__g___main____acc87
  %t14547 = load %NxVal, ptr @nx__g___main____c
  %t14548 = load %NxVal, ptr @nx__g___main____i87
  %t14549 = add i64 87, 0
  %t14550 = extractvalue %NxVal %t14548, 1
  %t14551 = add i64 %t14550, %t14549
  %t14553 = getelementptr [2 x %NxVal], ptr %t14552, i64 0, i64 0
  store %NxVal %t14547, ptr %t14553
  %t14554 = call %NxVal @nx_int(i64 %t14551)
  %t14555 = getelementptr [2 x %NxVal], ptr %t14552, i64 0, i64 1
  store %NxVal %t14554, ptr %t14555
  %t14556 = getelementptr [2 x %NxVal], ptr %t14552, i64 0, i64 0
  %t14557 = call %NxVal @nx__m_3____main____Cell__m87(ptr %t14556, i64 2)
  %t14558 = extractvalue %NxVal %t14557, 1
  %t14559 = extractvalue %NxVal %t14546, 1
  %t14560 = add i64 %t14559, %t14558
  %t14561 = add i64 65535, 0
  %t14562 = and i64 %t14560, %t14561
  %t14563 = call %NxVal @nx_int(i64 %t14562)
  store %NxVal %t14563, ptr @nx__g___main____acc87
  %t14564 = load %NxVal, ptr @nx__g___main____acc87
  %t14565 = add i64 94, 0
  %t14566 = extractvalue %NxVal %t14564, 1
  %t14567 = or i64 %t14566, %t14565
  %t14568 = add i64 35, 0
  %t14569 = load %NxVal, ptr @nx__g___main____i87
  %t14570 = extractvalue %NxVal %t14569, 1
  %t14571 = add i64 %t14568, %t14570
  %t14572 = or i64 %t14567, %t14571
  %t14573 = add i64 65535, 0
  %t14574 = and i64 %t14572, %t14573
  %t14575 = call %NxVal @nx_int(i64 %t14574)
  store %NxVal %t14575, ptr @nx__g___main____acc87
  %t14576 = load %NxVal, ptr @nx__g___main____acc87
  %t14577 = add i64 75, 0
  %t14578 = extractvalue %NxVal %t14576, 1
  %t14579 = mul i64 %t14578, %t14577
  %t14580 = add i64 57, 0
  %t14581 = mul i64 %t14579, %t14580
  %t14582 = load %NxVal, ptr @nx__g___main____i87
  %t14583 = extractvalue %NxVal %t14582, 1
  %t14584 = add i64 %t14581, %t14583
  %t14585 = add i64 65535, 0
  %t14586 = and i64 %t14584, %t14585
  %t14587 = call %NxVal @nx_int(i64 %t14586)
  store %NxVal %t14587, ptr @nx__g___main____acc87
  %t14588 = load %NxVal, ptr @nx__g___main____acc87
  %t14589 = add i64 94, 0
  %t14590 = extractvalue %NxVal %t14588, 1
  %t14591 = sub i64 %t14590, %t14589
  %t14592 = add i64 35, 0
  %t14593 = sub i64 %t14591, %t14592
  %t14594 = load %NxVal, ptr @nx__g___main____i87
  %t14595 = extractvalue %NxVal %t14594, 1
  %t14596 = add i64 %t14593, %t14595
  %t14597 = add i64 65535, 0
  %t14598 = and i64 %t14596, %t14597
  %t14599 = call %NxVal @nx_int(i64 %t14598)
  store %NxVal %t14599, ptr @nx__g___main____acc87
  %t14600 = load %NxVal, ptr @nx__g___main____acc87
  %t14601 = add i64 3, 0
  %t14602 = extractvalue %NxVal %t14600, 1
  %t14603 = add i64 %t14602, %t14601
  %t14604 = add i64 36, 0
  %t14605 = add i64 %t14603, %t14604
  %t14606 = load %NxVal, ptr @nx__g___main____i87
  %t14607 = extractvalue %NxVal %t14606, 1
  %t14608 = add i64 %t14605, %t14607
  %t14609 = add i64 65535, 0
  %t14610 = and i64 %t14608, %t14609
  %t14611 = call %NxVal @nx_int(i64 %t14610)
  store %NxVal %t14611, ptr @nx__g___main____acc87
  %t14612 = load %NxVal, ptr @nx__g___main____i87
  %t14613 = add i64 1, 0
  %t14614 = extractvalue %NxVal %t14612, 1
  %t14615 = add i64 %t14614, %t14613
  %t14616 = call %NxVal @nx_int(i64 %t14615)
  store %NxVal %t14616, ptr @nx__g___main____i87
  br label %wcond264
wend266:
  %t14617 = load %NxVal, ptr @nx__g___main____total
  %t14618 = load %NxVal, ptr @nx__g___main____acc87
  %t14619 = extractvalue %NxVal %t14617, 1
  %t14620 = extractvalue %NxVal %t14618, 1
  %t14621 = add i64 %t14619, %t14620
  %t14622 = add i64 65535, 0
  %t14623 = and i64 %t14621, %t14622
  %t14624 = call %NxVal @nx_int(i64 %t14623)
  store %NxVal %t14624, ptr @nx__g___main____total
  %t14625 = add i64 0, 0
  %t14626 = call %NxVal @nx_int(i64 %t14625)
  store %NxVal %t14626, ptr @nx__g___main____i88
  %t14627 = add i64 0, 0
  %t14628 = call %NxVal @nx_int(i64 %t14627)
  store %NxVal %t14628, ptr @nx__g___main____acc88
  br label %wcond267
wcond267:
  %t14629 = load %NxVal, ptr @nx__g___main____i88
  %t14630 = add i64 3, 0
  %t14631 = extractvalue %NxVal %t14629, 1
  %t14632 = icmp slt i64 %t14631, %t14630
  br i1 %t14632, label %wbody268, label %wend269
wbody268:
  %t14633 = load %NxVal, ptr @nx__g___main____acc88
  %t14634 = load %NxVal, ptr @nx__g___main____c
  %t14635 = load %NxVal, ptr @nx__g___main____i88
  %t14636 = add i64 88, 0
  %t14637 = extractvalue %NxVal %t14635, 1
  %t14638 = add i64 %t14637, %t14636
  %t14640 = getelementptr [2 x %NxVal], ptr %t14639, i64 0, i64 0
  store %NxVal %t14634, ptr %t14640
  %t14641 = call %NxVal @nx_int(i64 %t14638)
  %t14642 = getelementptr [2 x %NxVal], ptr %t14639, i64 0, i64 1
  store %NxVal %t14641, ptr %t14642
  %t14643 = getelementptr [2 x %NxVal], ptr %t14639, i64 0, i64 0
  %t14644 = call %NxVal @nx__m_3____main____Cell__m88(ptr %t14643, i64 2)
  %t14645 = extractvalue %NxVal %t14644, 1
  %t14646 = extractvalue %NxVal %t14633, 1
  %t14647 = add i64 %t14646, %t14645
  %t14648 = add i64 65535, 0
  %t14649 = and i64 %t14647, %t14648
  %t14650 = call %NxVal @nx_int(i64 %t14649)
  store %NxVal %t14650, ptr @nx__g___main____acc88
  %t14651 = load %NxVal, ptr @nx__g___main____acc88
  %t14652 = add i64 59, 0
  %t14653 = extractvalue %NxVal %t14651, 1
  %t14654 = or i64 %t14653, %t14652
  %t14655 = add i64 4, 0
  %t14656 = load %NxVal, ptr @nx__g___main____i88
  %t14657 = extractvalue %NxVal %t14656, 1
  %t14658 = add i64 %t14655, %t14657
  %t14659 = or i64 %t14654, %t14658
  %t14660 = add i64 65535, 0
  %t14661 = and i64 %t14659, %t14660
  %t14662 = call %NxVal @nx_int(i64 %t14661)
  store %NxVal %t14662, ptr @nx__g___main____acc88
  %t14663 = load %NxVal, ptr @nx__g___main____acc88
  %t14664 = add i64 10, 0
  %t14665 = extractvalue %NxVal %t14663, 1
  %t14666 = add i64 %t14665, %t14664
  %t14667 = add i64 21, 0
  %t14668 = add i64 %t14666, %t14667
  %t14669 = load %NxVal, ptr @nx__g___main____i88
  %t14670 = extractvalue %NxVal %t14669, 1
  %t14671 = add i64 %t14668, %t14670
  %t14672 = add i64 65535, 0
  %t14673 = and i64 %t14671, %t14672
  %t14674 = call %NxVal @nx_int(i64 %t14673)
  store %NxVal %t14674, ptr @nx__g___main____acc88
  %t14675 = load %NxVal, ptr @nx__g___main____acc88
  %t14676 = add i64 63, 0
  %t14677 = extractvalue %NxVal %t14675, 1
  %t14678 = and i64 %t14677, %t14676
  %t14679 = add i64 14, 0
  %t14680 = load %NxVal, ptr @nx__g___main____i88
  %t14681 = extractvalue %NxVal %t14680, 1
  %t14682 = add i64 %t14679, %t14681
  %t14683 = and i64 %t14678, %t14682
  %t14684 = add i64 65535, 0
  %t14685 = and i64 %t14683, %t14684
  %t14686 = call %NxVal @nx_int(i64 %t14685)
  store %NxVal %t14686, ptr @nx__g___main____acc88
  %t14687 = load %NxVal, ptr @nx__g___main____acc88
  %t14688 = add i64 93, 0
  %t14689 = extractvalue %NxVal %t14687, 1
  %t14690 = and i64 %t14689, %t14688
  %t14691 = add i64 61, 0
  %t14692 = load %NxVal, ptr @nx__g___main____i88
  %t14693 = extractvalue %NxVal %t14692, 1
  %t14694 = add i64 %t14691, %t14693
  %t14695 = and i64 %t14690, %t14694
  %t14696 = add i64 65535, 0
  %t14697 = and i64 %t14695, %t14696
  %t14698 = call %NxVal @nx_int(i64 %t14697)
  store %NxVal %t14698, ptr @nx__g___main____acc88
  %t14699 = load %NxVal, ptr @nx__g___main____i88
  %t14700 = add i64 1, 0
  %t14701 = extractvalue %NxVal %t14699, 1
  %t14702 = add i64 %t14701, %t14700
  %t14703 = call %NxVal @nx_int(i64 %t14702)
  store %NxVal %t14703, ptr @nx__g___main____i88
  br label %wcond267
wend269:
  %t14704 = load %NxVal, ptr @nx__g___main____total
  %t14705 = load %NxVal, ptr @nx__g___main____acc88
  %t14706 = extractvalue %NxVal %t14704, 1
  %t14707 = extractvalue %NxVal %t14705, 1
  %t14708 = add i64 %t14706, %t14707
  %t14709 = add i64 65535, 0
  %t14710 = and i64 %t14708, %t14709
  %t14711 = call %NxVal @nx_int(i64 %t14710)
  store %NxVal %t14711, ptr @nx__g___main____total
  %t14712 = add i64 0, 0
  %t14713 = call %NxVal @nx_int(i64 %t14712)
  store %NxVal %t14713, ptr @nx__g___main____i89
  %t14714 = add i64 0, 0
  %t14715 = call %NxVal @nx_int(i64 %t14714)
  store %NxVal %t14715, ptr @nx__g___main____acc89
  br label %wcond270
wcond270:
  %t14716 = load %NxVal, ptr @nx__g___main____i89
  %t14717 = add i64 3, 0
  %t14718 = extractvalue %NxVal %t14716, 1
  %t14719 = icmp slt i64 %t14718, %t14717
  br i1 %t14719, label %wbody271, label %wend272
wbody271:
  %t14720 = load %NxVal, ptr @nx__g___main____acc89
  %t14721 = load %NxVal, ptr @nx__g___main____c
  %t14722 = load %NxVal, ptr @nx__g___main____i89
  %t14723 = add i64 89, 0
  %t14724 = extractvalue %NxVal %t14722, 1
  %t14725 = add i64 %t14724, %t14723
  %t14727 = getelementptr [2 x %NxVal], ptr %t14726, i64 0, i64 0
  store %NxVal %t14721, ptr %t14727
  %t14728 = call %NxVal @nx_int(i64 %t14725)
  %t14729 = getelementptr [2 x %NxVal], ptr %t14726, i64 0, i64 1
  store %NxVal %t14728, ptr %t14729
  %t14730 = getelementptr [2 x %NxVal], ptr %t14726, i64 0, i64 0
  %t14731 = call %NxVal @nx__m_3____main____Cell__m89(ptr %t14730, i64 2)
  %t14732 = extractvalue %NxVal %t14731, 1
  %t14733 = extractvalue %NxVal %t14720, 1
  %t14734 = add i64 %t14733, %t14732
  %t14735 = add i64 65535, 0
  %t14736 = and i64 %t14734, %t14735
  %t14737 = call %NxVal @nx_int(i64 %t14736)
  store %NxVal %t14737, ptr @nx__g___main____acc89
  %t14738 = load %NxVal, ptr @nx__g___main____acc89
  %t14739 = add i64 10, 0
  %t14740 = extractvalue %NxVal %t14738, 1
  %t14741 = and i64 %t14740, %t14739
  %t14742 = add i64 52, 0
  %t14743 = load %NxVal, ptr @nx__g___main____i89
  %t14744 = extractvalue %NxVal %t14743, 1
  %t14745 = add i64 %t14742, %t14744
  %t14746 = and i64 %t14741, %t14745
  %t14747 = add i64 65535, 0
  %t14748 = and i64 %t14746, %t14747
  %t14749 = call %NxVal @nx_int(i64 %t14748)
  store %NxVal %t14749, ptr @nx__g___main____acc89
  %t14750 = load %NxVal, ptr @nx__g___main____acc89
  %t14751 = add i64 5, 0
  %t14752 = extractvalue %NxVal %t14750, 1
  %t14753 = sub i64 %t14752, %t14751
  %t14754 = add i64 39, 0
  %t14755 = sub i64 %t14753, %t14754
  %t14756 = load %NxVal, ptr @nx__g___main____i89
  %t14757 = extractvalue %NxVal %t14756, 1
  %t14758 = add i64 %t14755, %t14757
  %t14759 = add i64 65535, 0
  %t14760 = and i64 %t14758, %t14759
  %t14761 = call %NxVal @nx_int(i64 %t14760)
  store %NxVal %t14761, ptr @nx__g___main____acc89
  %t14762 = load %NxVal, ptr @nx__g___main____acc89
  %t14763 = add i64 69, 0
  %t14764 = extractvalue %NxVal %t14762, 1
  %t14765 = add i64 %t14764, %t14763
  %t14766 = add i64 42, 0
  %t14767 = add i64 %t14765, %t14766
  %t14768 = load %NxVal, ptr @nx__g___main____i89
  %t14769 = extractvalue %NxVal %t14768, 1
  %t14770 = add i64 %t14767, %t14769
  %t14771 = add i64 65535, 0
  %t14772 = and i64 %t14770, %t14771
  %t14773 = call %NxVal @nx_int(i64 %t14772)
  store %NxVal %t14773, ptr @nx__g___main____acc89
  %t14774 = load %NxVal, ptr @nx__g___main____acc89
  %t14775 = add i64 44, 0
  %t14776 = extractvalue %NxVal %t14774, 1
  %t14777 = add i64 %t14776, %t14775
  %t14778 = add i64 69, 0
  %t14779 = add i64 %t14777, %t14778
  %t14780 = load %NxVal, ptr @nx__g___main____i89
  %t14781 = extractvalue %NxVal %t14780, 1
  %t14782 = add i64 %t14779, %t14781
  %t14783 = add i64 65535, 0
  %t14784 = and i64 %t14782, %t14783
  %t14785 = call %NxVal @nx_int(i64 %t14784)
  store %NxVal %t14785, ptr @nx__g___main____acc89
  %t14786 = load %NxVal, ptr @nx__g___main____i89
  %t14787 = add i64 1, 0
  %t14788 = extractvalue %NxVal %t14786, 1
  %t14789 = add i64 %t14788, %t14787
  %t14790 = call %NxVal @nx_int(i64 %t14789)
  store %NxVal %t14790, ptr @nx__g___main____i89
  br label %wcond270
wend272:
  %t14791 = load %NxVal, ptr @nx__g___main____total
  %t14792 = load %NxVal, ptr @nx__g___main____acc89
  %t14793 = extractvalue %NxVal %t14791, 1
  %t14794 = extractvalue %NxVal %t14792, 1
  %t14795 = add i64 %t14793, %t14794
  %t14796 = add i64 65535, 0
  %t14797 = and i64 %t14795, %t14796
  %t14798 = call %NxVal @nx_int(i64 %t14797)
  store %NxVal %t14798, ptr @nx__g___main____total
  %t14799 = add i64 0, 0
  %t14800 = call %NxVal @nx_int(i64 %t14799)
  store %NxVal %t14800, ptr @nx__g___main____i90
  %t14801 = add i64 0, 0
  %t14802 = call %NxVal @nx_int(i64 %t14801)
  store %NxVal %t14802, ptr @nx__g___main____acc90
  br label %wcond273
wcond273:
  %t14803 = load %NxVal, ptr @nx__g___main____i90
  %t14804 = add i64 3, 0
  %t14805 = extractvalue %NxVal %t14803, 1
  %t14806 = icmp slt i64 %t14805, %t14804
  br i1 %t14806, label %wbody274, label %wend275
wbody274:
  %t14807 = load %NxVal, ptr @nx__g___main____acc90
  %t14808 = load %NxVal, ptr @nx__g___main____c
  %t14809 = load %NxVal, ptr @nx__g___main____i90
  %t14810 = add i64 90, 0
  %t14811 = extractvalue %NxVal %t14809, 1
  %t14812 = add i64 %t14811, %t14810
  %t14814 = getelementptr [2 x %NxVal], ptr %t14813, i64 0, i64 0
  store %NxVal %t14808, ptr %t14814
  %t14815 = call %NxVal @nx_int(i64 %t14812)
  %t14816 = getelementptr [2 x %NxVal], ptr %t14813, i64 0, i64 1
  store %NxVal %t14815, ptr %t14816
  %t14817 = getelementptr [2 x %NxVal], ptr %t14813, i64 0, i64 0
  %t14818 = call %NxVal @nx__m_3____main____Cell__m90(ptr %t14817, i64 2)
  %t14819 = extractvalue %NxVal %t14818, 1
  %t14820 = extractvalue %NxVal %t14807, 1
  %t14821 = add i64 %t14820, %t14819
  %t14822 = add i64 65535, 0
  %t14823 = and i64 %t14821, %t14822
  %t14824 = call %NxVal @nx_int(i64 %t14823)
  store %NxVal %t14824, ptr @nx__g___main____acc90
  %t14825 = load %NxVal, ptr @nx__g___main____acc90
  %t14826 = add i64 87, 0
  %t14827 = extractvalue %NxVal %t14825, 1
  %t14828 = xor i64 %t14827, %t14826
  %t14829 = add i64 87, 0
  %t14830 = load %NxVal, ptr @nx__g___main____i90
  %t14831 = extractvalue %NxVal %t14830, 1
  %t14832 = add i64 %t14829, %t14831
  %t14833 = xor i64 %t14828, %t14832
  %t14834 = add i64 65535, 0
  %t14835 = and i64 %t14833, %t14834
  %t14836 = call %NxVal @nx_int(i64 %t14835)
  store %NxVal %t14836, ptr @nx__g___main____acc90
  %t14837 = load %NxVal, ptr @nx__g___main____acc90
  %t14838 = add i64 5, 0
  %t14839 = extractvalue %NxVal %t14837, 1
  %t14840 = add i64 %t14839, %t14838
  %t14841 = add i64 17, 0
  %t14842 = add i64 %t14840, %t14841
  %t14843 = load %NxVal, ptr @nx__g___main____i90
  %t14844 = extractvalue %NxVal %t14843, 1
  %t14845 = add i64 %t14842, %t14844
  %t14846 = add i64 65535, 0
  %t14847 = and i64 %t14845, %t14846
  %t14848 = call %NxVal @nx_int(i64 %t14847)
  store %NxVal %t14848, ptr @nx__g___main____acc90
  %t14849 = load %NxVal, ptr @nx__g___main____acc90
  %t14850 = add i64 55, 0
  %t14851 = extractvalue %NxVal %t14849, 1
  %t14852 = xor i64 %t14851, %t14850
  %t14853 = add i64 68, 0
  %t14854 = load %NxVal, ptr @nx__g___main____i90
  %t14855 = extractvalue %NxVal %t14854, 1
  %t14856 = add i64 %t14853, %t14855
  %t14857 = xor i64 %t14852, %t14856
  %t14858 = add i64 65535, 0
  %t14859 = and i64 %t14857, %t14858
  %t14860 = call %NxVal @nx_int(i64 %t14859)
  store %NxVal %t14860, ptr @nx__g___main____acc90
  %t14861 = load %NxVal, ptr @nx__g___main____acc90
  %t14862 = add i64 61, 0
  %t14863 = extractvalue %NxVal %t14861, 1
  %t14864 = xor i64 %t14863, %t14862
  %t14865 = add i64 81, 0
  %t14866 = load %NxVal, ptr @nx__g___main____i90
  %t14867 = extractvalue %NxVal %t14866, 1
  %t14868 = add i64 %t14865, %t14867
  %t14869 = xor i64 %t14864, %t14868
  %t14870 = add i64 65535, 0
  %t14871 = and i64 %t14869, %t14870
  %t14872 = call %NxVal @nx_int(i64 %t14871)
  store %NxVal %t14872, ptr @nx__g___main____acc90
  %t14873 = load %NxVal, ptr @nx__g___main____i90
  %t14874 = add i64 1, 0
  %t14875 = extractvalue %NxVal %t14873, 1
  %t14876 = add i64 %t14875, %t14874
  %t14877 = call %NxVal @nx_int(i64 %t14876)
  store %NxVal %t14877, ptr @nx__g___main____i90
  br label %wcond273
wend275:
  %t14878 = load %NxVal, ptr @nx__g___main____total
  %t14879 = load %NxVal, ptr @nx__g___main____acc90
  %t14880 = extractvalue %NxVal %t14878, 1
  %t14881 = extractvalue %NxVal %t14879, 1
  %t14882 = add i64 %t14880, %t14881
  %t14883 = add i64 65535, 0
  %t14884 = and i64 %t14882, %t14883
  %t14885 = call %NxVal @nx_int(i64 %t14884)
  store %NxVal %t14885, ptr @nx__g___main____total
  %t14886 = add i64 0, 0
  %t14887 = call %NxVal @nx_int(i64 %t14886)
  store %NxVal %t14887, ptr @nx__g___main____i91
  %t14888 = add i64 0, 0
  %t14889 = call %NxVal @nx_int(i64 %t14888)
  store %NxVal %t14889, ptr @nx__g___main____acc91
  br label %wcond276
wcond276:
  %t14890 = load %NxVal, ptr @nx__g___main____i91
  %t14891 = add i64 3, 0
  %t14892 = extractvalue %NxVal %t14890, 1
  %t14893 = icmp slt i64 %t14892, %t14891
  br i1 %t14893, label %wbody277, label %wend278
wbody277:
  %t14894 = load %NxVal, ptr @nx__g___main____acc91
  %t14895 = load %NxVal, ptr @nx__g___main____c
  %t14896 = load %NxVal, ptr @nx__g___main____i91
  %t14897 = add i64 91, 0
  %t14898 = extractvalue %NxVal %t14896, 1
  %t14899 = add i64 %t14898, %t14897
  %t14901 = getelementptr [2 x %NxVal], ptr %t14900, i64 0, i64 0
  store %NxVal %t14895, ptr %t14901
  %t14902 = call %NxVal @nx_int(i64 %t14899)
  %t14903 = getelementptr [2 x %NxVal], ptr %t14900, i64 0, i64 1
  store %NxVal %t14902, ptr %t14903
  %t14904 = getelementptr [2 x %NxVal], ptr %t14900, i64 0, i64 0
  %t14905 = call %NxVal @nx__m_3____main____Cell__m91(ptr %t14904, i64 2)
  %t14906 = extractvalue %NxVal %t14905, 1
  %t14907 = extractvalue %NxVal %t14894, 1
  %t14908 = add i64 %t14907, %t14906
  %t14909 = add i64 65535, 0
  %t14910 = and i64 %t14908, %t14909
  %t14911 = call %NxVal @nx_int(i64 %t14910)
  store %NxVal %t14911, ptr @nx__g___main____acc91
  %t14912 = load %NxVal, ptr @nx__g___main____acc91
  %t14913 = add i64 19, 0
  %t14914 = extractvalue %NxVal %t14912, 1
  %t14915 = or i64 %t14914, %t14913
  %t14916 = add i64 87, 0
  %t14917 = load %NxVal, ptr @nx__g___main____i91
  %t14918 = extractvalue %NxVal %t14917, 1
  %t14919 = add i64 %t14916, %t14918
  %t14920 = or i64 %t14915, %t14919
  %t14921 = add i64 65535, 0
  %t14922 = and i64 %t14920, %t14921
  %t14923 = call %NxVal @nx_int(i64 %t14922)
  store %NxVal %t14923, ptr @nx__g___main____acc91
  %t14924 = load %NxVal, ptr @nx__g___main____acc91
  %t14925 = add i64 69, 0
  %t14926 = extractvalue %NxVal %t14924, 1
  %t14927 = sub i64 %t14926, %t14925
  %t14928 = add i64 9, 0
  %t14929 = sub i64 %t14927, %t14928
  %t14930 = load %NxVal, ptr @nx__g___main____i91
  %t14931 = extractvalue %NxVal %t14930, 1
  %t14932 = add i64 %t14929, %t14931
  %t14933 = add i64 65535, 0
  %t14934 = and i64 %t14932, %t14933
  %t14935 = call %NxVal @nx_int(i64 %t14934)
  store %NxVal %t14935, ptr @nx__g___main____acc91
  %t14936 = load %NxVal, ptr @nx__g___main____acc91
  %t14937 = add i64 15, 0
  %t14938 = extractvalue %NxVal %t14936, 1
  %t14939 = sub i64 %t14938, %t14937
  %t14940 = add i64 81, 0
  %t14941 = sub i64 %t14939, %t14940
  %t14942 = load %NxVal, ptr @nx__g___main____i91
  %t14943 = extractvalue %NxVal %t14942, 1
  %t14944 = add i64 %t14941, %t14943
  %t14945 = add i64 65535, 0
  %t14946 = and i64 %t14944, %t14945
  %t14947 = call %NxVal @nx_int(i64 %t14946)
  store %NxVal %t14947, ptr @nx__g___main____acc91
  %t14948 = load %NxVal, ptr @nx__g___main____acc91
  %t14949 = add i64 33, 0
  %t14950 = extractvalue %NxVal %t14948, 1
  %t14951 = xor i64 %t14950, %t14949
  %t14952 = add i64 6, 0
  %t14953 = load %NxVal, ptr @nx__g___main____i91
  %t14954 = extractvalue %NxVal %t14953, 1
  %t14955 = add i64 %t14952, %t14954
  %t14956 = xor i64 %t14951, %t14955
  %t14957 = add i64 65535, 0
  %t14958 = and i64 %t14956, %t14957
  %t14959 = call %NxVal @nx_int(i64 %t14958)
  store %NxVal %t14959, ptr @nx__g___main____acc91
  %t14960 = load %NxVal, ptr @nx__g___main____i91
  %t14961 = add i64 1, 0
  %t14962 = extractvalue %NxVal %t14960, 1
  %t14963 = add i64 %t14962, %t14961
  %t14964 = call %NxVal @nx_int(i64 %t14963)
  store %NxVal %t14964, ptr @nx__g___main____i91
  br label %wcond276
wend278:
  %t14965 = load %NxVal, ptr @nx__g___main____total
  %t14966 = load %NxVal, ptr @nx__g___main____acc91
  %t14967 = extractvalue %NxVal %t14965, 1
  %t14968 = extractvalue %NxVal %t14966, 1
  %t14969 = add i64 %t14967, %t14968
  %t14970 = add i64 65535, 0
  %t14971 = and i64 %t14969, %t14970
  %t14972 = call %NxVal @nx_int(i64 %t14971)
  store %NxVal %t14972, ptr @nx__g___main____total
  %t14973 = add i64 0, 0
  %t14974 = call %NxVal @nx_int(i64 %t14973)
  store %NxVal %t14974, ptr @nx__g___main____i92
  %t14975 = add i64 0, 0
  %t14976 = call %NxVal @nx_int(i64 %t14975)
  store %NxVal %t14976, ptr @nx__g___main____acc92
  br label %wcond279
wcond279:
  %t14977 = load %NxVal, ptr @nx__g___main____i92
  %t14978 = add i64 3, 0
  %t14979 = extractvalue %NxVal %t14977, 1
  %t14980 = icmp slt i64 %t14979, %t14978
  br i1 %t14980, label %wbody280, label %wend281
wbody280:
  %t14981 = load %NxVal, ptr @nx__g___main____acc92
  %t14982 = load %NxVal, ptr @nx__g___main____c
  %t14983 = load %NxVal, ptr @nx__g___main____i92
  %t14984 = add i64 92, 0
  %t14985 = extractvalue %NxVal %t14983, 1
  %t14986 = add i64 %t14985, %t14984
  %t14988 = getelementptr [2 x %NxVal], ptr %t14987, i64 0, i64 0
  store %NxVal %t14982, ptr %t14988
  %t14989 = call %NxVal @nx_int(i64 %t14986)
  %t14990 = getelementptr [2 x %NxVal], ptr %t14987, i64 0, i64 1
  store %NxVal %t14989, ptr %t14990
  %t14991 = getelementptr [2 x %NxVal], ptr %t14987, i64 0, i64 0
  %t14992 = call %NxVal @nx__m_3____main____Cell__m92(ptr %t14991, i64 2)
  %t14993 = extractvalue %NxVal %t14992, 1
  %t14994 = extractvalue %NxVal %t14981, 1
  %t14995 = add i64 %t14994, %t14993
  %t14996 = add i64 65535, 0
  %t14997 = and i64 %t14995, %t14996
  %t14998 = call %NxVal @nx_int(i64 %t14997)
  store %NxVal %t14998, ptr @nx__g___main____acc92
  %t14999 = load %NxVal, ptr @nx__g___main____acc92
  %t15000 = add i64 70, 0
  %t15001 = extractvalue %NxVal %t14999, 1
  %t15002 = xor i64 %t15001, %t15000
  %t15003 = add i64 37, 0
  %t15004 = load %NxVal, ptr @nx__g___main____i92
  %t15005 = extractvalue %NxVal %t15004, 1
  %t15006 = add i64 %t15003, %t15005
  %t15007 = xor i64 %t15002, %t15006
  %t15008 = add i64 65535, 0
  %t15009 = and i64 %t15007, %t15008
  %t15010 = call %NxVal @nx_int(i64 %t15009)
  store %NxVal %t15010, ptr @nx__g___main____acc92
  %t15011 = load %NxVal, ptr @nx__g___main____acc92
  %t15012 = add i64 35, 0
  %t15013 = extractvalue %NxVal %t15011, 1
  %t15014 = and i64 %t15013, %t15012
  %t15015 = add i64 1, 0
  %t15016 = load %NxVal, ptr @nx__g___main____i92
  %t15017 = extractvalue %NxVal %t15016, 1
  %t15018 = add i64 %t15015, %t15017
  %t15019 = and i64 %t15014, %t15018
  %t15020 = add i64 65535, 0
  %t15021 = and i64 %t15019, %t15020
  %t15022 = call %NxVal @nx_int(i64 %t15021)
  store %NxVal %t15022, ptr @nx__g___main____acc92
  %t15023 = load %NxVal, ptr @nx__g___main____acc92
  %t15024 = add i64 14, 0
  %t15025 = extractvalue %NxVal %t15023, 1
  %t15026 = and i64 %t15025, %t15024
  %t15027 = add i64 78, 0
  %t15028 = load %NxVal, ptr @nx__g___main____i92
  %t15029 = extractvalue %NxVal %t15028, 1
  %t15030 = add i64 %t15027, %t15029
  %t15031 = and i64 %t15026, %t15030
  %t15032 = add i64 65535, 0
  %t15033 = and i64 %t15031, %t15032
  %t15034 = call %NxVal @nx_int(i64 %t15033)
  store %NxVal %t15034, ptr @nx__g___main____acc92
  %t15035 = load %NxVal, ptr @nx__g___main____acc92
  %t15036 = add i64 41, 0
  %t15037 = extractvalue %NxVal %t15035, 1
  %t15038 = sub i64 %t15037, %t15036
  %t15039 = add i64 46, 0
  %t15040 = sub i64 %t15038, %t15039
  %t15041 = load %NxVal, ptr @nx__g___main____i92
  %t15042 = extractvalue %NxVal %t15041, 1
  %t15043 = add i64 %t15040, %t15042
  %t15044 = add i64 65535, 0
  %t15045 = and i64 %t15043, %t15044
  %t15046 = call %NxVal @nx_int(i64 %t15045)
  store %NxVal %t15046, ptr @nx__g___main____acc92
  %t15047 = load %NxVal, ptr @nx__g___main____i92
  %t15048 = add i64 1, 0
  %t15049 = extractvalue %NxVal %t15047, 1
  %t15050 = add i64 %t15049, %t15048
  %t15051 = call %NxVal @nx_int(i64 %t15050)
  store %NxVal %t15051, ptr @nx__g___main____i92
  br label %wcond279
wend281:
  %t15052 = load %NxVal, ptr @nx__g___main____total
  %t15053 = load %NxVal, ptr @nx__g___main____acc92
  %t15054 = extractvalue %NxVal %t15052, 1
  %t15055 = extractvalue %NxVal %t15053, 1
  %t15056 = add i64 %t15054, %t15055
  %t15057 = add i64 65535, 0
  %t15058 = and i64 %t15056, %t15057
  %t15059 = call %NxVal @nx_int(i64 %t15058)
  store %NxVal %t15059, ptr @nx__g___main____total
  %t15060 = add i64 0, 0
  %t15061 = call %NxVal @nx_int(i64 %t15060)
  store %NxVal %t15061, ptr @nx__g___main____i93
  %t15062 = add i64 0, 0
  %t15063 = call %NxVal @nx_int(i64 %t15062)
  store %NxVal %t15063, ptr @nx__g___main____acc93
  br label %wcond282
wcond282:
  %t15064 = load %NxVal, ptr @nx__g___main____i93
  %t15065 = add i64 3, 0
  %t15066 = extractvalue %NxVal %t15064, 1
  %t15067 = icmp slt i64 %t15066, %t15065
  br i1 %t15067, label %wbody283, label %wend284
wbody283:
  %t15068 = load %NxVal, ptr @nx__g___main____acc93
  %t15069 = load %NxVal, ptr @nx__g___main____c
  %t15070 = load %NxVal, ptr @nx__g___main____i93
  %t15071 = add i64 93, 0
  %t15072 = extractvalue %NxVal %t15070, 1
  %t15073 = add i64 %t15072, %t15071
  %t15075 = getelementptr [2 x %NxVal], ptr %t15074, i64 0, i64 0
  store %NxVal %t15069, ptr %t15075
  %t15076 = call %NxVal @nx_int(i64 %t15073)
  %t15077 = getelementptr [2 x %NxVal], ptr %t15074, i64 0, i64 1
  store %NxVal %t15076, ptr %t15077
  %t15078 = getelementptr [2 x %NxVal], ptr %t15074, i64 0, i64 0
  %t15079 = call %NxVal @nx__m_3____main____Cell__m93(ptr %t15078, i64 2)
  %t15080 = extractvalue %NxVal %t15079, 1
  %t15081 = extractvalue %NxVal %t15068, 1
  %t15082 = add i64 %t15081, %t15080
  %t15083 = add i64 65535, 0
  %t15084 = and i64 %t15082, %t15083
  %t15085 = call %NxVal @nx_int(i64 %t15084)
  store %NxVal %t15085, ptr @nx__g___main____acc93
  %t15086 = load %NxVal, ptr @nx__g___main____acc93
  %t15087 = add i64 91, 0
  %t15088 = extractvalue %NxVal %t15086, 1
  %t15089 = add i64 %t15088, %t15087
  %t15090 = add i64 86, 0
  %t15091 = add i64 %t15089, %t15090
  %t15092 = load %NxVal, ptr @nx__g___main____i93
  %t15093 = extractvalue %NxVal %t15092, 1
  %t15094 = add i64 %t15091, %t15093
  %t15095 = add i64 65535, 0
  %t15096 = and i64 %t15094, %t15095
  %t15097 = call %NxVal @nx_int(i64 %t15096)
  store %NxVal %t15097, ptr @nx__g___main____acc93
  %t15098 = load %NxVal, ptr @nx__g___main____acc93
  %t15099 = add i64 10, 0
  %t15100 = extractvalue %NxVal %t15098, 1
  %t15101 = or i64 %t15100, %t15099
  %t15102 = add i64 7, 0
  %t15103 = load %NxVal, ptr @nx__g___main____i93
  %t15104 = extractvalue %NxVal %t15103, 1
  %t15105 = add i64 %t15102, %t15104
  %t15106 = or i64 %t15101, %t15105
  %t15107 = add i64 65535, 0
  %t15108 = and i64 %t15106, %t15107
  %t15109 = call %NxVal @nx_int(i64 %t15108)
  store %NxVal %t15109, ptr @nx__g___main____acc93
  %t15110 = load %NxVal, ptr @nx__g___main____acc93
  %t15111 = add i64 8, 0
  %t15112 = extractvalue %NxVal %t15110, 1
  %t15113 = and i64 %t15112, %t15111
  %t15114 = add i64 50, 0
  %t15115 = load %NxVal, ptr @nx__g___main____i93
  %t15116 = extractvalue %NxVal %t15115, 1
  %t15117 = add i64 %t15114, %t15116
  %t15118 = and i64 %t15113, %t15117
  %t15119 = add i64 65535, 0
  %t15120 = and i64 %t15118, %t15119
  %t15121 = call %NxVal @nx_int(i64 %t15120)
  store %NxVal %t15121, ptr @nx__g___main____acc93
  %t15122 = load %NxVal, ptr @nx__g___main____acc93
  %t15123 = add i64 95, 0
  %t15124 = extractvalue %NxVal %t15122, 1
  %t15125 = or i64 %t15124, %t15123
  %t15126 = add i64 63, 0
  %t15127 = load %NxVal, ptr @nx__g___main____i93
  %t15128 = extractvalue %NxVal %t15127, 1
  %t15129 = add i64 %t15126, %t15128
  %t15130 = or i64 %t15125, %t15129
  %t15131 = add i64 65535, 0
  %t15132 = and i64 %t15130, %t15131
  %t15133 = call %NxVal @nx_int(i64 %t15132)
  store %NxVal %t15133, ptr @nx__g___main____acc93
  %t15134 = load %NxVal, ptr @nx__g___main____i93
  %t15135 = add i64 1, 0
  %t15136 = extractvalue %NxVal %t15134, 1
  %t15137 = add i64 %t15136, %t15135
  %t15138 = call %NxVal @nx_int(i64 %t15137)
  store %NxVal %t15138, ptr @nx__g___main____i93
  br label %wcond282
wend284:
  %t15139 = load %NxVal, ptr @nx__g___main____total
  %t15140 = load %NxVal, ptr @nx__g___main____acc93
  %t15141 = extractvalue %NxVal %t15139, 1
  %t15142 = extractvalue %NxVal %t15140, 1
  %t15143 = add i64 %t15141, %t15142
  %t15144 = add i64 65535, 0
  %t15145 = and i64 %t15143, %t15144
  %t15146 = call %NxVal @nx_int(i64 %t15145)
  store %NxVal %t15146, ptr @nx__g___main____total
  %t15147 = add i64 0, 0
  %t15148 = call %NxVal @nx_int(i64 %t15147)
  store %NxVal %t15148, ptr @nx__g___main____i94
  %t15149 = add i64 0, 0
  %t15150 = call %NxVal @nx_int(i64 %t15149)
  store %NxVal %t15150, ptr @nx__g___main____acc94
  br label %wcond285
wcond285:
  %t15151 = load %NxVal, ptr @nx__g___main____i94
  %t15152 = add i64 3, 0
  %t15153 = extractvalue %NxVal %t15151, 1
  %t15154 = icmp slt i64 %t15153, %t15152
  br i1 %t15154, label %wbody286, label %wend287
wbody286:
  %t15155 = load %NxVal, ptr @nx__g___main____acc94
  %t15156 = load %NxVal, ptr @nx__g___main____c
  %t15157 = load %NxVal, ptr @nx__g___main____i94
  %t15158 = add i64 94, 0
  %t15159 = extractvalue %NxVal %t15157, 1
  %t15160 = add i64 %t15159, %t15158
  %t15162 = getelementptr [2 x %NxVal], ptr %t15161, i64 0, i64 0
  store %NxVal %t15156, ptr %t15162
  %t15163 = call %NxVal @nx_int(i64 %t15160)
  %t15164 = getelementptr [2 x %NxVal], ptr %t15161, i64 0, i64 1
  store %NxVal %t15163, ptr %t15164
  %t15165 = getelementptr [2 x %NxVal], ptr %t15161, i64 0, i64 0
  %t15166 = call %NxVal @nx__m_3____main____Cell__m94(ptr %t15165, i64 2)
  %t15167 = extractvalue %NxVal %t15166, 1
  %t15168 = extractvalue %NxVal %t15155, 1
  %t15169 = add i64 %t15168, %t15167
  %t15170 = add i64 65535, 0
  %t15171 = and i64 %t15169, %t15170
  %t15172 = call %NxVal @nx_int(i64 %t15171)
  store %NxVal %t15172, ptr @nx__g___main____acc94
  %t15173 = load %NxVal, ptr @nx__g___main____acc94
  %t15174 = add i64 72, 0
  %t15175 = extractvalue %NxVal %t15173, 1
  %t15176 = call i64 @nx_mod_i64(i64 %t15175, i64 %t15174)
  %t15177 = add i64 16, 0
  %t15178 = call i64 @nx_mod_i64(i64 %t15176, i64 %t15177)
  %t15179 = load %NxVal, ptr @nx__g___main____i94
  %t15180 = extractvalue %NxVal %t15179, 1
  %t15181 = add i64 %t15178, %t15180
  %t15182 = add i64 65535, 0
  %t15183 = and i64 %t15181, %t15182
  %t15184 = call %NxVal @nx_int(i64 %t15183)
  store %NxVal %t15184, ptr @nx__g___main____acc94
  %t15185 = load %NxVal, ptr @nx__g___main____acc94
  %t15186 = add i64 17, 0
  %t15187 = extractvalue %NxVal %t15185, 1
  %t15188 = and i64 %t15187, %t15186
  %t15189 = add i64 27, 0
  %t15190 = load %NxVal, ptr @nx__g___main____i94
  %t15191 = extractvalue %NxVal %t15190, 1
  %t15192 = add i64 %t15189, %t15191
  %t15193 = and i64 %t15188, %t15192
  %t15194 = add i64 65535, 0
  %t15195 = and i64 %t15193, %t15194
  %t15196 = call %NxVal @nx_int(i64 %t15195)
  store %NxVal %t15196, ptr @nx__g___main____acc94
  %t15197 = load %NxVal, ptr @nx__g___main____acc94
  %t15198 = add i64 30, 0
  %t15199 = extractvalue %NxVal %t15197, 1
  %t15200 = add i64 %t15199, %t15198
  %t15201 = add i64 61, 0
  %t15202 = add i64 %t15200, %t15201
  %t15203 = load %NxVal, ptr @nx__g___main____i94
  %t15204 = extractvalue %NxVal %t15203, 1
  %t15205 = add i64 %t15202, %t15204
  %t15206 = add i64 65535, 0
  %t15207 = and i64 %t15205, %t15206
  %t15208 = call %NxVal @nx_int(i64 %t15207)
  store %NxVal %t15208, ptr @nx__g___main____acc94
  %t15209 = load %NxVal, ptr @nx__g___main____acc94
  %t15210 = add i64 30, 0
  %t15211 = extractvalue %NxVal %t15209, 1
  %t15212 = mul i64 %t15211, %t15210
  %t15213 = add i64 64, 0
  %t15214 = mul i64 %t15212, %t15213
  %t15215 = load %NxVal, ptr @nx__g___main____i94
  %t15216 = extractvalue %NxVal %t15215, 1
  %t15217 = add i64 %t15214, %t15216
  %t15218 = add i64 65535, 0
  %t15219 = and i64 %t15217, %t15218
  %t15220 = call %NxVal @nx_int(i64 %t15219)
  store %NxVal %t15220, ptr @nx__g___main____acc94
  %t15221 = load %NxVal, ptr @nx__g___main____i94
  %t15222 = add i64 1, 0
  %t15223 = extractvalue %NxVal %t15221, 1
  %t15224 = add i64 %t15223, %t15222
  %t15225 = call %NxVal @nx_int(i64 %t15224)
  store %NxVal %t15225, ptr @nx__g___main____i94
  br label %wcond285
wend287:
  %t15226 = load %NxVal, ptr @nx__g___main____total
  %t15227 = load %NxVal, ptr @nx__g___main____acc94
  %t15228 = extractvalue %NxVal %t15226, 1
  %t15229 = extractvalue %NxVal %t15227, 1
  %t15230 = add i64 %t15228, %t15229
  %t15231 = add i64 65535, 0
  %t15232 = and i64 %t15230, %t15231
  %t15233 = call %NxVal @nx_int(i64 %t15232)
  store %NxVal %t15233, ptr @nx__g___main____total
  %t15234 = add i64 0, 0
  %t15235 = call %NxVal @nx_int(i64 %t15234)
  store %NxVal %t15235, ptr @nx__g___main____i95
  %t15236 = add i64 0, 0
  %t15237 = call %NxVal @nx_int(i64 %t15236)
  store %NxVal %t15237, ptr @nx__g___main____acc95
  br label %wcond288
wcond288:
  %t15238 = load %NxVal, ptr @nx__g___main____i95
  %t15239 = add i64 3, 0
  %t15240 = extractvalue %NxVal %t15238, 1
  %t15241 = icmp slt i64 %t15240, %t15239
  br i1 %t15241, label %wbody289, label %wend290
wbody289:
  %t15242 = load %NxVal, ptr @nx__g___main____acc95
  %t15243 = load %NxVal, ptr @nx__g___main____c
  %t15244 = load %NxVal, ptr @nx__g___main____i95
  %t15245 = add i64 95, 0
  %t15246 = extractvalue %NxVal %t15244, 1
  %t15247 = add i64 %t15246, %t15245
  %t15249 = getelementptr [2 x %NxVal], ptr %t15248, i64 0, i64 0
  store %NxVal %t15243, ptr %t15249
  %t15250 = call %NxVal @nx_int(i64 %t15247)
  %t15251 = getelementptr [2 x %NxVal], ptr %t15248, i64 0, i64 1
  store %NxVal %t15250, ptr %t15251
  %t15252 = getelementptr [2 x %NxVal], ptr %t15248, i64 0, i64 0
  %t15253 = call %NxVal @nx__m_3____main____Cell__m95(ptr %t15252, i64 2)
  %t15254 = extractvalue %NxVal %t15253, 1
  %t15255 = extractvalue %NxVal %t15242, 1
  %t15256 = add i64 %t15255, %t15254
  %t15257 = add i64 65535, 0
  %t15258 = and i64 %t15256, %t15257
  %t15259 = call %NxVal @nx_int(i64 %t15258)
  store %NxVal %t15259, ptr @nx__g___main____acc95
  %t15260 = load %NxVal, ptr @nx__g___main____acc95
  %t15261 = add i64 5, 0
  %t15262 = extractvalue %NxVal %t15260, 1
  %t15263 = sub i64 %t15262, %t15261
  %t15264 = add i64 25, 0
  %t15265 = sub i64 %t15263, %t15264
  %t15266 = load %NxVal, ptr @nx__g___main____i95
  %t15267 = extractvalue %NxVal %t15266, 1
  %t15268 = add i64 %t15265, %t15267
  %t15269 = add i64 65535, 0
  %t15270 = and i64 %t15268, %t15269
  %t15271 = call %NxVal @nx_int(i64 %t15270)
  store %NxVal %t15271, ptr @nx__g___main____acc95
  %t15272 = load %NxVal, ptr @nx__g___main____acc95
  %t15273 = add i64 31, 0
  %t15274 = extractvalue %NxVal %t15272, 1
  %t15275 = and i64 %t15274, %t15273
  %t15276 = add i64 10, 0
  %t15277 = load %NxVal, ptr @nx__g___main____i95
  %t15278 = extractvalue %NxVal %t15277, 1
  %t15279 = add i64 %t15276, %t15278
  %t15280 = and i64 %t15275, %t15279
  %t15281 = add i64 65535, 0
  %t15282 = and i64 %t15280, %t15281
  %t15283 = call %NxVal @nx_int(i64 %t15282)
  store %NxVal %t15283, ptr @nx__g___main____acc95
  %t15284 = load %NxVal, ptr @nx__g___main____acc95
  %t15285 = add i64 59, 0
  %t15286 = extractvalue %NxVal %t15284, 1
  %t15287 = and i64 %t15286, %t15285
  %t15288 = add i64 25, 0
  %t15289 = load %NxVal, ptr @nx__g___main____i95
  %t15290 = extractvalue %NxVal %t15289, 1
  %t15291 = add i64 %t15288, %t15290
  %t15292 = and i64 %t15287, %t15291
  %t15293 = add i64 65535, 0
  %t15294 = and i64 %t15292, %t15293
  %t15295 = call %NxVal @nx_int(i64 %t15294)
  store %NxVal %t15295, ptr @nx__g___main____acc95
  %t15296 = load %NxVal, ptr @nx__g___main____acc95
  %t15297 = add i64 76, 0
  %t15298 = extractvalue %NxVal %t15296, 1
  %t15299 = call i64 @nx_mod_i64(i64 %t15298, i64 %t15297)
  %t15300 = add i64 4, 0
  %t15301 = call i64 @nx_mod_i64(i64 %t15299, i64 %t15300)
  %t15302 = load %NxVal, ptr @nx__g___main____i95
  %t15303 = extractvalue %NxVal %t15302, 1
  %t15304 = add i64 %t15301, %t15303
  %t15305 = add i64 65535, 0
  %t15306 = and i64 %t15304, %t15305
  %t15307 = call %NxVal @nx_int(i64 %t15306)
  store %NxVal %t15307, ptr @nx__g___main____acc95
  %t15308 = load %NxVal, ptr @nx__g___main____i95
  %t15309 = add i64 1, 0
  %t15310 = extractvalue %NxVal %t15308, 1
  %t15311 = add i64 %t15310, %t15309
  %t15312 = call %NxVal @nx_int(i64 %t15311)
  store %NxVal %t15312, ptr @nx__g___main____i95
  br label %wcond288
wend290:
  %t15313 = load %NxVal, ptr @nx__g___main____total
  %t15314 = load %NxVal, ptr @nx__g___main____acc95
  %t15315 = extractvalue %NxVal %t15313, 1
  %t15316 = extractvalue %NxVal %t15314, 1
  %t15317 = add i64 %t15315, %t15316
  %t15318 = add i64 65535, 0
  %t15319 = and i64 %t15317, %t15318
  %t15320 = call %NxVal @nx_int(i64 %t15319)
  store %NxVal %t15320, ptr @nx__g___main____total
  %t15321 = add i64 0, 0
  %t15322 = call %NxVal @nx_int(i64 %t15321)
  store %NxVal %t15322, ptr @nx__g___main____i96
  %t15323 = add i64 0, 0
  %t15324 = call %NxVal @nx_int(i64 %t15323)
  store %NxVal %t15324, ptr @nx__g___main____acc96
  br label %wcond291
wcond291:
  %t15325 = load %NxVal, ptr @nx__g___main____i96
  %t15326 = add i64 3, 0
  %t15327 = extractvalue %NxVal %t15325, 1
  %t15328 = icmp slt i64 %t15327, %t15326
  br i1 %t15328, label %wbody292, label %wend293
wbody292:
  %t15329 = load %NxVal, ptr @nx__g___main____acc96
  %t15330 = load %NxVal, ptr @nx__g___main____c
  %t15331 = load %NxVal, ptr @nx__g___main____i96
  %t15332 = add i64 96, 0
  %t15333 = extractvalue %NxVal %t15331, 1
  %t15334 = add i64 %t15333, %t15332
  %t15336 = getelementptr [2 x %NxVal], ptr %t15335, i64 0, i64 0
  store %NxVal %t15330, ptr %t15336
  %t15337 = call %NxVal @nx_int(i64 %t15334)
  %t15338 = getelementptr [2 x %NxVal], ptr %t15335, i64 0, i64 1
  store %NxVal %t15337, ptr %t15338
  %t15339 = getelementptr [2 x %NxVal], ptr %t15335, i64 0, i64 0
  %t15340 = call %NxVal @nx__m_3____main____Cell__m96(ptr %t15339, i64 2)
  %t15341 = extractvalue %NxVal %t15340, 1
  %t15342 = extractvalue %NxVal %t15329, 1
  %t15343 = add i64 %t15342, %t15341
  %t15344 = add i64 65535, 0
  %t15345 = and i64 %t15343, %t15344
  %t15346 = call %NxVal @nx_int(i64 %t15345)
  store %NxVal %t15346, ptr @nx__g___main____acc96
  %t15347 = load %NxVal, ptr @nx__g___main____acc96
  %t15348 = add i64 50, 0
  %t15349 = extractvalue %NxVal %t15347, 1
  %t15350 = and i64 %t15349, %t15348
  %t15351 = add i64 48, 0
  %t15352 = load %NxVal, ptr @nx__g___main____i96
  %t15353 = extractvalue %NxVal %t15352, 1
  %t15354 = add i64 %t15351, %t15353
  %t15355 = and i64 %t15350, %t15354
  %t15356 = add i64 65535, 0
  %t15357 = and i64 %t15355, %t15356
  %t15358 = call %NxVal @nx_int(i64 %t15357)
  store %NxVal %t15358, ptr @nx__g___main____acc96
  %t15359 = load %NxVal, ptr @nx__g___main____acc96
  %t15360 = add i64 26, 0
  %t15361 = extractvalue %NxVal %t15359, 1
  %t15362 = add i64 %t15361, %t15360
  %t15363 = add i64 62, 0
  %t15364 = add i64 %t15362, %t15363
  %t15365 = load %NxVal, ptr @nx__g___main____i96
  %t15366 = extractvalue %NxVal %t15365, 1
  %t15367 = add i64 %t15364, %t15366
  %t15368 = add i64 65535, 0
  %t15369 = and i64 %t15367, %t15368
  %t15370 = call %NxVal @nx_int(i64 %t15369)
  store %NxVal %t15370, ptr @nx__g___main____acc96
  %t15371 = load %NxVal, ptr @nx__g___main____acc96
  %t15372 = add i64 40, 0
  %t15373 = extractvalue %NxVal %t15371, 1
  %t15374 = or i64 %t15373, %t15372
  %t15375 = add i64 24, 0
  %t15376 = load %NxVal, ptr @nx__g___main____i96
  %t15377 = extractvalue %NxVal %t15376, 1
  %t15378 = add i64 %t15375, %t15377
  %t15379 = or i64 %t15374, %t15378
  %t15380 = add i64 65535, 0
  %t15381 = and i64 %t15379, %t15380
  %t15382 = call %NxVal @nx_int(i64 %t15381)
  store %NxVal %t15382, ptr @nx__g___main____acc96
  %t15383 = load %NxVal, ptr @nx__g___main____acc96
  %t15384 = add i64 63, 0
  %t15385 = extractvalue %NxVal %t15383, 1
  %t15386 = xor i64 %t15385, %t15384
  %t15387 = add i64 86, 0
  %t15388 = load %NxVal, ptr @nx__g___main____i96
  %t15389 = extractvalue %NxVal %t15388, 1
  %t15390 = add i64 %t15387, %t15389
  %t15391 = xor i64 %t15386, %t15390
  %t15392 = add i64 65535, 0
  %t15393 = and i64 %t15391, %t15392
  %t15394 = call %NxVal @nx_int(i64 %t15393)
  store %NxVal %t15394, ptr @nx__g___main____acc96
  %t15395 = load %NxVal, ptr @nx__g___main____i96
  %t15396 = add i64 1, 0
  %t15397 = extractvalue %NxVal %t15395, 1
  %t15398 = add i64 %t15397, %t15396
  %t15399 = call %NxVal @nx_int(i64 %t15398)
  store %NxVal %t15399, ptr @nx__g___main____i96
  br label %wcond291
wend293:
  %t15400 = load %NxVal, ptr @nx__g___main____total
  %t15401 = load %NxVal, ptr @nx__g___main____acc96
  %t15402 = extractvalue %NxVal %t15400, 1
  %t15403 = extractvalue %NxVal %t15401, 1
  %t15404 = add i64 %t15402, %t15403
  %t15405 = add i64 65535, 0
  %t15406 = and i64 %t15404, %t15405
  %t15407 = call %NxVal @nx_int(i64 %t15406)
  store %NxVal %t15407, ptr @nx__g___main____total
  %t15408 = add i64 0, 0
  %t15409 = call %NxVal @nx_int(i64 %t15408)
  store %NxVal %t15409, ptr @nx__g___main____i97
  %t15410 = add i64 0, 0
  %t15411 = call %NxVal @nx_int(i64 %t15410)
  store %NxVal %t15411, ptr @nx__g___main____acc97
  br label %wcond294
wcond294:
  %t15412 = load %NxVal, ptr @nx__g___main____i97
  %t15413 = add i64 3, 0
  %t15414 = extractvalue %NxVal %t15412, 1
  %t15415 = icmp slt i64 %t15414, %t15413
  br i1 %t15415, label %wbody295, label %wend296
wbody295:
  %t15416 = load %NxVal, ptr @nx__g___main____acc97
  %t15417 = load %NxVal, ptr @nx__g___main____c
  %t15418 = load %NxVal, ptr @nx__g___main____i97
  %t15419 = add i64 97, 0
  %t15420 = extractvalue %NxVal %t15418, 1
  %t15421 = add i64 %t15420, %t15419
  %t15423 = getelementptr [2 x %NxVal], ptr %t15422, i64 0, i64 0
  store %NxVal %t15417, ptr %t15423
  %t15424 = call %NxVal @nx_int(i64 %t15421)
  %t15425 = getelementptr [2 x %NxVal], ptr %t15422, i64 0, i64 1
  store %NxVal %t15424, ptr %t15425
  %t15426 = getelementptr [2 x %NxVal], ptr %t15422, i64 0, i64 0
  %t15427 = call %NxVal @nx__m_3____main____Cell__m97(ptr %t15426, i64 2)
  %t15428 = extractvalue %NxVal %t15427, 1
  %t15429 = extractvalue %NxVal %t15416, 1
  %t15430 = add i64 %t15429, %t15428
  %t15431 = add i64 65535, 0
  %t15432 = and i64 %t15430, %t15431
  %t15433 = call %NxVal @nx_int(i64 %t15432)
  store %NxVal %t15433, ptr @nx__g___main____acc97
  %t15434 = load %NxVal, ptr @nx__g___main____acc97
  %t15435 = add i64 53, 0
  %t15436 = extractvalue %NxVal %t15434, 1
  %t15437 = and i64 %t15436, %t15435
  %t15438 = add i64 6, 0
  %t15439 = load %NxVal, ptr @nx__g___main____i97
  %t15440 = extractvalue %NxVal %t15439, 1
  %t15441 = add i64 %t15438, %t15440
  %t15442 = and i64 %t15437, %t15441
  %t15443 = add i64 65535, 0
  %t15444 = and i64 %t15442, %t15443
  %t15445 = call %NxVal @nx_int(i64 %t15444)
  store %NxVal %t15445, ptr @nx__g___main____acc97
  %t15446 = load %NxVal, ptr @nx__g___main____acc97
  %t15447 = add i64 87, 0
  %t15448 = extractvalue %NxVal %t15446, 1
  %t15449 = sub i64 %t15448, %t15447
  %t15450 = add i64 87, 0
  %t15451 = sub i64 %t15449, %t15450
  %t15452 = load %NxVal, ptr @nx__g___main____i97
  %t15453 = extractvalue %NxVal %t15452, 1
  %t15454 = add i64 %t15451, %t15453
  %t15455 = add i64 65535, 0
  %t15456 = and i64 %t15454, %t15455
  %t15457 = call %NxVal @nx_int(i64 %t15456)
  store %NxVal %t15457, ptr @nx__g___main____acc97
  %t15458 = load %NxVal, ptr @nx__g___main____acc97
  %t15459 = add i64 1, 0
  %t15460 = extractvalue %NxVal %t15458, 1
  %t15461 = call i64 @nx_mod_i64(i64 %t15460, i64 %t15459)
  %t15462 = add i64 17, 0
  %t15463 = call i64 @nx_mod_i64(i64 %t15461, i64 %t15462)
  %t15464 = load %NxVal, ptr @nx__g___main____i97
  %t15465 = extractvalue %NxVal %t15464, 1
  %t15466 = add i64 %t15463, %t15465
  %t15467 = add i64 65535, 0
  %t15468 = and i64 %t15466, %t15467
  %t15469 = call %NxVal @nx_int(i64 %t15468)
  store %NxVal %t15469, ptr @nx__g___main____acc97
  %t15470 = load %NxVal, ptr @nx__g___main____acc97
  %t15471 = add i64 36, 0
  %t15472 = extractvalue %NxVal %t15470, 1
  %t15473 = mul i64 %t15472, %t15471
  %t15474 = add i64 2, 0
  %t15475 = mul i64 %t15473, %t15474
  %t15476 = load %NxVal, ptr @nx__g___main____i97
  %t15477 = extractvalue %NxVal %t15476, 1
  %t15478 = add i64 %t15475, %t15477
  %t15479 = add i64 65535, 0
  %t15480 = and i64 %t15478, %t15479
  %t15481 = call %NxVal @nx_int(i64 %t15480)
  store %NxVal %t15481, ptr @nx__g___main____acc97
  %t15482 = load %NxVal, ptr @nx__g___main____i97
  %t15483 = add i64 1, 0
  %t15484 = extractvalue %NxVal %t15482, 1
  %t15485 = add i64 %t15484, %t15483
  %t15486 = call %NxVal @nx_int(i64 %t15485)
  store %NxVal %t15486, ptr @nx__g___main____i97
  br label %wcond294
wend296:
  %t15487 = load %NxVal, ptr @nx__g___main____total
  %t15488 = load %NxVal, ptr @nx__g___main____acc97
  %t15489 = extractvalue %NxVal %t15487, 1
  %t15490 = extractvalue %NxVal %t15488, 1
  %t15491 = add i64 %t15489, %t15490
  %t15492 = add i64 65535, 0
  %t15493 = and i64 %t15491, %t15492
  %t15494 = call %NxVal @nx_int(i64 %t15493)
  store %NxVal %t15494, ptr @nx__g___main____total
  %t15495 = add i64 0, 0
  %t15496 = call %NxVal @nx_int(i64 %t15495)
  store %NxVal %t15496, ptr @nx__g___main____i98
  %t15497 = add i64 0, 0
  %t15498 = call %NxVal @nx_int(i64 %t15497)
  store %NxVal %t15498, ptr @nx__g___main____acc98
  br label %wcond297
wcond297:
  %t15499 = load %NxVal, ptr @nx__g___main____i98
  %t15500 = add i64 3, 0
  %t15501 = extractvalue %NxVal %t15499, 1
  %t15502 = icmp slt i64 %t15501, %t15500
  br i1 %t15502, label %wbody298, label %wend299
wbody298:
  %t15503 = load %NxVal, ptr @nx__g___main____acc98
  %t15504 = load %NxVal, ptr @nx__g___main____c
  %t15505 = load %NxVal, ptr @nx__g___main____i98
  %t15506 = add i64 98, 0
  %t15507 = extractvalue %NxVal %t15505, 1
  %t15508 = add i64 %t15507, %t15506
  %t15510 = getelementptr [2 x %NxVal], ptr %t15509, i64 0, i64 0
  store %NxVal %t15504, ptr %t15510
  %t15511 = call %NxVal @nx_int(i64 %t15508)
  %t15512 = getelementptr [2 x %NxVal], ptr %t15509, i64 0, i64 1
  store %NxVal %t15511, ptr %t15512
  %t15513 = getelementptr [2 x %NxVal], ptr %t15509, i64 0, i64 0
  %t15514 = call %NxVal @nx__m_3____main____Cell__m98(ptr %t15513, i64 2)
  %t15515 = extractvalue %NxVal %t15514, 1
  %t15516 = extractvalue %NxVal %t15503, 1
  %t15517 = add i64 %t15516, %t15515
  %t15518 = add i64 65535, 0
  %t15519 = and i64 %t15517, %t15518
  %t15520 = call %NxVal @nx_int(i64 %t15519)
  store %NxVal %t15520, ptr @nx__g___main____acc98
  %t15521 = load %NxVal, ptr @nx__g___main____acc98
  %t15522 = add i64 5, 0
  %t15523 = extractvalue %NxVal %t15521, 1
  %t15524 = mul i64 %t15523, %t15522
  %t15525 = add i64 31, 0
  %t15526 = mul i64 %t15524, %t15525
  %t15527 = load %NxVal, ptr @nx__g___main____i98
  %t15528 = extractvalue %NxVal %t15527, 1
  %t15529 = add i64 %t15526, %t15528
  %t15530 = add i64 65535, 0
  %t15531 = and i64 %t15529, %t15530
  %t15532 = call %NxVal @nx_int(i64 %t15531)
  store %NxVal %t15532, ptr @nx__g___main____acc98
  %t15533 = load %NxVal, ptr @nx__g___main____acc98
  %t15534 = add i64 2, 0
  %t15535 = extractvalue %NxVal %t15533, 1
  %t15536 = and i64 %t15535, %t15534
  %t15537 = add i64 31, 0
  %t15538 = load %NxVal, ptr @nx__g___main____i98
  %t15539 = extractvalue %NxVal %t15538, 1
  %t15540 = add i64 %t15537, %t15539
  %t15541 = and i64 %t15536, %t15540
  %t15542 = add i64 65535, 0
  %t15543 = and i64 %t15541, %t15542
  %t15544 = call %NxVal @nx_int(i64 %t15543)
  store %NxVal %t15544, ptr @nx__g___main____acc98
  %t15545 = load %NxVal, ptr @nx__g___main____acc98
  %t15546 = add i64 12, 0
  %t15547 = extractvalue %NxVal %t15545, 1
  %t15548 = sub i64 %t15547, %t15546
  %t15549 = add i64 52, 0
  %t15550 = sub i64 %t15548, %t15549
  %t15551 = load %NxVal, ptr @nx__g___main____i98
  %t15552 = extractvalue %NxVal %t15551, 1
  %t15553 = add i64 %t15550, %t15552
  %t15554 = add i64 65535, 0
  %t15555 = and i64 %t15553, %t15554
  %t15556 = call %NxVal @nx_int(i64 %t15555)
  store %NxVal %t15556, ptr @nx__g___main____acc98
  %t15557 = load %NxVal, ptr @nx__g___main____acc98
  %t15558 = add i64 19, 0
  %t15559 = extractvalue %NxVal %t15557, 1
  %t15560 = add i64 %t15559, %t15558
  %t15561 = add i64 63, 0
  %t15562 = add i64 %t15560, %t15561
  %t15563 = load %NxVal, ptr @nx__g___main____i98
  %t15564 = extractvalue %NxVal %t15563, 1
  %t15565 = add i64 %t15562, %t15564
  %t15566 = add i64 65535, 0
  %t15567 = and i64 %t15565, %t15566
  %t15568 = call %NxVal @nx_int(i64 %t15567)
  store %NxVal %t15568, ptr @nx__g___main____acc98
  %t15569 = load %NxVal, ptr @nx__g___main____i98
  %t15570 = add i64 1, 0
  %t15571 = extractvalue %NxVal %t15569, 1
  %t15572 = add i64 %t15571, %t15570
  %t15573 = call %NxVal @nx_int(i64 %t15572)
  store %NxVal %t15573, ptr @nx__g___main____i98
  br label %wcond297
wend299:
  %t15574 = load %NxVal, ptr @nx__g___main____total
  %t15575 = load %NxVal, ptr @nx__g___main____acc98
  %t15576 = extractvalue %NxVal %t15574, 1
  %t15577 = extractvalue %NxVal %t15575, 1
  %t15578 = add i64 %t15576, %t15577
  %t15579 = add i64 65535, 0
  %t15580 = and i64 %t15578, %t15579
  %t15581 = call %NxVal @nx_int(i64 %t15580)
  store %NxVal %t15581, ptr @nx__g___main____total
  %t15582 = add i64 0, 0
  %t15583 = call %NxVal @nx_int(i64 %t15582)
  store %NxVal %t15583, ptr @nx__g___main____i99
  %t15584 = add i64 0, 0
  %t15585 = call %NxVal @nx_int(i64 %t15584)
  store %NxVal %t15585, ptr @nx__g___main____acc99
  br label %wcond300
wcond300:
  %t15586 = load %NxVal, ptr @nx__g___main____i99
  %t15587 = add i64 3, 0
  %t15588 = extractvalue %NxVal %t15586, 1
  %t15589 = icmp slt i64 %t15588, %t15587
  br i1 %t15589, label %wbody301, label %wend302
wbody301:
  %t15590 = load %NxVal, ptr @nx__g___main____acc99
  %t15591 = load %NxVal, ptr @nx__g___main____c
  %t15592 = load %NxVal, ptr @nx__g___main____i99
  %t15593 = add i64 99, 0
  %t15594 = extractvalue %NxVal %t15592, 1
  %t15595 = add i64 %t15594, %t15593
  %t15597 = getelementptr [2 x %NxVal], ptr %t15596, i64 0, i64 0
  store %NxVal %t15591, ptr %t15597
  %t15598 = call %NxVal @nx_int(i64 %t15595)
  %t15599 = getelementptr [2 x %NxVal], ptr %t15596, i64 0, i64 1
  store %NxVal %t15598, ptr %t15599
  %t15600 = getelementptr [2 x %NxVal], ptr %t15596, i64 0, i64 0
  %t15601 = call %NxVal @nx__m_3____main____Cell__m99(ptr %t15600, i64 2)
  %t15602 = extractvalue %NxVal %t15601, 1
  %t15603 = extractvalue %NxVal %t15590, 1
  %t15604 = add i64 %t15603, %t15602
  %t15605 = add i64 65535, 0
  %t15606 = and i64 %t15604, %t15605
  %t15607 = call %NxVal @nx_int(i64 %t15606)
  store %NxVal %t15607, ptr @nx__g___main____acc99
  %t15608 = load %NxVal, ptr @nx__g___main____acc99
  %t15609 = add i64 59, 0
  %t15610 = extractvalue %NxVal %t15608, 1
  %t15611 = sub i64 %t15610, %t15609
  %t15612 = add i64 37, 0
  %t15613 = sub i64 %t15611, %t15612
  %t15614 = load %NxVal, ptr @nx__g___main____i99
  %t15615 = extractvalue %NxVal %t15614, 1
  %t15616 = add i64 %t15613, %t15615
  %t15617 = add i64 65535, 0
  %t15618 = and i64 %t15616, %t15617
  %t15619 = call %NxVal @nx_int(i64 %t15618)
  store %NxVal %t15619, ptr @nx__g___main____acc99
  %t15620 = load %NxVal, ptr @nx__g___main____acc99
  %t15621 = add i64 58, 0
  %t15622 = extractvalue %NxVal %t15620, 1
  %t15623 = xor i64 %t15622, %t15621
  %t15624 = add i64 67, 0
  %t15625 = load %NxVal, ptr @nx__g___main____i99
  %t15626 = extractvalue %NxVal %t15625, 1
  %t15627 = add i64 %t15624, %t15626
  %t15628 = xor i64 %t15623, %t15627
  %t15629 = add i64 65535, 0
  %t15630 = and i64 %t15628, %t15629
  %t15631 = call %NxVal @nx_int(i64 %t15630)
  store %NxVal %t15631, ptr @nx__g___main____acc99
  %t15632 = load %NxVal, ptr @nx__g___main____acc99
  %t15633 = add i64 73, 0
  %t15634 = extractvalue %NxVal %t15632, 1
  %t15635 = sub i64 %t15634, %t15633
  %t15636 = add i64 57, 0
  %t15637 = sub i64 %t15635, %t15636
  %t15638 = load %NxVal, ptr @nx__g___main____i99
  %t15639 = extractvalue %NxVal %t15638, 1
  %t15640 = add i64 %t15637, %t15639
  %t15641 = add i64 65535, 0
  %t15642 = and i64 %t15640, %t15641
  %t15643 = call %NxVal @nx_int(i64 %t15642)
  store %NxVal %t15643, ptr @nx__g___main____acc99
  %t15644 = load %NxVal, ptr @nx__g___main____acc99
  %t15645 = add i64 61, 0
  %t15646 = extractvalue %NxVal %t15644, 1
  %t15647 = sub i64 %t15646, %t15645
  %t15648 = add i64 46, 0
  %t15649 = sub i64 %t15647, %t15648
  %t15650 = load %NxVal, ptr @nx__g___main____i99
  %t15651 = extractvalue %NxVal %t15650, 1
  %t15652 = add i64 %t15649, %t15651
  %t15653 = add i64 65535, 0
  %t15654 = and i64 %t15652, %t15653
  %t15655 = call %NxVal @nx_int(i64 %t15654)
  store %NxVal %t15655, ptr @nx__g___main____acc99
  %t15656 = load %NxVal, ptr @nx__g___main____i99
  %t15657 = add i64 1, 0
  %t15658 = extractvalue %NxVal %t15656, 1
  %t15659 = add i64 %t15658, %t15657
  %t15660 = call %NxVal @nx_int(i64 %t15659)
  store %NxVal %t15660, ptr @nx__g___main____i99
  br label %wcond300
wend302:
  %t15661 = load %NxVal, ptr @nx__g___main____total
  %t15662 = load %NxVal, ptr @nx__g___main____acc99
  %t15663 = extractvalue %NxVal %t15661, 1
  %t15664 = extractvalue %NxVal %t15662, 1
  %t15665 = add i64 %t15663, %t15664
  %t15666 = add i64 65535, 0
  %t15667 = and i64 %t15665, %t15666
  %t15668 = call %NxVal @nx_int(i64 %t15667)
  store %NxVal %t15668, ptr @nx__g___main____total
  %t15669 = add i64 0, 0
  %t15670 = call %NxVal @nx_int(i64 %t15669)
  store %NxVal %t15670, ptr @nx__g___main____i100
  %t15671 = add i64 0, 0
  %t15672 = call %NxVal @nx_int(i64 %t15671)
  store %NxVal %t15672, ptr @nx__g___main____acc100
  br label %wcond303
wcond303:
  %t15673 = load %NxVal, ptr @nx__g___main____i100
  %t15674 = add i64 3, 0
  %t15675 = extractvalue %NxVal %t15673, 1
  %t15676 = icmp slt i64 %t15675, %t15674
  br i1 %t15676, label %wbody304, label %wend305
wbody304:
  %t15677 = load %NxVal, ptr @nx__g___main____acc100
  %t15678 = load %NxVal, ptr @nx__g___main____c
  %t15679 = load %NxVal, ptr @nx__g___main____i100
  %t15680 = add i64 100, 0
  %t15681 = extractvalue %NxVal %t15679, 1
  %t15682 = add i64 %t15681, %t15680
  %t15684 = getelementptr [2 x %NxVal], ptr %t15683, i64 0, i64 0
  store %NxVal %t15678, ptr %t15684
  %t15685 = call %NxVal @nx_int(i64 %t15682)
  %t15686 = getelementptr [2 x %NxVal], ptr %t15683, i64 0, i64 1
  store %NxVal %t15685, ptr %t15686
  %t15687 = getelementptr [2 x %NxVal], ptr %t15683, i64 0, i64 0
  %t15688 = call %NxVal @nx__m_4____main____Cell__m100(ptr %t15687, i64 2)
  %t15689 = extractvalue %NxVal %t15688, 1
  %t15690 = extractvalue %NxVal %t15677, 1
  %t15691 = add i64 %t15690, %t15689
  %t15692 = add i64 65535, 0
  %t15693 = and i64 %t15691, %t15692
  %t15694 = call %NxVal @nx_int(i64 %t15693)
  store %NxVal %t15694, ptr @nx__g___main____acc100
  %t15695 = load %NxVal, ptr @nx__g___main____acc100
  %t15696 = add i64 62, 0
  %t15697 = extractvalue %NxVal %t15695, 1
  %t15698 = and i64 %t15697, %t15696
  %t15699 = add i64 57, 0
  %t15700 = load %NxVal, ptr @nx__g___main____i100
  %t15701 = extractvalue %NxVal %t15700, 1
  %t15702 = add i64 %t15699, %t15701
  %t15703 = and i64 %t15698, %t15702
  %t15704 = add i64 65535, 0
  %t15705 = and i64 %t15703, %t15704
  %t15706 = call %NxVal @nx_int(i64 %t15705)
  store %NxVal %t15706, ptr @nx__g___main____acc100
  %t15707 = load %NxVal, ptr @nx__g___main____acc100
  %t15708 = add i64 78, 0
  %t15709 = extractvalue %NxVal %t15707, 1
  %t15710 = and i64 %t15709, %t15708
  %t15711 = add i64 58, 0
  %t15712 = load %NxVal, ptr @nx__g___main____i100
  %t15713 = extractvalue %NxVal %t15712, 1
  %t15714 = add i64 %t15711, %t15713
  %t15715 = and i64 %t15710, %t15714
  %t15716 = add i64 65535, 0
  %t15717 = and i64 %t15715, %t15716
  %t15718 = call %NxVal @nx_int(i64 %t15717)
  store %NxVal %t15718, ptr @nx__g___main____acc100
  %t15719 = load %NxVal, ptr @nx__g___main____acc100
  %t15720 = add i64 12, 0
  %t15721 = extractvalue %NxVal %t15719, 1
  %t15722 = add i64 %t15721, %t15720
  %t15723 = add i64 82, 0
  %t15724 = add i64 %t15722, %t15723
  %t15725 = load %NxVal, ptr @nx__g___main____i100
  %t15726 = extractvalue %NxVal %t15725, 1
  %t15727 = add i64 %t15724, %t15726
  %t15728 = add i64 65535, 0
  %t15729 = and i64 %t15727, %t15728
  %t15730 = call %NxVal @nx_int(i64 %t15729)
  store %NxVal %t15730, ptr @nx__g___main____acc100
  %t15731 = load %NxVal, ptr @nx__g___main____acc100
  %t15732 = add i64 60, 0
  %t15733 = extractvalue %NxVal %t15731, 1
  %t15734 = sub i64 %t15733, %t15732
  %t15735 = add i64 88, 0
  %t15736 = sub i64 %t15734, %t15735
  %t15737 = load %NxVal, ptr @nx__g___main____i100
  %t15738 = extractvalue %NxVal %t15737, 1
  %t15739 = add i64 %t15736, %t15738
  %t15740 = add i64 65535, 0
  %t15741 = and i64 %t15739, %t15740
  %t15742 = call %NxVal @nx_int(i64 %t15741)
  store %NxVal %t15742, ptr @nx__g___main____acc100
  %t15743 = load %NxVal, ptr @nx__g___main____i100
  %t15744 = add i64 1, 0
  %t15745 = extractvalue %NxVal %t15743, 1
  %t15746 = add i64 %t15745, %t15744
  %t15747 = call %NxVal @nx_int(i64 %t15746)
  store %NxVal %t15747, ptr @nx__g___main____i100
  br label %wcond303
wend305:
  %t15748 = load %NxVal, ptr @nx__g___main____total
  %t15749 = load %NxVal, ptr @nx__g___main____acc100
  %t15750 = extractvalue %NxVal %t15748, 1
  %t15751 = extractvalue %NxVal %t15749, 1
  %t15752 = add i64 %t15750, %t15751
  %t15753 = add i64 65535, 0
  %t15754 = and i64 %t15752, %t15753
  %t15755 = call %NxVal @nx_int(i64 %t15754)
  store %NxVal %t15755, ptr @nx__g___main____total
  %t15756 = add i64 0, 0
  %t15757 = call %NxVal @nx_int(i64 %t15756)
  store %NxVal %t15757, ptr @nx__g___main____i101
  %t15758 = add i64 0, 0
  %t15759 = call %NxVal @nx_int(i64 %t15758)
  store %NxVal %t15759, ptr @nx__g___main____acc101
  br label %wcond306
wcond306:
  %t15760 = load %NxVal, ptr @nx__g___main____i101
  %t15761 = add i64 3, 0
  %t15762 = extractvalue %NxVal %t15760, 1
  %t15763 = icmp slt i64 %t15762, %t15761
  br i1 %t15763, label %wbody307, label %wend308
wbody307:
  %t15764 = load %NxVal, ptr @nx__g___main____acc101
  %t15765 = load %NxVal, ptr @nx__g___main____c
  %t15766 = load %NxVal, ptr @nx__g___main____i101
  %t15767 = add i64 101, 0
  %t15768 = extractvalue %NxVal %t15766, 1
  %t15769 = add i64 %t15768, %t15767
  %t15771 = getelementptr [2 x %NxVal], ptr %t15770, i64 0, i64 0
  store %NxVal %t15765, ptr %t15771
  %t15772 = call %NxVal @nx_int(i64 %t15769)
  %t15773 = getelementptr [2 x %NxVal], ptr %t15770, i64 0, i64 1
  store %NxVal %t15772, ptr %t15773
  %t15774 = getelementptr [2 x %NxVal], ptr %t15770, i64 0, i64 0
  %t15775 = call %NxVal @nx__m_4____main____Cell__m101(ptr %t15774, i64 2)
  %t15776 = extractvalue %NxVal %t15775, 1
  %t15777 = extractvalue %NxVal %t15764, 1
  %t15778 = add i64 %t15777, %t15776
  %t15779 = add i64 65535, 0
  %t15780 = and i64 %t15778, %t15779
  %t15781 = call %NxVal @nx_int(i64 %t15780)
  store %NxVal %t15781, ptr @nx__g___main____acc101
  %t15782 = load %NxVal, ptr @nx__g___main____acc101
  %t15783 = add i64 75, 0
  %t15784 = extractvalue %NxVal %t15782, 1
  %t15785 = call i64 @nx_mod_i64(i64 %t15784, i64 %t15783)
  %t15786 = add i64 85, 0
  %t15787 = call i64 @nx_mod_i64(i64 %t15785, i64 %t15786)
  %t15788 = load %NxVal, ptr @nx__g___main____i101
  %t15789 = extractvalue %NxVal %t15788, 1
  %t15790 = add i64 %t15787, %t15789
  %t15791 = add i64 65535, 0
  %t15792 = and i64 %t15790, %t15791
  %t15793 = call %NxVal @nx_int(i64 %t15792)
  store %NxVal %t15793, ptr @nx__g___main____acc101
  %t15794 = load %NxVal, ptr @nx__g___main____acc101
  %t15795 = add i64 51, 0
  %t15796 = extractvalue %NxVal %t15794, 1
  %t15797 = call i64 @nx_mod_i64(i64 %t15796, i64 %t15795)
  %t15798 = add i64 5, 0
  %t15799 = call i64 @nx_mod_i64(i64 %t15797, i64 %t15798)
  %t15800 = load %NxVal, ptr @nx__g___main____i101
  %t15801 = extractvalue %NxVal %t15800, 1
  %t15802 = add i64 %t15799, %t15801
  %t15803 = add i64 65535, 0
  %t15804 = and i64 %t15802, %t15803
  %t15805 = call %NxVal @nx_int(i64 %t15804)
  store %NxVal %t15805, ptr @nx__g___main____acc101
  %t15806 = load %NxVal, ptr @nx__g___main____acc101
  %t15807 = add i64 23, 0
  %t15808 = extractvalue %NxVal %t15806, 1
  %t15809 = xor i64 %t15808, %t15807
  %t15810 = add i64 72, 0
  %t15811 = load %NxVal, ptr @nx__g___main____i101
  %t15812 = extractvalue %NxVal %t15811, 1
  %t15813 = add i64 %t15810, %t15812
  %t15814 = xor i64 %t15809, %t15813
  %t15815 = add i64 65535, 0
  %t15816 = and i64 %t15814, %t15815
  %t15817 = call %NxVal @nx_int(i64 %t15816)
  store %NxVal %t15817, ptr @nx__g___main____acc101
  %t15818 = load %NxVal, ptr @nx__g___main____acc101
  %t15819 = add i64 56, 0
  %t15820 = extractvalue %NxVal %t15818, 1
  %t15821 = xor i64 %t15820, %t15819
  %t15822 = add i64 67, 0
  %t15823 = load %NxVal, ptr @nx__g___main____i101
  %t15824 = extractvalue %NxVal %t15823, 1
  %t15825 = add i64 %t15822, %t15824
  %t15826 = xor i64 %t15821, %t15825
  %t15827 = add i64 65535, 0
  %t15828 = and i64 %t15826, %t15827
  %t15829 = call %NxVal @nx_int(i64 %t15828)
  store %NxVal %t15829, ptr @nx__g___main____acc101
  %t15830 = load %NxVal, ptr @nx__g___main____i101
  %t15831 = add i64 1, 0
  %t15832 = extractvalue %NxVal %t15830, 1
  %t15833 = add i64 %t15832, %t15831
  %t15834 = call %NxVal @nx_int(i64 %t15833)
  store %NxVal %t15834, ptr @nx__g___main____i101
  br label %wcond306
wend308:
  %t15835 = load %NxVal, ptr @nx__g___main____total
  %t15836 = load %NxVal, ptr @nx__g___main____acc101
  %t15837 = extractvalue %NxVal %t15835, 1
  %t15838 = extractvalue %NxVal %t15836, 1
  %t15839 = add i64 %t15837, %t15838
  %t15840 = add i64 65535, 0
  %t15841 = and i64 %t15839, %t15840
  %t15842 = call %NxVal @nx_int(i64 %t15841)
  store %NxVal %t15842, ptr @nx__g___main____total
  %t15843 = add i64 0, 0
  %t15844 = call %NxVal @nx_int(i64 %t15843)
  store %NxVal %t15844, ptr @nx__g___main____i102
  %t15845 = add i64 0, 0
  %t15846 = call %NxVal @nx_int(i64 %t15845)
  store %NxVal %t15846, ptr @nx__g___main____acc102
  br label %wcond309
wcond309:
  %t15847 = load %NxVal, ptr @nx__g___main____i102
  %t15848 = add i64 3, 0
  %t15849 = extractvalue %NxVal %t15847, 1
  %t15850 = icmp slt i64 %t15849, %t15848
  br i1 %t15850, label %wbody310, label %wend311
wbody310:
  %t15851 = load %NxVal, ptr @nx__g___main____acc102
  %t15852 = load %NxVal, ptr @nx__g___main____c
  %t15853 = load %NxVal, ptr @nx__g___main____i102
  %t15854 = add i64 102, 0
  %t15855 = extractvalue %NxVal %t15853, 1
  %t15856 = add i64 %t15855, %t15854
  %t15858 = getelementptr [2 x %NxVal], ptr %t15857, i64 0, i64 0
  store %NxVal %t15852, ptr %t15858
  %t15859 = call %NxVal @nx_int(i64 %t15856)
  %t15860 = getelementptr [2 x %NxVal], ptr %t15857, i64 0, i64 1
  store %NxVal %t15859, ptr %t15860
  %t15861 = getelementptr [2 x %NxVal], ptr %t15857, i64 0, i64 0
  %t15862 = call %NxVal @nx__m_4____main____Cell__m102(ptr %t15861, i64 2)
  %t15863 = extractvalue %NxVal %t15862, 1
  %t15864 = extractvalue %NxVal %t15851, 1
  %t15865 = add i64 %t15864, %t15863
  %t15866 = add i64 65535, 0
  %t15867 = and i64 %t15865, %t15866
  %t15868 = call %NxVal @nx_int(i64 %t15867)
  store %NxVal %t15868, ptr @nx__g___main____acc102
  %t15869 = load %NxVal, ptr @nx__g___main____acc102
  %t15870 = add i64 69, 0
  %t15871 = extractvalue %NxVal %t15869, 1
  %t15872 = and i64 %t15871, %t15870
  %t15873 = add i64 38, 0
  %t15874 = load %NxVal, ptr @nx__g___main____i102
  %t15875 = extractvalue %NxVal %t15874, 1
  %t15876 = add i64 %t15873, %t15875
  %t15877 = and i64 %t15872, %t15876
  %t15878 = add i64 65535, 0
  %t15879 = and i64 %t15877, %t15878
  %t15880 = call %NxVal @nx_int(i64 %t15879)
  store %NxVal %t15880, ptr @nx__g___main____acc102
  %t15881 = load %NxVal, ptr @nx__g___main____acc102
  %t15882 = add i64 92, 0
  %t15883 = extractvalue %NxVal %t15881, 1
  %t15884 = or i64 %t15883, %t15882
  %t15885 = add i64 26, 0
  %t15886 = load %NxVal, ptr @nx__g___main____i102
  %t15887 = extractvalue %NxVal %t15886, 1
  %t15888 = add i64 %t15885, %t15887
  %t15889 = or i64 %t15884, %t15888
  %t15890 = add i64 65535, 0
  %t15891 = and i64 %t15889, %t15890
  %t15892 = call %NxVal @nx_int(i64 %t15891)
  store %NxVal %t15892, ptr @nx__g___main____acc102
  %t15893 = load %NxVal, ptr @nx__g___main____acc102
  %t15894 = add i64 81, 0
  %t15895 = extractvalue %NxVal %t15893, 1
  %t15896 = add i64 %t15895, %t15894
  %t15897 = add i64 36, 0
  %t15898 = add i64 %t15896, %t15897
  %t15899 = load %NxVal, ptr @nx__g___main____i102
  %t15900 = extractvalue %NxVal %t15899, 1
  %t15901 = add i64 %t15898, %t15900
  %t15902 = add i64 65535, 0
  %t15903 = and i64 %t15901, %t15902
  %t15904 = call %NxVal @nx_int(i64 %t15903)
  store %NxVal %t15904, ptr @nx__g___main____acc102
  %t15905 = load %NxVal, ptr @nx__g___main____acc102
  %t15906 = add i64 39, 0
  %t15907 = extractvalue %NxVal %t15905, 1
  %t15908 = sub i64 %t15907, %t15906
  %t15909 = add i64 63, 0
  %t15910 = sub i64 %t15908, %t15909
  %t15911 = load %NxVal, ptr @nx__g___main____i102
  %t15912 = extractvalue %NxVal %t15911, 1
  %t15913 = add i64 %t15910, %t15912
  %t15914 = add i64 65535, 0
  %t15915 = and i64 %t15913, %t15914
  %t15916 = call %NxVal @nx_int(i64 %t15915)
  store %NxVal %t15916, ptr @nx__g___main____acc102
  %t15917 = load %NxVal, ptr @nx__g___main____i102
  %t15918 = add i64 1, 0
  %t15919 = extractvalue %NxVal %t15917, 1
  %t15920 = add i64 %t15919, %t15918
  %t15921 = call %NxVal @nx_int(i64 %t15920)
  store %NxVal %t15921, ptr @nx__g___main____i102
  br label %wcond309
wend311:
  %t15922 = load %NxVal, ptr @nx__g___main____total
  %t15923 = load %NxVal, ptr @nx__g___main____acc102
  %t15924 = extractvalue %NxVal %t15922, 1
  %t15925 = extractvalue %NxVal %t15923, 1
  %t15926 = add i64 %t15924, %t15925
  %t15927 = add i64 65535, 0
  %t15928 = and i64 %t15926, %t15927
  %t15929 = call %NxVal @nx_int(i64 %t15928)
  store %NxVal %t15929, ptr @nx__g___main____total
  %t15930 = add i64 0, 0
  %t15931 = call %NxVal @nx_int(i64 %t15930)
  store %NxVal %t15931, ptr @nx__g___main____i103
  %t15932 = add i64 0, 0
  %t15933 = call %NxVal @nx_int(i64 %t15932)
  store %NxVal %t15933, ptr @nx__g___main____acc103
  br label %wcond312
wcond312:
  %t15934 = load %NxVal, ptr @nx__g___main____i103
  %t15935 = add i64 3, 0
  %t15936 = extractvalue %NxVal %t15934, 1
  %t15937 = icmp slt i64 %t15936, %t15935
  br i1 %t15937, label %wbody313, label %wend314
wbody313:
  %t15938 = load %NxVal, ptr @nx__g___main____acc103
  %t15939 = load %NxVal, ptr @nx__g___main____c
  %t15940 = load %NxVal, ptr @nx__g___main____i103
  %t15941 = add i64 103, 0
  %t15942 = extractvalue %NxVal %t15940, 1
  %t15943 = add i64 %t15942, %t15941
  %t15945 = getelementptr [2 x %NxVal], ptr %t15944, i64 0, i64 0
  store %NxVal %t15939, ptr %t15945
  %t15946 = call %NxVal @nx_int(i64 %t15943)
  %t15947 = getelementptr [2 x %NxVal], ptr %t15944, i64 0, i64 1
  store %NxVal %t15946, ptr %t15947
  %t15948 = getelementptr [2 x %NxVal], ptr %t15944, i64 0, i64 0
  %t15949 = call %NxVal @nx__m_4____main____Cell__m103(ptr %t15948, i64 2)
  %t15950 = extractvalue %NxVal %t15949, 1
  %t15951 = extractvalue %NxVal %t15938, 1
  %t15952 = add i64 %t15951, %t15950
  %t15953 = add i64 65535, 0
  %t15954 = and i64 %t15952, %t15953
  %t15955 = call %NxVal @nx_int(i64 %t15954)
  store %NxVal %t15955, ptr @nx__g___main____acc103
  %t15956 = load %NxVal, ptr @nx__g___main____acc103
  %t15957 = add i64 66, 0
  %t15958 = extractvalue %NxVal %t15956, 1
  %t15959 = mul i64 %t15958, %t15957
  %t15960 = add i64 61, 0
  %t15961 = mul i64 %t15959, %t15960
  %t15962 = load %NxVal, ptr @nx__g___main____i103
  %t15963 = extractvalue %NxVal %t15962, 1
  %t15964 = add i64 %t15961, %t15963
  %t15965 = add i64 65535, 0
  %t15966 = and i64 %t15964, %t15965
  %t15967 = call %NxVal @nx_int(i64 %t15966)
  store %NxVal %t15967, ptr @nx__g___main____acc103
  %t15968 = load %NxVal, ptr @nx__g___main____acc103
  %t15969 = add i64 71, 0
  %t15970 = extractvalue %NxVal %t15968, 1
  %t15971 = call i64 @nx_mod_i64(i64 %t15970, i64 %t15969)
  %t15972 = add i64 68, 0
  %t15973 = call i64 @nx_mod_i64(i64 %t15971, i64 %t15972)
  %t15974 = load %NxVal, ptr @nx__g___main____i103
  %t15975 = extractvalue %NxVal %t15974, 1
  %t15976 = add i64 %t15973, %t15975
  %t15977 = add i64 65535, 0
  %t15978 = and i64 %t15976, %t15977
  %t15979 = call %NxVal @nx_int(i64 %t15978)
  store %NxVal %t15979, ptr @nx__g___main____acc103
  %t15980 = load %NxVal, ptr @nx__g___main____acc103
  %t15981 = add i64 66, 0
  %t15982 = extractvalue %NxVal %t15980, 1
  %t15983 = sub i64 %t15982, %t15981
  %t15984 = add i64 38, 0
  %t15985 = sub i64 %t15983, %t15984
  %t15986 = load %NxVal, ptr @nx__g___main____i103
  %t15987 = extractvalue %NxVal %t15986, 1
  %t15988 = add i64 %t15985, %t15987
  %t15989 = add i64 65535, 0
  %t15990 = and i64 %t15988, %t15989
  %t15991 = call %NxVal @nx_int(i64 %t15990)
  store %NxVal %t15991, ptr @nx__g___main____acc103
  %t15992 = load %NxVal, ptr @nx__g___main____acc103
  %t15993 = add i64 30, 0
  %t15994 = extractvalue %NxVal %t15992, 1
  %t15995 = and i64 %t15994, %t15993
  %t15996 = add i64 83, 0
  %t15997 = load %NxVal, ptr @nx__g___main____i103
  %t15998 = extractvalue %NxVal %t15997, 1
  %t15999 = add i64 %t15996, %t15998
  %t16000 = and i64 %t15995, %t15999
  %t16001 = add i64 65535, 0
  %t16002 = and i64 %t16000, %t16001
  %t16003 = call %NxVal @nx_int(i64 %t16002)
  store %NxVal %t16003, ptr @nx__g___main____acc103
  %t16004 = load %NxVal, ptr @nx__g___main____i103
  %t16005 = add i64 1, 0
  %t16006 = extractvalue %NxVal %t16004, 1
  %t16007 = add i64 %t16006, %t16005
  %t16008 = call %NxVal @nx_int(i64 %t16007)
  store %NxVal %t16008, ptr @nx__g___main____i103
  br label %wcond312
wend314:
  %t16009 = load %NxVal, ptr @nx__g___main____total
  %t16010 = load %NxVal, ptr @nx__g___main____acc103
  %t16011 = extractvalue %NxVal %t16009, 1
  %t16012 = extractvalue %NxVal %t16010, 1
  %t16013 = add i64 %t16011, %t16012
  %t16014 = add i64 65535, 0
  %t16015 = and i64 %t16013, %t16014
  %t16016 = call %NxVal @nx_int(i64 %t16015)
  store %NxVal %t16016, ptr @nx__g___main____total
  %t16017 = add i64 0, 0
  %t16018 = call %NxVal @nx_int(i64 %t16017)
  store %NxVal %t16018, ptr @nx__g___main____i104
  %t16019 = add i64 0, 0
  %t16020 = call %NxVal @nx_int(i64 %t16019)
  store %NxVal %t16020, ptr @nx__g___main____acc104
  br label %wcond315
wcond315:
  %t16021 = load %NxVal, ptr @nx__g___main____i104
  %t16022 = add i64 3, 0
  %t16023 = extractvalue %NxVal %t16021, 1
  %t16024 = icmp slt i64 %t16023, %t16022
  br i1 %t16024, label %wbody316, label %wend317
wbody316:
  %t16025 = load %NxVal, ptr @nx__g___main____acc104
  %t16026 = load %NxVal, ptr @nx__g___main____c
  %t16027 = load %NxVal, ptr @nx__g___main____i104
  %t16028 = add i64 104, 0
  %t16029 = extractvalue %NxVal %t16027, 1
  %t16030 = add i64 %t16029, %t16028
  %t16032 = getelementptr [2 x %NxVal], ptr %t16031, i64 0, i64 0
  store %NxVal %t16026, ptr %t16032
  %t16033 = call %NxVal @nx_int(i64 %t16030)
  %t16034 = getelementptr [2 x %NxVal], ptr %t16031, i64 0, i64 1
  store %NxVal %t16033, ptr %t16034
  %t16035 = getelementptr [2 x %NxVal], ptr %t16031, i64 0, i64 0
  %t16036 = call %NxVal @nx__m_4____main____Cell__m104(ptr %t16035, i64 2)
  %t16037 = extractvalue %NxVal %t16036, 1
  %t16038 = extractvalue %NxVal %t16025, 1
  %t16039 = add i64 %t16038, %t16037
  %t16040 = add i64 65535, 0
  %t16041 = and i64 %t16039, %t16040
  %t16042 = call %NxVal @nx_int(i64 %t16041)
  store %NxVal %t16042, ptr @nx__g___main____acc104
  %t16043 = load %NxVal, ptr @nx__g___main____acc104
  %t16044 = add i64 54, 0
  %t16045 = extractvalue %NxVal %t16043, 1
  %t16046 = or i64 %t16045, %t16044
  %t16047 = add i64 62, 0
  %t16048 = load %NxVal, ptr @nx__g___main____i104
  %t16049 = extractvalue %NxVal %t16048, 1
  %t16050 = add i64 %t16047, %t16049
  %t16051 = or i64 %t16046, %t16050
  %t16052 = add i64 65535, 0
  %t16053 = and i64 %t16051, %t16052
  %t16054 = call %NxVal @nx_int(i64 %t16053)
  store %NxVal %t16054, ptr @nx__g___main____acc104
  %t16055 = load %NxVal, ptr @nx__g___main____acc104
  %t16056 = add i64 91, 0
  %t16057 = extractvalue %NxVal %t16055, 1
  %t16058 = mul i64 %t16057, %t16056
  %t16059 = add i64 36, 0
  %t16060 = mul i64 %t16058, %t16059
  %t16061 = load %NxVal, ptr @nx__g___main____i104
  %t16062 = extractvalue %NxVal %t16061, 1
  %t16063 = add i64 %t16060, %t16062
  %t16064 = add i64 65535, 0
  %t16065 = and i64 %t16063, %t16064
  %t16066 = call %NxVal @nx_int(i64 %t16065)
  store %NxVal %t16066, ptr @nx__g___main____acc104
  %t16067 = load %NxVal, ptr @nx__g___main____acc104
  %t16068 = add i64 2, 0
  %t16069 = extractvalue %NxVal %t16067, 1
  %t16070 = call i64 @nx_mod_i64(i64 %t16069, i64 %t16068)
  %t16071 = add i64 82, 0
  %t16072 = call i64 @nx_mod_i64(i64 %t16070, i64 %t16071)
  %t16073 = load %NxVal, ptr @nx__g___main____i104
  %t16074 = extractvalue %NxVal %t16073, 1
  %t16075 = add i64 %t16072, %t16074
  %t16076 = add i64 65535, 0
  %t16077 = and i64 %t16075, %t16076
  %t16078 = call %NxVal @nx_int(i64 %t16077)
  store %NxVal %t16078, ptr @nx__g___main____acc104
  %t16079 = load %NxVal, ptr @nx__g___main____acc104
  %t16080 = add i64 16, 0
  %t16081 = extractvalue %NxVal %t16079, 1
  %t16082 = mul i64 %t16081, %t16080
  %t16083 = add i64 68, 0
  %t16084 = mul i64 %t16082, %t16083
  %t16085 = load %NxVal, ptr @nx__g___main____i104
  %t16086 = extractvalue %NxVal %t16085, 1
  %t16087 = add i64 %t16084, %t16086
  %t16088 = add i64 65535, 0
  %t16089 = and i64 %t16087, %t16088
  %t16090 = call %NxVal @nx_int(i64 %t16089)
  store %NxVal %t16090, ptr @nx__g___main____acc104
  %t16091 = load %NxVal, ptr @nx__g___main____i104
  %t16092 = add i64 1, 0
  %t16093 = extractvalue %NxVal %t16091, 1
  %t16094 = add i64 %t16093, %t16092
  %t16095 = call %NxVal @nx_int(i64 %t16094)
  store %NxVal %t16095, ptr @nx__g___main____i104
  br label %wcond315
wend317:
  %t16096 = load %NxVal, ptr @nx__g___main____total
  %t16097 = load %NxVal, ptr @nx__g___main____acc104
  %t16098 = extractvalue %NxVal %t16096, 1
  %t16099 = extractvalue %NxVal %t16097, 1
  %t16100 = add i64 %t16098, %t16099
  %t16101 = add i64 65535, 0
  %t16102 = and i64 %t16100, %t16101
  %t16103 = call %NxVal @nx_int(i64 %t16102)
  store %NxVal %t16103, ptr @nx__g___main____total
  %t16104 = add i64 0, 0
  %t16105 = call %NxVal @nx_int(i64 %t16104)
  store %NxVal %t16105, ptr @nx__g___main____i105
  %t16106 = add i64 0, 0
  %t16107 = call %NxVal @nx_int(i64 %t16106)
  store %NxVal %t16107, ptr @nx__g___main____acc105
  br label %wcond318
wcond318:
  %t16108 = load %NxVal, ptr @nx__g___main____i105
  %t16109 = add i64 3, 0
  %t16110 = extractvalue %NxVal %t16108, 1
  %t16111 = icmp slt i64 %t16110, %t16109
  br i1 %t16111, label %wbody319, label %wend320
wbody319:
  %t16112 = load %NxVal, ptr @nx__g___main____acc105
  %t16113 = load %NxVal, ptr @nx__g___main____c
  %t16114 = load %NxVal, ptr @nx__g___main____i105
  %t16115 = add i64 105, 0
  %t16116 = extractvalue %NxVal %t16114, 1
  %t16117 = add i64 %t16116, %t16115
  %t16119 = getelementptr [2 x %NxVal], ptr %t16118, i64 0, i64 0
  store %NxVal %t16113, ptr %t16119
  %t16120 = call %NxVal @nx_int(i64 %t16117)
  %t16121 = getelementptr [2 x %NxVal], ptr %t16118, i64 0, i64 1
  store %NxVal %t16120, ptr %t16121
  %t16122 = getelementptr [2 x %NxVal], ptr %t16118, i64 0, i64 0
  %t16123 = call %NxVal @nx__m_4____main____Cell__m105(ptr %t16122, i64 2)
  %t16124 = extractvalue %NxVal %t16123, 1
  %t16125 = extractvalue %NxVal %t16112, 1
  %t16126 = add i64 %t16125, %t16124
  %t16127 = add i64 65535, 0
  %t16128 = and i64 %t16126, %t16127
  %t16129 = call %NxVal @nx_int(i64 %t16128)
  store %NxVal %t16129, ptr @nx__g___main____acc105
  %t16130 = load %NxVal, ptr @nx__g___main____acc105
  %t16131 = add i64 5, 0
  %t16132 = extractvalue %NxVal %t16130, 1
  %t16133 = and i64 %t16132, %t16131
  %t16134 = add i64 20, 0
  %t16135 = load %NxVal, ptr @nx__g___main____i105
  %t16136 = extractvalue %NxVal %t16135, 1
  %t16137 = add i64 %t16134, %t16136
  %t16138 = and i64 %t16133, %t16137
  %t16139 = add i64 65535, 0
  %t16140 = and i64 %t16138, %t16139
  %t16141 = call %NxVal @nx_int(i64 %t16140)
  store %NxVal %t16141, ptr @nx__g___main____acc105
  %t16142 = load %NxVal, ptr @nx__g___main____acc105
  %t16143 = add i64 69, 0
  %t16144 = extractvalue %NxVal %t16142, 1
  %t16145 = mul i64 %t16144, %t16143
  %t16146 = add i64 72, 0
  %t16147 = mul i64 %t16145, %t16146
  %t16148 = load %NxVal, ptr @nx__g___main____i105
  %t16149 = extractvalue %NxVal %t16148, 1
  %t16150 = add i64 %t16147, %t16149
  %t16151 = add i64 65535, 0
  %t16152 = and i64 %t16150, %t16151
  %t16153 = call %NxVal @nx_int(i64 %t16152)
  store %NxVal %t16153, ptr @nx__g___main____acc105
  %t16154 = load %NxVal, ptr @nx__g___main____acc105
  %t16155 = add i64 22, 0
  %t16156 = extractvalue %NxVal %t16154, 1
  %t16157 = or i64 %t16156, %t16155
  %t16158 = add i64 51, 0
  %t16159 = load %NxVal, ptr @nx__g___main____i105
  %t16160 = extractvalue %NxVal %t16159, 1
  %t16161 = add i64 %t16158, %t16160
  %t16162 = or i64 %t16157, %t16161
  %t16163 = add i64 65535, 0
  %t16164 = and i64 %t16162, %t16163
  %t16165 = call %NxVal @nx_int(i64 %t16164)
  store %NxVal %t16165, ptr @nx__g___main____acc105
  %t16166 = load %NxVal, ptr @nx__g___main____acc105
  %t16167 = add i64 31, 0
  %t16168 = extractvalue %NxVal %t16166, 1
  %t16169 = and i64 %t16168, %t16167
  %t16170 = add i64 18, 0
  %t16171 = load %NxVal, ptr @nx__g___main____i105
  %t16172 = extractvalue %NxVal %t16171, 1
  %t16173 = add i64 %t16170, %t16172
  %t16174 = and i64 %t16169, %t16173
  %t16175 = add i64 65535, 0
  %t16176 = and i64 %t16174, %t16175
  %t16177 = call %NxVal @nx_int(i64 %t16176)
  store %NxVal %t16177, ptr @nx__g___main____acc105
  %t16178 = load %NxVal, ptr @nx__g___main____i105
  %t16179 = add i64 1, 0
  %t16180 = extractvalue %NxVal %t16178, 1
  %t16181 = add i64 %t16180, %t16179
  %t16182 = call %NxVal @nx_int(i64 %t16181)
  store %NxVal %t16182, ptr @nx__g___main____i105
  br label %wcond318
wend320:
  %t16183 = load %NxVal, ptr @nx__g___main____total
  %t16184 = load %NxVal, ptr @nx__g___main____acc105
  %t16185 = extractvalue %NxVal %t16183, 1
  %t16186 = extractvalue %NxVal %t16184, 1
  %t16187 = add i64 %t16185, %t16186
  %t16188 = add i64 65535, 0
  %t16189 = and i64 %t16187, %t16188
  %t16190 = call %NxVal @nx_int(i64 %t16189)
  store %NxVal %t16190, ptr @nx__g___main____total
  %t16191 = add i64 0, 0
  %t16192 = call %NxVal @nx_int(i64 %t16191)
  store %NxVal %t16192, ptr @nx__g___main____i106
  %t16193 = add i64 0, 0
  %t16194 = call %NxVal @nx_int(i64 %t16193)
  store %NxVal %t16194, ptr @nx__g___main____acc106
  br label %wcond321
wcond321:
  %t16195 = load %NxVal, ptr @nx__g___main____i106
  %t16196 = add i64 3, 0
  %t16197 = extractvalue %NxVal %t16195, 1
  %t16198 = icmp slt i64 %t16197, %t16196
  br i1 %t16198, label %wbody322, label %wend323
wbody322:
  %t16199 = load %NxVal, ptr @nx__g___main____acc106
  %t16200 = load %NxVal, ptr @nx__g___main____c
  %t16201 = load %NxVal, ptr @nx__g___main____i106
  %t16202 = add i64 106, 0
  %t16203 = extractvalue %NxVal %t16201, 1
  %t16204 = add i64 %t16203, %t16202
  %t16206 = getelementptr [2 x %NxVal], ptr %t16205, i64 0, i64 0
  store %NxVal %t16200, ptr %t16206
  %t16207 = call %NxVal @nx_int(i64 %t16204)
  %t16208 = getelementptr [2 x %NxVal], ptr %t16205, i64 0, i64 1
  store %NxVal %t16207, ptr %t16208
  %t16209 = getelementptr [2 x %NxVal], ptr %t16205, i64 0, i64 0
  %t16210 = call %NxVal @nx__m_4____main____Cell__m106(ptr %t16209, i64 2)
  %t16211 = extractvalue %NxVal %t16210, 1
  %t16212 = extractvalue %NxVal %t16199, 1
  %t16213 = add i64 %t16212, %t16211
  %t16214 = add i64 65535, 0
  %t16215 = and i64 %t16213, %t16214
  %t16216 = call %NxVal @nx_int(i64 %t16215)
  store %NxVal %t16216, ptr @nx__g___main____acc106
  %t16217 = load %NxVal, ptr @nx__g___main____acc106
  %t16218 = add i64 60, 0
  %t16219 = extractvalue %NxVal %t16217, 1
  %t16220 = or i64 %t16219, %t16218
  %t16221 = add i64 58, 0
  %t16222 = load %NxVal, ptr @nx__g___main____i106
  %t16223 = extractvalue %NxVal %t16222, 1
  %t16224 = add i64 %t16221, %t16223
  %t16225 = or i64 %t16220, %t16224
  %t16226 = add i64 65535, 0
  %t16227 = and i64 %t16225, %t16226
  %t16228 = call %NxVal @nx_int(i64 %t16227)
  store %NxVal %t16228, ptr @nx__g___main____acc106
  %t16229 = load %NxVal, ptr @nx__g___main____acc106
  %t16230 = add i64 74, 0
  %t16231 = extractvalue %NxVal %t16229, 1
  %t16232 = add i64 %t16231, %t16230
  %t16233 = add i64 9, 0
  %t16234 = add i64 %t16232, %t16233
  %t16235 = load %NxVal, ptr @nx__g___main____i106
  %t16236 = extractvalue %NxVal %t16235, 1
  %t16237 = add i64 %t16234, %t16236
  %t16238 = add i64 65535, 0
  %t16239 = and i64 %t16237, %t16238
  %t16240 = call %NxVal @nx_int(i64 %t16239)
  store %NxVal %t16240, ptr @nx__g___main____acc106
  %t16241 = load %NxVal, ptr @nx__g___main____acc106
  %t16242 = add i64 36, 0
  %t16243 = extractvalue %NxVal %t16241, 1
  %t16244 = and i64 %t16243, %t16242
  %t16245 = add i64 5, 0
  %t16246 = load %NxVal, ptr @nx__g___main____i106
  %t16247 = extractvalue %NxVal %t16246, 1
  %t16248 = add i64 %t16245, %t16247
  %t16249 = and i64 %t16244, %t16248
  %t16250 = add i64 65535, 0
  %t16251 = and i64 %t16249, %t16250
  %t16252 = call %NxVal @nx_int(i64 %t16251)
  store %NxVal %t16252, ptr @nx__g___main____acc106
  %t16253 = load %NxVal, ptr @nx__g___main____acc106
  %t16254 = add i64 50, 0
  %t16255 = extractvalue %NxVal %t16253, 1
  %t16256 = or i64 %t16255, %t16254
  %t16257 = add i64 58, 0
  %t16258 = load %NxVal, ptr @nx__g___main____i106
  %t16259 = extractvalue %NxVal %t16258, 1
  %t16260 = add i64 %t16257, %t16259
  %t16261 = or i64 %t16256, %t16260
  %t16262 = add i64 65535, 0
  %t16263 = and i64 %t16261, %t16262
  %t16264 = call %NxVal @nx_int(i64 %t16263)
  store %NxVal %t16264, ptr @nx__g___main____acc106
  %t16265 = load %NxVal, ptr @nx__g___main____i106
  %t16266 = add i64 1, 0
  %t16267 = extractvalue %NxVal %t16265, 1
  %t16268 = add i64 %t16267, %t16266
  %t16269 = call %NxVal @nx_int(i64 %t16268)
  store %NxVal %t16269, ptr @nx__g___main____i106
  br label %wcond321
wend323:
  %t16270 = load %NxVal, ptr @nx__g___main____total
  %t16271 = load %NxVal, ptr @nx__g___main____acc106
  %t16272 = extractvalue %NxVal %t16270, 1
  %t16273 = extractvalue %NxVal %t16271, 1
  %t16274 = add i64 %t16272, %t16273
  %t16275 = add i64 65535, 0
  %t16276 = and i64 %t16274, %t16275
  %t16277 = call %NxVal @nx_int(i64 %t16276)
  store %NxVal %t16277, ptr @nx__g___main____total
  %t16278 = add i64 0, 0
  %t16279 = call %NxVal @nx_int(i64 %t16278)
  store %NxVal %t16279, ptr @nx__g___main____i107
  %t16280 = add i64 0, 0
  %t16281 = call %NxVal @nx_int(i64 %t16280)
  store %NxVal %t16281, ptr @nx__g___main____acc107
  br label %wcond324
wcond324:
  %t16282 = load %NxVal, ptr @nx__g___main____i107
  %t16283 = add i64 3, 0
  %t16284 = extractvalue %NxVal %t16282, 1
  %t16285 = icmp slt i64 %t16284, %t16283
  br i1 %t16285, label %wbody325, label %wend326
wbody325:
  %t16286 = load %NxVal, ptr @nx__g___main____acc107
  %t16287 = load %NxVal, ptr @nx__g___main____c
  %t16288 = load %NxVal, ptr @nx__g___main____i107
  %t16289 = add i64 107, 0
  %t16290 = extractvalue %NxVal %t16288, 1
  %t16291 = add i64 %t16290, %t16289
  %t16293 = getelementptr [2 x %NxVal], ptr %t16292, i64 0, i64 0
  store %NxVal %t16287, ptr %t16293
  %t16294 = call %NxVal @nx_int(i64 %t16291)
  %t16295 = getelementptr [2 x %NxVal], ptr %t16292, i64 0, i64 1
  store %NxVal %t16294, ptr %t16295
  %t16296 = getelementptr [2 x %NxVal], ptr %t16292, i64 0, i64 0
  %t16297 = call %NxVal @nx__m_4____main____Cell__m107(ptr %t16296, i64 2)
  %t16298 = extractvalue %NxVal %t16297, 1
  %t16299 = extractvalue %NxVal %t16286, 1
  %t16300 = add i64 %t16299, %t16298
  %t16301 = add i64 65535, 0
  %t16302 = and i64 %t16300, %t16301
  %t16303 = call %NxVal @nx_int(i64 %t16302)
  store %NxVal %t16303, ptr @nx__g___main____acc107
  %t16304 = load %NxVal, ptr @nx__g___main____acc107
  %t16305 = add i64 88, 0
  %t16306 = extractvalue %NxVal %t16304, 1
  %t16307 = xor i64 %t16306, %t16305
  %t16308 = add i64 11, 0
  %t16309 = load %NxVal, ptr @nx__g___main____i107
  %t16310 = extractvalue %NxVal %t16309, 1
  %t16311 = add i64 %t16308, %t16310
  %t16312 = xor i64 %t16307, %t16311
  %t16313 = add i64 65535, 0
  %t16314 = and i64 %t16312, %t16313
  %t16315 = call %NxVal @nx_int(i64 %t16314)
  store %NxVal %t16315, ptr @nx__g___main____acc107
  %t16316 = load %NxVal, ptr @nx__g___main____acc107
  %t16317 = add i64 39, 0
  %t16318 = extractvalue %NxVal %t16316, 1
  %t16319 = xor i64 %t16318, %t16317
  %t16320 = add i64 29, 0
  %t16321 = load %NxVal, ptr @nx__g___main____i107
  %t16322 = extractvalue %NxVal %t16321, 1
  %t16323 = add i64 %t16320, %t16322
  %t16324 = xor i64 %t16319, %t16323
  %t16325 = add i64 65535, 0
  %t16326 = and i64 %t16324, %t16325
  %t16327 = call %NxVal @nx_int(i64 %t16326)
  store %NxVal %t16327, ptr @nx__g___main____acc107
  %t16328 = load %NxVal, ptr @nx__g___main____acc107
  %t16329 = add i64 70, 0
  %t16330 = extractvalue %NxVal %t16328, 1
  %t16331 = call i64 @nx_mod_i64(i64 %t16330, i64 %t16329)
  %t16332 = add i64 12, 0
  %t16333 = call i64 @nx_mod_i64(i64 %t16331, i64 %t16332)
  %t16334 = load %NxVal, ptr @nx__g___main____i107
  %t16335 = extractvalue %NxVal %t16334, 1
  %t16336 = add i64 %t16333, %t16335
  %t16337 = add i64 65535, 0
  %t16338 = and i64 %t16336, %t16337
  %t16339 = call %NxVal @nx_int(i64 %t16338)
  store %NxVal %t16339, ptr @nx__g___main____acc107
  %t16340 = load %NxVal, ptr @nx__g___main____acc107
  %t16341 = add i64 21, 0
  %t16342 = extractvalue %NxVal %t16340, 1
  %t16343 = call i64 @nx_mod_i64(i64 %t16342, i64 %t16341)
  %t16344 = add i64 70, 0
  %t16345 = call i64 @nx_mod_i64(i64 %t16343, i64 %t16344)
  %t16346 = load %NxVal, ptr @nx__g___main____i107
  %t16347 = extractvalue %NxVal %t16346, 1
  %t16348 = add i64 %t16345, %t16347
  %t16349 = add i64 65535, 0
  %t16350 = and i64 %t16348, %t16349
  %t16351 = call %NxVal @nx_int(i64 %t16350)
  store %NxVal %t16351, ptr @nx__g___main____acc107
  %t16352 = load %NxVal, ptr @nx__g___main____i107
  %t16353 = add i64 1, 0
  %t16354 = extractvalue %NxVal %t16352, 1
  %t16355 = add i64 %t16354, %t16353
  %t16356 = call %NxVal @nx_int(i64 %t16355)
  store %NxVal %t16356, ptr @nx__g___main____i107
  br label %wcond324
wend326:
  %t16357 = load %NxVal, ptr @nx__g___main____total
  %t16358 = load %NxVal, ptr @nx__g___main____acc107
  %t16359 = extractvalue %NxVal %t16357, 1
  %t16360 = extractvalue %NxVal %t16358, 1
  %t16361 = add i64 %t16359, %t16360
  %t16362 = add i64 65535, 0
  %t16363 = and i64 %t16361, %t16362
  %t16364 = call %NxVal @nx_int(i64 %t16363)
  store %NxVal %t16364, ptr @nx__g___main____total
  %t16365 = add i64 0, 0
  %t16366 = call %NxVal @nx_int(i64 %t16365)
  store %NxVal %t16366, ptr @nx__g___main____i108
  %t16367 = add i64 0, 0
  %t16368 = call %NxVal @nx_int(i64 %t16367)
  store %NxVal %t16368, ptr @nx__g___main____acc108
  br label %wcond327
wcond327:
  %t16369 = load %NxVal, ptr @nx__g___main____i108
  %t16370 = add i64 3, 0
  %t16371 = extractvalue %NxVal %t16369, 1
  %t16372 = icmp slt i64 %t16371, %t16370
  br i1 %t16372, label %wbody328, label %wend329
wbody328:
  %t16373 = load %NxVal, ptr @nx__g___main____acc108
  %t16374 = load %NxVal, ptr @nx__g___main____c
  %t16375 = load %NxVal, ptr @nx__g___main____i108
  %t16376 = add i64 108, 0
  %t16377 = extractvalue %NxVal %t16375, 1
  %t16378 = add i64 %t16377, %t16376
  %t16380 = getelementptr [2 x %NxVal], ptr %t16379, i64 0, i64 0
  store %NxVal %t16374, ptr %t16380
  %t16381 = call %NxVal @nx_int(i64 %t16378)
  %t16382 = getelementptr [2 x %NxVal], ptr %t16379, i64 0, i64 1
  store %NxVal %t16381, ptr %t16382
  %t16383 = getelementptr [2 x %NxVal], ptr %t16379, i64 0, i64 0
  %t16384 = call %NxVal @nx__m_4____main____Cell__m108(ptr %t16383, i64 2)
  %t16385 = extractvalue %NxVal %t16384, 1
  %t16386 = extractvalue %NxVal %t16373, 1
  %t16387 = add i64 %t16386, %t16385
  %t16388 = add i64 65535, 0
  %t16389 = and i64 %t16387, %t16388
  %t16390 = call %NxVal @nx_int(i64 %t16389)
  store %NxVal %t16390, ptr @nx__g___main____acc108
  %t16391 = load %NxVal, ptr @nx__g___main____acc108
  %t16392 = add i64 51, 0
  %t16393 = extractvalue %NxVal %t16391, 1
  %t16394 = mul i64 %t16393, %t16392
  %t16395 = add i64 26, 0
  %t16396 = mul i64 %t16394, %t16395
  %t16397 = load %NxVal, ptr @nx__g___main____i108
  %t16398 = extractvalue %NxVal %t16397, 1
  %t16399 = add i64 %t16396, %t16398
  %t16400 = add i64 65535, 0
  %t16401 = and i64 %t16399, %t16400
  %t16402 = call %NxVal @nx_int(i64 %t16401)
  store %NxVal %t16402, ptr @nx__g___main____acc108
  %t16403 = load %NxVal, ptr @nx__g___main____acc108
  %t16404 = add i64 97, 0
  %t16405 = extractvalue %NxVal %t16403, 1
  %t16406 = and i64 %t16405, %t16404
  %t16407 = add i64 23, 0
  %t16408 = load %NxVal, ptr @nx__g___main____i108
  %t16409 = extractvalue %NxVal %t16408, 1
  %t16410 = add i64 %t16407, %t16409
  %t16411 = and i64 %t16406, %t16410
  %t16412 = add i64 65535, 0
  %t16413 = and i64 %t16411, %t16412
  %t16414 = call %NxVal @nx_int(i64 %t16413)
  store %NxVal %t16414, ptr @nx__g___main____acc108
  %t16415 = load %NxVal, ptr @nx__g___main____acc108
  %t16416 = add i64 46, 0
  %t16417 = extractvalue %NxVal %t16415, 1
  %t16418 = and i64 %t16417, %t16416
  %t16419 = add i64 52, 0
  %t16420 = load %NxVal, ptr @nx__g___main____i108
  %t16421 = extractvalue %NxVal %t16420, 1
  %t16422 = add i64 %t16419, %t16421
  %t16423 = and i64 %t16418, %t16422
  %t16424 = add i64 65535, 0
  %t16425 = and i64 %t16423, %t16424
  %t16426 = call %NxVal @nx_int(i64 %t16425)
  store %NxVal %t16426, ptr @nx__g___main____acc108
  %t16427 = load %NxVal, ptr @nx__g___main____acc108
  %t16428 = add i64 72, 0
  %t16429 = extractvalue %NxVal %t16427, 1
  %t16430 = mul i64 %t16429, %t16428
  %t16431 = add i64 32, 0
  %t16432 = mul i64 %t16430, %t16431
  %t16433 = load %NxVal, ptr @nx__g___main____i108
  %t16434 = extractvalue %NxVal %t16433, 1
  %t16435 = add i64 %t16432, %t16434
  %t16436 = add i64 65535, 0
  %t16437 = and i64 %t16435, %t16436
  %t16438 = call %NxVal @nx_int(i64 %t16437)
  store %NxVal %t16438, ptr @nx__g___main____acc108
  %t16439 = load %NxVal, ptr @nx__g___main____i108
  %t16440 = add i64 1, 0
  %t16441 = extractvalue %NxVal %t16439, 1
  %t16442 = add i64 %t16441, %t16440
  %t16443 = call %NxVal @nx_int(i64 %t16442)
  store %NxVal %t16443, ptr @nx__g___main____i108
  br label %wcond327
wend329:
  %t16444 = load %NxVal, ptr @nx__g___main____total
  %t16445 = load %NxVal, ptr @nx__g___main____acc108
  %t16446 = extractvalue %NxVal %t16444, 1
  %t16447 = extractvalue %NxVal %t16445, 1
  %t16448 = add i64 %t16446, %t16447
  %t16449 = add i64 65535, 0
  %t16450 = and i64 %t16448, %t16449
  %t16451 = call %NxVal @nx_int(i64 %t16450)
  store %NxVal %t16451, ptr @nx__g___main____total
  %t16452 = add i64 0, 0
  %t16453 = call %NxVal @nx_int(i64 %t16452)
  store %NxVal %t16453, ptr @nx__g___main____i109
  %t16454 = add i64 0, 0
  %t16455 = call %NxVal @nx_int(i64 %t16454)
  store %NxVal %t16455, ptr @nx__g___main____acc109
  br label %wcond330
wcond330:
  %t16456 = load %NxVal, ptr @nx__g___main____i109
  %t16457 = add i64 3, 0
  %t16458 = extractvalue %NxVal %t16456, 1
  %t16459 = icmp slt i64 %t16458, %t16457
  br i1 %t16459, label %wbody331, label %wend332
wbody331:
  %t16460 = load %NxVal, ptr @nx__g___main____acc109
  %t16461 = load %NxVal, ptr @nx__g___main____c
  %t16462 = load %NxVal, ptr @nx__g___main____i109
  %t16463 = add i64 109, 0
  %t16464 = extractvalue %NxVal %t16462, 1
  %t16465 = add i64 %t16464, %t16463
  %t16467 = getelementptr [2 x %NxVal], ptr %t16466, i64 0, i64 0
  store %NxVal %t16461, ptr %t16467
  %t16468 = call %NxVal @nx_int(i64 %t16465)
  %t16469 = getelementptr [2 x %NxVal], ptr %t16466, i64 0, i64 1
  store %NxVal %t16468, ptr %t16469
  %t16470 = getelementptr [2 x %NxVal], ptr %t16466, i64 0, i64 0
  %t16471 = call %NxVal @nx__m_4____main____Cell__m109(ptr %t16470, i64 2)
  %t16472 = extractvalue %NxVal %t16471, 1
  %t16473 = extractvalue %NxVal %t16460, 1
  %t16474 = add i64 %t16473, %t16472
  %t16475 = add i64 65535, 0
  %t16476 = and i64 %t16474, %t16475
  %t16477 = call %NxVal @nx_int(i64 %t16476)
  store %NxVal %t16477, ptr @nx__g___main____acc109
  %t16478 = load %NxVal, ptr @nx__g___main____acc109
  %t16479 = add i64 37, 0
  %t16480 = extractvalue %NxVal %t16478, 1
  %t16481 = mul i64 %t16480, %t16479
  %t16482 = add i64 56, 0
  %t16483 = mul i64 %t16481, %t16482
  %t16484 = load %NxVal, ptr @nx__g___main____i109
  %t16485 = extractvalue %NxVal %t16484, 1
  %t16486 = add i64 %t16483, %t16485
  %t16487 = add i64 65535, 0
  %t16488 = and i64 %t16486, %t16487
  %t16489 = call %NxVal @nx_int(i64 %t16488)
  store %NxVal %t16489, ptr @nx__g___main____acc109
  %t16490 = load %NxVal, ptr @nx__g___main____acc109
  %t16491 = add i64 55, 0
  %t16492 = extractvalue %NxVal %t16490, 1
  %t16493 = sub i64 %t16492, %t16491
  %t16494 = add i64 89, 0
  %t16495 = sub i64 %t16493, %t16494
  %t16496 = load %NxVal, ptr @nx__g___main____i109
  %t16497 = extractvalue %NxVal %t16496, 1
  %t16498 = add i64 %t16495, %t16497
  %t16499 = add i64 65535, 0
  %t16500 = and i64 %t16498, %t16499
  %t16501 = call %NxVal @nx_int(i64 %t16500)
  store %NxVal %t16501, ptr @nx__g___main____acc109
  %t16502 = load %NxVal, ptr @nx__g___main____acc109
  %t16503 = add i64 19, 0
  %t16504 = extractvalue %NxVal %t16502, 1
  %t16505 = sub i64 %t16504, %t16503
  %t16506 = add i64 4, 0
  %t16507 = sub i64 %t16505, %t16506
  %t16508 = load %NxVal, ptr @nx__g___main____i109
  %t16509 = extractvalue %NxVal %t16508, 1
  %t16510 = add i64 %t16507, %t16509
  %t16511 = add i64 65535, 0
  %t16512 = and i64 %t16510, %t16511
  %t16513 = call %NxVal @nx_int(i64 %t16512)
  store %NxVal %t16513, ptr @nx__g___main____acc109
  %t16514 = load %NxVal, ptr @nx__g___main____acc109
  %t16515 = add i64 92, 0
  %t16516 = extractvalue %NxVal %t16514, 1
  %t16517 = and i64 %t16516, %t16515
  %t16518 = add i64 42, 0
  %t16519 = load %NxVal, ptr @nx__g___main____i109
  %t16520 = extractvalue %NxVal %t16519, 1
  %t16521 = add i64 %t16518, %t16520
  %t16522 = and i64 %t16517, %t16521
  %t16523 = add i64 65535, 0
  %t16524 = and i64 %t16522, %t16523
  %t16525 = call %NxVal @nx_int(i64 %t16524)
  store %NxVal %t16525, ptr @nx__g___main____acc109
  %t16526 = load %NxVal, ptr @nx__g___main____i109
  %t16527 = add i64 1, 0
  %t16528 = extractvalue %NxVal %t16526, 1
  %t16529 = add i64 %t16528, %t16527
  %t16530 = call %NxVal @nx_int(i64 %t16529)
  store %NxVal %t16530, ptr @nx__g___main____i109
  br label %wcond330
wend332:
  %t16531 = load %NxVal, ptr @nx__g___main____total
  %t16532 = load %NxVal, ptr @nx__g___main____acc109
  %t16533 = extractvalue %NxVal %t16531, 1
  %t16534 = extractvalue %NxVal %t16532, 1
  %t16535 = add i64 %t16533, %t16534
  %t16536 = add i64 65535, 0
  %t16537 = and i64 %t16535, %t16536
  %t16538 = call %NxVal @nx_int(i64 %t16537)
  store %NxVal %t16538, ptr @nx__g___main____total
  %t16539 = add i64 0, 0
  %t16540 = call %NxVal @nx_int(i64 %t16539)
  store %NxVal %t16540, ptr @nx__g___main____i110
  %t16541 = add i64 0, 0
  %t16542 = call %NxVal @nx_int(i64 %t16541)
  store %NxVal %t16542, ptr @nx__g___main____acc110
  br label %wcond333
wcond333:
  %t16543 = load %NxVal, ptr @nx__g___main____i110
  %t16544 = add i64 3, 0
  %t16545 = extractvalue %NxVal %t16543, 1
  %t16546 = icmp slt i64 %t16545, %t16544
  br i1 %t16546, label %wbody334, label %wend335
wbody334:
  %t16547 = load %NxVal, ptr @nx__g___main____acc110
  %t16548 = load %NxVal, ptr @nx__g___main____c
  %t16549 = load %NxVal, ptr @nx__g___main____i110
  %t16550 = add i64 110, 0
  %t16551 = extractvalue %NxVal %t16549, 1
  %t16552 = add i64 %t16551, %t16550
  %t16554 = getelementptr [2 x %NxVal], ptr %t16553, i64 0, i64 0
  store %NxVal %t16548, ptr %t16554
  %t16555 = call %NxVal @nx_int(i64 %t16552)
  %t16556 = getelementptr [2 x %NxVal], ptr %t16553, i64 0, i64 1
  store %NxVal %t16555, ptr %t16556
  %t16557 = getelementptr [2 x %NxVal], ptr %t16553, i64 0, i64 0
  %t16558 = call %NxVal @nx__m_4____main____Cell__m110(ptr %t16557, i64 2)
  %t16559 = extractvalue %NxVal %t16558, 1
  %t16560 = extractvalue %NxVal %t16547, 1
  %t16561 = add i64 %t16560, %t16559
  %t16562 = add i64 65535, 0
  %t16563 = and i64 %t16561, %t16562
  %t16564 = call %NxVal @nx_int(i64 %t16563)
  store %NxVal %t16564, ptr @nx__g___main____acc110
  %t16565 = load %NxVal, ptr @nx__g___main____acc110
  %t16566 = add i64 30, 0
  %t16567 = extractvalue %NxVal %t16565, 1
  %t16568 = call i64 @nx_mod_i64(i64 %t16567, i64 %t16566)
  %t16569 = add i64 11, 0
  %t16570 = call i64 @nx_mod_i64(i64 %t16568, i64 %t16569)
  %t16571 = load %NxVal, ptr @nx__g___main____i110
  %t16572 = extractvalue %NxVal %t16571, 1
  %t16573 = add i64 %t16570, %t16572
  %t16574 = add i64 65535, 0
  %t16575 = and i64 %t16573, %t16574
  %t16576 = call %NxVal @nx_int(i64 %t16575)
  store %NxVal %t16576, ptr @nx__g___main____acc110
  %t16577 = load %NxVal, ptr @nx__g___main____acc110
  %t16578 = add i64 49, 0
  %t16579 = extractvalue %NxVal %t16577, 1
  %t16580 = and i64 %t16579, %t16578
  %t16581 = add i64 41, 0
  %t16582 = load %NxVal, ptr @nx__g___main____i110
  %t16583 = extractvalue %NxVal %t16582, 1
  %t16584 = add i64 %t16581, %t16583
  %t16585 = and i64 %t16580, %t16584
  %t16586 = add i64 65535, 0
  %t16587 = and i64 %t16585, %t16586
  %t16588 = call %NxVal @nx_int(i64 %t16587)
  store %NxVal %t16588, ptr @nx__g___main____acc110
  %t16589 = load %NxVal, ptr @nx__g___main____acc110
  %t16590 = add i64 79, 0
  %t16591 = extractvalue %NxVal %t16589, 1
  %t16592 = call i64 @nx_mod_i64(i64 %t16591, i64 %t16590)
  %t16593 = add i64 88, 0
  %t16594 = call i64 @nx_mod_i64(i64 %t16592, i64 %t16593)
  %t16595 = load %NxVal, ptr @nx__g___main____i110
  %t16596 = extractvalue %NxVal %t16595, 1
  %t16597 = add i64 %t16594, %t16596
  %t16598 = add i64 65535, 0
  %t16599 = and i64 %t16597, %t16598
  %t16600 = call %NxVal @nx_int(i64 %t16599)
  store %NxVal %t16600, ptr @nx__g___main____acc110
  %t16601 = load %NxVal, ptr @nx__g___main____acc110
  %t16602 = add i64 95, 0
  %t16603 = extractvalue %NxVal %t16601, 1
  %t16604 = and i64 %t16603, %t16602
  %t16605 = add i64 27, 0
  %t16606 = load %NxVal, ptr @nx__g___main____i110
  %t16607 = extractvalue %NxVal %t16606, 1
  %t16608 = add i64 %t16605, %t16607
  %t16609 = and i64 %t16604, %t16608
  %t16610 = add i64 65535, 0
  %t16611 = and i64 %t16609, %t16610
  %t16612 = call %NxVal @nx_int(i64 %t16611)
  store %NxVal %t16612, ptr @nx__g___main____acc110
  %t16613 = load %NxVal, ptr @nx__g___main____i110
  %t16614 = add i64 1, 0
  %t16615 = extractvalue %NxVal %t16613, 1
  %t16616 = add i64 %t16615, %t16614
  %t16617 = call %NxVal @nx_int(i64 %t16616)
  store %NxVal %t16617, ptr @nx__g___main____i110
  br label %wcond333
wend335:
  %t16618 = load %NxVal, ptr @nx__g___main____total
  %t16619 = load %NxVal, ptr @nx__g___main____acc110
  %t16620 = extractvalue %NxVal %t16618, 1
  %t16621 = extractvalue %NxVal %t16619, 1
  %t16622 = add i64 %t16620, %t16621
  %t16623 = add i64 65535, 0
  %t16624 = and i64 %t16622, %t16623
  %t16625 = call %NxVal @nx_int(i64 %t16624)
  store %NxVal %t16625, ptr @nx__g___main____total
  %t16626 = add i64 0, 0
  %t16627 = call %NxVal @nx_int(i64 %t16626)
  store %NxVal %t16627, ptr @nx__g___main____i111
  %t16628 = add i64 0, 0
  %t16629 = call %NxVal @nx_int(i64 %t16628)
  store %NxVal %t16629, ptr @nx__g___main____acc111
  br label %wcond336
wcond336:
  %t16630 = load %NxVal, ptr @nx__g___main____i111
  %t16631 = add i64 3, 0
  %t16632 = extractvalue %NxVal %t16630, 1
  %t16633 = icmp slt i64 %t16632, %t16631
  br i1 %t16633, label %wbody337, label %wend338
wbody337:
  %t16634 = load %NxVal, ptr @nx__g___main____acc111
  %t16635 = load %NxVal, ptr @nx__g___main____c
  %t16636 = load %NxVal, ptr @nx__g___main____i111
  %t16637 = add i64 111, 0
  %t16638 = extractvalue %NxVal %t16636, 1
  %t16639 = add i64 %t16638, %t16637
  %t16641 = getelementptr [2 x %NxVal], ptr %t16640, i64 0, i64 0
  store %NxVal %t16635, ptr %t16641
  %t16642 = call %NxVal @nx_int(i64 %t16639)
  %t16643 = getelementptr [2 x %NxVal], ptr %t16640, i64 0, i64 1
  store %NxVal %t16642, ptr %t16643
  %t16644 = getelementptr [2 x %NxVal], ptr %t16640, i64 0, i64 0
  %t16645 = call %NxVal @nx__m_4____main____Cell__m111(ptr %t16644, i64 2)
  %t16646 = extractvalue %NxVal %t16645, 1
  %t16647 = extractvalue %NxVal %t16634, 1
  %t16648 = add i64 %t16647, %t16646
  %t16649 = add i64 65535, 0
  %t16650 = and i64 %t16648, %t16649
  %t16651 = call %NxVal @nx_int(i64 %t16650)
  store %NxVal %t16651, ptr @nx__g___main____acc111
  %t16652 = load %NxVal, ptr @nx__g___main____acc111
  %t16653 = add i64 32, 0
  %t16654 = extractvalue %NxVal %t16652, 1
  %t16655 = xor i64 %t16654, %t16653
  %t16656 = add i64 41, 0
  %t16657 = load %NxVal, ptr @nx__g___main____i111
  %t16658 = extractvalue %NxVal %t16657, 1
  %t16659 = add i64 %t16656, %t16658
  %t16660 = xor i64 %t16655, %t16659
  %t16661 = add i64 65535, 0
  %t16662 = and i64 %t16660, %t16661
  %t16663 = call %NxVal @nx_int(i64 %t16662)
  store %NxVal %t16663, ptr @nx__g___main____acc111
  %t16664 = load %NxVal, ptr @nx__g___main____acc111
  %t16665 = add i64 61, 0
  %t16666 = extractvalue %NxVal %t16664, 1
  %t16667 = sub i64 %t16666, %t16665
  %t16668 = add i64 66, 0
  %t16669 = sub i64 %t16667, %t16668
  %t16670 = load %NxVal, ptr @nx__g___main____i111
  %t16671 = extractvalue %NxVal %t16670, 1
  %t16672 = add i64 %t16669, %t16671
  %t16673 = add i64 65535, 0
  %t16674 = and i64 %t16672, %t16673
  %t16675 = call %NxVal @nx_int(i64 %t16674)
  store %NxVal %t16675, ptr @nx__g___main____acc111
  %t16676 = load %NxVal, ptr @nx__g___main____acc111
  %t16677 = add i64 49, 0
  %t16678 = extractvalue %NxVal %t16676, 1
  %t16679 = or i64 %t16678, %t16677
  %t16680 = add i64 11, 0
  %t16681 = load %NxVal, ptr @nx__g___main____i111
  %t16682 = extractvalue %NxVal %t16681, 1
  %t16683 = add i64 %t16680, %t16682
  %t16684 = or i64 %t16679, %t16683
  %t16685 = add i64 65535, 0
  %t16686 = and i64 %t16684, %t16685
  %t16687 = call %NxVal @nx_int(i64 %t16686)
  store %NxVal %t16687, ptr @nx__g___main____acc111
  %t16688 = load %NxVal, ptr @nx__g___main____acc111
  %t16689 = add i64 94, 0
  %t16690 = extractvalue %NxVal %t16688, 1
  %t16691 = xor i64 %t16690, %t16689
  %t16692 = add i64 23, 0
  %t16693 = load %NxVal, ptr @nx__g___main____i111
  %t16694 = extractvalue %NxVal %t16693, 1
  %t16695 = add i64 %t16692, %t16694
  %t16696 = xor i64 %t16691, %t16695
  %t16697 = add i64 65535, 0
  %t16698 = and i64 %t16696, %t16697
  %t16699 = call %NxVal @nx_int(i64 %t16698)
  store %NxVal %t16699, ptr @nx__g___main____acc111
  %t16700 = load %NxVal, ptr @nx__g___main____i111
  %t16701 = add i64 1, 0
  %t16702 = extractvalue %NxVal %t16700, 1
  %t16703 = add i64 %t16702, %t16701
  %t16704 = call %NxVal @nx_int(i64 %t16703)
  store %NxVal %t16704, ptr @nx__g___main____i111
  br label %wcond336
wend338:
  %t16705 = load %NxVal, ptr @nx__g___main____total
  %t16706 = load %NxVal, ptr @nx__g___main____acc111
  %t16707 = extractvalue %NxVal %t16705, 1
  %t16708 = extractvalue %NxVal %t16706, 1
  %t16709 = add i64 %t16707, %t16708
  %t16710 = add i64 65535, 0
  %t16711 = and i64 %t16709, %t16710
  %t16712 = call %NxVal @nx_int(i64 %t16711)
  store %NxVal %t16712, ptr @nx__g___main____total
  %t16713 = add i64 0, 0
  %t16714 = call %NxVal @nx_int(i64 %t16713)
  store %NxVal %t16714, ptr @nx__g___main____i112
  %t16715 = add i64 0, 0
  %t16716 = call %NxVal @nx_int(i64 %t16715)
  store %NxVal %t16716, ptr @nx__g___main____acc112
  br label %wcond339
wcond339:
  %t16717 = load %NxVal, ptr @nx__g___main____i112
  %t16718 = add i64 3, 0
  %t16719 = extractvalue %NxVal %t16717, 1
  %t16720 = icmp slt i64 %t16719, %t16718
  br i1 %t16720, label %wbody340, label %wend341
wbody340:
  %t16721 = load %NxVal, ptr @nx__g___main____acc112
  %t16722 = load %NxVal, ptr @nx__g___main____c
  %t16723 = load %NxVal, ptr @nx__g___main____i112
  %t16724 = add i64 112, 0
  %t16725 = extractvalue %NxVal %t16723, 1
  %t16726 = add i64 %t16725, %t16724
  %t16728 = getelementptr [2 x %NxVal], ptr %t16727, i64 0, i64 0
  store %NxVal %t16722, ptr %t16728
  %t16729 = call %NxVal @nx_int(i64 %t16726)
  %t16730 = getelementptr [2 x %NxVal], ptr %t16727, i64 0, i64 1
  store %NxVal %t16729, ptr %t16730
  %t16731 = getelementptr [2 x %NxVal], ptr %t16727, i64 0, i64 0
  %t16732 = call %NxVal @nx__m_4____main____Cell__m112(ptr %t16731, i64 2)
  %t16733 = extractvalue %NxVal %t16732, 1
  %t16734 = extractvalue %NxVal %t16721, 1
  %t16735 = add i64 %t16734, %t16733
  %t16736 = add i64 65535, 0
  %t16737 = and i64 %t16735, %t16736
  %t16738 = call %NxVal @nx_int(i64 %t16737)
  store %NxVal %t16738, ptr @nx__g___main____acc112
  %t16739 = load %NxVal, ptr @nx__g___main____acc112
  %t16740 = add i64 73, 0
  %t16741 = extractvalue %NxVal %t16739, 1
  %t16742 = call i64 @nx_mod_i64(i64 %t16741, i64 %t16740)
  %t16743 = add i64 21, 0
  %t16744 = call i64 @nx_mod_i64(i64 %t16742, i64 %t16743)
  %t16745 = load %NxVal, ptr @nx__g___main____i112
  %t16746 = extractvalue %NxVal %t16745, 1
  %t16747 = add i64 %t16744, %t16746
  %t16748 = add i64 65535, 0
  %t16749 = and i64 %t16747, %t16748
  %t16750 = call %NxVal @nx_int(i64 %t16749)
  store %NxVal %t16750, ptr @nx__g___main____acc112
  %t16751 = load %NxVal, ptr @nx__g___main____acc112
  %t16752 = add i64 92, 0
  %t16753 = extractvalue %NxVal %t16751, 1
  %t16754 = add i64 %t16753, %t16752
  %t16755 = add i64 22, 0
  %t16756 = add i64 %t16754, %t16755
  %t16757 = load %NxVal, ptr @nx__g___main____i112
  %t16758 = extractvalue %NxVal %t16757, 1
  %t16759 = add i64 %t16756, %t16758
  %t16760 = add i64 65535, 0
  %t16761 = and i64 %t16759, %t16760
  %t16762 = call %NxVal @nx_int(i64 %t16761)
  store %NxVal %t16762, ptr @nx__g___main____acc112
  %t16763 = load %NxVal, ptr @nx__g___main____acc112
  %t16764 = add i64 52, 0
  %t16765 = extractvalue %NxVal %t16763, 1
  %t16766 = call i64 @nx_mod_i64(i64 %t16765, i64 %t16764)
  %t16767 = add i64 24, 0
  %t16768 = call i64 @nx_mod_i64(i64 %t16766, i64 %t16767)
  %t16769 = load %NxVal, ptr @nx__g___main____i112
  %t16770 = extractvalue %NxVal %t16769, 1
  %t16771 = add i64 %t16768, %t16770
  %t16772 = add i64 65535, 0
  %t16773 = and i64 %t16771, %t16772
  %t16774 = call %NxVal @nx_int(i64 %t16773)
  store %NxVal %t16774, ptr @nx__g___main____acc112
  %t16775 = load %NxVal, ptr @nx__g___main____acc112
  %t16776 = add i64 79, 0
  %t16777 = extractvalue %NxVal %t16775, 1
  %t16778 = mul i64 %t16777, %t16776
  %t16779 = add i64 59, 0
  %t16780 = mul i64 %t16778, %t16779
  %t16781 = load %NxVal, ptr @nx__g___main____i112
  %t16782 = extractvalue %NxVal %t16781, 1
  %t16783 = add i64 %t16780, %t16782
  %t16784 = add i64 65535, 0
  %t16785 = and i64 %t16783, %t16784
  %t16786 = call %NxVal @nx_int(i64 %t16785)
  store %NxVal %t16786, ptr @nx__g___main____acc112
  %t16787 = load %NxVal, ptr @nx__g___main____i112
  %t16788 = add i64 1, 0
  %t16789 = extractvalue %NxVal %t16787, 1
  %t16790 = add i64 %t16789, %t16788
  %t16791 = call %NxVal @nx_int(i64 %t16790)
  store %NxVal %t16791, ptr @nx__g___main____i112
  br label %wcond339
wend341:
  %t16792 = load %NxVal, ptr @nx__g___main____total
  %t16793 = load %NxVal, ptr @nx__g___main____acc112
  %t16794 = extractvalue %NxVal %t16792, 1
  %t16795 = extractvalue %NxVal %t16793, 1
  %t16796 = add i64 %t16794, %t16795
  %t16797 = add i64 65535, 0
  %t16798 = and i64 %t16796, %t16797
  %t16799 = call %NxVal @nx_int(i64 %t16798)
  store %NxVal %t16799, ptr @nx__g___main____total
  %t16800 = add i64 0, 0
  %t16801 = call %NxVal @nx_int(i64 %t16800)
  store %NxVal %t16801, ptr @nx__g___main____i113
  %t16802 = add i64 0, 0
  %t16803 = call %NxVal @nx_int(i64 %t16802)
  store %NxVal %t16803, ptr @nx__g___main____acc113
  br label %wcond342
wcond342:
  %t16804 = load %NxVal, ptr @nx__g___main____i113
  %t16805 = add i64 3, 0
  %t16806 = extractvalue %NxVal %t16804, 1
  %t16807 = icmp slt i64 %t16806, %t16805
  br i1 %t16807, label %wbody343, label %wend344
wbody343:
  %t16808 = load %NxVal, ptr @nx__g___main____acc113
  %t16809 = load %NxVal, ptr @nx__g___main____c
  %t16810 = load %NxVal, ptr @nx__g___main____i113
  %t16811 = add i64 113, 0
  %t16812 = extractvalue %NxVal %t16810, 1
  %t16813 = add i64 %t16812, %t16811
  %t16815 = getelementptr [2 x %NxVal], ptr %t16814, i64 0, i64 0
  store %NxVal %t16809, ptr %t16815
  %t16816 = call %NxVal @nx_int(i64 %t16813)
  %t16817 = getelementptr [2 x %NxVal], ptr %t16814, i64 0, i64 1
  store %NxVal %t16816, ptr %t16817
  %t16818 = getelementptr [2 x %NxVal], ptr %t16814, i64 0, i64 0
  %t16819 = call %NxVal @nx__m_4____main____Cell__m113(ptr %t16818, i64 2)
  %t16820 = extractvalue %NxVal %t16819, 1
  %t16821 = extractvalue %NxVal %t16808, 1
  %t16822 = add i64 %t16821, %t16820
  %t16823 = add i64 65535, 0
  %t16824 = and i64 %t16822, %t16823
  %t16825 = call %NxVal @nx_int(i64 %t16824)
  store %NxVal %t16825, ptr @nx__g___main____acc113
  %t16826 = load %NxVal, ptr @nx__g___main____acc113
  %t16827 = add i64 94, 0
  %t16828 = extractvalue %NxVal %t16826, 1
  %t16829 = xor i64 %t16828, %t16827
  %t16830 = add i64 87, 0
  %t16831 = load %NxVal, ptr @nx__g___main____i113
  %t16832 = extractvalue %NxVal %t16831, 1
  %t16833 = add i64 %t16830, %t16832
  %t16834 = xor i64 %t16829, %t16833
  %t16835 = add i64 65535, 0
  %t16836 = and i64 %t16834, %t16835
  %t16837 = call %NxVal @nx_int(i64 %t16836)
  store %NxVal %t16837, ptr @nx__g___main____acc113
  %t16838 = load %NxVal, ptr @nx__g___main____acc113
  %t16839 = add i64 44, 0
  %t16840 = extractvalue %NxVal %t16838, 1
  %t16841 = mul i64 %t16840, %t16839
  %t16842 = add i64 42, 0
  %t16843 = mul i64 %t16841, %t16842
  %t16844 = load %NxVal, ptr @nx__g___main____i113
  %t16845 = extractvalue %NxVal %t16844, 1
  %t16846 = add i64 %t16843, %t16845
  %t16847 = add i64 65535, 0
  %t16848 = and i64 %t16846, %t16847
  %t16849 = call %NxVal @nx_int(i64 %t16848)
  store %NxVal %t16849, ptr @nx__g___main____acc113
  %t16850 = load %NxVal, ptr @nx__g___main____acc113
  %t16851 = add i64 19, 0
  %t16852 = extractvalue %NxVal %t16850, 1
  %t16853 = xor i64 %t16852, %t16851
  %t16854 = add i64 88, 0
  %t16855 = load %NxVal, ptr @nx__g___main____i113
  %t16856 = extractvalue %NxVal %t16855, 1
  %t16857 = add i64 %t16854, %t16856
  %t16858 = xor i64 %t16853, %t16857
  %t16859 = add i64 65535, 0
  %t16860 = and i64 %t16858, %t16859
  %t16861 = call %NxVal @nx_int(i64 %t16860)
  store %NxVal %t16861, ptr @nx__g___main____acc113
  %t16862 = load %NxVal, ptr @nx__g___main____acc113
  %t16863 = add i64 24, 0
  %t16864 = extractvalue %NxVal %t16862, 1
  %t16865 = sub i64 %t16864, %t16863
  %t16866 = add i64 30, 0
  %t16867 = sub i64 %t16865, %t16866
  %t16868 = load %NxVal, ptr @nx__g___main____i113
  %t16869 = extractvalue %NxVal %t16868, 1
  %t16870 = add i64 %t16867, %t16869
  %t16871 = add i64 65535, 0
  %t16872 = and i64 %t16870, %t16871
  %t16873 = call %NxVal @nx_int(i64 %t16872)
  store %NxVal %t16873, ptr @nx__g___main____acc113
  %t16874 = load %NxVal, ptr @nx__g___main____i113
  %t16875 = add i64 1, 0
  %t16876 = extractvalue %NxVal %t16874, 1
  %t16877 = add i64 %t16876, %t16875
  %t16878 = call %NxVal @nx_int(i64 %t16877)
  store %NxVal %t16878, ptr @nx__g___main____i113
  br label %wcond342
wend344:
  %t16879 = load %NxVal, ptr @nx__g___main____total
  %t16880 = load %NxVal, ptr @nx__g___main____acc113
  %t16881 = extractvalue %NxVal %t16879, 1
  %t16882 = extractvalue %NxVal %t16880, 1
  %t16883 = add i64 %t16881, %t16882
  %t16884 = add i64 65535, 0
  %t16885 = and i64 %t16883, %t16884
  %t16886 = call %NxVal @nx_int(i64 %t16885)
  store %NxVal %t16886, ptr @nx__g___main____total
  %t16887 = add i64 0, 0
  %t16888 = call %NxVal @nx_int(i64 %t16887)
  store %NxVal %t16888, ptr @nx__g___main____i114
  %t16889 = add i64 0, 0
  %t16890 = call %NxVal @nx_int(i64 %t16889)
  store %NxVal %t16890, ptr @nx__g___main____acc114
  br label %wcond345
wcond345:
  %t16891 = load %NxVal, ptr @nx__g___main____i114
  %t16892 = add i64 3, 0
  %t16893 = extractvalue %NxVal %t16891, 1
  %t16894 = icmp slt i64 %t16893, %t16892
  br i1 %t16894, label %wbody346, label %wend347
wbody346:
  %t16895 = load %NxVal, ptr @nx__g___main____acc114
  %t16896 = load %NxVal, ptr @nx__g___main____c
  %t16897 = load %NxVal, ptr @nx__g___main____i114
  %t16898 = add i64 114, 0
  %t16899 = extractvalue %NxVal %t16897, 1
  %t16900 = add i64 %t16899, %t16898
  %t16902 = getelementptr [2 x %NxVal], ptr %t16901, i64 0, i64 0
  store %NxVal %t16896, ptr %t16902
  %t16903 = call %NxVal @nx_int(i64 %t16900)
  %t16904 = getelementptr [2 x %NxVal], ptr %t16901, i64 0, i64 1
  store %NxVal %t16903, ptr %t16904
  %t16905 = getelementptr [2 x %NxVal], ptr %t16901, i64 0, i64 0
  %t16906 = call %NxVal @nx__m_4____main____Cell__m114(ptr %t16905, i64 2)
  %t16907 = extractvalue %NxVal %t16906, 1
  %t16908 = extractvalue %NxVal %t16895, 1
  %t16909 = add i64 %t16908, %t16907
  %t16910 = add i64 65535, 0
  %t16911 = and i64 %t16909, %t16910
  %t16912 = call %NxVal @nx_int(i64 %t16911)
  store %NxVal %t16912, ptr @nx__g___main____acc114
  %t16913 = load %NxVal, ptr @nx__g___main____acc114
  %t16914 = add i64 50, 0
  %t16915 = extractvalue %NxVal %t16913, 1
  %t16916 = xor i64 %t16915, %t16914
  %t16917 = add i64 48, 0
  %t16918 = load %NxVal, ptr @nx__g___main____i114
  %t16919 = extractvalue %NxVal %t16918, 1
  %t16920 = add i64 %t16917, %t16919
  %t16921 = xor i64 %t16916, %t16920
  %t16922 = add i64 65535, 0
  %t16923 = and i64 %t16921, %t16922
  %t16924 = call %NxVal @nx_int(i64 %t16923)
  store %NxVal %t16924, ptr @nx__g___main____acc114
  %t16925 = load %NxVal, ptr @nx__g___main____acc114
  %t16926 = add i64 27, 0
  %t16927 = extractvalue %NxVal %t16925, 1
  %t16928 = add i64 %t16927, %t16926
  %t16929 = add i64 50, 0
  %t16930 = add i64 %t16928, %t16929
  %t16931 = load %NxVal, ptr @nx__g___main____i114
  %t16932 = extractvalue %NxVal %t16931, 1
  %t16933 = add i64 %t16930, %t16932
  %t16934 = add i64 65535, 0
  %t16935 = and i64 %t16933, %t16934
  %t16936 = call %NxVal @nx_int(i64 %t16935)
  store %NxVal %t16936, ptr @nx__g___main____acc114
  %t16937 = load %NxVal, ptr @nx__g___main____acc114
  %t16938 = add i64 92, 0
  %t16939 = extractvalue %NxVal %t16937, 1
  %t16940 = add i64 %t16939, %t16938
  %t16941 = add i64 7, 0
  %t16942 = add i64 %t16940, %t16941
  %t16943 = load %NxVal, ptr @nx__g___main____i114
  %t16944 = extractvalue %NxVal %t16943, 1
  %t16945 = add i64 %t16942, %t16944
  %t16946 = add i64 65535, 0
  %t16947 = and i64 %t16945, %t16946
  %t16948 = call %NxVal @nx_int(i64 %t16947)
  store %NxVal %t16948, ptr @nx__g___main____acc114
  %t16949 = load %NxVal, ptr @nx__g___main____acc114
  %t16950 = add i64 33, 0
  %t16951 = extractvalue %NxVal %t16949, 1
  %t16952 = xor i64 %t16951, %t16950
  %t16953 = add i64 88, 0
  %t16954 = load %NxVal, ptr @nx__g___main____i114
  %t16955 = extractvalue %NxVal %t16954, 1
  %t16956 = add i64 %t16953, %t16955
  %t16957 = xor i64 %t16952, %t16956
  %t16958 = add i64 65535, 0
  %t16959 = and i64 %t16957, %t16958
  %t16960 = call %NxVal @nx_int(i64 %t16959)
  store %NxVal %t16960, ptr @nx__g___main____acc114
  %t16961 = load %NxVal, ptr @nx__g___main____i114
  %t16962 = add i64 1, 0
  %t16963 = extractvalue %NxVal %t16961, 1
  %t16964 = add i64 %t16963, %t16962
  %t16965 = call %NxVal @nx_int(i64 %t16964)
  store %NxVal %t16965, ptr @nx__g___main____i114
  br label %wcond345
wend347:
  %t16966 = load %NxVal, ptr @nx__g___main____total
  %t16967 = load %NxVal, ptr @nx__g___main____acc114
  %t16968 = extractvalue %NxVal %t16966, 1
  %t16969 = extractvalue %NxVal %t16967, 1
  %t16970 = add i64 %t16968, %t16969
  %t16971 = add i64 65535, 0
  %t16972 = and i64 %t16970, %t16971
  %t16973 = call %NxVal @nx_int(i64 %t16972)
  store %NxVal %t16973, ptr @nx__g___main____total
  %t16974 = add i64 0, 0
  %t16975 = call %NxVal @nx_int(i64 %t16974)
  store %NxVal %t16975, ptr @nx__g___main____i115
  %t16976 = add i64 0, 0
  %t16977 = call %NxVal @nx_int(i64 %t16976)
  store %NxVal %t16977, ptr @nx__g___main____acc115
  br label %wcond348
wcond348:
  %t16978 = load %NxVal, ptr @nx__g___main____i115
  %t16979 = add i64 3, 0
  %t16980 = extractvalue %NxVal %t16978, 1
  %t16981 = icmp slt i64 %t16980, %t16979
  br i1 %t16981, label %wbody349, label %wend350
wbody349:
  %t16982 = load %NxVal, ptr @nx__g___main____acc115
  %t16983 = load %NxVal, ptr @nx__g___main____c
  %t16984 = load %NxVal, ptr @nx__g___main____i115
  %t16985 = add i64 115, 0
  %t16986 = extractvalue %NxVal %t16984, 1
  %t16987 = add i64 %t16986, %t16985
  %t16989 = getelementptr [2 x %NxVal], ptr %t16988, i64 0, i64 0
  store %NxVal %t16983, ptr %t16989
  %t16990 = call %NxVal @nx_int(i64 %t16987)
  %t16991 = getelementptr [2 x %NxVal], ptr %t16988, i64 0, i64 1
  store %NxVal %t16990, ptr %t16991
  %t16992 = getelementptr [2 x %NxVal], ptr %t16988, i64 0, i64 0
  %t16993 = call %NxVal @nx__m_4____main____Cell__m115(ptr %t16992, i64 2)
  %t16994 = extractvalue %NxVal %t16993, 1
  %t16995 = extractvalue %NxVal %t16982, 1
  %t16996 = add i64 %t16995, %t16994
  %t16997 = add i64 65535, 0
  %t16998 = and i64 %t16996, %t16997
  %t16999 = call %NxVal @nx_int(i64 %t16998)
  store %NxVal %t16999, ptr @nx__g___main____acc115
  %t17000 = load %NxVal, ptr @nx__g___main____acc115
  %t17001 = add i64 40, 0
  %t17002 = extractvalue %NxVal %t17000, 1
  %t17003 = call i64 @nx_mod_i64(i64 %t17002, i64 %t17001)
  %t17004 = add i64 61, 0
  %t17005 = call i64 @nx_mod_i64(i64 %t17003, i64 %t17004)
  %t17006 = load %NxVal, ptr @nx__g___main____i115
  %t17007 = extractvalue %NxVal %t17006, 1
  %t17008 = add i64 %t17005, %t17007
  %t17009 = add i64 65535, 0
  %t17010 = and i64 %t17008, %t17009
  %t17011 = call %NxVal @nx_int(i64 %t17010)
  store %NxVal %t17011, ptr @nx__g___main____acc115
  %t17012 = load %NxVal, ptr @nx__g___main____acc115
  %t17013 = add i64 4, 0
  %t17014 = extractvalue %NxVal %t17012, 1
  %t17015 = xor i64 %t17014, %t17013
  %t17016 = add i64 27, 0
  %t17017 = load %NxVal, ptr @nx__g___main____i115
  %t17018 = extractvalue %NxVal %t17017, 1
  %t17019 = add i64 %t17016, %t17018
  %t17020 = xor i64 %t17015, %t17019
  %t17021 = add i64 65535, 0
  %t17022 = and i64 %t17020, %t17021
  %t17023 = call %NxVal @nx_int(i64 %t17022)
  store %NxVal %t17023, ptr @nx__g___main____acc115
  %t17024 = load %NxVal, ptr @nx__g___main____acc115
  %t17025 = add i64 51, 0
  %t17026 = extractvalue %NxVal %t17024, 1
  %t17027 = add i64 %t17026, %t17025
  %t17028 = add i64 24, 0
  %t17029 = add i64 %t17027, %t17028
  %t17030 = load %NxVal, ptr @nx__g___main____i115
  %t17031 = extractvalue %NxVal %t17030, 1
  %t17032 = add i64 %t17029, %t17031
  %t17033 = add i64 65535, 0
  %t17034 = and i64 %t17032, %t17033
  %t17035 = call %NxVal @nx_int(i64 %t17034)
  store %NxVal %t17035, ptr @nx__g___main____acc115
  %t17036 = load %NxVal, ptr @nx__g___main____acc115
  %t17037 = add i64 29, 0
  %t17038 = extractvalue %NxVal %t17036, 1
  %t17039 = or i64 %t17038, %t17037
  %t17040 = add i64 15, 0
  %t17041 = load %NxVal, ptr @nx__g___main____i115
  %t17042 = extractvalue %NxVal %t17041, 1
  %t17043 = add i64 %t17040, %t17042
  %t17044 = or i64 %t17039, %t17043
  %t17045 = add i64 65535, 0
  %t17046 = and i64 %t17044, %t17045
  %t17047 = call %NxVal @nx_int(i64 %t17046)
  store %NxVal %t17047, ptr @nx__g___main____acc115
  %t17048 = load %NxVal, ptr @nx__g___main____i115
  %t17049 = add i64 1, 0
  %t17050 = extractvalue %NxVal %t17048, 1
  %t17051 = add i64 %t17050, %t17049
  %t17052 = call %NxVal @nx_int(i64 %t17051)
  store %NxVal %t17052, ptr @nx__g___main____i115
  br label %wcond348
wend350:
  %t17053 = load %NxVal, ptr @nx__g___main____total
  %t17054 = load %NxVal, ptr @nx__g___main____acc115
  %t17055 = extractvalue %NxVal %t17053, 1
  %t17056 = extractvalue %NxVal %t17054, 1
  %t17057 = add i64 %t17055, %t17056
  %t17058 = add i64 65535, 0
  %t17059 = and i64 %t17057, %t17058
  %t17060 = call %NxVal @nx_int(i64 %t17059)
  store %NxVal %t17060, ptr @nx__g___main____total
  %t17061 = add i64 0, 0
  %t17062 = call %NxVal @nx_int(i64 %t17061)
  store %NxVal %t17062, ptr @nx__g___main____i116
  %t17063 = add i64 0, 0
  %t17064 = call %NxVal @nx_int(i64 %t17063)
  store %NxVal %t17064, ptr @nx__g___main____acc116
  br label %wcond351
wcond351:
  %t17065 = load %NxVal, ptr @nx__g___main____i116
  %t17066 = add i64 3, 0
  %t17067 = extractvalue %NxVal %t17065, 1
  %t17068 = icmp slt i64 %t17067, %t17066
  br i1 %t17068, label %wbody352, label %wend353
wbody352:
  %t17069 = load %NxVal, ptr @nx__g___main____acc116
  %t17070 = load %NxVal, ptr @nx__g___main____c
  %t17071 = load %NxVal, ptr @nx__g___main____i116
  %t17072 = add i64 116, 0
  %t17073 = extractvalue %NxVal %t17071, 1
  %t17074 = add i64 %t17073, %t17072
  %t17076 = getelementptr [2 x %NxVal], ptr %t17075, i64 0, i64 0
  store %NxVal %t17070, ptr %t17076
  %t17077 = call %NxVal @nx_int(i64 %t17074)
  %t17078 = getelementptr [2 x %NxVal], ptr %t17075, i64 0, i64 1
  store %NxVal %t17077, ptr %t17078
  %t17079 = getelementptr [2 x %NxVal], ptr %t17075, i64 0, i64 0
  %t17080 = call %NxVal @nx__m_4____main____Cell__m116(ptr %t17079, i64 2)
  %t17081 = extractvalue %NxVal %t17080, 1
  %t17082 = extractvalue %NxVal %t17069, 1
  %t17083 = add i64 %t17082, %t17081
  %t17084 = add i64 65535, 0
  %t17085 = and i64 %t17083, %t17084
  %t17086 = call %NxVal @nx_int(i64 %t17085)
  store %NxVal %t17086, ptr @nx__g___main____acc116
  %t17087 = load %NxVal, ptr @nx__g___main____acc116
  %t17088 = add i64 36, 0
  %t17089 = extractvalue %NxVal %t17087, 1
  %t17090 = call i64 @nx_mod_i64(i64 %t17089, i64 %t17088)
  %t17091 = add i64 32, 0
  %t17092 = call i64 @nx_mod_i64(i64 %t17090, i64 %t17091)
  %t17093 = load %NxVal, ptr @nx__g___main____i116
  %t17094 = extractvalue %NxVal %t17093, 1
  %t17095 = add i64 %t17092, %t17094
  %t17096 = add i64 65535, 0
  %t17097 = and i64 %t17095, %t17096
  %t17098 = call %NxVal @nx_int(i64 %t17097)
  store %NxVal %t17098, ptr @nx__g___main____acc116
  %t17099 = load %NxVal, ptr @nx__g___main____acc116
  %t17100 = add i64 21, 0
  %t17101 = extractvalue %NxVal %t17099, 1
  %t17102 = or i64 %t17101, %t17100
  %t17103 = add i64 57, 0
  %t17104 = load %NxVal, ptr @nx__g___main____i116
  %t17105 = extractvalue %NxVal %t17104, 1
  %t17106 = add i64 %t17103, %t17105
  %t17107 = or i64 %t17102, %t17106
  %t17108 = add i64 65535, 0
  %t17109 = and i64 %t17107, %t17108
  %t17110 = call %NxVal @nx_int(i64 %t17109)
  store %NxVal %t17110, ptr @nx__g___main____acc116
  %t17111 = load %NxVal, ptr @nx__g___main____acc116
  %t17112 = add i64 3, 0
  %t17113 = extractvalue %NxVal %t17111, 1
  %t17114 = sub i64 %t17113, %t17112
  %t17115 = add i64 4, 0
  %t17116 = sub i64 %t17114, %t17115
  %t17117 = load %NxVal, ptr @nx__g___main____i116
  %t17118 = extractvalue %NxVal %t17117, 1
  %t17119 = add i64 %t17116, %t17118
  %t17120 = add i64 65535, 0
  %t17121 = and i64 %t17119, %t17120
  %t17122 = call %NxVal @nx_int(i64 %t17121)
  store %NxVal %t17122, ptr @nx__g___main____acc116
  %t17123 = load %NxVal, ptr @nx__g___main____acc116
  %t17124 = add i64 59, 0
  %t17125 = extractvalue %NxVal %t17123, 1
  %t17126 = call i64 @nx_mod_i64(i64 %t17125, i64 %t17124)
  %t17127 = add i64 83, 0
  %t17128 = call i64 @nx_mod_i64(i64 %t17126, i64 %t17127)
  %t17129 = load %NxVal, ptr @nx__g___main____i116
  %t17130 = extractvalue %NxVal %t17129, 1
  %t17131 = add i64 %t17128, %t17130
  %t17132 = add i64 65535, 0
  %t17133 = and i64 %t17131, %t17132
  %t17134 = call %NxVal @nx_int(i64 %t17133)
  store %NxVal %t17134, ptr @nx__g___main____acc116
  %t17135 = load %NxVal, ptr @nx__g___main____i116
  %t17136 = add i64 1, 0
  %t17137 = extractvalue %NxVal %t17135, 1
  %t17138 = add i64 %t17137, %t17136
  %t17139 = call %NxVal @nx_int(i64 %t17138)
  store %NxVal %t17139, ptr @nx__g___main____i116
  br label %wcond351
wend353:
  %t17140 = load %NxVal, ptr @nx__g___main____total
  %t17141 = load %NxVal, ptr @nx__g___main____acc116
  %t17142 = extractvalue %NxVal %t17140, 1
  %t17143 = extractvalue %NxVal %t17141, 1
  %t17144 = add i64 %t17142, %t17143
  %t17145 = add i64 65535, 0
  %t17146 = and i64 %t17144, %t17145
  %t17147 = call %NxVal @nx_int(i64 %t17146)
  store %NxVal %t17147, ptr @nx__g___main____total
  %t17148 = add i64 0, 0
  %t17149 = call %NxVal @nx_int(i64 %t17148)
  store %NxVal %t17149, ptr @nx__g___main____i117
  %t17150 = add i64 0, 0
  %t17151 = call %NxVal @nx_int(i64 %t17150)
  store %NxVal %t17151, ptr @nx__g___main____acc117
  br label %wcond354
wcond354:
  %t17152 = load %NxVal, ptr @nx__g___main____i117
  %t17153 = add i64 3, 0
  %t17154 = extractvalue %NxVal %t17152, 1
  %t17155 = icmp slt i64 %t17154, %t17153
  br i1 %t17155, label %wbody355, label %wend356
wbody355:
  %t17156 = load %NxVal, ptr @nx__g___main____acc117
  %t17157 = load %NxVal, ptr @nx__g___main____c
  %t17158 = load %NxVal, ptr @nx__g___main____i117
  %t17159 = add i64 117, 0
  %t17160 = extractvalue %NxVal %t17158, 1
  %t17161 = add i64 %t17160, %t17159
  %t17163 = getelementptr [2 x %NxVal], ptr %t17162, i64 0, i64 0
  store %NxVal %t17157, ptr %t17163
  %t17164 = call %NxVal @nx_int(i64 %t17161)
  %t17165 = getelementptr [2 x %NxVal], ptr %t17162, i64 0, i64 1
  store %NxVal %t17164, ptr %t17165
  %t17166 = getelementptr [2 x %NxVal], ptr %t17162, i64 0, i64 0
  %t17167 = call %NxVal @nx__m_4____main____Cell__m117(ptr %t17166, i64 2)
  %t17168 = extractvalue %NxVal %t17167, 1
  %t17169 = extractvalue %NxVal %t17156, 1
  %t17170 = add i64 %t17169, %t17168
  %t17171 = add i64 65535, 0
  %t17172 = and i64 %t17170, %t17171
  %t17173 = call %NxVal @nx_int(i64 %t17172)
  store %NxVal %t17173, ptr @nx__g___main____acc117
  %t17174 = load %NxVal, ptr @nx__g___main____acc117
  %t17175 = add i64 21, 0
  %t17176 = extractvalue %NxVal %t17174, 1
  %t17177 = mul i64 %t17176, %t17175
  %t17178 = add i64 67, 0
  %t17179 = mul i64 %t17177, %t17178
  %t17180 = load %NxVal, ptr @nx__g___main____i117
  %t17181 = extractvalue %NxVal %t17180, 1
  %t17182 = add i64 %t17179, %t17181
  %t17183 = add i64 65535, 0
  %t17184 = and i64 %t17182, %t17183
  %t17185 = call %NxVal @nx_int(i64 %t17184)
  store %NxVal %t17185, ptr @nx__g___main____acc117
  %t17186 = load %NxVal, ptr @nx__g___main____acc117
  %t17187 = add i64 72, 0
  %t17188 = extractvalue %NxVal %t17186, 1
  %t17189 = and i64 %t17188, %t17187
  %t17190 = add i64 77, 0
  %t17191 = load %NxVal, ptr @nx__g___main____i117
  %t17192 = extractvalue %NxVal %t17191, 1
  %t17193 = add i64 %t17190, %t17192
  %t17194 = and i64 %t17189, %t17193
  %t17195 = add i64 65535, 0
  %t17196 = and i64 %t17194, %t17195
  %t17197 = call %NxVal @nx_int(i64 %t17196)
  store %NxVal %t17197, ptr @nx__g___main____acc117
  %t17198 = load %NxVal, ptr @nx__g___main____acc117
  %t17199 = add i64 66, 0
  %t17200 = extractvalue %NxVal %t17198, 1
  %t17201 = xor i64 %t17200, %t17199
  %t17202 = add i64 9, 0
  %t17203 = load %NxVal, ptr @nx__g___main____i117
  %t17204 = extractvalue %NxVal %t17203, 1
  %t17205 = add i64 %t17202, %t17204
  %t17206 = xor i64 %t17201, %t17205
  %t17207 = add i64 65535, 0
  %t17208 = and i64 %t17206, %t17207
  %t17209 = call %NxVal @nx_int(i64 %t17208)
  store %NxVal %t17209, ptr @nx__g___main____acc117
  %t17210 = load %NxVal, ptr @nx__g___main____acc117
  %t17211 = add i64 67, 0
  %t17212 = extractvalue %NxVal %t17210, 1
  %t17213 = sub i64 %t17212, %t17211
  %t17214 = add i64 89, 0
  %t17215 = sub i64 %t17213, %t17214
  %t17216 = load %NxVal, ptr @nx__g___main____i117
  %t17217 = extractvalue %NxVal %t17216, 1
  %t17218 = add i64 %t17215, %t17217
  %t17219 = add i64 65535, 0
  %t17220 = and i64 %t17218, %t17219
  %t17221 = call %NxVal @nx_int(i64 %t17220)
  store %NxVal %t17221, ptr @nx__g___main____acc117
  %t17222 = load %NxVal, ptr @nx__g___main____i117
  %t17223 = add i64 1, 0
  %t17224 = extractvalue %NxVal %t17222, 1
  %t17225 = add i64 %t17224, %t17223
  %t17226 = call %NxVal @nx_int(i64 %t17225)
  store %NxVal %t17226, ptr @nx__g___main____i117
  br label %wcond354
wend356:
  %t17227 = load %NxVal, ptr @nx__g___main____total
  %t17228 = load %NxVal, ptr @nx__g___main____acc117
  %t17229 = extractvalue %NxVal %t17227, 1
  %t17230 = extractvalue %NxVal %t17228, 1
  %t17231 = add i64 %t17229, %t17230
  %t17232 = add i64 65535, 0
  %t17233 = and i64 %t17231, %t17232
  %t17234 = call %NxVal @nx_int(i64 %t17233)
  store %NxVal %t17234, ptr @nx__g___main____total
  %t17235 = add i64 0, 0
  %t17236 = call %NxVal @nx_int(i64 %t17235)
  store %NxVal %t17236, ptr @nx__g___main____i118
  %t17237 = add i64 0, 0
  %t17238 = call %NxVal @nx_int(i64 %t17237)
  store %NxVal %t17238, ptr @nx__g___main____acc118
  br label %wcond357
wcond357:
  %t17239 = load %NxVal, ptr @nx__g___main____i118
  %t17240 = add i64 3, 0
  %t17241 = extractvalue %NxVal %t17239, 1
  %t17242 = icmp slt i64 %t17241, %t17240
  br i1 %t17242, label %wbody358, label %wend359
wbody358:
  %t17243 = load %NxVal, ptr @nx__g___main____acc118
  %t17244 = load %NxVal, ptr @nx__g___main____c
  %t17245 = load %NxVal, ptr @nx__g___main____i118
  %t17246 = add i64 118, 0
  %t17247 = extractvalue %NxVal %t17245, 1
  %t17248 = add i64 %t17247, %t17246
  %t17250 = getelementptr [2 x %NxVal], ptr %t17249, i64 0, i64 0
  store %NxVal %t17244, ptr %t17250
  %t17251 = call %NxVal @nx_int(i64 %t17248)
  %t17252 = getelementptr [2 x %NxVal], ptr %t17249, i64 0, i64 1
  store %NxVal %t17251, ptr %t17252
  %t17253 = getelementptr [2 x %NxVal], ptr %t17249, i64 0, i64 0
  %t17254 = call %NxVal @nx__m_4____main____Cell__m118(ptr %t17253, i64 2)
  %t17255 = extractvalue %NxVal %t17254, 1
  %t17256 = extractvalue %NxVal %t17243, 1
  %t17257 = add i64 %t17256, %t17255
  %t17258 = add i64 65535, 0
  %t17259 = and i64 %t17257, %t17258
  %t17260 = call %NxVal @nx_int(i64 %t17259)
  store %NxVal %t17260, ptr @nx__g___main____acc118
  %t17261 = load %NxVal, ptr @nx__g___main____acc118
  %t17262 = add i64 67, 0
  %t17263 = extractvalue %NxVal %t17261, 1
  %t17264 = add i64 %t17263, %t17262
  %t17265 = add i64 10, 0
  %t17266 = add i64 %t17264, %t17265
  %t17267 = load %NxVal, ptr @nx__g___main____i118
  %t17268 = extractvalue %NxVal %t17267, 1
  %t17269 = add i64 %t17266, %t17268
  %t17270 = add i64 65535, 0
  %t17271 = and i64 %t17269, %t17270
  %t17272 = call %NxVal @nx_int(i64 %t17271)
  store %NxVal %t17272, ptr @nx__g___main____acc118
  %t17273 = load %NxVal, ptr @nx__g___main____acc118
  %t17274 = add i64 53, 0
  %t17275 = extractvalue %NxVal %t17273, 1
  %t17276 = and i64 %t17275, %t17274
  %t17277 = add i64 75, 0
  %t17278 = load %NxVal, ptr @nx__g___main____i118
  %t17279 = extractvalue %NxVal %t17278, 1
  %t17280 = add i64 %t17277, %t17279
  %t17281 = and i64 %t17276, %t17280
  %t17282 = add i64 65535, 0
  %t17283 = and i64 %t17281, %t17282
  %t17284 = call %NxVal @nx_int(i64 %t17283)
  store %NxVal %t17284, ptr @nx__g___main____acc118
  %t17285 = load %NxVal, ptr @nx__g___main____acc118
  %t17286 = add i64 36, 0
  %t17287 = extractvalue %NxVal %t17285, 1
  %t17288 = mul i64 %t17287, %t17286
  %t17289 = add i64 31, 0
  %t17290 = mul i64 %t17288, %t17289
  %t17291 = load %NxVal, ptr @nx__g___main____i118
  %t17292 = extractvalue %NxVal %t17291, 1
  %t17293 = add i64 %t17290, %t17292
  %t17294 = add i64 65535, 0
  %t17295 = and i64 %t17293, %t17294
  %t17296 = call %NxVal @nx_int(i64 %t17295)
  store %NxVal %t17296, ptr @nx__g___main____acc118
  %t17297 = load %NxVal, ptr @nx__g___main____acc118
  %t17298 = add i64 7, 0
  %t17299 = extractvalue %NxVal %t17297, 1
  %t17300 = call i64 @nx_mod_i64(i64 %t17299, i64 %t17298)
  %t17301 = add i64 31, 0
  %t17302 = call i64 @nx_mod_i64(i64 %t17300, i64 %t17301)
  %t17303 = load %NxVal, ptr @nx__g___main____i118
  %t17304 = extractvalue %NxVal %t17303, 1
  %t17305 = add i64 %t17302, %t17304
  %t17306 = add i64 65535, 0
  %t17307 = and i64 %t17305, %t17306
  %t17308 = call %NxVal @nx_int(i64 %t17307)
  store %NxVal %t17308, ptr @nx__g___main____acc118
  %t17309 = load %NxVal, ptr @nx__g___main____i118
  %t17310 = add i64 1, 0
  %t17311 = extractvalue %NxVal %t17309, 1
  %t17312 = add i64 %t17311, %t17310
  %t17313 = call %NxVal @nx_int(i64 %t17312)
  store %NxVal %t17313, ptr @nx__g___main____i118
  br label %wcond357
wend359:
  %t17314 = load %NxVal, ptr @nx__g___main____total
  %t17315 = load %NxVal, ptr @nx__g___main____acc118
  %t17316 = extractvalue %NxVal %t17314, 1
  %t17317 = extractvalue %NxVal %t17315, 1
  %t17318 = add i64 %t17316, %t17317
  %t17319 = add i64 65535, 0
  %t17320 = and i64 %t17318, %t17319
  %t17321 = call %NxVal @nx_int(i64 %t17320)
  store %NxVal %t17321, ptr @nx__g___main____total
  %t17322 = add i64 0, 0
  %t17323 = call %NxVal @nx_int(i64 %t17322)
  store %NxVal %t17323, ptr @nx__g___main____i119
  %t17324 = add i64 0, 0
  %t17325 = call %NxVal @nx_int(i64 %t17324)
  store %NxVal %t17325, ptr @nx__g___main____acc119
  br label %wcond360
wcond360:
  %t17326 = load %NxVal, ptr @nx__g___main____i119
  %t17327 = add i64 3, 0
  %t17328 = extractvalue %NxVal %t17326, 1
  %t17329 = icmp slt i64 %t17328, %t17327
  br i1 %t17329, label %wbody361, label %wend362
wbody361:
  %t17330 = load %NxVal, ptr @nx__g___main____acc119
  %t17331 = load %NxVal, ptr @nx__g___main____c
  %t17332 = load %NxVal, ptr @nx__g___main____i119
  %t17333 = add i64 119, 0
  %t17334 = extractvalue %NxVal %t17332, 1
  %t17335 = add i64 %t17334, %t17333
  %t17337 = getelementptr [2 x %NxVal], ptr %t17336, i64 0, i64 0
  store %NxVal %t17331, ptr %t17337
  %t17338 = call %NxVal @nx_int(i64 %t17335)
  %t17339 = getelementptr [2 x %NxVal], ptr %t17336, i64 0, i64 1
  store %NxVal %t17338, ptr %t17339
  %t17340 = getelementptr [2 x %NxVal], ptr %t17336, i64 0, i64 0
  %t17341 = call %NxVal @nx__m_4____main____Cell__m119(ptr %t17340, i64 2)
  %t17342 = extractvalue %NxVal %t17341, 1
  %t17343 = extractvalue %NxVal %t17330, 1
  %t17344 = add i64 %t17343, %t17342
  %t17345 = add i64 65535, 0
  %t17346 = and i64 %t17344, %t17345
  %t17347 = call %NxVal @nx_int(i64 %t17346)
  store %NxVal %t17347, ptr @nx__g___main____acc119
  %t17348 = load %NxVal, ptr @nx__g___main____acc119
  %t17349 = add i64 16, 0
  %t17350 = extractvalue %NxVal %t17348, 1
  %t17351 = xor i64 %t17350, %t17349
  %t17352 = add i64 86, 0
  %t17353 = load %NxVal, ptr @nx__g___main____i119
  %t17354 = extractvalue %NxVal %t17353, 1
  %t17355 = add i64 %t17352, %t17354
  %t17356 = xor i64 %t17351, %t17355
  %t17357 = add i64 65535, 0
  %t17358 = and i64 %t17356, %t17357
  %t17359 = call %NxVal @nx_int(i64 %t17358)
  store %NxVal %t17359, ptr @nx__g___main____acc119
  %t17360 = load %NxVal, ptr @nx__g___main____acc119
  %t17361 = add i64 24, 0
  %t17362 = extractvalue %NxVal %t17360, 1
  %t17363 = or i64 %t17362, %t17361
  %t17364 = add i64 83, 0
  %t17365 = load %NxVal, ptr @nx__g___main____i119
  %t17366 = extractvalue %NxVal %t17365, 1
  %t17367 = add i64 %t17364, %t17366
  %t17368 = or i64 %t17363, %t17367
  %t17369 = add i64 65535, 0
  %t17370 = and i64 %t17368, %t17369
  %t17371 = call %NxVal @nx_int(i64 %t17370)
  store %NxVal %t17371, ptr @nx__g___main____acc119
  %t17372 = load %NxVal, ptr @nx__g___main____acc119
  %t17373 = add i64 19, 0
  %t17374 = extractvalue %NxVal %t17372, 1
  %t17375 = or i64 %t17374, %t17373
  %t17376 = add i64 71, 0
  %t17377 = load %NxVal, ptr @nx__g___main____i119
  %t17378 = extractvalue %NxVal %t17377, 1
  %t17379 = add i64 %t17376, %t17378
  %t17380 = or i64 %t17375, %t17379
  %t17381 = add i64 65535, 0
  %t17382 = and i64 %t17380, %t17381
  %t17383 = call %NxVal @nx_int(i64 %t17382)
  store %NxVal %t17383, ptr @nx__g___main____acc119
  %t17384 = load %NxVal, ptr @nx__g___main____acc119
  %t17385 = add i64 32, 0
  %t17386 = extractvalue %NxVal %t17384, 1
  %t17387 = mul i64 %t17386, %t17385
  %t17388 = add i64 43, 0
  %t17389 = mul i64 %t17387, %t17388
  %t17390 = load %NxVal, ptr @nx__g___main____i119
  %t17391 = extractvalue %NxVal %t17390, 1
  %t17392 = add i64 %t17389, %t17391
  %t17393 = add i64 65535, 0
  %t17394 = and i64 %t17392, %t17393
  %t17395 = call %NxVal @nx_int(i64 %t17394)
  store %NxVal %t17395, ptr @nx__g___main____acc119
  %t17396 = load %NxVal, ptr @nx__g___main____i119
  %t17397 = add i64 1, 0
  %t17398 = extractvalue %NxVal %t17396, 1
  %t17399 = add i64 %t17398, %t17397
  %t17400 = call %NxVal @nx_int(i64 %t17399)
  store %NxVal %t17400, ptr @nx__g___main____i119
  br label %wcond360
wend362:
  %t17401 = load %NxVal, ptr @nx__g___main____total
  %t17402 = load %NxVal, ptr @nx__g___main____acc119
  %t17403 = extractvalue %NxVal %t17401, 1
  %t17404 = extractvalue %NxVal %t17402, 1
  %t17405 = add i64 %t17403, %t17404
  %t17406 = add i64 65535, 0
  %t17407 = and i64 %t17405, %t17406
  %t17408 = call %NxVal @nx_int(i64 %t17407)
  store %NxVal %t17408, ptr @nx__g___main____total
  %t17410 = load %NxVal, ptr @nx__g___main____total
  %t17411 = getelementptr [1 x %NxVal], ptr %t17409, i64 0, i64 0
  store %NxVal %t17410, ptr %t17411
  %t17412 = getelementptr [1 x %NxVal], ptr %t17409, i64 0, i64 0
  call void @nx_print(ptr %t17412, i64 1)
  %t17414 = load %NxVal, ptr @nx__g___main____c
  %t17415 = add i64 1, 0
  %t17417 = getelementptr [2 x %NxVal], ptr %t17416, i64 0, i64 0
  store %NxVal %t17414, ptr %t17417
  %t17418 = call %NxVal @nx_int(i64 %t17415)
  %t17419 = getelementptr [2 x %NxVal], ptr %t17416, i64 0, i64 1
  store %NxVal %t17418, ptr %t17419
  %t17420 = getelementptr [2 x %NxVal], ptr %t17416, i64 0, i64 0
  %t17421 = call %NxVal @nx__m_2____main____Cell__m0(ptr %t17420, i64 2)
  %t17422 = extractvalue %NxVal %t17421, 1
  %t17423 = call %NxVal @nx_int(i64 %t17422)
  %t17424 = getelementptr [2 x %NxVal], ptr %t17413, i64 0, i64 0
  store %NxVal %t17423, ptr %t17424
  %t17425 = load %NxVal, ptr @nx__g___main____c
  %t17426 = add i64 2, 0
  %t17428 = getelementptr [2 x %NxVal], ptr %t17427, i64 0, i64 0
  store %NxVal %t17425, ptr %t17428
  %t17429 = call %NxVal @nx_int(i64 %t17426)
  %t17430 = getelementptr [2 x %NxVal], ptr %t17427, i64 0, i64 1
  store %NxVal %t17429, ptr %t17430
  %t17431 = getelementptr [2 x %NxVal], ptr %t17427, i64 0, i64 0
  %t17432 = call %NxVal @nx__m_2____main____Cell__m0(ptr %t17431, i64 2)
  %t17433 = extractvalue %NxVal %t17432, 1
  %t17434 = call %NxVal @nx_int(i64 %t17433)
  %t17435 = getelementptr [2 x %NxVal], ptr %t17413, i64 0, i64 1
  store %NxVal %t17434, ptr %t17435
  %t17436 = getelementptr [2 x %NxVal], ptr %t17413, i64 0, i64 0
  call void @nx_print(ptr %t17436, i64 2)
  ret void
initskip2:
  ret void
}
define i32 @main() {
entry:
  call void @nx__init___main__()
  ret i32 0
}
