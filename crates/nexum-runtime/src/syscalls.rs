//! The system call layer.
//!
//! Nexum programs do not use the C library: they reach the operating
//! system through the system call instruction directly. That is what makes a
//! program freestanding - it depends on the kernel and on nothing else.
//!
//! The wrappers are written as inline assembly in the generated LLVM IR. Each
//! one names the registers the system call uses, so the compiler puts the
//! arguments exactly where the instruction expects them. The system call
//! number is a constant in the assembly text: it differs per system and per
//! call, and it is never a value the program controls.

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
                read: 0,
                write: 4,
                open: 5,
                close: 6,
                mmap: 197,
                munmap: 199,
                exit: 200,
                lseek: 26,
                open_read: 0,
                open_write: 1 | 512 | 1024,
                open_mode: 0o644,
            },
        }
    }
}

/// The calling convention of the system call instruction.
#[derive(Debug, Clone, Copy)]
pub enum Convention {
    /// Linux on x86-64: the number goes in `rax`, the arguments in `rdi`,
    /// `rsi`, `rdx`, `r10`, `r8`, `r9`.
    LinuxX86_64,
    /// Darwin on x86-64: the same registers as Linux.
    DarwinX86_64,
    /// Linux on AArch64: the number goes in `x8`, the arguments in `x0` to
    /// `x5`.
    LinuxAarch64,
    /// Darwin on AArch64: the number goes in `x16`, the arguments in `x0` to
    /// `x5`.
    DarwinAarch64,
}

impl Convention {
    /// The registers the arguments go in, in order, before the call.
    fn arg_registers(self) -> &'static [&'static str] {
        match self {
            Convention::LinuxX86_64 | Convention::DarwinX86_64 => {
                &["rdi", "rsi", "rdx", "r10", "r8", "r9"]
            }
            // AArch64 takes the first argument in the same register the
            // result comes back in, so the arguments are staged in the
            // registers the system call does not touch and moved into place
            // by the assembly.
            Convention::LinuxAarch64 => &["x9", "x10", "x11", "x12", "x13", "x14"],
            Convention::DarwinAarch64 => &["x9", "x10", "x11", "x12", "x13", "x14"],
        }
    }

    /// The registers the system call takes its arguments in.
    fn call_registers(self) -> &'static [&'static str] {
        match self {
            Convention::LinuxX86_64 | Convention::DarwinX86_64 => {
                &["rdi", "rsi", "rdx", "r10", "r8", "r9"]
            }
            Convention::LinuxAarch64 | Convention::DarwinAarch64 => {
                &["x0", "x1", "x2", "x3", "x4", "x5"]
            }
        }
    }

    /// The register the result comes back in.
    fn result_register(self) -> &'static str {
        match self {
            Convention::LinuxX86_64 | Convention::DarwinX86_64 => "rax",
            Convention::LinuxAarch64 | Convention::DarwinAarch64 => "x0",
        }
    }

    /// The register the system call number goes in.
    fn number_register(self) -> &'static str {
        match self {
            Convention::LinuxX86_64 | Convention::DarwinX86_64 => "rax",
            Convention::LinuxAarch64 => "x8",
            Convention::DarwinAarch64 => "x16",
        }
    }

    /// The registers the instruction itself clobbers.
    fn clobbers(self) -> &'static [&'static str] {
        match self {
            Convention::LinuxX86_64 | Convention::DarwinX86_64 => &["rcx", "r11"],
            Convention::LinuxAarch64 => &["x8"],
            Convention::DarwinAarch64 => &["x16"],
        }
    }

    /// The instruction that makes the call.
    fn call(self) -> &'static str {
        match self {
            Convention::LinuxX86_64 | Convention::DarwinX86_64 => "syscall",
            Convention::LinuxAarch64 | Convention::DarwinAarch64 => "svc #0",
        }
    }
}

