//! The system call layer.
//!
//! Nexum programs do not use the C library: they reach the operating
//! system through the system call instruction directly. That is what makes a
//! program freestanding - it depends on the kernel and on nothing else.
//!
//! The wrappers are written as inline assembly in the generated LLVM IR.
//! Two rules shape them:
//!
//! * The arguments are pinned to the registers the instruction reads them
//!   from, with the constraint string, so the assembly never has to move
//!   them and never has to refer to an operand by number. The numbering of
//!   the operands in an assembly block counts the output as operand zero,
//!   and a mistake there is silent and fatal; naming the registers avoids
//!   the question.
//! * The system call instruction needs a 16-byte aligned stack. A function
//!   without a frame is entered with the stack at 8 modulo 16, so the
//!   wrapper aligns it itself and restores it afterwards.
//!
//! The system call numbers differ between the systems, so they are looked
//! up here.

/// The operating systems the layer covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysOs {
    Linux,
    Macos,
}

/// The instruction sets the layer covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SysArch {
    X86_64,
    Aarch64,
}

/// The system call numbers and the flags, per operating system.
#[derive(Debug, Clone, Copy)]
pub struct Syscalls {
    pub read: i64,
    pub write: i64,
    pub open: i64,
    pub close: i64,
    pub mmap: i64,
    pub munmap: i64,
    pub exit: i64,
    pub lseek: i64,
    /// The flags for opening a file for reading.
    pub open_read: i64,
    /// The flags for opening a file for writing.
    pub open_write: i64,
    /// The permissions for a file that is created.
    pub open_mode: i64,
    /// The protection an anonymous mapping asks for: read and write.
    pub map_prot: i64,
    /// The flags of an anonymous mapping: private, and not backed by a file.
    pub map_flags: i64,
}

impl Syscalls {
    /// The numbers for an operating system.
    pub fn for_os(os: SysOs) -> Self {
        match os {
            SysOs::Linux => Self {
                read: 0,
                write: 1,
                open: 2,
                close: 3,
                mmap: 9,
                munmap: 11,
                exit: 60,
                lseek: 8,
                open_read: 0,
                open_write: 1 | 64 | 512,
                open_mode: 0o644,
                map_prot: 1 | 2,
                map_flags: 0x02 | 0x20,
            },
            SysOs::Macos => Self {
                // Darwin numbers its system calls from the BSD master table,
                // and it does not match Linux: read is 3, exit is 1 and
                // lseek is 199 here.
                read: 3,
                write: 4,
                open: 5,
                close: 6,
                mmap: 197,
                munmap: 198,
                exit: 1,
                lseek: 199,
                open_read: 0,
                open_write: 1 | 512 | 1024,
                open_mode: 0o644,
                map_prot: 1 | 2,
                map_flags: 0x2 | 0x1000,
            },
        }
    }
}

/// The page size of a target.
///
/// It is not the same everywhere: macOS on Apple Silicon has 16 KiB pages,
/// and its munmap refuses a length that is not a multiple of the page
/// size, so the wrong constant leaks every large block.
pub fn page_size(os: SysOs, arch: SysArch) -> i64 {
    match (os, arch) {
        (SysOs::Macos, SysArch::Aarch64) => 16384,
        _ => 4096,
    }
}

/// Where the registers of a system call live.
struct Layout {
    /// The registers the argument values are placed in.
    staging: &'static [&'static str],
    /// The registers the instruction reads the arguments from.
    call: &'static [&'static str],
    /// The register the result comes back in.
    result: &'static str,
    /// The register the system call number goes in.
    number: &'static str,
    /// The instruction that makes the call.
    instruction: &'static str,
    /// The registers the instruction clobbers.
    clobbers: &'static [&'static str],
}

