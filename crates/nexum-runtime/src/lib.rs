//! Nexum runtime support (bootstrap).
//!
//! This crate emits the runtime module as LLVM IR text. The compiler links it
//! into every program as `internal` definitions, so no separate runtime build
//! step or archive is required on the bootstrap toolchain path.
//!
//! What lives here (Phase 4: memory correctness):
//!
//! * the panic primitive (`nexum_panic`),
//! * the managed heap with a precise, non-moving mark-sweep collector
//!   (`nexum_alloc`, `nexum_collect`, `nexum_gc_maybe`),
//! * root-frame registration for compiler-managed roots
//!   (`nexum_roots_push`, `nexum_roots_pop`),
//! * string helpers (`nexum_concat`) and heap statistics (`nexum_heap_used`).
//!
//! Layout conventions are pinned in `docs/runtime-abi.md`:
//!
//! * Object block (allocated with `malloc`):
//!   `[0]` size of the payload rounded up to 16 (i64),
//!   `[8]` mark bit (i64), `[16]` next object in the global list (ptr),
//!   `[24]` reserved (i64); payload starts at `+32` and is 16-byte aligned.
//! * Root frame (allocated by the compiler on the function's stack):
//!   `[0]` previous frame (ptr), `[8]` slots array (ptr of ptr),
//!   `[16]` slot count (i64).
//! * A root slot holds `null`, a pointer to a static string literal, or the
//!   payload pointer of a heap object. Marking compares payload pointers, so
//!   static literals never need heap metadata and never enter the heap list.
//!
//! Collector policy (bootstrap): non-moving, precise, single-threaded. A
//! collection runs only inside `nexum_alloc` when the allocation watermark is
//! exceeded, or when a program calls the `collect()` builtin. Because the
//! collector never moves objects, values held in registers across a
//! collection stay valid as long as the object is reachable from a root slot.