/// The convention for an operating system and an architecture.
pub fn convention(os: SysOs, arch: SysArch) -> Convention {
    match (os, arch) {
        (SysOs::Linux, SysArch::X86_64) => Convention::LinuxX86_64,
        (SysOs::Macos, SysArch::X86_64) => Convention::DarwinX86_64,
        (SysOs::Linux, SysArch::Aarch64) => Convention::LinuxAarch64,
        (SysOs::Macos, SysArch::Aarch64) => Convention::DarwinAarch64,
    }
}

/// One argument of a system call: the register it goes in and where the
/// value comes from.
struct Arg {
    /// Which argument register the value goes in, counting from zero.
    index: usize,
    /// The operand that holds the value, or `None` for a constant.
    source: Option<&'static str>,
    /// The constant, when there is no source.
    constant: i64,
}

/// The inline assembly of one system call.
///
/// The arguments are pinned to the registers the instruction reads them from,
/// so the body only has to set the register the number goes in and make the
/// call. The result comes back in the register the ABI uses, which is also
/// the output of the assembly block.
fn call_body(conv: Convention, nr: i64, args: &[Arg]) -> String {
    let arg_regs = conv.arg_registers();
    let call_regs = conv.call_registers();
    let result_reg = conv.result_register();
    let number_reg = conv.number_register();
    // The number goes into the register the convention uses. It is a
    // constant in the text: it differs per system and per call.
    let mut text = match conv {
        Convention::LinuxX86_64 | Convention::DarwinX86_64 => {
            format!("movq {nr}, %{number_reg}")
        }
        Convention::LinuxAarch64 => format!("mov {number_reg}, {nr}"),
        Convention::DarwinAarch64 => format!("mov {number_reg}, {nr}"),
    };
    // The arguments move into the registers the instruction reads them from.
    for (i, arg) in args.iter().enumerate() {
        let dst = call_regs[arg.index];
        // The value comes from the operand, which the constraint has placed
        // in the staging register; a constant is materialised there.
        let value = match arg.source {
            Some(_) => format!("${}", i),
            None => match conv {
                Convention::LinuxX86_64 | Convention::DarwinX86_64 => {
                    format!("{}", arg.constant)
                }
                Convention::LinuxAarch64 | Convention::DarwinAarch64 => {
                    format!("{}", arg.constant)
                }
            },
        };
        text.push_str(&match conv {
            Convention::LinuxX86_64 | Convention::DarwinX86_64 => {
                format!("; movq {value}, %{dst}")
            }
            Convention::LinuxAarch64 | Convention::DarwinAarch64 => {
                format!("; mov {dst}, {value}")
            }
        });
    }
    text.push_str(&format!("; {}", conv.call()));
    // The constraints: the result, then one per argument, then the registers
    // the instruction clobbers.
    let mut constraints = vec![format!("={{{result_reg}}}")];
    let mut operands: Vec<String> = Vec::new();
    for (i, arg) in args.iter().enumerate() {
        let reg = arg_regs[arg.index];
        constraints.push(format!("{{{reg}}}"));
        match arg.source {
            Some(src) => operands.push(format!("i64 {src}")),
            None => operands.push(format!("i64 {}", arg.constant)),
        }
        let _ = i;
    }
    for c in conv.clobbers() {
        constraints.push(format!("~{{{c}}}"));
    }
    constraints.push("~{memory}".to_string());
    // The text of an assembly block is one line: the newlines are escapes,
    // and a literal percent sign is doubled.
    let text = match conv {
        Convention::LinuxX86_64 | Convention::DarwinX86_64 => {
            text.replace('\n', "\\0A")
        }
        Convention::LinuxAarch64 | Convention::DarwinAarch64 => text.replace('\n', "\\0A"),
    };
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
pub fn wrappers(conv: Convention, sys: &Syscalls) -> String {
    let mut s = String::new();
    let mut push = |name: &str, nr: i64, args: &[Arg]| {
        s.push_str(&format!(
            "define internal i64 @nexum_sys_{name}(i64 %a0, i64 %a1, i64 %a2, i64 %a3) {{\nentry:\n{}  ret i64 %r\n}}\n\n",
            call_body(conv, nr, args)
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