/// The layout for an operating system and an architecture.
fn layout(os: SysOs, arch: SysArch) -> Layout {
    match (os, arch) {
        (SysOs::Linux, SysArch::X86_64) => Layout {
            staging: &["rdi", "rsi", "rdx", "r10", "r8", "r9"],
            call: &["rdi", "rsi", "rdx", "r10", "r8", "r9"],
            result: "rax",
            number: "rax",
            instruction: "syscall",
            // The instruction destroys the return address and the flags, and
            // the scratch register the alignment uses.
            clobbers: &["rcx", "r11", "r12"],
        },
        (SysOs::Macos, SysArch::X86_64) => Layout {
            staging: &["rdi", "rsi", "rdx", "r10", "r8", "r9"],
            call: &["rdi", "rsi", "rdx", "r10", "r8", "r9"],
            result: "rax",
            number: "rax",
            instruction: "syscall",
            clobbers: &["rcx", "r11", "r12"],
        },
        // On AArch64 the result comes back in the register the first
        // argument went in, so the arguments are staged in the registers the
        // instruction does not touch and moved into place by the assembly.
        (SysOs::Linux, SysArch::Aarch64) => Layout {
            staging: &["x9", "x10", "x11", "x12", "x13", "x14"],
            call: &["x0", "x1", "x2", "x3", "x4", "x5"],
            result: "x0",
            number: "x8",
            instruction: "svc #0",
            clobbers: &["x8"],
        },
        (SysOs::Macos, SysArch::Aarch64) => Layout {
            staging: &["x9", "x10", "x11", "x12", "x13", "x14"],
            call: &["x0", "x1", "x2", "x3", "x4", "x5"],
            result: "x0",
            number: "x16",
            instruction: "svc #0x80",
            clobbers: &["x16"],
        },
    }
}

/// One argument of a system call: which register it goes in and where the
/// value comes from.
struct Arg {
    /// Which argument register the value goes in, counting from zero.
    index: usize,
    /// The operand that holds the value.
    source: Option<&'static str>,
    /// The constant, when there is no source.
    constant: i64,
}

/// The inline assembly of one system call.
///
/// The arguments are pinned to the registers the instruction reads them from,
/// so the body only has to set the register the number goes in and make the
/// call.
fn call_body(os: SysOs, arch: SysArch, nr: i64, args: &[Arg]) -> String {
    let lay = layout(os, arch);
    let mut text = String::new();
    // The stack has to be aligned for the call. A function without a frame is
    // entered with the stack at 8 modulo 16, and the kernel wants 16.
    match arch {
        SysArch::X86_64 => {
            // A literal dollar is written twice in an assembly block: a
            // single one starts an operand reference.
            text.push_str("mov %rsp, %r12; and $$-16, %rsp; ");
        }
        SysArch::Aarch64 => {}
    }
    // The number goes into the register the convention uses. It is a
    // constant in the text: it differs per system and per call. A literal
    // dollar is written twice in an assembly block, because a single one
    // starts an operand reference.
    match arch {
        SysArch::X86_64 => text.push_str(&format!("movq $${nr}, %{}\n", lay.number)),
        SysArch::Aarch64 => text.push_str(&format!("mov {}, #{nr}\n", lay.number)),
    }
    // The arguments move into the registers the instruction reads them from.
    // On x86-64 they are already there: the constraint placed them.
    for arg in args {
        let dst = lay.call[arg.index];
        let src = lay.staging[arg.index];
        match arch {
            SysArch::X86_64 => {}
            SysArch::Aarch64 => text.push_str(&format!("mov {dst}, {src}\n")),
        }
        let _ = dst;
    }
    text.push_str(lay.instruction);
    text.push('\n');
    // The stack is restored after the call.
    match arch {
        SysArch::X86_64 => text.push_str("mov %r12, %rsp"),
        SysArch::Aarch64 => {}
    }
    // The text of an assembly block is one line: the newlines are escapes.
    let text = text.replace('\n', "\\0A");
    // The constraints: the result, then one per argument, then the registers
    // the instruction clobbers.
    let mut constraints = vec![format!("={{{}}}", lay.result)];
    for arg in args {
        constraints.push(format!("{{{}}}", lay.staging[arg.index]));
    }
    for c in lay.clobbers {
        constraints.push(format!("~{{{c}}}"));
    }
    constraints.push("~{memory}".to_string());
    let operands: Vec<String> = args
        .iter()
        .map(|arg| match arg.source {
            Some(src) => format!("i64 {src}"),
            None => format!("i64 {}", arg.constant),
        })
        .collect();
    format!(
        "  %r = call i64 asm sideeffect \"{text}\", \"{}\"({})\n  ret i64 %r\n",
        constraints.join(","),
        operands.join(", ")
    )
}

