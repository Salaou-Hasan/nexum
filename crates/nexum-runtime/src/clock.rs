//! The clock.
//!
//! The program needs to know what time it is, and the language has no way
//! to ask yet - so this is the runtime's half of that, and the builtin that
//! reaches it comes with the language's own clock work.
//!
//! The wall clock is `gettimeofday`, because Darwin has no
//! `clock_gettime` and `gettimeofday` is on both systems. It writes a
//! structure of two 64-bit numbers - the seconds and the microseconds
//! since the epoch - so the wrapper needs somewhere to put them, and a
//! frame is the natural place.

/// The clock's entry point.
///
/// `nexum_now_micros` returns the microseconds since the epoch, or zero
/// if the system call failed. The runtime is freestanding, so there is no
/// C library to ask.
pub fn clock_routines() -> String {
    let mut s = String::new();
    s.push_str(
        "define i64 @nexum_now_micros() {\n\
         entry:\n\
         \x20 %ts = alloca [16 x i8], align 8\n\
         \x20 %tsi = ptrtoint ptr %ts to i64\n\
         \x20 %r = call i64 @nexum_sys_gettimeofday(i64 %tsi, i64 0)\n\
         \x20 %bad = icmp slt i64 %r, 0\n\
         \x20 br i1 %bad, label %fail, label %ok\n\
         fail:\n\
         \x20 ret i64 0\n\
         ok:\n\
         \x20 %p1 = getelementptr i8, ptr %ts, i64 0\n\
         \x20 %sec = load i64, ptr %p1\n\
         \x20 %p2 = getelementptr i8, ptr %ts, i64 8\n\
         \x20 %usec = load i64, ptr %p2\n\
         \x20 %s = mul i64 %sec, 1000000\n\
         \x20 %t = add i64 %s, %usec\n\
         \x20 ret i64 %t\n\
         }\n\n",
    );
    s
}
