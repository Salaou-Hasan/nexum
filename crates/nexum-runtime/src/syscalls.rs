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
            },
        }
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
            "define internal i64 @nexum_sys_{name}(i64 %a0, i64 %a1, i64 %a2, i64 %a3) {{\nentry:\n{}  ret i64 %r\n}}\n\n",
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
    // mmap: the file descriptor is -1 (the mapping is anonymous) and the
    // offset is 0, so both are constants.
    push(
        "mmap",
        sys.mmap,
        &[
            arg(0, "%a0"),
            arg(1, "%a1"),
            arg(2, "%a2"),
            const_arg(3, -1),
            const_arg(4, 0),
        ],
    );
    push("munmap", sys.munmap, &[arg(0, "%a0"), arg(1, "%a1")]);
    push("exit", sys.exit, &[arg(0, "%a0")]);
    s
}
