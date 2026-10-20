//! Nexum runtime support (bootstrap).
//!
//! This crate emits the runtime module as LLVM IR text. The compiler links it
//! into every program as `internal` definitions, so no separate runtime build
//! step or archive is required on the bootstrap toolchain path.
//!
//! What lives here (Phase 4 memory correctness, Phase 10 GC tracing model):
//!
//! * the panic primitive (`nexum_panic`),
//! * the managed heap with a non-moving mark-sweep collector
//!   (`nexum_alloc`, `nexum_collect`, `nexum_gc_maybe`),
//! * root-frame registration for compiler-managed roots
//!   (`nexum_roots_push`, `nexum_roots_pop`),
//! * string helpers (`nexum_concat`, `nexum_slice`, `nexum_str_char_at`),
//! * file I/O helpers (`nexum_read_file`, `nexum_write_file`),
//! * the growable `List` collection (`nexum_list_*`).
//!
//! Layout conventions are pinned in `docs/runtime-abi.md` and the tracing
//! model is decided in `docs/language-reference/decisions-gc.md`:
//!
//! * Object block (allocated with `malloc`):
//!   `[0]` size of the payload rounded up to 16 (i64),
//!   `[8]` mark bit (i64), `[16]` next object in the global list (ptr),
//!   `[24]` number of payload bytes to scan for interior references (i64);
//!   the payload starts at `+32` and is 16-byte aligned.
//! * Root frame (allocated by the compiler on the function's stack):
//!   `[0]` previous frame (ptr), `[8]` entries array (ptr),
//!   `[16]` entry count (i64).
//! * Root entry: `[0]` address of the root (ptr), `[8]` bytes to scan (i64).
//!   A `Str` local scans 8 bytes; a record or array that contains references
//!   scans its whole size; a numeric local scans 0 bytes and is not
//!   registered at all.
//!
//! IMPORTANT: `nexum_alloc` and `nexum_alloc_scanned` return the *payload*
//! pointer (header + 32), not the header. Callers must not add the header
//! offset again.
//!
//! Collector policy: non-moving, single-threaded, mostly-precise. Roots are
//! described exactly by the compiler; the *contents* of an aggregate are
//! scanned conservatively. A collection runs only inside an allocation when
//! the watermark is exceeded, or when a program calls the `collect()` builtin.

pub mod bytes;
pub mod clock;
pub mod format;
pub mod heap;
pub mod math;
pub mod strings;
pub mod syscalls;

