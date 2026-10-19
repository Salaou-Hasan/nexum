//! The allocator that replaces `malloc` and `free` in the runtime.
//!
//! A Nexum program on a POSIX target depends on the kernel and on nothing
//! else, and the heap is the last thing the C library used to provide. This
//! is the allocator that takes its place: a size-class allocator over
//! mapped chunks, with a bump pointer and one free list per class.
//!
//! The design is settled in `specs/research/allocator-design.md` and the
//! contract is in `specs/contracts/phase-11a2.md`. Three rules make it
//! small enough to be written as LLVM IR text:
//!
//! * **The class is computed, not searched.** 255 exact 16-byte classes
//!   from 32 to 4096 bytes, then four doubling classes to 65536, then a
//!   class of its own for anything larger. Nothing is split and nothing is
//!   merged, so there is no coalescing code and no first-fit search.
//! * **The free list lives in the collector's `+16` word.** A block is
//!   either a live object or a free block, never both, so the link costs no
//!   bytes per object and the object layout does not change at all.
//! * **A block larger than the biggest class has its own mapping**, which
//!   is unmapped when the block dies.
//!
//! There is no loop anywhere in the allocator: the class index is an
//! arithmetic function of the size, and a fresh chunk is always big enough
//! for the request, so the chunk path needs no second fit test.
//!
//! The interface the collector uses:
//!
//! ```text
//! nexum_heap_alloc(total) -> ptr    ; a block of `total` bytes, 16-byte aligned
//! nexum_heap_free(ptr, total)       ; the block goes back to its class
//! nexum_size_class(total) -> class  ; the class of a size, computed
//! ```
//!
//! `total` is the size of the whole block, header included: the collector
//! asks for `32 + round16(payload)`. **The same `total` has to be passed to
//! the free function that was passed to the allocation function**: the class
//! is derived from the size at both ends, so a block that is freed with a
//! different size lands on the wrong list and can be handed out later with
//! a capacity smaller than the payload it holds.
//!
//! The raw entry point is for the callers the collector does not manage -
//! the arenas of phase 11d, and anything a program hands to the kernel:
//!
//! ```text
//! nexum_alloc_raw(size) -> ptr      ; a block of its own, `size` bytes
//! nexum_free_raw(ptr, size)
//! ```
//!
//! A raw block has no header and no free list: it is a mapping of its own,
//! so the caller gets the size it asked for - the mapping is rounded up to
//! a page and the rest is slack - and the block can be given to the kernel
//! as it is.

use crate::syscalls::{SysArch, SysOs, Syscalls};

/// The number of size classes: 255 fine, four coarse, and the large one.
const CLASS_COUNT: i64 = 260;

/// The class of a block that is larger than the biggest small class.
const LARGE_CLASS: i64 = 259;

/// The class of a size that is out of range. It is not a real class: it
/// only marks the size as one the allocator refuses.
const INVALID_CLASS: i64 = 260;

/// The biggest small class, in bytes.
const MAX_SMALL: i64 = 65536;

/// The biggest total the allocator accepts. A larger one is refused rather
/// than turned into a negative length.
const MAX_TOTAL: i64 = 140737488355328; // 2^47

/// The size of a chunk: one megabyte.
const CHUNK_SIZE: i64 = 1048576;

/// The size of a chunk's header: the size of the chunk and the link to the
/// next one. The blocks start after it.
const CHUNK_HEADER: i64 = 32;

/// The class ladder.
///
/// The class of a size is an arithmetic function of the size, so there is
/// no search and no loop. A size below 32 is not a size the allocator
/// serves, and it gets the invalid class, which the callers check.
const CLASS_IR: &str = r#"define i64 @nexum_size_class(i64 %total) {
entry:
  %low = icmp ult i64 %total, 32
  br i1 %low, label %invalid, label %inrange
invalid:
  ret i64 260
inrange:
  %fine = icmp ule i64 %total, 4096
  br i1 %fine, label %fineclass, label %coarse
fineclass:
  %d = sub i64 %total, 32
  %r = add i64 %d, 15
  %c = lshr i64 %r, 4
  ret i64 %c
coarse:
  %k1 = icmp ule i64 %total, 8192
  br i1 %k1, label %c255, label %k2
c255:
  ret i64 255
k2:
  %k2b = icmp ule i64 %total, 16384
  br i1 %k2b, label %c256, label %k3
c256:
  ret i64 256
