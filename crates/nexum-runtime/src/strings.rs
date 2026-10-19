//! The runtime's own string and file routines.
//!
//! These are written here rather than taken from the C library, so that a
//! Nexum program does not depend on libc. They are the same on every
//! platform: the file routines differ, because the system calls do.

/// The string routines, as LLVM IR.
///
/// They are not `internal`: the code generator calls them for struct copies
/// and string handling, so they have to be visible in the module.
pub fn string_routines() -> String {
    let mut s = String::new();
    // The length of a NUL terminated string.
    s.push_str(
        "define i64 @nexum_strlen(ptr %s) {\nentry:\n  %i = alloca i64\n  store i64 0, ptr %i\n  br label %loop\nloop:\n  %i2 = load i64, ptr %i\n  %p = getelementptr i8, ptr %s, i64 %i2\n  %c = load i8, ptr %p\n  %z = icmp eq i8 %c, 0\n  br i1 %z, label %done, label %body\nbody:\n  %i3 = add i64 %i2, 1\n  store i64 %i3, ptr %i\n  br label %loop\ndone:\n  ret i64 %i2\n}\n\n",
    );
    // A byte copy.
    s.push_str(
        "define ptr @nexum_memcpy(ptr %dst, ptr %src, i64 %n) {\nentry:\n  %i = alloca i64\n  store i64 0, ptr %i\n  br label %loop\nloop:\n  %i2 = load i64, ptr %i\n  %done = icmp uge i64 %i2, %n\n  br i1 %done, label %end, label %body\nbody:\n  %sp = getelementptr i8, ptr %src, i64 %i2\n  %c = load i8, ptr %sp\n  %dp = getelementptr i8, ptr %dst, i64 %i2\n  store i8 %c, ptr %dp\n  %i3 = add i64 %i2, 1\n  store i64 %i3, ptr %i\n  br label %loop\nend:\n  ret ptr %dst\n}\n\n",
    );
    // A comparison, like strcmp: the difference of the first bytes that
    // differ, or zero when the strings are equal.
    s.push_str(
        "define i32 @nexum_strcmp(ptr %a, ptr %b) {\nentry:\n  %i = alloca i64\n  store i64 0, ptr %i\n  br label %loop\nloop:\n  %i2 = load i64, ptr %i\n  %ap = getelementptr i8, ptr %a, i64 %i2\n  %ac = load i8, ptr %ap\n  %bp = getelementptr i8, ptr %b, i64 %i2\n  %bc = load i8, ptr %bp\n  %diff = icmp ne i8 %ac, %bc\n  br i1 %diff, label %neq, label %same\nneq:\n  %d1 = zext i8 %ac to i32\n  %d2 = zext i8 %bc to i32\n  %r = sub i32 %d1, %d2\n  ret i32 %r\nsame:\n  %z1 = icmp eq i8 %ac, 0\n  br i1 %z1, label %end, label %body\nbody:\n  %i3 = add i64 %i2, 1\n  store i64 %i3, ptr %i\n  br label %loop\nend:\n  ret i32 0\n}\n\n",
    );
    s
}

/// The file routines, as LLVM IR.
///
/// `posix` selects the system call based version; the other one goes through
/// the C library, which is what the Windows build still does.
pub fn file_routines(posix: bool, open_write: i64, open_mode: i64) -> String {
    let mut s = String::new();
    if posix {
        // Read a whole file. The size comes from seeking to the end, so that
        // the file does not have to be read in chunks.
        s.push_str(
            "define internal ptr @nexum_read_file(ptr %path) {\nentry:\n  %pathi = ptrtoint ptr %path to i64
  %fd = call i64 @nexum_sys_open(i64 %pathi, i64 0, i64 0)\n  %bad = icmp slt i64 %fd, 0\n  br i1 %bad, label %fail, label %ok\nfail:\n  call void @nexum_panic(ptr @nexum_msg_notfound)\n  unreachable\nok:\n  %size = call i64 @nexum_sys_lseek(i64 %fd, i64 0, i64 2)\n  %rewind = call i64 @nexum_sys_lseek(i64 %fd, i64 0, i64 0)\n  %n1 = add i64 %size, 1\n  %buf = call ptr @nexum_alloc(i64 %n1)\n  %bufi = ptrtoint ptr %buf to i64
  %rd = call i64 @nexum_sys_read(i64 %fd, i64 %bufi, i64 %size)\n  %nulp = getelementptr i8, ptr %buf, i64 %size\n  store i8 0, ptr %nulp\n  %cl = call i64 @nexum_sys_close(i64 %fd)\n  ret ptr %buf\n}\n\n",
        );
        // Write a whole file.
        s.push_str(&format!("define internal void @nexum_write_file(ptr %path, ptr %content) {{\nentry:\n  %fd = call i64 @nexum_sys_open(ptr %path, i64 {open_write}, i64 {open_mode})\n  %bad = icmp slt i64 %fd, 0\n  br i1 %bad, label %fail, label %ok\nfail:\n  call void @nexum_panic(ptr @nexum_msg_notfound)\n  unreachable\nok:\n  %len = call i64 @nexum_strlen(ptr %content)\n  %contenti = ptrtoint ptr %content to i64
  %wr = call i64 @nexum_sys_write(i64 %fd, i64 %contenti, i64 %len)\n  %cl = call i64 @nexum_sys_close(i64 %fd)\n  ret void\n}}\n\n"));
    } else {
        s.push_str(
            "define internal ptr @nexum_read_file(ptr %path) {\nentry:\n  %f = call ptr @fopen(ptr %path, ptr @nexum_mode_r)\n  %isnull = icmp eq ptr %f, null\n  br i1 %isnull, label %fail, label %ok\nfail:\n  call void @nexum_panic(ptr @nexum_msg_notfound)\n  unreachable\nok:\n  %s1 = call i32 @fseek(ptr %f, i64 0, i32 2)\n  %size = call i64 @ftell(ptr %f)\n  %s2 = call i32 @fseek(ptr %f, i64 0, i32 0)\n  %n1 = add i64 %size, 1\n  %buf = call ptr @nexum_alloc(i64 %n1)\n  %rd = call i64 @fread(ptr %buf, i64 1, i64 %size, ptr %f)\n  %nulp = getelementptr i8, ptr %buf, i64 %size\n  store i8 0, ptr %nulp\n  %cl = call i32 @fclose(ptr %f)\n  ret ptr %buf\n}\n\n",
        );
        s.push_str(
            "define internal void @nexum_write_file(ptr %path, ptr %content) {\nentry:\n  %f = call ptr @fopen(ptr %path, ptr @nexum_mode_w)\n  %isnull = icmp eq ptr %f, null\n  br i1 %isnull, label %fail, label %ok\nfail:\n  call void @nexum_panic(ptr @nexum_msg_notfound)\n  unreachable\nok:\n  %len = call i64 @nexum_strlen(ptr %content)\n  %wr = call i64 @fwrite(ptr %content, i64 1, i64 %len, ptr %f)\n  %cl = call i32 @fclose(ptr %f)\n  ret void\n}\n\n",
        );
    }
    s
}