pub fn runtime_ir(msvc: bool) -> String {
    let mut s = String::new();
    s.push_str("declare i32 @printf(ptr, ...)\n");
    s.push_str("declare i32 @fputs(ptr, ptr)\n");
    s.push_str("declare i32 @fputc(i32, ptr)\n");
    s.push_str("declare void @exit(i32)\n");
    s.push_str("declare ptr @malloc(i64)\n");
    s.push_str("declare void @free(ptr)\n");
    s.push_str("declare i64 @strlen(ptr)\n");
    s.push_str("declare ptr @memcpy(ptr, ptr, i64)\n");
    s.push_str("declare i32 @strcmp(ptr, ptr)\n");
    if msvc {
        // The UCRT exposes stdio streams through __acrt_iob_func; the classic
        // `stderr` global only exists on non-Windows targets.
        s.push_str("declare ptr @__acrt_iob_func(i32)\n\n");
    } else {
        s.push_str("@stderr = external global ptr\n\n");
    }

    s.push_str("@nexum_obj_head = internal global ptr null, align 8\n");
    s.push_str("@nexum_roots_head = internal global ptr null, align 8\n");
    s.push_str("@nexum_live = internal global i64 0, align 8\n");
    s.push_str("@nexum_since = internal global i64 0, align 8\n");
    s.push_str("@nexum_threshold = internal global i64 65536, align 8\n");
    s.push_str("@nexum_msg_oom = private unnamed_addr constant [14 x i8] c\"out of memory\\00\"\n");
    s.push_str(
        "@nexum_msg_oob = private unnamed_addr constant [20 x i8] c\"index out of bounds\\00\"\n\n",
    );

    // panic: message + newline to stderr, then exit 1.
    if msvc {
        s.push_str(
            "define internal void @nexum_panic(ptr %msg) {\n  %err = call ptr @__acrt_iob_func(i32 2)\n  %f = call i32 @fputs(ptr %msg, ptr %err)\n  %n = call i32 @fputc(i32 10, ptr %err)\n  call void @exit(i32 1)\n  unreachable\n}\n\n",
        );
    } else {
        s.push_str(
            "define internal void @nexum_panic(ptr %msg) {\n  %err = load ptr, ptr @stderr\n  %f = call i32 @fputs(ptr %msg, ptr %err)\n  %n = call i32 @fputc(i32 10, ptr %err)\n  call void @exit(i32 1)\n  unreachable\n}\n\n",
        );
    }

    // out-of-memory handler.
    s.push_str(
        "define internal void @nexum_oom() {\nentry:\n  call void @nexum_panic(ptr @nexum_msg_oom)\n  unreachable\n}\n\n",
    );

    // Root-frame registration. The compiler builds the frame in its own stack
    // frame and calls push at function entry / pop before every return.
    s.push_str(
        "define internal void @nexum_roots_push(ptr %frame) {\nentry:\n  %h = load ptr, ptr @nexum_roots_head\n  store ptr %h, ptr %frame\n  store ptr %frame, ptr @nexum_roots_head\n  ret void\n}\n\n",
    );
    s.push_str(
        "define internal void @nexum_roots_pop() {\nentry:\n  %h = load ptr, ptr @nexum_roots_head\n  %p = load ptr, ptr %h\n  store ptr %p, ptr @nexum_roots_head\n  ret void\n}\n\n",
    );

    // Allocation. Rounds the payload to 16 bytes, maybe collects, links the
    // new block at the head of the object list and returns the payload.
    s.push_str(
        "define internal ptr @nexum_alloc(i64 %payload) {\nentry:\n  %sz0 = add i64 %payload, 15\n  %sz = and i64 %sz0, -16\n  %total = add i64 %sz, 32\n  call void @nexum_gc_maybe()\n  %p = call ptr @malloc(i64 %total)\n  %isnull = icmp eq ptr %p, null\n  br i1 %isnull, label %oom, label %ok\noom:\n  call void @nexum_oom()\n  unreachable\nok:\n  store i64 %sz, ptr %p\n  %mp = getelementptr i8, ptr %p, i64 8\n  store i64 0, ptr %mp\n  %np = getelementptr i8, ptr %p, i64 16\n  %head = load ptr, ptr @nexum_obj_head\n  store ptr %head, ptr %np\n  store ptr %p, ptr @nexum_obj_head\n  %live = load i64, ptr @nexum_live\n  %live2 = add i64 %live, %sz\n  store i64 %live2, ptr @nexum_live\n  %since = load i64, ptr @nexum_since\n  %since2 = add i64 %since, %sz\n  store i64 %since2, ptr @nexum_since\n  %pay = getelementptr i8, ptr %p, i64 32\n  ret ptr %pay\n}\n\n",
    );

    // Watermark-triggered collection.
    s.push_str(
        "define internal void @nexum_gc_maybe() {\nentry:\n  %v = load i64, ptr @nexum_since\n  %t = load i64, ptr @nexum_threshold\n  %c = icmp ugt i64 %v, %t\n  br i1 %c, label %do, label %out\ndo:\n  call void @nexum_collect()\n  br label %out\nout:\n  ret void\n}\n\n",
    );

    // Collection: mark from every root slot of every registered frame, then
    // sweep the object list.
    s.push_str(
        "define internal void @nexum_collect() {\nentry:\n  store i64 0, ptr @nexum_since\n  %fvar = alloca ptr, align 8\n  %ivar = alloca i64, align 8\n  %head = load ptr, ptr @nexum_roots_head\n  store ptr %head, ptr %fvar\n  br label %frameloop\nframeloop:\n  %f = load ptr, ptr %fvar\n  %fnull = icmp eq ptr %f, null\n  br i1 %fnull, label %framedone, label %framebody\nframebody:\n  %sp = getelementptr i8, ptr %f, i64 8\n  %slots = load ptr, ptr %sp\n  %cp = getelementptr i8, ptr %f, i64 16\n  %cnt = load i64, ptr %cp\n  store i64 0, ptr %ivar\n  br label %slotloop\nslotloop:\n  %i = load i64, ptr %ivar\n  %sc = icmp ult i64 %i, %cnt\n  br i1 %sc, label %slotbody, label %slotdone\nslotbody:\n  %eltp = getelementptr ptr, ptr %slots, i64 %i\n  %val = load ptr, ptr %eltp\n  %vnull = icmp eq ptr %val, null\n  br i1 %vnull, label %slotnext, label %slotmark\nslotmark:\n  call void @nexum_mark(ptr %val)\n  br label %slotnext\nslotnext:\n  %i2 = add i64 %i, 1\n  store i64 %i2, ptr %ivar\n  br label %slotloop\nslotdone:\n  %prev = load ptr, ptr %f\n  store ptr %prev, ptr %fvar\n  br label %frameloop\nframedone:\n  call void @nexum_sweep()\n  ret void\n}\n\n",
    );

    // Mark a payload: linear search of the object list (bootstrap: the object
    // graph is flat, and static literals simply never match).
    //
    // The argument is a pointer to a ROOT SLOT, not to the payload itself, so
    // that the (possibly changing) contents of the slot are read at collection
    // time. A null slot contents is a no-op.
    s.push_str(
    "define internal void @nexum_mark(ptr %slot) {\nentry:\n  %pay = load ptr, ptr %slot\n  %pnull = icmp eq ptr %pay, null\n  br i1 %pnull, label %markdone, label %walk\nwalk:\n  %ovar = alloca ptr, align 8\n  %head = load ptr, ptr @nexum_obj_head\n  store ptr %head, ptr %ovar\n  br label %objloop\nobjloop:\n  %o = load ptr, ptr %ovar\n  %onull = icmp eq ptr %o, null\n  br i1 %onull, label %markdone, label %objbody\nobjbody:\n  %payp = getelementptr i8, ptr %o, i64 32\n  %eq = icmp eq ptr %payp, %pay\n  br i1 %eq, label %objhit, label %objmiss\nobjhit:\n  %mp = getelementptr i8, ptr %o, i64 8\n  store i64 1, ptr %mp\n  br label %markdone\nobjmiss:\n  %np = getelementptr i8, ptr %o, i64 16\n  %nxt = load ptr, ptr %np\n  store ptr %nxt, ptr %ovar\n  br label %objloop\nmarkdone:\n  ret void\n}\n\n",
);

    // Sweep: unmarked objects are freed and unlinked; marked objects are kept
    // and their mark bit is reset for the next cycle.
    s.push_str(
        "define internal void @nexum_sweep() {\nentry:\n  %pvar = alloca ptr, align 8\n  %cvar = alloca ptr, align 8\n  store ptr null, ptr %pvar\n  %head = load ptr, ptr @nexum_obj_head\n  store ptr %head, ptr %cvar\n  br label %sloop\nsloop:\n  %cur = load ptr, ptr %cvar\n  %cnull = icmp eq ptr %cur, null\n  br i1 %cnull, label %sdone, label %sbody\nsbody:\n  %mp = getelementptr i8, ptr %cur, i64 8\n  %m = load i64, ptr %mp\n  %alive = icmp ne i64 %m, 0\n  br i1 %alive, label %keep, label %drop\nkeep:\n  store i64 0, ptr %mp\n  store ptr %cur, ptr %pvar\n  %np = getelementptr i8, ptr %cur, i64 16\n  %nxt = load ptr, ptr %np\n  store ptr %nxt, ptr %cvar\n  br label %sloop\ndrop:\n  %dsp = getelementptr i8, ptr %cur, i64 0\n  %sz = load i64, ptr %dsp\n  %live = load i64, ptr @nexum_live\n  %live2 = sub i64 %live, %sz\n  store i64 %live2, ptr @nexum_live\n  %dnp = getelementptr i8, ptr %cur, i64 16\n  %dnxt = load ptr, ptr %dnp\n  %pr = load ptr, ptr %pvar\n  %pnull = icmp eq ptr %pr, null\n  br i1 %pnull, label %sethead, label %setprev\nsethead:\n  store ptr %dnxt, ptr @nexum_obj_head\n  br label %advance\nsetprev:\n  %pnp = getelementptr i8, ptr %pr, i64 16\n  store ptr %dnxt, ptr %pnp\n  br label %advance\nadvance:\n  store ptr %dnxt, ptr %cvar\n  call void @free(ptr %cur)\n  br label %sloop\nsdone:\n  ret void\n}\n\n",
    );

    // String concatenation: fresh NUL-terminated buffer on the managed heap.
    s.push_str(
        "define internal ptr @nexum_concat(ptr %a, ptr %b) {\nentry:\n  %la = call i64 @strlen(ptr %a)\n  %lb = call i64 @strlen(ptr %b)\n  %sum = add i64 %la, %lb\n  %n1 = add i64 %sum, 1\n  %buf = call ptr @nexum_alloc(i64 %n1)\n  call ptr @memcpy(ptr %buf, ptr %a, i64 %la)\n  %dst = getelementptr i8, ptr %buf, i64 %la\n  %n2 = add i64 %lb, 1\n  call ptr @memcpy(ptr %dst, ptr %b, i64 %n2)\n  ret ptr %buf\n}\n\n",
    );

    // String indexing: returns a one-character NUL-terminated string.
    s.push_str(
        "define internal ptr @nexum_str_char_at(ptr %s, i64 %i) {\nentry:\n  %n = call i64 @strlen(ptr %s)\n  %neg = icmp slt i64 %i, 0\n  %oob = icmp sge i64 %i, %n\n  %bad = or i1 %neg, %oob\n  br i1 %bad, label %fail, label %ok\nfail:\n  call void @nexum_panic(ptr @nexum_msg_oob)\n  unreachable\nok:\n  %buf = call ptr @nexum_alloc(i64 1)\n  %srcp = getelementptr i8, ptr %s, i64 %i\n  %ch = load i8, ptr %srcp\n  store i8 %ch, ptr %buf\n  %nulp = getelementptr i8, ptr %buf, i64 1\n  store i8 0, ptr %nulp\n  ret ptr %buf\n}\n\n",
    );

    // Live payload bytes (payload rounded up to 16, header excluded).
    s.push_str(
        "define internal i64 @nexum_heap_used() {\nentry:\n  %v = load i64, ptr @nexum_live\n  ret i64 %v\n}\n\n",
    );

    s
}