k3:
  %k3b = icmp ule i64 %total, 32768
  br i1 %k3b, label %c257, label %k4
c257:
  ret i64 257
k4:
  %k4b = icmp ule i64 %total, 65536
  br i1 %k4b, label %c258, label %large
c258:
  ret i64 258
large:
  ret i64 259
}

"#;

/// The size of a class.
///
/// The fine classes are exact: the class of `32 + 16k` bytes is `32 + 16k`
/// bytes, so a request the collector makes has no internal fragmentation
/// at all. The coarse classes double, and the large class has no size of
/// its own.
const CLASS_SIZE_IR: &str = r#"define internal i64 @nexum_class_size(i64 %c) {
entry:
  %fine = icmp ule i64 %c, 254
  br i1 %fine, label %small, label %coarse
small:
  %s = shl i64 %c, 4
  %r = add i64 %s, 32
  ret i64 %r
coarse:
  %k = sub i64 %c, 255
  %sz = shl i64 8192, %k
  ret i64 %sz
}

"#;

/// A length rounded up to a whole number of pages.
///
/// The same constant is used for the mapping length and for the unmap
/// length, so the two always agree: `munmap` refuses a length that is not
/// a multiple of the page size.
const PAGE_ROUND_IR: &str = r#"define internal i64 @nexum_page_round_up(i64 %n) {
entry:
  %a = add i64 %n, {add}
  %r = and i64 %a, {mask}
  ret i64 %r
}

"#;

/// A chunk: one mapping with a header, linked into the chunk list.
///
/// The blocks are carved out of the chunk by the bump pointer, and the
/// chunk is never unmapped during the program's life. The function
/// reports whether the mapping worked: a failed mapping leaves the bump
/// pointer where it was, and the caller must not carve from a chunk that
/// did not fit the request.
const CHUNK_IR: &str = r#"define internal i1 @nexum_heap_new_chunk() {
entry:
  %p = call ptr @nexum_heap_map(i64 {size})
  %bad = icmp eq ptr %p, null
  br i1 %bad, label %fail, label %ok
fail:
  ret i1 false
ok:
  store i64 {size}, ptr %p
  %np = getelementptr i8, ptr %p, i64 8
  %old = load ptr, ptr @nexum_chunk_list
  store ptr %old, ptr %np
  store ptr %p, ptr @nexum_chunk_list
  %first = getelementptr i8, ptr %p, i64 {header}
  store ptr %first, ptr @nexum_chunk_bump
  %end = getelementptr i8, ptr %p, i64 {size}
  store ptr %end, ptr @nexum_chunk_end
  ret i1 true
}

"#;

/// The mapping layer of the POSIX targets.
///
/// The arguments of a mapping are in the order the system call expects:
/// address, length, protection, flags, descriptor, offset. The flags go in
/// the fourth register and the descriptor in the fifth, and swapping them
/// maps the world with the wrong flags.
const POSIX_MAP_IR: &str = r#"define internal ptr @nexum_heap_map(i64 %len) {
entry:
  %r = call i64 @nexum_sys_mmap(i64 0, i64 %len, i64 {prot}, i64 {flags}, i64 -1, i64 0)
  %bad = icmp slt i64 %r, 0
  br i1 %bad, label %fail, label %ok
fail:
  ret ptr null
ok:
  %p = inttoptr i64 %r to ptr
  ret ptr %p
}

define internal void @nexum_heap_unmap(ptr %p, i64 %len) {
entry:
  %pi = ptrtoint ptr %p to i64
  call i64 @nexum_sys_munmap(i64 %pi, i64 %len)
  ret void
}

"#;

/// The mapping layer of the Windows target.
///
/// Windows has no direct system call instruction, but `VirtualAlloc` is a
/// kernel32 export and not the C library, so the program still stops
/// depending on the CRT's heap. The allocator's algorithms are the same;
/// only the two raw-block calls differ.
const WINDOWS_MAP_IR: &str = r#"declare ptr @VirtualAlloc(ptr, i64, i32, i32)
declare i32 @VirtualFree(ptr, i64, i32)

define internal ptr @nexum_heap_map(i64 %len) {
entry:
  %p = call ptr @VirtualAlloc(ptr null, i64 %len, i32 12288, i32 4)
  ret ptr %p
}

define internal void @nexum_heap_unmap(ptr %p, i64 %len) {
entry:
  call i32 @VirtualFree(ptr %p, i64 0, i32 32768)
  ret void
}

