//! The byte primitives: a byte's numeric value, and a numeric value's byte.
//!
//! These are the primitives every collection in the standard library is
//! built on. The map's hash, the sort's comparison and the interning table
//! all need the numeric value of a byte, and the language has no other way
//! to get it: `s[i]` yields a one-byte `Str`, and a `Str` has no ordering
//! and no conversion to `Int` (`specs/research/standard-library-plan.md`,
//! "The hash function, and what it is blocked on").
//!
//! The two entry points are:
//!
//! ```text
//! nexum_byte_value(s: Str, i: Int) -> Int   ; the byte at index i, as a number
//! nexum_byte_from_value(v: Int) -> Str       ; the byte with the value v, as a string
//! ```
//!
//! They are the same on every platform: they touch the heap and the panic
//! path, and nothing else. A string is a NUL-terminated byte string in the
//! runtime, so the index is checked against `nexum_strlen` exactly as
//! `nexum_str_char_at` checks it.
//!
//! The two failure modes are the ones the runtime already has: an index
//! outside the string panics with "index out of bounds", and a value that
//! is not a byte panics with "byte value out of range". A panic writes the
//! message to the error stream and exits with 1; the language has no
//! `Option`, so there is nothing to return instead.
//!
//! One consequence of the NUL-terminated representation is worth knowing:
//! the byte 0 has no string of its own. `byte_from_value(0)` writes the
//! byte and its NUL terminator, so the runtime reads the result as the
//! empty string (`docs/runtime-abi.md`, section 8).

/// The byte routines, as LLVM IR.
///
/// `runtime_ir` has to splice them into the module, the way it does for the
/// string routines:
///
/// ```text
/// s.push_str(&bytes::byte_routines());
/// ```
pub fn byte_routines() -> String {
    let mut s = String::new();

    // The panic message for a byte value that is not in 0..255. The length
    // in the constant is the number of bytes the message takes, the NUL
    // included: "byte value out of range" is 23 bytes, so 24 with the NUL.
    // A length that is too small is a module that does not compile.
    s.push_str(
        "@nexum_msg_byte_range = private unnamed_addr constant [24 x i8] c\"byte value out of range\\00\"\n\n",
    );

    // The numeric value of the byte at index `i`. The index is checked the
    // way `nexum_str_char_at` checks it: a negative index and an index at
    // or past the end are both out of bounds.
    s.push_str(
        "define internal i64 @nexum_byte_value(ptr %s, i64 %i) {\nentry:\n  %n = call i64 @nexum_strlen(ptr %s)\n  %neg = icmp slt i64 %i, 0\n  %oob = icmp sge i64 %i, %n\n  %bad = or i1 %neg, %oob\n  br i1 %bad, label %fail, label %ok\nfail:\n  call void @nexum_panic(ptr @nexum_msg_oob)\n  unreachable\nok:\n  %p = getelementptr i8, ptr %s, i64 %i\n  %b = load i8, ptr %p\n  %v = zext i8 %b to i64\n  ret i64 %v\n}\n\n",
    );

    // The byte with the numeric value `v`, as a one-byte string. A value
    // outside 0..255 is not a byte, so it panics: the language has no
    // `Option` and no other way to say "this is not a byte".
    s.push_str(
        "define internal ptr @nexum_byte_from_value(i64 %v) {\nentry:\n  %neg = icmp slt i64 %v, 0\n  %big = icmp sgt i64 %v, 255\n  %bad = or i1 %neg, %big\n  br i1 %bad, label %fail, label %ok\nfail:\n  call void @nexum_panic(ptr @nexum_msg_byte_range)\n  unreachable\nok:\n  %buf = call ptr @nexum_alloc(i64 1)\n  %b = trunc i64 %v to i8\n  store i8 %b, ptr %buf\n  %nulp = getelementptr i8, ptr %buf, i64 1\n  store i8 0, ptr %nulp\n  ret ptr %buf\n}\n\n",
    );

    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The symbols the given IR calls.
    fn called_symbols(ir: &str) -> BTreeSet<String> {
        ir.lines()
            .filter_map(|l| l.split("call ").nth(1))
            .filter_map(|l| l.split('@').nth(1))
            .filter_map(|l| l.split('(').next())
            .map(|s| s.to_string())
            .collect()
    }

    /// The symbols the given IR defines: the functions and the globals.
    fn defined_symbols(ir: &str) -> BTreeSet<String> {
        ir.lines()
            .filter_map(|l| {
                if let Some(rest) = l.strip_prefix("define ") {
                    // "internal i64 @name(args)"
                    let name = rest.split('@').nth(1)?;
                    Some(name.split('(').next().unwrap_or(name).to_string())
                } else {
                    // "@name = ..."
                    let rest = l.strip_prefix('@')?;
                    Some(rest.split(' ').next().unwrap_or(rest).to_string())
                }
            })
            .collect()
    }

    /// The byte routines must not need a symbol the rest of the runtime does
    /// not already have: the phase's rule is that a program declares no
    /// external symbol, and a call to a symbol that is not defined is a
    /// module that does not verify.
    #[test]
    fn the_byte_routines_call_only_runtime_symbols() {
        let runtime = crate::runtime_ir("linux-x86_64", "x86_64");
        let defined = defined_symbols(&runtime);
        for symbol in called_symbols(&byte_routines()) {
            assert!(
                defined.contains(&symbol),
                "the byte routines call `@{}`, which the runtime does not define",
                symbol
            );
        }
    }

    /// The two entry points are the ones the sema lowers to.
    #[test]
    fn the_entry_points_have_their_signatures() {
        let ir = byte_routines();
        assert!(
            ir.contains("define internal i64 @nexum_byte_value(ptr %s, i64 %i)"),
            "the byte value entry point is missing:\n{ir}"
        );
        assert!(
            ir.contains("define internal ptr @nexum_byte_from_value(i64 %v)"),
            "the byte from value entry point is missing:\n{ir}"
        );
    }

    /// The byte's value is unsigned: a byte of 0xFF is 255, not -1.
    #[test]
    fn the_value_is_unsigned() {
        let ir = byte_routines();
        assert!(
            ir.contains("zext i8 %b to i64"),
            "the byte's value must be zero extended:\n{ir}"
        );
        assert!(
            !ir.contains("sext i8"),
            "the byte's value must not be sign extended:\n{ir}"
        );
    }

    /// The panic message's declared length has to be the number of bytes the
    /// message takes. LLVM rejects a constant whose size does not match its
    /// initialiser, and the mismatch is easy to make by editing the text.
    #[test]
    fn the_panic_message_length_matches_its_bytes() {
        let ir = byte_routines();
        let line = ir
            .lines()
            .find(|l| l.starts_with("@nexum_msg_byte_range"))
            .expect("the panic message is missing");
        let declared: usize = line
            .split('[')
            .nth(1)
            .and_then(|s| s.split(' ').next())
            .and_then(|s| s.parse().ok())
            .expect("the panic message has no length");
        let text = line
            .split("c\"")
            .nth(1)
            .and_then(|s| s.strip_suffix('"'))
            .expect("the panic message has no text");
        // Every `\XX` escape is one byte; every other character is one byte.
        let mut bytes = 0usize;
        let mut rest = text;
        while let Some(i) = rest.find('\\') {
            bytes += rest[..i].len();
            rest = &rest[i + 3..];
            bytes += 1;
        }
        bytes += rest.len();
        assert_eq!(
            declared, bytes,
            "the panic message's length does not match its bytes: {line}"
        );
    }
}