/// The system call wrappers for a convention.
///
/// Every wrapper takes its arguments as `i64` values and returns the raw
/// system call result, where a negative value means failure.
pub fn wrappers(os: SysOs, arch: SysArch, sys: &Syscalls) -> String {
    let mut s = String::new();
    let mut push = |name: &str, nr: i64, args: &[Arg]| {
        s.push_str(&format!(
            "define internal i64 @nexum_sys_{name}(i64 %a0, i64 %a1, i64 %a2, i64 %a3, i64 %a4, i64 %a5) {{\nentry:\n{}  ret i64 %r\n}}\n\n",
            call_body(os, arch, nr, args)
        ));
    };
    let arg = |index: usize, source: &'static str| Arg {
        index,
        source: Some(source),
        constant: 0,
    };
    let const_arg = |index: usize, constant: i64| Arg {
        index,
        source: None,
        constant,
    };
    push(
        "read",
        sys.read,
        &[arg(0, "%a0"), arg(1, "%a1"), arg(2, "%a2")],
    );
    push(
        "write",
        sys.write,
        &[arg(0, "%a0"), arg(1, "%a1"), arg(2, "%a2")],
    );
    push(
        "open",
        sys.open,
        &[arg(0, "%a0"), arg(1, "%a1"), arg(2, "%a2")],
    );
    push(
        "lseek",
        sys.lseek,
        &[arg(0, "%a0"), arg(1, "%a1"), arg(2, "%a2")],
    );
    push("close", sys.close, &[arg(0, "%a0")]);
    // mmap: the arguments are (address, length, protection, flags, file
    // descriptor, offset). The descriptor is -1 and the offset is 0, so the
    // mapping is anonymous, and the protection and the flags come from the
    // system. The order matters: the flags go in the fourth register and
    // the descriptor in the fifth, and swapping them maps the world.
    push(
        "mmap",
        sys.mmap,
        &[
            arg(0, "%a0"),
            arg(1, "%a1"),
            const_arg(2, sys.map_prot),
            const_arg(3, sys.map_flags),
            const_arg(4, -1),
            const_arg(5, 0),
        ],
    );
    push("munmap", sys.munmap, &[arg(0, "%a0"), arg(1, "%a1")]);
    push("exit", sys.exit, &[arg(0, "%a0")]);
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two systems number their calls differently, and a wrong number is
    /// silent: the call reaches the kernel as a different one.
    #[test]
    fn the_numbers_are_per_system() {
        let linux = Syscalls::for_os(SysOs::Linux);
        assert_eq!(linux.read, 0);
        assert_eq!(linux.write, 1);
        assert_eq!(linux.open, 2);
        assert_eq!(linux.close, 3);
        assert_eq!(linux.lseek, 8);
        assert_eq!(linux.exit, 60);

        let darwin = Syscalls::for_os(SysOs::Macos);
        assert_eq!(darwin.read, 3);
        assert_eq!(darwin.write, 4);
        assert_eq!(darwin.open, 5);
        assert_eq!(darwin.close, 6);
        assert_eq!(darwin.lseek, 199);
        assert_eq!(darwin.exit, 1);
    }

    /// In AT&T syntax `movq 1, %rax` reads the 64-bit value at the address
    /// 1; an immediate needs the sigil, and a literal dollar is written twice
    /// because a single one starts an operand reference.
    #[test]
    fn the_number_is_an_immediate() {
        let linux = Syscalls::for_os(SysOs::Linux);
        let ir = wrappers(SysOs::Linux, SysArch::X86_64, &linux);
        assert!(ir.contains("movq $$0, %rax"), "{ir}");
        assert!(ir.contains("movq $$2, %rax"), "{ir}");
        assert!(!ir.contains("movq 0, %rax"), "{ir}");
        assert!(!ir.contains("movq 2, %rax"), "{ir}");
    }

    /// The stack is aligned for the call and restored afterwards.
    #[test]
    fn the_stack_is_aligned_for_the_call() {
        let linux = Syscalls::for_os(SysOs::Linux);
        let ir = wrappers(SysOs::Linux, SysArch::X86_64, &linux);
        assert!(ir.contains("and $$-16, %rsp"), "{ir}");
        assert!(ir.contains("mov %r12, %rsp"), "{ir}");
    }

    /// The arguments are pinned to their registers, so the body never
    /// refers to an operand by number: the numbering counts the output as
    /// operand zero.
    #[test]
    fn the_arguments_are_pinned_to_their_registers() {
        let linux = Syscalls::for_os(SysOs::Linux);
        let ir = wrappers(SysOs::Linux, SysArch::X86_64, &linux);
        assert!(ir.contains("\"={rax},{rdi},{rsi},{rdx}"), "{ir}");
        assert!(!has_operand_reference(&ir), "{ir}");
    }

    /// A dollar that is not doubled is an operand reference. The immediates
    /// in the assembly text are written `$$1` and the like.
    fn has_operand_reference(ir: &str) -> bool {
        let bytes = ir.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'$' {
                if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
                    i += 2;
                    continue;
                }
                return true;
            }
            i += 1;
        }
        false
    }

    /// AArch64 takes the first argument in the register the result comes
    /// back in, so the arguments are staged elsewhere and moved into place.
    #[test]
    fn aarch64_stages_its_arguments() {
        let linux = Syscalls::for_os(SysOs::Linux);
        let ir = wrappers(SysOs::Linux, SysArch::Aarch64, &linux);
        assert!(ir.contains("\"={x0},{x9},{x10},{x11}"), "{ir}");
        assert!(ir.contains("mov x0, x9"), "{ir}");
        assert!(ir.contains("svc #0"), "{ir}");

        let darwin = Syscalls::for_os(SysOs::Macos);
        let ir = wrappers(SysOs::Macos, SysArch::Aarch64, &darwin);
        assert!(ir.contains("mov x16, #"), "{ir}");
        assert!(ir.contains("svc #0x80"), "{ir}");
    }

    /// The mapping arguments are in the order the system call expects:
    /// address, length, protection, flags, descriptor, offset. The flags go
    /// in the fourth register and the descriptor in the fifth, and swapping
    /// them maps the world with the wrong flags.
    #[test]
    fn the_mapping_arguments_are_in_order() {
        let linux = Syscalls::for_os(SysOs::Linux);
        let ir = wrappers(SysOs::Linux, SysArch::X86_64, &linux);
        let mmap = ir
            .split("define internal i64 @nexum_sys_mmap")
            .nth(1)
            .expect("no mmap wrapper");
        let mmap = mmap.split("}\n\n").next().unwrap_or(mmap);
        // Six arguments, six registers.
        assert!(mmap.contains("{rdi},{rsi},{rdx},{r10},{r8},{r9}"), "{mmap}");
        // The protection, the flags, the descriptor and the offset.
        assert!(mmap.contains("i64 3, i64 34, i64 -1, i64 0"), "{mmap}");
    }

    /// The page size is not the same everywhere: macOS on Apple Silicon has
    /// 16 KiB pages, and its `munmap` refuses a length that is not a
    /// multiple of the page size.
    #[test]
    fn the_page_size_is_per_target() {
        assert_eq!(page_size(SysOs::Linux, SysArch::X86_64), 4096);
        assert_eq!(page_size(SysOs::Linux, SysArch::Aarch64), 4096);
        assert_eq!(page_size(SysOs::Macos, SysArch::X86_64), 4096);
        assert_eq!(page_size(SysOs::Macos, SysArch::Aarch64), 16384);
    }
}