"#;

/// The allocation.
///
/// A request is served from its class's free list first, and from the bump
/// pointer second. A request the classes cannot hold gets a mapping of its
/// own. A failed mapping is a null, and the collector turns that into
/// `nexum_oom()`.
const ALLOC_IR: &str = r#"define ptr @nexum_heap_alloc(i64 %total) {
entry:
  %low = icmp ult i64 %total, 32
  %high = icmp ugt i64 %total, {max}
  %bad = or i1 %low, %high
  br i1 %bad, label %fail, label %classed
classed:
  %c = call i64 @nexum_size_class(i64 %total)
  %islarge = icmp eq i64 %c, 259
  br i1 %islarge, label %large, label %small
small:
  %slot = getelementptr [260 x ptr], ptr @nexum_free_lists, i64 0, i64 %c
  %head = load ptr, ptr %slot
  %empty = icmp eq ptr %head, null
  br i1 %empty, label %bump, label %pop
pop:
  %np = getelementptr i8, ptr %head, i64 16
  %nxt = load ptr, ptr %np
  store ptr %nxt, ptr %slot
  ret ptr %head
bump:
  %sz = call i64 @nexum_class_size(i64 %c)
  %cur = load ptr, ptr @nexum_chunk_bump
  %none = icmp eq ptr %cur, null
  br i1 %none, label %chunk, label %fits
fits:
  %end = load ptr, ptr @nexum_chunk_end
  %next = getelementptr i8, ptr %cur, i64 %sz
  %room = icmp ule ptr %next, %end
  br i1 %room, label %carve, label %chunk
carve:
  store ptr %next, ptr @nexum_chunk_bump
  ret ptr %cur
chunk:
  %mapped = call i1 @nexum_heap_new_chunk()
  br i1 %mapped, label %carve2, label %fail
carve2:
  %b4 = load ptr, ptr @nexum_chunk_bump
  %next2 = getelementptr i8, ptr %b4, i64 %sz
  store ptr %next2, ptr @nexum_chunk_bump
  ret ptr %b4
large:
  %len = call i64 @nexum_page_round_up(i64 %total)
  %p = call ptr @nexum_heap_map(i64 %len)
  %isnull = icmp eq ptr %p, null
  br i1 %isnull, label %fail, label %got
got:
  ret ptr %p
fail:
  ret ptr null
}

"#;

/// The free.
///
/// The block goes on the free list of its class, and the link is written
/// into the `+16` word of the block. That word holds the collector's
/// next-object link while the object is live, and the collector unlinks an
/// object before it frees it, so the word is free exactly when it is
/// needed.
const FREE_IR: &str = r#"define void @nexum_heap_free(ptr %p, i64 %total) {
entry:
  %c = call i64 @nexum_size_class(i64 %total)
  %islarge = icmp eq i64 %c, 259
  br i1 %islarge, label %large, label %small
small:
  %invalid = icmp eq i64 %c, 260
  br i1 %invalid, label %done, label %push
push:
  %slot = getelementptr [260 x ptr], ptr @nexum_free_lists, i64 0, i64 %c
  %old = load ptr, ptr %slot
  %np = getelementptr i8, ptr %p, i64 16
  store ptr %old, ptr %np
  store ptr %p, ptr %slot
  br label %done
large:
  %len = call i64 @nexum_page_round_up(i64 %total)
  call void @nexum_heap_unmap(ptr %p, i64 %len)
  br label %done
done:
  ret void
}

"#;

/// The raw entry point.
///
/// A raw block is a mapping of its own: no header, no free list, and no
/// collector. The caller gets the size it asked for - the mapping is
/// rounded up to a page and the rest is slack - and the block can be
/// handed to the kernel as it is. It is not recycled: a raw block is
/// unmapped when it is freed, and a caller that wants many small blocks
/// out of one mapping should carve them itself.
const RAW_IR: &str = r#"define ptr @nexum_alloc_raw(i64 %size) {
entry:
  %empty = icmp ule i64 %size, 0
  br i1 %empty, label %fail, label %map
map:
  %len = call i64 @nexum_page_round_up(i64 %size)
  %p = call ptr @nexum_heap_map(i64 %len)
  %isnull = icmp eq ptr %p, null
  br i1 %isnull, label %fail, label %got
got:
  ret ptr %p
fail:
  ret ptr null
}