pub fn runtime_ir(os: &str, arch: &str) -> String {
    let mut s = String::new();
    let msvc = os.contains("windows");
    // The output formatting is ours on the POSIX targets, so the C
    // library's stdio is not needed there. Windows still uses it.
    if msvc {
        s.push_str("declare i32 @printf(ptr, ...)\n");
        s.push_str("declare i32 @fputs(ptr, ptr)\n");
        s.push_str("declare i32 @fputc(i32, ptr)\n");
        s.push_str("declare void @exit(i32)\n");
    }

    // The string routines are ours on every platform: a program should not
    // need the C library for a byte copy.
    s.push_str(&strings::string_routines());
    s.push_str(&heap::allocator_ir(os, arch));
    s.push_str(&bytes::byte_routines());
    // The clock reaches the system through the call layer, and Windows has
    // no call layer yet, so the clock is POSIX only for now.
    if !msvc {
        s.push_str(&clock::clock_routines());
    }
    s.push_str(&math::math_routines());
    s.push_str(&format::format_routines(!msvc));

    if msvc {
        // The UCRT exposes stdio streams through __acrt_iob_func; the classic
        // `stderr` global only exists on non-Windows targets.
        s.push_str("declare ptr @__acrt_iob_func(i32)\n\n");
        // Windows has no direct system call instruction: its kernel
        // interface lives behind ntdll. The file routines still go through
        // the C library until the system call stubs exist (phase 11a.2).
        s.push_str("declare ptr @fopen(ptr, ptr)\n");
        s.push_str("declare i32 @fclose(ptr)\n");
        s.push_str("declare i32 @fseek(ptr, i64, i32)\n");
        s.push_str("declare i64 @ftell(ptr)\n");
        s.push_str("declare i64 @fread(ptr, i64, i64, ptr)\n");
        s.push_str("declare i64 @fwrite(ptr, i64, i64, ptr)\n\n");
        s.push_str("@nexum_mode_r = private unnamed_addr constant [3 x i8] c\"rb\\00\"\n");
        s.push_str("@nexum_mode_w = private unnamed_addr constant [3 x i8] c\"wb\\00\"\n\n");
    } else {
        // The system call layer: the program reaches the kernel through it,
        // and not through the C library. The error stream is written to
        // through it as well: macOS does not export `stderr` as a global,
        // and the C library accessor for it differs between the systems.
        let sys_os = if os.contains("macos") {
            syscalls::SysOs::Macos
        } else {
            syscalls::SysOs::Linux
        };
        let sys_arch = if arch.contains("aarch64") {
            syscalls::SysArch::Aarch64
        } else {
            syscalls::SysArch::X86_64
        };
        let numbers = syscalls::Syscalls::for_os(sys_os);
        s.push_str(&syscalls::wrappers(sys_os, sys_arch, &numbers));
        s.push_str(&strings::file_routines(
            true,
            numbers.open_write,
            numbers.open_mode,
        ));
    }
    if msvc {
        s.push_str(&strings::file_routines(false, 0, 0));
    }

    s.push_str("%nexum.root_frame = type { ptr, ptr, i64 }\n");
    s.push_str("%nexum.root_entry = type { ptr, i64 }\n\n");
    s.push_str("@nexum_obj_head = internal global ptr null, align 8\n");
    s.push_str("@nexum_roots_head = internal global ptr null, align 8\n");
    s.push_str("@nexum_live = internal global i64 0, align 8\n");
    s.push_str("@nexum_since = internal global i64 0, align 8\n");
    s.push_str("@nexum_threshold = internal global i64 65536, align 8\n");
    s.push_str("@nexum_msg_oom = private unnamed_addr constant [14 x i8] c\"out of memory\\00\"\n");
    s.push_str(
        "@nexum_msg_oob = private unnamed_addr constant [20 x i8] c\"index out of bounds\\00\"\n",
    );
    s.push_str("@nexum_nl = private unnamed_addr constant [2 x i8] c\"\\0A\\00\"\n\n");

    // panic: message + newline to stderr, then exit 1.
    if msvc {
        s.push_str(
            "define internal void @nexum_panic(ptr %msg) {\n  %err = call ptr @__acrt_iob_func(i32 2)\n  %f = call i32 @fputs(ptr %msg, ptr %err)\n  %n = call i32 @fputc(i32 10, ptr %err)\n  call void @exit(i32 1)\n  unreachable\n}\n\n",
        );
    } else {
        s.push_str(
            "define internal void @nexum_panic(ptr %msg) {\n  %len = call i64 @nexum_strlen(ptr %msg)\n  %msgi = ptrtoint ptr %msg to i64
  %w = call i64 @nexum_sys_write(i64 2, i64 %msgi, i64 %len)\n  %nli = ptrtoint ptr @nexum_nl to i64
  %n = call i64 @nexum_sys_write(i64 2, i64 %nli, i64 1)\n  call void @nexum_sys_exit(i64 1)\n  unreachable\n}\n\n",
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

    // Allocation. Every object's *whole* payload is scanned for interior
    // references by the collector, so the scan size is the payload size; the
    // `scan_bytes` argument is kept for callers that know better and is
    // currently the same value.
    s.push_str(
        "define internal ptr @nexum_alloc_scanned(i64 %payload, i64 %scan_bytes) {\nentry:\n  %sz0 = add i64 %payload, 15\n  %sz = and i64 %sz0, -16\n  %total = add i64 %sz, 32\n  call void @nexum_gc_maybe()\n  %p = call ptr @nexum_heap_alloc(i64 %total)\n  %isnull = icmp eq ptr %p, null\n  br i1 %isnull, label %oom, label %ok\noom:\n  call void @nexum_oom()\n  unreachable\nok:\n  store i64 %sz, ptr %p\n  %mp = getelementptr i8, ptr %p, i64 8\n  store i64 0, ptr %mp\n  %np = getelementptr i8, ptr %p, i64 16\n  %head = load ptr, ptr @nexum_obj_head\n  store ptr %head, ptr %np\n  store ptr %p, ptr @nexum_obj_head\n  %sbp = getelementptr i8, ptr %p, i64 24\n  store i64 %scan_bytes, ptr %sbp\n  %live = load i64, ptr @nexum_live\n  %live2 = add i64 %live, %sz\n  store i64 %live2, ptr @nexum_live\n  %since = load i64, ptr @nexum_since\n  %since2 = add i64 %since, %sz\n  store i64 %since2, ptr @nexum_since\n  %pay = getelementptr i8, ptr %p, i64 32\n  ret ptr %pay\n}\n\n",
    );
    s.push_str(
        "define internal ptr @nexum_alloc(i64 %payload) {\nentry:\n  %p = call ptr @nexum_alloc_scanned(i64 %payload, i64 0)\n  ret ptr %p\n}\n\n",
    );

    // Watermark-triggered collection.
    s.push_str(
        "define internal void @nexum_gc_maybe() {\nentry:\n  %v = load i64, ptr @nexum_since\n  %t = load i64, ptr @nexum_threshold\n  %c = icmp ugt i64 %v, %t\n  br i1 %c, label %do, label %out\ndo:\n  call void @nexum_collect()\n  br label %out\nout:\n  ret void\n}\n\n",
    );

    // Mark one candidate pointer. Returns 1 if a previously unmarked object
    // was reached. A candidate that does not match any live payload (a static
    // literal, a small integer that looks like an address) is ignored.
    s.push_str(
        "define internal i1 @nexum_mark_ptr(ptr %pay) {\nentry:\n  %res = alloca i64, align 8\n  %ovar = alloca ptr, align 8\n  store i64 0, ptr %res\n  %pnull = icmp eq ptr %pay, null\n  br i1 %pnull, label %markdone, label %walk\nwalk:\n  %head = load ptr, ptr @nexum_obj_head\n  store ptr %head, ptr %ovar\n  br label %objloop\nobjloop:\n  %o = load ptr, ptr %ovar\n  %onull = icmp eq ptr %o, null\n  br i1 %onull, label %markdone, label %objbody\nobjbody:\n  %payp = getelementptr i8, ptr %o, i64 32\n  %eq = icmp eq ptr %payp, %pay\n  br i1 %eq, label %objhit, label %objmiss\nobjhit:\n  %mp = getelementptr i8, ptr %o, i64 8\n  %m = load i64, ptr %mp\n  %already = icmp ne i64 %m, 0\n  store i64 1, ptr %mp\n  %fresh = xor i1 %already, true\n  %az = zext i1 %fresh to i64\n  store i64 %az, ptr %res\n  br label %markdone\nobjmiss:\n  %np = getelementptr i8, ptr %o, i64 16\n  %nxt = load ptr, ptr %np\n  store ptr %nxt, ptr %ovar\n  br label %objloop\nmarkdone:\n  %r = load i64, ptr %res\n  %rb = trunc i64 %r to i1\n  ret i1 %rb\n}\n\n",
    );

    // Scan a byte range as a sequence of candidate pointers. Returns 1 if any
    // object was newly marked.
    s.push_str(
        "define internal i1 @nexum_scan_range(ptr %base, i64 %bytes) {\nentry:\n  %words = lshr i64 %bytes, 3\n  %ivar = alloca i64, align 8\n  %any = alloca i64, align 8\n  store i64 0, ptr %ivar\n  store i64 0, ptr %any\n  br label %loop\nloop:\n  %i = load i64, ptr %ivar\n  %c = icmp ult i64 %i, %words\n  br i1 %c, label %body, label %done\nbody:\n  %p = getelementptr i64, ptr %base, i64 %i\n  %w = load i64, ptr %p\n  %wp = inttoptr i64 %w to ptr\n  %m = call i1 @nexum_mark_ptr(ptr %wp)\n  %mz = zext i1 %m to i64\n  %av = load i64, ptr %any\n  %a2 = or i64 %av, %mz\n  store i64 %a2, ptr %any\n  %i2 = add i64 %i, 1\n  store i64 %i2, ptr %ivar\n  br label %loop\ndone:\n  %a = load i64, ptr %any\n  %ab = trunc i64 %a to i1\n  ret i1 %ab\n}\n\n",
    );

    // Collection: mark from every root entry of every registered frame, then
    // trace the heap to a fixpoint, then sweep.
    s.push_str(
        "define internal void @nexum_collect() {\nentry:\n  store i64 0, ptr @nexum_since\n  %fvar = alloca ptr, align 8\n  %ivar = alloca i64, align 8\n  %head = load ptr, ptr @nexum_roots_head\n  store ptr %head, ptr %fvar\n  br label %frameloop\nframeloop:\n  %f = load ptr, ptr %fvar\n  %fnull = icmp eq ptr %f, null\n  br i1 %fnull, label %framedone, label %framebody\nframebody:\n  %ep = getelementptr i8, ptr %f, i64 8\n  %entries = load ptr, ptr %ep\n  %cp = getelementptr i8, ptr %f, i64 16\n  %cnt = load i64, ptr %cp\n  store i64 0, ptr %ivar\n  br label %entryloop\nentryloop:\n  %i = load i64, ptr %ivar\n  %sc = icmp ult i64 %i, %cnt\n  br i1 %sc, label %entrybody, label %entrydone\nentrybody:\n  %eoff = mul i64 %i, 16\n  %ep2 = getelementptr i8, ptr %entries, i64 %eoff\n  %addrp = getelementptr i8, ptr %ep2, i64 0\n  %addr = load ptr, ptr %addrp\n  %sbp = getelementptr i8, ptr %ep2, i64 8\n  %sb = load i64, ptr %sbp\n  %sbz = icmp sgt i64 %sb, 0\n  br i1 %sbz, label %scanit, label %entrynext\nscanit:\n  call i1 @nexum_scan_range(ptr %addr, i64 %sb)\n  br label %entrynext\nentrynext:\n  %i2 = add i64 %i, 1\n  store i64 %i2, ptr %ivar\n  br label %entryloop\nentrydone:\n  %prev = load ptr, ptr %f\n  store ptr %prev, ptr %fvar\n  br label %frameloop\nframedone:\n  call void @nexum_gc_trace()\n  call void @nexum_sweep()\n  ret void\n}\n\n",
    );

    // Trace the heap to a fixpoint: a marked object can reference further
    // objects, which can reference further objects.
    s.push_str(
        "define internal void @nexum_gc_trace() {\nentry:\n  %changed = alloca i64, align 8\n  %ovar = alloca ptr, align 8\n  store i64 1, ptr %changed\n  br label %outer\nouter:\n  %ch = load i64, ptr %changed\n  %cont = icmp ne i64 %ch, 0\n  br i1 %cont, label %inner, label %done\ninner:\n  store i64 0, ptr %changed\n  %head = load ptr, ptr @nexum_obj_head\n  store ptr %head, ptr %ovar\n  br label %objloop\nobjloop:\n  %o = load ptr, ptr %ovar\n  %onull = icmp eq ptr %o, null\n  br i1 %onull, label %outer, label %objbody\nobjbody:\n  %mp = getelementptr i8, ptr %o, i64 8\n  %m = load i64, ptr %mp\n  %alive = icmp ne i64 %m, 0\n  br i1 %alive, label %scan, label %next\nscan:\n  %sbp = getelementptr i8, ptr %o, i64 24\n  %sb = load i64, ptr %sbp\n  %payp = getelementptr i8, ptr %o, i64 32\n  %r = call i1 @nexum_scan_range(ptr %payp, i64 %sb)\n  %rz = zext i1 %r to i64\n  %cv = load i64, ptr %changed\n  %c2 = or i64 %cv, %rz\n  store i64 %c2, ptr %changed\n  br label %next\nnext:\n  %np = getelementptr i8, ptr %o, i64 16\n  %nxt = load ptr, ptr %np\n  store ptr %nxt, ptr %ovar\n  br label %objloop\ndone:\n  ret void\n}\n\n",
    );

    // Sweep: unmarked objects are freed and unlinked; marked objects are kept
    // and their mark bit is reset for the next cycle.
    s.push_str(
        "define internal void @nexum_sweep() {\nentry:\n  %pvar = alloca ptr, align 8\n  %cvar = alloca ptr, align 8\n  store ptr null, ptr %pvar\n  %head = load ptr, ptr @nexum_obj_head\n  store ptr %head, ptr %cvar\n  br label %sloop\nsloop:\n  %cur = load ptr, ptr %cvar\n  %cnull = icmp eq ptr %cur, null\n  br i1 %cnull, label %sdone, label %sbody\nsbody:\n  %mp = getelementptr i8, ptr %cur, i64 8\n  %m = load i64, ptr %mp\n  %alive = icmp ne i64 %m, 0\n  br i1 %alive, label %keep, label %drop\nkeep:\n  store i64 0, ptr %mp\n  store ptr %cur, ptr %pvar\n  %np = getelementptr i8, ptr %cur, i64 16\n  %nxt = load ptr, ptr %np\n  store ptr %nxt, ptr %cvar\n  br label %sloop\ndrop:\n  %dsp = getelementptr i8, ptr %cur, i64 0\n  %sz = load i64, ptr %dsp\n  %live = load i64, ptr @nexum_live\n  %live2 = sub i64 %live, %sz\n  store i64 %live2, ptr @nexum_live\n  %dnp = getelementptr i8, ptr %cur, i64 16\n  %dnxt = load ptr, ptr %dnp\n  %pr = load ptr, ptr %pvar\n  %pnull = icmp eq ptr %pr, null\n  br i1 %pnull, label %sethead, label %setprev\nsethead:\n  store ptr %dnxt, ptr @nexum_obj_head\n  br label %advance\nsetprev:\n  %pnp = getelementptr i8, ptr %pr, i64 16\n  store ptr %dnxt, ptr %pnp\n  br label %advance\nadvance:\n  store ptr %dnxt, ptr %cvar\n  %tot = add i64 %sz, 32\n  call void @nexum_heap_free(ptr %cur, i64 %tot)\n  br label %sloop\nsdone:\n  ret void\n}\n\n",
    );

    // String concatenation: fresh NUL-terminated buffer on the managed heap.
    s.push_str(
        "define internal ptr @nexum_concat(ptr %a, ptr %b) {\nentry:\n  %la = call i64 @nexum_strlen(ptr %a)\n  %lb = call i64 @nexum_strlen(ptr %b)\n  %sum = add i64 %la, %lb\n  %n1 = add i64 %sum, 1\n  %buf = call ptr @nexum_alloc(i64 %n1)\n  call ptr @nexum_memcpy(ptr %buf, ptr %a, i64 %la)\n  %dst = getelementptr i8, ptr %buf, i64 %la\n  %n2 = add i64 %lb, 1\n  call ptr @nexum_memcpy(ptr %dst, ptr %b, i64 %n2)\n  ret ptr %buf\n}\n\n",
    );

    // String indexing: returns a one-character NUL-terminated string.
    s.push_str(
        "define internal ptr @nexum_str_char_at(ptr %s, i64 %i) {\nentry:\n  %n = call i64 @nexum_strlen(ptr %s)\n  %neg = icmp slt i64 %i, 0\n  %oob = icmp sge i64 %i, %n\n  %bad = or i1 %neg, %oob\n  br i1 %bad, label %fail, label %ok\nfail:\n  call void @nexum_panic(ptr @nexum_msg_oob)\n  unreachable\nok:\n  %buf = call ptr @nexum_alloc(i64 1)\n  %srcp = getelementptr i8, ptr %s, i64 %i\n  %ch = load i8, ptr %srcp\n  store i8 %ch, ptr %buf\n  %nulp = getelementptr i8, ptr %buf, i64 1\n  store i8 0, ptr %nulp\n  ret ptr %buf\n}\n\n",
    );

    // String slicing: s[lo..hi] (lo inclusive, hi exclusive), NUL-terminated.
    s.push_str(
        "define internal ptr @nexum_slice(ptr %s, i64 %lo, i64 %hi) {\nentry:\n  %n = call i64 @nexum_strlen(ptr %s)\n  %neglo = icmp slt i64 %lo, 0\n  %neghi = icmp slt i64 %hi, 0\n  %hioob = icmp sgt i64 %hi, %n\n  %lohi = icmp sgt i64 %lo, %hi\n  %bad1 = or i1 %neglo, %neghi\n  %bad2 = or i1 %hioob, %lohi\n  %bad = or i1 %bad1, %bad2\n  br i1 %bad, label %fail, label %ok\nfail:\n  call void @nexum_panic(ptr @nexum_msg_oob)\n  unreachable\nok:\n  %len = sub i64 %hi, %lo\n  %n1 = add i64 %len, 1\n  %buf = call ptr @nexum_alloc(i64 %n1)\n  %srcp = getelementptr i8, ptr %s, i64 %lo\n  call ptr @nexum_memcpy(ptr %buf, ptr %srcp, i64 %len)\n  %nulp = getelementptr i8, ptr %buf, i64 %len\n  store i8 0, ptr %nulp\n  ret ptr %buf\n}\n\n",
    );

    // File I/O helpers.
    s.push_str(
        "@nexum_msg_notfound = private unnamed_addr constant [15 x i8] c\"file not found\\00\"\n\n",
    );
    // ---- growable list ----------------------------------------------------
    //
    // A list is a heap object whose payload is
    //   [0] element buffer (ptr), [8] length (i64), [16] capacity (i64),
    //   [24] element size (i64), [32] element scan size (i64).
    // Only the buffer pointer is a reference, so the list object itself scans
    // 8 bytes. The element buffer is a separate object so that growing the
    // list never moves the list object (the user-visible handle stays valid).
    // The list handle is the payload pointer returned by `nexum_list_new`.
    s.push_str(
        "define internal ptr @nexum_list_new() {\nentry:\n  %lp = call ptr @nexum_alloc_scanned(i64 48, i64 8)\n  store ptr null, ptr %lp\n  %lenp = getelementptr i8, ptr %lp, i64 8\n  store i64 0, ptr %lenp\n  %capp = getelementptr i8, ptr %lp, i64 16\n  store i64 0, ptr %capp\n  %esp = getelementptr i8, ptr %lp, i64 24\n  store i64 0, ptr %esp\n  %escp = getelementptr i8, ptr %lp, i64 32\n  store i64 0, ptr %escp\n  ret ptr %lp\n}\n\n",
    );
    s.push_str(
        "define internal i64 @nexum_list_len(ptr %list) {\nentry:\n  %lenp = getelementptr i8, ptr %list, i64 8\n  %len = load i64, ptr %lenp\n  ret i64 %len\n}\n\n",
    );
    s.push_str(
        "define internal void @nexum_list_push(ptr %list, ptr %elem, i64 %elem_size, i64 %elem_scan) {\nentry:\n  %datap = getelementptr i8, ptr %list, i64 0\n  %lenp = getelementptr i8, ptr %list, i64 8\n  %capp = getelementptr i8, ptr %list, i64 16\n  %esp = getelementptr i8, ptr %list, i64 24\n  %escp = getelementptr i8, ptr %list, i64 32\n  %esize = load i64, ptr %esp\n  %iszero = icmp eq i64 %esize, 0\n  br i1 %iszero, label %first, label %growcheck\nfirst:\n  store i64 %elem_size, ptr %esp\n  store i64 %elem_scan, ptr %escp\n  %cap0 = mul i64 %elem_size, 8\n  store i64 %cap0, ptr %capp\n  %noscan = icmp eq i64 %elem_scan, 0\n  %scan0 = select i1 %noscan, i64 0, i64 %cap0\n  %buf0 = call ptr @nexum_alloc_scanned(i64 %cap0, i64 %scan0)\n  store ptr %buf0, ptr %datap\n  br label %append\ngrowcheck:\n  %len = load i64, ptr %lenp\n  %cap = load i64, ptr %capp\n  %used = mul i64 %len, %esize\n  %full = icmp uge i64 %used, %cap\n  br i1 %full, label %grow, label %append\ngrow:\n  %newcap = mul i64 %cap, 2\n  store i64 %newcap, ptr %capp\n  %escan = load i64, ptr %escp\n  %noscan2 = icmp eq i64 %escan, 0\n  %newscan = select i1 %noscan2, i64 0, i64 %newcap\n  %olddata = load ptr, ptr %datap\n  %oldlen = load i64, ptr %lenp\n  %oldbytes = mul i64 %oldlen, %esize\n  %newbuf = call ptr @nexum_alloc_scanned(i64 %newcap, i64 %newscan)\n  call ptr @nexum_memcpy(ptr %newbuf, ptr %olddata, i64 %oldbytes)\n  store ptr %newbuf, ptr %datap\n  br label %append\nappend:\n  %len2 = load i64, ptr %lenp\n  %esize2 = load i64, ptr %esp\n  %data = load ptr, ptr %datap\n  %off = mul i64 %len2, %esize2\n  %dst = getelementptr i8, ptr %data, i64 %off\n  call ptr @nexum_memcpy(ptr %dst, ptr %elem, i64 %esize2)\n  %len3 = add i64 %len2, 1\n  store i64 %len3, ptr %lenp\n  ret void\n}\n\n",
    );
    s.push_str(
        "define internal ptr @nexum_list_get(ptr %list, i64 %i) {\nentry:\n  %lenp = getelementptr i8, ptr %list, i64 8\n  %len = load i64, ptr %lenp\n  %neg = icmp slt i64 %i, 0\n  %oob = icmp sge i64 %i, %len\n  %bad = or i1 %neg, %oob\n  br i1 %bad, label %fail, label %ok\nfail:\n  call void @nexum_panic(ptr @nexum_msg_oob)\n  unreachable\nok:\n  %datap = getelementptr i8, ptr %list, i64 0\n  %data = load ptr, ptr %datap\n  %esp = getelementptr i8, ptr %list, i64 24\n  %esize = load i64, ptr %esp\n  %off = mul i64 %i, %esize\n  %p = getelementptr i8, ptr %data, i64 %off\n  ret ptr %p\n}\n\n",
    );
    s.push_str(
        "define internal void @nexum_list_set(ptr %list, i64 %i, ptr %elem) {\nentry:\n  %lenp = getelementptr i8, ptr %list, i64 8\n  %len = load i64, ptr %lenp\n  %neg = icmp slt i64 %i, 0\n  %oob = icmp sge i64 %i, %len\n  %bad = or i1 %neg, %oob\n  br i1 %bad, label %fail, label %ok\nfail:\n  call void @nexum_panic(ptr @nexum_msg_oob)\n  unreachable\nok:\n  %datap = getelementptr i8, ptr %list, i64 0\n  %data = load ptr, ptr %datap\n  %esp = getelementptr i8, ptr %list, i64 24\n  %esize = load i64, ptr %esp\n  %off = mul i64 %i, %esize\n  %dst = getelementptr i8, ptr %data, i64 %off\n  call ptr @nexum_memcpy(ptr %dst, ptr %elem, i64 %esize)\n  ret void\n}\n\n",
    );

    // Live payload bytes (payload rounded up to 16, header excluded).
    s.push_str(
        "define internal i64 @nexum_heap_used() {\nentry:\n  %v = load i64, ptr @nexum_live\n  ret i64 %v\n}\n\n",
    );

    s
}

#[cfg(test)]
mod tests {
    /// The runtime must not define a symbol the C library also defines.
    ///
    /// A program that links the C library - a hosted build, or a test that
    /// has a C driver - fails to link when two objects define the same
    /// symbol, and the failure appears in somebody else's build, far from the
    /// line that caused it. The maths builtins once did exactly this: they
    /// defined `floor`, `ceil`, `trunc` and `round`.
    ///
    /// So the rule is: every function the runtime defines that is not
    /// internal is named `nexum_` something. The internal ones are local to
    /// the module and cannot collide.
    #[test]
    fn the_runtime_does_not_define_c_library_names() {
        for (os, arch) in [
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("macos", "x86_64"),
            ("macos", "aarch64"),
            ("windows", "x86_64"),
        ] {
            let ir = super::runtime_ir(os, arch);
            for line in ir.lines() {
                let Some(rest) = line.strip_prefix("define ") else {
                    continue;
                };
                // `define internal <ty> @name(` is local to the module.
                if rest.starts_with("internal ") {
                    continue;
                }
                let Some(at) = rest.find('@') else {
                    continue;
                };
                let tail = &rest[at + 1..];
                let Some(open) = tail.find('(') else {
                    continue;
                };
                let name = &tail[..open];
                assert!(
                    name.starts_with("nexum_"),
                    "{os}/{arch}: the runtime defines @{name}, and the C library may define it too"
                );
            }
        }
    }
}