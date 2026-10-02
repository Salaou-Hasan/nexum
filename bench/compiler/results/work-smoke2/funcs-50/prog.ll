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

@nx__g___main____total = global %NxVal zeroinitializer
@nx__done___main__ = global i1 false
define %NxVal @nx__f_2____main____f0(%NxVal* %args, i64 %nargs) {
entry:
  %t1 = alloca %NxVal
  %t5 = alloca i64
  %t9 = alloca i64
  %t20 = alloca i64
  store %NxVal zeroinitializer, ptr %t1
  %t2 = call i1 @nx_memo_get(i64 1, ptr %args, i64 %nargs, ptr %t1)
  br i1 %t2, label %mhit1, label %mmiss2
mhit1:
  %t3 = load %NxVal, ptr %t1
  %t4 = call %NxVal @nx_clone(%NxVal %t3)
  ret %NxVal %t4
mmiss2:
  %t6 = getelementptr %NxVal, ptr %args, i64 0
  %t7 = load %NxVal, ptr %t6
  %t8 = extractvalue %NxVal %t7, 1
  store i64 %t8, ptr %t5
  %t10 = getelementptr %NxVal, ptr %args, i64 1
  %t11 = load %NxVal, ptr %t10
  %t12 = extractvalue %NxVal %t11, 1
  store i64 %t12, ptr %t9
  %t13 = load i64, ptr %t5
  %t14 = load i64, ptr %t9
  %t15 = add i64 %t13, %t14
  %t16 = add i64 0, 0
  %t17 = add i64 %t15, %t16
  %t18 = add i64 65535, 0
  %t19 = and i64 %t17, %t18
  store i64 %t19, ptr %t20
  %t21 = load i64, ptr %t20
  %t22 = add i64 70, 0
  %t23 = and i64 %t21, %t22
  %t24 = add i64 40, 0
  %t25 = and i64 %t23, %t24
  %t26 = add i64 65535, 0
  %t27 = and i64 %t25, %t26
  store i64 %t27, ptr %t20
  %t28 = load i64, ptr %t20
  %t29 = add i64 16, 0
  %t30 = or i64 %t28, %t29
  %t31 = add i64 67, 0
  %t32 = or i64 %t30, %t31
  %t33 = add i64 65535, 0
  %t34 = and i64 %t32, %t33
  store i64 %t34, ptr %t20
  %t35 = load i64, ptr %t20
  %t36 = add i64 56, 0
  %t37 = or i64 %t35, %t36
  %t38 = add i64 44, 0
  %t39 = or i64 %t37, %t38
  %t40 = add i64 65535, 0
  %t41 = and i64 %t39, %t40
  store i64 %t41, ptr %t20
  %t42 = load i64, ptr %t20
  %t43 = add i64 94, 0
  %t44 = mul i64 %t42, %t43
  %t45 = add i64 65, 0
  %t46 = mul i64 %t44, %t45
  %t47 = add i64 65535, 0
  %t48 = and i64 %t46, %t47
  store i64 %t48, ptr %t20
  %t49 = load i64, ptr %t20
  %t50 = add i64 12, 0
  %t51 = xor i64 %t49, %t50
  %t52 = add i64 5, 0
  %t53 = xor i64 %t51, %t52
  %t54 = add i64 65535, 0
  %t55 = and i64 %t53, %t54
  store i64 %t55, ptr %t20
  %t56 = load i64, ptr %t20
  %t57 = add i64 84, 0
  %t58 = mul i64 %t56, %t57
  %t59 = add i64 28, 0
  %t60 = mul i64 %t58, %t59
  %t61 = add i64 65535, 0
  %t62 = and i64 %t60, %t61
  store i64 %t62, ptr %t20
  %t63 = load i64, ptr %t20
  %t64 = add i64 3, 0
  %t65 = add i64 %t63, %t64
  %t66 = add i64 48, 0
  %t67 = add i64 %t65, %t66
  %t68 = add i64 65535, 0
  %t69 = and i64 %t67, %t68
  store i64 %t69, ptr %t20
  %t70 = load i64, ptr %t20
  %t71 = call %NxVal @nx_int(i64 %t70)
  call void @nx_memo_put(i64 1, ptr %args, i64 %nargs, %NxVal %t71)
  ret %NxVal %t71
}
define %NxVal @nx__f_2____main____f1(%NxVal* %args, i64 %nargs) {
entry:
  %t72 = alloca %NxVal
  %t76 = alloca i64
  %t80 = alloca i64
  %t91 = alloca i64
  store %NxVal zeroinitializer, ptr %t72
  %t73 = call i1 @nx_memo_get(i64 2, ptr %args, i64 %nargs, ptr %t72)
  br i1 %t73, label %mhit3, label %mmiss4
mhit3:
  %t74 = load %NxVal, ptr %t72
  %t75 = call %NxVal @nx_clone(%NxVal %t74)
  ret %NxVal %t75
mmiss4:
  %t77 = getelementptr %NxVal, ptr %args, i64 0
  %t78 = load %NxVal, ptr %t77
  %t79 = extractvalue %NxVal %t78, 1
  store i64 %t79, ptr %t76
  %t81 = getelementptr %NxVal, ptr %args, i64 1
  %t82 = load %NxVal, ptr %t81
  %t83 = extractvalue %NxVal %t82, 1
  store i64 %t83, ptr %t80
  %t84 = load i64, ptr %t76
  %t85 = load i64, ptr %t80
  %t86 = add i64 %t84, %t85
  %t87 = add i64 1, 0
  %t88 = add i64 %t86, %t87
  %t89 = add i64 65535, 0
  %t90 = and i64 %t88, %t89
  store i64 %t90, ptr %t91
  %t92 = load i64, ptr %t91
  %t93 = add i64 51, 0
  %t94 = add i64 %t92, %t93
  %t95 = add i64 40, 0
  %t96 = add i64 %t94, %t95
  %t97 = add i64 65535, 0
  %t98 = and i64 %t96, %t97
  store i64 %t98, ptr %t91
  %t99 = load i64, ptr %t91
  %t100 = add i64 2, 0
  %t101 = and i64 %t99, %t100
  %t102 = add i64 14, 0
  %t103 = and i64 %t101, %t102
  %t104 = add i64 65535, 0
  %t105 = and i64 %t103, %t104
  store i64 %t105, ptr %t91
  %t106 = load i64, ptr %t91
  %t107 = add i64 3, 0
  %t108 = or i64 %t106, %t107
  %t109 = add i64 72, 0
  %t110 = or i64 %t108, %t109
  %t111 = add i64 65535, 0
  %t112 = and i64 %t110, %t111
  store i64 %t112, ptr %t91
  %t113 = load i64, ptr %t91
  %t114 = add i64 88, 0
  %t115 = sub i64 %t113, %t114
  %t116 = add i64 76, 0
  %t117 = sub i64 %t115, %t116
  %t118 = add i64 65535, 0
  %t119 = and i64 %t117, %t118
  store i64 %t119, ptr %t91
  %t120 = load i64, ptr %t91
  %t121 = add i64 46, 0
  %t122 = or i64 %t120, %t121
  %t123 = add i64 64, 0
  %t124 = or i64 %t122, %t123
  %t125 = add i64 65535, 0
  %t126 = and i64 %t124, %t125
  store i64 %t126, ptr %t91
  %t127 = load i64, ptr %t91
  %t128 = add i64 10, 0
  %t129 = xor i64 %t127, %t128
  %t130 = add i64 47, 0
  %t131 = xor i64 %t129, %t130
  %t132 = add i64 65535, 0
  %t133 = and i64 %t131, %t132
  store i64 %t133, ptr %t91
  %t134 = load i64, ptr %t91
  %t135 = add i64 37, 0
  %t136 = and i64 %t134, %t135
  %t137 = add i64 15, 0
  %t138 = and i64 %t136, %t137
  %t139 = add i64 65535, 0
  %t140 = and i64 %t138, %t139
  store i64 %t140, ptr %t91
  %t141 = load i64, ptr %t91
  %t142 = call %NxVal @nx_int(i64 %t141)
  call void @nx_memo_put(i64 2, ptr %args, i64 %nargs, %NxVal %t142)
  ret %NxVal %t142
}
define %NxVal @nx__f_2____main____f2(%NxVal* %args, i64 %nargs) {
entry:
  %t143 = alloca %NxVal
  %t147 = alloca i64
  %t151 = alloca i64
  %t162 = alloca i64
  store %NxVal zeroinitializer, ptr %t143
  %t144 = call i1 @nx_memo_get(i64 13, ptr %args, i64 %nargs, ptr %t143)
  br i1 %t144, label %mhit5, label %mmiss6
mhit5:
  %t145 = load %NxVal, ptr %t143
  %t146 = call %NxVal @nx_clone(%NxVal %t145)
  ret %NxVal %t146
mmiss6:
  %t148 = getelementptr %NxVal, ptr %args, i64 0
  %t149 = load %NxVal, ptr %t148
  %t150 = extractvalue %NxVal %t149, 1
  store i64 %t150, ptr %t147
  %t152 = getelementptr %NxVal, ptr %args, i64 1
  %t153 = load %NxVal, ptr %t152
  %t154 = extractvalue %NxVal %t153, 1
  store i64 %t154, ptr %t151
  %t155 = load i64, ptr %t147
  %t156 = load i64, ptr %t151
  %t157 = add i64 %t155, %t156
  %t158 = add i64 2, 0
  %t159 = add i64 %t157, %t158
  %t160 = add i64 65535, 0
  %t161 = and i64 %t159, %t160
  store i64 %t161, ptr %t162
  %t163 = load i64, ptr %t162
  %t164 = add i64 22, 0
  %t165 = sub i64 %t163, %t164
  %t166 = add i64 85, 0
  %t167 = sub i64 %t165, %t166
  %t168 = add i64 65535, 0
  %t169 = and i64 %t167, %t168
  store i64 %t169, ptr %t162
  %t170 = load i64, ptr %t162
  %t171 = add i64 22, 0
  %t172 = sub i64 %t170, %t171
  %t173 = add i64 49, 0
  %t174 = sub i64 %t172, %t173
  %t175 = add i64 65535, 0
  %t176 = and i64 %t174, %t175
  store i64 %t176, ptr %t162
  %t177 = load i64, ptr %t162
  %t178 = add i64 59, 0
  %t179 = call i64 @nx_mod_i64(i64 %t177, i64 %t178)
  %t180 = add i64 26, 0
  %t181 = call i64 @nx_mod_i64(i64 %t179, i64 %t180)
  %t182 = add i64 65535, 0
  %t183 = and i64 %t181, %t182
  store i64 %t183, ptr %t162
  %t184 = load i64, ptr %t162
  %t185 = add i64 30, 0
  %t186 = call i64 @nx_mod_i64(i64 %t184, i64 %t185)
  %t187 = add i64 21, 0
  %t188 = call i64 @nx_mod_i64(i64 %t186, i64 %t187)
  %t189 = add i64 65535, 0
  %t190 = and i64 %t188, %t189
  store i64 %t190, ptr %t162
  %t191 = load i64, ptr %t162
  %t192 = add i64 8, 0
  %t193 = xor i64 %t191, %t192
  %t194 = add i64 37, 0
  %t195 = xor i64 %t193, %t194
  %t196 = add i64 65535, 0
  %t197 = and i64 %t195, %t196
  store i64 %t197, ptr %t162
  %t198 = load i64, ptr %t162
  %t199 = add i64 42, 0
  %t200 = call i64 @nx_mod_i64(i64 %t198, i64 %t199)
  %t201 = add i64 69, 0
  %t202 = call i64 @nx_mod_i64(i64 %t200, i64 %t201)
  %t203 = add i64 65535, 0
  %t204 = and i64 %t202, %t203
  store i64 %t204, ptr %t162
  %t205 = load i64, ptr %t162
  %t206 = add i64 32, 0
  %t207 = add i64 %t205, %t206
  %t208 = add i64 6, 0
  %t209 = add i64 %t207, %t208
  %t210 = add i64 65535, 0
  %t211 = and i64 %t209, %t210
  store i64 %t211, ptr %t162
  %t212 = load i64, ptr %t162
  %t213 = call %NxVal @nx_int(i64 %t212)
  call void @nx_memo_put(i64 13, ptr %args, i64 %nargs, %NxVal %t213)
  ret %NxVal %t213
}
define %NxVal @nx__f_2____main____f3(%NxVal* %args, i64 %nargs) {
entry:
  %t214 = alloca %NxVal
  %t218 = alloca i64
  %t222 = alloca i64
  %t233 = alloca i64
  store %NxVal zeroinitializer, ptr %t214
  %t215 = call i1 @nx_memo_get(i64 24, ptr %args, i64 %nargs, ptr %t214)
  br i1 %t215, label %mhit7, label %mmiss8
mhit7:
  %t216 = load %NxVal, ptr %t214
  %t217 = call %NxVal @nx_clone(%NxVal %t216)
  ret %NxVal %t217
mmiss8:
  %t219 = getelementptr %NxVal, ptr %args, i64 0
  %t220 = load %NxVal, ptr %t219
  %t221 = extractvalue %NxVal %t220, 1
  store i64 %t221, ptr %t218
  %t223 = getelementptr %NxVal, ptr %args, i64 1
  %t224 = load %NxVal, ptr %t223
  %t225 = extractvalue %NxVal %t224, 1
  store i64 %t225, ptr %t222
  %t226 = load i64, ptr %t218
  %t227 = load i64, ptr %t222
  %t228 = add i64 %t226, %t227
  %t229 = add i64 3, 0
  %t230 = add i64 %t228, %t229
  %t231 = add i64 65535, 0
  %t232 = and i64 %t230, %t231
  store i64 %t232, ptr %t233
  %t234 = load i64, ptr %t233
  %t235 = add i64 19, 0
  %t236 = xor i64 %t234, %t235
  %t237 = add i64 4, 0
  %t238 = xor i64 %t236, %t237
  %t239 = add i64 65535, 0
  %t240 = and i64 %t238, %t239
  store i64 %t240, ptr %t233
  %t241 = load i64, ptr %t233
  %t242 = add i64 31, 0
  %t243 = sub i64 %t241, %t242
  %t244 = add i64 41, 0
  %t245 = sub i64 %t243, %t244
  %t246 = add i64 65535, 0
  %t247 = and i64 %t245, %t246
  store i64 %t247, ptr %t233
  %t248 = load i64, ptr %t233
  %t249 = add i64 37, 0
  %t250 = sub i64 %t248, %t249
  %t251 = add i64 81, 0
  %t252 = sub i64 %t250, %t251
  %t253 = add i64 65535, 0
  %t254 = and i64 %t252, %t253
  store i64 %t254, ptr %t233
  %t255 = load i64, ptr %t233
  %t256 = add i64 91, 0
  %t257 = or i64 %t255, %t256
  %t258 = add i64 15, 0
  %t259 = or i64 %t257, %t258
  %t260 = add i64 65535, 0
  %t261 = and i64 %t259, %t260
  store i64 %t261, ptr %t233
  %t262 = load i64, ptr %t233
  %t263 = add i64 37, 0
  %t264 = sub i64 %t262, %t263
  %t265 = add i64 9, 0
  %t266 = sub i64 %t264, %t265
  %t267 = add i64 65535, 0
  %t268 = and i64 %t266, %t267
  store i64 %t268, ptr %t233
  %t269 = load i64, ptr %t233
  %t270 = add i64 58, 0
  %t271 = and i64 %t269, %t270
  %t272 = add i64 58, 0
  %t273 = and i64 %t271, %t272
  %t274 = add i64 65535, 0
  %t275 = and i64 %t273, %t274
  store i64 %t275, ptr %t233
  %t276 = load i64, ptr %t233
  %t277 = add i64 6, 0
  %t278 = or i64 %t276, %t277
  %t279 = add i64 87, 0
  %t280 = or i64 %t278, %t279
  %t281 = add i64 65535, 0
  %t282 = and i64 %t280, %t281
  store i64 %t282, ptr %t233
  %t283 = load i64, ptr %t233
  %t284 = call %NxVal @nx_int(i64 %t283)
  call void @nx_memo_put(i64 24, ptr %args, i64 %nargs, %NxVal %t284)
  ret %NxVal %t284
}
define %NxVal @nx__f_2____main____f4(%NxVal* %args, i64 %nargs) {
entry:
  %t285 = alloca %NxVal
  %t289 = alloca i64
  %t293 = alloca i64
  %t304 = alloca i64
  store %NxVal zeroinitializer, ptr %t285
  %t286 = call i1 @nx_memo_get(i64 35, ptr %args, i64 %nargs, ptr %t285)
  br i1 %t286, label %mhit9, label %mmiss10
mhit9:
  %t287 = load %NxVal, ptr %t285
  %t288 = call %NxVal @nx_clone(%NxVal %t287)
  ret %NxVal %t288
mmiss10:
  %t290 = getelementptr %NxVal, ptr %args, i64 0
  %t291 = load %NxVal, ptr %t290
  %t292 = extractvalue %NxVal %t291, 1
  store i64 %t292, ptr %t289
  %t294 = getelementptr %NxVal, ptr %args, i64 1
  %t295 = load %NxVal, ptr %t294
  %t296 = extractvalue %NxVal %t295, 1
  store i64 %t296, ptr %t293
  %t297 = load i64, ptr %t289
  %t298 = load i64, ptr %t293
  %t299 = add i64 %t297, %t298
  %t300 = add i64 4, 0
  %t301 = add i64 %t299, %t300
  %t302 = add i64 65535, 0
  %t303 = and i64 %t301, %t302
  store i64 %t303, ptr %t304
  %t305 = load i64, ptr %t304
  %t306 = add i64 6, 0
  %t307 = call i64 @nx_mod_i64(i64 %t305, i64 %t306)
  %t308 = add i64 63, 0
  %t309 = call i64 @nx_mod_i64(i64 %t307, i64 %t308)
  %t310 = add i64 65535, 0
  %t311 = and i64 %t309, %t310
  store i64 %t311, ptr %t304
  %t312 = load i64, ptr %t304
  %t313 = add i64 73, 0
  %t314 = and i64 %t312, %t313
  %t315 = add i64 89, 0
  %t316 = and i64 %t314, %t315
  %t317 = add i64 65535, 0
  %t318 = and i64 %t316, %t317
  store i64 %t318, ptr %t304
  %t319 = load i64, ptr %t304
  %t320 = add i64 56, 0
  %t321 = xor i64 %t319, %t320
  %t322 = add i64 59, 0
  %t323 = xor i64 %t321, %t322
  %t324 = add i64 65535, 0
  %t325 = and i64 %t323, %t324
  store i64 %t325, ptr %t304
  %t326 = load i64, ptr %t304
  %t327 = add i64 45, 0
  %t328 = sub i64 %t326, %t327
  %t329 = add i64 4, 0
  %t330 = sub i64 %t328, %t329
  %t331 = add i64 65535, 0
  %t332 = and i64 %t330, %t331
  store i64 %t332, ptr %t304
  %t333 = load i64, ptr %t304
  %t334 = add i64 8, 0
  %t335 = sub i64 %t333, %t334
  %t336 = add i64 83, 0
  %t337 = sub i64 %t335, %t336
  %t338 = add i64 65535, 0
  %t339 = and i64 %t337, %t338
  store i64 %t339, ptr %t304
  %t340 = load i64, ptr %t304
  %t341 = add i64 61, 0
  %t342 = call i64 @nx_mod_i64(i64 %t340, i64 %t341)
  %t343 = add i64 2, 0
  %t344 = call i64 @nx_mod_i64(i64 %t342, i64 %t343)
  %t345 = add i64 65535, 0
  %t346 = and i64 %t344, %t345
  store i64 %t346, ptr %t304
  %t347 = load i64, ptr %t304
  %t348 = add i64 34, 0
  %t349 = mul i64 %t347, %t348
  %t350 = add i64 60, 0
  %t351 = mul i64 %t349, %t350
  %t352 = add i64 65535, 0
  %t353 = and i64 %t351, %t352
  store i64 %t353, ptr %t304
  %t354 = load i64, ptr %t304
  %t355 = call %NxVal @nx_int(i64 %t354)
  call void @nx_memo_put(i64 35, ptr %args, i64 %nargs, %NxVal %t355)
  ret %NxVal %t355
}
define %NxVal @nx__f_2____main____f5(%NxVal* %args, i64 %nargs) {
entry:
  %t356 = alloca %NxVal
  %t360 = alloca i64
  %t364 = alloca i64
  %t375 = alloca i64
  store %NxVal zeroinitializer, ptr %t356
  %t357 = call i1 @nx_memo_get(i64 46, ptr %args, i64 %nargs, ptr %t356)
  br i1 %t357, label %mhit11, label %mmiss12
mhit11:
  %t358 = load %NxVal, ptr %t356
  %t359 = call %NxVal @nx_clone(%NxVal %t358)
  ret %NxVal %t359
mmiss12:
  %t361 = getelementptr %NxVal, ptr %args, i64 0
  %t362 = load %NxVal, ptr %t361
  %t363 = extractvalue %NxVal %t362, 1
  store i64 %t363, ptr %t360
  %t365 = getelementptr %NxVal, ptr %args, i64 1
  %t366 = load %NxVal, ptr %t365
  %t367 = extractvalue %NxVal %t366, 1
  store i64 %t367, ptr %t364
  %t368 = load i64, ptr %t360
  %t369 = load i64, ptr %t364
  %t370 = add i64 %t368, %t369
  %t371 = add i64 5, 0
  %t372 = add i64 %t370, %t371
  %t373 = add i64 65535, 0
  %t374 = and i64 %t372, %t373
  store i64 %t374, ptr %t375
  %t376 = load i64, ptr %t375
  %t377 = add i64 33, 0
  %t378 = and i64 %t376, %t377
  %t379 = add i64 60, 0
  %t380 = and i64 %t378, %t379
  %t381 = add i64 65535, 0
  %t382 = and i64 %t380, %t381
  store i64 %t382, ptr %t375
  %t383 = load i64, ptr %t375
  %t384 = add i64 20, 0
  %t385 = or i64 %t383, %t384
  %t386 = add i64 65, 0
  %t387 = or i64 %t385, %t386
  %t388 = add i64 65535, 0
  %t389 = and i64 %t387, %t388
  store i64 %t389, ptr %t375
  %t390 = load i64, ptr %t375
  %t391 = add i64 36, 0
  %t392 = and i64 %t390, %t391
  %t393 = add i64 18, 0
  %t394 = and i64 %t392, %t393
  %t395 = add i64 65535, 0
  %t396 = and i64 %t394, %t395
  store i64 %t396, ptr %t375
  %t397 = load i64, ptr %t375
  %t398 = add i64 23, 0
  %t399 = call i64 @nx_mod_i64(i64 %t397, i64 %t398)
  %t400 = add i64 15, 0
  %t401 = call i64 @nx_mod_i64(i64 %t399, i64 %t400)
  %t402 = add i64 65535, 0
  %t403 = and i64 %t401, %t402
  store i64 %t403, ptr %t375
  %t404 = load i64, ptr %t375
  %t405 = add i64 73, 0
  %t406 = call i64 @nx_mod_i64(i64 %t404, i64 %t405)
  %t407 = add i64 69, 0
  %t408 = call i64 @nx_mod_i64(i64 %t406, i64 %t407)
  %t409 = add i64 65535, 0
  %t410 = and i64 %t408, %t409
  store i64 %t410, ptr %t375
  %t411 = load i64, ptr %t375
  %t412 = add i64 45, 0
  %t413 = xor i64 %t411, %t412
  %t414 = add i64 8, 0
  %t415 = xor i64 %t413, %t414
  %t416 = add i64 65535, 0
  %t417 = and i64 %t415, %t416
  store i64 %t417, ptr %t375
  %t418 = load i64, ptr %t375
  %t419 = add i64 94, 0
  %t420 = xor i64 %t418, %t419
  %t421 = add i64 70, 0
  %t422 = xor i64 %t420, %t421
  %t423 = add i64 65535, 0
  %t424 = and i64 %t422, %t423
  store i64 %t424, ptr %t375
  %t425 = load i64, ptr %t375
  %t426 = call %NxVal @nx_int(i64 %t425)
  call void @nx_memo_put(i64 46, ptr %args, i64 %nargs, %NxVal %t426)
  ret %NxVal %t426
}
define %NxVal @nx__f_2____main____f6(%NxVal* %args, i64 %nargs) {
entry:
  %t427 = alloca %NxVal
  %t431 = alloca i64
  %t435 = alloca i64
  %t446 = alloca i64
  store %NxVal zeroinitializer, ptr %t427
  %t428 = call i1 @nx_memo_get(i64 47, ptr %args, i64 %nargs, ptr %t427)
  br i1 %t428, label %mhit13, label %mmiss14
mhit13:
  %t429 = load %NxVal, ptr %t427
  %t430 = call %NxVal @nx_clone(%NxVal %t429)
  ret %NxVal %t430
mmiss14:
  %t432 = getelementptr %NxVal, ptr %args, i64 0
  %t433 = load %NxVal, ptr %t432
  %t434 = extractvalue %NxVal %t433, 1
  store i64 %t434, ptr %t431
  %t436 = getelementptr %NxVal, ptr %args, i64 1
  %t437 = load %NxVal, ptr %t436
  %t438 = extractvalue %NxVal %t437, 1
  store i64 %t438, ptr %t435
  %t439 = load i64, ptr %t431
  %t440 = load i64, ptr %t435
  %t441 = add i64 %t439, %t440
  %t442 = add i64 6, 0
  %t443 = add i64 %t441, %t442
  %t444 = add i64 65535, 0
  %t445 = and i64 %t443, %t444
  store i64 %t445, ptr %t446
  %t447 = load i64, ptr %t446
  %t448 = add i64 66, 0
  %t449 = or i64 %t447, %t448
  %t450 = add i64 52, 0
  %t451 = or i64 %t449, %t450
  %t452 = add i64 65535, 0
  %t453 = and i64 %t451, %t452
  store i64 %t453, ptr %t446
  %t454 = load i64, ptr %t446
  %t455 = add i64 44, 0
  %t456 = xor i64 %t454, %t455
  %t457 = add i64 35, 0
  %t458 = xor i64 %t456, %t457
  %t459 = add i64 65535, 0
  %t460 = and i64 %t458, %t459
  store i64 %t460, ptr %t446
  %t461 = load i64, ptr %t446
  %t462 = add i64 83, 0
  %t463 = xor i64 %t461, %t462
  %t464 = add i64 87, 0
  %t465 = xor i64 %t463, %t464
  %t466 = add i64 65535, 0
  %t467 = and i64 %t465, %t466
  store i64 %t467, ptr %t446
  %t468 = load i64, ptr %t446
  %t469 = add i64 93, 0
  %t470 = and i64 %t468, %t469
  %t471 = add i64 37, 0
  %t472 = and i64 %t470, %t471
  %t473 = add i64 65535, 0
  %t474 = and i64 %t472, %t473
  store i64 %t474, ptr %t446
  %t475 = load i64, ptr %t446
  %t476 = add i64 21, 0
  %t477 = and i64 %t475, %t476
  %t478 = add i64 3, 0
  %t479 = and i64 %t477, %t478
  %t480 = add i64 65535, 0
  %t481 = and i64 %t479, %t480
  store i64 %t481, ptr %t446
  %t482 = load i64, ptr %t446
  %t483 = add i64 79, 0
  %t484 = sub i64 %t482, %t483
  %t485 = add i64 64, 0
  %t486 = sub i64 %t484, %t485
  %t487 = add i64 65535, 0
  %t488 = and i64 %t486, %t487
  store i64 %t488, ptr %t446
  %t489 = load i64, ptr %t446
  %t490 = add i64 38, 0
  %t491 = xor i64 %t489, %t490
  %t492 = add i64 24, 0
  %t493 = xor i64 %t491, %t492
  %t494 = add i64 65535, 0
  %t495 = and i64 %t493, %t494
  store i64 %t495, ptr %t446
  %t496 = load i64, ptr %t446
  %t497 = call %NxVal @nx_int(i64 %t496)
  call void @nx_memo_put(i64 47, ptr %args, i64 %nargs, %NxVal %t497)
  ret %NxVal %t497
}
define %NxVal @nx__f_2____main____f7(%NxVal* %args, i64 %nargs) {
entry:
  %t498 = alloca %NxVal
  %t502 = alloca i64
  %t506 = alloca i64
  %t517 = alloca i64
  store %NxVal zeroinitializer, ptr %t498
  %t499 = call i1 @nx_memo_get(i64 48, ptr %args, i64 %nargs, ptr %t498)
  br i1 %t499, label %mhit15, label %mmiss16
mhit15:
  %t500 = load %NxVal, ptr %t498
  %t501 = call %NxVal @nx_clone(%NxVal %t500)
  ret %NxVal %t501
mmiss16:
  %t503 = getelementptr %NxVal, ptr %args, i64 0
  %t504 = load %NxVal, ptr %t503
  %t505 = extractvalue %NxVal %t504, 1
  store i64 %t505, ptr %t502
  %t507 = getelementptr %NxVal, ptr %args, i64 1
  %t508 = load %NxVal, ptr %t507
  %t509 = extractvalue %NxVal %t508, 1
  store i64 %t509, ptr %t506
  %t510 = load i64, ptr %t502
  %t511 = load i64, ptr %t506
  %t512 = add i64 %t510, %t511
  %t513 = add i64 7, 0
  %t514 = add i64 %t512, %t513
  %t515 = add i64 65535, 0
  %t516 = and i64 %t514, %t515
  store i64 %t516, ptr %t517
  %t518 = load i64, ptr %t517
  %t519 = add i64 13, 0
  %t520 = xor i64 %t518, %t519
  %t521 = add i64 32, 0
  %t522 = xor i64 %t520, %t521
  %t523 = add i64 65535, 0
  %t524 = and i64 %t522, %t523
  store i64 %t524, ptr %t517
  %t525 = load i64, ptr %t517
  %t526 = add i64 76, 0
  %t527 = call i64 @nx_mod_i64(i64 %t525, i64 %t526)
  %t528 = add i64 4, 0
  %t529 = call i64 @nx_mod_i64(i64 %t527, i64 %t528)
  %t530 = add i64 65535, 0
  %t531 = and i64 %t529, %t530
  store i64 %t531, ptr %t517
  %t532 = load i64, ptr %t517
  %t533 = add i64 71, 0
  %t534 = call i64 @nx_mod_i64(i64 %t532, i64 %t533)
  %t535 = add i64 74, 0
  %t536 = call i64 @nx_mod_i64(i64 %t534, i64 %t535)
  %t537 = add i64 65535, 0
  %t538 = and i64 %t536, %t537
  store i64 %t538, ptr %t517
  %t539 = load i64, ptr %t517
  %t540 = add i64 8, 0
  %t541 = xor i64 %t539, %t540
  %t542 = add i64 84, 0
  %t543 = xor i64 %t541, %t542
  %t544 = add i64 65535, 0
  %t545 = and i64 %t543, %t544
  store i64 %t545, ptr %t517
  %t546 = load i64, ptr %t517
  %t547 = add i64 53, 0
  %t548 = call i64 @nx_mod_i64(i64 %t546, i64 %t547)
  %t549 = add i64 42, 0
  %t550 = call i64 @nx_mod_i64(i64 %t548, i64 %t549)
  %t551 = add i64 65535, 0
  %t552 = and i64 %t550, %t551
  store i64 %t552, ptr %t517
  %t553 = load i64, ptr %t517
  %t554 = add i64 2, 0
  %t555 = call i64 @nx_mod_i64(i64 %t553, i64 %t554)
  %t556 = add i64 53, 0
  %t557 = call i64 @nx_mod_i64(i64 %t555, i64 %t556)
  %t558 = add i64 65535, 0
  %t559 = and i64 %t557, %t558
  store i64 %t559, ptr %t517
  %t560 = load i64, ptr %t517
  %t561 = add i64 80, 0
  %t562 = add i64 %t560, %t561
  %t563 = add i64 75, 0
  %t564 = add i64 %t562, %t563
  %t565 = add i64 65535, 0
  %t566 = and i64 %t564, %t565
  store i64 %t566, ptr %t517
  %t567 = load i64, ptr %t517
  %t568 = call %NxVal @nx_int(i64 %t567)
  call void @nx_memo_put(i64 48, ptr %args, i64 %nargs, %NxVal %t568)
  ret %NxVal %t568
}
define %NxVal @nx__f_2____main____f8(%NxVal* %args, i64 %nargs) {
entry:
  %t569 = alloca %NxVal
  %t573 = alloca i64
  %t577 = alloca i64
  %t588 = alloca i64
  store %NxVal zeroinitializer, ptr %t569
  %t570 = call i1 @nx_memo_get(i64 49, ptr %args, i64 %nargs, ptr %t569)
  br i1 %t570, label %mhit17, label %mmiss18
mhit17:
  %t571 = load %NxVal, ptr %t569
  %t572 = call %NxVal @nx_clone(%NxVal %t571)
  ret %NxVal %t572
mmiss18:
  %t574 = getelementptr %NxVal, ptr %args, i64 0
  %t575 = load %NxVal, ptr %t574
  %t576 = extractvalue %NxVal %t575, 1
  store i64 %t576, ptr %t573
  %t578 = getelementptr %NxVal, ptr %args, i64 1
  %t579 = load %NxVal, ptr %t578
  %t580 = extractvalue %NxVal %t579, 1
  store i64 %t580, ptr %t577
  %t581 = load i64, ptr %t573
  %t582 = load i64, ptr %t577
  %t583 = add i64 %t581, %t582
  %t584 = add i64 8, 0
  %t585 = add i64 %t583, %t584
  %t586 = add i64 65535, 0
  %t587 = and i64 %t585, %t586
  store i64 %t587, ptr %t588
  %t589 = load i64, ptr %t588
  %t590 = add i64 10, 0
  %t591 = xor i64 %t589, %t590
  %t592 = add i64 28, 0
  %t593 = xor i64 %t591, %t592
  %t594 = add i64 65535, 0
  %t595 = and i64 %t593, %t594
  store i64 %t595, ptr %t588
  %t596 = load i64, ptr %t588
  %t597 = add i64 53, 0
  %t598 = add i64 %t596, %t597
  %t599 = add i64 10, 0
  %t600 = add i64 %t598, %t599
  %t601 = add i64 65535, 0
  %t602 = and i64 %t600, %t601
  store i64 %t602, ptr %t588
  %t603 = load i64, ptr %t588
  %t604 = add i64 59, 0
  %t605 = call i64 @nx_mod_i64(i64 %t603, i64 %t604)
  %t606 = add i64 3, 0
  %t607 = call i64 @nx_mod_i64(i64 %t605, i64 %t606)
  %t608 = add i64 65535, 0
  %t609 = and i64 %t607, %t608
  store i64 %t609, ptr %t588
  %t610 = load i64, ptr %t588
  %t611 = add i64 1, 0
  %t612 = add i64 %t610, %t611
  %t613 = add i64 19, 0
  %t614 = add i64 %t612, %t613
  %t615 = add i64 65535, 0
  %t616 = and i64 %t614, %t615
  store i64 %t616, ptr %t588
  %t617 = load i64, ptr %t588
  %t618 = add i64 47, 0
  %t619 = call i64 @nx_mod_i64(i64 %t617, i64 %t618)
  %t620 = add i64 87, 0
  %t621 = call i64 @nx_mod_i64(i64 %t619, i64 %t620)
  %t622 = add i64 65535, 0
  %t623 = and i64 %t621, %t622
  store i64 %t623, ptr %t588
  %t624 = load i64, ptr %t588
  %t625 = add i64 80, 0
  %t626 = call i64 @nx_mod_i64(i64 %t624, i64 %t625)
  %t627 = add i64 28, 0
  %t628 = call i64 @nx_mod_i64(i64 %t626, i64 %t627)
  %t629 = add i64 65535, 0
  %t630 = and i64 %t628, %t629
  store i64 %t630, ptr %t588
  %t631 = load i64, ptr %t588
  %t632 = add i64 83, 0
  %t633 = call i64 @nx_mod_i64(i64 %t631, i64 %t632)
  %t634 = add i64 23, 0
  %t635 = call i64 @nx_mod_i64(i64 %t633, i64 %t634)
  %t636 = add i64 65535, 0
  %t637 = and i64 %t635, %t636
  store i64 %t637, ptr %t588
  %t638 = load i64, ptr %t588
  %t639 = call %NxVal @nx_int(i64 %t638)
  call void @nx_memo_put(i64 49, ptr %args, i64 %nargs, %NxVal %t639)
  ret %NxVal %t639
}
define %NxVal @nx__f_2____main____f9(%NxVal* %args, i64 %nargs) {
entry:
  %t640 = alloca %NxVal
  %t644 = alloca i64
  %t648 = alloca i64
  %t659 = alloca i64
  store %NxVal zeroinitializer, ptr %t640
  %t641 = call i1 @nx_memo_get(i64 50, ptr %args, i64 %nargs, ptr %t640)
  br i1 %t641, label %mhit19, label %mmiss20
mhit19:
  %t642 = load %NxVal, ptr %t640
  %t643 = call %NxVal @nx_clone(%NxVal %t642)
  ret %NxVal %t643
mmiss20:
  %t645 = getelementptr %NxVal, ptr %args, i64 0
  %t646 = load %NxVal, ptr %t645
  %t647 = extractvalue %NxVal %t646, 1
  store i64 %t647, ptr %t644
  %t649 = getelementptr %NxVal, ptr %args, i64 1
  %t650 = load %NxVal, ptr %t649
  %t651 = extractvalue %NxVal %t650, 1
  store i64 %t651, ptr %t648
  %t652 = load i64, ptr %t644
  %t653 = load i64, ptr %t648
  %t654 = add i64 %t652, %t653
  %t655 = add i64 9, 0
  %t656 = add i64 %t654, %t655
  %t657 = add i64 65535, 0
  %t658 = and i64 %t656, %t657
  store i64 %t658, ptr %t659
  %t660 = load i64, ptr %t659
  %t661 = add i64 88, 0
  %t662 = or i64 %t660, %t661
  %t663 = add i64 52, 0
  %t664 = or i64 %t662, %t663
  %t665 = add i64 65535, 0
  %t666 = and i64 %t664, %t665
  store i64 %t666, ptr %t659
  %t667 = load i64, ptr %t659
  %t668 = add i64 63, 0
  %t669 = mul i64 %t667, %t668
  %t670 = add i64 37, 0
  %t671 = mul i64 %t669, %t670
  %t672 = add i64 65535, 0
  %t673 = and i64 %t671, %t672
  store i64 %t673, ptr %t659
  %t674 = load i64, ptr %t659
  %t675 = add i64 3, 0
  %t676 = mul i64 %t674, %t675
  %t677 = add i64 31, 0
  %t678 = mul i64 %t676, %t677
  %t679 = add i64 65535, 0
  %t680 = and i64 %t678, %t679
  store i64 %t680, ptr %t659
  %t681 = load i64, ptr %t659
  %t682 = add i64 13, 0
  %t683 = xor i64 %t681, %t682
  %t684 = add i64 11, 0
  %t685 = xor i64 %t683, %t684
  %t686 = add i64 65535, 0
  %t687 = and i64 %t685, %t686
  store i64 %t687, ptr %t659
  %t688 = load i64, ptr %t659
  %t689 = add i64 39, 0
  %t690 = sub i64 %t688, %t689
  %t691 = add i64 69, 0
  %t692 = sub i64 %t690, %t691
  %t693 = add i64 65535, 0
  %t694 = and i64 %t692, %t693
  store i64 %t694, ptr %t659
  %t695 = load i64, ptr %t659
  %t696 = add i64 30, 0
  %t697 = add i64 %t695, %t696
  %t698 = add i64 42, 0
  %t699 = add i64 %t697, %t698
  %t700 = add i64 65535, 0
  %t701 = and i64 %t699, %t700
  store i64 %t701, ptr %t659
  %t702 = load i64, ptr %t659
  %t703 = add i64 12, 0
  %t704 = call i64 @nx_mod_i64(i64 %t702, i64 %t703)
  %t705 = add i64 88, 0
  %t706 = call i64 @nx_mod_i64(i64 %t704, i64 %t705)
  %t707 = add i64 65535, 0
  %t708 = and i64 %t706, %t707
  store i64 %t708, ptr %t659
  %t709 = load i64, ptr %t659
  %t710 = call %NxVal @nx_int(i64 %t709)
  call void @nx_memo_put(i64 50, ptr %args, i64 %nargs, %NxVal %t710)
  ret %NxVal %t710
}
define %NxVal @nx__f_3____main____f10(%NxVal* %args, i64 %nargs) {
entry:
  %t711 = alloca %NxVal
  %t715 = alloca i64
  %t719 = alloca i64
  %t730 = alloca i64
  store %NxVal zeroinitializer, ptr %t711
  %t712 = call i1 @nx_memo_get(i64 3, ptr %args, i64 %nargs, ptr %t711)
  br i1 %t712, label %mhit21, label %mmiss22
mhit21:
  %t713 = load %NxVal, ptr %t711
  %t714 = call %NxVal @nx_clone(%NxVal %t713)
  ret %NxVal %t714
mmiss22:
  %t716 = getelementptr %NxVal, ptr %args, i64 0
  %t717 = load %NxVal, ptr %t716
  %t718 = extractvalue %NxVal %t717, 1
  store i64 %t718, ptr %t715
  %t720 = getelementptr %NxVal, ptr %args, i64 1
  %t721 = load %NxVal, ptr %t720
  %t722 = extractvalue %NxVal %t721, 1
  store i64 %t722, ptr %t719
  %t723 = load i64, ptr %t715
  %t724 = load i64, ptr %t719
  %t725 = add i64 %t723, %t724
  %t726 = add i64 10, 0
  %t727 = add i64 %t725, %t726
  %t728 = add i64 65535, 0
  %t729 = and i64 %t727, %t728
  store i64 %t729, ptr %t730
  %t731 = load i64, ptr %t730
  %t732 = add i64 37, 0
  %t733 = add i64 %t731, %t732
  %t734 = add i64 71, 0
  %t735 = add i64 %t733, %t734
  %t736 = add i64 65535, 0
  %t737 = and i64 %t735, %t736
  store i64 %t737, ptr %t730
  %t738 = load i64, ptr %t730
  %t739 = add i64 44, 0
  %t740 = mul i64 %t738, %t739
  %t741 = add i64 78, 0
  %t742 = mul i64 %t740, %t741
  %t743 = add i64 65535, 0
  %t744 = and i64 %t742, %t743
  store i64 %t744, ptr %t730
  %t745 = load i64, ptr %t730
  %t746 = add i64 40, 0
  %t747 = call i64 @nx_mod_i64(i64 %t745, i64 %t746)
  %t748 = add i64 71, 0
  %t749 = call i64 @nx_mod_i64(i64 %t747, i64 %t748)
  %t750 = add i64 65535, 0
  %t751 = and i64 %t749, %t750
  store i64 %t751, ptr %t730
  %t752 = load i64, ptr %t730
  %t753 = add i64 46, 0
  %t754 = mul i64 %t752, %t753
  %t755 = add i64 7, 0
  %t756 = mul i64 %t754, %t755
  %t757 = add i64 65535, 0
  %t758 = and i64 %t756, %t757
  store i64 %t758, ptr %t730
  %t759 = load i64, ptr %t730
  %t760 = add i64 67, 0
  %t761 = add i64 %t759, %t760
  %t762 = add i64 51, 0
  %t763 = add i64 %t761, %t762
  %t764 = add i64 65535, 0
  %t765 = and i64 %t763, %t764
  store i64 %t765, ptr %t730
  %t766 = load i64, ptr %t730
  %t767 = add i64 3, 0
  %t768 = sub i64 %t766, %t767
  %t769 = add i64 55, 0
  %t770 = sub i64 %t768, %t769
  %t771 = add i64 65535, 0
  %t772 = and i64 %t770, %t771
  store i64 %t772, ptr %t730
  %t773 = load i64, ptr %t730
  %t774 = add i64 82, 0
  %t775 = and i64 %t773, %t774
  %t776 = add i64 62, 0
  %t777 = and i64 %t775, %t776
  %t778 = add i64 65535, 0
  %t779 = and i64 %t777, %t778
  store i64 %t779, ptr %t730
  %t780 = load i64, ptr %t730
  %t781 = call %NxVal @nx_int(i64 %t780)
  call void @nx_memo_put(i64 3, ptr %args, i64 %nargs, %NxVal %t781)
  ret %NxVal %t781
}
define %NxVal @nx__f_3____main____f11(%NxVal* %args, i64 %nargs) {
entry:
  %t782 = alloca %NxVal
  %t786 = alloca i64
  %t790 = alloca i64
  %t801 = alloca i64
  store %NxVal zeroinitializer, ptr %t782
  %t783 = call i1 @nx_memo_get(i64 4, ptr %args, i64 %nargs, ptr %t782)
  br i1 %t783, label %mhit23, label %mmiss24
mhit23:
  %t784 = load %NxVal, ptr %t782
  %t785 = call %NxVal @nx_clone(%NxVal %t784)
  ret %NxVal %t785
mmiss24:
  %t787 = getelementptr %NxVal, ptr %args, i64 0
  %t788 = load %NxVal, ptr %t787
  %t789 = extractvalue %NxVal %t788, 1
  store i64 %t789, ptr %t786
  %t791 = getelementptr %NxVal, ptr %args, i64 1
  %t792 = load %NxVal, ptr %t791
  %t793 = extractvalue %NxVal %t792, 1
  store i64 %t793, ptr %t790
  %t794 = load i64, ptr %t786
  %t795 = load i64, ptr %t790
  %t796 = add i64 %t794, %t795
  %t797 = add i64 11, 0
  %t798 = add i64 %t796, %t797
  %t799 = add i64 65535, 0
  %t800 = and i64 %t798, %t799
  store i64 %t800, ptr %t801
  %t802 = load i64, ptr %t801
  %t803 = add i64 78, 0
  %t804 = mul i64 %t802, %t803
  %t805 = add i64 89, 0
  %t806 = mul i64 %t804, %t805
  %t807 = add i64 65535, 0
  %t808 = and i64 %t806, %t807
  store i64 %t808, ptr %t801
  %t809 = load i64, ptr %t801
  %t810 = add i64 22, 0
  %t811 = add i64 %t809, %t810
  %t812 = add i64 78, 0
  %t813 = add i64 %t811, %t812
  %t814 = add i64 65535, 0
  %t815 = and i64 %t813, %t814
  store i64 %t815, ptr %t801
  %t816 = load i64, ptr %t801
  %t817 = add i64 70, 0
  %t818 = xor i64 %t816, %t817
  %t819 = add i64 58, 0
  %t820 = xor i64 %t818, %t819
  %t821 = add i64 65535, 0
  %t822 = and i64 %t820, %t821
  store i64 %t822, ptr %t801
  %t823 = load i64, ptr %t801
  %t824 = add i64 76, 0
  %t825 = sub i64 %t823, %t824
  %t826 = add i64 31, 0
  %t827 = sub i64 %t825, %t826
  %t828 = add i64 65535, 0
  %t829 = and i64 %t827, %t828
  store i64 %t829, ptr %t801
  %t830 = load i64, ptr %t801
  %t831 = add i64 50, 0
  %t832 = xor i64 %t830, %t831
  %t833 = add i64 82, 0
  %t834 = xor i64 %t832, %t833
  %t835 = add i64 65535, 0
  %t836 = and i64 %t834, %t835
  store i64 %t836, ptr %t801
  %t837 = load i64, ptr %t801
  %t838 = add i64 54, 0
  %t839 = xor i64 %t837, %t838
  %t840 = add i64 16, 0
  %t841 = xor i64 %t839, %t840
  %t842 = add i64 65535, 0
  %t843 = and i64 %t841, %t842
  store i64 %t843, ptr %t801
  %t844 = load i64, ptr %t801
  %t845 = add i64 21, 0
  %t846 = call i64 @nx_mod_i64(i64 %t844, i64 %t845)
  %t847 = add i64 71, 0
  %t848 = call i64 @nx_mod_i64(i64 %t846, i64 %t847)
  %t849 = add i64 65535, 0
  %t850 = and i64 %t848, %t849
  store i64 %t850, ptr %t801
  %t851 = load i64, ptr %t801
  %t852 = call %NxVal @nx_int(i64 %t851)
  call void @nx_memo_put(i64 4, ptr %args, i64 %nargs, %NxVal %t852)
  ret %NxVal %t852
}
define %NxVal @nx__f_3____main____f12(%NxVal* %args, i64 %nargs) {
entry:
  %t853 = alloca %NxVal
  %t857 = alloca i64
  %t861 = alloca i64
  %t872 = alloca i64
  store %NxVal zeroinitializer, ptr %t853
  %t854 = call i1 @nx_memo_get(i64 5, ptr %args, i64 %nargs, ptr %t853)
  br i1 %t854, label %mhit25, label %mmiss26
mhit25:
  %t855 = load %NxVal, ptr %t853
  %t856 = call %NxVal @nx_clone(%NxVal %t855)
  ret %NxVal %t856
mmiss26:
  %t858 = getelementptr %NxVal, ptr %args, i64 0
  %t859 = load %NxVal, ptr %t858
  %t860 = extractvalue %NxVal %t859, 1
  store i64 %t860, ptr %t857
  %t862 = getelementptr %NxVal, ptr %args, i64 1
  %t863 = load %NxVal, ptr %t862
  %t864 = extractvalue %NxVal %t863, 1
  store i64 %t864, ptr %t861
  %t865 = load i64, ptr %t857
  %t866 = load i64, ptr %t861
  %t867 = add i64 %t865, %t866
  %t868 = add i64 12, 0
  %t869 = add i64 %t867, %t868
  %t870 = add i64 65535, 0
  %t871 = and i64 %t869, %t870
  store i64 %t871, ptr %t872
  %t873 = load i64, ptr %t872
  %t874 = add i64 89, 0
  %t875 = and i64 %t873, %t874
  %t876 = add i64 54, 0
  %t877 = and i64 %t875, %t876
  %t878 = add i64 65535, 0
  %t879 = and i64 %t877, %t878
  store i64 %t879, ptr %t872
  %t880 = load i64, ptr %t872
  %t881 = add i64 8, 0
  %t882 = call i64 @nx_mod_i64(i64 %t880, i64 %t881)
  %t883 = add i64 3, 0
  %t884 = call i64 @nx_mod_i64(i64 %t882, i64 %t883)
  %t885 = add i64 65535, 0
  %t886 = and i64 %t884, %t885
  store i64 %t886, ptr %t872
  %t887 = load i64, ptr %t872
  %t888 = add i64 9, 0
  %t889 = call i64 @nx_mod_i64(i64 %t887, i64 %t888)
  %t890 = add i64 56, 0
  %t891 = call i64 @nx_mod_i64(i64 %t889, i64 %t890)
  %t892 = add i64 65535, 0
  %t893 = and i64 %t891, %t892
  store i64 %t893, ptr %t872
  %t894 = load i64, ptr %t872
  %t895 = add i64 52, 0
  %t896 = and i64 %t894, %t895
  %t897 = add i64 40, 0
  %t898 = and i64 %t896, %t897
  %t899 = add i64 65535, 0
  %t900 = and i64 %t898, %t899
  store i64 %t900, ptr %t872
  %t901 = load i64, ptr %t872
  %t902 = add i64 11, 0
  %t903 = sub i64 %t901, %t902
  %t904 = add i64 66, 0
  %t905 = sub i64 %t903, %t904
  %t906 = add i64 65535, 0
  %t907 = and i64 %t905, %t906
  store i64 %t907, ptr %t872
  %t908 = load i64, ptr %t872
  %t909 = add i64 48, 0
  %t910 = xor i64 %t908, %t909
  %t911 = add i64 36, 0
  %t912 = xor i64 %t910, %t911
  %t913 = add i64 65535, 0
  %t914 = and i64 %t912, %t913
  store i64 %t914, ptr %t872
  %t915 = load i64, ptr %t872
  %t916 = add i64 18, 0
  %t917 = or i64 %t915, %t916
  %t918 = add i64 73, 0
  %t919 = or i64 %t917, %t918
  %t920 = add i64 65535, 0
  %t921 = and i64 %t919, %t920
  store i64 %t921, ptr %t872
  %t922 = load i64, ptr %t872
  %t923 = call %NxVal @nx_int(i64 %t922)
  call void @nx_memo_put(i64 5, ptr %args, i64 %nargs, %NxVal %t923)
  ret %NxVal %t923
}
define %NxVal @nx__f_3____main____f13(%NxVal* %args, i64 %nargs) {
entry:
  %t924 = alloca %NxVal
  %t928 = alloca i64
  %t932 = alloca i64
  %t943 = alloca i64
  store %NxVal zeroinitializer, ptr %t924
  %t925 = call i1 @nx_memo_get(i64 6, ptr %args, i64 %nargs, ptr %t924)
  br i1 %t925, label %mhit27, label %mmiss28
mhit27:
  %t926 = load %NxVal, ptr %t924
  %t927 = call %NxVal @nx_clone(%NxVal %t926)
  ret %NxVal %t927
mmiss28:
  %t929 = getelementptr %NxVal, ptr %args, i64 0
  %t930 = load %NxVal, ptr %t929
  %t931 = extractvalue %NxVal %t930, 1
  store i64 %t931, ptr %t928
  %t933 = getelementptr %NxVal, ptr %args, i64 1
  %t934 = load %NxVal, ptr %t933
  %t935 = extractvalue %NxVal %t934, 1
  store i64 %t935, ptr %t932
  %t936 = load i64, ptr %t928
  %t937 = load i64, ptr %t932
  %t938 = add i64 %t936, %t937
  %t939 = add i64 13, 0
  %t940 = add i64 %t938, %t939
  %t941 = add i64 65535, 0
  %t942 = and i64 %t940, %t941
  store i64 %t942, ptr %t943
  %t944 = load i64, ptr %t943
  %t945 = add i64 3, 0
  %t946 = call i64 @nx_mod_i64(i64 %t944, i64 %t945)
  %t947 = add i64 48, 0
  %t948 = call i64 @nx_mod_i64(i64 %t946, i64 %t947)
  %t949 = add i64 65535, 0
  %t950 = and i64 %t948, %t949
  store i64 %t950, ptr %t943
  %t951 = load i64, ptr %t943
  %t952 = add i64 20, 0
  %t953 = mul i64 %t951, %t952
  %t954 = add i64 2, 0
  %t955 = mul i64 %t953, %t954
  %t956 = add i64 65535, 0
  %t957 = and i64 %t955, %t956
  store i64 %t957, ptr %t943
  %t958 = load i64, ptr %t943
  %t959 = add i64 81, 0
  %t960 = add i64 %t958, %t959
  %t961 = add i64 63, 0
  %t962 = add i64 %t960, %t961
  %t963 = add i64 65535, 0
  %t964 = and i64 %t962, %t963
  store i64 %t964, ptr %t943
  %t965 = load i64, ptr %t943
  %t966 = add i64 14, 0
  %t967 = mul i64 %t965, %t966
  %t968 = add i64 62, 0
  %t969 = mul i64 %t967, %t968
  %t970 = add i64 65535, 0
  %t971 = and i64 %t969, %t970
  store i64 %t971, ptr %t943
  %t972 = load i64, ptr %t943
  %t973 = add i64 36, 0
  %t974 = sub i64 %t972, %t973
  %t975 = add i64 45, 0
  %t976 = sub i64 %t974, %t975
  %t977 = add i64 65535, 0
  %t978 = and i64 %t976, %t977
  store i64 %t978, ptr %t943
  %t979 = load i64, ptr %t943
  %t980 = add i64 69, 0
  %t981 = call i64 @nx_mod_i64(i64 %t979, i64 %t980)
  %t982 = add i64 60, 0
  %t983 = call i64 @nx_mod_i64(i64 %t981, i64 %t982)
  %t984 = add i64 65535, 0
  %t985 = and i64 %t983, %t984
  store i64 %t985, ptr %t943
  %t986 = load i64, ptr %t943
  %t987 = add i64 55, 0
  %t988 = xor i64 %t986, %t987
  %t989 = add i64 39, 0
  %t990 = xor i64 %t988, %t989
  %t991 = add i64 65535, 0
  %t992 = and i64 %t990, %t991
  store i64 %t992, ptr %t943
  %t993 = load i64, ptr %t943
  %t994 = call %NxVal @nx_int(i64 %t993)
  call void @nx_memo_put(i64 6, ptr %args, i64 %nargs, %NxVal %t994)
  ret %NxVal %t994
}
define %NxVal @nx__f_3____main____f14(%NxVal* %args, i64 %nargs) {
entry:
  %t995 = alloca %NxVal
  %t999 = alloca i64
  %t1003 = alloca i64
  %t1014 = alloca i64
  store %NxVal zeroinitializer, ptr %t995
  %t996 = call i1 @nx_memo_get(i64 7, ptr %args, i64 %nargs, ptr %t995)
  br i1 %t996, label %mhit29, label %mmiss30
mhit29:
  %t997 = load %NxVal, ptr %t995
  %t998 = call %NxVal @nx_clone(%NxVal %t997)
  ret %NxVal %t998
mmiss30:
  %t1000 = getelementptr %NxVal, ptr %args, i64 0
  %t1001 = load %NxVal, ptr %t1000
  %t1002 = extractvalue %NxVal %t1001, 1
  store i64 %t1002, ptr %t999
  %t1004 = getelementptr %NxVal, ptr %args, i64 1
  %t1005 = load %NxVal, ptr %t1004
  %t1006 = extractvalue %NxVal %t1005, 1
  store i64 %t1006, ptr %t1003
  %t1007 = load i64, ptr %t999
  %t1008 = load i64, ptr %t1003
  %t1009 = add i64 %t1007, %t1008
  %t1010 = add i64 14, 0
  %t1011 = add i64 %t1009, %t1010
  %t1012 = add i64 65535, 0
  %t1013 = and i64 %t1011, %t1012
  store i64 %t1013, ptr %t1014
  %t1015 = load i64, ptr %t1014
  %t1016 = add i64 17, 0
  %t1017 = add i64 %t1015, %t1016
  %t1018 = add i64 75, 0
  %t1019 = add i64 %t1017, %t1018
  %t1020 = add i64 65535, 0
  %t1021 = and i64 %t1019, %t1020
  store i64 %t1021, ptr %t1014
  %t1022 = load i64, ptr %t1014
  %t1023 = add i64 38, 0
  %t1024 = add i64 %t1022, %t1023
  %t1025 = add i64 77, 0
  %t1026 = add i64 %t1024, %t1025
  %t1027 = add i64 65535, 0
  %t1028 = and i64 %t1026, %t1027
  store i64 %t1028, ptr %t1014
  %t1029 = load i64, ptr %t1014
  %t1030 = add i64 58, 0
  %t1031 = sub i64 %t1029, %t1030
  %t1032 = add i64 2, 0
  %t1033 = sub i64 %t1031, %t1032
  %t1034 = add i64 65535, 0
  %t1035 = and i64 %t1033, %t1034
  store i64 %t1035, ptr %t1014
  %t1036 = load i64, ptr %t1014
  %t1037 = add i64 27, 0
  %t1038 = call i64 @nx_mod_i64(i64 %t1036, i64 %t1037)
  %t1039 = add i64 73, 0
  %t1040 = call i64 @nx_mod_i64(i64 %t1038, i64 %t1039)
  %t1041 = add i64 65535, 0
  %t1042 = and i64 %t1040, %t1041
  store i64 %t1042, ptr %t1014
  %t1043 = load i64, ptr %t1014
  %t1044 = add i64 51, 0
  %t1045 = add i64 %t1043, %t1044
  %t1046 = add i64 40, 0
  %t1047 = add i64 %t1045, %t1046
  %t1048 = add i64 65535, 0
  %t1049 = and i64 %t1047, %t1048
  store i64 %t1049, ptr %t1014
  %t1050 = load i64, ptr %t1014
  %t1051 = add i64 29, 0
  %t1052 = sub i64 %t1050, %t1051
  %t1053 = add i64 61, 0
  %t1054 = sub i64 %t1052, %t1053
  %t1055 = add i64 65535, 0
  %t1056 = and i64 %t1054, %t1055
  store i64 %t1056, ptr %t1014
  %t1057 = load i64, ptr %t1014
  %t1058 = add i64 65, 0
  %t1059 = or i64 %t1057, %t1058
  %t1060 = add i64 34, 0
  %t1061 = or i64 %t1059, %t1060
  %t1062 = add i64 65535, 0
  %t1063 = and i64 %t1061, %t1062
  store i64 %t1063, ptr %t1014
  %t1064 = load i64, ptr %t1014
  %t1065 = call %NxVal @nx_int(i64 %t1064)
  call void @nx_memo_put(i64 7, ptr %args, i64 %nargs, %NxVal %t1065)
  ret %NxVal %t1065
}
define %NxVal @nx__f_3____main____f15(%NxVal* %args, i64 %nargs) {
entry:
  %t1066 = alloca %NxVal
  %t1070 = alloca i64
  %t1074 = alloca i64
  %t1085 = alloca i64
  store %NxVal zeroinitializer, ptr %t1066
  %t1067 = call i1 @nx_memo_get(i64 8, ptr %args, i64 %nargs, ptr %t1066)
  br i1 %t1067, label %mhit31, label %mmiss32
mhit31:
  %t1068 = load %NxVal, ptr %t1066
  %t1069 = call %NxVal @nx_clone(%NxVal %t1068)
  ret %NxVal %t1069
mmiss32:
  %t1071 = getelementptr %NxVal, ptr %args, i64 0
  %t1072 = load %NxVal, ptr %t1071
  %t1073 = extractvalue %NxVal %t1072, 1
  store i64 %t1073, ptr %t1070
  %t1075 = getelementptr %NxVal, ptr %args, i64 1
  %t1076 = load %NxVal, ptr %t1075
  %t1077 = extractvalue %NxVal %t1076, 1
  store i64 %t1077, ptr %t1074
  %t1078 = load i64, ptr %t1070
  %t1079 = load i64, ptr %t1074
  %t1080 = add i64 %t1078, %t1079
  %t1081 = add i64 15, 0
  %t1082 = add i64 %t1080, %t1081
  %t1083 = add i64 65535, 0
  %t1084 = and i64 %t1082, %t1083
  store i64 %t1084, ptr %t1085
  %t1086 = load i64, ptr %t1085
  %t1087 = add i64 6, 0
  %t1088 = or i64 %t1086, %t1087
  %t1089 = add i64 10, 0
  %t1090 = or i64 %t1088, %t1089
  %t1091 = add i64 65535, 0
  %t1092 = and i64 %t1090, %t1091
  store i64 %t1092, ptr %t1085
  %t1093 = load i64, ptr %t1085
  %t1094 = add i64 83, 0
  %t1095 = and i64 %t1093, %t1094
  %t1096 = add i64 11, 0
  %t1097 = and i64 %t1095, %t1096
  %t1098 = add i64 65535, 0
  %t1099 = and i64 %t1097, %t1098
  store i64 %t1099, ptr %t1085
  %t1100 = load i64, ptr %t1085
  %t1101 = add i64 51, 0
  %t1102 = sub i64 %t1100, %t1101
  %t1103 = add i64 29, 0
  %t1104 = sub i64 %t1102, %t1103
  %t1105 = add i64 65535, 0
  %t1106 = and i64 %t1104, %t1105
  store i64 %t1106, ptr %t1085
  %t1107 = load i64, ptr %t1085
  %t1108 = add i64 83, 0
  %t1109 = add i64 %t1107, %t1108
  %t1110 = add i64 52, 0
  %t1111 = add i64 %t1109, %t1110
  %t1112 = add i64 65535, 0
  %t1113 = and i64 %t1111, %t1112
  store i64 %t1113, ptr %t1085
  %t1114 = load i64, ptr %t1085
  %t1115 = add i64 14, 0
  %t1116 = mul i64 %t1114, %t1115
  %t1117 = add i64 50, 0
  %t1118 = mul i64 %t1116, %t1117
  %t1119 = add i64 65535, 0
  %t1120 = and i64 %t1118, %t1119
  store i64 %t1120, ptr %t1085
  %t1121 = load i64, ptr %t1085
  %t1122 = add i64 7, 0
  %t1123 = and i64 %t1121, %t1122
  %t1124 = add i64 39, 0
  %t1125 = and i64 %t1123, %t1124
  %t1126 = add i64 65535, 0
  %t1127 = and i64 %t1125, %t1126
  store i64 %t1127, ptr %t1085
  %t1128 = load i64, ptr %t1085
  %t1129 = add i64 47, 0
  %t1130 = mul i64 %t1128, %t1129
  %t1131 = add i64 11, 0
  %t1132 = mul i64 %t1130, %t1131
  %t1133 = add i64 65535, 0
  %t1134 = and i64 %t1132, %t1133
  store i64 %t1134, ptr %t1085
  %t1135 = load i64, ptr %t1085
  %t1136 = call %NxVal @nx_int(i64 %t1135)
  call void @nx_memo_put(i64 8, ptr %args, i64 %nargs, %NxVal %t1136)
  ret %NxVal %t1136
}
define %NxVal @nx__f_3____main____f16(%NxVal* %args, i64 %nargs) {
entry:
  %t1137 = alloca %NxVal
  %t1141 = alloca i64
  %t1145 = alloca i64
  %t1156 = alloca i64
  store %NxVal zeroinitializer, ptr %t1137
  %t1138 = call i1 @nx_memo_get(i64 9, ptr %args, i64 %nargs, ptr %t1137)
  br i1 %t1138, label %mhit33, label %mmiss34
mhit33:
  %t1139 = load %NxVal, ptr %t1137
  %t1140 = call %NxVal @nx_clone(%NxVal %t1139)
  ret %NxVal %t1140
mmiss34:
  %t1142 = getelementptr %NxVal, ptr %args, i64 0
  %t1143 = load %NxVal, ptr %t1142
  %t1144 = extractvalue %NxVal %t1143, 1
  store i64 %t1144, ptr %t1141
  %t1146 = getelementptr %NxVal, ptr %args, i64 1
  %t1147 = load %NxVal, ptr %t1146
  %t1148 = extractvalue %NxVal %t1147, 1
  store i64 %t1148, ptr %t1145
  %t1149 = load i64, ptr %t1141
  %t1150 = load i64, ptr %t1145
  %t1151 = add i64 %t1149, %t1150
  %t1152 = add i64 16, 0
  %t1153 = add i64 %t1151, %t1152
  %t1154 = add i64 65535, 0
  %t1155 = and i64 %t1153, %t1154
  store i64 %t1155, ptr %t1156
  %t1157 = load i64, ptr %t1156
  %t1158 = add i64 29, 0
  %t1159 = call i64 @nx_mod_i64(i64 %t1157, i64 %t1158)
  %t1160 = add i64 41, 0
  %t1161 = call i64 @nx_mod_i64(i64 %t1159, i64 %t1160)
  %t1162 = add i64 65535, 0
  %t1163 = and i64 %t1161, %t1162
  store i64 %t1163, ptr %t1156
  %t1164 = load i64, ptr %t1156
  %t1165 = add i64 81, 0
  %t1166 = mul i64 %t1164, %t1165
  %t1167 = add i64 51, 0
  %t1168 = mul i64 %t1166, %t1167
  %t1169 = add i64 65535, 0
  %t1170 = and i64 %t1168, %t1169
  store i64 %t1170, ptr %t1156
  %t1171 = load i64, ptr %t1156
  %t1172 = add i64 15, 0
  %t1173 = or i64 %t1171, %t1172
  %t1174 = add i64 79, 0
  %t1175 = or i64 %t1173, %t1174
  %t1176 = add i64 65535, 0
  %t1177 = and i64 %t1175, %t1176
  store i64 %t1177, ptr %t1156
  %t1178 = load i64, ptr %t1156
  %t1179 = add i64 83, 0
  %t1180 = sub i64 %t1178, %t1179
  %t1181 = add i64 15, 0
  %t1182 = sub i64 %t1180, %t1181
  %t1183 = add i64 65535, 0
  %t1184 = and i64 %t1182, %t1183
  store i64 %t1184, ptr %t1156
  %t1185 = load i64, ptr %t1156
  %t1186 = add i64 33, 0
  %t1187 = or i64 %t1185, %t1186
  %t1188 = add i64 45, 0
  %t1189 = or i64 %t1187, %t1188
  %t1190 = add i64 65535, 0
  %t1191 = and i64 %t1189, %t1190
  store i64 %t1191, ptr %t1156
  %t1192 = load i64, ptr %t1156
  %t1193 = add i64 25, 0
  %t1194 = or i64 %t1192, %t1193
  %t1195 = add i64 23, 0
  %t1196 = or i64 %t1194, %t1195
  %t1197 = add i64 65535, 0
  %t1198 = and i64 %t1196, %t1197
  store i64 %t1198, ptr %t1156
  %t1199 = load i64, ptr %t1156
  %t1200 = add i64 23, 0
  %t1201 = call i64 @nx_mod_i64(i64 %t1199, i64 %t1200)
  %t1202 = add i64 2, 0
  %t1203 = call i64 @nx_mod_i64(i64 %t1201, i64 %t1202)
  %t1204 = add i64 65535, 0
  %t1205 = and i64 %t1203, %t1204
  store i64 %t1205, ptr %t1156
  %t1206 = load i64, ptr %t1156
  %t1207 = call %NxVal @nx_int(i64 %t1206)
  call void @nx_memo_put(i64 9, ptr %args, i64 %nargs, %NxVal %t1207)
  ret %NxVal %t1207
}
define %NxVal @nx__f_3____main____f17(%NxVal* %args, i64 %nargs) {
entry:
  %t1208 = alloca %NxVal
  %t1212 = alloca i64
  %t1216 = alloca i64
  %t1227 = alloca i64
  store %NxVal zeroinitializer, ptr %t1208
  %t1209 = call i1 @nx_memo_get(i64 10, ptr %args, i64 %nargs, ptr %t1208)
  br i1 %t1209, label %mhit35, label %mmiss36
mhit35:
  %t1210 = load %NxVal, ptr %t1208
  %t1211 = call %NxVal @nx_clone(%NxVal %t1210)
  ret %NxVal %t1211
mmiss36:
  %t1213 = getelementptr %NxVal, ptr %args, i64 0
  %t1214 = load %NxVal, ptr %t1213
  %t1215 = extractvalue %NxVal %t1214, 1
  store i64 %t1215, ptr %t1212
  %t1217 = getelementptr %NxVal, ptr %args, i64 1
  %t1218 = load %NxVal, ptr %t1217
  %t1219 = extractvalue %NxVal %t1218, 1
  store i64 %t1219, ptr %t1216
  %t1220 = load i64, ptr %t1212
  %t1221 = load i64, ptr %t1216
  %t1222 = add i64 %t1220, %t1221
  %t1223 = add i64 17, 0
  %t1224 = add i64 %t1222, %t1223
  %t1225 = add i64 65535, 0
  %t1226 = and i64 %t1224, %t1225
  store i64 %t1226, ptr %t1227
  %t1228 = load i64, ptr %t1227
  %t1229 = add i64 56, 0
  %t1230 = call i64 @nx_mod_i64(i64 %t1228, i64 %t1229)
  %t1231 = add i64 78, 0
  %t1232 = call i64 @nx_mod_i64(i64 %t1230, i64 %t1231)
  %t1233 = add i64 65535, 0
  %t1234 = and i64 %t1232, %t1233
  store i64 %t1234, ptr %t1227
  %t1235 = load i64, ptr %t1227
  %t1236 = add i64 75, 0
  %t1237 = xor i64 %t1235, %t1236
  %t1238 = add i64 70, 0
  %t1239 = xor i64 %t1237, %t1238
  %t1240 = add i64 65535, 0
  %t1241 = and i64 %t1239, %t1240
  store i64 %t1241, ptr %t1227
  %t1242 = load i64, ptr %t1227
  %t1243 = add i64 63, 0
  %t1244 = or i64 %t1242, %t1243
  %t1245 = add i64 30, 0
  %t1246 = or i64 %t1244, %t1245
  %t1247 = add i64 65535, 0
  %t1248 = and i64 %t1246, %t1247
  store i64 %t1248, ptr %t1227
  %t1249 = load i64, ptr %t1227
  %t1250 = add i64 51, 0
  %t1251 = mul i64 %t1249, %t1250
  %t1252 = add i64 30, 0
  %t1253 = mul i64 %t1251, %t1252
  %t1254 = add i64 65535, 0
  %t1255 = and i64 %t1253, %t1254
  store i64 %t1255, ptr %t1227
  %t1256 = load i64, ptr %t1227
  %t1257 = add i64 87, 0
  %t1258 = and i64 %t1256, %t1257
  %t1259 = add i64 48, 0
  %t1260 = and i64 %t1258, %t1259
  %t1261 = add i64 65535, 0
  %t1262 = and i64 %t1260, %t1261
  store i64 %t1262, ptr %t1227
  %t1263 = load i64, ptr %t1227
  %t1264 = add i64 57, 0
  %t1265 = add i64 %t1263, %t1264
  %t1266 = add i64 44, 0
  %t1267 = add i64 %t1265, %t1266
  %t1268 = add i64 65535, 0
  %t1269 = and i64 %t1267, %t1268
  store i64 %t1269, ptr %t1227
  %t1270 = load i64, ptr %t1227
  %t1271 = add i64 54, 0
  %t1272 = or i64 %t1270, %t1271
  %t1273 = add i64 15, 0
  %t1274 = or i64 %t1272, %t1273
  %t1275 = add i64 65535, 0
  %t1276 = and i64 %t1274, %t1275
  store i64 %t1276, ptr %t1227
  %t1277 = load i64, ptr %t1227
  %t1278 = call %NxVal @nx_int(i64 %t1277)
  call void @nx_memo_put(i64 10, ptr %args, i64 %nargs, %NxVal %t1278)
  ret %NxVal %t1278
}
define %NxVal @nx__f_3____main____f18(%NxVal* %args, i64 %nargs) {
entry:
  %t1279 = alloca %NxVal
  %t1283 = alloca i64
  %t1287 = alloca i64
  %t1298 = alloca i64
  store %NxVal zeroinitializer, ptr %t1279
  %t1280 = call i1 @nx_memo_get(i64 11, ptr %args, i64 %nargs, ptr %t1279)
  br i1 %t1280, label %mhit37, label %mmiss38
mhit37:
  %t1281 = load %NxVal, ptr %t1279
  %t1282 = call %NxVal @nx_clone(%NxVal %t1281)
  ret %NxVal %t1282
mmiss38:
  %t1284 = getelementptr %NxVal, ptr %args, i64 0
  %t1285 = load %NxVal, ptr %t1284
  %t1286 = extractvalue %NxVal %t1285, 1
  store i64 %t1286, ptr %t1283
  %t1288 = getelementptr %NxVal, ptr %args, i64 1
  %t1289 = load %NxVal, ptr %t1288
  %t1290 = extractvalue %NxVal %t1289, 1
  store i64 %t1290, ptr %t1287
  %t1291 = load i64, ptr %t1283
  %t1292 = load i64, ptr %t1287
  %t1293 = add i64 %t1291, %t1292
  %t1294 = add i64 18, 0
  %t1295 = add i64 %t1293, %t1294
  %t1296 = add i64 65535, 0
  %t1297 = and i64 %t1295, %t1296
  store i64 %t1297, ptr %t1298
  %t1299 = load i64, ptr %t1298
  %t1300 = add i64 65, 0
  %t1301 = add i64 %t1299, %t1300
  %t1302 = add i64 44, 0
  %t1303 = add i64 %t1301, %t1302
  %t1304 = add i64 65535, 0
  %t1305 = and i64 %t1303, %t1304
  store i64 %t1305, ptr %t1298
  %t1306 = load i64, ptr %t1298
  %t1307 = add i64 48, 0
  %t1308 = add i64 %t1306, %t1307
  %t1309 = add i64 62, 0
  %t1310 = add i64 %t1308, %t1309
  %t1311 = add i64 65535, 0
  %t1312 = and i64 %t1310, %t1311
  store i64 %t1312, ptr %t1298
  %t1313 = load i64, ptr %t1298
  %t1314 = add i64 95, 0
  %t1315 = add i64 %t1313, %t1314
  %t1316 = add i64 82, 0
  %t1317 = add i64 %t1315, %t1316
  %t1318 = add i64 65535, 0
  %t1319 = and i64 %t1317, %t1318
  store i64 %t1319, ptr %t1298
  %t1320 = load i64, ptr %t1298
  %t1321 = add i64 33, 0
  %t1322 = mul i64 %t1320, %t1321
  %t1323 = add i64 78, 0
  %t1324 = mul i64 %t1322, %t1323
  %t1325 = add i64 65535, 0
  %t1326 = and i64 %t1324, %t1325
  store i64 %t1326, ptr %t1298
  %t1327 = load i64, ptr %t1298
  %t1328 = add i64 72, 0
  %t1329 = add i64 %t1327, %t1328
  %t1330 = add i64 82, 0
  %t1331 = add i64 %t1329, %t1330
  %t1332 = add i64 65535, 0
  %t1333 = and i64 %t1331, %t1332
  store i64 %t1333, ptr %t1298
  %t1334 = load i64, ptr %t1298
  %t1335 = add i64 50, 0
  %t1336 = add i64 %t1334, %t1335
  %t1337 = add i64 7, 0
  %t1338 = add i64 %t1336, %t1337
  %t1339 = add i64 65535, 0
  %t1340 = and i64 %t1338, %t1339
  store i64 %t1340, ptr %t1298
  %t1341 = load i64, ptr %t1298
  %t1342 = add i64 90, 0
  %t1343 = sub i64 %t1341, %t1342
  %t1344 = add i64 57, 0
  %t1345 = sub i64 %t1343, %t1344
  %t1346 = add i64 65535, 0
  %t1347 = and i64 %t1345, %t1346
  store i64 %t1347, ptr %t1298
  %t1348 = load i64, ptr %t1298
  %t1349 = call %NxVal @nx_int(i64 %t1348)
  call void @nx_memo_put(i64 11, ptr %args, i64 %nargs, %NxVal %t1349)
  ret %NxVal %t1349
}
define %NxVal @nx__f_3____main____f19(%NxVal* %args, i64 %nargs) {
entry:
  %t1350 = alloca %NxVal
  %t1354 = alloca i64
  %t1358 = alloca i64
  %t1369 = alloca i64
  store %NxVal zeroinitializer, ptr %t1350
  %t1351 = call i1 @nx_memo_get(i64 12, ptr %args, i64 %nargs, ptr %t1350)
  br i1 %t1351, label %mhit39, label %mmiss40
mhit39:
  %t1352 = load %NxVal, ptr %t1350
  %t1353 = call %NxVal @nx_clone(%NxVal %t1352)
  ret %NxVal %t1353
mmiss40:
  %t1355 = getelementptr %NxVal, ptr %args, i64 0
  %t1356 = load %NxVal, ptr %t1355
  %t1357 = extractvalue %NxVal %t1356, 1
  store i64 %t1357, ptr %t1354
  %t1359 = getelementptr %NxVal, ptr %args, i64 1
  %t1360 = load %NxVal, ptr %t1359
  %t1361 = extractvalue %NxVal %t1360, 1
  store i64 %t1361, ptr %t1358
  %t1362 = load i64, ptr %t1354
  %t1363 = load i64, ptr %t1358
  %t1364 = add i64 %t1362, %t1363
  %t1365 = add i64 19, 0
  %t1366 = add i64 %t1364, %t1365
  %t1367 = add i64 65535, 0
  %t1368 = and i64 %t1366, %t1367
  store i64 %t1368, ptr %t1369
  %t1370 = load i64, ptr %t1369
  %t1371 = add i64 56, 0
  %t1372 = and i64 %t1370, %t1371
  %t1373 = add i64 58, 0
  %t1374 = and i64 %t1372, %t1373
  %t1375 = add i64 65535, 0
  %t1376 = and i64 %t1374, %t1375
  store i64 %t1376, ptr %t1369
  %t1377 = load i64, ptr %t1369
  %t1378 = add i64 2, 0
  %t1379 = mul i64 %t1377, %t1378
  %t1380 = add i64 13, 0
  %t1381 = mul i64 %t1379, %t1380
  %t1382 = add i64 65535, 0
  %t1383 = and i64 %t1381, %t1382
  store i64 %t1383, ptr %t1369
  %t1384 = load i64, ptr %t1369
  %t1385 = add i64 19, 0
  %t1386 = sub i64 %t1384, %t1385
  %t1387 = add i64 37, 0
  %t1388 = sub i64 %t1386, %t1387
  %t1389 = add i64 65535, 0
  %t1390 = and i64 %t1388, %t1389
  store i64 %t1390, ptr %t1369
  %t1391 = load i64, ptr %t1369
  %t1392 = add i64 97, 0
  %t1393 = xor i64 %t1391, %t1392
  %t1394 = add i64 46, 0
  %t1395 = xor i64 %t1393, %t1394
  %t1396 = add i64 65535, 0
  %t1397 = and i64 %t1395, %t1396
  store i64 %t1397, ptr %t1369
  %t1398 = load i64, ptr %t1369
  %t1399 = add i64 6, 0
  %t1400 = add i64 %t1398, %t1399
  %t1401 = add i64 13, 0
  %t1402 = add i64 %t1400, %t1401
  %t1403 = add i64 65535, 0
  %t1404 = and i64 %t1402, %t1403
  store i64 %t1404, ptr %t1369
  %t1405 = load i64, ptr %t1369
  %t1406 = add i64 16, 0
  %t1407 = xor i64 %t1405, %t1406
  %t1408 = add i64 52, 0
  %t1409 = xor i64 %t1407, %t1408
  %t1410 = add i64 65535, 0
  %t1411 = and i64 %t1409, %t1410
  store i64 %t1411, ptr %t1369
  %t1412 = load i64, ptr %t1369
  %t1413 = add i64 18, 0
  %t1414 = sub i64 %t1412, %t1413
  %t1415 = add i64 65, 0
  %t1416 = sub i64 %t1414, %t1415
  %t1417 = add i64 65535, 0
  %t1418 = and i64 %t1416, %t1417
  store i64 %t1418, ptr %t1369
  %t1419 = load i64, ptr %t1369
  %t1420 = call %NxVal @nx_int(i64 %t1419)
  call void @nx_memo_put(i64 12, ptr %args, i64 %nargs, %NxVal %t1420)
  ret %NxVal %t1420
}
define %NxVal @nx__f_3____main____f20(%NxVal* %args, i64 %nargs) {
entry:
  %t1421 = alloca %NxVal
  %t1425 = alloca i64
  %t1429 = alloca i64
  %t1440 = alloca i64
  store %NxVal zeroinitializer, ptr %t1421
  %t1422 = call i1 @nx_memo_get(i64 14, ptr %args, i64 %nargs, ptr %t1421)
  br i1 %t1422, label %mhit41, label %mmiss42
mhit41:
  %t1423 = load %NxVal, ptr %t1421
  %t1424 = call %NxVal @nx_clone(%NxVal %t1423)
  ret %NxVal %t1424
mmiss42:
  %t1426 = getelementptr %NxVal, ptr %args, i64 0
  %t1427 = load %NxVal, ptr %t1426
  %t1428 = extractvalue %NxVal %t1427, 1
  store i64 %t1428, ptr %t1425
  %t1430 = getelementptr %NxVal, ptr %args, i64 1
  %t1431 = load %NxVal, ptr %t1430
  %t1432 = extractvalue %NxVal %t1431, 1
  store i64 %t1432, ptr %t1429
  %t1433 = load i64, ptr %t1425
  %t1434 = load i64, ptr %t1429
  %t1435 = add i64 %t1433, %t1434
  %t1436 = add i64 20, 0
  %t1437 = add i64 %t1435, %t1436
  %t1438 = add i64 65535, 0
  %t1439 = and i64 %t1437, %t1438
  store i64 %t1439, ptr %t1440
  %t1441 = load i64, ptr %t1440
  %t1442 = add i64 81, 0
  %t1443 = sub i64 %t1441, %t1442
  %t1444 = add i64 5, 0
  %t1445 = sub i64 %t1443, %t1444
  %t1446 = add i64 65535, 0
  %t1447 = and i64 %t1445, %t1446
  store i64 %t1447, ptr %t1440
  %t1448 = load i64, ptr %t1440
  %t1449 = add i64 80, 0
  %t1450 = xor i64 %t1448, %t1449
  %t1451 = add i64 45, 0
  %t1452 = xor i64 %t1450, %t1451
  %t1453 = add i64 65535, 0
  %t1454 = and i64 %t1452, %t1453
  store i64 %t1454, ptr %t1440
  %t1455 = load i64, ptr %t1440
  %t1456 = add i64 85, 0
  %t1457 = xor i64 %t1455, %t1456
  %t1458 = add i64 69, 0
  %t1459 = xor i64 %t1457, %t1458
  %t1460 = add i64 65535, 0
  %t1461 = and i64 %t1459, %t1460
  store i64 %t1461, ptr %t1440
  %t1462 = load i64, ptr %t1440
  %t1463 = add i64 88, 0
  %t1464 = call i64 @nx_mod_i64(i64 %t1462, i64 %t1463)
  %t1465 = add i64 20, 0
  %t1466 = call i64 @nx_mod_i64(i64 %t1464, i64 %t1465)
  %t1467 = add i64 65535, 0
  %t1468 = and i64 %t1466, %t1467
  store i64 %t1468, ptr %t1440
  %t1469 = load i64, ptr %t1440
  %t1470 = add i64 81, 0
  %t1471 = sub i64 %t1469, %t1470
  %t1472 = add i64 15, 0
  %t1473 = sub i64 %t1471, %t1472
  %t1474 = add i64 65535, 0
  %t1475 = and i64 %t1473, %t1474
  store i64 %t1475, ptr %t1440
  %t1476 = load i64, ptr %t1440
  %t1477 = add i64 36, 0
  %t1478 = and i64 %t1476, %t1477
  %t1479 = add i64 66, 0
  %t1480 = and i64 %t1478, %t1479
  %t1481 = add i64 65535, 0
  %t1482 = and i64 %t1480, %t1481
  store i64 %t1482, ptr %t1440
  %t1483 = load i64, ptr %t1440
  %t1484 = add i64 84, 0
  %t1485 = and i64 %t1483, %t1484
  %t1486 = add i64 25, 0
  %t1487 = and i64 %t1485, %t1486
  %t1488 = add i64 65535, 0
  %t1489 = and i64 %t1487, %t1488
  store i64 %t1489, ptr %t1440
  %t1490 = load i64, ptr %t1440
  %t1491 = call %NxVal @nx_int(i64 %t1490)
  call void @nx_memo_put(i64 14, ptr %args, i64 %nargs, %NxVal %t1491)
  ret %NxVal %t1491
}
define %NxVal @nx__f_3____main____f21(%NxVal* %args, i64 %nargs) {
entry:
  %t1492 = alloca %NxVal
  %t1496 = alloca i64
  %t1500 = alloca i64
  %t1511 = alloca i64
  store %NxVal zeroinitializer, ptr %t1492
  %t1493 = call i1 @nx_memo_get(i64 15, ptr %args, i64 %nargs, ptr %t1492)
  br i1 %t1493, label %mhit43, label %mmiss44
mhit43:
  %t1494 = load %NxVal, ptr %t1492
  %t1495 = call %NxVal @nx_clone(%NxVal %t1494)
  ret %NxVal %t1495
mmiss44:
  %t1497 = getelementptr %NxVal, ptr %args, i64 0
  %t1498 = load %NxVal, ptr %t1497
  %t1499 = extractvalue %NxVal %t1498, 1
  store i64 %t1499, ptr %t1496
  %t1501 = getelementptr %NxVal, ptr %args, i64 1
  %t1502 = load %NxVal, ptr %t1501
  %t1503 = extractvalue %NxVal %t1502, 1
  store i64 %t1503, ptr %t1500
  %t1504 = load i64, ptr %t1496
  %t1505 = load i64, ptr %t1500
  %t1506 = add i64 %t1504, %t1505
  %t1507 = add i64 21, 0
  %t1508 = add i64 %t1506, %t1507
  %t1509 = add i64 65535, 0
  %t1510 = and i64 %t1508, %t1509
  store i64 %t1510, ptr %t1511
  %t1512 = load i64, ptr %t1511
  %t1513 = add i64 11, 0
  %t1514 = sub i64 %t1512, %t1513
  %t1515 = add i64 66, 0
  %t1516 = sub i64 %t1514, %t1515
  %t1517 = add i64 65535, 0
  %t1518 = and i64 %t1516, %t1517
  store i64 %t1518, ptr %t1511
  %t1519 = load i64, ptr %t1511
  %t1520 = add i64 3, 0
  %t1521 = add i64 %t1519, %t1520
  %t1522 = add i64 61, 0
  %t1523 = add i64 %t1521, %t1522
  %t1524 = add i64 65535, 0
  %t1525 = and i64 %t1523, %t1524
  store i64 %t1525, ptr %t1511
  %t1526 = load i64, ptr %t1511
  %t1527 = add i64 61, 0
  %t1528 = mul i64 %t1526, %t1527
  %t1529 = add i64 48, 0
  %t1530 = mul i64 %t1528, %t1529
  %t1531 = add i64 65535, 0
  %t1532 = and i64 %t1530, %t1531
  store i64 %t1532, ptr %t1511
  %t1533 = load i64, ptr %t1511
  %t1534 = add i64 45, 0
  %t1535 = mul i64 %t1533, %t1534
  %t1536 = add i64 37, 0
  %t1537 = mul i64 %t1535, %t1536
  %t1538 = add i64 65535, 0
  %t1539 = and i64 %t1537, %t1538
  store i64 %t1539, ptr %t1511
  %t1540 = load i64, ptr %t1511
  %t1541 = add i64 21, 0
  %t1542 = and i64 %t1540, %t1541
  %t1543 = add i64 41, 0
  %t1544 = and i64 %t1542, %t1543
  %t1545 = add i64 65535, 0
  %t1546 = and i64 %t1544, %t1545
  store i64 %t1546, ptr %t1511
  %t1547 = load i64, ptr %t1511
  %t1548 = add i64 28, 0
  %t1549 = and i64 %t1547, %t1548
  %t1550 = add i64 39, 0
  %t1551 = and i64 %t1549, %t1550
  %t1552 = add i64 65535, 0
  %t1553 = and i64 %t1551, %t1552
  store i64 %t1553, ptr %t1511
  %t1554 = load i64, ptr %t1511
  %t1555 = add i64 75, 0
  %t1556 = mul i64 %t1554, %t1555
  %t1557 = add i64 8, 0
  %t1558 = mul i64 %t1556, %t1557
  %t1559 = add i64 65535, 0
  %t1560 = and i64 %t1558, %t1559
  store i64 %t1560, ptr %t1511
  %t1561 = load i64, ptr %t1511
  %t1562 = call %NxVal @nx_int(i64 %t1561)
  call void @nx_memo_put(i64 15, ptr %args, i64 %nargs, %NxVal %t1562)
  ret %NxVal %t1562
}
define %NxVal @nx__f_3____main____f22(%NxVal* %args, i64 %nargs) {
entry:
  %t1563 = alloca %NxVal
  %t1567 = alloca i64
  %t1571 = alloca i64
  %t1582 = alloca i64
  store %NxVal zeroinitializer, ptr %t1563
  %t1564 = call i1 @nx_memo_get(i64 16, ptr %args, i64 %nargs, ptr %t1563)
  br i1 %t1564, label %mhit45, label %mmiss46
mhit45:
  %t1565 = load %NxVal, ptr %t1563
  %t1566 = call %NxVal @nx_clone(%NxVal %t1565)
  ret %NxVal %t1566
mmiss46:
  %t1568 = getelementptr %NxVal, ptr %args, i64 0
  %t1569 = load %NxVal, ptr %t1568
  %t1570 = extractvalue %NxVal %t1569, 1
  store i64 %t1570, ptr %t1567
  %t1572 = getelementptr %NxVal, ptr %args, i64 1
  %t1573 = load %NxVal, ptr %t1572
  %t1574 = extractvalue %NxVal %t1573, 1
  store i64 %t1574, ptr %t1571
  %t1575 = load i64, ptr %t1567
  %t1576 = load i64, ptr %t1571
  %t1577 = add i64 %t1575, %t1576
  %t1578 = add i64 22, 0
  %t1579 = add i64 %t1577, %t1578
  %t1580 = add i64 65535, 0
  %t1581 = and i64 %t1579, %t1580
  store i64 %t1581, ptr %t1582
  %t1583 = load i64, ptr %t1582
  %t1584 = add i64 38, 0
  %t1585 = and i64 %t1583, %t1584
  %t1586 = add i64 87, 0
  %t1587 = and i64 %t1585, %t1586
  %t1588 = add i64 65535, 0
  %t1589 = and i64 %t1587, %t1588
  store i64 %t1589, ptr %t1582
  %t1590 = load i64, ptr %t1582
  %t1591 = add i64 37, 0
  %t1592 = call i64 @nx_mod_i64(i64 %t1590, i64 %t1591)
  %t1593 = add i64 9, 0
  %t1594 = call i64 @nx_mod_i64(i64 %t1592, i64 %t1593)
  %t1595 = add i64 65535, 0
  %t1596 = and i64 %t1594, %t1595
  store i64 %t1596, ptr %t1582
  %t1597 = load i64, ptr %t1582
  %t1598 = add i64 16, 0
  %t1599 = xor i64 %t1597, %t1598
  %t1600 = add i64 52, 0
  %t1601 = xor i64 %t1599, %t1600
  %t1602 = add i64 65535, 0
  %t1603 = and i64 %t1601, %t1602
  store i64 %t1603, ptr %t1582
  %t1604 = load i64, ptr %t1582
  %t1605 = add i64 89, 0
  %t1606 = mul i64 %t1604, %t1605
  %t1607 = add i64 49, 0
  %t1608 = mul i64 %t1606, %t1607
  %t1609 = add i64 65535, 0
  %t1610 = and i64 %t1608, %t1609
  store i64 %t1610, ptr %t1582
  %t1611 = load i64, ptr %t1582
  %t1612 = add i64 44, 0
  %t1613 = add i64 %t1611, %t1612
  %t1614 = add i64 24, 0
  %t1615 = add i64 %t1613, %t1614
  %t1616 = add i64 65535, 0
  %t1617 = and i64 %t1615, %t1616
  store i64 %t1617, ptr %t1582
  %t1618 = load i64, ptr %t1582
  %t1619 = add i64 66, 0
  %t1620 = or i64 %t1618, %t1619
  %t1621 = add i64 36, 0
  %t1622 = or i64 %t1620, %t1621
  %t1623 = add i64 65535, 0
  %t1624 = and i64 %t1622, %t1623
  store i64 %t1624, ptr %t1582
  %t1625 = load i64, ptr %t1582
  %t1626 = add i64 57, 0
  %t1627 = and i64 %t1625, %t1626
  %t1628 = add i64 2, 0
  %t1629 = and i64 %t1627, %t1628
  %t1630 = add i64 65535, 0
  %t1631 = and i64 %t1629, %t1630
  store i64 %t1631, ptr %t1582
  %t1632 = load i64, ptr %t1582
  %t1633 = call %NxVal @nx_int(i64 %t1632)
  call void @nx_memo_put(i64 16, ptr %args, i64 %nargs, %NxVal %t1633)
  ret %NxVal %t1633
}
define %NxVal @nx__f_3____main____f23(%NxVal* %args, i64 %nargs) {
entry:
  %t1634 = alloca %NxVal
  %t1638 = alloca i64
  %t1642 = alloca i64
  %t1653 = alloca i64
  store %NxVal zeroinitializer, ptr %t1634
  %t1635 = call i1 @nx_memo_get(i64 17, ptr %args, i64 %nargs, ptr %t1634)
  br i1 %t1635, label %mhit47, label %mmiss48
mhit47:
  %t1636 = load %NxVal, ptr %t1634
  %t1637 = call %NxVal @nx_clone(%NxVal %t1636)
  ret %NxVal %t1637
mmiss48:
  %t1639 = getelementptr %NxVal, ptr %args, i64 0
  %t1640 = load %NxVal, ptr %t1639
  %t1641 = extractvalue %NxVal %t1640, 1
  store i64 %t1641, ptr %t1638
  %t1643 = getelementptr %NxVal, ptr %args, i64 1
  %t1644 = load %NxVal, ptr %t1643
  %t1645 = extractvalue %NxVal %t1644, 1
  store i64 %t1645, ptr %t1642
  %t1646 = load i64, ptr %t1638
  %t1647 = load i64, ptr %t1642
  %t1648 = add i64 %t1646, %t1647
  %t1649 = add i64 23, 0
  %t1650 = add i64 %t1648, %t1649
  %t1651 = add i64 65535, 0
  %t1652 = and i64 %t1650, %t1651
  store i64 %t1652, ptr %t1653
  %t1654 = load i64, ptr %t1653
  %t1655 = add i64 26, 0
  %t1656 = call i64 @nx_mod_i64(i64 %t1654, i64 %t1655)
  %t1657 = add i64 88, 0
  %t1658 = call i64 @nx_mod_i64(i64 %t1656, i64 %t1657)
  %t1659 = add i64 65535, 0
  %t1660 = and i64 %t1658, %t1659
  store i64 %t1660, ptr %t1653
  %t1661 = load i64, ptr %t1653
  %t1662 = add i64 15, 0
  %t1663 = add i64 %t1661, %t1662
  %t1664 = add i64 53, 0
  %t1665 = add i64 %t1663, %t1664
  %t1666 = add i64 65535, 0
  %t1667 = and i64 %t1665, %t1666
  store i64 %t1667, ptr %t1653
  %t1668 = load i64, ptr %t1653
  %t1669 = add i64 95, 0
  %t1670 = call i64 @nx_mod_i64(i64 %t1668, i64 %t1669)
  %t1671 = add i64 82, 0
  %t1672 = call i64 @nx_mod_i64(i64 %t1670, i64 %t1671)
  %t1673 = add i64 65535, 0
  %t1674 = and i64 %t1672, %t1673
  store i64 %t1674, ptr %t1653
  %t1675 = load i64, ptr %t1653
  %t1676 = add i64 77, 0
  %t1677 = call i64 @nx_mod_i64(i64 %t1675, i64 %t1676)
  %t1678 = add i64 17, 0
  %t1679 = call i64 @nx_mod_i64(i64 %t1677, i64 %t1678)
  %t1680 = add i64 65535, 0
  %t1681 = and i64 %t1679, %t1680
  store i64 %t1681, ptr %t1653
  %t1682 = load i64, ptr %t1653
  %t1683 = add i64 89, 0
  %t1684 = and i64 %t1682, %t1683
  %t1685 = add i64 26, 0
  %t1686 = and i64 %t1684, %t1685
  %t1687 = add i64 65535, 0
  %t1688 = and i64 %t1686, %t1687
  store i64 %t1688, ptr %t1653
  %t1689 = load i64, ptr %t1653
  %t1690 = add i64 3, 0
  %t1691 = or i64 %t1689, %t1690
  %t1692 = add i64 89, 0
  %t1693 = or i64 %t1691, %t1692
  %t1694 = add i64 65535, 0
  %t1695 = and i64 %t1693, %t1694
  store i64 %t1695, ptr %t1653
  %t1696 = load i64, ptr %t1653
  %t1697 = add i64 30, 0
  %t1698 = and i64 %t1696, %t1697
  %t1699 = add i64 58, 0
  %t1700 = and i64 %t1698, %t1699
  %t1701 = add i64 65535, 0
  %t1702 = and i64 %t1700, %t1701
  store i64 %t1702, ptr %t1653
  %t1703 = load i64, ptr %t1653
  %t1704 = call %NxVal @nx_int(i64 %t1703)
  call void @nx_memo_put(i64 17, ptr %args, i64 %nargs, %NxVal %t1704)
  ret %NxVal %t1704
}
define %NxVal @nx__f_3____main____f24(%NxVal* %args, i64 %nargs) {
entry:
  %t1705 = alloca %NxVal
  %t1709 = alloca i64
  %t1713 = alloca i64
  %t1724 = alloca i64
  store %NxVal zeroinitializer, ptr %t1705
  %t1706 = call i1 @nx_memo_get(i64 18, ptr %args, i64 %nargs, ptr %t1705)
  br i1 %t1706, label %mhit49, label %mmiss50
mhit49:
  %t1707 = load %NxVal, ptr %t1705
  %t1708 = call %NxVal @nx_clone(%NxVal %t1707)
  ret %NxVal %t1708
mmiss50:
  %t1710 = getelementptr %NxVal, ptr %args, i64 0
  %t1711 = load %NxVal, ptr %t1710
  %t1712 = extractvalue %NxVal %t1711, 1
  store i64 %t1712, ptr %t1709
  %t1714 = getelementptr %NxVal, ptr %args, i64 1
  %t1715 = load %NxVal, ptr %t1714
  %t1716 = extractvalue %NxVal %t1715, 1
  store i64 %t1716, ptr %t1713
  %t1717 = load i64, ptr %t1709
  %t1718 = load i64, ptr %t1713
  %t1719 = add i64 %t1717, %t1718
  %t1720 = add i64 24, 0
  %t1721 = add i64 %t1719, %t1720
  %t1722 = add i64 65535, 0
  %t1723 = and i64 %t1721, %t1722
  store i64 %t1723, ptr %t1724
  %t1725 = load i64, ptr %t1724
  %t1726 = add i64 8, 0
  %t1727 = sub i64 %t1725, %t1726
  %t1728 = add i64 38, 0
  %t1729 = sub i64 %t1727, %t1728
  %t1730 = add i64 65535, 0
  %t1731 = and i64 %t1729, %t1730
  store i64 %t1731, ptr %t1724
  %t1732 = load i64, ptr %t1724
  %t1733 = add i64 20, 0
  %t1734 = mul i64 %t1732, %t1733
  %t1735 = add i64 20, 0
  %t1736 = mul i64 %t1734, %t1735
  %t1737 = add i64 65535, 0
  %t1738 = and i64 %t1736, %t1737
  store i64 %t1738, ptr %t1724
  %t1739 = load i64, ptr %t1724
  %t1740 = add i64 11, 0
  %t1741 = sub i64 %t1739, %t1740
  %t1742 = add i64 36, 0
  %t1743 = sub i64 %t1741, %t1742
  %t1744 = add i64 65535, 0
  %t1745 = and i64 %t1743, %t1744
  store i64 %t1745, ptr %t1724
  %t1746 = load i64, ptr %t1724
  %t1747 = add i64 26, 0
  %t1748 = add i64 %t1746, %t1747
  %t1749 = add i64 31, 0
  %t1750 = add i64 %t1748, %t1749
  %t1751 = add i64 65535, 0
  %t1752 = and i64 %t1750, %t1751
  store i64 %t1752, ptr %t1724
  %t1753 = load i64, ptr %t1724
  %t1754 = add i64 88, 0
  %t1755 = or i64 %t1753, %t1754
  %t1756 = add i64 7, 0
  %t1757 = or i64 %t1755, %t1756
  %t1758 = add i64 65535, 0
  %t1759 = and i64 %t1757, %t1758
  store i64 %t1759, ptr %t1724
  %t1760 = load i64, ptr %t1724
  %t1761 = add i64 32, 0
  %t1762 = mul i64 %t1760, %t1761
  %t1763 = add i64 6, 0
  %t1764 = mul i64 %t1762, %t1763
  %t1765 = add i64 65535, 0
  %t1766 = and i64 %t1764, %t1765
  store i64 %t1766, ptr %t1724
  %t1767 = load i64, ptr %t1724
  %t1768 = add i64 55, 0
  %t1769 = sub i64 %t1767, %t1768
  %t1770 = add i64 3, 0
  %t1771 = sub i64 %t1769, %t1770
  %t1772 = add i64 65535, 0
  %t1773 = and i64 %t1771, %t1772
  store i64 %t1773, ptr %t1724
  %t1774 = load i64, ptr %t1724
  %t1775 = call %NxVal @nx_int(i64 %t1774)
  call void @nx_memo_put(i64 18, ptr %args, i64 %nargs, %NxVal %t1775)
  ret %NxVal %t1775
}
define %NxVal @nx__f_3____main____f25(%NxVal* %args, i64 %nargs) {
entry:
  %t1776 = alloca %NxVal
  %t1780 = alloca i64
  %t1784 = alloca i64
  %t1795 = alloca i64
  store %NxVal zeroinitializer, ptr %t1776
  %t1777 = call i1 @nx_memo_get(i64 19, ptr %args, i64 %nargs, ptr %t1776)
  br i1 %t1777, label %mhit51, label %mmiss52
mhit51:
  %t1778 = load %NxVal, ptr %t1776
  %t1779 = call %NxVal @nx_clone(%NxVal %t1778)
  ret %NxVal %t1779
mmiss52:
  %t1781 = getelementptr %NxVal, ptr %args, i64 0
  %t1782 = load %NxVal, ptr %t1781
  %t1783 = extractvalue %NxVal %t1782, 1
  store i64 %t1783, ptr %t1780
  %t1785 = getelementptr %NxVal, ptr %args, i64 1
  %t1786 = load %NxVal, ptr %t1785
  %t1787 = extractvalue %NxVal %t1786, 1
  store i64 %t1787, ptr %t1784
  %t1788 = load i64, ptr %t1780
  %t1789 = load i64, ptr %t1784
  %t1790 = add i64 %t1788, %t1789
  %t1791 = add i64 25, 0
  %t1792 = add i64 %t1790, %t1791
  %t1793 = add i64 65535, 0
  %t1794 = and i64 %t1792, %t1793
  store i64 %t1794, ptr %t1795
  %t1796 = load i64, ptr %t1795
  %t1797 = add i64 48, 0
  %t1798 = or i64 %t1796, %t1797
  %t1799 = add i64 30, 0
  %t1800 = or i64 %t1798, %t1799
  %t1801 = add i64 65535, 0
  %t1802 = and i64 %t1800, %t1801
  store i64 %t1802, ptr %t1795
  %t1803 = load i64, ptr %t1795
  %t1804 = add i64 24, 0
  %t1805 = xor i64 %t1803, %t1804
  %t1806 = add i64 56, 0
  %t1807 = xor i64 %t1805, %t1806
  %t1808 = add i64 65535, 0
  %t1809 = and i64 %t1807, %t1808
  store i64 %t1809, ptr %t1795
  %t1810 = load i64, ptr %t1795
  %t1811 = add i64 22, 0
  %t1812 = or i64 %t1810, %t1811
  %t1813 = add i64 64, 0
  %t1814 = or i64 %t1812, %t1813
  %t1815 = add i64 65535, 0
  %t1816 = and i64 %t1814, %t1815
  store i64 %t1816, ptr %t1795
  %t1817 = load i64, ptr %t1795
  %t1818 = add i64 52, 0
  %t1819 = call i64 @nx_mod_i64(i64 %t1817, i64 %t1818)
  %t1820 = add i64 55, 0
  %t1821 = call i64 @nx_mod_i64(i64 %t1819, i64 %t1820)
  %t1822 = add i64 65535, 0
  %t1823 = and i64 %t1821, %t1822
  store i64 %t1823, ptr %t1795
  %t1824 = load i64, ptr %t1795
  %t1825 = add i64 40, 0
  %t1826 = xor i64 %t1824, %t1825
  %t1827 = add i64 57, 0
  %t1828 = xor i64 %t1826, %t1827
  %t1829 = add i64 65535, 0
  %t1830 = and i64 %t1828, %t1829
  store i64 %t1830, ptr %t1795
  %t1831 = load i64, ptr %t1795
  %t1832 = add i64 81, 0
  %t1833 = and i64 %t1831, %t1832
  %t1834 = add i64 71, 0
  %t1835 = and i64 %t1833, %t1834
  %t1836 = add i64 65535, 0
  %t1837 = and i64 %t1835, %t1836
  store i64 %t1837, ptr %t1795
  %t1838 = load i64, ptr %t1795
  %t1839 = add i64 38, 0
  %t1840 = add i64 %t1838, %t1839
  %t1841 = add i64 6, 0
  %t1842 = add i64 %t1840, %t1841
  %t1843 = add i64 65535, 0
  %t1844 = and i64 %t1842, %t1843
  store i64 %t1844, ptr %t1795
  %t1845 = load i64, ptr %t1795
  %t1846 = call %NxVal @nx_int(i64 %t1845)
  call void @nx_memo_put(i64 19, ptr %args, i64 %nargs, %NxVal %t1846)
  ret %NxVal %t1846
}
define %NxVal @nx__f_3____main____f26(%NxVal* %args, i64 %nargs) {
entry:
  %t1847 = alloca %NxVal
  %t1851 = alloca i64
  %t1855 = alloca i64
  %t1866 = alloca i64
  store %NxVal zeroinitializer, ptr %t1847
  %t1848 = call i1 @nx_memo_get(i64 20, ptr %args, i64 %nargs, ptr %t1847)
  br i1 %t1848, label %mhit53, label %mmiss54
mhit53:
  %t1849 = load %NxVal, ptr %t1847
  %t1850 = call %NxVal @nx_clone(%NxVal %t1849)
  ret %NxVal %t1850
mmiss54:
  %t1852 = getelementptr %NxVal, ptr %args, i64 0
  %t1853 = load %NxVal, ptr %t1852
  %t1854 = extractvalue %NxVal %t1853, 1
  store i64 %t1854, ptr %t1851
  %t1856 = getelementptr %NxVal, ptr %args, i64 1
  %t1857 = load %NxVal, ptr %t1856
  %t1858 = extractvalue %NxVal %t1857, 1
  store i64 %t1858, ptr %t1855
  %t1859 = load i64, ptr %t1851
  %t1860 = load i64, ptr %t1855
  %t1861 = add i64 %t1859, %t1860
  %t1862 = add i64 26, 0
  %t1863 = add i64 %t1861, %t1862
  %t1864 = add i64 65535, 0
  %t1865 = and i64 %t1863, %t1864
  store i64 %t1865, ptr %t1866
  %t1867 = load i64, ptr %t1866
  %t1868 = add i64 8, 0
  %t1869 = or i64 %t1867, %t1868
  %t1870 = add i64 35, 0
  %t1871 = or i64 %t1869, %t1870
  %t1872 = add i64 65535, 0
  %t1873 = and i64 %t1871, %t1872
  store i64 %t1873, ptr %t1866
  %t1874 = load i64, ptr %t1866
  %t1875 = add i64 47, 0
  %t1876 = sub i64 %t1874, %t1875
  %t1877 = add i64 85, 0
  %t1878 = sub i64 %t1876, %t1877
  %t1879 = add i64 65535, 0
  %t1880 = and i64 %t1878, %t1879
  store i64 %t1880, ptr %t1866
  %t1881 = load i64, ptr %t1866
  %t1882 = add i64 31, 0
  %t1883 = call i64 @nx_mod_i64(i64 %t1881, i64 %t1882)
  %t1884 = add i64 15, 0
  %t1885 = call i64 @nx_mod_i64(i64 %t1883, i64 %t1884)
  %t1886 = add i64 65535, 0
  %t1887 = and i64 %t1885, %t1886
  store i64 %t1887, ptr %t1866
  %t1888 = load i64, ptr %t1866
  %t1889 = add i64 4, 0
  %t1890 = mul i64 %t1888, %t1889
  %t1891 = add i64 55, 0
  %t1892 = mul i64 %t1890, %t1891
  %t1893 = add i64 65535, 0
  %t1894 = and i64 %t1892, %t1893
  store i64 %t1894, ptr %t1866
  %t1895 = load i64, ptr %t1866
  %t1896 = add i64 53, 0
  %t1897 = or i64 %t1895, %t1896
  %t1898 = add i64 19, 0
  %t1899 = or i64 %t1897, %t1898
  %t1900 = add i64 65535, 0
  %t1901 = and i64 %t1899, %t1900
  store i64 %t1901, ptr %t1866
  %t1902 = load i64, ptr %t1866
  %t1903 = add i64 71, 0
  %t1904 = xor i64 %t1902, %t1903
  %t1905 = add i64 35, 0
  %t1906 = xor i64 %t1904, %t1905
  %t1907 = add i64 65535, 0
  %t1908 = and i64 %t1906, %t1907
  store i64 %t1908, ptr %t1866
  %t1909 = load i64, ptr %t1866
  %t1910 = add i64 64, 0
  %t1911 = add i64 %t1909, %t1910
  %t1912 = add i64 4, 0
  %t1913 = add i64 %t1911, %t1912
  %t1914 = add i64 65535, 0
  %t1915 = and i64 %t1913, %t1914
  store i64 %t1915, ptr %t1866
  %t1916 = load i64, ptr %t1866
  %t1917 = call %NxVal @nx_int(i64 %t1916)
  call void @nx_memo_put(i64 20, ptr %args, i64 %nargs, %NxVal %t1917)
  ret %NxVal %t1917
}
define %NxVal @nx__f_3____main____f27(%NxVal* %args, i64 %nargs) {
entry:
  %t1918 = alloca %NxVal
  %t1922 = alloca i64
  %t1926 = alloca i64
  %t1937 = alloca i64
  store %NxVal zeroinitializer, ptr %t1918
  %t1919 = call i1 @nx_memo_get(i64 21, ptr %args, i64 %nargs, ptr %t1918)
  br i1 %t1919, label %mhit55, label %mmiss56
mhit55:
  %t1920 = load %NxVal, ptr %t1918
  %t1921 = call %NxVal @nx_clone(%NxVal %t1920)
  ret %NxVal %t1921
mmiss56:
  %t1923 = getelementptr %NxVal, ptr %args, i64 0
  %t1924 = load %NxVal, ptr %t1923
  %t1925 = extractvalue %NxVal %t1924, 1
  store i64 %t1925, ptr %t1922
  %t1927 = getelementptr %NxVal, ptr %args, i64 1
  %t1928 = load %NxVal, ptr %t1927
  %t1929 = extractvalue %NxVal %t1928, 1
  store i64 %t1929, ptr %t1926
  %t1930 = load i64, ptr %t1922
  %t1931 = load i64, ptr %t1926
  %t1932 = add i64 %t1930, %t1931
  %t1933 = add i64 27, 0
  %t1934 = add i64 %t1932, %t1933
  %t1935 = add i64 65535, 0
  %t1936 = and i64 %t1934, %t1935
  store i64 %t1936, ptr %t1937
  %t1938 = load i64, ptr %t1937
  %t1939 = add i64 41, 0
  %t1940 = or i64 %t1938, %t1939
  %t1941 = add i64 39, 0
  %t1942 = or i64 %t1940, %t1941
  %t1943 = add i64 65535, 0
  %t1944 = and i64 %t1942, %t1943
  store i64 %t1944, ptr %t1937
  %t1945 = load i64, ptr %t1937
  %t1946 = add i64 30, 0
  %t1947 = and i64 %t1945, %t1946
  %t1948 = add i64 63, 0
  %t1949 = and i64 %t1947, %t1948
  %t1950 = add i64 65535, 0
  %t1951 = and i64 %t1949, %t1950
  store i64 %t1951, ptr %t1937
  %t1952 = load i64, ptr %t1937
  %t1953 = add i64 57, 0
  %t1954 = or i64 %t1952, %t1953
  %t1955 = add i64 19, 0
  %t1956 = or i64 %t1954, %t1955
  %t1957 = add i64 65535, 0
  %t1958 = and i64 %t1956, %t1957
  store i64 %t1958, ptr %t1937
  %t1959 = load i64, ptr %t1937
  %t1960 = add i64 9, 0
  %t1961 = add i64 %t1959, %t1960
  %t1962 = add i64 31, 0
  %t1963 = add i64 %t1961, %t1962
  %t1964 = add i64 65535, 0
  %t1965 = and i64 %t1963, %t1964
  store i64 %t1965, ptr %t1937
  %t1966 = load i64, ptr %t1937
  %t1967 = add i64 14, 0
  %t1968 = add i64 %t1966, %t1967
  %t1969 = add i64 87, 0
  %t1970 = add i64 %t1968, %t1969
  %t1971 = add i64 65535, 0
  %t1972 = and i64 %t1970, %t1971
  store i64 %t1972, ptr %t1937
  %t1973 = load i64, ptr %t1937
  %t1974 = add i64 75, 0
  %t1975 = add i64 %t1973, %t1974
  %t1976 = add i64 2, 0
  %t1977 = add i64 %t1975, %t1976
  %t1978 = add i64 65535, 0
  %t1979 = and i64 %t1977, %t1978
  store i64 %t1979, ptr %t1937
  %t1980 = load i64, ptr %t1937
  %t1981 = add i64 93, 0
  %t1982 = add i64 %t1980, %t1981
  %t1983 = add i64 10, 0
  %t1984 = add i64 %t1982, %t1983
  %t1985 = add i64 65535, 0
  %t1986 = and i64 %t1984, %t1985
  store i64 %t1986, ptr %t1937
  %t1987 = load i64, ptr %t1937
  %t1988 = call %NxVal @nx_int(i64 %t1987)
  call void @nx_memo_put(i64 21, ptr %args, i64 %nargs, %NxVal %t1988)
  ret %NxVal %t1988
}
define %NxVal @nx__f_3____main____f28(%NxVal* %args, i64 %nargs) {
entry:
  %t1989 = alloca %NxVal
  %t1993 = alloca i64
  %t1997 = alloca i64
  %t2008 = alloca i64
  store %NxVal zeroinitializer, ptr %t1989
  %t1990 = call i1 @nx_memo_get(i64 22, ptr %args, i64 %nargs, ptr %t1989)
  br i1 %t1990, label %mhit57, label %mmiss58
mhit57:
  %t1991 = load %NxVal, ptr %t1989
  %t1992 = call %NxVal @nx_clone(%NxVal %t1991)
  ret %NxVal %t1992
mmiss58:
  %t1994 = getelementptr %NxVal, ptr %args, i64 0
  %t1995 = load %NxVal, ptr %t1994
  %t1996 = extractvalue %NxVal %t1995, 1
  store i64 %t1996, ptr %t1993
  %t1998 = getelementptr %NxVal, ptr %args, i64 1
  %t1999 = load %NxVal, ptr %t1998
  %t2000 = extractvalue %NxVal %t1999, 1
  store i64 %t2000, ptr %t1997
  %t2001 = load i64, ptr %t1993
  %t2002 = load i64, ptr %t1997
  %t2003 = add i64 %t2001, %t2002
  %t2004 = add i64 28, 0
  %t2005 = add i64 %t2003, %t2004
  %t2006 = add i64 65535, 0
  %t2007 = and i64 %t2005, %t2006
  store i64 %t2007, ptr %t2008
  %t2009 = load i64, ptr %t2008
  %t2010 = add i64 86, 0
  %t2011 = sub i64 %t2009, %t2010
  %t2012 = add i64 4, 0
  %t2013 = sub i64 %t2011, %t2012
  %t2014 = add i64 65535, 0
  %t2015 = and i64 %t2013, %t2014
  store i64 %t2015, ptr %t2008
  %t2016 = load i64, ptr %t2008
  %t2017 = add i64 26, 0
  %t2018 = and i64 %t2016, %t2017
  %t2019 = add i64 29, 0
  %t2020 = and i64 %t2018, %t2019
  %t2021 = add i64 65535, 0
  %t2022 = and i64 %t2020, %t2021
  store i64 %t2022, ptr %t2008
  %t2023 = load i64, ptr %t2008
  %t2024 = add i64 96, 0
  %t2025 = mul i64 %t2023, %t2024
  %t2026 = add i64 16, 0
  %t2027 = mul i64 %t2025, %t2026
  %t2028 = add i64 65535, 0
  %t2029 = and i64 %t2027, %t2028
  store i64 %t2029, ptr %t2008
  %t2030 = load i64, ptr %t2008
  %t2031 = add i64 97, 0
  %t2032 = add i64 %t2030, %t2031
  %t2033 = add i64 25, 0
  %t2034 = add i64 %t2032, %t2033
  %t2035 = add i64 65535, 0
  %t2036 = and i64 %t2034, %t2035
  store i64 %t2036, ptr %t2008
  %t2037 = load i64, ptr %t2008
  %t2038 = add i64 50, 0
  %t2039 = and i64 %t2037, %t2038
  %t2040 = add i64 78, 0
  %t2041 = and i64 %t2039, %t2040
  %t2042 = add i64 65535, 0
  %t2043 = and i64 %t2041, %t2042
  store i64 %t2043, ptr %t2008
  %t2044 = load i64, ptr %t2008
  %t2045 = add i64 6, 0
  %t2046 = call i64 @nx_mod_i64(i64 %t2044, i64 %t2045)
  %t2047 = add i64 23, 0
  %t2048 = call i64 @nx_mod_i64(i64 %t2046, i64 %t2047)
  %t2049 = add i64 65535, 0
  %t2050 = and i64 %t2048, %t2049
  store i64 %t2050, ptr %t2008
  %t2051 = load i64, ptr %t2008
  %t2052 = add i64 27, 0
  %t2053 = mul i64 %t2051, %t2052
  %t2054 = add i64 4, 0
  %t2055 = mul i64 %t2053, %t2054
  %t2056 = add i64 65535, 0
  %t2057 = and i64 %t2055, %t2056
  store i64 %t2057, ptr %t2008
  %t2058 = load i64, ptr %t2008
  %t2059 = call %NxVal @nx_int(i64 %t2058)
  call void @nx_memo_put(i64 22, ptr %args, i64 %nargs, %NxVal %t2059)
  ret %NxVal %t2059
}
define %NxVal @nx__f_3____main____f29(%NxVal* %args, i64 %nargs) {
entry:
  %t2060 = alloca %NxVal
  %t2064 = alloca i64
  %t2068 = alloca i64
  %t2079 = alloca i64
  store %NxVal zeroinitializer, ptr %t2060
  %t2061 = call i1 @nx_memo_get(i64 23, ptr %args, i64 %nargs, ptr %t2060)
  br i1 %t2061, label %mhit59, label %mmiss60
mhit59:
  %t2062 = load %NxVal, ptr %t2060
  %t2063 = call %NxVal @nx_clone(%NxVal %t2062)
  ret %NxVal %t2063
mmiss60:
  %t2065 = getelementptr %NxVal, ptr %args, i64 0
  %t2066 = load %NxVal, ptr %t2065
  %t2067 = extractvalue %NxVal %t2066, 1
  store i64 %t2067, ptr %t2064
  %t2069 = getelementptr %NxVal, ptr %args, i64 1
  %t2070 = load %NxVal, ptr %t2069
  %t2071 = extractvalue %NxVal %t2070, 1
  store i64 %t2071, ptr %t2068
  %t2072 = load i64, ptr %t2064
  %t2073 = load i64, ptr %t2068
  %t2074 = add i64 %t2072, %t2073
  %t2075 = add i64 29, 0
  %t2076 = add i64 %t2074, %t2075
  %t2077 = add i64 65535, 0
  %t2078 = and i64 %t2076, %t2077
  store i64 %t2078, ptr %t2079
  %t2080 = load i64, ptr %t2079
  %t2081 = add i64 3, 0
  %t2082 = mul i64 %t2080, %t2081
  %t2083 = add i64 6, 0
  %t2084 = mul i64 %t2082, %t2083
  %t2085 = add i64 65535, 0
  %t2086 = and i64 %t2084, %t2085
  store i64 %t2086, ptr %t2079
  %t2087 = load i64, ptr %t2079
  %t2088 = add i64 35, 0
  %t2089 = add i64 %t2087, %t2088
  %t2090 = add i64 18, 0
  %t2091 = add i64 %t2089, %t2090
  %t2092 = add i64 65535, 0
  %t2093 = and i64 %t2091, %t2092
  store i64 %t2093, ptr %t2079
  %t2094 = load i64, ptr %t2079
  %t2095 = add i64 20, 0
  %t2096 = call i64 @nx_mod_i64(i64 %t2094, i64 %t2095)
  %t2097 = add i64 24, 0
  %t2098 = call i64 @nx_mod_i64(i64 %t2096, i64 %t2097)
  %t2099 = add i64 65535, 0
  %t2100 = and i64 %t2098, %t2099
  store i64 %t2100, ptr %t2079
  %t2101 = load i64, ptr %t2079
  %t2102 = add i64 32, 0
  %t2103 = and i64 %t2101, %t2102
  %t2104 = add i64 36, 0
  %t2105 = and i64 %t2103, %t2104
  %t2106 = add i64 65535, 0
  %t2107 = and i64 %t2105, %t2106
  store i64 %t2107, ptr %t2079
  %t2108 = load i64, ptr %t2079
  %t2109 = add i64 2, 0
  %t2110 = call i64 @nx_mod_i64(i64 %t2108, i64 %t2109)
  %t2111 = add i64 14, 0
  %t2112 = call i64 @nx_mod_i64(i64 %t2110, i64 %t2111)
  %t2113 = add i64 65535, 0
  %t2114 = and i64 %t2112, %t2113
  store i64 %t2114, ptr %t2079
  %t2115 = load i64, ptr %t2079
  %t2116 = add i64 78, 0
  %t2117 = xor i64 %t2115, %t2116
  %t2118 = add i64 42, 0
  %t2119 = xor i64 %t2117, %t2118
  %t2120 = add i64 65535, 0
  %t2121 = and i64 %t2119, %t2120
  store i64 %t2121, ptr %t2079
  %t2122 = load i64, ptr %t2079
  %t2123 = add i64 86, 0
  %t2124 = call i64 @nx_mod_i64(i64 %t2122, i64 %t2123)
  %t2125 = add i64 51, 0
  %t2126 = call i64 @nx_mod_i64(i64 %t2124, i64 %t2125)
  %t2127 = add i64 65535, 0
  %t2128 = and i64 %t2126, %t2127
  store i64 %t2128, ptr %t2079
  %t2129 = load i64, ptr %t2079
  %t2130 = call %NxVal @nx_int(i64 %t2129)
  call void @nx_memo_put(i64 23, ptr %args, i64 %nargs, %NxVal %t2130)
  ret %NxVal %t2130
}
define %NxVal @nx__f_3____main____f30(%NxVal* %args, i64 %nargs) {
entry:
  %t2131 = alloca %NxVal
  %t2135 = alloca i64
  %t2139 = alloca i64
  %t2150 = alloca i64
  store %NxVal zeroinitializer, ptr %t2131
  %t2132 = call i1 @nx_memo_get(i64 25, ptr %args, i64 %nargs, ptr %t2131)
  br i1 %t2132, label %mhit61, label %mmiss62
mhit61:
  %t2133 = load %NxVal, ptr %t2131
  %t2134 = call %NxVal @nx_clone(%NxVal %t2133)
  ret %NxVal %t2134
mmiss62:
  %t2136 = getelementptr %NxVal, ptr %args, i64 0
  %t2137 = load %NxVal, ptr %t2136
  %t2138 = extractvalue %NxVal %t2137, 1
  store i64 %t2138, ptr %t2135
  %t2140 = getelementptr %NxVal, ptr %args, i64 1
  %t2141 = load %NxVal, ptr %t2140
  %t2142 = extractvalue %NxVal %t2141, 1
  store i64 %t2142, ptr %t2139
  %t2143 = load i64, ptr %t2135
  %t2144 = load i64, ptr %t2139
  %t2145 = add i64 %t2143, %t2144
  %t2146 = add i64 30, 0
  %t2147 = add i64 %t2145, %t2146
  %t2148 = add i64 65535, 0
  %t2149 = and i64 %t2147, %t2148
  store i64 %t2149, ptr %t2150
  %t2151 = load i64, ptr %t2150
  %t2152 = add i64 5, 0
  %t2153 = sub i64 %t2151, %t2152
  %t2154 = add i64 8, 0
  %t2155 = sub i64 %t2153, %t2154
  %t2156 = add i64 65535, 0
  %t2157 = and i64 %t2155, %t2156
  store i64 %t2157, ptr %t2150
  %t2158 = load i64, ptr %t2150
  %t2159 = add i64 19, 0
  %t2160 = add i64 %t2158, %t2159
  %t2161 = add i64 26, 0
  %t2162 = add i64 %t2160, %t2161
  %t2163 = add i64 65535, 0
  %t2164 = and i64 %t2162, %t2163
  store i64 %t2164, ptr %t2150
  %t2165 = load i64, ptr %t2150
  %t2166 = add i64 75, 0
  %t2167 = and i64 %t2165, %t2166
  %t2168 = add i64 3, 0
  %t2169 = and i64 %t2167, %t2168
  %t2170 = add i64 65535, 0
  %t2171 = and i64 %t2169, %t2170
  store i64 %t2171, ptr %t2150
  %t2172 = load i64, ptr %t2150
  %t2173 = add i64 79, 0
  %t2174 = xor i64 %t2172, %t2173
  %t2175 = add i64 71, 0
  %t2176 = xor i64 %t2174, %t2175
  %t2177 = add i64 65535, 0
  %t2178 = and i64 %t2176, %t2177
  store i64 %t2178, ptr %t2150
  %t2179 = load i64, ptr %t2150
  %t2180 = add i64 65, 0
  %t2181 = xor i64 %t2179, %t2180
  %t2182 = add i64 65, 0
  %t2183 = xor i64 %t2181, %t2182
  %t2184 = add i64 65535, 0
  %t2185 = and i64 %t2183, %t2184
  store i64 %t2185, ptr %t2150
  %t2186 = load i64, ptr %t2150
  %t2187 = add i64 60, 0
  %t2188 = xor i64 %t2186, %t2187
  %t2189 = add i64 33, 0
  %t2190 = xor i64 %t2188, %t2189
  %t2191 = add i64 65535, 0
  %t2192 = and i64 %t2190, %t2191
  store i64 %t2192, ptr %t2150
  %t2193 = load i64, ptr %t2150
  %t2194 = add i64 38, 0
  %t2195 = mul i64 %t2193, %t2194
  %t2196 = add i64 3, 0
  %t2197 = mul i64 %t2195, %t2196
  %t2198 = add i64 65535, 0
  %t2199 = and i64 %t2197, %t2198
  store i64 %t2199, ptr %t2150
  %t2200 = load i64, ptr %t2150
  %t2201 = call %NxVal @nx_int(i64 %t2200)
  call void @nx_memo_put(i64 25, ptr %args, i64 %nargs, %NxVal %t2201)
  ret %NxVal %t2201
}
define %NxVal @nx__f_3____main____f31(%NxVal* %args, i64 %nargs) {
entry:
  %t2202 = alloca %NxVal
  %t2206 = alloca i64
  %t2210 = alloca i64
  %t2221 = alloca i64
  store %NxVal zeroinitializer, ptr %t2202
  %t2203 = call i1 @nx_memo_get(i64 26, ptr %args, i64 %nargs, ptr %t2202)
  br i1 %t2203, label %mhit63, label %mmiss64
mhit63:
  %t2204 = load %NxVal, ptr %t2202
  %t2205 = call %NxVal @nx_clone(%NxVal %t2204)
  ret %NxVal %t2205
mmiss64:
  %t2207 = getelementptr %NxVal, ptr %args, i64 0
  %t2208 = load %NxVal, ptr %t2207
  %t2209 = extractvalue %NxVal %t2208, 1
  store i64 %t2209, ptr %t2206
  %t2211 = getelementptr %NxVal, ptr %args, i64 1
  %t2212 = load %NxVal, ptr %t2211
  %t2213 = extractvalue %NxVal %t2212, 1
  store i64 %t2213, ptr %t2210
  %t2214 = load i64, ptr %t2206
  %t2215 = load i64, ptr %t2210
  %t2216 = add i64 %t2214, %t2215
  %t2217 = add i64 31, 0
  %t2218 = add i64 %t2216, %t2217
  %t2219 = add i64 65535, 0
  %t2220 = and i64 %t2218, %t2219
  store i64 %t2220, ptr %t2221
  %t2222 = load i64, ptr %t2221
  %t2223 = add i64 90, 0
  %t2224 = and i64 %t2222, %t2223
  %t2225 = add i64 65, 0
  %t2226 = and i64 %t2224, %t2225
  %t2227 = add i64 65535, 0
  %t2228 = and i64 %t2226, %t2227
  store i64 %t2228, ptr %t2221
  %t2229 = load i64, ptr %t2221
  %t2230 = add i64 82, 0
  %t2231 = mul i64 %t2229, %t2230
  %t2232 = add i64 50, 0
  %t2233 = mul i64 %t2231, %t2232
  %t2234 = add i64 65535, 0
  %t2235 = and i64 %t2233, %t2234
  store i64 %t2235, ptr %t2221
  %t2236 = load i64, ptr %t2221
  %t2237 = add i64 16, 0
  %t2238 = call i64 @nx_mod_i64(i64 %t2236, i64 %t2237)
  %t2239 = add i64 83, 0
  %t2240 = call i64 @nx_mod_i64(i64 %t2238, i64 %t2239)
  %t2241 = add i64 65535, 0
  %t2242 = and i64 %t2240, %t2241
  store i64 %t2242, ptr %t2221
  %t2243 = load i64, ptr %t2221
  %t2244 = add i64 89, 0
  %t2245 = add i64 %t2243, %t2244
  %t2246 = add i64 20, 0
  %t2247 = add i64 %t2245, %t2246
  %t2248 = add i64 65535, 0
  %t2249 = and i64 %t2247, %t2248
  store i64 %t2249, ptr %t2221
  %t2250 = load i64, ptr %t2221
  %t2251 = add i64 67, 0
  %t2252 = mul i64 %t2250, %t2251
  %t2253 = add i64 27, 0
  %t2254 = mul i64 %t2252, %t2253
  %t2255 = add i64 65535, 0
  %t2256 = and i64 %t2254, %t2255
  store i64 %t2256, ptr %t2221
  %t2257 = load i64, ptr %t2221
  %t2258 = add i64 81, 0
  %t2259 = mul i64 %t2257, %t2258
  %t2260 = add i64 41, 0
  %t2261 = mul i64 %t2259, %t2260
  %t2262 = add i64 65535, 0
  %t2263 = and i64 %t2261, %t2262
  store i64 %t2263, ptr %t2221
  %t2264 = load i64, ptr %t2221
  %t2265 = add i64 54, 0
  %t2266 = sub i64 %t2264, %t2265
  %t2267 = add i64 84, 0
  %t2268 = sub i64 %t2266, %t2267
  %t2269 = add i64 65535, 0
  %t2270 = and i64 %t2268, %t2269
  store i64 %t2270, ptr %t2221
  %t2271 = load i64, ptr %t2221
  %t2272 = call %NxVal @nx_int(i64 %t2271)
  call void @nx_memo_put(i64 26, ptr %args, i64 %nargs, %NxVal %t2272)
  ret %NxVal %t2272
}
define %NxVal @nx__f_3____main____f32(%NxVal* %args, i64 %nargs) {
entry:
  %t2273 = alloca %NxVal
  %t2277 = alloca i64
  %t2281 = alloca i64
  %t2292 = alloca i64
  store %NxVal zeroinitializer, ptr %t2273
  %t2274 = call i1 @nx_memo_get(i64 27, ptr %args, i64 %nargs, ptr %t2273)
  br i1 %t2274, label %mhit65, label %mmiss66
mhit65:
  %t2275 = load %NxVal, ptr %t2273
  %t2276 = call %NxVal @nx_clone(%NxVal %t2275)
  ret %NxVal %t2276
mmiss66:
  %t2278 = getelementptr %NxVal, ptr %args, i64 0
  %t2279 = load %NxVal, ptr %t2278
  %t2280 = extractvalue %NxVal %t2279, 1
  store i64 %t2280, ptr %t2277
  %t2282 = getelementptr %NxVal, ptr %args, i64 1
  %t2283 = load %NxVal, ptr %t2282
  %t2284 = extractvalue %NxVal %t2283, 1
  store i64 %t2284, ptr %t2281
  %t2285 = load i64, ptr %t2277
  %t2286 = load i64, ptr %t2281
  %t2287 = add i64 %t2285, %t2286
  %t2288 = add i64 32, 0
  %t2289 = add i64 %t2287, %t2288
  %t2290 = add i64 65535, 0
  %t2291 = and i64 %t2289, %t2290
  store i64 %t2291, ptr %t2292
  %t2293 = load i64, ptr %t2292
  %t2294 = add i64 94, 0
  %t2295 = add i64 %t2293, %t2294
  %t2296 = add i64 79, 0
  %t2297 = add i64 %t2295, %t2296
  %t2298 = add i64 65535, 0
  %t2299 = and i64 %t2297, %t2298
  store i64 %t2299, ptr %t2292
  %t2300 = load i64, ptr %t2292
  %t2301 = add i64 62, 0
  %t2302 = sub i64 %t2300, %t2301
  %t2303 = add i64 72, 0
  %t2304 = sub i64 %t2302, %t2303
  %t2305 = add i64 65535, 0
  %t2306 = and i64 %t2304, %t2305
  store i64 %t2306, ptr %t2292
  %t2307 = load i64, ptr %t2292
  %t2308 = add i64 2, 0
  %t2309 = call i64 @nx_mod_i64(i64 %t2307, i64 %t2308)
  %t2310 = add i64 46, 0
  %t2311 = call i64 @nx_mod_i64(i64 %t2309, i64 %t2310)
  %t2312 = add i64 65535, 0
  %t2313 = and i64 %t2311, %t2312
  store i64 %t2313, ptr %t2292
  %t2314 = load i64, ptr %t2292
  %t2315 = add i64 59, 0
  %t2316 = mul i64 %t2314, %t2315
  %t2317 = add i64 68, 0
  %t2318 = mul i64 %t2316, %t2317
  %t2319 = add i64 65535, 0
  %t2320 = and i64 %t2318, %t2319
  store i64 %t2320, ptr %t2292
  %t2321 = load i64, ptr %t2292
  %t2322 = add i64 55, 0
  %t2323 = or i64 %t2321, %t2322
  %t2324 = add i64 18, 0
  %t2325 = or i64 %t2323, %t2324
  %t2326 = add i64 65535, 0
  %t2327 = and i64 %t2325, %t2326
  store i64 %t2327, ptr %t2292
  %t2328 = load i64, ptr %t2292
  %t2329 = add i64 69, 0
  %t2330 = or i64 %t2328, %t2329
  %t2331 = add i64 12, 0
  %t2332 = or i64 %t2330, %t2331
  %t2333 = add i64 65535, 0
  %t2334 = and i64 %t2332, %t2333
  store i64 %t2334, ptr %t2292
  %t2335 = load i64, ptr %t2292
  %t2336 = add i64 15, 0
  %t2337 = and i64 %t2335, %t2336
  %t2338 = add i64 86, 0
  %t2339 = and i64 %t2337, %t2338
  %t2340 = add i64 65535, 0
  %t2341 = and i64 %t2339, %t2340
  store i64 %t2341, ptr %t2292
  %t2342 = load i64, ptr %t2292
  %t2343 = call %NxVal @nx_int(i64 %t2342)
  call void @nx_memo_put(i64 27, ptr %args, i64 %nargs, %NxVal %t2343)
  ret %NxVal %t2343
}
define %NxVal @nx__f_3____main____f33(%NxVal* %args, i64 %nargs) {
entry:
  %t2344 = alloca %NxVal
  %t2348 = alloca i64
  %t2352 = alloca i64
  %t2363 = alloca i64
  store %NxVal zeroinitializer, ptr %t2344
  %t2345 = call i1 @nx_memo_get(i64 28, ptr %args, i64 %nargs, ptr %t2344)
  br i1 %t2345, label %mhit67, label %mmiss68
mhit67:
  %t2346 = load %NxVal, ptr %t2344
  %t2347 = call %NxVal @nx_clone(%NxVal %t2346)
  ret %NxVal %t2347
mmiss68:
  %t2349 = getelementptr %NxVal, ptr %args, i64 0
  %t2350 = load %NxVal, ptr %t2349
  %t2351 = extractvalue %NxVal %t2350, 1
  store i64 %t2351, ptr %t2348
  %t2353 = getelementptr %NxVal, ptr %args, i64 1
  %t2354 = load %NxVal, ptr %t2353
  %t2355 = extractvalue %NxVal %t2354, 1
  store i64 %t2355, ptr %t2352
  %t2356 = load i64, ptr %t2348
  %t2357 = load i64, ptr %t2352
  %t2358 = add i64 %t2356, %t2357
  %t2359 = add i64 33, 0
  %t2360 = add i64 %t2358, %t2359
  %t2361 = add i64 65535, 0
  %t2362 = and i64 %t2360, %t2361
  store i64 %t2362, ptr %t2363
  %t2364 = load i64, ptr %t2363
  %t2365 = add i64 88, 0
  %t2366 = and i64 %t2364, %t2365
  %t2367 = add i64 43, 0
  %t2368 = and i64 %t2366, %t2367
  %t2369 = add i64 65535, 0
  %t2370 = and i64 %t2368, %t2369
  store i64 %t2370, ptr %t2363
  %t2371 = load i64, ptr %t2363
  %t2372 = add i64 31, 0
  %t2373 = sub i64 %t2371, %t2372
  %t2374 = add i64 20, 0
  %t2375 = sub i64 %t2373, %t2374
  %t2376 = add i64 65535, 0
  %t2377 = and i64 %t2375, %t2376
  store i64 %t2377, ptr %t2363
  %t2378 = load i64, ptr %t2363
  %t2379 = add i64 53, 0
  %t2380 = mul i64 %t2378, %t2379
  %t2381 = add i64 83, 0
  %t2382 = mul i64 %t2380, %t2381
  %t2383 = add i64 65535, 0
  %t2384 = and i64 %t2382, %t2383
  store i64 %t2384, ptr %t2363
  %t2385 = load i64, ptr %t2363
  %t2386 = add i64 75, 0
  %t2387 = mul i64 %t2385, %t2386
  %t2388 = add i64 9, 0
  %t2389 = mul i64 %t2387, %t2388
  %t2390 = add i64 65535, 0
  %t2391 = and i64 %t2389, %t2390
  store i64 %t2391, ptr %t2363
  %t2392 = load i64, ptr %t2363
  %t2393 = add i64 65, 0
  %t2394 = call i64 @nx_mod_i64(i64 %t2392, i64 %t2393)
  %t2395 = add i64 3, 0
  %t2396 = call i64 @nx_mod_i64(i64 %t2394, i64 %t2395)
  %t2397 = add i64 65535, 0
  %t2398 = and i64 %t2396, %t2397
  store i64 %t2398, ptr %t2363
  %t2399 = load i64, ptr %t2363
  %t2400 = add i64 64, 0
  %t2401 = and i64 %t2399, %t2400
  %t2402 = add i64 49, 0
  %t2403 = and i64 %t2401, %t2402
  %t2404 = add i64 65535, 0
  %t2405 = and i64 %t2403, %t2404
  store i64 %t2405, ptr %t2363
  %t2406 = load i64, ptr %t2363
  %t2407 = add i64 15, 0
  %t2408 = sub i64 %t2406, %t2407
  %t2409 = add i64 88, 0
  %t2410 = sub i64 %t2408, %t2409
  %t2411 = add i64 65535, 0
  %t2412 = and i64 %t2410, %t2411
  store i64 %t2412, ptr %t2363
  %t2413 = load i64, ptr %t2363
  %t2414 = call %NxVal @nx_int(i64 %t2413)
  call void @nx_memo_put(i64 28, ptr %args, i64 %nargs, %NxVal %t2414)
  ret %NxVal %t2414
}
define %NxVal @nx__f_3____main____f34(%NxVal* %args, i64 %nargs) {
entry:
  %t2415 = alloca %NxVal
  %t2419 = alloca i64
  %t2423 = alloca i64
  %t2434 = alloca i64
  store %NxVal zeroinitializer, ptr %t2415
  %t2416 = call i1 @nx_memo_get(i64 29, ptr %args, i64 %nargs, ptr %t2415)
  br i1 %t2416, label %mhit69, label %mmiss70
mhit69:
  %t2417 = load %NxVal, ptr %t2415
  %t2418 = call %NxVal @nx_clone(%NxVal %t2417)
  ret %NxVal %t2418
mmiss70:
  %t2420 = getelementptr %NxVal, ptr %args, i64 0
  %t2421 = load %NxVal, ptr %t2420
  %t2422 = extractvalue %NxVal %t2421, 1
  store i64 %t2422, ptr %t2419
  %t2424 = getelementptr %NxVal, ptr %args, i64 1
  %t2425 = load %NxVal, ptr %t2424
  %t2426 = extractvalue %NxVal %t2425, 1
  store i64 %t2426, ptr %t2423
  %t2427 = load i64, ptr %t2419
  %t2428 = load i64, ptr %t2423
  %t2429 = add i64 %t2427, %t2428
  %t2430 = add i64 34, 0
  %t2431 = add i64 %t2429, %t2430
  %t2432 = add i64 65535, 0
  %t2433 = and i64 %t2431, %t2432
  store i64 %t2433, ptr %t2434
  %t2435 = load i64, ptr %t2434
  %t2436 = add i64 18, 0
  %t2437 = xor i64 %t2435, %t2436
  %t2438 = add i64 12, 0
  %t2439 = xor i64 %t2437, %t2438
  %t2440 = add i64 65535, 0
  %t2441 = and i64 %t2439, %t2440
  store i64 %t2441, ptr %t2434
  %t2442 = load i64, ptr %t2434
  %t2443 = add i64 44, 0
  %t2444 = mul i64 %t2442, %t2443
  %t2445 = add i64 63, 0
  %t2446 = mul i64 %t2444, %t2445
  %t2447 = add i64 65535, 0
  %t2448 = and i64 %t2446, %t2447
  store i64 %t2448, ptr %t2434
  %t2449 = load i64, ptr %t2434
  %t2450 = add i64 67, 0
  %t2451 = sub i64 %t2449, %t2450
  %t2452 = add i64 84, 0
  %t2453 = sub i64 %t2451, %t2452
  %t2454 = add i64 65535, 0
  %t2455 = and i64 %t2453, %t2454
  store i64 %t2455, ptr %t2434
  %t2456 = load i64, ptr %t2434
  %t2457 = add i64 18, 0
  %t2458 = call i64 @nx_mod_i64(i64 %t2456, i64 %t2457)
  %t2459 = add i64 17, 0
  %t2460 = call i64 @nx_mod_i64(i64 %t2458, i64 %t2459)
  %t2461 = add i64 65535, 0
  %t2462 = and i64 %t2460, %t2461
  store i64 %t2462, ptr %t2434
  %t2463 = load i64, ptr %t2434
  %t2464 = add i64 83, 0
  %t2465 = xor i64 %t2463, %t2464
  %t2466 = add i64 6, 0
  %t2467 = xor i64 %t2465, %t2466
  %t2468 = add i64 65535, 0
  %t2469 = and i64 %t2467, %t2468
  store i64 %t2469, ptr %t2434
  %t2470 = load i64, ptr %t2434
  %t2471 = add i64 84, 0
  %t2472 = sub i64 %t2470, %t2471
  %t2473 = add i64 40, 0
  %t2474 = sub i64 %t2472, %t2473
  %t2475 = add i64 65535, 0
  %t2476 = and i64 %t2474, %t2475
  store i64 %t2476, ptr %t2434
  %t2477 = load i64, ptr %t2434
  %t2478 = add i64 54, 0
  %t2479 = and i64 %t2477, %t2478
  %t2480 = add i64 10, 0
  %t2481 = and i64 %t2479, %t2480
  %t2482 = add i64 65535, 0
  %t2483 = and i64 %t2481, %t2482
  store i64 %t2483, ptr %t2434
  %t2484 = load i64, ptr %t2434
  %t2485 = call %NxVal @nx_int(i64 %t2484)
  call void @nx_memo_put(i64 29, ptr %args, i64 %nargs, %NxVal %t2485)
  ret %NxVal %t2485
}
define %NxVal @nx__f_3____main____f35(%NxVal* %args, i64 %nargs) {
entry:
  %t2486 = alloca %NxVal
  %t2490 = alloca i64
  %t2494 = alloca i64
  %t2505 = alloca i64
  store %NxVal zeroinitializer, ptr %t2486
  %t2487 = call i1 @nx_memo_get(i64 30, ptr %args, i64 %nargs, ptr %t2486)
  br i1 %t2487, label %mhit71, label %mmiss72
mhit71:
  %t2488 = load %NxVal, ptr %t2486
  %t2489 = call %NxVal @nx_clone(%NxVal %t2488)
  ret %NxVal %t2489
mmiss72:
  %t2491 = getelementptr %NxVal, ptr %args, i64 0
  %t2492 = load %NxVal, ptr %t2491
  %t2493 = extractvalue %NxVal %t2492, 1
  store i64 %t2493, ptr %t2490
  %t2495 = getelementptr %NxVal, ptr %args, i64 1
  %t2496 = load %NxVal, ptr %t2495
  %t2497 = extractvalue %NxVal %t2496, 1
  store i64 %t2497, ptr %t2494
  %t2498 = load i64, ptr %t2490
  %t2499 = load i64, ptr %t2494
  %t2500 = add i64 %t2498, %t2499
  %t2501 = add i64 35, 0
  %t2502 = add i64 %t2500, %t2501
  %t2503 = add i64 65535, 0
  %t2504 = and i64 %t2502, %t2503
  store i64 %t2504, ptr %t2505
  %t2506 = load i64, ptr %t2505
  %t2507 = add i64 30, 0
  %t2508 = add i64 %t2506, %t2507
  %t2509 = add i64 52, 0
  %t2510 = add i64 %t2508, %t2509
  %t2511 = add i64 65535, 0
  %t2512 = and i64 %t2510, %t2511
  store i64 %t2512, ptr %t2505
  %t2513 = load i64, ptr %t2505
  %t2514 = add i64 62, 0
  %t2515 = sub i64 %t2513, %t2514
  %t2516 = add i64 12, 0
  %t2517 = sub i64 %t2515, %t2516
  %t2518 = add i64 65535, 0
  %t2519 = and i64 %t2517, %t2518
  store i64 %t2519, ptr %t2505
  %t2520 = load i64, ptr %t2505
  %t2521 = add i64 58, 0
  %t2522 = and i64 %t2520, %t2521
  %t2523 = add i64 7, 0
  %t2524 = and i64 %t2522, %t2523
  %t2525 = add i64 65535, 0
  %t2526 = and i64 %t2524, %t2525
  store i64 %t2526, ptr %t2505
  %t2527 = load i64, ptr %t2505
  %t2528 = add i64 97, 0
  %t2529 = sub i64 %t2527, %t2528
  %t2530 = add i64 64, 0
  %t2531 = sub i64 %t2529, %t2530
  %t2532 = add i64 65535, 0
  %t2533 = and i64 %t2531, %t2532
  store i64 %t2533, ptr %t2505
  %t2534 = load i64, ptr %t2505
  %t2535 = add i64 79, 0
  %t2536 = or i64 %t2534, %t2535
  %t2537 = add i64 85, 0
  %t2538 = or i64 %t2536, %t2537
  %t2539 = add i64 65535, 0
  %t2540 = and i64 %t2538, %t2539
  store i64 %t2540, ptr %t2505
  %t2541 = load i64, ptr %t2505
  %t2542 = add i64 16, 0
  %t2543 = add i64 %t2541, %t2542
  %t2544 = add i64 53, 0
  %t2545 = add i64 %t2543, %t2544
  %t2546 = add i64 65535, 0
  %t2547 = and i64 %t2545, %t2546
  store i64 %t2547, ptr %t2505
  %t2548 = load i64, ptr %t2505
  %t2549 = add i64 41, 0
  %t2550 = add i64 %t2548, %t2549
  %t2551 = add i64 51, 0
  %t2552 = add i64 %t2550, %t2551
  %t2553 = add i64 65535, 0
  %t2554 = and i64 %t2552, %t2553
  store i64 %t2554, ptr %t2505
  %t2555 = load i64, ptr %t2505
  %t2556 = call %NxVal @nx_int(i64 %t2555)
  call void @nx_memo_put(i64 30, ptr %args, i64 %nargs, %NxVal %t2556)
  ret %NxVal %t2556
}
define %NxVal @nx__f_3____main____f36(%NxVal* %args, i64 %nargs) {
entry:
  %t2557 = alloca %NxVal
  %t2561 = alloca i64
  %t2565 = alloca i64
  %t2576 = alloca i64
  store %NxVal zeroinitializer, ptr %t2557
  %t2558 = call i1 @nx_memo_get(i64 31, ptr %args, i64 %nargs, ptr %t2557)
  br i1 %t2558, label %mhit73, label %mmiss74
mhit73:
  %t2559 = load %NxVal, ptr %t2557
  %t2560 = call %NxVal @nx_clone(%NxVal %t2559)
  ret %NxVal %t2560
mmiss74:
  %t2562 = getelementptr %NxVal, ptr %args, i64 0
  %t2563 = load %NxVal, ptr %t2562
  %t2564 = extractvalue %NxVal %t2563, 1
  store i64 %t2564, ptr %t2561
  %t2566 = getelementptr %NxVal, ptr %args, i64 1
  %t2567 = load %NxVal, ptr %t2566
  %t2568 = extractvalue %NxVal %t2567, 1
  store i64 %t2568, ptr %t2565
  %t2569 = load i64, ptr %t2561
  %t2570 = load i64, ptr %t2565
  %t2571 = add i64 %t2569, %t2570
  %t2572 = add i64 36, 0
  %t2573 = add i64 %t2571, %t2572
  %t2574 = add i64 65535, 0
  %t2575 = and i64 %t2573, %t2574
  store i64 %t2575, ptr %t2576
  %t2577 = load i64, ptr %t2576
  %t2578 = add i64 49, 0
  %t2579 = call i64 @nx_mod_i64(i64 %t2577, i64 %t2578)
  %t2580 = add i64 3, 0
  %t2581 = call i64 @nx_mod_i64(i64 %t2579, i64 %t2580)
  %t2582 = add i64 65535, 0
  %t2583 = and i64 %t2581, %t2582
  store i64 %t2583, ptr %t2576
  %t2584 = load i64, ptr %t2576
  %t2585 = add i64 95, 0
  %t2586 = xor i64 %t2584, %t2585
  %t2587 = add i64 37, 0
  %t2588 = xor i64 %t2586, %t2587
  %t2589 = add i64 65535, 0
  %t2590 = and i64 %t2588, %t2589
  store i64 %t2590, ptr %t2576
  %t2591 = load i64, ptr %t2576
  %t2592 = add i64 43, 0
  %t2593 = or i64 %t2591, %t2592
  %t2594 = add i64 31, 0
  %t2595 = or i64 %t2593, %t2594
  %t2596 = add i64 65535, 0
  %t2597 = and i64 %t2595, %t2596
  store i64 %t2597, ptr %t2576
  %t2598 = load i64, ptr %t2576
  %t2599 = add i64 70, 0
  %t2600 = and i64 %t2598, %t2599
  %t2601 = add i64 68, 0
  %t2602 = and i64 %t2600, %t2601
  %t2603 = add i64 65535, 0
  %t2604 = and i64 %t2602, %t2603
  store i64 %t2604, ptr %t2576
  %t2605 = load i64, ptr %t2576
  %t2606 = add i64 12, 0
  %t2607 = mul i64 %t2605, %t2606
  %t2608 = add i64 13, 0
  %t2609 = mul i64 %t2607, %t2608
  %t2610 = add i64 65535, 0
  %t2611 = and i64 %t2609, %t2610
  store i64 %t2611, ptr %t2576
  %t2612 = load i64, ptr %t2576
  %t2613 = add i64 73, 0
  %t2614 = or i64 %t2612, %t2613
  %t2615 = add i64 26, 0
  %t2616 = or i64 %t2614, %t2615
  %t2617 = add i64 65535, 0
  %t2618 = and i64 %t2616, %t2617
  store i64 %t2618, ptr %t2576
  %t2619 = load i64, ptr %t2576
  %t2620 = add i64 34, 0
  %t2621 = add i64 %t2619, %t2620
  %t2622 = add i64 40, 0
  %t2623 = add i64 %t2621, %t2622
  %t2624 = add i64 65535, 0
  %t2625 = and i64 %t2623, %t2624
  store i64 %t2625, ptr %t2576
  %t2626 = load i64, ptr %t2576
  %t2627 = call %NxVal @nx_int(i64 %t2626)
  call void @nx_memo_put(i64 31, ptr %args, i64 %nargs, %NxVal %t2627)
  ret %NxVal %t2627
}
define %NxVal @nx__f_3____main____f37(%NxVal* %args, i64 %nargs) {
entry:
  %t2628 = alloca %NxVal
  %t2632 = alloca i64
  %t2636 = alloca i64
  %t2647 = alloca i64
  store %NxVal zeroinitializer, ptr %t2628
  %t2629 = call i1 @nx_memo_get(i64 32, ptr %args, i64 %nargs, ptr %t2628)
  br i1 %t2629, label %mhit75, label %mmiss76
mhit75:
  %t2630 = load %NxVal, ptr %t2628
  %t2631 = call %NxVal @nx_clone(%NxVal %t2630)
  ret %NxVal %t2631
mmiss76:
  %t2633 = getelementptr %NxVal, ptr %args, i64 0
  %t2634 = load %NxVal, ptr %t2633
  %t2635 = extractvalue %NxVal %t2634, 1
  store i64 %t2635, ptr %t2632
  %t2637 = getelementptr %NxVal, ptr %args, i64 1
  %t2638 = load %NxVal, ptr %t2637
  %t2639 = extractvalue %NxVal %t2638, 1
  store i64 %t2639, ptr %t2636
  %t2640 = load i64, ptr %t2632
  %t2641 = load i64, ptr %t2636
  %t2642 = add i64 %t2640, %t2641
  %t2643 = add i64 37, 0
  %t2644 = add i64 %t2642, %t2643
  %t2645 = add i64 65535, 0
  %t2646 = and i64 %t2644, %t2645
  store i64 %t2646, ptr %t2647
  %t2648 = load i64, ptr %t2647
  %t2649 = add i64 10, 0
  %t2650 = or i64 %t2648, %t2649
  %t2651 = add i64 27, 0
  %t2652 = or i64 %t2650, %t2651
  %t2653 = add i64 65535, 0
  %t2654 = and i64 %t2652, %t2653
  store i64 %t2654, ptr %t2647
  %t2655 = load i64, ptr %t2647
  %t2656 = add i64 84, 0
  %t2657 = sub i64 %t2655, %t2656
  %t2658 = add i64 15, 0
  %t2659 = sub i64 %t2657, %t2658
  %t2660 = add i64 65535, 0
  %t2661 = and i64 %t2659, %t2660
  store i64 %t2661, ptr %t2647
  %t2662 = load i64, ptr %t2647
  %t2663 = add i64 45, 0
  %t2664 = add i64 %t2662, %t2663
  %t2665 = add i64 84, 0
  %t2666 = add i64 %t2664, %t2665
  %t2667 = add i64 65535, 0
  %t2668 = and i64 %t2666, %t2667
  store i64 %t2668, ptr %t2647
  %t2669 = load i64, ptr %t2647
  %t2670 = add i64 55, 0
  %t2671 = xor i64 %t2669, %t2670
  %t2672 = add i64 52, 0
  %t2673 = xor i64 %t2671, %t2672
  %t2674 = add i64 65535, 0
  %t2675 = and i64 %t2673, %t2674
  store i64 %t2675, ptr %t2647
  %t2676 = load i64, ptr %t2647
  %t2677 = add i64 85, 0
  %t2678 = call i64 @nx_mod_i64(i64 %t2676, i64 %t2677)
  %t2679 = add i64 65, 0
  %t2680 = call i64 @nx_mod_i64(i64 %t2678, i64 %t2679)
  %t2681 = add i64 65535, 0
  %t2682 = and i64 %t2680, %t2681
  store i64 %t2682, ptr %t2647
  %t2683 = load i64, ptr %t2647
  %t2684 = add i64 90, 0
  %t2685 = sub i64 %t2683, %t2684
  %t2686 = add i64 33, 0
  %t2687 = sub i64 %t2685, %t2686
  %t2688 = add i64 65535, 0
  %t2689 = and i64 %t2687, %t2688
  store i64 %t2689, ptr %t2647
  %t2690 = load i64, ptr %t2647
  %t2691 = add i64 46, 0
  %t2692 = or i64 %t2690, %t2691
  %t2693 = add i64 65, 0
  %t2694 = or i64 %t2692, %t2693
  %t2695 = add i64 65535, 0
  %t2696 = and i64 %t2694, %t2695
  store i64 %t2696, ptr %t2647
  %t2697 = load i64, ptr %t2647
  %t2698 = call %NxVal @nx_int(i64 %t2697)
  call void @nx_memo_put(i64 32, ptr %args, i64 %nargs, %NxVal %t2698)
  ret %NxVal %t2698
}
define %NxVal @nx__f_3____main____f38(%NxVal* %args, i64 %nargs) {
entry:
  %t2699 = alloca %NxVal
  %t2703 = alloca i64
  %t2707 = alloca i64
  %t2718 = alloca i64
  store %NxVal zeroinitializer, ptr %t2699
  %t2700 = call i1 @nx_memo_get(i64 33, ptr %args, i64 %nargs, ptr %t2699)
  br i1 %t2700, label %mhit77, label %mmiss78
mhit77:
  %t2701 = load %NxVal, ptr %t2699
  %t2702 = call %NxVal @nx_clone(%NxVal %t2701)
  ret %NxVal %t2702
mmiss78:
  %t2704 = getelementptr %NxVal, ptr %args, i64 0
  %t2705 = load %NxVal, ptr %t2704
  %t2706 = extractvalue %NxVal %t2705, 1
  store i64 %t2706, ptr %t2703
  %t2708 = getelementptr %NxVal, ptr %args, i64 1
  %t2709 = load %NxVal, ptr %t2708
  %t2710 = extractvalue %NxVal %t2709, 1
  store i64 %t2710, ptr %t2707
  %t2711 = load i64, ptr %t2703
  %t2712 = load i64, ptr %t2707
  %t2713 = add i64 %t2711, %t2712
  %t2714 = add i64 38, 0
  %t2715 = add i64 %t2713, %t2714
  %t2716 = add i64 65535, 0
  %t2717 = and i64 %t2715, %t2716
  store i64 %t2717, ptr %t2718
  %t2719 = load i64, ptr %t2718
  %t2720 = add i64 73, 0
  %t2721 = or i64 %t2719, %t2720
  %t2722 = add i64 16, 0
  %t2723 = or i64 %t2721, %t2722
  %t2724 = add i64 65535, 0
  %t2725 = and i64 %t2723, %t2724
  store i64 %t2725, ptr %t2718
  %t2726 = load i64, ptr %t2718
  %t2727 = add i64 41, 0
  %t2728 = call i64 @nx_mod_i64(i64 %t2726, i64 %t2727)
  %t2729 = add i64 65, 0
  %t2730 = call i64 @nx_mod_i64(i64 %t2728, i64 %t2729)
  %t2731 = add i64 65535, 0
  %t2732 = and i64 %t2730, %t2731
  store i64 %t2732, ptr %t2718
  %t2733 = load i64, ptr %t2718
  %t2734 = add i64 91, 0
  %t2735 = mul i64 %t2733, %t2734
  %t2736 = add i64 69, 0
  %t2737 = mul i64 %t2735, %t2736
  %t2738 = add i64 65535, 0
  %t2739 = and i64 %t2737, %t2738
  store i64 %t2739, ptr %t2718
  %t2740 = load i64, ptr %t2718
  %t2741 = add i64 93, 0
  %t2742 = and i64 %t2740, %t2741
  %t2743 = add i64 82, 0
  %t2744 = and i64 %t2742, %t2743
  %t2745 = add i64 65535, 0
  %t2746 = and i64 %t2744, %t2745
  store i64 %t2746, ptr %t2718
  %t2747 = load i64, ptr %t2718
  %t2748 = add i64 43, 0
  %t2749 = mul i64 %t2747, %t2748
  %t2750 = add i64 48, 0
  %t2751 = mul i64 %t2749, %t2750
  %t2752 = add i64 65535, 0
  %t2753 = and i64 %t2751, %t2752
  store i64 %t2753, ptr %t2718
  %t2754 = load i64, ptr %t2718
  %t2755 = add i64 50, 0
  %t2756 = mul i64 %t2754, %t2755
  %t2757 = add i64 28, 0
  %t2758 = mul i64 %t2756, %t2757
  %t2759 = add i64 65535, 0
  %t2760 = and i64 %t2758, %t2759
  store i64 %t2760, ptr %t2718
  %t2761 = load i64, ptr %t2718
  %t2762 = add i64 50, 0
  %t2763 = sub i64 %t2761, %t2762
  %t2764 = add i64 79, 0
  %t2765 = sub i64 %t2763, %t2764
  %t2766 = add i64 65535, 0
  %t2767 = and i64 %t2765, %t2766
  store i64 %t2767, ptr %t2718
  %t2768 = load i64, ptr %t2718
  %t2769 = call %NxVal @nx_int(i64 %t2768)
  call void @nx_memo_put(i64 33, ptr %args, i64 %nargs, %NxVal %t2769)
  ret %NxVal %t2769
}
define %NxVal @nx__f_3____main____f39(%NxVal* %args, i64 %nargs) {
entry:
  %t2770 = alloca %NxVal
  %t2774 = alloca i64
  %t2778 = alloca i64
  %t2789 = alloca i64
  store %NxVal zeroinitializer, ptr %t2770
  %t2771 = call i1 @nx_memo_get(i64 34, ptr %args, i64 %nargs, ptr %t2770)
  br i1 %t2771, label %mhit79, label %mmiss80
mhit79:
  %t2772 = load %NxVal, ptr %t2770
  %t2773 = call %NxVal @nx_clone(%NxVal %t2772)
  ret %NxVal %t2773
mmiss80:
  %t2775 = getelementptr %NxVal, ptr %args, i64 0
  %t2776 = load %NxVal, ptr %t2775
  %t2777 = extractvalue %NxVal %t2776, 1
  store i64 %t2777, ptr %t2774
  %t2779 = getelementptr %NxVal, ptr %args, i64 1
  %t2780 = load %NxVal, ptr %t2779
  %t2781 = extractvalue %NxVal %t2780, 1
  store i64 %t2781, ptr %t2778
  %t2782 = load i64, ptr %t2774
  %t2783 = load i64, ptr %t2778
  %t2784 = add i64 %t2782, %t2783
  %t2785 = add i64 39, 0
  %t2786 = add i64 %t2784, %t2785
  %t2787 = add i64 65535, 0
  %t2788 = and i64 %t2786, %t2787
  store i64 %t2788, ptr %t2789
  %t2790 = load i64, ptr %t2789
  %t2791 = add i64 29, 0
  %t2792 = mul i64 %t2790, %t2791
  %t2793 = add i64 28, 0
  %t2794 = mul i64 %t2792, %t2793
  %t2795 = add i64 65535, 0
  %t2796 = and i64 %t2794, %t2795
  store i64 %t2796, ptr %t2789
  %t2797 = load i64, ptr %t2789
  %t2798 = add i64 43, 0
  %t2799 = sub i64 %t2797, %t2798
  %t2800 = add i64 83, 0
  %t2801 = sub i64 %t2799, %t2800
  %t2802 = add i64 65535, 0
  %t2803 = and i64 %t2801, %t2802
  store i64 %t2803, ptr %t2789
  %t2804 = load i64, ptr %t2789
  %t2805 = add i64 29, 0
  %t2806 = xor i64 %t2804, %t2805
  %t2807 = add i64 12, 0
  %t2808 = xor i64 %t2806, %t2807
  %t2809 = add i64 65535, 0
  %t2810 = and i64 %t2808, %t2809
  store i64 %t2810, ptr %t2789
  %t2811 = load i64, ptr %t2789
  %t2812 = add i64 51, 0
  %t2813 = or i64 %t2811, %t2812
  %t2814 = add i64 32, 0
  %t2815 = or i64 %t2813, %t2814
  %t2816 = add i64 65535, 0
  %t2817 = and i64 %t2815, %t2816
  store i64 %t2817, ptr %t2789
  %t2818 = load i64, ptr %t2789
  %t2819 = add i64 84, 0
  %t2820 = xor i64 %t2818, %t2819
  %t2821 = add i64 4, 0
  %t2822 = xor i64 %t2820, %t2821
  %t2823 = add i64 65535, 0
  %t2824 = and i64 %t2822, %t2823
  store i64 %t2824, ptr %t2789
  %t2825 = load i64, ptr %t2789
  %t2826 = add i64 17, 0
  %t2827 = xor i64 %t2825, %t2826
  %t2828 = add i64 41, 0
  %t2829 = xor i64 %t2827, %t2828
  %t2830 = add i64 65535, 0
  %t2831 = and i64 %t2829, %t2830
  store i64 %t2831, ptr %t2789
  %t2832 = load i64, ptr %t2789
  %t2833 = add i64 64, 0
  %t2834 = or i64 %t2832, %t2833
  %t2835 = add i64 66, 0
  %t2836 = or i64 %t2834, %t2835
  %t2837 = add i64 65535, 0
  %t2838 = and i64 %t2836, %t2837
  store i64 %t2838, ptr %t2789
  %t2839 = load i64, ptr %t2789
  %t2840 = call %NxVal @nx_int(i64 %t2839)
  call void @nx_memo_put(i64 34, ptr %args, i64 %nargs, %NxVal %t2840)
  ret %NxVal %t2840
}
define %NxVal @nx__f_3____main____f40(%NxVal* %args, i64 %nargs) {
entry:
  %t2841 = alloca %NxVal
  %t2845 = alloca i64
  %t2849 = alloca i64
  %t2860 = alloca i64
  store %NxVal zeroinitializer, ptr %t2841
  %t2842 = call i1 @nx_memo_get(i64 36, ptr %args, i64 %nargs, ptr %t2841)
  br i1 %t2842, label %mhit81, label %mmiss82
mhit81:
  %t2843 = load %NxVal, ptr %t2841
  %t2844 = call %NxVal @nx_clone(%NxVal %t2843)
  ret %NxVal %t2844
mmiss82:
  %t2846 = getelementptr %NxVal, ptr %args, i64 0
  %t2847 = load %NxVal, ptr %t2846
  %t2848 = extractvalue %NxVal %t2847, 1
  store i64 %t2848, ptr %t2845
  %t2850 = getelementptr %NxVal, ptr %args, i64 1
  %t2851 = load %NxVal, ptr %t2850
  %t2852 = extractvalue %NxVal %t2851, 1
  store i64 %t2852, ptr %t2849
  %t2853 = load i64, ptr %t2845
  %t2854 = load i64, ptr %t2849
  %t2855 = add i64 %t2853, %t2854
  %t2856 = add i64 40, 0
  %t2857 = add i64 %t2855, %t2856
  %t2858 = add i64 65535, 0
  %t2859 = and i64 %t2857, %t2858
  store i64 %t2859, ptr %t2860
  %t2861 = load i64, ptr %t2860
  %t2862 = add i64 53, 0
  %t2863 = or i64 %t2861, %t2862
  %t2864 = add i64 33, 0
  %t2865 = or i64 %t2863, %t2864
  %t2866 = add i64 65535, 0
  %t2867 = and i64 %t2865, %t2866
  store i64 %t2867, ptr %t2860
  %t2868 = load i64, ptr %t2860
  %t2869 = add i64 89, 0
  %t2870 = call i64 @nx_mod_i64(i64 %t2868, i64 %t2869)
  %t2871 = add i64 70, 0
  %t2872 = call i64 @nx_mod_i64(i64 %t2870, i64 %t2871)
  %t2873 = add i64 65535, 0
  %t2874 = and i64 %t2872, %t2873
  store i64 %t2874, ptr %t2860
  %t2875 = load i64, ptr %t2860
  %t2876 = add i64 44, 0
  %t2877 = call i64 @nx_mod_i64(i64 %t2875, i64 %t2876)
  %t2878 = add i64 1, 0
  %t2879 = call i64 @nx_mod_i64(i64 %t2877, i64 %t2878)
  %t2880 = add i64 65535, 0
  %t2881 = and i64 %t2879, %t2880
  store i64 %t2881, ptr %t2860
  %t2882 = load i64, ptr %t2860
  %t2883 = add i64 17, 0
  %t2884 = xor i64 %t2882, %t2883
  %t2885 = add i64 35, 0
  %t2886 = xor i64 %t2884, %t2885
  %t2887 = add i64 65535, 0
  %t2888 = and i64 %t2886, %t2887
  store i64 %t2888, ptr %t2860
  %t2889 = load i64, ptr %t2860
  %t2890 = add i64 62, 0
  %t2891 = and i64 %t2889, %t2890
  %t2892 = add i64 77, 0
  %t2893 = and i64 %t2891, %t2892
  %t2894 = add i64 65535, 0
  %t2895 = and i64 %t2893, %t2894
  store i64 %t2895, ptr %t2860
  %t2896 = load i64, ptr %t2860
  %t2897 = add i64 11, 0
  %t2898 = xor i64 %t2896, %t2897
  %t2899 = add i64 76, 0
  %t2900 = xor i64 %t2898, %t2899
  %t2901 = add i64 65535, 0
  %t2902 = and i64 %t2900, %t2901
  store i64 %t2902, ptr %t2860
  %t2903 = load i64, ptr %t2860
  %t2904 = add i64 82, 0
  %t2905 = sub i64 %t2903, %t2904
  %t2906 = add i64 21, 0
  %t2907 = sub i64 %t2905, %t2906
  %t2908 = add i64 65535, 0
  %t2909 = and i64 %t2907, %t2908
  store i64 %t2909, ptr %t2860
  %t2910 = load i64, ptr %t2860
  %t2911 = call %NxVal @nx_int(i64 %t2910)
  call void @nx_memo_put(i64 36, ptr %args, i64 %nargs, %NxVal %t2911)
  ret %NxVal %t2911
}
define %NxVal @nx__f_3____main____f41(%NxVal* %args, i64 %nargs) {
entry:
  %t2912 = alloca %NxVal
  %t2916 = alloca i64
  %t2920 = alloca i64
  %t2931 = alloca i64
  store %NxVal zeroinitializer, ptr %t2912
  %t2913 = call i1 @nx_memo_get(i64 37, ptr %args, i64 %nargs, ptr %t2912)
  br i1 %t2913, label %mhit83, label %mmiss84
mhit83:
  %t2914 = load %NxVal, ptr %t2912
  %t2915 = call %NxVal @nx_clone(%NxVal %t2914)
  ret %NxVal %t2915
mmiss84:
  %t2917 = getelementptr %NxVal, ptr %args, i64 0
  %t2918 = load %NxVal, ptr %t2917
  %t2919 = extractvalue %NxVal %t2918, 1
  store i64 %t2919, ptr %t2916
  %t2921 = getelementptr %NxVal, ptr %args, i64 1
  %t2922 = load %NxVal, ptr %t2921
  %t2923 = extractvalue %NxVal %t2922, 1
  store i64 %t2923, ptr %t2920
  %t2924 = load i64, ptr %t2916
  %t2925 = load i64, ptr %t2920
  %t2926 = add i64 %t2924, %t2925
  %t2927 = add i64 41, 0
  %t2928 = add i64 %t2926, %t2927
  %t2929 = add i64 65535, 0
  %t2930 = and i64 %t2928, %t2929
  store i64 %t2930, ptr %t2931
  %t2932 = load i64, ptr %t2931
  %t2933 = add i64 88, 0
  %t2934 = sub i64 %t2932, %t2933
  %t2935 = add i64 17, 0
  %t2936 = sub i64 %t2934, %t2935
  %t2937 = add i64 65535, 0
  %t2938 = and i64 %t2936, %t2937
  store i64 %t2938, ptr %t2931
  %t2939 = load i64, ptr %t2931
  %t2940 = add i64 83, 0
  %t2941 = add i64 %t2939, %t2940
  %t2942 = add i64 18, 0
  %t2943 = add i64 %t2941, %t2942
  %t2944 = add i64 65535, 0
  %t2945 = and i64 %t2943, %t2944
  store i64 %t2945, ptr %t2931
  %t2946 = load i64, ptr %t2931
  %t2947 = add i64 35, 0
  %t2948 = call i64 @nx_mod_i64(i64 %t2946, i64 %t2947)
  %t2949 = add i64 3, 0
  %t2950 = call i64 @nx_mod_i64(i64 %t2948, i64 %t2949)
  %t2951 = add i64 65535, 0
  %t2952 = and i64 %t2950, %t2951
  store i64 %t2952, ptr %t2931
  %t2953 = load i64, ptr %t2931
  %t2954 = add i64 27, 0
  %t2955 = xor i64 %t2953, %t2954
  %t2956 = add i64 17, 0
  %t2957 = xor i64 %t2955, %t2956
  %t2958 = add i64 65535, 0
  %t2959 = and i64 %t2957, %t2958
  store i64 %t2959, ptr %t2931
  %t2960 = load i64, ptr %t2931
  %t2961 = add i64 6, 0
  %t2962 = add i64 %t2960, %t2961
  %t2963 = add i64 34, 0
  %t2964 = add i64 %t2962, %t2963
  %t2965 = add i64 65535, 0
  %t2966 = and i64 %t2964, %t2965
  store i64 %t2966, ptr %t2931
  %t2967 = load i64, ptr %t2931
  %t2968 = add i64 95, 0
  %t2969 = mul i64 %t2967, %t2968
  %t2970 = add i64 4, 0
  %t2971 = mul i64 %t2969, %t2970
  %t2972 = add i64 65535, 0
  %t2973 = and i64 %t2971, %t2972
  store i64 %t2973, ptr %t2931
  %t2974 = load i64, ptr %t2931
  %t2975 = add i64 31, 0
  %t2976 = xor i64 %t2974, %t2975
  %t2977 = add i64 69, 0
  %t2978 = xor i64 %t2976, %t2977
  %t2979 = add i64 65535, 0
  %t2980 = and i64 %t2978, %t2979
  store i64 %t2980, ptr %t2931
  %t2981 = load i64, ptr %t2931
  %t2982 = call %NxVal @nx_int(i64 %t2981)
  call void @nx_memo_put(i64 37, ptr %args, i64 %nargs, %NxVal %t2982)
  ret %NxVal %t2982
}
define %NxVal @nx__f_3____main____f42(%NxVal* %args, i64 %nargs) {
entry:
  %t2983 = alloca %NxVal
  %t2987 = alloca i64
  %t2991 = alloca i64
  %t3002 = alloca i64
  store %NxVal zeroinitializer, ptr %t2983
  %t2984 = call i1 @nx_memo_get(i64 38, ptr %args, i64 %nargs, ptr %t2983)
  br i1 %t2984, label %mhit85, label %mmiss86
mhit85:
  %t2985 = load %NxVal, ptr %t2983
  %t2986 = call %NxVal @nx_clone(%NxVal %t2985)
  ret %NxVal %t2986
mmiss86:
  %t2988 = getelementptr %NxVal, ptr %args, i64 0
  %t2989 = load %NxVal, ptr %t2988
  %t2990 = extractvalue %NxVal %t2989, 1
  store i64 %t2990, ptr %t2987
  %t2992 = getelementptr %NxVal, ptr %args, i64 1
  %t2993 = load %NxVal, ptr %t2992
  %t2994 = extractvalue %NxVal %t2993, 1
  store i64 %t2994, ptr %t2991
  %t2995 = load i64, ptr %t2987
  %t2996 = load i64, ptr %t2991
  %t2997 = add i64 %t2995, %t2996
  %t2998 = add i64 42, 0
  %t2999 = add i64 %t2997, %t2998
  %t3000 = add i64 65535, 0
  %t3001 = and i64 %t2999, %t3000
  store i64 %t3001, ptr %t3002
  %t3003 = load i64, ptr %t3002
  %t3004 = add i64 55, 0
  %t3005 = and i64 %t3003, %t3004
  %t3006 = add i64 40, 0
  %t3007 = and i64 %t3005, %t3006
  %t3008 = add i64 65535, 0
  %t3009 = and i64 %t3007, %t3008
  store i64 %t3009, ptr %t3002
  %t3010 = load i64, ptr %t3002
  %t3011 = add i64 46, 0
  %t3012 = call i64 @nx_mod_i64(i64 %t3010, i64 %t3011)
  %t3013 = add i64 78, 0
  %t3014 = call i64 @nx_mod_i64(i64 %t3012, i64 %t3013)
  %t3015 = add i64 65535, 0
  %t3016 = and i64 %t3014, %t3015
  store i64 %t3016, ptr %t3002
  %t3017 = load i64, ptr %t3002
  %t3018 = add i64 69, 0
  %t3019 = mul i64 %t3017, %t3018
  %t3020 = add i64 75, 0
  %t3021 = mul i64 %t3019, %t3020
  %t3022 = add i64 65535, 0
  %t3023 = and i64 %t3021, %t3022
  store i64 %t3023, ptr %t3002
  %t3024 = load i64, ptr %t3002
  %t3025 = add i64 96, 0
  %t3026 = mul i64 %t3024, %t3025
  %t3027 = add i64 87, 0
  %t3028 = mul i64 %t3026, %t3027
  %t3029 = add i64 65535, 0
  %t3030 = and i64 %t3028, %t3029
  store i64 %t3030, ptr %t3002
  %t3031 = load i64, ptr %t3002
  %t3032 = add i64 90, 0
  %t3033 = sub i64 %t3031, %t3032
  %t3034 = add i64 85, 0
  %t3035 = sub i64 %t3033, %t3034
  %t3036 = add i64 65535, 0
  %t3037 = and i64 %t3035, %t3036
  store i64 %t3037, ptr %t3002
  %t3038 = load i64, ptr %t3002
  %t3039 = add i64 94, 0
  %t3040 = and i64 %t3038, %t3039
  %t3041 = add i64 83, 0
  %t3042 = and i64 %t3040, %t3041
  %t3043 = add i64 65535, 0
  %t3044 = and i64 %t3042, %t3043
  store i64 %t3044, ptr %t3002
  %t3045 = load i64, ptr %t3002
  %t3046 = add i64 1, 0
  %t3047 = sub i64 %t3045, %t3046
  %t3048 = add i64 10, 0
  %t3049 = sub i64 %t3047, %t3048
  %t3050 = add i64 65535, 0
  %t3051 = and i64 %t3049, %t3050
  store i64 %t3051, ptr %t3002
  %t3052 = load i64, ptr %t3002
  %t3053 = call %NxVal @nx_int(i64 %t3052)
  call void @nx_memo_put(i64 38, ptr %args, i64 %nargs, %NxVal %t3053)
  ret %NxVal %t3053
}
define %NxVal @nx__f_3____main____f43(%NxVal* %args, i64 %nargs) {
entry:
  %t3054 = alloca %NxVal
  %t3058 = alloca i64
  %t3062 = alloca i64
  %t3073 = alloca i64
  store %NxVal zeroinitializer, ptr %t3054
  %t3055 = call i1 @nx_memo_get(i64 39, ptr %args, i64 %nargs, ptr %t3054)
  br i1 %t3055, label %mhit87, label %mmiss88
mhit87:
  %t3056 = load %NxVal, ptr %t3054
  %t3057 = call %NxVal @nx_clone(%NxVal %t3056)
  ret %NxVal %t3057
mmiss88:
  %t3059 = getelementptr %NxVal, ptr %args, i64 0
  %t3060 = load %NxVal, ptr %t3059
  %t3061 = extractvalue %NxVal %t3060, 1
  store i64 %t3061, ptr %t3058
  %t3063 = getelementptr %NxVal, ptr %args, i64 1
  %t3064 = load %NxVal, ptr %t3063
  %t3065 = extractvalue %NxVal %t3064, 1
  store i64 %t3065, ptr %t3062
  %t3066 = load i64, ptr %t3058
  %t3067 = load i64, ptr %t3062
  %t3068 = add i64 %t3066, %t3067
  %t3069 = add i64 43, 0
  %t3070 = add i64 %t3068, %t3069
  %t3071 = add i64 65535, 0
  %t3072 = and i64 %t3070, %t3071
  store i64 %t3072, ptr %t3073
  %t3074 = load i64, ptr %t3073
  %t3075 = add i64 97, 0
  %t3076 = mul i64 %t3074, %t3075
  %t3077 = add i64 42, 0
  %t3078 = mul i64 %t3076, %t3077
  %t3079 = add i64 65535, 0
  %t3080 = and i64 %t3078, %t3079
  store i64 %t3080, ptr %t3073
  %t3081 = load i64, ptr %t3073
  %t3082 = add i64 68, 0
  %t3083 = add i64 %t3081, %t3082
  %t3084 = add i64 86, 0
  %t3085 = add i64 %t3083, %t3084
  %t3086 = add i64 65535, 0
  %t3087 = and i64 %t3085, %t3086
  store i64 %t3087, ptr %t3073
  %t3088 = load i64, ptr %t3073
  %t3089 = add i64 17, 0
  %t3090 = call i64 @nx_mod_i64(i64 %t3088, i64 %t3089)
  %t3091 = add i64 54, 0
  %t3092 = call i64 @nx_mod_i64(i64 %t3090, i64 %t3091)
  %t3093 = add i64 65535, 0
  %t3094 = and i64 %t3092, %t3093
  store i64 %t3094, ptr %t3073
  %t3095 = load i64, ptr %t3073
  %t3096 = add i64 79, 0
  %t3097 = xor i64 %t3095, %t3096
  %t3098 = add i64 53, 0
  %t3099 = xor i64 %t3097, %t3098
  %t3100 = add i64 65535, 0
  %t3101 = and i64 %t3099, %t3100
  store i64 %t3101, ptr %t3073
  %t3102 = load i64, ptr %t3073
  %t3103 = add i64 78, 0
  %t3104 = and i64 %t3102, %t3103
  %t3105 = add i64 40, 0
  %t3106 = and i64 %t3104, %t3105
  %t3107 = add i64 65535, 0
  %t3108 = and i64 %t3106, %t3107
  store i64 %t3108, ptr %t3073
  %t3109 = load i64, ptr %t3073
  %t3110 = add i64 79, 0
  %t3111 = xor i64 %t3109, %t3110
  %t3112 = add i64 8, 0
  %t3113 = xor i64 %t3111, %t3112
  %t3114 = add i64 65535, 0
  %t3115 = and i64 %t3113, %t3114
  store i64 %t3115, ptr %t3073
  %t3116 = load i64, ptr %t3073
  %t3117 = add i64 87, 0
  %t3118 = call i64 @nx_mod_i64(i64 %t3116, i64 %t3117)
  %t3119 = add i64 67, 0
  %t3120 = call i64 @nx_mod_i64(i64 %t3118, i64 %t3119)
  %t3121 = add i64 65535, 0
  %t3122 = and i64 %t3120, %t3121
  store i64 %t3122, ptr %t3073
  %t3123 = load i64, ptr %t3073
  %t3124 = call %NxVal @nx_int(i64 %t3123)
  call void @nx_memo_put(i64 39, ptr %args, i64 %nargs, %NxVal %t3124)
  ret %NxVal %t3124
}
define %NxVal @nx__f_3____main____f44(%NxVal* %args, i64 %nargs) {
entry:
  %t3125 = alloca %NxVal
  %t3129 = alloca i64
  %t3133 = alloca i64
  %t3144 = alloca i64
  store %NxVal zeroinitializer, ptr %t3125
  %t3126 = call i1 @nx_memo_get(i64 40, ptr %args, i64 %nargs, ptr %t3125)
  br i1 %t3126, label %mhit89, label %mmiss90
mhit89:
  %t3127 = load %NxVal, ptr %t3125
  %t3128 = call %NxVal @nx_clone(%NxVal %t3127)
  ret %NxVal %t3128
mmiss90:
  %t3130 = getelementptr %NxVal, ptr %args, i64 0
  %t3131 = load %NxVal, ptr %t3130
  %t3132 = extractvalue %NxVal %t3131, 1
  store i64 %t3132, ptr %t3129
  %t3134 = getelementptr %NxVal, ptr %args, i64 1
  %t3135 = load %NxVal, ptr %t3134
  %t3136 = extractvalue %NxVal %t3135, 1
  store i64 %t3136, ptr %t3133
  %t3137 = load i64, ptr %t3129
  %t3138 = load i64, ptr %t3133
  %t3139 = add i64 %t3137, %t3138
  %t3140 = add i64 44, 0
  %t3141 = add i64 %t3139, %t3140
  %t3142 = add i64 65535, 0
  %t3143 = and i64 %t3141, %t3142
  store i64 %t3143, ptr %t3144
  %t3145 = load i64, ptr %t3144
  %t3146 = add i64 22, 0
  %t3147 = call i64 @nx_mod_i64(i64 %t3145, i64 %t3146)
  %t3148 = add i64 58, 0
  %t3149 = call i64 @nx_mod_i64(i64 %t3147, i64 %t3148)
  %t3150 = add i64 65535, 0
  %t3151 = and i64 %t3149, %t3150
  store i64 %t3151, ptr %t3144
  %t3152 = load i64, ptr %t3144
  %t3153 = add i64 58, 0
  %t3154 = add i64 %t3152, %t3153
  %t3155 = add i64 52, 0
  %t3156 = add i64 %t3154, %t3155
  %t3157 = add i64 65535, 0
  %t3158 = and i64 %t3156, %t3157
  store i64 %t3158, ptr %t3144
  %t3159 = load i64, ptr %t3144
  %t3160 = add i64 26, 0
  %t3161 = or i64 %t3159, %t3160
  %t3162 = add i64 52, 0
  %t3163 = or i64 %t3161, %t3162
  %t3164 = add i64 65535, 0
  %t3165 = and i64 %t3163, %t3164
  store i64 %t3165, ptr %t3144
  %t3166 = load i64, ptr %t3144
  %t3167 = add i64 72, 0
  %t3168 = add i64 %t3166, %t3167
  %t3169 = add i64 24, 0
  %t3170 = add i64 %t3168, %t3169
  %t3171 = add i64 65535, 0
  %t3172 = and i64 %t3170, %t3171
  store i64 %t3172, ptr %t3144
  %t3173 = load i64, ptr %t3144
  %t3174 = add i64 83, 0
  %t3175 = sub i64 %t3173, %t3174
  %t3176 = add i64 66, 0
  %t3177 = sub i64 %t3175, %t3176
  %t3178 = add i64 65535, 0
  %t3179 = and i64 %t3177, %t3178
  store i64 %t3179, ptr %t3144
  %t3180 = load i64, ptr %t3144
  %t3181 = add i64 78, 0
  %t3182 = and i64 %t3180, %t3181
  %t3183 = add i64 33, 0
  %t3184 = and i64 %t3182, %t3183
  %t3185 = add i64 65535, 0
  %t3186 = and i64 %t3184, %t3185
  store i64 %t3186, ptr %t3144
  %t3187 = load i64, ptr %t3144
  %t3188 = add i64 17, 0
  %t3189 = call i64 @nx_mod_i64(i64 %t3187, i64 %t3188)
  %t3190 = add i64 43, 0
  %t3191 = call i64 @nx_mod_i64(i64 %t3189, i64 %t3190)
  %t3192 = add i64 65535, 0
  %t3193 = and i64 %t3191, %t3192
  store i64 %t3193, ptr %t3144
  %t3194 = load i64, ptr %t3144
  %t3195 = call %NxVal @nx_int(i64 %t3194)
  call void @nx_memo_put(i64 40, ptr %args, i64 %nargs, %NxVal %t3195)
  ret %NxVal %t3195
}
define %NxVal @nx__f_3____main____f45(%NxVal* %args, i64 %nargs) {
entry:
  %t3196 = alloca %NxVal
  %t3200 = alloca i64
  %t3204 = alloca i64
  %t3215 = alloca i64
  store %NxVal zeroinitializer, ptr %t3196
  %t3197 = call i1 @nx_memo_get(i64 41, ptr %args, i64 %nargs, ptr %t3196)
  br i1 %t3197, label %mhit91, label %mmiss92
mhit91:
  %t3198 = load %NxVal, ptr %t3196
  %t3199 = call %NxVal @nx_clone(%NxVal %t3198)
  ret %NxVal %t3199
mmiss92:
  %t3201 = getelementptr %NxVal, ptr %args, i64 0
  %t3202 = load %NxVal, ptr %t3201
  %t3203 = extractvalue %NxVal %t3202, 1
  store i64 %t3203, ptr %t3200
  %t3205 = getelementptr %NxVal, ptr %args, i64 1
  %t3206 = load %NxVal, ptr %t3205
  %t3207 = extractvalue %NxVal %t3206, 1
  store i64 %t3207, ptr %t3204
  %t3208 = load i64, ptr %t3200
  %t3209 = load i64, ptr %t3204
  %t3210 = add i64 %t3208, %t3209
  %t3211 = add i64 45, 0
  %t3212 = add i64 %t3210, %t3211
  %t3213 = add i64 65535, 0
  %t3214 = and i64 %t3212, %t3213
  store i64 %t3214, ptr %t3215
  %t3216 = load i64, ptr %t3215
  %t3217 = add i64 62, 0
  %t3218 = mul i64 %t3216, %t3217
  %t3219 = add i64 76, 0
  %t3220 = mul i64 %t3218, %t3219
  %t3221 = add i64 65535, 0
  %t3222 = and i64 %t3220, %t3221
  store i64 %t3222, ptr %t3215
  %t3223 = load i64, ptr %t3215
  %t3224 = add i64 21, 0
  %t3225 = mul i64 %t3223, %t3224
  %t3226 = add i64 69, 0
  %t3227 = mul i64 %t3225, %t3226
  %t3228 = add i64 65535, 0
  %t3229 = and i64 %t3227, %t3228
  store i64 %t3229, ptr %t3215
  %t3230 = load i64, ptr %t3215
  %t3231 = add i64 53, 0
  %t3232 = sub i64 %t3230, %t3231
  %t3233 = add i64 43, 0
  %t3234 = sub i64 %t3232, %t3233
  %t3235 = add i64 65535, 0
  %t3236 = and i64 %t3234, %t3235
  store i64 %t3236, ptr %t3215
  %t3237 = load i64, ptr %t3215
  %t3238 = add i64 3, 0
  %t3239 = xor i64 %t3237, %t3238
  %t3240 = add i64 87, 0
  %t3241 = xor i64 %t3239, %t3240
  %t3242 = add i64 65535, 0
  %t3243 = and i64 %t3241, %t3242
  store i64 %t3243, ptr %t3215
  %t3244 = load i64, ptr %t3215
  %t3245 = add i64 17, 0
  %t3246 = or i64 %t3244, %t3245
  %t3247 = add i64 87, 0
  %t3248 = or i64 %t3246, %t3247
  %t3249 = add i64 65535, 0
  %t3250 = and i64 %t3248, %t3249
  store i64 %t3250, ptr %t3215
  %t3251 = load i64, ptr %t3215
  %t3252 = add i64 41, 0
  %t3253 = add i64 %t3251, %t3252
  %t3254 = add i64 62, 0
  %t3255 = add i64 %t3253, %t3254
  %t3256 = add i64 65535, 0
  %t3257 = and i64 %t3255, %t3256
  store i64 %t3257, ptr %t3215
  %t3258 = load i64, ptr %t3215
  %t3259 = add i64 20, 0
  %t3260 = call i64 @nx_mod_i64(i64 %t3258, i64 %t3259)
  %t3261 = add i64 43, 0
  %t3262 = call i64 @nx_mod_i64(i64 %t3260, i64 %t3261)
  %t3263 = add i64 65535, 0
  %t3264 = and i64 %t3262, %t3263
  store i64 %t3264, ptr %t3215
  %t3265 = load i64, ptr %t3215
  %t3266 = call %NxVal @nx_int(i64 %t3265)
  call void @nx_memo_put(i64 41, ptr %args, i64 %nargs, %NxVal %t3266)
  ret %NxVal %t3266
}
define %NxVal @nx__f_3____main____f46(%NxVal* %args, i64 %nargs) {
entry:
  %t3267 = alloca %NxVal
  %t3271 = alloca i64
  %t3275 = alloca i64
  %t3286 = alloca i64
  store %NxVal zeroinitializer, ptr %t3267
  %t3268 = call i1 @nx_memo_get(i64 42, ptr %args, i64 %nargs, ptr %t3267)
  br i1 %t3268, label %mhit93, label %mmiss94
mhit93:
  %t3269 = load %NxVal, ptr %t3267
  %t3270 = call %NxVal @nx_clone(%NxVal %t3269)
  ret %NxVal %t3270
mmiss94:
  %t3272 = getelementptr %NxVal, ptr %args, i64 0
  %t3273 = load %NxVal, ptr %t3272
  %t3274 = extractvalue %NxVal %t3273, 1
  store i64 %t3274, ptr %t3271
  %t3276 = getelementptr %NxVal, ptr %args, i64 1
  %t3277 = load %NxVal, ptr %t3276
  %t3278 = extractvalue %NxVal %t3277, 1
  store i64 %t3278, ptr %t3275
  %t3279 = load i64, ptr %t3271
  %t3280 = load i64, ptr %t3275
  %t3281 = add i64 %t3279, %t3280
  %t3282 = add i64 46, 0
  %t3283 = add i64 %t3281, %t3282
  %t3284 = add i64 65535, 0
  %t3285 = and i64 %t3283, %t3284
  store i64 %t3285, ptr %t3286
  %t3287 = load i64, ptr %t3286
  %t3288 = add i64 82, 0
  %t3289 = sub i64 %t3287, %t3288
  %t3290 = add i64 24, 0
  %t3291 = sub i64 %t3289, %t3290
  %t3292 = add i64 65535, 0
  %t3293 = and i64 %t3291, %t3292
  store i64 %t3293, ptr %t3286
  %t3294 = load i64, ptr %t3286
  %t3295 = add i64 9, 0
  %t3296 = and i64 %t3294, %t3295
  %t3297 = add i64 5, 0
  %t3298 = and i64 %t3296, %t3297
  %t3299 = add i64 65535, 0
  %t3300 = and i64 %t3298, %t3299
  store i64 %t3300, ptr %t3286
  %t3301 = load i64, ptr %t3286
  %t3302 = add i64 28, 0
  %t3303 = xor i64 %t3301, %t3302
  %t3304 = add i64 42, 0
  %t3305 = xor i64 %t3303, %t3304
  %t3306 = add i64 65535, 0
  %t3307 = and i64 %t3305, %t3306
  store i64 %t3307, ptr %t3286
  %t3308 = load i64, ptr %t3286
  %t3309 = add i64 69, 0
  %t3310 = or i64 %t3308, %t3309
  %t3311 = add i64 41, 0
  %t3312 = or i64 %t3310, %t3311
  %t3313 = add i64 65535, 0
  %t3314 = and i64 %t3312, %t3313
  store i64 %t3314, ptr %t3286
  %t3315 = load i64, ptr %t3286
  %t3316 = add i64 77, 0
  %t3317 = and i64 %t3315, %t3316
  %t3318 = add i64 87, 0
  %t3319 = and i64 %t3317, %t3318
  %t3320 = add i64 65535, 0
  %t3321 = and i64 %t3319, %t3320
  store i64 %t3321, ptr %t3286
  %t3322 = load i64, ptr %t3286
  %t3323 = add i64 5, 0
  %t3324 = mul i64 %t3322, %t3323
  %t3325 = add i64 2, 0
  %t3326 = mul i64 %t3324, %t3325
  %t3327 = add i64 65535, 0
  %t3328 = and i64 %t3326, %t3327
  store i64 %t3328, ptr %t3286
  %t3329 = load i64, ptr %t3286
  %t3330 = add i64 45, 0
  %t3331 = and i64 %t3329, %t3330
  %t3332 = add i64 54, 0
  %t3333 = and i64 %t3331, %t3332
  %t3334 = add i64 65535, 0
  %t3335 = and i64 %t3333, %t3334
  store i64 %t3335, ptr %t3286
  %t3336 = load i64, ptr %t3286
  %t3337 = call %NxVal @nx_int(i64 %t3336)
  call void @nx_memo_put(i64 42, ptr %args, i64 %nargs, %NxVal %t3337)
  ret %NxVal %t3337
}
define %NxVal @nx__f_3____main____f47(%NxVal* %args, i64 %nargs) {
entry:
  %t3338 = alloca %NxVal
  %t3342 = alloca i64
  %t3346 = alloca i64
  %t3357 = alloca i64
  store %NxVal zeroinitializer, ptr %t3338
  %t3339 = call i1 @nx_memo_get(i64 43, ptr %args, i64 %nargs, ptr %t3338)
  br i1 %t3339, label %mhit95, label %mmiss96
mhit95:
  %t3340 = load %NxVal, ptr %t3338
  %t3341 = call %NxVal @nx_clone(%NxVal %t3340)
  ret %NxVal %t3341
mmiss96:
  %t3343 = getelementptr %NxVal, ptr %args, i64 0
  %t3344 = load %NxVal, ptr %t3343
  %t3345 = extractvalue %NxVal %t3344, 1
  store i64 %t3345, ptr %t3342
  %t3347 = getelementptr %NxVal, ptr %args, i64 1
  %t3348 = load %NxVal, ptr %t3347
  %t3349 = extractvalue %NxVal %t3348, 1
  store i64 %t3349, ptr %t3346
  %t3350 = load i64, ptr %t3342
  %t3351 = load i64, ptr %t3346
  %t3352 = add i64 %t3350, %t3351
  %t3353 = add i64 47, 0
  %t3354 = add i64 %t3352, %t3353
  %t3355 = add i64 65535, 0
  %t3356 = and i64 %t3354, %t3355
  store i64 %t3356, ptr %t3357
  %t3358 = load i64, ptr %t3357
  %t3359 = add i64 57, 0
  %t3360 = or i64 %t3358, %t3359
  %t3361 = add i64 74, 0
  %t3362 = or i64 %t3360, %t3361
  %t3363 = add i64 65535, 0
  %t3364 = and i64 %t3362, %t3363
  store i64 %t3364, ptr %t3357
  %t3365 = load i64, ptr %t3357
  %t3366 = add i64 5, 0
  %t3367 = and i64 %t3365, %t3366
  %t3368 = add i64 17, 0
  %t3369 = and i64 %t3367, %t3368
  %t3370 = add i64 65535, 0
  %t3371 = and i64 %t3369, %t3370
  store i64 %t3371, ptr %t3357
  %t3372 = load i64, ptr %t3357
  %t3373 = add i64 46, 0
  %t3374 = and i64 %t3372, %t3373
  %t3375 = add i64 65, 0
  %t3376 = and i64 %t3374, %t3375
  %t3377 = add i64 65535, 0
  %t3378 = and i64 %t3376, %t3377
  store i64 %t3378, ptr %t3357
  %t3379 = load i64, ptr %t3357
  %t3380 = add i64 81, 0
  %t3381 = call i64 @nx_mod_i64(i64 %t3379, i64 %t3380)
  %t3382 = add i64 31, 0
  %t3383 = call i64 @nx_mod_i64(i64 %t3381, i64 %t3382)
  %t3384 = add i64 65535, 0
  %t3385 = and i64 %t3383, %t3384
  store i64 %t3385, ptr %t3357
  %t3386 = load i64, ptr %t3357
  %t3387 = add i64 79, 0
  %t3388 = and i64 %t3386, %t3387
  %t3389 = add i64 87, 0
  %t3390 = and i64 %t3388, %t3389
  %t3391 = add i64 65535, 0
  %t3392 = and i64 %t3390, %t3391
  store i64 %t3392, ptr %t3357
  %t3393 = load i64, ptr %t3357
  %t3394 = add i64 35, 0
  %t3395 = sub i64 %t3393, %t3394
  %t3396 = add i64 81, 0
  %t3397 = sub i64 %t3395, %t3396
  %t3398 = add i64 65535, 0
  %t3399 = and i64 %t3397, %t3398
  store i64 %t3399, ptr %t3357
  %t3400 = load i64, ptr %t3357
  %t3401 = add i64 12, 0
  %t3402 = and i64 %t3400, %t3401
  %t3403 = add i64 50, 0
  %t3404 = and i64 %t3402, %t3403
  %t3405 = add i64 65535, 0
  %t3406 = and i64 %t3404, %t3405
  store i64 %t3406, ptr %t3357
  %t3407 = load i64, ptr %t3357
  %t3408 = call %NxVal @nx_int(i64 %t3407)
  call void @nx_memo_put(i64 43, ptr %args, i64 %nargs, %NxVal %t3408)
  ret %NxVal %t3408
}
define %NxVal @nx__f_3____main____f48(%NxVal* %args, i64 %nargs) {
entry:
  %t3409 = alloca %NxVal
  %t3413 = alloca i64
  %t3417 = alloca i64
  %t3428 = alloca i64
  store %NxVal zeroinitializer, ptr %t3409
  %t3410 = call i1 @nx_memo_get(i64 44, ptr %args, i64 %nargs, ptr %t3409)
  br i1 %t3410, label %mhit97, label %mmiss98
mhit97:
  %t3411 = load %NxVal, ptr %t3409
  %t3412 = call %NxVal @nx_clone(%NxVal %t3411)
  ret %NxVal %t3412
mmiss98:
  %t3414 = getelementptr %NxVal, ptr %args, i64 0
  %t3415 = load %NxVal, ptr %t3414
  %t3416 = extractvalue %NxVal %t3415, 1
  store i64 %t3416, ptr %t3413
  %t3418 = getelementptr %NxVal, ptr %args, i64 1
  %t3419 = load %NxVal, ptr %t3418
  %t3420 = extractvalue %NxVal %t3419, 1
  store i64 %t3420, ptr %t3417
  %t3421 = load i64, ptr %t3413
  %t3422 = load i64, ptr %t3417
  %t3423 = add i64 %t3421, %t3422
  %t3424 = add i64 48, 0
  %t3425 = add i64 %t3423, %t3424
  %t3426 = add i64 65535, 0
  %t3427 = and i64 %t3425, %t3426
  store i64 %t3427, ptr %t3428
  %t3429 = load i64, ptr %t3428
  %t3430 = add i64 52, 0
  %t3431 = sub i64 %t3429, %t3430
  %t3432 = add i64 6, 0
  %t3433 = sub i64 %t3431, %t3432
  %t3434 = add i64 65535, 0
  %t3435 = and i64 %t3433, %t3434
  store i64 %t3435, ptr %t3428
  %t3436 = load i64, ptr %t3428
  %t3437 = add i64 59, 0
  %t3438 = call i64 @nx_mod_i64(i64 %t3436, i64 %t3437)
  %t3439 = add i64 79, 0
  %t3440 = call i64 @nx_mod_i64(i64 %t3438, i64 %t3439)
  %t3441 = add i64 65535, 0
  %t3442 = and i64 %t3440, %t3441
  store i64 %t3442, ptr %t3428
  %t3443 = load i64, ptr %t3428
  %t3444 = add i64 55, 0
  %t3445 = mul i64 %t3443, %t3444
  %t3446 = add i64 8, 0
  %t3447 = mul i64 %t3445, %t3446
  %t3448 = add i64 65535, 0
  %t3449 = and i64 %t3447, %t3448
  store i64 %t3449, ptr %t3428
  %t3450 = load i64, ptr %t3428
  %t3451 = add i64 49, 0
  %t3452 = sub i64 %t3450, %t3451
  %t3453 = add i64 85, 0
  %t3454 = sub i64 %t3452, %t3453
  %t3455 = add i64 65535, 0
  %t3456 = and i64 %t3454, %t3455
  store i64 %t3456, ptr %t3428
  %t3457 = load i64, ptr %t3428
  %t3458 = add i64 84, 0
  %t3459 = mul i64 %t3457, %t3458
  %t3460 = add i64 37, 0
  %t3461 = mul i64 %t3459, %t3460
  %t3462 = add i64 65535, 0
  %t3463 = and i64 %t3461, %t3462
  store i64 %t3463, ptr %t3428
  %t3464 = load i64, ptr %t3428
  %t3465 = add i64 24, 0
  %t3466 = xor i64 %t3464, %t3465
  %t3467 = add i64 42, 0
  %t3468 = xor i64 %t3466, %t3467
  %t3469 = add i64 65535, 0
  %t3470 = and i64 %t3468, %t3469
  store i64 %t3470, ptr %t3428
  %t3471 = load i64, ptr %t3428
  %t3472 = add i64 20, 0
  %t3473 = xor i64 %t3471, %t3472
  %t3474 = add i64 44, 0
  %t3475 = xor i64 %t3473, %t3474
  %t3476 = add i64 65535, 0
  %t3477 = and i64 %t3475, %t3476
  store i64 %t3477, ptr %t3428
  %t3478 = load i64, ptr %t3428
  %t3479 = call %NxVal @nx_int(i64 %t3478)
  call void @nx_memo_put(i64 44, ptr %args, i64 %nargs, %NxVal %t3479)
  ret %NxVal %t3479
}
define %NxVal @nx__f_3____main____f49(%NxVal* %args, i64 %nargs) {
entry:
  %t3480 = alloca %NxVal
  %t3484 = alloca i64
  %t3488 = alloca i64
  %t3499 = alloca i64
  store %NxVal zeroinitializer, ptr %t3480
  %t3481 = call i1 @nx_memo_get(i64 45, ptr %args, i64 %nargs, ptr %t3480)
  br i1 %t3481, label %mhit99, label %mmiss100
mhit99:
  %t3482 = load %NxVal, ptr %t3480
  %t3483 = call %NxVal @nx_clone(%NxVal %t3482)
  ret %NxVal %t3483
mmiss100:
  %t3485 = getelementptr %NxVal, ptr %args, i64 0
  %t3486 = load %NxVal, ptr %t3485
  %t3487 = extractvalue %NxVal %t3486, 1
  store i64 %t3487, ptr %t3484
  %t3489 = getelementptr %NxVal, ptr %args, i64 1
  %t3490 = load %NxVal, ptr %t3489
  %t3491 = extractvalue %NxVal %t3490, 1
  store i64 %t3491, ptr %t3488
  %t3492 = load i64, ptr %t3484
  %t3493 = load i64, ptr %t3488
  %t3494 = add i64 %t3492, %t3493
  %t3495 = add i64 49, 0
  %t3496 = add i64 %t3494, %t3495
  %t3497 = add i64 65535, 0
  %t3498 = and i64 %t3496, %t3497
  store i64 %t3498, ptr %t3499
  %t3500 = load i64, ptr %t3499
  %t3501 = add i64 51, 0
  %t3502 = or i64 %t3500, %t3501
  %t3503 = add i64 2, 0
  %t3504 = or i64 %t3502, %t3503
  %t3505 = add i64 65535, 0
  %t3506 = and i64 %t3504, %t3505
  store i64 %t3506, ptr %t3499
  %t3507 = load i64, ptr %t3499
  %t3508 = add i64 66, 0
  %t3509 = and i64 %t3507, %t3508
  %t3510 = add i64 16, 0
  %t3511 = and i64 %t3509, %t3510
  %t3512 = add i64 65535, 0
  %t3513 = and i64 %t3511, %t3512
  store i64 %t3513, ptr %t3499
  %t3514 = load i64, ptr %t3499
  %t3515 = add i64 91, 0
  %t3516 = call i64 @nx_mod_i64(i64 %t3514, i64 %t3515)
  %t3517 = add i64 72, 0
  %t3518 = call i64 @nx_mod_i64(i64 %t3516, i64 %t3517)
  %t3519 = add i64 65535, 0
  %t3520 = and i64 %t3518, %t3519
  store i64 %t3520, ptr %t3499
  %t3521 = load i64, ptr %t3499
  %t3522 = add i64 84, 0
  %t3523 = add i64 %t3521, %t3522
  %t3524 = add i64 70, 0
  %t3525 = add i64 %t3523, %t3524
  %t3526 = add i64 65535, 0
  %t3527 = and i64 %t3525, %t3526
  store i64 %t3527, ptr %t3499
  %t3528 = load i64, ptr %t3499
  %t3529 = add i64 94, 0
  %t3530 = and i64 %t3528, %t3529
  %t3531 = add i64 65, 0
  %t3532 = and i64 %t3530, %t3531
  %t3533 = add i64 65535, 0
  %t3534 = and i64 %t3532, %t3533
  store i64 %t3534, ptr %t3499
  %t3535 = load i64, ptr %t3499
  %t3536 = add i64 29, 0
  %t3537 = or i64 %t3535, %t3536
  %t3538 = add i64 28, 0
  %t3539 = or i64 %t3537, %t3538
  %t3540 = add i64 65535, 0
  %t3541 = and i64 %t3539, %t3540
  store i64 %t3541, ptr %t3499
  %t3542 = load i64, ptr %t3499
  %t3543 = add i64 83, 0
  %t3544 = mul i64 %t3542, %t3543
  %t3545 = add i64 2, 0
  %t3546 = mul i64 %t3544, %t3545
  %t3547 = add i64 65535, 0
  %t3548 = and i64 %t3546, %t3547
  store i64 %t3548, ptr %t3499
  %t3549 = load i64, ptr %t3499
  %t3550 = call %NxVal @nx_int(i64 %t3549)
  call void @nx_memo_put(i64 45, ptr %args, i64 %nargs, %NxVal %t3550)
  ret %NxVal %t3550
}
define void @nx__init___main__() {
entry:
  %t3557 = alloca [2 x %NxVal]
  %t3573 = alloca [2 x %NxVal]
  %t3589 = alloca [2 x %NxVal]
  %t3605 = alloca [2 x %NxVal]
  %t3621 = alloca [2 x %NxVal]
  %t3637 = alloca [2 x %NxVal]
  %t3653 = alloca [2 x %NxVal]
  %t3669 = alloca [2 x %NxVal]
  %t3685 = alloca [2 x %NxVal]
  %t3701 = alloca [2 x %NxVal]
  %t3717 = alloca [2 x %NxVal]
  %t3733 = alloca [2 x %NxVal]
  %t3749 = alloca [2 x %NxVal]
  %t3765 = alloca [2 x %NxVal]
  %t3781 = alloca [2 x %NxVal]
  %t3797 = alloca [2 x %NxVal]
  %t3813 = alloca [2 x %NxVal]
  %t3829 = alloca [2 x %NxVal]
  %t3845 = alloca [2 x %NxVal]
  %t3861 = alloca [2 x %NxVal]
  %t3877 = alloca [2 x %NxVal]
  %t3893 = alloca [2 x %NxVal]
  %t3909 = alloca [2 x %NxVal]
  %t3925 = alloca [2 x %NxVal]
  %t3941 = alloca [2 x %NxVal]
  %t3957 = alloca [2 x %NxVal]
  %t3973 = alloca [2 x %NxVal]
  %t3989 = alloca [2 x %NxVal]
  %t4005 = alloca [2 x %NxVal]
  %t4021 = alloca [2 x %NxVal]
  %t4037 = alloca [2 x %NxVal]
  %t4053 = alloca [2 x %NxVal]
  %t4069 = alloca [2 x %NxVal]
  %t4085 = alloca [2 x %NxVal]
  %t4101 = alloca [2 x %NxVal]
  %t4117 = alloca [2 x %NxVal]
  %t4133 = alloca [2 x %NxVal]
  %t4149 = alloca [2 x %NxVal]
  %t4165 = alloca [2 x %NxVal]
  %t4181 = alloca [2 x %NxVal]
  %t4197 = alloca [2 x %NxVal]
  %t4213 = alloca [2 x %NxVal]
  %t4229 = alloca [2 x %NxVal]
  %t4245 = alloca [2 x %NxVal]
  %t4261 = alloca [2 x %NxVal]
  %t4277 = alloca [2 x %NxVal]
  %t4293 = alloca [2 x %NxVal]
  %t4309 = alloca [2 x %NxVal]
  %t4325 = alloca [2 x %NxVal]
  %t4341 = alloca [2 x %NxVal]
  %t4354 = alloca [1 x %NxVal]
  %t3551 = load i1, ptr @nx__done___main__
  br i1 %t3551, label %initskip102, label %initrun101
initrun101:
  store i1 true, ptr @nx__done___main__
  %t3552 = add i64 0, 0
  %t3553 = call %NxVal @nx_int(i64 %t3552)
  store %NxVal %t3553, ptr @nx__g___main____total
  %t3554 = load %NxVal, ptr @nx__g___main____total
  %t3555 = add i64 200, 0
  %t3556 = add i64 383, 0
  %t3558 = call %NxVal @nx_int(i64 %t3555)
  %t3559 = getelementptr [2 x %NxVal], ptr %t3557, i64 0, i64 0
  store %NxVal %t3558, ptr %t3559
  %t3560 = call %NxVal @nx_int(i64 %t3556)
  %t3561 = getelementptr [2 x %NxVal], ptr %t3557, i64 0, i64 1
  store %NxVal %t3560, ptr %t3561
  %t3562 = getelementptr [2 x %NxVal], ptr %t3557, i64 0, i64 0
  %t3563 = call %NxVal @nx__f_2____main____f0(ptr %t3562, i64 2)
  %t3564 = extractvalue %NxVal %t3563, 1
  %t3565 = extractvalue %NxVal %t3554, 1
  %t3566 = add i64 %t3565, %t3564
  %t3567 = add i64 65535, 0
  %t3568 = and i64 %t3566, %t3567
  %t3569 = call %NxVal @nx_int(i64 %t3568)
  store %NxVal %t3569, ptr @nx__g___main____total
  %t3570 = load %NxVal, ptr @nx__g___main____total
  %t3571 = add i64 62, 0
  %t3572 = add i64 349, 0
  %t3574 = call %NxVal @nx_int(i64 %t3571)
  %t3575 = getelementptr [2 x %NxVal], ptr %t3573, i64 0, i64 0
  store %NxVal %t3574, ptr %t3575
  %t3576 = call %NxVal @nx_int(i64 %t3572)
  %t3577 = getelementptr [2 x %NxVal], ptr %t3573, i64 0, i64 1
  store %NxVal %t3576, ptr %t3577
  %t3578 = getelementptr [2 x %NxVal], ptr %t3573, i64 0, i64 0
  %t3579 = call %NxVal @nx__f_2____main____f1(ptr %t3578, i64 2)
  %t3580 = extractvalue %NxVal %t3579, 1
  %t3581 = extractvalue %NxVal %t3570, 1
  %t3582 = add i64 %t3581, %t3580
  %t3583 = add i64 65535, 0
  %t3584 = and i64 %t3582, %t3583
  %t3585 = call %NxVal @nx_int(i64 %t3584)
  store %NxVal %t3585, ptr @nx__g___main____total
  %t3586 = load %NxVal, ptr @nx__g___main____total
  %t3587 = add i64 14, 0
  %t3588 = add i64 477, 0
  %t3590 = call %NxVal @nx_int(i64 %t3587)
  %t3591 = getelementptr [2 x %NxVal], ptr %t3589, i64 0, i64 0
  store %NxVal %t3590, ptr %t3591
  %t3592 = call %NxVal @nx_int(i64 %t3588)
  %t3593 = getelementptr [2 x %NxVal], ptr %t3589, i64 0, i64 1
  store %NxVal %t3592, ptr %t3593
  %t3594 = getelementptr [2 x %NxVal], ptr %t3589, i64 0, i64 0
  %t3595 = call %NxVal @nx__f_2____main____f2(ptr %t3594, i64 2)
  %t3596 = extractvalue %NxVal %t3595, 1
  %t3597 = extractvalue %NxVal %t3586, 1
  %t3598 = add i64 %t3597, %t3596
  %t3599 = add i64 65535, 0
  %t3600 = and i64 %t3598, %t3599
  %t3601 = call %NxVal @nx_int(i64 %t3600)
  store %NxVal %t3601, ptr @nx__g___main____total
  %t3602 = load %NxVal, ptr @nx__g___main____total
  %t3603 = add i64 86, 0
  %t3604 = add i64 142, 0
  %t3606 = call %NxVal @nx_int(i64 %t3603)
  %t3607 = getelementptr [2 x %NxVal], ptr %t3605, i64 0, i64 0
  store %NxVal %t3606, ptr %t3607
  %t3608 = call %NxVal @nx_int(i64 %t3604)
  %t3609 = getelementptr [2 x %NxVal], ptr %t3605, i64 0, i64 1
  store %NxVal %t3608, ptr %t3609
  %t3610 = getelementptr [2 x %NxVal], ptr %t3605, i64 0, i64 0
  %t3611 = call %NxVal @nx__f_2____main____f3(ptr %t3610, i64 2)
  %t3612 = extractvalue %NxVal %t3611, 1
  %t3613 = extractvalue %NxVal %t3602, 1
  %t3614 = add i64 %t3613, %t3612
  %t3615 = add i64 65535, 0
  %t3616 = and i64 %t3614, %t3615
  %t3617 = call %NxVal @nx_int(i64 %t3616)
  store %NxVal %t3617, ptr @nx__g___main____total
  %t3618 = load %NxVal, ptr @nx__g___main____total
  %t3619 = add i64 47, 0
  %t3620 = add i64 76, 0
  %t3622 = call %NxVal @nx_int(i64 %t3619)
  %t3623 = getelementptr [2 x %NxVal], ptr %t3621, i64 0, i64 0
  store %NxVal %t3622, ptr %t3623
  %t3624 = call %NxVal @nx_int(i64 %t3620)
  %t3625 = getelementptr [2 x %NxVal], ptr %t3621, i64 0, i64 1
  store %NxVal %t3624, ptr %t3625
  %t3626 = getelementptr [2 x %NxVal], ptr %t3621, i64 0, i64 0
  %t3627 = call %NxVal @nx__f_2____main____f4(ptr %t3626, i64 2)
  %t3628 = extractvalue %NxVal %t3627, 1
  %t3629 = extractvalue %NxVal %t3618, 1
  %t3630 = add i64 %t3629, %t3628
  %t3631 = add i64 65535, 0
  %t3632 = and i64 %t3630, %t3631
  %t3633 = call %NxVal @nx_int(i64 %t3632)
  store %NxVal %t3633, ptr @nx__g___main____total
  %t3634 = load %NxVal, ptr @nx__g___main____total
  %t3635 = add i64 15, 0
  %t3636 = add i64 29, 0
  %t3638 = call %NxVal @nx_int(i64 %t3635)
  %t3639 = getelementptr [2 x %NxVal], ptr %t3637, i64 0, i64 0
  store %NxVal %t3638, ptr %t3639
  %t3640 = call %NxVal @nx_int(i64 %t3636)
  %t3641 = getelementptr [2 x %NxVal], ptr %t3637, i64 0, i64 1
  store %NxVal %t3640, ptr %t3641
  %t3642 = getelementptr [2 x %NxVal], ptr %t3637, i64 0, i64 0
  %t3643 = call %NxVal @nx__f_2____main____f5(ptr %t3642, i64 2)
  %t3644 = extractvalue %NxVal %t3643, 1
  %t3645 = extractvalue %NxVal %t3634, 1
  %t3646 = add i64 %t3645, %t3644
  %t3647 = add i64 65535, 0
  %t3648 = and i64 %t3646, %t3647
  %t3649 = call %NxVal @nx_int(i64 %t3648)
  store %NxVal %t3649, ptr @nx__g___main____total
  %t3650 = load %NxVal, ptr @nx__g___main____total
  %t3651 = add i64 296, 0
  %t3652 = add i64 279, 0
  %t3654 = call %NxVal @nx_int(i64 %t3651)
  %t3655 = getelementptr [2 x %NxVal], ptr %t3653, i64 0, i64 0
  store %NxVal %t3654, ptr %t3655
  %t3656 = call %NxVal @nx_int(i64 %t3652)
  %t3657 = getelementptr [2 x %NxVal], ptr %t3653, i64 0, i64 1
  store %NxVal %t3656, ptr %t3657
  %t3658 = getelementptr [2 x %NxVal], ptr %t3653, i64 0, i64 0
  %t3659 = call %NxVal @nx__f_2____main____f6(ptr %t3658, i64 2)
  %t3660 = extractvalue %NxVal %t3659, 1
  %t3661 = extractvalue %NxVal %t3650, 1
  %t3662 = add i64 %t3661, %t3660
  %t3663 = add i64 65535, 0
  %t3664 = and i64 %t3662, %t3663
  %t3665 = call %NxVal @nx_int(i64 %t3664)
  store %NxVal %t3665, ptr @nx__g___main____total
  %t3666 = load %NxVal, ptr @nx__g___main____total
  %t3667 = add i64 455, 0
  %t3668 = add i64 312, 0
  %t3670 = call %NxVal @nx_int(i64 %t3667)
  %t3671 = getelementptr [2 x %NxVal], ptr %t3669, i64 0, i64 0
  store %NxVal %t3670, ptr %t3671
  %t3672 = call %NxVal @nx_int(i64 %t3668)
  %t3673 = getelementptr [2 x %NxVal], ptr %t3669, i64 0, i64 1
  store %NxVal %t3672, ptr %t3673
  %t3674 = getelementptr [2 x %NxVal], ptr %t3669, i64 0, i64 0
  %t3675 = call %NxVal @nx__f_2____main____f7(ptr %t3674, i64 2)
  %t3676 = extractvalue %NxVal %t3675, 1
  %t3677 = extractvalue %NxVal %t3666, 1
  %t3678 = add i64 %t3677, %t3676
  %t3679 = add i64 65535, 0
  %t3680 = and i64 %t3678, %t3679
  %t3681 = call %NxVal @nx_int(i64 %t3680)
  store %NxVal %t3681, ptr @nx__g___main____total
  %t3682 = load %NxVal, ptr @nx__g___main____total
  %t3683 = add i64 181, 0
  %t3684 = add i64 331, 0
  %t3686 = call %NxVal @nx_int(i64 %t3683)
  %t3687 = getelementptr [2 x %NxVal], ptr %t3685, i64 0, i64 0
  store %NxVal %t3686, ptr %t3687
  %t3688 = call %NxVal @nx_int(i64 %t3684)
  %t3689 = getelementptr [2 x %NxVal], ptr %t3685, i64 0, i64 1
  store %NxVal %t3688, ptr %t3689
  %t3690 = getelementptr [2 x %NxVal], ptr %t3685, i64 0, i64 0
  %t3691 = call %NxVal @nx__f_2____main____f8(ptr %t3690, i64 2)
  %t3692 = extractvalue %NxVal %t3691, 1
  %t3693 = extractvalue %NxVal %t3682, 1
  %t3694 = add i64 %t3693, %t3692
  %t3695 = add i64 65535, 0
  %t3696 = and i64 %t3694, %t3695
  %t3697 = call %NxVal @nx_int(i64 %t3696)
  store %NxVal %t3697, ptr @nx__g___main____total
  %t3698 = load %NxVal, ptr @nx__g___main____total
  %t3699 = add i64 408, 0
  %t3700 = add i64 456, 0
  %t3702 = call %NxVal @nx_int(i64 %t3699)
  %t3703 = getelementptr [2 x %NxVal], ptr %t3701, i64 0, i64 0
  store %NxVal %t3702, ptr %t3703
  %t3704 = call %NxVal @nx_int(i64 %t3700)
  %t3705 = getelementptr [2 x %NxVal], ptr %t3701, i64 0, i64 1
  store %NxVal %t3704, ptr %t3705
  %t3706 = getelementptr [2 x %NxVal], ptr %t3701, i64 0, i64 0
  %t3707 = call %NxVal @nx__f_2____main____f9(ptr %t3706, i64 2)
  %t3708 = extractvalue %NxVal %t3707, 1
  %t3709 = extractvalue %NxVal %t3698, 1
  %t3710 = add i64 %t3709, %t3708
  %t3711 = add i64 65535, 0
  %t3712 = and i64 %t3710, %t3711
  %t3713 = call %NxVal @nx_int(i64 %t3712)
  store %NxVal %t3713, ptr @nx__g___main____total
  %t3714 = load %NxVal, ptr @nx__g___main____total
  %t3715 = add i64 81, 0
  %t3716 = add i64 180, 0
  %t3718 = call %NxVal @nx_int(i64 %t3715)
  %t3719 = getelementptr [2 x %NxVal], ptr %t3717, i64 0, i64 0
  store %NxVal %t3718, ptr %t3719
  %t3720 = call %NxVal @nx_int(i64 %t3716)
  %t3721 = getelementptr [2 x %NxVal], ptr %t3717, i64 0, i64 1
  store %NxVal %t3720, ptr %t3721
  %t3722 = getelementptr [2 x %NxVal], ptr %t3717, i64 0, i64 0
  %t3723 = call %NxVal @nx__f_3____main____f10(ptr %t3722, i64 2)
  %t3724 = extractvalue %NxVal %t3723, 1
  %t3725 = extractvalue %NxVal %t3714, 1
  %t3726 = add i64 %t3725, %t3724
  %t3727 = add i64 65535, 0
  %t3728 = and i64 %t3726, %t3727
  %t3729 = call %NxVal @nx_int(i64 %t3728)
  store %NxVal %t3729, ptr @nx__g___main____total
  %t3730 = load %NxVal, ptr @nx__g___main____total
  %t3731 = add i64 351, 0
  %t3732 = add i64 124, 0
  %t3734 = call %NxVal @nx_int(i64 %t3731)
  %t3735 = getelementptr [2 x %NxVal], ptr %t3733, i64 0, i64 0
  store %NxVal %t3734, ptr %t3735
  %t3736 = call %NxVal @nx_int(i64 %t3732)
  %t3737 = getelementptr [2 x %NxVal], ptr %t3733, i64 0, i64 1
  store %NxVal %t3736, ptr %t3737
  %t3738 = getelementptr [2 x %NxVal], ptr %t3733, i64 0, i64 0
  %t3739 = call %NxVal @nx__f_3____main____f11(ptr %t3738, i64 2)
  %t3740 = extractvalue %NxVal %t3739, 1
  %t3741 = extractvalue %NxVal %t3730, 1
  %t3742 = add i64 %t3741, %t3740
  %t3743 = add i64 65535, 0
  %t3744 = and i64 %t3742, %t3743
  %t3745 = call %NxVal @nx_int(i64 %t3744)
  store %NxVal %t3745, ptr @nx__g___main____total
  %t3746 = load %NxVal, ptr @nx__g___main____total
  %t3747 = add i64 314, 0
  %t3748 = add i64 477, 0
  %t3750 = call %NxVal @nx_int(i64 %t3747)
  %t3751 = getelementptr [2 x %NxVal], ptr %t3749, i64 0, i64 0
  store %NxVal %t3750, ptr %t3751
  %t3752 = call %NxVal @nx_int(i64 %t3748)
  %t3753 = getelementptr [2 x %NxVal], ptr %t3749, i64 0, i64 1
  store %NxVal %t3752, ptr %t3753
  %t3754 = getelementptr [2 x %NxVal], ptr %t3749, i64 0, i64 0
  %t3755 = call %NxVal @nx__f_3____main____f12(ptr %t3754, i64 2)
  %t3756 = extractvalue %NxVal %t3755, 1
  %t3757 = extractvalue %NxVal %t3746, 1
  %t3758 = add i64 %t3757, %t3756
  %t3759 = add i64 65535, 0
  %t3760 = and i64 %t3758, %t3759
  %t3761 = call %NxVal @nx_int(i64 %t3760)
  store %NxVal %t3761, ptr @nx__g___main____total
  %t3762 = load %NxVal, ptr @nx__g___main____total
  %t3763 = add i64 183, 0
  %t3764 = add i64 191, 0
  %t3766 = call %NxVal @nx_int(i64 %t3763)
  %t3767 = getelementptr [2 x %NxVal], ptr %t3765, i64 0, i64 0
  store %NxVal %t3766, ptr %t3767
  %t3768 = call %NxVal @nx_int(i64 %t3764)
  %t3769 = getelementptr [2 x %NxVal], ptr %t3765, i64 0, i64 1
  store %NxVal %t3768, ptr %t3769
  %t3770 = getelementptr [2 x %NxVal], ptr %t3765, i64 0, i64 0
  %t3771 = call %NxVal @nx__f_3____main____f13(ptr %t3770, i64 2)
  %t3772 = extractvalue %NxVal %t3771, 1
  %t3773 = extractvalue %NxVal %t3762, 1
  %t3774 = add i64 %t3773, %t3772
  %t3775 = add i64 65535, 0
  %t3776 = and i64 %t3774, %t3775
  %t3777 = call %NxVal @nx_int(i64 %t3776)
  store %NxVal %t3777, ptr @nx__g___main____total
  %t3778 = load %NxVal, ptr @nx__g___main____total
  %t3779 = add i64 106, 0
  %t3780 = add i64 20, 0
  %t3782 = call %NxVal @nx_int(i64 %t3779)
  %t3783 = getelementptr [2 x %NxVal], ptr %t3781, i64 0, i64 0
  store %NxVal %t3782, ptr %t3783
  %t3784 = call %NxVal @nx_int(i64 %t3780)
  %t3785 = getelementptr [2 x %NxVal], ptr %t3781, i64 0, i64 1
  store %NxVal %t3784, ptr %t3785
  %t3786 = getelementptr [2 x %NxVal], ptr %t3781, i64 0, i64 0
  %t3787 = call %NxVal @nx__f_3____main____f14(ptr %t3786, i64 2)
  %t3788 = extractvalue %NxVal %t3787, 1
  %t3789 = extractvalue %NxVal %t3778, 1
  %t3790 = add i64 %t3789, %t3788
  %t3791 = add i64 65535, 0
  %t3792 = and i64 %t3790, %t3791
  %t3793 = call %NxVal @nx_int(i64 %t3792)
  store %NxVal %t3793, ptr @nx__g___main____total
  %t3794 = load %NxVal, ptr @nx__g___main____total
  %t3795 = add i64 254, 0
  %t3796 = add i64 188, 0
  %t3798 = call %NxVal @nx_int(i64 %t3795)
  %t3799 = getelementptr [2 x %NxVal], ptr %t3797, i64 0, i64 0
  store %NxVal %t3798, ptr %t3799
  %t3800 = call %NxVal @nx_int(i64 %t3796)
  %t3801 = getelementptr [2 x %NxVal], ptr %t3797, i64 0, i64 1
  store %NxVal %t3800, ptr %t3801
  %t3802 = getelementptr [2 x %NxVal], ptr %t3797, i64 0, i64 0
  %t3803 = call %NxVal @nx__f_3____main____f15(ptr %t3802, i64 2)
  %t3804 = extractvalue %NxVal %t3803, 1
  %t3805 = extractvalue %NxVal %t3794, 1
  %t3806 = add i64 %t3805, %t3804
  %t3807 = add i64 65535, 0
  %t3808 = and i64 %t3806, %t3807
  %t3809 = call %NxVal @nx_int(i64 %t3808)
  store %NxVal %t3809, ptr @nx__g___main____total
  %t3810 = load %NxVal, ptr @nx__g___main____total
  %t3811 = add i64 157, 0
  %t3812 = add i64 355, 0
  %t3814 = call %NxVal @nx_int(i64 %t3811)
  %t3815 = getelementptr [2 x %NxVal], ptr %t3813, i64 0, i64 0
  store %NxVal %t3814, ptr %t3815
  %t3816 = call %NxVal @nx_int(i64 %t3812)
  %t3817 = getelementptr [2 x %NxVal], ptr %t3813, i64 0, i64 1
  store %NxVal %t3816, ptr %t3817
  %t3818 = getelementptr [2 x %NxVal], ptr %t3813, i64 0, i64 0
  %t3819 = call %NxVal @nx__f_3____main____f16(ptr %t3818, i64 2)
  %t3820 = extractvalue %NxVal %t3819, 1
  %t3821 = extractvalue %NxVal %t3810, 1
  %t3822 = add i64 %t3821, %t3820
  %t3823 = add i64 65535, 0
  %t3824 = and i64 %t3822, %t3823
  %t3825 = call %NxVal @nx_int(i64 %t3824)
  store %NxVal %t3825, ptr @nx__g___main____total
  %t3826 = load %NxVal, ptr @nx__g___main____total
  %t3827 = add i64 334, 0
  %t3828 = add i64 352, 0
  %t3830 = call %NxVal @nx_int(i64 %t3827)
  %t3831 = getelementptr [2 x %NxVal], ptr %t3829, i64 0, i64 0
  store %NxVal %t3830, ptr %t3831
  %t3832 = call %NxVal @nx_int(i64 %t3828)
  %t3833 = getelementptr [2 x %NxVal], ptr %t3829, i64 0, i64 1
  store %NxVal %t3832, ptr %t3833
  %t3834 = getelementptr [2 x %NxVal], ptr %t3829, i64 0, i64 0
  %t3835 = call %NxVal @nx__f_3____main____f17(ptr %t3834, i64 2)
  %t3836 = extractvalue %NxVal %t3835, 1
  %t3837 = extractvalue %NxVal %t3826, 1
  %t3838 = add i64 %t3837, %t3836
  %t3839 = add i64 65535, 0
  %t3840 = and i64 %t3838, %t3839
  %t3841 = call %NxVal @nx_int(i64 %t3840)
  store %NxVal %t3841, ptr @nx__g___main____total
  %t3842 = load %NxVal, ptr @nx__g___main____total
  %t3843 = add i64 81, 0
  %t3844 = add i64 150, 0
  %t3846 = call %NxVal @nx_int(i64 %t3843)
  %t3847 = getelementptr [2 x %NxVal], ptr %t3845, i64 0, i64 0
  store %NxVal %t3846, ptr %t3847
  %t3848 = call %NxVal @nx_int(i64 %t3844)
  %t3849 = getelementptr [2 x %NxVal], ptr %t3845, i64 0, i64 1
  store %NxVal %t3848, ptr %t3849
  %t3850 = getelementptr [2 x %NxVal], ptr %t3845, i64 0, i64 0
  %t3851 = call %NxVal @nx__f_3____main____f18(ptr %t3850, i64 2)
  %t3852 = extractvalue %NxVal %t3851, 1
  %t3853 = extractvalue %NxVal %t3842, 1
  %t3854 = add i64 %t3853, %t3852
  %t3855 = add i64 65535, 0
  %t3856 = and i64 %t3854, %t3855
  %t3857 = call %NxVal @nx_int(i64 %t3856)
  store %NxVal %t3857, ptr @nx__g___main____total
  %t3858 = load %NxVal, ptr @nx__g___main____total
  %t3859 = add i64 132, 0
  %t3860 = add i64 86, 0
  %t3862 = call %NxVal @nx_int(i64 %t3859)
  %t3863 = getelementptr [2 x %NxVal], ptr %t3861, i64 0, i64 0
  store %NxVal %t3862, ptr %t3863
  %t3864 = call %NxVal @nx_int(i64 %t3860)
  %t3865 = getelementptr [2 x %NxVal], ptr %t3861, i64 0, i64 1
  store %NxVal %t3864, ptr %t3865
  %t3866 = getelementptr [2 x %NxVal], ptr %t3861, i64 0, i64 0
  %t3867 = call %NxVal @nx__f_3____main____f19(ptr %t3866, i64 2)
  %t3868 = extractvalue %NxVal %t3867, 1
  %t3869 = extractvalue %NxVal %t3858, 1
  %t3870 = add i64 %t3869, %t3868
  %t3871 = add i64 65535, 0
  %t3872 = and i64 %t3870, %t3871
  %t3873 = call %NxVal @nx_int(i64 %t3872)
  store %NxVal %t3873, ptr @nx__g___main____total
  %t3874 = load %NxVal, ptr @nx__g___main____total
  %t3875 = add i64 448, 0
  %t3876 = add i64 315, 0
  %t3878 = call %NxVal @nx_int(i64 %t3875)
  %t3879 = getelementptr [2 x %NxVal], ptr %t3877, i64 0, i64 0
  store %NxVal %t3878, ptr %t3879
  %t3880 = call %NxVal @nx_int(i64 %t3876)
  %t3881 = getelementptr [2 x %NxVal], ptr %t3877, i64 0, i64 1
  store %NxVal %t3880, ptr %t3881
  %t3882 = getelementptr [2 x %NxVal], ptr %t3877, i64 0, i64 0
  %t3883 = call %NxVal @nx__f_3____main____f20(ptr %t3882, i64 2)
  %t3884 = extractvalue %NxVal %t3883, 1
  %t3885 = extractvalue %NxVal %t3874, 1
  %t3886 = add i64 %t3885, %t3884
  %t3887 = add i64 65535, 0
  %t3888 = and i64 %t3886, %t3887
  %t3889 = call %NxVal @nx_int(i64 %t3888)
  store %NxVal %t3889, ptr @nx__g___main____total
  %t3890 = load %NxVal, ptr @nx__g___main____total
  %t3891 = add i64 315, 0
  %t3892 = add i64 12, 0
  %t3894 = call %NxVal @nx_int(i64 %t3891)
  %t3895 = getelementptr [2 x %NxVal], ptr %t3893, i64 0, i64 0
  store %NxVal %t3894, ptr %t3895
  %t3896 = call %NxVal @nx_int(i64 %t3892)
  %t3897 = getelementptr [2 x %NxVal], ptr %t3893, i64 0, i64 1
  store %NxVal %t3896, ptr %t3897
  %t3898 = getelementptr [2 x %NxVal], ptr %t3893, i64 0, i64 0
  %t3899 = call %NxVal @nx__f_3____main____f21(ptr %t3898, i64 2)
  %t3900 = extractvalue %NxVal %t3899, 1
  %t3901 = extractvalue %NxVal %t3890, 1
  %t3902 = add i64 %t3901, %t3900
  %t3903 = add i64 65535, 0
  %t3904 = and i64 %t3902, %t3903
  %t3905 = call %NxVal @nx_int(i64 %t3904)
  store %NxVal %t3905, ptr @nx__g___main____total
  %t3906 = load %NxVal, ptr @nx__g___main____total
  %t3907 = add i64 231, 0
  %t3908 = add i64 412, 0
  %t3910 = call %NxVal @nx_int(i64 %t3907)
  %t3911 = getelementptr [2 x %NxVal], ptr %t3909, i64 0, i64 0
  store %NxVal %t3910, ptr %t3911
  %t3912 = call %NxVal @nx_int(i64 %t3908)
  %t3913 = getelementptr [2 x %NxVal], ptr %t3909, i64 0, i64 1
  store %NxVal %t3912, ptr %t3913
  %t3914 = getelementptr [2 x %NxVal], ptr %t3909, i64 0, i64 0
  %t3915 = call %NxVal @nx__f_3____main____f22(ptr %t3914, i64 2)
  %t3916 = extractvalue %NxVal %t3915, 1
  %t3917 = extractvalue %NxVal %t3906, 1
  %t3918 = add i64 %t3917, %t3916
  %t3919 = add i64 65535, 0
  %t3920 = and i64 %t3918, %t3919
  %t3921 = call %NxVal @nx_int(i64 %t3920)
  store %NxVal %t3921, ptr @nx__g___main____total
  %t3922 = load %NxVal, ptr @nx__g___main____total
  %t3923 = add i64 465, 0
  %t3924 = add i64 428, 0
  %t3926 = call %NxVal @nx_int(i64 %t3923)
  %t3927 = getelementptr [2 x %NxVal], ptr %t3925, i64 0, i64 0
  store %NxVal %t3926, ptr %t3927
  %t3928 = call %NxVal @nx_int(i64 %t3924)
  %t3929 = getelementptr [2 x %NxVal], ptr %t3925, i64 0, i64 1
  store %NxVal %t3928, ptr %t3929
  %t3930 = getelementptr [2 x %NxVal], ptr %t3925, i64 0, i64 0
  %t3931 = call %NxVal @nx__f_3____main____f23(ptr %t3930, i64 2)
  %t3932 = extractvalue %NxVal %t3931, 1
  %t3933 = extractvalue %NxVal %t3922, 1
  %t3934 = add i64 %t3933, %t3932
  %t3935 = add i64 65535, 0
  %t3936 = and i64 %t3934, %t3935
  %t3937 = call %NxVal @nx_int(i64 %t3936)
  store %NxVal %t3937, ptr @nx__g___main____total
  %t3938 = load %NxVal, ptr @nx__g___main____total
  %t3939 = add i64 386, 0
  %t3940 = add i64 198, 0
  %t3942 = call %NxVal @nx_int(i64 %t3939)
  %t3943 = getelementptr [2 x %NxVal], ptr %t3941, i64 0, i64 0
  store %NxVal %t3942, ptr %t3943
  %t3944 = call %NxVal @nx_int(i64 %t3940)
  %t3945 = getelementptr [2 x %NxVal], ptr %t3941, i64 0, i64 1
  store %NxVal %t3944, ptr %t3945
  %t3946 = getelementptr [2 x %NxVal], ptr %t3941, i64 0, i64 0
  %t3947 = call %NxVal @nx__f_3____main____f24(ptr %t3946, i64 2)
  %t3948 = extractvalue %NxVal %t3947, 1
  %t3949 = extractvalue %NxVal %t3938, 1
  %t3950 = add i64 %t3949, %t3948
  %t3951 = add i64 65535, 0
  %t3952 = and i64 %t3950, %t3951
  %t3953 = call %NxVal @nx_int(i64 %t3952)
  store %NxVal %t3953, ptr @nx__g___main____total
  %t3954 = load %NxVal, ptr @nx__g___main____total
  %t3955 = add i64 96, 0
  %t3956 = add i64 291, 0
  %t3958 = call %NxVal @nx_int(i64 %t3955)
  %t3959 = getelementptr [2 x %NxVal], ptr %t3957, i64 0, i64 0
  store %NxVal %t3958, ptr %t3959
  %t3960 = call %NxVal @nx_int(i64 %t3956)
  %t3961 = getelementptr [2 x %NxVal], ptr %t3957, i64 0, i64 1
  store %NxVal %t3960, ptr %t3961
  %t3962 = getelementptr [2 x %NxVal], ptr %t3957, i64 0, i64 0
  %t3963 = call %NxVal @nx__f_3____main____f25(ptr %t3962, i64 2)
  %t3964 = extractvalue %NxVal %t3963, 1
  %t3965 = extractvalue %NxVal %t3954, 1
  %t3966 = add i64 %t3965, %t3964
  %t3967 = add i64 65535, 0
  %t3968 = and i64 %t3966, %t3967
  %t3969 = call %NxVal @nx_int(i64 %t3968)
  store %NxVal %t3969, ptr @nx__g___main____total
  %t3970 = load %NxVal, ptr @nx__g___main____total
  %t3971 = add i64 268, 0
  %t3972 = add i64 133, 0
  %t3974 = call %NxVal @nx_int(i64 %t3971)
  %t3975 = getelementptr [2 x %NxVal], ptr %t3973, i64 0, i64 0
  store %NxVal %t3974, ptr %t3975
  %t3976 = call %NxVal @nx_int(i64 %t3972)
  %t3977 = getelementptr [2 x %NxVal], ptr %t3973, i64 0, i64 1
  store %NxVal %t3976, ptr %t3977
  %t3978 = getelementptr [2 x %NxVal], ptr %t3973, i64 0, i64 0
  %t3979 = call %NxVal @nx__f_3____main____f26(ptr %t3978, i64 2)
  %t3980 = extractvalue %NxVal %t3979, 1
  %t3981 = extractvalue %NxVal %t3970, 1
  %t3982 = add i64 %t3981, %t3980
  %t3983 = add i64 65535, 0
  %t3984 = and i64 %t3982, %t3983
  %t3985 = call %NxVal @nx_int(i64 %t3984)
  store %NxVal %t3985, ptr @nx__g___main____total
  %t3986 = load %NxVal, ptr @nx__g___main____total
  %t3987 = add i64 197, 0
  %t3988 = add i64 285, 0
  %t3990 = call %NxVal @nx_int(i64 %t3987)
  %t3991 = getelementptr [2 x %NxVal], ptr %t3989, i64 0, i64 0
  store %NxVal %t3990, ptr %t3991
  %t3992 = call %NxVal @nx_int(i64 %t3988)
  %t3993 = getelementptr [2 x %NxVal], ptr %t3989, i64 0, i64 1
  store %NxVal %t3992, ptr %t3993
  %t3994 = getelementptr [2 x %NxVal], ptr %t3989, i64 0, i64 0
  %t3995 = call %NxVal @nx__f_3____main____f27(ptr %t3994, i64 2)
  %t3996 = extractvalue %NxVal %t3995, 1
  %t3997 = extractvalue %NxVal %t3986, 1
  %t3998 = add i64 %t3997, %t3996
  %t3999 = add i64 65535, 0
  %t4000 = and i64 %t3998, %t3999
  %t4001 = call %NxVal @nx_int(i64 %t4000)
  store %NxVal %t4001, ptr @nx__g___main____total
  %t4002 = load %NxVal, ptr @nx__g___main____total
  %t4003 = add i64 194, 0
  %t4004 = add i64 357, 0
  %t4006 = call %NxVal @nx_int(i64 %t4003)
  %t4007 = getelementptr [2 x %NxVal], ptr %t4005, i64 0, i64 0
  store %NxVal %t4006, ptr %t4007
  %t4008 = call %NxVal @nx_int(i64 %t4004)
  %t4009 = getelementptr [2 x %NxVal], ptr %t4005, i64 0, i64 1
  store %NxVal %t4008, ptr %t4009
  %t4010 = getelementptr [2 x %NxVal], ptr %t4005, i64 0, i64 0
  %t4011 = call %NxVal @nx__f_3____main____f28(ptr %t4010, i64 2)
  %t4012 = extractvalue %NxVal %t4011, 1
  %t4013 = extractvalue %NxVal %t4002, 1
  %t4014 = add i64 %t4013, %t4012
  %t4015 = add i64 65535, 0
  %t4016 = and i64 %t4014, %t4015
  %t4017 = call %NxVal @nx_int(i64 %t4016)
  store %NxVal %t4017, ptr @nx__g___main____total
  %t4018 = load %NxVal, ptr @nx__g___main____total
  %t4019 = add i64 155, 0
  %t4020 = add i64 261, 0
  %t4022 = call %NxVal @nx_int(i64 %t4019)
  %t4023 = getelementptr [2 x %NxVal], ptr %t4021, i64 0, i64 0
  store %NxVal %t4022, ptr %t4023
  %t4024 = call %NxVal @nx_int(i64 %t4020)
  %t4025 = getelementptr [2 x %NxVal], ptr %t4021, i64 0, i64 1
  store %NxVal %t4024, ptr %t4025
  %t4026 = getelementptr [2 x %NxVal], ptr %t4021, i64 0, i64 0
  %t4027 = call %NxVal @nx__f_3____main____f29(ptr %t4026, i64 2)
  %t4028 = extractvalue %NxVal %t4027, 1
  %t4029 = extractvalue %NxVal %t4018, 1
  %t4030 = add i64 %t4029, %t4028
  %t4031 = add i64 65535, 0
  %t4032 = and i64 %t4030, %t4031
  %t4033 = call %NxVal @nx_int(i64 %t4032)
  store %NxVal %t4033, ptr @nx__g___main____total
  %t4034 = load %NxVal, ptr @nx__g___main____total
  %t4035 = add i64 416, 0
  %t4036 = add i64 177, 0
  %t4038 = call %NxVal @nx_int(i64 %t4035)
  %t4039 = getelementptr [2 x %NxVal], ptr %t4037, i64 0, i64 0
  store %NxVal %t4038, ptr %t4039
  %t4040 = call %NxVal @nx_int(i64 %t4036)
  %t4041 = getelementptr [2 x %NxVal], ptr %t4037, i64 0, i64 1
  store %NxVal %t4040, ptr %t4041
  %t4042 = getelementptr [2 x %NxVal], ptr %t4037, i64 0, i64 0
  %t4043 = call %NxVal @nx__f_3____main____f30(ptr %t4042, i64 2)
  %t4044 = extractvalue %NxVal %t4043, 1
  %t4045 = extractvalue %NxVal %t4034, 1
  %t4046 = add i64 %t4045, %t4044
  %t4047 = add i64 65535, 0
  %t4048 = and i64 %t4046, %t4047
  %t4049 = call %NxVal @nx_int(i64 %t4048)
  store %NxVal %t4049, ptr @nx__g___main____total
  %t4050 = load %NxVal, ptr @nx__g___main____total
  %t4051 = add i64 320, 0
  %t4052 = add i64 264, 0
  %t4054 = call %NxVal @nx_int(i64 %t4051)
  %t4055 = getelementptr [2 x %NxVal], ptr %t4053, i64 0, i64 0
  store %NxVal %t4054, ptr %t4055
  %t4056 = call %NxVal @nx_int(i64 %t4052)
  %t4057 = getelementptr [2 x %NxVal], ptr %t4053, i64 0, i64 1
  store %NxVal %t4056, ptr %t4057
  %t4058 = getelementptr [2 x %NxVal], ptr %t4053, i64 0, i64 0
  %t4059 = call %NxVal @nx__f_3____main____f31(ptr %t4058, i64 2)
  %t4060 = extractvalue %NxVal %t4059, 1
  %t4061 = extractvalue %NxVal %t4050, 1
  %t4062 = add i64 %t4061, %t4060
  %t4063 = add i64 65535, 0
  %t4064 = and i64 %t4062, %t4063
  %t4065 = call %NxVal @nx_int(i64 %t4064)
  store %NxVal %t4065, ptr @nx__g___main____total
  %t4066 = load %NxVal, ptr @nx__g___main____total
  %t4067 = add i64 98, 0
  %t4068 = add i64 118, 0
  %t4070 = call %NxVal @nx_int(i64 %t4067)
  %t4071 = getelementptr [2 x %NxVal], ptr %t4069, i64 0, i64 0
  store %NxVal %t4070, ptr %t4071
  %t4072 = call %NxVal @nx_int(i64 %t4068)
  %t4073 = getelementptr [2 x %NxVal], ptr %t4069, i64 0, i64 1
  store %NxVal %t4072, ptr %t4073
  %t4074 = getelementptr [2 x %NxVal], ptr %t4069, i64 0, i64 0
  %t4075 = call %NxVal @nx__f_3____main____f32(ptr %t4074, i64 2)
  %t4076 = extractvalue %NxVal %t4075, 1
  %t4077 = extractvalue %NxVal %t4066, 1
  %t4078 = add i64 %t4077, %t4076
  %t4079 = add i64 65535, 0
  %t4080 = and i64 %t4078, %t4079
  %t4081 = call %NxVal @nx_int(i64 %t4080)
  store %NxVal %t4081, ptr @nx__g___main____total
  %t4082 = load %NxVal, ptr @nx__g___main____total
  %t4083 = add i64 462, 0
  %t4084 = add i64 18, 0
  %t4086 = call %NxVal @nx_int(i64 %t4083)
  %t4087 = getelementptr [2 x %NxVal], ptr %t4085, i64 0, i64 0
  store %NxVal %t4086, ptr %t4087
  %t4088 = call %NxVal @nx_int(i64 %t4084)
  %t4089 = getelementptr [2 x %NxVal], ptr %t4085, i64 0, i64 1
  store %NxVal %t4088, ptr %t4089
  %t4090 = getelementptr [2 x %NxVal], ptr %t4085, i64 0, i64 0
  %t4091 = call %NxVal @nx__f_3____main____f33(ptr %t4090, i64 2)
  %t4092 = extractvalue %NxVal %t4091, 1
  %t4093 = extractvalue %NxVal %t4082, 1
  %t4094 = add i64 %t4093, %t4092
  %t4095 = add i64 65535, 0
  %t4096 = and i64 %t4094, %t4095
  %t4097 = call %NxVal @nx_int(i64 %t4096)
  store %NxVal %t4097, ptr @nx__g___main____total
  %t4098 = load %NxVal, ptr @nx__g___main____total
  %t4099 = add i64 388, 0
  %t4100 = add i64 384, 0
  %t4102 = call %NxVal @nx_int(i64 %t4099)
  %t4103 = getelementptr [2 x %NxVal], ptr %t4101, i64 0, i64 0
  store %NxVal %t4102, ptr %t4103
  %t4104 = call %NxVal @nx_int(i64 %t4100)
  %t4105 = getelementptr [2 x %NxVal], ptr %t4101, i64 0, i64 1
  store %NxVal %t4104, ptr %t4105
  %t4106 = getelementptr [2 x %NxVal], ptr %t4101, i64 0, i64 0
  %t4107 = call %NxVal @nx__f_3____main____f34(ptr %t4106, i64 2)
  %t4108 = extractvalue %NxVal %t4107, 1
  %t4109 = extractvalue %NxVal %t4098, 1
  %t4110 = add i64 %t4109, %t4108
  %t4111 = add i64 65535, 0
  %t4112 = and i64 %t4110, %t4111
  %t4113 = call %NxVal @nx_int(i64 %t4112)
  store %NxVal %t4113, ptr @nx__g___main____total
  %t4114 = load %NxVal, ptr @nx__g___main____total
  %t4115 = add i64 290, 0
  %t4116 = add i64 463, 0
  %t4118 = call %NxVal @nx_int(i64 %t4115)
  %t4119 = getelementptr [2 x %NxVal], ptr %t4117, i64 0, i64 0
  store %NxVal %t4118, ptr %t4119
  %t4120 = call %NxVal @nx_int(i64 %t4116)
  %t4121 = getelementptr [2 x %NxVal], ptr %t4117, i64 0, i64 1
  store %NxVal %t4120, ptr %t4121
  %t4122 = getelementptr [2 x %NxVal], ptr %t4117, i64 0, i64 0
  %t4123 = call %NxVal @nx__f_3____main____f35(ptr %t4122, i64 2)
  %t4124 = extractvalue %NxVal %t4123, 1
  %t4125 = extractvalue %NxVal %t4114, 1
  %t4126 = add i64 %t4125, %t4124
  %t4127 = add i64 65535, 0
  %t4128 = and i64 %t4126, %t4127
  %t4129 = call %NxVal @nx_int(i64 %t4128)
  store %NxVal %t4129, ptr @nx__g___main____total
  %t4130 = load %NxVal, ptr @nx__g___main____total
  %t4131 = add i64 321, 0
  %t4132 = add i64 346, 0
  %t4134 = call %NxVal @nx_int(i64 %t4131)
  %t4135 = getelementptr [2 x %NxVal], ptr %t4133, i64 0, i64 0
  store %NxVal %t4134, ptr %t4135
  %t4136 = call %NxVal @nx_int(i64 %t4132)
  %t4137 = getelementptr [2 x %NxVal], ptr %t4133, i64 0, i64 1
  store %NxVal %t4136, ptr %t4137
  %t4138 = getelementptr [2 x %NxVal], ptr %t4133, i64 0, i64 0
  %t4139 = call %NxVal @nx__f_3____main____f36(ptr %t4138, i64 2)
  %t4140 = extractvalue %NxVal %t4139, 1
  %t4141 = extractvalue %NxVal %t4130, 1
  %t4142 = add i64 %t4141, %t4140
  %t4143 = add i64 65535, 0
  %t4144 = and i64 %t4142, %t4143
  %t4145 = call %NxVal @nx_int(i64 %t4144)
  store %NxVal %t4145, ptr @nx__g___main____total
  %t4146 = load %NxVal, ptr @nx__g___main____total
  %t4147 = add i64 471, 0
  %t4148 = add i64 170, 0
  %t4150 = call %NxVal @nx_int(i64 %t4147)
  %t4151 = getelementptr [2 x %NxVal], ptr %t4149, i64 0, i64 0
  store %NxVal %t4150, ptr %t4151
  %t4152 = call %NxVal @nx_int(i64 %t4148)
  %t4153 = getelementptr [2 x %NxVal], ptr %t4149, i64 0, i64 1
  store %NxVal %t4152, ptr %t4153
  %t4154 = getelementptr [2 x %NxVal], ptr %t4149, i64 0, i64 0
  %t4155 = call %NxVal @nx__f_3____main____f37(ptr %t4154, i64 2)
  %t4156 = extractvalue %NxVal %t4155, 1
  %t4157 = extractvalue %NxVal %t4146, 1
  %t4158 = add i64 %t4157, %t4156
  %t4159 = add i64 65535, 0
  %t4160 = and i64 %t4158, %t4159
  %t4161 = call %NxVal @nx_int(i64 %t4160)
  store %NxVal %t4161, ptr @nx__g___main____total
  %t4162 = load %NxVal, ptr @nx__g___main____total
  %t4163 = add i64 406, 0
  %t4164 = add i64 82, 0
  %t4166 = call %NxVal @nx_int(i64 %t4163)
  %t4167 = getelementptr [2 x %NxVal], ptr %t4165, i64 0, i64 0
  store %NxVal %t4166, ptr %t4167
  %t4168 = call %NxVal @nx_int(i64 %t4164)
  %t4169 = getelementptr [2 x %NxVal], ptr %t4165, i64 0, i64 1
  store %NxVal %t4168, ptr %t4169
  %t4170 = getelementptr [2 x %NxVal], ptr %t4165, i64 0, i64 0
  %t4171 = call %NxVal @nx__f_3____main____f38(ptr %t4170, i64 2)
  %t4172 = extractvalue %NxVal %t4171, 1
  %t4173 = extractvalue %NxVal %t4162, 1
  %t4174 = add i64 %t4173, %t4172
  %t4175 = add i64 65535, 0
  %t4176 = and i64 %t4174, %t4175
  %t4177 = call %NxVal @nx_int(i64 %t4176)
  store %NxVal %t4177, ptr @nx__g___main____total
  %t4178 = load %NxVal, ptr @nx__g___main____total
  %t4179 = add i64 74, 0
  %t4180 = add i64 464, 0
  %t4182 = call %NxVal @nx_int(i64 %t4179)
  %t4183 = getelementptr [2 x %NxVal], ptr %t4181, i64 0, i64 0
  store %NxVal %t4182, ptr %t4183
  %t4184 = call %NxVal @nx_int(i64 %t4180)
  %t4185 = getelementptr [2 x %NxVal], ptr %t4181, i64 0, i64 1
  store %NxVal %t4184, ptr %t4185
  %t4186 = getelementptr [2 x %NxVal], ptr %t4181, i64 0, i64 0
  %t4187 = call %NxVal @nx__f_3____main____f39(ptr %t4186, i64 2)
  %t4188 = extractvalue %NxVal %t4187, 1
  %t4189 = extractvalue %NxVal %t4178, 1
  %t4190 = add i64 %t4189, %t4188
  %t4191 = add i64 65535, 0
  %t4192 = and i64 %t4190, %t4191
  %t4193 = call %NxVal @nx_int(i64 %t4192)
  store %NxVal %t4193, ptr @nx__g___main____total
  %t4194 = load %NxVal, ptr @nx__g___main____total
  %t4195 = add i64 63, 0
  %t4196 = add i64 370, 0
  %t4198 = call %NxVal @nx_int(i64 %t4195)
  %t4199 = getelementptr [2 x %NxVal], ptr %t4197, i64 0, i64 0
  store %NxVal %t4198, ptr %t4199
  %t4200 = call %NxVal @nx_int(i64 %t4196)
  %t4201 = getelementptr [2 x %NxVal], ptr %t4197, i64 0, i64 1
  store %NxVal %t4200, ptr %t4201
  %t4202 = getelementptr [2 x %NxVal], ptr %t4197, i64 0, i64 0
  %t4203 = call %NxVal @nx__f_3____main____f40(ptr %t4202, i64 2)
  %t4204 = extractvalue %NxVal %t4203, 1
  %t4205 = extractvalue %NxVal %t4194, 1
  %t4206 = add i64 %t4205, %t4204
  %t4207 = add i64 65535, 0
  %t4208 = and i64 %t4206, %t4207
  %t4209 = call %NxVal @nx_int(i64 %t4208)
  store %NxVal %t4209, ptr @nx__g___main____total
  %t4210 = load %NxVal, ptr @nx__g___main____total
  %t4211 = add i64 152, 0
  %t4212 = add i64 301, 0
  %t4214 = call %NxVal @nx_int(i64 %t4211)
  %t4215 = getelementptr [2 x %NxVal], ptr %t4213, i64 0, i64 0
  store %NxVal %t4214, ptr %t4215
  %t4216 = call %NxVal @nx_int(i64 %t4212)
  %t4217 = getelementptr [2 x %NxVal], ptr %t4213, i64 0, i64 1
  store %NxVal %t4216, ptr %t4217
  %t4218 = getelementptr [2 x %NxVal], ptr %t4213, i64 0, i64 0
  %t4219 = call %NxVal @nx__f_3____main____f41(ptr %t4218, i64 2)
  %t4220 = extractvalue %NxVal %t4219, 1
  %t4221 = extractvalue %NxVal %t4210, 1
  %t4222 = add i64 %t4221, %t4220
  %t4223 = add i64 65535, 0
  %t4224 = and i64 %t4222, %t4223
  %t4225 = call %NxVal @nx_int(i64 %t4224)
  store %NxVal %t4225, ptr @nx__g___main____total
  %t4226 = load %NxVal, ptr @nx__g___main____total
  %t4227 = add i64 183, 0
  %t4228 = add i64 134, 0
  %t4230 = call %NxVal @nx_int(i64 %t4227)
  %t4231 = getelementptr [2 x %NxVal], ptr %t4229, i64 0, i64 0
  store %NxVal %t4230, ptr %t4231
  %t4232 = call %NxVal @nx_int(i64 %t4228)
  %t4233 = getelementptr [2 x %NxVal], ptr %t4229, i64 0, i64 1
  store %NxVal %t4232, ptr %t4233
  %t4234 = getelementptr [2 x %NxVal], ptr %t4229, i64 0, i64 0
  %t4235 = call %NxVal @nx__f_3____main____f42(ptr %t4234, i64 2)
  %t4236 = extractvalue %NxVal %t4235, 1
  %t4237 = extractvalue %NxVal %t4226, 1
  %t4238 = add i64 %t4237, %t4236
  %t4239 = add i64 65535, 0
  %t4240 = and i64 %t4238, %t4239
  %t4241 = call %NxVal @nx_int(i64 %t4240)
  store %NxVal %t4241, ptr @nx__g___main____total
  %t4242 = load %NxVal, ptr @nx__g___main____total
  %t4243 = add i64 154, 0
  %t4244 = add i64 365, 0
  %t4246 = call %NxVal @nx_int(i64 %t4243)
  %t4247 = getelementptr [2 x %NxVal], ptr %t4245, i64 0, i64 0
  store %NxVal %t4246, ptr %t4247
  %t4248 = call %NxVal @nx_int(i64 %t4244)
  %t4249 = getelementptr [2 x %NxVal], ptr %t4245, i64 0, i64 1
  store %NxVal %t4248, ptr %t4249
  %t4250 = getelementptr [2 x %NxVal], ptr %t4245, i64 0, i64 0
  %t4251 = call %NxVal @nx__f_3____main____f43(ptr %t4250, i64 2)
  %t4252 = extractvalue %NxVal %t4251, 1
  %t4253 = extractvalue %NxVal %t4242, 1
  %t4254 = add i64 %t4253, %t4252
  %t4255 = add i64 65535, 0
  %t4256 = and i64 %t4254, %t4255
  %t4257 = call %NxVal @nx_int(i64 %t4256)
  store %NxVal %t4257, ptr @nx__g___main____total
  %t4258 = load %NxVal, ptr @nx__g___main____total
  %t4259 = add i64 59, 0
  %t4260 = add i64 28, 0
  %t4262 = call %NxVal @nx_int(i64 %t4259)
  %t4263 = getelementptr [2 x %NxVal], ptr %t4261, i64 0, i64 0
  store %NxVal %t4262, ptr %t4263
  %t4264 = call %NxVal @nx_int(i64 %t4260)
  %t4265 = getelementptr [2 x %NxVal], ptr %t4261, i64 0, i64 1
  store %NxVal %t4264, ptr %t4265
  %t4266 = getelementptr [2 x %NxVal], ptr %t4261, i64 0, i64 0
  %t4267 = call %NxVal @nx__f_3____main____f44(ptr %t4266, i64 2)
  %t4268 = extractvalue %NxVal %t4267, 1
  %t4269 = extractvalue %NxVal %t4258, 1
  %t4270 = add i64 %t4269, %t4268
  %t4271 = add i64 65535, 0
  %t4272 = and i64 %t4270, %t4271
  %t4273 = call %NxVal @nx_int(i64 %t4272)
  store %NxVal %t4273, ptr @nx__g___main____total
  %t4274 = load %NxVal, ptr @nx__g___main____total
  %t4275 = add i64 495, 0
  %t4276 = add i64 507, 0
  %t4278 = call %NxVal @nx_int(i64 %t4275)
  %t4279 = getelementptr [2 x %NxVal], ptr %t4277, i64 0, i64 0
  store %NxVal %t4278, ptr %t4279
  %t4280 = call %NxVal @nx_int(i64 %t4276)
  %t4281 = getelementptr [2 x %NxVal], ptr %t4277, i64 0, i64 1
  store %NxVal %t4280, ptr %t4281
  %t4282 = getelementptr [2 x %NxVal], ptr %t4277, i64 0, i64 0
  %t4283 = call %NxVal @nx__f_3____main____f45(ptr %t4282, i64 2)
  %t4284 = extractvalue %NxVal %t4283, 1
  %t4285 = extractvalue %NxVal %t4274, 1
  %t4286 = add i64 %t4285, %t4284
  %t4287 = add i64 65535, 0
  %t4288 = and i64 %t4286, %t4287
  %t4289 = call %NxVal @nx_int(i64 %t4288)
  store %NxVal %t4289, ptr @nx__g___main____total
  %t4290 = load %NxVal, ptr @nx__g___main____total
  %t4291 = add i64 455, 0
  %t4292 = add i64 382, 0
  %t4294 = call %NxVal @nx_int(i64 %t4291)
  %t4295 = getelementptr [2 x %NxVal], ptr %t4293, i64 0, i64 0
  store %NxVal %t4294, ptr %t4295
  %t4296 = call %NxVal @nx_int(i64 %t4292)
  %t4297 = getelementptr [2 x %NxVal], ptr %t4293, i64 0, i64 1
  store %NxVal %t4296, ptr %t4297
  %t4298 = getelementptr [2 x %NxVal], ptr %t4293, i64 0, i64 0
  %t4299 = call %NxVal @nx__f_3____main____f46(ptr %t4298, i64 2)
  %t4300 = extractvalue %NxVal %t4299, 1
  %t4301 = extractvalue %NxVal %t4290, 1
  %t4302 = add i64 %t4301, %t4300
  %t4303 = add i64 65535, 0
  %t4304 = and i64 %t4302, %t4303
  %t4305 = call %NxVal @nx_int(i64 %t4304)
  store %NxVal %t4305, ptr @nx__g___main____total
  %t4306 = load %NxVal, ptr @nx__g___main____total
  %t4307 = add i64 497, 0
  %t4308 = add i64 260, 0
  %t4310 = call %NxVal @nx_int(i64 %t4307)
  %t4311 = getelementptr [2 x %NxVal], ptr %t4309, i64 0, i64 0
  store %NxVal %t4310, ptr %t4311
  %t4312 = call %NxVal @nx_int(i64 %t4308)
  %t4313 = getelementptr [2 x %NxVal], ptr %t4309, i64 0, i64 1
  store %NxVal %t4312, ptr %t4313
  %t4314 = getelementptr [2 x %NxVal], ptr %t4309, i64 0, i64 0
  %t4315 = call %NxVal @nx__f_3____main____f47(ptr %t4314, i64 2)
  %t4316 = extractvalue %NxVal %t4315, 1
  %t4317 = extractvalue %NxVal %t4306, 1
  %t4318 = add i64 %t4317, %t4316
  %t4319 = add i64 65535, 0
  %t4320 = and i64 %t4318, %t4319
  %t4321 = call %NxVal @nx_int(i64 %t4320)
  store %NxVal %t4321, ptr @nx__g___main____total
  %t4322 = load %NxVal, ptr @nx__g___main____total
  %t4323 = add i64 23, 0
  %t4324 = add i64 185, 0
  %t4326 = call %NxVal @nx_int(i64 %t4323)
  %t4327 = getelementptr [2 x %NxVal], ptr %t4325, i64 0, i64 0
  store %NxVal %t4326, ptr %t4327
  %t4328 = call %NxVal @nx_int(i64 %t4324)
  %t4329 = getelementptr [2 x %NxVal], ptr %t4325, i64 0, i64 1
  store %NxVal %t4328, ptr %t4329
  %t4330 = getelementptr [2 x %NxVal], ptr %t4325, i64 0, i64 0
  %t4331 = call %NxVal @nx__f_3____main____f48(ptr %t4330, i64 2)
  %t4332 = extractvalue %NxVal %t4331, 1
  %t4333 = extractvalue %NxVal %t4322, 1
  %t4334 = add i64 %t4333, %t4332
  %t4335 = add i64 65535, 0
  %t4336 = and i64 %t4334, %t4335
  %t4337 = call %NxVal @nx_int(i64 %t4336)
  store %NxVal %t4337, ptr @nx__g___main____total
  %t4338 = load %NxVal, ptr @nx__g___main____total
  %t4339 = add i64 446, 0
  %t4340 = add i64 372, 0
  %t4342 = call %NxVal @nx_int(i64 %t4339)
  %t4343 = getelementptr [2 x %NxVal], ptr %t4341, i64 0, i64 0
  store %NxVal %t4342, ptr %t4343
  %t4344 = call %NxVal @nx_int(i64 %t4340)
  %t4345 = getelementptr [2 x %NxVal], ptr %t4341, i64 0, i64 1
  store %NxVal %t4344, ptr %t4345
  %t4346 = getelementptr [2 x %NxVal], ptr %t4341, i64 0, i64 0
  %t4347 = call %NxVal @nx__f_3____main____f49(ptr %t4346, i64 2)
  %t4348 = extractvalue %NxVal %t4347, 1
  %t4349 = extractvalue %NxVal %t4338, 1
  %t4350 = add i64 %t4349, %t4348
  %t4351 = add i64 65535, 0
  %t4352 = and i64 %t4350, %t4351
  %t4353 = call %NxVal @nx_int(i64 %t4352)
  store %NxVal %t4353, ptr @nx__g___main____total
  %t4355 = load %NxVal, ptr @nx__g___main____total
  %t4356 = getelementptr [1 x %NxVal], ptr %t4354, i64 0, i64 0
  store %NxVal %t4355, ptr %t4356
  %t4357 = getelementptr [1 x %NxVal], ptr %t4354, i64 0, i64 0
  call void @nx_print(ptr %t4357, i64 1)
  ret void
initskip102:
  ret void
}
define i32 @main() {
entry:
  call void @nx__init___main__()
  ret i32 0
}