define void @nexum_free_raw(ptr %p, i64 %size) {
entry:
  %isnull = icmp eq ptr %p, null
  br i1 %isnull, label %done, label %unmap
unmap:
  %len = call i64 @nexum_page_round_up(i64 %size)
  call void @nexum_heap_unmap(ptr %p, i64 %len)
  br label %done
done:
  ret void
}

"#;

/// Fill the placeholders of a piece of IR.
///
/// The IR is written as text with `{name}` in the places a constant goes,
/// so the constants that differ per target can be filled in.
fn fill(ir: &str, values: &[(&str, i64)]) -> String {
    let mut s = ir.to_string();
    for (name, value) in values {
        s = s.replace(&format!("{{{name}}}"), &value.to_string());
    }
    s
}

/// The allocator, as LLVM IR.
///
/// `os` and `arch` are the target's, the same pair `runtime_ir` takes.
/// The result is spliced into the runtime module: the collector's
/// `malloc` and `free` calls become `nexum_heap_alloc` and
/// `nexum_heap_free`.
pub fn allocator_ir(os: &str, arch: &str) -> String {
    let windows = os.contains("windows");
    let page = page_size(os, arch);
    let mut s = String::new();

    // The allocator's state: one free list per class, and the chunk that
    // is being carved.
    s.push_str(&format!(
        "@nexum_free_lists = internal global [{} x ptr] zeroinitializer, align 8\n",
        CLASS_COUNT
    ));
    s.push_str("@nexum_chunk_list = internal global ptr null, align 8\n");
    s.push_str("@nexum_chunk_bump = internal global ptr null, align 8\n");
    s.push_str("@nexum_chunk_end = internal global ptr null, align 8\n\n");

    // The class ladder and the arithmetic around it.
    s.push_str(CLASS_IR);
    s.push_str(CLASS_SIZE_IR);
    s.push_str(&fill(PAGE_ROUND_IR, &[("add", page - 1), ("mask", -page)]));
    s.push_str(&fill(
        CHUNK_IR,
        &[("size", CHUNK_SIZE), ("header", CHUNK_HEADER)],
    ));

    // The raw-block layer: the only part that differs between the systems.
    if windows {
        s.push_str(WINDOWS_MAP_IR);
    } else {
        let numbers = Syscalls::for_os(if os.contains("macos") {
            SysOs::Macos
        } else {
            SysOs::Linux
        });
        s.push_str(&fill(
            POSIX_MAP_IR,
            &[("prot", numbers.map_prot), ("flags", numbers.map_flags)],
        ));
    }

    // The allocator itself.
    s.push_str(&fill(ALLOC_IR, &[("max", MAX_TOTAL)]));
    s.push_str(FREE_IR);
    s.push_str(RAW_IR);
    s
}

/// The page size of a target.
///
/// It is 16 KiB on macOS on Apple Silicon and 4 KiB everywhere else. The
/// same constant is used for the mapping length and for the unmap length,
/// so the two always agree: macOS's `munmap` refuses a length that is not
/// a multiple of the page size, and the wrong constant leaks every large
/// block.
fn page_size(os: &str, arch: &str) -> i64 {
    if os.contains("windows") {
        // The Windows build has no system call layer, but the page size of
        // the kernel is the same there.
        return 4096;
    }
    let sys_os = if os.contains("macos") {
        SysOs::Macos
    } else {
        SysOs::Linux
    };
    let sys_arch = if arch.contains("aarch64") {
        SysArch::Aarch64
    } else {
        SysArch::X86_64
    };
    crate::syscalls::page_size(sys_os, sys_arch)
}

/// The class of a size, in Rust.
///
/// The same ladder the runtime computes, so a caller that has to know the
/// class of a size does not have to call into the runtime for it.
pub const fn size_class(total: i64) -> i64 {
    if total < 32 {
        return INVALID_CLASS;
    }
    if total <= 4096 {
        return (total - 32 + 15) >> 4;
    }
    if total <= 8192 {
        return 255;
    }
    if total <= 16384 {
        return 256;
    }
    if total <= 32768 {
        return 257;
    }
    if total <= MAX_SMALL {
        return 258;
    }
    LARGE_CLASS
}

/// The size of a class, in Rust.
pub const fn class_size(class: i64) -> i64 {
    if class <= 254 {
        32 + (class << 4)
    } else if class <= LARGE_CLASS {
        8192 << (class - 255)
    } else {
        0
    }
}
